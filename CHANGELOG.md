# Changelog

All notable Lightagent changes are documented here.

## [Unreleased]

### Security

- Profile-scoped remote credentials, not activated. An untrusted run's
  outbound credentials (web search, HTTP MCP) are now grants of one profile
  for one service. Each grant names its operations and destination origins,
  and can carry an expiry and a per-run request budget; grants can be
  rotated and revoked.
  - Storage: secrets are sealed with AES-256-GCM from `ring` (already the
    TLS provider) under a per-record HKDF key bound to the credential,
    profile, service and version. The master key is kept outside the home
    (`LIGHTAGENT_CREDENTIAL_KEY_FILE`). A missing or wrong key, an
    integrity failure or a corrupt vault fails closed.
  - Enforcement: the grant is re-read before every request, and there is
    no fallback to installation credentials.
  - Management: `lightagent remote-credential` (`keygen`, `status`, `add`,
    `rotate`, `revoke`, `list`, `audit`, `rekey`) runs locally for the
    operator only, and never takes a secret on the command line.

  Operators, the terminal, ACP and single-user runs are unchanged. Details:
  `docs/architecture/profile-remote-credentials.md`.
- Profile-scoped API endpoints, not activated. In multi-user mode an
  ordinary user reaches profile-specific data only through a server-verified
  authorization of the profile, decided by current ownership.
  - Sessions: users see, search, create, change, delete and continue only
    their own, within authorized profiles. Sessions gain an additive,
    server-written `owner`; sessions without one, which is every existing
    session, stay operator-only. Another user's, an unowned and a missing
    session are answered identically.
  - `/profiles` lists only accessible profiles.
  - `/tools` and `/skills` show the profile's effective catalog for an
    untrusted run.
  - `/jev` shows only the profile's own permissions and latest outcome.
  - `/provider` withholds the endpoint and catalogs.

  Settings and Jev verification stay operator-only. Single-user mode, the
  terminal and ACP are unchanged. Details:
  `docs/architecture/profile-scoped-api.md`.
- Bounded revalidation of active runs, not activated. One shared monitor in
  `RunManager` re-checks every active revocable run on a bounded interval:
  the default is 5 s, clamped to 100 ms–300 s
  (`RunManager::with_revalidation_interval`). A run is revoked and torn down
  even while it waits on a model stream, a provider connection being set up,
  a tool, an MCP call, a delegated worker or an approval, and open event
  streams re-check their subscriber at every sweep. A sweep reads the
  identity store once, however many runs it checks, and reads it again at
  every sweep. A corrupt store fails closed. The monitor runs only while a
  revocable run is active; trusted single-user, ACP and terminal runs never
  start it. Open Terminal re-checks authorization before every poll. Measured
  on GitHub-hosted runners: detection within one interval plus about 2 ms; a
  sweep over 200 runs takes about 0.4 ms. Details:
  `docs/architecture/bounded-revocation-monitor.md`.
- Revocation-aware run lifecycle, not activated. A run started in
  multi-user mode carries a `RunAuthorization` that is re-checked at every
  enforcement point:
  - every model request (`AuthorizedProvider`);
  - every tool approval and launch (`BoundedExecutor::with_authorization`);
  - every `web.fetch` hop, `web.search`, `rag.realtime`, MCP `tools/call`
    and Open Terminal request;
  - before and inside every delegated worker;
  - before an approval decision is acted on.

  The first refusal is final and cancels the run, tearing down the model
  stream, the tool in flight and any worker. The run manager acts as the
  supervisor: `RunManager::revoke` revokes the runs a credential, principal,
  profile or grant change affects (operator only), and
  `RunManager::revalidate` re-checks every active run and ends event streams
  whose subscriber is no longer authorized. Stale approvals are refused
  (`409`). A retained `Caller` can no longer bypass a revocation, because
  `authorize_run` re-reads the credential. Single-user, ACP and terminal
  runs are unchanged. Details:
  `docs/architecture/identity-revocation-lifecycle.md`.
- `lightagent identity`: local, operator-only management of the durable
  identity store. It covers status, users (create, list, suspend), user
  credentials (issue, rotate, revoke, list), profile ownership (assign,
  reassign, remove), grants (list, revoke) and the sanitized audit trail.
  A new secret is shown once: only to an interactive terminal outside CI,
  never in `--json` output, or written to a new owner-only file
  (`--secret-file`). An undelivered credential is revoked. The command does
  not activate multi-user mode and exposes nothing remotely.
