<!--
Governing specification for Lightagent Desktop, committed verbatim as supplied
by the repository owner (prepared 2026-10-29). Later execution instructions
are in DESKTOP_CLAUDE_PROMPT.md. Status and evidence live in
DESKTOP_PROGRESS.md; nothing in this file is a claim that work is done.
-->

# Lightagent Desktop — build plan and Claude execution prompt

Prepared October 29, 2026. Repository: https://github.com/dlroqa/Lightagent

## Purpose and evidence boundary

Build an independently installed Lightagent application with a WebUI-style interface and complete coverage of existing CLI capabilities, including tools, providers, skills, and other existing functions. The application must start its bundled runtime without requiring a terminal or a separately started WebUI. Remote server connections are a secondary mode.

This is a proposed implementation plan, not a completed source audit. Repository access was unavailable when preparing it. Existing command names, APIs, features, supported OS versions, current releases, and framework compatibility must be verified from the current repository. Do not use similarly named LightAgent projects as evidence.

## Target architecture

Use one shared Lightagent application/service layer beneath CLI, WebUI, and Desktop. Retain current behavior and reuse existing backend operations. Extract missing shared operations incrementally; avoid a wholesale runtime rewrite. Prefer the existing WebUI frontend where technically appropriate.

The desktop host owns window lifecycle, packaged assets, native integration, and the bundled runtime lifecycle. A narrow typed IPC or authenticated local transport connects the frontend to the application service. Do not implement desktop features by scraping human-readable CLI output or constructing arbitrary shell command strings. A structured, versioned CLI transport is acceptable only if the audit shows it is the safest reusable boundary.

Evaluate Tauri and Electron against the real frontend, Rust integration, process model, Intel Mac support, platform dependencies, security, memory use, packaging, accessibility, and maintenance. Tauri is a candidate, not a predetermined decision. Record an architecture decision and choose one before building the shell. Any measurements must come from GitHub-hosted runners and be labeled as runner measurements.

Local mode bundles the application runtime, not model weights or every external MCP/tool dependency. Explain required external executables and provider connectivity clearly. Remote mode never silently executes remote file operations on the desktop machine; show the active endpoint, profile, workspace, and execution location.

## Mandatory execution policy

- Use GitHub Actions/Workflows on GitHub-hosted runners for all builds, formatting, linting, type checking, rendering, tests, benchmarks, dependency validation, packaging, installer checks, and release dry-runs. No self-hosted runners, local CI emulators, local app launches, or local validation commands.
- Local machines are restricted to repository copying/safekeeping, static source reading, and writing progress/plan documentation. Do not locally edit implementation source or workflows. Make implementation changes through GitHub-native branch/file APIs or an explicitly configured GitHub-hosted Actions editing workflow. Never silently relax this rule for convenience.
- If the available remote editing mechanism cannot complete a required operation, document the precise blocker and request a narrow policy clarification. Do not substitute local builds, tests, or source edits.
- This prompt authorizes development branches, draft PRs, source changes through the permitted remote mechanism, and necessary GitHub-hosted CI runs. It does not authorize merging, tagging, release publication, deployment, production activation, or automatic updates to installed applications.
- Keep existing work intact. Read applicable AGENTS.md and repository contribution/security instructions. Report any conflict with this policy before performing the conflicting action.
- Never include secrets in source, logs, screenshots, artifacts, fixtures, or progress documentation. Use deterministic fake providers and MCP servers for routine verification. Do not use paid/live services or production credentials without separate authorization.

## Structured work order and gates

All slices below are proposed PR scopes, not actual PR numbers. Maintain dependency order. A dependent slice may build on an unmerged branch, but cannot be declared integration-validated until its exact combined tree has passed the relevant CI. Do not merge any slice under this prompt.

