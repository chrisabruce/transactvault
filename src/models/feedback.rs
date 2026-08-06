//! `feedback` table — notes signed-in users leave via the floating
//! widget, triaged on `/admin/feedback`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use surrealdb::types::{RecordId, SurrealValue};

/// One submitted note. `user_name` / `user_email` are denormalized at
/// submit time so the row still reads sensibly if the author's account
/// is later deleted; `user` stays a live link for the admin's
/// jump-to-profile.
#[derive(Debug, Clone, Serialize, Deserialize, SurrealValue)]
pub struct Feedback {
    pub id: RecordId,
    /// `feedback` (in-app widget) or `contact` (public form).
    #[serde(default = "default_kind")]
    pub kind: String,
    pub user: Option<RecordId>,
    pub user_name: String,
    pub user_email: String,
    pub brokerage_name: Option<String>,
    pub body: String,
    /// Path of the page the widget was opened on (from the Referer),
    /// e.g. `/app/transactions/xyz` — often the whole bug report.
    pub page: Option<String>,
    /// `open` | `resolved`.
    pub status: String,
    /// Sender IP — populated for anonymous contacts only.
    pub ip: Option<String>,
    pub resolved_by: Option<String>,
    pub resolved_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, SurrealValue)]
pub struct NewFeedback {
    pub kind: String,
    pub user: Option<RecordId>,
    pub user_name: String,
    pub user_email: String,
    pub brokerage_name: Option<String>,
    pub body: String,
    pub page: Option<String>,
    pub ip: Option<String>,
}

fn default_kind() -> String {
    "feedback".to_string()
}

impl Feedback {
    /// True for messages from the public contact form, which may have
    /// no account behind them.
    pub fn is_contact(&self) -> bool {
        self.kind == "contact"
    }

    /// URL-safe record key for building the resolve/delete action URLs.
    pub fn key(&self) -> String {
        crate::db::record_key(&self.id)
    }

    pub fn is_resolved(&self) -> bool {
        self.status == "resolved"
    }
}
