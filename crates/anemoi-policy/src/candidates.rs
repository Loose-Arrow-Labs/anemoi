use anemoi_core::{
    ColocationConstraints, Decision, DecisionAction, DecisionReason, DecisionScore, Explanation,
    InferenceRequest, ModelId, ModelProfile, RejectedOption, ResidencyGroup, ResidencyGroupId,
    ResidencyState, RuntimeId, RuntimeMemorySnapshot, RuntimeSnapshot,
};
use chrono::Utc;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateSet {
    pub candidates: Vec<Candidate>,
    pub rejected_options: Vec<RejectedOption>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub action: DecisionAction,
    pub model_id: ModelId,
    pub runtime_id: RuntimeId,
    pub group_id: ResidencyGroupId,
    pub model_profile: ModelProfile,
    pub residency_state: ResidencyState,
    pub load_estimate_ms: u64,
    pub runtime_memory: RuntimeMemorySnapshot,
    pub active_request_count: usize,
    pub group_keep_hot: bool,
    /// Colocation feasibility of the runtime this candidate targets, copied from
    /// the observed snapshot. `None` when the runtime exposes no matrix, so the
    /// policy applies no co-residency constraint. Consulted when planning
    /// co-resident loadouts (e.g. background staging) to avoid proposing a
    /// loadout the matrix forbids.
    pub colocation: Option<ColocationConstraints>,
}

pub(crate) fn generate_candidate(
    group: &ResidencyGroup,
    model: &ModelProfile,
    runtime_id: &RuntimeId,
    snapshot: &RuntimeSnapshot,
) -> Candidate {
    let resident = snapshot
        .residents
        .iter()
        .find(|resident| resident.model_id == model.id);

    let state = resident
        .map(|resident| resident.state.clone())
        .unwrap_or(ResidencyState::Cold);

    let action = match state {
        ResidencyState::HotGpu | ResidencyState::Serving => DecisionAction::ReuseHot,
        ResidencyState::WarmCpu | ResidencyState::Partial | ResidencyState::Loading => {
            DecisionAction::PromoteWarm
        }
        ResidencyState::Cold | ResidencyState::Failed => DecisionAction::ColdLoad,
        ResidencyState::Draining | ResidencyState::Evicting => DecisionAction::Defer,
    };

    let load_estimate_ms = match action {
        DecisionAction::ColdLoad => model.cold_load_estimate_ms.unwrap_or(30_000),
        DecisionAction::PromoteWarm => model.cold_load_estimate_ms.unwrap_or(10_000) / 3,
        _ => 0,
    };

    Candidate {
        action,
        model_id: model.id.clone(),
        runtime_id: runtime_id.clone(),
        group_id: group.id.clone(),
        model_profile: model.clone(),
        residency_state: state,
        load_estimate_ms,
        runtime_memory: snapshot.memory.clone(),
        active_request_count: snapshot.active_requests.len(),
        group_keep_hot: group.keep_hot,
        colocation: snapshot.colocation.clone(),
    }
}

pub(crate) fn deny_decision(
    request: &InferenceRequest,
    rejected_options: Vec<RejectedOption>,
) -> Decision {
    Decision {
        id: Uuid::new_v4(),
        request_id: request.id.clone(),
        action: DecisionAction::Deny,
        selected_model: None,
        selected_runtime: None,
        selected_group: None,
        background_model: None,
        score: DecisionScore::default(),
        explanation: Explanation {
            summary: "No runnable model candidate was available.".to_string(),
            reasons: vec![DecisionReason {
                code: "no_candidate".to_string(),
                detail: "all configured model/runtime options were rejected".to_string(),
                impact: -100,
            }],
            rejected_options,
        },
        created_at: Utc::now(),
    }
}
