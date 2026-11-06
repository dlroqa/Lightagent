# Lightagent Desktop — execution instructions

The repository owner's execution instructions for Lightagent Desktop, committed
verbatim in the order they were given. The original copy-and-paste prompt is
the last section of [DESKTOP_BUILD_PLAN.md](DESKTOP_BUILD_PLAN.md). The
instructions below refine it and take precedence where they are more specific.
Nothing here is a claim that work is done; see
[DESKTOP_PROGRESS.md](DESKTOP_PROGRESS.md).

## 1. Continuation after the first report (2026-10-29)

Continue with the existing Lightagent Desktop build plan. This first-report checkpoint does not require another approval. You are authorized to create development branches, draft PRs, and run necessary GitHub-hosted Actions within the plan.

1. Finish D0 before declaring its gate passed:
   - Inventory individual flags, chat key bindings, ACP methods, MCP/extension registration, and setup options.
   - Give each capability a stable ID, source reference, desktop applicability, and intended UI/backend mapping.
   - Reconcile overlapping CLI/API/config entries so the coverage denominator is clear.
   - Keep the six unresolved applicability decisions explicit until D1 resolves them.
   - Record exact baseline Actions links. Explain the deliberate fixture failures with workflow/test evidence; do not simply exclude unexplained failures.

2. Use the existing remote source-edit workflow to create the audit branch and draft PR. Verify its fixed base, resulting commit, branch update, and provenance. The drift check must detect additions to the inventoried command structure and document any coverage it cannot automatically detect.

3. Complete D1 before committing to a desktop framework:
   - Compare Tauri and Electron against the repository dependency policy, existing React frontend, packaging, process lifecycle, and supported operating systems.
   - Treat macOS 10.14 compatibility as an unverified target. Distinguish Intel architecture support from minimum macOS support, and distinguish build success from runtime evidence.
   - Decide profile switching explicitly, including active runs, locks, credentials, event streams, and failure recovery.
   - Define how the desktop launch credential reaches the UI without placing it in URLs, persistent browser storage, logs, or artifacts.
   - Address authentication, Host validation, Origin validation, and event-stream authentication together. A random API key alone does not complete the local-server security design.

4. Continue D2 through D10 in dependency order once prerequisite gates pass. Keep PRs small and maintain the exact proposed merge order. Do not stop after routine progress reports.

Preserve the execution policy:
- GitHub-native APIs or the GitHub-hosted source-edit workflow for implementation changes.
- GitHub Actions on GitHub-hosted runners only for builds, formatting, rendering, tests, benchmarks, packaging, and release dry-runs.
- Local machines only for repository safekeeping, static reading, and progress documentation.
- No merge, tag, release publication, deployment, production activation, or installed-app updates without separate explicit approval.

Stop only for a genuine blocker or authorization boundary. Otherwise continue through implementation and validation, ending with the complete review package and exact tested heads/trees.

## 2. GitHub repository and execution requirements (2026-10-29)

The Lightagent Desktop project must be stored in the existing GitHub repository:
https://github.com/dlroqa/Lightagent

Do not create a separate repository unless I explicitly approve it.

1. Put all desktop source code, shared backend changes, build configuration, GitHub Actions workflows, capability inventories, architecture decisions, and progress documentation into this repository through development branches and draft PRs.

2. Commit the complete governing build plan and this execution prompt to the repository as documentation, using repository conventions. Include:
   - DESKTOP_BUILD_PLAN.md
   - DESKTOP_CLAUDE_PROMPT.md
   - DESKTOP_PROGRESS.md
   - DESKTOP_CLI_PARITY.md
   - desktop-capabilities.json
   - DESKTOP_ARCHITECTURE.md
   - DESKTOP_ACCEPTANCE.md
   - DESKTOP_MERGE_ORDER.md

   Create each document when its stage begins and keep it current. Do not create empty documents merely to claim completion.

3. Perform implementation edits through GitHub-native APIs or the existing GitHub-hosted source-edit workflow. All building, formatting, linting, rendering, testing, benchmarking, packaging, installer verification, and release dry-runs must run exclusively in GitHub Actions on GitHub-hosted runners.

4. Use local machines ONLY for repository copies/safekeeping, static source reading, and progress documentation. Do not locally edit implementation source or workflows, install project dependencies, build, run, render, or test the application.

5. Confirm that remotely generated commits are attached to the correct development branches and included in draft PRs. An unattached commit or local-only file does not satisfy delivery.

