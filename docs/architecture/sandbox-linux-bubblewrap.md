# Experimental Linux sandbox backend (bubblewrap)

Status: **experimental. Implemented and tested on one GitHub-hosted Linux
runner. Not independently verified. Never eligible for untrusted runs.**
Multi-user mode stays unactivatable. No setting selects this backend, and
nothing in Lightagent constructs it outside its tests.

> A working subprocess is not proof of a security sandbox. This document
> keeps three things apart throughout:
>
> | Level | Meaning | This backend |
> | --- | --- | --- |
> | **Implemented** | Code requests the control | Yes, for the controls listed below |
> | **Tested** | A live test on a real kernel observed the control holding | Yes, on `ubuntu-24.04` (kernel 6.17 Azure, bwrap 0.9.0), for the controls marked tested |
> | **Independently verified** | Reviewed security evidence, every guarantee the gate requires demonstrated, separate authorization | **No.** `VerifiedIsolation` still has no values. |

## 1. What exists today (audit, master `cab4219`)

| Component | Source | Behavior relevant here |
| --- | --- | --- |
| `SandboxBackend` trait | `crates/lightagent-tools/src/sandbox.rs` L637 | `start` / `execute` / `release`, `assurance`, `capabilities` |
| `ExecutionContext` | `sandbox.rs` L381 | Run, profile, principal, trust, `Grants` (root labels with access, network, secret names, limits), depth, cancel token. Untrusted only from `AuthorizedProfile` + `RunsWrite`. |
| `SandboxGate` | `sandbox.rs` L842; `eligible` L884, `route` L903, `execute` L922 | Trusted → `Route::Host`. Untrusted → only a verified backend enforcing all 14 guarantees. No host execution path. |
| `Guarantee`, `Capabilities` | `sandbox.rs` L51, L180 | 14 guarantees; `enforced` + `demonstrated_on` |
| `VerifiedIsolation` | `sandbox.rs` L157 | Uninhabited enum: no verified backend can exist |
| Terminal executor | `builtins/terminal.rs` `TerminalRun::call` | `tokio::process::Command`, no shell, deny-list/allowlist, `kill_on_drop` on the direct child, **inherits the server environment** |
| Process spawning and cancellation | `executor.rs` `BoundedExecutor::invoke` (per-call timeout L340), `ToolCtx::cancel` | Timeout/cancel drop the tool future; `kill_on_drop` kills the direct child only |
| Environment | `terminal.rs`, `lightagent-mcp/src/connection.rs` `spawn_in` | Children inherit the full server environment (stdio MCP adds configured `env`) |
| Workspace | `workspace.rs` `resolve_existing` / `resolve_new` | Canonicalize-and-check by path, as the server's OS account |
| Tool policy and run trust | `policy.rs` `resolve`, `ToolReach::requires_isolation`; `RunTrust` | Untrusted runs never receive local execution tools |
| Delegated workers | `builtins/delegate.rs`, `ExecutionContext::for_worker` | Workers only narrow; same trust; child cancel token |

**Integration in this PR:** none. The backend is a new module
(`sandbox/bubblewrap.rs`, Linux only). The trusted terminal and file tools
are unchanged.

## 2. Architecture (as built)

```text
Tool call (trusted run)                       Tool call (untrusted run)
   │                                             │
   ▼                                             ▼
BoundedExecutor → terminal.run / fs.*         policy::resolve withholds local-reach tools
(unchanged, host OS account)                     │  (and if anything asked the gate:)
                                                 ▼
                                       SandboxGate::route
                                         assurance Experimental → Denied(not_verified)
                                         assurance Refuses      → Denied(no_backend)

Tests only (crates/lightagent-tools/tests/sandbox_bubblewrap.rs):
  ExecutionContext::local_operator ──► BubblewrapBackend::start / execute / release
                                         │ refuses RemoteUser contexts itself
                                         ▼
  tokio::process::Command(/usr/bin/bwrap, argv)   env_clear + explicit env
                                         │
                       ┌─────────────────┴────────────────────┐
                       │ new user/mount/pid/net/ipc/uts/cgroup │
                       │ namespaces; new session; caps dropped; │
                       │ no_new_privs; nested userns disabled   │
                       │  /usr ro, /bin→usr/bin…, ld.so.cache   │
                       │  /proc /dev, /tmp tmpfs (16 MiB)       │
                       │  /scratch  (per-run host dir, rw)      │
                       │  /work/<label> (own profile's roots)   │
                       │  program from a fixed allowlist        │
                       └────────────────────────────────────────┘
```

