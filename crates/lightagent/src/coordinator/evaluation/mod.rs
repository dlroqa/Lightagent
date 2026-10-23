//! Offline evaluation of Jev task classification against independent labels.
//!
//! The labeled, synthetic dataset lives in `evaluation/jev/` and is described
//! there. This module loads it, runs every case through the production
//! classification path ([`classify::evaluate`](super::classify::evaluate)),
//! the sanitizer and the workflow policy, and scores the results with
//! [`metrics`]. Nothing here is compiled into the shipped binary.
//!
//! Two kinds of run exist and are never mixed:
//!
//! * `mock_contract` — Jev is a local stub answering from
//!   `mock-responses.v1.json`. It proves the pipeline, the privacy gates and
//!   the arithmetic, and is deterministic. Its scores describe the script,
//!   not Jev, and every report says so.
//! * `live_jev` — the real TypeSafe endpoint, through the protected `jev-live`
//!   workflow only ([`live`]). Only these numbers measure classification
//!   quality, and only they may inform a threshold proposal.
//!
//! When `LIGHTAGENT_JEV_EVAL_OUT` names a directory, each run writes its
//! machine-readable report and a Markdown summary there.

mod live;
pub(crate) mod metrics;
mod routing;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use lightagent_core::permissions::ApprovalPolicy;
use lightagent_core::{Config, JevClassificationMode, PolicyEngine, RiskClass, RunLimits};
use lightagent_tools::ToolRegistry;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::classify::{self, ClassificationReport, Outcome, TaskCategory};
use super::sanitize;
use super::workflow::{self, WorkflowInputs};
use metrics::{Binary, Candidate, Confusion, Ratio, round};

pub(crate) const DATASET: &str = include_str!("../../../../../evaluation/jev/benchmark.v1.json");
pub(crate) const MOCK_SCRIPT: &str =
    include_str!("../../../../../evaluation/jev/mock-responses.v1.json");

/// The profile every dataset case runs as unless it names another. Only this
/// profile is permitted to send sanitized task descriptions.
pub(crate) const PERMITTED_PROFILE: &str = "permitted";

/// Confidence thresholds the accuracy-coverage curve is drawn at.
pub(crate) const THRESHOLD_GRID: [f64; 11] = [
    0.5, 0.6, 0.7, 0.75, 0.8, 0.85, 0.9, 0.925, 0.95, 0.975, 0.99,
];
/// A proposal needs this many accepted calibration samples.
pub(crate) const MIN_ACCEPTED_FOR_PROPOSAL: u32 = 30;
/// A proposal's selective accuracy must have a Wilson lower bound this high.
pub(crate) const TARGET_SELECTIVE_ACCURACY: f64 = 0.9;
/// Fewer classified samples than this make ECE descriptive only.
pub(crate) const MIN_SAMPLES_FOR_ECE: usize = 50;
/// The probability at which a yes/no requirement counts as "yes".
pub(crate) const REQUIREMENT_CUT_OFF: f64 = 0.5;

/// The versioned benchmark dataset.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Dataset {
    pub schema: u32,
    pub dataset: String,
    pub version: String,
    pub created: String,
    pub rubric: String,
    pub provenance: Value,
    pub capabilities: BTreeMap<String, Capability>,
    pub cases: Vec<Case>,
}

/// A capability fixture: the tools, approval policy and limits a case's run
/// is assumed to have when its workflow eligibility is decided.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Capability {
    pub description: String,
    pub tools: Vec<String>,
    pub deny: Vec<RiskClass>,
    pub max_turns: Option<u32>,
}

/// One labeled case.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Case {
    pub id: String,
    #[serde(default = "benchmark_split")]
    pub split: String,
    pub group: String,
    pub text: String,
    /// Repeat `text` this many times (to build an over-long input).
    #[serde(default)]
    pub repeat: Option<usize>,
    #[serde(default)]
    pub attachment: bool,
    #[serde(default = "permitted_profile")]
    pub profile: String,
    #[serde(default = "full_capability")]
    pub capability: String,
    /// The acceptable categories, primary first. Empty when no
    /// classification may happen.
    pub categories: Vec<String>,
    /// `None`: either answer is acceptable.
    pub planning: Option<bool>,
    pub retrieval: Option<bool>,
    pub tools: Option<bool>,
    /// Expected eligibility for the primary category, accepted, under
    /// `capability`.
    pub workflow: Option<String>,
    /// `send` or `refuse`.
    #[serde(default = "send_privacy")]
    pub privacy: String,
    /// The expected skip code when nothing may be sent.
    #[serde(default)]
    pub fallback: Option<String>,
    /// Strings that must never leave the machine in a sanitized input.
    #[serde(default)]
    pub markers: Vec<String>,
    /// Sensitive strings pattern redaction is known not to catch.
    #[serde(default)]
    pub known_gaps: Vec<String>,
    #[serde(default)]
    pub notes: Option<String>,
}

