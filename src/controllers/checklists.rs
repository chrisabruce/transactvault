//! Checklist item create + toggle.
//!
//! Two creation paths:
//!
//! - **Add from CAR library** — pick a known form code; we look up its
//!   canonical group + name automatically.
//! - **Add custom item** — free-text title, lands in
//!   `Additional Disclosures` so it's visible but never blocks compliance.

use axum::Form;
use axum::extract::{Path, State};
use axum::response::Redirect;
use serde::Deserialize;
use surrealdb::types::{RecordId, SurrealValue};

use crate::auth::CurrentUser;
use crate::controllers::transactions::authorize_transaction;
use crate::error::AppError;
use crate::forms::{self, FormGroup};
use crate::models::{ChecklistItem, NewChecklistItem};
use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct NewItemInput {
    /// Either a CAR form code (preferred) or empty if `title` is set.
    #[serde(default)]
    pub form_code: Option<String>,
    /// Custom title for free-text items. Ignored if `form_code` is set.
    #[serde(default)]
    pub title: Option<String>,
    /// Optional required flag — defaults to false for additions.
    #[serde(default)]
    pub required: Option<String>,
}

pub async fn create(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<String>,
    Form(input): Form<NewItemInput>,
) -> Result<Redirect, AppError> {
    let tx_id = RecordId::new("transaction", id.as_str());
    let _ = authorize_transaction(&state, &user, &tx_id).await?;

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

    let required = input
        .required
        .as_deref()
        .map(|v| matches!(v, "1" | "true" | "on" | "yes"))
        .unwrap_or(false);

    let new_item = match input.form_code.as_deref().filter(|s| !s.is_empty()) {
        Some(code) => {
            // Resolve metadata from the brokerage's DB catalog first —
            // that's what the Add-an-item picker offered, and it's the
            // only source that knows admin-added and broker-custom
            // forms. Codes it doesn't know fall back to the compiled
            // CAR library, and finally to a graceful free-text item
            // (title = code, group = Additional) so a hand-typed code
            // is never rejected.
            let db_form =
                crate::db::forms::find_brokerage_form(&state.db, &user.brokerage_id, code)
                    .await
                    .map_err(|e| AppError::Internal(e.context("resolving form for add")))?;
            let compiled = forms::lookup(code);

            let canonical_code = db_form
                .as_ref()
                .map(|f| f.code.clone())
                .or_else(|| compiled.map(|f| f.code.to_string()))
                .unwrap_or_else(|| code.to_string());
            let title = db_form
                .as_ref()
                .map(|f| f.name.clone())
                .or_else(|| compiled.map(|f| f.name.to_string()))
                .unwrap_or_else(|| code.to_string());
            let allows_multiple = db_form
                .as_ref()
                .map(|f| f.allows_multiple)
                .or_else(|| compiled.map(|f| f.allows_multiple))
                .unwrap_or(false);
            // Group: the DB form's own group when it has one; otherwise
            // infer from the code. `seed_group()` maps to the same
            // (name, order) the seeder used, so a manually-added form
            // buckets with its seeded peers.
            let (group_name, group_order) = match db_form.as_ref().and_then(|f| f.group()) {
                Some((name, order)) => (name, order),
                None => {
                    let g = compiled
                        .map(|f| forms::infer_group_from_code(f.code))
                        .unwrap_or(FormGroup::AdditionalDisclosures);
                    let (n, o) = g.seed_group();
                    (n.to_string(), o)
                }
            };

            // The picker offers the whole catalog, including forms already
            // on this checklist — so single-instance duplicates must be
            // rejected here. Forms marked `allows_multiple` (addenda,
            // counter offers) are always allowed again.
            if !allows_multiple {
                let mut codes_q = state
                    .db
                    .query(
                        "SELECT VALUE form_code FROM $t->has_item->checklist_item \
                         WHERE form_code != NONE",
                    )
                    .bind(("t", tx_id.clone()))
                    .await?;
                let existing: Vec<String> = codes_q.take(0).unwrap_or_default();
                if existing
                    .iter()
                    .any(|c| c.eq_ignore_ascii_case(&canonical_code))
                {
                    return Err(AppError::invalid(format!(
                        "{} is already on this checklist — find it under its \
                         category above.",
                        canonical_code.to_ascii_uppercase()
                    )));
                }
            }

            NewChecklistItem {
                title,
                form_code: Some(canonical_code),
                group_name,
                group_order,
                position,
                required,
            }
        }
        None => {
            let title = input
                .title
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| AppError::invalid("Either pick a form or enter a custom title."))?
                .to_string();
            let (group_name, group_order) = FormGroup::AdditionalDisclosures.seed_group();
            NewChecklistItem {
                title,
                form_code: None,
                group_name: group_name.to_string(),
                group_order,
                position,
                required,
            }
        }
    };

    let item: Option<ChecklistItem> = state.db.create("checklist_item").content(new_item).await?;
    let item = item
        .ok_or_else(|| AppError::Internal(anyhow::anyhow!("checklist insert returned nothing")))?;

    state
        .db
        .query("RELATE $t->has_item->$c")
        .bind(("t", tx_id.clone()))
        .bind(("c", item.id))
        .await?;

    state
        .events
        .publish(crate::events::Event::BrokerageMutation(
            user.brokerage_id.clone(),
        ));

    Ok(Redirect::to(&format!("/app/transactions/{id}")))
}

