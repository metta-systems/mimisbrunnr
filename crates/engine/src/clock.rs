//! [`HybridClock`] — small wrapper around [`HybridTimestamp`].
//!
//! Pure logic, no I/O. Reads physical wall-clock nanoseconds via
//! [`std::time::SystemTime`].

use std::time::{SystemTime, UNIX_EPOCH};

use mimisbrunnr_types::{HybridTimestamp, NodeId};

/// HLC clock source. Owns a monotonic `last` snapshot used to break ties.
#[derive(Debug, Clone)]
pub struct HybridClock {
    pub node_id: NodeId,
    last: HybridTimestamp,
}

impl HybridClock {
    /// New clock attributing every event to `node_id`.
    pub fn new(node_id: NodeId) -> Self {
        Self {
            node_id,
            last: HybridTimestamp::new(0, 0, node_id),
        }
    }

    /// Latest emitted timestamp.
    pub fn latest(&self) -> HybridTimestamp {
        self.last
    }

    /// Read wall-clock physical nanoseconds. Falls back to `last.physical_ns`
    /// if the system time is somehow before the Unix epoch.
    fn now_ns(&self) -> i64 {
        let dur = SystemTime::now().duration_since(UNIX_EPOCH);
        match dur {
            Ok(d) => i64::try_from(d.as_nanos()).unwrap_or(self.last.physical_ns),
            Err(_) => self.last.physical_ns,
        }
    }

    /// Advance the clock for a *local* event and return the new timestamp.
    /// HLC `tick`: the physical reading wins if it strictly exceeds `last`,
    /// otherwise the logical counter increments.
    pub fn now(&mut self) -> HybridTimestamp {
        let now_ns = self.now_ns();
        let mut next = self.last;
        next.tick(now_ns);
        // Pin the originating node id (tick() doesn't touch it, but defend
        // against external mutation of `last`).
        next.node_id = self.node_id;
        self.last = next;
        next
    }

    /// Wire-form helper: same as [`Self::now`] but explicitly named for the
    /// WAL append path which talks in `HybridTimestamp` directly.
    pub fn now_wire(&mut self) -> HybridTimestamp {
        self.now()
    }

    /// HLC `recv`: merge a remote timestamp.
    pub fn observe(&mut self, remote: HybridTimestamp) {
        let now_ns = self.now_ns();
        let mut merged = self.last;
        merged.recv(now_ns, &remote);
        merged.node_id = self.node_id;
        self.last = merged;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn now_is_monotonic_over_many_calls() {
        let mut clk = HybridClock::new(7);
        let mut prev = clk.now();
        for _ in 0..1000 {
            let next = clk.now();
            assert!(next >= prev, "clock went backwards: {prev} -> {next}");
            prev = next;
        }
    }

    #[test]
    fn now_carries_node_id() {
        let mut clk = HybridClock::new(42);
        let ts = clk.now();
        assert_eq!(ts.node_id, 42);
    }

    #[test]
    fn observe_advances_past_remote() {
        let mut clk = HybridClock::new(1);
        // Remote far in the future.
        let remote = HybridTimestamp::new(i64::MAX / 2, 5, 99);
        clk.observe(remote);
        let after = clk.now();
        assert!(after >= remote);
    }
}
