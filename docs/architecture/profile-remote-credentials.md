# Profile-scoped remote credentials

Status: implemented, **not activated**. Profile grants apply only to
untrusted (multi-user) runs, and `lightagent serve` never builds multi-user
mode. Everything below is exercised by tests that build untrusted runs
directly, with synthetic secrets and local stub services. None of it is
production end-to-end behavior yet.

A user must independently be authorized for:

1. the **profile**, by ownership (PR #16/#24);
2. the **tool**, by the profile's tool policy (PR #17);
3. the **credential**, by a vault grant (this PR);
4. the **destination**, by the egress rules and the grant's origins (PR #20,
   this PR).

No one of these implies another.

## 1. Credential architecture before this PR (master `8022213`)

```text
config.toml ── SecretRef {Env | File} ── resolve() at run build ──┐
  web.search.api_key                                                ├─ operator run: always
  web.search.credential_profiles  (profiles whose untrusted         │
                                   runs may borrow it)              ├─ untrusted run: only if listed
  mcp.servers[].auth / headers                                      │
  mcp.servers[].credential_profiles / untrusted_profiles            ┘
inference.api_key (provider)  ── operator-only; the provider is installation-wide
platform.jev.endpoint.api_key ── operator-only; Jev permissions are separate (D3)
```

`chat::web_context_for` and `chat::to_mcp_spec` resolved the installation's
secret into the run when the profile was listed. Every credential was
installation-wide: an untrusted run could only *borrow* one, and nothing
re-checked the borrowing after the run was built. Lightagent API credentials
(the identity store) are verified credentials, stored as one-way verifiers;
they are not used for outbound requests.

## 2. Model

`lightagent_core::credentials::CredentialGrant`:

| Field | Meaning |
|---|---|
| `id` | 64-bit random hex; not a secret |
| `profile` | the one profile it is granted to |
| `service` | `web-search` or `mcp:<server>` |
| `operations` | tool names it may be used for (`None`: all of the service) |
| `destinations` | origins (`scheme://host[:port]`) it may be sent to; never empty |
| `expires_unix`, `revoked_unix` | lifecycle; revocation is final |
| `version`, `rotated_unix` | bumped by rotation |
| `max_requests_per_run` | optional per-run request budget |
| `sealed` | the encrypted secret (§3) |

Lookup is by `(profile, service)`, never by an id a caller supplies, so one
profile cannot select another's credential. Owning a profile grants no
credential, and a tool being available grants no credential.

## 3. Secret storage and key management

**Options evaluated:**

- **OS keychains** need a new dependency and are unavailable on headless CI
  runners and servers.
- **A key file inside the home** sits beside the ciphertext. Rejected by
  requirement.
- **Obfuscation or plaintext.** Rejected.

**Chosen:** encryption with key material outside the home.

- **Cipher.** AES-256-GCM from `ring` 0.17, which is already in the tree as
  the rustls provider; the dependency policy pins `ring`. No new crate and
  no custom cryptography.
- **Per-record keys.** Each record's key is derived with
  HKDF-SHA256(master key, 128-bit random salt,
  `"lightagent-credential-v1"`), with a random 96-bit nonce.
- **Binding.** The additional authenticated data is
  `id ‖ profile ‖ service ‖ version`, so a sealed secret moved to another
  record, profile, service or version fails its integrity check.
- **Master key source.** The 256-bit key comes from
  `LIGHTAGENT_CREDENTIAL_KEY_FILE` (64 hex digits) or
  `LIGHTAGENT_CREDENTIAL_KEY`. The key file must be **outside** the
  Lightagent home (checked on canonical paths). On Unix it must be a
  regular file, mode `0600` or stricter, owned by the account that owns the
  home; mode and owner are checked on the opened file, so a path swapped
  after the check is never read. An unreadable home proves no owner, so the
  key is refused. `remote-credential keygen` creates one as a new `0600`
  file.
- **Windows.** There is no POSIX mode or owner to check, so neither check
  runs: the key file's ACL is the operator's responsibility (owner-only
  read, no inherited group access). The outside-the-home check still
  applies. Prefer the key file over the environment variable there too.