## 3. Interface change (minimal, compatible)

| Change | Why |
| --- | --- |
| `Assurance::Experimental` | A backend that really runs programs but is not verified needed a status that is neither `Simulated` (runs nothing) nor `Verified`. The gate returns `Denied(not_verified)` for it **in every build, including the test-only gate that accepts simulations**. |
| `BackendFault::TimedOut`, `BackendFault::Cancelled` | A real backend enforces the wall clock and cancellation itself; these map to the existing `SandboxError::TimedOut` / `Cancelled`. |

`VerifiedIsolation` is untouched and still uninhabited. Tests:
`an_experimental_backend_is_never_eligible_for_untrusted_runs` (all
platforms), and the live `unsafe_roots_other_profiles_and_untrusted_runs_fail_closed`
(production gate over the real backend → `not_verified`).

## 4. Command construction and security policy

Implementation: `crates/lightagent-tools/src/sandbox/bubblewrap.rs`
(`base_args`, `exec_args`, `BubblewrapBackend`).

**Binary.** `/usr/bin/bwrap`, accepted only if it is at a canonical absolute
path, is a regular file, is root-owned, is not group- or world-writable,
**is not setuid or setgid**, and is in a root-owned, non-writable
directory. The same checks apply to `/usr/bin/kill`, which is used for tree
cleanup. A setuid `bwrap` is refused, never used as a fallback. The
backend refuses to run when the server is root.

**Spawn.** `tokio::process::Command` with an argument vector. No shell
builds or runs anything. `env_clear()`, then exactly `PATH`, `HOME=/scratch`
and the granted secrets. Secrets travel in the environment, never in argv,
which other host processes can read. stdin is `/dev/null`. `kill_on_drop`.

**Arguments**, in order:

```text
--unshare-user --unshare-ipc --unshare-pid --unshare-net --unshare-uts --unshare-cgroup
--die-with-parent --new-session --cap-drop ALL --hostname sandbox
--ro-bind /usr /usr
[--disable-userns]                          when bwrap supports it (0.9.0: yes)
--symlink usr/bin /bin  (…per merged-/usr link; --ro-bind for real dirs)
--ro-bind /etc/ld.so.cache /etc/ld.so.cache
--proc /proc --dev /dev
[--size 16777216] --tmpfs /tmp              when bwrap supports --size (0.9.0: yes)
--dir /work
--bind <per-run scratch> /scratch
--ro-bind|--bind <host root> /work/<label>  (only the run's own profile; ro unless granted rw)
--chdir /work/<root>[/<relative cwd>]
-- <program> <args…>
```

Request values (program, arguments, `cwd`) only appear as single argv
elements: `cwd` after `--chdir`, program and arguments after `--`. A request
argument such as `--bind / /` is an argument to the program, never an option
to `bwrap` (unit test `request_values_are_single_arguments_after_the_options`).

**Refused before spawning** (`ExecutionFailed` / `StartFailed`):
- A program outside the operator's fixed allowlist. There is no production
  list: tests use `/usr/bin/python3`, `env`, `true` and `mount`.
- A `cwd` with `..`, a root component or a NUL byte.
- A root label not granted to the run.
- A granted label with no entry for the run's own profile.
- A host root that is not a canonical directory (for example, a symlink).
- Any `RemoteUser` context.

**Errors.** Codes only. bwrap's own `bwrap: …` stderr lines can name host
paths and are replaced by `[sandbox setup message withheld]`. The program
cannot name host paths, because it sees none.

