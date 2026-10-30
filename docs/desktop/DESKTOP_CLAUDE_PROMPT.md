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
