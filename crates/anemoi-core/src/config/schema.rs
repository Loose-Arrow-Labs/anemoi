use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::types::{DomainId, ModelId, ResidencyGroupId, ResidencyState, RuntimeId};

use super::validation::{expand_env_vars, validate_config, ConfigValidationError};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DomainConfig {
    /// Static roster: named residency groups whose models are always candidates.
    #[serde(default)]
    pub rosters: Vec<ResidencyGroupId>,
    /// Live roster: when set, use this runtime's `configured_models` snapshot
    /// as the candidate pool instead of (or in addition to) any static rosters.
    /// Model profiles are synthesised on the fly from the model IDs reported by
    /// the runtime.
    #[serde(default)]
    pub live_roster: Option<RuntimeId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeConfig {
    pub adapter: String,
    pub base_url: Option<String>,
    pub auth_token: Option<String>,
    #[serde(default)]
    pub initial_residents: Vec<RuntimeResidentConfig>,
    /// Path to the runtime's own config file, read for adapter-specific
    /// metadata not exposed over the wire. For the `llama_swap` adapter this is
    /// the llama-swap YAML whose `matrix` block declares colocation sets.
    #[serde(default)]
    pub config_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeResidentConfig {
    pub model_id: ModelId,
    pub state: ResidencyState,
    pub vram_mb: Option<u64>,
    pub ram_mb: Option<u64>,
    pub kv_cache_mb: Option<u64>,
}

impl RuntimeResidentConfig {
    pub fn into_resident(self) -> crate::types::ModelResident {
        crate::types::ModelResident {
            model_id: self.model_id,
            state: self.state,
            vram_mb: self.vram_mb,
            ram_mb: self.ram_mb,
            kv_cache_mb: self.kv_cache_mb,
            loaded_since: None,
        }
    }
}

pub const KNOWN_RUNTIME_ADAPTERS: &[&str] =
    &["mock", "ollama", "llama_cpp", "llama_server", "llama_swap"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContinuityConfig {
    pub keep_small_worker_hot: bool,
    pub background_load: bool,
    pub max_blank_wait_ms: u64,
    pub prefer_degraded_response_over_silence: bool,
}

impl Default for ContinuityConfig {
    fn default() -> Self {
        Self {
            keep_small_worker_hot: true,
            background_load: true,
            max_blank_wait_ms: 1500,
            prefer_degraded_response_over_silence: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnemoiConfig {
    pub domains: HashMap<DomainId, DomainConfig>,
    /// Static residency groups. Not required when all domains use `live_roster`.
    #[serde(default)]
    pub residency_groups: HashMap<ResidencyGroupId, ResidencyGroupConfig>,
    /// Static model profiles. Not required when all domains use `live_roster`.
    #[serde(default)]
    pub models: HashMap<ModelId, ModelProfileConfig>,
    pub runtimes: HashMap<RuntimeId, RuntimeConfig>,
    #[serde(default)]
    pub continuity: ContinuityConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResidencyGroupConfig {
    #[serde(default)]
    pub purpose: Vec<String>,
    pub models: Vec<ModelId>,
    #[serde(default)]
    pub keep_hot: bool,
    #[serde(default)]
    pub allow_background_load: bool,
    #[serde(default)]
    pub pinned: bool,
}

impl ResidencyGroupConfig {
    pub fn into_group(self, id: ResidencyGroupId) -> crate::types::ResidencyGroup {
        crate::types::ResidencyGroup {
            id,
            purpose: self.purpose,
            models: self.models,
            keep_hot: self.keep_hot,
            allow_background_load: self.allow_background_load,
            pinned: self.pinned,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelProfileConfig {
    pub family: String,
    pub parameter_class: String,
    pub context_window: Option<u32>,
    pub vram_required_mb: Option<u64>,
    pub ram_required_mb: Option<u64>,
    pub cold_load_estimate_ms: Option<u64>,
    pub supported_runtimes: Vec<RuntimeId>,
    /// Whether the model supports SSE streaming responses.
    /// `None` means unknown (treat as permissive); `Some(false)` means
    /// explicitly non-streaming; `Some(true)` means streaming is supported.
    #[serde(default)]
    pub supports_streaming: Option<bool>,
}

impl ModelProfileConfig {
    pub fn into_profile(self, id: ModelId) -> crate::types::ModelProfile {
        crate::types::ModelProfile {
            id,
            family: self.family,
            parameter_class: self.parameter_class,
            context_window: self.context_window,
            vram_required_mb: self.vram_required_mb,
            ram_required_mb: self.ram_required_mb,
            cold_load_estimate_ms: self.cold_load_estimate_ms,
            supported_runtimes: self.supported_runtimes,
            supports_streaming: self.supports_streaming,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("failed to read config: {0}")]
    Read(#[from] std::io::Error),
    #[error("failed to parse config: {0}")]
    Parse(#[from] serde_yaml::Error),
}

impl AnemoiConfig {
    pub fn from_yaml_file(path: impl AsRef<std::path::Path>) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path)?;
        let expanded = expand_env_vars(&text);
        Ok(serde_yaml::from_str(&expanded)?)
    }

    pub fn from_yaml_file_raw(path: impl AsRef<std::path::Path>) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path)?;
        Ok(serde_yaml::from_str(&text)?)
    }

    pub fn from_yaml_str(text: &str) -> Result<Self, ConfigError> {
        let expanded = expand_env_vars(text);
        Ok(serde_yaml::from_str(&expanded)?)
    }

    pub fn validate(&self) -> Result<(), ConfigValidationError> {
        let diagnostics = validate_config(self);
        if diagnostics.is_empty() {
            Ok(())
        } else {
            Err(ConfigValidationError { diagnostics })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{live_llama_swap_config, live_llama_swap_example_config};

    #[test]
    fn accepts_live_llama_swap_example_config() {
        let config = live_llama_swap_example_config();
        let diagnostics = validate_config(&config);
        assert_eq!(diagnostics, Vec::new());
    }

    #[test]
    fn live_config_uses_environment_for_auth() {
        let config: AnemoiConfig = serde_yaml::from_str(
            r#"
domains:
  coding:
    rosters: [small_swarm]
residency_groups:
  small_swarm:
    models: [qwen9b]
models:
  qwen9b:
    family: qwen
    parameter_class: 9b
    supported_runtimes: [llama_swap]
runtimes:
  llama_swap:
    adapter: llama_swap
    base_url: http://127.0.0.1:8085
    auth_token: "${ANEMOI_LLAMA_SWAP_AUTH_TOKEN}"
"#,
        )
        .expect("config with env auth reference");

        let diagnostics = validate_config(&config);
        assert_eq!(diagnostics, Vec::new());
        assert_eq!(
            config
                .runtimes
                .get(&RuntimeId("llama_swap".to_string()))
                .expect("runtime")
                .auth_token,
            Some("${ANEMOI_LLAMA_SWAP_AUTH_TOKEN}".to_string())
        );
    }

    #[test]
    fn model_profile_config_deserializes_supports_streaming_true() {
        let config: ModelProfileConfig = serde_yaml::from_str(
            r#"
family: qwen
parameter_class: 9b
supported_runtimes: [llama_swap]
supports_streaming: true
"#,
        )
        .expect("profile with supports_streaming: true");
        assert_eq!(config.supports_streaming, Some(true));
    }

    #[test]
    fn model_profile_config_deserializes_supports_streaming_false() {
        let config: ModelProfileConfig = serde_yaml::from_str(
            r#"
family: qwen
parameter_class: 9b
supported_runtimes: [llama_swap]
supports_streaming: false
"#,
        )
        .expect("profile with supports_streaming: false");
        assert_eq!(config.supports_streaming, Some(false));
    }

    #[test]
    fn model_profile_config_supports_streaming_absent_is_none() {
        let config: ModelProfileConfig = serde_yaml::from_str(
            r#"
family: qwen
parameter_class: 9b
supported_runtimes: [llama_swap]
"#,
        )
        .expect("profile without supports_streaming");
        assert_eq!(config.supports_streaming, None);
    }

    #[test]
    fn into_profile_carries_supports_streaming() {
        let config = ModelProfileConfig {
            family: "qwen".to_string(),
            parameter_class: "9b".to_string(),
            context_window: None,
            vram_required_mb: None,
            ram_required_mb: None,
            cold_load_estimate_ms: None,
            supported_runtimes: vec![RuntimeId("llama_swap".to_string())],
            supports_streaming: Some(true),
        };
        let profile = config.into_profile(ModelId("qwen9b".to_string()));
        assert_eq!(profile.supports_streaming, Some(true));
    }

    #[test]
    fn live_config_keeps_small_worker_and_large_target_in_distinct_groups() {
        let config = live_llama_swap_config();
        let small_group = config
            .residency_groups
            .get(&ResidencyGroupId("small_swarm".to_string()))
            .expect("small_swarm group");
        let large_group = config
            .residency_groups
            .get(&ResidencyGroupId("large_models".to_string()))
            .expect("large_models group");
        assert_eq!(small_group.models, vec![ModelId("qwen9b".to_string())]);
        assert_eq!(large_group.models, vec![ModelId("qwen35_a3b".to_string())]);
        assert!(small_group.keep_hot);
        assert!(!large_group.keep_hot);
    }
}
