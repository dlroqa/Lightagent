//! Scoped bearer authentication.
//!
//! The API draws the same single distinction the inference gateway does:
//! loopback is trusted, anything else needs a key. An [`AuthConfig::open`] admits
//! every request (the default for a loopback bind and for tests); a keyed config
//! requires `Authorization: Bearer <key>` **and** that the key carries the
//! [`Scope`] the route needs, so a token minted for reading runs cannot start
//! them. Refusals are the two the plan calls for: `401` for a missing or wrong
//! key, `403` for a valid key that lacks the scope.
//!
//! **Multi-user mode** ([`AuthConfig::multi_user`]) is off unless a server is
//! built with it; `lightagent serve` never builds it yet. In that mode every
//! request needs a credential, loopback included, and nothing about the
//! connection (bind address, forwarded headers, proxy origin) stands in for
//! one. Callers come from the [`IdentityAuthority`]'s issued credentials, or
//! from the pre-existing key, kept as a legacy operator credential. Routes not
//! yet scoped to profiles are operator-only there, so nothing is partially
//! protected.

use std::collections::{HashSet, VecDeque};
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::http::{HeaderMap, StatusCode};
use lightagent_core::ProfileId;
use lightagent_core::identity::store::{IdentityStore, StoredAudit};
use lightagent_core::identity::{
    AuditAction, AuditRecord, AuthorizedProfile, Caller, Capability, CredentialStore, Denial,
    OwnershipRegistry, PrincipalSource, RevocationCheck, Role, authorize, authorize_capability,
    current_sweep,
};

use crate::manager::RunOwner;

/// A capability a route requires.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Scope {
    RunsRead,
    RunsWrite,
    SessionsRead,
    SessionsWrite,
    ToolsRead,
    ApprovalsWrite,
    /// Grants every scope.
    Admin,
}

impl From<Scope> for Capability {
    fn from(scope: Scope) -> Self {
        match scope {
            Scope::RunsRead => Capability::RunsRead,
            Scope::RunsWrite => Capability::RunsWrite,
            Scope::SessionsRead => Capability::SessionsRead,
            Scope::SessionsWrite => Capability::SessionsWrite,
            Scope::ToolsRead => Capability::ToolsRead,
            Scope::ApprovalsWrite => Capability::ApprovalsWrite,
            Scope::Admin => Capability::Administer,
        }
    }
}

/// Issued credentials and profile ownership: what multi-user mode decides
/// from. Held in memory and read at every request, so a revocation applies
/// to the next request and to open event streams.
#[derive(Clone, Debug, Default)]
pub struct IdentityState {
    pub credentials: CredentialStore,
    pub ownership: OwnershipRegistry,
}

/// The most recent authorization decisions kept in memory.
const AUDIT_CAPACITY: usize = 4_096;

/// The time credential expiry is judged at, in Unix seconds.
pub type Clock = Arc<dyn Fn() -> u64 + Send + Sync>;

fn system_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default()
}

/// The multi-user identity authority. Every check reads the current state;
/// a poisoned lock denies rather than admits.
pub struct IdentityAuthority {
    state: RwLock<IdentityState>,
    audit: Mutex<VecDeque<AuditRecord>>,
    /// When set, the state is re-read from this store before every check,
    /// so a change made elsewhere (another process, a restart) applies to
    /// the next request, and an unreadable store denies everything.
    store: Option<Arc<IdentityStore>>,
    /// Counts in-memory changes, standing in for the store's generation.
    changes: AtomicU64,
    now: Clock,
    /// The sweep whose store read the state reflects, and that read's
    /// outcome: every check inside the same sweep answers from it.
    swept: Mutex<(u64, Result<(), Denial>)>,
    /// How many times the durable store was read (diagnostics).
    reads: AtomicU64,
}

impl fmt::Debug for IdentityAuthority {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("IdentityAuthority { .. }")
    }
}

impl IdentityAuthority {
    pub fn new(state: IdentityState) -> Self {
        Self {
            state: RwLock::new(state),
            audit: Mutex::new(VecDeque::new()),
            store: None,
            changes: AtomicU64::new(0),
            now: Arc::new(system_now),
            swept: Mutex::new((0, Err(Denial::Unauthenticated))),
            reads: AtomicU64::new(0),
        }
    }

