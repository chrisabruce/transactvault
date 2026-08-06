//! `passkey` table — WebAuthn credentials registered from the profile
//! page and used for one-tap sign-in.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use surrealdb::types::{RecordId, SurrealValue};

/// One registered authenticator. `credential` holds the serialized
/// [`webauthn_rs::prelude::Passkey`] (public key, signature counter,
/// backup flags); everything else is bookkeeping for lookup and for the
/// management list on the profile page.
#[derive(Debug, Clone, Serialize, Deserialize, SurrealValue)]
pub struct PasskeyRow {
    pub id: RecordId,
    pub user: RecordId,
    /// The WebAuthn user handle (a UUID string). Minted at the user's
    /// first registration and reused for every later passkey, so an
    /// authenticator can overwrite its own old entry instead of piling
    /// up duplicates.
    pub webauthn_id: String,
    /// base64url credential id — unique across the table; a sign-in
    /// response is matched to its row through this.
    pub cred_id: String,
    /// `serde_json`-encoded `webauthn_rs::prelude::Passkey`.
    pub credential: String,
    pub label: String,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, SurrealValue)]
pub struct NewPasskeyRow {
    pub user: RecordId,
    pub webauthn_id: String,
    pub cred_id: String,
    pub credential: String,
    pub label: String,
}

impl PasskeyRow {
    pub fn key(&self) -> String {
        crate::db::record_key(&self.id)
    }
}
