# Anemoi Feature Validation Report

Issue: #127

Date: 2026-06-09

Base commit validated: `128615c801ce23f0ef366f9902ac5c44e5561137`

## Status Definitions

| Status | Meaning |
|---|---|
| Fully operational | Reachable from the real daemon, CLI, gateway, or crate API; covered by behavioral tests or smoke checks; no known blocking caveat in the validated surface. |
| Base implementation | Code and tests exist, but live wiring, operator workflow, production path, or cross-process behavior is incomplete. |
| Broken | A reachable path is stale, contradicts docs, ignores accepted input, or cannot work as documented. |
| Needs validation | Live runtime, DNS, secret, operator approval, or missing evidence prevents an honest pass/fail claim. |

## Evidence Collected

Validation commands run on 2026-06-09:

```powershell
cargo fmt --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo run -p anemoi-guard -- crates
```

Result: all passed.

Mock CLI smoke:

```powershell
cargo run -p anemoi-cli -- status
cargo run -p anemoi-cli -- residents
cargo run -p anemoi-cli -- decide --domain coding --latency-budget-ms 1500
cargo run -p anemoi-cli -- policy check
```

Result: all passed. `decide` selected `qwen9b`, returned
`StageBackground`, and staged `qwen35_a3b`.

Mock daemon smoke used the real `target/debug/anemoi-daemon.exe` binary with:

```powershell
$env:ANEMOI_BIND = "127.0.0.1:17070"
```

Smoke results:

| Endpoint | Result |
|---|---|
| `GET /health` | `{"ok":true}` |
| `GET /status` | one runtime |
| `GET /residents` | one resident |
| `GET /v1/models` | governance domain `coding` |
| `POST /v1/chat/completions` | HTTP 200, `x-anemoi-selected-model: qwen9b`, `x-anemoi-action: stage_background`, SSE mock response |

No live runtime mutation was performed. No real llama-swap, Ollama, or
llama.cpp host was contacted.

Static checks:

- `crates/anemoi-daemon/src/lib.rs` routes `/health`, `/status`,
  `/residents`, `/decide`, `/execute`, `/decisions/:id`, `/explain/:id`,
  `/staging`, `/v1/models`, `/v1/chat/completions`, and `/openapi.json`.
- No `/telemetry/*` or `/dashboard` route exists on `main`.
- `deploy/docker/docker-compose.yml` still maps `8080:8080` and mounts
  `deploy/config/appsettings.example.json`, which is legacy deployment drift.
- `git ls-files -- '*.cs' '*.csproj' 'Anemoi.sln' 'deploy/config/appsettings.example.json'`
  reports 44 tracked legacy files.

## Readiness Matrix