    /// How many times the durable store has been read.
    pub fn store_reads(&self) -> u64 {
        self.reads.load(Ordering::Relaxed)
    }

    /// Judge credential expiry by `now` instead of the system clock (tests,
    /// or a host with its own time source).
    pub fn with_clock(mut self, now: Clock) -> Self {
        self.now = now;
        self
    }

    /// The identity-state generation: the store's for a persistent
    /// authority, else the number of in-memory changes. `None` if the store
    /// cannot be read.
    pub fn generation(&self) -> Option<u64> {
        match &self.store {
            Some(store) => store.load().ok().map(|document| document.generation()),
            None => Some(self.changes.load(Ordering::SeqCst)),
        }
    }

    /// An authority backed by the durable store. It holds no state of its
    /// own between requests: every check re-reads the store, so nothing a
    /// revocation removed is retained. Changes go through
    /// [`IdentityStore::transact`], never [`update`](Self::update).
    pub fn persistent(store: Arc<IdentityStore>) -> Self {
        Self {
            state: RwLock::new(IdentityState::default()),
            audit: Mutex::new(VecDeque::new()),
            store: Some(store),
            changes: AtomicU64::new(0),
            now: Arc::new(system_now),
            swept: Mutex::new((0, Err(Denial::Unauthenticated))),
            reads: AtomicU64::new(0),
        }
    }

    /// Reload from the store, if any. On a store that cannot be read the
    /// state is emptied, so every credential and grant is refused.
    ///
    /// Inside a supervisor's [`sweep`](lightagent_core::identity::sweep)
    /// the store is read once and every check of that sweep answers from
    /// that read, failure included. Outside a sweep, every check reads.
    fn sync(&self) -> Result<(), Denial> {
        if self.store.is_none() {
            return Ok(());
        }
        let Some(epoch) = current_sweep() else {
            return self.reload();
        };
        let mut swept = self.swept.lock().map_err(|_| Denial::Unauthenticated)?;
        if swept.0 != epoch {
            *swept = (epoch, self.reload());
        }
        swept.1
    }

    fn reload(&self) -> Result<(), Denial> {
        let Some(store) = &self.store else {
            return Ok(());
        };
        self.reads.fetch_add(1, Ordering::Relaxed);
        let fresh = store.load().map(|document| IdentityState {
            credentials: document.credentials().clone(),
            ownership: store.effective_ownership(&document),
        });
        let mut state = self.state.write().map_err(|_| Denial::Unauthenticated)?;
        match fresh {
            Ok(fresh) => {
                *state = fresh;
                Ok(())
            }
            Err(_) => {
                *state = IdentityState::default();
                Err(Denial::Unauthenticated)
            }
        }
    }

    /// Change the in-memory state (issue or revoke a credential, assign an
    /// owner, grant or revoke a capability). `None` if the state is
    /// unavailable, or if the authority is persistent: durable changes are
    /// transactions on the store.
    pub fn update<R>(&self, change: impl FnOnce(&mut IdentityState) -> R) -> Option<R> {
        if self.store.is_some() {
            return None;
        }
        let changed = self.state.write().ok().map(|mut state| change(&mut state));
        if changed.is_some() {
            self.changes.fetch_add(1, Ordering::SeqCst);
        }
        changed
    }

    fn authenticate(&self, token: &str) -> Result<Caller, Denial> {
        self.sync()?;
        let state = self.state.read().map_err(|_| Denial::Unauthenticated)?;
        state.credentials.authenticate_at(token, (self.now)())
    }

    /// The caller as the current state sees it: an issued credential is read
    /// again by key id, so a revoked or narrowed credential takes effect. The
    /// legacy operator key cannot change while the server runs.
    pub fn refresh(&self, caller: &Caller) -> Result<Caller, Denial> {
        match &caller.principal.source {
            PrincipalSource::Credential { key_id } => {
                self.sync().map_err(|_| Denial::Revoked)?;
                let state = self.state.read().map_err(|_| Denial::Revoked)?;
                state.credentials.caller_for_at(key_id, (self.now)())
            }
            _ => Ok(caller.clone()),
        }
    }

