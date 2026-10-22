//! The order a run's specialist plan puts its workers in, and the change
//! each dependent check verified.
//!
//! A plan is decided before the run starts (by the coordinator, from fixed
//! rules over the run's own checks) and only ever narrows when a worker may
//! start: a role the plan puts after another starts only once that one has
//! completed in this run. It grants nothing: resolution, authorization,
//! approvals and budgets apply exactly as without a plan. A role the plan
//! does not name is not affected.
//!
//! A role that follows the coder (a tester, a reviewer) checks one exact
//! change: the run's latest patch, by id, content and base (see
//! [`crate::builtins::patch::current_subject`]). Its result is recorded
//! against that change. When the coder proposes a different change, or the
//! files or the checkout move, every earlier check becomes stale: a role
//! that depends on a stale check is refused until that check is run again
//! on the current change.
//!
//! How many workers run at once is the admission's concern, and how many an
//! execution starts in all is the child allowance's. Past that allowance a
//! planned run may still repair, within a fixed number of rounds: each round
//! lets the coder revise a change that a check has completed on, and each
//! planned check then run once more on the new change. A check never runs
//! twice on the same change, and a coder never revises a change nobody
//! checked, so a round cannot loop. Every start still shares the run's
//! turns, tool calls, time and worker count.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

/// The role whose output dependent checks verify.
const CODER: &str = "coder";

/// The most repair rounds a run may be configured with.
pub const MAX_REPAIR_ROUNDS: u8 = 2;

/// Each planned role, the roles that must complete before it, and the change
/// each check verified.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RolePlan {
    after: BTreeMap<String, BTreeSet<String>>,
    completed: BTreeSet<String>,
    verified: BTreeMap<String, String>,
    /// Repair rounds opened: coder starts past the initial allowance.
    repairs: u8,
    /// Re-checks each check role has started past the initial allowance.
    rechecks: BTreeMap<String, u8>,
}

impl RolePlan {
    /// A plan of `(role, roles it runs after)`.
    pub fn new(steps: impl IntoIterator<Item = (String, Vec<String>)>) -> Self {
        Self {
            after: steps
                .into_iter()
                .map(|(role, after)| (role, after.into_iter().collect()))
                .collect(),
            completed: BTreeSet::new(),
            verified: BTreeMap::new(),
            repairs: 0,
            rechecks: BTreeMap::new(),
        }
    }

    /// The roles `role` still waits for; empty when it may start.
    pub fn missing(&self, role: &str) -> Vec<String> {
        self.after
            .get(role)
            .map(|after| after.difference(&self.completed).cloned().collect())
            .unwrap_or_default()
    }

    /// The planned roles, in name order.
    pub fn roles(&self) -> Vec<String> {
        self.after.keys().cloned().collect()
    }

    /// Whether `role` checks the coder's change: it runs after the coder,
    /// directly or through another check.
    pub fn checks_a_change(&self, role: &str) -> bool {
        self.checks(role, 0)
    }

    fn checks(&self, role: &str, depth: usize) -> bool {
        depth < 4
            && self.after.get(role).is_some_and(|after| {
                after
                    .iter()
                    .any(|before| before == CODER || self.checks(before, depth + 1))
            })
    }

