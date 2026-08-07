//! Runtime configuration loaded from environment variables.
//!
//! Every setting has a sensible local-dev default so `cargo run` works
//! without a `.env` file. Production deployments should supply their own
//! values, most importantly `JWT_SECRET` and a persistent `SURREAL_URL`.

use std::env;

use anyhow::Context;

/// Immutable process-wide configuration.
#[derive(Debug, Clone)]
pub struct Config {
    pub app_name: String,
    pub base_url: String,
    pub host: String,
    pub port: u16,
    pub pretty_logs: bool,

    pub surreal_url: String,
    pub surreal_user: String,
    pub surreal_pass: String,
    pub surreal_ns: String,
    pub surreal_db: String,

    pub jwt_secret: String,
    pub jwt_expiry_hours: i64,

    /// Comma-separated emails (lower-cased) granted access to `/admin/*`.
    /// Membership is checked at request time via the `admin_required`
    /// extractor; this is independent of the per-brokerage `broker` role.
    pub super_admin_emails: Vec<String>,

    /// Password-reset link expiry, in hours. Deliberately short — the
    /// link is a bearer credential that grants account takeover, so it
    /// should outlive a slow inbox but nothing more.
    pub reset_expiry_hours: i64,

    /// Verification-link expiry. After this window the user has to request
    /// a fresh link to finish signup. 24 hours is a reasonable default.
    pub verification_expiry_hours: i64,

    /// Proof-of-work difficulty in leading-zero bits. 18 ≈ 0.5–2s of
    /// JavaScript work for honest users; 0 disables the check entirely
    /// (handy in tests).
    pub pow_difficulty_bits: u32,

    /// Token-bucket rate limit for `/signup`: max requests per IP per hour.
    pub signup_rate_per_hour: u32,
    /// Same idea for `/login`, per IP per 15 minutes.
    pub login_rate_per_quarter_hour: u32,

    /// **DEV-ONLY.** When `true`, the app drops every domain table AND every
    /// object in the storage bucket at boot before applying the schema —
    /// destroying users, brokerages, transactions, audit events, and uploaded
    /// documents. Triggered only when `DEV_RESET_ON_BOOT` is set to the exact
    /// phrase `"yes-destroy-all-data"`. Anything else (including the literal
    /// strings `"true"`, `"1"`, `"yes"`) leaves data alone. Designed so it
    /// can't be flipped on by a typo or a copy-pasted env var. Never set in
    /// production.
    pub dev_reset_on_boot: bool,

    /// How many reverse proxies sit in front of the app. Only the last
    /// this-many `X-Forwarded-For` entries were written by
    /// infrastructure we control, so this decides which entry
    /// [`crate::security::client_ip`] trusts as the real client. `1`
    /// suits the standard single-Traefik deployment; `0` means the app
    /// is exposed directly and forwarding headers are ignored.
    pub trusted_proxy_hops: usize,

    /// Length of the card-free trial, in days, counted from the
    /// brokerage's FIRST transaction rather than from signup. Distinct
    /// from `stripe.trial_days`: this one gates the app before anyone
    /// has entered a card, and Checkout grants only whatever is left of
    /// it so the total free period stays exactly this long.
    pub trial_days: u32,

    /// Who gets an email when someone sends feedback or uses the
    /// contact form. Comma-separated in `NOTIFY_EMAILS`; empty disables
    /// the notification (the message is still stored and visible in
    /// Admin → Feedback).
    pub notify_emails: Vec<String>,

    /// Boot straight into maintenance mode (`MAINTENANCE_MODE=true`).
    /// The switch for server migrations and database restores: the gate
    /// works without touching the database, so the app can answer with
    /// the friendly 503 page while there is nothing behind it. Runtime
    /// state lives in [`crate::state::Ops`]; a super-admin can also flip
    /// it from `/admin/ops` without a restart.
    pub maintenance_mode: bool,

    pub rustfs: RustFsConfig,
    pub email: EmailConfig,
    pub stripe: StripeConfig,
}

/// S3-compatible object storage settings (RustFS by default).
#[derive(Debug, Clone)]
pub struct RustFsConfig {
    pub endpoint: String,
    /// Browser-facing endpoint used ONLY for presigned direct-upload
    /// URLs. `None` falls back to `endpoint`, which is right whenever
    /// that address is publicly reachable (Contabo, any managed
    /// provider). Set `S3_PUBLIC_ENDPOINT` when the app reaches
    /// storage over an address the browser can't — the docker-compose
    /// dev stack talks to RustFS at `http://rustfs:9000`, while the
    /// browser needs the host-mapped `http://127.0.0.1:37421`.
    pub public_endpoint: Option<String>,
    pub region: String,
    pub access_key: String,
    pub secret_key: String,
    pub bucket: String,
    /// Whether to issue `CreateBucket` at startup.
    ///
    /// True suits local RustFS, where the bucket is created on first
    /// boot. Point this at a managed provider whose bucket you made in
    /// their console and it becomes a liability: keys scoped to that one
    /// bucket answer `CreateBucket` with `403 AccessDenied`, which is
    /// not the "already exists" response the startup check forgives, so
    /// the app retries and then refuses to boot against a bucket that
    /// was fine all along. Set `S3_AUTO_CREATE_BUCKET=false` there.
    pub auto_create_bucket: bool,
}

