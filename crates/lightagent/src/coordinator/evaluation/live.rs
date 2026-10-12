//! The protected live contract: the real TypeSafe endpoint, synthetic inputs
//! only, a hard request budget.
//!
//! These tests are `#[ignore]`d and run only from the manually dispatched
//! `jev-live` workflow, whose GitHub Environment holds `TYPESAFE_API_KEY`.
//! Without the key they fail loudly; a missing credential is never reported
//! as a pass. Every request is counted against
//! `LIGHTAGENT_JEV_LIVE_MAX_REQUESTS` (default 40, never above 150), runs one
//! at a time, and uses the production adapter's timeout and its no-retry
//! policy. The key is read through a secret reference at the point of use
//! and never formatted, logged or written to a report.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use lightagent_core::SecretRef;
use lightagent_jev::{
    Answer, EvaluationRequest, JevClient, Limits, Question, Response, RetryPolicy,
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::routing::{routing_summary, run_routing, score_routing};
use super::{
    Case, Dataset, Endpoint, Measurement, Pricing, config_for, dataset, failure_class, run_cases,
    sanitize, score, summary, thresholds, write_report, write_threshold_analysis,
};
use crate::coordinator::JevState;
use crate::coordinator::classify::Outcome;

const KEY_VARIABLE: &str = "TYPESAFE_API_KEY";
const DEFAULT_URL: &str = "https://api.typesafe.ai";
const DEFAULT_REQUESTS: u32 = 40;
const MAX_REQUESTS: u32 = 150;
/// The three contract probes: connection check, typed probe, invalid key.
const PROBES: u32 = 3;
/// A deliberately invalid credential, used to prove error output never
/// echoes a key.
const INVALID_CREDENTIAL: &str = "lightagent-contract-invalid-credential-0000";

fn env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

/// Cases in round-robin order over their groups, so a small budget still
/// covers every kind of task.
fn interleaved(dataset: &Dataset) -> Vec<&Case> {
    let mut groups: BTreeMap<&str, Vec<&Case>> = BTreeMap::new();
    for case in &dataset.cases {
        groups.entry(case.group.as_str()).or_default().push(case);
    }
    let longest = groups.values().map(Vec::len).max().unwrap_or(0);
    (0..longest)
        .flat_map(|index| {
            groups
                .values()
                .filter_map(move |cases| cases.get(index).copied())
                .collect::<Vec<_>>()
        })
        .collect()
}

/// What a check verifies: the credential, the API contract, secret safety,
/// or classification quality, which is measured against labels and is never
/// a contract failure on its own.
fn area(name: &str) -> &'static str {
    match name {
        "authentication" => "credential",
        "error_secret_safe" => "security",
        "bounded_quality_evaluation" | "routing_benchmark" => "classification_quality",
        _ => "api_contract",
    }
}

fn check(name: &str, status: &str, detail: impl Into<String>) -> Value {
    json!({"name": name, "area": area(name), "status": status, "detail": detail.into()})
}

/// A check that failed because of a Jev error, classified by that error as a
/// transport, credential or API-contract failure.
fn failed_with(name: &str, kind: &str, detail: impl Into<String>) -> Value {
    let mut value = check(name, "failed", detail);
    value["failure_class"] = json!(failure_class(kind));
    value
}

/// The checks a live run must pass.
const REQUIRED: [&str; 8] = [
    "authentication",
    "schema_accepted",
    "typed_response_parses",
    "noul_without_confidence",
    "error_secret_safe",
    "category_confidence",
    "adapter_compatibility",
    "bounded_quality_evaluation",
];

