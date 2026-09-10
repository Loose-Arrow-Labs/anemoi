use super::*;

#[test]
fn avoids_cold_large_model_when_small_worker_is_hot() {
    let config: AnemoiConfig = serde_yaml::from_str(
        r#"
domains:
  coding:
    rosters: [small_swarm, large_models]
residency_groups:
  small_swarm:
    keep_hot: true
    allow_background_load: true
    models: [qwen9b]
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
    supported_runtimes: [ollama]
  qwen35_a3b:
    family: qwen
    parameter_class: 35b
    context_window: 32768
    vram_required_mb: 30000
    ram_required_mb: 45000
    cold_load_estimate_ms: 45000
    supported_runtimes: [ollama]
runtimes:
  ollama:
    adapter: mock
continuity:
  keep_small_worker_hot: true
  background_load: true
  max_blank_wait_ms: 1500
  prefer_degraded_response_over_silence: true
"#,
    )
    .expect("valid config");
    let scheduler = Scheduler::new(config);
    let request = InferenceRequest {
        id: RequestId::new(),
        domain: DomainId("coding".to_string()),
        mode: ExecutionMode::Interactive,
        prompt_tokens_estimate: Some(2000),
        max_output_tokens: Some(800),
        latency_budget_ms: Some(1500),
        quality_floor: None,
        escalation_intent: None,
    };
    let snapshot = RuntimeSnapshot {
        runtime_id: RuntimeId("ollama".to_string()),
        available: true,
        residents: vec![ModelResident {
            model_id: ModelId("qwen9b".to_string()),
            state: ResidencyState::HotGpu,
            vram_mb: Some(9000),
            ram_mb: None,
            kv_cache_mb: None,
            loaded_since: None,
        }],
        configured_models: Vec::new(),
        memory: RuntimeMemorySnapshot::default(),
        active_requests: Vec::new(),
        colocation: None,
    };
    let decision = scheduler.decide(&request, &[snapshot]).expect("decision");
    assert_eq!(decision.action, DecisionAction::StageBackground);
    assert_eq!(decision.selected_model, Some(ModelId("qwen9b".to_string())));
    assert_eq!(
        decision.background_model,
        Some(ModelId("qwen35_a3b".to_string()))
    );
    assert!(decision
        .explanation
        .reasons
        .iter()
        .any(|reason| reason.code == "continuity.stage_background"));
}

#[test]
fn colocation_matrix_gates_background_staging() {
    let config: AnemoiConfig = serde_yaml::from_str(
        r#"
domains:
  coding:
    rosters: [small_swarm, large_models]
residency_groups:
  small_swarm:
    keep_hot: true
    allow_background_load: true
    models: [qwen9b]
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
    supported_runtimes: [ollama]
  qwen35_a3b:
    family: qwen
    parameter_class: 35b
    context_window: 32768
    vram_required_mb: 30000
    ram_required_mb: 45000
    cold_load_estimate_ms: 45000
    supported_runtimes: [ollama]
runtimes:
  ollama:
    adapter: mock
continuity:
  keep_small_worker_hot: true
  background_load: true
  max_blank_wait_ms: 1500
  prefer_degraded_response_over_silence: true
"#,
    )
    .expect("valid config");
    let request = InferenceRequest {
        id: RequestId::new(),
        domain: DomainId("coding".to_string()),
        mode: ExecutionMode::Interactive,
        prompt_tokens_estimate: Some(2000),
        max_output_tokens: Some(800),
        latency_budget_ms: Some(1500),
        quality_floor: None,
        escalation_intent: None,
    };
    let m = |id: &str| ModelId(id.to_string());
    let snapshot_with = |colocation: Option<ColocationConstraints>| RuntimeSnapshot {
        runtime_id: RuntimeId("ollama".to_string()),
        available: true,
        residents: vec![ModelResident {
            model_id: ModelId("qwen9b".to_string()),
            state: ResidencyState::HotGpu,
            vram_mb: Some(9000),
            ram_mb: None,
            kv_cache_mb: None,
            loaded_since: None,
        }],
        configured_models: Vec::new(),
        memory: RuntimeMemorySnapshot::default(),
        active_requests: Vec::new(),
        colocation,
    };
    let scheduler = Scheduler::new(config);
    let allowed = scheduler
        .decide(
            &request,
            &[snapshot_with(Some(ColocationConstraints {
                loadouts: vec![vec![m("qwen9b"), m("qwen35_a3b")]],
            }))],
        )
        .expect("decision");
    assert_eq!(allowed.action, DecisionAction::StageBackground);
    assert_eq!(allowed.background_model, Some(m("qwen35_a3b")));
    let forbidden = scheduler
        .decide(
            &request,
            &[snapshot_with(Some(ColocationConstraints {
                loadouts: vec![vec![m("qwen9b")], vec![m("qwen35_a3b")]],
            }))],
        )
        .expect("decision");
    assert_ne!(forbidden.action, DecisionAction::StageBackground);
    assert_eq!(forbidden.background_model, None);
    assert_eq!(forbidden.selected_model, Some(m("qwen9b")));
    assert!(
        forbidden
            .explanation
            .reasons
            .iter()
            .any(|reason| reason.code == "continuity.stage_blocked_colocation"),
        "a matrix-forbidden co-resident stage must be explained"
    );
}

