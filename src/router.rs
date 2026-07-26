//! HTTP route table. Public marketing pages sit at the root, the app proper
//! is namespaced under `/app`, and the privileged super-admin views under
//! `/admin`. Fragment endpoints return HTML chunks so Datastar can swap
//! them straight into the DOM without client-side rendering.

use std::time::Duration;

use axum::Router;
use axum::extract::Request;
use axum::middleware::{Next, from_fn};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use tower_cookies::CookieManagerLayer;
use tower_http::compression::CompressionLayer;
use tower_http::services::ServeDir;
use tower_http::trace::{DefaultOnFailure, DefaultOnResponse, TraceLayer};
use tracing::Level;

use crate::controllers::{
    admin, auth, checklists, comments, documents, forms, health, marketing, members, orphan,
    profile, subscribe, tiers, transactions, webhooks,
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
        .route("/forgot", get(auth::forgot_form).post(auth::forgot))
        .route("/reset/{token}", get(auth::reset_form).post(auth::reset))
        .route("/logout", post(auth::logout))
        .route(
            "/invite/{token}",
            get(auth::invite_form).post(auth::accept_invite),
        );

    let app = Router::new()
        // `/app` and `/app/transactions` render the same view (the
        // transactions list with stats + sortable headers). The list
        // handler detects which URL it was reached at and sets the
        // nav highlight accordingly.
        .route("/app", get(transactions::list))
        .route("/app/stats", get(transactions::stats_fragment))
        .route("/app/stats/stream", get(transactions::stats_stream))
        .route("/app/search", get(transactions::search))
        .route(
            "/app/transactions",
            get(transactions::list).post(transactions::create),
        )
        .route("/app/transactions/new", get(transactions::new_form))
        .route(
            "/app/transactions/unassigned",
            get(transactions::unassigned_list),
        )
        .route("/app/transactions/reassign", post(transactions::reassign))
        .route("/app/transactions/{id}", get(transactions::show))
        .route(
            "/app/transactions/{id}/edit",
            get(transactions::edit_form).post(transactions::update),
        )
        .route(
            "/app/transactions/{id}/status",
            post(transactions::update_status),
        )
        .route("/app/transactions/{id}/delete", post(transactions::delete))
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
        .route("/app/documents/{id}/preview", get(documents::preview))
        .route("/app/documents/{id}/delete", post(documents::delete))
        .route("/app/team", get(members::list))
        .route("/app/team/export", get(documents::export_brokerage_zip))
        .route(
            "/app/team/{user_id}/export",
            get(documents::export_member_zip),
        )
        .route("/app/team/invite", post(members::invite))
        .route(
            "/app/team/invite/{token}/resend",
            post(members::resend_invite),
        )
        .route(
            "/app/team/invite/{token}/cancel",
            post(members::cancel_invite),
        )
        .route("/app/team/{user_id}/role", post(members::change_role))
        .route("/app/team/{user_id}/remove", post(members::remove_member))
        .route("/app/team/audit", get(members::audit_log))
        .route(
            "/app/team/delete-brokerage",
            get(members::delete_brokerage_form).post(members::delete_brokerage),
        )
        .route("/app/profile", get(profile::show).post(profile::update))
        .route("/app/profile/password", post(profile::change_password))
        // Avatar uploads get their own small body limit. The handler
        // rejects >8 MB, but only AFTER buffering the whole field, so
        // without this every request could pin the global 100 MB cap.
        .route(
            "/app/profile/avatar",
            post(profile::upload_avatar)
                .layer(axum::extract::DefaultBodyLimit::max(8 * 1024 * 1024)),
        )
        .route("/app/profile/avatar/delete", post(profile::delete_avatar))
        .route("/app/users/{key}/avatar", get(profile::serve_avatar))
        // Registered BEFORE the `{slug}` route so "return" is matched
        // literally rather than being read as a tier slug.
        .route("/app/subscribe/return", get(subscribe::checkout_return))
        .route("/app/subscribe/{slug}", get(subscribe::subscribe))
        .route("/app/billing/portal", get(subscribe::portal))
        .route("/app/no-brokerage", get(orphan::landing))
        .route("/app/invites/{token}/accept", post(orphan::accept))
        .route("/app/invites/{token}/decline", post(orphan::decline))
        .route("/app/forms", get(forms::broker_forms))
        .route("/app/forms/locality", post(forms::set_locality))
        .route("/app/forms/hide/{key}", post(forms::toggle_hide))
        .route("/app/forms/custom", post(forms::add_custom));

    let admin_routes = Router::new()
        .route("/admin", get(admin::users))
        .route("/admin/brokerages", get(admin::brokerages))
        .route("/admin/brokerages/{key}", get(admin::brokerage_detail))
        .route(
            "/admin/brokerages/{key}/comp",
            post(admin::toggle_brokerage_comp),
        )
        .route("/admin/tiers", get(tiers::list))
        .route("/admin/tiers/relink", post(tiers::relink_stripe))
        .route("/admin/tiers/new", get(tiers::new_form).post(tiers::create))
        .route(
            "/admin/tiers/{key}/edit",
            get(tiers::edit_form).post(tiers::update),
        )
        .route("/admin/forms", get(forms::admin_list))
        .route("/admin/forms/sets", post(forms::admin_create_set))
        .route("/admin/forms/{key}", get(forms::admin_set_detail))
        .route("/admin/forms/{key}/rename", post(forms::admin_rename_set))
        .route("/admin/forms/{key}/delete", post(forms::admin_delete_set))
        .route("/admin/forms/{key}/groups", post(forms::admin_add_group))
        .route(
            "/admin/forms/{key}/groups/{group_key}/rename",
            post(forms::admin_rename_group),
        )
        .route(
            "/admin/forms/{key}/groups/{group_key}/delete",
            post(forms::admin_delete_group),
        )
        .route("/admin/forms/{key}/forms", post(forms::admin_add_form))
        .route(
            "/admin/forms/{key}/reorder_groups",
            post(forms::admin_reorder_groups),
        )
        .route(
            "/admin/forms/{key}/reorder_forms",
            post(forms::admin_reorder_forms),
        )
        .route(
            "/admin/forms/{key}/forms/{form_key}/toggle",
            post(forms::admin_toggle_form),
        )
        .route(
            "/admin/forms/{key}/forms/{form_key}/delete",
            post(forms::admin_delete_form),
        )
        .route(
            "/admin/forms/{key}/forms/{form_key}/edit",
            get(forms::admin_edit_form).post(forms::admin_update_form),
        )
        .route("/admin/audit", get(admin::audit_log))
        .route("/admin/errors", get(admin::error_log))
        .route("/admin/changelog", get(admin::changelog));

    let base = Router::new()
        .merge(public)
        .merge(app)
        .merge(admin_routes)
        .route("/webhooks/stripe", post(webhooks::stripe))
        .route("/healthcheck", get(health::healthcheck));

    // A route that always panics, so tests can assert what the panic
    // path actually returns through the real middleware stack. Compiled
    // out of every non-test build.
    #[cfg(test)]
    let base = base
        .route("/__test/panic", get(always_panics))
        .route("/__test/own-headers", get(sets_own_headers));

    base.nest_service("/static", ServeDir::new("static"))
        .layer(CookieManagerLayer::new())
        .layer(CompressionLayer::new())
        .layer(
            TraceLayer::new_for_http()
                // Custom span instead of `DefaultMakeSpan`, purely to
                // keep secrets out of the highest-volume log we emit:
                // the default records the raw `uri`, and reset / verify
                // / invite tokens travel as path segments, so every one
                // of those links was printed in full at INFO on the
                // request that used it.
                .make_span_with(|req: &Request| {
                    tracing::info_span!(
                        "request",
                        method = %req.method(),
                        uri = %crate::sanitize::redact_secret_path(req.uri().path()),
                        version = ?req.version(),
                    )
                })
                .on_response(DefaultOnResponse::new().level(Level::INFO))
                .on_failure(DefaultOnFailure::new().level(Level::ERROR)),
        )
        // Signed-out visitors navigating to an authenticated page get
        // the login screen, not a bare 401 error page. See the fn doc
        // for why only NAVIGATIONS are rewritten.
        .layer(from_fn(redirect_unauthenticated))
        // CSRF: reject state-changing requests a browser reports as
        // coming from another site. Sits above the handlers so it runs
        // before any extractor or DB work.
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            reject_cross_site_writes,
        ))
        // Safety-net 5xx logger. `AppError::IntoResponse` already logs
        // every internal error it produces, but any response with a
        // 5xx status — including ones synthesized by middleware, body
        // limits, or an extractor rejection we forgot to handle —
        // should leave a breadcrumb. Without this layer the previous
        // upload bug returned a "naked" 500 to the browser and we had
        // to guess at the cause; now every 5xx surfaces here at least.
        .layer(from_fn(log_5xx))
        // Catch panics inside handlers so they surface in the logs as
        // tracing errors instead of disappearing into Axum's default
        // empty-500 fallback. Without this layer, a panic in any
        // handler returns 500 with zero log output — exactly the
        // symptom that turned a checklist render bug into a guessing
        // game.
        .layer(tower_http::catch_panic::CatchPanicLayer::custom(
            handle_panic,
        ))
        // Error telemetry: persist 5xx + meaningful 4xx responses to
        // the DB for /admin/errors (detached write; adds no request
        // latency). Sits OUTSIDE the panic catcher so even synthesized
        // panic-500s get a row.
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::audit::capture_errors,
        ))
        .with_state(state.clone())
        .layer(axum::extract::DefaultBodyLimit::max(100 * 1024 * 1024))
        .layer(tower_http::timeout::TimeoutLayer::with_status_code(
            axum::http::StatusCode::GATEWAY_TIMEOUT,
            Duration::from_secs(60),
        ))
        // Baseline security headers on every response (CSP,
        // frame-ancestors, nosniff, referrer policy, HSTS on https).
        //
        // Deliberately the OUTERMOST layer. `.layer()` calls wrap the
        // ones added before them, so while this sat further up the list
        // it ran inside `CatchPanicLayer` and `TimeoutLayer` — meaning
        // the two response kinds those layers synthesize, a panic-500
        // and a 504, were the only ones served with no CSP, no
        // `nosniff`, no `X-Frame-Options` and no HSTS. Out here it sees
        // every response, however it was produced.
        .layer(axum::middleware::from_fn_with_state(
            state,
            security_headers,
        ))
}

