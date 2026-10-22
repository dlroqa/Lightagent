//! The `AgentProvider` adapter for the OpenAI-compatible gateway.
//!
//! Builds a streamed `POST /v1/chat/completions`, decodes the SSE response with
//! this crate's own [`SseDecoder`], and maps each chunk onto a
//! [`ProviderEvent`]. It knows the wire contract — `stream: true`,
//! `stream_options.include_usage: true`, an optional bearer key, index-keyed
//! tool-call deltas, an empty-`choices` usage chunk, a `[DONE]` terminator and
//! a terminal `error` chunk — and it depends only on `lightagent-core`, never
//! on any `lightweight-*` crate.
//!
//! A turn is finished only when the stream says so: a `finish_reason` (which
//! stays valid if the body then ends without `[DONE]`) or a `[DONE]`. A body
//! that ends with neither is reported as [`FinishReason::Incomplete`], never
//! as a successful stop, so a cut connection cannot pass for an answer.

use std::collections::VecDeque;

use async_trait::async_trait;
use bytes::Bytes;
use futures_util::stream::{self, BoxStream, StreamExt};
use lightagent_core::provider::{
    AgentProvider, FinishReason, ProviderError, ProviderEvent, ProviderMessage, ProviderRequest,
    ProviderStream, Role, Usage,
};
use serde_json::{Map, Value, json};
use tokio_util::sync::CancellationToken;

use crate::names::ToolNameMap;
use crate::sse::{SseDecodeError, SseDecoder, SseEvent};
use crate::tls;
use crate::wire::{ChatChunk, ModelsResponse};

/// How to reach the gateway.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderConfig {
    /// The OpenAI-compatible base URL, e.g. `http://127.0.0.1:11434`.
    pub base_url: String,
    /// The catalog model id sent on every request. Empty or `default` discovers
    /// the model advertised by the gateway before each generation.
    pub model: String,
    /// The bearer key, when the gateway requires one. Loopback needs none.
    pub api_key: Option<String>,
    /// How tool names are spelled on the wire (see [`crate::names`]).
    pub tool_names: lightagent_core::ToolNameMode,
}

impl ProviderConfig {
    pub fn new(base_url: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            model: model.into(),
            api_key: None,
            tool_names: lightagent_core::ToolNameMode::Native,
        }
    }

    /// Set how tool names are spelled on the wire.
    pub fn with_tool_names(mut self, tool_names: lightagent_core::ToolNameMode) -> Self {
        self.tool_names = tool_names;
        self
    }

    /// Set the bearer key.
    pub fn with_api_key(mut self, api_key: impl Into<String>) -> Self {
        self.api_key = Some(api_key.into());
        self
    }
}

/// An [`AgentProvider`] backed by the OpenAI-compatible gateway.
#[derive(Clone, Debug)]
pub struct LightweightProvider {
    client: reqwest::Client,
    config: ProviderConfig,
}

impl LightweightProvider {
    /// Build a provider, installing the rustls provider first.
    ///
    /// Without [`tls::ensure_provider`], `reqwest::Client::builder().build()`
    /// panics even for plain HTTP — see [`crate::tls`].
    pub fn new(config: ProviderConfig) -> Result<Self, ProviderError> {
        tls::ensure_provider();
        let client = reqwest::Client::builder()
            .build()
            .map_err(|err| ProviderError::Transport(err.to_string()))?;
        Ok(Self { client, config })
    }

    /// The base URL, without a trailing slash.
    fn base(&self) -> &str {
        self.config.base_url.trim_end_matches('/')
    }

    /// Resolve the configured model against the gateway's current model list.
    ///
    /// Do not cache: the panel can load a different model between runs. A
    /// configured model wins when the gateway advertises it. Lightweight serves
    /// one resident model, so that sole model wins over a stale profile value.
    pub async fn resolve_model(&self) -> Result<String, ProviderError> {
        let configured = self.config.model.trim();
        let models = self
            .models()
            .await?
            .into_iter()
            .filter(|model| !model.trim().is_empty())
            .collect::<Vec<_>>();
        if !configured.is_empty() && models.iter().any(|model| model == configured) {
            return Ok(self.config.model.clone());
        }
        match models.as_slice() {
            [model] => Ok(model.clone()),
            [] => Err(ProviderError::Upstream(
                "No model is loaded. Load a model in Lightweight's Models screen, then retry."
                    .into(),
            )),
            _ => Err(ProviderError::Upstream(format!(
                "Model {configured:?} is not available. The provider advertises multiple models; set inference.model or the profile's model to one of them."
            ))),
        }
    }