#[test]
fn does_not_stage_background_when_policy_disallows_background_load() {
    let mut config = candidate_config();
    config.continuity.background_load = false;
    let scheduler = Scheduler::new(config);
    let decision = scheduler
        .decide(&candidate_request(), &[candidate_snapshot(true)])
        .expect("decision");
    assert_ne!(decision.action, DecisionAction::StageBackground);
    assert_eq!(decision.background_model, None);
}

#[test]
fn does_not_stage_background_when_latency_budget_allows_cold_load() {
    let scheduler = Scheduler::new(candidate_config());
    let mut request = candidate_request();
    request.latency_budget_ms = Some(60_000);
    let decision = scheduler
        .decide(&request, &[candidate_snapshot(true)])
        .expect("decision");
    assert_ne!(decision.action, DecisionAction::StageBackground);
    assert_eq!(decision.background_model, None);
}

#[test]
fn does_not_stage_background_without_hot_fallback() {
    let scheduler = Scheduler::new(candidate_config());
    let snapshot = RuntimeSnapshot {
        runtime_id: RuntimeId("mock".to_string()),
        available: true,
        residents: Vec::new(),
        configured_models: Vec::new(),
        memory: RuntimeMemorySnapshot::default(),
        active_requests: Vec::new(),
        colocation: None,
    };
    let decision = scheduler
        .decide(&candidate_request(), &[snapshot])
        .expect("decision");
    assert_ne!(decision.action, DecisionAction::StageBackground);
    assert_eq!(decision.background_model, None);
}

#[test]
fn records_background_model_in_decision() {
    let scheduler = Scheduler::new(candidate_config());
    let decision = scheduler
        .decide(&candidate_request(), &[candidate_snapshot(true)])
        .expect("decision");
    assert_eq!(decision.action, DecisionAction::StageBackground);
    assert_eq!(
        decision.background_model,
        Some(ModelId("qwen35_a3b".to_string()))
    );
}

#[test]
fn explanation_names_selected_and_staged_models() {
    let scheduler = Scheduler::new(candidate_config());
    let decision = scheduler
        .decide(&candidate_request(), &[candidate_snapshot(true)])
        .expect("decision");
    let continuity_reason = decision
        .explanation
        .reasons
        .iter()
        .find(|reason| reason.code == "continuity.stage_background")
        .expect("continuity reason");
    assert!(continuity_reason.detail.contains("qwen9b"));
    assert!(continuity_reason.detail.contains("qwen35_a3b"));
    assert!(continuity_reason.detail.contains("45000ms"));
    assert!(continuity_reason.detail.contains("1500ms"));
    assert!(continuity_reason
        .detail
        .contains("prefers degraded response over silence"));
}

#[test]
fn score_includes_continuity_contribution() {
    let scheduler = Scheduler::new(candidate_config());
    let decision = scheduler
        .decide(&candidate_request(), &[candidate_snapshot(true)])
        .expect("decision");
    assert!(decision
        .score
        .contributions
        .iter()
        .any(
            |contribution| contribution.label == "continuity background staging"
                && contribution.value == 50
        ));
}

#[cfg(debug_assertions)]
#[test]
#[should_panic(expected = "StageBackground decision must carry a background_model")]
fn into_decision_panics_on_stage_background_without_model() {
    let candidate = Candidate {
        action: DecisionAction::StageBackground,
        model_id: ModelId("qwen9b".to_string()),
        runtime_id: RuntimeId("mock".to_string()),
        group_id: ResidencyGroupId("small_swarm".to_string()),
        model_profile: ModelProfile {
            id: ModelId("qwen9b".to_string()),
            family: "qwen".to_string(),
            parameter_class: "9b".to_string(),
            context_window: None,
            vram_required_mb: None,
            ram_required_mb: None,
            cold_load_estimate_ms: None,
            supported_runtimes: vec![RuntimeId("mock".to_string())],
            supports_streaming: None,
        },
        residency_state: ResidencyState::HotGpu,
        load_estimate_ms: 0,
        runtime_memory: RuntimeMemorySnapshot::default(),
        active_request_count: 0,
        group_keep_hot: false,
        colocation: None,
    };
    let scored = crate::scoring::ScoredCandidate {
        action: DecisionAction::StageBackground,
        candidate,
        background_model: None,
        score: DecisionScore::default(),
        reasons: Vec::new(),
    };
    let _ = scored.into_decision(&candidate_request(), Vec::new());
}