6. Save generated installers, checksums, screenshots, test reports, and other validation outputs as GitHub Actions artifacts with documented retention. Keep source and durable progress records in Git; do not commit generated binaries or secrets.

7. Every progress report must identify:
   - Repository and development branch.
   - Draft PR URL.
   - Exact source and tested commit SHA/tree.
   - GitHub Actions run URLs and results.
   - Artifact links and outstanding blockers.
   - Next dependency-ordered work stage.

8. Continue D0 through D10 under the existing build plan. Development branches, draft PRs, and necessary hosted Actions runs are authorized. No additional approval is needed for routine progress checkpoints.

9. Do NOT merge, tag, publish releases, deploy, activate production, or update installed applications without my separate explicit approval.

Begin by completing D0, putting its audit documents and the governing plan/prompt on the GitHub development branch, opening the draft PR, and validating the inventory checks through GitHub Actions. Then continue in the established dependency order.

## 3. Local repository backup and build progress (2026-10-29)

Maintain a complete local copy of:
https://github.com/dlroqa/Lightagent

GitHub remains the authoritative source for implementation and validation. The local copy is required for safekeeping, static source reading, and project progress documentation.

1. Verify the existing local repository path and remote URL before creating another copy. Preserve all existing files and uncommitted work.

2. After each completed work stage, safely fetch the relevant GitHub branches and synchronize the local backup with the remotely committed source and documentation. Never overwrite local changes, force-reset, clean, or delete files to synchronize. If necessary, use a separate clean backup directory.

3. Keep DESKTOP_PROGRESS.md current on GitHub and maintain its latest copy locally. Record:
   - Completed stage and remaining work.
   - Implementation changes and decisions.
   - Branch and draft PR URL.
   - Exact source SHA/tree and tested SHA/tree.
   - GitHub Actions run URLs and actual results.
   - Installer/report artifact links and retention.
   - Failures, blockers, limitations, and next steps.
   - Last local synchronization time and backed-up commit.

4. Distinguish the latest implementation commit from the latest tested commit. Never describe untested changes as validated.

5. Confirm the local backup path, backed-up commit, and progress-document path in each stage-completion report. A local backup does not replace the GitHub branch, draft PR, or Actions evidence.

EXECUTION BOUNDARIES

- Local machine: repository copying/synchronization, safekeeping, static source reading, and progress documentation ONLY.
- Implementation source and workflow edits: GitHub-native APIs or the existing GitHub-hosted source-edit workflow.
- Builds, formatting, linting, rendering, testing, benchmarking, packaging, and installer verification: GitHub Actions on GitHub-hosted runners ONLY.
- Do not run the application, install project dependencies, or perform validation locally.
- Do not merge, tag, publish, deploy, activate production, or update installed applications without separate explicit approval.

Continue the established D0–D10 dependency order. Local backup and progress documentation are ongoing requirements throughout the project, not tasks deferred until completion.

## 4. Continuation: session writes, D4 acceptance, D4d onward (2026-11-02)

You are continuing the existing Lightagent Desktop project in:
https://github.com/dlroqa/Lightagent

This is a continuation, not a fresh implementation. Read the committed project documents and verify GitHub’s current state before changing anything.

GOAL

Build an installable Lightagent Desktop application with full coverage of existing CLI capabilities, reusing the shared backend and React WebUI.

Keep desktop-specific implementation in desktop/. Include the response-action feature: copy, rating, read aloud, share, regenerate, and the More menu, with honest availability states for unfinished functions.

AUTHORITATIVE DOCUMENTS

Read the current versions on the relevant desktop branches:
- DESKTOP_BUILD_PLAN.md
- DESKTOP_CLAUDE_PROMPT.md
- docs/desktop/DESKTOP_PROGRESS.md
- DESKTOP_CLI_PARITY.md
- desktop-capabilities.json
- DESKTOP_ARCHITECTURE.md
- DESKTOP_ACCEPTANCE.md
- DESKTOP_MERGE_ORDER.md
- docs/response-action-bar-progress.md

Locate files using repository conventions rather than assuming every file is at the root. Report missing documents without inventing their contents.

SESSION OWNERSHIP

Before editing, check whether another session owns this work.
- Do not modify another session’s staging folders.
- Preserve all existing files and uncommitted work.
- Do not overwrite, reset, clean, or delete mixed or older drafts.
- Use one implementation owner for the branch stack.
- If another session is actively writing to this stack, resolve ownership before any mutation; read-only investigation can continue.

