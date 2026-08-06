//! Scheduled full-database backups.
//!
//! # Why a second database connection
//!
//! The SurrealDB v3 Rust SDK implements `export` / `import` on the HTTP
//! and embedded engines ONLY. Verified by reading the crate: the WS
//! engine (`src/engine/remote/ws/`) contains no `Command::Export*`
//! handling at all. Production connects over `ws://`, so calling
//! `state.db.export(...)` there would fail.
//!
//! SurrealDB serves HTTP and WebSocket on the same port, so
//! [`backup_endpoint`] rewrites the configured URL's scheme (`ws` →
//! `http`, `wss` → `https`) and every backup opens its own short-lived
//! client against it. That also keeps the app's long-lived `Arc<Surreal>`
//! session untouched, which matters here: in v3 each clone registers its
//! own server-side session, and sharing exactly one is what fixed the
//! intermittent production 500s.
//!
//! # What a backup contains
//!
//! `export(())` streams the database's own logical dump: every table
//! definition, index, field, and row in the selected namespace and
//! database. Deliberately not a table-by-table walk — a hand-maintained
//! list silently misses whatever table the next feature adds, which is
//! the failure mode that makes a backup worthless exactly when it is
//! needed. Anything SurrealDB can create, this captures.

use anyhow::Context;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use surrealdb::Surreal;
use surrealdb::engine::any::{self, Any};
use surrealdb::opt::auth::Root;
use surrealdb::types::{RecordId, SurrealValue};
use tokio::io::AsyncWriteExt;

use crate::state::AppState;

/// Flatten an `anyhow` chain onto one line.
///
/// `%e` on an `anyhow::Error` prints only the outermost context, which
/// is why the first production backup failure logged "connecting to
/// http://tv-surrealdb:8000 for backup" and not one word about why. The
/// cause is the entire value of the message.
pub fn describe(err: &anyhow::Error) -> String {
    let mut parts = vec![err.to_string()];
    let mut source = err.source();
    while let Some(cause) = source {
        parts.push(cause.to_string());
        source = cause.source();
    }
    parts.join(": ")
}

/// Object-storage prefix for backup artifacts.
const BACKUP_PREFIX: &str = "backups/";

/// How often the scheduler wakes to ask "is one due yet". Independent of
/// the configured interval: a short tick keeps a changed setting taking
/// effect promptly without restarting the app.
const TICK: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// One stored backup.
#[derive(Debug, Clone, Serialize, Deserialize, SurrealValue)]
pub struct Backup {
    pub id: RecordId,
    pub storage_key: String,
    pub filename: String,
    pub size_bytes: i64,
    pub kind: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, SurrealValue)]
struct NewBackup {
    storage_key: String,
    filename: String,
    size_bytes: i64,
    kind: String,
}

impl Backup {
    pub fn key(&self) -> String {
        crate::db::record_key(&self.id)
    }

    pub fn is_manual(&self) -> bool {
        self.kind == "manual"
    }
}

/// Rewrite the configured SurrealDB URL into one whose engine can
/// export. Embedded engines and HTTP are already fine; only the
/// WebSocket schemes need swapping.
pub fn backup_endpoint(surreal_url: &str) -> String {
    match surreal_url.split_once("://") {
        Some(("ws", rest)) => format!("http://{rest}"),
        Some(("wss", rest)) => format!("https://{rest}"),
        _ => surreal_url.to_string(),
    }
}

/// Connect a short-lived client suitable for export/import.
async fn connect_for_backup(state: &AppState) -> anyhow::Result<Surreal<Any>> {
    let url = backup_endpoint(&state.config.surreal_url);
    let db = any::connect(&url)
        .await
        .with_context(|| format!("connecting to {url} for backup"))?;

    let is_remote = matches!(
        url.split("://").next(),
        Some("ws" | "wss" | "http" | "https")
    );
    if is_remote {
        db.signin(Root {
            username: state.config.surreal_user.clone(),
            password: state.config.surreal_pass.clone(),
        })
        .await
        .context("signing in for backup")?;
    }
    db.use_ns(&state.config.surreal_ns)
        .use_db(&state.config.surreal_db)
        .await
        .context("selecting ns/db for backup")?;
    Ok(db)
}

