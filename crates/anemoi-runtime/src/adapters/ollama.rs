use anemoi_core::{
    ModelId, ModelResident, ResidencyState, RuntimeId, RuntimeMemorySnapshot, RuntimeSnapshot,
};
use async_trait::async_trait;
use reqwest::Url;
use serde::Deserialize;
use uuid::Uuid;

use crate::util::bytes_to_mb;
use crate::{ExecutionHandle, ExecutionRequest, LoadHandle, RuntimeAdapter, RuntimeError};

#[derive(Debug, Clone)]
pub struct OllamaAdapter {
    id: RuntimeId,
    base_url: Url,
    client: reqwest::Client,
}

impl OllamaAdapter {
    pub fn new(id: RuntimeId, base_url: &str) -> Result<Self, RuntimeError> {
        Ok(Self {
            id,
            base_url: Url::parse(base_url).map_err(|error| RuntimeError::Url(error.to_string()))?,
            client: reqwest::Client::new(),
        })
    }
}

#[derive(Debug, Deserialize)]
struct OllamaPsResponse {
    #[serde(default)]
    models: Vec<OllamaRunningModel>,
}

#[derive(Debug, Deserialize)]
struct OllamaRunningModel {
    name: String,
    #[serde(default)]
    size_vram: Option<u64>,
    #[serde(default)]
    size: Option<u64>,
}

#[async_trait]
impl RuntimeAdapter for OllamaAdapter {
    fn id(&self) -> RuntimeId {
        self.id.clone()
    }

    async fn inspect(&self) -> Result<RuntimeSnapshot, RuntimeError> {
        let url = self
            .base_url
            .join("/api/ps")
            .map_err(|error| RuntimeError::Url(error.to_string()))?;
        let http_response = self.client.get(url).send().await?;
        if !http_response.status().is_success() {
            return Ok(RuntimeSnapshot {
                runtime_id: self.id.clone(),
                available: false,
                residents: Vec::new(),
                configured_models: Vec::new(),
                memory: RuntimeMemorySnapshot::default(),
                active_requests: Vec::new(),
                colocation: None,
            });
        }

        let response: OllamaPsResponse = http_response.json().await?;
        let residents = response
            .models
            .into_iter()
            .map(|model| ModelResident {
                model_id: ModelId(model.name),
                state: ResidencyState::HotGpu,
                vram_mb: model.size_vram.map(bytes_to_mb),
                ram_mb: model.size.map(bytes_to_mb),
                kv_cache_mb: None,
                loaded_since: None,
            })
            .collect();

        Ok(RuntimeSnapshot {
            runtime_id: self.id.clone(),
            available: true,
            residents,
            configured_models: Vec::new(),
            memory: RuntimeMemorySnapshot::default(),
            active_requests: Vec::new(),
            colocation: None,
        })
    }

    /// Ollama loads a model into memory lazily on its first inference request,
    /// and weight downloads happen via an explicit `ollama pull` outside Anemoi's
    /// control plane. Anemoi decides residency; it does not mutate runtime
    /// infrastructure (AGENTS.md §2/§4), so it does not proactively load Ollama
    /// models. Returning `Unsupported` (rather than a fabricated `LoadHandle`)
    /// keeps staging honest: the worker observes that the load did not happen
    /// instead of marking the intent `Completed` for a load that never ran.
    async fn load_model(&self, _model: &ModelId) -> Result<LoadHandle, RuntimeError> {
        Err(RuntimeError::Unsupported("ollama load"))
    }

    async fn unload_model(&self, _model: &ModelId) -> Result<(), RuntimeError> {
        Err(RuntimeError::Unsupported("ollama unload"))
    }

    async fn execute(&self, request: ExecutionRequest) -> Result<ExecutionHandle, RuntimeError> {
        Ok(ExecutionHandle {
            id: Uuid::new_v4(),
            request,
        })
    }
}
