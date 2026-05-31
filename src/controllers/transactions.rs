//! Dashboard + transaction CRUD and the in-app search.
//!
//! Access control is graph-shaped: a user sees a transaction when either
//! (a) their brokerage has an outbound `has_transaction` edge to it, AND
//! (b) for agents, they also have an outbound `owns` edge to it.
//! Brokers and coordinators see every transaction in their brokerage.

// SurrealDB's `RecordId` has interior mutability through its lazily-init
// regex caches, which trips clippy's `mutable_key_type` lint when it's
// used as a `HashMap` / `HashSet` key. Hash + Eq are still deterministic
// (computed from the table + key fields, not the cache state), so the
// lint is a false positive for our usage — silence it module-wide so the
// batched-query helpers can keep building tx-id-keyed maps.
#![allow(clippy::mutable_key_type)]

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
    ChecklistGroup, ChecklistRow, CommentView, CompliancePanel, SearchDocument, SearchPage,
    TransactionEditPage, TransactionNewPage, TransactionRowsFragment, TransactionShowPage,
    TransactionsListPage, UnassignedAssignee, UnassignedPage,
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
    let header = crate::controllers::common::build_app_header(&state, &user, "transactions").await;
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
    let header = crate::controllers::common::build_app_header(&state, &user, "transactions").await;
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
    let tx_id = RecordId::new("transaction", id.as_str());
    let tx = authorize_transaction(&state, &user, &tx_id).await?;

    let items = load_checklist(&state, &tx.id).await?;
    let groups = build_grouped_checklist(&state, items, user.role).await?;
    let owner_name = load_transaction_owner_name(&state, &tx.id).await?;
    let available_forms = available_forms(&groups);
    let transaction_comments = load_comments(&state, &tx.id).await?;

    let header = crate::controllers::common::build_app_header(&state, &user, "transactions").await;
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

    let header = crate::controllers::common::build_app_header(&state, &user, "transactions").await;
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

    let header = crate::controllers::common::build_app_header(&state, &user, "search").await;
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

/// Resolve a transaction record by ID after confirming the caller is
/// allowed to see it. This is the single chokepoint every transaction
/// mutation flows through — call it **before** any read or write that
/// references a tx record id from URL/form input.
///
/// Two layers of access control, both required:
///
/// 1. **Brokerage scope** — the transaction must hang off the user's
///    brokerage via the `has_transaction` edge. A foreign tx id
///    returns [`AppError::NotFound`] (404, not 403) so cross-tenant
///    probes can't enumerate other brokerages' record ids.
/// 2. **Role scope** — agents must additionally have an outbound
///    `owns` edge to the tx; brokers and coordinators
///    ([`Role::sees_all_transactions`]) bypass this check. An agent
///    asking about a teammate's tx gets [`AppError::Forbidden`].
///
/// # Errors
///
/// - `NotFound` — tx doesn't exist, or it does but isn't in this
///   brokerage. Same code so the response leaks nothing.
/// - `Forbidden` — tx exists in this brokerage but the agent doesn't
///   own it (only reachable on the agent path).
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

