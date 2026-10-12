//! The deterministic evaluation suite: dataset integrity, label/policy
//! consistency, and the mock-contract run through the production classifier.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;

use super::*;
use crate::coordinator::classify::tests::read_request;

const GROUPS: [&str; 12] = [
    "conversation",
    "research",
    "coding",
    "analysis",
    "automation",
    "multi_step",
    "ambiguous",
    "retrieval",
    "no_tools",
    "unavailable_capability",
    "privacy",
    "malformed",
];

const ELIGIBILITIES: [&str; 10] = [
    "eligible",
    "default_behavior",
    "advisory_only",
    "unsupported",
    "not_configured",
    "unauthorized",
    "insufficient_budget",
    "uncertain",
    "not_classified",
    "cancelled",
];

fn offline_config() -> Config {
    config_for(&Endpoint {
        base_url: "http://127.0.0.1:9".to_owned(),
        api_key: None,
        model: None,
        timeout_secs: 2,
    })
}

#[test]
fn the_dataset_is_versioned_complete_and_independently_labeled() {
    let dataset = dataset();
    assert_eq!(dataset.schema, 1);
    assert_eq!(dataset.dataset, "lightagent-jev-benchmark");
    assert!(!dataset.version.is_empty() && !dataset.created.is_empty());
    assert!(dataset.rubric.ends_with("#labeling-rubric"));
    assert_eq!(
        dataset.provenance["jev_consulted"], false,
        "labels never come from Jev"
    );
    assert_eq!(dataset.provenance["source"], "synthetic");
    for capability in dataset.capabilities.values() {
        assert!(!capability.description.is_empty());
    }

    let mut ids = BTreeSet::new();
    for case in &dataset.cases {
        let (prefix, number) = case.id.split_once('-').expect("ids are prefix-NNN");
        assert!(
            prefix.chars().all(|c| c.is_ascii_lowercase())
                && number.len() == 3
                && number.chars().all(|c| c.is_ascii_digit()),
            "{} is not a stable id",
            case.id
        );
        assert!(ids.insert(case.id.clone()), "{} is duplicated", case.id);
        assert!(GROUPS.contains(&case.group.as_str()), "{}", case.id);
        assert!(
            ["benchmark", "calibration"].contains(&case.split.as_str()),
            "{}",
            case.id
        );
        assert!(
            dataset.capabilities.contains_key(&case.capability),
            "{}",
            case.id
        );
        for category in &case.categories {
            assert!(TaskCategory::parse(category).is_some(), "{}", case.id);
        }
        if let Some(workflow) = &case.workflow {
            assert!(ELIGIBILITIES.contains(&workflow.as_str()), "{}", case.id);
        }
        match case.privacy.as_str() {
            "send" => {
                assert!(!case.categories.is_empty(), "{} needs a label", case.id);
                assert!(case.workflow.is_some(), "{} needs a workflow", case.id);
                assert!(case.fallback.is_none(), "{}", case.id);
            }
            "refuse" => {
                assert!(case.categories.is_empty(), "{}", case.id);
                assert!(case.workflow.is_none(), "{}", case.id);
                assert!(case.fallback.is_some(), "{} needs a fallback", case.id);
                assert!(
                    case.planning.is_none() && case.retrieval.is_none() && case.tools.is_none()
                );
            }
            other => panic!("{}: unknown privacy expectation {other}", case.id),
        }
        if let Some(notes) = &case.notes {
            assert!(!notes.is_empty());
        }
    }
    let groups: BTreeSet<&str> = dataset
        .cases
        .iter()
        .map(|case| case.group.as_str())
        .collect();
    assert_eq!(
        groups.len(),
        GROUPS.len(),
        "every required group is covered"
    );
    for split in ["benchmark", "calibration"] {
        let primaries: BTreeSet<&str> = dataset
            .cases
            .iter()
            .filter(|case| case.split == split)
            .filter_map(Case::primary)
            .collect();
        for category in TaskCategory::ALL {
            assert!(
                primaries.contains(category.as_str()),
                "{split} split has no {} case",
                category.as_str()
            );
        }
    }
}

#[test]
fn workflow_labels_agree_with_the_workflow_policy() {
    // The workflow label is written from the rubric's capability table; the
    // policy must decide the same for the primary category, accepted.
    let dataset = dataset();
    for case in dataset.cases.iter().filter(|case| case.expects_send()) {
        let primary = TaskCategory::parse(case.primary().unwrap()).unwrap();
        let decision = decide_workflow(&accepted(primary), &dataset.capabilities[&case.capability]);
        assert_eq!(
            Some(decision.eligibility.as_str()),
            case.workflow.as_deref(),
            "{}",
            case.id
        );
    }
}

