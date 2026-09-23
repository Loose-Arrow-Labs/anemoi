use super::*;
use anemoi_core::{
    ColocationConstraints, DecisionAction, DecisionScore, ExecutionMode, ModelId, ModelProfile,
    ModelResident, RequestId, ResidencyState, RuntimeMemorySnapshot, RuntimeSnapshot,
};

mod context_window_tests;
mod continuity_tests;
mod eviction_tests;
mod profile_tests;
mod quality_floor_tests;
mod scheduler_tests;
mod scoring_tests;

// -- Shared test helpers -------------------------------------------------------

pub(super) fn candidate_request() -> InferenceRequest {
    InferenceRequest {
        id: RequestId::new(),
        domain: DomainId("coding".to_string()),
        mode: ExecutionMode::Interactive,
        prompt_tokens_estimate: Some(1000),
        max_output_tokens: Some(500),
        latency_budget_ms: Some(1500),
        quality_floor: None,
        escalation_intent: None,
    }
}

pub(super) fn candidate_snapshot(available: bool) -> RuntimeSnapshot {
    candidate_snapshot_with_residents(
        available,
        vec![("qwen9b", ResidencyState::HotGpu, Some(9000))],
    )
}

pub(super) fn candidate_snapshot_with_residents(
    available: bool,
    residents: Vec<(&str, ResidencyState, Option<u64>)>,
) -> RuntimeSnapshot {
    RuntimeSnapshot {
        runtime_id: RuntimeId("mock".to_string()),
        available,
        residents: residents
            .into_iter()
            .map(|(model_id, state, vram_mb)| ModelResident {
                model_id: ModelId(model_id.to_string()),
                state,
                vram_mb,
                ram_mb: None,
                kv_cache_mb: None,
                loaded_since: None,
            })
            .collect(),
        configured_models: Vec::new(),
        memory: RuntimeMemorySnapshot::default(),
        active_requests: Vec::new(),
        colocation: None,
    }
}

pub(super) fn candidate_config() -> AnemoiConfig {
    serde_yaml::from_str(
        r#"
domains:
  coding:
    rosters: [small_swarm, large_models]
residency_groups:
  small_swarm:
    keep_hot: true
    allow_background_load: true
    models: [qwen9b, granite8b]
  large_models:
    keep_hot: false
    allow_background_load: true
    models: [qwen35_a3b]
models:
  qwen9b:
    family: qwen
    parameter_class: 9b
    context_window: 32768
    vram_required_mb: 9000
    ram_required_mb: 12000
    cold_load_estimate_ms: 18000
    supported_runtimes: [mock]
  granite8b:
    family: granite
    parameter_class: 8b
    context_window: 8192
    vram_required_mb: 8000
    ram_required_mb: 10000
    cold_load_estimate_ms: 15000
    supported_runtimes: [mock]
  qwen35_a3b:
    family: qwen
    parameter_class: 35b
    context_window: 32768
    vram_required_mb: 30000
    ram_required_mb: 45000
    cold_load_estimate_ms: 45000
    supported_runtimes: [mock]
runtimes:
  mock:
    adapter: mock
"#,
    )
    .expect("candidate config")
}

pub(super) fn fast_only_config() -> AnemoiConfig {
    serde_yaml::from_str(
        r#"
domains:
  coding:
    rosters: [small_swarm]
residency_groups:
  small_swarm:
    keep_hot: true
    allow_background_load: true
    models: [qwen9b]
models:
  qwen9b:
    family: qwen
    parameter_class: 9b
    context_window: 32768
    vram_required_mb: 9000
    ram_required_mb: 12000
    cold_load_estimate_ms: 18000
    supported_runtimes: [mock]
runtimes:
  mock:
    adapter: mock
"#,
    )
    .expect("fast-only config")
}

pub(super) fn request_with_quality_floor(floor: &str) -> InferenceRequest {
    let mut request = candidate_request();
    request.quality_floor = Some(anemoi_core::QualityFloor {
        minimum_parameter_class: Some(floor.to_string()),
    });
    request
}

// Two models with identical profiles in one keep-hot group; `model_order`
// controls the order they are listed (and therefore generated). When both
// are hot-resident they score identically, exercising the score-tie path.
pub(super) fn tie_config_ordered(model_order: &str) -> AnemoiConfig {
    let profile = "{ family: qwen, parameter_class: 9b, context_window: 32768, \
         vram_required_mb: 9000, ram_required_mb: 12000, cold_load_estimate_ms: 18000, \
         supported_runtimes: [mock] }";
    let yaml = format!(
        "domains:\n  coding:\n    rosters: [swarm]\n\
         residency_groups:\n  swarm:\n    keep_hot: true\n    allow_background_load: true\n    models: {model_order}\n\
         models:\n  alpha: {profile}\n  beta: {profile}\n\
         runtimes:\n  mock:\n    adapter: mock\n"
    );
    serde_yaml::from_str(&yaml).expect("tie config")
}

pub(super) fn both_hot_snapshot() -> RuntimeSnapshot {
    let hot = |id: &str| ModelResident {
        model_id: ModelId(id.to_string()),
        state: ResidencyState::HotGpu,
        vram_mb: Some(9000),
        ram_mb: None,
        kv_cache_mb: None,
        loaded_since: None,
    };
    RuntimeSnapshot {
        runtime_id: RuntimeId("mock".to_string()),
        available: true,
        residents: vec![hot("alpha"), hot("beta")],
        configured_models: Vec::new(),
        memory: RuntimeMemorySnapshot::default(),
        active_requests: Vec::new(),
        colocation: None,
    }
}
