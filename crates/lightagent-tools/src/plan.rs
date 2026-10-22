//! The order a run's specialist plan puts its workers in.
//!
//! A plan is decided before the run starts (by the coordinator, from fixed
//! rules over the run's own checks) and only ever narrows when a worker may
//! start: a role the plan puts after another starts only once that one has
//! completed in this run. It grants nothing: resolution, authorization,
//! approvals and budgets apply exactly as without a plan. A role the plan
//! does not name is not affected.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

/// Each planned role and the roles that must complete before it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RolePlan {
    after: BTreeMap<String, BTreeSet<String>>,
    completed: BTreeSet<String>,
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

    /// Record that a worker of `role` completed.
    pub fn completed(&self, role: &str) {
        if let Ok(mut inner) = self.inner.lock()
            && let Some(plan) = inner.as_mut()
        {
            plan.completed.insert(role.to_owned());
        }
    }

    /// The current plan, if any.
    pub fn snapshot(&self) -> Option<RolePlan> {
        self.inner.lock().ok()?.clone()
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
}
