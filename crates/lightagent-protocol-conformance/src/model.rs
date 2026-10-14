//! A model of one harness serving one create job, with calls serialized by
//! PR #26's ledger lock (§10.4 P2) and recorded before effect (P3). It
//! replays `execution-deadline.json`, and it lets the review ask whether
//! Web's §13.2 R3 conclusion can be wrong under harness clock steps.
//!
//! Nothing here is the service. The L3 change belongs in
//! `ProvisioningService`; this only reasons about the protocol.

use super::MAX_CLOCK_SKEW_SECONDS;
use crate::web::{Outcome, Registry, Resolution, Transmission, resolve_create_job};

/// How the harness guards a create's start of execution.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Guard {
    /// PR #26 today: no check after the lock.
    None,
    /// r2 §10 step 14: refuse when the harness clock read after acquiring
    /// the lock is at or past `expiresAt + 30`.
    Deadline,
    /// Step 14 plus a durable execution fence (proposed RC5): refuse also
    /// when `expiresAt + 30 ≤ fence`, where `fence` is the largest
    /// `issuedAt − 30` of any create that passed local admission (step 15)
    /// before. The model has no admission refusals, so that is the same
    /// point as passing step 14. The fence uses only signed Web stamps, so
    /// no harness clock step can move it backwards.
    DeadlineAndFence,
}

/// §10 step 14, single check: `proceed` or `EXPIRED`.
pub fn execution_deadline(expires_at: u64, lock_clock: u64) -> &'static str {
    if lock_clock >= expires_at + MAX_CLOCK_SKEW_SECONDS {
        "EXPIRED"
    } else {
        "proceed"
    }
}

/// One transmission as the harness sees it, in execution (lock) order.
#[derive(Clone, Debug)]
pub struct Attempt {
    /// Index in Web's transmission journal.
    pub index: usize,
    pub issued_at: u64,
    pub expires_at: u64,
    /// Harness clock at §10 step 12.
    pub admitted_clock: u64,
    /// Harness clock read after acquiring the lock (step 14).
    pub lock_clock: u64,
    /// Whether quota is free when this attempt executes.
    pub quota_free: bool,
    pub response_lost: bool,
    pub crash_after_record: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RaceResult {
    /// Per journal index.
    pub harness: Vec<String>,
    /// The operation's final state, if one exists.
    pub operation: Option<String>,
    pub web: Resolution,
    /// Web concluded `failed` while an operation exists.
    pub unsafe_: bool,
}

/// The fixtures' placeholder operation id.
pub const OPERATION_ID: &str = "00000000000000000000000000000000";

/// Run `attempts` (already in lock order) against one job's harness state.
pub fn run(registry: &Registry, attempts: &[Attempt], guard: Guard) -> RaceResult {
    let mut harness = vec![String::new(); attempts.len()];
    let mut operation: Option<&'static str> = None;
    let mut fence = 0u64;
    for a in attempts {
        let skew = MAX_CLOCK_SKEW_SECONDS;
        let answer = if a.admitted_clock >= a.expires_at + skew {
            "EXPIRED".to_owned()
        } else if a.issued_at > a.admitted_clock + skew {
            "NOT_YET_VALID".to_owned()
        } else if guard != Guard::None
            && (execution_deadline(a.expires_at, a.lock_clock) == "EXPIRED"
                || (guard == Guard::DeadlineAndFence && a.expires_at + skew <= fence))
        {
            "EXPIRED".to_owned()
        } else {
            if guard == Guard::DeadlineAndFence {
                fence = fence.max(a.issued_at.saturating_sub(skew));
            }
            match operation {
                // The idempotency lookup finds the recorded operation and
                // resumes it to a terminal state (P3, P4).
                Some(_) => {
                    operation = Some("completed");
                    "OPERATION:completed".to_owned()
                }
                None if !a.quota_free => "QUOTA_EXCEEDED".to_owned(),
                None if a.crash_after_record => {
                    operation = Some("pending");
                    "STORAGE_UNAVAILABLE".to_owned()
                }
                None => {
                    operation = Some("completed");
                    "OPERATION:completed".to_owned()
                }
            }
        };
        harness[a.index] = answer;
    }
    let mut journal: Vec<&Attempt> = attempts.iter().collect();
    journal.sort_by_key(|a| a.index);
    let transmissions: Vec<Transmission> = journal
        .iter()
        .map(|a| Transmission {
            issued_at: a.issued_at,
            expires_at: a.expires_at,
            outcome: web_outcome(&harness[a.index], a.response_lost),
        })
        .collect();
    let web = resolve_create_job(registry, &transmissions);
    let unsafe_ = matches!(web, Resolution::Failed { .. }) && operation.is_some();
    RaceResult {
        harness,
        operation: operation.map(str::to_owned),
        web,
        unsafe_,
    }
}

fn web_outcome(answer: &str, lost: bool) -> Outcome {
    if lost || answer == "STORAGE_UNAVAILABLE" {
        return Outcome::Uncertain;
    }
    match answer.strip_prefix("OPERATION:") {
        Some(state) => Outcome::Operation {
            operation_id: OPERATION_ID.to_owned(),
            state: state.to_owned(),
        },
        None => Outcome::Refused {
            code: answer.to_owned(),
        },
    }
}

/// A small deterministic generator (xorshift64*), so schedule tests are
/// reproducible without a new dependency.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    /// Uniform in `lo..=hi`.
    pub fn range(&mut self, lo: i64, hi: i64) -> i64 {
        lo + (self.next_u64() % (hi - lo + 1) as u64) as i64
    }

