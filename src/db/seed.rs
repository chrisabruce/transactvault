//! One-time seed of the California master form set.
//!
//! Mirrors the in-memory `forms.rs` engine into the graph tables
//! (`form_set` / `form_group` / `form` + edges) so the DB reproduces
//! today's checklists exactly. Runs on boot but is **seed-once**: if a
//! California `form_set` already exists we leave it alone, so admin
//! edits made through the (future) management UI are never clobbered.
//!
//! Each form's `applies_types` / `applies_sides` / `applies_conditions`
//! criteria are derived by enumerating [`forms::build_default_checklist`]
//! over every `(transaction_type × sales_type × special_condition)`
//! combination and recording which combos include the form. The
//! resolution query in the next epic increment matches against these
//! arrays — `CONTAINS type AND CONTAINS side AND CONTAINS condition` —
//! which is exactly what the in-memory engine computes.

use std::collections::{BTreeSet, HashMap};

use anyhow::Context;
use serde::Deserialize;
use surrealdb::Surreal;
use surrealdb::engine::any::Any;
use surrealdb::types::{RecordId, SurrealValue};

use crate::forms::{self, FormGroup};
use crate::models::{SalesType, SpecialSalesCondition, TransactionType};

#[derive(Debug, Deserialize, SurrealValue)]
struct CountRow {
    count: i64,
}

#[derive(Debug, Deserialize, SurrealValue)]
struct Created {
    id: RecordId,
}

/// Per-form accumulator built while enumerating the engine.
#[derive(Default)]
struct Acc {
    group: Option<FormGroup>,
    required: bool,
    types: BTreeSet<&'static str>,
    sides: BTreeSet<&'static str>,
    conditions: BTreeSet<&'static str>,
}

/// Seed the California state form set if it isn't present yet, then
/// top it up with any compiled-catalog forms it's missing (see
/// [`backfill_catalog`]). The full seed remains seed-once so admin
/// edits are never clobbered; the backfill is exactly-once **per
/// code** via the `seeded_form` ledger.
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
    backfill_catalog(db).await
}

/// One-time full seed of the California set from the default-checklist
/// engine — every form that appears on some default checklist, with
/// its derived `applies_*` criteria.
async fn seed_california(db: &Surreal<Any>) -> anyhow::Result<()> {
    tracing::info!("seeding California master form set from the in-memory library");

    // 1. Enumerate every (type, sales, condition) combo and fold the
    //    resulting checklist items into a per-code accumulator.
    let mut acc: HashMap<&'static str, Acc> = HashMap::new();
    for t in TransactionType::all() {
        for sales in SalesType::all() {
            let side = forms::sales_side(sales);
            for cond in SpecialSalesCondition::all() {
                for item in forms::build_default_checklist(t, cond, sales) {
                    let e = acc.entry(item.code).or_default();
                    e.group = Some(item.group);
                    e.required |= item.required;
                    e.types.insert(t.as_str());
                    e.sides.insert(side.as_str());
                    e.conditions.insert(cond.as_str());
                }
            }
        }
    }

    // 2. Create the California state set.
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

    // 3. Create the groups actually used, RELATE them to the set, and
    //    remember each group's record id by name.
    let mut group_ids: HashMap<&'static str, RecordId> = HashMap::new();
    let mut seen_groups: BTreeSet<&'static str> = BTreeSet::new();
    for e in acc.values() {
        if let Some(g) = e.group {
            seen_groups.insert(g.seed_group().0);
        }
    }
    // Deterministic order: sort by the group's sort_order.
    let mut ordered: Vec<(&'static str, i64)> =
        FormGroup::ORDERED.iter().map(|g| g.seed_group()).collect();
    ordered.sort_by_key(|(_, o)| *o);
    ordered.dedup_by_key(|(name, _)| *name);
    for (name, sort_order) in ordered {
        if !seen_groups.contains(name) {
            continue;
        }
        let grp: Option<Created> = db
            .create("form_group")
            .content(NewFormGroup {
                name: name.to_string(),
                sort_order,
            })
            .await
            .context("creating form_group")?;
        let grp_id = grp
            .ok_or_else(|| anyhow::anyhow!("form_group create returned nothing"))?
            .id;
        db.query("RELATE $s->has_group->$g")
            .bind(("s", set_id.clone()))
            .bind(("g", grp_id.clone()))
            .await
            .context("relating form_set->has_group->form_group")?;
        group_ids.insert(name, grp_id);
    }

    // 4. Create each form, RELATE it to its group.
    for (code, e) in &acc {
        let Some(group) = e.group else { continue };
        let (group_name, _) = group.seed_group();
        let Some(group_id) = group_ids.get(group_name) else {
            continue;
        };

        let meta = forms::lookup(code);
        let name = meta.map(|f| f.name).unwrap_or(code);
        let description = meta.map(|f| f.description).unwrap_or("");
        let includes = meta.map(|f| f.includes()).unwrap_or("");
        let allows_multiple = meta.map(|f| f.allows_multiple).unwrap_or(false);

        let form: Option<Created> = db
            .create("form")
            .content(NewForm {
                code: code.to_string(),
                name: name.to_string(),
                description: description.to_string(),
                includes: includes.to_string(),
                form_order: forms::canonical_position(code) as i64,
                required: e.required,
                allows_multiple,
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
    }

    tracing::info!(
        forms = acc.len(),
        groups = group_ids.len(),
        "California form set seeded"
    );
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
            let group_id = match group_ids.get(group_name) {
                Some(id) => id.clone(),
                None => {
                    // An admin renamed or deleted this standard group.
                    // Recreate it so the form has a home — it shows up
                    // in Admin → Forms where the admin can merge or
                    // rename it again.
                    let created: Option<Created> = db
                        .create("form_group")
                        .content(NewFormGroup {
                            name: group_name.to_string(),
                            sort_order,
                        })
                        .await
                        .context("recreating group for catalog backfill")?;
                    let gid = created
                        .ok_or_else(|| anyhow::anyhow!("form_group create returned nothing"))?
                        .id;
                    db.query("RELATE $s->has_group->$g")
                        .bind(("s", set_id.clone()))
                        .bind(("g", gid.clone()))
                        .await
                        .context("relating recreated group")?;
                    group_ids.insert(group_name.to_string(), gid.clone());
                    gid
                }
            };

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
