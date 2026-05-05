//! Foundational identifier types (DESIGN §2.1).
//!
//! Every other identifier in the system is built on top of these. The names
//! here are pinned by `docs/REWRITE_CONTRACT.md` §3 — Phase 1b's `storage`
//! crate references them directly.

use serde::{Deserialize, Serialize};

/// Cluster-wide node identifier. Top 16 bits of every [`crate::ObjectId`].
pub type NodeId = u16;

/// Disk identifier within a single pool.
pub type DiskId = u16;

/// Subscription identifier (DESIGN §11.2).
pub type SubscriptionId = u64;

/// Ontology module identifier (DESIGN §4) — e.g. `"systems.metta.music"`.
pub type ModuleId = String;

/// Tag identifier (DESIGN §2.1). Newtype wrapper around `u32` so that tags
/// can never be confused with raw integers in attribute values, object IDs,
/// or counters.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct TagId(pub u32);

impl TagId {
    /// Construct a tag id from a raw `u32`.
    #[inline]
    pub const fn new(raw: u32) -> Self {
        Self(raw)
    }

    /// Extract the raw `u32` value.
    #[inline]
    pub const fn raw(self) -> u32 {
        self.0
    }
}

impl std::fmt::Display for TagId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "tag:{}", self.0)
    }
}

impl From<u32> for TagId {
    #[inline]
    fn from(v: u32) -> Self {
        Self(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tagid_round_trip() {
        let t = TagId::new(0xdead_beef);
        assert_eq!(t.raw(), 0xdead_beef);
        assert_eq!(TagId::from(0xdead_beef_u32), t);
    }

    #[test]
    fn tagid_display() {
        assert_eq!(format!("{}", TagId::new(42)), "tag:42");
    }

    #[test]
    fn tagid_ordering() {
        let mut tags = [TagId::new(3), TagId::new(1), TagId::new(2)];
        tags.sort();
        assert_eq!(tags, [TagId::new(1), TagId::new(2), TagId::new(3)]);
    }
}
