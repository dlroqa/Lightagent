//! The resolver over synthetic registries and profiles.

use std::collections::BTreeSet;
use std::sync::Arc;

use async_trait::async_trait;
use lightagent_core::identity::RunTrust;
use lightagent_core::{RiskClass, ToolOutcome, ToolPolicyMode};
use serde_json::{Value, json};

use super::*;
use crate::context::ToolCtx;
use crate::definition::{Tool, ToolDefinition};

/// A stand-in for tools defined outside this crate (MCP, memory, RAG, Jev).
struct Named(ToolDefinition);

#[async_trait]
impl Tool for Named {
    fn definition(&self) -> &ToolDefinition {
        &self.0
    }

    async fn call(&self, _args: &Value, _ctx: &ToolCtx) -> ToolOutcome {
        ToolOutcome::ok("ran")
    }
}

fn named(name: &str, risk: RiskClass) -> Arc<dyn Tool> {
    Arc::new(Named(ToolDefinition::new(
        name,
        "synthetic",
        json!({"type": "object"}),
        risk,
        vec![],
    )))
}

/// Every built-in, plus memory, RAG, Jev, an HTTP MCP server (`docs`) and a
/// stdio MCP server (`shell`).
fn full_registry() -> ToolRegistry {
    let mut registry = ToolRegistry::builtin();
    for (name, risk) in [
        ("memory.search", RiskClass::Observe),
        ("memory.write", RiskClass::Mutating),
        ("rag.search", RiskClass::Observe),
        ("rag.realtime", RiskClass::External),
        ("jev.classify_task", RiskClass::External),
        ("mcp.docs.lookup", RiskClass::External),
        ("mcp.shell.exec", RiskClass::Executable),
    ] {
        registry.insert(named(name, risk));
    }
    registry
}

/// MCP servers reached over HTTP: `docs`. `shell` is a stdio server.
fn network() -> BTreeSet<String> {
    BTreeSet::from(["docs".to_owned()])
}

fn lists(entries: &[&str]) -> Vec<String> {
    entries.iter().map(|entry| (*entry).to_owned()).collect()
}

fn resolve_with(
    registry: &ToolRegistry,
    toolsets: &[&str],
    mode: ToolPolicyMode,
    trust: RunTrust,
) -> ToolPolicy {
    resolve(registry, &lists(toolsets), mode, trust, &network())
}

fn allowed(policy: &ToolPolicy) -> Vec<&str> {
    policy.allowed.iter().map(String::as_str).collect()
}

const OPERATOR: RunTrust = RunTrust::LocalOperator;
const USER: RunTrust = RunTrust::RemoteUser;

#[test]
fn off_is_todays_behavior_whatever_the_profile_lists() {
    let registry = full_registry();
    for toolsets in [&[][..], &["datetime.now"][..], &["not a tool"][..]] {
        let policy = resolve_with(&registry, toolsets, ToolPolicyMode::Off, OPERATOR);
        assert!(!policy.enforced);
        assert_eq!(policy.allowed, registry.names().into_iter().collect());
        assert!(policy.withheld.is_empty());
        assert_eq!(policy.apply(&registry).names(), registry.names());
        assert_eq!(
            policy
                .worker_registry(&ToolRegistry::worker_default())
                .names(),
            ToolRegistry::worker_default().names()
        );
    }
}

#[test]
fn report_only_keeps_every_tool_and_reports_what_enforce_would_remove() {
    let registry = full_registry();
    let report = resolve_with(&registry, &["web.*"], ToolPolicyMode::ReportOnly, OPERATOR);
    let enforce = resolve_with(&registry, &["web.*"], ToolPolicyMode::Enforce, OPERATOR);
    assert!(!report.enforced);
    assert_eq!(report.apply(&registry).names(), registry.names());
    assert_eq!(
        report.withheld, enforce.withheld,
        "the report is exactly what enforce removes"
    );
    let summary = report.summary("lead").unwrap();
    assert!(
        summary.starts_with("tool policy (report_only) would withhold for profile lead:"),
        "{summary}"
    );
    assert!(summary.contains("fs.write (not_in_profile)"), "{summary}");
}