    pub fn chance(&mut self, percent: u64) -> bool {
        self.next_u64() % 100 < percent
    }
}

/// One random schedule: Web issues transmissions of one job (Web clock =
/// real time). The harness clock is `real + offset` with `|offset| ≤ 30`,
/// plus, when `steps` is set, clock steps of up to ±`max_step` seconds at
/// random real times. Copies are delayed in the network, wait for the
/// serialized lock, may lose their answer or crash after recording, and
/// quota is free only inside one random real-time window.
///
/// Returns the worst result over every prefix of the journal, because Web
/// may conclude after any answer and then stop sending.
pub fn random_schedule(
    registry: &Registry,
    rng: &mut Rng,
    guard: Guard,
    max_step: i64,
) -> RaceResult {
    const T0: i64 = 1_791_000_000;
    let n = rng.range(1, 4) as usize;
    let mut issued = Vec::with_capacity(n);
    let mut t = T0;
    for _ in 0..n {
        t += rng.range(0, 200);
        issued.push(t);
    }
    let offset = rng.range(-30, 30);
    let steps: Vec<(i64, i64)> = (0..if max_step > 0 { rng.range(0, 3) } else { 0 })
        .map(|_| (T0 + rng.range(0, 1200), rng.range(-max_step, max_step)))
        .collect();
    let clock = |real: i64| -> u64 {
        let stepped: i64 = steps
            .iter()
            .filter(|(at, _)| *at <= real)
            .map(|(_, d)| d)
            .sum();
        (real + offset + stepped) as u64
    };
    let quota = (T0 + rng.range(0, 1000), T0 + rng.range(0, 1400));
    let plans: Vec<(i64, i64, i64, bool, bool, i64)> = issued
        .iter()
        .map(|&issued_at| {
            let lifetime = rng.range(1, 120);
            let arrive = issued_at
                + if rng.chance(70) {
                    rng.range(0, 2)
                } else {
                    rng.range(0, 200)
                };
            let wait = if rng.chance(50) { 0 } else { rng.range(0, 600) };
            (
                lifetime,
                arrive,
                arrive + wait,
                rng.chance(40),
                rng.chance(10),
                rng.range(0, 30),
            )
        })
        .collect();
    let mut worst: Option<RaceResult> = None;
    for k in 1..=n {
        // Serialize the first k transmissions on the lock in real time.
        let mut order: Vec<usize> = (0..k).collect();
        order.sort_by_key(|&i| (plans[i].2, i));
        let mut free_at = i64::MIN;
        let attempts: Vec<Attempt> = order
            .iter()
            .map(|&i| {
                let (lifetime, arrive, want, lost, crash, hold) = plans[i];
                let acquired = want.max(free_at);
                free_at = acquired + hold;
                Attempt {
                    index: i,
                    issued_at: issued[i] as u64,
                    expires_at: (issued[i] + lifetime) as u64,
                    admitted_clock: clock(arrive),
                    lock_clock: clock(acquired),
                    quota_free: (quota.0..=quota.1).contains(&acquired),
                    response_lost: lost,
                    crash_after_record: crash,
                }
            })
            .collect();
        let result = run(registry, &attempts, guard);
        if result.unsafe_ || worst.is_none() {
            worst = Some(result);
        }
        if worst.as_ref().is_some_and(|w| w.unsafe_) {
            break;
        }
    }
    worst.expect("n ≥ 1")
}

/// The clock-step hazard r2 leaves open, randomized within r2's limits.
/// Two transmissions of one job: `u` is admitted, loses its answer and
/// waits for the lock. `t` is issued at or after `u`'s horizon, is admitted
/// and runs first while quota is exhausted. Then the harness clock steps by
/// `step` seconds (negative is backward), quota frees, and `u` acquires the
/// lock. Returns the result and the step.
pub fn stepped_pair(registry: &Registry, rng: &mut Rng, guard: Guard) -> (RaceResult, i64) {
    const T0: i64 = 1_791_000_000;
    let offset = rng.range(-30, 30);
    let u_issued = T0;
    let u_expires = u_issued + rng.range(1, 120);
    let t_issued = u_expires + 60 + rng.range(0, 30);
    let t_expires = t_issued + rng.range(1, 120);
    let u_arrive = u_issued + rng.range(0, 2);
    let t_arrive = t_issued + rng.range(0, 2);
    let u_lock = t_arrive + rng.range(1, 300);
    let step = rng.range(-90, 90);
    let clock = |real: i64, stepped: bool| (real + offset + if stepped { step } else { 0 }) as u64;
    let attempts = [
        Attempt {
            index: 1,
            issued_at: t_issued as u64,
            expires_at: t_expires as u64,
            admitted_clock: clock(t_arrive, false),
            lock_clock: clock(t_arrive, false),
            quota_free: false,
            response_lost: false,
            crash_after_record: false,
        },
        Attempt {
            index: 0,
            issued_at: u_issued as u64,
            expires_at: u_expires as u64,
            admitted_clock: clock(u_arrive, false),
            lock_clock: clock(u_lock, true),
            quota_free: true,
            response_lost: true,
            crash_after_record: false,
        },
    ];
    (run(registry, &attempts, guard), step)
}