#[test]
fn privacy_labels_agree_with_the_sanitizer_and_gates_offline() {
    let dataset = dataset();
    let config = offline_config();
    let max_chars = config.platform.jev.classification.max_input_chars;
    for case in &dataset.cases {
        let refusal = offline_refusal(case, &config);
        assert_eq!(refusal, case.fallback.as_deref(), "{}", case.id);
        if refusal.is_some() {
            continue;
        }
        let summary = sanitize::summarize(&case.message(), max_chars)
            .unwrap()
            .text;
        for marker in &case.markers {
            assert!(
                !summary.contains(marker.as_str()),
                "{}: {marker} survives sanitization: {summary}",
                case.id
            );
        }
        for gap in &case.known_gaps {
            assert!(
                summary.contains(gap.as_str()),
                "{}: the documented gap {gap} is no longer a gap; update the dataset",
                case.id
            );
        }
    }
}

/// A local stand-in for Jev answering from the mock script.
pub(crate) struct MockJev {
    pub url: String,
    /// The `(status, body, retry-after)` the next request receives.
    pub next: Arc<Mutex<(u16, String, Option<String>)>>,
    /// Request bodies received since the last drain.
    pub seen: Arc<Mutex<Vec<String>>>,
}

pub(crate) async fn mock_jev() -> MockJev {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let next = Arc::new(Mutex::new((200, "{}".to_owned(), None::<String>)));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (answer, log) = (Arc::clone(&next), Arc::clone(&seen));
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let body = read_request(&mut stream).await;
            log.lock().unwrap().push(body);
            let (status, body, retry_after) = answer.lock().unwrap().clone();
            let extra = retry_after
                .map(|value| format!("retry-after: {value}\r\n"))
                .unwrap_or_default();
            let response = format!(
                "HTTP/1.1 {status} Test\r\ncontent-type: application/json\r\ncontent-length: {}\r\n{extra}connection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes()).await;
        }
    });
    MockJev { url, next, seen }
}

/// The `state` of each request body.
pub(crate) fn states(bodies: Vec<String>) -> Vec<String> {
    bodies
        .iter()
        .filter_map(|body| serde_json::from_str::<Value>(body).ok())
        .filter_map(|body| body["state"].as_str().map(str::to_owned))
        .collect()
}

/// The scripted answer for `case`.
fn scripted(case: &Case, script: &Value) -> (u16, String, Option<String>) {
    let default = &script["default"];
    let over = &script["overrides"][&case.id];
    let status = over["status"].as_u64().map_or(200, |status| status as u16);
    let retry_after = over["retry_after"].as_str().map(str::to_owned);
    if let Some(body) = over["body"].as_str() {
        return (status, body.to_owned(), retry_after);
    }
    let number = |field: &str| {
        over[field]
            .as_f64()
            .or_else(|| default[field].as_f64())
            .unwrap()
    };
    // A case that must be refused has no label and never reaches Jev; its
    // answer exists only so that a leak would show in the privacy check.
    let category = over["category"]
        .as_str()
        .or_else(|| case.primary())
        .unwrap_or("conversation");
    let confidence = number("category_confidence");
    let mut probabilities = serde_json::Map::new();
    for other in TaskCategory::ALL {
        let value = if other.as_str() == category {
            confidence
        } else {
            (1.0 - confidence) / 5.0
        };
        probabilities.insert(other.as_str().to_owned(), json!(value));
    }
    let noul = |label: Option<bool>, field: &str| {
        over[field].as_f64().unwrap_or_else(|| match label {
            Some(true) => number("yes_probability"),
            Some(false) => number("no_probability"),
            None => number("unlabeled_probability"),
        })
    };
    let body = json!({
        "model": over["model"].as_str().or_else(|| default["model"].as_str()),
        "answers": {
            "task": {"type": "choice", "choice": category, "confidence": confidence, "probabilities": probabilities},
            "planning": {"type": "noul", "noul": noul(case.planning, "planning_probability")},
            "retrieval": {"type": "noul", "noul": noul(case.retrieval, "retrieval_probability")},
            "tools": {"type": "noul", "noul": noul(case.tools, "tools_probability")},
            "complexity": {
                "type": "score",
                "score": number("complexity_score"),
                "legend": {"0": "simple", "1": "moderate", "2": "complex"},
                "probabilities": {"0": 0.1, "1": 0.8, "2": 0.1},
                "confidence": number("complexity_confidence"),
            },
        },
        "usage": default["usage"],
    });
    (status, body.to_string(), retry_after)
}

