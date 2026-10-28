//! Adaptive output allocation: how a run's output pool is divided between
//! the lead and the specialist workers its plan names.
//!
//! An [`OutputPlan`] is the small, validated contract a run is divided by:
//! the planned roles, each with a bounded [`Demand`] class, where the classes
//! came from (`source`) and why a richer source was not used
//! (`fallback_reason`). It carries no token counts. Every number comes from
//! [`allocate`], here, from the pool the operator configured:
//!
//! 1. The **lead reserve** is set aside first, for the lead's coordination
//!    and final answer: [`LEAD_RESERVE_PERCENT`] of what is available, and
//!    never less than the lead's own per-turn reservation (`lead_floor`)
//!    unless that would take more than half the pool. Workers can never
//!    reserve from it.
//! 2. A **follow-up reserve** of [`FOLLOW_UP_RESERVE_PERCENT`] is kept when
//!    an answer may be continued or a change repaired: continuations of any
//!    run and repeat starts of a planned role may draw on it.
//! 3. The rest is **split by demand weight** between the planned roles
//!    ([`split`]): one role gets all of it, equal demands get equal shares,
//!    and unequal demands bounded weighted shares (1 : 2 : 3 at most).
//!    Rounding is always down; what is left over stays unallocated.
//!
//! A planned share is held for its role only, from the moment the plan is
//! installed, and becomes reachable when that role's first worker starts.
//! The worker's own capacity then caps it (the most its model can be granted
//! per turn, times the turns it was granted), and anything above the cap is
//! split again between the roles that have not started yet. When the worker
//! ends, however it ends, what is left of its share is released for later
//! work. Nothing here sends a request or grants a token: the pool reserves,
//! each request is sent what it reserved as `max_tokens`, and the existing
//! ceilings, windows and limits apply exactly as before.
//!
//! Without an installed plan a pool behaves exactly as it did: one undivided
//! amount every turn draws on.

use std::collections::BTreeSet;

/// The version of the [`OutputPlan`] contract.
pub const PLAN_VERSION: &str = "output-plan.v1";

/// The part of the pool kept for the lead's coordination and final answer.
pub const LEAD_RESERVE_PERCENT: u32 = 25;

/// The part of the pool kept for continuations and repair rounds, when
/// either may happen.
pub const FOLLOW_UP_RESERVE_PERCENT: u32 = 10;

/// The most planned shares a plan may carry: one per worker role.
pub const MAX_SHARES: usize = 4;

/// The longest role name a plan may carry.
const MAX_ROLE_CHARS: usize = 32;

/// How much output a planned role's part is expected to need, relative to
/// the others. A bounded class, never a token count.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Demand {
    Light,
    Standard,
    Heavy,
}

impl Demand {
    /// The class's weight in a split: 1, 2 or 3.
    pub fn weight(self) -> u32 {
        match self {
            Self::Light => 1,
            Self::Standard => 2,
            Self::Heavy => 3,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Standard => "standard",
            Self::Heavy => "heavy",
        }
    }

    /// One class higher, at most heavy.
    pub fn raised(self) -> Self {
        match self {
            Self::Light => Self::Standard,
            Self::Standard | Self::Heavy => Self::Heavy,
        }
    }
}

/// One planned role and its demand class.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlannedShare {
    pub role: String,
    pub demand: Demand,
}

/// What a run's output pool is divided by. See the module documentation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OutputPlan {
    /// Where the demand classes came from, as a stable code: `role_defaults`
    /// (Lightagent's fixed classes), `jev_complexity` (raised by the task
    /// complexity Jev reported) or `no_plan` (no planned role: the lead's
    /// reserve only).
    pub source: String,
    /// Why a richer source was not used, as a stable code.
    pub fallback_reason: Option<String>,
    /// The planned roles, each at most once.
    pub shares: Vec<PlannedShare>,
    /// The least the lead reserve holds, pool permitting: the lead's own
    /// per-turn reservation.
    pub lead_floor: u32,
    /// Whether a follow-up reserve is kept.
    pub follow_up: bool,
}

impl OutputPlan {
    /// Refuse a plan the allocator must not act on: too many shares, a role
    /// name that is not a plain role code, or a role named twice.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.shares.len() > MAX_SHARES {
            return Err("too_many_shares");
        }
        let mut seen = BTreeSet::new();
        for share in &self.shares {
            let role = share.role.as_str();
            if role.is_empty()
                || role.len() > MAX_ROLE_CHARS
                || !role
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c == '_' || c == '-')
            {
                return Err("invalid_role");
            }
            if !seen.insert(role) {
                return Err("duplicate_role");
            }
        }
        Ok(())
    }
}

/// How [`allocate`] divided an amount. The parts never add up to more than
/// the amount divided.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Allocation {
    pub lead: u32,
    pub follow_up: u32,
    /// Each planned role's share, in the plan's order.
    pub shares: Vec<u32>,
    /// What no part holds: rounding, or the whole worker part without roles.
    pub unallocated: u32,
}

