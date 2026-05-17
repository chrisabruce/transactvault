//! Document upload, download, and transaction-level ZIP export.
//!
//! Storage layout follows: `<brokerage>/<property folder>/<form code>/<file>`
//! where the property folder uses APN if available, otherwise the
//! transaction record key. This mirrors how a brokerage would organise
//! files in a physical filing cabinet.

use std::io::Write;

use axum::body::Body;
use axum::extract::{Multipart, Path, State};
use axum::http::{StatusCode, header};
use axum::response::{Redirect, Response};
use futures::TryStreamExt;
use surrealdb::types::RecordId;
use tokio_util::io::StreamReader;

use crate::auth::CurrentUser;
use crate::controllers::transactions::authorize_transaction;
use crate::error::AppError;
use crate::forms;
use crate::models::{Document, NewDocument, Transaction};
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
                    let mut r = state
                        .db
                        .query("SELECT form_code, approval_status FROM ONLY $i")
                        .bind(("i", item_ref))
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

                // Versioning: find the latest doc with the same filename +
                // form code on this tx so we know what version number to
                // assign and which prior row to RELATE `version_of` to.
                let mut existing_q = state
                    .db
                    .query(
                        "SELECT * FROM $t->has_document->document
                         WHERE filename = $f AND form_code = $fc
                         ORDER BY version DESC LIMIT 1",
                    )
                    .bind(("t", tx_id.clone()))
                    .bind(("f", filename.clone()))
                    .bind(("fc", form_code.clone()))
                    .await?;
                let previous: Option<Document> = existing_q.take(0)?;
                let version = previous.as_ref().map(|p| p.version + 1).unwrap_or(1);

                let storage_key =
                    make_storage_key(&user.brokerage_id, &tx, &form_code, &filename);

                // Pipe the multipart field straight into S3 multipart
                // upload — no Vec<u8> buffering, so a 100 MB upload stays
                // O(part_size) in process memory regardless of total size.
                let body_stream = (&mut field).map_err(|e| {
                    std::io::Error::other(format!("multipart read: {e}"))
                });
                let mut reader = StreamReader::new(body_stream);
                let size = state
                    .storage
                    .put_stream(&storage_key, &mut reader, &content_type)
                    .await
                    .map_err(|e| AppError::Internal(anyhow::anyhow!("store upload: {e}")))?;

                tracing::info!(
                    %filename,
                    %form_code,
                    bytes = size,
                    key = %storage_key,
                    "document streamed"
                );

                let signed = looks_signed(&filename, &content_type);

                let new_doc: Option<Document> = state
                    .db
                    .create("document")
                    .content(NewDocument {
                        filename: filename.clone(),
                        form_code: form_code.clone(),
                        storage_key: storage_key.clone(),
                        size_bytes: size as i64,
                        content_type,
                        signed,
                        version,
                    })
                    .await?;
                let doc = new_doc.ok_or_else(|| {
                    AppError::Internal(anyhow::anyhow!("insert returned nothing"))
                })?;

                uploaded = Some(UploadedDoc { doc, previous });
            }
            _ => {}
        }
    }

    let UploadedDoc { doc, previous } = uploaded
        .ok_or_else(|| AppError::invalid("Please choose a file to upload."))?;

    state
        .db
        .query("RELATE $t->has_document->$d; RELATE $u->uploaded->$d;")
        .bind(("t", tx_id.clone()))
        .bind(("d", doc.id.clone()))
        .bind(("u", user.user_id.clone()))
        .await?;

    if let Some(ref iid) = item_id {
        let item_ref = RecordId::new("checklist_item", iid.as_str());
        state
            .db
            .query("RELATE $d->for_item->$i")
            .bind(("d", doc.id.clone()))
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
        if let Some(ref iid) = item_id {
            let item_ref = RecordId::new("checklist_item", iid.as_str());
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

    let tx_key = crate::db::record_key(&tx_id);
    Ok(Redirect::to(&format!("/app/transactions/{tx_key}")))
}

/// Stream a single document's bytes to the browser as a download
/// (`Content-Disposition: attachment`). Same auth as preview.
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
        .body(Body::from(bytes))
        .map_err(|e| AppError::Internal(anyhow::anyhow!("build response: {e}")))
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
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, doc.content_type)
        .header(
            header::CONTENT_DISPOSITION,
            format!("inline; filename=\"{safe_name}\""),
        )
        .header(header::CACHE_CONTROL, "private, no-store")
        .header("X-Content-Type-Options", "nosniff")
        .header("Content-Security-Policy", "sandbox")
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

