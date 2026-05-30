use anemoi_core::{
    ActionKind, ActionPlan, AnemoiConfig, Decision, DecisionAction, DomainId, ExecutionMode,
    InferenceRequest, ModelId, ModelResident, RequestId, ResidencyState, RuntimeId,
    RuntimeSnapshot,
};
use anemoi_policy::{EvictionCandidateResident, EvictionPlan, EvictionRequest, Scheduler};
use anemoi_runtime::{
    DynRuntimeAdapter, ForwardedChatCompletion, LlamaCppAdapter, LlamaSwapAdapter,
    LlamaSwapEventStream, MockRuntimeAdapter, OllamaAdapter,
};
use anemoi_telemetry::{DynDecisionLog, InMemoryDecisionLog, ResidentTransitionRecord};
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, PoisonError, RwLock as StdRwLock};
use tokio::sync::RwLock;
use tower_http::trace::TraceLayer;
use uuid::Uuid;

const DEFAULT_RECONCILIATION_TTL_MS: u64 = 5000;
const DEFAULT_STAGING_POLL_MS: u64 = 5000;

#[derive(Debug, Clone)]
pub struct ReconciledSnapshot {
    pub snapshot: RuntimeSnapshot,
    pub last_inspected: DateTime<Utc>,
    pub last_error: Option<String>,
    pub is_stale: bool,
}

impl ReconciledSnapshot {
    pub fn fresh(snapshot: RuntimeSnapshot) -> Self {
        Self {
            snapshot,
            last_inspected: Utc::now(),
            last_error: None,
            is_stale: false,
        }
    }

    pub fn stale(snapshot: RuntimeSnapshot, last_error: Option<String>) -> Self {
        Self {
            snapshot,
            last_inspected: Utc::now(),
            last_error,
            is_stale: true,
        }
    }

    pub fn from_error(runtime_id: RuntimeId, error: String) -> Self {
        let snapshot = RuntimeSnapshot {
            runtime_id,
            available: false,
            residents: Vec::new(),
            configured_models: Vec::new(),
            memory: anemoi_core::RuntimeMemorySnapshot::default(),
            active_requests: Vec::new(),
        };
        Self {
            snapshot,
            last_inspected: Utc::now(),
            last_error: Some(error),
            is_stale: true,
        }
    }
}

#[derive(Clone)]
pub struct Reconciler {
    cache: Arc<RwLock<HashMap<String, ReconciledSnapshot>>>,
    ttl_ms: u64,
}

impl Reconciler {
    pub fn new(ttl_ms: u64) -> Self {
        Self {
            cache: Arc::new(RwLock::new(HashMap::new())),
            ttl_ms,
        }
    }

    pub async fn update(&self, runtime_id: &str, snapshot: RuntimeSnapshot) {
        let mut cache = self.cache.write().await;
        cache.insert(runtime_id.to_string(), ReconciledSnapshot::fresh(snapshot));
    }

    pub async fn record_error(&self, runtime_id: &str, error: String) {
        let mut cache = self.cache.write().await;
        let runtime_id_obj = RuntimeId(runtime_id.to_string());
        cache.insert(
            runtime_id.to_string(),
            ReconciledSnapshot::from_error(runtime_id_obj, error),
        );
    }

    pub async fn mark_stale(&self) {
        let mut cache = self.cache.write().await;
        for (_, reconciled) in cache.iter_mut() {
            reconciled.is_stale = true;
        }
    }

    pub async fn get(&self, runtime_id: &str) -> Option<ReconciledSnapshot> {
        let cache = self.cache.read().await;
        let mut reconciled = cache.get(runtime_id).cloned();
        if let Some(ref mut r) = reconciled {
            let elapsed = Utc::now()
                .signed_duration_since(r.last_inspected)
                .num_milliseconds();
            r.is_stale = r.is_stale || elapsed > self.ttl_ms as i64;
        }
        reconciled
    }

    pub async fn all(&self) -> Vec<ReconciledSnapshot> {
        let cache = self.cache.read().await;
        cache
            .values()
            .map(|r| {
                let mut r = r.clone();
                let elapsed = Utc::now()
                    .signed_duration_since(r.last_inspected)
                    .num_milliseconds();
                r.is_stale = r.is_stale || elapsed > self.ttl_ms as i64;
                r
            })
            .collect()
    }

    pub async fn get_snapshots(&self) -> Vec<RuntimeSnapshot> {
        self.all().await.into_iter().map(|r| r.snapshot).collect()
    }

    pub async fn has_cache(&self) -> bool {
        !self.cache.read().await.is_empty()
    }
}

impl Default for Reconciler {
    fn default() -> Self {
        Self::new(DEFAULT_RECONCILIATION_TTL_MS)
    }
}

/// A resident state transition detected between two consecutive reconciliation
/// snapshots (issue #12).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResidentTransitionDetail {
    pub model_id: ModelId,
    pub from_state: ResidencyState,
    pub to_state: ResidencyState,
    /// True when `to_state` is the first time this model is observed and
    /// `from_state` was inferred as `Cold` rather than read from a prior
    /// snapshot.
    pub first_observation: bool,
}

/// Detects residency transitions between the previous and current snapshot's
/// residents (issue #12).
///
/// A model present in `current` whose state differs from `previous` — or that
/// is newly present — yields one transition. A newly present model's prior
/// state is inferred as `Cold`: the established-vocabulary representation of
/// "not resident". Issue #12 forbids inventing an `Unknown` variant, and `Cold`
/// is the honest stand-in (the `first_observation` flag and an accompanying
/// note mark it as inferred rather than observed).
///
/// Models that vanished from `current` produce no transition: their target
/// state is not observed, and inferring one (cold? evicting?) would be
/// speculative. Only observed states are recorded.
fn resident_transitions(
    previous: &[ModelResident],
    current: &[ModelResident],
) -> Vec<ResidentTransitionDetail> {
    let prior: HashMap<&ModelId, &ResidencyState> =
        previous.iter().map(|r| (&r.model_id, &r.state)).collect();
    let mut transitions = Vec::new();
    for resident in current {
        match prior.get(&resident.model_id) {
            Some(&prev) if *prev == resident.state => {}
            Some(&prev) => transitions.push(ResidentTransitionDetail {
                model_id: resident.model_id.clone(),
                from_state: prev.clone(),
                to_state: resident.state.clone(),
                first_observation: false,
            }),
            None => transitions.push(ResidentTransitionDetail {
                model_id: resident.model_id.clone(),
                from_state: ResidencyState::Cold,
                to_state: resident.state.clone(),
                first_observation: true,
            }),
        }
    }
    transitions
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StagingState {
    Blocked,
    Pending,
    Failed,
    Completed,
}

impl StagingState {
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Failed | Self::Completed)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StagingIntent {
    pub id: Uuid,
    pub decision_id: Uuid,
    pub foreground_model: Option<ModelId>,
    pub background_model: ModelId,
    pub target_runtime: RuntimeId,
    pub reason: String,
    pub created_at: DateTime<Utc>,
    pub state: StagingState,
    pub last_error: Option<String>,
    /// Context payload declared via `escalation_intent` at decision time, held
    /// until the foreground model is evicted. On the eviction signal it is
    /// written to the [`EscalationContextCache`], `context_ref` is set, and this
    /// is cleared. Skipped in serialized output — the payload is local only and
    /// `context_ref` is what consumers read (issue #64).
    #[serde(skip)]
    pub pending_context: Option<String>,
    /// Reference to the stored context payload, set once the context has been
    /// committed to the escalation cache on the eviction signal. `None` until
    /// then, and when no context was declared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_ref: Option<ContextRef>,
}

impl StagingIntent {
    pub fn new(
        decision_id: Uuid,
        foreground_model: Option<ModelId>,
        background_model: ModelId,
        target_runtime: RuntimeId,
        reason: String,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            decision_id,
            foreground_model,
            background_model,
            target_runtime,
            reason,
            created_at: Utc::now(),
            state: StagingState::Blocked,
            last_error: None,
            pending_context: None,
            context_ref: None,
        }
    }

    pub fn mark_pending(&mut self) {
        self.state = StagingState::Pending;
    }

    pub fn mark_completed(&mut self) {
        self.state = StagingState::Completed;
    }

    pub fn mark_failed(&mut self, error: String) {
        self.state = StagingState::Failed;
        self.last_error = Some(error);
    }
}

#[derive(Clone)]
pub struct StagingWorker {
    queue: Arc<RwLock<Vec<StagingIntent>>>,
}

impl StagingWorker {
    pub fn new() -> Self {
        Self {
            queue: Arc::new(RwLock::new(Vec::new())),
        }
    }

    pub async fn enqueue(&self, intent: StagingIntent) {
        let mut queue = self.queue.write().await;
        queue.push(intent);
    }

    pub async fn get_all(&self) -> Vec<StagingIntent> {
        self.queue.read().await.clone()
    }

    pub async fn get_pending(&self) -> Vec<StagingIntent> {
        let queue = self.queue.read().await;
        queue
            .iter()
            .filter(|i| i.state == StagingState::Pending)
            .cloned()
            .collect()
    }

    pub async fn update_state(
        &self,
        intent_id: Uuid,
        new_state: StagingState,
        error: Option<String>,
    ) {
        let mut queue = self.queue.write().await;
        if let Some(intent) = queue.iter_mut().find(|i| i.id == intent_id) {
            match new_state {
                StagingState::Pending => intent.mark_pending(),
                StagingState::Completed => intent.mark_completed(),
                StagingState::Failed => intent.mark_failed(error.unwrap_or_default()),
                StagingState::Blocked => {
                    intent.state = StagingState::Blocked;
                }
            }
        }
    }

    pub async fn execute_pending(&self, runtime: &DynRuntimeAdapter) -> anyhow::Result<()> {
        // Loading a model mutates a real runtime, so it is gated exactly like the
        // daemon's reconciliation tick (`run_staging_tick`): a load may fire only
        // for a mock adapter or when `ANEMOI_ENABLE_LIVE_EXECUTE=1` is set. With
        // the gate closed the pending intents are left untouched — still
        // `Pending`, so the SSE-readiness path (`reconcile_ready`) can still
        // complete them if the model becomes resident by other means — rather
        // than dead-ended. Mock-ness comes from the adapter itself (`is_mock()`)
        // because this method holds only the adapter, not the runtime config that
        // `run_staging_tick` consults via `runtime_adapter_type`.
        if !runtime.is_mock() && !live_execution_enabled() {
            return Ok(());
        }
        let pending = self.get_pending().await;
        for intent in pending {
            match runtime.load_model(&intent.background_model).await {
                Ok(_) => {
                    self.update_state(intent.id, StagingState::Completed, None)
                        .await;
                }
                Err(e) => {
                    self.update_state(intent.id, StagingState::Failed, Some(e.to_string()))
                        .await;
                }
            }
        }
        Ok(())
    }

    /// Marks every `Pending` intent whose `background_model` is in `ready_models`
    /// as `Completed`, returning the completed ids. This is the SSE-driven
    /// completion path (issue #62): once the readiness stream reports a staged
    /// model as ready, the intent is satisfied without calling the runtime.
    pub async fn reconcile_ready(&self, ready_models: &HashSet<ModelId>) -> Vec<Uuid> {
        let mut queue = self.queue.write().await;
        let mut completed = Vec::new();
        for intent in queue.iter_mut() {
            if intent.state == StagingState::Pending
                && ready_models.contains(&intent.background_model)
            {
                intent.mark_completed();
                completed.push(intent.id);
            }
        }
        completed
    }

    pub async fn unblock_and_queue(
        &self,
        decision_id: Uuid,
        foreground_model: Option<ModelId>,
        background_model: ModelId,
        target_runtime: RuntimeId,
        reason: String,
        pending_context: Option<String>,
    ) {
        let mut intent = StagingIntent::new(
            decision_id,
            foreground_model,
            background_model,
            target_runtime,
            reason,
        );
        intent.pending_context = pending_context;
        intent.mark_pending();
        self.enqueue(intent).await;
    }

    /// On the eviction signal (issue #64): for each `Pending` intent whose
    /// foreground model is now draining, write its declared `pending_context` to
    /// `cache` and record the resulting [`ContextRef`] on the intent. The
    /// eviction window is the right moment to write — the foreground model is no
    /// longer serving, so the snapshot is complete. Intents already committed, or
    /// with no declared context, are skipped. Returns the committed intent ids.
    pub async fn commit_eviction_context(
        &self,
        draining_models: &HashSet<ModelId>,
        cache: &EscalationContextCache,
    ) -> Vec<Uuid> {
        let mut queue = self.queue.write().await;
        let mut committed = Vec::new();
        for intent in queue.iter_mut() {
            if intent.context_ref.is_some() {
                continue;
            }
            let foreground_draining = intent
                .foreground_model
                .as_ref()
                .is_some_and(|model| draining_models.contains(model));
            if !foreground_draining {
                continue;
            }
            if let Some(payload) = intent.pending_context.take() {
                intent.context_ref = Some(cache.write(intent.id, payload));
                committed.push(intent.id);
            }
        }
        committed
    }

    /// The [`ContextRef`] of a committed intent whose `background_model` is
    /// `model`, clearing it from the intent so the context is handed off once.
    /// Used by the escalation read path when the big model becomes the selected
    /// model (issue #64). Returns `None` when no such staged context exists.
    pub async fn take_context_ref_for_background(&self, model: &ModelId) -> Option<ContextRef> {
        let mut queue = self.queue.write().await;
        let intent = queue
            .iter_mut()
            .find(|i| &i.background_model == model && i.context_ref.is_some())?;
        intent.context_ref.take()
    }
}

impl Default for StagingWorker {
    fn default() -> Self {
        Self::new()
    }
}

/// Reference into the [`EscalationContextCache`]. Wraps the staging-intent id the
/// payload is keyed by, so a [`StagingIntent`] can point at its stored context
/// without inlining the payload. Local-only — never transmitted off-machine
/// (issue #64).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextRef(pub Uuid);

/// Default time-to-live (seconds) for a cached escalation context before it is
/// pruned, and the maximum number of entries retained. `chrono::Duration` is not
/// const-constructible, so the TTL is stored as seconds and built in `Default`.
const DEFAULT_ESCALATION_CONTEXT_TTL_SECS: i64 = 600;
const DEFAULT_ESCALATION_CONTEXT_CAPACITY: usize = 256;

#[derive(Debug, Clone)]
struct EscalationContextEntry {
    payload: String,
    stored_at: DateTime<Utc>,
}

/// Bounded, in-memory store of serialized escalation context payloads keyed by
/// staging-intent id (issue #64). The agent declares context up front; on the
/// eviction signal (foreground model draining) the payload is written here, then
/// consumed by the execute/escalation path once the big model is ready.
///
/// Bounded two ways: each entry expires after `ttl`, and the store holds at most
/// `capacity` entries (the oldest is dropped on overflow). Reads consume the
/// entry, so a context is handed off at most once. The payload is local only —
/// never persisted, never transmitted off-machine.
#[derive(Clone)]
pub struct EscalationContextCache {
    entries: Arc<StdRwLock<HashMap<Uuid, EscalationContextEntry>>>,
    ttl: chrono::Duration,
    capacity: usize,
}

impl EscalationContextCache {
    pub fn new(ttl: chrono::Duration, capacity: usize) -> Self {
        Self {
            entries: Arc::new(StdRwLock::new(HashMap::new())),
            ttl,
            capacity: capacity.max(1),
        }
    }

    /// Writes `payload` for `intent_id`, returning the [`ContextRef`] consumers
    /// use to retrieve it. Prunes expired entries and enforces the capacity cap
    /// (evicting the oldest entry) before inserting.
    pub fn write(&self, intent_id: Uuid, payload: String) -> ContextRef {
        self.write_at(intent_id, payload, Utc::now())
    }

