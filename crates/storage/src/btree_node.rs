//! Large-region (256 KiB) headers for B+ tree and radix node containers.
//!
//! Implements IMPL §1.5.1 — *structs and parse/serialise helpers only*.
//! The live-node machinery (sorted-run merge, append-only growth, full
//! compaction, format promotion, packed-key codec) is deferred to the
//! index/meta crates.

use {
    bytemuck::{Pod, Zeroable},
    static_assertions::const_assert_eq,
};

use crate::{
    block::{BLOCK_PREAMBLE_MAGIC_BTREE, BlockPreamble, BtreeKind},
    error::StorageError,
};

// ---------- BtreeNodeHeader ----------

/// Flag: full compaction in progress (recovery hint).
pub const BTREE_NODE_FLAG_COMPACTION_IN_PROGRESS: u8 = 1 << 0;
/// Flag: head of a `ChunkList` chain (carries `ChunkParamsRecord`).
pub const BTREE_NODE_FLAG_HEAD_OF_CHAIN: u8 = 1 << 1;

/// 64-byte header at offset 0 of every 256 KiB region.
#[repr(C, packed)]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
pub struct BtreeNodeHeader {
    pub pre: BlockPreamble,             // [0..8]   magic = "MIMB", kind ∈ BtreeKind
    pub seq: u64,                       // [8..16]  monotonic per-region
    pub last_persisted_lsn: u64,        // [16..24] BlockHeader.lsn analogue
    pub region_size_log2: u8,           // [24..25] 18 = 256 KiB
    pub level: u8,                      // [25..26] 0 = leaf, ≥1 = inner
    pub sorted_run_count: u8,           // [26..27]
    pub flags: u8,                      // [27..28] BTREE_NODE_FLAG_*
    pub payload_used: u32,              // [28..32]
    pub min_key: [u8; 16],              // [32..48]
    pub max_key: [u8; 16],              // [48..64]
}

const_assert_eq!(core::mem::size_of::<BtreeNodeHeader>(), 64);

impl BtreeNodeHeader {
    /// Build a fresh header for a region of the given `kind` and `level`.
    pub fn new(kind: BtreeKind, format_version: u16, level: u8, region_size_log2: u8) -> Self {
        Self {
            pre: BlockPreamble {
                magic: BLOCK_PREAMBLE_MAGIC_BTREE,
                kind: kind as u16,
                format_version,
            },
            seq: 0,
            last_persisted_lsn: 0,
            region_size_log2,
            level,
            sorted_run_count: 0,
            flags: 0,
            payload_used: 0,
            min_key: [0u8; 16],
            max_key: [0u8; 16],
        }
    }

    /// Parse a `BtreeNodeHeader` from a byte slice. Validates the magic.
    pub fn parse(bytes: &[u8]) -> Result<Self, StorageError> {
        if bytes.len() < core::mem::size_of::<Self>() {
            return Err(StorageError::BufferTooSmall {
                need: core::mem::size_of::<Self>(),
                have: bytes.len(),
            });
        }
        let header_bytes = &bytes[..core::mem::size_of::<Self>()];
        let header: Self = *bytemuck::from_bytes(header_bytes);
        let magic = { header.pre.magic };
        if magic != BLOCK_PREAMBLE_MAGIC_BTREE {
            return Err(StorageError::InvalidMagic {
                expected: BLOCK_PREAMBLE_MAGIC_BTREE,
                actual: magic,
            });
        }
        Ok(header)
    }

    /// Serialise as bytes (zero-copy view).
    pub fn as_bytes(&self) -> &[u8] {
        bytemuck::bytes_of(self)
    }
}

// ---------- SortedRunHeader ----------

/// Flag: entries are encoded with `SortedRunKeyFormat` packing.
pub const SORTED_RUN_FLAG_PACKED_KEYS: u32 = 1 << 0;
/// Flag: sorted-run payload is encrypted.
pub const SORTED_RUN_FLAG_ENCRYPTED: u32 = 1 << 1;

/// 32-byte header before each sorted run within a region. IMPL §1.5.1.
#[repr(C, packed)]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
pub struct SortedRunHeader {
    pub magic: u32,           // [0..4]   "BSET" little-endian
    pub seq: u32,             // [4..8]   monotonic within region
    pub journal_seq: u64,     // [8..16]  newest WAL LSN merged in
    pub entry_count: u32,     // [16..20]
    pub payload_length: u32,  // [20..24]
    pub flags: u32,           // [24..28] SORTED_RUN_FLAG_*
    pub crc: u32,             // [28..32] CRC32C over (header || payload), CRC slot zeroed
}

const_assert_eq!(core::mem::size_of::<SortedRunHeader>(), 32);

