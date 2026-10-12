//! Model routing with `legacy_raw` versus `sanitized` input, over the same
//! synthetic cases, through the production router
//! ([`route_for_turn`](crate::jev::route_for_turn)).
//!
//! Routing is measured apart from classification: it is a different question
//! with its own input policy. The route options carry only model names, so
//! no independent "correct route" label can be written for these cases and
//! routing accuracy is reported as not measured rather than invented.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use lightagent_core::{
    JevConfig, JevRoutingInput, JevRoutingPermission, PlatformEndpointConfig, SecretRef,
};
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

use super::metrics::{self, Ratio, round};
use super::{Case, Dataset, Measurement, Pricing, count, usage_json, write_report};
use crate::coordinator::classify::tests::read_request;

/// The allowlisted models every case may be routed to.
pub(crate) const ROUTES: [&str; 2] = ["fast", "careful"];
/// The configured model routing keeps when it falls back.
pub(crate) const CONFIGURED: &str = "configured-model";
/// Fallback codes that mean the sanitizer refused to send anything.
const REFUSALS: [&str; 4] = [
    "attachment",
    "key_material",
    "empty_after_sanitization",
    "input_too_long",
];

/// Routing over the benchmark's allowlist. Under `strict`, only
/// [`PERMITTED_PROFILE`](super::PERMITTED_PROFILE) may route, the same
/// profile classification permits.
pub(crate) fn routing_config(
    url: &str,
    api_key: Option<SecretRef>,
    model: Option<&str>,
    input: JevRoutingInput,
    permission: JevRoutingPermission,
) -> JevConfig {
    let mut config = JevConfig {
        endpoint: PlatformEndpointConfig {
            enabled: true,
            base_url: Some(url.to_owned()),
            api_key,
        },
        allowed_models: ROUTES.map(str::to_owned).to_vec(),
        timeout_secs: 5,
        routing_input: input,
        routing_permission: permission,
        routing_allowed_profiles: vec![super::PERMITTED_PROFILE.to_owned()],
        ..JevConfig::default()
    };
    if let Some(model) = model {
        config.model = model.to_owned();
    }
    config
}

/// One case routed under one input policy.
pub(crate) struct RoutingRow {
    pub case: Case,
    pub input: JevRoutingInput,
    /// What the policy would send, or the refusal code.
    pub state: Result<String, &'static str>,
    pub report: crate::jev::RouteReport,
    pub latency_ms: u64,
    /// The `state` of each request that reached Jev.
    pub sent: Vec<String>,
}

impl RoutingRow {
    fn route(&self) -> String {
        match (&self.report.decision.model, &self.report.decision.profile) {
            (_, Some(profile)) => format!("profile:{profile}"),
            (Some(model), None) => model.clone(),
            (None, None) => CONFIGURED.to_owned(),
        }
    }
}

/// Route every case as its profile under both input policies, sequentially,
/// stopping once `max_requests` requests have been made.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_routing<'a, S>(
    cases: impl IntoIterator<Item = &'a Case>,
    url: &str,
    api_key: Option<SecretRef>,
    model: Option<&str>,
    permission: JevRoutingPermission,
    max_requests: u32,
    mut sent: S,
) -> (Vec<RoutingRow>, bool)
where
    S: FnMut(&RoutingRow) -> Vec<String>,
{
    let mut rows = Vec::new();
    let mut requests = 0u32;
    for case in cases {
        // Both policies run for a case, or neither, so pairs stay complete.
        if requests + 2 > max_requests {
            return (rows, true);
        }
        for input in [JevRoutingInput::LegacyRaw, JevRoutingInput::Sanitized] {
            let config = routing_config(url, api_key.clone(), model, input, permission);
            let message = case.message();
            let state = crate::jev::routing_state(&config, &message, case.attachment)
                .map_err(|fallback| fallback.code());
            let started = Instant::now();
            let report = crate::jev::route_for_turn(
                &config,
                &case.profile,
                &message,
                CONFIGURED.to_owned(),
                false,
                case.attachment,
                &CancellationToken::new(),
            )
            .await;
            requests += u32::from(report.invoked);
            let mut row = RoutingRow {
                case: case.clone(),
                input,
                state,
                report,
                latency_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                sent: Vec::new(),
            };
            row.sent = sent(&row);
            rows.push(row);
        }
    }
    (rows, false)
}

/// Markers protected by a sanitizer pattern, as opposed to a case whose only
/// protection is the profile permission, which routing lacks under the
/// compatibility policy.
fn pattern_markers(case: &Case) -> &[String] {
    if case.fallback.as_deref() == Some("profile_not_permitted") {
        &[]
    } else {
        &case.markers
    }
}

fn transmitted<'a>(markers: &'a [String], sent: &[String]) -> Vec<&'a String> {
    markers
        .iter()
        .filter(|marker| sent.iter().any(|state| state.contains(marker.as_str())))
        .collect()
}