fn benchmark_split() -> String {
    "benchmark".to_owned()
}
fn permitted_profile() -> String {
    PERMITTED_PROFILE.to_owned()
}
fn full_capability() -> String {
    "full".to_owned()
}
fn send_privacy() -> String {
    "send".to_owned()
}

impl Case {
    /// The message as the user would send it.
    pub(crate) fn message(&self) -> String {
        self.text.repeat(self.repeat.unwrap_or(1))
    }

    pub(crate) fn expects_send(&self) -> bool {
        self.privacy == "send"
    }

    pub(crate) fn primary(&self) -> Option<&str> {
        self.categories.first().map(String::as_str)
    }
}

pub(crate) fn dataset() -> Dataset {
    serde_json::from_str(DATASET).expect("the benchmark dataset parses")
}

/// The run-time pieces a capability fixture stands for.
pub(crate) struct Fixture {
    pub registry: ToolRegistry,
    pub policy: PolicyEngine,
    pub limits: RunLimits,
}

impl Capability {
    pub(crate) fn fixture(&self) -> Fixture {
        let mut policy = ApprovalPolicy::balanced();
        policy.deny.extend(self.deny.iter().copied());
        let mut limits = RunLimits::default();
        if let Some(max_turns) = self.max_turns {
            limits.max_turns = max_turns;
        }
        Fixture {
            registry: ToolRegistry::builtin().scoped(&self.tools),
            policy: PolicyEngine::new(policy),
            limits,
        }
    }
}

/// The workflow decision for `outcome` under `capability`, in `active` mode
/// with workflow guidance on, so `applied` shows what active mode would do.
pub(crate) fn decide_workflow(
    outcome: &Outcome,
    capability: &Capability,
) -> workflow::WorkflowDecision {
    let fixture = capability.fixture();
    let mut config = Config::default();
    config.platform.jev.classification.workflow_guidance = true;
    let report = ClassificationReport {
        mode: JevClassificationMode::Active,
        outcome: outcome.clone(),
        invoked: true,
        latency: None,
    };
    workflow::evaluate(&WorkflowInputs {
        report: &report,
        registry: &fixture.registry,
        policy: &fixture.policy,
        limits: &fixture.limits,
        config: &config,
        cancelled: false,
    })
}

/// An accepted classification of `category`, for checking labels against the
/// policy. Its numbers are placeholders and are never scored.
pub(crate) fn accepted(category: TaskCategory) -> Outcome {
    Outcome::Classified(classify::TaskClassification {
        category,
        category_confidence: 1.0,
        planning_probability: 0.5,
        retrieval_probability: 0.5,
        tools_probability: 0.5,
        complexity: classify::Complexity::Moderate,
        complexity_confidence: 1.0,
        model_version: None,
        usage: None,
        threshold: 0.9,
        selection: None,
    })
}

/// A Jev endpoint to evaluate against.
pub(crate) struct Endpoint {
    pub base_url: String,
    pub api_key: Option<lightagent_core::SecretRef>,
    pub model: Option<String>,
    pub timeout_secs: u64,
}

/// The configuration every case runs under: classification in `shadow` mode,
/// only [`PERMITTED_PROFILE`] permitted, default sanitizer bounds.
pub(crate) fn config_for(endpoint: &Endpoint) -> Config {
    let mut config = Config::default();
    config.platform.jev.endpoint.enabled = true;
    config.platform.jev.endpoint.base_url = Some(endpoint.base_url.clone());
    config.platform.jev.endpoint.api_key = endpoint.api_key.clone();
    if let Some(model) = &endpoint.model {
        config.platform.jev.model = model.clone();
    }
    config.platform.jev.timeout_secs = endpoint.timeout_secs;
    config.platform.jev.classification.mode = JevClassificationMode::Shadow;
    config.platform.jev.classification.allowed_profiles = vec![PERMITTED_PROFILE.to_owned()];
    config
}

/// What one case produced.
#[derive(Clone, Debug)]
pub(crate) struct CaseResult {
    pub case: Case,
    pub outcome: Outcome,
    /// Whether a Jev request was made, as the classifier reports it.
    pub invoked: bool,
    pub latency: Option<Duration>,
    /// The `state` of each request that reached Jev for this case. Recorded by
    /// the mock server; for a live run, the sanitizer's output, which is what
    /// the classifier sends.
    pub sent: Vec<String>,
    pub eligibility: &'static str,
    pub applied: bool,
}

