//! Forms configuration UI.
//!
//! Two audiences:
//! - **Brokers** (`/app/forms`, linked from the account dropdown):
//!   choose a locality, hide library forms they don't use, and add
//!   their own custom forms.
//! - **Super-admins** (`/admin/forms`): manage the master state + local
//!   form sets, their groups, and the forms inside them.
//!
//! All relationships are graph edges (see `db/schema.surql`): a
//! brokerage `uses_state` / `uses_locality` a `form_set`; `hides_form`
//! suppresses a library form; `owns_form` is a custom form. Sets hold
//! `form_group`s (`has_group`) which hold `form`s (`has_form`).

use axum::Form;
use axum::extract::{Path, State};
use axum::response::Redirect;
use serde::Deserialize;
use surrealdb::types::{RecordId, SurrealValue};

use crate::auth::CurrentUser;
use crate::auth::middleware::SuperAdmin;
use crate::controllers::render;
use crate::error::AppError;
use crate::state::AppState;
use crate::templates::{
    AdminFormSetDetailPage, AdminFormSetRow, AdminFormsPage, AppliesChoice, BrokerFormRow,
    BrokerFormsPage, FormGroupView, FormSetOption,
};

/// The standard group names a broker can drop a custom form into.
/// Mirrors the master California groups so custom forms slot in beside
/// library forms instead of inventing new headings.
const GROUP_CHOICES: &[(&str, i64)] = &[
    ("MLS Data Sheets", 0),
    ("Listing Contracts", 1),
    ("Purchase Contracts", 2),
    ("Mandatory Disclosures", 3),
    ("Additional Disclosures", 4),
    ("Escrow Documents", 5),
    ("Reports, Certificates & Clearances", 6),
    ("Release Disclosures", 7),
];

/// (slug, label) pairs for each dimension a form's appearance can be
/// restricted by. Used by the admin Add / Edit form pages and the
/// broker custom-form page to drive checkbox UIs whose POST data the
/// `*Applies` structs deserialize directly. Slugs match the values
/// stored in `form.applies_*` and the values `resolve_checklist`
/// filters by.
pub const APPLIES_TYPES: &[(&str, &str)] = &[
    ("residential", "Residential"),
    ("commercial", "Commercial"),
    ("vacant_lots_land", "Vacant Lots & Land"),
    ("manufactured_home", "Manufactured / Mobile Home"),
    ("business_opportunity", "Business Opportunity"),
    ("commercial_lease", "Commercial Lease"),
    ("rental_lease", "Rental / Lease"),
];
pub const APPLIES_SIDES: &[(&str, &str)] = &[
    ("listing", "Listing"),
    ("purchase", "Purchase"),
    ("both", "Both (dual representation)"),
];
pub const APPLIES_CONDITIONS: &[(&str, &str)] = &[
    ("none", "Standard"),
    ("probate", "Probate"),
    ("short_sale", "Short Sale"),
    ("reo", "REO / Foreclosure"),
];

/// Checkbox state for a form's applicability picker. Each field is
/// `Some(_)` when the matching `<dimension>_<slug>` checkbox is ticked
/// (the value sent is `"1"`; we just check presence). Both the admin
/// and broker create handlers embed this so the same checkbox markup
/// works in both templates.
#[derive(Debug, Default, Deserialize)]
pub struct AppliesPicker {
    #[serde(default)]
    pub type_residential: Option<String>,
    #[serde(default)]
    pub type_commercial: Option<String>,
    #[serde(default)]
    pub type_vacant_lots_land: Option<String>,
    #[serde(default)]
    pub type_manufactured_home: Option<String>,
    #[serde(default)]
    pub type_business_opportunity: Option<String>,
    #[serde(default)]
    pub type_commercial_lease: Option<String>,
    #[serde(default)]
    pub type_rental_lease: Option<String>,

    #[serde(default)]
    pub side_listing: Option<String>,
    #[serde(default)]
    pub side_purchase: Option<String>,
    #[serde(default)]
    pub side_both: Option<String>,

    #[serde(default)]
    pub cond_none: Option<String>,
    #[serde(default)]
    pub cond_probate: Option<String>,
    #[serde(default)]
    pub cond_short_sale: Option<String>,
    #[serde(default)]
    pub cond_reo: Option<String>,
}

impl AppliesPicker {
    pub fn types(&self) -> Vec<String> {
        let mut v = Vec::new();
        if self.type_residential.is_some() {
            v.push("residential".into());
        }
        if self.type_commercial.is_some() {
            v.push("commercial".into());
        }
        if self.type_vacant_lots_land.is_some() {
            v.push("vacant_lots_land".into());
        }
        if self.type_manufactured_home.is_some() {
            v.push("manufactured_home".into());
        }
        if self.type_business_opportunity.is_some() {
            v.push("business_opportunity".into());
        }
        if self.type_commercial_lease.is_some() {
            v.push("commercial_lease".into());
        }
        if self.type_rental_lease.is_some() {
            v.push("rental_lease".into());
        }
        v
    }
    pub fn sides(&self) -> Vec<String> {
        let mut v = Vec::new();
        if self.side_listing.is_some() {
            v.push("listing".into());
        }
        if self.side_purchase.is_some() {
            v.push("purchase".into());
        }
        if self.side_both.is_some() {
            v.push("both".into());
        }
        v
    }
    pub fn conditions(&self) -> Vec<String> {
        let mut v = Vec::new();
        if self.cond_none.is_some() {
            v.push("none".into());
        }
        if self.cond_probate.is_some() {
            v.push("probate".into());
        }
        if self.cond_short_sale.is_some() {
            v.push("short_sale".into());
        }
        if self.cond_reo.is_some() {
            v.push("reo".into());
        }
        v
    }
}

