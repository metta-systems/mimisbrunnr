//! Helpers around [`WatchEvent`] (the logical type lives in
//! `mimisbrunnr-types`).

use mimisbrunnr_types::{HybridTimestamp, ObjectId, WatchEvent};

/// Extract the object id from any [`WatchEvent`] variant.
pub fn event_object_id(ev: &WatchEvent) -> ObjectId {
    match ev {
        WatchEvent::Entered { oid, .. }
        | WatchEvent::Exited { oid, .. }
        | WatchEvent::TagAdded { oid, .. }
        | WatchEvent::TagRemoved { oid, .. }
        | WatchEvent::ContentChanged { oid, .. }
        | WatchEvent::Deleted { oid, .. }
        | WatchEvent::Created { oid, .. } => *oid,
    }
}

/// Extract the timestamp from any [`WatchEvent`] variant.
pub fn event_timestamp(ev: &WatchEvent) -> HybridTimestamp {
    match ev {
        WatchEvent::Entered { timestamp, .. }
        | WatchEvent::Exited { timestamp, .. }
        | WatchEvent::TagAdded { timestamp, .. }
        | WatchEvent::TagRemoved { timestamp, .. }
        | WatchEvent::ContentChanged { timestamp, .. }
        | WatchEvent::Deleted { timestamp, .. }
        | WatchEvent::Created { timestamp, .. } => *timestamp,
    }
}

/// Discriminant tag used by debounce coalescing — compares only "what kind
/// of event" rather than payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EventKind {
    Entered,
    Exited,
    TagAdded,
    TagRemoved,
    ContentChanged,
    Deleted,
    Created,
}

impl From<&WatchEvent> for EventKind {
    fn from(ev: &WatchEvent) -> Self {
        match ev {
            WatchEvent::Entered { .. } => Self::Entered,
            WatchEvent::Exited { .. } => Self::Exited,
            WatchEvent::TagAdded { .. } => Self::TagAdded,
            WatchEvent::TagRemoved { .. } => Self::TagRemoved,
            WatchEvent::ContentChanged { .. } => Self::ContentChanged,
            WatchEvent::Deleted { .. } => Self::Deleted,
            WatchEvent::Created { .. } => Self::Created,
        }
    }
}