/// Run `cases` sequentially. `before` is told each case before it runs (the
/// mock uses it to choose its answer) and `sent` reports what reached Jev,
/// given whether the classifier says it sent anything; the mock answers from
/// its own request log regardless.
/// Stops before `max_requests` would be exceeded and reports how many cases
/// ran.
pub(crate) async fn run_cases<'a, B, S>(
    dataset: &Dataset,
    cases: impl IntoIterator<Item = &'a Case>,
    config: &Config,
    max_requests: u32,
    mut before: B,
    mut sent: S,
) -> (Vec<CaseResult>, bool)
where
    B: FnMut(&Case),
    S: FnMut(&Case, bool) -> Vec<String>,
{
    let mut results = Vec::new();
    let mut requests = 0u32;
    let mut truncated = false;
    for case in cases {
        if case.expects_send() && requests >= max_requests {
            truncated = true;
            break;
        }
        before(case);
        let (outcome, invoked, latency) = classify::evaluate(
            config,
            &case.profile,
            &case.message(),
            case.attachment,
            &CancellationToken::new(),
        )
        .await;
        requests += u32::from(invoked);
        let decision = decide_workflow(&outcome, &dataset.capabilities[&case.capability]);
        results.push(CaseResult {
            case: case.clone(),
            sent: sent(case, invoked),
            outcome,
            invoked,
            latency,
            eligibility: decision.eligibility.as_str(),
            applied: decision.applied,
        });
    }
    (results, truncated)
}

/// Whether the run measured Jev or a script.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Measurement {
    MockContract,
    LiveJev,
}

impl Measurement {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::MockContract => "mock_contract",
            Self::LiveJev => "live_jev",
        }
    }

    fn disclaimer(self) -> &'static str {
        match self {
            Self::MockContract => {
                "Deterministic contract run against scripted answers. These numbers test the evaluation pipeline and say nothing about Jev's accuracy."
            }
            Self::LiveJev => {
                "EXPLORATORY. Live TypeSafe Jev answers on a small synthetic dataset whose labels have a single author and have not been independently reviewed. Treat every rate with its sample size and interval; do not change thresholds or defaults from it."
            }
        }
    }
}

/// Verified per-token pricing, read from the environment. Without all three
/// values no cost is estimated.
pub(crate) struct Pricing {
    pub input_per_million: f64,
    pub output_per_million: f64,
    pub source: String,
}

impl Pricing {
    pub(crate) fn from_env() -> Option<Self> {
        let number = |name| {
            std::env::var(name)
                .ok()
                .and_then(|value| value.trim().parse::<f64>().ok())
                .filter(|value| value.is_finite() && *value >= 0.0)
        };
        let source = std::env::var("JEV_PRICING_SOURCE")
            .ok()
            .filter(|source| !source.trim().is_empty())?;
        Some(Self {
            input_per_million: number("JEV_PRICE_INPUT_PER_MTOK")?,
            output_per_million: number("JEV_PRICE_OUTPUT_PER_MTOK")?,
            source,
        })
    }

    pub(crate) fn estimate(&self, input: u64, output: u64) -> f64 {
        (input as f64 * self.input_per_million + output as f64 * self.output_per_million)
            / 1_000_000.0
    }
}

pub(crate) fn usage_json(
    input: u64,
    output: u64,
    with_usage: u32,
    pricing: Option<&Pricing>,
) -> Value {
    let basis = match pricing {
        Some(pricing) => json!({
            "status": "verified_by_operator",
            "source": pricing.source,
            "input_per_million_usd": pricing.input_per_million,
            "output_per_million_usd": pricing.output_per_million,
        }),
        None => json!({
            "status": "unverified",
            "note": "No verified TypeSafe pricing was supplied; no cost is estimated.",
        }),
    };
    json!({
        "responses_with_usage": with_usage,
        "input_tokens": input,
        "output_tokens": output,
        "estimated_cost_usd": pricing.map(|pricing| round(pricing.estimate(input, output))),
        "pricing": basis,
    })
}

fn outcome_code(outcome: &Outcome) -> String {
    match outcome {
        Outcome::Classified(classification) if classification.accepted() => "classified".to_owned(),
        Outcome::Classified(_) => "low_confidence".to_owned(),
        Outcome::Skipped(code) => format!("skipped:{code}"),
        Outcome::Failed(code) => format!("failed:{code}"),
    }
}

