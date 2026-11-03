# Lightagent Desktop — CLI parity matrix

Frozen at `12582a046ba37a7fd59c86c1d4fb3a911fa7564a` (tree `59b8f136534d228e328ef6795c26f63de6f47faa`). Machine-readable source: [desktop-capabilities.json](desktop-capabilities.json). The CLI command tree is pinned in [cli-tree.txt](cli-tree.txt) and checked against clap by `desktop_inventory` in `crates/lightagent/src/lib.rs`; the other surfaces are checked by `scripts/check-desktop-inventory.py`.

## Coverage

- Denominator: 227
- Implemented: 99 of 227
- Pending D1 decision rows: 0
- D5a note (2026-11-02, rows 21-24): `slash.approve`, `slash.reject`, `slash.stop`, `slash.continue` and `key.steer` are implemented at `e164dde`: browser UI e2e (e2e/runs.mjs), host-level (desktop/e2e/runs.py, switch.py) and backend (desktop_continue.rs) evidence; native WebView interaction is D8 and not claimed. #98's regenerate (`api.post.sessions.id.regenerate`) has host-level R1 evidence and stays not counted until its browser and native checks (R1 is met at host level; R2-R5 are D8)
- D6 note (2026-11-02, rows 26-27): the 13 `cli.extensions.*` rows, the 5 `extension.manifest.*` rows and `extension.roots` are implemented at `c89c989` through `lightagent_extensions::manage`: backend (crates/lightagent/tests/desktop_extensions.rs, CLI and API alike), host-level (desktop/e2e/runs.py `extensions`) and browser UI (e2e/extensions.mjs) evidence; native WebView interaction is D8 and not claimed. `config.extensions` waits for a master-switch control (row 28); the five `api.*.extensions*` rows are backend rows and not counted
- D6 note (2026-11-02, rows 28-31): `mcp.server.stdio` (+`command`, `args`, `env`), `mcp.server.http` (+`url`, `headers`), `config.mcp`, `cli.tools.list` are implemented at `62e87e6` with backend, host and browser evidence; native WebView is D8. `mcp.server.http.auth`, `mcp.server.http.untrusted_profiles` and `mcp.server.http.credential_profiles` are supported by the shared operation and the MCP card's form but not yet exercised by a browser check, so they are not counted. Difference recorded: `lightagent extensions install` turns MCP on for a bundle with MCP servers (and says so); the API and desktop do only when asked, and the desktop's MCP switch asks for confirmation. Skill import (`api.post.skills.imports`) is new scope beyond the CLI and never counted Correction (row 32): `slash.skills` and `skills.roots` were counted at `5c98b3e` without host evidence (runs.py `skills` never called GET /skills); they are not-started again until the row 32 host scenario passes.
- D6 note (2026-11-03, row 32): `slash.skills`, `skills.roots` (host: GET /skills through the host with scope and source), `mcp.server.http.auth`, `mcp.server.http.untrusted_profiles`, `mcp.server.http.credential_profiles` (browser and host: saved and shown by name) and `config.extensions` (the master switch through the shared config.set operation, with per-extension enable/disable) are implemented at `c70c2f9` with backend, host and browser evidence; native WebView is D8. Install activation differs on purpose: `lightagent extensions install` turns MCP on for a bundle with MCP servers and says so; the API and the desktop do so only when asked (`activate_mcp`), and a skills import never does. MCP servers are connected per run: extension and MCP changes apply from the next run, and a run in progress keeps the connections it made.
- D7 note (2026-11-03, rows 34-35): the 24 `cli.memory.*` rows (add, list, search, reflect, candidates, promote, update, forget, clear, with their arguments and options) are implemented at `630988a` through one shared implementation (`memory::perform` via `RunFactory::memory`): backend `desktop_memory.rs` (CLI and API alike), host `runs.py memory` (every operation through the host, matched with the CLI) and browser `e2e/memory.mjs`; native WebView is D8. The nine `api.*.memories*` rows are backend rows and not counted; `config.memory` (auto capture, injected count, result count) has no control yet
- D7 note (2026-11-03, rows 36-37): `cli.rag.add` (+`path`), `cli.rag.search` (+`query`, `--top-k`), `cli.rag.list` and `cli.rag.clear` are implemented at `c65132f` through `rag::perform` via `RunFactory::rag`: backend `desktop_knowledge.rs` and the RagStore two-writer test, host `runs.py knowledge`, browser `e2e/knowledge.mjs`; native WebView is D8. `cli.rag.add.opt.source` (the source name) has a UI field but no test sets it yet, so it is not counted. The RagStore now writes under a lock, atomically and owner-only; before, a CLI and an API add at the same time could drop a document
- Total rows: 361

| Kind | Rows |
|---|---|
| acp-method | 8 |
| alias | 2 |
| api | 52 |
| argument | 47 |
| command | 86 |
| config | 14 |
| env | 12 |
| extension-registration | 6 |
| keybinding | 6 |
| mcp-registration | 10 |
| option | 58 |
| setup-option | 10 |
| skill-registration | 1 |
| slash | 16 |
| value | 6 |

| Applicability | Rows |
|---|---|
| applicable | 298 |
| host-owned | 7 |
| not-applicable | 29 |

## Applicability decisions (resolved in D1)

