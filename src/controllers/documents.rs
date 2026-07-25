//! Document upload, download, and transaction-level ZIP export.
//!
//! Storage layout follows: `<brokerage>/<property folder>/<form code>/<file>`
//! where the property folder uses APN if available, otherwise the
//! transaction record key. This mirrors how a brokerage would organise
//! files in a physical filing cabinet.

// SurrealDB's `RecordId` has interior mutability through lazy-init
// regex caches inside Value/Array, which trips the lint when we keep
// id-keyed maps (docs-per-transaction, owner-per-transaction). Hash +
// Eq are still deterministic — same rationale as
// `controllers/transactions.rs`.
#![allow(clippy::mutable_key_type)]

use axum::body::Body;
use axum::extract::{Multipart, Path, State};
use axum::http::{StatusCode, header};
use axum::response::{Redirect, Response};
use futures::TryStreamExt;
use surrealdb::types::{RecordId, SurrealValue};
use tokio_util::io::StreamReader;

use crate::auth::CurrentUser;
use crate::controllers::transactions::authorize_transaction;
use crate::error::AppError;
use crate::forms;
use crate::models::{Document, Transaction};
use crate::state::AppState;
use crate::storage::Storage;

/// Multipart upload handler.
///
/// Form fields:
/// - `file` (required) — the binary
/// - `form_code` (required) — which CAR form this fulfils (e.g. `RPA`)
/// - `item_id` (optional) — link the upload to a specific checklist item
///   so the per-item document list shows it. If empty, the document is
///   still attached to the transaction but not tied to a checklist row.
pub async fn upload(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<String>,
    mut multipart: Multipart,
) -> Result<Redirect, AppError> {
    let tx_id = RecordId::new("transaction", id.as_str());
    let tx = authorize_transaction(&state, &user, &tx_id).await?;

    // Transaction-level lock: once every checklist item is approved the
    // file set is frozen, regardless of whether the upload targets a
    // specific item or not. (An empty checklist is *not* locked — that
    // would block any first upload on a brand-new transaction.)
    if all_items_approved(&state, &tx_id).await? {
        return Err(AppError::invalid(
            "This transaction is locked — every checklist item is approved. Have a coordinator deny an item to reopen it for changes.",
        ));
    }

    let mut form_code = String::from("MISC");
    let mut item_id: Option<String> = None;
    // Set the moment the file field arrives and we successfully stream
    // it into S3 — kept in an Option so the loop can keep draining
    // trailing fields after the upload completes.
    let mut uploaded: Option<UploadedDoc> = None;

    while let Some(mut field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::invalid(format!("upload read failed: {e}")))?
    {
        match field.name().unwrap_or("") {
            "form_code" => {
                let v = field
                    .text()
                    .await
                    .map_err(|e| AppError::invalid(format!("bad form_code: {e}")))?;
                if forms::lookup(&v).is_some() {
                    form_code = v.to_ascii_uppercase();
                }
            }
            "item_id" => {
                let v = field
                    .text()
                    .await
                    .map_err(|e| AppError::invalid(format!("bad item_id: {e}")))?;
                if !v.is_empty() {
                    item_id = Some(v);
                }
            }
            "file" if uploaded.is_none() => {
                let filename = field
                    .file_name()
                    .map(|n| sanitize_filename(n.to_string()))
                    .unwrap_or_else(|| "upload.bin".into());
                let content_type = field
                    .content_type()
                    .unwrap_or("application/octet-stream")
                    .to_string();

                // Resolve the canonical form_code from the linked item
                // and reject uploads against an already-approved item —
                // these checks run *before* a single byte hits storage,
                // so the worst a probe can do is open a TCP connection.
                if let Some(ref iid) = item_id {
                    let item_ref = RecordId::new("checklist_item", iid.as_str());
                    use surrealdb::types::SurrealValue;
                    #[derive(serde::Deserialize, SurrealValue)]
                    struct ItemRow {
                        form_code: Option<String>,
                        approval_status: String,
                    }
                    // Cross-tenant guard: the item id is supplied by the
                    // client, so we can't trust it. Restrict the lookup
                    // to items that actually hang off the *URL's*
                    // transaction (already authorized above). A foreign
                    // or fabricated item_id falls through as NotFound.
                    let mut r = state
                        .db
                        .query(
                            "SELECT form_code, approval_status FROM ONLY $i \
                             WHERE id IN (SELECT VALUE out FROM has_item WHERE in = $t)",
                        )
                        .bind(("i", item_ref))
                        .bind(("t", tx_id.clone()))
                        .await?;
                    let row: Option<ItemRow> = r.take(0).ok().flatten();
                    let row = row.ok_or(AppError::NotFound)?;
                    if row.approval_status == "approved" {
                        return Err(AppError::invalid(
                            "This item has been approved and is locked. Ask a coordinator to deny it before uploading a replacement.",
                        ));
                    }
                    if let Some(code) = row.form_code {
                        form_code = code.to_ascii_uppercase();
                    }
                }

                let storage_key = make_storage_key(&user.brokerage_id, &tx, &form_code, &filename);

                // Pipe the multipart field straight into S3 multipart
                // upload — no Vec<u8> buffering, so a 100 MB upload stays
                // O(part_size) in process memory regardless of total size.
                let body_stream =
                    (&mut field).map_err(|e| std::io::Error::other(format!("multipart read: {e}")));
                let mut reader = StreamReader::new(body_stream);
                let size = state
                    .storage
                    .put_stream(&storage_key, &mut reader, &content_type)
                    .await
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("store upload: {e}")))?;

                // Scrubbed, not raw: `%` formats through `Display`, which
                // the pretty formatter (the production default) emits
                // verbatim, so a newline in either value would forge a
                // whole log record. `form_code` is freely hand-typeable
                // at the checklist form and stored without an ASSERT.
                tracing::info!(
                    filename = %crate::sanitize::scrub(&filename),
                    form_code = %crate::sanitize::scrub(&form_code),
                    bytes = size,
                    key = %storage_key,
                    "document streamed"
                );

                let signed = looks_signed(&filename, &content_type);

                // Atomic version-lookup + create. Two concurrent uploads
                // of the same filename / form_code used to both read
                // "latest = v1" and both insert v2 — see the duplicate
                // entries that started this fix. Wrapping the read +
                // write in a SurrealDB transaction makes the engine
                // serialize them: the second one reads v2 (the row the
                // first just inserted) and gets v3 instead of a dup v2.
                let (previous, doc) = create_versioned_document(
                    &state,
                    &tx_id,
                    &filename,
                    &form_code,
                    &storage_key,
                    size as i64,
                    &content_type,
                    signed,
                )
                .await?;

                uploaded = Some(UploadedDoc { doc, previous });
            }
            _ => {}
        }
    }

    let UploadedDoc { doc, previous } =
        uploaded.ok_or_else(|| AppError::invalid("Please choose a file to upload."))?;

    // SECURITY BACKSTOP — re-validate `item_id` here, after the
    // multipart loop, and use the RecordId this query returns for every
    // write below.
    //
    // The identical check inside the `"file"` arm above is only a
    // pre-storage fast-fail, and it CANNOT be the only one: multipart
    // part order is chosen by the client, so sending `file` before
    // `item_id` leaves `item_id == None` when that arm runs and skips
    // the guard entirely. The writes that follow then took a RecordId
    // built straight from unvalidated client input — which let a caller
    // attach a document to, and clear the review state of, a checklist
    // item in ANOTHER brokerage, and bypass the approved-item lock on
    // their own. Validating post-loop closes both regardless of order.
    //
    // A failure here leaves the document attached to the transaction
    // (a legitimate transaction-level upload) but unlinked from any
    // checklist item — no orphan, no partial write.
    let validated_item: Option<RecordId> = match item_id.as_deref() {
        Some(iid) => {
            #[derive(serde::Deserialize, SurrealValue)]
            struct ItemRow {
                id: RecordId,
                approval_status: String,
            }
            let mut r = state
                .db
                .query(
                    "SELECT id, approval_status FROM ONLY $i \
                     WHERE id IN (SELECT VALUE out FROM has_item WHERE in = $t)",
                )
                .bind(("i", RecordId::new("checklist_item", iid)))
                .bind(("t", tx_id.clone()))
                .await?;
            let row: Option<ItemRow> = r.take(0).ok().flatten();
            let row = row.ok_or(AppError::NotFound)?;
            if row.approval_status == "approved" {
                return Err(AppError::invalid(
                    "This item has been approved and is locked. Ask a coordinator to deny it before uploading a replacement.",
                ));
            }
            Some(row.id)
        }
        None => None,
    };

    state
        .db
        .query("RELATE $t->has_document->$d; RELATE $u->uploaded->$d;")
        .bind(("t", tx_id.clone()))
        .bind(("d", doc.id.clone()))
        .bind(("u", user.user_id.clone()))
        .await?;

    if let Some(item_ref) = validated_item.clone() {
        state
            .db
            .query("RELATE $d->for_item->$i")
            .bind(("d", doc.id.clone()))
            .bind(("i", item_ref.clone()))
            .await?;

        // A fresh upload against a previously DENIED item un-denies it:
        // the agent has corrected the issue, so the box flips back to
        // pending review (color + status pill) and the prior reviewer
        // attribution clears (otherwise the audit line would still say
        // "Denied by Alice on May 3" on a pending row). The deny-reason
        // comment from the popover stays in the thread as history.
        // No-op for items that aren't in the denied state — the WHERE
        // clause makes this safe for every upload path.
        state
            .db
            .query(
                "UPDATE $i SET approval_status = 'pending', \
                 reviewed_at = NONE, reviewed_by = NONE \
                 WHERE approval_status = 'denied'",
            )
            .bind(("i", item_ref))
            .await?;
    }

    if let Some(prev) = previous {
        state
            .db
            .query("RELATE $new->version_of->$old")
            .bind(("new", doc.id.clone()))
            .bind(("old", prev.id.clone()))
            .await?;

        // System-generated comment on the checklist item so the prior
        // version stays one click away even after newer ones bury it in
        // the doc list. Only posted when this upload is tied to an item
        // — transaction-level uploads have no comment thread to land in.
        if let Some(item_ref) = validated_item {
            let body = format!(
                "Uploaded v{} — replaces v{} of {}.",
                doc.version, prev.version, prev.filename,
            );
            let _: Option<crate::models::Comment> = state
                .db
                .create("comment")
                .content(crate::models::NewComment {
                    body,
                    target: item_ref,
                    author: user.user_id.clone(),
                    references_document: Some(prev.id),
                })
                .await?;
        }
    }

    state
        .events
        .publish(crate::events::Event::BrokerageMutation(
            user.brokerage_id.clone(),
        ));

    Ok(Redirect::to(&format!("/app/transactions/{id}")))
}

