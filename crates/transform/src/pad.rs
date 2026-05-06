//! Sector-alignment padding (DESIGN §9.1, step 3).
//!
//! Block-cipher modes used for on-disk encryption — XTS in particular —
//! operate on fixed-size sectors. The padder zero-extends a buffer in
//! place to the next multiple of `sector_size`. Tracking the *original*
//! length so the buffer can be restored later is the caller's job: it
//! belongs in metadata (e.g. `ObjectRecord::content_size`), not in the
//! padded bytes themselves.

/// Default sector size used by Mímisbrunnr (4 KiB).
pub const SECTOR_SIZE: usize = 4096;

/// Stateless sector padder.
#[derive(Debug, Default, Clone, Copy)]
pub struct SectorPadder;

impl SectorPadder {
    pub const fn new() -> Self {
        Self
    }

    /// Zero-extend `data` in place so its length is a multiple of
    /// `sector_size`. A zero-length buffer is left untouched. Panics if
    /// `sector_size == 0`.
    pub fn pad_to(&self, data: &mut Vec<u8>, sector_size: usize) {
        assert!(sector_size > 0, "sector_size must be > 0");
        if data.is_empty() {
            return;
        }
        let aligned = data.len().div_ceil(sector_size) * sector_size;
        data.resize(aligned, 0);
    }

    /// Returns true if `data.len()` is already a multiple of `sector_size`
    /// (or empty). Panics if `sector_size == 0`.
    pub fn is_aligned(&self, data: &[u8], sector_size: usize) -> bool {
        assert!(sector_size > 0, "sector_size must be > 0");
        data.is_empty() || data.len().is_multiple_of(sector_size)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pad_small_to_4k() {
        let p = SectorPadder::new();
        let mut data = b"hello".to_vec();
        p.pad_to(&mut data, SECTOR_SIZE);
        assert_eq!(data.len(), SECTOR_SIZE);
        assert_eq!(&data[..5], b"hello");
        assert!(data[5..].iter().all(|&b| b == 0));
    }

    #[test]
    fn pad_exact_multiple_unchanged() {
        let p = SectorPadder::new();
        let mut data = vec![0xABu8; SECTOR_SIZE];
        let snapshot = data.clone();
        p.pad_to(&mut data, SECTOR_SIZE);
        assert_eq!(data, snapshot);
    }

    #[test]
    fn pad_just_over_one_sector() {
        let p = SectorPadder::new();
        let mut data = vec![0xCDu8; SECTOR_SIZE + 1];
        p.pad_to(&mut data, SECTOR_SIZE);
        assert_eq!(data.len(), 2 * SECTOR_SIZE);
    }

    #[test]
    fn pad_empty_is_noop() {
        let p = SectorPadder::new();
        let mut data: Vec<u8> = Vec::new();
        p.pad_to(&mut data, SECTOR_SIZE);
        assert!(data.is_empty());
    }

    #[test]
    fn round_trip_with_caller_tracked_length() {
        // The padder doesn't track original length — caller does.
        let p = SectorPadder::new();
        let original = b"the quick brown fox".to_vec();
        let original_len = original.len();
        let mut padded = original.clone();
        p.pad_to(&mut padded, SECTOR_SIZE);
        assert!(p.is_aligned(&padded, SECTOR_SIZE));
        // Caller restores by truncating to the recorded length.
        padded.truncate(original_len);
        assert_eq!(padded, original);
    }

    #[test]
    fn is_aligned_cases() {
        let p = SectorPadder::new();
        assert!(p.is_aligned(b"", SECTOR_SIZE));
        assert!(!p.is_aligned(b"x", SECTOR_SIZE));
        assert!(p.is_aligned(&vec![0; SECTOR_SIZE], SECTOR_SIZE));
        assert!(!p.is_aligned(&vec![0; SECTOR_SIZE + 1], SECTOR_SIZE));
    }
}
