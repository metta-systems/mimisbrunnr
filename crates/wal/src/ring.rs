//! WAL ring: append, iterate, checkpoint.
//!
//! Implements the runtime side of IMPL §3 — the on-disk header (`WalHeader`)
//! lives in two adjacent A/B blocks at `Superblock.wal_offset`, the active one
//! selected by `BlockHeader.generation`. The ring data area follows
//! immediately after.
//!
//! Per IMPL §3.2 each entry is fully framed inside a single 4 KiB sector and
//! never crosses a sector boundary. The ring is therefore *sector-addressed*:
//! the write/read cursors are byte offsets that always sit on sector
//! boundaries, and an entry that wouldn't fit in the remainder of the current
//! sector is simply written to the next one (the leftover bytes are zero
//! padding the iterator skips over).

use {
    log::trace,
    mimisbrunnr_storage::{BLOCK_SIZE, BlockDevice, block_crc},
    mimisbrunnr_types::HybridTimestamp as LogicalHybridTimestamp,
    serde::Serialize,
};

use crate::{
    entry::{
        WAL_ENTRY_FLAG_COMPRESSED, WAL_ENTRY_FLAG_ENCRYPTED, WAL_FRAMING_CRC_LEN,
        WAL_MAX_PAYLOAD_PLAINTEXT, WAL_SECTOR_SIZE, WalEntryHeader, WalOpKind,
    },
    error::WalError,
    header::WalHeader,
};

/// Per-sector framing: a sector contains at most one entry. The header lives
/// at the start, payload immediately after, and the trailing framing CRC32C
/// occupies the last 4 bytes of the entry frame (not the sector — anything
/// after is zero padding that the iterator skips).
///
/// Encryption (`WAL_ENTRY_FLAG_ENCRYPTED`) and compression
/// (`WAL_ENTRY_FLAG_COMPRESSED`) are recognised here but rejected at append /
/// iter time. Phase 2a only supports plaintext payloads.
//
// TODO(rewrite-phase-N): wire AES-256-GCM for WAL_ENTRY_FLAG_ENCRYPTED
// payloads. Nonce = 64-bit lsn || 32-bit zero. AAD = first 36 bytes of
// WalEntryHeader. Tag (16 B) sits between ciphertext and the framing CRC.
//
// TODO(rewrite-phase-N): wire zstd compression for WAL_ENTRY_FLAG_COMPRESSED
// payloads (compress *before* encrypt — never the reverse, GCM ciphertext
// is incompressible).
pub struct Wal {
    /// Byte offset on the block device where the header / ring starts.
    /// Two 4 KiB header blocks live at `[base..base+8192]`; the ring data
    /// follows from `base + 2*BLOCK_SIZE` to `base + size`.
    base: u64,
    /// Total reservation size on disk (header A + header B + data area).
    /// Must be a non-zero multiple of `BLOCK_SIZE`.
    size: u64,
    /// Cached active header copy (most recent valid generation).
    header: WalHeader,
    /// `0` or `1` — which slot held the active header on the last
    /// `read_or_format`. The next `flush_header` writes to the *other* slot.
    active_slot: u8,
}

/// Per IMPL §3.5 / §2.2: WAL header A/B alternation flips on every commit. We
/// pick the slot by max valid generation; on the first format we write to
/// slot 0.
const HEADER_SLOT_A: u8 = 0;
const HEADER_SLOT_B: u8 = 1;

impl Wal {
    // ---------- Geometry helpers ----------

    /// Number of sectors in the ring data area (i.e. excluding the two
    /// 4 KiB header copies).
    pub fn data_sectors(&self) -> u64 {
        (self.size - 2 * BLOCK_SIZE as u64) / BLOCK_SIZE as u64
    }

    /// Byte size of the ring data area.
    pub fn data_capacity(&self) -> u64 {
        self.data_sectors() * BLOCK_SIZE as u64
    }

    /// Absolute byte offset of the start of the data area.
    fn data_offset(&self) -> u64 {
        self.base + 2 * BLOCK_SIZE as u64
    }

    /// Absolute byte offset of header slot A or B.
    fn header_slot_offset(&self, slot: u8) -> u64 {
        self.base + (slot as u64) * BLOCK_SIZE as u64
    }

