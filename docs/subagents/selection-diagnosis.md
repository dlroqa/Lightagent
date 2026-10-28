# Specialist selection: diagnosis of live run 40503493185

The first live Jev specialist-selection run (2026-10-27, `jev-1.14.0`, code `b1ffd79`) was **BELOW_THRESHOLD**:
- decision accuracy 12/24;
- unnecessary delegation 0/12;
- missed delegation 12/12;
- every task was kept with the lead.

That result stands. Its requests, Jev's answers and the verdict are kept byte for byte in `evaluation/subagents/runs/2026-10-27-40503493185/`. `the_recorded_result_is_kept_as_the_baseline` holds them to it.

Everything below comes from replaying those recorded answers through Lightagent's own parsing and rules on GitHub Actions (`diagnose_the_recorded_run`, report `selection-diagnosis`). It is **development evidence**. It shows what the rules do with these answers. It cannot show how Jev would answer other prompts or changed questions.

## Trace: input, Jev, parsing, rules, plan

**Input.** The state sent was exactly the prompt for all 24 cases; the sanitizer removed nothing. The requests were byte-identical to what the code exports.

**Jev's raw answers** (per prompt in the `selection-diagnosis` report):
- **Keep-with-lead prompts (12):**
  - All 12 were rated simple, with a complexity score of 0.00–0.14.
  - `specialists` was 0.03–0.10.
  - Most roles were low. The exceptions were research prompts, which scored researcher at 0.71–0.94, and the rename prompt, which scored coder at 0.88.
- **Delegate prompts (12):**
  - All 12 were rated moderate or complex (0.93–1.94).
  - `specialists` was 0.26–0.68.
  - The expected roles mostly scored ≥ 0.7. At the 0.7 threshold, Jev's role answers covered 95.8% of the expected roles on average and matched the expected set exactly in 7/12.

**Parsing.** Polarity was read correctly: `noul` is a yes-probability. Confidence is read only from choice and score answers, and identifiers mapped as asked.

**Rules.** Permissions, readiness and limits affected **no** decision; every role was ready and `agent.delegate` was permitted. Two Lightagent rules ended all 12 delegate cases, each on its own:

| Rule (earlier code) | Threshold | Delegate cases it ended | Would also end, if passed |
| --- | --- | --- | --- |
| Category confidence below its threshold → `low_confidence` | 0.9 on the 6-way category choice | 10 (sel-013–015, 017–020, 022–024) | all 10 then hit the next rule |
| `specialists` below the selection threshold → `delegation_not_useful` | 0.7 | 2 (sel-016, sel-021) | the other 10 too: the highest was 0.68 |

Applying only the role answers and the later rules (fit, single-step research, readiness, cap, budget) to the same answers plans all 12 delegate cases with 11/12 exact role sets, and keeps all 12 lead cases with the lead (`simple_task`). Those rules were not the problem.

## Causes

1. **Category confidence was a gate on the wrong decision.** That confidence concerns which of six categories the task belongs to. A task with several parts is naturally split between categories that admit the same workers. For example, sel-013 was 76% coding and 23% multi-step, and sel-022 was 86% multi-step and 9% coding. Selection uses the category only to check that a role fits, yet a low category confidence ended selection before any role was considered.

2. **An uncertain `specialists` answer was treated as a no, and the question is stronger than the plan model.**
   - **No confidence of its own.** `specialists` is a yes-probability with no confidence of its own. Values of 0.26–0.68 are uncertain, not negative, and they rank the cases correctly: every delegate case scored above every lead case.
   - **The wording sets a higher bar.** The question asks whether the task would *clearly* benefit from being split among *different* assistants "rather than handled directly by one assistant". The role questions ask whether a *distinct piece* of each kind of work is needed. Five of the twelve delegate labels expect a single specialist, which a "split among different assistants" question can reasonably answer no.
   - **Same threshold, different scales.** The same 0.7 threshold applies to both kinds of question, although they are not on the same scale.

A low `specialists` score alone is not evidence of a model defect: the answer was consistent with the question asked.

## Correction (smallest justified)

