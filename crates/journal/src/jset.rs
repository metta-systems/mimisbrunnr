//! Journal sets: `JsetHeader`, `EntryHead`, LSNs and HLC offsets (JOURNAL-DESIGN§1.2).

use {
    bytemuck::{Pod, Zeroable},
    mimisbrunnr_storage::{BLOCK_SIZE, Preamble, Structure, checksum64},
    mimisbrunnr_types::HybridTimestamp,
    static_assertions::const_assert_eq,
    std::collections::BTreeMap,
};

use crate::{
    entry::{WalHybridTimestampWire, WalOpKind},
    error::WalError,
};

/// Entries are AEAD-encrypted (ENCRYPTION-DESIGN§1).
pub const JSET_FLAG_ENCRYPTED: u16 = 1 << 0;
/// Entries are compressed before encryption.
pub const JSET_FLAG_COMPRESSED: u16 = 1 << 1;
/// A full 16-byte `HybridTimestamp` precedes the entry's payload.
pub const ENTRY_FLAG_HLC_EXPLICIT: u8 = 1 << 0;

/// Largest jset, header, AEAD tag and padding included.
pub const JSET_MAX_SIZE: usize = 256 * 1024;
/// AEAD tag after the entries of an encrypted jset.
const AEAD_TAG_LEN: usize = 16;
/// Byte offset of `JsetHeader.checksum`.
const CHECKSUM_OFFSET: usize = 56;

/// Header of one journal set (64 bytes).
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
pub struct JsetHeader {
    pub pre: Preamble,                     //  [0..8]   magic "JRNL", version, kind 0
    pub seq: u64,                          //  [8..16]  monotonic jset sequence number
    pub last_seq: u64,                     // [16..24]  oldest seq still needed (JOURNAL-DESIGN§1.4)
    pub hlc_base: WalHybridTimestampWire,  // [24..40]  entry timestamps are offsets from this
    pub payload_len: u32,                  // [40..44]  bytes of entries as stored
    pub entry_count: u32,                  // [44..48]
    pub plain_len: u32,                    // [48..52]  bytes of entries before compression
    pub flags: u16,                        // [52..54]  JSET_FLAG_*
    pub _pad: u16,                         // [54..56]
    pub checksum: u64,                     // [56..64]  over the whole jset with this field zero
}

const_assert_eq!(size_of::<JsetHeader>(), 64);
const_assert_eq!(core::mem::offset_of!(JsetHeader, checksum), CHECKSUM_OFFSET);

/// Head of one entry inside a jset (8 bytes, 8-aligned).
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
pub struct EntryHead {
    pub op_kind: u8,    // [0..1]  WalOpKind
    pub flags: u8,      // [1..2]  ENTRY_FLAG_*
    pub len: u16,       // [2..4]  payload length in 8-byte words, padding included
    pub hlc_delta: u32, // [4..8]  nanoseconds after hlc_base.physical_ns
}

const_assert_eq!(size_of::<EntryHead>(), 8);

/// LSN of entry `index` of jset `seq`: `seq << 16 | index`.
pub const fn lsn(seq: u64, index: u16) -> u64 {
    seq << 16 | index as u64
}

/// One journal entry as read back.
#[derive(Debug, Clone, PartialEq)]
pub struct WalEntry {
    pub lsn: u64,
    pub op_kind: u8,
    pub timestamp: HybridTimestamp,
    pub payload: Vec<u8>,
}

/// The bytes of a jset `seq` holding one entry, timestamped `timestamp` itself (`hlc_delta` 0).
pub(crate) fn encode_single(
    seq: u64,
    last_seq: u64,
    op_kind: WalOpKind,
    payload: &[u8],
    timestamp: HybridTimestamp,
) -> Result<Vec<u8>, WalError> {
    debug_assert!(payload.len().is_multiple_of(8), "payload records are word-aligned");
    let entries_len = size_of::<EntryHead>() + payload.len();
    let size = (size_of::<JsetHeader>() + entries_len).next_multiple_of(BLOCK_SIZE);
    if size > JSET_MAX_SIZE {
        return Err(WalError::PayloadTooLarge {
            size: payload.len(),
            max: JSET_MAX_SIZE - size_of::<JsetHeader>() - size_of::<EntryHead>(),
        });
    }
    let header = JsetHeader {
        pre: Preamble::new(Structure::JournalSet),
        seq,
        last_seq,
        hlc_base: WalHybridTimestampWire::from_logical(timestamp),
        payload_len: entries_len as u32,
        entry_count: 1,
        plain_len: entries_len as u32,
        flags: 0,
        _pad: 0,
        checksum: 0,
    };
    let head = EntryHead {
        op_kind: op_kind as u8,
        flags: 0,
        len: (payload.len() / 8) as u16, // fits: the jset cap is below 2¹⁶ words
        hlc_delta: 0,
    };
    let mut bytes = vec![0u8; size];
    let entries_start = size_of::<JsetHeader>();
    bytes[..entries_start].copy_from_slice(bytemuck::bytes_of(&header));
    bytes[entries_start..entries_start + 8].copy_from_slice(bytemuck::bytes_of(&head));
    bytes[entries_start + 8..entries_start + entries_len].copy_from_slice(payload);
    let checksum = checksum64(&bytes);
    bytes[CHECKSUM_OFFSET..CHECKSUM_OFFSET + 8].copy_from_slice(&checksum.to_le_bytes());
    Ok(bytes)
}