    // ---------- Format / open ----------

    /// Initialise a freshly formatted ring: zero header, write to slot A,
    /// leave the data area untouched (bookkeeping is by `next_lsn` /
    /// `write_cursor` only). `size` is the **total** byte reservation,
    /// **including** the two header blocks. Must satisfy:
    ///   - multiple of `BLOCK_SIZE`
    ///   - >= `3 * BLOCK_SIZE` (two headers + at least one data sector)
    pub fn format(
        device: &dyn BlockDevice,
        offset: u64,
        size: u64,
    ) -> Result<Self, WalError> {
        trace!("wal::format base={offset} size={size}");
        if size == 0 || !size.is_multiple_of(BLOCK_SIZE as u64) || size < 3 * BLOCK_SIZE as u64 {
            return Err(WalError::InvalidSize(size));
        }

        let mut header = WalHeader::default();
        header.header.generation = 1;
        header.next_lsn = 1;
        header.write_cursor = 0;
        header.read_cursor = 0;
        header.used_bytes = 0;
        header.last_checkpoint_lsn = 0;
        header.recompute_crc();

        let mut wal = Self {
            base: offset,
            size,
            header,
            active_slot: HEADER_SLOT_B, // so the first flush will write A
        };

        // Initial flush — pick slot A explicitly.
        wal.write_header_to(device, HEADER_SLOT_A)?;
        // Mirror to slot B with a slightly older generation so future commits
        // always overtake it cleanly.
        let mut mirror = wal.header;
        mirror.header.generation = 0;
        mirror.recompute_crc();
        let mirror_bytes = bytemuck::bytes_of(&mirror);
        device.write_at(wal.header_slot_offset(HEADER_SLOT_B), mirror_bytes)?;
        wal.active_slot = HEADER_SLOT_A;
        device.sync()?;

        Ok(wal)
    }

    /// Read the two header copies, pick the one with the larger generation
    /// whose CRC validates, return a runtime ring.
    pub fn open(
        device: &dyn BlockDevice,
        offset: u64,
        size: u64,
    ) -> Result<Self, WalError> {
        trace!("wal::open base={offset} size={size}");
        if size == 0 || !size.is_multiple_of(BLOCK_SIZE as u64) || size < 3 * BLOCK_SIZE as u64 {
            return Err(WalError::InvalidSize(size));
        }

        let mut buf_a = [0u8; BLOCK_SIZE];
        let mut buf_b = [0u8; BLOCK_SIZE];
        device.read_at(offset, &mut buf_a)?;
        device.read_at(offset + BLOCK_SIZE as u64, &mut buf_b)?;

        let cand_a: &WalHeader = bytemuck::from_bytes(&buf_a);
        let cand_b: &WalHeader = bytemuck::from_bytes(&buf_b);

        let a_ok = cand_a.verify_kind().is_ok() && cand_a.verify_crc().is_ok();
        let b_ok = cand_b.verify_kind().is_ok() && cand_b.verify_crc().is_ok();

        let (header, slot) = match (a_ok, b_ok) {
            (true, true) => {
                let gen_a = { cand_a.header.generation };
                let gen_b = { cand_b.header.generation };
                if gen_b > gen_a {
                    (*cand_b, HEADER_SLOT_B)
                } else {
                    (*cand_a, HEADER_SLOT_A)
                }
            }
            (true, false) => (*cand_a, HEADER_SLOT_A),
            (false, true) => (*cand_b, HEADER_SLOT_B),
            (false, false) => return Err(WalError::NoValidHeader),
        };

        Ok(Self {
            base: offset,
            size,
            header,
            active_slot: slot,
        })
    }

    /// Flush the current header to the inactive slot (alternating writes —
    /// the spec's "A/B alternation"), bumping `BlockHeader.generation`.
    fn flush_header(&mut self, device: &dyn BlockDevice) -> Result<(), WalError> {
        let next_slot = if self.active_slot == HEADER_SLOT_A {
            HEADER_SLOT_B
        } else {
            HEADER_SLOT_A
        };
        let new_gen = { self.header.header.generation }
            .checked_add(1)
            .ok_or(WalError::LsnOverflow)?;
        self.header.header.generation = new_gen;
        self.header.recompute_crc();
        self.write_header_to(device, next_slot)?;
        self.active_slot = next_slot;
        Ok(())
    }

