//! S3-compatible object storage client — intended to run against RustFS in
//! self-hosted deployments, but any S3 provider will work.
//!
//! Uses the [`s3`] crate (community-maintained `rust-s3`) rather than the
//! AWS SDK. For a single-endpoint self-hosted setup we don't need any of
//! the heavier AWS features (IAM roles, STS, SSE-KMS, event streams) and
//! the 10× reduction in compile time and dependency surface is worth it.

use std::time::Duration;

use anyhow::Context;
use bytes::Bytes;
use s3::creds::Credentials;
use s3::error::S3Error;
use s3::{Bucket, BucketConfiguration, Region};
use tokio::io::AsyncRead;

use crate::config::RustFsConfig;

/// One bucket entry from [`Storage::list_all`].
#[derive(Debug, Clone)]
pub struct StoredObject {
    pub key: String,
    pub size: u64,
    /// `None` when the store returned an unparseable timestamp — the
    /// scanner treats that as "too new to touch", failing safe.
    pub last_modified: Option<chrono::DateTime<chrono::Utc>>,
}

/// What an unauthenticated stranger can get from the bucket. Produced
/// by [`Storage::probe_public_access`].
#[derive(Debug, Clone)]
pub struct PublicAccessProbe {
    /// The browser-facing bucket URL that was probed.
    pub endpoint: String,
    /// `Some(true)` = anonymous key enumeration works (critical).
    /// `Some(false)` = refused (healthy). `None` = probe inconclusive.
    pub listable: Option<bool>,
    /// Same tri-state for anonymous download of a real object.
    pub readable: Option<bool>,
    /// Why the probe couldn't run, when applicable.
    pub note: Option<String>,
}

impl PublicAccessProbe {
    /// Placeholder for paths that bailed before the probe could run
    /// (e.g. the bucket listing itself failed).
    pub fn inconclusive() -> Self {
        Self::unknown("The scan stopped before this check could run.".to_string())
    }

    fn unknown(note: String) -> Self {
        Self {
            endpoint: String::new(),
            listable: None,
            readable: None,
            note: Some(note),
        }
    }

    /// True when either probe came back open. Drives the red banner.
    pub fn is_exposed(&self) -> bool {
        self.listable == Some(true) || self.readable == Some(true)
    }

    /// True only when both probes ran AND both were refused.
    pub fn is_confirmed_private(&self) -> bool {
        self.listable == Some(false) && self.readable == Some(false)
    }

    pub fn listing_label(&self) -> &'static str {
        match self.listable {
            Some(true) => "OPEN — anyone can list every file",
            Some(false) => "Refused",
            None => "Could not check",
        }
    }

    pub fn read_label(&self) -> &'static str {
        match self.readable {
            Some(true) => "OPEN — anyone with a URL can download",
            Some(false) => "Refused",
            None => "Could not check",
        }
    }
}

/// How long a presigned upload URL stays valid. This bounds when the
/// PUT may *start* — signatures are checked at request start, so a
/// slow transfer that began in time may run as long as the provider
/// allows. The upload JS PUTs immediately after presigning, so ten
/// minutes is already generous; keeping it short shrinks the window
/// in which a leaked URL is usable at all.
pub const PRESIGN_EXPIRY_SECS: u32 = 10 * 60;

/// Clonable handle to the object store. `Bucket` is `Send + Sync` and cheap
/// to clone; it wraps a shared reqwest client internally.
#[derive(Clone)]
pub struct Storage {
    bucket: Box<Bucket>,
    /// Handle used ONLY for presigning browser-facing URLs. Same
    /// bucket + credentials, but pointed at `S3_PUBLIC_ENDPOINT` when
    /// configured — the SigV4 signature covers the `Host` header, so a
    /// URL signed for the app's internal endpoint would be rejected
    /// the moment the browser sends it to the public one.
    presign_bucket: Box<Bucket>,
    /// Browser-facing endpoint origin (public if configured, else the
    /// app's own). Used by [`Storage::probe_public_access`] to ask what
    /// an unauthenticated outsider can see.
    public_base: String,
}

impl Storage {
    /// Construct a non-functional [`Storage`] for tests that don't
    /// touch object storage. The bucket points at a non-routable
    /// address; any actual `get_object` / `put_object` call will fail.
    /// Use only when the code under test never hits the storage layer
    /// (billing gates, authz checks, query helpers, etc.).
    #[cfg(test)]
    pub fn null_for_tests() -> Self {
        let credentials = Credentials::new(Some("test"), Some("test"), None, None, None)
            .expect("test credentials");
        let region = Region::Custom {
            region: "us-east-1".into(),
            endpoint: "http://127.0.0.1:1".into(),
        };
        let bucket = Bucket::new("test", region, credentials)
            .expect("bucket handle for tests")
            .with_path_style();
        Self {
            presign_bucket: bucket.clone(),
            bucket,
            public_base: "http://127.0.0.1:1".into(),
        }
    }

