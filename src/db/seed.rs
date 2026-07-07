//! Seeding + sync of the California master form set. Three phases, all
//! run at boot by [`seed_forms`]:
//!
//! 1. **Full seed** (`seed_california`) — only when no California
//!    `form_set` exists. Mirrors the in-memory `forms.rs` engine into
//!    the graph tables (`form_set` / `form_group` / `form` + edges).
//! 2. **Criteria sync** (`sync_engine_criteria`) — versioned migration.
//!    When the compiled checklist engine changes shape (new transaction
//!    types, restructured groups), existing DBs re-derive the
//!    engine-owned parts of the catalog: `applies_*` arrays are
//!    recomputed and missing (code, group) rows are created. Admin
//!    edits to names / required flags / active state on existing rows
//!    are left alone. Gated by `ENGINE_CRITERIA_VERSION` in the
//!    `seed_meta` table so it runs once per engine revision.
//! 3. **Catalog backfill** (`backfill_catalog`) — tops the set up with
//!    compiled-library forms it doesn't hold, as picker-only entries
//!    (empty applies). Exactly-once per code via the `seeded_form`
//!    ledger, so admin deletions stay deleted.
//!
//! Each form's `applies_types` / `applies_sides` / `applies_conditions`
//! criteria are derived by enumerating [`forms::build_default_checklist`]
//! over every `(transaction_type × sales_type × special_condition)`
//! combination and recording which combos include the form. The
//! enumeration keys by **(code, group)** rather than code alone: the
//! same form can print in different sections on different checklists
//! (CC&R/HOA live under Escrow Documents on sale checklists but under
//! Governing Documents on leases), and each placement becomes its own
//! `form` row with disjoint applicability.

use std::collections::{BTreeSet, HashMap};

use anyhow::Context;
use serde::Deserialize;
use surrealdb::Surreal;
use surrealdb::engine::any::Any;
use surrealdb::types::{RecordId, SurrealValue};

use crate::forms::{self, FormGroup};
use crate::models::{SalesType, SpecialSalesCondition, TransactionType};

/// Bump when the checklist engine changes in a way existing DBs must
/// pick up (new transaction types, moved forms, new groups). Each bump
/// re-runs [`sync_engine_criteria`] exactly once per database.
const ENGINE_CRITERIA_VERSION: i64 = 1;

#[derive(Debug, Deserialize, SurrealValue)]
struct CountRow {
    count: i64,
}

#[derive(Debug, Deserialize, SurrealValue)]
struct Created {
    id: RecordId,
}

/// Per-(code, group) accumulator built while enumerating the engine.
struct Acc {
    group: FormGroup,
    required: bool,
    types: BTreeSet<&'static str>,
    sides: BTreeSet<&'static str>,
    conditions: BTreeSet<&'static str>,
}

/// Enumerate every (type, sales, condition) combo and fold the
/// resulting checklist items into per-(code, group-name) criteria.
fn engine_criteria() -> HashMap<(&'static str, &'static str), Acc> {
    let mut acc: HashMap<(&'static str, &'static str), Acc> = HashMap::new();
    for t in TransactionType::all() {
        for sales in SalesType::all() {
            let side = forms::sales_side(sales);
            for cond in SpecialSalesCondition::all() {
                for item in forms::build_default_checklist(t, cond, sales) {
                    let (group_name, _) = item.group.seed_group();
                    let e = acc.entry((item.code, group_name)).or_insert_with(|| Acc {
                        group: item.group,
                        required: false,
                        types: BTreeSet::new(),
                        sides: BTreeSet::new(),
                        conditions: BTreeSet::new(),
                    });
                    e.required |= item.required;
                    e.types.insert(t.as_str());
                    e.sides.insert(side.as_str());
                    e.conditions.insert(cond.as_str());
                }
            }
        }
    }
    acc
}

/// Seed the California state form set if absent, sync engine-derived
/// criteria if the engine changed, then top up the picker catalog.
/// Order matters: the sync must run before the backfill so engine
/// forms are created with their real required flags + applicability,
/// not as inert picker-only entries.
pub async fn seed_forms(db: &Surreal<Any>) -> anyhow::Result<()> {
    let mut existing = db
        .query(
            "SELECT count() FROM form_set WHERE scope = 'state' AND name = 'California' GROUP ALL",
        )
        .await
        .context("checking for existing California form_set")?;
    let count: Option<CountRow> = existing.take(0).ok().flatten();
    if count.map(|c| c.count > 0).unwrap_or(false) {
        tracing::debug!("California form_set already present — skipping full seed");
    } else {
        seed_california(db).await?;
    }
    sync_engine_criteria(db).await?;
    backfill_catalog(db).await
}

