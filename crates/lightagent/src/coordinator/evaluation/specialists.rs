//! Specialist-selection accuracy: `evaluation/subagents/selection.v1.json`.
//!
//! The labeled set and its scoring rules were recorded before any request.
//! The deterministic tests here check the set and the scorer in CI. The live
//! run sends each prompt through the real classification request (with the
//! specialist questions) and the real plan rules, against the real Jev
//! endpoint, from the protected `jev-live` environment only; without its key
//! it fails loudly and is never reported as a pass. It measures Jev's
//! selection on these prompts only: not task completion, not specialist
//! competence, and not the lead model's own choice of workers, which needs a
//! general inference endpoint this repository does not have.

use std::collections::{BTreeMap, BTreeSet};

use lightagent_core::identity::RunTrust;
use lightagent_core::permissions::ApprovalPolicy;
use lightagent_core::workers::{Readiness, RoleReadiness, WorkerRole};
use lightagent_core::{Config, JevClassificationMode, PolicyEngine, RunLimits, SpecialistSelection};
use lightagent_tools::ToolRegistry;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::coordinator::classify::{ClassificationReport, Outcome};
use crate::coordinator::{sanitize, selection};

const SET: &str = include_str!("../../../../../evaluation/subagents/selection.v1.json");

#[derive(Debug, Deserialize)]
struct SelectionSet {
    schema: String,
    pass: Pass,
    cases: Vec<SelectionCase>,
}

#[derive(Debug, Deserialize)]
struct Pass {
    decision_accuracy_min: f64,
    unnecessary_delegation_max: f64,
}

#[derive(Clone, Debug, Deserialize)]
struct SelectionCase {
    id: String,
    prompt: String,
    delegate: bool,
    #[serde(default)]
    roles: Vec<Vec<String>>,
}

fn set() -> SelectionSet {
    serde_json::from_str(SET).expect("the selection set parses")
}

/// How one case scored.
#[derive(Clone, Debug, PartialEq)]
struct Scored {
    id: String,
    planned: bool,
    decision_ok: bool,
    unnecessary: bool,
    missed: bool,
    /// Whether the planned roles are a labeled set, when both delegate.
    roles_ok: Option<bool>,
    /// The fallback code when the lead kept the task.
    fallback: Option<String>,
}

fn score_case(case: &SelectionCase, record: &SpecialistSelection) -> Scored {
    let planned = record.outcome == "plan";
    let roles_ok = (planned && case.delegate).then(|| {
        let got: BTreeSet<&str> = record.steps.iter().map(|step| step.role.as_str()).collect();
        case.roles.iter().any(|set| {
            let want: BTreeSet<&str> = set.iter().map(String::as_str).collect();
            want == got
        })
    });
    Scored {
        id: case.id.clone(),
        planned,
        decision_ok: planned == case.delegate,
        unnecessary: planned && !case.delegate,
        missed: !planned && case.delegate,
        roles_ok,
        fallback: record.fallback_reason.clone(),
    }
}

fn ratio(numerator: usize, denominator: usize) -> Value {
    json!({
        "numerator": numerator,
        "denominator": denominator,
        "value": (denominator > 0).then(|| numerator as f64 / denominator as f64),
    })
}

/// The totals of a run, and whether they meet the recorded pass rule.
fn totals(set: &SelectionSet, scored: &[Scored]) -> (Value, bool) {
    let count = |keep: &dyn Fn(&Scored) -> bool| scored.iter().filter(|s| keep(s)).count();
    let lead_cases = set.cases.iter().filter(|case| !case.delegate).count();
    let decisions = count(&|s| s.decision_ok);
    let unnecessary = count(&|s| s.unnecessary);
    let both = count(&|s| s.roles_ok.is_some());
    let roles = count(&|s| s.roles_ok == Some(true));
    let accuracy = decisions as f64 / scored.len().max(1) as f64;
    let unnecessary_rate = unnecessary as f64 / lead_cases.max(1) as f64;
    let passed = scored.len() == set.cases.len()
        && accuracy >= set.pass.decision_accuracy_min
        && unnecessary_rate <= set.pass.unnecessary_delegation_max;
    let report = json!({
        "cases": scored.len(),
        "decision_accuracy": ratio(decisions, scored.len()),
        "unnecessary_delegation": ratio(unnecessary, lead_cases),
        "missed_delegation": ratio(count(&|s| s.missed), scored.len() - lead_cases.min(scored.len())),
        "role_match": ratio(roles, both),
        "passed": passed,
    });
    (report, passed)
}