/// Score both policies and their agreement.
pub(crate) fn score_routing(
    dataset: &Dataset,
    rows: &[RoutingRow],
    measurement: Measurement,
    pricing: Option<&Pricing>,
    truncated: bool,
) -> Value {
    let mut modes = serde_json::Map::new();
    for input in [JevRoutingInput::LegacyRaw, JevRoutingInput::Sanitized] {
        let mine: Vec<&RoutingRow> = rows.iter().filter(|row| row.input == input).collect();
        let total = count(mine.len());
        let requested: Vec<&&RoutingRow> = mine.iter().filter(|row| !row.sent.is_empty()).collect();
        let refusals = mine
            .iter()
            .filter(|row| {
                row.report
                    .fallback
                    .is_some_and(|code| REFUSALS.contains(&code))
            })
            .count();
        let mut fallbacks: BTreeMap<&str, u32> = BTreeMap::new();
        let mut routes: BTreeMap<String, u32> = BTreeMap::new();
        let (mut markers, mut marker_cases, mut gated, mut gaps) =
            (0usize, 0usize, Vec::new(), Vec::new());
        let (mut input_tokens, mut output_tokens, mut with_usage) = (0u64, 0u64, 0u32);
        for row in &mine {
            if let Some(code) = row.report.fallback {
                *fallbacks.entry(code).or_default() += 1;
            }
            *routes.entry(row.route()).or_default() += 1;
            let leaked = transmitted(pattern_markers(&row.case), &row.sent);
            markers += leaked.len();
            marker_cases += usize::from(!leaked.is_empty());
            if pattern_markers(&row.case).is_empty() && !row.case.markers.is_empty() {
                for marker in transmitted(&row.case.markers, &row.sent) {
                    gated.push(json!({"id": row.case.id, "marker": marker}));
                }
            }
            for gap in transmitted(&row.case.known_gaps, &row.sent) {
                gaps.push(json!({"id": row.case.id, "marker": gap}));
            }
            if let Some(usage) = row.report.usage {
                with_usage += 1;
                input_tokens += usage.input_tokens.unwrap_or(0);
                output_tokens += usage.output_tokens.unwrap_or(0);
            }
        }
        let chars: Vec<u64> = requested
            .iter()
            .flat_map(|row| row.sent.iter().map(|state| state.chars().count() as u64))
            .collect();
        let latencies: Vec<u64> = requested.iter().map(|row| row.latency_ms).collect();
        modes.insert(
            input.as_str().to_owned(),
            json!({
                "cases": total,
                "requests": count(requested.len()),
                "sanitizer_refusal_rate": Ratio::new(count(refusals), total).json(),
                "fallback_rate": Ratio::new(fallbacks.values().sum(), total).json(),
                "fallbacks": fallbacks,
                "routes": routes,
                "chars_sent": {
                    "total": chars.iter().sum::<u64>(),
                    "mean": (!chars.is_empty()).then(|| round(chars.iter().sum::<u64>() as f64 / chars.len() as f64)),
                },
                "pattern_markers_transmitted": markers,
                "cases_with_pattern_markers_transmitted": marker_cases,
                "profile_gated_markers_transmitted": gated,
                "known_gaps_transmitted": gaps,
                "latency": metrics::latency_json(&latencies),
                "usage": usage_json(input_tokens, output_tokens, with_usage, pricing),
            }),
        );
    }

    // Pairwise: the same case under both policies.
    let mut both = 0u32;
    let mut agree = 0u32;
    let mut altered = Vec::new();
    let mut reductions = Vec::new();
    for pair in rows.chunks(2) {
        let [raw, sanitized] = pair else { continue };
        if raw.case.id != sanitized.case.id {
            continue;
        }
        if let (Ok(raw_state), Ok(sanitized_state)) = (&raw.state, &sanitized.state) {
            let raw_chars = raw_state.chars().count();
            if raw_chars > 0 {
                let kept = sanitized_state.chars().count() as f64 / raw_chars as f64;
                reductions.push(1.0 - kept);
            }
        }
        if raw.sent.is_empty() || sanitized.sent.is_empty() {
            continue;
        }
        both += 1;
        if raw.route() == sanitized.route() {
            agree += 1;
        } else {
            altered.push(json!({
                "id": raw.case.id,
                "legacy_raw": raw.route(),
                "sanitized": sanitized.route(),
            }));
        }
    }
    let mean_reduction = (!reductions.is_empty())
        .then(|| round(reductions.iter().sum::<f64>() / reductions.len() as f64));
    let disclaimer = match measurement {
        Measurement::MockContract => {
            "A content-sensitive stand-in router answered, so these route changes show only that minimization can change a decision. They say nothing about Jev's routing."
        }
        Measurement::LiveJev => {
            "Live TypeSafe Jev routing over synthetic inputs. Route options carry only model names."
        }
    };
    json!({
        "report": "jev-routing-input-benchmark",
        "measurement": measurement.as_str(),
        "disclaimer": disclaimer,
        "dataset": {"name": dataset.dataset, "version": dataset.version, "cases_run": rows.len() / 2, "truncated_by_request_budget": truncated},
        "modes": modes,
        "agreement": {
            "both_requested": both,
            "same_route": Ratio::new(agree, both).json(),
            "altered": altered,
        },
        "character_reduction": {
            "mean_fraction_removed": mean_reduction,
            "pairs": reductions.len(),
        },
        "accuracy_against_labels": {
            "value": Value::Null,
            "reason": "No independent route labels exist: the route options describe model names only, so no route can be called correct for a task.",
        },
        "configuration_semantics": "Unchanged. Existing configurations keep legacy_raw; nothing is migrated.",
    })
}