/// True when every checklist item on the transaction has been approved.
/// An empty checklist is *not* considered locked — there's nothing to
/// approve, and we don't want to block uploads on a fresh transaction.
async fn all_items_approved(state: &AppState, tx_id: &RecordId) -> Result<bool, AppError> {
    use surrealdb::types::SurrealValue;
    #[derive(serde::Deserialize, SurrealValue)]
    struct Row {
        total: i64,
        approved: i64,
    }
    let mut r = state
        .db
        .query(
            "SELECT
                count() AS total,
                count(approval_status = 'approved') AS approved
             FROM $t->has_item->checklist_item
             GROUP ALL",
        )
        .bind(("t", tx_id.clone()))
        .await?;
    let row: Option<Row> = r.take(0)?;
    Ok(match row {
        Some(r) => r.total > 0 && r.total == r.approved,
        None => false,
    })
}

/// True when at least one checklist item on the transaction is
/// approved. Once that's the case we treat the transaction as past the
/// "fix-it-myself" phase and forbid document deletions — the agent has
/// to ask a coordinator to deny the affected item and re-upload.
async fn any_item_approved(state: &AppState, tx_id: &RecordId) -> Result<bool, AppError> {
    use surrealdb::types::SurrealValue;
    #[derive(serde::Deserialize, SurrealValue)]
    struct Row {
        count: i64,
    }
    let mut r = state
        .db
        .query(
            "SELECT count() FROM $t->has_item->checklist_item
             WHERE approval_status = 'approved'
             GROUP ALL",
        )
        .bind(("t", tx_id.clone()))
        .await?;
    let row: Option<Row> = r.take(0)?;
    Ok(row.map(|c| c.count > 0).unwrap_or(false))
}

/// Delete a single uploaded document. Available to any authorized
/// brokerage member as long as no checklist item on the transaction
/// has been approved yet — once a reviewer has signed off on anything,
/// removing files would muddy the audit trail.
///
/// Order of operations is "DB row first, then storage": if the storage
/// purge fails we end up with an orphan object (cheap, recoverable),
/// not an orphan DB row pointing at a missing key (which would 500 on
/// download). Edges defined `ON DELETE CASCADE` in the schema clean
/// themselves up.
pub async fn delete(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(doc_id): Path<String>,
) -> Result<Redirect, AppError> {
    let doc_ref = RecordId::new("document", doc_id.as_str());
    let doc: Option<Document> = state.db.select(doc_ref.clone()).await?;
    let doc = doc.ok_or(AppError::NotFound)?;

    // Find the owning transaction so we can authorize the caller.
    let mut r = state
        .db
        .query("SELECT VALUE in FROM has_document WHERE out = $d LIMIT 1")
        .bind(("d", doc_ref.clone()))
        .await?;
    let txs: Vec<RecordId> = r.take(0)?;
    let tx_id = txs.into_iter().next().ok_or(AppError::NotFound)?;
    let _ = authorize_transaction(&state, &user, &tx_id).await?;

    if any_item_approved(&state, &tx_id).await? {
        return Err(AppError::invalid(
            "Documents are locked once a coordinator has approved any checklist item. Have them deny the affected item to reopen the transaction for edits.",
        ));
    }

    // Drop edges first so the doc record can't be left dangling under a
    // half-completed delete. SurrealDB's `DELETE` on a record cleans up
    // its incoming/outgoing edges automatically, but being explicit here
    // makes intent and ordering obvious in a review.
    state
        .db
        .query(
            "DELETE has_document WHERE out = $d;
             DELETE for_item     WHERE in  = $d;
             DELETE uploaded     WHERE out = $d;
             DELETE version_of   WHERE in  = $d OR out = $d;
             DELETE $d;",
        )
        .bind(("d", doc_ref))
        .await?;

    // Best-effort storage purge — if RustFS hiccups we still want the
    // DB state consistent. The object becomes a small recoverable orphan
    // that the next `dev_wipe_bucket` or a periodic sweep would clean.
    if let Err(e) = state.storage.delete(&doc.storage_key).await {
        tracing::warn!(error = %e, key = %doc.storage_key, "storage delete failed; DB row already removed");
    }

    crate::audit::record(
        &state.db,
        "document_deleted",
        Some(user.user_id.clone()),
        Some(user.email.clone()),
        None,
        None,
        Some(format!(
            "filename={} version={} form={}",
            doc.filename, doc.version, doc.form_code
        )),
    )
    .await;

    state
        .events
        .publish(crate::events::Event::BrokerageMutation(
            user.brokerage_id.clone(),
        ));

    let tx_key = crate::db::record_key(&tx_id);
    Ok(Redirect::to(&format!("/app/transactions/{tx_key}")))
}

