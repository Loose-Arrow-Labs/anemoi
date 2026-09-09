use super::*;

#[test]
fn pressure_model_calculates_vram_pressure_from_snapshot() {
    let memory = RuntimeMemorySnapshot {
        vram_total_mb: Some(10_000),
        vram_used_mb: Some(7_500),
        ram_total_mb: None,
        ram_used_mb: None,
    };
    let assessment = PressureModel::default().assess(&PressureInputs {
        memory: &memory,
        vram_required_mb: Some(1_000),
        ram_required_mb: None,
        is_cold_load: false,
        active_request_count: 0,
    });
    assert_eq!(assessment.vram, Pressure::Known(0.75));
}

#[test]
fn pressure_model_calculates_ram_pressure_from_snapshot() {
    let memory = RuntimeMemorySnapshot {
        vram_total_mb: None,
        vram_used_mb: None,
        ram_total_mb: Some(8_000),
        ram_used_mb: Some(6_000),
    };
    let assessment = PressureModel::default().assess(&PressureInputs {
        memory: &memory,
        vram_required_mb: None,
        ram_required_mb: Some(2_000),
        is_cold_load: false,
        active_request_count: 0,
    });
    assert_eq!(assessment.ram, Pressure::Known(0.75));
}

#[test]
fn pressure_model_preserves_unknown_when_capacity_is_missing() {
    let memory = RuntimeMemorySnapshot {
        vram_total_mb: None,
        vram_used_mb: Some(5_000),
        ram_total_mb: None,
        ram_used_mb: Some(4_000),
    };
    let assessment = PressureModel::default().assess(&PressureInputs {
        memory: &memory,
        vram_required_mb: Some(2_000),
        ram_required_mb: Some(2_000),
        is_cold_load: true,
        active_request_count: 0,
    });
    // Missing capacity must stay unknown, never collapse into 0.0 pressure.
    assert_eq!(assessment.vram, Pressure::Unknown);
    assert_eq!(assessment.ram, Pressure::Unknown);
    assert_ne!(assessment.vram, Pressure::Known(0.0));
    assert_ne!(assessment.ram, Pressure::Known(0.0));
}

#[test]
fn high_pressure_penalizes_cold_load_candidate() {
    let memory = RuntimeMemorySnapshot {
        vram_total_mb: Some(10_000),
        vram_used_mb: Some(9_000),
        ram_total_mb: Some(16_000),
        ram_used_mb: Some(8_000),
    };
    let model = PressureModel::default();
    let cold = model.assess(&PressureInputs {
        memory: &memory,
        vram_required_mb: Some(2_000),
        ram_required_mb: Some(2_000),
        is_cold_load: true,
        active_request_count: 0,
    });
    let reuse = model.assess(&PressureInputs {
        memory: &memory,
        vram_required_mb: Some(2_000),
        ram_required_mb: Some(2_000),
        is_cold_load: false,
        active_request_count: 0,
    });
    assert!(
        cold.penalty < 0,
        "cold load under high pressure must be penalized, got {}",
        cold.penalty
    );
    assert!(
        cold.penalty < reuse.penalty,
        "cold load ({}) must be penalized more than reuse ({})",
        cold.penalty,
        reuse.penalty
    );
}

#[test]
fn pressure_explanation_names_vram_ram_and_unknown_inputs() {
    let memory = RuntimeMemorySnapshot {
        vram_total_mb: Some(10_000),
        vram_used_mb: Some(5_000),
        ram_total_mb: None,
        ram_used_mb: None,
    };
    let assessment = PressureModel::default().assess(&PressureInputs {
        memory: &memory,
        vram_required_mb: Some(1_000),
        ram_required_mb: Some(2_000),
        is_cold_load: true,
        active_request_count: 0,
    });
    assert!(
        assessment
            .reasons
            .iter()
            .any(|reason| reason.code.contains("vram")),
        "expected a vram pressure reason"
    );
    assert!(
        assessment
            .reasons
            .iter()
            .any(|reason| reason.code.contains("ram") && !reason.code.contains("vram")),
        "expected a ram pressure reason distinct from vram"
    );
    assert!(
        assessment
            .reasons
            .iter()
            .any(|reason| reason.detail.to_lowercase().contains("unknown")),
        "expected an explicit unknown-capacity reason"
    );
}

#[test]
fn active_request_pressure_penalizes_busy_runtime() {
    let memory = RuntimeMemorySnapshot::default();
    let model = PressureModel::default();
    let busy = model.assess(&PressureInputs {
        memory: &memory,
        vram_required_mb: None,
        ram_required_mb: None,
        is_cold_load: false,
        active_request_count: 4,
    });
    let idle = model.assess(&PressureInputs {
        memory: &memory,
        vram_required_mb: None,
        ram_required_mb: None,
        is_cold_load: false,
        active_request_count: 0,
    });
    assert!(
        busy.penalty < idle.penalty,
        "busy runtime ({}) must score lower than idle ({})",
        busy.penalty,
        idle.penalty
    );
    assert!(busy
        .reasons
        .iter()
        .any(|reason| { reason.code.contains("active_request") && reason.impact < 0 }));
}

#[test]
fn scores_hot_small_model_before_large_model_for_fast_turn() {
    let config = candidate_config();
    let scheduler = Scheduler::new(config);
    let decision = scheduler
        .decide(&candidate_request(), &[candidate_snapshot(true)])
        .expect("decision");
    assert_eq!(decision.action, DecisionAction::StageBackground);
    assert_eq!(decision.selected_model, Some(ModelId("qwen9b".to_string())));
    assert_eq!(
        decision.background_model,
        Some(ModelId("qwen35_a3b".to_string()))
    );
}

#[test]
fn scores_keep_hot_resident_candidate_higher_than_cold_candidate() {
    let mut config = candidate_config();
    config.continuity.background_load = false;
    let scheduler = Scheduler::new(config);
    let decision = scheduler
        .decide(&candidate_request(), &[candidate_snapshot(true)])
        .expect("decision");
    assert_eq!(decision.action, DecisionAction::ReuseHot);
    assert_eq!(decision.selected_model, Some(ModelId("qwen9b".to_string())));
}

#[test]
fn scores_large_model_higher_when_request_needs_quality_floor() {
    let scheduler = Scheduler::new(candidate_config());
    let mut request = candidate_request();
    request.quality_floor = Some(anemoi_core::QualityFloor {
        minimum_parameter_class: Some("35b".to_string()),
    });
    request.latency_budget_ms = Some(90_000);
    let decision = scheduler
        .decide(&request, &[candidate_snapshot(true)])
        .expect("decision");
    assert_eq!(decision.action, DecisionAction::ColdLoad);
    assert_eq!(
        decision.selected_model,
        Some(ModelId("qwen35_a3b".to_string()))
    );
}

#[test]
fn score_explanation_includes_reasons() {
    let scheduler = Scheduler::new(candidate_config());
    let decision = scheduler
        .decide(&candidate_request(), &[candidate_snapshot(true)])
        .expect("decision");
    let labels = decision
        .score
        .contributions
        .iter()
        .map(|contribution| contribution.label.clone())
        .collect::<Vec<_>>();
    assert!(labels.contains(&"residency".to_string()));
    assert!(labels.contains(&"latency_budget".to_string()));
    assert!(labels.contains(&"continuity background staging".to_string()));
}
