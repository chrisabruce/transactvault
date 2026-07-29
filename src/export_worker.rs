//! Background brokerage-export builder.
//!
//! The synchronous ZIP exports in `controllers::documents` are capped at
//! [`crate::controllers::documents::EXPORT_MAX_BYTES`] because they build
//! the whole archive inside one HTTP request. A brokerage's full document
//! set can run to hundreds of gigabytes, so the full dump runs here
//! instead: a broker queues an `export_job` row, this worker claims it,
//! plans the archive as **chunks** (one per agent + year, split by month
//! and then by size-capped parts when needed), builds each chunk ZIP
//! through a temp file, and uploads it to object storage under
//! `exports/<brokerage>/<job>/`. Downloads then go straight to the store
//! via presigned GETs — resumable (`Range`) and off the app's bandwidth.
//!
//! Resource profile, by design:
//! - **Memory**: O(one stream chunk); objects stream in and the ZIP
//!   writer appends to disk, same as [`stream_archive`]'s approach.
//! - **Disk**: at most [`MAX_CHUNK_BYTES`] (plus ZIP overhead) of temp
//!   file at a time — chunks build strictly one after another. The temp
//!   file is unlinked at creation, so a crash can't leak it.
//! - **CPU**: one core worst case, and mostly far less — already-
//!   compressed formats (PDF, images, docx/xlsx) are STOREd, not
//!   deflated, which makes the build effectively network-bound.
//! - **DB/network**: one job at a time per app instance, claimed off a
//!   status column so a restart never loses or double-runs work.
//!
//! [`stream_archive`]: crate::controllers::documents

// RecordId-keyed maps (docs per transaction, owner per transaction) —
// interior mutability of lazy regex caches trips the lint; Hash + Eq
// stay deterministic. Same rationale as `controllers/transactions.rs`.
#![allow(clippy::mutable_key_type)]

use std::collections::HashMap;
use std::time::Duration;

use chrono::Datelike;
use surrealdb::types::{RecordId, SurrealValue};

use crate::controllers::documents::{
    export_property_folder, load_docs_for_transactions, manifest_field, sanitize_path_segment,
    unique_entry_path, unique_folder, zip_safe_filename,
};
use crate::db::record_key;
use crate::events::Event;
use crate::models::{ExportJob, NewExportChunk, Transaction};
use crate::state::AppState;

/// Size cap for one chunk's *uncompressed* content. 4 GiB keeps every
/// chunk ZIP inside classic (non-Zip64) offsets for maximum extractor
/// compatibility, bounds worker temp-disk to the same figure, and stays
/// far under the ~78 GiB ceiling of `rust-s3`'s streaming multipart
/// upload (10,000 parts × its fixed 8 MiB part size).
///
/// A single transaction whose documents alone exceed the cap still
/// becomes one (oversized) chunk — transactions are never split across
/// archives, because half a deal is useless to an auditor.
pub(crate) const MAX_CHUNK_BYTES: i64 = 4 * 1024 * 1024 * 1024;

/// How long finished artifacts stay downloadable. Mirrors the `7d` in
/// the `expires_at` updates below and the copy on the Exports page —
/// keep the three in sync.
pub(crate) const RETENTION_LABEL: &str = "7 days";

/// Idle delay between queue polls. A single indexed SELECT every few
/// seconds — same order of cost as the 45 s DB heartbeat in `main`.
const POLL_INTERVAL: Duration = Duration::from_secs(5);

/// Re-check for a user cancel every N documents inside a chunk build,
/// so a multi-GB chunk doesn't have to finish before a cancel lands.
const CANCEL_CHECK_EVERY_DOCS: usize = 50;

/// Placeholder body for objects that vanished from storage — one broken
/// object must not sink a compliance archive.
const MISSING: &[u8] = b"[file missing from storage]";

// ---------------------------------------------------------------------------
// Worker loop
// ---------------------------------------------------------------------------

/// Start the background worker. Called once from `main` after
/// [`AppState`] is built; never called in tests, so test DBs keep jobs
/// in `queued` deterministically.
pub fn spawn(state: AppState) {
    tokio::spawn(run(state));
}

