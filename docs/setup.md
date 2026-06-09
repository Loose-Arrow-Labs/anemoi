# Setup

Anemoi v1 is a local-first Rust governance daemon.

```text
Anemoi decides.
Runtimes execute.
```

The v1 mock-config demo needs Rust and does not require Ollama, llama.cpp,
llama-swap, SQLite, or a cloud provider.

## Local Prerequisites

- Rust toolchain with `cargo`
- PowerShell or another shell that can run local commands

Optional for later runtime inspection:

- Ollama
- llama-swap
- llama.cpp server

## Configuration

Default config:

```text
config/anemoi.example.yaml
```

Environment variables:

```powershell
$env:ANEMOI_CONFIG = "config/anemoi.example.yaml"
$env:ANEMOI_BIND = "127.0.0.1:7070"
$env:ANEMOI_DECISION_LOG = "logs/anemoi-decisions.jsonl"
```

`ANEMOI_DECISION_LOG` is optional. Without it, recent decisions stay in process
memory only.

## Run The Daemon

```powershell
cargo run -p anemoi-daemon
```

The daemon binds to `127.0.0.1:7070` by default.

Useful endpoints:

```text
GET  /health
GET  /status
GET  /residents
POST /decide
POST /execute
GET  /decisions/:id
GET  /explain/:id
GET  /openapi.json
```

In v1, `/execute` returns a decision plus a handoff status. It may request a
model load, but it does not forward full inference and reports
`full_inference_forwarded: false`.

## Run The CLI

```powershell
cargo run -p anemoi-cli -- status
cargo run -p anemoi-cli -- policy check
cargo run -p anemoi-cli -- residents
cargo run -p anemoi-cli -- runtimes
cargo run -p anemoi-cli -- decide --domain coding --latency-budget-ms 1500
```

With the example config, the mock runtime starts with `qwen9b` hot. The decide
command should select `qwen9b` immediately and stage `qwen35_a3b` in the
background.

## Validate

```powershell
cargo fmt --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

If `clippy` is not installed:

```powershell
rustup component add clippy
```

## Deferred

- Full inference forwarding
- Provider gateway behavior
- Cloud execution
- Database-backed analytics
