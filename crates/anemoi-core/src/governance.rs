//! Operator-managed governance overrides layered over the baseline config.
//!
//! Runtime catalogs (llama-swap today; ComfyUI/Pipecat later) are **read-only
//! discovery input**: the dashboard never edits a runtime's own config. Instead,
//! operators curate Anemoi-owned governance — which models belong to which
//! rosters, the roster flags, domain→roster assignments, and per-model profile
//! metadata that the runtime catalog does not provide. Those edits are captured
//! here as [`GovernanceOverrides`] and merged onto the baseline [`AnemoiConfig`]
//! by [`apply_overrides`] to produce the *effective* config the scheduler
//! consumes. Overrides are persisted separately from the baseline YAML, so the
//! hand-authored config stays the reviewable source of truth and operator edits
//! layer on top (the "hybrid" persistence model).
//!
//! Precedence: an operator override always wins over inferred/synthesized or
//! baseline-config metadata for the same field. Provenance is surfaced via
//! [`MetadataSource`] so every effective value is attributable in the dashboard.

use crate::{
    AnemoiConfig, DomainId, ModelId, ModelProfileConfig, ResidencyGroupConfig, ResidencyGroupId,
    RuntimeId,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Where an effective metadata value came from, surfaced in the model catalog so
/// operators can see and trust each field's provenance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetadataSource {
    /// Reported directly by the runtime catalog (e.g. llama-swap `/v1/models`).
    Runtime,
    /// Inferred by Anemoi from the model id (family prefix, `NNb` size token).
    Inferred,
    /// Set explicitly by an operator; takes precedence over all of the above.
    OperatorOverride,
    /// Declared in the baseline Anemoi YAML config.
    Config,
}

/// An operator-editable residency group (roster). Mirrors
/// [`ResidencyGroupConfig`] so it round-trips into the effective config without
/// inventing a parallel model the policy cannot consume.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RosterOverride {
    #[serde(default)]
    pub purpose: Vec<String>,
    #[serde(default)]
    pub models: Vec<ModelId>,
    #[serde(default)]
    pub keep_hot: bool,
    #[serde(default)]
    pub allow_background_load: bool,
    #[serde(default)]
    pub pinned: bool,
}

impl RosterOverride {
    pub fn into_config(self) -> ResidencyGroupConfig {
        ResidencyGroupConfig {
            purpose: self.purpose,
            models: self.models,
            keep_hot: self.keep_hot,
            allow_background_load: self.allow_background_load,
            pinned: self.pinned,
        }
    }

    pub fn from_config(config: &ResidencyGroupConfig) -> Self {
        Self {
            purpose: config.purpose.clone(),
            models: config.models.clone(),
            keep_hot: config.keep_hot,
            allow_background_load: config.allow_background_load,
            pinned: config.pinned,
        }
    }
}

/// Operator-set profile fields that take precedence over inferred or
/// baseline-config metadata. Every field is optional: only the fields an
/// operator actually sets override; unset fields fall through to the underlying
/// source.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelProfileOverride {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameter_class: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vram_required_mb: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ram_required_mb: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cold_load_estimate_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_streaming: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supported_runtimes: Option<Vec<RuntimeId>>,
}

impl ModelProfileOverride {
    pub fn is_empty(&self) -> bool {
        self.family.is_none()
            && self.parameter_class.is_none()
            && self.context_window.is_none()
            && self.vram_required_mb.is_none()
            && self.ram_required_mb.is_none()
            && self.cold_load_estimate_ms.is_none()
            && self.supports_streaming.is_none()
            && self.supported_runtimes.is_none()
    }
}

