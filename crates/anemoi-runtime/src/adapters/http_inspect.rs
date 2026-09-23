use anemoi_core::{ModelId, RuntimeId, RuntimeMemorySnapshot, RuntimeSnapshot};
use async_trait::async_trait;
use reqwest::Url;
use uuid::Uuid;

use crate::{ExecutionHandle, ExecutionRequest, LoadHandle, RuntimeAdapter, RuntimeError};

#[derive(Debug, Clone)]
pub struct HttpInspectAdapter {
    id: RuntimeId,
    base_url: Url,
    client: reqwest::Client,
}

impl HttpInspectAdapter {
    pub fn new(id: RuntimeId, base_url: &str) -> Result<Self, RuntimeError> {
        Ok(Self {
            id,
            base_url: Url::parse(base_url).map_err(|error| RuntimeError::Url(error.to_string()))?,
            client: reqwest::Client::new(),
        })
    }
}

#[async_trait]
impl RuntimeAdapter for HttpInspectAdapter {
    fn id(&self) -> RuntimeId {
        self.id.clone()
    }

    async fn inspect(&self) -> Result<RuntimeSnapshot, RuntimeError> {
        let health_url = self
            .base_url
            .join("/")
            .map_err(|error| RuntimeError::Url(error.to_string()))?;
        let available = self.client.get(health_url).send().await.is_ok();
        Ok(RuntimeSnapshot {
            runtime_id: self.id.clone(),
            available,
            residents: Vec::new(),
            configured_models: Vec::new(),
            memory: RuntimeMemorySnapshot::default(),
            active_requests: Vec::new(),
            colocation: None,
        })
    }

    async fn load_model(&self, model: &ModelId) -> Result<LoadHandle, RuntimeError> {
        Ok(LoadHandle {
            id: Uuid::new_v4(),
            model_id: model.clone(),
        })
    }

    async fn unload_model(&self, _model: &ModelId) -> Result<(), RuntimeError> {
        Err(RuntimeError::Unsupported("http unload"))
    }

    async fn execute(&self, request: ExecutionRequest) -> Result<ExecutionHandle, RuntimeError> {
        Ok(ExecutionHandle {
            id: Uuid::new_v4(),
            request,
        })
    }
}