/// Reject state-changing requests that a browser tells us came from
/// another site.
///
/// The app authenticates with a cookie and every form is a plain
/// `<form method="post">` with no CSRF token, so `SameSite=Lax` was the
/// only thing preventing cross-site POSTs. Lax is scoped to the
/// registrable domain, not the origin, which leaves two real holes:
/// any sibling subdomain (`blog.`, a staging box, anything with a
/// wildcard DNS record) counts as *same-site* and could POST to
/// `/app/team/delete-brokerage` with the session cookie attached.
///
/// Fetch Metadata closes that without per-form tokens: browsers send
/// `Sec-Fetch-Site` on every request and it cannot be forged by page
/// script. We allow only `same-origin` (our own pages) and `none`
/// (typed/bookmarked), which rejects `cross-site` AND `same-site`.
///
/// Non-browser clients send no `Sec-Fetch-Site` — we fall back to
/// comparing `Origin` against `BASE_URL`, and allow the request when
/// neither header is present. That is deliberate: it keeps the Stripe
/// webhook (`POST /webhooks/stripe`, authenticated by signature) and
/// any future API client working, while browsers — the only things
/// that carry ambient cookies — are always covered.
/// Handler behind the test-only `/__test/panic` route.
///
/// Lets the test suite assert what a panicking handler actually returns
/// through the real middleware stack — notably that `security_headers`
/// still decorates the 500 `CatchPanicLayer` synthesizes.
#[cfg(test)]
async fn always_panics() -> Response {
    panic!("deliberate test panic")
}

