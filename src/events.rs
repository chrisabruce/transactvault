//! In-process pub/sub for live dashboard updates.
//!
//! Mutating HTTP handlers publish an [`Event`] after they finish so
//! that any open SSE stream subscribed to the affected brokerage (or
//! user) can react. The bus is a single [`tokio::sync::broadcast`]
//! channel; subscribers each filter incoming events down to the ones
//! that apply to their session.
//!
//! Why not per-brokerage channels? With the typical brokerage count
//! (~hundreds) and event rate (~ones-per-minute per active brokerage),
//! one global channel with cheap filtering in the subscriber is
//! simpler and avoids managing a `DashMap<BrokerageId, Sender>` and
//! its eviction. Switch to sharded channels later if profiling shows
//! the filter is hot.
//!
//! The channel is **bounded** (`broadcast::channel(256)`). A slow
//! subscriber that falls behind gets a `Lagged` error from `recv()`;
//! they handle that by re-running their full render from current state
//! instead of trying to replay missed events. Stats are idempotent —
//! the only cost of a missed event is a one-recompute delay.

use surrealdb::types::RecordId;
use tokio::sync::broadcast;

/// Anything worth telling a live dashboard about.
#[derive(Debug, Clone)]
pub enum Event {
    /// Some state inside this brokerage just changed in a way that
    /// could shift a Needs Attention number — an approval, denial,
    /// upload, comment, status change, reassign, or transaction
    /// create/delete. Published from the mutating handler that did
    /// the change.
    BrokerageMutation(RecordId),

    /// This user's brokerage membership changed: switched brokerage,
    /// promoted/demoted, or removed entirely. Live SSE streams owned
    /// by this user must drop their connection so the next reconnect
    /// picks up the new role + brokerage via the `CurrentUser`
    /// extractor. Without this, a demoted broker could keep watching
    /// dashboard numbers their new role isn't authorized to see until
    /// they happen to reload the page.
    UserMembershipChanged(RecordId),
}

/// Cheap-to-clone handle to the brokerage event bus. Lives on
/// [`crate::state::AppState`] alongside the DB handle; every place
/// that publishes does so through this handle, and every SSE
/// subscriber gets a fresh `broadcast::Receiver` from
/// [`Self::subscribe`].
#[derive(Clone)]
pub struct Events {
    tx: broadcast::Sender<Event>,
}

impl Events {
    /// Build a new bus with a 256-slot ring buffer. Re-tune later if
    /// burst behavior justifies it — the realistic upper bound on
    /// events per second is single digits across the whole tenant
    /// set, so 256 is comfortably more than any subscriber's worst
    /// case before they hit `Lagged` and re-sync.
    pub fn new() -> Self {
        let (tx, _rx) = broadcast::channel(256);
        Self { tx }
    }

    /// Fire-and-forget publish. `send` only errors when no receivers
    /// exist — totally fine, just means nothing's listening right
    /// now.
    pub fn publish(&self, event: Event) {
        let _ = self.tx.send(event);
    }

    /// New receiver. Receivers see events sent *after* they subscribe;
    /// any events published before are gone.
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.tx.subscribe()
    }
}

impl Default for Events {
    fn default() -> Self {
        Self::new()
    }
}
