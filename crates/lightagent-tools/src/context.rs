//! What a tool is given when it runs.
//!
//! A [`ToolCtx`] carries the cancellation token, the id of the run the call
//! belongs to (so a delegated child can name its parent), a [`Clock`] a tool
//! reads instead of the wall clock (which is what makes `datetime.now`
//! deterministic under test), and — only when delegation is enabled — a
//! [`Delegation`] bundle with the worker profile store, the provider factory and
//! the registry and bounds a worker run is given.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{
    Arc,
    atomic::{AtomicU8, Ordering},
};
use std::time::{Duration, SystemTime};

use lightagent_core::identity::{RunAuthorization, RunTrust, revoked_message};
use lightagent_core::{
    ApprovalPolicy, ProfileStore, ProviderFactory, RunId, RunLimits, SkillStore, ToolPolicyMode,
};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::budget::{Admission, TreeBudget};
use crate::executor::CallGuard;
use crate::registry::ToolRegistry;
use crate::worker::WorkerResolver;
use crate::workspace::Workspace;

/// The time source a tool reads.
///
/// `System` reads the real clock; `Fixed` returns a pinned instant, so a test
/// asserts an exact rendering rather than a moving one.
#[derive(Clone, Debug, Default)]
pub enum Clock {
    /// The real wall clock.
    #[default]
    System,
    /// A pinned instant, for deterministic tests.
    Fixed(SystemTime),
}

impl Clock {
    /// The current instant according to this clock.
    pub fn now(&self) -> SystemTime {
        match self {
            Self::System => SystemTime::now(),
            Self::Fixed(instant) => *instant,
        }
    }
}

/// Everything `agent.delegate` needs to start a worker run, injected by the
/// caller so this crate never depends on a transport.
#[derive(Clone)]
pub struct Delegation {
    /// Where worker profiles ("bots") are loaded from.
    pub profiles: Arc<ProfileStore>,
    /// Maps a worker's routing to a provider without this crate knowing how.
    pub factory: Arc<dyn ProviderFactory>,
    /// The tools a worker run may use. `agent.delegate` is absent from it, so a
    /// worker cannot delegate again — delegation is one level deep this pass.
    pub worker_registry: ToolRegistry,
    /// The per-call timeout a worker run's executor enforces.
    pub worker_per_call: Duration,
    /// The maximum time a delegated worker may occupy the parent tool call.
    pub delegate_timeout: Duration,
    /// The output ceiling a worker run's executor enforces.
    pub worker_max_output_bytes: usize,
    /// Extra, opt-in controls for named hierarchical workers. When disabled,
    /// legacy profile delegation remains deliberately one level deep.
    pub subagents: SubagentPolicy,
    /// When set, a worker's limits never exceed these: the originating run's
    /// own limits. Nested delegation inherits it. `None` keeps the worker
    /// profile's limits, as before.
    pub limit_ceiling: Option<RunLimits>,
    /// When set, a worker runs under the stricter of its profile's approval
    /// policy and this one, the originating run's. Nested delegation inherits
    /// it. `None` keeps the worker profile's policy, as before.
    pub approval_floor: Option<ApprovalPolicy>,
    /// Depth of this execution in the delegation tree. The lead is zero.
    pub depth: u8,
    /// Number of direct children this execution has reserved. This is shared
    /// only by calls made from the same executor and therefore cannot be used
    /// by a descendant to consume its parent's allowance.
    pub(crate) children: Arc<AtomicU8>,
    /// Resolves a role or a legacy profile target to a worker. `None` keeps
    /// the legacy resolution through `profiles` and `subagents.roles`.
    pub resolver: Option<Arc<dyn WorkerResolver>>,
    /// The originating run's trust. An untrusted run's worker tools are
    /// evaluated under the strict policy, as its own are.
    pub trust: RunTrust,
    /// The run's `tools.profile_policy`, applied to a worker profile's own
    /// `toolsets` exactly as it is to the lead's.
    pub profile_policy: ToolPolicyMode,
    /// The MCP servers reached over HTTP, for the worker's tool policy.
    pub network_mcp_servers: BTreeSet<String>,
    /// The originating run's per-call guard (its current profile policy),
    /// re-checked at every worker tool call too.
    pub call_guard: Option<CallGuard>,
    /// The allowance the whole delegation tree shares. `None`: each worker
    /// is bounded only by its own limits and the ceiling, as before.
    pub budget: Option<TreeBudget>,
    /// How many workers may run at once. Applied to the lead's workers; a
    /// nested worker runs inside its parent's slot.
    pub admission: Option<Admission>,
    /// Workers paused on an action that needs a person's approval.
    pub parked: crate::builtins::delegate::ParkedWorkers,
    /// How long a parked worker's pending action may wait for a decision.
    pub approval_ttl: Duration,
    /// The run's specialist plan, if one was given to the lead: the order
    /// its planned roles may start in. Empty by default.
    pub plan: crate::plan::PlanSlot,
}

