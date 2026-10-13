//! Execution isolation: the interface untrusted execution must go through.
//!
//! ```text
//! tool policy (which tools)  ->  SandboxGate (may this run execute, and where)
//!                                  |-- LocalOperator: Route::Host, the existing
//!                                  |   local executor, unchanged
//!                                  `-- RemoteUser: an eligible SandboxBackend,
//!                                      or a denial. Never the host.
//! ```
//!
//! **This module isolates nothing.** It is the typed seam a future OS-isolated
//! backend plugs into, and the gate that refuses untrusted execution until one
//! exists. Three things must not be confused:
//!
//! * the *interface* ([`SandboxBackend`], [`ExecutionContext`],
//!   [`SandboxGate`]): what a backend is asked to do and what it claims;
//! * a *simulated* backend (the test mock, [`Assurance::Simulated`]): runs
//!   nothing, isolates nothing, and the production gate never accepts it;
//! * *verified* OS isolation ([`Assurance::Verified`]): its witness,
//!   [`VerifiedIsolation`], has no values. No backend in this build can claim
//!   it, so every untrusted execution request is denied.
//!
//! The production gate holds [`DenyBackend`] unless a caller supplies another
//! backend, and even then routes an untrusted run to it only when its
//! assurance is verified and it enforces every guarantee the run requires.
//! No request path constructs a gate yet, and no configuration selects a
//! backend.

use std::collections::{BTreeMap, BTreeSet};
use std::net::IpAddr;
use std::path::{Component, Path};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use lightagent_core::identity::{
    AuthorizedProfile, Capability, LOCAL_OPERATOR, PrincipalId, RunTrust,
};
use lightagent_core::{ProfileId, RunId};
use serde::Serialize;
use tokio_util::sync::CancellationToken;

/// One isolation property a backend can enforce, and an untrusted run
/// requires. A fixed vocabulary, safe to log and report.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Guarantee {
    /// Only the run's granted roots are reachable on the filesystem.
    FilesystemRoots,
    /// A root granted read-only cannot be written.
    ReadOnlyRoots,
    /// Symbolic and hard links cannot reach outside a granted root.
    LinkEscape,
    /// Host credentials and private files (keys, tokens, the server's own
    /// data directory) are not reachable.
    HostCredentialIsolation,
    /// Processes run under an identity other than the server's OS account,
    /// or in a stronger boundary (container, VM).
    SeparateIdentity,
    /// The child starts with an empty environment plus explicit grants; it
    /// inherits nothing from the server.
    CleanEnvironment,
    /// Every process the execution started is gone when it ends.
    ProcessTreeCleanup,
    /// CPU time is bounded by the OS.
    CpuLimit,
    /// Memory is bounded by the OS.
    MemoryLimit,
    /// The number of processes is bounded by the OS.
    ProcessLimit,
    /// Outbound network is denied unless a destination is granted.
    EgressDenyByDefault,
    /// Loopback, private, link-local and metadata addresses are unreachable.
    PrivateNetworkBlock,
    /// A granted host name cannot be re-pointed at a blocked address.
    DnsRebindingProtection,
    /// Only secrets granted to the run reach the child.
    SecretGrantsOnly,
}

impl Guarantee {
    /// Every guarantee. An untrusted run requires all of them.
    pub const ALL: [Guarantee; 14] = [
        Self::FilesystemRoots,
        Self::ReadOnlyRoots,
        Self::LinkEscape,
        Self::HostCredentialIsolation,
        Self::SeparateIdentity,
        Self::CleanEnvironment,
        Self::ProcessTreeCleanup,
        Self::CpuLimit,
        Self::MemoryLimit,
        Self::ProcessLimit,
        Self::EgressDenyByDefault,
        Self::PrivateNetworkBlock,
        Self::DnsRebindingProtection,
        Self::SecretGrantsOnly,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::FilesystemRoots => "filesystem_roots",
            Self::ReadOnlyRoots => "read_only_roots",
            Self::LinkEscape => "link_escape",
            Self::HostCredentialIsolation => "host_credential_isolation",
            Self::SeparateIdentity => "separate_identity",
            Self::CleanEnvironment => "clean_environment",
            Self::ProcessTreeCleanup => "process_tree_cleanup",
            Self::CpuLimit => "cpu_limit",
            Self::MemoryLimit => "memory_limit",
            Self::ProcessLimit => "process_limit",
            Self::EgressDenyByDefault => "egress_deny_by_default",
            Self::PrivateNetworkBlock => "private_network_block",
            Self::DnsRebindingProtection => "dns_rebinding_protection",
            Self::SecretGrantsOnly => "secret_grants_only",
        }
    }
}

