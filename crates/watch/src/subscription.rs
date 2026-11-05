//! Runtime [`Subscription`] record. SPECIFICATION-DESIGN§11.2.
//!
//! On disk each subscription is a CBOR value in the `BtreeKind::Subscriptions`
//! B+ tree (SUBSCRIPTIONS-DESIGN§1); see `SubscriptionEngine` for the flush.

use mimisbrunnr_types::{ChangeInterest, Query, SubscriptionId, SubscriptionState};
use roaring::RoaringBitmap;
use serde::{Deserialize, Serialize};

/// Retention policy for an offline (or slow) consumer's pending events.
///
/// SPECIFICATION-DESIGN§11.6 mentions "max events" / "coalescing"; we expose the storage
/// shape here. The consumer-facing `debounce_ms` lives on [`Subscription`]
/// directly to mirror the spec's per-subscription batching knob.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Retention {
    /// Drop events when the consumer can't keep up.
    AtMostOnce,
    /// Bounded ring buffer of `max_events`. Oldest events are dropped first.
    Bounded { max_events: u32 },
    /// Grow without bound — caller must drain.
    Unlimited,
}

impl Default for Retention {
    fn default() -> Self {
        Self::Bounded { max_events: 1024 }
    }
}

/// Live subscription record (SPECIFICATION-DESIGN§11.2).
#[derive(Debug, Clone)]
pub struct Subscription {
    pub id: SubscriptionId,
    pub name: String,
    pub query: Query,
    pub interest: ChangeInterest,
    /// LSN of the last event delivered to the consumer.
    pub cursor: u64,
    pub state: SubscriptionState,
    pub retention: Retention,
    /// Optional debounce window (milliseconds). If `Some(ms)`, repeated
    /// events for the same `oid` within `ms` are coalesced into a single
    /// event of the latest kind. The field is honoured by `tick()`
    /// but a full per-tick flush is left as a TODO.
    // TODO(SUBSCRIPTION4): wire full debounce flush in `tick()`.
    pub debounce_ms: Option<u32>,
    /// Current member set. Maintained by the engine on every mutation hook.
    pub cached_result: RoaringBitmap,
}

impl Subscription {
    /// Construct a fresh `Active` subscription.
    ///
    /// The engine layer is responsible for computing `cached_result` (the
    /// initial query evaluation) — SPECIFICATION-DESIGN§11.4 atomic subscribe + snapshot.
    pub fn new(
        id: SubscriptionId,
        name: String,
        query: Query,
        interest: ChangeInterest,
        retention: Retention,
        cursor: u64,
        cached_result: RoaringBitmap,
    ) -> Self {
        Self {
            id,
            name,
            query,
            interest,
            cursor,
            state: SubscriptionState::Active,
            retention,
            debounce_ms: None,
            cached_result,
        }
    }

    pub fn is_active(&self) -> bool {
        matches!(self.state, SubscriptionState::Active)
    }
}