    /// Authorize `caller` for `capability` on `profile` and audit it.
    pub fn authorize_profile(
        &self,
        caller: &Caller,
        capability: Capability,
        profile: &ProfileId,
    ) -> Result<AuthorizedProfile, Denial> {
        let decision = self
            .sync()
            .map_err(|_| Denial::ProfileNotGranted)
            .and_then(|()| self.state.read().map_err(|_| Denial::ProfileNotGranted))
            .and_then(|state| authorize(caller, capability, profile, &state.ownership));
        self.record(
            Some(caller),
            AuditAction::Request(capability),
            Some(profile),
            decision.as_ref().map(|_| ()).map_err(|denial| *denial),
        );
        decision
    }

    /// Whether `caller` may take `capability` on a run started by `owner`.
    ///
    /// An operator may act on every run. A user only on a run it started
    /// itself, and only while it is still authorized for that capability on
    /// the run's profile. A run with no recorded principal (started before
    /// multi-user mode, through ACP, or by the single-user API) is
    /// operator-only. Knowing a run id authorizes nothing.
    ///
    /// The caller's credential is read again first, so a [`Caller`] kept
    /// from before a revocation, expiry or narrowing authorizes nothing.
    pub fn authorize_run(
        &self,
        caller: &Caller,
        owner: &RunOwner,
        capability: Capability,
    ) -> Result<(), Denial> {
        let current = self.refresh(caller)?;
        let caller = &current;
        authorize_capability(caller, capability)?;
        if caller.principal.role == Role::Operator {
            return Ok(());
        }
        let (Some(principal), Some(profile)) = (owner.principal(), owner.profile()) else {
            return Err(Denial::ProfileNotGranted);
        };
        if principal != &caller.principal.id {
            return Err(Denial::ProfileNotGranted);
        }
        self.sync().map_err(|_| Denial::ProfileNotGranted)?;
        let state = self.state.read().map_err(|_| Denial::ProfileNotGranted)?;
        authorize(caller, capability, profile, &state.ownership).map(|_| ())
    }

    /// The check a revocable run started by `caller` re-runs at every
    /// enforcement point: the credential is read again, then the run's
    /// owner and profile are authorized for `runs_write` against the current
    /// ownership. It holds only identifiers, never an earlier decision. The
    /// first refusal is audited (the run stops checking after it).
    pub fn run_check(self: &Arc<Self>, caller: Caller, owner: RunOwner) -> RevocationCheck {
        let identity = Arc::clone(self);
        Arc::new(move || {
            let decision = identity.authorize_run(&caller, &owner, Capability::RunsWrite);
            if let Err(denial) = decision {
                identity.record(
                    Some(&caller),
                    AuditAction::Request(Capability::RunsWrite),
                    owner.profile(),
                    Err(denial),
                );
            }
            decision
        })
    }

    /// Append one audit record, keeping the most recent [`AUDIT_CAPACITY`].
    pub fn record(
        &self,
        caller: Option<&Caller>,
        action: AuditAction,
        profile: Option<&ProfileId>,
        outcome: Result<(), Denial>,
    ) {
        let at = (self.now)();
        let record = AuditRecord::decision(at, caller, action, profile, outcome);
        if let Some(store) = &self.store {
            // Durable and bounded; a failure to record never admits.
            let _ = store.append_audit(&[StoredAudit {
                generation: None,
                subject: None,
                record: record.clone(),
            }]);
        }
        if let Ok(mut audit) = self.audit.lock() {
            if audit.len() == AUDIT_CAPACITY {
                audit.pop_front();
            }
            audit.push_back(record);
        }
    }

    /// The retained audit records, oldest first. Identifiers and codes only.
    pub fn audit_log(&self) -> Vec<AuditRecord> {
        self.audit
            .lock()
            .map(|audit| audit.iter().cloned().collect())
            .unwrap_or_default()
    }
}

