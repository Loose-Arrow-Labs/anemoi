use anemoi_core::{ColocationConstraints, ModelId};
use serde::Deserialize;
use std::collections::{BTreeSet, HashMap};

use crate::RuntimeError;

/// A value in the llama-swap matrix `vars` block. llama-swap allows two kinds:
/// numeric budget values (`gpu: 24576` for total VRAM in MB) and string
/// aliases for model ids used in set expressions (`g31: gemma-4-31b-it`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum MatrixVar {
    Uint(u64),
    String(String),
}

/// Deserializes the `sets` field from either the llama-swap list format
/// (`[{name: ..., models: ...}]`) or the mapping format (`{name: expr}`).
fn deserialize_colocation_sets<'de, D>(deserializer: D) -> Result<Vec<ColocationSet>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::{MapAccess, SeqAccess, Visitor};

    struct ColocationSetsVisitor;

    impl<'de> Visitor<'de> for ColocationSetsVisitor {
        type Value = Vec<ColocationSet>;

        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            write!(
                f,
                "a sequence of {{name, models}} objects or a name-to-expression mapping"
            )
        }

        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
            let mut sets = Vec::new();
            while let Some(set) = seq.next_element::<ColocationSet>()? {
                sets.push(set);
            }
            Ok(sets)
        }

        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
            let mut sets = Vec::new();
            while let Some((name, models)) = map.next_entry::<String, String>()? {
                sets.push(ColocationSet { name, models });
            }
            Ok(sets)
        }
    }

    deserializer.deserialize_any(ColocationSetsVisitor)
}

/// The `matrix` block of a llama-swap YAML config. llama-swap does not expose
/// this over its API, so Anemoi reads the file directly (both run on the same
/// host). Anemoi reads the colocation sets to answer feasibility questions; it
/// does not re-implement llama-swap's matrix solver, so `vars` and
/// `evict_costs` are retained verbatim for callers that want the raw numbers.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
pub struct LlamaSwapMatrixConfig {
    /// Free-form variables: numeric budget values (`gpu: 24576`) or string
    /// aliases for model ids used in set expressions (`g31: gemma-4-31b-it`).
    #[serde(default)]
    pub vars: HashMap<String, MatrixVar>,
    /// Per-model cold-load cost estimates in milliseconds, keyed by model id
    /// (or a var alias when the config uses shorthand names).
    #[serde(default)]
    pub evict_costs: HashMap<String, u64>,
    /// Declared colocation sets. Supports both llama-swap formats: a sequence
    /// of `{name, models}` objects, or a mapping from set name to expression.
    #[serde(default, deserialize_with = "deserialize_colocation_sets")]
    pub sets: Vec<ColocationSet>,
}

/// One named colocation set. `models` is a llama-swap matrix DSL expression
/// over model ids using `&` (colocate / AND), `|` (alternative / OR), and
/// parentheses for grouping — e.g. `qwen9b & qwen35_a3b` or
/// `(qwen9b | qwen4b) & gemma`.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ColocationSet {
    pub name: String,
    pub models: String,
}

/// Wrapper that picks only the `matrix` block out of a full llama-swap config
/// file. Every other top-level key (models, groups, healthCheckTimeout, ...) is
/// ignored, so the same parser tolerates an arbitrary llama-swap config.
#[derive(Debug, Deserialize)]
struct LlamaSwapConfigFile {
    #[serde(default)]
    matrix: Option<LlamaSwapMatrixConfig>,
}

impl LlamaSwapMatrixConfig {
    /// Parses the `matrix` block out of the llama-swap YAML at `path`. Returns
    /// `Ok(None)` when the file parses but declares no `matrix` block.
    pub fn from_yaml_file(path: impl AsRef<std::path::Path>) -> Result<Option<Self>, RuntimeError> {
        let text = std::fs::read_to_string(path)
            .map_err(|error| RuntimeError::Config(error.to_string()))?;
        Self::from_yaml_str(&text)
    }

    /// Parses the `matrix` block out of a llama-swap YAML string. Returns
    /// `Ok(None)` when the document parses but declares no `matrix` block.
    pub fn from_yaml_str(text: &str) -> Result<Option<Self>, RuntimeError> {
        let file: LlamaSwapConfigFile =
            serde_yaml::from_str(text).map_err(|error| RuntimeError::Config(error.to_string()))?;
        Ok(file.matrix)
    }

    /// Value of a numeric var (e.g. `gpu: 24576` for total VRAM in MB).
    pub fn numeric_var(&self, key: &str) -> Option<u64> {
        match self.vars.get(key)? {
            MatrixVar::Uint(n) => Some(*n),
            _ => None,
        }
    }

    /// String-valued vars as an alias map, for expanding shorthand names in
    /// set expressions before model-ID comparison (e.g. `g31 → gemma-4-31b-it`).
    fn string_vars(&self) -> HashMap<&str, &str> {
        self.vars
            .iter()
            .filter_map(|(k, v)| match v {
                MatrixVar::String(s) => Some((k.as_str(), s.as_str())),
                _ => None,
            })
            .collect()
    }