/// Postmark transactional email settings. An empty `server_token`
/// disables the transport — messages are logged at INFO level but not
/// delivered, which keeps local dev one-command.
///
/// `message_stream` controls which Postmark stream the message is
/// posted to — Postmark requires this field on every send. Default is
/// `"outbound"` (every Postmark server has a default outbound stream);
/// override via `POSTMARK_MESSAGE_STREAM` if you've defined a custom
/// stream for, say, separating invite emails from welcome emails so
/// they have independent analytics + suppression lists.
#[derive(Debug, Clone)]
pub struct EmailConfig {
    pub server_token: String,
    pub from: String,
    pub reply_to: Option<String>,
    pub message_stream: String,
}

impl EmailConfig {
    pub fn is_enabled(&self) -> bool {
        !self.server_token.is_empty()
    }
}

/// Stripe settings. An empty `secret_key` disables the Stripe client —
/// tier writes still happen locally but Product/Price sync is skipped
/// and Checkout endpoints will refuse with a clear error. Set this
/// **once** before brokers start subscribing; flipping it on later
/// won't backfill Stripe IDs onto existing tiers (you'd need to
/// re-save each tier from the admin UI).
#[derive(Debug, Clone)]
pub struct StripeConfig {
    pub secret_key: String,
    /// `whsec_…` from the Stripe Dashboard. Required to verify
    /// incoming webhook payloads; if empty, the webhook handler
    /// returns 400 to avoid mistakenly trusting unsigned requests.
    pub webhook_secret: String,
    /// Free-trial length on Checkout, in days. `0` disables the trial
    /// (Checkout charges the card immediately).
    pub trial_days: u32,
}

impl StripeConfig {
    pub fn is_enabled(&self) -> bool {
        !self.secret_key.is_empty()
    }
}

impl Config {
    /// Minimal test config — every external integration disabled so
    /// the AppState built from this config doesn't reach off-host:
    /// Stripe client is None, Postmark token is empty (mailer logs
    /// instead of sending), S3 endpoint is a non-routable address.
    /// PoW disabled. Suitable for `tower::ServiceExt::oneshot`-style
    /// HTTP tests.
    #[cfg(test)]
    pub fn for_tests() -> Self {
        Self {
            app_name: "TransactVault Test".into(),
            base_url: "http://test.local".into(),
            host: "127.0.0.1".into(),
            port: 0,
            pretty_logs: false,
            surreal_url: "mem://".into(),
            surreal_user: String::new(),
            surreal_pass: String::new(),
            surreal_ns: "test".into(),
            surreal_db: "test".into(),
            jwt_secret: "test-jwt-secret-at-least-32-chars-long".into(),
            jwt_expiry_hours: 24,
            super_admin_emails: vec!["admin@test".into()],
            verification_expiry_hours: 24,
            reset_expiry_hours: 1,
            pow_difficulty_bits: 0,
            signup_rate_per_hour: 1000,
            login_rate_per_quarter_hour: 1000,
            dev_reset_on_boot: false,
            trusted_proxy_hops: 0,
            trial_days: 14,
            notify_emails: vec!["ops@test".into()],
            maintenance_mode: false,
            rustfs: RustFsConfig {
                endpoint: "http://127.0.0.1:1".into(),
                public_endpoint: None,
                region: "us-east-1".into(),
                access_key: "test".into(),
                secret_key: "test".into(),
                bucket: "test".into(),
                auto_create_bucket: false,
            },
            email: EmailConfig {
                server_token: String::new(),
                from: "test@test.local".into(),
                reply_to: None,
                message_stream: "outbound".into(),
            },
            stripe: StripeConfig {
                secret_key: String::new(),
                webhook_secret: String::new(),
                trial_days: 14,
            },
        }
    }

