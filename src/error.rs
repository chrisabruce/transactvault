//! Application-wide error type. Every controller returns `Result<_, AppError>`,
//! which converts cleanly into an HTTP response with an appropriate status code
//! and user-safe message.

use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use thiserror::Error;

/// All request-handling errors funnel through this type.
#[allow(dead_code)]
#[derive(Debug, Error)]
pub enum AppError {
    #[error("not found")]
    NotFound,
    #[error("forbidden")]
    Forbidden,
    #[error("unauthorized")]
    Unauthorized,
    #[error("{0}")]
    Validation(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error(transparent)]
    Database(#[from] surrealdb::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Render(#[from] askama::Error),
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

impl AppError {
    /// Validation helper so call sites stay terse: `AppError::invalid("x")`.
    pub fn invalid(msg: impl Into<String>) -> Self {
        Self::Validation(msg.into())
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, message) = match &self {
            AppError::NotFound => (StatusCode::NOT_FOUND, "Page not found".to_string()),
            AppError::Forbidden => (StatusCode::FORBIDDEN, "You don't have access to that.".to_string()),
            AppError::Unauthorized => (StatusCode::UNAUTHORIZED, "Please sign in to continue.".to_string()),
            AppError::Validation(msg) => (StatusCode::BAD_REQUEST, msg.clone()),
            AppError::Conflict(msg) => (StatusCode::CONFLICT, msg.clone()),
            other => {
                // Use {:?} so the full error chain (cause/source) prints,
                // not just the top-level Display message — surrealdb and
                // askama errors hide the most useful detail in their source.
                tracing::error!(error = ?other, error_display = %other, "internal server error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Something went wrong. Please try again.".to_string(),
                )
            }
        };

        let body = Html(format!(
            "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><title>{code}</title><link rel=\"stylesheet\" href=\"/static/css/main.css\"></head><body><main class=\"error-page\"><h1>{code}</h1><p>{msg}</p><p><a href=\"/\">Back to home</a></p></main></body></html>",
            code = status.as_u16(),
            msg = html_escape(&message),
        ));

        (status, body).into_response()
    }
}

/// Minimal HTML escape for the fallback error page so error messages can't
/// inject markup. Anything user-facing elsewhere should flow through Askama's
/// auto-escaping.
fn html_escape(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
