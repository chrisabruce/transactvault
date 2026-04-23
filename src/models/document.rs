//! `document` table — a stored file attached to a transaction.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use surrealdb::types::{RecordId, SurrealValue};

use crate::record_key;

#[derive(Debug, Clone, Serialize, Deserialize, SurrealValue)]
pub struct Document {
    pub id: RecordId,
    pub filename: String,
    pub category: String,
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

    /// First few uppercase characters of the filename extension; used as the
    /// little badge in document lists.
    pub fn extension_badge(&self) -> String {
        match self.filename.rsplit('.').next() {
            Some(ext) if ext.len() <= 5 && !ext.contains(' ') => ext.to_ascii_uppercase(),
            _ => "FILE".into(),
        }
    }

    #[allow(dead_code)]
    pub fn category_label(&self) -> &str {
        match self.category.as_str() {
            "contract" => "Contract",
            "disclosures" => "Disclosures",
            "inspection" => "Inspection",
            "appraisal" => "Appraisal",
            "title" => "Title",
            "closing" => "Closing",
            _ => "General",
        }
    }
}

#[derive(Debug, Clone, Serialize, SurrealValue)]
pub struct NewDocument {
    pub filename: String,
    pub category: String,
    pub storage_key: String,
    pub size_bytes: i64,
    pub content_type: String,
    pub signed: bool,
    pub version: i64,
}
