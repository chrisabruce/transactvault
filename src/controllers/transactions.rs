//! Dashboard + transaction CRUD and the in-app search.
//!
//! Access control is graph-shaped: a user sees a transaction when either
//! (a) their brokerage has an outbound `has_transaction` edge to it, AND
//! (b) for agents, they also have an outbound `owns` edge to it.
//! Brokers and coordinators see every transaction in their brokerage.

use axum::Form;
use axum::extract::{Path, Query, State};
use axum::response::{Html, IntoResponse, Redirect, Response};
use serde::Deserialize;
use surrealdb::types::{RecordId, SurrealValue};

use crate::auth::{CurrentUser, Role};
use crate::controllers::render;
use crate::error::AppError;
use crate::forms::{self, FormGroup};
use crate::models::{
    Brokerage, ChecklistItem, Document, NewChecklistItem, NewTransaction, SalesType,
    SpecialSalesCondition, Transaction, TransactionStatus, TransactionType,
};
use crate::state::AppState;
use crate::templates::{
    AppHeader, ChecklistGroup, ChecklistRow, CommentView, CompliancePanel, SearchDocument,
    SearchPage, TransactionEditPage, TransactionNewPage, TransactionRowsFragment,
    TransactionShowPage, TransactionsListPage,
};

/// Default page size for the transactions list. Tuned for first-paint
/// speed plus enough rows to fill a typical viewport — Datastar fetches
/// the next page when the sentinel scrolls into view.
const TX_LIST_PAGE_SIZE: usize = 30;

// ---------------------------------------------------------------------------
// Dashboard
// ---------------------------------------------------------------------------

/// Stat-grid totals over the un-filtered transaction set. Returned as
/// `(total, active_count, pending_count, needs_attention, sold_count)` —
/// the five cards the dashboard renders. Active and Pending are split
/// into their own cards (per the corrections set) so a broker sees the
/// live/under-contract breakdown at a glance.
async fn stat_grid_totals(
    state: &AppState,
    transactions: &[Transaction],
    role: Role,
    brokerage_id: &RecordId,
) -> Result<StatTotals, AppError> {
    let total = transactions.len();
    let active_count = transactions
        .iter()
        .filter(|t| matches!(t.status_enum(), TransactionStatus::Active))
        .count();
    let pending_count = transactions
        .iter()
        .filter(|t| matches!(t.status_enum(), TransactionStatus::Pending))
        .count();
    let sold_count = transactions
        .iter()
        .filter(|t| matches!(t.status_enum(), TransactionStatus::Sold))
        .count();
    let needs_attention = count_needs_attention(state, transactions, role, brokerage_id).await?;
    Ok(StatTotals {
        total,
        active_count,
        pending_count,
        needs_attention,
        sold_count,
    })
}

/// The five dashboard counters. A named struct keeps the call site
/// readable now that there are five fields instead of four.
struct StatTotals {
    total: usize,
    active_count: usize,
    pending_count: usize,
    needs_attention: usize,
    sold_count: usize,
}

// ---------------------------------------------------------------------------
// Transactions list
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct ListFilters {
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub q: Option<String>,
    /// `"1"` (or any truthy string) restricts the list to transactions
    /// with at least one denied checklist item — the same predicate
    /// that drives the dashboard "Needs attention" counter.
    #[serde(default)]
    pub attention: Option<String>,
    /// 1-indexed page number. Defaults to 1.
    #[serde(default)]
    pub page: Option<usize>,
    /// `"rows"` returns just the row HTML (for infinite-scroll appends);
    /// anything else (or absent) renders the full page chrome.
    #[serde(default)]
    pub fragment: Option<String>,
    /// Column to sort by: `age` (default, newest first), `property`,
    /// `price`, `type`, or `status`. Unknown values fall back to `age`.
    #[serde(default)]
    pub sort: Option<String>,
    /// Sort direction: `asc` or `desc`. Defaults to `desc` for `age` /
    /// `price` (newest, most expensive first), `asc` for text columns.
    #[serde(default)]
    pub dir: Option<String>,
}

/// Which column the list is sorted by. Kept on the page model so the
/// header strip can render the right active-state arrow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    Age,
    Property,
    Price,
    Type,
    Status,
}

impl SortKey {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "age" => Some(Self::Age),
            "property" => Some(Self::Property),
            "price" => Some(Self::Price),
            "type" => Some(Self::Type),
            "status" => Some(Self::Status),
            _ => None,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Age => "age",
            Self::Property => "property",
            Self::Price => "price",
            Self::Type => "type",
            Self::Status => "status",
        }
    }
    /// Direction a fresh click on this column should land on. Numeric /
    /// date columns default to descending (newest, biggest first); text
    /// columns default to ascending (A → Z).
    pub fn default_dir(self) -> SortDir {
        match self {
            Self::Age | Self::Price => SortDir::Desc,
            Self::Property | Self::Type | Self::Status => SortDir::Asc,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDir {
    Asc,
    Desc,
}

impl SortDir {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "asc" => Some(Self::Asc),
            "desc" => Some(Self::Desc),
            _ => None,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Asc => "asc",
            Self::Desc => "desc",
        }
    }
    pub fn flip(self) -> Self {
        match self {
            Self::Asc => Self::Desc,
            Self::Desc => Self::Asc,
        }
    }
}