    /// List the model IDs currently advertised by the gateway.
    pub async fn models(&self) -> Result<Vec<String>, ProviderError> {
        let mut request = self
            .client
            .get(format!("{}/v1/models", self.base()))
            .timeout(std::time::Duration::from_secs(10));
        if let Some(key) = &self.config.api_key {
            request = request.bearer_auth(key);
        }
        let response = request
            .send()
            .await
            .map_err(|err| ProviderError::Transport(err.to_string()))?;
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|err| ProviderError::Transport(err.to_string()))?;
        if !status.is_success() {
            return Err(ProviderError::Upstream(format!(
                "the provider answered {status} to GET /v1/models"
            )));
        }
        let parsed: ModelsResponse =
            serde_json::from_str(&body).map_err(|err| ProviderError::Upstream(err.to_string()))?;
        Ok(parsed.data.into_iter().map(|entry| entry.id).collect())
    }
}

/// Build the OpenAI request body for a turn.
///
/// Free-standing so it is testable without a client: the request-body-shape
/// test calls this directly.
pub(crate) fn build_body(config: &ProviderConfig, request: &ProviderRequest) -> Value {
    body_with(config, request, None)
}

/// The tool-name map for `request` under `config`: `None` sends names as
/// they are. Built from every name the request carries, declared or in its
/// history, so a follow-up or resumed request spells each one the same way.
pub(crate) fn name_map(
    config: &ProviderConfig,
    request: &ProviderRequest,
) -> Result<Option<ToolNameMap>, ProviderError> {
    if config.tool_names == lightagent_core::ToolNameMode::Native {
        return Ok(None);
    }
    let declared = request.tools.iter().map(|tool| tool.name.as_str());
    let called = request
        .messages
        .iter()
        .flat_map(|message| message.tool_calls.iter().map(|call| call.name.as_str()));
    ToolNameMap::portable(declared.chain(called)).map(Some)
}

/// The request body, with tool names spelled through `names` when given.
pub(crate) fn body_with(
    config: &ProviderConfig,
    request: &ProviderRequest,
    names: Option<&ToolNameMap>,
) -> Value {
    let mut body = Map::new();
    body.insert("model".into(), json!(config.model));
    body.insert(
        "messages".into(),
        messages_to_json(&request.messages, names),
    );
    // The usage chunk is not optional: the loop reads token counts from it.
    body.insert("stream".into(), Value::Bool(true));
    body.insert("stream_options".into(), json!({ "include_usage": true }));
    if !request.tools.is_empty() {
        body.insert("tools".into(), tools_to_json(&request.tools, names));
    }
    if let Some(temperature) = request.temperature {
        body.insert("temperature".into(), json!(temperature));
    }
    if let Some(max_tokens) = request.max_tokens {
        body.insert("max_tokens".into(), json!(max_tokens));
    }
    Value::Object(body)
}

/// The wire spelling of an internal tool name.
fn wire_name(internal: &str, names: Option<&ToolNameMap>) -> String {
    match names {
        Some(names) => names.outbound(internal),
        None => internal.to_owned(),
    }
}

/// Render messages as the OpenAI chat format.
fn messages_to_json(messages: &[ProviderMessage], names: Option<&ToolNameMap>) -> Value {
    Value::Array(
        messages
            .iter()
            .map(|message| {
                let mut object = Map::new();
                object.insert("role".into(), json!(message.role.as_str()));
                object.insert("content".into(), json!(message.content));
                if let Some(name) = &message.name {
                    // A tool result's author is the tool: spelled like it.
                    let name = if message.role == Role::Tool {
                        wire_name(name, names)
                    } else {
                        name.clone()
                    };
                    object.insert("name".into(), json!(name));
                }
                if let Some(tool_call_id) = &message.tool_call_id {
                    object.insert("tool_call_id".into(), json!(tool_call_id));
                }
                if message.role == Role::Assistant && !message.tool_calls.is_empty() {
                    object.insert(
                        "tool_calls".into(),
                        Value::Array(
                            message
                                .tool_calls
                                .iter()
                                .map(|call| {
                                    json!({
                                        "id": call.id,
                                        "type": "function",
                                        "function": {
                                            "name": wire_name(&call.name, names),
                                            "arguments": call.arguments,
                                        },
                                    })
                                })
                                .collect(),
                        ),
                    );
                }
                Value::Object(object)
            })
            .collect(),
    )
}

