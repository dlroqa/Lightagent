//! A run's output and context budget, end to end through the loop with a
//! scripted provider. These are deterministic checks of what is requested
//! and what the loop does with the answer; they say nothing about how well
//! any real model uses the room it is given.

use async_trait::async_trait;
use lightagent_core::provider::{FinishReason, ProviderEvent, ProviderMessage, Role, Usage};
use lightagent_core::{
    AgentEvent, AgentLoop, AgentProvider, ApprovalNeed, ContextWindow, GenerationBudget,
    GenerationReport, MockProvider, OutputPool, Reasoning, RunConfig, StopReason, ToolCall,
    ToolInvoker, ToolOutcome, ToolSchema, WindowSource,
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
        output_pool: None,
        max_continuations: 0,
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
    assert_eq!(
        report.context_window, None,
        "an unknown window is reported as such"
    );
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
    let events = run(
        MockProvider::new(vec![truncated]),
        small(),
        budget,
        Vec::new(),
    )
    .await;
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::ToolCallStarted { .. })),
        "a possibly truncated call never runs"
    );
    assert_eq!(stop_reason(&events), Some(StopReason::MaxTokens));
}

/// A budget with an 8k window, a 1024-token ceiling and a run-wide pool.
fn pooled(total: u32) -> GenerationBudget {
    GenerationBudget {
        context_window: window(8_192),
        max_output_tokens: Some(1_024),
        reserve_answer_tokens: 512,
        output_pool: Some(OutputPool::new(total)),
        ..GenerationBudget::default()
    }
}

/// A budget that continues a cut-off answer up to `times` times.
fn continuing(times: u32) -> GenerationBudget {
    GenerationBudget {
        context_window: window(8_192),
        reserve_answer_tokens: 1_024,
        max_continuations: times,
        ..GenerationBudget::default()
    }
}

#[tokio::test]
async fn the_run_pool_caps_each_request_and_ends_research_when_low() {
    let mock = MockProvider::new(vec![
        search("call_1", usage(100, 50)),
        answer("brief answer", FinishReason::Stop, usage(200, 30)),
    ]);
    let inspector = mock.clone();
    let budget = pooled(700);
    let pool = budget.output_pool.clone().expect("pool");
    let events = run(mock, small(), budget, Vec::new()).await;
    let requests = inspector.requests();
    assert_eq!(
        requests[0].max_tokens,
        Some(700),
        "never more than the pool"
    );
    // 650 left is above the 512 reserve: an ordinary turn, granted the rest.
    assert_eq!(requests[1].max_tokens, Some(650));
    assert_eq!(pool.spent(), 80, "settled against reported usage");
    assert_eq!(stop_reason(&events), Some(StopReason::EndTurn));
    let last = reports(&events).pop().expect("a report");
    assert_eq!(last.configured_max_tokens, Some(1_024));
    assert_eq!(last.requested_max_tokens, Some(650));
    assert_eq!(
        (last.output_budget, last.output_spent),
        (Some(700), Some(80))
    );
    assert!(!last.output_estimated);
}

#[tokio::test]
async fn a_low_pool_turns_the_next_turn_into_the_answer() {
    let mock = MockProvider::new(vec![
        search("call_1", usage(100, 300)),
        answer("brief answer", FinishReason::Stop, usage(200, 30)),
    ]);
    let inspector = mock.clone();
    let events = run(mock, small(), pooled(700), Vec::new()).await;
    let requests = inspector.requests();
    // 400 left is below the 512 reserve: tools are withheld and the answer
    // gets what remains.
    assert!(requests[1].tools.is_empty());
    assert_eq!(requests[1].max_tokens, Some(400));
    let said = requests[1].messages.last().map(|m| m.content.as_str());
    assert!(said.is_some_and(|said| said.contains("output budget")));
    assert_eq!(content(&events), "brief answer");
    assert_eq!(stop_reason(&events), Some(StopReason::OutputBudget));
}

#[tokio::test]
async fn a_spent_pool_takes_no_turn_and_missing_usage_is_charged_in_full() {
    let mock = MockProvider::new(vec![answer("x", FinishReason::Stop, None)]);
    let inspector = mock.clone();
    let events = run(mock, small(), pooled(10), Vec::new()).await;
    assert!(inspector.requests().is_empty(), "nothing left to grant");
    assert_eq!(stop_reason(&events), Some(StopReason::OutputBudget));

    let mock = MockProvider::new(vec![
        search("call_1", None),
        answer("done", FinishReason::Stop, None),
    ]);
    let budget = pooled(5_000);
    let pool = budget.output_pool.clone().expect("pool");
    let events = run(mock, small(), budget, Vec::new()).await;
    let first = &reports(&events)[0];
    assert_eq!(first.output_spent, Some(1_024), "the whole reservation");
    assert!(first.output_estimated, "and said to be an estimate");
    assert_eq!(pool.spent(), 2_048);
}

