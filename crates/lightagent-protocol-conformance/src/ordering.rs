//! An independent, event-driven model of one harness serving one
//! `profile.create` job, written from r3 §9.2 (nonces), §10 (steps 10–16),
//! §10.5 (the execution fence) and §13.2/§13.10 (Web's resolution over its
//! journal). Each r3 rule is a switch, so `ordering-model.json`'s ablations
//! can be replayed, and the review's own adversarial schedules can be run.
//!
//! The only thing taken from the fixtures rather than the specification is
//! the timing convention: a call holds the lock for `holdFor` seconds
//! (default 1), and every answer reaches Web at the end of its call.

use crate::web::{Outcome, Registry, Resolution, Transmission, resolve_create_job_with};

const SKEW: u64 = 30;

/// The r3 rules, as `ordering-model.json` names them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rules {
    pub execution_deadline: bool,
    pub fence: bool,
    pub fence_durable_before_unlock: bool,
    pub nonce_durable: bool,
    pub nonce_retained_until_fenced: bool,
    pub protocol_refusals_final: bool,
    pub journal_before_send: bool,
}

impl Rules {
    pub const R3: Rules = Rules {
        execution_deadline: true,
        fence: true,
        fence_durable_before_unlock: true,
        nonce_durable: true,
        nonce_retained_until_fenced: true,
        protocol_refusals_final: false,
        journal_before_send: true,
    };
}

#[derive(Clone, Debug)]
pub struct Tx {
    pub issued_at: u64,
    pub expires_at: u64,
    /// `false` only models a send without a journal row (impossible under
    /// RC6, so ignored when `journal_before_send` holds).
    pub journaled: bool,
}

