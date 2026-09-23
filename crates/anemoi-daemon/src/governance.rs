//! Runtime holder for operator governance edits.
//!
//! Wraps the immutable baseline [`AnemoiConfig`] (from YAML) together with the
//! Anemoi-owned [`GovernanceOverrides`] and a derived *effective* bundle (the
//! merged config plus a [`Scheduler`] built from it). Roster edits mutate the
//! overrides, rebuild the bundle, and swap it in atomically, so the very next
//! `/decide` runs against the edited rosters through the real scheduler path.
//! Overrides persist to an Anemoi-owned JSON file (never the runtime's config).

use anemoi_core::{apply_overrides, validate_governance, AnemoiConfig, GovernanceOverrides};
use anemoi_policy::Scheduler;
use std::path::{Path, PathBuf};
use std::sync::{Arc, PoisonError, RwLock};

/// The effective (baseline + overrides) config and a scheduler built from it.
/// Swapped wholesale on each edit; readers clone the `Arc` and never block
/// writers for longer than a pointer read.
pub struct EffectiveBundle {
    pub config: AnemoiConfig,
    pub scheduler: Scheduler,
}

/// Reasons an operator governance edit is rejected without changing any state.
#[derive(Debug, thiserror::Error)]
pub enum GovernanceError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("invalid: {0}")]
    Invalid(String),
    #[error("failed to persist governance overrides: {0}")]
    Persist(String),
}

/// Thread-safe holder for the baseline config, operator overrides, and the
/// derived effective bundle. Cloned cheaply via the surrounding `Arc`.
pub struct Governance {
    baseline: AnemoiConfig,
    overrides: RwLock<GovernanceOverrides>,
    effective: RwLock<Arc<EffectiveBundle>>,
    overrides_path: Option<PathBuf>,
}

impl Governance {
    /// Build a holder from a baseline and an initial set of overrides.
    pub fn new(
        baseline: AnemoiConfig,
        overrides: GovernanceOverrides,
        overrides_path: Option<PathBuf>,
    ) -> Self {
        let effective = Arc::new(build_bundle(&baseline, &overrides));
        Self {
            baseline,
            overrides: RwLock::new(overrides),
            effective: RwLock::new(effective),
            overrides_path,
        }
    }

    /// Build a holder, loading persisted overrides from `overrides_path` when it
    /// exists. A missing or unreadable file starts from empty overrides (the
    /// baseline alone); a malformed file is treated as a hard start-up error.
    pub fn load(
        baseline: AnemoiConfig,
        overrides_path: Option<PathBuf>,
    ) -> Result<Self, GovernanceError> {
        let overrides = match &overrides_path {
            Some(path) if path.exists() => {
                let text = std::fs::read_to_string(path)
                    .map_err(|e| GovernanceError::Persist(e.to_string()))?;
                serde_json::from_str(&text).map_err(|e| {
                    GovernanceError::Invalid(format!("overrides file {}: {e}", path.display()))
                })?
            }
            _ => GovernanceOverrides::default(),
        };
        Ok(Self::new(baseline, overrides, overrides_path))
    }