/// Download a ZIP of every document attached to a transaction — the
/// one-click compliance export. Files are nested under their CAR form code
/// folder so the archive mirrors the storage layout.
pub async fn export_zip(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Response, AppError> {
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

    let zip_bytes = build_zip(&state.storage, &tx, &documents).await?;
    let zip_name = zip_filename_for(&tx);

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/zip")
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{zip_name}\""),
        )
        .body(Body::from(zip_bytes))
        .map_err(|e| AppError::Internal(anyhow::anyhow!("build response: {e}")))
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

fn sanitize_path_segment(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '/' | '\\' | '\0' | ' ' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect::<String>()
        .trim_matches('_')
        .to_string()
}

async fn build_zip(
    storage: &Storage,
    tx: &Transaction,
    docs: &[Document],
) -> Result<Vec<u8>, AppError> {
    use zip::write::SimpleFileOptions;

    // Missing-from-storage and transport-failure are both surfaced as a
    // visible placeholder file in the ZIP so the export still completes;
    // a busted single document shouldn't sink the whole compliance
    // archive.
    const MISSING: &[u8] = b"[file missing from storage]";
    let payloads = futures::future::join_all(docs.iter().map(|doc| async move {
        let bytes = match storage.get_bytes(&doc.storage_key).await {
            Ok(Some(b)) => b,
            Ok(None) => bytes::Bytes::from_static(MISSING),
            Err(e) => {
                tracing::warn!(error = %e, key = %doc.storage_key, "zip: get_bytes failed");
                bytes::Bytes::from_static(MISSING)
            }
        };
        (doc, bytes)
    }))
    .await;

    let mut cursor = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut cursor);
        let options =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);

        let manifest = format!(
            "TransactVault export\n\
             Property: {}\n\
             APN: {}\n\
             Status: {}\n\
             Type: {}\n\
             Generated: {}\n\
             Document count: {}\n",
            tx.property_address,
            tx.apn.as_deref().unwrap_or("—"),
            tx.status,
            tx.transaction_type,
            chrono::Utc::now().to_rfc3339(),
            docs.len(),
        );
        writer
            .start_file("MANIFEST.txt", options)
            .map_err(|e| AppError::Internal(anyhow::anyhow!("zip manifest: {e}")))?;
        writer
            .write_all(manifest.as_bytes())
            .map_err(|e| AppError::Internal(anyhow::anyhow!("zip manifest write: {e}")))?;

        for (doc, bytes) in payloads {
            let arc_name = format!("{}/{}", doc.form_code, zip_safe_filename(doc));
            writer
                .start_file(arc_name, options)
                .map_err(|e| AppError::Internal(anyhow::anyhow!("zip entry: {e}")))?;
            writer
                .write_all(&bytes)
                .map_err(|e| AppError::Internal(anyhow::anyhow!("zip write: {e}")))?;
        }
        writer
            .finish()
            .map_err(|e| AppError::Internal(anyhow::anyhow!("zip finish: {e}")))?;
    }
    Ok(cursor.into_inner())
}

fn zip_safe_filename(doc: &Document) -> String {
    if doc.version > 1 {
        let (stem, ext) = split_filename(&doc.filename);
        if ext.is_empty() {
            format!("{stem}_v{}", doc.version)
        } else {
            format!("{stem}_v{}.{}", doc.version, ext)
        }
    } else {
        doc.filename.clone()
    }
}

fn split_filename(name: &str) -> (String, String) {
    match name.rfind('.') {
        Some(idx) if idx > 0 => (name[..idx].to_string(), name[idx + 1..].to_string()),
        _ => (name.to_string(), String::new()),
    }
}

fn zip_filename_for(tx: &Transaction) -> String {
    let slug: String = tx
        .property_address
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect::<String>()
        .trim_matches('_')
        .to_string();
    let stem = if slug.is_empty() {
        "transaction".into()
    } else {
        slug
    };
    format!("transactvault-{stem}.zip")
}

fn sanitize_filename(name: String) -> String {
    name.chars()
        .map(|c| match c {
            '/' | '\\' | '\0' => '_',
            c if c.is_control() => '_',
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
