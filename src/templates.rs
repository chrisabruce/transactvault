//! Askama template structs. One file keeps the template catalogue easy to
//! scan; each struct maps 1:1 to an HTML file under `templates/`.
//!
//! Some fields (`signed_in`, `app_name`, `base_url`, `category`) are part of
//! the template-rendering data contract even where a particular page does
//! not currently read them. Allow dead code here so the API stays symmetric.
#![allow(dead_code)]

use askama::Template;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use surrealdb::types::{RecordId, SurrealValue};

use crate::auth::Role;
use crate::forms::{CarForm, FormGroup};
use crate::models::{
    AuditEvent, ChecklistItem, Document, Invitation, SalesType, SpecialSalesCondition, Transaction,
    TransactionStatus, TransactionType,
};

// ---------------------------------------------------------------------------
// Marketing
// ---------------------------------------------------------------------------

#[derive(Template)]
#[template(path = "pages/landing.html")]
pub struct LandingPage<'a> {
    pub app_name: &'a str,
    pub base_url: &'a str,
    pub signed_in: bool,
}

#[derive(Template)]
#[template(path = "pages/pricing.html")]
pub struct PricingPage<'a> {
    pub app_name: &'a str,
    pub base_url: &'a str,
    pub signed_in: bool,
    pub plans: &'a [PricingPlan],
}

/// Public brand book at `/brand` — the canonical guide for designers,
/// vendors, and the internal team. Renders the same content as the
/// downloadable PDF kept at `/static/brand/transactvault-brand-book.pdf`.
#[derive(Template)]
#[template(path = "pages/brand.html")]
pub struct BrandPage<'a> {
    pub app_name: &'a str,
    pub base_url: &'a str,
    pub signed_in: bool,
}

#[derive(Debug, Clone)]
pub struct PricingPlan {
    pub slug: &'static str,
    pub name: &'static str,
    pub price_monthly: u32,
    pub limit: &'static str,
    pub best_for: &'static str,
    pub features: &'static [&'static str],
    pub popular: bool,
}

/// Static catalogue used on the marketing pricing page. Keep this array in
/// sync with the plan `ASSERT` in `db/schema.surql`.
pub const PRICING_PLANS: &[PricingPlan] = &[
    PricingPlan {
        slug: "starter",
        name: "Starter",
        price_monthly: 149,
        limit: "Up to 25 transactions / month",
        best_for: "Solo agents & small teams",
        features: &[
            "Full transaction management",
            "Checklist & compliance workflow",
            "3-year compliant storage",
            "Email support",
        ],
        popular: false,
    },
    PricingPlan {
        slug: "growth",
        name: "Growth",
        price_monthly: 299,
        limit: "Up to 100 transactions / month",
        best_for: "Typical brokerages",
        features: &[
            "Everything in Starter",
            "Priority support",
            "Unlimited team members",
            "Custom checklist templates",
        ],
        popular: true,
    },
    PricingPlan {
        slug: "scale",
        name: "Scale",
        price_monthly: 499,
        limit: "Up to 250 transactions / month",
        best_for: "Growing & mid-size offices",
        features: &[
            "Everything in Growth",
            "Advanced search & reporting",
            "Audit-ready export tooling",
            "Dedicated onboarding",
        ],
        popular: false,
    },
    PricingPlan {
        slug: "enterprise",
        name: "Enterprise",
        price_monthly: 799,
        limit: "Unlimited transactions",
        best_for: "Large & multi-office brokerages",
        features: &[
            "Everything in Scale",
            "Dedicated support contact",
            "Custom compliance policies",
            "Multi-office visibility",
        ],
        popular: false,
    },
];

// ---------------------------------------------------------------------------
// Auth
// ---------------------------------------------------------------------------

#[derive(Template)]
#[template(path = "pages/login.html")]
pub struct LoginPage<'a> {
    pub app_name: &'a str,
    pub base_url: &'a str,
    pub error: Option<&'a str>,
    pub signed_in: bool,
}

#[derive(Template)]
#[template(path = "pages/signup.html")]
pub struct SignupPage<'a> {
    pub app_name: &'a str,
    pub base_url: &'a str,
    pub error: Option<&'a str>,
    pub signed_in: bool,
    /// Hex-encoded HMAC-signed PoW challenge.
    pub pow_challenge: String,
    /// Number of leading zero bits required in the SHA-256 solution.
    pub pow_difficulty: u32,
}