/// Stream a single document's bytes to the browser as a download
/// (`Content-Disposition: attachment`). Same auth as preview.
///
/// Carries the same hardening headers as [`preview`]. The stored
/// `Content-Type` is whatever the uploader's browser claimed (there is
/// no magic-byte validation), so `nosniff` + a `sandbox` CSP make sure
/// an `.html` or SVG masquerading as a contract can never execute in
/// this origin if the disposition is ever relaxed or a browser decides
/// to render it anyway.
pub async fn download(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(doc_id): Path<String>,
) -> Result<Response, AppError> {
    let (doc, bytes) = authorize_and_fetch(&state, &user, &doc_id).await?;
    let safe_name = doc.filename.replace('"', "_");
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, doc.content_type)
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{safe_name}\""),
        )
        .header(header::CACHE_CONTROL, "private, no-store")
        .header("X-Content-Type-Options", "nosniff")
        .header("Content-Security-Policy", "sandbox")
        .body(Body::from(bytes))
        .map_err(|e| AppError::Internal(anyhow::anyhow!("build response: {e}")))
}
/// Content-Security-Policy for an inline document preview.
///
/// These bytes are displayed inside the same-origin preview lightbox, so
/// the response must permit being framed by us. The app-wide policy says
/// `frame-ancestors 'none'`, which blocks that outright — which is why
/// `security_headers` treats an already-set CSP as authoritative and
/// leaves this one alone.
///
/// PDFs additionally must NOT be sandboxed. A sandboxed document has the
/// sandboxed-plugins flag set, and the browser's built-in PDF viewer is
/// a plugin: the frame loads and then renders nothing at all. Dropping
/// `sandbox` for them is safe because `Content-Type: application/pdf`
/// plus `nosniff` means the bytes can never be interpreted as a document
/// in our origin, and PDF script runs inside the viewer rather than on
/// the page.
///
/// Every other kind stays sandboxed, and that is load-bearing rather
/// than decorative: [`Document::preview_kind`] admits any `image/*`, the
/// *uploader* chooses the declared content type, and `image/svg+xml` is
/// a scriptable format. Without the opaque origin `sandbox` imposes,
/// navigating straight to this URL would run an attacker's SVG script as
/// the signed-in user.
fn preview_csp(kind: Option<&str>) -> &'static str {
    match kind {
        Some("pdf") => "frame-ancestors 'self'",
        _ => "sandbox; frame-ancestors 'self'",
    }
}

/// Stream a document's bytes inline so the browser renders it in place
/// (PDF viewer, `<img>`, `<video>`, `<audio>`). Auth is identical to
/// download — same JWT cookie, same `authorize_transaction` brokerage
/// gate — so a copied URL is useless without a valid session. We only
/// serve preview-safe MIME families; anything else returns 404 so the
/// caller falls back to Download.
///
/// Headers worth calling out:
/// - `Cache-Control: private, no-store` keeps a shared device's cache
///   from holding sensitive contracts.
/// - `X-Content-Type-Options: nosniff` prevents the browser from
///   reinterpreting a misnamed payload as HTML/JS.
/// - `Content-Security-Policy: sandbox` neutralises scripts in any
///   embedded SVG/PDF the browser might otherwise execute.
pub async fn preview(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(doc_id): Path<String>,
) -> Result<Response, AppError> {
    let (doc, bytes) = authorize_and_fetch(&state, &user, &doc_id).await?;
    if !doc.can_preview() {
        return Err(AppError::NotFound);
    }
    let safe_name = doc.filename.replace('"', "_");

    let csp = preview_csp(doc.preview_kind());

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, doc.content_type)
        .header(
            header::CONTENT_DISPOSITION,
            format!("inline; filename=\"{safe_name}\""),
        )
        .header(header::CACHE_CONTROL, "private, no-store")
        .header("X-Content-Type-Options", "nosniff")
        .header("Content-Security-Policy", csp)
        // Paired with `frame-ancestors 'self'` for browsers that still
        // consult this; `DENY` (the app default) blocks even our own
        // lightbox.
        .header("X-Frame-Options", "SAMEORIGIN")
        .body(Body::from(bytes))
        .map_err(|e| AppError::Internal(anyhow::anyhow!("build response: {e}")))
}

/// Shared prologue for [`download`] and [`preview`]: look up the
/// document row, find its owning transaction via the `has_document`
/// edge, run the standard brokerage authorisation, then fetch bytes.
///
/// Returns 404 (`AppError::NotFound`) for: missing doc, doc with no
/// owning transaction, cross-brokerage requests, or missing storage
/// object. We deliberately don't distinguish "missing" from "forbidden"
/// here so a probe can't enumerate document IDs.
async fn authorize_and_fetch(
    state: &AppState,
    user: &CurrentUser,
    doc_id: &str,
) -> Result<(Document, bytes::Bytes), AppError> {
    let doc_ref = RecordId::new("document", doc_id);
    let doc: Option<Document> = state.db.select(doc_ref.clone()).await?;
    let doc = doc.ok_or(AppError::NotFound)?;

    let mut r = state
        .db
        .query("SELECT VALUE in FROM has_document WHERE out = $d LIMIT 1")
        .bind(("d", doc_ref))
        .await?;
    let txs: Vec<RecordId> = r.take(0)?;
    let tx_id = txs.into_iter().next().ok_or(AppError::NotFound)?;
    let _ = authorize_transaction(state, user, &tx_id).await?;

    let bytes = state
        .storage
        .get_bytes(&doc.storage_key)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("fetch bytes: {e}")))?
        .ok_or(AppError::NotFound)?;
    Ok((doc, bytes))
}

/// Throttle a caller's ZIP exports.
///
/// Exports are the most expensive thing a signed-in user can ask for: up
/// to [`EXPORT_MAX_BYTES`] streamed out of object storage into a temp
/// file, per request. They are also plain `GET`s, so `SameSite=Lax`
/// attaches the session cookie on a top-level navigation and the
/// Fetch-Metadata layer — which only guards unsafe methods — never sees
/// them. A hostile page could therefore drive a signed-in broker's
/// browser through full-brokerage exports on repeat.
///
/// Keyed per user rather than per IP so one busy office cannot throttle
/// a colleague.
fn allow_export(state: &AppState, user: &CurrentUser) -> Result<(), AppError> {
    let key = format!("export:{}", crate::db::record_key(&user.user_id));
    if crate::security::allow_per_hour(&state.rate_limiter, &key, 30) {
        Ok(())
    } else {
        Err(AppError::invalid(
            "You've requested a lot of exports in a short time. Give it a few minutes and try again.",
        ))
    }
}

