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
use crate::forms::CarForm;
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
    /// Selectable tiers loaded from the DB at request time. Ordered by
    /// `sort_order` then `price_cents`. Empty when the admin hasn't
    /// created any tiers yet — the template falls back to a friendly
    /// "Plans coming soon" message.
    pub tiers: Vec<PricingTierView>,
}

/// Per-tier render data for the public pricing grid. Wraps the [`Tier`]
/// row with the pre-computed CTA URL so the template doesn't have to
/// know whether the visitor is signed in.
#[derive(Debug, Clone)]
pub struct PricingTierView {
    pub tier: crate::models::Tier,
    /// Where the "Start free trial" / "Subscribe" button points.
    /// Signed-out visitors get `/signup?plan={slug}`; signed-in users
    /// get `/app/subscribe/{slug}` so they skip the signup form and
    /// land on Stripe Checkout directly. `#` when this is the user's
    /// current plan (button is dimmed and inert).
    pub subscribe_href: String,
    pub button_label: &'static str,
    /// True when this tier matches the signed-in user's
    /// `brokerage.plan`. Drives the "Current plan" pill on the card
    /// and the disabled-style CTA.
    pub is_current: bool,
    /// Worked examples that show what a typical month costs at this
    /// tier: one at the limit, one over. Pre-computed in the
    /// controller so the template doesn't carry pricing math. Empty
    /// when the tier is `Unlimited` (no overage to demonstrate).
    pub scenarios: Vec<PricingScenario>,
    /// Optional comparison footnote shown beneath the scenarios —
    /// "At 100 txs/mo, Dotloop costs ~$X for the same team size."
    /// `None` when no apples-to-apples reference exists.
    pub comparison_note: Option<String>,
    /// Display-ready overage rate, e.g. `"$3.00"`, or `None` when the
    /// tier hard-blocks at the limit. Preformatted here so the template
    /// stays free of arithmetic.
    pub overage_per_tx_display: Option<String>,
}

/// One row in the "what would I pay?" example table on a tier card.
/// Computed server-side so the rendered numbers are guaranteed
/// consistent with the live tier configuration — nothing about the
/// math is duplicated in the template.
#[derive(Debug, Clone)]
pub struct PricingScenario {
    /// "60 transactions / month" — the input volume.
    pub label: String,
    /// "$249" — the resulting total cost as a display string.
    pub total: String,
    /// Short qualifier: "included", "5 over", "all-in". Helps the
    /// reader spot which scenario crosses the limit.
    pub qualifier: &'static str,
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

/// Landing for an authenticated user with no `works_at` edge —
/// "you're not at any brokerage yet" plus any pending invites they can
/// accept/decline. `redirect_now=true` is a sentinel for "the user
/// actually has a brokerage; the template just bounces to /app" so the
/// handler doesn't need a second response shape.
#[derive(Template)]
#[template(path = "pages/no_brokerage.html")]
pub struct NoBrokeragePage<'a> {
    pub app_name: &'a str,
    pub base_url: &'a str,
    pub signed_in: bool,
    pub user_name: &'a str,
    pub user_email: &'a str,
    pub invitations: Vec<NoBrokerageInvite>,
    pub redirect_now: bool,
}

/// One pending invitation row on the no-brokerage landing.
#[derive(Debug, Clone)]
pub struct NoBrokerageInvite {
    pub token: String,
    pub role: String,
    pub brokerage_name: String,
    pub inviter_name: String,
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
    /// When true, hide the name+password form and show a "sign in to
    /// accept" CTA instead — the recipient already has an account so
    /// they should authenticate and accept from `/app/no-brokerage`.
    pub prompt_login: bool,
}

// ---------------------------------------------------------------------------
// App: shared user header data
// ---------------------------------------------------------------------------

