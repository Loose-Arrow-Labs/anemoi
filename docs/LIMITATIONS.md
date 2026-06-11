# Anemoi Known Limitations

This document centralizes the caveats that matter before public beta use.
Status words are intentional:

- `Stable`: implemented, tested, and reachable through the intended surface.
- `Beta`: implemented and tested, but live/operator hardening is still in progress.
- `Base implementation`: useful code exists, but the production/operator path is incomplete.
- `Needs validation`: a real runtime, DNS route, secret, or operator-owned environment is required before claiming readiness.
- `Not implemented`: no current product surface.
- `Out of scope`: intentionally not owned by Anemoi v1.

## Limitations Matrix

| Surface | Status | Limitation | Follow-up |
|---|---|---|---|
| Mock runtime demo | Stable | The mock path is the reliable local demo path. It proves decisions, explanations, staging, CLI status, gateway mock forwarding, and telemetry headers. | None. |
| Live runtime validation | Needs validation | Fixture tests cover live adapters, but this checkout does not yet include a sanitized, repeatable real llama-swap validation path. | #133 |
| llama-swap residency evidence | Beta | `/v1/models` and configured catalog data are not proof that a model is resident. Residency should come from observed runtime state, especially `/api/events` when available. | #133 |
| llama-swap matrix colocation | Base implementation | Matrix parsing and `can_colocate` are tested in `anemoi-runtime`, but policy `decide()` does not yet use the matrix to score or reject candidates. | #112 |
| Ollama adapter | Base implementation | Ollama inspection via `/api/ps` is fixture-tested. Anemoi does not load, unload, or pull Ollama models in v1. | Needs live read-only validation if used. |
| llama.cpp adapter | Base implementation | llama.cpp is inspect-only (`/health` plus `/v1/models`) and does not support live load/unload/execute through Anemoi. | `docs/live_validation/llama-cpp-probe.md` |
| Live execution gate | Stable | Live mutation and non-mock forwarding require `ANEMOI_ENABLE_LIVE_EXECUTE=1`. Without it, Anemoi records blocked/skipped staging instead of mutating runtimes. | Keep this gate in every mutating path. |
| Quality floor escalation | Beta | Gateway metadata is accepted on `main`, but policy enforcement lands in #137. Until that merges, a `32b` request can still stay on the fast 9B path. | #137 |
| Concurrent transition coordination | Base implementation | The policy crate has transition lease/fencing behavior, but daemon scheduling does not yet route live transitions through that coordinator. | #79 |
| Docker/DNS deployment | Needs validation | The Rust daemon uses port 7070, but current compose/deployment docs are still being corrected. Treat `anemoi.home.arpa` as pending until validated end to end. | #131 |
| Telemetry JSON endpoints | Not implemented | `/telemetry/*` read-only JSON endpoints do not exist yet. | #130 |
| Web dashboard | Not implemented | `/dashboard` and the Vite TypeScript dashboard do not exist yet. | #130, #138 |
| SQLite event store | Beta | SQLite durable events work when `ANEMOI_DATABASE_URL=sqlite://...` is configured. Retention, compaction, backup/restore automation, and query UX are still operator-owned. | Future release policy |
| JSONL decision log | Stable | JSONL records decisions and reloads them. It does not persist rich event history such as resident transitions or action-plan events. | Use SQLite for event history. |
| MCP surface | Base implementation | `anemoi-mcp` exposes a tested library service, but there is no standalone MCP server binary in this checkout. | Add server wiring if MCP distribution is required. |
| Security boundary | Beta | The daemon is local-first and binds to loopback by default. LAN/DNS exposure should be behind an operator-controlled reverse proxy or trusted network; Anemoi is not an auth gateway in v1. | #131 |
| Legacy .NET residue | Broken | Legacy `.NET`/C# files and old appsettings deployment assumptions remain on `main` until the cleanup PR lands. They are not the active product surface. | #129 |
| README and public docs | Beta | Public docs contain useful commands, but some claims are stale or too broad for beta readiness. This page is the source of truth for limitations until the README rewrite lands. | #132 |

## Out Of Scope For Core v1

Anemoi does not own model execution internals, model weights, prompt planning,
agent memory, retrieval, training, provider account management, or direct
runtime infrastructure mutation beyond explicit, gated load/unload handoff.

Runtimes execute. Anemoi decides.