/// The status and fixed message for a refusal. A profile that is unowned and
/// one owned by someone else are refused alike, so a refusal does not reveal
/// which profiles exist or who owns them.
pub fn rejection(denial: Denial) -> (StatusCode, String) {
    match denial {
        Denial::Unauthenticated | Denial::Revoked | Denial::Expired => (
            StatusCode::UNAUTHORIZED,
            "a valid `Authorization: Bearer <key>` is required".to_owned(),
        ),
        Denial::MissingCapability => (
            StatusCode::FORBIDDEN,
            "the credential does not allow this action".to_owned(),
        ),
        Denial::ProfileOutsideCredential | Denial::ProfileUnowned | Denial::ProfileNotGranted => (
            StatusCode::FORBIDDEN,
            "the caller is not authorized for this profile".to_owned(),
        ),
    }
}

/// The API's authentication policy.
#[derive(Clone)]
pub struct AuthConfig {
    key: Option<String>,
    scopes: HashSet<Scope>,
    /// Set only in multi-user mode.
    identity: Option<Arc<IdentityAuthority>>,
}

/// Never prints the key.
impl fmt::Debug for AuthConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AuthConfig")
            .field("key", &self.key.as_ref().map(|_| "<redacted>"))
            .field("scopes", &self.scopes)
            .field("multi_user", &self.identity.is_some())
            .finish()
    }
}

impl AuthConfig {
    /// Admit every request (loopback, or a test). No key, no scope checks.
    pub fn open() -> Self {
        Self {
            key: None,
            scopes: HashSet::new(),
            identity: None,
        }
    }

    /// Require `key`, granting exactly `scopes` (add [`Scope::Admin`] for all).
    pub fn keyed(key: impl Into<String>, scopes: impl IntoIterator<Item = Scope>) -> Self {
        Self {
            key: Some(key.into()),
            scopes: scopes.into_iter().collect(),
            identity: None,
        }
    }

    /// Multi-user mode: every request needs a credential from `identity`,
    /// or the pre-existing `legacy_operator_key`, which keeps its full
    /// operator authority (it is never turned into a user credential).
    pub fn multi_user(
        identity: Arc<IdentityAuthority>,
        legacy_operator_key: Option<String>,
    ) -> Self {
        Self {
            key: legacy_operator_key,
            scopes: HashSet::from([Scope::Admin]),
            identity: Some(identity),
        }
    }

    /// The authority, in multi-user mode.
    pub fn identity(&self) -> Option<&Arc<IdentityAuthority>> {
        self.identity.as_ref()
    }