/// The header a jset starting with `first_block` declares and the jset's stored size, or
/// `None` when the block does not begin a plausible jset.
pub(crate) fn header_and_size(first_block: &[u8]) -> Option<(JsetHeader, usize)> {
    if Preamble::identify(first_block).ok()? != Structure::JournalSet {
        return None;
    }
    let header: JsetHeader = bytemuck::pod_read_unaligned(&first_block[..size_of::<JsetHeader>()]);
    let tag = if header.flags & JSET_FLAG_ENCRYPTED != 0 { AEAD_TAG_LEN } else { 0 };
    let size = (size_of::<JsetHeader>() + header.payload_len as usize + tag).next_multiple_of(BLOCK_SIZE);
    (size <= JSET_MAX_SIZE).then_some((header, size))
}

/// Whether `bytes`, a whole jset, carries its own checksum.
pub(crate) fn checksum_holds(bytes: &[u8]) -> bool {
    let stored: u64 = bytemuck::pod_read_unaligned(&bytes[CHECKSUM_OFFSET..CHECKSUM_OFFSET + 8]);
    let mut zeroed = bytes.to_vec();
    zeroed[CHECKSUM_OFFSET..CHECKSUM_OFFSET + 8].fill(0);
    checksum64(&zeroed) == stored
}

