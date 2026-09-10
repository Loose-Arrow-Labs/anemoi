use crate::{Decision, DecisionLog, TelemetryError};
use async_trait::async_trait;
use std::collections::HashMap;
use uuid::Uuid;

#[derive(Debug, Default)]
pub struct InMemoryDecisionLog {
    state: std::sync::RwLock<InMemoryDecisionLogState>,
}

impl InMemoryDecisionLog {
    pub fn new() -> Self {
        Self::default()
    }

    /// Synchronously inserts a decision, de-duplicated by id and preserving
    /// first-seen order. Used to rehydrate the index from a persisted log on
    /// construction, where an async `record_decision` cannot be awaited.
    pub(crate) fn insert_sync(&self, decision: Decision) {
        let mut state = self.state.write().unwrap();
        if !state.decisions.contains_key(&decision.id) {
            state.order.push(decision.id);
        }
        state.decisions.insert(decision.id, decision);
    }
}

#[derive(Debug, Default)]
struct InMemoryDecisionLogState {
    decisions: HashMap<Uuid, Decision>,
    order: Vec<Uuid>,
}

#[async_trait]
impl DecisionLog for InMemoryDecisionLog {
    async fn record_decision(&self, decision: &Decision) -> Result<(), TelemetryError> {
        let mut state = self.state.write().unwrap();
        if !state.decisions.contains_key(&decision.id) {
            state.order.push(decision.id);
        }
        state.decisions.insert(decision.id, decision.clone());
        Ok(())
    }

    async fn get_decision(&self, id: Uuid) -> Result<Option<Decision>, TelemetryError> {
        let state = self.state.read().unwrap();
        Ok(state.decisions.get(&id).cloned())
    }

    async fn list_decisions(&self) -> Result<Vec<Decision>, TelemetryError> {
        let state = self.state.read().unwrap();
        Ok(state
            .order
            .iter()
            .filter_map(|id| state.decisions.get(id).cloned())
            .collect())
    }
}