/// "Check your inbox" landing rendered after every signup outcome — keeps
/// the response constant so attackers can't enumerate registered emails.
#[derive(Template)]
#[template(path = "pages/verify_pending.html")]
pub struct VerifyPendingPage<'a> {
    pub app_name: &'a str,
    pub base_url: &'a str,
    pub signed_in: bool,
}

/// Failure page for `/verify/{token}` (success path redirects to `/app`).
#[derive(Template)]
#[template(path = "pages/verify_result.html")]
pub struct VerifyResultPage<'a> {
    pub app_name: &'a str,
    pub base_url: &'a str,
    pub signed_in: bool,
    pub success: bool,
    pub message: &'a str,
}

#[derive(Template)]
#[template(path = "pages/invite.html")]
pub struct InvitePage<'a> {
    pub app_name: &'a str,
    pub base_url: &'a str,
    pub signed_in: bool,
    pub invitation: &'a Invitation,
    pub brokerage_name: &'a str,
    pub inviter_name: &'a str,
    pub error: Option<&'a str>,
}

// ---------------------------------------------------------------------------
// App: shared user header data
// ---------------------------------------------------------------------------

/// Per-page header data reused across every authenticated template.
pub struct AppHeader<'a> {
    pub user_name: &'a str,
    pub user_email: &'a str,
    pub user_initials: String,
    pub role: Role,
    pub brokerage_name: &'a str,
    pub active_nav: &'a str,
    pub is_super_admin: bool,
}

impl<'a> AppHeader<'a> {
    pub fn new(
        user_name: &'a str,
        user_email: &'a str,
        role: Role,
        brokerage_name: &'a str,
        active_nav: &'a str,
    ) -> Self {
        Self {
            user_name,
            user_email,
            user_initials: initials(user_name),
            role,
            brokerage_name,
            active_nav,
            is_super_admin: false,
        }
    }

    /// Builder-style toggle used by the admin controllers (super-admin
    /// status is derived from config, not the row, so we set it after
    /// construction).
    pub fn with_super_admin(mut self, yes: bool) -> Self {
        self.is_super_admin = yes;
        self
    }
}

fn initials(name: &str) -> String {
    name.split_whitespace()
        .take(2)
        .filter_map(|w| w.chars().next())
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

// ---------------------------------------------------------------------------
// App: dashboard + transactions
// ---------------------------------------------------------------------------

#[derive(Template)]
#[template(path = "pages/dashboard.html")]
pub struct DashboardPage<'a> {
    pub app_name: &'a str,
    pub base_url: &'a str,
    pub signed_in: bool,
    pub header: AppHeader<'a>,
    pub total: usize,
    pub open_count: usize,
    pub needs_attention: usize,
    pub complete_count: usize,
    pub recent: Vec<Transaction>,
}

#[derive(Template)]
#[template(path = "pages/transactions_list.html")]
pub struct TransactionsListPage<'a> {
    pub app_name: &'a str,
    pub base_url: &'a str,
    pub signed_in: bool,
    pub header: AppHeader<'a>,
    pub transactions: Vec<Transaction>,
    pub filter_status: &'a str,
    pub query: &'a str,
}

#[derive(Template)]
#[template(path = "pages/transaction_new.html")]
pub struct TransactionNewPage<'a> {
    pub app_name: &'a str,
    pub base_url: &'a str,
    pub signed_in: bool,
    pub header: AppHeader<'a>,
    pub error: Option<&'a str>,
    pub statuses: Vec<TransactionStatus>,
    pub types: Vec<TransactionType>,
    pub conditions: Vec<SpecialSalesCondition>,
    pub sales_types: Vec<SalesType>,
}

