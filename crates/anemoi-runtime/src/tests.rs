use super::*;
use anemoi_core::{ModelResident, RequestId, ResidencyState, RuntimeMemorySnapshot};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use uuid::Uuid;

use crate::llama_swap::event_stream::{
    apply_model_status, parse_model_status_payload, residents_from_states, SseDecoder,
};
use crate::util::normalize_model_id;

#[test]
fn adapter_id_is_stable() {
    let adapter = MockRuntimeAdapter::new(RuntimeId("mock".to_string()), Vec::new());

    assert_eq!(adapter.id(), RuntimeId("mock".to_string()));
    assert_eq!(adapter.id(), RuntimeId("mock".to_string()));
}

#[tokio::test]
async fn inspect_returns_normalized_runtime_snapshot() {
    let adapter = MockRuntimeAdapter::new(
        RuntimeId("mock".to_string()),
        vec![ModelResident {
            model_id: ModelId("qwen9b".to_string()),
            state: ResidencyState::HotGpu,
            vram_mb: Some(9000),
            ram_mb: Some(12000),
            kv_cache_mb: Some(512),
            loaded_since: None,
        }],
    )
    .with_memory(RuntimeMemorySnapshot {
        vram_total_mb: Some(24_000),
        vram_used_mb: Some(9_000),
        ram_total_mb: Some(64_000),
        ram_used_mb: Some(12_000),
    });

    let snapshot = adapter.inspect().await.expect("snapshot");

    assert_eq!(snapshot.runtime_id, RuntimeId("mock".to_string()));
    assert!(snapshot.available);
    assert_eq!(snapshot.residents.len(), 1);
    assert_eq!(
        snapshot.residents[0].model_id,
        ModelId("qwen9b".to_string())
    );
    assert_eq!(snapshot.residents[0].state, ResidencyState::HotGpu);
    assert_eq!(snapshot.memory.pressure_percent(), Some(37));
}

#[tokio::test]
async fn load_model_returns_model_load_handle() {
    let adapter = MockRuntimeAdapter::new(RuntimeId("mock".to_string()), Vec::new());
    let model_id = ModelId("qwen9b".to_string());

    let handle = adapter.load_model(&model_id).await.expect("load handle");
    let snapshot = adapter.inspect().await.expect("snapshot");

    assert_eq!(handle.model_id, model_id);
    assert!(snapshot.residents.iter().any(|resident| {
        resident.model_id == ModelId("qwen9b".to_string())
            && resident.state == ResidencyState::Loading
    }));
}

#[tokio::test]
async fn execute_returns_execution_handle() {
    let adapter = MockRuntimeAdapter::new(RuntimeId("mock".to_string()), Vec::new());
    let request = ExecutionRequest {
        request_id: RequestId::new(),
        model_id: ModelId("qwen9b".to_string()),
        prompt: Some("hello".to_string()),
    };

    let handle = adapter
        .execute(request.clone())
        .await
        .expect("execute handle");
    let snapshot = adapter.inspect().await.expect("snapshot");

    assert_eq!(handle.request, request);
    assert_eq!(snapshot.active_requests.len(), 1);
    assert_eq!(snapshot.active_requests[0].request_id, request.request_id);
    assert_eq!(snapshot.active_requests[0].model_id, request.model_id);
}

#[tokio::test]
async fn unsupported_unload_returns_runtime_error() {
    let adapter =
        HttpInspectAdapter::new(RuntimeId("llama_swap".to_string()), "http://localhost:8080")
            .expect("adapter");

    let error = adapter
        .unload_model(&ModelId("qwen9b".to_string()))
        .await
        .expect_err("unsupported unload");

    assert_eq!(
        error.to_string(),
        "runtime operation is not supported: http unload"
    );
}

#[test]
fn runtime_errors_are_human_readable() {
    let unavailable = RuntimeError::Unavailable(RuntimeId("ollama".to_string()));
    let unsupported = RuntimeError::Unsupported("ollama unload");
    let url = RuntimeError::Url("relative URL without a base".to_string());

    assert_eq!(unavailable.to_string(), "runtime ollama is unavailable");
    assert_eq!(
        unsupported.to_string(),
        "runtime operation is not supported: ollama unload"
    );
    assert_eq!(
        url.to_string(),
        "invalid runtime url: relative URL without a base"
    );
}

#[tokio::test]
async fn mock_runtime_starts_with_configured_residents() {
    let adapter = MockRuntimeAdapter::new(
        RuntimeId("mock".to_string()),
        vec![ModelResident {
            model_id: ModelId("qwen9b".to_string()),
            state: ResidencyState::HotGpu,
            vram_mb: Some(9000),
            ram_mb: None,
            kv_cache_mb: None,
            loaded_since: None,
        }],
    );

    let snapshot = adapter.inspect().await.expect("snapshot");

    assert_eq!(snapshot.residents.len(), 1);
    assert_eq!(
        snapshot.residents[0].model_id,
        ModelId("qwen9b".to_string())
    );
    assert_eq!(snapshot.residents[0].state, ResidencyState::HotGpu);
}

#[tokio::test]
async fn mock_runtime_load_adds_loading_resident_once() {
    let adapter = MockRuntimeAdapter::new(RuntimeId("mock".to_string()), Vec::new());
    let model_id = ModelId("qwen9b".to_string());

    adapter.load_model(&model_id).await.expect("first load");
    adapter.load_model(&model_id).await.expect("second load");
    let snapshot = adapter.inspect().await.expect("snapshot");

    assert_eq!(
        snapshot
            .residents
            .iter()
            .filter(|resident| resident.model_id == model_id)
            .count(),
        1
    );
    assert_eq!(snapshot.residents[0].state, ResidencyState::Loading);
}

#[tokio::test]
async fn mock_runtime_unload_removes_resident() {
    let adapter = MockRuntimeAdapter::new(RuntimeId("mock".to_string()), Vec::new())
        .with_resident_state(ModelId("qwen9b".to_string()), ResidencyState::HotGpu);

    adapter
        .unload_model(&ModelId("qwen9b".to_string()))
        .await
        .expect("unload");
    let snapshot = adapter.inspect().await.expect("snapshot");

    assert!(snapshot.residents.is_empty());
}

