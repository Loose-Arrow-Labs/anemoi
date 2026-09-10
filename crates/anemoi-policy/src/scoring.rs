use anemoi_core::{
    AnemoiConfig, Decision, DecisionAction, DecisionReason, DecisionScore, Explanation,
    InferenceRequest, ModelId, ModelProfile, RejectedOption, ScoreContribution,
};
use chrono::Utc;
use uuid::Uuid;

use crate::floors;
use crate::pressure::{PressureInputs, PressureModel};
use crate::Candidate;

#[derive(Debug, Clone)]
pub(crate) struct ScoredCandidate {
    pub(crate) action: DecisionAction,
    pub(crate) candidate: Candidate,
    pub(crate) background_model: Option<ModelId>,
    pub(crate) score: DecisionScore,
    pub(crate) reasons: Vec<DecisionReason>,
}

impl ScoredCandidate {
    pub(crate) fn into_decision(
        self,
        request: &InferenceRequest,
        rejected_options: Vec<RejectedOption>,
    ) -> Decision {
        // A StageBackground decision must always carry the model it is staging.
        // `decide` only ever pairs the two, so the `_` arm below would otherwise
        // emit a generic "with action StageBackground" summary that silently
        // dropped the staged model if a future caller forgot to set it.
        debug_assert!(
            !matches!(self.action, DecisionAction::StageBackground)
                || self.background_model.is_some(),
            "StageBackground decision must carry a background_model"
        );
        let summary = match (&self.action, &self.background_model) {
            (DecisionAction::StageBackground, Some(background)) => format!(
                "Selected {} via {} and staged {} to avoid an interactive cold-load wait.",
                self.candidate.model_id, self.candidate.runtime_id, background
            ),
            _ => format!(
                "Selected {} via {} with action {:?}.",
                self.candidate.model_id, self.candidate.runtime_id, self.action
            ),
        };

        Decision {
            id: Uuid::new_v4(),
            request_id: request.id.clone(),
            action: self.action,
            selected_model: Some(self.candidate.model_id),
            selected_runtime: Some(self.candidate.runtime_id),
            selected_group: Some(self.candidate.group_id),
            background_model: self.background_model,
            score: self.score,
            explanation: Explanation {
                summary,
                reasons: self.reasons,
                rejected_options,
            },
            created_at: Utc::now(),
        }
    }
}

pub(crate) fn score_candidate(
    request: &InferenceRequest,
    candidate: &Candidate,
    config: &AnemoiConfig,
) -> ScoredCandidate {
    let model = &candidate.model_profile;
    let state = &candidate.residency_state;

    let mut score = DecisionScore::default();
    let mut reasons = Vec::new();

    push(
        &mut score,
        &mut reasons,
        "quality",
        quality_score(model),
        format!(
            "{} satisfies the configured roster quality target",
            model.id
        ),
    );
    push(
        &mut score,
        &mut reasons,
        "residency",
        state.reuse_bonus(),
        format!("{} is currently {:?}", model.id, state),
    );
    push(
        &mut score,
        &mut reasons,
        "load_penalty",
        -((candidate.load_estimate_ms / 1000) as i32),
        format!("estimated load cost is {}ms", candidate.load_estimate_ms),
    );

    if let Some(budget) = request.latency_budget_ms {
        let penalty = if candidate.load_estimate_ms > budget {
            -(((candidate.load_estimate_ms - budget) / 500) as i32)
        } else {
            10
        };
        push(
            &mut score,
            &mut reasons,
            "latency_budget",
            penalty,
            format!("latency budget is {}ms", budget),
        );
    }

    if let Some((required, available)) = floors::context_window_fit(request, model) {
        push(
            &mut score,
            &mut reasons,
            "context_window.fit",
            10,
            format!(
                "request requires {required} token(s) and {} provides a {available} token context window",
                model.id
            ),
        );
    }

    let pressure = PressureModel::default().assess(&PressureInputs {
        memory: &candidate.runtime_memory,
        vram_required_mb: model.vram_required_mb,
        ram_required_mb: model.ram_required_mb,
        is_cold_load: candidate.action == DecisionAction::ColdLoad,
        active_request_count: candidate.active_request_count,
    });
    for reason in pressure.reasons {
        push(
            &mut score,
            &mut reasons,
            &reason.code,
            reason.impact,
            reason.detail,
        );
    }

    if candidate.group_keep_hot || config.continuity.keep_small_worker_hot {
        push(
            &mut score,
            &mut reasons,
            "continuity",
            20,
            format!(
                "{} belongs to a continuity-friendly residency group",
                model.id
            ),
        );
    }

    if let Some(supports_streaming) = model.supports_streaming {
        let detail = if supports_streaming {
            format!("{} supports streaming responses", model.id)
        } else {
            format!("{} does not support streaming responses", model.id)
        };
        // Informational only: streaming capability is surfaced for the
        // forwarding gateway but does not influence the score.
        push(&mut score, &mut reasons, "streaming_capability", 0, detail);
    }

    ScoredCandidate {
        action: candidate.action.clone(),
        candidate: candidate.clone(),
        background_model: None,
        score,
        reasons,
    }
}

pub(crate) fn push(
    score: &mut DecisionScore,
    reasons: &mut Vec<DecisionReason>,
    label: &str,
    value: i32,
    detail: String,
) {
    score.total += value;
    score.contributions.push(ScoreContribution {
        label: label.to_string(),
        value,
    });
    reasons.push(DecisionReason {
        code: label.to_string(),
        detail,
        impact: value,
    });
}

pub(crate) fn quality_score(model: &ModelProfile) -> i32 {
    parameter_class_value(&model.parameter_class)
}

pub(crate) fn parameter_class_value(parameter_class: &str) -> i32 {
    let digits = parameter_class
        .chars()
        .filter(|ch| ch.is_ascii_digit())
        .collect::<String>()
        .parse::<i32>()
        .unwrap_or(1);
    digits.clamp(1, 100)
}
