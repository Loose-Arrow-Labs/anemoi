use anemoi_core::{
    ColocationConstraints, ModelId, RuntimeId, RuntimeMemorySnapshot, RuntimeSnapshot,
};
use async_trait::async_trait;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};
use reqwest::Url;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::{Arc, PoisonError, RwLock};
use std::time::Duration;
use uuid::Uuid;

use crate::util::normalize_model_id;
use crate::{ExecutionHandle, ExecutionRequest, LoadHandle, RuntimeAdapter, RuntimeError};

use super::event_stream::{
    residents_from_states, run_event_stream, LlamaSwapEventStream, ModelStateCache,
};
use super::matrix::LlamaSwapMatrixConfig;

#[derive(Debug, Clone)]
pub struct LlamaSwapAdapter {
    id: RuntimeId,
    base_url: Url,
    client: reqwest::Client,
    auth_token: Option<String>,
    /// Push-updated model residency, maintained by [`LlamaSwapEventStream`]
    /// from the `/api/events` SSE stream. Shared (cloned `Arc`) with any event
    /// stream started off this adapter, so `inspect` observes live state.
    model_states: ModelStateCache,
    /// Parsed `matrix` block from the llama-swap YAML, when a config path was
    /// supplied via [`LlamaSwapAdapter::with_matrix_config_path`]. `None` means
    /// matrix awareness is disabled and [`LlamaSwapAdapter::can_colocate`]
    /// conservatively reports that no two models are known to colocate.
    matrix: Option<LlamaSwapMatrixConfig>,
}

impl LlamaSwapAdapter {
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
            model_states: Arc::new(RwLock::new(HashMap::new())),
            matrix: None,
        })
    }

    pub fn with_bearer_token(mut self, token: impl Into<String>) -> Self {
        self.auth_token = Some(token.into());
        self
    }

    /// Reads and parses the `matrix` block from the llama-swap YAML config at
    /// `path`, enabling colocation awareness via
    /// [`LlamaSwapAdapter::can_colocate`]. A config file without a `matrix`
    /// block parses successfully and leaves matrix awareness disabled. Returns
    /// [`RuntimeError::Config`] when the file cannot be read or parsed.
    pub fn with_matrix_config_path(
        mut self,
        path: impl AsRef<std::path::Path>,
    ) -> Result<Self, RuntimeError> {
        self.matrix = LlamaSwapMatrixConfig::from_yaml_file(path)?;
        Ok(self)
    }

    /// Sets the parsed matrix config directly. Primarily for callers that have
    /// already loaded the config; most code should prefer
    /// [`LlamaSwapAdapter::with_matrix_config_path`].
    pub fn with_matrix_config(mut self, matrix: LlamaSwapMatrixConfig) -> Self {
        self.matrix = Some(matrix);
        self
    }

    /// The parsed matrix config, or `None` when matrix awareness is disabled.
    pub fn matrix(&self) -> Option<&LlamaSwapMatrixConfig> {
        self.matrix.as_ref()
    }

    /// Whether `a` and `b` can be GPU-resident at the same time according to the
    /// llama-swap matrix colocation sets. Returns `false` when matrix awareness
    /// is disabled (no config supplied) — absent a declared colocation set, the
    /// safe assumption is that loading one model evicts the other.
    pub fn can_colocate(&self, a: &ModelId, b: &ModelId) -> bool {
        self.matrix
            .as_ref()
            .is_some_and(|matrix| matrix.can_colocate(a, b))
    }

    /// Colocation feasibility for the scheduling policy, surfaced onto the
    /// observed snapshot. `None` when matrix awareness is disabled (no config
    /// supplied), leaving colocation unknown so the policy applies no
    /// constraint; `Some` lowers the parsed matrix into the policy-facing form.
    pub fn colocation_constraints(&self) -> Option<ColocationConstraints> {
        self.matrix
            .as_ref()
            .map(LlamaSwapMatrixConfig::colocation_constraints)
    }

    /// Read handle to the push-updated model-state cache. Empty until an event
    /// stream is started via [`LlamaSwapAdapter::start_event_stream`] and the
    /// first `modelStatus` frame arrives.
    pub fn model_states(&self) -> ModelStateCache {
        Arc::clone(&self.model_states)
    }

    /// Spawns the `/api/events` SSE subscriber on the current tokio runtime,
    /// returning a handle that keeps the background task alive (the task is
    /// aborted when the handle is dropped). Returns `None` when called outside a
    /// tokio runtime (e.g. synchronous tests), leaving the cache empty rather
    /// than panicking. The spawned task shares this adapter's cache, so
    /// [`LlamaSwapAdapter::inspect`] reflects what the stream observes.
    pub fn start_event_stream(&self) -> Option<LlamaSwapEventStream> {
        let runtime = tokio::runtime::Handle::try_current().ok()?;
        let events_url = self.base_url.join("/api/events").ok()?;
        let cache = Arc::clone(&self.model_states);
        let handle = runtime.spawn(run_event_stream(
            events_url,
            self.auth_token.clone(),
            Arc::clone(&cache),
        ));
        Some(LlamaSwapEventStream { cache, handle })
    }

    pub async fn inspect_models(&self) -> Result<Vec<ModelId>, RuntimeError> {
        let url = self
            .base_url
            .join("/v1/models")
            .map_err(|error| RuntimeError::Url(error.to_string()))?;
        let response: LlamaSwapModelsResponse = self
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
struct LlamaSwapModelsResponse {
    #[serde(default)]
    data: Vec<LlamaSwapModel>,
}

#[derive(Debug, Deserialize)]
struct LlamaSwapModel {
    id: String,
}

#[async_trait]
impl RuntimeAdapter for LlamaSwapAdapter {
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
        let residents = {
            let states = self
                .model_states
                .read()
                .unwrap_or_else(PoisonError::into_inner);
            residents_from_states(&states)
        };

        Ok(RuntimeSnapshot {
            runtime_id: self.id.clone(),
            available: true,
            // Residents come from the push-updated `/api/events` SSE cache — the
            // only residency evidence we trust. `/v1/models` proves
            // configuration, not load state; see
            // docs/live_validation/residency-truth-contract.md.
            residents,
            configured_models,
            memory: RuntimeMemorySnapshot::default(),
            active_requests: Vec::new(),
            colocation: self.colocation_constraints(),
        })
    }

    async fn load_model(&self, model: &ModelId) -> Result<LoadHandle, RuntimeError> {
        let url = self
            .base_url
            .join("/v1/chat/completions")
            .map_err(|error| RuntimeError::Url(error.to_string()))?;
        let body = serde_json::json!({
            "model": model.to_string(),
            "messages": [{"role": "user", "content": "ping"}],
            "max_tokens": 1,
        });
        self.client
            .post(url)
            .headers(self.headers()?)
            .json(&body)
            .send()
            .await?
            .error_for_status()?;
        Ok(LoadHandle {
            id: Uuid::new_v4(),
            model_id: model.clone(),
        })
    }

    async fn unload_model(&self, _model: &ModelId) -> Result<(), RuntimeError> {
        Err(RuntimeError::Unsupported("llama-swap unload"))
    }

    async fn execute(&self, request: ExecutionRequest) -> Result<ExecutionHandle, RuntimeError> {
        Ok(ExecutionHandle {
            id: Uuid::new_v4(),
            request,
        })
    }
}
