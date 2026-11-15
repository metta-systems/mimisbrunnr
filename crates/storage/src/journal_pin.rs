//! Journal retention pin over a node's pending journal entries
//! (JOURNAL-DESIGN§1.4, BTREE-DESIGN§1.3).

/// The journal must not reclaim entries at or above the lowest live
/// `lsn_min` across all pins. An empty pin (`count == 0`) holds nothing back.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct JournalPin {
    /// Lowest LSN in the pending journal.
    pub lsn_min: u64,
    /// Highest LSN in the pending journal.
    pub lsn_max: u64,
    /// Number of pending journal entries.
    pub count: u32,
}

impl JournalPin {
    /// Extend the pin over one more pending entry at `lsn`.
    pub fn record(&mut self, lsn: u64) {
        if self.count == 0 {
            self.lsn_min = lsn;
            self.lsn_max = lsn;
        } else {
            self.lsn_min = self.lsn_min.min(lsn);
            self.lsn_max = self.lsn_max.max(lsn);
        }
        self.count = self.count.saturating_add(1);
    }

    /// `true` while the pin holds journal entries back.
    pub fn is_active(&self) -> bool {
        self.count > 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pin_covers_every_recorded_lsn_until_cleared() {
        let mut pin = JournalPin::default();
        assert!(!pin.is_active());
        pin.record(40);
        pin.record(7);
        pin.record(19);
        assert!(pin.is_active());
        assert_eq!((pin.lsn_min, pin.lsn_max, pin.count), (7, 40, 3));
        // A flush clears the pin by replacing it.
        pin = JournalPin::default();
        assert!(!pin.is_active());
    }
}
