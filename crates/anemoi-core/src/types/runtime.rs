use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::ids::{ModelId, RequestId, ResidencyGroupId, RuntimeId};
use super::states::ResidencyState;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelProfile {
    pub id: ModelId,
    pub family: String,
    pub parameter_class: String,
    pub context_window: Option<u32>,
    pub vram_required_mb: Option<u64>,
    pub ram_required_mb: Option<u64>,
    pub cold_load_estimate_ms: Option<u64>,
    pub supported_runtimes: Vec<RuntimeId>,
    /// Whether the model supports SSE streaming responses.
    /// `None` means unknown (treat as permissive); `Some(false)` means
    /// explicitly non-streaming; `Some(true)` means streaming is supported.
    pub supports_streaming: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResidencyGroup {
    pub id: ResidencyGroupId,
    pub purpose: Vec<String>,
    pub models: Vec<ModelId>,
    pub keep_hot: bool,
    pub allow_background_load: bool,
    /// Pinned groups are protected from eviction unless a force policy
    /// explicitly overrides protection.
    pub pinned: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeSnapshot {
    pub runtime_id: RuntimeId,
    pub available: bool,
    pub residents: Vec<ModelResident>,
    /// Models the runtime reports as configured/registered (e.g. listed by
    /// `/v1/models`). Configuration is not evidence of residency — see
    /// `docs/live_validation/residency-truth-contract.md`. Use this for
    /// candidate enumeration and rejected-options reasoning, not for hot
    /// reuse bonuses.
    #[serde(default)]
    pub configured_models: Vec<ModelId>,
    pub memory: RuntimeMemorySnapshot,
    pub active_requests: Vec<ActiveExecution>,
    /// Co-residency feasibility observed from the runtime's colocation matrix.
    /// `None` means the runtime exposes no matrix, so colocation is *unknown*
    /// and the policy must not infer a constraint; `Some` is authoritative —
    /// a model pair absent from every loadout cannot be GPU-resident together.
    /// Currently populated only by the llama-swap adapter when a `config_path`
    /// with a `matrix` block is supplied.
    #[serde(default)]
    pub colocation: Option<ColocationConstraints>,
}

/// Co-residency feasibility derived from a runtime's colocation matrix
/// (currently llama-swap's `matrix` block, read off-API from the config file
/// since llama-swap does not expose it over HTTP). Surfaced on
/// [`RuntimeSnapshot`] so scheduling policy can answer "may A and B be
/// GPU-resident together?" through the observed snapshot, without depending on
/// any runtime adapter.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColocationConstraints {
    /// Declared co-resident loadouts. Each inner vec is a set of model ids that
    /// may be GPU-resident together — sorted and deduplicated at construction —
    /// derived from one branch of a matrix colocation set's DSL expression.
    pub loadouts: Vec<Vec<ModelId>>,
}

impl ColocationConstraints {
    /// Whether `a` and `b` may be GPU-resident at the same time: `true` when
    /// some declared loadout contains both. Note an empty constraint set admits
    /// no pair — a runtime that declares a matrix with no colocation set treats
    /// every pair as mutually exclusive.
    pub fn can_colocate(&self, a: &ModelId, b: &ModelId) -> bool {
        self.loadouts
            .iter()
            .any(|loadout| loadout.contains(a) && loadout.contains(b))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeMemorySnapshot {
    pub vram_total_mb: Option<u64>,
    pub vram_used_mb: Option<u64>,
    pub ram_total_mb: Option<u64>,
    pub ram_used_mb: Option<u64>,
}

impl RuntimeMemorySnapshot {
    pub fn pressure_percent(&self) -> Option<u64> {
        let used = self.vram_used_mb?;
        let total = self.vram_total_mb?;
        (total > 0).then_some((used * 100) / total)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelResident {
    pub model_id: ModelId,
    pub state: ResidencyState,
    pub vram_mb: Option<u64>,
    pub ram_mb: Option<u64>,
    pub kv_cache_mb: Option<u64>,
    pub loaded_since: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveExecution {
    pub request_id: RequestId,
    pub model_id: ModelId,
    pub started_at: DateTime<Utc>,
}