    /// The checks `role` depends on that did not verify the change `key`.
    pub fn stale_before(&self, role: &str, key: &str) -> Vec<String> {
        self.after
            .get(role)
            .map(|after| {
                after
                    .iter()
                    .filter(|before| self.checks_a_change(before))
                    .filter(|before| self.verified.get(*before).map(String::as_str) != Some(key))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The checks recorded against another change than `key` (every one,
    /// when there is no current change).
    pub fn stale(&self, key: Option<&str>) -> Vec<String> {
        self.verified
            .iter()
            .filter(|(_, verified)| Some(verified.as_str()) != key)
            .map(|(role, _)| role.clone())
            .collect()
    }

    /// Whether `role` may start past the initial allowance as one step of a
    /// repair, with at most `max_rounds` rounds. The coder opens a round,
    /// and only once a check completed on its current change; each check
    /// then runs once per round, never on a change it already verified.
    /// `change` is the current change's key.
    pub fn admit_repair(
        &mut self,
        role: &str,
        change: Option<&str>,
        max_rounds: u8,
    ) -> Result<(), String> {
        if !self.after.contains_key(role) {
            return Err(format!(
                "the {role} is not in this run's specialist plan, so it has no repair allowance"
            ));
        }
        let Some(change) = change else {
            return Err("there is no current change to repair or check".to_owned());
        };
        if role == CODER {
            if !self.verified.values().any(|verified| verified == change) {
                return Err(
                    "no check of the coder's current change has completed, so there is nothing \
                     to repair yet"
                        .to_owned(),
                );
            }
            if self.repairs >= max_rounds.min(MAX_REPAIR_ROUNDS) {
                return Err(format!("this run's repair rounds ({max_rounds}) are used"));
            }
            self.repairs += 1;
            return Ok(());
        }
        if !self.checks_a_change(role) {
            return Err(format!(
                "the {role} does not check the coder's change, so it has no repair allowance"
            ));
        }
        if self.verified.get(role).map(String::as_str) == Some(change) {
            return Err(format!(
                "the {role} already checked exactly this change: reuse its result"
            ));
        }
        let used = self.rechecks.get(role).copied().unwrap_or(0);
        if used >= self.repairs {
            return Err(format!(
                "the {role} has no re-check left: each repair round allows one, and {} are open",
                self.repairs
            ));
        }
        self.rechecks.insert(role.to_owned(), used + 1);
        Ok(())
    }
}

/// A run's plan, shared by its lead's delegations. Empty: no plan, no
/// effect. The terminal replaces it at every turn.
#[derive(Clone, Debug, Default)]
pub struct PlanSlot {
    inner: Arc<Mutex<Option<RolePlan>>>,
}

impl PlanSlot {
    /// Replace the plan (`None` removes it).
    pub fn set(&self, plan: Option<RolePlan>) {
        if let Ok(mut inner) = self.inner.lock() {
            *inner = plan;
        }
    }

    /// Whether `role` may start now under the plan, or which planned role
    /// it must wait for.
    pub fn check(&self, role: &str) -> Result<(), String> {
        let Ok(inner) = self.inner.lock() else {
            return Ok(());
        };
        let Some(plan) = inner.as_ref() else {
            return Ok(());
        };
        let missing = plan.missing(role);
        if missing.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "this run's specialist plan starts the {role} only after the {} has completed: \
                 delegate that first, or continue without the {role}",
                missing.join(" and the ")
            ))
        }
    }

    /// Whether `role` checks the coder's change under the plan.
    pub fn checks_a_change(&self, role: &str) -> bool {
        self.inner
            .lock()
            .ok()
            .and_then(|inner| inner.as_ref().map(|plan| plan.checks_a_change(role)))
            .unwrap_or(false)
    }

    /// Whether `role` may check the change `key` (described by `label`): every
    /// check it depends on must have verified exactly that change.
    pub fn check_fresh(&self, role: &str, key: &str, label: &str) -> Result<(), String> {
        let Ok(inner) = self.inner.lock() else {
            return Ok(());
        };
        let Some(plan) = inner.as_ref() else {
            return Ok(());
        };
        let stale = plan.stale_before(role, key);
        if stale.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "the {} result is for an earlier change, or there is none: delegate the {} \
                 again for {label} before the {role}",
                stale.join(" and the "),
                stale.join(" and the ")
            ))
        }
    }

    /// Record that a worker of `role` completed.
    pub fn completed(&self, role: &str) {
        if let Ok(mut inner) = self.inner.lock()
            && let Some(plan) = inner.as_mut()
        {
            plan.completed.insert(role.to_owned());
        }
    }

    /// Record that a worker of `role` completed its check of the change `key`.
    pub fn verified(&self, role: &str, key: &str) {
        if let Ok(mut inner) = self.inner.lock()
            && let Some(plan) = inner.as_mut()
        {
            plan.verified.insert(role.to_owned(), key.to_owned());
        }
    }

    /// The checks that no longer cover the current change `key`.
    pub fn stale(&self, key: Option<&str>) -> Vec<String> {
        self.inner
            .lock()
            .ok()
            .and_then(|inner| inner.as_ref().map(|plan| plan.stale(key)))
            .unwrap_or_default()
    }

    /// The current plan, if any.
    pub fn snapshot(&self) -> Option<RolePlan> {
        self.inner.lock().ok()?.clone()
    }

    /// Admit one repair start of `role` (see [`RolePlan::admit_repair`]).
    /// Without a plan there is no repair allowance.
    pub fn admit_repair(
        &self,
        role: &str,
        change: Option<&str>,
        max_rounds: u8,
    ) -> Result<(), String> {
        let Ok(mut inner) = self.inner.lock() else {
            return Err("this run's specialist plan is unavailable".to_owned());
        };
        match inner.as_mut() {
            Some(plan) => plan.admit_repair(role, change, max_rounds),
            None => {
                Err("this run has no specialist plan, so it has no repair allowance".to_owned())
            }
        }
    }

    /// Whether `role` completed its check of exactly the change `key`.
    pub fn already_verified(&self, role: &str, key: &str) -> bool {
        self.inner
            .lock()
            .ok()
            .and_then(|inner| {
                inner
                    .as_ref()
                    .map(|plan| plan.verified.get(role).map(String::as_str) == Some(key))
            })
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan() -> RolePlan {
        RolePlan::new([
            ("coder".to_owned(), Vec::new()),
            ("tester".to_owned(), vec!["coder".to_owned()]),
            (
                "reviewer".to_owned(),
                vec!["coder".to_owned(), "tester".to_owned()],
            ),
        ])
    }

    #[test]
    fn an_empty_slot_never_refuses() {
        let slot = PlanSlot::default();
        assert!(slot.check("tester").is_ok());
        assert!(slot.check_fresh("reviewer", "k", "patch").is_ok());
        assert!(!slot.checks_a_change("tester"));
        slot.completed("coder");
        assert_eq!(slot.snapshot(), None);
    }

    #[test]
    fn a_role_starts_only_after_what_it_needs() {
        let slot = PlanSlot::default();
        slot.set(Some(plan()));
        assert!(slot.check("coder").is_ok());
        let refused = slot.check("tester").unwrap_err();
        assert!(refused.contains("after the coder"), "{refused}");
        // A role the plan does not name is unaffected.
        assert!(slot.check("researcher").is_ok());
        slot.completed("coder");
        assert!(slot.check("tester").is_ok());
        assert_eq!(
            slot.snapshot().unwrap().missing("reviewer"),
            ["tester".to_owned()]
        );
        slot.completed("tester");
        assert!(slot.check("reviewer").is_ok());
        slot.set(None);
        assert_eq!(slot.snapshot(), None);
    }

    #[test]
    fn checks_are_bound_to_one_change_and_go_stale_with_it() {
        let slot = PlanSlot::default();
        slot.set(Some(plan()));
        assert!(slot.checks_a_change("tester") && slot.checks_a_change("reviewer"));
        assert!(!slot.checks_a_change("coder"));
        // The reviewer needs the tester's check of this exact change.
        let refused = slot.check_fresh("reviewer", "p1", "patch p1").unwrap_err();
        assert!(
            refused.contains("delegate the tester again for patch p1"),
            "{refused}"
        );
        slot.verified("tester", "p1");
        assert!(slot.check_fresh("reviewer", "p1", "patch p1").is_ok());
        slot.verified("reviewer", "p1");
        assert!(slot.stale(Some("p1")).is_empty());
        // The coder changes the patch: both checks are stale, and the
        // reviewer waits for a new test of the new change.
        assert_eq!(slot.stale(Some("p2")), ["reviewer", "tester"]);
        assert!(slot.check_fresh("reviewer", "p2", "patch p2").is_err());
        slot.verified("tester", "p2");
        assert!(slot.check_fresh("reviewer", "p2", "patch p2").is_ok());
        assert_eq!(slot.stale(Some("p2")), ["reviewer"]);
        assert_eq!(slot.stale(None), ["reviewer", "tester"]);
    }

    #[test]
    fn repairs_are_bounded_and_never_repeat_a_check() {
        let slot = PlanSlot::default();
        let refused = slot.admit_repair("coder", Some("p1"), 1).unwrap_err();
        assert!(refused.contains("no specialist plan"), "{refused}");
        slot.set(Some(plan()));
        // Nothing checked the change yet: there is nothing to repair.
        assert!(slot.admit_repair("coder", Some("p1"), 1).is_err());
        slot.completed("coder");
        slot.verified("tester", "p1");
        slot.completed("tester");
        // A re-check needs an open round.
        assert!(slot.admit_repair("tester", Some("p2"), 1).is_err());
        // No rounds configured: no repair.
        assert!(slot.admit_repair("coder", Some("p1"), 0).is_err());
        assert!(slot.admit_repair("coder", Some("p1"), 1).is_ok());
        let refused = slot.admit_repair("coder", Some("p1"), 1).unwrap_err();
        assert!(refused.contains("repair rounds"), "{refused}");
        // Each check runs once on the new change, never again on one it
        // already verified.
        let refused = slot.admit_repair("tester", Some("p1"), 1).unwrap_err();
        assert!(refused.contains("reuse its result"), "{refused}");
        assert!(slot.admit_repair("tester", Some("p2"), 1).is_ok());
        assert!(slot.admit_repair("tester", Some("p3"), 1).is_err());
        assert!(slot.admit_repair("reviewer", Some("p2"), 1).is_ok());
        assert!(slot.admit_repair("reviewer", Some("p2"), 1).is_err());
        // A role outside the plan, or no current change, repairs nothing.
        assert!(slot.admit_repair("researcher", Some("p2"), 1).is_err());
        assert!(slot.admit_repair("tester", None, 1).is_err());
        assert!(slot.already_verified("tester", "p1"));
        assert!(!slot.already_verified("tester", "p2"));
    }
}
