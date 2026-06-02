//! Eviction and pinning policy.
//!
//! Decides which residents may be evicted to free capacity. Eviction is
//! explainable and conservative by default: keep-hot continuity workers and
//! pinned models are protected, and actively serving models are blocked unless
//! a force policy explicitly overrides protection. The planner never mutates a
//! runtime — it only produces candidates, protections, and blocks for the
//! controlled execution path to act on.

use anemoi_core::{DecisionReason, ModelId, ResidencyState, RuntimeId};
use std::cmp::Ordering;

/// A resident under consideration for eviction, with the policy-relevant flags
/// already resolved from config and runtime state.
#[derive(Debug, Clone)]
pub struct EvictionCandidateResident {
    pub model_id: ModelId,
    pub runtime_id: RuntimeId,
    pub state: ResidencyState,
    /// Member of a keep-hot continuity group.
    pub keep_hot: bool,
    /// Member of a pinned residency group.
    pub pinned: bool,
    /// Idle time in seconds; higher means a better eviction candidate.
    /// `None` means idle time is unknown.
    pub idle_secs: Option<u64>,
}

impl EvictionCandidateResident {
    fn is_serving(&self) -> bool {
        matches!(self.state, ResidencyState::Serving)
    }
}

/// Inputs to the eviction planner.
#[derive(Debug, Clone)]
pub struct EvictionRequest<'a> {
    pub residents: &'a [EvictionCandidateResident],
    /// When set, the force policy overrides keep-hot, pinning, and serving
    /// protections. Used only for explicit operator-driven eviction.
    pub force: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvictionCandidate {
    pub model_id: ModelId,
    pub runtime_id: RuntimeId,
    pub idle_secs: Option<u64>,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtectedResident {
    pub model_id: ModelId,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockedEviction {
    pub model_id: ModelId,
    pub reason: String,
}

/// The outcome of eviction planning: ranked candidates, protected residents,
/// blocked residents, and the structured reasons that explain each decision.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EvictionPlan {
    pub candidates: Vec<EvictionCandidate>,
    pub protected: Vec<ProtectedResident>,
    pub blocked: Vec<BlockedEviction>,
    pub reasons: Vec<DecisionReason>,
}

impl EvictionPlan {
    pub fn is_empty(&self) -> bool {
        self.candidates.is_empty()
    }
}

/// Plan evictions over the supplied residents. Pure and side-effect free.
pub fn plan_evictions(request: &EvictionRequest<'_>) -> EvictionPlan {
    let mut plan = EvictionPlan::default();

    for resident in request.residents {
        let model = &resident.model_id;

        if resident.keep_hot && !request.force {
            plan.protected.push(ProtectedResident {
                model_id: model.clone(),
                reason: "keep-hot continuity worker is protected from eviction".to_string(),
            });
            plan.reasons.push(DecisionReason {
                code: "eviction.protected.keep_hot".to_string(),
                detail: format!("{model} is protected as a keep-hot continuity worker"),
                impact: 0,
            });
            continue;
        }

        if resident.pinned && !request.force {
            plan.protected.push(ProtectedResident {
                model_id: model.clone(),
                reason: "pinned model is protected from eviction".to_string(),
            });
            plan.reasons.push(DecisionReason {
                code: "eviction.protected.pinned".to_string(),
                detail: format!("{model} is protected because its residency group is pinned"),
                impact: 0,
            });
            continue;
        }

        if resident.is_serving() && !request.force {
            plan.blocked.push(BlockedEviction {
                model_id: model.clone(),
                reason: "model is actively serving; eviction requires a force policy".to_string(),
            });
            plan.reasons.push(DecisionReason {
                code: "eviction.blocked.serving".to_string(),
                detail: format!("{model} is actively serving and cannot be evicted without force"),
                impact: 0,
            });
            continue;
        }

        plan.candidates.push(EvictionCandidate {
            model_id: model.clone(),
            runtime_id: resident.runtime_id.clone(),
            idle_secs: resident.idle_secs,
            reason: "idle, unpinned resident eligible for eviction".to_string(),
        });
        plan.reasons.push(DecisionReason {
            code: "eviction.candidate".to_string(),
            detail: format!("{model} is an eligible eviction candidate"),
            impact: 0,
        });
    }

    // Prefer the most idle candidate first; unknown idle ranks last. Ties break
    // on model id for determinism.
    //
    // Known idle always ranks ahead of unknown idle, and among known residents a
    // higher `idle_secs` (a better eviction candidate) comes first. A previous
    // `unwrap_or(0)` key tied `None` with `Some(0)`, which let the alphabetical
    // model-id tiebreak surface an unknown-idle resident ahead of a known
    // zero-idle one — contradicting "unknown idle ranks last".
    plan.candidates.sort_by(|a, b| {
        match (a.idle_secs, b.idle_secs) {
            (Some(x), Some(y)) => y.cmp(&x),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        }
        .then_with(|| a.model_id.to_string().cmp(&b.model_id.to_string()))
    });

    plan
}