/// An operating system a backend's enforcement can be demonstrated on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Platform {
    Linux,
    MacOs,
    Windows,
    Other,
}

impl Platform {
    /// The platform this build runs on.
    pub fn current() -> Self {
        if cfg!(target_os = "linux") {
            Self::Linux
        } else if cfg!(target_os = "macos") {
            Self::MacOs
        } else if cfg!(target_os = "windows") {
            Self::Windows
        } else {
            Self::Other
        }
    }
}

/// Witness that a backend's isolation has been demonstrated on this
/// platform. **It has no values**: this build contains no verified backend,
/// so [`Assurance::Verified`] cannot be constructed anywhere, by any crate.
///
/// The first real backend adds a variant here, constructed only after its
/// enforcement is demonstrated on the running platform, in the same change
/// that adds the escape tests proving it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum VerifiedIsolation {}

/// How far a backend's claims can be trusted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Assurance {
    /// Runs nothing; refuses every request.
    Refuses,
    /// Pretends to run, in process, for tests. Isolates nothing, whatever
    /// guarantees it lists. Never accepted by the production gate.
    Simulated,
    /// OS isolation, demonstrated on this platform.
    Verified(VerifiedIsolation),
}

/// What a backend says it enforces. Advisory except through [`Assurance`]:
/// only a verified backend's list is ever relied on.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Capabilities {
    pub enforced: BTreeSet<Guarantee>,
    /// Platforms where the enforcement has been demonstrated by tests on
    /// GitHub-hosted runners. Empty for every backend in this build.
    pub demonstrated_on: BTreeSet<Platform>,
}

/// How a root is granted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Access {
    ReadOnly,
    ReadWrite,
}

/// A network destination a run may reach. Host names are kept as given;
/// resolving them, and refusing a resolution to a blocked address, is the
/// backend's [`Guarantee::DnsRebindingProtection`].
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct Destination {
    host: String,
    port: u16,
}

impl Destination {
    /// A public destination. Loopback, private, link-local, metadata and
    /// other non-public address literals are refused, as are `localhost`
    /// names. There is no way to grant a private destination yet.
    pub fn new(host: &str, port: u16) -> Result<Self, SandboxError> {
        let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
        let literal = host.trim_start_matches('[').trim_end_matches(']');
        if port == 0 || host.is_empty() || host.len() > 253 {
            return Err(SandboxError::InvalidContext(
                ContextFault::InvalidDestination,
            ));
        }
        if let Ok(ip) = literal.parse::<IpAddr>() {
            if !crate::builtins::web::is_public_ip(ip) {
                return Err(SandboxError::InvalidContext(
                    ContextFault::PrivateDestination,
                ));
            }
            return Ok(Self {
                host: ip.to_string(),
                port,
            });
        }
        let valid_label = |label: &str| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        };
        if !host.split('.').all(valid_label) {
            return Err(SandboxError::InvalidContext(
                ContextFault::InvalidDestination,
            ));
        }
        if host == "localhost" || host.ends_with(".localhost") {
            return Err(SandboxError::InvalidContext(
                ContextFault::PrivateDestination,
            ));
        }
        Ok(Self { host, port })
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn port(&self) -> u16 {
        self.port
    }
}

/// Outbound network for a run.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkGrant {
    /// No outbound network. The default.
    #[default]
    Deny,
    /// Only these destinations.
    Allow(BTreeSet<Destination>),
}

impl NetworkGrant {
    fn within(&self, parent: &Self) -> bool {
        match (self, parent) {
            (Self::Deny, _) => true,
            (Self::Allow(_), Self::Deny) => false,
            (Self::Allow(own), Self::Allow(cap)) => own.is_subset(cap),
        }
    }
}