    /// Notices a server prints when it starts with this policy. Never the key.
    pub fn startup_notices(&self) -> Vec<&'static str> {
        let mut notices = Vec::new();
        if self.identity.is_some() {
            notices.push(
                "Multi-user mode: every API request needs a credential, including on loopback.",
            );
            if self.key.is_some() {
                notices.push(
                    "Warning: the --key-env key is a legacy OPERATOR credential with full authority over every profile and setting. Rotate it, or replace it with an issued operator credential and remove it.",
                );
            }
        }
        notices
    }

    /// Whether this policy requires a key.
    pub fn requires_key(&self) -> bool {
        self.key.is_some() || self.identity.is_some()
    }

    /// The caller this policy establishes for `headers`, in the shared
    /// identity vocabulary. Today's API has one identity: the local operator,
    /// reached through the open loopback bind or the single configured key
    /// (the compatibility mode). A key's scopes become the caller's
    /// capability restriction, so [`lightagent_core::identity::authorize`]
    /// agrees with [`AuthConfig::authorize`] for every scope. Handlers do not
    /// call this yet; it is the seam profile ownership will use.
    pub fn caller(&self, headers: &HeaderMap) -> Result<Caller, Denial> {
        if let Some(identity) = &self.identity {
            let caller = self.multi_user_caller(identity, headers);
            if caller.is_err() {
                identity.record(
                    None,
                    AuditAction::Authenticate,
                    None,
                    caller.as_ref().map(|_| ()).map_err(|denial| *denial),
                );
            }
            return caller;
        }
        let Some(expected) = &self.key else {
            return Ok(Caller::local_operator(PrincipalSource::LocalCompatibility));
        };
        match bearer(headers) {
            Some(token) if constant_time_eq(token.as_bytes(), expected.as_bytes()) => {
                let mut caller = Caller::local_operator(PrincipalSource::LocalCompatibility);
                if !self.scopes.contains(&Scope::Admin) {
                    caller.restriction.capabilities =
                        Some(self.scopes.iter().copied().map(Capability::from).collect());
                }
                Ok(caller)
            }
            _ => Err(Denial::Unauthenticated),
        }
    }

    /// Multi-user identity: a bearer credential is required whatever the
    /// connection. The legacy key is compared first, in constant time.
    fn multi_user_caller(
        &self,
        identity: &IdentityAuthority,
        headers: &HeaderMap,
    ) -> Result<Caller, Denial> {
        let token = bearer(headers).ok_or(Denial::Unauthenticated)?;
        if let Some(legacy) = &self.key
            && constant_time_eq(token.as_bytes(), legacy.as_bytes())
        {
            return Ok(Caller::local_operator(PrincipalSource::LegacyOperatorKey));
        }
        identity.authenticate(token)
    }

    /// Authorize a request carrying `headers` for a route needing `required`.
    ///
    /// In multi-user mode this gate serves routes not yet scoped to profiles,
    /// so it admits operators only: a user is refused rather than given
    /// installation-wide access.
    pub fn authorize(
        &self,
        headers: &HeaderMap,
        required: Scope,
    ) -> Result<(), (StatusCode, String)> {
        if let Some(identity) = &self.identity {
            let caller = self.caller(headers).map_err(rejection)?;
            let outcome = if caller.principal.role == Role::Operator {
                authorize_capability(&caller, required.into())
            } else {
                Err(Denial::MissingCapability)
            };
            identity.record(
                Some(&caller),
                AuditAction::Request(required.into()),
                None,
                outcome,
            );
            return outcome.map_err(|denial| match denial {
                Denial::MissingCapability if caller.principal.role == Role::User => (
                    StatusCode::FORBIDDEN,
                    "this endpoint is operator-only in multi-user mode".to_owned(),
                ),
                other => rejection(other),
            });
        }
        let Some(expected) = &self.key else {
            return Ok(()); // open policy: loopback is the boundary
        };
        match bearer(headers) {
            // Constant-time compare, so a key sharing a prefix with the real one
            // is not rejected measurably slower than one differing at the first
            // byte — the same protection the inference gateway gives its keys.
            Some(token) if constant_time_eq(token.as_bytes(), expected.as_bytes()) => {
                if self.scopes.contains(&Scope::Admin) || self.scopes.contains(&required) {
                    Ok(())
                } else {
                    Err((
                        StatusCode::FORBIDDEN,
                        format!("the key lacks the required scope {required:?}"),
                    ))
                }
            }
            _ => Err((
                StatusCode::UNAUTHORIZED,
                "a valid `Authorization: Bearer <key>` is required".to_owned(),
            )),
        }
    }
}

/// The bearer token in `headers`, if any. The auth scheme is
/// case-insensitive per RFC 7235, and clients vary — accept `Bearer`,
/// `bearer`, `BEARER`.
fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| {
            let (scheme, token) = value.split_once(' ')?;
            scheme
                .eq_ignore_ascii_case("bearer")
                .then_some(token.trim())
        })
}