#[derive(Clone, Debug)]
pub struct Delivery {
    pub transmission: usize,
    pub arrives_at: u64,
    pub lock_at: Option<u64>,
    pub hold_for: u64,
    pub answer_to_web: bool,
    pub crash_after_record: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Scenario {
    pub transmissions: Vec<Tx>,
    pub deliveries: Vec<Delivery>,
    /// `(from real time, harness clock offset)`, ascending.
    pub clock: Vec<(u64, i64)>,
    /// Inclusive real-time windows in which quota is free.
    pub quota: Vec<(u64, u64)>,
    pub restarts: Vec<u64>,
    pub housekeeping: Vec<u64>,
    /// Real time from which the signing key is trusted (step 10).
    pub key_trusted_from: u64,
    /// Restores of the ledger (`fresh_ledger`) under r3's §10.5 restore rule,
    /// at real time `at`, with operator-verified time `at` (`T`).
    pub ledger_restores: Vec<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Run {
    pub answers: Vec<String>,
    /// `(state, recorded by transmission)`.
    pub operation: Option<(String, usize)>,
    pub web: Resolution,
    pub failed_at: Option<u64>,
    pub unsafe_: bool,
    /// The user owns the profile at the end (the identity store's binding),
    /// created by this transmission. A ledger replacement does not remove it.
    pub bound: Option<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    Restart,
    Restore,
    Housekeeping,
    Arrive,
    Lock,
}

pub fn simulate(registry: &Registry, scenario: &Scenario, rules: Rules) -> Run {
    let s = scenario;
    let clock = |real: u64| -> u64 {
        let offset = s
            .clock
            .iter()
            .rev()
            .find(|(from, _)| *from <= real)
            .map_or(0, |(_, o)| *o);
        (real as i64 + offset) as u64
    };
    let quota_free = |real: u64| s.quota.iter().any(|(a, b)| (*a..=*b).contains(&real));

    let mut events: Vec<(u64, Kind, usize)> = Vec::new();
    for &t in &s.restarts {
        events.push((t, Kind::Restart, 0));
    }
    for &t in &s.ledger_restores {
        events.push((t, Kind::Restore, 0));
    }
    for &t in &s.housekeeping {
        events.push((t, Kind::Housekeeping, 0));
    }
    for (i, d) in s.deliveries.iter().enumerate() {
        events.push((d.arrives_at, Kind::Arrive, i));
    }

    let mut answers = vec![String::new(); s.deliveries.len()];
    let mut answered_at = vec![0u64; s.deliveries.len()];
    // Nonce store: transmission index → expiresAt.
    let mut nonces: Vec<(usize, u64)> = Vec::new();
    // The durable fence, and an advance not yet visible to the next locker.
    let mut fence = 0u64;
    let mut pending_fence: Option<(u64, u64)> = None;
    // The ledger's one operation for the job's key, and the identity
    // store's binding (which a ledger restore does not touch).
    let mut operation: Option<(&'static str, usize)> = None;
    let mut bound: Option<usize> = None;
    // The profile directory and ownership record, which `begin` checks
    // ("profile id unknown everywhere") and a ledger restore leaves alone.
    let mut profile_known = false;
    let mut last_release = 0u64;

    while !events.is_empty() {
        events.sort_by_key(|(t, k, i)| (*t, *k, *i));
        let (now, kind, i) = events.remove(0);
        if let Some((value, visible)) = pending_fence
            && now >= visible
        {
            fence = fence.max(value);
            pending_fence = None;
        }
        match kind {
            Kind::Restart => {
                if !rules.nonce_durable {
                    nonces.clear();
                }
            }
            Kind::Restore => {
                // §10.5: a new ledger for this harness id. Operations are
                // lost; the fence becomes max(restored, T + 150).
                operation = None;
                fence = fence.max(now + 150);
            }
            Kind::Housekeeping => {
                let c = clock(now);
                nonces.retain(|(_, expires)| {
                    if expires + SKEW > c {
                        return true;
                    }
                    if rules.nonce_retained_until_fenced {
                        if !rules.fence {
                            return true;
                        }
                        fence = fence.max(expires + SKEW);
                    }
                    false
                });
            }
            Kind::Arrive => {
                let d = &s.deliveries[i];
                let tx = &s.transmissions[d.transmission];
                let c = clock(now);
                let refusal = if now < s.key_trusted_from {
                    Some("UNKNOWN_KEY")
                } else if tx.issued_at > c + SKEW {
                    Some("NOT_YET_VALID")
                } else if c >= tx.expires_at + SKEW {
                    Some("EXPIRED")
                } else if nonces.iter().any(|(t, _)| *t == d.transmission) {
                    Some("REPLAY_DETECTED")
                } else {
                    nonces.push((d.transmission, tx.expires_at));
                    None
                };
                match refusal {
                    Some(code) => {
                        answers[i] = code.to_owned();
                        answered_at[i] = now;
                    }
                    None => events.push((d.lock_at.unwrap_or(now), Kind::Lock, i)),
                }
            }
            Kind::Lock => {
                let d = &s.deliveries[i];
                let tx = &s.transmissions[d.transmission];
                let acquired = now.max(last_release);
                let release = acquired + d.hold_for;
                let c = clock(acquired);
                let deadline = tx.expires_at + SKEW;
                let answer = if (rules.execution_deadline && c >= deadline)
                    || (rules.fence && deadline <= fence)
                {
                    "EXPIRED".to_owned()
                } else {
                    // Step 15 passes (no admission faults are modelled);
                    // the fence advances.
                    if rules.fence {
                        let value = tx.issued_at.saturating_sub(SKEW);
                        if rules.fence_durable_before_unlock {
                            fence = fence.max(value);
                        } else {
                            pending_fence = Some((value, release + 1));
                        }
                    }
                    match operation {
                        Some((_, by)) => {
                            operation = Some(("completed", by));
                            bound = Some(by);
                            "OPERATION:completed".to_owned()
                        }
                        None if profile_known => "PROFILE_UNAVAILABLE".to_owned(),
                        None if !quota_free(acquired) => "QUOTA_EXCEEDED".to_owned(),
                        None if d.crash_after_record => {
                            operation = Some(("pending", d.transmission));
                            profile_known = true;
                            "STORAGE_UNAVAILABLE".to_owned()
                        }
                        None => {
                            operation = Some(("completed", d.transmission));
                            profile_known = true;
                            bound = Some(d.transmission);
                            "OPERATION:completed".to_owned()
                        }
                    }
                };
                answers[i] = answer;
                answered_at[i] = release;
                last_release = release;
            }
        }
    }

    // Web's journal: every transmission (RC6), or only journaled ones.
    let visible: Vec<usize> = (0..s.transmissions.len())
        .filter(|&t| rules.journal_before_send || s.transmissions[t].journaled)
        .collect();
    let resolve_at = |at: Option<u64>| -> Resolution {
        let journal: Vec<Transmission> = visible
            .iter()
            .filter(|&&t| at.is_none_or(|at| s.transmissions[t].issued_at <= at))
            .map(|&t| {
                let answer = s
                    .deliveries
                    .iter()
                    .enumerate()
                    .filter(|(i, d)| {
                        d.transmission == t
                            && d.answer_to_web
                            && at.is_none_or(|at| answered_at[*i] <= at)
                    })
                    .min_by_key(|(i, _)| answered_at[*i])
                    .map(|(i, _)| answers[i].as_str());
                Transmission {
                    issued_at: s.transmissions[t].issued_at,
                    expires_at: s.transmissions[t].expires_at,
                    outcome: web_outcome(answer),
                }
            })
            .collect();
        resolve_create_job_with(registry, &journal, rules.protocol_refusals_final)
    };
    let mut moments: Vec<u64> = answered_at
        .iter()
        .zip(&s.deliveries)
        .filter(|(_, d)| d.answer_to_web)
        .map(|(t, _)| *t)
        .collect();
    moments.sort_unstable();
    moments.dedup();
    let failed_at = moments
        .into_iter()
        .find(|&at| matches!(resolve_at(Some(at)), Resolution::Failed { .. }));
    let unsafe_ = failed_at
        .is_some_and(|at| operation.is_some_and(|(_, by)| s.transmissions[by].issued_at <= at));
    let web = resolve_at(None);
    Run {
        answers,
        operation: operation.map(|(state, by)| (state.to_owned(), by)),
        web,
        failed_at,
        unsafe_,
        bound,
    }
}

fn web_outcome(answer: Option<&str>) -> Outcome {
    match answer {
        None | Some("STORAGE_UNAVAILABLE" | "REPLAY_DETECTED") => Outcome::Uncertain,
        Some(a) => match a.strip_prefix("OPERATION:") {
            Some(state) => Outcome::Operation {
                operation_id: crate::model::OPERATION_ID.to_owned(),
                state: state.to_owned(),
            },
            None => Outcome::Refused { code: a.to_owned() },
        },
    }
}

/// A random schedule for the review's own adversarial search, within r3's
/// limits: 1–4 transmissions (some exactly at the previous horizon), 1–3
/// copies each, network delay, lock waits and long holds, lost answers,
/// crashes after recording, a harness clock offset of ±30 s plus up to two
/// steps of ±90 s, restarts, nonce housekeeping, a key trusted late, a quota
/// window, and (for the journal ablation) unjournaled sends. A ledger
/// replacement is added only when `restore` is set.
pub fn random_scenario(rng: &mut crate::model::Rng, restore: bool) -> Scenario {
    const T0: i64 = 1_791_000_000;
    let at = |rng: &mut crate::model::Rng, lo: i64, hi: i64| (T0 + rng.range(lo, hi)) as u64;
    let n = rng.range(1, 4) as usize;
    let mut transmissions: Vec<Tx> = Vec::new();
    let mut issued = T0;
    for k in 0..n {
        if k > 0 {
            let previous = &transmissions[k - 1];
            issued = if rng.chance(30) {
                previous.expires_at as i64 + 60
            } else {
                issued + rng.range(0, 250)
            };
        }
        transmissions.push(Tx {
            issued_at: issued as u64,
            expires_at: (issued + rng.range(1, 120)) as u64,
            journaled: !rng.chance(10),
        });
    }
    let mut deliveries = Vec::new();
    for (t, tx) in transmissions.iter().enumerate() {
        for _ in 0..if rng.chance(25) { rng.range(2, 3) } else { 1 } {
            let arrives = tx.issued_at
                + if rng.chance(60) {
                    rng.range(0, 2)
                } else {
                    rng.range(0, 400)
                } as u64;
            deliveries.push(Delivery {
                transmission: t,
                arrives_at: arrives,
                lock_at: Some(arrives + if rng.chance(50) { 0 } else { rng.range(0, 300) } as u64),
                hold_for: if rng.chance(80) { 1 } else { rng.range(1, 200) } as u64,
                answer_to_web: !rng.chance(40),
                crash_after_record: rng.chance(10),
            });
        }
    }
    let mut clock = vec![(0, rng.range(-30, 30))];
    for _ in 0..rng.range(0, 2) {
        clock.push((at(rng, 0, 900), rng.range(-90, 90)));
    }
    clock.sort_unstable();
    let window = (at(rng, 0, 900), at(rng, 0, 1300));
    Scenario {
        transmissions,
        deliveries,
        clock,
        quota: vec![window],
        restarts: (0..rng.range(0, 2)).map(|_| at(rng, 0, 900)).collect(),
        housekeeping: (0..rng.range(0, 2)).map(|_| at(rng, 0, 900)).collect(),
        key_trusted_from: if rng.chance(30) { at(rng, 0, 300) } else { 0 },
        ledger_restores: if restore {
            vec![at(rng, 0, 900)]
        } else {
            Vec::new()
        },
    }
}

impl Run {
    /// Web concluded `failed`, while the job's operation exists or the user
    /// owns the profile.
    pub fn violates_s(&self, scenario: &Scenario) -> bool {
        // Only transmissions Web had issued when it concluded count: a
        // failed job is never sent again.
        let issued_by = |t: usize, at: u64| scenario.transmissions[t].issued_at <= at;
        self.unsafe_
            || self.failed_at.is_some_and(|at| {
                self.bound.is_some_and(|t| issued_by(t, at))
                    || self
                        .operation
                        .as_ref()
                        .is_some_and(|(_, by)| issued_by(*by, at))
            })
    }
}
