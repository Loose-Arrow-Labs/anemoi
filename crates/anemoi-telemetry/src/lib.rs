use anemoi_core::{ActionPlan, Decision, ResidencyState, RuntimeSnapshot};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum TelemetryError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),
}

/// Parameters for recording a resident state transition (issue #12). Bundled
/// into one borrowing struct so the `DecisionLog` trait method stays within
/// argument limits and callers name each field at the call site.
pub struct ResidentTransitionRecord<'a> {
    pub model_id: &'a str,
    pub runtime_id: &'a str,
    pub from_state: ResidencyState,
    pub to_state: ResidencyState,
    pub observed_at: DateTime<Utc>,
    pub evidence_source: &'a str,
    pub decision_id: Option<Uuid>,
    pub note: Option<&'a str>,
}

#[async_trait]
pub trait DecisionLog: Send + Sync {
    async fn record_decision(&self, decision: &Decision) -> Result<(), TelemetryError>;
    async fn get_decision(&self, id: Uuid) -> Result<Option<Decision>, TelemetryError>;
    async fn list_decisions(&self) -> Result<Vec<Decision>, TelemetryError>;

    /// Records a resident state transition observed by the reconciliation loop
    /// (issue #12). Default no-op so the event store stays optional: the
    /// in-memory and JSONL logs silently ignore transitions (no database = no
    /// resident events, no error). Only [`SqliteEventStore`] persists them.
    async fn record_resident_transition(
        &self,
        _transition: ResidentTransitionRecord<'_>,
    ) -> Result<(), TelemetryError> {
        Ok(())
    }
}

pub type DynDecisionLog = Arc<dyn DecisionLog>;

/// SQLite-backed durable event store.
///
/// SQLite is the source of truth: `get_decision`/`list_decisions` read from the
/// database, so decisions survive a process restart. A `parking_lot::Mutex`
/// serializes access to the single connection across async contexts.
pub struct SqliteEventStore {
    conn: Mutex<Connection>,
}