/// Anemoi-owned governance overrides, persisted separately from the baseline
/// YAML. Effective config = baseline + these (see [`apply_overrides`]).
///
/// `BTreeMap`/`BTreeSet` keep serialization deterministic so the persisted
/// overrides file and its audit diffs stay stable.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GovernanceOverrides {
    /// Residency groups created or updated by an operator, keyed by id. An id
    /// that also exists in the baseline replaces it; a new id adds a group.
    #[serde(default)]
    pub residency_groups: BTreeMap<ResidencyGroupId, RosterOverride>,
    /// Baseline residency-group ids the operator removed. Removal of a group
    /// still referenced by a domain is rejected at the API layer, never here.
    #[serde(default)]
    pub removed_residency_groups: BTreeSet<ResidencyGroupId>,
    /// Domain→roster assignments that replace the baseline domain's `rosters`.
    #[serde(default)]
    pub domain_rosters: BTreeMap<DomainId, Vec<ResidencyGroupId>>,
    /// Per-model operator profile overrides.
    #[serde(default)]
    pub model_profiles: BTreeMap<ModelId, ModelProfileOverride>,
}

impl GovernanceOverrides {
    pub fn is_empty(&self) -> bool {
        self.residency_groups.is_empty()
            && self.removed_residency_groups.is_empty()
            && self.domain_rosters.is_empty()
            && self.model_profiles.is_empty()
    }
}

/// Merge operator [`GovernanceOverrides`] onto a baseline [`AnemoiConfig`] to
/// produce the effective config the scheduler consumes. Pure and side-effect
/// free; the baseline is never mutated.
///
/// Merge rules:
/// - Residency groups in `residency_groups` insert-or-replace by id.
/// - Ids in `removed_residency_groups` are dropped from the effective config.
/// - `domain_rosters` replaces a domain's `rosters` list (domains absent from
///   the baseline are ignored — domains are not operator-creatable in v1).
/// - `model_profiles` insert-or-merge per field onto the baseline model profile,
///   synthesizing a base profile from the model id when the baseline has none.
pub fn apply_overrides(baseline: &AnemoiConfig, overrides: &GovernanceOverrides) -> AnemoiConfig {
    let mut effective = baseline.clone();

    for (id, roster) in &overrides.residency_groups {
        effective
            .residency_groups
            .insert(id.clone(), roster.clone().into_config());
    }

    for id in &overrides.removed_residency_groups {
        effective.residency_groups.remove(id);
    }

    for (domain_id, rosters) in &overrides.domain_rosters {
        if let Some(domain) = effective.domains.get_mut(domain_id) {
            domain.rosters = rosters.clone();
        }
    }

    for (model_id, profile_override) in &overrides.model_profiles {
        let base = effective.models.get(model_id);
        let merged = merge_profile(model_id, base, profile_override);
        effective.models.insert(model_id.clone(), merged);
    }

    effective
}

/// Merge a [`ModelProfileOverride`] onto an optional baseline profile, filling
/// any field the operator did not set from the baseline (or, lacking a baseline,
/// from values inferred from the model id). Required fields (`family`,
/// `parameter_class`) always resolve to a concrete value.
pub fn merge_profile(
    model_id: &ModelId,
    base: Option<&ModelProfileConfig>,
    profile_override: &ModelProfileOverride,
) -> ModelProfileConfig {
    let (inferred_family, inferred_class) = infer_family_and_class(&model_id.0);
    ModelProfileConfig {
        family: profile_override
            .family
            .clone()
            .or_else(|| base.map(|b| b.family.clone()))
            .unwrap_or(inferred_family),
        parameter_class: profile_override
            .parameter_class
            .clone()
            .or_else(|| base.map(|b| b.parameter_class.clone()))
            .unwrap_or(inferred_class),
        context_window: profile_override
            .context_window
            .or_else(|| base.and_then(|b| b.context_window)),
        vram_required_mb: profile_override
            .vram_required_mb
            .or_else(|| base.and_then(|b| b.vram_required_mb)),
        ram_required_mb: profile_override
            .ram_required_mb
            .or_else(|| base.and_then(|b| b.ram_required_mb)),
        cold_load_estimate_ms: profile_override
            .cold_load_estimate_ms
            .or_else(|| base.and_then(|b| b.cold_load_estimate_ms)),
        supported_runtimes: profile_override
            .supported_runtimes
            .clone()
            .or_else(|| base.map(|b| b.supported_runtimes.clone()))
            .unwrap_or_default(),
        supports_streaming: profile_override
            .supports_streaming
            .or_else(|| base.and_then(|b| b.supports_streaming)),
    }
}