#[tokio::test]
async fn runs_sharing_a_pool_never_together_exceed_it() {
    // As a lead's workers do: every budget derived from the lead's shares
    // its pool, and they may run at the same time.
    // No usage is reported, so every reservation is charged in full and the
    // bound holds however the runs interleave.
    let lead = pooled(1_500);
    let pool = lead.output_pool.clone().expect("pool");
    let mocks: Vec<MockProvider> = (0..4)
        .map(|_| MockProvider::looping(vec![answer("part ", FinishReason::Length, None)]))
        .collect();
    let runs = mocks
        .iter()
        .map(|mock| run(mock.clone(), small(), lead.for_worker(true), Vec::new()));
    let outcomes = futures_util::future::join_all(runs).await;
    let granted: u32 = mocks
        .iter()
        .flat_map(MockProvider::requests)
        .filter_map(|request| request.max_tokens)
        .sum();
    assert_eq!(granted, 1_500, "the whole pool and no more was granted");
    assert_eq!(pool.spent(), 1_500);
    assert!(pool.estimated());
    assert!(
        outcomes
            .iter()
            .any(|events| stop_reason(events) == Some(StopReason::OutputBudget)),
        "some run found the pool spent"
    );
}

/// One model turn asking for `count` searches at once.
fn searches(count: u32) -> Vec<ProviderEvent> {
    let mut turn = vec![ProviderEvent::RoleStarted];
    for index in 0..count {
        turn.push(ProviderEvent::ToolCallDelta {
            index,
            id: Some(format!("call_{index}")),
            name: Some("rag.search".into()),
            arguments: Some(format!("{{\"q\":{index}}}")),
        });
    }
    turn.push(ProviderEvent::Finished {
        reason: FinishReason::ToolCalls,
        usage: None,
    });
    turn
}

#[tokio::test]
async fn shortening_large_results_keeps_every_tool_exchange_paired() {
    let history = vec![
        ProviderMessage::user("earlier"),
        ProviderMessage::assistant("y".repeat(3_000)),
    ];
    let mock = MockProvider::new(vec![
        searches(3),
        answer("answer", FinishReason::Stop, None),
    ]);
    let inspector = mock.clone();
    let budget = GenerationBudget {
        context_window: window(4_096),
        reserve_answer_tokens: 1_024,
        ..GenerationBudget::default()
    };
    let invoker = Retrieval {
        result: "passage ".repeat(4_000),
    };
    let events = run(mock, invoker, budget, history).await;
    let requests = inspector.requests();
    let answer_turn = &requests[1];
    let calls: Vec<&str> = answer_turn
        .messages
        .iter()
        .flat_map(|message| message.tool_calls.iter().map(|call| call.id.as_str()))
        .collect();
    let results: Vec<&str> = answer_turn
        .messages
        .iter()
        .filter(|message| message.role == Role::Tool)
        .filter_map(|message| message.tool_call_id.as_deref())
        .collect();
    assert_eq!(calls, ["call_0", "call_1", "call_2"]);
    assert_eq!(results, calls, "every call keeps its result, in order");
    assert!(
        answer_turn
            .messages
            .iter()
            .filter(|message| message.role == Role::Tool)
            .all(|message| message.content.contains("shortened this result")),
        "each result was fitted to the room left"
    );
    assert_eq!(reports(&events)[1].shortened_results, 3);
    assert_eq!(stop_reason(&events), Some(StopReason::ContextLimit));
}

#[tokio::test]
async fn a_cut_off_answer_is_continued_by_a_tool_less_turn() {
    let mock = MockProvider::new(vec![
        answer("The bridge opened in", FinishReason::Length, usage(100, 50)),
        answer(" 1932.", FinishReason::Stop, usage(160, 4)),
    ]);
    let inspector = mock.clone();
    let events = run(mock, small(), continuing(1), Vec::new()).await;
    assert_eq!(content(&events), "The bridge opened in 1932.");
    assert_eq!(stop_reason(&events), Some(StopReason::EndTurn));
    let requests = inspector.requests();
    assert_eq!(requests.len(), 2);
    assert!(
        requests[1].tools.is_empty(),
        "a continuation offers no tools"
    );
    let messages = &requests[1].messages;
    let partial = &messages[messages.len() - 2];
    assert_eq!(partial.role, Role::Assistant);
    assert_eq!(partial.content, "The bridge opened in");
    assert!(messages[messages.len() - 1].content.contains("cut off"));
    let reports = reports(&events);
    assert_eq!(reports[0].finish, "length");
    assert_eq!(reports[1].purpose, "continuation");
    assert_eq!(reports[1].continuations, 1);
}