LAST VERIFIED BASELINE — RECHECK

The previous review verified:
- #97 D4b head: 7c3e988; desktop and check passed.
- #101 D4c configuration:
  branch desktop/d4c-config
  head 551e73a809c0ee6044a77be5b6b9f0e9d80e60ff
  check 41360645573 and desktop 41360645549 passed.
- #102 response-action adoption:
  branch desktop/d4c1-response-actions
  head fbd008c01ee5c8933f6ef9e852f0056084f5527a
  check 41364196274 and desktop 41364196416 passed.
- Jev evaluation and subagents also passed on both latest heads.
- #101 and #102 were draft, open, and unmerged.
- Session-write race is tracked in issue #100; its fix had not been implemented.
- Desktop capability coverage was 22/226 on #101 and 22/227 after response-action adoption.
- These are historical checkpoints, not permission to assume GitHub is unchanged.

Verify current heads, bases, source trees, CI results, and any newer PRs before proceeding. Avoid duplicating work completed since this checkpoint.

NEXT WORK — IN ORDER

1. Fix issue #100: concurrent session writers

Prepare a focused draft PR stacked on #102.

Audit every session writer, including:
- rename, pin, and archive;
- normal-run completion;
- regenerate;
- CLI chat;
- ACP;
- any other writer discovered during the audit.

Use one shared cross-process read-modify-write mechanism:
- Re-read authoritative state under the lock.
- Apply the intended change to current state.
- Preserve concurrent metadata and transcript updates.
- Do not simply lock and save a stale whole-session snapshot.
- Use atomic persistence and preserve valid data on failure.
- Keep short persistence locking separate from long-running inference.
- Preserve run-admission, cancellation, permission, ownership, and profile-isolation behavior.

Add deterministic separate-process tests covering metadata changes racing normal completion and regeneration, concurrent run admission, failed writes, provider failure, and cancellation.

2. Complete remaining D4 acceptance requirements

D4c’s implementation exists, but its acceptance gate was not complete.

Finish:
- CLI/desktop configuration equivalence using isolated homes.
- Deterministic stale-revision rejection.
- Concurrent CLI and Settings saves without lost updates.
- Unreadable configured secrets failing closed with redacted errors.
- Secrets absent from renderer state dumps and diagnostics bundles.
- Jev routing-report shared operation, API route, and UI.
- Settings conflict handling that preserves unsaved input and offers refresh/review without silently discarding edits.

Concurrency expectations:
- Load an older Settings revision, complete a CLI write, then submit Settings: require 409 and no overwrite.
- If Settings commits first, a later CLI update must preserve that change.
- Do not require every unordered race to produce exactly one 409.

For byte-for-byte configuration comparisons, control environment-dependent values or document narrowly justified normalization. Do not hide meaningful differences.

3. Continue D4d and later stages

Once prerequisites pass, continue providers, models, protected credentials, endpoint checks, workspaces, and D5–D10 in the committed plan’s dependency order.

Keep PRs small and retain exact acceptance evidence. Routine progress reports do not require another approval.

RESPONSE-ACTION DESKTOP REQUIREMENTS

#102 incorporates #98’s response-action work and desktop transport adoption.

Maintain these requirements:
- Regenerate uses the shared backend and desktop host transport.
- Failed, cancelled, empty, or refused regeneration preserves the previous saved answer.
- Session-write permission and ownership are enforced.
- New run creation and regeneration are blocked appropriately during profile switching.
- Clipboard, speech, and sharing are verified in the actual desktop WebView on supported platforms.
- Unsupported capabilities have clear availability states.
- Keyboard navigation, tooltips, accessibility, streaming visibility, and narrow-window layout are checked.
- Ratings remain temporary until persistence exists.
- Branching remains disabled until its backend exists.
- Previous-answer navigation remains unavailable until implemented.

Contract tests and browser tests are not substitutes for native desktop verification. Keep native gaps explicit and do not count unfinished features as completed parity.

PROPOSED MERGE ORDER — NO MERGES AUTHORIZED

Previously established sequence:
#90 → #92 → #93 → #94 → #95 → #96 → #97 → #101 → #102
→ session-write fix → remaining D4 work → subsequent desktop stages.

