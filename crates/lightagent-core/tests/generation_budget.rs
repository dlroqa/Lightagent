//! A run's output and context budget, end to end through the loop with a
//! scripted provider. These are deterministic checks of what is requested
//! and what the loop does with the answer; they say nothing about how well
//! any real model uses the room it is given.

use async_trait::async_trait;
use lightagent_core::provider::{FinishReason, ProviderEvent, ProviderMessage, Role, Usage};
use lightagent_core::{
    AgentEvent, AgentLoop, ApprovalNeed, ContextWindow, GenerationBudget, GenerationReport,
    MockProvider, Reasoning, RunConfig, StopReason, ToolCall, ToolInvoker, ToolOutcome,
    ToolSchema, WindowSource,
};
use tokio_util::sync::CancellationToken;

/// One auto-approved retrieval tool whose every result is `result`.
struct Retrieval {
    result: String,
}

#[async_trait]
impl ToolInvoker for Retrieval {
    fn schemas(&self) -> Vec<ToolSchema> {
        vec![ToolSchema::new(
            "rag.search",
            "find passages",
            serde_json::json!({"type": "object"}),
        )]
    }

    fn approval_for(&self, _call: &ToolCall) -> ApprovalNeed {
        ApprovalNeed::AutoApprove
    }

    async fn invoke(&self, _call: &ToolCall, _cancel: CancellationToken) -> ToolOutcome {
        ToolOutcome::ok(self.result.clone())
    }
}

fn usage(prompt: u32, completion: u32) -> Option<Usage> {
    Some(Usage {
        prompt_tokens: prompt,
        completion_tokens: completion,
        total_tokens: prompt + completion,
    })
}

fn search(id: &str, usage: Option<Usage>) -> Vec<ProviderEvent> {
    vec![
        ProviderEvent::RoleStarted,
        ProviderEvent::ToolCallDelta {
            index: 0,
            id: Some(id.to_owned()),
            name: Some("rag.search".into()),
            arguments: Some(format!("{{\"q\":\"{id}\"}}")),
        },
        ProviderEvent::Finished {
            reason: FinishReason::ToolCalls,
            usage,
        },
    ]
}

fn answer(text: &str, reason: FinishReason, usage: Option<Usage>) -> Vec<ProviderEvent> {
    vec![
        ProviderEvent::RoleStarted,
        ProviderEvent::Content(text.to_owned()),
        ProviderEvent::Finished { reason, usage },
    ]
}

fn window(tokens: u32) -> Option<ContextWindow> {
    Some(ContextWindow {
        tokens,
        source: WindowSource::Provider,
    })
}

fn config(generation: GenerationBudget) -> RunConfig {
    let mut config = RunConfig::new("m");
    config.limits.wall_clock_secs = None;
    config.generation = generation;
    config
}

async fn run(
    provider: MockProvider,
    invoker: Retrieval,
    generation: GenerationBudget,
    history: Vec<ProviderMessage>,
) -> Vec<AgentEvent> {
    AgentLoop::new(provider, invoker, config(generation))
        .run_with_history(history, "research this", CancellationToken::new())
        .await
        .expect("run")
        .into_events()
}

fn stop_reason(events: &[AgentEvent]) -> Option<StopReason> {
    match events.last() {
        Some(AgentEvent::RunCompleted { reason }) => Some(*reason),
        _ => None,
    }
}

fn reports(events: &[AgentEvent]) -> Vec<GenerationReport> {
    events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::GenerationReport { report } => Some(report.as_ref().clone()),
            _ => None,
        })
        .collect()
}