/// The privacy verdict for one case.
pub(crate) fn privacy_row(result: &CaseResult) -> Value {
    let case = &result.case;
    let leaked: Vec<&String> = case
        .markers
        .iter()
        .filter(|marker| {
            result
                .sent
                .iter()
                .any(|state| state.contains(marker.as_str()))
        })
        .collect();
    let gaps: Vec<&String> = case
        .known_gaps
        .iter()
        .filter(|gap| result.sent.iter().any(|state| state.contains(gap.as_str())))
        .collect();
    let expected_skip = case.fallback.as_deref();
    let pass = if case.expects_send() {
        leaked.is_empty()
    } else {
        !result.invoked
            && result.sent.is_empty()
            && matches!(&result.outcome, Outcome::Skipped(code) if Some(*code) == expected_skip)
    };
    json!({
        "id": case.id,
        "expected": case.privacy,
        "outcome": outcome_code(&result.outcome),
        "request_sent": result.invoked,
        "markers_checked": case.markers.len(),
        "markers_transmitted": leaked,
        "known_gaps_transmitted": gaps,
        "pass": pass,
    })
}

/// Score `results` against their labels.
pub(crate) fn score(
    dataset: &Dataset,
    results: &[CaseResult],
    measurement: Measurement,
    current_thresholds: (f32, &BTreeMap<String, f32>),
    pricing: Option<&Pricing>,
    truncated: bool,
) -> Value {
    let categories: Vec<&str> = TaskCategory::ALL
        .iter()
        .map(|category| category.as_str())
        .collect();
    let send: Vec<&CaseResult> = results
        .iter()
        .filter(|result| result.case.expects_send())
        .collect();
    let send_count = count(send.len());

    let mut outcomes: BTreeMap<String, u32> = BTreeMap::new();
    let mut confusion = Confusion::with_labels(categories.iter().copied());
    let mut confusion_by_split: BTreeMap<String, Confusion> = BTreeMap::new();
    let mut points: Vec<(f64, bool)> = Vec::new();
    let mut points_by_split: BTreeMap<String, Vec<(f64, bool)>> = BTreeMap::new();
    let mut send_by_split: BTreeMap<String, u32> = BTreeMap::new();
    let mut classified = 0u32;
    let mut correct = 0u32;
    let mut accepted = 0u32;
    let mut accepted_correct = 0u32;
    let mut accepted_by_split: BTreeMap<String, (u32, u32)> = BTreeMap::new();
    let mut high_confidence_errors = Vec::new();
    let mut planning = Binary::default();
    let mut retrieval = Binary::default();
    let mut tools = Binary::default();
    let mut planning_points = Vec::new();
    let mut retrieval_points = Vec::new();
    let mut tools_points = Vec::new();
    let mut complexity: BTreeMap<&str, u32> = BTreeMap::new();
    let mut complexity_confidence = Vec::new();
    let mut versions: BTreeMap<String, (u32, u32, u32)> = BTreeMap::new();
    let mut latencies = Vec::new();
    let (mut input_tokens, mut output_tokens, mut with_usage) = (0u64, 0u64, 0u32);
    let mut workflow_agree = 0u32;
    let mut workflow_labeled = 0u32;
    let mut eligible_false_positive = Vec::new();
    let mut eligible_false_negative = Vec::new();
    let mut eligibility_counts: BTreeMap<&str, u32> = BTreeMap::new();
    let mut applied = 0u32;

    for result in &send {
        let case = &result.case;
        *outcomes.entry(outcome_code(&result.outcome)).or_default() += 1;
        *send_by_split.entry(case.split.clone()).or_default() += 1;
        if let Some(latency) = result.latency {
            latencies.push(metrics_millis(latency));
        }
        if let Some(expected) = case.workflow.as_deref() {
            workflow_labeled += 1;
            if expected == result.eligibility {
                workflow_agree += 1;
            }
            if result.eligibility == "eligible" && expected != "eligible" {
                eligible_false_positive.push(case.id.clone());
            }
            if expected == "eligible" && result.eligibility != "eligible" {
                eligible_false_negative.push(case.id.clone());
            }
        }
        *eligibility_counts.entry(result.eligibility).or_default() += 1;
        applied += u32::from(result.applied);
        let Outcome::Classified(prediction) = &result.outcome else {
            continue;
        };
        classified += 1;
        let predicted = prediction.category.as_str();
        let is_correct = case.categories.iter().any(|category| category == predicted);
        let gold = if is_correct {
            predicted
        } else {
            case.primary().unwrap_or("none")
        };
        confusion.add(gold, predicted);
        confusion_by_split
            .entry(case.split.clone())
            .or_insert_with(|| Confusion::with_labels(categories.iter().copied()))
            .add(gold, predicted);
        correct += u32::from(is_correct);
        points.push((prediction.category_confidence, is_correct));
        points_by_split
            .entry(case.split.clone())
            .or_default()
            .push((prediction.category_confidence, is_correct));
        if prediction.accepted() {
            accepted += 1;
            accepted_correct += u32::from(is_correct);
            let split = accepted_by_split.entry(case.split.clone()).or_default();
            split.0 += 1;
            split.1 += u32::from(is_correct);
            if !is_correct {
                high_confidence_errors.push(json!({
                    "id": case.id,
                    "labels": case.categories,
                    "predicted": predicted,
                    "confidence": round(prediction.category_confidence),
                    "threshold": round(f64::from(prediction.threshold)),
                    "model_version": prediction.model_version,
                }));
            }
        }
        for (binary, samples, label, probability) in [
            (
                &mut planning,
                &mut planning_points,
                case.planning,
                prediction.planning_probability,
            ),
            (
                &mut retrieval,
                &mut retrieval_points,
                case.retrieval,
                prediction.retrieval_probability,
            ),
            (
                &mut tools,
                &mut tools_points,
                case.tools,
                prediction.tools_probability,
            ),
        ] {
            if let Some(label) = label {
                binary.add(probability, label, REQUIREMENT_CUT_OFF);
                samples.push((probability, label));
            }
        }
        *complexity
            .entry(prediction.complexity.as_str())
            .or_default() += 1;
        complexity_confidence.push(prediction.complexity_confidence);
        let version = prediction
            .model_version
            .clone()
            .unwrap_or_else(|| "unreported".to_owned());
        let entry = versions.entry(version).or_default();
        entry.0 += 1;
        entry.1 += u32::from(is_correct);
        entry.2 += u32::from(prediction.accepted());
        if let Some(usage) = prediction.usage {
            with_usage += 1;
            input_tokens += usage.input_tokens.unwrap_or(0);
            output_tokens += usage.output_tokens.unwrap_or(0);
        }
    }

    let privacy: Vec<Value> = results.iter().map(privacy_row).collect();
    let privacy_failures: Vec<&Value> = privacy.iter().filter(|row| row["pass"] != true).collect();
    let curve = metrics::coverage_curve(&points, send_count, &THRESHOLD_GRID);
    let bins = metrics::reliability(&points, 10);
    let calibration = calibration_json(
        measurement,
        &points_by_split,
        &send_by_split,
        &bins,
        points.len(),
        current_thresholds,
    );
    let requirement = |binary: &Binary, points: &[(f64, bool)]| {
        let mut value = binary.json();
        value["reliability"] = metrics::bins_json(&metrics::reliability(points, 5));
        value
    };
    // Each split is scored on its own; nothing is pooled across them.
    let per_split: BTreeMap<&String, Value> = send_by_split
        .iter()
        .map(|(split, sent)| {
            let empty = Confusion::with_labels(categories.iter().copied());
            let confusion = confusion_by_split.get(split).unwrap_or(&empty);
            let (taken, taken_correct) = accepted_by_split.get(split).copied().unwrap_or((0, 0));
            let classified = confusion.total();
            (
                split,
                json!({
                    "expected_send": sent,
                    "classified": classified,
                    "accuracy": Ratio::new(confusion.correct(), classified).json(),
                    "abstention_rate": Ratio::new(sent - classified, *sent).json(),
                    "fallback_rate": Ratio::new(sent - taken, *sent).json(),
                    "accepted_coverage": Ratio::new(taken, *sent).json(),
                    "coverage_adjusted_accuracy": Ratio::new(taken_correct, taken).json(),
                    "macro_f1": confusion.macro_f1().map(round),
                    "confusion_matrix": confusion.json(),
                }),
            )
        })
        .collect();
    let mut failures_by_class: BTreeMap<&str, u32> = BTreeMap::new();
    for result in &send {
        if let Outcome::Failed(code) = &result.outcome {
            *failures_by_class.entry(failure_class(code)).or_default() += 1;
        }
    }
    let mut confidences: Vec<f64> = points.iter().map(|(confidence, _)| *confidence).collect();
    confidences.sort_by(f64::total_cmp);
    let confidence_distribution = json!({
        "samples": confidences.len(),
        "min": confidences.first().map(|value| round(*value)),
        "median": confidences.get(confidences.len() / 2).map(|value| round(*value)),
        "max": confidences.last().map(|value| round(*value)),
        "histogram_bins": bins.iter().map(|bin| json!({
            "range": [round(bin.low), round(bin.high)],
            "count": bin.count,
        })).collect::<Vec<_>>(),
    });
    let mut version_rows = BTreeMap::new();
    for (version, (n, right, taken)) in &versions {
        version_rows.insert(
            version.clone(),
            json!({
                "classified": n,
                "accuracy": Ratio::new(*right, *n).json(),
                "accepted": Ratio::new(*taken, *n).json(),
            }),
        );
    }
    let jev_calls: u32 = results.iter().map(|result| u32::from(result.invoked)).sum();
    json!({
        "report": "jev-classification-evaluation",
        "measurement": measurement.as_str(),
        "disclaimer": measurement.disclaimer(),
        "dataset": {
            "name": dataset.dataset,
            "version": dataset.version,
            "cases_total": dataset.cases.len(),
            "cases_run": results.len(),
            "truncated_by_request_budget": truncated,
            "expected_send": send_count,
            "expected_refuse": count(results.len()) - send_count,
            "by_split": send_by_split,
            "provenance": dataset.provenance,
        },
        "exploratory": measurement == Measurement::LiveJev,
        "label_review": "single_author_unreviewed",
        "outcomes": outcomes,
        "failures_by_class": failures_by_class,
        "confidence_distribution": confidence_distribution,
        "category": {
            "classified": classified,
            "accuracy": Ratio::new(correct, classified).json(),
            "abstention_rate": Ratio::new(send_count - classified, send_count).json(),
            "fallback_rate": Ratio::new(send_count - accepted, send_count).json(),
            "accepted_coverage": Ratio::new(accepted, send_count).json(),
            "coverage_adjusted_accuracy": Ratio::new(accepted_correct, accepted).json(),
            "high_confidence_misclassifications": high_confidence_errors,
            "by_split": per_split,
            "scoring_rule": "A prediction is correct when it is any acceptable category. The confusion matrix records a correct prediction under the predicted category and a wrong one under the primary label.",
        },
        "confusion_matrix": confusion.json(),
        "requirements": {
            "cut_off": REQUIREMENT_CUT_OFF,
            "note": "Yes/no requirements are probabilities with no confidence; none is derived. Unlabeled (either-answer) cases are excluded.",
            "planning": requirement(&planning, &planning_points),
            "retrieval": requirement(&retrieval, &retrieval_points),
            "tools": requirement(&tools, &tools_points),
        },
        "complexity": {
            "note": "Complexity is not labeled; only its distribution is reported.",
            "levels": complexity,
            "mean_score_confidence": mean(&complexity_confidence).map(round),
        },
        "workflow": {
            "agreement": Ratio::new(workflow_agree, workflow_labeled).json(),
            "eligible_false_positives": eligible_false_positive,
            "eligible_false_negatives": eligible_false_negative,
            "eligibility": eligibility_counts,
            "applied_in_active_with_guidance": Ratio::new(applied, send_count).json(),
            "note": "Expected eligibility is labeled for the primary category; an acceptable alternative category can legitimately map to another workflow.",
        },
        "privacy": {
            "cases": privacy.len(),
            "failures": privacy_failures.len(),
            "rows": privacy,
        },
        "calibration": calibration,
        "reliability_curve": metrics::bins_json(&bins),
        "accuracy_coverage_curve": curve.iter().map(|point| point.json()).collect::<Vec<_>>(),
        "versions": version_rows,
        "cost": {
            "jev_calls": jev_calls,
            "latency": metrics::latency_json(&latencies),
            "usage": usage_json(input_tokens, output_tokens, with_usage, pricing),
        },
    })
}