/// All-broad applicability — the fallback used when an admin/broker
/// submits with no checkboxes ticked in a dimension. Lets them create
/// an unrestricted form by skipping the picker entirely (mirrors the
/// pre-picker default behavior).
fn all_types() -> Vec<String> {
    APPLIES_TYPES.iter().map(|(s, _)| (*s).into()).collect()
}
fn all_sides() -> Vec<String> {
    APPLIES_SIDES.iter().map(|(s, _)| (*s).into()).collect()
}
fn all_conditions() -> Vec<String> {
    APPLIES_CONDITIONS
        .iter()
        .map(|(s, _)| (*s).into())
        .collect()
}

/// Build the three checkbox lists for an applicability picker. Pass
/// `None` for the Add-form UI (every box pre-checked = broad default)
/// or `Some((types, sides, conditions))` for the Edit-form UI so each
/// box reflects what the form already stores.
fn picker_choices(
    selected: Option<(&[String], &[String], &[String])>,
) -> (Vec<AppliesChoice>, Vec<AppliesChoice>, Vec<AppliesChoice>) {
    fn dim(defs: &[(&str, &str)], prefix: &str, sel: Option<&[String]>) -> Vec<AppliesChoice> {
        defs.iter()
            .map(|(slug, label)| AppliesChoice {
                field_name: format!("{prefix}_{slug}"),
                label: (*label).into(),
                checked: match sel {
                    Some(s) => s.iter().any(|v| v == slug),
                    None => true,
                },
            })
            .collect()
    }
    let (st, ss, sc) = match selected {
        Some((t, s, c)) => (Some(t), Some(s), Some(c)),
        None => (None, None, None),
    };
    (
        dim(APPLIES_TYPES, "type", st),
        dim(APPLIES_SIDES, "side", ss),
        dim(APPLIES_CONDITIONS, "cond", sc),
    )
}

/// Resolve the picker's three Vecs, falling back to "applies to all"
/// for any dimension the user left entirely unchecked. Empty-list
/// semantics in resolution mean "applies to nothing", which is
/// almost never what someone clicking "save" intends.
fn applies_or_all(picker: &AppliesPicker) -> (Vec<String>, Vec<String>, Vec<String>) {
    let t = picker.types();
    let s = picker.sides();
    let c = picker.conditions();
    (
        if t.is_empty() { all_types() } else { t },
        if s.is_empty() { all_sides() } else { s },
        if c.is_empty() { all_conditions() } else { c },
    )
}

// ---------------------------------------------------------------------------
// Broker config
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, SurrealValue)]
struct SetRow {
    id: RecordId,
    name: String,
}

#[derive(Debug, Deserialize, SurrealValue)]
struct FormListRow {
    id: RecordId,
    code: String,
    name: String,
    group_name: Option<String>,
}