    fn write_at(&self, intent_id: Uuid, payload: String, now: DateTime<Utc>) -> ContextRef {
        let mut entries = self.entries.write().unwrap_or_else(PoisonError::into_inner);
        entries.retain(|_, e| now.signed_duration_since(e.stored_at) < self.ttl);
        // Evict oldest-first until there is room. Overwriting an existing id does
        // not grow the map, so only evict when inserting a genuinely new key.
        while entries.len() >= self.capacity && !entries.contains_key(&intent_id) {
            let Some(oldest) = entries
                .iter()
                .min_by_key(|(_, e)| e.stored_at)
                .map(|(id, _)| *id)
            else {
                break;
            };
            entries.remove(&oldest);
        }
        entries.insert(
            intent_id,
            EscalationContextEntry {
                payload,
                stored_at: now,
            },
        );
        ContextRef(intent_id)
    }

    /// Consumes and returns the payload referenced by `context_ref`, or `None`
    /// when it is absent or expired. Reads remove the entry (consume-on-read).
    pub fn take(&self, context_ref: &ContextRef) -> Option<String> {
        self.take_at(context_ref, Utc::now())
    }

    fn take_at(&self, context_ref: &ContextRef, now: DateTime<Utc>) -> Option<String> {
        let mut entries = self.entries.write().unwrap_or_else(PoisonError::into_inner);
        let entry = entries.remove(&context_ref.0)?;
        (now.signed_duration_since(entry.stored_at) < self.ttl).then_some(entry.payload)
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }
}

impl Default for EscalationContextCache {
    fn default() -> Self {
        Self::new(
            chrono::Duration::seconds(DEFAULT_ESCALATION_CONTEXT_TTL_SECS),
            DEFAULT_ESCALATION_CONTEXT_CAPACITY,
        )
    }
}