/// Every role ready, as for a profile with the built-in workers.
fn all_ready() -> Vec<RoleReadiness> {
    WorkerRole::ALL
        .into_iter()
        .map(|role| RoleReadiness {
            role,
            state: Readiness::Ready,
            worker: format!("builtin:{}@1", role.as_str()),
            model: None,
            model_source: "lead",
            skill: None,
            tools: Vec::new(),
            reasons: Vec::new(),
        })
        .collect()
}

/// The plan Lightagent builds from `report` for an operator run with every
/// built-in ready, the default limits and the balanced policy.
fn plan(config: &Config, report: &ClassificationReport) -> SpecialistSelection {
    let registry = ToolRegistry::builtin();
    let policy = PolicyEngine::new(ApprovalPolicy::balanced());
    let readiness = all_ready();
    let limits = RunLimits::default();
    selection::decide(&selection::SelectionInputs {
        config,
        profile: super::PERMITTED_PROFILE,
        report,
        readiness: &readiness,
        registry: &registry,
        policy: &policy,
        limits: &limits,
        trust: RunTrust::LocalOperator,
        cancelled: false,
    })
    .map(|chosen| chosen.record)
    .expect("selection is on")
}

#[test]
fn the_selection_set_is_well_formed_and_balanced() {
    let set = set();
    assert_eq!(set.schema, "lightagent.specialist_selection_eval.v1");
    let ids: BTreeSet<&str> = set.cases.iter().map(|case| case.id.as_str()).collect();
    assert_eq!(ids.len(), set.cases.len(), "ids are unique");
    let roles: BTreeSet<&str> = WorkerRole::ALL.iter().map(|role| role.as_str()).collect();
    for case in &set.cases {
        // Every prompt is something the classifier would actually send.
        assert!(
            sanitize::summarize(&case.prompt, 400).is_ok(),
            "{} would not be sent",
            case.id
        );
        assert_eq!(case.delegate, !case.roles.is_empty(), "{}", case.id);
        for labeled in &case.roles {
            assert!(!labeled.is_empty() && labeled.len() < 4, "{}", case.id);
            assert!(labeled.iter().all(|role| roles.contains(role.as_str())));
        }
    }
    let lead = set.cases.iter().filter(|case| !case.delegate).count();
    assert!(lead >= 10 && set.cases.len() - lead >= 10, "both classes covered");
    assert!(set.pass.decision_accuracy_min > 0.5);
}

