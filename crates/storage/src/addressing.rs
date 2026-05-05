//! Block-addressing helpers. IMPL §2.3 and §12.3.

use crate::block::BLOCK_SIZE_LOG2;

/// Decompose a logical 4 KiB `block_no` into `(bucket_no, in_bucket_offset)`
/// for a disk whose buckets are `1 << bucket_size_log2` bytes each.
///
/// IMPL §12.3:
/// ```text
/// bucket_no = block_no >> (bucket_size_log2 - 12)
/// in_bucket = block_no &  ((1 << (bucket_size_log2 - 12)) - 1)
/// ```
pub fn block_no_to_bucket(block_no: u32, bucket_size_log2: u8) -> (u32, u32) {
    debug_assert!(
        bucket_size_log2 >= BLOCK_SIZE_LOG2,
        "bucket must be at least one block"
    );
    let shift = bucket_size_log2 - BLOCK_SIZE_LOG2;
    let bucket_no = block_no >> shift;
    let in_bucket = block_no & ((1u32 << shift) - 1);
    (bucket_no, in_bucket)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_mib_buckets() {
        // 1 MiB bucket, 4 KiB block ⇒ shift = 8.
        let (bn, off) = block_no_to_bucket(0x1234, 20);
        assert_eq!(bn, 0x12);
        assert_eq!(off, 0x34);
    }

    #[test]
    fn two_fifty_six_kib_buckets() {
        // 256 KiB bucket ⇒ shift = 6.
        let (bn, off) = block_no_to_bucket(0b1010_1010_1010, 18);
        assert_eq!(bn, 0b1010_1010_1010 >> 6);
        assert_eq!(off, 0b1010_1010_1010 & 0x3F);
    }

    #[test]
    fn block_zero_is_bucket_zero() {
        let (bn, off) = block_no_to_bucket(0, 20);
        assert_eq!((bn, off), (0, 0));
    }
}