fn calibration_json(
    measurement: Measurement,
    points_by_split: &BTreeMap<String, Vec<(f64, bool)>>,
    send_by_split: &BTreeMap<String, u32>,
    bins: &[metrics::Bin],
    samples: usize,
    (default_threshold, version_thresholds): (f32, &BTreeMap<String, f32>),
) -> Value {
    let ece = metrics::expected_calibration_error(bins);
    let version_thresholds: BTreeMap<&String, f64> = version_thresholds
        .iter()
        .map(|(version, threshold)| (version, round(f64::from(*threshold))))
        .collect();
    let current = json!({
        "default_threshold": round(f64::from(default_threshold)),
        "version_thresholds": version_thresholds,
        "changed_by_this_run": false,
    });
    let ece = json!({
        "value": ece.map(round),
        "samples": samples,
        "meaningful": samples >= MIN_SAMPLES_FOR_ECE,
        "bins": 10,
    });
    if measurement == Measurement::MockContract {
        return json!({
            "status": "pending",
            "reason": "No live Jev predictions were evaluated. Scripted confidences cannot calibrate a threshold, so the existing thresholds are retained.",
            "current_thresholds": current,
            "expected_calibration_error_of_script": ece,
            "candidate_threshold": Value::Null,
        });
    }
    // Propose on the calibration split; check the proposal on the benchmark
    // split, which played no part in choosing it.
    let empty = Vec::new();
    let calibration = points_by_split.get("calibration").unwrap_or(&empty);
    let benchmark = points_by_split.get("benchmark").unwrap_or(&empty);
    let calibration_total = send_by_split.get("calibration").copied().unwrap_or(0);
    let benchmark_total = send_by_split.get("benchmark").copied().unwrap_or(0);
    let calibration_curve =
        metrics::coverage_curve(calibration, calibration_total, &THRESHOLD_GRID);
    let candidate = match metrics::candidate_threshold(
        &calibration_curve,
        TARGET_SELECTIVE_ACCURACY,
        MIN_ACCEPTED_FOR_PROPOSAL,
    ) {
        Candidate::Proposed(point) => {
            let held_out = metrics::coverage_curve(benchmark, benchmark_total, &[point.threshold]);
            json!({
                "status": "proposed_for_review",
                "threshold": point.threshold,
                "calibration_split": point.json(),
                "benchmark_split_check": held_out.first().map(|point| point.json()),
                "applied": false,
            })
        }
        Candidate::InsufficientEvidence { reason } => json!({
            "status": "insufficient_evidence",
            "reason": reason,
            "applied": false,
        }),
    };
    json!({
        "status": "measured",
        "current_thresholds": current,
        "expected_calibration_error": ece,
        "rule": format!(
            "Lowest grid threshold with at least {MIN_ACCEPTED_FOR_PROPOSAL} accepted calibration samples whose selective accuracy has a 95% Wilson lower bound of at least {TARGET_SELECTIVE_ACCURACY}. Never applied automatically; never proposed below the evidence."
        ),
        "candidate_threshold": candidate,
        "calibration_split_curve": calibration_curve.iter().map(|point| point.json()).collect::<Vec<_>>(),
    })
}

