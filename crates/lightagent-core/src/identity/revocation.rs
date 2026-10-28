//! Revocation during a run: authorization that is re-checked, not remembered.
//!
//! A request is authorized once, when it starts a run, but the run then acts
//! for seconds or minutes: model turns, tool calls, remote requests,
//! delegated workers, approval waits. A [`RunAuthorization`] travels with the
//! run and is asked again at each of those points ([`verify`]). It holds no
//! authorization of its own, only a [`RevocationCheck`] that reads the
//! *current* identity state, so an authorization object kept in memory can
//! never outlive a revocation.
//!
//! The first refusal is final. It is recorded, the run's cancellation token
//! is cancelled (stopping the model, the tool in flight and any worker that
//! shares the token), and every later check answers with the same denial,
//! even if the state would admit the caller again.
//!
//! A run that is not revocable ([`RunAuthorization::unconditional`]) is
//! today's trusted run: the terminal, ACP and the single-user API. Its checks
//! never consult anything and never fail, so single-user behavior does not
//! change.
//!
//! [`verify`]: RunAuthorization::verify

use std::cell::Cell;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use super::{Denial, PrincipalId};
use crate::profile::ProfileId;
use crate::provider::{AgentProvider, ProviderError, ProviderRequest, ProviderStream, ServedModel};

/// Reads the current identity state and answers whether the run's initiator
/// is still authorized for it. Called at every enforcement point, so it must
/// be bounded: no polling, no unbounded I/O.
pub type RevocationCheck = Arc<dyn Fn() -> Result<(), Denial> + Send + Sync>;

/// What a run is authorized as, re-checked during the run.
#[derive(Clone)]
pub struct RunAuthorization {
    inner: Arc<Inner>,
}

struct Inner {
    check: Option<RevocationCheck>,
    cancel: CancellationToken,
    revoked: OnceLock<Denial>,
    generation: Option<u64>,
    checks: AtomicU64,
}

impl fmt::Debug for RunAuthorization {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RunAuthorization")
            .field("revocable", &self.is_revocable())
            .field("revoked", &self.revoked())
            .field("generation", &self.inner.generation)
            .finish()
    }
}

impl RunAuthorization {
    /// A trusted run's authorization: never consulted, never revoked by a
    /// check. Operators may still cancel the run.
    pub fn unconditional(cancel: CancellationToken) -> Self {
        Self::build(None, cancel, None)
    }

    /// A revocable run's authorization. `check` is consulted at every
    /// enforcement point; `generation` is the identity-state generation the
    /// run was authorized at, kept for diagnostics.
    pub fn revocable(
        check: RevocationCheck,
        cancel: CancellationToken,
        generation: Option<u64>,
    ) -> Self {
        Self::build(Some(check), cancel, generation)
    }

    fn build(
        check: Option<RevocationCheck>,
        cancel: CancellationToken,
        generation: Option<u64>,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                check,
                cancel,
                revoked: OnceLock::new(),
                generation,
                checks: AtomicU64::new(0),
            }),
        }
    }

    /// Whether the run can lose its authorization.
    pub fn is_revocable(&self) -> bool {
        self.inner.check.is_some()
    }

    /// The identity-state generation the run was authorized at, if known.
    pub fn generation(&self) -> Option<u64> {
        self.inner.generation
    }

    /// Why the run lost its authorization, once it has.
    pub fn revoked(&self) -> Option<Denial> {
        self.inner.revoked.get().copied()
    }

    /// How many times the check was consulted (for diagnostics and tests).
    pub fn checks(&self) -> u64 {
        self.inner.checks.load(Ordering::Relaxed)
    }

    /// The run's cancellation token.
    pub fn cancel_token(&self) -> &CancellationToken {
        &self.inner.cancel
    }

    /// Whether the run may take its next action. A refusal revokes the run
    /// for good and cancels it.
    pub fn verify(&self) -> Result<(), Denial> {
        if let Some(denial) = self.revoked() {
            return Err(denial);
        }
        let Some(check) = &self.inner.check else {
            return Ok(());
        };
        self.inner.checks.fetch_add(1, Ordering::Relaxed);
        match check() {
            Ok(()) => Ok(()),
            Err(denial) => {
                self.revoke(denial);
                Err(self.revoked().unwrap_or(denial))
            }
        }
    }

    /// Revoke the run (a supervisor observed a change that affects it):
    /// records `denial`, unless an earlier one was recorded, and cancels the
    /// run. Returns whether this call revoked it.
    pub fn revoke(&self, denial: Denial) -> bool {
        let newly = self.inner.revoked.set(denial).is_ok();
        self.inner.cancel.cancel();
        newly
    }
}

