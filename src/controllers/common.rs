//! Cross-handler helpers that every authenticated controller needs.
//!
//! Centralising these stops the same boilerplate from drifting across
//! a dozen controllers — a single source of truth for "how do we build
//! an `AppHeader`?" means a new optional header field (e.g. a feature
//! flag, an unread-count badge) lands in one place.

use crate::auth::CurrentUser;
use crate::billing;
use crate::state::AppState;
use crate::templates::AppHeader;

/// Build a fully-populated [`AppHeader`] for an authenticated page.
///
/// This rolls together the three things every header needs:
///
/// 1. The brokerage row (for the name and the subscription banner).
/// 2. The super-admin check (config-driven; cheap).
/// 3. The avatar wiring (URL-safe user key + has-avatar flag).
///
/// `active_nav` is the navigation slug for the current section
/// (`"transactions"`, `"team"`, `"admin"`, …) — the template uses it
/// to mark the active link in the top nav.
pub async fn build_app_header(
    state: &AppState,
    user: &CurrentUser,
    active_nav: impl Into<String>,
) -> AppHeader {
    let info = billing::header_info_for_user(state, user).await;
    AppHeader::new(
        user.name.clone(),
        user.email.clone(),
        user.role,
        info.brokerage_name,
        active_nav,
    )
    .with_super_admin(super::is_super_admin(state, user))
    .with_avatar(crate::db::record_key(&user.user_id), user.has_avatar)
    .with_banner(info.banner)
}