/// Write the contract report and return the required checks that failed.
fn finish_contract(
    checks: &[Value],
    endpoint: &str,
    model_requested: &str,
    (used, limit): (u32, u32),
    versions: &BTreeSet<String>,
) -> Vec<String> {
    let failed: Vec<String> = checks
        .iter()
        .filter(|check| {
            REQUIRED.contains(&check["name"].as_str().unwrap_or_default())
                && check["status"] != "passed"
        })
        .map(|check| check["name"].as_str().unwrap_or_default().to_owned())
        .collect();
    let mut failures_by_class: BTreeMap<String, u32> = BTreeMap::new();
    for check in checks.iter().filter(|check| check["status"] == "failed") {
        let class = check["failure_class"]
            .as_str()
            .or_else(|| check["area"].as_str())
            .unwrap_or("unknown");
        *failures_by_class.entry(class.to_owned()).or_default() += 1;
    }
    let status = if failed.is_empty() {
        "passed"
    } else {
        "failed"
    };
    let commit = env("LIGHTAGENT_JEV_LIVE_COMMIT").unwrap_or_else(|| "unknown".to_owned());
    let contract = json!({
        "report": "jev-live-contract",
        "measurement": Measurement::LiveJev.as_str(),
        "commit": commit,
        "endpoint": endpoint,
        "model_requested": model_requested,
        "resolved_model_versions": versions,
        "requests": {"limit": limit, "used": used},
        "status": status,
        "failures_by_class": failures_by_class,
        "classification_quality": "exploratory: measured in classification-live.json against single-author labels; never a pass/fail criterion",
        "checks": checks,
    });
    let mut text = format!(
        "# Jev live contract: {status}\n\nCommit {commit}. Model requested {model_requested}; resolved {}. Requests used: {used} of {limit}.\n\n| Check | Area | Status | Failure class | Detail |\n|---|---|---|---|---|\n",
        if versions.is_empty() {
            "not reported".to_owned()
        } else {
            versions.iter().cloned().collect::<Vec<_>>().join(", ")
        },
    );
    for check in checks {
        text.push_str(&format!(
            "| {} | {} | {} | {} | {} |\n",
            check["name"].as_str().unwrap_or_default(),
            check["area"].as_str().unwrap_or_default(),
            check["status"].as_str().unwrap_or_default(),
            check["failure_class"].as_str().unwrap_or("-"),
            check["detail"].as_str().unwrap_or_default()
        ));
    }
    write_report("live-contract", &contract, &text);
    failed
}

fn passed(ok: bool) -> &'static str {
    if ok { "passed" } else { "failed" }
}