pub async fn broker_forms(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<axum::response::Html<String>, AppError> {
    if !user.role.is_broker() {
        return Err(AppError::Forbidden);
    }
    let brokerage = crate::controllers::transactions::load_brokerage(&state, &user).await?;
    let b = brokerage.id.clone();

    // State set name (display only — California for now).
    let mut sq = state
        .db
        .query("SELECT VALUE out.name FROM uses_state WHERE in = $b LIMIT 1")
        .bind(("b", b.clone()))
        .await?;
    let state_name: Option<String> = sq
        .take::<Vec<String>>(0)
        .unwrap_or_default()
        .into_iter()
        .next();

    // Currently-selected locality.
    let mut lq = state
        .db
        .query("SELECT VALUE out FROM uses_locality WHERE in = $b LIMIT 1")
        .bind(("b", b.clone()))
        .await?;
    let selected_locality: Option<RecordId> = lq
        .take::<Vec<RecordId>>(0)
        .unwrap_or_default()
        .into_iter()
        .next();
    let selected_key = selected_locality.as_ref().map(crate::db::record_key);

    // All local sets, as picker options.
    let mut allq = state
        .db
        .query("SELECT id, name FROM form_set WHERE scope = 'local' AND is_active = true ORDER BY name ASC")
        .await?;
    let local_rows: Vec<SetRow> = allq.take(0).unwrap_or_default();
    let localities: Vec<FormSetOption> = local_rows
        .into_iter()
        .map(|r| {
            let key = crate::db::record_key(&r.id);
            let selected = selected_key.as_deref() == Some(key.as_str());
            FormSetOption {
                key,
                name: r.name,
                selected,
            }
        })
        .collect();

    // Hidden library form ids.
    let mut hq = state
        .db
        .query("SELECT VALUE out FROM hides_form WHERE in = $b")
        .bind(("b", b.clone()))
        .await?;
    let hidden: Vec<RecordId> = hq.take(0).unwrap_or_default();

    // Library forms from the brokerage's state + locality sets.
    let mut fq = state
        .db
        .query(
            "SELECT id, code, name, (<-has_form<-form_group)[0].name AS group_name
             FROM $b->(uses_state, uses_locality)->form_set->has_group->form_group->has_form->form
             ORDER BY code ASC",
        )
        .bind(("b", b.clone()))
        .await?;
    let lib_rows: Vec<FormListRow> = fq.take(0).unwrap_or_default();
    let master_forms: Vec<BrokerFormRow> = lib_rows
        .into_iter()
        .map(|r| BrokerFormRow {
            key: crate::db::record_key(&r.id),
            code: r.code,
            name: r.name,
            group_name: r.group_name.unwrap_or_default(),
            hidden: hidden.contains(&r.id),
            custom: false,
        })
        .collect();

    // Custom forms owned by the brokerage.
    let mut cq = state
        .db
        .query(
            "SELECT id, code, name, (group_name ?? 'Additional Disclosures') AS group_name
             FROM $b->owns_form->form ORDER BY code ASC",
        )
        .bind(("b", b.clone()))
        .await?;
    let custom_rows: Vec<FormListRow> = cq.take(0).unwrap_or_default();
    let custom_forms: Vec<BrokerFormRow> = custom_rows
        .into_iter()
        .map(|r| BrokerFormRow {
            key: crate::db::record_key(&r.id),
            code: r.code,
            name: r.name,
            group_name: r.group_name.unwrap_or_default(),
            hidden: false,
            custom: true,
        })
        .collect();

    let header = crate::controllers::common::build_app_header(&state, &user, "forms").await;

    let (picker_types, picker_sides, picker_conditions) = picker_choices(None);
    render(&BrokerFormsPage {
        app_name: &state.config.app_name,
        base_url: &state.config.base_url,
        signed_in: true,
        header,
        state_name: state_name.unwrap_or_else(|| "—".into()),
        localities,
        master_forms,
        custom_forms,
        group_choices: GROUP_CHOICES.iter().map(|(n, _)| *n).collect(),
        picker_types,
        picker_sides,
        picker_conditions,
    })
}

#[derive(Debug, Deserialize)]
pub struct LocalityInput {
    /// `""` clears the locality; otherwise a form_set key.
    pub locality: String,
}

pub async fn set_locality(
    State(state): State<AppState>,
    user: CurrentUser,
    Form(input): Form<LocalityInput>,
) -> Result<Redirect, AppError> {
    if !user.role.is_broker() {
        return Err(AppError::Forbidden);
    }
    let b = user.brokerage_id.clone();
    // Replace any existing locality edge.
    state
        .db
        .query("DELETE uses_locality WHERE in = $b")
        .bind(("b", b.clone()))
        .await?;
    let key = input.locality.trim();
    if !key.is_empty() {
        let set = RecordId::new("form_set", key);
        state
            .db
            .query("RELATE $b->uses_locality->$s")
            .bind(("b", b))
            .bind(("s", set))
            .await?;
    }
    Ok(Redirect::to("/app/forms?flash=locality"))
}

pub async fn toggle_hide(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(key): Path<String>,
) -> Result<Redirect, AppError> {
    if !user.role.is_broker() {
        return Err(AppError::Forbidden);
    }
    let b = user.brokerage_id.clone();
    let form = RecordId::new("form", key.as_str());
    // Toggle: drop the edge if present, else create it.
    state
        .db
        .query(
            "IF (SELECT VALUE id FROM hides_form WHERE in = $b AND out = $f) = [] {
                RELATE $b->hides_form->$f
            } ELSE {
                DELETE hides_form WHERE in = $b AND out = $f
            }",
        )
        .bind(("b", b))
        .bind(("f", form))
        .await?;
    Ok(Redirect::to("/app/forms?flash=hidden"))
}

#[derive(Debug, Deserialize)]
pub struct CustomFormInput {
    pub code: String,
    pub name: String,
    pub group_name: String,
    #[serde(default)]
    pub required: Option<String>,
    // Applicability picker — `serde_urlencoded` doesn't support
    // `#[serde(flatten)]`, so the checkbox fields live on this struct
    // directly. `applies_picker()` packages them into an
    // `AppliesPicker` for the shared helper.
    #[serde(default)]
    pub type_residential: Option<String>,
    #[serde(default)]
    pub type_commercial: Option<String>,
    #[serde(default)]
    pub type_vacant_lots_land: Option<String>,
    #[serde(default)]
    pub type_manufactured_home: Option<String>,
    #[serde(default)]
    pub type_business_opportunity: Option<String>,
    #[serde(default)]
    pub type_commercial_lease: Option<String>,
    #[serde(default)]
    pub type_rental_lease: Option<String>,
    #[serde(default)]
    pub side_listing: Option<String>,
    #[serde(default)]
    pub side_purchase: Option<String>,
    #[serde(default)]
    pub side_both: Option<String>,
    #[serde(default)]
    pub cond_none: Option<String>,
    #[serde(default)]
    pub cond_probate: Option<String>,
    #[serde(default)]
    pub cond_short_sale: Option<String>,
    #[serde(default)]
    pub cond_reo: Option<String>,
}

