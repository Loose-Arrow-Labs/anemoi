mod decisions;
mod ids;
mod requests;
mod runtime;
mod states;

pub use decisions::{
    ActionKind, ActionPlan, Decision, DecisionAction, DecisionReason, DecisionScore, Explanation,
    RejectedOption, RuntimeAction, ScoreContribution,
};
pub use ids::{DomainId, ModelId, RequestId, ResidencyGroupId, RuntimeId};
pub use requests::{
    EscalationIntent, ExecutionMode, ExecutionRequest, InferenceRequest, QualityFloor,
};
pub use runtime::{
    ActiveExecution, ColocationConstraints, ModelProfile, ModelResident, ResidencyGroup,
    RuntimeMemorySnapshot, RuntimeSnapshot,
};
pub use states::ResidencyState;

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

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
        Uuid::parse_str(&request.id.0).expect("uuid request id");
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
}
