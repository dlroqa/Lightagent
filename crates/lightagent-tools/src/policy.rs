//! Per-profile tool policy: which of the enabled tools a run may use.
//!
//! ```text
//! effective = registry (global settings ∩ runtime capabilities)
//!           ∩ the profile's toolsets (by mode)
//!           ∩ what the run's trust allows without execution isolation
//! ```
//!
//! The registry a run is built with is already the global-and-capability
//! part: `configured_registry` only adds tools whose settings are enabled and
//! whose service is available. This module narrows it; it never adds a tool.
//! Approvals are separate. The run's `PolicyEngine` still gates every
//! remaining call, and nothing here relaxes it.
//!
//! The result is applied by building the run's registry from it.
//! [`BoundedExecutor`](crate::BoundedExecutor) refuses any call to a tool not
//! in its registry, so a withheld tool cannot be invoked by naming it, not
//! only hidden from the model.

use std::collections::BTreeSet;

use lightagent_core::ToolPolicyMode;
use lightagent_core::identity::RunTrust;
use serde::Serialize;

use crate::registry::ToolRegistry;

/// What a tool can reach through the server's own OS account.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolReach {
    /// Nothing outside the process (`datetime.now`).
    Inert,
    /// The network or an external service (`web.*`, `rag.realtime`,
    /// `jev.classify_task`, MCP servers over HTTP).
    Network,
    /// The profile's own data directory, confined by path construction only
    /// (`memory.*`, `session.lookup`, `rag.search`, `skill.read`).
    ProfileData,
    /// Commands in the installation's shared Open Terminal service, or
    /// workflows on its GitHub repository (`actions.*`).
    RemoteExecution,
    /// Files in the workspace, read and written as the server's OS account.
    LocalFilesystem,
    /// Local processes as the server's OS account (`terminal.run`, MCP stdio
    /// servers), and any tool this module does not recognize.
    LocalCommand,
    /// `agent.delegate`: a worker run, under a profile the model names.
    Delegation,
}

impl ToolReach {
    /// Whether the tool acts through the server's OS account or another
    /// profile, which no allowlist alone makes safe for an untrusted caller.
    /// Until real execution isolation exists these are never offered to a
    /// `RemoteUser` run.
    pub fn requires_isolation(self) -> bool {
        matches!(
            self,
            Self::LocalFilesystem | Self::LocalCommand | Self::RemoteExecution | Self::Delegation
        )
    }
}

/// Classify `name`. `network_mcp_servers` names the MCP servers reached over
/// HTTP; a tool of any other MCP server (stdio, or a name that does not
/// match) is a local command. Unknown tools are local commands too: the most
/// restricted class, so a mistake here can only withhold, never expose.
pub fn reach_of(name: &str, network_mcp_servers: &BTreeSet<String>) -> ToolReach {
    match name {
        "datetime.now" => ToolReach::Inert,
        "web.fetch" | "web.search" | "rag.realtime" | "jev.classify_task" => ToolReach::Network,
        // A read-only researcher whose own tools are the run's, each checked
        // again: it reaches what the run already may, nothing more.
        "agent.research" => ToolReach::Network,
        "memory.write" | "memory.search" | "memory.reflect" | "session.lookup" | "rag.search"
        | "skill.read" => ToolReach::ProfileData,
        "open_terminal.run" => ToolReach::RemoteExecution,
        // Runs workflows on the operator's GitHub repository.
        "actions.dispatch" | "actions.status" | "actions.logs" | "actions.cancel" => {
            ToolReach::RemoteExecution
        }
        "fs.read" | "fs.list" | "fs.write" | "patch.propose" | "patch.apply" => {
            ToolReach::LocalFilesystem
        }
        "repo.commit" => ToolReach::RemoteExecution,
        "terminal.run" => ToolReach::LocalCommand,
        "agent.delegate" | "agent.resume_worker" | "agent.decline_worker" => ToolReach::Delegation,
        _ => match name
            .strip_prefix("mcp.")
            .and_then(|rest| rest.split_once('.'))
        {
            Some((server, _)) if network_mcp_servers.contains(server) => ToolReach::Network,
            _ => ToolReach::LocalCommand,
        },
    }
}

/// One `toolsets` entry.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Entry {
    /// `all`: every enabled tool. Not accepted under `strict`.
    All,
    /// `read-only`: tools whose risk class is `observe`.
    ReadOnly,
    /// `ns.*`: every tool whose name starts with `ns.`.
    Namespace(String),
    /// An exact tool name.
    Exact(String),
}

fn parse_entry(raw: &str) -> Option<Entry> {
    let valid_name = |name: &str| {
        !name.is_empty()
            && name.split('.').all(|part| {
                !part.is_empty()
                    && part.bytes().all(|b| {
                        b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-'
                    })
            })
    };
    match raw {
        "all" => Some(Entry::All),
        "read-only" => Some(Entry::ReadOnly),
        _ => match raw.strip_suffix(".*") {
            Some(prefix) if valid_name(prefix) => Some(Entry::Namespace(format!("{prefix}."))),
            Some(_) => None,
            None if raw.contains('.') && valid_name(raw) => Some(Entry::Exact(raw.to_owned())),
            None => None,
        },
    }
}

/// Why a tool was withheld. A fixed vocabulary, safe to log and report.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Withheld {
    /// The profile's `toolsets` does not include it.
    NotInProfile,
    /// The profile's `toolsets` holds an entry `strict` does not accept, so
    /// every tool is withheld.
    InvalidProfileToolsets,
    /// The tool reaches the server's OS account or another profile, and the
    /// run's caller is not trusted with it without execution isolation.
    RequiresIsolation,
}