| Order | Slice | Work and required deliverables | Exit gate |
|---|---|---|---|
| D0 | Baseline and feature audit | Record default branch/full SHA/tree; inspect CLI parser, interactive commands, configuration, runtime, WebUI, APIs, extensions, and CI. Inventory every command/subcommand/flag and user-facing operation. | Source-backed inventory; existing versus planned features distinguished; current baseline Actions evidence or clearly recorded blockers. |
| D1 | Architecture and UI specification | Decide framework and backend boundary; document process ownership, configuration compatibility, data paths, credential handling, local/remote semantics, minimum OS/architecture targets, screen map, and parity mapping. | Architecture decision, threat model, wireframes, dependency graph, and measurable acceptance criteria. |
| D2 | CI and desktop skeleton | Add remote-only build/test/render workflow; minimal packaged shell; mocked backend; version handshake; artifact metadata; frontend/backend contract generation or validation. | Shell builds and renders on declared targets; CI fails on incompatible contracts; screenshots and logs saved as Actions artifacts. |
| D3 | Shared service and runtime lifecycle | Expose/reuse typed operations; bundle matching runtime; readiness handshake, cancellation, bounded logs, shutdown, crash handling, single-instance policy, connection recovery, and version mismatch reporting. | Service contract and lifecycle tests pass; clean launch requires no developer toolchain or terminal; only owned processes are stopped. |
| D4 | Profiles, providers, credentials, workspaces | Wire real configuration operations, provider/model selection, endpoint testing, profile isolation, workspace selection, supported settings, atomic writes, and migration backup. | CLI/Desktop/WebUI share intended state; concurrent writes are safe; secret read failures fail closed; credentials never appear in renderer state or diagnostics. |
| D5 | Sessions and agent execution | Streaming chat, history, run status, cancellation, tool requests/results, approvals, errors, usage/budgets where implemented, and reconnect reconciliation. | Deterministic end-to-end run survives relevant lifecycle cases without duplicate execution or lost approvals; failures remain visible. |
| D6 | Tools, MCP, extensions, and skills | Provide source-backed management and execution controls for each existing capability. Support existing local/repository install mechanisms; clearly identify any genuinely missing backend operation as additional scoped work. | Installation/activation/removal and permission flows verified with fixtures; untrusted skill text cannot bypass application permissions; external dependencies/errors are visible. |
| D7 | Remaining CLI parity | Wire all remaining inventoried operations: memory/RAG, agents/subagents, scheduling, diagnostics, import/export, provisioning, or other areas only where present. Use dedicated controls or typed command-palette forms. | Every inventory row has a usable UI path and evidence; destructive/admin operations retain existing restrictions; no concealed unsupported items. |
| D8 | Remote mode and regression hardening | Endpoint authentication, transport restrictions, local/remote scope labels, capability negotiation, reconnect, concurrency, accessibility, keyboard use, and existing CLI/WebUI regression coverage. | Remote failures do not trigger silent local execution; version/capability mismatch is explicit; security and regression checks pass. |
| D9 | Installers and release dry-run | Build declared platform packages; verify bundled runtime and dependency requirements, clean-user launch, upgrade/uninstall behavior, checksums, licenses, SBOM, and vulnerability gates. | Exact installer artifacts tested where runner environment supports it; missing interactive/native tests explicitly block affected readiness claims. No publish/tag. |
| D10 | Review and approval package | Reconcile parity inventory, review changes, resolve defects, validate exact integration tree and proposed dependency-ordered merge sequence, provide recovery notes and complete evidence. | Ready-for-review report with unresolved risks and separate merge/release approval boundaries. Stop before merge. |

Required dependency order: D0 → D1 → D2 → D3 → D4 → D5 → D6 → D7 → D8 → D9 → D10. Split oversized slices into numbered child PRs with explicit prerequisites. Refine dependencies after the audit while preserving foundation-before-consumer order. Existing rolling-summary or other feature branches are dependencies only if actually required; do not silently absorb unrelated work.

## CLI parity contract

Create a machine-readable capability inventory and a readable matrix. Use stable IDs and include: source location, command/subcommand/option or interactive operation, behavior, backend operation, configuration/data scope, local/remote applicability, required permission, UI screen/action, implementation status, verification type, exact tested SHA, and Actions run/artifact link.

Include non-command entry points such as slash commands, interactive prompts, environment-driven behavior, config-only features, extension registrations, and relevant API operations. A command palette counts only when it presents supported typed inputs, invokes the real operation, and displays structured output. An embedded terminal or a generic command textbox does not establish parity.

For every row, distinguish implemented, unsupported with reason, intentionally deferred, and not applicable. Freeze the initial inventory at the audited SHA and check newly added commands for mapping drift in CI. Report numerator/denominator and excluded items; do not claim 100% while any applicable existing operation remains unwired. New functions such as GitHub skill installation must be labeled new scope if the current backend does not support them.

## UI organization

Reuse established WebUI language and navigation where practical. Proposed areas: Conversations; Runs/Agents; Providers/Models; Tools/MCP/Extensions; Skills; Memory/Knowledge; Profiles/Workspaces; Settings/Diagnostics. Show unavailable capabilities with a reason rather than nonfunctional controls. Preserve keyboard navigation, accessible labels, readable errors, and a clear indication of where tools will execute. Render mockups and final screens only in GitHub Actions.

## Backend and desktop requirements

