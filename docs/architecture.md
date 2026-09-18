# Architecture

Lightagent is an agent harness, not an inference engine.

```text
Terminal UI ─┐
Web UI ──────┼─> Lightagent API / CLI wiring
ACP client ──┘              │
                             v
                    provider-neutral agent loop
                      │      │       │
                    tools  sessions  memory
                      │      │       │
                    MCP   approvals  retrieval
                             │
                             v
                  AgentProvider trait (HTTP adapter)
                             │
                             v
               external OpenAI-compatible endpoint
```

The dependency rule is one-way: harness crates may implement protocol clients,
but no inference-engine crate belongs to this workspace. The default provider
adapter uses HTTP/SSE and shares no Rust type or crate with Lightweight.

## Core ownership

`lightagent-core` defines provider, tool, event, profile, permission, and limit
contracts. It contains no HTTP server, terminal UI, filesystem tool, or provider
implementation. The generic agent loop is therefore testable with mock
providers and tools.

`lightagent-tools` owns capability discovery, JSON-schema argument validation,
risk classification, approvals, cancellation, output bounds, workspace
confinement, and built-ins. Tool implementations do not own conversation
history.

`lightagent-store` persists complete sessions and cited tool evidence under the
active profile. `lightagent-memory` is separate: it stores reviewed durable facts
that can cross session boundaries.

`lightagent-api` exposes runs, sessions, approvals, tools, settings, and SSE
events. It can serve the compiled `frontend/` directory from the same origin.
The Web UI does not depend on or proxy through an inference gateway.

## Provider boundary

The `AgentProvider` trait accepts model history and returns canonical streaming
events. The current HTTP adapter resolves available models, translates
OpenAI-compatible SSE, and keeps transport details out of the loop. Provider
selection and credentials are profile configuration.

The crate is historically named `lightagent-provider-lightweight`, but it has no
`lightweight-*` dependency. Lightweight is a supported endpoint. The boundary is
the wire protocol, not a shared implementation.

`lightagent-runtime` is an optional HTTP client for providers exposing placement
or runtime-control endpoints. Its failure cannot move model loading into the
harness; ordinary generation remains available through the provider trait.

## Security and cancellation

Every run has explicit turn, tool-call, output, and optional wall-clock limits.
Cancellation tokens flow through model calls and tool execution. Permission
policy is evaluated before execution, with risk classes providing conservative
defaults. Filesystem paths are resolved under a configured workspace root, web
redirects are revalidated, and secrets are environment references.

Loopback service mode is open by default because the network boundary is local.
Non-loopback service mode requires a key supplied through an environment
variable. The API uses scoped authorization and never accepts a key from the Web
UI bundle.

## Extension boundaries

Skills are instructions discovered from bounded roots. Extensions bundle skills,
MCP declarations, and persona instructions; installation and composition are
owned by `lightagent-extensions`. MCP processes remain external and communicate
through the protocol client. ACP is a separate stdio transport over the same run
manager used by the HTTP API.