- #91: framework-spike evidence only.
- #99: transport-integration evidence only.
- #98: incorporated through #102; do not merge it separately under this plan.
- Verify this sequence against current GitHub state and update documentation if new work changes dependencies.
- Before eventual merge approval, simulate the exact candidate sequence and compare candidate trees with tested trees.
- Stacked PR bases alone do not prove that every resulting master tree is validated.
- Do not rewrite or force-update existing branches merely to simplify the sequence.

MANDATORY EXECUTION POLICY

GitHub is authoritative for source, workflows, plans, and durable progress records.

Implementation source and workflow edits:
- GitHub-native APIs or the existing GitHub-hosted source-edit workflow only.
- Attach generated commits to the correct development branches and draft PRs.
- Verify the fixed base, resulting commit/tree, and provenance.
- No local implementation or workflow editing, including temporary staging folders.

All formatting, dependency installation for builds or validation, linting, building, rendering, testing, benchmarking, packaging, installer verification, and release dry-runs:
- GitHub Actions only.
- GitHub-hosted runners only.
- No self-hosted runners.
- No local application execution or validation.

Respect the repository dependency policy. Never put credentials in source-edit requests, logs, screenshots, artifacts, or commits.

The earlier local-edit departure for #98 is already recorded. Preserve that record; do not rewrite history or claim it complied with this policy.

LOCAL BACKUP AND PROGRESS — REQUIRED

Verify the existing backup before creating another clone:
~/Repositories/LAcode/Lightagent

Previous state:
- Clean checked-out master at 12582a0.
- Desktop branches preserved as fetched remote refs.
- Separate progress copies in:
  ~/Repositories/LAcode/desktop-progress/

Local use is limited to:
- repository copying and safe synchronization;
- safekeeping;
- static source reading;
- progress documentation.

After each completed stage:
- Safely fetch relevant branches without force, reset, clean, or overwriting work.
- State which commits are preserved as remote refs and which are checked out.
- Refresh local progress-document copies from their authoritative GitHub versions.
- Preserve older drafts and progress snapshots.
- Record the actual synchronization time, backed-up SHA, and file paths.
- Distinguish the documentation commit from the implementation commit it describes.

REPORTING AND ACCEPTANCE

For each stage, report:
- Repository, branch, and draft PR URL.
- Exact source head SHA/tree.
- Exact tested SHA/tree and whether CI tested a head or merge candidate.
- Actions run URLs and actual results.
- Failures, reruns, fixes, and outstanding gaps.
- Capability coverage with a reconciled denominator.
- Artifact links and retention.
- Local backup state and progress-document paths.
- Next dependency-ordered stage.

Keep installers, screenshots, reports, and checksums as Actions artifacts. Keep durable conclusions and evidence references in Git. Do not commit generated binaries.

AUTHORIZATION

Development branches, draft PRs, GitHub-hosted source editing, and necessary hosted Actions runs are authorized.

Do NOT merge, tag, publish releases, deploy, activate production, or update installed applications without my separate explicit approval.

Begin with a concise verified status report, then continue the authorized work. Do not stop after that report unless you encounter a genuine blocker or authorization boundary.

## 5. Continuation: D4d-2b, fail-closed credentials, D4e, D5b (2026-11-02, third continuation)

Continue the existing Lightagent Desktop project:
https://github.com/dlroqa/Lightagent

This is a handoff to a new session. Verify the current GitHub state and read the committed instructions before making changes. Do not restart or duplicate completed work.

PROJECT GOAL

Build an installable Lightagent Desktop application with full CLI capability coverage, using the shared Lightagent backend and React WebUI.

Keep desktop-specific code in desktop/. Preserve integration with providers, models, credentials, tools, skills, MCP, profiles, workspaces, sessions and response actions.

READ FIRST

The previous session reported that all nine governing documents are under docs/desktop/:
- DESKTOP_BUILD_PLAN.md
- DESKTOP_CLAUDE_PROMPT.md
- DESKTOP_PROGRESS.md
- DESKTOP_CLI_PARITY.md
- desktop-capabilities.json
- DESKTOP_ARCHITECTURE.md
- DESKTOP_ACCEPTANCE.md
- DESKTOP_MERGE_ORDER.md

Locate and read the full governing document set, including any additional document there, plus:
docs/response-action-bar-progress.md

Read the versions on the latest relevant development branch, not just master. Follow the documented D0–D10 dependency order.

OWNERSHIP AND PRESERVATION

