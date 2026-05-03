//! `document` table — a stored file attached to a transaction.
//!
//! Documents are grouped under a specific CAR form code so the storage
//! layout follows: brokerage / property folder / form code / file. Each
//! `document` may also be linked to one or more `checklist_item`s via the
//! `for_item` graph edge — the same uploaded contract can fulfil more than
//! one checklist requirement.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use surrealdb::types::{RecordId, SurrealValue};

use crate::record_key;

#[derive(Debug, Clone, Serialize, Deserialize, SurrealValue)]
pub struct Document {
    pub id: RecordId,
    pub filename: String,
    pub form_code: String,
    pub storage_key: String,
    pub size_bytes: i64,
    pub content_type: String,
    pub signed: bool,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

impl Document {
    pub fn url_key(&self) -> String {
        record_key(&self.id)
    }

    /// Human-readable size for list views (`1.2 MB`, `342 KB`, ...).
    pub fn size_display(&self) -> String {
        humansize::format_size(self.size_bytes as u64, humansize::DECIMAL)
    }

    /// First few uppercase characters of the filename extension; used as
    /// the little badge in document lists.
    pub fn extension_badge(&self) -> String {
        match self.filename.rsplit('.').next() {
            Some(ext) if ext.len() <= 5 && !ext.contains(' ') => ext.to_ascii_uppercase(),
            _ => "FILE".into(),
        }
    }

    pub fn form_label(&self) -> String {
        match crate::forms::lookup(&self.form_code) {
            Some(f) => format!("{} — {}", f.code, f.name),
            None => self.form_code.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, SurrealValue)]
pub struct NewDocument {
    pub filename: String,
    pub form_code: String,
    pub storage_key: String,
    pub size_bytes: i64,
    pub content_type: String,
    pub signed: bool,
    pub version: i64,
}
