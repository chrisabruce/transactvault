//! `brokerage` table — the tenant account.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use surrealdb::types::{RecordId, SurrealValue};

#[derive(Debug, Clone, Serialize, Deserialize, SurrealValue)]
pub struct Brokerage {
    pub id: RecordId,
    pub name: String,
    pub city: Option<String>,
    pub state: String,
    pub plan: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, SurrealValue)]
pub struct NewBrokerage {
    pub name: String,
    pub city: Option<String>,
}