/// What kind of failure a stable Jev error code is: the network or service
/// (`transport`), the credential (`credential`), or the request/response
/// contract (`api_contract`). Classification quality is never a failure
/// code; it is measured against labels.
pub(crate) fn failure_class(code: &str) -> &'static str {
    match code {
        "connect" | "transport" | "timeout" | "cancelled" | "status" | "overloaded"
        | "rate_limited" => "transport",
        "unauthorized" => "credential",
        _ => "api_contract",
    }
}

fn count(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

fn mean(values: &[f64]) -> Option<f64> {
    (!values.is_empty()).then(|| values.iter().sum::<f64>() / values.len() as f64)
}

fn metrics_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// The directory reports go to, when one was requested.
pub(crate) fn output_dir() -> Option<PathBuf> {
    std::env::var_os("LIGHTAGENT_JEV_EVAL_OUT")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// Write `report` as `<name>.json` and `summary` as `<name>.md`, when an
/// output directory was requested. Reports hold only fixed vocabulary,
/// numbers, case ids and synthetic marker strings.
pub(crate) fn write_report(name: &str, report: &Value, summary: &str) {
    let Some(dir) = output_dir() else {
        return;
    };
    std::fs::create_dir_all(&dir).expect("the report directory can be created");
    let text = serde_json::to_string_pretty(report).expect("a report serializes");
    std::fs::write(dir.join(format!("{name}.json")), text).expect("the report is written");
    std::fs::write(dir.join(format!("{name}.md")), summary).expect("the summary is written");
}

fn ratio_text(value: &Value) -> String {
    match value["value"].as_f64() {
        Some(rate) => format!(
            "{:.1}% ({}/{})",
            rate * 100.0,
            value["numerator"],
            value["denominator"]
        ),
        None => format!("n/a (0/{})", value["denominator"]),
    }
}

/// A human-readable summary of a classification report.
pub(crate) fn summary(report: &Value) -> String {
    let category = &report["category"];
    let mut text = format!(
        "# Jev classification evaluation ({})\n\n> {}\n\n",
        report["measurement"].as_str().unwrap_or_default(),
        report["disclaimer"].as_str().unwrap_or_default()
    );
    text.push_str(&format!(
        "Dataset {} v{}: {} cases run of {} ({} expected to be sent).\n\n",
        report["dataset"]["name"].as_str().unwrap_or_default(),
        report["dataset"]["version"].as_str().unwrap_or_default(),
        report["dataset"]["cases_run"],
        report["dataset"]["cases_total"],
        report["dataset"]["expected_send"],
    ));
    text.push_str("| Metric | Value |\n|---|---|\n");
    for (label, value) in [
        ("Category accuracy (classified)", &category["accuracy"]),
        ("Accepted coverage", &category["accepted_coverage"]),
        (
            "Coverage-adjusted accuracy",
            &category["coverage_adjusted_accuracy"],
        ),
        ("Abstention rate", &category["abstention_rate"]),
        ("Fallback rate", &category["fallback_rate"]),
        ("Workflow agreement", &report["workflow"]["agreement"]),
    ] {
        text.push_str(&format!("| {label} | {} |\n", ratio_text(value)));
    }
    text.push_str(&format!(
        "| Macro F1 | {} |\n| High-confidence misclassifications | {} |\n| Privacy failures | {} of {} |\n| Jev calls | {} |\n| Latency p50 / p95 | {} / {} ms |\n| Tokens in / out | {} / {} |\n| Estimated cost | {} |\n",
        report["confusion_matrix"]["macro_f1"],
        category["high_confidence_misclassifications"].as_array().map_or(0, Vec::len),
        report["privacy"]["failures"],
        report["privacy"]["cases"],
        report["cost"]["jev_calls"],
        report["cost"]["latency"]["p50_ms"],
        report["cost"]["latency"]["p95_ms"],
        report["cost"]["usage"]["input_tokens"],
        report["cost"]["usage"]["output_tokens"],
        match report["cost"]["usage"]["estimated_cost_usd"].as_f64() {
            Some(cost) => format!("${cost:.4}"),
            None => "not estimated (pricing unverified)".to_owned(),
        },
    ));
    text.push_str(&format!(
        "\nCalibration: **{}**. Current default threshold {}; this run changed no threshold.\n",
        report["calibration"]["status"].as_str().unwrap_or_default(),
        report["calibration"]["current_thresholds"]["default_threshold"],
    ));
    text
}

/// The threshold analysis a classification report carries, as its own
/// artifact: calibration status, the reliability and accuracy-coverage
/// curves, and any proposal. Nothing in it is applied.
pub(crate) fn write_threshold_analysis(name: &str, report: &Value) {
    let analysis = json!({
        "report": "jev-threshold-analysis",
        "measurement": report["measurement"],
        "disclaimer": report["disclaimer"],
        "calibration": report["calibration"],
        "reliability_curve": report["reliability_curve"],
        "accuracy_coverage_curve": report["accuracy_coverage_curve"],
        "versions": report["versions"],
        "high_confidence_misclassifications": report["category"]["high_confidence_misclassifications"],
    });
    let mut text = format!(
        "# Jev threshold analysis ({})\n\nCalibration status: **{}**. No threshold is changed by an evaluation run.\n\n| Threshold | Coverage | Selective accuracy | Fallback rate |\n|---|---|---|---|\n",
        report["measurement"].as_str().unwrap_or_default(),
        report["calibration"]["status"].as_str().unwrap_or_default(),
    );
    for point in report["accuracy_coverage_curve"]
        .as_array()
        .into_iter()
        .flatten()
    {
        text.push_str(&format!(
            "| {} | {} | {} | {} |\n",
            point["threshold"],
            ratio_text(&point["coverage"]),
            ratio_text(&point["selective_accuracy"]),
            ratio_text(&point["fallback_rate"]),
        ));
    }
    write_report(name, &analysis, &text);
}

/// The thresholds in force in `config`.
pub(crate) fn thresholds(config: &Config) -> (f32, BTreeMap<String, f32>) {
    let settings = &config.platform.jev.classification;
    (
        settings.default_threshold,
        settings.version_thresholds.clone(),
    )
}

/// Which sanitizer refusal, if any, the case's message meets offline.
pub(crate) fn offline_refusal(case: &Case, config: &Config) -> Option<&'static str> {
    let settings = &config.platform.jev.classification;
    if !settings.permits_profile(&case.profile) {
        return Some("profile_not_permitted");
    }
    if case.attachment {
        return Some("attachment");
    }
    sanitize::summarize(&case.message(), settings.max_input_chars)
        .err()
        .map(|refusal| refusal.code())
}

pub(crate) mod tests;