#[tokio::test]
async fn a_restated_ending_is_not_shown_twice() {
    let mock = MockProvider::new(vec![
        answer(
            "Sources agree that the bridge opened in",
            FinishReason::Length,
            None,
        ),
        vec![
            ProviderEvent::RoleStarted,
            ProviderEvent::Content("Sources agree that the ".into()),
            ProviderEvent::Content("bridge opened in 1932.".into()),
            ProviderEvent::Finished {
                reason: FinishReason::Stop,
                usage: None,
            },
        ],
    ]);
    let events = run(mock, small(), continuing(1), Vec::new()).await;
    assert_eq!(
        content(&events),
        "Sources agree that the bridge opened in 1932."
    );
}

#[tokio::test]
async fn continuations_stop_at_their_cap_and_keep_every_part() {
    let mock = MockProvider::looping(vec![answer(
        "more text ",
        FinishReason::Length,
        usage(200, 64),
    )]);
    let inspector = mock.clone();
    let events = run(mock, small(), continuing(2), Vec::new()).await;
    assert_eq!(inspector.requests().len(), 3, "the first turn and two more");
    assert_eq!(content(&events), "more text more text more text ");
    // Still cut off after the last permitted continuation: reported partial.
    assert_eq!(stop_reason(&events), Some(StopReason::MaxTokens));
    assert!(!StopReason::MaxTokens.is_complete());
    let report = reports(&events).pop().expect("a report");
    assert_eq!(
        (report.finish.as_str(), report.continuations),
        ("length", 2)
    );
    // The parts are kept as one answer, so roles still alternate.
    let last = inspector.requests().pop().expect("a request");
    let assistants = last
        .messages
        .iter()
        .filter(|message| message.role == Role::Assistant)
        .count();
    assert_eq!(assistants, 1);
}

#[tokio::test]
async fn nothing_is_continued_that_is_unsafe_or_has_no_room() {
    let truncated_call = vec![
        ProviderEvent::RoleStarted,
        ProviderEvent::Content("Let me look".into()),
        ProviderEvent::ToolCallDelta {
            index: 0,
            id: Some("call_1".into()),
            name: Some("rag.search".into()),
            arguments: Some("{\"q\":".into()),
        },
        ProviderEvent::Finished {
            reason: FinishReason::Length,
            usage: None,
        },
    ];
    let reasoning_only = vec![
        ProviderEvent::RoleStarted,
        ProviderEvent::Reasoning("thinking".into()),
        ProviderEvent::Finished {
            reason: FinishReason::Length,
            usage: None,
        },
    ];
    let full_window = answer("Most of", FinishReason::Length, usage(7_900, 200));
    let cases = [
        ("incomplete tool arguments", truncated_call, continuing(1)),
        ("no visible text", reasoning_only.clone(), continuing(1)),
        ("no room left", full_window, continuing(1)),
        (
            "no known window",
            answer("Half", FinishReason::Length, None),
            GenerationBudget {
                max_continuations: 1,
                ..GenerationBudget::default()
            },
        ),
        ("continuations off", reasoning_only, continuing(0)),
    ];
    for (case, script, budget) in cases {
        let mock = MockProvider::looping(vec![script]);
        let inspector = mock.clone();
        let events = run(mock, small(), budget, Vec::new()).await;
        assert_eq!(inspector.requests().len(), 1, "{case}: continued");
        assert_eq!(stop_reason(&events), Some(StopReason::MaxTokens), "{case}");
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, AgentEvent::ToolCallStarted { .. })),
            "{case}: a tool ran"
        );
    }
}