/// A server-provided role preset. The model supplies only a role id; the
/// profile, permissions and provider routing remain server-owned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubagentRole {
    pub profile: String,
}

/// Controls the optional hierarchical delegation path.
///
/// This is intentionally separate from the legacy `agent.delegate` profile
/// argument. Keeping it disabled by default makes an existing installation
/// retain its exact single-worker behavior until the CLI/server opts in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubagentPolicy {
    pub enabled: bool,
    /// Maximum depth below the lead. Values above two are clamped when used.
    pub max_depth: u8,
    pub max_children_per_agent: u8,
    pub roles: BTreeMap<String, SubagentRole>,
}

impl Default for SubagentPolicy {
    fn default() -> Self {
        let roles = ["researcher", "coder", "reviewer", "tester"]
            .into_iter()
            .map(|id| {
                (
                    id.to_owned(),
                    SubagentRole {
                        profile: id.to_owned(),
                    },
                )
            })
            .collect();
        Self {
            enabled: false,
            max_depth: 2,
            max_children_per_agent: 3,
            roles,
        }
    }
}

impl Delegation {
    /// Start a root delegation context with legacy-safe policy defaults.
    pub fn new(
        profiles: Arc<ProfileStore>,
        factory: Arc<dyn ProviderFactory>,
        worker_registry: ToolRegistry,
        worker_per_call: Duration,
        worker_max_output_bytes: usize,
    ) -> Self {
        Self {
            profiles,
            factory,
            worker_registry,
            worker_per_call,
            delegate_timeout: Duration::from_secs(60),
            worker_max_output_bytes,
            subagents: SubagentPolicy::default(),
            limit_ceiling: None,
            approval_floor: None,
            depth: 0,
            children: Arc::new(AtomicU8::new(0)),
            resolver: None,
            trust: RunTrust::LocalOperator,
            profile_policy: ToolPolicyMode::Off,
            network_mcp_servers: BTreeSet::new(),
            call_guard: None,
            budget: None,
            admission: None,
            parked: Default::default(),
            approval_ttl: crate::builtins::delegate::APPROVAL_TTL,
            plan: Default::default(),
        }
    }

    /// Return the configured profile for an allowed named role.
    pub fn role_profile(&self, role: &str) -> Option<&str> {
        self.subagents
            .roles
            .get(role)
            .map(|preset| preset.profile.as_str())
    }

    /// The deepest worker generation hierarchical delegation allows.
    fn max_depth(&self) -> u8 {
        self.subagents.max_depth.min(2)
    }

    /// Whether this execution may start a worker: the worker sits one level
    /// below it, which must not pass the maximum depth. Legacy delegation
    /// (hierarchy off) is one level deep: only the lead holds a context.
    pub fn may_start_worker(&self) -> Result<(), String> {
        let limit = if self.subagents.enabled {
            self.max_depth()
        } else {
            1
        };
        if self.depth.saturating_add(1) > limit {
            return Err(format!("the subagent depth limit ({limit}) is reached"));
        }
        Ok(())
    }

    /// Whether a worker started here may itself be offered `agent.delegate`:
    /// only when one more generation still fits under the maximum depth.
    pub fn permits_nested_delegation(&self) -> bool {
        self.subagents.enabled && self.depth.saturating_add(2) <= self.max_depth()
    }

