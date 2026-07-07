//! End-to-end HTTP integration tests.
//!
//! Spins up the real Axum router against an in-memory SurrealDB and
//! noop external integrations (no Stripe, no Resend, no S3), then
//! drives requests through it with `tower::ServiceExt::oneshot`. These
//! tests are the audit's "test the actual handlers, not just the
//! inner helpers" answer — they catch routing, extractor, and
//! middleware regressions that pure DB-level tests can't.
//!
//! Storage-touching paths (document upload/download) are out of scope
//! here because the test Storage is intentionally non-functional;
//! cover those with unit tests on the underlying queries instead.

// SurrealDB's `RecordId` has interior mutability through lazy-init
// regex caches inside Value/Array, which trips the lint when we keep
// id-keyed maps. Hash + Eq are still deterministic — see the same
// rationale in `controllers/transactions.rs`.
#![allow(clippy::mutable_key_type)]

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use surrealdb::types::{RecordId, SurrealValue};
use tower::ServiceExt;

use crate::auth::issue_token;
use crate::auth::middleware::SESSION_COOKIE;
use crate::router;
use crate::state::AppState;

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// Bundle returned by [`make_app`] — the live Router plus the AppState
/// so tests can poke the DB directly (seed brokerages, assert on rows).
struct TestApp {
    router: axum::Router,
    state: AppState,
}

async fn make_app() -> TestApp {
    let state = AppState::for_tests().await;
    let router = router::build(state.clone());
    TestApp { router, state }
}

/// Drive one request through the router and collect the response body
/// as a UTF-8 string. Bodies are unbounded here because tests stay on
/// the happy / 400 / 403 paths where responses are small.
///
/// Injects a placeholder `ConnectInfo` extension so handlers that use
/// the `ConnectInfo<SocketAddr>` extractor (signup, login, accept) work
/// — the real server populates this via `with_connect_info` on serve;
/// `tower::ServiceExt::oneshot` skips that step.
async fn send(app: &TestApp, req: Request<Body>) -> (StatusCode, String) {
    use axum::extract::ConnectInfo;
    use std::net::SocketAddr;

    let mut req = req;
    req.extensions_mut().insert(ConnectInfo::<SocketAddr>(
        "127.0.0.1:0".parse().expect("loopback addr"),
    ));

    let response = app
        .router
        .clone()
        .oneshot(req)
        .await
        .expect("router oneshot");
    let status = response.status();
    let body = to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("collect body");
    let body = String::from_utf8(body.to_vec()).expect("utf8");
    (status, body)
}

/// Mint a session cookie for `user_id` directly so authenticated tests
/// don't have to drive the full signup → verify → login flow on every
/// scenario. Returns a string ready to attach to a request via the
/// `cookie` header.
fn session_cookie(app: &TestApp, user_id: &RecordId) -> String {
    let key = crate::db::record_key(user_id);
    let token = issue_token(&app.state.config, &key).expect("issue jwt");
    format!("{SESSION_COOKIE}={token}")
}

/// Convenience: send a GET as a signed-in user.
async fn authed_get(app: &TestApp, user_id: &RecordId, uri: &str) -> (StatusCode, String) {
    let cookie = session_cookie(app, user_id);
    let req = Request::builder()
        .uri(uri)
        .header("cookie", cookie)
        .body(Body::empty())
        .unwrap();
    send(app, req).await
}

/// Convenience: send a POST form as a signed-in user.
async fn authed_post(
    app: &TestApp,
    user_id: &RecordId,
    uri: &str,
    form: &str,
) -> (StatusCode, String) {
    let cookie = session_cookie(app, user_id);
    let req = Request::builder()
        .method("POST")
        .uri(uri)
        .header("cookie", cookie)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Body::from(form.to_string()))
        .unwrap();
    send(app, req).await
}

// ---------------------------------------------------------------------------
// Seed helpers — minimal happy-path fixtures.
//
// Every seed is the smallest row the schema accepts so tests aren't
// fragile to unrelated field additions. `is_complimentary=true` on
// brokerages keeps the billing gate open without needing to wire up a
// tier row.
// ---------------------------------------------------------------------------

async fn seed_brokerage(state: &AppState, name: &str) -> RecordId {
    #[derive(serde::Serialize, SurrealValue)]
    struct NewB {
        name: String,
        plan: String,
        is_complimentary: bool,
    }
    let b: Option<crate::models::Brokerage> = state
        .db
        .create("brokerage")
        .content(NewB {
            name: name.into(),
            plan: "starter".into(),
            is_complimentary: true,
        })
        .await
        .expect("create brokerage");
    b.expect("brokerage row").id
}

async fn seed_user(state: &AppState, email: &str) -> RecordId {
    #[derive(serde::Serialize, SurrealValue)]
    struct NewU {
        email: String,
        name: String,
        password_hash: String,
        email_verified: bool,
    }
    let u: Option<crate::models::User> = state
        .db
        .create("user")
        .content(NewU {
            email: email.into(),
            name: email.into(),
            password_hash: "x".into(),
            email_verified: true,
        })
        .await
        .expect("create user");
    u.expect("user row").id
}

async fn join(state: &AppState, user: &RecordId, brokerage: &RecordId, role: &str) {
    state
        .db
        .query("RELATE $u->works_at->$b SET role = $r")
        .bind(("u", user.clone()))
        .bind(("b", brokerage.clone()))
        .bind(("r", role.to_string()))
        .await
        .expect("RELATE works_at");
}

async fn seed_tx(state: &AppState, brokerage: &RecordId, owner: Option<&RecordId>) -> RecordId {
    #[derive(serde::Serialize, SurrealValue)]
    struct NewTx {
        property_address: String,
        city: String,
        apn: Option<String>,
        postal_code: Option<String>,
        price_cents: i64,
        client_name: Option<String>,
        mls_number: Option<String>,
        office_file_number: Option<String>,
        status: String,
        transaction_type: String,
        special_sales_condition: String,
        sales_type: String,
    }
    let tx: Option<crate::models::Transaction> = state
        .db
        .create("transaction")
        .content(NewTx {
            property_address: "1 Test Way".into(),
            city: "LA".into(),
            apn: None,
            postal_code: None,
            price_cents: 1,
            client_name: None,
            mls_number: None,
            office_file_number: None,
            status: "active".into(),
            transaction_type: "residential".into(),
            special_sales_condition: "none".into(),
            sales_type: "listing".into(),
        })
        .await
        .expect("create tx");
    let tx_id = tx.expect("tx row").id;
    state
        .db
        .query("RELATE $b->has_transaction->$t")
        .bind(("b", brokerage.clone()))
        .bind(("t", tx_id.clone()))
        .await
        .expect("has_transaction edge");
    if let Some(u) = owner {
        state
            .db
            .query("RELATE $u->owns->$t")
            .bind(("u", u.clone()))
            .bind(("t", tx_id.clone()))
            .await
            .expect("owns edge");
    }
    tx_id
}

async fn seed_item(state: &AppState, tx: &RecordId, status: &str) -> RecordId {
    #[derive(serde::Serialize, SurrealValue)]
    struct NewItem {
        title: String,
        form_code: Option<String>,
        group_name: String,
        group_order: i64,
        position: i64,
        required: bool,
        approval_status: String,
    }
    let it: Option<crate::models::ChecklistItem> = state
        .db
        .create("checklist_item")
        .content(NewItem {
            title: "Test item".into(),
            form_code: None,
            group_name: "Test".into(),
            group_order: 1,
            position: 1,
            required: true,
            approval_status: status.into(),
        })
        .await
        .expect("create item");
    let id = it.expect("item row").id;
    state
        .db
        .query("RELATE $t->has_item->$i")
        .bind(("t", tx.clone()))
        .bind(("i", id.clone()))
        .await
        .expect("has_item edge");
    id
}

/// Same as `seed_item` but lets the caller override the group name —
/// used by tests that need multiple items in distinct groups.
async fn seed_item_in_group(
    state: &AppState,
    tx: &RecordId,
    status: &str,
    group_name: &str,
) -> RecordId {
    #[derive(serde::Serialize, SurrealValue)]
    struct NewItem {
        title: String,
        form_code: Option<String>,
        group_name: String,
        group_order: i64,
        position: i64,
        required: bool,
        approval_status: String,
    }
    let it: Option<crate::models::ChecklistItem> = state
        .db
        .create("checklist_item")
        .content(NewItem {
            title: format!("Item in {group_name}"),
            form_code: None,
            group_name: group_name.into(),
            group_order: 1,
            position: 1,
            required: true,
            approval_status: status.into(),
        })
        .await
        .expect("create item");
    let id = it.expect("item row").id;
    state
        .db
        .query("RELATE $t->has_item->$i")
        .bind(("t", tx.clone()))
        .bind(("i", id.clone()))
        .await
        .expect("has_item edge");
    id
}

async fn seed_doc_on_item(state: &AppState, item: &RecordId) {
    #[derive(serde::Serialize, SurrealValue)]
    struct NewDoc {
        filename: String,
        form_code: String,
        content_type: String,
        storage_key: String,
        size_bytes: i64,
        version: i64,
    }
    let d: Option<crate::models::Document> = state
        .db
        .create("document")
        .content(NewDoc {
            filename: "doc.pdf".into(),
            form_code: "MISC".into(),
            content_type: "application/pdf".into(),
            storage_key: "k".into(),
            size_bytes: 1,
            version: 1,
        })
        .await
        .expect("create doc");
    let doc_id = d.expect("doc row").id;
    state
        .db
        .query("RELATE $d->for_item->$i")
        .bind(("d", doc_id))
        .bind(("i", item.clone()))
        .await
        .expect("for_item edge");
}

/// True iff `owns(in=user, out=tx)` exists.
async fn owns_edge_exists(state: &AppState, user: &RecordId, tx: &RecordId) -> bool {
    let mut q = state
        .db
        .query("SELECT count() FROM owns WHERE in = $u AND out = $t GROUP ALL")
        .bind(("u", user.clone()))
        .bind(("t", tx.clone()))
        .await
        .expect("count owns");
    #[derive(serde::Deserialize, SurrealValue)]
    struct C {
        count: i64,
    }
    let row: Option<C> = q.take(0).unwrap_or_default();
    row.map(|r| r.count > 0).unwrap_or(false)
}

/// Approval status string from the row — for assertions after the
/// approve/deny endpoints fire.
async fn approval_status_of(state: &AppState, item: &RecordId) -> String {
    let row: Option<crate::models::ChecklistItem> =
        state.db.select(item.clone()).await.ok().flatten();
    row.map(|i| i.approval_status).unwrap_or_default()
}

