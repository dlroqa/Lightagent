# Bounded revocation monitoring

Status: implemented, **not activated**. `lightagent serve` still never builds
multi-user mode, so no production request starts a revocable run. The
behavior below is proven by tests that inject an identity authority. It is
not yet production end-to-end behavior.

Principle: revocation is checked before actions and observed during
long-running activity. A user whose access is revoked keeps no authority
through an existing model stream, tool wait or delegated worker.

## 1. Source audit (master `e52c3cf`)

| Concern | Integration point |
|---|---|
| Authority | `IdentityAuthority::{sync, refresh, authorize_run, run_check}` (`lightagent-api/src/auth.rs`). A persistent authority re-reads `identity/state.json` on every check. |
| Run authorization | `RunAuthorization::{verify, revoke}`, sticky and cancelling (`lightagent-core/src/identity/revocation.rs`) |
| Supervisor | `RunManager::{start_supervised, revoke, revalidate, affected_by}` (`lightagent-api/src/manager.rs`). Before this PR it ran only when called. |
| Generations | `IdentityDocument::generation`, recorded per run (`RunAuthorization::generation`). It can only be read by parsing the whole document. |
| Cancellation | One `CancellationToken` per run, shared by the loop, executor, tool context and delegated workers |
| Model streams | `AgentLoop::take_turn` selects on cancel around `stream.next()`, but **not** around `provider.stream(...)` (connection setup). `LightweightProvider::stream` awaits `send()` before returning a stream. |
| SSE | `event_stream` guard. It re-checks every 2 s, on every event, and when `RunState::request_recheck` fires. |
| Tools | `BoundedExecutor::invoke` selects on cancel around every call, so the call future is dropped on cancellation. |
| HTTP / MCP | `fetch_text_authorized` (per hop), `McpTool::call`. Request futures are dropped on cancel. |
| Open Terminal | `OpenTerminalRun::run` polls status in a loop and deletes the remote process when cancelled. Before this PR it did not re-check authorization between polls. |
| Delegated workers | Share the run's token, executor authorization and `AuthorizedProvider` |
| Approval waits | `drive_supervised` selects on cancel and verifies before resuming |
| Store persistence | `IdentityStore::load`: readers never lock; writers lock and replace by atomic rename, so a reader sees the old or new document whole. |

Gap closed here: a run that took no action (a long model stream, a held tool,
an MCP call, a worker, an approval wait) only observed a revocation pushed
in-process, or made in another process, at its next action. Nothing
re-checked it on its own.

## 2. Architecture

```text
RunManager (existing)
 ├─ runs: HashMap<id, RunState{ authorization, cancel, recheck, … }>
 └─ monitor: Monitor { clock, task slot, active count, state }
      start_supervised ─► active += 1 ─► ensure_monitor (spawn if none, under the slot lock)
      run task ends ─────► active -= 1 (before the terminal status is visible)

watch_runs (one task per manager, only while active > 0):
  loop {
    clock.tick()                      ── RevalidationInterval: sleep(interval)
    sweep_runs(runs):
      active revocable runs (not terminal, not yet revoked)
      core::identity::sweep(|| for each: authorization.verify())
         └─ IdentityAuthority::sync: first check of the sweep reads the store,
            the rest reuse that read (failure included)
      request_recheck() on every run   ── wakes guarded SSE streams
    if active == 0 (under the slot lock) → clear the slot, stop
  }
```

- **One monitor per manager**, shared by all its runs, rather than one per run.
  A sweep costs one store read plus one in-memory check per run. The monitor
  holds only a `Weak` to the run table, so it stops when the manager is
  dropped.
- **Lifecycle.** The monitor starts with the first revocable run and stops at
  the first sweep with none active. The start check and the stop decision
  are made under the same lock, so a run that starts while the monitor stops
  always gets a monitor. Trusted runs (`start`, `start_as`: terminal, ACP,
  single-user API) never touch it.
- **Interval.** `RevalidationInterval` defaults to 5 s
  (`DEFAULT_REVALIDATION_INTERVAL`) and is clamped to 100 ms–300 s, so the
  monitor neither spins nor goes unchecked. An embedding sets it with
  `RunManager::with_revalidation_interval`, or supplies its own
  `RevalidationClock`. No configuration key is added, because the server
  does not activate multi-user mode yet.

## 3. Revalidation strategy

Every sweep re-checks each active revocable run's credential (by key id:
revoked, expired at the authority's clock) and its owner and profile
(ownership, reassignment, grants, credential restriction). It uses the same
`run_check` as every enforcement point. The first refusal is final. It
cancels the run, which tears down whatever the run is waiting on, and it is
audited once.

