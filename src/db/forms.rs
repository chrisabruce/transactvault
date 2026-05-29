//! Graph-backed checklist resolution.
//!
//! Replaces the in-memory `forms::build_default_checklist` lookup with a
//! traversal over the seeded form tables. Kept separate from the pure
//! `forms` module because this layer needs DB access.
//!
//! The parity test at the bottom proves the seeded California set +
//! this resolver reproduce the in-memory engine's output for every
//! `(type × sales × condition)` combination, so the eventual cutover is
//! provably behavior-preserving.

use anyhow::Context;
use serde::Deserialize;
use surrealdb::Surreal;
use surrealdb::engine::any::Any;
use surrealdb::types::{RecordId, SurrealValue};

/// One resolved checklist line — everything the seeder stored, plus the
/// group it lives in (name + order) pulled back through the inbound
/// `has_form` edge.
#[derive(Debug, Clone, Deserialize, SurrealValue)]
pub struct ResolvedForm {
    pub id: RecordId,
    pub code: String,
    pub name: String,
    pub description: String,
    pub includes: String,
    pub form_order: i64,
    pub required: bool,
    pub allows_multiple: bool,
    pub group_name: String,
    pub group_order: i64,
}

/// Record id of the seeded California state set, if present.
pub async fn california_set_id(db: &Surreal<Any>) -> anyhow::Result<Option<RecordId>> {
    let mut q = db
        .query(
            "SELECT VALUE id FROM form_set WHERE scope = 'state' AND name = 'California' LIMIT 1",
        )
        .await
        .context("looking up California form_set")?;
    let ids: Vec<RecordId> = q.take(0).unwrap_or_default();
    Ok(ids.into_iter().next())
}

/// Attach a brokerage to the default state form set (California) via a
/// `uses_state` edge, unless it already has one. Called at signup so a
/// brand-new brokerage resolves the California checklist out of the box.
/// A no-op (logged) if California hasn't been seeded yet.
pub async fn attach_default_state(
    db: &Surreal<Any>,
    brokerage_id: &RecordId,
) -> anyhow::Result<()> {
    let Some(ca) = california_set_id(db).await? else {
        tracing::warn!("no California form_set — skipping default uses_state edge");
        return Ok(());
    };
    db.query(
        "IF (SELECT VALUE id FROM uses_state WHERE in = $b LIMIT 1) = [] {
            RELATE $b->uses_state->$s
        }",
    )
    .bind(("b", brokerage_id.clone()))
    .bind(("s", ca))
    .await
    .context("attaching default uses_state edge")?;
    Ok(())
}

/// Resolve the forms a single form_set contributes for a given
/// transaction type / side / condition. Results are sorted by
/// (group_order, form_order) to match the printed-checklist sequence.
pub async fn resolve_for_set(
    db: &Surreal<Any>,
    set_id: &RecordId,
    type_str: &str,
    side_str: &str,
    cond_str: &str,
) -> anyhow::Result<Vec<ResolvedForm>> {
    let mut q = db
        .query(
            "SELECT
                id, code, name, description, includes, form_order, required, allows_multiple,
                (<-has_form<-form_group)[0].name AS group_name,
                (<-has_form<-form_group)[0].sort_order AS group_order
             FROM $set->has_group->form_group->has_form->form
             WHERE applies_types CONTAINS $t
               AND applies_sides CONTAINS $s
               AND applies_conditions CONTAINS $c
               AND is_active = true",
        )
        .bind(("set", set_id.clone()))
        .bind(("t", type_str.to_string()))
        .bind(("s", side_str.to_string()))
        .bind(("c", cond_str.to_string()))
        .await
        .context("resolving forms for set")?;
    let mut rows: Vec<ResolvedForm> = q.take(0)?;
    // Sort in Rust rather than ORDER BY on computed aliases, which is
    // brittle across SurrealDB versions.
    rows.sort_by(|a, b| {
        a.group_order
            .cmp(&b.group_order)
            .then(a.form_order.cmp(&b.form_order))
            .then(a.code.cmp(&b.code))
    });
    Ok(rows)
}