/// Download a ZIP of every document attached to a transaction — the
/// one-click compliance export. Files are nested under their CAR form code
/// folder so the archive mirrors the storage layout.
pub async fn export_zip(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Response, AppError> {
    allow_export(&state, &user)?;
    let tx_id = RecordId::new("transaction", id.as_str());
    let tx = authorize_transaction(&state, &user, &tx_id).await?;

    let mut r = state
        .db
        .query(
            "SELECT * FROM $t->has_document->document \
             ORDER BY form_code, filename, version DESC",
        )
        .bind(("t", tx_id.clone()))
        .await?;
    let documents: Vec<Document> = r.take(0)?;

    let (manifest, items) = transaction_archive(&tx, &documents);
    let body = stream_archive(&state.storage, manifest, items).await?;
    // Same response builder as the team exports — this handler used to
    // hand-roll an identical one.
    zip_response(body, &format!("transactvault-{}", tx.property_address))
}

// ---------------------------------------------------------------------------
// Storage + ZIP helpers
// ---------------------------------------------------------------------------

/// `brokerage/<property folder>/<form code>/<uuid-filename>`.
/// Property folder = APN if available, else the transaction record key.
fn make_storage_key(
    brokerage: &RecordId,
    tx: &Transaction,
    form_code: &str,
    filename: &str,
) -> String {
    let property = property_folder(tx);
    format!(
        "{brokerage_key}/{property}/{form}/{uuid}-{name}",
        brokerage_key = crate::db::record_key(brokerage),
        property = property,
        form = sanitize_path_segment(form_code),
        uuid = uuid::Uuid::now_v7(),
        name = sanitize_path_segment(filename),
    )
}

fn property_folder(tx: &Transaction) -> String {
    let raw = tx
        .apn
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| crate::db::record_key(&tx.id));
    sanitize_path_segment(&raw)
}

/// Make one user-supplied string safe to use as a single path segment
/// — in an S3 key and in a ZIP entry name.
///
/// Separators and control characters become `_`, and the result is
/// never `.`, `..`, or empty.
///
/// The dot-segment rule is SECURITY, not cosmetics. Both consumers
/// resolve `..` rather than treating it as a literal name:
///
/// - **S3 keys**: `rust-s3` builds the request URL with `Url::parse`,
///   which applies WHATWG dot-segment removal *before* signing, so a
///   key like `<brokerage>/../RPA/file` silently writes to `RPA/file`
///   at the bucket root — outside the brokerage prefix that every
///   prefix-scoped IAM policy, lifecycle rule and quota depends on.
/// - **ZIP entries**: `ZipWriter::start_file` stores the name verbatim,
///   so `../../../x` is a classic zip-slip against whoever extracts the
///   compliance archive.
///
/// Callers must apply this to EVERY segment they interpolate. Form
/// codes are the sharp edge: `checklists::create` deliberately accepts
/// a hand-typed code for unknown forms, so `form_code` is
/// attacker-controlled all the way to both sinks.
///
/// Longest a single path segment may be, in characters.
///
/// S3 caps a whole object key at 1024 bytes. A key here is
/// `<brokerage>/<property>/<form code>/<filename>`, so bounding each
/// segment keeps the composed key inside that budget no matter what a
/// caller uploads — an unbounded filename previously produced a key the
/// provider rejected, surfacing as a 500 at the end of a completed
/// upload.
const SEGMENT_MAX: usize = 120;

fn sanitize_path_segment(s: &str) -> String {
    // `is_unsafe_text_char` widens the old `is_control` test to the
    // Unicode bidi overrides and zero-width characters. `is_control` is
    // category Cc only, so U+202E RIGHT-TO-LEFT OVERRIDE used to survive
    // into ZIP entry names and object keys, where it makes a listing show
    // a different extension than the file actually has. The length cap
    // keeps a long name from pushing the composed S3 key past the
    // provider's 1024-byte limit, which surfaced as a 500 on upload.
    let cleaned: String = crate::sanitize::truncate_chars(s, SEGMENT_MAX)
        .chars()
        .map(|c| match c {
            '/' | '\\' | '\0' | ' ' => '_',
            c if crate::sanitize::is_unsafe_text_char(c) => '_',
            c => c,
        })
        .collect::<String>()
        .trim_matches('_')
        .to_string();
    // `.` and `..` survive the pass above untouched — neutralize them
    // explicitly, along with the empty string.
    if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
        return "_".to_string();
    }
    cleaned
}

/// Make an untrusted value safe to interpolate into `MANIFEST.txt`.
///
/// The manifest is newline-delimited `Key: value`, so a newline inside a
/// property address, agent name or brokerage name lets the person who
/// typed it append lines of their own — `Document count: 99`, an extra
/// `Property:` entry, a fabricated agent section. A compliance auditor
/// reads that file as a statement of what the archive contains, which is
/// exactly why forging lines in it matters even though nothing executes.
fn manifest_field(s: &str) -> String {
    crate::sanitize::scrub(s)
}

/// Entry list + manifest for a single transaction's archive, ready for
/// [`stream_archive`].
fn transaction_archive(tx: &Transaction, docs: &[Document]) -> (String, Vec<ArchiveItem>) {
    let manifest = format!(
        "TransactVault export\n\
         Property: {}\n\
         APN: {}\n\
         Status: {}\n\
         Type: {}\n\
         Generated: {}\n\
         Document count: {}\n",
        manifest_field(&tx.property_address),
        manifest_field(tx.apn.as_deref().unwrap_or("—")),
        tx.status,
        tx.transaction_type,
        chrono::Utc::now().to_rfc3339(),
        docs.len(),
    );
    let items = docs
        .iter()
        .map(|doc| ArchiveItem {
            // Both segments sanitized — `form_code` is attacker-
            // controlled and `start_file` stores names verbatim.
            path: format!(
                "{}/{}",
                sanitize_path_segment(&doc.form_code),
                zip_safe_filename(doc)
            ),
            storage_key: doc.storage_key.clone(),
            size_bytes: doc.size_bytes,
        })
        .collect();
    (manifest, items)
}

// ---------------------------------------------------------------------------
// Team-level ZIP exports (broker only)
// ---------------------------------------------------------------------------

/// Hard ceiling on the total (uncompressed) content size of one export.
///
/// Since [`stream_archive`] streams both in and out, this no longer
/// guards process memory — it bounds **temp-file disk** and how long a
/// single request can occupy a connection. Past the cap we refuse with
/// a message pointing at the narrower per-transaction export.
const EXPORT_MAX_BYTES: i64 = 400 * 1024 * 1024;

/// Reject an export whose total content would exceed
/// [`EXPORT_MAX_BYTES`], before a single byte is fetched.
///
/// `size_bytes` is trustworthy: it is written from the byte count
/// `put_stream` actually observed, not from a client-supplied
/// `Content-Length`.
fn enforce_export_size(sizes: impl Iterator<Item = i64>) -> Result<(), AppError> {
    let total: i64 = sizes.map(|b| b.max(0)).sum();
    if total > EXPORT_MAX_BYTES {
        return Err(AppError::invalid(format!(
            "This export would be about {} MB — more than the {} MB limit for a \
             single archive. Export individual transactions instead (each \
             transaction page has its own Export ZIP button).",
            total / (1024 * 1024),
            EXPORT_MAX_BYTES / (1024 * 1024),
        )));
    }
    Ok(())
}