async fn run(state: AppState) {
    requeue_orphans(&state).await;

    // Retention sweep every ~12 polls (≈ 1 minute idle) — artifacts
    // live for days, so second-level precision buys nothing.
    let mut ticks_until_sweep = 0u32;
    loop {
        if ticks_until_sweep == 0 {
            sweep_expired_exports(&state).await;
            ticks_until_sweep = 12;
        }
        ticks_until_sweep -= 1;

        match claim_next(&state).await {
            Ok(Some(job)) => process_job(&state, job).await,
            Ok(None) => tokio::time::sleep(POLL_INTERVAL).await,
            Err(e) => {
                tracing::warn!(error = %e, "export worker: claim query failed");
                tokio::time::sleep(Duration::from_secs(10)).await;
            }
        }
    }
}

/// Put any job a previous process died while running back in the queue.
/// Chunk rows and objects from the interrupted attempt are cleaned up
/// at the start of the re-run (`build_job` hygiene), so a rebuild is
/// safe and complete.
async fn requeue_orphans(state: &AppState) {
    match state
        .db
        .query(
            "UPDATE export_job SET status = 'queued', started_at = NONE \
             WHERE status = 'running' RETURN AFTER",
        )
        .await
        .and_then(|mut r| r.take::<Vec<ExportJob>>(0))
    {
        Ok(orphans) if !orphans.is_empty() => {
            tracing::warn!(count = orphans.len(), "requeued interrupted export jobs");
        }
        Ok(_) => {}
        Err(e) => tracing::warn!(error = %e, "export worker: orphan requeue failed"),
    }
}

/// Claim the oldest queued job. The `WHERE status = 'queued'` guard on
/// the UPDATE makes the claim atomic per record — a cancel (or another
/// instance) racing in between simply wins, and we get `None` back.
async fn claim_next(state: &AppState) -> Result<Option<ExportJob>, surrealdb::Error> {
    let mut r = state
        .db
        .query(
            "SELECT * FROM export_job WHERE status = 'queued' \
             ORDER BY created_at ASC LIMIT 1",
        )
        .await?;
    let candidates: Vec<ExportJob> = r.take(0)?;
    let Some(job) = candidates.into_iter().next() else {
        return Ok(None);
    };
    let mut u = state
        .db
        .query(
            "UPDATE $j SET status = 'running', started_at = time::now() \
             WHERE status = 'queued' RETURN AFTER",
        )
        .bind(("j", job.id))
        .await?;
    let claimed: Vec<ExportJob> = u.take(0)?;
    Ok(claimed.into_iter().next())
}

/// Outcome of one job build, for `process_job` to record.
enum BuildOutcome {
    Completed { chunks: usize, zip_bytes: u64 },
    Canceled,
}

async fn process_job(state: &AppState, job: ExportJob) {
    let job_key = record_key(&job.id);
    tracing::info!(
        job = %job_key,
        brokerage = %record_key(&job.brokerage),
        "export job started"
    );
    state
        .events
        .publish(Event::BrokerageMutation(job.brokerage.clone()));

    match build_job(state, &job).await {
        Ok(BuildOutcome::Completed { chunks, zip_bytes }) => {
            mark_completed(state, &job).await;
            tracing::info!(job = %job_key, chunks, zip_bytes, "export job completed");
            notify_requester(state, &job, chunks, zip_bytes).await;
        }
        Ok(BuildOutcome::Canceled) => {
            finalize_cancel(state, &job).await;
            tracing::info!(job = %job_key, "export job canceled");
        }
        Err(e) => {
            tracing::error!(job = %job_key, error = %crate::error::error_chain(e.as_ref()), "export job failed");
            mark_failed(state, &job).await;
        }
    }

    state
        .events
        .publish(Event::BrokerageMutation(job.brokerage.clone()));
}

// ---------------------------------------------------------------------------
// Job build
// ---------------------------------------------------------------------------