    fn write_header_to(
        &self,
        device: &dyn BlockDevice,
        slot: u8,
    ) -> Result<(), WalError> {
        let bytes = bytemuck::bytes_of(&self.header);
        device.write_at(self.header_slot_offset(slot), bytes)?;
        Ok(())
    }

    // ---------- Public state accessors ----------

    pub fn next_lsn(&self) -> u64 {
        self.header.next_lsn
    }

    pub fn write_cursor(&self) -> u64 {
        self.header.write_cursor
    }

    pub fn read_cursor(&self) -> u64 {
        self.header.read_cursor
    }

    pub fn used_bytes(&self) -> u64 {
        self.header.used_bytes
    }

    pub fn last_checkpoint_lsn(&self) -> u64 {
        self.header.last_checkpoint_lsn
    }

    /// Snapshot of the header. Useful for tests / debugging.
    pub fn header_snapshot(&self) -> WalHeader {
        self.header
    }

    // ---------- Append ----------

    /// Append a CBOR-serialised payload as a fresh entry. Returns the assigned
    /// LSN. The producer supplies the `HybridTimestamp` (via the
    /// in-process HLC; see `mimisbrunnr-types`). Panics on `next_lsn`
    /// overflow per IMPL §14.
    pub fn append(
        &mut self,
        device: &dyn BlockDevice,
        op_kind: WalOpKind,
        payload: &impl Serialize,
        ts: LogicalHybridTimestamp,
    ) -> Result<u64, WalError> {
        // Serialise CBOR.
        let mut payload_bytes: Vec<u8> = Vec::new();
        ciborium::ser::into_writer(payload, &mut payload_bytes)?;
        self.append_raw(device, op_kind, &payload_bytes, ts, 0)
    }

    /// Append already-serialised payload bytes. Useful when the caller already
    /// has the encoded CBOR (e.g. from `WalOp::encode`). `flags` bits other
    /// than `WAL_ENTRY_FLAG_*` are ignored; the encrypted/compressed bits are
    /// rejected (Phase 2a).
    pub fn append_raw(
        &mut self,
        device: &dyn BlockDevice,
        op_kind: WalOpKind,
        payload: &[u8],
        ts: LogicalHybridTimestamp,
        flags: u16,
    ) -> Result<u64, WalError> {
        if flags & WAL_ENTRY_FLAG_ENCRYPTED != 0 {
            return Err(WalError::EncryptionUnsupported);
        }
        if flags & WAL_ENTRY_FLAG_COMPRESSED != 0 {
            return Err(WalError::CompressionUnsupported);
        }

        let payload_len = payload.len();
        if payload_len > WAL_MAX_PAYLOAD_PLAINTEXT {
            return Err(WalError::PayloadTooLarge {
                size: payload_len,
                max: WAL_MAX_PAYLOAD_PLAINTEXT,
            });
        }

        // Sector-aligned framing: each entry occupies one sector.
        let frame_len = core::mem::size_of::<WalEntryHeader>() + payload_len + WAL_FRAMING_CRC_LEN;
        debug_assert!(frame_len <= WAL_SECTOR_SIZE);

        let data_cap = self.data_capacity();
        let used = { self.header.used_bytes };
        let sector_cap = BLOCK_SIZE as u64;
        if used + sector_cap > data_cap {
            return Err(WalError::RingFull {
                used,
                capacity: data_cap,
            });
        }

        let lsn = { self.header.next_lsn };
        // IMPL §14: panic on LSN overflow.
        let next_lsn = lsn.checked_add(1).expect("WAL next_lsn overflow");

        // Build entry header with payload_crc filled in.
        let mut hdr = WalEntryHeader::new(op_kind, flags, lsn, ts, payload_len as u32);
        hdr.payload_crc = block_crc(payload);

        // Assemble sector buffer (zero-padded tail).
        let mut sector = [0u8; WAL_SECTOR_SIZE];
        let hdr_bytes = bytemuck::bytes_of(&hdr);
        sector[..hdr_bytes.len()].copy_from_slice(hdr_bytes);
        let payload_off = hdr_bytes.len();
        sector[payload_off..payload_off + payload_len].copy_from_slice(payload);
        // Trailing framing CRC = CRC32C(WalEntryHeader || payload).
        let frame_crc = block_crc(&sector[..hdr_bytes.len() + payload_len]);
        let crc_off = hdr_bytes.len() + payload_len;
        sector[crc_off..crc_off + WAL_FRAMING_CRC_LEN]
            .copy_from_slice(&frame_crc.to_le_bytes());

        // Write the sector at the current cursor.
        let cursor = { self.header.write_cursor };
        let abs_offset = self.data_offset() + cursor;
        device.write_at(abs_offset, &sector)?;

        // Advance bookkeeping.
        let new_cursor = (cursor + sector_cap) % data_cap;
        self.header.write_cursor = new_cursor;
        self.header.used_bytes = used + sector_cap;
        self.header.next_lsn = next_lsn;
        self.flush_header(device)?;
        device.sync()?;

        trace!("wal::append lsn={lsn} kind={op_kind:?} payload_len={payload_len}");
        Ok(lsn)
    }

