# Remote tool egress: redirects, SSRF and credential isolation

Status: **implemented on branch `security/remote-tool-egress`.** It
hardens existing behavior for every run, and adds deny-by-default scoping for
untrusted runs, which still cannot exist because multi-user mode remains
unactivatable. Lightweight, Jev and the sandbox are unchanged.

Every remote request now passes independent checks:

```text
 caller identity ──► profile authorization ──► tool authorization ──► credential authorization ──► destination policy ──► safe transport
 (AuthConfig, #16)   (identity::authorize)     (policy::resolve,     (RemoteAccess::may_use:       (egress::vet:            (egress::pinned_client:
                                                call guard, #17)      search key, MCP auth and      scheme, userinfo,        pinned addresses, no
                                                                      headers per profile)          profile scope, allow-    proxy, no auto-redirect;
                                                                                                    list, EVERY resolved     egress::Redirects per hop;
                                                                                                    address, management      MCP: redirects refused)
                                                                                                    endpoints)
```

Passing one check implies nothing about the next. Being allowed to call
`web.search` does not grant the search key, and an allow-listed host is not
an authorized address. Jev classification feeds none of these checks.

## 1. Request architecture (audited at master `b4a8182`)

| Path | Source | Flow before this change |
| --- | --- | --- |
| `web.fetch` | `lightagent-tools/src/builtins/web.rs` `WebFetch::call` → `fetch_text` | `guard_url` (scheme, `allow_domains`; **if listed, return without any address check**), then `guard_host` (resolve; all addresses public) → pinned client (`resolve_to_addrs`) or the shared client. Manual redirects, up to 5, each re-guarded. |
| Composite retrieval | `lightagent-rag/src/realtime.rs` `RealtimeRag` → `search_results` + `fetch_text` | Same guards |
| `web.search` | `web.rs` `search_results` | Shared client (redirects off) to the operator's endpoint, `bearer_auth(search_api_key)`: one installation key for every run |
| Web context | `lightagent/src/chat.rs` `web_context` | One `WebContext` per run from config; the key resolved for every run |
| HTTP MCP | `chat.rs` `mcp_tools` → `lightagent-mcp` `McpHub::connect` → `HttpConnection::builder`/`request` | reqwest client **with default redirects (up to 10)**; configured `headers` and `auth` bearer sent on every request, for every run |
| Stdio MCP | `chat.rs` `to_mcp_spec` → `StdioConnection::spawn_in` | Spawned for every run's registry, untrusted runs included (their tools were then withheld by the tool policy) |
| MCP risk | `lightagent-mcp/src/client.rs` `risk_from_annotations` | `readOnlyHint` → External (auto-approved under balanced); `destructiveHint` → Mutating; none → Executable |
| Address classification | `web.rs` `is_public_ip`, `embedded_v4` | Loopback, private, link-local, CGNAT, documentation, multicast, reserved, ULA, and v4 embedded in v6 → non-public |
| Approvals | `executor.rs` `BoundedExecutor::approval_for`/`invoke` | Per tool risk; unchanged here except untrusted MCP (below) |
| Run trust | `serve.rs` `run_as(trust)`; `RunTrust` | Used by the tool policy only, not by credentials or destinations |
| Existing tests | `web.rs` `guard_returns_pins_for_ips_and_skips_trusted_hosts`, `public_addresses_are_told_from_private_ones`, …; `context.rs` `web_policy_debug_never_prints_the_key`; `realtime.rs` local stub via `allow_domains: ["localhost"]` | The first and last **relied on the allow-list bypass** |

## 2. Findings: confirmed versus theoretical

No test made a request to a real internal, metadata or production system.
"Confirmed" means the code path exists as described, and the adversarial
tests below reproduce it against local stubs or the dependency's own source.

