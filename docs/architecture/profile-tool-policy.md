# Per-profile tool policy and execution isolation

Status: **tool policy implemented; execution isolation designed, not built.**
This implements
[`profile-toolset-enforcement-proposal.md`](profile-toolset-enforcement-proposal.md)
(item 4 of [`jev-security-implementation-plan.md`](jev-security-implementation-plan.md))
and closes blocker 1 of
[`profile-ownership-architecture.md`](profile-ownership-architecture.md#multi-user-readiness-and-blockers)
for tool *selection*. It does **not** isolate execution. Tools that run do so
as the server's OS account.

Four boundaries, each enforced on its own:

| Boundary | Decides | Where |
| --- | --- | --- |
| Identity | Who acts | `AuthConfig` / `IdentityAuthority` (#15, #16) |
| Profile ownership | Which profile they may operate | `identity::authorize`, `RunOwner` (#16) |
| **Tool policy** | **Which tools that profile's run may use** | **`lightagent_tools::policy` (this change)** |
| Execution isolation | Which OS resources those tools reach | Not built. Until it exists, unisolated local execution is withheld from untrusted runs. |

## Audit: tool registry and execution paths

| Area | Source | Behavior | Finding |
| --- | --- | --- | --- |
| Registry assembly | `chat::configured_registry`, `configured_builtin_registry` | Built-ins gated by `web.*`, `tools.enabled` and `tools.allow_terminal`; plus MCP tools (`mcp_tools`), `rag.search`, `rag.realtime`, `memory.*`, `session.lookup` | This is the global and runtime-capability part. It only adds tools that are enabled and available. |
| Jev explicit tool | `coordinator::add_explicit_tool` | Adds `jev.classify_task` for permitted profiles | Added after the registry. The policy runs after it, so the tool obeys `toolsets`. |
| Registry builders | `serve.rs` `tools()` and `run()`; `chat.rs` `build_chat_runtime`; `lib.rs` `tools_list` | Each builds the registry | All four now apply the policy |
| Execution | `BoundedExecutor` (`lightagent-tools/src/executor.rs`) | `schemas()` is what the model sees. `invoke` refuses a name not in the registry ("is not available"). `approval_for` asks `PolicyEngine`. | Narrowing the registry closes both the declaration and the invocation path. A model naming a withheld tool is refused. |
| Approvals | `PolicyEngine` from `AgentProfile.approval_policy` | Mutating and executable calls ask; privileged calls are blocked | Independent of the policy and unchanged (tested) |
| `AgentProfile.toolsets` | `lightagent-core/src/profile.rs` | Stored and, before this change, read by nothing | Now the profile half of the policy |
| Filesystem | `fs.read`, `fs.list`, `fs.write` + `WorkspaceContext` (`workspace.rs`) | Confined to the workspace root by path canonicalization in code | Runs as the server's OS account: confinement by code, **not OS isolation** |
| Commands | `terminal.run` (`tools.allow_terminal`, `terminal_allowlist`, `blocked_command`) | A local subprocess as the server's OS account | Not isolated |
| Remote commands | `open_terminal.run` | Runs in the installation's shared Open Terminal service | Isolated from this host, but shared across profiles, not per-profile |
| Network | `web.fetch`, `web.search`, `rag.realtime`, `jev.classify_task`, MCP over HTTP | Outbound HTTP with the existing SSRF guard | No local OS reach |
| MCP | `mcp.<server>.<tool>`; servers from `mcp.servers` and extensions | Stdio servers are local processes as the OS account; HTTP servers are remote | Stdio tools are local commands |
| Profile data | `memory.*`, `session.lookup`, `rag.search`, `skill.read` | Files under the profile directory or skill dirs | Confined by path construction, as the OS account |
| Delegation | `agent.delegate` (`builtins/delegate.rs`), `Delegation` (`context.rs`) | A worker run under a profile the model names, with `worker_registry` (was `ToolRegistry::worker_default()`, every built-in except delegate) and the **worker profile's** approval policy and limits | Before this change, workers could be offered tools the run lacked (inert without contexts, but advertised), and ran under the worker profile's policy and limits. Now narrowed when enforced (below). |
| ACP | `lightagent-acp` → `RunManager::start` → `run()` | A local operator run | Policy applies with `LocalOperator` trust |
| Run budgets | `RunLimits`; `intersect_caps` for workers | Worker limits = worker profile limits ∩ caller caps | Now also bounded by the originating run's limits when enforced |
| Ownership | `RunOwner` (#16) | Records principal and profile | Now also records the run's `RunTrust`, passed to the runtime through `RunFactory::run_as` |

## Policy

```
effective = registry (global settings ∩ runtime capabilities)
          ∩ profile toolsets, by mode
          ∩ what the run's trust allows without execution isolation
```

`lightagent_tools::policy::resolve(registry, toolsets, mode, trust,
network_mcp_servers) -> ToolPolicy`. `ToolPolicy::apply` builds the run's
registry. It can only remove tools.

**`toolsets` entries**: an exact name (`fs.read`), a namespace (`fs.*`,
`mcp.docs.*`), `read-only` (risk class `observe`), or `all` (not accepted in
strict). Anything else is an unrecognized entry. It matches nothing and is
never read as `all`.

**Modes** (`tools.profile_policy`, CLI `lightagent config set
tools.profile_policy <mode>`):

| Mode | Empty `toolsets` | Non-empty `toolsets` | Unrecognized entry | Effect on runs |
| --- | --- | --- | --- | --- |
| `off` (default, every existing and new configuration) | every enabled tool | ignored | ignored | Today's behavior |
| `report_only` | every enabled tool | evaluated, not applied | reported | Unchanged. One content-free line names what enforcement would withhold. |
| `enforce` | every enabled tool | only matching tools | matches nothing | Narrowed |
| `strict` | **no tools** | only matching tools; `all` refused | **every tool withheld** (fail closed) | Narrowed |

**Run trust.** A run started by an ordinary multi-user API user
(`RunTrust::RemoteUser`) is **always strict**, whatever the configured mode.
It is also **never offered** tools that act through the server's OS account
or another profile, even when its profile lists them: `fs.*`, `terminal.run`,
`open_terminal.run`, `agent.delegate`, stdio MCP tools, and any unrecognized
tool. Withheld with reason `requires_isolation`. Terminal, ACP, the
single-user API and operators run as `LocalOperator`.

**Tool classification** (`reach_of`) is by tool identity, not risk class.
`fs.read` is `observe` risk but reads files as the OS account. An MCP tool
counts as network only if its server is configured as HTTP; any other is a
local command. Classification errors can only withhold a tool, never expose
one.

**Failing closed.**
- A runtime that cannot confine tools refuses untrusted runs: the default
  `RunFactory::run_as` fails a `RemoteUser` run before it starts.
- The call guard refuses when the configuration or profile cannot be read.
- Strict refuses on unrecognized entries.
- An unknown mode fails configuration loading.

## Enforcement points

1. **Registry construction** (all four builders): the model sees only
   effective tools, and the executor holds only those, so direct invocation of
   a withheld tool is refused ("is not available").
2. **Per call** (`BoundedExecutor::with_call_guard`, installed for enforced
   policies): before each call the guard re-reads the configuration and the
   profile and re-resolves the policy. A tool withdrawn mid-run is denied before
   any approval prompt and at invocation ("no longer permitted for this run").
   `off` and `report_only` runs install no guard. A change takes effect at
   their next run, as before.
3. **Delegation** (enforced policies): the worker registry is the run's
   effective tools ∩ the worker defaults. Worker limits are capped by the
   originating run's (`limit_ceiling`). The worker's approval policy is the
   stricter of its own and the run's (`approval_floor`). Nested delegation
   inherits all three (`Delegation::child_context`). Untrusted runs get no
   `agent.delegate` at all.

## Authorization and approval interaction

- Ownership decides *whether* a caller may run a profile. Tool policy decides
  *what* that run may use. **Owning a profile grants no tool, and identity
  alone grants nothing:** an untrusted run with an empty `toolsets` has no
  tools.
- Approvals are applied after the policy, by the unchanged `PolicyEngine`.
  The policy never weakens an approval. Tested: for each of permissive,
  balanced and strict, every permitted tool has the same approval need with or
  without the policy.
- **Jev.** Classification runs before the registry is built. The workflow
  policy inspects the *effective* registry, so research guidance is
  `not_configured` when the profile withholds retrieval tools (tested). A
  classification cannot add a tool, change an approval, widen files, enable a
  terminal, raise delegation, or grant network access. `jev.classify_task`
  itself obeys `toolsets`. Lightweight is unchanged.

## Execution isolation: actual limitations

**Every local tool runs as the Lightagent server's OS account.**
- `fs.*`: workspace confinement is canonicalize-and-check in code. It
  protects against path mistakes, not against a determined process; there is
  no OS sandbox.
- `terminal.run`: a direct subprocess with the server's privileges,
  environment and network. The allowlist and blocked patterns are guards, not
  isolation.
- Stdio MCP servers: local processes with the server's privileges.
- Profile data tools: confined by path construction.
- `open_terminal.run`: isolated from this host, but one service is shared by
  all profiles.

**None of these are tenant-isolated, and this change does not claim they
are.** That is why untrusted runs never receive them.

## Future sandboxed execution (separate plan)

To be built as its own PRs before any local execution is offered to
untrusted runs:

1. **Execution broker.** Local tools call a `SandboxExecutor` trait instead
   of acting directly. The default `InProcess` implementation keeps today's
   behavior for local operators, and `Isolated` implementations serve
   untrusted runs.
2. **Per-profile filesystem roots.** One root per profile, mounted only into
   that profile's worker. Opened with `openat2(RESOLVE_BENEATH |
   RESOLVE_NO_SYMLINKS)` or the platform equivalent; no traversal or symlink
   escape. Read-only mounts by default.
3. **Command isolation.** A worker per run or profile, launched in a sandbox
   (Linux: user namespaces with seccomp, Landlock, cgroups v2; macOS: sandbox
   profile; Windows: AppContainer or job objects). An option for an external
   runner (container or microVM) behind the same trait. No container
   orchestration in Lightagent itself.
4. **Resource limits.** CPU, memory, process count, wall clock and output
   bytes per worker, enforced by the OS.
5. **Network.** Off by default. An egress allowlist when enabled.
6. **Environment and secrets.** Workers receive no server environment and no
   provider keys. Only secrets explicitly bound to the profile, injected per
   call.
7. **Lifecycle.** Workers are torn down at run end, cancellation, or
   credential/ownership revocation. Temporary files are removed. A watchdog
   handles stragglers.
8. **Profile-bound delegation.** A worker profile must be owned by, or
   granted to, the run's principal. Workers inherit the run's sandbox
   restrictions.
9. **Revocation.** The ownership guard from #16 cancels in-flight untrusted
   runs when a credential or ownership is revoked.
10. **Tests.** Escape attempts (`..`, symlinks, `/proc`, environment
    probing), resource exhaustion, network egress, and cancellation teardown,
    on GitHub-hosted runners of all three platforms.

Only after a tool runs through an `Isolated` executor may its reach be
treated as not requiring isolation for untrusted runs.

## Tests

| Requirement | Test |
| --- | --- |
| Global enabled, profile denied | `global_enabled_and_profile_denied_is_withheld` |
| Global disabled, profile allowed | `global_disabled_and_profile_allowed_is_never_added` |
| Both allowed | `both_global_and_profile_allowed_is_kept`; serve `an_enforced_profile_policy_shapes_the_provider_request` |
| Empty toolset, legacy vs strict | `an_empty_toolset_is_unrestricted_only_outside_strict`; serve enforced test |
| Invalid identifiers | `unrecognized_entries_match_nothing_and_poison_strict` |
| Direct invocation bypass | `a_withheld_tool_cannot_be_invoked_by_naming_it` |
| Runtime policy changes | `a_call_guard_withdraws_a_tool_mid_run`, `the_tool_policy_guard_follows_the_current_profile_and_fails_closed` |
| Approvals preserved | `approval_requirements_are_unchanged_for_permitted_tools` |
| Restricted filesystem / terminal unavailable to untrusted runs | `an_untrusted_run_is_strict_and_never_gets_unisolated_local_execution`, serve `an_untrusted_run_never_receives_unisolated_local_execution` |
| MCP restrictions | stdio vs HTTP classification in the untrusted and `unknown_tools_are_classified_as_local_commands` tests |
| Delegation inheritance | `a_worker_is_offered_no_tool_its_run_was_denied`, `a_worker_runs_under_the_stricter_of_its_and_its_runs_approval_policy`, `a_worker_never_exceeds_its_runs_limits`, `nested_delegation_inherits_every_restriction`, `the_worker_registry_never_exceeds_the_run` |
| Cross-profile tool access | Policy uses the run's resolved profile only. Untrusted runs cannot delegate to another profile. Ownership tests (#16) cover running another profile. |
| Jev under denied tools | serve `jev_workflow_eligibility_follows_the_effective_tools` |
| Cancellation and revocation | the call guard tests; untrusted runs refused by a non-confining runtime (`a_runtime_that_cannot_confine_tools_refuses_untrusted_runs`); existing cancellation tests unchanged |
| Run trust reaches the runtime | `a_users_run_reaches_the_runtime_as_untrusted_and_an_operators_as_local` |
| Single-user compatibility | `off_is_todays_behavior_whatever_the_profile_lists`, serve `off_ignores_toolsets_and_report_only_changes_nothing`, every existing test unchanged |
| Configuration | `the_tool_policy_mode_key_round_trips_and_rejects_unknown_values` |

## Migration and rollback

- **Existing installations:** none. `tools.profile_policy` loads as `off`,
  and every profile keeps exactly its current tools.
- **Adopting it:**
  1. `lightagent config set tools.profile_policy report_only`, then watch the
     `tool policy (report_only) would withhold …` lines from `serve`, the
     terminal and `lightagent tools list`.
  2. Edit each profile's `toolsets` (in its profile manifest).
  3. Switch to `enforce`, or `strict` where every profile has an explicit
     list.
- **Rollback:** `lightagent config set tools.profile_policy off` restores
  today's tools at the next run. Reverting the code removes the policy and the
  delegation narrowing. Untrusted runs cannot exist without multi-user mode,
  which remains unactivatable, so no rollback re-exposes tools to them.

## Remaining blockers for multi-user activation

1. **Execution isolation** (plan above). Until then, untrusted runs get no
   local files, commands, stdio MCP or delegation, by design.
2. Identity store, persistence and CLI (credentials, owners, grants), and an
   activation command.
3. Sessions, `/profiles`, `/jev`, `/tools` scoped to the authorized profile.
   They are operator-only in multi-user mode today.
4. A persistent audit log.
5. Cancelling an untrusted run on credential or ownership revocation. Reads
   stop today; the run itself continues to completion.
6. TLS and trusted ingress for external exposure.
7. The provisioning API.
8. Rate limiting of failed authentications.