Enforcement points (the pull checks before actions, from PR #22) still apply.
The monitor adds observation *between* actions.

## 4. Cross-process consistency and the generation question

- A change made by another process (the operator CLI) is committed by
  atomic rename. The next sweep reads the file and sees it, and so does any
  enforcement point. This is proven with real `lightagent identity` child
  processes in `crates/lightagent/tests/revocation_cross_process.rs`.
- **Generation as an optimization: evaluated, not used to skip reads.**
  Reading the generation requires reading and parsing the same single
  document, so comparing generations would save nothing. Trusting an
  in-memory generation without reading the file would miss another
  process's change. What the generation *can* safely save is repeated reads
  within one sweep. The sweep therefore reads the store **once per sweep,
  every sweep**, whatever the number of runs, and no positive answer outlives
  its sweep. A constant-time probe (for example, a generation file written
  in the same transaction) remains a possible future optimization. Even
  then it must be re-read every sweep, never cached across sweeps.
- **Unreadable or corrupt store.** The sweep's single read fails, every
  protected run in it is refused, and all are revoked: fail closed.
- **Several server processes.** Each supervises its own runs; all read the
  same store.

## 5. Cancellation propagation

```text
revocation observed (sweep, enforcement point, or RunManager::revoke)
  └─ RunAuthorization::revoke ─► run CancellationToken.cancel()
       ├─ model turn: loop select! drops the stream (HTTP connection closed)
       │    └─ connection still being set up: AuthorizedProvider select! drops it (new)
       ├─ tool call: executor select! drops the call future
       │    ├─ web.fetch / web.search / rag.realtime: request dropped, no further hop
       │    ├─ HTTP MCP: request dropped; stdio MCP: request abandoned
       │    ├─ terminal.run: kill_on_drop kills the child
       │    └─ Open Terminal: re-checks before every poll; on refusal deletes the remote process (new)
       ├─ delegated worker: same token: worker stream/tools dropped; its checks refuse more
       ├─ approval wait: drive_supervised select! ends the wait; later decisions refused
       └─ SSE: request_recheck wakes guarded streams; subscriber guards re-run
```

"Streaming tool output": no built-in tool streams output to the run today.
Open Terminal is the long-polling case and re-checks before every poll.

## 6. Measurements (GitHub Actions)

From the "Revocation detection latency (measurement)" job (run
[38341770346](https://github.com/dlroqa/Lightagent/actions/runs/38341770346),
head `81d0789`). Conditions:

- GitHub-hosted `ubuntu-24.04`, linux x86_64.
- Tokio multi-thread runtime with 2 workers; synthetic in-memory authority.
- One held model stream per run.
- The revocation is written without notifying the run (pull only).
- Each trial uses a new manager and revokes right after the run starts.

Because the monitor's first tick is one interval after it starts, these
trials measure close to the **worst case** (a full interval). A revocation at
a random moment in steady state waits between 0 and one interval.

| interval | trials | min | median | p95 | max |
|---|---|---|---|---|---|
| 250 ms | 10 | 251.58 ms | 251.66 ms | 251.73 ms | 251.73 ms |
| 1 s | 10 | 1.0006 s | 1.0015 s | 1.0016 s | 1.0016 s |

Detection is bounded by the interval plus about 2 ms of sweep and
scheduling time on this runner. At the default 5 s interval the bound is
about 5 s. This is measured behavior, not a hard real-time guarantee.

**Sweep overhead** (one sweep over N held runs against a persistent
authority on the runner's disk; one store read per sweep):

| runs | sweep time | store reads | per run |
|---|---|---|---|
| 1 | 58.7 µs | 1 | 58.7 µs |
| 50 | 133.2 µs | 1 | 2.7 µs |
| 200 | 392.7 µs | 1 | 2.0 µs |

At the 5 s default, 200 active runs cost about 0.4 ms of work every 5 s
(under 0.01% of one core). With no revocable run active, the monitor does
not exist and costs nothing. The trade-off: halving the interval halves the
worst-case latency and doubles this (small) cost. The 100 ms floor caps the
cost at 10 sweeps per second.

## 7. Tests

| Requirement | Test |
|---|---|
| Idle model stream | `revocation_monitor::an_idle_model_stream_is_revoked_at_the_next_sweep` |
| Continuous token streaming | `…::continuous_streaming_is_revoked_and_stops` |
| Approval wait | `…::an_approval_wait_is_revoked_by_reassignment` |
| Long MCP activity | `lightagent` `revocation_cross_process::a_cli_revocation_in_another_process_stops_a_long_mcp_call`; `lightagent-mcp` `revocation::*` |
| Delegation | `…::a_delegated_worker_is_revoked` |
| Open Terminal | `open_terminal::tests::revocation_between_polls_deletes_the_process_and_stops_polling` |
| Credential expiry during a run | `…::expiry_is_observed_by_sweeps_for_runs_and_streams` |
| Profile reassignment while running | `…::an_approval_wait_is_revoked_by_reassignment` |
| Cross-process changes | `revocation_cross_process::*` (real CLI processes); `…::durable_sweeps_read_once_and_fail_closed` |
| Corrupt / inaccessible store | `…::durable_sweeps_read_once_and_fail_closed` |
| Many concurrent runs | `…::many_concurrent_runs_are_swept_precisely` (36 runs, 3 principals) |
| Only the right principal / profile | the same, plus the approval-wait test (another user's run unaffected) |
| SSE termination | `…::expiry_is_observed_by_sweeps_for_runs_and_streams` |
| Cancellation during connection setup | `…::a_connection_being_set_up_is_abandoned` |
| No tool invocation after revocation | `…::a_long_tool_call_is_revoked`, approval-wait test; PR #22 suites |
| No new external request after revocation | cross-process test (one MCP request), Open Terminal test; PR #22 suites |
| Single-user compatibility | `…::the_monitor_runs_only_while_revocable_runs_are_active` (trusted run never starts it); unchanged suites |
| No uncontrolled polling or task leaks | `…::the_monitor_runs_only_while_revocable_runs_are_active`, `…::the_interval_is_bounded` |
| Store read once per sweep | `…::durable_sweeps_read_once_and_fail_closed` |

All of these use a revalidation clock the test drives (a sweep happens
exactly when the test sends a tick) and explicit signals. Timeouts appear
only as guards against a hang. The measurement test is the only one that
uses real time, and it is `#[ignore]`d outside its job.

## 8. Limits and side effects

- **Detection is bounded, not hard real-time.** A revocation is observed
  within about one interval, plus one sweep, plus scheduling delay. No
  hard deadline is promised.
- **Revocation does not undo committed side effects.** A remote server may
  complete an MCP call, fetch or Open Terminal command it already received.
  A provider may finish generating a response it was streaming. Only further
  dispatch is prevented.
- **Open Terminal** deletion on revocation is best-effort, as it is for any
  cancellation (unchanged).
- **Delegated workers** inherit the run's token; a worker blocked in
  non-cancellable work is dropped, with the same side-effect limits as its
  tools.
- **Expiry** is judged by the authority's clock at sweep time.

## 9. Compatibility

- Trusted runs (terminal, ACP, single-user API, every operator run started
  with `start`/`start_as`) never start the monitor or pass through the new
  connection-setup select, so their behavior is unchanged.
- `AuthorizedProvider` keeps the exact pass-through for unconditional runs.
- Open Terminal's new check is a no-op without an authorization (always the
  case for trusted runs).
- `RunManager` gains builder methods and a `monitor_state` receiver. Its
  existing methods are unchanged, and `revalidate` keeps its contract.
- Tests: the run-lifecycle fixtures moved to
  `crates/lightagent-api/tests/support/fixtures.rs` (included by three test
  binaries). Every assertion of the PR #22 tests is kept. The tests now pace
  the monitor with a test clock, so no background sweep can race them.
- CI: one additional, non-required measurement job. The ten required checks
  are unchanged.
- No new dependencies; `Cargo.lock` unchanged; identity schema unchanged.

## 10. Rollback

Revert the PR. Nothing is persisted by the monitor, and no schema or
configuration is added. Without it, revocations are observed at enforcement
points and on explicit `revalidate`/`revoke`, exactly as after PR #22.

## 11. Remaining multi-user activation blockers

1. Serve wiring: a persistent authority, an explicit and validated
   activation setting, and a configuration key for the revalidation interval
   at that point.
2. Profile-scoped session, tool-catalog and settings endpoints.
3. A verified execution sandbox (the experimental backend stays refused).
4. Provisioning service and external issuers (not implemented by design).
5. Operator-credential lifecycle; legacy-key replacement.
6. Credential-attempt rate limiting and lockout.
7. A dedicated revocation event type for clients.
8. Guaranteed Open Terminal cleanup.
9. A constant-time generation probe if per-sweep parse cost becomes
   significant at scale (see §4 and §6).
