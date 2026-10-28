//! Adaptive output allocation (opt-in): the plan a run's output pool is
//! divided by.
//!
//! The plan is built only from what the run already decided: the roles of an
//! applied specialist plan, and under `generation.adaptive_allocation = jev`
//! the task complexity from the same classification request that chose them.
//! Jev is asked nothing more, names no number and changes no setting: every
//! token figure comes from the deterministic allocator in
//! `lightagent_core::allocation`, inside the operator's pool, ceilings and
//! limits, and every request is still sent only what it reserved.
//!
//! Jev answers no per-role demand question. A missing answer to any question
//! invalidates the whole shared classification, so asking more would put
//! routing and selection at risk for a signal nothing has evaluated. The only
//! demand signal it contributes is the task's complexity, used when its
//! confidence reaches the same threshold the classification is held to.

use lightagent_core::workers::WorkerRole;
use lightagent_core::{
    AdaptiveAllocation, Config, Demand, GenerationBudget, OutputPlan, PlannedShare,
};

use super::classify::{ClassificationReport, Complexity, Outcome};
use super::selection::Selection;

/// The demand class Lightagent assumes for a role's part: a coder writes a
/// change, a researcher a cited report, a reviewer located findings, a tester
/// a short verdict on what it ran. Fixed and illustrative, not measured.
fn role_demand(role: WorkerRole) -> Demand {
    match role {
        WorkerRole::Coder => Demand::Heavy,
        WorkerRole::Researcher | WorkerRole::Reviewer => Demand::Standard,
        WorkerRole::Tester => Demand::Light,
    }
}

/// The plan for this run, or `None` when adaptive allocation is off or the
/// run has no output pool to divide.
pub(crate) fn plan(
    config: &Config,
    report: Option<&ClassificationReport>,
    selection: Option<&Selection>,
    lead: &GenerationBudget,
) -> Option<OutputPlan> {
    let mode = config.generation.adaptive_allocation;
    if mode == AdaptiveAllocation::Off {
        return None;
    }
    let pool = lead.output_pool.as_ref()?;
    let mut plan = OutputPlan {
        source: "role_defaults".to_owned(),
        fallback_reason: None,
        shares: Vec::new(),
        lead_floor: lead
            .requested_max_tokens()
            .unwrap_or_else(|| pool.turn_tokens()),
        follow_up: lead.max_continuations > 0,
    };
    // Only a plan given to the lead divides the pool: a recorded (shadow)
    // plan, or one for a profile not enabled, changes nothing about the run.
    let applied =
        selection.filter(|chosen| chosen.record.applied && chosen.record.outcome == "plan");
    let Some(chosen) = applied else {
        plan.source = "no_plan".to_owned();
        plan.fallback_reason = Some(
            match selection {
                None => "no_selection",
                Some(chosen) if chosen.record.outcome != "plan" => "lead_keeps_task",
                Some(_) => "plan_not_applied",
            }
            .to_owned(),
        );
        return Some(plan);
    };
    let roles: Vec<WorkerRole> = chosen
        .record
        .steps
        .iter()
        .filter_map(|step| WorkerRole::parse(&step.role))
        .collect();
    // Jev's only demand signal: the task's complexity, held to the
    // classification's own threshold. Only `complex` changes a class.
    let complexity = match (mode, report.map(|report| &report.outcome)) {
        (AdaptiveAllocation::Jev, Some(Outcome::Classified(classification))) => {
            if classification.complexity_confidence >= f64::from(classification.threshold) {
                Some(classification.complexity)
            } else {
                plan.fallback_reason = Some("complexity_low_confidence".to_owned());
                None
            }
        }
        (AdaptiveAllocation::Jev, _) => {
            plan.fallback_reason = Some("classification_unavailable".to_owned());
            None
        }
        _ => None,
    };
    let complex = complexity == Some(Complexity::Complex);
    plan.shares = roles
        .iter()
        .map(|role| {
            let base = role_demand(*role);
            // Many sources or many changes: the research and the change
            // grow with the task, the checks of it much less.
            let raised = complex && matches!(role, WorkerRole::Researcher | WorkerRole::Coder);
            PlannedShare {
                role: role.as_str().to_owned(),
                demand: if raised { base.raised() } else { base },
            }
        })
        .collect();
    // Named for what actually happened: a confident classification that
    // changed no relative weight leaves the fixed classes, and says why. A
    // coder is already heavy, so only a planned researcher can be raised,
    // and a sole share takes the whole worker part whatever its class.
    if complexity.is_some() {
        let changed = plan.shares.len() > 1
            && plan
                .shares
                .iter()
                .zip(&roles)
                .any(|(share, role)| share.demand != role_demand(*role));
        if changed {
            plan.source = "jev_complexity".to_owned();
        } else {
            plan.fallback_reason = Some(
                if complex {
                    "complexity_changes_no_share"
                } else {
                    "complexity_not_complex"
                }
                .to_owned(),
            );
        }
    }
    let has = |role: WorkerRole| roles.contains(&role);
    plan.follow_up |= config.subagents.max_repair_rounds > 0
        && has(WorkerRole::Coder)
        && (has(WorkerRole::Tester) || has(WorkerRole::Reviewer));
    Some(plan)
}