impl CustomFormInput {
    fn applies_picker(&self) -> AppliesPicker {
        AppliesPicker {
            type_residential: self.type_residential.clone(),
            type_commercial: self.type_commercial.clone(),
            type_vacant_lots_land: self.type_vacant_lots_land.clone(),
            type_manufactured_home: self.type_manufactured_home.clone(),
            type_business_opportunity: self.type_business_opportunity.clone(),
            type_commercial_lease: self.type_commercial_lease.clone(),
            type_rental_lease: self.type_rental_lease.clone(),
            side_listing: self.side_listing.clone(),
            side_purchase: self.side_purchase.clone(),
            side_both: self.side_both.clone(),
            cond_none: self.cond_none.clone(),
            cond_probate: self.cond_probate.clone(),
            cond_short_sale: self.cond_short_sale.clone(),
            cond_reo: self.cond_reo.clone(),
        }
    }
}

pub async fn add_custom(
    State(state): State<AppState>,
    user: CurrentUser,
    Form(input): Form<CustomFormInput>,
) -> Result<Redirect, AppError> {
    if !user.role.is_broker() {
        return Err(AppError::Forbidden);
    }
    let code = input.code.trim().to_ascii_uppercase();
    let name = input.name.trim().to_string();
    if code.is_empty() || name.is_empty() {
        return Err(AppError::invalid("Custom forms need a code and a name."));
    }
    let group_order = GROUP_CHOICES
        .iter()
        .find(|(n, _)| *n == input.group_name)
        .map(|(_, o)| *o)
        .unwrap_or(4);
    let required = matches!(input.required.as_deref(), Some("1" | "true" | "on" | "yes"));

    // Picker-driven applicability. Unchecking everything in a
    // dimension means "applies to all of that dimension" — see
    // `applies_or_all` for the rationale.
    let (applies_types, applies_sides, applies_conditions) =
        applies_or_all(&input.applies_picker());
    let created: Option<CreatedForm> = state
        .db
        .create("form")
        .content(NewCustomForm {
            code,
            name,
            description: String::new(),
            includes: String::new(),
            form_order: 9000, // sort after library forms within the group
            required,
            allows_multiple: false,
            group_name: Some(input.group_name.clone()),
            group_order: Some(group_order),
            applies_types,
            applies_sides,
            applies_conditions,
            is_active: true,
        })
        .await?;
    let form_id = created
        .ok_or_else(|| AppError::Internal(anyhow::anyhow!("custom form create returned nothing")))?
        .id;
    state
        .db
        .query("RELATE $b->owns_form->$f")
        .bind(("b", user.brokerage_id.clone()))
        .bind(("f", form_id))
        .await?;
    Ok(Redirect::to("/app/forms?flash=custom_added"))
}

#[derive(Debug, Deserialize, SurrealValue)]
struct CreatedForm {
    id: RecordId,
}

#[derive(Debug, serde::Serialize, SurrealValue)]
struct NewCustomForm {
    code: String,
    name: String,
    description: String,
    includes: String,
    form_order: i64,
    required: bool,
    allows_multiple: bool,
    group_name: Option<String>,
    group_order: Option<i64>,
    applies_types: Vec<String>,
    applies_sides: Vec<String>,
    applies_conditions: Vec<String>,
    is_active: bool,
}

// ---------------------------------------------------------------------------
// Admin: form-set management
// ---------------------------------------------------------------------------

pub async fn admin_list(
    State(state): State<AppState>,
    SuperAdmin(user): SuperAdmin,
) -> Result<axum::response::Html<String>, AppError> {
    let sets = load_admin_set_rows(&state).await?;
    let (state_sets, local_sets): (Vec<_>, Vec<_>) =
        sets.into_iter().partition(|s| s.scope == "state");

    let header = crate::controllers::common::build_app_header(&state, &user, "admin").await;

    render(&AdminFormsPage {
        app_name: &state.config.app_name,
        base_url: &state.config.base_url,
        signed_in: true,
        header,
        state_sets,
        local_sets,
    })
}

async fn load_admin_set_rows(state: &AppState) -> Result<Vec<AdminFormSetRow>, AppError> {
    let mut q = state
        .db
        .query(
            "SELECT id, name, scope,
                    count(->has_group->form_group) AS group_count,
                    count(->has_group->form_group->has_form->form) AS form_count
             FROM form_set ORDER BY scope ASC, name ASC",
        )
        .await?;
    #[derive(Debug, Deserialize, SurrealValue)]
    struct Row {
        id: RecordId,
        name: String,
        scope: String,
        group_count: Option<i64>,
        form_count: Option<i64>,
    }
    let rows: Vec<Row> = q.take(0).unwrap_or_default();
    Ok(rows
        .into_iter()
        .map(|r| AdminFormSetRow {
            key: crate::db::record_key(&r.id),
            name: r.name,
            scope: r.scope,
            group_count: r.group_count.unwrap_or(0),
            form_count: r.form_count.unwrap_or(0),
        })
        .collect())
}

#[derive(Debug, Deserialize)]
pub struct NewSetInput {
    pub name: String,
}