/// Handler behind the test-only `/__test/own-headers` route.
///
/// Mirrors what `documents::preview` does — sets its own CSP and
/// `X-Frame-Options` — so the suite can assert `security_headers` treats
/// those as authoritative rather than overwriting them.
#[cfg(test)]
async fn sets_own_headers() -> Response {
    (
        [
            (
                "content-security-policy",
                "sandbox; frame-ancestors \'self\'",
            ),
            ("x-frame-options", "SAMEORIGIN"),
        ],
        "ok",
    )
        .into_response()
}

/// Reduce a URL to its bare `scheme://host[:port]`, lowercased.
///
/// `BASE_URL` may carry a trailing slash or a path; an `Origin` header
/// never does, but normalizing both sides means the comparison can't be
/// broken by a stray slash in configuration.
fn origin_of(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    if authority.is_empty() {
        return None;
    }
    Some(format!(
        "{}://{}",
        scheme.to_ascii_lowercase(),
        authority.to_ascii_lowercase()
    ))
}

/// True when `origin` names exactly the same origin as `base_url`.
///
/// Substring and prefix comparisons are unsafe here in both directions:
/// `starts_with(origin)` accepts a shorter attacker domain that happens
/// to prefix ours, and `contains` would accept
/// `https://app.example.com.evil.test`. Only full equality of the
/// normalized `scheme://host[:port]` triple is sound.
pub(crate) fn origin_matches(base_url: &str, origin: &str) -> bool {
    match (origin_of(base_url), origin_of(origin)) {
        (Some(a), Some(b)) => a == b,
        // An unparseable origin (including the literal `null` that
        // sandboxed iframes and some redirects send) is never a match.
        _ => false,
    }
}