    /// Whether `a` and `b` may be GPU-resident at the same time: `true` when
    /// some colocation set admits a loadout containing both. Models joined only
    /// by `|` are alternatives and do not colocate on that basis. Var aliases
    /// in set expressions (e.g. `g31`) are expanded via the `vars` map.
    pub fn can_colocate(&self, a: &ModelId, b: &ModelId) -> bool {
        let vars = self.string_vars();
        self.sets
            .iter()
            .flat_map(|set| set.loadouts_with_vars(&vars))
            .any(|loadout| loadout.contains(&a.0) && loadout.contains(&b.0))
    }

    /// Lower the parsed matrix into a runtime-agnostic [`ColocationConstraints`]
    /// the scheduling policy can consume off the observed snapshot. Each
    /// declared colocation set contributes one loadout per `|` branch of its DSL
    /// expression; the model ids within each loadout are sorted and
    /// deduplicated (they come from a `BTreeSet`).
    pub fn colocation_constraints(&self) -> ColocationConstraints {
        let vars = self.string_vars();
        ColocationConstraints {
            loadouts: self
                .sets
                .iter()
                .flat_map(|set| set.loadouts_with_vars(&vars))
                .map(|loadout| loadout.into_iter().map(ModelId).collect())
                .collect(),
        }
    }
}

impl ColocationSet {
    /// Co-resident loadouts implied by this set's `models` DSL expression,
    /// with var aliases expanded via `vars`. Each returned group is a set of
    /// model ids that may be resident together. An all-`&` expression yields
    /// one group; `|` branches yield one group per alternative. A malformed
    /// expression yields no groups.
    ///
    /// Pass an empty map when no alias expansion is needed.
    fn loadouts_with_vars(&self, vars: &HashMap<&str, &str>) -> Vec<BTreeSet<String>> {
        let tokens = tokenize_colocation_expr(&self.models);
        let mut pos = 0;
        let loadouts = parse_or(&tokens, &mut pos, vars);
        dedup_loadouts(loadouts)
    }
}

#[derive(Debug, PartialEq, Eq)]
enum MatrixToken {
    Ident(String),
    And,
    Or,
    Open,
    Close,
}

fn tokenize_colocation_expr(expr: &str) -> Vec<MatrixToken> {
    let mut tokens = Vec::new();
    let mut ident = String::new();
    for ch in expr.chars() {
        let operator = match ch {
            '&' => Some(MatrixToken::And),
            '|' => Some(MatrixToken::Or),
            '(' => Some(MatrixToken::Open),
            ')' => Some(MatrixToken::Close),
            _ => None,
        };
        match operator {
            Some(token) => {
                if !ident.is_empty() {
                    tokens.push(MatrixToken::Ident(std::mem::take(&mut ident)));
                }
                tokens.push(token);
            }
            None if ch.is_whitespace() => {
                if !ident.is_empty() {
                    tokens.push(MatrixToken::Ident(std::mem::take(&mut ident)));
                }
            }
            None => ident.push(ch),
        }
    }
    if !ident.is_empty() {
        tokens.push(MatrixToken::Ident(ident));
    }
    tokens
}

fn parse_or(
    tokens: &[MatrixToken],
    pos: &mut usize,
    vars: &HashMap<&str, &str>,
) -> Vec<BTreeSet<String>> {
    let mut loadouts = parse_and(tokens, pos, vars);
    while matches!(tokens.get(*pos), Some(MatrixToken::Or)) {
        *pos += 1;
        loadouts.extend(parse_and(tokens, pos, vars));
    }
    loadouts
}

fn parse_and(
    tokens: &[MatrixToken],
    pos: &mut usize,
    vars: &HashMap<&str, &str>,
) -> Vec<BTreeSet<String>> {
    let mut loadouts = parse_factor(tokens, pos, vars);
    while matches!(tokens.get(*pos), Some(MatrixToken::And)) {
        *pos += 1;
        let rhs = parse_factor(tokens, pos, vars);
        loadouts = cross_union(&loadouts, &rhs);
    }
    loadouts
}

fn parse_factor(
    tokens: &[MatrixToken],
    pos: &mut usize,
    vars: &HashMap<&str, &str>,
) -> Vec<BTreeSet<String>> {
    match tokens.get(*pos) {
        Some(MatrixToken::Ident(id)) => {
            *pos += 1;
            let resolved = vars.get(id.as_str()).copied().unwrap_or(id.as_str());
            vec![BTreeSet::from([resolved.to_string()])]
        }
        Some(MatrixToken::Open) => {
            *pos += 1;
            let inner = parse_or(tokens, pos, vars);
            if matches!(tokens.get(*pos), Some(MatrixToken::Close)) {
                *pos += 1;
            }
            inner
        }
        _ => Vec::new(),
    }
}

/// Cross product of two loadout lists, unioning each pair (the `&` operator).
/// An empty side means that operand had no parseable models; the other side is
/// returned unchanged so a trailing or malformed operand cannot erase models.
fn cross_union(lhs: &[BTreeSet<String>], rhs: &[BTreeSet<String>]) -> Vec<BTreeSet<String>> {
    if lhs.is_empty() {
        return rhs.to_vec();
    }
    if rhs.is_empty() {
        return lhs.to_vec();
    }
    let mut out = Vec::new();
    for left in lhs {
        for right in rhs {
            out.push(left.union(right).cloned().collect());
        }
    }
    out
}

fn dedup_loadouts(loadouts: Vec<BTreeSet<String>>) -> Vec<BTreeSet<String>> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for loadout in loadouts {
        if seen.insert(loadout.clone()) {
            out.push(loadout);
        }
    }
    out
}
