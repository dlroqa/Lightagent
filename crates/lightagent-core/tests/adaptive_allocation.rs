//! Adaptive output allocation: the deterministic allocator, the pool's
//! accounts, and the loop reserving from them, with scripted providers.
//!
//! These prove control logic only: that no division, reservation or request
//! exceeds what is available, that a worker never reaches the lead's
//! reserve, that the lead can still spend what no worker uses, and that the
//! pool behaves as before without a plan. They say nothing about whether any
//! split suits a real model or task.

use async_trait::async_trait;
use lightagent_core::allocation::{
    FOLLOW_UP_RESERVE_PERCENT, LEAD_RESERVE_PERCENT, MAX_SHARES, allocate, split,
};
use lightagent_core::provider::{FinishReason, ProviderEvent, Usage};
use lightagent_core::{
    Account, AgentEvent, AgentLoop, ApprovalNeed, ContextWindow, Demand, GenerationBudget,
    GenerationReport, MockProvider, OutputPlan, OutputPool, PlannedShare, RunConfig, StopReason,
    ToolCall, ToolInvoker, ToolOutcome, ToolSchema, WindowSource,
};
use tokio_util::sync::CancellationToken;

struct Search;

/// A search whose every result is about 3,000 bytes (some 1,100 estimated
/// tokens), so each turn's prompt grows.
struct Bulky;

#[async_trait]
impl ToolInvoker for Bulky {
    fn schemas(&self) -> Vec<ToolSchema> {
        Search.schemas()
    }

    fn approval_for(&self, _call: &ToolCall) -> ApprovalNeed {
        ApprovalNeed::AutoApprove
    }

    async fn invoke(&self, _call: &ToolCall, _cancel: CancellationToken) -> ToolOutcome {
        ToolOutcome::ok("passage ".repeat(375))
    }
}

#[async_trait]
impl ToolInvoker for Search {
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
        ToolOutcome::ok("a passage")
    }
}

fn usage(prompt: u32, completion: u32) -> Option<Usage> {
    Some(Usage {
        prompt_tokens: prompt,
        completion_tokens: completion,
        total_tokens: prompt + completion,
    })
}