async fn build_job(state: &AppState, job: &ExportJob) -> anyhow::Result<BuildOutcome> {
    // Hygiene for re-runs (worker restart mid-job): drop chunk rows and
    // objects from the interrupted attempt so counts can't double up.
    purge_job_artifacts(state, job).await?;

    let brokerage_name = load_brokerage_name(state, &job.brokerage).await;
    let units = load_plan_units(state, job).await?;
    let chunks = plan_chunks(units, MAX_CHUNK_BYTES);

    state
        .db
        .query("UPDATE $j SET chunk_total = $n")
        .bind(("j", job.id.clone()))
        .bind(("n", chunks.len() as i64))
        .await?;

    let prefix = job_prefix(&job.brokerage, &job.id);
    let mut total_zip_bytes: u64 = 0;

    for (i, chunk) in chunks.iter().enumerate() {
        if job_canceled(state, &job.id).await {
            purge_job_artifacts(state, job).await?;
            return Ok(BuildOutcome::Canceled);
        }

        let seq = (i + 1) as i64;
        let key = format!("{prefix}{seq:03}-{stem}.zip", stem = chunk.file_stem);

        let Some(file) = build_chunk_zip(state, &job.id, chunk, &brokerage_name).await? else {
            // Canceled mid-chunk.
            purge_job_artifacts(state, job).await?;
            return Ok(BuildOutcome::Canceled);
        };

        let mut reader = tokio::fs::File::from_std(file);
        let uploaded = state
            .storage
            .put_stream(&key, &mut reader, "application/zip")
            .await?;
        total_zip_bytes += uploaded;

        let new_chunk = NewExportChunk {
            job: job.id.clone(),
            seq,
            label: chunk.label.clone(),
            filename: format!("transactvault-{}.zip", chunk.file_stem),
            storage_key: key,
            size_bytes: uploaded as i64,
            content_bytes: chunk.content_bytes,
            doc_count: chunk.doc_count as i64,
            tx_count: chunk.txs.len() as i64,
        };
        let _: Option<crate::models::ExportChunk> =
            state.db.create("export_chunk").content(new_chunk).await?;

        state
            .db
            .query("UPDATE $j SET chunks_done += 1, total_bytes += $b")
            .bind(("j", job.id.clone()))
            .bind(("b", uploaded as i64))
            .await?;
        state
            .events
            .publish(Event::BrokerageMutation(job.brokerage.clone()));

        tracing::info!(
            job = %record_key(&job.id), seq, of = chunks.len(), zip_bytes = uploaded,
            label = %chunk.label, "export chunk uploaded"
        );
    }

    Ok(BuildOutcome::Completed {
        chunks: chunks.len(),
        zip_bytes: total_zip_bytes,
    })
}

/// Assemble one chunk's ZIP into an unlinked temp file, streaming each
/// object from storage. Returns `None` when the job was canceled while
/// building. Mirrors `documents::stream_archive` — objects stream in,
/// the writer appends to disk, and every stream chunk is a yield point.
async fn build_chunk_zip(
    state: &AppState,
    job_id: &RecordId,
    chunk: &PlannedChunk,
    brokerage_name: &str,
) -> anyhow::Result<Option<std::fs::File>> {
    use futures::StreamExt;
    use std::io::{Seek, Write};
    use zip::write::SimpleFileOptions;

    let file = tokio::task::spawn_blocking(tempfile::tempfile).await??;
    let mut writer = zip::ZipWriter::new(file);
    let manifest_options =
        SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);

    writer.start_file("MANIFEST.txt", manifest_options)?;
    writer.write_all(chunk_manifest(chunk, brokerage_name).as_bytes())?;

    // Seeded with the manifest so no document can shadow it.
    let mut used_paths: std::collections::HashSet<String> =
        std::iter::once("MANIFEST.txt".to_string()).collect();
    let mut used_folders: Vec<String> = Vec::new();
    let mut docs_written = 0usize;

    for tx in &chunk.txs {
        let folder = unique_folder(&tx.folder, &mut used_folders);
        for doc in &tx.docs {
            if docs_written > 0
                && docs_written.is_multiple_of(CANCEL_CHECK_EVERY_DOCS)
                && job_canceled(state, job_id).await
            {
                return Ok(None);
            }
            docs_written += 1;

            let method = if doc.deflate {
                zip::CompressionMethod::Deflated
            } else {
                // Already-compressed formats: STORE saves a core's worth
                // of deflate for ~0% size difference.
                zip::CompressionMethod::Stored
            };
            let options = SimpleFileOptions::default()
                .compression_method(method)
                // Upload cap is 100 MB today, but `size_bytes` is data —
                // never assume it fits 32-bit offsets.
                .large_file(doc.size_bytes.max(0) as u64 >= u32::MAX as u64);

            let path = format!("{folder}/{}/{}", doc.form_segment, doc.zip_name);
            let entry = unique_entry_path(&path, &mut used_paths);
            writer.start_file(entry, options)?;

            match state.storage.get_stream(&doc.storage_key).await {
                Ok(Some(mut stream)) => {
                    while let Some(part) = stream.bytes.next().await {
                        let part =
                            part.map_err(|e| anyhow::anyhow!("stream {}: {e}", doc.storage_key))?;
                        writer.write_all(&part)?;
                    }
                }
                Ok(None) => writer.write_all(MISSING)?,
                Err(e) => {
                    tracing::warn!(error = %e, key = %doc.storage_key, "export: object stream failed");
                    writer.write_all(MISSING)?;
                }
            }
        }
    }

    let mut file = writer.finish()?;
    file.rewind()?;
    Ok(Some(file))
}

