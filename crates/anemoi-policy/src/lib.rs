use anemoi_core::{
    AnemoiConfig, Decision, DomainId, InferenceRequest, RejectedOption, ResidencyGroup,
    ResidencyGroupId, RuntimeId, RuntimeSnapshot,
};
use std::cmp::Reverse;
use std::collections::HashMap;

mod candidates;
mod continuity;
mod eviction;
mod floors;
mod pressure;
mod profile_utils;
mod scoring;
mod transition;

pub use candidates::{Candidate, CandidateSet};
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
            .map(|candidate| scoring::score_candidate(request, candidate, &self.config))
            .collect::<Vec<_>>();

        // Highest score wins. This is a stable sort, so candidates whose scores
        // tie keep their candidate-generation order: roster order, then the
        // group's `models` order, then `supported_runtimes` order \u2014 all driven by
        // config `Vec`s, so the tie-break is deterministic across runs. See the
        // `decide_score_tie_*` tests, which pin this contract.
        candidates.sort_by_key(|candidate| Reverse(candidate.score.total));

        let floor = floors::quality_floor_value(request);
        let eligible = candidates
            .iter()
            .filter(|candidate| floors::candidate_satisfies_floor(candidate, floor.as_ref()))
            .collect::<Vec<_>>();

        let Some(best) = eligible.first().map(|candidate| (*candidate).clone()) else {
            let rejected_options = floors::combine_rejected(
                generated.rejected_options,
                floors::quality_floor_rejections(&candidates, floor.as_ref(), None),
            );
            return Ok(candidates::deny_decision(request, rejected_options));
        };

        let selected = continuity::apply_continuity_staging(
            request,
            &candidates,
            &eligible,
            best,
            &self.config.continuity,
            floor.as_ref(),
        );

        let rejected_options = floors::combine_rejected(
            generated.rejected_options,
            floors::quality_floor_rejections(
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
                    let profile = profile_utils::synthesize_profile(model_id, live_runtime_id);
                    if floors::context_window_rejection(request, &profile).is_some() {
                        None
                    } else {
                        Some(candidates::generate_candidate(
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

                if let Some(reason) = floors::context_window_rejection(request, model) {
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
                    candidates.push(candidates::generate_candidate(
                        group, model, runtime_id, snapshot,
                    ));
                }
            }
        }

        Ok(CandidateSet {
            candidates,
            rejected_options,
        })
    }
}

#[cfg(test)]
mod tests;