static SWEEPS: AtomicU64 = AtomicU64::new(1);

thread_local! {
    static SWEEP: Cell<Option<u64>> = const { Cell::new(None) };
}

/// Run `checks` as one **sweep**: a supervisor re-checking many runs at
/// once. Inside it, an identity authority may read its durable state once
/// and answer every check from that read, instead of once per run. The
/// state is read at the start of every sweep: nothing is carried from one
/// sweep to the next, so a sweep never relies on an earlier positive
/// answer. `checks` must not await (the scope is the calling thread).
pub fn sweep<R>(checks: impl FnOnce() -> R) -> R {
    let epoch = SWEEPS.fetch_add(1, Ordering::Relaxed);
    let previous = SWEEP.with(|sweep| sweep.replace(Some(epoch)));
    let result = checks();
    SWEEP.with(|sweep| sweep.set(previous));
    result
}

/// The sweep the current thread is in, if any.
pub fn current_sweep() -> Option<u64> {
    SWEEP.with(Cell::get)
}

/// The message a run's tools and events report when its authorization was
/// revoked: the fixed denial code only.
pub fn revoked_message(denial: Denial) -> String {
    format!(
        "the run's authorization was withdrawn ({}); no further actions are taken",
        denial.code()
    )
}

/// Which runs an identity change can affect, by what the run recorded when
/// it started. Used by a supervisor to find the runs to revoke.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RevocationScope {
    /// One credential was revoked or expired.
    Credential(String),
    /// A principal was suspended (every credential of it revoked).
    Principal(PrincipalId),
    /// A profile's ownership was removed or reassigned.
    Profile(ProfileId),
    /// A principal lost a grant on a profile.
    Grant {
        profile: ProfileId,
        principal: PrincipalId,
    },
}

/// A provider that asks the run's authorization before every model request.
/// A revoked run sends nothing more to the model: the turn ends as
/// cancelled, because the revocation already cancelled the run's token.
pub struct AuthorizedProvider<P> {
    inner: P,
    authorization: RunAuthorization,
}

impl<P> AuthorizedProvider<P> {
    pub fn new(inner: P, authorization: RunAuthorization) -> Self {
        Self {
            inner,
            authorization,
        }
    }
}

#[async_trait]
impl<P: AgentProvider> AgentProvider for AuthorizedProvider<P> {
    /// Discovery is a request made with the run's credentials: a revoked run
    /// makes none, and is budgeted as having an unknown window.
    async fn served_model(&self) -> ServedModel {
        if self.authorization.verify().is_err() {
            return ServedModel::default();
        }
        self.inner.served_model().await
    }