/// Infer `(family, parameter_class)` from a model id: the family is the leading
/// run of alphabetic characters (e.g. `qwen`, `gemma`), and the parameter class
/// is the first `NNb` token (e.g. `9b`, `35b`). Unknown when no such token
/// exists. Mirrors the live-roster synthesis the scheduler already performs.
pub fn infer_family_and_class(model_id: &str) -> (String, String) {
    let family: String = model_id.chars().take_while(|c| c.is_alphabetic()).collect();
    let family = if family.is_empty() {
        "unknown".to_string()
    } else {
        family
    };

    let bytes = model_id.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            if i < bytes.len() && (bytes[i] == b'b' || bytes[i] == b'B') {
                return (family, format!("{}b", &model_id[start..i]));
            }
        } else {
            i += 1;
        }
    }
    (family, "unknown".to_string())
}

/// Numeric billions of parameters parsed from a `parameter_class` like `"35b"`,
/// or `None` when the class is unknown/non-numeric. Used for escalation checks.
pub fn parameter_billions(parameter_class: &str) -> Option<u32> {
    let digits: String = parameter_class
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    if digits.is_empty() {
        None
    } else {
        digits.parse::<u32>().ok()
    }
}

/// A non-fatal governance warning surfaced by [`validate_governance`]. These are
/// advisory: an effective config can be served with warnings, but the dashboard
/// should show them loudly so operators do not silently strand a domain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GovernanceWarning {
    /// Stable machine code, e.g. `roster.empty`, `domain.no_escalation`.
    pub code: String,
    /// Where the warning applies (roster id, domain id, model id).
    pub target: String,
    /// Human-readable explanation.
    pub detail: String,
}

/// The parameter-class threshold (inclusive) below which a static-roster domain
/// is considered unable to escalate. A domain whose largest roster model is at
/// most this size triggers `domain.no_escalation` — the exact failure mode from
/// issue #137/#138 where `coding` was stuck on 9B.
pub const ESCALATION_FLOOR_BILLIONS: u32 = 9;

