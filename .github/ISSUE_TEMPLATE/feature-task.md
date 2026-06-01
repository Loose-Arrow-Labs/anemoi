---
name: Feature / Task
about: A scoped task with acceptance criteria
title: "[feat]<scope>: <short title>"
labels: enhancement
assignees: alucero270

---

## Summary
Describe what we are building in 1–3 sentences.

## Context
Why this exists (architecture, phase, dependency). Link to related issues if any.

## Scope

**Allowed:**

- 

**Not allowed / not required:**

- 

## Affected Surfaces

| Crate | Change |
|---|---|
| `anemoi-` | |

## Contract Details

<!--
Types, endpoint shapes, config fields, state machines, or other contracts this task introduces or modifies.
-->

## Architecture Constraints

<!--
Rules this implementation must not violate.
-->

- Domain crates must not perform network I/O.
- Policy scoring belongs in `anemoi-policy`.
- No provider-specific payloads in `anemoi-core`.

## Technical Requirements
- Language/runtime: Rust (edition 2021), async via Tokio
- Logging: `tracing` spans/events for structured output
- Transport: llama-swap HTTP API; auth via `X-API-Key`
- Tests: `#[tokio::test]` unit tests required for new logic; exact names declared below

## Test Expectations

<!--
List exact test function names. These must appear verbatim in `cargo test --workspace` output.
-->

Exact test function names required:

- `test_name_here`

## Acceptance Criteria
- [ ] Implements only the scope described above
- [ ] CI passes (`fmt` + `clippy` + `test` jobs green)
- [ ] Unit tests added/updated for new logic
- [ ] No secrets committed (no tokens/keys/hostnames in repo)

## Validation

```powershell
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## Notes for Codex
Paste any specific constraints, pseudo-code, or examples here.

<!--
- Depends on: issue #N (reason)
- Required by: issue #N (reason)
- Related: #N
-->