Check whether another session is actively writing to this branch stack before editing.
- Use one implementation owner.
- Do not touch another session’s staging directories.
- Preserve existing files, uncommitted work and older drafts.
- No reset, clean, deletion, force push or overwriting work to synchronize.
- If active ownership conflicts, continue read-only investigation while resolving ownership.

LAST VERIFIED CHECKPOINT — RECHECK

All implementation remains unmerged.

Latest verified PRs:
- #106, provider credentials:
  branch desktop/d4d1-credentials
  head 8f0717d2f05ab1422bca4cf479afee65a244bcbb
  desktop 41379449836: passed
  check 41379449916: passed
- #107, endpoint check:
  branch desktop/d4d2-endpoint-check
  head 2bed55cd5329d80b9f438cc685ac829c85dc8804
  desktop 41382470709: passed
  check 41382470844: passed

Jev evaluation and subagents also passed on both heads. Both PRs were draft, open and unmerged.

Earlier completed work:
- D4b runtime/profile lifecycle corrections.
- #101 shared locked, revision-checked configuration updates.
- #102 response-action integration and regenerate safety.
- #103 shared cross-process session writes.
- #104 remaining D4 configuration acceptance work.

Reported capability coverage: 25 of 227.
This is capability coverage, not overall project completion.

Do not assume this checkpoint is still current. Inspect newer PRs, branches, issue updates and Actions results before choosing the next change.

WHAT IS ALREADY IMPLEMENTED

Provider credentials:
- Shared save/replace/remove operations.
- Write-only Settings entry and status.
- Stale-save protection.
- Configured unusable credentials fail closed in CLI chat, routed requests, models, setup model listing and Desktop runtime.
- Intentionally absent credentials remain supported.

Endpoint checks:
- Shared CLI/API operation, triggered explicitly.
- Bounded timeout: 1–30 seconds, default 10.
- Redirects reported rather than followed.
- Hosted tests for authentication, models, 401, redirects and timeout.

NEXT WORK — IN ORDER

1. D4d-2b: provider/model selection and saved providers

Create small draft PRs stacked on the verified latest prerequisite.

Use shared operations for CLI, WebUI and Desktop:
- Provider creation, editing, selection and removal.
- Saved-provider listing and details.
- Model listing and selection.
- Protected credential references and status.
- Consistent validation and error handling.

Credential isolation:
- Bind credentials to their intended provider/endpoint.
- Never implicitly send the previous provider’s key to a newly selected endpoint.
- Define behavior for endpoint changes, provider rename/deletion, replacement and profile switching.
- Never expose stored keys to renderer responses, logs, errors, screenshots or artifacts.

Settings behavior:
- Retain revision checks.
- Preserve unsaved input after conflicts.
- Make refresh/review and retry explicit.
- Explain whether a change affects the next run or requires a restart.
- Do not silently alter an active run’s provider/model.

Validate shared behavior, credential isolation, stale saves, provider removal, endpoint errors, model selection and profile switching through hosted tests.

2. Finish runtime credential handling

The previous report identified lightagent runtime control-plane commands as still using the old credential-resolution behavior.

Audit every remaining credential-resolution path and complete the fail-closed conversion before declaring D4d complete:
- Configured but unreadable, missing-from-environment, empty or invalid credentials must stop the operation.
- Errors must be redacted.
- Intentionally absent credentials must remain supported.
- Tests must prove that the configured endpoint receives no request when credential resolution fails.

3. D4e: workspaces

Continue according to the committed plan after D4d’s prerequisites pass.

4. D5, including issue #105

Issue #105 tracks cross-process run admission.

Session-write locking is already implemented, but admission remains per process: separate processes can start turns against the same session concurrently.

Implement its planned D5b fix:
- Exclusive per-session run admission across Desktop runtime, other serve processes, CLI chat and ACP.
- Crash recovery through OS-managed lease/lock release.
- Cancellation and normal completion release.
- Different sessions remain able to run concurrently.
- Keep the run lease separate from short session-file write locks.
- Do not hold the session write lock during inference.
- Use deterministic separate-process tests on supported platforms.

Then continue D5–D10 in dependency order.

RESPONSE-ACTION REQUIREMENTS

Keep the shared response-action feature in Desktop scope:
- Copy, read aloud, share, regenerate and More.
- Regenerate must preserve the previous saved answer after refusal, failure, cancellation or empty output.
- Enforce session ownership and required permissions.
- Preserve profile-switch run gates.
- Verify actual Desktop clipboard, speech and sharing behavior per platform.
- Show honest unavailable states where unsupported.
- Verify keyboard navigation, tooltips, accessibility, streaming visibility and narrow windows.