pub async fn list(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(filters): Query<ListFilters>,
) -> Result<Html<String>, AppError> {
    let brokerage = load_brokerage(&state, &user).await?;
    let mut transactions = load_visible_transactions(&state, &user).await?;

    // Stat-grid totals are computed on the FULL visible set, before any
    // filter is applied, so the cards keep showing real counts even when
    // a filter is active. (Otherwise "Active" view would say "Total: 5"
    // which defeats the purpose of leaving the cards on the page.)
    let totals = stat_grid_totals(&state, &transactions, user.role, &user.brokerage_id).await?;

    let status_filter = filters.status.clone().unwrap_or_default();
    if !status_filter.is_empty() && status_filter != "all" {
        // `open` is a legacy alias meaning Active OR Pending. The
        // dashboard no longer links to it (Active and Pending are now
        // separate cards), but we keep honoring it so old bookmarks
        // don't 404 into an empty list.
        if status_filter == "open" {
            transactions.retain(|t| {
                matches!(
                    t.status_enum(),
                    TransactionStatus::Active | TransactionStatus::Pending
                )
            });
        } else {
            transactions.retain(|t| t.status == status_filter);
        }
    }

    let query = filters.q.clone().unwrap_or_default();
    if !query.trim().is_empty() {
        let needle = query.to_ascii_lowercase();
        transactions.retain(|t| {
            t.property_address.to_ascii_lowercase().contains(&needle)
                || t.city.to_ascii_lowercase().contains(&needle)
                || t.client_name
                    .as_deref()
                    .map(|s| s.to_ascii_lowercase().contains(&needle))
                    .unwrap_or(false)
                || t.mls_number
                    .as_deref()
                    .map(|s| s.to_ascii_lowercase().contains(&needle))
                    .unwrap_or(false)
        });
    }

    let attention_on = is_truthy(&filters.attention);
    if attention_on {
        let flags =
            needs_attention_flags(&state, &transactions, user.role, &user.brokerage_id).await?;
        transactions = transactions
            .into_iter()
            .zip(flags)
            .filter_map(|(t, f)| if f { Some(t) } else { None })
            .collect();
    }

    // Sort the filtered set. Defaults: Age column, desc (newest first).
    // We sort in Rust rather than pushing ORDER BY into the SurrealDB
    // query because the `attention` filter is computed per-row in Rust
    // — by the time we'd hit the DB we'd lose the filter alignment.
    let sort_key = filters
        .sort
        .as_deref()
        .and_then(SortKey::parse)
        .unwrap_or(SortKey::Age);
    let sort_dir = filters
        .dir
        .as_deref()
        .and_then(SortDir::parse)
        .unwrap_or_else(|| sort_key.default_dir());
    sort_transactions(&mut transactions, sort_key, sort_dir);

    // Pagination over the filtered + sorted slice. We don't push
    // pagination into the DB query because the `attention` filter
    // requires N small round-trips per transaction anyway — there's no
    // win from a SQL LIMIT/OFFSET. With per-brokerage volumes in the
    // hundreds this is cheap; we'd revisit only past five-digit
    // per-brokerage counts.
    let page = filters.page.unwrap_or(1).max(1);
    let total_filtered = transactions.len();
    let start = (page - 1).saturating_mul(TX_LIST_PAGE_SIZE);
    let end = start.saturating_add(TX_LIST_PAGE_SIZE).min(total_filtered);
    let page_rows: Vec<Transaction> = if start < end {
        transactions[start..end].to_vec()
    } else {
        Vec::new()
    };
    let has_next_page = end < total_filtered;
    let next_url = if has_next_page {
        build_list_url(
            &status_filter,
            &query,
            attention_on,
            Some(page + 1),
            true,
            Some((sort_key, sort_dir)),
        )
    } else {
        String::new()
    };

    // Fragment mode: just the rows (plus the next sentinel). Used by
    // the Datastar infinite-scroll trigger to append the next page.
    if filters.fragment.as_deref() == Some("rows") {
        return render(&TransactionRowsFragment {
            transactions: page_rows,
            has_next_page,
            next_url,
            is_broker: user.role.is_broker(),
        });
    }

    // Pre-render the header strip so the template doesn't have to
    // think about URL composition — each link already encodes the
    // right next sort direction (toggle if active, default if not)
    // and carries all existing filters forward.
    let sort_headers = build_sort_headers(&status_filter, &query, attention_on, sort_key, sort_dir);

    // Mounted at both `/app` and `/app/transactions`; same view, same
    // nav highlight — Transactions is the canonical entry point.
    let header = AppHeader::new(
        &user.name,
        &user.email,
        user.role,
        &brokerage.name,
        "transactions",
    )
    .with_super_admin(crate::controllers::is_super_admin(&state, &user))
    .with_avatar(crate::db::record_key(&user.user_id), user.has_avatar)
    .with_banner(crate::billing::banner_for(&brokerage));
    let active_filter = derive_active_filter(&status_filter, attention_on);

    render(&TransactionsListPage {
        app_name: &state.config.app_name,
        base_url: &state.config.base_url,
        signed_in: true,
        header,
        transactions: page_rows,
        filter_status: &status_filter,
        query: &query,
        attention_on,
        has_next_page,
        next_url,
        total: totals.total,
        active_count: totals.active_count,
        pending_count: totals.pending_count,
        needs_attention: totals.needs_attention,
        sold_count: totals.sold_count,
        active_filter,
        sort_headers,
        is_broker: user.role.is_broker(),
    })
}

/// Sort an already-filtered slice. Uses `sort_by` (stable) with a
/// comparator that swaps direction at the top so each column's
/// comparator stays read-as-asc.
fn sort_transactions(rows: &mut [Transaction], key: SortKey, dir: SortDir) {
    use std::cmp::Ordering;
    rows.sort_by(|a, b| {
        let ord = match key {
            SortKey::Age => a.created_at.cmp(&b.created_at),
            SortKey::Property => a
                .property_address
                .to_ascii_lowercase()
                .cmp(&b.property_address.to_ascii_lowercase()),
            SortKey::Price => a.price_cents.cmp(&b.price_cents),
            SortKey::Type => a.transaction_type.cmp(&b.transaction_type),
            SortKey::Status => a.status.cmp(&b.status),
        };
        if ord == Ordering::Equal {
            // Tiebreak on created_at desc so identical rows still land
            // in a deterministic newest-first order.
            return b.created_at.cmp(&a.created_at);
        }
        match dir {
            SortDir::Asc => ord,
            SortDir::Desc => ord.reverse(),
        }
    });
}

/// One clickable header — label, the URL the click should navigate to,
/// and whether this is the currently-active column (so the template
/// can show the right arrow).
#[derive(Debug, Clone)]
pub struct SortHeader {
    pub key: &'static str,
    pub label: &'static str,
    pub url: String,
    pub active: bool,
    /// Arrow glyph: `▲` when active+asc, `▼` when active+desc, empty
    /// otherwise (the template renders a faint hint glyph for inactive
    /// columns so they read as sortable).
    pub arrow: &'static str,
}

fn build_sort_headers(
    status: &str,
    query: &str,
    attention: bool,
    current_key: SortKey,
    current_dir: SortDir,
) -> Vec<SortHeader> {
    [
        (SortKey::Property, "Property"),
        (SortKey::Price, "Price"),
        (SortKey::Type, "Type"),
        (SortKey::Age, "Age"),
        (SortKey::Status, "Status"),
    ]
    .iter()
    .map(|&(key, label)| {
        let active = key == current_key;
        // Clicking the active column flips the direction; clicking an
        // inactive column starts from its natural default.
        let next_dir = if active {
            current_dir.flip()
        } else {
            key.default_dir()
        };
        let url = build_list_url(status, query, attention, None, false, Some((key, next_dir)));
        let arrow = if active {
            match current_dir {
                SortDir::Asc => "▲",
                SortDir::Desc => "▼",
            }
        } else {
            ""
        };
        SortHeader {
            key: key.as_str(),
            label,
            url,
            active,
            arrow,
        }
    })
    .collect()
}

/// Map the request's filter combination to the stat-card the list page
/// should mark "active". An empty status + attention-off means the user
/// is looking at the unfiltered list, so we highlight Total.
///
/// Map the request's status + attention flags onto the stat card that
/// should be highlighted. Active and Pending are distinct cards now;
/// the legacy `open` alias still lights the Active card.
fn derive_active_filter(status: &str, attention_on: bool) -> &'static str {
    if attention_on {
        "attention"
    } else {
        match status {
            "" | "all" => "total",
            "active" | "open" => "active",
            "pending" => "pending",
            "sold" => "sold",
            _ => "",
        }
    }
}

/// Whether a query-string flag should be treated as on. Accepts any of
/// the common truthy spellings users (or links from the dashboard) might
/// hand us.
fn is_truthy(v: &Option<String>) -> bool {
    matches!(v.as_deref(), Some("1" | "true" | "on" | "yes"))
}

/// Build the canonical list URL for a given (filter, page, sort) tuple.
/// Used to compose the "next page" link for the infinite-scroll sentinel
/// and each clickable sortable-column header — every filter / sort
/// stays applied as the user navigates.
fn build_list_url(
    status: &str,
    query: &str,
    attention: bool,
    page: Option<usize>,
    fragment: bool,
    sort: Option<(SortKey, SortDir)>,
) -> String {
    let mut params: Vec<(&str, String)> = Vec::new();
    if !status.is_empty() && status != "all" {
        params.push(("status", status.to_string()));
    }
    if !query.trim().is_empty() {
        params.push(("q", query.to_string()));
    }
    if attention {
        params.push(("attention", "1".to_string()));
    }
    if let Some(p) = page {
        params.push(("page", p.to_string()));
    }
    if fragment {
        params.push(("fragment", "rows".to_string()));
    }
    if let Some((k, d)) = sort {
        params.push(("sort", k.as_str().to_string()));
        params.push(("dir", d.as_str().to_string()));
    }
    if params.is_empty() {
        return "/app/transactions".to_string();
    }
    let qs: Vec<String> = params
        .into_iter()
        .map(|(k, v)| format!("{k}={}", urlencoding::encode(&v)))
        .collect();
    format!("/app/transactions?{}", qs.join("&"))
}