/// Resource bounds for one run's execution.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct ExecutionLimits {
    /// Wall clock per execution. Enforced by the gate as well as the backend.
    pub wall_clock: Duration,
    /// CPU time per execution ([`Guarantee::CpuLimit`]).
    pub cpu: Duration,
    /// Memory per execution ([`Guarantee::MemoryLimit`]).
    pub memory_bytes: u64,
    /// Processes per execution ([`Guarantee::ProcessLimit`]).
    pub max_processes: u32,
    /// Output returned per execution. Enforced by the gate as well as the
    /// backend.
    pub max_output_bytes: usize,
}

impl Default for ExecutionLimits {
    fn default() -> Self {
        Self {
            wall_clock: Duration::from_secs(30),
            cpu: Duration::from_secs(30),
            memory_bytes: 512 * 1024 * 1024,
            max_processes: 32,
            max_output_bytes: 64 * 1024,
        }
    }
}

impl ExecutionLimits {
    fn valid(&self) -> bool {
        !self.wall_clock.is_zero()
            && !self.cpu.is_zero()
            && self.memory_bytes > 0
            && self.max_processes > 0
            && self.max_output_bytes > 0
    }

    /// The tighter of each bound.
    fn within(self, ceiling: Self) -> Self {
        Self {
            wall_clock: self.wall_clock.min(ceiling.wall_clock),
            cpu: self.cpu.min(ceiling.cpu),
            memory_bytes: self.memory_bytes.min(ceiling.memory_bytes),
            max_processes: self.max_processes.min(ceiling.max_processes),
            max_output_bytes: self.max_output_bytes.min(ceiling.max_output_bytes),
        }
    }
}

/// What a run is granted, before validation. Roots are logical labels
/// (`workspace`), never host paths: a backend maps a label to its own mount,
/// so a context carries no host path to leak.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Grants {
    pub roots: BTreeMap<String, Access>,
    pub network: NetworkGrant,
    /// Names of secrets the child may receive, resolved by the backend at
    /// execution. Nothing else from the server's environment is passed.
    pub secrets: BTreeSet<String>,
    pub limits: ExecutionLimits,
}

fn valid_root_label(label: &str) -> bool {
    (1..=64).contains(&label.len())
        && label
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

fn valid_secret_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && !name.starts_with(|c: char| c.is_ascii_digit())
        && name
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
}

impl Grants {
    fn validate(&self) -> Result<(), SandboxError> {
        if !self.roots.keys().all(|label| valid_root_label(label)) {
            return Err(SandboxError::InvalidContext(ContextFault::InvalidRoot));
        }
        if !self.secrets.iter().all(|name| valid_secret_name(name)) {
            return Err(SandboxError::InvalidContext(
                ContextFault::InvalidSecretName,
            ));
        }
        if !self.limits.valid() {
            return Err(SandboxError::InvalidContext(ContextFault::InvalidLimits));
        }
        Ok(())
    }
}

/// The deepest delegation an execution context may reach.
pub const MAX_DEPTH: u8 = 4;

/// Who an execution is for and what it may reach: one run of one profile.
/// Fields are private; build one with [`local_operator`](Self::local_operator)
/// or [`remote_user`](Self::remote_user), and narrow it for a delegated
/// worker with [`for_worker`](Self::for_worker).
#[derive(Clone, Debug)]
pub struct ExecutionContext {
    run: RunId,
    profile: ProfileId,
    principal: PrincipalId,
    trust: RunTrust,
    grants: Grants,
    depth: u8,
    cancel: CancellationToken,
}

impl ExecutionContext {
    /// A trusted run of the terminal, ACP, the single-user API or an
    /// operator. The gate routes it to the existing local executor.
    pub fn local_operator(
        run: RunId,
        profile: ProfileId,
        grants: Grants,
        cancel: CancellationToken,
    ) -> Result<Self, SandboxError> {
        grants.validate()?;
        let principal = PrincipalId::new(LOCAL_OPERATOR)
            .map_err(|_| SandboxError::InvalidContext(ContextFault::NotAuthorizedToRun))?;
        Ok(Self {
            run,
            profile,
            principal,
            trust: RunTrust::LocalOperator,
            grants,
            depth: 0,
            cancel,
        })
    }