/// Download a ZIP of every document across every transaction owned by
/// one team member — the per-agent compliance archive on the Team page.
/// Layout: `<Property>/<FORM CODE>/<file>`, with a MANIFEST.txt at the
/// root listing each transaction. Broker only.
pub async fn export_member_zip(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(user_key): Path<String>,
) -> Result<Response, AppError> {
    if !user.role.is_broker() {
        return Err(AppError::Forbidden);
    }
    allow_export(&state, &user)?;

    // The target must be a member of the caller's brokerage — a broker
    // can't export another office's agent by guessing a user key.
    let target = RecordId::new("user", user_key.as_str());
    let mut mq = state
        .db
        .query("SELECT VALUE in FROM works_at WHERE in = $u AND out = $b LIMIT 1")
        .bind(("u", target.clone()))
        .bind(("b", user.brokerage_id.clone()))
        .await?;
    let member: Vec<RecordId> = mq.take(0).unwrap_or_default();
    if member.is_empty() {
        return Err(AppError::NotFound);
    }

    #[derive(serde::Deserialize, SurrealValue)]
    struct UserMeta {
        name: String,
        email: String,
    }
    let mut uq = state
        .db
        .query("SELECT name, email FROM ONLY $u")
        .bind(("u", target.clone()))
        .await?;
    let meta: Option<UserMeta> = uq.take(0)?;
    let meta = meta.ok_or(AppError::NotFound)?;

    // Their transactions, re-scoped to this brokerage (defense in depth;
    // ownership already implies membership under current invariants).
    let mut tq = state
        .db
        .query(
            "SELECT * FROM $u->owns->transaction \
             WHERE id IN (SELECT VALUE out FROM has_transaction WHERE in = $b) \
             ORDER BY property_address ASC",
        )
        .bind(("u", target.clone()))
        .bind(("b", user.brokerage_id.clone()))
        .await?;
    let txs: Vec<Transaction> = tq.take(0).unwrap_or_default();

    let docs_by_tx = load_docs_for_transactions(&state, &txs).await?;

    // `<Property>/<FORM>/<file>` — property folders deduped so two
    // same-address transactions don't merge into one folder.
    let mut used_folders: Vec<String> = Vec::new();
    let mut entries: Vec<ArchiveItem> = Vec::new();
    let mut manifest = format!(
        "TransactVault agent export\nAgent: {} <{}>\nGenerated: {}\nTransactions: {}\n\n",
        manifest_field(&meta.name),
        manifest_field(&meta.email),
        chrono::Utc::now().to_rfc3339(),
        txs.len(),
    );
    for tx in &txs {
        let folder = unique_folder(&export_property_folder(tx), &mut used_folders);
        let docs = docs_by_tx.get(&tx.id).cloned().unwrap_or_default();
        manifest.push_str(&format!(
            "{folder}/ — {} ({} · {} document(s))\n",
            manifest_field(&tx.property_address),
            tx.status,
            docs.len(),
        ));
        for doc in docs {
            let path = format!(
                "{folder}/{}/{}",
                sanitize_path_segment(&doc.form_code),
                zip_safe_filename(&doc)
            );
            entries.push(ArchiveItem {
                path,
                storage_key: doc.storage_key.clone(),
                size_bytes: doc.size_bytes,
            });
        }
    }

    let body = stream_archive(&state.storage, manifest, entries).await?;
    zip_response(body, &format!("transactvault-{}-transactions", meta.name))
}

/// Download a ZIP of every document in the whole brokerage, organized
/// alphabetically by agent: `<Agent>/<Property>/<FORM CODE>/<file>`.
/// Transactions with no owner land under `Unassigned/`. Broker only.
pub async fn export_brokerage_zip(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Response, AppError> {
    if !user.role.is_broker() {
        return Err(AppError::Forbidden);
    }
    allow_export(&state, &user)?;

    #[derive(serde::Deserialize, SurrealValue)]
    struct BrokerageMeta {
        name: String,
    }
    let mut bq = state
        .db
        .query("SELECT name FROM ONLY $b")
        .bind(("b", user.brokerage_id.clone()))
        .await?;
    let brokerage: Option<BrokerageMeta> = bq.take(0)?;
    let brokerage_name = brokerage
        .map(|b| b.name)
        .unwrap_or_else(|| "brokerage".into());

    let mut tq = state
        .db
        .query("SELECT * FROM $b->has_transaction->transaction ORDER BY property_address ASC")
        .bind(("b", user.brokerage_id.clone()))
        .await?;
    let txs: Vec<Transaction> = tq.take(0).unwrap_or_default();
    let tx_ids: Vec<RecordId> = txs.iter().map(|t| t.id.clone()).collect();

    // Owner per transaction (one query), then owner display names.
    #[derive(serde::Deserialize, SurrealValue)]
    struct OwnsEdge {
        tx: RecordId,
        owner: RecordId,
    }
    let owner_by_tx: std::collections::HashMap<RecordId, RecordId> = if tx_ids.is_empty() {
        Default::default()
    } else {
        let mut oq = state
            .db
            .query("SELECT out AS tx, in AS owner FROM owns WHERE out IN $ids")
            .bind(("ids", tx_ids.clone()))
            .await?;
        let edges: Vec<OwnsEdge> = oq.take(0).unwrap_or_default();
        edges.into_iter().map(|e| (e.tx, e.owner)).collect()
    };
    #[derive(serde::Deserialize, SurrealValue)]
    struct UserRow {
        id: RecordId,
        name: String,
    }
    let owner_ids: Vec<RecordId> = {
        let mut seen: Vec<RecordId> = Vec::new();
        for o in owner_by_tx.values() {
            if !seen.contains(o) {
                seen.push(o.clone());
            }
        }
        seen
    };
    let names: std::collections::HashMap<RecordId, String> = if owner_ids.is_empty() {
        Default::default()
    } else {
        let mut nq = state
            .db
            .query("SELECT id, name FROM user WHERE id IN $ids")
            .bind(("ids", owner_ids))
            .await?;
        let rows: Vec<UserRow> = nq.take(0).unwrap_or_default();
        rows.into_iter().map(|r| (r.id, r.name)).collect()
    };

    let docs_by_tx = load_docs_for_transactions(&state, &txs).await?;

    // Alphabetical by agent (case-insensitive), "Unassigned" last, then
    // by address — the order the client reads the archive in.
    let mut ordered: Vec<(String, &Transaction)> = txs
        .iter()
        .map(|tx| {
            let agent = owner_by_tx
                .get(&tx.id)
                .and_then(|o| names.get(o))
                .cloned()
                .unwrap_or_else(|| "Unassigned".to_string());
            (agent, tx)
        })
        .collect();
    ordered.sort_by(|a, b| {
        let a_unassigned = a.0 == "Unassigned";
        let b_unassigned = b.0 == "Unassigned";
        a_unassigned
            .cmp(&b_unassigned)
            .then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase()))
            .then_with(|| a.1.property_address.cmp(&b.1.property_address))
    });

    let mut used_folders: Vec<String> = Vec::new();
    let mut entries: Vec<ArchiveItem> = Vec::new();
    let mut manifest = format!(
        "TransactVault brokerage export\nBrokerage: {}\nGenerated: {}\nTransactions: {}\n\n",
        manifest_field(&brokerage_name),
        chrono::Utc::now().to_rfc3339(),
        ordered.len(),
    );
    let mut last_agent = String::new();
    for (agent, tx) in &ordered {
        if *agent != last_agent {
            manifest.push_str(&format!("\n== {} ==\n", manifest_field(agent)));
            last_agent = agent.clone();
        }
        let agent_folder = sanitize_path_segment(agent);
        let folder = unique_folder(
            &format!("{agent_folder}/{}", export_property_folder(tx)),
            &mut used_folders,
        );
        let docs = docs_by_tx.get(&tx.id).cloned().unwrap_or_default();
        manifest.push_str(&format!(
            "{folder}/ — {} ({} · {} document(s))\n",
            manifest_field(&tx.property_address),
            tx.status,
            docs.len(),
        ));
        for doc in docs {
            let path = format!(
                "{folder}/{}/{}",
                sanitize_path_segment(&doc.form_code),
                zip_safe_filename(&doc)
            );
            entries.push(ArchiveItem {
                path,
                storage_key: doc.storage_key.clone(),
                size_bytes: doc.size_bytes,
            });
        }
    }

    let body = stream_archive(&state.storage, manifest, entries).await?;
    zip_response(
        body,
        &format!("transactvault-{brokerage_name}-all-transactions"),
    )
}