/// Per-page header data reused across every authenticated template.
/// Owns its strings so callers can build it from short-lived borrows
/// (`info.brokerage_name` is owned by the temporary `HeaderInfo`
/// returned by [`crate::billing::header_info_for_user`]) without
/// fighting lifetimes.
pub struct AppHeader {
    pub user_name: String,
    pub user_email: String,
    pub user_initials: String,
    pub role: Role,
    pub brokerage_name: String,
    pub active_nav: String,
    pub is_super_admin: bool,
    /// URL-safe key for the signed-in user, used to compose the avatar
    /// URL `/app/users/{user_key}/avatar` for the header dropdown.
    pub user_key: String,
    /// Whether the user has uploaded an avatar — drives the
    /// `<img>` vs initials choice in the header.
    pub has_avatar: bool,
    /// Optional subscription-status banner — `past_due`, `canceling`,
    /// `wind_down`. Rendered above the page header by the partial in
    /// `components/app_header.html`.
    pub banner: Option<crate::billing::SubscriptionBanner>,
}

impl AppHeader {
    pub fn new(
        user_name: impl Into<String>,
        user_email: impl Into<String>,
        role: Role,
        brokerage_name: impl Into<String>,
        active_nav: impl Into<String>,
    ) -> Self {
        let user_name = user_name.into();
        Self {
            user_initials: initials(&user_name),
            user_name,
            user_email: user_email.into(),
            role,
            brokerage_name: brokerage_name.into(),
            active_nav: active_nav.into(),
            is_super_admin: false,
            user_key: String::new(),
            has_avatar: false,
            banner: None,
        }
    }

    /// Builder-style toggle used by the admin controllers (super-admin
    /// status is derived from config, not the row, so we set it after
    /// construction).
    pub fn with_super_admin(mut self, yes: bool) -> Self {
        self.is_super_admin = yes;
        self
    }

    /// Set the avatar-related fields from a [`CurrentUser`]. Every
    /// authenticated page calls this so the header dropdown renders
    /// the right thumbnail.
    pub fn with_avatar(mut self, user_key: String, has_avatar: bool) -> Self {
        self.user_key = user_key;
        self.has_avatar = has_avatar;
        self
    }

    /// Attach the subscription banner returned by
    /// [`crate::billing::header_info_for_user`] so the page renders the
    /// "card failed / canceling / wind-down" strip.
    pub fn with_banner(mut self, banner: Option<crate::billing::SubscriptionBanner>) -> Self {
        self.banner = banner;
        self
    }
}

pub fn initials(name: &str) -> String {
    name.split_whitespace()
        .take(2)
        .filter_map(|w| w.chars().next())
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

// ---------------------------------------------------------------------------
// App: transactions
// ---------------------------------------------------------------------------

/// Unassigned-transactions view + mass-reassign form. Renders only the
/// transactions in the broker's brokerage that currently have no
/// `owns` edge (e.g. left orphaned after an agent was removed).
#[derive(Template)]
#[template(path = "pages/unassigned.html")]
pub struct UnassignedPage<'a> {
    pub app_name: &'a str,
    pub base_url: &'a str,
    pub signed_in: bool,
    pub header: AppHeader,
    pub transactions: Vec<Transaction>,
    pub assignees: Vec<UnassignedAssignee>,
}

/// One option in the mass-reassign dropdown — every active member of
/// the broker's brokerage.
#[derive(Debug, Clone)]
pub struct UnassignedAssignee {
    pub key: String,
    pub name: String,
    pub role_label: String,
}

#[derive(Template)]
#[template(path = "pages/transactions_list.html")]
pub struct TransactionsListPage<'a> {
    pub app_name: &'a str,
    pub base_url: &'a str,
    pub signed_in: bool,
    pub header: AppHeader,
    /// First page of rows (subsequent pages stream in via the
    /// infinite-scroll fragment endpoint).
    pub transactions: Vec<Transaction>,
    pub filter_status: &'a str,
    pub query: &'a str,
    /// True when the dashboard's "Needs attention" filter is on; carried
    /// into the URL on form submits + the sentinel's next-page link.
    pub attention_on: bool,
    /// More rows exist past this page → render the infinite-scroll
    /// sentinel pointing at `next_url`.
    pub has_next_page: bool,
    /// Fully-formed URL to GET for the next page (already includes the
    /// `fragment=rows` flag).
    pub next_url: String,
    /// Totals across the full visible set (NOT the filtered page) so
    /// the stat-grid cards always show real numbers regardless of
    /// which filter is on. Active and Pending are split into separate
    /// cards per the corrections set.
    pub total: usize,
    pub active_count: usize,
    pub pending_count: usize,
    pub needs_attention: usize,
    pub sold_count: usize,
    /// Which stat-card to highlight — derived from `filter_status` +
    /// `attention_on`. One of `""`, `"total"`, `"active"`,
    /// `"pending"`, `"attention"`, `"sold"`.
    pub active_filter: &'a str,
    /// Header strip — one entry per sortable column, with the URL to
    /// click + arrow glyph for the active column.
    pub sort_headers: Vec<crate::controllers::transactions::SortHeader>,
    /// Brokers get a red Delete button on each row (corrections set).
    pub is_broker: bool,
    /// URL the toolbar's search box live-fetches on input (Datastar
    /// `@get`). Carries status/attention/sort; the typed query travels
    /// as the bound `q` signal.
    pub live_results_url: String,
}