`selection.rs` changes. The questions, the configured thresholds and the request do not.

1. **The category's confidence no longer ends selection.** A role fits when the accepted category is one it serves, as before. It also fits when the categories it serves hold at least the **same** category threshold (0.9) of the category distribution.
   - The categories are the options of one choice question, so they exclude each other and their shares add up.
   - Selection accepts a distribution only if it names every category exactly once, with finite values in [0, 1] that sum to 1 within the adapter's 0.02. Anything else is malformed, and the classification falls back as a whole. The adapter already enforces this; selection checks it again.
   - A role whose fit cannot be established because the category is uncertain is reported as `low_confidence` and not planned. If no role remains, the task ends `low_confidence`.
   - A chat (the most likely category is conversation) and a simple task still stay with the lead, whatever the confidence.
2. **Only a confident no on `specialists` keeps the task with the lead:** `≤ 1 − threshold`, which is 0.3 at the default 70%.

### This is a changed decision policy

The configured numbers are unchanged (`selection_threshold_percent` 70, category threshold 0.9), but the **effective bar for delegating is lower**:

- **Before:** delegation required `specialists ≥ 0.7`, and the category's own confidence had to be at least 0.9.
- **Now:** a `specialists` answer above 0.3 may proceed to a plan. This happens only when there is strong role evidence: each planned role scores at least 0.7. All remaining checks must also pass:
  - not a simple task, and not a chat;
  - the role fits the task by the 0.9 category rule;
  - not a lone researcher on a task that isn't complex;
  - the role is ready for the requesting profile;
  - `agent.delegate` (or `agent.research`) is permitted by the run's tools and approval policy;
  - the lead's budget has room;
  - the worker cap.
- **Unchanged:** an uncertain `specialists` answer with no role at 0.7 still plans nothing (`no_role_needed`). Shadow mode only records. Active selection stays off unless an operator enables it per profile.

Tests pin each boundary:
- `specialists` 0.3 ends selection, 0.31 does not;
- a role at 0.7 is recommended, 0.69 is not;
- category shares summing to 0.9 fit, 0.89 do not; an accepted category at exactly 0.9 fits as before;
- simple tasks, single searches, chats, refused roles, approval-policy refusal, budgets, shadow mode, and malformed, non-finite or inconsistent distributions are all covered.

Classification itself is unchanged. The category, its confidence, complexity, planning, retrieval and tools are parsed and accepted exactly as before. The classifier only hands the category distribution it already received to selection. With selection off (the default) the selection answers are not even read, and the deterministic Jev evaluation suite, unchanged, passes on GitHub Actions.

Rejected alternatives:
- lowering any threshold (arbitrary);
- matching prompts;
- rewording the questions (replay cannot validate this; it needs its own live run);
- ignoring `specialists` entirely (a confident no should still count).

## Before and after on the recorded answers

These are the same answers to the same questions, through the earlier rules and then the corrected ones. This is **development evidence**: the rules were corrected after seeing these answers, so it is not evidence of generalization. The "after" column is asserted by `the_corrected_rules_on_the_recorded_answers` on GitHub Actions.

Confusion matrices (rows: label; columns: decision):

| Before (recorded) | planned | lead kept |
| --- | --- | --- |
| labeled delegate (12) | 0 | 12 |
| labeled lead (12) | 0 | 12 |

| After (corrected, replayed) | planned | lead kept |
| --- | --- | --- |
| labeled delegate (12) | 9 | 3 |
| labeled lead (12) | 0 | 12 |

| | Before | After |
| --- | --- | --- |
| Decision accuracy | 12/24 (50%) | 21/24 (87.5%) |
| Unnecessary delegation | 0/12 | 0/12 |
| Missed delegation | 12/12 | 3/12 |
| Delegation precision | n/a (0 plans) | 9/9 |
| Delegation recall | 0/12 | 9/12 |
| Exact role set, among plans | 0/0 | 9/9 |
| Exact role set, over all 12 delegate cases | 0/12 | 9/12 |
| Mean expected-role coverage, over all 12 delegate cases | 0% | 75% (9 at 100%, 3 at 0%) |