Ratings remain temporary until persistence exists.
Branching remains disabled until its backend exists.
Previous-answer navigation remains unavailable until implemented.

Do not count transport integration or browser tests as native Desktop verification.

CARRIED ACCEPTANCE REQUIREMENTS

- D7: diagnostics-bundle secret-redaction tests.
- D8: native Desktop WebView checks, including Settings, profile switching, close-with-active-runs and response actions.
- Retain all other requirements in the committed acceptance document.
- Update parity only when implementation and evidence support it.

PROPOSED MERGE ORDER — VERIFY, DO NOT EXECUTE

#90 → #92 → #93 → #94 → #95 → #96 → #97
→ #101 → #102 → #103 → #104 → #106 → #107
→ subsequent dependency-ordered slices.

- #91: framework-spike evidence only.
- #99: transport evidence only.
- #98: incorporated through #102; do not merge separately under this plan.

Current CI evidence validates PR heads, not the final merge sequence.
Before requesting eventual merge approval:
- Simulate the exact proposed merges.
- Record resulting commits and trees.
- Validate candidate trees through hosted Actions.
- Verify each intermediate master state remains valid.
- Do not infer equivalence from stacked bases alone.

MANDATORY EXECUTION POLICY

Implementation source and workflow edits:
- GitHub-native APIs or the existing GitHub-hosted source-edit workflow only.
- Verify the fixed base, generated commit/tree and provenance.
- Attach commits to the correct development branches and draft PRs.
- No local implementation or workflow editing, including temporary staging folders.

All formatting, dependency installation for builds/validation, linting, building, rendering, testing, benchmarking, packaging, installer verification and release dry-runs:
- GitHub Actions only.
- GitHub-hosted runners only.
- No self-hosted runners.
- No local application execution or validation.

Respect the repository dependency policy.
Never put credentials in edit requests, logs, screenshots, artifacts or commits.

Preserve the recorded earlier local-edit departure for #98. Do not rewrite history or describe it as compliant.

LOCAL BACKUP AND PROGRESS — REQUIRED

Verify the existing backup:
~/Repositories/LAcode/Lightagent

Previously:
- master 12582a0 checked out, working tree clean.
- Desktop branches retained as fetched remote refs.
- Progress copies stored in:
  ~/Repositories/LAcode/desktop-progress/

Local machines may be used only for:
- Repository copying and safe synchronization.
- Safekeeping.
- Static source reading.
- Progress documentation.

After each completed stage:
- Safely fetch relevant branches.
- Preserve existing local work and older snapshots.
- State which commits are fetched refs versus checked-out files.
- Refresh local progress copies from authoritative GitHub documents.
- Record actual synchronization time and backed-up SHA.

REPORTING

For each completed stage, record:
- Repository, branch and draft PR URL.
- Exact implementation head SHA/tree.
- Exact tested SHA/tree and head-versus-merge-candidate distinction.
- Actions URLs and actual outcomes.
- Failures, reruns and fixes.
- Capability coverage and reconciled denominator.
- Remaining gaps and next stage.
- Artifact links and retention.
- Local backup and progress-document paths.

Keep generated installers and validation outputs as Actions artifacts, not committed binaries. Preserve durable conclusions and evidence references in Git.

AUTHORIZATION

Development branches, draft PRs, remote source editing and necessary GitHub-hosted Actions runs are authorized.

Do NOT merge, tag, publish releases, deploy, activate production or update installed applications without my separate explicit approval.

Begin with a concise verified status report, then continue the authorized work. Do not stop at routine progress checkpoints unless a genuine blocker or authorization boundary requires my input.

## 6. Continuation: D5a and the D5b review follow-through (2026-11-02, fourth continuation)

Continue the existing Lightagent Desktop project:
https://github.com/dlroqa/Lightagent

This is a new-session handoff. Verify current GitHub state, read the committed instructions, and continue the next unfinished work. Do not restart or duplicate completed slices.

GOAL

Build an installable Lightagent Desktop application with full CLI capability coverage, reusing the shared backend and React WebUI.

Keep desktop-specific implementation in desktop/. Preserve providers, models, credentials, profiles, workspaces, sessions, tools, skills, MCP and response-action integration.

READ FIRST