// ---------------------------------------------------------------------------
// New / Create
// ---------------------------------------------------------------------------

pub async fn new_form(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Html<String>, AppError> {
    let brokerage = load_brokerage(&state, &user).await?;
    let header = AppHeader::new(
        &user.name,
        &user.email,
        user.role,
        &brokerage.name,
        "transactions",
    )
    .with_super_admin(crate::controllers::is_super_admin(&state, &user))
    .with_avatar(crate::db::record_key(&user.user_id), user.has_avatar)
    .with_banner(crate::billing::banner_for(&brokerage));
    render(&TransactionNewPage {
        app_name: &state.config.app_name,
        base_url: &state.config.base_url,
        signed_in: true,
        header,
        error: None,
        statuses: TransactionStatus::all().to_vec(),
        types: TransactionType::all().to_vec(),
        conditions: SpecialSalesCondition::all().to_vec(),
        sales_types: SalesType::all().to_vec(),
    })
}

#[derive(Debug, Deserialize)]
pub struct CreateInput {
    pub property_address: String,
    #[serde(default)]
    pub city: Option<String>,
    #[serde(default)]
    pub apn: Option<String>,
    #[serde(default)]
    pub postal_code: Option<String>,
    #[serde(default)]
    pub sales_price: Option<String>,
    #[serde(default)]
    pub client_name: Option<String>,
    #[serde(default)]
    pub mls_number: Option<String>,
    #[serde(default)]
    pub office_file_number: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub transaction_type: Option<String>,
    #[serde(default)]
    pub special_sales_condition: Option<String>,
    #[serde(default)]
    pub sales_type: Option<String>,
}

pub async fn create(
    State(state): State<AppState>,
    user: CurrentUser,
    Form(input): Form<CreateInput>,
) -> Result<Redirect, AppError> {
    // Land deals frequently have an APN but no street address (raw
    // parcels, off-grid lots). Accept either, but require at least
    // one. The schema still asserts `property_address` is non-empty,
    // so when only the APN is given we synthesize a placeholder
    // address from it — that keeps list/search/export rendering
    // unchanged without forcing the user to type a fake street.
    let address_input = input.property_address.trim().to_string();
    let apn_input = input
        .apn
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let property_address = match (address_input.is_empty(), &apn_input) {
        (false, _) => address_input,
        (true, Some(apn)) => format!("APN {apn}"),
        (true, None) => {
            return Err(AppError::invalid(
                "Enter a property address or an APN — at least one is required.",
            ));
        }
    };

    // Parse and validate the dropdowns. Falling back to sensible defaults
    // means a malformed POST still creates *something* the UI can correct.
    let tx_type = input
        .transaction_type
        .as_deref()
        .and_then(TransactionType::parse)
        .unwrap_or(TransactionType::Residential);
    let condition = input
        .special_sales_condition
        .as_deref()
        .and_then(SpecialSalesCondition::parse)
        .unwrap_or(SpecialSalesCondition::None);
    let sales = input
        .sales_type
        .as_deref()
        .and_then(SalesType::parse)
        .unwrap_or(SalesType::Listing);
    let status = input
        .status
        .as_deref()
        .and_then(TransactionStatus::parse)
        .unwrap_or(TransactionStatus::Active);

    let price_cents = parse_price_cents(input.sales_price.as_deref().unwrap_or(""));

    // Tier-based usage enforcement. Either allows the create, allows
    // with metered overage (Stripe usage POST is best-effort below),
    // or returns a 400 with a friendly "limit reached" message.
    let decision = crate::billing::enforce_transaction_limit(&state, &user).await?;

    let new_tx = NewTransaction {
        property_address,
        city: input.city.unwrap_or_default().trim().to_string(),
        apn: apn_input,
        postal_code: input
            .postal_code
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty()),
        price_cents,
        client_name: input
            .client_name
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
        mls_number: input
            .mls_number
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
        office_file_number: input
            .office_file_number
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
        status: status.as_str().to_string(),
        transaction_type: tx_type.as_str().to_string(),
        special_sales_condition: condition.as_str().to_string(),
        sales_type: sales.as_str().to_string(),
    };

    let tx: Option<Transaction> = state.db.create("transaction").content(new_tx).await?;
    let tx = tx.ok_or_else(|| AppError::Internal(anyhow::anyhow!("create returned nothing")))?;

    state
        .db
        .query("RELATE $b->has_transaction->$t; RELATE $u->owns->$t;")
        .bind(("b", user.brokerage_id.clone()))
        .bind(("t", tx.id.clone()))
        .bind(("u", user.user_id.clone()))
        .await?;

    seed_default_checklist(
        &state,
        &tx.id,
        &user.brokerage_id,
        tx_type,
        condition,
        sales,
    )
    .await?;

    // If this transaction pushed the brokerage over the monthly cap
    // and the tier opts into metered overage, fire-and-forget a
    // Stripe usage record. Failures are logged but don't fail the
    // create — usage reconciliation can happen at the end of the
    // billing period if Stripe was unreachable now.
    if let crate::billing::LimitDecision::AllowedAsOverage {
        stripe_subscription_id,
    } = decision
        && let Some(sub_id) = stripe_subscription_id
        && let Err(e) = state.stripe.report_overage_usage(&sub_id, 1).await
    {
        tracing::warn!(
            error = %e,
            subscription = %sub_id,
            "metered overage usage report failed (will retry at period end)"
        );
    }

    let key = crate::db::record_key(&tx.id);
    Ok(Redirect::to(&format!("/app/transactions/{key}")))
}

// ---------------------------------------------------------------------------
// Show
// ---------------------------------------------------------------------------

