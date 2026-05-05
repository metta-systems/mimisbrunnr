//! Globally unique [`ObjectId`] (DESIGN §2.1).
//!
//! Bit layout:
//! ```text
//! 63                           48 47                                       0
//! ┌─────────────────────────────┬──────────────────────────────────────────┐
//! │       node id (u16)         │              local seq (u48)             │
//! └─────────────────────────────┴──────────────────────────────────────────┘
//! ```
//!
//! 16-bit node prefix lets cluster nodes mint IDs with zero coordination, and
//! the 48-bit local sequence is large enough that exhaustion is not a
//! practical concern (281 trillion IDs per node).

use arbitrary_int::u48;
use bitbybit::bitfield;
use serde::{Deserialize, Serialize};

use crate::ids::NodeId;

/// Object identifier — 64 bits total, packed as `node:16 || local:48`.
#[bitfield(u64, default = 0)]
#[derive(PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ObjectId {
    /// Local sequence (low 48 bits).
    #[bits(0..=47, rw)]
    local: u48,
    /// Originating cluster node (high 16 bits).
    #[bits(48..=63, rw)]
    node: u16,
}

impl ObjectId {
    /// Construct from `(node, local)`. `local` is silently masked to 48 bits.
    #[inline]
    pub const fn from_parts(node: NodeId, local: u64) -> Self {
        let masked = local & 0x0000_ffff_ffff_ffff;
        Self::DEFAULT.with_node(node).with_local(u48::new(masked))
    }

    /// Convert to its raw `u64` representation.
    #[inline]
    pub const fn to_u64(self) -> u64 {
        self.raw_value()
    }

    /// Construct from a raw `u64`.
    #[inline]
    pub const fn from_u64(raw: u64) -> Self {
        Self::new_with_raw_value(raw)
    }

    /// Cluster node that minted this id.
    #[inline]
    pub const fn node_id(self) -> NodeId {
        self.node()
    }

    /// Local sequence — the low 48 bits, returned widened to `u64`.
    #[inline]
    pub const fn local_seq(self) -> u64 {
        self.local().value()
    }
}

impl std::fmt::Debug for ObjectId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ObjectId")
            .field("node", &self.node_id())
            .field("local", &self.local_seq())
            .finish()
    }
}

impl std::fmt::Display for ObjectId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "obj:{:x}:{}", self.node_id(), self.local_seq())
    }
}

impl Serialize for ObjectId {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        self.to_u64().serialize(ser)
    }
}

impl<'de> Deserialize<'de> for ObjectId {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        u64::deserialize(de).map(Self::from_u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_parts_to_u64() {
        let id = ObjectId::from_parts(0xabcd, 0x0000_0001_2345_6789);
        let raw = id.to_u64();
        let back = ObjectId::from_u64(raw);
        assert_eq!(back.node_id(), 0xabcd);
        assert_eq!(back.local_seq(), 0x0000_0001_2345_6789);
        assert_eq!(id, back);
    }

    #[test]
    fn raw_layout_matches_spec() {
        // node=0x1234 occupies bits 48..64, local=0x5_6789_abcd_ef occupies
        // bits 0..48. Packed u64 = (node as u64 << 48) | local.
        let id = ObjectId::from_parts(0x1234, 0x0567_89ab_cdef);
        let expected: u64 = (0x1234u64 << 48) | 0x0567_89ab_cdef;
        assert_eq!(id.to_u64(), expected);
    }

    #[test]
    fn local_high_bits_are_masked() {
        // Construct with a >48-bit local; the 16 high bits should be discarded
        // rather than corrupting the node field.
        let id = ObjectId::from_parts(7, u64::MAX);
        assert_eq!(id.node_id(), 7);
        assert_eq!(id.local_seq(), 0x0000_ffff_ffff_ffff);
    }

    #[test]
    fn ordering_by_node_then_local() {
        let a = ObjectId::from_parts(1, 100);
        let b = ObjectId::from_parts(1, 200);
        let c = ObjectId::from_parts(2, 0);
        assert!(a < b);
        assert!(b < c);
    }

    #[test]
    fn serde_round_trip_via_cbor() {
        let id = ObjectId::from_parts(42, 1_000_000);
        let mut buf = Vec::new();
        ciborium::ser::into_writer(&id, &mut buf).unwrap();
        let back: ObjectId = ciborium::de::from_reader(buf.as_slice()).unwrap();
        assert_eq!(id, back);
    }
}