- Durable identity store (`lightagent_core::identity::store`), not activated.
  Principals, credentials, profile ownership, grants, reassignment history
  and idempotency keys are kept in one versioned document,
  `<home>/identity/state.json`:
  - every change is a cross-process-locked, `fsync`ed atomic transaction,
    with the previous generation kept;
  - corruption and newer schemas fail closed;
  - changes are operator-only (`provision` / `administer`);
  - ownership counts only while the profile exists and carries its binding
    marker;
  - audit is a bounded, rotated `identity/audit.jsonl`.

  Credentials gain expiry, rotation and a domain-separated `v1:` verifier,
  and the plaintext is never stored. `IdentityAuthority::persistent` re-reads
  the store at every check, so revocations from another process apply at
  the next request. Nothing is read or written for existing installations,
  and multi-user mode stays unavailable. Details:
  `docs/architecture/persistent-identity-store.md`.
- Remote tool egress hardening (`lightagent_tools::egress`), effective
  immediately for every run:
  - `web.fetch` vets every resolved address of every hop, **including
    allow-listed hosts**, which no longer bypass the private-address check.
    It connects only to the vetted addresses, with no environment or system
    proxy.
  - Cloud metadata and host management endpoints (including the Azure
    WireServer) are refused even with `block_private_addresses = false`.
  - Userinfo URLs, HTTPS-to-HTTP redirects and redirect loops are refused.
  - HTTP MCP no longer follows redirects, which forwarded custom headers and
    request bodies to the redirect target.
  - Remote errors name the host only.

  Internal services are reached through the new operator setting
  `web.trusted_networks` (CIDR). For untrusted (multi-user) runs, still
  unactivatable, the installation's search key and MCP credentials are
  used only by profiles granted them. Destinations are limited to the
  profile's `web.untrusted_destinations`. HTTP MCP servers are offered only
  when granted, and their tools always require approval. Stdio MCP servers
  are never started. Details, compatibility and migration:
  `docs/architecture/remote-tool-egress.md`.
- Experimental Linux sandbox backend (`lightagent_tools::sandbox::bubblewrap`,
  Linux only). It runs a fixed allowlist of programs under bubblewrap, with:
  - new user, mount, PID, network, IPC, UTS and cgroup namespaces;
  - a new session, all capabilities dropped, `no_new_privs` and no nested
    user namespaces;
  - a minimal read-only filesystem, the run's own profile roots only, and a
    per-run scratch;
  - no network and an environment of granted secrets only;
  - wall-clock and output bounds, and whole-tree cleanup.

  It refuses a setuid `bwrap`, running as root and untrusted contexts. Its
  assurance is the new `Assurance::Experimental`, which no gate accepts.
  CPU, memory and process limits, separate identity and link-escape
  protection are unsupported and not claimed. Nothing constructs it outside
  tests; a new, non-required CI job runs its live tests on `ubuntu-24.04`.
  Findings, including the remote web and MCP tool assessment and the
  Proxmox guidance: `docs/architecture/sandbox-linux-bubblewrap.md`.
- Execution-isolation foundation (`lightagent_tools::sandbox`): a typed
  `SandboxBackend` interface with a capability description (14 isolation
  guarantees), per-run and per-profile `ExecutionContext` (logical root
  grants, deny-by-default network, named secret grants, resource limits,
  cancellation), and a `SandboxGate` that:
  - routes trusted runs to the existing local executor, unchanged;
  - denies every untrusted execution unless a backend with *verified*
    isolation enforces every required guarantee. No such backend exists,
    and its witness type has no values, so a mock or self-declared backend
    can never be selected;
  - has no host execution path, so a failed sandbox never falls back;
  - re-checks authorization, honors cancellation, bounds wall clock and
    output, and always releases the sandbox;
  - reports fixed error codes with no paths or credentials.

  Nothing calls it yet and no setting selects a backend. Design, threat
  model and backend comparison:
  `docs/architecture/sandbox-executor-foundation.md`.
- Per-profile tool policy (`tools.profile_policy`: `off` (default),
  `report_only`, `enforce`, `strict`) over `AgentProfile.toolsets`. Effective
  tools are the enabled tools, narrowed by the profile's list and the run's
  trust, and applied to the model's tools, the executor, the workflow policy
  and delegated workers in the server, terminal, ACP and `tools list`:
  - A withheld tool cannot be invoked by name.
  - Enforced runs re-check each call, so a tool withdrawn mid-run is refused.
  - Approvals are unchanged.
  - Delegated workers never exceed their run's tools, limits or approval
    policy.
  - Untrusted (multi-user) runs are always strict and never receive local
    files, commands, stdio MCP or delegation, because tools run as the
    server's OS account.

  Existing installations are unchanged. Design, audit and the sandboxing
  plan: `docs/architecture/profile-tool-policy.md`.
