//! Free-text comments attachable to a transaction or a checklist item.
//!
//! A single `comment` table backs both surfaces. The `target` field is a
//! polymorphic record link (validated server-side to point at one of the
//! two table types) so we can hydrate either thread with a single query.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use surrealdb::types::{RecordId, SurrealValue};

#[derive(Debug, Clone, Serialize, Deserialize, SurrealValue)]
pub struct Comment {
    pub id: RecordId,
    pub body: String,
    pub target: RecordId,
    pub author: RecordId,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, SurrealValue)]
pub struct NewComment {
    pub body: String,
    pub target: RecordId,
    pub author: RecordId,
}
