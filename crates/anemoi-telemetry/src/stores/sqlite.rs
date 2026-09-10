use crate::{
    ActionPlan, ActionPlanEvent, Decision, DecisionLog, RawActionPlanEvent,
    RawRuntimeSnapshotEvent, ResidencyState, ResidentEvent, ResidentTransitionRecord,
    RuntimeSnapshot, RuntimeSnapshotEvent, StagingEvent, TelemetryError,
};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;
use uuid::Uuid;

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
        self.resident_events_for_query(Some(model_id), usize::MAX, false)
    }

    fn resident_events_for_query(
        &self,
        model_id: Option<&str>,
        limit: usize,
        newest_first: bool,
    ) -> Result<Vec<ResidentEvent>, TelemetryError> {
        let limit = normalize_limit(limit);
        let guard = self.conn.lock();
        let order = if newest_first { "DESC" } else { "ASC" };
        let sql = match model_id {
            Some(_) => format!(
                r#"SELECT model_id, runtime_id, from_state, to_state, observed_at,
                          evidence_source, decision_id, note
                   FROM resident_events WHERE model_id = ?1 ORDER BY id {order} LIMIT ?2"#
            ),
            None => format!(
                r#"SELECT model_id, runtime_id, from_state, to_state, observed_at,
                          evidence_source, decision_id, note
                   FROM resident_events ORDER BY id {order} LIMIT ?1"#
            ),
        };
        let mut stmt = guard.prepare(&sql)?;
        let map_row = |row: &rusqlite::Row<'_>| {
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
        };
        let rows = match model_id {
            Some(model_id) => stmt.query_map(params![model_id, limit as i64], map_row)?,
            None => stmt.query_map(params![limit as i64], map_row)?,
        };
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    fn staging_events_for_query(&self, limit: usize) -> Result<Vec<StagingEvent>, TelemetryError> {
        let guard = self.conn.lock();
        let mut stmt = guard.prepare(
            r#"SELECT intent_id, decision_id, background_model, target_runtime,
                      reason, state, recorded_at
               FROM staging_events ORDER BY id DESC LIMIT ?1"#,
        )?;
        let rows = stmt.query_map(params![normalize_limit(limit) as i64], |row| {
            Ok(StagingEvent {
                intent_id: row.get(0)?,
                decision_id: row.get(1)?,
                background_model: row.get(2)?,
                target_runtime: row.get(3)?,
                reason: row.get(4)?,
                state: row.get(5)?,
                recorded_at: row.get(6)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    fn action_plan_events_for_query(
        &self,
        decision_id: Option<Uuid>,
        limit: usize,
    ) -> Result<Vec<ActionPlanEvent>, TelemetryError> {
        let guard = self.conn.lock();
        let sql = match decision_id {
            Some(_) => {
                r#"SELECT decision_id, plan_json, recorded_at
                   FROM action_plan_events WHERE decision_id = ?1 ORDER BY id DESC LIMIT ?2"#
            }
            None => {
                r#"SELECT decision_id, plan_json, recorded_at
                   FROM action_plan_events ORDER BY id DESC LIMIT ?1"#
            }
        };
        let mut stmt = guard.prepare(sql)?;
        let map_row = |row: &rusqlite::Row<'_>| {
            Ok(RawActionPlanEvent {
                decision_id: row.get(0)?,
                plan_json: row.get(1)?,
                recorded_at: row.get(2)?,
            })
        };
        let raw_events = match decision_id {
            Some(decision_id) => stmt
                .query_map(
                    params![decision_id.to_string(), normalize_limit(limit) as i64],
                    map_row,
                )?
                .collect::<Result<Vec<_>, _>>()?,
            None => stmt
                .query_map(params![normalize_limit(limit) as i64], map_row)?
                .collect::<Result<Vec<_>, _>>()?,
        };
        raw_events
            .into_iter()
            .map(|raw| {
                Ok(ActionPlanEvent {
                    decision_id: raw.decision_id,
                    plan: serde_json::from_str(&raw.plan_json)?,
                    recorded_at: raw.recorded_at,
                })
            })
            .collect()
    }

    fn runtime_snapshot_events_for_query(
        &self,
        runtime_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<RuntimeSnapshotEvent>, TelemetryError> {
        let guard = self.conn.lock();
        let sql = match runtime_id {
            Some(_) => {
                r#"SELECT event_id, runtime_id, snapshot_json, observed_at
                   FROM runtime_snapshots WHERE runtime_id = ?1 ORDER BY rowid DESC LIMIT ?2"#
            }
            None => {
                r#"SELECT event_id, runtime_id, snapshot_json, observed_at
                   FROM runtime_snapshots ORDER BY rowid DESC LIMIT ?1"#
            }
        };
        let mut stmt = guard.prepare(sql)?;
        let map_row = |row: &rusqlite::Row<'_>| {
            Ok(RawRuntimeSnapshotEvent {
                event_id: row.get(0)?,
                runtime_id: row.get(1)?,
                snapshot_json: row.get(2)?,
                observed_at: row.get(3)?,
            })
        };
        let raw_events = match runtime_id {
            Some(runtime_id) => stmt
                .query_map(params![runtime_id, normalize_limit(limit) as i64], map_row)?
                .collect::<Result<Vec<_>, _>>()?,
            None => stmt
                .query_map(params![normalize_limit(limit) as i64], map_row)?
                .collect::<Result<Vec<_>, _>>()?,
        };
        raw_events
            .into_iter()
            .map(|raw| {
                Ok(RuntimeSnapshotEvent {
                    event_id: raw.event_id,
                    runtime_id: raw.runtime_id,
                    snapshot: serde_json::from_str(&raw.snapshot_json)?,
                    observed_at: raw.observed_at,
                })
            })
            .collect()
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

    async fn record_runtime_snapshot_event(
        &self,
        id: Uuid,
        runtime_id: &str,
        snapshot: &RuntimeSnapshot,
        observed_at: DateTime<Utc>,
    ) -> Result<(), TelemetryError> {
        self.record_runtime_snapshot(id, runtime_id, snapshot, observed_at)
    }

    async fn record_staging_status(
        &self,
        intent_id: Uuid,
        decision_id: Uuid,
        background_model: &str,
        target_runtime: &str,
        reason: &str,
        state: &str,
    ) -> Result<(), TelemetryError> {
        self.record_staging_event(
            intent_id,
            decision_id,
            background_model,
            target_runtime,
            reason,
            state,
        )
    }

    async fn record_action_plan(&self, plan: &ActionPlan) -> Result<(), TelemetryError> {
        self.record_action_plan_event(plan)
    }

    async fn list_resident_events(
        &self,
        model_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<ResidentEvent>, TelemetryError> {
        self.resident_events_for_query(model_id, limit, true)
    }

    async fn list_staging_events(&self, limit: usize) -> Result<Vec<StagingEvent>, TelemetryError> {
        self.staging_events_for_query(limit)
    }

    async fn list_action_plan_events(
        &self,
        decision_id: Option<Uuid>,
        limit: usize,
    ) -> Result<Vec<ActionPlanEvent>, TelemetryError> {
        self.action_plan_events_for_query(decision_id, limit)
    }

    async fn list_runtime_snapshot_events(
        &self,
        runtime_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<RuntimeSnapshotEvent>, TelemetryError> {
        self.runtime_snapshot_events_for_query(runtime_id, limit)
    }
}

fn normalize_limit(limit: usize) -> usize {
    limit.clamp(1, 500)
}

fn residency_state_str(state: &ResidencyState) -> String {
    serde_json::to_value(state)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_default()
}