    /// An untrusted run. It needs proof the caller may start runs on the
    /// profile ([`Capability::RunsWrite`]); the profile and principal come
    /// from that proof, never from the request.
    pub fn remote_user(
        run: RunId,
        authorized: &AuthorizedProfile,
        grants: Grants,
        cancel: CancellationToken,
    ) -> Result<Self, SandboxError> {
        if authorized.capability() != Capability::RunsWrite {
            return Err(SandboxError::InvalidContext(
                ContextFault::NotAuthorizedToRun,
            ));
        }
        grants.validate()?;
        Ok(Self {
            run,
            profile: authorized.profile().clone(),
            principal: authorized.principal().clone(),
            trust: RunTrust::RemoteUser,
            grants,
            depth: 0,
            cancel,
        })
    }

    /// The context of a delegated worker. It keeps this run's principal and
    /// trust, and its grants must fit inside this run's: a root it was not
    /// granted, write access to a read-only root, a destination or secret
    /// beyond this run's, is refused. Limits are capped at this run's.
    /// Cancelling this run cancels the worker. An untrusted run's worker
    /// stays on the same profile.
    pub fn for_worker(
        &self,
        run: RunId,
        profile: ProfileId,
        requested: Grants,
    ) -> Result<Self, SandboxError> {
        if self.depth >= MAX_DEPTH {
            return Err(SandboxError::InvalidContext(ContextFault::DepthExceeded));
        }
        if self.trust == RunTrust::RemoteUser && profile != self.profile {
            return Err(SandboxError::InvalidContext(
                ContextFault::CrossProfileWorker,
            ));
        }
        requested.validate()?;
        let roots_fit = requested.roots.iter().all(|(label, access)| {
            self.grants
                .roots
                .get(label)
                .is_some_and(|granted| access <= granted)
        });
        if !roots_fit
            || !requested.network.within(&self.grants.network)
            || !requested.secrets.is_subset(&self.grants.secrets)
        {
            return Err(SandboxError::InvalidContext(ContextFault::WidensParent));
        }
        let limits = requested.limits.within(self.grants.limits);
        Ok(Self {
            run,
            profile,
            principal: self.principal.clone(),
            trust: self.trust,
            grants: Grants {
                limits,
                ..requested
            },
            depth: self.depth + 1,
            cancel: self.cancel.child_token(),
        })
    }

    pub fn run(&self) -> &RunId {
        &self.run
    }

    pub fn profile(&self) -> &ProfileId {
        &self.profile
    }

    pub fn principal(&self) -> &PrincipalId {
        &self.principal
    }

    pub fn trust(&self) -> RunTrust {
        self.trust
    }

    pub fn grants(&self) -> &Grants {
        &self.grants
    }

    pub fn depth(&self) -> u8 {
        self.depth
    }

    pub fn cancel(&self) -> &CancellationToken {
        &self.cancel
    }

    /// The guarantees a backend must enforce to execute for this run: all of
    /// them for an untrusted run, none for a trusted one (which never reaches
    /// a backend).
    pub fn required(&self) -> BTreeSet<Guarantee> {
        match self.trust {
            RunTrust::RemoteUser => Guarantee::ALL.into_iter().collect(),
            RunTrust::LocalOperator => BTreeSet::new(),
        }
    }

    /// The complete environment a child receives: the granted secrets that
    /// `resolve` finds, and nothing else. A backend starts the child from an
    /// empty environment and adds exactly this.
    pub fn child_environment(
        &self,
        resolve: &dyn Fn(&str) -> Option<String>,
    ) -> BTreeMap<String, String> {
        self.grants
            .secrets
            .iter()
            .filter_map(|name| resolve(name).map(|value| (name.clone(), value)))
            .collect()
    }
}

/// One program to run in a sandbox. Never through a shell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecRequest {
    pub program: String,
    pub args: Vec<String>,
    /// The granted root to run in.
    pub root: String,
    /// A path relative to `root`; the root itself when `None`.
    pub cwd: Option<String>,
}

