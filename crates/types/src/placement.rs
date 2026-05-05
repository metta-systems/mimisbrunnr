//! Placement rules and chunking parameters (DESIGN §8.3).
//!
//! Placement rules bind semantic queries to physical topology — *and* to the
//! granularity at which objects are stored. Chunking is a placement decision
//! (single contiguous extent vs. content-defined chunks).

use serde::{Deserialize, Serialize};

use crate::{disk::StorageTier, query::Query};

/// Content-defined chunking algorithm. Discriminants are pinned by DESIGN
/// §8.3 and travel into on-disk superblock defaults — do not renumber.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum ChunkingAlgo {
    /// Do not chunk; store the object as one contiguous extent. The
    /// format-time default and the explicit "force unchunked" admin override.
    None = 0,
    /// Fixed-size chunks. Cheap, gives resumability without the CDC cost.
    FixedSize = 1,
    /// Content-defined; default for the chunked path.
    FastCDC = 2,
    // 3+ reserved for future algorithms.
}

impl ChunkingAlgo {
    pub const fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::None),
            1 => Some(Self::FixedSize),
            2 => Some(Self::FastCDC),
            _ => None,
        }
    }
}

/// Parameters for chunking a single object.
///
/// `min_size` / `avg_size` / `max_size` are plaintext byte targets. They are
/// ignored when `algo == ChunkingAlgo::None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkParams {
    pub algo: ChunkingAlgo,
    pub min_size: u32,
    pub avg_size: u32,
    pub max_size: u32,
}

impl Default for ChunkParams {
    fn default() -> Self {
        // Match DESIGN §8.4 defaults for FastCDC.
        Self {
            algo: ChunkingAlgo::None,
            min_size: 16 * 1024,
            avg_size: 64 * 1024,
            max_size: 256 * 1024,
        }
    }
}

/// Admin-time placement rule. Evaluated against an object's tag set at
/// write-/migration-time to produce the storage decision for that object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PlacementRule {
    /// Hard pin: matching objects must live on the named tier.
    Pin { query: Query, tier: StorageTier },
    /// Soft preference: weighted choice. Higher `priority` outranks lower.
    Prefer {
        query: Query,
        tier: StorageTier,
        priority: u8,
    },
    /// Replicate matching objects across `min_replicas` copies.
    Replicate {
        query: Query,
        min_replicas: u8,
        across_disks: bool,
    },
    /// Co-locate matching objects on a single disk for locality.
    Colocate { query: Query },
    /// Auto-tier by access age.
    AutoTier {
        hot_threshold_days: u32,
        warm_threshold_days: u32,
        cold_after: u32,
    },
    /// Override chunking decisions.
    Chunk { query: Query, params: ChunkParams },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::TagId;

    #[test]
    fn chunking_algo_discriminants() {
        assert_eq!(ChunkingAlgo::None as u8, 0);
        assert_eq!(ChunkingAlgo::FixedSize as u8, 1);
        assert_eq!(ChunkingAlgo::FastCDC as u8, 2);
        assert_eq!(ChunkingAlgo::from_u8(0), Some(ChunkingAlgo::None));
        assert_eq!(ChunkingAlgo::from_u8(2), Some(ChunkingAlgo::FastCDC));
        assert_eq!(ChunkingAlgo::from_u8(99), None);
    }

    #[test]
    fn placement_rule_round_trips() {
        let rule = PlacementRule::Pin {
            query: Query::HasTag(TagId::new(7)),
            tier: StorageTier::Cold,
        };
        let mut buf = Vec::new();
        ciborium::ser::into_writer(&rule, &mut buf).unwrap();
        let back: PlacementRule = ciborium::de::from_reader(buf.as_slice()).unwrap();
        assert_eq!(rule, back);
    }

    #[test]
    fn chunk_rule_round_trip() {
        let rule = PlacementRule::Chunk {
            query: Query::HasTag(TagId::new(1)),
            params: ChunkParams {
                algo: ChunkingAlgo::FastCDC,
                min_size: 16 * 1024,
                avg_size: 64 * 1024,
                max_size: 256 * 1024,
            },
        };
        let mut buf = Vec::new();
        ciborium::ser::into_writer(&rule, &mut buf).unwrap();
        let back: PlacementRule = ciborium::de::from_reader(buf.as_slice()).unwrap();
        assert_eq!(rule, back);
    }
}