pub async fn admin_create_set(
    State(state): State<AppState>,
    SuperAdmin(admin): SuperAdmin,
    Form(input): Form<NewSetInput>,
) -> Result<Redirect, AppError> {
    let name = input.name.trim().to_string();
    if name.is_empty() {
        return Err(AppError::invalid("Locality name is required."));
    }
    // Create the local set and hang it off California via has_locality.
    let created: Option<CreatedForm> = state
        .db
        .create("form_set")
        .content(NewSet {
            scope: "local".to_string(),
            name,
            is_active: true,
        })
        .await?;
    let set_id = created
        .ok_or_else(|| AppError::Internal(anyhow::anyhow!("form_set create returned nothing")))?
        .id;
    if let Some(ca) = crate::db::forms::california_set_id(&state.db).await? {
        state
            .db
            .query("RELATE $s->has_locality->$l")
            .bind(("s", ca))
            .bind(("l", set_id.clone()))
            .await?;
    }
    crate::audit::record(
        &state.db,
        "admin_view",
        Some(admin.user_id.clone()),
        Some(admin.email.clone()),
        None,
        None,
        Some(format!(
            "created local form set {}",
            crate::db::record_key(&set_id)
        )),
    )
    .await;
    Ok(Redirect::to(&format!(
        "/admin/forms/{}",
        crate::db::record_key(&set_id)
    )))
}

#[derive(Debug, serde::Serialize, SurrealValue)]
struct NewSet {
    scope: String,
    name: String,
    is_active: bool,
}

pub async fn admin_set_detail(
    State(state): State<AppState>,
    SuperAdmin(user): SuperAdmin,
    Path(key): Path<String>,
) -> Result<axum::response::Html<String>, AppError> {
    let set_id = RecordId::new("form_set", key.as_str());
    let mut sq = state
        .db
        .query("SELECT name, scope FROM ONLY $s")
        .bind(("s", set_id.clone()))
        .await?;
    #[derive(Debug, Deserialize, SurrealValue)]
    struct SetMeta {
        name: String,
        scope: String,
    }
    let meta: Option<SetMeta> = sq.take(0)?;
    let meta = meta.ok_or(AppError::NotFound)?;

    // Groups + their forms.
    let mut gq = state
        .db
        .query("SELECT id, name, sort_order FROM $s->has_group->form_group ORDER BY sort_order ASC")
        .bind(("s", set_id.clone()))
        .await?;
    #[derive(Debug, Deserialize, SurrealValue)]
    struct GroupRow {
        id: RecordId,
        name: String,
        sort_order: i64,
    }
    let group_rows: Vec<GroupRow> = gq.take(0).unwrap_or_default();

    let mut groups: Vec<FormGroupView> = Vec::new();
    for g in group_rows {
        let mut fq = state
            .db
            .query(
                "SELECT id, code, name, required, is_active, form_order
                 FROM $g->has_form->form ORDER BY form_order ASC, code ASC",
            )
            .bind(("g", g.id.clone()))
            .await?;
        #[derive(Debug, Deserialize, SurrealValue)]
        struct FRow {
            id: RecordId,
            code: String,
            name: String,
            required: bool,
            is_active: bool,
            form_order: i64,
        }
        let frows: Vec<FRow> = fq.take(0).unwrap_or_default();
        groups.push(FormGroupView {
            key: crate::db::record_key(&g.id),
            name: g.name,
            sort_order: g.sort_order,
            forms: frows
                .into_iter()
                .map(|f| crate::templates::AdminFormRow {
                    key: crate::db::record_key(&f.id),
                    code: f.code,
                    name: f.name,
                    required: f.required,
                    is_active: f.is_active,
                    form_order: f.form_order,
                })
                .collect(),
        });
    }

    let header = crate::controllers::common::build_app_header(&state, &user, "admin").await;

    let (picker_types, picker_sides, picker_conditions) = picker_choices(None);
    render(&AdminFormSetDetailPage {
        app_name: &state.config.app_name,
        base_url: &state.config.base_url,
        signed_in: true,
        header,
        set_key: key,
        set_name: meta.name,
        set_scope: meta.scope,
        groups,
        group_choices: GROUP_CHOICES.iter().map(|(n, _)| *n).collect(),
        picker_types,
        picker_sides,
        picker_conditions,
    })
}

#[derive(Debug, Deserialize)]
pub struct NewGroupInput {
    pub name: String,
    #[serde(default)]
    pub sort_order: Option<i64>,
}

pub async fn admin_add_group(
    State(state): State<AppState>,
    SuperAdmin(_user): SuperAdmin,
    Path(key): Path<String>,
    Form(input): Form<NewGroupInput>,
) -> Result<Redirect, AppError> {
    let name = input.name.trim().to_string();
    if name.is_empty() {
        return Err(AppError::invalid("Group name is required."));
    }
    let set_id = RecordId::new("form_set", key.as_str());
    // Refuse to RELATE a child into a dangling parent — otherwise a
    // stale URL silently creates orphan groups under a missing set.
    assert_set_exists(&state.db, &set_id).await?;
    let sort_order = input.sort_order.unwrap_or(50);
    let created: Option<CreatedForm> = state
        .db
        .create("form_group")
        .content(NewGroup { name, sort_order })
        .await?;
    let gid = created
        .ok_or_else(|| AppError::Internal(anyhow::anyhow!("form_group create returned nothing")))?
        .id;
    state
        .db
        .query("RELATE $s->has_group->$g")
        .bind(("s", set_id))
        .bind(("g", gid))
        .await?;
    Ok(Redirect::to(&format!(
        "/admin/forms/{key}?flash=group_added"
    )))
}

#[derive(Debug, serde::Serialize, SurrealValue)]
struct NewGroup {
    name: String,
    sort_order: i64,
}

