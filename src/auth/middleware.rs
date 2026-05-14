//! Request-time authentication.
//!
//! Every authenticated route takes a [`CurrentUser`] as an extractor argument.
//! The extractor pulls the JWT from the `tv_session` cookie, validates it,
//! then looks up the user and their brokerage membership via graph traversal.

use axum::extract::FromRequestParts;
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
            .query("SELECT email, name FROM ONLY $u")
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

        Ok(CurrentUser {
            user_id,
            brokerage_id: membership.brokerage,
            email: profile.email,
            name: profile.name,
            role,
        })
    }
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