impl SqliteEventStore {
    /// Opens (creating if needed) a SQLite database at `path` and ensures the
    /// event tables exist. Reopening the same path sees previously written rows.
    pub fn create(path: impl AsRef<Path>) -> Result<Self, TelemetryError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }

        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL;")?;

        let store = Self {
            conn: Mutex::new(conn),
        };
        store.init_tables()?;
        Ok(store)
    }

    fn init_tables(&self) -> Result<(), TelemetryError> {
        // All event tables are append-only. `resident_events` follows the
        // schema in GitHub issue #12: evidence_source is NOT NULL (a transition
        // is never recorded anonymously); decision_id and note are nullable
        // because observation-only transitions have no triggering decision.
        self.conn.lock().execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS decisions (
                id TEXT PRIMARY KEY,
                decision_json TEXT NOT NULL,
                recorded_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS runtime_snapshots (
                event_id TEXT NOT NULL,
                runtime_id TEXT NOT NULL,
                snapshot_json TEXT NOT NULL,
                observed_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS resident_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                model_id TEXT NOT NULL,
                runtime_id TEXT NOT NULL,
                from_state TEXT NOT NULL,
                to_state TEXT NOT NULL,
                observed_at TEXT NOT NULL,
                evidence_source TEXT NOT NULL,
                decision_id TEXT,
                note TEXT
            );
            CREATE TABLE IF NOT EXISTS staging_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                intent_id TEXT NOT NULL,
                decision_id TEXT NOT NULL,
                background_model TEXT NOT NULL,
                target_runtime TEXT NOT NULL,
                reason TEXT NOT NULL,
                state TEXT NOT NULL,
                recorded_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS action_plan_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                decision_id TEXT NOT NULL,
                plan_json TEXT NOT NULL,
                recorded_at TEXT NOT NULL
            );
            "#,
        )?;
        Ok(())
    }

    /// Records a runtime snapshot event with the time it was observed.
    pub fn record_runtime_snapshot(
        &self,
        id: Uuid,
        runtime_id: &str,
        snapshot: &RuntimeSnapshot,
        observed_at: DateTime<Utc>,
    ) -> Result<(), TelemetryError> {
        let json = serde_json::to_string(snapshot)?;
        self.conn.lock().execute(
            "INSERT INTO runtime_snapshots (event_id, runtime_id, snapshot_json, observed_at) VALUES (?1, ?2, ?3, ?4)",
            params![id.to_string(), runtime_id, &json, observed_at.to_rfc3339()],
        )?;
        Ok(())
    }

    /// Records a resident state transition (issue #12). `evidence_source` is
    /// required — which adapter and inspection round observed the transition —
    /// so a transition is never anonymous. `decision_id` is `None` for
    /// observation-only transitions that no decision triggered.
    // The argument list mirrors the issue #12 `resident_events` columns; each is
    // a distinct, required field rather than incidental parameter sprawl.
    #[allow(clippy::too_many_arguments)]
    pub fn record_resident_event(
        &self,
        model_id: &str,
        runtime_id: &str,
        from_state: &ResidencyState,
        to_state: &ResidencyState,
        observed_at: DateTime<Utc>,
        evidence_source: &str,
        decision_id: Option<Uuid>,
        note: Option<&str>,
    ) -> Result<(), TelemetryError> {
        self.conn.lock().execute(
            r#"INSERT INTO resident_events
               (model_id, runtime_id, from_state, to_state, observed_at, evidence_source, decision_id, note)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)"#,
            params![
                model_id,
                runtime_id,
                residency_state_str(from_state),
                residency_state_str(to_state),
                observed_at.to_rfc3339(),
                evidence_source,
                decision_id.map(|id| id.to_string()),
                note,
            ],
        )?;
        Ok(())
    }

    /// Reads every resident transition for a model in insert order.
    pub fn resident_events(&self, model_id: &str) -> Result<Vec<ResidentEvent>, TelemetryError> {
        let guard = self.conn.lock();
        let mut stmt = guard.prepare(
            r#"SELECT model_id, runtime_id, from_state, to_state, observed_at,
                      evidence_source, decision_id, note
               FROM resident_events WHERE model_id = ?1 ORDER BY id"#,
        )?;
        let rows = stmt.query_map(params![model_id], |row| {
            Ok(ResidentEvent {
                model_id: row.get(0)?,
                runtime_id: row.get(1)?,
                from_state: row.get(2)?,
                to_state: row.get(3)?,
                observed_at: row.get(4)?,
                evidence_source: row.get(5)?,
                decision_id: row.get(6)?,
                note: row.get(7)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Records a staging event.
    pub fn record_staging_event(
        &self,
        intent_id: Uuid,
        decision_id: Uuid,
        background_model: &str,
        target_runtime: &str,
        reason: &str,
        state: &str,
    ) -> Result<(), TelemetryError> {
        self.conn.lock().execute(
            "INSERT INTO staging_events
               (intent_id, decision_id, background_model, target_runtime, reason, state, recorded_at)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                intent_id.to_string(),
                decision_id.to_string(),
                background_model,
                target_runtime,
                reason,
                state,
                Utc::now().to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    /// Records an action plan event.
    pub fn record_action_plan_event(&self, plan: &ActionPlan) -> Result<(), TelemetryError> {
        let json = serde_json::to_string(plan)?;
        self.conn.lock().execute(
            "INSERT INTO action_plan_events (decision_id, plan_json, recorded_at) VALUES (?1, ?2, ?3)",
            params![plan.decision_id.to_string(), &json, Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    /// Replays a decision's explanation from durable storage, used by
    /// `/explain/:id` to answer why a decision happened after a restart.
    pub fn get_decision_explanation(
        &self,
        id: Uuid,
    ) -> Result<Option<anemoi_core::Explanation>, TelemetryError> {
        Ok(self.read_decision(id)?.map(|decision| decision.explanation))
    }

    fn read_decision(&self, id: Uuid) -> Result<Option<Decision>, TelemetryError> {
        let guard = self.conn.lock();
        let mut stmt = guard.prepare("SELECT decision_json FROM decisions WHERE id = ?1")?;
        let json: Option<String> = stmt
            .query_row(params![id.to_string()], |row| row.get(0))
            .optional()?;
        json.map(|json| serde_json::from_str(&json).map_err(Into::into))
            .transpose()
    }
}

/// A resident transition row read back from `resident_events`. States are the
/// snake_case `ResidencyState` strings as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResidentEvent {
    pub model_id: String,
    pub runtime_id: String,
    pub from_state: String,
    pub to_state: String,
    pub observed_at: String,
    pub evidence_source: String,
    pub decision_id: Option<String>,
    pub note: Option<String>,
}

fn residency_state_str(state: &ResidencyState) -> String {
    serde_json::to_value(state)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_default()
}

#[async_trait]
impl DecisionLog for SqliteEventStore {
    async fn record_decision(&self, decision: &Decision) -> Result<(), TelemetryError> {
        let json = serde_json::to_string(decision)?;
        self.conn.lock().execute(
            "INSERT OR REPLACE INTO decisions (id, decision_json, recorded_at) VALUES (?1, ?2, ?3)",
            params![decision.id.to_string(), &json, Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    async fn get_decision(&self, id: Uuid) -> Result<Option<Decision>, TelemetryError> {
        self.read_decision(id)
    }

    async fn list_decisions(&self) -> Result<Vec<Decision>, TelemetryError> {
        let guard = self.conn.lock();
        let mut stmt = guard.prepare("SELECT decision_json FROM decisions ORDER BY rowid")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut decisions = Vec::new();
        for json in rows {
            decisions.push(serde_json::from_str(&json?)?);
        }
        Ok(decisions)
    }

    async fn record_resident_transition(
        &self,
        transition: ResidentTransitionRecord<'_>,
    ) -> Result<(), TelemetryError> {
        self.record_resident_event(
            transition.model_id,
            transition.runtime_id,
            &transition.from_state,
            &transition.to_state,
            transition.observed_at,
            transition.evidence_source,
            transition.decision_id,
            transition.note,
        )
    }
}

/// Builds the decision log for the daemon and CLI.
///
/// Precedence: `ANEMOI_DATABASE_URL` (a `sqlite://` URL) selects the durable
/// SQLite store; otherwise a JSONL path appends to a file-backed log; otherwise
/// an in-memory log. This is the single place production wires the store, so a
/// `sqlite://` URL reaches `SqliteEventStore` from the real binary.
pub fn default_decision_log(jsonl_path: Option<PathBuf>) -> Result<DynDecisionLog, TelemetryError> {
    let database_url = std::env::var("ANEMOI_DATABASE_URL")
        .ok()
        .filter(|url| !url.is_empty());
    decision_log_from(database_url.as_deref(), jsonl_path)
}

/// Pure constructor used by `default_decision_log` and by tests, so behavior can
/// be exercised without mutating process environment.
pub fn decision_log_from(
    database_url: Option<&str>,
    jsonl_path: Option<PathBuf>,
) -> Result<DynDecisionLog, TelemetryError> {
    if let Some(url) = database_url {
        let path = sqlite_path_from_url(url).ok_or_else(|| {
            TelemetryError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("unsupported ANEMOI_DATABASE_URL: {url}"),
            ))
        })?;
        return Ok(Arc::new(SqliteEventStore::create(path)?));
    }
    if let Some(path) = jsonl_path {
        return Ok(Arc::new(JsonlDecisionLog::new(path)?));
    }
    Ok(Arc::new(InMemoryDecisionLog::default()))
}

/// Parses a `sqlite://` URL into a filesystem path. `sqlite:///var/lib/x.db`
/// yields the absolute Unix path `/var/lib/x.db`; on Windows a leading slash
/// before a drive letter (`/C:/x.db`) is dropped.
fn sqlite_path_from_url(url: &str) -> Option<PathBuf> {
    let rest = url.strip_prefix("sqlite://")?;
    let bytes = rest.as_bytes();
    let trimmed = if bytes.first() == Some(&b'/')
        && bytes.get(1).is_some_and(u8::is_ascii_alphabetic)
        && bytes.get(2) == Some(&b':')
    {
        &rest[1..]
    } else {
        rest
    };
    Some(PathBuf::from(trimmed))
}

#[derive(Debug, Default)]
pub struct InMemoryDecisionLog {
    state: std::sync::RwLock<InMemoryDecisionLogState>,
}

impl InMemoryDecisionLog {
    pub fn new() -> Self {
        Self::default()
    }
}

#[derive(Debug, Default)]
struct InMemoryDecisionLogState {
    decisions: HashMap<Uuid, Decision>,
    order: Vec<Uuid>,
}

#[async_trait]
impl DecisionLog for InMemoryDecisionLog {
    async fn record_decision(&self, decision: &Decision) -> Result<(), TelemetryError> {
        let mut state = self.state.write().unwrap();
        if !state.decisions.contains_key(&decision.id) {
            state.order.push(decision.id);
        }
        state.decisions.insert(decision.id, decision.clone());
        Ok(())
    }

    async fn get_decision(&self, id: Uuid) -> Result<Option<Decision>, TelemetryError> {
        let state = self.state.read().unwrap();
        Ok(state.decisions.get(&id).cloned())
    }

    async fn list_decisions(&self) -> Result<Vec<Decision>, TelemetryError> {
        let state = self.state.read().unwrap();
        Ok(state
            .order
            .iter()
            .filter_map(|id| state.decisions.get(id).cloned())
            .collect())
    }
}

#[derive(Debug)]
pub struct JsonlDecisionLog {
    memory: InMemoryDecisionLog,
    path: PathBuf,
}

impl JsonlDecisionLog {
    pub fn new(path: impl Into<PathBuf>) -> Result<Self, TelemetryError> {
        let path = path.into();
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        Ok(Self {
            memory: InMemoryDecisionLog::default(),
            path,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[async_trait]
impl DecisionLog for JsonlDecisionLog {
    async fn record_decision(&self, decision: &Decision) -> Result<(), TelemetryError> {
        self.memory.record_decision(decision).await?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        serde_json::to_writer(&mut file, decision)?;
        file.write_all(b"\n")?;
        Ok(())
    }

    async fn get_decision(&self, id: Uuid) -> Result<Option<Decision>, TelemetryError> {
        self.memory.get_decision(id).await
    }

    async fn list_decisions(&self) -> Result<Vec<Decision>, TelemetryError> {
        self.memory.list_decisions().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anemoi_core::{DecisionAction, DecisionScore, Explanation, RejectedOption, RequestId};
    use std::sync::Arc;

    fn sample_decision() -> Decision {
        sample_decision_with_summary("No runnable model candidate was available.")
    }

    fn sample_decision_with_summary(summary: &str) -> Decision {
        Decision {
            id: Uuid::new_v4(),
            request_id: RequestId::new(),
            action: DecisionAction::Deny,
            selected_model: None,
            selected_runtime: None,
            selected_group: None,
            background_model: None,
            score: DecisionScore::default(),
            explanation: Explanation {
                summary: summary.to_string(),
                reasons: Vec::new(),
                rejected_options: Vec::<RejectedOption>::new(),
            },
            created_at: Utc::now(),
        }
    }

    fn temp_jsonl_path() -> PathBuf {
        std::env::temp_dir().join(format!("anemoi-{}.jsonl", Uuid::new_v4()))
    }

    #[tokio::test]
    async fn memory_decision_log_stores_and_gets_decision() {
        let log = InMemoryDecisionLog::default();
        let decision = sample_decision();
        log.record_decision(&decision).await.expect("record");
        assert_eq!(
            log.get_decision(decision.id).await.expect("get"),
            Some(decision)
        );
    }

    #[tokio::test]
    async fn memory_decision_log_returns_none_for_unknown_decision() {
        let log = InMemoryDecisionLog::default();
        let found = log.get_decision(Uuid::new_v4()).await.expect("get");
        assert_eq!(found, None);
    }

    #[tokio::test]
    async fn memory_decision_log_keeps_recent_decisions_in_insert_order() {
        let log = InMemoryDecisionLog::default();
        let first = sample_decision_with_summary("first");
        let second = sample_decision_with_summary("second");
        log.record_decision(&second).await.expect("record second");
        log.record_decision(&first).await.expect("record first");
        assert_eq!(
            log.list_decisions()
                .await
                .expect("list")
                .into_iter()
                .map(|d| d.explanation.summary)
                .collect::<Vec<_>>(),
            vec!["second", "first"]
        );
    }

    #[tokio::test]
    async fn jsonl_decision_log_appends_one_json_object_per_decision() {
        let path = temp_jsonl_path();
        let log = JsonlDecisionLog::new(&path).expect("jsonl log");
        let first = sample_decision_with_summary("first");
        let second = sample_decision_with_summary("second");
        log.record_decision(&first).await.expect("record first");
        log.record_decision(&second).await.expect("record second");
        let text = std::fs::read_to_string(&path).expect("jsonl file");
        let lines: Vec<_> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(
            serde_json::from_str::<Decision>(lines[0]).expect("first json"),
            first
        );
        assert_eq!(
            serde_json::from_str::<Decision>(lines[1]).expect("second json"),
            second
        );
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn jsonl_decision_log_creates_parent_directory_when_needed() {
        let dir = std::env::temp_dir().join(format!("anemoi-{}", Uuid::new_v4()));
        let path = dir.join("nested").join("decisions.jsonl");
        let log = JsonlDecisionLog::new(&path).expect("jsonl log");
        log.record_decision(&sample_decision())
            .await
            .expect("record");
        assert!(path.exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn jsonl_decision_log_does_not_require_sqlite() {
        let log = default_decision_log(None).expect("memory log");
        let decision = sample_decision();
        log.record_decision(&decision).await.expect("record");
        assert_eq!(
            log.get_decision(decision.id).await.expect("get"),
            Some(decision)
        );
    }

    #[tokio::test]
    async fn telemetry_trait_supports_memory_and_jsonl_logs() {
        let memory: DynDecisionLog = Arc::new(InMemoryDecisionLog::default());
        let jsonl_path = temp_jsonl_path();
        let jsonl: DynDecisionLog = Arc::new(JsonlDecisionLog::new(&jsonl_path).expect("jsonl"));
        for log in [memory, jsonl] {
            let decision = sample_decision();
            log.record_decision(&decision).await.expect("record");
            assert_eq!(
                log.get_decision(decision.id).await.expect("get"),
                Some(decision)
            );
        }
        let _ = std::fs::remove_file(jsonl_path);
    }

    #[tokio::test]
    async fn decision_log_defaults_to_memory() {
        let log = default_decision_log(None).expect("default log");
        let decision = sample_decision();
        log.record_decision(&decision)
            .await
            .expect("record decision");
        let found = log
            .get_decision(decision.id)
            .await
            .expect("get decision")
            .expect("exists");
        assert_eq!(found.id, decision.id);
    }

    #[tokio::test]
    async fn jsonl_decision_log_appends_decisions() {
        let path = std::env::temp_dir().join(format!("anemoi-{}.jsonl", Uuid::new_v4()));
        let log = JsonlDecisionLog::new(&path).expect("jsonl log");
        let decision = sample_decision();
        log.record_decision(&decision)
            .await
            .expect("record decision");
        let text = std::fs::read_to_string(&path).expect("jsonl file");
        assert!(text.contains(&decision.id.to_string()));
        let _ = std::fs::remove_file(path);
    }

    // =======================================================================
    // SQLite Event Store Tests (Prompt 27)
    // =======================================================================

    fn temp_db_path() -> PathBuf {
        std::env::temp_dir().join(format!("anemoi-{}.db", Uuid::new_v4()))
    }

    fn sample_snapshot() -> RuntimeSnapshot {
        RuntimeSnapshot {
            runtime_id: anemoi_core::RuntimeId("mock".to_string()),
            available: true,
            residents: Vec::new(),
            configured_models: vec![],
            memory: anemoi_core::RuntimeMemorySnapshot::default(),
            active_requests: Vec::new(),
        }
    }

    #[test]
    fn sqlite_event_store_records_decision_event() {
        let db_path = temp_db_path();
        let decision = sample_decision_with_summary("Denied: no runnable candidate.");

        // Record through the original store, then drop it.
        {
            let store = SqliteEventStore::create(&db_path).expect("sqlite event store");
            futures::executor::block_on(store.record_decision(&decision)).expect("record");
        }

        // Reopen a fresh store on the same file: the decision survives the
        // "restart" and round-trips identically.
        let reopened = SqliteEventStore::create(&db_path).expect("reopen sqlite event store");
        let stored = futures::executor::block_on(reopened.get_decision(decision.id))
            .expect("get")
            .expect("decision is durable across reopen");
        assert_eq!(stored, decision);

        let _ = std::fs::remove_file(&db_path);
    }

    #[test]
    fn sqlite_event_store_records_runtime_snapshot_event() {
        let db_path = temp_db_path();
        let snapshot = sample_snapshot();
        let event_id = Uuid::new_v4();
        let observed_at = Utc::now();

        {
            let store = SqliteEventStore::create(&db_path).expect("sqlite event store");
            store
                .record_runtime_snapshot(event_id, "mock", &snapshot, observed_at)
                .expect("record snapshot");
        }

        // Reopen and read the row back: the stored JSON deserializes to the
        // same snapshot and keeps the observed-at timestamp.
        let conn = rusqlite::Connection::open(&db_path).expect("conn");
        let (json, stored_observed_at): (String, String) = conn
            .query_row(
                "SELECT snapshot_json, observed_at FROM runtime_snapshots WHERE event_id = ?1",
                params![event_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("snapshot row");
        let roundtrip: RuntimeSnapshot = serde_json::from_str(&json).expect("snapshot json");
        assert_eq!(roundtrip, snapshot);
        assert_eq!(stored_observed_at, observed_at.to_rfc3339());

        let _ = std::fs::remove_file(&db_path);
    }

    #[test]
    fn sqlite_event_store_records_resident_event() {
        // Issue #12: a resident transition is recorded with a required
        // evidence_source and survives a reopen, read back via resident_events.
        let db_path = temp_db_path();
        let observed_at = Utc::now();
        let decision_id = Uuid::new_v4();

        {
            let store = SqliteEventStore::create(&db_path).expect("sqlite event store");
            store
                .record_resident_event(
                    "qwen35_a3b",
                    "mock",
                    &ResidencyState::WarmCpu,
                    &ResidencyState::HotGpu,
                    observed_at,
                    "mock-adapter inspect round 7",
                    Some(decision_id),
                    Some("promoted to GPU"),
                )
                .expect("record resident event");
        }

        let reopened = SqliteEventStore::create(&db_path).expect("reopen sqlite event store");
        let events = reopened
            .resident_events("qwen35_a3b")
            .expect("resident events");
        assert_eq!(
            events,
            vec![ResidentEvent {
                model_id: "qwen35_a3b".to_string(),
                runtime_id: "mock".to_string(),
                from_state: "warm_cpu".to_string(),
                to_state: "hot_gpu".to_string(),
                observed_at: observed_at.to_rfc3339(),
                evidence_source: "mock-adapter inspect round 7".to_string(),
                decision_id: Some(decision_id.to_string()),
                note: Some("promoted to GPU".to_string()),
            }]
        );

        let _ = std::fs::remove_file(&db_path);
    }

    #[test]
    fn sqlite_event_store_records_staging_event() {
        let db_path = temp_db_path();
        let decision_id = Uuid::new_v4();
        let intent_id = Uuid::new_v4();

        {
            let store = SqliteEventStore::create(&db_path).expect("sqlite event store");
            store
                .record_staging_event(
                    intent_id,
                    decision_id,
                    "qwen35_a3b",
                    "mock",
                    "background staging test",
                    "pending",
                )
                .expect("record staging");
        }

        // Reopen the file and read the stored columns back.
        let conn = rusqlite::Connection::open(&db_path).expect("conn");
        let (background_model, target_runtime, reason, state): (String, String, String, String) =
            conn.query_row(
                "SELECT background_model, target_runtime, reason, state
                 FROM staging_events WHERE intent_id = ?1",
                params![intent_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .expect("staging row");
        assert_eq!(background_model, "qwen35_a3b");
        assert_eq!(target_runtime, "mock");
        assert_eq!(reason, "background staging test");
        assert_eq!(state, "pending");

        let _ = std::fs::remove_file(&db_path);
    }

    #[test]
    fn sqlite_event_store_records_action_plan_event() {
        let db_path = temp_db_path();
        let decision_id = Uuid::new_v4();
        let plan = ActionPlan::new(decision_id, true);

        {
            let store = SqliteEventStore::create(&db_path).expect("sqlite event store");
            store
                .record_action_plan_event(&plan)
                .expect("record action plan");
        }

        // Reopen the file: the stored plan JSON round-trips to the same plan.
        let conn = rusqlite::Connection::open(&db_path).expect("conn");
        let (stored_decision_id, json): (String, String) = conn
            .query_row(
                "SELECT decision_id, plan_json FROM action_plan_events WHERE decision_id = ?1",
                params![decision_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("action plan row");
        assert_eq!(stored_decision_id, decision_id.to_string());
        let roundtrip: ActionPlan = serde_json::from_str(&json).expect("plan json");
        assert_eq!(roundtrip, plan);

        let _ = std::fs::remove_file(&db_path);
    }

    #[test]
    fn sqlite_event_store_replays_decision_explanation_by_id() {
        let db_path = temp_db_path();
        let decision = sample_decision_with_summary("Selected hot model.");

        // Record a decision through the DecisionLog trait, then drop the store.
        {
            let log: DynDecisionLog =
                Arc::new(SqliteEventStore::create(&db_path).expect("sqlite event store"));
            futures::executor::block_on(log.record_decision(&decision)).expect("record");
        }

        // Reopen and replay the explanation by id.
        let reopened = SqliteEventStore::create(&db_path).expect("reopen sqlite event store");
        let explanation = reopened
            .get_decision_explanation(decision.id)
            .expect("get explanation")
            .expect("explanation is durable across reopen");
        assert_eq!(explanation, decision.explanation);

        let _ = std::fs::remove_file(&db_path);
    }
}
