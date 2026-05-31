//! `audit_event` table — append-only security event log.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use surrealdb::types::{RecordId, SurrealValue};

/// Append-only security event. Persisted by [`crate::audit::record`]
/// on signup attempts (blocked or successful), every login, every
/// admin action, and any tenant-affecting mutation. Drives the
/// brokerage audit log + the super-admin audit page. The `kind`
/// allowlist lives in `db/schema.surql`; keep [`Self::kind_label`]
/// + [`Self::kind_class`] in sync when adding a new kind.
#[derive(Debug, Clone, Serialize, Deserialize, SurrealValue)]
pub struct AuditEvent {
    pub id: RecordId,
    pub kind: String,
    pub actor_email: Option<String>,
    /// Record link to the user that triggered the event when known —
    /// missing on signup-blocked rows because no user exists yet.
    pub actor: Option<RecordId>,
    pub ip: Option<String>,
    pub user_agent: Option<String>,
    /// Free-form context (e.g. `"removed alice@x.com (agent)"` for a
    /// member_removed event). Not user-rendered raw — controllers
    /// template it.
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
            "invite_declined" => "Invite declined",
            "admin_view" => "Admin view",
            "document_deleted" => "Document deleted",
            "profile_updated" => "Profile updated",
            "password_changed" => "Password changed",
            "avatar_updated" => "Avatar updated",
            "brokerage_deleted" => "Brokerage deleted",
            "transaction_deleted" => "Transaction deleted",
            "transaction_reassigned" => "Transaction reassigned",
            "member_removed" => "Member removed",
            "tier_created" => "Tier created",
            "tier_updated" => "Tier updated",
            "brokerage_comp_granted" => "Comp access granted",
            "brokerage_comp_revoked" => "Comp access revoked",
            _ => "Event",
        }
    }

    /// CSS hook — green for OK, amber for blocked, red for failures.
    pub fn kind_class(&self) -> &'static str {
        match self.kind.as_str() {
            "verify_success" | "login_success" | "invite_accepted" | "signup_pending" => "ok",
            "logout"
            | "invite_sent"
            | "invite_resent"
            | "admin_view"
            | "transaction_reassigned" => "neutral",
            "login_failure"
            | "verify_failure"
            | "invite_cancelled"
            | "invite_declined"
            | "document_deleted"
            | "brokerage_deleted"
            | "transaction_deleted"
            | "member_removed" => "fail",
            _ => "blocked",
        }
    }
}