- Run-ownership enforcement for a multi-user API mode that stays **off**
  (`lightagent serve` cannot enable it yet). In that mode:
  - Every request needs a credential, loopback included.
  - The `--key-env` key is a legacy operator credential.
  - A user may start runs only on a profile it owns, under C6's minimum
    bundle; unowned profiles are refused.
  - Each run records its initiating principal and profile immutably. Read,
    status, events, cancel and approvals are checked against them; run-id
    knowledge alone gives 404.
  - Event streams end on revocation.
  - Every other route is operator-only.

  Single-user behavior is unchanged. `AuthConfig`'s `Debug` no longer prints
  the key.
- Identity and profile-ownership foundation (`lightagent_core::identity`):
  principals and roles, capabilities, credential-limited callers, an
  ownership registry with idempotent and conflict-detecting assignment,
  hashed credentials (`la_<key id>_<secret>`, SHA-256 at rest,
  constant-time), content-free audit records, and `authorize`, which alone
  produces an `AuthorizedProfile`. `AuthConfig::caller` maps today's API
  authentication onto it. Nothing calls either on a request path yet, so
  behavior is unchanged. Architecture, audit, permission matrix, provisioning
  contract and threat model:
  `docs/architecture/profile-ownership-architecture.md`.
- Per-profile permission for sending task content to Jev for model routing
  (`platform.jev.routing_permission` and
  `platform.jev.routing_allowed_profiles`). It is checked on the server, for
  the run's resolved profile, before any routing input is prepared, under
  both `legacy_raw` and `sanitized`. A profile that is not permitted makes no
  routing request and keeps its configured model (fallback
  `profile_not_permitted`). API, ACP and terminal runs share the check.
  Classification permission and routing permission never grant each other.
  `Auto` is unchanged.
- New configurations are `strict` with no profile permitted. A configuration
  written before this release keeps routing every profile under the
  explicitly named `legacy_all_profiles` compatibility policy. Settings, the
  Jev status, `routing.selected` and the terminal report it. Nothing is
  migrated automatically: `lightagent config jev-routing-report` is a dry
  run that lists who strict enforcement would deny, and two `config set`
  commands switch. See `docs/architecture/jev-routing-permission.md`.

### Added

- Independent label-review package for the Jev benchmark: a blinded
  worksheet, author labels and key built in CI
  (`scripts/jev-review-package.py`, artifact `jev-label-review-package`),
  an annotation schema and a disagreement procedure
  (`evaluation/jev/review/`).
- The first live Jev baseline (run 38311407479: contract passed,
  `jev-1.13.0`, exploratory 6/6) and a prioritized security implementation
  plan (`docs/architecture/jev-security-implementation-plan.md`, plan only).
- Jev live-validation readiness: the live contract records the commit and
  every resolved model version, classes each failure as transport,
  credential or API contract, stops after a failed authentication, and runs
  only from `master`. Live classification reports are marked exploratory
  and score the benchmark and calibration splits separately.
- A routing-permission and profile-authorization proposal
  (`docs/architecture/jev-routing-permission-and-profile-authorization-proposal.md`)
  documenting five open privacy and authorization issues. Proposal only.
- Jev evaluation: a versioned, synthetic, independently labeled benchmark
  (`evaluation/jev/benchmark.v1.json`, 101 cases with a calibration split)
  and a test-only runner over the production classifier, sanitizer, workflow
  policy and router. It reports category accuracy, per-category precision,
  recall and F1, a confusion matrix, abstention, fallback and coverage,
  requirement error rates, workflow agreement, reliability curves, ECE and
  accuracy-coverage curves, with sample sizes and intervals. Mock-contract
  and live results are labeled separately; thresholds are only ever
  proposed, never applied.
- `legacy_raw` versus `sanitized` routing benchmark, an adversarial privacy,
  prompt-injection, authorization and reliability suite, and a preflight
  performance simulation.
- Terminal workflow parity: terminal turns now evaluate the workflow policy.
  In `active` mode with `workflow_guidance` on, eligible fixed guidance is
  added to that turn only, beside the memory catalog; the session prompt and
  stored history are unchanged.
- CI: `jev-evaluation.yml` (deterministic, every pull request, uploads
  reports) and `jev-live.yml` (manual dispatch, `jev-live` environment,
  `TYPESAFE_API_KEY`, bounded requests; reports pending without the secret).


