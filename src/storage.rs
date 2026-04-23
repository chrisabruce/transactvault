//! S3-compatible object storage client — intended to run against RustFS in
//! self-hosted deployments, but any S3 provider will work.
//!
//! Why hand-rolled credentials / virtual-host style disabled: RustFS and most
//! local dev S3 servers only speak the path-style addressing (bucket in the
//! URL path, not the host subdomain), and a static `access_key` / `secret_key`
//! pair from the environment is enough — we don't want to pull in the full
//! AWS credential provider chain for a self-hosted app.

use std::time::Duration;

use anyhow::Context;
use aws_credential_types::Credentials;
use aws_sdk_s3::Client;
use aws_sdk_s3::config::{Region, retry::RetryConfig, timeout::TimeoutConfig};
use aws_sdk_s3::error::SdkError;
use aws_sdk_s3::operation::get_object::GetObjectError;
use aws_sdk_s3::operation::head_bucket::HeadBucketError;
use aws_sdk_s3::primitives::ByteStream;
use bytes::Bytes;

use crate::config::RustFsConfig;

/// Clonable handle to the object store. Wraps the AWS SDK S3 client, which
/// is itself cheap to clone (internally reference-counted).
#[derive(Clone)]
pub struct Storage {
    client: Client,
    bucket: String,
}

impl Storage {
    /// Build a RustFS/S3 client from the configured endpoint and credentials.
    /// Also makes sure the bucket exists — creating it on first boot if it
    /// does not, which keeps the local-dev experience one-command.
    pub async fn connect(cfg: &RustFsConfig) -> anyhow::Result<Self> {
        let credentials = Credentials::new(
            cfg.access_key.clone(),
            cfg.secret_key.clone(),
            None,
            None,
            "tv-static",
        );

        let s3_config = aws_sdk_s3::Config::builder()
            .behavior_version_latest()
            .region(Region::new(cfg.region.clone()))
            .endpoint_url(cfg.endpoint.clone())
            .force_path_style(true)
            .credentials_provider(credentials)
            .retry_config(RetryConfig::standard().with_max_attempts(3))
            .timeout_config(
                TimeoutConfig::builder()
                    .operation_timeout(Duration::from_secs(30))
                    .build(),
            )
            .build();
        let client = Client::from_conf(s3_config);
        let storage = Self { client, bucket: cfg.bucket.clone() };
        storage.ensure_bucket().await.context("ensuring bucket exists")?;
        Ok(storage)
    }

    async fn ensure_bucket(&self) -> anyhow::Result<()> {
        match self.client.head_bucket().bucket(&self.bucket).send().await {
            Ok(_) => {
                tracing::info!(bucket = %self.bucket, "bucket reachable");
                Ok(())
            }
            Err(SdkError::ServiceError(svc)) if matches!(svc.err(), HeadBucketError::NotFound(_)) => {
                tracing::info!(bucket = %self.bucket, "creating bucket");
                self.client
                    .create_bucket()
                    .bucket(&self.bucket)
                    .send()
                    .await
                    .context("creating bucket")?;
                Ok(())
            }
            Err(e) => Err(anyhow::anyhow!("head bucket: {e}")),
        }
    }

    /// Upload bytes to `key`. Overwrites existing objects at the same key —
    /// versioning is handled at the application layer via the `document`
    /// table and `version_of` edges, not S3 versioning.
    pub async fn put_bytes(
        &self,
        key: &str,
        bytes: Vec<u8>,
        content_type: &str,
    ) -> anyhow::Result<()> {
        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(key)
            .content_type(content_type)
            .body(ByteStream::from(bytes))
            .send()
            .await
            .context("put_object")?;
        Ok(())
    }

    /// Fetch the full object bytes. Streams into memory — acceptable for the
    /// transaction-sized documents we handle; swap to a streaming reader if
    /// we ever store multi-GB files.
    pub async fn get_bytes(&self, key: &str) -> anyhow::Result<Bytes> {
        let out = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .map_err(|e| match e {
                SdkError::ServiceError(svc) if matches!(svc.err(), GetObjectError::NoSuchKey(_)) => {
                    anyhow::anyhow!("not found")
                }
                other => anyhow::anyhow!("get_object: {other}"),
            })?;
        let aggregated = out.body.collect().await.context("read body")?;
        Ok(aggregated.into_bytes())
    }
}
