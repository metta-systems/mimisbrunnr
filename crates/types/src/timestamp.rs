//! Hybrid Logical Clock timestamp (DESIGN §10.4).
//!
//! 16-byte logical structure used wherever the spec calls for cluster-wide
//! ordering without coordination. The on-disk WAL header embeds the
//! identical byte layout (IMPL §3); this type exposes the *logical* shape
//! so other crates can construct/compare without reaching into the WAL crate.
//!
//! Total order: `physical_ns → logical → node_id`.

use serde::{Deserialize, Serialize};

use crate::ids::NodeId;

/// Hybrid Logical Clock timestamp. Layout (DESIGN §10.4):
///
/// | Bytes  | Field         | Type   |
/// |--------|---------------|--------|
/// | 0..8   | `physical_ns` | i64    |
/// | 8..10  | `logical`     | u16    |
/// | 10..12 | `node_id`     | NodeId |
/// | 12..16 | (pad)         | u32    |
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct HybridTimestamp {
    /// Monotonic wall-clock nanoseconds since the Unix epoch.
    pub physical_ns: i64,
    /// Same-tick disambiguation counter.
    pub logical: u16,
    /// Originating cluster node.
    pub node_id: NodeId,
}

impl HybridTimestamp {
    /// Construct a fully-specified timestamp.
    pub const fn new(physical_ns: i64, logical: u16, node_id: NodeId) -> Self {
        Self {
            physical_ns,
            logical,
            node_id,
        }
    }

    /// Zero / epoch timestamp from node 0. Useful as a sentinel.
    pub const fn zero() -> Self {
        Self {
            physical_ns: 0,
            logical: 0,
            node_id: 0,
        }
    }

    /// Local-event tick (HLC `send`/event step). Pulls the clock forward to
    /// `now_ns` if that is in the future; otherwise increments the logical
    /// counter to break ties.
    pub fn tick(&mut self, now_ns: i64) {
        if now_ns > self.physical_ns {
            self.physical_ns = now_ns;
            self.logical = 0;
        } else {
            self.logical = self.logical.saturating_add(1);
        }
    }

    /// HLC `recv` step: merge a received timestamp from another node.
    pub fn recv(&mut self, now_ns: i64, other: &HybridTimestamp) {
        if now_ns > self.physical_ns && now_ns > other.physical_ns {
            self.physical_ns = now_ns;
            self.logical = 0;
        } else if self.physical_ns == other.physical_ns {
            self.logical = self.logical.max(other.logical).saturating_add(1);
        } else if other.physical_ns > self.physical_ns {
            self.physical_ns = other.physical_ns;
            self.logical = other.logical.saturating_add(1);
        } else {
            self.logical = self.logical.saturating_add(1);
        }
    }
}

impl Ord for HybridTimestamp {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.physical_ns
            .cmp(&other.physical_ns)
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
        write!(f, "{}:{}@{}", self.physical_ns, self.logical, self.node_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordering_by_physical_then_logical_then_node() {
        let a = HybridTimestamp::new(100, 0, 1);
        let b = HybridTimestamp::new(200, 0, 1);
        let c = HybridTimestamp::new(200, 1, 1);
        let d = HybridTimestamp::new(200, 1, 2);
        assert!(a < b);
        assert!(b < c);
        assert!(c < d);
    }

    #[test]
    fn tick_advances_clock() {
        let mut ts = HybridTimestamp::new(100, 5, 1);
        ts.tick(200);
        assert_eq!(ts.physical_ns, 200);
        assert_eq!(ts.logical, 0);
    }

    #[test]
    fn tick_same_or_stale_increments_logical() {
        let mut ts = HybridTimestamp::new(100, 5, 1);
        ts.tick(100);
        assert_eq!((ts.physical_ns, ts.logical), (100, 6));
        ts.tick(50);
        assert_eq!((ts.physical_ns, ts.logical), (100, 7));
    }

    #[test]
    fn recv_remote_ahead() {
        let mut local = HybridTimestamp::new(100, 3, 1);
        let remote = HybridTimestamp::new(150, 2, 2);
        local.recv(120, &remote);
        assert_eq!((local.physical_ns, local.logical), (150, 3));
    }

    #[test]
    fn recv_same_physical() {
        let mut local = HybridTimestamp::new(100, 3, 1);
        let remote = HybridTimestamp::new(100, 5, 2);
        local.recv(90, &remote);
        assert_eq!((local.physical_ns, local.logical), (100, 6));
    }

    #[test]
    fn cbor_round_trip() {
        let ts = HybridTimestamp::new(1_700_000_000_000_000_000, 42, 7);
        let mut buf = Vec::new();
        ciborium::ser::into_writer(&ts, &mut buf).unwrap();
        let back: HybridTimestamp = ciborium::de::from_reader(buf.as_slice()).unwrap();
        assert_eq!(ts, back);
    }
}
