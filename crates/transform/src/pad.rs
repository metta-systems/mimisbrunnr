/// Sector alignment: pad data to 4KB boundary.
///
/// Required before encryption since XTS and HCTR2 are length-preserving
/// and operate on sector-aligned data.
pub struct SectorPadder;

/// Sector size (4 KiB).
pub const SECTOR_SIZE: usize = 4096;

impl SectorPadder {
    /// Pad data to the next 4KB boundary. Returns (padded_data, original_length).
    pub fn pad(data: &[u8]) -> (Vec<u8>, usize) {
        let original_len = data.len();
        let padded_len = Self::padded_size(original_len);

        let mut padded = Vec::with_capacity(padded_len);
        padded.extend_from_slice(data);
        padded.resize(padded_len, 0);

        (padded, original_len)
    }

    /// Remove padding given the original length.
    pub fn unpad(data: &[u8], original_len: usize) -> &[u8] {
        &data[..original_len.min(data.len())]
    }

    /// Compute the padded size for a given original size.
    pub fn padded_size(original_len: usize) -> usize {
        if original_len == 0 {
            return 0;
        }
        original_len.div_ceil(SECTOR_SIZE) * SECTOR_SIZE
    }

    /// Check if data is already sector-aligned.
    pub fn is_aligned(data: &[u8]) -> bool {
        data.is_empty() || data.len().is_multiple_of(SECTOR_SIZE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pad_small_data() {
        let data = b"hello";
        let (padded, orig_len) = SectorPadder::pad(data);
        assert_eq!(orig_len, 5);
        assert_eq!(padded.len(), SECTOR_SIZE);
        assert_eq!(&padded[..5], b"hello");
        assert!(padded[5..].iter().all(|&b| b == 0));
    }

    #[test]
    fn pad_exact_sector() {
        let data = vec![0xAB; SECTOR_SIZE];
        let (padded, orig_len) = SectorPadder::pad(&data);
        assert_eq!(orig_len, SECTOR_SIZE);
        assert_eq!(padded.len(), SECTOR_SIZE);
        assert_eq!(padded, data);
    }

    #[test]
    fn pad_just_over_sector() {
        let data = vec![0xCD; SECTOR_SIZE + 1];
        let (padded, orig_len) = SectorPadder::pad(&data);
        assert_eq!(orig_len, SECTOR_SIZE + 1);
        assert_eq!(padded.len(), SECTOR_SIZE * 2);
    }

    #[test]
    fn pad_empty() {
        let (padded, orig_len) = SectorPadder::pad(b"");
        assert_eq!(orig_len, 0);
        assert_eq!(padded.len(), 0);
    }

    #[test]
    fn unpad_restores_original() {
        let data = b"original content here!";
        let (padded, orig_len) = SectorPadder::pad(data);
        let restored = SectorPadder::unpad(&padded, orig_len);
        assert_eq!(restored, data);
    }

    #[test]
    fn padded_size() {
        assert_eq!(SectorPadder::padded_size(0), 0);
        assert_eq!(SectorPadder::padded_size(1), SECTOR_SIZE);
        assert_eq!(SectorPadder::padded_size(SECTOR_SIZE), SECTOR_SIZE);
        assert_eq!(SectorPadder::padded_size(SECTOR_SIZE + 1), SECTOR_SIZE * 2);
        assert_eq!(SectorPadder::padded_size(SECTOR_SIZE * 3), SECTOR_SIZE * 3);
    }

    #[test]
    fn is_aligned() {
        assert!(SectorPadder::is_aligned(b""));
        assert!(!SectorPadder::is_aligned(b"x"));
        assert!(SectorPadder::is_aligned(&vec![0; SECTOR_SIZE]));
        assert!(!SectorPadder::is_aligned(&vec![0; SECTOR_SIZE + 1]));
    }

    #[test]
    fn large_data_round_trip() {
        let data = vec![0xFF; 100_000];
        let (padded, orig_len) = SectorPadder::pad(&data);
        assert!(SectorPadder::is_aligned(&padded));
        let restored = SectorPadder::unpad(&padded, orig_len);
        assert_eq!(restored, &data[..]);
    }
}
