use super::*;

#[test]
fn generates_candidates_for_domain_rosters() {
    let scheduler = Scheduler::new(candidate_config());
    let generated = scheduler
        .generate_candidates(&candidate_request(), &[candidate_snapshot(true)])
        .expect("candidates");
    assert_eq!(
        generated
            .candidates
            .iter()
            .map(|candidate| candidate.model_id.to_string())
            .collect::<Vec<_>>(),
        vec!["qwen9b", "granite8b", "qwen35_a3b"]
    );
    assert!(generated.rejected_options.is_empty());
}

#[test]
fn candidate_includes_residency_group() {
    let scheduler = Scheduler::new(candidate_config());
    let generated = scheduler
        .generate_candidates(&candidate_request(), &[candidate_snapshot(true)])
        .expect("candidates");
    assert_eq!(
        generated.candidates[0].group_id,
        ResidencyGroupId("small_swarm".to_string())
    );
    assert_eq!(
        generated.candidates[2].group_id,
        ResidencyGroupId("large_models".to_string())
    );
}

#[test]
fn candidate_includes_model_profile() {
    let scheduler = Scheduler::new(candidate_config());
    let generated = scheduler
        .generate_candidates(&candidate_request(), &[candidate_snapshot(true)])
        .expect("candidates");
    let qwen = generated
        .candidates
        .iter()
        .find(|candidate| candidate.model_id == ModelId("qwen9b".to_string()))
        .expect("qwen candidate");
    assert_eq!(qwen.model_profile.family, "qwen");
    assert_eq!(qwen.model_profile.parameter_class, "9b");
}

#[test]
fn candidate_includes_available_supported_runtime() {
    let scheduler = Scheduler::new(candidate_config());
    let generated = scheduler
        .generate_candidates(&candidate_request(), &[candidate_snapshot(true)])
        .expect("candidates");
    assert!(generated.candidates.iter().all(|candidate| {
        candidate.runtime_id == RuntimeId("mock".to_string())
            && matches!(
                candidate.action,
                DecisionAction::ReuseHot | DecisionAction::ColdLoad
            )
    }));
}

#[test]
fn rejects_model_without_available_runtime() {
    let scheduler = Scheduler::new(candidate_config());
    let generated = scheduler
        .generate_candidates(&candidate_request(), &[candidate_snapshot(false)])
        .expect("candidates");
    assert!(generated.candidates.is_empty());
    assert_eq!(generated.rejected_options.len(), 3);
    assert!(generated
        .rejected_options
        .iter()
        .all(|rejection| { rejection.reason == "no supported runtime is currently available" }));
}

#[test]
fn rejects_group_model_missing_profile() {
    let mut config = candidate_config();
    config.models.remove(&ModelId("granite8b".to_string()));
    let scheduler = Scheduler::new(config);
    let generated = scheduler
        .generate_candidates(&candidate_request(), &[candidate_snapshot(true)])
        .expect("candidates");
    assert_eq!(generated.candidates.len(), 2);
    assert_eq!(
        generated.rejected_options,
        vec![RejectedOption {
            model_id: Some(ModelId("granite8b".to_string())),
            runtime_id: None,
            reason: "model is referenced by a residency group but has no profile".to_string(),
        }]
    );
}

#[test]
fn candidate_order_is_deterministic() {
    let scheduler = Scheduler::new(candidate_config());
    let first = scheduler
        .generate_candidates(&candidate_request(), &[candidate_snapshot(true)])
        .expect("first");
    let second = scheduler
        .generate_candidates(&candidate_request(), &[candidate_snapshot(true)])
        .expect("second");
    assert_eq!(first, second);
    assert_eq!(
        first
            .candidates
            .iter()
            .map(|candidate| {
                (
                    candidate.group_id.to_string(),
                    candidate.model_id.to_string(),
                    candidate.runtime_id.to_string(),
                )
            })
            .collect::<Vec<_>>(),
        vec![
            (
                "small_swarm".to_string(),
                "qwen9b".to_string(),
                "mock".to_string()
            ),
            (
                "small_swarm".to_string(),
                "granite8b".to_string(),
                "mock".to_string()
            ),
            (
                "large_models".to_string(),
                "qwen35_a3b".to_string(),
                "mock".to_string()
            ),
        ]
    );
}