#[tokio::test]
async fn mock_runtime_execute_records_active_request() {
    let adapter = MockRuntimeAdapter::new(RuntimeId("mock".to_string()), Vec::new());
    let request = ExecutionRequest {
        request_id: RequestId::new(),
        model_id: ModelId("qwen9b".to_string()),
        prompt: None,
    };

    adapter.execute(request.clone()).await.expect("execute");
    let snapshot = adapter.inspect().await.expect("snapshot");

    assert_eq!(snapshot.active_requests.len(), 1);
    assert_eq!(snapshot.active_requests[0].request_id, request.request_id);
    assert_eq!(snapshot.active_requests[0].model_id, request.model_id);
}

#[tokio::test]
async fn mock_runtime_memory_snapshot_is_configurable() {
    let adapter = MockRuntimeAdapter::new(RuntimeId("mock".to_string()), Vec::new())
        .with_memory(RuntimeMemorySnapshot {
            vram_total_mb: Some(30_000),
            vram_used_mb: Some(27_000),
            ram_total_mb: Some(64_000),
            ram_used_mb: Some(12_000),
        })
        .with_available(false);

    let snapshot = adapter.inspect().await.expect("snapshot");

    assert!(!snapshot.available);
    assert_eq!(snapshot.memory.pressure_percent(), Some(90));
}

#[tokio::test]
async fn mock_runtime_inspect_is_repeatable() {
    let adapter = MockRuntimeAdapter::new(RuntimeId("mock".to_string()), Vec::new())
        .with_resident_state(ModelId("qwen9b".to_string()), ResidencyState::WarmCpu)
        .with_memory(RuntimeMemorySnapshot {
            vram_total_mb: Some(24_000),
            vram_used_mb: Some(12_000),
            ram_total_mb: None,
            ram_used_mb: None,
        });

    let first = adapter.inspect().await.expect("first snapshot");
    let second = adapter.inspect().await.expect("second snapshot");

    assert_eq!(first, second);
}