#[tokio::test]
async fn a_continuation_spends_the_runs_own_turns() {
    let mock = MockProvider::new(vec![
        search("call_1", None),
        answer("The answer is", FinishReason::Length, None),
        answer(" forty-two.", FinishReason::Stop, None),
    ]);
    let inspector = mock.clone();
    let mut generation = continuing(1);
    generation.max_continuations = 3;
    let mut config = config(generation);
    config.limits.max_turns = 3;
    let events = AgentLoop::new(mock, small(), config.clone())
        .run("research this", CancellationToken::new())
        .await
        .expect("run")
        .into_events();
    assert_eq!(inspector.requests().len(), 3);
    assert_eq!(content(&events), "The answer is forty-two.");
    assert_eq!(stop_reason(&events), Some(StopReason::EndTurn));

    // With no turn left after the cut-off answer, it is not continued.
    let mock = MockProvider::looping(vec![answer("Cut", FinishReason::Length, None)]);
    let inspector = mock.clone();
    config.limits.max_turns = 1;
    let events = AgentLoop::new(mock, small(), config)
        .run("research this", CancellationToken::new())
        .await
        .expect("run")
        .into_events();
    assert_eq!(inspector.requests().len(), 1);
    assert_eq!(stop_reason(&events), Some(StopReason::MaxTokens));
}

/// A provider that cancels the run as its second turn is requested.
struct CancelOnSecondTurn {
    inner: MockProvider,
    cancel: CancellationToken,
    turns: std::sync::atomic::AtomicUsize,
}

#[async_trait]
impl AgentProvider for CancelOnSecondTurn {
    async fn stream(
        &self,
        request: lightagent_core::ProviderRequest,
        cancel: CancellationToken,
    ) -> Result<lightagent_core::ProviderStream, lightagent_core::ProviderError> {
        let turn = self.turns.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if turn == 1 {
            self.cancel.cancel();
        }
        self.inner.stream(request, cancel).await
    }
}

#[tokio::test]
async fn a_cancelled_continuation_keeps_the_partial_answer() {
    let cancel = CancellationToken::new();
    let provider = CancelOnSecondTurn {
        inner: MockProvider::looping(vec![answer(
            "The partial answer",
            FinishReason::Length,
            None,
        )]),
        cancel: cancel.clone(),
        turns: std::sync::atomic::AtomicUsize::new(0),
    };
    let events = AgentLoop::new(provider, small(), config(continuing(1)))
        .run("research this", cancel)
        .await
        .expect("run")
        .into_events();
    assert_eq!(content(&events), "The partial answer");
    assert_eq!(stop_reason(&events), Some(StopReason::Cancelled));
}

#[tokio::test]
async fn continuations_obey_the_per_request_ceiling_and_the_run_pool() {
    let mock = MockProvider::looping(vec![answer(
        "chunk ",
        FinishReason::Length,
        usage(100, 256),
    )]);
    let inspector = mock.clone();
    let mut budget = continuing(2);
    budget.max_output_tokens = Some(256);
    budget.output_pool = Some(OutputPool::new(600));
    let pool = budget.output_pool.clone().expect("pool");
    let events = run(mock, small(), budget, Vec::new()).await;
    let requests = inspector.requests();
    // The ceiling applies to every request, continuations included; the
    // pool bounds the run. After two turns 88 tokens remain, too few to
    // continue usefully, so the answer stays cut off.
    assert_eq!(requests.len(), 2);
    assert!(
        requests
            .iter()
            .all(|request| request.max_tokens == Some(256))
    );
    assert_eq!(pool.spent(), 512);
    assert_eq!(stop_reason(&events), Some(StopReason::MaxTokens));
    assert_eq!(content(&events), "chunk chunk ");
}

/// A pool-backed budget with no output ceiling and no known window: only
/// the pool and its per-turn default bound a turn.
fn unbounded(pool: &OutputPool) -> GenerationBudget {
    GenerationBudget {
        output_pool: Some(pool.clone()),
        ..GenerationBudget::default()
    }
}

#[tokio::test]
async fn concurrent_workers_of_unknown_capacity_share_the_pool() {
    // Before: a turn with nothing to bound it asked for everything left, so
    // the first worker held the whole pool and its siblings stopped with
    // OutputBudget. Now each holds the per-turn default.
    let pool = OutputPool::new(10_000);
    let mocks: Vec<MockProvider> = (0..4)
        .map(|_| MockProvider::new(vec![answer("report", FinishReason::Stop, usage(50, 120))]))
        .collect();
    let runs = mocks
        .iter()
        .map(|mock| run(mock.clone(), small(), unbounded(&pool), Vec::new()));
    let outcomes = futures_util::future::join_all(runs).await;
    for (mock, events) in mocks.iter().zip(&outcomes) {
        assert_eq!(
            mock.requests()[0].max_tokens,
            Some(2_048),
            "sent as the ceiling"
        );
        assert_eq!(stop_reason(events), Some(StopReason::EndTurn));
        let report = &reports(events)[0];
        assert_eq!(
            report.reservation_basis.as_deref(),
            Some("unknown_capacity")
        );
        assert_eq!(report.context_window, None, "capacity stays unknown");
    }
    assert_eq!(pool.spent(), 480, "settled against reported usage");
    assert!(!pool.estimated());
}

