//! Request-time authentication.
//!
//! Every authenticated route takes a [`CurrentUser`] as an extractor argument.
//! The extractor pulls the JWT from the `tv_session` cookie, validates it,
//! then looks up the user and their brokerage membership via graph traversal.

use axum::extract::FromRequestParts;
use axum::http::Method;
use axum::http::request::Parts;
use serde::Deserialize;
use surrealdb::types::{RecordId, SurrealValue};
use tower_cookies::Cookies;

use crate::auth::{Claims, CurrentUser, Role, decode_token};
use crate::error::AppError;
use crate::state::AppState;

/// Name of the HTTP-only cookie that holds the JWT.
pub const SESSION_COOKIE: &str = "tv_session";

#[derive(Debug, Deserialize, SurrealValue)]
struct UserProfile {
    email: String,
    name: String,
    avatar_storage_key: Option<String>,
}

#[derive(Debug, Deserialize, SurrealValue)]
struct MembershipRow {
    brokerage: RecordId,
    role: String,
}

impl FromRequestParts<AppState> for CurrentUser {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let cookies = Cookies::from_request_parts(parts, state)
            .await
            .map_err(|_| AppError::Unauthorized)?;

        let token = cookies
            .get(SESSION_COOKIE)
            .map(|c| c.value().to_string())
            .ok_or(AppError::Unauthorized)?;

        let claims: Claims =
            decode_token(&state.config, &token).map_err(|_| AppError::Unauthorized)?;

        let user_id = claims.user_id();

        let mut profile_q = state
            .db
            .query("SELECT email, name, avatar_storage_key FROM ONLY $u")
            .bind(("u", user_id.clone()))
            .await?;
        let profile: Option<UserProfile> = profile_q.take(0)?;
        let profile = profile.ok_or(AppError::Unauthorized)?;

        // Graph hop: user -> works_at -> brokerage. We also grab the role
        // stored on the relation edge in the same round trip.
        let mut response = state
            .db
            .query("SELECT out AS brokerage, role FROM works_at WHERE in = $u LIMIT 1")
            .bind(("u", user_id.clone()))
            .await?;
        let membership: Option<MembershipRow> = response.take(0)?;
        let membership = membership.ok_or(AppError::Forbidden)?;

        let role = Role::parse(&membership.role).ok_or(AppError::Forbidden)?;

        let current = CurrentUser {
            user_id,
            brokerage_id: membership.brokerage,
            email: profile.email,
            name: profile.name,
            role,
            has_avatar: profile.avatar_storage_key.is_some(),
        };

        // Subscription gate. Reads stay open so a brokerage in
        // wind-down can still export their data during the grace
        // period; writes (anything mutating data) get blocked. Scoped
        // to `/app/*` so admin routes — which go through `SuperAdmin`
        // wrapping `CurrentUser` — aren't affected by their own
        // brokerage's billing state.
        if is_app_write(parts) {
            crate::billing::assert_brokerage_writable(state, &current).await?;
        }

        Ok(current)
    }
}

/// True when the request is a write under `/app/*`. Used by the
/// subscription gate to skip reads (export-friendly during grace)
/// and to leave `/admin/*` + public routes alone.
fn is_app_write(parts: &Parts) -> bool {
    let writing = matches!(
        parts.method,
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    );
    writing && parts.uri.path().starts_with("/app/")
}

/// Optional variant — used on pages that change their shape when signed in
/// (landing/pricing top nav) but do not strictly require authentication.
#[derive(Debug, Clone)]
pub struct MaybeCurrentUser(pub Option<CurrentUser>);

impl FromRequestParts<AppState> for MaybeCurrentUser {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        Ok(MaybeCurrentUser(
            CurrentUser::from_request_parts(parts, state).await.ok(),
        ))
    }
}

/// Extractor that gates a route to super-admin users only.
///
/// Super-admin status is configured via the `SUPERADMIN_EMAILS` env var
/// (comma-separated). Membership is checked at request time so promoting
/// a user is a one-line config change + a redeploy — no DB migration.
#[derive(Debug, Clone)]
pub struct SuperAdmin(pub CurrentUser);

impl FromRequestParts<AppState> for SuperAdmin {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let user = CurrentUser::from_request_parts(parts, state).await?;
        let email = user.email.to_ascii_lowercase();
        if state.config.super_admin_emails.iter().any(|e| e == &email) {
            Ok(SuperAdmin(user))
        } else {
            Err(AppError::Forbidden)
        }
    }
}