    // ---------- Iterate ----------

    /// Replay iterator: walk every entry currently in the ring (from
    /// `read_cursor` forward, stopping after `used_bytes`) and yield those
    /// with `lsn >= start_lsn`.
    pub fn iter_from<'a>(
        &self,
        device: &'a dyn BlockDevice,
        start_lsn: u64,
    ) -> WalIter<'a> {
        WalIter {
            device,
            data_offset: self.data_offset(),
            data_capacity: self.data_capacity(),
            cursor: self.header.read_cursor,
            remaining: self.header.used_bytes,
            start_lsn,
            done: false,
        }
    }

    // ---------- Checkpoint ----------

    /// Advance `last_checkpoint_lsn` and `read_cursor` past every sector whose
    /// entry has `lsn <= last_checkpoint_lsn`. Returns the number of sectors
    /// reclaimed.
    pub fn checkpoint(
        &mut self,
        device: &dyn BlockDevice,
        last_checkpoint_lsn: u64,
    ) -> Result<u64, WalError> {
        let mut reclaimed: u64 = 0;
        let data_cap = self.data_capacity();
        let sector_cap = BLOCK_SIZE as u64;

        // Walk forward sector-by-sector — same logic as WalIter but mutating.
        loop {
            if self.header.used_bytes == 0 {
                break;
            }
            let cursor = { self.header.read_cursor };
            let abs = self.data_offset() + cursor;
            let mut sector = [0u8; WAL_SECTOR_SIZE];
            device.read_at(abs, &mut sector)?;
            let hdr: &WalEntryHeader =
                bytemuck::from_bytes(&sector[..core::mem::size_of::<WalEntryHeader>()]);
            // If the slot is empty / never written, we wouldn't expect to see
            // it inside `used_bytes`. Treat as end-of-window defensively.
            if hdr.check_magic().is_err() {
                break;
            }
            let entry_lsn = { hdr.lsn };
            if entry_lsn > last_checkpoint_lsn {
                break;
            }
            self.header.read_cursor = (cursor + sector_cap) % data_cap;
            self.header.used_bytes -= sector_cap;
            reclaimed += 1;
        }

        self.header.last_checkpoint_lsn = last_checkpoint_lsn;
        self.flush_header(device)?;
        device.sync()?;
        Ok(reclaimed)
    }
}

/// Replay iterator. Yields `(WalEntryHeader, payload_bytes)` pairs.
pub struct WalIter<'a> {
    device: &'a dyn BlockDevice,
    data_offset: u64,
    data_capacity: u64,
    cursor: u64,
    remaining: u64,
    start_lsn: u64,
    done: bool,
}

/// One decoded WAL entry yielded by [`WalIter`].
#[derive(Debug, Clone)]
pub struct WalEntry {
    pub header: WalEntryHeader,
    pub payload: Vec<u8>,
}

