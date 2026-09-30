# Subagent integration map

This is the Phase 0 baseline for the hierarchical-subagent upgrade. It is a
source-derived map of the checkout, not a proposed replacement architecture.

## Baseline captured

| Item | Observed value |
| --- | --- |
| Harness repository | `https://github.com/dlroqa/Lightagent.git` (`origin`) |
| Harness commit | `e7fd88f2edcd986017a16fe92e959163f6716a36` — `Merge pull request #2 from dlroqa/chore/isolate-runtime-control-plane` (2026-09-29T15:44:26-07:00) |
| Workspace version | `0.4.0`; Rust edition 2024; MSRV `1.98` ([Cargo.toml](../../Cargo.toml)) |
| Gateway repository configured as a remote | `https://github.com/dlroqa/Lightweight.git` (`lightweight-source`); no Lightweight source checkout is present in this workspace, so its SHA and implementation language are not verifiable here. |
| Gateway integration actually used | The Rust crate `lightagent-provider-lightweight` speaks to a configured OpenAI-compatible Lightweight endpoint; Lightagent does not embed or execute the gateway. |
| UI package | `lightagent-web` `0.4.0`; React `19`, React Router `7`, Vite `6`, TypeScript `5.7` ([frontend/package.json](../../frontend/package.json)) |
| Dirty state at capture (before this document) | Modified, uncommitted files: `Cargo.lock`, `Cargo.toml`, `README.md`, `crates/lightagent-acp/src/server.rs`, `crates/lightagent-api/src/{lib.rs,manager.rs}`, `crates/lightagent-api/tests/api.rs`, `crates/lightagent-core/src/{config.rs,lib.rs,paths.rs}`, `crates/lightagent/src/{lib.rs,serve.rs,setup.rs}`, `docs/architecture.md`, `frontend/src/api/agent.ts`, `frontend/src/screens/Agent.tsx`, and `frontend/src/state/preferences.tsx`. Preserve these in-progress changes. |

The implementation guide's suggested Python execution plane diverges from the
verified system: the harness, HTTP gateway adapter, runtime, and execution
plane are all Rust/Tokio. The chosen adapter is therefore the existing Rust
`RunFactory` / `RunManager` / `AgentLoop` path, rather than a Python adapter.
The only currently versioned public run protocol is HTTP `/api/lightagent/v1`.

## Startup and entry points

The binary entry point is [crates/lightagent/src/bin/lightagent.rs](../../crates/lightagent/src/bin/lightagent.rs), which calls `lightagent::run_cli()`.
Command parsing and dispatch live in [crates/lightagent/src/lib.rs](../../crates/lightagent/src/lib.rs).

| Surface | Command / entry point | Source |
| --- | --- | --- |
| Terminal chat | `lightagent` (interactive terminal) or `lightagent chat [--profile ID] [--session ID]` | `chat::run` in [crates/lightagent/src/chat.rs](../../crates/lightagent/src/chat.rs) |
| HTTP API | `lightagent serve [--host 127.0.0.1] [--port 8735] [--key-env ENV] [--web-root frontend/dist]` | `serve::run` in [crates/lightagent/src/serve.rs](../../crates/lightagent/src/serve.rs) |
| Browser UI development | `npm run dev --prefix frontend`; Vite proxies `/api/lightagent` to `http://127.0.0.1:8735` (or `LIGHTAGENT_DEV_ORIGIN`) | [frontend/vite.config.ts](../../frontend/vite.config.ts) |
| Browser UI production | `npm run build --prefix frontend` then `lightagent serve --web-root frontend/dist` | [README.md](../../README.md) |
| Editor protocol | `lightagent acp` (stdio JSON-RPC) | [crates/lightagent-acp/src/server.rs](../../crates/lightagent-acp/src/server.rs) |
| Provider catalog | `lightagent models` | `models` in [crates/lightagent/src/lib.rs](../../crates/lightagent/src/lib.rs) |
| Profile/session management | `lightagent profile`/`profiles` and `lightagent sessions` | [crates/lightagent/src/lib.rs](../../crates/lightagent/src/lib.rs) |

`serve` constructs `LightweightRunFactory`, `RunManager`, `AppState`, and the
Axum router. It runs `axum::serve` until the listener exits; there is no
verified graceful-drain or durable scheduler shutdown hook today. The ACP
server uses the same `RunManager`; each editor prompt starts a managed run and
`session/cancel` reaches that run's cancellation token.

## Existing execution and transport path

```text
HTTP POST /runs or ACP session/prompt
  -> lightagent_api::RunManager::start
  -> tokio background task / RunFactory::run
  -> Lightagent serve::LightweightRunFactory
  -> AgentLoop + BoundedExecutor + LightweightProvider
  -> AgentEventSink
  -> in-memory RunState event buffer + SSE / ACP projection
```

