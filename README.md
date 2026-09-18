# Lightagent

Lightagent is a standalone agent harness for local or remote language-model
providers. It owns the agent loop, tools, approvals, profiles, sessions, durable
memory, skills, extensions, MCP, ACP, a terminal UI, and a browser UI. It does
not load models, estimate inference memory, supervise inference engines, or
implement an inference gateway.

Inference is a provider boundary. Lightagent speaks HTTP to an
OpenAI-compatible endpoint; [Lightweight](https://github.com/dlroqa/Lightweight)
is one supported local provider, not a library or workspace dependency.

## Quick install and Web UI

> [!NOTE]
> Lightagent needs a running OpenAI-compatible inference provider. Its default
> provider address is `http://127.0.0.1:11434`.

### Install a prebuilt release

Download and extract the archive for your platform from
[Lightagent Releases](https://github.com/dlroqa/Lightagent/releases), then run
these commands from the extracted directory:

```sh
./lightagent init
./lightagent setup provider
./lightagent doctor
./lightagent serve --web-root ./web
```

### Install from source

Building from source requires Rust 1.98 or newer and Node.js:

```sh
git clone https://github.com/dlroqa/Lightagent.git
cd Lightagent

cargo install --path crates/lightagent --locked

npm ci --prefix frontend
npm run build --prefix frontend

lightagent init
lightagent setup provider
lightagent doctor
lightagent serve --web-root "$PWD/frontend/dist"
```

If the `lightagent` command is not found after installation, add
`~/.cargo/bin` to your `PATH`.

Once the server is running, open these local addresses:

- Web UI: <http://127.0.0.1:8735/>
- API: <http://127.0.0.1:8735/api/lightagent/v1>
- Health check: <http://127.0.0.1:8735/health>

API keys are stored as environment-variable references, not literal secrets.
Use `lightagent setup`, `lightagent profile`, and `lightagent config --help` for
additional runtime configuration.

## Terminal harness

Running `lightagent` or `lightagent chat` starts a streaming terminal session.
The harness provides:

- multi-turn, persisted sessions and explicit session resume;
- bounded agent turns, tool calls, output size, and wall time;
- risk-classified tool approvals and per-profile policy;
- confined filesystem and terminal tools;
- web search/fetch and one-call realtime retrieval;
- durable memory with review, correction, and forgetting;
- skills, installable extensions, MCP servers, and ACP editor integration;
- delegated worker profiles and provider-supplied reasoning display.

Useful commands:

```sh
lightagent sessions
lightagent chat --session <id>
lightagent tools list
lightagent memory reflect
lightagent extensions list
lightagent acp
```

See [permissions](docs/permissions.md), [memory](docs/memory.md), and
[extensions](docs/extensions.md) for the detailed contracts.

## Standalone Web UI

The Web UI is served by Lightagent itself; no inference-gateway proxy is part of
the architecture.

```sh
npm ci --prefix frontend
npm run build --prefix frontend
lightagent serve --web-root frontend/dist
```

Open the address printed by `lightagent serve`. The UI exposes agent sessions,
live tool calls and approvals, the runtime tool catalog, harness limits,
capability switches, memory policy, and appearance settings. The API remains at
`/api/lightagent/v1` and `/health` reports the harness service.

For development, run `npm run dev --prefix frontend`. Vite proxies only the
Lightagent API and defaults to `http://127.0.0.1:8735`; set
`LIGHTAGENT_DEV_ORIGIN` to use another server.

## HTTP API and service mode

```sh
lightagent serve --host 127.0.0.1 --port 8735 --web-root frontend/dist
```

Loopback binds need no API key. A non-loopback bind is rejected unless
`--key-env` names an environment variable containing a key. The Linux user
service examples live in `packaging/systemd/`.

The versioned API supports sessions, runs, SSE events, cancellation, approvals,
tools, and harness settings. Runs may be stateless or attached to a persisted
session.

## Repository architecture

| Path | Responsibility |
|---|---|
| `crates/lightagent-core` | Provider-neutral agent loop, events, profiles, configuration, limits, permissions, skills |
| `crates/lightagent-tools` | Tool registry, schemas, bounded execution, built-in filesystem, terminal, web, datetime, skill, and delegation tools |
| `crates/lightagent-store` | Owner-private sessions, messages, run metadata, and tool history |
| `crates/lightagent-api` | HTTP + SSE harness API and static Web UI serving |
| `crates/lightagent-provider-lightweight` | HTTP adapter for the OpenAI-compatible provider contract; no Lightweight crate dependency |
| `crates/lightagent-mcp` | MCP client over stdio and streamable HTTP |
| `crates/lightagent-rag` | Lexical, semantic, and realtime retrieval |
| `crates/lightagent-memory` | Durable per-profile memory and reflection tools |
| `crates/lightagent-acp` | ACP server for editor integration |
| `crates/lightagent-extensions` | Installable bundles of skills, MCP declarations, and instructions |
| `crates/lightagent-runtime` | Optional HTTP placement/control client for capable providers |
| `crates/lightagent` | CLI, terminal UI, setup, chat, service wiring, imports |
| `frontend` | Standalone React Web UI for the harness |
| `extensions` | Bundled harness-oriented extension examples |

No `lightweight-*` crate is a workspace member or dependency. The provider
adapter and placement client are protocol clients: they can be changed or
replaced without moving inference code into the harness.

## Build, test, and package

```sh
npm ci --prefix frontend
bash scripts/check.sh
npm ci --prefix e2e
npx --prefix e2e playwright install chromium
bash scripts/render-panel.sh

npm run build --prefix frontend
cargo build --release -p lightagent
bash scripts/package-lightagent.sh
bash scripts/smoke-lightagent-package.sh
```

The archive contains the CLI, `web/`, the license, and Linux service examples.
Run the packaged Web UI with `./lightagent serve --web-root ./web`.

## Security boundary

Lightagent treats provider output, web content, tools, and extension metadata as
untrusted inputs. Tool arguments are schema-validated; filesystem access is
root-confined; redirects are revalidated by web tools; secrets are referenced
through environment variables; logs and error messages avoid prompt and key
contents; and non-loopback API exposure requires explicit authentication.

## License

Apache-2.0. See [LICENSE](LICENSE).
