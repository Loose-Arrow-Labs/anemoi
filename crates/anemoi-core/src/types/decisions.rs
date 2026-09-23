use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::ids::{ModelId, RequestId, ResidencyGroupId, RuntimeId};

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