| # | Finding | Status | Exploitable when |
| --- | --- | --- | --- |
| C1 | `allow_domains` skipped every address check and pinning: an allow-listed name resolving to loopback, private or metadata was fetched | **Confirmed** | Any allow-listed domain or subdomain (the list matches subdomains) whose DNS returns, or can be made to return, a non-public address |
| C2 | HTTP MCP followed redirects; reqwest 0.13.4 strips only `Authorization`, cookies and proxy auth on a cross-host hop (`redirect.rs` `remove_sensitive_headers`), so **custom `headers` (API keys) and, on 307/308, the JSON-RPC body (tool arguments) reached the redirect target** | **Confirmed** | A configured MCP server (or anything in its path) answers with a redirect |
| C3 | reqwest honors `HTTP_PROXY`/`HTTPS_PROXY`/`ALL_PROXY` by default (`Matcher::system()`); a proxied request ignores `resolve_to_addrs`, so the **proxy re-resolves the name** (DNS rebinding) and can reach private networks | **Confirmed** | The server runs with proxy variables set |
| C4 | The Azure WireServer host agent is a public-range address and was fetchable | **Confirmed** (not probed) | Deployed on Azure, which includes GitHub-hosted runners |
| C5 | `web.block_private_addresses = false` also opened cloud metadata | **Confirmed** | An operator turned the switch off |
| C6 | Error messages and reqwest errors carried the full URL (paths, query secrets); userinfo URLs were accepted, and reqwest sends them as Basic auth | **Confirmed** | A secret in a URL, or `user:pass@` |
| C7 | Installation-wide search key and MCP credentials were given to every run's context, untrusted included; untrusted runs started stdio MCP servers | **Latent** | Only once multi-user runs exist; not reachable today |
| C8 | A server's own `readOnlyHint` made its tool auto-approved under balanced | **Theoretical** | A malicious or compromised MCP server |
| T1 | An operator run's `web.fetch` can be steered (prompt injection) to send data to any public host | Theoretical, by design for operators | Untrusted runs are now scoped (below) |
| T2 | The search endpoint and MCP URLs are not address-vetted | Theoretical | Operator-configured, often deliberately private |
| T3 | No rate limits on remote calls | Theoretical | — |

## 3. Changed source files

| File | Change |
| --- | --- |
| `crates/lightagent-tools/src/egress.rs` (new) | `EgressRules`, `vet`, `Resolver`/`SystemResolver`, `Network`, `is_management_endpoint`, `Redirects`, `pinned_client`, `host_matches`, `EgressDenial` |
| `crates/lightagent-tools/src/egress/tests.rs` (new) | Unit tests (no network) |
| `crates/lightagent-tools/src/builtins/web.rs` | `fetch_text` on `egress`; old `guard_url`/`guard_host`/`pinned_client` removed; search credential and destination gates; URL-free errors; the bypass-asserting test replaced |
| `crates/lightagent-tools/src/context.rs` | `WebPolicy` gains `trusted_networks`, `untrusted_destinations`, `search_credential_withheld`, `Default`, `egress_rules()`; `WebContext` gains `resolver` and `new()` |
| `crates/lightagent-tools/tests/remote_egress.rs` (new) | HTTP-level adversarial tests |
| `crates/lightagent-core/src/config.rs` | `web.trusted_networks`, `web.untrusted_destinations`, `web.search.credential_profiles`, MCP `untrusted_profiles` / `credential_profiles`; validation; tests |
| `crates/lightagent-mcp/src/connection.rs` | Redirects reported and refused; errors without URLs |
| `crates/lightagent/src/chat.rs` | `RemoteAccess`; `web_context_for`, `configured_registry_for`; MCP client without redirects; grants applied; `ApprovalRequired` for untrusted MCP tools; tests |
| `crates/lightagent/src/serve.rs` | `run_as` builds the web context and registry for the run's trust and profile |
| `crates/lightagent-rag/src/realtime.rs` | Test reaches its local stub through `trusted_networks` |

## 4. Redirect and credential handling

- **`web.fetch`** (carries no credential): automatic redirects are off.
  Each `Location` passes `egress::Redirects::next`:
  - at most 5 hops;
  - a revisited URL is refused as a loop;
  - http(s) only;
  - no userinfo;
  - **no HTTPS-to-HTTP downgrade**.

  Then the target is vetted and pinned like the first hop.
- **Credentialed requests** (`Redirects::new(.., credentialed: true)`): any
  hop to another scheme, host or port is refused (`cross_origin_redirect`).
  No credential is ever re-sent to a new origin.
- **HTTP MCP**: the client has redirects off, and `HttpConnection` reports
  `redirect refused (HTTP 30x); configure the server's final URL`. Headers,
  bearer and body never leave the configured origin. This does not rely on
  reqwest's partial header stripping.
