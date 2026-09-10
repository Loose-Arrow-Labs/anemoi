use serde::{Deserialize, Serialize};
use std::fmt::{Display, Formatter};
use std::sync::LazyLock;

use super::schema::AnemoiConfig;
use super::KNOWN_RUNTIME_ADAPTERS;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigDiagnostic {
    pub path: String,
    pub severity: DiagnosticSeverity,
    pub message: String,
}

impl ConfigDiagnostic {
    pub fn error(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            severity: DiagnosticSeverity::Error,
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticSeverity {
    Error,
    Warning,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigValidationError {
    pub diagnostics: Vec<ConfigDiagnostic>,
}

impl Display for ConfigValidationError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str("invalid Anemoi config")?;
        for diagnostic in &self.diagnostics {
            write!(
                f,
                "\n- {:?} {}: {}",
                diagnostic.severity, diagnostic.path, diagnostic.message
            )?;
        }
        Ok(())
    }
}

impl std::error::Error for ConfigValidationError {}

static ENV_VAR_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    // Either a braced reference `${...}` (whose body may carry a `:-`/`:+`
    // modifier) or a bare `$VAR` whose name is a standard identifier.
    regex::Regex::new(r"\$\{([^}]*)\}|\$([A-Za-z_][A-Za-z0-9_]*)").expect("env var regex")
});

/// Expands environment-variable references in `text` before YAML parsing.
///
/// Supported forms:
/// - `${VAR}` / `$VAR` — replaced with the value of `VAR`. A bare `$VAR` name is
///   `[A-Za-z_][A-Za-z0-9_]*`.
/// - `${VAR:-default}` — the value of `VAR` if set and non-empty, else `default`.
/// - `${VAR:+alt}` — `alt` if `VAR` is set and non-empty, else the empty string.
///
/// A plain `${VAR}` / `$VAR` whose variable is unset is left **verbatim** (e.g.
/// `${MISSING}` stays `${MISSING}`) so a missing value is visible in the parsed
/// config rather than silently becoming empty. The `:-` / `:+` forms always
/// resolve — supplying a default is the whole point.
///
/// Not supported: nested references (expansion is a single left-to-right pass,
/// so `${${INNER}}` is not resolved inner-first) and `$$` escaping. Keep config
/// references to the forms listed above.
pub(crate) fn expand_env_vars(text: &str) -> String {
    ENV_VAR_RE
        .replace_all(text, |captures: &regex::Captures| {
            // Bare `$VAR`: expand if set, else preserve the literal.
            if let Some(bare) = captures.get(2) {
                return std::env::var(bare.as_str()).unwrap_or_else(|_| captures[0].to_string());
            }

            // Braced `${...}`, possibly with a `:-` / `:+` modifier.
            let inner = &captures[1];
            if let Some((name, default)) = inner.split_once(":-") {
                match std::env::var(name) {
                    Ok(value) if !value.is_empty() => value,
                    _ => default.to_string(),
                }
            } else if let Some((name, alt)) = inner.split_once(":+") {
                match std::env::var(name) {
                    Ok(value) if !value.is_empty() => alt.to_string(),
                    _ => String::new(),
                }
            } else {
                // Plain `${VAR}`: expand if set, else preserve the literal.
                std::env::var(inner).unwrap_or_else(|_| captures[0].to_string())
            }
        })
        .into_owned()
}