fn chunk_manifest(chunk: &PlannedChunk, brokerage_name: &str) -> String {
    let mut m = format!(
        "TransactVault brokerage export\n\
         Brokerage: {}\n\
         Agent: {}\n\
         Period: {}\n\
         Generated: {}\n\
         Transactions: {}\n\
         Documents: {}\n\n",
        manifest_field(brokerage_name),
        manifest_field(&chunk.agent),
        chunk.period_line(),
        chrono::Utc::now().to_rfc3339(),
        chunk.txs.len(),
        chunk.doc_count,
    );
    // Folder names here mirror `build_chunk_zip` exactly: same
    // `unique_folder` walk over the same ordering.
    let mut used_folders: Vec<String> = Vec::new();
    for tx in &chunk.txs {
        let folder = unique_folder(&tx.folder, &mut used_folders);
        m.push_str(&format!(
            "{folder}/ — {} ({} · {} document(s))\n",
            manifest_field(&tx.address),
            tx.status,
            tx.docs.len(),
        ));
    }
    m
}

// ---------------------------------------------------------------------------
// Status transitions + notifications
// ---------------------------------------------------------------------------

async fn mark_completed(state: &AppState, job: &ExportJob) {
    // The WHERE guards against a cancel landing in the instant after
    // the last chunk uploaded: cancel wins, and the artifacts it left
    // behind are purged here instead of lingering until the sweep.
    let done: Vec<ExportJob> = match state
        .db
        .query(
            "UPDATE $j SET status = 'completed', finished_at = time::now(), \
             expires_at = time::now() + 7d WHERE status = 'running' RETURN AFTER",
        )
        .bind(("j", job.id.clone()))
        .await
        .and_then(|mut r| r.take(0))
    {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(job = %record_key(&job.id), error = %e, "could not mark export completed");
            return;
        }
    };
    if done.is_empty() {
        if let Err(e) = purge_job_artifacts(state, job).await {
            tracing::warn!(job = %record_key(&job.id), error = %e, "purge after late cancel failed");
        }
        finalize_cancel(state, job).await;
    }
}

async fn mark_failed(state: &AppState, job: &ExportJob) {
    // User-safe summary only; the real chain is already in the log.
    if let Err(e) = state
        .db
        .query(
            "UPDATE $j SET status = 'failed', \
             error = 'The export hit an internal error and stopped. Finished archives below are still downloadable — delete this export and start a new one to retry.', \
             finished_at = time::now(), expires_at = time::now() + 7d \
             WHERE status = 'running'",
        )
        .bind(("j", job.id.clone()))
        .await
    {
        tracing::warn!(job = %record_key(&job.id), error = %e, "could not mark export failed");
    }
}

async fn finalize_cancel(state: &AppState, job: &ExportJob) {
    if let Err(e) = state
        .db
        .query(
            "UPDATE $j SET finished_at = time::now(), expires_at = time::now() + 7d \
             WHERE status = 'canceled'",
        )
        .bind(("j", job.id.clone()))
        .await
    {
        tracing::warn!(job = %record_key(&job.id), error = %e, "could not finalize canceled export");
    }
}

/// Email the broker who queued the job — builds can run for hours, and
/// nobody keeps a tab open that long.
async fn notify_requester(state: &AppState, job: &ExportJob, chunks: usize, zip_bytes: u64) {
    #[derive(serde::Deserialize, SurrealValue)]
    struct Requester {
        name: String,
        email: String,
    }
    let requester: Option<Requester> = match state
        .db
        .query("SELECT name, email FROM ONLY $u")
        .bind(("u", job.requested_by.clone()))
        .await
        .and_then(|mut r| r.take(0))
    {
        Ok(row) => row,
        Err(e) => {
            tracing::warn!(job = %record_key(&job.id), error = %e, "export ready email: requester lookup failed");
            None
        }
    };
    let Some(requester) = requester else { return };
    let link = format!(
        "{}/app/exports",
        state.config.base_url.trim_end_matches('/')
    );
    state
        .mailer
        .send_export_ready(
            &requester.email,
            &requester.name,
            &link,
            chunks,
            &humansize::format_size(zip_bytes, humansize::DECIMAL),
        )
        .await;
}

