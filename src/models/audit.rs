//! `audit_event` table — append-only security event log.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use surrealdb::types::{RecordId, SurrealValue};

#[derive(Debug, Clone, Serialize, Deserialize, SurrealValue)]
pub struct AuditEvent {
    pub id: RecordId,
    pub kind: String,
    pub actor_email: Option<String>,
    pub actor: Option<RecordId>,
    pub ip: Option<String>,
    pub user_agent: Option<String>,
    pub detail: Option<String>,
    pub at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, SurrealValue)]
pub struct NewAuditEvent {
    pub kind: String,
    pub actor_email: Option<String>,
    pub actor: Option<RecordId>,
    pub ip: Option<String>,
    pub user_agent: Option<String>,
    pub detail: Option<String>,
}

impl AuditEvent {
    /// Stable, human-friendly label used by the admin UI.
    pub fn kind_label(&self) -> &'static str {
        match self.kind.as_str() {
            "signup_pending" => "Signup (pending verify)",
            "signup_blocked_honeypot" => "Signup blocked — honeypot",
            "signup_blocked_pow" => "Signup blocked — proof-of-work",
            "signup_blocked_rate_limit" => "Signup blocked — rate limit",
            "signup_blocked_blacklist" => "Signup blocked — disposable email",
            "signup_blocked_duplicate" => "Signup blocked — duplicate email",
            "verify_success" => "Email verified",
            "verify_failure" => "Verify failed",
            "login_success" => "Login",
            "login_failure" => "Login failed",
            "login_blocked_unverified" => "Login blocked — unverified",
            "logout" => "Logout",
            "invite_sent" => "Invite sent",
            "invite_resent" => "Invite resent",
            "invite_cancelled" => "Invite cancelled",
            "invite_accepted" => "Invite accepted",
            "admin_view" => "Admin view",
            "document_deleted" => "Document deleted",
            _ => "Event",
        }
    }

    /// CSS hook — green for OK, amber for blocked, red for failures.
    pub fn kind_class(&self) -> &'static str {
        match self.kind.as_str() {
            "verify_success" | "login_success" | "invite_accepted" | "signup_pending" => "ok",
            "logout" | "invite_sent" | "invite_resent" | "admin_view" => "neutral",
            "login_failure" | "verify_failure" | "invite_cancelled" | "document_deleted" => "fail",
            _ => "blocked",
        }
    }
}
