use super::*;

#[test]
fn quality_floor_rejects_candidates_below_minimum_parameter_class() {
    let scheduler = Scheduler::new(fast_only_config());
    let decision = scheduler
        .decide(
            &request_with_quality_floor("32b"),
            &[candidate_snapshot(true)],
        )
        .expect("decision");
    assert_eq!(decision.action, DecisionAction::Deny);
    assert_eq!(decision.selected_model, None);
    assert!(
        decision
            .explanation
            .rejected_options
            .iter()
            .any(|rejected| {
                rejected.model_id == Some(ModelId("qwen9b".to_string()))
                    && rejected.reason.contains("9b")
                    && rejected.reason.contains("32b")
            }),
        "a denied quality-floor request must explain the undersized candidate"
    );
}

#[test]
fn quality_floor_allows_candidate_at_or_above_minimum_parameter_class() {
    let scheduler = Scheduler::new(candidate_config());
    let mut request = request_with_quality_floor("32b");
    request.latency_budget_ms = Some(60_000);
    let decision = scheduler
        .decide(&request, &[candidate_snapshot(true)])
        .expect("decision");
    assert_eq!(decision.action, DecisionAction::ColdLoad);
    assert_eq!(
        decision.selected_model,
        Some(ModelId("qwen35_a3b".to_string()))
    );
    assert_eq!(decision.background_model, None);
    assert!(
        decision
            .explanation
            .rejected_options
            .iter()
            .any(|rejected| {
                rejected.model_id == Some(ModelId("qwen9b".to_string()))
                    && rejected.reason.contains("quality floor 32b")
            }),
        "the smaller hot worker should be rejected by the explicit quality floor"
    );
}

#[test]
fn quality_floor_explanation_names_requested_and_candidate_parameter_class() {
    let scheduler = Scheduler::new(candidate_config());
    let decision = scheduler
        .decide(
            &request_with_quality_floor("32b"),
            &[candidate_snapshot(true)],
        )
        .expect("decision");
    let reason = decision
        .explanation
        .reasons
        .iter()
        .find(|reason| reason.code == "quality_floor.degraded_fallback")
        .expect("quality-floor degraded fallback reason");
    assert!(reason.detail.contains("32b"));
    assert!(reason.detail.contains("qwen9b"));
    assert!(reason.detail.contains("9b"));
    assert!(reason.detail.contains("qwen35_a3b"));
    assert!(reason.detail.contains("35b"));
}

#[test]
fn escalation_selects_large_hot_model_when_available() {
    let scheduler = Scheduler::new(candidate_config());
    let snapshot = candidate_snapshot_with_residents(
        true,
        vec![
            ("qwen9b", ResidencyState::HotGpu, Some(9000)),
            ("qwen35_a3b", ResidencyState::HotGpu, Some(30000)),
        ],
    );
    let decision = scheduler
        .decide(&request_with_quality_floor("32b"), &[snapshot])
        .expect("decision");
    assert_eq!(decision.action, DecisionAction::ReuseHot);
    assert_eq!(
        decision.selected_model,
        Some(ModelId("qwen35_a3b".to_string()))
    );
    assert_eq!(decision.background_model, None);
}

#[test]
fn escalation_uses_hot_worker_and_stages_large_model_when_latency_is_tight() {
    let scheduler = Scheduler::new(candidate_config());
    let decision = scheduler
        .decide(
            &request_with_quality_floor("32b"),
            &[candidate_snapshot(true)],
        )
        .expect("decision");
    assert_eq!(decision.action, DecisionAction::StageBackground);
    assert_eq!(decision.selected_model, Some(ModelId("qwen9b".to_string())));
    assert_eq!(
        decision.background_model,
        Some(ModelId("qwen35_a3b".to_string()))
    );
    assert!(decision.explanation.reasons.iter().any(|reason| {
        reason.code == "quality_floor.degraded_fallback"
            && reason.detail.contains("selected hot qwen9b")
            && reason.detail.contains("staging qualifying qwen35_a3b")
    }));
}

#[test]
fn escalation_does_not_silently_satisfy_32b_request_with_9b() {
    let scheduler = Scheduler::new(fast_only_config());
    let decision = scheduler
        .decide(
            &request_with_quality_floor("32b"),
            &[candidate_snapshot(true)],
        )
        .expect("decision");
    assert_eq!(decision.action, DecisionAction::Deny);
    assert_ne!(decision.selected_model, Some(ModelId("qwen9b".to_string())));
    assert!(
        decision
            .explanation
            .rejected_options
            .iter()
            .any(|rejected| {
                rejected.model_id == Some(ModelId("qwen9b".to_string()))
                    && rejected.reason.contains("quality floor 32b")
            }),
        "a 32b request cannot be quietly satisfied by the 9b-only roster"
    );
}