#[test]
fn the_scorer_counts_unnecessary_and_missed_delegation_and_role_sets() {
    let set = set();
    let lead_case = set.cases.iter().find(|case| !case.delegate).unwrap();
    let coding = set.cases.iter().find(|case| case.id == "sel-021").unwrap();
    let step = |role: &str| lightagent_core::SpecialistStep {
        role: role.to_owned(),
        ..Default::default()
    };
    let plan_of = |roles: &[&str]| SpecialistSelection {
        outcome: "plan".to_owned(),
        steps: roles.iter().map(|role| step(role)).collect(),
        ..Default::default()
    };
    let kept = SpecialistSelection {
        outcome: "lead".to_owned(),
        fallback_reason: Some("simple_task".to_owned()),
        ..Default::default()
    };
    assert!(score_case(lead_case, &kept).decision_ok);
    let unnecessary = score_case(lead_case, &plan_of(&["researcher"]));
    assert!(unnecessary.unnecessary && !unnecessary.decision_ok);
    let missed = score_case(coding, &kept);
    assert!(missed.missed && missed.fallback.as_deref() == Some("simple_task"));
    // Either labeled set matches, in any order; anything else does not.
    assert_eq!(
        score_case(coding, &plan_of(&["tester", "coder"])).roles_ok,
        Some(true)
    );
    assert_eq!(score_case(coding, &plan_of(&["coder"])).roles_ok, Some(true));
    assert_eq!(
        score_case(coding, &plan_of(&["coder", "reviewer"])).roles_ok,
        Some(false)
    );
    // A run that scored every case correctly passes; a partial run never does.
    let perfect: Vec<Scored> = set
        .cases
        .iter()
        .map(|case| {
            let record = match case.roles.first() {
                Some(roles) => plan_of(&roles.iter().map(String::as_str).collect::<Vec<_>>()),
                None => kept.clone(),
            };
            score_case(case, &record)
        })
        .collect();
    assert!(totals(&set, &perfect).1);
    assert!(!totals(&set, &perfect[..5]).1);
}

#[tokio::test]
async fn the_live_run_scores_what_the_plan_rules_build() {
    // A scripted Jev answering every prompt alike: the scores are those of
    // the stub, which says nothing about Jev. This only proves the path the
    // live run takes: one request per prompt, plan, score.
    let mut body: Value = serde_json::from_str(&crate::coordinator::test_support::answer(
        "coding", 0.95,
    ))
    .unwrap();
    let answers = body["answers"].as_object_mut().unwrap();
    answers.insert("specialists".into(), json!({"type": "noul", "noul": 0.9}));
    for (role, probability) in [
        ("researcher", 0.1),
        ("coder", 0.9),
        ("reviewer", 0.1),
        ("tester", 0.9),
    ] {
        answers.insert(
            format!("role_{role}"),
            json!({"type": "noul", "noul": probability}),
        );
    }
    answers.insert(
        "structure".into(),
        json!({"type": "choice", "choice": "sequential", "confidence": 0.9,
               "probabilities": {"single": 0.05, "independent": 0.05, "sequential": 0.9}}),
    );
    let (url, seen) = crate::coordinator::test_support::jev_stub(
        200,
        body.to_string(),
        std::time::Duration::ZERO,
    )
    .await;
    let mut config = super::config_for(&super::Endpoint {
        base_url: url,
        api_key: None,
        model: None,
        timeout_secs: 2,
    });
    config.subagents.selection = JevClassificationMode::Shadow;
    let set = set();
    let case = set.cases.iter().find(|case| case.id == "sel-016").unwrap();
    let report = crate::coordinator::classify::classify(
        &config,
        super::PERMITTED_PROFILE,
        &case.prompt,
        false,
        &tokio_util::sync::CancellationToken::new(),
    )
    .await
    .unwrap();
    assert!(matches!(report.outcome, Outcome::Classified(_)));
    let scored = score_case(case, &plan(&config, &report));
    assert_eq!(scored.roles_ok, Some(true), "{scored:?}");
    assert_eq!(seen.lock().unwrap().len(), 1);
}

const KEY_VARIABLE: &str = "TYPESAFE_API_KEY";
const DEFAULT_REQUESTS: u32 = 24;
const MAX_REQUESTS: u32 = 48;

fn env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