/// All documents for a set of transactions in two round trips (edges,
/// then rows), bucketed by transaction id.
async fn load_docs_for_transactions(
    state: &AppState,
    txs: &[Transaction],
) -> Result<std::collections::HashMap<RecordId, Vec<Document>>, AppError> {
    use std::collections::HashMap;
    let tx_ids: Vec<RecordId> = txs.iter().map(|t| t.id.clone()).collect();
    if tx_ids.is_empty() {
        return Ok(HashMap::new());
    }
    #[derive(serde::Deserialize, SurrealValue)]
    struct DocEdge {
        tx: RecordId,
        doc: RecordId,
    }
    let mut eq = state
        .db
        .query("SELECT in AS tx, out AS doc FROM has_document WHERE in IN $ids")
        .bind(("ids", tx_ids))
        .await?;
    let edges: Vec<DocEdge> = eq.take(0).unwrap_or_default();
    let doc_ids: Vec<RecordId> = edges.iter().map(|e| e.doc.clone()).collect();
    if doc_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let mut dq = state
        .db
        .query("SELECT * FROM document WHERE id IN $ids")
        .bind(("ids", doc_ids))
        .await?;
    let docs: Vec<Document> = dq.take(0).unwrap_or_default();
    let by_id: HashMap<RecordId, Document> = docs.into_iter().map(|d| (d.id.clone(), d)).collect();

    let mut out: HashMap<RecordId, Vec<Document>> = HashMap::new();
    for e in edges {
        if let Some(doc) = by_id.get(&e.doc) {
            out.entry(e.tx).or_default().push(doc.clone());
        }
    }
    // Stable order inside each folder: form code, then name, then
    // newest version first — mirrors the single-transaction export.
    for docs in out.values_mut() {
        docs.sort_by(|a, b| {
            a.form_code
                .cmp(&b.form_code)
                .then_with(|| a.filename.cmp(&b.filename))
                .then_with(|| b.version.cmp(&a.version))
        });
    }
    Ok(out)
}

/// Human-readable per-transaction folder: the street address when
/// present, else the APN/record-key folder used by the storage layout.
fn export_property_folder(tx: &Transaction) -> String {
    let addr = sanitize_path_segment(tx.property_address.trim());
    if addr.is_empty() {
        property_folder(tx)
    } else {
        addr
    }
}

/// Dedupe a folder path against the ones already handed out — a second
/// "123_Main_St" becomes "123_Main_St~2".
fn unique_folder(base: &str, used: &mut Vec<String>) -> String {
    let mut candidate = base.to_string();
    let mut n = 1;
    while used.iter().any(|u| u == &candidate) {
        n += 1;
        candidate = format!("{base}~{n}");
    }
    used.push(candidate.clone());
    candidate
}

/// Disambiguate an archive entry path against the ones already written.
///
/// [`unique_folder`] keeps two transactions from sharing a folder, but
/// nothing kept two *files* from sharing a name, and
/// [`sanitize_path_segment`] is lossy in a way that manufactures
/// collisions: `Q1 report.pdf` and `Q1_report.pdf` both reduce to
/// `Q1_report.pdf`. `ZipWriter` accepts duplicate names happily and most
/// extractors keep only the last one, so a compliance export could
/// silently ship fewer documents than it claimed — the manifest counted
/// both.
///
/// The counter goes before the extension (`Q1_report~2.pdf`) so the file
/// still opens in whatever application owns that type.
fn unique_entry_path(path: &str, used: &mut std::collections::HashSet<String>) -> String {
    if used.insert(path.to_string()) {
        return path.to_string();
    }
    let (stem, ext) = match path.rsplit_once('.') {
        // A leading dot is a dotfile, not an extension.
        Some((stem, ext)) if !stem.is_empty() && !stem.ends_with('/') => {
            (stem.to_string(), format!(".{ext}"))
        }
        _ => (path.to_string(), String::new()),
    };
    for n in 2.. {
        let candidate = format!("{stem}~{n}{ext}");
        if used.insert(candidate.clone()) {
            return candidate;
        }
    }
    unreachable!("an unbounded counter always finds a free name")
}

/// One file destined for an archive: where it lands inside the ZIP and
/// where to stream it from.
struct ArchiveItem {
    /// Path inside the archive. Every segment must already be
    /// sanitized — [`sanitize_path_segment`] explains why.
    path: String,
    storage_key: String,
    /// Used only for the pre-flight size check; the actual bytes are
    /// streamed, never trusted from this number.
    size_bytes: i64,
}