#[derive(Debug, Deserialize)]
pub struct NewAdminFormInput {
    pub group_key: String,
    pub code: String,
    pub name: String,
    #[serde(default)]
    pub form_order: Option<i64>,
    #[serde(default)]
    pub required: Option<String>,
    // Applicability picker — same inlined-fields pattern as
    // `CustomFormInput` (serde_urlencoded won't flatten).
    #[serde(default)]
    pub type_residential: Option<String>,
    #[serde(default)]
    pub type_commercial: Option<String>,
    #[serde(default)]
    pub type_vacant_lots_land: Option<String>,
    #[serde(default)]
    pub type_manufactured_home: Option<String>,
    #[serde(default)]
    pub type_business_opportunity: Option<String>,
    #[serde(default)]
    pub type_commercial_lease: Option<String>,
    #[serde(default)]
    pub type_rental_lease: Option<String>,
    #[serde(default)]
    pub side_listing: Option<String>,
    #[serde(default)]
    pub side_purchase: Option<String>,
    #[serde(default)]
    pub side_both: Option<String>,
    #[serde(default)]
    pub cond_none: Option<String>,
    #[serde(default)]
    pub cond_probate: Option<String>,
    #[serde(default)]
    pub cond_short_sale: Option<String>,
    #[serde(default)]
    pub cond_reo: Option<String>,
}

impl NewAdminFormInput {
    fn applies_picker(&self) -> AppliesPicker {
        AppliesPicker {
            type_residential: self.type_residential.clone(),
            type_commercial: self.type_commercial.clone(),
            type_vacant_lots_land: self.type_vacant_lots_land.clone(),
            type_manufactured_home: self.type_manufactured_home.clone(),
            type_business_opportunity: self.type_business_opportunity.clone(),
            type_commercial_lease: self.type_commercial_lease.clone(),
            type_rental_lease: self.type_rental_lease.clone(),
            side_listing: self.side_listing.clone(),
            side_purchase: self.side_purchase.clone(),
            side_both: self.side_both.clone(),
            cond_none: self.cond_none.clone(),
            cond_probate: self.cond_probate.clone(),
            cond_short_sale: self.cond_short_sale.clone(),
            cond_reo: self.cond_reo.clone(),
        }
    }
}

pub async fn admin_add_form(
    State(state): State<AppState>,
    SuperAdmin(_user): SuperAdmin,
    Path(key): Path<String>,
    Form(input): Form<NewAdminFormInput>,
) -> Result<Redirect, AppError> {
    let code = input.code.trim().to_ascii_uppercase();
    let name = input.name.trim().to_string();
    if code.is_empty() || name.is_empty() {
        return Err(AppError::invalid("Form code and name are required."));
    }
    // Confirm the chosen group actually belongs to this set — stops
    // SuperAdmin URL-mismatch errors from attaching a form to a group
    // in an unrelated set.
    let set_id = RecordId::new("form_set", key.as_str());
    let group_key = input.group_key.trim();
    assert_groups_in_set(&state.db, &set_id, &[group_key]).await?;
    let group_id = RecordId::new("form_group", group_key);
    let required = matches!(input.required.as_deref(), Some("1" | "true" | "on" | "yes"));
    let (applies_types, applies_sides, applies_conditions) =
        applies_or_all(&input.applies_picker());
    let created: Option<CreatedForm> = state
        .db
        .create("form")
        .content(NewCustomForm {
            code,
            name,
            description: String::new(),
            includes: String::new(),
            form_order: input.form_order.unwrap_or(500),
            required,
            allows_multiple: false,
            group_name: None,
            group_order: None,
            applies_types,
            applies_sides,
            applies_conditions,
            is_active: true,
        })
        .await?;
    let fid = created
        .ok_or_else(|| AppError::Internal(anyhow::anyhow!("form create returned nothing")))?
        .id;
    state
        .db
        .query("RELATE $g->has_form->$f")
        .bind(("g", group_id))
        .bind(("f", fid))
        .await?;
    Ok(Redirect::to(&format!(
        "/admin/forms/{key}?flash=form_added"
    )))
}

/// Drag-and-drop reorder payload: `order` is a comma-separated list of
/// record keys in their new display order. The client also sends a
/// `group` field for form reordering, but record keys are globally
/// unique so it isn't needed here — serde drops the unknown field.
#[derive(Debug, Deserialize)]
pub struct ReorderInput {
    pub order: String,
}

/// Verify that every group key in `keys` belongs to `set_id` via
/// `set->has_group->form_group`. Returns `AppError::NotFound` if even
/// one key is foreign — that closes the door on a SuperAdmin (or a
/// crafted request) reaching into an unrelated set's groups by URL
/// manipulation. Empty input is a no-op.
async fn assert_groups_in_set(
    db: &crate::state::Db,
    set_id: &RecordId,
    keys: &[&str],
) -> Result<(), AppError> {
    if keys.is_empty() {
        return Ok(());
    }
    let ids: Vec<RecordId> = keys
        .iter()
        .map(|k| RecordId::new("form_group", *k))
        .collect();
    let mut q = db
        .query(
            "SELECT count() FROM $s->has_group->form_group \
             WHERE id IN $ids GROUP ALL",
        )
        .bind(("s", set_id.clone()))
        .bind(("ids", ids.clone()))
        .await?;
    #[derive(Debug, Deserialize, SurrealValue)]
    struct Counted {
        count: i64,
    }
    let row: Option<Counted> = q.take(0)?;
    let found = row.map(|r| r.count).unwrap_or(0);
    if found as usize != ids.len() {
        return Err(AppError::NotFound);
    }
    Ok(())
}

