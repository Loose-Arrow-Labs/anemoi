# Anemoi Test Roadmap

This roadmap keeps the Rust rewrite prompt-aligned. Later scaffolding can exist,
but a prompt is not accepted until its tests prove the contract named here.

## Promotion Rule

Each prompt must leave the project in a state where:

- required tests exist with the prompt's requested names
- tests are single-scoped
- earlier prompt invariants still pass
- daemon and CLI paths do not bypass core validation or policy logic
- `cargo fmt --check` and `cargo test --workspace` pass

`cargo clippy --workspace --all-targets -- -D warnings` is part of release
hardening when the local toolchain has the `clippy` component installed.

## Prompt Gates

| Prompt | Gate | Owner Crate(s) | Status |
|---|---|---|---|
| 00 v1 scope/no database | No required DB; memory by default; optional JSONL only. | `anemoi-telemetry`, docs | Passing |
| 01 config validation | Invalid config produces deterministic diagnostics and cannot start daemon/CLI runtime setup. | `anemoi-core`, `anemoi-daemon`, `anemoi-cli` | Passing |
| 02 core domain contracts | Domain IDs, actions, residency states, requests, decisions, and explanations serialize predictably. | `anemoi-core` | Passing |
| 03 runtime adapter contract | Adapter trait behavior is bounded and testable without policy decisions. | `anemoi-runtime` | Passing |
| 04 mock runtime snapshots | Mock runtime state is deterministic for policy tests. | `anemoi-runtime` | Passing |
| 05 policy candidate generation | Requests expand into explainable candidates from domain/roster/group/model/runtime. | `anemoi-policy` | Passing |
| 06 continuity staging | Hot-worker fallback stages cold large models when policy allows. | `anemoi-policy` | Passing |
| 07 telemetry memory/JSONL | Decision logs keep recent memory and optionally append JSONL. | `anemoi-telemetry` | Passing |
| 08 daemon decision API | API exposes health, status, residents, decide, decisions, and explain without hidden execution. | `anemoi-daemon` | Passing |
| 09 CLI operator loop | CLI gives useful status/decide/explain/residents output without duplicating scheduler logic. | `anemoi-cli` | Passing |
| 10 llama-swap inspection | Real llama-swap inspection normalizes residents without forwarding execution. | `anemoi-runtime` | Passing |
| 11 Ollama inspection | Ollama inspection is fixture-tested and normalizes resident state. | `anemoi-runtime` | Passing |
| 12 OpenAPI contract | API contract is published and tested against handlers. | `anemoi-daemon` | Passing |
| 13 MCP minimum surface | MCP exposes only stable control-plane decisions/resources. | `anemoi-mcp` | Passing |
| 14 hardening release checklist | Docs, validation, security, and release checks are clean. | workspace | Passing |
| 15 live validation safety plan | Read-only live validation procedure is documented before touching real runtimes. | docs | Passing |
| 16 llama-swap read-only probe | Live llama-swap evidence is captured without mutating endpoints or false residency claims. | `anemoi-runtime`, docs | Passing |
| 17 live runtime config profile | Sanitized llama-swap config validates without secrets or private host paths. | `anemoi-core`, config, docs | Passing |
| 18 residency truth contract | Runtime evidence maps honestly to resident, hot, configured, unknown, or failed state. | `anemoi-core`, `anemoi-runtime`, `anemoi-policy`, docs | Passing |
| 19 live decision smoke | Read-only live snapshots feed real `/decide` and CLI decisions with recorded limitations. | `anemoi-daemon`, `anemoi-cli`, docs | Passing |
| 20 controlled execution gate | Live load/unload/execution validation requires explicit approval and opt-in. | `anemoi-daemon`, `anemoi-runtime`, docs | Passing |
| 21 runtime reconciliation loop | Runtime inspection feeds a fresh cached observed state without mutating runtimes. | `anemoi-daemon`, `anemoi-runtime` | Passing |
| 22 background staging worker | Stage recommendations become observable staging intents and mock-executable jobs. | `anemoi-core`, `anemoi-daemon`, `anemoi-policy` | Passing |
| 23 load/unload action plan | Decisions produce explicit dry-run action plans before runtime mutation. | `anemoi-core`, `anemoi-daemon`, `anemoi-runtime` | Passing |
| 24 resource pressure model | Candidate scoring uses explicit VRAM, RAM, KV, load, and active-request pressure evidence. | `anemoi-policy`, `anemoi-core` | Passing |
| 25 eviction and pinning policy | Keep-hot workers are protected and eviction plans are explainable and gated. | `anemoi-policy`, `anemoi-core`, `anemoi-runtime` | Passing |
| 26 operator status surface | Status and CLI output show runtime health, residents, staging, policy, and unknown/stale state. | `anemoi-daemon`, `anemoi-cli` | Passing |
| 27 durable event store | Optional SQLite history records decisions, snapshots, staging, action plans, and explanations. | `anemoi-telemetry`, `anemoi-daemon` | Passing |
| 28 inference forwarding gateway | `POST /v1/chat/completions` maps model field to domain, runs decide, forwards to selected runtime, streams response. | `anemoi-daemon`, `anemoi-runtime`, `anemoi-core` | Passing |

