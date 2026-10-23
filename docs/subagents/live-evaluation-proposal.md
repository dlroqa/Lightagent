# Live role evaluation: configuration proposal

**Status: proposal, not configured.** No model endpoint exists for this repository today. The only secret, `TYPESAFE_API_KEY` in the `jev-live` environment, is Jev's classification credential. It is **not** an inference credential and must not be used as one. Until an operator configures the items below, the `Role evaluation (live models)` job records **BLOCKED**, and no model-quality result is claimed.

## What runs

`crates/lightagent/tests/role_eval.rs`, test `live_role_evaluation`, runs the recorded set `evaluation/subagents/roles.v1.json`.
- **Set**: 7 tasks covering all four roles. The set was recorded on 2026-10-21, before any run.
- **Runs**: 3 per task, per model.
- **Pass rule**: a run passes only if every check holds. A role passes at a rate of at least 0.67.

Each run drives the real worker runtime and real tools against local fixtures: loopback web pages, a confined workspace and a GitHub stand-in. The model endpoint is therefore the job's only external destination.

## Endpoint contract

The harness uses Lightagent's existing provider (`lightagent-provider-lightweight`), unchanged.

| Item | Requirement |
| --- | --- |
| Protocol | OpenAI-compatible Chat Completions: `POST {base}/v1/chat/completions`. `{base}` is the URL **without** `/v1`. |
| Auth | `Authorization: Bearer <key>`, or no key for a keyless gateway. |
| Streaming | `stream: true` with `stream_options.include_usage: true`. SSE `data:` chunks carry `choices[0].delta.content` and `choices[0].delta.tool_calls[]` (indexed, with arguments streamed as JSON text), a `finish_reason` (`stop`, `tool_calls` or `length`), and `[DONE]`. A stream that ends without a finish reason is recorded as `Incomplete`, not as an answer. |
| Tools | `tools: [{ "type": "function", "function": { name, description, parameters } }]`, with JSON Schema parameters. **Function names contain dots** (`web.search`, `fs.read`, `actions.dispatch`, `patch.propose`). An endpoint that enforces `^[a-zA-Z0-9_-]{1,64}$`, as OpenAI's own API does, rejects them. Use a gateway that accepts them, such as vLLM, llama.cpp server, Ollama's OpenAI endpoint or Lightweight; otherwise a name-mapping layer must be added first, which is not part of this proposal. |
| Usage | `usage` with `prompt_tokens`, `completion_tokens` and `total_tokens` in the final chunk. It is recorded, not required. |
| Other parameters | No `temperature`, `max_tokens` or `tool_choice` is sent. The endpoint's defaults apply. |

## Model capability requirements

- Reliable function calling: valid JSON arguments that follow the schema, and several sequential tool calls in one run. Parallel tool calls are not needed, because workers run tools one at a time.
- A context window of at least 16k tokens; 32k is recommended.
- Instruction following for a roughly 1–2k-token system prompt, made of the role skill and output contract.
- No vision, audio or JSON mode.

## Expected volume, per model

| Quantity | Bound | Basis |
| --- | --- | --- |
| Runs | 21 | 7 tasks × 3 runs |
| Chat requests per run | ≤ 8 | Built-in worker limit `max_turns = 8`. The tester polls through tools, not the model. |
| Chat requests in all | ≤ 168 | |
| Input tokens per request | ≈ 2–7k | System prompt about 1.5–2k, tool schemas about 1k, and a transcript of small fixtures |
| Input tokens in all | ≤ ~1.2M; typical about 0.3M | |
| Output tokens in all | ≤ ~50k | |
| Wall time | ≤ 150 s per run; job timeout 90 min | Runs are serial, with one request in flight at a time |

`SUBAGENT_EVAL_MODELS` may list several models, separated by commas. The volume scales linearly with the number of models.

## How the secrets are protected

1. Create the repository environment **`subagent-eval`**:
   - required reviewers: the repository owner;
   - deployment branches: `master` only.

   This is a settings change for the operator. It is not made here.
2. Store, **as environment secrets** rather than repository secrets:
   - `SUBAGENT_EVAL_BASE_URL`;
   - `SUBAGENT_EVAL_API_KEY` (omit it for a keyless gateway);
   - the variable `SUBAGENT_EVAL_MODELS`.
3. Make the job **`workflow_dispatch` only**, with `environment: subagent-eval` and `permissions: contents: read`. It must never run on pull requests, so PR code never runs while those secrets are present. The current job only reports BLOCKED on pull requests and reads no secrets there. The one-line switch to the environment is listed under "Activation", because referencing an environment that does not exist creates an unprotected one.
4. In the process, the key is kept only by the provider, whose `Debug` output redacts it. It is sent only to `SUBAGENT_EVAL_BASE_URL`, never logged, and never written to the report artifact. The artifact holds the typed worker results: answers, sources and tool use. It is kept for 7 days.
5. The Jev key is never read by this job.

## Activation (operator, separately authorized)

1. Create the environment and its secrets as above.
2. Merge the subagent stack, so the workflow exists on `master` and can be dispatched.
3. In `subagents.yml`, give `role-eval-live` the line `environment: subagent-eval` and `if: github.event_name == 'workflow_dispatch'`.
4. Dispatch the workflow on `master` and read `role-eval.json` and the job summary. A role below its recorded threshold fails the job.