    /// Build a bucket handle pointed at the configured endpoint + region.
    /// Uses path-style addressing because RustFS and MinIO (and most local
    /// dev S3 servers) only speak that dialect — virtual-host-style requires
    /// bucket-per-subdomain DNS.
    pub async fn connect(cfg: &RustFsConfig) -> anyhow::Result<Self> {
        let credentials = Credentials::new(
            Some(&cfg.access_key),
            Some(&cfg.secret_key),
            None,
            None,
            None,
        )
        .map_err(|e| anyhow::anyhow!("invalid credentials: {e}"))?;

        let region = Region::Custom {
            region: cfg.region.clone(),
            endpoint: cfg.endpoint.clone(),
        };

        let mut bucket = Bucket::new(&cfg.bucket, region.clone(), credentials.clone())
            .map_err(|e| anyhow::anyhow!("bucket handle: {e}"))?
            .with_path_style();
        // Per-request ceiling to the object store, sized for the
        // slowest single request we make rather than the average:
        // streaming uploads go up as ~8 MiB parts (one request each),
        // `get_bytes` pulls a whole ≤100 MB object in one GET, and
        // CompleteMultipartUpload can take a while on Ceph-based
        // providers (Contabo) while parts are assembled. The previous
        // 30 s proved too tight against Contabo in production.
        bucket.set_request_timeout(Some(Duration::from_secs(300)));

        let presign_bucket = match &cfg.public_endpoint {
            Some(public) => Bucket::new(
                &cfg.bucket,
                Region::Custom {
                    region: cfg.region.clone(),
                    endpoint: public.clone(),
                },
                credentials.clone(),
            )
            .map_err(|e| anyhow::anyhow!("presign bucket handle: {e}"))?
            .with_path_style(),
            None => bucket.clone(),
        };

        let public_base = cfg
            .public_endpoint
            .clone()
            .unwrap_or_else(|| cfg.endpoint.clone());
        let storage = Self {
            bucket,
            presign_bucket,
            public_base,
        };
        if cfg.auto_create_bucket {
            storage
                .ensure_bucket(&cfg.bucket, region, credentials)
                .await
                .context("ensuring bucket exists")?;
        } else {
            tracing::info!(
                bucket = %cfg.bucket,
                "skipping bucket creation (S3_AUTO_CREATE_BUCKET=false)"
            );
        }
        Ok(storage)
    }

    /// Make sure the bucket is ready to accept writes. `CreateBucket` is
    /// idempotent across S3 implementations — existing-bucket responses
    /// come back as a 409 with a specific error code, which we treat as
    /// success. Retries transient startup failures because RustFS takes a
    /// few seconds to finish loading its IAM subsystem after accepting TCP.
    async fn ensure_bucket(
        &self,
        name: &str,
        region: Region,
        credentials: Credentials,
    ) -> anyhow::Result<()> {
        let mut attempt: u32 = 0;
        loop {
            let created = Bucket::create_with_path_style(
                name,
                region.clone(),
                credentials.clone(),
                BucketConfiguration::default(),
            )
            .await;

            match created {
                Ok(_) => {
                    tracing::info!(bucket = %name, "bucket ready");
                    return Ok(());
                }
                Err(e) if is_bucket_already_exists(&e) => {
                    tracing::info!(bucket = %name, "bucket already exists");
                    return Ok(());
                }
                Err(e) if attempt < 10 => {
                    tracing::warn!(
                        attempt = attempt + 1,
                        error = %e,
                        "ensure_bucket failed, retrying (rustfs may still be warming up)"
                    );
                    tokio::time::sleep(backoff(attempt)).await;
                    attempt += 1;
                }
                Err(e) => return Err(anyhow::anyhow!("ensure_bucket: {e}")),
            }
        }
    }

    /// Upload bytes to `key`. Overwrites existing objects at the same key —
    /// versioning is handled at the application layer via the `document`
    /// table and `version_of` edges, not via S3 versioning.
    #[allow(dead_code)]
    pub async fn put_bytes(
        &self,
        key: &str,
        bytes: Vec<u8>,
        content_type: &str,
    ) -> anyhow::Result<()> {
        self.bucket
            .put_object_with_content_type(key, &bytes, content_type)
            .await
            .context("put_object")?;
        Ok(())
    }