- **`web.search`**: already no redirects (a 30x is an error).
- **Messages**: name the host only. `reqwest::Error::without_url()`
  everywhere a transport error is shown, and `EgressDenial` messages are
  fixed text.

## 5. DNS and SSRF enforcement

`egress::vet(rules, url, resolver)`:

1. Scheme http or https. **Userinfo refused.**
2. Host extracted after the URL parser has normalized alternate IPv4 forms:
   `2130706433`, `0x7f.1`, `127.1` and `0177.0.0.1` all become
   `127.0.0.1`.
3. Untrusted run: the host must match its profile's
   `untrusted_destinations` (empty means nothing).
4. `allow_domains`, if set, must match. This is a scope, **never an
   exemption**.
5. A literal IP is judged directly. A name is resolved once through the
   injectable `Resolver`, and **every** address must pass:
   - **Management endpoint** (the link-local cloud metadata service and the
     ECS task-metadata address, Azure WireServer, Alibaba and Oracle metadata, AWS IMDS over IPv6,
     IPv4-mapped forms): refused. The only exception is an operator
     trusted-network entry naming that exact address (`/32` or `/128`); a
     broad range never covers it.
   - **Operator trusted network** (`web.trusted_networks`, CIDR): permitted
     for operator runs only.
   - **Public** (`is_public_ip`: excludes loopback, RFC 1918, link-local,
     CGNAT, unspecified, broadcast, documentation, benchmarking, multicast,
     reserved, ULA, IPv6 link-local, and every v4-in-v6 form judged as its
     v4): permitted.
   - `block_private_addresses = false`: the legacy operator switch;
     non-public permitted for **operator** runs only, never management.
   - Otherwise refused (`non_public_address`).
6. `pinned_client`:
   - `resolve_to_addrs(host, vetted)`, so there is no second resolution and
     DNS rebinding cannot move the connection.
   - TLS verification and SNI still use the URL's host.
   - `no_proxy()`, so no environment or system proxy is used.
   - `redirect::Policy::none()`.

## 6. Profile credential isolation

| Credential class | Today | Grant |
| --- | --- | --- |
| Operator-managed installation credentials (search key, MCP `auth`, MCP `headers`) | Used by operator runs as before | Untrusted run: only if its profile is in `web.search.credential_profiles` / the server's `credential_profiles` |
| Profile-specific credentials | **Not implemented** (needs a credential store, §10) | — |
| Run-delegated credentials | **Not implemented** | — |
| Public, no credential (`web.fetch`) | No credential attached, ever | Destination policy only |

Implementation: `chat::RemoteAccess::{Operator, Untrusted(profile)}`
derived from the run's `RunTrust` in `serve.rs` `run_as`.

- A profile not granted the search key: the key is **never resolved** into
  its run, and `web.search` refuses, rather than sending the query
  anonymously in its place.
- An HTTP MCP server not granted to the profile is **never contacted**.
- A server granted without its credentials is contacted **without** `auth`
  **and without** custom headers, since headers can carry secrets.
- For an HTTP MCP server the run may use, its installation `auth`:

  | `auth` | The run may use it (an operator run, or a profile in `credential_profiles`) | Result |
  | --- | --- | --- |
  | Not configured | — | Contacted without a bearer; custom headers only if the run may use them |
  | Configured | No | **Never looked up**, so a broken one changes nothing: contacted without `auth` and without custom headers, as above |
  | Configured, cannot be used | Yes | **Not contacted**: no request, so neither a bearer nor its custom headers, and never an unauthenticated connection in their place |
  | Configured, usable | Yes | Contacted with the bearer and the custom headers |

  "Cannot be used" covers an unset variable, an unreadable file, an empty
  value, and a value that cannot be sent as an `Authorization` header. The
  run reports `mcp_auth_unusable` with a fixed category
  (`credential_unresolved`, `credential_unreadable`, `credential_empty`,
  `credential_invalid_header`) and the server's name only: never the value,
  the path or variable, or an error's text. Other servers connect as before.
- That check only withholds credentials; it grants no access. Whether the
  run may use the server at all is decided first, by `untrusted_profiles`,
  and a profile not granted the server never reaches it, nor is told
  anything about its `auth`. A profile's own vault grant is resolved as
  before.