    /// Reserve one direct child. Reservations are monotonic for the lifetime
    /// of an execution, preventing repeated calls from bypassing the limit.
    pub fn reserve_child(&self) -> Result<(), String> {
        let maximum = self.subagents.max_children_per_agent;
        if maximum == 0 {
            return Err("subagent policy does not allow child workers".to_owned());
        }
        let result = self
            .children
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |used| {
                (used < maximum).then_some(used + 1)
            });
        result
            .map(|_| ())
            .map_err(|_| format!("subagent child limit ({maximum}) reached for this execution"))
    }

    /// Build an isolated child context. Its direct-child counter is new, so a
    /// descendant cannot spend its parent's budget.
    pub fn child_context(&self) -> Self {
        Self {
            profiles: Arc::clone(&self.profiles),
            factory: Arc::clone(&self.factory),
            worker_registry: self.worker_registry.clone(),
            worker_per_call: self.worker_per_call,
            delegate_timeout: self.delegate_timeout,
            worker_max_output_bytes: self.worker_max_output_bytes,
            subagents: self.subagents.clone(),
            limit_ceiling: self.limit_ceiling,
            approval_floor: self.approval_floor,
            depth: self.depth.saturating_add(1),
            children: Arc::new(AtomicU8::new(0)),
            resolver: self.resolver.clone(),
            trust: self.trust,
            profile_policy: self.profile_policy,
            network_mcp_servers: self.network_mcp_servers.clone(),
            call_guard: self.call_guard.clone(),
            budget: self.budget.clone(),
            admission: self.admission.clone(),
            parked: self.parked.clone(),
            approval_ttl: self.approval_ttl,
            // A plan orders the lead's workers only.
            plan: Default::default(),
        }
    }
}

/// The effective web-access settings a web tool enforces.
///
/// A plain, resolved view — the caller translates the persisted `WebConfig` into
/// this once (resolving the search key's [`SecretRef`](lightagent_core::SecretRef)
/// to a value held only in memory), so this crate never learns the config format
/// and never re-reads a secret. `allow_domains`, when non-empty, is a fetch
/// allow-list; an allow-listed host must still resolve to a permitted address
/// (see [`crate::egress`]).
#[derive(Clone)]
pub struct WebPolicy {
    /// When non-empty, a fetch host must match one of these (exactly or as a
    /// subdomain). Never an exemption from the address checks.
    pub allow_domains: Vec<String>,
    /// Refuse a fetch whose host resolves to a non-global address. Only an
    /// operator run can turn this off; management endpoints stay refused.
    pub block_private_addresses: bool,
    /// Operator-approved networks a non-public address may be reached in.
    /// Never applied to an untrusted run.
    pub trusted_networks: Vec<crate::egress::Network>,
    /// `Some` for an untrusted run: the hosts its profile may reach (fetch and
    /// search). `None` for an operator run.
    pub untrusted_destinations: Option<Vec<String>>,
    /// A search key is configured but this run's profile is not granted it:
    /// `web.search` refuses rather than send the query with or without it.
    pub search_credential_withheld: bool,
    /// The most bytes a single fetch reads before truncating.
    pub max_fetch_bytes: usize,
    /// The per-request timeout.
    pub timeout: Duration,
    /// The `web.search` endpoint; `None` means no backend is configured.
    pub search_endpoint: Option<String>,
    /// The query-string parameter the search endpoint reads the query from.
    pub search_query_param: String,
    /// The resolved search bearer key, held only in memory; never logged.
    pub search_api_key: Option<String>,
    /// When the key is a profile's own grant (an untrusted run), the grant
    /// re-checked before every search request: revoked, expired, rotated,
    /// over budget or to another origin means the request is not sent.
    pub search_credential: Option<Arc<lightagent_core::credentials::CredentialUse>>,
    /// The most results a single search returns.
    pub search_max_results: usize,
}