/// HTML fragment containing only the row markup for a given page. The
/// list endpoint returns this (no chrome, no `<html>`) when the request
/// carries `?fragment=rows`, so the infinite-scroll trigger can append
/// the response directly into the existing list.
#[derive(Template)]
#[template(path = "partials/transaction_rows.html")]
pub struct TransactionRowsFragment {
    pub transactions: Vec<Transaction>,
    pub has_next_page: bool,
    pub next_url: String,
    /// Brokers get a red Delete button on each row (corrections set).
    /// Threaded into the fragment so infinitely-scrolled rows keep it.
    pub is_broker: bool,
}

/// The list's whole results region — header strip + rows, or the
/// empty state. Returned by `?fragment=results` for the live-search
/// swap of `<div id="tx-results">`; the same partial is included by
/// the full page so the two render paths can't drift.
#[derive(Template)]
#[template(path = "partials/tx_results.html")]
pub struct TxResultsFragment {
    pub transactions: Vec<Transaction>,
    pub has_next_page: bool,
    pub next_url: String,
    pub is_broker: bool,
    pub sort_headers: Vec<crate::controllers::transactions::SortHeader>,
}

/// The search page's results region (transactions + documents), for
/// the `?fragment=results` live-search swap of `<div id="search-results">`.
#[derive(Template)]
#[template(path = "partials/search_results.html")]
pub struct SearchResultsFragment {
    pub query: String,
    pub sort_headers: Vec<crate::controllers::transactions::SortHeader>,
    pub transactions: Vec<Transaction>,
    pub documents: Vec<SearchDocument>,
}

#[derive(Template)]
#[template(path = "pages/transaction_new.html")]
pub struct TransactionNewPage<'a> {
    pub app_name: &'a str,
    pub base_url: &'a str,
    pub signed_in: bool,
    pub header: AppHeader,
    pub error: Option<&'a str>,
    pub statuses: Vec<TransactionStatus>,
    pub types: Vec<TransactionType>,
    pub conditions: Vec<SpecialSalesCondition>,
    pub sales_types: Vec<SalesType>,
}

#[derive(Template)]
#[template(path = "pages/profile.html")]
pub struct ProfilePage<'a> {
    pub app_name: &'a str,
    pub base_url: &'a str,
    pub signed_in: bool,
    pub header: AppHeader,
    pub name: &'a str,
    pub email: &'a str,
    pub user_key: String,
    pub has_avatar: bool,
    pub profile_error: Option<&'a str>,
    pub password_error: Option<&'a str>,
}

#[derive(Template)]
#[template(path = "pages/transaction_edit.html")]
pub struct TransactionEditPage<'a> {
    pub app_name: &'a str,
    pub base_url: &'a str,
    pub signed_in: bool,
    pub header: AppHeader,
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
    pub header: AppHeader,
    pub transaction: Transaction,
    pub transaction_key: String,
    pub compliance: CompliancePanel,
    pub owner_name: String,
    /// The brokerage's full form catalog (DB library + custom forms,
    /// compiled-catalog fallback) — feeds the "Add an item" picker.
    pub available_forms: Vec<PickerForm>,
    pub statuses: Vec<TransactionStatus>,
    /// Comments attached to the transaction itself (not to a specific item).
    pub transaction_comments: Vec<CommentView>,
    /// Whether the viewing user can approve/deny rows (broker / TC).
    pub can_review: bool,
}