    /// Read every setting from the process environment. Missing values fall
    /// back to development defaults, but `JWT_SECRET` must be overridden in
    /// any shared/production deployment.
    pub fn from_env() -> anyhow::Result<Self> {
        let config = Self {
            app_name: env_or("APP_NAME", "TransactVault"),
            base_url: env_or("BASE_URL", "http://localhost:37420"),
            host: env_or("HOST", "0.0.0.0"),
            port: env_or("PORT", "37420")
                .parse()
                .context("PORT must be a valid port number")?,
            pretty_logs: env_flag("PRETTY_LOGS", true),

            surreal_url: env_or("SURREAL_URL", "mem://"),
            surreal_user: env_or("SURREAL_USER", "root"),
            surreal_pass: env_or("SURREAL_PASS", "root"),
            surreal_ns: env_or("SURREAL_NS", "transactvault"),
            surreal_db: env_or("SURREAL_DB", "app"),

            jwt_secret: env_or(
                "JWT_SECRET",
                "dev-only-secret-change-me-change-me-change-me-change-me",
            ),
            jwt_expiry_hours: env_or("JWT_EXPIRY_HOURS", "168")
                .parse()
                .context("JWT_EXPIRY_HOURS must be an integer")?,

            super_admin_emails: env_or("SUPERADMIN_EMAILS", "")
                .split(',')
                .map(|s| s.trim().to_ascii_lowercase())
                .filter(|s| !s.is_empty())
                .collect(),

            verification_expiry_hours: env_or("VERIFICATION_EXPIRY_HOURS", "24")
                .parse()
                .context("VERIFICATION_EXPIRY_HOURS must be an integer")?,
            reset_expiry_hours: env_or("RESET_EXPIRY_HOURS", "1")
                .parse()
                .context("RESET_EXPIRY_HOURS must be an integer")?,

            pow_difficulty_bits: env_or("POW_DIFFICULTY_BITS", "18")
                .parse()
                .context("POW_DIFFICULTY_BITS must be an integer")?,

            signup_rate_per_hour: env_or("SIGNUP_RATE_PER_HOUR", "5")
                .parse()
                .context("SIGNUP_RATE_PER_HOUR must be an integer")?,
            login_rate_per_quarter_hour: env_or("LOGIN_RATE_PER_QH", "20")
                .parse()
                .context("LOGIN_RATE_PER_QH must be an integer")?,

            // Foot-gun guard: only the literal phrase enables the wipe, and
            // the env var name itself starts with `DEV_` so production
            // configs are unlikely to accidentally include it.
            dev_reset_on_boot: env_or("DEV_RESET_ON_BOOT", "") == "yes-destroy-all-data",
            trusted_proxy_hops: env_or("TRUSTED_PROXY_HOPS", "1")
                .parse()
                .context("TRUSTED_PROXY_HOPS must be a non-negative integer")?,
            trial_days: env_or("TRIAL_DAYS", "14")
                .parse()
                .context("TRIAL_DAYS must be a non-negative integer")?,
            notify_emails: env_or(
                "NOTIFY_EMAILS",
                "jason@transactvault.app,chris@transactvault.app",
            )
            .split(',')
            .map(|s| s.trim().to_ascii_lowercase())
            .filter(|s| !s.is_empty())
            .collect(),
            maintenance_mode: matches!(
                env_or("MAINTENANCE_MODE", "false")
                    .to_ascii_lowercase()
                    .as_str(),
                "true" | "1" | "yes" | "on"
            ),

            rustfs: RustFsConfig {
                endpoint: env_either("S3_ENDPOINT", "RUSTFS_ENDPOINT", "http://127.0.0.1:37421"),
                public_endpoint: std::env::var("S3_PUBLIC_ENDPOINT")
                    .ok()
                    .filter(|v| !v.trim().is_empty()),
                region: env_either("S3_REGION", "RUSTFS_REGION", "us-east-1"),
                access_key: env_either("S3_ACCESS_KEY", "RUSTFS_ACCESS_KEY", "rustfsadmin"),
                secret_key: env_either("S3_SECRET_KEY", "RUSTFS_SECRET_KEY", "rustfsadmin"),
                bucket: env_either("S3_BUCKET", "RUSTFS_BUCKET", "transactvault"),
                auto_create_bucket: env_flag("S3_AUTO_CREATE_BUCKET", true),
            },
            email: EmailConfig {
                server_token: env_or("POSTMARK_SERVER_TOKEN", ""),
                from: env_or(
                    "POSTMARK_FROM",
                    "TransactVault <no-reply@transactvault.example>",
                ),
                reply_to: env::var("POSTMARK_REPLY_TO").ok().filter(|s| !s.is_empty()),
                message_stream: env_or("POSTMARK_MESSAGE_STREAM", "outbound"),
            },
            stripe: StripeConfig {
                secret_key: env_or("STRIPE_SECRET_KEY", ""),
                webhook_secret: env_or("STRIPE_WEBHOOK_SECRET", ""),
                trial_days: env_or("STRIPE_TRIAL_DAYS", "14")
                    .parse()
                    .context("STRIPE_TRIAL_DAYS must be a non-negative integer")?,
            },
        };

        config.assert_safe_for_deployment()?;
        Ok(config)
    }

