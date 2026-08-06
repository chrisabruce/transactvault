//! Operational controls: maintenance mode and the scheduled-maintenance
//! notice. Super-admin only, under `/admin/ops`.
//!
//! The live values sit in [`crate::state::Ops`] (process memory) so the
//! maintenance gate keeps answering while the database is being restored
//! or moved. Every toggle here mirrors the new state into the
//! `system_setting:main` row, best-effort, so a restart with a healthy
//! database resumes where the admin left things.

use axum::Form;
use axum::extract::State;
use axum::response::{Html, IntoResponse, Redirect, Response};
use serde::Deserialize;
use surrealdb::types::{RecordId, SurrealValue};

use crate::audit;
use crate::auth::middleware::SuperAdmin;
use crate::controllers::render;
use crate::error::AppError;
use crate::state::AppState;
use crate::templates::AdminOpsPage;

/// Shape of the `system_setting:main` row.
#[derive(Debug, Clone, Default, Deserialize, serde::Serialize, SurrealValue)]
struct SettingRow {
    #[serde(default)]
    maintenance_mode: bool,
    #[serde(default)]
    maintenance_notice: Option<String>,
}

fn setting_id() -> RecordId {
    RecordId::new("system_setting", "main")
}

/// Load persisted switches into [`crate::state::Ops`] at boot.
///
/// `MAINTENANCE_MODE=true` in the environment wins over the stored row:
/// that's the switch an operator sets while moving servers, when the
/// database (and therefore the row) may not exist yet. Failure to read
/// the row is quietly tolerated for the same reason.
pub async fn bootstrap(state: &AppState) {
    let row: Option<SettingRow> = state.db.select(setting_id()).await.ok().flatten();
    let row = row.unwrap_or_default();

    let on = state.config.maintenance_mode || row.maintenance_mode;
    state.ops.set_maintenance(on);
    state.ops.set_notice(row.maintenance_notice.clone());

    if on {
        tracing::warn!(
            from_env = state.config.maintenance_mode,
            from_db = row.maintenance_mode,
            "booting in MAINTENANCE MODE — app routes answer 503"
        );
    }
}

/// Mirror the current in-memory switches into `system_setting:main`.
/// Best-effort: a failed write is logged, never surfaced — the live gate
/// already changed, which is what the admin asked for.
async fn persist(state: &AppState) {
    let row = SettingRow {
        maintenance_mode: state.ops.maintenance_on(),
        maintenance_notice: state.ops.notice(),
    };
    let result = state
        .db
        .query("UPSERT $id SET maintenance_mode = $on, maintenance_notice = $notice")
        .bind(("id", setting_id()))
        .bind(("on", row.maintenance_mode))
        .bind(("notice", row.maintenance_notice))
        .await;
    if let Err(e) = result {
        tracing::warn!(error = %e, "failed to persist system settings (in-memory state already applied)");
    }
}

/// `GET /admin/ops` — the switchboard page.
pub async fn page(
    State(state): State<AppState>,
    SuperAdmin(user): SuperAdmin,
    axum::extract::Query(q): axum::extract::Query<OpsQuery>,
) -> Result<Html<String>, AppError> {
    let header = crate::controllers::common::build_app_header(&state, &user, "admin").await;
    render(&AdminOpsPage {
        app_name: &state.config.app_name,
        base_url: &state.config.base_url,
        signed_in: true,
        header,
        maintenance_on: state.ops.maintenance_on(),
        notice: state.ops.notice().unwrap_or_default(),
        flash: q.flash,
        env_forced: state.config.maintenance_mode,
    })
}

