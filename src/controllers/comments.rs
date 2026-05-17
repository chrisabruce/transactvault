//! Comments — free-text notes attached to either a transaction or a
//! single checklist item. Two POST endpoints share one create handler;
//! the target table is determined by the URL.

use axum::Form;
use axum::extract::{Path, State};
use axum::response::Redirect;
use serde::Deserialize;
use surrealdb::types::RecordId;

use crate::auth::CurrentUser;
use crate::controllers::transactions::authorize_transaction;
use crate::error::AppError;
use crate::models::NewComment;
use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct NewCommentInput {
    pub body: String,
}

pub async fn create_on_transaction(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(tx_key): Path<String>,
    Form(input): Form<NewCommentInput>,
) -> Result<Redirect, AppError> {
    let tx_id = RecordId::new("transaction", tx_key.as_str());
    let _ = authorize_transaction(&state, &user, &tx_id).await?;

    insert_comment(&state, &user, tx_id.clone(), input.body).await?;
    Ok(Redirect::to(&format!("/app/transactions/{tx_key}")))
}

pub async fn create_on_item(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(item_key): Path<String>,
    Form(input): Form<NewCommentInput>,
) -> Result<Redirect, AppError> {
    let item_id = RecordId::new("checklist_item", item_key.as_str());

    // Find the owning transaction so we can authorize and redirect.
    let mut response = state
        .db
        .query("SELECT VALUE in FROM has_item WHERE out = $c LIMIT 1")
        .bind(("c", item_id.clone()))
        .await?;
    let txs: Vec<RecordId> = response.take(0)?;
    let tx_id = txs.into_iter().next().ok_or(AppError::NotFound)?;
    let _ = authorize_transaction(&state, &user, &tx_id).await?;

    insert_comment(&state, &user, item_id, input.body).await?;
    let key = crate::db::record_key(&tx_id);
    Ok(Redirect::to(&format!("/app/transactions/{key}")))
}

async fn insert_comment(
    state: &AppState,
    user: &CurrentUser,
    target: RecordId,
    body: String,
) -> Result<(), AppError> {
    let body = body.trim().to_string();
    if body.is_empty() {
        return Err(AppError::invalid("Comment body can't be empty."));
    }
    if body.len() > 4000 {
        return Err(AppError::invalid("Comment is too long (max 4000 chars)."));
    }
    let new_comment = NewComment {
        body,
        target,
        author: user.user_id.clone(),
        references_document: None,
    };
    let _: Option<crate::models::Comment> = state.db.create("comment").content(new_comment).await?;
    Ok(())
}
