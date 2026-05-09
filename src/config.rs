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

    pub rustfs: RustFsConfig,
    pub email: EmailConfig,
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

/// Resend transactional email settings. An empty `api_key` disables the
/// transport — messages are logged but not delivered.
#[derive(Debug, Clone)]
pub struct EmailConfig {
    pub api_key: String,
    pub from: String,
    pub reply_to: Option<String>,
}

impl EmailConfig {
    pub fn is_enabled(&self) -> bool {
        !self.api_key.is_empty()
    }
}

impl Config {
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

            rustfs: RustFsConfig {
                endpoint: env_or("RUSTFS_ENDPOINT", "http://127.0.0.1:37421"),
                region: env_or("RUSTFS_REGION", "us-east-1"),
                access_key: env_or("RUSTFS_ACCESS_KEY", "rustfsadmin"),
                secret_key: env_or("RUSTFS_SECRET_KEY", "rustfsadmin"),
                bucket: env_or("RUSTFS_BUCKET", "transactvault"),
            },
            email: EmailConfig {
                api_key: env_or("RESEND_API_KEY", ""),
                from: env_or(
                    "RESEND_FROM",
                    "TransactVault <no-reply@transactvault.example>",
                ),
                reply_to: env::var("RESEND_REPLY_TO").ok().filter(|s| !s.is_empty()),
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
