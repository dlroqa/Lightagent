//! Fail-closed advisory routing through TypeSafe Jev.
//!
//! Jev receives a bounded user message and the already configured model
//! allowlist. Its answer can only replace the provider model for this run; it
//! never selects a profile and therefore cannot alter tools, approval policy,
//! workspace, persona, or limits.

use std::collections::BTreeSet;
use std::time::Duration;

use lightagent_core::JevConfig;
use serde_json::{Value, json};

/// A successfully validated advisory route.
#[derive(Clone, Debug, PartialEq)]
struct Decision {
    model: String,
    confidence: f32,
}

/// Resolve the model Jev recommends, or retain `default_model` on every
/// unavailable, malformed, low-confidence, or unauthorized outcome.
///
/// `explicit_model` is intentionally checked before any outbound request: a
/// caller-selected model is an explicit contract and Jev is advisory only.
pub(crate) async fn select_model(
    config: &JevConfig,
    message: &str,
    default_model: String,
    explicit_model: bool,
) -> String {
    if explicit_model || !config.endpoint.enabled || config.allowed_models.is_empty() {
        return default_model;
    }
    let Some(base_url) = config.endpoint.base_url.as_deref() else {
        return default_model;
    };
    let allowed_models: BTreeSet<_> = config
        .allowed_models
        .iter()
        .map(|model| model.trim())
        .filter(|model| !model.is_empty())
        .map(str::to_owned)
        .collect();
    if allowed_models.is_empty() {
        return default_model;
    }
    // This workspace deliberately builds reqwest with `rustls-no-provider`;
    // install its approved ring provider before constructing the client.
    lightagent_provider_lightweight::ensure_provider();
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(config.timeout_secs.max(1)))
        .build()
    {
        Ok(client) => client,
        Err(_) => return default_model,
    };
    let endpoint = format!("{}/v1/systemone", base_url.trim_end_matches('/'));
    let request = json!({
        "model": config.model,
        "input": {
            "message": bounded_message(message),
            "allowed_models": allowed_models,
        }
    });
    let mut request = client.post(endpoint).json(&request);
    if let Some(secret) = config
        .endpoint
        .api_key
        .as_ref()
        .and_then(|key| key.resolve())
    {
        request = request.bearer_auth(secret);
    }
    let response = match request.send().await {
        Ok(response) if response.status().is_success() => response,
        _ => return default_model,
    };
    let body = match response.json::<Value>().await {
        Ok(body) => body,
        Err(_) => return default_model,
    };
    let Some(decision) = parse_decision(&body) else {
        return default_model;
    };
    if decision.confidence < config.confidence_threshold
        || !allowed_models.contains(&decision.model)
    {
        return default_model;
    }
    decision.model
}

fn bounded_message(message: &str) -> String {
    const MAX_CHARS: usize = 4_096;
    message.chars().take(MAX_CHARS).collect()
}

/// Accept the stable, typed `{ model, confidence }` result and the equivalent
/// `output` envelope returned by the hosted API. Anything else is rejected.
fn parse_decision(value: &Value) -> Option<Decision> {
    let result = value.get("output").unwrap_or(value);
    let model = ["model", "route", "choice"]
        .iter()
        .find_map(|field| result.get(*field).and_then(Value::as_str))?
        .trim();
    let confidence = ["confidence", "score"]
        .iter()
        .find_map(|field| result.get(*field).and_then(Value::as_f64))?;
    if model.is_empty() || !confidence.is_finite() || !(0.0..=1.0).contains(&confidence) {
        return None;
    }
    Some(Decision {
        model: model.to_owned(),
        confidence: confidence as f32,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightagent_core::PlatformEndpointConfig;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[test]
    fn parses_typed_and_enveloped_responses() {
        assert_eq!(
            parse_decision(&json!({"model": "fast", "confidence": 0.91})),
            Some(Decision {
                model: "fast".to_owned(),
                confidence: 0.91,
            })
        );
        assert_eq!(
            parse_decision(&json!({"output": {"choice": "careful", "score": 0.9}})),
            Some(Decision {
                model: "careful".to_owned(),
                confidence: 0.9,
            })
        );
    }

    #[test]
    fn rejects_malformed_or_out_of_range_decisions() {
        assert!(parse_decision(&json!({"model": "fast"})).is_none());
        assert!(parse_decision(&json!({"model": "fast", "confidence": 1.1})).is_none());
        assert!(parse_decision(&json!({"model": "", "confidence": 0.8})).is_none());
    }

    #[test]
    fn bounds_routing_prompt_without_splitting_unicode() {
        let message = "🦀".repeat(5_000);
        assert_eq!(bounded_message(&message).chars().count(), 4_096);
    }

    #[tokio::test]
    async fn accepts_only_an_allowlisted_high_confidence_http_route() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = vec![0_u8; 4_096];
            let read = stream.read(&mut request).await.unwrap();
            let request = String::from_utf8_lossy(&request[..read]);
            assert!(request.starts_with("POST /v1/systemone HTTP/1.1"));
            assert!(request.contains("\"allowed_models\":[\"careful\",\"fast\"]"));
            let body = r#"{"output":{"choice":"careful","score":0.91}}"#;
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        });
        let config = JevConfig {
            endpoint: PlatformEndpointConfig {
                enabled: true,
                base_url: Some(format!("http://{address}")),
                api_key: None,
            },
            model: "jev-test".to_owned(),
            confidence_threshold: 0.85,
            allowed_models: vec!["fast".to_owned(), "careful".to_owned()],
            timeout_secs: 3,
        };
        assert_eq!(
            select_model(&config, "route this", "default".to_owned(), false).await,
            "careful"
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn explicit_model_and_low_confidence_retain_the_default_route() {
        let config = JevConfig {
            endpoint: PlatformEndpointConfig {
                enabled: true,
                // An explicit selection must not try to contact this address.
                base_url: Some("http://127.0.0.1:1".to_owned()),
                api_key: None,
            },
            allowed_models: vec!["fast".to_owned()],
            ..JevConfig::default()
        };
        assert_eq!(
            select_model(&config, "route this", "user-chosen".to_owned(), true).await,
            "user-chosen"
        );
        assert_eq!(
            parse_decision(&json!({"model": "fast", "confidence": 0.2}))
                .filter(|decision| decision.confidence >= config.confidence_threshold),
            None
        );
    }
}