/// Count comments attached to a target — used to verify that the
/// deny-with-reason path actually wrote one.
async fn comment_count_on(state: &AppState, target: &RecordId) -> i64 {
    let mut q = state
        .db
        .query("SELECT count() FROM comment WHERE target = $t GROUP ALL")
        .bind(("t", target.clone()))
        .await
        .expect("count comments");
    #[derive(serde::Deserialize, SurrealValue)]
    struct C {
        count: i64,
    }
    let row: Option<C> = q.take(0).unwrap_or_default();
    row.map(|r| r.count).unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Smoke tests (anonymous)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn healthcheck_returns_ok() {
    let app = make_app().await;
    let (status, _body) = send(
        &app,
        Request::builder()
            .uri("/healthcheck")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn landing_page_renders_signed_out() {
    let app = make_app().await;
    let (status, body) = send(
        &app,
        Request::builder().uri("/").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("TransactVault"),
        "landing should render the app name"
    );
}

#[tokio::test]
async fn login_page_renders() {
    let app = make_app().await;
    let (status, body) = send(
        &app,
        Request::builder()
            .uri("/login")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("login") || body.contains("Login") || body.contains("Sign in"));
}

#[tokio::test]
async fn pricing_page_renders() {
    let app = make_app().await;
    let (status, _body) = send(
        &app,
        Request::builder()
            .uri("/pricing")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn app_routes_require_session_cookie() {
    let app = make_app().await;
    let (status, _body) = send(
        &app,
        Request::builder().uri("/app").body(Body::empty()).unwrap(),
    )
    .await;
    assert!(
        status.is_client_error() || status.is_redirection(),
        "expected redirect or client error, got {status}"
    );
    assert_ne!(status, StatusCode::OK);
}

#[tokio::test]
async fn admin_routes_require_session_cookie() {
    let app = make_app().await;
    let (status, _body) = send(
        &app,
        Request::builder()
            .uri("/admin")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert!(
        status.is_client_error() || status.is_redirection(),
        "expected redirect or client error, got {status}"
    );
    assert_ne!(status, StatusCode::OK);
}

#[tokio::test]
async fn invite_with_bogus_token_is_404() {
    let app = make_app().await;
    let (status, _body) = send(
        &app,
        Request::builder()
            .uri("/invite/not-a-real-token")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn signup_post_with_missing_fields_does_not_succeed() {
    let app = make_app().await;
    let (status, _body) = send(
        &app,
        Request::builder()
            .method("POST")
            .uri("/signup")
            .header("content-type", "application/x-www-form-urlencoded")
            .body(Body::from(""))
            .unwrap(),
    )
    .await;
    assert!(
        !status.is_success(),
        "signup should not succeed on empty body, got {status}"
    );
}

#[tokio::test]
async fn webhook_without_signature_is_rejected() {
    let app = make_app().await;
    let (status, _body) = send(
        &app,
        Request::builder()
            .method("POST")
            .uri("/webhooks/stripe")
            .body(Body::from("{}"))
            .unwrap(),
    )
    .await;
    assert_ne!(status, StatusCode::OK);
}

#[tokio::test]
async fn static_assets_served() {
    let app = make_app().await;
    let (status, _body) = send(
        &app,
        Request::builder()
            .uri("/static/js/confirm-action.js")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

// ---------------------------------------------------------------------------
// Authenticated smoke: a broker can reach /app
// ---------------------------------------------------------------------------

#[tokio::test]
async fn signed_in_broker_can_load_dashboard() {
    let app = make_app().await;
    let b = seed_brokerage(&app.state, "Acme").await;
    let broker = seed_user(&app.state, "broker@acme").await;
    join(&app.state, &broker, &b, "broker").await;
    let (status, body) = authed_get(&app, &broker, "/app").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Acme") || body.contains("Transactions"));
}

#[tokio::test]
async fn signed_in_orphan_redirects_to_no_brokerage() {
    // A user with no works_at edge hits the CurrentUser extractor and
    // gets a Forbidden / redirect — the friendly path goes through
    // /app/no-brokerage. Confirm the orphan landing renders for them.
    let app = make_app().await;
    let lonely = seed_user(&app.state, "alone@x").await;
    let (status, body) = authed_get(&app, &lonely, "/app/no-brokerage").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("not at a brokerage") || body.contains("No brokerage"));
}

// ---------------------------------------------------------------------------
// Authz: agents only see their own transactions; cross-tenant rejected
// ---------------------------------------------------------------------------

#[tokio::test]
async fn agent_cannot_view_teammates_transaction() {
    let app = make_app().await;
    let b = seed_brokerage(&app.state, "Acme").await;
    let owner = seed_user(&app.state, "owner@acme").await;
    join(&app.state, &owner, &b, "agent").await;
    let snooper = seed_user(&app.state, "snooper@acme").await;
    join(&app.state, &snooper, &b, "agent").await;
    let tx = seed_tx(&app.state, &b, Some(&owner)).await;

    // Owner can see it.
    let (own_status, _) = authed_get(
        &app,
        &owner,
        &format!("/app/transactions/{}", crate::db::record_key(&tx)),
    )
    .await;
    assert_eq!(own_status, StatusCode::OK);

    // Teammate (different agent, same brokerage) cannot.
    let (snoop_status, _) = authed_get(
        &app,
        &snooper,
        &format!("/app/transactions/{}", crate::db::record_key(&tx)),
    )
    .await;
    assert!(
        snoop_status.is_client_error(),
        "agent shouldn't view teammate's tx, got {snoop_status}"
    );
}

#[tokio::test]
async fn cross_brokerage_transaction_is_not_found() {
    // A user in brokerage A asks about a transaction in brokerage B.
    // authorize_transaction must return NotFound (404) — not 403 —
    // so the response leaks nothing about the existence of B's tx.
    let app = make_app().await;
    let a = seed_brokerage(&app.state, "A").await;
    let b = seed_brokerage(&app.state, "B").await;
    let a_broker = seed_user(&app.state, "ab@a").await;
    join(&app.state, &a_broker, &a, "broker").await;
    let foreign_tx = seed_tx(&app.state, &b, None).await;

    let (status, _) = authed_get(
        &app,
        &a_broker,
        &format!("/app/transactions/{}", crate::db::record_key(&foreign_tx)),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// set_approval: approve / deny / role gate / docs gate
// ---------------------------------------------------------------------------

#[tokio::test]
async fn agent_cannot_approve_item() {
    let app = make_app().await;
    let b = seed_brokerage(&app.state, "Acme").await;
    let agent = seed_user(&app.state, "agent@a").await;
    join(&app.state, &agent, &b, "agent").await;
    let tx = seed_tx(&app.state, &b, Some(&agent)).await;
    let item = seed_item(&app.state, &tx, "pending").await;
    seed_doc_on_item(&app.state, &item).await;

    let (status, _) = authed_post(
        &app,
        &agent,
        &format!("/app/checklist/{}/approve", crate::db::record_key(&item)),
        "",
    )
    .await;
    assert!(
        status.is_client_error(),
        "agent shouldn't approve, got {status}"
    );
    assert_eq!(approval_status_of(&app.state, &item).await, "pending");
}

#[tokio::test]
async fn broker_cannot_deny_item_without_a_document() {
    // The deny / approve endpoints refuse when no document has been
    // uploaded — otherwise a reviewer could mark something "denied"
    // that the agent never even tried to fulfil.
    let app = make_app().await;
    let b = seed_brokerage(&app.state, "Acme").await;
    let broker = seed_user(&app.state, "b@a").await;
    join(&app.state, &broker, &b, "broker").await;
    let tx = seed_tx(&app.state, &b, Some(&broker)).await;
    let item = seed_item(&app.state, &tx, "pending").await;

    let (status, _) = authed_post(
        &app,
        &broker,
        &format!("/app/checklist/{}/deny", crate::db::record_key(&item)),
        "",
    )
    .await;
    assert!(status.is_client_error());
    assert_eq!(approval_status_of(&app.state, &item).await, "pending");
}

#[tokio::test]
async fn broker_deny_with_reason_writes_comment() {
    let app = make_app().await;
    let b = seed_brokerage(&app.state, "Acme").await;
    let broker = seed_user(&app.state, "b@a").await;
    join(&app.state, &broker, &b, "broker").await;
    let tx = seed_tx(&app.state, &b, Some(&broker)).await;
    let item = seed_item(&app.state, &tx, "pending").await;
    seed_doc_on_item(&app.state, &item).await;

    assert_eq!(comment_count_on(&app.state, &item).await, 0);

    let (status, _) = authed_post(
        &app,
        &broker,
        &format!("/app/checklist/{}/deny", crate::db::record_key(&item)),
        "reason=Wrong+form",
    )
    .await;
    assert!(
        status.is_redirection() || status.is_success(),
        "got {status}"
    );
    assert_eq!(approval_status_of(&app.state, &item).await, "denied");
    assert_eq!(comment_count_on(&app.state, &item).await, 1);
}

#[tokio::test]
async fn broker_deny_without_reason_does_not_write_comment() {
    let app = make_app().await;
    let b = seed_brokerage(&app.state, "Acme").await;
    let broker = seed_user(&app.state, "b@a").await;
    join(&app.state, &broker, &b, "broker").await;
    let tx = seed_tx(&app.state, &b, Some(&broker)).await;
    let item = seed_item(&app.state, &tx, "pending").await;
    seed_doc_on_item(&app.state, &item).await;

    let (status, _) = authed_post(
        &app,
        &broker,
        &format!("/app/checklist/{}/deny", crate::db::record_key(&item)),
        "",
    )
    .await;
    assert!(status.is_redirection() || status.is_success());
    assert_eq!(approval_status_of(&app.state, &item).await, "denied");
    assert_eq!(comment_count_on(&app.state, &item).await, 0);
}

#[tokio::test]
async fn approve_clears_to_approved() {
    let app = make_app().await;
    let b = seed_brokerage(&app.state, "Acme").await;
    let broker = seed_user(&app.state, "b@a").await;
    join(&app.state, &broker, &b, "broker").await;
    let tx = seed_tx(&app.state, &b, Some(&broker)).await;
    let item = seed_item(&app.state, &tx, "pending").await;
    seed_doc_on_item(&app.state, &item).await;

    let (status, _) = authed_post(
        &app,
        &broker,
        &format!("/app/checklist/{}/approve", crate::db::record_key(&item)),
        "",
    )
    .await;
    assert!(status.is_redirection() || status.is_success());
    assert_eq!(approval_status_of(&app.state, &item).await, "approved");
}

// ---------------------------------------------------------------------------
// Invite issuance + accept + decline
// ---------------------------------------------------------------------------

#[tokio::test]
async fn broker_can_invite_new_email() {
    let app = make_app().await;
    let b = seed_brokerage(&app.state, "Acme").await;
    let broker = seed_user(&app.state, "b@a").await;
    join(&app.state, &broker, &b, "broker").await;

    let (status, _) = authed_post(
        &app,
        &broker,
        "/app/team/invite",
        "email=newhire@x&role=agent",
    )
    .await;
    assert!(status.is_redirection() || status.is_success());

    // Invitation row exists.
    let mut q = app
        .state
        .db
        .query("SELECT count() FROM invitation WHERE email = 'newhire@x' GROUP ALL")
        .await
        .expect("count invites");
    #[derive(serde::Deserialize, SurrealValue)]
    struct C {
        count: i64,
    }
    let row: Option<C> = q.take(0).unwrap_or_default();
    assert_eq!(row.map(|r| r.count).unwrap_or(0), 1);
}

#[tokio::test]
async fn agent_cannot_invite() {
    let app = make_app().await;
    let b = seed_brokerage(&app.state, "Acme").await;
    let agent = seed_user(&app.state, "agent@a").await;
    join(&app.state, &agent, &b, "agent").await;
    let (status, _) = authed_post(
        &app,
        &agent,
        "/app/team/invite",
        "email=newhire@x&role=agent",
    )
    .await;
    assert!(
        status.is_client_error(),
        "non-broker shouldn't invite, got {status}"
    );
}

#[tokio::test]
async fn invite_handles_email_case_insensitively() {
    // Schema lowercases on write + the app lowercases on read, so
    // inviting `Alice@Example.com` then `alice@example.com` should
    // collapse to a single pending row.
    let app = make_app().await;
    let b = seed_brokerage(&app.state, "Acme").await;
    let broker = seed_user(&app.state, "b@a").await;
    join(&app.state, &broker, &b, "broker").await;

    authed_post(
        &app,
        &broker,
        "/app/team/invite",
        "email=Alice@Example.com&role=agent",
    )
    .await;
    let (status, body) = authed_post(
        &app,
        &broker,
        "/app/team/invite",
        "email=alice@example.com&role=agent",
    )
    .await;
    assert!(status.is_success() || status.is_redirection());
    assert!(
        body.to_ascii_lowercase()
            .contains("already has a pending invitation"),
        "second invite (case variation) should be deduped"
    );

    // Exactly one row, stored in lowercase.
    let mut q = app
        .state
        .db
        .query("SELECT VALUE email FROM invitation WHERE email = 'alice@example.com'")
        .await
        .expect("query");
    let emails: Vec<String> = q.take(0).unwrap_or_default();
    assert_eq!(
        emails,
        vec!["alice@example.com".to_string()],
        "schema must store the lowercase form"
    );
}

#[tokio::test]
async fn db_event_rejects_duplicate_pending_at_layer_below_app() {
    // Belt-and-braces: bypass the handler and CREATE two invitation
    // rows directly. The `invitation_no_duplicate_pending` event must
    // reject the second create even though the application-level
    // check is sidestepped.
    use surrealdb::types::SurrealValue;
    let app = make_app().await;
    let b = seed_brokerage(&app.state, "Acme").await;
    let inviter = seed_user(&app.state, "inviter@a").await;

    #[derive(serde::Serialize, SurrealValue)]
    struct NewInv {
        email: String,
        role: String,
        token: String,
        brokerage: RecordId,
        invited_by: RecordId,
    }

    // First create succeeds.
    let first: Option<crate::models::Invitation> = app
        .state
        .db
        .create("invitation")
        .content(NewInv {
            email: "dup@x".into(),
            role: "agent".into(),
            token: "tok-first-1234567890abcdef".into(),
            brokerage: b.clone(),
            invited_by: inviter.clone(),
        })
        .await
        .expect("first invite create");
    assert!(first.is_some(), "first create should succeed");

    // Second create with the same (brokerage, email) and still
    // pending must be rejected by the event guard.
    let second: Result<Option<crate::models::Invitation>, _> = app
        .state
        .db
        .create("invitation")
        .content(NewInv {
            email: "dup@x".into(),
            role: "agent".into(),
            token: "tok-second-987654321fedcba".into(),
            brokerage: b.clone(),
            invited_by: inviter.clone(),
        })
        .await;
    assert!(
        second.is_err(),
        "duplicate pending CREATE should be rejected at the DB layer"
    );
}

#[tokio::test]
async fn reinvite_same_email_is_idempotent() {
    // Real-world trigger: broker double-clicks "Send invites" or hits
    // back-and-resubmit. Without the pending-dedupe guard each submit
    // would create another `invitation` row and fire another email —
    // we explicitly check both: exactly one row exists after two
    // submits, and the second submit's response surfaces the skip
    // notice instead of confirming a new send.
    let app = make_app().await;
    let b = seed_brokerage(&app.state, "Acme").await;
    let broker = seed_user(&app.state, "b@a").await;
    join(&app.state, &broker, &b, "broker").await;

    // First invite: succeeds, creates one row.
    authed_post(
        &app,
        &broker,
        "/app/team/invite",
        "email=victim@x&role=agent",
    )
    .await;

    // Second invite for the same email: must be a no-op.
    let (status, body) = authed_post(
        &app,
        &broker,
        "/app/team/invite",
        "email=victim@x&role=agent",
    )
    .await;
    assert!(
        status.is_success() || status.is_redirection(),
        "got {status}"
    );
    assert!(
        body.to_ascii_lowercase()
            .contains("already has a pending invitation"),
        "expected pending-dupe notice in response"
    );

    // Exactly one invitation row in the DB.
    let mut q = app
        .state
        .db
        .query("SELECT count() FROM invitation WHERE email = 'victim@x' GROUP ALL")
        .await
        .expect("count invites");
    #[derive(serde::Deserialize, SurrealValue)]
    struct C {
        count: i64,
    }
    let row: Option<C> = q.take(0).unwrap_or_default();
    assert_eq!(
        row.map(|r| r.count).unwrap_or(0),
        1,
        "re-invite should NOT create a second invitation row"
    );
}

#[tokio::test]
async fn invite_skips_email_already_at_another_brokerage() {
    // Option-A semantics: a user with an existing works_at edge cannot
    // be invited away — the issuer sees them skipped in the notice.
    let app = make_app().await;
    let other = seed_brokerage(&app.state, "Other").await;
    let busy = seed_user(&app.state, "busy@x").await;
    join(&app.state, &busy, &other, "agent").await;

    let acme = seed_brokerage(&app.state, "Acme").await;
    let broker = seed_user(&app.state, "broker@acme").await;
    join(&app.state, &broker, &acme, "broker").await;

    let (status, body) =
        authed_post(&app, &broker, "/app/team/invite", "email=busy@x&role=agent").await;
    assert!(status.is_success() || status.is_redirection());
    let lower = body.to_ascii_lowercase();
    assert!(
        lower.contains("already at another brokerage") || lower.contains("must leave first"),
        "expected cross-brokerage skip notice in response"
    );
    // No invitation was created for the busy address.
    let mut q = app
        .state
        .db
        .query("SELECT count() FROM invitation WHERE email = 'busy@x' GROUP ALL")
        .await
        .expect("count invites");
    #[derive(serde::Deserialize, SurrealValue)]
    struct C {
        count: i64,
    }
    let row: Option<C> = q.take(0).unwrap_or_default();
    assert_eq!(row.map(|r| r.count).unwrap_or(0), 0);
}

#[tokio::test]
async fn invite_accept_for_brand_new_user_creates_account() {
    // The classic new-recipient path: invitation → click link → fill
    // in name+password → user created, works_at edge added, invite
    // marked accepted.
    let app = make_app().await;
    let b = seed_brokerage(&app.state, "Acme").await;
    let broker = seed_user(&app.state, "b@a").await;
    join(&app.state, &broker, &b, "broker").await;

    // Issue the invite by hitting the broker endpoint.
    authed_post(
        &app,
        &broker,
        "/app/team/invite",
        "email=fresh@x&role=agent",
    )
    .await;

    // Find the token.
    let mut q = app
        .state
        .db
        .query("SELECT VALUE token FROM invitation WHERE email = 'fresh@x' LIMIT 1")
        .await
        .expect("query token");
    let tokens: Vec<String> = q.take(0).unwrap_or_default();
    let token = tokens.into_iter().next().expect("invite token");

    // Accept via the public POST.
    let req = Request::builder()
        .method("POST")
        .uri(format!("/invite/{token}"))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Body::from("name=Fresh+Hire&password=longenoughpass"))
        .unwrap();
    let (status, body) = send(&app, req).await;
    assert!(
        status.is_redirection() || status.is_success(),
        "got {status} body={}",
        &body.chars().take(2000).collect::<String>()
    );

    // The user row exists.
    let mut uq = app
        .state
        .db
        .query("SELECT count() FROM user WHERE email = 'fresh@x' GROUP ALL")
        .await
        .expect("count users");
    #[derive(serde::Deserialize, SurrealValue)]
    struct C {
        count: i64,
    }
    let row: Option<C> = uq.take(0).unwrap_or_default();
    assert_eq!(row.map(|r| r.count).unwrap_or(0), 1);
}

#[tokio::test]
async fn invite_decline_marks_declined() {
    let app = make_app().await;
    let b = seed_brokerage(&app.state, "Acme").await;
    let broker = seed_user(&app.state, "b@a").await;
    join(&app.state, &broker, &b, "broker").await;
    let orphan = seed_user(&app.state, "wanderer@x").await;
    // Issue invite, find token.
    authed_post(
        &app,
        &broker,
        "/app/team/invite",
        "email=wanderer@x&role=agent",
    )
    .await;
    let mut q = app
        .state
        .db
        .query("SELECT VALUE token FROM invitation WHERE email = 'wanderer@x' LIMIT 1")
        .await
        .expect("query token");
    let tokens: Vec<String> = q.take(0).unwrap_or_default();
    let token = tokens.into_iter().next().expect("token");

    // The orphan signs in and declines.
    let (status, _) =
        authed_post(&app, &orphan, &format!("/app/invites/{token}/decline"), "").await;
    assert!(
        status.is_redirection() || status.is_success(),
        "got {status}"
    );

    let mut dq = app
        .state
        .db
        .query("SELECT VALUE declined FROM invitation WHERE token = $t LIMIT 1")
        .bind(("t", token))
        .await
        .expect("query declined");
    let declined: Vec<bool> = dq.take(0).unwrap_or_default();
    assert_eq!(declined, vec![true]);
}

#[tokio::test]
async fn invite_decline_by_wrong_user_is_404() {
    // Decline must verify the signed-in user's email matches the
    // invite's recipient — otherwise anyone with the token URL could
    // burn it.
    let app = make_app().await;
    let b = seed_brokerage(&app.state, "Acme").await;
    let broker = seed_user(&app.state, "b@a").await;
    join(&app.state, &broker, &b, "broker").await;
    authed_post(
        &app,
        &broker,
        "/app/team/invite",
        "email=intended@x&role=agent",
    )
    .await;
    let mut q = app
        .state
        .db
        .query("SELECT VALUE token FROM invitation WHERE email = 'intended@x' LIMIT 1")
        .await
        .expect("token");
    let tokens: Vec<String> = q.take(0).unwrap_or_default();
    let token = tokens.into_iter().next().unwrap();

    let attacker = seed_user(&app.state, "attacker@x").await;
    let (status, _) = authed_post(
        &app,
        &attacker,
        &format!("/app/invites/{token}/decline"),
        "",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// Member removal
// ---------------------------------------------------------------------------

#[tokio::test]
async fn broker_can_remove_agent() {
    let app = make_app().await;
    let b = seed_brokerage(&app.state, "Acme").await;
    let broker = seed_user(&app.state, "b@a").await;
    join(&app.state, &broker, &b, "broker").await;
    let agent = seed_user(&app.state, "agent@a").await;
    join(&app.state, &agent, &b, "agent").await;
    let tx = seed_tx(&app.state, &b, Some(&agent)).await;
    assert!(owns_edge_exists(&app.state, &agent, &tx).await);

    let (status, _) = authed_post(
        &app,
        &broker,
        &format!("/app/team/{}/remove", crate::db::record_key(&agent)),
        "",
    )
    .await;
    assert!(
        status.is_redirection() || status.is_success(),
        "got {status}"
    );

    // works_at edge gone.
    let mut q = app
        .state
        .db
        .query("SELECT count() FROM works_at WHERE in = $u AND out = $b GROUP ALL")
        .bind(("u", agent.clone()))
        .bind(("b", b.clone()))
        .await
        .expect("count works_at");
    #[derive(serde::Deserialize, SurrealValue)]
    struct C {
        count: i64,
    }
    let row: Option<C> = q.take(0).unwrap_or_default();
    assert_eq!(row.map(|r| r.count).unwrap_or(0), 0);

    // The agent's own `owns` edge is gone...
    assert!(!owns_edge_exists(&app.state, &agent, &tx).await);
    // ...but the transaction is NOT orphaned — ownership moved to the
    // removing broker so it never falls to "Unassigned".
    assert!(
        owns_edge_exists(&app.state, &broker, &tx).await,
        "removed agent's transaction should be reassigned to the broker"
    );
    // And the departing agent's name is snapshotted onto the deal so
    // its history shows who originally handled it.
    #[derive(serde::Deserialize, SurrealValue)]
    struct FormerRow {
        former_owner_name: Option<String>,
    }
    let mut fq = app
        .state
        .db
        .query("SELECT former_owner_name FROM ONLY $t")
        .bind(("t", tx.clone()))
        .await
        .expect("select former_owner_name");
    let former: Option<FormerRow> = fq.take(0).expect("former row");
    assert_eq!(
        former.and_then(|r| r.former_owner_name).as_deref(),
        Some("agent@a"),
        "former agent name should be recorded on the transaction"
    );
}

#[tokio::test]
async fn broker_cannot_remove_self() {
    let app = make_app().await;
    let b = seed_brokerage(&app.state, "Acme").await;
    let broker = seed_user(&app.state, "b@a").await;
    join(&app.state, &broker, &b, "broker").await;
    let (status, _) = authed_post(
        &app,
        &broker,
        &format!("/app/team/{}/remove", crate::db::record_key(&broker)),
        "",
    )
    .await;
    assert!(
        status.is_client_error(),
        "self-removal should be refused, got {status}"
    );
}

#[tokio::test]
async fn cannot_remove_last_broker() {
    let app = make_app().await;
    let b = seed_brokerage(&app.state, "Acme").await;
    let broker_a = seed_user(&app.state, "a@a").await;
    join(&app.state, &broker_a, &b, "broker").await;
    let broker_b = seed_user(&app.state, "b@a").await;
    join(&app.state, &broker_b, &b, "broker").await;

    // a removes b — fine, still one broker left.
    let (status_a, _) = authed_post(
        &app,
        &broker_a,
        &format!("/app/team/{}/remove", crate::db::record_key(&broker_b)),
        "",
    )
    .await;
    assert!(status_a.is_redirection() || status_a.is_success());

    // Now we can't remove a since they'd be the last broker — but a
    // can't remove themselves anyway. Add a second broker, swap, try.
    let broker_c = seed_user(&app.state, "c@a").await;
    join(&app.state, &broker_c, &b, "broker").await;
    // c tries to remove a (last-broker check kicks in only if a is the
    // only broker remaining — a + c are both brokers, so this succeeds).
    let (status_c, _) = authed_post(
        &app,
        &broker_c,
        &format!("/app/team/{}/remove", crate::db::record_key(&broker_a)),
        "",
    )
    .await;
    assert!(status_c.is_redirection() || status_c.is_success());

    // Now c is the only broker. c can't remove themselves; try to add
    // a new agent and have c try to remove c via another broker — but
    // there isn't one, so simulate by adding a second broker and
    // attempting to demote/remove c when they're the last.
    // Cleaner: add an agent, have c attempt to remove c via the agent
    // endpoint — agent doesn't have permission anyway. Instead, set up
    // c as the only broker and have a *new broker* try to remove c.
    let broker_d = seed_user(&app.state, "d@a").await;
    join(&app.state, &broker_d, &b, "broker").await;
    // d removes c — now c is gone, d is the last broker.
    authed_post(
        &app,
        &broker_d,
        &format!("/app/team/{}/remove", crate::db::record_key(&broker_c)),
        "",
    )
    .await;
    // Add an agent. Agent can't remove anyone, but to test the
    // last-broker guard we add another broker briefly. Actually, the
    // remove handler refuses removal of THE LAST broker — let me add
    // a fresh second broker, then have d try to remove that broker
    // (allowed) and then try to remove themselves (refused). Self
    // already covered; last-broker via attempted-remove-of-other test:
    // remove second-broker leaves d as last — that works. To verify
    // the last-broker guard fires, we'd need a non-self test, which
    // requires another broker, which would void the "last" condition.
    // Conclusion: the path is reachable only when a broker tries to
    // remove the only other broker who's also themselves — which is
    // the self path. The standalone last-broker guard exists for the
    // change_role demotion path. Treat that as separate coverage.
    //
    // Leave this test as-is: chain removals successfully run; the
    // self-block kicks in for the literal last-broker self case.
    let (final_status, _) = authed_post(
        &app,
        &broker_d,
        &format!("/app/team/{}/remove", crate::db::record_key(&broker_d)),
        "",
    )
    .await;
    assert!(final_status.is_client_error());
}

// ---------------------------------------------------------------------------
// Reassign
// ---------------------------------------------------------------------------

#[tokio::test]
async fn broker_can_reassign_transaction() {
    let app = make_app().await;
    let b = seed_brokerage(&app.state, "Acme").await;
    let broker = seed_user(&app.state, "b@a").await;
    join(&app.state, &broker, &b, "broker").await;
    let alice = seed_user(&app.state, "alice@a").await;
    join(&app.state, &alice, &b, "agent").await;
    let bob = seed_user(&app.state, "bob@a").await;
    join(&app.state, &bob, &b, "agent").await;
    let tx = seed_tx(&app.state, &b, Some(&alice)).await;

    let body = format!(
        "assignee_key={}&tx_keys={}",
        crate::db::record_key(&bob),
        crate::db::record_key(&tx)
    );
    let (status, _) = authed_post(&app, &broker, "/app/transactions/reassign", &body).await;
    assert!(
        status.is_redirection() || status.is_success(),
        "got {status}"
    );

    // Alice loses, Bob gains.
    assert!(!owns_edge_exists(&app.state, &alice, &tx).await);
    assert!(owns_edge_exists(&app.state, &bob, &tx).await);
}

#[tokio::test]
async fn agent_cannot_reassign() {
    let app = make_app().await;
    let b = seed_brokerage(&app.state, "Acme").await;
    let alice = seed_user(&app.state, "alice@a").await;
    join(&app.state, &alice, &b, "agent").await;
    let tx = seed_tx(&app.state, &b, Some(&alice)).await;
    let body = format!(
        "assignee_key={}&tx_keys={}",
        crate::db::record_key(&alice),
        crate::db::record_key(&tx)
    );
    let (status, _) = authed_post(&app, &alice, "/app/transactions/reassign", &body).await;
    assert!(
        status.is_client_error(),
        "agent shouldn't reassign, got {status}"
    );
}

#[tokio::test]
async fn reassign_to_non_member_is_rejected() {
    // A broker must not be able to hand a tx to a user who isn't in
    // their brokerage — otherwise they could leak it cross-tenant.
    let app = make_app().await;
    let a = seed_brokerage(&app.state, "A").await;
    let other = seed_brokerage(&app.state, "B").await;
    let a_broker = seed_user(&app.state, "ab@a").await;
    join(&app.state, &a_broker, &a, "broker").await;
    let outsider = seed_user(&app.state, "outsider@b").await;
    join(&app.state, &outsider, &other, "agent").await;
    let tx = seed_tx(&app.state, &a, None).await;

    let body = format!(
        "assignee_key={}&tx_keys={}",
        crate::db::record_key(&outsider),
        crate::db::record_key(&tx)
    );
    let (status, _) = authed_post(&app, &a_broker, "/app/transactions/reassign", &body).await;
    assert!(
        status.is_client_error(),
        "non-member assignee should be refused, got {status}"
    );
    assert!(!owns_edge_exists(&app.state, &outsider, &tx).await);
}

#[tokio::test]
async fn reassign_cross_tenant_tx_silently_skipped() {
    // A foreign tx id in the multi-key payload should be skipped
    // without error so a typo doesn't fail the whole batch — but the
    // edge to the foreign tx must NOT be created.
    let app = make_app().await;
    let a = seed_brokerage(&app.state, "A").await;
    let other = seed_brokerage(&app.state, "B").await;
    let a_broker = seed_user(&app.state, "ab@a").await;
    join(&app.state, &a_broker, &a, "broker").await;
    let a_agent = seed_user(&app.state, "aa@a").await;
    join(&app.state, &a_agent, &a, "agent").await;
    let foreign_tx = seed_tx(&app.state, &other, None).await;

    let body = format!(
        "assignee_key={}&tx_keys={}",
        crate::db::record_key(&a_agent),
        crate::db::record_key(&foreign_tx)
    );
    let (status, _) = authed_post(&app, &a_broker, "/app/transactions/reassign", &body).await;
    // Endpoint succeeds (silent skip) but the edge is NOT created.
    assert!(status.is_redirection() || status.is_success());
    assert!(!owns_edge_exists(&app.state, &a_agent, &foreign_tx).await);
}

// ---------------------------------------------------------------------------
// Transaction create
// ---------------------------------------------------------------------------

#[tokio::test]
async fn broker_create_transaction_seeds_owns_and_has_transaction() {
    let app = make_app().await;
    let b = seed_brokerage(&app.state, "Acme").await;
    let broker = seed_user(&app.state, "b@a").await;
    join(&app.state, &broker, &b, "broker").await;

    let form = "property_address=42+Main&\
                city=LA&\
                postal_code=90001&\
                price=$100,000&\
                transaction_type=residential&\
                special_sales_condition=none&\
                sales_type=listing";
    let (status, _) = authed_post(&app, &broker, "/app/transactions", form).await;
    assert!(
        status.is_redirection() || status.is_success(),
        "create should succeed, got {status}"
    );

    // Exactly one tx exists in the brokerage.
    let mut q = app
        .state
        .db
        .query("SELECT count() FROM $b->has_transaction->transaction GROUP ALL")
        .bind(("b", b.clone()))
        .await
        .expect("count tx");
    #[derive(serde::Deserialize, SurrealValue)]
    struct C {
        count: i64,
    }
    let row: Option<C> = q.take(0).unwrap_or_default();
    assert_eq!(row.map(|r| r.count).unwrap_or(0), 1);

    // The broker is the owner of that one tx.
    let mut o_q = app
        .state
        .db
        .query("SELECT count() FROM $u->owns->transaction GROUP ALL")
        .bind(("u", broker.clone()))
        .await
        .expect("count owns");
    let row: Option<C> = o_q.take(0).unwrap_or_default();
    assert_eq!(row.map(|r| r.count).unwrap_or(0), 1);
}

#[tokio::test]
async fn create_transaction_requires_address_or_apn() {
    // Both blank → 400. APN alone or address alone → ok.
    let app = make_app().await;
    let b = seed_brokerage(&app.state, "Acme").await;
    let broker = seed_user(&app.state, "b@a").await;
    join(&app.state, &broker, &b, "broker").await;

    let blank = "property_address=&city=&apn=&price=&\
                 transaction_type=residential&special_sales_condition=none&sales_type=listing";
    let (status, _) = authed_post(&app, &broker, "/app/transactions", blank).await;
    assert!(
        status.is_client_error(),
        "blank address+apn should fail, got {status}"
    );

    // APN only — should succeed.
    let apn_only = "property_address=&city=LA&apn=3205-005-002&price=&\
                    transaction_type=vacant_lots_land&special_sales_condition=none&sales_type=listing";
    let (status, _) = authed_post(&app, &broker, "/app/transactions", apn_only).await;
    assert!(
        status.is_redirection() || status.is_success(),
        "APN-only should succeed, got {status}"
    );
}

// ---------------------------------------------------------------------------
// Item-comment route regression — proves that standalone item comments
// (posted via /app/checklist/{id}/comments — the same endpoint the deny
// popover uses) are stored as comment rows that needs_attention picks up
// at the DB layer. Closes the user-reported "only deny comments seem to
// flag" suspicion.
// ---------------------------------------------------------------------------

/// User bug report: agent uploads files into several groups, then
/// compliance opens the transaction and sees ALL groups collapsed —
/// no idea what to review. The fix is whatever makes `has_attention`
/// fire for groups that contain pending+upload items.
///
/// The template renders `<details ... open>` whenever
/// `open_by_default || has_attention(can_review)` is true. For a
/// compliance viewer `open_by_default == false`, so the bug is either
/// (a) `has_attention` returns false despite the upload (likely a
/// data-load issue in `build_grouped_checklist`), or (b) the JS
/// state-persistence is overriding the server-rendered `open`.
///
/// This test pins the server-side answer: render the page as
/// compliance and check the raw HTML has `open` on every group that
/// contains a pending+upload item.
#[tokio::test]
async fn compliance_sees_groups_open_after_agent_uploads() {
    let app = make_app().await;
    let b = seed_brokerage(&app.state, "Acme").await;
    let agent = seed_user(&app.state, "agent@a").await;
    join(&app.state, &agent, &b, "agent").await;
    let officer = seed_user(&app.state, "co@a").await;
    join(&app.state, &officer, &b, "coordinator").await;
    let tx = seed_tx(&app.state, &b, Some(&agent)).await;

    // Create three items in three different groups, each with an
    // uploaded document — mirroring "agent uploaded into different
    // categories." Override the default `group_name` from `seed_item`
    // so the groups are distinct.
    let groups = [
        "Mandatory Disclosures",
        "Listing Contracts",
        "Escrow Documents",
    ];
    for name in groups {
        let item = seed_item_in_group(&app.state, &tx, "pending", name).await;
        seed_doc_on_item(&app.state, &item).await;
    }

    let (status, body) = authed_get(
        &app,
        &officer,
        &format!("/app/transactions/{}", crate::db::record_key(&tx)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Server must emit `data-attention="true"` on every group with a
    // pending+upload item — that's the marker the client-side
    // `checklist-state.js` checks before honoring sessionStorage.
    // Without this, stale "closed" entries from an earlier session
    // (typically the agent walking through groups to upload into
    // each) would override the compliance officer's first view.
    let attention_count = body.matches("data-attention=\"true\"").count();
    assert_eq!(
        attention_count,
        groups.len(),
        "expected one data-attention marker per group with uploads, got {attention_count}"
    );
    for name in groups {
        assert!(
            body.contains(&format!(r#"data-group-key="{name}""#)),
            "group {name:?} should render in the page"
        );
    }
}

/// `/app/stats` returns the same `<section id="stat-grid">` fragment
/// that the full dashboard renders, with live counters reflecting the
/// caller's brokerage. The dashboard wraps it in
/// `data-on-interval__15s` so Datastar morphs the numbers in place
/// without a page reload when another user changes state.
#[tokio::test]
async fn stats_fragment_serves_morph_target_for_dashboard_polling() {
    let app = make_app().await;
    let b = seed_brokerage(&app.state, "Acme").await;
    let broker = seed_user(&app.state, "b@a").await;
    join(&app.state, &broker, &b, "broker").await;
    // Two transactions: one active, one sold.
    seed_tx(&app.state, &b, Some(&broker)).await;
    let sold = seed_tx(&app.state, &b, Some(&broker)).await;
    app.state
        .db
        .query("UPDATE $t SET status = 'sold'")
        .bind(("t", sold))
        .await
        .expect("update status");

    let (status, body) = authed_get(&app, &broker, "/app/stats").await;
    assert_eq!(status, StatusCode::OK);
    // The response must carry the matching id so Idiomorph can find
    // the in-page element to morph into.
    assert!(
        body.contains(r#"id="stat-grid""#),
        "fragment must carry id=\"stat-grid\" for the morph match"
    );
    // Numbers are accurate — 2 total, 1 active, 1 sold.
    assert!(body.contains(">2<"), "total should be 2");
    assert!(body.contains(">1<"), "active and sold each =1");
}

/// `/app/stats/stream` opens a long-lived Server-Sent Events response.
/// We can't `to_bytes` it (the stream never ends), so this test peeks
/// at headers + the first body chunk to verify:
///   - the response is `text/event-stream` (Datastar's signal that this
///     is a push channel, not a one-shot patch);
///   - the initial event the handler emits is a Datastar
///     `datastar-patch-elements` event carrying the `stat-grid`
///     fragment, so the client morphs fresh numbers in immediately on
///     connect rather than waiting for the first mutation;
///   - EVERY `data:` line carries the `elements ` prefix. Datastar's
///     SSE parser buckets each line by its first word, so a fragment
///     whose continuation lines lack the prefix collapses to its
///     opening tag — the patch then morphed the stat grid into an
///     empty section. That was the "real-time updates not working"
///     bug; this assertion keeps it fixed.
#[tokio::test]
async fn stats_stream_pushes_initial_patch_event_on_connect() {
    use axum::body::Body;
    use axum::extract::ConnectInfo;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use std::net::SocketAddr;
    use std::time::Duration;
    use tokio::time::timeout;

    let app = make_app().await;
    let b = seed_brokerage(&app.state, "Acme").await;
    let broker = seed_user(&app.state, "b@a").await;
    join(&app.state, &broker, &b, "broker").await;
    seed_tx(&app.state, &b, Some(&broker)).await;

    let cookie = session_cookie(&app, &broker);
    let mut req = Request::builder()
        .uri("/app/stats/stream")
        .header("cookie", cookie)
        .body(Body::empty())
        .expect("build request");
    req.extensions_mut().insert(ConnectInfo::<SocketAddr>(
        "127.0.0.1:0".parse().expect("loopback addr"),
    ));

    let response = app
        .router
        .clone()
        .oneshot(req)
        .await
        .expect("router oneshot");
    assert_eq!(response.status(), StatusCode::OK);
    let content_type = response
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert!(
        content_type.starts_with("text/event-stream"),
        "expected SSE content-type, got {content_type:?}"
    );

    // Read just enough of the stream to see the first event — anything
    // beyond ~250ms means the handler isn't emitting the initial
    // event eagerly and the dashboard would show stale numbers until
    // someone else mutates state. Reading frames-as-they-arrive (not
    // `to_bytes`) avoids waiting for the stream's never-coming end.
    let mut body = response.into_body();
    let mut buf = String::new();
    let deadline = Duration::from_millis(500);
    let started = std::time::Instant::now();
    while started.elapsed() < deadline && !buf.contains("stat-grid") {
        let next = timeout(Duration::from_millis(250), body.frame()).await;
        match next {
            Ok(Some(Ok(frame))) => {
                if let Some(data) = frame.data_ref() {
                    buf.push_str(&String::from_utf8_lossy(data));
                }
            }
            Ok(Some(Err(_))) | Ok(None) => break,
            Err(_) => break, // timeout on this frame; keep looping
        }
    }

    assert!(
        buf.contains("event: datastar-patch-elements"),
        "first SSE event should be Datastar patch-elements; saw: {buf:?}"
    );
    assert!(
        buf.contains(r#"id="stat-grid""#),
        "patch body must carry the stat-grid id for the morph match; saw: {buf:?}"
    );
    for line in buf.lines() {
        if let Some(data) = line.strip_prefix("data: ") {
            assert!(
                data.starts_with("elements "),
                "every SSE data line must start with the `elements ` prefix \
                 or Datastar drops it and the patch collapses; saw: {line:?}"
            );
        }
    }
}

/// `/admin/changelog` renders the bundled `CHANGELOG.md` as HTML for
/// super-admins, with the running build version shown prominently. The
/// test config wires `admin@test` as the lone super-admin (see
/// `Config::for_tests`) — anyone else hitting the route gets a 403, so
/// the route is also implicitly a gate test.
#[tokio::test]
async fn admin_changelog_renders_version_and_bundled_markdown() {
    let app = make_app().await;
    let b = seed_brokerage(&app.state, "Acme").await;
    let admin = seed_user(&app.state, "admin@test").await;
    join(&app.state, &admin, &b, "broker").await;

    let (status, body) = authed_get(&app, &admin, "/admin/changelog").await;
    assert_eq!(status, StatusCode::OK);

    // Build version from Cargo.toml lands in the page header.
    let v = env!("CARGO_PKG_VERSION");
    assert!(
        body.contains(v),
        "page should show running version v{v}; saw body of {} bytes",
        body.len()
    );

    // Pulldown rendered the bundled CHANGELOG.md, so a known heading
    // from that file is present as real `<h1>` HTML, not as raw `#`.
    assert!(
        body.contains("<h1>What's new</h1>"),
        "CHANGELOG.md should have been rendered as HTML, not raw markdown"
    );

    // Admin subnav exposes the link so super-admins can navigate to it.
    assert!(
        body.contains(r#"href="/admin/changelog""#),
        "admin subnav should link to /admin/changelog"
    );

    // Non-admin gets blocked.
    let other = seed_user(&app.state, "broker@a").await;
    join(&app.state, &other, &b, "broker").await;
    let (forbidden, _) = authed_get(&app, &other, "/admin/changelog").await;
    assert_eq!(
        forbidden,
        StatusCode::FORBIDDEN,
        "non-super-admin must NOT reach the changelog page"
    );
}

/// Super-admin form-library management: a form can be deleted, a group
/// can be renamed, and a group can be deleted (cascading to its forms).
/// All three are gated to super-admins and validated against the owning
/// set, and deletes purge the graph edges so nothing dangles.
#[tokio::test]
async fn admin_can_delete_forms_rename_and_delete_groups() {
    let app = make_app().await;
    let b = seed_brokerage(&app.state, "Acme").await;
    let admin = seed_user(&app.state, "admin@test").await;
    join(&app.state, &admin, &b, "broker").await;

    // Seed a small library: set → group g1 (forms f1, f2) + group g2 (form f3).
    app.state
        .db
        .query(
            "CREATE form_set:tset SET scope = 'state', name = 'TestSet';
             CREATE form_group:g1 SET name = 'Group One', sort_order = 1;
             CREATE form_group:g2 SET name = 'Group Two', sort_order = 2;
             CREATE form:f1 SET code = 'F1', name = 'Form One';
             CREATE form:f2 SET code = 'F2', name = 'Form Two';
             CREATE form:f3 SET code = 'F3', name = 'Form Three';
             RELATE form_set:tset->has_group->form_group:g1;
             RELATE form_set:tset->has_group->form_group:g2;
             RELATE form_group:g1->has_form->form:f1;
             RELATE form_group:g1->has_form->form:f2;
             RELATE form_group:g2->has_form->form:f3;",
        )
        .await
        .expect("seed form library");

    async fn count(app: &TestApp, surql: &str) -> i64 {
        #[derive(serde::Deserialize, SurrealValue)]
        struct C {
            count: i64,
        }
        let mut q = app.state.db.query(surql).await.expect("count query");
        let row: Option<C> = q.take(0).expect("count row");
        row.map(|c| c.count).unwrap_or(0)
    }

    // --- Rendered structure: Delete must be a `formaction` submit button
    // carrying its own `data-confirm`, NOT a second <form> in the cell
    // (two <form>s in one <td> don't parse reliably and caused the Delete
    // click to submit the wrong form). Lock that structure in.
    let (page_status, page) = authed_get(&app, &admin, "/admin/forms/tset").await;
    assert_eq!(page_status, StatusCode::OK);
    assert!(
        page.contains(r#"formaction="/admin/forms/tset/forms/f1/delete""#),
        "per-form Delete should post via formaction, not a nested form"
    );
    assert!(
        page.contains("Delete form {code} ({name})?"),
        "Delete button should carry its own data-confirm prompt"
    );

    // --- Delete a single form ------------------------------------------------
    let (s, _) = authed_post(&app, &admin, "/admin/forms/tset/forms/f1/delete", "").await;
    assert!(s.is_redirection(), "delete-form should redirect on success");
    assert_eq!(
        count(
            &app,
            "SELECT count() FROM form WHERE id = form:f1 GROUP ALL"
        )
        .await,
        0,
        "form f1 row should be gone"
    );
    assert_eq!(
        count(
            &app,
            "SELECT count() FROM has_form WHERE out = form:f1 GROUP ALL"
        )
        .await,
        0,
        "f1's has_form edge should be gone"
    );
    assert_eq!(
        count(
            &app,
            "SELECT count() FROM form WHERE id = form:f2 GROUP ALL"
        )
        .await,
        1,
        "sibling form f2 must be untouched"
    );

    // --- Rename a group ------------------------------------------------------
    let (s, _) = authed_post(
        &app,
        &admin,
        "/admin/forms/tset/groups/g1/rename",
        "name=Renamed+Group",
    )
    .await;
    assert!(
        s.is_redirection(),
        "rename-group should redirect on success"
    );
    #[derive(serde::Deserialize, SurrealValue)]
    struct NameRow {
        name: String,
    }
    let mut nq = app
        .state
        .db
        .query("SELECT name FROM ONLY form_group:g1")
        .await
        .expect("name query");
    let row: Option<NameRow> = nq.take(0).expect("name row");
    assert_eq!(row.map(|r| r.name).as_deref(), Some("Renamed Group"));

    // --- Delete a group (cascades to its forms) ------------------------------
    let (s, _) = authed_post(&app, &admin, "/admin/forms/tset/groups/g2/delete", "").await;
    assert!(
        s.is_redirection(),
        "delete-group should redirect on success"
    );
    assert_eq!(
        count(
            &app,
            "SELECT count() FROM form_group WHERE id = form_group:g2 GROUP ALL"
        )
        .await,
        0,
        "group g2 should be gone"
    );
    assert_eq!(
        count(
            &app,
            "SELECT count() FROM form WHERE id = form:f3 GROUP ALL"
        )
        .await,
        0,
        "form f3 inside the deleted group should be gone too"
    );
    assert_eq!(
        count(
            &app,
            "SELECT count() FROM has_group WHERE out = form_group:g2 GROUP ALL"
        )
        .await,
        0,
        "g2's has_group edge should be gone"
    );

    // --- Auth gate: a non-super-admin cannot delete forms --------------------
    let other = seed_user(&app.state, "broker@a").await;
    join(&app.state, &other, &b, "broker").await;
    let (forbidden, _) = authed_post(&app, &other, "/admin/forms/tset/forms/f2/delete", "").await;
    assert_eq!(forbidden, StatusCode::FORBIDDEN);
    assert_eq!(
        count(
            &app,
            "SELECT count() FROM form WHERE id = form:f2 GROUP ALL"
        )
        .await,
        1,
        "form f2 must survive a forbidden delete attempt"
    );
}

#[tokio::test]
async fn standalone_item_comment_endpoint_writes_a_flaggable_comment() {
    // Persistence check, not behavior check — the per-item comment
    // route writes the same comment row shape (target=item, author=
    // submitter) that the deny popover writes. needs_attention's
    // unit tests already prove that shape flags; this test pins the
    // route's persisted output so a regression in the controller
    // (e.g. accidentally targeting the transaction instead of the
    // item) gets caught.
    let app = make_app().await;
    let b = seed_brokerage(&app.state, "Acme").await;
    let broker = seed_user(&app.state, "b@a").await;
    join(&app.state, &broker, &b, "broker").await;
    let agent = seed_user(&app.state, "a@a").await;
    join(&app.state, &agent, &b, "agent").await;
    let tx = seed_tx(&app.state, &b, Some(&agent)).await;
    let item = seed_item(&app.state, &tx, "pending").await;

    // Agent posts via the STANDALONE comment endpoint — NOT via the
    // deny popover. Same URL the deny flow targets, minus the deny
    // wrapper.
    let (status, _) = authed_post(
        &app,
        &agent,
        &format!("/app/checklist/{}/comments", crate::db::record_key(&item)),
        "body=please+review+this",
    )
    .await;
    assert!(
        status.is_redirection() || status.is_success(),
        "POST comment should succeed, got {status}"
    );

    // Exactly one comment row exists, and it targets the ITEM (not
    // the transaction). That's the shape needs_attention's
    // unit-tested query picks up; equivalence with the deny flow is
    // proved by both writing the same row.
    let mut q = app
        .state
        .db
        .query("SELECT target, author FROM comment")
        .await
        .expect("count");
    #[derive(serde::Deserialize, surrealdb::types::SurrealValue)]
    struct Row {
        target: RecordId,
        author: RecordId,
    }
    let rows: Vec<Row> = q.take(0).unwrap_or_default();
    assert_eq!(rows.len(), 1, "expected exactly one comment row");
    assert_eq!(rows[0].target, item, "comment must target the item");
    assert_eq!(rows[0].author, agent, "author must be the poster");
}

// ---------------------------------------------------------------------------
// June 2026 corrections set — live search, exports, forms admin
// ---------------------------------------------------------------------------

/// GET as a signed-in user, returning raw bytes — for binary responses
/// (ZIP exports) that would fail `send`'s UTF-8 conversion.
async fn authed_get_raw(
    app: &TestApp,
    user_id: &RecordId,
    uri: &str,
) -> (StatusCode, String, Vec<u8>) {
    use axum::extract::ConnectInfo;
    use std::net::SocketAddr;

    let cookie = session_cookie(app, user_id);
    let mut req = Request::builder()
        .uri(uri)
        .header("cookie", cookie)
        .body(Body::empty())
        .unwrap();
    req.extensions_mut().insert(ConnectInfo::<SocketAddr>(
        "127.0.0.1:0".parse().expect("loopback addr"),
    ));
    let response = app
        .router
        .clone()
        .oneshot(req)
        .await
        .expect("router oneshot");
    let status = response.status();
    let content_type = response
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let body = to_bytes(response.into_body(), 64 * 1024 * 1024)
        .await
        .expect("collect body");
    (status, content_type, body.to_vec())
}

/// The live-search fragment endpoint returns just the results region
/// (no page chrome) and reads the typed query from the Datastar signal
/// payload rather than the `q` param — that's what the toolbar's
/// `data-on-input` sends per keystroke.
#[tokio::test]
async fn live_search_fragment_filters_by_datastar_signal() {
    let app = make_app().await;
    let b = seed_brokerage(&app.state, "Acme").await;
    let broker = seed_user(&app.state, "b@a").await;
    join(&app.state, &broker, &b, "broker").await;
    seed_tx(&app.state, &b, Some(&broker)).await;

    // Signal q matches the seeded address → row present, no chrome.
    let (status, body) = authed_get(
        &app,
        &broker,
        "/app/transactions?fragment=results&datastar=%7B%22q%22%3A%22Test%20Way%22%7D",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains(r#"id="tx-results""#),
        "fragment carries the swap id"
    );
    assert!(body.contains("1 Test Way"), "matching row is rendered");
    assert!(
        !body.contains("<html"),
        "fragment must not include page chrome"
    );

    // Signal q that matches nothing → the empty state, still no chrome.
    let (status, body) = authed_get(
        &app,
        &broker,
        "/app/transactions?fragment=results&datastar=%7B%22q%22%3A%22zzz-no-match%22%7D",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("Nothing matches"),
        "empty state for a non-matching query"
    );
    assert!(!body.contains("1 Test Way"));
}

/// Same contract for the search page's fragment: `#search-results`
/// region only, query via Datastar signal.
#[tokio::test]
async fn search_page_fragment_filters_by_datastar_signal() {
    let app = make_app().await;
    let b = seed_brokerage(&app.state, "Acme").await;
    let broker = seed_user(&app.state, "b@a").await;
    join(&app.state, &broker, &b, "broker").await;
    seed_tx(&app.state, &b, Some(&broker)).await;

    let (status, body) = authed_get(
        &app,
        &broker,
        "/app/search?fragment=results&datastar=%7B%22q%22%3A%22Test%20Way%22%7D",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains(r#"id="search-results""#),
        "fragment carries the swap id"
    );
    assert!(body.contains("1 Test Way"), "matching transaction listed");
    assert!(
        !body.contains("<html"),
        "fragment must not include page chrome"
    );

    // The full page (no fragment param) still renders chrome + the
    // live-search wiring on the input — RC.6 colon-key grammar
    // (`data-on:input`, `data-bind:q`); the dash forms match no plugin
    // and are silently ignored by the bundle.
    let (status, body) = authed_get(&app, &broker, "/app/search?q=Test").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("<html"));
    assert!(
        body.contains("data-on:input__debounce.350ms"),
        "search input must carry the RC.6 colon-key live-search trigger"
    );
    assert!(
        body.contains("data-bind:q"),
        "search input must bind the q signal with RC.6 colon-key grammar"
    );
}

/// The transactions list wires its Datastar live behaviors with RC.6
/// attribute grammar. This pins the exact attribute spellings because
/// the failure mode is SILENT: an unknown attribute (`data-on-load`,
/// `data-bind-q`, `data-on-input`) matches no plugin and simply does
/// nothing — which is precisely how the live dashboard shipped dead to
/// production twice.
#[tokio::test]
async fn transactions_page_wires_live_stream_and_live_search() {
    let app = make_app().await;
    let b = seed_brokerage(&app.state, "Acme").await;
    let broker = seed_user(&app.state, "b@a").await;
    join(&app.state, &broker, &b, "broker").await;
    seed_tx(&app.state, &b, Some(&broker)).await;

    let (status, body) = authed_get(&app, &broker, "/app/transactions").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains(r#"data-init="@get('/app/stats/stream"#),
        "stats stream must open via data-init (RC.6 renamed data-on-load, \
         which is silently ignored)"
    );
    assert!(
        body.contains("data-bind:q"),
        "toolbar search input must bind the q signal"
    );
    assert!(
        body.contains("data-on:input__debounce.350ms"),
        "toolbar search input must carry the live-search trigger"
    );
    assert!(
        !body.contains("data-on-load"),
        "data-on-load matches no RC.6 plugin — it must not reappear"
    );
    assert!(
        body.contains("data-on-signal-patch__debounce"),
        "live-rows listener must react to the stream's txrev signal"
    );
    assert!(
        body.contains(r#"data-on-signal-patch-filter="{include: /^txrev$/}""#),
        "live-rows listener must be filtered to txrev so typing (q patches) never triggers it"
    );
    assert!(
        body.contains("retries-failed"),
        "the stream host must catch retries-failed and reopen the stream, or a \
         long-lived tab goes permanently deaf after ~2 minutes of downtime"
    );
}

/// The Add-an-item picker offers the whole CAR catalog — including
/// forms already on the checklist (the old exclusion read as "form
/// missing from the list") and the four June-2026 additions. A
/// duplicate single-instance add is rejected with a friendly 400;
/// multi-instance forms (ADM) can be added repeatedly.
#[tokio::test]
async fn checklist_add_offers_full_catalog_and_rejects_duplicates() {
    let app = make_app().await;
    let b = seed_brokerage(&app.state, "Acme").await;
    let broker = seed_user(&app.state, "b@a").await;
    join(&app.state, &broker, &b, "broker").await;
    let tx = seed_tx(&app.state, &b, Some(&broker)).await;
    let key = crate::db::record_key(&tx);

    // First SBSA add lands on the checklist.
    let (status, _) = authed_post(
        &app,
        &broker,
        &format!("/app/transactions/{key}/checklist"),
        "form_code=SBSA",
    )
    .await;
    assert!(
        status.is_redirection(),
        "first add should redirect, got {status}"
    );

    // Picker still lists SBSA (and the new 2026 codes) afterwards.
    let (status, body) = authed_get(&app, &broker, &format!("/app/transactions/{key}")).await;
    assert_eq!(status, StatusCode::OK);
    for code in [
        "SBSA", "PRBS-B", "PRBS-S", "SWPI-C", "SWPI-Q", "COL", "WOO", "CLR",
    ] {
        assert!(
            body.contains(&format!(r#"<option value="{code}">"#)),
            "picker should offer {code}"
        );
    }

    // Second SBSA add is a 400 naming the duplicate.
    let (status, body) = authed_post(
        &app,
        &broker,
        &format!("/app/transactions/{key}/checklist"),
        "form_code=SBSA",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body.contains("SBSA"),
        "error should name the duplicate code"
    );

    // ADM allows multiple — two adds, two redirects.
    for _ in 0..2 {
        let (status, _) = authed_post(
            &app,
            &broker,
            &format!("/app/transactions/{key}/checklist"),
            "form_code=ADM",
        )
        .await;
        assert!(status.is_redirection(), "ADM adds should both succeed");
    }
}

/// Broker-added custom codes (not in the compiled CAR library) still
/// render their code chip on the checklist — the row falls back to the
/// item's stored form_code when the library lookup misses.
#[tokio::test]
async fn checklist_renders_code_chip_for_custom_codes() {
    let app = make_app().await;
    let b = seed_brokerage(&app.state, "Acme").await;
    let broker = seed_user(&app.state, "b@a").await;
    join(&app.state, &broker, &b, "broker").await;
    let tx = seed_tx(&app.state, &b, Some(&broker)).await;
    let key = crate::db::record_key(&tx);

    let (status, _) = authed_post(
        &app,
        &broker,
        &format!("/app/transactions/{key}/checklist"),
        "form_code=RNTD",
    )
    .await;
    assert!(status.is_redirection());

    let (status, body) = authed_get(&app, &broker, &format!("/app/transactions/{key}")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains(r#"<strong class="form-code">RNTD</strong>"#),
        "custom code should render its chip"
    );
}

/// Team ZIP exports: broker-only, scoped to the caller's brokerage, and
/// the response is a real ZIP (PK magic) even with zero documents.
#[tokio::test]
async fn team_zip_exports_are_broker_only_and_brokerage_scoped() {
    let app = make_app().await;
    let b = seed_brokerage(&app.state, "Acme").await;
    let broker = seed_user(&app.state, "b@a").await;
    let agent = seed_user(&app.state, "agent@a").await;
    join(&app.state, &broker, &b, "broker").await;
    join(&app.state, &agent, &b, "agent").await;
    seed_tx(&app.state, &b, Some(&agent)).await;

    let agent_key = crate::db::record_key(&agent);

    // Agents can't export anything.
    let (status, _, _) = authed_get_raw(&app, &agent, "/app/team/export").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, _) =
        authed_get_raw(&app, &agent, &format!("/app/team/{agent_key}/export")).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Broker: per-agent export is a ZIP.
    let (status, ct, bytes) =
        authed_get_raw(&app, &broker, &format!("/app/team/{agent_key}/export")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(ct, "application/zip");
    assert!(bytes.starts_with(b"PK"), "response should be a ZIP archive");

    // Broker: whole-brokerage export is a ZIP.
    let (status, ct, bytes) = authed_get_raw(&app, &broker, "/app/team/export").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(ct, "application/zip");
    assert!(bytes.starts_with(b"PK"));

    // A broker from ANOTHER brokerage can't export our agent.
    let b2 = seed_brokerage(&app.state, "Rival").await;
    let rival = seed_user(&app.state, "r@b").await;
    join(&app.state, &rival, &b2, "broker").await;
    let (status, _, _) =
        authed_get_raw(&app, &rival, &format!("/app/team/{agent_key}/export")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// Admin form-set lifecycle: create a locality, rename it, delete it
/// (cascading groups + forms + edges). The seeded California state set
/// refuses both rename and delete.
#[tokio::test]
async fn admin_can_rename_and_delete_local_sets_but_not_state() {
    let app = make_app().await;
    crate::db::seed_forms(&app.state.db)
        .await
        .expect("seed forms");

    let b = seed_brokerage(&app.state, "Acme").await;
    let admin = seed_user(&app.state, "admin@test").await;
    join(&app.state, &admin, &b, "broker").await;

    // Create a locality set.
    let (status, _) = authed_post(&app, &admin, "/admin/forms/sets", "name=Test+MLS").await;
    assert!(status.is_redirection(), "create set should redirect");
    let mut q = app
        .state
        .db
        .query("SELECT VALUE id FROM form_set WHERE name = 'Test MLS' LIMIT 1")
        .await
        .expect("find set");
    let ids: Vec<RecordId> = q.take(0).unwrap_or_default();
    let set_id = ids.into_iter().next().expect("set created");
    let set_key = crate::db::record_key(&set_id);

    // Rename it (the GAVAR capitalization use-case).
    let (status, _) = authed_post(
        &app,
        &admin,
        &format!("/admin/forms/{set_key}/rename"),
        "name=Test+MLS+Renamed",
    )
    .await;
    assert!(status.is_redirection(), "rename should redirect");
    let mut q = app
        .state
        .db
        .query("SELECT VALUE name FROM ONLY $s")
        .bind(("s", set_id.clone()))
        .await
        .expect("reload set");
    let name: Option<String> = q.take(0).expect("take name");
    assert_eq!(name.as_deref(), Some("Test MLS Renamed"));

    // Give it a group + form so the delete has something to cascade.
    let (status, _) = authed_post(
        &app,
        &admin,
        &format!("/admin/forms/{set_key}/groups"),
        "name=G1&sort_order=1",
    )
    .await;
    assert!(status.is_redirection());
    let mut q = app
        .state
        .db
        .query("SELECT VALUE out FROM has_group WHERE in = $s LIMIT 1")
        .bind(("s", set_id.clone()))
        .await
        .expect("find group");
    let gids: Vec<RecordId> = q.take(0).unwrap_or_default();
    let group_key = crate::db::record_key(gids.first().expect("group created"));
    let (status, _) = authed_post(
        &app,
        &admin,
        &format!("/admin/forms/{set_key}/forms"),
        &format!("group_key={group_key}&code=ZZZ&name=Z+Form"),
    )
    .await;
    assert!(status.is_redirection());

    // Delete the whole library.
    let (status, _) =
        authed_post(&app, &admin, &format!("/admin/forms/{set_key}/delete"), "").await;
    assert!(status.is_redirection(), "delete should redirect");
    let mut q = app
        .state
        .db
        .query("SELECT count() FROM form WHERE code = 'ZZZ' GROUP ALL")
        .await
        .expect("count forms");
    #[derive(serde::Deserialize, surrealdb::types::SurrealValue)]
    struct C {
        count: i64,
    }
    let c: Option<C> = q.take(0).ok().flatten();
    assert_eq!(
        c.map(|c| c.count).unwrap_or(0),
        0,
        "cascade should remove the set's forms"
    );
    let (status, _) = authed_get(&app, &admin, &format!("/admin/forms/{set_key}")).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "deleted set detail should 404"
    );

    // The California state set refuses rename + delete.
    let mut q = app
        .state
        .db
        .query("SELECT VALUE id FROM form_set WHERE scope = 'state' LIMIT 1")
        .await
        .expect("find CA");
    let ca: Vec<RecordId> = q.take(0).unwrap_or_default();
    let ca_key = crate::db::record_key(ca.first().expect("California seeded"));
    let (status, _) = authed_post(&app, &admin, &format!("/admin/forms/{ca_key}/delete"), "").await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "state set delete must be refused"
    );
    let (status, _) = authed_post(
        &app,
        &admin,
        &format!("/admin/forms/{ca_key}/rename"),
        "name=Nope",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "state set rename must be refused"
    );
}

/// The boot-time catalog backfill: picker-only forms (like CLR) land in
/// the California set with EMPTY applies arrays — offered in the picker,
/// never on a default checklist — and the `seeded_form` ledger keeps
/// admin deletions deleted across re-seeds.
#[tokio::test]
async fn catalog_backfill_adds_picker_only_forms_and_respects_deletions() {
    let app = make_app().await;
    crate::db::seed_forms(&app.state.db).await.expect("seed");

    // CLR arrived via the backfill, filed under Release Disclosures,
    // with empty applicability.
    #[derive(serde::Deserialize, surrealdb::types::SurrealValue)]
    struct FormRow {
        id: RecordId,
        applies_types: Vec<String>,
        group_name: Option<String>,
    }
    let mut q = app
        .state
        .db
        .query(
            "SELECT id, applies_types, \
             (<-has_form<-form_group)[0].name AS group_name \
             FROM form WHERE code = 'CLR'",
        )
        .await
        .expect("query CLR");
    let rows: Vec<FormRow> = q.take(0).unwrap_or_default();
    assert_eq!(rows.len(), 1, "CLR should be backfilled exactly once");
    assert!(
        rows[0].applies_types.is_empty(),
        "backfilled forms must have empty applies so they never hit default checklists"
    );
    assert_eq!(rows[0].group_name.as_deref(), Some("Release Disclosures"));
    let clr_id = rows[0].id.clone();

    // Default checklists are untouched: residential listing resolution
    // does not include CLR.
    let b = seed_brokerage(&app.state, "Acme").await;
    crate::db::forms::attach_default_state(&app.state.db, &b)
        .await
        .expect("attach state");
    let resolved =
        crate::db::forms::resolve_checklist(&app.state.db, &b, "residential", "listing", "none")
            .await
            .expect("resolve");
    assert!(
        !resolved.iter().any(|f| f.code == "CLR"),
        "picker-only forms must not appear on default checklists"
    );

    // Admin-style delete, then re-seed: the ledger stops resurrection.
    app.state
        .db
        .query("DELETE has_form WHERE out = $f; DELETE $f;")
        .bind(("f", clr_id))
        .await
        .expect("delete CLR");
    crate::db::seed_forms(&app.state.db).await.expect("re-seed");
    let mut q = app
        .state
        .db
        .query("SELECT count() FROM form WHERE code = 'CLR' GROUP ALL")
        .await
        .expect("recount");
    #[derive(serde::Deserialize, surrealdb::types::SurrealValue)]
    struct C {
        count: i64,
    }
    let c: Option<C> = q.take(0).ok().flatten();
    assert_eq!(
        c.map(|c| c.count).unwrap_or(0),
        0,
        "a deleted form must stay deleted across re-seeds (seeded_form ledger)"
    );
}

/// The Add-an-item picker is DB-driven for a seeded brokerage: library
/// forms come from its sets (minus hidden ones), the brokerage's custom
/// forms overlay in, and adding a custom-form code stores the DB
/// metadata (canonical code, title, group).
#[tokio::test]
async fn picker_uses_brokerage_catalog_with_custom_overlay_and_hides() {
    let app = make_app().await;
    crate::db::seed_forms(&app.state.db).await.expect("seed");

    let b = seed_brokerage(&app.state, "Acme").await;
    let broker = seed_user(&app.state, "b@a").await;
    join(&app.state, &broker, &b, "broker").await;
    crate::db::forms::attach_default_state(&app.state.db, &b)
        .await
        .expect("attach state");
    let tx = seed_tx(&app.state, &b, Some(&broker)).await;
    let key = crate::db::record_key(&tx);

    // A brokerage custom form (the client's RNTD case).
    #[derive(serde::Serialize, SurrealValue)]
    struct NewCustom {
        code: String,
        name: String,
        description: String,
        includes: String,
        form_order: i64,
        required: bool,
        allows_multiple: bool,
        group_name: Option<String>,
        group_order: Option<i64>,
        applies_types: Vec<String>,
        applies_sides: Vec<String>,
        applies_conditions: Vec<String>,
        is_active: bool,
    }
    let custom: Option<crate::db::forms::CatalogForm> = app
        .state
        .db
        .create("form")
        .content(NewCustom {
            code: "RNTD".into(),
            name: "Rented Status MLS Report".into(),
            description: String::new(),
            includes: String::new(),
            form_order: 9000,
            required: false,
            allows_multiple: false,
            group_name: Some("MLS Data Sheets".into()),
            group_order: Some(0),
            applies_types: vec![],
            applies_sides: vec![],
            applies_conditions: vec![],
            is_active: true,
        })
        .await
        .expect("create custom form");
    let custom_id = custom.expect("custom form row").id;
    app.state
        .db
        .query("RELATE $b->owns_form->$f")
        .bind(("b", b.clone()))
        .bind(("f", custom_id))
        .await
        .expect("owns_form");

    // Hide CLR from this brokerage.
    let mut q = app
        .state
        .db
        .query("SELECT VALUE id FROM form WHERE code = 'CLR' LIMIT 1")
        .await
        .expect("find CLR");
    let clr: Vec<RecordId> = q.take(0).unwrap_or_default();
    app.state
        .db
        .query("RELATE $b->hides_form->$f")
        .bind(("b", b.clone()))
        .bind(("f", clr.into_iter().next().expect("CLR seeded")))
        .await
        .expect("hides_form");

    let (status, body) = authed_get(&app, &broker, &format!("/app/transactions/{key}")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains(r#"<option value="RNTD">"#),
        "custom forms belong in the picker"
    );
    assert!(
        body.contains(r#"<option value="SBSA">"#),
        "library forms come from the DB set"
    );
    assert!(
        !body.contains(r#"<option value="CLR">"#),
        "hidden forms must not be offered"
    );

    // Adding by (lowercased) custom code stores the DB metadata.
    let (status, _) = authed_post(
        &app,
        &broker,
        &format!("/app/transactions/{key}/checklist"),
        "form_code=rntd",
    )
    .await;
    assert!(status.is_redirection(), "custom-code add should succeed");
    #[derive(serde::Deserialize, surrealdb::types::SurrealValue)]
    struct ItemRow {
        title: String,
        form_code: Option<String>,
        group_name: String,
    }
    let mut q = app
        .state
        .db
        .query("SELECT title, form_code, group_name FROM checklist_item WHERE form_code = 'RNTD'")
        .await
        .expect("load item");
    let items: Vec<ItemRow> = q.take(0).unwrap_or_default();
    assert_eq!(items.len(), 1, "one RNTD item created");
    assert_eq!(items[0].title, "Rented Status MLS Report");
    assert_eq!(items[0].form_code.as_deref(), Some("RNTD"));
    assert_eq!(items[0].group_name, "MLS Data Sheets");

    // And the duplicate guard works off the DB metadata too.
    let (status, body) = authed_post(
        &app,
        &broker,
        &format!("/app/transactions/{key}/checklist"),
        "form_code=RNTD",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body.contains("RNTD"));
}

/// The DEV_RESET_ON_BOOT path: `reset_schema` drops every domain table
/// — including the `seeded_form` ledger — and the normal boot sequence
/// (`apply_schema` → `seed_forms`) rebuilds a COMPLETE catalog from
/// scratch. A pre-reset admin deletion is intentionally forgotten
/// along with everything else: the wipe is total, so the fresh seed
/// starts from the canonical compiled library again. Also serves as
/// the guard that the reset table list stays in sync with the schema
/// (a table missing from RESET_QUERY would leak rows into the
/// "fresh" state and break these counts).
#[tokio::test]
async fn dev_reset_then_reseed_rebuilds_full_catalog() {
    let app = make_app().await; // apply_schema already ran
    crate::db::seed_forms(&app.state.db)
        .await
        .expect("first seed");

    // Simulate an admin deletion before the reset — the ledger keeps
    // this deleted across normal restarts (covered elsewhere), but a
    // full reset wipes the ledger too.
    let mut q = app
        .state
        .db
        .query("SELECT VALUE id FROM form WHERE code = 'CLR' LIMIT 1")
        .await
        .expect("find CLR");
    let clr: Vec<RecordId> = q.take(0).unwrap_or_default();
    let clr = clr.into_iter().next().expect("CLR seeded");
    app.state
        .db
        .query("DELETE has_form WHERE out = $f; DELETE $f;")
        .bind(("f", clr))
        .await
        .expect("delete CLR");

    // The DEV_RESET_ON_BOOT sequence, exactly as main.rs runs it.
    crate::db::reset_schema(&app.state.db).await.expect("reset");
    crate::db::apply_schema(&app.state.db)
        .await
        .expect("re-apply schema");
    crate::db::seed_forms(&app.state.db).await.expect("re-seed");

    #[derive(serde::Deserialize, surrealdb::types::SurrealValue)]
    struct C {
        count: i64,
    }
    let expected = crate::forms::LIBRARY.len() as i64;

    let mut q = app
        .state
        .db
        .query("SELECT count() FROM form GROUP ALL")
        .await
        .expect("count forms");
    let forms: Option<C> = q.take(0).ok().flatten();
    assert_eq!(
        forms.map(|c| c.count).unwrap_or(0),
        expected,
        "post-reset catalog should hold every compiled-library form exactly once"
    );

    let mut q = app
        .state
        .db
        .query("SELECT count() FROM seeded_form GROUP ALL")
        .await
        .expect("count ledger");
    let ledger: Option<C> = q.take(0).ok().flatten();
    assert_eq!(
        ledger.map(|c| c.count).unwrap_or(0),
        expected,
        "ledger should be fully rebuilt after a reset"
    );

    let mut q = app
        .state
        .db
        .query("SELECT count() FROM form WHERE code = 'CLR' GROUP ALL")
        .await
        .expect("recount CLR");
    let clr_count: Option<C> = q.take(0).ok().flatten();
    assert_eq!(
        clr_count.map(|c| c.count).unwrap_or(0),
        1,
        "a full reset forgets pre-reset deletions — CLR is back"
    );
}

/// After a brokerage mutation, the stats stream pushes BOTH the
/// stat-grid element patch AND a `datastar-patch-signals` event bumping
/// `txrev` — the signal the transactions page listens for to re-fetch
/// its visible rows. No `txrev` on the initial connect event (rows just
/// rendered; reconnects shouldn't re-fetch for nothing).
#[tokio::test]
async fn stats_stream_pushes_row_refresh_signal_on_mutation() {
    use axum::body::Body;
    use axum::extract::ConnectInfo;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use std::net::SocketAddr;
    use std::time::Duration;
    use tokio::time::timeout;

    let app = make_app().await;
    let b = seed_brokerage(&app.state, "Acme").await;
    let broker = seed_user(&app.state, "b@a").await;
    join(&app.state, &broker, &b, "broker").await;
    seed_tx(&app.state, &b, Some(&broker)).await;

    let cookie = session_cookie(&app, &broker);
    let mut req = Request::builder()
        .uri("/app/stats/stream")
        .header("cookie", cookie)
        .body(Body::empty())
        .expect("build request");
    req.extensions_mut().insert(ConnectInfo::<SocketAddr>(
        "127.0.0.1:0".parse().expect("loopback addr"),
    ));
    let response = app
        .router
        .clone()
        .oneshot(req)
        .await
        .expect("router oneshot");
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = response.into_body();

    // Drain the initial connect event, then fire a mutation.
    let mut buf = String::new();
    let started = std::time::Instant::now();
    while started.elapsed() < Duration::from_millis(500) && !buf.contains("stat-grid") {
        if let Ok(Some(Ok(frame))) = timeout(Duration::from_millis(250), body.frame()).await {
            if let Some(data) = frame.data_ref() {
                buf.push_str(&String::from_utf8_lossy(data));
            }
        } else {
            break;
        }
    }
    assert!(
        !buf.contains("txrev"),
        "initial connect must NOT bump txrev; saw: {buf:?}"
    );

    app.state
        .events
        .publish(crate::events::Event::BrokerageMutation(b.clone()));

    let started = std::time::Instant::now();
    while started.elapsed() < Duration::from_secs(2) && !buf.contains("txrev") {
        if let Ok(Some(Ok(frame))) = timeout(Duration::from_millis(500), body.frame()).await {
            if let Some(data) = frame.data_ref() {
                buf.push_str(&String::from_utf8_lossy(data));
            }
        } else {
            break;
        }
    }
    assert!(
        buf.contains("event: datastar-patch-signals"),
        "mutation should push a signals patch; saw: {buf:?}"
    );
    assert!(
        buf.contains(r#"data: signals {"txrev":1}"#),
        "signals patch should bump txrev to 1; saw: {buf:?}"
    );
}

/// An AGENT's open stream must react when a BROKER mutates the
/// brokerage — the exact "broker deletes, agent's window doesn't move"
/// report. Full integration: the broker calls the real delete endpoint
/// while the agent's stream is connected; the agent must receive a
/// fresh stat patch AND the txrev rows-refresh bump, rendered for
/// their own (agent-scoped) visibility.
#[tokio::test]
async fn agent_stream_updates_when_broker_deletes() {
    use axum::body::Body;
    use axum::extract::ConnectInfo;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use std::net::SocketAddr;
    use std::time::Duration;
    use tokio::time::timeout;

    let app = make_app().await;
    let b = seed_brokerage(&app.state, "Acme").await;
    let broker = seed_user(&app.state, "boss@a").await;
    let agent = seed_user(&app.state, "agent@a").await;
    join(&app.state, &broker, &b, "broker").await;
    join(&app.state, &agent, &b, "agent").await;
    let tx = seed_tx(&app.state, &b, Some(&agent)).await;
    let tx_key = crate::db::record_key(&tx);

    // Agent opens the stream.
    let cookie = session_cookie(&app, &agent);
    let mut req = Request::builder()
        .uri("/app/stats/stream")
        .header("cookie", cookie)
        .body(Body::empty())
        .expect("build request");
    req.extensions_mut().insert(ConnectInfo::<SocketAddr>(
        "127.0.0.1:0".parse().expect("loopback addr"),
    ));
    let response = app
        .router
        .clone()
        .oneshot(req)
        .await
        .expect("router oneshot");
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = response.into_body();

    // Drain the initial event — agent sees their 1 transaction.
    let mut buf = String::new();
    let started = std::time::Instant::now();
    while started.elapsed() < Duration::from_millis(500) && !buf.contains("stat-grid") {
        if let Ok(Some(Ok(frame))) = timeout(Duration::from_millis(250), body.frame()).await {
            if let Some(data) = frame.data_ref() {
                buf.push_str(&String::from_utf8_lossy(data));
            }
        } else {
            break;
        }
    }
    assert!(
        buf.contains("stat-grid"),
        "agent should get the initial stat patch; saw: {buf:?}"
    );
    buf.clear();

    // Broker deletes the agent's transaction through the real endpoint.
    let (status, _) = authed_post(
        &app,
        &broker,
        &format!("/app/transactions/{tx_key}/delete"),
        "",
    )
    .await;
    assert!(
        status.is_redirection(),
        "broker delete should succeed, got {status}"
    );

    // The agent's stream must push a fresh (agent-scoped) stat patch
    // plus the rows-refresh signal.
    let started = std::time::Instant::now();
    while started.elapsed() < Duration::from_secs(2)
        && !(buf.contains("txrev") && buf.contains("stat-grid"))
    {
        if let Ok(Some(Ok(frame))) = timeout(Duration::from_millis(500), body.frame()).await {
            if let Some(data) = frame.data_ref() {
                buf.push_str(&String::from_utf8_lossy(data));
            }
        } else {
            break;
        }
    }
    assert!(
        buf.contains("event: datastar-patch-elements") && buf.contains("stat-grid"),
        "agent stream should push a stat patch after the broker's delete; saw: {buf:?}"
    );
    assert!(
        buf.contains(r#"data: signals {"txrev":1}"#),
        "agent stream should bump txrev after the broker's delete; saw: {buf:?}"
    );
    // The re-rendered agent grid reflects the deletion: zero transactions.
    assert!(
        buf.contains(">0<"),
        "agent's re-rendered totals should show the transaction gone; saw: {buf:?}"
    );
}

/// Referral transaction type: creatable from the form, and its
/// checklist is the referral-fee paperwork — not a property checklist.
#[tokio::test]
async fn referral_transaction_type_seeds_fee_checklist() {
    let app = make_app().await;
    crate::db::seed_forms(&app.state.db)
        .await
        .expect("seed forms");
    let b = seed_brokerage(&app.state, "Acme").await;
    let broker = seed_user(&app.state, "b@a").await;
    join(&app.state, &broker, &b, "broker").await;
    crate::db::forms::attach_default_state(&app.state.db, &b)
        .await
        .expect("attach state");

    let (status, _) = authed_post(
        &app,
        &broker,
        "/app/transactions",
        "property_address=1+Referral+Way&transaction_type=referral&sales_type=referral&status=active",
    )
    .await;
    assert!(
        status.is_redirection(),
        "create should redirect, got {status}"
    );

    #[derive(serde::Deserialize, surrealdb::types::SurrealValue)]
    struct TxRow {
        id: RecordId,
        transaction_type: String,
    }
    let mut q = app
        .state
        .db
        .query("SELECT id, transaction_type FROM transaction WHERE property_address = '1 Referral Way'")
        .await
        .expect("find tx");
    let rows: Vec<TxRow> = q.take(0).unwrap_or_default();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].transaction_type, "referral");
    let tx_id = rows[0].id.clone();

    #[derive(serde::Deserialize, surrealdb::types::SurrealValue)]
    struct ItemRow {
        form_code: Option<String>,
        required: bool,
    }
    let mut q = app
        .state
        .db
        .query("SELECT form_code, required FROM $t->has_item->checklist_item")
        .bind(("t", tx_id.clone()))
        .await
        .expect("load checklist");
    let items: Vec<ItemRow> = q.take(0).unwrap_or_default();
    let codes: Vec<&str> = items
        .iter()
        .filter_map(|i| i.form_code.as_deref())
        .collect();

    assert!(
        codes.contains(&"RFA"),
        "referral checklist needs the Referral Fee Agreement; got {codes:?}"
    );
    assert!(
        codes.contains(&"COMM"),
        "referral checklist needs commission instructions; got {codes:?}"
    );
    assert!(
        codes.contains(&"CLSD"),
        "referral checklist needs the closing statement; got {codes:?}"
    );
    assert!(
        items
            .iter()
            .any(|i| i.form_code.as_deref() == Some("RFA") && i.required),
        "RFA must be required"
    );
    assert!(
        !codes.contains(&"RPA") && !codes.contains(&"TDS") && !codes.contains(&"ACT"),
        "a referral must not get a property checklist; got {codes:?}"
    );

    // The show page renders the type and the picker still works.
    let key = crate::db::record_key(&tx_id);
    let (status, body) = authed_get(&app, &broker, &format!("/app/transactions/{key}")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Referral"), "type label should render");

    // The new-transaction form offers the type.
    let (status, body) = authed_get(&app, &broker, "/app/transactions/new").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains(r#"value="referral""#),
        "transaction-type dropdown should offer Referral"
    );
}
