//! Askama template structs. One file keeps the template catalogue easy to
//! scan; each struct maps 1:1 to an HTML file under `templates/`.
//!
//! Some fields (`signed_in`, `app_name`, `base_url`, `category`) are part of
//! the template-rendering data contract even where a particular page does
//! not currently read them. Allow dead code here so the API stays symmetric.
#![allow(dead_code)]

use askama::Template;

use crate::auth::Role;
use crate::models::{ChecklistItem, Document, Invitation, Transaction};

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
        }
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
    pub documents: Vec<DocumentGroup>,
    pub owner_name: String,
}

/// Data bundle for the compliance panel on the transaction show page.
/// Rendered inline from the parent template via `compliance.*` accessors —
/// no standalone `Template` derive because it only ever renders as part of
/// the larger page.
pub struct CompliancePanel {
    pub total: usize,
    pub completed: usize,
    pub percent: u32,
    pub all_complete: bool,
    pub items: Vec<ChecklistItem>,
    pub audit_labels: Vec<String>,
    pub transaction_key: String,
}

impl CompliancePanel {
    pub fn build(
        items: Vec<ChecklistItem>,
        audit_labels: Vec<String>,
        transaction_key: String,
    ) -> Self {
        let total = items.len();
        let completed = items.iter().filter(|i| i.completed).count();
        let percent = if total == 0 {
            0
        } else {
            ((completed as f32 / total as f32) * 100.0).round() as u32
        };
        let all_complete = total > 0 && completed == total;
        Self {
            total,
            completed,
            percent,
            all_complete,
            items,
            audit_labels,
            transaction_key,
        }
    }
}

#[derive(Debug, Clone)]
pub struct DocumentGroup {
    pub category: String,
    pub label: String,
    pub documents: Vec<Document>,
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
    pub name: String,
    pub email: String,
    pub role: Role,
    pub initials: String,
}

impl Member {
    pub fn new(name: String, email: String, role: Role) -> Self {
        let initials = initials(&name);
        Self { name, email, role, initials }
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