/// Install this run's plan in the lead's output pool. Returns one line
/// describing the outcome, or `None` when adaptive allocation is off.
pub(crate) fn install(
    config: &Config,
    report: Option<&ClassificationReport>,
    selection: Option<&Selection>,
    lead: &GenerationBudget,
) -> Option<String> {
    if config.generation.adaptive_allocation == AdaptiveAllocation::Off {
        return None;
    }
    let mode = config.generation.adaptive_allocation.as_str();
    let (Some(pool), Some(plan)) = (&lead.output_pool, plan(config, report, selection, lead))
    else {
        return Some(format!(
            "· Output allocation ({mode}): inactive, no generation.max_run_output_tokens pool to \
             divide"
        ));
    };
    let total = pool.total();
    Some(match pool.install(plan.clone()) {
        Ok(allocation) => {
            let mut parts = vec![format!("lead {}", allocation.lead)];
            if allocation.follow_up > 0 {
                parts.push(format!("follow-up {}", allocation.follow_up));
            }
            for (share, tokens) in plan.shares.iter().zip(&allocation.shares) {
                parts.push(format!(
                    "{} {tokens} ({})",
                    share.role,
                    share.demand.as_str()
                ));
            }
            parts.push(format!("unallocated {}", allocation.unallocated));
            let reason = plan
                .fallback_reason
                .map(|reason| format!(", {}", reason.replace('_', " ")))
                .unwrap_or_default();
            format!(
                "· Output allocation ({mode}, {}{reason}): {} of {total}",
                plan.source.replace('_', " "),
                parts.join(", ")
            )
        }
        Err(reason) => format!(
            "· Output allocation ({mode}): not applied, the pool stays undivided ({})",
            reason.replace('_', " ")
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coordinator::classify::TaskCategory;
    use crate::coordinator::classify::TaskClassification;
    use lightagent_core::{
        GenerationConfig, JevClassificationMode, OutputPool, SpecialistSelection, SpecialistStep,
        SubagentsConfig,
    };
    use std::time::Duration;

    fn config(mode: AdaptiveAllocation, repairs: u8) -> Config {
        Config {
            generation: GenerationConfig {
                max_run_output_tokens: Some(32_000),
                adaptive_allocation: mode,
                ..GenerationConfig::default()
            },
            subagents: SubagentsConfig {
                max_repair_rounds: repairs,
                ..SubagentsConfig::default()
            },
            ..Config::default()
        }
    }

    fn lead(pool: Option<u32>) -> GenerationBudget {
        GenerationBudget {
            output_pool: pool.map(OutputPool::new),
            max_continuations: 1,
            ..GenerationBudget::default()
        }
    }

    fn selection(roles: &[&str], applied: bool) -> Selection {
        Selection {
            record: SpecialistSelection {
                mode: "active".to_owned(),
                outcome: "plan".to_owned(),
                steps: roles
                    .iter()
                    .map(|role| SpecialistStep {
                        role: (*role).to_owned(),
                        ..SpecialistStep::default()
                    })
                    .collect(),
                applied,
                ..SpecialistSelection::default()
            },
            instruction: None,
            order: None,
        }
    }

    fn report(complexity: Complexity, confidence: f64) -> ClassificationReport {
        ClassificationReport {
            mode: JevClassificationMode::Active,
            outcome: Outcome::Classified(TaskClassification {
                category: TaskCategory::Coding,
                category_confidence: 0.95,
                planning_probability: 0.8,
                retrieval_probability: 0.5,
                tools_probability: 0.9,
                complexity,
                complexity_confidence: confidence,
                model_version: Some("jev-1.14.0".to_owned()),
                usage: None,
                threshold: 0.9,
                selection: None,
            }),
            invoked: true,
            latency: Some(Duration::from_millis(5)),
        }
    }

    fn demands(plan: &OutputPlan) -> Vec<(&str, Demand)> {
        plan.shares
            .iter()
            .map(|share| (share.role.as_str(), share.demand))
            .collect()
    }

    #[test]
    fn off_or_without_a_pool_there_is_no_plan_and_nothing_is_installed() {
        let chosen = selection(&["coder"], true);
        let off = config(AdaptiveAllocation::Off, 1);
        assert!(plan(&off, None, Some(&chosen), &lead(Some(32_000))).is_none());
        assert!(install(&off, None, Some(&chosen), &lead(Some(32_000))).is_none());
        let on = config(AdaptiveAllocation::Plan, 1);
        assert!(plan(&on, None, Some(&chosen), &lead(None)).is_none());
        let line = install(&on, None, Some(&chosen), &lead(None)).unwrap();
        assert!(line.contains("inactive"), "{line}");
    }

    #[test]
    fn only_an_applied_plan_gives_workers_shares() {
        let on = config(AdaptiveAllocation::Plan, 0);
        let budget = lead(Some(32_000));
        for (chosen, reason) in [
            (None, "no_selection"),
            (Some(selection(&["coder"], false)), "plan_not_applied"),
        ] {
            let plan = plan(&on, None, chosen.as_ref(), &budget).unwrap();
            assert_eq!(plan.source, "no_plan");
            assert_eq!(plan.fallback_reason.as_deref(), Some(reason));
            assert!(
                plan.shares.is_empty(),
                "no role gets a share it was not planned"
            );
        }
        let mut kept = selection(&[], true);
        kept.record.outcome = "lead".to_owned();
        let plan = plan(&on, None, Some(&kept), &budget).unwrap();
        assert_eq!(plan.fallback_reason.as_deref(), Some("lead_keeps_task"));
    }

    #[test]
    fn role_defaults_ignore_every_classification_signal() {
        let on = config(AdaptiveAllocation::Plan, 0);
        let chosen = selection(&["researcher", "coder", "tester"], true);
        let complex = report(Complexity::Complex, 0.99);
        let plan = plan(&on, Some(&complex), Some(&chosen), &lead(Some(32_000))).unwrap();
        assert_eq!(plan.source, "role_defaults");
        assert_eq!(plan.fallback_reason, None);
        assert_eq!(
            demands(&plan),
            [
                ("researcher", Demand::Standard),
                ("coder", Demand::Heavy),
                ("tester", Demand::Light)
            ]
        );
    }

    #[test]
    fn jev_complexity_raises_research_and_code_only_when_confident() {
        let on = config(AdaptiveAllocation::Jev, 0);
        let chosen = selection(&["researcher", "reviewer"], true);
        let budget = lead(Some(32_000));
        let confident = report(Complexity::Complex, 0.95);
        let raised = plan(&on, Some(&confident), Some(&chosen), &budget).unwrap();
        assert_eq!(raised.source, "jev_complexity");
        assert_eq!(
            demands(&raised),
            [
                ("researcher", Demand::Heavy),
                ("reviewer", Demand::Standard)
            ]
        );
        // Below the classification's own threshold: the fixed classes.
        let unsure = report(Complexity::Complex, 0.5);
        let kept = plan(&on, Some(&unsure), Some(&chosen), &budget).unwrap();
        assert_eq!(kept.source, "role_defaults");
        assert_eq!(
            kept.fallback_reason.as_deref(),
            Some("complexity_low_confidence")
        );
        assert_eq!(
            demands(&kept),
            [
                ("researcher", Demand::Standard),
                ("reviewer", Demand::Standard)
            ]
        );
        // A failed or missing classification: the fixed classes, labelled.
        let failed = ClassificationReport {
            outcome: Outcome::Failed("malformed"),
            ..report(Complexity::Complex, 0.99)
        };
        for missing in [Some(&failed), None] {
            let plan = plan(&on, missing, Some(&chosen), &budget).unwrap();
            assert_eq!(plan.source, "role_defaults");
            assert_eq!(
                plan.fallback_reason.as_deref(),
                Some("classification_unavailable")
            );
        }
    }

    #[test]
    fn the_follow_up_reserve_covers_continuations_and_repairs() {
        let budget = GenerationBudget {
            max_continuations: 0,
            ..lead(Some(32_000))
        };
        let chosen = selection(&["coder", "tester"], true);
        let without = plan(
            &config(AdaptiveAllocation::Plan, 0),
            None,
            Some(&chosen),
            &budget,
        );
        assert!(!without.unwrap().follow_up);
        let repairs = plan(
            &config(AdaptiveAllocation::Plan, 1),
            None,
            Some(&chosen),
            &budget,
        );
        assert!(repairs.unwrap().follow_up);
        let research = selection(&["researcher"], true);
        let alone = plan(
            &config(AdaptiveAllocation::Plan, 1),
            None,
            Some(&research),
            &budget,
        );
        assert!(!alone.unwrap().follow_up, "no change to repair");
    }

    #[test]
    fn the_lead_floor_is_its_ceiling_or_the_pools_turn_default() {
        let on = config(AdaptiveAllocation::Plan, 0);
        let ceiling = GenerationBudget {
            max_output_tokens: Some(4_096),
            ..lead(Some(32_000))
        };
        assert_eq!(plan(&on, None, None, &ceiling).unwrap().lead_floor, 4_096);
        assert_eq!(
            plan(&on, None, None, &lead(Some(32_000)))
                .unwrap()
                .lead_floor,
            2_048
        );
    }

    #[test]
    fn install_divides_the_pool_once_and_says_how() {
        let on = config(AdaptiveAllocation::Plan, 0);
        let budget = lead(Some(32_000));
        let chosen = selection(&["coder", "tester"], true);
        let line = install(&on, None, Some(&chosen), &budget).unwrap();
        // 25% lead, 10% follow-up (one continuation), 20,800 split 3 : 1.
        assert_eq!(
            line,
            "· Output allocation (plan, role defaults): lead 8000, follow-up 3200, coder 15600 \
             (heavy), tester 5200 (light), unallocated 0 of 32000"
        );
        let again = install(&on, None, Some(&chosen), &budget).unwrap();
        assert!(again.contains("already installed"), "{again}");
    }

    /// The source, the fallback reason and the worker shares of 32,000
    /// (lead 8,000 and follow-up 3,200 in every case) for `roles`.
    fn outcome(
        mode: AdaptiveAllocation,
        complexity: Option<Complexity>,
        roles: &[&str],
    ) -> (String, Option<String>, Vec<u32>) {
        let classified = complexity.map(|complexity| report(complexity, 0.95));
        let chosen = selection(roles, true);
        let budget = lead(Some(32_000));
        let plan = plan(
            &config(mode, 0),
            classified.as_ref(),
            Some(&chosen),
            &budget,
        )
        .unwrap();
        let allocation = lightagent_core::allocation::allocate(32_000, &plan);
        assert_eq!((allocation.lead, allocation.follow_up), (8_000, 3_200));
        (plan.source, plan.fallback_reason, allocation.shares)
    }

    #[test]
    fn jev_differs_from_plan_only_for_a_complex_task_that_plans_a_researcher() {
        use AdaptiveAllocation::{Jev, Plan};
        let research = ["researcher", "reviewer"];
        let as_plan = outcome(Plan, Some(Complexity::Complex), &research);
        assert_eq!(
            as_plan,
            ("role_defaults".into(), None, vec![10_400, 10_400])
        );
        // Complex: the researcher is raised to heavy, 3 : 2.
        assert_eq!(
            outcome(Jev, Some(Complexity::Complex), &research),
            ("jev_complexity".into(), None, vec![12_480, 8_320])
        );
        // Simple or moderate: identical to `plan`, and said so. (Selection
        // keeps a simple task with the lead, so it never has a plan.)
        for level in [Complexity::Simple, Complexity::Moderate] {
            assert_eq!(
                outcome(Jev, Some(level), &research),
                (
                    "role_defaults".into(),
                    Some("complexity_not_complex".into()),
                    vec![10_400, 10_400]
                )
            );
        }
        // No researcher planned: complex changes nothing (a coder is heavy
        // already), and says so; nor for a sole share.
        let code = ["coder", "tester"];
        assert_eq!(
            outcome(Jev, Some(Complexity::Complex), &code),
            (
                "role_defaults".into(),
                Some("complexity_changes_no_share".into()),
                vec![15_600, 5_200]
            )
        );
        assert_eq!(outcome(Plan, None, &code).2, vec![15_600, 5_200]);
        assert_eq!(
            outcome(Jev, Some(Complexity::Complex), &["researcher"]),
            (
                "role_defaults".into(),
                Some("complexity_changes_no_share".into()),
                vec![20_800]
            )
        );
    }
}
