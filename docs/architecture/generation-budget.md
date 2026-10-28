# Generation cutoffs: diagnosis and output/context budgeting

Status:

- Implemented on branch `fix/generation-budget` (draft PR #61).
- Stacked on it: `fix/length-continuation` (draft PR #62).
- Neither is merged.

Bases:

- Lightagent `master` at `0a07e74`.
- Lightweight read at `32530a3` (v0.8.1) and at tag `v0.3.25`.

Everything here comes from reading source and from deterministic tests on
GitHub-hosted Actions. No application, model or test was run locally.
Scripted providers prove the control logic only. They establish nothing
about real-model quality, speed or memory use, or whether this addresses
the original incident.

## Diagnosis

### Path traced

1. **Entry and history packing.** A prompt arrives at `POST /runs`, the
   terminal or ACP. Session history is packed with
   `lightagent_store::model_history`.
2. **Preflight.** `coordinator::preflight` runs Jev routing and
   classification.
3. **Request.** The `AgentLoop` builds a `ProviderRequest`.
   `LightweightProvider` sends it as `POST /v1/chat/completions` with
   `stream_options.include_usage`.
4. **Gateway.** The Lightweight gateway counts the prompt with the engine's
   tokenizer and refuses `prompt >= n_ctx` with a 400. It clamps
   `max_tokens` to `n_ctx - prompt - 1`. The llama.cpp upstream forwards
   `max_tokens` and `reasoning_effort`.
5. **Return.** `finish_reason` and the usage chunk come back. The loop maps
   them to `TurnCompleted` and `RunCompleted { reason }`.
6. **Consumers.** SSE, session persistence (`record_run_events`
   concatenates every `Content` fragment), the WebUI (`describeStop`), ACP
   and the terminal.

### What the code shows

| # | Finding | Class |
|---|---|---|
| F1 | Lightagent sent no output allowance (`max_tokens: None` on every request). | Code |
| F2 | Lightweight lets such a turn generate up to `n_ctx - prompt - 1` (same at `v0.3.25` and `v0.8.1`). On that path, with that backend, a `length` finish means prompt, reasoning and answer together reached the loaded window. | Code |
| F3 | Restored history was bounded at 35% of an assumed window, at most about 16k estimated tokens. Tool results added during a run were appended whole, up to 256 KiB each. | Code |
| F4 | A prompt that outgrew the window got a 400, and the run ended `Error`, not `MaxTokens`. | Code |
| F5 | Reasoning shares the turn's budget. No `reasoning_effort` was ever sent, so each model's template default applied. | Code; per-model effect unknown |
| F6 | `master`: a `length` finish ends the run `MaxTokens` and keeps the text. A cut-off tool call is never run. | Code |
| F7 | The **installed** Lightagent 0.4.1 maps `Length` like `Stop`, so a cut-off answer is reported `EndTurn` there. | Code at tag `v0.4.1` |
| F8 | `serve` and `acp` packed history against `runtime.n_ctx`, or 4096 when unset. The terminal used `runtime.n_ctx`, else a number parsed from the model's name, and that setting took precedence over anything discovered. | Code; **fixed in #61** |
| F9 | Installed: `lightagent/0.4.1` and gateway `hermes-gateway-0.3.25`, with `runtime.n_ctx` unset. 0.3.25 has no `/v1/capabilities`; its `/v1/models` row carries `n_ctx` and the `hermes` namespace. | Local files, read only |

### What is not known

The cause of the original incident is **unconfirmed**. Its model, loaded
window, prompt size, finish reason and backend path were never observed.
No logs, sessions or endpoints were examined.

- **A `length` finish** is evidence that *a* generation limit was reached.
  On Lightweight with no allowance sent, that limit is the window (F2). It
  does not show that the incident exhausted the context: that needs the
  incident's own finish reason and confirmation that it went through this
  backend path.
- **The installed 0.4.1** cannot even show a `length` finish (F7).
- **Incomplete source retrieval** ("3 of 5 sources read") is a separate
  condition. It can come from any of:
  - the turn, tool-call or wall-clock budgets;
  - `rag.realtime`'s deadline;
  - a failed fetch;
  - a 400 overflow;
  - the model stopping.

  It is not evidence of token exhaustion.

The `generation.report` event added here records each turn's window,
prompt figure, allowance and finish reason, so a future occurrence can be
told apart.

## What changed

### #61: one budget, resolved the same way everywhere

`lightagent::generation::resolve` is the single resolution path. It
discovers the window the model is served with:

- from Lightweight's `/v1/capabilities`;
- on gateways older than v0.4.0, from the `/v1/models` row, and only when
  that row carries the `hermes` namespace;
- only for the exact model the run resolved;
- never from a model name.

It produces two things:

- the run's `GenerationBudget`, which workers receive through
  `GenerationSlot`;
- the `HistoryWindow` that session history is packed against.

Who calls it:

- **Terminal:** before each turn, for history, the run and its workers.
- **API and ACP:** before packing, through `RunFactory::history_window` /
  `RunManager::history_limit`. Both go through the same call, and the
  serve factory implements it with the same `resolve`. The run itself
  resolves its budget again when it starts.

**Nothing is cached.** Each resolution asks the provider again, so a
different endpoint, a different resident model or a reload at another
window takes effect on the next run.

The history window is the first of these that applies, and it says which:

| Source | When |
|---|---|
| `provider` | The provider reported a window: that window, verified capacity. It is never cut down to `runtime.n_ctx` or the 4096 fallback. |
| `configured` | `generation.context_window` is set and smaller than the reported window, or nothing was reported. It is a ceiling: it can narrow a reported window, never stretch it. |
| `placement` | `runtime.n_ctx` is set: the window a placement *asks for*, not verified to be loaded. |
| `fallback` | Nothing applies: 4096 tokens. The terminal status line shows no limit rather than a guess. |

The packing policy itself is unchanged:

- 35% of the window, at most about 16k estimated tokens;
- the newest turns kept verbatim;
- older material as labelled excerpts.

Restored history is user/assistant text plus excerpt summaries. It
carries no raw tool exchanges, so no tool-call/result pair can be split
there. Within a run, a tool result that would crowd out the answer is
shortened **in place**, so it stays paired with its call. The run's
events keep the whole result. A test checks that every call keeps its
result, in order.

Within a run, the prompt is budgeted as the provider's measured
`prompt_tokens` where reported, plus an estimate of what was added since.
Otherwise everything is estimated: system text, tool schemas, history and
tool or worker results, at one token per three bytes, plus 10%, plus a 2%
window margin. The estimate is cautious for English and code. It can be
wrong for unusual scripts or tokenizers, and the server's count decides.

### Output and reasoning settings, precisely

- **`max_output_tokens` is a ceiling per request.** When unset, nothing is
  sent and Lightweight lets a turn use all the room the prompt leaves.
  Setting it, for example to 2048, can only **lower** that allowance. It
  never adds capacity. It is sent capped at the window and, with a pool,
  at what the pool grants. The server may clamp further. The allowance the
  server actually applied is not reported back, so it is not shown as a
  fact.
- **`reserve_answer_tokens` is generation headroom.** It does not
  guarantee answer tokens. When the room left falls below it, results are
  shortened and tools withheld so the model has that much room to
  generate. Where reasoning and answer share one backend budget, as with
  Lightweight, a model may spend that room thinking. No visible-answer
  reservation exists or is claimed.
- **`reasoning_effort`** is sent only if it is listed in
  `reasoning_supported` for the model; otherwise the report says
  `unsupported`. It is a label the model's template interprets, not a
  token budget. `"none"` disables thinking where the backend and template
  honour it (Lightweight's pinned engine does). That is an optional,
  model-specific choice, not a recommended default.
- **Report vocabulary:**

  | Kind | Fields |
  |---|---|
  | configured | `configured_max_tokens` |
  | requested (what was sent) | `requested_max_tokens` |
  | estimated or measured | `prompt_estimate` with `prompt_basis` |
  | verified, as reported by the provider | `context_source: "provider"`, `prompt_tokens`, `completion_tokens` |

  Unknown values are `null`, never zero.

### Parent and worker bounds: what each limit covers

| Limit | Scope |
|---|---|
| `max_output_tokens` | per request (the lead's, each worker's, each continuation's) |
| context window | per request |
| `max_turns`, `max_tool_calls`, `wall_clock_secs` | per run; workers draw theirs from one `TreeBudget` sized from the lead's limits, so worker turns, calls and time are shared across the tree, on top of the lead's own |
| `max_workers_per_run` | per tree |
| `max_active_workers`, per-profile | concurrency, installation-wide and per profile (`Admission`) |
| continuations (#62) | per run, each spending one of that run's turns |
| **`max_run_output_tokens`** (new) | **aggregate generated tokens for the lead, all its workers and all continuations** |

Before #61 there was **no** aggregate token bound. The table's limits only
implied one, of (lead turns + tree turns) × window, which is not a token
allowance.

`max_run_output_tokens` creates an `OutputPool` per run. Workers share it
through `GenerationSlot`.

- **Before a turn.** The turn reserves up to its ceiling, or the window
  when no ceiling is set, and is sent no more than it was granted.
  Concurrent workers therefore can never together be granted more than
  remains.
- **After a turn.** The reservation is settled against reported
  `completion_tokens`.
- **Missing usage.** A turn with no reported usage, or one that was
  cancelled or failed, is charged its whole reservation. The account errs
  high, and `output_estimated` says so.
- **When the pool runs low.** Once less is left than the headroom, the
  next turn is a tool-less answer with what remains, and the run ends
  `OutputBudget`. With less than 16 tokens left, no turn is taken.
- **Contention.** A worker that finds the pool fully reserved by siblings
  stops with `OutputBudget`. Size the pool above
  `max_active_workers` × ceiling.

Unset means no pool, and runs behave exactly as before. Cancellation,
ownership, permissions and worker isolation are untouched: the pool only
lowers `max_tokens` or declines to start a turn.

### #62: bounded continuation (stacked)

After a `length` stop, the loop may take up to `generation.continuations`
more turns (default 1, at most 3) asking the model to carry on from where
it stopped. These turns are **answer-only**: no tools are offered, and any
tool call a continuation produces is not run.

A continuation is taken only when **all** of these hold:

- the window is known;
- visible text arrived (a reasoning-only stop is reported `MaxTokens`,
  not continued);
- no tool call was being streamed (its arguments may be cut off; it is
  never run, and completed tools are never replayed);
- the run is not cancelled;
- the run has a turn and time left in its own budget (each continuation
  spends one turn);
- the window still has at least 256 tokens of room once the partial answer
  is in;
- the run's output pool, if set, can still grant at least that much.

Limits:

- **Per request.** `max_output_tokens` applies to each continuation
  request exactly as to any other.
- **Per run.** Total generation is bounded only by `max_run_output_tokens`,
  which continuations reserve from like any turn.
- Without a pool, a run's total is at most (1 + continuations) × ceiling
  for its answer, inside its turn and time limits.

Text handling:

- The parts are replayed as **one** assistant message, so roles still
  alternate.
- A continuation's opening is held until it can be compared with the end
  of the answer so far. Only a **clear** restatement is trimmed: at least
  32 bytes, more than one word, and starting at a word boundary in the
  earlier text.
- Shorter, single-word or mid-word repeats are kept, so legitimate
  repetition (a refrain, a repeated identifier) is never removed. The cost
  is that a short restatement may appear twice.
- A model that starts its whole answer again is not detected.

Persistence and replay:

- SSE events keep their buffer ids, so a reconnect resumes without
  duplicates.
- The saved session concatenates the parts into one assistant message.
- A cancelled or failed continuation keeps the partial answer already
  emitted.

Completion status. The terminal reason says whether the answer finished:

- `EndTurn` (or the wrap-up's reason) when a continuation finished it;
- `MaxTokens` (`complete: false`) when it is still cut off after the last
  permitted continuation, or when no continuation was allowed;
- `generation.report` counts the continuations taken.

## Settings

Defaults. Nothing needs to be set, and with nothing set:

- no `max_tokens` and no reasoning field are sent;
- no run-wide pool exists;
- the window is discovered from Lightweight;
- history is packed against that window;
- headroom is min(1024, window ÷ 4) and applies only when a window is
  known.

Optional ceilings:

```json
{
  "generation": {
    "max_output_tokens": 2048,
    "max_run_output_tokens": 32000,
    "reserve_answer_tokens": 1024,
    "context_window": 16384,
    "models": {
      "some-thinking-model@32k": {
        "reasoning_effort": "none",
        "reasoning_supported": ["none"]
      }
    }
  }
}
```

Notes on these:

- `context_window` only narrows a reported window.
- The reasoning lines apply only to a model the operator has verified
  honours `"none"`. Leave them out otherwise.
- `lightagent config set generation.<key> <value>` works for
  `max_output_tokens`, `max_run_output_tokens`, `reserve_answer_tokens`,
  `context_window` and `reasoning_effort`. An empty value clears the key.

**Larger contexts are a server setting, configured separately.** Nothing
here loads a model, changes a running server or asks for a larger window.

- To serve a larger window, start Lightweight with `--ctx N`, or set
  Lightagent's `runtime.n_ctx` and run `lightagent runtime place <model>`.
- The window must be one the model supports and the machine can hold.
  Lightweight's memory admission refuses a load that does not fit; the
  Lightweight CLI's `estimate model.gguf --ctx N --kv-type q8_0` reports
  the cost first.
- Once it is loaded, Lightagent discovers it and uses it for history and
  budgets with no further setting.
- A very large window, such as 270K, works the same way, but only if
  separately configured and supported. It is not enabled here.

## Resource tradeoffs

- **Loaded context and memory.** A larger loaded window raises the
  memory a backend needs, chiefly KV cache. Lightweight sizes `n_ctx` per
  slot and multiplies it by its slot count at load. More slots or a larger
  window both cost memory, and admission refuses what does not fit.
- **Output allowance.** Within a loaded window, a larger `max_tokens`
  mostly costs decode time per turn. Whether it also changes memory
  depends on the backend: one that preallocates the full window per slot
  (as Lightweight's engine does at load) uses no more, while one that
  grows KV cache as tokens are generated uses more while the turn runs.
  This was not measured.
- **Concurrent users.** More slots let more requests run at once, each
  with its own window. Longer turns hold a slot longer, so others wait
  longer in the gateway's queue. No universal formula is claimed; it
  depends on the backend, slot count and request mix.
- **Workers.** Admission bounds how many run at once. The tree budget
  bounds their turns, calls and time. `max_run_output_tokens`, when set,
  bounds their generated tokens together with the lead's.

## Lightweight

No code change is needed. Its existing API provides:

- the effective window: `/v1/capabilities` from v0.4.0, the `/v1/models`
  row before that;
- `max_tokens` / `max_completion_tokens` (the smaller wins when both are
  sent; Lightagent sends only `max_tokens`);
- clamping by its own count;
- `reasoning_effort` passed through.

A larger window is configuration only (above). Upgrading the installed
0.3.25 to ≥ v0.4.0 gives the versioned capabilities contract, though
0.3.25 already works through the fallback. Nothing in the running server
was changed.

## Deferred and limitations

- **Jev-assisted task-budget selection is deferred.** Using Jev
  classification to choose budgets would be a new Jev influence path. That
  needs its own permission decision, and none is added here.
- Token figures before the first measured turn are estimates.
- Pool contention among concurrent workers is resolved by refusal, not
  waiting.
- A model can still be cut off. Then continuation (#62) or an honest
  `MaxTokens` applies.
- Reasoning support is whatever the operator declares. Lightweight does
  not advertise per-model reasoning controls.
- Real-model efficiency, speed and memory use are **unverified**. No
  live endpoint was used.

## Validation

GitHub-hosted Actions only, with deterministic scripted providers. Exact
commits, run links and check totals are in the PR descriptions.