#[tokio::test]
async fn the_per_turn_default_is_configurable_and_bounded_by_what_is_left() {
    for (pool, expected) in [
        (OutputPool::new(10_000).with_turn_tokens(512), 512),
        (OutputPool::new(300), 300),
    ] {
        let mock = MockProvider::new(vec![answer("a", FinishReason::Stop, usage(10, 5))]);
        let inspector = mock.clone();
        run(mock, small(), unbounded(&pool), Vec::new()).await;
        assert_eq!(inspector.requests()[0].max_tokens, Some(expected));
    }
}

#[tokio::test]
async fn a_known_window_reserves_only_the_room_the_prompt_leaves() {
    let budget = GenerationBudget {
        context_window: window(8_192),
        output_pool: Some(OutputPool::new(100_000)),
        ..GenerationBudget::default()
    };
    let history = vec![
        ProviderMessage::user("q"),
        ProviderMessage::assistant("x".repeat(3_000)),
    ];
    let mock = MockProvider::new(vec![answer("a", FinishReason::Stop, usage(10, 5))]);
    let inspector = mock.clone();
    let events = run(mock, small(), budget, history).await;
    let sent = inspector.requests()[0].max_tokens.expect("a ceiling");
    let report = &reports(&events)[0];
    assert_eq!(
        report.reservation_basis.as_deref(),
        Some("remaining_context")
    );
    // The window less the estimated prompt and the 2% margin, not the window.
    assert_eq!(sent, 8_192 - report.prompt_estimate - 163);
}

#[tokio::test]
async fn an_explicit_or_inherited_ceiling_bounds_the_reservation() {
    let lead = GenerationBudget {
        max_output_tokens: Some(700),
        output_pool: Some(OutputPool::new(10_000)),
        ..GenerationBudget::default()
    };
    // A worker on another model of unknown capacity inherits the ceiling.
    let worker = lead.for_routed_worker(GenerationBudget::default());
    for budget in [lead, worker] {
        let mock = MockProvider::new(vec![answer("a", FinishReason::Stop, usage(10, 5))]);
        let inspector = mock.clone();
        let events = run(mock, small(), budget, Vec::new()).await;
        assert_eq!(inspector.requests()[0].max_tokens, Some(700));
        assert_eq!(
            reports(&events)[0].reservation_basis.as_deref(),
            Some("ceiling")
        );
    }
}

#[tokio::test]
async fn usage_beyond_the_grant_is_recorded_and_stops_further_grants() {
    let pool = OutputPool::new(10_000).with_turn_tokens(256);
    // A provider that did not honour its max_tokens.
    let mock = MockProvider::new(vec![
        search("call_1", usage(100, 900)),
        answer("never", FinishReason::Stop, None),
    ]);
    let inspector = mock.clone();
    let events = run(mock, small(), unbounded(&pool), Vec::new()).await;
    assert_eq!(inspector.requests().len(), 1, "no turn after the overrun");
    assert_eq!((pool.spent(), pool.overrun()), (900, 644));
    assert_eq!(reports(&events)[0].output_overrun, 644);
    assert_eq!(stop_reason(&events), Some(StopReason::OutputBudget));
}

/// A provider whose stream fails after it starts.
struct FailsMidTurn;

#[async_trait]
impl AgentProvider for FailsMidTurn {
    async fn stream(
        &self,
        _: lightagent_core::ProviderRequest,
        _: CancellationToken,
    ) -> Result<lightagent_core::ProviderStream, lightagent_core::ProviderError> {
        Ok(Box::pin(futures_util::stream::iter(vec![
            Ok(ProviderEvent::RoleStarted),
            Err(lightagent_core::ProviderError::Transport(
                "connection reset".into(),
            )),
        ])))
    }
}