- Untrusted runs never start stdio MCP servers.
- Every MCP tool in an untrusted run requires approval, whatever its
  annotations say.

Revocation (removing a grant) takes effect at the run's next start. The
existing per-call tool guard (#17) still withdraws tools mid-run.

## 7. Egress policy

The typed policy is `egress::EgressRules` per run. It considers:
- profile, via `untrusted_destinations`;
- run trust (`Some`/`None`, which also gates trusted networks and the
  legacy switch);
- tool identity: `web.fetch` and `web.search` are scoped, and MCP servers
  are granted per server;
- scheme, host, every resolved address, and port (part of the pinned
  address and of the origin comparison);
- credential requirement (`Redirects` credentialed mode);
- redirect destination (re-vetted per hop);
- approved network scope (`trusted_networks`).

It only refuses. It grants no tool, and neither Jev nor the model's
arguments can widen it.

## 8. Security tests

| # | Requirement | Test(s) |
| --- | --- | --- |
| 1 | Same-origin redirect, no credential leakage | `a_same_origin_redirect_is_followed_without_credentials` (HTTP); `redirects_are_validated_before_they_are_followed` |
| 2, 3 | Cross-origin redirect with Authorization / custom secret header | `an_http_mcp_redirect_is_refused_and_no_header_follows_it` (real MCP client: the target never sees bearer or header); `redirects_are_validated…` (credentialed) |
| 4 | HTTPS→HTTP downgrade | `redirects_are_validated…` (unit; no local TLS fixture without a new dependency) |
| 5 | Loops | `a_redirect_loop_is_detected` |
| 6 | Redirect into prohibited destinations | `a_redirect_into_a_prohibited_destination_is_refused` (metadata, private, private-by-DNS, `file:`, userinfo; one request only) |
| 7 | Allow-listed host resolving privately | `an_allow_listed_name_resolving_privately_is_refused` (no connection); `names_are_judged_on_every_resolved_address`; replaced `the_guard_vets_literals_and_resolves_allow_listed_hosts` |
| 8 | DNS rebinding | `the_connection_is_pinned_to_the_vetted_answer` (one lookup; a name only the test resolver knows still connects) |
| 9 | IPv4-mapped IPv6 | `every_non_public_and_management_destination_is_refused`, `prohibited_destinations_are_never_connected` |
| 10 | Loopback and link-local | same, plus alternate notations |
| 11 | Cloud metadata (simulated) | same; `metadata.google.internal` via the test resolver |
| 12 | Host management (simulated) | WireServer by literal and by name |
| 13 | Environment proxy bypass | `environment_proxies_are_never_used_for_a_fetch` (child process with proxy variables: the control request hits the proxy, the fetch goes direct) |
| 14, 15 | Unauthorized and authorized profile with shared credentials | `the_search_key_is_used_only_where_granted` (HTTP); `the_web_context_grants_search_credentials_and_destinations_per_profile`; `http_mcp_servers_and_credentials_follow_profile_grants` |
| 16 | User-controlled destination receiving data | `an_untrusted_run_cannot_send_data_to_an_unauthorized_host`; `untrusted_runs_reach_only_their_profiles_destinations` |
| 17 | Read-only vs side-effect MCP approvals | `untrusted_runs_need_approval_for_every_mcp_tool_whatever_its_annotations` (operator unchanged; untrusted always asks) |
| 18 | Cancellation | `cancelling_the_run_stops_a_fetch` (real `BoundedExecutor`) |
| 19 | Timeout | `a_hanging_destination_times_out` |
| 20 | Oversized responses | `an_oversized_response_is_bounded`; existing MCP body cap tests |
| 21 | Invalid configuration | `egress_settings_are_validated_and_old_configs_still_load` |
| 22 | Profile revocation | MCP and web-context tests (grant removed → next run denied) |
| 23 | Single-user compatibility | operator cases in each test; old configuration loads; all existing tests |
| 24 | Lightweight `Auto` contract | No Lightweight or coordinator change; existing Auto-guard and paired E2E checks |

All secrets are synthetic, and all servers are local stubs. Prohibited
destinations are refused before any connection, and the tests assert the
stub saw nothing.

## 9. Compatibility

| Change | Old behavior | Risk | New behavior | When | Affected configurations | Migration | Rollback |
| --- | --- | --- | --- | --- | --- | --- | --- |
| Allow-list is not an address exemption | Listed hosts skipped address checks | C1 | Listed hosts are vetted and pinned | **Immediately** | Allow-lists naming internal services | Add the service's network to `web.trusted_networks` (`10.20.0.0/16`) | Revert (reopens C1) |
| No proxy for `web.fetch` | Environment and system proxies used | C3 | Direct, pinned connection | **Immediately** | Hosts whose only egress is an HTTP proxy | None yet: a policy-enforcing trusted egress proxy is future work. Such a host cannot use `web.fetch` until then | Revert (reopens C3) |
| Management endpoints | Allowed with `block_private_addresses=false`; WireServer always | C4, C5 | Refused unless an exact `/32` trusted entry | **Immediately** | Deliberate metadata access (rare) | Exact entry in `trusted_networks` | Revert |
| Redirect validation | Followed http↔https, loops until the hop limit | Downgrade, privacy | Downgrade and loops refused | **Immediately** | Sites redirecting https→http | None (insecure by nature) | Revert |
| Userinfo URLs | Accepted (sent as Basic auth) | C6 | Refused | **Immediately** | URLs with `user:pass@` | Use `auth` for MCP | Revert |
| HTTP MCP redirects | Followed, headers forwarded | C2 | Refused with a clear message | **Immediately** | MCP URLs that redirect (http→https, trailing slash) | Configure the final URL | Revert (reopens C2) |
| URL-free errors | Full URLs in errors | C6 | Host only | **Immediately** | — | — | — |
| Untrusted runs: scoped destinations, credential grants, MCP grants, approvals, no stdio | Installation-wide everything | C7, C8 | Deny unless granted | Effective only for untrusted runs, which cannot exist yet | None | Grant per profile when multi-user is activated | — |
| Operator credentials | — | — | Unchanged | — | — | — | — |

There is no switch that disables these protections. `block_private_addresses`
remains the existing operator setting, now bounded: operator runs only, and
never management endpoints.

## 10. Remaining limitations

- Profile-scoped and run-delegated credentials need a credential store (§11).
- Revocation applies at the next run, not mid-run, for credential and
  destination grants.
- No trusted egress proxy: proxied-only deployments lose `web.fetch`.
- The search endpoint and MCP URLs are operator-trusted and not
  address-vetted. A future option could vet them for untrusted runs.
- HTTPS→HTTP is covered by unit tests only, because a local TLS fixture
  would need a new dependency.
- No per-profile rate limits.
- No tenant-safe audit events for remote calls.
- MCP servers holding cross-profile state remain the operator's
  responsibility; grant them per profile.
- System DNS: no DNSSEC or DoH. Pinning prevents rebinding, but not a
  poisoned first answer that is itself public.

## 11. Recommended credential-store architecture (future)

1. **Credential records** under the installation, never in config:
   - id;
   - class (`operator`, `profile`, `run`);
   - owner profile;
   - the tools and destination origins it may be sent to;
   - a `SecretRef` (env, owner-only file, or OS keychain);
   - created, rotated and revoked timestamps.
2. **Credential broker.** Every request asks
   `(run, profile, tool, origin) → header?` at send time, so revocation and
   rotation apply mid-run. Redirects never reuse a credential for a new
   origin.
3. **Run-delegated credentials.** Short-lived tokens minted per run and
   scoped to one origin and tool. They expire at run end and are revoked on
   cancellation or ownership change.
4. **Grants.** Only the operator, or the profile owner within operator
   limits (C6 minimum bundle), may bind a credential to a profile. Jev and
   the model never can.
5. **Audit.** Every use is recorded (principal, profile, tool, credential
   id, destination host, outcome), never with the secret.
6. **Migration.** Today's installation-wide keys become `operator` records
   granted to the profiles listed in the new `credential_profiles` keys.

## 12. Rollback

Revert the PR. The new configuration keys are ignored by older builds,
because unknown fields are not rejected, so existing configurations load
either way. Reverting reopens C1–C6 for operator runs. No data or
state migration is involved.
