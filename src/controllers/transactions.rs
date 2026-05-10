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

use crate::auth::CurrentUser;
use crate::controllers::render;
use crate::error::AppError;
use crate::forms::{self, FormGroup};
use crate::models::{
    Brokerage, ChecklistItem, Document, NewChecklistItem, NewTransaction, SalesType,
    SpecialSalesCondition, Transaction, TransactionStatus, TransactionType,
};
use crate::state::AppState;
use crate::templates::{
    AppHeader, ChecklistGroup, ChecklistRow, CommentView, CompliancePanel, DashboardPage,
    SearchDocument, SearchPage, TransactionEditPage, TransactionNewPage, TransactionShowPage,
    TransactionsListPage,
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
        .filter(|t| {
            matches!(
                t.status_enum(),
                TransactionStatus::Active | TransactionStatus::Pending
            )
        })
        .count();
    let complete_count = transactions
        .iter()
        .filter(|t| matches!(t.status_enum(), TransactionStatus::Sold))
        .count();

    let needs_attention = count_needs_attention(&state, &transactions).await?;

    let recent: Vec<Transaction> = transactions.iter().take(6).cloned().collect();

    let header = AppHeader::new(
        &user.name,
        &user.email,
        user.role,
        &brokerage.name,
        "dashboard",
    )
    .with_super_admin(crate::controllers::is_super_admin(&state, &user));
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

    let header = AppHeader::new(
        &user.name,
        &user.email,
        user.role,
        &brokerage.name,
        "transactions",
    )
    .with_super_admin(crate::controllers::is_super_admin(&state, &user));
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
    )
    .with_super_admin(crate::controllers::is_super_admin(&state, &user));
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
    let property_address = input.property_address.trim().to_string();
    if property_address.is_empty() {
        return Err(AppError::invalid("Property address is required."));
    }

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

    let new_tx = NewTransaction {
        property_address,
        city: input.city.unwrap_or_default().trim().to_string(),
        apn: input
            .apn
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
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

    seed_default_checklist(&state, &tx.id, tx_type, condition, sales).await?;

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
    .with_super_admin(crate::controllers::is_super_admin(&state, &user));
    let tx_key = crate::record_key(&tx.id);
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

    let dropdowns_locked = any_item_reviewed(&state, &tx.id).await?;

    let header = AppHeader::new(
        &user.name,
        &user.email,
        user.role,
        &brokerage.name,
        "transactions",
    )
    .with_super_admin(crate::controllers::is_super_admin(&state, &user));
    let tx_key = crate::record_key(&tx.id);
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

    let property_address = input.property_address.trim().to_string();
    if property_address.is_empty() {
        return Err(AppError::invalid("Property address is required."));
    }

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
        .bind((
            "city",
            input.city.unwrap_or_default().trim().to_string(),
        ))
        .bind((
            "apn",
            input
                .apn
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
        ))
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
    let additional_slug = FormGroup::AdditionalDisclosures.slug().to_string();

    // First pass: update or relocate existing items.
    for item in &items {
        let Some(code) = item.form_code.as_deref() else {
            continue;
        };
        match defaults_by_code.get(code) {
            Some(target) => {
                let target_slug = target.group.slug().to_string();
                if target_slug != item.group_slug || target.required != item.required {
                    state
                        .db
                        .query("UPDATE $i SET group_slug = $g, required = $r")
                        .bind(("i", item.id.clone()))
                        .bind(("g", target_slug))
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
                    if item.group_slug != additional_slug || item.required {
                        state
                            .db
                            .query("UPDATE $i SET group_slug = $g, required = false")
                            .bind(("i", item.id.clone()))
                            .bind(("g", additional_slug.clone()))
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
            async move {
                let new_item: Option<ChecklistItem> = state
                    .db
                    .create("checklist_item")
                    .content(NewChecklistItem {
                        title,
                        form_code: Some(d.code.to_string()),
                        group_slug: d.group.slug().to_string(),
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
                    || t.client_name
                        .as_deref()
                        .map(|s| s.to_ascii_lowercase().contains(&needle))
                        .unwrap_or(false)
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
    )
    .with_super_admin(crate::controllers::is_super_admin(&state, &user));
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

    // Bucket rows into groups, in the canonical render order
    // (FormGroup::ORDERED matches the section order in the printed CAR
    // checklists). Items inside each bucket are sorted by their form code's
    // canonical PDF position, falling back to created_at for custom items.
    let mut buckets: Vec<(FormGroup, Vec<ChecklistRow>)> = FormGroup::ORDERED
        .iter()
        .map(|g| (*g, Vec::new()))
        .collect();

    for (((item, docs), audit), comments) in items
        .into_iter()
        .zip(docs_per_item)
        .zip(audit_labels)
        .zip(comments_per_item)
    {
        let group = FormGroup::parse(&item.group_slug).unwrap_or(FormGroup::AdditionalDisclosures);
        let form = item.form_code.as_deref().and_then(forms::lookup);
        let row = ChecklistRow {
            item,
            form,
            audit_label: audit,
            documents: docs,
            comments,
        };
        if let Some((_, bucket)) = buckets.iter_mut().find(|(g, _)| *g == group) {
            bucket.push(row);
        }
    }

    // Sort each bucket by canonical PDF order. Items with a known CAR form
    // code use `forms::canonical_position`; custom (form_code == None) items
    // sort to the end of their group, ordered by creation time.
    for (_, bucket) in buckets.iter_mut() {
        bucket.sort_by_key(|row| {
            let primary = row
                .item
                .form_code
                .as_deref()
                .map(forms::canonical_position)
                .unwrap_or(u32::MAX);
            (primary, row.item.created_at)
        });
    }

    // Drop empty groups so the page doesn't render hollow sections.
    let groups = buckets
        .into_iter()
        .filter(|(_, items)| !items.is_empty())
        .map(|(g, items)| ChecklistGroup::build(g, items))
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
            "SELECT body, created_at, author.name AS author_name, \
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
        author_name: Option<String>,
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
                        key: crate::record_key(&id),
                        filename,
                        version,
                    })
                }
                _ => None,
            };
            CommentView {
                body: r.body,
                author_name: r.author_name.unwrap_or_else(|| "Someone".into()),
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
fn available_forms(groups: &[ChecklistGroup]) -> Vec<&'static crate::forms::CarForm> {
    let used: std::collections::HashSet<&str> = groups
        .iter()
        .flat_map(|g| g.items.iter())
        .filter_map(|r| r.form.map(|f| f.code))
        .collect();
    forms::LIBRARY
        .iter()
        .filter(|f| f.allows_multiple || !used.contains(f.code))
        .collect()
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

async fn count_needs_attention(
    state: &AppState,
    transactions: &[Transaction],
) -> Result<usize, AppError> {
    let futures = transactions
        .iter()
        .filter(|t| {
            !matches!(
                t.status_enum(),
                TransactionStatus::Sold
                    | TransactionStatus::Canceled
                    | TransactionStatus::Withdrawn
            )
        })
        .map(|t| async move {
            let mut r = state
                .db
                .query(
                    "SELECT count() FROM $t->has_item->checklist_item \
                     WHERE required = true AND approval_status != 'approved' GROUP ALL",
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
                    transaction_key: crate::record_key(&tx.id),
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
    tx_type: TransactionType,
    cond: SpecialSalesCondition,
    sales: SalesType,
) -> Result<(), AppError> {
    let defaults = forms::build_default_checklist(tx_type, cond, sales);
    let item_futures = defaults.iter().enumerate().map(|(i, di)| async move {
        let form = forms::lookup(di.code);
        let title = form
            .map(|f| f.name.to_string())
            .unwrap_or_else(|| di.code.to_string());
        let item: Option<ChecklistItem> = state
            .db
            .create("checklist_item")
            .content(NewChecklistItem {
                title,
                form_code: Some(di.code.to_string()),
                group_slug: di.group.slug().to_string(),
                position: i as i64,
                required: di.required,
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