/// Resolve the full default checklist for a brokerage's transaction:
/// the forms its configured state + locality sets contribute for the
/// given (type, side, condition), minus any forms the broker has
/// hidden via `hides_form`. (Broker-authored custom forms via
/// `owns_form` join in with the builder UI in the next increment.)
///
/// Sorted by (group_order, form_order); local-set groups keep their own
/// names so the render layer can label them by locality.
pub async fn resolve_checklist(
    db: &Surreal<Any>,
    brokerage_id: &RecordId,
    type_str: &str,
    side_str: &str,
    cond_str: &str,
) -> anyhow::Result<Vec<ResolvedForm>> {
    // State set(s) first, then locality set(s) — concatenated so state
    // groups render above local ones.
    let mut sets_q = db
        .query(
            "RETURN array::concat(
                (SELECT VALUE out FROM uses_state WHERE in = $b),
                (SELECT VALUE out FROM uses_locality WHERE in = $b)
            )",
        )
        .bind(("b", brokerage_id.clone()))
        .await
        .context("loading brokerage form sets")?;
    let sets: Vec<RecordId> = sets_q.take(0).unwrap_or_default();

    // Forms the broker has explicitly hidden. A small Vec + `contains`
    // (rather than a HashSet) sidesteps clippy's `mutable_key_type`
    // lint on `RecordId` and is plenty for the handful of hides a
    // brokerage realistically sets.
    let mut hides_q = db
        .query("SELECT VALUE out FROM hides_form WHERE in = $b")
        .bind(("b", brokerage_id.clone()))
        .await
        .context("loading hidden forms")?;
    let hidden: Vec<RecordId> = hides_q.take(0).unwrap_or_default();

    let mut out: Vec<ResolvedForm> = Vec::new();
    for set in &sets {
        for f in resolve_for_set(db, set, type_str, side_str, cond_str).await? {
            if !hidden.contains(&f.id) {
                out.push(f);
            }
        }
    }

    // Brokerage-authored custom forms. These carry their group inline
    // (group_name / group_order) rather than through a set edge.
    let mut custom_q = db
        .query(
            "SELECT
                id, code, name, description, includes, form_order, required, allows_multiple,
                (group_name ?? 'Additional Disclosures') AS group_name,
                (group_order ?? 4) AS group_order
             FROM $b->owns_form->form
             WHERE applies_types CONTAINS $t
               AND applies_sides CONTAINS $s
               AND applies_conditions CONTAINS $c
               AND is_active = true",
        )
        .bind(("b", brokerage_id.clone()))
        .bind(("t", type_str.to_string()))
        .bind(("s", side_str.to_string()))
        .bind(("c", cond_str.to_string()))
        .await
        .context("resolving custom forms")?;
    let custom: Vec<ResolvedForm> = custom_q.take(0).unwrap_or_default();
    out.extend(custom.into_iter().filter(|f| !hidden.contains(&f.id)));

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forms;
    use crate::models::{SalesType, SpecialSalesCondition, TransactionType};
    use std::collections::BTreeSet;

    /// Prove the seeded California set + graph resolver reproduce the
    /// in-memory engine for every (type, sales, condition) combo. If
    /// this passes, the cutover in the next increment can't change any
    /// checklist's contents.
    #[tokio::test]
    async fn seed_resolution_matches_engine() {
        let db = surrealdb::engine::any::connect("mem://")
            .await
            .expect("connect mem");
        db.use_ns("test").use_db("test").await.expect("use ns/db");
        crate::db::apply_schema(&db).await.expect("apply schema");
        crate::db::seed_forms(&db).await.expect("seed");

        // California set id.
        let mut q = db
            .query("SELECT VALUE id FROM form_set WHERE name = 'California' LIMIT 1")
            .await
            .expect("query set");
        let ids: Vec<RecordId> = q.take(0).expect("take set id");
        let set_id = ids.into_iter().next().expect("California set exists");

        // Special-condition addenda whose group heading is allowed to
        // differ: the in-memory engine forces them into the *listing*
        // contract group on dual-side deals (a placement its own
        // comment calls "somewhat arbitrary"), but a form can only live
        // in one DB group, so a purchase-side addendum sensibly sits
        // under "Purchase Contracts". Membership + required must still
        // match exactly — only the heading is exempt for these codes.
        let arbitrary_group = ["PLA", "SSLA", "REOL", "PA", "SSA", "REO"];

        let mut mismatches: Vec<String> = Vec::new();

        for t in TransactionType::all() {
            for sales in SalesType::all() {
                let side = forms::sales_side(sales);
                for cond in SpecialSalesCondition::all() {
                    // Engine truth.
                    let engine_items = forms::build_default_checklist(t, cond, sales);
                    // Membership + required (exact).
                    let engine_membership: BTreeSet<(String, bool)> = engine_items
                        .iter()
                        .map(|d| (d.code.to_string(), d.required))
                        .collect();
                    // Group placement, excluding the arbitrary addenda.
                    let engine_groups: BTreeSet<(String, String)> = engine_items
                        .iter()
                        .filter(|d| !arbitrary_group.contains(&d.code))
                        .map(|d| (d.code.to_string(), d.group.seed_group().0.to_string()))
                        .collect();

                    // DB resolution for the same combo.
                    let resolved_rows =
                        resolve_for_set(&db, &set_id, t.as_str(), side.as_str(), cond.as_str())
                            .await
                            .expect("resolve");
                    let db_membership: BTreeSet<(String, bool)> = resolved_rows
                        .iter()
                        .map(|r| (r.code.clone(), r.required))
                        .collect();
                    let db_groups: BTreeSet<(String, String)> = resolved_rows
                        .iter()
                        .filter(|r| !arbitrary_group.contains(&r.code.as_str()))
                        .map(|r| (r.code.clone(), r.group_name.clone()))
                        .collect();

                    let combo = format!(
                        "type={} sales={} cond={}",
                        t.as_str(),
                        sales.as_str(),
                        cond.as_str()
                    );
                    if engine_membership != db_membership {
                        mismatches.push(format!(
                            "{combo} [membership]\n  only-engine: {:?}\n  only-db: {:?}",
                            engine_membership
                                .difference(&db_membership)
                                .collect::<Vec<_>>(),
                            db_membership
                                .difference(&engine_membership)
                                .collect::<Vec<_>>(),
                        ));
                    }
                    if engine_groups != db_groups {
                        mismatches.push(format!(
                            "{combo} [grouping]\n  only-engine: {:?}\n  only-db: {:?}",
                            engine_groups.difference(&db_groups).collect::<Vec<_>>(),
                            db_groups.difference(&engine_groups).collect::<Vec<_>>(),
                        ));
                    }
                }
            }
        }

        assert!(
            mismatches.is_empty(),
            "DB-resolved checklists diverge from the engine in {} case(s):\n{}",
            mismatches.len(),
            mismatches.join("\n")
        );
    }

    /// Brokerage-level resolution: a brokerage wired to California
    /// resolves the same set as the raw California set, and a
    /// `hides_form` edge removes exactly that one form.
    #[tokio::test]
    async fn brokerage_resolution_applies_state_and_hides() {
        let db = surrealdb::engine::any::connect("mem://")
            .await
            .expect("connect mem");
        db.use_ns("test").use_db("test").await.expect("use ns/db");
        crate::db::apply_schema(&db).await.expect("apply schema");
        crate::db::seed_forms(&db).await.expect("seed");

        // A brokerage wired to California.
        let mut bq = db
            .query("CREATE ONLY brokerage CONTENT { name: 'Test Brokerage', state: 'CA' }")
            .await
            .expect("create brokerage");
        let brokerage: Option<RecordId> = bq.take("id").expect("brokerage id");
        let brokerage = brokerage.expect("brokerage created");
        super::attach_default_state(&db, &brokerage)
            .await
            .expect("attach state");

        // Residential listing, no special condition.
        let (t, s, c) = ("residential", "listing", "none");
        let base = resolve_checklist(&db, &brokerage, t, s, c)
            .await
            .expect("resolve brokerage");
        assert!(
            base.iter().any(|f| f.code == "RLA"),
            "residential listing should include RLA"
        );

        // Hide RLA and confirm it disappears (and nothing else does).
        let rla = base.iter().find(|f| f.code == "RLA").unwrap().id.clone();
        db.query("RELATE $b->hides_form->$f")
            .bind(("b", brokerage.clone()))
            .bind(("f", rla))
            .await
            .expect("hide RLA");

        let after = resolve_checklist(&db, &brokerage, t, s, c)
            .await
            .expect("resolve after hide");
        assert!(
            !after.iter().any(|f| f.code == "RLA"),
            "hidden RLA should be gone"
        );
        assert_eq!(
            after.len(),
            base.len() - 1,
            "hiding one form removes exactly one line"
        );
    }
}