#[tokio::test]
async fn llama_swap_health_marks_runtime_available() {
    let server = spawn_fixture(vec![
        http_response(200, "{}"),
        http_response(200, r#"{"data":[]}"#),
    ])
    .await;
    let adapter = LlamaSwapAdapter::new(RuntimeId("llama_swap".to_string()), &server.base_url)
        .expect("adapter");

    let snapshot = adapter.inspect().await.expect("snapshot");

    assert!(snapshot.available);
}

#[tokio::test]
async fn llama_swap_failed_health_marks_runtime_unavailable() {
    let server = spawn_fixture(vec![http_response(500, "{}")]).await;
    let adapter = LlamaSwapAdapter::new(RuntimeId("llama_swap".to_string()), &server.base_url)
        .expect("adapter");

    let snapshot = adapter.inspect().await.expect("snapshot");

    assert!(!snapshot.available);
    assert!(snapshot.residents.is_empty());
}

#[tokio::test]
async fn llama_swap_inspect_tolerates_flaky_models_endpoint() {
    // /health succeeds but /v1/models returns 500. The runtime is healthy, so
    // inspect must report it available with no configured models rather than
    // propagating the error (which would make it look like a hard outage).
    let server = spawn_fixture(vec![
        http_response(200, "{}"), // /health ok
        http_response(500, "{}"), // /v1/models fails
    ])
    .await;
    let adapter = LlamaSwapAdapter::new(RuntimeId("llama_swap".to_string()), &server.base_url)
        .expect("adapter");

    let snapshot = adapter
        .inspect()
        .await
        .expect("healthy runtime with flaky /v1/models must still inspect");

    assert!(snapshot.available, "healthy runtime must stay available");
    assert!(
        snapshot.configured_models.is_empty(),
        "a failed /v1/models yields no configured models"
    );
    assert!(snapshot.residents.is_empty());
}

#[tokio::test]
async fn llama_cpp_inspect_tolerates_flaky_models_endpoint() {
    let server = spawn_fixture(vec![
        http_response(200, "{}"), // /health ok
        http_response(500, "{}"), // /v1/models fails
    ])
    .await;
    let adapter = LlamaCppAdapter::new(RuntimeId("llama_cpp".to_string()), &server.base_url)
        .expect("adapter");

    let snapshot = adapter
        .inspect()
        .await
        .expect("healthy runtime with flaky /v1/models must still inspect");

    assert!(snapshot.available, "healthy runtime must stay available");
    assert!(
        snapshot.configured_models.is_empty(),
        "a failed /v1/models yields no configured models"
    );
}

#[tokio::test]
async fn llama_swap_models_response_normalizes_model_ids() {
    let server = spawn_fixture(vec![http_response(
        200,
        r#"{"data":[{"id":"models/qwen9b.gguf"},{"id":"granite8b"}]}"#,
    )])
    .await;
    let adapter = LlamaSwapAdapter::new(RuntimeId("llama_swap".to_string()), &server.base_url)
        .expect("adapter");

    let models = adapter.inspect_models().await.expect("models");

    assert_eq!(
        models,
        vec![
            ModelId("qwen9b".to_string()),
            ModelId("granite8b".to_string())
        ]
    );
}

#[tokio::test]
async fn llama_swap_models_response_skips_empty_model_ids() {
    let server = spawn_fixture(vec![http_response(
        200,
        r#"{"data":[{"id":""},{"id":"models/qwen9b.gguf"},{"id":"models/"}]}"#,
    )])
    .await;
    let adapter = LlamaSwapAdapter::new(RuntimeId("llama_swap".to_string()), &server.base_url)
        .expect("adapter");

    let models = adapter.inspect_models().await.expect("models");

    // The empty id and the directory-only "models/" id (which normalizes to
    // empty) are dropped; only the valid model survives.
    assert_eq!(models, vec![ModelId("qwen9b".to_string())]);
}

#[test]
fn normalize_model_id_strips_path_and_extension() {
    assert_eq!(
        normalize_model_id("models/qwen9b.gguf").expect("id"),
        ModelId("qwen9b".to_string())
    );
    assert_eq!(
        normalize_model_id(r"weights\granite8b.bin").expect("id"),
        ModelId("granite8b".to_string())
    );
}

#[test]
fn normalize_model_id_rejects_empty_input() {
    // A runtime reporting a model with no usable id is malformed; an empty id
    // must be rejected rather than yielding ModelId("").
    let err = normalize_model_id("").expect_err("empty id must be rejected");
    assert!(
        err.to_string().contains("empty model id"),
        "unexpected error: {err}"
    );
    assert!(normalize_model_id("models/").is_err());
    assert!(normalize_model_id(r"weights\").is_err());
}

#[tokio::test]
async fn llama_swap_inspect_returns_runtime_snapshot() {
    let server = spawn_fixture(vec![
        http_response(200, "{}"),
        http_response(200, r#"{"data":[{"id":"models/qwen9b.gguf"}]}"#),
    ])
    .await;
    let adapter = LlamaSwapAdapter::new(RuntimeId("llama_swap".to_string()), &server.base_url)
        .expect("adapter");

    let snapshot = adapter.inspect().await.expect("snapshot");

    assert_eq!(snapshot.runtime_id, RuntimeId("llama_swap".to_string()));
    assert!(snapshot.available);
    assert!(snapshot.residents.is_empty());
    assert_eq!(snapshot.memory, RuntimeMemorySnapshot::default());
}

#[tokio::test]
async fn llama_swap_probe_does_not_require_mutating_endpoint() {
    let server = spawn_fixture(vec![
        http_response(200, "{}"),
        http_response(200, r#"{"data":[]}"#),
    ])
    .await;
    let adapter = LlamaSwapAdapter::new(RuntimeId("llama_swap".to_string()), &server.base_url)
        .expect("adapter");

    let _ = adapter.inspect().await.expect("inspect");

    let requests = server.requests.lock().expect("requests").clone();
    for request_text in &requests {
        assert!(
            request_text.starts_with("GET"),
            "probe must use GET only, got: {request_text:?}"
        );
    }
}

#[tokio::test]
async fn llama_swap_probe_records_unknown_residency_when_endpoint_is_ambiguous() {
    let server = spawn_fixture(vec![
        http_response(200, "{}"),
        http_response(
            200,
            r#"{"data":[{"id":"models/qwen9b.gguf"},{"id":"models/qwen35_a3b.gguf"}]}"#,
        ),
    ])
    .await;
    let adapter = LlamaSwapAdapter::new(RuntimeId("llama_swap".to_string()), &server.base_url)
        .expect("adapter");

    let snapshot = adapter.inspect().await.expect("snapshot");

    // /v1/models returns models but does not prove hot residency.
    assert!(snapshot.available);
    assert!(
        snapshot.residents.is_empty(),
        "ambiguous endpoint must not claim hot residency"
    );
}

#[tokio::test]
async fn llama_swap_probe_maps_configured_models_without_claiming_hot_residency() {
    let server = spawn_fixture(vec![
        // inspect_models: /v1/models
        http_response(
            200,
            r#"{"data":[{"id":"models/qwen9b.gguf"},{"id":"models/qwen35_a3b.gguf"}]}"#,
        ),
        // inspect: /health
        http_response(200, "{}"),
        // inspect: /v1/models
        http_response(
            200,
            r#"{"data":[{"id":"models/qwen9b.gguf"},{"id":"models/qwen35_a3b.gguf"}]}"#,
        ),
    ])
    .await;
    let adapter = LlamaSwapAdapter::new(RuntimeId("llama_swap".to_string()), &server.base_url)
        .expect("adapter");

    let models = adapter.inspect_models().await.expect("models");

    // inspect_models returns configured model ids but inspect does not claim residency.
    assert_eq!(
        models,
        vec![
            ModelId("qwen9b".to_string()),
            ModelId("qwen35_a3b".to_string()),
        ]
    );

    let snapshot = adapter.inspect().await.expect("snapshot");
    assert!(
        snapshot.residents.is_empty(),
        "configured models must not be reported as hot residents"
    );
}

#[tokio::test]
async fn configured_model_without_runtime_residency_evidence_is_not_hot() {
    let server = spawn_fixture(vec![
        // /health returns ok
        http_response(200, "{}"),
        // /v1/models returns configured models
        http_response(
            200,
            r#"{"data":[{"id":"models/qwen9b.gguf"},{"id":"models/qwen35_a3b.gguf"}]}"#,
        ),
    ])
    .await;
    let adapter = LlamaSwapAdapter::new(RuntimeId("llama_swap".to_string()), &server.base_url)
        .expect("adapter");

    let snapshot = adapter.inspect().await.expect("snapshot");

    // /v1/models returns models but inspect must not claim hot residency.
    assert!(snapshot.available);
    assert_eq!(
        snapshot.residents.len(),
        0,
        "configured models without runtime evidence must not be hot"
    );
}

#[tokio::test]
async fn llama_swap_inspect_populates_residents_from_models_endpoint() {
    // Name preserved from issue #46. Per the residency truth contract,
    // /v1/models proves configuration, not residency — so configured
    // models surface in `configured_models`, not `residents`.
    let server = spawn_fixture(vec![
        http_response(200, "{}"),
        http_response(
            200,
            r#"{"data":[{"id":"models/qwen9b.gguf"},{"id":"granite8b"}]}"#,
        ),
    ])
    .await;
    let adapter = LlamaSwapAdapter::new(RuntimeId("llama_swap".to_string()), &server.base_url)
        .expect("adapter");

    let snapshot = adapter.inspect().await.expect("snapshot");

    assert!(snapshot.available);
    assert_eq!(
        snapshot.configured_models,
        vec![
            ModelId("qwen9b".to_string()),
            ModelId("granite8b".to_string()),
        ]
    );
}

#[tokio::test]
async fn llama_swap_inspect_marks_residents_as_configured_not_hot() {
    // Name preserved from issue #46. Configured models from /v1/models must
    // not be reported as residents at all — residency requires evidence
    // beyond configuration (see residency-truth-contract.md).
    let server = spawn_fixture(vec![
        http_response(200, "{}"),
        http_response(200, r#"{"data":[{"id":"models/qwen9b.gguf"}]}"#),
    ])
    .await;
    let adapter = LlamaSwapAdapter::new(RuntimeId("llama_swap".to_string()), &server.base_url)
        .expect("adapter");

    let snapshot = adapter.inspect().await.expect("snapshot");

    assert_eq!(
        snapshot.configured_models,
        vec![ModelId("qwen9b".to_string())]
    );
    assert!(
        snapshot.residents.is_empty(),
        "configured models must not appear as residents"
    );
}

#[test]
fn llama_swap_model_state_maps_wire_strings() {
    assert_eq!(
        LlamaSwapModelState::from_wire("stopped"),
        Some(LlamaSwapModelState::Stopped)
    );
    assert_eq!(
        LlamaSwapModelState::from_wire("starting"),
        Some(LlamaSwapModelState::Starting)
    );
    assert_eq!(
        LlamaSwapModelState::from_wire("ready"),
        Some(LlamaSwapModelState::Ready)
    );
    assert_eq!(
        LlamaSwapModelState::from_wire("stopping"),
        Some(LlamaSwapModelState::Stopping)
    );
    assert_eq!(
        LlamaSwapModelState::from_wire("shutdown"),
        Some(LlamaSwapModelState::Shutdown)
    );
    assert_eq!(LlamaSwapModelState::from_wire("bogus"), None);
}

#[test]
fn llama_swap_model_state_residency_omits_unloaded() {
    assert_eq!(LlamaSwapModelState::Stopped.residency(), None);
    assert_eq!(LlamaSwapModelState::Shutdown.residency(), None);
    assert_eq!(
        LlamaSwapModelState::Starting.residency(),
        Some(ResidencyState::Loading)
    );
    assert_eq!(
        LlamaSwapModelState::Ready.residency(),
        Some(ResidencyState::HotGpu)
    );
    assert_eq!(
        LlamaSwapModelState::Stopping.residency(),
        Some(ResidencyState::Draining)
    );
}

#[test]
fn parse_model_status_payload_extracts_double_encoded_states() {
    // Captured shape: `data` is a JSON-encoded *string* of the entries.
    let payload = r#"{"type":"modelStatus","data":"[{\"id\":\"minimax\",\"state\":\"starting\"},{\"id\":\"gemma\",\"state\":\"ready\"}]"}"#;

    let parsed = parse_model_status_payload(payload).expect("modelStatus frame");

    assert_eq!(
        parsed,
        vec![
            ("minimax".to_string(), LlamaSwapModelState::Starting),
            ("gemma".to_string(), LlamaSwapModelState::Ready),
        ]
    );
}

#[test]
fn parse_model_status_payload_ignores_other_frame_types() {
    let payload = r#"{"type":"logData","data":"some log line"}"#;
    assert_eq!(parse_model_status_payload(payload), None);
}

#[test]
fn sse_decoder_emits_frame_only_once_complete() {
    let event = format!(
        "data: {}\n\n",
        r#"{"type":"modelStatus","data":"[{\"id\":\"gemma\",\"state\":\"ready\"}]"}"#
    );
    let (head, tail) = event.split_at(20);
    let mut decoder = SseDecoder::default();

    assert!(
        decoder.push(head.as_bytes()).is_empty(),
        "a partial frame must not emit an update"
    );
    let updates = decoder.push(tail.as_bytes());

    assert_eq!(
        updates,
        vec![vec![("gemma".to_string(), LlamaSwapModelState::Ready)]]
    );
}

#[test]
fn sse_decoder_splits_multiple_events_in_one_chunk() {
    let chunk = format!(
        "data: {}\n\ndata: {}\n\n",
        r#"{"type":"modelStatus","data":"[{\"id\":\"a\",\"state\":\"starting\"}]"}"#,
        r#"{"type":"modelStatus","data":"[{\"id\":\"a\",\"state\":\"ready\"}]"}"#
    );
    let mut decoder = SseDecoder::default();

    let updates = decoder.push(chunk.as_bytes());

    assert_eq!(
        updates,
        vec![
            vec![("a".to_string(), LlamaSwapModelState::Starting)],
            vec![("a".to_string(), LlamaSwapModelState::Ready)],
        ]
    );
}

#[test]
fn residents_from_states_omits_unloaded_and_sorts() {
    let mut states = HashMap::new();
    states.insert("models/qwen9b.gguf".to_string(), LlamaSwapModelState::Ready);
    states.insert("granite8b".to_string(), LlamaSwapModelState::Starting);
    states.insert("minimax".to_string(), LlamaSwapModelState::Stopped);

    let residents = residents_from_states(&states);

    assert_eq!(residents.len(), 2, "stopped model must be omitted");
    assert_eq!(residents[0].model_id, ModelId("granite8b".to_string()));
    assert_eq!(residents[0].state, ResidencyState::Loading);
    assert_eq!(residents[1].model_id, ModelId("qwen9b".to_string()));
    assert_eq!(residents[1].state, ResidencyState::HotGpu);
}

#[test]
fn apply_model_status_replaces_previous_snapshot() {
    let cache: ModelStateCache = Arc::new(RwLock::new(HashMap::new()));
    apply_model_status(
        &cache,
        vec![("gemma".to_string(), LlamaSwapModelState::Ready)],
    );
    apply_model_status(
        &cache,
        vec![("qwen9b".to_string(), LlamaSwapModelState::Starting)],
    );

    let guard = cache.read().expect("cache");
    assert_eq!(guard.len(), 1, "full snapshot replaces, not merges");
    assert_eq!(guard.get("qwen9b"), Some(&LlamaSwapModelState::Starting));
    assert!(guard.get("gemma").is_none());
}

#[tokio::test]
async fn llama_swap_inspect_reports_residents_from_event_cache() {
    let server = spawn_fixture(vec![
        http_response(200, "{}"),
        http_response(200, r#"{"data":[{"id":"models/qwen9b.gguf"}]}"#),
    ])
    .await;
    let adapter = LlamaSwapAdapter::new(RuntimeId("llama_swap".to_string()), &server.base_url)
        .expect("adapter");

    // Simulate the SSE stream having observed a ready and a stopped model.
    apply_model_status(
        &adapter.model_states(),
        vec![
            ("qwen9b".to_string(), LlamaSwapModelState::Ready),
            ("granite8b".to_string(), LlamaSwapModelState::Stopped),
        ],
    );

    let snapshot = adapter.inspect().await.expect("snapshot");

    assert_eq!(
        snapshot.residents.len(),
        1,
        "only the ready model is resident"
    );
    assert_eq!(
        snapshot.residents[0].model_id,
        ModelId("qwen9b".to_string())
    );
    assert_eq!(snapshot.residents[0].state, ResidencyState::HotGpu);
}

#[tokio::test]
async fn failed_runtime_health_maps_to_unavailable_snapshot() {
    let server = spawn_fixture(vec![http_response(503, "Service Unavailable")]).await;
    let adapter = LlamaSwapAdapter::new(RuntimeId("llama_swap".to_string()), &server.base_url)
        .expect("adapter");

    let snapshot = adapter.inspect().await.expect("snapshot");

    assert!(!snapshot.available);
    assert!(snapshot.residents.is_empty());
    assert_eq!(snapshot.memory, RuntimeMemorySnapshot::default());
}

#[tokio::test]
async fn running_model_endpoint_maps_to_hot_or_serving() {
    let server = spawn_fixture(vec![http_response(
        200,
        r#"{"models":[{"name":"qwen9b","size_vram":9437184000}]}"#,
    )])
    .await;
    let adapter =
        OllamaAdapter::new(RuntimeId("ollama".to_string()), &server.base_url).expect("adapter");

    let snapshot = adapter.inspect().await.expect("snapshot");

    assert_eq!(snapshot.residents.len(), 1);
    assert_eq!(snapshot.residents[0].state, ResidencyState::HotGpu);
}

#[tokio::test]
async fn ollama_load_model_is_unsupported() {
    // Ollama loads lazily on first inference; Anemoi does not proactively
    // load it. load_model must report Unsupported (not a fake LoadHandle) so
    // staging does not mark an intent Completed for a load that never ran.
    let adapter = OllamaAdapter::new(RuntimeId("ollama".to_string()), "http://localhost:11434")
        .expect("adapter");

    let error = adapter
        .load_model(&ModelId("qwen9b".to_string()))
        .await
        .expect_err("ollama load must be unsupported");

    assert_eq!(
        error.to_string(),
        "runtime operation is not supported: ollama load"
    );
}

#[tokio::test]
async fn llama_swap_load_model_posts_to_chat_completions() {
    let server = spawn_fixture(vec![http_response(
        200,
        r#"{"choices":[{"message":{"role":"assistant","content":"ok"}}]}"#,
    )])
    .await;
    let adapter = LlamaSwapAdapter::new(RuntimeId("llama_swap".to_string()), &server.base_url)
        .expect("adapter");

    let handle = adapter
        .load_model(&ModelId("qwen9b".to_string()))
        .await
        .expect("load handle");

    assert_eq!(handle.model_id, ModelId("qwen9b".to_string()));

    let requests = server.requests.lock().expect("requests").clone();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert!(
        request.starts_with("POST /v1/chat/completions"),
        "load_model must POST to /v1/chat/completions, got: {request:?}"
    );
    assert!(
        request.contains("\"model\":\"qwen9b\""),
        "request must carry the requested model id, got: {request:?}"
    );
}

#[tokio::test]
async fn llama_swap_load_model_surfaces_upstream_5xx_as_error() {
    let server = spawn_fixture(vec![http_response(500, "{}")]).await;
    let adapter = LlamaSwapAdapter::new(RuntimeId("llama_swap".to_string()), &server.base_url)
        .expect("adapter");

    let error = adapter
        .load_model(&ModelId("qwen9b".to_string()))
        .await
        .expect_err("upstream 5xx must surface as error");

    assert!(matches!(error, RuntimeError::Http(_)));
}

#[tokio::test]
async fn llama_swap_timeout_returns_runtime_error() {
    let base_url = spawn_timeout_server().await;
    let adapter = LlamaSwapAdapter::new_with_timeout(
        RuntimeId("llama_swap".to_string()),
        &base_url,
        Duration::from_millis(50),
    )
    .expect("adapter");

    let error = adapter.inspect().await.expect_err("timeout");

    assert!(matches!(error, RuntimeError::Http(error) if error.is_timeout()));
}

#[tokio::test]
async fn llama_swap_auth_header_is_applied_when_configured() {
    let server = spawn_fixture(vec![
        http_response(200, "{}"),
        http_response(200, r#"{"data":[]}"#),
    ])
    .await;
    let adapter = LlamaSwapAdapter::new(RuntimeId("llama_swap".to_string()), &server.base_url)
        .expect("adapter")
        .with_bearer_token("secret");

    let _ = adapter.inspect().await.expect("snapshot");

    let requests = server.requests.lock().expect("requests").clone();
    assert_eq!(requests.len(), 2);
    assert!(requests.iter().all(|request| request
        .to_ascii_lowercase()
        .contains("authorization: bearer secret")));
}

#[tokio::test]
async fn ollama_ps_response_maps_running_models_to_hot_residents() {
    let server = spawn_fixture(vec![http_response(
        200,
        r#"{"models":[{"name":"qwen9b","size_vram":9437184000,"size":12582912000}]}"#,
    )])
    .await;
    let adapter =
        OllamaAdapter::new(RuntimeId("ollama".to_string()), &server.base_url).expect("adapter");

    let snapshot = adapter.inspect().await.expect("snapshot");

    assert!(snapshot.available);
    assert_eq!(snapshot.residents.len(), 1);
    assert_eq!(
        snapshot.residents[0].model_id,
        ModelId("qwen9b".to_string())
    );
    assert_eq!(snapshot.residents[0].state, ResidencyState::HotGpu);
}

#[tokio::test]
async fn ollama_ps_empty_response_returns_no_residents() {
    let server = spawn_fixture(vec![http_response(200, r#"{"models":[]}"#)]).await;
    let adapter =
        OllamaAdapter::new(RuntimeId("ollama".to_string()), &server.base_url).expect("adapter");

    let snapshot = adapter.inspect().await.expect("snapshot");

    assert!(snapshot.available);
    assert!(snapshot.residents.is_empty());
}

#[tokio::test]
async fn ollama_ps_vram_bytes_convert_to_mb() {
    let server = spawn_fixture(vec![http_response(
        200,
        r#"{"models":[{"name":"qwen9b","size_vram":9437184000,"size":12582912000}]}"#,
    )])
    .await;
    let adapter =
        OllamaAdapter::new(RuntimeId("ollama".to_string()), &server.base_url).expect("adapter");

    let snapshot = adapter.inspect().await.expect("snapshot");

    assert_eq!(snapshot.residents[0].vram_mb, Some(9000));
    assert_eq!(snapshot.residents[0].ram_mb, Some(12000));
}

#[tokio::test]
async fn ollama_unavailable_runtime_returns_error_or_unavailable_snapshot() {
    let server = spawn_fixture(vec![http_response(503, "{}")]).await;
    let adapter =
        OllamaAdapter::new(RuntimeId("ollama".to_string()), &server.base_url).expect("adapter");

    let snapshot = adapter.inspect().await.expect("snapshot");

    assert!(!snapshot.available);
    assert!(snapshot.residents.is_empty());
}

#[tokio::test]
async fn ollama_malformed_response_returns_runtime_error() {
    let server = spawn_fixture(vec![http_response(200, "not json")]).await;
    let adapter =
        OllamaAdapter::new(RuntimeId("ollama".to_string()), &server.base_url).expect("adapter");

    let error = adapter.inspect().await.expect_err("malformed json");

    assert!(matches!(error, RuntimeError::Http(_)));
}

#[test]
fn ollama_base_url_validation_rejects_invalid_url() {
    let error =
        OllamaAdapter::new(RuntimeId("ollama".to_string()), "not a url").expect_err("invalid url");

    assert!(error.to_string().contains("invalid runtime url"));
}

#[tokio::test]
async fn llama_cpp_health_marks_runtime_available() {
    let server = spawn_fixture(vec![
        http_response(200, "{}"),
        http_response(200, r#"{"data":[]}"#),
    ])
    .await;
    let adapter = LlamaCppAdapter::new(RuntimeId("llama_cpp".to_string()), &server.base_url)
        .expect("adapter");

    let snapshot = adapter.inspect().await.expect("snapshot");

    assert!(snapshot.available);
}

#[tokio::test]
async fn llama_cpp_failed_health_marks_runtime_unavailable() {
    let server = spawn_fixture(vec![http_response(500, "{}")]).await;
    let adapter = LlamaCppAdapter::new(RuntimeId("llama_cpp".to_string()), &server.base_url)
        .expect("adapter");

    let snapshot = adapter.inspect().await.expect("snapshot");

    assert!(!snapshot.available);
    assert!(snapshot.residents.is_empty());
    assert!(snapshot.configured_models.is_empty());
}

#[tokio::test]
async fn llama_cpp_models_response_normalizes_model_ids() {
    let server = spawn_fixture(vec![http_response(
        200,
        r#"{"data":[{"id":"models/qwen9b.gguf"},{"id":"granite8b"}]}"#,
    )])
    .await;
    let adapter = LlamaCppAdapter::new(RuntimeId("llama_cpp".to_string()), &server.base_url)
        .expect("adapter");

    let models = adapter.inspect_models().await.expect("models");

    assert_eq!(
        models,
        vec![
            ModelId("qwen9b".to_string()),
            ModelId("granite8b".to_string()),
        ]
    );
}

#[tokio::test]
async fn llama_cpp_inspect_surfaces_configured_models_without_claiming_residency() {
    let server = spawn_fixture(vec![
        http_response(200, "{}"),
        http_response(200, r#"{"data":[{"id":"models/qwen9b.gguf"}]}"#),
    ])
    .await;
    let adapter = LlamaCppAdapter::new(RuntimeId("llama_cpp".to_string()), &server.base_url)
        .expect("adapter");

    let snapshot = adapter.inspect().await.expect("snapshot");

    assert_eq!(snapshot.runtime_id, RuntimeId("llama_cpp".to_string()));
    assert!(snapshot.available);
    assert_eq!(
        snapshot.configured_models,
        vec![ModelId("qwen9b".to_string())]
    );
    assert!(
        snapshot.residents.is_empty(),
        "/v1/models proves configuration, not residency"
    );
    assert_eq!(snapshot.memory, RuntimeMemorySnapshot::default());
}

#[tokio::test]
async fn llama_cpp_probe_uses_get_only() {
    let server = spawn_fixture(vec![
        http_response(200, "{}"),
        http_response(200, r#"{"data":[]}"#),
    ])
    .await;
    let adapter = LlamaCppAdapter::new(RuntimeId("llama_cpp".to_string()), &server.base_url)
        .expect("adapter");

    let _ = adapter.inspect().await.expect("inspect");

    let requests = server.requests.lock().expect("requests").clone();
    for request_text in &requests {
        assert!(
            request_text.starts_with("GET"),
            "inspect must use GET only, got: {request_text:?}"
        );
    }
}

#[tokio::test]
async fn llama_cpp_load_unload_execute_are_unsupported() {
    let adapter = LlamaCppAdapter::new(RuntimeId("llama_cpp".to_string()), "http://localhost:8080")
        .expect("adapter");
    let model = ModelId("qwen9b".to_string());

    let load_error = adapter.load_model(&model).await.expect_err("load");
    let unload_error = adapter.unload_model(&model).await.expect_err("unload");
    let execute_error = adapter
        .execute(ExecutionRequest {
            request_id: RequestId::new(),
            model_id: model,
            prompt: None,
        })
        .await
        .expect_err("execute");

    assert_eq!(
        load_error.to_string(),
        "runtime operation is not supported: llama-cpp load"
    );
    assert_eq!(
        unload_error.to_string(),
        "runtime operation is not supported: llama-cpp unload"
    );
    assert_eq!(
        execute_error.to_string(),
        "runtime operation is not supported: llama-cpp execute"
    );
}

#[tokio::test]
async fn llama_cpp_auth_header_is_applied_when_configured() {
    let server = spawn_fixture(vec![
        http_response(200, "{}"),
        http_response(200, r#"{"data":[]}"#),
    ])
    .await;
    let adapter = LlamaCppAdapter::new(RuntimeId("llama_cpp".to_string()), &server.base_url)
        .expect("adapter")
        .with_bearer_token("secret");

    let _ = adapter.inspect().await.expect("snapshot");

    let requests = server.requests.lock().expect("requests").clone();
    assert_eq!(requests.len(), 2);
    assert!(requests.iter().all(|request| request
        .to_ascii_lowercase()
        .contains("authorization: bearer secret")));
}

#[tokio::test]
async fn llama_cpp_timeout_returns_runtime_error() {
    let base_url = spawn_timeout_server().await;
    let adapter = LlamaCppAdapter::new_with_timeout(
        RuntimeId("llama_cpp".to_string()),
        &base_url,
        Duration::from_millis(50),
    )
    .expect("adapter");

    let error = adapter.inspect().await.expect_err("timeout");

    assert!(matches!(error, RuntimeError::Http(error) if error.is_timeout()));
}

#[test]
fn llama_cpp_base_url_validation_rejects_invalid_url() {
    let error = LlamaCppAdapter::new(RuntimeId("llama_cpp".to_string()), "not a url")
        .expect_err("invalid url");

    assert!(error.to_string().contains("invalid runtime url"));
}

#[tokio::test]
async fn inference_gateway_injects_runtime_auth_token() {
    let server = spawn_fixture(vec![http_response(200, "{}")]).await;
    let target = ForwardTarget {
        base_url: server.base_url.clone(),
        auth_token: Some("s3cr3t-token".to_string()),
    };
    let body = serde_json::json!({ "model": "qwen9b", "messages": [] });

    forward_chat_completion(&target, &body)
        .await
        .expect("forward");

    let request = server.requests.lock().expect("requests")[0].to_lowercase();
    assert!(
        request.contains("authorization: bearer s3cr3t-token"),
        "forwarded request must carry the runtime bearer token, got:\n{request}"
    );
}

#[tokio::test]
async fn forward_chat_completion_omits_auth_when_unset() {
    let server = spawn_fixture(vec![http_response(200, "{}")]).await;
    let target = ForwardTarget {
        base_url: server.base_url.clone(),
        auth_token: None,
    };
    let body = serde_json::json!({ "model": "qwen9b", "messages": [] });

    forward_chat_completion(&target, &body)
        .await
        .expect("forward");

    let request = server.requests.lock().expect("requests")[0].to_lowercase();
    assert!(
        !request.contains("authorization:"),
        "no auth header should be sent when the runtime has no token"
    );
}

struct TestServer {
    base_url: String,
    requests: Arc<Mutex<Vec<String>>>,
}

async fn spawn_fixture(responses: Vec<String>) -> TestServer {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let requests = Arc::new(Mutex::new(Vec::new()));
    let request_log = requests.clone();
    let responses = Arc::new(Mutex::new(VecDeque::from(responses)));
    let response_queue = responses.clone();

    tokio::spawn(async move {
        loop {
            let Some(response) = response_queue.lock().expect("responses").pop_front() else {
                break;
            };
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut buffer = vec![0; 4096];
            let read = socket.read(&mut buffer).await.expect("read");
            request_log
                .lock()
                .expect("requests")
                .push(String::from_utf8_lossy(&buffer[..read]).to_string());
            socket
                .write_all(response.as_bytes())
                .await
                .expect("write response");
        }
    });

    TestServer {
        base_url: format!("http://{addr}"),
        requests,
    }
}

async fn spawn_timeout_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("accept");
        let mut buffer = vec![0; 1024];
        let _ = socket.read(&mut buffer).await;
        tokio::time::sleep(Duration::from_secs(2)).await;
    });
    format!("http://{addr}")
}

fn http_response(status: u16, body: &str) -> String {
    let reason = match status {
        200 => "OK",
        500 => "Internal Server Error",
        _ => "Unknown",
    };
    format!(
        "HTTP/1.1 {status} {reason}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
        body.len()
    )
}

fn m(id: &str) -> ModelId {
    ModelId::from(id)
}

#[test]
fn matrix_parses_vars_evict_costs_and_sets() {
    let matrix = LlamaSwapMatrixConfig::from_yaml_str(
        r#"
matrix:
  vars:
    gpu: 24576
  evict_costs:
    minimax: 90000
    qwen9b: 1
  sets:
    - name: small_plus_medium
      models: qwen9b & qwen35_a3b
"#,
    )
    .expect("parse")
    .expect("matrix block present");

    assert_eq!(matrix.numeric_var("gpu"), Some(24_576));
    assert_eq!(matrix.evict_costs.get("minimax"), Some(&90_000));
    assert_eq!(matrix.evict_costs.get("qwen9b"), Some(&1));
    assert_eq!(matrix.sets.len(), 1);
    assert_eq!(matrix.sets[0].name, "small_plus_medium");
    assert_eq!(matrix.sets[0].models, "qwen9b & qwen35_a3b");
}

#[test]
fn can_colocate_true_for_models_in_shared_and_set() {
    let matrix = LlamaSwapMatrixConfig::from_yaml_str(
        r#"
matrix:
  sets:
    - name: pair
      models: qwen9b & qwen35_a3b
"#,
    )
    .expect("parse")
    .expect("matrix");

    assert!(matrix.can_colocate(&m("qwen9b"), &m("qwen35_a3b")));
    assert!(matrix.can_colocate(&m("qwen35_a3b"), &m("qwen9b")));
}

#[test]
fn matrix_lowers_to_policy_facing_colocation_constraints() {
    let matrix = LlamaSwapMatrixConfig::from_yaml_str(
        r#"
matrix:
  sets:
    - name: pair
      models: qwen9b & qwen35_a3b
    - name: solo
      models: minimax
"#,
    )
    .expect("parse")
    .expect("matrix");

    // The lowered, runtime-agnostic form answers the same colocation
    // questions as the matrix itself — this is what the policy consumes off
    // the observed snapshot.
    let constraints = matrix.colocation_constraints();
    assert!(constraints.can_colocate(&m("qwen9b"), &m("qwen35_a3b")));
    assert!(!constraints.can_colocate(&m("qwen9b"), &m("minimax")));
}

#[test]
fn adapter_surfaces_no_colocation_constraints_without_matrix() {
    let adapter = LlamaSwapAdapter::new(RuntimeId("ls".to_string()), "http://localhost:8085")
        .expect("adapter");
    assert!(adapter.colocation_constraints().is_none());
}

#[test]
fn can_colocate_false_for_models_not_in_any_set_together() {
    let matrix = LlamaSwapMatrixConfig::from_yaml_str(
        r#"
matrix:
  sets:
    - name: pair
      models: qwen9b & qwen35_a3b
    - name: solo
      models: minimax
"#,
    )
    .expect("parse")
    .expect("matrix");

    // minimax runs alone — it shares no set with the colocating pair.
    assert!(!matrix.can_colocate(&m("qwen9b"), &m("minimax")));
    assert!(!matrix.can_colocate(&m("qwen35_a3b"), &m("minimax")));
    // A model absent from every set never colocates.
    assert!(!matrix.can_colocate(&m("qwen9b"), &m("gemma")));
}

#[test]
fn or_branches_are_alternatives_not_colocated() {
    let matrix = LlamaSwapMatrixConfig::from_yaml_str(
        r#"
matrix:
  sets:
    - name: either
      models: qwen9b | qwen35_a3b
"#,
    )
    .expect("parse")
    .expect("matrix");

    assert!(!matrix.can_colocate(&m("qwen9b"), &m("qwen35_a3b")));
}

#[test]
fn grouping_colocates_each_alternative_with_shared_factor() {
    let matrix = LlamaSwapMatrixConfig::from_yaml_str(
        r#"
matrix:
  sets:
    - name: grouped
      models: (qwen4b | qwen2b) & gemma_e2b
"#,
    )
    .expect("parse")
    .expect("matrix");

    assert!(matrix.can_colocate(&m("qwen4b"), &m("gemma_e2b")));
    assert!(matrix.can_colocate(&m("qwen2b"), &m("gemma_e2b")));
    // The two alternatives never load together.
    assert!(!matrix.can_colocate(&m("qwen4b"), &m("qwen2b")));
}

#[test]
fn full_llama_swap_config_ignores_unrelated_keys() {
    let matrix = LlamaSwapMatrixConfig::from_yaml_str(
        r#"
healthCheckTimeout: 90
models:
  qwen9b-co:
    cmd: llama-server -m qwen9b.gguf
  minimax:
    cmd: llama-server -m minimax.gguf
groups:
  default:
    - qwen9b-co
matrix:
  sets:
    - name: pair
      models: qwen9b-co & qwen35_a3b-co
"#,
    )
    .expect("parse")
    .expect("matrix");

    // Model ids carry the `-co` colocation suffix verbatim from the config.
    assert!(matrix.can_colocate(&m("qwen9b-co"), &m("qwen35_a3b-co")));
    assert!(!matrix.can_colocate(&m("qwen9b-co"), &m("minimax")));
}

#[test]
fn dict_format_sets_parse_like_list_format() {
    let matrix = LlamaSwapMatrixConfig::from_yaml_str(
        r#"
matrix:
  sets:
    pair: qwen9b & qwen35_a3b
"#,
    )
    .expect("parse")
    .expect("matrix");

    assert!(matrix.can_colocate(&m("qwen9b"), &m("qwen35_a3b")));
    assert!(!matrix.can_colocate(&m("qwen9b"), &m("minimax")));
}

#[test]
fn string_var_aliases_expand_in_colocation_check() {
    let matrix = LlamaSwapMatrixConfig::from_yaml_str(
        r#"
matrix:
  vars:
    q9m: qwen3.5-9b-mtp
    q35co: qwen3.6-35b-a3b-mtp-co
    ge2m: gemma-4-e2b-it
  sets:
    small_pair: q9m & q35co
    big_pool: q35co | ge2m
"#,
    )
    .expect("parse")
    .expect("matrix");

    // Aliases expand to full model IDs for can_colocate.
    assert!(matrix.can_colocate(&m("qwen3.5-9b-mtp"), &m("qwen3.6-35b-a3b-mtp-co")));
    // | alternatives do not colocate even after alias expansion.
    assert!(!matrix.can_colocate(&m("qwen3.6-35b-a3b-mtp-co"), &m("gemma-4-e2b-it")));
    // Models not in any & set never colocate.
    assert!(!matrix.can_colocate(&m("qwen3.5-9b-mtp"), &m("gemma-4-e2b-it")));
}

#[test]
fn config_without_matrix_block_parses_to_none() {
    let parsed = LlamaSwapMatrixConfig::from_yaml_str(
        r#"
healthCheckTimeout: 90
models:
  qwen9b:
    cmd: llama-server -m qwen9b.gguf
"#,
    )
    .expect("parse");

    assert!(parsed.is_none());
}

#[test]
fn invalid_yaml_is_a_config_error() {
    let result = LlamaSwapMatrixConfig::from_yaml_str("matrix: [unterminated");

    assert!(matches!(result, Err(RuntimeError::Config(_))));
}

#[test]
fn from_yaml_file_reads_matrix_block() {
    let path = std::env::temp_dir().join(format!("anemoi-matrix-{}.yaml", Uuid::new_v4()));
    std::fs::write(
        &path,
        "matrix:\n  sets:\n    - name: pair\n      models: qwen9b & qwen35_a3b\n",
    )
    .expect("write fixture");

    let matrix = LlamaSwapMatrixConfig::from_yaml_file(&path)
        .expect("parse")
        .expect("matrix");
    let _ = std::fs::remove_file(&path);

    assert!(matrix.can_colocate(&m("qwen9b"), &m("qwen35_a3b")));
}

#[test]
fn adapter_can_colocate_false_without_matrix() {
    let adapter =
        LlamaSwapAdapter::new(RuntimeId("llama_swap".to_string()), "http://localhost:8080")
            .expect("adapter");

    assert!(adapter.matrix().is_none());
    assert!(!adapter.can_colocate(&m("qwen9b"), &m("qwen35_a3b")));
}

#[test]
fn adapter_can_colocate_uses_matrix() {
    let matrix = LlamaSwapMatrixConfig::from_yaml_str(
        r#"
matrix:
  sets:
    - name: pair
      models: qwen9b & qwen35_a3b
"#,
    )
    .expect("parse")
    .expect("matrix");
    let adapter =
        LlamaSwapAdapter::new(RuntimeId("llama_swap".to_string()), "http://localhost:8080")
            .expect("adapter")
            .with_matrix_config(matrix);

    assert!(adapter.can_colocate(&m("qwen9b"), &m("qwen35_a3b")));
    assert!(!adapter.can_colocate(&m("qwen9b"), &m("minimax")));
}