`AgentLoop` is the verified native async execution primitive
([crates/lightagent-core/src/loop_.rs](../../crates/lightagent-core/src/loop_.rs)).
It streams provider turns, invokes tools, pauses for approval, and accepts a
`tokio_util::sync::CancellationToken`. `RunManager::drive` in
[crates/lightagent-api/src/manager.rs](../../crates/lightagent-api/src/manager.rs)
adapts paused agent outcomes to approval decisions and maps completion to a
`RunStatus`. The manager creates a Tokio task per run, buffers `AgentEvent`s in
memory, and uses `Notify` for late/live SSE consumers.

Tool registration begins with `ToolRegistry::builtin()` and is narrowed by
configuration/profile/extension context in `configured_registry` in
[crates/lightagent/src/chat.rs](../../crates/lightagent/src/chat.rs). The
production factory installs that registry in `BoundedExecutor`, binds the run
ID, optional web/workspace/skills contexts, and configures the provider and
profile in [crates/lightagent/src/serve.rs](../../crates/lightagent/src/serve.rs).

## Current HTTP API and auth boundary

The authoritative router is [crates/lightagent-api/src/lib.rs](../../crates/lightagent-api/src/lib.rs).
All run routes use protocol prefix `/api/lightagent/v1`; `/health` is unversioned.

| Route | Operation | Required scope |
| --- | --- | --- |
| `GET /health` | Service health | none |
| `GET /api/lightagent/v1/tools` | Tool definitions | `ToolsRead` |
| `GET /api/lightagent/v1/provider` | Connected provider catalog/capabilities | `ToolsRead` |
| `GET /api/lightagent/v1/profiles` | Profile summaries | `SessionsRead` |
| `GET, PUT /api/lightagent/v1/settings` | CLI-backed settings | `Admin` |
| `POST /api/lightagent/v1/runs` | Start a managed run | `RunsWrite` |
| `GET /api/lightagent/v1/runs/{id}` | In-memory run status | `RunsRead` |
| `GET /api/lightagent/v1/runs/{id}/events` | Named SSE events | `RunsRead` |
| `POST /api/lightagent/v1/runs/{id}/cancel` | Cancel token | `RunsWrite` |
| `GET, POST /api/lightagent/v1/sessions` | List/create persisted sessions | `SessionsRead` / `SessionsWrite` |
| `GET, DELETE /api/lightagent/v1/sessions/{id}` | Read/delete persisted session | `SessionsRead` / `SessionsWrite` |
| `GET /api/lightagent/v1/approvals` | Pending approvals | `ApprovalsWrite` |
| `POST /api/lightagent/v1/approvals/{run}` | Deliver approve/deny decision | `ApprovalsWrite` |

[crates/lightagent-api/src/auth.rs](../../crates/lightagent-api/src/auth.rs)
implements a single bearer key with scopes; a keyed server compares tokens in
constant time. Loopback binds are intentionally open, while `serve` refuses a
non-loopback bind without `--key-env`. There is no multi-user tenant or
per-resource ownership model: session files are owner-private on the local
filesystem and the API's one bearer key authorizes all resources it can reach.

SSE translation is [crates/lightagent-api/src/sse.rs](../../crates/lightagent-api/src/sse.rs).
The UI listens for `run.started`, `model.delta`, tool, approval, turn, terminal,
and error event names in [frontend/src/hooks/useRunEvents.ts](../../frontend/src/hooks/useRunEvents.ts).
The event buffer is process-local and not an event log; reconnect works only
while the `RunState` remains in the running process.

## Persistence, identity, and migration baseline

`lightagent-store` persists a session as an owner-private JSON file under a
profile `sessions/` directory. Its public models are `Session`, `RunRecord`,
`StoredMessage`, and `ToolHistoryEntry` in
[crates/lightagent-store/src/session.rs](../../crates/lightagent-store/src/session.rs).
`SessionId` is generated locally and validated before it is used as a filename.
There is no SQLite/Postgres database, migration framework, durable run/task
record, agent tree, inbox, budget ledger, artifact table, workspace lease, or
idempotency record in the verified checkout.

The existing `RunState` is ephemeral (`HashMap` + `Mutex` in `RunManager`), so
run cancellation, event history, pending approvals, and model/tool execution
state do not survive a process restart. Session writes occur when an HTTP run
becomes terminal; `busy_sessions` prevents overlapping writes to one session.

## Existing delegation baseline

