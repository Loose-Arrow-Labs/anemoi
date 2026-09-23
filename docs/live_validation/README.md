# Live Validation

This directory contains procedures for read-only and controlled live validation
of Anemoi against real runtimes.

## Files

| File | Purpose |
|---|---|
| `llama-swap-live-path.md` | End-to-end validation report for one real llama-swap live path: config, inspect, decide, and controlled-execution gate, with sanitized evidence. |
| `safety-plan.md` | Permission boundary, operator inputs, and procedure for read-only live validation. |
| `llama-swap-probe.md` | Read-only HTTP probe procedure for a llama-swap runtime. |
| `controlled-execution-gate.md` | The `ANEMOI_ENABLE_LIVE_EXECUTE` boundary for non-mock mutation and forwarding. |
| `residency-truth-contract.md` | Why configured models are not residency evidence. |

## Phase Policy

Live validation follows a strict phase policy:

1. **Read-only inspection** (prompts 15-19): HTTP GET probes, decision smoke
   tests, no runtime mutation.
2. **Controlled execution** (prompt 20+): Requires explicit opt-in, approval,
   and documented rollback plans before any load/unload or inference handoff.

No live runtime command may be run without explicit user approval in the current
task.