/// Find (or create + relate) a group by display name within the set,
/// memoizing in `group_ids`. Groups can be missing on older DBs (the
/// lease/referral sections postdate the original seed) or after an
/// admin rename; recreating under the standard name gives the form a
/// home the admin can re-merge from Admin → Forms.
async fn ensure_group(
    db: &Surreal<Any>,
    set_id: &RecordId,
    group_ids: &mut HashMap<String, RecordId>,
    name: &str,
    sort_order: i64,
) -> anyhow::Result<RecordId> {
    if let Some(id) = group_ids.get(name) {
        return Ok(id.clone());
    }
    let created: Option<Created> = db
        .create("form_group")
        .content(NewFormGroup {
            name: name.to_string(),
            sort_order,
        })
        .await
        .context("creating form_group")?;
    let gid = created
        .ok_or_else(|| anyhow::anyhow!("form_group create returned nothing"))?
        .id;
    db.query("RELATE $s->has_group->$g")
        .bind(("s", set_id.clone()))
        .bind(("g", gid.clone()))
        .await
        .context("relating form_set->has_group->form_group")?;
    group_ids.insert(name.to_string(), gid.clone());
    Ok(gid)
}

/// Create one `form` row from engine criteria and relate it to its group.
async fn create_engine_form(
    db: &Surreal<Any>,
    group_id: &RecordId,
    code: &str,
    e: &Acc,
) -> anyhow::Result<()> {
    let meta = forms::lookup(code);
    let form: Option<Created> = db
        .create("form")
        .content(NewForm {
            code: code.to_string(),
            name: meta.map(|f| f.name).unwrap_or(code).to_string(),
            description: meta.map(|f| f.description).unwrap_or("").to_string(),
            includes: meta.map(|f| f.includes()).unwrap_or("").to_string(),
            form_order: forms::canonical_position(code) as i64,
            required: e.required,
            allows_multiple: meta.map(|f| f.allows_multiple).unwrap_or(false),
            applies_types: e.types.iter().map(|s| s.to_string()).collect(),
            applies_sides: e.sides.iter().map(|s| s.to_string()).collect(),
            applies_conditions: e.conditions.iter().map(|s| s.to_string()).collect(),
            is_active: true,
        })
        .await
        .with_context(|| format!("creating form {code}"))?;
    let form_id = form
        .ok_or_else(|| anyhow::anyhow!("form create returned nothing"))?
        .id;
    db.query("RELATE $g->has_form->$f")
        .bind(("g", group_id.clone()))
        .bind(("f", form_id))
        .await
        .context("relating form_group->has_form->form")?;
    Ok(())
}