/// `"BSET"` magic for sorted runs, little-endian. IMPL §1.5.1.
pub const SORTED_RUN_MAGIC: u32 = u32::from_le_bytes(*b"BSET");

impl SortedRunHeader {
    /// Compute the CRC32C of (header || payload) with the CRC slot zeroed.
    pub fn compute_crc(header_with_zero_crc: &[u8], payload: &[u8]) -> u32 {
        let mut h = crc32c::crc32c(header_with_zero_crc);
        h = crc32c::crc32c_append(h, payload);
        h
    }

    /// Validate this header's CRC against `payload`.
    pub fn verify(&self, payload: &[u8]) -> Result<(), StorageError> {
        let expected = { self.crc };
        let mut header_copy = *self;
        header_copy.crc = 0;
        let actual = Self::compute_crc(bytemuck::bytes_of(&header_copy), payload);
        if expected == actual {
            Ok(())
        } else {
            Err(StorageError::CrcMismatch { expected, actual })
        }
    }
}

// ---------- SortedRunKeyFormat / FieldFormat ----------

/// Field-format flag: signed integer.
pub const FIELD_FORMAT_FLAG_SIGNED: u8 = 1 << 0;
/// Field-format flag: MSB-first packing (big-endian).
pub const FIELD_FORMAT_FLAG_MSB_FIRST: u8 = 1 << 1;

/// `SortedRunKeyFormat.value_size_kind = 0` — every entry has the same value
/// length; no per-entry length prefix on the wire.
pub const VALUE_SIZE_KIND_FIXED: u8 = 0;
/// `SortedRunKeyFormat.value_size_kind = 1` — each entry is preceded by an
/// unsigned LEB128 varint giving the *elided-tail* length in bytes.
pub const VALUE_SIZE_KIND_VARINT: u8 = 1;

/// 16-byte per-field descriptor for packed keys. IMPL §1.5.6.
#[repr(C, packed)]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
pub struct FieldFormat {
    pub bit_width: u8, // [0..1]   0 ⇒ constant
    pub flags: u8,     // [1..2]   FIELD_FORMAT_FLAG_*
    pub _pad0: u16,    // [2..4]
    pub base: u64,     // [4..12]  subtracted from each field at write time
    pub _pad1: u32,    // [12..16] tail pad to keep struct multiple-of-8
}

const_assert_eq!(core::mem::size_of::<FieldFormat>(), 16);

/// Variable-size sorted-run key-format descriptor. The struct itself is the
/// 8-byte fixed header; the on-disk wire form continues with
/// `[FieldFormat; nr_fields]` immediately followed by `common_value_prefix`
/// raw bytes carrying the elided value prefix. IMPL §1.5.6.
#[repr(C, packed)]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
pub struct SortedRunKeyFormat {
    pub nr_fields: u8,           // [0..1]   1..=8
    pub key_header_bytes: u8,    // [1..2]   0..=4
    pub common_value_prefix: u8, // [2..3]   0..=255
    pub value_size_kind: u8,     // [3..4]   0 = fixed; 1 = varint per-entry tail length
    pub sum_bit_width: u32,      // [4..8]   total packed-key bits incl. header
}

const_assert_eq!(core::mem::size_of::<SortedRunKeyFormat>(), 8);

impl SortedRunKeyFormat {
    /// Total on-disk size of this descriptor including the trailing field
    /// array and the `common_value_prefix` byte tail.
    pub fn total_size(&self) -> usize {
        let nr = { self.nr_fields } as usize;
        let prefix = { self.common_value_prefix } as usize;
        core::mem::size_of::<Self>() + nr * core::mem::size_of::<FieldFormat>() + prefix
    }

    /// Parse the descriptor, the trailing field array, and the elided
    /// `value_prefix` bytes out of `bytes`. The returned `Vec<u8>` has length
    /// `head.common_value_prefix` and carries the bytes that were elided from
    /// every per-entry value tail; readers reconstruct each full value as
    /// `value_prefix || value_tail_i`.
    pub fn parse(
        bytes: &[u8],
    ) -> Result<(Self, Vec<FieldFormat>, Vec<u8>), StorageError> {
        let head_size = core::mem::size_of::<Self>();
        if bytes.len() < head_size {
            return Err(StorageError::BufferTooSmall {
                need: head_size,
                have: bytes.len(),
            });
        }
        let head: Self = *bytemuck::from_bytes(&bytes[..head_size]);
        let nr = { head.nr_fields } as usize;
        let prefix_len = { head.common_value_prefix } as usize;
        let fields_end = head_size + nr * core::mem::size_of::<FieldFormat>();
        let total = fields_end + prefix_len;
        if bytes.len() < total {
            return Err(StorageError::BufferTooSmall {
                need: total,
                have: bytes.len(),
            });
        }
        let fields_bytes = &bytes[head_size..fields_end];
        let fields: Vec<FieldFormat> =
            bytemuck::cast_slice::<u8, FieldFormat>(fields_bytes).to_vec();
        let value_prefix = bytes[fields_end..total].to_vec();
        Ok((head, fields, value_prefix))
    }

