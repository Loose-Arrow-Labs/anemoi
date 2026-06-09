use anemoi_core::{
    ActiveExecution, ColocationConstraints, ExecutionRequest, ModelId, ModelResident,
    ResidencyState, RuntimeId, RuntimeMemorySnapshot, RuntimeSnapshot,
};
use async_trait::async_trait;
use chrono::Utc;
use futures::stream::{BoxStream, StreamExt};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};
use reqwest::Url;
use serde::Deserialize;
use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, PoisonError, RwLock};
use std::time::Duration;
use uuid::Uuid;

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
    pub id: Uuid,
    pub model_id: ModelId,
}

#[derive(Debug, Clone)]
pub struct ExecutionHandle {
    pub id: Uuid,
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

    async fn inspect(&self) -> Result<RuntimeSnapshot, RuntimeError>;

    async fn load_model(&self, model: &ModelId) -> Result<LoadHandle, RuntimeError>;

    async fn unload_model(&self, model: &ModelId) -> Result<(), RuntimeError>;

    async fn execute(&self, request: ExecutionRequest) -> Result<ExecutionHandle, RuntimeError>;
}

pub type DynRuntimeAdapter = Arc<dyn RuntimeAdapter>;

#[derive(Debug, Clone)]
pub struct MockRuntimeAdapter {
    id: RuntimeId,
    snapshot: Arc<RwLock<RuntimeSnapshot>>,
}

impl MockRuntimeAdapter {
    pub fn new(id: RuntimeId, residents: Vec<ModelResident>) -> Self {
        Self {
            id: id.clone(),
            snapshot: Arc::new(RwLock::new(RuntimeSnapshot {
                runtime_id: id,
                available: true,
                residents,
                configured_models: Vec::new(),
                memory: RuntimeMemorySnapshot::default(),
                active_requests: Vec::new(),
                colocation: None,
            })),
        }
    }

    pub fn with_memory(self, memory: RuntimeMemorySnapshot) -> Self {
        self.snapshot.write().expect("mock runtime lock").memory = memory;
        self
    }

    pub fn with_available(self, available: bool) -> Self {
        self.snapshot.write().expect("mock runtime lock").available = available;
        self
    }

    pub fn with_resident_state(self, model_id: ModelId, state: ResidencyState) -> Self {
        let mut snapshot = self.snapshot.write().expect("mock runtime lock");
        if let Some(resident) = snapshot
            .residents
            .iter_mut()
            .find(|resident| resident.model_id == model_id)
        {
            resident.state = state;
        } else {
            snapshot.residents.push(ModelResident {
                model_id,
                state,
                vram_mb: None,
                ram_mb: None,
                kv_cache_mb: None,
                loaded_since: None,
            });
        }
        drop(snapshot);
        self
    }
}

#[async_trait]
impl RuntimeAdapter for MockRuntimeAdapter {
    fn id(&self) -> RuntimeId {
        self.id.clone()
    }

    fn is_mock(&self) -> bool {
        true
    }

    async fn inspect(&self) -> Result<RuntimeSnapshot, RuntimeError> {
        Ok(self.snapshot.read().expect("mock runtime lock").clone())
    }

    async fn load_model(&self, model: &ModelId) -> Result<LoadHandle, RuntimeError> {
        let mut snapshot = self.snapshot.write().expect("mock runtime lock");
        if !snapshot
            .residents
            .iter()
            .any(|resident| resident.model_id == *model)
        {
            snapshot.residents.push(ModelResident {
                model_id: model.clone(),
                state: ResidencyState::Loading,
                vram_mb: None,
                ram_mb: None,
                kv_cache_mb: None,
                loaded_since: None,
            });
        }

        Ok(LoadHandle {
            id: Uuid::new_v4(),
            model_id: model.clone(),
        })
    }

    async fn unload_model(&self, model: &ModelId) -> Result<(), RuntimeError> {
        let mut snapshot = self.snapshot.write().expect("mock runtime lock");
        snapshot
            .residents
            .retain(|resident| resident.model_id != *model);
        Ok(())
    }

    async fn execute(&self, request: ExecutionRequest) -> Result<ExecutionHandle, RuntimeError> {
        let mut snapshot = self.snapshot.write().expect("mock runtime lock");
        snapshot.active_requests.push(ActiveExecution {
            request_id: request.request_id.clone(),
            model_id: request.model_id.clone(),
            started_at: Utc::now(),
        });

        Ok(ExecutionHandle {
            id: Uuid::new_v4(),
            request,
        })
    }
}

#[derive(Debug, Clone)]
pub struct OllamaAdapter {
    id: RuntimeId,
    base_url: Url,
    client: reqwest::Client,
}

impl OllamaAdapter {
    pub fn new(id: RuntimeId, base_url: &str) -> Result<Self, RuntimeError> {
        Ok(Self {
            id,
            base_url: Url::parse(base_url).map_err(|error| RuntimeError::Url(error.to_string()))?,
            client: reqwest::Client::new(),
        })
    }
}

#[derive(Debug, Deserialize)]
struct OllamaPsResponse {
    #[serde(default)]
    models: Vec<OllamaRunningModel>,
}

#[derive(Debug, Deserialize)]
struct OllamaRunningModel {
    name: String,
    #[serde(default)]
    size_vram: Option<u64>,
    #[serde(default)]
    size: Option<u64>,
}

#[async_trait]
impl RuntimeAdapter for OllamaAdapter {
    fn id(&self) -> RuntimeId {
        self.id.clone()
    }

    async fn inspect(&self) -> Result<RuntimeSnapshot, RuntimeError> {
        let url = self
            .base_url
            .join("/api/ps")
            .map_err(|error| RuntimeError::Url(error.to_string()))?;
        let http_response = self.client.get(url).send().await?;
        if !http_response.status().is_success() {
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

        let response: OllamaPsResponse = http_response.json().await?;
        let residents = response
            .models
            .into_iter()
            .map(|model| ModelResident {
                model_id: ModelId(model.name),
                state: ResidencyState::HotGpu,
                vram_mb: model.size_vram.map(bytes_to_mb),
                ram_mb: model.size.map(bytes_to_mb),
                kv_cache_mb: None,
                loaded_since: None,
            })
            .collect();

        Ok(RuntimeSnapshot {
            runtime_id: self.id.clone(),
            available: true,
            residents,
            configured_models: Vec::new(),
            memory: RuntimeMemorySnapshot::default(),
            active_requests: Vec::new(),
            colocation: None,
        })
    }

    /// Ollama loads a model into memory lazily on its first inference request,
    /// and weight downloads happen via an explicit `ollama pull` outside Anemoi's
    /// control plane. Anemoi decides residency; it does not mutate runtime
    /// infrastructure (AGENTS.md §2/§4), so it does not proactively load Ollama
    /// models. Returning `Unsupported` (rather than a fabricated `LoadHandle`)
    /// keeps staging honest: the worker observes that the load did not happen
    /// instead of marking the intent `Completed` for a load that never ran.
    async fn load_model(&self, _model: &ModelId) -> Result<LoadHandle, RuntimeError> {
        Err(RuntimeError::Unsupported("ollama load"))
    }

    async fn unload_model(&self, _model: &ModelId) -> Result<(), RuntimeError> {
        Err(RuntimeError::Unsupported("ollama unload"))
    }

    async fn execute(&self, request: ExecutionRequest) -> Result<ExecutionHandle, RuntimeError> {
        Ok(ExecutionHandle {
            id: Uuid::new_v4(),
            request,
        })
    }
}

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

/// The `matrix` block of a llama-swap YAML config. llama-swap does not expose
/// this over its API, so Anemoi reads the file directly (both run on the same
/// host). Anemoi reads the colocation sets to answer feasibility questions; it
/// does not re-implement llama-swap's matrix solver, so `vars` and
/// `evict_costs` are retained verbatim for callers that want the raw numbers.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
pub struct LlamaSwapMatrixConfig {
    /// Free-form numeric variables (e.g. `gpu: 24576` for total VRAM in MB).
    #[serde(default)]
    pub vars: HashMap<String, u64>,
    /// Per-model cold-load cost estimates in milliseconds, keyed by model id.
    #[serde(default)]
    pub evict_costs: HashMap<String, u64>,
    /// Declared colocation sets. Each set's `models` expression names the
    /// models that may be GPU-resident together.
    #[serde(default)]
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

    /// Whether `a` and `b` may be GPU-resident at the same time: `true` when
    /// some colocation set admits a loadout containing both. Models joined only
    /// by `|` are alternatives and do not colocate on that basis.
    pub fn can_colocate(&self, a: &ModelId, b: &ModelId) -> bool {
        self.sets
            .iter()
            .flat_map(|set| set.loadouts())
            .any(|loadout| loadout.contains(&a.0) && loadout.contains(&b.0))
    }

