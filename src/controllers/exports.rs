//! Export center: queue, watch, and download background brokerage
//! archives. Broker-only throughout.
//!
//! The heavy lifting happens in [`crate::export_worker`]; these
//! handlers only write small rows and hand out presigned URLs, so every
//! one of them fits comfortably inside the page timeout. Downloads
//! never proxy bytes through the app: the chunk endpoint 303-redirects
//! to a short-lived presigned GET and the object store serves the
//! transfer itself, Range requests included — that is what makes a
//! multi-GB download resumable.

use std::convert::Infallible;
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::{StatusCode, header};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{Html, Redirect, Response};
use futures::stream::Stream;
use surrealdb::types::RecordId;

use crate::auth::CurrentUser;
use crate::controllers::common::patch_elements_event;
use crate::controllers::{render, render_str};
use crate::error::AppError;
use crate::events::Event;
use crate::export_worker;
use crate::models::{ExportChunk, ExportJob, NewExportJob};
use crate::state::AppState;
use crate::templates::{ExportJobView, ExportJobsFragment, ExportsPage};

/// Presigned-GET lifetime for a single click-through download. Checked
/// at request start only, so a slow transfer that began in time keeps
/// running — same semantics as the upload presign.
const DOWNLOAD_PRESIGN_SECS: u32 = 10 * 60;

/// Presigned-GET lifetime for the `urls.txt` batch list. Longer than a
/// click-through because a sequential batch downloader reaches the last
/// URL long after the first; still short enough that a leaked file goes
/// stale the same afternoon.
const URLS_TXT_PRESIGN_SECS: u32 = 60 * 60;

/// How many recent jobs the page shows. Old jobs age out via the
/// worker's retention sweep anyway; this just bounds the render.
const JOBS_SHOWN: usize = 10;

/// `GET /app/exports` — the export center page.
pub async fn page(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Html<String>, AppError> {
    if !user.role.is_broker() {
        return Err(AppError::Forbidden);
    }
    let jobs = load_job_views(&state, &user.brokerage_id).await?;
    let header = crate::controllers::common::build_app_header(&state, &user, "team").await;
    render(&ExportsPage {
        app_name: &state.config.app_name,
        base_url: &state.config.base_url,
        signed_in: true,
        header,
        jobs,
        retention_label: export_worker::RETENTION_LABEL,
    })
}

/// `POST /app/exports` — queue a new background export.
///
/// One active job per brokerage: if one is already queued or running,
/// this quietly lands back on the page showing it — clicking Start
/// twice should never build the archive twice.
pub async fn create(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Redirect, AppError> {
    if !user.role.is_broker() {
        return Err(AppError::Forbidden);
    }
    let rl_key = format!("exportjob:{}", crate::db::record_key(&user.user_id));
    if !crate::security::allow_per_hour(&state.rate_limiter, &rl_key, 4) {
        return Err(AppError::invalid(
            "You've queued several exports in a short time. Give it a few minutes and try again.",
        ));
    }

    let mut aq = state
        .db
        .query(
            "SELECT VALUE id FROM export_job \
             WHERE brokerage = $b AND status IN ['queued', 'running'] LIMIT 1",
        )
        .bind(("b", user.brokerage_id.clone()))
        .await?;
    let active: Vec<RecordId> = aq.take(0).unwrap_or_default();
    if active.is_empty() {
        let _: Option<ExportJob> = state
            .db
            .create("export_job")
            .content(NewExportJob {
                brokerage: user.brokerage_id.clone(),
                requested_by: user.user_id.clone(),
            })
            .await?;
        crate::audit::record(
            &state.db,
            "export_requested",
            Some(user.user_id.clone()),
            Some(user.email.clone()),
            None,
            None,
            Some("brokerage-wide background export queued".into()),
        )
        .await;
        state
            .events
            .publish(Event::BrokerageMutation(user.brokerage_id.clone()));
    }
    Ok(Redirect::to("/app/exports"))
}

/// `GET /app/exports/stream` — SSE stream that re-renders the jobs
/// section whenever something in the brokerage changes (the worker
/// publishes after every chunk). Same lifecycle and re-authorization
/// discipline as `transactions::stats_stream`, plus a broker-role gate:
/// a demoted broker's open stream closes on the first event after the
/// change.
pub async fn stream(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Sse<impl Stream<Item = Result<SseEvent, Infallible>>>, AppError> {
    if !user.role.is_broker() {
        return Err(AppError::Forbidden);
    }
    let user_id = user.user_id.clone();
    let brokerage = user.brokerage_id.clone();
    let role = user.role;
    let mut rx = state.events.subscribe();

    let stream = async_stream::stream! {
        if let Ok(html) = render_jobs_html(&state, &brokerage).await {
            yield Ok(patch_elements_event(&html));
        }
        loop {
            use tokio::sync::broadcast::error::RecvError;
            let should_render = match rx.recv().await {
                Ok(Event::UserMembershipChanged(uid)) if uid == user_id => break,
                Ok(Event::BrokerageMutation(bid)) if bid == brokerage => {
                    // Re-verify membership + role before rendering —
                    // this fragment is broker-only.
                    match crate::controllers::transactions::current_membership(&state.db, &user_id).await {
                        Some((bid_now, role_now))
                            if bid_now == brokerage && role_now == role && role_now.is_broker() => true,
                        _ => break,
                    }
                }
                Ok(_) => false,
                Err(RecvError::Lagged(_)) => true,
                Err(RecvError::Closed) => break,
            };
            if should_render && let Ok(html) = render_jobs_html(&state, &brokerage).await {
                yield Ok(patch_elements_event(&html));
            }
        }
    };
    Ok(Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15))))
}