**Cleanup.** Normal exit: bwrap's PID-1 exits and the kernel kills
everything left in the PID namespace. Timeout or cancellation: `SIGSTOP` to
the outer bwrap, `SIGKILL` to every descendant found through `/proc` (the
namespace init among them), then `SIGKILL` to bwrap. The per-run scratch is
removed on `release` (`remove_dir_all`, which does not follow links).

> **Defect found and fixed by the live tests.** Killing only the outer
> bwrap leaked the namespace's init when a run was cancelled within about
> 1 ms of start. `--die-with-parent` arms the parent-death signal only
> after `clone`. The stop-then-kill-descendants sequence closes it: 30
> cancellations at 0–500 ms delays leave no process, across repeated runs.

## 5. Filesystem layout

| Path in sandbox | Source | Access |
| --- | --- | --- |
| `/usr`, `/bin`→`usr/bin`, `/sbin`, `/lib`, `/lib32`, `/lib64` | host runtime | read-only |
| `/etc/ld.so.cache` | host | read-only (no other `/etc` file) |
| `/proc` | new, for the sandbox PID namespace | — |
| `/dev` | minimal new devtmpfs set | — |
| `/tmp` | new tmpfs, 16 MiB | read-write, per execution |
| `/scratch` | per-run host directory (mode 0700), removed at release | read-write, per run |
| `/work/<label>` | the operator's host directory for *(run profile, label)* | as granted |

Nothing else: no `/home`, `/root`, `/run`, `/var`, `/etc/passwd`, sockets,
or other profiles' roots.

## 6. Test results

Live suite: `crates/lightagent-tools/tests/sandbox_bubblewrap.rs`, job
**Linux sandbox (bubblewrap, experimental)**. Values below are copied from
the job output.

### Filesystem (tested)

| Attempt | Result |
| --- | --- |
| Read / write granted rw root | ok; host sees the write |
| Read ro root | ok |
| Write / create in ro root | `EROFS` |
| Read the synthetic host secret by absolute host path | `ENOENT` |
| `..` traversal to the secret | `ENOENT` |
| Host `$HOME`, `/root`, `/etc/shadow`, `/etc/passwd` | `ENOENT` |
| Other profile's workspace, by host path and by label | `ENOENT` |
| Symlink planted by the sandbox → host secret | `ENOENT` inside; the host's `Workspace::resolve_existing` also refuses it |
| Symlink placed **by the host** → host secret | `ENOENT` |
| Hard link from ro root or `/usr` into rw root | `EXDEV` |
| Hard link placed **by the host** in a granted root → host secret | **readable (known risk; `link_escape` not claimed)** |
| Rename a mount point | `EBUSY` |
| Write `/usr` | `EROFS` |
| `unshare(CLONE_NEWNS)` | `EPERM` |
| `unshare(CLONE_NEWUSER)` (nested user namespace) | `ENOSPC` (`--disable-userns`) |
| setuid-root `/usr/bin/mount` tmpfs / remount rw | non-zero exit (no privilege gained) |
| Granted root behind a symlink | `StartFailed` |
| `cwd` `../../billing` | refused |
| Program not in the allowlist (`/usr/bin/bash`) | refused |

Filesystem race conditions: a host root is checked (canonical, directory)
at `start` and again at each `execute`, then bound by path. A process
**inside** the sandbox cannot affect host paths outside its mounts, and
renaming its mount points fails (`EBUSY`). A **host-side** writer of a
root's parent directories could swap it between the check and the bind.
That is a known risk; see §10.

### Process (tested)