pub async fn approve(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(item_id): Path<String>,
) -> Result<Redirect, AppError> {
    set_approval(&state, &user, item_id, "approved", None).await
}

/// POST body for the Deny action. The reason field is optional — the
/// review workflow shouldn't be blocked on the reviewer typing one, but
/// when supplied it becomes a comment on the item so the agent sees the
/// explanation in the same thread they already read.
#[derive(Debug, Deserialize)]
pub struct DenyInput {
    #[serde(default)]
    pub reason: Option<String>,
}

pub async fn deny(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(item_id): Path<String>,
    Form(input): Form<DenyInput>,
) -> Result<Redirect, AppError> {
    set_approval(&state, &user, item_id, "denied", input.reason).await
}

async fn set_approval(
    state: &AppState,
    user: &CurrentUser,
    item_id: String,
    new_status: &'static str,
    reason: Option<String>,
) -> Result<Redirect, AppError> {
    if !user.role.can_review() {
        return Err(AppError::Forbidden);
    }
    let item_ref = RecordId::new("checklist_item", item_id.as_str());

    // Find the owning transaction via the incoming `has_item` edge so we
    // can authorize the caller and redirect them back.
    let mut response = state
        .db
        .query("SELECT VALUE in FROM has_item WHERE out = $c LIMIT 1")
        .bind(("c", item_ref.clone()))
        .await?;
    let txs: Vec<RecordId> = response.take(0)?;
    let tx_id = txs.into_iter().next().ok_or(AppError::NotFound)?;
    let _ = authorize_transaction(state, user, &tx_id).await?;

    // Nothing to review until something has actually been uploaded against
    // the item — the UI hides the buttons in this state, but enforce it
    // server-side too so a direct POST can't bypass the gate.
    let mut count_q = state
        .db
        .query("SELECT count() FROM $c<-for_item<-document GROUP ALL")
        .bind(("c", item_ref.clone()))
        .await?;
    #[derive(serde::Deserialize, SurrealValue)]
    struct CountRow {
        count: i64,
    }
    let count: Option<CountRow> = count_q.take(0)?;
    if count.map(|c| c.count).unwrap_or(0) == 0 {
        return Err(AppError::invalid(
            "Nothing to review — no document has been uploaded against this item yet.",
        ));
    }

    state
        .db
        .query(
            "UPDATE $c SET
                approval_status = $s,
                reviewed_at     = time::now(),
                reviewed_by     = $u",
        )
        .bind(("c", item_ref.clone()))
        .bind(("u", user.user_id.clone()))
        .bind(("s", new_status))
        .await?;

    // If the reviewer supplied a reason on Deny, drop it into the item's
    // comment thread so the agent reads the explanation in the place
    // they're already watching. Empty / whitespace-only is silently
    // skipped — the prompt is optional.
    if let Some(text) = reason
        && !text.trim().is_empty()
    {
        crate::controllers::comments::insert_comment(state, user, item_ref, text).await?;
    }

    state
        .events
        .publish(crate::events::Event::BrokerageMutation(
            user.brokerage_id.clone(),
        ));

    let key = crate::db::record_key(&tx_id);
    Ok(Redirect::to(&format!("/app/transactions/{key}")))
}

// `infer_group_from_code` moved to `crate::forms` — the catalog
// backfill seeder places picker-only forms with the same rules the
// manual-add path uses, so both call one implementation.