There is a narrow, synchronous single-level delegation tool today:
`agent.delegate` in
[crates/lightagent-tools/src/builtins/delegate.rs](../../crates/lightagent-tools/src/builtins/delegate.rs).
It accepts `profile`, `task`, optional `max_turns`, `max_seconds`, and
`tool_scope`; loads that profile; creates a fresh `AgentLoop`; intersects the
worker limits with caller caps; removes `agent.delegate` from the child tool
registry; and awaits the child to return final content as its tool result.
It is `RiskClass::Executable`, requires `agent:spawn`, and therefore follows
the existing approval policy. Production enablement is performed by
`BoundedExecutor::with_delegation` in
[crates/lightagent/src/serve.rs](../../crates/lightagent/src/serve.rs).

This is valuable to reuse as a runtime adapter input, but it is **not** a
hierarchical subagent manager: it has no durable child identity, task DAG,
roles registry, asynchronous scheduler, child event stream, inbox/results,
review pipeline, worktree lease, idempotency, child cancellation subtree, or
budget ledger. It explicitly removes child delegation, so depth is one.

## Capability matrix

| Primitive | Status | Verified implementation / adaptation implication |
| --- | --- | --- |
| Single agent async run | Reuse | `AgentLoop`, `RunFactory`, `RunManager::drive` |
| Provider streaming | Reuse | `LightweightProvider` -> `AgentEventSink`; HTTP SSE adapter |
| Tool registry and schema validation | Reuse | `ToolRegistry`, `BoundedExecutor`, tool definitions |
| Tool approval | Reuse | `PolicyEngine`, suspended outcomes, approval API/ACP bridge |
| Root run cancellation | Reuse/adapt | `CancellationToken` is passed to model/tool run; no child tree exists |
| Profile configuration | Reuse | `ProfileStore`, `AgentProfile`, `RunLimits` |
| Current one-level delegate | Adapt | `agent.delegate`; preserve compatibility while routing new hierarchy through manager |
| Hierarchical agent identity/state transitions | New | No `agents`, `tasks`, attempts, or CAS lifecycle records |
| Durable event log/replay | New | Current `RunState.events` is in-memory only |
| Queue/DAG scheduler/recovery | New | No durable queue or dependency model |
| Budget reservations/accounting | New | Run limits exist but no shared ledger/cost attribution |
| Child inbox/results/artifacts | New | Existing tool output only; session history is not a child result store |
| Workspace leases/worktrees | New | Workspace confinement exists; no leases/worktrees |
| Subtree cancellation | New/adapt | Build on `CancellationToken` but create parent/child ownership |
| Context compiler | New/adapt | Use profile persona/history/workspace controls, but no child-specific compiler |
| Feature flag | New | No verified `subagents_enabled` config/flag currently exists |
| Agent tree/inspector UI | New | Current UI has chat, tools, and settings only |
| UI reconnect projection | Adapt/new | `useRunEvents` handles SSE reconnection for one root run; tree replay needs durable event sequence/cursor |
| Auth/ownership | Adapt | Scope authorization exists; per-run/session owner enforcement is not multi-user capable |
| Database migration system | New | JSON session store has no migrations |

## Frontend baseline

The UI is a React SPA with routes `/`, `/tools`, and `/settings`
([frontend/src/App.tsx](../../frontend/src/App.tsx)). `Agent.tsx` owns chat
session/run/model-selection state locally; `usePoll` polls API catalog/settings
data; `useRunEvents` consumes the root-run SSE feed. Browser-only presentation
preferences are a React context persisted to `localStorage` in
[frontend/src/state/preferences.tsx](../../frontend/src/state/preferences.tsx).
There is no global server-state/query library, agent tree state, inspector,
subagent mutation API, or persisted browser-side authentication facility.

## Validation commands

The repository integrity script is `bash scripts/check.sh`: it runs Rust format,
Clippy, workspace tests, optional frontend build, version/dependency/secret
checks. The documented frontend commands are `npm ci --prefix frontend`,
`npm run build --prefix frontend`, and `npm run dev --prefix frontend`.
The CI workflow additionally builds `cargo build --release -p lightagent` and
smoke-tests the package. Existing delegation tests are in
[crates/lightagent-tools/tests/delegate.rs](../../crates/lightagent-tools/tests/delegate.rs);
HTTP API tests are in [crates/lightagent-api/tests/api.rs](../../crates/lightagent-api/tests/api.rs).

## Phase-1 boundary decision

Do not automatically convert chat to multi-agent execution. Add a
server-controlled `subagents_enabled` flag defaulting to false, retain the
present root `RunManager` behavior when disabled, and negotiate a new explicit
subagent protocol version only at the new subagent API boundary. The existing
`/api/lightagent/v1` chat contracts and `agent.delegate` tool must remain
compatible while the durable manager is introduced.