// ---------------------------------------------------------------------------
// Shared helpers (also used by the exports controller)
// ---------------------------------------------------------------------------

/// Object-key prefix for one job's artifacts — always ends in `/` so
/// `delete_prefix` can't catch sibling jobs.
pub(crate) fn job_prefix(brokerage: &RecordId, job: &RecordId) -> String {
    format!("exports/{}/{}/", record_key(brokerage), record_key(job))
}

/// Delete a job's chunk objects and rows (not the job row itself).
/// Objects first: if that fails we keep the rows so a later pass can
/// retry, instead of orphaning objects forever.
pub(crate) async fn purge_job_artifacts(
    state: &AppState,
    job: &ExportJob,
) -> anyhow::Result<usize> {
    let deleted = state
        .storage
        .delete_prefix(&job_prefix(&job.brokerage, &job.id))
        .await?;
    state
        .db
        .query("DELETE export_chunk WHERE job = $j")
        .bind(("j", job.id.clone()))
        .await?;
    Ok(deleted)
}

/// Purge expired jobs (objects + chunk rows + the job row). Bounded per
/// call; the next sweep picks up the rest.
async fn sweep_expired_exports(state: &AppState) {
    let expired: Vec<ExportJob> = match state
        .db
        .query(
            "SELECT * FROM export_job \
             WHERE expires_at != NONE AND expires_at < time::now() \
               AND status IN ['completed', 'failed', 'canceled'] \
             LIMIT 2",
        )
        .await
        .and_then(|mut r| r.take(0))
    {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(error = %e, "export retention sweep query failed");
            return;
        }
    };
    for job in expired {
        match purge_job_artifacts(state, &job).await {
            Ok(objects) => {
                let key = record_key(&job.id);
                let _: Result<Option<ExportJob>, _> = state.db.delete(job.id.clone()).await;
                tracing::info!(job = %key, objects, "expired export purged");
            }
            Err(e) => {
                tracing::warn!(job = %record_key(&job.id), error = %e, "expired export purge failed; will retry");
            }
        }
    }
}

async fn job_canceled(state: &AppState, job: &RecordId) -> bool {
    let status: Option<String> = match state
        .db
        .query("SELECT VALUE status FROM ONLY $j")
        .bind(("j", job.clone()))
        .await
    {
        Ok(mut r) => r.take(0).ok().flatten(),
        // A transient DB error must not abort a multi-hour build —
        // keep going; a real outage will fail the next durable write.
        Err(_) => None,
    };
    status.as_deref() == Some("canceled")
}

async fn load_brokerage_name(state: &AppState, brokerage: &RecordId) -> String {
    #[derive(serde::Deserialize, SurrealValue)]
    struct Meta {
        name: String,
    }
    let meta: Option<Meta> = match state
        .db
        .query("SELECT name FROM ONLY $b")
        .bind(("b", brokerage.clone()))
        .await
    {
        Ok(mut r) => r.take(0).unwrap_or(None),
        Err(_) => None,
    };
    meta.map(|m| m.name).unwrap_or_else(|| "brokerage".into())
}

// ---------------------------------------------------------------------------
// Chunk planning
// ---------------------------------------------------------------------------

/// One document as the planner sees it — ZIP path pieces pre-sanitized.
#[derive(Debug, Clone)]
pub(crate) struct PlanDoc {
    pub form_segment: String,
    pub zip_name: String,
    pub storage_key: String,
    pub size_bytes: i64,
    pub deflate: bool,
}

/// One transaction (with its documents) as a planning unit. Transactions
/// are atomic: the planner never splits one across chunks.
#[derive(Debug, Clone)]
pub(crate) struct PlanTx {
    /// Record key of the owning agent; empty string = unassigned.
    pub agent_key: String,
    /// Display name; the planner uniquifies duplicates across distinct
    /// keys ("Jane Smith", "Jane Smith (2)").
    pub agent: String,
    pub year: i32,
    /// 1–12.
    pub month: u32,
    /// Property folder segment (already sanitized).
    pub folder: String,
    pub address: String,
    pub status: String,
    pub docs: Vec<PlanDoc>,
}

impl PlanTx {
    fn bytes(&self) -> i64 {
        self.docs.iter().map(|d| d.size_bytes.max(0)).sum()
    }
}