#[derive(Template)]
#[template(path = "pages/transaction_edit.html")]
pub struct TransactionEditPage<'a> {
    pub app_name: &'a str,
    pub base_url: &'a str,
    pub signed_in: bool,
    pub header: AppHeader<'a>,
    pub transaction: Transaction,
    pub transaction_key: String,
    pub statuses: Vec<TransactionStatus>,
    pub types: Vec<TransactionType>,
    pub conditions: Vec<SpecialSalesCondition>,
    pub sales_types: Vec<SalesType>,
    /// True when at least one checklist item has been approved or denied.
    /// The three "special conditions" selects render as `disabled` in
    /// this state — the backend still enforces it, this is just the UX
    /// signal so legit users see why they can't change those fields.
    pub dropdowns_locked: bool,
    pub error: Option<&'a str>,
}

#[derive(Template)]
#[template(path = "pages/transaction_show.html")]
pub struct TransactionShowPage<'a> {
    pub app_name: &'a str,
    pub base_url: &'a str,
    pub signed_in: bool,
    pub header: AppHeader<'a>,
    pub transaction: Transaction,
    pub transaction_key: String,
    pub compliance: CompliancePanel,
    pub owner_name: String,
    /// Forms NOT yet on this transaction's checklist — feeds the
    /// "Add optional form" picker.
    pub available_forms: Vec<&'static CarForm>,
    pub statuses: Vec<TransactionStatus>,
    /// Comments attached to the transaction itself (not to a specific item).
    pub transaction_comments: Vec<CommentView>,
    /// Whether the viewing user can approve/deny rows (broker / TC).
    pub can_review: bool,
}

/// One row within a checklist group on the transaction show page. Bundles
/// the persisted item with denormalised form metadata + per-item documents
/// so the template doesn't have to look anything up at render time.
#[derive(Debug, Clone)]
pub struct ChecklistRow {
    pub item: ChecklistItem,
    pub form: Option<&'static CarForm>,
    pub audit_label: String,
    pub documents: Vec<Document>,
    pub comments: Vec<CommentView>,
}

/// Display-friendly view of a single comment (denormalised author name,
/// plus an optional reference to a document the comment points back to —
/// used by the system-generated "replaced previous version" notes).
#[derive(Debug, Clone)]
pub struct CommentView {
    pub body: String,
    pub author_name: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub referenced_document: Option<ReferencedDocument>,
}

#[derive(Debug, Clone)]
pub struct ReferencedDocument {
    pub key: String,
    pub filename: String,
    pub version: i64,
}

impl CommentView {
    pub fn date_label(&self) -> String {
        self.created_at.format("%b %-d, %Y").to_string()
    }
}

/// One section of the grouped checklist (e.g. Mandatory Disclosures).
#[derive(Debug, Clone)]
pub struct ChecklistGroup {
    pub group: FormGroup,
    pub label: &'static str,
    pub slug: &'static str,
    pub items: Vec<ChecklistRow>,
    pub total: usize,
    pub completed: usize,
    pub required_total: usize,
    pub required_completed: usize,
    pub percent: u32,
}

impl ChecklistGroup {
    pub fn build(group: FormGroup, items: Vec<ChecklistRow>) -> Self {
        let total = items.len();
        let completed = items.iter().filter(|r| r.item.is_approved()).count();
        let required_total = items.iter().filter(|r| r.item.required).count();
        let required_completed = items
            .iter()
            .filter(|r| r.item.required && r.item.is_approved())
            .count();
        let percent = if total == 0 {
            0
        } else {
            ((completed as f32 / total as f32) * 100.0).round() as u32
        };
        Self {
            group,
            label: group.label(),
            slug: group.slug(),
            items,
            total,
            completed,
            required_total,
            required_completed,
            percent,
        }
    }

    pub fn complete(&self) -> bool {
        self.required_total > 0 && self.required_completed == self.required_total
    }
}

/// Aggregate panel covering the whole transaction's checklist.
pub struct CompliancePanel {
    pub groups: Vec<ChecklistGroup>,
    pub transaction_key: String,
    pub total: usize,
    pub completed: usize,
    pub required_total: usize,
    pub required_completed: usize,
    pub percent: u32,
    pub all_required_complete: bool,
}

