use anemoi_core::{
    AnemoiConfig, ColocationConstraints, Decision, DecisionAction, DecisionReason, DecisionScore,
    DomainId, Explanation, InferenceRequest, ModelId, ModelProfile, RejectedOption, ResidencyGroup,
    ResidencyGroupId, ResidencyState, RuntimeId, RuntimeMemorySnapshot, RuntimeSnapshot,
    ScoreContribution,
};
use chrono::Utc;
use std::cmp::Reverse;
use std::collections::HashMap;
use uuid::Uuid;

mod eviction;
mod pressure;
mod transition;
pub use eviction::{
    plan_evictions, BlockedEviction, EvictionCandidate, EvictionCandidateResident, EvictionPlan,
    EvictionRequest, ProtectedResident,
};
pub use pressure::{Pressure, PressureAssessment, PressureInputs, PressureModel, PressureReason};
pub use transition::{
    ActiveTransition, TransitionCoordinator, TransitionDecision, TransitionPath, TransitionRequest,
};

#[derive(Debug, thiserror::Error)]
pub enum PolicyError {
    #[error("unknown domain {0}")]
    UnknownDomain(DomainId),
    #[error("domain {0} has no configured roster")]
    EmptyRoster(DomainId),
    #[error("domain {0} live_roster references unknown runtime {1}")]
    LiveRosterRuntimeMissing(DomainId, RuntimeId),
}

#[derive(Debug, Clone)]
pub struct Scheduler {
    config: AnemoiConfig,
}

impl Scheduler {
    pub fn new(config: AnemoiConfig) -> Self {
        Self { config }
    }