/// Render tool schemas as the OpenAI `tools` array.
fn tools_to_json(tools: &[lightagent_core::ToolSchema], names: Option<&ToolNameMap>) -> Value {
    Value::Array(
        tools
            .iter()
            .map(|tool| {
                let parameters = if tool.parameters.is_null() {
                    json!({ "type": "object", "properties": {} })
                } else {
                    tool.parameters.clone()
                };
                json!({
                    "type": "function",
                    "function": {
                        "name": wire_name(&tool.name, names),
                        "description": tool.description,
                        "parameters": parameters,
                    },
                })
            })
            .collect(),
    )
}

#[async_trait]
impl AgentProvider for LightweightProvider {
    async fn stream(
        &self,
        request: ProviderRequest,
        cancel: CancellationToken,
    ) -> Result<ProviderStream, ProviderError> {
        let mut config = self.config.clone();
        config.model = self.resolve_model().await?;
        // An ambiguous mapping is refused here, before anything is sent.
        let names = name_map(&config, &request)?;
        let body = body_with(&config, &request, names.as_ref());
        let mut builder = self
            .client
            .post(format!("{}/v1/chat/completions", self.base()))
            .json(&body);
        if let Some(key) = &self.config.api_key {
            builder = builder.bearer_auth(key);
        }

        let response = builder
            .send()
            .await
            .map_err(|err| ProviderError::Transport(err.to_string()))?;

        let status = response.status();
        if !status.is_success() {
            // A refusal before any stream bytes: an ordinary error, not a
            // truncated stream.
            let detail = response.text().await.unwrap_or_default();
            return Err(ProviderError::Upstream(format!(
                "the provider answered {status}: {}",
                detail.trim()
            )));
        }

        Ok(translate(response.bytes_stream().boxed(), cancel, names))
    }
}

/// State carried while translating one response stream.
struct Translation {
    body: BoxStream<'static, reqwest::Result<Bytes>>,
    decoder: SseDecoder,
    pending: VecDeque<Result<ProviderEvent, ProviderError>>,
    cancel: CancellationToken,
    role_seen: bool,
    finish_reason: Option<FinishReason>,
    /// Whether a `[DONE]` terminator arrived.
    done: bool,
    usage: Option<Usage>,
    finished: bool,
    emitted_finish: bool,
    /// The request's tool-name map, to turn returned names back into
    /// internal ones; `None` when names were sent unchanged.
    names: Option<ToolNameMap>,
}

/// Translate an SSE byte stream into a stream of [`ProviderEvent`]s.
fn translate(
    body: BoxStream<'static, reqwest::Result<Bytes>>,
    cancel: CancellationToken,
    names: Option<ToolNameMap>,
) -> ProviderStream {
    let state = Translation {
        body,
        decoder: SseDecoder::new(),
        pending: VecDeque::new(),
        cancel,
        role_seen: false,
        finish_reason: None,
        done: false,
        usage: None,
        finished: false,
        emitted_finish: false,
        names,
    };

    stream::unfold(state, |mut state| async move {
        loop {
            if let Some(event) = state.pending.pop_front() {
                return Some((event, state));
            }
            if state.finished {
                return None;
            }

            let chunk = tokio::select! {
                biased;
                () = state.cancel.cancelled() => return None,
                chunk = state.body.next() => chunk,
            };

            match chunk {
                Some(Ok(bytes)) => {
                    if let Err(err) = state.decoder.feed(&bytes) {
                        state.finished = true;
                        state.pending.push_back(Err(map_decode_error(err)));
                        continue;
                    }
                    while let Some(event) = state.decoder.next_event() {
                        absorb(&mut state, &event);
                    }
                }
                Some(Err(err)) => {
                    state.finished = true;
                    state
                        .pending
                        .push_back(Err(ProviderError::Stream(err.to_string())));
                }
                None => {
                    // End of body. Emit a terminal Finished if the stream ended
                    // without a `[DONE]`, so the loop always sees one; without
                    // a finish reason either, it says the turn is incomplete.
                    state.finished = true;
                    if !state.emitted_finish {
                        let event = finish_event(&mut state);
                        state.pending.push_back(Ok(event));
                    }
                }
            }
        }
    })
    .boxed()
}