async fn reject_cross_site_writes(
    axum::extract::State(state): axum::extract::State<AppState>,
    req: Request,
    next: Next,
) -> Response {
    let unsafe_method = matches!(
        *req.method(),
        axum::http::Method::POST
            | axum::http::Method::PUT
            | axum::http::Method::PATCH
            | axum::http::Method::DELETE
    );
    if !unsafe_method {
        return next.run(req).await;
    }

    // Read both headers up front so nothing borrows `req` across the
    // move into `next.run`.
    let fetch_site = req
        .headers()
        .get("sec-fetch-site")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let origin = req
        .headers()
        .get(axum::http::header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let described = format!("{} {}", req.method(), req.uri().path());

    let allowed = match fetch_site.as_deref() {
        Some("same-origin") | Some("none") => true,
        // `cross-site` and `same-site` are both rejected — see the doc
        // comment on why same-site is not good enough here.
        Some(_) => false,
        None => match origin.as_deref() {
            // Exact `scheme://host[:port]` equality, both sides normalized.
            //
            // This was `base_url.starts_with(origin)`, which asks the
            // question backwards: it accepts any origin that is a *prefix*
            // of BASE_URL. With BASE_URL `https://app.transactvault.com`,
            // an attacker holding the real domain `app.transactvault.co`
            // passed — as did `https://app` and `https:`.
            Some(origin) => origin_matches(&state.config.base_url, origin),
            None => true,
        },
    };

    if allowed {
        next.run(req).await
    } else {
        tracing::warn!(request = %described, "blocked cross-site state-changing request");
        (
            axum::http::StatusCode::FORBIDDEN,
            axum::response::Html(
                "<!doctype html><title>403</title><h1>403</h1>\
                 <p>This request looked like it came from another site, so it was blocked. \
                 If you reached this page normally, please go back and try again.</p>",
            ),
        )
            .into_response()
    }
}

/// Attach baseline security headers to every response.
///
/// - `frame-ancestors` / `X-Frame-Options` stop clickjacking, which was
///   wide open: a hostile page could frame the app and trick a signed-in
///   broker into clicking Approve or Delete.
/// - `form-action 'self'` stops an injected form from posting the user's
///   data off-site; `base-uri 'self'` stops `<base>` hijacking of every
///   relative URL on the page.
/// - HSTS is emitted **only** when BASE_URL is https, because the header
///   is sticky in browsers — shipping it from a local http build would
///   poison the developer's browser for that host.
///
/// `style-src` includes jsDelivr because the avatar cropper ships a
/// stylesheet from there (`pages/profile.html`) — omitting it silently
/// rendered the cropper unstyled. Both CDN assets carry SRI hashes, so
/// a compromised CDN cannot substitute different bytes.
///
/// `img-src` must list `blob:`. The avatar cropper shows the picked file
/// with `URL.createObjectURL(file)` and previews the crop the same way;
/// with only `data:` allowed, the `<img>` never loaded, its `onload`
/// never fired, and the handler that calls `dialog.showModal()` hung off
/// that event — so choosing a photo appeared to do nothing at all.
/// `blob:` is same-origin-only and cannot be forged by injected markup
/// into a cross-origin fetch, so this costs nothing defensively.
///
/// The script policy is honest about what this app needs: Datastar
/// compiles its `data-*` expressions with the `Function` constructor
/// (`'unsafe-eval'`), the templates carry inline `<script>` blocks
/// (`'unsafe-inline'`), and the bundle is served from jsDelivr. So CSP
/// is defense in depth here, not an XSS cure — the cure is not
/// interpolating untrusted data into those sinks (see
/// `canonical_status_filter`). Self-hosting the bundle and moving the
/// inline blocks into files would let this tighten considerably.
async fn security_headers(
    axum::extract::State(state): axum::extract::State<AppState>,
    req: Request,
    next: Next,
) -> Response {
    use axum::http::HeaderValue;

    let https = state.config.base_url.starts_with("https://");
    let mut response = next.run(req).await;
    let headers = response.headers_mut();

    // These two are DEFAULTS, not mandates: a handler that has already
    // set its own is making a deliberate, better-informed choice, and
    // overwriting it silently breaks things.
    //
    // `documents::preview` is the case in point. It serves attachment
    // bytes to be framed by the same-origin preview lightbox and sets
    // its own `sandbox` policy for the formats where that matters. The
    // unconditional `insert` here replaced that with the page policy —
    // whose `frame-ancestors 'none'` blocked the frame outright, so PDF
    // preview showed nothing, while simultaneously *discarding* the
    // sandbox that stops a malicious SVG running script in our origin.
    if !headers.contains_key("content-security-policy") {
        headers.insert(
            "content-security-policy",
            HeaderValue::from_static(
                "default-src 'self'; \
                 script-src 'self' 'unsafe-inline' 'unsafe-eval' https://cdn.jsdelivr.net; \
                 style-src 'self' 'unsafe-inline' https://cdn.jsdelivr.net; \
                 img-src 'self' data: blob:; \
                 connect-src 'self'; \
                 font-src 'self'; \
                 object-src 'none'; \
                 base-uri 'self'; \
                 form-action 'self'; \
                 frame-ancestors 'none'",
            ),
        );
    }
    if !headers.contains_key("x-frame-options") {
        headers.insert("x-frame-options", HeaderValue::from_static("DENY"));
    }
    headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        "referrer-policy",
        HeaderValue::from_static("strict-origin-when-cross-origin"),
    );
    if https {
        headers.insert(
            "strict-transport-security",
            HeaderValue::from_static("max-age=31536000; includeSubDomains"),
        );
    }
    response
}