    /// Refuse to boot on a configuration that would be catastrophic in a
    /// shared or production deployment. Called at the end of
    /// [`Self::from_env`], so there is no way to construct a live
    /// `Config` that skips it.
    ///
    /// # Errors
    ///
    /// - **Development JWT secret.** Every fallback below is a public
    ///   string (this repository is published), and `JWT_SECRET` is the
    ///   sole thing standing between an attacker and a forged session
    ///   for any user — `sub` is just a record key. A missing or
    ///   misspelled env var used to boot silently on the default, so
    ///   this fails loudly instead. Also enforces 32 bytes minimum.
    /// - **Destructive reset against an HTTPS deployment.**
    ///   `DEV_RESET_ON_BOOT` drops every table and empties the document
    ///   bucket. It has been observed set in a real production
    ///   environment, where the next redeploy would have destroyed all
    ///   customer data; an `https://` BASE_URL is a reliable "this is
    ///   not a laptop" signal.
    pub(crate) fn assert_safe_for_deployment(&self) -> anyhow::Result<()> {
        const DEV_SECRETS: &[&str] = &[
            "dev-only-secret-change-me-change-me-change-me-change-me",
            "change-me-to-a-long-random-secret-please-please-please",
            "test-secret-test-secret-test-secret-test-secret",
        ];
        if DEV_SECRETS.contains(&self.jwt_secret.as_str()) || self.jwt_secret.contains("change-me")
        {
            anyhow::bail!(
                "JWT_SECRET is still set to a publicly-known development value. \
                 Generate a unique secret (e.g. `openssl rand -base64 48`) and set \
                 JWT_SECRET before starting."
            );
        }
        if self.jwt_secret.len() < 32 {
            anyhow::bail!(
                "JWT_SECRET must be at least 32 characters (got {}). \
                 Generate one with `openssl rand -base64 48`.",
                self.jwt_secret.len()
            );
        }
        if self.dev_reset_on_boot && self.base_url.starts_with("https://") {
            anyhow::bail!(
                "DEV_RESET_ON_BOOT is set on an https:// deployment ({}). That would \
                 delete every user, brokerage, transaction, and document, and empty \
                 the storage bucket. Remove DEV_RESET_ON_BOOT from this environment.",
                self.base_url
            );
        }
        Ok(())
    }
}
/// Read `preferred`, falling back to `legacy`, then to `default`.
///
/// Lets the storage settings use provider-neutral `S3_*` names without
/// breaking deployments still setting the original `RUSTFS_*` ones —
/// the object store is any S3-compatible service, not specifically
/// RustFS.
fn env_either(preferred: &str, legacy: &str, default: &str) -> String {
    pick_env(env::var(preferred).ok(), env::var(legacy).ok(), default)
}

/// The precedence rule behind [`env_either`], split out so it can be
/// tested without mutating the process environment (which this crate
/// forbids, and which would race the multi-threaded test binary anyway).
///
/// An empty string counts as unset: a Dokploy field left blank should
/// fall through rather than blank out a value the legacy name supplies.
fn pick_env(preferred: Option<String>, legacy: Option<String>, default: &str) -> String {
    preferred
        .filter(|v| !v.is_empty())
        .or_else(|| legacy.filter(|v| !v.is_empty()))
        .unwrap_or_else(|| default.to_string())
}

fn env_or(key: &str, default: &str) -> String {
    env::var(key).unwrap_or_else(|_| default.to_string())
}

fn env_flag(key: &str, default: bool) -> bool {
    env::var(key)
        .ok()
        .map(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(default)
}

#[cfg(test)]
mod env_naming_tests {
    use super::pick_env;

    fn v(s: &str) -> Option<String> {
        Some(s.to_string())
    }

    /// The storage settings moved to provider-neutral `S3_*` names, but
    /// the dev `docker-compose.yml` still passes the original `RUSTFS_*`
    /// ones. If the fallback broke, `docker compose up` would quietly
    /// ignore the rustfs container and use the built-in localhost
    /// defaults instead.
    #[test]
    fn legacy_storage_env_names_still_win_over_the_default() {
        // Neither set → default.
        assert_eq!(pick_env(None, None, "fallback"), "fallback");

        // Only the legacy name → legacy value. This is the dev-compose case.
        assert_eq!(
            pick_env(None, v("http://rustfs:9000"), "fallback"),
            "http://rustfs:9000",
            "docker-compose.yml sets only RUSTFS_* and must keep working"
        );

        // Both set → the new name wins.
        assert_eq!(
            pick_env(
                v("https://eu2.contabostorage.com"),
                v("http://rustfs:9000"),
                "fallback"
            ),
            "https://eu2.contabostorage.com"
        );

        // Empty counts as unset on either side.
        assert_eq!(
            pick_env(v(""), v("http://rustfs:9000"), "fallback"),
            "http://rustfs:9000"
        );
        assert_eq!(pick_env(v(""), v(""), "fallback"), "fallback");
    }
}