/// One planned chunk ZIP.
#[derive(Debug)]
pub(crate) struct PlannedChunk {
    pub agent: String,
    /// `"2025"` for a whole-year chunk, `"2025-03"` for a month chunk.
    pub period: String,
    /// 1-based part number within (agent, period).
    pub part: u32,
    /// Parts in this (agent, period) group — 1 means no split.
    pub parts: u32,
    pub label: String,
    /// Slug for the object key / download filename (no extension).
    pub file_stem: String,
    pub txs: Vec<PlanTx>,
    pub content_bytes: i64,
    pub doc_count: usize,
}

impl PlannedChunk {
    fn period_line(&self) -> String {
        if self.parts > 1 {
            format!("{} (part {} of {})", self.period, self.part, self.parts)
        } else {
            self.period.clone()
        }
    }
}

/// Group transactions into chunk plans: agent + year, split to months
/// when a year exceeds `cap`, split months into size parts when a month
/// still exceeds it. Ordering is stable: agents alphabetically with
/// Unassigned last, then chronologically.
pub(crate) fn plan_chunks(mut units: Vec<PlanTx>, cap: i64) -> Vec<PlannedChunk> {
    uniquify_agent_names(&mut units);

    units.sort_by(|a, b| {
        let a_unassigned = a.agent_key.is_empty();
        let b_unassigned = b.agent_key.is_empty();
        a_unassigned
            .cmp(&b_unassigned)
            .then_with(|| a.agent.to_lowercase().cmp(&b.agent.to_lowercase()))
            .then_with(|| a.year.cmp(&b.year))
            .then_with(|| a.month.cmp(&b.month))
            .then_with(|| a.address.cmp(&b.address))
    });

    let mut chunks: Vec<PlannedChunk> = Vec::new();
    for year_group in group_consecutive(units, |t| (t.agent_key.clone(), t.year)) {
        let total: i64 = year_group.iter().map(PlanTx::bytes).sum();
        let year = year_group[0].year;
        if total <= cap {
            push_parts(&mut chunks, year.to_string(), vec![year_group]);
            continue;
        }
        // Year too big → month granularity, each month split by size.
        for month_group in group_consecutive(year_group, |t| t.month) {
            let month = month_group[0].month;
            let parts = split_by_size(month_group, cap);
            push_parts(&mut chunks, format!("{year}-{month:02}"), parts);
        }
    }
    chunks
}

/// Emit one `PlannedChunk` per part, wiring part numbering and labels.
fn push_parts(chunks: &mut Vec<PlannedChunk>, period: String, parts: Vec<Vec<PlanTx>>) {
    let total_parts = parts.len() as u32;
    for (i, txs) in parts.into_iter().enumerate() {
        let part = i as u32 + 1;
        let agent = txs[0].agent.clone();
        let label = if total_parts > 1 {
            format!("{agent} — {period} (part {part} of {total_parts})")
        } else {
            format!("{agent} — {period}")
        };
        let mut file_stem = format!("{}-{period}", slug(&agent));
        if total_parts > 1 {
            file_stem.push_str(&format!("-part{part}"));
        }
        let content_bytes = txs.iter().map(PlanTx::bytes).sum();
        let doc_count = txs.iter().map(|t| t.docs.len()).sum();
        chunks.push(PlannedChunk {
            agent,
            period: period.clone(),
            part,
            parts: total_parts,
            label,
            file_stem,
            txs,
            content_bytes,
            doc_count,
        });
    }
}

/// Split a run of transactions into consecutive parts that each stay
/// under `cap`, never splitting one transaction. A single transaction
/// over the cap gets a part of its own.
fn split_by_size(txs: Vec<PlanTx>, cap: i64) -> Vec<Vec<PlanTx>> {
    let mut parts: Vec<Vec<PlanTx>> = Vec::new();
    let mut current: Vec<PlanTx> = Vec::new();
    let mut current_bytes: i64 = 0;
    for tx in txs {
        let bytes = tx.bytes();
        if !current.is_empty() && current_bytes + bytes > cap {
            parts.push(std::mem::take(&mut current));
            current_bytes = 0;
        }
        current_bytes += bytes;
        current.push(tx);
    }
    if !current.is_empty() {
        parts.push(current);
    }
    parts
}

/// Group consecutive items sharing a key — callers sort first.
fn group_consecutive<T, K: PartialEq>(items: Vec<T>, key: impl Fn(&T) -> K) -> Vec<Vec<T>> {
    let mut groups: Vec<Vec<T>> = Vec::new();
    for item in items {
        match groups.last_mut() {
            Some(last) if key(&last[0]) == key(&item) => last.push(item),
            _ => groups.push(vec![item]),
        }
    }
    groups
}

