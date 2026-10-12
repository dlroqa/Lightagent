# Proposal: enforceable profile toolsets

Status: **proposal only.** Nothing here is implemented. It answers the
authorization gap recorded in `jev-coordination-audit.md` (finding 6) and must
be approved separately before any runtime change.

## Current behavior

`AgentProfile.toolsets` (`lightagent-core/src/profile.rs`) is stored in each
profile manifest but is read by no runtime code. A run's tools are decided by:

1. **Availability:** `configured_registry` (`crates/lightagent/src/chat.rs`)
   builds one registry from global settings: built-ins enabled by `tools.*`
   and `web.*`, MCP servers, RAG, memory and skills.
2. **Authorization:** `BoundedExecutor` asks `PolicyEngine` built from the
   profile's `approval_policy`. Mutating and executable tools ask on every
   call, and privileged tools always block (`docs/permissions.md`).

Every profile therefore sees the same tool list. Lightagent's setup, settings
and subagent provisioning all create profiles with `toolsets = []`. No shipped
code path writes a non-empty value.

## What Jev may do meanwhile

Jev classification must never add, remove or relax tools or approvals. Until
this proposal lands, an "authorized workflow" means one whose tools are already
in the run's registry *and* would pass `PolicyEngine` unchanged. A
classification can only narrow emphasis among those tools, never widen it.

## Proposed semantics

- `toolsets = []` (the default and every existing Lightagent-created profile)
  means **no restriction**. This is exactly today's behavior.
- A non-empty list is an allow-list applied **after** `configured_registry`
  with `ToolRegistry::scoped`. It can only remove tools, never add one, so it
  cannot grant anything the global settings and approval policy do not already
  allow.
- Entries may be:
  - an exact tool name (`fs.read`);
  - a namespace wildcard (`fs.*`, `mcp.<server>.*`);
  - a named group: `read-only` (tools whose risk class is `Observe`) or `all`.
- Unknown entries match nothing and are reported by `lightagent tools list`
  and the Tools screen. They are never treated as `all`.
- The same filter applies to `GET /tools`, run execution, and delegated
  workers. A worker's registry is the intersection of the worker default, the
  delegating run's registry and the worker profile's toolsets. The approval
  policy still applies to every remaining call.

## Rollout (compatibility-safe)

1. **Report only.** `lightagent tools list` and the Tools screen show, for
   the active profile, which tools a non-empty `toolsets` *would* hide.
   Behavior is unchanged.
2. **Opt in.** A new `tools.enforce_profile_toolsets` setting, default
   `false`. When it is `true`, non-empty lists are enforced. Existing configs
   deserialize unchanged.
3. **Default on**, in a later minor release with a changelog entry, once
   report-only output has been available. Profiles with `toolsets = []`
   remain unaffected at every step.

Effect on existing profiles: none at steps 1–2. At step 3, only a profile
whose manifest someone edited by hand to a non-empty list changes, and it can
only lose tools.

## Tests required when implemented

- Empty list → identical registry to today, for runs, `GET /tools` and workers.
- Non-empty list → only matching tools are offered, and an unlisted tool call
  is refused even if the model names it.
- A list can never add a tool absent from `configured_registry`.
- Approval prompts are unchanged for every remaining tool.
- A Jev classification has no effect on the resulting registry.