/// Consolidated operator-facing view of all governance state, derived purely
/// from the reconciled snapshot cache, configuration, staging queue, and
/// decision log. Missing or aged data is labeled explicitly rather than
/// silently omitted — see [`RuntimeStatus::freshness`] and unknown labels.
#[derive(Debug, Clone, Serialize)]
pub struct OperatorStatus {
    /// Whether the reconciliation cache has any inspected runtimes yet.
    pub cache_populated: bool,
    /// One entry per configured runtime, including those never inspected.
    pub runtimes: Vec<RuntimeStatus>,
    /// Health of each configured residency group.
    pub residency_groups: Vec<GroupHealth>,
    /// Active inference requests observed across all runtimes.
    pub active_request_count: usize,
    /// Background staging queue summary.
    pub staging: StagingSummary,
    /// Number of decisions retained in the decision log.
    pub recent_decision_count: usize,
    /// Human-readable policy warnings (e.g. keep-hot group with no hot resident).
    pub policy_warnings: Vec<String>,
    /// Whether `ANEMOI_ENABLE_LIVE_EXECUTE=1` is set (live runtime mutation).
    pub live_execution_enabled: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct RuntimeStatus {
    pub runtime_id: String,
    pub adapter: String,
    /// `"available"`, `"unavailable"`, or `"unknown"` (never inspected).
    pub availability: String,
    /// `"fresh"`, `"stale"`, or `"unknown"` (never inspected).
    pub freshness: String,
    pub last_inspected: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub active_request_count: usize,
    pub residents: Vec<ResidentStatus>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResidentStatus {
    pub model_id: String,
    pub state: ResidencyState,
    /// Idle seconds since load; `None` means idle time is unknown.
    pub idle_secs: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GroupHealth {
    pub group_id: String,
    pub keep_hot: bool,
    pub pinned: bool,
    pub member_count: usize,
    pub hot_resident_count: usize,
    /// `"healthy"`, `"degraded"` (keep-hot group with no hot residents), or
    /// `"unknown"` (no reconciled data yet).
    pub health: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct StagingSummary {
    pub total: usize,
    pub blocked: usize,
    pub pending: usize,
    pub failed: usize,
    pub completed: usize,
}

impl StagingSummary {
    fn from_intents(intents: &[StagingIntent]) -> Self {
        let mut summary = StagingSummary {
            total: intents.len(),
            ..StagingSummary::default()
        };
        for intent in intents {
            match intent.state {
                StagingState::Blocked => summary.blocked += 1,
                StagingState::Pending => summary.pending += 1,
                StagingState::Failed => summary.failed += 1,
                StagingState::Completed => summary.completed += 1,
            }
        }
        summary
    }
}

#[derive(Clone)]
pub struct AppState {
    config: AnemoiConfig,
    scheduler: Scheduler,
    runtimes: HashMap<String, DynRuntimeAdapter>,
    decision_log: DynDecisionLog,
    reconciler: Reconciler,
    staging_worker: StagingWorker,
    /// Store of escalation context payloads awaiting handoff to a background
    /// model (issue #64). Written on the eviction signal, consumed by the
    /// execute path when the big model is ready. Cloning `AppState` shares the
    /// same store (it is `Arc`-backed internally).
    escalation_context: EscalationContextCache,
    /// Live `/api/events` SSE subscribers for llama_swap runtimes. Held only to
    /// keep the background tasks alive for the process lifetime — the tasks are
    /// aborted when the last `AppState` clone drops. Shared via `Arc` so cloning
    /// `AppState` (axum does this per request) does not abort them. Never read:
    /// its sole purpose is ownership, so the SSE tasks live as long as the state.
    #[allow(dead_code)]
    event_streams: Arc<Vec<LlamaSwapEventStream>>,
    /// Monotonic reconciliation tick counter. Stamped into the
    /// `evidence_source` of resident events so each transition records which
    /// inspection round observed it (issue #12).
    reconciliation_round: Arc<AtomicU64>,
    /// Test seam for the live-execute gate. `None` reads the real
    /// `ANEMOI_ENABLE_LIVE_EXECUTE` env var (production); `Some(_)` overrides it
    /// so tests can exercise the gate without mutating process-global state.
    live_execute: Option<bool>,
}

impl AppState {
    pub fn new(config: AnemoiConfig, decision_log: DynDecisionLog) -> anyhow::Result<Self> {
        config.validate()?;
        let scheduler = Scheduler::new(config.clone());
        let mut runtimes = HashMap::new();
        let mut event_streams = Vec::new();

        for (runtime_id, runtime) in &config.runtimes {
            let adapter: DynRuntimeAdapter = match runtime.adapter.as_str() {
                "ollama" => Arc::new(OllamaAdapter::new(
                    runtime_id.clone(),
                    runtime
                        .base_url
                        .as_deref()
                        .unwrap_or("http://localhost:11434"),
                )?),
                "mock" => Arc::new(MockRuntimeAdapter::new(
                    runtime_id.clone(),
                    runtime
                        .initial_residents
                        .clone()
                        .into_iter()
                        .map(|resident| resident.into_resident())
                        .collect(),
                )),
                "llama_swap" => {
                    let adapter = LlamaSwapAdapter::new(
                        runtime_id.clone(),
                        runtime
                            .base_url
                            .as_deref()
                            .unwrap_or("http://localhost:8080"),
                    )?;
                    let adapter = if let Some(token) = &runtime.auth_token {
                        adapter.with_bearer_token(token)
                    } else {
                        adapter
                    };
                    // Read the matrix block from the llama-swap YAML, if a path
                    // is configured, so the adapter can answer colocation
                    // questions. Absent or unset leaves matrix awareness off.
                    let adapter = if let Some(path) = &runtime.config_path {
                        adapter.with_matrix_config_path(path)?
                    } else {
                        adapter
                    };
                    // Subscribe to the push-based residency stream so inspect()
                    // observes real load state. No-op outside a tokio runtime.
                    if let Some(stream) = adapter.start_event_stream() {
                        event_streams.push(stream);
                    }
                    Arc::new(adapter)
                }
                "llama_cpp" | "llama_server" => {
                    let adapter = LlamaCppAdapter::new(
                        runtime_id.clone(),
                        runtime
                            .base_url
                            .as_deref()
                            .unwrap_or("http://localhost:8080"),
                    )?;
                    let adapter = if let Some(token) = &runtime.auth_token {
                        adapter.with_bearer_token(token)
                    } else {
                        adapter
                    };
                    Arc::new(adapter)
                }
                _ => Arc::new(MockRuntimeAdapter::new(runtime_id.clone(), Vec::new())),
            };
            runtimes.insert(runtime_id.to_string(), adapter);
        }

        let reconciler = Reconciler::default();
        let staging_worker = StagingWorker::new();

        Ok(Self {
            config,
            scheduler,
            runtimes,
            decision_log,
            reconciler,
            staging_worker,
            escalation_context: EscalationContextCache::default(),
            event_streams: Arc::new(event_streams),
            reconciliation_round: Arc::new(AtomicU64::new(0)),
            live_execute: None,
        })
    }

    /// Whether forwarding to a non-mock runtime is permitted. Production reads
    /// `ANEMOI_ENABLE_LIVE_EXECUTE=1`; tests can override via
    /// [`AppState::with_live_execute`].
    pub fn live_execute_enabled(&self) -> bool {
        self.live_execute.unwrap_or_else(live_execution_enabled)
    }

    #[cfg(test)]
    fn with_live_execute(mut self, enabled: bool) -> Self {
        self.live_execute = Some(enabled);
        self
    }

    /// Resolves the forward target (base URL + auth token) for a runtime from
    /// config. Returns `None` when the runtime is unknown or has no base URL.
    fn runtime_forward_target(&self, runtime_id: &str) -> Option<anemoi_runtime::ForwardTarget> {
        let config = self
            .config
            .runtimes
            .get(&RuntimeId(runtime_id.to_string()))?;
        let base_url = config.base_url.clone()?;
        Some(anemoi_runtime::ForwardTarget {
            base_url,
            auth_token: config.auth_token.clone(),
        })
    }

    pub fn with_mock_residents(
        config: AnemoiConfig,
        residents: HashMap<String, Vec<ModelResident>>,
    ) -> anyhow::Result<Self> {
        config.validate()?;
        let scheduler = Scheduler::new(config.clone());
        let mut runtimes = HashMap::new();
        for (runtime_id, runtime) in &config.runtimes {
            let adapter: DynRuntimeAdapter = if runtime.adapter == "ollama" {
                Arc::new(OllamaAdapter::new(
                    runtime_id.clone(),
                    runtime
                        .base_url
                        .as_deref()
                        .unwrap_or("http://localhost:11434"),
                )?)
            } else {
                Arc::new(MockRuntimeAdapter::new(
                    runtime_id.clone(),
                    residents
                        .get(&runtime_id.to_string())
                        .cloned()
                        .unwrap_or_default(),
                ))
            };
            runtimes.insert(runtime_id.to_string(), adapter);
        }

        let reconciler = Reconciler::default();
        let staging_worker = StagingWorker::new();

        Ok(Self {
            config,
            scheduler,
            runtimes,
            decision_log: Arc::new(InMemoryDecisionLog::default()),
            reconciler,
            staging_worker,
            escalation_context: EscalationContextCache::default(),
            event_streams: Arc::new(Vec::new()),
            reconciliation_round: Arc::new(AtomicU64::new(0)),
            live_execute: None,
        })
    }

    pub fn reconciler(&self) -> Reconciler {
        self.reconciler.clone()
    }

    pub fn staging_worker(&self) -> StagingWorker {
        self.staging_worker.clone()
    }

    pub async fn snapshots(&self) -> Vec<RuntimeSnapshot> {
        let mut snapshots = Vec::new();
        for runtime in self.runtimes.values() {
            if let Ok(snapshot) = runtime.inspect().await {
                snapshots.push(snapshot);
            }
        }
        snapshots
    }

    /// Build the consolidated operator status view from the reconciled cache,
    /// configuration, staging queue, and decision log. Read-only: never
    /// triggers live runtime inspection.
    pub async fn operator_status(&self) -> OperatorStatus {
        let reconciled = self.reconciler.all().await;
        let cache_populated = !reconciled.is_empty();

        let mut by_runtime: HashMap<String, ReconciledSnapshot> = HashMap::new();
        for r in reconciled {
            by_runtime.insert(r.snapshot.runtime_id.to_string(), r);
        }

        let now = Utc::now();
        let mut runtimes = Vec::new();
        let mut active_request_count = 0usize;

        let mut runtime_ids: Vec<String> =
            self.config.runtimes.keys().map(|k| k.to_string()).collect();
        runtime_ids.sort();

        for runtime_id in runtime_ids {
            let adapter = self
                .runtime_adapter_type(&runtime_id)
                .unwrap_or("unknown")
                .to_string();

            match by_runtime.get(&runtime_id) {
                Some(r) => {
                    let availability = if r.snapshot.available {
                        "available"
                    } else {
                        "unavailable"
                    }
                    .to_string();
                    let freshness = if r.is_stale { "stale" } else { "fresh" }.to_string();
                    let residents = r
                        .snapshot
                        .residents
                        .iter()
                        .map(|res| ResidentStatus {
                            model_id: res.model_id.to_string(),
                            state: res.state.clone(),
                            idle_secs: res
                                .loaded_since
                                .map(|since| (now - since).num_seconds().max(0) as u64),
                        })
                        .collect();
                    let active = r.snapshot.active_requests.len();
                    active_request_count += active;
                    runtimes.push(RuntimeStatus {
                        runtime_id,
                        adapter,
                        availability,
                        freshness,
                        last_inspected: Some(r.last_inspected),
                        last_error: r.last_error.clone(),
                        active_request_count: active,
                        residents,
                    });
                }
                None => {
                    runtimes.push(RuntimeStatus {
                        runtime_id,
                        adapter,
                        availability: "unknown".to_string(),
                        freshness: "unknown".to_string(),
                        last_inspected: None,
                        last_error: None,
                        active_request_count: 0,
                        residents: Vec::new(),
                    });
                }
            }
        }

        let hot_models: std::collections::HashSet<String> = by_runtime
            .values()
            .flat_map(|r| r.snapshot.residents.iter())
            .filter(|res| matches!(res.state, ResidencyState::HotGpu | ResidencyState::Serving))
            .map(|res| res.model_id.to_string())
            .collect();

        let mut residency_groups = Vec::new();
        let mut policy_warnings = Vec::new();
        let mut group_ids: Vec<_> = self.config.residency_groups.keys().cloned().collect();
        group_ids.sort_by_key(|g| g.to_string());
        for group_id in group_ids {
            let group = &self.config.residency_groups[&group_id];
            let hot_resident_count = group
                .models
                .iter()
                .filter(|m| hot_models.contains(&m.to_string()))
                .count();
            let degraded = group.keep_hot && hot_resident_count == 0;
            let health = if !cache_populated {
                "unknown"
            } else if degraded {
                "degraded"
            } else {
                "healthy"
            }
            .to_string();
            if cache_populated && degraded {
                policy_warnings.push(format!("keep-hot group '{group_id}' has no hot residents"));
            }
            residency_groups.push(GroupHealth {
                group_id: group_id.to_string(),
                keep_hot: group.keep_hot,
                pinned: group.pinned,
                member_count: group.models.len(),
                hot_resident_count,
                health,
            });
        }

        let staging = StagingSummary::from_intents(&self.staging_worker.get_all().await);
        let recent_decision_count = self
            .decision_log
            .list_decisions()
            .await
            .map(|d| d.len())
            .unwrap_or(0);

        OperatorStatus {
            cache_populated,
            runtimes,
            residency_groups,
            active_request_count,
            staging,
            recent_decision_count,
            policy_warnings,
            live_execution_enabled: live_execution_enabled(),
        }
    }

    pub async fn decide(&self, request: &InferenceRequest) -> anyhow::Result<Decision> {
        if !self.reconciler.has_cache().await {
            self.run_reconciliation_tick().await;
        }
        let snapshots = self.reconciler.get_snapshots().await;
        let decision = self.scheduler.decide(request, &snapshots)?;

        if decision.action == DecisionAction::StageBackground {
            if let (Some(runtime_id), Some(model_id)) =
                (&decision.selected_runtime, &decision.background_model)
            {
                let reason = decision.explanation.summary.clone();
                let pending_context = request
                    .escalation_intent
                    .as_ref()
                    .and_then(|intent| intent.context.clone());
                self.staging_worker
                    .unblock_and_queue(
                        decision.id,
                        decision.selected_model.clone(),
                        model_id.clone(),
                        runtime_id.clone(),
                        reason,
                        pending_context,
                    )
                    .await;
            }
        }

        self.decision_log.record_decision(&decision).await?;
        Ok(decision)
    }

    pub fn generate_action_plan(&self, decision: &Decision, dry_run: bool) -> ActionPlan {
        let mut plan = ActionPlan::new(decision.id, dry_run);

        match decision.action {
            DecisionAction::ReuseHot => {
                plan.add_noop(
                    decision
                        .selected_runtime
                        .clone()
                        .unwrap_or_else(|| RuntimeId("unknown".to_string())),
                    "Reuse hot model - no action needed".to_string(),
                );
            }
            DecisionAction::PromoteWarm => {
                if let (Some(runtime_id), Some(model_id)) =
                    (&decision.selected_runtime, &decision.selected_model)
                {
                    plan.add_load(
                        runtime_id.clone(),
                        model_id.clone(),
                        true,
                        "Promote warm model to hot".to_string(),
                        decision
                            .selected_model
                            .as_ref()
                            .and_then(|_| self.config.models.get(model_id))
                            .and_then(|p| p.cold_load_estimate_ms),
                    );
                }
            }
            DecisionAction::ColdLoad => {
                if let (Some(runtime_id), Some(model_id)) =
                    (&decision.selected_runtime, &decision.selected_model)
                {
                    plan.add_load(
                        runtime_id.clone(),
                        model_id.clone(),
                        true,
                        "Cold load required model".to_string(),
                        decision
                            .selected_model
                            .as_ref()
                            .and_then(|_| self.config.models.get(model_id))
                            .and_then(|p| p.cold_load_estimate_ms),
                    );
                }
            }
            DecisionAction::StageBackground => {
                if let Some(runtime_id) = &decision.selected_runtime {
                    if let Some(model_id) = &decision.background_model {
                        plan.add_load(
                            runtime_id.clone(),
                            model_id.clone(),
                            false,
                            "Background staging load".to_string(),
                            self.config
                                .models
                                .get(model_id)
                                .and_then(|p| p.cold_load_estimate_ms),
                        );
                    }
                }
            }
            DecisionAction::Downgrade | DecisionAction::Defer | DecisionAction::Deny => {
                plan.add_noop(
                    decision
                        .selected_runtime
                        .clone()
                        .unwrap_or_else(|| RuntimeId("none".to_string())),
                    format!("{:?} - no action", decision.action),
                );
            }
        }

        plan
    }

    pub async fn execute_action_plan(&self, plan: &ActionPlan) -> anyhow::Result<Vec<String>> {
        let mut results = Vec::new();

        if plan.dry_run {
            return Ok(results);
        }

        if !live_execution_enabled() {
            return Err(anyhow::anyhow!(
                "Live execution requires ANEMOI_ENABLE_LIVE_EXECUTE=1"
            ));
        }

        for action in &plan.actions {
            if action.kind == ActionKind::Load {
                if let Some(model_id) = &action.model_id {
                    if let Some(runtime) = self.runtimes.get(&action.runtime_id.to_string()) {
                        match runtime.load_model(model_id).await {
                            Ok(handle) => {
                                results.push(format!(
                                    "Loaded {} on {} (handle: {})",
                                    model_id, action.runtime_id, handle.id
                                ));
                            }
                            Err(e) => {
                                results.push(format!(
                                    "Failed to load {} on {}: {}",
                                    model_id, action.runtime_id, e
                                ));
                            }
                        }
                    }
                }
            }
        }

        Ok(results)
    }

    /// Build an eviction plan from the reconciled cache and configured policy.
    /// Pure read of governance state — never mutates a runtime.
    pub async fn plan_evictions(&self, force: bool) -> EvictionPlan {
        let snapshots = self.reconciler.get_snapshots().await;
        let now = chrono::Utc::now();

        let mut residents = Vec::new();
        for snapshot in &snapshots {
            for resident in &snapshot.residents {
                let (keep_hot, pinned) = self.group_protection(&resident.model_id);
                let serving = snapshot
                    .active_requests
                    .iter()
                    .any(|active| active.model_id == resident.model_id);
                let state = if serving {
                    ResidencyState::Serving
                } else {
                    resident.state.clone()
                };
                let idle_secs = resident
                    .loaded_since
                    .map(|since| (now - since).num_seconds().max(0) as u64);
                residents.push(EvictionCandidateResident {
                    model_id: resident.model_id.clone(),
                    runtime_id: snapshot.runtime_id.clone(),
                    state,
                    keep_hot,
                    pinned,
                    idle_secs,
                });
            }
        }

        anemoi_policy::plan_evictions(&EvictionRequest {
            residents: &residents,
            force,
        })
    }

    fn group_protection(&self, model_id: &ModelId) -> (bool, bool) {
        let mut keep_hot = false;
        let mut pinned = false;
        for group in self.config.residency_groups.values() {
            if group.models.contains(model_id) {
                keep_hot |= group.keep_hot;
                pinned |= group.pinned;
            }
        }
        (keep_hot, pinned)
    }

    /// Execute the unload actions in an eviction plan. Mock runtimes execute
    /// when the plan is approved; non-mock runtimes additionally require
    /// `ANEMOI_ENABLE_LIVE_EXECUTE=1`. Dry-run or unapproved plans do nothing.
    pub async fn execute_eviction_plan(
        &self,
        plan: &ActionPlan,
        approved: bool,
    ) -> anyhow::Result<Vec<String>> {
        let mut results = Vec::new();

        if plan.dry_run || !approved {
            return Ok(results);
        }

        for action in &plan.actions {
            if action.kind != ActionKind::Unload {
                continue;
            }
            let Some(model_id) = &action.model_id else {
                continue;
            };

            let is_mock = self.runtime_adapter_type(&action.runtime_id.to_string()) == Some("mock");
            if !is_mock && !live_execution_enabled() {
                return Err(anyhow::anyhow!(
                    "Live eviction requires ANEMOI_ENABLE_LIVE_EXECUTE=1"
                ));
            }

            if let Some(runtime) = self.runtimes.get(&action.runtime_id.to_string()) {
                runtime.unload_model(model_id).await?;
                results.push(format!("Unloaded {} on {}", model_id, action.runtime_id));
            }
        }

        Ok(results)
    }

    pub fn runtime_adapter_type(&self, runtime_id: &str) -> Option<&str> {
        self.config
            .runtimes
            .get(&RuntimeId(runtime_id.to_string()))
            .map(|config| config.adapter.as_str())
    }

    pub async fn run_reconciliation_tick(&self) {
        let round = self.reconciliation_round.fetch_add(1, Ordering::Relaxed) + 1;
        for (runtime_id, adapter) in &self.runtimes {
            match adapter.inspect().await {
                Ok(snapshot) => {
                    let previous = self.reconciler.get(runtime_id).await;
                    let prior_residents = previous
                        .as_ref()
                        .map(|p| p.snapshot.residents.as_slice())
                        .unwrap_or(&[]);
                    self.emit_resident_transitions(
                        runtime_id,
                        prior_residents,
                        &snapshot.residents,
                        round,
                    )
                    .await;
                    self.reconciler.update(runtime_id, snapshot).await;
                }
                Err(e) => {
                    self.reconciler
                        .record_error(runtime_id, e.to_string())
                        .await;
                }
            }
        }
    }

    /// Models the reconciliation cache currently reports as hot/serving — i.e.
    /// ready to use. The residency comes from the push-updated `/api/events` SSE
    /// cache by way of the last `inspect()` snapshots.
    async fn ready_models(&self) -> HashSet<ModelId> {
        self.reconciler
            .get_snapshots()
            .await
            .into_iter()
            .flat_map(|snapshot| snapshot.residents)
            .filter(|resident| {
                matches!(
                    resident.state,
                    ResidencyState::HotGpu | ResidencyState::Serving
                )
            })
            .map(|resident| resident.model_id)
            .collect()
    }

    /// Models the reconciliation cache reports as draining/evicting — i.e. in the
    /// eviction window (llama-swap `stopping`, mapped to
    /// [`ResidencyState::Draining`]). This is the signal to snapshot escalation
    /// context before the foreground model stops serving (issue #64).
    async fn draining_models(&self) -> HashSet<ModelId> {
        self.reconciler
            .get_snapshots()
            .await
            .into_iter()
            .flat_map(|snapshot| snapshot.residents)
            .filter(|resident| {
                matches!(
                    resident.state,
                    ResidencyState::Draining | ResidencyState::Evicting
                )
            })
            .map(|resident| resident.model_id)
            .collect()
    }

    /// Records resident transitions for one runtime into the durable event
    /// store (issue #12). Reconciliation is observation-only, so every
    /// transition is recorded with `decision_id = None`; the `evidence_source`
    /// names the adapter and inspection round so no transition is anonymous.
    /// Recording is best-effort — a telemetry error must not abort the tick.
    async fn emit_resident_transitions(
        &self,
        runtime_id: &str,
        previous: &[ModelResident],
        current: &[ModelResident],
        round: u64,
    ) {
        let transitions = resident_transitions(previous, current);
        if transitions.is_empty() {
            return;
        }
        let adapter = self.runtime_adapter_type(runtime_id).unwrap_or("unknown");
        let evidence = format!("{adapter} reconciliation round {round}");
        let observed_at = Utc::now();
        for transition in &transitions {
            let note = transition
                .first_observation
                .then_some("first observation; prior state inferred as cold");
            let _ = self
                .decision_log
                .record_resident_transition(ResidentTransitionRecord {
                    model_id: transition.model_id.0.as_str(),
                    runtime_id,
                    from_state: transition.from_state.clone(),
                    to_state: transition.to_state.clone(),
                    observed_at,
                    evidence_source: &evidence,
                    decision_id: None,
                    note,
                })
                .await;
        }
    }

    pub async fn run_staging_tick(&self) {
        // Eviction window (issue #64): a draining foreground model is no longer
        // serving, so its staged context snapshot is complete — write it to the
        // escalation cache before the model stops. In-memory only, so this runs
        // regardless of the live-execute gate.
        let draining_models = self.draining_models().await;
        if !draining_models.is_empty() {
            self.staging_worker
                .commit_eviction_context(&draining_models, &self.escalation_context)
                .await;
        }

        // A staged model the readiness stream already reports as ready needs no
        // runtime call — complete those intents first (issue #62).
        let ready_models = self.ready_models().await;
        if !ready_models.is_empty() {
            self.staging_worker.reconcile_ready(&ready_models).await;
        }

        let pending = self.staging_worker.get_pending().await;
        for intent in pending {
            let runtime_id = intent.target_runtime.0.clone();
            let is_mock = self.runtime_adapter_type(&runtime_id) == Some("mock");
            if !is_mock && !live_execution_enabled() {
                continue;
            }
            if let Some(adapter) = self.runtimes.get(&runtime_id) {
                match adapter.load_model(&intent.background_model).await {
                    Ok(_) => {
                        self.staging_worker
                            .update_state(intent.id, StagingState::Completed, None)
                            .await;
                    }
                    Err(e) => {
                        self.staging_worker
                            .update_state(intent.id, StagingState::Failed, Some(e.to_string()))
                            .await;
                    }
                }
            }
        }
    }

    /// Consumes any escalation context staged for `model` and returns it as the
    /// initial-prompt payload for the big model's first inference call (issue
    /// #64). Finds a committed staging intent whose `background_model` is `model`,
    /// takes its `context_ref`, and consumes the payload from the cache. Returns
    /// `None` when no context was staged for this model.
    async fn take_escalation_context(&self, model: &ModelId) -> Option<String> {
        let context_ref = self
            .staging_worker
            .take_context_ref_for_background(model)
            .await?;
        self.escalation_context.take(&context_ref)
    }
}

fn live_execution_enabled() -> bool {
    std::env::var("ANEMOI_ENABLE_LIVE_EXECUTE").as_deref() == Ok("1")
}

#[cfg(test)]
mod tests {
    use super::*;
    use anemoi_core::{
        DecisionAction, DomainId, EscalationIntent, ExecutionMode, ModelId, ModelProfileConfig,
        ModelResident, RequestId, ResidencyState, RuntimeConfig, RuntimeId,
    };
    use anemoi_telemetry::{decision_log_from, DecisionLog, InMemoryDecisionLog, SqliteEventStore};
    use axum::body::{to_bytes, Body};
    use axum::http::{Method, Request};
    use serde_json::Value;
    use std::collections::HashMap;
    use tower::ServiceExt;

    // anemoi-guard:allow vacuous-test - construction smoke test; richer
    // no-database behavior is covered by cli_decide_works_without_database_url
    #[test]
    fn daemon_starts_without_database_url() {
        let config = example_config();

        let state = AppState::new(config, Arc::new(InMemoryDecisionLog::default()));

        assert!(state.is_ok());
    }

    #[tokio::test]
    async fn daemon_starts_with_memory_store_when_database_url_is_missing() {
        // With no database URL, the daemon wires an in-memory log: a decision it
        // records is retrievable from that same log instance.
        let log = decision_log_from(None, None).expect("memory log");
        let state = AppState::new(example_config(), log.clone()).expect("state");

        let decision = state.decide(&sample_request()).await.expect("decide");
        let found = log
            .get_decision(decision.id)
            .await
            .expect("get")
            .expect("decision is recorded in the memory log");
        assert_eq!(found, decision);
    }

    #[tokio::test]
    async fn daemon_uses_sqlite_store_when_database_url_is_present() {
        // A `sqlite://` URL routes the daemon through the same code path the
        // real binary uses (default_decision_log -> decision_log_from), and a
        // decision it records survives a process "restart" (reopen of the file).
        let temp_db_path = std::env::temp_dir().join(format!("anemoi-test-{}.db", Uuid::new_v4()));
        let url = format!("sqlite:///{}", temp_db_path.display());

        let decision = {
            let log = decision_log_from(Some(&url), None).expect("sqlite log");
            let state = AppState::new(example_config(), log).expect("state");
            state.decide(&sample_request()).await.expect("decide")
        };

        // Reopen the SQLite file fresh: the decision is durable across restart.
        let reopened = SqliteEventStore::create(&temp_db_path).expect("reopen sqlite store");
        let found = reopened
            .get_decision(decision.id)
            .await
            .expect("get")
            .expect("decision is durable in the SQLite store");
        assert_eq!(found, decision);

        let _ = std::fs::remove_file(&temp_db_path);
    }

    #[tokio::test]
    async fn health_returns_ok() {
        let response = test_router()
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(json_body(response).await["ok"], true);
    }

    #[tokio::test]
    async fn status_returns_operator_summary() {
        let response = test_router()
            .oneshot(
                Request::builder()
                    .uri("/status")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        let body = json_body(response).await;

        assert!(body["runtimes"].is_array());
        assert!(body["residency_groups"].is_array());
        assert!(body["staging"].is_object());
        assert!(body["recent_decision_count"].is_number());
        assert!(body["live_execution_enabled"].is_boolean());
    }

    #[tokio::test]
    async fn residents_returns_runtime_snapshots() {
        let mut residents = HashMap::new();
        residents.insert(
            "mock".to_string(),
            vec![ModelResident {
                model_id: ModelId("qwen9b".to_string()),
                state: ResidencyState::HotGpu,
                vram_mb: Some(9000),
                ram_mb: None,
                kv_cache_mb: None,
                loaded_since: None,
            }],
        );
        let state = AppState::with_mock_residents(example_config(), residents).expect("state");

        let response = router(state)
            .oneshot(
                Request::builder()
                    .uri("/residents")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        let snapshots: Vec<RuntimeSnapshot> =
            serde_json::from_value(json_body(response).await).expect("snapshots");

        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].runtime_id.to_string(), "mock");
        assert_eq!(snapshots[0].residents.len(), 1);
    }

    #[tokio::test]
    async fn decide_returns_structured_decision() {
        let response = test_router()
            .oneshot(json_request("/decide", &sample_request()))
            .await
            .expect("response");
        let decision: Decision =
            serde_json::from_value(json_body(response).await).expect("decision");

        assert!(decision.selected_model.is_some());
        assert!(decision.selected_runtime.is_some());
        assert!(!decision.explanation.summary.is_empty());
        assert!(!decision.score.contributions.is_empty());
    }

    #[tokio::test]
    async fn decide_records_decision_in_log() {
        let log = Arc::new(InMemoryDecisionLog::default());
        let state = AppState::new(example_config(), log.clone()).expect("state");

        let response = router(state)
            .oneshot(json_request("/decide", &sample_request()))
            .await
            .expect("response");
        let decision: Decision =
            serde_json::from_value(json_body(response).await).expect("decision");

        assert_eq!(
            log.get_decision(decision.id).await.expect("decision log"),
            Some(decision)
        );
    }

    #[tokio::test]
    async fn explain_returns_recorded_explanation() {
        let app = test_router();
        let response = app
            .clone()
            .oneshot(json_request("/decide", &sample_request()))
            .await
            .expect("decide response");
        let decision: Decision =
            serde_json::from_value(json_body(response).await).expect("decision");

        let explain_response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/explain/{}", decision.id))
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("explain response");
        let explanation: anemoi_core::Explanation =
            serde_json::from_value(json_body(explain_response).await).expect("explanation");

        assert_eq!(explanation, decision.explanation);
    }

    #[tokio::test]
    async fn explain_returns_not_found_for_unknown_decision() {
        let response = test_router()
            .oneshot(
                Request::builder()
                    .uri(format!("/explain/{}", Uuid::new_v4()))
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn execute_returns_honest_handoff_response() {
        let response = test_router()
            .oneshot(json_request("/execute", &sample_request()))
            .await
            .expect("response");
        let execute: ExecuteResponse =
            serde_json::from_value(json_body(response).await).expect("execute response");

        assert!(!execute.handoff.full_inference_forwarded);
        assert!(execute.decision.selected_model.is_some());
        assert!(!execute.action_plan.actions.is_empty() || execute.action_plan.dry_run);
    }

    #[tokio::test]
    async fn live_smoke_decide_uses_runtime_snapshot_without_execute() {
        let response = test_router()
            .oneshot(json_request("/decide", &sample_request()))
            .await
            .expect("response");
        let decision: Decision =
            serde_json::from_value(json_body(response).await).expect("decision");

        // Decision is based on a runtime snapshot.
        assert!(
            decision.selected_runtime.is_some(),
            "decide must use runtime snapshot to select a runtime"
        );
        // The /decide endpoint does not execute inference.
        assert_ne!(decision.action, DecisionAction::Deny);
    }

    #[tokio::test]
    async fn live_smoke_decision_records_runtime_evidence_source() {
        let log = Arc::new(InMemoryDecisionLog::default());
        let state = AppState::new(example_config(), log.clone()).expect("state");

        let decision = state.decide(&sample_request()).await.expect("decision");

        // The decision records which runtime was selected based on snapshot evidence.
        assert!(
            decision.selected_runtime.is_some(),
            "decision must record selected runtime"
        );
        let recorded = log
            .get_decision(decision.id)
            .await
            .expect("get decision")
            .expect("decision exists");
        assert_eq!(recorded.selected_runtime, decision.selected_runtime);
    }

    #[tokio::test]
    async fn live_smoke_explanation_includes_latency_and_residency_reasons() {
        let response = test_router()
            .oneshot(json_request("/decide", &sample_request()))
            .await
            .expect("response");
        let decision: Decision =
            serde_json::from_value(json_body(response).await).expect("decision");

        let reason_details: Vec<&str> = decision
            .explanation
            .reasons
            .iter()
            .map(|reason| reason.detail.as_str())
            .collect();
        let all_reasons = reason_details.join(" ");

        assert!(
            all_reasons.contains("latency"),
            "explanation should mention latency budget: {}",
            all_reasons
        );
        assert!(
            all_reasons.contains("residency") || all_reasons.contains("hot"),
            "explanation should mention residency: {}",
            all_reasons
        );
    }

    #[tokio::test]
    async fn openapi_document_is_served() {
        let response = test_router()
            .oneshot(
                Request::builder()
                    .uri("/openapi.json")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        let document = json_body(response).await;

        assert_eq!(document["openapi"], "3.0.3");
        assert_eq!(document["info"]["title"], "Anemoi Local Governance API");
    }

    #[test]
    fn openapi_document_includes_health_status_residents_decide_execute() {
        let document = openapi_document();
        let paths = document["paths"].as_object().expect("paths");

        for path in ["/health", "/status", "/residents", "/decide", "/execute"] {
            assert!(paths.contains_key(path), "missing path {path}");
        }
    }

    #[test]
    fn openapi_decide_schema_matches_decision_response() {
        let document = openapi_document();

        assert_eq!(
            document["paths"]["/decide"]["post"]["requestBody"]["content"]["application/json"]
                ["schema"]["$ref"],
            "#/components/schemas/InferenceRequest"
        );
        assert_eq!(
            document["paths"]["/decide"]["post"]["responses"]["200"]["content"]["application/json"]
                ["schema"]["$ref"],
            "#/components/schemas/Decision"
        );
        assert!(document["components"]["schemas"]["Decision"]["required"]
            .as_array()
            .expect("required")
            .iter()
            .any(|field| field == "explanation"));
    }

    #[test]
    fn openapi_explain_schema_matches_explanation_response() {
        let document = openapi_document();

        assert_eq!(
            document["paths"]["/explain/{id}"]["get"]["responses"]["200"]["content"]
                ["application/json"]["schema"]["$ref"],
            "#/components/schemas/Explanation"
        );
        assert!(document["components"]["schemas"]["Explanation"]["required"]
            .as_array()
            .expect("required")
            .iter()
            .any(|field| field == "summary"));
    }

    #[test]
    fn openapi_contract_serializes_without_panic() {
        let document = openapi_document();
        let text = serde_json::to_string(&document).expect("serialize contract");

        assert!(text.contains("/execute"));
        assert!(text.contains("full_inference_forwarded"));
    }

    #[tokio::test]
    async fn live_execute_requires_explicit_enable_flag() {
        // Create a config with a non-mock runtime adapter.
        let mut config = example_config();
        config.runtimes.insert(
            RuntimeId("live_target".to_string()),
            RuntimeConfig {
                adapter: "llama_swap".to_string(),
                base_url: Some("http://127.0.0.1:8085".to_string()),
                auth_token: None,
                initial_residents: Vec::new(),
                config_path: None,
            },
        );
        // Add a model that references the new runtime.
        config.models.insert(
            ModelId("test_model".to_string()),
            ModelProfileConfig {
                family: "test".to_string(),
                parameter_class: "7b".to_string(),
                context_window: None,
                vram_required_mb: None,
                ram_required_mb: None,
                cold_load_estimate_ms: None,
                supported_runtimes: vec![RuntimeId("live_target".to_string())],
                supports_streaming: None,
            },
        );

        let state =
            AppState::new(config, Arc::new(InMemoryDecisionLog::default())).expect("app state");
        let app = router(state);

        let response = app
            .oneshot(json_request("/execute", &sample_request()))
            .await
            .expect("response");
        let execute: ExecuteResponse =
            serde_json::from_value(json_body(response).await).expect("execute response");

        // Without ANEMOI_ENABLE_LIVE_EXECUTE=1, non-mock runtime load is skipped.
        assert!(!execute.decision.explanation.summary.is_empty());
    }

    #[tokio::test]
    async fn live_execute_rejects_without_runtime_target() {
        let request = InferenceRequest {
            id: RequestId::new(),
            domain: DomainId("unknown".to_string()),
            mode: ExecutionMode::Interactive,
            prompt_tokens_estimate: None,
            max_output_tokens: None,
            latency_budget_ms: None,
            quality_floor: None,
            escalation_intent: None,
        };

        let response = test_router()
            .oneshot(json_request("/execute", &request))
            .await
            .expect("response");

        // The domain is unknown, so the scheduler should return a deny decision.
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn live_execute_returns_handoff_metadata_without_forwarding_by_default() {
        let response = test_router()
            .oneshot(json_request("/execute", &sample_request()))
            .await
            .expect("response");
        let execute: ExecuteResponse =
            serde_json::from_value(json_body(response).await).expect("execute response");

        assert!(
            !execute.handoff.full_inference_forwarded,
            "execute must report full_inference_forwarded as false"
        );
        assert!(
            execute.handoff.message.contains("model-load handoff only"),
            "execute must describe v1 limitation: {}",
            execute.handoff.message
        );
    }

    #[tokio::test]
    async fn live_execute_records_mutating_action_in_decision_explanation() {
        let log = Arc::new(InMemoryDecisionLog::default());
        let state = AppState::new(example_config(), log.clone()).expect("state");

        let response = router(state)
            .oneshot(json_request("/execute", &sample_request()))
            .await
            .expect("response");
        let execute: ExecuteResponse =
            serde_json::from_value(json_body(response).await).expect("execute response");

        // The decision is recorded and the explanation includes reasons.
        let recorded = log
            .get_decision(execute.decision.id)
            .await
            .expect("get decision")
            .expect("decision exists");
        assert!(!recorded.explanation.reasons.is_empty());
    }

    fn test_router() -> Router {
        router(
            AppState::new(example_config(), Arc::new(InMemoryDecisionLog::default()))
                .expect("state"),
        )
    }

    fn json_request<T: Serialize>(uri: &str, value: &T) -> Request<Body> {
        Request::builder()
            .method(Method::POST)
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(value).expect("json")))
            .expect("request")
    }

    async fn json_body(response: axum::response::Response) -> Value {
        assert!(
            response.status().is_success(),
            "unexpected response status {}",
            response.status()
        );
        let bytes = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body bytes");
        serde_json::from_slice(&bytes).expect("json body")
    }

    fn sample_request() -> InferenceRequest {
        InferenceRequest {
            id: RequestId::new(),
            domain: DomainId("coding".to_string()),
            mode: ExecutionMode::Interactive,
            prompt_tokens_estimate: Some(1000),
            max_output_tokens: Some(500),
            latency_budget_ms: Some(1500),
            quality_floor: None,
            escalation_intent: None,
        }
    }

    fn example_config() -> AnemoiConfig {
        let config_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("config")
            .join("anemoi.example.yaml");
        AnemoiConfig::from_yaml_file(config_path).expect("example config")
    }

    #[tokio::test]
    async fn reconciliation_loop_updates_snapshot_cache_from_runtime_inspect() {
        let state = AppState::new(example_config(), Arc::new(InMemoryDecisionLog::default()))
            .expect("state");

        state.run_reconciliation_tick().await;

        let reconciler = state.reconciler();
        let has_cache = reconciler.has_cache().await;
        assert!(has_cache, "reconciler should have cache after tick");

        let snapshots = reconciler.get_snapshots().await;
        assert!(
            !snapshots.is_empty(),
            "should have at least one snapshot after tick"
        );
    }

    #[tokio::test]
    async fn reconciliation_loop_marks_snapshot_stale_after_ttl() {
        let reconciler = Reconciler::new(1);

        let snapshot = RuntimeSnapshot {
            runtime_id: RuntimeId("test".to_string()),
            available: true,
            residents: vec![],
            configured_models: Vec::new(),
            memory: anemoi_core::RuntimeMemorySnapshot::default(),
            active_requests: vec![],
        };
        reconciler.update("test", snapshot).await;

        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

        let stale = reconciler.get("test").await.expect("snapshot should exist");
        assert!(
            stale.is_stale,
            "snapshot should be stale after TTL exceeded"
        );
    }

    #[tokio::test]
    async fn reconciliation_loop_records_runtime_inspection_error_without_panicking() {
        let reconciler = Reconciler::new(5000);

        reconciler
            .record_error("test_runtime", "connection refused".to_string())
            .await;

        let snapshot = reconciler.get("test_runtime").await;
        assert!(snapshot.is_some(), "error record should exist in cache");
        let snapshot = snapshot.unwrap();
        assert!(snapshot.last_error.is_some(), "error should be recorded");
        assert_eq!(snapshot.last_error.unwrap(), "connection refused");
        assert!(
            !snapshot.snapshot.available,
            "snapshot should be marked unavailable"
        );
    }

    #[test]
    fn resident_transitions_detects_first_observation_change_and_skips_unchanged() {
        let model = |id: &str, state: ResidencyState| ModelResident {
            model_id: ModelId(id.to_string()),
            state,
            vram_mb: None,
            ram_mb: None,
            kv_cache_mb: None,
            loaded_since: None,
        };

        // First observation: no prior snapshot → from_state inferred as Cold.
        let first = resident_transitions(&[], &[model("qwen9b", ResidencyState::HotGpu)]);
        assert_eq!(
            first,
            vec![ResidentTransitionDetail {
                model_id: ModelId("qwen9b".to_string()),
                from_state: ResidencyState::Cold,
                to_state: ResidencyState::HotGpu,
                first_observation: true,
            }]
        );

        // State change between ticks: from_state is the observed prior state.
        let changed = resident_transitions(
            &[model("qwen9b", ResidencyState::Loading)],
            &[model("qwen9b", ResidencyState::HotGpu)],
        );
        assert_eq!(
            changed,
            vec![ResidentTransitionDetail {
                model_id: ModelId("qwen9b".to_string()),
                from_state: ResidencyState::Loading,
                to_state: ResidencyState::HotGpu,
                first_observation: false,
            }]
        );

        // Unchanged state records no transition (absence is the behavior).
        let unchanged = resident_transitions(
            &[model("qwen9b", ResidencyState::HotGpu)],
            &[model("qwen9b", ResidencyState::HotGpu)],
        );
        assert!(unchanged.is_empty());

        // A vanished resident records no transition: its target state is not
        // observed, so inferring one would be speculative.
        let vanished = resident_transitions(&[model("qwen9b", ResidencyState::HotGpu)], &[]);
        assert!(vanished.is_empty());
    }

    #[tokio::test]
    async fn reconciliation_tick_records_resident_transition() {
        // Issue #12: the reconciliation loop emits resident_events reachable
        // from the daemon (not just the telemetry unit test). The example
        // config's mock runtime starts with qwen9b hot_gpu, so the first tick
        // records a single cold → hot_gpu transition; a second tick with no
        // change records nothing.
        let db_path =
            std::env::temp_dir().join(format!("anemoi-resident-events-{}.db", Uuid::new_v4()));
        let store = Arc::new(SqliteEventStore::create(&db_path).expect("sqlite event store"));
        let state = AppState::new(example_config(), store.clone()).expect("state");

        state.run_reconciliation_tick().await;

        let events = store.resident_events("qwen9b").expect("resident events");
        assert_eq!(events.len(), 1, "first tick records one transition");
        let event = &events[0];
        assert_eq!(event.from_state, "cold");
        assert_eq!(event.to_state, "hot_gpu");
        assert_eq!(event.runtime_id, "mock");
        assert_eq!(
            event.decision_id, None,
            "reconciliation is observation-only"
        );
        assert_eq!(
            event.note.as_deref(),
            Some("first observation; prior state inferred as cold")
        );
        assert!(
            event.evidence_source.contains("reconciliation round 1"),
            "evidence names the inspection round, got {:?}",
            event.evidence_source
        );

        // Second tick: qwen9b is unchanged, so no new transition is recorded.
        state.run_reconciliation_tick().await;
        let events = store.resident_events("qwen9b").expect("resident events");
        assert_eq!(events.len(), 1, "unchanged state records no new transition");

        let _ = std::fs::remove_file(&db_path);
    }

    #[tokio::test]
    async fn status_uses_reconciled_snapshot_when_available() {
        let state = AppState::new(example_config(), Arc::new(InMemoryDecisionLog::default()))
            .expect("state");

        state.run_reconciliation_tick().await;

        let reconciler = state.reconciler();
        assert!(
            reconciler.has_cache().await,
            "cache should exist after tick"
        );

        let snapshots = reconciler.get_snapshots().await;
        assert!(
            !snapshots.is_empty(),
            "reconciled snapshots should be available"
        );
    }

    #[tokio::test]
    async fn status_summary_includes_runtime_availability_and_staleness() {
        let state = AppState::new(example_config(), Arc::new(InMemoryDecisionLog::default()))
            .expect("state");

        // Before any inspection, availability and freshness are explicitly
        // unknown — never silently omitted.
        let cold = state.operator_status().await;
        assert!(!cold.cache_populated);
        let mock_cold = cold
            .runtimes
            .iter()
            .find(|r| r.runtime_id == "mock")
            .expect("mock runtime listed even when uninspected");
        assert_eq!(mock_cold.availability, "unknown");
        assert_eq!(mock_cold.freshness, "unknown");
        assert!(mock_cold.last_inspected.is_none());

        state.run_reconciliation_tick().await;
        let fresh = state.operator_status().await;
        let mock_fresh = fresh
            .runtimes
            .iter()
            .find(|r| r.runtime_id == "mock")
            .expect("mock runtime");
        assert_eq!(mock_fresh.availability, "available");
        assert_eq!(mock_fresh.freshness, "fresh");
        assert!(mock_fresh.last_inspected.is_some());

        state.reconciler().mark_stale().await;
        let aged = state.operator_status().await;
        let mock_aged = aged
            .runtimes
            .iter()
            .find(|r| r.runtime_id == "mock")
            .expect("mock runtime");
        assert_eq!(mock_aged.freshness, "stale");
    }

    #[tokio::test]
    async fn status_summary_includes_residency_group_health() {
        let state = AppState::new(example_config(), Arc::new(InMemoryDecisionLog::default()))
            .expect("state");
        state.run_reconciliation_tick().await;

        let status = state.operator_status().await;
        assert_eq!(status.residency_groups.len(), 2);

        let small = status
            .residency_groups
            .iter()
            .find(|g| g.group_id == "small_swarm")
            .expect("small_swarm group");
        assert!(small.keep_hot);
        // qwen9b is hot_gpu in the mock runtime and belongs to small_swarm.
        assert!(small.hot_resident_count >= 1);
        assert_eq!(small.health, "healthy");

        let large = status
            .residency_groups
            .iter()
            .find(|g| g.group_id == "large_models")
            .expect("large_models group");
        assert!(!large.keep_hot);
        assert_eq!(large.hot_resident_count, 0);
        // Not keep-hot, so zero hot residents is still healthy.
        assert_eq!(large.health, "healthy");
    }

    #[tokio::test]
    async fn status_summary_includes_recent_decision_count() {
        let state = AppState::new(example_config(), Arc::new(InMemoryDecisionLog::default()))
            .expect("state");

        let before = state.operator_status().await;
        assert_eq!(before.recent_decision_count, 0);

        state.decide(&sample_request()).await.expect("decision");

        let after = state.operator_status().await;
        assert_eq!(after.recent_decision_count, 1);
    }

    #[tokio::test]
    async fn decide_uses_reconciled_snapshot_without_reinspecting_when_fresh() {
        let state = AppState::new(example_config(), Arc::new(InMemoryDecisionLog::default()))
            .expect("state");

        state.run_reconciliation_tick().await;

        let reconciler = state.reconciler();
        assert!(
            reconciler.has_cache().await,
            "cache should exist after tick"
        );

        let decision = state.decide(&sample_request()).await.expect("decision");
        assert!(
            decision.selected_runtime.is_some(),
            "decision should use reconciled snapshot"
        );
    }

    #[tokio::test]
    async fn stage_background_decision_enqueues_staging_intent() {
        let mut config = example_config();
        config.continuity.background_load = true;
        config.continuity.keep_small_worker_hot = true;

        let state = AppState::new(config, Arc::new(InMemoryDecisionLog::default())).expect("state");

        let staging_worker = state.staging_worker();
        assert!(
            staging_worker.get_all().await.is_empty(),
            "staging queue should be empty initially"
        );

        state.decide(&sample_request()).await.expect("decision");

        let intents = staging_worker.get_all().await;
        assert!(
            !intents.is_empty(),
            "StageBackground decision should enqueue staging intent"
        );
    }

    #[tokio::test]
    async fn staging_worker_does_not_mutate_live_runtime_without_enable_flag() {
        let config = example_config();
        let state = AppState::new(config, Arc::new(InMemoryDecisionLog::default())).expect("state");

        let staging_worker = state.staging_worker();

        let intent = StagingIntent::new(
            Uuid::new_v4(),
            Some(ModelId("qwen9b".to_string())),
            ModelId("qwen35_a3b".to_string()),
            RuntimeId("mock".to_string()),
            "background staging test".to_string(),
        );
        staging_worker.enqueue(intent).await;

        staging_worker
            .execute_pending(state.runtimes.get("mock").expect("mock runtime"))
            .await
            .expect("execute should not panic");

        let intents = staging_worker.get_all().await;
        assert!(
            !intents.is_empty(),
            "intents should still exist after execution attempt"
        );
    }

    #[tokio::test]
    async fn staging_worker_can_load_model_on_mock_runtime() {
        let config = example_config();
        let state = AppState::new(config, Arc::new(InMemoryDecisionLog::default())).expect("state");

        let staging_worker = state.staging_worker();

        let mut intent = StagingIntent::new(
            Uuid::new_v4(),
            Some(ModelId("qwen9b".to_string())),
            ModelId("qwen35_a3b".to_string()),
            RuntimeId("mock".to_string()),
            "background staging test".to_string(),
        );
        intent.mark_pending();
        staging_worker.enqueue(intent).await;

        let mock_runtime = state.runtimes.get("mock").expect("mock runtime");

        staging_worker
            .execute_pending(mock_runtime)
            .await
            .expect("execute should succeed");

        let intents = staging_worker.get_all().await;
        let completed = intents
            .iter()
            .find(|i| i.state == StagingState::Completed)
            .expect("staging should complete on mock runtime");
        assert_eq!(
            completed.background_model,
            ModelId("qwen35_a3b".to_string())
        );
    }

    #[tokio::test]
    async fn execute_pending_skips_live_runtime_without_enable_flag() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        // A live (non-mock) adapter that records every load_model call. It wraps a
        // mock for the other trait methods but does NOT override is_mock(), so it
        // presents as a real runtime — exactly the case the gate must refuse.
        struct LoadCountingAdapter {
            inner: DynRuntimeAdapter,
            loads: Arc<AtomicUsize>,
        }

        #[async_trait::async_trait]
        impl anemoi_runtime::RuntimeAdapter for LoadCountingAdapter {
            fn id(&self) -> RuntimeId {
                self.inner.id()
            }
            async fn inspect(&self) -> Result<RuntimeSnapshot, anemoi_runtime::RuntimeError> {
                self.inner.inspect().await
            }
            async fn load_model(
                &self,
                model: &ModelId,
            ) -> Result<anemoi_runtime::LoadHandle, anemoi_runtime::RuntimeError> {
                self.loads.fetch_add(1, Ordering::SeqCst);
                self.inner.load_model(model).await
            }
            async fn unload_model(
                &self,
                model: &ModelId,
            ) -> Result<(), anemoi_runtime::RuntimeError> {
                self.inner.unload_model(model).await
            }
            async fn execute(
                &self,
                request: anemoi_core::ExecutionRequest,
            ) -> Result<anemoi_runtime::ExecutionHandle, anemoi_runtime::RuntimeError> {
                self.inner.execute(request).await
            }
        }

        let loads = Arc::new(AtomicUsize::new(0));
        let runtime: DynRuntimeAdapter = Arc::new(LoadCountingAdapter {
            inner: Arc::new(MockRuntimeAdapter::new(
                RuntimeId("llama_swap".to_string()),
                Vec::new(),
            )),
            loads: loads.clone(),
        });
        assert!(
            !runtime.is_mock(),
            "spy adapter must present as a live runtime for this gate test"
        );

        let worker = StagingWorker::new();
        let mut intent = StagingIntent::new(
            Uuid::new_v4(),
            Some(ModelId("qwen9b".to_string())),
            ModelId("qwen35_a3b".to_string()),
            runtime.id(),
            "background staging test".to_string(),
        );
        intent.mark_pending();
        let intent_id = intent.id;
        worker.enqueue(intent).await;

        worker
            .execute_pending(&runtime)
            .await
            .expect("execute_pending should not error when gated");

        assert_eq!(
            loads.load(Ordering::SeqCst),
            0,
            "load_model must NOT be called on a non-mock runtime without ANEMOI_ENABLE_LIVE_EXECUTE"
        );

        let intent = worker
            .get_all()
            .await
            .into_iter()
            .find(|i| i.id == intent_id)
            .expect("intent should still be queued");
        assert_eq!(
            intent.state,
            StagingState::Pending,
            "a gated intent must stay Pending (not dead-ended) so the readiness \
             path can still complete it — matching run_staging_tick"
        );
    }

    #[tokio::test]
    async fn staging_status_reports_pending_blocked_failed_and_completed() {
        let config = example_config();
        let state = AppState::new(config, Arc::new(InMemoryDecisionLog::default())).expect("state");

        let staging_worker = state.staging_worker();

        let blocked_intent = StagingIntent::new(
            Uuid::new_v4(),
            Some(ModelId("qwen9b".to_string())),
            ModelId("qwen35_a3b".to_string()),
            RuntimeId("mock".to_string()),
            "blocked test".to_string(),
        );
        staging_worker.enqueue(blocked_intent).await;

        let mut pending_intent = StagingIntent::new(
            Uuid::new_v4(),
            Some(ModelId("qwen9b".to_string())),
            ModelId("qwen35_a3b".to_string()),
            RuntimeId("mock".to_string()),
            "pending test".to_string(),
        );
        pending_intent.mark_pending();
        staging_worker.enqueue(pending_intent).await;

        let mut failed_intent = StagingIntent::new(
            Uuid::new_v4(),
            Some(ModelId("qwen9b".to_string())),
            ModelId("qwen35_a3b".to_string()),
            RuntimeId("mock".to_string()),
            "failed test".to_string(),
        );
        failed_intent.mark_failed("test error".to_string());
        staging_worker.enqueue(failed_intent).await;

        let mut completed_intent = StagingIntent::new(
            Uuid::new_v4(),
            Some(ModelId("qwen9b".to_string())),
            ModelId("qwen35_a3b".to_string()),
            RuntimeId("mock".to_string()),
            "completed test".to_string(),
        );
        completed_intent.mark_completed();
        staging_worker.enqueue(completed_intent).await;

        let all = staging_worker.get_all().await;

        let has_blocked = all.iter().any(|i| i.state == StagingState::Blocked);
        let has_pending = all.iter().any(|i| i.state == StagingState::Pending);
        let has_failed = all.iter().any(|i| i.state == StagingState::Failed);
        let has_completed = all.iter().any(|i| i.state == StagingState::Completed);

        assert!(has_blocked, "should have blocked intent");
        assert!(has_pending, "should have pending intent");
        assert!(has_failed, "should have failed intent");
        assert!(has_completed, "should have completed intent");
    }

    #[tokio::test]
    async fn staging_intent_records_decision_id_model_runtime_and_reason() {
        let decision_id = Uuid::new_v4();
        let model_id = ModelId("qwen35_a3b".to_string());
        let runtime_id = RuntimeId("mock".to_string());
        let reason = "background staging for quality upgrade".to_string();

        let intent = StagingIntent::new(
            decision_id,
            Some(ModelId("qwen9b".to_string())),
            model_id.clone(),
            runtime_id.clone(),
            reason.clone(),
        );

        assert_eq!(intent.decision_id, decision_id);
        assert_eq!(intent.background_model, model_id);
        assert_eq!(intent.target_runtime, runtime_id);
        assert_eq!(intent.reason, reason);
        assert!(intent.foreground_model.is_some());
    }

    #[tokio::test]
    async fn decision_action_plan_contains_foreground_load_when_required() {
        let state = AppState::new(example_config(), Arc::new(InMemoryDecisionLog::default()))
            .expect("state");

        let decision = Decision {
            id: Uuid::new_v4(),
            request_id: RequestId::new(),
            action: DecisionAction::ColdLoad,
            selected_model: Some(ModelId("qwen35_a3b".to_string())),
            selected_runtime: Some(RuntimeId("mock".to_string())),
            selected_group: None,
            background_model: None,
            score: anemoi_core::DecisionScore::default(),
            explanation: anemoi_core::Explanation {
                summary: "Cold load required".to_string(),
                reasons: vec![],
                rejected_options: vec![],
            },
            created_at: chrono::Utc::now(),
        };

        let plan = state.generate_action_plan(&decision, true);

        let load = plan
            .get_foreground_load()
            .expect("action plan should contain foreground load for cold load decision");
        assert_eq!(load.model_id, Some(ModelId("qwen35_a3b".to_string())));
        assert_eq!(load.runtime_id, RuntimeId("mock".to_string()));
    }

    #[tokio::test]
    async fn stage_background_action_plan_contains_background_load_intent() {
        let state = AppState::new(example_config(), Arc::new(InMemoryDecisionLog::default()))
            .expect("state");

        let decision = Decision {
            id: Uuid::new_v4(),
            request_id: RequestId::new(),
            action: DecisionAction::StageBackground,
            selected_model: Some(ModelId("qwen9b".to_string())),
            selected_runtime: Some(RuntimeId("mock".to_string())),
            selected_group: None,
            background_model: Some(ModelId("qwen35_a3b".to_string())),
            score: anemoi_core::DecisionScore::default(),
            explanation: anemoi_core::Explanation {
                summary: "Stage background".to_string(),
                reasons: vec![],
                rejected_options: vec![],
            },
            created_at: chrono::Utc::now(),
        };

        let plan = state.generate_action_plan(&decision, true);

        let load = plan
            .get_background_load()
            .expect("action plan should contain background load for stage background decision");
        assert_eq!(load.model_id, Some(ModelId("qwen35_a3b".to_string())));
        assert!(!load.is_foreground);
    }

    #[tokio::test]
    async fn reuse_hot_action_plan_contains_no_mutating_action() {
        let state = AppState::new(example_config(), Arc::new(InMemoryDecisionLog::default()))
            .expect("state");

        let decision = Decision {
            id: Uuid::new_v4(),
            request_id: RequestId::new(),
            action: DecisionAction::ReuseHot,
            selected_model: Some(ModelId("qwen9b".to_string())),
            selected_runtime: Some(RuntimeId("mock".to_string())),
            selected_group: None,
            background_model: None,
            score: anemoi_core::DecisionScore::default(),
            explanation: anemoi_core::Explanation {
                summary: "Reuse hot".to_string(),
                reasons: vec![],
                rejected_options: vec![],
            },
            created_at: chrono::Utc::now(),
        };

        let plan = state.generate_action_plan(&decision, true);

        assert!(
            !plan.has_mutating_actions(),
            "action plan for reuse_hot should contain no mutating actions"
        );
    }

    #[tokio::test]
    async fn action_plan_dry_run_does_not_call_runtime_adapter() {
        let state = AppState::new(example_config(), Arc::new(InMemoryDecisionLog::default()))
            .expect("state");

        let decision = Decision {
            id: Uuid::new_v4(),
            request_id: RequestId::new(),
            action: DecisionAction::ColdLoad,
            selected_model: Some(ModelId("qwen35_a3b".to_string())),
            selected_runtime: Some(RuntimeId("mock".to_string())),
            selected_group: None,
            background_model: None,
            score: anemoi_core::DecisionScore::default(),
            explanation: anemoi_core::Explanation {
                summary: "Cold load".to_string(),
                reasons: vec![],
                rejected_options: vec![],
            },
            created_at: chrono::Utc::now(),
        };

        let plan = state.generate_action_plan(&decision, true);

        assert!(plan.dry_run, "action plan should be dry run");
        assert!(
            plan.actions.iter().all(|a| !a.is_mutating || plan.dry_run),
            "dry run plan should not execute mutating actions"
        );
    }

    #[tokio::test]
    async fn live_action_plan_execution_requires_explicit_enable_flag() {
        let state = AppState::new(example_config(), Arc::new(InMemoryDecisionLog::default()))
            .expect("state");

        let mut plan = ActionPlan::new(Uuid::new_v4(), false);
        plan.add_load(
            RuntimeId("mock".to_string()),
            ModelId("qwen35_a3b".to_string()),
            true,
            "Test load".to_string(),
            None,
        );

        let result = state.execute_action_plan(&plan).await;
        assert!(
            result.is_err(),
            "live execution should fail without ANEMOI_ENABLE_LIVE_EXECUTE=1"
        );
    }

    #[tokio::test]
    async fn mock_eviction_executes_unload_action_when_plan_is_approved() {
        let state = AppState::new(example_config(), Arc::new(InMemoryDecisionLog::default()))
            .expect("state");

        // The example mock runtime starts with qwen9b resident.
        let before = state.snapshots().await;
        assert!(
            before.iter().any(|snapshot| snapshot
                .residents
                .iter()
                .any(|resident| resident.model_id == ModelId("qwen9b".to_string()))),
            "qwen9b should be resident before eviction"
        );

        let mut plan = ActionPlan::new(Uuid::new_v4(), false);
        plan.add_unload(
            RuntimeId("mock".to_string()),
            ModelId("qwen9b".to_string()),
            "approved eviction".to_string(),
        );

        let results = state
            .execute_eviction_plan(&plan, true)
            .await
            .expect("mock eviction should execute");
        assert!(
            results.iter().any(|line| line.contains("qwen9b")),
            "eviction result should report unloading qwen9b"
        );

        let after = state.snapshots().await;
        assert!(
            !after.iter().any(|snapshot| snapshot
                .residents
                .iter()
                .any(|resident| resident.model_id == ModelId("qwen9b".to_string()))),
            "qwen9b should no longer be resident after approved mock eviction"
        );
    }

    #[tokio::test]
    async fn live_eviction_requires_explicit_enable_flag() {
        let config = AnemoiConfig::from_yaml_str(
            r#"
domains:
  coding:
    rosters: [pool]
residency_groups:
  pool:
    models: [qwen9b]
models:
  qwen9b:
    family: qwen
    parameter_class: 9b
    context_window: 32768
    vram_required_mb: 9000
    ram_required_mb: 12000
    cold_load_estimate_ms: 18000
    supported_runtimes: [live]
runtimes:
  live:
    adapter: ollama
    base_url: http://127.0.0.1:11434
"#,
        )
        .expect("valid config");

        let state = AppState::new(config, Arc::new(InMemoryDecisionLog::default())).expect("state");

        let mut plan = ActionPlan::new(Uuid::new_v4(), false);
        plan.add_unload(
            RuntimeId("live".to_string()),
            ModelId("qwen9b".to_string()),
            "operator eviction".to_string(),
        );

        let result = state.execute_eviction_plan(&plan, true).await;
        assert!(
            result.is_err(),
            "live (non-mock) eviction must fail without ANEMOI_ENABLE_LIVE_EXECUTE=1"
        );
    }

    #[tokio::test]
    async fn action_plan_explanation_lists_each_planned_action() {
        let state = AppState::new(example_config(), Arc::new(InMemoryDecisionLog::default()))
            .expect("state");

        let decision = Decision {
            id: Uuid::new_v4(),
            request_id: RequestId::new(),
            action: DecisionAction::ColdLoad,
            selected_model: Some(ModelId("qwen35_a3b".to_string())),
            selected_runtime: Some(RuntimeId("mock".to_string())),
            selected_group: None,
            background_model: None,
            score: anemoi_core::DecisionScore::default(),
            explanation: anemoi_core::Explanation {
                summary: "Cold load required".to_string(),
                reasons: vec![],
                rejected_options: vec![],
            },
            created_at: chrono::Utc::now(),
        };

        let plan = state.generate_action_plan(&decision, true);

        assert!(
            !plan.actions.is_empty(),
            "action plan should list at least one action"
        );
        let action = &plan.actions[0];
        assert_eq!(action.kind, ActionKind::Load);
        assert!(action.is_foreground);
        assert!(action.is_mutating);
    }

    #[tokio::test]
    async fn staging_tick_completes_pending_intent_for_correct_runtime() {
        let state = AppState::new(example_config(), Arc::new(InMemoryDecisionLog::default()))
            .expect("state");

        state
            .staging_worker
            .unblock_and_queue(
                Uuid::new_v4(),
                Some(ModelId("qwen9b".to_string())),
                ModelId("granite8b".to_string()),
                RuntimeId("mock".to_string()),
                "background staging test".to_string(),
                None,
            )
            .await;

        let before = state.staging_worker.get_pending().await;
        assert_eq!(before.len(), 1, "one intent should be pending before tick");

        state.run_staging_tick().await;

        let after = state.staging_worker.get_pending().await;
        assert_eq!(
            after.len(),
            0,
            "pending intent should be processed after staging tick"
        );

        let all = state.staging_worker.get_all().await;
        assert_eq!(all[0].state, StagingState::Completed);
    }

    #[tokio::test]
    async fn reconcile_ready_completes_only_the_ready_models_intent() {
        let worker = StagingWorker::new();

        let mut ready_intent = StagingIntent::new(
            Uuid::new_v4(),
            None,
            ModelId("granite8b".to_string()),
            RuntimeId("swap_rt".to_string()),
            "ready via SSE".to_string(),
        );
        ready_intent.mark_pending();
        let ready_id = ready_intent.id;
        worker.enqueue(ready_intent).await;

        let mut loading_intent = StagingIntent::new(
            Uuid::new_v4(),
            None,
            ModelId("minimax".to_string()),
            RuntimeId("swap_rt".to_string()),
            "still loading".to_string(),
        );
        loading_intent.mark_pending();
        worker.enqueue(loading_intent).await;

        let ready: HashSet<ModelId> = [ModelId("granite8b".to_string())].into_iter().collect();
        let completed = worker.reconcile_ready(&ready).await;

        assert_eq!(completed, vec![ready_id]);
        let all = worker.get_all().await;
        let granite = all
            .iter()
            .find(|i| i.background_model == ModelId("granite8b".to_string()))
            .expect("granite intent");
        assert_eq!(granite.state, StagingState::Completed);
        let minimax = all
            .iter()
            .find(|i| i.background_model == ModelId("minimax".to_string()))
            .expect("minimax intent");
        assert_eq!(
            minimax.state,
            StagingState::Pending,
            "a model not yet ready must remain pending"
        );
    }

    #[tokio::test]
    async fn staging_tick_completes_intent_from_readiness_without_live_execute() {
        // Runtime typed non-mock so the runtime-load path is gated off, but
        // backed by a mock adapter that reports the staged model hot — i.e. what
        // the `/api/events` SSE cache produces via inspect().
        let mut config = example_config();
        let swap_rt = RuntimeId("swap_rt".to_string());
        config.runtimes.insert(
            swap_rt.clone(),
            RuntimeConfig {
                adapter: "llama_swap".to_string(),
                base_url: Some("http://127.0.0.1:9".to_string()),
                auth_token: None,
                initial_residents: vec![],
                config_path: None,
            },
        );

        let mut residents = HashMap::new();
        residents.insert(
            swap_rt.to_string(),
            vec![ModelResident {
                model_id: ModelId("granite8b".to_string()),
                state: ResidencyState::HotGpu,
                vram_mb: None,
                ram_mb: None,
                kv_cache_mb: None,
                loaded_since: None,
            }],
        );
        let state = AppState::with_mock_residents(config, residents)
            .expect("state")
            .with_live_execute(false);

        state
            .staging_worker
            .unblock_and_queue(
                Uuid::new_v4(),
                None,
                ModelId("granite8b".to_string()),
                swap_rt,
                "ready via SSE cache".to_string(),
                None,
            )
            .await;

        // Populate the reconciler cache from inspect (mock reports granite8b hot).
        state.run_reconciliation_tick().await;
        state.run_staging_tick().await;

        let all = state.staging_worker.get_all().await;
        assert_eq!(all.len(), 1);
        assert_eq!(
            all[0].state,
            StagingState::Completed,
            "the readiness cache must complete the intent even with the live-load gate closed"
        );
    }

    #[tokio::test]
    async fn staging_tick_skips_intent_with_unknown_runtime() {
        let state = AppState::new(example_config(), Arc::new(InMemoryDecisionLog::default()))
            .expect("state");

        state
            .staging_worker
            .unblock_and_queue(
                Uuid::new_v4(),
                None,
                ModelId("granite8b".to_string()),
                RuntimeId("nonexistent".to_string()),
                "should be skipped".to_string(),
                None,
            )
            .await;

        state.run_staging_tick().await;

        let still_pending = state.staging_worker.get_pending().await;
        assert_eq!(
            still_pending.len(),
            1,
            "intent for unknown runtime should remain pending"
        );
    }

    #[tokio::test]
    async fn staging_tick_skips_non_mock_runtime_without_live_execution_flag() {
        // Build a config with a non-mock adapter type. AppState::new() falls
        // through to MockRuntimeAdapter for any unrecognised adapter string,
        // but runtime_adapter_type() still returns the configured string, so
        // the live-execution gate treats it as a live runtime.
        let mut config = example_config();
        let live_runtime_id = RuntimeId("live_rt".to_string());
        config.runtimes.insert(
            live_runtime_id.clone(),
            RuntimeConfig {
                adapter: "ollama".to_string(),
                base_url: Some("http://127.0.0.1:11434".to_string()),
                auth_token: None,
                initial_residents: vec![],
                config_path: None,
            },
        );

        let state = AppState::new(config, Arc::new(InMemoryDecisionLog::default())).expect("state");

        state
            .staging_worker
            .unblock_and_queue(
                Uuid::new_v4(),
                None,
                ModelId("qwen9b".to_string()),
                live_runtime_id,
                "should be held behind live gate".to_string(),
                None,
            )
            .await;

        // ANEMOI_ENABLE_LIVE_EXECUTE is not set — tick must not fire the load.
        state.run_staging_tick().await;

        let still_pending = state.staging_worker.get_pending().await;
        assert_eq!(
            still_pending.len(),
            1,
            "non-mock staging intent should stay pending without ANEMOI_ENABLE_LIVE_EXECUTE"
        );
    }

    #[tokio::test]
    async fn decide_burst_reads_reconciler_cache_not_runtime_inspect() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct CountingAdapter {
            inner: DynRuntimeAdapter,
            inspects: Arc<AtomicUsize>,
        }

        #[async_trait::async_trait]
        impl anemoi_runtime::RuntimeAdapter for CountingAdapter {
            fn id(&self) -> RuntimeId {
                self.inner.id()
            }
            async fn inspect(&self) -> Result<RuntimeSnapshot, anemoi_runtime::RuntimeError> {
                self.inspects.fetch_add(1, Ordering::SeqCst);
                self.inner.inspect().await
            }
            async fn load_model(
                &self,
                model: &ModelId,
            ) -> Result<anemoi_runtime::LoadHandle, anemoi_runtime::RuntimeError> {
                self.inner.load_model(model).await
            }
            async fn unload_model(
                &self,
                model: &ModelId,
            ) -> Result<(), anemoi_runtime::RuntimeError> {
                self.inner.unload_model(model).await
            }
            async fn execute(
                &self,
                request: anemoi_core::ExecutionRequest,
            ) -> Result<anemoi_runtime::ExecutionHandle, anemoi_runtime::RuntimeError> {
                self.inner.execute(request).await
            }
        }

        let mut state = AppState::new(example_config(), Arc::new(InMemoryDecisionLog::default()))
            .expect("state");
        let inspects = Arc::new(AtomicUsize::new(0));
        let original = state
            .runtimes
            .get("mock")
            .expect("mock runtime present")
            .clone();
        state.runtimes.insert(
            "mock".to_string(),
            Arc::new(CountingAdapter {
                inner: original,
                inspects: inspects.clone(),
            }),
        );

        for _ in 0..8 {
            state.decide(&sample_request()).await.expect("decision");
        }

        let total = inspects.load(Ordering::SeqCst);
        assert_eq!(
            total, 1,
            "decide burst must reuse the reconciler cache; got {total} inspects"
        );
    }

    fn chat_body(model: &str) -> Value {
        serde_json::json!({
            "model": model,
            "messages": [{ "role": "user", "content": "hello" }],
        })
    }

    async fn body_value(response: Response) -> Value {
        let bytes = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body bytes");
        serde_json::from_slice(&bytes).expect("json body")
    }

    async fn body_text(response: Response) -> String {
        let bytes = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body bytes");
        String::from_utf8(bytes.to_vec()).expect("utf8 body")
    }

    fn decision_id_header(response: &Response) -> Uuid {
        let raw = response
            .headers()
            .get("x-anemoi-decision-id")
            .expect("x-anemoi-decision-id header")
            .to_str()
            .expect("header is ascii");
        Uuid::parse_str(raw).expect("decision id is a uuid")
    }

    /// Config whose only runtime is a non-mock (ollama) adapter pointed at a dead
    /// port. The reconciler is pre-seeded by the caller so `decide` selects this
    /// runtime without any live `inspect()`.
    fn non_mock_gateway_state(live_execute: bool) -> AppState {
        let yaml = r#"
domains:
  coding:
    rosters:
      - ollama_group
residency_groups:
  ollama_group:
    purpose:
      - test
    keep_hot: true
    allow_background_load: false
    models:
      - llama8b
models:
  llama8b:
    family: llama
    parameter_class: 8b
    context_window: 8192
    vram_required_mb: 8000
    ram_required_mb: 10000
    cold_load_estimate_ms: 12000
    supports_streaming: true
    supported_runtimes:
      - remote
runtimes:
  remote:
    adapter: ollama
    base_url: http://127.0.0.1:9
    initial_residents:
      - model_id: llama8b
        state: hot_gpu
        vram_mb: 8000
        ram_mb: 10000
continuity:
  keep_small_worker_hot: true
  background_load: false
  max_blank_wait_ms: 1500
  prefer_degraded_response_over_silence: true
"#;
        let config = AnemoiConfig::from_yaml_str(yaml).expect("non-mock config");
        AppState::with_mock_residents(config, HashMap::new())
            .expect("state")
            .with_live_execute(live_execute)
    }

    fn remote_hot_snapshot() -> RuntimeSnapshot {
        RuntimeSnapshot {
            runtime_id: RuntimeId("remote".to_string()),
            available: true,
            residents: vec![ModelResident {
                model_id: ModelId("llama8b".to_string()),
                state: ResidencyState::HotGpu,
                vram_mb: Some(8000),
                ram_mb: Some(10000),
                kv_cache_mb: None,
                loaded_since: Some(chrono::Utc::now()),
            }],
            configured_models: vec![ModelId("llama8b".to_string())],
            memory: anemoi_core::RuntimeMemorySnapshot::default(),
            active_requests: vec![],
        }
    }

    #[test]
    fn inference_gateway_maps_model_field_to_domain() {
        assert_eq!(resolve_domain("coding"), "coding");
        assert_eq!(resolve_domain("general"), "general");
    }

    #[test]
    fn inference_gateway_strips_anemoi_prefix_from_model_field() {
        assert_eq!(resolve_domain("anemoi-coding"), "coding");
        // Only the leading `anemoi-` is stripped; bare names are unchanged.
        assert_eq!(resolve_domain("coding"), "coding");
    }

    #[tokio::test]
    async fn inference_gateway_returns_error_for_unknown_domain() {
        let response = test_router()
            .oneshot(json_request("/v1/chat/completions", &chat_body("nonsense")))
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = body_value(response).await;
        assert_eq!(body["error"]["type"], "anemoi_gateway_error");
        assert!(
            body["error"]["message"]
                .as_str()
                .expect("message")
                .contains("nonsense"),
            "error should name the unknown domain"
        );
    }

    #[tokio::test]
    async fn inference_gateway_runs_decide_before_forwarding() {
        let log = Arc::new(InMemoryDecisionLog::default());
        let state = AppState::new(example_config(), log.clone()).expect("state");

        let response = router(state)
            .oneshot(json_request("/v1/chat/completions", &chat_body("coding")))
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        let id = decision_id_header(&response);
        assert!(
            log.get_decision(id).await.expect("log").is_some(),
            "a decision must be recorded before forwarding"
        );
    }

    #[tokio::test]
    async fn inference_gateway_rewrites_model_to_selected_model() {
        let response = test_router()
            .oneshot(json_request("/v1/chat/completions", &chat_body("coding")))
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        let selected = response
            .headers()
            .get("x-anemoi-selected-model")
            .expect("x-anemoi-selected-model header")
            .to_str()
            .expect("ascii")
            .to_string();
        assert_ne!(
            selected, "coding",
            "selected model must be a runtime model, not the domain"
        );

        // The mock runtime echoes the model it was asked to serve, proving the
        // caller's domain hint was rewritten to the governed model id.
        let body = body_text(response).await;
        assert!(
            body.contains(&selected),
            "forwarded SSE body should reference the selected model `{selected}`"
        );
    }

    #[tokio::test]
    async fn inference_gateway_records_decision_in_telemetry() {
        let log = Arc::new(InMemoryDecisionLog::default());
        let state = AppState::new(example_config(), log.clone()).expect("state");

        let response = router(state)
            .oneshot(json_request("/v1/chat/completions", &chat_body("coding")))
            .await
            .expect("response");

        let id = decision_id_header(&response);
        let recorded = log
            .get_decision(id)
            .await
            .expect("log")
            .expect("decision recorded in telemetry");
        assert_eq!(recorded.id, id);
        assert!(recorded.selected_model.is_some());
    }

    #[tokio::test]
    async fn inference_gateway_returns_decision_id_in_response_header() {
        let response = test_router()
            .oneshot(json_request("/v1/chat/completions", &chat_body("coding")))
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        // Parses as a uuid or panics.
        let _ = decision_id_header(&response);
    }

    #[tokio::test]
    async fn inference_gateway_forwards_mock_without_live_execute_flag() {
        // test_router has no live-execute flag set; the mock runtime must still
        // forward because mock forwarding never touches a real runtime.
        let response = test_router()
            .oneshot(json_request("/v1/chat/completions", &chat_body("coding")))
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = body_text(response).await;
        assert!(
            body.contains("data:"),
            "mock forward should return an SSE stream"
        );
    }

    #[tokio::test]
    async fn inference_gateway_requires_live_execute_flag_for_non_mock() {
        let state = non_mock_gateway_state(false);
        state
            .reconciler()
            .update("remote", remote_hot_snapshot())
            .await;

        let response = router(state)
            .oneshot(json_request("/v1/chat/completions", &chat_body("coding")))
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let body = body_value(response).await;
        assert_eq!(body["error"]["type"], "anemoi_gateway_error");
    }

    #[tokio::test]
    async fn inference_gateway_returns_structured_error_on_runtime_failure() {
        let state = non_mock_gateway_state(true);
        state
            .reconciler()
            .update("remote", remote_hot_snapshot())
            .await;

        let response = router(state)
            .oneshot(json_request("/v1/chat/completions", &chat_body("coding")))
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        let id_header = response
            .headers()
            .get("x-anemoi-decision-id")
            .expect("decision id header on forwarding failure")
            .to_str()
            .expect("ascii")
            .to_string();
        let body = body_value(response).await;
        assert_eq!(body["error"]["type"], "anemoi_gateway_error");
        assert_eq!(
            body["error"]["decision_id"].as_str().expect("decision_id"),
            id_header,
            "structured error must carry the decision id"
        );
    }

    #[test]
    fn escalation_context_cache_roundtrips_then_consumes() {
        let cache = EscalationContextCache::default();
        let id = Uuid::new_v4();

        let cref = cache.write(id, "prompt context".to_string());
        assert_eq!(
            cref,
            ContextRef(id),
            "the ref is keyed by the staging-intent id"
        );
        assert_eq!(cache.take(&cref), Some("prompt context".to_string()));
        assert_eq!(
            cache.take(&cref),
            None,
            "a consumed context is handed off only once"
        );
    }

    #[test]
    fn escalation_context_cache_expires_after_ttl() {
        let cache = EscalationContextCache::new(chrono::Duration::seconds(60), 16);
        let id = Uuid::new_v4();
        let stored = Utc::now();

        cache.write_at(id, "context".to_string(), stored);
        assert_eq!(
            cache.take_at(&ContextRef(id), stored + chrono::Duration::seconds(30)),
            Some("context".to_string()),
            "an entry within its TTL is retrievable"
        );

        cache.write_at(id, "context".to_string(), stored);
        assert_eq!(
            cache.take_at(&ContextRef(id), stored + chrono::Duration::seconds(61)),
            None,
            "an entry past its TTL is not handed off"
        );
    }

    #[test]
    fn escalation_context_cache_evicts_oldest_over_capacity() {
        let cache = EscalationContextCache::new(chrono::Duration::seconds(600), 2);
        let base = Utc::now();
        let oldest = Uuid::new_v4();
        let middle = Uuid::new_v4();
        let newest = Uuid::new_v4();

        cache.write_at(oldest, "1".to_string(), base);
        cache.write_at(middle, "2".to_string(), base + chrono::Duration::seconds(1));
        cache.write_at(newest, "3".to_string(), base + chrono::Duration::seconds(2));

        assert_eq!(
            cache.len(),
            2,
            "the capacity cap holds the store at two entries"
        );
        assert_eq!(
            cache.take(&ContextRef(oldest)),
            None,
            "the oldest entry is evicted first on overflow"
        );
        assert_eq!(cache.take(&ContextRef(middle)), Some("2".to_string()));
        assert_eq!(cache.take(&ContextRef(newest)), Some("3".to_string()));
    }

    #[tokio::test]
    async fn commit_eviction_context_writes_when_foreground_drains() {
        let worker = StagingWorker::new();
        let cache = EscalationContextCache::default();
        let small = ModelId("qwen9b".to_string());

        worker
            .unblock_and_queue(
                Uuid::new_v4(),
                Some(small.clone()),
                ModelId("minimax".to_string()),
                RuntimeId("swap_rt".to_string()),
                "escalate to big model".to_string(),
                Some("carry this prompt".to_string()),
            )
            .await;
        // A second intent whose foreground is not draining must be left alone.
        worker
            .unblock_and_queue(
                Uuid::new_v4(),
                Some(ModelId("granite8b".to_string())),
                ModelId("qwen35_a3b".to_string()),
                RuntimeId("swap_rt".to_string()),
                "unrelated staging".to_string(),
                Some("should stay pending".to_string()),
            )
            .await;

        let draining: HashSet<ModelId> = [small.clone()].into_iter().collect();
        let committed = worker.commit_eviction_context(&draining, &cache).await;
        assert_eq!(
            committed.len(),
            1,
            "only the intent whose foreground is draining commits"
        );

        let all = worker.get_all().await;
        let escalating = all
            .iter()
            .find(|i| i.foreground_model.as_ref() == Some(&small))
            .expect("escalating intent");
        let cref = escalating
            .context_ref
            .clone()
            .expect("context_ref is set on the eviction signal");
        assert!(
            escalating.pending_context.is_none(),
            "the pending context is moved into the cache, not left on the intent"
        );
        assert_eq!(cache.take(&cref), Some("carry this prompt".to_string()));

        let untouched = all
            .iter()
            .find(|i| i.foreground_model.as_ref() == Some(&ModelId("granite8b".to_string())))
            .expect("unrelated intent");
        assert!(
            untouched.context_ref.is_none(),
            "an intent whose foreground is not draining keeps its pending context"
        );
        assert_eq!(
            untouched.pending_context.as_deref(),
            Some("should stay pending")
        );

        let again = worker.commit_eviction_context(&draining, &cache).await;
        assert!(
            again.is_empty(),
            "an already-committed intent does not commit again"
        );
    }

    #[tokio::test]
    async fn decide_attaches_escalation_context_to_staging_intent() {
        let mut config = example_config();
        config.continuity.background_load = true;
        config.continuity.keep_small_worker_hot = true;
        let state = AppState::new(config, Arc::new(InMemoryDecisionLog::default())).expect("state");

        let mut request = sample_request();
        request.escalation_intent = Some(EscalationIntent {
            task_type: Some("planning".to_string()),
            context: Some("multistep troubleshooting context".to_string()),
        });

        state.decide(&request).await.expect("decision");

        let intents = state.staging_worker.get_all().await;
        let staged = intents.first().expect("a staging intent was enqueued");
        assert_eq!(
            staged.pending_context.as_deref(),
            Some("multistep troubleshooting context"),
            "the declared escalation context rides along on the staging intent"
        );
        assert!(
            staged.context_ref.is_none(),
            "context_ref is set only on the eviction signal, not at decision time"
        );
    }

    #[tokio::test]
    async fn execute_hands_off_staged_escalation_context() {
        let state = AppState::new(example_config(), Arc::new(InMemoryDecisionLog::default()))
            .expect("state");
        state.run_reconciliation_tick().await;

        let decision = state.decide(&sample_request()).await.expect("decision");
        let model = decision.selected_model.clone().expect("a selected model");
        let runtime = decision
            .selected_runtime
            .clone()
            .expect("a selected runtime");

        // Simulate a prior escalation: a completed staging intent for the
        // selected model carrying a committed context reference.
        let mut intent = StagingIntent::new(
            decision.id,
            Some(model.clone()),
            model.clone(),
            runtime,
            "prior escalation".to_string(),
        );
        intent.mark_completed();
        let cref = state
            .escalation_context
            .write(intent.id, "prior small-model context".to_string());
        intent.context_ref = Some(cref);
        state.staging_worker.enqueue(intent).await;

        let response = router(state.clone())
            .oneshot(json_request("/execute", &sample_request()))
            .await
            .expect("response");
        let execute: ExecuteResponse =
            serde_json::from_value(json_body(response).await).expect("execute response");
        assert_eq!(
            execute.handoff.escalation_context.as_deref(),
            Some("prior small-model context"),
            "execute hands the staged context to the selected model as its initial prompt"
        );

        let again = router(state)
            .oneshot(json_request("/execute", &sample_request()))
            .await
            .expect("response");
        let again: ExecuteResponse =
            serde_json::from_value(json_body(again).await).expect("execute response");
        assert!(
            again.handoff.escalation_context.is_none(),
            "the context is handed off at most once"
        );
    }
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/status", get(status))
        .route("/residents", get(residents))
        .route("/decide", post(decide))
        .route("/execute", post(execute))
        .route("/decisions/:id", get(decision))
        .route("/explain/:id", get(explain))
        .route("/staging", get(staging))
        .route("/v1/models", get(list_models))
        .route("/v1/chat/completions", post(chat_completions))
        .route("/openapi.json", get(openapi))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

pub async fn serve(addr: SocketAddr, state: AppState) -> anyhow::Result<()> {
    let reconciliation_state = state.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_millis(
            DEFAULT_RECONCILIATION_TTL_MS / 2,
        ));
        loop {
            interval.tick().await;
            reconciliation_state.run_reconciliation_tick().await;
        }
    });

    let staging_state = state.clone();
    tokio::spawn(async move {
        let mut interval =
            tokio::time::interval(tokio::time::Duration::from_millis(DEFAULT_STAGING_POLL_MS));
        loop {
            interval.tick().await;
            staging_state.run_staging_tick().await;
        }
    });

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, router(state)).await?;
    Ok(())
}

#[derive(Serialize)]
struct HealthResponse {
    ok: bool,
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse { ok: true })
}