#[test]
fn the_mock_script_only_names_dataset_cases() {
    let dataset = dataset();
    let script: Value = serde_json::from_str(MOCK_SCRIPT).unwrap();
    let ids: BTreeSet<&str> = dataset.cases.iter().map(|case| case.id.as_str()).collect();
    for (id, over) in script["overrides"].as_object().unwrap() {
        assert!(ids.contains(id.as_str()), "{id} is not a dataset case");
        assert!(
            over["why"].as_str().is_some_and(|why| !why.is_empty()),
            "{id}"
        );
    }
}

/// Run the whole dataset against the mock. Shared with the adversarial and
/// routing suites.
pub(crate) async fn mock_run() -> (Dataset, Vec<CaseResult>, Config) {
    let dataset = dataset();
    let script: Value = serde_json::from_str(MOCK_SCRIPT).unwrap();
    let mock = mock_jev().await;
    let config = config_for(&Endpoint {
        base_url: mock.url.clone(),
        api_key: None,
        model: None,
        timeout_secs: 5,
    });
    let (next, before_seen, after_seen) = (
        Arc::clone(&mock.next),
        Arc::clone(&mock.seen),
        Arc::clone(&mock.seen),
    );
    let (results, truncated) = run_cases(
        &dataset,
        dataset.cases.iter(),
        &config,
        u32::MAX,
        |case| {
            before_seen.lock().unwrap().clear();
            *next.lock().unwrap() = scripted(case, &script);
        },
        |_, _| states(std::mem::take(&mut *after_seen.lock().unwrap())),
    )
    .await;
    assert!(!truncated);
    (dataset, results, config)
}

