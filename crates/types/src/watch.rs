//! Subscription / watch types (DESIGN §11.2).
//!
//! Only the data shapes — the engine, oplog cursor management, and inverted
//! tag→subscription index live in `mimisbrunnr-watch`.

use serde::{Deserialize, Serialize};

use crate::{ObjectId, ids::TagId, timestamp::HybridTimestamp};

/// Bitflag set indicating which kinds of changes a subscription wants
/// notified about. Mirrors the `inotify`-style "what events do I care
/// about" mask, but expressed over query-defined object sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct ChangeInterest(pub u32);

impl ChangeInterest {
    pub const TAG_ADDED: Self = Self(1 << 0);
    pub const TAG_REMOVED: Self = Self(1 << 1);
    pub const CONTENT_CHANGED: Self = Self(1 << 2);
    pub const CREATED: Self = Self(1 << 3);
    pub const DELETED: Self = Self(1 << 4);
    /// Object newly satisfies the subscription's query.
    pub const ENTERED: Self = Self(1 << 5);
    /// Object no longer satisfies the subscription's query.
    pub const EXITED: Self = Self(1 << 6);

    /// Subscribe to every event type.
    pub const ALL: Self = Self(
        Self::TAG_ADDED.0
            | Self::TAG_REMOVED.0
            | Self::CONTENT_CHANGED.0
            | Self::CREATED.0
            | Self::DELETED.0
            | Self::ENTERED.0
            | Self::EXITED.0,
    );

    /// `true` iff every flag in `mask` is set in `self`.
    pub const fn contains(self, mask: Self) -> bool {
        (self.0 & mask.0) == mask.0
    }

    /// Bitwise union.
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Empty interest set (subscription receives nothing).
    pub const fn empty() -> Self {
        Self(0)
    }
}

impl std::ops::BitOr for ChangeInterest {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        self.union(rhs)
    }
}

impl std::ops::BitOrAssign for ChangeInterest {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

/// Lifecycle of a subscription. A `Dormant` subscription keeps its cursor
/// but generates no events until reactivated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SubscriptionState {
    Active,
    Dormant,
}

/// One delivered event. Carries the originating object, the originator
/// timestamp (HLC), and any per-variant context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WatchEvent {
    /// Object newly entered the subscription's query result set.
    Entered {
        oid: ObjectId,
        timestamp: HybridTimestamp,
    },
    /// Object left the subscription's query result set.
    Exited {
        oid: ObjectId,
        timestamp: HybridTimestamp,
    },
    TagAdded {
        oid: ObjectId,
        tag: TagId,
        timestamp: HybridTimestamp,
    },
    TagRemoved {
        oid: ObjectId,
        tag: TagId,
        timestamp: HybridTimestamp,
    },
    ContentChanged {
        oid: ObjectId,
        timestamp: HybridTimestamp,
    },
    Deleted {
        oid: ObjectId,
        timestamp: HybridTimestamp,
    },
    Created {
        oid: ObjectId,
        timestamp: HybridTimestamp,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn change_interest_bit_ops() {
        let m = ChangeInterest::TAG_ADDED | ChangeInterest::DELETED;
        assert!(m.contains(ChangeInterest::TAG_ADDED));
        assert!(m.contains(ChangeInterest::DELETED));
        assert!(!m.contains(ChangeInterest::ENTERED));
        assert!(ChangeInterest::ALL.contains(m));
        assert_eq!(ChangeInterest::empty().0, 0);
    }

    #[test]
    fn watch_event_round_trip() {
        let ev = WatchEvent::Entered {
            oid: ObjectId::from_parts(1, 42),
            timestamp: HybridTimestamp::new(1_000_000, 0, 1),
        };
        let mut buf = Vec::new();
        ciborium::ser::into_writer(&ev, &mut buf).unwrap();
        let back: WatchEvent = ciborium::de::from_reader(buf.as_slice()).unwrap();
        assert_eq!(ev, back);
    }
}
