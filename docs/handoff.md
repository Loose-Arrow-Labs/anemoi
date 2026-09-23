# Handoff

## V1 Scope

Anemoi v1 is a small local inference governance daemon:

```text
config file in
runtime snapshots in
decision out
explanation out
optional JSONL log out
```

No database is required. SQLite is optional and enabled with
`ANEMOI_DATABASE_URL=sqlite://...`. Current caveats are centralized in
[Known Limitations](LIMITATIONS.md).

## Validation

Last local validation run:

```powershell
cargo fmt --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Status: passing.

Last smoke run:

```powershell
cargo run -p anemoi-cli -- decide --domain coding --latency-budget-ms 1500
GET  http://127.0.0.1:7071/health
GET  http://127.0.0.1:7071/status
GET  http://127.0.0.1:7071/residents
GET  http://127.0.0.1:7071/openapi.json
POST http://127.0.0.1:7071/decide
POST http://127.0.0.1:7071/execute
```

Status: passing. The example config starts the mock runtime with `qwen9b`
resident as `hot_gpu`; an interactive coding request with a 1500 ms latency
budget selects `qwen9b`, stages `qwen35_a3b`, and reports
`full_inference_forwarded: false` from `/execute`.

Browser preview note: PowerShell HTTP checks pass on `127.0.0.1:7071`, but the
Codex in-app browser automation reported `net::ERR_BLOCKED_BY_CLIENT` when
opening `http://127.0.0.1:7071/health` and `http://localhost:7071/health`.

## Needs Validation

See [Known Limitations](LIMITATIONS.md) for the current centralized list.
