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

/// Single round-trip session load: profile + (optional) brokerage
/// membership. Both extractors call this so authenticated requests
/// pay for one DB hop instead of two. Returns `(profile, membership)`;
/// `Ok(None)` for profile means "the JWT references a user that no
/// longer exists" (treated as Unauthorized by callers).
async fn load_session(
    db: &crate::state::Db,
    user_id: &RecordId,
) -> Result<(Option<UserProfile>, Option<MembershipRow>), AppError> {
    let mut q = db
        .query(
            "SELECT email, name, avatar_storage_key FROM ONLY $u; \
             SELECT out AS brokerage, role FROM works_at WHERE in = $u LIMIT 1",
        )
        .bind(("u", user_id.clone()))
        .await?;
    let profile: Option<UserProfile> = q.take(0)?;
    let membership: Option<MembershipRow> = q.take(1)?;
    Ok((profile, membership))
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
        let (profile, membership) = load_session(&state.db, &user_id).await?;
        let profile = profile.ok_or(AppError::Unauthorized)?;
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

/// Authenticated-but-maybe-brokerage-less variant. Used by the
/// no-brokerage landing page and the invite accept/decline handlers,
/// which must work for a user who is signed in but isn't currently
/// attached to any brokerage (newly removed agent, or someone whose
/// previous brokerage closed). Every other authenticated route should
/// stay on [`CurrentUser`] so `brokerage_id` is statically guaranteed.
#[derive(Debug, Clone)]
pub struct LooseCurrentUser {
    pub user_id: RecordId,
    pub email: String,
    pub name: String,
    // Currently only checked for the friendly avatar fallback inside
    // the no-brokerage landing; kept on the struct so callers don't
    // have to second-query the profile.
    #[allow(dead_code)]
    pub has_avatar: bool,
    pub membership: Option<LooseMembership>,
}

/// Brokerage membership data preserved on the loose extractor so a
/// future brokerage-switcher (option B) can route the user without
/// re-running the works_at lookup. Currently only `is_some()` is read.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct LooseMembership {
    pub brokerage_id: RecordId,
    pub role: Role,
}

impl FromRequestParts<AppState> for LooseCurrentUser {
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
        let (profile, row) = load_session(&state.db, &user_id).await?;
        let profile = profile.ok_or(AppError::Unauthorized)?;
        let membership = row.and_then(|r| {
            Role::parse(&r.role).map(|role| LooseMembership {
                brokerage_id: r.brokerage,
                role,
            })
        });

        Ok(LooseCurrentUser {
            user_id,
            email: profile.email,
            name: profile.name,
            has_avatar: profile.avatar_storage_key.is_some(),
            membership,
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