#[tokio::test]
async fn the_mock_contract_run_scores_the_pipeline_against_independent_labels() {
    let (dataset, results, config) = mock_run().await;
    let (default_threshold, version_thresholds) = thresholds(&config);
    let report = score(
        &dataset,
        &results,
        Measurement::MockContract,
        (default_threshold, &version_thresholds),
        None,
        false,
    );
    write_report("classification-mock", &report, &summary(&report));
    write_threshold_analysis("threshold-analysis-mock", &report);
    write_report(
        "confusion-matrix-mock",
        &report["confusion_matrix"],
        "Confusion matrix of the mock-contract run (scripted, not Jev accuracy). See classification-mock.json.\n",
    );

    // Everything ran, and the run is labeled for what it is.
    assert_eq!(report["measurement"], "mock_contract");
    assert!(
        report["disclaimer"]
            .as_str()
            .unwrap()
            .contains("say nothing about Jev's accuracy")
    );
    assert_eq!(report["dataset"]["cases_run"], 101);
    assert_eq!(report["dataset"]["expected_send"], 93);
    assert_eq!(report["dataset"]["expected_refuse"], 8);

    // Privacy: every refusal sent nothing; no marker reached the stub.
    assert_eq!(report["privacy"]["failures"], 0, "{}", report["privacy"]);
    let requests: usize = results.iter().map(|result| result.sent.len()).sum();
    assert_eq!(
        requests, 93,
        "one request per sendable case, none for refusals"
    );
    assert_eq!(report["cost"]["jev_calls"], 93);
    for result in results.iter().filter(|result| !result.case.expects_send()) {
        assert!(
            !result.invoked && result.sent.is_empty(),
            "{}",
            result.case.id
        );
    }

    // The scripted failures each fall back with their own reason.
    let outcomes = &report["outcomes"];
    assert_eq!(
        report["exploratory"], false,
        "only live runs are exploratory"
    );
    assert_eq!(report["failures_by_class"]["transport"], 2, "429 and 529");
    assert_eq!(
        report["failures_by_class"]["api_contract"], 2,
        "malformed and legacy"
    );
    assert_eq!(report["confidence_distribution"]["samples"], 89);
    // The splits are scored separately and together cover every case.
    let splits = &report["category"]["by_split"];
    assert_eq!(splits["benchmark"]["expected_send"], 68);
    assert_eq!(splits["calibration"]["expected_send"], 25);
    assert_eq!(
        splits["benchmark"]["classified"].as_u64().unwrap()
            + splits["calibration"]["classified"].as_u64().unwrap(),
        89
    );
    assert!(splits["benchmark"]["confusion_matrix"]["macro_f1"].is_number());
    for code in [
        "failed:rate_limited",
        "failed:overloaded",
        "failed:malformed",
        "failed:unsupported_format",
    ] {
        assert_eq!(outcomes[code], 1, "{code}");
    }

    // Hand-derived from the script: 89 classified, 3 wrong (conv-007,
    // ana-006, ms-002); 4 below 0.9 (conv-007, auto-004, amb-001, amb-005).
    let category = &report["category"];
    assert_eq!(category["classified"], 89);
    assert_eq!(category["accuracy"]["numerator"], 86);
    assert_eq!(category["accuracy"]["denominator"], 89);
    assert_eq!(category["accepted_coverage"]["numerator"], 85);
    assert_eq!(category["accepted_coverage"]["denominator"], 93);
    assert_eq!(category["coverage_adjusted_accuracy"]["numerator"], 83);
    assert_eq!(category["coverage_adjusted_accuracy"]["denominator"], 85);
    assert_eq!(category["abstention_rate"]["numerator"], 4);
    assert_eq!(category["fallback_rate"]["numerator"], 8);
    let errors: BTreeSet<&str> = category["high_confidence_misclassifications"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["id"].as_str().unwrap())
        .collect();
    assert_eq!(errors, BTreeSet::from(["ana-006", "ms-002"]));

    // The confusion matrix records the three errors under their primary label.
    let matrix = &report["confusion_matrix"];
    let labels: Vec<&str> = matrix["labels"]
        .as_array()
        .unwrap()
        .iter()
        .map(|label| label.as_str().unwrap())
        .collect();
    let cell = |gold: &str, predicted: &str| {
        let row = labels.iter().position(|label| *label == gold).unwrap();
        let column = labels.iter().position(|label| *label == predicted).unwrap();
        matrix["matrix"][row][column].as_u64().unwrap()
    };
    assert_eq!(cell("conversation", "analysis"), 1);
    assert_eq!(cell("analysis", "conversation"), 1);
    assert_eq!(cell("multi_step", "analysis"), 1);
    assert!(matrix["macro_f1"].as_f64().unwrap() > 0.0);

    // Requirements: one scripted retrieval miss, one scripted tools false alarm.
    let requirements = &report["requirements"];
    assert_eq!(requirements["retrieval"]["fn"], 1);
    assert_eq!(requirements["retrieval"]["fp"], 0);
    assert_eq!(requirements["tools"]["fp"], 1);
    assert_eq!(requirements["tools"]["fn"], 0);
    assert_eq!(requirements["planning"]["fp"], 0);
    assert_eq!(requirements["planning"]["fn"], 0);

    // Workflow: 13 disagreements (9 changed or uncertain predictions, 4
    // failures); no case is made eligible that its label says is not.
    let workflow = &report["workflow"];
    assert_eq!(workflow["agreement"]["numerator"], 80);
    assert_eq!(workflow["agreement"]["denominator"], 93);
    assert_eq!(workflow["eligible_false_positives"], json!([]));
    let missed: BTreeSet<&str> = workflow["eligible_false_negatives"]
        .as_array()
        .unwrap()
        .iter()
        .map(|id| id.as_str().unwrap())
        .collect();
    assert_eq!(
        missed,
        BTreeSet::from([
            "ms-002", "res-005", "code-006", "ret-003", "res-006", "code-004"
        ])
    );
    assert_eq!(
        workflow["applied_in_active_with_guidance"]["numerator"],
        workflow["eligibility"]["eligible"],
        "every eligible decision has fixed guidance; nothing else is applied"
    );

    // Versions and usage are carried through; no cost without verified prices.
    assert_eq!(report["versions"]["jev-mock-1.0.0"]["classified"], 88);
    assert_eq!(report["versions"]["jev-mock-1.1.0"]["classified"], 1);
    assert_eq!(report["cost"]["usage"]["input_tokens"], 89 * 200);
    assert_eq!(report["cost"]["usage"]["output_tokens"], 89 * 40);
    assert_eq!(report["cost"]["usage"]["estimated_cost_usd"], Value::Null);
    assert_eq!(report["cost"]["usage"]["pricing"]["status"], "unverified");

    // Calibration is pending: scripted confidences never propose a threshold
    // and the configured thresholds are untouched.
    let calibration = &report["calibration"];
    assert_eq!(calibration["status"], "pending");
    assert_eq!(calibration["candidate_threshold"], Value::Null);
    assert_eq!(calibration["current_thresholds"]["default_threshold"], 0.9);
    assert_eq!(
        calibration["current_thresholds"]["changed_by_this_run"],
        false
    );
    assert_eq!(
        config.platform.jev.classification.default_threshold, 0.9,
        "the run changed no threshold"
    );
    assert!(summary(&report).contains("Calibration: **pending**"));
}