- **Fingerprints.** Each record carries a short fingerprint of the key that
  sealed it, so the wrong key is reported as `key_mismatch` instead of a
  decryption failure.
- **Fail closed.** These all mean *no credential*, never plaintext and never
  another credential:
  - no key, an invalid key, a mismatched key;
  - an authentication-tag failure;
  - a corrupt, older or newer document.
- **At rest.** Metadata and ciphertext live in `<home>/credentials/state.json`
  (owner-only). It is written by the identity store's locked, `fsync`ed,
  atomic-rename helpers, reused rather than duplicated. A backup or copy of
  the home holds no usable credential without the key.
- **Redaction.** `MasterKey`, `ExternalSecret`, `SealedSecret`,
  `CredentialUse`, `ProfileCredentials` and `WebPolicy` all redact in
  `Debug`. Errors and audit records hold codes only.

**Limits.**

- A process that has the key *and* the home can open every credential.
  That is inherent to retrievable outbound credentials.
- The key in an environment variable is visible to processes of the same
  user. Prefer the key file.
- There is no hardware or OS keystore integration yet.

## 4. Request-time enforcement

```text
untrusted run built ─► ProfileCredentials::load(home, profile)  (vault exists?)
                         ├─ no vault  ─► legacy configuration grants (unchanged)
                         └─ vault     ─► credentials::resolve(profile, service)
                                           Absent  → no credential (nothing borrowed)
                                           Denied  → service refused (search) / not reached (MCP)
                                           Active  → secret + CredentialUse guard
each remote request:
  ctx.authorized()             run authorization (PR #22; supervised by the PR #23 monitor)
  call guard                   tool still permitted by the profile's tool policy (PR #17)
  egress / untrusted dests     destination allowed for the profile (PR #20)
  CredentialUse::authorize()   re-reads the vault: grant present, same profile and
                               service, not revoked or expired, same version, origin
                               in destinations, operation allowed, budget left
  cancellation                 the executor's select! (PR #22)
  ─► only then is the request built with the credential
```

- **Every request re-reads the vault metadata.** That is a small bounded
  file read; no positive answer is cached. A revocation, expiry or rotation
  made by the operator CLI in another process stops the next request.
- **Rotation supersedes.** A run that opened version *n* stops when version
  *n+1* exists (`superseded`). New runs get the new secret.
- **Budgets** count requests per run, including MCP protocol requests.
  The count is one atomic decrement per request on the run's single
  `CredentialUse`, taken only after every other check passes, so racing
  requests never exceed it (`concurrent_requests_never_exceed_the_budget`:
  64 threads, budget 5, exactly 5 succeed). A run builds one web context
  (shared with its delegated workers) and one MCP registry, so it holds one
  counter per grant. Listing a profile's tools (`tools_for`) connects with a
  separate, throwaway count: MCP protocol requests only, never a tool call.
  Operations restrict MCP `tools/call` by tool name, and `web.search`.
- **No fallback.** Once a vault exists, untrusted runs never receive an
  installation credential, even for a profile the configuration lists in
  `credential_profiles`. A denied grant refuses `web.search` outright, and
  the MCP server is not connected at all. Installation `auth` and custom
  headers are never sent to an untrusted run in vault mode.
- **Denials are audited** (`credential_use`, `denied:<code>`).

## 5. Remote integration

**`web.search`.** The search client never follows redirects (unchanged). The
grant's origin must match the configured endpoint. The PR #20 untrusted
destination list still applies.

**HTTP MCP.**
- `HttpConnection::with_credential` checks the grant before *every* request
  and notification. The tool name is the operation for `tools/call`.