fn percent(tokens: u32, percent: u32) -> u32 {
    u32::try_from(u64::from(tokens) * u64::from(percent) / 100).unwrap_or(tokens)
}

/// Divide `available` tokens by `plan`. See the module documentation.
pub fn allocate(available: u32, plan: &OutputPlan) -> Allocation {
    let lead = percent(available, LEAD_RESERVE_PERCENT)
        .max(plan.lead_floor.min(available / 2))
        .min(available);
    let follow_up = if plan.follow_up {
        percent(available, FOLLOW_UP_RESERVE_PERCENT)
    } else {
        0
    }
    .min(available - lead);
    let workers = available - lead - follow_up;
    let parts: Vec<(u32, Option<u32>)> = plan
        .shares
        .iter()
        .map(|share| (share.demand.weight(), None))
        .collect();
    let shares = split(workers, &parts);
    let given = shares.iter().fold(0u32, |sum, share| sum.saturating_add(*share));
    Allocation {
        lead,
        follow_up,
        shares,
        unallocated: workers.saturating_sub(given),
    }
}

/// Split `tokens` between parts of `(weight, cap)` in proportion to their
/// weights, rounding down. A part whose cap is below its proportional share
/// gets its cap, and what it could not take is split again between the
/// others, so a small model never strands tokens a larger one could use. A
/// part of weight zero gets nothing. The result never adds up to more than
/// `tokens`.
pub fn split(tokens: u32, parts: &[(u32, Option<u32>)]) -> Vec<u32> {
    let mut shares = vec![0u32; parts.len()];
    let mut open: Vec<usize> = (0..parts.len()).filter(|i| parts[*i].0 > 0).collect();
    let mut left = tokens;
    while !open.is_empty() && left > 0 {
        let total: u64 = open.iter().map(|i| u64::from(parts[*i].0)).sum();
        let proportional = |i: usize| {
            u32::try_from(u64::from(left) * u64::from(parts[i].0) / total).unwrap_or(left)
        };
        let capped: Vec<usize> = open
            .iter()
            .copied()
            .filter(|i| parts[*i].1.is_some_and(|cap| cap <= proportional(*i)))
            .collect();
        if capped.is_empty() {
            for i in &open {
                shares[*i] = proportional(*i);
            }
            break;
        }
        let mut given = 0u32;
        for i in &capped {
            shares[*i] = parts[*i].1.unwrap_or(0);
            given = given.saturating_add(shares[*i]);
        }
        left = left.saturating_sub(given);
        open.retain(|i| !capped.contains(i));
    }
    shares
}

/// Which part of a run's output pool a handle reserves from once a plan is
/// installed. Without one, every handle draws on the whole pool alike.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Account {
    /// The lead: its reserve, then what is unallocated.
    #[default]
    Lead,
    /// The first worker of a planned role: that role's share, then what is
    /// unallocated.
    Share(String),
    /// A repeat start of a planned role (a repair or re-check): the
    /// follow-up reserve, then what is unallocated.
    FollowUp,
    /// Any other worker: only what is unallocated.
    Unplanned,
}

impl Account {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Lead => "lead",
            Self::Share(_) => "share",
            Self::FollowUp => "follow_up",
            Self::Unplanned => "unplanned",
        }
    }

    /// The role of a share account.
    pub fn role(&self) -> Option<&str> {
        match self {
            Self::Share(role) => Some(role),
            _ => None,
        }
    }
}

/// One planned share, as reported.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShareReport {
    pub role: String,
    /// `light`, `standard` or `heavy`.
    pub demand: String,
    pub weight: u32,
    /// The share planned for the role, after any cap or redistribution.
    pub planned: u32,
    /// The cap its worker's own capacity put on it when it started, if any.
    pub capacity_cap: Option<u32>,
    /// What the share still holds for the role; zero once released.
    pub left: u32,
    /// `planned` (not started), `running` or `released`.
    pub state: String,
}

/// The privacy-safe state of an installed plan, as one turn saw it. Fixed
/// codes, role names and token counts only.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AllocationReport {
    /// [`PLAN_VERSION`].
    pub version: String,
    pub source: String,
    pub fallback_reason: Option<String>,
    /// The account this turn reserved from (`lead`, `share`, `follow_up`
    /// or `unplanned`) and, for a share, its role.
    pub account: String,
    pub role: Option<String>,
    /// The lead reserve as installed, and what is left of it.
    pub lead_reserve: u32,
    pub lead_reserve_left: u32,
    /// The follow-up reserve as installed, and what is left of it.
    pub follow_up_reserve: u32,
    pub follow_up_left: u32,
    pub shares: Vec<ShareReport>,
    /// What no account holds now: unallocated, released or returned.
    pub unallocated_available: u32,
    /// What this turn asked to reserve and what it was granted; a grant
    /// below the ask was reduced by its account or the pool.
    pub asked: Option<u32>,
    pub granted: Option<u32>,
}