#[test]
fn a_live_run_proposes_thresholds_only_from_enough_evidence() {
    // Synthetic scored rows stand in for live results to exercise the
    // proposal rule; the measurement label decides whether it may run.
    let dataset = dataset();
    let rows = |split: &str, n: usize, confidence: f64, correct: bool| {
        (0..n)
            .map(|index| {
                let mut case = dataset.cases[0].clone();
                case.id = format!("{split}-{index:03}");
                case.split = split.to_owned();
                if !correct {
                    case.categories = vec!["coding".to_owned()];
                }
                CaseResult {
                    outcome: Outcome::Classified(classify::TaskClassification {
                        category: TaskCategory::Conversation,
                        category_confidence: confidence,
                        planning_probability: 0.1,
                        retrieval_probability: 0.1,
                        tools_probability: 0.1,
                        complexity: classify::Complexity::Simple,
                        complexity_confidence: 0.9,
                        model_version: Some("jev-test".to_owned()),
                        usage: None,
                        threshold: 0.9,
                    }),
                    invoked: true,
                    latency: None,
                    sent: Vec::new(),
                    eligibility: "default_behavior",
                    applied: false,
                    case,
                }
            })
            .collect::<Vec<_>>()
    };
    let thresholds = BTreeMap::new();
    // Too few calibration samples: no proposal, and still nothing applied.
    let few = rows("calibration", 10, 0.95, true);
    let report = score(
        &dataset,
        &few,
        Measurement::LiveJev,
        (0.9, &thresholds),
        None,
        false,
    );
    assert_eq!(report["calibration"]["status"], "measured");
    assert_eq!(report["exploratory"], true);
    assert!(
        report["disclaimer"]
            .as_str()
            .unwrap()
            .starts_with("EXPLORATORY")
    );
    assert_eq!(
        report["calibration"]["candidate_threshold"]["status"],
        "insufficient_evidence"
    );
    assert_eq!(
        report["calibration"]["candidate_threshold"]["applied"],
        false
    );
    // Enough consistent evidence: a proposal for review, checked on the
    // benchmark split.
    let mut many = rows("calibration", 40, 0.95, true);
    many.extend(rows("benchmark", 20, 0.95, true));
    let report = score(
        &dataset,
        &many,
        Measurement::LiveJev,
        (0.9, &thresholds),
        None,
        false,
    );
    let candidate = &report["calibration"]["candidate_threshold"];
    assert_eq!(candidate["status"], "proposed_for_review");
    assert_eq!(
        candidate["threshold"], 0.95,
        "no lower threshold is evidenced by these samples"
    );
    assert_eq!(candidate["applied"], false);
    assert_eq!(
        candidate["benchmark_split_check"]["coverage"]["denominator"],
        20
    );
    // Wrong but confident answers never earn a proposal.
    let wrong = rows("calibration", 40, 0.95, false);
    let report = score(
        &dataset,
        &wrong,
        Measurement::LiveJev,
        (0.9, &thresholds),
        None,
        false,
    );
    assert_eq!(
        report["calibration"]["candidate_threshold"]["reason"],
        "target_not_met"
    );
    assert_eq!(
        report["category"]["high_confidence_misclassifications"]
            .as_array()
            .unwrap()
            .len(),
        40
    );
}

#[test]
fn failures_are_classed_as_transport_credential_or_contract() {
    for code in [
        "connect",
        "transport",
        "timeout",
        "rate_limited",
        "overloaded",
        "status",
    ] {
        assert_eq!(failure_class(code), "transport", "{code}");
    }
    assert_eq!(failure_class("unauthorized"), "credential");
    for code in [
        "rejected",
        "malformed",
        "unsupported_format",
        "response_too_large",
    ] {
        assert_eq!(failure_class(code), "api_contract", "{code}");
    }
}

#[test]
fn pricing_is_used_only_when_fully_verified() {
    let pricing = Pricing {
        input_per_million: 2.0,
        output_per_million: 8.0,
        source: "operator-verified price sheet".to_owned(),
    };
    // 1,000,000 in and 500,000 out: 2.0 + 4.0.
    assert!((pricing.estimate(1_000_000, 500_000) - 6.0).abs() < 1e-9);
    let usage = usage_json(10, 5, 1, Some(&pricing));
    assert_eq!(usage["pricing"]["status"], "verified_by_operator");
    let unpriced = usage_json(10, 5, 1, None);
    assert_eq!(unpriced["estimated_cost_usd"], Value::Null);
}