- Redirects are refused (PR #20, unchanged), so a credential cannot follow
  one.
- `untrusted_profiles` still decides whether a profile may reach the server
  at all.
- Untrusted runs never spawn stdio servers (unchanged).

**Preserved.** DNS-pinned vetting, private-network and metadata
protections, the proxy bypass and secret-safe errors all stay unchanged.
Credential approval never relaxes a destination rule.

**Not in scope.** The provider key (`inference.api_key`) and the Jev key
stay installation-wide and operator-only. Untrusted runs do not choose a
provider, and Jev's permissions are separate (D3). `web.fetch` sends no
credential.

## 6. Authorization matrix (untrusted runs, vault present)

| Profile ownership | Tool permitted | Grant for (profile, service) | Destination | Result |
|---|---|---|---|---|
| no | — | — | — | run refused (PR #16) |
| yes | no | any | any | tool not offered and refused by the call guard (PR #17) |
| yes | yes | none | allowed | MCP connects without credentials; search runs only if no key is configured |
| yes | yes | revoked, expired, unusable | any | search refused; MCP server not reached |
| yes | yes | active | origin not in grant | request not sent (`destination`) |
| yes | yes | active | in grant, egress allowed | sent with the profile's credential, if the operation and budget allow |

Operators, the terminal, ACP and single-user runs keep installation
credentials exactly as before.

## 7. Operator CLI (`lightagent remote-credential`)

| Command | Effect |
|---|---|
| `keygen <path>` | new key in a new `0600` file outside the home; prints only its fingerprint |
| `status` | vault presence, generation, grant counts, whether the configured key opens them |
| `add <profile> --service S --destination O… [--operation N…] [--expires-in S] [--max-requests-per-run N] (--secret-file F \| --secret-stdin)` | seal and grant |
| `rotate <id> (--secret-file F \| --secret-stdin)` | new version |
| `revoke <id>` | final |
| `list [--profile P]` | metadata only; never ciphertext |
| `audit [--limit N]` | codes and identifiers |
| `rekey --previous-key-file F` | re-seal every active grant from the previous key to the configured one, all or nothing |

The CLI rules:

- Secrets are never accepted on the command line, which would expose them
  in shell history and the process list.
- Secrets are never printed.
- Read-only commands never create the vault.
- Only an operator holding `administer` changes it.

There is no API and no user-facing management; the provisioning website is
out of scope.

## 8. Security tests

| # | Requirement | Test |
|---|---|---|
| 1 | A cannot use B's credential | `credentials::tests::a_profile_resolves_only_its_own_grant`; `chat::…::vault_grants_replace_installation_credentials_for_untrusted_runs`; `vault_mcp_credentials_follow_profile_grants` |
| 2 | Ownership alone grants nothing | same (an owned profile without a grant gets nothing) |
| 3 | Enabled tool without a grant | `vault_grants_replace…` (billing); MCP billing case |
| 4 | Expiry and revocation | `every_request_is_checked_against_the_current_grant`; `every_search_rechecks_the_profile_grant` |
| 5 | Rotation | `every_request_is_checked…` (superseded); `rekeying_is_all_or_nothing`; CLI test |
| 6 | Revocation during an active run | `every_search_rechecks…`; `vault_mcp_credentials…` (mid-run call refused, no request) |
| 7 | Withheld from unauthorized MCP servers | `vault_mcp_credentials…`; unchanged PR #20 MCP tests |
| 8 | Not forwarded through redirects | `every_search_rechecks…` (redirect target sees nothing); `an_http_mcp_redirect_is_refused…` |
| 9 | Destination denial | `every_request_is_checked…`; `every_search_rechecks…` |
| 10 | Failed decryption | `opening_fails_closed` (wrong key, tampering, moved records) |
| 11 | Corrupt records | `opening_fails_closed` (corrupt, older, newer) |
| 12 | Concurrent use and revocation | `every_request_is_checked…` (threads around a revocation) |
| 13 | Secret-free logs, Debug, errors and audit | `nothing_persisted_holds_the_secret_or_the_key`; CLI and `every_search…` audit checks |
| 14 | No plaintext at rest | `nothing_persisted_holds_the_secret_or_the_key` |
| 15 | No privileged fallback | `vault_grants_replace…`; `vault_mcp_credentials…` |
| 16 | Single-user compatibility | operator assertions in `vault_grants_replace…`; unchanged suites |
| 17, 18 | Jev and Lightweight Auto unaffected | no Jev, routing or Lightweight code changed; Jev evaluation and paired E2E checks |
| 19 | Cross-platform storage | core vault tests on Linux, Windows and macOS runners (POSIX permission checks are `cfg(unix)`) |
| 20 | Failed identity-store access | run authorization precedes credential use; PR #22/#23 fail-closed tests unchanged |

Plus `the_key_must_be_well_formed_and_kept_apart`,
`changes_are_operator_only_and_validated`, and the CLI tests
`credentials_are_added_listed_rotated_and_revoked_without_disclosure`,
`invalid_commands_change_nothing` and
`keygen_writes_a_new_owner_only_key_outside_the_home`.

## 9. Threat model

| Threat | Mitigation |
|---|---|
| Disk or backup theft of the home | Ciphertext only; the key is kept outside the home |
| Copying a sealed secret to another profile or record | AAD binding: fails integrity |
| Stale or long-held authorization in a run | Per-request vault re-read; final revocation; version supersession |
| Credential exfiltration by redirect | Redirects never followed; origin check before sending |
| Prompt-driven destination change | Grant origins plus egress rules, both enforced server-side |
| A profile naming another's credential | Lookup by `(profile, service)` only |
| Privilege fallback | Vault mode never resolves an installation credential for untrusted runs |
| Log or diagnostic leakage | Redacted `Debug`; code-only errors and audit |
| Wrong or missing key | `key_mismatch` / `key_missing`: fail closed |

## 10. Compatibility and migration

- **Nothing changes until an operator creates a vault** (the first
  `remote-credential add`). The CLI changes nothing else either.
- **Activating vault mode.** Once the vault exists, untrusted runs stop
  using the configuration's `credential_profiles` and must be given grants.
  To move a profile:
  1. `remote-credential add <profile> --service … --destination …` for each
     service;
  2. remove the profile from the `credential_profiles` lists.
- **Trusted runs.** Operator, terminal, ACP and single-user runs are
  unchanged.
- **Code compatibility.** `WebPolicy` gains `search_credential` (default
  `None`). `McpTransportSpec::Http` gains `credential`. `HttpConnection`
  gains `with_credential`.
- **Dependencies.** `ring` becomes a direct dependency of `lightagent-core`.
  It was already in the graph; no new crate.

## 11. Rollback and recovery

- **Rollback (backup-preserving).** Never delete vault data as a rollback
  shortcut.
  1. Stop the server and any `remote-credential` command.
  2. Copy `<home>/credentials/` (`state.json`, `audit.jsonl*`) to an
     owner-only backup location, and record the key fingerprint
     (`remote-credential status`). Keep the key file; the backup is
     ciphertext only.
  3. Revert the PR and deploy the previous build. That build never reads
     `<home>/credentials/`, so the directory stays in place, unused.
     Untrusted runs return to the configuration's grants (multi-user mode
     is not activatable in either build).
  4. Moving forward again reads the same, untouched vault.
- **Never restore a revoked credential.** Revocation is a field in
  `state.json`, so an older copy can hold a grant as active that was
  revoked since. Before restoring any backup of `state.json`:
  1. compare its `generation` with the current one; restore only the
     newest copy that is not known to be corrupt;
  2. read `audit.jsonl` (kept outside the restored file) for every
     `credential_revoked` and `credential_rotated` after the backup's time,
     and re-run `remote-credential revoke <id>` for each revocation right
     after the restore, before the server starts;
  3. for a rotation after the backup, `revoke` the restored grant and
     `add` it again with the current secret (the restored version holds
     the old secret);
  4. where the remote service supports it, also revoke the secret at the
     service itself: that is the only revocation no restore can undo.
- **Lost key.** Sealed secrets cannot be recovered. Revoke them and add new
  ones under a new key.
- **Key rotation.** `remote-credential keygen` a new key, point
  `LIGHTAGENT_CREDENTIAL_KEY_FILE` at it, then
  `remote-credential rekey --previous-key-file <old>`. It is all or nothing.
- **Corrupt vault.** Untrusted runs get no credential (fail closed). Restore
  `state.json` from a backup as above (it holds no plaintext), replaying
  later revocations before the server starts.

## 12. Remaining limitations

- Multi-user mode is not activated, so none of this runs in production.
- No OS or hardware keystore. The master key is as protected as the file or
  environment that holds it.
- Budgets are per run, not per period. There is no rate limiting across
  runs.
- Per-request vault reads are bounded but not free: one small JSON read and
  parse per remote request of a vault-granted run.
- The provider and Jev keys stay installation-wide by design.
- Remote side effects already committed are not undone by revocation.
