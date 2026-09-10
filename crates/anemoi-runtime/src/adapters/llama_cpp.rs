use anemoi_core::{ModelId, RuntimeId, RuntimeMemorySnapshot, RuntimeSnapshot};
use async_trait::async_trait;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};
use reqwest::Url;
use serde::Deserialize;
use std::time::Duration;

use crate::util::normalize_model_id;
use crate::{ExecutionHandle, ExecutionRequest, LoadHandle, RuntimeAdapter, RuntimeError};

/// Inspect-only adapter for a llama.cpp / `llama-server` instance.
///
/// llama.cpp exposes an OpenAI-compatible `/v1/models` endpoint and a
/// `/health` endpoint. Per the residency truth contract
/// (`docs/live_validation/residency-truth-contract.md`), `/v1/models` proves
/// configuration, not residency — so `inspect()` surfaces those ids in
/// `configured_models` and leaves `residents` empty. This adapter never
/// loads, unloads, or executes.
#[derive(Debug, Clone)]
pub struct LlamaCppAdapter {
    id: RuntimeId,
    base_url: Url,
    client: reqwest::Client,
    auth_token: Option<String>,
}

impl LlamaCppAdapter {
    pub fn new(id: RuntimeId, base_url: &str) -> Result<Self, RuntimeError> {
        Self::new_with_timeout(id, base_url, Duration::from_secs(5))
    }

    pub fn new_with_timeout(
        id: RuntimeId,
        base_url: &str,
        timeout: Duration,
    ) -> Result<Self, RuntimeError> {
        Ok(Self {
            id,
            base_url: Url::parse(base_url).map_err(|error| RuntimeError::Url(error.to_string()))?,
            client: reqwest::Client::builder().timeout(timeout).build()?,
            auth_token: None,
        })
    }

    /// Configure a bearer token. The token must originate from the environment
    /// (expanded at config load); it is never committed.
    pub fn with_bearer_token(mut self, token: impl Into<String>) -> Self {
        self.auth_token = Some(token.into());
        self
    }

    /// Returns the configured model ids reported by `/v1/models`. This is
    /// configuration evidence, not residency evidence.
    pub async fn inspect_models(&self) -> Result<Vec<ModelId>, RuntimeError> {
        let url = self
            .base_url
            .join("/v1/models")
            .map_err(|error| RuntimeError::Url(error.to_string()))?;
        let response: LlamaCppModelsResponse = self
            .client
            .get(url)
            .headers(self.headers()?)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(response
            .data
            .into_iter()
            // Skip models whose id normalizes to empty; a runtime reporting a
            // blank id is malformed and must not yield an empty ModelId.
            .filter_map(|model| normalize_model_id(&model.id).ok())
            .collect())
    }

    fn headers(&self) -> Result<HeaderMap, RuntimeError> {
        let mut headers = HeaderMap::new();
        if let Some(token) = &self.auth_token {
            let value = HeaderValue::from_str(&format!("Bearer {token}"))
                .map_err(|error| RuntimeError::Url(error.to_string()))?;
            headers.insert(AUTHORIZATION, value);
        }
        Ok(headers)
    }
}

#[derive(Debug, Deserialize)]
struct LlamaCppModelsResponse {
    #[serde(default)]
    data: Vec<LlamaCppModel>,
}

#[derive(Debug, Deserialize)]
struct LlamaCppModel {
    id: String,
}

#[async_trait]
impl RuntimeAdapter for LlamaCppAdapter {
    fn id(&self) -> RuntimeId {
        self.id.clone()
    }

    async fn inspect(&self) -> Result<RuntimeSnapshot, RuntimeError> {
        let health_url = self
            .base_url
            .join("/health")
            .map_err(|error| RuntimeError::Url(error.to_string()))?;
        let health = self
            .client
            .get(health_url)
            .headers(self.headers()?)
            .send()
            .await?;

        if !health.status().is_success() {
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

        // Health passed but the model-listing endpoint can be flaky (500 or
        // timeout). The runtime is up, so report it available with no configured
        // models rather than treating a partial failure as a hard outage — that
        // would hide a healthy runtime from the scheduling path entirely.
        let configured_models = self.inspect_models().await.unwrap_or_else(|error| {
            tracing::warn!(
                runtime = %self.id,
                %error,
                "model listing failed after a healthy /health probe; reporting available with no configured models"
            );
            Vec::new()
        });

        Ok(RuntimeSnapshot {
            runtime_id: self.id.clone(),
            available: true,
            // /v1/models proves configuration, not residency — see
            // docs/live_validation/residency-truth-contract.md. residents
            // stays empty until we have evidence the model is loaded.
            residents: Vec::new(),
            configured_models,
            memory: RuntimeMemorySnapshot::default(),
            active_requests: Vec::new(),
            colocation: None,
        })
    }

    async fn load_model(&self, _model: &ModelId) -> Result<LoadHandle, RuntimeError> {
        Err(RuntimeError::Unsupported("llama-cpp load"))
    }

    async fn unload_model(&self, _model: &ModelId) -> Result<(), RuntimeError> {
        Err(RuntimeError::Unsupported("llama-cpp unload"))
    }

    async fn execute(&self, _request: ExecutionRequest) -> Result<ExecutionHandle, RuntimeError> {
        Err(RuntimeError::Unsupported("llama-cpp execute"))
    }
}
