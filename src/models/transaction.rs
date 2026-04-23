//! `transaction` table — one real estate deal, the unit of compliance work.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use surrealdb::types::{RecordId, SurrealValue};

use crate::record_key;

/// Lifecycle of a deal. Stored on the row as a lowercase string via `as_str`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransactionStatus {
    Open,
    UnderContract,
    Closed,
    Cancelled,
}

#[allow(dead_code)]
impl TransactionStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            TransactionStatus::Open => "open",
            TransactionStatus::UnderContract => "under_contract",
            TransactionStatus::Closed => "closed",
            TransactionStatus::Cancelled => "cancelled",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            TransactionStatus::Open => "Open",
            TransactionStatus::UnderContract => "Under Contract",
            TransactionStatus::Closed => "Closed",
            TransactionStatus::Cancelled => "Cancelled",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "open" => Some(Self::Open),
            "under_contract" => Some(Self::UnderContract),
            "closed" => Some(Self::Closed),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }

    pub fn all() -> [Self; 4] {
        [Self::Open, Self::UnderContract, Self::Closed, Self::Cancelled]
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, SurrealValue)]
pub struct Transaction {
    pub id: RecordId,
    pub property_address: String,
    pub city: String,
    pub state: String,
    pub postal_code: Option<String>,
    pub price_cents: i64,
    pub buyer_name: Option<String>,
    pub seller_name: Option<String>,
    pub expected_close: Option<DateTime<Utc>>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Transaction {
    pub fn status_enum(&self) -> TransactionStatus {
        TransactionStatus::parse(&self.status).unwrap_or(TransactionStatus::Open)
    }

    /// Human-friendly price for templates. We store cents as `i64` to avoid
    /// float drift; formatting is a view concern.
    pub fn price_display(&self) -> String {
        if self.price_cents <= 0 {
            return "—".into();
        }
        let dollars = self.price_cents / 100;
        let s = dollars.to_string();
        let chars: Vec<char> = s.chars().collect();
        let len = chars.len();
        let mut out = String::with_capacity(len + len / 3 + 1);
        out.push('$');
        chars.iter().enumerate().for_each(|(i, c)| {
            out.push(*c);
            let remaining = len - i - 1;
            if remaining > 0 && remaining % 3 == 0 {
                out.push(',');
            }
        });
        out
    }

    pub fn status_label(&self) -> &'static str {
        self.status_enum().label()
    }

    /// Stable string key extracted from the `RecordId`, suitable for URLs.
    pub fn url_key(&self) -> String {
        record_key(&self.id)
    }

    /// Stable CSS hook for status colouring; matches the variants above.
    pub fn status_class(&self) -> &'static str {
        match self.status_enum() {
            TransactionStatus::Open => "status-open",
            TransactionStatus::UnderContract => "status-contract",
            TransactionStatus::Closed => "status-closed",
            TransactionStatus::Cancelled => "status-cancelled",
        }
    }
}

#[derive(Debug, Clone, Serialize, SurrealValue)]
pub struct NewTransaction {
    pub property_address: String,
    pub city: String,
    pub state: String,
    pub postal_code: Option<String>,
    pub price_cents: i64,
    pub buyer_name: Option<String>,
    pub seller_name: Option<String>,
    pub expected_close: Option<DateTime<Utc>>,
    pub status: String,
}