    /// Lower the parsed matrix into a runtime-agnostic [`ColocationConstraints`]
    /// the scheduling policy can consume off the observed snapshot. Each
    /// declared colocation set contributes one loadout per `|` branch of its DSL
    /// expression; the model ids within each loadout are sorted and
    /// deduplicated (they come from a `BTreeSet`).
    pub fn colocation_constraints(&self) -> ColocationConstraints {
        ColocationConstraints {
            loadouts: self
                .sets
                .iter()
                .flat_map(|set| set.loadouts())
                .map(|loadout| loadout.into_iter().map(ModelId).collect())
                .collect(),
        }
    }
}

impl ColocationSet {
    /// Co-resident loadouts implied by this set's `models` DSL expression: each
    /// returned group is a set of model ids that may be resident together. An
    /// all-`&` expression yields one group; `|` branches yield one group per
    /// alternative. A malformed expression yields no groups.
    fn loadouts(&self) -> Vec<BTreeSet<String>> {
        parse_colocation_expr(&self.models)
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

/// Parses a matrix colocation DSL expression into its co-resident loadouts.
/// Grammar: `expr := term ('|' term)*`, `term := factor ('&' factor)*`,
/// `factor := IDENT | '(' expr ')'`. `&` unions loadouts (co-resident); `|`
/// concatenates them (alternatives). Parsing is lenient: unbalanced or empty
/// input yields no loadouts rather than erroring.
fn parse_colocation_expr(expr: &str) -> Vec<BTreeSet<String>> {
    let tokens = tokenize_colocation_expr(expr);
    let mut pos = 0;
    let loadouts = parse_or(&tokens, &mut pos);
    dedup_loadouts(loadouts)
}

fn parse_or(tokens: &[MatrixToken], pos: &mut usize) -> Vec<BTreeSet<String>> {
    let mut loadouts = parse_and(tokens, pos);
    while matches!(tokens.get(*pos), Some(MatrixToken::Or)) {
        *pos += 1;
        loadouts.extend(parse_and(tokens, pos));
    }
    loadouts
}

fn parse_and(tokens: &[MatrixToken], pos: &mut usize) -> Vec<BTreeSet<String>> {
    let mut loadouts = parse_factor(tokens, pos);
    while matches!(tokens.get(*pos), Some(MatrixToken::And)) {
        *pos += 1;
        let rhs = parse_factor(tokens, pos);
        loadouts = cross_union(&loadouts, &rhs);
    }
    loadouts
}

fn parse_factor(tokens: &[MatrixToken], pos: &mut usize) -> Vec<BTreeSet<String>> {
    match tokens.get(*pos) {
        Some(MatrixToken::Ident(id)) => {
            *pos += 1;
            vec![BTreeSet::from([id.clone()])]
        }
        Some(MatrixToken::Open) => {
            *pos += 1;
            let inner = parse_or(tokens, pos);
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

#[derive(Debug, Deserialize)]
struct LlamaSwapModelsResponse {
    #[serde(default)]
    data: Vec<LlamaSwapModel>,
}

#[derive(Debug, Deserialize)]
struct LlamaSwapModel {
    id: String,
}

/// Process state of a model as reported over llama-swap's `/api/events` SSE
/// stream. Mirrors the `state` field of each `modelStatus` entry; the lifecycle
/// is `stopped` → `starting` → `ready` → `stopping` → `shutdown`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LlamaSwapModelState {
    Stopped,
    Starting,
    Ready,
    Stopping,
    Shutdown,
}

impl LlamaSwapModelState {
    fn from_wire(raw: &str) -> Option<Self> {
        match raw {
            "stopped" => Some(Self::Stopped),
            "starting" => Some(Self::Starting),
            "ready" => Some(Self::Ready),
            "stopping" => Some(Self::Stopping),
            "shutdown" => Some(Self::Shutdown),
            _ => None,
        }
    }

    /// Residency state to report for a model in this process state, or `None`
    /// when the model is not loaded (stopped/shut down) and should be omitted
    /// from a snapshot's resident list. `ready` is treated as hot on GPU;
    /// `stopping` as draining.
    pub fn residency(self) -> Option<ResidencyState> {
        match self {
            Self::Stopped | Self::Shutdown => None,
            Self::Starting => Some(ResidencyState::Loading),
            Self::Ready => Some(ResidencyState::HotGpu),
            Self::Stopping => Some(ResidencyState::Draining),
        }
    }
}

/// Shared, push-updated map of model alias → llama-swap process state. Written
/// by [`LlamaSwapEventStream`] on each SSE frame; read by
/// [`LlamaSwapAdapter::inspect`].
pub type ModelStateCache = Arc<RwLock<HashMap<String, LlamaSwapModelState>>>;

#[derive(Debug, Deserialize)]
struct ModelStatusFrame {
    #[serde(rename = "type")]
    kind: String,
    /// llama-swap double-encodes the snapshot: `data` is a JSON *string*
    /// containing the array of `{id, state}` entries, not a nested array.
    data: String,
}

#[derive(Debug, Deserialize)]
struct ModelStatusEntry {
    id: String,
    state: String,
}

/// Parses one SSE event's `data:` payload. Returns the model states carried by a
/// `modelStatus` frame, or `None` for any other event type or malformed payload
/// (heartbeats, comments, partial frames). Unknown `state` strings are dropped.
fn parse_model_status_payload(payload: &str) -> Option<Vec<(String, LlamaSwapModelState)>> {
    let frame: ModelStatusFrame = serde_json::from_str(payload).ok()?;
    if frame.kind != "modelStatus" {
        return None;
    }
    let entries: Vec<ModelStatusEntry> = serde_json::from_str(&frame.data).ok()?;
    Some(
        entries
            .into_iter()
            .filter_map(|entry| {
                LlamaSwapModelState::from_wire(&entry.state).map(|state| (entry.id, state))
            })
            .collect(),
    )
}

/// Incremental decoder for an SSE byte stream. Accumulates bytes across chunk
/// boundaries, splits on blank-line event delimiters, and yields the parsed
/// `modelStatus` updates from each complete event. Carriage returns are stripped
/// so `\r\n\r\n` and `\n\n` delimiters are handled alike.
#[derive(Default)]
struct SseDecoder {
    buffer: Vec<u8>,
}

impl SseDecoder {
    fn push(&mut self, chunk: &[u8]) -> Vec<Vec<(String, LlamaSwapModelState)>> {
        self.buffer
            .extend(chunk.iter().copied().filter(|&b| b != b'\r'));
        let mut updates = Vec::new();
        while let Some(end) = find_subslice(&self.buffer, b"\n\n") {
            let event: Vec<u8> = self.buffer.drain(..end + 2).collect();
            if let Ok(text) = std::str::from_utf8(&event[..end]) {
                if let Some(payload) = sse_data_field(text) {
                    if let Some(update) = parse_model_status_payload(&payload) {
                        updates.push(update);
                    }
                }
            }
        }
        updates
    }
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Joins the `data:` lines of one SSE event into a single payload, per the SSE
/// spec (multiple `data:` lines concatenate with newlines). Returns `None` when
/// the event has no data lines.
fn sse_data_field(event: &str) -> Option<String> {
    let mut data_lines = Vec::new();
    for line in event.lines() {
        if let Some(rest) = line.strip_prefix("data:") {
            data_lines.push(rest.strip_prefix(' ').unwrap_or(rest));
        }
    }
    (!data_lines.is_empty()).then(|| data_lines.join("\n"))
}

/// Replaces the cache with a full `modelStatus` snapshot. llama-swap pushes the
/// complete model set on every change, so this is replace, not merge.
fn apply_model_status(cache: &ModelStateCache, entries: Vec<(String, LlamaSwapModelState)>) {
    let mut guard = cache.write().unwrap_or_else(PoisonError::into_inner);
    *guard = entries.into_iter().collect();
}

/// Builds snapshot residents from a model-state cache, omitting models that are
/// not loaded (stopped/shut down). Sorted by model id for deterministic output
/// so reconciliation diffs are stable.
fn residents_from_states(states: &HashMap<String, LlamaSwapModelState>) -> Vec<ModelResident> {
    let mut residents: Vec<ModelResident> = states
        .iter()
        .filter_map(|(name, state)| {
            let residency = state.residency()?;
            // Drop residents whose id normalizes to empty (malformed runtime data).
            let model_id = normalize_model_id(name).ok()?;
            Some(ModelResident {
                model_id,
                state: residency,
                vram_mb: None,
                ram_mb: None,
                kv_cache_mb: None,
                loaded_since: None,
            })
        })
        .collect();
    residents.sort_by(|a, b| a.model_id.0.cmp(&b.model_id.0));
    residents
}

/// Background subscriber to llama-swap's `/api/events` SSE stream. Maintains a
/// [`ModelStateCache`] without polling, reconnecting on disconnect. The spawned
/// task is aborted when this handle is dropped.
pub struct LlamaSwapEventStream {
    cache: ModelStateCache,
    handle: tokio::task::JoinHandle<()>,
}

impl LlamaSwapEventStream {
    /// Read handle to the cache this stream maintains.
    pub fn cache(&self) -> ModelStateCache {
        Arc::clone(&self.cache)
    }
}

impl Drop for LlamaSwapEventStream {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

const EVENT_STREAM_RECONNECT_DELAY: Duration = Duration::from_secs(3);

async fn run_event_stream(url: Url, auth_token: Option<String>, cache: ModelStateCache) {
    let client = reqwest::Client::new();
    loop {
        if let Err(error) = stream_events_once(&client, &url, auth_token.as_deref(), &cache).await {
            tracing::debug!(%url, %error, "llama-swap event stream disconnected; reconnecting");
        }
        tokio::time::sleep(EVENT_STREAM_RECONNECT_DELAY).await;
    }
}

async fn stream_events_once(
    client: &reqwest::Client,
    url: &Url,
    auth_token: Option<&str>,
    cache: &ModelStateCache,
) -> Result<(), RuntimeError> {
    let mut request = client.get(url.clone());
    if let Some(token) = auth_token {
        request = request.bearer_auth(token);
    }
    let response = request.send().await?.error_for_status()?;
    let mut stream = response.bytes_stream();
    let mut decoder = SseDecoder::default();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        for update in decoder.push(&chunk) {
            apply_model_status(cache, update);
        }
    }
    Ok(())
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

fn bytes_to_mb(bytes: u64) -> u64 {
    bytes / 1024 / 1024
}

/// Normalizes a runtime-reported model identifier to a bare model id: strips any
/// directory prefix (handling both `/` and `\` separators) and a trailing
/// `.gguf`/`.bin` extension.
///
/// Returns [`RuntimeError::Url`] for an id that normalizes to empty (`""`,
/// `"models/"`, `"weights\\"`, …). A runtime that reports a model with no usable
/// id is malformed; callers skip such entries so an empty `ModelId` never
/// propagates into scheduling, logging, and telemetry.
fn normalize_model_id(raw: &str) -> Result<ModelId, RuntimeError> {
    let normalized = raw.replace('\\', "/");
    let leaf = normalized.rsplit('/').next().unwrap_or(&normalized);
    let leaf = leaf
        .strip_suffix(".gguf")
        .or_else(|| leaf.strip_suffix(".bin"))
        .unwrap_or(leaf);
    if leaf.is_empty() {
        return Err(RuntimeError::Url(format!("empty model id (raw: {raw:?})")));
    }
    Ok(ModelId(leaf.to_string()))
}

/// Where a forwarded chat completion should go. `auth_token` originates from
/// the environment (expanded at config load) and is never committed.
#[derive(Debug, Clone)]
pub struct ForwardTarget {
    pub base_url: String,
    pub auth_token: Option<String>,
}

/// A chat-completion response forwarded back from a runtime. The body is a
/// stream so the caller can relay it without buffering (the inference gateway
/// requirement).
pub struct ForwardedChatCompletion {
    pub status: u16,
    pub content_type: Option<String>,
    pub body: BoxStream<'static, Result<Vec<u8>, RuntimeError>>,
}

const FORWARD_TIMEOUT: Duration = Duration::from_secs(120);

/// Forwards an OpenAI-style chat completion `body` to a runtime's
/// `/v1/chat/completions` endpoint, injecting the runtime bearer token when one
/// is configured. The response body is streamed straight through; only
/// transport failures (connection refused, timeout) surface as an `Err`.
pub async fn forward_chat_completion(
    target: &ForwardTarget,
    body: &serde_json::Value,
) -> Result<ForwardedChatCompletion, RuntimeError> {
    let base =
        Url::parse(&target.base_url).map_err(|error| RuntimeError::Url(error.to_string()))?;
    let url = base
        .join("/v1/chat/completions")
        .map_err(|error| RuntimeError::Url(error.to_string()))?;

    let client = reqwest::Client::builder()
        .timeout(FORWARD_TIMEOUT)
        .build()?;
    let mut request = client.post(url).json(body);
    if let Some(token) = &target.auth_token {
        request = request.bearer_auth(token);
    }

    let response = request.send().await?;
    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let body = response
        .bytes_stream()
        .map(|chunk| {
            chunk
                .map(|bytes| bytes.to_vec())
                .map_err(RuntimeError::from)
        })
        .boxed();

    Ok(ForwardedChatCompletion {
        status,
        content_type,
        body,
    })
}

/// Builds a deterministic OpenAI-style SSE response for the mock runtime. The
/// payload echoes `selected_model`, so a test can confirm the gateway rewrote
/// the `model` field to the decision's selected model. No network is involved.
pub fn mock_chat_completion(selected_model: &str) -> ForwardedChatCompletion {
    let chunk = format!(
        "data: {{\"object\":\"chat.completion.chunk\",\"model\":\"{selected_model}\",\
\"choices\":[{{\"index\":0,\"delta\":{{\"role\":\"assistant\",\"content\":\"mock response from {selected_model}\"}}}}]}}\n\n"
    );
    let chunks: Vec<Result<Vec<u8>, RuntimeError>> =
        vec![Ok(chunk.into_bytes()), Ok(b"data: [DONE]\n\n".to_vec())];
    ForwardedChatCompletion {
        status: 200,
        content_type: Some("text/event-stream".to_string()),
        body: futures::stream::iter(chunks).boxed(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anemoi_core::{RequestId, RuntimeMemorySnapshot};
    use std::collections::VecDeque;
    use std::sync::Mutex;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[test]
    fn adapter_id_is_stable() {
        let adapter = MockRuntimeAdapter::new(RuntimeId("mock".to_string()), Vec::new());

        assert_eq!(adapter.id(), RuntimeId("mock".to_string()));
        assert_eq!(adapter.id(), RuntimeId("mock".to_string()));
    }

    #[tokio::test]
    async fn inspect_returns_normalized_runtime_snapshot() {
        let adapter = MockRuntimeAdapter::new(
            RuntimeId("mock".to_string()),
            vec![ModelResident {
                model_id: ModelId("qwen9b".to_string()),
                state: ResidencyState::HotGpu,
                vram_mb: Some(9000),
                ram_mb: Some(12000),
                kv_cache_mb: Some(512),
                loaded_since: None,
            }],
        )
        .with_memory(RuntimeMemorySnapshot {
            vram_total_mb: Some(24_000),
            vram_used_mb: Some(9_000),
            ram_total_mb: Some(64_000),
            ram_used_mb: Some(12_000),
        });

        let snapshot = adapter.inspect().await.expect("snapshot");

        assert_eq!(snapshot.runtime_id, RuntimeId("mock".to_string()));
        assert!(snapshot.available);
        assert_eq!(snapshot.residents.len(), 1);
        assert_eq!(
            snapshot.residents[0].model_id,
            ModelId("qwen9b".to_string())
        );
        assert_eq!(snapshot.residents[0].state, ResidencyState::HotGpu);
        assert_eq!(snapshot.memory.pressure_percent(), Some(37));
    }

    #[tokio::test]
    async fn load_model_returns_model_load_handle() {
        let adapter = MockRuntimeAdapter::new(RuntimeId("mock".to_string()), Vec::new());
        let model_id = ModelId("qwen9b".to_string());

        let handle = adapter.load_model(&model_id).await.expect("load handle");
        let snapshot = adapter.inspect().await.expect("snapshot");

        assert_eq!(handle.model_id, model_id);
        assert!(snapshot.residents.iter().any(|resident| {
            resident.model_id == ModelId("qwen9b".to_string())
                && resident.state == ResidencyState::Loading
        }));
    }

    #[tokio::test]
    async fn execute_returns_execution_handle() {
        let adapter = MockRuntimeAdapter::new(RuntimeId("mock".to_string()), Vec::new());
        let request = ExecutionRequest {
            request_id: RequestId::new(),
            model_id: ModelId("qwen9b".to_string()),
            prompt: Some("hello".to_string()),
        };

        let handle = adapter
            .execute(request.clone())
            .await
            .expect("execute handle");
        let snapshot = adapter.inspect().await.expect("snapshot");

        assert_eq!(handle.request, request);
        assert_eq!(snapshot.active_requests.len(), 1);
        assert_eq!(snapshot.active_requests[0].request_id, request.request_id);
        assert_eq!(snapshot.active_requests[0].model_id, request.model_id);
    }

    #[tokio::test]
    async fn unsupported_unload_returns_runtime_error() {
        let adapter =
            HttpInspectAdapter::new(RuntimeId("llama_swap".to_string()), "http://localhost:8080")
                .expect("adapter");

        let error = adapter
            .unload_model(&ModelId("qwen9b".to_string()))
            .await
            .expect_err("unsupported unload");

        assert_eq!(
            error.to_string(),
            "runtime operation is not supported: http unload"
        );
    }

    #[test]
    fn runtime_errors_are_human_readable() {
        let unavailable = RuntimeError::Unavailable(RuntimeId("ollama".to_string()));
        let unsupported = RuntimeError::Unsupported("ollama unload");
        let url = RuntimeError::Url("relative URL without a base".to_string());

        assert_eq!(unavailable.to_string(), "runtime ollama is unavailable");
        assert_eq!(
            unsupported.to_string(),
            "runtime operation is not supported: ollama unload"
        );
        assert_eq!(
            url.to_string(),
            "invalid runtime url: relative URL without a base"
        );
    }

    #[tokio::test]
    async fn mock_runtime_starts_with_configured_residents() {
        let adapter = MockRuntimeAdapter::new(
            RuntimeId("mock".to_string()),
            vec![ModelResident {
                model_id: ModelId("qwen9b".to_string()),
                state: ResidencyState::HotGpu,
                vram_mb: Some(9000),
                ram_mb: None,
                kv_cache_mb: None,
                loaded_since: None,
            }],
        );

        let snapshot = adapter.inspect().await.expect("snapshot");

        assert_eq!(snapshot.residents.len(), 1);
        assert_eq!(
            snapshot.residents[0].model_id,
            ModelId("qwen9b".to_string())
        );
        assert_eq!(snapshot.residents[0].state, ResidencyState::HotGpu);
    }

    #[tokio::test]
    async fn mock_runtime_load_adds_loading_resident_once() {
        let adapter = MockRuntimeAdapter::new(RuntimeId("mock".to_string()), Vec::new());
        let model_id = ModelId("qwen9b".to_string());

        adapter.load_model(&model_id).await.expect("first load");
        adapter.load_model(&model_id).await.expect("second load");
        let snapshot = adapter.inspect().await.expect("snapshot");

        assert_eq!(
            snapshot
                .residents
                .iter()
                .filter(|resident| resident.model_id == model_id)
                .count(),
            1
        );
        assert_eq!(snapshot.residents[0].state, ResidencyState::Loading);
    }

    #[tokio::test]
    async fn mock_runtime_unload_removes_resident() {
        let adapter = MockRuntimeAdapter::new(RuntimeId("mock".to_string()), Vec::new())
            .with_resident_state(ModelId("qwen9b".to_string()), ResidencyState::HotGpu);

        adapter
            .unload_model(&ModelId("qwen9b".to_string()))
            .await
            .expect("unload");
        let snapshot = adapter.inspect().await.expect("snapshot");

        assert!(snapshot.residents.is_empty());
    }

    #[tokio::test]
    async fn mock_runtime_execute_records_active_request() {
        let adapter = MockRuntimeAdapter::new(RuntimeId("mock".to_string()), Vec::new());
        let request = ExecutionRequest {
            request_id: RequestId::new(),
            model_id: ModelId("qwen9b".to_string()),
            prompt: None,
        };

        adapter.execute(request.clone()).await.expect("execute");
        let snapshot = adapter.inspect().await.expect("snapshot");

        assert_eq!(snapshot.active_requests.len(), 1);
        assert_eq!(snapshot.active_requests[0].request_id, request.request_id);
        assert_eq!(snapshot.active_requests[0].model_id, request.model_id);
    }

    #[tokio::test]
    async fn mock_runtime_memory_snapshot_is_configurable() {
        let adapter = MockRuntimeAdapter::new(RuntimeId("mock".to_string()), Vec::new())
            .with_memory(RuntimeMemorySnapshot {
                vram_total_mb: Some(30_000),
                vram_used_mb: Some(27_000),
                ram_total_mb: Some(64_000),
                ram_used_mb: Some(12_000),
            })
            .with_available(false);

        let snapshot = adapter.inspect().await.expect("snapshot");

        assert!(!snapshot.available);
        assert_eq!(snapshot.memory.pressure_percent(), Some(90));
    }

    #[tokio::test]
    async fn mock_runtime_inspect_is_repeatable() {
        let adapter = MockRuntimeAdapter::new(RuntimeId("mock".to_string()), Vec::new())
            .with_resident_state(ModelId("qwen9b".to_string()), ResidencyState::WarmCpu)
            .with_memory(RuntimeMemorySnapshot {
                vram_total_mb: Some(24_000),
                vram_used_mb: Some(12_000),
                ram_total_mb: None,
                ram_used_mb: None,
            });

        let first = adapter.inspect().await.expect("first snapshot");
        let second = adapter.inspect().await.expect("second snapshot");

        assert_eq!(first, second);
    }

    #[tokio::test]
    async fn llama_swap_health_marks_runtime_available() {
        let server = spawn_fixture(vec![
            http_response(200, "{}"),
            http_response(200, r#"{"data":[]}"#),
        ])
        .await;
        let adapter = LlamaSwapAdapter::new(RuntimeId("llama_swap".to_string()), &server.base_url)
            .expect("adapter");

        let snapshot = adapter.inspect().await.expect("snapshot");

        assert!(snapshot.available);
    }

    #[tokio::test]
    async fn llama_swap_failed_health_marks_runtime_unavailable() {
        let server = spawn_fixture(vec![http_response(500, "{}")]).await;
        let adapter = LlamaSwapAdapter::new(RuntimeId("llama_swap".to_string()), &server.base_url)
            .expect("adapter");

        let snapshot = adapter.inspect().await.expect("snapshot");

        assert!(!snapshot.available);
        assert!(snapshot.residents.is_empty());
    }

    #[tokio::test]
    async fn llama_swap_inspect_tolerates_flaky_models_endpoint() {
        // /health succeeds but /v1/models returns 500. The runtime is healthy, so
        // inspect must report it available with no configured models rather than
        // propagating the error (which would make it look like a hard outage).
        let server = spawn_fixture(vec![
            http_response(200, "{}"), // /health ok
            http_response(500, "{}"), // /v1/models fails
        ])
        .await;
        let adapter = LlamaSwapAdapter::new(RuntimeId("llama_swap".to_string()), &server.base_url)
            .expect("adapter");

        let snapshot = adapter
            .inspect()
            .await
            .expect("healthy runtime with flaky /v1/models must still inspect");

        assert!(snapshot.available, "healthy runtime must stay available");
        assert!(
            snapshot.configured_models.is_empty(),
            "a failed /v1/models yields no configured models"
        );
        assert!(snapshot.residents.is_empty());
    }

    #[tokio::test]
    async fn llama_cpp_inspect_tolerates_flaky_models_endpoint() {
        let server = spawn_fixture(vec![
            http_response(200, "{}"), // /health ok
            http_response(500, "{}"), // /v1/models fails
        ])
        .await;
        let adapter = LlamaCppAdapter::new(RuntimeId("llama_cpp".to_string()), &server.base_url)
            .expect("adapter");

        let snapshot = adapter
            .inspect()
            .await
            .expect("healthy runtime with flaky /v1/models must still inspect");

        assert!(snapshot.available, "healthy runtime must stay available");
        assert!(
            snapshot.configured_models.is_empty(),
            "a failed /v1/models yields no configured models"
        );
    }

    #[tokio::test]
    async fn llama_swap_models_response_normalizes_model_ids() {
        let server = spawn_fixture(vec![http_response(
            200,
            r#"{"data":[{"id":"models/qwen9b.gguf"},{"id":"granite8b"}]}"#,
        )])
        .await;
        let adapter = LlamaSwapAdapter::new(RuntimeId("llama_swap".to_string()), &server.base_url)
            .expect("adapter");

        let models = adapter.inspect_models().await.expect("models");

        assert_eq!(
            models,
            vec![
                ModelId("qwen9b".to_string()),
                ModelId("granite8b".to_string())
            ]
        );
    }

    #[tokio::test]
    async fn llama_swap_models_response_skips_empty_model_ids() {
        let server = spawn_fixture(vec![http_response(
            200,
            r#"{"data":[{"id":""},{"id":"models/qwen9b.gguf"},{"id":"models/"}]}"#,
        )])
        .await;
        let adapter = LlamaSwapAdapter::new(RuntimeId("llama_swap".to_string()), &server.base_url)
            .expect("adapter");

        let models = adapter.inspect_models().await.expect("models");

        // The empty id and the directory-only "models/" id (which normalizes to
        // empty) are dropped; only the valid model survives.
        assert_eq!(models, vec![ModelId("qwen9b".to_string())]);
    }

    #[test]
    fn normalize_model_id_strips_path_and_extension() {
        assert_eq!(
            normalize_model_id("models/qwen9b.gguf").expect("id"),
            ModelId("qwen9b".to_string())
        );
        assert_eq!(
            normalize_model_id(r"weights\granite8b.bin").expect("id"),
            ModelId("granite8b".to_string())
        );
    }

    #[test]
    fn normalize_model_id_rejects_empty_input() {
        // A runtime reporting a model with no usable id is malformed; an empty id
        // must be rejected rather than yielding ModelId("").
        let err = normalize_model_id("").expect_err("empty id must be rejected");
        assert!(
            err.to_string().contains("empty model id"),
            "unexpected error: {err}"
        );
        assert!(normalize_model_id("models/").is_err());
        assert!(normalize_model_id(r"weights\").is_err());
    }

    #[tokio::test]
    async fn llama_swap_inspect_returns_runtime_snapshot() {
        let server = spawn_fixture(vec![
            http_response(200, "{}"),
            http_response(200, r#"{"data":[{"id":"models/qwen9b.gguf"}]}"#),
        ])
        .await;
        let adapter = LlamaSwapAdapter::new(RuntimeId("llama_swap".to_string()), &server.base_url)
            .expect("adapter");

        let snapshot = adapter.inspect().await.expect("snapshot");

        assert_eq!(snapshot.runtime_id, RuntimeId("llama_swap".to_string()));
        assert!(snapshot.available);
        assert!(snapshot.residents.is_empty());
        assert_eq!(snapshot.memory, RuntimeMemorySnapshot::default());
    }

    #[tokio::test]
    async fn llama_swap_probe_does_not_require_mutating_endpoint() {
        let server = spawn_fixture(vec![
            http_response(200, "{}"),
            http_response(200, r#"{"data":[]}"#),
        ])
        .await;
        let adapter = LlamaSwapAdapter::new(RuntimeId("llama_swap".to_string()), &server.base_url)
            .expect("adapter");

        let _ = adapter.inspect().await.expect("inspect");

        let requests = server.requests.lock().expect("requests").clone();
        for request_text in &requests {
            assert!(
                request_text.starts_with("GET"),
                "probe must use GET only, got: {request_text:?}"
            );
        }
    }

    #[tokio::test]
    async fn llama_swap_probe_records_unknown_residency_when_endpoint_is_ambiguous() {
        let server = spawn_fixture(vec![
            http_response(200, "{}"),
            http_response(
                200,
                r#"{"data":[{"id":"models/qwen9b.gguf"},{"id":"models/qwen35_a3b.gguf"}]}"#,
            ),
        ])
        .await;
        let adapter = LlamaSwapAdapter::new(RuntimeId("llama_swap".to_string()), &server.base_url)
            .expect("adapter");

        let snapshot = adapter.inspect().await.expect("snapshot");

        // /v1/models returns models but does not prove hot residency.
        assert!(snapshot.available);
        assert!(
            snapshot.residents.is_empty(),
            "ambiguous endpoint must not claim hot residency"
        );
    }

    #[tokio::test]
    async fn llama_swap_probe_maps_configured_models_without_claiming_hot_residency() {
        let server = spawn_fixture(vec![
            // inspect_models: /v1/models
            http_response(
                200,
                r#"{"data":[{"id":"models/qwen9b.gguf"},{"id":"models/qwen35_a3b.gguf"}]}"#,
            ),
            // inspect: /health
            http_response(200, "{}"),
            // inspect: /v1/models
            http_response(
                200,
                r#"{"data":[{"id":"models/qwen9b.gguf"},{"id":"models/qwen35_a3b.gguf"}]}"#,
            ),
        ])
        .await;
        let adapter = LlamaSwapAdapter::new(RuntimeId("llama_swap".to_string()), &server.base_url)
            .expect("adapter");

        let models = adapter.inspect_models().await.expect("models");

        // inspect_models returns configured model ids but inspect does not claim residency.
        assert_eq!(
            models,
            vec![
                ModelId("qwen9b".to_string()),
                ModelId("qwen35_a3b".to_string()),
            ]
        );

        let snapshot = adapter.inspect().await.expect("snapshot");
        assert!(
            snapshot.residents.is_empty(),
            "configured models must not be reported as hot residents"
        );
    }

    #[tokio::test]
    async fn configured_model_without_runtime_residency_evidence_is_not_hot() {
        let server = spawn_fixture(vec![
            // /health returns ok
            http_response(200, "{}"),
            // /v1/models returns configured models
            http_response(
                200,
                r#"{"data":[{"id":"models/qwen9b.gguf"},{"id":"models/qwen35_a3b.gguf"}]}"#,
            ),
        ])
        .await;
        let adapter = LlamaSwapAdapter::new(RuntimeId("llama_swap".to_string()), &server.base_url)
            .expect("adapter");

        let snapshot = adapter.inspect().await.expect("snapshot");

        // /v1/models returns models but inspect must not claim hot residency.
        assert!(snapshot.available);
        assert_eq!(
            snapshot.residents.len(),
            0,
            "configured models without runtime evidence must not be hot"
        );
    }

    #[tokio::test]
    async fn llama_swap_inspect_populates_residents_from_models_endpoint() {
        // Name preserved from issue #46. Per the residency truth contract,
        // /v1/models proves configuration, not residency — so configured
        // models surface in `configured_models`, not `residents`.
        let server = spawn_fixture(vec![
            http_response(200, "{}"),
            http_response(
                200,
                r#"{"data":[{"id":"models/qwen9b.gguf"},{"id":"granite8b"}]}"#,
            ),
        ])
        .await;
        let adapter = LlamaSwapAdapter::new(RuntimeId("llama_swap".to_string()), &server.base_url)
            .expect("adapter");

        let snapshot = adapter.inspect().await.expect("snapshot");

        assert!(snapshot.available);
        assert_eq!(
            snapshot.configured_models,
            vec![
                ModelId("qwen9b".to_string()),
                ModelId("granite8b".to_string()),
            ]
        );
    }

    #[tokio::test]
    async fn llama_swap_inspect_marks_residents_as_configured_not_hot() {
        // Name preserved from issue #46. Configured models from /v1/models must
        // not be reported as residents at all — residency requires evidence
        // beyond configuration (see residency-truth-contract.md).
        let server = spawn_fixture(vec![
            http_response(200, "{}"),
            http_response(200, r#"{"data":[{"id":"models/qwen9b.gguf"}]}"#),
        ])
        .await;
        let adapter = LlamaSwapAdapter::new(RuntimeId("llama_swap".to_string()), &server.base_url)
            .expect("adapter");

        let snapshot = adapter.inspect().await.expect("snapshot");

        assert_eq!(
            snapshot.configured_models,
            vec![ModelId("qwen9b".to_string())]
        );
        assert!(
            snapshot.residents.is_empty(),
            "configured models must not appear as residents"
        );
    }

    #[test]
    fn llama_swap_model_state_maps_wire_strings() {
        assert_eq!(
            LlamaSwapModelState::from_wire("stopped"),
            Some(LlamaSwapModelState::Stopped)
        );
        assert_eq!(
            LlamaSwapModelState::from_wire("starting"),
            Some(LlamaSwapModelState::Starting)
        );
        assert_eq!(
            LlamaSwapModelState::from_wire("ready"),
            Some(LlamaSwapModelState::Ready)
        );
        assert_eq!(
            LlamaSwapModelState::from_wire("stopping"),
            Some(LlamaSwapModelState::Stopping)
        );
        assert_eq!(
            LlamaSwapModelState::from_wire("shutdown"),
            Some(LlamaSwapModelState::Shutdown)
        );
        assert_eq!(LlamaSwapModelState::from_wire("bogus"), None);
    }

    #[test]
    fn llama_swap_model_state_residency_omits_unloaded() {
        assert_eq!(LlamaSwapModelState::Stopped.residency(), None);
        assert_eq!(LlamaSwapModelState::Shutdown.residency(), None);
        assert_eq!(
            LlamaSwapModelState::Starting.residency(),
            Some(ResidencyState::Loading)
        );
        assert_eq!(
            LlamaSwapModelState::Ready.residency(),
            Some(ResidencyState::HotGpu)
        );
        assert_eq!(
            LlamaSwapModelState::Stopping.residency(),
            Some(ResidencyState::Draining)
        );
    }

    #[test]
    fn parse_model_status_payload_extracts_double_encoded_states() {
        // Captured shape: `data` is a JSON-encoded *string* of the entries.
        let payload = r#"{"type":"modelStatus","data":"[{\"id\":\"minimax\",\"state\":\"starting\"},{\"id\":\"gemma\",\"state\":\"ready\"}]"}"#;

        let parsed = parse_model_status_payload(payload).expect("modelStatus frame");

        assert_eq!(
            parsed,
            vec![
                ("minimax".to_string(), LlamaSwapModelState::Starting),
                ("gemma".to_string(), LlamaSwapModelState::Ready),
            ]
        );
    }

    #[test]
    fn parse_model_status_payload_ignores_other_frame_types() {
        let payload = r#"{"type":"logData","data":"some log line"}"#;
        assert_eq!(parse_model_status_payload(payload), None);
    }

    #[test]
    fn sse_decoder_emits_frame_only_once_complete() {
        let event = format!(
            "data: {}\n\n",
            r#"{"type":"modelStatus","data":"[{\"id\":\"gemma\",\"state\":\"ready\"}]"}"#
        );
        let (head, tail) = event.split_at(20);
        let mut decoder = SseDecoder::default();

        assert!(
            decoder.push(head.as_bytes()).is_empty(),
            "a partial frame must not emit an update"
        );
        let updates = decoder.push(tail.as_bytes());

        assert_eq!(
            updates,
            vec![vec![("gemma".to_string(), LlamaSwapModelState::Ready)]]
        );
    }

    #[test]
    fn sse_decoder_splits_multiple_events_in_one_chunk() {
        let chunk = format!(
            "data: {}\n\ndata: {}\n\n",
            r#"{"type":"modelStatus","data":"[{\"id\":\"a\",\"state\":\"starting\"}]"}"#,
            r#"{"type":"modelStatus","data":"[{\"id\":\"a\",\"state\":\"ready\"}]"}"#
        );
        let mut decoder = SseDecoder::default();

        let updates = decoder.push(chunk.as_bytes());

        assert_eq!(
            updates,
            vec![
                vec![("a".to_string(), LlamaSwapModelState::Starting)],
                vec![("a".to_string(), LlamaSwapModelState::Ready)],
            ]
        );
    }

    #[test]
    fn residents_from_states_omits_unloaded_and_sorts() {
        let mut states = HashMap::new();
        states.insert("models/qwen9b.gguf".to_string(), LlamaSwapModelState::Ready);
        states.insert("granite8b".to_string(), LlamaSwapModelState::Starting);
        states.insert("minimax".to_string(), LlamaSwapModelState::Stopped);

        let residents = residents_from_states(&states);

        assert_eq!(residents.len(), 2, "stopped model must be omitted");
        assert_eq!(residents[0].model_id, ModelId("granite8b".to_string()));
        assert_eq!(residents[0].state, ResidencyState::Loading);
        assert_eq!(residents[1].model_id, ModelId("qwen9b".to_string()));
        assert_eq!(residents[1].state, ResidencyState::HotGpu);
    }

    #[test]
    fn apply_model_status_replaces_previous_snapshot() {
        let cache: ModelStateCache = Arc::new(RwLock::new(HashMap::new()));
        apply_model_status(
            &cache,
            vec![("gemma".to_string(), LlamaSwapModelState::Ready)],
        );
        apply_model_status(
            &cache,
            vec![("qwen9b".to_string(), LlamaSwapModelState::Starting)],
        );

        let guard = cache.read().expect("cache");
        assert_eq!(guard.len(), 1, "full snapshot replaces, not merges");
        assert_eq!(guard.get("qwen9b"), Some(&LlamaSwapModelState::Starting));
        assert!(guard.get("gemma").is_none());
    }

    #[tokio::test]
    async fn llama_swap_inspect_reports_residents_from_event_cache() {
        let server = spawn_fixture(vec![
            http_response(200, "{}"),
            http_response(200, r#"{"data":[{"id":"models/qwen9b.gguf"}]}"#),
        ])
        .await;
        let adapter = LlamaSwapAdapter::new(RuntimeId("llama_swap".to_string()), &server.base_url)
            .expect("adapter");

        // Simulate the SSE stream having observed a ready and a stopped model.
        apply_model_status(
            &adapter.model_states(),
            vec![
                ("qwen9b".to_string(), LlamaSwapModelState::Ready),
                ("granite8b".to_string(), LlamaSwapModelState::Stopped),
            ],
        );

        let snapshot = adapter.inspect().await.expect("snapshot");

        assert_eq!(
            snapshot.residents.len(),
            1,
            "only the ready model is resident"
        );
        assert_eq!(
            snapshot.residents[0].model_id,
            ModelId("qwen9b".to_string())
        );
        assert_eq!(snapshot.residents[0].state, ResidencyState::HotGpu);
    }

    #[tokio::test]
    async fn failed_runtime_health_maps_to_unavailable_snapshot() {
        let server = spawn_fixture(vec![http_response(503, "Service Unavailable")]).await;
        let adapter = LlamaSwapAdapter::new(RuntimeId("llama_swap".to_string()), &server.base_url)
            .expect("adapter");

        let snapshot = adapter.inspect().await.expect("snapshot");

        assert!(!snapshot.available);
        assert!(snapshot.residents.is_empty());
        assert_eq!(snapshot.memory, RuntimeMemorySnapshot::default());
    }

    #[tokio::test]
    async fn running_model_endpoint_maps_to_hot_or_serving() {
        let server = spawn_fixture(vec![http_response(
            200,
            r#"{"models":[{"name":"qwen9b","size_vram":9437184000}]}"#,
        )])
        .await;
        let adapter =
            OllamaAdapter::new(RuntimeId("ollama".to_string()), &server.base_url).expect("adapter");

        let snapshot = adapter.inspect().await.expect("snapshot");

        assert_eq!(snapshot.residents.len(), 1);
        assert_eq!(snapshot.residents[0].state, ResidencyState::HotGpu);
    }

    #[tokio::test]
    async fn ollama_load_model_is_unsupported() {
        // Ollama loads lazily on first inference; Anemoi does not proactively
        // load it. load_model must report Unsupported (not a fake LoadHandle) so
        // staging does not mark an intent Completed for a load that never ran.
        let adapter = OllamaAdapter::new(RuntimeId("ollama".to_string()), "http://localhost:11434")
            .expect("adapter");

        let error = adapter
            .load_model(&ModelId("qwen9b".to_string()))
            .await
            .expect_err("ollama load must be unsupported");

        assert_eq!(
            error.to_string(),
            "runtime operation is not supported: ollama load"
        );
    }

    #[tokio::test]
    async fn llama_swap_load_model_posts_to_chat_completions() {
        let server = spawn_fixture(vec![http_response(
            200,
            r#"{"choices":[{"message":{"role":"assistant","content":"ok"}}]}"#,
        )])
        .await;
        let adapter = LlamaSwapAdapter::new(RuntimeId("llama_swap".to_string()), &server.base_url)
            .expect("adapter");

        let handle = adapter
            .load_model(&ModelId("qwen9b".to_string()))
            .await
            .expect("load handle");

        assert_eq!(handle.model_id, ModelId("qwen9b".to_string()));

        let requests = server.requests.lock().expect("requests").clone();
        assert_eq!(requests.len(), 1);
        let request = &requests[0];
        assert!(
            request.starts_with("POST /v1/chat/completions"),
            "load_model must POST to /v1/chat/completions, got: {request:?}"
        );
        assert!(
            request.contains("\"model\":\"qwen9b\""),
            "request must carry the requested model id, got: {request:?}"
        );
    }

    #[tokio::test]
    async fn llama_swap_load_model_surfaces_upstream_5xx_as_error() {
        let server = spawn_fixture(vec![http_response(500, "{}")]).await;
        let adapter = LlamaSwapAdapter::new(RuntimeId("llama_swap".to_string()), &server.base_url)
            .expect("adapter");

        let error = adapter
            .load_model(&ModelId("qwen9b".to_string()))
            .await
            .expect_err("upstream 5xx must surface as error");

        assert!(matches!(error, RuntimeError::Http(_)));
    }

    #[tokio::test]
    async fn llama_swap_timeout_returns_runtime_error() {
        let base_url = spawn_timeout_server().await;
        let adapter = LlamaSwapAdapter::new_with_timeout(
            RuntimeId("llama_swap".to_string()),
            &base_url,
            Duration::from_millis(50),
        )
        .expect("adapter");

        let error = adapter.inspect().await.expect_err("timeout");

        assert!(matches!(error, RuntimeError::Http(error) if error.is_timeout()));
    }

    #[tokio::test]
    async fn llama_swap_auth_header_is_applied_when_configured() {
        let server = spawn_fixture(vec![
            http_response(200, "{}"),
            http_response(200, r#"{"data":[]}"#),
        ])
        .await;
        let adapter = LlamaSwapAdapter::new(RuntimeId("llama_swap".to_string()), &server.base_url)
            .expect("adapter")
            .with_bearer_token("secret");

        let _ = adapter.inspect().await.expect("snapshot");

        let requests = server.requests.lock().expect("requests").clone();
        assert_eq!(requests.len(), 2);
        assert!(requests.iter().all(|request| request
            .to_ascii_lowercase()
            .contains("authorization: bearer secret")));
    }

    #[tokio::test]
    async fn ollama_ps_response_maps_running_models_to_hot_residents() {
        let server = spawn_fixture(vec![http_response(
            200,
            r#"{"models":[{"name":"qwen9b","size_vram":9437184000,"size":12582912000}]}"#,
        )])
        .await;
        let adapter =
            OllamaAdapter::new(RuntimeId("ollama".to_string()), &server.base_url).expect("adapter");

        let snapshot = adapter.inspect().await.expect("snapshot");

        assert!(snapshot.available);
        assert_eq!(snapshot.residents.len(), 1);
        assert_eq!(
            snapshot.residents[0].model_id,
            ModelId("qwen9b".to_string())
        );
        assert_eq!(snapshot.residents[0].state, ResidencyState::HotGpu);
    }

    #[tokio::test]
    async fn ollama_ps_empty_response_returns_no_residents() {
        let server = spawn_fixture(vec![http_response(200, r#"{"models":[]}"#)]).await;
        let adapter =
            OllamaAdapter::new(RuntimeId("ollama".to_string()), &server.base_url).expect("adapter");

        let snapshot = adapter.inspect().await.expect("snapshot");

        assert!(snapshot.available);
        assert!(snapshot.residents.is_empty());
    }

    #[tokio::test]
    async fn ollama_ps_vram_bytes_convert_to_mb() {
        let server = spawn_fixture(vec![http_response(
            200,
            r#"{"models":[{"name":"qwen9b","size_vram":9437184000,"size":12582912000}]}"#,
        )])
        .await;
        let adapter =
            OllamaAdapter::new(RuntimeId("ollama".to_string()), &server.base_url).expect("adapter");

        let snapshot = adapter.inspect().await.expect("snapshot");

        assert_eq!(snapshot.residents[0].vram_mb, Some(9000));
        assert_eq!(snapshot.residents[0].ram_mb, Some(12000));
    }

    #[tokio::test]
    async fn ollama_unavailable_runtime_returns_error_or_unavailable_snapshot() {
        let server = spawn_fixture(vec![http_response(503, "{}")]).await;
        let adapter =
            OllamaAdapter::new(RuntimeId("ollama".to_string()), &server.base_url).expect("adapter");

        let snapshot = adapter.inspect().await.expect("snapshot");

        assert!(!snapshot.available);
        assert!(snapshot.residents.is_empty());
    }

    #[tokio::test]
    async fn ollama_malformed_response_returns_runtime_error() {
        let server = spawn_fixture(vec![http_response(200, "not json")]).await;
        let adapter =
            OllamaAdapter::new(RuntimeId("ollama".to_string()), &server.base_url).expect("adapter");

        let error = adapter.inspect().await.expect_err("malformed json");

        assert!(matches!(error, RuntimeError::Http(_)));
    }

    #[test]
    fn ollama_base_url_validation_rejects_invalid_url() {
        let error = OllamaAdapter::new(RuntimeId("ollama".to_string()), "not a url")
            .expect_err("invalid url");

        assert!(error.to_string().contains("invalid runtime url"));
    }

    #[tokio::test]
    async fn llama_cpp_health_marks_runtime_available() {
        let server = spawn_fixture(vec![
            http_response(200, "{}"),
            http_response(200, r#"{"data":[]}"#),
        ])
        .await;
        let adapter = LlamaCppAdapter::new(RuntimeId("llama_cpp".to_string()), &server.base_url)
            .expect("adapter");

        let snapshot = adapter.inspect().await.expect("snapshot");

        assert!(snapshot.available);
    }

    #[tokio::test]
    async fn llama_cpp_failed_health_marks_runtime_unavailable() {
        let server = spawn_fixture(vec![http_response(500, "{}")]).await;
        let adapter = LlamaCppAdapter::new(RuntimeId("llama_cpp".to_string()), &server.base_url)
            .expect("adapter");

        let snapshot = adapter.inspect().await.expect("snapshot");

        assert!(!snapshot.available);
        assert!(snapshot.residents.is_empty());
        assert!(snapshot.configured_models.is_empty());
    }

    #[tokio::test]
    async fn llama_cpp_models_response_normalizes_model_ids() {
        let server = spawn_fixture(vec![http_response(
            200,
            r#"{"data":[{"id":"models/qwen9b.gguf"},{"id":"granite8b"}]}"#,
        )])
        .await;
        let adapter = LlamaCppAdapter::new(RuntimeId("llama_cpp".to_string()), &server.base_url)
            .expect("adapter");

        let models = adapter.inspect_models().await.expect("models");

        assert_eq!(
            models,
            vec![
                ModelId("qwen9b".to_string()),
                ModelId("granite8b".to_string()),
            ]
        );
    }

    #[tokio::test]
    async fn llama_cpp_inspect_surfaces_configured_models_without_claiming_residency() {
        let server = spawn_fixture(vec![
            http_response(200, "{}"),
            http_response(200, r#"{"data":[{"id":"models/qwen9b.gguf"}]}"#),
        ])
        .await;
        let adapter = LlamaCppAdapter::new(RuntimeId("llama_cpp".to_string()), &server.base_url)
            .expect("adapter");

        let snapshot = adapter.inspect().await.expect("snapshot");

        assert_eq!(snapshot.runtime_id, RuntimeId("llama_cpp".to_string()));
        assert!(snapshot.available);
        assert_eq!(
            snapshot.configured_models,
            vec![ModelId("qwen9b".to_string())]
        );
        assert!(
            snapshot.residents.is_empty(),
            "/v1/models proves configuration, not residency"
        );
        assert_eq!(snapshot.memory, RuntimeMemorySnapshot::default());
    }

    #[tokio::test]
    async fn llama_cpp_probe_uses_get_only() {
        let server = spawn_fixture(vec![
            http_response(200, "{}"),
            http_response(200, r#"{"data":[]}"#),
        ])
        .await;
        let adapter = LlamaCppAdapter::new(RuntimeId("llama_cpp".to_string()), &server.base_url)
            .expect("adapter");

        let _ = adapter.inspect().await.expect("inspect");

        let requests = server.requests.lock().expect("requests").clone();
        for request_text in &requests {
            assert!(
                request_text.starts_with("GET"),
                "inspect must use GET only, got: {request_text:?}"
            );
        }
    }

    #[tokio::test]
    async fn llama_cpp_load_unload_execute_are_unsupported() {
        let adapter =
            LlamaCppAdapter::new(RuntimeId("llama_cpp".to_string()), "http://localhost:8080")
                .expect("adapter");
        let model = ModelId("qwen9b".to_string());

        let load_error = adapter.load_model(&model).await.expect_err("load");
        let unload_error = adapter.unload_model(&model).await.expect_err("unload");
        let execute_error = adapter
            .execute(ExecutionRequest {
                request_id: RequestId::new(),
                model_id: model,
                prompt: None,
            })
            .await
            .expect_err("execute");

        assert_eq!(
            load_error.to_string(),
            "runtime operation is not supported: llama-cpp load"
        );
        assert_eq!(
            unload_error.to_string(),
            "runtime operation is not supported: llama-cpp unload"
        );
        assert_eq!(
            execute_error.to_string(),
            "runtime operation is not supported: llama-cpp execute"
        );
    }

    #[tokio::test]
    async fn llama_cpp_auth_header_is_applied_when_configured() {
        let server = spawn_fixture(vec![
            http_response(200, "{}"),
            http_response(200, r#"{"data":[]}"#),
        ])
        .await;
        let adapter = LlamaCppAdapter::new(RuntimeId("llama_cpp".to_string()), &server.base_url)
            .expect("adapter")
            .with_bearer_token("secret");

        let _ = adapter.inspect().await.expect("snapshot");

        let requests = server.requests.lock().expect("requests").clone();
        assert_eq!(requests.len(), 2);
        assert!(requests.iter().all(|request| request
            .to_ascii_lowercase()
            .contains("authorization: bearer secret")));
    }

    #[tokio::test]
    async fn llama_cpp_timeout_returns_runtime_error() {
        let base_url = spawn_timeout_server().await;
        let adapter = LlamaCppAdapter::new_with_timeout(
            RuntimeId("llama_cpp".to_string()),
            &base_url,
            Duration::from_millis(50),
        )
        .expect("adapter");

        let error = adapter.inspect().await.expect_err("timeout");

        assert!(matches!(error, RuntimeError::Http(error) if error.is_timeout()));
    }

    #[test]
    fn llama_cpp_base_url_validation_rejects_invalid_url() {
        let error = LlamaCppAdapter::new(RuntimeId("llama_cpp".to_string()), "not a url")
            .expect_err("invalid url");

        assert!(error.to_string().contains("invalid runtime url"));
    }

    #[tokio::test]
    async fn inference_gateway_injects_runtime_auth_token() {
        let server = spawn_fixture(vec![http_response(200, "{}")]).await;
        let target = ForwardTarget {
            base_url: server.base_url.clone(),
            auth_token: Some("s3cr3t-token".to_string()),
        };
        let body = serde_json::json!({ "model": "qwen9b", "messages": [] });

        forward_chat_completion(&target, &body)
            .await
            .expect("forward");

        let request = server.requests.lock().expect("requests")[0].to_lowercase();
        assert!(
            request.contains("authorization: bearer s3cr3t-token"),
            "forwarded request must carry the runtime bearer token, got:\n{request}"
        );
    }

    #[tokio::test]
    async fn forward_chat_completion_omits_auth_when_unset() {
        let server = spawn_fixture(vec![http_response(200, "{}")]).await;
        let target = ForwardTarget {
            base_url: server.base_url.clone(),
            auth_token: None,
        };
        let body = serde_json::json!({ "model": "qwen9b", "messages": [] });

        forward_chat_completion(&target, &body)
            .await
            .expect("forward");

        let request = server.requests.lock().expect("requests")[0].to_lowercase();
        assert!(
            !request.contains("authorization:"),
            "no auth header should be sent when the runtime has no token"
        );
    }

    struct TestServer {
        base_url: String,
        requests: Arc<Mutex<Vec<String>>>,
    }

    async fn spawn_fixture(responses: Vec<String>) -> TestServer {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let request_log = requests.clone();
        let responses = Arc::new(Mutex::new(VecDeque::from(responses)));
        let response_queue = responses.clone();

        tokio::spawn(async move {
            loop {
                let Some(response) = response_queue.lock().expect("responses").pop_front() else {
                    break;
                };
                let (mut socket, _) = listener.accept().await.expect("accept");
                let mut buffer = vec![0; 4096];
                let read = socket.read(&mut buffer).await.expect("read");
                request_log
                    .lock()
                    .expect("requests")
                    .push(String::from_utf8_lossy(&buffer[..read]).to_string());
                socket
                    .write_all(response.as_bytes())
                    .await
                    .expect("write response");
            }
        });

        TestServer {
            base_url: format!("http://{addr}"),
            requests,
        }
    }

    async fn spawn_timeout_server() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut buffer = vec![0; 1024];
            let _ = socket.read(&mut buffer).await;
            tokio::time::sleep(Duration::from_secs(2)).await;
        });
        format!("http://{addr}")
    }

    fn http_response(status: u16, body: &str) -> String {
        let reason = match status {
            200 => "OK",
            500 => "Internal Server Error",
            _ => "Unknown",
        };
        format!(
            "HTTP/1.1 {status} {reason}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
            body.len()
        )
    }

    fn m(id: &str) -> ModelId {
        ModelId::from(id)
    }

    #[test]
    fn matrix_parses_vars_evict_costs_and_sets() {
        let matrix = LlamaSwapMatrixConfig::from_yaml_str(
            r#"
matrix:
  vars:
    gpu: 24576
  evict_costs:
    minimax: 90000
    qwen9b: 1
  sets:
    - name: small_plus_medium
      models: qwen9b & qwen35_a3b
"#,
        )
        .expect("parse")
        .expect("matrix block present");

        assert_eq!(matrix.vars.get("gpu"), Some(&24_576));
        assert_eq!(matrix.evict_costs.get("minimax"), Some(&90_000));
        assert_eq!(matrix.evict_costs.get("qwen9b"), Some(&1));
        assert_eq!(matrix.sets.len(), 1);
        assert_eq!(matrix.sets[0].name, "small_plus_medium");
        assert_eq!(matrix.sets[0].models, "qwen9b & qwen35_a3b");
    }

    #[test]
    fn can_colocate_true_for_models_in_shared_and_set() {
        let matrix = LlamaSwapMatrixConfig::from_yaml_str(
            r#"
matrix:
  sets:
    - name: pair
      models: qwen9b & qwen35_a3b
"#,
        )
        .expect("parse")
        .expect("matrix");

        assert!(matrix.can_colocate(&m("qwen9b"), &m("qwen35_a3b")));
        assert!(matrix.can_colocate(&m("qwen35_a3b"), &m("qwen9b")));
    }

    #[test]
    fn matrix_lowers_to_policy_facing_colocation_constraints() {
        let matrix = LlamaSwapMatrixConfig::from_yaml_str(
            r#"
matrix:
  sets:
    - name: pair
      models: qwen9b & qwen35_a3b
    - name: solo
      models: minimax
"#,
        )
        .expect("parse")
        .expect("matrix");

        // The lowered, runtime-agnostic form answers the same colocation
        // questions as the matrix itself — this is what the policy consumes off
        // the observed snapshot.
        let constraints = matrix.colocation_constraints();
        assert!(constraints.can_colocate(&m("qwen9b"), &m("qwen35_a3b")));
        assert!(!constraints.can_colocate(&m("qwen9b"), &m("minimax")));
    }

    #[test]
    fn adapter_surfaces_no_colocation_constraints_without_matrix() {
        let adapter = LlamaSwapAdapter::new(RuntimeId("ls".to_string()), "http://localhost:8085")
            .expect("adapter");
        assert!(adapter.colocation_constraints().is_none());
    }

    #[test]
    fn can_colocate_false_for_models_not_in_any_set_together() {
        let matrix = LlamaSwapMatrixConfig::from_yaml_str(
            r#"
matrix:
  sets:
    - name: pair
      models: qwen9b & qwen35_a3b
    - name: solo
      models: minimax
"#,
        )
        .expect("parse")
        .expect("matrix");

        // minimax runs alone — it shares no set with the colocating pair.
        assert!(!matrix.can_colocate(&m("qwen9b"), &m("minimax")));
        assert!(!matrix.can_colocate(&m("qwen35_a3b"), &m("minimax")));
        // A model absent from every set never colocates.
        assert!(!matrix.can_colocate(&m("qwen9b"), &m("gemma")));
    }

    #[test]
    fn or_branches_are_alternatives_not_colocated() {
        let matrix = LlamaSwapMatrixConfig::from_yaml_str(
            r#"
matrix:
  sets:
    - name: either
      models: qwen9b | qwen35_a3b
"#,
        )
        .expect("parse")
        .expect("matrix");

        assert!(!matrix.can_colocate(&m("qwen9b"), &m("qwen35_a3b")));
    }

    #[test]
    fn grouping_colocates_each_alternative_with_shared_factor() {
        let matrix = LlamaSwapMatrixConfig::from_yaml_str(
            r#"
matrix:
  sets:
    - name: grouped
      models: (qwen4b | qwen2b) & gemma_e2b
"#,
        )
        .expect("parse")
        .expect("matrix");

        assert!(matrix.can_colocate(&m("qwen4b"), &m("gemma_e2b")));
        assert!(matrix.can_colocate(&m("qwen2b"), &m("gemma_e2b")));
        // The two alternatives never load together.
        assert!(!matrix.can_colocate(&m("qwen4b"), &m("qwen2b")));
    }

    #[test]
    fn full_llama_swap_config_ignores_unrelated_keys() {
        let matrix = LlamaSwapMatrixConfig::from_yaml_str(
            r#"
healthCheckTimeout: 90
models:
  qwen9b-co:
    cmd: llama-server -m qwen9b.gguf
  minimax:
    cmd: llama-server -m minimax.gguf
groups:
  default:
    - qwen9b-co
matrix:
  sets:
    - name: pair
      models: qwen9b-co & qwen35_a3b-co
"#,
        )
        .expect("parse")
        .expect("matrix");

        // Model ids carry the `-co` colocation suffix verbatim from the config.
        assert!(matrix.can_colocate(&m("qwen9b-co"), &m("qwen35_a3b-co")));
        assert!(!matrix.can_colocate(&m("qwen9b-co"), &m("minimax")));
    }

    #[test]
    fn config_without_matrix_block_parses_to_none() {
        let parsed = LlamaSwapMatrixConfig::from_yaml_str(
            r#"
healthCheckTimeout: 90
models:
  qwen9b:
    cmd: llama-server -m qwen9b.gguf
"#,
        )
        .expect("parse");

        assert!(parsed.is_none());
    }

    #[test]
    fn invalid_yaml_is_a_config_error() {
        let result = LlamaSwapMatrixConfig::from_yaml_str("matrix: [unterminated");

        assert!(matches!(result, Err(RuntimeError::Config(_))));
    }

    #[test]
    fn from_yaml_file_reads_matrix_block() {
        let path = std::env::temp_dir().join(format!("anemoi-matrix-{}.yaml", Uuid::new_v4()));
        std::fs::write(
            &path,
            "matrix:\n  sets:\n    - name: pair\n      models: qwen9b & qwen35_a3b\n",
        )
        .expect("write fixture");

        let matrix = LlamaSwapMatrixConfig::from_yaml_file(&path)
            .expect("parse")
            .expect("matrix");
        let _ = std::fs::remove_file(&path);

        assert!(matrix.can_colocate(&m("qwen9b"), &m("qwen35_a3b")));
    }

    #[test]
    fn adapter_can_colocate_false_without_matrix() {
        let adapter =
            LlamaSwapAdapter::new(RuntimeId("llama_swap".to_string()), "http://localhost:8080")
                .expect("adapter");

        assert!(adapter.matrix().is_none());
        assert!(!adapter.can_colocate(&m("qwen9b"), &m("qwen35_a3b")));
    }

    #[test]
    fn adapter_can_colocate_uses_matrix() {
        let matrix = LlamaSwapMatrixConfig::from_yaml_str(
            r#"
matrix:
  sets:
    - name: pair
      models: qwen9b & qwen35_a3b
"#,
        )
        .expect("parse")
        .expect("matrix");
        let adapter =
            LlamaSwapAdapter::new(RuntimeId("llama_swap".to_string()), "http://localhost:8080")
                .expect("adapter")
                .with_matrix_config(matrix);

        assert!(adapter.can_colocate(&m("qwen9b"), &m("qwen35_a3b")));
        assert!(!adapter.can_colocate(&m("qwen9b"), &m("minimax")));
    }
}
