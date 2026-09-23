mod config;
mod types;

pub mod governance;

// Domain types
pub use types::{
    ActionKind, ActionPlan, ActiveExecution, ColocationConstraints, Decision, DecisionAction,
    DecisionReason, DecisionScore, DomainId, EscalationIntent, ExecutionMode, ExecutionRequest,
    Explanation, InferenceRequest, ModelId, ModelProfile, ModelResident, QualityFloor,
    RejectedOption, RequestId, ResidencyGroup, ResidencyGroupId, ResidencyState, RuntimeAction,
    RuntimeId, RuntimeMemorySnapshot, RuntimeSnapshot, ScoreContribution,
};

// Configuration types and validation
pub use config::{
    validate_config, AnemoiConfig, ConfigDiagnostic, ConfigError, ConfigValidationError,
    ContinuityConfig, DiagnosticSeverity, DomainConfig, ModelProfileConfig, ResidencyGroupConfig,
    RuntimeConfig, RuntimeResidentConfig, KNOWN_RUNTIME_ADAPTERS,
};

// Governance re-exports
pub use governance::{
    apply_overrides, infer_family_and_class, merge_profile, parameter_billions,
    validate_governance, GovernanceOverrides, GovernanceWarning, MetadataSource,
    ModelProfileOverride, RosterOverride, ESCALATION_FLOOR_BILLIONS,
};
