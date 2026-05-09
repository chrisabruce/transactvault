//! HTTP handlers, organised by feature.

pub mod admin;
pub mod auth;
pub mod checklists;
pub mod comments;
pub mod documents;
pub mod health;
pub mod marketing;
pub mod members;
pub mod transactions;

use askama::Template;
use axum::response::Html;

use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::state::AppState;

/// Convenience: is this user listed in `SUPERADMIN_EMAILS`?
pub fn is_super_admin(state: &AppState, user: &CurrentUser) -> bool {
    let email = user.email.to_ascii_lowercase();
    state.config.super_admin_emails.iter().any(|e| e == &email)
}

/// Render an Askama template and wrap it in `Html<String>` for Axum.
///
/// Keeping this helper local avoids pulling in `askama_axum` (which has been
/// split off into separate integration crates that churn between releases).
pub fn render<T: Template>(tpl: &T) -> Result<Html<String>, AppError> {
    Ok(Html(tpl.render()?))
}

/// Render an Askama template directly to a `String` — used when the caller
/// wraps the result in something other than `Html` (SSE fragments, ZIP
/// archives, etc.).
#[allow(dead_code)]
pub fn render_str<T: Template>(tpl: &T) -> Result<String, AppError> {
    Ok(tpl.render()?)
}
