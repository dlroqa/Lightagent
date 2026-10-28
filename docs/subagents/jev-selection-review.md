# Live Jev specialist selection: review packet

**Status: prepared for review. Nothing has been dispatched, and no request has been sent.** This page is what the owner reviews before authorizing the run. It covers what the run sends, what it costs, when it stops, and how it can evaluate an unmerged commit without exposing the key.

## What the 24 prompts are

`evaluation/subagents/selection.v1.json` holds 24 synthetic prompts, written and labeled by hand on 2026-10-21 before any request. No prompt contains personal data or a credential. Labels are never changed after a run.

**12 that the lead should keep** (`delegate: false`). Selection is right if it plans nothing.

| Kind | Cases |
| --- | --- |
| Factual or conversational | sel-001 capital city, sel-002 thanks, sel-003 TCP vs UDP, sel-007 regex meaning, sel-008 haiku, sel-009 unit conversion, sel-012 five-word summary |
| Single-step searches (the "simple search stays with the lead" rule) | sel-004 latest Rust release, sel-005 one documentation page |
| Small code tasks | sel-006 rename one variable, sel-010 brief performance question, sel-011 list files |

**12 that benefit from workers** (`delegate: true`). Each lists every role set that counts as correct.

| Expected roles | Cases |
| --- | --- |
| coder + tester | sel-013 parser fix with CI regression tests, sel-016 validation with CI, sel-024 migration tested in CI. sel-021 also accepts coder alone. |
| researcher + coder | sel-014 research JWT rotation, then implement it |
| coder + reviewer | sel-018 refactor, then review for regressions |
| reviewer only | sel-015 security review of pending changes, sel-023 audit without changing code |
| tester only | sel-019 find the failing CI test |
| researcher only (complex) | sel-017 compare three papers with sources, sel-020 license survey |
| researcher + coder, coder + tester, or all three | sel-022 port an upstream fix and verify it in CI |

**Pass rule, fixed in the file:**
- decision accuracy ≥ 0.80, meaning a plan is built exactly when `delegate` is true;
- unnecessary delegation ≤ 0.10 of the 12 lead cases, so at most one.

Role-set match and missed delegation are reported but are not pass criteria.

## Requests: 24 per dispatch