/// Verify that every form key in `keys` belongs to `set_id`
/// transitively via `set->has_group->form_group->has_form->form`.
/// Same rationale as [`assert_groups_in_set`] — keeps each set's
/// hierarchy tamper-proof from URL/form-body fuzzing.
async fn assert_forms_in_set(
    db: &crate::state::Db,
    set_id: &RecordId,
    keys: &[&str],
) -> Result<(), AppError> {
    if keys.is_empty() {
        return Ok(());
    }
    let ids: Vec<RecordId> = keys.iter().map(|k| RecordId::new("form", *k)).collect();
    let mut q = db
        .query(
            "SELECT count() FROM $s->has_group->form_group->has_form->form \
             WHERE id IN $ids GROUP ALL",
        )
        .bind(("s", set_id.clone()))
        .bind(("ids", ids.clone()))
        .await?;
    #[derive(Debug, Deserialize, SurrealValue)]
    struct Counted {
        count: i64,
    }
    let row: Option<Counted> = q.take(0)?;
    let found = row.map(|r| r.count).unwrap_or(0);
    if found as usize != ids.len() {
        return Err(AppError::NotFound);
    }
    Ok(())
}

/// Confirm the set itself exists so we don't `RELATE` a child into a
/// dangling parent when an admin pastes a stale key.
async fn assert_set_exists(db: &crate::state::Db, set_id: &RecordId) -> Result<(), AppError> {
    let mut q = db
        .query("SELECT VALUE id FROM ONLY $s")
        .bind(("s", set_id.clone()))
        .await?;
    let exists: Option<RecordId> = q.take(0)?;
    if exists.is_none() {
        return Err(AppError::NotFound);
    }
    Ok(())
}

/// Persist a new group order within a set: each `form_group`'s
/// `sort_order` becomes its index in the dropped sequence. Returns 204
/// (the client already moved the DOM; it only needs success/failure).
pub async fn admin_reorder_groups(
    State(state): State<AppState>,
    SuperAdmin(_user): SuperAdmin,
    Path(set_key): Path<String>,
    Form(input): Form<ReorderInput>,
) -> Result<axum::http::StatusCode, AppError> {
    let set_id = RecordId::new("form_set", set_key.as_str());
    let keys: Vec<&str> = input
        .order
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    assert_groups_in_set(&state.db, &set_id, &keys).await?;
    for (i, key) in keys.iter().enumerate() {
        let gid = RecordId::new("form_group", *key);
        state
            .db
            .query("UPDATE $g SET sort_order = $o")
            .bind(("g", gid))
            .bind(("o", i as i64))
            .await?;
    }
    Ok(axum::http::StatusCode::NO_CONTENT)
}