/// Build the grouped checklist view: bucket items by group, attach
/// per-item documents, and pre-render audit strings.
///
/// Every per-item lookup (documents, reviewer names, comments) used to
/// fire one query per item — fine on small checklists but the dashboard
/// `show` view loads ~30 items so we'd burn ~90 round-trips. This
/// version batches each lookup into a single query and groups the
/// results in Rust.
async fn build_grouped_checklist(
    state: &AppState,
    items: Vec<ChecklistItem>,
    role: crate::auth::Role,
) -> Result<Vec<ChecklistGroup>, AppError> {
    use std::collections::HashMap;

    let item_ids: Vec<RecordId> = items.iter().map(|i| i.id.clone()).collect();

    // Documents: one query for every `for_item` edge across the
    // checklist, then one query for the actual document rows.
    let docs_per_item: HashMap<RecordId, Vec<Document>> = if item_ids.is_empty() {
        HashMap::new()
    } else {
        #[derive(Debug, Deserialize, SurrealValue)]
        struct DocEdge {
            doc: RecordId,
            item: RecordId,
        }
        let mut edge_q = state
            .db
            .query("SELECT in AS doc, out AS item FROM for_item WHERE out IN $items")
            .bind(("items", item_ids.clone()))
            .await?;
        let edges: Vec<DocEdge> = edge_q.take(0).unwrap_or_default();
        let doc_ids: Vec<RecordId> = edges.iter().map(|e| e.doc.clone()).collect();

        let docs_by_id: HashMap<RecordId, Document> = if doc_ids.is_empty() {
            HashMap::new()
        } else {
            let mut doc_q = state
                .db
                .query("SELECT * FROM document WHERE id IN $ids")
                .bind(("ids", doc_ids))
                .await?;
            let docs: Vec<Document> = doc_q.take(0).unwrap_or_default();
            docs.into_iter().map(|d| (d.id.clone(), d)).collect()
        };

        // Group by item id while preserving the original `version DESC,
        // created_at DESC` order via in-place sort in Rust.
        let mut by_item: HashMap<RecordId, Vec<Document>> = HashMap::new();
        for e in edges {
            if let Some(doc) = docs_by_id.get(&e.doc) {
                by_item.entry(e.item).or_default().push(doc.clone());
            }
        }
        for docs in by_item.values_mut() {
            docs.sort_by(|a, b| {
                b.version
                    .cmp(&a.version)
                    .then_with(|| b.created_at.cmp(&a.created_at))
            });
        }
        by_item
    };

    // Reviewer names: one query for every distinct reviewer across the
    // whole checklist (typically 1-2 unique users), then build labels.
    let reviewer_ids: Vec<RecordId> = {
        let mut seen: std::collections::HashSet<RecordId> = std::collections::HashSet::new();
        items
            .iter()
            .filter_map(|i| i.reviewed_by.clone())
            .filter(|id| seen.insert(id.clone()))
            .collect()
    };
    let reviewer_names: HashMap<RecordId, String> = if reviewer_ids.is_empty() {
        HashMap::new()
    } else {
        #[derive(Debug, Deserialize, SurrealValue)]
        struct UserRow {
            id: RecordId,
            name: String,
        }
        let mut u_q = state
            .db
            .query("SELECT id, name FROM user WHERE id IN $ids")
            .bind(("ids", reviewer_ids))
            .await?;
        let rows: Vec<UserRow> = u_q.take(0).unwrap_or_default();
        rows.into_iter().map(|r| (r.id, r.name)).collect()
    };
    let audit_labels: Vec<String> = items
        .iter()
        .map(|item| match (&item.reviewed_by, item.reviewed_at) {
            (Some(uid), Some(when)) => {
                let who = reviewer_names
                    .get(uid)
                    .cloned()
                    .unwrap_or_else(|| "Someone".into());
                let verb = match item.status() {
                    crate::models::ApprovalStatus::Approved => "Approved",
                    crate::models::ApprovalStatus::Denied => "Denied",
                    crate::models::ApprovalStatus::Pending => "Reviewed",
                };
                format!("{verb} by {who} on {}", when.format("%b %-d, %Y"))
            }
            _ => String::new(),
        })
        .collect();

    // Comments: one batched query across every item, then bucket in
    // Rust. Mirrors the projection shape used by `load_comments` so the
    // template gets the same `CommentView` rows.
    let comments_per_item: HashMap<RecordId, Vec<CommentView>> = if item_ids.is_empty() {
        HashMap::new()
    } else {
        load_comments_for_targets(state, &item_ids).await?
    };

    let docs_in_order: Vec<Vec<Document>> = items
        .iter()
        .map(|i| docs_per_item.get(&i.id).cloned().unwrap_or_default())
        .collect();
    let comments_in_order: Vec<Vec<CommentView>> = items
        .iter()
        .map(|i| comments_per_item.get(&i.id).cloned().unwrap_or_default())
        .collect();

    // Bucket rows by the group snapshotted on each item (name + order).
    // Groups are data-driven now, so we discover them from the items
    // rather than a fixed enum, then sort groups by `group_order`.
    let mut buckets: Vec<(i64, String, Vec<ChecklistRow>)> = Vec::new();

    for (((item, docs), audit), comments) in items
        .into_iter()
        .zip(docs_in_order)
        .zip(audit_labels)
        .zip(comments_in_order)
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
        .map(|(order, name, items)| ChecklistGroup::build(name, order, items, role))
        .collect();
    Ok(groups)
}