- Jev workflow policy: a typed, read-only eligibility check that maps an
  accepted classification to an existing capability (retrieval, workspace,
  delegation) only when the run already has it, its approval policy permits
  it and its limits allow it. Analysis is unsupported and automation advisory
  only. Guidance is applied only with `active` mode and the new
  `classification.workflow_guidance` setting (off by default).
- `platform.jev.routing_input` (`legacy_raw` | `sanitized`): sanitized model
  routing sends the minimized task and never an attachment or key material.
  Existing Jev configurations keep `legacy_raw`; new configurations use
  `sanitized`.
- `classification.completed` schema 2: workflow eligibility, applied versus
  advisory, usable tools, approval, selection overhead and routing input.

- Jev in the Tools catalog and Settings: a Decision services card (configured,
  enabled and verified shown separately), classification controls, an
  administrator-only connection check, and the profile's latest sanitized
  classification. New endpoints: `GET /api/lightagent/v1/jev` and
  `POST /api/lightagent/v1/jev/verify`.
- Optional explicit `jev.classify_task` tool for permitted profiles
  (`platform.jev.classification.explicit_tool`, off by default), sharing the
  sanitizer and gates of automatic classification and bounded by a run-level
  Jev request budget (`max_calls_per_run`).

- Optional Jev task classification (`platform.jev.classification`, off by
  default): one sanitized, batched request per run or terminal turn, permitted
  per profile, reported through a new `classification.completed` event. In
  `active` mode an accepted category may add one fixed advisory instruction
  about tools the run already has; it never grants tools, approvals or models.

### Changed

- Linux CI jobs are pinned to `ubuntu-24.04` ahead of the `ubuntu-latest`
  move to 26.04. Required check names are unchanged.
- Lightagent's Jev model routing no longer replaces Lightweight's `Auto`
  model; `Auto` reaches the provider unchanged so Lightweight decides the route.
- `active` classification alone no longer adds advisory guidance; it also
  requires `classification.workflow_guidance` (unreleased behavior narrowed).

- Moved TypeSafe Jev transport, response validation and error handling into a
  new shared `lightagent-jev` crate used by the advisory model router. Routing
  decisions, allowlists, thresholds and settings are unchanged.
- A cancelled API or ACP run now stops waiting for Jev's routing answer at once
  instead of waiting up to the routing timeout; it keeps its configured model.

## [0.4.0] - 2026-09-18

### Changed

- Extracted Lightagent into its own repository and Cargo workspace.
- Removed every inference-engine, gateway, model-management, benchmark, and
  desktop-shell crate from the harness repository.
- Made the browser UI a standalone Lightagent surface served directly by
  `lightagent serve`; it no longer depends on a Lightweight reverse proxy.
- Split update, CI, packaging, and release workflows from Lightweight.
- Added the compiled Web UI to Lightagent release archives.

## [0.3.25] - 2026-09-17

- Added the responsive Agent Web UI with persisted sessions, live SSE runs,
  tool timelines, approvals, queued steering, runtime tool discovery, and
  harness settings.
- Kept dropdown menus within the viewport and made the render harness
  deterministic.

## [0.3.24] - 2026-09-15

- Added terminal reasoning visibility, session-scoped approvals, durable
  sessions, memory reflection, realtime retrieval, and guided setup refinements.

## [0.3.8] - 2026-09-12

- Added bounded run-time decisions, extensions, skills, MCP, ACP, RAG, durable
  memory, provider model discovery, and end-to-end tool execution coverage.

## [0.3.0] - 2026-09-09

- Introduced Lightagent as an agent harness with a provider-neutral loop, tools,
  approvals, profiles, persisted sessions, HTTP/SSE API, terminal CLI, and a
  protocol-only adapter for the Lightweight OpenAI-compatible endpoint.

Versions through 0.3.25 were originally tagged in the combined Lightweight
repository. Subsequent Lightagent releases are produced from this standalone
repository.

[Unreleased]: https://github.com/dlroqa/Lightagent/compare/v0.4.0...HEAD
[0.4.0]: https://github.com/dlroqa/Lightagent/releases/tag/v0.4.0
[0.3.25]: https://github.com/dlroqa/Lightweight/releases/tag/v0.3.25
[0.3.24]: https://github.com/dlroqa/Lightweight/releases/tag/v0.3.24
[0.3.8]: https://github.com/dlroqa/Lightweight/releases/tag/v0.3.8
[0.3.0]: https://github.com/dlroqa/Lightweight/releases/tag/v0.3.0
