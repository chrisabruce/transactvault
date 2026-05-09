//! JWT claim structure and the resolved `CurrentUser` context handed to
//! controllers. Keeping the two apart lets middleware hit the database once
//! per request to look up the user's role + brokerage, rather than forcing
//! every handler to re-query.

use serde::{Deserialize, Serialize};
use surrealdb::types::RecordId;

/// JWT payload. Only the user's record key lives in the token — the rest of
/// the profile is looked up server-side each request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    pub sub: String,
    pub iat: usize,
    pub exp: usize,
}

impl Claims {
    /// Rebuild the full `RecordId` from the token subject.
    pub fn user_id(&self) -> RecordId {
        RecordId::new("user", self.sub.as_str())
    }
}

/// The caller's role within their brokerage. Drives visibility across the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Broker,
    Agent,
    Coordinator,
}

impl Role {
    /// Can this role see every transaction in the brokerage, or only their own?
    pub fn sees_all_transactions(self) -> bool {
        matches!(self, Role::Broker | Role::Coordinator)
    }

    /// Only brokers can invite teammates and manage the brokerage itself.
    pub fn is_broker(self) -> bool {
        matches!(self, Role::Broker)
    }

    /// Brokers and TCs can approve/deny checklist items.
    pub fn can_review(self) -> bool {
        matches!(self, Role::Broker | Role::Coordinator)
    }

    /// Only brokers can change other members' roles.
    pub fn can_change_roles(self) -> bool {
        matches!(self, Role::Broker)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Role::Broker => "broker",
            Role::Agent => "agent",
            Role::Coordinator => "coordinator",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Role::Broker => "Broker",
            Role::Agent => "Agent",
            Role::Coordinator => "Transaction Coordinator",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "broker" => Some(Role::Broker),
            "agent" => Some(Role::Agent),
            "coordinator" => Some(Role::Coordinator),
            _ => None,
        }
    }
}

/// Fully resolved principal: who they are, where they work, and what they can do.
#[derive(Debug, Clone)]
pub struct CurrentUser {
    pub user_id: RecordId,
    pub brokerage_id: RecordId,
    pub email: String,
    pub name: String,
    pub role: Role,
}
