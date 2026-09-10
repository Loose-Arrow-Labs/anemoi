use anemoi_core::InferenceRequest;
use anemoi_core::ModelId;
use anemoi_core::ModelProfile;
use anemoi_core::RejectedOption;

use crate::scoring::parameter_class_value;
use crate::scoring::quality_score;
use crate::scoring::ScoredCandidate;

pub(crate) fn quality_floor_value(request: &InferenceRequest) -> Option<(String, i32)> {
    request
        .quality_floor
        .as_ref()?
        .minimum_parameter_class
        .as_ref()
        .map(|floor| (floor.clone(), parameter_class_value(floor)))
}

pub(crate) fn candidate_satisfies_floor(
    candidate: &ScoredCandidate,
    floor: Option<&(String, i32)>,
) -> bool {
    floor.is_none_or(|(_, minimum)| quality_score(&candidate.candidate.model_profile) >= *minimum)
}

pub(crate) fn quality_floor_rejections(
    candidates: &[ScoredCandidate],
    floor: Option<&(String, i32)>,
    selected_model: Option<&ModelId>,
) -> Vec<RejectedOption> {
    let Some((floor_label, minimum)) = floor else {
        return Vec::new();
    };

    candidates
        .iter()
        .filter(|candidate| {
            selected_model != Some(&candidate.candidate.model_id)
                && quality_score(&candidate.candidate.model_profile) < *minimum
        })
        .map(|candidate| RejectedOption {
            model_id: Some(candidate.candidate.model_id.clone()),
            runtime_id: Some(candidate.candidate.runtime_id.clone()),
            reason: format!(
                "{} parameter class {} is below requested quality floor {floor_label}",
                candidate.candidate.model_id, candidate.candidate.model_profile.parameter_class
            ),
        })
        .collect()
}

pub(crate) fn combine_rejected(
    mut first: Vec<RejectedOption>,
    mut second: Vec<RejectedOption>,
) -> Vec<RejectedOption> {
    first.append(&mut second);
    first
}

pub(crate) fn request_required_tokens(request: &InferenceRequest) -> Option<u32> {
    request
        .prompt_tokens_estimate
        .map(|prompt| prompt.saturating_add(request.max_output_tokens.unwrap_or(0)))
}

pub(crate) fn context_window_fit(
    request: &InferenceRequest,
    model: &ModelProfile,
) -> Option<(u32, u32)> {
    let required = request_required_tokens(request)?;
    let available = model.context_window?;
    (required <= available).then_some((required, available))
}

pub(crate) fn context_window_rejection(
    request: &InferenceRequest,
    model: &ModelProfile,
) -> Option<String> {
    let required = request_required_tokens(request)?;
    let available = model.context_window?;
    (required > available).then(|| {
        format!(
            "request requires {required} token(s) but {} context window is {available} token(s)",
            model.id
        )
    })
}