/// Turn a 401 on an `/app/*` or `/admin/*` BROWSER NAVIGATION into a
/// redirect to `/login`. Without this, opening a bookmarked app URL in
/// a fresh browser (incognito, new machine, expired session) lands on
/// a bare "401 — Please sign in to continue" error page instead of the
/// login screen.
///
/// Only navigations are rewritten. Programmatic requests — the
/// Datastar stats stream, live-search fragment fetches, infinite
/// scroll — must keep their plain 401: a redirect would hand them the
/// login page's HTML, which Datastar would happily morph into the
/// results region. Detection:
/// - `Sec-Fetch-Mode: navigate` (every modern browser sends it on real
///   navigations; fetch()/EventSource send `cors`/`same-origin`);
/// - fallback for clients without Sec-Fetch metadata: an
///   `Accept: text/html` request that is NOT marked by Datastar's
///   `Datastar-Request` header.
async fn redirect_unauthenticated(req: Request, next: Next) -> Response {
    let gated_path = {
        let p = req.uri().path();
        p.starts_with("/app") || p.starts_with("/admin")
    };
    let is_navigation = match req.headers().get("sec-fetch-mode") {
        Some(mode) => mode.as_bytes() == b"navigate",
        None => {
            !req.headers().contains_key("datastar-request")
                && req
                    .headers()
                    .get(axum::http::header::ACCEPT)
                    .and_then(|v| v.to_str().ok())
                    .map(|a| a.contains("text/html"))
                    .unwrap_or(false)
        }
    };

    let response = next.run(req).await;
    if gated_path && is_navigation && response.status() == axum::http::StatusCode::UNAUTHORIZED {
        return axum::response::Redirect::to("/login").into_response();
    }
    response
}

