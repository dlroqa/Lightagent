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
use lightagent_core::{
    Config, JevClassificationMode, PolicyEngine, RunLimits, SpecialistSelection,
};
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
    assert!(
        lead >= 10 && set.cases.len() - lead >= 10,
        "both classes covered"
    );
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
    assert_eq!(
        score_case(coding, &plan_of(&["coder"])).roles_ok,
        Some(true)
    );
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
    let mut body: Value =
        serde_json::from_str(&crate::coordinator::test_support::answer("coding", 0.95)).unwrap();
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
const DEFAULT_URL: &str = "https://api.typesafe.ai";
const DEFAULT_REQUESTS: u32 = 24;
const MAX_REQUESTS: u32 = 48;

fn env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

/// The request ceiling of a run: the variable, else one pass over the set,
/// never above [`MAX_REQUESTS`].
fn ceiling() -> u32 {
    env("LIGHTAGENT_SELECTION_LIVE_MAX_REQUESTS")
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(DEFAULT_REQUESTS)
        .min(MAX_REQUESTS)
}

/// The configuration every selection case runs under, against `base_url`.
fn selection_config(base_url: String, api_key: Option<lightagent_core::SecretRef>) -> Config {
    let mut config = super::config_for(&super::Endpoint {
        base_url,
        api_key,
        model: env("LIGHTAGENT_JEV_LIVE_MODEL"),
        timeout_secs: 10,
    });
    config.subagents.selection = JevClassificationMode::Shadow;
    config
}

/// What one pass over the set produced.
struct SetRun {
    scored: Vec<Scored>,
    requests: u32,
    input_tokens: u64,
    output_tokens: u64,
    latencies: Vec<u64>,
    failures: BTreeMap<String, usize>,
    stopped: Option<&'static str>,
}

/// Classify and plan each case in turn, one request at a time, within
/// `limit` requests. A credential failure, or three failures in a row, stops
/// the run so nothing more is spent on a broken endpoint.
async fn run_set(set: &SelectionSet, config: &Config, limit: u32) -> SetRun {
    let mut run = SetRun {
        scored: Vec::new(),
        requests: 0,
        input_tokens: 0,
        output_tokens: 0,
        latencies: Vec::new(),
        failures: BTreeMap::new(),
        stopped: None,
    };
    let mut consecutive_failures = 0u32;
    for case in &set.cases {
        if run.requests >= limit {
            run.stopped = Some("request_ceiling");
            break;
        }
        let report = crate::coordinator::classify::classify(
            config,
            super::PERMITTED_PROFILE,
            &case.prompt,
            false,
            &tokio_util::sync::CancellationToken::new(),
        )
        .await
        .expect("classification is on");
        run.requests += u32::from(report.invoked);
        if let Some(latency) = report.latency {
            run.latencies
                .push(u64::try_from(latency.as_millis()).unwrap_or(u64::MAX));
        }
        match &report.outcome {
            Outcome::Classified(classification) => {
                consecutive_failures = 0;
                if let Some(usage) = classification.usage {
                    run.input_tokens += usage.input_tokens.unwrap_or(0);
                    run.output_tokens += usage.output_tokens.unwrap_or(0);
                }
            }
            Outcome::Failed(code) | Outcome::Skipped(code) => {
                *run.failures.entry((*code).to_owned()).or_default() += 1;
                consecutive_failures += 1;
                if super::failure_class(code) == "credential" || consecutive_failures >= 3 {
                    run.stopped = Some("endpoint_failures");
                }
            }
        }
        let result = score_case(case, &plan(config, &report));
        println!(
            "EVIDENCE {} planned={} decision_ok={} roles_ok={:?} fallback={:?}",
            result.id, result.planned, result.decision_ok, result.roles_ok, result.fallback
        );
        run.scored.push(result);
        if run.stopped.is_some() {
            break;
        }
    }
    run
}