Under both rule sets, all 12 lead cases stay with the lead because the task is simple (`simple_task`). Before, 5 of them first hit `low_confidence`.

### The three remaining misses

- **sel-016:** "Add input validation to the signup form handler and make sure the existing test suite still passes in CI." Labeled coder + tester.
  - **Ended by** `delegation_not_useful`: `specialists` was 0.28, a confident no under the new rule.
  - **Other answers:** coder 0.92, tester 0.66, reviewer 0.67; 95% coding; moderate.
  - **Even past that rule,** only the coder would be planned, since the tester was under 0.7. The label is itself arguable (see below).
- **sel-019:** "Our nightly CI workflow started failing yesterday; find out which test fails on the latest commit and why." Labeled tester.
  - **Ended by** `delegation_not_useful`: `specialists` was 0.26.
  - **Other answers:** tester 0.79, researcher 0.58; category split 37% multi-step, 23% coding, 20% research, 16% analysis.
  - **Even past that rule,** the tester's fit (coding or multi-step, 60%) is under 0.9, so it would end `low_confidence`. The label is disputed.
- **sel-024:** "Write a database migration … and test it with the CI database job." Labeled coder + tester.
  - **Ended by** `low_confidence`: coder 0.90 and tester 0.84 were recommended, but coding and multi-step held 0.89 of the distribution, just under 0.9. Analysis was 0 and automation 0.11.
  - **Kept deliberately:** the 0.9 threshold was not moved to admit it.

## Disputed labels (kept, not relabeled)

- **sel-015 and sel-023** (one reviewer), **sel-017 and sel-020** (one researcher) and **sel-019** (one tester) label single-specialist delegation. That is consistent with the plan model, which allows single-worker plans, but not with the wording of the `specialists` question.
- **sel-019** ("find out which test fails on the latest commit and why") is labeled tester. It could equally be read as an investigation by a researcher or a coder; Jev gave tester 0.79 and researcher 0.58.
- **sel-016** expects coder and tester for "make sure the existing test suite still passes in CI". Whether running an existing suite is a distinct tester's piece of work is arguable; Jev gave tester 0.66.

## Held-out set and a bounded live proposal

`evaluation/subagents/selection-heldout.v1.json` holds 24 new prompts (12 to delegate, 12 to keep). They were labeled on 2026-10-27, after this diagnosis but before any request with them. The set has the same pass rule, and its scoring is fixed in the file. Several keep-with-lead prompts are moderate but single-piece, to catch over-delegation by the corrected rules.

**Frozen.** The prompts, labels and scoring rules are fixed, and `the_held_out_set_is_frozen` fails CI on any byte change. This holds until a live run on it has been reported. If any rule, threshold or question is ever tuned against this set's responses, the set is relabeled as development data, and a new held-out set is written and frozen before any held-out claim is made.

Proposal, each step needing its own authorization:
1. **Review and merge** this correction. Selection stays `off` by default, and active selection is not enabled.
2. **Prepare a small PR** that lets `jev-selection-pinned.yml` and its guard take the prompt set as an input, `selection.v1.json` or `selection-heldout.v1.json`, defaulting to v1. The input must be from a fixed list. Today both read `selection.v1.json`.
3. **Run one pinned live pass** over the held-out set, on the merged master commit:
   - 24 requests, no retries, and the same stop conditions;
   - the reviewed export, checksum and owner approval of `send`;
   - an estimated 24k input and 6k output tokens, as on 2026-10-27.
   - The result is reported as PASSED, BELOW_THRESHOLD or INCOMPLETE, with the confusion matrix, precision and recall, role measures, the returned model identifier, and which kind of rule kept each task.
4. **No tuning between steps 2 and 3,** and none against step 3's responses without the relabeling above.
5. **Optionally, and separately:** a rewording of the `specialists` question would need its own held-out live run. Replay cannot evaluate it.

A pass on the held-out set would measure Jev-assisted selection on those 24 prompts only. It would not show specialist competence, efficiency, or that selection beats the lead choosing alone.
