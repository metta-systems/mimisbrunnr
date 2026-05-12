//! Journal pinning for WAL retention.
//!
//! Implements IMPL §3.4 `DirtyNode` accounting — the WAL trimmer must not
//! reclaim entries past the lowest live `lsn_min` across all pins.

/// A pin held by a `LoadedNode` to prevent WAL truncation of pending entries.
///
/// The WAL checkpoint process queries the lowest `lsn_min` across all pins
/// and will not advance `read_cursor` past it.
#[derive(Clone, Copy, Debug)]
pub struct JournalPin {
    /// Minimum LSN of journal entries referenced by this node.
    pub lsn_min: u64,
    /// Maximum LSN of journal entries in this node's pending journal.
    pub lsn_max: u64,
    /// Count of entries in the pending journal (for debugging/stats).
    pub count: u32,
}

impl JournalPin {
    /// Create a new pin covering the given LSN range.
    pub fn new(lsn_min: u64, lsn_max: u64, count: u32) -> Self {
        Self {
            lsn_min,
            lsn_max,
            count,
        }
    }

    /// Update the pin to reflect a new journal entry with the given LSN.
    /// Returns true if the pin was modified.
    pub fn update_with_lsn(&mut self, lsn: u64, count_delta: u32) -> bool {
        let updated = if lsn < self.lsn_min {
            self.lsn_min = lsn;
            true
        } else {
            false
        };
        self.lsn_max = lsn;
        self.count = self.count.saturating_add(count_delta);
        updated
    }

    /// Clear the pin (used when flushing the pending journal).
    pub fn clear(&mut self) {
        self.lsn_min = u64::MAX;
        self.lsn_max = 0;
        self.count = 0;
    }

    /// Check if this pin is active (has pending entries).
    pub fn is_active(&self) -> bool {
        self.count > 0 && self.lsn_min <= self.lsn_max
    }
}

impl Default for JournalPin {
    fn default() -> Self {
        Self {
            lsn_min: u64::MAX,
            lsn_max: 0,
            count: 0,
        }
    }
}
