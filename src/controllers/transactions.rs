//! Dashboard + transaction CRUD and the in-app search.
//!
//! Access control is graph-shaped: a user sees a transaction when either
//! (a) their brokerage has an outbound `has_transaction` edge to it, AND
//! (b) for agents, they also have an outbound `owns` edge to it.
//! Brokers and coordinators see every transaction in their brokerage.

use axum::Form;
use axum::extract::{Path, Query, State};
use axum::response::{Html, IntoResponse, Redirect, Response};
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use serde::Deserialize;
use surrealdb::types::{RecordId, SurrealValue};

use crate::auth::CurrentUser;
use crate::controllers::render;
use crate::error::AppError;
use crate::models::{
    Brokerage, ChecklistItem, DEFAULT_CHECKLIST, NewChecklistItem, NewTransaction, Transaction,
};
use crate::state::AppState;
use crate::templates::{
    AppHeader, DashboardPage, SearchDocument, SearchPage, TransactionNewPage, TransactionsListPage,
    TransactionShowPage, CompliancePanel, DocumentGroup,
};

// ---------------------------------------------------------------------------
// Dashboard
// ---------------------------------------------------------------------------

pub async fn dashboard(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Html<String>, AppError> {
    let brokerage = load_brokerage(&state, &user).await?;
    let transactions = load_visible_transactions(&state, &user).await?;

    let total = transactions.len();
    let open_count = transactions
        .iter()
        .filter(|t| t.status == "open" || t.status == "under_contract")
        .count();
    let complete_count = transactions.iter().filter(|t| t.status == "closed").count();

    // "Needs attention" = still-open transactions whose checklist is < 100%.
    let needs_attention = count_needs_attention(&state, &transactions).await?;

    let recent: Vec<Transaction> = transactions.iter().take(6).cloned().collect();

    let header = AppHeader::new(
        &user.name,
        &user.email,
        user.role,
        &brokerage.name,
        "dashboard",
    );
    render(&DashboardPage {
        app_name: &state.config.app_name,
        base_url: &state.config.base_url,
        signed_in: true,
        header,
        total,
        open_count,
        needs_attention,
        complete_count,
        recent,
    })
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
}

pub async fn list(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(filters): Query<ListFilters>,
) -> Result<Html<String>, AppError> {
    let brokerage = load_brokerage(&state, &user).await?;
    let mut transactions = load_visible_transactions(&state, &user).await?;

    let status_filter = filters.status.unwrap_or_default();
    if !status_filter.is_empty() && status_filter != "all" {
        transactions.retain(|t| t.status == status_filter);
    }

    let query = filters.q.unwrap_or_default();
    if !query.trim().is_empty() {
        let needle = query.to_ascii_lowercase();
        transactions.retain(|t| {
            t.property_address.to_ascii_lowercase().contains(&needle)
                || t.city.to_ascii_lowercase().contains(&needle)
                || t.buyer_name
                    .as_deref()
                    .map(|s| s.to_ascii_lowercase().contains(&needle))
                    .unwrap_or(false)
                || t.seller_name
                    .as_deref()
                    .map(|s| s.to_ascii_lowercase().contains(&needle))
                    .unwrap_or(false)
        });
    }

    let header = AppHeader::new(
        &user.name,
        &user.email,
        user.role,
        &brokerage.name,
        "transactions",
    );
    render(&TransactionsListPage {
        app_name: &state.config.app_name,
        base_url: &state.config.base_url,
        signed_in: true,
        header,
        transactions,
        filter_status: &status_filter,
        query: &query,
    })
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
    );
    render(&TransactionNewPage {
        app_name: &state.config.app_name,
        base_url: &state.config.base_url,
        signed_in: true,
        header,
        error: None,
    })
}

#[derive(Debug, Deserialize)]
pub struct CreateInput {
    pub property_address: String,
    #[serde(default)]
    pub city: Option<String>,
    #[serde(default)]
    pub postal_code: Option<String>,
    #[serde(default)]
    pub price: Option<String>,
    #[serde(default)]
    pub buyer_name: Option<String>,
    #[serde(default)]
    pub seller_name: Option<String>,
    #[serde(default)]
    pub expected_close: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
}