async fn openapi() -> Json<serde_json::Value> {
    Json(openapi_document())
}

pub fn openapi_document() -> serde_json::Value {
    serde_json::json!({
        "openapi": "3.0.3",
        "info": {
            "title": "Anemoi Local Governance API",
            "version": "0.1.0",
            "description": "Local control-plane API. In v1, /execute performs decision logging and model-load handoff only; it does not forward full inference."
        },
        "paths": {
            "/health": {
                "get": {
                    "responses": {
                        "200": {
                            "description": "Daemon is healthy",
                            "content": {
                                "application/json": {
                                    "schema": { "$ref": "#/components/schemas/HealthResponse" }
                                }
                            }
                        }
                    }
                }
            },
            "/status": {
                "get": {
                    "responses": {
                        "200": {
                            "description": "Configured control-plane counts",
                            "content": {
                                "application/json": {
                                    "schema": { "$ref": "#/components/schemas/OperatorStatus" }
                                }
                            }
                        }
                    }
                }
            },
            "/residents": {
                "get": {
                    "responses": {
                        "200": {
                            "description": "Normalized runtime snapshots",
                            "content": {
                                "application/json": {
                                    "schema": {
                                        "type": "array",
                                        "items": { "$ref": "#/components/schemas/RuntimeSnapshot" }
                                    }
                                }
                            }
                        }
                    }
                }
            },
            "/decide": {
                "post": {
                    "requestBody": {
                        "required": true,
                        "content": {
                            "application/json": {
                                "schema": { "$ref": "#/components/schemas/InferenceRequest" }
                            }
                        }
                    },
                    "responses": {
                        "200": {
                            "description": "Structured decision without execution",
                            "content": {
                                "application/json": {
                                    "schema": { "$ref": "#/components/schemas/Decision" }
                                }
                            }
                        },
                        "500": { "$ref": "#/components/responses/Error" }
                    }
                }
            },
            "/execute": {
                "post": {
                    "description": "v1 model-load handoff only; full inference forwarding is false.",
                    "requestBody": {
                        "required": true,
                        "content": {
                            "application/json": {
                                "schema": { "$ref": "#/components/schemas/InferenceRequest" }
                            }
                        }
                    },
                    "responses": {
                        "200": {
                            "description": "Decision plus explicit handoff status",
                            "content": {
                                "application/json": {
                                    "schema": { "$ref": "#/components/schemas/ExecuteResponse" }
                                }
                            }
                        },
                        "500": { "$ref": "#/components/responses/Error" }
                    }
                }
            },
            "/decisions/{id}": {
                "get": {
                    "parameters": [{ "$ref": "#/components/parameters/DecisionId" }],
                    "responses": {
                        "200": {
                            "description": "Recorded decision from process memory",
                            "content": {
                                "application/json": {
                                    "schema": { "$ref": "#/components/schemas/Decision" }
                                }
                            }
                        },
                        "404": { "$ref": "#/components/responses/Error" }
                    }
                }
            },
            "/explain/{id}": {
                "get": {
                    "parameters": [{ "$ref": "#/components/parameters/DecisionId" }],
                    "responses": {
                        "200": {
                            "description": "Recorded explanation from process memory",
                            "content": {
                                "application/json": {
                                    "schema": { "$ref": "#/components/schemas/Explanation" }
                                }
                            }
                        },
                        "404": { "$ref": "#/components/responses/Error" }
                    }
                }
            }
        },
        "components": {
            "parameters": {
                "DecisionId": {
                    "name": "id",
                    "in": "path",
                    "required": true,
                    "schema": { "type": "string", "format": "uuid" }
                }
            },
            "responses": {
                "Error": {
                    "description": "Structured error text",
                    "content": {
                        "text/plain": {
                            "schema": { "$ref": "#/components/schemas/ErrorResponse" }
                        }
                    }
                }
            },
            "schemas": {
                "HealthResponse": {
                    "type": "object",
                    "required": ["ok"],
                    "properties": { "ok": { "type": "boolean" } }
                },
                "OperatorStatus": {
                    "type": "object",
                    "required": [
                        "cache_populated",
                        "runtimes",
                        "residency_groups",
                        "active_request_count",
                        "staging",
                        "recent_decision_count",
                        "policy_warnings",
                        "live_execution_enabled"
                    ],
                    "properties": {
                        "cache_populated": { "type": "boolean" },
                        "runtimes": { "type": "array", "items": { "type": "object" } },
                        "residency_groups": { "type": "array", "items": { "type": "object" } },
                        "active_request_count": { "type": "integer" },
                        "staging": { "type": "object" },
                        "recent_decision_count": { "type": "integer" },
                        "policy_warnings": { "type": "array", "items": { "type": "string" } },
                        "live_execution_enabled": { "type": "boolean" }
                    }
                },
                "InferenceRequest": {
                    "type": "object",
                    "required": ["domain", "mode"],
                    "properties": {
                        "id": { "type": "string" },
                        "domain": { "type": "string" },
                        "mode": { "type": "string", "enum": ["interactive", "batch", "background"] },
                        "prompt_tokens_estimate": { "type": "integer", "nullable": true },
                        "max_output_tokens": { "type": "integer", "nullable": true },
                        "latency_budget_ms": { "type": "integer", "nullable": true },
                        "quality_floor": { "type": "object", "nullable": true },
                        "escalation_intent": { "type": "object", "nullable": true }
                    }
                },
                "RuntimeSnapshot": {
                    "type": "object",
                    "required": ["runtime_id", "available", "residents", "memory", "active_requests"],
                    "properties": {
                        "runtime_id": { "type": "string" },
                        "available": { "type": "boolean" },
                        "residents": { "type": "array", "items": { "type": "object" } },
                        "memory": { "type": "object" },
                        "active_requests": { "type": "array", "items": { "type": "object" } }
                    }
                },
                "Decision": {
                    "type": "object",
                    "required": ["id", "request_id", "action", "score", "explanation", "created_at"],
                    "properties": {
                        "id": { "type": "string", "format": "uuid" },
                        "request_id": { "type": "string" },
                        "action": { "type": "string", "enum": ["reuse_hot", "promote_warm", "cold_load", "stage_background", "downgrade", "defer", "deny"] },
                        "selected_model": { "type": "string", "nullable": true },
                        "selected_runtime": { "type": "string", "nullable": true },
                        "selected_group": { "type": "string", "nullable": true },
                        "background_model": { "type": "string", "nullable": true },
                        "score": { "type": "object" },
                        "explanation": { "$ref": "#/components/schemas/Explanation" },
                        "created_at": { "type": "string", "format": "date-time" }
                    }
                },
                "Explanation": {
                    "type": "object",
                    "required": ["summary", "reasons", "rejected_options"],
                    "properties": {
                        "summary": { "type": "string" },
                        "reasons": { "type": "array", "items": { "type": "object" } },
                        "rejected_options": { "type": "array", "items": { "type": "object" } }
                    }
                },
                "ExecuteResponse": {
                    "type": "object",
                    "required": ["decision", "handoff"],
                    "properties": {
                        "decision": { "$ref": "#/components/schemas/Decision" },
                        "handoff": { "$ref": "#/components/schemas/ExecuteHandoff" }
                    }
                },
                "ExecuteHandoff": {
                    "type": "object",
                    "required": ["load_requested", "full_inference_forwarded", "message"],
                    "properties": {
                        "load_requested": { "type": "boolean" },
                        "full_inference_forwarded": { "type": "boolean", "enum": [false] },
                        "escalation_context": { "type": "string", "nullable": true },
                        "message": { "type": "string" }
                    }
                },
                "ErrorResponse": {
                    "type": "string"
                }
            }
        }
    })
}