/// Persist a new form order within a group: each `form`'s `form_order`
/// becomes its index in the dropped sequence.
pub async fn admin_reorder_forms(
    State(state): State<AppState>,
    SuperAdmin(_user): SuperAdmin,
    Path(set_key): Path<String>,
    Form(input): Form<ReorderInput>,
) -> Result<axum::http::StatusCode, AppError> {
    let set_id = RecordId::new("form_set", set_key.as_str());
    let keys: Vec<&str> = input
        .order
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    assert_forms_in_set(&state.db, &set_id, &keys).await?;
    for (i, key) in keys.iter().enumerate() {
        let fid = RecordId::new("form", *key);
        state
            .db
            .query("UPDATE $f SET form_order = $o")
            .bind(("f", fid))
            .bind(("o", i as i64))
            .await?;
    }
    Ok(axum::http::StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
pub struct ToggleFormPath {
    pub key: String,
    pub form_key: String,
}

pub async fn admin_toggle_form(
    State(state): State<AppState>,
    SuperAdmin(_user): SuperAdmin,
    Path(p): Path<ToggleFormPath>,
) -> Result<Redirect, AppError> {
    let set_id = RecordId::new("form_set", p.key.as_str());
    assert_forms_in_set(&state.db, &set_id, &[p.form_key.as_str()]).await?;
    let form_id = RecordId::new("form", p.form_key.as_str());
    state
        .db
        .query("UPDATE $f SET is_active = !is_active")
        .bind(("f", form_id))
        .await?;
    Ok(Redirect::to(&format!(
        "/admin/forms/{}?flash=form_toggled",
        p.key
    )))
}

/// GET `/admin/forms/{set_key}/forms/{form_key}/edit` — render the
/// per-form edit page with applicability checkboxes pre-populated
/// from the stored `applies_*` arrays.
pub async fn admin_edit_form(
    State(state): State<AppState>,
    SuperAdmin(user): SuperAdmin,
    Path(p): Path<ToggleFormPath>,
) -> Result<axum::response::Html<String>, AppError> {
    let set_id = RecordId::new("form_set", p.key.as_str());
    assert_forms_in_set(&state.db, &set_id, &[p.form_key.as_str()]).await?;

    #[derive(Debug, Deserialize, SurrealValue)]
    struct LoadedForm {
        code: String,
        name: String,
        form_order: i64,
        required: bool,
        applies_types: Vec<String>,
        applies_sides: Vec<String>,
        applies_conditions: Vec<String>,
    }
    let form_id = RecordId::new("form", p.form_key.as_str());
    let mut fq = state
        .db
        .query(
            "SELECT code, name, form_order, required, \
             applies_types, applies_sides, applies_conditions FROM ONLY $f",
        )
        .bind(("f", form_id.clone()))
        .await?;
    let form: Option<LoadedForm> = fq.take(0)?;
    let form = form.ok_or(AppError::NotFound)?;

    #[derive(Debug, Deserialize, SurrealValue)]
    struct SetMeta {
        name: String,
    }
    let set_id = RecordId::new("form_set", p.key.as_str());
    let mut sq = state
        .db
        .query("SELECT name FROM ONLY $s")
        .bind(("s", set_id))
        .await?;
    let set_meta: Option<SetMeta> = sq.take(0)?;
    let set_name = set_meta.map(|s| s.name).unwrap_or_default();

    let header = crate::controllers::common::build_app_header(&state, &user, "admin").await;

    let (picker_types, picker_sides, picker_conditions) = picker_choices(Some((
        &form.applies_types,
        &form.applies_sides,
        &form.applies_conditions,
    )));

    render(&crate::templates::AdminFormEditPage {
        app_name: &state.config.app_name,
        base_url: &state.config.base_url,
        signed_in: true,
        header,
        set_key: p.key,
        set_name,
        form_key: p.form_key,
        form_code: form.code,
        form_name: form.name,
        form_order: form.form_order,
        form_required: form.required,
        picker_types,
        picker_sides,
        picker_conditions,
    })
}

/// POST payload for the admin edit-form page. Same applicability
/// checkbox shape as `NewAdminFormInput`, plus the editable name /
/// order / required fields. `code` is intentionally absent — it's
/// shown read-only because downstream references key off it.
#[derive(Debug, Deserialize)]
pub struct EditAdminFormInput {
    pub name: String,
    #[serde(default)]
    pub form_order: Option<i64>,
    #[serde(default)]
    pub required: Option<String>,
    #[serde(default)]
    pub type_residential: Option<String>,
    #[serde(default)]
    pub type_commercial: Option<String>,
    #[serde(default)]
    pub type_vacant_lots_land: Option<String>,
    #[serde(default)]
    pub type_manufactured_home: Option<String>,
    #[serde(default)]
    pub type_business_opportunity: Option<String>,
    #[serde(default)]
    pub type_commercial_lease: Option<String>,
    #[serde(default)]
    pub type_rental_lease: Option<String>,
    #[serde(default)]
    pub side_listing: Option<String>,
    #[serde(default)]
    pub side_purchase: Option<String>,
    #[serde(default)]
    pub side_both: Option<String>,
    #[serde(default)]
    pub cond_none: Option<String>,
    #[serde(default)]
    pub cond_probate: Option<String>,
    #[serde(default)]
    pub cond_short_sale: Option<String>,
    #[serde(default)]
    pub cond_reo: Option<String>,
}

impl EditAdminFormInput {
    fn applies_picker(&self) -> AppliesPicker {
        AppliesPicker {
            type_residential: self.type_residential.clone(),
            type_commercial: self.type_commercial.clone(),
            type_vacant_lots_land: self.type_vacant_lots_land.clone(),
            type_manufactured_home: self.type_manufactured_home.clone(),
            type_business_opportunity: self.type_business_opportunity.clone(),
            type_commercial_lease: self.type_commercial_lease.clone(),
            type_rental_lease: self.type_rental_lease.clone(),
            side_listing: self.side_listing.clone(),
            side_purchase: self.side_purchase.clone(),
            side_both: self.side_both.clone(),
            cond_none: self.cond_none.clone(),
            cond_probate: self.cond_probate.clone(),
            cond_short_sale: self.cond_short_sale.clone(),
            cond_reo: self.cond_reo.clone(),
        }
    }
}

/// POST `/admin/forms/{set_key}/forms/{form_key}/edit` — persist
/// the picker selections + name/order/required for an existing
/// library form.
pub async fn admin_update_form(
    State(state): State<AppState>,
    SuperAdmin(_user): SuperAdmin,
    Path(p): Path<ToggleFormPath>,
    Form(input): Form<EditAdminFormInput>,
) -> Result<Redirect, AppError> {
    let set_id = RecordId::new("form_set", p.key.as_str());
    assert_forms_in_set(&state.db, &set_id, &[p.form_key.as_str()]).await?;

    let name = input.name.trim().to_string();
    if name.is_empty() {
        return Err(AppError::invalid("Form name is required."));
    }
    let required = matches!(input.required.as_deref(), Some("1" | "true" | "on" | "yes"));
    let (applies_types, applies_sides, applies_conditions) =
        applies_or_all(&input.applies_picker());
    let form_id = RecordId::new("form", p.form_key.as_str());
    state
        .db
        .query(
            "UPDATE $f SET name = $n, form_order = $o, required = $r, \
             applies_types = $at, applies_sides = $asd, applies_conditions = $ac",
        )
        .bind(("f", form_id))
        .bind(("n", name))
        .bind(("o", input.form_order.unwrap_or(500)))
        .bind(("r", required))
        .bind(("at", applies_types))
        .bind(("asd", applies_sides))
        .bind(("ac", applies_conditions))
        .await?;
    Ok(Redirect::to(&format!(
        "/admin/forms/{}?flash=form_updated",
        p.key
    )))
}
