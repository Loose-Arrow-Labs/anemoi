# Prompt 14: Hardening And Release Checklist

## Goal

Prepare Anemoi v1 for use as a small, honest local inference governance daemon.

## Required Tests

Add only focused tests for discovered gaps. Do not add broad brittle tests.

Required checks:

- config validation covers the example config
- policy proof test passes
- daemon API smoke tests pass
- CLI smoke tests pass
- runtime adapter fixture tests pass
- no database is required

## Implementation

Hardening tasks:

- update README with exact v1 capabilities
- update setup docs to Rust commands instead of previous service commands
- document any retired surface clearly or archive it if a maintainer explicitly
  approves
- document environment variables
- document local binding defaults
- document optional JSONL decision log
- document `/execute` limitations
- remove stale SQLite requirements from docs
- ensure errors are actionable
- ensure examples use `config/anemoi.example.yaml`

Security tasks:

- no secrets in docs or example config
- local-first defaults
- no cloud execution defaults
- no raw prompt logging by default

## Acceptance Criteria

- A new operator can run the mock-config demo from README.
- V1 scope is clear.
- Deferred features are explicit.
- Validation commands and results are documented in the handoff.

## Validation

```powershell
cargo fmt --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```
