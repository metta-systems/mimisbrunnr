//! Subscription / watch types (DESIGN §11.2).
//!
//! Only the data shapes — the engine, oplog cursor management, and inverted
//! tag→subscription index live in `mimisbrunnr-watch`.

use core::fmt;

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

impl fmt::Display for ChangeInterest {
    /// Pipe-separated mnemonic spelling, e.g. `"TAG_ADDED|DELETED"`. Empty
    /// interest sets render as `""`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        const FLAGS: &[(u32, &str)] = &[
            (1 << 0, "TAG_ADDED"),
            (1 << 1, "TAG_REMOVED"),
            (1 << 2, "CONTENT_CHANGED"),
            (1 << 3, "CREATED"),
            (1 << 4, "DELETED"),
            (1 << 5, "ENTERED"),
            (1 << 6, "EXITED"),
        ];
        let mut first = true;
        for (bit, name) in FLAGS {
            if (self.0 & bit) != 0 {
                if !first {
                    f.write_str("|")?;
                }
                first = false;
                f.write_str(name)?;
            }
        }
        Ok(())
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

impl WatchEvent {
    /// Discriminant name as a stable string. Used by [`Self::fmt`].
    pub const fn kind_str(&self) -> &'static str {
        match self {
            Self::Entered { .. } => "Entered",
            Self::Exited { .. } => "Exited",
            Self::TagAdded { .. } => "TagAdded",
            Self::TagRemoved { .. } => "TagRemoved",
            Self::ContentChanged { .. } => "ContentChanged",
            Self::Deleted { .. } => "Deleted",
            Self::Created { .. } => "Created",
        }
    }
}

impl fmt::Display for WatchEvent {
    /// Stable JSON representation of a watch event. Hand-formatted (no
    /// `serde_json` dep). Field order is fixed:
    /// `kind, oid, [tag,] timestamp{physical_ns, logical, node_id}`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (oid, tag, ts): (ObjectId, Option<TagId>, &HybridTimestamp) = match self {
            Self::Entered { oid, timestamp }
            | Self::Exited { oid, timestamp }
            | Self::ContentChanged { oid, timestamp }
            | Self::Deleted { oid, timestamp }
            | Self::Created { oid, timestamp } => (*oid, None, timestamp),
            Self::TagAdded { oid, tag, timestamp } | Self::TagRemoved { oid, tag, timestamp } => {
                (*oid, Some(*tag), timestamp)
            }
        };
        write!(f, "{{\"kind\":\"{}\"", self.kind_str())?;
        write!(f, ",\"oid\":{}", oid.to_u64())?;
        if let Some(t) = tag {
            write!(f, ",\"tag\":{}", t.raw())?;
        }
        write!(
            f,
            ",\"timestamp\":{{\"physical_ns\":{},\"logical\":{},\"node_id\":{}}}}}",
            ts.physical_ns, ts.logical, ts.node_id
        )
    }
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
    fn change_interest_display_pipes_flags() {
        let m = ChangeInterest::TAG_ADDED | ChangeInterest::DELETED;
        assert_eq!(m.to_string(), "TAG_ADDED|DELETED");
        let all = ChangeInterest::ALL;
        assert_eq!(
            all.to_string(),
            "TAG_ADDED|TAG_REMOVED|CONTENT_CHANGED|CREATED|DELETED|ENTERED|EXITED"
        );
        assert_eq!(ChangeInterest::empty().to_string(), "");
    }

    #[test]
    fn watch_event_display_is_stable_json() {
        let oid = ObjectId::from_parts(0, 42);
        let ts = HybridTimestamp::new(1_700_000_000_000, 7, 3);
        let entered = WatchEvent::Entered { oid, timestamp: ts };
        assert_eq!(
            entered.to_string(),
            "{\"kind\":\"Entered\",\"oid\":42,\"timestamp\":\
             {\"physical_ns\":1700000000000,\"logical\":7,\"node_id\":3}}"
        );

        let tag_added = WatchEvent::TagAdded {
            oid,
            tag: TagId::new(99),
            timestamp: ts,
        };
        assert_eq!(
            tag_added.to_string(),
            "{\"kind\":\"TagAdded\",\"oid\":42,\"tag\":99,\"timestamp\":\
             {\"physical_ns\":1700000000000,\"logical\":7,\"node_id\":3}}"
        );

        let deleted = WatchEvent::Deleted { oid, timestamp: ts };
        assert!(deleted.to_string().starts_with("{\"kind\":\"Deleted\""));
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