/// One option in the Add-an-item datalist. Owned strings because the
/// catalog is DB-resolved per brokerage (library + custom forms), not
/// borrowed from the compiled library.
#[derive(Debug, Clone)]
pub struct PickerForm {
    pub code: String,
    pub name: String,
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
    pub author_initials: String,
    /// URL-safe key for the author, used by the template to render
    /// their avatar via `/app/users/{key}/avatar`. Always present even
    /// for the initials-fallback case so the template stays uniform.
    pub author_key: String,
    pub author_has_avatar: bool,
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
/// Data-driven now: `name` + `order` come straight from the resolved
/// form's group, so locality-labeled groups ("Los Angeles — Mandatory
/// Disclosures") render as first-class sections.
#[derive(Debug, Clone)]
pub struct ChecklistGroup {
    pub name: String,
    pub order: i64,
    /// Contract groups start expanded; everything else folded.
    pub open_by_default: bool,
    pub items: Vec<ChecklistRow>,
    pub total: usize,
    pub completed: usize,
    pub required_total: usize,
    pub required_completed: usize,
    pub percent: u32,
}

impl ChecklistGroup {
    pub fn build(name: String, order: i64, items: Vec<ChecklistRow>, role: Role) -> Self {
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
        // Agents see every group expanded — their working view is "what
        // still needs my attention" and folding hides items behind a
        // click. Brokers and coordinators start with every group
        // collapsed; the template's `has_attention()` branch still
        // pops open any group that has a flagged item, so a denial or
        // a pending-with-upload never hides behind a fold for them.
        let open_by_default = role == Role::Agent;
        Self {
            name,
            order,
            open_by_default,
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

    /// Does any item in this group need the viewer's attention? Drives
    /// the accordion auto-open so a flagged item is never hidden behind
    /// a folded category. `reviewer` = broker/compliance-officer view.
    /// Takes `&bool` because Askama hands method args by reference.
    pub fn has_attention(&self, reviewer: &bool) -> bool {
        self.items.iter().any(|r| r.needs_attention(reviewer))
    }
}

impl ChecklistRow {
    /// The form code to display / attach uploads under. Prefers the
    /// canonical CAR-library code when the item's code resolves there,
    /// but falls back to the code stored on the item itself — broker
    /// custom forms (added via /app/forms or Admin → Forms) carry codes
    /// like `RNTD` or `R&R` that don't exist in the compiled library,
    /// and they should still render their chip and label their uploads.
    pub fn code(&self) -> Option<&str> {
        self.form
            .map(|f| f.code)
            .or(self.item.form_code.as_deref())
            .map(str::trim)
            .filter(|c| !c.is_empty())
    }

    /// Per-item attention flag mirroring [`needs_attention_flags`] at
    /// row granularity: agents see denied/rejected forms; reviewers see
    /// items with an uploaded document still awaiting review.
    pub fn needs_attention(&self, reviewer: &bool) -> bool {
        if *reviewer {
            self.item.approval_status == "pending" && !self.documents.is_empty()
        } else {
            self.item.is_denied()
        }
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
    pub header: AppHeader,
    pub members: Vec<Member>,
    pub pending: Vec<Invitation>,
    pub invite_error: Option<&'a str>,
    pub invite_link: Option<String>,
    /// Summary line after a multi-address invite, e.g. "Sent 3
    /// invitations." Shown instead of the single copyable link when
    /// more than one email was submitted.
    pub invite_notice: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Member {
    pub user_key: String,
    pub name: String,
    pub email: String,
    pub role: Role,
    pub initials: String,
    pub is_self: bool,
    pub has_avatar: bool,
}

impl Member {
    pub fn new(
        user_key: String,
        name: String,
        email: String,
        role: Role,
        is_self: bool,
        has_avatar: bool,
    ) -> Self {
        let initials = initials(&name);
        Self {
            user_key,
            name,
            email,
            role,
            initials,
            is_self,
            has_avatar,
        }
    }
}

/// Brokerage-scoped audit log. Shows events where the `actor` is (or
/// was) a member of the current user's brokerage. Brokers + Compliance
/// Officers can read this; agents cannot.
#[derive(Template)]
#[template(path = "pages/brokerage_audit.html")]
pub struct BrokerageAuditPage<'a> {
    pub app_name: &'a str,
    pub base_url: &'a str,
    pub signed_in: bool,
    pub header: AppHeader,
    pub events: Vec<AuditEvent>,
    pub kind_filter: String,
    pub query: String,
    pub kinds: Vec<String>,
    pub has_next_page: bool,
    pub next_url: String,
}

/// Rows-only fragment for the audit log's infinite scroll. Mirrors
/// `TransactionRowsFragment` — the controller renders just the next
/// page's rows + (optional) sentinel; client JS appends them in
/// place of the previous sentinel.
#[derive(Template)]
#[template(path = "partials/audit_rows.html")]
pub struct AuditRowsFragment {
    pub events: Vec<AuditEvent>,
    pub has_next_page: bool,
    pub next_url: String,
}

/// Pre-delete confirmation page for the destructive "delete entire
/// brokerage" flow. The numbers below are computed server-side so the
/// warning shows exactly what's about to disappear.
#[derive(Template)]
#[template(path = "pages/brokerage_delete.html")]
pub struct BrokerageDeletePage<'a> {
    pub app_name: &'a str,
    pub base_url: &'a str,
    pub signed_in: bool,
    pub header: AppHeader,
    pub brokerage_name: String,
    pub user_count: usize,
    pub transaction_count: usize,
    pub document_count: usize,
    pub storage_display: String,
    pub error: Option<&'a str>,
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
    pub header: AppHeader,
    pub query: &'a str,
    /// Selected status filter (matches the transactions list dropdown).
    pub status_filter: &'a str,
    /// Sortable column headers for the transaction results (links back
    /// to `/app/search`, query + status preserved).
    pub sort_headers: Vec<crate::controllers::transactions::SortHeader>,
    pub transactions: Vec<Transaction>,
    pub documents: Vec<SearchDocument>,
    /// URL the search box live-fetches on input (Datastar `@get`);
    /// the typed query travels as the bound `q` signal.
    pub live_results_url: String,
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
    pub header: AppHeader,
    pub users: Vec<AdminUser>,
    pub total: usize,
    pub verified_count: usize,
    pub unverified_count: usize,
    pub query: String,
    pub status_filter: String,
}

#[derive(Template)]
#[template(path = "pages/admin_changelog.html")]
pub struct AdminChangelogPage<'a> {
    pub app_name: &'a str,
    pub base_url: &'a str,
    pub signed_in: bool,
    pub header: AppHeader,
    /// Pre-rendered HTML from `CHANGELOG.md`. Built once per request
    /// via [`pulldown_cmark`]; trusted source (compiled-in repo file)
    /// so it's emitted as `{{ … |safe }}` rather than auto-escaped.
    pub body_html: String,
    /// Current build version, mirrored from `CARGO_PKG_VERSION` so the
    /// page can show "Currently running vX.Y.Z" prominently alongside
    /// the changelog itself.
    pub version: &'static str,
}

#[derive(Template)]
#[template(path = "pages/admin_audit.html")]
pub struct AdminAuditPage<'a> {
    pub app_name: &'a str,
    pub base_url: &'a str,
    pub signed_in: bool,
    pub header: AppHeader,
    pub events: Vec<AuditEvent>,
    pub kind_filter: String,
    pub query: String,
    pub kinds: Vec<String>,
}

#[derive(Template)]
#[template(path = "pages/admin_tiers.html")]
pub struct AdminTiersPage<'a> {
    pub app_name: &'a str,
    pub base_url: &'a str,
    pub signed_in: bool,
    pub header: AppHeader,
    pub tiers: Vec<crate::models::Tier>,
    pub stripe_enabled: bool,
    pub flash: Option<&'a str>,
}

