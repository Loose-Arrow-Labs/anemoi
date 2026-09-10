use super::*;

fn eviction_resident(
    id: &str,
    state: ResidencyState,
    keep_hot: bool,
    pinned: bool,
    idle_secs: Option<u64>,
) -> EvictionCandidateResident {
    EvictionCandidateResident {
        model_id: ModelId(id.to_string()),
        runtime_id: RuntimeId("mock".to_string()),
        state,
        keep_hot,
        pinned,
        idle_secs,
    }
}

#[test]
fn keep_hot_group_members_are_not_evicted_for_background_stage() {
    let residents = vec![
        eviction_resident(
            "small_worker",
            ResidencyState::HotGpu,
            true,
            false,
            Some(10),
        ),
        eviction_resident("big_idle", ResidencyState::HotGpu, false, false, Some(600)),
    ];
    let plan = plan_evictions(&EvictionRequest {
        residents: &residents,
        force: false,
    });
    assert!(
        plan.protected
            .iter()
            .any(|protected| protected.model_id == ModelId("small_worker".to_string())),
        "keep-hot worker must be protected"
    );
    assert!(
        !plan
            .candidates
            .iter()
            .any(|candidate| candidate.model_id == ModelId("small_worker".to_string())),
        "keep-hot worker must not be an eviction candidate"
    );
}

#[test]
fn eviction_plan_prefers_unpinned_idle_resident() {
    let residents = vec![
        eviction_resident(
            "pinned_worker",
            ResidencyState::HotGpu,
            false,
            true,
            Some(9_999),
        ),
        eviction_resident(
            "recent_resident",
            ResidencyState::HotGpu,
            false,
            false,
            Some(5),
        ),
        eviction_resident(
            "idle_resident",
            ResidencyState::HotGpu,
            false,
            false,
            Some(900),
        ),
    ];
    let plan = plan_evictions(&EvictionRequest {
        residents: &residents,
        force: false,
    });
    assert!(
        plan.protected
            .iter()
            .any(|protected| protected.model_id == ModelId("pinned_worker".to_string())),
        "pinned worker must be protected, not a candidate"
    );
    assert_eq!(
        plan.candidates.first().map(|candidate| &candidate.model_id),
        Some(&ModelId("idle_resident".to_string())),
        "the most-idle unpinned resident should rank first"
    );
}

#[test]
fn eviction_plan_rejects_serving_model_without_force_policy() {
    let residents = vec![eviction_resident(
        "serving_model",
        ResidencyState::Serving,
        false,
        false,
        Some(0),
    )];
    let plan = plan_evictions(&EvictionRequest {
        residents: &residents,
        force: false,
    });
    assert!(
        plan.blocked
            .iter()
            .any(|blocked| blocked.model_id == ModelId("serving_model".to_string())),
        "a serving model must be blocked without force"
    );
    assert!(
        plan.candidates.is_empty(),
        "a serving model must not be an eviction candidate without force"
    );
    let forced = plan_evictions(&EvictionRequest {
        residents: &residents,
        force: true,
    });
    assert!(
        forced
            .candidates
            .iter()
            .any(|candidate| candidate.model_id == ModelId("serving_model".to_string())),
        "force policy must allow evicting a serving model"
    );
}

#[test]
fn pinning_policy_explanation_names_protected_model() {
    let residents = vec![eviction_resident(
        "pinned_model",
        ResidencyState::HotGpu,
        false,
        true,
        Some(120),
    )];
    let plan = plan_evictions(&EvictionRequest {
        residents: &residents,
        force: false,
    });
    assert!(
        plan.reasons.iter().any(|reason| {
            reason.code.contains("pinned") && reason.detail.contains("pinned_model")
        }),
        "explanation must name the protected pinned model"
    );
}
