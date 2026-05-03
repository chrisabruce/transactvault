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
use surrealdb::types::RecordId;

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

    let mut form_code = String::from("MISC");
    let mut item_id: Option<String> = None;
    let mut filename = String::new();
    let mut content_type = String::from("application/octet-stream");
    let mut bytes: Vec<u8> = Vec::new();

    while let Some(field) = multipart
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
            "file" => {
                filename = field
                    .file_name()
                    .map(|n| sanitize_filename(n.to_string()))
                    .unwrap_or_else(|| "upload.bin".into());
                content_type = field
                    .content_type()
                    .unwrap_or("application/octet-stream")
                    .to_string();
                bytes = field
                    .bytes()
                    .await
                    .map_err(|e| AppError::invalid(format!("read body: {e}")))?
                    .to_vec();
            }
            _ => {}
        }
    }

    if filename.is_empty() || bytes.is_empty() {
        return Err(AppError::invalid("Please choose a file to upload."));
    }

    // If item_id was sent, look up the item's form_code to use as the
    // canonical bucket — keeps the storage layout consistent even if the
    // form-code field on the upload form is stale.
    if let Some(ref iid) = item_id {
        let item_ref = RecordId::new("checklist_item", iid.as_str());
        use surrealdb::types::SurrealValue;
        #[derive(serde::Deserialize, SurrealValue)]
        struct CodeRow {
            form_code: Option<String>,
        }
        let mut r = state
            .db
            .query("SELECT form_code FROM ONLY $i")
            .bind(("i", item_ref))
            .await?;
        let row: Option<CodeRow> = r.take(0).ok().flatten();
        if let Some(code) = row.and_then(|r| r.form_code) {
            form_code = code.to_ascii_uppercase();
        }
    }

    // Versioning: latest doc with the same filename + form code on this tx.
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
    let signed = looks_signed(&filename, &content_type, &bytes);

    let storage_key = make_storage_key(&user.brokerage_id, &tx, &form_code, &filename);
    state
        .storage
        .put_bytes(&storage_key, bytes.clone(), &content_type)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("store upload: {e}")))?;
    tracing::info!(
        %filename,
        %form_code,
        bytes = bytes.len(),
        key = %storage_key,
        "document stored"
    );

    let new_doc: Option<Document> = state
        .db
        .create("document")
        .content(NewDocument {
            filename: filename.clone(),
            form_code: form_code.clone(),
            storage_key: storage_key.clone(),
            size_bytes: bytes.len() as i64,
            content_type,
            signed,
            version,
        })
        .await?;
    let doc = new_doc.ok_or_else(|| AppError::Internal(anyhow::anyhow!("insert returned nothing")))?;

    state
        .db
        .query("RELATE $t->has_document->$d; RELATE $u->uploaded->$d;")
        .bind(("t", tx_id.clone()))
        .bind(("d", doc.id.clone()))
        .bind(("u", user.user_id.clone()))
        .await?;

    if let Some(iid) = item_id {
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
            .bind(("old", prev.id))
            .await?;
    }

    Ok(Redirect::to(&format!("/app/transactions/{id}")))
}

/// Stream a single document's bytes to the browser, preserving the
/// original filename via `Content-Disposition`.
pub async fn download(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(doc_id): Path<String>,
) -> Result<Response, AppError> {
    let doc_ref = RecordId::new("document", doc_id.as_str());
    let doc: Option<Document> = state.db.select(doc_ref.clone()).await?;
    let doc = doc.ok_or(AppError::NotFound)?;

    let mut r = state
        .db
        .query("SELECT VALUE in FROM has_document WHERE out = $d LIMIT 1")
        .bind(("d", doc_ref))
        .await?;
    let txs: Vec<RecordId> = r.take(0)?;
    let tx_id = txs.into_iter().next().ok_or(AppError::NotFound)?;
    let _ = authorize_transaction(&state, &user, &tx_id).await?;

    let bytes = state
        .storage
        .get_bytes(&doc.storage_key)
        .await
        .map_err(|e| {
            if e.to_string().contains("not found") {
                AppError::NotFound
            } else {
                AppError::Internal(anyhow::anyhow!("fetch bytes: {e}"))
            }
        })?;
    let filename = doc.filename;

    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, doc.content_type)
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{}\"", filename.replace('"', "_")),
        )
        .body(Body::from(bytes))
        .map_err(|e| AppError::Internal(anyhow::anyhow!("build response: {e}")))?)
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

    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/zip")
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{zip_name}\""),
        )
        .body(Body::from(zip_bytes))
        .map_err(|e| AppError::Internal(anyhow::anyhow!("build response: {e}")))?)
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
        brokerage_key = crate::record_key(brokerage),
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
        .unwrap_or_else(|| crate::record_key(&tx.id));
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

    let payloads = futures::future::join_all(docs.iter().map(|doc| async move {
        let bytes = storage
            .get_bytes(&doc.storage_key)
            .await
            .unwrap_or_else(|_| bytes::Bytes::from_static(b"[file missing from storage]"));
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
    let stem = if slug.is_empty() { "transaction".into() } else { slug };
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
fn looks_signed(filename: &str, content_type: &str, _bytes: &[u8]) -> bool {
    let lower = filename.to_ascii_lowercase();
    (content_type == "application/pdf" || lower.ends_with(".pdf"))
        && (lower.contains("signed")
            || lower.contains("executed")
            || lower.contains("final"))
}
