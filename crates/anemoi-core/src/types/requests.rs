use serde::{Deserialize, Serialize};

use super::ids::{DomainId, ModelId, RequestId};

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