/// Write the run's report and summary; whether it meets the pass rule.
fn finish(set: &SelectionSet, run: &SetRun, measurement: &str, limit: u32) -> bool {
    let (totals, passed) = totals(set, &run.scored);
    let status = if run.stopped == Some("endpoint_failures") {
        "FAILED"
    } else if passed {
        "PASSED"
    } else {
        "BELOW_THRESHOLD"
    };
    let report = json!({
        "schema": "lightagent.specialist_selection_report.v1",
        "evaluation": "jev_specialist_selection",
        "status": status,
        "evaluated": true,
        "counts_as_competence_evidence": false,
        "measurement": measurement,
        "commit": env("LIGHTAGENT_JEV_LIVE_COMMIT"),
        "requests": run.requests,
        "request_ceiling": limit,
        "retries": 0,
        "input_tokens": run.input_tokens,
        "output_tokens": run.output_tokens,
        "latency_ms": run.latencies,
        "failures": run.failures,
        "stopped": run.stopped,
        "totals": totals,
        "cases": run.scored.iter().map(|s| json!({
            "id": s.id,
            "planned": s.planned,
            "decision_ok": s.decision_ok,
            "roles_ok": s.roles_ok,
            "fallback": s.fallback,
        })).collect::<Vec<_>>(),
    });
    let summary = format!(
        "### Specialist selection ({measurement}): {status}\n\nRequests {} of {limit}; tokens in {}, out {}; stopped: {}. This measures Jev's selection only, not specialist competence.\n\n```json\n{}\n```\n",
        run.requests,
        run.input_tokens,
        run.output_tokens,
        run.stopped.unwrap_or("no"),
        serde_json::to_string_pretty(&totals).unwrap_or_default()
    );
    super::write_report("specialist-selection", &report, &summary);
    println!("{summary}");
    assert!(
        run.stopped != Some("endpoint_failures"),
        "the endpoint failed: {:?}",
        run.failures
    );
    passed
}

#[tokio::test]
#[ignore = "calls the live TypeSafe API; run only by the protected jev-live workflow"]
async fn live_specialist_selection_evaluation() {
    assert!(
        env(KEY_VARIABLE).is_some(),
        "{KEY_VARIABLE} is not set: live selection evaluation is BLOCKED, not passed"
    );
    let limit = ceiling();
    let config = selection_config(
        env("LIGHTAGENT_JEV_LIVE_URL").unwrap_or_else(|| DEFAULT_URL.to_owned()),
        Some(lightagent_core::SecretRef::env(KEY_VARIABLE)),
    );
    let set = set();
    let run = run_set(&set, &config, limit).await;
    assert!(
        finish(&set, &run, "live_jev", limit),
        "below the recorded pass rule"
    );
}

/// Write each case's exact request body to `dir`, as `<id>.json`, with a
/// manifest. Nothing is sent and no credential is read.
fn export_requests(dir: &std::path::Path) -> Vec<String> {
    let config = selection_config(DEFAULT_URL.to_owned(), None);
    std::fs::create_dir_all(dir).expect("the export directory can be created");
    let mut ids = Vec::new();
    for case in &set().cases {
        let request = crate::coordinator::classify::request_for(&config, &case.prompt)
            .expect("every recorded prompt is sendable");
        let body = serde_json::to_string(&request).expect("a request serializes");
        std::fs::write(dir.join(format!("{}.json", case.id)), body).expect("written");
        ids.push(case.id.clone());
    }
    let manifest = json!({
        "schema": "lightagent.specialist_selection_requests.v1",
        "commit": env("LIGHTAGENT_JEV_LIVE_COMMIT"),
        "endpoint_path": "/v1/systemone",
        "cases": ids,
    });
    std::fs::write(dir.join("manifest.json"), manifest.to_string()).expect("written");
    ids
}

/// A loopback Jev that answers each request with the recorded response to
/// the exported request of the same state, and 404 to anything else.
async fn replay_server(requests: &std::path::Path, responses: &std::path::Path) -> String {
    use tokio::io::AsyncWriteExt;
    let mut answers: BTreeMap<String, String> = BTreeMap::new();
    for case in &set().cases {
        let Ok(request) = std::fs::read_to_string(requests.join(format!("{}.json", case.id)))
        else {
            continue;
        };
        let Ok(response) = std::fs::read_to_string(responses.join(format!("{}.json", case.id)))
        else {
            continue;
        };
        let state = serde_json::from_str::<Value>(&request)
            .ok()
            .and_then(|body| body["state"].as_str().map(str::to_owned));
        if let Some(state) = state {
            answers.insert(state, response);
        }
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let answers = std::sync::Arc::new(answers);
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let answers = std::sync::Arc::clone(&answers);
            tokio::spawn(async move {
                let body = crate::coordinator::classify_read_request(&mut stream).await;
                let state = serde_json::from_str::<Value>(&body)
                    .ok()
                    .and_then(|body| body["state"].as_str().map(str::to_owned));
                let (status, text) = match state.and_then(|state| answers.get(&state).cloned()) {
                    Some(text) => (200, text),
                    None => (404, "{}".to_owned()),
                };
                let response = format!(
                    "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{text}",
                    text.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
            });
        }
    });
    format!("http://{address}")
}