#[tokio::test]
async fn reasoning_is_charged_once_and_unaccounted_turns_in_full() {
    // completion_tokens already covers reasoning; the streamed reasoning
    // text is not charged again.
    let pool = OutputPool::new(10_000);
    let thinking = MockProvider::new(vec![vec![
        ProviderEvent::RoleStarted,
        ProviderEvent::Reasoning("r".repeat(3_000)),
        ProviderEvent::Content("answer".into()),
        ProviderEvent::Finished {
            reason: FinishReason::Stop,
            usage: usage(50, 400),
        },
    ]]);
    run(thinking, small(), unbounded(&pool), Vec::new()).await;
    assert_eq!(pool.spent(), 400);
    assert!(!pool.estimated());

    // Missing usage: the whole reservation, labelled estimated.
    let pool = OutputPool::new(10_000);
    let silent = MockProvider::new(vec![answer("a", FinishReason::Stop, None)]);
    run(silent, small(), unbounded(&pool), Vec::new()).await;
    assert_eq!(pool.spent(), 2_048);
    assert!(pool.estimated());

    // A provider error mid-turn: the same.
    let pool = OutputPool::new(10_000);
    let events = AgentLoop::new(FailsMidTurn, small(), config(unbounded(&pool)))
        .run("go", CancellationToken::new())
        .await
        .expect("run")
        .into_events();
    assert_eq!(stop_reason(&events), Some(StopReason::Error));
    assert_eq!(pool.spent(), 2_048);
    assert!(pool.estimated());

    // Cancelled as the turn starts: the same.
    let cancel = CancellationToken::new();
    let pool = OutputPool::new(10_000);
    let provider = CancelOnSecondTurn {
        inner: MockProvider::looping(vec![answer("x", FinishReason::Stop, None)]),
        cancel: cancel.clone(),
        turns: std::sync::atomic::AtomicUsize::new(1),
    };
    let events = AgentLoop::new(provider, small(), config(unbounded(&pool)))
        .run("go", cancel)
        .await
        .expect("run")
        .into_events();
    assert_eq!(stop_reason(&events), Some(StopReason::Cancelled));
    assert_eq!(pool.spent(), 2_048);
}

#[tokio::test]
async fn a_continuation_reserves_from_the_same_pool_by_the_same_policy() {
    let pool = OutputPool::new(50_000);
    let mut budget = continuing(1);
    budget.output_pool = Some(pool.clone());
    let mock = MockProvider::new(vec![
        answer(
            "The bridge opened in",
            FinishReason::Length,
            usage(100, 300),
        ),
        answer(" 1932.", FinishReason::Stop, usage(150, 4)),
    ]);
    let inspector = mock.clone();
    let events = run(mock, small(), budget, Vec::new()).await;
    let reports = reports(&events);
    assert_eq!(reports[1].purpose, "continuation");
    assert_eq!(
        reports[1].reservation_basis.as_deref(),
        Some("remaining_context")
    );
    assert!(
        inspector
            .requests()
            .iter()
            .all(|request| request.max_tokens.is_some_and(|tokens| tokens < 8_192))
    );
    assert_eq!(pool.spent(), 304);
    assert_eq!(stop_reason(&events), Some(StopReason::EndTurn));
}

#[tokio::test]
async fn unknown_capacity_enables_no_context_protection_or_continuation() {
    let pool = OutputPool::new(10_000);
    let mut budget = unbounded(&pool);
    budget.max_continuations = 1;
    budget.reserve_answer_tokens = 1_024;
    let mock = MockProvider::new(vec![
        search("call_1", None),
        answer("half", FinishReason::Length, usage(10, 2_048)),
    ]);
    let inspector = mock.clone();
    let invoker = Retrieval {
        result: "passage ".repeat(4_000),
    };
    let events = run(mock, invoker, budget, Vec::new()).await;
    let requests = inspector.requests();
    assert_eq!(requests.len(), 2, "no continuation without a known window");
    assert!(!requests[1].tools.is_empty(), "no context guard either");
    let result = requests[1]
        .messages
        .iter()
        .find(|message| message.role == Role::Tool)
        .expect("the result");
    assert_eq!(result.content.len(), 32_000, "nothing shortened");
    assert!(
        reports(&events)
            .iter()
            .all(|report| report.context_window.is_none())
    );
    assert_eq!(stop_reason(&events), Some(StopReason::MaxTokens));
}

#[tokio::test]
async fn legitimate_repetition_at_the_seam_is_kept() {
    let mock = MockProvider::new(vec![
        answer("The chorus goes: la la la", FinishReason::Length, None),
        answer(" la la la, and then it ends.", FinishReason::Stop, None),
    ]);
    let events = run(mock, small(), continuing(1), Vec::new()).await;
    assert_eq!(
        content(&events),
        "The chorus goes: la la la la la la, and then it ends."
    );
}
