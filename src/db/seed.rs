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

/// Seed the California state form set if it isn't present yet.
pub async fn seed_forms(db: &Surreal<Any>) -> anyhow::Result<()> {
    let mut existing = db
        .query(
            "SELECT count() FROM form_set WHERE scope = 'state' AND name = 'California' GROUP ALL",
        )
        .await
        .context("checking for existing California form_set")?;
    let count: Option<CountRow> = existing.take(0).ok().flatten();
    if count.map(|c| c.count > 0).unwrap_or(false) {
        tracing::debug!("California form_set already present — skipping seed");
        return Ok(());
    }

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
