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

    pub rustfs: RustFsConfig,
    pub email: EmailConfig,
    pub stripe: StripeConfig,
}

/// S3-compatible object storage settings (RustFS by default).
#[derive(Debug, Clone)]
pub struct RustFsConfig {
    pub endpoint: String,
    pub region: String,
    pub access_key: String,
    pub secret_key: String,
    pub bucket: String,
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
            pow_difficulty_bits: 0,
            signup_rate_per_hour: 1000,
            login_rate_per_quarter_hour: 1000,
            dev_reset_on_boot: false,
            rustfs: RustFsConfig {
                endpoint: "http://127.0.0.1:1".into(),
                region: "us-east-1".into(),
                access_key: "test".into(),
                secret_key: "test".into(),
                bucket: "test".into(),
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
        Ok(Self {
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

            rustfs: RustFsConfig {
                endpoint: env_or("RUSTFS_ENDPOINT", "http://127.0.0.1:37421"),
                region: env_or("RUSTFS_REGION", "us-east-1"),
                access_key: env_or("RUSTFS_ACCESS_KEY", "rustfsadmin"),
                secret_key: env_or("RUSTFS_SECRET_KEY", "rustfsadmin"),
                bucket: env_or("RUSTFS_BUCKET", "transactvault"),
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
        })
    }
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
