//! What a delegation tree may spend, and how many workers may run at once.
//!
//! A [`TreeBudget`] is shared by every worker below one lead: siblings and
//! descendants draw from it, so no worker is handed the parent's whole
//! remaining allowance on top of what others already used. A worker is
//! granted at most what remains, up front, and what it did not use is
//! returned when it finishes.
//!
//! [`Admission`] bounds how many workers run at once across the
//! installation and per requesting profile. Waiting is first come, first
//! served (Tokio's semaphore is fair), bounded by a queue timeout, and ends
//! at once when the run is cancelled or revoked.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lightagent_core::RunLimits;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

/// The allowance shared by one delegation tree.
#[derive(Clone, Debug)]
pub struct TreeBudget {
    remaining: Arc<Mutex<Remaining>>,
}

#[derive(Debug)]
struct Remaining {
    turns: u32,
    tool_calls: u32,
    workers: u32,
    deadline: Option<Instant>,
}

/// What one worker run may use, taken from a [`TreeBudget`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Grant {
    pub turns: u32,
    pub tool_calls: u32,
    /// Seconds until the tree's deadline, when it has one.
    pub seconds: Option<u64>,
}

impl TreeBudget {
    /// A budget of `limits` (normally the lead's own) for at most `workers`
    /// worker runs, its clock starting now.
    pub fn new(limits: &RunLimits, workers: u32) -> Self {
        let deadline = limits.wall_clock().map(|budget| Instant::now() + budget);
        Self {
            remaining: Arc::new(Mutex::new(Remaining {
                turns: limits.max_turns,
                tool_calls: limits.max_tool_calls,
                workers,
                deadline,
            })),
        }
    }

    /// Take what one worker may use: at most `want`, never more than
    /// remains. Refused when the tree's turns, tool calls, workers or time
    /// are spent.
    pub fn grant(&self, want: &RunLimits) -> Result<Grant, String> {
        let Ok(mut remaining) = self.remaining.lock() else {
            return Err("the delegation budget is unavailable".to_owned());
        };
        let seconds = remaining
            .deadline
            .map(|deadline| deadline.saturating_duration_since(Instant::now()).as_secs());
        let spent = if remaining.workers == 0 {
            Some("workers")
        } else if remaining.turns == 0 {
            Some("model turns")
        } else if remaining.tool_calls == 0 {
            Some("tool calls")
        } else if seconds == Some(0) {
            Some("time")
        } else {
            None
        };
        if let Some(what) = spent {
            return Err(format!("the delegation budget has no {what} left"));
        }
        let grant = Grant {
            turns: want.max_turns.min(remaining.turns),
            tool_calls: want.max_tool_calls.min(remaining.tool_calls),
            seconds: match (want.wall_clock_secs, seconds) {
                (Some(own), Some(left)) => Some(own.min(left)),
                (own, None) => own,
                (None, left) => left,
            },
        };
        remaining.turns -= grant.turns;
        remaining.tool_calls -= grant.tool_calls;
        remaining.workers -= 1;
        Ok(grant)
    }

    /// Return what a worker granted `grant` did not use.
    pub fn settle(&self, grant: Grant, used_turns: u32, used_tool_calls: u32) {
        if let Ok(mut remaining) = self.remaining.lock() {
            remaining.turns += grant.turns.saturating_sub(used_turns);
            remaining.tool_calls += grant.tool_calls.saturating_sub(used_tool_calls);
        }
    }

    /// Model turns and tool calls left, for reporting.
    pub fn left(&self) -> (u32, u32) {
        self.remaining
            .lock()
            .map(|remaining| (remaining.turns, remaining.tool_calls))
            .unwrap_or((0, 0))
    }
}

/// Bounds how many workers run at once.
#[derive(Clone, Debug)]
pub struct Admission {
    /// Shared by every run of the installation.
    pub global: Arc<Semaphore>,
    /// Shared by every run of the requesting profile.
    pub profile: Arc<Semaphore>,
    pub queue_timeout: Duration,
}