#[test]
#[ignore = "export: writes the selection requests for the protected send job"]
fn export_selection_requests() {
    let dir =
        env("LIGHTAGENT_SELECTION_EXPORT").expect("LIGHTAGENT_SELECTION_EXPORT names a directory");
    let ids = export_requests(std::path::Path::new(&dir));
    println!("EVIDENCE exported {} requests", ids.len());
}

#[tokio::test]
#[ignore = "replay: scores responses the protected send job recorded"]
async fn replay_specialist_selection_evaluation() {
    let requests = env("LIGHTAGENT_SELECTION_EXPORT").expect("the exported requests");
    let responses = env("LIGHTAGENT_SELECTION_RESPONSES").expect("the recorded responses");
    let url = replay_server(
        std::path::Path::new(&requests),
        std::path::Path::new(&responses),
    )
    .await;
    let limit = ceiling();
    let set = set();
    let run = run_set(&set, &selection_config(url, None), limit).await;
    assert!(
        finish(&set, &run, "replayed_live_jev", limit),
        "below the recorded pass rule"
    );
}

#[tokio::test]
async fn exported_requests_replay_exactly_and_unknown_requests_are_not_answered() {
    let root = std::env::temp_dir().join(format!(
        "lightagent-selection-replay-{}",
        lightagent_core::RunId::new().as_str()
    ));
    let (requests, responses) = (root.join("requests"), root.join("responses"));
    let ids = export_requests(&requests);
    assert_eq!(ids.len(), set().cases.len());
    // Every export is one typed request: no free-form generation, nothing
    // beyond the recorded synthetic prompt in its state.
    let first: Value =
        serde_json::from_str(&std::fs::read_to_string(requests.join("sel-016.json")).unwrap())
            .unwrap();
    assert_eq!(first["questions"].as_object().unwrap().len(), 11);
    assert!(first.get("api_key").is_none() && !first.to_string().contains("Bearer"));
    // Record a stand-in response for one case only.
    let mut body: Value =
        serde_json::from_str(&crate::coordinator::test_support::answer("coding", 0.95)).unwrap();
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
    std::fs::create_dir_all(&responses).unwrap();
    std::fs::write(responses.join("sel-016.json"), body.to_string()).unwrap();
    let url = replay_server(&requests, &responses).await;
    let config = selection_config(url, None);
    let set = set();
    let recorded = set.cases.iter().find(|case| case.id == "sel-016").unwrap();
    let report = crate::coordinator::classify::classify(
        &config,
        super::PERMITTED_PROFILE,
        &recorded.prompt,
        false,
        &tokio_util::sync::CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(
        score_case(recorded, &plan(&config, &report)).roles_ok,
        Some(true)
    );
    // A case with no recorded response is a failure, never a guess.
    let unrecorded = set.cases.iter().find(|case| case.id == "sel-001").unwrap();
    let report = crate::coordinator::classify::classify(
        &config,
        super::PERMITTED_PROFILE,
        &unrecorded.prompt,
        false,
        &tokio_util::sync::CancellationToken::new(),
    )
    .await
    .unwrap();
    assert!(
        matches!(report.outcome, Outcome::Failed(_)),
        "{:?}",
        report.outcome
    );
    let _ = std::fs::remove_dir_all(root);
}

/// The owner-approved live run 40503493185 (2026-10-27, `jev-1.14.0`), kept
/// for offline diagnosis. Development evidence only: replaying it reproduces
/// that run's answers to that run's questions, never a new live result, and
/// never how Jev would answer different questions.
fn recorded_run() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../evaluation/subagents/runs/2026-10-27-40503493185")
}