// A hand-written `Debug` so the resolved search key can never reach a log or an
// error through a `{:?}`, even if one is added later: its presence is shown, its
// value never is. The comment on `search_api_key` is thereby made structural
// rather than a promise.
impl std::fmt::Debug for WebPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebPolicy")
            .field("allow_domains", &self.allow_domains)
            .field("block_private_addresses", &self.block_private_addresses)
            .field("max_fetch_bytes", &self.max_fetch_bytes)
            .field("timeout", &self.timeout)
            .field("search_endpoint", &self.search_endpoint)
            .field("search_query_param", &self.search_query_param)
            .field(
                "search_api_key",
                &self.search_api_key.as_ref().map(|_| "<redacted>"),
            )
            .field("search_max_results", &self.search_max_results)
            .field("trusted_networks", &self.trusted_networks)
            .field("untrusted_destinations", &self.untrusted_destinations)
            .field(
                "search_credential_withheld",
                &self.search_credential_withheld,
            )
            .field(
                "search_credential",
                &self
                    .search_credential
                    .as_ref()
                    .map(|credential| credential.id()),
            )
            .finish()
    }
}

impl Default for WebPolicy {
    fn default() -> Self {
        Self {
            allow_domains: Vec::new(),
            block_private_addresses: true,
            trusted_networks: Vec::new(),
            untrusted_destinations: None,
            search_credential_withheld: false,
            max_fetch_bytes: 1_000_000,
            timeout: Duration::from_secs(20),
            search_endpoint: None,
            search_query_param: "q".to_owned(),
            search_api_key: None,
            search_credential: None,
            search_max_results: 5,
        }
    }
}

impl WebPolicy {
    /// The destination scope [`crate::egress::vet`] applies to this run.
    pub fn egress_rules(&self) -> crate::egress::EgressRules {
        crate::egress::EgressRules {
            allow_domains: self.allow_domains.clone(),
            trusted_networks: self.trusted_networks.clone(),
            block_private_addresses: self.block_private_addresses,
            untrusted_destinations: self.untrusted_destinations.clone(),
        }
    }
}

/// Everything the web tools need to run, injected by the caller so this crate
/// never builds an HTTP client or reads the config format itself.
#[derive(Clone)]
pub struct WebContext {
    /// The HTTP client the caller built (with the rustls provider installed and
    /// automatic redirects disabled, so the tool follows them under its guard).
    pub client: reqwest::Client,
    /// The effective web policy.
    pub policy: Arc<WebPolicy>,
    /// Resolves fetch hosts for the destination checks; the system resolver
    /// unless a test injects one.
    pub resolver: Arc<dyn crate::egress::Resolver>,
}

impl WebContext {
    /// A context over `client` and `policy` with the system resolver.
    pub fn new(client: reqwest::Client, policy: Arc<WebPolicy>) -> Self {
        Self {
            client,
            policy,
            resolver: Arc::new(crate::egress::SystemResolver),
        }
    }
}

/// The effective filesystem/terminal settings the workspace tools enforce.
///
/// A resolved view the caller builds once from the persisted `ToolsConfig`, so
/// this crate never learns the config format. `allow_terminal` gates
/// `terminal.run` on top of the workspace being present at all.
#[derive(Clone, Debug)]
pub struct WorkspacePolicy {
    /// The most bytes a single `fs.read`/`fs.write` may move.
    pub max_file_bytes: usize,
    /// Whether `terminal.run` may run at all.
    pub allow_terminal: bool,
    /// The wall-clock timeout for one `terminal.run`.
    pub terminal_timeout: Duration,
    /// When non-empty, the only program names `terminal.run` may launch.
    pub terminal_allowlist: Vec<String>,
}

/// Everything the filesystem and terminal tools need, injected by the caller so
/// this crate never resolves the workspace path or reads the config itself.
#[derive(Clone)]
pub struct WorkspaceContext {
    /// The confined root every path is resolved through.
    pub workspace: Arc<Workspace>,
    /// The effective policy.
    pub policy: Arc<WorkspacePolicy>,
}

/// Resolved connection details for an Open Terminal instance.
///
/// The bearer credential is held only in this run-local object and is redacted
/// from `Debug`; persisted configuration remains a `SecretRef` in the caller.
#[derive(Clone)]
pub struct OpenTerminalContext {
    /// HTTP client constructed by the harness.
    pub client: reqwest::Client,
    /// Effective remote-execution policy.
    pub policy: Arc<OpenTerminalPolicy>,
}

