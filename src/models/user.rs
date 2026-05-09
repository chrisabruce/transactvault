//! `user` table — authenticated principal.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use surrealdb::types::{RecordId, SurrealValue};

/// Full user row as stored in SurrealDB. Never render `password_hash` in
/// templates — use [`UserProfile`] for view layers.
#[derive(Debug, Clone, Serialize, Deserialize, SurrealValue)]
pub struct User {
    pub id: RecordId,
    pub email: String,
    pub name: String,
    pub password_hash: String,
    pub photo_url: Option<String>,

    /// Email verification gate — accounts start unverified and can't sign in
    /// until they click the verify link sent at signup.
    pub email_verified: bool,
    pub verification_token: Option<String>,
    pub verification_expires: Option<DateTime<Utc>>,

    /// Forensics — captured from the request that created or last touched
    /// the user. `last_login_at` is updated on each successful login.
    pub signup_ip: Option<String>,
    pub signup_user_agent: Option<String>,
    pub last_login_at: Option<DateTime<Utc>>,

    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Safe-to-expose projection. Keep this in sync with `user.password_hash`
/// staying put — templates should never touch the hash.
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize, SurrealValue)]
pub struct UserProfile {
    pub id: RecordId,
    pub email: String,
    pub name: String,
    pub photo_url: Option<String>,
}

impl From<User> for UserProfile {
    fn from(u: User) -> Self {
        Self {
            id: u.id,
            email: u.email,
            name: u.name,
            photo_url: u.photo_url,
        }
    }
}

/// Insert shape. The database fills in `id`, `created_at`, and `updated_at`.
#[derive(Debug, Clone, Serialize, SurrealValue)]
pub struct NewUser {
    pub email: String,
    pub name: String,
    pub password_hash: String,
    pub email_verified: bool,
    pub verification_token: Option<String>,
    pub verification_expires: Option<DateTime<Utc>>,
    pub signup_ip: Option<String>,
    pub signup_user_agent: Option<String>,
}
