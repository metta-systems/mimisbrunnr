//! Placement-rule serialisation helpers.
//!
//! ## Persistence (R1b-10)
//!
//! On disk the rule set occupies one 256 KiB §1.5 B+ tree region of
//! [`BtreeKind::PlacementRules`]. The whole `Vec<PlacementRule>` is
//! materialised into a single CBOR-encoded sorted-run entry keyed by
//! `u32 snapshot` (always `0` today) via [`BtreeRegion::write_full`];
//! reload goes through [`BtreeRegion::read`]. The
//! [`encode_placement_rules`] / [`decode_placement_rules`] CBOR-blob
//! helpers are retained for one revision so callers using the raw
//! CBOR-blob path keep working while the engine migration lands.
//!
//! TODO(rewrite-phase-R1d): replace the single-entry blob with the IMPL
//! §10.4 native per-rule shape — a sorted run keyed by `(rule_id,
//! snapshot)` with one `PlacementRule` per entry. `RootPointer.placement_rules_root`
//! refers to this region.

use {
    mimisbrunnr_storage::{BlockDevice, BtreeKind, BtreeRegion, LoadedNode, SortedRun},
    mimisbrunnr_types::PlacementRule,
};

use crate::error::PoolError;

/// 256 KiB region size in bytes (matches the spec's 18-bit
/// `region_size_log2`). Engine code sizes the on-disk slot from this.
#[allow(dead_code)] // consumed by engine layout once R1b lands engine-side.
pub const PLACEMENT_RULES_REGION_SIZE: u64 = 256 * 1024;
const REGION_SIZE_LOG2: u8 = 18;

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

// ----------------------------------------------------------------
// R1b-10: §1.5 B+ tree persistence (single-entry CBOR run).
// ----------------------------------------------------------------

/// Build a [`LoadedNode`] containing a single sorted-run entry
/// `(snapshot=0, rules.to_vec())`. The node uses
/// [`BtreeKind::PlacementRules`] and the spec's 18-bit (256 KiB) region
/// size.
pub fn placement_rules_to_loaded_node(
    rules: &[PlacementRule],
) -> LoadedNode<u32, Vec<PlacementRule>> {
    let entries = vec![(0u32, rules.to_vec())];
    let mut node: LoadedNode<u32, Vec<PlacementRule>> =
        LoadedNode::new(BtreeKind::PlacementRules, 0, REGION_SIZE_LOG2);
    let run = SortedRun::from_sorted(0, 0, entries);
    node.sorted_runs.push(run);
    node.header.sorted_run_count = 1;
    node
}

/// Restore the rule list from a [`LoadedNode`] read via
/// [`BtreeRegion::read`]. Picks the entry under `snapshot = 0`; an empty
/// node returns `Vec::new()`.
pub fn placement_rules_from_loaded_node(
    node: &LoadedNode<u32, Vec<PlacementRule>>,
) -> Vec<PlacementRule> {
    for (k, v) in node.merge_iter() {
        if *k == 0 {
            return v.clone();
        }
    }
    Vec::new()
}

/// Write the rule list as a fresh 256 KiB region at byte `offset` on
/// `device`.
pub fn flush_placement_rules_to_region<D: BlockDevice>(
    rules: &[PlacementRule],
    device: &mut D,
    offset: u64,
) -> Result<(), PoolError> {
    let mut node = placement_rules_to_loaded_node(rules);
    BtreeRegion::write_full::<u32, Vec<PlacementRule>>(device, offset, &mut node)?;
    Ok(())
}

/// Read the rule list from the 256 KiB region at byte `offset` on
/// `device`. An all-zero region returns `Vec::new()`.
pub fn load_placement_rules_from_region<D: BlockDevice>(
    device: &mut D,
    offset: u64,
) -> Result<Vec<PlacementRule>, PoolError> {
    let mut probe = [0u8; 8];
    device.read_at(offset, &mut probe)?;
    if probe.iter().all(|&b| b == 0) {
        return Ok(Vec::new());
    }
    let node = BtreeRegion::read_as_loaded_node::<u32, Vec<PlacementRule>>(device, offset)?;
    Ok(placement_rules_from_loaded_node(&node))
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

    // ----- B+ tree region round-trip (R1b-10) -----

    use {mimisbrunnr_storage::FileBlockDevice, tempfile::TempDir};

    fn fresh_device() -> (TempDir, FileBlockDevice) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("placement_rules.bin");
        let dev = FileBlockDevice::open(&path, 1 << 20).unwrap();
        (dir, dev)
    }

    fn sample_rules() -> Vec<PlacementRule> {
        vec![
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
        ]
    }

    #[test]
    fn placement_rules_region_round_trip_empty_returns_empty() {
        let (_dir, dev) = fresh_device();
        let rules = load_placement_rules_from_region(&dev, 0).unwrap();
        assert!(rules.is_empty());
    }

    #[test]
    fn placement_rules_region_round_trip_preserves_rules() {
        let (_dir, dev) = fresh_device();
        let rules = sample_rules();
        flush_placement_rules_to_region(&rules, &dev, 0).unwrap();
        let back = load_placement_rules_from_region(&dev, 0).unwrap();
        assert_eq!(rules, back);
    }

    #[test]
    fn placement_rules_region_round_trip_single_rule() {
        let (_dir, dev) = fresh_device();
        let rules = vec![PlacementRule::Pin {
            query: Query::HasTag(TagId::new(42)),
            tier: StorageTier::Cold,
        }];
        flush_placement_rules_to_region(&rules, &dev, 0).unwrap();
        let back = load_placement_rules_from_region(&dev, 0).unwrap();
        assert_eq!(rules, back);
    }

    #[test]
    fn placement_rules_region_overwrite_replaces_state() {
        let (_dir, dev) = fresh_device();
        let first = sample_rules();
        flush_placement_rules_to_region(&first, &dev, 0).unwrap();

        let second = vec![PlacementRule::Pin {
            query: Query::HasTag(TagId::new(99)),
            tier: StorageTier::Hot,
        }];
        flush_placement_rules_to_region(&second, &dev, 0).unwrap();
        let back = load_placement_rules_from_region(&dev, 0).unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!(back, second);
    }
}
