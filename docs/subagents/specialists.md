# Specialist workers

Lightagent has four specialist roles a lead agent can delegate to: researcher, coder, reviewer and tester. Each one is a typed definition that runs under the requesting profile's authority, with only the tools, contexts and budget that authority allows. This page covers how roles resolve, what bounds them, how to configure and migrate them, and what is not built yet.

## Roles

| Role | Tool ceiling | What it returns | Needs, for `ready` |
| --- | --- | --- | --- |
| researcher | `web.search`, `web.fetch`, `rag.search`, `rag.realtime`, `skill.read`, `datetime.now` (read-only) | Findings with citations, evidence vs. inference, contradictions, limitations, retrieval status | One retrieval tool. Search and page reading are degraded without them. |
| coder | `fs.read`, `fs.list`, `fs.write`, `skill.read`, `datetime.now` | Patch, rationale, checks to run, risks, base | `fs.read`. Without `fs.write` it returns a diff and is `degraded`. |
| reviewer | `fs.read`, `fs.list`, `skill.read`, `datetime.now` (read-only) | Prioritized findings with location, evidence, impact and remediation, or an honest "no findings" | `fs.read` |
| tester | `actions.*`, `fs.read`, `fs.list`, `skill.read`, `datetime.now` | Run IDs and URLs, exact SHA, job conclusions, failures, coverage gaps, blocked checks | The workflow broker (`actions.dispatch`, `actions.status`) |

A built-in role never receives `agent.delegate`, `terminal.run`, `open_terminal.run` or `jev.classify_task`. No role compiles or runs code locally: on this project, code runs only in GitHub Actions jobs reached through the broker.

Each role has a versioned skill (`worker-<role>@1`, in `crates/lightagent-core/src/workers/`). The skill is placed in full in the worker's system prompt, together with a rule that retrieved documents, files and tool output are untrusted data. The skill is also served by `skill.read` alongside the profile's own skills. A profile or extension skill with the same name replaces the body but never adds a tool: tools come only from policy.

## How a role resolves

Resolution is deterministic. A refusal is never followed by a different worker.

1. **The requesting profile's assignment of the role**, from `<profile>/workers.json`. An assignment targets one of:
   - the built-in worker;
   - a private worker profile with **the same effective owner**. Both profiles may also be unowned, which is how operator-managed profiles look.

   Ownership and the assignment are checked when the worker starts and again before every worker model turn and tool call. Withdrawing the assignment, deleting the target or changing its owner stops the worker, and its results are withheld. An assigned private profile keeps its persona and routing, but runs inside the role's ceiling and gets the role skill.
2. **A legacy profile named after the role.** Earlier versions cloned the lead into profiles named `researcher`, `coder` and so on.
   - A clone that is still exactly as generated is superseded by the built-in.
   - A clone someone customized keeps being used. Readiness reports it as `legacy_profile` until it is assigned or superseded explicitly.
3. **The built-in worker.** It runs on the lead's configured model, or on an operator route (below), with the lead's approval policy and within the lead's limits.

The legacy `profile` argument of `agent.delegate` remains for **trusted operator runs** only, subject to `subagents.legacy_profile_argument`, and goes through the same ownership check. Untrusted runs can only delegate by role.

## What bounds a worker

A worker's tools are the intersection of:
- the lead's **effective** tools, so RAG, MCP and extension tools are included, and anything withheld from the lead is withheld from the worker;
- minus lead-only tools (`agent.delegate`, `agent.research`, `jev.classify_task`, `memory.write`, `memory.reflect`);
- ∩ the role's ceiling;
- ∩ a worker profile's own `toolsets`, under the run's `tools.profile_policy`;
- ∩ the model's `tool_scope`, which can only narrow.

A worker also:
- gets **only the run contexts its tools need**: the requesting run's own web, workspace, broker and skill contexts. They are never copied wholesale and never selected from a shared tenant directory;
- re-checks the lead's **per-call policy guard** and the run's **authorization** at every call. Its cancellation token is a child of the run's;
- runs under the stricter of its own approval policy and the lead's when the profile policy is enforced. Mutating and executable calls always need an approval that a worker cannot obtain. The worker then stops with status `blocked`, names the tool in `blocked_on`, and nothing runs. No worker is left suspended;
- draws turns, tool calls and time from the **tree budget**. That budget is the lead's limits, with at most `subagents.max_workers_per_run` worker runs. Grants are taken up front and the unused part is returned;
- is **admitted** through an installation-wide pool and a per-profile pool. Waiting is FIFO, bounded by `queue_timeout_secs`, and cancelled with the run.

Depth: a worker may start one only while one more generation fits under `subagents.max_depth`. Built-in roles never nest.

## What the lead and the UI see

A role worker returns `lightagent.worker_result.v1`. Its fields:
- `status`: `completed`, `partial`, `timed_out`, `blocked`, `failed` or `cancelled`;
- `worker`, `role`, `model`, `model_source`, `skills` (with versions), `tools_offered`;
- `stop_reason` and the bounded `answer`;
- `sources`: fetched pages, and URLs a retrieval tool returned;
- `unverified_citations`: URLs the answer cites that no tool returned in the run;
- `tool_calls`, `blocked_on`, `usage` (turns, tool calls, tokens), `limitations` and `notes`.