    /// Serialise the descriptor, field array, and elided `value_prefix` bytes
    /// into `out`. The caller must ensure `value_prefix.len() ==
    /// self.common_value_prefix`.
    pub fn serialise(&self, fields: &[FieldFormat], value_prefix: &[u8], out: &mut Vec<u8>) {
        debug_assert_eq!(
            value_prefix.len(),
            { self.common_value_prefix } as usize,
            "serialise: value_prefix length must match descriptor",
        );
        out.extend_from_slice(bytemuck::bytes_of(self));
        out.extend_from_slice(bytemuck::cast_slice(fields));
        out.extend_from_slice(value_prefix);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn btree_node_header_is_64_bytes() {
        assert_eq!(core::mem::size_of::<BtreeNodeHeader>(), 64);
    }

    #[test]
    fn sorted_run_header_is_32_bytes() {
        assert_eq!(core::mem::size_of::<SortedRunHeader>(), 32);
    }

    #[test]
    fn field_format_is_16_bytes() {
        assert_eq!(core::mem::size_of::<FieldFormat>(), 16);
    }

    #[test]
    fn parse_btree_node_header_validates_magic() {
        let h = BtreeNodeHeader::new(BtreeKind::Forward, 1, 0, 18);
        let mut bytes = Vec::new();
        bytes.extend_from_slice(h.as_bytes());
        let parsed = BtreeNodeHeader::parse(&bytes).unwrap();
        assert_eq!({ parsed.region_size_log2 }, 18);

        // Corrupt the magic.
        bytes[0] = b'X';
        let err = BtreeNodeHeader::parse(&bytes).unwrap_err();
        assert!(matches!(err, StorageError::InvalidMagic { .. }));
    }

    #[test]
    fn sorted_run_header_crc_round_trip() {
        let payload = b"hello world";
        let mut h = SortedRunHeader {
            magic: SORTED_RUN_MAGIC,
            seq: 1,
            journal_seq: 0,
            entry_count: 0,
            payload_length: payload.len() as u32,
            flags: 0,
            crc: 0,
        };
        h.crc = SortedRunHeader::compute_crc(bytemuck::bytes_of(&h), payload);
        h.verify(payload).unwrap();

        let mut tampered = payload.to_vec();
        tampered[0] ^= 0xFF;
        let err = h.verify(&tampered).unwrap_err();
        assert!(matches!(err, StorageError::CrcMismatch { .. }));
    }

    #[test]
    fn key_format_round_trip() {
        let head = SortedRunKeyFormat {
            nr_fields: 2,
            key_header_bytes: 2,
            common_value_prefix: 4,
            value_size_kind: VALUE_SIZE_KIND_FIXED,
            sum_bit_width: 24,
        };
        let fields = vec![
            FieldFormat { bit_width: 8, flags: 0, _pad0: 0, base: 0, _pad1: 0 },
            FieldFormat { bit_width: 16, flags: FIELD_FORMAT_FLAG_SIGNED, _pad0: 0, base: 100, _pad1: 0 },
        ];
        let prefix = [0xDE, 0xAD, 0xBE, 0xEF];
        let mut buf = Vec::new();
        head.serialise(&fields, &prefix, &mut buf);
        assert_eq!(buf.len(), head.total_size());
        let (parsed, parsed_fields, parsed_prefix) = SortedRunKeyFormat::parse(&buf).unwrap();
        assert_eq!({ parsed.nr_fields }, 2);
        assert_eq!(parsed_fields.len(), 2);
        let f1_base = { parsed_fields[1].base };
        assert_eq!(f1_base, 100);
        assert_eq!(parsed_prefix, prefix);
    }

    #[test]
    fn key_format_zero_prefix_is_empty_tail() {
        let head = SortedRunKeyFormat {
            nr_fields: 1,
            key_header_bytes: 0,
            common_value_prefix: 0,
            value_size_kind: VALUE_SIZE_KIND_FIXED,
            sum_bit_width: 32,
        };
        let fields = vec![FieldFormat {
            bit_width: 32,
            flags: 0,
            _pad0: 0,
            base: 0,
            _pad1: 0,
        }];
        let mut buf = Vec::new();
        head.serialise(&fields, &[], &mut buf);
        assert_eq!(buf.len(), 8 + 16);
        let (_parsed, parsed_fields, parsed_prefix) =
            SortedRunKeyFormat::parse(&buf).unwrap();
        assert_eq!(parsed_fields.len(), 1);
        assert!(parsed_prefix.is_empty());
    }
}