async fn status(State(state): State<AppState>) -> Json<OperatorStatus> {
    Json(state.operator_status().await)
}

async fn residents(State(state): State<AppState>) -> Json<Vec<RuntimeSnapshot>> {
    let has_cache = state.reconciler.has_cache().await;
    let snapshots = if has_cache {
        state.reconciler.get_snapshots().await
    } else {
        state.snapshots().await
    };
    Json(snapshots)
}

async fn staging(State(state): State<AppState>) -> Json<Vec<StagingIntent>> {
    Json(state.staging_worker.get_all().await)
}

async fn decide(
    State(state): State<AppState>,
    Json(request): Json<InferenceRequest>,
) -> Result<Json<Decision>, (StatusCode, String)> {
    state
        .decide(&request)
        .await
        .map(Json)
        .map_err(internal_error)
}

async fn execute(
    State(state): State<AppState>,
    Json(request): Json<InferenceRequest>,
) -> Result<Json<ExecuteResponse>, (StatusCode, String)> {
    let decision = state.decide(&request).await.map_err(internal_error)?;

    // If the selected model is one a prior escalation staged context for, hand
    // that context off now as its initial prompt, consuming it from the cache
    // (issue #64). Safe regardless of the live-execute gate — it is a cache read.
    let escalation_context = match &decision.selected_model {
        Some(model) => state.take_escalation_context(model).await,
        None => None,
    };

    let dry_run = !live_execution_enabled();
    let action_plan = state.generate_action_plan(&decision, dry_run);

    let mut load_requested = false;
    let mut action_results = Vec::new();

    if !dry_run {
        if let (Some(runtime_id), Some(model_id)) =
            (&decision.selected_runtime, &decision.selected_model)
        {
            let adapter_type = state.runtime_adapter_type(&runtime_id.to_string());
            let is_mock = adapter_type == Some("mock");
            if is_mock {
                if let Some(runtime) = state.runtimes.get(&runtime_id.to_string()) {
                    match runtime.load_model(model_id).await {
                        Ok(handle) => {
                            load_requested = true;
                            action_results
                                .push(format!("Loaded {} (handle: {})", model_id, handle.id));
                        }
                        Err(e) => {
                            action_results.push(format!("Load failed: {}", e));
                        }
                    }
                }
            }
        }
    }

    Ok(Json(ExecuteResponse {
        decision,
        action_plan,
        handoff: ExecuteHandoff {
            load_requested,
            full_inference_forwarded: false,
            escalation_context,
            message: if dry_run {
                "v1 execute performs dry-run action plan and model-load handoff only"
            } else {
                "v1 execute performs decision logging and model-load handoff only"
            }
            .to_string(),
        },
    }))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecuteResponse {
    pub decision: Decision,
    pub action_plan: ActionPlan,
    pub handoff: ExecuteHandoff,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecuteHandoff {
    pub load_requested: bool,
    pub full_inference_forwarded: bool,
    /// Escalation context (issue #64) consumed from the cache and supplied as the
    /// selected model's initial prompt, when a staged handoff was pending for it.
    /// `None` when there was no escalation context to hand off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub escalation_context: Option<String>,
    pub message: String,
}

async fn decision(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Decision>, (StatusCode, String)> {
    state
        .decision_log
        .get_decision(id)
        .await
        .map_err(internal_error)?
        .map(Json)
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                format!("decision {id} was not found"),
            )
        })
}