/// Bounded, session-scoped Open Terminal execution policy.
#[derive(Clone)]
pub struct OpenTerminalPolicy {
    /// Service root, without a trailing slash.
    pub base_url: String,
    /// Resolved bearer token. It is never included in diagnostics.
    pub api_key: Option<String>,
    /// Polling interval for a background process.
    pub poll_interval: Duration,
    /// Timeout for each HTTP operation, including cleanup.
    pub request_timeout: Duration,
    /// Maximum duration of a command, including service checks.
    pub execution_timeout: Duration,
    /// Maximum output returned by this adapter.
    pub max_output_bytes: usize,
}

impl std::fmt::Debug for OpenTerminalPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenTerminalPolicy")
            .field("base_url", &self.base_url)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("poll_interval", &self.poll_interval)
            .field("request_timeout", &self.request_timeout)
            .field("execution_timeout", &self.execution_timeout)
            .field("max_output_bytes", &self.max_output_bytes)
            .finish()
    }
}

/// The GitHub Actions workflow broker's resolved settings, built by the
/// caller from its configuration. The token is held only in this run-local
/// object and is redacted from `Debug`; tools never return it.
#[derive(Clone)]
pub struct ActionsPolicy {
    /// The REST API root, without a trailing slash.
    pub api_base: String,
    /// `owner/name`.
    pub repository: String,
    pub token: Option<String>,
    pub workflows: Vec<lightagent_core::AllowedWorkflow>,
    /// Branches `repo.commit` may advance: exact names or `prefix*`.
    pub commit_refs: Vec<String>,
    pub poll_interval: Duration,
    pub max_wait: Duration,
    pub max_log_bytes: usize,
}

impl std::fmt::Debug for ActionsPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ActionsPolicy")
            .field("api_base", &self.api_base)
            .field("repository", &self.repository)
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .field("workflows", &self.workflows)
            .field("commit_refs", &self.commit_refs)
            .field("poll_interval", &self.poll_interval)
            .field("max_wait", &self.max_wait)
            .field("max_log_bytes", &self.max_log_bytes)
            .finish()
    }
}

/// What the `actions.*` tools need: a client with redirects disabled, the
/// policy, and this run's dispatches, so an identical dispatch is never sent
/// twice in one run.
#[derive(Clone)]
pub struct ActionsContext {
    pub client: reqwest::Client,
    pub policy: Arc<ActionsPolicy>,
    pub dispatched: Arc<std::sync::Mutex<BTreeMap<String, Value>>>,
}

impl ActionsContext {
    pub fn new(client: reqwest::Client, policy: ActionsPolicy) -> Self {
        Self {
            client,
            policy: Arc::new(policy),
            dispatched: Arc::new(std::sync::Mutex::new(BTreeMap::new())),
        }
    }
}

/// The skills a run's `skill.read` tool can serve, injected by the caller so this
/// crate does not read the skills directories itself.
#[derive(Clone)]
pub struct SkillContext {
    /// The loaded skills.
    pub skills: Arc<SkillStore>,
}

/// The context handed to a [`Tool`](crate::Tool) when it runs.
#[derive(Clone)]
pub struct ToolCtx {
    /// Cancelled when the run is torn down; a long tool should stop.
    pub cancel: CancellationToken,
    /// The run this call belongs to, named as the `parent` of any child run the
    /// call starts. `None` when the caller did not supply one.
    pub run: Option<RunId>,
    /// The time source, so time-reading tools are testable.
    pub clock: Clock,
    /// Present only when delegation is enabled for this run.
    pub delegation: Option<Delegation>,
    /// Present only when web access is enabled for this run.
    pub web: Option<WebContext>,
    /// Present only when filesystem/terminal access is enabled for this run.
    pub workspace: Option<WorkspaceContext>,
    /// Present only when a configured Open Terminal service is available.
    pub open_terminal: Option<OpenTerminalContext>,
    /// Present only when the workflow broker is configured for this run.
    pub actions: Option<ActionsContext>,
    /// The run's proposed patches, and a worker's write scope.
    pub patches: Option<crate::builtins::PatchContext>,
    /// Present only when skills are available for this run.
    pub skills: Option<SkillContext>,
    /// The run's authorization, re-checked before a tool reaches outside the
    /// process (a remote request, a delegated worker). `None` for a run that
    /// is not revocable.
    pub authorization: Option<RunAuthorization>,
    /// Where the call may report progress while it runs. `None` when the
    /// caller does not stream it.
    pub progress: Option<lightagent_core::ToolProgressSink>,
}