/// `GET /app/exports/{job}/chunks/{chunk}/download` — 303 to a
/// presigned GET so the store serves the bytes (resumable, no app
/// bandwidth). The chunk must belong to the job in the path and the
/// job to the caller's brokerage.
pub async fn download_chunk(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((job_key, chunk_key)): Path<(String, String)>,
) -> Result<Redirect, AppError> {
    if !user.role.is_broker() {
        return Err(AppError::Forbidden);
    }
    allow_download(&state, &user)?;
    let (job, chunk) = authorize_chunk(&state, &user, &job_key, &chunk_key).await?;

    let url = state
        .storage
        .presign_get(
            &chunk.storage_key,
            DOWNLOAD_PRESIGN_SECS,
            Some(&chunk.filename),
        )
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("presign download: {e}")))?;

    crate::audit::record(
        &state.db,
        "export_downloaded",
        Some(user.user_id.clone()),
        Some(user.email.clone()),
        None,
        None,
        Some(format!(
            "chunk \"{}\" of job {}",
            chunk.label,
            job.url_key()
        )),
    )
    .await;
    Ok(Redirect::to(&url))
}

/// `GET /app/exports/{job}/urls.txt` — every chunk as a presigned URL,
/// one per line, for batch downloaders. `aria2c -c -j4 -i urls.txt`
/// gives parallel, resumable retrieval of the whole export without the
/// browser in the loop.
pub async fn urls_txt(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(job_key): Path<String>,
) -> Result<Response, AppError> {
    if !user.role.is_broker() {
        return Err(AppError::Forbidden);
    }
    allow_download(&state, &user)?;
    let job = authorize_job(&state, &user, &job_key).await?;
    let chunks = load_chunks(&state, &job.id).await?;
    if chunks.is_empty() {
        return Err(AppError::invalid(
            "This export has no finished archives yet — try again once the first one appears.",
        ));
    }

    let mut body = format!(
        "# TransactVault brokerage export — {} archive(s).\n\
         # Links are valid for 1 hour; reload this file for fresh ones.\n\
         # Batch download (parallel + resumable): aria2c -c -j4 -i urls.txt\n",
        chunks.len(),
    );
    for chunk in &chunks {
        let url = state
            .storage
            .presign_get(
                &chunk.storage_key,
                URLS_TXT_PRESIGN_SECS,
                Some(&chunk.filename),
            )
            .await
            .map_err(|e| AppError::Internal(anyhow::anyhow!("presign urls.txt: {e}")))?;
        body.push_str(&url);
        body.push('\n');
    }

    crate::audit::record(
        &state.db,
        "export_downloaded",
        Some(user.user_id.clone()),
        Some(user.email.clone()),
        None,
        None,
        Some(format!(
            "urls.txt ({} chunks) of job {}",
            chunks.len(),
            job.url_key()
        )),
    )
    .await;

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .header(
            header::CONTENT_DISPOSITION,
            "attachment; filename=\"urls.txt\"",
        )
        // Every line is a live, signature-bearing URL to a whole-brokerage
        // archive. Match what `documents::download` does rather than
        // leaving it to heuristic caching.
        .header(header::CACHE_CONTROL, "private, no-store")
        .body(body.into())
        .map_err(|e| AppError::Internal(anyhow::anyhow!("build urls.txt response: {e}")))
}

/// `POST /app/exports/{job}/cancel` — stop a queued or running job. The
/// worker notices between objects and purges whatever it had built.
pub async fn cancel(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(job_key): Path<String>,
) -> Result<Redirect, AppError> {
    if !user.role.is_broker() {
        return Err(AppError::Forbidden);
    }
    let job = authorize_job(&state, &user, &job_key).await?;
    let mut cq = state
        .db
        .query(
            "UPDATE $j SET status = 'canceled', finished_at = time::now(), \
             expires_at = time::now() + 7d \
             WHERE status IN ['queued', 'running'] RETURN AFTER",
        )
        .bind(("j", job.id.clone()))
        .await?;
    let canceled: Vec<ExportJob> = cq.take(0).unwrap_or_default();
    if !canceled.is_empty() {
        crate::audit::record(
            &state.db,
            "export_canceled",
            Some(user.user_id.clone()),
            Some(user.email.clone()),
            None,
            None,
            Some(format!("job {}", job.url_key())),
        )
        .await;
        state
            .events
            .publish(Event::BrokerageMutation(user.brokerage_id.clone()));
    }
    Ok(Redirect::to("/app/exports"))
}

