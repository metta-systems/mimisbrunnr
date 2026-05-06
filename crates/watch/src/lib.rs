//! `mimisbrunnr-watch` — query subscription engine (DESIGN §11).
//!
//! Owns the runtime [`SubscriptionEngine`] (an inverted `tag → [sub_id]`
//! index plus per-subscription pending event queues) and the
//! [`Subscription`] record. The logical types — `WatchEvent`,
//! `ChangeInterest`, `SubscriptionState` — live in `mimisbrunnr-types` per
//! `docs/REWRITE_CONTRACT.md` §3 and are re-exported here for convenience.
//!
//! ## What this crate does
//!
//! - Atomic subscribe + snapshot (DESIGN §11.4) via
//!   [`SubscriptionEngine::register`]; the caller (engine layer, Phase 5)
//!   passes in the precomputed initial result bitmap.
//! - Inverted-index maintenance (DESIGN §11.3) via
//!   [`extract_tags`] over the [`Query`](mimisbrunnr_types::Query) AST.
//! - Mutation hooks ([`SubscriptionEngine::on_tag_added`] /
//!   `on_tag_removed` / `on_object_created` / `on_object_deleted` /
//!   `on_content_changed`) that recompute membership and dispatch
//!   [`WatchEvent`](mimisbrunnr_types::WatchEvent)s, masked by
//!   [`ChangeInterest`](mimisbrunnr_types::ChangeInterest).
//! - Lifecycle (DESIGN §11.7): [`SubscriptionEngine::pause`] /
//!   [`SubscriptionEngine::resume`].
//! - Ontology-aware indexing (DESIGN §11.8) via
//!   [`SubscriptionEngine::refresh_ontology_links`].
//! - CBOR persistence shim ([`SubscriptionEngine::serialise`] /
//!   `deserialise`) — placeholder for the IMPL §10.2 B+ tree shape.
//!
//! ## What this crate does NOT do
//!
//! - It does not depend on `mimisbrunnr-query`. Initial-result computation
//!   and complex boolean re-evaluation are the engine layer's job — call
//!   [`SubscriptionEngine::set_membership`] to push a fresh bitmap.
//! - It does not replay the WAL for offline catch-up (DESIGN §11.5). The
//!   engine layer scans the WAL between `cursor + 1` and the current LSN
//!   and feeds reconstructed events back through the mutation hooks.
//!   `TODO(rewrite-phase-N): wire WAL scan in engine`.
//! - Full per-tick debounce flush is left as a TODO; the
//!   [`Subscription::debounce_ms`] field and
//!   [`SubscriptionEngine::tick`] entry point are present so callers can
//!   wire it up later without an API break.

#![forbid(unsafe_code)]

mod engine;
mod error;
mod event;
mod subscription;

pub use engine::{SubscriptionEngine, extract_tags};
pub use error::WatchError;
pub use event::{EventKind, event_object_id, event_timestamp};
pub use subscription::{Retention, Subscription};

// Re-export the logical types so consumers don't need a separate
// `mimisbrunnr-types` dependency for the watch surface.
pub use mimisbrunnr_types::{ChangeInterest, SubscriptionId, SubscriptionState, WatchEvent};