- Each prompt costs **exactly one** Jev request: the classification request carrying the 11 typed questions. No other request is made: no connection probe, no routing request, no retry.
- `max_requests=24` covers **one complete pass over the set**, every prompt once. That is the recommended run.
- The ceiling is **24 per dispatch** (since #50; it was 48). A **second pass**, to check whether answers are stable from one call to the next or to finish a pass that was cut short, is a separate dispatch with a separate `jev-live` approval, and needs its own authorization.
- A run below 24 is **incomplete**. It scores only the prompts it reached and cannot meet the pass rule.

## Retries, tokens, cost

| Item | Value |
| --- | --- |
| Retries | **None.** The adapter's no-retry policy applies, and the pinned workflow's `curl` makes one attempt per prompt. |
| Timeout | 10 s per request |
| Input tokens | Measured: Jev reports usage on every answer (live baseline run 38311407479), and the report totals it. Estimated at about 0.6–1k input tokens per request (a state of at most 400 characters plus 11 short questions), so roughly **15–24k input tokens** for 24 requests. |
| Output tokens | Small: typed answers only. Measured and reported. |
| Hard limits | At most 24 requests per dispatch (the workflow input check; the harness's own ceiling is 48), and a request body of at most 16 KiB (the adapter's bound, checked again in the send job). Responses are capped at 64 KiB. There is no token cap beyond these. |
| Expected cost | **Not available.** No verified price is recorded: the `jev-live` environment has no `JEV_PRICE_*` variables. Cost is the reported tokens multiplied by the contract price. |

## Stop conditions

A run stops, and is reported as not passed, on any of:
- a credential error (401 or 403);
- three failed requests in a row;
- the request ceiling.

Without the key it does nothing and reports **BLOCKED — NOT EVALUATED**. That record is written in the summary, as an annotation, and as `status.json` in the run's artifact.

## Evaluating a pinned, unmerged commit safely

**Can a protected manual run evaluate a reviewed pull-request SHA before merging?** Yes, provided the key never reaches the pull request's code. The obvious approach does not achieve that: checking out the PR commit in the `jev-live` job and running its tests hands the key to code that has not been merged. `jev-selection-pinned.yml` splits the work instead:

| Job | Environment | Runs repository code? | Has the key? | Does |
| --- | --- | --- | --- | --- |
| `prepare` | none | yes, the pinned commit | **no** | Builds the commit and exports the exact request bodies it would send, one per prompt, as an artifact |
| `send` | `jev-live` (required reviewer, `master` only) | **no**: no checkout at all | yes | Validates each body with `jq` (only `model`, `state` and at most 11 choice/noul/score questions, ≤ 16 KiB, ids `sel-NNN`), sends it with `curl` to the fixed TypeSafe endpoint, and records the responses |
| `score` | none | yes, the pinned commit | **no** | Replays the recorded responses through the commit's own classification and plan rules, and scores them |

Protections:
- **The workflow definition is master's.** A manual dispatch always runs the default branch's file, so a pull request cannot change these steps. The first step also refuses any dispatch ref other than `master`.
- **The pinned commit must be the current head of an open pull request from a branch of this repository, or a commit already on `master`**: `master`'s dispatch commit or one of its ancestors, named by its full id. Both are checked through the API before anything is built. A fork's commit, a commit ahead of `master` or diverged from it, and any API error or unexpected answer are refused. (GitHub lists only the merging pull request for a commit already on the default branch, so a merged commit cannot be pinned through a pull request.) The run's summary records the workflow-source commit and the commit under test separately.
- **The prompt set is one of a fixed list.** The dispatch's `set` input is `selection.v1` (the default) or `selection-heldout.v1`, and nothing else. The guard pins each to the git blob id of its frozen file and refuses any other content at the commit under test. The export manifest, the harness report and the verdict must all name that set. `prepare` checks the export is exactly that set's prompts before anyone is asked to approve sending it. Only `selection.v1` may come from a harness that predates naming its set, such as `b1ffd79`; that harness exports `selection.v1` whatever is asked, so the check refuses it for any other set.
- **Only a complete run has a result.** `scripts/jev-selection-guard.py`, run from the workflow's own commit and never in `send`, decides the status after scoring. It is PASSED only when each of the 24 prompts was sent once and answered, and the send record, the exported requests and their digest, the responses and the harness report all agree, with the pass rule recomputed. A failed, missing, duplicate or malformed response, a stopped or partial run, or records that disagree make it **INCOMPLETE**, which fails the run; its ratios are kept apart as `diagnostic_metrics`. This matters because the harness scores a failed request as the lead keeping the task, which is correct for the 12 keep-with-lead prompts, so lone failures could otherwise pass. The status is in the summary and in `verdict.json` in the `jev-selection-reports` artifact. The verdict also reports delegation precision and recall, exact role-set match among plans and over every case labeled for delegation, and mean expected-role coverage. Each is recomputed from the frozen labels and checked against the harness where it reports the same measure. None of them changes the pass rule.
- **The reviewer approves `send` after `prepare` has finished**, and can download and read the `selection-requests` artifact first. Those are exactly the bytes that will be sent.
- **Nothing in `send` executes untrusted code.** Files are only parsed by `jq` and posted by `curl`. A malicious commit could at most send different typed questions to Jev, within the ceiling, under the reviewer's eyes, and never sees the key.
- **The digest binds what was reviewed to what is sent.** `prepare` publishes the SHA-256 of each request and of the set, in its summary and as a job output. `send` refuses to send unless the downloaded set matches it, and accepts only `sel-NNN.json` files (at most 24).
- `curl` is limited to HTTPS with no redirects, and the destination is fixed in the workflow file.
- The responses hold only Jev's typed answers and usage. A response that is not a JSON object, or that contains the key, is discarded.

**What it requires.** The workflow file must be on `master` to be dispatched there, because the environment allows only `master`. The commit under test does not need to be merged: only this one workflow file does. The code it runs, the export and replay tests, comes from the pinned commit, which must contain them (this PR or any later one). No protection is weakened. The environment, its reviewer and its branch policy are unchanged, and the pull-request run of this workflow executes `prepare` only, with no secret.

Steps, once authorized:
1. Merge only `.github/workflows/jev-selection-pinned.yml` to `master`, through its own small PR.
2. Dispatch it on `master` with `sha=<reviewed head of the top PR>` and `max_requests=24`.
3. Read the `selection-requests` artifact, then approve the `jev-live` deployment.

## What the result will and will not mean

It will measure how often Jev's typed recommendation, through Lightagent's fixed rules, matches the labels on these 24 prompts. It will not measure:
- task completion;
- specialist competence;
- latency or cost savings;
- how Jev compares with the lead choosing alone, which needs a general model (stage 4 of `live-evaluation-proposal.md`).