## Current Focus

Build prompts 00-28 are passing. Prompt 28 (inference forwarding gateway) makes Anemoi an OpenAI-compatible endpoint for opencode: `POST /v1/chat/completions` treats the `model` field as a governance domain, runs `decide`, records telemetry, rewrites `model` to the selected runtime model, and streams the runtime response back with `X-Anemoi-Decision-Id`, `X-Anemoi-Selected-Model`, and `X-Anemoi-Action` headers. Mock forwarding works without `ANEMOI_ENABLE_LIVE_EXECUTE=1`; non-mock forwarding requires it.

Issue #12 (resident transition emission) is complete: the reconciliation loop now diffs resident sets each tick and records every state change to the `resident_events` table through the `DecisionLog` trait, with a non-anonymous evidence source naming the adapter and reconciliation round. The event store stays optional (default no-op trait method; only SQLite persists transitions).

Prompt 01 passed with:

- `accepts_example_config`
- `rejects_runtime_initial_resident_referencing_unknown_model`
- `rejects_domain_roster_referencing_unknown_group`
- `rejects_group_referencing_unknown_model`
- `rejects_model_referencing_unknown_runtime`
- `rejects_runtime_with_unknown_adapter`
- `rejects_empty_domain_roster`
- `rejects_empty_residency_group_models`
- `reports_all_config_diagnostics_deterministically`

Prompt 02 passed with:

- `serializes_residency_state_as_snake_case`
- `serializes_decision_action_as_snake_case`
- `deserializes_interactive_execution_mode`
- `request_id_defaults_to_uuid`
- `decision_explanation_roundtrips_json`
- `score_contributions_preserve_order`
- `runtime_memory_pressure_is_none_without_total`
- `runtime_memory_pressure_calculates_percent`

Prompt 03 passed with:

- `adapter_id_is_stable`
- `inspect_returns_normalized_runtime_snapshot`
- `load_model_returns_model_load_handle`
- `execute_returns_execution_handle`
- `unsupported_unload_returns_runtime_error`
- `runtime_errors_are_human_readable`

Prompt 04 passed with:

- `mock_runtime_starts_with_configured_residents`
- `mock_runtime_load_adds_loading_resident_once`
- `mock_runtime_unload_removes_resident`
- `mock_runtime_execute_records_active_request`
- `mock_runtime_memory_snapshot_is_configurable`
- `mock_runtime_inspect_is_repeatable`

Prompt 05 passed with:

- `generates_candidates_for_domain_rosters`
- `candidate_includes_residency_group`
- `candidate_includes_model_profile`
- `candidate_includes_available_supported_runtime`
- `rejects_model_without_available_runtime`
- `rejects_group_model_missing_profile`
- `candidate_order_is_deterministic`

Prompt 06 passed with:

- `avoids_cold_large_model_when_small_worker_is_hot`
- `does_not_stage_background_when_policy_disallows_background_load`
- `does_not_stage_background_when_latency_budget_allows_cold_load`
- `does_not_stage_background_without_hot_fallback`
- `records_background_model_in_decision`
- `explanation_names_selected_and_staged_models`
- `score_includes_continuity_contribution`

Prompt 07 passed with:

- `memory_decision_log_stores_and_gets_decision`
- `memory_decision_log_returns_none_for_unknown_decision`
- `memory_decision_log_keeps_recent_decisions_in_insert_order`
- `jsonl_decision_log_appends_one_json_object_per_decision`
- `jsonl_decision_log_creates_parent_directory_when_needed`
- `jsonl_decision_log_does_not_require_sqlite`
- `telemetry_trait_supports_memory_and_jsonl_logs`