- Define request/response/event schemas with correlation IDs, stable error categories, run IDs, sequence handling, cancellation semantics, and a version/capability handshake. Reconcile authoritative state after event gaps; do not assume uninterrupted streaming.
- Preserve the existing configuration and secret-store authority where secure. Do not introduce a second unsynchronized credential store. Restrict secret access to the backend; show only redacted status in the UI. Any migration requires backup, atomic update, failure recovery, and documented compatibility.
- If using an HTTP sidecar, bind locally by default, use an unpredictable per-launch credential, validate origins/hosts as appropriate, avoid credentials in URLs, and prevent unauthenticated local access. If using IPC, expose narrowly scoped commands with validated inputs.
- Disable unnecessary renderer privileges and remote content execution. Validate file paths, archive extraction, subprocess arguments, external links, and dependency installation. Keep shell/tool permissions in the shared runtime; UI text must not grant authority.
- Keep CLI, WebUI, and Desktop behavior consistent by calling shared services. Avoid multiple writers corrupting session/configuration data. Define locks/transactions and ownership of active runs.
- On app close, distinguish owned local processes from external/shared servers. Warn or follow a documented policy for active runs; never kill an unrelated Lightagent server.
- Record package/runtime/protocol versions. Use the bundled compatible runtime by default; do not automatically select an arbitrary binary on PATH. Updates remain manual unless separately designed and approved.

## Verification strategy

Use meaningful service/contract tests, CLI regression tests, frontend component tests for important interaction states, and deterministic packaged end-to-end tests. Exercise fake provider streaming, tool calls, approvals/denials, cancellation, reconnect, malformed responses, unavailable endpoints, permission failures, and process crashes. Test that equivalent CLI and desktop operations produce equivalent persistent state where applicable.

Test configuration migration and concurrent access with isolated temporary homes. Cover secret-store unavailable/locked/unreadable conditions using fixtures; use platform credential integration tests only where safely supported. Check that errors and artifacts remain redacted.

For skills/extensions, verify source identity and revision, installation boundaries, path traversal rejection, failure recovery, duplicate handling, and explicit executable/dependency setup. Do not claim arbitrary GitHub repositories are compatible without format validation.

Define the supported target matrix from evidence. Prioritize an Intel macOS package for the user's Mac, then Apple Silicon macOS, Windows x64, and Linux x64 as feasible. Verify minimum supported OS versions and availability of GitHub-hosted runner architectures before promising coverage. A cross-compiled binary is not proof it runs on that architecture or on an older macOS release. If runner coverage is unavailable, mark that gate blocked and propose an allowed hosted alternative; never substitute local validation.

Measure packaged startup, idle memory, installer size, and stream responsiveness on named runners. Agree thresholds in D1 based on evidence rather than inventing performance claims. Perform installer smoke tests under a clean user/environment without development tools on PATH. Separately report build success, packaged launch success, native UI coverage, signing/notarization status, and supported OS evidence.

Signing/notarization requiring credentials or an externally visible submission is a separate authorization boundary. Unsigned CI artifacts can support review but must not be presented as a polished publicly distributable release. Never bypass existing security gates or retry a flaky test until green without retaining and investigating failures.

## Progress and review records

Proposed documentation names (adapt to repository conventions): DESKTOP_BUILD_PLAN.md, DESKTOP_ARCHITECTURE.md, DESKTOP_CLI_PARITY.md, desktop-capabilities.json, DESKTOP_PROGRESS.md, DESKTOP_ACCEPTANCE.md, and DESKTOP_MERGE_ORDER.md.

Record each slice's status, changed behavior, branch/PR, full head and tested SHA/tree, dependencies, Actions links, checks, artifacts, failures, policy exceptions, open risks, and next step. Separate proposed, implemented, CI-validated, packaged-tested, and approved states. Preserve the existing repository copy locally for safekeeping and keep progress documentation current; do not store secrets or compiled artifacts there as a workaround.

Before requesting merge approval, list exact PR heads, proposed order, prospective combined trees, validation evidence, migration/recovery notes, and blockers. Validate proposed merge results on GitHub-hosted runners. If an approved head later changes, that approval no longer covers it. Future merges require explicit approval and validation of the resulting default-branch tree at each approved boundary. Release/tag/publish permission is separate from merge permission.

## Definition of done

The installed application launches independently, starts its own compatible runtime, and exposes every applicable existing CLI capability through working desktop actions. Shared data and behavior remain compatible with CLI/WebUI; local/remote scope is explicit; credentials and permissions remain enforced. All declared packages have the stated hosted-runner evidence, the parity inventory has no unexplained gaps, and documentation accurately states limitations. Completion under this prompt means a validated review package and installer artifacts—not a merged or published release.

---

# Copy-and-paste prompt for Claude

You are implementing Lightagent Desktop for https://github.com/dlroqa/Lightagent. Follow the complete build plan above as the governing task specification. If this prompt is pasted separately, attach this entire document and read it before starting.

