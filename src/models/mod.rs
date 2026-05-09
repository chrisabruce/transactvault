//! Persistent domain models. Each entity has a view struct used for reads,
//! and where appropriate a separate `New*` struct for inserts so we never
//! have to worry about server-generated fields (IDs, timestamps) being
//! accidentally sent from the client.

pub mod audit;
pub mod brokerage;
pub mod checklist;
pub mod comment;
pub mod document;
pub mod invitation;
pub mod transaction;
pub mod user;

pub use audit::{AuditEvent, NewAuditEvent};
pub use brokerage::{Brokerage, NewBrokerage};
pub use checklist::{ApprovalStatus, ChecklistItem, NewChecklistItem};
pub use comment::{Comment, NewComment};
pub use document::{Document, NewDocument};
pub use invitation::{Invitation, NewInvitation};
pub use transaction::{
    NewTransaction, SalesType, SpecialSalesCondition, Transaction, TransactionStatus,
    TransactionType,
};
#[allow(unused_imports)]
pub use user::{NewUser, User, UserProfile};