/// Catch-all 5xx logger. Wraps every handler so a server-error
/// response is guaranteed to leave a tracing record, even if it was
/// produced by an extractor rejection, a middleware layer, or some
/// future code path that bypasses [`crate::error::AppError`]. Logs
/// the method + path + status so the breadcrumb is enough to start
/// diagnosing without a panic backtrace.
async fn log_5xx(req: Request, next: Next) -> Response {
    let method = req.method().clone();
    let uri = req.uri().clone();
    let response = next.run(req).await;
    let status = response.status();
    if status.is_server_error() {
        // Path redacted: reset / verify / invite tokens are path
        // segments, and a 5xx during a reset would otherwise print a
        // live token straight into the log stream.
        let path = crate::sanitize::redact_secret_path(uri.path()).into_owned();
        let target = match uri.query() {
            Some(q) => format!("{path}?{q}"),
            None => path,
        };
        tracing::error!(
            %status,
            %method,
            uri = %target,
            "5xx response leaving the server"
        );
    }
    response
}

/// Panic handler for `CatchPanicLayer`. Logs the panic payload + the
/// backtrace if `RUST_BACKTRACE` is set, then returns a generic 500.
fn handle_panic(err: Box<dyn std::any::Any + Send + 'static>) -> axum::response::Response {
    let msg = if let Some(s) = err.downcast_ref::<String>() {
        s.clone()
    } else if let Some(s) = err.downcast_ref::<&'static str>() {
        (*s).to_string()
    } else {
        "<non-string panic payload>".into()
    };
    tracing::error!(panic = %msg, "handler panic — returning 500");
    let body = axum::body::Body::from(
        "<!doctype html><title>500</title><h1>500</h1><p>Something went wrong. Please try again.</p>",
    );
    axum::response::Response::builder()
        .status(axum::http::StatusCode::INTERNAL_SERVER_ERROR)
        .header(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8")
        .body(body)
        .unwrap_or_else(|_| axum::response::Response::new(axum::body::Body::empty()))
}