impl Iterator for WalIter<'_> {
    type Item = Result<WalEntry, WalError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        let sector_cap = BLOCK_SIZE as u64;
        loop {
            if self.remaining == 0 {
                self.done = true;
                return None;
            }
            let abs = self.data_offset + self.cursor;
            let mut sector = [0u8; WAL_SECTOR_SIZE];
            if let Err(e) = self.device.read_at(abs, &mut sector) {
                self.done = true;
                return Some(Err(WalError::from(e)));
            }
            // Advance cursor before yielding so an early return still lets
            // callers continue.
            self.cursor = (self.cursor + sector_cap) % self.data_capacity;
            self.remaining -= sector_cap;

            // Decode header.
            let hdr_size = core::mem::size_of::<WalEntryHeader>();
            let hdr: &WalEntryHeader = bytemuck::from_bytes(&sector[..hdr_size]);
            if let Err(e) = hdr.check_magic() {
                self.done = true;
                return Some(Err(e));
            }
            let payload_len = { hdr.payload_length } as usize;
            if hdr_size + payload_len + WAL_FRAMING_CRC_LEN > WAL_SECTOR_SIZE {
                self.done = true;
                return Some(Err(WalError::PayloadTooLarge {
                    size: payload_len,
                    max: WAL_MAX_PAYLOAD_PLAINTEXT,
                }));
            }

            // Verify trailing framing CRC.
            let frame_end = hdr_size + payload_len;
            let stored_crc = u32::from_le_bytes([
                sector[frame_end],
                sector[frame_end + 1],
                sector[frame_end + 2],
                sector[frame_end + 3],
            ]);
            let computed = block_crc(&sector[..frame_end]);
            if stored_crc != computed {
                self.done = true;
                return Some(Err(WalError::CrcMismatch {
                    expected: stored_crc,
                    actual: computed,
                }));
            }

            // Verify payload CRC (computed over plaintext payload bytes).
            let payload_slice = &sector[hdr_size..hdr_size + payload_len];
            let stored_payload_crc = { hdr.payload_crc };
            let computed_payload_crc = block_crc(payload_slice);
            if stored_payload_crc != computed_payload_crc {
                self.done = true;
                return Some(Err(WalError::CrcMismatch {
                    expected: stored_payload_crc,
                    actual: computed_payload_crc,
                }));
            }

            // Phase 2a: reject encrypted/compressed payloads explicitly.
            let flags = { hdr.flags };
            if flags & WAL_ENTRY_FLAG_ENCRYPTED != 0 {
                self.done = true;
                return Some(Err(WalError::EncryptionUnsupported));
            }
            if flags & WAL_ENTRY_FLAG_COMPRESSED != 0 {
                self.done = true;
                return Some(Err(WalError::CompressionUnsupported));
            }

            // Skip entries below start_lsn (still consume the sector).
            let entry_lsn = { hdr.lsn };
            if entry_lsn < self.start_lsn {
                continue;
            }

            return Some(Ok(WalEntry {
                header: *hdr,
                payload: payload_slice.to_vec(),
            }));
        }
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::entry::{CreateObject, WalOp},
        mimisbrunnr_storage::{BlockDevice, FileBlockDevice},
        std::sync::Arc,
        tempfile::NamedTempFile,
    };

    /// Wraps an `Arc<dyn BlockDevice>` for tests where we want to call &dyn
    /// methods without dragging the file path around.
    fn open_test_dev(size: u64) -> (NamedTempFile, Arc<dyn BlockDevice>) {
        let tmp = NamedTempFile::new().unwrap();
        let dev: Arc<dyn BlockDevice> =
            Arc::new(FileBlockDevice::open(tmp.path(), size).unwrap());
        (tmp, dev)
    }

    fn ts() -> LogicalHybridTimestamp {
        LogicalHybridTimestamp::new(1_700_000_000_000_000_000, 0, 1)
    }

    #[test]
    fn format_then_open_returns_empty_ring() {
        let size = 16 * BLOCK_SIZE as u64;
        let (_tmp, dev) = open_test_dev(size);
        Wal::format(&*dev, 0, size).unwrap();
        let wal = Wal::open(&*dev, 0, size).unwrap();
        assert_eq!(wal.next_lsn(), 1);
        assert_eq!(wal.used_bytes(), 0);
        assert_eq!(wal.read_cursor(), 0);
        assert_eq!(wal.write_cursor(), 0);
    }

    #[test]
    fn append_round_trip_create_object() {
        let size = 16 * BLOCK_SIZE as u64;
        let (_tmp, dev) = open_test_dev(size);
        let mut wal = Wal::format(&*dev, 0, size).unwrap();

        let op = CreateObject {
            oid: 0xdead_beef,
            generation: 1,
            created_ns: 12345,
        };
        let lsn = wal.append(&*dev, WalOpKind::CreateObject, &op, ts()).unwrap();
        assert_eq!(lsn, 1);

        let entries: Vec<WalEntry> = wal.iter_from(&*dev, 0).map(|r| r.unwrap()).collect();
        assert_eq!(entries.len(), 1);
        let kind = WalOpKind::from_u8(entries[0].header.op_kind).unwrap();
        let decoded = WalOp::decode(kind, &entries[0].payload).unwrap();
        match decoded {
            WalOp::CreateObject(d) => assert_eq!(d, op),
            _ => panic!("wrong op variant"),
        }
    }

    #[test]
    fn lsn_strictly_monotonic_over_100_appends() {
        let size = 256 * BLOCK_SIZE as u64;
        let (_tmp, dev) = open_test_dev(size);
        let mut wal = Wal::format(&*dev, 0, size).unwrap();
        let mut last = 0u64;
        for i in 0..100u32 {
            let op = CreateObject {
                oid: i as u64,
                generation: 0,
                created_ns: 0,
            };
            let lsn = wal.append(&*dev, WalOpKind::CreateObject, &op, ts()).unwrap();
            assert!(lsn > last, "lsn not monotonic at iter {i}");
            last = lsn;
        }
        assert_eq!(last, 100);
    }

    #[test]
    fn payload_over_cap_is_rejected() {
        let size = 16 * BLOCK_SIZE as u64;
        let (_tmp, dev) = open_test_dev(size);
        let mut wal = Wal::format(&*dev, 0, size).unwrap();
        let big = vec![0u8; WAL_MAX_PAYLOAD_PLAINTEXT + 1];
        let err = wal
            .append_raw(&*dev, WalOpKind::WriteBlob, &big, ts(), 0)
            .unwrap_err();
        assert!(matches!(err, WalError::PayloadTooLarge { .. }));
    }

    #[test]
    fn payload_at_exact_cap_succeeds() {
        let size = 16 * BLOCK_SIZE as u64;
        let (_tmp, dev) = open_test_dev(size);
        let mut wal = Wal::format(&*dev, 0, size).unwrap();
        let exact = vec![0xabu8; WAL_MAX_PAYLOAD_PLAINTEXT];
        let lsn = wal
            .append_raw(&*dev, WalOpKind::WriteBlob, &exact, ts(), 0)
            .unwrap();
        assert_eq!(lsn, 1);
        let entries: Vec<WalEntry> = wal.iter_from(&*dev, 0).map(|r| r.unwrap()).collect();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].payload.len(), WAL_MAX_PAYLOAD_PLAINTEXT);
    }

    #[test]
    fn corrupting_payload_byte_yields_crc_mismatch() {
        let size = 16 * BLOCK_SIZE as u64;
        let (_tmp, dev) = open_test_dev(size);
        let mut wal = Wal::format(&*dev, 0, size).unwrap();
        let op = CreateObject {
            oid: 1,
            generation: 0,
            created_ns: 0,
        };
        wal.append(&*dev, WalOpKind::CreateObject, &op, ts()).unwrap();
        // Corrupt one byte deep in the first data sector.
        let abs = 2 * BLOCK_SIZE as u64 + (core::mem::size_of::<WalEntryHeader>() as u64) + 2;
        let mut byte = [0u8; 1];
        dev.read_at(abs, &mut byte).unwrap();
        byte[0] ^= 0xFF;
        dev.write_at(abs, &byte).unwrap();

        let result: Vec<_> = wal.iter_from(&*dev, 0).collect();
        assert_eq!(result.len(), 1);
        let err = result[0].as_ref().unwrap_err();
        assert!(matches!(err, WalError::CrcMismatch { .. }));
    }

    #[test]
    fn active_header_picks_higher_generation() {
        let size = 16 * BLOCK_SIZE as u64;
        let (_tmp, dev) = open_test_dev(size);
        // Hand-craft two headers: A with generation=5, B with generation=7.
        let mut a = WalHeader::default();
        a.header.generation = 5;
        a.next_lsn = 100;
        a.recompute_crc();
        let mut b = WalHeader::default();
        b.header.generation = 7;
        b.next_lsn = 200;
        b.recompute_crc();
        dev.write_at(0, bytemuck::bytes_of(&a)).unwrap();
        dev.write_at(BLOCK_SIZE as u64, bytemuck::bytes_of(&b)).unwrap();

        let wal = Wal::open(&*dev, 0, size).unwrap();
        assert_eq!(wal.next_lsn(), 200);
    }

    #[test]
    fn checkpoint_advances_read_cursor() {
        let size = 16 * BLOCK_SIZE as u64;
        let (_tmp, dev) = open_test_dev(size);
        let mut wal = Wal::format(&*dev, 0, size).unwrap();
        for i in 0..5u32 {
            let op = CreateObject {
                oid: i as u64,
                generation: 0,
                created_ns: 0,
            };
            wal.append(&*dev, WalOpKind::CreateObject, &op, ts()).unwrap();
        }
        let used_before = wal.used_bytes();
        let read_before = wal.read_cursor();
        let reclaimed = wal.checkpoint(&*dev, 3).unwrap();
        assert_eq!(reclaimed, 3);
        assert_eq!(wal.last_checkpoint_lsn(), 3);
        assert!(wal.read_cursor() > read_before);
        assert!(wal.used_bytes() < used_before);
        // iter_from(0) now sees only the unreclaimed entries: lsn 4, 5.
        let surviving: Vec<u64> = wal
            .iter_from(&*dev, 0)
            .map(|r| { r.unwrap().header.lsn })
            .collect();
        assert_eq!(surviving, vec![4, 5]);
    }

    #[test]
    fn iter_from_filters_by_start_lsn() {
        let size = 16 * BLOCK_SIZE as u64;
        let (_tmp, dev) = open_test_dev(size);
        let mut wal = Wal::format(&*dev, 0, size).unwrap();
        for i in 0..5u32 {
            let op = CreateObject {
                oid: i as u64,
                generation: 0,
                created_ns: 0,
            };
            wal.append(&*dev, WalOpKind::CreateObject, &op, ts()).unwrap();
        }
        let collected: Vec<u64> = wal
            .iter_from(&*dev, 3)
            .map(|r| { r.unwrap().header.lsn })
            .collect();
        assert_eq!(collected, vec![3, 4, 5]);
    }

    #[test]
    fn invalid_size_rejected() {
        let (_tmp, dev) = open_test_dev(4096);
        match Wal::format(&*dev, 0, 4096) {
            Err(WalError::InvalidSize(_)) => {}
            _ => panic!("expected InvalidSize for 4096"),
        }
        match Wal::format(&*dev, 0, 4097) {
            Err(WalError::InvalidSize(_)) => {}
            _ => panic!("expected InvalidSize for 4097"),
        }
    }

    #[test]
    fn hand_built_op_kind_decodes_to_right_variant() {
        // Build a WriteBlob payload by hand and verify that supplying
        // op_kind = 8 (the pinned discriminant) decodes to the right variant.
        let payload = crate::entry::WriteBlob {
            oid: 7,
            content_hash: [0xaa; 32],
            extent: mimisbrunnr_storage::BlockRef::new(0, 1, 1).into(),
            size: 4096,
        };
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&payload, &mut bytes).unwrap();

        let kind = WalOpKind::from_u8(8).unwrap();
        assert_eq!(kind, WalOpKind::WriteBlob);
        let op = WalOp::decode(kind, &bytes).unwrap();
        match op {
            WalOp::WriteBlob(p) => assert_eq!(p, payload),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn append_raw_rejects_encrypted_flag() {
        let size = 16 * BLOCK_SIZE as u64;
        let (_tmp, dev) = open_test_dev(size);
        let mut wal = Wal::format(&*dev, 0, size).unwrap();
        let err = wal
            .append_raw(
                &*dev,
                WalOpKind::CreateObject,
                &[1u8, 2, 3],
                ts(),
                WAL_ENTRY_FLAG_ENCRYPTED,
            )
            .unwrap_err();
        assert!(matches!(err, WalError::EncryptionUnsupported));
    }
}