/// Build a ZIP and return it as a streaming response body.
///
/// Memory is O(one chunk), independent of both document size and
/// document count. Two things make that true:
///
/// 1. **Objects stream in.** Each document is pulled from storage in
///    chunks ([`Storage::get_stream`]) and fed straight into the
///    compressor, instead of being materialized whole.
/// 2. **The archive streams out.** The `zip` crate needs `Seek` (it
///    rewrites local headers), so the archive is assembled in a
///    temporary file rather than a `Vec`, then streamed to the client.
///    `tempfile::tempfile()` unlinks the file the moment it is created,
///    so it has no path, cannot be read by anything else, and is
///    reclaimed by the OS even if the process is killed mid-export.
///
/// The previous implementation buffered every object AND the finished
/// archive in memory, then held the whole thing for the response — a
/// brokerage-wide export peaked around 1.6 GB and could OOM the
/// process, taking every tenant down.
///
/// Disk replaces RAM here, which is the right trade for an export
/// endpoint; [`EXPORT_MAX_BYTES`] still bounds how much of it a single
/// request can use.
///
/// # Errors
///
/// Fails before any I/O when the archive would exceed
/// [`EXPORT_MAX_BYTES`]. A document that is missing from storage (or
/// whose fetch fails) does not fail the export — it becomes a visible
/// placeholder file, so one broken object can't sink an entire
/// compliance archive.
async fn stream_archive(
    storage: &Storage,
    manifest: String,
    items: Vec<ArchiveItem>,
) -> Result<Body, AppError> {
    use futures::StreamExt;
    use std::io::{Seek, Write};
    use zip::write::SimpleFileOptions;

    enforce_export_size(items.iter().map(|i| i.size_bytes))?;

    const MISSING: &[u8] = b"[file missing from storage]";
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);

    let file = tokio::task::spawn_blocking(tempfile::tempfile)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("spawn temp file: {e}")))?
        .map_err(|e| AppError::Internal(anyhow::anyhow!("create temp file: {e}")))?;
    let mut writer = zip::ZipWriter::new(file);

    writer
        .start_file("MANIFEST.txt", options)
        .map_err(|e| AppError::Internal(anyhow::anyhow!("zip manifest: {e}")))?;
    writer
        .write_all(manifest.as_bytes())
        .map_err(|e| AppError::Internal(anyhow::anyhow!("zip manifest write: {e}")))?;

    // Seeded with the manifest so no document can shadow it.
    let mut used_paths: std::collections::HashSet<String> =
        std::iter::once("MANIFEST.txt".to_string()).collect();

    for item in items {
        let entry_path = unique_entry_path(&item.path, &mut used_paths);
        writer
            .start_file(entry_path, options)
            .map_err(|e| AppError::Internal(anyhow::anyhow!("zip entry: {e}")))?;

        match storage.get_stream(&item.storage_key).await {
            Ok(Some(mut stream)) => {
                // Compress chunk-by-chunk. Each `next().await` is a
                // yield point, so the short synchronous deflate bursts
                // between them never starve the runtime.
                while let Some(chunk) = stream.bytes.next().await {
                    let chunk = chunk
                        .map_err(|e| AppError::Internal(anyhow::anyhow!("stream object: {e}")))?;
                    writer
                        .write_all(&chunk)
                        .map_err(|e| AppError::Internal(anyhow::anyhow!("zip write: {e}")))?;
                }
            }
            Ok(None) => {
                writer
                    .write_all(MISSING)
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("zip write: {e}")))?;
            }
            Err(e) => {
                tracing::warn!(error = %e, key = %item.storage_key, "export: object stream failed");
                writer
                    .write_all(MISSING)
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("zip write: {e}")))?;
            }
        }
    }

    // `finish` writes the central directory and hands the file back.
    let mut file = writer
        .finish()
        .map_err(|e| AppError::Internal(anyhow::anyhow!("zip finish: {e}")))?;
    file.rewind()
        .map_err(|e| AppError::Internal(anyhow::anyhow!("rewind archive: {e}")))?;

    Ok(Body::from_stream(tokio_util::io::ReaderStream::new(
        tokio::fs::File::from_std(file),
    )))
}

/// Wrap a streaming archive body in a download response. `stem` is slugged
/// the same way the single-transaction export names its file.
fn zip_response(body: Body, stem: &str) -> Result<Response, AppError> {
    let slug: String = stem
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect::<String>()
        .trim_matches('_')
        .to_string();
    let name = if slug.is_empty() {
        "transactvault-export.zip".to_string()
    } else {
        format!("{slug}.zip")
    };
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/zip")
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{name}\""),
        )
        .body(body)
        .map_err(|e| AppError::Internal(anyhow::anyhow!("build response: {e}")))
}

/// Archive entry leaf name: the stored filename, with `_v2`-style
/// version suffixes so multiple versions don't collide in one folder.
///
/// Runs through [`sanitize_path_segment`] because `sanitize_filename`
/// (applied at upload) neutralizes separators but not dot segments —
/// a file literally named `..` would otherwise become a traversing ZIP
/// entry. Ordinary filenames pass through unchanged.
fn zip_safe_filename(doc: &Document) -> String {
    let name = if doc.version > 1 {
        let (stem, ext) = split_filename(&doc.filename);
        if ext.is_empty() {
            format!("{stem}_v{}", doc.version)
        } else {
            format!("{stem}_v{}.{}", doc.version, ext)
        }
    } else {
        doc.filename.clone()
    };
    sanitize_path_segment(&name)
}

fn split_filename(name: &str) -> (String, String) {
    match name.rfind('.') {
        Some(idx) if idx > 0 => (name[..idx].to_string(), name[idx + 1..].to_string()),
        _ => (name.to_string(), String::new()),
    }
}

fn sanitize_filename(name: String) -> String {
    crate::sanitize::truncate_chars(&name, SEGMENT_MAX)
        .chars()
        .map(|c| match c {
            '/' | '\\' | '\0' => '_',
            c if crate::sanitize::is_unsafe_text_char(c) => '_',
            c => c,
        })
        .collect::<String>()
        .trim()
        .to_string()
}

/// Heuristic: treat uploads as "signed" if the filename hints at it. PDFs
/// containing a signature dictionary would be a nicer signal but require
/// a PDF parser we don't want to bring in for the PoC.
fn looks_signed(filename: &str, content_type: &str) -> bool {
    let lower = filename.to_ascii_lowercase();
    (content_type == "application/pdf" || lower.ends_with(".pdf"))
        && (lower.contains("signed") || lower.contains("executed") || lower.contains("final"))
}

/// Result of a successful streaming upload — kept inside an `Option` while
/// the multipart loop runs so trailing fields can still be drained.
struct UploadedDoc {
    doc: Document,
    previous: Option<Document>,
}

/// Determine the next version number and create the document row.
///
/// Implementation note: an earlier version of this function wrapped
/// the read + write in a `BEGIN/COMMIT` block with `LET` bindings + a
/// `RETURN { prev, new }` payload, intending to atomically serialize
/// two concurrent uploads of the same `(transaction, filename,
/// form_code)`. That structure consistently came back empty from
/// `take(0)` against SurrealDB v3 — the slot indexing inside a
/// transaction with LET statements is brittle and several wrap
/// attempts (block scoping, statement reordering) didn't change the
/// outcome. The simpler two-query path below works reliably; the
/// rare interleave (two uploads of the same filename + form_code
/// landing in the same millisecond) is handled by the browser-side
/// double-submit guard added in the original "duplicate upload
/// race" fix.
///
/// Returns `(previous, new)` so the caller can wire the `version_of`
/// edge and post the audit comment exactly as before.
#[allow(clippy::too_many_arguments)]
async fn create_versioned_document(
    state: &AppState,
    tx_id: &RecordId,
    filename: &str,
    form_code: &str,
    storage_key: &str,
    size_bytes: i64,
    content_type: &str,
    signed: bool,
) -> Result<(Option<Document>, Document), AppError> {
    // Find the most-recent existing version (if any). Returns an
    // empty vec when nothing matches yet — fresh document path.
    let mut q = state
        .db
        .query(
            "SELECT * FROM $t->has_document->document
             WHERE filename = $f AND form_code = $fc
             ORDER BY version DESC LIMIT 1",
        )
        .bind(("t", tx_id.clone()))
        .bind(("f", filename.to_string()))
        .bind(("fc", form_code.to_string()))
        .await?;
    let prev_docs: Vec<Document> = q.take(0).unwrap_or_default();
    let previous = prev_docs.into_iter().next();
    let next_version = previous.as_ref().map(|p| p.version + 1).unwrap_or(1);

    let created: Option<Document> = state
        .db
        .create("document")
        .content(crate::models::NewDocument {
            filename: filename.to_string(),
            form_code: form_code.to_string(),
            storage_key: storage_key.to_string(),
            size_bytes,
            content_type: content_type.to_string(),
            signed,
            version: next_version,
        })
        .await?;
    let new = created
        .ok_or_else(|| AppError::Internal(anyhow::anyhow!("CREATE document returned nothing")))?;

    Ok((previous, new))
}

