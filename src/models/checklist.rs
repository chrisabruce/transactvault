//! `checklist_item` table — one compliance step on a transaction.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use surrealdb::types::{RecordId, SurrealValue};

use crate::record_key;

#[derive(Debug, Clone, Serialize, Deserialize, SurrealValue)]
pub struct ChecklistItem {
    pub id: RecordId,
    pub title: String,
    pub category: String,
    pub position: i64,
    pub completed: bool,
    pub completed_at: Option<DateTime<Utc>>,
    pub completed_by: Option<RecordId>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl ChecklistItem {
    pub fn url_key(&self) -> String {
        record_key(&self.id)
    }

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
pub struct NewChecklistItem {
    pub title: String,
    pub category: String,
    pub position: i64,
}

/// Canonical starter checklist every new transaction receives. Brokers can
/// add their own items; this set matches California TC standards.
pub const DEFAULT_CHECKLIST: &[(&str, &str)] = &[
    ("Purchase Contract signed", "contract"),
    ("Seller Disclosures delivered", "disclosures"),
    ("Transfer Disclosure Statement", "disclosures"),
    ("Natural Hazard Disclosure", "disclosures"),
    ("Home Inspection Report", "inspection"),
    ("Pest Inspection Report", "inspection"),
    ("Appraisal Report", "appraisal"),
    ("Preliminary Title Report", "title"),
    ("Closing Disclosure", "closing"),
    ("Final Settlement Statement", "closing"),
];