/// The entries of a valid jset, in order, with their LSNs and timestamps.
pub(crate) fn decode_entries(header: &JsetHeader, bytes: &[u8]) -> Result<Vec<WalEntry>, WalError> {
    let seq = header.seq;
    let malformed = |reason| WalError::JsetMalformed { seq, reason };
    if header.flags & JSET_FLAG_ENCRYPTED != 0 {
        return Err(WalError::EncryptionUnsupported); // TODO(ENCRYPTION3)
    }
    if header.flags & JSET_FLAG_COMPRESSED != 0 {
        return Err(WalError::CompressionUnsupported); // TODO(JOURNAL5)
    }
    if header.flags != 0 || header.plain_len != header.payload_len {
        return Err(malformed("flags"));
    }
    if header.entry_count > 1 << 16 {
        return Err(malformed("entry count"));
    }
    let entries = &bytes[size_of::<JsetHeader>()..size_of::<JsetHeader>() + header.payload_len as usize];
    let base = header.hlc_base.to_logical();
    let mut earlier_at_physical: BTreeMap<i64, u16> = BTreeMap::new();
    let mut offset = 0;
    let mut out = Vec::with_capacity(header.entry_count as usize);
    for index in 0..header.entry_count {
        let head_bytes = entries.get(offset..offset + 8).ok_or(malformed("entry head"))?;
        let head: EntryHead = bytemuck::pod_read_unaligned(head_bytes);
        offset += 8;
        if head.flags & !ENTRY_FLAG_HLC_EXPLICIT != 0 {
            return Err(malformed("entry flags"));
        }
        let timestamp = if head.flags & ENTRY_FLAG_HLC_EXPLICIT != 0 {
            let wire = entries.get(offset..offset + 16).ok_or(malformed("explicit timestamp"))?;
            offset += 16;
            bytemuck::pod_read_unaligned::<WalHybridTimestampWire>(wire).to_logical()
        } else {
            let physical_ns = base.physical_ns + head.hlc_delta as i64;
            let earlier = earlier_at_physical.entry(physical_ns).or_default();
            let logical = base.logical.checked_add(*earlier).ok_or(malformed("logical clock"))?;
            *earlier += 1;
            HybridTimestamp::new(physical_ns, logical, base.node_id)
        };
        let payload_len = head.len as usize * 8;
        let payload = entries.get(offset..offset + payload_len).ok_or(malformed("entry length"))?;
        offset += payload_len;
        out.push(WalEntry {
            lsn: lsn(seq, index as u16),
            op_kind: head.op_kind,
            timestamp,
            payload: payload.to_vec(),
        });
    }
    if offset != entries.len() {
        return Err(malformed("entries length"));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timestamp() -> HybridTimestamp {
        HybridTimestamp::new(1_700_000_000_000_000_000, 3, 7)
    }

    #[test]
    fn header_and_entry_head_sizes() {
        assert_eq!(size_of::<JsetHeader>(), 64);
        assert_eq!(size_of::<EntryHead>(), 8);
    }

    #[test]
    fn lsn_is_seq_and_index() {
        assert_eq!(lsn(5, 0), 5 << 16);
        assert_eq!(lsn(5, 3), (5 << 16) + 3);
        assert!(lsn(5, u16::MAX) < lsn(6, 0));
    }

    #[test]
    fn single_entry_jset_round_trips() {
        let payload = [0xabu8; 16];
        let bytes = encode_single(9, 4, WalOpKind::AddTag, &payload, timestamp()).unwrap();
        assert_eq!(bytes.len(), BLOCK_SIZE);
        assert_eq!(&bytes[..4], b"JRNL");
        let (header, size) = header_and_size(&bytes).unwrap();
        assert_eq!(size, BLOCK_SIZE);
        assert_eq!((header.seq, header.last_seq, header.entry_count), (9, 4, 1));
        assert!(checksum_holds(&bytes));
        let entries = decode_entries(&header, &bytes).unwrap();
        assert_eq!(
            entries,
            vec![WalEntry {
                lsn: lsn(9, 0),
                op_kind: WalOpKind::AddTag as u8,
                timestamp: timestamp(),
                payload: payload.to_vec(),
            }]
        );
    }

    #[test]
    fn a_flipped_bit_fails_the_checksum() {
        let mut bytes = encode_single(1, 0, WalOpKind::AddTag, &[1u8; 16], timestamp()).unwrap();
        bytes[BLOCK_SIZE - 1] ^= 1; // padding is covered too
        assert!(!checksum_holds(&bytes));
    }

    #[test]
    fn timestamps_are_offsets_from_the_base() {
        // Three entries: two at base + 10 ns, one with an explicit timestamp.
        let explicit = HybridTimestamp::new(42, 9, 3);
        let mut entries = Vec::new();
        for delta in [10u32, 10] {
            entries.extend_from_slice(bytemuck::bytes_of(&EntryHead { op_kind: 1, flags: 0, len: 1, hlc_delta: delta }));
            entries.extend_from_slice(&[0; 8]);
        }
        entries.extend_from_slice(bytemuck::bytes_of(&EntryHead {
            op_kind: 2,
            flags: ENTRY_FLAG_HLC_EXPLICIT,
            len: 0,
            hlc_delta: 0,
        }));
        entries.extend_from_slice(bytemuck::bytes_of(&WalHybridTimestampWire::from_logical(explicit)));
        let header = JsetHeader {
            pre: Preamble::new(Structure::JournalSet),
            seq: 2,
            last_seq: 0,
            hlc_base: WalHybridTimestampWire::from_logical(timestamp()),
            payload_len: entries.len() as u32,
            entry_count: 3,
            plain_len: entries.len() as u32,
            flags: 0,
            _pad: 0,
            checksum: 0,
        };
        let mut bytes = bytemuck::bytes_of(&header).to_vec();
        bytes.extend_from_slice(&entries);
        let decoded = decode_entries(&header, &bytes).unwrap();
        let base = timestamp();
        assert_eq!(decoded[0].timestamp, HybridTimestamp::new(base.physical_ns + 10, 3, 7));
        assert_eq!(decoded[1].timestamp, HybridTimestamp::new(base.physical_ns + 10, 4, 7));
        assert_eq!(decoded[2].timestamp, explicit);
        assert_eq!(decoded.iter().map(|entry| entry.lsn).collect::<Vec<_>>(), [lsn(2, 0), lsn(2, 1), lsn(2, 2)]);
    }

    #[test]
    fn an_entry_running_past_the_set_is_malformed() {
        let bytes = encode_single(3, 0, WalOpKind::AddTag, &[0u8; 16], timestamp()).unwrap();
        let (mut header, _) = header_and_size(&bytes).unwrap();
        header.payload_len = 16; // shorter than head + payload
        header.plain_len = 16;
        assert!(matches!(
            decode_entries(&header, &bytes),
            Err(WalError::JsetMalformed { seq: 3, .. })
        ));
    }

    #[test]
    fn a_set_over_256_kib_is_refused() {
        let payload = vec![0u8; JSET_MAX_SIZE];
        assert!(matches!(
            encode_single(1, 0, WalOpKind::AppendChunks, &payload, timestamp()),
            Err(WalError::PayloadTooLarge { .. })
        ));
    }
}