/// One-time full seed of the California set from the default-checklist
/// engine.
async fn seed_california(db: &Surreal<Any>) -> anyhow::Result<()> {
    tracing::info!("seeding California master form set from the in-memory library");

    let acc = engine_criteria();

    let set: Option<Created> = db
        .create("form_set")
        .content(NewFormSet {
            scope: "state".to_string(),
            name: "California".to_string(),
            is_active: true,
        })
        .await
        .context("creating California form_set")?;
    let set_id = set
        .ok_or_else(|| anyhow::anyhow!("form_set create returned nothing"))?
        .id;

    // Create the groups actually used, in sort order for determinism.
    let seen_groups: BTreeSet<&'static str> =
        acc.values().map(|e| e.group.seed_group().0).collect();
    let mut ordered: Vec<(&'static str, i64)> =
        FormGroup::ORDERED.iter().map(|g| g.seed_group()).collect();
    ordered.sort_by_key(|(_, o)| *o);
    ordered.dedup_by_key(|(name, _)| *name);
    let mut group_ids: HashMap<String, RecordId> = HashMap::new();
    for (name, sort_order) in ordered {
        if seen_groups.contains(name) {
            ensure_group(db, &set_id, &mut group_ids, name, sort_order).await?;
        }
    }

    for ((code, group_name), e) in &acc {
        let (_, sort_order) = e.group.seed_group();
        let group_id = ensure_group(db, &set_id, &mut group_ids, group_name, sort_order).await?;
        create_engine_form(db, &group_id, code, e).await?;
    }

    // A fresh seed already reflects the current engine — record the
    // criteria version so the sync pass doesn't redo ~100 updates.
    db.query(
        "DELETE seed_meta WHERE key = 'engine_criteria_version'; \
         CREATE seed_meta SET key = 'engine_criteria_version', value = $v;",
    )
    .bind(("v", ENGINE_CRITERIA_VERSION))
    .await
    .context("recording engine criteria version")?;

    tracing::info!(
        forms = acc.len(),
        groups = group_ids.len(),
        "California form set seeded"
    );
    Ok(())
}

/// Versioned re-sync of the engine-owned parts of the catalog. For
/// every engine-derived (code, group) pair: update the matching row's
/// `applies_*` arrays (they decide WHICH checklists a form appears on
/// — the fix that moves lease transactions off the old sale
/// checklists), or create the row when the placement is new (e.g.
/// CC&R's second home under Governing Documents, the lease and
/// referral contract sections). Everything an admin can edit that the
/// engine doesn't own — name, required, active state, order, plus any
/// admin-added or custom forms — is untouched.
async fn sync_engine_criteria(db: &Surreal<Any>) -> anyhow::Result<()> {
    let Some(set_id) = super::forms::california_set_id(db).await? else {
        tracing::warn!("no California form_set — skipping engine criteria sync");
        return Ok(());
    };

    let mut vq = db
        .query("SELECT VALUE value FROM seed_meta WHERE key = 'engine_criteria_version' LIMIT 1")
        .await
        .context("loading engine criteria version")?;
    let stored: Vec<i64> = vq.take(0).unwrap_or_default();
    let stored = stored.into_iter().next().unwrap_or(0);
    if stored >= ENGINE_CRITERIA_VERSION {
        return Ok(());
    }
    tracing::info!(
        from = stored,
        to = ENGINE_CRITERIA_VERSION,
        "syncing engine-derived form criteria into the California set"
    );

    #[derive(Debug, Deserialize, SurrealValue)]
    struct ExistingRow {
        id: RecordId,
        code: String,
        group_name: Option<String>,
    }
    let mut rq = db
        .query(
            "SELECT id, code, (<-has_form<-form_group)[0].name AS group_name \
             FROM $s->has_group->form_group->has_form->form",
        )
        .bind(("s", set_id.clone()))
        .await
        .context("loading existing set forms")?;
    let rows: Vec<ExistingRow> = rq.take(0).unwrap_or_default();
    let existing: HashMap<(String, String), RecordId> = rows
        .into_iter()
        .filter_map(|r| {
            r.group_name
                .map(|g| ((r.code.to_ascii_uppercase(), g), r.id))
        })
        .collect();

    #[derive(Debug, Deserialize, SurrealValue)]
    struct GroupRow {
        id: RecordId,
        name: String,
    }
    let mut gq = db
        .query("SELECT id, name FROM $s->has_group->form_group")
        .bind(("s", set_id.clone()))
        .await
        .context("loading set groups")?;
    let group_rows: Vec<GroupRow> = gq.take(0).unwrap_or_default();
    let mut group_ids: HashMap<String, RecordId> =
        group_rows.into_iter().map(|g| (g.name, g.id)).collect();

    let mut updated = 0usize;
    let mut created = 0usize;
    for ((code, group_name), e) in &engine_criteria() {
        match existing.get(&(code.to_ascii_uppercase(), (*group_name).to_string())) {
            Some(fid) => {
                db.query(
                    "UPDATE $f SET applies_types = $t, applies_sides = $s, \
                     applies_conditions = $c",
                )
                .bind(("f", fid.clone()))
                .bind((
                    "t",
                    e.types.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
                ))
                .bind((
                    "s",
                    e.sides.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
                ))
                .bind((
                    "c",
                    e.conditions
                        .iter()
                        .map(|s| s.to_string())
                        .collect::<Vec<_>>(),
                ))
                .await
                .with_context(|| format!("syncing criteria for {code}"))?;
                updated += 1;
            }
            None => {
                let (_, sort_order) = e.group.seed_group();
                let group_id =
                    ensure_group(db, &set_id, &mut group_ids, group_name, sort_order).await?;
                create_engine_form(db, &group_id, code, e).await?;
                created += 1;
            }
        }
    }

    db.query(
        "DELETE seed_meta WHERE key = 'engine_criteria_version'; \
         CREATE seed_meta SET key = 'engine_criteria_version', value = $v;",
    )
    .bind(("v", ENGINE_CRITERIA_VERSION))
    .await
    .context("recording engine criteria version")?;

    tracing::info!(updated, created, "engine criteria sync complete");
    Ok(())
}

/// Top the California set up with every compiled-library form it
/// doesn't hold yet — as picker-only entries with EMPTY `applies_*`
/// arrays, so they never land on a default checklist until an admin
/// deliberately broadens them from Admin → Forms. This is what makes
/// the whole CAR catalog manageable from the admin UI (and visible in
/// the Add-an-item picker) without a deploy.
///
/// Exactly-once semantics via the `seeded_form` ledger: each code is
/// auto-added at most once EVER. A form an admin deletes from
/// Admin → Forms stays deleted across restarts, while a code newly
/// added to the compiled library still arrives on its first boot.
/// Runs on every startup; once the catalog is fully ledgered it's a
/// no-op.
async fn backfill_catalog(db: &Surreal<Any>) -> anyhow::Result<()> {
    let Some(set_id) = super::forms::california_set_id(db).await? else {
        tracing::warn!("no California form_set — skipping catalog backfill");
        return Ok(());
    };

    let mut ledger_q = db
        .query("SELECT VALUE code FROM seeded_form")
        .await
        .context("loading seeded_form ledger")?;
    let ledgered: Vec<String> = ledger_q.take(0).unwrap_or_default();
    let ledgered: BTreeSet<String> = ledgered
        .into_iter()
        .map(|c| c.to_ascii_uppercase())
        .collect();

    let mut present_q = db
        .query("SELECT VALUE code FROM $s->has_group->form_group->has_form->form")
        .bind(("s", set_id.clone()))
        .await
        .context("loading existing set codes")?;
    let present: Vec<String> = present_q.take(0).unwrap_or_default();
    let present: BTreeSet<String> = present
        .into_iter()
        .map(|c| c.to_ascii_uppercase())
        .collect();

    #[derive(Debug, Deserialize, SurrealValue)]
    struct GroupRow {
        id: RecordId,
        name: String,
    }
    let mut groups_q = db
        .query("SELECT id, name FROM $s->has_group->form_group")
        .bind(("s", set_id.clone()))
        .await
        .context("loading set groups")?;
    let group_rows: Vec<GroupRow> = groups_q.take(0).unwrap_or_default();
    let mut group_ids: HashMap<String, RecordId> =
        group_rows.into_iter().map(|g| (g.name, g.id)).collect();

    let mut added = 0usize;
    let mut newly_ledgered: Vec<String> = Vec::new();
    for f in forms::LIBRARY {
        let code_uc = f.code.to_ascii_uppercase();
        if ledgered.contains(&code_uc) {
            continue;
        }

        if !present.contains(&code_uc) {
            let (group_name, sort_order) = forms::infer_group_from_code(f.code).seed_group();
            let group_id =
                ensure_group(db, &set_id, &mut group_ids, group_name, sort_order).await?;
            let form: Option<Created> = db
                .create("form")
                .content(NewForm {
                    code: f.code.to_string(),
                    name: f.name.to_string(),
                    description: f.description.to_string(),
                    includes: f.includes().to_string(),
                    form_order: forms::canonical_position(f.code) as i64,
                    required: false,
                    allows_multiple: f.allows_multiple,
                    applies_types: Vec::new(),
                    applies_sides: Vec::new(),
                    applies_conditions: Vec::new(),
                    is_active: true,
                })
                .await
                .with_context(|| format!("backfilling form {}", f.code))?;
            let form_id = form
                .ok_or_else(|| anyhow::anyhow!("form create returned nothing"))?
                .id;
            db.query("RELATE $g->has_form->$f")
                .bind(("g", group_id))
                .bind(("f", form_id))
                .await
                .context("relating backfilled form")?;
            added += 1;
        }

        // Ledger the code whether we just created it or it was already
        // present (original seed or a manual admin add) — either way it
        // must never be auto-added again. Recording happens after the
        // create so a crash mid-backfill re-runs cleanly: the
        // `present` check stops duplicates, then the ledger lands.
        newly_ledgered.push(code_uc);
    }

    if !newly_ledgered.is_empty() {
        db.query("FOR $c IN $codes { CREATE seeded_form SET code = $c }")
            .bind(("codes", newly_ledgered))
            .await
            .context("recording seeded_form ledger entries")?;
    }
    if added > 0 {
        tracing::info!(
            added,
            "catalog backfill: added compiled-library forms to the California set"
        );
    }
    Ok(())
}

#[derive(serde::Serialize, SurrealValue)]
struct NewFormSet {
    scope: String,
    name: String,
    is_active: bool,
}

#[derive(serde::Serialize, SurrealValue)]
struct NewFormGroup {
    name: String,
    sort_order: i64,
}

#[derive(serde::Serialize, SurrealValue)]
struct NewForm {
    code: String,
    name: String,
    description: String,
    includes: String,
    form_order: i64,
    required: bool,
    allows_multiple: bool,
    applies_types: Vec<String>,
    applies_sides: Vec<String>,
    applies_conditions: Vec<String>,
    is_active: bool,
}