/// Like [`load_comments`] but for a batch of targets. Returns a map
/// `target → comments` so [`build_grouped_checklist`] can drop the
/// per-item query and replace it with a single round trip.
async fn load_comments_for_targets(
    state: &AppState,
    targets: &[RecordId],
) -> Result<std::collections::HashMap<RecordId, Vec<CommentView>>, AppError> {
    use std::collections::HashMap;

    if targets.is_empty() {
        return Ok(HashMap::new());
    }
    let mut response = state
        .db
        .query(
            "SELECT target, body, created_at, \
                    author.id AS author_id, \
                    author.name AS author_name, \
                    author.avatar_storage_key AS author_avatar_key, \
                    references_document.id AS ref_id, \
                    references_document.filename AS ref_filename, \
                    references_document.version AS ref_version \
             FROM comment WHERE target IN $targets ORDER BY created_at ASC",
        )
        .bind(("targets", targets.to_vec()))
        .await?;
    #[derive(Debug, serde::Deserialize, SurrealValue)]
    struct Row {
        target: RecordId,
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
    let mut by_target: HashMap<RecordId, Vec<CommentView>> = HashMap::new();
    for r in rows {
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
        by_target.entry(r.target).or_default().push(CommentView {
            body: r.body,
            author_initials,
            author_name,
            author_key,
            author_has_avatar: r.author_avatar_key.is_some(),
            created_at: r.created_at,
            referenced_document,
        });
    }
    Ok(by_target)
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
    needs_attention_flags_with(&state.db, transactions, role, brokerage_id).await
}

/// DB-only variant exposed for unit testing — see the `mod tests`
/// block at the bottom of this file. The outer wrapper keeps handler
/// call sites passing the familiar `&AppState`.
async fn needs_attention_flags_with(
    db: &crate::state::Db,
    transactions: &[Transaction],
    role: Role,
    brokerage_id: &RecordId,
) -> Result<Vec<bool>, AppError> {
    use std::collections::{HashMap, HashSet};

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

    // Every visible tx is in scope — even Sold/Canceled/Withdrawn rows
    // can pick up a fresh comment or denial after the deal closed, and
    // the user explicitly wants that to surface.
    let tx_ids: Vec<RecordId> = transactions.iter().map(|t| t.id.clone()).collect();

    // Comment-author roster — one query for the whole brokerage. This
    // is the "other side" — the users whose comments should flag the
    // current viewer.
    let mut au = db
        .query("SELECT VALUE in FROM works_at WHERE out = $b AND role IN $roles")
        .bind(("b", brokerage_id.clone()))
        .bind(("roles", author_roles))
        .await?;
    let other_side_authors: Vec<RecordId> = au.take(0).unwrap_or_default();

    // Item → transaction map. One query gives us every item under every
    // visible tx; we feed the item list straight into the latest-comment
    // query and use the reverse map to attribute item-level comments
    // back to their parent tx. (`in` is renamed via SQL `AS` because
    // SurrealValue derive doesn't honour serde rename attributes.)
    let mut im = db
        .query("SELECT in AS tx, out AS item FROM has_item WHERE in IN $txs")
        .bind(("txs", tx_ids.clone()))
        .await?;
    #[derive(Debug, Deserialize, SurrealValue)]
    struct ItemEdge {
        tx: RecordId,
        item: RecordId,
    }
    let edges: Vec<ItemEdge> = im.take(0)?;
    let mut item_to_tx: HashMap<RecordId, RecordId> = HashMap::with_capacity(edges.len());
    let mut all_items: Vec<RecordId> = Vec::with_capacity(edges.len());
    for e in edges {
        all_items.push(e.item.clone());
        item_to_tx.insert(e.item, e.tx);
    }

    // Form signal — per-tx queries fanned out via `try_join_all` so
    // they run concurrently (wall-clock = slowest single query, not
    // sum). A single batched query against `checklist_item` would be
    // nicer but the `<-for_item<-document` graph traversal only parses
    // at the top of an expression, and `$item.<-…` isn't supported.
    let form_sql_reviewer = "SELECT count() FROM $t->has_item->checklist_item \
                             WHERE approval_status = 'pending' \
                               AND array::len(<-for_item<-document) > 0 GROUP ALL";
    let form_sql_agent = "SELECT count() FROM $t->has_item->checklist_item \
                          WHERE approval_status = 'denied' GROUP ALL";
    let form_sql = if reviewer_view {
        form_sql_reviewer
    } else {
        form_sql_agent
    };

    let form_futures = tx_ids.iter().map(|tx_id| {
        let tx_id = tx_id.clone();
        async move {
            let mut q = db.query(form_sql).bind(("t", tx_id.clone())).await?;
            let row: Option<CountRow> = q.take(0)?;
            Ok::<_, AppError>((tx_id, row.map(|c| c.count > 0).unwrap_or(false)))
        }
    });
    let form_results = futures::future::try_join_all(form_futures).await?;
    let mut flagged: HashSet<RecordId> = form_results
        .into_iter()
        .filter_map(|(t, hit)| hit.then_some(t))
        .collect();

    // Comment signal — only ITEM-LEVEL comments count, and only when
    // the latest comment on that item is from the other side. This
    // models "the ball is in your court":
    //   * Agent posts on an item → reviewers flag (their turn).
    //   * Reviewer replies on the same item → agent flags, reviewers
    //     stop flagging (ball moved back).
    //   * Transaction-target comments are self-notes — they never flag
    //     anyone, on either side.
    if !other_side_authors.is_empty() && !all_items.is_empty() {
        // ORDER BY created_at DESC; the first hit per `target` is the
        // latest comment on that item. SurrealDB requires every field
        // referenced in ORDER BY to appear in the SELECT projection,
        // so `created_at` is pulled even though we never inspect its
        // value in Rust.
        let mut cr = db
            .query(
                "SELECT target, author, created_at FROM comment \
                 WHERE target IN $items \
                 ORDER BY created_at DESC",
            )
            .bind(("items", all_items.clone()))
            .await?;
        #[derive(Debug, Deserialize, SurrealValue)]
        struct CommentRow {
            target: RecordId,
            author: RecordId,
            #[allow(dead_code)]
            created_at: chrono::DateTime<chrono::Utc>,
        }
        let rows: Vec<CommentRow> = cr.take(0).unwrap_or_default();
        let other_side: HashSet<&RecordId> = other_side_authors.iter().collect();
        let mut seen_items: HashSet<RecordId> = HashSet::new();
        for row in &rows {
            // Skip every row that isn't the newest one for its item.
            if !seen_items.insert(row.target.clone()) {
                continue;
            }
            if other_side.contains(&row.author)
                && let Some(tx) = item_to_tx.get(&row.target)
            {
                flagged.insert(tx.clone());
            }
        }
    }

    Ok(transactions
        .iter()
        .map(|t| flagged.contains(&t.id))
        .collect())
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
    if transactions.is_empty() {
        return Ok(Vec::new());
    }

    // One batched query for every visible tx instead of N parallel
    // queries — same wall-clock on a hot connection, far less load
    // when the visible set grows. The `AS` aliases dodge the
    // `SurrealValue` derive's lack of serde-rename support and avoid
    // shadowing the SurrealQL keywords `in` / `out`.
    let tx_ids: Vec<RecordId> = transactions.iter().map(|t| t.id.clone()).collect();
    let mut response = state
        .db
        .query("SELECT in AS tx, out AS doc FROM has_document WHERE in IN $ids")
        .bind(("ids", tx_ids))
        .await?;
    #[derive(Debug, Deserialize, SurrealValue)]
    struct Edge {
        tx: RecordId,
        doc: RecordId,
    }
    let edges: Vec<Edge> = response.take(0).unwrap_or_default();
    if edges.is_empty() {
        return Ok(Vec::new());
    }

    let doc_ids: Vec<RecordId> = edges.iter().map(|e| e.doc.clone()).collect();
    let mut doc_q = state
        .db
        .query("SELECT * FROM document WHERE id IN $ids")
        .bind(("ids", doc_ids))
        .await?;
    let docs: Vec<Document> = doc_q.take(0).unwrap_or_default();
    let docs_by_id: std::collections::HashMap<RecordId, Document> =
        docs.into_iter().map(|d| (d.id.clone(), d)).collect();
    let txs_by_id: std::collections::HashMap<RecordId, &Transaction> =
        transactions.iter().map(|t| (t.id.clone(), t)).collect();

    let matches: Vec<SearchDocument> = edges
        .into_iter()
        .filter_map(|e| {
            let doc = docs_by_id.get(&e.doc)?.clone();
            let tx = txs_by_id.get(&e.tx)?;
            let hit = doc.filename.to_ascii_lowercase().contains(needle)
                || doc.form_code.to_ascii_lowercase().contains(needle);
            hit.then(|| SearchDocument {
                document: doc,
                transaction_key: crate::db::record_key(&tx.id),
                transaction_address: tx.property_address.clone(),
            })
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

// ---------------------------------------------------------------------------
// Reassign — unassigned view + mass-reassign endpoint
// ---------------------------------------------------------------------------

/// GET `/app/transactions/unassigned` — broker view of transactions that
/// have no `owns` edge (typically orphaned by a removed agent). Drives
/// the mass-reassign UI: checkboxes + brokerage-member dropdown +
/// Apply. Coordinators see it read-only; agents are bounced.
pub async fn unassigned_list(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Html<String>, AppError> {
    if !user.role.is_broker() {
        return Err(AppError::Forbidden);
    }

    let mut q = state
        .db
        .query(
            "SELECT * FROM $b->has_transaction->transaction
             WHERE array::len(<-owns<-user) = 0
             ORDER BY created_at DESC",
        )
        .bind(("b", user.brokerage_id.clone()))
        .await?;
    let transactions: Vec<Transaction> = q.take(0)?;

    // Brokerage members go into the dropdown. Brokers + coordinators
    // are valid owners alongside agents — the broker might want to put
    // themselves on a deal as the working agent.
    #[derive(serde::Deserialize, SurrealValue)]
    struct MemberRow {
        user_id: RecordId,
        name: String,
        role: String,
    }
    let mut m_q = state
        .db
        .query(
            "SELECT in AS user_id, in.name AS name, role
             FROM works_at WHERE out = $b ORDER BY in.name ASC",
        )
        .bind(("b", user.brokerage_id.clone()))
        .await?;
    let rows: Vec<MemberRow> = m_q.take(0)?;
    let assignees = rows
        .into_iter()
        .filter_map(|r| {
            Role::parse(&r.role).map(|role| UnassignedAssignee {
                key: crate::db::record_key(&r.user_id),
                name: r.name,
                role_label: role.label().to_string(),
            })
        })
        .collect();

    let header = crate::controllers::common::build_app_header(&state, &user, "transactions").await;

    render(&UnassignedPage {
        app_name: &state.config.app_name,
        base_url: &state.config.base_url,
        signed_in: true,
        header,
        transactions,
        assignees,
    })
}

#[derive(Debug, Deserialize)]
pub struct ReassignInput {
    pub assignee_key: String,
    /// Comma-separated list of transaction keys. The unassigned page
    /// posts every checked row in one shot; per-transaction reassign
    /// from other entry points just sends one key.
    pub tx_keys: String,
}

/// POST `/app/transactions/reassign` — broker-only. Apply a single
/// `assignee` to one or more transactions (typically all the checked
/// rows on the unassigned page). Each transaction's existing `owns`
/// edges are cleared so reassignment is idempotent. Audits per tx.
pub async fn reassign(
    State(state): State<AppState>,
    user: CurrentUser,
    Form(input): Form<ReassignInput>,
) -> Result<Redirect, AppError> {
    if !user.role.is_broker() {
        return Err(AppError::Forbidden);
    }

    let assignee_id = RecordId::new("user", input.assignee_key.trim());

    // Confirm the assignee is in this brokerage — otherwise a broker
    // could hand a transaction to a user from a different tenant.
    let mut member_q = state
        .db
        .query("SELECT VALUE id FROM works_at WHERE in = $u AND out = $b LIMIT 1")
        .bind(("u", assignee_id.clone()))
        .bind(("b", user.brokerage_id.clone()))
        .await?;
    let member: Vec<RecordId> = member_q.take(0).unwrap_or_default();
    if member.is_empty() {
        return Err(AppError::invalid(
            "That assignee isn't a member of your brokerage.",
        ));
    }

    let tx_keys: Vec<String> = input
        .tx_keys
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if tx_keys.is_empty() {
        return Err(AppError::invalid(
            "Select at least one transaction to reassign.",
        ));
    }

    for key in tx_keys {
        let tx_id = RecordId::new("transaction", key.as_str());

        // Tenant check — has_transaction edge from THIS brokerage.
        let mut ok_q = state
            .db
            .query("SELECT count() FROM has_transaction WHERE in = $b AND out = $t GROUP ALL")
            .bind(("b", user.brokerage_id.clone()))
            .bind(("t", tx_id.clone()))
            .await?;
        #[derive(serde::Deserialize, SurrealValue)]
        struct CountRow {
            count: i64,
        }
        let cnt: Option<CountRow> = ok_q.take(0)?;
        if cnt.map(|c| c.count).unwrap_or(0) == 0 {
            // Silently skip cross-tenant attempts — same as authorize.
            continue;
        }

        // Replace any existing owns edges so the result is "exactly
        // one owner = the new assignee", regardless of prior state.
        state
            .db
            .query("DELETE owns WHERE out = $t")
            .bind(("t", tx_id.clone()))
            .await?;
        state
            .db
            .query("RELATE $u->owns->$t")
            .bind(("u", assignee_id.clone()))
            .bind(("t", tx_id.clone()))
            .await?;

        crate::audit::record(
            &state.db,
            "transaction_reassigned",
            Some(user.user_id.clone()),
            Some(user.email.clone()),
            None,
            None,
            Some(format!("tx={} → user={}", key, input.assignee_key)),
        )
        .await;
    }

    Ok(Redirect::to("/app/transactions/unassigned"))
}

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

#[cfg(test)]
mod tests {
    //! Coverage for the dashboard's "Needs attention" predicate. The
    //! tests spin up an in-memory SurrealDB, apply the real schema, and
    //! drive [`needs_attention_flags_with`] directly so we don't have
    //! to stand up an `AppState` (storage / stripe / mailer) just to
    //! exercise pure DB logic.
    use super::*;
    use crate::auth::Role;
    use surrealdb::types::SurrealValue;

    async fn make_db() -> crate::state::Db {
        let db = surrealdb::engine::any::connect("mem://")
            .await
            .expect("mem connect");
        db.use_ns("test").use_db("test").await.expect("use ns/db");
        crate::db::apply_schema(&db).await.expect("apply schema");
        db
    }

    #[derive(Debug, serde::Serialize, SurrealValue)]
    struct NewBrokerage {
        name: String,
        plan: String,
        is_complimentary: bool,
    }

    async fn insert_brokerage(db: &crate::state::Db) -> RecordId {
        let b: Option<crate::models::Brokerage> = db
            .create("brokerage")
            .content(NewBrokerage {
                name: "TestCo".into(),
                plan: "starter".into(),
                is_complimentary: true,
            })
            .await
            .expect("create brokerage");
        b.expect("brokerage").id
    }

    #[derive(Debug, serde::Serialize, SurrealValue)]
    struct NewUser {
        email: String,
        name: String,
        password_hash: String,
        email_verified: bool,
    }

    async fn insert_user(db: &crate::state::Db, email: &str) -> RecordId {
        let u: Option<crate::models::User> = db
            .create("user")
            .content(NewUser {
                email: email.into(),
                name: email.into(),
                password_hash: "x".into(),
                email_verified: true,
            })
            .await
            .expect("create user");
        u.expect("user").id
    }

    async fn put_user_in_brokerage(
        db: &crate::state::Db,
        user: &RecordId,
        brokerage: &RecordId,
        role: &str,
    ) {
        db.query("RELATE $u->works_at->$b SET role = $r")
            .bind(("u", user.clone()))
            .bind(("b", brokerage.clone()))
            .bind(("r", role.to_string()))
            .await
            .expect("RELATE works_at");
    }

    #[derive(Debug, serde::Serialize, SurrealValue)]
    struct NewTx {
        property_address: String,
        city: String,
        apn: Option<String>,
        postal_code: Option<String>,
        price_cents: i64,
        client_name: Option<String>,
        mls_number: Option<String>,
        office_file_number: Option<String>,
        status: String,
        transaction_type: String,
        special_sales_condition: String,
        sales_type: String,
    }

    async fn insert_tx(db: &crate::state::Db, brokerage: &RecordId, status: &str) -> Transaction {
        let tx: Option<Transaction> = db
            .create("transaction")
            .content(NewTx {
                property_address: format!("addr-{status}"),
                city: "LA".into(),
                apn: None,
                postal_code: None,
                price_cents: 1,
                client_name: None,
                mls_number: None,
                office_file_number: None,
                status: status.into(),
                transaction_type: "residential".into(),
                special_sales_condition: "none".into(),
                sales_type: "listing".into(),
            })
            .await
            .expect("create tx");
        let tx = tx.expect("tx row");
        db.query("RELATE $b->has_transaction->$t")
            .bind(("b", brokerage.clone()))
            .bind(("t", tx.id.clone()))
            .await
            .expect("RELATE has_transaction");
        tx
    }

    #[derive(Debug, serde::Serialize, SurrealValue)]
    struct NewItem {
        title: String,
        form_code: Option<String>,
        group_name: String,
        group_order: i64,
        position: i64,
        required: bool,
        approval_status: String,
    }

    async fn insert_item(
        db: &crate::state::Db,
        tx_id: &RecordId,
        approval_status: &str,
    ) -> RecordId {
        let it: Option<crate::models::ChecklistItem> = db
            .create("checklist_item")
            .content(NewItem {
                title: "Test item".into(),
                form_code: None,
                group_name: "Test".into(),
                group_order: 1,
                position: 1,
                required: true,
                approval_status: approval_status.into(),
            })
            .await
            .expect("create item");
        let id = it.expect("item").id;
        db.query("RELATE $t->has_item->$i")
            .bind(("t", tx_id.clone()))
            .bind(("i", id.clone()))
            .await
            .expect("RELATE has_item");
        id
    }

    /// Attach a real document edge so "pending-with-upload" predicate
    /// fires for reviewer view.
    async fn attach_document(db: &crate::state::Db, item_id: &RecordId) {
        #[derive(Debug, serde::Serialize, SurrealValue)]
        struct NewDoc {
            filename: String,
            form_code: String,
            content_type: String,
            storage_key: String,
            size_bytes: i64,
            version: i64,
        }
        let doc: Option<crate::models::Document> = db
            .create("document")
            .content(NewDoc {
                filename: "t.pdf".into(),
                form_code: "MISC".into(),
                content_type: "application/pdf".into(),
                storage_key: "k".into(),
                size_bytes: 1,
                version: 1,
            })
            .await
            .expect("create doc");
        let id = doc.expect("doc").id;
        db.query("RELATE $d->for_item->$i")
            .bind(("d", id))
            .bind(("i", item_id.clone()))
            .await
            .expect("RELATE for_item");
    }

    #[derive(Debug, serde::Serialize, SurrealValue)]
    struct NewComment {
        body: String,
        target: RecordId,
        author: RecordId,
    }

    async fn add_comment(db: &crate::state::Db, target: &RecordId, author: &RecordId) {
        let _: Option<crate::models::Comment> = db
            .create("comment")
            .content(NewComment {
                body: "note".into(),
                target: target.clone(),
                author: author.clone(),
            })
            .await
            .expect("create comment");
    }

    // ---- empty input ----

    #[tokio::test]
    async fn empty_input_returns_empty() {
        let db = make_db().await;
        let b = insert_brokerage(&db).await;
        let flags = needs_attention_flags_with(&db, &[], Role::Broker, &b)
            .await
            .expect("flags");
        assert!(flags.is_empty());
    }

    // ---- closed statuses CAN still flag ----

    #[tokio::test]
    async fn closed_statuses_flag_on_new_activity() {
        // Even after a deal closes, late activity (a denial that the
        // agent still needs to fix, a comment from the reviewer)
        // should bubble up — the "ball in your court" rule isn't
        // gated on lifecycle status.
        let db = make_db().await;
        let b = insert_brokerage(&db).await;
        let sold = insert_tx(&db, &b, "sold").await;
        let canceled = insert_tx(&db, &b, "canceled").await;
        let withdrawn = insert_tx(&db, &b, "withdrawn").await;
        for tx in [&sold, &canceled, &withdrawn] {
            insert_item(&db, &tx.id, "denied").await;
        }
        let flags = needs_attention_flags_with(
            &db,
            &[sold.clone(), canceled.clone(), withdrawn.clone()],
            Role::Agent,
            &b,
        )
        .await
        .expect("flags");
        assert_eq!(flags, vec![true, true, true]);
    }

    // ---- agent view ----

    #[tokio::test]
    async fn agent_view_flags_on_denied_item() {
        let db = make_db().await;
        let b = insert_brokerage(&db).await;
        let tx = insert_tx(&db, &b, "active").await;
        insert_item(&db, &tx.id, "denied").await;
        let flags = needs_attention_flags_with(&db, &[tx], Role::Agent, &b)
            .await
            .expect("flags");
        assert_eq!(flags, vec![true]);
    }

    #[tokio::test]
    async fn agent_view_flags_on_reviewer_item_comment() {
        // Reviewer drops a comment on an item — that's a "ball in
        // your court" handoff for the agent.
        let db = make_db().await;
        let b = insert_brokerage(&db).await;
        let tx = insert_tx(&db, &b, "active").await;
        let item = insert_item(&db, &tx.id, "pending").await;
        let broker = insert_user(&db, "broker@x.com").await;
        put_user_in_brokerage(&db, &broker, &b, "broker").await;
        add_comment(&db, &item, &broker).await;
        let flags = needs_attention_flags_with(&db, &[tx], Role::Agent, &b)
            .await
            .expect("flags");
        assert_eq!(flags, vec![true]);
    }

    #[tokio::test]
    async fn agent_view_ignores_self_authored_comments() {
        // An agent's own comment shouldn't make their own row flag.
        let db = make_db().await;
        let b = insert_brokerage(&db).await;
        let tx = insert_tx(&db, &b, "active").await;
        let item = insert_item(&db, &tx.id, "pending").await;
        let agent = insert_user(&db, "agent@x.com").await;
        put_user_in_brokerage(&db, &agent, &b, "agent").await;
        add_comment(&db, &item, &agent).await;
        let flags = needs_attention_flags_with(&db, &[tx], Role::Agent, &b)
            .await
            .expect("flags");
        assert_eq!(flags, vec![false]);
    }

    #[tokio::test]
    async fn transaction_target_comments_never_flag() {
        // Comments on a transaction itself are "self-notes" — they
        // don't bubble up to the other side's needs-attention list,
        // even when the author is the opposite role.
        let db = make_db().await;
        let b = insert_brokerage(&db).await;
        let tx = insert_tx(&db, &b, "active").await;
        let broker = insert_user(&db, "broker@x.com").await;
        put_user_in_brokerage(&db, &broker, &b, "broker").await;
        let agent = insert_user(&db, "agent@x.com").await;
        put_user_in_brokerage(&db, &agent, &b, "agent").await;
        // Reviewer leaves a note on the transaction itself.
        add_comment(&db, &tx.id, &broker).await;
        // Neither side should be flagged by a tx-target comment.
        let agent_flags =
            needs_attention_flags_with(&db, std::slice::from_ref(&tx), Role::Agent, &b)
                .await
                .expect("flags");
        let reviewer_flags = needs_attention_flags_with(&db, &[tx], Role::Broker, &b)
            .await
            .expect("flags");
        assert_eq!(agent_flags, vec![false]);
        assert_eq!(reviewer_flags, vec![false]);
    }

    #[tokio::test]
    async fn latest_comment_rule_handoff() {
        // The "ball in your court" rule: only the LATEST comment on an
        // item determines who flags.
        //   1. Agent comments → reviewer flags.
        //   2. Reviewer comments back → agent flags, reviewer clear.
        let db = make_db().await;
        let b = insert_brokerage(&db).await;
        let tx = insert_tx(&db, &b, "active").await;
        let item = insert_item(&db, &tx.id, "pending").await;
        let agent = insert_user(&db, "agent@x.com").await;
        put_user_in_brokerage(&db, &agent, &b, "agent").await;
        let broker = insert_user(&db, "broker@x.com").await;
        put_user_in_brokerage(&db, &broker, &b, "broker").await;

        // Step 1: agent posts first — reviewer should flag.
        add_comment(&db, &item, &agent).await;
        let reviewer_flags =
            needs_attention_flags_with(&db, std::slice::from_ref(&tx), Role::Broker, &b)
                .await
                .expect("flags");
        assert_eq!(
            reviewer_flags,
            vec![true],
            "reviewer should flag after agent posts"
        );
        let agent_flags =
            needs_attention_flags_with(&db, std::slice::from_ref(&tx), Role::Agent, &b)
                .await
                .expect("flags");
        assert_eq!(
            agent_flags,
            vec![false],
            "agent should not flag on own comment"
        );

        // SurrealDB's `time::now()` resolves at insert time; insert a
        // small delay so the reviewer's comment sorts strictly later
        // than the agent's. (SurrealKV's mem engine has microsecond
        // resolution; a 10ms sleep is generous.)
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        // Step 2: reviewer replies — handoff completes.
        add_comment(&db, &item, &broker).await;
        let reviewer_flags =
            needs_attention_flags_with(&db, std::slice::from_ref(&tx), Role::Broker, &b)
                .await
                .expect("flags");
        assert_eq!(
            reviewer_flags,
            vec![false],
            "reviewer should clear after replying"
        );
        let agent_flags = needs_attention_flags_with(&db, &[tx], Role::Agent, &b)
            .await
            .expect("flags");
        assert_eq!(
            agent_flags,
            vec![true],
            "agent should flag after reviewer replies"
        );
    }

    // ---- reviewer view ----

    #[tokio::test]
    async fn reviewer_view_flags_on_pending_with_upload() {
        let db = make_db().await;
        let b = insert_brokerage(&db).await;
        let tx = insert_tx(&db, &b, "active").await;
        let item = insert_item(&db, &tx.id, "pending").await;
        attach_document(&db, &item).await;
        let flags = needs_attention_flags_with(&db, &[tx], Role::Broker, &b)
            .await
            .expect("flags");
        assert_eq!(flags, vec![true]);
    }

    #[tokio::test]
    async fn reviewer_view_does_not_flag_pending_without_upload() {
        let db = make_db().await;
        let b = insert_brokerage(&db).await;
        let tx = insert_tx(&db, &b, "active").await;
        insert_item(&db, &tx.id, "pending").await; // no document attached
        let flags = needs_attention_flags_with(&db, &[tx], Role::Broker, &b)
            .await
            .expect("flags");
        assert_eq!(flags, vec![false]);
    }

    #[tokio::test]
    async fn reviewer_view_flags_on_agent_comment_on_item() {
        let db = make_db().await;
        let b = insert_brokerage(&db).await;
        let tx = insert_tx(&db, &b, "active").await;
        let item = insert_item(&db, &tx.id, "pending").await;
        let agent = insert_user(&db, "agent@x.com").await;
        put_user_in_brokerage(&db, &agent, &b, "agent").await;
        add_comment(&db, &item, &agent).await;
        let flags = needs_attention_flags_with(&db, &[tx], Role::Broker, &b)
            .await
            .expect("flags");
        assert_eq!(flags, vec![true]);
    }

    #[tokio::test]
    async fn no_signals_no_flag() {
        let db = make_db().await;
        let b = insert_brokerage(&db).await;
        let tx = insert_tx(&db, &b, "active").await;
        insert_item(&db, &tx.id, "approved").await;
        let flags = needs_attention_flags_with(&db, &[tx], Role::Broker, &b)
            .await
            .expect("flags");
        assert_eq!(flags, vec![false]);
    }
}