/// Fold one decoded SSE event into the translation state.
fn absorb(state: &mut Translation, event: &SseEvent) {
    if event.is_done() {
        state.done = true;
        state.finished = true;
        if !state.emitted_finish {
            let event = finish_event(state);
            state.pending.push_back(Ok(event));
        }
        return;
    }

    let Ok(chunk) = serde_json::from_str::<ChatChunk>(&event.data) else {
        // Not a chunk we recognize; a stray proxy line must not lose the
        // stream.
        return;
    };

    if let Some(error) = &chunk.error {
        // The terminal error chunk: surface a Finished{Error} and then the
        // error itself, then end.
        let message = if error.message.is_empty() {
            "the provider reported an error".to_owned()
        } else {
            error.message.clone()
        };
        state.finish_reason = Some(FinishReason::Error);
        state.emitted_finish = true;
        state.finished = true;
        state.pending.push_back(Ok(ProviderEvent::Finished {
            reason: FinishReason::Error,
            usage: state.usage,
        }));
        state
            .pending
            .push_back(Err(ProviderError::MidStream { message }));
        return;
    }

    if let Some(usage) = &chunk.usage {
        state.usage = Some(Usage {
            prompt_tokens: usage.prompt_tokens,
            completion_tokens: usage.completion_tokens,
            total_tokens: usage.total_tokens,
        });
    }

    for choice in &chunk.choices {
        let delta = &choice.delta;
        if delta.role.is_some() && !state.role_seen {
            state.role_seen = true;
            state.pending.push_back(Ok(ProviderEvent::RoleStarted));
        }
        if let Some(text) = delta.reasoning_text()
            && !text.is_empty()
        {
            state
                .pending
                .push_back(Ok(ProviderEvent::Reasoning(text.to_owned())));
        }
        if let Some(content) = &delta.content
            && !content.is_empty()
        {
            state
                .pending
                .push_back(Ok(ProviderEvent::Content(content.clone())));
        }
        for call in delta.tool_calls.iter().flatten() {
            let function = call.function.as_ref();
            // A returned name is turned back into the internal name it was
            // declared as, or into one no tool has.
            let name = function
                .and_then(|function| function.name.clone())
                .filter(|name| !name.is_empty())
                .map(|name| match &state.names {
                    Some(names) => names.inbound(&name),
                    None => name,
                });
            state.pending.push_back(Ok(ProviderEvent::ToolCallDelta {
                index: call.index,
                id: call.id.clone().filter(|id| !id.is_empty()),
                name,
                arguments: function.and_then(|function| function.arguments.clone()),
            }));
        }
        if let Some(reason) = &choice.finish_reason {
            state.finish_reason = Some(match reason.as_str() {
                "tool_calls" => FinishReason::ToolCalls,
                "length" => FinishReason::Length,
                "error" => FinishReason::Error,
                _ => FinishReason::Stop,
            });
        }
    }
}

/// Build the terminal Finished event from whatever the stream established.
fn finish_event(state: &mut Translation) -> ProviderEvent {
    state.emitted_finish = true;
    let reason = match state.finish_reason {
        Some(reason) => reason,
        // `[DONE]` without a reason still ends the turn deliberately.
        None if state.done => FinishReason::Stop,
        // The body ended with no completion signal at all.
        None => FinishReason::Incomplete,
    };
    ProviderEvent::Finished {
        reason,
        usage: state.usage,
    }
}

