use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::sync::LazyLock;
use uuid::Uuid;

pub mod governance;
pub use governance::{
    apply_overrides, infer_family_and_class, merge_profile, parameter_billions,
    validate_governance, GovernanceOverrides, GovernanceWarning, MetadataSource,
    ModelProfileOverride, RosterOverride, ESCALATION_FLOOR_BILLIONS,
};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RequestId(pub String);

impl RequestId {
    pub fn new() -> Self {
        Self(Uuid::new_v4().to_string())
    }
}

impl Default for RequestId {
    fn default() -> Self {
        Self::new()
    }
}

impl Display for RequestId {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

macro_rules! id_type {
    ($name:ident) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl Display for $name {
            fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_string())
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }
    };
}

id_type!(DomainId);
id_type!(ModelId);
id_type!(RuntimeId);
id_type!(ResidencyGroupId);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResidencyState {
    Cold,
    Loading,
    WarmCpu,
    Partial,
    HotGpu,
    Serving,
    Draining,
    Evicting,
    Failed,
}

impl ResidencyState {
    pub fn is_resident(&self) -> bool {
        !matches!(self, Self::Cold | Self::Failed)
    }

    pub fn reuse_bonus(&self) -> i32 {
        match self {
            Self::HotGpu | Self::Serving => 60,
            Self::WarmCpu => 35,
            Self::Partial | Self::Loading => 15,
            Self::Draining | Self::Evicting => -10,
            Self::Cold | Self::Failed => 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionMode {
    Interactive,
    Batch,
    Background,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QualityFloor {
    pub minimum_parameter_class: Option<String>,
}

/// Agent-declared escalation intent (issue #64). When an agent service knows a
/// larger model will be needed for an upcoming task, it declares the task type
/// and the context payload to hand off, letting Anemoi pre-stage the big model
/// and cache the context proactively. v1 is explicit-declaration only; heuristic
/// inference of escalation from request content is deferred to v2+.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EscalationIntent {
    /// Free-form task-type label (e.g. `"planning"`, `"troubleshooting"`).
    /// Recorded for observability; not interpreted by the scheduler in v1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_type: Option<String>,
    /// Prompt/context to hand to the escalated model on its first inference
    /// call. Cached on the eviction signal and replayed when the big model is
    /// ready. `None` is valid — escalation proceeds without a context handoff.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InferenceRequest {
    #[serde(default)]
    pub id: RequestId,
    pub domain: DomainId,
    pub mode: ExecutionMode,
    pub prompt_tokens_estimate: Option<u32>,
    pub max_output_tokens: Option<u32>,
    pub latency_budget_ms: Option<u64>,
    pub quality_floor: Option<QualityFloor>,
    /// Optional agent-declared escalation intent. When present and carrying a
    /// `context`, that payload is staged for handoff to the background model on
    /// model eviction (issue #64).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub escalation_intent: Option<EscalationIntent>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionRequest {
    pub request_id: RequestId,
    pub model_id: ModelId,
    pub prompt: Option<String>,
}

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Decision {
    pub id: Uuid,
    pub request_id: RequestId,
    pub action: DecisionAction,
    pub selected_model: Option<ModelId>,
    pub selected_runtime: Option<RuntimeId>,
    pub selected_group: Option<ResidencyGroupId>,
    pub background_model: Option<ModelId>,
    pub score: DecisionScore,
    pub explanation: Explanation,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionAction {
    ReuseHot,
    PromoteWarm,
    ColdLoad,
    StageBackground,
    Downgrade,
    Defer,
    Deny,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionScore {
    pub total: i32,
    pub contributions: Vec<ScoreContribution>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScoreContribution {
    pub label: String,
    pub value: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Explanation {
    pub summary: String,
    pub reasons: Vec<DecisionReason>,
    pub rejected_options: Vec<RejectedOption>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionReason {
    pub code: String,
    pub detail: String,
    pub impact: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RejectedOption {
    pub model_id: Option<ModelId>,
    pub runtime_id: Option<RuntimeId>,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    Load,
    Unload,
    Keep,
    Stage,
    Defer,
    Deny,
    NoOp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeAction {
    pub kind: ActionKind,
    pub runtime_id: RuntimeId,
    pub model_id: Option<ModelId>,
    pub is_foreground: bool,
    pub is_mutating: bool,
    pub reason: String,
    pub expected_cost_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionPlan {
    pub decision_id: Uuid,
    pub actions: Vec<RuntimeAction>,
    pub dry_run: bool,
}

impl ActionPlan {
    pub fn new(decision_id: Uuid, dry_run: bool) -> Self {
        Self {
            decision_id,
            actions: Vec::new(),
            dry_run,
        }
    }

    pub fn add_load(
        &mut self,
        runtime_id: RuntimeId,
        model_id: ModelId,
        is_foreground: bool,
        reason: String,
        expected_cost_ms: Option<u64>,
    ) {
        self.actions.push(RuntimeAction {
            kind: ActionKind::Load,
            runtime_id,
            model_id: Some(model_id),
            is_foreground,
            is_mutating: true,
            reason,
            expected_cost_ms,
        });
    }

    pub fn add_unload(&mut self, runtime_id: RuntimeId, model_id: ModelId, reason: String) {
        self.actions.push(RuntimeAction {
            kind: ActionKind::Unload,
            runtime_id,
            model_id: Some(model_id),
            is_foreground: true,
            is_mutating: true,
            reason,
            expected_cost_ms: None,
        });
    }

    pub fn add_noop(&mut self, runtime_id: RuntimeId, reason: String) {
        self.actions.push(RuntimeAction {
            kind: ActionKind::NoOp,
            runtime_id,
            model_id: None,
            is_foreground: false,
            is_mutating: false,
            reason,
            expected_cost_ms: None,
        });
    }

    pub fn get_foreground_load(&self) -> Option<&RuntimeAction> {
        self.actions
            .iter()
            .find(|a| a.kind == ActionKind::Load && a.is_foreground)
    }

    pub fn get_background_load(&self) -> Option<&RuntimeAction> {
        self.actions
            .iter()
            .find(|a| a.kind == ActionKind::Load && !a.is_foreground)
    }

    pub fn has_mutating_actions(&self) -> bool {
        self.actions.iter().any(|a| a.is_mutating)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DomainConfig {
    /// Static roster: named residency groups whose models are always candidates.
    #[serde(default)]
    pub rosters: Vec<ResidencyGroupId>,
    /// Live roster: when set, use this runtime's `configured_models` snapshot
    /// as the candidate pool instead of (or in addition to) any static rosters.
    /// Model profiles are synthesised on the fly from the model IDs reported by
    /// the runtime.
    #[serde(default)]
    pub live_roster: Option<RuntimeId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeConfig {
    pub adapter: String,
    pub base_url: Option<String>,
    pub auth_token: Option<String>,
    #[serde(default)]
    pub initial_residents: Vec<RuntimeResidentConfig>,
    /// Path to the runtime's own config file, read for adapter-specific
    /// metadata not exposed over the wire. For the `llama_swap` adapter this is
    /// the llama-swap YAML whose `matrix` block declares colocation sets.
    #[serde(default)]
    pub config_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeResidentConfig {
    pub model_id: ModelId,
    pub state: ResidencyState,
    pub vram_mb: Option<u64>,
    pub ram_mb: Option<u64>,
    pub kv_cache_mb: Option<u64>,
}

impl RuntimeResidentConfig {
    pub fn into_resident(self) -> ModelResident {
        ModelResident {
            model_id: self.model_id,
            state: self.state,
            vram_mb: self.vram_mb,
            ram_mb: self.ram_mb,
            kv_cache_mb: self.kv_cache_mb,
            loaded_since: None,
        }
    }
}

pub const KNOWN_RUNTIME_ADAPTERS: &[&str] =
    &["mock", "ollama", "llama_cpp", "llama_server", "llama_swap"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigDiagnostic {
    pub path: String,
    pub severity: DiagnosticSeverity,
    pub message: String,
}

impl ConfigDiagnostic {
    pub fn error(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            severity: DiagnosticSeverity::Error,
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticSeverity {
    Error,
    Warning,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigValidationError {
    pub diagnostics: Vec<ConfigDiagnostic>,
}

impl Display for ConfigValidationError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str("invalid Anemoi config")?;
        for diagnostic in &self.diagnostics {
            write!(
                f,
                "\n- {:?} {}: {}",
                diagnostic.severity, diagnostic.path, diagnostic.message
            )?;
        }
        Ok(())
    }
}

impl std::error::Error for ConfigValidationError {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContinuityConfig {
    pub keep_small_worker_hot: bool,
    pub background_load: bool,
    pub max_blank_wait_ms: u64,
    pub prefer_degraded_response_over_silence: bool,
}

impl Default for ContinuityConfig {
    fn default() -> Self {
        Self {
            keep_small_worker_hot: true,
            background_load: true,
            max_blank_wait_ms: 1500,
            prefer_degraded_response_over_silence: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnemoiConfig {
    pub domains: HashMap<DomainId, DomainConfig>,
    /// Static residency groups. Not required when all domains use `live_roster`.
    #[serde(default)]
    pub residency_groups: HashMap<ResidencyGroupId, ResidencyGroupConfig>,
    /// Static model profiles. Not required when all domains use `live_roster`.
    #[serde(default)]
    pub models: HashMap<ModelId, ModelProfileConfig>,
    pub runtimes: HashMap<RuntimeId, RuntimeConfig>,
    #[serde(default)]
    pub continuity: ContinuityConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResidencyGroupConfig {
    #[serde(default)]
    pub purpose: Vec<String>,
    pub models: Vec<ModelId>,
    #[serde(default)]
    pub keep_hot: bool,
    #[serde(default)]
    pub allow_background_load: bool,
    #[serde(default)]
    pub pinned: bool,
}

impl ResidencyGroupConfig {
    pub fn into_group(self, id: ResidencyGroupId) -> ResidencyGroup {
        ResidencyGroup {
            id,
            purpose: self.purpose,
            models: self.models,
            keep_hot: self.keep_hot,
            allow_background_load: self.allow_background_load,
            pinned: self.pinned,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelProfileConfig {
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
    #[serde(default)]
    pub supports_streaming: Option<bool>,
}

impl ModelProfileConfig {
    pub fn into_profile(self, id: ModelId) -> ModelProfile {
        ModelProfile {
            id,
            family: self.family,
            parameter_class: self.parameter_class,
            context_window: self.context_window,
            vram_required_mb: self.vram_required_mb,
            ram_required_mb: self.ram_required_mb,
            cold_load_estimate_ms: self.cold_load_estimate_ms,
            supported_runtimes: self.supported_runtimes,
            supports_streaming: self.supports_streaming,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("failed to read config: {0}")]
    Read(#[from] std::io::Error),
    #[error("failed to parse config: {0}")]
    Parse(#[from] serde_yaml::Error),
}

impl AnemoiConfig {
    pub fn from_yaml_file(path: impl AsRef<std::path::Path>) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path)?;
        let expanded = expand_env_vars(&text);
        Ok(serde_yaml::from_str(&expanded)?)
    }

    pub fn from_yaml_file_raw(path: impl AsRef<std::path::Path>) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path)?;
        Ok(serde_yaml::from_str(&text)?)
    }

    pub fn from_yaml_str(text: &str) -> Result<Self, ConfigError> {
        let expanded = expand_env_vars(text);
        Ok(serde_yaml::from_str(&expanded)?)
    }

    pub fn validate(&self) -> Result<(), ConfigValidationError> {
        let diagnostics = validate_config(self);
        if diagnostics.is_empty() {
            Ok(())
        } else {
            Err(ConfigValidationError { diagnostics })
        }
    }
}

static ENV_VAR_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    // Either a braced reference `${...}` (whose body may carry a `:-`/`:+`
    // modifier) or a bare `$VAR` whose name is a standard identifier.
    regex::Regex::new(r"\$\{([^}]*)\}|\$([A-Za-z_][A-Za-z0-9_]*)").expect("env var regex")
});

/// Expands environment-variable references in `text` before YAML parsing.
///
/// Supported forms:
/// - `${VAR}` / `$VAR` — replaced with the value of `VAR`. A bare `$VAR` name is
///   `[A-Za-z_][A-Za-z0-9_]*`.
/// - `${VAR:-default}` — the value of `VAR` if set and non-empty, else `default`.
/// - `${VAR:+alt}` — `alt` if `VAR` is set and non-empty, else the empty string.
///
/// A plain `${VAR}` / `$VAR` whose variable is unset is left **verbatim** (e.g.
/// `${MISSING}` stays `${MISSING}`) so a missing value is visible in the parsed
/// config rather than silently becoming empty. The `:-` / `:+` forms always
/// resolve — supplying a default is the whole point.
///
/// Not supported: nested references (expansion is a single left-to-right pass,
/// so `${${INNER}}` is not resolved inner-first) and `$$` escaping. Keep config
/// references to the forms listed above.
fn expand_env_vars(text: &str) -> String {
    ENV_VAR_RE
        .replace_all(text, |captures: &regex::Captures| {
            // Bare `$VAR`: expand if set, else preserve the literal.
            if let Some(bare) = captures.get(2) {
                return std::env::var(bare.as_str()).unwrap_or_else(|_| captures[0].to_string());
            }

            // Braced `${...}`, possibly with a `:-` / `:+` modifier.
            let inner = &captures[1];
            if let Some((name, default)) = inner.split_once(":-") {
                match std::env::var(name) {
                    Ok(value) if !value.is_empty() => value,
                    _ => default.to_string(),
                }
            } else if let Some((name, alt)) = inner.split_once(":+") {
                match std::env::var(name) {
                    Ok(value) if !value.is_empty() => alt.to_string(),
                    _ => String::new(),
                }
            } else {
                // Plain `${VAR}`: expand if set, else preserve the literal.
                std::env::var(inner).unwrap_or_else(|_| captures[0].to_string())
            }
        })
        .into_owned()
}

pub fn validate_config(config: &AnemoiConfig) -> Vec<ConfigDiagnostic> {
    let mut diagnostics = Vec::new();

    let mut domains = config.domains.iter().collect::<Vec<_>>();
    domains.sort_by_key(|(domain_id, _)| domain_id.to_string());
    for (domain_id, domain) in domains {
        if domain.rosters.is_empty() && domain.live_roster.is_none() {
            diagnostics.push(ConfigDiagnostic::error(
                format!("domains.{domain_id}.rosters"),
                "domain must reference at least one residency group or set live_roster",
            ));
        }

        for (index, group_id) in domain.rosters.iter().enumerate() {
            if !config.residency_groups.contains_key(group_id) {
                diagnostics.push(ConfigDiagnostic::error(
                    format!("domains.{domain_id}.rosters[{index}]"),
                    format!("unknown residency group '{group_id}'"),
                ));
            }
        }
    }

    let mut groups = config.residency_groups.iter().collect::<Vec<_>>();
    groups.sort_by_key(|(group_id, _)| group_id.to_string());
    for (group_id, group) in groups {
        if group.models.is_empty() {
            diagnostics.push(ConfigDiagnostic::error(
                format!("residency_groups.{group_id}.models"),
                "residency group must reference at least one model",
            ));
        }

        for (index, model_id) in group.models.iter().enumerate() {
            if !config.models.contains_key(model_id) {
                diagnostics.push(ConfigDiagnostic::error(
                    format!("residency_groups.{group_id}.models[{index}]"),
                    format!("unknown model '{model_id}'"),
                ));
            }
        }
    }

    let mut models = config.models.iter().collect::<Vec<_>>();
    models.sort_by_key(|(model_id, _)| model_id.to_string());
    for (model_id, model) in models {
        for (index, runtime_id) in model.supported_runtimes.iter().enumerate() {
            if !config.runtimes.contains_key(runtime_id) {
                diagnostics.push(ConfigDiagnostic::error(
                    format!("models.{model_id}.supported_runtimes[{index}]"),
                    format!("unknown runtime '{runtime_id}'"),
                ));
            }
        }
    }

    let mut runtimes = config.runtimes.iter().collect::<Vec<_>>();
    runtimes.sort_by_key(|(runtime_id, _)| runtime_id.to_string());
    for (runtime_id, runtime) in runtimes {
        if !KNOWN_RUNTIME_ADAPTERS.contains(&runtime.adapter.as_str()) {
            diagnostics.push(ConfigDiagnostic::error(
                format!("runtimes.{runtime_id}.adapter"),
                format!("unknown runtime adapter '{}'", runtime.adapter),
            ));
        }

        if KNOWN_RUNTIME_ADAPTERS.contains(&runtime.adapter.as_str())
            && runtime.adapter != "mock"
            && runtime.base_url.is_none()
        {
            diagnostics.push(ConfigDiagnostic::error(
                format!("runtimes.{runtime_id}.base_url"),
                format!("runtime adapter '{}' requires a base_url", runtime.adapter),
            ));
        }

        for (index, resident) in runtime.initial_residents.iter().enumerate() {
            if !config.models.contains_key(&resident.model_id) {
                diagnostics.push(ConfigDiagnostic::error(
                    format!("runtimes.{runtime_id}.initial_residents[{index}].model_id"),
                    format!("unknown model '{}'", resident.model_id),
                ));
            }
        }
    }

    diagnostics.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then(left.severity.cmp(&right.severity))
            .then(left.message.cmp(&right.message))
    });
    diagnostics
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_example_config() {
        let config = example_config();

        let diagnostics = validate_config(&config);

        assert_eq!(diagnostics, Vec::new());
    }

    #[test]
    fn serializes_residency_state_as_snake_case() {
        let json = serde_json::to_string(&ResidencyState::HotGpu).expect("json");

        assert_eq!(json, "\"hot_gpu\"");
    }

    #[test]
    fn serializes_decision_action_as_snake_case() {
        let json = serde_json::to_string(&DecisionAction::StageBackground).expect("json");

        assert_eq!(json, "\"stage_background\"");
    }

    #[test]
    fn deserializes_interactive_execution_mode() {
        let mode: ExecutionMode = serde_json::from_str("\"interactive\"").expect("mode");

        assert_eq!(mode, ExecutionMode::Interactive);
    }

    #[test]
    fn request_id_defaults_to_uuid() {
        let request: InferenceRequest = serde_json::from_value(serde_json::json!({
            "domain": "coding",
            "mode": "interactive",
            "prompt_tokens_estimate": 12,
            "max_output_tokens": 24,
            "latency_budget_ms": 1500,
            "quality_floor": null
        }))
        .expect("request");

        uuid::Uuid::parse_str(&request.id.0).expect("uuid request id");
    }

    #[test]
    fn decision_explanation_roundtrips_json() {
        let explanation = Explanation {
            summary: "Selected hot qwen9b.".to_string(),
            reasons: vec![DecisionReason {
                code: "residency".to_string(),
                detail: "qwen9b is currently HotGpu".to_string(),
                impact: 60,
            }],
            rejected_options: vec![RejectedOption {
                model_id: Some(ModelId("qwen35_a3b".to_string())),
                runtime_id: Some(RuntimeId("mock".to_string())),
                reason: "cold load exceeded latency budget".to_string(),
            }],
        };

        let json = serde_json::to_string(&explanation).expect("json");
        let roundtrip: Explanation = serde_json::from_str(&json).expect("roundtrip");

        assert_eq!(roundtrip, explanation);
    }

    #[test]
    fn score_contributions_preserve_order() {
        let score = DecisionScore {
            total: 11,
            contributions: vec![
                ScoreContribution {
                    label: "quality".to_string(),
                    value: 9,
                },
                ScoreContribution {
                    label: "latency_budget".to_string(),
                    value: 2,
                },
            ],
        };

        let json = serde_json::to_string(&score).expect("json");
        let roundtrip: DecisionScore = serde_json::from_str(&json).expect("roundtrip");

        assert_eq!(
            roundtrip
                .contributions
                .iter()
                .map(|contribution| contribution.label.as_str())
                .collect::<Vec<_>>(),
            vec!["quality", "latency_budget"]
        );
    }

    #[test]
    fn runtime_memory_pressure_is_none_without_total() {
        let memory = RuntimeMemorySnapshot {
            vram_total_mb: None,
            vram_used_mb: Some(12_000),
            ram_total_mb: None,
            ram_used_mb: None,
        };

        assert_eq!(memory.pressure_percent(), None);
    }

    #[test]
    fn runtime_memory_pressure_calculates_percent() {
        let memory = RuntimeMemorySnapshot {
            vram_total_mb: Some(24_000),
            vram_used_mb: Some(18_000),
            ram_total_mb: None,
            ram_used_mb: None,
        };

        assert_eq!(memory.pressure_percent(), Some(75));
    }

    #[test]
    fn rejects_domain_roster_referencing_unknown_group() {
        let mut config = example_config();
        config
            .domains
            .get_mut(&DomainId("coding".to_string()))
            .expect("coding domain")
            .rosters
            .push(ResidencyGroupId("missing_group".to_string()));

        let diagnostics = validate_config(&config);

        assert_eq!(
            diagnostics,
            vec![ConfigDiagnostic::error(
                "domains.coding.rosters[2]",
                "unknown residency group 'missing_group'",
            )]
        );
    }

    #[test]
    fn rejects_group_referencing_unknown_model() {
        let mut config = example_config();
        config
            .residency_groups
            .get_mut(&ResidencyGroupId("small_swarm".to_string()))
            .expect("small_swarm group")
            .models
            .push(ModelId("missing_model".to_string()));

        let diagnostics = validate_config(&config);

        assert_eq!(
            diagnostics,
            vec![ConfigDiagnostic::error(
                "residency_groups.small_swarm.models[2]",
                "unknown model 'missing_model'",
            )]
        );
    }

    #[test]
    fn rejects_model_referencing_unknown_runtime() {
        let mut config = example_config();
        config
            .models
            .get_mut(&ModelId("qwen9b".to_string()))
            .expect("qwen9b model")
            .supported_runtimes
            .push(RuntimeId("missing_runtime".to_string()));

        let diagnostics = validate_config(&config);

        assert_eq!(
            diagnostics,
            vec![ConfigDiagnostic::error(
                "models.qwen9b.supported_runtimes[1]",
                "unknown runtime 'missing_runtime'",
            )]
        );
    }

    #[test]
    fn rejects_runtime_with_unknown_adapter() {
        let mut config = example_config();
        config
            .runtimes
            .get_mut(&RuntimeId("mock".to_string()))
            .expect("mock runtime")
            .adapter = "mystery".to_string();

        let diagnostics = validate_config(&config);

        assert_eq!(
            diagnostics,
            vec![ConfigDiagnostic::error(
                "runtimes.mock.adapter",
                "unknown runtime adapter 'mystery'",
            )]
        );
    }

    #[test]
    fn rejects_runtime_initial_resident_referencing_unknown_model() {
        let mut config = example_config();
        config
            .runtimes
            .get_mut(&RuntimeId("mock".to_string()))
            .expect("mock runtime")
            .initial_residents
            .push(RuntimeResidentConfig {
                model_id: ModelId("missing_model".to_string()),
                state: ResidencyState::HotGpu,
                vram_mb: None,
                ram_mb: None,
                kv_cache_mb: None,
            });

        let diagnostics = validate_config(&config);

        assert_eq!(
            diagnostics,
            vec![ConfigDiagnostic::error(
                "runtimes.mock.initial_residents[1].model_id",
                "unknown model 'missing_model'",
            )]
        );
    }

    #[test]
    fn rejects_empty_domain_roster() {
        let mut config = example_config();
        config
            .domains
            .get_mut(&DomainId("coding".to_string()))
            .expect("coding domain")
            .rosters
            .clear();

        let diagnostics = validate_config(&config);

        assert_eq!(
            diagnostics,
            vec![ConfigDiagnostic::error(
                "domains.coding.rosters",
                "domain must reference at least one residency group or set live_roster",
            )]
        );
    }

    #[test]
    fn accepts_domain_with_live_roster_and_no_static_rosters() {
        let config: AnemoiConfig = serde_yaml::from_str(
            r#"
domains:
  coding:
    live_roster: llama_swap
runtimes:
  llama_swap:
    adapter: llama_swap
    base_url: "http://localhost:8085"
    auth_token: "LOCAL"
"#,
        )
        .expect("config");

        let diagnostics = validate_config(&config);
        assert!(
            diagnostics.is_empty(),
            "live_roster domain must pass validation without static rosters: {:?}",
            diagnostics
        );
    }

    #[test]
    fn rejects_empty_residency_group_models() {
        let mut config = example_config();
        config
            .residency_groups
            .get_mut(&ResidencyGroupId("small_swarm".to_string()))
            .expect("small_swarm group")
            .models
            .clear();

        let diagnostics = validate_config(&config);

        assert_eq!(
            diagnostics,
            vec![ConfigDiagnostic::error(
                "residency_groups.small_swarm.models",
                "residency group must reference at least one model",
            )]
        );
    }

    #[test]
    fn reports_all_config_diagnostics_deterministically() {
        let mut config = example_config();
        config
            .domains
            .get_mut(&DomainId("coding".to_string()))
            .expect("coding domain")
            .rosters
            .push(ResidencyGroupId("missing_group".to_string()));
        config
            .residency_groups
            .get_mut(&ResidencyGroupId("small_swarm".to_string()))
            .expect("small_swarm group")
            .models
            .push(ModelId("missing_model".to_string()));
        config
            .models
            .get_mut(&ModelId("qwen9b".to_string()))
            .expect("qwen9b model")
            .supported_runtimes
            .push(RuntimeId("missing_runtime".to_string()));
        config
            .runtimes
            .get_mut(&RuntimeId("mock".to_string()))
            .expect("mock runtime")
            .adapter = "mystery".to_string();

        let first = validate_config(&config);
        let second = validate_config(&config);

        assert_eq!(first, second);
        assert_eq!(
            first,
            vec![
                ConfigDiagnostic::error(
                    "domains.coding.rosters[2]",
                    "unknown residency group 'missing_group'",
                ),
                ConfigDiagnostic::error(
                    "models.qwen9b.supported_runtimes[1]",
                    "unknown runtime 'missing_runtime'",
                ),
                ConfigDiagnostic::error(
                    "residency_groups.small_swarm.models[2]",
                    "unknown model 'missing_model'",
                ),
                ConfigDiagnostic::error(
                    "runtimes.mock.adapter",
                    "unknown runtime adapter 'mystery'",
                ),
            ]
        );
    }

    #[test]
    fn model_profile_config_deserializes_supports_streaming_true() {
        let config: ModelProfileConfig = serde_yaml::from_str(
            r#"
family: qwen
parameter_class: 9b
supported_runtimes: [llama_swap]
supports_streaming: true
"#,
        )
        .expect("profile with supports_streaming: true");

        assert_eq!(config.supports_streaming, Some(true));
    }

    #[test]
    fn model_profile_config_deserializes_supports_streaming_false() {
        let config: ModelProfileConfig = serde_yaml::from_str(
            r#"
family: qwen
parameter_class: 9b
supported_runtimes: [llama_swap]
supports_streaming: false
"#,
        )
        .expect("profile with supports_streaming: false");

        assert_eq!(config.supports_streaming, Some(false));
    }

    #[test]
    fn model_profile_config_supports_streaming_absent_is_none() {
        let config: ModelProfileConfig = serde_yaml::from_str(
            r#"
family: qwen
parameter_class: 9b
supported_runtimes: [llama_swap]
"#,
        )
        .expect("profile without supports_streaming");

        assert_eq!(config.supports_streaming, None);
    }

    #[test]
    fn into_profile_carries_supports_streaming() {
        let config = ModelProfileConfig {
            family: "qwen".to_string(),
            parameter_class: "9b".to_string(),
            context_window: None,
            vram_required_mb: None,
            ram_required_mb: None,
            cold_load_estimate_ms: None,
            supported_runtimes: vec![RuntimeId("llama_swap".to_string())],
            supports_streaming: Some(true),
        };

        let profile = config.into_profile(ModelId("qwen9b".to_string()));

        assert_eq!(profile.supports_streaming, Some(true));
    }

    #[test]
    fn accepts_live_llama_swap_example_config() {
        let config_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("config")
            .join("anemoi.llama-swap.example.yaml");
        let config = AnemoiConfig::from_yaml_file(config_path).expect("llama-swap example config");

        let diagnostics = validate_config(&config);

        assert_eq!(diagnostics, Vec::new());
    }

    #[test]
    fn live_config_uses_environment_for_auth() {
        let config: AnemoiConfig = serde_yaml::from_str(
            r#"
domains:
  coding:
    rosters: [small_swarm]
residency_groups:
  small_swarm:
    models: [qwen9b]
models:
  qwen9b:
    family: qwen
    parameter_class: 9b
    supported_runtimes: [llama_swap]
runtimes:
  llama_swap:
    adapter: llama_swap
    base_url: http://127.0.0.1:8085
    auth_token: "${ANEMOI_LLAMA_SWAP_AUTH_TOKEN}"
"#,
        )
        .expect("config with env auth reference");

        let diagnostics = validate_config(&config);

        assert_eq!(diagnostics, Vec::new());
        assert_eq!(
            config
                .runtimes
                .get(&RuntimeId("llama_swap".to_string()))
                .expect("runtime")
                .auth_token,
            Some("${ANEMOI_LLAMA_SWAP_AUTH_TOKEN}".to_string())
        );
    }

    #[test]
    fn expand_env_vars_expands_set_variable() {
        std::env::set_var("ANEMOI_TEST_EXPAND_SET", "swift");
        // Both the braced and the bare form expand to the variable's value.
        assert_eq!(expand_env_vars("a=${ANEMOI_TEST_EXPAND_SET}"), "a=swift");
        assert_eq!(expand_env_vars("a=$ANEMOI_TEST_EXPAND_SET/b"), "a=swift/b");
        std::env::remove_var("ANEMOI_TEST_EXPAND_SET");
    }

    #[test]
    fn expand_env_vars_preserves_unset_variable_literally() {
        // Unset variables stay verbatim so a missing value surfaces in the parsed
        // config instead of silently collapsing to an empty string.
        assert_eq!(
            expand_env_vars("t=${ANEMOI_TEST_EXPAND_MISSING}"),
            "t=${ANEMOI_TEST_EXPAND_MISSING}"
        );
        assert_eq!(
            expand_env_vars("t=$ANEMOI_TEST_EXPAND_MISSING"),
            "t=$ANEMOI_TEST_EXPAND_MISSING"
        );
    }

    #[test]
    fn expand_env_vars_resolves_default_and_alternate_forms() {
        std::env::set_var("ANEMOI_TEST_EXPAND_PRESENT", "yes");
        // `:-` yields the value when set, the default when unset.
        assert_eq!(
            expand_env_vars("${ANEMOI_TEST_EXPAND_PRESENT:-fallback}"),
            "yes"
        );
        assert_eq!(
            expand_env_vars("${ANEMOI_TEST_EXPAND_ABSENT:-fallback}"),
            "fallback"
        );
        // `:+` yields the alternate when set, the empty string when unset.
        assert_eq!(expand_env_vars("${ANEMOI_TEST_EXPAND_PRESENT:+alt}"), "alt");
        assert_eq!(expand_env_vars("${ANEMOI_TEST_EXPAND_ABSENT:+alt}"), "");
        std::env::remove_var("ANEMOI_TEST_EXPAND_PRESENT");
    }

    #[test]
    fn live_config_rejects_missing_required_runtime_url_when_no_default_exists() {
        let mut config = live_llama_swap_config();
        config
            .runtimes
            .get_mut(&RuntimeId("llama_swap".to_string()))
            .expect("llama_swap runtime")
            .base_url = None;

        let diagnostics = validate_config(&config);

        assert_eq!(
            diagnostics,
            vec![ConfigDiagnostic::error(
                "runtimes.llama_swap.base_url",
                "runtime adapter 'llama_swap' requires a base_url",
            )]
        );
    }

    #[test]
    fn live_config_keeps_small_worker_and_large_target_in_distinct_groups() {
        let config = live_llama_swap_config();

        let small_group = config
            .residency_groups
            .get(&ResidencyGroupId("small_swarm".to_string()))
            .expect("small_swarm group");
        let large_group = config
            .residency_groups
            .get(&ResidencyGroupId("large_models".to_string()))
            .expect("large_models group");

        assert_eq!(small_group.models, vec![ModelId("qwen9b".to_string())]);
        assert_eq!(large_group.models, vec![ModelId("qwen35_a3b".to_string())]);
        assert!(small_group.keep_hot);
        assert!(!large_group.keep_hot);
    }

    fn live_llama_swap_config() -> AnemoiConfig {
        serde_yaml::from_str(
            r#"
domains:
  coding:
    rosters: [small_swarm, large_models]
residency_groups:
  small_swarm:
    purpose: [interactive coding continuity]
    keep_hot: true
    allow_background_load: true
    models: [qwen9b]
  large_models:
    purpose: [higher quality coding synthesis]
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
    supported_runtimes: [llama_swap]
  qwen35_a3b:
    family: qwen
    parameter_class: 35b
    context_window: 32768
    vram_required_mb: 30000
    ram_required_mb: 45000
    cold_load_estimate_ms: 45000
    supported_runtimes: [llama_swap]
runtimes:
  llama_swap:
    adapter: llama_swap
    base_url: http://127.0.0.1:8085
continuity:
  keep_small_worker_hot: true
  background_load: true
  max_blank_wait_ms: 1500
  prefer_degraded_response_over_silence: true
"#,
        )
        .expect("live llama-swap config")
    }

    fn example_config() -> AnemoiConfig {
        let config_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("config")
            .join("anemoi.example.yaml");
        AnemoiConfig::from_yaml_file(config_path).expect("example config")
    }
}