#[test]
fn ambiguous_runtime_state_preserves_unknown_or_cold_candidate_reason() {
    let snapshot = RuntimeSnapshot {
        runtime_id: RuntimeId("llama_swap".to_string()),
        available: true,
        residents: Vec::new(),
        configured_models: Vec::new(),
        memory: RuntimeMemorySnapshot::default(),
        active_requests: Vec::new(),
        colocation: None,
    };
    let scheduler = Scheduler::new(candidate_config());
    let generated = scheduler
        .generate_candidates(&candidate_request(), &[snapshot])
        .expect("candidates");
    for candidate in &generated.candidates {
        assert_eq!(
            candidate.residency_state,
            ResidencyState::Cold,
            "model {} must be Cold when runtime provides no resident evidence",
            candidate.model_id
        );
    }
}

#[test]
fn decision_explanation_mentions_ambiguous_residency_evidence() {
    let snapshot = RuntimeSnapshot {
        runtime_id: RuntimeId("llama_swap".to_string()),
        available: true,
        residents: Vec::new(),
        configured_models: Vec::new(),
        memory: RuntimeMemorySnapshot::default(),
        active_requests: Vec::new(),
        colocation: None,
    };
    let scheduler = Scheduler::new(candidate_config());
    let decision = scheduler
        .decide(&candidate_request(), &[snapshot])
        .expect("decision");
    let summary_lower = decision.explanation.summary.to_lowercase();
    let all_reasons = decision
        .explanation
        .reasons
        .iter()
        .map(|reason| reason.detail.to_lowercase())
        .collect::<Vec<_>>();
    let all_text = [summary_lower]
        .into_iter()
        .chain(all_reasons.iter().cloned())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        all_text.contains("cold")
            || all_text.contains("no runtime")
            || decision.action == DecisionAction::Deny,
        "decision explanation should mention cold/unknown residency evidence: {}",
        decision.explanation.summary
    );
}

#[test]
fn decide_score_tie_breaks_on_generation_order() {
    let request = candidate_request();
    let snapshots = [both_hot_snapshot()];
    let alpha_first = Scheduler::new(tie_config_ordered("[alpha, beta]"))
        .decide(&request, &snapshots)
        .expect("decision");
    let beta_first = Scheduler::new(tie_config_ordered("[beta, alpha]"))
        .decide(&request, &snapshots)
        .expect("decision");
    assert_eq!(alpha_first.score.total, beta_first.score.total);
    assert_eq!(
        alpha_first.selected_model,
        Some(ModelId("alpha".to_string()))
    );
    assert_eq!(beta_first.selected_model, Some(ModelId("beta".to_string())));
}

#[test]
fn decide_score_tie_winner_is_stable_across_invocations() {
    let request = candidate_request();
    let snapshots = [both_hot_snapshot()];
    let scheduler = Scheduler::new(tie_config_ordered("[alpha, beta]"));
    let winners: std::collections::HashSet<_> = (0..32)
        .map(|_| {
            scheduler
                .decide(&request, &snapshots)
                .expect("decision")
                .selected_model
        })
        .collect();
    assert_eq!(
        winners.len(),
        1,
        "score-tie winner must be deterministic across invocations, saw {winners:?}"
    );
    assert_eq!(
        winners.into_iter().next().unwrap(),
        Some(ModelId("alpha".to_string()))
    );
}

#[test]
fn live_roster_generates_candidates_from_configured_models() {
    let config: AnemoiConfig = serde_yaml::from_str(
        r#"
domains:
  coding:
    live_roster: llama_swap
runtimes:
  llama_swap:
    adapter: mock
continuity:
  keep_small_worker_hot: false
  background_load: false
  max_blank_wait_ms: 5000
  prefer_degraded_response_over_silence: false
"#,
    )
    .expect("config");
    let scheduler = Scheduler::new(config);
    let snapshot = RuntimeSnapshot {
        runtime_id: RuntimeId("llama_swap".to_string()),
        available: true,
        residents: Vec::new(),
        configured_models: vec![
            ModelId("qwen3.5-9b-mtp".to_string()),
            ModelId("qwen3.6-35b-a3b-mtp".to_string()),
        ],
        memory: RuntimeMemorySnapshot::default(),
        active_requests: Vec::new(),
        colocation: None,
    };
    let request = InferenceRequest {
        id: RequestId::new(),
        domain: DomainId("coding".to_string()),
        mode: ExecutionMode::Interactive,
        prompt_tokens_estimate: None,
        max_output_tokens: None,
        latency_budget_ms: None,
        quality_floor: None,
        escalation_intent: None,
    };
    let set = scheduler
        .generate_candidates(&request, &[snapshot])
        .expect("candidates");
    assert_eq!(set.candidates.len(), 2);
    assert!(set.rejected_options.is_empty());
    assert_eq!(
        set.candidates
            .iter()
            .map(|c| c.model_id.to_string())
            .collect::<Vec<_>>(),
        vec!["qwen3.5-9b-mtp", "qwen3.6-35b-a3b-mtp"]
    );
    assert!(set
        .candidates
        .iter()
        .all(|c| c.group_id == ResidencyGroupId("live".to_string())));
}