pub fn validate_config(config: &AnemoiConfig) -> Vec<ConfigDiagnostic> {
    let mut diagnostics = Vec::new();

    let mut domains = config.domains.iter().collect::<Vec<_>>();
    domains.sort_by_key(|(domain_id, _)| domain_id.to_string());
    for (domain_id, domain) in domains {
        if domain.rosters.is_empty() && domain.live_roster.is_none() {
            diagnostics.push(ConfigDiagnostic::error(
                format!("domains.{domain_id}.rosters"),
                "domain must reference at least one residency group or set live_roster",
            ));
        }

        for (index, group_id) in domain.rosters.iter().enumerate() {
            if !config.residency_groups.contains_key(group_id) {
                diagnostics.push(ConfigDiagnostic::error(
                    format!("domains.{domain_id}.rosters[{index}]"),
                    format!("unknown residency group '{group_id}'"),
                ));
            }
        }
    }

    let mut groups = config.residency_groups.iter().collect::<Vec<_>>();
    groups.sort_by_key(|(group_id, _)| group_id.to_string());
    for (group_id, group) in groups {
        if group.models.is_empty() {
            diagnostics.push(ConfigDiagnostic::error(
                format!("residency_groups.{group_id}.models"),
                "residency group must reference at least one model",
            ));
        }

        for (index, model_id) in group.models.iter().enumerate() {
            if !config.models.contains_key(model_id) {
                diagnostics.push(ConfigDiagnostic::error(
                    format!("residency_groups.{group_id}.models[{index}]"),
                    format!("unknown model '{model_id}'"),
                ));
            }
        }
    }

    let mut models = config.models.iter().collect::<Vec<_>>();
    models.sort_by_key(|(model_id, _)| model_id.to_string());
    for (model_id, model) in models {
        for (index, runtime_id) in model.supported_runtimes.iter().enumerate() {
            if !config.runtimes.contains_key(runtime_id) {
                diagnostics.push(ConfigDiagnostic::error(
                    format!("models.{model_id}.supported_runtimes[{index}]"),
                    format!("unknown runtime '{runtime_id}'"),
                ));
            }
        }
    }

    let mut runtimes = config.runtimes.iter().collect::<Vec<_>>();
    runtimes.sort_by_key(|(runtime_id, _)| runtime_id.to_string());
    for (runtime_id, runtime) in runtimes {
        if !KNOWN_RUNTIME_ADAPTERS.contains(&runtime.adapter.as_str()) {
            diagnostics.push(ConfigDiagnostic::error(
                format!("runtimes.{runtime_id}.adapter"),
                format!("unknown runtime adapter '{}'", runtime.adapter),
            ));
        }

        if KNOWN_RUNTIME_ADAPTERS.contains(&runtime.adapter.as_str())
            && runtime.adapter != "mock"
            && runtime.base_url.is_none()
        {
            diagnostics.push(ConfigDiagnostic::error(
                format!("runtimes.{runtime_id}.base_url"),
                format!("runtime adapter '{}' requires a base_url", runtime.adapter),
            ));
        }

        for (index, resident) in runtime.initial_residents.iter().enumerate() {
            if !config.models.contains_key(&resident.model_id) {
                diagnostics.push(ConfigDiagnostic::error(
                    format!("runtimes.{runtime_id}.initial_residents[{index}].model_id"),
                    format!("unknown model '{}'", resident.model_id),
                ));
            }
        }
    }

    diagnostics.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then(left.severity.cmp(&right.severity))
            .then(left.message.cmp(&right.message))
    });
    diagnostics
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::schema::*;
    use crate::config::{example_config, live_llama_swap_config};
    use crate::types::*;

    #[test]
    fn accepts_example_config() {
        let config = example_config();
        let diagnostics = validate_config(&config);
        assert_eq!(diagnostics, Vec::new());
    }

    #[test]
    fn rejects_domain_roster_referencing_unknown_group() {
        let mut config = example_config();
        config
            .domains
            .get_mut(&DomainId("coding".to_string()))
            .expect("coding domain")
            .rosters
            .push(ResidencyGroupId("missing_group".to_string()));
        let diagnostics = validate_config(&config);
        assert_eq!(
            diagnostics,
            vec![ConfigDiagnostic::error(
                "domains.coding.rosters[2]",
                "unknown residency group 'missing_group'",
            )]
        );
    }

    #[test]
    fn rejects_group_referencing_unknown_model() {
        let mut config = example_config();
        config
            .residency_groups
            .get_mut(&ResidencyGroupId("small_swarm".to_string()))
            .expect("small_swarm group")
            .models
            .push(ModelId("missing_model".to_string()));
        let diagnostics = validate_config(&config);
        assert_eq!(
            diagnostics,
            vec![ConfigDiagnostic::error(
                "residency_groups.small_swarm.models[2]",
                "unknown model 'missing_model'",
            )]
        );
    }

    #[test]
    fn rejects_model_referencing_unknown_runtime() {
        let mut config = example_config();
        config
            .models
            .get_mut(&ModelId("qwen9b".to_string()))
            .expect("qwen9b model")
            .supported_runtimes
            .push(RuntimeId("missing_runtime".to_string()));
        let diagnostics = validate_config(&config);
        assert_eq!(
            diagnostics,
            vec![ConfigDiagnostic::error(
                "models.qwen9b.supported_runtimes[1]",
                "unknown runtime 'missing_runtime'",
            )]
        );
    }

    #[test]
    fn rejects_runtime_with_unknown_adapter() {
        let mut config = example_config();
        config
            .runtimes
            .get_mut(&RuntimeId("mock".to_string()))
            .expect("mock runtime")
            .adapter = "mystery".to_string();
        let diagnostics = validate_config(&config);
        assert_eq!(
            diagnostics,
            vec![ConfigDiagnostic::error(
                "runtimes.mock.adapter",
                "unknown runtime adapter 'mystery'",
            )]
        );
    }

    #[test]
    fn rejects_runtime_initial_resident_referencing_unknown_model() {
        let mut config = example_config();
        config
            .runtimes
            .get_mut(&RuntimeId("mock".to_string()))
            .expect("mock runtime")
            .initial_residents
            .push(RuntimeResidentConfig {
                model_id: ModelId("missing_model".to_string()),
                state: ResidencyState::HotGpu,
                vram_mb: None,
                ram_mb: None,
                kv_cache_mb: None,
            });
        let diagnostics = validate_config(&config);
        assert_eq!(
            diagnostics,
            vec![ConfigDiagnostic::error(
                "runtimes.mock.initial_residents[1].model_id",
                "unknown model 'missing_model'",
            )]
        );
    }

    #[test]
    fn rejects_empty_domain_roster() {
        let mut config = example_config();
        config
            .domains
            .get_mut(&DomainId("coding".to_string()))
            .expect("coding domain")
            .rosters
            .clear();
        let diagnostics = validate_config(&config);
        assert_eq!(
            diagnostics,
            vec![ConfigDiagnostic::error(
                "domains.coding.rosters",
                "domain must reference at least one residency group or set live_roster",
            )]
        );
    }

    #[test]
    fn accepts_domain_with_live_roster_and_no_static_rosters() {
        let config: AnemoiConfig = serde_yaml::from_str(
            r#"
domains:
  coding:
    live_roster: llama_swap
runtimes:
  llama_swap:
    adapter: llama_swap
    base_url: "http://localhost:8085"
    auth_token: "LOCAL"
"#,
        )
        .expect("config");
        let diagnostics = validate_config(&config);
        assert!(
            diagnostics.is_empty(),
            "live_roster domain must pass validation without static rosters: {:?}",
            diagnostics
        );
    }

    #[test]
    fn rejects_empty_residency_group_models() {
        let mut config = example_config();
        config
            .residency_groups
            .get_mut(&ResidencyGroupId("small_swarm".to_string()))
            .expect("small_swarm group")
            .models
            .clear();
        let diagnostics = validate_config(&config);
        assert_eq!(
            diagnostics,
            vec![ConfigDiagnostic::error(
                "residency_groups.small_swarm.models",
                "residency group must reference at least one model",
            )]
        );
    }

    #[test]
    fn reports_all_config_diagnostics_deterministically() {
        let mut config = example_config();
        config
            .domains
            .get_mut(&DomainId("coding".to_string()))
            .expect("coding domain")
            .rosters
            .push(ResidencyGroupId("missing_group".to_string()));
        config
            .residency_groups
            .get_mut(&ResidencyGroupId("small_swarm".to_string()))
            .expect("small_swarm group")
            .models
            .push(ModelId("missing_model".to_string()));
        config
            .models
            .get_mut(&ModelId("qwen9b".to_string()))
            .expect("qwen9b model")
            .supported_runtimes
            .push(RuntimeId("missing_runtime".to_string()));
        config
            .runtimes
            .get_mut(&RuntimeId("mock".to_string()))
            .expect("mock runtime")
            .adapter = "mystery".to_string();
        let first = validate_config(&config);
        let second = validate_config(&config);
        assert_eq!(first, second);
        assert_eq!(
            first,
            vec![
                ConfigDiagnostic::error(
                    "domains.coding.rosters[2]",
                    "unknown residency group 'missing_group'",
                ),
                ConfigDiagnostic::error(
                    "models.qwen9b.supported_runtimes[1]",
                    "unknown runtime 'missing_runtime'",
                ),
                ConfigDiagnostic::error(
                    "residency_groups.small_swarm.models[2]",
                    "unknown model 'missing_model'",
                ),
                ConfigDiagnostic::error(
                    "runtimes.mock.adapter",
                    "unknown runtime adapter 'mystery'",
                ),
            ]
        );
    }

    #[test]
    fn expand_env_vars_expands_set_variable() {
        std::env::set_var("ANEMOI_TEST_EXPAND_SET", "swift");
        assert_eq!(expand_env_vars("a=${ANEMOI_TEST_EXPAND_SET}"), "a=swift");
        assert_eq!(expand_env_vars("a=$ANEMOI_TEST_EXPAND_SET/b"), "a=swift/b");
        std::env::remove_var("ANEMOI_TEST_EXPAND_SET");
    }

    #[test]
    fn expand_env_vars_preserves_unset_variable_literally() {
        assert_eq!(
            expand_env_vars("t=${ANEMOI_TEST_EXPAND_MISSING}"),
            "t=${ANEMOI_TEST_EXPAND_MISSING}"
        );
        assert_eq!(
            expand_env_vars("t=$ANEMOI_TEST_EXPAND_MISSING"),
            "t=$ANEMOI_TEST_EXPAND_MISSING"
        );
    }

    #[test]
    fn expand_env_vars_resolves_default_and_alternate_forms() {
        std::env::set_var("ANEMOI_TEST_EXPAND_PRESENT", "yes");
        assert_eq!(
            expand_env_vars("${ANEMOI_TEST_EXPAND_PRESENT:-fallback}"),
            "yes"
        );
        assert_eq!(
            expand_env_vars("${ANEMOI_TEST_EXPAND_ABSENT:-fallback}"),
            "fallback"
        );
        assert_eq!(expand_env_vars("${ANEMOI_TEST_EXPAND_PRESENT:+alt}"), "alt");
        assert_eq!(expand_env_vars("${ANEMOI_TEST_EXPAND_ABSENT:+alt}"), "");
        std::env::remove_var("ANEMOI_TEST_EXPAND_PRESENT");
    }

    #[test]
    fn live_config_rejects_missing_required_runtime_url_when_no_default_exists() {
        let mut config = live_llama_swap_config();
        config
            .runtimes
            .get_mut(&RuntimeId("llama_swap".to_string()))
            .expect("llama_swap runtime")
            .base_url = None;
        let diagnostics = validate_config(&config);
        assert_eq!(
            diagnostics,
            vec![ConfigDiagnostic::error(
                "runtimes.llama_swap.base_url",
                "runtime adapter 'llama_swap' requires a base_url",
            )]
        );
    }
}