#[derive(Template)]
#[template(path = "pages/admin_tier_edit.html")]
pub struct AdminTierEditPage<'a> {
    pub app_name: &'a str,
    pub base_url: &'a str,
    pub signed_in: bool,
    pub header: AppHeader,
    /// `None` when rendering the "new tier" form; `Some` when editing.
    pub existing: Option<crate::models::Tier>,
    pub stripe_enabled: bool,
    pub error: Option<&'a str>,
}

#[derive(Template)]
#[template(path = "pages/admin_brokerages.html")]
pub struct AdminBrokeragesPage<'a> {
    pub app_name: &'a str,
    pub base_url: &'a str,
    pub signed_in: bool,
    pub header: AppHeader,
    pub rows: Vec<AdminBrokerageRow>,
    /// Brokerages past their 60-day wind-down purge window. Rendered
    /// as a separate "pending deletion" section above the main list so
    /// super-admins can act on them.
    pub pending: Vec<AdminBrokerageRow>,
    /// Pre-formatted totals (humansized + thousands-separated) so the
    /// template stays presentation-only.
    pub total_brokerages_display: String,
    pub total_transactions_display: String,
    pub total_documents_display: String,
    pub total_storage_display: String,
}

/// Single-brokerage deep-dive page used by super-admins to debug
/// Stripe sync, billing state, and membership. Every Stripe field on
/// the row is exposed; timestamps are pre-formatted by the
/// controller so the template stays purely presentational.
#[derive(Template)]
#[template(path = "pages/admin_brokerage_detail.html")]
pub struct AdminBrokerageDetailPage<'a> {
    pub app_name: &'a str,
    pub base_url: &'a str,
    pub signed_in: bool,
    pub header: AppHeader,
    pub brokerage_key: String,
    pub brokerage_name: String,
    pub plan_slug: String,
    pub is_complimentary: bool,
    pub city: Option<String>,
    pub state_code: String,
    pub stripe_customer_id: Option<String>,
    pub stripe_subscription_id: Option<String>,
    pub subscription_status: String,
    pub current_period_end_display: Option<String>,
    pub cancel_at_display: Option<String>,
    pub wind_down_purge_at_display: Option<String>,
    pub created_at_display: String,
    pub updated_at_display: String,
    /// Resolved from `brokerage.plan` slug → `tier` row. `None` when
    /// the brokerage hasn't subscribed yet (default `plan='trial'`).
    pub tier_name: Option<String>,
    pub tier_price_display: Option<String>,
    pub tier_transaction_limit_display: Option<String>,
    pub tier_user_limit_display: Option<String>,
    pub members: Vec<AdminBrokerageMember>,
    pub recent_events: Vec<AuditEvent>,
}