    async fn stream(
        &self,
        request: ProviderRequest,
        cancel: CancellationToken,
    ) -> Result<ProviderStream, ProviderError> {
        if self.authorization.verify().is_err() {
            // The token is cancelled; a stream that never yields lets the
            // loop take its cancellation path without a model request.
            cancel.cancel();
            return Ok(Box::pin(futures_util::stream::pending()));
        }
        if !self.authorization.is_revocable() {
            return self.inner.stream(request, cancel).await;
        }
        // A revocable run also abandons a connection still being set up:
        // the loop only watches cancellation once the stream exists.
        tokio::select! {
            biased;
            () = cancel.cancelled() => Ok(Box::pin(futures_util::stream::pending())),
            stream = self.inner.stream(request, cancel.clone()) => stream,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    fn switch() -> (Arc<AtomicBool>, RevocationCheck) {
        let allowed = Arc::new(AtomicBool::new(true));
        let flag = allowed.clone();
        let check: RevocationCheck = Arc::new(move || {
            if flag.load(Ordering::SeqCst) {
                Ok(())
            } else {
                Err(Denial::Revoked)
            }
        });
        (allowed, check)
    }

    #[test]
    fn an_unconditional_run_is_never_consulted_or_revoked_by_a_check() {
        let cancel = CancellationToken::new();
        let authorization = RunAuthorization::unconditional(cancel.clone());
        for _ in 0..3 {
            assert_eq!(authorization.verify(), Ok(()));
        }
        assert!(!authorization.is_revocable());
        assert_eq!(authorization.checks(), 0);
        assert!(!cancel.is_cancelled());
    }

    #[test]
    fn the_first_refusal_is_final_and_cancels_the_run() {
        let (allowed, check) = switch();
        let cancel = CancellationToken::new();
        let authorization = RunAuthorization::revocable(check, cancel.clone(), Some(7));
        assert_eq!(authorization.verify(), Ok(()));
        allowed.store(false, Ordering::SeqCst);
        assert_eq!(authorization.verify(), Err(Denial::Revoked));
        assert!(cancel.is_cancelled());
        // The state admitting the caller again does not restore the run.
        allowed.store(true, Ordering::SeqCst);
        assert_eq!(authorization.verify(), Err(Denial::Revoked));
        assert_eq!(authorization.revoked(), Some(Denial::Revoked));
        assert_eq!(authorization.generation(), Some(7));
    }

    #[test]
    fn a_pushed_revocation_keeps_the_first_reason() {
        let (_allowed, check) = switch();
        let cancel = CancellationToken::new();
        let authorization = RunAuthorization::revocable(check, cancel.clone(), None);
        assert!(authorization.revoke(Denial::ProfileNotGranted));
        assert!(!authorization.revoke(Denial::Expired));
        assert_eq!(authorization.verify(), Err(Denial::ProfileNotGranted));
        assert!(cancel.is_cancelled());
        // Clones share the state.
        assert_eq!(
            authorization.clone().revoked(),
            Some(Denial::ProfileNotGranted)
        );
    }

    #[test]
    fn sweeps_are_scoped_to_the_thread_and_never_reused() {
        assert_eq!(current_sweep(), None);
        let (first, nested, after) = sweep(|| {
            let first = current_sweep();
            let nested = sweep(current_sweep);
            (first, nested, current_sweep())
        });
        assert!(first.is_some() && nested.is_some());
        assert_ne!(first, nested, "every sweep reads afresh");
        assert_eq!(after, first, "the outer sweep is restored");
        assert_eq!(current_sweep(), None);
        let other = std::thread::spawn(|| sweep(|| std::thread::spawn(current_sweep).join()))
            .join()
            .unwrap()
            .unwrap();
        assert_eq!(other, None, "another thread is never in this sweep");
        assert_ne!(sweep(current_sweep), first);
    }

    #[test]
    fn the_message_carries_only_the_code() {
        assert_eq!(
            revoked_message(Denial::Expired),
            "the run's authorization was withdrawn (expired); no further actions are taken"
        );
    }

    #[tokio::test]
    async fn a_revoked_run_sends_no_model_request() {
        use crate::mock::MockProvider;
        use futures_util::{FutureExt as _, StreamExt as _};
        let (allowed, check) = switch();
        let cancel = CancellationToken::new();
        let authorization = RunAuthorization::revocable(check, cancel.clone(), None);
        let provider = AuthorizedProvider::new(
            MockProvider::looping(vec![vec![crate::provider::ProviderEvent::Content(
                "hi".into(),
            )]]),
            authorization,
        );
        let request = ProviderRequest {
            model: "m".into(),
            messages: Vec::new(),
            tools: Vec::new(),
            temperature: None,
            max_tokens: None,
            reasoning_effort: None,
        };
        let mut stream = provider
            .stream(request.clone(), cancel.clone())
            .await
            .unwrap();
        assert!(
            stream.next().await.is_some(),
            "authorized: the model answers"
        );
        allowed.store(false, Ordering::SeqCst);
        let mut stream = provider.stream(request, cancel.clone()).await.unwrap();
        assert!(cancel.is_cancelled());
        // Never yields: the loop's cancellation arm ends the turn.
        assert!(stream.next().now_or_never().is_none());
    }

    struct Serves(Arc<std::sync::atomic::AtomicUsize>);

    #[async_trait]
    impl AgentProvider for Serves {
        async fn served_model(&self) -> ServedModel {
            self.0.fetch_add(1, Ordering::SeqCst);
            ServedModel {
                model: Some("qwen@32k".into()),
                context_window: Some(32_768),
            }
        }

        async fn stream(
            &self,
            _: ProviderRequest,
            _: CancellationToken,
        ) -> Result<ProviderStream, ProviderError> {
            Ok(Box::pin(futures_util::stream::empty()))
        }
    }

    #[tokio::test]
    async fn a_revoked_run_makes_no_discovery_request() {
        let (allowed, check) = switch();
        let asked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let provider = AuthorizedProvider::new(
            Serves(asked.clone()),
            RunAuthorization::revocable(check, CancellationToken::new(), None),
        );
        assert_eq!(provider.served_model().await.context_window, Some(32_768));
        assert_eq!(asked.load(Ordering::SeqCst), 1);
        allowed.store(false, Ordering::SeqCst);
        assert_eq!(provider.served_model().await, ServedModel::default());
        assert_eq!(asked.load(Ordering::SeqCst), 1, "nothing was asked");
        // The default answer of a provider that cannot say is unknown.
        let plain = AuthorizedProvider::new(
            crate::mock::MockProvider::looping(Vec::new()),
            RunAuthorization::unconditional(CancellationToken::new()),
        );
        assert_eq!(plain.served_model().await, ServedModel::default());
    }
}
