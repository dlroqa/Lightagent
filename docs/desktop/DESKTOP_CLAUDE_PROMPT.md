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