impl CompliancePanel {
    pub fn build(groups: Vec<ChecklistGroup>, transaction_key: String) -> Self {
        let total: usize = groups.iter().map(|g| g.total).sum();
        let completed: usize = groups.iter().map(|g| g.completed).sum();
        let required_total: usize = groups.iter().map(|g| g.required_total).sum();
        let required_completed: usize = groups.iter().map(|g| g.required_completed).sum();
        let percent = if required_total == 0 {
            if total == 0 {
                0
            } else {
                ((completed as f32 / total as f32) * 100.0).round() as u32
            }
        } else {
            ((required_completed as f32 / required_total as f32) * 100.0).round() as u32
        };
        let all_required_complete = required_total > 0 && required_completed == required_total;
        Self {
            groups,
            transaction_key,
            total,
            completed,
            required_total,
            required_completed,
            percent,
            all_required_complete,
        }
    }

    /// True if any checklist item on the transaction is approved. The
    /// template uses this to hide per-document delete buttons — once a
    /// reviewer has approved anything we treat the transaction as past
    /// the "fix-it-myself" phase. Backend enforces the same gate in
    /// [`controllers::documents::delete`].
    pub fn has_any_approval(&self) -> bool {
        self.completed > 0
    }

    /// Every item on the transaction has been approved → the file set is
    /// frozen. Backend enforces this in [`controllers::documents::upload`];
    /// the template uses it to suppress upload UI.
    pub fn is_locked(&self) -> bool {
        self.total > 0 && self.total == self.completed
    }
}

// ---------------------------------------------------------------------------
// App: team
// ---------------------------------------------------------------------------

#[derive(Template)]
#[template(path = "pages/team.html")]
pub struct TeamPage<'a> {
    pub app_name: &'a str,
    pub base_url: &'a str,
    pub signed_in: bool,
    pub header: AppHeader<'a>,
    pub members: Vec<Member>,
    pub pending: Vec<Invitation>,
    pub invite_error: Option<&'a str>,
    pub invite_link: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Member {
    pub user_key: String,
    pub name: String,
    pub email: String,
    pub role: Role,
    pub initials: String,
    pub is_self: bool,
}

impl Member {
    pub fn new(user_key: String, name: String, email: String, role: Role, is_self: bool) -> Self {
        let initials = initials(&name);
        Self {
            user_key,
            name,
            email,
            role,
            initials,
            is_self,
        }
    }
}

// ---------------------------------------------------------------------------
// App: search
// ---------------------------------------------------------------------------

#[derive(Template)]
#[template(path = "pages/search.html")]
pub struct SearchPage<'a> {
    pub app_name: &'a str,
    pub base_url: &'a str,
    pub signed_in: bool,
    pub header: AppHeader<'a>,
    pub query: &'a str,
    pub transactions: Vec<Transaction>,
    pub documents: Vec<SearchDocument>,
}

#[derive(Debug, Clone)]
pub struct SearchDocument {
    pub document: Document,
    pub transaction_key: String,
    pub transaction_address: String,
}

// ---------------------------------------------------------------------------
// Admin
// ---------------------------------------------------------------------------

#[derive(Template)]
#[template(path = "pages/admin_users.html")]
pub struct AdminUsersPage<'a> {
    pub app_name: &'a str,
    pub base_url: &'a str,
    pub signed_in: bool,
    pub header: AppHeader<'a>,
    pub users: Vec<AdminUser>,
    pub total: usize,
    pub verified_count: usize,
    pub unverified_count: usize,
    pub query: String,
    pub status_filter: String,
}

#[derive(Template)]
#[template(path = "pages/admin_audit.html")]
pub struct AdminAuditPage<'a> {
    pub app_name: &'a str,
    pub base_url: &'a str,
    pub signed_in: bool,
    pub header: AppHeader<'a>,
    pub events: Vec<AuditEvent>,
    pub kind_filter: String,
    pub query: String,
    pub kinds: Vec<String>,
}

/// Cross-brokerage user view used by the admin dashboard. Hydrated from a
/// SurrealQL projection so the template doesn't have to know about graph
/// relations.
#[derive(Debug, Clone, Deserialize, SurrealValue)]
pub struct AdminUser {
    pub id: RecordId,
    pub email: String,
    pub name: String,
    pub email_verified: bool,
    pub signup_ip: Option<String>,
    pub signup_user_agent: Option<String>,
    pub last_login_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub brokerage_name: Option<String>,
    pub role: Option<String>,
}

impl AdminUser {
    pub fn url_key(&self) -> String {
        crate::record_key(&self.id)
    }
}
