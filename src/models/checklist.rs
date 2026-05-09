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

/// TC review verdict on a single checklist item.
///
/// The agent uploads a document; the row sits at `Pending` until a TC or
/// broker reviews it. `Approved` counts toward the compliance progress
/// bar; `Denied` does not, and a comment thread is the canonical place
/// for the reviewer to explain why.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalStatus {
    Pending,
    Approved,
    Denied,
}

impl ApprovalStatus {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(ApprovalStatus::Pending),
            "approved" => Some(ApprovalStatus::Approved),
            "denied" => Some(ApprovalStatus::Denied),
            _ => None,
        }
    }

    pub fn is_approved(self) -> bool {
        matches!(self, ApprovalStatus::Approved)
    }

    pub fn is_denied(self) -> bool {
        matches!(self, ApprovalStatus::Denied)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, SurrealValue)]
pub struct ChecklistItem {
    pub id: RecordId,
    pub title: String,
    pub form_code: Option<String>,
    pub group_slug: String,
    pub position: i64,
    pub required: bool,
    pub approval_status: String,
    pub reviewed_at: Option<DateTime<Utc>>,
    pub reviewed_by: Option<RecordId>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl ChecklistItem {
    pub fn url_key(&self) -> String {
        record_key(&self.id)
    }

    pub fn status(&self) -> ApprovalStatus {
        ApprovalStatus::parse(&self.approval_status).unwrap_or(ApprovalStatus::Pending)
    }

    pub fn is_approved(&self) -> bool {
        self.status().is_approved()
    }

    pub fn is_denied(&self) -> bool {
        self.status().is_denied()
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