| Feature | Surface | Evidence checked | Status | What works | What breaks | Follow-up |
|---|---|---|---|---|---|---|
| Domain, model, runtime, residency group types | `anemoi-core` | `accepts_example_config`, serialization tests | Fully operational | Core domain objects deserialize, validate, and round-trip through decisions. | No break found in mock path. | None. |
| Residency vocabulary | `anemoi-core` | `serializes_residency_state_as_snake_case`, runtime adapter tests | Fully operational | Established states serialize as expected and adapters map runtime evidence into them. | Unknown states still require adapter-specific handling. | Keep state vocabulary controlled. |
| Config loading and env expansion | `anemoi-core`, daemon, CLI | `expand_env_vars_*`, `live_config_uses_environment_for_auth`, CLI `policy check` smoke | Fully operational | YAML config, validation diagnostics, and environment variable expansion work. | CLI `policy check --config ...` is invalid; config path is a top-level CLI option (`--config ... policy check`). | Fix stale command examples where present. |
| Static rosters | `anemoi-core`, `anemoi-policy` | `generates_candidates_for_domain_rosters`, CLI/daemon smoke | Fully operational | Static domain roster to group to model scheduling works. | None in mock path. | None. |
| `live_roster` config | `anemoi-core`, `anemoi-policy` | `accepts_domain_with_live_roster_and_no_static_rosters`, `live_roster_generates_candidates_from_configured_models` | Base implementation | Policy can generate candidates from runtime `configured_models`. | Live runtime behavior still needs real llama-swap validation. | #133. |
| Example mock profile | `config/anemoi.example.yaml` | full tests, CLI smoke, daemon smoke | Fully operational | Starts with mock `qwen9b` hot and can decide/stage `qwen35_a3b`. | None in mock path. | None. |
| Prometheus llama-swap profile | `config/anemoi.prometheus.yaml` | static scan only | Needs validation | Defines real llama-swap env vars and fast coding models. | On `main`, `coding` is fast-only, so escalation beyond 9B is not naturally available; #141 changes this. | #137/#141, #133. |
| Candidate generation | `anemoi-policy` | `generates_candidates_for_domain_rosters`, `candidate_order_is_deterministic` | Fully operational | Produces deterministic candidates with group/runtime/profile data. | None in mock path. | None. |
| Deterministic scoring and tie-breaking | `anemoi-policy` | `decide_score_tie_*`, score contribution tests | Fully operational | Stable config order breaks score ties deterministically. | None found. | None. |
| Hot resident reuse | `anemoi-policy`, daemon | `candidate_includes_available_supported_runtime`, daemon smoke | Fully operational | Hot `qwen9b` is reused and explained. | None in mock path. | None. |
| Cold-load avoidance and continuity staging | `anemoi-policy`, daemon, CLI | `avoids_cold_large_model_when_small_worker_is_hot`, `records_background_model_in_decision`, CLI smoke | Fully operational | Tight latency budget selects hot worker and stages larger model. | Live staging still gated for real runtimes. | #133 for live proof. |
| `quality_floor` scheduling | gateway parse to policy | `inference_gateway_accepts_anemoi_selection_metadata`; static scan | Broken | Gateway parses `anemoi.quality_floor.minimum_parameter_class`. | On `main`, policy does not enforce the floor; a request can still stay on 9B. | #137/#141 fixes this. |
| Context-window fit/rejection | `anemoi-policy`, gateway | `context_window_fit_rejects_candidate_too_small_for_request`, `inference_gateway_large_context_request_selects_larger_context_model` | Fully operational | Known-too-small context windows are rejected and larger context candidates can win. | Unknown prompt size intentionally does not reject. | None. |
| Resource pressure scoring | `anemoi-policy` | `pressure_model_*`, `active_request_pressure_penalizes_busy_runtime` | Fully operational | VRAM/RAM/active-request pressure affects scores and explanations. | KV/cache pressure depends on runtime evidence quality. | Live adapter evidence validation. |
| Eviction and pinning policy | `anemoi-policy`, daemon | `eviction_plan_*`, `live_eviction_requires_explicit_enable_flag` | Fully operational | Pinned/serving models are protected and live eviction is gated. | Live unload remains opt-in and not smoke-tested here. | #133 or separate live eviction validation. |
| Concurrent transition coordinator | `anemoi-policy::transition` | `same_model_request_joins_existing_transition`, `stale_owner_cannot_complete_after_takeover`, `every_transition_decision_carries_explanation` | Base implementation | Policy crate has lease/fencing/collision behavior with strong tests. | Static scan found no daemon call to `TransitionCoordinator::request_transition`; not real multi-instance protection yet. | Wire into daemon before advertising as operational. |
| Resident transition emission | daemon + telemetry | `resident_transitions_detects_first_observation_change_and_skips_unchanged`, `reconciliation_tick_records_resident_transition` | Fully operational | Reconciliation emits SQLite resident events with evidence source. | Vanished residents are intentionally not recorded because target state is unobserved. | None. |
| Mock runtime adapter | `anemoi-runtime` | `mock_runtime_*`, CLI/daemon smoke | Fully operational | Inspect/load/unload/execute and active request accounting work for offline development. | None found. | None. |
| Ollama adapter | `anemoi-runtime` | `ollama_ps_*`, `ollama_load_model_is_unsupported` | Base implementation | Read-only `/api/ps` inspection is fixture-tested; load/unload are explicitly unsupported. | No real Ollama smoke in this pass. | Live read-only validation if Ollama becomes target. |
| llama.cpp adapter | `anemoi-runtime` | `llama_cpp_*`, `llama_cpp_load_unload_execute_are_unsupported` | Base implementation | Inspect-only health/model catalog behavior is fixture-tested. | No real llama.cpp server smoke in this pass; no mutation support. | `docs/live_validation/llama-cpp-probe.md`. |
| llama-swap health/model inspection | `anemoi-runtime` | `llama_swap_health_*`, `llama_swap_inspect_*` | Base implementation | Health, model normalization, configured model evidence, auth, timeout, and flaky endpoint behavior are fixture-tested. | Real llama-swap host not contacted in this pass. | #133. |
| llama-swap `/api/events` residency cache | `anemoi-runtime`, daemon | `parse_model_status_payload_*`, `sse_decoder_*`, `llama_swap_inspect_reports_residents_from_event_cache`, `reconcile_ready_completes_only_the_ready_models_intent` | Base implementation | SSE parsing and readiness-driven staging completion are fixture-tested. | Needs real event stream validation. | #133. |
| llama-swap matrix parsing | `anemoi-runtime` | `matrix_parses_vars_evict_costs_and_sets`, `adapter_can_colocate_uses_matrix` | Base implementation | Matrix DSL is parsed and `can_colocate` works in adapter tests. | `decide()` does not consult matrix colocation yet; #112 remains open. | #112. |
| Runtime forwarding and auth injection | `anemoi-runtime`, daemon gateway | `inference_gateway_injects_runtime_auth_token`, gateway tests | Base implementation | Forwarding code and mock gateway path work; non-mock forwarding is gated. | No live non-mock forward validated. | #133. |
| `GET /health` | daemon | `health_returns_ok`, daemon smoke | Fully operational | Real binary returned `{"ok":true}`. | None found. | None. |
| `GET /status` | daemon | `status_*`, daemon smoke | Fully operational | Reports runtime availability/freshness, groups, recent decisions, staging, policy warnings. | No browser/dashboard surface yet. | #130. |
| `GET /residents` | daemon | `residents_returns_runtime_snapshots`, daemon smoke | Fully operational | Returns normalized runtime snapshots. | Live residency truth depends on adapters/events. | #133. |
| `POST /decide` | daemon | `decide_returns_structured_decision`, `decide_records_decision_in_log` | Fully operational | Returns structured decision and records telemetry. | `quality_floor` is not enforced on `main`. | #137/#141. |
| `POST /execute` | daemon | `execute_*`, live gate tests | Base implementation | Executes action plans and gated handoff/load behavior; mock paths are tested. | It is not the OpenAI inference-forwarding path; full chat forwarding is `/v1/chat/completions`. | Keep docs clear. |
| `GET /decisions/:id` and `/explain/:id` | daemon | `explain_returns_recorded_explanation`, `explain_returns_not_found_for_unknown_decision` | Fully operational | Recorded decisions can be explained through daemon state/log. | Cross-process persistence requires JSONL or SQLite config. | None. |
| `GET /staging` | daemon | `staging_status_reports_pending_blocked_failed_and_completed`, live gate skip tests | Fully operational | Staging queue reports pending/blocked/failed/completed and skip reasons. | Live load remains gated. | None. |
| Reconciliation loop and stale cache | daemon | `reconciliation_loop_*`, `decide_burst_reads_reconciler_cache_not_runtime_inspect` | Fully operational | Cache freshness, stale refresh, inspection errors, and status behavior are tested. | Live runtime quality depends on adapter evidence. | #133. |
| OpenAPI document | daemon | `openapi_document_is_served`, schema tests | Fully operational | `/openapi.json` is served and covers current daemon routes. | It does not include future `/telemetry/*` or `/dashboard`. | #130. |
| OpenAI-compatible model catalog | daemon gateway | `inference_gateway_maps_model_field_to_domain`, daemon smoke | Fully operational | `/v1/models` returns governance domains, not runtime models. | None found. | None. |
| OpenAI-compatible chat gateway | daemon gateway | gateway tests, daemon smoke | Fully operational for mock; Base for live | Mock forwarding works and returns telemetry headers. Non-mock forwarding has a safety gate. | Live non-mock forwarding not validated; `quality_floor` not enforced on `main`. | #133, #137/#141. |
| Gateway private `anemoi` metadata | daemon gateway | `inference_gateway_accepts_anemoi_selection_metadata`, `inference_gateway_strips_anemoi_metadata_before_forwarding` | Base implementation | Metadata is parsed and stripped before forwarding. | `quality_floor` is parsed but not enforced on `main`; `escalation_intent` is context/staging metadata, not automatic semantic routing. | #137/#141, #125. |
| CLI `status` | `anemoi-cli` | CLI test and smoke | Fully operational | Prints operator summary with runtime, group, staging, and policy status. | None found. | None. |
| CLI `residents` | `anemoi-cli` | CLI test and smoke | Fully operational | Prints runtime snapshots JSON. | None found. | None. |
| CLI `decide` | `anemoi-cli` | CLI test and smoke | Fully operational | Prints selected model/action and explanation reasons. | No CLI flags for `quality_floor` or `escalation_intent`. | Add when #137/#138 matures. |
| CLI `runtimes` | `anemoi-cli` | `cli_runtimes_prints_configured_adapters` | Fully operational | Lists configured adapters. | None found. | None. |
| CLI `policy check` | `anemoi-cli` | `cli_policy_check_*`, smoke | Fully operational | Validates config and prints diagnostics. | `--config` must be placed before the subcommand; `policy check --config` fails. | Fix docs/examples. |
| CLI `explain` | `anemoi-cli` | code inspection | Base implementation | Reads a decision from the configured decision log. | No dedicated CLI test proved cross-process explain after a prior `decide`; default in-memory log is per process. | Add CLI persistence smoke. |
| MCP service | `anemoi-mcp` library crate | `mcp_*` tests | Base implementation | Library service exposes status, residents, decide, explain, and policy check behavior. | There is no MCP server binary in `crates/anemoi-mcp`; integration surface is library-only. | Add server wiring if required. |
| In-memory decision log | `anemoi-telemetry` | `memory_decision_log_*` | Fully operational | Recent decisions are stored and retrieved in process. | Not durable across process restart. | Use JSONL/SQLite when durability is required. |
| JSONL decision log | `anemoi-telemetry` | `jsonl_decision_log_*` | Fully operational | Appends decisions and reloads after restart; tolerates malformed lines. | Resident/action/staging event history is SQLite-only. | Document scope clearly. |
| SQLite event store | `anemoi-telemetry`, daemon | `sqlite_event_store_*`, `daemon_uses_sqlite_store_when_database_url_is_present` | Fully operational | Durable decisions, runtime snapshots, resident events, staging events, action plans, and explanation replay are tested with restart round trips. | `execution_events` and `policy_events` remain deferred per roadmap. | Future event schemas if needed. |
| Telemetry JSON endpoints | daemon HTTP | route scan | Broken | None on `main`. | `/telemetry/*` routes do not exist. | #130. |
| Web dashboard | daemon/web | route and repo scan | Broken | None on `main`. | `/dashboard` and Vite dashboard do not exist. | #130, #138. |
| Docker compose deployment | `deploy/docker/docker-compose.yml` | static scan | Broken | Dockerfile exposes `7070`. | Compose still maps `8080:8080` and mounts legacy `appsettings.example.json`. | #129/#140, #131. |
| DNS/reverse proxy deployment | docs/deploy | static scan only | Needs validation | Docs mention 7070 and `anemoi.home.arpa` concepts. | No local DNS/Traefik validation run; compose is stale on `main`. | #131. |
| README public promise/quickstart | `README.md` | static scan | Broken | It describes many implemented Rust surfaces. | It overstates readiness (`All issues #30-34 delivered`, integration tested, complete docs) and still references legacy `.NET` as present. | #132, #129/#140. |
| Deployment docs | `docs/DEPLOYMENT.md`, compose | static scan | Broken | Rust Dockerfile examples mention 7070. | Compose and some deployment assumptions are stale or inconsistent. | #131. |
| Handoff docs | `docs/handoff.md` | static scan | Broken | Mock smoke notes are useful. | Still says SQLite/database-backed analytics are deferred, which conflicts with implemented SQLite event store. | #134 or docs cleanup. |
| Live validation docs | `docs/live_validation/*` | static scan | Needs validation | Safety plans and probe documents exist. | Several fields remain `TBD`/`Needs validation`; no live path was run in this audit. | #133. |
| Legacy .NET surface | `Anemoi.sln`, `src/Anemoi.*` | `git ls-files` | Broken | None for current Rust product. | 44 tracked legacy files remain on `main`; Docker compose still points at `appsettings`. | #129/#140. |