/// Two different agents named "Jane Smith" must not merge into one
/// archive series — suffix later keys with " (2)", " (3)", …
fn uniquify_agent_names(units: &mut [PlanTx]) {
    let mut by_key: HashMap<String, String> = HashMap::new(); // key → final name
    let mut taken: Vec<String> = Vec::new();
    for unit in units.iter_mut() {
        let name = match by_key.get(&unit.agent_key) {
            Some(name) => name.clone(),
            None => {
                let mut candidate = unit.agent.clone();
                let mut n = 1;
                while taken.iter().any(|t| t.eq_ignore_ascii_case(&candidate)) {
                    n += 1;
                    candidate = format!("{} ({n})", unit.agent);
                }
                taken.push(candidate.clone());
                by_key.insert(unit.agent_key.clone(), candidate.clone());
                candidate
            }
        };
        unit.agent = name;
    }
}

/// Filename-safe slug, mirroring `documents::zip_response`.
fn slug(s: &str) -> String {
    let out: String = s
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect::<String>()
        .trim_matches('_')
        .to_string();
    if out.is_empty() { "agent".into() } else { out }
}

/// Deflate only formats that actually shrink. PDFs, images and the
/// OOXML formats are already compressed — STORE them and save the CPU.
pub(crate) fn should_deflate(content_type: &str) -> bool {
    let ct = content_type.to_ascii_lowercase();
    ct.starts_with("text/")
        || matches!(
            ct.as_str(),
            "application/rtf" | "application/msword" | "application/vnd.ms-excel"
        )
}