#[derive(Debug, Clone)]
pub struct AdminBrokerageMember {
    pub user_key: String,
    pub email: String,
    pub name: String,
    pub role: String,
}

// ---------------------------------------------------------------------------
// Forms configuration (broker + admin)
// ---------------------------------------------------------------------------

/// Broker's forms configuration page (`/app/forms`).
#[derive(Template)]
#[template(path = "pages/broker_forms.html")]
pub struct BrokerFormsPage<'a> {
    pub app_name: &'a str,
    pub base_url: &'a str,
    pub signed_in: bool,
    pub header: AppHeader,
    pub state_name: String,
    pub localities: Vec<FormSetOption>,
    pub master_forms: Vec<BrokerFormRow>,
    pub custom_forms: Vec<BrokerFormRow>,
    pub group_choices: Vec<&'static str>,
    /// Pre-checked applicability choices for the "Add custom form" form.
    pub picker_types: Vec<AppliesChoice>,
    pub picker_sides: Vec<AppliesChoice>,
    pub picker_conditions: Vec<AppliesChoice>,
}

#[derive(Debug, Clone)]
pub struct FormSetOption {
    pub key: String,
    pub name: String,
    pub selected: bool,
}

#[derive(Debug, Clone)]
pub struct BrokerFormRow {
    pub key: String,
    pub code: String,
    pub name: String,
    pub group_name: String,
    pub hidden: bool,
    pub custom: bool,
}

/// Admin form-set list (`/admin/forms`).
#[derive(Template)]
#[template(path = "pages/admin_forms.html")]
pub struct AdminFormsPage<'a> {
    pub app_name: &'a str,
    pub base_url: &'a str,
    pub signed_in: bool,
    pub header: AppHeader,
    pub state_sets: Vec<AdminFormSetRow>,
    pub local_sets: Vec<AdminFormSetRow>,
}

#[derive(Debug, Clone)]
pub struct AdminFormSetRow {
    pub key: String,
    pub name: String,
    pub scope: String,
    pub group_count: i64,
    pub form_count: i64,
}