/// How each prompt ended in the recorded run, as its score job logged it.
const RECORDED_FALLBACKS: [(&str, &str); 24] = [
    ("sel-001", "simple_task"),
    ("sel-002", "simple_task"),
    ("sel-003", "low_confidence"),
    ("sel-004", "simple_task"),
    ("sel-005", "simple_task"),
    ("sel-006", "simple_task"),
    ("sel-007", "low_confidence"),
    ("sel-008", "simple_task"),
    ("sel-009", "simple_task"),
    ("sel-010", "low_confidence"),
    ("sel-011", "low_confidence"),
    ("sel-012", "low_confidence"),
    ("sel-013", "low_confidence"),
    ("sel-014", "low_confidence"),
    ("sel-015", "low_confidence"),
    ("sel-016", "delegation_not_useful"),
    ("sel-017", "low_confidence"),
    ("sel-018", "low_confidence"),
    ("sel-019", "low_confidence"),
    ("sel-020", "low_confidence"),
    ("sel-021", "delegation_not_useful"),
    ("sel-022", "low_confidence"),
    ("sel-023", "low_confidence"),
    ("sel-024", "low_confidence"),
];

fn read_json(path: &std::path::Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).expect("a recorded file"))
        .expect("recorded JSON")
}

/// The replay answers these exact requests only: a request that differs in
/// any byte from the recorded one (another question, another wording) gets
/// no recorded answer to stand in for Jev's.
#[test]
fn the_recorded_requests_are_what_this_code_exports() {
    let dir = std::env::temp_dir().join(format!(
        "lightagent-selection-export-{}",
        lightagent_core::RunId::new().as_str()
    ));
    let ids = export_requests(&dir);
    let recorded = recorded_run().join("requests");
    for id in &ids {
        let exported = std::fs::read_to_string(dir.join(format!("{id}.json"))).unwrap();
        let kept = std::fs::read_to_string(recorded.join(format!("{id}.json"))).unwrap();
        assert_eq!(exported, kept, "{id} is no longer the recorded request");
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn the_recorded_run_replays_to_its_recorded_result() {
    let dir = recorded_run();
    let url = replay_server(&dir.join("requests"), &dir.join("responses")).await;
    let set = set();
    let run = run_set(&set, &selection_config(url, None), DEFAULT_REQUESTS).await;
    let ended: Vec<(&str, &str)> = run
        .scored
        .iter()
        .map(|s| (s.id.as_str(), s.fallback.as_deref().unwrap_or("plan")))
        .collect();
    assert_eq!(ended, RECORDED_FALLBACKS);
    assert_eq!((run.requests, run.failures.len()), (24, 0));
    let (totals, passed) = totals(&set, &run.scored);
    assert!(!passed);
    assert_eq!(totals["decision_accuracy"]["numerator"], 12);
    assert_eq!(totals["missed_delegation"]["numerator"], 12);
    let verdict = read_json(&dir.join("verdict.json"));
    assert_eq!(verdict["status"], "BELOW_THRESHOLD");
    assert_eq!(verdict["metrics"]["decision_accuracy"]["numerator"], 12);
}

/// Precision and recall of `predicted` against `expected`, per case.
fn precision_recall(pairs: &[(bool, bool)]) -> Value {
    let tp = pairs.iter().filter(|(e, p)| *e && *p).count();
    let fp = pairs.iter().filter(|(e, p)| !*e && *p).count();
    let fn_ = pairs.iter().filter(|(e, p)| *e && !*p).count();
    json!({"precision": ratio(tp, tp + fp), "recall": ratio(tp, tp + fn_)})
}

/// The share of the best-matching labeled role set that `got` covers, and
/// whether `got` equals one of the sets.
fn role_coverage(labeled: &[Vec<String>], got: &BTreeSet<String>) -> (f64, bool) {
    let mut best = 0.0f64;
    let mut exact = false;
    for set in labeled {
        let want: BTreeSet<String> = set.iter().cloned().collect();
        let covered = want.intersection(got).count() as f64 / want.len() as f64;
        best = best.max(covered);
        exact |= &want == got;
    }
    (best, exact)
}

fn planned_roles(record: &SpecialistSelection) -> BTreeSet<String> {
    if record.outcome != "plan" {
        return BTreeSet::new();
    }
    record.steps.iter().map(|step| step.role.clone()).collect()
}

fn ended(record: &SpecialistSelection) -> String {
    match record.outcome.as_str() {
        "plan" => "plan".to_owned(),
        _ => record.fallback_reason.clone().unwrap_or_default(),
    }
}

/// Codes only the run's own permissions, readiness or limits produce.
const RUNTIME_CODES: [&str; 3] = ["delegation_unavailable", "no_ready_role", "insufficient_budget"];

/// The recorded run traced prompt by prompt: what was sent, what Jev
/// answered, which rule ended selection and with what threshold, and what
/// the later rules would have done had the earlier ones passed. Every value
/// comes from the recorded answers through this code's own parsing and
/// rules; the "if passed" plans are counterfactual readings of the same
/// answers, not predictions of Jev.
#[tokio::test]
async fn diagnose_the_recorded_run() {
    let dir = recorded_run();
    let url = replay_server(&dir.join("requests"), &dir.join("responses")).await;
    let config = selection_config(url, None);
    let selection_threshold = f64::from(config.subagents.selection_threshold_percent) / 100.0;
    let set = set();
    let mut rows = Vec::new();
    let mut table = String::from(
        "| Case | Expected | Category (conf) | Complexity | specialists | researcher / coder / reviewer / tester | Structure | Rule that ended it | Next rule if confidence passed | Plan from roles alone |\n| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |\n",
    );
    let (mut actual, mut raw, mut roles_only) = (Vec::new(), Vec::new(), Vec::new());
    let mut coverage = BTreeMap::<&str, (f64, usize)>::new();
    for case in &set.cases {
        let request = read_json(&dir.join("requests").join(format!("{}.json", case.id)));
        let response = read_json(&dir.join("responses").join(format!("{}.json", case.id)));
        let report = crate::coordinator::classify::classify(
            &config,
            super::PERMITTED_PROFILE,
            &case.prompt,
            false,
            &tokio_util::sync::CancellationToken::new(),
        )
        .await
        .expect("classification is on");
        let Outcome::Classified(classification) = &report.outcome else {
            panic!("{} did not replay: {:?}", case.id, report.outcome);
        };
        let answers = classification.selection.as_ref().expect("selection asked");
        let record = plan(&config, &report);
        // The same answers with the category-confidence rule passed, and
        // then with the specialists rule passed too: the rules after them.
        let mut past_confidence = report.clone();
        let mut past_both = report.clone();
        if let Outcome::Classified(c) = &mut past_confidence.outcome {
            c.category_confidence = 1.0;
        }
        if let Outcome::Classified(c) = &mut past_both.outcome {
            c.category_confidence = 1.0;
            if let Some(s) = c.selection.as_mut() {
                s.delegation = 1.0;
            }
        }
        let next = plan(&config, &past_confidence);
        let from_roles = plan(&config, &past_both);
        let recommended: BTreeSet<String> = answers
            .roles
            .iter()
            .filter(|(_, p)| **p >= selection_threshold)
            .map(|(role, _)| role.as_str().to_owned())
            .collect();
        let runtime_affected = [&record, &next, &from_roles].iter().any(|r| {
            RUNTIME_CODES.contains(&ended(r).as_str())
                || r.restricted.iter().any(|x| {
                    !matches!(x.reason.as_str(), "not_for_this_task" | "worker_cap" | "needs_coder")
                })
        });
        let state = request["state"].as_str().unwrap_or_default();
        let role_p = |role: WorkerRole| answers.roles.get(&role).copied().unwrap_or(f64::NAN);
        let delegation_recommended = answers.delegation >= selection_threshold;
        actual.push((case.delegate, record.outcome == "plan"));
        raw.push((case.delegate, delegation_recommended));
        roles_only.push((case.delegate, from_roles.outcome == "plan"));
        if case.delegate {
            for (name, got) in [
                ("planned", planned_roles(&record)),
                ("jev_roles_at_threshold", recommended.clone()),
                ("plan_from_roles_alone", planned_roles(&from_roles)),
            ] {
                let (covered, exact) = role_coverage(&case.roles, &got);
                let entry = coverage.entry(name).or_default();
                entry.0 += covered;
                entry.1 += usize::from(exact);
            }
        }
        rows.push(json!({
            "id": case.id,
            "expected": {"delegate": case.delegate, "roles": case.roles},
            "state_sent": state,
            "state_equals_prompt": state == case.prompt,
            "jev": {
                "model": response["model"],
                "category": classification.category.as_str(),
                "category_confidence": classification.category_confidence,
                "category_probabilities": response["answers"]["task"]["probabilities"],
                "complexity_score": response["answers"]["complexity"]["score"],
                "complexity": classification.complexity.as_str(),
                "complexity_confidence": classification.complexity_confidence,
                "planning": classification.planning_probability,
                "retrieval": classification.retrieval_probability,
                "tools": classification.tools_probability,
                "specialists": answers.delegation,
                "roles": answers.roles.iter().map(|(r, p)| (r.as_str().to_owned(), json!(p))).collect::<serde_json::Map<_, _>>(),
                "structure": format!("{:?}", answers.structure).to_lowercase(),
                "structure_confidence": answers.structure_confidence,
            },
            "rules": {
                "category_confidence": {"value": classification.category_confidence, "threshold": classification.threshold, "passes": classification.accepted()},
                "not_simple": classification.complexity.as_str() != "simple",
                "not_conversation": classification.category.as_str() != "conversation",
                "specialists": {"value": answers.delegation, "threshold": selection_threshold, "passes": delegation_recommended},
                "roles_at_threshold": recommended,
            },
            "ended": ended(&record),
            "next_rule_if_confidence_passed": ended(&next),
            "plan_from_roles_alone": {"ended": ended(&from_roles), "roles": planned_roles(&from_roles), "restricted": from_roles.restricted.iter().map(|x| format!("{}:{}", x.role, x.reason)).collect::<Vec<_>>()},
            "runtime_checks_affected_decision": runtime_affected,
        }));
        table.push_str(&format!(
            "| {} | {} | {} ({:.2}) | {} ({}) | {:.2} | {:.2} / {:.2} / {:.2} / {:.2} | {:?} ({:.2}) | {} | {} | {} {:?} |\n",
            case.id,
            if case.delegate { format!("delegate {:?}", case.roles) } else { "lead".to_owned() },
            classification.category.as_str(),
            classification.category_confidence,
            classification.complexity.as_str(),
            response["answers"]["complexity"]["score"],
            answers.delegation,
            role_p(WorkerRole::Researcher),
            role_p(WorkerRole::Coder),
            role_p(WorkerRole::Reviewer),
            role_p(WorkerRole::Tester),
            answers.structure,
            answers.structure_confidence,
            ended(&record),
            ended(&next),
            ended(&from_roles),
            planned_roles(&from_roles),
        ));
    }
    let delegate_cases = set.cases.iter().filter(|case| case.delegate).count();
    let coverage_json: serde_json::Map<String, Value> = coverage
        .iter()
        .map(|(name, (sum, exact))| {
            (
                (*name).to_owned(),
                json!({
                    "mean_expected_role_coverage": sum / delegate_cases as f64,
                    "exact_role_set_match": ratio(*exact, delegate_cases),
                }),
            )
        })
        .collect();
    let report = json!({
        "schema": "lightagent.specialist_selection_diagnosis.v1",
        "run": 40503493185u64,
        "evidence": "development: recorded answers replayed through this code; not a live result",
        "selection_threshold": selection_threshold,
        "category_confidence_threshold": config.platform.jev.classification.default_threshold,
        "delegation": {
            "planned": precision_recall(&actual),
            "jev_specialists_at_threshold": precision_recall(&raw),
            "plan_from_roles_alone": precision_recall(&roles_only),
        },
        "should_delegate_role_coverage": coverage_json,
        "cases": rows,
    });
    let summary = format!(
        "### Selection diagnosis: recorded run 40503493185 (development evidence, replayed)\n\n{table}\n```json\n{}\n```\n",
        serde_json::to_string_pretty(&json!({
            "delegation": report["delegation"],
            "should_delegate_role_coverage": report["should_delegate_role_coverage"],
        }))
        .unwrap_or_default()
    );
    super::write_report("selection-diagnosis", &report, &summary);
    println!("{summary}");
    assert_eq!(rows.len(), 24);
}
