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
            // Look up canonical metadata for the form. Unknown codes get a
            // graceful fallback — title = code, group = Additional.
            let form = forms::lookup(code);
            let title = form
                .map(|f| f.name.to_string())
                .unwrap_or_else(|| code.to_string());
            let group = form
                .and_then(|f| {
                    forms::LIBRARY.iter().find(|cf| cf.code == f.code).map(|_| {
                        // Most ad-hoc adds are supporting docs — drop them
                        // into "Disclosures — If Applicable" by default,
                        // unless the code is for a contract or report.
                        infer_group_from_code(f.code)
                    })
                })
                .unwrap_or(FormGroup::AdditionalDisclosures);
            NewChecklistItem {
                title,
                form_code: Some(code.to_string()),
                group_slug: group.slug().to_string(),
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
            NewChecklistItem {
                title,
                form_code: None,
                group_slug: FormGroup::AdditionalDisclosures.slug().to_string(),
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

    Ok(Redirect::to(&format!("/app/transactions/{id}")))
}

pub async fn approve(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(item_id): Path<String>,
) -> Result<Redirect, AppError> {
    set_approval(&state, &user, item_id, "approved").await
}

pub async fn deny(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(item_id): Path<String>,
) -> Result<Redirect, AppError> {
    set_approval(&state, &user, item_id, "denied").await
}

async fn set_approval(
    state: &AppState,
    user: &CurrentUser,
    item_id: String,
    new_status: &'static str,
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
        .bind(("c", item_ref))
        .bind(("u", user.user_id.clone()))
        .bind(("s", new_status))
        .await?;

    let key = crate::record_key(&tx_id);
    Ok(Redirect::to(&format!("/app/transactions/{key}")))
}

/// Best-guess of which checklist group a freshly-added form should live in.
/// We can't know for sure without the original transaction-type context,
/// so use the printed CAR taxonomy: contracts go to Contracts, reports to
/// Reports, escrow forms to Escrow, mandatory disclosures to Mandatory,
/// everything else falls into "Disclosures — If Applicable".
fn infer_group_from_code(code: &str) -> FormGroup {
    match code {
        // Listing / purchase agreements
        "RPA" | "RIPA" | "RLA" | "CPA" | "CLA" | "VLPA" | "VLL" | "BPA" | "BLA" | "MHPA"
        | "MHLA" | "LR" | "LL" => FormGroup::ListingPurchasingContracts,

        // Mandatory disclosures
        "AVID-1" | "AVID-2" | "FHDS" | "LPD" | "RGM" | "SBSA" | "SPQ" | "TDS" | "WCMD" | "WFDA"
        | "WHSD" | "VP" | "CSPQ" | "MHDA" | "MHTDS" | "VLQ" | "BDS" => {
            FormGroup::MandatoryDisclosures
        }

        // Special-conditions
        "PLA" | "PA" | "SSA" | "SSLA" | "REO" | "REOL" => FormGroup::SpecialConditionsDisclosures,

        // MLS sheets
        "ACT" | "PEND" | "SOLD" => FormGroup::MlsDataSheets,

        // Escrow
        "APRL" | "CC&R" | "CLSD" | "COMM" | "EMD" | "EA" | "EI" | "HOA" | "NET" | "NHD"
        | "NHDS" | "PREL" => FormGroup::EscrowDocuments,

        // Reports & clearances
        "BIW" | "CHIM" | "HOME" | "HPP" | "POOL" | "ROOF" | "SEPT" | "SOLAR" | "TERM" | "WELL" => {
            FormGroup::ReportsCertificatesClearances
        }

        // Release
        "CC" | "COL" | "WOO" => FormGroup::ReleaseDisclosures,

        // Additional support
        "AVAA" | "BCA" | "BRBC" | "EQ" | "EQ-R" | "HID" | "MCA" | "QUAL" | "POF" | "BP-FFE" => {
            FormGroup::AdditionalDisclosures
        }

        _ => FormGroup::DisclosuresIfApplicable,
    }
}
