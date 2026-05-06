//! `OverflowRecord` (4 KiB) — IMPL §5.2.
//!
//! When an object has `tag_count > 4`, any `attr_count`, or any
//! `relation_count`, the inline tag slots in `ObjectRecord` are abandoned and
//! all three lists move to a chain of 4 KiB `OverflowRecord` blocks in the
//! metadata zone (head addressed by `ObjectRecord.overflow_offset`,
//! continuation by `OverflowRecord.next_overflow`). See IMPL §5.2 for the
//! variable-length payload format.
//!
//! This phase ships **only the fixed-size header** (the 56-byte preamble
//! that the §1.5 block-management code can validate without knowing the
//! payload schema). The chain navigation, payload encoder/decoder, and
//! `OverflowAttr` variant decoding are deferred — see TODO below.

use {
    bytemuck::{Pod, Zeroable},
    mimisbrunnr_storage::BlockHeader,
    static_assertions::const_assert_eq,
};

/// Total on-disk size of an overflow record (one 4 KiB block).
pub const OVERFLOW_RECORD_SIZE: usize = 4096;

/// Set in an `OverflowAttr.flags` byte to indicate the body is a 16-byte
/// `BlobRef` rather than a 96-byte inline value (IMPL §5.2).
pub const OVERFLOW_ATTR_FLAG_SPILL: u8 = 1 << 0;

/// Fixed-size header of an overflow record: covers everything up to the
/// variable-length payload region. The trailing payload `[56..4092]` is
/// 4036 bytes of tightly-packed tag IDs, `OverflowAttr` records, and
/// `(predicate, target)` relation tuples in that order.
///
/// The total record size is exactly 4 KiB; [`OVERFLOW_RECORD_SIZE`] is
/// asserted at compile time.
///
/// Layout:
/// ```text
///  [0..32]   header           BlockHeader   kind = OverflowRecord
///  [32..40]  object_id        u64
///  [40..42]  tag_count        u16   per-block count, NOT object-wide
///  [42..44]  attr_count       u16   per-block count
///  [44..46]  relation_count   u16   per-block count
///  [46..48]  _pad             u16
///  [48..56]  next_overflow    u64   block_no of next overflow block, 0 if last
/// ```
///
/// `#[repr(C, packed)]` matches the spec convention (IMPL §1.1) and keeps
/// the layout deterministic across compilers; readers must use
/// `let v = { hdr.field };` to copy aligned locals before use (see
/// REWRITE_CONTRACT.md §4.7).
//
// TODO(rewrite-phase-N): expose payload encoder / decoder, `OverflowAttr`
// struct, and chain-walking helpers. They are deferred along with the live
// 4 KiB block-management path that owns `BLOCK_FLAG_CONTINUATION`.
#[repr(C, packed)]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
pub struct OverflowHeader {
    /// `[0..32]` standard 4 KiB block header (kind = `OverflowRecord`).
    pub header: BlockHeader,
    /// `[32..40]` owning object id.
    pub object_id: u64,
    /// `[40..42]` per-block tag count (lower bound of the object-wide total).
    pub tag_count: u16,
    /// `[42..44]` per-block attr count.
    pub attr_count: u16,
    /// `[44..46]` per-block relation count.
    pub relation_count: u16,
    /// `[46..48]` padding.
    pub _pad: u16,
    /// `[48..56]` block_no of the next overflow block in the chain
    /// (`0` if this is the last).
    pub next_overflow: u64,
}

/// Size of [`OverflowHeader`]: 32 + 8 + 2 + 2 + 2 + 2 + 8 = 56 bytes.
pub const OVERFLOW_HEADER_SIZE: usize = 56;

const_assert_eq!(core::mem::size_of::<OverflowHeader>(), OVERFLOW_HEADER_SIZE);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_size_is_56() {
        // 32 (BlockHeader) + 8 (object_id) + 2+2+2+2 (counts + pad) + 8 (next_overflow) = 56
        assert_eq!(core::mem::size_of::<OverflowHeader>(), 56);
    }

    #[test]
    fn record_size_is_4_kib() {
        assert_eq!(OVERFLOW_RECORD_SIZE, 4096);
    }

    #[test]
    fn round_trip_via_bytemuck() {
        let mut hdr: OverflowHeader = OverflowHeader::zeroed();
        hdr.object_id = 0xdead_beef_cafe_babe;
        hdr.tag_count = 17;
        hdr.attr_count = 3;
        hdr.relation_count = 5;
        hdr.next_overflow = 0x1234_5678;

        let bytes = bytemuck::bytes_of(&hdr).to_vec();
        assert_eq!(bytes.len(), OVERFLOW_HEADER_SIZE);

        let hdr2: &OverflowHeader = bytemuck::from_bytes(&bytes);
        // Copy out packed fields before comparing.
        assert_eq!({ hdr2.object_id }, 0xdead_beef_cafe_babe);
        assert_eq!({ hdr2.tag_count }, 17);
        assert_eq!({ hdr2.attr_count }, 3);
        assert_eq!({ hdr2.relation_count }, 5);
        assert_eq!({ hdr2.next_overflow }, 0x1234_5678);
    }
}
