# Generation cutoffs: diagnosis and output/context budgeting

Status: implemented on branch `fix/generation-budget` (draft PR #61) and,
stacked on it, `fix/length-continuation` (draft PR #62). Neither is merged.
Base: Lightagent `master` at `0a07e74`. Lightweight was read at `32530a3`
(v0.8.1) and at tag `v0.3.25`.

## The problem

Answers sometimes stop early. This document separates what the code does
from what is only plausible, says what could not be determined, and
describes the change.

Everything here comes from reading source. No application, model or
test was run locally. All validation ran on GitHub-hosted Actions with
scripted (mock) providers. Nothing here measures how well a real model
uses the extra room.

## Path traced

The traced path:

1. A prompt goes to `POST /runs`, the terminal or ACP, and on to
   `coordinator::preflight` (Jev routing and classification).
2. History is packed with `lightagent_store::model_history` and the run
   becomes an `AgentLoop`.
3. Each turn builds a `ProviderRequest`. `LightweightProvider` sends it as
   `POST /v1/chat/completions` with `stream_options.include_usage`.
4. On the Lightweight gateway, `routes.rs` counts the prompt with the
   engine's tokenizer, refuses `prompt >= n_ctx` with a 400 and clamps
   `max_tokens`. The llama.cpp upstream forwards `max_tokens` and
   `reasoning_effort`.
5. The streamed `finish_reason` and usage chunk come back to
   `FinishReason`/`Usage`, which the loop turns into `TurnCompleted` and
   `RunCompleted { reason }`.
6. From there the result reaches the SSE event names (`sse.rs`), session
   persistence (`record_run_events`, which concatenates every `Content`
   fragment), the WebUI (`describeStop` in `Agent.tsx`), ACP (`stop_reason`)
   and the terminal renderer.

## Findings

**Confirmed** means the code at the base commit does this, as read.
**Configuration** is a fact about the local installation, read from its
files without running it. **Hypothesis** is plausible but not shown.

| # | Finding | Class |
|---|---|---|
| F1 | Lightagent sent no output allowance: `loop_.rs` built every request with `max_tokens: None`. | Confirmed |
| F2 | Lightweight then lets a turn generate everything the prompt leaves: `clamp_max_tokens(None, n_ctx, prompt) = n_ctx - prompt - 1` (the same at `v0.3.25` and `v0.8.1`). With no allowance sent, a `length` finish from Lightweight means prompt, reasoning and answer together filled the loaded window. An output cap was not the limit. | Confirmed (code) |
| F3 | Only *restored* history was budgeted (`model_history`: 35% of the window, at most about 16k estimated tokens). Tool results added during a run were appended whole, up to `max_tool_output_bytes` = 256 KiB each. That is roughly 87k estimated tokens, far more than an 8k–32k window. So every retrieval result shrank the room left for the answer. | Confirmed |
| F4 | A prompt that outgrew the window got a 400 `ContextOverflow` from the gateway. The run then ended `Error`, not `MaxTokens`. | Confirmed |
| F5 | Reasoning shares the same budget. Lightagent never sent `reasoning_effort`, so the model's template default applied. Reasoning tokens count in `completion_tokens` and use the window, but are not replayed into later turns. | Confirmed (code); per-model effect unknown |
| F6 | On `master`, a `length` finish ends the run as `MaxTokens`. The text is kept, a tool call cut off mid-arguments is not run, and nothing continues. | Confirmed |
| F7 | The **installed** Lightagent 0.4.1 maps `FinishReason::Length` to the same path as `Stop` (`v0.4.1:loop_.rs:665`). A cut-off answer there is reported as complete (`EndTurn`). `master` fixed this in PR #30. | Confirmed (tag source) |
| F8 | The usage chunk (`prompt_tokens`, `completion_tokens`) was parsed and shown in the terminal status line, but never used for budgeting. | Confirmed |
| F9 | `serve` and `acp` pack history against `runtime.n_ctx`, or 4096 when unset, not against the window the model is actually served with. | Confirmed; not changed here |
| F10 | Installed: `lightagent/0.4.1` (binary of 2026-09-30) and gateway `hermes-gateway-0.3.25`. `runtime.n_ctx` is unset and the base URL is loopback `:11434`. Gateway 0.3.25 has no `/v1/capabilities`; its `/v1/models` row carries the effective `n_ctx` and the `hermes` namespace. | Configuration |
| H1 | In long research runs, accumulated tool results left little room in the window, so the final answer hit `length` (F2 + F3). | Hypothesis |
| H2 | A thinking model's reasoning used most of the room before any visible text (F5). | Hypothesis |

### Not determined

The affected model, its loaded `n_ctx`, the prompt size of the failing turn
and the provider's finish reason are all unknown. No run logs, session
files or live endpoint were examined (privacy, and the rule against local
execution). On the installed 0.4.1, a length stop and a normal end both
look like `EndTurn` (F7), so even a saved stop reason cannot tell them
apart there.

"3 of 5 sources read" is **not** evidence of token exhaustion. These can
each leave sources unread:

- the turn budget (default 8);
- the tool-call budget (32);
- the 300 s wall clock;
- `rag.realtime`'s own deadline;
- a failed fetch;
- a 400 overflow (F4);
- the model choosing to stop.

PR #30 made the loop say which one ended a run. The report added here
(below) records each turn's window, prompt size, allowance and finish
reason, so the next occurrence can be told apart.

## What changed

### PR #61: budget each turn against the served window

A run now carries a `GenerationBudget` (`lightagent_core::generation`). It
is assembled per run by `lightagent::generation::for_run`:

- **Context window.** It is read from Lightweight's versioned
  `/v1/capabilities` contract. For gateways older than v0.4.0 it is read
  from the `/v1/models` row, and only when that row carries Lightweight's
  `hermes` namespace. It is never inferred from a model name. It counts
  only if the reported model is the one the run resolved.
  `generation.context_window` can narrow it but never stretch it. Nothing
  is sent to the server and nothing is reloaded, so memory admission is
  untouched.
- **Output allowance.** `generation.max_output_tokens` is sent as
  `max_tokens`, never above the window. The server clamps it again by its
  own count. When unset, nothing is sent, as before.
- **Reasoning.** `generation.reasoning_effort` is sent only if it is listed
  in `generation.reasoning_supported`. Otherwise it is reported as
  `unsupported` and not sent. A label is not a token budget. Reasoning and
  answer share one allowance, and Lightagent cannot set part of it aside
  for the answer.
- **Answer reserve.** `generation.reserve_answer_tokens` defaults to the
  smaller of 1024 and a quarter of the window, and applies only when the
  window is known.
  - A tool result that would leave less room is shortened in what the
    model sees, with a note saying so. The run's events, SSE and saved
    session keep the whole result.
  - The next turn is then a tool-less answer.
  - If the conversation alone leaves less room than the reserve, tools are
    withheld too.
  - Either way the run ends with the new `StopReason::ContextLimit`
    (WebUI: "Research filled the model's context window…").
- **Prompt size.** Where the provider reported an earlier turn's
  `prompt_tokens`, that measured count is used, plus an estimate of what
  was added since. Otherwise the whole prompt is estimated: system text,
  tool schemas, history, tool results and worker results, at one token per
  three bytes, plus 10%, plus a 2% window margin. The estimate is cautious
  for English and code. It can be wrong for unusual scripts or tokenizers,
  and the server's count still decides.
- **Diagnostics.** When a budget is active, each turn emits a
  `generation.report` SSE event, which is also stored in the run buffer. It
  records:
  - the window and its source;
  - the prompt figure and its basis (`measured` or `estimated`);
  - the requested `max_tokens` (`null` when none was sent);
  - the reasoning status;
  - the number of tools offered and of shortened results;
  - the provider's finish reason and counts;
  - for a `length` stop, whether the allowance or the window ran out, or
    `unknown`.

  It contains only numbers and fixed codes, plus the operator's own
  reasoning label: no prompt, tool output or reasoning text.
- **Workers.** A `GenerationSlot` on `Delegation` carries the lead's budget.
  A worker never gets a larger allowance. It gets the window and reasoning
  label only when it reaches the same model at the same endpoint.

### PR #62: bounded continuation

After a `length` stop, the loop may take up to `generation.continuations`
more tool-less turns (default 1, at most 3) that ask the model to pick up
where it stopped. It does so only when all of these hold:

- the window is known;
- visible text arrived;
- no tool call was being streamed (its arguments may be cut short, and it
  is never run);
- the run is not cancelled;
- the run has a turn and time left in its own budget;
- the window still has at least 256 tokens of room once the partial answer
  is in.

How the continued text is handled:

- The parts are replayed as **one** assistant message, so roles still
  alternate.
- A restated ending of 16 bytes or more is trimmed before it is shown or
  saved.
- A model that restarts its whole answer is not detected; the instruction
  asks it not to.
- A cancelled or failed continuation keeps the partial answer.
- Each continuation spends one of the run's turns, which bounds workers
  within their tree's grant.
- Persistence and SSE replay need no change: content is concatenated in
  order and every event keeps its buffer id.

### Before and after

| Situation | Before | After |
|---|---|---|
| Nothing known, nothing configured | no `max_tokens`, run as before | identical: no field, no report, no change |
| 8k window, five 20 KB retrieval results | each appended whole; prompt overflows → 400 → `Error`, or the answer gets a few tokens → `MaxTokens` | the result that would crowd the answer is shortened; next turn answers → `ContextLimit` with a full-length answer |
| Answer hits `length` with room left (#62) | `MaxTokens`, partial text | one tool-less continuation; overlap trimmed; `EndTurn` if it finishes |
| Answer hits `length` mid tool-call | call dropped, `MaxTokens` | unchanged; never continued |
| `reasoning_effort = "low"` configured but not declared | n/a | not sent; report says `unsupported` |

## Configuration

```json
{
  "generation": {
    "max_output_tokens": 2048,
    "reserve_answer_tokens": 1024,
    "reasoning_effort": "none",
    "reasoning_supported": ["none"],
    "continuations": 1,
    "models": {
      "qwen3-8b-q4_k_m@32k": { "max_output_tokens": 8192, "reasoning_supported": [] }
    }
  }
}
```

- Every field is optional. Model keys are the exact id the provider lists.
  A model's override replaces the matching setting, field by field.
- `lightagent config set generation.max_output_tokens 2048` (likewise
  `reserve_answer_tokens`, `context_window`, `reasoning_effort` and, with
  #62, `continuations`). An empty value clears a setting.
- `reserve_answer_tokens = 0` turns the context guard off.
  `continuations = 0` turns continuation off.
- Validation refuses: zero allowances or windows, a reserve that is not
  smaller than a configured window, empty labels, and more than 3
  continuations.

## Resource tradeoffs

- **Memory.** It is set by the window the server *loaded*, not by
  `max_tokens`. Lightweight sizes the KV cache for `n_ctx` per slot, times
  the slot count. Raising the output allowance does not add memory; it
  lengthens a turn.
- **Time.** A larger allowance or a continuation means more decode time per
  turn. Both stay inside the run's wall clock (300 s by default) and turn
  budget, and a continuation spends a turn.
- **Concurrency.** Workers draw turns from one `TreeBudget` and slots from
  `Admission`. Allowances do not multiply this: a worker's allowance is at
  most the lead's, and its continuations come out of its granted turns.
  The most one run can generate is bounded by
  (lead turns + tree turns) × the window.
- **A bigger window** is a server setting, with a memory cost. See below.

## Lightweight: configuration only, no code change

Lightweight's existing API is sufficient:

- the effective window, from `/v1/capabilities` (≥ v0.4.0) or the
  `/v1/models` row;
- `max_tokens` and `max_completion_tokens` (the smaller wins when both are
  sent; Lightagent sends only `max_tokens`);
- clamping to the room its own tokenizer count leaves;
- `reasoning_effort` passed through (`"none"` disables thinking at the
  pinned engine build; other labels go to the model's template).

No server gap was found. The only server-side lever is configuration:

- **A larger loaded window.** Start Lightweight with `--ctx N` (with no
  `--ctx` it picks the largest window that loads safely), or set
  Lightagent's `runtime.n_ctx` and run `lightagent runtime place <model>`.
  Check the memory first with the Lightweight CLI's `estimate model.gguf
  --ctx N --kv-type q8_0`. The KV cache grows linearly with `n_ctx` and with the
  number of slots, and the gateway refuses a load that does not fit.
- **Upgrading the installed gateway** (0.3.25) to ≥ v0.4.0 gives the
  versioned capabilities contract. 0.3.25 already works through the
  `/v1/models` fallback.

Nothing in the running server was changed.

## Limitations

- Token figures before the first measured turn are estimates.
- A model that ignores the reserve and writes past it can still be cut
  off; then continuation (#62) or the honest `MaxTokens` applies.
- Reasoning support is whatever the operator declares. Lightweight does
  not advertise per-model reasoning controls, so nothing is inferred.
- `serve`/`acp` history packing still assumes `runtime.n_ctx` or 4096
  (F9).
- The shortened-result note can prompt a narrower follow-up search only
  if room remains, and by construction it rarely does.
- Jev task classification is **not** used to pick budgets in this change.
  Doing so would be a new influence path that the Jev permission decisions
  do not yet cover. It needs an owner decision; see the PR description.
- Live efficiency against a real model is **unverified**. No approved
  endpoint was used.

## Validation

GitHub-hosted Actions only. Required checks use deterministic mocks:

- `crates/lightagent-core/src/generation.rs` (unit);
- `crates/lightagent-core/tests/generation_budget.rs` (loop);
- `crates/lightagent-provider-lightweight/tests/context_window.rs`
  (loopback discovery);
- the provider body test (both token spellings, reasoning field);
- config, SSE and worker-slot tests.

Exact commits and run URLs are recorded in the PR descriptions.
