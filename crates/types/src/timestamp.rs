use crate::NodeId;

/// Hybrid Logical Clock timestamp for total ordering without coordination.
///
/// Combines wall-clock milliseconds, a logical counter for same-ms ordering,
/// and the originating node ID for tie-breaking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HybridTimestamp {
    pub wall_ms: u64,
    pub logical: u16,
    pub node_id: NodeId,
}

impl HybridTimestamp {
    pub fn new(wall_ms: u64, logical: u16, node_id: NodeId) -> Self {
        Self {
            wall_ms,
            logical,
            node_id,
        }
    }

    pub fn zero() -> Self {
        Self {
            wall_ms: 0,
            logical: 0,
            node_id: 0,
        }
    }

    /// Pack into a u64 for compact storage.
    /// Layout: [wall_ms: 48 bits][logical: 16 bits]
    /// Node ID is stored separately when needed.
    pub fn pack_u64(&self) -> u64 {
        (self.wall_ms << 16) | (self.logical as u64)
    }

    /// Unpack from a u64 (node_id must be supplied separately).
    pub fn unpack_u64(packed: u64, node_id: NodeId) -> Self {
        Self {
            wall_ms: packed >> 16,
            logical: (packed & 0xFFFF) as u16,
            node_id,
        }
    }

    /// Advance the clock given the current wall time and a received timestamp.
    pub fn tick(&mut self, now_ms: u64) {
        if now_ms > self.wall_ms {
            self.wall_ms = now_ms;
            self.logical = 0;
        } else {
            self.logical = self.logical.saturating_add(1);
        }
    }

    /// Merge with a received timestamp (receive event in HLC protocol).
    pub fn recv(&mut self, now_ms: u64, other: &HybridTimestamp) {
        if now_ms > self.wall_ms && now_ms > other.wall_ms {
            self.wall_ms = now_ms;
            self.logical = 0;
        } else if self.wall_ms == other.wall_ms {
            self.logical = self.logical.max(other.logical) + 1;
        } else if other.wall_ms > self.wall_ms {
            self.wall_ms = other.wall_ms;
            self.logical = other.logical + 1;
        } else {
            self.logical += 1;
        }
    }
}

impl Ord for HybridTimestamp {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.wall_ms
            .cmp(&other.wall_ms)
            .then(self.logical.cmp(&other.logical))
            .then(self.node_id.cmp(&other.node_id))
    }
}

impl PartialOrd for HybridTimestamp {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl std::fmt::Display for HybridTimestamp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}@{}", self.wall_ms, self.logical, self.node_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordering_by_wall_time() {
        let a = HybridTimestamp::new(100, 0, 1);
        let b = HybridTimestamp::new(200, 0, 1);
        assert!(a < b);
    }

    #[test]
    fn ordering_by_logical() {
        let a = HybridTimestamp::new(100, 0, 1);
        let b = HybridTimestamp::new(100, 1, 1);
        assert!(a < b);
    }

    #[test]
    fn ordering_by_node() {
        let a = HybridTimestamp::new(100, 0, 1);
        let b = HybridTimestamp::new(100, 0, 2);
        assert!(a < b);
    }

    #[test]
    fn pack_unpack() {
        let ts = HybridTimestamp::new(1_000_000, 42, 7);
        let packed = ts.pack_u64();
        let unpacked = HybridTimestamp::unpack_u64(packed, 7);
        assert_eq!(ts, unpacked);
    }

    #[test]
    fn tick_advances_wall() {
        let mut ts = HybridTimestamp::new(100, 5, 1);
        ts.tick(200);
        assert_eq!(ts.wall_ms, 200);
        assert_eq!(ts.logical, 0);
    }

    #[test]
    fn tick_same_wall_increments_logical() {
        let mut ts = HybridTimestamp::new(100, 5, 1);
        ts.tick(100);
        assert_eq!(ts.wall_ms, 100);
        assert_eq!(ts.logical, 6);
    }

    #[test]
    fn tick_stale_wall_increments_logical() {
        let mut ts = HybridTimestamp::new(100, 5, 1);
        ts.tick(50);
        assert_eq!(ts.wall_ms, 100);
        assert_eq!(ts.logical, 6);
    }

    #[test]
    fn recv_merges_timestamps() {
        let mut local = HybridTimestamp::new(100, 3, 1);
        let remote = HybridTimestamp::new(150, 2, 2);
        local.recv(120, &remote);
        // Remote is ahead, so we adopt remote wall + increment logical
        assert_eq!(local.wall_ms, 150);
        assert_eq!(local.logical, 3);
    }

    #[test]
    fn recv_same_wall() {
        let mut local = HybridTimestamp::new(100, 3, 1);
        let remote = HybridTimestamp::new(100, 5, 2);
        local.recv(90, &remote);
        assert_eq!(local.wall_ms, 100);
        assert_eq!(local.logical, 6); // max(3,5) + 1
    }

    #[test]
    fn display() {
        let ts = HybridTimestamp::new(1000, 5, 3);
        assert_eq!(format!("{ts}"), "1000:5@3");
    }
}
