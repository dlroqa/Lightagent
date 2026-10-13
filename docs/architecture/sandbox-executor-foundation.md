# Sandbox executor foundation

Status: **interface and deny-by-default gate implemented; no isolation
backend exists.** This is item 1 of "Future sandboxed execution" in
[`profile-tool-policy.md`](profile-tool-policy.md#future-sandboxed-execution-separate-plan),
and the fourth of the four independent boundaries:

| Boundary | Decides | Where | State |
| --- | --- | --- | --- |
| Identity | Who acts | `AuthConfig` / `IdentityAuthority` | Built (#15, #16), multi-user mode not activatable |
| Profile ownership | Which profile they may operate | `identity::authorize`, `RunOwner` | Built (#16) |
| Tool policy | Which capabilities they may request | `lightagent_tools::policy` | Built (#17) |
| **Execution sandbox** | **Which OS resources those capabilities reach** | **`lightagent_tools::sandbox`** | **Interface + gate only. Every untrusted execution is denied.** |

Each is enforced on its own. The sandbox gate does not trust the tool policy
to have withheld anything, and the tool policy does not assume a sandbox.

## Interface, simulation and verified isolation

These three must not be confused, in code, tests or review:

| | What it is | Isolates? | Accepted for untrusted runs? |
| --- | --- | --- | --- |
| **Interface** (`SandboxBackend`, `ExecutionContext`, `SandboxGate`) | Types describing what a backend is asked and what it claims | No | n/a |
| **Simulation** (`Assurance::Simulated`, the test `MockBackend`) | In-process stand-in that runs nothing | **No** | **Never** by the production gate; only by a `#[cfg(test)]` gate constructor absent from every shipped build |
| **Experimental** (`Assurance::Experimental`, the Linux bubblewrap backend) | Really runs programs behind OS controls; guarantees implemented and tested on one runner, not independently verified ([`sandbox-linux-bubblewrap.md`](sandbox-linux-bubblewrap.md)) | Partially | **Never**, by any gate, including the test-only one |
| **Verified isolation** (`Assurance::Verified(VerifiedIsolation)`) | A backend whose enforcement was demonstrated on this platform | Yes, by definition | Yes, if it enforces every required guarantee |

`VerifiedIsolation` is an enum **with no variants**. No value of it can
exist, so `Assurance::Verified` cannot be constructed by this crate, a
downstream crate, a configuration file or a test. The compiler, not a
review, guarantees that this build has no verified backend. The first real
backend adds a variant, constructed only after a platform check, in the same
change as the escape tests that justify it.

## Source integration points (audit)

Exact paths as of master `943cac4`.

| Area | Source | Today | Sandbox relevance |
| --- | --- | --- | --- |
| Tool dispatch | `crates/lightagent-tools/src/executor.rs` `BoundedExecutor::invoke` (L273), `approval_for` (L237), `ctx` (L207) | Registry lookup, call guard, schema check, approval, per-call timeout, cancellation, output clamp | The future hook: a tool whose reach requires isolation calls `SandboxGate::execute` instead of acting directly. Not wired in this PR. |
| Tool selection | `crates/lightagent-tools/src/policy.rs` `resolve` (L225), `reach_of`, `ToolReach::requires_isolation` | Withholds `fs.*`, `terminal.run`, `open_terminal.run`, `agent.delegate`, stdio MCP and unknown tools from `RemoteUser` runs | Unchanged. Agreement with the gate is tested (`the_policy_and_the_gate_agree_on_untrusted_execution`). |
| Filesystem | `crates/lightagent-tools/src/workspace.rs` `Workspace::resolve_existing` (L60), `resolve_new` (L75); `builtins/fs.rs` `FsRead/FsList/FsWrite::call` | Canonicalize-and-check under one root, as the server's OS account. Symlink leaves refused on write. | Code confinement only. **Hard links are not detected** (a hard link inside the root to an outside inode is indistinguishable by path). `Guarantee::LinkEscape` covers both for a future backend. |
| Local commands | `crates/lightagent-tools/src/builtins/terminal.rs` `TerminalRun::call` (L123, spawn L165) | Direct `tokio::process::Command`, no shell, deny-list, allowlist, timeout, `kill_on_drop` | **Inherits the server's full environment** (no `env_clear`), including provider keys. `kill_on_drop` kills the direct child only, not its descendants. Both are why it is withheld from untrusted runs. |
| Open Terminal | `crates/lightagent-tools/src/builtins/open_terminal.rs` `OpenTerminalRun::call` (L298); `crates/lightagent/src/chat.rs` `open_terminal_context` (L164) | HTTP to an operator-configured service; bearer key held in memory, redacted from `Debug` | Remote from this host but **one service shared by every profile**: not per-profile isolation. |
| MCP | `crates/lightagent-mcp/src/connection.rs` `spawn_in` (L66); `chat.rs` `mcp_tools` (L382), `network_mcp_servers` (L253) | Stdio servers spawned as the server's OS account with **inherited environment plus configured `env`**; HTTP servers are remote | Stdio = local command. HTTP = remote tool (see [remote tools](#remote-tools-are-not-safe-by-default)). |
| Delegation | `crates/lightagent-tools/src/builtins/delegate.rs` `AgentDelegate::call` (L101); `context.rs` `Delegation::child_context` (L183), `reserve_child` (L166) | Worker run; registry, limits and approval narrowed when policy is enforced (#17) | `ExecutionContext::for_worker` is the sandbox-side equivalent: subset grants, capped limits, same trust, child cancellation. |
| Run trust | `crates/lightagent-core/src/identity/mod.rs` `RunTrust`, `Role::run_trust`; `crates/lightagent-api/src/manager.rs` `RunFactory::run_as` (L281), `RunManager::start_as` (L419); `crates/lightagent/src/serve.rs` `run_as` (L221) | Trust captured at run start and passed to the runtime; default `run_as` refuses `RemoteUser` | `ExecutionContext::remote_user` requires an `AuthorizedProfile` (only `identity::authorize` constructs one) with `RunsWrite`. |
| Cancellation | `manager.rs` `RunState::cancel` (L156), `cancel_token` (L162); `ToolCtx::cancel` | One `CancellationToken` per run, passed to every tool | `ExecutionContext` carries the run's token; workers get `child_token()`; the gate races every start and execute against it. |
| Revocation | `identity/credential.rs` `CredentialStore::revoke` (L108), `authenticate` (L123); `identity/ownership.rs` `OwnershipRegistry::revoke` (L175) | Revocation stops new requests; an in-flight run continues (#16 blocker 5) | `AuthorizationCheck` is consulted before every start and again after start, before execute. |
| Budgets | `crates/lightagent-core/src/limits.rs` `RunLimits`; `delegate.rs` `intersect_caps`, `within_ceiling` | Turns, tool calls, output bytes, wall clock | `ExecutionLimits` adds per-execution CPU, memory, process count, wall clock and output. |
| Secrets | `crates/lightagent-core/src/config.rs` `SecretRef::resolve` (L63); `WebPolicy` / `OpenTerminalPolicy` redacted `Debug` | Resolved from the server's environment into run-local structs | `Grants::secrets` names the only secrets a child may receive; `ExecutionContext::child_environment` builds the complete child environment from them. |
| Tool config | `crates/lightagent/src/chat.rs` `workspace_context` (L125), `configured_builtin_registry` (L200), `configured_registry` (L229) | Installation-wide `tools.*` settings | Unchanged. No sandbox configuration key exists. |

## The interface (`crates/lightagent-tools/src/sandbox.rs`)

```text
Authenticated user ─► AuthorizedProfile (identity::authorize, RunsWrite)
                          │
Tool policy (#17) ────────┤  which tools
                          ▼
                 ExecutionContext ── run, profile, principal, trust,
                          │          Grants { roots, network, secrets, limits },
                          │          depth, CancellationToken
                          ▼
                    SandboxGate::route
                          │ 1 cancelled?      → Cancelled
                          │ 2 authorized?     → Revoked
                          │ 3 LocalOperator   → Route::Host (existing executor, unchanged)
                          │ 4 RemoteUser:
                          │     assurance Refuses   → Denied(no_backend)
                          │     assurance Simulated → Denied(not_verified)
                          │     assurance Verified  → (impossible in this build)
                          │     missing guarantee   → Denied(unsupported:<g>)
                          ▼
                    SandboxGate::execute (Route::Sandbox only)
                          validate request ─► start ─► re-check authorization
                          ─► execute (raced with cancel + wall clock)
                          ─► bound output ─► release (always, once)
```

### Backend trait

```rust
#[async_trait]
pub trait SandboxBackend: Send + Sync {
    fn name(&self) -> &'static str;
    fn assurance(&self) -> Assurance;          // Refuses | Simulated | Verified(witness)
    fn capabilities(&self) -> Capabilities;    // enforced guarantees + demonstrated platforms
    async fn start(&self, ctx: &ExecutionContext) -> Result<SandboxLease, BackendFault>;
    async fn execute(&self, lease: &SandboxLease, ctx: &ExecutionContext,
                     request: &ExecRequest) -> Result<ExecOutput, BackendFault>;
    async fn release(&self, lease: SandboxLease);
}
```

Contract: `start` is cancel-safe; `execute` stops on `ctx.cancel()`;
`release` removes processes, mounts and temporary files and is called exactly
once per successful start on every outcome; the child's environment is
exactly `ctx.child_environment(..)`.

### Capability description

`Guarantee` (14, a fixed vocabulary) is what a backend can enforce and an
untrusted run requires. **An untrusted run requires all of them.** A trusted
run requires none and never reaches a backend.

| Group | Guarantees |
| --- | --- |
| Filesystem | `filesystem_roots`, `read_only_roots`, `link_escape`, `host_credential_isolation` |
| Commands | `separate_identity`, `clean_environment`, `process_tree_cleanup`, `cpu_limit`, `memory_limit`, `process_limit` |
| Network | `egress_deny_by_default`, `private_network_block`, `dns_rebinding_protection` |
| Secrets | `secret_grants_only` |

`Capabilities { enforced, demonstrated_on }`. A backend's list is relied on
only when its assurance is verified. `demonstrated_on` is empty for every
backend in this build.

Wall clock and output size are enforced by the gate itself, in process, as
well as by any backend: the gate times out `execute` and cuts stdout+stderr
to `max_output_bytes` on a character boundary. These are bounds on what
returns to the run, **not** OS resource limits.

### Execution context

| Field | Source | Notes |
| --- | --- | --- |
| `run` | caller | Per execution context; a worker gets its own |
| `profile`, `principal` | `AuthorizedProfile` for `RemoteUser`; caller + `local-operator` for `LocalOperator` | Never from the request |
| `trust` | constructor | Workers inherit it unchanged |
| `grants.roots` | label → `ReadOnly`/`ReadWrite` | Labels (`[a-z0-9_-]{1,64}`), **never host paths**; the backend maps a label to its mount, so a context holds no path to leak |
| `grants.network` | `Deny` (default) or `Allow(destinations)` | `Destination::new` refuses loopback, private, link-local, metadata, CGNAT, unique-local, embedded-v4 and `localhost` names, reusing the web tools' SSRF classifier. No private grant exists yet. |
| `grants.secrets` | secret names (`[A-Z_][A-Z0-9_]{0,63}`) | Resolved by the backend at execution; nothing else is inherited |
| `grants.limits` | `ExecutionLimits` | All non-zero |
| `depth` | 0, +1 per worker | ≤ `MAX_DEPTH` (4) |
| `cancel` | the run's token | Workers: `child_token()` |

`for_worker` refuses (`widens_parent`) any root not granted to the parent,
write access to a read-only root, a destination or secret beyond the
parent's, and network when the parent has none. An untrusted run's worker
must stay on the same profile (`cross_profile_worker`); profile-bound
delegation to other owned profiles is future work. Limits are capped, not
refused.

### Errors

`SandboxError` and everything it carries (`DenialReason`, `ContextFault`,
`RequestFault`, `Guarantee`) are fixed codes. A backend reports a
`BackendFault`, which is a code with no text, so it cannot pass a path,
program, argument, environment value or OS error message through. The test
`sandbox_errors_carry_no_paths_or_credentials` renders every variant.

## Isolation requirements for a future backend

| Area | Requirement | Guarantee(s) | Foundation in this PR |
| --- | --- | --- | --- |
| Filesystem | Per-profile roots; read/write separation | `filesystem_roots`, `read_only_roots` | Root labels with access; worker subset rule |
| | Traversal, symlink and hard-link escape | `link_escape` | `ExecRequest` refuses absolute and `..` `cwd`; the backend must mount, not path-check |
| | Host credentials and private files unreachable | `host_credential_isolation` | Required |
| Commands | Separate process identity or stronger | `separate_identity` | Required |
| | No inherited privileged environment | `clean_environment` | `child_environment` = grants only (tested) |
| | Process-tree cleanup | `process_tree_cleanup` | `release` on every outcome (tested) |
| | CPU, memory, processes | `cpu_limit`, `memory_limit`, `process_limit` | Required; missing → denied (tested) |
| | Execution time, bounded output | — | Gate-enforced (tested) |
| Network | Deny by default; approved destinations | `egress_deny_by_default` | `NetworkGrant::Deny` default |
| | No metadata, management, loopback, private | `private_network_block` | Literal addresses refused at grant time |
| | DNS and rebinding | `dns_rebinding_protection` | Required; names are resolved by the backend |
| Secrets | Explicit per-run grants only; no admin secrets; not in logs, errors or children | `secret_grants_only`, `clean_environment` | Names only in context; codes only in errors |
| Delegation | Worker ≤ originating run; same or stronger isolation; no privileged fallback | — | `for_worker`; `required()` identical for workers; the gate has no host execution path at all |

## Threat model and trust boundaries

**Assets:** the server's OS account and its files (`~/.lightagent`, provider
keys, SSH keys, other profiles' data), the server's environment, the host
network (loopback services, LAN, cloud metadata), other profiles' runs, and
the operator's external accounts reachable through tool credentials.

**Adversary:** an authenticated ordinary user (`Role::User`) and anything that
can steer their run's model: prompt injection from fetched pages, files,
MCP results or memory. Assume the model will request whatever it can.

**Boundaries:**

1. HTTP → identity (`AuthConfig`): unauthenticated callers stop here.
2. Identity → profile (`authorize`): a user reaches only owned or granted
   profiles.
3. Profile → tools (`policy::resolve`): an untrusted run is strict and never
   receives local-reach tools.
4. Tools → OS (`SandboxGate`): an untrusted execution needs a verified
   backend that enforces every guarantee. None exists, so it is denied.
5. Process → host: today there is **no** boundary for trusted runs; they act
   as the server's OS account by design (C4: local operator interfaces).

**Out of scope for this PR:** kernel exploits, side channels, a malicious
operator, and supply-chain compromise of a future backend's helper binary.

**Attacks the foundation closes (by denial):** an untrusted run executing
anything locally; a failed sandbox falling back to host execution; a mock or
self-declared backend being trusted; a worker widening its parent's grants;
continuing after revocation or cancellation; secrets or paths leaking through
sandbox errors.

## Remote tools are not safe by default

A tool that spawns no local process still acts with the installation's
credentials and network position. For untrusted runs, `ToolReach::Network`
tools (`web.fetch`, `web.search`, `rag.realtime`, `jev.classify_task`, HTTP
MCP) are offered **when the profile's strict `toolsets` lists them**. The
sandbox gate does not cover them. Their risks:

| Risk | Example | Requirement before multi-user activation |
| --- | --- | --- |
| Confused deputy | An HTTP MCP server configured with one installation credential acts for every profile | Per-profile credential binding for remote tools; an untrusted run never uses an installation-wide credential |
| Unauthorized external action | An MCP tool that sends mail or creates issues | Side-effect classification per remote tool (observe vs mutate); mutate requires approval by the run's owner, and an operator grant per profile |
| Cross-profile disclosure | A shared MCP server or search index returns another profile's data | Remote tools with server-side state must be partitioned per profile, or withheld |
| Exfiltration | Injected instructions make `web.fetch` send profile data to an attacker URL | Per-profile egress allowlist for `web.fetch` in untrusted runs; query/body size limits |
| SSRF | Operator-configured HTTP MCP base URLs on private networks | HTTP MCP endpoints treated as operator-trusted only; untrusted runs reach only endpoints marked for them |
| Shared remote execution | `open_terminal.run` uses one service for all profiles | Already withheld (`requires_isolation`); eligible only with a per-profile instance |
| External classification | `jev.classify_task` sends a task summary off-host | Existing per-profile permission and sanitization (D3); **Jev never grants tools, approvals, files, terminal, network or delegation**, and nothing in `sandbox` takes Jev input |

These requirements are recorded as activation blockers below; this PR does
not change remote-tool behavior.

## Backend comparison

| Option | Isolation strength | Privilege needed | Overhead / startup | OS | Network isolation | Filesystem guarantees | Deployment / recovery | Proxmox |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| **Linux namespaces + Landlock + seccomp + cgroups v2**, via `bubblewrap` and a delegated systemd scope | Good (shared kernel) | Unprivileged user namespaces (Ubuntu 23.10+ restricts them through AppArmor; that `bwrap` is permitted must be demonstrated on the runner), cgroup delegation | ~ms per exec; negligible memory | Linux only | Empty net namespace = deny all; allowlist via a host-side egress proxy over a bound Unix socket | Bind-mount only granted roots (read-only where granted); no host paths visible; link escape impossible across mounts | One helper binary; no daemon; failure = deny | Works in a **VM**; in unprivileged LXC needs `nesting=1`, weaker |
| Rootless containers (Podman) | Good (shared kernel) + image control | Unprivileged userns, subuid ranges | 100s of ms; image storage | Linux (macOS/Windows via a VM) | Per-container netns; allowlist via proxy | Volume mounts per root | Image lifecycle, storage cleanup | VM preferred; LXC needs nesting + fuse |
| gVisor (`runsc`) | Strong (user-space kernel) | As containers | Higher syscall cost | Linux | Own netstack | As containers | Extra runtime | VM |
| MicroVM (Firecracker / Cloud Hypervisor) | Strongest (separate kernel) | `/dev/kvm` | ~125 ms boot, ~tens of MB per VM | Linux + KVM | Tap device per VM; allowlist on host | Per-VM block device | Kernel/rootfs images, jailer | Needs nested virtualization in a Proxmox VM |
| macOS `sandbox-exec` / App Sandbox | `sandbox-exec` is deprecated and undocumented; App Sandbox needs signed entitlements | — | — | macOS | Partial | Partial | Fragile | n/a |
| Windows AppContainer / Job objects | Good for AppContainer; Job objects give limits only | Win32 APIs (unsafe Rust or a new dependency) | Low | Windows | AppContainer capabilities | ACL-based | Moderate | n/a |
| **Remote sandbox worker** (any of the Linux options, on another host, behind the same trait) | As its backend, plus host separation | On the worker only | Network round trip | Any client OS | On the worker | On the worker | Separate service to operate; failure = deny | Natural fit: a dedicated sandbox VM |

### Recommendation

1. **First backend: Linux, `bubblewrap`-based, in a separate helper
   process.** It needs no `unsafe` Rust and no new crate (the workspace
   forbids both), runs unprivileged, and maps one-to-one onto the
   guarantees: mount namespace with only granted roots, PID namespace
   (process-tree cleanup on exit, `--die-with-parent`), empty network
   namespace, `--clearenv`, a precompiled seccomp filter (`--seccomp`), and a
   systemd transient scope for cgroups v2 CPU, memory and pids limits.
   Landlock as defense in depth needs syscalls `bwrap` does not make, so it
   would need a new dependency or `unsafe` code: a separate decision. Network allowlists go through a host-side
   egress proxy that resolves names once, applies the existing SSRF
   classifier and pins the address (rebinding). Absent `bwrap`, user
   namespaces or cgroup delegation, the backend reports `Refuses`.
2. **Second: a remote sandbox worker** speaking the same trait, for
   internet-exposed or high-risk deployments, so microVMs (Firecracker) can
   be used where KVM exists without putting a hypervisor in Lightagent.
3. **macOS and Windows: deny.** No trustworthy native backend is in reach
   without new dependencies or unsafe code. Untrusted runs on these
   platforms can use a remote worker once it exists.
4. **Proxmox:** run Lightagent in a full VM (not LXC) so its kernel provides
   user namespaces, Landlock and cgroups v2 directly; or deploy a dedicated
   sandbox-worker VM.

Each backend is its own PR, with escape tests (traversal, symlinks, hard
links, `/proc`, environment probing, fork bombs, memory exhaustion, egress to
loopback/metadata, DNS rebinding, cancellation teardown) on GitHub-hosted
runners of the platform it claims.

## Platform support matrix

| Platform | This build | Planned |
| --- | --- | --- |
| Linux x64 | Gate + deny. Untrusted execution denied. An experimental bubblewrap backend exists (tests only, never eligible). | Verification of a backend (see the bubblewrap document's remaining work) |
| macOS ARM64 | Gate + deny. Untrusted execution denied. | Deny; remote worker |
| Windows x64 | Gate + deny. Untrusted execution denied. | Deny; remote worker |
| Any | Trusted runs: unchanged local execution as the server's OS account | Unchanged |

The CI tests on all three platforms exercise the gate's decisions with a
simulation. **They demonstrate no OS isolation on any platform**, and
`no_backend_in_this_build_claims_verified_isolation` asserts so.

## Security regression tests

`crates/lightagent-tools/src/sandbox/tests.rs`, run by `check` on Linux,
Windows and macOS.

| # | Requirement | Test |
| --- | --- | --- |
| 1 | Untrusted execution denied without an approved sandbox | `untrusted_execution_is_denied_without_an_approved_sandbox` |
| 2 | Trusted single-user compatibility | `a_trusted_run_routes_to_the_existing_host_executor`; every existing tool test unchanged |
| 3 | Mock never a production executor | `a_simulated_backend_is_never_a_production_executor` |
| 4 | Unsupported requirement → denial | `an_unsupported_isolation_requirement_is_denied` (each of the 14) |
| 5 | Invalid profiles and contexts fail closed | `invalid_profiles_and_contexts_fail_closed`, `private_and_malformed_destinations_are_refused`, `requests_outside_the_grants_never_reach_the_backend` |
| 6 | Revocation stops new requests | `revoked_authorization_stops_new_sandbox_requests` |
| 7 | Cancellation propagates | `cancellation_propagates_and_releases_the_sandbox`, `cancelling_a_run_cancels_its_workers` |
| 8 | No fallback to host | `a_failed_sandbox_never_falls_back_to_the_host` |
| 9 | Workers within originating limits | `a_worker_cannot_exceed_its_originating_run` |
| 10 | No server secrets inherited | `untrusted_execution_inherits_no_server_secret` |
| 11 | Errors carry no paths or credentials | `sandbox_errors_carry_no_paths_or_credentials` |
| 12 | Limits enforced or reported unsupported | `output_is_bounded_by_the_gate_whatever_the_backend_returns`, `a_wall_clock_overrun_is_stopped_by_the_gate_and_released`, `os_resource_limits_a_backend_does_not_enforce_are_reported_and_denied` |
| 13 | Mocks never mistaken for OS isolation | `no_backend_in_this_build_claims_verified_isolation` (compile-time witness + empty `demonstrated_on`) |
| — | Policy and gate agree | `the_policy_and_the_gate_agree_on_untrusted_execution` |

Data is synthetic: profiles `research`/`billing`, principal `alice`, root
label `workspace`, fake secret values. No process is spawned and no network
is used.

## Performance and resource implications

- **This PR:** none at runtime. No request path constructs a gate, so no
  run, tool call or startup does additional work. Binary size grows by one
  module.
- **Future Linux backend:** a helper process and namespace setup per
  execution (milliseconds), a systemd scope per execution, and a proxy task
  per run with network grants. Memory overhead is the child's own.
- **Future remote worker:** a network round trip per start/execute/release;
  worker capacity becomes a deployment concern.

## Failure and rollback

- **Failure behavior:** every failure denies. A backend that cannot start
  reports `unavailable`; the gate has no host execution path, so nothing
  falls back to the server's OS account. Cancellation and wall-clock overrun
  release the lease.
- **Rollback of this PR:** revert the commit. Nothing calls the module and
  no configuration or data format changed, so revert is behavior-neutral.
  `is_public_ip` in `builtins/web.rs` changed only from private to
  `pub(crate)`.
- **Operational recovery (future backends):** a backend that starts failing
  turns untrusted execution into denials, not into host execution; the
  operator fixes the host (namespaces, cgroup delegation, helper binary) and
  no state needs repair. A watchdog for leaked leases belongs with the first
  real backend.

## Remaining multi-user activation blockers

1. **A verified backend** (Linux first), with its escape tests demonstrated
   on GitHub-hosted runners, and wiring: tools whose reach requires
   isolation call `SandboxGate::execute` for untrusted runs; `policy::resolve`
   then offers them only when `route` returns `Sandbox`.
2. **Remote-tool authorization** for untrusted runs: per-profile credentials,
   side-effect classification and approvals, per-profile egress allowlists
   for `web.fetch`, partitioned remote state (see above).
3. **Revocation cancels in-flight runs**: wire `AuthorizationCheck` to the
   credential and ownership stores, and cancel the run's token on revocation.
4. **Profile-bound delegation** across owned profiles (today: same profile
   only for untrusted workers).
5. **Operator-authorized private destinations** (today: impossible).
6. Carried over from [`profile-tool-policy.md`](profile-tool-policy.md#remaining-blockers-for-multi-user-activation):
   identity store and CLI, scoped sessions/profiles/Jev/tools endpoints,
   persistent audit log, TLS and trusted ingress, provisioning API, failed
   authentication rate limiting.

Multi-user mode remains unactivatable, and no configuration option selects a
sandbox backend.