    /// The current effective bundle. Cheap: clones an `Arc`.
    pub fn effective(&self) -> Arc<EffectiveBundle> {
        self.effective
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// A snapshot of the current overrides.
    pub fn overrides_snapshot(&self) -> GovernanceOverrides {
        self.overrides
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The immutable baseline config (pre-overrides).
    pub fn baseline(&self) -> &AnemoiConfig {
        &self.baseline
    }

    /// Advisory governance warnings for the current effective config.
    pub fn warnings(&self) -> Vec<anemoi_core::GovernanceWarning> {
        validate_governance(&self.effective().config)
    }

    /// Apply an edit to the overrides transactionally: the closure mutates a
    /// *copy* and may reject the edit by returning `Err` (nothing changes). On
    /// success the overrides are persisted first, then the in-memory overrides
    /// and effective bundle are swapped in. A persistence failure aborts the
    /// edit with no in-memory change, so disk and memory never diverge.
    ///
    /// `f` receives the working overrides and the baseline config so it can
    /// validate against the effective shape (e.g. reject removing a roster a
    /// domain still references).
    pub fn mutate<F>(&self, f: F) -> Result<(), GovernanceError>
    where
        F: FnOnce(&mut GovernanceOverrides, &AnemoiConfig) -> Result<(), GovernanceError>,
    {
        let mut overrides = self
            .overrides
            .write()
            .unwrap_or_else(PoisonError::into_inner);
        let mut candidate = overrides.clone();
        f(&mut candidate, &self.baseline)?;

        if let Some(path) = &self.overrides_path {
            write_overrides_atomic(path, &candidate)
                .map_err(|e| GovernanceError::Persist(e.to_string()))?;
        }

        let bundle = Arc::new(build_bundle(&self.baseline, &candidate));
        *overrides = candidate;
        *self
            .effective
            .write()
            .unwrap_or_else(PoisonError::into_inner) = bundle;
        Ok(())
    }
}

fn build_bundle(baseline: &AnemoiConfig, overrides: &GovernanceOverrides) -> EffectiveBundle {
    let config = apply_overrides(baseline, overrides);
    let scheduler = Scheduler::new(config.clone());
    EffectiveBundle { config, scheduler }
}

/// Write overrides as pretty JSON via a temp file + rename so a crash mid-write
/// never leaves a truncated overrides file.
fn write_overrides_atomic(path: &Path, overrides: &GovernanceOverrides) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let json = serde_json::to_string_pretty(overrides)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json.as_bytes())?;
    // std::fs::rename replaces an existing destination on both Unix and Windows.
    match std::fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(_) if path.exists() => {
            // Fallback for platforms/filesystems that refuse replace-on-rename.
            std::fs::remove_file(path)?;
            std::fs::rename(&tmp, path)
        }
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anemoi_core::{ModelId, ResidencyGroupId, RosterOverride};

    fn baseline() -> AnemoiConfig {
        AnemoiConfig::from_yaml_str(
            r#"
domains:
  coding:
    rosters: [fast]
residency_groups:
  fast:
    models: [qwen9b]
    allow_background_load: true
models:
  qwen9b: {family: qwen, parameter_class: 9b, supported_runtimes: [llama_swap]}
  qwen35b: {family: qwen, parameter_class: 35b, supported_runtimes: [llama_swap]}
runtimes:
  llama_swap: {adapter: mock}
"#,
        )
        .expect("baseline")
    }

    #[test]
    fn mutate_rebuilds_effective_bundle_immediately() {
        let gov = Governance::new(baseline(), GovernanceOverrides::default(), None);
        assert_eq!(
            gov.effective().config.residency_groups[&ResidencyGroupId("fast".into())].models,
            vec![ModelId("qwen9b".into())]
        );

        gov.mutate(|ov, _baseline| {
            ov.residency_groups.insert(
                ResidencyGroupId("fast".into()),
                RosterOverride {
                    models: vec![ModelId("qwen9b".into()), ModelId("qwen35b".into())],
                    allow_background_load: true,
                    ..Default::default()
                },
            );
            Ok(())
        })
        .expect("mutate");

        // The effective bundle reflects the edit right away.
        assert_eq!(
            gov.effective().config.residency_groups[&ResidencyGroupId("fast".into())].models,
            vec![ModelId("qwen9b".into()), ModelId("qwen35b".into())]
        );
    }

    #[test]
    fn rejected_mutation_leaves_state_unchanged() {
        let gov = Governance::new(baseline(), GovernanceOverrides::default(), None);
        let before = gov.overrides_snapshot();
        let result = gov.mutate(|_ov, _baseline| Err(GovernanceError::Conflict("nope".into())));
        assert!(result.is_err());
        assert_eq!(gov.overrides_snapshot(), before);
    }

    #[test]
    fn overrides_round_trip_through_disk() {
        let dir = std::env::temp_dir().join(format!("anemoi-gov-test-{}", std::process::id()));
        let path = dir.join("overrides.json");
        let _ = std::fs::remove_dir_all(&dir);

        {
            let gov = Governance::load(baseline(), Some(path.clone())).expect("load");
            gov.mutate(|ov, _baseline| {
                ov.domain_rosters.insert(
                    anemoi_core::DomainId("coding".into()),
                    vec![ResidencyGroupId("fast".into())],
                );
                Ok(())
            })
            .expect("mutate");
        }

        // A fresh holder reloads the persisted overrides.
        let reloaded = Governance::load(baseline(), Some(path.clone())).expect("reload");
        assert!(reloaded
            .overrides_snapshot()
            .domain_rosters
            .contains_key(&anemoi_core::DomainId("coding".into())));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