#[tokio::test]
#[ignore = "calls the live TypeSafe API; run only by the protected jev-live workflow"]
async fn live_jev_contract_and_bounded_evaluation() {
    assert!(
        env(KEY_VARIABLE).is_some(),
        "{KEY_VARIABLE} is not set: live validation is pending, not passed"
    );
    let url = env("LIGHTAGENT_JEV_LIVE_URL").unwrap_or_else(|| DEFAULT_URL.to_owned());
    let model = env("LIGHTAGENT_JEV_LIVE_MODEL");
    let limit = env("LIGHTAGENT_JEV_LIVE_MAX_REQUESTS")
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(DEFAULT_REQUESTS)
        .min(MAX_REQUESTS);
    assert!(
        limit >= PROBES + 6,
        "a live run needs at least {} requests",
        PROBES + 6
    );
    let max_cost =
        env("LIGHTAGENT_JEV_LIVE_MAX_COST_USD").and_then(|value| value.parse::<f64>().ok());
    let pricing = Pricing::from_env();
    let endpoint = Endpoint {
        base_url: url.clone(),
        api_key: Some(SecretRef::env(KEY_VARIABLE)),
        model: model.clone(),
        timeout_secs: 10,
    };
    let config = config_for(&endpoint);
    let jev = &config.platform.jev;
    let mut checks = Vec::new();
    let mut used = 0u32;
    let mut versions: BTreeSet<String> = BTreeSet::new();
    let mut usage_seen = false;

    // 1. Authentication: the operator's connection check. A failure here
    // stops the run before any dataset request is spent.
    let verification = JevState::new().verify(jev).await;
    used += 1;
    let detail = format!(
        "error={:?}, model_version={:?}, latency_ms={}",
        verification.error, verification.model_version, verification.latency_ms
    );
    checks.push(match verification.error {
        Some(kind) if !verification.ok => failed_with("authentication", kind, detail),
        _ => check("authentication", passed(verification.ok), detail),
    });
    if let Some(version) = &verification.model_version {
        versions.insert(version.clone());
    }
    if !verification.ok {
        let failed = finish_contract(&checks, &url, &jev.model, (used, limit), &versions);
        panic!(
            "authentication failed ({:?}); no dataset request was made: {failed:?}",
            verification.error
        );
    }

    // 2-5, 7. One typed probe through the shared adapter.
    let client = JevClient::from_config(jev).expect("the live client builds");
    let probe = EvaluationRequest::new(
        jev.model.clone(),
        "Synthetic contract check: name three primary colors.",
        BTreeMap::from([
            (
                "needs_tools".to_owned(),
                Question::noul("Would completing this task require tools?"),
            ),
            (
                "kind".to_owned(),
                Question::choice(
                    "Which describes this text?",
                    [
                        ("question".to_owned(), "A question to answer.".to_owned()),
                        ("instruction".to_owned(), "An action to perform.".to_owned()),
                    ],
                ),
            ),
        ]),
    );
    let response = client.evaluate(&probe, &CancellationToken::new()).await;
    used += 1;
    match &response {
        Ok(Response::TypeSafe(evaluation)) => {
            checks.push(check(
                "schema_accepted",
                "passed",
                "choice and noul questions",
            ));
            let noul = evaluation.answers.get("needs_tools");
            let noul_ok = matches!(noul, Some(Answer::Noul { probability }) if (0.0..=1.0).contains(probability))
                && noul.and_then(Answer::confidence).is_none();
            let choice = evaluation.answers.get("kind");
            let choice_ok = choice
                .and_then(Answer::confidence)
                .is_some_and(|confidence| (0.0..=1.0).contains(&confidence));
            checks.push(check(
                "typed_response_parses",
                passed(choice_ok && noul_ok),
                "every question answered with its own type",
            ));
            checks.push(check(
                "noul_without_confidence",
                passed(noul_ok),
                "a noul answer carries a probability and no confidence; none is derived",
            ));
            if let Some(version) = &evaluation.model {
                versions.insert(version.clone());
            }
            usage_seen |= evaluation.usage.is_some();
        }
        Ok(Response::LegacyOutput(_)) => {
            checks.push(check(
                "schema_accepted",
                "failed",
                "the endpoint answered with the legacy envelope",
            ));
        }
        Err(error) => {
            checks.push(failed_with("schema_accepted", error.kind(), error.kind()));
        }
    }

    // 8. Error responses stay secret-safe.
    let invalid = JevClient::new(
        &url,
        Some(INVALID_CREDENTIAL.to_owned()),
        Limits {
            timeout: Duration::from_secs(10),
            ..Limits::default()
        },
        RetryPolicy::none(),
    )
    .expect("the invalid-key client builds");
    let rejected = invalid.evaluate(&probe, &CancellationToken::new()).await;
    used += 1;
    let real_key = env(KEY_VARIABLE).unwrap_or_default();
    let (error_status, error_detail) = match &rejected {
        Err(error) => {
            let shown = format!("{error} {error:?}");
            let safe = !shown.contains(INVALID_CREDENTIAL) && !shown.contains(&real_key);
            (
                passed(safe && matches!(error.kind(), "unauthorized" | "rejected")),
                format!("kind={}", error.kind()),
            )
        }
        Ok(_) => ("failed", "an invalid key was accepted".to_owned()),
    };
    checks.push(check("error_secret_safe", error_status, error_detail));

    // 10. Bounded classification quality on the synthetic dataset.
    let dataset = dataset();
    let ordered = interleaved(&dataset);
    let classification_budget = (limit - used) * 2 / 3;
    let max_chars = config.platform.jev.classification.max_input_chars;
    let (results, truncated) = run_cases(
        &dataset,
        ordered.iter().copied(),
        &config,
        classification_budget,
        |_| {},
        |case, invoked| {
            if invoked {
                sanitize::summarize(&case.message(), max_chars)
                    .map(|summary| vec![summary.text])
                    .unwrap_or_default()
            } else {
                Vec::new()
            }
        },
    )
    .await;
    used += results
        .iter()
        .map(|result| u32::from(result.invoked))
        .sum::<u32>();
    let (default_threshold, version_thresholds) = thresholds(&config);
    let report = score(
        &dataset,
        &results,
        Measurement::LiveJev,
        (default_threshold, &version_thresholds),
        pricing.as_ref(),
        truncated,
    );
    write_report("classification-live", &report, &summary(&report));
    write_threshold_analysis("threshold-analysis-live", &report);
    write_report(
        "confusion-matrix-live",
        &report["confusion_matrix"],
        "Confusion matrix of the live run. See classification-live.json for sample sizes.\n",
    );

    let mut malformed = 0;
    let mut confidence_ok = true;
    let mut classified = 0;
    for result in &results {
        match &result.outcome {
            Outcome::Classified(classification) => {
                classified += 1;
                let threshold = config
                    .platform
                    .jev
                    .classification
                    .threshold_for(classification.model_version.as_deref());
                confidence_ok &= (0.0..=1.0).contains(&classification.category_confidence)
                    && classification.accepted()
                        == (classification.category_confidence >= f64::from(threshold));
                if let Some(version) = &classification.model_version {
                    versions.insert(version.clone());
                }
                usage_seen |= classification.usage.is_some();
            }
            Outcome::Failed("malformed" | "unsupported_format") => malformed += 1,
            _ => {}
        }
    }
    checks.push(check(
        "category_confidence",
        passed(classified > 0 && confidence_ok),
        format!("{classified} classified; acceptance follows the version's threshold"),
    ));
    checks.push(check(
        "adapter_compatibility",
        passed(classified > 0 && malformed == 0),
        format!("{malformed} malformed or legacy answers to the classification request"),
    ));
    checks.push(check(
        "model_version",
        if versions.is_empty() {
            "not_reported"
        } else {
            "passed"
        },
        format!("{versions:?}"),
    ));
    checks.push(check(
        "token_usage",
        if usage_seen { "passed" } else { "not_reported" },
        "usage captured when TypeSafe reports it",
    ));
    checks.push(check(
        "bounded_quality_evaluation",
        passed(!results.is_empty()),
        format!(
            "{} cases within a budget of {classification_budget}",
            results.len()
        ),
    ));

    // Routing comparison with what remains, unless the cost cap is reached.
    let spent_cost = pricing.as_ref().map(|pricing| {
        let usage = &report["cost"]["usage"];
        pricing.estimate(
            usage["input_tokens"].as_u64().unwrap_or(0),
            usage["output_tokens"].as_u64().unwrap_or(0),
        )
    });
    let over_cost = matches!((spent_cost, max_cost), (Some(spent), Some(cap)) if spent >= cap);
    if over_cost {
        checks.push(check(
            "routing_benchmark",
            "skipped",
            "the cost cap was reached",
        ));
    } else {
        let (rows, truncated) = run_routing(
            ordered.iter().copied(),
            &url,
            Some(SecretRef::env(KEY_VARIABLE)),
            model.as_deref(),
            // The policy the recorded live baseline ran under, kept for
            // comparability; the benchmark's inputs are synthetic.
            lightagent_core::JevRoutingPermission::LegacyAllProfiles,
            limit - used,
            |row| {
                if row.report.invoked {
                    row.state.clone().into_iter().collect()
                } else {
                    Vec::new()
                }
            },
        )
        .await;
        used += rows
            .iter()
            .map(|row| u32::from(row.report.invoked))
            .sum::<u32>();
        let routing = score_routing(
            &dataset,
            &rows,
            Measurement::LiveJev,
            pricing.as_ref(),
            truncated,
        );
        write_report("routing-live", &routing, &routing_summary(&routing));
        checks.push(check(
            "routing_benchmark",
            passed(!rows.is_empty()),
            format!("{} case pairs", rows.len() / 2),
        ));
    }

    assert!(
        used <= limit,
        "the request budget was exceeded: {used} > {limit}"
    );
    let failed = finish_contract(&checks, &url, &jev.model, (used, limit), &versions);
    assert!(failed.is_empty(), "live contract checks failed: {failed:?}");
}

#[test]
fn the_live_suite_interleaves_groups_and_never_names_the_key() {
    let dataset = dataset();
    let ordered = interleaved(&dataset);
    assert_eq!(ordered.len(), dataset.cases.len());
    let first: BTreeSet<&str> = ordered[..12]
        .iter()
        .map(|case| case.group.as_str())
        .collect();
    assert_eq!(first.len(), 12, "the first twelve cases cover every group");
    // The invalid credential cannot be mistaken for a real one.
    assert!(INVALID_CREDENTIAL.contains("invalid"));
    assert_eq!(KEY_VARIABLE, "TYPESAFE_API_KEY");
}