#[cfg(test)]
mod path_safety_tests {
    use super::{
        manifest_field, preview_csp, sanitize_path_segment, split_filename, unique_entry_path,
    };

    /// Preview responses must be framable by our own lightbox, and PDFs
    /// must not be sandboxed.
    ///
    /// Both halves are regressions that shipped: the app-wide
    /// `frame-ancestors 'none'` blocked the preview `<iframe>` entirely,
    /// and a sandboxed document cannot instantiate the browser's PDF
    /// viewer plugin, so the frame would render blank even once framing
    /// was allowed.
    #[test]
    fn preview_policy_allows_framing_and_spares_the_pdf_viewer() {
        let pdf = preview_csp(Some("pdf"));
        assert!(pdf.contains("frame-ancestors 'self'"));
        assert!(
            !pdf.contains("sandbox"),
            "sandboxing a PDF blocks the viewer plugin: {pdf}"
        );

        // Everything else keeps the sandbox. `preview_kind` admits any
        // `image/*` and the uploader picks the declared type, so
        // `image/svg+xml` reaches here — scriptable, and dangerous on a
        // direct navigation without an opaque origin.
        for kind in [Some("image"), Some("video"), Some("audio"), None] {
            let csp = preview_csp(kind);
            assert!(
                csp.contains("sandbox"),
                "{kind:?} must stay sandboxed: {csp}"
            );
            assert!(csp.contains("frame-ancestors 'self'"), "{kind:?}: {csp}");
        }

        // Nothing may carry the page policy's framing ban.
        for kind in [Some("pdf"), Some("image"), None] {
            assert!(!preview_csp(kind).contains("frame-ancestors 'none'"));
        }
    }

    /// A bidi override in a filename makes a listing show one extension
    /// while the bytes are another — the classic `\u{202E}` trick turns
    /// `Contract<RLO>gpj.exe` into a displayed `Contractexe.jpg`.
    /// `char::is_control` is category Cc only and never caught it.
    #[test]
    fn bidi_overrides_do_not_survive_into_paths() {
        let spoofed = sanitize_path_segment("Contract\u{202E}gpj.exe");
        assert!(!spoofed.contains('\u{202E}'), "got {spoofed:?}");
        assert_eq!(spoofed, "Contract_gpj.exe");

        for c in ['\u{200B}', '\u{200E}', '\u{2028}', '\u{FEFF}', '\u{2066}'] {
            let out = sanitize_path_segment(&format!("a{c}b"));
            assert!(!out.contains(c), "U+{:04X} survived as {out:?}", c as u32);
        }
    }

    /// S3 caps a key at 1024 bytes, and the key is four segments joined.
    /// An unbounded filename used to produce a key the provider refused,
    /// which surfaced as a 500 at the very end of a completed upload.
    #[test]
    fn path_segments_are_length_capped() {
        let out = sanitize_path_segment(&"x".repeat(5000));
        assert!(out.chars().count() <= super::SEGMENT_MAX, "{}", out.len());
    }

    /// Sanitizing is lossy, so distinct filenames can collide. ZIP
    /// tolerates duplicate entry names and most extractors keep only the
    /// last, which would silently drop documents from a compliance
    /// export whose manifest counted them all.
    #[test]
    fn duplicate_archive_entries_are_disambiguated() {
        let mut used = std::collections::HashSet::new();
        used.insert("MANIFEST.txt".to_string());

        assert_eq!(
            unique_entry_path("RPA/report.pdf", &mut used),
            "RPA/report.pdf"
        );
        assert_eq!(
            unique_entry_path("RPA/report.pdf", &mut used),
            "RPA/report~2.pdf",
            "the counter goes before the extension so the file still opens"
        );
        assert_eq!(
            unique_entry_path("RPA/report.pdf", &mut used),
            "RPA/report~3.pdf"
        );

        // Nothing may shadow the manifest.
        assert_eq!(
            unique_entry_path("MANIFEST.txt", &mut used),
            "MANIFEST~2.txt"
        );

        // Extensionless names and dotfiles still get a unique name.
        assert_eq!(unique_entry_path("RPA/notes", &mut used), "RPA/notes");
        assert_eq!(unique_entry_path("RPA/notes", &mut used), "RPA/notes~2");
    }

    /// `MANIFEST.txt` is newline-delimited `Key: value`, so a newline in
    /// a property address let the person who typed it append lines an
    /// auditor reads as a statement of what the archive contains.
    #[test]
    fn manifest_fields_cannot_forge_lines() {
        let forged = manifest_field("123 Main St\nDocument count: 99\nProperty: 999 Fake Ave");
        assert!(!forged.contains('\n'));
        assert!(!forged.contains('\r'));
        assert_eq!(
            forged,
            "123 Main St Document count: 99 Property: 999 Fake Ave"
        );
        // Ordinary addresses are untouched.
        assert_eq!(
            manifest_field("1250 Oak Ave, Suite 3"),
            "1250 Oak Ave, Suite 3"
        );
    }

    /// Dot segments must never survive: S3 URL building resolves them
    /// (escaping the brokerage prefix) and ZIP extractors resolve them
    /// on disk (zip-slip). Form codes reach both sinks and are
    /// attacker-controlled — `checklists::create` accepts any
    /// hand-typed code for unknown forms.
    #[test]
    fn dot_segments_are_neutralized() {
        assert_eq!(sanitize_path_segment(".."), "_");
        assert_eq!(sanitize_path_segment("."), "_");
        assert_eq!(sanitize_path_segment(""), "_");
        // Separators inside a segment are flattened, so a traversing
        // code collapses to one inert segment.
        assert_eq!(
            sanitize_path_segment("../../etc/passwd"),
            ".._.._etc_passwd"
        );
        assert_eq!(sanitize_path_segment("/absolute"), "absolute");
        assert_eq!(sanitize_path_segment("a\\b"), "a_b");
        assert_eq!(sanitize_path_segment("with space"), "with_space");
        assert_eq!(sanitize_path_segment("nul\0byte"), "nul_byte");
    }

    /// Ordinary values pass through untouched — the guard must not
    /// mangle real form codes or filenames.
    #[test]
    fn ordinary_segments_are_untouched() {
        assert_eq!(sanitize_path_segment("RPA"), "RPA");
        assert_eq!(sanitize_path_segment("SWPI-C"), "SWPI-C");
        assert_eq!(sanitize_path_segment("CC&R"), "CC&R");
        assert_eq!(
            sanitize_path_segment("Disclosure.v2.pdf"),
            "Disclosure.v2.pdf"
        );
    }

    #[test]
    fn split_filename_handles_dotfiles_and_bare_names() {
        assert_eq!(
            split_filename("report.pdf"),
            ("report".to_string(), "pdf".to_string())
        );
        assert_eq!(
            split_filename("no-extension"),
            ("no-extension".to_string(), String::new())
        );
    }
}
