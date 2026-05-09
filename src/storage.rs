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

use crate::config::RustFsConfig;

/// Clonable handle to the object store. `Bucket` is `Send + Sync` and cheap
/// to clone; it wraps a shared reqwest client internally.
#[derive(Clone)]
pub struct Storage {
    bucket: Box<Bucket>,
}

impl Storage {
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
        bucket.set_request_timeout(Some(Duration::from_secs(30)));

        let storage = Self { bucket };
        storage
            .ensure_bucket(&cfg.bucket, region, credentials)
            .await
            .context("ensuring bucket exists")?;
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

    /// Fetch the full object bytes. Streams into memory — acceptable for the
    /// transaction-sized documents we handle; swap to a streaming reader if
    /// we ever store multi-GB files.
    pub async fn get_bytes(&self, key: &str) -> anyhow::Result<Bytes> {
        let resp = self.bucket.get_object(key).await.map_err(|e| {
            if is_not_found(&e) {
                anyhow::anyhow!("not found")
            } else {
                anyhow::anyhow!("get_object: {e}")
            }
        })?;
        Ok(Bytes::from(resp.to_vec()))
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
