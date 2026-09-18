# Changelog

All notable Lightagent changes are documented here.

## [Unreleased]

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