/// Validate an *effective* config for governance problems that policy cannot
/// itself surface as hard errors: empty rosters, domains that produce no
/// candidates, roster references to undefined groups, roster models lacking a
/// profile, and static-roster domains with no escalation candidate above the 9B
/// floor. Returns advisory warnings, never mutating anything.
pub fn validate_governance(effective: &AnemoiConfig) -> Vec<GovernanceWarning> {
    let mut warnings = Vec::new();

    for (id, group) in &effective.residency_groups {
        if group.models.is_empty() {
            warnings.push(GovernanceWarning {
                code: "roster.empty".to_string(),
                target: id.to_string(),
                detail: format!("residency group {id} has no models; it can produce no candidates"),
            });
        }
        for model in &group.models {
            if !effective.models.contains_key(model) {
                warnings.push(GovernanceWarning {
                    code: "model.missing_profile".to_string(),
                    target: model.to_string(),
                    detail: format!(
                        "model {model} in residency group {id} has no profile; policy will reject it as a candidate"
                    ),
                });
            }
        }
    }

    for (domain_id, domain) in &effective.domains {
        if domain.live_roster.is_some() {
            // Live-roster domains draw candidates from the runtime snapshot, so
            // static-roster checks do not apply.
            continue;
        }

        if domain.rosters.is_empty() {
            warnings.push(GovernanceWarning {
                code: "domain.no_roster".to_string(),
                target: domain_id.to_string(),
                detail: format!(
                    "domain {domain_id} has no rosters and no live_roster; it can produce no candidates"
                ),
            });
            continue;
        }

        let mut largest: Option<u32> = None;
        let mut saw_known_class = false;
        for roster_id in &domain.rosters {
            let Some(group) = effective.residency_groups.get(roster_id) else {
                warnings.push(GovernanceWarning {
                    code: "domain.unknown_roster".to_string(),
                    target: domain_id.to_string(),
                    detail: format!(
                        "domain {domain_id} references residency group {roster_id}, which is not defined"
                    ),
                });
                continue;
            };
            for model in &group.models {
                if let Some(profile) = effective.models.get(model) {
                    if let Some(billions) = parameter_billions(&profile.parameter_class) {
                        saw_known_class = true;
                        largest = Some(largest.map_or(billions, |cur| cur.max(billions)));
                    }
                }
            }
        }

        // Only warn about escalation when we actually know some sizes; a domain
        // built entirely from unknown-size models is a separate metadata gap
        // already covered by `model.missing_profile`.
        if saw_known_class && largest.is_some_and(|b| b <= ESCALATION_FLOOR_BILLIONS) {
            warnings.push(GovernanceWarning {
                code: "domain.no_escalation".to_string(),
                target: domain_id.to_string(),
                detail: format!(
                    "domain {domain_id} cannot escalate beyond {ESCALATION_FLOOR_BILLIONS}B: its largest roster model is {}B; add a larger model to enable escalation",
                    largest.unwrap_or(0)
                ),
            });
        }
    }

    warnings
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DomainConfig;

    fn m(id: &str) -> ModelId {
        ModelId(id.to_string())
    }
    fn g(id: &str) -> ResidencyGroupId {
        ResidencyGroupId(id.to_string())
    }
    fn d(id: &str) -> DomainId {
        DomainId(id.to_string())
    }

    fn baseline() -> AnemoiConfig {
        serde_yaml::from_str(
            r#"
domains:
  coding:
    rosters: [fast]
residency_groups:
  fast:
    models: [qwen9b, qwen4b]
    allow_background_load: true
  big:
    models: [qwen35b]
models:
  qwen9b: {family: qwen, parameter_class: 9b, supported_runtimes: [llama_swap]}
  qwen4b: {family: qwen, parameter_class: 4b, supported_runtimes: [llama_swap]}
  qwen35b: {family: qwen, parameter_class: 35b, supported_runtimes: [llama_swap]}
runtimes:
  llama_swap: {adapter: llama_swap}
"#,
        )
        .expect("baseline config")
    }

    #[test]
    fn empty_overrides_yield_baseline() {
        let base = baseline();
        let effective = apply_overrides(&base, &GovernanceOverrides::default());
        assert_eq!(effective, base);
    }

    #[test]
    fn create_roster_then_assign_to_domain_changes_effective_candidates() {
        let base = baseline();
        let mut overrides = GovernanceOverrides::default();
        // Operator creates a roster from a catalog model and assigns it to coding.
        overrides.residency_groups.insert(
            g("escalation"),
            RosterOverride {
                models: vec![m("qwen35b")],
                allow_background_load: true,
                ..Default::default()
            },
        );
        overrides
            .domain_rosters
            .insert(d("coding"), vec![g("fast"), g("escalation")]);

        let effective = apply_overrides(&base, &overrides);
        assert_eq!(
            effective.domains[&d("coding")].rosters,
            vec![g("fast"), g("escalation")]
        );
        assert!(effective.residency_groups.contains_key(&g("escalation")));
        // The escalation candidate is now reachable from the coding domain.
        assert!(validate_governance(&effective)
            .iter()
            .all(|w| w.code != "domain.no_escalation"));
    }

    #[test]
    fn add_and_remove_model_from_roster() {
        let base = baseline();
        let mut overrides = GovernanceOverrides::default();
        overrides.residency_groups.insert(
            g("fast"),
            RosterOverride {
                models: vec![m("qwen9b")], // removed qwen4b
                allow_background_load: true,
                ..Default::default()
            },
        );
        let effective = apply_overrides(&base, &overrides);
        assert_eq!(
            effective.residency_groups[&g("fast")].models,
            vec![m("qwen9b")]
        );
    }

    #[test]
    fn removed_residency_group_drops_from_effective() {
        let base = baseline();
        let mut overrides = GovernanceOverrides::default();
        overrides.removed_residency_groups.insert(g("big"));
        let effective = apply_overrides(&base, &overrides);
        assert!(!effective.residency_groups.contains_key(&g("big")));
    }

    #[test]
    fn operator_override_takes_precedence_over_inferred_profile_metadata() {
        let base = baseline();
        let mut overrides = GovernanceOverrides::default();
        // qwen9b would infer parameter_class "9b"; operator overrides context.
        overrides.model_profiles.insert(
            m("qwen9b"),
            ModelProfileOverride {
                context_window: Some(262144),
                parameter_class: Some("12b".to_string()),
                ..Default::default()
            },
        );
        let effective = apply_overrides(&base, &overrides);
        let profile = &effective.models[&m("qwen9b")];
        assert_eq!(profile.context_window, Some(262144));
        assert_eq!(profile.parameter_class, "12b");
        // Untouched fields fall through to the baseline.
        assert_eq!(profile.family, "qwen");
    }

    #[test]
    fn profile_override_synthesizes_base_for_catalog_only_model() {
        let base = baseline();
        let mut overrides = GovernanceOverrides::default();
        // A model present in a runtime catalog but absent from baseline models.
        overrides.model_profiles.insert(
            m("gemma-4-31b-it"),
            ModelProfileOverride {
                supports_streaming: Some(true),
                ..Default::default()
            },
        );
        let effective = apply_overrides(&base, &overrides);
        let profile = &effective.models[&m("gemma-4-31b-it")];
        assert_eq!(profile.family, "gemma"); // inferred
        assert_eq!(profile.parameter_class, "31b"); // inferred
        assert_eq!(profile.supports_streaming, Some(true)); // operator-set
    }

    #[test]
    fn validate_flags_empty_roster() {
        let base = baseline();
        let mut overrides = GovernanceOverrides::default();
        overrides
            .residency_groups
            .insert(g("empty"), RosterOverride::default());
        let effective = apply_overrides(&base, &overrides);
        assert!(validate_governance(&effective)
            .iter()
            .any(|w| w.code == "roster.empty" && w.target == "empty"));
    }

    #[test]
    fn validate_flags_domain_stuck_below_escalation_floor() {
        // coding -> [fast] where fast tops out at 9b: no escalation candidate.
        let base = baseline();
        let warnings = validate_governance(&base);
        assert!(
            warnings
                .iter()
                .any(|w| w.code == "domain.no_escalation" && w.target == "coding"),
            "coding domain stuck at 9B must warn, got {warnings:?}"
        );
    }

    #[test]
    fn infer_family_and_class_handles_common_ids() {
        assert_eq!(
            infer_family_and_class("qwen3.6-35b-a3b-mtp"),
            ("qwen".to_string(), "35b".to_string())
        );
        assert_eq!(
            infer_family_and_class("gemma-4-e2b-it"),
            ("gemma".to_string(), "2b".to_string())
        );
        assert_eq!(
            infer_family_and_class("minimax-256k"),
            ("minimax".to_string(), "unknown".to_string())
        );
    }

    #[test]
    fn baseline_domain_config_helper_compiles() {
        // Touch DomainConfig so the import is exercised even if other tests change.
        let domain = DomainConfig {
            rosters: vec![g("fast")],
            live_roster: None,
        };
        assert_eq!(domain.rosters, vec![g("fast")]);
    }
}
