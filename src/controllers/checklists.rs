//! Checklist item create + toggle.
//!
//! Completions write both a timestamp and the completing user onto the item,
//! giving us a free audit trail. The toggle endpoint redirects back to the
//! transaction page — a new full render is effectively instantaneous because
//! everything is a single SurrealDB round-trip.

use axum::Form;
use axum::extract::{Path, State};
use axum::response::Redirect;
use serde::Deserialize;
use surrealdb::types::{RecordId, SurrealValue};

use crate::auth::CurrentUser;
use crate::controllers::transactions::{authorize_transaction, load_brokerage};
use crate::error::AppError;
use crate::models::{ChecklistItem, NewChecklistItem};
use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct NewItemInput {
    pub title: String,
    #[serde(default)]
    pub category: Option<String>,
}

pub async fn create(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<String>,
    Form(input): Form<NewItemInput>,
) -> Result<Redirect, AppError> {
    let tx_id = RecordId::new("transaction", id.as_str());
    let _ = authorize_transaction(&state, &user, &tx_id).await?;

    let title = input.title.trim().to_string();
    if title.is_empty() {
        return Err(AppError::invalid("Checklist item needs a title."));
    }

    let category = match input.category.as_deref() {
        Some("contract" | "disclosures" | "inspection" | "appraisal" | "title" | "closing") => {
            input.category.unwrap()
        }
        _ => "general".into(),
    };

    // Position = count of existing items, so new items land at the bottom.
    let mut count_q = state
        .db
        .query("SELECT count() FROM $t->has_item->checklist_item GROUP ALL")
        .bind(("t", tx_id.clone()))
        .await?;
    #[derive(serde::Deserialize, SurrealValue)]
    struct CountRow {
        count: i64,
    }
    let count: Option<CountRow> = count_q.take(0)?;
    let position = count.map(|c| c.count).unwrap_or(0);

    let item: Option<ChecklistItem> = state
        .db
        .create("checklist_item")
        .content(NewChecklistItem { title, category, position })
        .await?;
    let item = item.ok_or_else(|| AppError::Internal(anyhow::anyhow!("insert returned nothing")))?;

    state
        .db
        .query("RELATE $t->has_item->$c")
        .bind(("t", tx_id.clone()))
        .bind(("c", item.id))
        .await?;

    // We don't strictly need this load, but it keeps the function honest about
    // side effects on the current user's brokerage context.
    let _ = load_brokerage(&state, &user).await?;

    Ok(Redirect::to(&format!("/app/transactions/{id}")))
}

pub async fn toggle(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(item_id): Path<String>,
) -> Result<Redirect, AppError> {
    let item_ref = RecordId::new("checklist_item", item_id.as_str());

    // Graph hop: find the transaction that owns this item (incoming `has_item`
    // edge) so we can authorize the caller and redirect them back.
    let mut response = state
        .db
        .query("SELECT VALUE in FROM has_item WHERE out = $c LIMIT 1")
        .bind(("c", item_ref.clone()))
        .await?;
    let txs: Vec<RecordId> = response.take(0)?;
    let tx_id = txs.into_iter().next().ok_or(AppError::NotFound)?;
    let _ = authorize_transaction(&state, &user, &tx_id).await?;

    // Flip the `completed` bit and stamp the audit fields atomically.
    state
        .db
        .query(
            "UPDATE $c SET
                completed    = !completed,
                completed_at = IF !completed THEN time::now() ELSE NONE END,
                completed_by = IF !completed THEN $u ELSE NONE END",
        )
        .bind(("c", item_ref))
        .bind(("u", user.user_id.clone()))
        .await?;

    let key = crate::record_key(&tx_id);
    Ok(Redirect::to(&format!("/app/transactions/{key}")))
}