    pub fn decide(
        &self,
        request: &InferenceRequest,
        snapshots: &[RuntimeSnapshot],
    ) -> Result<Decision, PolicyError> {
        let generated = self.generate_candidates(request, snapshots)?;
        let mut candidates = generated
            .candidates
            .iter()
            .map(|candidate| score_candidate(request, candidate, &self.config))
            .collect::<Vec<_>>();

        // Highest score wins. This is a stable sort, so candidates whose scores
        // tie keep their candidate-generation order: roster order, then the
        // group's `models` order, then `supported_runtimes` order — all driven by
        // config `Vec`s, so the tie-break is deterministic across runs. See the
        // `decide_score_tie_*` tests, which pin this contract.
        candidates.sort_by_key(|candidate| Reverse(candidate.score.total));

        let floor = quality_floor_value(request);
        let eligible = candidates
            .iter()
            .filter(|candidate| candidate_satisfies_floor(candidate, floor.as_ref()))
            .collect::<Vec<_>>();

        let Some(mut best) = eligible.first().map(|candidate| (*candidate).clone()) else {
            let rejected_options = combine_rejected(
                generated.rejected_options,
                quality_floor_rejections(&candidates, floor.as_ref(), None),
            );
            return Ok(deny_decision(request, rejected_options));
        };

        let continuity = &self.config.continuity;
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

        let selected = if let (Some(cold), Some(fallback)) = (cold_large, hot_fallback) {
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
                    Some(constraints) => constraints
                        .can_colocate(&fallback.candidate.model_id, &cold.candidate.model_id),
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
                if let Some((floor_label, floor_value)) = floor.as_ref() {
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
        };

        let rejected_options = combine_rejected(
            generated.rejected_options,
            quality_floor_rejections(
                &candidates,
                floor.as_ref(),
                Some(&selected.candidate.model_id),
            ),
        );

        Ok(selected.into_decision(request, rejected_options))
    }

    pub fn generate_candidates(
        &self,
        request: &InferenceRequest,
        snapshots: &[RuntimeSnapshot],
    ) -> Result<CandidateSet, PolicyError> {
        let domain = self
            .config
            .domains
            .get(&request.domain)
            .ok_or_else(|| PolicyError::UnknownDomain(request.domain.clone()))?;

        // Live roster: use the runtime's configured_models snapshot directly.
        // Model profiles are synthesised from model IDs — no static config needed.
        if let Some(live_runtime_id) = &domain.live_roster {
            let Some(snapshot) = snapshots.iter().find(|s| &s.runtime_id == live_runtime_id) else {
                return Err(PolicyError::LiveRosterRuntimeMissing(
                    request.domain.clone(),
                    live_runtime_id.clone(),
                ));
            };

            if !snapshot.available {
                return Ok(CandidateSet {
                    candidates: Vec::new(),
                    rejected_options: vec![RejectedOption {
                        model_id: None,
                        runtime_id: Some(live_runtime_id.clone()),
                        reason: format!("live_roster runtime {} is not available", live_runtime_id),
                    }],
                });
            }

            let live_group = ResidencyGroup {
                id: ResidencyGroupId("live".to_string()),
                purpose: Vec::new(),
                models: snapshot.configured_models.clone(),
                keep_hot: false,
                allow_background_load: true,
                pinned: false,
            };

            let candidates = snapshot
                .configured_models
                .iter()
                .filter_map(|model_id| {
                    let profile = synthesize_profile(model_id, live_runtime_id);
                    if context_window_rejection(request, &profile).is_some() {
                        None
                    } else {
                        Some(generate_candidate(
                            &live_group,
                            &profile,
                            live_runtime_id,
                            snapshot,
                        ))
                    }
                })
                .collect();

            return Ok(CandidateSet {
                candidates,
                rejected_options: Vec::new(),
            });
        }

        // Static roster path.
        if domain.rosters.is_empty() {
            return Err(PolicyError::EmptyRoster(request.domain.clone()));
        }

        let groups = domain
            .rosters
            .iter()
            .filter_map(|id| {
                self.config
                    .residency_groups
                    .get(id)
                    .cloned()
                    .map(|group| group.into_group(id.clone()))
            })
            .collect::<Vec<_>>();

        let models = self
            .config
            .models
            .iter()
            .map(|(id, model)| (id.clone(), model.clone().into_profile(id.clone())))
            .collect::<HashMap<_, _>>();

        let mut candidates = Vec::new();
        let mut rejected_options = Vec::new();

        for group in &groups {
            for model_id in &group.models {
                let Some(model) = models.get(model_id) else {
                    rejected_options.push(RejectedOption {
                        model_id: Some(model_id.clone()),
                        runtime_id: None,
                        reason: "model is referenced by a residency group but has no profile"
                            .to_string(),
                    });
                    continue;
                };

                if let Some(reason) = context_window_rejection(request, model) {
                    rejected_options.push(RejectedOption {
                        model_id: Some(model_id.clone()),
                        runtime_id: None,
                        reason,
                    });
                    continue;
                }

                let runtime_candidates = model
                    .supported_runtimes
                    .iter()
                    .filter_map(|runtime_id| {
                        snapshots
                            .iter()
                            .find(|snapshot| {
                                snapshot.runtime_id == *runtime_id && snapshot.available
                            })
                            .map(|snapshot| (runtime_id, snapshot))
                    })
                    .collect::<Vec<_>>();

                if runtime_candidates.is_empty() {
                    rejected_options.push(RejectedOption {
                        model_id: Some(model_id.clone()),
                        runtime_id: None,
                        reason: "no supported runtime is currently available".to_string(),
                    });
                    continue;
                }

                for (runtime_id, snapshot) in runtime_candidates {
                    candidates.push(generate_candidate(group, model, runtime_id, snapshot));
                }
            }
        }

        Ok(CandidateSet {
            candidates,
            rejected_options,
        })
    }
}

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

#[derive(Debug, Clone)]
struct ScoredCandidate {
    action: DecisionAction,
    candidate: Candidate,
    background_model: Option<ModelId>,
    score: DecisionScore,
    reasons: Vec<DecisionReason>,
}

impl ScoredCandidate {
    fn into_decision(
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

/// Build a synthetic `ModelProfile` from a model ID reported by a live runtime.
///
/// Family is the leading alphabetic prefix of the ID (e.g. `qwen`, `gemma`,
/// `minimax`).  Parameter class is extracted from the first `NNb` token found
/// in the ID (e.g. `9b`, `35b`, `122b`); models whose IDs carry no such token
/// (e.g. `minimax-256k`, `nemotron-udiq4-256k`) get `"unknown"` and will score
/// low on quality but are still selectable when hot.
fn synthesize_profile(model_id: &ModelId, runtime_id: &RuntimeId) -> ModelProfile {
    let family: String = model_id
        .0
        .chars()
        .take_while(|c| c.is_alphabetic())
        .collect();

    let parameter_class = extract_parameter_class(&model_id.0);

    ModelProfile {
        id: model_id.clone(),
        family: if family.is_empty() {
            "unknown".to_string()
        } else {
            family
        },
        parameter_class,
        context_window: None,
        vram_required_mb: None,
        ram_required_mb: None,
        cold_load_estimate_ms: None,
        supports_streaming: Some(true),
        supported_runtimes: vec![runtime_id.clone()],
    }
}

/// Scan `id` for the first `NNb` token (one or more ASCII digits immediately
/// followed by the letter `b`) and return it, e.g. `"35b"`.  Returns
/// `"unknown"` when no such token is found.
fn extract_parameter_class(id: &str) -> String {
    let bytes = id.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            if i < bytes.len() && (bytes[i] == b'b' || bytes[i] == b'B') {
                return format!("{}b", &id[start..i]);
            }
        } else {
            i += 1;
        }
    }
    "unknown".to_string()
}

fn generate_candidate(
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

fn score_candidate(
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

    if let Some((required, available)) = context_window_fit(request, model) {
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

fn push(
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

fn quality_score(model: &ModelProfile) -> i32 {
    parameter_class_value(&model.parameter_class)
}

fn parameter_class_value(parameter_class: &str) -> i32 {
    let digits = parameter_class
        .chars()
        .filter(|ch| ch.is_ascii_digit())
        .collect::<String>()
        .parse::<i32>()
        .unwrap_or(1);
    digits.clamp(1, 100)
}

fn quality_floor_value(request: &InferenceRequest) -> Option<(String, i32)> {
    request
        .quality_floor
        .as_ref()?
        .minimum_parameter_class
        .as_ref()
        .map(|floor| (floor.clone(), parameter_class_value(floor)))
}

fn candidate_satisfies_floor(candidate: &ScoredCandidate, floor: Option<&(String, i32)>) -> bool {
    floor.is_none_or(|(_, minimum)| quality_score(&candidate.candidate.model_profile) >= *minimum)
}

fn quality_floor_rejections(
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

fn combine_rejected(
    mut first: Vec<RejectedOption>,
    mut second: Vec<RejectedOption>,
) -> Vec<RejectedOption> {
    first.append(&mut second);
    first
}

fn request_required_tokens(request: &InferenceRequest) -> Option<u32> {
    request
        .prompt_tokens_estimate
        .map(|prompt| prompt.saturating_add(request.max_output_tokens.unwrap_or(0)))
}

fn context_window_fit(request: &InferenceRequest, model: &ModelProfile) -> Option<(u32, u32)> {
    let required = request_required_tokens(request)?;
    let available = model.context_window?;
    (required <= available).then_some((required, available))
}

fn context_window_rejection(request: &InferenceRequest, model: &ModelProfile) -> Option<String> {
    let required = request_required_tokens(request)?;
    let available = model.context_window?;
    (required > available).then(|| {
        format!(
            "request requires {required} token(s) but {} context window is {available} token(s)",
            model.id
        )
    })
}

fn deny_decision(request: &InferenceRequest, rejected_options: Vec<RejectedOption>) -> Decision {
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

#[cfg(test)]
mod tests {
    use super::*;
    use anemoi_core::{
        ExecutionMode, ModelResident, RequestId, RuntimeMemorySnapshot, RuntimeSnapshot,
    };

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
        assert!(generated.rejected_options.iter().all(|rejection| {
            rejection.reason == "no supported runtime is currently available"
        }));
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
                    "mock".to_string(),
                ),
                (
                    "small_swarm".to_string(),
                    "granite8b".to_string(),
                    "mock".to_string(),
                ),
                (
                    "large_models".to_string(),
                    "qwen35_a3b".to_string(),
                    "mock".to_string(),
                ),
            ]
        );
    }

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
        // Same keep-small-hot / cold-big setup as
        // `avoids_cold_large_model_when_small_worker_is_hot`, but the runtime
        // snapshot now carries a colocation matrix. Background staging keeps
        // qwen9b hot while loading qwen35_a3b — a co-resident loadout — so it is
        // only safe when the matrix admits the pair. The decision must change
        // purely on what the matrix allows.
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

        // Matrix admits {qwen9b, qwen35_a3b}: stage the big model in the
        // background while serving the hot small worker — the continuity move.
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

        // Matrix forbids the pair (each model colocates only with itself):
        // loading the big model would evict the hot worker, so the decision
        // changes — serve the hot worker alone, stage nothing, and explain why.
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
    fn ambiguous_runtime_state_preserves_unknown_or_cold_candidate_reason() {
        // Runtime snapshot has no residents (ambiguous/unknown state).
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

        // All candidates should have Cold state (not hot) when runtime
        // provides no resident evidence.
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
        // Runtime snapshot has no residents (ambiguous state).
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

        // With no hot residents, the decision should either ColdLoad or Deny.
        // Either way, the explanation should mention the lack of residency.
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

    // debug_assert is compiled out under --release, so this invariant test only
    // runs when debug assertions are active (the default for `cargo test`).
    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "StageBackground decision must carry a background_model")]
    fn into_decision_panics_on_stage_background_without_model() {
        // (StageBackground, None) is unreachable from decide(), which always
        // pairs a staging action with a background model. The debug_assert pins
        // that invariant so the generic `_` summary arm can't silently swallow a
        // staging decision that forgot to record its staged model.
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
        let scored = ScoredCandidate {
            action: DecisionAction::StageBackground,
            candidate,
            background_model: None,
            score: DecisionScore::default(),
            reasons: Vec::new(),
        };

        let _ = scored.into_decision(&candidate_request(), Vec::new());
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

    fn candidate_request() -> InferenceRequest {
        InferenceRequest {
            id: RequestId::new(),
            domain: DomainId("coding".to_string()),
            mode: ExecutionMode::Interactive,
            prompt_tokens_estimate: Some(1000),
            max_output_tokens: Some(500),
            latency_budget_ms: Some(1500),
            quality_floor: None,
            escalation_intent: None,
        }
    }

    fn candidate_snapshot(available: bool) -> RuntimeSnapshot {
        candidate_snapshot_with_residents(
            available,
            vec![("qwen9b", ResidencyState::HotGpu, Some(9000))],
        )
    }

    fn candidate_snapshot_with_residents(
        available: bool,
        residents: Vec<(&str, ResidencyState, Option<u64>)>,
    ) -> RuntimeSnapshot {
        RuntimeSnapshot {
            runtime_id: RuntimeId("mock".to_string()),
            available,
            residents: residents
                .into_iter()
                .map(|(model_id, state, vram_mb)| ModelResident {
                    model_id: ModelId(model_id.to_string()),
                    state,
                    vram_mb,
                    ram_mb: None,
                    kv_cache_mb: None,
                    loaded_since: None,
                })
                .collect(),
            configured_models: Vec::new(),
            memory: RuntimeMemorySnapshot::default(),
            active_requests: Vec::new(),
            colocation: None,
        }
    }

    fn candidate_config() -> AnemoiConfig {
        serde_yaml::from_str(
            r#"
domains:
  coding:
    rosters: [small_swarm, large_models]
residency_groups:
  small_swarm:
    keep_hot: true
    allow_background_load: true
    models: [qwen9b, granite8b]
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
    supported_runtimes: [mock]
  granite8b:
    family: granite
    parameter_class: 8b
    context_window: 8192
    vram_required_mb: 8000
    ram_required_mb: 10000
    cold_load_estimate_ms: 15000
    supported_runtimes: [mock]
  qwen35_a3b:
    family: qwen
    parameter_class: 35b
    context_window: 32768
    vram_required_mb: 30000
    ram_required_mb: 45000
    cold_load_estimate_ms: 45000
    supported_runtimes: [mock]
runtimes:
  mock:
    adapter: mock
"#,
        )
        .expect("candidate config")
    }

    fn fast_only_config() -> AnemoiConfig {
        serde_yaml::from_str(
            r#"
domains:
  coding:
    rosters: [small_swarm]
residency_groups:
  small_swarm:
    keep_hot: true
    allow_background_load: true
    models: [qwen9b]
models:
  qwen9b:
    family: qwen
    parameter_class: 9b
    context_window: 32768
    vram_required_mb: 9000
    ram_required_mb: 12000
    cold_load_estimate_ms: 18000
    supported_runtimes: [mock]
runtimes:
  mock:
    adapter: mock
"#,
        )
        .expect("fast-only config")
    }

    fn request_with_quality_floor(floor: &str) -> InferenceRequest {
        let mut request = candidate_request();
        request.quality_floor = Some(anemoi_core::QualityFloor {
            minimum_parameter_class: Some(floor.to_string()),
        });
        request
    }

    // Two models with identical profiles in one keep-hot group; `model_order`
    // controls the order they are listed (and therefore generated). When both
    // are hot-resident they score identically, exercising the score-tie path.
    fn tie_config_ordered(model_order: &str) -> AnemoiConfig {
        let profile = "{ family: qwen, parameter_class: 9b, context_window: 32768, \
             vram_required_mb: 9000, ram_required_mb: 12000, cold_load_estimate_ms: 18000, \
             supported_runtimes: [mock] }";
        let yaml = format!(
            "domains:\n  coding:\n    rosters: [swarm]\n\
             residency_groups:\n  swarm:\n    keep_hot: true\n    allow_background_load: true\n    models: {model_order}\n\
             models:\n  alpha: {profile}\n  beta: {profile}\n\
             runtimes:\n  mock:\n    adapter: mock\n"
        );
        serde_yaml::from_str(&yaml).expect("tie config")
    }

    fn both_hot_snapshot() -> RuntimeSnapshot {
        let hot = |id: &str| ModelResident {
            model_id: ModelId(id.to_string()),
            state: ResidencyState::HotGpu,
            vram_mb: Some(9000),
            ram_mb: None,
            kv_cache_mb: None,
            loaded_since: None,
        };
        RuntimeSnapshot {
            runtime_id: RuntimeId("mock".to_string()),
            available: true,
            residents: vec![hot("alpha"), hot("beta")],
            configured_models: Vec::new(),
            memory: RuntimeMemorySnapshot::default(),
            active_requests: Vec::new(),
            colocation: None,
        }
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

        // Identical profiles => identical scores, so the winner's score is the
        // same regardless of order. This proves we are genuinely on the score-tie
        // path rather than just picking the higher-scoring model.
        assert_eq!(alpha_first.score.total, beta_first.score.total);
        // The model listed first in the group wins the tie.
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

    // ── live roster tests ──────────────────────────────────────────────────

    #[test]
    fn extract_parameter_class_parses_common_model_ids() {
        let cases = [
            ("qwen3.5-9b-mtp", "9b"),
            ("qwen3.6-35b-a3b-mtp", "35b"),
            ("qwen3.5-122b-a10b-mtp", "122b"),
            ("qwen3.5-2b-mtp", "2b"),
            ("qwen3.5-4b-mtp", "4b"),
            ("qwen3.6-27b-mtp", "27b"),
            ("gemma-4-26b-a4b-it-mtp", "26b"),
            ("gemma-4-31b-it", "31b"),
            ("gemma-4-e2b-it", "2b"),
            ("gemma-4-e4b-it", "4b"),
            ("granite-4.1-8b-gpu", "8b"),
        ];
        for (id, expected) in cases {
            assert_eq!(extract_parameter_class(id), expected, "failed for {}", id);
        }
    }

    #[test]
    fn extract_parameter_class_returns_unknown_for_non_parametric_ids() {
        let cases = ["minimax-256k", "nemotron-udiq4-256k", "minimax-256k-iq3s"];
        for id in cases {
            assert_eq!(
                extract_parameter_class(id),
                "unknown",
                "expected unknown for {}",
                id
            );
        }
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
        // group id is "live" for synthesised candidates
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
}