/// Admin detail for one form set (`/admin/forms/{key}`).
#[derive(Template)]
#[template(path = "pages/admin_form_set.html")]
pub struct AdminFormSetDetailPage<'a> {
    pub app_name: &'a str,
    pub base_url: &'a str,
    pub signed_in: bool,
    pub header: AppHeader,
    pub set_key: String,
    pub set_name: String,
    pub set_scope: String,
    pub groups: Vec<FormGroupView>,
    pub group_choices: Vec<&'static str>,
    /// Checkbox choices for the inline "Add form" applicability picker.
    /// On this page every box is pre-checked (= broad default); the
    /// edit page reflects each form's actual stored selections.
    pub picker_types: Vec<AppliesChoice>,
    pub picker_sides: Vec<AppliesChoice>,
    pub picker_conditions: Vec<AppliesChoice>,
}

/// One checkbox in a form-applicability fieldset. `field_name` is the
/// POST key (e.g. `cond_short_sale`), `label` is the user-facing
/// caption, `checked` is the initial state.
#[derive(Debug, Clone)]
pub struct AppliesChoice {
    pub field_name: String,
    pub label: String,
    pub checked: bool,
}

/// Admin edit page for an existing library form. Lets a super-admin
/// narrow which transaction types/sides/sales conditions the form
/// applies to, plus tweak its name/order/required flag. Code is shown
/// read-only — renaming an existing code is rarely what's intended
/// and would break references downstream.
#[derive(Template)]
#[template(path = "pages/admin_form_edit.html")]
pub struct AdminFormEditPage<'a> {
    pub app_name: &'a str,
    pub base_url: &'a str,
    pub signed_in: bool,
    pub header: AppHeader,
    pub set_key: String,
    pub set_name: String,
    pub form_key: String,
    pub form_code: String,
    pub form_name: String,
    pub form_order: i64,
    pub form_required: bool,
    pub picker_types: Vec<AppliesChoice>,
    pub picker_sides: Vec<AppliesChoice>,
    pub picker_conditions: Vec<AppliesChoice>,
}

#[derive(Debug, Clone)]
pub struct FormGroupView {
    pub key: String,
    pub name: String,
    pub sort_order: i64,
    pub forms: Vec<AdminFormRow>,
}

#[derive(Debug, Clone)]
pub struct AdminFormRow {
    pub key: String,
    pub code: String,
    pub name: String,
    pub required: bool,
    pub is_active: bool,
    pub form_order: i64,
}

/// Stat-grid HTML rendered standalone for the Datastar polling
/// endpoint at `GET /app/stats`. The dashboard wraps the in-page
/// stat-grid include with a `data-on-interval` element that fetches
/// this fragment every few seconds; Idiomorph matches `id="stat-grid"`
/// in the response against the same id on the page and morphs the
/// numbers in place — no page reload, no flicker.
///
/// Fields mirror the parent page's stat-grid context exactly so the
/// shared partial template (`partials/stat_grid.html`) renders the
/// same way whether it's pulled in from the full page or this
/// fragment.
#[derive(Template)]
#[template(path = "partials/stat_grid.html")]
pub struct StatGridFragment {
    pub total: usize,
    pub active_count: usize,
    pub pending_count: usize,
    pub needs_attention: usize,
    pub sold_count: usize,
    pub active_filter: String,
}

/// One row on the brokerages admin page: name + already-human-formatted
/// counts and byte size, so the template doesn't have to call
/// `humansize` / `num-format` directly.
#[derive(Debug, Clone)]
pub struct AdminBrokerageRow {
    /// URL-safe brokerage key (e.g. `k02yhbjnyg9bv3s8kxsb`). Used by
    /// the template to POST to `/admin/brokerages/{key}/comp`.
    pub key: String,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub tx_count_display: String,
    pub storage_display: String,
    pub document_count_display: String,
    /// Mirrors `brokerage.is_complimentary`. Drives the "Comp" badge
    /// + toggle-button label.
    pub is_complimentary: bool,
    /// When the row appears in the "pending deletion" list, this is
    /// the timestamp the 60-day grace ended (so the template can
    /// render "wind-down ended N days ago").
    pub purge_due_at: Option<DateTime<Utc>>,
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
        crate::db::record_key(&self.id)
    }
}