fn content(events: &[AgentEvent]) -> String {
    events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::Content { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn small() -> Retrieval {
    Retrieval {
        result: "passage one".into(),
    }
}

#[tokio::test]
async fn the_default_budget_leaves_every_request_and_event_as_before() {
    let mock = MockProvider::new(vec![
        search("call_1", None),
        answer("done", FinishReason::Stop, None),
    ]);
    let inspector = mock.clone();
    let events = run(mock, small(), GenerationBudget::default(), Vec::new()).await;
    for request in inspector.requests() {
        assert_eq!(request.max_tokens, None);
        assert_eq!(request.reasoning_effort, None);
    }
    assert!(reports(&events).is_empty(), "no report without a budget");
    assert_eq!(stop_reason(&events), Some(StopReason::EndTurn));
}

#[tokio::test]
async fn the_allowance_and_a_supported_reasoning_label_ride_on_every_turn() {
    let mock = MockProvider::new(vec![
        search("call_1", usage(120, 9)),
        answer("done", FinishReason::Stop, usage(160, 4)),
    ]);
    let inspector = mock.clone();
    let budget = GenerationBudget {
        context_window: window(8_192),
        max_output_tokens: Some(1_024),
        reserve_answer_tokens: 512,
        reasoning: Reasoning::resolve(Some("low"), &["none".into(), "low".into()]),
    };
    let events = run(mock, small(), budget, Vec::new()).await;
    let requests = inspector.requests();
    assert_eq!(requests.len(), 2);
    for request in &requests {
        assert_eq!(request.max_tokens, Some(1_024));
        assert_eq!(request.reasoning_effort.as_deref(), Some("low"));
    }
    let reports = reports(&events);
    assert_eq!(reports.len(), 2, "one report per turn");
    assert_eq!(reports[0].prompt_basis, "estimated");
    assert_eq!(
        reports[1].prompt_basis, "measured",
        "the first turn's count replaces the estimate of what it covered"
    );
    assert!(reports[1].prompt_estimate > 120);
    assert_eq!(reports[1].reasoning, "applied");
    assert_eq!(reports[1].requested_max_tokens, Some(1_024));
    assert_eq!(reports[1].context_window, Some(8_192));
    assert_eq!(reports[1].context_source.as_deref(), Some("provider"));
    assert_eq!(reports[1].finish, "stop");
    assert_eq!(reports[1].completion_tokens, Some(4));
    assert_eq!(stop_reason(&events), Some(StopReason::EndTurn));
}

#[tokio::test]
async fn an_unsupported_reasoning_label_is_reported_and_never_sent() {
    let mock = MockProvider::new(vec![answer("done", FinishReason::Stop, None)]);
    let inspector = mock.clone();
    let budget = GenerationBudget {
        reasoning: Reasoning::resolve(Some("high"), &["none".into()]),
        ..GenerationBudget::default()
    };
    let events = run(mock, small(), budget, Vec::new()).await;
    assert_eq!(inspector.requests()[0].reasoning_effort, None);
    let report = &reports(&events)[0];
    assert_eq!(report.reasoning, "unsupported");
    assert_eq!(report.reasoning_effort.as_deref(), Some("high"));
    assert_eq!(report.context_window, None, "an unknown window is reported as such");
    assert_eq!(report.context_source, None);
}

#[tokio::test]
async fn a_result_too_large_for_the_window_is_shortened_and_the_model_answers() {
    let huge = "evidence ".repeat(8_000);
    let mock = MockProvider::new(vec![
        search("call_1", None),
        answer("answer from the passage", FinishReason::Stop, None),
    ]);
    let inspector = mock.clone();
    let budget = GenerationBudget {
        context_window: window(2_048),
        reserve_answer_tokens: 512,
        ..GenerationBudget::default()
    };
    let invoker = Retrieval {
        result: huge.clone(),
    };
    let events = run(mock, invoker, budget, Vec::new()).await;

    // The run's own record keeps the whole result.
    let recorded = events.iter().find_map(|event| match event {
        AgentEvent::ToolCallCompleted { outcome, .. } => Some(outcome.content.clone()),
        _ => None,
    });
    assert_eq!(recorded.as_deref(), Some(huge.as_str()));

    let requests = inspector.requests();
    assert_eq!(requests.len(), 2);
    let seen = requests[1]
        .messages
        .iter()
        .find(|message| message.role == Role::Tool)
        .expect("the tool result");
    assert!(seen.content.len() < huge.len() / 4);
    assert!(seen.content.contains("shortened this result"));
    // The window is at its reserve, so the next turn is the answer.
    assert!(requests[1].tools.is_empty(), "no more tools are offered");
    let said = requests[1].messages.last().map(|m| m.content.as_str());
    assert!(said.is_some_and(|said| said.contains("context window")));
    assert_eq!(content(&events), "answer from the passage");
    assert_eq!(stop_reason(&events), Some(StopReason::ContextLimit));
    assert!(!StopReason::ContextLimit.is_complete());
    let last = reports(&events).pop().expect("a report");
    assert_eq!(last.purpose, "answer");
    assert_eq!(last.shortened_results, 1);
}

#[tokio::test]
async fn long_history_with_results_leaves_no_room_so_tools_stop() {
    // A long restored conversation plus small results: nothing is shortened,
    // but the room left drops below the reserve before the next turn.
    let history = vec![
        ProviderMessage::user("earlier question"),
        ProviderMessage::assistant("x".repeat(4_800)),
    ];
    let mock = MockProvider::new(vec![
        search("call_1", None),
        answer("short answer", FinishReason::Stop, None),
    ]);
    let inspector = mock.clone();
    let budget = GenerationBudget {
        context_window: window(2_048),
        reserve_answer_tokens: 512,
        ..GenerationBudget::default()
    };
    let events = run(mock, small(), budget, history).await;
    let requests = inspector.requests();
    assert!(
        !requests[0].tools.is_empty(),
        "without tool results there is nothing to answer from yet"
    );
    assert!(requests[1].tools.is_empty());
    assert_eq!(reports(&events)[0].shortened_results, 0);
    assert_eq!(stop_reason(&events), Some(StopReason::ContextLimit));
}

#[tokio::test]
async fn a_window_with_room_to_spare_changes_nothing_about_tool_use() {
    let mock = MockProvider::new(vec![
        search("call_1", None),
        search("call_2", None),
        answer("done", FinishReason::Stop, None),
    ]);
    let inspector = mock.clone();
    let budget = GenerationBudget {
        context_window: window(32_768),
        reserve_answer_tokens: 1_024,
        ..GenerationBudget::default()
    };
    let events = run(mock, small(), budget, Vec::new()).await;
    let requests = inspector.requests();
    assert_eq!(requests.len(), 3);
    assert!(requests.iter().all(|request| !request.tools.is_empty()));
    assert!(requests.iter().all(|request| request.max_tokens.is_none()));
    assert_eq!(stop_reason(&events), Some(StopReason::EndTurn));
}

#[tokio::test]
async fn a_length_stop_with_only_reasoning_keeps_nothing_it_did_not_receive() {
    let mock = MockProvider::new(vec![vec![
        ProviderEvent::RoleStarted,
        ProviderEvent::Reasoning("thinking about it".into()),
        ProviderEvent::Finished {
            reason: FinishReason::Length,
            usage: usage(200, 512),
        },
    ]]);
    let budget = GenerationBudget {
        context_window: window(8_192),
        max_output_tokens: Some(512),
        reserve_answer_tokens: 1_024,
        ..GenerationBudget::default()
    };
    let events = run(mock, small(), budget, Vec::new()).await;
    assert_eq!(content(&events), "", "no visible text arrived");
    assert_eq!(stop_reason(&events), Some(StopReason::MaxTokens));
    let report = &reports(&events)[0];
    assert_eq!(report.finish, "length");
    assert_eq!(report.length_cause.as_deref(), Some("output_allowance"));
}

#[tokio::test]
async fn a_length_stop_that_filled_the_window_is_attributed_to_it() {
    let mock = MockProvider::new(vec![answer(
        "The first half of",
        FinishReason::Length,
        usage(7_000, 1_191),
    )]);
    let budget = GenerationBudget {
        context_window: window(8_192),
        ..GenerationBudget::default()
    };
    let events = run(mock, small(), budget, Vec::new()).await;
    assert_eq!(content(&events), "The first half of");
    assert_eq!(stop_reason(&events), Some(StopReason::MaxTokens));
    assert_eq!(
        reports(&events)[0].length_cause.as_deref(),
        Some("context_window")
    );
}

#[tokio::test]
async fn a_length_stop_inside_tool_arguments_never_runs_the_call() {
    let truncated = vec![
        ProviderEvent::RoleStarted,
        ProviderEvent::ToolCallDelta {
            index: 0,
            id: Some("call_1".into()),
            name: Some("rag.search".into()),
            arguments: Some("{\"q\":\"unfin".into()),
        },
        ProviderEvent::Finished {
            reason: FinishReason::Length,
            usage: usage(300, 64),
        },
    ];
    let budget = GenerationBudget {
        context_window: window(8_192),
        max_output_tokens: Some(64),
        ..GenerationBudget::default()
    };
    let events = run(MockProvider::new(vec![truncated]), small(), budget, Vec::new()).await;
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::ToolCallStarted { .. })),
        "a possibly truncated call never runs"
    );
    assert_eq!(stop_reason(&events), Some(StopReason::MaxTokens));
}
