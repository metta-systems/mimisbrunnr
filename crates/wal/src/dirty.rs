//! Journal pin / dirty-node bookkeeping. IMPL §3.4.
//!
//! Phase 2 scope: the struct shape only. The actual journal-reclaim driver
//! lives in a later phase.

use mimisbrunnr_storage::BlockRef;

/// In-memory pinning record for a dirty btree node.
///
/// IMPL §3.4: each in-memory dirty btree node carries a journal pin — the LSN
/// of the oldest WAL entry whose update has not yet been merged into the
/// on-disk node. Reclaim of the WAL ring is gated on
/// `min(all_dirty_nodes.pending_lsn_min)`.
///
/// `in_memory` deliberately stays opaque — Phase 2b crates (`meta`, `index`)
/// will land their own `NodeContent` types and carry them via a generic
/// parameter or trait. For now the struct just owns the pin metadata.
//
// TODO(rewrite-phase-N): journal-reclaim driver — choose flush victims by
// (pending_lsn_max - pending_lsn_min) * pending_count and rewrite via the
// COW path described in §3.4. This is the thread that advances
// `WalHeader.read_cursor` past flushed pages.
#[derive(Debug, Clone)]
pub struct DirtyNode {
    /// On-disk location of the node.
    pub block_ref: BlockRef,
    /// `BlockHeader.lsn` of the on-disk version.
    pub last_persisted_lsn: u64,
    /// Oldest WAL entry pinning this node.
    pub pending_lsn_min: u64,
    /// Newest WAL entry pinning this node.
    pub pending_lsn_max: u64,
    /// Count of WAL entries waiting to merge.
    pub pending_count: u32,
}

impl DirtyNode {
    /// Construct an empty pin: no pending entries yet, `last_persisted_lsn`
    /// matches the on-disk node's `BlockHeader.lsn`.
    pub fn new(block_ref: BlockRef, last_persisted_lsn: u64) -> Self {
        Self {
            block_ref,
            last_persisted_lsn,
            pending_lsn_min: 0,
            pending_lsn_max: 0,
            pending_count: 0,
        }
    }

    /// Add a pending WAL entry. First pending entry sets `pending_lsn_min`;
    /// every subsequent entry bumps `pending_lsn_max` and the count.
    pub fn pin(&mut self, lsn: u64) {
        if self.pending_count == 0 {
            self.pending_lsn_min = lsn;
        }
        self.pending_lsn_max = lsn;
        self.pending_count += 1;
    }

    /// Returns `true` when no entries are pending.
    pub fn is_clean(&self) -> bool {
        self.pending_count == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dirty_node_pinning_tracks_lsn_window() {
        let mut node = DirtyNode::new(BlockRef::new(0, 100, 1), 5);
        assert!(node.is_clean());
        node.pin(10);
        node.pin(11);
        node.pin(20);
        assert_eq!(node.pending_lsn_min, 10);
        assert_eq!(node.pending_lsn_max, 20);
        assert_eq!(node.pending_count, 3);
        assert!(!node.is_clean());
    }
}
