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
