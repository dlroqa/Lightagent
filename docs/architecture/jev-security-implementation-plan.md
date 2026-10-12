# Security implementation plan: Jev routing, sensitive data and profile authorization

Status: **plan only.** Nothing here is implemented, and each PR below needs
its own approval. It turns the open issues in
[`jev-routing-permission-and-profile-authorization-proposal.md`](jev-routing-permission-and-profile-authorization-proposal.md)
and [`profile-toolset-enforcement-proposal.md`](profile-toolset-enforcement-proposal.md)
into ordered, reviewable work.

Principles that hold for every PR:

- **Additive.** A configuration written before the PR behaves exactly as
  before, unless an operator opts in.
- **Nothing silent.** Every default change is announced, is reversible by
  one setting, and comes after a report-only release.
- **Unchanged throughout:** Lightweight, the Auto guard, the 0.9 threshold,
  classification `off` and `workflow_guidance = false` by default.
- **Fail closed** for anything new (a missing permission means *no*
  external call), while existing paths keep their current behavior until
  the operator opts in.
- **CI:** the eight gates and the deterministic evaluation stay green, and
  each PR adds its security tests to the adversarial suite.

## Priority order

| # | Work | Why this position | Depends on |
| --- | --- | --- | --- |
| 1 | Per-profile permission for legacy external routing | Closes the only path that sends any profile's text to an external service without a profile check; small and self-contained | — |
| 2 | Safer sensitive-message handling | Removes key material and attachments from `legacy_raw`, behind an opt-in first | 1 (reporting reuses its diagnostics) |
| 3 | User identity and profile ownership | Makes per-profile permissions mean per-user protection | — |
| 4 | Runtime profile-toolset enforcement | Narrows what each profile can do; already specified | — (benefits from 3) |
| 5 | Tenant-safe diagnostics and authorization | Diagnostics, run events and status only for the caller's profiles | 3 |

Items 1 and 2 address the data that leaves the machine, so they come first.
Item 3 is the foundation for 5. Item 4 is independent and can run in
parallel with 3.

## 1. Per-profile permission for legacy external routing

> **Implemented** in [`jev-routing-permission.md`](jev-routing-permission.md),
> with these changes from the sketch below, as directed for this PR: the list is
> `routing_allowed_profiles`, under an explicit `routing_permission` policy
> (`strict`, the default for new configurations, or the identified
> `legacy_all_profiles` compatibility policy for existing ones) instead of an
> `Option` whose `None` means all profiles. A dry-run report
> (`lightagent config jev-routing-report`) shows who strict would deny.

**Change.**
- Add `platform.jev.routing_profiles: Option<Vec<String>>`.
- `jev::route` checks it before `routing_state`, so nothing is derived from a non-permitted message.
- The fallback code is `profile_not_permitted`, and `routing.selected` reports it.
- Settings shows the permission beside classification's.
- A warning appears when routing is enabled with `routing_profiles` unset and `routing_input = legacy_raw`.

**Compatibility.** `None`, the default for every existing file, means all
profiles: today's behavior. `Some([])` disables routing for everyone.

**Migration.** None automatic. The release notes and the Settings warning
tell operators with sensitive profiles to set it.

**Security tests.**
- A non-permitted profile makes no routing request, under both input policies.
- `None` is byte-for-byte today's request.
- The check precedes `routing_state`; a cancelled run checks nothing.
- The Auto guard and explicit models are unchanged.
- The routing benchmark's `profile_gated_markers_transmitted` drops to zero when the permission is set.

**PR boundary.** Configuration field, `jev::route` check, settings get/set,
Settings UI row, render state, tests, docs. No change to classification or
the adapter. About 300 lines plus tests.

## 2. Safer defaults for sensitive-message handling

**Change, in three releases:**
1. **Report-only.** Diagnostics count, per routing request, what
   `legacy_raw` sent that `sanitized` would not have: key material,
   attachment, long paste, pattern marker. Only counts are kept, never
   content. Settings shows them.
2. **Opt-in guard.** A new `platform.jev.legacy_raw_guard` (default
   `false`). When on, `legacy_raw` still sends the raw message but never key
   material, attachments or text over the sanitizer's source bound.
3. **New-installation default.** Configurations created after the release
   get `routing_profiles = Some([])` and `routing_input = sanitized`.
   Existing files are untouched. An explicit `lightagent config migrate-jev-privacy` command, previewed with `--dry-run`, offers the change to
   existing installations.