async fn explain(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<anemoi_core::Explanation>, (StatusCode, String)> {
    let decision = state
        .decision_log
        .get_decision(id)
        .await
        .map_err(internal_error)?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                format!("decision {id} was not found"),
            )
        })?;
    Ok(Json(decision.explanation))
}

fn internal_error(error: impl std::fmt::Display) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, error.to_string())
}

// ===== Inference Forwarding Gateway (prompt 28) =====

/// Latency budget applied to gateway-originated decisions when the caller does
/// not express one. The OpenAI chat-completions schema has no budget field.
const DEFAULT_GATEWAY_LATENCY_BUDGET_MS: u64 = 30_000;

/// Treats the OpenAI `model` field as a governance domain hint, stripping the
/// optional `anemoi-` prefix. `"coding"` and `"anemoi-coding"` both map to the
/// `coding` domain.
fn resolve_domain(model_field: &str) -> &str {
    model_field.strip_prefix("anemoi-").unwrap_or(model_field)
}

fn decision_action_str(action: &DecisionAction) -> String {
    serde_json::to_value(action)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_default()
}

#[derive(Serialize)]
struct ModelCatalog {
    object: &'static str,
    data: Vec<ModelCatalogEntry>,
}

#[derive(Serialize)]
struct ModelCatalogEntry {
    id: String,
    object: &'static str,
    owned_by: &'static str,
    anemoi_domain: bool,
}