/// Backup scheduling, read from `system_setting:main`.
#[derive(Debug, Clone, Deserialize, SurrealValue)]
pub struct BackupSettings {
    #[serde(default)]
    pub backup_enabled: bool,
    #[serde(default = "default_every_hours")]
    pub backup_every_hours: i64,
    #[serde(default = "default_keep_days")]
    pub backup_keep_days: i64,
    #[serde(default)]
    pub backup_last_run_at: Option<DateTime<Utc>>,
}

fn default_every_hours() -> i64 {
    24
}
fn default_keep_days() -> i64 {
    30
}

impl Default for BackupSettings {
    fn default() -> Self {
        Self {
            backup_enabled: false,
            backup_every_hours: default_every_hours(),
            backup_keep_days: default_keep_days(),
            backup_last_run_at: None,
        }
    }
}

pub async fn load_settings(state: &AppState) -> BackupSettings {
    let row: Option<BackupSettings> = state
        .db
        .select(RecordId::new("system_setting", "main"))
        .await
        .ok()
        .flatten();
    row.unwrap_or_default()
}

/// Run one backup now: export the database, stream it into object
/// storage, catalog it, then apply retention.
///
/// Returns the created row. Errors are returned rather than swallowed so
/// the admin's "Back up now" button can report honestly; the scheduler
/// logs them and carries on.
pub async fn run_backup(state: &AppState, kind: &str) -> anyhow::Result<Backup> {
    let started = Utc::now();
    let stamp = started.format("%Y%m%d-%H%M%S");
    let filename = format!("transactvault-{stamp}.surql");
    let storage_key = format!("{BACKUP_PREFIX}{filename}");

    let db = connect_for_backup(state).await?;

    // Export streams; spool it to a temp file rather than holding the
    // whole dump in memory, so a large database can't take the app down
    // while protecting it.
    let temp = tempfile::NamedTempFile::new().context("creating temp file for backup")?;
    let path = temp.path().to_path_buf();
    {
        let mut file = tokio::fs::File::create(&path)
            .await
            .context("opening temp backup file")?;
        let mut stream = db.export(()).await.context("starting database export")?;
        let mut total: u64 = 0;
        while let Some(chunk) = stream.next().await {
            let bytes = chunk.context("reading export stream")?;
            total += bytes.len() as u64;
            file.write_all(&bytes)
                .await
                .context("writing export chunk")?;
        }
        file.flush().await.context("flushing backup file")?;
        if total == 0 {
            anyhow::bail!("export produced no bytes — refusing to store an empty backup");
        }
    }

    let size_bytes = std::fs::metadata(&path)
        .context("sizing backup file")?
        .len();

    let reader = tokio::fs::File::open(&path)
        .await
        .context("reopening backup file for upload")?;
    let mut reader = tokio::io::BufReader::new(reader);
    state
        .storage
        .put_stream(&storage_key, &mut reader, "application/sql")
        .await
        .context("uploading backup to storage")?;

    // Drop the temp file explicitly so it is gone before we report success.
    let _ = temp.close();

    let created: Option<Backup> = state
        .db
        .create("backup")
        .content(NewBackup {
            storage_key: storage_key.clone(),
            filename: filename.clone(),
            size_bytes: size_bytes as i64,
            kind: kind.to_string(),
        })
        .await
        .context("cataloging backup")?;
    let created = created.ok_or_else(|| anyhow::anyhow!("backup row create returned nothing"))?;

    state
        .db
        .query("UPSERT $id SET backup_last_run_at = time::now()")
        .bind(("id", RecordId::new("system_setting", "main")))
        .await
        .context("recording backup timestamp")?;

    tracing::info!(
        %filename,
        size_bytes,
        kind,
        elapsed_ms = (Utc::now() - started).num_milliseconds(),
        "database backup stored"
    );

    if let Err(e) = purge_expired(state).await {
        tracing::warn!(error = %e, "backup retention sweep failed");
    }

    Ok(created)
}

/// Delete backups older than the configured retention window, storage
/// object first so a failure can't leave a row pointing at nothing.
pub async fn purge_expired(state: &AppState) -> anyhow::Result<usize> {
    let settings = load_settings(state).await;
    if settings.backup_keep_days <= 0 {
        return Ok(0);
    }
    let cutoff = Utc::now() - ChronoDuration::days(settings.backup_keep_days);

    let mut q = state
        .db
        .query("SELECT * FROM backup WHERE created_at < $cutoff")
        .bind(("cutoff", cutoff))
        .await
        .context("selecting expired backups")?;
    let expired: Vec<Backup> = q.take(0).unwrap_or_default();

    let mut removed = 0usize;
    for backup in expired {
        if let Err(e) = state.storage.delete(&backup.storage_key).await {
            tracing::warn!(error = %e, key = %backup.storage_key, "expired backup object delete failed");
            continue;
        }
        let _ = state
            .db
            .query("DELETE $id")
            .bind(("id", backup.id.clone()))
            .await;
        removed += 1;
    }
    if removed > 0 {
        tracing::info!(removed, "expired backups purged");
    }
    Ok(removed)
}