#[test]
fn live_roster_synthesises_correct_family_and_parameter_class() {
    let config: AnemoiConfig = serde_yaml::from_str(
        r#"
domains:
  coding:
    live_roster: llama_swap
runtimes:
  llama_swap:
    adapter: mock
"#,
    )
    .expect("config");
    let scheduler = Scheduler::new(config);
    let snapshot = RuntimeSnapshot {
        runtime_id: RuntimeId("llama_swap".to_string()),
        available: true,
        residents: Vec::new(),
        configured_models: vec![ModelId("qwen3.6-35b-a3b-mtp".to_string())],
        memory: RuntimeMemorySnapshot::default(),
        active_requests: Vec::new(),
        colocation: None,
    };
    let request = InferenceRequest {
        id: RequestId::new(),
        domain: DomainId("coding".to_string()),
        mode: ExecutionMode::Interactive,
        prompt_tokens_estimate: None,
        max_output_tokens: None,
        latency_budget_ms: None,
        quality_floor: None,
        escalation_intent: None,
    };
    let set = scheduler
        .generate_candidates(&request, &[snapshot])
        .expect("candidates");
    let candidate = &set.candidates[0];
    assert_eq!(candidate.model_profile.family, "qwen");
    assert_eq!(candidate.model_profile.parameter_class, "35b");
}

#[test]
fn live_roster_returns_empty_candidates_when_runtime_unavailable() {
    let config: AnemoiConfig = serde_yaml::from_str(
        r#"
domains:
  coding:
    live_roster: llama_swap
runtimes:
  llama_swap:
    adapter: mock
"#,
    )
    .expect("config");
    let scheduler = Scheduler::new(config);
    let snapshot = RuntimeSnapshot {
        runtime_id: RuntimeId("llama_swap".to_string()),
        available: false,
        residents: Vec::new(),
        configured_models: vec![ModelId("qwen3.5-9b-mtp".to_string())],
        memory: RuntimeMemorySnapshot::default(),
        active_requests: Vec::new(),
        colocation: None,
    };
    let request = InferenceRequest {
        id: RequestId::new(),
        domain: DomainId("coding".to_string()),
        mode: ExecutionMode::Interactive,
        prompt_tokens_estimate: None,
        max_output_tokens: None,
        latency_budget_ms: None,
        quality_floor: None,
        escalation_intent: None,
    };
    let set = scheduler
        .generate_candidates(&request, &[snapshot])
        .expect("candidates");
    assert!(set.candidates.is_empty());
    assert_eq!(set.rejected_options.len(), 1);
    assert!(set.rejected_options[0].reason.contains("not available"));
}

#[test]
fn live_roster_error_when_runtime_snapshot_absent() {
    let config: AnemoiConfig = serde_yaml::from_str(
        r#"
domains:
  coding:
    live_roster: llama_swap
runtimes:
  llama_swap:
    adapter: mock
"#,
    )
    .expect("config");
    let scheduler = Scheduler::new(config);
    let request = InferenceRequest {
        id: RequestId::new(),
        domain: DomainId("coding".to_string()),
        mode: ExecutionMode::Interactive,
        prompt_tokens_estimate: None,
        max_output_tokens: None,
        latency_budget_ms: None,
        quality_floor: None,
        escalation_intent: None,
    };
    let err = scheduler
        .generate_candidates(&request, &[])
        .expect_err("should error when runtime has no snapshot");
    assert!(matches!(err, PolicyError::LiveRosterRuntimeMissing(_, _)));
}