pub async fn show(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Html<String>, AppError> {
    let brokerage = load_brokerage(&state, &user).await?;
    let tx_id = RecordId::new("transaction", id.as_str());
    let tx = authorize_transaction(&state, &user, &tx_id).await?;

    let items = load_checklist(&state, &tx.id).await?;
    let groups = build_grouped_checklist(&state, items).await?;
    let owner_name = load_transaction_owner_name(&state, &tx.id).await?;
    let available_forms = available_forms(&groups);
    let transaction_comments = load_comments(&state, &tx.id).await?;

    let header = AppHeader::new(
        &user.name,
        &user.email,
        user.role,
        &brokerage.name,
        "transactions",
    )
    .with_super_admin(crate::controllers::is_super_admin(&state, &user))
    .with_avatar(crate::db::record_key(&user.user_id), user.has_avatar)
    .with_banner(crate::billing::banner_for(&brokerage));
    let tx_key = crate::db::record_key(&tx.id);
    let can_review = user.role.can_review();
    render(&TransactionShowPage {
        app_name: &state.config.app_name,
        base_url: &state.config.base_url,
        signed_in: true,
        header,
        compliance: CompliancePanel::build(groups, tx_key.clone()),
        owner_name,
        transaction_key: tx_key,
        transaction: tx,
        available_forms,
        statuses: TransactionStatus::all().to_vec(),
        transaction_comments,
        can_review,
    })
}

// ---------------------------------------------------------------------------
// Status update
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct StatusInput {
    pub status: String,
}

pub async fn update_status(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<String>,
    Form(input): Form<StatusInput>,
) -> Result<Response, AppError> {
    let tx_id = RecordId::new("transaction", id.as_str());
    let _tx = authorize_transaction(&state, &user, &tx_id).await?;

    if TransactionStatus::parse(&input.status).is_none() {
        return Err(AppError::invalid("Unknown status"));
    }

    state
        .db
        .query("UPDATE $t SET status = $s")
        .bind(("t", tx_id.clone()))
        .bind(("s", input.status))
        .await?;

    Ok(Redirect::to(&format!("/app/transactions/{id}")).into_response())
}

/// Broker-only hard delete of a single transaction and everything
/// hanging off it — documents (DB rows + storage objects), checklist
/// items, comments, and all graph edges. Added for the corrections
/// set so a broker can clean up duplicate / mistaken transactions
/// without an admin. The confirm dialog lives in the row template;
/// the role gate here is the real guard.
pub async fn delete(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Redirect, AppError> {
    if !user.role.is_broker() {
        return Err(AppError::Forbidden);
    }
    let tx_id = RecordId::new("transaction", id.as_str());
    // authorize_transaction confirms the caller's brokerage owns this
    // transaction — a broker can't delete another office's record.
    let tx = authorize_transaction(&state, &user, &tx_id).await?;

    // Collect storage keys before the rows vanish so we can purge the
    // bucket. Best-effort: a failed object delete is logged, not fatal.
    let mut kq = state
        .db
        .query("SELECT VALUE storage_key FROM $t->has_document->document")
        .bind(("t", tx_id.clone()))
        .await?;
    let keys: Vec<String> = kq.take(0).unwrap_or_default();

    // Audit BEFORE the cascade so the intent is recorded even if a
    // later step fails partway through.
    crate::audit::record(
        &state.db,
        "transaction_deleted",
        Some(user.user_id.clone()),
        Some(user.email.clone()),
        None,
        None,
        Some(format!("address=\"{}\" key={id}", tx.property_address)),
    )
    .await;

    for key in &keys {
        if let Err(e) = state.storage.delete(key).await {
            tracing::warn!(error = %e, %key, "transaction delete: storage purge failed");
        }
    }

    // DB cascade in a single transaction so we never leave a
    // half-deleted graph behind.
    state
        .db
        .query(
            r#"
            BEGIN TRANSACTION;
            LET $doc_ids  = $t->has_document->document.id;
            LET $item_ids = $t->has_item->checklist_item.id;
            DELETE comment      WHERE target = $t OR target IN $item_ids;
            DELETE for_item     WHERE in IN $doc_ids;
            DELETE version_of   WHERE in IN $doc_ids OR out IN $doc_ids;
            DELETE uploaded     WHERE out IN $doc_ids;
            DELETE has_document WHERE out IN $doc_ids;
            DELETE document     WHERE id IN $doc_ids;
            DELETE has_item     WHERE out IN $item_ids;
            DELETE checklist_item WHERE id IN $item_ids;
            DELETE has_transaction WHERE out = $t;
            DELETE owns            WHERE out = $t;
            DELETE transaction     WHERE id = $t;
            COMMIT TRANSACTION;
            "#,
        )
        .bind(("t", tx_id.clone()))
        .await?;

    Ok(Redirect::to("/app/transactions?flash=tx_deleted"))
}

// ---------------------------------------------------------------------------
// Edit / Update
// ---------------------------------------------------------------------------

pub async fn edit_form(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Html<String>, AppError> {
    let brokerage = load_brokerage(&state, &user).await?;
    let tx_id = RecordId::new("transaction", id.as_str());
    let tx = authorize_transaction(&state, &user, &tx_id).await?;

    // Fully-approved transactions are read-only — the "Edit" button is
    // hidden in this state but enforce it server-side too so the GET
    // can't be opened by typing the URL.
    if transaction_fully_approved(&state, &tx.id).await? {
        return Err(AppError::invalid(
            "This transaction is fully approved and locked. Have a coordinator deny an item if you need to edit details.",
        ));
    }

    let dropdowns_locked = any_item_reviewed(&state, &tx.id).await?;

    let header = AppHeader::new(
        &user.name,
        &user.email,
        user.role,
        &brokerage.name,
        "transactions",
    )
    .with_super_admin(crate::controllers::is_super_admin(&state, &user))
    .with_avatar(crate::db::record_key(&user.user_id), user.has_avatar)
    .with_banner(crate::billing::banner_for(&brokerage));
    let tx_key = crate::db::record_key(&tx.id);
    render(&TransactionEditPage {
        app_name: &state.config.app_name,
        base_url: &state.config.base_url,
        signed_in: true,
        header,
        transaction_key: tx_key,
        transaction: tx,
        statuses: TransactionStatus::all().to_vec(),
        types: TransactionType::all().to_vec(),
        conditions: SpecialSalesCondition::all().to_vec(),
        sales_types: SalesType::all().to_vec(),
        dropdowns_locked,
        error: None,
    })
}

pub async fn update(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<String>,
    Form(input): Form<CreateInput>,
) -> Result<Redirect, AppError> {
    let tx_id = RecordId::new("transaction", id.as_str());
    let tx = authorize_transaction(&state, &user, &tx_id).await?;

    if transaction_fully_approved(&state, &tx.id).await? {
        return Err(AppError::invalid(
            "This transaction is fully approved and locked.",
        ));
    }

    // Same "address OR APN" rule as create. Synthesize an `APN …`
    // placeholder for the address when only the APN is given so the
    // schema's non-empty ASSERT stays satisfied and the list/search
    // views keep working.
    let address_input = input.property_address.trim().to_string();
    let apn_input = input
        .apn
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let property_address = match (address_input.is_empty(), &apn_input) {
        (false, _) => address_input,
        (true, Some(apn)) => format!("APN {apn}"),
        (true, None) => {
            return Err(AppError::invalid(
                "Enter a property address or an APN — at least one is required.",
            ));
        }
    };

    // Parse new dropdown values, falling back to the existing record's
    // values when an input is missing/unknown — this keeps "disabled"
    // selects (which don't post a value) from clobbering the row.
    let new_type = input
        .transaction_type
        .as_deref()
        .and_then(TransactionType::parse)
        .unwrap_or_else(|| tx.type_enum());
    let new_condition = input
        .special_sales_condition
        .as_deref()
        .and_then(SpecialSalesCondition::parse)
        .unwrap_or_else(|| tx.condition_enum());
    let new_sales = input
        .sales_type
        .as_deref()
        .and_then(SalesType::parse)
        .unwrap_or_else(|| tx.sales_enum());
    let new_status = input
        .status
        .as_deref()
        .and_then(TransactionStatus::parse)
        .unwrap_or_else(|| tx.status_enum());

    let old_type = tx.type_enum();
    let old_condition = tx.condition_enum();
    let old_sales = tx.sales_enum();

    let dropdowns_changed =
        new_type != old_type || new_condition != old_condition || new_sales != old_sales;

    // Lock rule: once any checklist item has been approved or denied, the
    // three special-condition dropdowns are frozen because changing them
    // would reshape the required-forms set under the reviewer's feet.
    // Cosmetic fields (address, APN, price, etc.) stay editable.
    if dropdowns_changed && any_item_reviewed(&state, &tx.id).await? {
        return Err(AppError::invalid(
            "Special conditions are locked because at least one checklist item has been approved or denied. Reset those items to pending before changing the transaction type, sales type, or special condition.",
        ));
    }

    let price_cents = parse_price_cents(input.sales_price.as_deref().unwrap_or(""));

    state
        .db
        .query(
            "UPDATE $t SET
                property_address       = $address,
                city                   = $city,
                apn                    = $apn,
                postal_code            = $postal,
                price_cents            = $price,
                client_name            = $client,
                mls_number             = $mls,
                office_file_number     = $office,
                status                 = $status,
                transaction_type       = $tx_type,
                special_sales_condition = $cond,
                sales_type             = $sales",
        )
        .bind(("t", tx_id.clone()))
        .bind(("address", property_address))
        .bind(("city", input.city.unwrap_or_default().trim().to_string()))
        .bind(("apn", apn_input))
        .bind((
            "postal",
            input
                .postal_code
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
        ))
        .bind(("price", price_cents))
        .bind((
            "client",
            input
                .client_name
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
        ))
        .bind((
            "mls",
            input
                .mls_number
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
        ))
        .bind((
            "office",
            input
                .office_file_number
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
        ))
        .bind(("status", new_status.as_str().to_string()))
        .bind(("tx_type", new_type.as_str().to_string()))
        .bind(("cond", new_condition.as_str().to_string()))
        .bind(("sales", new_sales.as_str().to_string()))
        .await?;

    if dropdowns_changed {
        reconcile_checklist(&state, &tx_id, new_type, new_condition, new_sales).await?;
    }

    Ok(Redirect::to(&format!("/app/transactions/{id}")))
}

/// True when every checklist item on the transaction has been approved.
/// Mirrors the in-template `compliance.is_locked()`; we recompute here
/// because the edit endpoints don't build a `CompliancePanel`. An empty
/// checklist is *not* locked — there's nothing to approve.
async fn transaction_fully_approved(state: &AppState, tx_id: &RecordId) -> Result<bool, AppError> {
    #[derive(serde::Deserialize, SurrealValue)]
    struct Row {
        total: i64,
        approved: i64,
    }
    let mut r = state
        .db
        .query(
            "SELECT
                count() AS total,
                count(approval_status = 'approved') AS approved
             FROM $t->has_item->checklist_item
             GROUP ALL",
        )
        .bind(("t", tx_id.clone()))
        .await?;
    let row: Option<Row> = r.take(0)?;
    Ok(match row {
        Some(r) => r.total > 0 && r.total == r.approved,
        None => false,
    })
}

/// True when any checklist item on the transaction has been approved or
/// denied — the trigger that locks the special-conditions dropdowns.
async fn any_item_reviewed(state: &AppState, tx_id: &RecordId) -> Result<bool, AppError> {
    #[derive(serde::Deserialize, SurrealValue)]
    struct CountRow {
        count: i64,
    }
    let mut q = state
        .db
        .query(
            "SELECT count() FROM $t->has_item->checklist_item
             WHERE approval_status IN ['approved', 'denied']
             GROUP ALL",
        )
        .bind(("t", tx_id.clone()))
        .await?;
    let row: Option<CountRow> = q.take(0)?;
    Ok(row.map(|c| c.count > 0).unwrap_or(false))
}

/// Bring the checklist back in sync with the special-conditions dropdowns
/// after they've been edited. Three cases per row:
///
/// 1. Item's `form_code` is still in the new required set → update its
///    `group_slug` and `required` flag so it reflects the new layout.
/// 2. Item's `form_code` is no longer required and has no documents
///    attached → delete the row (and the `has_item` edge).
/// 3. Item's `form_code` is no longer required *but* has documents
///    attached → move it to "Additional Disclosures" and clear the
///    required flag, so the uploaded files survive the recategorization.
///
/// Custom items (`form_code = None`) are left alone. Any form in the new
/// required set that isn't already on the checklist is added.
async fn reconcile_checklist(
    state: &AppState,
    tx_id: &RecordId,
    tx_type: TransactionType,
    cond: SpecialSalesCondition,
    sales: SalesType,
) -> Result<(), AppError> {
    let defaults = forms::build_default_checklist(tx_type, cond, sales);
    let defaults_by_code: std::collections::HashMap<&str, &forms::DefaultItem> =
        defaults.iter().map(|d| (d.code, d)).collect();

    let items = load_checklist(state, tx_id).await?;
    let (additional_name, additional_order) = FormGroup::AdditionalDisclosures.seed_group();

    // First pass: update or relocate existing items.
    for item in &items {
        let Some(code) = item.form_code.as_deref() else {
            continue;
        };
        match defaults_by_code.get(code) {
            Some(target) => {
                let (gname, gorder) = target.group.seed_group();
                if item.group_name.as_str() != gname || target.required != item.required {
                    state
                        .db
                        .query("UPDATE $i SET group_name = $gn, group_order = $go, required = $r")
                        .bind(("i", item.id.clone()))
                        .bind(("gn", gname.to_string()))
                        .bind(("go", gorder))
                        .bind(("r", target.required))
                        .await?;
                }
            }
            None => {
                // No longer part of the required set. Preserve any
                // uploaded documents by relocating the item to the
                // catch-all group; otherwise drop the row entirely.
                let has_docs = item_has_documents(state, &item.id).await?;
                if has_docs {
                    if item.group_name.as_str() != additional_name || item.required {
                        state
                            .db
                            .query(
                                "UPDATE $i SET group_name = $gn, group_order = $go, required = false",
                            )
                            .bind(("i", item.id.clone()))
                            .bind(("gn", additional_name.to_string()))
                            .bind(("go", additional_order))
                            .await?;
                    }
                } else {
                    state
                        .db
                        .query("DELETE has_item WHERE out = $i; DELETE $i;")
                        .bind(("i", item.id.clone()))
                        .await?;
                }
            }
        }
    }

    // Second pass: add any newly-required forms that aren't on the list yet.
    let existing_codes: std::collections::HashSet<&str> = items
        .iter()
        .filter_map(|i| i.form_code.as_deref())
        .collect();

    let max_position = items.iter().map(|i| i.position).max().unwrap_or(-1);
    let to_add: Vec<&forms::DefaultItem> = defaults
        .iter()
        .filter(|d| !existing_codes.contains(d.code))
        .collect();

    if !to_add.is_empty() {
        let create_futures = to_add.iter().enumerate().map(|(i, d)| {
            let position = max_position + 1 + i as i64;
            let title = forms::lookup(d.code)
                .map(|f| f.name.to_string())
                .unwrap_or_else(|| d.code.to_string());
            let (gname, gorder) = d.group.seed_group();
            async move {
                let new_item: Option<ChecklistItem> = state
                    .db
                    .create("checklist_item")
                    .content(NewChecklistItem {
                        title,
                        form_code: Some(d.code.to_string()),
                        group_name: gname.to_string(),
                        group_order: gorder,
                        position,
                        required: d.required,
                    })
                    .await?;
                let id = new_item.map(|c| c.id).ok_or_else(|| {
                    AppError::Internal(anyhow::anyhow!("checklist insert returned nothing"))
                })?;
                Ok::<RecordId, AppError>(id)
            }
        });
        let ids: Vec<RecordId> = futures::future::try_join_all(create_futures).await?;
        if !ids.is_empty() {
            state
                .db
                .query("FOR $cid IN $items { RELATE $t->has_item->$cid }")
                .bind(("t", tx_id.clone()))
                .bind(("items", ids))
                .await?;
        }
    }

    Ok(())
}

async fn item_has_documents(state: &AppState, item_id: &RecordId) -> Result<bool, AppError> {
    #[derive(serde::Deserialize, SurrealValue)]
    struct CountRow {
        count: i64,
    }
    let mut q = state
        .db
        .query("SELECT count() FROM $i<-for_item<-document GROUP ALL")
        .bind(("i", item_id.clone()))
        .await?;
    let row: Option<CountRow> = q.take(0)?;
    Ok(row.map(|c| c.count > 0).unwrap_or(false))
}

// ---------------------------------------------------------------------------
// Search
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct SearchInput {
    #[serde(default)]
    pub q: Option<String>,
    /// Same status vocabulary as the transactions list (no "open"
    /// alias surfaced; `all`/empty means no filter).
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub sort: Option<String>,
    #[serde(default)]
    pub dir: Option<String>,
}

pub async fn search(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(input): Query<SearchInput>,
) -> Result<Html<String>, AppError> {
    let brokerage = load_brokerage(&state, &user).await?;
    let query = input.q.unwrap_or_default();
    let needle = query.trim().to_ascii_lowercase();
    let status_filter = input.status.clone().unwrap_or_default();
    let sort_key = input
        .sort
        .as_deref()
        .and_then(SortKey::parse)
        .unwrap_or(SortKey::Property);
    let sort_dir = input
        .dir
        .as_deref()
        .and_then(SortDir::parse)
        .unwrap_or_else(|| sort_key.default_dir());

    let (transactions, documents) = if needle.is_empty() {
        (Vec::new(), Vec::new())
    } else {
        // Scope is unchanged from the list: brokers/COs see the whole
        // brokerage, agents see only what they own.
        let all = load_visible_transactions(&state, &user).await?;
        let mut filtered: Vec<Transaction> = all
            .iter()
            .filter(|t| {
                t.property_address.to_ascii_lowercase().contains(&needle)
                    || t.city.to_ascii_lowercase().contains(&needle)
                    || t.client_name
                        .as_deref()
                        .map(|s| s.to_ascii_lowercase().contains(&needle))
                        .unwrap_or(false)
                    || t.mls_number
                        .as_deref()
                        .map(|s| s.to_ascii_lowercase().contains(&needle))
                        .unwrap_or(false)
            })
            .cloned()
            .collect();

        if !status_filter.is_empty() && status_filter != "all" {
            filtered.retain(|t| t.status == status_filter);
        }
        sort_transactions(&mut filtered, sort_key, sort_dir);

        let docs = search_documents(&state, &all, &needle).await?;
        (filtered, docs)
    };

    let sort_headers = build_search_sort_headers(&query, &status_filter, sort_key, sort_dir);

    let header = AppHeader::new(
        &user.name,
        &user.email,
        user.role,
        &brokerage.name,
        "search",
    )
    .with_super_admin(crate::controllers::is_super_admin(&state, &user))
    .with_avatar(crate::db::record_key(&user.user_id), user.has_avatar)
    .with_banner(crate::billing::banner_for(&brokerage));
    render(&SearchPage {
        app_name: &state.config.app_name,
        base_url: &state.config.base_url,
        signed_in: true,
        header,
        query: &query,
        status_filter: &status_filter,
        sort_headers,
        transactions,
        documents,
    })
}

/// Sortable-column header strip for the search page. Mirrors
/// [`build_sort_headers`] but the links point back at `/app/search`
/// and carry the query + status filter forward.
fn build_search_sort_headers(
    query: &str,
    status: &str,
    current_key: SortKey,
    current_dir: SortDir,
) -> Vec<SortHeader> {
    [
        (SortKey::Property, "Property"),
        (SortKey::Price, "Price"),
        (SortKey::Type, "Type"),
        (SortKey::Age, "Age"),
        (SortKey::Status, "Status"),
    ]
    .iter()
    .map(|&(key, label)| {
        let active = key == current_key;
        let next_dir = if active {
            current_dir.flip()
        } else {
            key.default_dir()
        };
        let mut params: Vec<(&str, String)> = vec![("q", query.to_string())];
        if !status.is_empty() && status != "all" {
            params.push(("status", status.to_string()));
        }
        params.push(("sort", key.as_str().to_string()));
        params.push(("dir", next_dir.as_str().to_string()));
        let qs = params
            .iter()
            .map(|(k, v)| format!("{k}={}", urlencoding::encode(v)))
            .collect::<Vec<_>>()
            .join("&");
        let arrow = if active {
            if current_dir == SortDir::Asc {
                "▲"
            } else {
                "▼"
            }
        } else {
            ""
        };
        SortHeader {
            key: key.as_str(),
            label,
            url: format!("/app/search?{qs}"),
            active,
            arrow,
        }
    })
    .collect()
}

// ---------------------------------------------------------------------------
// Shared loaders (crate-visible so checklists/documents/members can reuse)
// ---------------------------------------------------------------------------

pub(crate) async fn load_brokerage(
    state: &AppState,
    user: &CurrentUser,
) -> Result<Brokerage, AppError> {
    let brokerage: Option<Brokerage> = state.db.select(user.brokerage_id.clone()).await?;
    brokerage.ok_or(AppError::NotFound)
}

/// Load every transaction the current user is allowed to see.
///
/// Visibility rule (single source of truth for the whole app):
///
/// - **Broker** and **Compliance Officer**: see every transaction in
///   their brokerage. Graph traversal: `brokerage → has_transaction →
///   transaction`. The brokerage is established from the JWT's
///   `works_at` edge by [`crate::auth::middleware`], so a user with no
///   active membership gets no rows back.
/// - **Agent**: sees only transactions they own. Graph traversal:
///   `user → owns → transaction`. Brokers/COs can create transactions
///   on behalf of an agent — the `owns` edge points at the agent.
///
/// The role gate lives on [`Role::sees_all_transactions`]; keep both
/// in sync if a new role is introduced.
///
/// Order is `created_at DESC` here (newest first); callers that need a
/// different sort do their own in-Rust sort after the fetch.
pub(crate) async fn load_visible_transactions(
    state: &AppState,
    user: &CurrentUser,
) -> Result<Vec<Transaction>, AppError> {
    let surql = if user.role.sees_all_transactions() {
        "SELECT * FROM $b->has_transaction->transaction ORDER BY created_at DESC"
    } else {
        "SELECT * FROM $u->owns->transaction ORDER BY created_at DESC"
    };

    let mut response = state
        .db
        .query(surql)
        .bind(("b", user.brokerage_id.clone()))
        .bind(("u", user.user_id.clone()))
        .await?;
    let transactions: Vec<Transaction> = response.take(0)?;
    Ok(transactions)
}

pub(crate) async fn authorize_transaction(
    state: &AppState,
    user: &CurrentUser,
    tx_id: &RecordId,
) -> Result<Transaction, AppError> {
    let tx: Option<Transaction> = state.db.select(tx_id.clone()).await?;
    let tx = tx.ok_or(AppError::NotFound)?;

    let mut in_brokerage = state
        .db
        .query("SELECT count() FROM has_transaction WHERE in = $b AND out = $t GROUP ALL")
        .bind(("b", user.brokerage_id.clone()))
        .bind(("t", tx_id.clone()))
        .await?;
    let count: Option<CountRow> = in_brokerage.take(0)?;
    if count.map(|c| c.count).unwrap_or(0) == 0 {
        return Err(AppError::NotFound);
    }

    if !user.role.sees_all_transactions() {
        let mut owned = state
            .db
            .query("SELECT count() FROM owns WHERE in = $u AND out = $t GROUP ALL")
            .bind(("u", user.user_id.clone()))
            .bind(("t", tx_id.clone()))
            .await?;
        let owns: Option<CountRow> = owned.take(0)?;
        if owns.map(|c| c.count).unwrap_or(0) == 0 {
            return Err(AppError::Forbidden);
        }
    }

    Ok(tx)
}

#[derive(Debug, serde::Deserialize, SurrealValue)]
struct CountRow {
    count: i64,
}

async fn load_checklist(
    state: &AppState,
    tx_id: &RecordId,
) -> Result<Vec<ChecklistItem>, AppError> {
    let mut response = state
        .db
        .query("SELECT * FROM $t->has_item->checklist_item ORDER BY position ASC, created_at ASC")
        .bind(("t", tx_id.clone()))
        .await?;
    let items: Vec<ChecklistItem> = response.take(0)?;
    Ok(items)
}

#[derive(Debug, serde::Deserialize, SurrealValue)]
struct NameOnly {
    name: String,
}

/// Build the grouped checklist view: bucket items by group, attach
/// per-item documents, and pre-render audit strings.
async fn build_grouped_checklist(
    state: &AppState,
    items: Vec<ChecklistItem>,
) -> Result<Vec<ChecklistGroup>, AppError> {
    // Per-item documents — one query per item is fine for the volumes we
    // expect; cheaper than a single mega-query that has to be split client-side.
    let docs_per_item = futures::future::try_join_all(items.iter().map(|item| async {
        let mut r = state
            .db
            .query("SELECT * FROM $i<-for_item<-document ORDER BY version DESC, created_at DESC")
            .bind(("i", item.id.clone()))
            .await?;
        let docs: Vec<Document> = r.take(0).unwrap_or_default();
        Ok::<Vec<Document>, AppError>(docs)
    }))
    .await?;

    let audit_labels = futures::future::try_join_all(items.iter().map(|item| async move {
        match (&item.reviewed_by, item.reviewed_at) {
            (Some(uid), Some(when)) => {
                let profile: Option<NameOnly> =
                    state.db.select(uid.clone()).await.map_err(AppError::from)?;
                let who = profile.map(|p| p.name).unwrap_or_else(|| "Someone".into());
                let verb = match item.status() {
                    crate::models::ApprovalStatus::Approved => "Approved",
                    crate::models::ApprovalStatus::Denied => "Denied",
                    crate::models::ApprovalStatus::Pending => "Reviewed",
                };
                Ok::<_, AppError>(format!("{verb} by {who} on {}", when.format("%b %-d, %Y")))
            }
            _ => Ok::<_, AppError>(String::new()),
        }
    }))
    .await?;

    let comments_per_item =
        futures::future::try_join_all(items.iter().map(|item| load_comments(state, &item.id)))
            .await?;

    // Bucket rows by the group snapshotted on each item (name + order).
    // Groups are data-driven now, so we discover them from the items
    // rather than a fixed enum, then sort groups by `group_order`.
    let mut buckets: Vec<(i64, String, Vec<ChecklistRow>)> = Vec::new();

    for (((item, docs), audit), comments) in items
        .into_iter()
        .zip(docs_per_item)
        .zip(audit_labels)
        .zip(comments_per_item)
    {
        let group_order = item.group_order;
        let group_name = item.group_name.clone();
        let form = item.form_code.as_deref().and_then(forms::lookup);
        let row = ChecklistRow {
            item,
            form,
            audit_label: audit,
            documents: docs,
            comments,
        };
        match buckets.iter_mut().find(|(_, name, _)| *name == group_name) {
            Some((_, _, bucket)) => bucket.push(row),
            None => buckets.push((group_order, group_name, vec![row])),
        }
    }

    // Groups in ascending order; within a group, items by their stored
    // position (canonical form order for seeded rows, append order for
    // manual adds), tie-broken by creation time.
    buckets.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    for (_, _, bucket) in buckets.iter_mut() {
        bucket.sort_by_key(|row| (row.item.position, row.item.created_at));
    }

    let groups = buckets
        .into_iter()
        .map(|(order, name, items)| ChecklistGroup::build(name, order, items))
        .collect();
    Ok(groups)
}

/// Load comments attached to a single target (transaction or checklist
/// item), returning denormalised rows ready for template rendering.
pub async fn load_comments(
    state: &AppState,
    target: &RecordId,
) -> Result<Vec<CommentView>, AppError> {
    let mut response = state
        .db
        .query(
            "SELECT body, created_at, \
                    author.id AS author_id, \
                    author.name AS author_name, \
                    author.avatar_storage_key AS author_avatar_key, \
                    references_document.id AS ref_id, \
                    references_document.filename AS ref_filename, \
                    references_document.version AS ref_version \
             FROM comment WHERE target = $t ORDER BY created_at ASC",
        )
        .bind(("t", target.clone()))
        .await?;
    #[derive(Debug, serde::Deserialize, SurrealValue)]
    struct Row {
        body: String,
        author_id: Option<RecordId>,
        author_name: Option<String>,
        author_avatar_key: Option<String>,
        created_at: chrono::DateTime<chrono::Utc>,
        ref_id: Option<RecordId>,
        ref_filename: Option<String>,
        ref_version: Option<i64>,
    }
    let rows: Vec<Row> = response.take(0).unwrap_or_default();
    let comments = rows
        .into_iter()
        .map(|r| {
            let referenced_document = match (r.ref_id, r.ref_filename, r.ref_version) {
                (Some(id), Some(filename), Some(version)) => {
                    Some(crate::templates::ReferencedDocument {
                        key: crate::db::record_key(&id),
                        filename,
                        version,
                    })
                }
                _ => None,
            };
            let author_name = r.author_name.unwrap_or_else(|| "Someone".into());
            let author_initials = crate::templates::initials(&author_name);
            let author_key = r
                .author_id
                .as_ref()
                .map(crate::db::record_key)
                .unwrap_or_default();
            CommentView {
                body: r.body,
                author_initials,
                author_name,
                author_key,
                author_has_avatar: r.author_avatar_key.is_some(),
                created_at: r.created_at,
                referenced_document,
            }
        })
        .collect();
    Ok(comments)
}

/// All CAR forms not currently on this transaction's checklist — feeds the
/// "Add optional form" picker. Forms marked `allows_multiple` always stay
/// available even if already attached.
///
/// Sorted case-insensitively by code so the picker reads alphabetically.
/// We can't return a `Vec<&'static CarForm>` from a sorted reference into
/// the static `LIBRARY` because the library itself isn't pre-sorted (it
/// groups forms thematically), so we sort the filtered references at
/// request time. With ~300 entries this is a few microseconds and not
/// worth caching.
fn available_forms(groups: &[ChecklistGroup]) -> Vec<&'static crate::forms::CarForm> {
    let used: std::collections::HashSet<&str> = groups
        .iter()
        .flat_map(|g| g.items.iter())
        .filter_map(|r| r.form.map(|f| f.code))
        .collect();
    let mut out: Vec<&'static crate::forms::CarForm> = forms::LIBRARY
        .iter()
        .filter(|f| f.allows_multiple || !used.contains(f.code))
        .collect();
    out.sort_by_key(|f| f.code.to_ascii_lowercase());
    out
}

async fn load_transaction_owner_name(
    state: &AppState,
    tx_id: &RecordId,
) -> Result<String, AppError> {
    let mut response = state
        .db
        .query("SELECT VALUE in.name FROM owns WHERE out = $t LIMIT 1")
        .bind(("t", tx_id.clone()))
        .await?;
    let names: Vec<String> = response.take(0).unwrap_or_default();
    Ok(names
        .into_iter()
        .next()
        .unwrap_or_else(|| "Unassigned".into()))
}

/// True/false per transaction (aligned with the input slice): does
/// this transaction need the *viewer's* attention? The predicate is
/// role-split (corrections set):
///
/// - **Agents** respond to reviewer feedback — flag if a checklist
///   item was denied/rejected, OR a broker/compliance officer left a
///   comment on the transaction (or one of its items).
/// - **Brokers / Compliance Officers** review agent work — flag if an
///   item has an uploaded document still awaiting review (pending with
///   a file attached), OR an agent left a comment.
///
/// Closed transactions (Sold/Canceled/Withdrawn) always return `false`
/// — nothing on a finished deal is actionable.
async fn needs_attention_flags(
    state: &AppState,
    transactions: &[Transaction],
    role: Role,
    brokerage_id: &RecordId,
) -> Result<Vec<bool>, AppError> {
    if transactions.is_empty() {
        return Ok(Vec::new());
    }

    // Reviewers (broker / coordinator) watch for agent activity;
    // agents watch for reviewer activity. This single bool drives both
    // the form predicate and which comment authors count.
    let reviewer_view = role.sees_all_transactions();
    let author_roles: Vec<String> = if reviewer_view {
        vec!["agent".to_string()]
    } else {
        vec!["broker".to_string(), "coordinator".to_string()]
    };

    // The set of users whose comments should flag a transaction —
    // fetched once for the whole brokerage rather than per row.
    let mut au = state
        .db
        .query("SELECT VALUE in FROM works_at WHERE out = $b AND role IN $roles")
        .bind(("b", brokerage_id.clone()))
        .bind(("roles", author_roles))
        .await?;
    let comment_authors: Vec<RecordId> = au.take(0).unwrap_or_default();

    let futures = transactions.iter().map(|t| {
        let comment_authors = comment_authors.clone();
        async move {
            if matches!(
                t.status_enum(),
                TransactionStatus::Sold
                    | TransactionStatus::Canceled
                    | TransactionStatus::Withdrawn
            ) {
                return Ok::<bool, AppError>(false);
            }

            // Form signal — denied (agent view) vs pending-with-upload
            // (reviewer view).
            let form_query = if reviewer_view {
                "SELECT count() FROM $t->has_item->checklist_item \
                 WHERE approval_status = 'pending' \
                   AND array::len(<-for_item<-document) > 0 GROUP ALL"
            } else {
                "SELECT count() FROM $t->has_item->checklist_item \
                 WHERE approval_status = 'denied' GROUP ALL"
            };
            let mut fr = state.db.query(form_query).bind(("t", t.id.clone())).await?;
            let forms: Option<CountRow> = fr.take(0)?;
            if forms.map(|c| c.count > 0).unwrap_or(false) {
                return Ok(true);
            }

            // Comment signal — a note from the other role on the
            // transaction or any of its checklist items.
            let mut cr = state
                .db
                .query(
                    "SELECT count() FROM comment \
                     WHERE (target = $t OR target IN (SELECT VALUE id FROM $t->has_item->checklist_item)) \
                       AND author IN $authors GROUP ALL",
                )
                .bind(("t", t.id.clone()))
                .bind(("authors", comment_authors))
                .await?;
            let comments: Option<CountRow> = cr.take(0)?;
            Ok(comments.map(|c| c.count > 0).unwrap_or(false))
        }
    });
    let results = futures::future::try_join_all(futures).await?;
    Ok(results)
}

/// Count of transactions that need the viewer's attention. Thin
/// wrapper around [`needs_attention_flags`] used by the dashboard
/// summary card.
async fn count_needs_attention(
    state: &AppState,
    transactions: &[Transaction],
    role: Role,
    brokerage_id: &RecordId,
) -> Result<usize, AppError> {
    let flags = needs_attention_flags(state, transactions, role, brokerage_id).await?;
    Ok(flags.into_iter().filter(|&b| b).count())
}

async fn search_documents(
    state: &AppState,
    transactions: &[Transaction],
    needle: &str,
) -> Result<Vec<SearchDocument>, AppError> {
    let lookups = transactions.iter().map(|t| async move {
        let mut response = state
            .db
            .query("SELECT * FROM $t->has_document->document")
            .bind(("t", t.id.clone()))
            .await?;
        let docs: Vec<Document> = response.take(0)?;
        Ok::<_, AppError>((t.clone(), docs))
    });

    let pairs = futures::future::try_join_all(lookups).await?;
    let matches: Vec<SearchDocument> = pairs
        .into_iter()
        .flat_map(|(tx, docs)| {
            docs.into_iter()
                .filter(|d| {
                    d.filename.to_ascii_lowercase().contains(needle)
                        || d.form_code.to_ascii_lowercase().contains(needle)
                })
                .map(move |d| SearchDocument {
                    document: d,
                    transaction_key: crate::db::record_key(&tx.id),
                    transaction_address: tx.property_address.clone(),
                })
                .collect::<Vec<_>>()
        })
        .collect();
    Ok(matches)
}

/// Seed the default California checklist for a freshly-created transaction.
async fn seed_default_checklist(
    state: &AppState,
    tx_id: &RecordId,
    brokerage_id: &RecordId,
    tx_type: TransactionType,
    cond: SpecialSalesCondition,
    sales: SalesType,
) -> Result<(), AppError> {
    // Each item: (title, form_code, group_name, group_order, position, required).
    type Seed = (String, Option<String>, String, i64, i64, bool);

    let side = forms::sales_side(sales);
    let resolved = crate::db::forms::resolve_checklist(
        &state.db,
        brokerage_id,
        tx_type.as_str(),
        side.as_str(),
        cond.as_str(),
    )
    .await
    .map_err(|e| AppError::Internal(e.context("resolve checklist")))?;

    let seeds: Vec<Seed> = if resolved.is_empty() {
        // Fallback: brokerage has no form set wired (shouldn't happen
        // after signup attaches California, but never ship an empty
        // checklist). Use the in-memory engine, mapping each group
        // through `seed_group()` so names match the DB-resolved path.
        forms::build_default_checklist(tx_type, cond, sales)
            .into_iter()
            .map(|di| {
                let (gname, gorder) = di.group.seed_group();
                let title = forms::lookup(di.code)
                    .map(|f| f.name.to_string())
                    .unwrap_or_else(|| di.code.to_string());
                (
                    title,
                    Some(di.code.to_string()),
                    gname.to_string(),
                    gorder,
                    forms::canonical_position(di.code) as i64,
                    di.required,
                )
            })
            .collect()
    } else {
        resolved
            .into_iter()
            .map(|f| {
                (
                    f.name,
                    Some(f.code),
                    f.group_name,
                    f.group_order,
                    f.form_order,
                    f.required,
                )
            })
            .collect()
    };

    let item_futures = seeds.into_iter().map(|seed| async move {
        let (title, form_code, group_name, group_order, position, required) = seed;
        let item: Option<ChecklistItem> = state
            .db
            .create("checklist_item")
            .content(NewChecklistItem {
                title,
                form_code,
                group_name,
                group_order,
                position,
                required,
            })
            .await?;
        let id = item.map(|c| c.id).ok_or_else(|| {
            AppError::Internal(anyhow::anyhow!("checklist insert returned nothing"))
        })?;
        Ok::<RecordId, AppError>(id)
    });
    let ids: Vec<RecordId> = futures::future::try_join_all(item_futures).await?;

    if !ids.is_empty() {
        state
            .db
            .query("FOR $cid IN $items { RELATE $t->has_item->$cid }")
            .bind(("t", tx_id.clone()))
            .bind(("items", ids))
            .await?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Local helpers
// ---------------------------------------------------------------------------

/// Accept inputs like `$649,000`, `649000`, `649000.00`. Returns 0 on empty.
fn parse_price_cents(input: &str) -> i64 {
    let cleaned: String = input
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    if cleaned.is_empty() {
        return 0;
    }
    match cleaned.parse::<f64>() {
        Ok(n) if n >= 0.0 => (n * 100.0).round() as i64,
        _ => 0,
    }
}
