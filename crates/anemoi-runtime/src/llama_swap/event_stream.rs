use anemoi_core::{ModelResident, ResidencyState};
use futures::stream::StreamExt;
use reqwest::Url;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::{Arc, PoisonError, RwLock};
use std::time::Duration;

use crate::util::normalize_model_id;
use crate::RuntimeError;

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
    pub(crate) fn from_wire(raw: &str) -> Option<Self> {
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
/// [`super::adapter::LlamaSwapAdapter::inspect`].
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
pub(crate) fn parse_model_status_payload(
    payload: &str,
) -> Option<Vec<(String, LlamaSwapModelState)>> {
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
pub(crate) struct SseDecoder {
    buffer: Vec<u8>,
}

impl SseDecoder {
    pub(crate) fn push(&mut self, chunk: &[u8]) -> Vec<Vec<(String, LlamaSwapModelState)>> {
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
pub(crate) fn apply_model_status(
    cache: &ModelStateCache,
    entries: Vec<(String, LlamaSwapModelState)>,
) {
    let mut guard = cache.write().unwrap_or_else(PoisonError::into_inner);
    *guard = entries.into_iter().collect();
}

/// Builds snapshot residents from a model-state cache, omitting models that are
/// not loaded (stopped/shut down). Sorted by model id for deterministic output
/// so reconciliation diffs are stable.
pub(crate) fn residents_from_states(
    states: &HashMap<String, LlamaSwapModelState>,
) -> Vec<ModelResident> {
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
    pub(crate) cache: ModelStateCache,
    pub(crate) handle: tokio::task::JoinHandle<()>,
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

pub(crate) async fn run_event_stream(url: Url, auth_token: Option<String>, cache: ModelStateCache) {
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