pub(crate) fn routing_summary(report: &Value) -> String {
    let mut text = format!(
        "# Jev routing input: legacy_raw vs sanitized ({})\n\n> {}\n\n| | legacy_raw | sanitized |\n|---|---|---|\n",
        report["measurement"].as_str().unwrap_or_default(),
        report["disclaimer"].as_str().unwrap_or_default(),
    );
    let modes = &report["modes"];
    for (label, key) in [
        ("Requests", "requests"),
        ("Pattern markers transmitted", "pattern_markers_transmitted"),
    ] {
        text.push_str(&format!(
            "| {label} | {} | {} |\n",
            modes["legacy_raw"][key], modes["sanitized"][key]
        ));
    }
    for (label, key) in [
        ("Sanitizer refusal rate", "sanitizer_refusal_rate"),
        ("Fallback rate", "fallback_rate"),
    ] {
        text.push_str(&format!(
            "| {label} | {} | {} |\n",
            modes["legacy_raw"][key]["value"], modes["sanitized"][key]["value"]
        ));
    }
    text.push_str(&format!(
        "| Mean characters sent | {} | {} |\n| Latency p50 (ms) | {} | {} |\n| Tokens in | {} | {} |\n\nSame route when both sent: {}/{}. Mean fraction of characters removed: {}. Routing accuracy: not measured (no independent route labels).\n",
        modes["legacy_raw"]["chars_sent"]["mean"],
        modes["sanitized"]["chars_sent"]["mean"],
        modes["legacy_raw"]["latency"]["p50_ms"],
        modes["sanitized"]["latency"]["p50_ms"],
        modes["legacy_raw"]["usage"]["input_tokens"],
        modes["sanitized"]["usage"]["input_tokens"],
        report["agreement"]["same_route"]["numerator"],
        report["agreement"]["same_route"]["denominator"],
        report["character_reduction"]["mean_fraction_removed"],
    ));
    text
}

/// The stand-in's rule: technical-looking text goes to `careful`, short text
/// to `fast`, everything else keeps the default. It is deliberately
/// content-sensitive so that the benchmark can show minimization changing a
/// route; it models no real router.
fn stand_in_route(state: &str) -> &'static str {
    let technical = ["```", "/", "\\", "://", "@", "=", "-----"]
        .iter()
        .any(|marker| state.contains(marker));
    if technical {
        "model:careful"
    } else if state.chars().count() <= 60 {
        "model:fast"
    } else {
        "default"
    }
}

/// A stand-in router recording each request's `state`.
async fn stand_in() -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let body = read_request(&mut stream).await;
            let state = serde_json::from_str::<Value>(&body)
                .ok()
                .and_then(|body| body["state"].as_str().map(str::to_owned))
                .unwrap_or_default();
            log.lock().unwrap().push(state.clone());
            let choice = stand_in_route(&state);
            let mut probabilities = serde_json::Map::new();
            for option in ["default", "model:fast", "model:careful"] {
                let value = if option == choice { 0.95 } else { 0.025 };
                probabilities.insert(option.to_owned(), json!(value));
            }
            let answer = json!({
                "model": "jev-mock-1.0.0",
                "answers": {"route": {"type": "choice", "choice": choice, "confidence": 0.95, "probabilities": probabilities}},
                "usage": {"input_tokens": state.chars().count().div_ceil(4), "output_tokens": 5},
            })
            .to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{answer}",
                answer.len()
            );
            let _ = stream.write_all(response.as_bytes()).await;
        }
    });
    (url, seen)
}