fn search(usage: Option<Usage>) -> Vec<ProviderEvent> {
    vec![
        ProviderEvent::RoleStarted,
        ProviderEvent::ToolCallDelta {
            index: 0,
            id: Some("call".to_owned()),
            name: Some("rag.search".into()),
            arguments: Some("{\"q\":\"x\"}".to_owned()),
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

fn on(pool: OutputPool) -> GenerationBudget {
    GenerationBudget {
        output_pool: Some(pool),
        ..GenerationBudget::default()
    }
}

async fn run(provider: MockProvider, generation: GenerationBudget) -> Vec<AgentEvent> {
    run_with(provider, Search, generation).await
}

async fn run_with(
    provider: MockProvider,
    tools: impl ToolInvoker + 'static,
    generation: GenerationBudget,
) -> Vec<AgentEvent> {
    let mut config = RunConfig::new("m");
    config.limits.wall_clock_secs = None;
    config.limits.max_turns = 40;
    config.limits.max_tool_calls = 40;
    // The scripted worker repeats one search until its budget stops it.
    config.limits.max_repeated_identical_calls = 40;
    config.generation = generation;
    AgentLoop::new(provider, tools, config)
        .run("do the part", CancellationToken::new())
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

fn sent(mock: &MockProvider) -> Vec<Option<u32>> {
    mock.requests()
        .iter()
        .map(|request| request.max_tokens)
        .collect()
}

fn share(role: &str, demand: Demand) -> PlannedShare {
    PlannedShare {
        role: role.to_owned(),
        demand,
    }
}

fn plan(shares: Vec<PlannedShare>, follow_up: bool) -> OutputPlan {
    OutputPlan {
        source: "role_defaults".to_owned(),
        fallback_reason: None,
        shares,
        lead_floor: 2_048,
        follow_up,
    }
}

/// What the plan's accounts, the free rest, the charges and the turns in
/// flight add up to. Without an overrun it is exactly the pool.
fn accounted(pool: &OutputPool, in_flight: u32) -> u32 {
    let report = pool.allocation_report(None).expect("a plan");
    let shares: u32 = report.shares.iter().map(|share| share.left).sum();
    report.lead_reserve_left
        + report.follow_up_left
        + shares
        + report.unallocated_available
        + pool.spent()
        + in_flight
}

#[test]
fn split_is_proportional_rounds_down_and_never_exceeds_the_amount() {
    assert_eq!(split(9_000, &[(2, None), (2, None)]), [4_500, 4_500]);
    assert_eq!(split(6_000, &[(3, None), (1, None)]), [4_500, 1_500]);
    assert_eq!(split(10, &[(1, None), (1, None), (1, None)]), [3, 3, 3]);
    assert_eq!(split(0, &[(3, None), (1, None)]), [0, 0]);
    assert_eq!(split(100, &[(0, None), (1, None)]), [0, 100]);
    assert!(split(100, &[]).is_empty());
    // One part alone takes everything.
    assert_eq!(split(20_800, &[(1, None)]), [20_800]);
}

#[test]
fn a_capped_part_takes_its_cap_and_the_rest_is_split_again() {
    // Proportionally 2,500 / 2,500 / 5,000; the first can take only 1,000.
    assert_eq!(
        split(10_000, &[(1, Some(1_000)), (1, None), (2, None)]),
        [1_000, 3_000, 6_000]
    );
    // Two capped at once, the third takes what they leave.
    assert_eq!(
        split(9_000, &[(1, Some(500)), (1, Some(3_000)), (1, None)]),
        [500, 3_000, 5_500]
    );
    // Every part capped: what none can take stays unsplit.
    assert_eq!(split(10_000, &[(1, Some(100)), (1, Some(200))]), [100, 200]);
}

#[test]
fn allocate_reserves_the_lead_and_follow_up_before_any_worker() {
    let one = allocate(32_000, &plan(vec![share("coder", Demand::Heavy)], true));
    assert_eq!((one.lead, one.follow_up), (8_000, 3_200));
    assert_eq!(
        one.shares,
        [20_800],
        "a sole worker gets the whole worker part"
    );
    assert_eq!(one.unallocated, 0);

    let equal = allocate(
        32_000,
        &plan(
            vec![
                share("researcher", Demand::Standard),
                share("reviewer", Demand::Standard),
            ],
            false,
        ),
    );
    assert_eq!(equal.lead, 8_000);
    assert_eq!(equal.shares, [12_000, 12_000]);

    let none = allocate(32_000, &plan(Vec::new(), false));
    assert_eq!((none.lead, none.unallocated), (8_000, 24_000));

    // A small pool still keeps one lead turn, but never more than half.
    let small = allocate(3_000, &plan(vec![share("coder", Demand::Heavy)], true));
    assert_eq!(
        (small.lead, small.follow_up, small.shares[0]),
        (1_500, 300, 1_200)
    );
    let ceiling = OutputPlan {
        lead_floor: 9_000,
        ..plan(Vec::new(), false)
    };
    assert_eq!(allocate(32_000, &ceiling).lead, 9_000);
}

#[test]
fn allocations_always_add_up_to_what_was_available() {
    let demands = [Demand::Light, Demand::Standard, Demand::Heavy];
    let roles = ["researcher", "coder", "reviewer", "tester"];
    for available in [0, 1, 15, 16, 100, 1_023, 4_096, 32_000, 1_000_003, u32::MAX] {
        for count in 0..=MAX_SHARES {
            for floor in [0, 2_048, u32::MAX] {
                for follow_up in [false, true] {
                    let shares = (0..count)
                        .map(|index| share(roles[index], demands[index % 3]))
                        .collect();
                    let plan = OutputPlan {
                        lead_floor: floor,
                        ..plan(shares, follow_up)
                    };
                    let a = allocate(available, &plan);
                    let parts = u64::from(a.lead)
                        + u64::from(a.follow_up)
                        + a.shares.iter().map(|s| u64::from(*s)).sum::<u64>()
                        + u64::from(a.unallocated);
                    assert_eq!(parts, u64::from(available), "{available} {count} {floor}");
                    assert!(
                        u64::from(a.lead) * 100
                            >= u64::from(available) * u64::from(LEAD_RESERVE_PERCENT)
                                - u64::from(available) * u64::from(LEAD_RESERVE_PERCENT) % 100
                    );
                    assert!(a.lead <= available / 2);
                    if follow_up {
                        assert!(
                            u64::from(a.follow_up) * 100
                                <= u64::from(available) * u64::from(FOLLOW_UP_RESERVE_PERCENT)
                        );
                    } else {
                        assert_eq!(a.follow_up, 0);
                    }
                }
            }
        }
    }
}

#[test]
fn an_invalid_plan_is_refused_and_the_pool_stays_undivided() {
    let pool = OutputPool::new(32_000);
    for bad in [
        plan(
            vec![share("coder", Demand::Heavy), share("coder", Demand::Light)],
            false,
        ),
        plan(vec![share("Coder", Demand::Heavy)], false),
        plan(vec![share("", Demand::Heavy)], false),
        plan(vec![share("rm -rf", Demand::Heavy)], false),
        plan(
            ["a", "b", "c", "d", "e"]
                .iter()
                .map(|role| share(role, Demand::Light))
                .collect(),
            false,
        ),
    ] {
        assert!(pool.install(bad).is_err());
        assert!(pool.allocation_report(None).is_none());
    }
    // Undivided, every handle reaches the whole pool, as before.
    assert_eq!(pool.with_account(Account::Unplanned).available(), 32_000);
}

#[test]
fn accounts_reach_only_their_own_tokens_and_the_free_rest() {
    let pool = OutputPool::new(32_000);
    let plan = plan(
        vec![
            share("coder", Demand::Heavy),
            share("tester", Demand::Light),
        ],
        true,
    );
    pool.install(plan.clone()).expect("installed");
    assert_eq!(pool.install(plan).err(), Some("already_installed"));

    // Everything is held. No worker has started, so the lead could, last,
    // borrow all of it; an unplanned worker reaches nothing at all.
    assert_eq!(pool.available(), 32_000);
    let unplanned = pool.with_account(Account::Unplanned);
    assert_eq!(unplanned.available(), 0);
    assert!(unplanned.reserve(Some(100)).is_none());
    // A continuation of an unplanned worker may use the follow-up reserve.
    assert_eq!(unplanned.available_for(true), 3_200);

    // The coder's first worker holds its share, and only its share.
    let (coder, claim) = pool.claim("coder", None);
    assert_eq!(coder.account(), &Account::Share("coder".to_owned()));
    let claim = claim.expect("the first coder claims the share");
    assert_eq!(coder.available(), 15_600);
    let grant = coder.reserve(Some(50_000)).expect("granted");
    assert_eq!(grant.tokens(), 15_600, "never the tester's or the lead's");
    let report = pool.allocation_report(None).expect("plan");
    assert_eq!(report.lead_reserve_left, 8_000, "the lead reserve is untouched");
    // The lead never reaches a running worker's share.
    assert_eq!(pool.available(), 8_000 + 3_200 + 5_200);
    assert_eq!(accounted(&pool, 15_600), 32_000);
    grant.settle(Some(600));
    assert_eq!(coder.available(), 15_000, "the unused part came back to it");
    assert_eq!(accounted(&pool, 0), 32_000);

    // Its share is released when its worker ends; later work can use it.
    drop(claim);
    assert_eq!(
        coder.available(),
        15_000,
        "now only as part of the free rest"
    );
    assert_eq!(unplanned.available(), 15_000);
    assert_eq!(pool.available(), 8_000 + 15_000 + 3_200 + 5_200);
    assert_eq!(accounted(&pool, 0), 32_000);

    // A repeat start of the coder (a repair) reserves from the follow-up
    // reserve and the free rest, never a second share.
    let (repair, none) = pool.claim("coder", None);
    assert!(none.is_none());
    assert_eq!(repair.account(), &Account::FollowUp);
    assert_eq!(repair.available(), 3_200 + 15_000);

    // A role the plan does not name is unplanned.
    let (other, none) = pool.claim("researcher", None);
    assert!(none.is_none());
    assert_eq!(other.account(), &Account::Unplanned);
}

#[test]
fn a_worker_capped_by_its_capacity_passes_the_rest_to_roles_not_started() {
    let pool = OutputPool::new(32_000);
    pool.install(plan(
        vec![
            share("coder", Demand::Heavy),
            share("tester", Demand::Light),
        ],
        true,
    ))
    .expect("installed");
    let (coder, _claim) = pool.claim("coder", Some(4_000));
    assert_eq!(coder.available(), 4_000);
    let report = pool.allocation_report(None).expect("plan");
    let coder_share = &report.shares[0];
    assert_eq!(
        (coder_share.planned, coder_share.capacity_cap),
        (4_000, Some(4_000))
    );
    assert_eq!(coder_share.state, "running");
    assert_eq!(report.shares[1].planned, 5_200 + 11_600, "redistributed");
    assert_eq!(report.shares[1].state, "planned");
    assert_eq!(accounted(&pool, 0), 32_000, "no token created or lost");
    // A worker already running is never cut back; the tester, once started,
    // keeps what it was given.
    let (tester, _claim) = pool.claim("tester", Some(1_000_000));
    assert_eq!(tester.available(), 16_800);
}

#[test]
fn unsettled_or_overrun_turns_are_charged_and_stop_further_grants() {
    let pool = OutputPool::new(10_000);
    pool.install(plan(vec![share("researcher", Demand::Standard)], false))
        .expect("installed");
    let (worker, _claim) = pool.claim("researcher", None);
    // Dropped unsettled (cancelled, failed): charged in full, nothing back.
    drop(worker.reserve(Some(1_000)));
    assert_eq!(pool.spent(), 1_000);
    assert!(pool.estimated());
    assert_eq!(worker.available(), 6_500);
    assert_eq!(accounted(&pool, 0), 10_000);
    // More than granted: the excess is charged and nothing more is granted,
    // to the lead or anyone.
    worker
        .reserve(Some(500))
        .expect("granted")
        .settle(Some(900));
    assert_eq!(pool.overrun(), 400);
    assert!(worker.reserve(Some(10)).is_none());
    assert!(pool.reserve(Some(10)).is_none());
    assert_eq!(
        OutputPool::new(100)
            .install(plan(Vec::new(), false))
            .map(|a| a.lead),
        Ok(50)
    );
}

/// A small deterministic generator, so the interleaving below is the same on
/// every run and every platform.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self, below: u32) -> u32 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        u32::try_from((self.0 >> 33) % u64::from(below.max(1))).unwrap_or(0)
    }
}

#[test]
fn random_interleavings_never_create_tokens_or_reach_the_lead_reserve() {
    for seed in 0..200u64 {
        let mut random = Lcg(seed);
        let total = 1_000 + random.next(60_000);
        let pool = OutputPool::new(total);
        let roles = ["researcher", "coder", "reviewer", "tester"];
        let count = random.next(5) as usize;
        let shares = roles[..count]
            .iter()
            .map(|role| {
                share(
                    role,
                    [Demand::Light, Demand::Standard, Demand::Heavy][random.next(3) as usize],
                )
            })
            .collect();
        pool.install(plan(shares, random.next(2) == 0))
            .expect("installed");
        let lead_reserve = pool.allocation_report(None).expect("plan").lead_reserve;
        // The lead first, then the workers.
        let mut handles = vec![pool.clone(), pool.with_account(Account::Unplanned)];
        let mut claims = Vec::new();
        let mut open = Vec::new();
        for _ in 0..80 {
            match random.next(5) {
                0 => {
                    let role = roles[random.next(4) as usize];
                    let cap = (random.next(2) == 0).then(|| random.next(20_000));
                    let (handle, claim) = pool.claim(role, cap);
                    handles.push(handle);
                    claims.extend(claim);
                }
                1 if !claims.is_empty() => {
                    let index = random.next(claims.len() as u32) as usize;
                    drop(claims.swap_remove(index));
                }
                2 | 3 => {
                    let handle = &handles[random.next(handles.len() as u32) as usize];
                    let ask = random.next(8_000);
                    let before = pool.allocation_report(None).expect("plan");
                    let continuation = random.next(3) == 0;
                    let reachable = handle.available_for(continuation);
                    if let Some(grant) = handle.reserve_for(Some(ask), continuation) {
                        assert!(grant.tokens() <= ask.max(1), "seed {seed}");
                        assert!(grant.tokens() <= reachable, "seed {seed}");
                        let after = pool.allocation_report(None).expect("plan");
                        let left = |report: &lightagent_core::AllocationReport, state: &str| {
                            report
                                .shares
                                .iter()
                                .filter(|share| share.state == state)
                                .map(|share| (share.role.clone(), share.left))
                                .collect::<Vec<_>>()
                        };
                        if handle.account() == &Account::Lead {
                            assert_eq!(
                                left(&after, "running"),
                                left(&before, "running"),
                                "seed {seed}: the lead reached a running share"
                            );
                        } else {
                            assert_eq!(
                                after.lead_reserve_left, before.lead_reserve_left,
                                "seed {seed}: a worker reached the lead reserve"
                            );
                            assert_eq!(
                                left(&after, "planned"),
                                left(&before, "planned"),
                                "seed {seed}: a worker reached a planned share"
                            );
                        }
                        open.push(grant);
                    }
                }
                _ if !open.is_empty() => {
                    let grant = open.swap_remove(random.next(open.len() as u32) as usize);
                    let used = match random.next(3) {
                        0 => None,
                        _ => Some(random.next(grant.tokens() + 1)),
                    };
                    grant.settle(used);
                }
                _ => {}
            }
            let in_flight: u32 = open.iter().map(|grant| grant.tokens()).sum();
            assert_eq!(accounted(&pool, in_flight), total, "seed {seed}");
            let report = pool.allocation_report(None).expect("plan");
            assert!(report.lead_reserve_left <= lead_reserve, "seed {seed}");
        }
        // The lead can still reserve what is left of its reserve.
        let left = pool
            .allocation_report(None)
            .expect("plan")
            .lead_reserve_left;
        if left >= 16 {
            assert!(
                pool.reserve(Some(left))
                    .is_some_and(|grant| grant.tokens() >= left)
            );
        }
    }
}

#[tokio::test]
async fn workers_cannot_starve_the_leads_answer_with_a_plan_and_could_without() {
    // Before: one worker looping on unknown capacity takes the whole pool,
    // and the lead has nothing left to answer with.
    let pool = OutputPool::new(10_000);
    let worker = MockProvider::looping(vec![search(None)]);
    let lead = on(pool.clone()).for_worker(true);
    let events = run(worker.clone(), lead).await;
    assert_eq!(stop_reason(&events), Some(StopReason::OutputBudget));
    assert_eq!(pool.available(), 0, "an undivided pool was drained");

    // With a plan: the worker's share runs out, the lead's reserve does not.
    let pool = OutputPool::new(10_000);
    pool.install(plan(vec![share("researcher", Demand::Standard)], false))
        .expect("installed");
    let (handle, claim) = pool.claim("researcher", None);
    let worker = MockProvider::looping(vec![search(None)]);
    let events = run(worker.clone(), on(handle)).await;
    drop(claim);
    assert_eq!(stop_reason(&events), Some(StopReason::OutputBudget));
    assert_eq!(
        sent(&worker),
        [Some(2_048), Some(2_048), Some(2_048), Some(1_356)]
    );
    let report = &reports(&events)[0];
    let allocation = report.allocation.as_ref().expect("reported");
    assert_eq!(allocation.account, "share");
    assert_eq!(allocation.role.as_deref(), Some("researcher"));
    assert_eq!(
        (allocation.asked, allocation.granted),
        (Some(2_048), Some(2_048))
    );
    assert_eq!(allocation.lead_reserve_left, 2_500);

    let lead = MockProvider::new(vec![answer(
        "the answer",
        FinishReason::Stop,
        usage(10, 300),
    )]);
    let events = run(lead.clone(), on(pool.clone())).await;
    assert_eq!(stop_reason(&events), Some(StopReason::EndTurn));
    assert_eq!(sent(&lead), [Some(2_048)], "the lead's reserve answered");
    let allocation = reports(&events)[0].allocation.clone().expect("reported");
    assert_eq!(allocation.account, "lead");
}

#[tokio::test]
async fn equal_workers_running_together_get_equal_shares_and_no_more() {
    let pool = OutputPool::new(32_000);
    pool.install(plan(
        vec![
            share("researcher", Demand::Standard),
            share("reviewer", Demand::Standard),
        ],
        false,
    ))
    .expect("installed");
    let (a, claim_a) = pool.claim("researcher", None);
    let (b, claim_b) = pool.claim("reviewer", None);
    let mocks = [
        MockProvider::looping(vec![search(None)]),
        MockProvider::looping(vec![search(None)]),
    ];
    let runs = [run(mocks[0].clone(), on(a)), run(mocks[1].clone(), on(b))];
    let outcomes = futures_util::future::join_all(runs).await;
    drop((claim_a, claim_b));
    for (mock, events) in mocks.iter().zip(&outcomes) {
        let granted: u32 = sent(mock).iter().flatten().sum();
        assert_eq!(granted, 12_000, "each exactly its share");
        assert_eq!(stop_reason(events), Some(StopReason::OutputBudget));
    }
    assert_eq!(pool.available(), 8_000, "the lead's reserve is whole");
}

#[tokio::test]
async fn a_continuation_may_use_the_follow_up_reserve_its_share_does_not_hold() {
    let pool = OutputPool::new(10_000);
    pool.install(plan(vec![share("coder", Demand::Heavy)], true))
        .expect("installed");
    // Lead 2,500, follow-up 1,000, the coder 6,500.
    let (handle, _claim) = pool.claim("coder", None);
    let budget = GenerationBudget {
        context_window: Some(ContextWindow {
            tokens: 8_192,
            source: WindowSource::Provider,
        }),
        max_output_tokens: Some(6_500),
        reserve_answer_tokens: 1_024,
        max_continuations: 1,
        output_pool: Some(handle),
        ..GenerationBudget::default()
    };
    let mock = MockProvider::new(vec![
        answer(
            "the first part of the change",
            FinishReason::Length,
            usage(100, 6_500),
        ),
        answer("and the rest of it", FinishReason::Stop, usage(200, 300)),
    ]);
    let events = run(mock.clone(), budget).await;
    assert_eq!(sent(&mock), [Some(6_500), Some(1_000)]);
    assert_eq!(stop_reason(&events), Some(StopReason::EndTurn));
    let report = pool.allocation_report(None).expect("plan");
    assert_eq!(
        report.lead_reserve_left, 2_500,
        "the lead's reserve is whole"
    );
    assert_eq!(report.follow_up_left, 700);
}

#[tokio::test]
async fn without_a_plan_nothing_is_reported_and_nothing_changes() {
    let pool = OutputPool::new(10_000);
    let mock = MockProvider::new(vec![answer("done", FinishReason::Stop, usage(10, 20))]);
    let events = run(mock.clone(), on(pool.clone()).for_worker(true)).await;
    assert_eq!(sent(&mock), [Some(2_048)]);
    assert!(
        reports(&events)
            .iter()
            .all(|report| report.allocation.is_none())
    );
    // A worker's handle is never the lead's, and is still the same pool.
    let worker = on(pool.clone()).for_worker(false);
    let handle = worker.output_pool.expect("pool");
    assert_eq!(handle.account(), &Account::Unplanned);
    assert_eq!(handle, pool);
    let (claimed, claim) = pool.claim("coder", Some(10));
    assert!(claim.is_none(), "nothing to claim without a plan");
    assert_eq!(claimed.available(), 10_000 - 20);
}

#[test]
fn turn_capacity_is_the_ceiling_else_the_window_else_the_pool_default() {
    let window = Some(ContextWindow {
        tokens: 10_000,
        source: WindowSource::Provider,
    });
    let ceiling = GenerationBudget {
        max_output_tokens: Some(1_024),
        context_window: window,
        ..GenerationBudget::default()
    };
    assert_eq!(ceiling.turn_capacity(), Some(1_024));
    let known = GenerationBudget {
        context_window: window,
        ..GenerationBudget::default()
    };
    assert_eq!(known.turn_capacity(), Some(9_800), "less the 2% margin");
    let unknown = on(OutputPool::new(50_000).with_turn_tokens(512));
    assert_eq!(unknown.turn_capacity(), Some(512));
    assert_eq!(GenerationBudget::default().turn_capacity(), None);
}

#[test]
fn the_lead_borrows_last_and_never_from_a_running_worker() {
    let pool = OutputPool::new(32_000);
    pool.install(plan(
        vec![
            share("coder", Demand::Heavy),
            share("tester", Demand::Light),
        ],
        true,
    ))
    .expect("installed");
    // In order: its own reserve, the free rest, the follow-up reserve, then
    // the shares of roles not started.
    let grant = pool.reserve(Some(10_000)).expect("granted");
    let report = pool.allocation_report(None).expect("plan");
    assert_eq!((report.lead_reserve_left, report.follow_up_left), (0, 1_200));
    assert_eq!((report.shares[0].left, report.shares[1].left), (15_600, 5_200));
    // Usage counts against its reserve first; the unused part goes back.
    grant.settle(Some(9_000));
    let report = pool.allocation_report(None).expect("plan");
    assert_eq!((report.lead_reserve_left, report.follow_up_left), (0, 2_200));
    assert_eq!(accounted(&pool, 0), 32_000);

    // The coder starts: its share is never lent.
    let (coder, _claim) = pool.claim("coder", None);
    assert_eq!(pool.available(), 2_200 + 5_200);
    let grant = pool.reserve(Some(50_000)).expect("granted");
    assert_eq!(grant.tokens(), 7_400);
    assert_eq!(coder.available(), 15_600, "the coder's share is whole");
    // What the lead borrowed and did not use goes back to that share.
    grant.settle(Some(2_500));
    let report = pool.allocation_report(None).expect("plan");
    assert_eq!(report.follow_up_left, 0);
    assert_eq!(report.shares[1].left, 5_200 - 300);
    assert_eq!(accounted(&pool, 0), 32_000);
    // A worker still reaches none of what the lead could.
    assert_eq!(pool.with_account(Account::Unplanned).available(), 0);
}

#[test]
fn shares_of_workers_that_fail_or_never_start_are_not_stranded() {
    let pool = OutputPool::new(10_000);
    pool.install(plan(
        vec![
            share("researcher", Demand::Standard),
            share("reviewer", Demand::Standard),
        ],
        true,
    ))
    .expect("installed");
    // Lead 2,500, follow-up 1,000, 3,250 each. The reviewer's worker
    // started and failed: its share is released to everyone.
    let (_, claim) = pool.claim("reviewer", None);
    drop(claim);
    assert_eq!(pool.with_account(Account::Unplanned).available(), 3_250);
    // The researcher never started (declined or refused): only the lead
    // may use its share, last.
    assert_eq!(pool.available(), 10_000);
    let grant = pool.reserve(None).expect("granted");
    assert_eq!(grant.tokens(), 10_000);
    // Cancelled before any usage: charged in full, nothing comes back.
    drop(grant);
    assert_eq!(pool.spent(), 10_000);
    assert!(pool.estimated());
    assert_eq!(pool.available(), 0);
    assert_eq!(accounted(&pool, 0), 10_000);
}

#[test]
fn continuations_and_repeat_starts_share_one_follow_up_reserve() {
    let pool = OutputPool::new(32_000);
    pool.install(plan(vec![share("coder", Demand::Heavy)], true))
        .expect("installed");
    // Lead 8,000, follow-up 3,200, the coder 20,800, nothing free.
    let (coder, _claim) = pool.claim("coder", None);
    let (repair, none) = pool.claim("coder", None);
    assert!(none.is_none());
    assert_eq!(repair.account(), &Account::FollowUp);
    let unplanned = pool.with_account(Account::Unplanned);
    // Not a continuation: an unplanned worker reaches nothing.
    assert!(unplanned.reserve(Some(100)).is_none());
    // Together they never get more than the reserve holds.
    let first = repair.reserve(Some(2_000)).expect("granted");
    let second = unplanned.reserve_for(Some(2_000), true).expect("granted");
    assert_eq!((first.tokens(), second.tokens()), (2_000, 1_200));
    assert!(repair.reserve(Some(100)).is_none(), "exhausted");
    // Neither reached the lead's reserve or the running coder's share.
    let report = pool.allocation_report(None).expect("plan");
    assert_eq!(report.lead_reserve_left, 8_000);
    assert_eq!(coder.available(), 20_800);
    // A cancelled continuation is charged in full; a settled repair gives
    // back what it did not use.
    drop(second);
    first.settle(Some(500));
    let report = pool.allocation_report(None).expect("plan");
    assert_eq!(report.follow_up_left, 1_500);
    assert_eq!(pool.spent(), 1_700);
    assert_eq!(accounted(&pool, 0), 32_000);
    // Never needed again, what is left is the lead's to use, last.
    assert_eq!(pool.available(), 8_000 + 1_500);
}

#[tokio::test]
async fn with_no_worker_running_the_lead_can_spend_the_whole_pool() {
    // No plan at all, and a plan whose coder never starts.
    for shares in [Vec::new(), vec![share("coder", Demand::Heavy)]] {
        let pool = OutputPool::new(5_000);
        pool.install(plan(shares, true)).expect("installed");
        let lead = MockProvider::looping(vec![search(None)]);
        let events = run(lead.clone(), on(pool.clone())).await;
        assert_eq!(stop_reason(&events), Some(StopReason::OutputBudget));
        // Each turn still asks no more than the per-turn default.
        assert_eq!(sent(&lead), [Some(2_048), Some(2_048), Some(904)]);
        assert_eq!(pool.spent(), 5_000, "not only its 25% reserve");
    }
}

#[tokio::test]
async fn the_final_answer_may_use_what_workers_and_follow_ups_left() {
    let pool = OutputPool::new(10_000);
    pool.install(plan(vec![share("researcher", Demand::Standard)], true))
        .expect("installed");
    // Lead 2,500, follow-up 1,000, the researcher 6,500. It used 500 of a
    // 2,048 reservation and ended.
    let (worker, claim) = pool.claim("researcher", None);
    worker.reserve(Some(2_048)).expect("granted").settle(Some(500));
    drop(claim);
    // The lead's answer, under an operator ceiling of 9,000 per request,
    // gets more than its own reserve and never more than the ceiling.
    let lead = MockProvider::new(vec![answer(
        "the answer",
        FinishReason::Stop,
        usage(10, 4_000),
    )]);
    let budget = GenerationBudget {
        max_output_tokens: Some(9_000),
        ..on(pool.clone())
    };
    let events = run(lead.clone(), budget).await;
    assert_eq!(stop_reason(&events), Some(StopReason::EndTurn));
    assert_eq!(sent(&lead), [Some(9_000)]);
    assert_eq!(pool.spent(), 4_500);
    assert_eq!(accounted(&pool, 0), 10_000);
}

#[tokio::test]
async fn each_turn_is_bounded_by_the_room_left_now_not_by_the_share() {
    let pool = OutputPool::new(100_000);
    pool.install(plan(vec![share("reviewer", Demand::Standard)], false))
        .expect("installed");
    // The share (75,000) is capped at four turns of a 4,096 window, and
    // that cap is far above what any one turn here can use.
    let (handle, _claim) = pool.claim("reviewer", Some(4 * 4_096));
    let budget = GenerationBudget {
        context_window: Some(ContextWindow {
            tokens: 4_096,
            source: WindowSource::Provider,
        }),
        output_pool: Some(handle),
        ..GenerationBudget::default()
    };
    let mock = MockProvider::new(vec![
        search(None),
        search(None),
        answer("done", FinishReason::Stop, None),
    ]);
    let events = run_with(mock.clone(), Bulky, budget).await;
    assert_eq!(stop_reason(&events), Some(StopReason::EndTurn));
    let asked: Vec<u32> = reports(&events)
        .iter()
        .map(|report| {
            let allocation = report.allocation.as_ref().expect("reported");
            assert_eq!(allocation.asked, allocation.granted, "the room, not the share, bounds it");
            allocation.granted.expect("granted")
        })
        .collect();
    assert_eq!(sent(&mock), asked.iter().map(|tokens| Some(*tokens)).collect::<Vec<_>>());
    // The room shrinks as each result lands, and every request fits it.
    assert_eq!(asked.len(), 3);
    assert!(asked[0] < 4_096 && asked[0] > asked[1] + 1_000, "{asked:?}");
    assert!(asked[1] > asked[2] + 1_000, "{asked:?}");
}