#[derive(Debug, Deserialize)]
pub struct OpsQuery {
    #[serde(default)]
    pub flash: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct MaintenanceForm {
    /// `"on"` to enable, anything else disables.
    #[serde(default)]
    pub set: String,
}

/// `POST /admin/ops/maintenance` — flip the gate.
pub async fn set_maintenance(
    State(state): State<AppState>,
    SuperAdmin(user): SuperAdmin,
    Form(form): Form<MaintenanceForm>,
) -> Result<Response, AppError> {
    let on = form.set == "on";
    state.ops.set_maintenance(on);
    persist(&state).await;

    audit::record(
        &state.db,
        if on {
            "maintenance_enabled"
        } else {
            "maintenance_disabled"
        },
        Some(user.user_id.clone()),
        Some(user.email.clone()),
        None,
        None,
        None,
    )
    .await;
    tracing::warn!(on, actor = %user.email, "maintenance mode toggled");

    let flash = if on {
        "Maintenance mode is ON. Visitors now see the maintenance page."
    } else {
        "Maintenance mode is off. The app is live again."
    };
    Ok(Redirect::to(&format!("/admin/ops?flash={}", urlencode(flash))).into_response())
}

#[derive(Debug, Deserialize)]
pub struct NoticeForm {
    #[serde(default)]
    pub notice: String,
}

/// `POST /admin/ops/notice` — set or clear the heads-up banner that
/// signed-in users see on every page. An empty submission clears it.
pub async fn set_notice(
    State(state): State<AppState>,
    SuperAdmin(user): SuperAdmin,
    Form(form): Form<NoticeForm>,
) -> Result<Response, AppError> {
    let trimmed = form.notice.trim();
    if trimmed.chars().count() > 300 {
        return Err(AppError::invalid(
            "Keep the notice under 300 characters. It's a banner, not a memo.",
        ));
    }

    let (value, kind, flash) = if trimmed.is_empty() {
        (None, "maintenance_notice_cleared", "Notice cleared.")
    } else {
        (
            Some(crate::sanitize::scrub_line(trimmed, 300)),
            "maintenance_notice_set",
            "Notice saved. Signed-in users will see it on every page.",
        )
    };

    state.ops.set_notice(value.clone());
    persist(&state).await;

    audit::record(
        &state.db,
        kind,
        Some(user.user_id.clone()),
        Some(user.email.clone()),
        None,
        None,
        value,
    )
    .await;

    Ok(Redirect::to(&format!("/admin/ops?flash={}", urlencode(flash))).into_response())
}

/// Minimal percent-encoding for the flash messages above — they are
/// static strings, so only spaces and punctuation actually occur.
fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Storage cleanup — find and remove orphaned objects
// ---------------------------------------------------------------------------

use std::collections::{HashMap, HashSet};

use humansize::{DECIMAL, format_size};

/// Objects younger than this are never shown or deleted: an upload can
/// be mid-flight (browser PUT done, finalize not yet arrived), and a
/// day of slack costs nothing next to deleting someone's disclosure.
const FRESH_GUARD_HOURS: i64 = 24;

/// One orphaned object, shaped for the template.
#[derive(Debug, Clone, serde::Serialize)]
pub struct OrphanView {
    pub key: String,
    pub size: String,
    pub size_bytes: u64,
    pub age: String,
    /// Resolved owner: brokerage name, user email, or "unknown".
    pub owner: String,
    /// What the key shape says it was: Document / Avatar / Export
    /// archive / Abandoned upload.
    pub kind: String,
}

pub struct StorageScan {
    pub total_objects: usize,
    pub total_size: String,
    pub referenced: usize,
    pub orphans: Vec<OrphanView>,
    pub orphan_size: String,
    pub fresh_skipped: usize,
    pub inflight: usize,
    pub error: Option<String>,
}

/// Every storage key the database can explain, plus how many pending
/// uploads are fresh enough to leave alone.
async fn referenced_keys(state: &AppState) -> anyhow::Result<(HashSet<String>, usize)> {
    let mut referenced: HashSet<String> = HashSet::new();

    let mut q = state
        .db
        .query("SELECT VALUE storage_key FROM document")
        .await?;
    let docs: Vec<String> = q.take(0).unwrap_or_default();
    referenced.extend(docs);

    let mut q = state
        .db
        .query("SELECT VALUE avatar_storage_key FROM user WHERE avatar_storage_key != NONE")
        .await?;
    let avatars: Vec<String> = q.take(0).unwrap_or_default();
    referenced.extend(avatars);

    let mut q = state
        .db
        .query("SELECT VALUE storage_key FROM export_chunk")
        .await?;
    let chunks: Vec<String> = q.take(0).unwrap_or_default();
    referenced.extend(chunks);

    // Fresh pending uploads are in-flight, not orphans. Stale ones are
    // exactly what this page exists to sweep: the browser PUT the bytes
    // and the finalize call never came.
    #[derive(serde::Deserialize, SurrealValue)]
    struct Pending {
        storage_key: String,
        created_at: chrono::DateTime<chrono::Utc>,
    }
    let mut q = state
        .db
        .query("SELECT storage_key, created_at FROM pending_upload")
        .await?;
    let pending: Vec<Pending> = q.take(0).unwrap_or_default();
    let cutoff = chrono::Utc::now() - chrono::Duration::hours(FRESH_GUARD_HOURS);
    let mut inflight = 0usize;
    for p in pending {
        if p.created_at > cutoff {
            referenced.insert(p.storage_key);
            inflight += 1;
        }
    }

    Ok((referenced, inflight))
}

/// Name lookups for the "belongs to" column. Both tables are small at
/// our scale; two full scans beat N per-row queries.
async fn owner_maps(state: &AppState) -> (HashMap<String, String>, HashMap<String, String>) {
    #[derive(serde::Deserialize, SurrealValue)]
    struct Named {
        id: RecordId,
        name: String,
    }
    #[derive(serde::Deserialize, SurrealValue)]
    struct Mailed {
        id: RecordId,
        email: String,
    }

    let brokerages: HashMap<String, String> =
        match state.db.query("SELECT id, name FROM brokerage").await {
            Ok(mut q) => q
                .take::<Vec<Named>>(0)
                .unwrap_or_default()
                .into_iter()
                .map(|b| (crate::db::record_key(&b.id), b.name))
                .collect(),
            Err(_) => HashMap::new(),
        };
    let users: HashMap<String, String> = match state.db.query("SELECT id, email FROM user").await {
        Ok(mut q) => q
            .take::<Vec<Mailed>>(0)
            .unwrap_or_default()
            .into_iter()
            .map(|u| (crate::db::record_key(&u.id), u.email))
            .collect(),
        Err(_) => HashMap::new(),
    };
    (brokerages, users)
}

fn classify(
    key: &str,
    brokerages: &HashMap<String, String>,
    users: &HashMap<String, String>,
) -> (String, String) {
    if let Some(rest) = key.strip_prefix("avatars/") {
        let user_key = rest.trim_end_matches(".png");
        let owner = users
            .get(user_key)
            .cloned()
            .unwrap_or_else(|| "deleted user".to_string());
        return ("Avatar".to_string(), owner);
    }
    if key.starts_with("exports/") {
        let owner = key
            .split('/')
            .find_map(|seg| brokerages.get(seg))
            .cloned()
            .unwrap_or_else(|| "deleted brokerage".to_string());
        return ("Export archive".to_string(), owner);
    }
    // Document layout: {brokerage}/{property}/{form}/{file}
    let first = key.split('/').next().unwrap_or_default();
    let owner = brokerages
        .get(first)
        .cloned()
        .unwrap_or_else(|| "deleted brokerage".to_string());
    ("Document".to_string(), owner)
}

/// Diff the bucket against the database. The expensive half is the
/// bucket LIST; the queries are all indexed or tiny.
async fn scan_storage(state: &AppState) -> StorageScan {
    let objects = match state.storage.list_all().await {
        Ok(objs) => objs,
        Err(e) => {
            return StorageScan {
                total_objects: 0,
                total_size: "0 B".into(),
                referenced: 0,
                orphans: Vec::new(),
                orphan_size: "0 B".into(),
                fresh_skipped: 0,
                inflight: 0,
                error: Some(format!("Couldn't list the storage bucket: {e}")),
            };
        }
    };

    let (referenced, inflight) = match referenced_keys(state).await {
        Ok(r) => r,
        Err(e) => {
            return StorageScan {
                total_objects: objects.len(),
                total_size: format_size(objects.iter().map(|o| o.size).sum::<u64>(), DECIMAL),
                referenced: 0,
                orphans: Vec::new(),
                orphan_size: "0 B".into(),
                fresh_skipped: 0,
                inflight: 0,
                error: Some(format!("Couldn't read reference tables: {e}")),
            };
        }
    };

    let (brokerages, users) = owner_maps(state).await;
    let cutoff = chrono::Utc::now() - chrono::Duration::hours(FRESH_GUARD_HOURS);

    let total_objects = objects.len();
    let total_bytes: u64 = objects.iter().map(|o| o.size).sum();
    let mut orphans = Vec::new();
    let mut orphan_bytes = 0u64;
    let mut fresh_skipped = 0usize;
    let mut referenced_count = 0usize;

    for obj in objects {
        if referenced.contains(&obj.key) {
            referenced_count += 1;
            continue;
        }
        // Fail safe on freshness: unknown age counts as fresh.
        let old_enough = obj.last_modified.map(|t| t < cutoff).unwrap_or(false);
        if !old_enough {
            fresh_skipped += 1;
            continue;
        }
        let (kind, owner) = classify(&obj.key, &brokerages, &users);
        let age = obj
            .last_modified
            .map(|t| t.format("%b %-d, %Y").to_string())
            .unwrap_or_else(|| "unknown".to_string());
        orphan_bytes += obj.size;
        orphans.push(OrphanView {
            key: obj.key,
            size: format_size(obj.size, DECIMAL),
            size_bytes: obj.size,
            age,
            owner,
            kind,
        });
    }
    // Biggest wins first.
    orphans.sort_by_key(|o| std::cmp::Reverse(o.size_bytes));

    StorageScan {
        total_objects,
        total_size: format_size(total_bytes, DECIMAL),
        referenced: referenced_count,
        orphans,
        orphan_size: format_size(orphan_bytes, DECIMAL),
        fresh_skipped,
        inflight,
        error: None,
    }
}

/// `GET /admin/storage` — run the scan and show the results.
pub async fn storage_page(
    State(state): State<AppState>,
    SuperAdmin(user): SuperAdmin,
    axum::extract::Query(q): axum::extract::Query<OpsQuery>,
) -> Result<Html<String>, AppError> {
    let scan = scan_storage(&state).await;
    let header = crate::controllers::common::build_app_header(&state, &user, "admin").await;
    render(&crate::templates::AdminStoragePage {
        app_name: &state.config.app_name,
        base_url: &state.config.base_url,
        signed_in: true,
        header,
        scan,
        flash: q.flash,
    })
}

/// Delete orphans. With `key`, exactly that one; without, every orphan
/// from a FRESH scan. Either way the reference check happens at delete
/// time, so a page that sat open for an hour can't delete an object a
/// finalize call claimed in the meantime.
async fn delete_orphans(state: &AppState, only_key: Option<String>) -> (usize, u64, usize) {
    let scan = scan_storage(state).await;
    let targets: Vec<&OrphanView> = match &only_key {
        Some(k) => scan.orphans.iter().filter(|o| &o.key == k).collect(),
        None => scan.orphans.iter().collect(),
    };

    let mut deleted = 0usize;
    let mut bytes = 0u64;
    let mut failed = 0usize;
    for orphan in targets {
        match state.storage.delete(&orphan.key).await {
            Ok(()) => {
                deleted += 1;
                bytes += orphan.size_bytes;
                // Drop any stale pending_upload row that pointed here,
                // so the table shrinks along with the bucket.
                let _ = state
                    .db
                    .query("DELETE pending_upload WHERE storage_key = $k")
                    .bind(("k", orphan.key.clone()))
                    .await;
            }
            Err(e) => {
                failed += 1;
                tracing::warn!(key = %orphan.key, error = %e, "orphan delete failed");
            }
        }
    }
    (deleted, bytes, failed)
}

/// `POST /admin/storage/delete-all` — sweep every orphan.
pub async fn storage_delete_all(
    State(state): State<AppState>,
    SuperAdmin(user): SuperAdmin,
) -> Result<Response, AppError> {
    let (deleted, bytes, failed) = delete_orphans(&state, None).await;

    audit::record(
        &state.db,
        "storage_orphans_deleted",
        Some(user.user_id.clone()),
        Some(user.email.clone()),
        None,
        None,
        Some(format!(
            "{deleted} object(s), {}",
            format_size(bytes, DECIMAL)
        )),
    )
    .await;
    tracing::info!(deleted, failed, bytes, actor = %user.email, "orphaned storage objects deleted");

    let flash = if failed == 0 {
        format!(
            "Deleted {deleted} orphaned object(s), reclaiming {}.",
            format_size(bytes, DECIMAL)
        )
    } else {
        format!(
            "Deleted {deleted} orphaned object(s) ({}); {failed} failed — see the logs.",
            format_size(bytes, DECIMAL)
        )
    };
    Ok(Redirect::to(&format!("/admin/storage?flash={}", urlencode(&flash))).into_response())
}

#[derive(Debug, Deserialize)]
pub struct DeleteOneForm {
    pub key: String,
}

/// `POST /admin/storage/delete` — delete a single orphan by key.
pub async fn storage_delete_one(
    State(state): State<AppState>,
    SuperAdmin(user): SuperAdmin,
    Form(form): Form<DeleteOneForm>,
) -> Result<Response, AppError> {
    let (deleted, bytes, _failed) = delete_orphans(&state, Some(form.key.clone())).await;

    if deleted > 0 {
        audit::record(
            &state.db,
            "storage_orphans_deleted",
            Some(user.user_id.clone()),
            Some(user.email.clone()),
            None,
            None,
            Some(format!(
                "1 object, {} ({})",
                format_size(bytes, DECIMAL),
                form.key
            )),
        )
        .await;
    }

    let flash = if deleted > 0 {
        format!("Deleted. Reclaimed {}.", format_size(bytes, DECIMAL))
    } else {
        "Nothing deleted: that object is no longer an orphan (or is already gone).".to_string()
    };
    Ok(Redirect::to(&format!("/admin/storage?flash={}", urlencode(&flash))).into_response())
}