Goal: create an independently installable desktop application similar to the existing WebUI, with the complete existing CLI functionality wired into working UI actions: tools, providers, models, skills, MCP/extensions, agents, sessions, memory, settings, and every other capability confirmed by the repository. Reuse the same backend logic across Desktop, CLI, and WebUI. Do not assume that proposed features already exist.

NON-NEGOTIABLE EXECUTION RULES:
1. All implementation editing must use GitHub-native branch/file APIs or an explicitly configured GitHub-hosted Actions editing workflow. Local machines are ONLY for repo copies/safekeeping, static source reading, and progress documentation. No local implementation/workflow edits, builds, launches, renders, formatting, linting, type checking, tests, packaging, dependency installation, or release dry-runs. No self-hosted runners or local CI emulation. If a required permitted editing path is unavailable, report the blocker; do not improvise a local exception.
2. Use GitHub Actions/Workflows and GitHub-hosted runners exclusively for validation and artifact production. Keep workflow permissions minimal and secrets protected. Use isolated fake services by default.
3. You may create development branches/draft PRs and run necessary hosted CI. Do NOT merge, tag, publish a release, deploy, activate production, submit externally visible signing/notarization work, or change installed user applications without separate explicit approval.
4. Preserve existing work and read applicable AGENTS.md/contribution/security instructions. Do not include unrelated refactors or silently pull in another project or feature stack.
5. Keep progress documentation with exact SHA/tree and Actions evidence. Never call predicted results verified; never claim parity from a terminal wrapper or from build success alone.

WORK ORDER:
Execute D0 through D10 in the dependency order specified above. Begin with repository inspection, not framework installation or UI scaffolding. First identify the actual default branch/full SHA/tree, existing CLI/WebUI architecture, active overlapping work, current workflow health, and supported targets. Build the complete capability inventory from source. Then choose the desktop framework and shared service boundary in a written architecture decision. Refine the proposed PR scopes and dependencies using the audit, and continue implementation in small reviewable draft PRs once the relevant prerequisite gates pass. Do not stop merely because a routine design choice needs judgment. Stop only for a genuine authorization boundary, conflicting requirements, missing access, unsafe migration, or a failed prerequisite you cannot resolve within scope.

For each slice: implement through the permitted remote mechanism; run appropriate Actions checks; inspect every result; fix failures; update progress; report its evidence and continue to the next dependent slice. Use a combined integration branch/tree as needed, without merging the PRs. Any workflow that creates commits must identify the final tested commit, avoid uncontrolled self-triggering, and preserve auditable provenance.

FEATURE COVERAGE:
Maintain a source-backed parity matrix for all CLI commands/subcommands/options, interactive and slash operations, configuration behavior, and related existing capabilities. Each row needs a real backend operation, UI path, permissions/scope, verification, and exact Actions evidence. Keep added features distinct from existing parity requirements. Implement typed controls or a typed command palette for advanced operations; arbitrary command execution is not coverage. Do not defer existing capabilities silently or call a partial app complete.

SECURITY AND LIFECYCLE:
Use a bundled compatible runtime, secure narrow IPC/local transport, existing secret authority, explicit local/remote scope, safe configuration writes, process ownership, cancellation, crash recovery, and event reconciliation. Do not expose backend credentials to the renderer. Do not silently fall back from a failed remote connection to local tool execution. Keep external dependencies visible and retain runtime permission enforcement.

VALIDATION:
Follow the plan's service, contract, frontend, parity, regression, lifecycle, security, migration, and packaged end-to-end gates. Produce declared platform installers and checksums on hosted runners. Prioritize the Intel Mac target but verify minimum OS compatibility; do not infer runtime compatibility from cross-compilation. Mark unsupported hosted-runner/native UI coverage explicitly. No live paid-provider calls or production credentials without authorization.

FINAL HANDOFF:
Provide the complete capability inventory, architecture decision, implemented/deferred/blocked counts, draft PRs in dependency order with exact heads, exact tested integration tree, Actions links and artifacts, platform/package evidence, unresolved risks, recovery instructions, and a concrete merge approval package. State clearly that nothing was merged, tagged, published, or deployed. Stop at that boundary. Do not ask for merge approval until the reviewable work and required validation are complete, or explain precisely why a blocker prevents completion.

START NOW with D0 and the source-backed audit. Treat old conversation SHAs, version numbers, API names, and this plan's proposed framework/paths as unverified until checked against the current repository. Your first progress report should identify the baseline, audit findings, permitted remote editing method, proposed framework decision process, concrete work order, and any access blockers—without claiming implementation has already passed.