pub async fn create(
    State(state): State<AppState>,
    user: CurrentUser,
    Form(input): Form<CreateInput>,
) -> Result<Redirect, AppError> {
    let property_address = input.property_address.trim().to_string();
    if property_address.is_empty() {
        return Err(AppError::invalid("Property address is required."));
    }

    let price_cents = parse_price_cents(input.price.as_deref().unwrap_or(""));
    let expected_close = parse_date(input.expected_close.as_deref().unwrap_or(""));

    let new_tx = NewTransaction {
        property_address,
        city: input.city.unwrap_or_default().trim().to_string(),
        state: "CA".into(),
        postal_code: input.postal_code.map(|p| p.trim().to_string()).filter(|p| !p.is_empty()),
        price_cents,
        buyer_name: input.buyer_name.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()),
        seller_name: input.seller_name.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()),
        expected_close,
        status: input.status.unwrap_or_else(|| "open".into()),
    };

    let tx: Option<Transaction> = state.db.create("transaction").content(new_tx).await?;
    let tx = tx.ok_or_else(|| AppError::Internal(anyhow::anyhow!("create returned nothing")))?;

    // Wire the graph: brokerage -> has_transaction -> transaction;
    //                 user -> owns -> transaction.
    state
        .db
        .query("RELATE $b->has_transaction->$t; RELATE $u->owns->$t;")
        .bind(("b", user.brokerage_id.clone()))
        .bind(("t", tx.id.clone()))
        .bind(("u", user.user_id.clone()))
        .await?;

    // Seed the default checklist as nodes + `has_item` edges.
    let checklist_ids: Vec<RecordId> = create_default_checklist(&state, &tx.id).await?;
    if !checklist_ids.is_empty() {
        state
            .db
            .query("FOR $cid IN $items { RELATE $t->has_item->$cid }")
            .bind(("t", tx.id.clone()))
            .bind(("items", checklist_ids))
            .await?;
    }

    let key = crate::record_key(&tx.id);
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
    let audit_labels = build_audit_labels(&state, &items).await?;
    let documents = load_document_groups(&state, &tx.id).await?;
    let owner_name = load_transaction_owner_name(&state, &tx.id).await?;

    let header = AppHeader::new(
        &user.name,
        &user.email,
        user.role,
        &brokerage.name,
        "transactions",
    );
    let tx_key = crate::record_key(&tx.id);
    render(&TransactionShowPage {
        app_name: &state.config.app_name,
        base_url: &state.config.base_url,
        signed_in: true,
        header,
        compliance: CompliancePanel::build(items, audit_labels, tx_key.clone()),
        documents,
        owner_name,
        transaction_key: tx_key,
        transaction: tx,
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

    match input.status.as_str() {
        "open" | "under_contract" | "closed" | "cancelled" => {}
        _ => return Err(AppError::invalid("Unknown status")),
    }

    state
        .db
        .query("UPDATE $t SET status = $s")
        .bind(("t", tx_id.clone()))
        .bind(("s", input.status))
        .await?;

    Ok(Redirect::to(&format!("/app/transactions/{id}")).into_response())
}

// ---------------------------------------------------------------------------
// Search
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct SearchInput {
    #[serde(default)]
    pub q: Option<String>,
}

pub async fn search(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(input): Query<SearchInput>,
) -> Result<Html<String>, AppError> {
    let brokerage = load_brokerage(&state, &user).await?;
    let query = input.q.unwrap_or_default();
    let needle = query.trim().to_ascii_lowercase();

    let (transactions, documents) = if needle.is_empty() {
        (Vec::new(), Vec::new())
    } else {
        let all = load_visible_transactions(&state, &user).await?;
        let filtered: Vec<Transaction> = all
            .iter()
            .filter(|t| {
                t.property_address.to_ascii_lowercase().contains(&needle)
                    || t.city.to_ascii_lowercase().contains(&needle)
            })
            .cloned()
            .collect();

        let docs = search_documents(&state, &all, &needle).await?;
        (filtered, docs)
    };

    let header = AppHeader::new(
        &user.name,
        &user.email,
        user.role,
        &brokerage.name,
        "search",
    );
    render(&SearchPage {
        app_name: &state.config.app_name,
        base_url: &state.config.base_url,
        signed_in: true,
        header,
        query: &query,
        transactions,
        documents,
    })
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

pub(crate) async fn load_visible_transactions(
    state: &AppState,
    user: &CurrentUser,
) -> Result<Vec<Transaction>, AppError> {
    // Brokers + coordinators get every transaction in the brokerage.
    // Agents see only the ones they personally own.
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

/// Authorize the caller against a specific transaction and return it.
pub(crate) async fn authorize_transaction(
    state: &AppState,
    user: &CurrentUser,
    tx_id: &RecordId,
) -> Result<Transaction, AppError> {
    let tx: Option<Transaction> = state.db.select(tx_id.clone()).await?;
    let tx = tx.ok_or(AppError::NotFound)?;

    // Is it in my brokerage?
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
        // Agents must also own the transaction.
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

/// Pre-build audit trail labels so the template doesn't need async lookups.
async fn build_audit_labels(
    state: &AppState,
    items: &[ChecklistItem],
) -> Result<Vec<String>, AppError> {
    let labels = futures::future::try_join_all(items.iter().map(|item| async {
        match (&item.completed_by, item.completed_at) {
            (Some(user_id), Some(when)) => {
                let profile: Option<NameOnly> =
                    state.db.select(user_id.clone()).await.map_err(AppError::from)?;
                let who = profile.map(|p| p.name).unwrap_or_else(|| "Someone".into());
                Ok::<_, AppError>(format!("Completed by {who} on {}", when.format("%b %-d, %Y")))
            }
            _ => Ok::<_, AppError>(String::new()),
        }
    }))
    .await?;
    Ok(labels)
}

#[derive(Debug, serde::Deserialize, SurrealValue)]
struct NameOnly {
    name: String,
}

async fn load_document_groups(
    state: &AppState,
    tx_id: &RecordId,
) -> Result<Vec<DocumentGroup>, AppError> {
    use crate::models::Document;

    let mut response = state
        .db
        .query("SELECT * FROM $t->has_document->document ORDER BY category, created_at DESC")
        .bind(("t", tx_id.clone()))
        .await?;
    let documents: Vec<Document> = response.take(0)?;

    // Group by category in presentation order.
    const ORDER: &[(&str, &str)] = &[
        ("contract", "Contract"),
        ("disclosures", "Disclosures"),
        ("inspection", "Inspection"),
        ("appraisal", "Appraisal"),
        ("title", "Title"),
        ("closing", "Closing"),
        ("general", "General"),
    ];

    let groups = ORDER
        .iter()
        .filter_map(|(slug, label)| {
            let docs: Vec<Document> = documents
                .iter()
                .filter(|d| d.category == *slug)
                .cloned()
                .collect();
            if docs.is_empty() {
                None
            } else {
                Some(DocumentGroup {
                    category: (*slug).into(),
                    label: (*label).into(),
                    documents: docs,
                })
            }
        })
        .collect();

    Ok(groups)
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
    Ok(names.into_iter().next().unwrap_or_else(|| "Unassigned".into()))
}

async fn count_needs_attention(
    state: &AppState,
    transactions: &[Transaction],
) -> Result<usize, AppError> {
    let futures = transactions.iter().filter(|t| t.status != "closed" && t.status != "cancelled").map(|t| async move {
        let mut r = state
            .db
            .query(
                "SELECT count() FROM $t->has_item->checklist_item WHERE completed = false GROUP ALL",
            )
            .bind(("t", t.id.clone()))
            .await?;
        let incomplete: Option<CountRow> = r.take(0)?;
        Ok::<_, AppError>(incomplete.map(|c| c.count > 0).unwrap_or(false))
    });
    let results = futures::future::try_join_all(futures).await?;
    Ok(results.into_iter().filter(|&b| b).count())
}

async fn search_documents(
    state: &AppState,
    transactions: &[Transaction],
    needle: &str,
) -> Result<Vec<SearchDocument>, AppError> {
    use crate::models::Document;

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
                .filter(|d| d.filename.to_ascii_lowercase().contains(needle))
                .map(move |d| SearchDocument {
                    document: d,
                    transaction_key: crate::record_key(&tx.id),
                    transaction_address: tx.property_address.clone(),
                })
                .collect::<Vec<_>>()
        })
        .collect();
    Ok(matches)
}

async fn create_default_checklist(
    state: &AppState,
    _tx: &RecordId,
) -> Result<Vec<RecordId>, AppError> {
    let futures = DEFAULT_CHECKLIST
        .iter()
        .enumerate()
        .map(|(position, (title, category))| async move {
            let item: Option<ChecklistItem> = state
                .db
                .create("checklist_item")
                .content(NewChecklistItem {
                    title: (*title).into(),
                    category: (*category).into(),
                    position: position as i64,
                })
                .await?;
            let id = item
                .map(|i| i.id)
                .ok_or_else(|| AppError::Internal(anyhow::anyhow!("checklist item insert returned nothing")))?;
            Ok::<_, AppError>(id)
        });
    let ids = futures::future::try_join_all(futures).await?;
    Ok(ids)
}

// ---------------------------------------------------------------------------
// Local helpers
// ---------------------------------------------------------------------------

/// Accept inputs like `$649,000`, `649000`, `649000.00`. Returns 0 on empty.
fn parse_price_cents(input: &str) -> i64 {
    let cleaned: String = input.chars().filter(|c| c.is_ascii_digit() || *c == '.').collect();
    if cleaned.is_empty() {
        return 0;
    }
    match cleaned.parse::<f64>() {
        Ok(n) if n >= 0.0 => (n * 100.0).round() as i64,
        _ => 0,
    }
}

fn parse_date(input: &str) -> Option<DateTime<Utc>> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    NaiveDate::parse_from_str(trimmed, "%Y-%m-%d")
        .ok()
        .and_then(|d| d.and_hms_opt(12, 0, 0))
        .map(|naive| Utc.from_utc_datetime(&naive))
}