    /// Stream an upload straight to the object store without buffering the
    /// whole payload in memory. `rust-s3` runs this as an S3 multipart
    /// upload internally, so 100 MB transfers don't pin 100 MB of process
    /// memory.
    ///
    /// Returns the byte count actually written, so the caller can store
    /// the size on the `document` row without trusting the client's
    /// `Content-Length` header.
    pub async fn put_stream<R>(
        &self,
        key: &str,
        reader: &mut R,
        content_type: &str,
    ) -> anyhow::Result<u64>
    where
        R: AsyncRead + Unpin,
    {
        let resp = self
            .bucket
            .put_object_stream_with_content_type(reader, key, content_type)
            .await
            .context("put_object_stream")?;
        Ok(resp.uploaded_bytes() as u64)
    }

    /// Presigned browser PUT for `key`, valid for
    /// [`PRESIGN_EXPIRY_SECS`]. The canonical `Content-Type` AND the
    /// exact `Content-Length` are folded into the signature, so the
    /// URL only works for the type the server chose and the byte count
    /// it approved. The browser always sends the true length (page
    /// script cannot override it), which makes the store itself reject
    /// a swapped or oversize file with a signature mismatch — the
    /// finalize-time HEAD check stays as belt-and-braces on top.
    pub async fn presign_put(
        &self,
        key: &str,
        content_type: &str,
        content_length: u64,
    ) -> anyhow::Result<String> {
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::CONTENT_TYPE,
            http::HeaderValue::from_str(content_type).context("content type as header")?,
        );
        headers.insert(
            http::header::CONTENT_LENGTH,
            http::HeaderValue::from_str(&content_length.to_string())
                .context("content length as header")?,
        );
        self.presign_bucket
            .presign_put(key, PRESIGN_EXPIRY_SECS, Some(headers), None)
            .await
            .map_err(|e| anyhow::anyhow!("presign_put: {e}"))
    }

    /// Presigned browser GET for `key`, valid for `expiry_secs`.
    ///
    /// This is how big export artifacts leave the system: the app hands
    /// out a signed URL and the store serves the bytes itself — with
    /// native `Range` support, so the browser (or aria2/curl) can
    /// resume an interrupted multi-GB download, and none of the
    /// transfer occupies an app connection.
    ///
    /// `download_name` rides in the signed
    /// `response-content-disposition` query parameter so the store
    /// serves the object as an attachment with a human filename instead
    /// of the raw key basename. Callers pass server-built names only —
    /// the value is embedded in a header by the store, so it must never
    /// carry user-typed text with quotes/control characters.
    pub async fn presign_get(
        &self,
        key: &str,
        expiry_secs: u32,
        download_name: Option<&str>,
    ) -> anyhow::Result<String> {
        let custom_queries = download_name.map(|name| {
            std::collections::HashMap::from([(
                "response-content-disposition".to_string(),
                format!("attachment; filename=\"{name}\""),
            )])
        });
        self.presign_bucket
            .presign_get(key, expiry_secs, custom_queries)
            .await
            .map_err(|e| anyhow::anyhow!("presign_get: {e}"))
    }

    /// Delete every object under `prefix` (paged listing, one
    /// `DeleteObject` per key — the portable lowest common denominator
    /// across S3 implementations). Returns how many objects were
    /// deleted; per-object failures are logged and skipped so a later
    /// sweep can retry them.
    ///
    /// Callers own prefix hygiene: pass the full `exports/<b>/<job>/`
    /// style prefix INCLUDING the trailing slash, or sibling keys that
    /// merely share the string prefix would be swept up too.
    pub async fn delete_prefix(&self, prefix: &str) -> anyhow::Result<usize> {
        let mut deleted = 0usize;
        let pages = self
            .bucket
            .list(prefix.to_string(), None)
            .await
            .map_err(|e| anyhow::anyhow!("list {prefix}: {e}"))?;
        for page in pages {
            for obj in page.contents {
                match self.bucket.delete_object(&obj.key).await {
                    Ok(_) => deleted += 1,
                    Err(e) if is_not_found(&e) => {}
                    Err(e) => tracing::warn!(key = %obj.key, error = %e, "prefix delete failed"),
                }
            }
        }
        Ok(deleted)
    }

    /// Probe the bucket for anonymous (unsigned) access.
    ///
    /// Every object here is a client's confidential transaction file, so
    /// "is the bucket private?" must be answered by observation, not by
    /// trusting that whoever created it ticked the right box in a
    /// provider console. This sends two UNSIGNED requests to the
    /// browser-facing endpoint — the same address an outsider would
    /// reach — and reports what a stranger gets back:
    ///
    /// - `GET {endpoint}/{bucket}?list-type=2` → anonymous LISTING.
    ///   A 200 here means anyone can enumerate every key in the vault.
    /// - `GET {endpoint}/{bucket}/{probe_key}` → anonymous READ of a
    ///   real object. A 200 means anyone holding (or guessing) a key
    ///   can download the document without a signature.
    ///
    /// Anything other than 200 (403/401/404) is the healthy answer.
    /// Network failure is reported as unknown rather than "safe": a
    /// probe that never arrived proves nothing.
    pub async fn probe_public_access(&self, probe_key: Option<&str>) -> PublicAccessProbe {
        let base = format!(
            "{}/{}",
            self.public_base.trim_end_matches('/'),
            self.bucket.name()
        );
        let client = match reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                return PublicAccessProbe::unknown(format!("could not build probe client: {e}"));
            }
        };

        let list_url = format!("{base}?list-type=2&max-keys=1");
        let listable = match client.get(&list_url).send().await {
            Ok(r) => Some(r.status().is_success()),
            Err(e) => {
                tracing::warn!(error = %e, "anonymous list probe failed to reach storage");
                None
            }
        };

        let readable = match probe_key {
            Some(key) => {
                let object_url = format!("{base}/{key}");
                match client.get(&object_url).send().await {
                    Ok(r) => Some(r.status().is_success()),
                    Err(e) => {
                        tracing::warn!(error = %e, "anonymous object probe failed to reach storage");
                        None
                    }
                }
            }
            None => None,
        };

        PublicAccessProbe {
            endpoint: base,
            listable,
            readable,
            note: None,
        }
    }

    /// Every object in the bucket: key, size, and last-modified time.
    /// Feeds the admin orphan scanner — which needs the whole inventory
    /// to diff against the database, so there is deliberately no prefix
    /// argument to get subtly wrong.
    pub async fn list_all(&self) -> anyhow::Result<Vec<StoredObject>> {
        let pages = self
            .bucket
            .list(String::new(), None)
            .await
            .map_err(|e| anyhow::anyhow!("list bucket: {e}"))?;
        let mut out = Vec::new();
        for page in pages {
            for obj in page.contents {
                out.push(StoredObject {
                    key: obj.key,
                    size: obj.size,
                    last_modified: chrono::DateTime::parse_from_rfc3339(&obj.last_modified)
                        .ok()
                        .map(|t| t.with_timezone(&chrono::Utc)),
                });
            }
        }
        Ok(out)
    }

    /// Size of the stored object, `Ok(None)` when the key doesn't
    /// exist. This is the post-upload enforcement half of the presigned
    /// flow: the server never saw the bytes, so the object's true size
    /// must come from the store, never from the client.
    pub async fn head_size(&self, key: &str) -> anyhow::Result<Option<u64>> {
        match self.bucket.head_object(key).await {
            Ok((head, code)) if (200..300).contains(&code) => {
                Ok(Some(head.content_length.unwrap_or(0).max(0) as u64))
            }
            Ok((_, 404)) => Ok(None),
            Ok((_, code)) => Err(anyhow::anyhow!("head_object: HTTP {code}")),
            Err(e) if is_not_found(&e) => Ok(None),
            Err(e) => Err(anyhow::anyhow!("head_object: {e}")),
        }
    }

    /// Best-effort bucket CORS so browsers can send presigned PUTs
    /// directly. Failure is a warning, not fatal: some providers
    /// reject the CORS API or scope keys without that permission —
    /// there the same rule must be created once in the provider
    /// console, and until then the upload JS falls back to the proxy
    /// path when the direct PUT is blocked.
    pub async fn ensure_cors(&self, app_origin: &str) {
        use s3::serde_types::{CorsConfiguration, CorsRule};
        let rule = CorsRule::new(
            Some(vec!["content-type".into()]),
            vec!["PUT".into()],
            vec![app_origin.to_string()],
            Some(vec!["ETag".into()]),
            None,
            Some(3600),
        );
        let config = CorsConfiguration::new(vec![rule]);
        match self.bucket.put_bucket_cors("", &config).await {
            Ok(_) => {
                tracing::info!(origin = %app_origin, "bucket CORS ready for direct uploads");
            }
            Err(e) => tracing::warn!(
                error = %e,
                origin = %app_origin,
                "could not configure bucket CORS — if direct uploads fall back to \
                 proxying, add a PUT rule for this origin in the storage provider's console"
            ),
        }
    }

    /// Delete a single object. Missing keys are treated as success —
    /// callers only invoke this after looking the key up via DB state,
    /// so racing a concurrent delete or finding storage already-purged
    /// shouldn't be an error condition.
    pub async fn delete(&self, key: &str) -> anyhow::Result<()> {
        match self.bucket.delete_object(key).await {
            Ok(_) => Ok(()),
            Err(e) if is_not_found(&e) => Ok(()),
            Err(e) => Err(anyhow::anyhow!("delete_object: {e}")),
        }
    }

    /// Fetch the full object bytes. Returns `Ok(None)` when the key
    /// doesn't exist so callers can decide between "missing" and
    /// "transport error" without resorting to string matching on the
    /// `anyhow` chain.
    ///
    /// Streams into memory — acceptable for the transaction-sized
    /// documents we handle; swap to a streaming reader if we ever store
    /// multi-GB files.
    pub async fn get_bytes(&self, key: &str) -> anyhow::Result<Option<Bytes>> {
        match self.bucket.get_object(key).await {
            Ok(resp) => Ok(Some(Bytes::from(resp.to_vec()))),
            Err(e) if is_not_found(&e) => Ok(None),
            Err(e) => Err(anyhow::anyhow!("get_object: {e}")),
        }
    }

    /// Open an object as a chunk stream instead of buffering it whole.
    ///
    /// [`Self::get_bytes`] materializes the entire object (and copies it
    /// once more on the way out), which is fine for serving one document
    /// to a browser but ruinous for archive building — a 20-document
    /// export used to hold every byte in memory at once. Streaming lets
    /// the ZIP writer consume a few kilobytes at a time, so peak memory
    /// is independent of both document size and document count.
    ///
    /// Returns `Ok(None)` when the key doesn't exist, matching
    /// `get_bytes`, so callers keep the same missing-object handling.
    pub async fn get_stream(
        &self,
        key: &str,
    ) -> anyhow::Result<Option<s3::request::ResponseDataStream>> {
        match self.bucket.get_object_stream(key).await {
            Ok(stream) if stream.status_code == 404 => Ok(None),
            Ok(stream) => Ok(Some(stream)),
            Err(e) if is_not_found(&e) => Ok(None),
            Err(e) => Err(anyhow::anyhow!("get_object_stream: {e}")),
        }
    }

    /// **DEV-ONLY.** Delete every object in the bucket. Walks the bucket
    /// listing (paged via S3's continuation tokens) and issues `DeleteObject`
    /// for each key. The bucket itself is left in place so the next write
    /// doesn't have to wait for `CreateBucket` to round-trip.
    ///
    /// Only invoked from the boot path when [`Config::dev_reset_on_boot`] is
    /// true. Logs the count of deleted objects at WARN.
    pub async fn dev_wipe_bucket(&self) -> anyhow::Result<()> {
        tracing::warn!(
            "DEV-ONLY: wiping every object in the storage bucket. This destroys \
             all uploaded documents permanently."
        );

        let mut deleted: usize = 0;
        let pages = self
            .bucket
            .list(String::new(), None)
            .await
            .context("list objects for wipe")?;

        for page in pages {
            for obj in page.contents {
                match self.bucket.delete_object(&obj.key).await {
                    Ok(_) => deleted += 1,
                    Err(e) => tracing::warn!(key = %obj.key, error = %e, "delete failed"),
                }
            }
        }

        tracing::warn!(deleted_objects = deleted, "DEV-ONLY storage wipe complete");
        Ok(())
    }
}

/// Exponential-ish backoff: 500ms, 1s, 1.5s, ... up to 5.5s.
fn backoff(attempt: u32) -> Duration {
    Duration::from_millis(500 * (attempt as u64 + 1))
}

/// Recognise "bucket already exists" responses so ensure_bucket can treat
/// them as success. S3 returns 409 with either `BucketAlreadyExists` or
/// `BucketAlreadyOwnedByYou` depending on ownership; RustFS/MinIO sometimes
/// use slightly different wording.
fn is_bucket_already_exists(err: &S3Error) -> bool {
    match err {
        S3Error::HttpFailWithBody(code, body) => {
            *code == 409
                || body.contains("BucketAlreadyOwnedByYou")
                || body.contains("BucketAlreadyExists")
                || body.contains("already exists")
        }
        _ => false,
    }
}

/// Recognise "object doesn't exist" for download responses.
fn is_not_found(err: &S3Error) -> bool {
    match err {
        S3Error::HttpFailWithBody(code, body) => *code == 404 || body.contains("NoSuchKey"),
        _ => false,
    }
}