/// The permits an admitted worker holds while it runs.
#[derive(Debug)]
pub struct Admitted {
    _profile: OwnedSemaphorePermit,
    _global: OwnedSemaphorePermit,
}

impl Admission {
    /// Admit one worker, calling `queued` once if it has to wait.
    pub async fn admit(
        &self,
        cancel: &CancellationToken,
        queued: impl FnOnce(),
    ) -> Result<Admitted, String> {
        let immediate = (
            Arc::clone(&self.profile).try_acquire_owned(),
            Arc::clone(&self.global).try_acquire_owned(),
        );
        if let (Ok(profile), Ok(global)) = immediate {
            return Ok(Admitted {
                _profile: profile,
                _global: global,
            });
        }
        queued();
        let wait = async {
            let profile = Arc::clone(&self.profile).acquire_owned().await;
            let global = Arc::clone(&self.global).acquire_owned().await;
            match (profile, global) {
                (Ok(profile), Ok(global)) => Ok(Admitted {
                    _profile: profile,
                    _global: global,
                }),
                _ => Err("worker admission is closed".to_owned()),
            }
        };
        tokio::select! {
            _ = cancel.cancelled() => Err("the run was cancelled while its worker waited".to_owned()),
            admitted = tokio::time::timeout(self.queue_timeout, wait) => match admitted {
                Ok(admitted) => admitted,
                Err(_) => Err(format!(
                    "no worker slot became free within {} s",
                    self.queue_timeout.as_secs()
                )),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits(turns: u32, tool_calls: u32) -> RunLimits {
        RunLimits {
            max_turns: turns,
            max_tool_calls: tool_calls,
            wall_clock_secs: Some(600),
            ..RunLimits::default()
        }
    }

    #[test]
    fn siblings_share_one_allowance_and_unused_parts_return() {
        let budget = TreeBudget::new(&limits(10, 6), 4);
        let first = budget.grant(&limits(8, 4)).unwrap();
        assert_eq!((first.turns, first.tool_calls), (8, 4));
        let second = budget.grant(&limits(8, 4)).unwrap();
        assert_eq!(
            (second.turns, second.tool_calls),
            (2, 2),
            "only what remains"
        );
        assert!(budget.grant(&limits(8, 4)).is_err(), "nothing remains");
        budget.settle(first, 3, 1);
        assert_eq!(budget.left(), (5, 3));
        let third = budget.grant(&limits(8, 4)).unwrap();
        assert_eq!((third.turns, third.tool_calls), (5, 3));
    }

    #[test]
    fn the_worker_count_and_the_deadline_bound_the_tree() {
        let budget = TreeBudget::new(&limits(100, 100), 1);
        let grant = budget.grant(&limits(1, 1)).unwrap();
        assert!(grant.seconds.is_some_and(|seconds| seconds <= 600));
        let error = budget.grant(&limits(1, 1)).unwrap_err();
        assert!(error.contains("workers"), "{error}");
    }

    #[tokio::test]
    async fn admission_queues_fairly_and_times_out() {
        let admission = Admission {
            global: Arc::new(Semaphore::new(1)),
            profile: Arc::new(Semaphore::new(1)),
            queue_timeout: Duration::from_millis(100),
        };
        let cancel = CancellationToken::new();
        let held = admission.admit(&cancel, || {}).await.unwrap();
        let mut waited = false;
        let error = admission
            .admit(&cancel, || waited = true)
            .await
            .unwrap_err();
        assert!(waited, "a waiting worker is reported as queued");
        assert!(error.contains("no worker slot"), "{error}");
        drop(held);
        assert!(admission.admit(&cancel, || {}).await.is_ok());
        let cancelled = CancellationToken::new();
        let _held = admission.admit(&cancel, || {}).await.unwrap();
        cancelled.cancel();
        let error = admission.admit(&cancelled, || {}).await.unwrap_err();
        assert!(error.contains("cancelled"), "{error}");
    }
}
