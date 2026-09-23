use crate::{Decision, DecisionLog, TelemetryError};
use async_trait::async_trait;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use uuid::Uuid;

use super::InMemoryDecisionLog;

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
        let memory = InMemoryDecisionLog::default();
        // Rehydrate the in-memory index from any existing log so decisions are
        // readable after a process restart (AGENTS.md §11: durability requires a
        // restart round-trip). Blank lines and lines that fail to parse (e.g. a
        // torn final record from an interrupted append) are skipped so a partial
        // write never prevents startup.
        if path.exists() {
            for line in std::fs::read_to_string(&path)?.lines() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                if let Ok(decision) = serde_json::from_str::<Decision>(line) {
                    memory.insert_sync(decision);
                }
            }
        }
        Ok(Self { memory, path })
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
