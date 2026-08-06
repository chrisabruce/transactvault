//! Signup, login, logout, and invitation acceptance.
//!
//! The signup path is hardened against the abuse vectors caught in the
//! email-spam audit:
//!
//! 1. **Honeypot field** — bots that auto-fill every input get rejected
//!    silently before any work is done.
//! 2. **Proof of work** — the form ships a HMAC-signed challenge that the
//!    browser must solve client-side before submitting. This makes burst
//!    abuse expensive without bothering humans.
//! 3. **IP rate limiting** — token-bucket limiter on `/signup` and `/login`.
//! 4. **Email verification gate** — the welcome email is gone; new accounts
//!    receive a verify link and stay disabled until clicked.
//! 5. **Disposable-email blacklist** — well-known throwaway providers are
//!    refused at the door.
//! 6. **Audit log** — every signup, login, and blocked attempt records to
//!    `audit_event` for forensic review in the admin panel.

use std::net::SocketAddr;

use axum::Form;
use axum::extract::{ConnectInfo, Path, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use chrono::{Duration, Utc};
use serde::Deserialize;
use surrealdb::types::{RecordId, SurrealValue};
use tower_cookies::{Cookie, Cookies};

use crate::audit;
use crate::auth::middleware::{MaybeCurrentUser, MaybeLooseCurrentUser, SESSION_COOKIE};
use crate::auth::{hash_password, issue_token, verify_password};
use crate::controllers::render;
use crate::error::AppError;
use crate::models::{Brokerage, Invitation, NewBrokerage, NewInvitation, NewUser, User};
use crate::security::{
    self, allow_per_hour, allow_per_quarter_hour, client_ip, is_disposable_email, throttle_key,
    user_agent,
};
use crate::state::AppState;
use crate::templates::{
    ForgotPasswordPage, InvitePage, LoginPage, ResetPasswordPage, SignupPage, VerifyPendingPage,
    VerifyResultPage,
};

// ---------------------------------------------------------------------------
// Signup
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct SignupInput {
    pub name: String,
    pub email: String,
    pub password: String,
    pub brokerage_name: String,
    #[serde(default)]
    pub city: Option<String>,

    /// Honeypot: any non-empty submission of `website` means a bot. Real
    /// users never see the field — it's positioned off-screen via CSS.
    #[serde(default)]
    pub website: Option<String>,

    /// Proof-of-work challenge that the JS solver embeds back into the
    /// form on submit.
    #[serde(default)]
    pub pow_challenge: Option<String>,
    #[serde(default)]
    pub pow_solution: Option<String>,
}

pub async fn signup_form(
    State(state): State<AppState>,
    MaybeCurrentUser(current): MaybeCurrentUser,
) -> Result<Response, AppError> {
    if current.is_some() {
        return Ok(Redirect::to("/app").into_response());
    }
    let challenge =
        security::issue_challenge(&state.config.jwt_secret, state.config.pow_difficulty_bits)?;
    let page = SignupPage {
        app_name: &state.config.app_name,
        base_url: &state.config.base_url,
        error: None,
        signed_in: false,
        pow_challenge: challenge.challenge,
        pow_difficulty: challenge.difficulty,
        prefill: Default::default(),
    };
    Ok(render(&page)?.into_response())
}

pub async fn signup(
    State(state): State<AppState>,
    cookies: Cookies,
    headers: HeaderMap,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Form(input): Form<SignupInput>,
) -> Result<Response, AppError> {
    let ip = client_ip(&headers, Some(&peer), state.config.trusted_proxy_hops);
    let ua = user_agent(&headers);

    // (1) Honeypot — bot autofilled the hidden field. Pretend success so the
    // bot can't tell its trick was caught, but skip every side effect.
    if input
        .website
        .as_deref()
        .map(|s| !s.is_empty())
        .unwrap_or(false)
    {
        audit::record(
            &state.db,
            "signup_blocked_honeypot",
            None,
            input.email.trim().to_ascii_lowercase().into(),
            ip.clone().into(),
            ua.clone(),
            None,
        )
        .await;
        return Ok(Redirect::to("/signup/check-email").into_response());
    }

    // (2) IP rate limit — short-circuits before any DB work.
    let rate_key = format!("signup:{ip}");
    if !allow_per_hour(
        &state.rate_limiter,
        &rate_key,
        state.config.signup_rate_per_hour,
    ) {
        audit::record(
            &state.db,
            "signup_blocked_rate_limit",
            None,
            input.email.trim().to_ascii_lowercase().into(),
            ip.clone().into(),
            ua.clone(),
            None,
        )
        .await;
        // Same generic success page so a probing attacker can't tell the
        // difference between "we rate-limited you" and "we accepted you".
        return Ok(Redirect::to("/signup/check-email").into_response());
    }

    // (3) Proof-of-work
    let challenge = input.pow_challenge.as_deref().unwrap_or("");
    let solution = input.pow_solution.as_deref().unwrap_or("");
    if !security::verify_challenge(
        &state.config.jwt_secret,
        challenge,
        solution,
        state.config.pow_difficulty_bits,
    ) {
        audit::record(
            &state.db,
            "signup_blocked_pow",
            None,
            input.email.trim().to_ascii_lowercase().into(),
            ip.clone().into(),
            ua.clone(),
            None,
        )
        .await;
        return render_signup_error(
            &state,
            "Please reload the page and try again.",
            signup_prefill(&input),
        )
        .await;
    }

    // (4) Field validation
    let email = input.email.trim().to_ascii_lowercase();
    let name = input.name.trim().to_string();
    let brokerage_name = input.brokerage_name.trim().to_string();

    if name.is_empty() || email.is_empty() || brokerage_name.is_empty() {
        return render_signup_error(
            &state,
            "Please fill in every field.",
            signup_prefill(&input),
        )
        .await;
    }
    // Both values reach email subject lines, export manifests and log
    // records as plain text. Rejecting invisible characters at the door
    // beats scrubbing at every sink and hoping none was missed.
    if crate::sanitize::has_unsafe_text(&name) || crate::sanitize::has_unsafe_text(&brokerage_name)
    {
        return render_signup_error(
            &state,
            "Names can't contain control or invisible characters.",
            signup_prefill(&input),
        )
        .await;
    }
    if input.password.len() < 8 {
        return render_signup_error(
            &state,
            "Password must be at least 8 characters.",
            signup_prefill(&input),
        )
        .await;
    }
    if !email.contains('@') {
        return render_signup_error(
            &state,
            "Please enter a valid email address.",
            signup_prefill(&input),
        )
        .await;
    }

    // (5) Disposable-email blacklist — silent reject (don't reveal which
    // domains we blocklist).
    if is_disposable_email(&email) {
        audit::record(
            &state.db,
            "signup_blocked_blacklist",
            None,
            email.clone().into(),
            ip.clone().into(),
            ua.clone(),
            None,
        )
        .await;
        return Ok(Redirect::to("/signup/check-email").into_response());
    }

    // (6) Email-already-registered — silent reject (don't enumerate users).
    let mut existing = state
        .db
        .query("SELECT id FROM user WHERE email = $e LIMIT 1")
        .bind(("e", email.clone()))
        .await?;
    let existing: Option<IdOnly> = existing.take(0)?;
    if existing.is_some() {
        audit::record(
            &state.db,
            "signup_blocked_duplicate",
            None,
            email.clone().into(),
            ip.clone().into(),
            ua.clone(),
            None,
        )
        .await;
        return Ok(Redirect::to("/signup/check-email").into_response());
    }

    // (7) Create the unverified account + brokerage and dispatch the
    // verify email. We DO NOT issue a session cookie — login is gated on
    // email_verified.
    let hashed = hash_password(&input.password).await?;
    let token = generate_token();
    let expiry = Utc::now() + Duration::hours(state.config.verification_expiry_hours);

    let user: Option<User> = state
        .db
        .create("user")
        .content(NewUser {
            email: email.clone(),
            name: name.clone(),
            password_hash: hashed,
            email_verified: false,
            verification_token: Some(token.clone()),
            verification_expires: Some(expiry),
            signup_ip: Some(ip.clone()),
            signup_user_agent: ua.clone(),
        })
        .await?;
    let user =
        user.ok_or_else(|| AppError::Internal(anyhow::anyhow!("user creation returned nothing")))?;

    let brokerage: Option<Brokerage> = state
        .db
        .create("brokerage")
        .content(NewBrokerage {
            name: brokerage_name,
            city: input
                .city
                .map(|c| c.trim().to_string())
                .filter(|c| !c.is_empty()),
        })
        .await?;
    let brokerage = brokerage.ok_or_else(|| {
        AppError::Internal(anyhow::anyhow!("brokerage creation returned nothing"))
    })?;

    state
        .db
        .query("RELATE $u->works_at->$b SET role = 'broker'")
        .bind(("u", user.id.clone()))
        .bind(("b", brokerage.id.clone()))
        .await?;

    // Default the new brokerage to the California state form set so its
    // transactions resolve the standard CA checklist. Best-effort —
    // a missing seed is logged, not fatal to signup.
    if let Err(e) = crate::db::forms::attach_default_state(&state.db, &brokerage.id).await {
        tracing::warn!(error = %e, "could not attach default form state at signup");
    }

    // Detached for the same reason as the reset mail: the duplicate-address
    // branch above returns without touching the network, so awaiting a
    // Postmark round-trip here widens the timing gap between "registered"
    // and "not registered" by a few hundred milliseconds. This does not
    // make signup fully timing-uniform — the new-account path genuinely
    // does more database work — but it removes the largest and most
    // variable component of the difference.
    let verify_link = format!("{}/verify/{}", state.config.base_url, token);
    let mailer = state.mailer.clone();
    let (to, display_name) = (user.email.clone(), user.name.clone());
    tokio::spawn(async move {
        mailer.send_verify(&to, &display_name, &verify_link).await;
    });

    audit::record(
        &state.db,
        "signup_pending",
        Some(user.id.clone()),
        Some(email),
        Some(ip),
        ua,
        None,
    )
    .await;

    // No session cookie until verified.
    let _ = cookies; // silence lint
    Ok(Redirect::to("/signup/check-email").into_response())
}

/// Static "check your inbox" landing page used as the response for every
/// signup outcome — whether we created an account, hit a duplicate, or
/// rejected for spam reasons. Constant response prevents enumeration.
pub async fn signup_check_email(State(state): State<AppState>) -> Result<Html<String>, AppError> {
    let page = VerifyPendingPage {
        app_name: &state.config.app_name,
        base_url: &state.config.base_url,
        signed_in: false,
    };
    render(&page)
}

// ---------------------------------------------------------------------------
// Email verification
// ---------------------------------------------------------------------------

/// `GET /verify/{token}` — confirm an address, then send them to log in.
///
/// Never mints a session; see the note on the final redirect.
pub async fn verify(
    State(state): State<AppState>,
    headers: HeaderMap,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(token): Path<String>,
) -> Result<Response, AppError> {
    let ip = client_ip(&headers, Some(&peer), state.config.trusted_proxy_hops);
    let ua = user_agent(&headers);

    let mut q = state
        .db
        .query("SELECT * FROM user WHERE verification_token = $t LIMIT 1")
        .bind(("t", token.clone()))
        .await?;
    let user: Option<User> = q.take(0)?;

    let user = match user {
        Some(u) => u,
        None => {
            audit::record(
                &state.db,
                "verify_failure",
                None,
                None,
                Some(ip),
                ua,
                Some("token not found".into()),
            )
            .await;
            return render_verify_failure(
                &state,
                "That verification link is invalid or has already been used.",
            )
            .await;
        }
    };

    if user.email_verified {
        // Already verified — send them to sign in. See the note on the
        // final redirect for why this route never mints a session.
        return Ok(Redirect::to("/login?verified=1").into_response());
    }

    if user
        .verification_expires
        .map(|d| d < Utc::now())
        .unwrap_or(true)
    {
        audit::record(
            &state.db,
            "verify_failure",
            Some(user.id.clone()),
            Some(user.email.clone()),
            Some(ip),
            ua,
            Some("token expired".into()),
        )
        .await;
        return render_verify_failure(
            &state,
            "That verification link has expired. Sign up again to request a fresh one.",
        )
        .await;
    }

    // Mark verified, clear the token, stamp last_login_at.
    state
        .db
        .query(
            "UPDATE $u SET
                email_verified = true,
                verification_token = NONE,
                verification_expires = NONE,
                last_login_at = time::now()",
        )
        .bind(("u", user.id.clone()))
        .await?;

    // NOW send the welcome — only delivered to verified addresses.
    let brokerage_name = lookup_brokerage_name(&state, &user.id).await;
    state
        .mailer
        .send_welcome(
            &user.email,
            &user.name,
            &brokerage_name,
            &state.config.base_url,
        )
        .await;

    audit::record(
        &state.db,
        "verify_success",
        Some(user.id.clone()),
        Some(user.email.clone()),
        Some(ip),
        ua,
        None,
    )
    .await;

    // Deliberately NOT signing them in.
    //
    // This is a GET, so `SameSite=Lax` sends cookies on a top-level
    // navigation and the Fetch-Metadata CSRF layer only guards unsafe
    // methods. Minting a session here therefore gave anyone a
    // session-fixation primitive: send a victim your own verification
    // link, and their browser silently becomes *your* account — every
    // document they then upload lands in the attacker's brokerage.
    // Making them sign in costs one form and removes the primitive.
    Ok(Redirect::to("/login?verified=1").into_response())
}

async fn lookup_brokerage_name(state: &AppState, user_id: &RecordId) -> String {
    let mut q = match state
        .db
        .query("SELECT VALUE out.name FROM works_at WHERE in = $u LIMIT 1")
        .bind(("u", user_id.clone()))
        .await
    {
        Ok(q) => q,
        Err(_) => return String::new(),
    };
    let names: Vec<String> = q.take(0).unwrap_or_default();
    names.into_iter().next().unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Login / Logout
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct LoginInput {
    pub email: String,
    pub password: String,
}

#[derive(Debug, Deserialize)]
pub struct LoginQuery {
    /// `1` right after a completed password reset.
    #[serde(default)]
    pub reset: Option<String>,
    /// `1` right after a completed email verification.
    #[serde(default)]
    pub verified: Option<String>,
}

pub async fn login_form(
    State(state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<LoginQuery>,
    MaybeCurrentUser(current): MaybeCurrentUser,
) -> Result<Response, AppError> {
    if current.is_some() {
        return Ok(Redirect::to("/app").into_response());
    }
    let page = LoginPage {
        app_name: &state.config.app_name,
        base_url: &state.config.base_url,
        error: None,
        signed_in: false,
        email: String::new(),
        just_reset: q.reset.as_deref() == Some("1"),
        just_verified: q.verified.as_deref() == Some("1"),
    };
    Ok(render(&page)?.into_response())
}

pub async fn login(
    State(state): State<AppState>,
    cookies: Cookies,
    headers: HeaderMap,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Form(input): Form<LoginInput>,
) -> Result<Response, AppError> {
    let ip = client_ip(&headers, Some(&peer), state.config.trusted_proxy_hops);
    let ua = user_agent(&headers);
    let email = input.email.trim().to_ascii_lowercase();

    let rate_key = format!("login:{ip}");
    if !allow_per_quarter_hour(
        &state.rate_limiter,
        &rate_key,
        state.config.login_rate_per_quarter_hour,
    ) {
        audit::record(
            &state.db,
            "login_failure",
            None,
            Some(email),
            Some(ip),
            ua,
            Some("rate limited".into()),
        )
        .await;
        return render_login_error(
            &state,
            "Too many attempts. Wait a few minutes and try again.",
            input.email.trim().to_string(),
        )
        .await;
    }

    let mut response = state
        .db
        .query("SELECT * FROM user WHERE email = $e LIMIT 1")
        .bind(("e", email.clone()))
        .await?;
    let user: Option<User> = response.take(0)?;

    let Some(user) = user else {
        // Burn the same Argon2id work an existing account would cost.
        // Returning early here made the unknown-email path measurably
        // faster (m=19 MiB, t=2 is tens of ms), turning login into a
        // user-enumeration oracle — for a brokerage-compliance product
        // that leaks the customer roster to anyone who can time a
        // request.
        verify_password(&input.password, dummy_password_hash())
            .await
            .ok();
        audit::record(
            &state.db,
            "login_failure",
            None,
            Some(email),
            Some(ip),
            ua,
            Some("unknown email".into()),
        )
        .await;
        return render_login_error(
            &state,
            "No account with those credentials.",
            input.email.trim().to_string(),
        )
        .await;
    };

    let ok = verify_password(&input.password, &user.password_hash).await?;
    if !ok {
        audit::record(
            &state.db,
            "login_failure",
            Some(user.id.clone()),
            Some(email),
            Some(ip),
            ua,
            Some("bad password".into()),
        )
        .await;
        return render_login_error(
            &state,
            "No account with those credentials.",
            input.email.trim().to_string(),
        )
        .await;
    }

    if !user.email_verified {
        audit::record(
            &state.db,
            "login_blocked_unverified",
            Some(user.id.clone()),
            Some(email),
            Some(ip),
            ua,
            None,
        )
        .await;
        return render_login_error(
            &state,
            "Please verify your email before signing in. Check your inbox for the link we sent.",
            input.email.trim().to_string(),
        )
        .await;
    }

    state
        .db
        .query("UPDATE $u SET last_login_at = time::now()")
        .bind(("u", user.id.clone()))
        .await?;

    audit::record(
        &state.db,
        "login_success",
        Some(user.id.clone()),
        Some(email),
        Some(ip),
        ua,
        None,
    )
    .await;

    set_session_cookie(&state, &cookies, &user.id).await?;

    // Send users without a brokerage to the friendly landing instead
    // of `/app`, which would otherwise 403 inside the `CurrentUser`
    // extractor.
    let mut membership_q = state
        .db
        .query("SELECT VALUE id FROM works_at WHERE in = $u LIMIT 1")
        .bind(("u", user.id.clone()))
        .await?;
    let memberships: Vec<RecordId> = membership_q.take(0).unwrap_or_default();
    if memberships.is_empty() {
        return Ok(Redirect::to("/app/no-brokerage").into_response());
    }

    Ok(Redirect::to("/app").into_response())
}

/// `POST /logout` — clear the cookie *and* revoke every live session.
///
/// Takes the loose extractor on purpose: `MaybeCurrentUser` resolves to
/// `None` for a signed-in user with no brokerage, which used to skip the
/// `bump_token_version` call below and leave a stolen JWT valid.
pub async fn logout(
    State(state): State<AppState>,
    cookies: Cookies,
    headers: HeaderMap,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    MaybeLooseCurrentUser(current): MaybeLooseCurrentUser,
) -> Redirect {
    let ip = client_ip(&headers, Some(&peer), state.config.trusted_proxy_hops);
    let ua = user_agent(&headers);
    let logged_out_user = current.as_ref().map(|u| u.user_id.clone());
    if let Some(u) = current {
        audit::record(
            &state.db,
            "logout",
            Some(u.user_id.clone()),
            Some(u.email),
            Some(ip),
            ua,
            None,
        )
        .await;
    }

    if let Some(uid) = logged_out_user {
        bump_token_version(&state, &uid).await;
    }
    clear_session_cookie(&state, &cookies);
    Redirect::to("/")
}

/// Expire the session cookie, mirroring every attribute
/// [`set_session_cookie`] sets.
///
/// Shared by logout and brokerage self-deletion so the two can't drift:
/// a clearing cookie must match the original's `Path` (and `Secure`, on
/// browsers that scope by it) or the browser keeps the live one.
/// Re-issue the caller's session cookie after their token version was
/// bumped, so the user who just changed their own password isn't logged
/// out by their own action (every *other* session stays revoked).
pub(crate) async fn reissue_session_cookie(
    state: &AppState,
    cookies: &Cookies,
    user_id: &RecordId,
) -> Result<(), AppError> {
    set_session_cookie(state, cookies, user_id).await
}

/// Invalidate every outstanding session for a user by incrementing the
/// counter their tokens were minted against.
///
/// Called on logout and password change. Without it, a JWT captured
/// before either event stayed valid for its full lifetime — "sign out
/// on the lost laptop" and "change my password because I was
/// compromised" both did nothing to an already-stolen token.
///
/// Best-effort: a failure here is logged, never surfaced. Failing a
/// logout because telemetry-adjacent bookkeeping broke would be worse
/// than the (already clearing) cookie.
pub(crate) async fn bump_token_version(state: &AppState, user_id: &RecordId) {
    let result = state
        .db
        .query("UPDATE $u SET token_version = (token_version ?? 0) + 1")
        .bind(("u", user_id.clone()))
        .await;
    if let Err(e) = result {
        tracing::warn!(error = %e, "failed to bump token_version — sessions not revoked");
    }
}

pub(crate) fn clear_session_cookie(state: &AppState, cookies: &Cookies) {
    let mut cookie = Cookie::new(SESSION_COOKIE, "");
    cookie.set_path("/");
    cookie.set_http_only(true);
    cookie.set_same_site(tower_cookies::cookie::SameSite::Lax);
    cookie.set_secure(state.config.base_url.starts_with("https://"));
    cookie.set_max_age(tower_cookies::cookie::time::Duration::seconds(0));
    cookies.remove(cookie);
}

// ---------------------------------------------------------------------------
// Invitation acceptance
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct AcceptInviteInput {
    pub name: String,
    pub password: String,
}

pub async fn invite_form(
    State(state): State<AppState>,
    Path(token): Path<String>,
) -> Result<Html<String>, AppError> {
    let (invitation, brokerage, inviter_name) = load_invitation(&state, &token).await?;

    // If a user already exists with this email, swap the form for a
    // "sign in to accept" CTA — they don't need to (and can't) create
    // a new account. Once signed in, the no-brokerage landing has the
    // accept/decline buttons.
    let mut existing_q = state
        .db
        .query("SELECT VALUE id FROM user WHERE email = $e LIMIT 1")
        .bind(("e", invitation.email.clone()))
        .await?;
    let existing: Vec<RecordId> = existing_q.take(0).unwrap_or_default();
    let prompt_login = !existing.is_empty();

    let page = InvitePage {
        app_name: &state.config.app_name,
        base_url: &state.config.base_url,
        signed_in: false,
        invitation: &invitation,
        brokerage_name: &brokerage.name,
        inviter_name: &inviter_name,
        error: None,
        prompt_login,
        name_prefill: String::new(),
    };
    render(&page)
}

pub async fn accept_invite(
    State(state): State<AppState>,
    cookies: Cookies,
    headers: HeaderMap,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(token): Path<String>,
    Form(input): Form<AcceptInviteInput>,
) -> Result<Response, AppError> {
    let (invitation, brokerage, inviter_name) = load_invitation(&state, &token).await?;

    // Validation errors re-render the invite page with the typed name
    // kept — bouncing to a bare 400 error page (the previous behavior)
    // threw away everything the invitee had filled in.
    let name = input.name.trim().to_string();
    if input.password.len() < 8 {
        let html = render(&InvitePage {
            app_name: &state.config.app_name,
            base_url: &state.config.base_url,
            signed_in: false,
            invitation: &invitation,
            brokerage_name: &brokerage.name,
            inviter_name: &inviter_name,
            error: Some("Password must be at least 8 characters."),
            prompt_login: false,
            name_prefill: name,
        })?;
        return Ok(html.into_response());
    }
    if name.is_empty() {
        let html = render(&InvitePage {
            app_name: &state.config.app_name,
            base_url: &state.config.base_url,
            signed_in: false,
            invitation: &invitation,
            brokerage_name: &brokerage.name,
            inviter_name: &inviter_name,
            error: Some("Name is required."),
            prompt_login: false,
            name_prefill: name,
        })?;
        return Ok(html.into_response());
    }

    // Defense in depth: if a user with this email already exists (e.g.
    // they signed up between invite issuance and acceptance), the
    // `idx_user_email` unique index would 500 on insert below. Catch it
    // first and re-render the invite page with a clear explanation
    // instead. The invitation token stays unconsumed so the recipient
    // can re-use it after sorting out their existing account.
    let mut existing_q = state
        .db
        .query("SELECT VALUE id FROM user WHERE email = $e LIMIT 1")
        .bind(("e", invitation.email.clone()))
        .await?;
    let existing: Vec<RecordId> = existing_q.take(0).unwrap_or_default();
    if !existing.is_empty() {
        let html = render(&InvitePage {
            app_name: &state.config.app_name,
            base_url: &state.config.base_url,
            signed_in: false,
            invitation: &invitation,
            brokerage_name: &brokerage.name,
            inviter_name: &inviter_name,
            error: Some(
                "This email already has a TransactVault account. \
                 Sign in below to accept the invitation from your existing account.",
            ),
            prompt_login: true,
            name_prefill: String::new(),
        })?;
        return Ok(html.into_response());
    }

    let hashed = hash_password(&input.password).await?;
    let ip = client_ip(&headers, Some(&peer), state.config.trusted_proxy_hops);
    let ua = user_agent(&headers);

    // Invited users are pre-verified — the invite link itself was the
    // proof-of-inbox-control.
    let user: Option<User> = state
        .db
        .create("user")
        .content(NewUser {
            email: invitation.email.clone(),
            name,
            password_hash: hashed,
            email_verified: true,
            verification_token: None,
            verification_expires: None,
            signup_ip: Some(ip.clone()),
            signup_user_agent: ua.clone(),
        })
        .await?;
    let user =
        user.ok_or_else(|| AppError::Internal(anyhow::anyhow!("user creation returned nothing")))?;

    state
        .db
        .query("RELATE $u->works_at->$b SET role = $r")
        .bind(("u", user.id.clone()))
        .bind(("b", invitation.brokerage.clone()))
        .bind(("r", invitation.role.clone()))
        .await?;

    state
        .db
        .query("UPDATE $i SET accepted = true")
        .bind(("i", invitation.id.clone()))
        .await?;

    audit::record(
        &state.db,
        "invite_accepted",
        Some(user.id.clone()),
        Some(invitation.email.clone()),
        Some(ip),
        ua,
        None,
    )
    .await;

    // Brokerage-side: the joined brokerage's dashboards may want to
    // reflect the new member.
    state
        .events
        .publish(crate::events::Event::BrokerageMutation(
            invitation.brokerage.clone(),
        ));
    // Target-user-side: harmless on the freshly-created account (no
    // existing SSE streams), but published for symmetry with the
    // orphan-accept path so future stream surfaces don't need to
    // special-case the source of the membership change.
    state
        .events
        .publish(crate::events::Event::UserMembershipChanged(user.id.clone()));

    set_session_cookie(&state, &cookies, &user.id).await?;
    Ok(Redirect::to("/app").into_response())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

#[derive(Debug, serde::Deserialize, SurrealValue)]
struct IdOnly {
    #[allow(dead_code)]
    id: RecordId,
}

async fn load_invitation(
    state: &AppState,
    token: &str,
) -> Result<(Invitation, Brokerage, String), AppError> {
    let mut response = state
        .db
        .query(
            "SELECT * FROM invitation \
             WHERE token = $t AND accepted = false AND declined = false \
               AND expires_at > time::now() \
             LIMIT 1",
        )
        .bind(("t", token.to_string()))
        .await?;
    let invitation: Option<Invitation> = response.take(0)?;
    let invitation = invitation.ok_or(AppError::NotFound)?;

    let brokerage: Option<Brokerage> = state.db.select(invitation.brokerage.clone()).await?;
    let brokerage = brokerage.ok_or(AppError::NotFound)?;

    let mut response = state
        .db
        .query("SELECT name FROM ONLY $u")
        .bind(("u", invitation.invited_by.clone()))
        .await?;
    let inviter: Option<NameOnly> = response.take(0)?;
    let inviter_name = inviter
        .map(|n| n.name)
        .unwrap_or_else(|| "Your teammate".into());

    Ok((invitation, brokerage, inviter_name))
}

#[derive(Debug, serde::Deserialize, SurrealValue)]
struct NameOnly {
    name: String,
}

/// Mint a session JWT for `user_id` and set it as the session cookie.
///
/// Reads the user's current `token_version` and embeds it in the token,
/// which is what lets [`bump_token_version`] revoke outstanding
/// sessions. A missing row yields version 0 — the token is then
/// rejected by the middleware anyway, since the user lookup fails.
pub(crate) async fn set_session_cookie(
    state: &AppState,
    cookies: &Cookies,
    user_id: &RecordId,
) -> Result<(), AppError> {
    let key = crate::db::record_key(user_id);
    let mut q = state
        .db
        .query("SELECT VALUE token_version FROM ONLY $u")
        .bind(("u", user_id.clone()))
        .await?;
    let token_version: i64 = q.take::<Option<i64>>(0).ok().flatten().unwrap_or(0);
    let token = issue_token(&state.config, &key, token_version)
        .map_err(|e| AppError::Internal(anyhow::anyhow!("issue token: {e}")))?;

    let mut cookie = Cookie::new(SESSION_COOKIE, token);
    cookie.set_path("/");
    cookie.set_http_only(true);
    cookie.set_same_site(tower_cookies::cookie::SameSite::Lax);
    // `Secure` on any HTTPS deployment: without it the browser attaches
    // this multi-day session JWT to plain-http requests for the same
    // host, so anyone on the network path can lift it by getting the
    // victim to load `http://<app-host>/anything` (an <img> tag on any
    // unencrypted page suffices). Derived from BASE_URL rather than
    // hardcoded so local http dev still works.
    cookie.set_secure(state.config.base_url.starts_with("https://"));
    cookie.set_max_age(tower_cookies::cookie::time::Duration::hours(
        state.config.jwt_expiry_hours,
    ));
    cookies.add(cookie);
    Ok(())
}

/// A real Argon2id PHC hash of a fixed throwaway password, used to
/// equalize login timing when the submitted email doesn't exist.
///
/// Computed once on first use — the parameters (and therefore the
/// verification cost) match anything [`hash_password`] produces.
fn dummy_password_hash() -> &'static str {
    use std::sync::OnceLock;
    static HASH: OnceLock<String> = OnceLock::new();
    HASH.get_or_init(|| {
        use argon2::password_hash::{PasswordHasher, SaltString, rand_core::OsRng};
        let salt = SaltString::generate(&mut OsRng);
        argon2::Argon2::default()
            .hash_password(b"timing-equalizer-not-a-real-password", &salt)
            .map(|h| h.to_string())
            // A failure here would only cost the timing defense, never
            // correctness — `verify_password` treats a malformed hash as
            // "does not match".
            .unwrap_or_default()
    })
    .as_str()
}

/// Everything worth re-populating from a failed signup submission —
/// the password is deliberately dropped (never echoed back into HTML).
fn signup_prefill(input: &SignupInput) -> crate::templates::SignupPrefill {
    crate::templates::SignupPrefill {
        name: input.name.trim().to_string(),
        email: input.email.trim().to_string(),
        brokerage_name: input.brokerage_name.trim().to_string(),
        city: input
            .city
            .as_deref()
            .map(str::trim)
            .unwrap_or_default()
            .to_string(),
    }
}

async fn render_signup_error(
    state: &AppState,
    message: &str,
    prefill: crate::templates::SignupPrefill,
) -> Result<Response, AppError> {
    let challenge =
        security::issue_challenge(&state.config.jwt_secret, state.config.pow_difficulty_bits)?;
    let page = SignupPage {
        app_name: &state.config.app_name,
        base_url: &state.config.base_url,
        error: Some(message),
        signed_in: false,
        pow_challenge: challenge.challenge,
        pow_difficulty: challenge.difficulty,
        prefill,
    };
    Ok(render(&page)?.into_response())
}

async fn render_login_error(
    state: &AppState,
    message: &str,
    email: String,
) -> Result<Response, AppError> {
    let page = LoginPage {
        app_name: &state.config.app_name,
        base_url: &state.config.base_url,
        error: Some(message),
        signed_in: false,
        email,
        just_reset: false,
        just_verified: false,
    };
    Ok(render(&page)?.into_response())
}

async fn render_verify_failure(state: &AppState, message: &str) -> Result<Response, AppError> {
    let page = VerifyResultPage {
        app_name: &state.config.app_name,
        base_url: &state.config.base_url,
        signed_in: false,
        success: false,
        message,
    };
    Ok(render(&page)?.into_response())
}

/// Public helper used by the team controller after creating an invitation.
///
/// `inviter_email` is wired into the email's `Reply-To` so replies bypass
/// the no-reply From address and land in the actual inviter's inbox.
//
// The argument list is wide because there's no smaller natural grouping —
// every parameter is needed exactly once and a builder would be more
// ceremony than the single internal call site is worth.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn create_invitation(
    state: &AppState,
    email: String,
    role: String,
    brokerage: RecordId,
    brokerage_name: &str,
    invited_by: RecordId,
    inviter_name: &str,
    inviter_email: &str,
    is_existing_user: bool,
) -> Result<Invitation, AppError> {
    let token = generate_token();
    let invite: Option<Invitation> = state
        .db
        .create("invitation")
        .content(NewInvitation {
            email: email.clone(),
            role: role.clone(),
            token: token.clone(),
            brokerage,
            invited_by: invited_by.clone(),
        })
        .await?;
    let invite = invite.ok_or_else(|| {
        AppError::Internal(anyhow::anyhow!("invitation creation returned nothing"))
    })?;

    let link = format!("{}/invite/{}", state.config.base_url, token);
    state
        .mailer
        .send_invite(
            &email,
            inviter_name,
            inviter_email,
            brokerage_name,
            &role,
            &link,
            is_existing_user,
        )
        .await;

    audit::record(
        &state.db,
        "invite_sent",
        Some(invited_by),
        Some(email),
        None,
        None,
        Some(format!("role={role}")),
    )
    .await;

    Ok(invite)
}

fn generate_token() -> String {
    use base64_stub::encode;
    let uuid = uuid::Uuid::now_v7();
    encode(uuid.as_bytes())
}

mod base64_stub {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

    pub fn encode(bytes: &[u8]) -> String {
        let mut out = String::with_capacity((bytes.len() * 4).div_ceil(3));
        let chunks = bytes.chunks(3);
        for chunk in chunks {
            let b0 = chunk[0] as u32;
            let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
            let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
            let triple = (b0 << 16) | (b1 << 8) | b2;
            out.push(ALPHABET[((triple >> 18) & 0x3f) as usize] as char);
            out.push(ALPHABET[((triple >> 12) & 0x3f) as usize] as char);
            if chunk.len() > 1 {
                out.push(ALPHABET[((triple >> 6) & 0x3f) as usize] as char);
            }
            if chunk.len() > 2 {
                out.push(ALPHABET[(triple & 0x3f) as usize] as char);
            }
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Password reset
// ---------------------------------------------------------------------------
//
// Threat model, since this flow is a standing account-takeover primitive:
//
// - **Enumeration.** `POST /forgot` responds identically whether or not
//   the address exists, and does the same visible work either way.
// - **Token strength.** 256 bits from the OS CSPRNG. The signup/invite
//   tokens are UUIDv7 (~74 bits of randomness plus a guessable
//   timestamp) — fine for those, not for a bearer credential that
//   grants takeover.
// - **Token at rest.** Only a SHA-256 hash is stored. A database dump,
//   a backup, or an admin reading rows cannot seize accounts.
// - **Blast radius.** Completing a reset bumps `token_version`, so every
//   session the attacker may already hold dies with it.
// - **Single use + short expiry**, and requesting a new link invalidates
//   the previous one.

/// Hash a reset token for storage/lookup. Deterministic (unlike a
/// password hash) because we must find the row by the presented token;
/// pre-image resistance is what protects the stored value, and the
/// token's own 256 bits of entropy make brute force irrelevant.
fn hash_reset_token(token: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    hex::encode(hasher.finalize())
}

/// 256 bits of CSPRNG output, URL-safe. `OsRng` (not `thread_rng`) so
/// the source is unambiguously the OS entropy pool.
fn generate_reset_token() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    base64_stub::encode(&bytes)
}

pub async fn forgot_form(
    State(state): State<AppState>,
    MaybeCurrentUser(current): MaybeCurrentUser,
) -> Result<Response, AppError> {
    if current.is_some() {
        return Ok(Redirect::to("/app").into_response());
    }
    Ok(render(&ForgotPasswordPage {
        app_name: &state.config.app_name,
        base_url: &state.config.base_url,
        signed_in: false,
        error: None,
        sent: false,
        email: String::new(),
    })?
    .into_response())
}

#[derive(Debug, Deserialize)]
pub struct ForgotInput {
    pub email: String,
}

/// `POST /forgot` — issue a reset link.
///
/// Always renders the same "check your inbox" confirmation. An unknown
/// address, an unverified account, and a successful send are
/// indistinguishable to the caller.
pub async fn forgot(
    State(state): State<AppState>,
    headers: HeaderMap,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Form(input): Form<ForgotInput>,
) -> Result<Response, AppError> {
    let ip = client_ip(&headers, Some(&peer), state.config.trusted_proxy_hops);
    let ua = user_agent(&headers);
    let email = input.email.trim().to_ascii_lowercase();

    // The confirmation every caller sees, whatever actually happened.
    let confirm = |state: &AppState| {
        render(&ForgotPasswordPage {
            app_name: &state.config.app_name,
            base_url: &state.config.base_url,
            signed_in: false,
            error: None,
            sent: true,
            email: String::new(),
        })
    };

    // Throttle per IP *and* per address: the first bounds mass
    // enumeration, the second stops someone mailbombing one person by
    // replaying the form.
    //
    // The `&&` short-circuit is deliberate. Evaluating both arms
    // unconditionally used to insert a limiter entry keyed on the
    // submitted address *even when the IP was already throttled*, handing
    // an attacker an unbounded supply of attacker-named keys — enough to
    // drive the limiter map past its ceiling and, back when eviction was a
    // `clear()`, wipe the brute-force defenses on every other route.
    // `throttle_key` additionally caps each entry at a fixed size.
    let allowed = allow_per_hour(&state.rate_limiter, &format!("forgot-ip:{ip}"), 10)
        && allow_per_hour(
            &state.rate_limiter,
            &throttle_key("forgot-email", &email),
            5,
        );
    if !allowed {
        audit::record(
            &state.db,
            "password_reset_failed",
            None,
            Some(email),
            Some(ip),
            ua,
            Some("rate limited".into()),
        )
        .await;
        return Ok(confirm(&state)?.into_response());
    }

    let mut q = state
        .db
        .query("SELECT * FROM user WHERE email = $e LIMIT 1")
        .bind(("e", email.clone()))
        .await?;
    let user: Option<User> = q.take(0)?;

    if let Some(user) = user {
        let token = generate_reset_token();
        let expiry = Utc::now() + Duration::hours(state.config.reset_expiry_hours);
        state
            .db
            .query("UPDATE $u SET reset_token_hash = $h, reset_expires = $x")
            .bind(("u", user.id.clone()))
            .bind(("h", hash_reset_token(&token)))
            .bind(("x", expiry))
            .await?;

        // Send off the request path. Awaiting here would make this branch
        // take an HTTPS round-trip to Postmark (~100-400ms) that the
        // unknown-address branch below does not, turning the uniform
        // response into a timing oracle that answers "is this address
        // registered?" without any statistics. Detaching it equalizes the
        // two paths; a send failure is already logged inside the mailer.
        let link = format!("{}/reset/{token}", state.config.base_url);
        let mailer = state.mailer.clone();
        let (to, name) = (user.email.clone(), user.name.clone());
        let window = state.config.reset_expiry_hours;
        tokio::spawn(async move {
            mailer.send_password_reset(&to, &name, &link, window).await;
        });

        audit::record(
            &state.db,
            "password_reset_requested",
            Some(user.id.clone()),
            Some(user.email.clone()),
            Some(ip),
            ua,
            None,
        )
        .await;
    } else {
        // Unknown address: record the attempt (useful signal when
        // someone is probing) but tell the caller nothing.
        audit::record(
            &state.db,
            "password_reset_failed",
            None,
            Some(email),
            Some(ip),
            ua,
            Some("no account".into()),
        )
        .await;
    }

    Ok(confirm(&state)?.into_response())
}

/// Look up the account a reset token belongs to, enforcing single-use
/// and expiry. Returns `None` for unknown, already-used, or expired
/// tokens — the caller must not distinguish between them.
async fn user_for_reset_token(state: &AppState, token: &str) -> Result<Option<User>, AppError> {
    let mut q = state
        .db
        .query(
            "SELECT * FROM user \
             WHERE reset_token_hash = $h AND reset_expires > time::now() \
             LIMIT 1",
        )
        .bind(("h", hash_reset_token(token)))
        .await?;
    Ok(q.take(0)?)
}

pub async fn reset_form(
    State(state): State<AppState>,
    Path(token): Path<String>,
) -> Result<Response, AppError> {
    let valid = user_for_reset_token(&state, &token).await?.is_some();
    Ok(render(&ResetPasswordPage {
        app_name: &state.config.app_name,
        base_url: &state.config.base_url,
        signed_in: false,
        token,
        error: None,
        valid,
    })?
    .into_response())
}

#[derive(Debug, Deserialize)]
pub struct ResetInput {
    pub new_password: String,
    pub confirm_password: String,
}

/// `POST /reset/{token}` — set the new password.
///
/// On success: writes the hash, **revokes every existing session** for
/// the account (the point of a reset — an attacker holding a stolen
/// cookie loses it here), clears the token so the link can't be
/// replayed, and marks the address verified, since receiving the mail
/// proves inbox control.
pub async fn reset(
    State(state): State<AppState>,
    headers: HeaderMap,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(token): Path<String>,
    Form(input): Form<ResetInput>,
) -> Result<Response, AppError> {
    let ip = client_ip(&headers, Some(&peer), state.config.trusted_proxy_hops);
    let ua = user_agent(&headers);

    let invalid_page = |state: &AppState, token: String, msg: Option<&str>, valid: bool| {
        render(&ResetPasswordPage {
            app_name: &state.config.app_name,
            base_url: &state.config.base_url,
            signed_in: false,
            token,
            error: msg,
            valid,
        })
    };

    let Some(user) = user_for_reset_token(&state, &token).await? else {
        audit::record(
            &state.db,
            "password_reset_failed",
            None,
            None,
            Some(ip),
            ua,
            Some("invalid or expired token".into()),
        )
        .await;
        return Ok(invalid_page(&state, token, None, false)?.into_response());
    };

    if input.new_password.len() < 8 {
        return Ok(invalid_page(
            &state,
            token,
            Some("Password must be at least 8 characters."),
            true,
        )?
        .into_response());
    }
    if input.new_password != input.confirm_password {
        return Ok(invalid_page(
            &state,
            token,
            Some("The two password fields don't match."),
            true,
        )?
        .into_response());
    }

    let hashed = hash_password(&input.new_password).await?;
    state
        .db
        .query(
            "UPDATE $u SET password_hash = $h, \
             reset_token_hash = NONE, reset_expires = NONE, \
             email_verified = true, \
             verification_token = NONE, verification_expires = NONE",
        )
        .bind(("u", user.id.clone()))
        .bind(("h", hashed))
        .await?;

    // Kill every session that existed before this reset.
    bump_token_version(&state, &user.id).await;

    audit::record(
        &state.db,
        "password_reset_completed",
        Some(user.id.clone()),
        Some(user.email.clone()),
        Some(ip),
        ua,
        None,
    )
    .await;

    // Deliberately NOT signing them in: a fresh login proves they know
    // the password they just set, and avoids handing a session to
    // whoever happened to open the link.
    Ok(Redirect::to("/login?reset=1").into_response())
}