| Decision | Rows | Question | Resolution |
|---|---|---|---|
| D1-Q1 | `cli.update`, `cli.update.opt.check`, `cli.update.opt.force` | Does the desktop expose CLI self-update (`lightagent update`)? The plan forbids automatic updates of installed apps. | resolved: [docs/desktop/DESKTOP_ARCHITECTURE.md §11](DESKTOP_ARCHITECTURE.md#11-applicability-decisions-d0--resolved) |
| D1-Q2 | `slash.update` | Same as D1-Q1 for the /update slash command. | resolved: [docs/desktop/DESKTOP_ARCHITECTURE.md §11](DESKTOP_ARCHITECTURE.md#11-applicability-decisions-d0--resolved) |
| D1-Q3 | `api.provisioning.put.profiles` | Is feature-gated profile provisioning a desktop capability? | resolved: [docs/desktop/DESKTOP_ARCHITECTURE.md §11](DESKTOP_ARCHITECTURE.md#11-applicability-decisions-d0--resolved) |
| D1-Q4 | `api.provisioning.get.operations` | Provisioning operation status in desktop? | resolved: [docs/desktop/DESKTOP_ARCHITECTURE.md §11](DESKTOP_ARCHITECTURE.md#11-applicability-decisions-d0--resolved) |
| D1-Q5 | `api.provisioning.post.operations` | Provisioning revoke in desktop? | resolved: [docs/desktop/DESKTOP_ARCHITECTURE.md §11](DESKTOP_ARCHITECTURE.md#11-applicability-decisions-d0--resolved) |
| D1-Q6 | `env.LIGHTAGENT_INSTALL_ROOT` | Does the desktop honour or expose the update install root? | resolved: [docs/desktop/DESKTOP_ARCHITECTURE.md §11](DESKTOP_ARCHITECTURE.md#11-applicability-decisions-d0--resolved) |

## Implemented rows and evidence

| ID | Tested SHA | Verification | Actions |
|---|---|---|---|
| `cli.profile.list` | `cd799a4` | Profiles screen list (UI e2e: e2e/profiles.mjs in the standalone Web UI render job); API = CLI (crates/lightagent/tests/desktop_profiles.rs (all four platforms)) | [check](https://github.com/dlroqa/Lightagent/actions/runs/41057799814), [desktop](https://github.com/dlroqa/Lightagent/actions/runs/41057799963) |
| `cli.profile.show` | `cd799a4` | Details card (UI e2e); API = CLI (crates/lightagent/tests/desktop_profiles.rs (all four platforms)) | [check](https://github.com/dlroqa/Lightagent/actions/runs/41057799814), [desktop](https://github.com/dlroqa/Lightagent/actions/runs/41057799963) |
| `cli.profile.show.arg.id` | `cd799a4` | Details card (UI e2e); API = CLI (crates/lightagent/tests/desktop_profiles.rs (all four platforms)) | [check](https://github.com/dlroqa/Lightagent/actions/runs/41057799814), [desktop](https://github.com/dlroqa/Lightagent/actions/runs/41057799963) |
| `cli.profile.create` | `cd799a4` | Create form with id rule and errors (UI e2e); API = CLI (crates/lightagent/tests/desktop_profiles.rs (all four platforms)) | [check](https://github.com/dlroqa/Lightagent/actions/runs/41057799814), [desktop](https://github.com/dlroqa/Lightagent/actions/runs/41057799963) |
| `cli.profile.create.arg.id` | `cd799a4` | Create form with id rule and errors (UI e2e); API = CLI (crates/lightagent/tests/desktop_profiles.rs (all four platforms)) | [check](https://github.com/dlroqa/Lightagent/actions/runs/41057799814), [desktop](https://github.com/dlroqa/Lightagent/actions/runs/41057799963) |
| `cli.profile.create.opt.name` | `cd799a4` | Create form with id rule and errors (UI e2e); API = CLI (crates/lightagent/tests/desktop_profiles.rs (all four platforms)) | [check](https://github.com/dlroqa/Lightagent/actions/runs/41057799814), [desktop](https://github.com/dlroqa/Lightagent/actions/runs/41057799963) |
| `cli.profile.create.opt.persona` | `cd799a4` | Create form with id rule and errors (UI e2e); API = CLI (crates/lightagent/tests/desktop_profiles.rs (all four platforms)) | [check](https://github.com/dlroqa/Lightagent/actions/runs/41057799814), [desktop](https://github.com/dlroqa/Lightagent/actions/runs/41057799963) |
| `cli.profile.create.opt.model` | `cd799a4` | Create form with id rule and errors (UI e2e); API = CLI (crates/lightagent/tests/desktop_profiles.rs (all four platforms)) | [check](https://github.com/dlroqa/Lightagent/actions/runs/41057799814), [desktop](https://github.com/dlroqa/Lightagent/actions/runs/41057799963) |
| `cli.profile.use` | `cd799a4` | Browser: Use sets the CLI's active profile (UI e2e; API = CLI, crates/lightagent/tests/desktop_profiles.rs (all four platforms)). Desktop: runtime switch (switch.rs) unit-tested and compiled on four targets; native end-to-end automation pending D8 (not claimed) | [check](https://github.com/dlroqa/Lightagent/actions/runs/41057799814), [desktop](https://github.com/dlroqa/Lightagent/actions/runs/41057799963) |
| `cli.profile.use.arg.id` | `cd799a4` | Browser: Use sets the CLI's active profile (UI e2e; API = CLI, crates/lightagent/tests/desktop_profiles.rs (all four platforms)). Desktop: runtime switch (switch.rs) unit-tested and compiled on four targets; native end-to-end automation pending D8 (not claimed) | [check](https://github.com/dlroqa/Lightagent/actions/runs/41057799814), [desktop](https://github.com/dlroqa/Lightagent/actions/runs/41057799963) |
| `cli.profile.preset` | `cd799a4` | Preset form (UI e2e); API = CLI (crates/lightagent/tests/desktop_profiles.rs (all four platforms)) | [check](https://github.com/dlroqa/Lightagent/actions/runs/41057799814), [desktop](https://github.com/dlroqa/Lightagent/actions/runs/41057799963) |
| `cli.profile.preset.arg.id` | `cd799a4` | Preset form (UI e2e); API = CLI (crates/lightagent/tests/desktop_profiles.rs (all four platforms)) | [check](https://github.com/dlroqa/Lightagent/actions/runs/41057799814), [desktop](https://github.com/dlroqa/Lightagent/actions/runs/41057799963) |
| `cli.profile.preset.arg.preset` | `cd799a4` | Preset form (UI e2e); API = CLI (crates/lightagent/tests/desktop_profiles.rs (all four platforms)) | [check](https://github.com/dlroqa/Lightagent/actions/runs/41057799814), [desktop](https://github.com/dlroqa/Lightagent/actions/runs/41057799963) |
| `cli.profile.delete` | `cd799a4` | Typed-confirmation delete, default protected (UI e2e); API = CLI (crates/lightagent/tests/desktop_profiles.rs (all four platforms)) | [check](https://github.com/dlroqa/Lightagent/actions/runs/41057799814), [desktop](https://github.com/dlroqa/Lightagent/actions/runs/41057799963) |
| `cli.profile.delete.arg.id` | `cd799a4` | Typed-confirmation delete, default protected (UI e2e); API = CLI (crates/lightagent/tests/desktop_profiles.rs (all four platforms)) | [check](https://github.com/dlroqa/Lightagent/actions/runs/41057799814), [desktop](https://github.com/dlroqa/Lightagent/actions/runs/41057799963) |
| `cli.config.show` | `0e46892` | Advanced configuration card shows the redacted config (UI e2e: e2e/config.mjs in the standalone Web UI render job); API = CLI (crates/lightagent/tests/desktop_config.rs, all four platforms); desktop: contract operations config.show/get/set; native WebView automation pending D8 (not claimed) | [check](https://github.com/dlroqa/Lightagent/actions/runs/41359276745), [desktop](https://github.com/dlroqa/Lightagent/actions/runs/41359276774) |
| `cli.config.path` | `0e46892` | Advanced configuration card shows the config path (UI e2e); GET /config path = `config path` (API = CLI (crates/lightagent/tests/desktop_config.rs, all four platforms)); desktop: contract operations config.show/get/set; native WebView automation pending D8 (not claimed) | [check](https://github.com/dlroqa/Lightagent/actions/runs/41359276745), [desktop](https://github.com/dlroqa/Lightagent/actions/runs/41359276774) |
| `cli.config.get` | `0e46892` | Advanced configuration card Read (UI e2e); API = CLI (crates/lightagent/tests/desktop_config.rs, all four platforms), including the unknown-key error; desktop: contract operations config.show/get/set; native WebView automation pending D8 (not claimed) | [check](https://github.com/dlroqa/Lightagent/actions/runs/41359276745), [desktop](https://github.com/dlroqa/Lightagent/actions/runs/41359276774) |
| `cli.config.get.arg.key` | `0e46892` | Advanced configuration card Key field (UI e2e); API = CLI (crates/lightagent/tests/desktop_config.rs, all four platforms); desktop: contract operations config.show/get/set; native WebView automation pending D8 (not claimed) | [check](https://github.com/dlroqa/Lightagent/actions/runs/41359276745), [desktop](https://github.com/dlroqa/Lightagent/actions/runs/41359276774) |
| `cli.config.set` | `0e46892` | Advanced configuration card Set: write, stale 409 with nothing written, invalid value refused (UI e2e); API = CLI (crates/lightagent/tests/desktop_config.rs, all four platforms); one cross-process lock (desktop_config.rs, core config tests); desktop: contract operations config.show/get/set; native WebView automation pending D8 (not claimed) | [check](https://github.com/dlroqa/Lightagent/actions/runs/41359276745), [desktop](https://github.com/dlroqa/Lightagent/actions/runs/41359276774) |
| `cli.config.set.arg.key` | `0e46892` | Advanced configuration card Key field (UI e2e); API = CLI (crates/lightagent/tests/desktop_config.rs, all four platforms); desktop: contract operations config.show/get/set; native WebView automation pending D8 (not claimed) | [check](https://github.com/dlroqa/Lightagent/actions/runs/41359276745), [desktop](https://github.com/dlroqa/Lightagent/actions/runs/41359276774) |
| `cli.config.set.arg.value` | `0e46892` | Advanced configuration card Value field, invalid value refused with the CLI's words (UI e2e; API = CLI (crates/lightagent/tests/desktop_config.rs, all four platforms)); desktop: contract operations config.show/get/set; native WebView automation pending D8 (not claimed) | [check](https://github.com/dlroqa/Lightagent/actions/runs/41359276745), [desktop](https://github.com/dlroqa/Lightagent/actions/runs/41359276774) |
| `cli.config.jev-routing-report` | `205f01b` | Settings dry-run table under the routing permission lists the server's report (UI e2e: e2e/settings.mjs); API = CLI `--json` report (crates/lightagent/tests/desktop_config.rs, all four platforms); desktop: contract operation jev.routing_report; native WebView automation pending D8 (not claimed) | [check](https://github.com/dlroqa/Lightagent/actions/runs/41370767535), [desktop](https://github.com/dlroqa/Lightagent/actions/runs/41370767531) |
| `setup.provider.key` | `6988f6c` | Settings Provider API key card: save, stale save refused with the entry kept, replace, remove after confirmation, key gone from the page (UI e2e, browser only); API = CLI, write-only, unusable keys fail closed (crates/lightagent/tests/desktop_credentials.rs, all four platforms); desktop: contract operations provider.credential.*; native WebView automation pending D8 (not claimed) | [check](https://github.com/dlroqa/Lightagent/actions/runs/41378487660), [desktop](https://github.com/dlroqa/Lightagent/actions/runs/41378487692) |
| `cli.models` | `e6f4506` | Settings Check connection, only when asked (UI e2e, browser only); API = CLI models through one bounded check: key to that endpoint only, redirect reported not followed, silent endpoint cut off, unusable key fails closed (crates/lightagent/tests/desktop_providers.rs, all four platforms); desktop: contract operation provider.check; native WebView automation pending D8 (not claimed) | [check](https://github.com/dlroqa/Lightagent/actions/runs/41380804834), [desktop](https://github.com/dlroqa/Lightagent/actions/runs/41380804835) |

| `setup.provider.saved` | `bb76e74` | Settings Providers and models card: add, stale save refused with the entry kept, rename, remove after confirmation with a stale removal refused (UI e2e, browser only); one shared operation set for setup and the API: duplicates refused, rename keeps the key, a new endpoint releases it unless keep_key, removal releases it (saved_providers unit tests; crates/lightagent/tests/desktop_saved_providers.rs at 108dac6, all four platforms); desktop: contract operations providers.*; native WebView automation pending D8 (not claimed) | [check](https://github.com/dlroqa/Lightagent/actions/runs/41397533047), [desktop](https://github.com/dlroqa/Lightagent/actions/runs/41397532957) |
| `setup.provider.url` | `bb76e74` | Settings Use selects a provider, the key card follows the new endpoint (UI e2e, browser only); the endpoint takes only its own key, never the previous one (crates/lightagent/tests/desktop_key_binding.rs at 8e3919b and desktop_saved_providers.rs at 108dac6, all four platforms); per-provider bounded check; desktop: contract operations provider.active.set, providers.check; native WebView automation pending D8 (not claimed) | [check](https://github.com/dlroqa/Lightagent/actions/runs/41397533047), [desktop](https://github.com/dlroqa/Lightagent/actions/runs/41397532957) |
| `setup.provider.model` | `bb76e74` | Settings model field and List models (UI e2e, browser only); API = shared select_model with the active profile following, ordered concurrency against a CLI write (desktop_saved_providers.rs at 108dac6, all four platforms); desktop: contract operation provider.model.set; native WebView automation pending D8 (not claimed) | [check](https://github.com/dlroqa/Lightagent/actions/runs/41397533047), [desktop](https://github.com/dlroqa/Lightagent/actions/runs/41397532957) |

| `setup.tools.workspace` | `ec1fc4e` | Settings Workspace card: stale save refused with the entry kept, relative folder refused, absolute folder saved, cleared to the profile's own folder (UI e2e, browser only); one shared operation for setup and the API: absolute, an existing folder or one inside an existing folder, revision-checked, effective folder follows the active profile (workspaces unit tests, setup line-mode tests, crates/lightagent/tests/desktop_workspace.rs, all four platforms); desktop: contract operations workspace.get/set; a native folder picker and native WebView automation pending D8 (not claimed) | [check](https://github.com/dlroqa/Lightagent/actions/runs/41402353290), [desktop](https://github.com/dlroqa/Lightagent/actions/runs/41402353271) |

## What the drift check cannot detect automatically

- Setup menu items inside a section (`setup.*` rows) — reviewed by hand when `crates/lightagent/src/setup.rs` changes.
- New modifier combinations or changed meaning of an existing key in `chat.rs` (new `KeyCode` variants are detected).
- Behaviour changes behind an unchanged command, flag, route, or field name.
- Config keys below the section level (sections are detected; keys are covered by the settings forms in D4).
- Environment variables read through a computed name (literal names are detected).

## command

| ID | Surface | Applicability | Counts | UI path | Backend | Status | Source |
|---|---|---|---|---|---|---|---|
| `cli` | `lightagent` | not-applicable | no | Container command; the default action is chat (cli.chat) |  | not-started | `crates/lightagent/src/lib.rs:53` |
| `cli.chat` | `lightagent chat` | applicable | yes | Conversations | api: POST /runs, GET /runs/{id}/events | not-started | `crates/lightagent/src/lib.rs:64` |
| `cli.init` | `lightagent init` | applicable | yes | First-run wizard | extract: lib.rs init() | not-started | `crates/lightagent/src/lib.rs:73` |
| `cli.setup` | `lightagent setup` | applicable | no | Settings (sections) | extract: setup.rs run_with_io() section operations | not-started | `crates/lightagent/src/lib.rs:88; crates/lightagent/src/setup.rs:15` |
| `cli.config` | `lightagent config` | applicable | dup→`cli.config.show` | Settings/Diagnostics | extract: lib.rs config_cmd() | not-started | `crates/lightagent/src/lib.rs:94` |
| `cli.config.show` | `lightagent config show` | applicable | yes | Settings/Diagnostics: effective configuration | api: GET /settings (subset); extract: config_cmd(Show) | implemented | `crates/lightagent/src/lib.rs:195` |
| `cli.config.path` | `lightagent config path` | applicable | yes | Settings/Diagnostics | extract: config_cmd(Path) | implemented | `crates/lightagent/src/lib.rs:197` |
| `cli.config.get` | `lightagent config get` | applicable | yes | Settings: advanced key editor (typed) | extract: config_cmd(Get) | implemented | `crates/lightagent/src/lib.rs:199` |
| `cli.config.set` | `lightagent config set` | applicable | yes | Settings: advanced key editor (typed, validated) | api: PUT /settings (subset); extract: config_cmd(Set) | implemented | `crates/lightagent/src/lib.rs:201` |
| `cli.config.jev-routing-report` | `lightagent config jev-routing-report` | applicable | yes | Settings/Diagnostics: Jev routing report | shared: lightagent_core::config_keys::jev_routing_report; api: GET /jev/routing-report | implemented | `crates/lightagent/src/lib.rs:203` |
| `cli.models` | `lightagent models` | applicable | yes | Providers/Models | shared: providers::check_active; api: POST /provider/check | implemented | `crates/lightagent/src/lib.rs:99` |
| `cli.tools` | `lightagent tools` | applicable | dup→`cli.tools.list` | Tools/MCP/Extensions | api: GET /tools | not-started | `crates/lightagent/src/lib.rs:101` |
| `cli.tools.list` | `lightagent tools list` | applicable | yes | Tools/MCP/Extensions: tool catalog | api: GET /tools | implemented | `crates/lightagent/src/lib.rs:211` |
| `cli.profile` | `lightagent profile` | applicable | dup→`cli.profile.list` | Profiles/Workspaces | extract: lib.rs profiles_cmd() | not-started | `crates/lightagent/src/lib.rs:106` |
| `cli.profile.list` | `lightagent profile list` | applicable | yes | Profiles/Workspaces | ops::Profiles::list; api: GET /profiles | implemented | `crates/lightagent/src/lib.rs:248` |
| `cli.profile.show` | `lightagent profile show` | applicable | yes | Profiles/Workspaces: detail | ops::Profiles::show; api: GET /profiles/{id} | implemented | `crates/lightagent/src/lib.rs:250` |
| `cli.profile.create` | `lightagent profile create` | applicable | yes | Profiles/Workspaces: create form | ops::Profiles::create; api: POST /profiles | implemented | `crates/lightagent/src/lib.rs:253` |
| `cli.profile.use` | `lightagent profile use` | applicable | yes | Profiles/Workspaces: switch (see D1 profile-switch protocol) | ops::Profiles::use_profile; api: PUT /profiles/{id}/active (+ host sidecar switch, D4b) | implemented | `crates/lightagent/src/lib.rs:263` |
| `cli.profile.preset` | `lightagent profile preset` | applicable | yes | Profiles/Workspaces: preset (admin) | ops::Profiles::preset; api: POST /profiles/{id}/preset | implemented | `crates/lightagent/src/lib.rs:265` |
| `cli.profile.delete` | `lightagent profile delete` | applicable | yes | Profiles/Workspaces: delete (typed confirmation) | ops::Profiles::delete; api: DELETE /profiles/{id} | implemented | `crates/lightagent/src/lib.rs:267` |
| `cli.extensions` | `lightagent extensions` | applicable | dup→`cli.extensions.list` | Tools/MCP/Extensions | extract: extensions.rs | not-started | `crates/lightagent/src/lib.rs:112` |
| `cli.extensions.list` | `lightagent extensions list` | applicable | yes | Tools/MCP/Extensions: extensions | extract: extensions::list() | implemented | `crates/lightagent/src/lib.rs:273` |
| `cli.extensions.show` | `lightagent extensions show` | applicable | yes | Tools/MCP/Extensions: detail | extract: extensions::show() | implemented | `crates/lightagent/src/lib.rs:275` |
| `cli.extensions.enable` | `lightagent extensions enable` | applicable | yes | Tools/MCP/Extensions: toggle | extract: extensions::enable() | implemented | `crates/lightagent/src/lib.rs:277` |
| `cli.extensions.disable` | `lightagent extensions disable` | applicable | yes | Tools/MCP/Extensions: toggle | extract: extensions::disable() | implemented | `crates/lightagent/src/lib.rs:279` |
| `cli.extensions.install` | `lightagent extensions install` | applicable | yes | Tools/MCP/Extensions: install (folder picker or HTTPS URL) | extract: extensions::install() | implemented | `crates/lightagent/src/lib.rs:281` |
| `cli.extensions.uninstall` | `lightagent extensions uninstall` | applicable | yes | Tools/MCP/Extensions: uninstall (confirm) | extract: extensions::uninstall() | implemented | `crates/lightagent/src/lib.rs:288` |
| `cli.sessions` | `lightagent sessions` | applicable | dup→`cli.sessions.list` | Conversations | api: GET /sessions | not-started | `crates/lightagent/src/lib.rs:117` |
| `cli.sessions.list` | `lightagent sessions list` | applicable | yes | Conversations: sidebar | api: GET /sessions | not-started | `crates/lightagent/src/lib.rs:379` |
| `cli.sessions.show` | `lightagent sessions show` | applicable | yes | Conversations: open session | api: GET /sessions/{id} | not-started | `crates/lightagent/src/lib.rs:381` |
| `cli.sessions.delete` | `lightagent sessions delete` | applicable | yes | Conversations: delete (confirm) | api: DELETE /sessions/{id} | not-started | `crates/lightagent/src/lib.rs:383` |
| `cli.workers` | `lightagent workers` | applicable | dup→`cli.workers.status` | Runs/Agents | api: GET /workers | not-started | `crates/lightagent/src/lib.rs:122` |
| `cli.workers.status` | `lightagent workers status` | applicable | yes | Runs/Agents: roles | api: GET /workers | not-started | `crates/lightagent/src/lib.rs:217` |
| `cli.workers.assign` | `lightagent workers assign` | applicable | yes | Runs/Agents: assign role | extract: workers_cmd(Assign) | not-started | `crates/lightagent/src/lib.rs:223` |
| `cli.workers.revoke` | `lightagent workers revoke` | applicable | yes | Runs/Agents: revoke role | extract: workers_cmd(Revoke) | not-started | `crates/lightagent/src/lib.rs:237` |
| `cli.serve` | `lightagent serve` | host-owned | no | (desktop runtime lifecycle; remote mode endpoint settings) | host: sidecar launch | not-started | `crates/lightagent/src/lib.rs:127; crates/lightagent/src/serve.rs:732` |
| `cli.identity` | `lightagent identity` | applicable | no | Settings/Identity (admin) | extract: identity_admin::run() | not-started | `crates/lightagent/src/lib.rs:143; crates/lightagent/src/identity_admin.rs:32` |
| `cli.identity.status` | `lightagent identity status` | applicable | yes | Settings/Identity | extract: identity_admin | not-started | `crates/lightagent/src/identity_admin.rs:34` |
| `cli.identity.user` | `lightagent identity user` | applicable | no | Settings/Identity: users | extract: identity_admin | not-started | `crates/lightagent/src/identity_admin.rs:36` |
| `cli.identity.user.create` | `lightagent identity user create` | applicable | yes | Settings/Identity: users | extract: identity_admin | not-started | `crates/lightagent/src/identity_admin.rs:66` |
| `cli.identity.user.list` | `lightagent identity user list` | applicable | yes | Settings/Identity: users | extract: identity_admin | not-started | `crates/lightagent/src/identity_admin.rs:68` |
| `cli.identity.user.suspend` | `lightagent identity user suspend` | applicable | yes | Settings/Identity: users (confirm) | extract: identity_admin | not-started | `crates/lightagent/src/identity_admin.rs:71` |
| `cli.identity.credential` | `lightagent identity credential` | applicable | no | Settings/Identity: credentials | extract: identity_admin | not-started | `crates/lightagent/src/identity_admin.rs:41` |
| `cli.identity.credential.issue` | `lightagent identity credential issue` | applicable | yes | Settings/Identity: issue → native save dialog for --secret-file | extract: identity_admin (secret goes to an owner-only file; never into renderer state) | not-started | `crates/lightagent/src/identity_admin.rs:77` |
| `cli.identity.credential.rotate` | `lightagent identity credential rotate` | applicable | yes | Settings/Identity: rotate | extract: identity_admin | not-started | `crates/lightagent/src/identity_admin.rs:97` |
| `cli.identity.credential.revoke` | `lightagent identity credential revoke` | applicable | yes | Settings/Identity: revoke (confirm) | extract: identity_admin | not-started | `crates/lightagent/src/identity_admin.rs:105` |
| `cli.identity.credential.list` | `lightagent identity credential list` | applicable | yes | Settings/Identity: credentials | extract: identity_admin | not-started | `crates/lightagent/src/identity_admin.rs:107` |
| `cli.identity.owner` | `lightagent identity owner` | applicable | no | Settings/Identity: ownership | extract: identity_admin | not-started | `crates/lightagent/src/identity_admin.rs:46` |
| `cli.identity.owner.assign` | `lightagent identity owner assign` | applicable | yes | Settings/Identity: ownership | extract: identity_admin | not-started | `crates/lightagent/src/identity_admin.rs:116` |
| `cli.identity.owner.reassign` | `lightagent identity owner reassign` | applicable | yes | Settings/Identity: ownership (confirm) | extract: identity_admin | not-started | `crates/lightagent/src/identity_admin.rs:118` |
| `cli.identity.owner.remove` | `lightagent identity owner remove` | applicable | yes | Settings/Identity: ownership (confirm) | extract: identity_admin | not-started | `crates/lightagent/src/identity_admin.rs:120` |
| `cli.identity.grants` | `lightagent identity grants` | applicable | dup→`cli.identity.grants.list` | Settings/Identity: grants | extract: identity_admin | not-started | `crates/lightagent/src/identity_admin.rs:51` |
| `cli.identity.grants.list` | `lightagent identity grants list` | applicable | yes | Settings/Identity: grants | extract: identity_admin | not-started | `crates/lightagent/src/identity_admin.rs:126` |
| `cli.identity.grants.revoke` | `lightagent identity grants revoke` | applicable | yes | Settings/Identity: grants (confirm) | extract: identity_admin | not-started | `crates/lightagent/src/identity_admin.rs:131` |
| `cli.identity.audit` | `lightagent identity audit` | applicable | yes | Settings/Identity: audit | extract: identity_admin | not-started | `crates/lightagent/src/identity_admin.rs:56` |
| `cli.remote-credential` | `lightagent remote-credential` | applicable | no | Settings/Credentials (admin) | extract: remote_credentials::run() | not-started | `crates/lightagent/src/lib.rs:149; crates/lightagent/src/remote_credentials.rs:26` |
| `cli.remote-credential.status` | `lightagent remote-credential status` | applicable | yes | Settings/Credentials | extract: remote_credentials | not-started | `crates/lightagent/src/remote_credentials.rs:29` |
| `cli.remote-credential.keygen` | `lightagent remote-credential keygen` | applicable | yes | Settings/Credentials: keygen (native save dialog) | extract: remote_credentials | not-started | `crates/lightagent/src/remote_credentials.rs:31` |
| `cli.remote-credential.add` | `lightagent remote-credential add` | applicable | yes | Settings/Credentials: add (secret via native file picker; never typed into the renderer) | extract: remote_credentials | not-started | `crates/lightagent/src/remote_credentials.rs:33` |
| `cli.remote-credential.rotate` | `lightagent remote-credential rotate` | applicable | yes | Settings/Credentials: rotate | extract: remote_credentials | not-started | `crates/lightagent/src/remote_credentials.rs:53` |
| `cli.remote-credential.revoke` | `lightagent remote-credential revoke` | applicable | yes | Settings/Credentials: revoke (confirm) | extract: remote_credentials | not-started | `crates/lightagent/src/remote_credentials.rs:59` |
| `cli.remote-credential.list` | `lightagent remote-credential list` | applicable | yes | Settings/Credentials | extract: remote_credentials | not-started | `crates/lightagent/src/remote_credentials.rs:61` |
| `cli.remote-credential.audit` | `lightagent remote-credential audit` | applicable | yes | Settings/Credentials: audit | extract: remote_credentials | not-started | `crates/lightagent/src/remote_credentials.rs:66` |
| `cli.remote-credential.rekey` | `lightagent remote-credential rekey` | applicable | yes | Settings/Credentials: rekey (native file picker) | extract: remote_credentials | not-started | `crates/lightagent/src/remote_credentials.rs:72` |
| `cli.acp` | `lightagent acp` | not-applicable | no | Editor transport over stdio; its operations duplicate Conversations rows (see acp.* rows). Desktop documents how to launch it. |  | not-started | `crates/lightagent/src/lib.rs:154` |
| `cli.architecture` | `lightagent architecture` | applicable | yes | Settings/Diagnostics: architecture | extract: lib.rs architecture() | not-started | `crates/lightagent/src/lib.rs:156` |
| `cli.doctor` | `lightagent doctor` | applicable | yes | Settings/Diagnostics: doctor | extract: lib.rs doctor(); api: GET /health (partial) | not-started | `crates/lightagent/src/lib.rs:159` |
| `cli.import` | `lightagent import` | applicable | no | Profiles/Workspaces: import |  | not-started | `crates/lightagent/src/lib.rs:161` |
| `cli.import.hermes` | `lightagent import hermes` | applicable | yes | Profiles/Workspaces: import from Hermes (preview then apply) | extract: import::hermes() | not-started | `crates/lightagent/src/lib.rs:363; crates/lightagent/src/import.rs:28` |
| `cli.rag` | `lightagent rag` | applicable | no | Memory/Knowledge: documents | extract: rag.rs | not-started | `crates/lightagent/src/lib.rs:166` |
| `cli.rag.add` | `lightagent rag add` | applicable | yes | Memory/Knowledge: add documents (picker) | extract: rag::add() | implemented | `crates/lightagent/src/lib.rs:342` |
| `cli.rag.search` | `lightagent rag search` | applicable | yes | Memory/Knowledge: search | extract: rag::search() | implemented | `crates/lightagent/src/lib.rs:349` |
| `cli.rag.list` | `lightagent rag list` | applicable | yes | Memory/Knowledge: documents | extract: rag::list() | implemented | `crates/lightagent/src/lib.rs:355` |
| `cli.rag.clear` | `lightagent rag clear` | applicable | yes | Memory/Knowledge: clear (confirm) | extract: rag::clear() | implemented | `crates/lightagent/src/lib.rs:357` |
| `cli.memory` | `lightagent memory` | applicable | no | Memory/Knowledge: memories | extract: memory.rs | not-started | `crates/lightagent/src/lib.rs:171` |
| `cli.memory.add` | `lightagent memory add` | applicable | yes | Memory/Knowledge: add | extract: memory::add() | implemented | `crates/lightagent/src/lib.rs:299` |
| `cli.memory.list` | `lightagent memory list` | applicable | yes | Memory/Knowledge: memories | extract: memory::list() | implemented | `crates/lightagent/src/lib.rs:307` |
| `cli.memory.reflect` | `lightagent memory reflect` | applicable | yes | Memory/Knowledge: reflect | extract: memory::reflect() | implemented | `crates/lightagent/src/lib.rs:309` |
| `cli.memory.search` | `lightagent memory search` | applicable | yes | Memory/Knowledge: search | extract: memory::search() | implemented | `crates/lightagent/src/lib.rs:311` |
| `cli.memory.candidates` | `lightagent memory candidates` | applicable | yes | Conversations: session → memory candidates | extract: memory::candidates() | implemented | `crates/lightagent/src/lib.rs:317` |
| `cli.memory.promote` | `lightagent memory promote` | applicable | yes | Conversations: message → remember | extract: memory::promote() | implemented | `crates/lightagent/src/lib.rs:319` |
| `cli.memory.update` | `lightagent memory update` | applicable | yes | Memory/Knowledge: edit | extract: memory::update() | implemented | `crates/lightagent/src/lib.rs:332` |
| `cli.memory.forget` | `lightagent memory forget` | applicable | yes | Memory/Knowledge: forget (confirm) | extract: memory::forget() | implemented | `crates/lightagent/src/lib.rs:334` |
| `cli.memory.clear` | `lightagent memory clear` | applicable | yes | Memory/Knowledge: clear (typed confirmation) | extract: memory::clear() | implemented | `crates/lightagent/src/lib.rs:336` |
| `cli.banner` | `lightagent banner` | not-applicable | no | Terminal cosmetic |  | not-started | `crates/lightagent/src/lib.rs:176` |
| `cli.update` | `lightagent update` | not-applicable (D1-Q1) | no | (decision D1-Q1) | release_update::run() | not-started | `crates/lightagent/src/lib.rs:182` |

## option

| ID | Surface | Applicability | Counts | UI path | Backend | Status | Source |
|---|---|---|---|---|---|---|---|
| `cli.opt.json` | `lightagent --json` | not-applicable | no | Desktop consumes typed results directly; JSON is the CLI's structured transport |  | not-started | `crates/lightagent/src/lib.rs:53` |
| `cli.chat.opt.profile` | `lightagent chat --profile` | applicable | yes | Conversations: profile selector | api: POST /runs {profile} (honoured only in multi-user mode today; see D1 profile decision) | not-started | `crates/lightagent/src/lib.rs:64` |
| `cli.chat.opt.session` | `lightagent chat --session` | applicable | yes | Conversations: session list → open | api: POST /runs {session_id} | not-started | `crates/lightagent/src/lib.rs:64` |
| `cli.init.opt.force` | `lightagent init --force` | applicable | yes | First-run wizard: reconfigure (confirm) | extract: lib.rs init() | not-started | `crates/lightagent/src/lib.rs:73` |
| `cli.init.opt.profile` | `lightagent init --profile` | applicable | yes | First-run wizard — input 'profile' | extract: lib.rs init() | not-started | `crates/lightagent/src/lib.rs:73` |
| `cli.init.opt.base-url` | `lightagent init --base-url` | applicable | yes | First-run wizard — input 'base-url' | extract: lib.rs init() | not-started | `crates/lightagent/src/lib.rs:73` |
| `cli.init.opt.model` | `lightagent init --model` | applicable | yes | First-run wizard — input 'model' | extract: lib.rs init() | not-started | `crates/lightagent/src/lib.rs:73` |
| `cli.profile.create.opt.name` | `lightagent profile create --name` | applicable | yes | Profiles/Workspaces: create form — input 'name' | ops::Profiles::create; api: POST /profiles | implemented | `crates/lightagent/src/lib.rs:253` |
| `cli.profile.create.opt.persona` | `lightagent profile create --persona` | applicable | yes | Profiles/Workspaces: create form — input 'persona' | ops::Profiles::create; api: POST /profiles | implemented | `crates/lightagent/src/lib.rs:253` |
| `cli.profile.create.opt.model` | `lightagent profile create --model` | applicable | yes | Profiles/Workspaces: create form — input 'model' | ops::Profiles::create; api: POST /profiles | implemented | `crates/lightagent/src/lib.rs:253` |
| `cli.extensions.install.opt.profile` | `lightagent extensions install --profile` | applicable | yes | Tools/MCP/Extensions: install (folder picker or HTTPS URL) — input 'profile' | extract: extensions::install() | implemented | `crates/lightagent/src/lib.rs:281` |
| `cli.extensions.uninstall.opt.profile` | `lightagent extensions uninstall --profile` | applicable | yes | Tools/MCP/Extensions: uninstall (confirm) — input 'profile' | extract: extensions::uninstall() | implemented | `crates/lightagent/src/lib.rs:288` |
| `cli.workers.status.opt.profile` | `lightagent workers status --profile` | applicable | yes | Runs/Agents: roles — input 'profile' | api: GET /workers | not-started | `crates/lightagent/src/lib.rs:217` |
| `cli.workers.assign.opt.worker` | `lightagent workers assign --worker` | applicable | yes | Runs/Agents: assign role — input 'worker' | extract: workers_cmd(Assign) | not-started | `crates/lightagent/src/lib.rs:223` |
| `cli.workers.assign.opt.builtin` | `lightagent workers assign --builtin` | applicable | yes | Runs/Agents: assign role — input 'builtin' | extract: workers_cmd(Assign) | not-started | `crates/lightagent/src/lib.rs:223` |
| `cli.workers.assign.opt.profile` | `lightagent workers assign --profile` | applicable | yes | Runs/Agents: assign role — input 'profile' | extract: workers_cmd(Assign) | not-started | `crates/lightagent/src/lib.rs:223` |
| `cli.workers.revoke.opt.profile` | `lightagent workers revoke --profile` | applicable | yes | Runs/Agents: revoke role — input 'profile' | extract: workers_cmd(Revoke) | not-started | `crates/lightagent/src/lib.rs:237` |
| `cli.serve.opt.host` | `lightagent serve --host` | host-owned | no | (desktop runtime lifecycle; remote mode endpoint settings) — input 'host' | host: sidecar launch | not-started | `crates/lightagent/src/lib.rs:127; crates/lightagent/src/serve.rs:732` |
| `cli.serve.opt.port` | `lightagent serve --port` | host-owned | no | (desktop runtime lifecycle; remote mode endpoint settings) — input 'port' | host: sidecar launch | not-started | `crates/lightagent/src/lib.rs:127; crates/lightagent/src/serve.rs:732` |
| `cli.serve.opt.key-env` | `lightagent serve --key-env` | host-owned | no | (desktop runtime lifecycle; remote mode endpoint settings) — input 'key-env' | host: sidecar launch | not-started | `crates/lightagent/src/lib.rs:127; crates/lightagent/src/serve.rs:732` |
| `cli.serve.opt.web-root` | `lightagent serve --web-root` | host-owned | no | (desktop runtime lifecycle; remote mode endpoint settings) — input 'web-root' | host: sidecar launch | not-started | `crates/lightagent/src/lib.rs:127; crates/lightagent/src/serve.rs:732` |
| `cli.serve.opt.desktop` | `lightagent serve --desktop` | host-owned | no | (desktop runtime lifecycle; remote mode endpoint settings) — input 'desktop' | host: sidecar launch | not-started | `crates/lightagent/src/serve.rs (run_desktop)` |
| `cli.serve.opt.profile` | `lightagent serve --profile` | host-owned | no | (desktop runtime lifecycle; remote mode endpoint settings) — input 'profile' | host: sidecar launch | not-started | `crates/lightagent/src/lib.rs:127; crates/lightagent/src/serve.rs:732` |
| `cli.identity.credential.issue.opt.expires-in` | `lightagent identity credential issue --expires-in` | applicable | yes | Settings/Identity: issue → native save dialog for --secret-file — input 'expires-in' | extract: identity_admin (secret goes to an owner-only file; never into renderer state) | not-started | `crates/lightagent/src/identity_admin.rs:77` |
| `cli.identity.credential.issue.opt.capability` | `lightagent identity credential issue --capability` | applicable | yes | Settings/Identity: issue → native save dialog for --secret-file — input 'capability' | extract: identity_admin (secret goes to an owner-only file; never into renderer state) | not-started | `crates/lightagent/src/identity_admin.rs:77` |
| `cli.identity.credential.issue.opt.profile` | `lightagent identity credential issue --profile` | applicable | yes | Settings/Identity: issue → native save dialog for --secret-file — input 'profile' | extract: identity_admin (secret goes to an owner-only file; never into renderer state) | not-started | `crates/lightagent/src/identity_admin.rs:77` |
| `cli.identity.credential.issue.opt.secret-file` | `lightagent identity credential issue --secret-file` | applicable | yes | Settings/Identity: issue → native save dialog for --secret-file — input 'secret-file' | extract: identity_admin (secret goes to an owner-only file; never into renderer state) | not-started | `crates/lightagent/src/identity_admin.rs:77` |
| `cli.identity.credential.rotate.opt.expires-in` | `lightagent identity credential rotate --expires-in` | applicable | yes | Settings/Identity: rotate — input 'expires-in' | extract: identity_admin | not-started | `crates/lightagent/src/identity_admin.rs:97` |
| `cli.identity.credential.rotate.opt.secret-file` | `lightagent identity credential rotate --secret-file` | applicable | yes | Settings/Identity: rotate — input 'secret-file' | extract: identity_admin | not-started | `crates/lightagent/src/identity_admin.rs:97` |
| `cli.identity.credential.list.opt.principal` | `lightagent identity credential list --principal` | applicable | yes | Settings/Identity: credentials — input 'principal' | extract: identity_admin | not-started | `crates/lightagent/src/identity_admin.rs:107` |
| `cli.identity.grants.list.opt.profile` | `lightagent identity grants list --profile` | applicable | yes | Settings/Identity: grants — input 'profile' | extract: identity_admin | not-started | `crates/lightagent/src/identity_admin.rs:126` |
| `cli.identity.audit.opt.limit` | `lightagent identity audit --limit` | applicable | yes | Settings/Identity: audit — input 'limit' | extract: identity_admin | not-started | `crates/lightagent/src/identity_admin.rs:56` |
| `cli.remote-credential.add.opt.service` | `lightagent remote-credential add --service` | applicable | yes | Settings/Credentials: add (secret via native file picker; never typed into the renderer) — input 'service' | extract: remote_credentials | not-started | `crates/lightagent/src/remote_credentials.rs:33` |
| `cli.remote-credential.add.opt.destination` | `lightagent remote-credential add --destination` | applicable | yes | Settings/Credentials: add (secret via native file picker; never typed into the renderer) — input 'destination' | extract: remote_credentials | not-started | `crates/lightagent/src/remote_credentials.rs:33` |
| `cli.remote-credential.add.opt.operation` | `lightagent remote-credential add --operation` | applicable | yes | Settings/Credentials: add (secret via native file picker; never typed into the renderer) — input 'operation' | extract: remote_credentials | not-started | `crates/lightagent/src/remote_credentials.rs:33` |
| `cli.remote-credential.add.opt.expires-in` | `lightagent remote-credential add --expires-in` | applicable | yes | Settings/Credentials: add (secret via native file picker; never typed into the renderer) — input 'expires-in' | extract: remote_credentials | not-started | `crates/lightagent/src/remote_credentials.rs:33` |
| `cli.remote-credential.add.opt.max-requests-per-run` | `lightagent remote-credential add --max-requests-per-run` | applicable | yes | Settings/Credentials: add (secret via native file picker; never typed into the renderer) — input 'max-requests-per-run' | extract: remote_credentials | not-started | `crates/lightagent/src/remote_credentials.rs:33` |
| `cli.remote-credential.add.opt.secret-file` | `lightagent remote-credential add --secret-file` | applicable | yes | Settings/Credentials: add (secret via native file picker; never typed into the renderer) — input 'secret-file' | extract: remote_credentials | not-started | `crates/lightagent/src/remote_credentials.rs:33` |
| `cli.remote-credential.add.opt.secret-stdin` | `lightagent remote-credential add --secret-stdin` | applicable | yes | Backend-only secret channel (host passes bytes to the sidecar; renderer never holds them) | extract: remote_credentials | not-started | `crates/lightagent/src/remote_credentials.rs:33` |
| `cli.remote-credential.rotate.opt.secret-file` | `lightagent remote-credential rotate --secret-file` | applicable | yes | Settings/Credentials: rotate — input 'secret-file' | extract: remote_credentials | not-started | `crates/lightagent/src/remote_credentials.rs:53` |
| `cli.remote-credential.rotate.opt.secret-stdin` | `lightagent remote-credential rotate --secret-stdin` | applicable | yes | Backend-only secret channel | extract: remote_credentials | not-started | `crates/lightagent/src/remote_credentials.rs:53` |
| `cli.remote-credential.list.opt.profile` | `lightagent remote-credential list --profile` | applicable | yes | Settings/Credentials — input 'profile' | extract: remote_credentials | not-started | `crates/lightagent/src/remote_credentials.rs:61` |
| `cli.remote-credential.audit.opt.limit` | `lightagent remote-credential audit --limit` | applicable | yes | Settings/Credentials: audit — input 'limit' | extract: remote_credentials | not-started | `crates/lightagent/src/remote_credentials.rs:66` |
| `cli.remote-credential.rekey.opt.previous-key-file` | `lightagent remote-credential rekey --previous-key-file` | applicable | yes | Settings/Credentials: rekey (native file picker) — input 'previous-key-file' | extract: remote_credentials | not-started | `crates/lightagent/src/remote_credentials.rs:72` |
| `cli.import.hermes.opt.from` | `lightagent import hermes --from` | applicable | yes | Profiles/Workspaces: import from Hermes (preview then apply) — input 'from' | extract: import::hermes() | not-started | `crates/lightagent/src/lib.rs:363; crates/lightagent/src/import.rs:28` |
| `cli.import.hermes.opt.dry-run` | `lightagent import hermes --dry-run` | applicable | yes | Profiles/Workspaces: import from Hermes (preview then apply) — input 'dry-run' | extract: import::hermes() | not-started | `crates/lightagent/src/lib.rs:363; crates/lightagent/src/import.rs:28` |
| `cli.import.hermes.opt.force` | `lightagent import hermes --force` | applicable | yes | Profiles/Workspaces: import from Hermes (preview then apply) — input 'force' | extract: import::hermes() | not-started | `crates/lightagent/src/lib.rs:363; crates/lightagent/src/import.rs:28` |
| `cli.rag.add.opt.source` | `lightagent rag add --source` | applicable | yes | Memory/Knowledge: add documents (picker) — input 'source' | extract: rag::add() | not-started | `crates/lightagent/src/lib.rs:342` |
| `cli.rag.search.opt.top-k` | `lightagent rag search --top-k` | applicable | yes | Memory/Knowledge: search — input 'top-k' | extract: rag::search() | implemented | `crates/lightagent/src/lib.rs:349` |
| `cli.memory.add.opt.kind` | `lightagent memory add --kind` | applicable | yes | Memory/Knowledge: add — input 'kind' | extract: memory::add() | implemented | `crates/lightagent/src/lib.rs:299` |
| `cli.memory.add.opt.tag` | `lightagent memory add --tag` | applicable | yes | Memory/Knowledge: add — input 'tag' | extract: memory::add() | implemented | `crates/lightagent/src/lib.rs:299` |
| `cli.memory.search.opt.top-k` | `lightagent memory search --top-k` | applicable | yes | Memory/Knowledge: search — input 'top-k' | extract: memory::search() | implemented | `crates/lightagent/src/lib.rs:311` |
| `cli.memory.promote.opt.text` | `lightagent memory promote --text` | applicable | yes | Conversations: message → remember — input 'text' | extract: memory::promote() | implemented | `crates/lightagent/src/lib.rs:319` |
| `cli.memory.promote.opt.kind` | `lightagent memory promote --kind` | applicable | yes | Conversations: message → remember — input 'kind' | extract: memory::promote() | implemented | `crates/lightagent/src/lib.rs:319` |
| `cli.memory.promote.opt.tag` | `lightagent memory promote --tag` | applicable | yes | Conversations: message → remember — input 'tag' | extract: memory::promote() | implemented | `crates/lightagent/src/lib.rs:319` |
| `cli.banner.opt.preview` | `lightagent banner --preview` | not-applicable | no | Option of lightagent banner: Terminal cosmetic |  | not-started | `crates/lightagent/src/lib.rs:176` |
| `cli.update.opt.check` | `lightagent update --check` | applicable (D1-Q1) | yes | Settings/Diagnostics: Check for updates (read-only; installs nothing) | extract: release_update check-only operation | not-started | `crates/lightagent/src/lib.rs:182` |
| `cli.update.opt.force` | `lightagent update --force` | not-applicable (D1-Q1) | no | (decision D1-Q1) — input 'force' | release_update::run() | not-started | `crates/lightagent/src/lib.rs:182` |

## argument

| ID | Surface | Applicability | Counts | UI path | Backend | Status | Source |
|---|---|---|---|---|---|---|---|
| `cli.setup.arg.section` | `lightagent setup <section>` | applicable | no | Settings (sections) — input 'section' | extract: setup.rs run_with_io() section operations | not-started | `crates/lightagent/src/lib.rs:88; crates/lightagent/src/setup.rs:15` |
| `cli.config.get.arg.key` | `lightagent config get <key>` | applicable | yes | Settings: advanced key editor (typed) — input 'key' | extract: config_cmd(Get) | implemented | `crates/lightagent/src/lib.rs:199` |
| `cli.config.set.arg.key` | `lightagent config set <key>` | applicable | yes | Settings: advanced key editor (typed, validated) — input 'key' | api: PUT /settings (subset); extract: config_cmd(Set) | implemented | `crates/lightagent/src/lib.rs:201` |
| `cli.config.set.arg.value` | `lightagent config set <value>` | applicable | yes | Settings: advanced key editor (typed, validated) — input 'value' | api: PUT /settings (subset); extract: config_cmd(Set) | implemented | `crates/lightagent/src/lib.rs:201` |
| `cli.profile.show.arg.id` | `lightagent profile show <id>` | applicable | yes | Profiles/Workspaces: detail — input 'id' | ops::Profiles::show; api: GET /profiles/{id} | implemented | `crates/lightagent/src/lib.rs:250` |
| `cli.profile.create.arg.id` | `lightagent profile create <id>` | applicable | yes | Profiles/Workspaces: create form — input 'id' | ops::Profiles::create; api: POST /profiles | implemented | `crates/lightagent/src/lib.rs:253` |
| `cli.profile.use.arg.id` | `lightagent profile use <id>` | applicable | yes | Profiles/Workspaces: switch (see D1 profile-switch protocol) — input 'id' | ops::Profiles::use_profile; api: PUT /profiles/{id}/active (+ host sidecar switch, D4b) | implemented | `crates/lightagent/src/lib.rs:263` |
| `cli.profile.preset.arg.id` | `lightagent profile preset <id>` | applicable | yes | Profiles/Workspaces: preset (admin) — input 'id' | ops::Profiles::preset; api: POST /profiles/{id}/preset | implemented | `crates/lightagent/src/lib.rs:265` |
| `cli.profile.preset.arg.preset` | `lightagent profile preset <preset>` | applicable | yes | Profiles/Workspaces: preset (admin) — input 'preset' | ops::Profiles::preset; api: POST /profiles/{id}/preset | implemented | `crates/lightagent/src/lib.rs:265` |
| `cli.profile.delete.arg.id` | `lightagent profile delete <id>` | applicable | yes | Profiles/Workspaces: delete (typed confirmation) — input 'id' | ops::Profiles::delete; api: DELETE /profiles/{id} | implemented | `crates/lightagent/src/lib.rs:267` |
| `cli.extensions.show.arg.name` | `lightagent extensions show <name>` | applicable | yes | Tools/MCP/Extensions: detail — input 'name' | extract: extensions::show() | implemented | `crates/lightagent/src/lib.rs:275` |
| `cli.extensions.enable.arg.name` | `lightagent extensions enable <name>` | applicable | yes | Tools/MCP/Extensions: toggle — input 'name' | extract: extensions::enable() | implemented | `crates/lightagent/src/lib.rs:277` |
| `cli.extensions.disable.arg.name` | `lightagent extensions disable <name>` | applicable | yes | Tools/MCP/Extensions: toggle — input 'name' | extract: extensions::disable() | implemented | `crates/lightagent/src/lib.rs:279` |
| `cli.extensions.install.arg.source` | `lightagent extensions install <source>` | applicable | yes | Tools/MCP/Extensions: install (folder picker or HTTPS URL) — input 'source' | extract: extensions::install() | implemented | `crates/lightagent/src/lib.rs:281` |
| `cli.extensions.uninstall.arg.name` | `lightagent extensions uninstall <name>` | applicable | yes | Tools/MCP/Extensions: uninstall (confirm) — input 'name' | extract: extensions::uninstall() | implemented | `crates/lightagent/src/lib.rs:288` |
| `cli.sessions.show.arg.id` | `lightagent sessions show <id>` | applicable | yes | Conversations: open session — input 'id' | api: GET /sessions/{id} | not-started | `crates/lightagent/src/lib.rs:381` |
| `cli.sessions.delete.arg.id` | `lightagent sessions delete <id>` | applicable | yes | Conversations: delete (confirm) — input 'id' | api: DELETE /sessions/{id} | not-started | `crates/lightagent/src/lib.rs:383` |
| `cli.workers.assign.arg.role` | `lightagent workers assign <role>` | applicable | yes | Runs/Agents: assign role — input 'role' | extract: workers_cmd(Assign) | not-started | `crates/lightagent/src/lib.rs:223` |
| `cli.workers.revoke.arg.role` | `lightagent workers revoke <role>` | applicable | yes | Runs/Agents: revoke role — input 'role' | extract: workers_cmd(Revoke) | not-started | `crates/lightagent/src/lib.rs:237` |
| `cli.identity.user.create.arg.id` | `lightagent identity user create <id>` | applicable | yes | Settings/Identity: users — input 'id' | extract: identity_admin | not-started | `crates/lightagent/src/identity_admin.rs:66` |
| `cli.identity.user.suspend.arg.id` | `lightagent identity user suspend <id>` | applicable | yes | Settings/Identity: users (confirm) — input 'id' | extract: identity_admin | not-started | `crates/lightagent/src/identity_admin.rs:71` |
| `cli.identity.credential.issue.arg.principal` | `lightagent identity credential issue <principal>` | applicable | yes | Settings/Identity: issue → native save dialog for --secret-file — input 'principal' | extract: identity_admin (secret goes to an owner-only file; never into renderer state) | not-started | `crates/lightagent/src/identity_admin.rs:77` |
| `cli.identity.credential.rotate.arg.key_id` | `lightagent identity credential rotate <key_id>` | applicable | yes | Settings/Identity: rotate — input 'key_id' | extract: identity_admin | not-started | `crates/lightagent/src/identity_admin.rs:97` |
| `cli.identity.credential.revoke.arg.key_id` | `lightagent identity credential revoke <key_id>` | applicable | yes | Settings/Identity: revoke (confirm) — input 'key_id' | extract: identity_admin | not-started | `crates/lightagent/src/identity_admin.rs:105` |
| `cli.identity.owner.assign.arg.profile` | `lightagent identity owner assign <profile>` | applicable | yes | Settings/Identity: ownership — input 'profile' | extract: identity_admin | not-started | `crates/lightagent/src/identity_admin.rs:116` |
| `cli.identity.owner.assign.arg.principal` | `lightagent identity owner assign <principal>` | applicable | yes | Settings/Identity: ownership — input 'principal' | extract: identity_admin | not-started | `crates/lightagent/src/identity_admin.rs:116` |
| `cli.identity.owner.reassign.arg.profile` | `lightagent identity owner reassign <profile>` | applicable | yes | Settings/Identity: ownership (confirm) — input 'profile' | extract: identity_admin | not-started | `crates/lightagent/src/identity_admin.rs:118` |
| `cli.identity.owner.reassign.arg.principal` | `lightagent identity owner reassign <principal>` | applicable | yes | Settings/Identity: ownership (confirm) — input 'principal' | extract: identity_admin | not-started | `crates/lightagent/src/identity_admin.rs:118` |
| `cli.identity.owner.remove.arg.profile` | `lightagent identity owner remove <profile>` | applicable | yes | Settings/Identity: ownership (confirm) — input 'profile' | extract: identity_admin | not-started | `crates/lightagent/src/identity_admin.rs:120` |
| `cli.identity.grants.revoke.arg.profile` | `lightagent identity grants revoke <profile>` | applicable | yes | Settings/Identity: grants (confirm) — input 'profile' | extract: identity_admin | not-started | `crates/lightagent/src/identity_admin.rs:131` |
| `cli.identity.grants.revoke.arg.principal` | `lightagent identity grants revoke <principal>` | applicable | yes | Settings/Identity: grants (confirm) — input 'principal' | extract: identity_admin | not-started | `crates/lightagent/src/identity_admin.rs:131` |
| `cli.identity.grants.revoke.arg.capability` | `lightagent identity grants revoke <capability>` | applicable | yes | Settings/Identity: grants (confirm) — input 'capability' | extract: identity_admin | not-started | `crates/lightagent/src/identity_admin.rs:131` |
| `cli.remote-credential.keygen.arg.path` | `lightagent remote-credential keygen <path>` | applicable | yes | Settings/Credentials: keygen (native save dialog) — input 'path' | extract: remote_credentials | not-started | `crates/lightagent/src/remote_credentials.rs:31` |
| `cli.remote-credential.add.arg.profile` | `lightagent remote-credential add <profile>` | applicable | yes | Settings/Credentials: add (secret via native file picker; never typed into the renderer) — input 'profile' | extract: remote_credentials | not-started | `crates/lightagent/src/remote_credentials.rs:33` |
| `cli.remote-credential.rotate.arg.id` | `lightagent remote-credential rotate <id>` | applicable | yes | Settings/Credentials: rotate — input 'id' | extract: remote_credentials | not-started | `crates/lightagent/src/remote_credentials.rs:53` |
| `cli.remote-credential.revoke.arg.id` | `lightagent remote-credential revoke <id>` | applicable | yes | Settings/Credentials: revoke (confirm) — input 'id' | extract: remote_credentials | not-started | `crates/lightagent/src/remote_credentials.rs:59` |
| `cli.rag.add.arg.path` | `lightagent rag add <path>` | applicable | yes | Memory/Knowledge: add documents (picker) — input 'path' | extract: rag::add() | implemented | `crates/lightagent/src/lib.rs:342` |
| `cli.rag.search.arg.query` | `lightagent rag search <query>` | applicable | yes | Memory/Knowledge: search — input 'query' | extract: rag::search() | implemented | `crates/lightagent/src/lib.rs:349` |
| `cli.memory.add.arg.text` | `lightagent memory add <text>` | applicable | yes | Memory/Knowledge: add — input 'text' | extract: memory::add() | implemented | `crates/lightagent/src/lib.rs:299` |
| `cli.memory.reflect.arg.topic` | `lightagent memory reflect <topic>` | applicable | yes | Memory/Knowledge: reflect — input 'topic' | extract: memory::reflect() | implemented | `crates/lightagent/src/lib.rs:309` |
| `cli.memory.search.arg.query` | `lightagent memory search <query>` | applicable | yes | Memory/Knowledge: search — input 'query' | extract: memory::search() | implemented | `crates/lightagent/src/lib.rs:311` |
| `cli.memory.candidates.arg.session` | `lightagent memory candidates <session>` | applicable | yes | Conversations: session → memory candidates — input 'session' | extract: memory::candidates() | implemented | `crates/lightagent/src/lib.rs:317` |
| `cli.memory.promote.arg.session` | `lightagent memory promote <session>` | applicable | yes | Conversations: message → remember — input 'session' | extract: memory::promote() | implemented | `crates/lightagent/src/lib.rs:319` |
| `cli.memory.promote.arg.message` | `lightagent memory promote <message>` | applicable | yes | Conversations: message → remember — input 'message' | extract: memory::promote() | implemented | `crates/lightagent/src/lib.rs:319` |
| `cli.memory.update.arg.id` | `lightagent memory update <id>` | applicable | yes | Memory/Knowledge: edit — input 'id' | extract: memory::update() | implemented | `crates/lightagent/src/lib.rs:332` |
| `cli.memory.update.arg.text` | `lightagent memory update <text>` | applicable | yes | Memory/Knowledge: edit — input 'text' | extract: memory::update() | implemented | `crates/lightagent/src/lib.rs:332` |
| `cli.memory.forget.arg.id` | `lightagent memory forget <id>` | applicable | yes | Memory/Knowledge: forget (confirm) — input 'id' | extract: memory::forget() | implemented | `crates/lightagent/src/lib.rs:334` |

## value

| ID | Surface | Applicability | Counts | UI path | Backend | Status | Source |
|---|---|---|---|---|---|---|---|
| `cli.setup.section.approvals` | `lightagent setup approvals` | applicable | dup→`setup.approvals.policy` | Settings | extract: setup.rs run_with_io() section operations | not-started | `crates/lightagent/src/lib.rs:88; crates/lightagent/src/setup.rs:15` |
| `cli.setup.section.profiles` | `lightagent setup profiles` | applicable | dup→`setup.profiles.menu` | Settings | extract: setup.rs run_with_io() section operations | not-started | `crates/lightagent/src/lib.rs:88; crates/lightagent/src/setup.rs:15` |
| `cli.setup.section.provider` | `lightagent setup provider` | applicable | dup→`setup.provider.url` | Settings | extract: setup.rs run_with_io() section operations | not-started | `crates/lightagent/src/lib.rs:88; crates/lightagent/src/setup.rs:15` |
| `cli.setup.section.tools` | `lightagent setup tools` | applicable | dup→`setup.tools.mode` | Settings | extract: setup.rs run_with_io() section operations | not-started | `crates/lightagent/src/lib.rs:88; crates/lightagent/src/setup.rs:15` |
| `cli.setup.section.tui` | `lightagent setup tui` | not-applicable | no | Settings | extract: setup.rs run_with_io() section operations | not-started | `crates/lightagent/src/lib.rs:88; crates/lightagent/src/setup.rs:15` |
| `cli.setup.section.web` | `lightagent setup web` | applicable | dup→`setup.web.mode` | Settings | extract: setup.rs run_with_io() section operations | not-started | `crates/lightagent/src/lib.rs:88; crates/lightagent/src/setup.rs:15` |

## alias

| ID | Surface | Applicability | Counts | UI path | Backend | Status | Source |
|---|---|---|---|---|---|---|---|
| `cli.profile.alias.profiles` | `lightagent profile (alias profiles)` | not-applicable | dup→`cli.profile` | Naming alias of an inventoried command |  | not-started | `crates/lightagent/src/lib.rs:106` |
| `cli.profile.create.alias.add` | `lightagent profile create (alias add)` | not-applicable | dup→`cli.profile.create` | Naming alias of an inventoried command |  | not-started | `crates/lightagent/src/lib.rs:252` |

## setup-option

| ID | Surface | Applicability | Counts | UI path | Backend | Status | Source |
|---|---|---|---|---|---|---|---|
| `setup.profiles.menu` | `lightagent setup → profiles` | applicable | dup→`cli.profile.create` | Profiles/Workspaces | same as cli.profile.* | not-started | `crates/lightagent/src/setup.rs:748` |
| `setup.provider.saved` | `lightagent setup → provider` | applicable | yes | Providers/Models: saved providers | shared: lightagent_core::saved_providers; api: GET\|POST /providers, GET\|PATCH\|DELETE /providers/{id} | implemented | `crates/lightagent/src/setup.rs:326` |
| `setup.provider.url` | `lightagent setup → provider` | applicable | yes | Providers/Models: endpoint + test connection | shared: saved_providers::select + provider_keys::bound_key; api: PUT /provider/active, POST /providers/{id}/check | implemented | `crates/lightagent/src/setup.rs:67` |
| `setup.provider.key` | `lightagent setup → provider` | applicable | yes | Providers/Models: key entry (write-only; host sends to backend; renderer shows redacted status) | shared: lightagent_core::provider_keys; api: GET\|PUT\|DELETE /provider/credential | implemented | `crates/lightagent/src/setup.rs:391` |
| `setup.provider.model` | `lightagent setup → provider` | applicable | yes | Providers/Models: model picker | shared: saved_providers::select_model; api: PUT /provider/model | implemented | `crates/lightagent/src/setup.rs:97` |
| `setup.tools.mode` | `lightagent setup → tools` | applicable | yes | Tools/MCP/Extensions: local tools | api: PUT /settings (partial) | not-started | `crates/lightagent/src/setup.rs:910` |
| `setup.tools.workspace` | `lightagent setup → tools` | applicable | yes | Profiles/Workspaces: workspace folder (picker) | shared: lightagent_core::workspaces; api: GET\|PUT /workspace | implemented | `crates/lightagent/src/setup.rs:920` |
| `setup.web.mode` | `lightagent setup → web` | applicable | yes | Settings: web access | api: PUT /settings (partial) | not-started | `crates/lightagent/src/setup.rs:945` |
| `setup.approvals.policy` | `lightagent setup → approvals` | applicable | yes | Settings: approvals | api: PUT /settings (partial) | not-started | `crates/lightagent/src/setup.rs:983` |
| `setup.tui.reasoning` | `lightagent setup → tui` | applicable | yes | Settings: appearance — reasoning panel | config [tui] (no API today) | not-started | `crates/lightagent/src/setup.rs:1007` |

## slash

| ID | Surface | Applicability | Counts | UI path | Backend | Status | Source |
|---|---|---|---|---|---|---|---|
| `slash.help` | `/help` | applicable | yes | Command palette: help | static | not-started | `crates/lightagent/src/slash.rs:56` |
| `slash.tools` | `/tools` | applicable | dup→`cli.tools.list` |  |  | not-started | `crates/lightagent/src/slash.rs:57` |
| `slash.skills` | `/skills` | applicable | yes | Skills | api: GET /skills | implemented | `crates/lightagent/src/slash.rs:58` |
| `slash.reload` | `/reload` | applicable | yes | Tools/MCP/Extensions: reload | extract: chat.rs reload path | not-started | `crates/lightagent/src/slash.rs:59` |
| `slash.extensions` | `/extensions` | applicable | dup→`cli.extensions.list` |  |  | not-started | `crates/lightagent/src/slash.rs:60` |
| `slash.extensions-install` | `/extensions install` | applicable | dup→`cli.extensions.install` |  |  | not-started | `crates/lightagent/src/slash.rs:63` |
| `slash.extensions-uninstall` | `/extensions uninstall` | applicable | dup→`cli.extensions.uninstall` |  |  | not-started | `crates/lightagent/src/slash.rs:67` |
| `slash.onboard` | `/onboard` | applicable | yes | Profiles/Workspaces: guidance (file picker / drop) | extract: chat.rs onboard | not-started | `crates/lightagent/src/slash.rs:75` |
| `slash.onboard-remove` | `/onboard remove` | applicable | yes | Profiles/Workspaces: guidance remove | extract: chat.rs onboard remove | not-started | `crates/lightagent/src/slash.rs:76` |
| `slash.new` | `/new` | applicable | yes | Conversations: new | api: POST /sessions | not-started | `crates/lightagent/src/slash.rs:82` |
| `slash.approve` | `/approve` | applicable | yes | Conversations: approval card | api: POST /approvals/{run} | implemented | `crates/lightagent/src/slash.rs:83` |
| `slash.reject` | `/reject` | applicable | yes | Conversations: approval card | api: POST /approvals/{run} | implemented | `crates/lightagent/src/slash.rs:84` |
| `slash.stop` | `/stop` | applicable | yes | Conversations: stop | api: POST /runs/{id}/cancel | implemented | `crates/lightagent/src/slash.rs:85` |
| `slash.continue` | `/continue` | applicable | yes | Conversations: continue | api: POST /runs/{id}/continue, for a run started with on_time_budget pause (D5a-3); resumes the same execution | implemented | `crates/lightagent/src/slash.rs:86` |
| `slash.update` | `/update` | not-applicable (D1-Q2) | no | (decision D1-Q2) |  | not-started | `crates/lightagent/src/slash.rs:87` |
| `slash.exit` | `/exit` | not-applicable | no | Window/session close is native |  | not-started | `crates/lightagent/src/slash.rs:88` |

## keybinding

| ID | Surface | Applicability | Counts | UI path | Backend | Status | Source |
|---|---|---|---|---|---|---|---|
| `key.enter` | `chat key: enter` | applicable | yes | Conversations: composer Enter to send (Shift+Enter newline) | api: POST /runs | not-started | `crates/lightagent/src/chat.rs:1969` |
| `key.ctrl-c` | `chat key: ctrl-c` | not-applicable | no | Native window close / Stop button (slash.stop) |  | not-started | `crates/lightagent/src/chat.rs:1963` |
| `key.ctrl-d` | `chat key: ctrl-d` | not-applicable | no | Native window close |  | not-started | `crates/lightagent/src/chat.rs:1964` |
| `key.line-editing` | `chat key: line-editing` | not-applicable | no | Provided by the native text field |  | not-started | `crates/lightagent/src/chat.rs:1973` |
| `key.steer` | `chat key: steer` | applicable | yes | Conversations: composer stays enabled during a run → queued steer | client-held FIFO queue over api: POST /runs, as the terminal holds it; no backend queue (D5a-3) | implemented | `crates/lightagent/src/chat.rs:2955` |
| `key.approval-answer` | `chat key: approval-answer` | applicable | dup→`slash.approve` |  |  | not-started | `crates/lightagent/src/chat.rs:2031` |

## acp-method

| ID | Surface | Applicability | Counts | UI path | Backend | Status | Source |
|---|---|---|---|---|---|---|---|
| `acp.initialize` | `ACP initialize` | not-applicable | no | Editor protocol transport (lightagent acp); the operation it carries is inventoried under (handshake) |  | not-started | `crates/lightagent-acp/src/server.rs:199` |
| `acp.authenticate` | `ACP authenticate` | not-applicable | no | Editor protocol transport (lightagent acp); the operation it carries is inventoried under (handshake) |  | not-started | `crates/lightagent-acp/src/server.rs:219` |
| `acp.session.new` | `ACP session/new` | not-applicable | dup→`slash.new` | Editor protocol transport (lightagent acp); the operation it carries is inventoried under slash.new |  | not-started | `crates/lightagent-acp/src/server.rs:224` |
| `acp.session.load` | `ACP session/load` | not-applicable | dup→`cli.sessions.show` | Editor protocol transport (lightagent acp); the operation it carries is inventoried under cli.sessions.show |  | not-started | `crates/lightagent-acp/src/server.rs:270` |
| `acp.session.cancel` | `ACP session/cancel` | not-applicable | dup→`slash.stop` | Editor protocol transport (lightagent acp); the operation it carries is inventoried under slash.stop |  | not-started | `crates/lightagent-acp/src/server.rs:341` |
| `acp.session.prompt` | `ACP session/prompt` | not-applicable | dup→`cli.chat` | Editor protocol transport (lightagent acp); the operation it carries is inventoried under cli.chat |  | not-started | `crates/lightagent-acp/src/server.rs:349` |
| `acp.session.update` | `ACP session/update` | not-applicable | dup→`api.get.runs.id.events` | Editor protocol transport (lightagent acp); the operation it carries is inventoried under api.get.runs.id.events |  | not-started | `crates/lightagent-acp/src/server.rs:329` |
| `acp.session.request_permission` | `ACP session/request_permission` | not-applicable | dup→`slash.approve` | Editor protocol transport (lightagent acp); the operation it carries is inventoried under slash.approve |  | not-started | `crates/lightagent-acp/src/server.rs:563` |

## mcp-registration

| ID | Surface | Applicability | Counts | UI path | Backend | Status | Source |
|---|---|---|---|---|---|---|---|
| `mcp.server.stdio` | `[mcp].servers transport=stdio` | applicable | yes | Tools/MCP/Extensions: MCP servers (shows command; external executable must exist) | config: [mcp].servers (no dedicated CLI/API operation; new typed config operation in D6) | implemented | `crates/lightagent-core/src/config.rs:416` |
| `mcp.server.stdio.command` | `[mcp].servers transport=stdio.command` | applicable | yes | Tools/MCP/Extensions: MCP server form field | config: [mcp].servers (no dedicated CLI/API operation; new typed config operation in D6) | implemented | `crates/lightagent-core/src/config.rs:418` |
| `mcp.server.stdio.args` | `[mcp].servers transport=stdio.args` | applicable | yes | Tools/MCP/Extensions: MCP server form field | config: [mcp].servers (no dedicated CLI/API operation; new typed config operation in D6) | implemented | `crates/lightagent-core/src/config.rs:420` |
| `mcp.server.stdio.env` | `[mcp].servers transport=stdio.env` | applicable | yes | Tools/MCP/Extensions: MCP server form field | config: [mcp].servers (no dedicated CLI/API operation; new typed config operation in D6) | implemented | `crates/lightagent-core/src/config.rs:422` |
| `mcp.server.http` | `[mcp].servers transport=http` | applicable | yes | Tools/MCP/Extensions: MCP servers | config: [mcp].servers (no dedicated CLI/API operation; new typed config operation in D6) | implemented | `crates/lightagent-core/src/config.rs:424` |
| `mcp.server.http.url` | `[mcp].servers transport=http.url` | applicable | yes | Tools/MCP/Extensions: MCP server form field | config: [mcp].servers (no dedicated CLI/API operation; new typed config operation in D6) | implemented | `crates/lightagent-core/src/config.rs:426` |
| `mcp.server.http.headers` | `[mcp].servers transport=http.headers` | applicable | yes | Tools/MCP/Extensions: MCP server form field | config: [mcp].servers (no dedicated CLI/API operation; new typed config operation in D6) | implemented | `crates/lightagent-core/src/config.rs:428` |
| `mcp.server.http.auth` | `[mcp].servers transport=http.auth` | applicable | yes | Tools/MCP/Extensions: auth status (redacted) | config: [mcp].servers (no dedicated CLI/API operation; new typed config operation in D6) | implemented | `crates/lightagent-core/src/config.rs:430` |
| `mcp.server.http.untrusted_profiles` | `[mcp].servers transport=http.untrusted_profiles` | applicable | yes | Tools/MCP/Extensions: MCP server form field | config: [mcp].servers (no dedicated CLI/API operation; new typed config operation in D6) | implemented | `crates/lightagent-core/src/config.rs:434` |
| `mcp.server.http.credential_profiles` | `[mcp].servers transport=http.credential_profiles` | applicable | yes | Tools/MCP/Extensions: MCP server form field | config: [mcp].servers (no dedicated CLI/API operation; new typed config operation in D6) | implemented | `crates/lightagent-core/src/config.rs:438` |

## extension-registration

| ID | Surface | Applicability | Counts | UI path | Backend | Status | Source |
|---|---|---|---|---|---|---|---|
| `extension.manifest.name` | `extension.json name` | applicable | yes | Tools/MCP/Extensions: extension detail | extract: extensions::show() | implemented | `crates/lightagent-extensions/src/lib.rs:51` |
| `extension.manifest.version` | `extension.json version` | applicable | yes | Tools/MCP/Extensions: extension detail | extract: extensions::show() | implemented | `crates/lightagent-extensions/src/lib.rs:53` |
| `extension.manifest.description` | `extension.json description` | applicable | yes | Tools/MCP/Extensions: extension detail | extract: extensions::show() | implemented | `crates/lightagent-extensions/src/lib.rs:55` |
| `extension.manifest.instructions` | `extension.json instructions` | applicable | yes | Tools/MCP/Extensions: extension detail | extract: extensions::show() | implemented | `crates/lightagent-extensions/src/lib.rs:57` |
| `extension.manifest.mcp_servers` | `extension.json mcp_servers` | applicable | yes | Tools/MCP/Extensions: extension detail | extract: extensions::show() | implemented | `crates/lightagent-extensions/src/lib.rs:59` |
| `extension.roots` | `<home>/extensions and <profile>/extensions` | applicable | yes | Tools/MCP/Extensions: scope badge (global/profile) | extract: extensions::list() | implemented | `crates/lightagent-extensions/src/lib.rs:17` |

## skill-registration

| ID | Surface | Applicability | Counts | UI path | Backend | Status | Source |
|---|---|---|---|---|---|---|---|
| `skills.roots` | `<home>/skills and <profile>/skills (SKILL.md)` | applicable | yes | Skills: list with scope and source | api: GET /skills | implemented | `crates/lightagent-core/src/skills.rs:204` |

## config

| ID | Surface | Applicability | Counts | UI path | Backend | Status | Source |
|---|---|---|---|---|---|---|---|
| `config.inference` | `config.toml [inference]` | applicable | yes | Settings | api: GET/PUT /settings (subset); extract: typed config read/write | not-started | `crates/lightagent-core/src/config.rs:Config.inference` |
| `config.agent` | `config.toml [agent]` | applicable | yes | Settings | api: GET/PUT /settings (subset); extract: typed config read/write | not-started | `crates/lightagent-core/src/config.rs:Config.agent` |
| `config.security` | `config.toml [security]` | applicable | yes | Settings | api: GET/PUT /settings (subset); extract: typed config read/write | not-started | `crates/lightagent-core/src/config.rs:Config.security` |
| `config.web` | `config.toml [web]` | applicable | yes | Settings | api: GET/PUT /settings (subset); extract: typed config read/write | not-started | `crates/lightagent-core/src/config.rs:Config.web` |
| `config.tools` | `config.toml [tools]` | applicable | yes | Settings | api: GET/PUT /settings (subset); extract: typed config read/write | not-started | `crates/lightagent-core/src/config.rs:Config.tools` |
| `config.mcp` | `config.toml [mcp]` | applicable | yes | Tools/MCP/Extensions | api: GET/PUT /settings (subset); extract: typed config read/write | implemented | `crates/lightagent-core/src/config.rs:Config.mcp` |
| `config.rag` | `config.toml [rag]` | applicable | yes | Memory/Knowledge | api: GET/PUT /settings (subset); extract: typed config read/write | not-started | `crates/lightagent-core/src/config.rs:Config.rag` |
| `config.memory` | `config.toml [memory]` | applicable | yes | Memory/Knowledge | api: GET/PUT /settings (subset); extract: typed config read/write | not-started | `crates/lightagent-core/src/config.rs:Config.memory` |
| `config.extensions` | `config.toml [extensions]` | applicable | yes | Settings | api: GET/PUT /settings (subset); extract: typed config read/write | implemented | `crates/lightagent-core/src/config.rs:Config.extensions` |
| `config.runtime` | `config.toml [runtime]` | applicable | yes | Settings | api: GET/PUT /settings (subset); extract: typed config read/write | not-started | `crates/lightagent-core/src/config.rs:Config.runtime` |
| `config.generation` | `config.toml [generation]` | applicable | yes | Settings | api: GET/PUT /settings (subset); extract: typed config read/write | not-started | `crates/lightagent-core/src/config.rs:Config.generation` |
| `config.tui` | `config.toml [tui]` | not-applicable | no | Settings | api: GET/PUT /settings (subset); extract: typed config read/write | not-started | `crates/lightagent-core/src/config.rs:Config.tui` |
| `config.platform` | `config.toml [platform]` | applicable | yes | Settings | api: GET/PUT /settings (subset); extract: typed config read/write | not-started | `crates/lightagent-core/src/config.rs:Config.platform` |
| `config.subagents` | `config.toml [subagents]` | applicable | yes | Settings | api: GET/PUT /settings (subset); extract: typed config read/write | not-started | `crates/lightagent-core/src/config.rs:Config.subagents` |

## env

| ID | Surface | Applicability | Counts | UI path | Backend | Status | Source |
|---|---|---|---|---|---|---|---|
| `env.LIGHTAGENT_HOME` | `LIGHTAGENT_HOME` | applicable | yes | Settings/Diagnostics: active home (host passes through to sidecar) | sidecar process environment | not-started | `crates (string literal)` |
| `env.LIGHTAGENT_PROVIDER_KEY` | `LIGHTAGENT_PROVIDER_KEY` | applicable | yes | Providers/Models: key source shown as env reference (redacted) | sidecar process environment | not-started | `crates (string literal)` |
| `env.LIGHTAGENT_API_KEY` | `LIGHTAGENT_API_KEY` | applicable | yes | Remote mode: endpoint key (stored by host, never in renderer) | sidecar process environment | not-started | `crates (string literal)` |
| `env.LIGHTAGENT_CREDENTIAL_KEY` | `LIGHTAGENT_CREDENTIAL_KEY` | applicable | yes | Settings/Credentials: key source status | sidecar process environment | not-started | `crates (string literal)` |
| `env.LIGHTAGENT_CREDENTIAL_KEY_FILE` | `LIGHTAGENT_CREDENTIAL_KEY_FILE` | applicable | yes | Settings/Credentials: key file status | sidecar process environment | not-started | `crates (string literal)` |
| `env.LIGHTAGENT_QDRANT_URL` | `LIGHTAGENT_QDRANT_URL` | applicable | yes | Settings: platform endpoints | sidecar process environment | not-started | `crates (string literal)` |
| `env.LIGHTAGENT_INFINITY_URL` | `LIGHTAGENT_INFINITY_URL` | applicable | yes | Settings: platform endpoints | sidecar process environment | not-started | `crates (string literal)` |
| `env.LIGHTAGENT_OPEN_TERMINAL_URL` | `LIGHTAGENT_OPEN_TERMINAL_URL` | applicable | yes | Settings: platform endpoints | sidecar process environment | not-started | `crates (string literal)` |
| `env.LIGHTAGENT_OPEN_TERMINAL_TOKEN` | `LIGHTAGENT_OPEN_TERMINAL_TOKEN` | applicable | yes | Settings: platform endpoints (redacted) | sidecar process environment | not-started | `crates (string literal)` |
| `env.LIGHTAGENT_NO_BANNER` | `LIGHTAGENT_NO_BANNER` | not-applicable | no | Terminal cosmetic | sidecar process environment | not-started | `crates (string literal)` |
| `env.LIGHTAGENT_INSTALL_ROOT` | `LIGHTAGENT_INSTALL_ROOT` | not-applicable (D1-Q6) | no | D1-Q6: read only by the update installer | sidecar process environment | not-started | `crates (string literal)` |
| `env.HERMES_HOME` | `HERMES_HOME` | applicable | yes | Profiles/Workspaces: import default folder | sidecar process environment | not-started | `crates (string literal)` |

## api

| ID | Surface | Applicability | Counts | UI path | Backend | Status | Source |
|---|---|---|---|---|---|---|---|
| `api.get.health` | `GET /health` | applicable | no | (backend reuse) | api: GET /health | not-started | `crates/lightagent-api/src/lib.rs:75` |
| `api.get.tools` | `GET /api/lightagent/v1/tools` | applicable | no | (backend reuse) | api: GET /tools | not-started | `crates/lightagent-api/src/lib.rs:76` |
| `api.get.workers` | `GET /api/lightagent/v1/workers` | applicable | no | (backend reuse) | api: GET /workers | not-started | `crates/lightagent-api/src/lib.rs:77` |
| `api.get.provider` | `GET /api/lightagent/v1/provider` | applicable | no | (backend reuse) | api: GET /provider | not-started | `crates/lightagent-api/src/lib.rs:78` |
| `api.get.jev` | `GET /api/lightagent/v1/jev` | applicable | yes | Settings: Jev status / verify (already in WebUI) | api: GET /jev | not-started | `crates/lightagent-api/src/lib.rs:82` |
| `api.post.jev.verify` | `POST /api/lightagent/v1/jev/verify` | applicable | yes | Settings: Jev status / verify (already in WebUI) | api: POST /jev/verify | not-started | `crates/lightagent-api/src/lib.rs:83` |
| `api.get.skills` | `GET /api/lightagent/v1/skills` | applicable | no | (backend reuse) | api: GET /skills | not-started | `crates/lightagent-api/src/lib.rs:84` |
| `api.get.profiles` | `GET /api/lightagent/v1/profiles` | applicable | no | (backend reuse) | api: GET /profiles | not-started | `crates/lightagent-api/src/lib.rs:85` |
| `api.get.settings` | `GET /api/lightagent/v1/settings` | applicable | no | (backend reuse) | api: GET /settings | not-started | `crates/lightagent-api/src/lib.rs:86` |
| `api.put.settings` | `PUT /api/lightagent/v1/settings` | applicable | no | (backend reuse) | api: PUT /settings | not-started | `crates/lightagent-api/src/lib.rs:86` |
| `api.post.runs` | `POST /api/lightagent/v1/runs` | applicable | no | (backend reuse) | api: POST /runs | not-started | `crates/lightagent-api/src/lib.rs:90` |
| `api.get.runs.id` | `GET /api/lightagent/v1/runs/{id}` | applicable | no | (backend reuse) | api: GET /runs/{id} | not-started | `crates/lightagent-api/src/lib.rs:91` |
| `api.get.runs.id.events` | `GET /api/lightagent/v1/runs/{id}/events` | applicable | no | (backend reuse) | api: GET /runs/{id}/events | not-started | `crates/lightagent-api/src/lib.rs:92` |
| `api.post.runs.id.cancel` | `POST /api/lightagent/v1/runs/{id}/cancel` | applicable | no | (backend reuse) | api: POST /runs/{id}/cancel | not-started | `crates/lightagent-api/src/lib.rs:93` |
| `api.post.runs.id.continue` | `POST /api/lightagent/v1/runs/{id}/continue` | applicable | no | (backend reuse) | api: POST /runs/{id}/continue | not-started | `crates/lightagent-api/src/lib.rs:160` |
| `api.get.extensions` | `GET /api/lightagent/v1/extensions` | applicable | no | (backend reuse) | shared: lightagent_extensions::manage::list | not-started | `crates/lightagent-api/src/lib.rs:131` |
| `api.post.extensions` | `POST /api/lightagent/v1/extensions` | applicable | no | (backend reuse) | shared: lightagent_extensions::manage::install | not-started | `crates/lightagent-api/src/lib.rs:131` |
| `api.get.extensions.name` | `GET /api/lightagent/v1/extensions/{name}` | applicable | no | (backend reuse) | shared: lightagent_extensions::manage::show | not-started | `crates/lightagent-api/src/lib.rs:135` |
| `api.delete.extensions.name` | `DELETE /api/lightagent/v1/extensions/{name}` | applicable | no | (backend reuse) | shared: lightagent_extensions::manage::uninstall | not-started | `crates/lightagent-api/src/lib.rs:135` |
| `api.put.extensions.name.enabled` | `PUT /api/lightagent/v1/extensions/{name}/enabled` | applicable | no | (backend reuse) | shared: lightagent_extensions::manage::set_enabled | not-started | `crates/lightagent-api/src/lib.rs:139` |
| `api.get.mcp` | `GET /api/lightagent/v1/mcp` | applicable | no | (backend reuse) | shared: lightagent_core::mcp_servers::view | not-started | `crates/lightagent-api/src/lib.rs:144` |
| `api.put.mcp` | `PUT /api/lightagent/v1/mcp` | applicable | no | (backend reuse) | shared: lightagent_core::mcp_servers::set | not-started | `crates/lightagent-api/src/lib.rs:144` |
| `api.post.mcp.servers` | `POST /api/lightagent/v1/mcp/servers` | applicable | no | (backend reuse) | shared: lightagent_core::mcp_servers::add | not-started | `crates/lightagent-api/src/lib.rs:144` |
| `api.put.mcp.servers.name` | `PUT /api/lightagent/v1/mcp/servers/{name}` | applicable | no | (backend reuse) | shared: lightagent_core::mcp_servers::update | not-started | `crates/lightagent-api/src/lib.rs:144` |
| `api.delete.mcp.servers.name` | `DELETE /api/lightagent/v1/mcp/servers/{name}` | applicable | no | (backend reuse) | shared: lightagent_core::mcp_servers::remove | not-started | `crates/lightagent-api/src/lib.rs:144` |
| `api.post.mcp.servers.name.check` | `POST /api/lightagent/v1/mcp/servers/{name}/check` | applicable | no | (backend reuse) | shared: chat::check_mcp_server via RunFactory::check_mcp | not-started | `crates/lightagent-api/src/lib.rs:150` |
| `api.post.skills.imports` | `POST /api/lightagent/v1/skills/imports` | applicable | no | Tools: Installed skills, Import | shared: lightagent_extensions::skill_import::import (new scope) | not-started | `crates/lightagent-api/src/lib.rs:132` |
| `api.get.memories` | `GET /api/lightagent/v1/memories` | applicable | no | (backend reuse) | shared: memory::perform via RunFactory::memory | not-started | `crates/lightagent-api/src/lib.rs:133` |
| `api.post.memories` | `POST /api/lightagent/v1/memories` | applicable | no | (backend reuse) | shared: memory::perform via RunFactory::memory | not-started | `crates/lightagent-api/src/lib.rs:133` |
| `api.delete.memories` | `DELETE /api/lightagent/v1/memories` | applicable | no | (backend reuse) | shared: memory::perform via RunFactory::memory | not-started | `crates/lightagent-api/src/lib.rs:133` |
| `api.get.memories.search` | `GET /api/lightagent/v1/memories/search` | applicable | no | (backend reuse) | shared: memory::perform via RunFactory::memory | not-started | `crates/lightagent-api/src/lib.rs:133` |
| `api.get.memories.reflect` | `GET /api/lightagent/v1/memories/reflect` | applicable | no | (backend reuse) | shared: memory::perform via RunFactory::memory | not-started | `crates/lightagent-api/src/lib.rs:133` |
| `api.get.memories.candidates` | `GET /api/lightagent/v1/memories/candidates` | applicable | no | (backend reuse) | shared: memory::perform via RunFactory::memory | not-started | `crates/lightagent-api/src/lib.rs:133` |
| `api.post.memories.promote` | `POST /api/lightagent/v1/memories/promote` | applicable | no | (backend reuse) | shared: memory::perform via RunFactory::memory | not-started | `crates/lightagent-api/src/lib.rs:133` |
| `api.put.memories.items.id` | `PUT /api/lightagent/v1/memories/items/{id}` | applicable | no | (backend reuse) | shared: memory::perform via RunFactory::memory | not-started | `crates/lightagent-api/src/lib.rs:133` |
| `api.delete.memories.items.id` | `DELETE /api/lightagent/v1/memories/items/{id}` | applicable | no | (backend reuse) | shared: memory::perform via RunFactory::memory | not-started | `crates/lightagent-api/src/lib.rs:133` |
| `api.get.knowledge` | `GET /api/lightagent/v1/knowledge` | applicable | no | (backend reuse) | shared: rag::perform via RunFactory::rag | not-started | `crates/lightagent-api/src/lib.rs:145` |
| `api.post.knowledge` | `POST /api/lightagent/v1/knowledge` | applicable | no | (backend reuse) | shared: rag::perform via RunFactory::rag | not-started | `crates/lightagent-api/src/lib.rs:145` |
| `api.delete.knowledge` | `DELETE /api/lightagent/v1/knowledge` | applicable | no | (backend reuse) | shared: rag::perform via RunFactory::rag | not-started | `crates/lightagent-api/src/lib.rs:145` |
| `api.get.knowledge.search` | `GET /api/lightagent/v1/knowledge/search` | applicable | no | (backend reuse) | shared: rag::perform via RunFactory::rag | not-started | `crates/lightagent-api/src/lib.rs:145` |
| `api.put.workers.role` | `PUT /api/lightagent/v1/workers/{role}` | applicable | no | (backend reuse) | shared: worker_assignment via RunFactory::workers_change | not-started | `crates/lightagent-api/src/lib.rs:85` |
| `api.delete.workers.role` | `DELETE /api/lightagent/v1/workers/{role}` | applicable | no | (backend reuse) | shared: worker_assignment via RunFactory::workers_change | not-started | `crates/lightagent-api/src/lib.rs:85` |
| `api.get.sessions` | `GET /api/lightagent/v1/sessions` | applicable | no | (backend reuse) | api: GET /sessions | not-started | `crates/lightagent-api/src/lib.rs:94` |
| `api.post.sessions` | `POST /api/lightagent/v1/sessions` | applicable | no | (backend reuse) | api: POST /sessions | not-started | `crates/lightagent-api/src/lib.rs:94` |
| `api.get.runs` | `GET /api/lightagent/v1/runs` | applicable | no | (backend reuse) | api: GET /runs | not-started | `crates/lightagent-api/src/lib.rs:90` |
| `api.post.profiles` | `POST /api/lightagent/v1/profiles` | applicable | no | (backend reuse) | api: POST /profiles | not-started | `crates/lightagent-api/src/lib.rs:85` |
| `api.get.profiles.id` | `GET /api/lightagent/v1/profiles/{id}` | applicable | no | (backend reuse) | api: GET /profiles/{id} | not-started | `crates/lightagent-api/src/lib.rs:85` |
| `api.delete.profiles.id` | `DELETE /api/lightagent/v1/profiles/{id}` | applicable | no | (backend reuse) | api: DELETE /profiles/{id} | not-started | `crates/lightagent-api/src/lib.rs:85` |
| `api.put.profiles.id.active` | `PUT /api/lightagent/v1/profiles/{id}/active` | applicable | no | (backend reuse) | api: PUT /profiles/{id}/active | not-started | `crates/lightagent-api/src/lib.rs:85` |
| `api.post.profiles.id.preset` | `POST /api/lightagent/v1/profiles/{id}/preset` | applicable | no | (backend reuse) | api: POST /profiles/{id}/preset | not-started | `crates/lightagent-api/src/lib.rs:85` |
| `api.get.sessions.search` | `GET /api/lightagent/v1/sessions/search` | applicable | yes | Conversations: search (already in WebUI) | api: GET /sessions/search | not-started | `crates/lightagent-api/src/lib.rs:98` |
| `api.get.sessions.id` | `GET /api/lightagent/v1/sessions/{id}` | applicable | no | (backend reuse) | api: GET /sessions/{id} | not-started | `crates/lightagent-api/src/lib.rs:99` |
| `api.patch.sessions.id` | `PATCH /api/lightagent/v1/sessions/{id}` | applicable | yes | Conversations: rename/pin (already in WebUI) | api: PATCH /sessions/{id} | not-started | `crates/lightagent-api/src/lib.rs:99` |
| `api.delete.sessions.id` | `DELETE /api/lightagent/v1/sessions/{id}` | applicable | no | (backend reuse) | api: DELETE /sessions/{id} | not-started | `crates/lightagent-api/src/lib.rs:99` |
| `api.post.sessions.id.attachments` | `POST /api/lightagent/v1/sessions/{id}/attachments` | applicable | yes | Conversations: attach file (already in WebUI) | api: POST /sessions/{id}/attachments | not-started | `crates/lightagent-api/src/lib.rs:105` |
| `api.post.sessions.id.regenerate` | `POST /api/lightagent/v1/sessions/{id}/regenerate` | applicable | yes | Conversations: Try again under the final answer (WebUI, #98) | api: POST /sessions/{id}/regenerate | not-started | `crates/lightagent-api/src/lib.rs:122` |
| `api.get.config` | `GET /api/lightagent/v1/config` | applicable | no | (backend reuse) | api: GET /config | not-started | `crates/lightagent-api/src/lib.rs` |
| `api.get.config.key` | `GET /api/lightagent/v1/config/{key}` | applicable | no | (backend reuse) | api: GET /config/{key} | not-started | `crates/lightagent-api/src/lib.rs` |
| `api.get.jev.routing-report` | `GET /api/lightagent/v1/jev/routing-report` | applicable | no | (backend reuse) | api: GET /jev/routing-report | not-started | `crates/lightagent-api/src/lib.rs` |
| `api.post.provider.check` | `POST /api/lightagent/v1/provider/check` | applicable | no | (backend reuse) | api: POST /provider/check | not-started | `crates/lightagent-api/src/lib.rs` |
| `api.get.provider.credential` | `GET /api/lightagent/v1/provider/credential` | applicable | no | (backend reuse) | api: GET /provider/credential | not-started | `crates/lightagent-api/src/lib.rs` |
| `api.put.provider.credential` | `PUT /api/lightagent/v1/provider/credential` | applicable | no | (backend reuse) | api: PUT /provider/credential | not-started | `crates/lightagent-api/src/lib.rs` |
| `api.delete.provider.credential` | `DELETE /api/lightagent/v1/provider/credential` | applicable | no | (backend reuse) | api: DELETE /provider/credential | not-started | `crates/lightagent-api/src/lib.rs` |
| `api.get.providers` | `GET /api/lightagent/v1/providers` | applicable | no | (backend reuse) | api: GET /providers | not-started | `crates/lightagent-api/src/lib.rs` |
| `api.post.providers` | `POST /api/lightagent/v1/providers` | applicable | no | (backend reuse) | api: POST /providers | not-started | `crates/lightagent-api/src/lib.rs` |
| `api.get.providers.id` | `GET /api/lightagent/v1/providers/{id}` | applicable | no | (backend reuse) | api: GET /providers/{id} | not-started | `crates/lightagent-api/src/lib.rs` |
| `api.patch.providers.id` | `PATCH /api/lightagent/v1/providers/{id}` | applicable | no | (backend reuse) | api: PATCH /providers/{id} | not-started | `crates/lightagent-api/src/lib.rs` |
| `api.delete.providers.id` | `DELETE /api/lightagent/v1/providers/{id}` | applicable | no | (backend reuse) | api: DELETE /providers/{id} | not-started | `crates/lightagent-api/src/lib.rs` |
| `api.post.providers.id.check` | `POST /api/lightagent/v1/providers/{id}/check` | applicable | no | (backend reuse) | api: POST /providers/{id}/check | not-started | `crates/lightagent-api/src/lib.rs` |
| `api.put.provider.active` | `PUT /api/lightagent/v1/provider/active` | applicable | no | (backend reuse) | api: PUT /provider/active | not-started | `crates/lightagent-api/src/lib.rs` |
| `api.put.provider.model` | `PUT /api/lightagent/v1/provider/model` | applicable | no | (backend reuse) | api: PUT /provider/model | not-started | `crates/lightagent-api/src/lib.rs` |
| `api.get.workspace` | `GET /api/lightagent/v1/workspace` | applicable | no | (backend reuse) | api: GET /workspace | not-started | `crates/lightagent-api/src/lib.rs` |
| `api.put.workspace` | `PUT /api/lightagent/v1/workspace` | applicable | no | (backend reuse) | api: PUT /workspace | not-started | `crates/lightagent-api/src/lib.rs` |
| `api.put.config.key` | `PUT /api/lightagent/v1/config/{key}` | applicable | no | (backend reuse) | api: PUT /config/{key} | not-started | `crates/lightagent-api/src/lib.rs` |
| `api.get.approvals` | `GET /api/lightagent/v1/approvals` | applicable | no | (backend reuse) | api: GET /approvals | not-started | `crates/lightagent-api/src/lib.rs:109` |
| `api.post.approvals.run` | `POST /api/lightagent/v1/approvals/{run}` | applicable | no | (backend reuse) | api: POST /approvals/{run} | not-started | `crates/lightagent-api/src/lib.rs:110` |
| `api.provisioning.put.profiles` | `PUT /api/lightagent/v1/provisioning/profiles/{profile_id}` | not-applicable (D1-Q3) | no | D1-Q3: compiled only with the provisioning-api feature, which no release build enables; remote capability listing deferred | api (feature provisioning-api) | not-started | `crates/lightagent-api/src/provisioning.rs:53` |
| `api.provisioning.get.operations` | `GET /api/lightagent/v1/provisioning/operations/{operation_id}` | not-applicable (D1-Q4) | no | D1-Q4: as D1-Q3 | api (feature provisioning-api) | not-started | `crates/lightagent-api/src/provisioning.rs:57` |
| `api.provisioning.post.operations` | `POST /api/lightagent/v1/provisioning/operations/{operation_id}/revoke` | not-applicable (D1-Q5) | no | D1-Q5: as D1-Q3 | api (feature provisioning-api) | not-started | `crates/lightagent-api/src/provisioning.rs:61` |
