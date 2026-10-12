# Jev benchmark dataset

`benchmark.v1.json` is the labeled, synthetic dataset Lightagent's Jev
evaluation runs against. `mock-responses.v1.json` scripts the stand-in Jev
used by the deterministic contract run. Both are read only by test code; the
shipped binary contains neither.

The evaluation design, metrics and policies are in
[`docs/architecture/jev-evaluation-and-calibration.md`](../../docs/architecture/jev-evaluation-and-calibration.md).

## Composition (v1.0.0)

101 cases: 75 in the `benchmark` split and 26 in the `calibration` split.
93 are expected to be sent for classification and 8 must be refused.

| Group | Cases | What it covers |
|---|---:|---|
| conversation | 10 | Chat, short writing, direct questions |
| research | 9 | Finding or verifying current or external information |
| coding | 10 | Writing, debugging or changing code |
| analysis | 8 | Reasoning over information the user supplies |
| automation | 7 | Actions that change files, systems or services |
| multi_step | 7 | Several distinct parts across categories |
| ambiguous | 8 | Mixed or underspecified requests with several acceptable labels |
| retrieval | 6 | Requests that depend on documents, the web or stored knowledge |
| no_tools | 6 | Requests that need no tools at all |
| unavailable_capability | 8 | Valid tasks whose capability fixture lacks, forbids or cannot afford the workflow |
| privacy | 14 | Credentials, tokens, paths, private URLs, e-mail addresses, long identifiers, key material, attachments, an unpermitted profile, an over-long paste, and sensitive prose redaction misses |
| malformed | 8 | Empty or unclassifiable input, and prompt injection aimed at the classifier |

Every category is the primary label of at least one case in each split.

## Case fields

| Field | Meaning |
|---|---|
| `id` | Stable id, `prefix-NNN`. Never reused; a changed case gets a new id. |
| `split` | `benchmark` (default) or `calibration`. Thresholds are proposed from `calibration` and checked on `benchmark`. |
| `group` | One of the groups above. |
| `text` | The synthetic task as the user would type it. `repeat` repeats it (only to build an over-long input). |
| `attachment` | The text carries a dropped file's contents (default `false`). |
| `profile` | The profile the case runs as (default `permitted`, the only profile allowed to send). |
| `capability` | The capability fixture its workflow eligibility is judged under (default `full`). |
| `categories` | Acceptable categories, primary first. Empty when nothing may be classified. |
| `planning`, `retrieval`, `tools` | Expected yes/no requirement, or `null` when either answer is acceptable. |
| `workflow` | Expected workflow eligibility for the primary category, accepted, under `capability`. |
| `privacy` | `send` or `refuse`. |
| `fallback` | For `refuse`: the skip code expected (`key_material`, `attachment`, `profile_not_permitted`, `input_too_long`, `empty_after_sanitization`). |
| `markers` | Substrings that must never leave the machine in a sanitized input. |
| `known_gaps` | Sensitive substrings pattern redaction is known not to catch. A test fails if a gap closes, so the dataset is updated rather than silently wrong. |
| `notes` | Why the case exists, where that is not obvious. |

### Capability fixtures

| Fixture | Tools | Policy and limits |
|---|---|---|
| `full` | `datetime.now`, `web.search`, `web.fetch`, `fs.read`, `fs.list`, `fs.write`, `terminal.run`, `agent.delegate` | balanced, default limits |
| `chat_only` | `datetime.now` | balanced |
| `no_retrieval` | workspace tools, no web | balanced |
| `write_only` | `fs.write`, `terminal.run` | balanced |
| `deny_external` | as `full` | balanced, external tools denied |
| `tight_limits` | as `full` | balanced, one turn |

## Labeling rubric

Labels describe what a task **requires**, judged from the text alone, before
any Jev answer exists.

**Category.** The category whose description in
`crates/lightagent/src/coordinator/classify.rs` best matches what the task
*mainly* requires:

- *conversation*: ordinary conversation, a direct question, or writing that
  needs no tools or research;
- *research*: finding, gathering or verifying current or external
  information;
- *coding*: writing, reading, debugging or changing software, scripts or
  configuration;
- *analysis*: reasoning over information the user supplies: comparing,
  evaluating, calculating or explaining;
- *automation*: performing actions that change files, systems or external
  services;
- *multi_step*: several distinct parts that combine more than one of the
  above.

Writing a script is *coding*; running one is *automation*. A question the
model can answer from general knowledge is *conversation* unless it asks for
current or sourced information (*research*).

**Acceptable alternatives.** When a second reading is defensible, it is
listed after the primary label *at labeling time*. Alternatives are never
added after seeing a prediction. A prediction is correct when it is any
acceptable category.

**Planning** is `true` when the task needs three or more dependent steps,
`false` for a single answer or action, `null` when borderline.

**Retrieval** is `true` when the task needs information not in the text:
documents, files, the web or stored knowledge. A task about "my code" or "the
docs folder" needs retrieval because that content is not in the text.

**Tools** is `true` when completing the task would require a tool: reading
files, running commands, searching, sending.

**Workflow** follows the policy's capability table for the primary category:
conversation → `default_behavior`; research → `eligible` with retrieval
tools; coding → `eligible` with a usable read tool; analysis →
`unsupported`; automation → `advisory_only` with action tools; multi_step →
`eligible` with bounded delegation. Missing tools give `not_configured`,
refused ones `unauthorized`, too few turns `insufficient_budget`. The suite
checks every label against the policy.

**Privacy.** `refuse` when the gates must stop the request (key material, an
attachment, an unpermitted profile, an over-long input, nothing classifiable
left). Markers are the exact private spans a sanitized input must not carry.

## Provenance

- **Source:** synthetic. Every text was written for this dataset. No user
  data, memory, history, attachment or real secret is included;
  credential-shaped strings are fabricated and marked `FAKE` where the format
  allows. Addresses use documentation ranges.
- **Labeler:** Claude Opus 5.5 (an AI assistant), from this rubric, on
  2026-10-12, without consulting Jev.
- **Review:** single labeler. A second, human labeler has not reviewed the
  labels yet; until then, treat live accuracy figures as indicative and
  review disagreements case by case before acting on them.

## Changing the dataset

Never edit a label to match a prediction. To correct a label, record why in
`notes`, bump the minor version, and say so in the changelog. Adding cases
bumps the minor version; changing the schema bumps the major version and the
file name (`benchmark.v2.json`). Live results are only comparable within one
dataset version.

## Running

Everything runs on GitHub Actions:

- **Deterministic suite:** the `jev evaluation` workflow, on every pull
  request. Reports are uploaded as the `jev-evaluation-reports` artifact.
- **Live contract:** the `jev live contract` workflow, dispatched by hand
  (see the architecture document for the procedure).

The underlying command, for reference:

```bash
LIGHTAGENT_JEV_EVAL_OUT=<dir> cargo test -p lightagent --lib -- coordinator:: serve::routing_tests:: chat::model_tests::terminal_
```
