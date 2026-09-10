use super::*;

#[test]
fn context_window_fit_rejects_candidate_too_small_for_request() {
    let scheduler = Scheduler::new(candidate_config());
    let mut request = candidate_request();
    request.prompt_tokens_estimate = Some(9000);
    request.max_output_tokens = Some(1);
    let generated = scheduler
        .generate_candidates(&request, &[candidate_snapshot(true)])
        .expect("candidates");
    assert!(
        generated
            .candidates
            .iter()
            .all(|candidate| candidate.model_id != ModelId("granite8b".to_string())),
        "granite8b must be rejected because its 8192-token context is too small"
    );
    assert!(
        generated.rejected_options.iter().any(|rejected| {
            rejected.model_id == Some(ModelId("granite8b".to_string()))
                && rejected.reason.contains("requires 9001")
                && rejected.reason.contains("8192")
        }),
        "rejected options should explain the required and available context window"
    );
}

#[test]
fn context_window_fit_allows_unknown_request_size() {
    let scheduler = Scheduler::new(candidate_config());
    let mut request = candidate_request();
    request.prompt_tokens_estimate = None;
    request.max_output_tokens = Some(500);
    let generated = scheduler
        .generate_candidates(&request, &[candidate_snapshot(true)])
        .expect("candidates");
    assert!(
        generated
            .candidates
            .iter()
            .any(|candidate| candidate.model_id == ModelId("granite8b".to_string())),
        "unknown prompt size must not create a false context-window rejection"
    );
}

#[test]
fn context_window_explanation_names_required_and_available_tokens() {
    let scheduler = Scheduler::new(candidate_config());
    let decision = scheduler
        .decide(&candidate_request(), &[candidate_snapshot(true)])
        .expect("decision");
    assert!(
        decision.explanation.reasons.iter().any(|reason| {
            reason.code == "context_window.fit"
                && reason.detail.contains("1500 token")
                && reason.detail.contains("32768 token context window")
        }),
        "selected-model explanation should name required and available context tokens"
    );
}
