//! Placement-rule serialisation helpers.
//!
//! Until the full `BtreeKind::PlacementRules` B+ tree (IMPL §10.4) lands,
//! placement rules are persisted as a CBOR-encoded `Vec<PlacementRule>`
//! blob. The blob lives in the pool-state region today; once the B+ tree
//! is wired this becomes the leaf-payload codec.
//
// TODO(rewrite-phase-N): persist via `BtreeKind::PlacementRules` B+ tree
// per IMPL §10.4; the PoolStateRoot stops carrying these as opaque CBOR.

use mimisbrunnr_types::PlacementRule;

use crate::error::PoolError;

/// Encoded placement-rule blob: opaque CBOR bytes ready to write into a
/// metadata-zone block.
pub type PlacementRulesBlob = Vec<u8>;

/// CBOR-encode the rule list.
pub fn encode_placement_rules(rules: &[PlacementRule]) -> Result<PlacementRulesBlob, PoolError> {
    let mut buf = Vec::new();
    ciborium::ser::into_writer(&rules.to_vec(), &mut buf)
        .map_err(|e| PoolError::CborEncode(e.to_string()))?;
    Ok(buf)
}

/// CBOR-decode a rule list blob.
pub fn decode_placement_rules(bytes: &[u8]) -> Result<Vec<PlacementRule>, PoolError> {
    ciborium::de::from_reader(bytes).map_err(|e| PoolError::CborDecode(e.to_string()))
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        mimisbrunnr_types::{ChunkParams, ChunkingAlgo, Query, StorageTier, TagId},
    };

    #[test]
    fn round_trip_empty() {
        let bytes = encode_placement_rules(&[]).unwrap();
        let back = decode_placement_rules(&bytes).unwrap();
        assert!(back.is_empty());
    }

    #[test]
    fn round_trip_mixed() {
        let rules = vec![
            PlacementRule::Pin {
                query: Query::HasTag(TagId::new(1)),
                tier: StorageTier::Hot,
            },
            PlacementRule::AutoTier {
                hot_threshold_days: 7,
                warm_threshold_days: 30,
                cold_after: 90,
            },
            PlacementRule::Chunk {
                query: Query::HasTag(TagId::new(2)),
                params: ChunkParams {
                    algo: ChunkingAlgo::FastCDC,
                    min_size: 16 * 1024,
                    avg_size: 64 * 1024,
                    max_size: 256 * 1024,
                },
            },
        ];
        let bytes = encode_placement_rules(&rules).unwrap();
        let back = decode_placement_rules(&bytes).unwrap();
        assert_eq!(rules, back);
    }
}