/// Delete one backup by record key. Storage object first, same reasoning
/// as the retention sweep.
pub async fn delete_backup(state: &AppState, key: &str) -> anyhow::Result<Backup> {
    let id = RecordId::new("backup", key);
    let row: Option<Backup> = state.db.select(id.clone()).await?;
    let row = row.ok_or_else(|| anyhow::anyhow!("no such backup"))?;
    state
        .storage
        .delete(&row.storage_key)
        .await
        .context("deleting backup object")?;
    state.db.query("DELETE $id").bind(("id", id)).await?;
    Ok(row)
}

/// Restore the database from a stored backup.
///
/// Downloads the dump and replays it through the same engine that wrote
/// it. **This applies the dump over the current database**: rows created
/// after the backup was taken are not removed, because SurrealDB's
/// import replays `DEFINE`/`INSERT`/`UPDATE` statements rather than
/// swapping the datastore. That makes it exactly right for recovering
/// into an empty or damaged database, and only approximately right as an
/// "undo". The admin page says so, and requires maintenance mode first
/// so nothing is writing underneath it.
pub async fn restore_backup(state: &AppState, key: &str) -> anyhow::Result<Backup> {
    let id = RecordId::new("backup", key);
    let row: Option<Backup> = state.db.select(id.clone()).await?;
    let row = row.ok_or_else(|| anyhow::anyhow!("no such backup"))?;

    let bytes = state
        .storage
        .get_bytes(&row.storage_key)
        .await
        .context("reading backup from storage")?
        .ok_or_else(|| anyhow::anyhow!("backup object is missing from storage"))?;

    let mut temp = tempfile::NamedTempFile::new().context("creating temp file for restore")?;
    std::io::Write::write_all(&mut temp, &bytes).context("writing restore file")?;
    std::io::Write::flush(&mut temp).context("flushing restore file")?;

    let db = connect_for_backup(state).await?;
    db.import(temp.path())
        .await
        .context("importing backup into the database")?;

    tracing::warn!(
        filename = %row.filename,
        size_bytes = row.size_bytes,
        "database RESTORED from backup"
    );
    Ok(row)
}

/// Background scheduler. Wakes every [`TICK`], and runs a backup when
/// the configured interval has elapsed since the last one.
///
/// Deliberately interval-since-last-run rather than wall-clock cron: it
/// survives restarts without double-running, and an app that was down
/// over its slot takes one as soon as it is back.
pub fn spawn(state: AppState) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(TICK);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        ticker.tick().await; // consume the immediate first tick

        loop {
            ticker.tick().await;
            let settings = load_settings(&state).await;
            if !settings.backup_enabled {
                continue;
            }
            let every = settings.backup_every_hours.max(1);
            let due = match settings.backup_last_run_at {
                Some(last) => Utc::now() - last >= ChronoDuration::hours(every),
                None => true,
            };
            if !due {
                continue;
            }
            match run_backup(&state, "auto").await {
                Ok(b) => tracing::info!(filename = %b.filename, "scheduled backup complete"),
                Err(e) => tracing::error!(error = %describe(&e), "scheduled backup FAILED"),
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::backup_endpoint;

    /// The whole backup feature rests on this rewrite: production talks
    /// WS, and the WS engine cannot export.
    #[test]
    fn websocket_urls_are_rewritten_to_http() {
        assert_eq!(
            backup_endpoint("ws://tv-surrealdb:8000"),
            "http://tv-surrealdb:8000"
        );
        assert_eq!(
            backup_endpoint("wss://db.example.com"),
            "https://db.example.com"
        );
        // Already-exportable endpoints pass through untouched.
        assert_eq!(
            backup_endpoint("http://localhost:8000"),
            "http://localhost:8000"
        );
        assert_eq!(backup_endpoint("mem://"), "mem://");
        assert_eq!(
            backup_endpoint("surrealkv://data/tv.db"),
            "surrealkv://data/tv.db"
        );
    }
}
