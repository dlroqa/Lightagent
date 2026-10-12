# Independent label review

The 101 labels in `benchmark.v1.json` have one author and no independent
review. Until they are reviewed, every live accuracy figure is exploratory
and no threshold or default may be derived from them. This directory defines
how the review is done.

## The package

CI builds it on every run of the `jev evaluation` workflow
(`scripts/jev-review-package.py`) and uploads it as the
`jev-label-review-package` artifact. Use the package from the master commit
whose `manifest.json` names the dataset version under review.

| File | Who sees it | When |
| --- | --- | --- |
| `blind-worksheet.csv` | Reviewers | Phase 1 |
| `author-labels.csv` | Reviewers | Phase 2, after **every** reviewer has submitted |
| `review-key.csv` | Coordinator only | Throughout |
| `manifest.json` | Everyone | Throughout |

The worksheet is blinded:
- **Review ids:** neutral ids (`R001`…) in a shuffled order. Case ids are left out because their prefixes name categories.
- **Withheld:** groups, the author's labels, and every model prediction, mock or live.
- **Shown:** each case's capability fixture, whether it carries an attachment, and whether it runs as a profile that may send task descriptions. Those facts decide some privacy expectations.

## Roles

- **Coordinator:** distributes the files, collects submissions, computes
  agreement, runs adjudication and edits the dataset. Never a reviewer for
  the same round.
- **Reviewers:** at least two, independent of the author and of each other.
  Each works alone from the worksheet and the labeling rubric
  ([`../README.md#labeling-rubric`](../README.md#labeling-rubric)).

## Phase 1: independent labeling

1. Each reviewer receives `blind-worksheet.csv`, the rubric and this file.
2. They fill every label column (see the schema below) without seeing the
   author's labels, any Jev output, or another reviewer's work, and without
   running any case through Jev.
3. They submit the CSV, or JSON valid against
   [`annotation.schema.json`](annotation.schema.json), including the three
   declarations: no author labels, no model predictions, no consultation.

## Phase 2: disagreement review

1. The coordinator joins the submissions to `author-labels.csv` through
   `review-key.csv` and computes, per field, raw agreement and Cohen's kappa
   for each reviewer pair. For category, kappa is computed on the primary
   label; a looser "acceptable-set overlap" rate is reported beside it.
2. **Agreement rules:**
   - Every primary category matches: accepted.
   - The primaries differ but each lies in the others' acceptable sets: accepted. The acceptable set becomes the union.
   - Anything else is a **disagreement**.
   - For planning, retrieval and tools, a split between `yes` and `no` becomes `either` unless adjudication settles it.
   - A `refuse` versus `send` split is always a disagreement.
3. Each disagreement goes to adjudication. The reviewers and the author
   meet, now with `author-labels.csv`, still **without** any model output.
   The outcome and a one-line rationale are recorded per case.
4. A case whose task is itself unclear may be rewritten instead of labeled.
   It then gets a **new id**, and the old id is retired.

## Phase 3: release

1. Apply the adjudicated labels to a new minor version
   (`benchmark.v1.json` `version` 1.1.0), with each changed case's
   rationale in `notes`.
2. Update `provenance`: the reviewers (pseudonymous ids), the dates, the
   agreement statistics, and `review: independently reviewed`.
3. Add a changelog entry. Live results are comparable only within one
   dataset version, so earlier live runs keep their old version.
4. Only after this may live measurements be described as other than
   exploratory, and only after a live run on the reviewed version may a
   threshold proposal be considered.

## Rules that are never broken

- No label is changed to agree with Jev, and no model prediction is shown
  to a reviewer before their initial labels are submitted.
- Alternatives are decided during labeling, never after seeing a
  prediction.
- No reviewer data beyond a pseudonymous id is stored in the repository.
- No paid Jev request is part of the review.

## Annotation schema

| Column | Values | Rule |
| --- | --- | --- |
| `primary_category` | `conversation`, `research`, `coding`, `analysis`, `automation`, `multi_step` | Empty only when `privacy` is `refuse` |
| `acceptable_alternatives` | `\|`-separated categories | Never repeats the primary; leave empty when no second reading is defensible |
| `planning`, `retrieval`, `tools` | `yes`, `no`, `either` | `either` when both answers are defensible |
| `privacy` | `send`, `refuse` | `refuse` when the gates must stop the request |
| `refusal_reason` | `key_material`, `attachment`, `profile_not_permitted`, `input_too_long`, `empty_after_sanitization`, `other` | Required with `refuse` |
| `label_confidence` | `high`, `medium`, `low` | Required with `send` |
| `notes` | free text | Required for `low` confidence or any alternative |

The workflow eligibility label is not reviewed: it is derived from the
primary category and the capability fixture, and a test checks it against
the workflow policy.
