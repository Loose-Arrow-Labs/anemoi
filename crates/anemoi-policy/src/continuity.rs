use anemoi_core::ContinuityConfig;
use anemoi_core::DecisionAction;
use anemoi_core::DecisionReason;
use anemoi_core::InferenceRequest;
use anemoi_core::ScoreContribution;

use crate::scoring::quality_score;
use crate::scoring::ScoredCandidate;

/// Apply continuity staging logic to select between a cold large model and a
/// hot fallback. Returns the (possibly modified) best candidate with staging
/// applied or a colocation-blocked explanation recorded.
pub(crate) fn apply_continuity_staging(
    request: &InferenceRequest,
    candidates: &[ScoredCandidate],
    eligible: &[&ScoredCandidate],
    mut best: ScoredCandidate,
    continuity: &ContinuityConfig,
    floor: Option<&(String, i32)>,
) -> ScoredCandidate {
    let cold_large = eligible
        .iter()
        .filter(|candidate| {
            candidate.candidate.action == DecisionAction::ColdLoad
                && candidate.candidate.load_estimate_ms > continuity.max_blank_wait_ms
        })
        .max_by_key(|candidate| {
            (
                quality_score(&candidate.candidate.model_profile),
                candidate.candidate.model_id.to_string(),
            )
        });
    let hot_fallback = candidates.iter().find(|candidate| {
        matches!(
            candidate.candidate.action,
            DecisionAction::ReuseHot | DecisionAction::PromoteWarm
        )
    });

    if let (Some(cold), Some(fallback)) = (cold_large, hot_fallback) {
        let wants_stage = continuity.background_load
            && continuity.prefer_degraded_response_over_silence
            && request.latency_budget_ms.unwrap_or(u64::MAX) < cold.candidate.load_estimate_ms;

        // Background staging keeps `fallback` hot while loading `cold` — a
        // co-resident loadout. It is only safe when the target runtime's
        // colocation matrix admits the pair: loading `cold` would otherwise
        // evict `fallback`, defeating the continuity it is meant to preserve.
        // A matrix-less runtime (`None`) leaves colocation unknown and keeps
        // the legacy staging behavior; a `cold` model on a different runtime
        // shares no GPU with `fallback`, so there is no colocation conflict.
        let colocation_admits = cold.candidate.runtime_id != fallback.candidate.runtime_id
            || match &fallback.candidate.colocation {
                None => true,
                Some(constraints) => {
                    constraints.can_colocate(&fallback.candidate.model_id, &cold.candidate.model_id)
                }
            };

        if wants_stage && colocation_admits {
            let mut staged = fallback.clone();
            staged.action = DecisionAction::StageBackground;
            staged.background_model = Some(cold.candidate.model_id.clone());
            staged.reasons.push(DecisionReason {
                code: "continuity.stage_background".to_string(),
                detail: format!(
                    "selected hot {} now and staged {} because cold load estimate {}ms exceeded latency budget {}ms and continuity policy prefers degraded response over silence",
                    fallback.candidate.model_id,
                    cold.candidate.model_id,
                    cold.candidate.load_estimate_ms,
                    request.latency_budget_ms.unwrap_or(u64::MAX)
                ),
                impact: 50,
            });
            staged.score.contributions.push(ScoreContribution {
                label: "continuity background staging".to_string(),
                value: 50,
            });
            staged.score.total += 50;
            if let Some((floor_label, floor_value)) = floor {
                let fallback_class = quality_score(&staged.candidate.model_profile);
                if fallback_class < *floor_value {
                    staged.reasons.push(DecisionReason {
                        code: "quality_floor.degraded_fallback".to_string(),
                        detail: format!(
                            "request required at least {floor_label}; selected hot {} ({}) only as an immediate fallback while staging qualifying {} ({})",
                            staged.candidate.model_id,
                            staged.candidate.model_profile.parameter_class,
                            cold.candidate.model_id,
                            cold.candidate.model_profile.parameter_class,
                        ),
                        impact: -25,
                    });
                    staged.score.contributions.push(ScoreContribution {
                        label: "quality floor degraded fallback".to_string(),
                        value: -25,
                    });
                    staged.score.total -= 25;
                }
            }
            staged
        } else {
            // When staging was warranted but the colocation matrix forbids
            // the co-resident pair, record why we held back so the decision
            // stays explainable rather than silently serving `best`.
            if wants_stage {
                let detail = format!(
                    "did not stage {} alongside {} for background load because the {} colocation matrix does not admit them as co-resident; serving {} alone preserves the hot worker",
                    cold.candidate.model_id,
                    fallback.candidate.model_id,
                    fallback.candidate.runtime_id,
                    best.candidate.model_id
                );
                best.reasons.push(DecisionReason {
                    code: "continuity.stage_blocked_colocation".to_string(),
                    detail,
                    impact: 0,
                });
            }
            best
        }
    } else {
        best
    }
}