/// `GET /v1/models` — OpenAI-format catalog of governance **domains** (issue
/// #15). Runtime model ids never appear here; the caller selects a domain and
/// Anemoi governs which model serves it. No live inspection is triggered.
async fn list_models(State(state): State<AppState>) -> Json<ModelCatalog> {
    let mut data: Vec<ModelCatalogEntry> = state
        .config
        .domains
        .keys()
        .map(|domain| ModelCatalogEntry {
            id: domain.to_string(),
            object: "model",
            owned_by: "anemoi",
            anemoi_domain: true,
        })
        .collect();
    data.sort_by(|a, b| a.id.cmp(&b.id));
    Json(ModelCatalog {
        object: "list",
        data,
    })
}

/// Builds an OpenAI-style structured error. When a decision was already made,
/// its id is included in the body and echoed as `X-Anemoi-Decision-Id` so the
/// caller can query `/explain/:id`.
fn gateway_error(
    status: StatusCode,
    message: impl Into<String>,
    decision_id: Option<Uuid>,
) -> Response {
    let body = serde_json::json!({
        "error": {
            "message": message.into(),
            "type": "anemoi_gateway_error",
            "decision_id": decision_id.map(|id| id.to_string()),
        }
    });
    let mut response = (status, Json(body)).into_response();
    if let Some(id) = decision_id {
        if let Ok(value) = HeaderValue::from_str(&id.to_string()) {
            response.headers_mut().insert("x-anemoi-decision-id", value);
        }
    }
    response
}

