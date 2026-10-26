# Live evaluation: staged proposal

**Status: proposal. Nothing here is configured, dispatched or paid for.** Each stage below needs its own explicit authorization from the repository owner. A missing credential means **BLOCKED**, which is never reported as a pass.

The earlier proposal went straight to a full role evaluation: 7 tasks × 3 runs, at most 168 model requests and about 1.2M input tokens per model. This version stages it. A small, bounded pilot comes first, and each stage starts only if the previous one gave a usable result.

## Evidence categories, kept separate

| # | Category | What it shows | Where | Status |
| --- | --- | --- | --- | --- |
| 1 | Seeded baseline regressions | A gap existed and is closed | `subagent_baseline.rs`, `seeded_*` | done, in `check` |
| 2 | Scripted model, real tools | Mechanics: tools, contexts, policy, approvals, budgets, isolation, ordering, name mapping | `specialists.rs`, `worker_approvals.rs`, `coder_workflow.rs`, `workers/isolation.rs`, `coordinator::selection`, provider `names` | done, in `check` |
| 3 | Real GitHub Actions | The broker and the coder's commit against real hosted runs | `subagents` workflow, "Tester broker (live GitHub Actions)" | done, on every PR head |
| 4 | Real Jev selection accuracy | Whether Jev's recommendation, through Lightagent's rules, matches labels fixed in advance | `coordinator::evaluation::specialists`, `jev-selection-live.yml` | **BLOCKED**: needs authorization and a merge to `master` |
| 5 | General-model specialist competence | What a real model achieves in each role | `role_eval.rs`, `subagents` workflow, "Role evaluation (live models)" | **BLOCKED**: no inference endpoint |

The Jev key (`TYPESAFE_API_KEY`, in the `jev-live` environment) authorizes classification requests only. It is never used as an inference credential, and category 5 never reads it.

No category stands in for another. A scripted test never counts as model competence, and Jev selection accuracy says nothing about whether a worker then does good work.

## Stage 1: Jev selection accuracy (category 4)

| Item | Value |
| --- | --- |
| Endpoint | TypeSafe Jev `POST /v1/systemone` through the shared adapter, the model configured in the `jev-live` environment (default `jev-latest`) |
| Credential | `TYPESAFE_API_KEY`, already in the `jev-live` environment (manual approval, `master` only). Nothing new is configured. |
| Inputs | The 24 synthetic, hand-labeled prompts in `evaluation/subagents/selection.v1.json`: 12 the lead should keep and 12 that benefit from workers. They were recorded on 2026-10-21, before any request. |
| Requests | Exactly one per prompt: the classification request with the 11 typed questions. Default 24, workflow ceiling 48, no retries, one at a time. |
| Tokens | Usage is measured and reported. Estimated at about 0.5–1k input tokens per request, so under about 25k input tokens for the whole set. |
| Cost | No price is recorded in the repository. The request ceiling is the only enforced bound. Reported usage multiplied by the operator's contract price gives the cost. |
| Stop conditions | A credential error, three failed requests in a row, or the request ceiling. A stopped run fails and is not a pass. |
| Scoring (fixed) | Decision accuracy (plan built exactly when `delegate` is true), the unnecessary-delegation rate over the 12 lead cases, the missed-delegation rate, and role-set match where both delegate. Pass: accuracy ≥ 0.80 and unnecessary delegation ≤ 0.10. |
| Evidence kept | Per case: id, planned yes or no, decision and roles correct, fallback code. In total: requests, tokens, latencies and failure codes. No prompt text beyond the synthetic set, and no key. An artifact (`jev-selection-reports`) plus the job summary. |
| How to run | After the stack is merged: dispatch `jev selection (live)` on `master` with `max_requests=24`. The owner approves the `jev-live` deployment in the UI. |

What stage 1 does **not** measure: task completion, latency or cost savings, and the lead model's own selection. Comparing Jev's selection with the lead choosing on its own needs a general model (stage 2).

## Stage 2: general-model pilot (category 5, small)

Prerequisites, all separately authorized:
- an OpenAI-compatible inference endpoint the owner approves;
- a GitHub environment **`subagent-eval`** with the owner as required reviewer, deployments limited to `master`, and the secrets and variables below;
- the one-line switch of `role-eval-live` to `environment: subagent-eval`. Referencing an environment before it exists would create an unprotected one.

| Item | Value |
| --- | --- |
| Endpoint | `SUBAGENT_EVAL_BASE_URL` (environment secret), with no `/v1` suffix. `SUBAGENT_EVAL_API_KEY` (environment secret) is optional for a keyless gateway. |
| Model | `SUBAGENT_EVAL_MODELS` (variable): **one** model for the pilot |
| Tool names | `SUBAGENT_EVAL_TOOL_NAMES=portable` for endpoints that require `^[a-zA-Z0-9_-]{1,64}$` (OpenAI's API and copies of it). `native` for gateways that accept dotted names (vLLM, llama.cpp, Ollama, Lightweight). |
| Model capabilities | Streamed Chat Completions with tool calls, valid JSON arguments, several sequential tool calls, a context window of at least 16k (32k recommended). No vision, no JSON mode, no parallel calls. |
| Tasks | One per role: `eval_tasks=res-1,cod-1,rev-1,tst-1`, `eval_runs=1` |
| Request ceiling | `eval_max_requests=32` (4 runs × the worker's 8-turn cap). The harness refuses every request past the ceiling, and the run then fails as incomplete. |
| Tokens | At most about 32 × 7k, roughly 0.22M input tokens, and typically about 0.08M. Usage is in the report. |
| Stop conditions | The ceiling; the 90-minute job timeout; any 401 or 403, which fails every run immediately and is visible in the first `EVIDENCE` line. The operator cancels the run on unexpected spend. |
| Scoring (fixed) | The checks recorded in `roles.v1.json` for each task. With one run per task, the result is a smoke signal per role (pass or fail), not a rate. |
| Evidence | `role-eval.json`: typed worker results (answers, sources, tool use), requests used, ceiling, and whether the run stopped at the ceiling. Kept 7 days. No key. |

The pilot shows only whether the endpoint, the name mapping and the four roles function with a real model. It does not show competence rates.

## Stage 3: full role evaluation (category 5)

This runs only after a stage 2 pilot that worked, and with new authorization. It is the recorded set in full: 7 tasks × 3 runs per model, at most 168 requests and about 1.2M input tokens per model (typically about 0.3M), with the per-role pass rate of at least 0.67 from `roles.v1.json`.

## Stage 4: Jev selection against the lead's own selection

This compares, on the stage 1 prompts and one model, two paths: selection `active` and selection `off`, where the lead decides alone. Each prompt runs once per path, at most 48 runs with the role cap of 8 turns each. It is a separate proposal, with its own ceiling, to be written after stages 1 and 2. The metrics are decided before the run:
- appropriate role selection;
- unnecessary delegation;
- task completion against the case's labeled check;
- model and tool calls;
- wall time;
- tokens and cost where reported.

No efficiency or quality claim is made before this stage has run.

## What stays true at every stage

- Pull requests never read these secrets. The live jobs report BLOCKED on pull requests.
- Keys stay inside the provider and the adapter, and are never logged or uploaded.
- Every fixture is synthetic: loopback pages, a confined workspace, a GitHub stand-in. The model endpoint and Jev are the only external destinations.
- Nothing in this document configures a secret, creates an environment or dispatches a paid run.