fn map_decode_error(err: SseDecodeError) -> ProviderError {
    match err {
        SseDecodeError::FrameTooLarge { limit } => ProviderError::FrameTooLarge { limit },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightagent_core::ToolSchema;

    #[test]
    fn client_builds_without_panicking() {
        let provider =
            LightweightProvider::new(ProviderConfig::new("http://127.0.0.1:11434", "m@8k"));
        assert!(provider.is_ok());
    }

    #[test]
    fn request_body_shape() {
        let config = ProviderConfig::new("http://127.0.0.1:11434", "lfm2@8k");
        let mut request = ProviderRequest::new(
            "lfm2@8k",
            vec![
                ProviderMessage::system("be brief"),
                ProviderMessage::user("hi"),
            ],
        );
        request.tools = vec![ToolSchema::new(
            "datetime.now",
            "current time",
            json!({"type":"object","properties":{}}),
        )];

        let body = build_body(&config, &request);
        assert_eq!(body["stream"], json!(true));
        assert_eq!(body["stream_options"]["include_usage"], json!(true));
        assert_eq!(body["model"], "lfm2@8k");
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][1]["content"], "hi");
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tools"][0]["function"]["name"], "datetime.now");
    }

    #[test]
    fn a_tool_message_carries_its_call_id() {
        let config = ProviderConfig::new("http://127.0.0.1:11434", "m");
        let request = ProviderRequest::new("m", vec![ProviderMessage::tool_result("call_1", "42")]);
        let body = build_body(&config, &request);
        assert_eq!(body["messages"][0]["role"], "tool");
        assert_eq!(body["messages"][0]["tool_call_id"], "call_1");
    }

    fn frames(frames: &[&str]) -> BoxStream<'static, reqwest::Result<Bytes>> {
        let chunks: Vec<reqwest::Result<Bytes>> = frames
            .iter()
            .map(|frame| Ok(Bytes::from(format!("data: {frame}\n\n"))))
            .collect();
        stream::iter(chunks).boxed()
    }

    async fn decode(frames_in: &[&str]) -> Vec<Result<ProviderEvent, ProviderError>> {
        translate(frames(frames_in), CancellationToken::new(), None)
            .collect()
            .await
    }

    fn finish_of(events: &[Result<ProviderEvent, ProviderError>]) -> Vec<FinishReason> {
        events
            .iter()
            .filter_map(|event| match event {
                Ok(ProviderEvent::Finished { reason, .. }) => Some(*reason),
                _ => None,
            })
            .collect()
    }

    const ROLE: &str = r#"{"choices":[{"index":0,"delta":{"role":"assistant"}}]}"#;
    const HALF: &str = r#"{"choices":[{"index":0,"delta":{"content":"The answer is"}}]}"#;

    #[tokio::test]
    async fn a_finish_reason_then_eof_is_a_valid_stop() {
        let stop = r#"{"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#;
        let events = decode(&[ROLE, HALF, stop]).await;
        assert_eq!(finish_of(&events), [FinishReason::Stop]);
    }

    #[tokio::test]
    async fn a_finish_reason_and_done_finish_once() {
        let stop = r#"{"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#;
        let events = decode(&[ROLE, HALF, stop, "[DONE]"]).await;
        assert_eq!(finish_of(&events), [FinishReason::Stop]);
    }

    #[tokio::test]
    async fn done_without_a_reason_is_a_deliberate_stop() {
        let events = decode(&[ROLE, HALF, "[DONE]"]).await;
        assert_eq!(finish_of(&events), [FinishReason::Stop]);
    }

    #[tokio::test]
    async fn eof_without_any_completion_signal_is_incomplete() {
        let events = decode(&[ROLE, HALF]).await;
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Ok(ProviderEvent::Content(text)) if text == "The answer is")),
            "the partial text is still delivered"
        );
        assert_eq!(finish_of(&events), [FinishReason::Incomplete]);
    }

    #[tokio::test]
    async fn eof_inside_tool_call_arguments_is_incomplete() {
        let call = r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"rag.realtime","arguments":"{\"query\":\"wh"}}]}}]}"#;
        let events = decode(&[ROLE, call]).await;
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Ok(ProviderEvent::ToolCallDelta { .. })))
        );
        assert_eq!(finish_of(&events), [FinishReason::Incomplete]);
    }

    #[tokio::test]
    async fn a_length_finish_is_reported_as_length() {
        let length = r#"{"choices":[{"index":0,"delta":{},"finish_reason":"length"}]}"#;
        let events = decode(&[ROLE, HALF, length, "[DONE]"]).await;
        assert_eq!(finish_of(&events), [FinishReason::Length]);
    }

    #[tokio::test]
    async fn a_broken_body_after_text_is_a_stream_error() {
        // Building a client needs the crypto provider; an invalid URL is the
        // cheapest offline way to get a reqwest::Error.
        tls::ensure_provider();
        let failing = Err::<Bytes, _>(
            reqwest::Client::new()
                .get("not a url")
                .build()
                .expect_err("an invalid URL fails to build"),
        );
        let body =
            stream::iter(vec![Ok(Bytes::from(format!("data: {HALF}\n\n"))), failing]).boxed();
        let events: Vec<_> = translate(body, CancellationToken::new(), None)
            .collect()
            .await;
        assert!(matches!(
            events.first(),
            Some(Ok(ProviderEvent::Content(_)))
        ));
        assert!(matches!(events.last(), Some(Err(ProviderError::Stream(_)))));
        assert!(finish_of(&events).is_empty());
    }

    fn portable_request() -> (ProviderConfig, ProviderRequest) {
        let config = ProviderConfig::new("http://127.0.0.1:11434", "m")
            .with_tool_names(lightagent_core::ToolNameMode::Portable);
        let assistant = ProviderMessage::assistant_tool_calls(
            "",
            vec![lightagent_core::provider::ProviderToolCall {
                id: "call_1".into(),
                name: "web.search".into(),
                arguments: "{}".into(),
            }],
        );
        let mut result = ProviderMessage::tool_result("call_1", "results");
        result.name = Some("web.search".into());
        let mut request = ProviderRequest::new(
            "m",
            vec![ProviderMessage::user("find it"), assistant, result],
        );
        request.tools = vec![
            ToolSchema::new("web.search", "search", json!({"type": "object"})),
            ToolSchema::new("agent.resume_worker", "resume", json!({"type": "object"})),
        ];
        (config, request)
    }

    #[test]
    fn native_names_are_sent_unchanged() {
        let (mut config, request) = portable_request();
        config.tool_names = lightagent_core::ToolNameMode::Native;
        assert_eq!(name_map(&config, &request).unwrap(), None);
        let body = body_with(&config, &request, None);
        assert_eq!(body["tools"][0]["function"]["name"], "web.search");
    }

    #[test]
    fn portable_names_cover_declarations_and_history_alike() {
        let (config, request) = portable_request();
        let names = name_map(&config, &request).unwrap().unwrap();
        let body = body_with(&config, &request, Some(&names));
        let declared: Vec<&str> = body["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["function"]["name"].as_str().unwrap())
            .collect();
        assert_eq!(declared, ["web__search", "agent__resume_worker"]);
        // The earlier call and its result replay under the same spelling,
        // so a follow-up or resumed request stays consistent.
        assert_eq!(
            body["messages"][1]["tool_calls"][0]["function"]["name"],
            "web__search"
        );
        assert_eq!(body["messages"][2]["name"], "web__search");
        for name in declared {
            assert!(crate::names::is_portable(name), "{name}");
        }
    }

    #[test]
    fn an_ambiguous_mapping_is_refused_before_sending() {
        // Two names that differ only in characters outside the alphabet,
        // with a hash collision forced by a third, already-portable name
        // equal to one's hashed spelling, cannot be told apart.
        let (config, mut request) = portable_request();
        let hashed = {
            let map = crate::names::ToolNameMap::portable(["x y", "x?y"]).unwrap();
            map.outbound("x y")
        };
        request.tools = vec![
            ToolSchema::new("x y", "a", json!({"type": "object"})),
            ToolSchema::new("x?y", "b", json!({"type": "object"})),
            ToolSchema::new(hashed.as_str(), "c", json!({"type": "object"})),
        ];
        request.messages.truncate(1);
        assert!(matches!(
            name_map(&config, &request),
            Err(ProviderError::Upstream(_))
        ));
    }

    #[tokio::test]
    async fn streamed_calls_come_back_under_their_internal_names() {
        let (config, request) = portable_request();
        let names = name_map(&config, &request).unwrap();
        // The name arrives once, on the first delta; arguments stream on.
        let first = r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_2","type":"function","function":{"name":"agent__resume_worker","arguments":"{\"han"}}]}}]}"#;
        let rest = r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"dle\":\"wk-1\"}"}}]}}]}"#;
        let invented = r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":1,"id":"call_3","type":"function","function":{"name":"web.search","arguments":"{}"}}]}}]}"#;
        let done = r#"{"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#;
        let events: Vec<_> = translate(
            frames(&[ROLE, first, rest, invented, done]),
            CancellationToken::new(),
            names,
        )
        .collect()
        .await;
        let named: Vec<(u32, String)> = events
            .iter()
            .filter_map(|event| match event {
                Ok(ProviderEvent::ToolCallDelta {
                    index,
                    name: Some(name),
                    ..
                }) => Some((*index, name.clone())),
                _ => None,
            })
            .collect();
        // The approval resume is executed and audited as itself; the
        // internal spelling the model was never given is not accepted.
        assert_eq!(
            named,
            [
                (0, "agent.resume_worker".to_owned()),
                (1, "undeclared:web.search".to_owned())
            ]
        );
        assert_eq!(finish_of(&events), [FinishReason::ToolCalls]);
    }
}
