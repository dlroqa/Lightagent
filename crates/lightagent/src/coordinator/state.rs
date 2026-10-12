//! Per-run Jev call budgets and per-profile diagnostics, in process memory.
//!
//! Nothing here is persisted or shared between profiles: diagnostics are keyed
//! by profile id and hold only the fixed-vocabulary record already sent as the
//! `classification.completed` event. A connection check is remembered only
//! while the settings it checked are unchanged.

use std::collections::BTreeMap;
use std::hash::{BuildHasher, Hash, Hasher, RandomState};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use lightagent_core::{ClassificationDiagnostics, JevConfig};
use lightagent_jev::{EvaluationRequest, JevClient, Question, Response};
use tokio_util::sync::CancellationToken;

/// More profiles than this is not a real deployment; the oldest record goes.
const MAX_PROFILES: usize = 256;

/// The Jev requests one run (or terminal turn) may still make.
#[derive(Debug)]
pub(crate) struct JevBudget {
    limit: u32,
    used: AtomicU32,
}

impl JevBudget {
    pub(crate) fn new(limit: u32) -> Arc<Self> {
        Arc::new(Self {
            limit,
            used: AtomicU32::new(0),
        })
    }

    /// Count requests already made, e.g. by the preflight. Never refused: the
    /// configured limit always leaves room for the preflight's two.
    pub(crate) fn record(&self, calls: u32) {
        self.used.fetch_add(calls, Ordering::SeqCst);
    }

    /// Reserve one request, or refuse when the budget is spent.
    pub(crate) fn try_acquire(&self) -> bool {
        self.used
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |used| {
                (used < self.limit).then_some(used + 1)
            })
            .is_ok()
    }

    /// Return a reservation that sent nothing.
    pub(crate) fn release(&self) {
        let _ = self
            .used
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |used| {
                used.checked_sub(1)
            });
    }

    /// Start a new terminal turn that has already used `calls`.
    pub(crate) fn reset(&self, calls: u32) {
        self.used.store(calls, Ordering::SeqCst);
    }

    pub(crate) fn used(&self) -> u32 {
        self.used.load(Ordering::SeqCst)
    }
}

/// What triggered a recorded classification.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Trigger {
    Preflight,
    Explicit,
}

impl Trigger {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Preflight => "preflight",
            Self::Explicit => "explicit",
        }
    }
}

/// The latest classification of one profile.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Recorded {
    pub diagnostics: ClassificationDiagnostics,
    pub recorded_at: SystemTime,
}

/// The result of an operator's connection check.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Verification {
    pub ok: bool,
    pub checked_at: SystemTime,
    pub model_version: Option<String>,
    pub latency_ms: u64,
    /// A stable error code; never a response body or credential.
    pub error: Option<&'static str>,
    fingerprint: u64,
}

/// Process-local Jev state shared by a server's runs.
#[derive(Debug, Default)]
pub(crate) struct JevState {
    last: Mutex<BTreeMap<String, Recorded>>,
    verification: Mutex<Option<Verification>>,
    hasher: RandomState,
}

impl JevState {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Remember `diagnostics` as `profile`'s latest classification.
    pub(crate) fn record(&self, profile: &str, diagnostics: ClassificationDiagnostics) {
        let Ok(mut last) = self.last.lock() else {
            return;
        };
        if !last.contains_key(profile) && last.len() >= MAX_PROFILES {
            let oldest = last
                .iter()
                .min_by_key(|(_, record)| record.recorded_at)
                .map(|(id, _)| id.clone());
            if let Some(oldest) = oldest {
                last.remove(&oldest);
            }
        }
        last.insert(
            profile.to_owned(),
            Recorded {
                diagnostics,
                recorded_at: SystemTime::now(),
            },
        );
    }

    /// `profile`'s latest classification; never another profile's.
    pub(crate) fn last(&self, profile: &str) -> Option<Recorded> {
        self.last.lock().ok()?.get(profile).cloned()
    }

    /// The last connection check, if it checked the settings now in force.
    pub(crate) fn verification(&self, config: &JevConfig) -> Option<Verification> {
        let fingerprint = self.fingerprint(config);
        self.verification
            .lock()
            .ok()?
            .clone()
            .filter(|verification| verification.fingerprint == fingerprint)
    }