| Check | Result |
| --- | --- |
| `CapEff`, `CapPrm`, `CapBnd` | all `0000000000000000` |
| `NoNewPrivs` | `1` |
| Processes visible | 2 (bwrap init + program) |
| Session | new; leader inside the sandbox; no controlling tty |
| Hostname | `sandbox` |
| Top-level entries | `bin,dev,etc,lib,lib32,lib64,proc,sbin,scratch,tmp,usr,work` |
| Orphaned, re-sessioned grandchild | gone when the program exits |
| Timeout (2 s) | `TimedOut`; tree gone; returns well under 10 s |
| Cancellation during execution (500 ms) and startup (0–50 ms), 30 runs | `Cancelled`; tree gone every time |
| Already cancelled | nothing starts |
| 8 concurrent sandboxes | each sees only its own `/scratch`; all scratch removed |
| Output 1 MiB + 1 MiB, cap 1000 | truncated; stdout exactly 1000 bytes |
| Write 64 MiB to `/tmp` | `ENOSPC` (16 MiB tmpfs) |

### Network (tested)

| Destination | Result |
| --- | --- |
| Public HTTPS / DNS server (numeric) | `ENETUNREACH` |
| Host loopback listener, v4 and v6 | `ECONNREFUSED` (the sandbox's own empty loopback); listener saw no connection |
| Private range (`10.0.0.1`), the link-local cloud metadata endpoint, the Azure WireServer host-management endpoint | `ENETUNREACH` |
| DNS lookup | fails (`gaierror`); no `/etc/resolv.conf` |
| Host abstract Unix socket | `ECONNREFUSED` (per network namespace) |
| Interfaces | `lo` only |

With no route at all, there is nothing for DNS rebinding to redirect. An
allowlisted egress (a `NetworkGrant::Allow`) is **not implemented**: this
backend denies all network.

### Secrets (tested)

| Check | Result |
| --- | --- |
| Child environment keys (synthetic `OPENAI_API_KEY`, `TYPESAFE_API_KEY` (Jev), `LIGHTAGENT_API_KEY`, `SSH_AUTH_SOCK` set in the server) | exactly `HOME, PATH, PWD, REPORT_TOKEN`; no server value appears |
| Granted `REPORT_TOKEN` | present with the granted value |
| Resolver knows `PROVIDER_API_KEY` but it is not granted | absent |
| No grants | only `HOME, PATH, PWD` |
| `SSH_AUTH_SOCK` path, `/var/run/docker.sock`, `/run/docker.sock`, `/run/dbus/system_bus_socket`, `/run/user` | `ENOENT` |

`PWD` is set by bwrap to the sandbox working directory (`/work/workspace`),
not a host path.

### Resource limits

| Limit | Status |
| --- | --- |
| Wall clock | **Enforced and tested** (backend and gate) |
| Output bytes | **Enforced and tested** (backend and gate) |
| `/tmp` size | **Enforced and tested** (tmpfs `--size`) |
| Per-run `/scratch` size | **Unsupported**: a host directory, no quota |
| CPU, memory, process count | **Unsupported, not claimed.** No kernel accounting is used. The runner's cgroup v2 is not delegated to the runner user (`cgroup_delegated: false`). |

## 7. Supported and unsupported guarantees

| Guarantee | Implemented | Tested here | Verified |
| --- | --- | --- | --- |
| `filesystem_roots` | yes | yes | no |
| `read_only_roots` | yes | yes | no |
| `host_credential_isolation` | yes | yes (secret file, home, sockets) | no |
| `clean_environment` | yes | yes | no |
| `process_tree_cleanup` | yes | yes | no |
| `egress_deny_by_default` | yes (deny all) | yes | no |
| `private_network_block` | yes (no route) | yes | no |
| `dns_rebinding_protection` | yes (no network) | yes | no |
| `secret_grants_only` | yes | yes | no |
| `link_escape` | **no**: host-placed hard links reach their target | shown | — |
| `separate_identity` | **no**: the user namespace maps to the server's host uid | — | — |
| `cpu_limit`, `memory_limit`, `process_limit` | **no** | reported | — |

Because five required guarantees are unsupported and the assurance is
`Experimental`, the gate denies twice over.

## 8. Prerequisites

| Requirement | Runner observation |
| --- | --- |
| Linux, not root | uid 1001 |
| `bubblewrap` package, root-owned, **not setuid** | 0.9.0, mode 755, root |
| `--size` and `--disable-userns` (bwrap ≥ 0.9 per probe) | both supported |
| User namespaces (`user.max_user_namespaces` > 0) | 63838 |
| Ubuntu ≥ 23.10: AppArmor must allow bwrap to create user namespaces | Stock runner: `apparmor_restrict_unprivileged_userns=1`, no bwrap profile → **backend refuses** (`launch_failed`, `namespace_denied`, tested) |
| `/usr/bin/kill` trusted | yes |
| cgroup v2 delegation (for future limits) | not delegated |

**The AppArmor step.** For the live suite, the CI job loads a profile that
grants `userns` to `/usr/bin/bwrap` only, and leaves the host-wide
restriction at `1`. This is Ubuntu's per-application mechanism. **It is a
host security trade-off:** any local user can then use bwrap to create user
namespaces, which exposes more kernel code to them. A deployment must make
that decision explicitly; Lightagent does not install the profile.

## 9. macOS and Windows

The `bubblewrap` module is compiled only on Linux. On macOS and Windows the
only backend is `DenyBackend`. `untrusted_execution_is_denied_on_macos_and_windows`
runs on both CI platforms and asserts a `no_backend` denial.

## 10. Threat model and known escape risks

The adversary is a future untrusted run, or a prompt-injected model, that
controls the program's arguments and behavior. Even if it were ever
eligible, which it is not, the backend has these risks:

1. **Same host uid.** Unprivileged user namespaces map to the server's
   account. Isolation comes from namespaces and mounts, not identity. Any
   kernel bug reachable from a user namespace operates as that account.
2. **Kernel attack surface.** There is no seccomp filter yet. User
   namespaces expose code paths that Ubuntu's AppArmor restriction exists
   to limit, and the AppArmor grant reopens them to everyone using bwrap.
3. **No CPU, memory or process limits.** A fork bomb or memory exhaustion
   affects the host. The wall clock bounds the duration only.
4. **Unbounded `/scratch`.** Disk can be filled.
5. **Host-placed hard links** in a granted root reach their target.
   Mitigation: operator-owned roots on a dedicated filesystem, and
   `fs.protected_hardlinks=1`.
6. **Root path TOCTOU** against a host-side writer between the check and
   bwrap's bind. Mitigation: fd-based binds (`--bind-fd`), or roots under a
   directory only the server can write.
7. **Server crash.** If Lightagent dies mid-run, `kill_on_drop` never runs,
   and the sandbox runs until its program exits; the wall clock is not
   enforced. Mitigation: a cgroup per sandbox and `cgroup.kill`, or a
   supervisor.
8. **argv visibility.** Program arguments are visible in host
   `/proc/<pid>/cmdline`. Secrets are not, because they travel in the
   environment.
9. **All of `/usr` is readable**, including `/usr/local`. Operators must
   not keep secrets there.
10. **Runner-specific evidence.** All results are from one kernel and image.
    Other kernels, distributions and AppArmor or SELinux policies are
    untested.

## 11. Performance

`/usr/bin/true`, 20 runs: sandboxed median ≈ 4.8 ms (p90 ≈ 5.1 ms) against
≈ 0.65 ms for a direct spawn, about 4 ms overhead per execution. `detect`
runs three short bwrap probes once. Memory overhead is two small bwrap
processes per running execution.

## 12. Remote web and MCP tools (assessment only; behavior unchanged)

Sandboxing a local process does nothing for these: they run in the server
process with the installation's credentials and network position.

| Surface | Source | Finding |
| --- | --- | --- |
| `web.fetch` | `builtins/web.rs` `fetch_text`, `guard_url`, `guard_host`, `pinned_client` | Good: http(s) only; private addresses refused; DNS answers pinned (no second resolution); redirects followed manually and re-guarded. **But:** `web.block_private_addresses` can be turned off; hosts in `web.allow_domains` **skip the private-address check**, so an allowlisted name that resolves privately reaches internal services; any public URL may be fetched, so injected instructions can exfiltrate data in a query string. |
| `web.search`, `rag.realtime` | `WebPolicy.search_api_key` | One installation-wide search key used for every profile's queries. The provider sees every profile's queries under one account. |
| HTTP MCP | `McpServerEntry::Http { url, headers, auth }`; client `chat.rs` `mcp_tools` (L402) | One `auth` secret and fixed `headers` per server, sent for **every profile's** calls (confused deputy). No SSRF guard on the URL (operator-configured). The client uses reqwest's **default redirect policy**: up to 10 redirects. reqwest drops `Authorization` and cookies on a cross-host redirect, but **custom `headers` are forwarded** to wherever the server redirects. |
| MCP tool side effects | MCP `tools/call` | Risk class is not derived from the server. A mutating MCP tool runs under the profile's approval policy only, with no per-profile side-effect grant. |
| Cross-profile state | Remote MCP servers and search indexes | Any server-side state (memory, documents, sessions) keyed on the shared credential is shared across profiles. |
| `jev.classify_task` | coordinator | Per-profile permission and sanitization already exist (D3). Jev never grants tools or execution, and nothing in `sandbox` reads Jev output. |
| Rate limits, audit | — | No per-profile rate limits on remote tools. No tenant-safe audit events for remote calls. |

**How one profile's data reaches another:** via shared MCP or search
credentials, and shared server-side state, which make the remote service
treat all profiles as one account. Via exfiltration, because `web.fetch`
can be steered to any public destination. Via internal services, when a
name is allowlisted or private blocking is off.

**Recommended separate PR (not implemented):**
1. Profile-scoped credentials: per-profile `SecretRef` for MCP `auth` and
   headers and for search. An untrusted run never uses an
   installation-wide credential.
2. Destination allowlists per profile for `web.fetch` and HTTP MCP in
   untrusted runs, with the private-address check applied even to
   allowlisted names unless the operator explicitly grants a private
   destination.
3. Side-effect approvals: an operator-declared risk class per MCP tool;
   mutating calls need the run owner's approval.
4. Rate limits per profile and per principal on remote calls.
5. Egress validation for MCP: the SSRF guard, a pinned resolution and
   redirects off (or re-guarded with all custom headers stripped).
6. Tenant-safe audit events: principal, profile, tool and destination
   host, with no bodies or secrets.

## 13. Proxmox deployment

| | Dedicated Proxmox VM | Proxmox LXC container | Standalone Linux host |
| --- | --- | --- | --- |
| Boundary to Proxmox host | **Hardware virtualization** | Shared host kernel | n/a (it is the host) |
| User namespaces for bwrap | Own kernel; configure as on any Ubuntu host | Needs `nesting=1` and AppArmor changes on the **Proxmox host**; nested user namespaces inside an unprivileged container | Own kernel |
| cgroup v2 delegation (future limits) | Under your control | Limited by the container's cgroup | Under your control |
| A sandbox escape reaches | The VM | The Proxmox host's kernel, shared with every container | The host |
| Recovery | Snapshot / restore the VM | Container restore; host exposure remains | Rebuild |

**Recommendation:** for the first security validation environment, use a
**full VM** dedicated to Lightagent (or to a future sandbox worker). It
gives an independent kernel and a virtualization boundary from the Proxmox
host. Do not relax sandbox requirements to fit LXC nesting. Automated
validation remains on GitHub-hosted runners. This PR requires no Proxmox
machine.

## 14. Rollback

Revert the PR. Nothing calls the backend, no configuration key or data
format changed, and the gate's behavior for every existing backend is
unchanged. The new CI job is a separate, non-required check. Removing it
leaves the nine required checks as they were.

## 15. Remaining before any verification

- A seccomp filter.
- Kernel resource limits (delegated cgroup v2 per sandbox, with
  `cgroup.kill` for crash-safe cleanup).
- A bounded scratch.
- fd-based root binds.
- A separate identity, or a microVM or remote worker.
- An egress proxy for allowlisted network.
- Escape testing on more kernels.
- Independent review.
- The remote-tool work in §12.
- Explicit authorization.