Read the governing documents on the latest relevant development branch, not only master:
- docs/desktop/DESKTOP_BUILD_PLAN.md
- docs/desktop/DESKTOP_CLAUDE_PROMPT.md
- docs/desktop/DESKTOP_PROGRESS.md
- docs/desktop/DESKTOP_CLI_PARITY.md
- docs/desktop/desktop-capabilities.json
- docs/desktop/DESKTOP_ARCHITECTURE.md
- docs/desktop/DESKTOP_ACCEPTANCE.md
- docs/desktop/DESKTOP_MERGE_ORDER.md
- docs/response-action-bar-progress.md

Discover any additional governing files and repository rules. Report missing files accurately.

SESSION OWNERSHIP

Verify whether another session is actively writing to this stack before editing.
- Maintain one implementation owner.
- Preserve all existing work, local drafts and staging folders.
- Do not reset, clean, delete, overwrite or force-update branches.
- If ownership conflicts, continue read-only investigation while resolving ownership.

LATEST VERIFIED CHECKPOINT — RECHECK

PR #113:
- Branch: desktop/d5b-run-admission
- Head: db44a465eeb6f0c592091c615c62dbb7da2308e2
- Draft, open and unmerged.
- desktop 41408566607: passed.
- check 41408566354: passed.
- Jev evaluation 41408566389: passed.
- subagents 41408566450: passed.

Reported coverage: 29/227 applicable capabilities.
This measures feature parity, not overall project completion.

Completed draft slices reported before this checkpoint:
- #108: endpoint-bound provider credentials, validated code head 8e3919b.
- #109: shared saved-provider/model operations, 108dac6.
- #110: Providers and models Settings card, bb76e74.
- #111: remaining live remote credential paths fail closed, c15e249.
- #112: shared absolute workspace setting and Settings card, ec1fc4e.
- #113: cross-process per-session run admission, 1e657e5, followed by the validated documentation head db44a46.

Earlier work includes configuration locking/revisions, session-write locking, profile lifecycle, response-action transport adoption, credential management and endpoint checks.

These are historical checkpoints. Verify current heads, trees, bases, Actions results, newer PRs and progress records before choosing the next change.

NEXT STAGE: D5a

Unless newer verified work has completed it, implement D5a on the latest validated prerequisite.

Use the real desktop host and real Lightagent runtime with a deterministic fake provider. Extend the existing harness:
- desktop/e2e/run.py
- desktop/e2e/switch.py

Cover:
1. Streaming and tool execution.
2. Approval granted and denied.
3. Cancellation.
4. Continue after a time-budget pause.
5. Steering.
6. Relay reconnect without missing or duplicate delivered events.
7. Pending approval during profile switching:
   - switching remains blocked unless the user explicitly cancels affected runs;
   - approvals cannot cross into another profile.
8. Sidecar crash mid-run:
   - report the interrupted run accurately;
   - automatic runtime recovery must never replay the task or tool side effects.
9. Regenerate through the desktop host:
   - successful replacement;
   - preserve the previous saved answer after refusal, provider failure, cancellation or empty output;
   - enforce permissions, ownership and profile-switch gates.

Use explicit synchronization rather than timing sleeps wherever practical. Test product behavior, not just the harness’s internal assumptions.

Clearly distinguish:
- backend/API evidence;
- host-level integration evidence;
- browser UI evidence;
- native Desktop WebView interaction.

Keep D8 native verification gaps explicit.

D5b REVIEW FOLLOW-THROUGH

#113 introduced a bounded 500 ms grace when acquiring the per-session run lease.

Review and verify:
- An active holder prevents a second provider request.
- A competing turn is refused within the documented bound.
- Normal completion, cancellation and crashes release the lease.
- Different sessions can run concurrently.
- All supported entry points use the same lease: Desktop runtime, additional serve processes, CLI chat, ACP and regeneration.
- Child processes do not unintentionally inherit lease handles and prolong ownership.
- The long-lived run lease remains separate from short session-file write locks.

If handle inheritance causes delayed release, fix it where feasible rather than relying solely on the grace period. Record platform-specific evidence and remaining limitations.

RECORDED FINDINGS — PRESERVE

- Provider credentials must stay bound to the endpoint they were saved for.
- Endpoint changes must not implicitly carry the old endpoint’s key.
- Provider names that collide in credential-file naming must be rejected safely.
- Workspaces are absolute to avoid process-dependent interpretation.
- The earlier runtime.rs control-plane concern was reported as dead, uncompiled code; verify before proposing work on it.
- Browser secret checks do not establish native Desktop verification.
- Diagnostics-bundle secret checks remain assigned to D7.