impl Withheld {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotInProfile => "not_in_profile",
            Self::InvalidProfileToolsets => "invalid_profile_toolsets",
            Self::RequiresIsolation => "requires_isolation",
        }
    }
}

/// The resolved policy for one run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ToolPolicy {
    /// The mode applied. A `RemoteUser` run is always `strict`.
    pub mode: ToolPolicyMode,
    pub trust: RunTrust,
    /// Whether [`apply`](Self::apply) narrows the registry. False in `off`
    /// and `report_only`.
    pub enforced: bool,
    /// The tools the run keeps.
    pub allowed: BTreeSet<String>,
    /// The tools withheld (or, in `report_only`, that would be), with why.
    pub withheld: Vec<(String, Withheld)>,
    /// `toolsets` entries that match nothing because they are not
    /// recognized. Never treated as `all`.
    pub invalid_entries: Vec<String>,
}

impl ToolPolicy {
    /// The registry the run is built with: narrowed when enforced, unchanged
    /// otherwise.
    pub fn apply(&self, registry: &ToolRegistry) -> ToolRegistry {
        if self.enforced {
            registry.scoped(&self.allowed.iter().cloned().collect::<Vec<_>>())
        } else {
            registry.clone()
        }
    }

    /// The tools a delegated worker may be offered: the worker set narrowed to
    /// this run's tools when enforced, so delegation never resets a run to
    /// installation-wide defaults.
    pub fn worker_registry(&self, worker_default: &ToolRegistry) -> ToolRegistry {
        if self.enforced {
            worker_default.scoped(&self.allowed.iter().cloned().collect::<Vec<_>>())
        } else {
            worker_default.clone()
        }
    }

    /// A one-line, content-free summary for logs, or `None` when nothing is
    /// or would be withheld.
    pub fn summary(&self, profile: &str) -> Option<String> {
        if self.withheld.is_empty() && self.invalid_entries.is_empty() {
            return None;
        }
        let withheld = self
            .withheld
            .iter()
            .map(|(name, why)| format!("{name} ({})", why.as_str()))
            .collect::<Vec<_>>()
            .join(", ");
        let verb = if self.enforced {
            "withholds"
        } else {
            "would withhold"
        };
        let mut line = format!(
            "tool policy ({}) {verb} for profile {profile}: {}",
            self.mode.as_str(),
            if withheld.is_empty() {
                "nothing"
            } else {
                &withheld
            }
        );
        if !self.invalid_entries.is_empty() {
            line.push_str(&format!(
                "; unrecognized toolsets entries: {}",
                self.invalid_entries.len()
            ));
        }
        Some(line)
    }
}

/// Resolve the policy for a run over `registry`, the tools its settings and
/// runtime enable.
pub fn resolve(
    registry: &ToolRegistry,
    toolsets: &[String],
    configured: ToolPolicyMode,
    trust: RunTrust,
    network_mcp_servers: &BTreeSet<String>,
) -> ToolPolicy {
    // An untrusted run can never be evaluated more loosely than strict.
    let mode = match trust {
        RunTrust::RemoteUser => ToolPolicyMode::Strict,
        RunTrust::LocalOperator => configured,
    };
    let names = registry.names();
    if mode == ToolPolicyMode::Off {
        return ToolPolicy {
            mode,
            trust,
            enforced: false,
            allowed: names.into_iter().collect(),
            withheld: Vec::new(),
            invalid_entries: Vec::new(),
        };
    }
    let strict = mode == ToolPolicyMode::Strict;
    let mut entries = Vec::new();
    let mut invalid_entries = Vec::new();
    for raw in toolsets {
        match parse_entry(raw.trim()) {
            Some(Entry::All) if strict => invalid_entries.push(raw.clone()),
            Some(entry) => entries.push(entry),
            None => invalid_entries.push(raw.clone()),
        }
    }
    // Strict fails closed on any entry it cannot read.
    let poisoned = strict && !invalid_entries.is_empty();
    let unrestricted = !strict && toolsets.is_empty();
    let listed = |name: &str| {
        unrestricted
            || entries.iter().any(|entry| match entry {
                Entry::All => true,
                Entry::ReadOnly => registry.get(name).is_some_and(|tool| {
                    tool.definition().risk == lightagent_core::RiskClass::Observe
                }),
                Entry::Namespace(prefix) => name.starts_with(prefix.as_str()),
                Entry::Exact(exact) => exact == name,
            })
    };
    let mut allowed = BTreeSet::new();
    let mut withheld = Vec::new();
    for name in names {
        let reason = if poisoned {
            Some(Withheld::InvalidProfileToolsets)
        } else if trust == RunTrust::RemoteUser
            && reach_of(&name, network_mcp_servers).requires_isolation()
        {
            Some(Withheld::RequiresIsolation)
        } else if !listed(&name) {
            Some(Withheld::NotInProfile)
        } else {
            None
        };
        match reason {
            Some(reason) => withheld.push((name, reason)),
            None => {
                allowed.insert(name);
            }
        }
    }
    let enforced = mode != ToolPolicyMode::ReportOnly;
    if !enforced {
        // Report-only keeps every tool and only records what would go.
        allowed = registry.names().into_iter().collect();
    }
    ToolPolicy {
        mode,
        trust,
        enforced,
        allowed,
        withheld,
        invalid_entries,
    }
}

#[cfg(test)]
mod tests;