#[tokio::test]
#[ignore = "calls the live TypeSafe API; run only by the protected jev-live workflow"]
async fn live_specialist_selection_evaluation() {
    assert!(
        env(KEY_VARIABLE).is_some(),
        "{KEY_VARIABLE} is not set: live selection evaluation is BLOCKED, not passed"
    );
    let limit = env("LIGHTAGENT_SELECTION_LIVE_MAX_REQUESTS")
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(DEFAULT_REQUESTS)
        .min(MAX_REQUESTS);
    let endpoint = super::Endpoint {
        base_url: env("LIGHTAGENT_JEV_LIVE_URL")
            .unwrap_or_else(|| "https://api.typesafe.ai".to_owned()),
        api_key: Some(lightagent_core::SecretRef::env(KEY_VARIABLE)),
        model: env("LIGHTAGENT_JEV_LIVE_MODEL"),
        timeout_secs: 10,
    };
    let mut config = super::config_for(&endpoint);
    config.subagents.selection = JevClassificationMode::Shadow;
    let set = set();
    let mut scored = Vec::new();
    let mut failures: BTreeMap<String, usize> = BTreeMap::new();
    let (mut requests, mut consecutive_failures) = (0u32, 0u32);
    let (mut input_tokens, mut output_tokens) = (0u64, 0u64);
    let mut latencies = Vec::new();
    let mut stopped = None;
    for case in &set.cases {
        if requests >= limit {
            stopped = Some("request_ceiling");
            break;
        }
        let report = crate::coordinator::classify::classify(
            &config,
            super::PERMITTED_PROFILE,
            &case.prompt,
            false,
            &tokio_util::sync::CancellationToken::new(),
        )
        .await
        .expect("classification is on");
        requests += u32::from(report.invoked);
        if let Some(latency) = report.latency {
            latencies.push(latency.as_millis());
        }
        match &report.outcome {
            Outcome::Classified(classification) => {
                consecutive_failures = 0;
                if let Some(usage) = classification.usage {
                    input_tokens += usage.input_tokens.unwrap_or(0);
                    output_tokens += usage.output_tokens.unwrap_or(0);
                }
            }
            Outcome::Failed(code) | Outcome::Skipped(code) => {
                *failures.entry((*code).to_owned()).or_default() += 1;
                consecutive_failures += 1;
                // A credential problem, or three failures in a row, stops the
                // run: nothing more is spent on a broken endpoint.
                if super::failure_class(code) == "credential" || consecutive_failures >= 3 {
                    stopped = Some("endpoint_failures");
                }
            }
        }
        let record = plan(&config, &report);
        let result = score_case(case, &record);
        println!(
            "EVIDENCE {} planned={} decision_ok={} roles_ok={:?} fallback={:?}",
            result.id, result.planned, result.decision_ok, result.roles_ok, result.fallback
        );
        scored.push(result);
        if stopped.is_some() {
            break;
        }
    }
    let (totals, passed) = totals(&set, &scored);
    let report = json!({
        "schema": "lightagent.specialist_selection_report.v1",
        "measurement": "live_jev",
        "commit": env("LIGHTAGENT_JEV_LIVE_COMMIT"),
        "requests": requests,
        "request_ceiling": limit,
        "input_tokens": input_tokens,
        "output_tokens": output_tokens,
        "latency_ms": latencies,
        "failures": failures,
        "stopped": stopped,
        "totals": totals,
        "cases": scored.iter().map(|s| json!({
            "id": s.id,
            "planned": s.planned,
            "decision_ok": s.decision_ok,
            "roles_ok": s.roles_ok,
            "fallback": s.fallback,
        })).collect::<Vec<_>>(),
    });
    let summary = format!(
        "### Specialist selection (live Jev)\n\nRequests {requests} of {limit}; tokens in {input_tokens}, out {output_tokens}; stopped: {}.\n\n```json\n{}\n```\n",
        stopped.unwrap_or("no"),
        serde_json::to_string_pretty(&totals).unwrap_or_default()
    );
    super::write_report("specialist-selection", &report, &summary);
    println!("{summary}");
    assert!(stopped != Some("endpoint_failures"), "the endpoint failed: {failures:?}");
    assert!(passed, "below the recorded pass rule: {totals}");
}
