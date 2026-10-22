# Sandbox cancellation review: "cancel-startup 2ms: process survived"

**Outcome: a real teardown defect in the experimental bubblewrap backend. It was reproduced on the unchanged base, fixed in PR #41, and measured with the same harness before and after the fix.** The backend has never been reachable from a request path, so no run was affected.

## The report

At J's earlier head `39efccd`, check run 39693086068, the job "Linux sandbox (bubblewrap, experimental)" failed once:

```text
panicked at crates/lightagent-tools/tests/sandbox_bubblewrap.rs:727:13:
cancel-startup 2ms: process survived
```

It passed at every later head. The earlier record called it a "pre-existing timing flake" because no changed file touched the sandbox. That was a guess, not a finding.

## Exposure

- `lightagent_tools::sandbox::bubblewrap` is constructed only by its own tests, `tests/sandbox_bubblewrap.rs`. No server, CLI, worker or tool path builds it.
- Its assurance is `Assurance::Experimental`, and `SandboxGate::route` refuses every experimental backend (`DenialReason::NotVerified`). That holds even in the test gate.
- Untrusted runs get no execution at all: coder, reviewer and tester stay `permission_denied` (`requires_isolation`). This review does not change that, and tenant execution stays disabled.

## Cause

On timeout or cancellation, `kill_tree` sent `SIGSTOP` to the outer `bwrap` and then listed its descendants in `/proc`. But `kill(1)` returns once a signal is **queued**, not delivered. The outer `bwrap` could still be running on another CPU and finish cloning the PID namespace's init after the list was taken.

`--die-with-parent` arms that init's parent-death signal only after the clone. So killing the outer `bwrap` left the init, and the sleeping test program under it, alive. Killed descendants also linger as zombies under the stopped parent and were listed again.

## Fix (PR #41, `crates/lightagent-tools/src/sandbox/bubblewrap.rs`)

1. After `SIGSTOP`, wait until `/proc/<pid>/stat` reports the outer process stopped (`T`), or gone, for at most 500 ms.
2. List live descendants, skipping zombies but still walking through them, and kill them.
3. Repeat up to 20 rounds until nothing live is left, then kill the outer process, as before.

## Evidence (GitHub-hosted runners, ubuntu-24.04)

The harness is a new, non-required workflow, `sandbox stress`. It runs the live test `the_whole_process_tree_ends_with_the_execution` repeatedly against any `source` commit. Each run has its cancellation loop lengthened from 30 to 300 attempts (cancellations from 0 to 500 ms), with every CPU busy, so the base and the fix are measured by the same harness.

| Source | Run | Test runs | Cancellations | Failed runs | Failure text |
| --- | --- | --- | --- | --- | --- |
| Base `30f5b2b` (K, unfixed) | 39701178748 | 10 | 300 each | **10** | (summary only) |
| Base `30f5b2b` (K, unfixed) | 39701848102 | 5 | 300 each | **5** | `cancel-startup 1ms`, `0ms`, `0ms`, `2ms`, `0ms`: `process survived` |
| Fix `f7c7df6` | 39700020112 (pull request) | 10 | 300 each | **0** | none |
| Fix `f7c7df6` | 39701181164 | 10 | 300 each | **0** | none |

Each failed run stops at its first survivor, so on the base the defect shows up within the first 300 startup cancellations under load. The regular `check` sandbox job, 30 attempts without load, hits it rarely, which matches the single CI failure. Every final head of the stack is also checked by the regular sandbox job.

What this does not prove: that teardown is complete under every kernel, scheduler or cgroup configuration. The backend stays experimental and unreachable. Kernel CPU, memory and process limits are still unsupported (cgroup v2 is not delegated on the runner), as `unsupported()` already declares.

## Rollback

Revert the commit on `bubblewrap.rs`. The stress workflow is a measurement and can stay or go independently.