impl ExecRequest {
    fn validate(&self, ctx: &ExecutionContext) -> Result<(), SandboxError> {
        let bad = |fault| Err(SandboxError::InvalidRequest(fault));
        if self.program.trim().is_empty()
            || self.program.contains('\0')
            || self.args.iter().any(|arg| arg.contains('\0'))
        {
            return bad(RequestFault::InvalidProgram);
        }
        if !ctx.grants.roots.contains_key(&self.root) {
            return bad(RequestFault::UngrantedRoot);
        }
        if let Some(cwd) = &self.cwd {
            let escapes = cwd.contains('\0')
                || Path::new(cwd).components().any(|component| {
                    !matches!(component, Component::Normal(_) | Component::CurDir)
                });
            if escapes {
                return bad(RequestFault::InvalidPath);
            }
        }
        Ok(())
    }
}

/// What an execution produced, bounded by the run's output limit.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExecOutput {
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    /// Whether the gate cut the output to the limit.
    pub truncated: bool,
}

/// A backend's handle to the environment it created for one run.
#[derive(Debug, PartialEq, Eq)]
pub struct SandboxLease {
    id: u64,
}

impl SandboxLease {
    pub fn new(id: u64) -> Self {
        Self { id }
    }

    pub fn id(&self) -> u64 {
        self.id
    }
}

/// How a backend failed. Codes only: a backend cannot pass a message, a
/// path or an environment value through to a caller.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendFault {
    /// The backend cannot create an environment on this host.
    Unavailable,
    /// Creating the environment failed.
    StartFailed,
    /// The program could not be run, or the environment failed while it ran.
    ExecutionFailed,
    /// The backend stopped the execution at a limit.
    LimitExceeded(Guarantee),
}

/// A pluggable executor that runs programs away from the server's OS
/// account.
///
/// Contract for an implementation:
/// - [`start`](Self::start) is cancel-safe: if its future is dropped, nothing
///   it created outlives the drop.
/// - [`execute`](Self::execute) stops when `ctx.cancel()` fires.
/// - [`release`](Self::release) tears down everything the lease created
///   (processes, mounts, temporary files). The gate calls it exactly once
///   per successful `start`, on every outcome.
/// - The child's environment is exactly
///   [`ExecutionContext::child_environment`].
/// - [`assurance`](Self::assurance) is [`Assurance::Verified`] only through a
///   [`VerifiedIsolation`] witness, which this build cannot construct.
#[async_trait]
pub trait SandboxBackend: Send + Sync {
    /// A short, fixed name for logs.
    fn name(&self) -> &'static str;

    fn assurance(&self) -> Assurance;

    fn capabilities(&self) -> Capabilities;

    async fn start(&self, ctx: &ExecutionContext) -> Result<SandboxLease, BackendFault>;

    async fn execute(
        &self,
        lease: &SandboxLease,
        ctx: &ExecutionContext,
        request: &ExecRequest,
    ) -> Result<ExecOutput, BackendFault>;

    async fn release(&self, lease: SandboxLease);
}

/// The backend of a build with no isolation: refuses everything.
#[derive(Clone, Copy, Debug, Default)]
pub struct DenyBackend;

#[async_trait]
impl SandboxBackend for DenyBackend {
    fn name(&self) -> &'static str {
        "deny"
    }

    fn assurance(&self) -> Assurance {
        Assurance::Refuses
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::default()
    }

    async fn start(&self, _ctx: &ExecutionContext) -> Result<SandboxLease, BackendFault> {
        Err(BackendFault::Unavailable)
    }

    async fn execute(
        &self,
        _lease: &SandboxLease,
        _ctx: &ExecutionContext,
        _request: &ExecRequest,
    ) -> Result<ExecOutput, BackendFault> {
        Err(BackendFault::Unavailable)
    }

    async fn release(&self, _lease: SandboxLease) {}
}

/// Why the gate refused to route an untrusted run to a backend.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DenialReason {
    /// The backend refuses everything ([`DenyBackend`]).
    NoBackend,
    /// The backend's isolation is not verified (a simulation).
    NotVerified,
    /// The backend does not enforce a guarantee the run requires.
    Unsupported(Guarantee),
}

/// Why an execution context could not be built.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextFault {
    NotAuthorizedToRun,
    InvalidRoot,
    InvalidDestination,
    PrivateDestination,
    InvalidSecretName,
    InvalidLimits,
    WidensParent,
    CrossProfileWorker,
    DepthExceeded,
}

/// Why a request was refused before reaching a backend.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestFault {
    InvalidProgram,
    UngrantedRoot,
    InvalidPath,
}