impl ToolCtx {
    /// A context with no delegation, no web access and the system clock.
    pub fn new(cancel: CancellationToken) -> Self {
        Self {
            cancel,
            run: None,
            clock: Clock::System,
            delegation: None,
            web: None,
            workspace: None,
            open_terminal: None,
            actions: None,
            patches: None,
            skills: None,
            authorization: None,
            progress: None,
        }
    }

    /// Stream this call's progress reports to `progress`.
    pub fn with_progress(mut self, progress: lightagent_core::ToolProgressSink) -> Self {
        self.progress = Some(progress);
        self
    }

    /// Report how far the call has got. A no-op when nobody listens.
    pub fn report(&self, progress: lightagent_core::ToolProgress) {
        if let Some(sink) = &self.progress {
            let _ = sink.send(progress);
        }
    }

    /// Re-check `authorization` before remote requests and delegation.
    pub fn with_authorization(mut self, authorization: RunAuthorization) -> Self {
        self.authorization = Some(authorization);
        self
    }

    /// Whether the run may still take an action that reaches outside the
    /// process. Always allowed for a run that is not revocable.
    pub fn authorized(&self) -> Result<(), String> {
        match &self.authorization {
            Some(authorization) => authorization.verify().map_err(revoked_message),
            None => Ok(()),
        }
    }

    /// Set the owning run id (named as a child's parent).
    pub fn with_run(mut self, run: RunId) -> Self {
        self.run = Some(run);
        self
    }

    /// Set the clock.
    pub fn with_clock(mut self, clock: Clock) -> Self {
        self.clock = clock;
        self
    }

    /// Enable delegation with the given bundle.
    pub fn with_delegation(mut self, delegation: Delegation) -> Self {
        self.delegation = Some(delegation);
        self
    }

    /// Enable web access with the given context.
    pub fn with_web(mut self, web: WebContext) -> Self {
        self.web = Some(web);
        self
    }

    /// Enable filesystem/terminal access with the given context.
    pub fn with_workspace(mut self, workspace: WorkspaceContext) -> Self {
        self.workspace = Some(workspace);
        self
    }

    /// Enable the isolated remote execution service for this run.
    pub fn with_open_terminal(mut self, open_terminal: OpenTerminalContext) -> Self {
        self.open_terminal = Some(open_terminal);
        self
    }

    /// Make skills available to the run's `skill.read` tool.
    pub fn with_skills(mut self, skills: SkillContext) -> Self {
        self.skills = Some(skills);
        self
    }

    /// Enable the `actions.*` workflow-broker tools.
    pub fn with_actions(mut self, actions: ActionsContext) -> Self {
        self.actions = Some(actions);
        self
    }

    /// Enable the patch tools over `patches`.
    pub fn with_patches(mut self, patches: crate::builtins::PatchContext) -> Self {
        self.patches = Some(patches);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web_policy_debug_never_prints_the_key() {
        let policy = WebPolicy {
            allow_domains: vec!["example.com".to_owned()],
            block_private_addresses: true,
            max_fetch_bytes: 1024,
            timeout: Duration::from_secs(5),
            search_endpoint: Some("https://searx.test/search".to_owned()),
            search_query_param: "q".to_owned(),
            search_api_key: Some("super-secret-token".to_owned()),
            search_max_results: 5,
            ..WebPolicy::default()
        };
        let rendered = format!("{policy:?}");
        assert!(
            !rendered.contains("super-secret-token"),
            "the key leaked into Debug output: {rendered}"
        );
        assert!(rendered.contains("<redacted>"), "presence is still shown");
        // A struct with no key shows None, not <redacted>.
        let no_key = WebPolicy {
            search_api_key: None,
            ..policy
        };
        assert!(format!("{no_key:?}").contains("search_api_key: None"));
    }
}
