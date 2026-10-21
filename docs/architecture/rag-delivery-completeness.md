# Research runs: truthful completion, bounded retrieval, reliable delivery

Status: implemented on branch `fix/rag-incomplete-delivery`, not merged.
Base: `master` at `506340b033fcb7a05fcb39db785dd3a019894be7`. This is also
the commit the earlier static inspection used.

## The problem

After a prompt, research sometimes stops, or the UI does not show its full
search results or final answer. This document covers four things:

- what the code at the base commit does on that path;
- which defects were confirmed;
- what Jev actually does in it;
- what this branch changes.

Static evidence shows how the symptom *can* happen. It does not prove which
mechanism caused a particular incident (see [Reproduction status](#reproduction-status)).

## Path traced

The path runs prompt → `POST /runs` (auth, profile ownership, session busy
lock) → `RunFactory::run_supervised` → `coordinator::preflight` (Jev routing
and classification, concurrently) → tool registry and tool policy → workflow
policy → `AgentLoop`. The loop streams turns from `LightweightProvider` and
runs tools through `BoundedExecutor`, with a 60 s limit per call. Tools
include `rag.search`, `rag.realtime` and `web.*`. Each event goes to the
`RunState` buffer, and from there to SSE (`GET /runs/{id}/events`), session
persistence (`record_run_events`), the WebUI (`useRunEvents` → `Agent.tsx`),
ACP (`stop_reason`) and the CLI renderer.

## Findings

Classification: **confirmed** means the code at the base commit does it, as
read. Each confirmed item now has a regression test that fails on the old
behavior. **Unconfirmed** means plausible, but not shown by this work.
**Already fixed** means correct at the base commit.

| # | Where | Finding | Class |
|---|---|---|---|
| 1 | `provider.rs` `finish_event` | A body that ends with no `finish_reason` and no `[DONE]` became `FinishReason::Stop`, so a cut connection looked like a finished answer. Partial tool-call arguments with no finish reason also ended as `Stop`. | confirmed |
| 1b | `provider.rs` | An explicit `finish_reason` followed by EOF with no `[DONE]` is valid. A transport error mid-body becomes `ProviderError::Stream`. | already correct, kept |
| 2a | `loop_.rs` | `FinishReason::Length` was mapped to `StopReason::EndTurn`: a truncated answer was reported as complete. | confirmed |
| 2b | `loop_.rs` `drive` | When the last permitted turn asked for tools, the batch ran and the run ended `MaxTurns` before any turn read the results. With the default `max_turns = 8`, a model still researching at turn 8 produced no answer. | confirmed |
| 2c | `loop_.rs` `process_batch` | Same pattern for `MaxToolCalls`: the run ended in the middle of a batch, with results nobody read. | confirmed (not in the original list) |
| 2d | `loop_.rs` | A provider stream with no `Finished` event at all also became `Stop`. | confirmed |
| 3a | `useRunEvents.ts` | `EventSource` reconnects by itself, and the server replayed from event 0 with no `id:`. The handler appended every frame, so a transient drop duplicated the transcript. | confirmed (not in the original list) |
| 3b | `useRunEvents.ts` | When polling found a terminal status after an SSE error, the hook added an empty terminal event and closed. Events missed during the gap (tool output, answer deltas) were never fetched. | confirmed |
| 3c | `useRunEvents.ts` | `routing.selected` and `classification.completed` were not subscribed, so the existing routing-status line could never render from a live run. | confirmed (not in the original list) |
| 3d | `useRunEvents.ts` | `EventSource` fires a plain `error` Event on every connection fault. The hook also listened for the run's named `error` frames, so each drop added a run error and the UI showed "The run failed." for a healthy run. Found by the render test in CI. | confirmed (found during validation) |
| 4a | `Agent.tsx` `foldModelOutput` | On a terminal event, only content from *completed* turns became the answer. Text from a turn cut short by a mid-stream error stayed in the reasoning panel, which is hidden once the run is done. | confirmed |
| 4b | `Agent.tsx` | Every terminal reason except cancel and fail showed as "completed". `MaxTurns`, a token limit and an early end were indistinguishable from success. | confirmed |
| 4c | `Agent.tsx` | Tool rows and the run badge live in the collapsed "Status Control" section, so retrieval had no visible progress by default. | confirmed |
| 5a | `realtime.rs` | All fetches were collected before ranking, and there was no overall deadline. Search (up to 20 s), plus two waves of 20 s fetches, plus ranking could pass the executor's 60 s per-call timeout. The timeout returns only `timed out`, so every page and snippet already gathered was lost. | confirmed |
| 5b | `embed.rs` | `EmbeddingClient` had no request timeout. `platform.infinity.timeout_secs` was applied only to the remote Qdrant path. A stalled embedder held `rag.realtime` and local `rag.search` ranking until the executor timeout. | confirmed (not in the original list) |
| 5c | `realtime.rs` | No progress was reported during searching, fetching or ranking. The result named only a count of fetch failures, not which sources were snippet-only. | confirmed |
| 6 | API session save | Sessions saved by the API recorded the run *status* (`Completed`) as `stop_reason`. The CLI records the real reason. | confirmed (not in the original list) |
| 7 | `RunManager` | Run buffers are kept in memory and are never evicted. This is pre-existing, outside this fix, and recorded for follow-up. | noted |
| H1 | network | Proxies or idle timeouts between browser and harness dropping long SSE streams. The client now recovers from this, but the frequency is unmeasured. | unconfirmed |
| H2 | model | Small models stopping research early by their own choice. Not a code defect; no change. | unconfirmed |

## Jev's verified role

Verified from `crates/lightagent/src/coordinator/{mod,classify,workflow,state,tool}.rs`,
`crates/lightagent/src/jev.rs`, `crates/lightagent-jev` and the
`serve.rs`/`chat.rs` call sites. Nothing below is assumed from the product name.

| Question | Answer from the code |
|---|---|
| Does Jev classify retrieval need? | Partly. Classification asks one categorical question: the task category (`conversation`, `research`, `coding`, `analysis`, `automation` or `multi_step`), with TypeSafe's confidence. It also asks three yes/no (`noul`) questions with probabilities and no confidence: `planning`, `retrieval` and `tools`. And it asks one complexity score. |
| Does classification influence execution? | Only in `active` mode **and** with `workflow_guidance = true` **and** an `Eligible` workflow. Then one fixed advisory paragraph (`RESEARCH_GUIDANCE` and similar) is appended to the persona. Nothing else changes: not the registry, approvals, limits or provider request. The default mode is `off`; `shadow` only records. |
| Does it select models, profiles or workflows? | Model routing (`platform.jev.enabled` plus allowlists) may pick an allowlisted model, never over `Auto`. Routing is a separate request from classification. Classification picks no model, profile or tool. Its "workflow" is a label checked for eligibility, not an execution path. |
| Do retrieval probabilities or thresholds affect decisions? | `retrieval_probability` is recorded in diagnostics and **used by no decision**. Only `category_confidence` is held to a threshold: `version_thresholds`, or the default 0.9. |
| Do tools and budgets constrain eligibility? | Yes, and only as read-only inspection. Research is eligible only if one of `rag.realtime`, `web.search`, `web.fetch` or `rag.search` is present and not denied by policy, and the limits allow at least 2 turns and 1 tool call. |
| Disabled, denied, unavailable, slow, malformed or uncertain? | `Skipped(endpoint_disabled \| no_endpoint \| profile_not_permitted \| attachment \| key_material \| … \| cancelled)` or `Failed(timeout \| connect \| unauthorized \| malformed \| unsupported_format \| …)`. Low confidence leads to `Uncertain`. In every case the run proceeds exactly as with Jev off. Covered by existing `classify.rs` tests (`timeout_and_cancellation_are_bounded`, refusals, failures by reason). |
| Latency? | One preflight per run (or terminal turn), before the loop starts. Routing and classification run concurrently, each bounded by `platform.jev.timeout_secs` (default 3 s), and both stop on cancellation. The Jev budget is `max_calls_per_run` (default 4, range 2–16), shared with the explicit `jev.classify_task` tool. |
| Can it suppress required retrieval? | No. No code path removes a tool or blocks a call because of a classification. A confident "conversation" yields `DefaultBehavior`, with no guidance and the same tools. A new test covers this: `a_confident_no_retrieval_category_never_withholds_retrieval`. |
| Is Jev a search engine, embedder, reranker or evidence verifier? | No. None of those capabilities exist in `lightagent-jev` or its API contract. |

**Gap.** Lightagent does not select a retrieval *path*. The model alone
chooses among `rag.search`, `rag.realtime` and `web.*` from the tools it is
offered. Jev's retrieval signal is diagnostic only. This fix adds no routing.
A proposal is under [Not implemented](#not-implemented-proposed-separately).

**Does Jev help this fix?** No, as a cause or as a remedy. Every confirmed
mechanism is downstream of Jev: provider EOF, loop budgets, retrieval
deadlines and SSE delivery. The only Jev-related change is observability: its
two existing events are now shown, labelled as predictions, and are kept
separate from actual retrieval progress.

## Decision workflow

Solid boxes and the "existing" column describe behavior that was already
there. Boxes marked **(new)** and the "this branch" column are what this
branch adds or changes.

```mermaid
flowchart TD
  A[POST /runs] --> B{auth, profile ownership,\nsession lock}
  B -- refused --> Z1[401/403/404/409]
  B --> C[run + cancel token\n+ RunAuthorization]
  C --> D{Jev classification mode\n≠ off, endpoint set,\nprofile permitted,\ninput summarizable?}
  D -- no --> E[Skipped: code recorded]
  D -- yes --> F[one sanitized request,\ntimeout 3 s, cancellable]
  F -- timeout / malformed / error --> G[Failed: code recorded]
  F -- ok --> H{confidence ≥ threshold?}
  H -- no --> I[Uncertain: no effect]
  H -- yes --> J{workflow eligible?\ntools present + permitted,\nbudget ≥ minimum}
  J -- no --> K[advisory only]
  J -- yes, active + guidance on --> L[append fixed advisory text]
  E & G & I & K & L --> M[classification.completed event\n**now shown in UI, labelled advisory (new)**]
  M --> N[agent loop: the model chooses tools\nfrom the unchanged registry]
  N --> O{tool}
  O -- rag.search --> P[profile index\n+ optional Qdrant/Infinity\n**embedder now time-bounded (new)**]
  O -- rag.realtime --> Q[**search → fetch as they arrive →\nrank within one 45 s deadline,\nprogress events, per-source status (new)**]
  O -- none --> R[ordinary answer]
  P & Q --> S{budget left?}
  S -- turns/tool calls remain --> N
  S -- **last permitted turn / tool calls spent (new)** --> T[**tool-less answer turn\nfrom gathered evidence (new)**]
  N --> U{finish}
  T --> U
  U -- stop --> V[EndTurn]
  U -- **length (new)** --> W[MaxTokens, text kept]
  U -- **EOF without signal (new)** --> X[Incomplete, text kept,\ntruncated tool call not run]
  U -- budget wrap-up --> Y[MaxTurns / MaxToolCalls /\nWallClockExceeded]
  V & W & X & Y --> AA[run.completed reason + **complete (new)**\nsession stop_reason **= real reason (new)**]
  AA --> AB[**SSE ids, Last-Event-ID / ?after= resume,\nclient in-order dedupe, replay before done (new)**]
```

| Situation | Existing behavior | This branch |
|---|---|---|
| Jev mode off (default), disabled, profile not permitted, attachment or key material | Skipped with a code; run unchanged | Same; the skip is now visible in the UI |
| Jev timeout (3 s), connection error, malformed or legacy format | Failed with a code; run unchanged | Same; shown with latency |
| Jev low confidence | Uncertain; no guidance | Same |
| Jev confident "research", eligible, `active` + guidance | Fixed advisory text added; same tools | Same |
| Jev confident but wrong (e.g. "conversation" for "search the web") | Tools unchanged; model decides | Same; now has a test |
| Indexed-document question | Model may call `rag.search` (profile-scoped) | Same; local semantic ranking bounded by the embedder timeout |
| Current-web question | Model may call `rag.realtime` if configured | Same choice; retrieval bounded by a deadline, partial evidence kept, progress shown |
| Both | Model may call both within the run's turn and tool-call budgets | Same; the last turn is reserved for the answer |
| No retrieval needed | Ordinary answer | Same |
| Required tool absent or denied | Not offered, or a controlled tool error the model sees | Same (no new routing) |
| Slow source | Whole call could hit the 60 s timeout, losing all evidence | Unfinished fetches dropped at the deadline; snippet kept and named |
| Some or all fetches fail | Snippets kept; failures counted only | Each source named (`fetch failed` / `not fetched in time`; `snippet only` / `no snippet`); `Retrieval status: evidence\|partial` |
| Nothing readable | Tool error | Same; progress outcome `failed` |
| Embedder stalls | Held until the 60 s timeout | 30 s client timeout (Infinity: configured); `rag.realtime` falls back to lexical order at its deadline |
| Last turn asks for more tools | Tools run, then the run ends `MaxTurns` with no answer | Last permitted turn offers no tools and answers; ends `MaxTurns` |
| Tool-call budget hit mid-batch | Run ends `MaxToolCalls`, no answer | The remaining calls get controlled errors, then one answer turn if the turn budget allows |
| `finish_reason: length` | `EndTurn` | `MaxTokens`, text kept, no tool run |
| EOF with neither finish reason nor `[DONE]` | `Stop` → `EndTurn` | `Incomplete`, text kept, no tool run, no retry |
| Explicit finish reason, then EOF | `Stop` | Same (valid) |
| SSE drops and the browser reconnects | Full replay appended: duplicated | Server resumes after `Last-Event-ID`; client accepts ids strictly in order |
| SSE dead while the run finishes | Synthetic terminal event; missed content lost | Replay after the last id (up to 4 tries); if that fails, `delivery: incomplete` with a visible notice |
| Cancellation during classification, retrieval or synthesis | Ends; partial events kept | Same; `rag.realtime` returns promptly; tested in the loop and the tool |

## What changed

- **Core** (`lightagent-core`): adds `FinishReason::Incomplete`,
  `StopReason::{MaxTokens, Incomplete}`, `StopReason::{is_complete,
  explanation}`, and `AgentEvent::ToolProgress` with `ToolProgress` and
  `ToolProgressSink`. `ToolInvoker::invoke_with_progress` has a default
  implementation, so existing invokers are unchanged. In the loop:
  - the turn finish is mapped truthfully;
  - a reserved, tool-less final turn (`TURN_BUDGET_INSTRUCTION`) is taken
    once tools have run;
  - after the tool-call budget runs out, a wrap-up turn follows
    (`TOOL_CALL_BUDGET_INSTRUCTION`);
  - progress is forwarded, at most 64 reports per call.
- **Provider** (`lightagent-provider-lightweight`): EOF with neither a finish
  reason nor `[DONE]` becomes `Incomplete`. `EmbeddingClient` gains a default
  30 s timeout and `with_timeout`.
- **Tools** (`lightagent-tools`): `ToolCtx.progress` with `report()`.
  `BoundedExecutor` passes the sink to the tool under the same checks and the
  same per-call timeout.
- **RAG** (`lightagent-rag`): `rag.realtime` gains:
  - one 45 s deadline (inside the 60 s per-call limit), with 10 s reserved
    for ranking;
  - fetches taken as they finish, and pending ones dropped at the deadline,
    which closes their connections;
  - semantic ranking bounded, with lexical fallback;
  - per-source status, a `Retrieval status:` line, and progress stages
    (`searching`, `fetching n/N`, `ranking`, then `completed` with an
    outcome).

  `with_deadline` can only tighten the bound. The remaining limits are
  unchanged: concurrency stays 4, and the source cap, SSRF checks and
  authorization are untouched.
- **App** (`lightagent`): Infinity's configured timeout applies to semantic
  ranking.
- **API** (`lightagent-api`):
  - every SSE frame has `id:` (its buffer position);
  - `GET /runs/{id}/events` resumes after `Last-Event-ID` or `?after=n`
    (the later one wins; a malformed `after` is 400; an unreadable header is
    ignored);
  - the stream goes through the same `authorized_run` check and multi-user
    stream guard;
  - `tool.progress` is a new event;
  - `run.completed` adds `complete`;
  - `GET /runs/{id}` adds `stop_reason`;
  - API-saved sessions store the real stop reason.

  All additions are additive: every existing field and event name is
  unchanged.
- **ACP**: `MaxTokens` maps to `max_tokens`, and `Incomplete` to `refusal`,
  like `Error`, never `end_turn`.
- **CLI**: a non-normal end prints the code and a short explanation.
- **WebUI**:
  - `useRunEvents` deduplicates by id and reconnects with a cursor, and
    replays to the terminal event before calling a run done. It now returns
    `delivery`.
  - `Agent.tsx` shows partial text as the answer, a stop-reason notice and
    badge, retrieval progress in the transcript and on the tool row, and the
    Jev classification and routing lines, worded as advisory.

## Not implemented (proposed separately)

- **Retrieval-path routing.** Recognize explicit intent deterministically
  ("search the web", "in my documents") and pre-select `rag.realtime`,
  `rag.search` or both under a shared budget. Treat Jev's
  `retrieval_probability` as one more advisory input, never as an override.
  This needs its own design: intent grammar, budgets, and UI for "the tool
  you asked for is not configured".
- **Run-buffer retention.** Evict terminal runs after a bounded time or count.
- **Live provider check.** Run a paired test against the Lightweight gateway
  for how it ends streams. The paired-E2E job exercises the mock gateway only.

## Reproduction status

The user's incident has **not** been reproduced on an installed application.
The regression tests reproduce each confirmed mechanism deterministically, in
CI only. Telling which mechanism a given incident hit requires the installed
app's evidence:

- `GET /api/lightagent/v1/runs/{id}` (`status`, `events`, and, after this
  change, `stop_reason`);
- the run's saved session `runs[].stop_reason`;
- the browser console and network log for the `events` request (drops and
  reconnects);
- the server log around the run;
- whether `rag.realtime` and Infinity or another semantic embedder are
  configured, and their timeouts;
- the profile's `max_turns` and `max_tool_calls`.

## Validation record

All builds, tests and renders ran on GitHub-hosted runners. Locally, the
work was limited to reading and editing source, git, the `gh` CLI, and
`rustfmt` on the edited Rust files. No compilation, tests, linting, type
checking, servers or browsers were run locally.

| Tested SHA | Workflow run | Result |
|---|---|---|
| `6fd3a45` | [39604696968](https://github.com/dlroqa/Lightagent/actions/runs/39604696968) | Failed: `lightagent-rag` did not compile. The fetch stream in `rag.realtime`, built from a closure that returns an async block and held across awaits, broke the `Send` bound of `#[async_trait]` ("`FnOnce` is not general enough"). Every job that builds the workspace failed for this reason. |
| `c2a84c4` | [39605330785](https://github.com/dlroqa/Lightagent/actions/runs/39605330785) | Rust green on Linux, macOS and Windows: fmt, clippy `-D warnings`, the full workspace tests, the provisioning feature, paired Lightweight E2E, Qdrant + Infinity, and package smoke. The Web UI render failed: the new scenarios mocked `POST /runs`, so the session stayed empty and the screen kept the welcome layout (a test-harness defect). |
| `38d2a11` | [39607176247](https://github.com/dlroqa/Lightagent/actions/runs/39607176247) | Rust green again. The render failed again: Playwright request routing never saw the `EventSource` request (0 stream requests). It also exposed a real UI bug: a connection fault showed "The run failed." (finding 3d). |
| `4419b84` | [39608861697](https://github.com/dlroqa/Lightagent/actions/runs/39608861697) | Render green, with all 5 delivery scenarios exercised through a real proxy. The browser resumed with `Last-Event-ID` 7 and 6; the dead-stream case made 4 bounded `?after=1` recoveries and then reported the gap. Screenshots and `delivery/report.json` are in the `lightagent-web-screens` artifact. Linux, macOS, paired E2E, Qdrant + Infinity, package smoke, provisioning fixtures, Open Terminal, sandbox and revocation measurement passed, and so did the Windows check. **All 11 jobs green.** |

The PR's own `pull_request` run validates the final head, which adds only this
record.
