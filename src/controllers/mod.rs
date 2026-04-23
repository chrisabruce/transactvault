//! HTTP handlers, organised by feature.

pub mod auth;
pub mod checklists;
pub mod documents;
pub mod health;
pub mod marketing;
pub mod members;
pub mod transactions;

use askama::Template;
use axum::response::Html;

use crate::error::AppError;

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