/// `POST /app/exports/{job}/delete` — purge a finished job's archives
/// now instead of waiting out the retention window. Refused while the
/// worker still owns the job (cancel first).
pub async fn purge(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(job_key): Path<String>,
) -> Result<Redirect, AppError> {
    if !user.role.is_broker() {
        return Err(AppError::Forbidden);
    }
    let job = authorize_job(&state, &user, &job_key).await?;
    if job.status_enum().is_active() {
        return Err(AppError::invalid(
            "This export is still building — cancel it first, then delete it.",
        ));
    }
    export_worker::purge_job_artifacts(&state, &job)
        .await
        .map_err(AppError::Internal)?;
    let _: Result<Option<ExportJob>, _> = state.db.delete(job.id.clone()).await;
    crate::audit::record(
        &state.db,
        "export_deleted",
        Some(user.user_id.clone()),
        Some(user.email.clone()),
        None,
        None,
        Some(format!("job {}", job.url_key())),
    )
    .await;
    state
        .events
        .publish(Event::BrokerageMutation(user.brokerage_id.clone()));
    Ok(Redirect::to("/app/exports"))
}

// ---------------------------------------------------------------------------
// Lookups + shared bits
// ---------------------------------------------------------------------------

/// Presigns are cheap local crypto, but a runaway script hammering the
/// redirect endpoint still spends audit rows and DB reads — cap it
/// generously above any honest batch (hundreds of chunks).
fn allow_download(state: &AppState, user: &CurrentUser) -> Result<(), AppError> {
    let key = format!("exportdl:{}", crate::db::record_key(&user.user_id));
    if crate::security::allow_per_hour(&state.rate_limiter, &key, 300) {
        Ok(())
    } else {
        Err(AppError::invalid(
            "That's a lot of download requests in a short time. Give it a few minutes and try again.",
        ))
    }
}

/// Load a job by URL key and prove it belongs to the caller's
/// brokerage. `NotFound` on any mismatch — don't leak that a foreign
/// job id exists.
async fn authorize_job(
    state: &AppState,
    user: &CurrentUser,
    job_key: &str,
) -> Result<ExportJob, AppError> {
    let job_id = RecordId::new("export_job", job_key);
    let mut jq = state
        .db
        .query("SELECT * FROM ONLY $j")
        .bind(("j", job_id))
        .await?;
    let job: Option<ExportJob> = jq.take(0)?;
    let job = job.ok_or(AppError::NotFound)?;
    if job.brokerage != user.brokerage_id {
        return Err(AppError::NotFound);
    }
    Ok(job)
}

/// [`authorize_job`] plus the chunk, verifying the chunk really hangs
/// off the job named in the path.
async fn authorize_chunk(
    state: &AppState,
    user: &CurrentUser,
    job_key: &str,
    chunk_key: &str,
) -> Result<(ExportJob, ExportChunk), AppError> {
    let job = authorize_job(state, user, job_key).await?;
    let chunk_id = RecordId::new("export_chunk", chunk_key);
    let mut cq = state
        .db
        .query("SELECT * FROM ONLY $c")
        .bind(("c", chunk_id))
        .await?;
    let chunk: Option<ExportChunk> = cq.take(0)?;
    let chunk = chunk.ok_or(AppError::NotFound)?;
    if chunk.job != job.id {
        return Err(AppError::NotFound);
    }
    Ok((job, chunk))
}

async fn load_chunks(state: &AppState, job: &RecordId) -> Result<Vec<ExportChunk>, AppError> {
    let mut cq = state
        .db
        .query("SELECT * FROM export_chunk WHERE job = $j ORDER BY seq ASC")
        .bind(("j", job.clone()))
        .await?;
    Ok(cq.take(0).unwrap_or_default())
}

/// Recent jobs (with their chunks) as render-ready views.
async fn load_job_views(
    state: &AppState,
    brokerage: &RecordId,
) -> Result<Vec<ExportJobView>, AppError> {
    let mut jq = state
        .db
        .query(
            "SELECT * FROM export_job WHERE brokerage = $b \
             ORDER BY created_at DESC LIMIT $n",
        )
        .bind(("b", brokerage.clone()))
        .bind(("n", JOBS_SHOWN as i64))
        .await?;
    let jobs: Vec<ExportJob> = jq.take(0).unwrap_or_default();

    let mut views = Vec::with_capacity(jobs.len());
    for job in jobs {
        let chunks = load_chunks(state, &job.id).await?;
        views.push(ExportJobView::build(job, chunks));
    }
    Ok(views)
}

/// The `#exports-live` fragment as HTML, for the SSE patcher.
async fn render_jobs_html(state: &AppState, brokerage: &RecordId) -> Result<String, AppError> {
    let jobs = load_job_views(state, brokerage).await?;
    render_str(&ExportJobsFragment { jobs })
}