CI FAILURES AND RELIABILITY

Earlier reports include:
- GitHub broker dispatched_unlocated failures.
- macOS hdiutil disk-image eject failures.
- Real D5b lease-release and timing defects that were fixed.

Preserve failing run references and logs.
Do not label broker failures definitively GitHub-side without evidence.
Investigate unresolved failures that affect acceptance.

Any packaging retry change must:
- match a specific established transient failure;
- remain bounded;
- retain every attempt’s logs;
- fail immediately on unrelated errors.

Do not retry failures merely to obtain a green result.

RESPONSE-ACTION REQUIREMENTS

Keep the feature in Desktop scope:
- Copy, read aloud, share, regenerate and More.
- Native clipboard, speech and sharing capability checks per platform.
- Clear unavailable states where unsupported.
- Keyboard navigation, tooltips, accessibility, streaming visibility and narrow-window layout.
- Ratings remain temporary until persistence exists.
- Branching remains disabled until its backend exists.
- Previous-answer navigation remains unavailable until implemented.

Do not count unfinished features as completed parity.

DEPENDENCY AND MERGE PLAN

Previously established sequence:
#90 → #92 → #93 → #94 → #95 → #96 → #97
→ #101 → #102 → #103 → #104 → #106 → #107
→ #108 → #109 → #110 → #111 → #112 → #113
→ D5a and subsequent dependency-ordered slices.

Verify and update this against current GitHub state.
Document why D5b was implemented before D5a and reflect actual dependencies.

- #91: framework-spike evidence only.
- #99: transport evidence only.
- #98: incorporated through #102; do not merge separately under this plan.

Existing evidence validates PR heads, not the complete merge sequence.
Before requesting eventual merge approval:
- simulate the exact proposed sequence;
- record resulting commits and trees;
- validate candidate trees through hosted Actions;
- establish that intermediate master states remain valid.

Do not infer merge-tree equivalence from stacked bases alone.

EXECUTION POLICY — MANDATORY

Implementation source and workflow edits:
- GitHub-native APIs or the existing GitHub-hosted source-edit workflow only.
- Verify the fixed base, generated commit/tree and provenance.
- Attach commits to the correct branches and draft PRs.
- No local source/workflow editing, including temporary staging directories.

All dependency installation for builds/validation, formatting, linting, building, rendering, testing, benchmarking, packaging, installer verification and release dry-runs:
- GitHub Actions only.
- GitHub-hosted runners only.
- No self-hosted runners.
- No local application execution or validation.

Respect repository dependency rules.
Never expose credentials in edit requests, logs, screenshots, artifacts or commits.
Preserve the recorded earlier #98 local-edit policy departure without rewriting history.

LOCAL BACKUP AND PROGRESS — REQUIRED

Verify the existing backup:
~/Repositories/LAcode/Lightagent

Previous state:
- master 12582a0 checked out, working tree clean.
- Desktop branches retained as fetched remote refs.
- Progress copies in:
  ~/Repositories/LAcode/desktop-progress/

Local use is limited to:
- repository copying and safe synchronization;
- safekeeping;
- static source reading;
- progress documentation.

After each completed stage:
- safely fetch relevant branches;
- preserve local work and older snapshots;
- distinguish fetched refs from checked-out files;
- refresh progress copies from authoritative GitHub documents;
- record actual synchronization time and backed-up SHA.

REPORTING

For each completed stage, provide:
- Repository, branch and draft PR URL.
- Exact source head SHA/tree.
- Exact tested SHA/tree and head-versus-merge-candidate distinction.
- Actions URLs and actual results.
- Failures, reruns, fixes and remaining gaps.
- Capability coverage and reconciled denominator.
- Artifact links and retention.
- Local backup state and progress-document paths.
- Next dependency-ordered stage.

Keep generated installers, screenshots and reports as Actions artifacts. Preserve durable conclusions and evidence references in Git. Do not commit generated binaries.

AUTHORIZATION

Development branches, draft PRs, remote source editing and necessary GitHub-hosted Actions runs are authorized.

Do NOT merge, tag, publish releases, deploy, activate production or update installed applications without separate explicit approval.

Start with a concise verified status report, then execute the next unfinished stage. Continue through the established D5–D10 plan after prerequisite gates pass. Do not stop at routine reports or stage boundaries unless genuinely blocked or at an authorization boundary.
