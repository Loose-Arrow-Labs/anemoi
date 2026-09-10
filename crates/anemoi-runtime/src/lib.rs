pub mod adapters;
pub mod forward;
pub mod llama_swap;
pub mod mock;
pub mod util;

use anemoi_core::{ExecutionRequest, ModelId, RuntimeId};
use async_trait::async_trait;
use std::sync::Arc;

#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error("runtime {0} is unavailable")]
    Unavailable(RuntimeId),
    #[error("http request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("invalid runtime url: {0}")]
    Url(String),
    #[error("runtime operation is not supported: {0}")]
    Unsupported(&'static str),
    #[error("failed to load llama-swap config: {0}")]
    Config(String),
}

#[derive(Debug, Clone)]
pub struct LoadHandle {
    pub id: uuid::Uuid,
    pub model_id: ModelId,
}

#[derive(Debug, Clone)]
pub struct ExecutionHandle {
    pub id: uuid::Uuid,
    pub request: ExecutionRequest,
}

#[async_trait]
pub trait RuntimeAdapter: Send + Sync {
    fn id(&self) -> RuntimeId;

    /// Whether this adapter is an in-memory mock. Live adapters (Ollama,
    /// llama-swap, llama.cpp) leave this `false`, letting callers that only hold
    /// a `&DynRuntimeAdapter` — with no access to runtime config — refuse to
    /// mutate a real runtime unless `ANEMOI_ENABLE_LIVE_EXECUTE=1` is set.
    fn is_mock(&self) -> bool {
        false
    }

    async fn inspect(&self) -> Result<anemoi_core::RuntimeSnapshot, RuntimeError>;

    async fn load_model(&self, model: &ModelId) -> Result<LoadHandle, RuntimeError>;

    async fn unload_model(&self, model: &ModelId) -> Result<(), RuntimeError>;

    async fn execute(&self, request: ExecutionRequest) -> Result<ExecutionHandle, RuntimeError>;
}

pub type DynRuntimeAdapter = Arc<dyn RuntimeAdapter>;

// Re-export adapter types.
pub use adapters::{HttpInspectAdapter, LlamaCppAdapter, OllamaAdapter};
pub use forward::{
    forward_chat_completion, mock_chat_completion, ForwardTarget, ForwardedChatCompletion,
};
pub use llama_swap::{
    ColocationSet, LlamaSwapAdapter, LlamaSwapEventStream, LlamaSwapMatrixConfig,
    LlamaSwapModelState, MatrixVar, ModelStateCache,
};
pub use mock::MockRuntimeAdapter;

#[cfg(test)]
mod tests;