    /// Check the configured connection with one minimal, synthetic request.
    /// No user or profile data is sent.
    pub(crate) async fn verify(&self, config: &JevConfig) -> Verification {
        let started = Instant::now();
        let (ok, model_version, error) = match check(config).await {
            Ok(model_version) => (true, model_version, None),
            Err(code) => (false, None, Some(code)),
        };
        let verification = Verification {
            ok,
            checked_at: SystemTime::now(),
            model_version,
            latency_ms: millis(started.elapsed()),
            error,
            fingerprint: self.fingerprint(config),
        };
        if let Ok(mut slot) = self.verification.lock() {
            *slot = Some(verification.clone());
        }
        verification
    }

    /// Identifies the endpoint, model and credential a check applied to. The
    /// credential contributes only through a per-process keyed hash held in
    /// memory, so replacing the key invalidates an earlier check.
    fn fingerprint(&self, config: &JevConfig) -> u64 {
        let mut hasher = self.hasher.build_hasher();
        config.endpoint.base_url.hash(&mut hasher);
        config.model.hash(&mut hasher);
        config
            .endpoint
            .api_key
            .as_ref()
            .and_then(|secret| secret.resolve())
            .hash(&mut hasher);
        hasher.finish()
    }
}

async fn check(config: &JevConfig) -> Result<Option<String>, &'static str> {
    if config.endpoint.base_url.is_none() {
        return Err("no_endpoint");
    }
    let client = JevClient::from_config(config).map_err(|error| error.kind())?;
    let request = EvaluationRequest::new(
        config.model.clone(),
        "Lightagent connection check.",
        BTreeMap::from([(
            "reachable".to_owned(),
            Question::noul("Is this text a connection check?"),
        )]),
    );
    match client.evaluate(&request, &CancellationToken::new()).await {
        Ok(Response::TypeSafe(evaluation)) => Ok(evaluation.model),
        Ok(Response::LegacyOutput(_)) => Err("unsupported_format"),
        Err(error) => Err(error.kind()),
    }
}

pub(crate) fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diagnostics(category: &str) -> ClassificationDiagnostics {
        ClassificationDiagnostics {
            schema: 1,
            trigger: "preflight".to_owned(),
            mode: "shadow".to_owned(),
            outcome: "classified".to_owned(),
            source: Some("jev".to_owned()),
            category: Some(category.to_owned()),
            category_confidence: Some(0.95),
            confidence_available: true,
            threshold: Some(0.9),
            planning_probability: None,
            retrieval_probability: None,
            tools_probability: None,
            complexity: None,
            complexity_confidence: None,
            model_version: None,
            input_tokens: None,
            output_tokens: None,
            jev_calls: 1,
            classification_latency_ms: Some(3),
            preflight_latency_ms: 3,
            fallback_reason: None,
            influenced: false,
        }
    }

    #[test]
    fn budgets_count_the_preflight_and_refuse_beyond_the_limit() {
        let budget = JevBudget::new(4);
        budget.record(2);
        assert!(budget.try_acquire());
        assert!(budget.try_acquire());
        assert!(!budget.try_acquire(), "the fifth request is refused");
        assert_eq!(budget.used(), 4);
        budget.reset(1);
        assert!(budget.try_acquire());
        assert_eq!(budget.used(), 2);
    }

    #[test]
    fn diagnostics_are_scoped_to_their_profile() {
        let state = JevState::new();
        state.record("alpha", diagnostics("research"));
        assert_eq!(
            state.last("alpha").unwrap().diagnostics.category.as_deref(),
            Some("research")
        );
        assert_eq!(state.last("beta"), None, "no cross-profile history");
        state.record("alpha", diagnostics("coding"));
        assert_eq!(
            state.last("alpha").unwrap().diagnostics.category.as_deref(),
            Some("coding"),
            "only the latest record is kept"
        );
    }

    #[tokio::test]
    async fn a_check_applies_only_to_the_settings_it_checked() {
        let state = JevState::new();
        let mut config = JevConfig::default();
        let verification = state.verify(&config).await;
        assert!(!verification.ok);
        assert_eq!(verification.error, Some("no_endpoint"));
        assert_eq!(state.verification(&config), Some(verification));
        config.model = "jev-1.13.0".to_owned();
        assert_eq!(
            state.verification(&config),
            None,
            "changed settings need a new check"
        );
    }
}