/// A sandbox failure. Every variant is a fixed code: no path, program,
/// argument, environment value or backend text is ever carried.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, thiserror::Error)]
#[serde(rename_all = "snake_case")]
pub enum SandboxError {
    #[error("sandboxed execution denied: {}", .0.as_str())]
    Denied(DenialReason),
    #[error("sandboxed execution refused: authorization was revoked")]
    Revoked,
    #[error("sandboxed execution cancelled")]
    Cancelled,
    #[error("invalid execution context: {}", .0.as_str())]
    InvalidContext(ContextFault),
    #[error("invalid execution request: {}", .0.as_str())]
    InvalidRequest(RequestFault),
    #[error("the sandbox could not be created")]
    Unavailable,
    #[error("sandboxed execution failed")]
    ExecutionFailed,
    #[error("sandboxed execution stopped at its {} limit", .0.as_str())]
    LimitExceeded(Guarantee),
    /// The gate stopped the execution at the run's wall-clock limit.
    #[error("sandboxed execution exceeded its wall-clock limit")]
    TimedOut,
    /// A trusted run asked the gate to execute. Trusted runs use the existing
    /// local executor; the gate never runs anything on the host.
    #[error("a trusted run does not execute through the sandbox")]
    NotSandboxed,
}

impl DenialReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NoBackend => "no_backend",
            Self::NotVerified => "not_verified",
            Self::Unsupported(guarantee) => guarantee.as_str(),
        }
    }
}

impl ContextFault {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotAuthorizedToRun => "not_authorized_to_run",
            Self::InvalidRoot => "invalid_root",
            Self::InvalidDestination => "invalid_destination",
            Self::PrivateDestination => "private_destination",
            Self::InvalidSecretName => "invalid_secret_name",
            Self::InvalidLimits => "invalid_limits",
            Self::WidensParent => "widens_parent",
            Self::CrossProfileWorker => "cross_profile_worker",
            Self::DepthExceeded => "depth_exceeded",
        }
    }
}

impl RequestFault {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidProgram => "invalid_program",
            Self::UngrantedRoot => "ungranted_root",
            Self::InvalidPath => "invalid_path",
        }
    }
}

impl From<BackendFault> for SandboxError {
    fn from(fault: BackendFault) -> Self {
        match fault {
            BackendFault::Unavailable | BackendFault::StartFailed => Self::Unavailable,
            BackendFault::ExecutionFailed => Self::ExecutionFailed,
            BackendFault::LimitExceeded(guarantee) => Self::LimitExceeded(guarantee),
        }
    }
}

/// Where the gate sends a run's execution.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Route {
    /// A trusted run: the existing local executor, unchanged.
    Host,
    /// An untrusted run with an eligible backend.
    Sandbox,
}

/// Whether a run's authorization still holds (credential and ownership not
/// revoked). Checked before every start and every execution.
pub type AuthorizationCheck = Arc<dyn Fn(&ExecutionContext) -> bool + Send + Sync>;

/// Which assurance the gate accepts. Production accepts only verified
/// isolation; the simulated variant exists only in this crate's tests.
#[derive(Clone, Copy, Debug)]
enum Acceptance {
    VerifiedOnly,
    #[cfg(test)]
    SimulatedForTests,
}

impl Acceptance {
    fn accepts_simulated(self) -> bool {
        match self {
            Self::VerifiedOnly => false,
            #[cfg(test)]
            Self::SimulatedForTests => true,
        }
    }
}

/// The sandbox eligibility gate: decides where a run executes and drives the
/// backend's lifecycle. It has no host execution path at all, so a failed or
/// refused sandbox can never fall back to the server's OS account.
pub struct SandboxGate {
    backend: Arc<dyn SandboxBackend>,
    authorized: AuthorizationCheck,
    acceptance: Acceptance,
}

impl SandboxGate {
    /// The production gate of this build: every untrusted execution is
    /// denied.
    pub fn deny_by_default(authorized: AuthorizationCheck) -> Self {
        Self::with_backend(Arc::new(DenyBackend), authorized)
    }

    /// A production gate over `backend`. It routes untrusted runs to it only
    /// if its assurance is verified, which no backend in this build is.
    pub fn with_backend(backend: Arc<dyn SandboxBackend>, authorized: AuthorizationCheck) -> Self {
        Self {
            backend,
            authorized,
            acceptance: Acceptance::VerifiedOnly,
        }
    }

