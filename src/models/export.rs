//! `export_job` / `export_chunk` tables — queued background builds of
//! the brokerage-wide document archive.
//!
//! A job is created from the Exports page and picked up by the
//! background worker ([`crate::export_worker`]), which plans chunks
//! (one per agent + year, split by month / part past the size cap),
//! builds each ZIP, and uploads it to object storage. Chunk rows appear
//! as they finish so the page can show live progress; downloads go
//! straight to the store via short-lived presigned GETs.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use surrealdb::types::{RecordId, SurrealValue};

use crate::db::record_key;

/// Lifecycle of an export job. Stored as the lowercase slug.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportStatus {
    Queued,
    Running,
    Completed,
    Failed,
    Canceled,
}

impl ExportStatus {
    // Status writes happen in SurrealQL literals (worker transitions,
    // cancel), so unlike the transaction enums there is no `as_str` —
    // add one back if a Rust-side writer ever appears.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "queued" => Some(Self::Queued),
            "running" => Some(Self::Running),
            "completed" => Some(Self::Completed),
            "failed" => Some(Self::Failed),
            "canceled" => Some(Self::Canceled),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Queued => "Queued",
            Self::Running => "Building",
            Self::Completed => "Ready",
            Self::Failed => "Failed",
            Self::Canceled => "Canceled",
        }
    }

    /// Still owned by the worker — cancel is offered, delete is not.
    pub fn is_active(self) -> bool {
        matches!(self, Self::Queued | Self::Running)
    }
}

/// One background export build. See the table comment in
/// `db/schema.surql` for field semantics.
#[derive(Debug, Clone, Serialize, Deserialize, SurrealValue)]
pub struct ExportJob {
    pub id: RecordId,
    pub brokerage: RecordId,
    pub requested_by: RecordId,
    pub status: String,
    pub error: Option<String>,
    pub total_bytes: i64,
    pub chunk_total: i64,
    pub chunks_done: i64,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
}

impl ExportJob {
    pub fn url_key(&self) -> String {
        record_key(&self.id)
    }

    pub fn status_enum(&self) -> ExportStatus {
        ExportStatus::parse(&self.status).unwrap_or(ExportStatus::Queued)
    }
}

/// Insert shape for `export_job` — everything else comes from schema
/// defaults (`status = "queued"`, counters at 0, `created_at = now`).
#[derive(Debug, Clone, Serialize, SurrealValue)]
pub struct NewExportJob {
    pub brokerage: RecordId,
    pub requested_by: RecordId,
}

/// One finished chunk ZIP of an export job.
#[derive(Debug, Clone, Serialize, Deserialize, SurrealValue)]
pub struct ExportChunk {
    pub id: RecordId,
    pub job: RecordId,
    pub seq: i64,
    pub label: String,
    pub filename: String,
    pub storage_key: String,
    pub size_bytes: i64,
    pub content_bytes: i64,
    pub doc_count: i64,
    pub tx_count: i64,
    pub created_at: DateTime<Utc>,
}

impl ExportChunk {
    pub fn url_key(&self) -> String {
        record_key(&self.id)
    }

    pub fn size_display(&self) -> String {
        humansize::format_size(self.size_bytes.max(0) as u64, humansize::DECIMAL)
    }
}

/// Insert shape for `export_chunk`.
#[derive(Debug, Clone, Serialize, SurrealValue)]
pub struct NewExportChunk {
    pub job: RecordId,
    pub seq: i64,
    pub label: String,
    pub filename: String,
    pub storage_key: String,
    pub size_bytes: i64,
    pub content_bytes: i64,
    pub doc_count: i64,
    pub tx_count: i64,
}
