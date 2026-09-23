pub mod schema;
pub mod validation;

pub use schema::{
    AnemoiConfig, ConfigError, ContinuityConfig, DomainConfig, ModelProfileConfig,
    ResidencyGroupConfig, RuntimeConfig, RuntimeResidentConfig, KNOWN_RUNTIME_ADAPTERS,
};
pub use validation::{
    validate_config, ConfigDiagnostic, ConfigValidationError, DiagnosticSeverity,
};

#[cfg(test)]
fn example_config() -> schema::AnemoiConfig {
    let config_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("config")
        .join("anemoi.example.yaml");
    schema::AnemoiConfig::from_yaml_file(config_path).expect("example config")
}

#[cfg(test)]
fn live_llama_swap_config() -> schema::AnemoiConfig {
    serde_yaml::from_str(
        r#"
domains:
  coding:
    rosters: [small_swarm, large_models]
residency_groups:
  small_swarm:
    purpose: [interactive coding continuity]
    keep_hot: true
    allow_background_load: true
    models: [qwen9b]
  large_models:
    purpose: [higher quality coding synthesis]
    keep_hot: false
    allow_background_load: true
    models: [qwen35_a3b]
models:
  qwen9b:
    family: qwen
    parameter_class: 9b
    context_window: 32768
    vram_required_mb: 9000
    ram_required_mb: 12000
    cold_load_estimate_ms: 18000
    supported_runtimes: [llama_swap]
  qwen35_a3b:
    family: qwen
    parameter_class: 35b
    context_window: 32768
    vram_required_mb: 30000
    ram_required_mb: 45000
    cold_load_estimate_ms: 45000
    supported_runtimes: [llama_swap]
runtimes:
  llama_swap:
    adapter: llama_swap
    base_url: http://127.0.0.1:8085
continuity:
  keep_small_worker_hot: true
  background_load: true
  max_blank_wait_ms: 1500
  prefer_degraded_response_over_silence: true
"#,
    )
    .expect("live llama-swap config")
}

#[cfg(test)]
fn live_llama_swap_example_config() -> schema::AnemoiConfig {
    let config_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("config")
        .join("anemoi.llama-swap.example.yaml");
    schema::AnemoiConfig::from_yaml_file(config_path).expect("llama-swap example config")
}