Prompt 08 passed with:

- `health_returns_ok`
- `status_returns_configured_counts`
- `residents_returns_runtime_snapshots`
- `decide_returns_structured_decision`
- `decide_records_decision_in_log`
- `explain_returns_recorded_explanation`
- `explain_returns_not_found_for_unknown_decision`
- `execute_returns_honest_handoff_response`

Prompt 09 passed with:

- `cli_status_prints_configured_counts`
- `cli_policy_check_reports_valid_config`
- `cli_policy_check_reports_invalid_config_diagnostics`
- `cli_decide_prints_selected_model_and_action`
- `cli_decide_prints_explanation_reasons`
- `cli_residents_prints_runtime_snapshots`
- `cli_runtimes_prints_configured_adapters`

Prompt 10 passed with:

- `llama_swap_health_marks_runtime_available`
- `llama_swap_failed_health_marks_runtime_unavailable`
- `llama_swap_models_response_normalizes_model_ids`
- `llama_swap_inspect_returns_runtime_snapshot`
- `llama_swap_timeout_returns_runtime_error`
- `llama_swap_auth_header_is_applied_when_configured`

Prompt 11 passed with:

- `ollama_ps_response_maps_running_models_to_hot_residents`
- `ollama_ps_empty_response_returns_no_residents`
- `ollama_ps_vram_bytes_convert_to_mb`
- `ollama_unavailable_runtime_returns_error_or_unavailable_snapshot`
- `ollama_malformed_response_returns_runtime_error`
- `ollama_base_url_validation_rejects_invalid_url`

Prompt 12 passed with:

- `openapi_document_is_served`
- `openapi_document_includes_health_status_residents_decide_execute`
- `openapi_decide_schema_matches_decision_response`
- `openapi_explain_schema_matches_explanation_response`
- `openapi_contract_serializes_without_panic`

Prompt 13 passed with:

- `mcp_lists_expected_tools`
- `mcp_decide_returns_same_decision_shape_as_http_api`
- `mcp_status_returns_runtime_and_policy_summary`
- `mcp_residents_returns_normalized_snapshots`
- `mcp_explain_returns_recorded_explanation`
- `mcp_rejects_invalid_decide_request`

Prompt 14 passed with:

- setup docs updated to Rust daemon and CLI commands
- handoff notes added
- no required database path
- local binding defaults documented
- optional JSONL decision log documented
- `/execute` v1 limitation documented
- legacy .NET surface marked `Needs validation`
- final validation passed

Prompt 15 passed with:

- `docs/live_validation/` directory created
- `docs/live_validation/README.md` with phase policy
- `docs/live_validation/safety-plan.md` with permission boundary, operator inputs,
  read-only commands, and stop conditions
- all earlier invariants still pass

Prompt 16 passed with:

- `llama_swap_probe_does_not_require_mutating_endpoint`
- `llama_swap_probe_records_unknown_residency_when_endpoint_is_ambiguous`
- `llama_swap_probe_maps_configured_models_without_claiming_hot_residency`
- `docs/live_validation/llama-swap-probe.md` with evidence table and
  interpretation rules

Prompt 17 passed with:

- `accepts_live_llama_swap_example_config`
- `live_config_uses_environment_for_auth`
- `live_config_rejects_missing_required_runtime_url_when_no_default_exists`
- `live_config_keeps_small_worker_and_large_target_in_distinct_groups`
- `config/anemoi.llama-swap.example.yaml` with env-var placeholders
- env var expansion (`${VAR}`) in config loading
- validation requires `base_url` for known non-mock adapters

Prompt 18 passed with:

- `configured_model_without_runtime_residency_evidence_is_not_hot`
- `running_model_endpoint_maps_to_hot_or_serving`
- `failed_runtime_health_maps_to_unavailable_snapshot`
- `ambiguous_runtime_state_preserves_unknown_or_cold_candidate_reason`
- `decision_explanation_mentions_ambiguous_residency_evidence`
- `docs/live_validation/residency-truth-contract.md` with evidence-to-state
  mapping, non-evidence table, and runtime-specific contracts

Prompt 19 passed with:

- `live_smoke_decide_uses_runtime_snapshot_without_execute`
- `live_smoke_decision_records_runtime_evidence_source`
- `live_smoke_explanation_includes_latency_and_residency_reasons`
- `docs/live_validation/decision-smoke.md` with smoke procedure, evidence
  collection table, and success criteria

Prompt 20 passed with:

- `live_execute_requires_explicit_enable_flag`
- `live_execute_rejects_without_runtime_target`
- `live_execute_returns_handoff_metadata_without_forwarding_by_default`
- `live_execute_records_mutating_action_in_decision_explanation`
- `ANEMOI_ENABLE_LIVE_EXECUTE=1` opt-in guard for non-mock execution
- `docs/live_validation/controlled-execution-gate.md` with approval checklist
  and execution path table

Prompt 27 passed with:

- `sqlite_event_store_records_decision_event`
- `sqlite_event_store_records_runtime_snapshot_event`
- `sqlite_event_store_records_staging_event`
- `sqlite_event_store_records_action_plan_event`
- `sqlite_event_store_replays_decision_explanation_by_id`
- `daemon_starts_with_memory_store_when_database_url_is_missing`
- `daemon_uses_sqlite_store_when_database_url_is_present`
- `sqlite_event_store_records_resident_event` (issue #12 `resident_events`)

SQLite is the source of truth: `get_decision`/`list_decisions` read from the
database, so every required test reopens the file (a process "restart") and
asserts the recorded value round-trips, rather than asserting `is_ok`. The
`resident_events` table follows the issue #12 schema with a NOT NULL
`evidence_source`. `execution_events`/`policy_events` tables are deferred: no
required test exercises them and they belong to later prompts (21-23).

Issue #12 (resident transition emission) wired the reconciliation loop to the
`resident_events` table:

- `resident_transitions_detects_first_observation_change_and_skips_unchanged`
  (`anemoi-daemon`)
- `reconciliation_tick_records_resident_transition` (`anemoi-daemon`)

Prompt 27 built the `resident_events` table but only its own test called
`record_resident_event`; nothing emitted transitions from the running daemon.
Issue #12 closes that gap: each reconciliation tick diffs the previous and
current resident sets per runtime and records every state change through the
`DecisionLog` trait via `record_resident_transition` (a default no-op so the
event store stays optional — in-memory/JSONL logs ignore transitions, only
SQLite persists them). First observations record a `cold` → observed
transition carrying a note, since `ResidencyState` has no `unknown` variant and
issue #12 forbids inventing one. Evidence source is never anonymous: it names
the adapter and the reconciliation round (`"<adapter> reconciliation round
<n>"`). Vanished residents are not recorded (their target state is
unobserved). `reconciliation_tick_records_resident_transition` reopens the
SQLite store and asserts the persisted row's `from_state`, `to_state`,
`runtime_id`, null `decision_id`, note text, and evidence string, rather than
asserting `is_ok`.

Prompt 28 passed with:

- `inference_gateway_maps_model_field_to_domain`
- `inference_gateway_strips_anemoi_prefix_from_model_field`
- `inference_gateway_returns_error_for_unknown_domain`
- `inference_gateway_runs_decide_before_forwarding`
- `inference_gateway_rewrites_model_to_selected_model`
- `inference_gateway_injects_runtime_auth_token` (`anemoi-runtime`)
- `inference_gateway_records_decision_in_telemetry`
- `inference_gateway_returns_decision_id_in_response_header`
- `inference_gateway_requires_live_execute_flag_for_non_mock`
- `inference_gateway_forwards_mock_without_live_execute_flag`
- `inference_gateway_returns_structured_error_on_runtime_failure`

The caller names a governance **domain** in the OpenAI `model` field (never a
runtime model); the gateway resolves it (stripping an optional `anemoi-`
prefix), runs the same decision path as `/decide` so telemetry is recorded,
rewrites `model` to the selected runtime model, and streams the runtime
response back. Forwarding lives in `anemoi-runtime` (`forward_chat_completion`
+ `mock_chat_completion`) so the daemon stays reqwest-free. Non-mock forwarding
is gated behind `ANEMOI_ENABLE_LIVE_EXECUTE=1` (prompt 20); mock forwarding is
always allowed for offline development. Runtime failures return an
OpenAI-shaped structured error carrying the decision id.
