# Anemoi Build Prompts

This folder contains ordered implementation prompts for building Anemoi with
test-driven development.

These prompts are historical build records. For current beta caveats and
implemented-vs-pending status, see [Known Limitations](../LIMITATIONS.md).

Each prompt should produce one reviewable change. Tests must be single-scoped:
one behavior, one rule, one reason to fail. Do not combine unrelated behavior
unless the prompt explicitly names an end-to-end smoke test.

## Phase Policy

Phase one does not use SQLite or any required database.

Use:

- in-memory recent decisions
- structured tracing
- optional append-only JSONL decision logs

Defer database-backed analytics until Anemoi has proven the core governance
loop.

## Prompt Order

| Prompt | Purpose |
|---|---|
| `00-v1-scope-no-database.md` | Lock v1 scope and document the no-database phase-one decision. |
| `01-config-validation.md` | Validate YAML config references and diagnostics. |
| `02-core-domain-contracts.md` | Harden core domain types and serialization contracts. |
| `03-runtime-adapter-contract.md` | Test and tighten the runtime adapter boundary. |
| `04-mock-runtime-snapshots.md` | Make mock runtime behavior deterministic for policy tests. |
| `05-policy-candidate-generation.md` | Generate candidates from domain, roster, group, model, runtime. |
| `06-policy-continuity-staging.md` | Prove hot-worker fallback and background staging behavior. |
| `07-telemetry-memory-jsonl.md` | Replace database assumptions with memory plus optional JSONL logging. |
| `08-daemon-decision-api.md` | Expose health, status, residents, decide, and explain endpoints. |
| `09-cli-operator-loop.md` | Make CLI commands useful for the first operator loop. |
| `10-llama-swap-inspection.md` | Add real llama-swap inspection without execution forwarding. |
| `11-ollama-inspection.md` | Add real Ollama inspection with HTTP fixtures. |
| `12-openapi-contract.md` | Publish and test the OpenAPI contract. |
| `13-mcp-minimum-surface.md` | Add the minimum MCP tools/resources after core API stabilizes. |
| `14-hardening-release-checklist.md` | Finish docs, validation, security, and release readiness. |
| `15-live-validation-safety-plan.md` | Define the read-only live validation protocol and operator inputs. |
| `16-llama-swap-readonly-probe.md` | Probe live llama-swap endpoints without loading, unloading, or restarting anything. |
| `17-live-runtime-config-profile.md` | Add a sanitized live config profile for the first validated runtime target. |
| `18-residency-truth-contract.md` | Decide what runtime evidence can honestly prove resident, hot, configured, or unknown state. |
| `19-live-decision-smoke.md` | Run the first read-only live decision smoke against real runtime snapshots. |
| `20-controlled-execution-gate.md` | Define the explicit approval gate for any future load/unload or execution handoff validation. |
| `21-runtime-reconciliation-loop.md` | Maintain a fresh observed runtime snapshot cache without mutating runtimes. |
| `22-background-staging-worker.md` | Turn background staging recommendations into observable staging intents. |
| `23-load-unload-action-plan.md` | Insert an explicit dry-run/action-plan layer before runtime mutation. |
| `24-resource-pressure-model.md` | Score candidates with explicit VRAM, RAM, KV, load, and active-request pressure. |
| `25-eviction-and-pinning-policy.md` | Protect keep-hot workers and produce explainable eviction plans. |
| `26-operator-status-surface.md` | Make status useful as an operator-facing control-plane surface. |
| `27-durable-event-store.md` | Add optional durable event history once the governance loop is proven. |

## Standard Validation

Run when Rust tooling is available:

```powershell
cargo fmt --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

If tooling is unavailable, report it clearly and do not claim validation passed.
