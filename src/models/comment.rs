//! Free-text comments attachable to a transaction or a checklist item.
//!
//! A single `comment` table backs both surfaces. The `target` field is a
//! polymorphic record link (validated server-side to point at one of the
//! two table types) so we can hydrate either thread with a single query.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use surrealdb::types::{RecordId, SurrealValue};

/// One free-text note on either a transaction or a checklist_item.
/// The `target` is a polymorphic record link constrained at the schema
/// level to those two table types only; pick the renderer by inspecting
/// `target.tb()`.
#[derive(Debug, Clone, Serialize, Deserialize, SurrealValue)]
pub struct Comment {
    pub id: RecordId,
    pub body: String,
    /// Polymorphic foreign key — either `transaction:<key>` or
    /// `checklist_item:<key>`. The schema's ASSERT clause rejects
    /// anything else.
    pub target: RecordId,
    pub author: RecordId,
    /// Set when the comment was emitted by the upload handler to flag
    /// a prior document version (e.g. "Uploaded v3 — replaces v2 of
    /// foo.pdf"). Templates use it to render an inline doc-link badge.
    pub references_document: Option<RecordId>,
    pub created_at: DateTime<Utc>,
}

/// Insert shape used by the comment / deny / upload handlers.
#[derive(Debug, Clone, Serialize, SurrealValue)]
pub struct NewComment {
    pub body: String,
    pub target: RecordId,
    pub author: RecordId,
    pub references_document: Option<RecordId>,
}