    /// A gate that also accepts a simulated backend, to exercise the
    /// lifecycle in this crate's tests. Absent from every non-test build.
    #[cfg(test)]
    pub(crate) fn accepting_simulated_for_tests(
        backend: Arc<dyn SandboxBackend>,
        authorized: AuthorizationCheck,
    ) -> Self {
        Self {
            backend,
            authorized,
            acceptance: Acceptance::SimulatedForTests,
        }
    }

    pub fn backend_name(&self) -> &'static str {
        self.backend.name()
    }

    /// Whether the backend may serve an untrusted run of `ctx`.
    fn eligible(&self, ctx: &ExecutionContext) -> Result<(), DenialReason> {
        match self.backend.assurance() {
            Assurance::Refuses => return Err(DenialReason::NoBackend),
            Assurance::Simulated if !self.acceptance.accepts_simulated() => {
                return Err(DenialReason::NotVerified);
            }
            Assurance::Simulated => {}
            Assurance::Verified(witness) => match witness {},
        }
        let enforced = self.backend.capabilities().enforced;
        match ctx.required().difference(&enforced).next() {
            Some(missing) => Err(DenialReason::Unsupported(*missing)),
            None => Ok(()),
        }
    }

    /// Where `ctx` executes. Checked in order: cancellation, authorization,
    /// trust, backend eligibility.
    pub fn route(&self, ctx: &ExecutionContext) -> Result<Route, SandboxError> {
        if ctx.cancel.is_cancelled() {
            return Err(SandboxError::Cancelled);
        }
        if !(self.authorized)(ctx) {
            return Err(SandboxError::Revoked);
        }
        match ctx.trust {
            RunTrust::LocalOperator => Ok(Route::Host),
            RunTrust::RemoteUser => self
                .eligible(ctx)
                .map(|()| Route::Sandbox)
                .map_err(SandboxError::Denied),
        }
    }

    /// Run `request` in the sandbox for an untrusted run. The wall-clock and
    /// output limits are enforced here as well as by the backend; the lease is
    /// released on every outcome after a successful start.
    pub async fn execute(
        &self,
        ctx: &ExecutionContext,
        request: &ExecRequest,
    ) -> Result<ExecOutput, SandboxError> {
        if self.route(ctx)? == Route::Host {
            return Err(SandboxError::NotSandboxed);
        }
        request.validate(ctx)?;
        let lease = tokio::select! {
            biased;
            () = ctx.cancel.cancelled() => return Err(SandboxError::Cancelled),
            started = self.backend.start(ctx) => started?,
        };
        // Authorization may have been revoked while the sandbox started.
        if !(self.authorized)(ctx) {
            self.backend.release(lease).await;
            return Err(SandboxError::Revoked);
        }
        let limits = ctx.grants.limits;
        let outcome = tokio::select! {
            biased;
            () = ctx.cancel.cancelled() => Err(SandboxError::Cancelled),
            ran = tokio::time::timeout(
                limits.wall_clock,
                self.backend.execute(&lease, ctx, request),
            ) => match ran {
                Ok(Ok(output)) => Ok(bound_output(output, limits.max_output_bytes)),
                Ok(Err(fault)) => Err(SandboxError::from(fault)),
                Err(_) => Err(SandboxError::TimedOut),
            },
        };
        self.backend.release(lease).await;
        outcome
    }
}

/// Cut stdout and stderr to `max_bytes` together, stdout first, on character
/// boundaries.
fn bound_output(mut output: ExecOutput, max_bytes: usize) -> ExecOutput {
    fn cut(text: &mut String, budget: usize) -> bool {
        if text.len() <= budget {
            return false;
        }
        let mut at = budget;
        while at > 0 && !text.is_char_boundary(at) {
            at -= 1;
        }
        text.truncate(at);
        true
    }
    let stdout_cut = cut(&mut output.stdout, max_bytes);
    let remaining = max_bytes.saturating_sub(output.stdout.len());
    let stderr_cut = cut(&mut output.stderr, remaining);
    output.truncated |= stdout_cut || stderr_cut;
    output
}

#[cfg(test)]
mod tests;