#[test]
fn global_enabled_and_profile_denied_is_withheld() {
    let registry = full_registry();
    let policy = resolve_with(
        &registry,
        &["datetime.now", "web.fetch"],
        ToolPolicyMode::Enforce,
        OPERATOR,
    );
    assert_eq!(allowed(&policy), ["datetime.now", "web.fetch"]);
    let applied = policy.apply(&registry);
    assert!(!applied.contains("terminal.run"));
    assert!(
        policy
            .withheld
            .contains(&("terminal.run".to_owned(), Withheld::NotInProfile))
    );
}

#[test]
fn global_disabled_and_profile_allowed_is_never_added() {
    // The settings enabled only datetime.now; the profile lists more.
    let registry = ToolRegistry::new().with(named("datetime.now", RiskClass::Observe));
    for mode in [ToolPolicyMode::Enforce, ToolPolicyMode::Strict] {
        let policy = resolve_with(
            &registry,
            &["terminal.run", "fs.*", "datetime.now"],
            mode,
            OPERATOR,
        );
        assert_eq!(allowed(&policy), ["datetime.now"], "{mode:?}");
        assert_eq!(policy.apply(&registry).names(), ["datetime.now"]);
    }
}

#[test]
fn both_global_and_profile_allowed_is_kept() {
    let registry = full_registry();
    let policy = resolve_with(
        &registry,
        &["fs.*", "mcp.docs.*"],
        ToolPolicyMode::Enforce,
        OPERATOR,
    );
    assert_eq!(
        allowed(&policy),
        ["fs.list", "fs.read", "fs.write", "mcp.docs.lookup"]
    );
}

#[test]
fn an_empty_toolset_is_unrestricted_only_outside_strict() {
    let registry = full_registry();
    let legacy = resolve_with(&registry, &[], ToolPolicyMode::Enforce, OPERATOR);
    assert_eq!(
        legacy.allowed.len(),
        registry.names().len(),
        "enforce: [] keeps today's tools"
    );
    let strict = resolve_with(&registry, &[], ToolPolicyMode::Strict, OPERATOR);
    assert!(strict.allowed.is_empty(), "strict: [] offers nothing");
    assert!(strict.apply(&registry).names().is_empty());
    assert!(
        strict
            .withheld
            .iter()
            .all(|(_, why)| *why == Withheld::NotInProfile)
    );
}

#[test]
fn unrecognized_entries_match_nothing_and_poison_strict() {
    let registry = full_registry();
    let invalid = [
        "Terminal.Run",
        "fs.",
        "*",
        "fs.**",
        "",
        "../fs.read",
        "web .fetch",
        "nodot",
    ];
    for entry in invalid {
        let enforce = resolve_with(
            &registry,
            &["datetime.now", entry],
            ToolPolicyMode::Enforce,
            OPERATOR,
        );
        assert_eq!(allowed(&enforce), ["datetime.now"], "enforce {entry:?}");
        assert_eq!(enforce.invalid_entries, [entry.to_owned()]);
        let strict = resolve_with(
            &registry,
            &["datetime.now", entry],
            ToolPolicyMode::Strict,
            OPERATOR,
        );
        assert!(
            strict.allowed.is_empty(),
            "strict fails closed on {entry:?}"
        );
        assert!(
            strict
                .withheld
                .iter()
                .all(|(_, why)| *why == Withheld::InvalidProfileToolsets)
        );
    }
    // `all` is a valid entry in enforce and refused in strict.
    let enforce = resolve_with(&registry, &["all"], ToolPolicyMode::Enforce, OPERATOR);
    assert_eq!(enforce.allowed.len(), registry.names().len());
    let strict = resolve_with(&registry, &["all"], ToolPolicyMode::Strict, OPERATOR);
    assert!(strict.allowed.is_empty());
    assert_eq!(strict.invalid_entries, ["all"]);
}

#[test]
fn read_only_selects_observe_tools() {
    let registry = full_registry();
    let policy = resolve_with(&registry, &["read-only"], ToolPolicyMode::Strict, OPERATOR);
    for name in allowed(&policy) {
        assert_eq!(
            registry.get(name).unwrap().definition().risk,
            RiskClass::Observe,
            "{name}"
        );
    }
    assert!(policy.allowed.contains("fs.read"));
    assert!(!policy.allowed.contains("fs.write"));
}