#[tokio::test]
async fn legacy_raw_and_sanitized_routing_are_compared_reproducibly() {
    let dataset = super::dataset();
    let (url, seen) = stand_in().await;
    // The compatibility policy routes every profile: the benchmark as it was
    // before the routing permission existed.
    let (rows, truncated) = run_routing(
        dataset.cases.iter(),
        &url,
        None,
        None,
        JevRoutingPermission::LegacyAllProfiles,
        u32::MAX,
        |_| std::mem::take(&mut *seen.lock().unwrap()),
    )
    .await;
    assert!(!truncated);
    let report = score_routing(&dataset, &rows, Measurement::MockContract, None, false);
    write_report("routing-mock", &report, &routing_summary(&report));
    let raw = &report["modes"]["legacy_raw"];
    let sanitized = &report["modes"]["sanitized"];

    // legacy_raw sends every case as written, including the key, the
    // attachment and the over-long paste; sanitized refuses those seven.
    assert_eq!(raw["requests"], 101);
    assert_eq!(raw["sanitizer_refusal_rate"]["numerator"], 0);
    assert_eq!(sanitized["sanitizer_refusal_rate"]["numerator"], 7);
    assert_eq!(sanitized["requests"], 94);
    for code in REFUSALS {
        assert!(
            sanitized["fallbacks"][code].as_u64().unwrap_or(0) >= 1,
            "{code}"
        );
    }

    // Pattern-protected markers: sanitized transmits none; legacy_raw
    // transmits them, including private-key material.
    assert_eq!(sanitized["pattern_markers_transmitted"], 0);
    assert!(raw["pattern_markers_transmitted"].as_u64().unwrap() >= 20);
    let key_case = rows
        .iter()
        .find(|row| row.case.id == "priv-007" && row.input == JevRoutingInput::LegacyRaw)
        .unwrap();
    assert!(key_case.sent.iter().any(|state| state.contains("OPENSSH")));

    // Under the compatibility policy routing has no per-profile gate: the
    // case only the profile permission protects is still sent, under both
    // input policies. `strict` closes it (below).
    for mode in [raw, sanitized] {
        assert_eq!(
            mode["profile_gated_markers_transmitted"],
            json!([{"id": "priv-009", "marker": "private journal"}])
        );
    }
    // The documented prose gap reaches Jev under both policies.
    assert_eq!(sanitized["known_gaps_transmitted"][0]["id"], "priv-010");

    // Minimization removes text and, for this stand-in, changes some routes.
    assert!(
        report["character_reduction"]["mean_fraction_removed"]
            .as_f64()
            .unwrap()
            > 0.0
    );
    let altered = report["agreement"]["altered"].as_array().unwrap();
    assert!(altered.iter().any(|row| row["id"] == "priv-001"));
    assert_eq!(
        report["agreement"]["both_requested"], 94,
        "every case sanitized routing sent was also sent raw"
    );
    assert_eq!(report["accuracy_against_labels"]["value"], Value::Null);
    // Auto is never routed and no route outside the allowlist is taken.
    for row in &rows {
        let route = row.route();
        assert!(
            route == CONFIGURED || ROUTES.contains(&route.as_str()),
            "{}: {route}",
            row.case.id
        );
    }
}

#[tokio::test]
async fn strict_routing_permission_stops_profile_gated_markers_under_both_input_policies() {
    let dataset = super::dataset();
    let (url, seen) = stand_in().await;
    let (rows, truncated) = run_routing(
        dataset.cases.iter(),
        &url,
        None,
        None,
        JevRoutingPermission::Strict,
        u32::MAX,
        |_| std::mem::take(&mut *seen.lock().unwrap()),
    )
    .await;
    assert!(!truncated);
    let report = score_routing(&dataset, &rows, Measurement::MockContract, None, false);
    let denied: Vec<&str> = dataset
        .cases
        .iter()
        .filter(|case| case.profile != super::PERMITTED_PROFILE)
        .map(|case| case.id.as_str())
        .collect();
    assert!(denied.contains(&"priv-009"), "{denied:?}");
    for input in ["legacy_raw", "sanitized"] {
        let mode = &report["modes"][input];
        assert_eq!(
            mode["profile_gated_markers_transmitted"],
            json!([]),
            "{input}"
        );
        assert_eq!(
            mode["fallbacks"]["profile_not_permitted"].as_u64(),
            Some(denied.len() as u64),
            "{input}"
        );
    }
    // Compared with the compatibility run, exactly the denied cases drop out.
    assert_eq!(
        report["modes"]["legacy_raw"]["requests"],
        101 - denied.len()
    );
    for row in rows
        .iter()
        .filter(|row| denied.contains(&row.case.id.as_str()))
    {
        assert!(row.sent.is_empty(), "{}", row.case.id);
        assert!(!row.report.invoked, "{}", row.case.id);
        assert_eq!(
            row.report.input, None,
            "{}: nothing was prepared",
            row.case.id
        );
        assert_eq!(row.route(), CONFIGURED);
    }
    // Permitted profiles keep the routing they had.
    assert_eq!(
        report["modes"]["sanitized"]["pattern_markers_transmitted"],
        0
    );
}
