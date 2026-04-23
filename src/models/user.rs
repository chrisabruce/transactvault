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
}
