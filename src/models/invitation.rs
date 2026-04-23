//! `invitation` table — a pending seat in a brokerage. Accepting an invite
//! creates the `user` row and the `works_at` edge in one step.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use surrealdb::types::{RecordId, SurrealValue};

#[derive(Debug, Clone, Serialize, Deserialize, SurrealValue)]
pub struct Invitation {
    pub id: RecordId,
    pub email: String,
    pub role: String,
    pub token: String,
    pub brokerage: RecordId,
    pub invited_by: RecordId,
    pub accepted: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, SurrealValue)]
pub struct NewInvitation {
    pub email: String,
    pub role: String,
    pub token: String,
    pub brokerage: RecordId,
    pub invited_by: RecordId,
}