/// Compare two byte strings without leaking their contents through timing.
///
/// Reproduced from the inference gateway's own `constant_time_eq` rather than
/// imported — this crate depends on no `lightweight-*` crate — so the API's key
/// check has the same protection the gateway's does. The length difference is
/// unavoidable and harmless; what matters is that a key sharing a prefix with the
/// real one does not take measurably longer to reject than one that differs at
/// the first byte.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers_with(token: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        if let Ok(value) = format!("Bearer {token}").parse() {
            headers.insert("authorization", value);
        }
        headers
    }

    #[test]
    fn an_open_policy_admits_everything() {
        let auth = AuthConfig::open();
        assert!(auth.authorize(&HeaderMap::new(), Scope::RunsWrite).is_ok());
    }

    #[test]
    fn a_keyed_policy_needs_the_key() {
        let auth = AuthConfig::keyed("secret", [Scope::RunsRead]);
        let (status, _) = auth
            .authorize(&HeaderMap::new(), Scope::RunsRead)
            .unwrap_err();
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let (status, _) = auth
            .authorize(&headers_with("wrong"), Scope::RunsRead)
            .unwrap_err();
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(
            auth.authorize(&headers_with("secret"), Scope::RunsRead)
                .is_ok()
        );
    }

    #[test]
    fn a_valid_key_without_the_scope_is_forbidden() {
        let auth = AuthConfig::keyed("secret", [Scope::RunsRead]);
        let (status, _) = auth
            .authorize(&headers_with("secret"), Scope::RunsWrite)
            .unwrap_err();
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    #[test]
    fn admin_grants_every_scope() {
        let auth = AuthConfig::keyed("secret", [Scope::Admin]);
        assert!(
            auth.authorize(&headers_with("secret"), Scope::SessionsWrite)
                .is_ok()
        );
    }

    #[test]
    fn the_bearer_scheme_is_case_insensitive() {
        let auth = AuthConfig::keyed("secret", [Scope::RunsRead]);
        for scheme in ["Bearer", "bearer", "BEARER"] {
            let mut headers = HeaderMap::new();
            if let Ok(value) = format!("{scheme} secret").parse() {
                headers.insert("authorization", value);
            }
            assert!(
                auth.authorize(&headers, Scope::RunsRead).is_ok(),
                "scheme {scheme:?} should be accepted"
            );
        }
    }

    #[test]
    fn constant_time_eq_matches_only_equal_slices() {
        assert!(constant_time_eq(b"secret", b"secret"));
        assert!(!constant_time_eq(b"secret", b"secreu")); // differs at the last byte
        assert!(!constant_time_eq(b"secret", b"xecret")); // differs at the first byte
        assert!(!constant_time_eq(b"secret", b"secre")); // shorter
        assert!(!constant_time_eq(b"secret", b"secrets")); // longer
        assert!(constant_time_eq(b"", b""));
    }

    const ALL_SCOPES: [Scope; 7] = [
        Scope::RunsRead,
        Scope::RunsWrite,
        Scope::SessionsRead,
        Scope::SessionsWrite,
        Scope::ToolsRead,
        Scope::ApprovalsWrite,
        Scope::Admin,
    ];

    /// The shared identity seam agrees with today's gate for every policy,
    /// presented key and required scope, so adopting it changes nothing.
    #[test]
    fn the_identity_caller_agrees_with_the_scope_gate() {
        use lightagent_core::identity::{LOCAL_OPERATOR, Role, authorize_capability};
        let mut policies = vec![
            AuthConfig::open(),
            AuthConfig::keyed("secret", [Scope::Admin]),
        ];
        for scope in ALL_SCOPES {
            policies.push(AuthConfig::keyed("secret", [scope]));
        }
        policies.push(AuthConfig::keyed(
            "secret",
            [Scope::RunsRead, Scope::SessionsRead],
        ));
        for policy in &policies {
            for headers in [
                HeaderMap::new(),
                headers_with("wrong"),
                headers_with("secret"),
            ] {
                for required in ALL_SCOPES {
                    let gate = policy.authorize(&headers, required);
                    let seam = policy
                        .caller(&headers)
                        .and_then(|caller| authorize_capability(&caller, required.into()));
                    match (&gate, &seam) {
                        (Ok(()), Ok(())) => {}
                        (Err((StatusCode::UNAUTHORIZED, _)), Err(Denial::Unauthenticated)) => {}
                        (Err((StatusCode::FORBIDDEN, _)), Err(Denial::MissingCapability)) => {}
                        _ => panic!("{policy:?} {required:?}: gate {gate:?} vs seam {seam:?}"),
                    }
                }
                if let Ok(caller) = policy.caller(&headers) {
                    assert_eq!(caller.principal.id.as_str(), LOCAL_OPERATOR);
                    assert_eq!(caller.principal.role, Role::Operator);
                    assert_eq!(caller.principal.source, PrincipalSource::LocalCompatibility);
                }
            }
        }
    }

    #[test]
    fn every_scope_maps_to_its_own_capability() {
        let mapped: std::collections::BTreeSet<Capability> =
            ALL_SCOPES.into_iter().map(Capability::from).collect();
        assert_eq!(mapped.len(), ALL_SCOPES.len());
        assert_eq!(Capability::from(Scope::Admin), Capability::Administer);
        assert!(
            !mapped.contains(&Capability::Provision),
            "no API scope provisions today"
        );
    }

    /// The persistent authority re-reads the durable store at every check:
    /// a revocation or profile deletion made elsewhere applies to the next
    /// request, a restart loses nothing, and an unreadable store denies.
    #[test]
    fn a_persistent_authority_follows_the_store_and_fails_closed() {
        use lightagent_core::identity::store::{IdentityStore, Outcome};
        use lightagent_core::identity::{AuditOutcome, PrincipalId, Restriction, Role};
        use lightagent_core::{AgentProfile, ProfileStore};

        let root = std::env::temp_dir().join(format!(
            "lightagent-api-identity-{}",
            lightagent_core::RunId::new().as_str()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let research = ProfileId::new("research").unwrap();
        ProfileStore::new(&root)
            .create(&AgentProfile::new(
                research.clone(),
                "Research",
                "Persona",
                "fast",
            ))
            .unwrap();
        let operator = Caller::local_operator(PrincipalSource::LocalProcess);
        let alice = PrincipalId::new("alice").unwrap();
        let admin = IdentityStore::new(&root);
        let Ok(Outcome::Applied {
            value: (key_id, token),
            ..
        }) = admin.transact(&operator, 1, None, |tx| {
            tx.create_principal(alice.clone(), Role::User, None)?;
            tx.assign_owner(&research, &alice)?;
            let (key_id, token) = tx.issue_credential(&alice, Restriction::default(), None)?;
            Ok((key_id, token.expose().to_owned()))
        })
        else {
            panic!("provisioning failed");
        };

        // A server started after provisioning: nothing held in memory.
        let authority = IdentityAuthority::persistent(Arc::new(IdentityStore::new(&root)));
        let caller = authority.authenticate(&token).unwrap();
        assert!(
            authority
                .authorize_profile(&caller, Capability::RunsWrite, &research)
                .is_ok()
        );
        assert!(authority.update(|_| ()).is_none(), "no in-memory changes");

        // Revoked through another handle (another process): refused at once,
        // including for the already-authenticated caller.
        admin
            .transact(&operator, 2, None, |tx| tx.revoke_credential(&key_id))
            .unwrap();
        assert_eq!(authority.authenticate(&token), Err(Denial::Revoked));
        assert_eq!(authority.refresh(&caller), Err(Denial::Revoked));

        // A deleted profile is no longer owned, whatever the record says.
        let second = admin
            .transact(&operator, 3, None, |tx| {
                tx.issue_credential(&alice, Restriction::default(), None)
                    .map(|(_, token)| token.expose().to_owned())
            })
            .unwrap();
        let Outcome::Applied { value: token, .. } = second else {
            panic!("issue failed");
        };
        let caller = authority.authenticate(&token).unwrap();
        ProfileStore::new(&root).delete(&research).unwrap();
        assert_eq!(
            authority.authorize_profile(&caller, Capability::RunsWrite, &research),
            Err(Denial::ProfileUnowned)
        );
        // Denials are durable audit records.
        assert!(
            admin
                .audit()
                .unwrap()
                .iter()
                .any(|record| record.record.outcome
                    == AuditOutcome::Denied(Denial::ProfileUnowned))
        );

        // An unreadable store denies everything.
        std::fs::write(root.join("identity").join("state.json"), b"{broken").unwrap();
        assert_eq!(authority.authenticate(&token), Err(Denial::Unauthenticated));
        std::fs::remove_dir_all(&root).ok();
    }
}