## Highest-Risk Gaps

1. `quality_floor` is parsed but not enforced on `main`, so a caller can request
   `32b` and still get the hot 9B path without a hard policy decision. PR #141
   addresses this.
2. Docker compose is stale: it maps `8080:8080` and mounts legacy
   `appsettings.example.json` while the Rust daemon uses `7070`. PR #140 and
   issue #131 address this.
3. Dashboard and telemetry JSON endpoints do not exist yet. Issue #130 is the
   right implementation target.
4. llama-swap live operation has strong fixture coverage but no recorded,
   sanitized real-host validation path in this audit. Issue #133 should close
   that public-readiness gap.
5. Concurrent transition coordination is policy-only today. Do not advertise it
   as daemon-wide multi-instance protection until daemon wiring exists.

## Follow-Up Issues

Existing issues already cover the audit findings:

| Finding | Existing issue |
|---|---|
| Remove tracked legacy .NET files and stale appsettings references | #129 / PR #140 |
| Enforce quality floor and escalation above 9B | #137 / PR #141 |
| Fix Docker/DNS deployment at `anemoi.home.arpa` | #131 |
| Add `/telemetry/*` endpoints and Vite dashboard | #130 |
| Validate one real llama-swap live path | #133 |
| Rewrite README around a sharp promise and working quickstart | #132 |
| Consolidate known limitations | #134 |
| Wire matrix colocation into policy decisions | #112 |