#[test]
fn an_untrusted_run_is_strict_and_never_gets_unisolated_local_execution() {
    let registry = full_registry();
    // Whatever the configured mode, and even when the profile lists them.
    for configured in [
        ToolPolicyMode::Off,
        ToolPolicyMode::ReportOnly,
        ToolPolicyMode::Enforce,
        ToolPolicyMode::Strict,
    ] {
        let listing_everything = [
            "fs.*",
            "terminal.run",
            "open_terminal.run",
            "agent.delegate",
            "mcp.shell.*",
            "mcp.docs.*",
            "web.*",
            "datetime.now",
            "memory.search",
        ];
        let policy = resolve_with(&registry, &listing_everything, configured, USER);
        assert_eq!(policy.mode, ToolPolicyMode::Strict, "{configured:?}");
        assert!(policy.enforced);
        for name in [
            "fs.read",
            "fs.list",
            "fs.write",
            "terminal.run",
            "open_terminal.run",
            "agent.delegate",
            "mcp.shell.exec",
        ] {
            assert!(!policy.allowed.contains(name), "{configured:?}: {name}");
            assert!(
                policy
                    .withheld
                    .contains(&(name.to_owned(), Withheld::RequiresIsolation))
            );
        }
        assert_eq!(
            allowed(&policy),
            [
                "datetime.now",
                "mcp.docs.lookup",
                "memory.search",
                "web.fetch",
                "web.search"
            ]
        );
        // An empty list gives an untrusted run nothing at all.
        let empty = resolve_with(&registry, &[], configured, USER);
        assert!(empty.allowed.is_empty());
    }
}

#[test]
fn unknown_tools_are_classified_as_local_commands() {
    let none = BTreeSet::new();
    assert_eq!(reach_of("custom.thing", &none), ToolReach::LocalCommand);
    assert_eq!(reach_of("mcp.malformed", &none), ToolReach::LocalCommand);
    assert_eq!(
        reach_of("mcp.docs.lookup", &none),
        ToolReach::LocalCommand,
        "an MCP server not known to be HTTP is treated as local"
    );
    assert_eq!(
        reach_of("mcp.shell.exec", &network()),
        ToolReach::LocalCommand
    );
    assert_eq!(reach_of("mcp.docs.lookup", &network()), ToolReach::Network);
    let registry = ToolRegistry::new().with(named("custom.thing", RiskClass::Observe));
    let policy = resolve_with(&registry, &["custom.thing"], ToolPolicyMode::Strict, USER);
    assert!(
        policy.allowed.is_empty(),
        "an unknown tool is never offered to an untrusted run"
    );
}

#[test]
fn the_worker_registry_never_exceeds_the_run() {
    let registry = full_registry();
    let policy = resolve_with(
        &registry,
        &["datetime.now", "web.fetch"],
        ToolPolicyMode::Enforce,
        OPERATOR,
    );
    let worker = policy.worker_registry(&ToolRegistry::worker_default());
    assert_eq!(worker.names(), ["datetime.now", "web.fetch"]);
    for name in worker.names() {
        assert!(policy.allowed.contains(&name), "{name}");
    }
    let untrusted = resolve_with(&registry, &["web.*"], ToolPolicyMode::Strict, USER);
    assert!(
        !untrusted
            .worker_registry(&ToolRegistry::worker_default())
            .contains("fs.read")
    );
}

#[test]
fn summaries_hold_tool_names_and_codes_only() {
    let registry = full_registry();
    let policy = resolve_with(
        &registry,
        &["datetime.now", "Bad Entry!"],
        ToolPolicyMode::Enforce,
        OPERATOR,
    );
    let summary = policy.summary("lead").unwrap();
    assert!(
        summary.contains("unrecognized toolsets entries: 1"),
        "{summary}"
    );
    assert!(
        !summary.contains("Bad Entry!"),
        "entries are counted, not echoed: {summary}"
    );
    let quiet = resolve_with(&registry, &[], ToolPolicyMode::Enforce, OPERATOR);
    assert_eq!(quiet.summary("lead"), None);
}
