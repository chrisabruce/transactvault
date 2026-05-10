//! HTTP route table. Public marketing pages sit at the root, the app proper
//! is namespaced under `/app`, and the privileged super-admin views under
//! `/admin`. Fragment endpoints return HTML chunks so Datastar can swap
//! them straight into the DOM without client-side rendering.

use std::time::Duration;

use axum::Router;
use axum::routing::{get, post};
use tower_cookies::CookieManagerLayer;
use tower_http::compression::CompressionLayer;
use tower_http::services::ServeDir;
use tower_http::trace::{DefaultMakeSpan, DefaultOnFailure, DefaultOnResponse, TraceLayer};
use tracing::Level;

use crate::controllers::{
    admin, auth, checklists, comments, documents, health, marketing, members, transactions,
};
use crate::state::AppState;

pub fn build(state: AppState) -> Router {
    let public = Router::new()
        .route("/", get(marketing::landing))
        .route("/pricing", get(marketing::pricing))
        .route("/brand", get(marketing::brand))
        .route("/login", get(auth::login_form).post(auth::login))
        .route("/signup", get(auth::signup_form).post(auth::signup))
        .route("/signup/check-email", get(auth::signup_check_email))
        .route("/verify/{token}", get(auth::verify))
        .route("/logout", post(auth::logout))
        .route(
            "/invite/{token}",
            get(auth::invite_form).post(auth::accept_invite),
        );

    let app = Router::new()
        .route("/app", get(transactions::dashboard))
        .route("/app/search", get(transactions::search))
        .route(
            "/app/transactions",
            get(transactions::list).post(transactions::create),
        )
        .route("/app/transactions/new", get(transactions::new_form))
        .route("/app/transactions/{id}", get(transactions::show))
        .route(
            "/app/transactions/{id}/status",
            post(transactions::update_status),
        )
        .route("/app/transactions/{id}/export", get(documents::export_zip))
        .route("/app/transactions/{id}/checklist", post(checklists::create))
        .route("/app/checklist/{id}/approve", post(checklists::approve))
        .route("/app/checklist/{id}/deny", post(checklists::deny))
        .route(
            "/app/checklist/{id}/comments",
            post(comments::create_on_item),
        )
        .route(
            "/app/transactions/{id}/comments",
            post(comments::create_on_transaction),
        )
        .route("/app/transactions/{id}/documents", post(documents::upload))
        .route("/app/documents/{id}/download", get(documents::download))
        .route("/app/team", get(members::list))
        .route("/app/team/invite", post(members::invite))
        .route(
            "/app/team/invite/{token}/resend",
            post(members::resend_invite),
        )
        .route(
            "/app/team/invite/{token}/cancel",
            post(members::cancel_invite),
        )
        .route("/app/team/{user_id}/role", post(members::change_role));

    let admin_routes = Router::new()
        .route("/admin", get(admin::users))
        .route("/admin/audit", get(admin::audit_log));

    Router::new()
        .merge(public)
        .merge(app)
        .merge(admin_routes)
        .route("/healthcheck", get(health::healthcheck))
        .nest_service("/static", ServeDir::new("static"))
        .layer(CookieManagerLayer::new())
        .layer(CompressionLayer::new())
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(
                    DefaultMakeSpan::new()
                        .level(Level::INFO)
                        .include_headers(false),
                )
                .on_response(DefaultOnResponse::new().level(Level::INFO))
                .on_failure(DefaultOnFailure::new().level(Level::ERROR)),
        )
        .with_state(state)
        .layer(axum::extract::DefaultBodyLimit::max(64 * 1024 * 1024))
        .layer(tower_http::timeout::TimeoutLayer::with_status_code(
            axum::http::StatusCode::GATEWAY_TIMEOUT,
            Duration::from_secs(60),
        ))
}
