use anemoi_core::{
    ActiveExecution, ModelId, ModelResident, ResidencyState, RuntimeId, RuntimeMemorySnapshot,
    RuntimeSnapshot,
};
use async_trait::async_trait;
use chrono::Utc;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use crate::{ExecutionHandle, ExecutionRequest, LoadHandle, RuntimeAdapter, RuntimeError};

#[derive(Debug, Clone)]
pub struct MockRuntimeAdapter {
    id: RuntimeId,
    snapshot: Arc<RwLock<RuntimeSnapshot>>,
}

impl MockRuntimeAdapter {
    pub fn new(id: RuntimeId, residents: Vec<ModelResident>) -> Self {
        Self {
            id: id.clone(),
            snapshot: Arc::new(RwLock::new(RuntimeSnapshot {
                runtime_id: id,
                available: true,
                residents,
                configured_models: Vec::new(),
                memory: RuntimeMemorySnapshot::default(),
                active_requests: Vec::new(),
                colocation: None,
            })),
        }
    }

    pub fn with_memory(self, memory: RuntimeMemorySnapshot) -> Self {
        self.snapshot.write().expect("mock runtime lock").memory = memory;
        self
    }

    pub fn with_available(self, available: bool) -> Self {
        self.snapshot.write().expect("mock runtime lock").available = available;
        self
    }

    pub fn with_resident_state(self, model_id: ModelId, state: ResidencyState) -> Self {
        let mut snapshot = self.snapshot.write().expect("mock runtime lock");
        if let Some(resident) = snapshot
            .residents
            .iter_mut()
            .find(|resident| resident.model_id == model_id)
        {
            resident.state = state;
        } else {
            snapshot.residents.push(ModelResident {
                model_id,
                state,
                vram_mb: None,
                ram_mb: None,
                kv_cache_mb: None,
                loaded_since: None,
            });
        }
        drop(snapshot);
        self
    }
}

#[async_trait]
impl RuntimeAdapter for MockRuntimeAdapter {
    fn id(&self) -> RuntimeId {
        self.id.clone()
    }

    fn is_mock(&self) -> bool {
        true
    }

    async fn inspect(&self) -> Result<RuntimeSnapshot, RuntimeError> {
        Ok(self.snapshot.read().expect("mock runtime lock").clone())
    }

    async fn load_model(&self, model: &ModelId) -> Result<LoadHandle, RuntimeError> {
        let mut snapshot = self.snapshot.write().expect("mock runtime lock");
        if !snapshot
            .residents
            .iter()
            .any(|resident| resident.model_id == *model)
        {
            snapshot.residents.push(ModelResident {
                model_id: model.clone(),
                state: ResidencyState::Loading,
                vram_mb: None,
                ram_mb: None,
                kv_cache_mb: None,
                loaded_since: None,
            });
        }

        Ok(LoadHandle {
            id: Uuid::new_v4(),
            model_id: model.clone(),
        })
    }

    async fn unload_model(&self, model: &ModelId) -> Result<(), RuntimeError> {
        let mut snapshot = self.snapshot.write().expect("mock runtime lock");
        snapshot
            .residents
            .retain(|resident| resident.model_id != *model);
        Ok(())
    }

    async fn execute(&self, request: ExecutionRequest) -> Result<ExecutionHandle, RuntimeError> {
        let mut snapshot = self.snapshot.write().expect("mock runtime lock");
        snapshot.active_requests.push(ActiveExecution {
            request_id: request.request_id.clone(),
            model_id: request.model_id.clone(),
            started_at: Utc::now(),
        });

        Ok(ExecutionHandle {
            id: Uuid::new_v4(),
            request,
        })
    }
}