A `blocked`, `failed` or `cancelled` result is a tool error, so the lead cannot mistake it for an answer.

While a worker runs, the delegate call streams `tool.progress` with the stages:
- `worker.queued`, `worker.started`, `worker.tool` (*n of m*), `worker.awaiting_approval`;
- a terminal stage: `worker.completed`, `worker.partial`, `worker.timed_out`, `worker.blocked`, `worker.failed` or `worker.cancelled`.

These stages travel over the existing SSE contract, with event ids and resume. The web UI words each stage and renders the typed result: a status label, sources as links, unverified citations and limitations.

## Readiness

`lightagent workers status [--profile ID] [--json]` reports each role's state:
- `ready`, `degraded`, `disabled`, `permission_denied` or `unavailable`;
- the worker (`builtin:<role>@<v>`, `assigned:<id>` or `legacy_profile:<role>`), with its model, model source, skill, offered tools and reason codes.

Readiness is a snapshot; every run rechecks.

## Configuration

```toml
[subagents]
enabled = true
resolution = "builtin"            # "legacy_profiles" restores the earlier behavior (rollback)
legacy_profile_argument = true    # operator runs only
max_active_workers = 4            # installation-wide; read when the first worker starts
max_active_workers_per_profile = 1
queue_timeout_secs = 30
max_workers_per_run = 6
remote_research = false           # offer untrusted runs agent.research (see below)
approved_models = []              # models a role route may select
[subagents.role_models]           # a route takes effect only for an approved model
# researcher = "some-model"

[platform.github_actions]         # the workflow broker; absent = tester unavailable
enabled = true
base_url = "https://api.github.com"
api_key = { env = "LIGHTAGENT_GITHUB_TOKEN" }
repository = "owner/name"
max_wait_secs = 120
[[platform.github_actions.workflows]]
file = "ci.yml"
refs = ["subagents/*"]
correlation_input = "correlation"
[platform.github_actions.workflows.inputs]
mode = ["pass", "fail"]
```

A role route whose model is not approved falls back to the lead's model, and readiness reports `model_route_not_approved`. Endpoints are never taken from a route.

Ordinary users need no GitHub credentials. The research, review and coding roles work without the broker, and only the tester reports `unavailable`.

## The workflow broker

The `actions.*` tools accept only:
- an allowlisted workflow file;
- a branch matching that workflow's ref allowlist;
- allowlisted input values;
- the SHA the branch is at **now**. A stale ref is refused.

How it behaves:
- **Finding the run.** The broker fills the operator-named correlation input, which the workflow writes into its `run-name`. Without one, a match that is not unique is reported as ambiguous.
- **No duplicates.** An identical dispatch is sent at most once per run.
- **Results.** Status reports a verdict and each job's conclusion.
- **Logs.** They are a bounded tail, fetched from GitHub's pre-signed URL without the token.
- **The token** stays in the harness and is never placed in a result.
- **Untrusted runs** never receive these tools (their tool reach is remote execution).

## Untrusted (multi-user) runs

Generic `agent.delegate` remains withheld from `RemoteUser` runs.

With `subagents.remote_research = true`, an untrusted run whose profile lists `agent.research` can ask the researcher a question:
- `agent.research` takes only a task;
- it always runs the profile's researcher, inside the read-only ceiling, never nested;
- every one of its calls is checked like any worker's.

The coder, reviewer and tester are `permission_denied` (`requires_isolation`) for untrusted runs until a verified per-tenant execution backend exists. Its absence is a reported blocker, not a security guarantee.

## Provisioning

The `assistant` template and the `LA-WEB-PROVISIONING/1` create, status and revoke contract are **unchanged**.

Operators can give an existing profile a versioned capability preset with `lightagent profile preset <id> research@1`. That preset:
- sets toolsets to read-only retrieval plus `agent.research`, and approval to balanced;
- creates no profile and does not touch the provisioning ledger;
- needs no new provisioned identities for shared built-in workers.

**Web handoff (not implemented here).** Letting the website choose a preset needs a versioned protocol revision, owned by the Web repository. It would add a `capabilityPreset` field whose value must be one of the harness's registered preset ids, validated against `provisioning::PRESETS` at admission. A preset would be applied inside the same create transaction and recorded in the ledger, and status would report the applied preset id. The website must never send tools, paths, endpoints, model ids or secrets. Until Web delivers such a revision and its fixtures, presets are operator-applied only, and there is no end-to-end web integration to claim.

## Migration and rollback

- **Upgrading**:
  - Generated role profiles are left on disk untouched and superseded.
  - Customized ones keep working and are reported as `legacy_profile`.
  - To move a customized one explicitly, run `lightagent workers assign <role> --worker <profile>` to keep it, or `--builtin` to switch to the built-in.
- **Rollback**: set `subagents.resolution = "legacy_profiles"`. Role profiles are created and used exactly as before, and assignments are ignored.
- **Behavior change**: workers in one lead run now share the lead's turns, tool calls and time, instead of each receiving its own full limits.