**Compatibility.** Releases 1 and 2 change nothing unless opted in. Release 3
affects only new configurations.

**Migration.** The explicit command only, with a dry run, a backup of the
config file, and a one-line revert.

**Security tests.**
- The guard refuses key material, attachments and over-long input under `legacy_raw`.
- Guard off is today's behavior.
- The report-only counters never contain message text.
- The migration command is idempotent and reversible.

**PR boundary.** One PR per release step. Sensitive prose remains
undetectable by patterns; the control for it is items 1 and 3.

## 3. User identity and profile ownership enforcement

**Today.** `lightagent serve` admits a loopback bind without any key.
Otherwise it takes one key through `--key-env`, and that key always holds
`Admin` (`serve.rs`). The API's `Scope` set exists, but no key is narrower
than `Admin` and none is bound to a profile.

**Change.**
- A configured key list: `api.keys = [{ id, secret_ref, scopes, profiles }]`.
  `--key-env` keeps working as one unbound `Admin` key.
- A key with `profiles` set may start runs as, and read sessions, approvals
  and run events of, those profiles only. `Admin` remains unrestricted.
- The check sits where the manager resolves `StartRun.profile`, before
  preflight, and on every session and run lookup.
- The terminal and ACP keep local-user semantics: one OS user, one installation.

**Compatibility.** `--key-env` and loopback-open mode keep today's
semantics exactly. Only keys an operator adds to `api.keys` can be narrower.

**Migration.** None. `api.keys` is new and empty by default.

**Security tests.**
- A bound key cannot run as another profile.
- It cannot read another profile's sessions, run events or approvals.
- It cannot approve another profile's pending call.
- An unbound key keeps today's access.
- Invalid bindings, such as an unknown profile, fail closed at load.

**PR boundary.** Auth model and manager checks only. No UI login, no user
accounts and no general access-control rewrite. A follow-up PR adds the
Settings view of keys.

## 4. Runtime profile-toolset enforcement

As specified in `profile-toolset-enforcement-proposal.md`.

**Change.**
- Report-only: list the tools a non-empty `toolsets` would hide.
- Then opt in through `tools.enforce_profile_toolsets`.
- Then default on in a later release.

**Compatibility.** `toolsets = []`, every existing profile, means no
restriction at every step. A list can only remove tools.

**Security tests.**
- An empty list gives today's registry for runs, `GET /tools` and workers.
- A non-empty list hides tools, and an unlisted tool call is refused even if
  the model names it.
- A list never adds a tool, and approvals are unchanged.
- Jev classification has no effect on the result.

**PR boundary.** One PR per step. The workflow policy needs no change: it
already inspects only the registry a run receives.

## 5. Tenant-safe diagnostics and authorization

**Change.** With item 3 in place:
- `GET /jev` returns the status of the caller's bound profile, or of a
  requested profile it is bound to, instead of always the active profile.
- `classification.completed` events go only to callers who may read that run.
- `POST /jev/verify` stays `Admin`.
- `JevState` is already keyed by profile and holds only fixed vocabulary; it
  gains a test that a bound key cannot read another profile's record.
- Settings shows Jev controls only to `Admin`.

**Compatibility.** Unbound keys see what they see today.

**Security tests.**
- A cross-profile status read is refused.
- Another profile's run events are not delivered.
- `verify` is refused without `Admin`.
- Diagnostics never contain message text. The existing assertion is extended
  to every new field.

**PR boundary.** API handlers and SSE filtering. It depends on item 3's
binding, so it follows it.

## Remaining risks while this plan is pending

- Every profile's text can reach TypeSafe through legacy routing.
  Mitigation: clear both routing allowlists on installations with sensitive
  profiles (tested).
- `legacy_raw` sends key material, attachments and long pastes.
  Mitigation: `routing_input = sanitized`, or routing off.
- Prose is never reliably redacted. Mitigation: profile permissions, and
  routing off for sensitive profiles.
- Any API caller, open loopback or holder of the single `Admin` key, can act
  as any profile. Mitigation: keep the API on loopback or behind a trusted
  proxy, and give the key only to trusted callers.
- Toolsets are advisory. Mitigation: approval policy per profile.

## Recommended next PR

Item 1, per-profile routing permission, alone. It is the smallest change
that closes the most direct external-disclosure path. It is additive with an
exact-compatibility default, and its tests extend the existing routing
benchmark and adversarial suite.