/// Load every planning unit for a job's brokerage: transactions with at
/// least one document, joined with their owner's name.
async fn load_plan_units(state: &AppState, job: &ExportJob) -> anyhow::Result<Vec<PlanTx>> {
    let mut tq = state
        .db
        .query("SELECT * FROM $b->has_transaction->transaction")
        .bind(("b", job.brokerage.clone()))
        .await?;
    let txs: Vec<Transaction> = tq.take(0).unwrap_or_default();
    let tx_ids: Vec<RecordId> = txs.iter().map(|t| t.id.clone()).collect();
    if tx_ids.is_empty() {
        return Ok(Vec::new());
    }

    #[derive(serde::Deserialize, SurrealValue)]
    struct OwnsEdge {
        tx: RecordId,
        owner: RecordId,
    }
    let mut oq = state
        .db
        .query("SELECT out AS tx, in AS owner FROM owns WHERE out IN $ids")
        .bind(("ids", tx_ids))
        .await?;
    let edges: Vec<OwnsEdge> = oq.take(0).unwrap_or_default();
    let owner_by_tx: HashMap<RecordId, RecordId> =
        edges.into_iter().map(|e| (e.tx, e.owner)).collect();

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
    let names: HashMap<RecordId, String> = if owner_ids.is_empty() {
        HashMap::new()
    } else {
        let mut nq = state
            .db
            .query("SELECT id, name FROM user WHERE id IN $ids")
            .bind(("ids", owner_ids))
            .await?;
        let rows: Vec<UserRow> = nq.take(0).unwrap_or_default();
        rows.into_iter().map(|r| (r.id, r.name)).collect()
    };

    let docs_by_tx = load_docs_for_transactions(state, &txs)
        .await
        .map_err(|e| anyhow::anyhow!("load documents: {e}"))?;

    let mut units: Vec<PlanTx> = Vec::new();
    for tx in &txs {
        let docs = docs_by_tx.get(&tx.id).cloned().unwrap_or_default();
        if docs.is_empty() {
            continue; // nothing to archive for this transaction
        }
        let (agent_key, agent) = match owner_by_tx.get(&tx.id) {
            Some(owner) => (
                record_key(owner),
                names
                    .get(owner)
                    .cloned()
                    .unwrap_or_else(|| "Unassigned".into()),
            ),
            None => (String::new(), "Unassigned".into()),
        };
        units.push(PlanTx {
            agent_key,
            agent,
            // The transaction's creation date decides its chunk — the
            // model has no close date, and created_at is stable.
            year: tx.created_at.year(),
            month: tx.created_at.month(),
            folder: export_property_folder(tx),
            address: tx.property_address.clone(),
            status: tx.status.clone(),
            docs: docs
                .iter()
                .map(|d| PlanDoc {
                    form_segment: sanitize_path_segment(&d.form_code),
                    zip_name: zip_safe_filename(d),
                    storage_key: d.storage_key.clone(),
                    size_bytes: d.size_bytes,
                    deflate: should_deflate(&d.content_type),
                })
                .collect(),
        });
    }
    Ok(units)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(agent_key: &str, agent: &str, year: i32, month: u32, addr: &str, mb: i64) -> PlanTx {
        PlanTx {
            agent_key: agent_key.into(),
            agent: agent.into(),
            year,
            month,
            folder: addr.replace(' ', "_"),
            address: addr.into(),
            status: "active".into(),
            docs: vec![PlanDoc {
                form_segment: "RPA".into(),
                zip_name: "contract.pdf".into(),
                storage_key: format!("k/{addr}"),
                size_bytes: mb * 1024 * 1024,
                deflate: false,
            }],
        }
    }

    const MB: i64 = 1024 * 1024;

    #[test]
    fn groups_by_agent_and_year_under_cap() {
        let chunks = plan_chunks(
            vec![
                unit("u2", "Zoe", 2025, 1, "9 Elm St", 10),
                unit("u1", "Al", 2024, 5, "1 Oak St", 10),
                unit("u1", "Al", 2024, 7, "2 Oak St", 10),
                unit("u1", "Al", 2025, 2, "3 Oak St", 10),
                unit("", "Unassigned", 2020, 1, "5 Pine St", 10),
            ],
            100 * MB,
        );
        let labels: Vec<&str> = chunks.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(
            labels,
            vec!["Al — 2024", "Al — 2025", "Zoe — 2025", "Unassigned — 2020",]
        );
        assert_eq!(chunks[0].txs.len(), 2);
        assert!(chunks.iter().all(|c| c.parts == 1));
    }

    #[test]
    fn oversized_year_splits_by_month() {
        let chunks = plan_chunks(
            vec![
                unit("u1", "Al", 2025, 1, "1 Oak St", 60),
                unit("u1", "Al", 2025, 2, "2 Oak St", 60),
            ],
            100 * MB,
        );
        let labels: Vec<&str> = chunks.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(labels, vec!["Al — 2025-01", "Al — 2025-02"]);
    }

    #[test]
    fn oversized_month_splits_into_parts_on_tx_boundaries() {
        let chunks = plan_chunks(
            vec![
                unit("u1", "Al", 2025, 3, "1 Oak St", 60),
                unit("u1", "Al", 2025, 3, "2 Oak St", 60),
                unit("u1", "Al", 2025, 3, "3 Oak St", 60),
            ],
            100 * MB,
        );
        let labels: Vec<&str> = chunks.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(
            labels,
            vec![
                "Al — 2025-03 (part 1 of 3)",
                "Al — 2025-03 (part 2 of 3)",
                "Al — 2025-03 (part 3 of 3)",
            ]
        );
        // Never split a transaction: each part carries whole ones.
        assert!(chunks.iter().all(|c| c.txs.len() == 1));
        assert_eq!(chunks[0].file_stem, "Al-2025-03-part1");
    }

    #[test]
    fn single_transaction_over_cap_gets_its_own_oversized_chunk() {
        let chunks = plan_chunks(
            vec![
                unit("u1", "Al", 2025, 3, "1 Oak St", 250),
                unit("u1", "Al", 2025, 3, "2 Oak St", 10),
            ],
            100 * MB,
        );
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].content_bytes, 250 * MB);
        assert_eq!(chunks[0].txs.len(), 1);
    }

    #[test]
    fn duplicate_agent_names_stay_separate() {
        let chunks = plan_chunks(
            vec![
                unit("u1", "Jane Smith", 2025, 1, "1 Oak St", 10),
                unit("u2", "Jane Smith", 2025, 1, "2 Elm St", 10),
            ],
            100 * MB,
        );
        assert_eq!(chunks.len(), 2);
        let mut agents: Vec<&str> = chunks.iter().map(|c| c.agent.as_str()).collect();
        agents.sort();
        assert_eq!(agents, vec!["Jane Smith", "Jane Smith (2)"]);
    }

    #[test]
    fn deflate_only_for_compressible_types() {
        assert!(should_deflate("text/plain"));
        assert!(should_deflate("text/csv"));
        assert!(should_deflate("application/msword"));
        assert!(!should_deflate("application/pdf"));
        assert!(!should_deflate("image/jpeg"));
        assert!(!should_deflate(
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
        ));
    }

    #[test]
    fn empty_plan_produces_no_chunks() {
        assert!(plan_chunks(Vec::new(), 100 * MB).is_empty());
    }
}
