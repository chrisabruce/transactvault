//! `checklist_item` table — one compliance step on a transaction.
//!
//! Each item now lives inside a named group (matching the printed CAR
//! checklists: Mandatory Disclosures, Escrow Documents, etc.) and may
//! point at a specific form code in the master CAR library. Custom items
//! created by an agent leave `form_code` as `None`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use surrealdb::types::{RecordId, SurrealValue};

use crate::record_key;

#[derive(Debug, Clone, Serialize, Deserialize, SurrealValue)]
pub struct ChecklistItem {
    pub id: RecordId,
    pub title: String,
    pub form_code: Option<String>,
    pub group_slug: String,
    pub position: i64,
    pub required: bool,
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
}

#[derive(Debug, Clone, Serialize, SurrealValue)]
pub struct NewChecklistItem {
    pub title: String,
    pub form_code: Option<String>,
    pub group_slug: String,
    pub position: i64,
    pub required: bool,
}