/// `POST /v1/chat/completions` — OpenAI-compatible inference forwarding. The
/// caller names a domain in `model`; Anemoi decides which runtime model serves
/// it, records the decision, rewrites `model` to the selected model, forwards
/// to the runtime, and streams the response back. The caller never selects a
/// runtime model directly.
async fn chat_completions(
    State(state): State<AppState>,
    Json(mut body): Json<serde_json::Value>,
) -> Response {
    let Some(model_field) = body
        .get("model")
        .and_then(|value| value.as_str())
        .map(str::to_string)
    else {
        return gateway_error(
            StatusCode::BAD_REQUEST,
            "request is missing a string `model` field",
            None,
        );
    };
    let domain = resolve_domain(&model_field).to_string();

    // Unknown domain is rejected before any decision or forwarding.
    if !state.config.domains.contains_key(&DomainId(domain.clone())) {
        return gateway_error(
            StatusCode::BAD_REQUEST,
            format!("unknown domain `{domain}`"),
            None,
        );
    }

    // Run the same decision path as POST /decide; this records telemetry.
    let request = InferenceRequest {
        id: RequestId::new(),
        domain: DomainId(domain.clone()),
        mode: ExecutionMode::Interactive,
        prompt_tokens_estimate: None,
        max_output_tokens: None,
        latency_budget_ms: Some(DEFAULT_GATEWAY_LATENCY_BUDGET_MS),
        quality_floor: None,
        escalation_intent: None,
    };
    let decision = match state.decide(&request).await {
        Ok(decision) => decision,
        Err(error) => {
            return gateway_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string(), None)
        }
    };

    let (Some(selected_model), Some(selected_runtime)) = (
        decision.selected_model.clone(),
        decision.selected_runtime.clone(),
    ) else {
        return gateway_error(
            StatusCode::SERVICE_UNAVAILABLE,
            format!(
                "no runnable model for domain `{domain}`: {}",
                decision.explanation.summary
            ),
            Some(decision.id),
        );
    };

    let runtime_id = selected_runtime.to_string();
    let is_mock = state.runtime_adapter_type(&runtime_id) == Some("mock");
    if !is_mock && !state.live_execute_enabled() {
        return gateway_error(
            StatusCode::FORBIDDEN,
            "forwarding to a non-mock runtime requires ANEMOI_ENABLE_LIVE_EXECUTE=1",
            Some(decision.id),
        );
    }

    // The caller's domain hint is replaced with the governed model id.
    body["model"] = serde_json::Value::String(selected_model.to_string());

    let forwarded = if is_mock {
        anemoi_runtime::mock_chat_completion(&selected_model.to_string())
    } else {
        let Some(target) = state.runtime_forward_target(&runtime_id) else {
            return gateway_error(
                StatusCode::BAD_GATEWAY,
                format!("runtime `{runtime_id}` has no base_url configured"),
                Some(decision.id),
            );
        };
        match anemoi_runtime::forward_chat_completion(&target, &body).await {
            Ok(forwarded) => forwarded,
            Err(error) => {
                return gateway_error(
                    StatusCode::BAD_GATEWAY,
                    format!("runtime forward failed: {error}"),
                    Some(decision.id),
                )
            }
        }
    };

    gateway_stream_response(forwarded, &decision, &selected_model)
}

fn gateway_stream_response(
    forwarded: ForwardedChatCompletion,
    decision: &Decision,
    selected_model: &ModelId,
) -> Response {
    let status = StatusCode::from_u16(forwarded.status).unwrap_or(StatusCode::OK);
    let content_type = forwarded
        .content_type
        .unwrap_or_else(|| "text/event-stream".to_string());
    let builder = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, content_type)
        .header("x-anemoi-decision-id", decision.id.to_string())
        .header("x-anemoi-selected-model", selected_model.to_string())
        .header("x-anemoi-action", decision_action_str(&decision.action));
    match builder.body(Body::from_stream(forwarded.body)) {
        Ok(response) => response,
        Err(error) => gateway_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            error.to_string(),
            Some(decision.id),
        ),
    }
}
