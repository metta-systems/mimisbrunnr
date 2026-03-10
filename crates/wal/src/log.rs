use mimisbrunnr_storage::BlockDevice;

use crate::entry::{WalEntry, WalOpKind, ENTRY_OVERHEAD, MAX_PAYLOAD_SIZE};
use crate::WalError;
use log::trace;

/// Write-Ahead Log: a circular buffer on a block device region.
///
/// All mutations go through the WAL before being applied to indexes/metadata.
/// On crash, replay from last checkpoint restores consistent state.
pub struct WriteAheadLog {
    /// Offset on the block device where the WAL region starts.
    region_offset: u64,
    /// Total size of the WAL region in bytes.
    region_size: u64,
    /// Next LSN to assign.
    next_lsn: u64,
    /// Write cursor: byte offset within the WAL region (wraps around).
    write_cursor: u64,
    /// Read cursor: byte offset of the oldest valid entry.
    read_cursor: u64,
    /// Number of bytes currently used.
    used: u64,
    /// LSN of the last checkpoint.
    last_checkpoint_lsn: u64,
}

/// WAL header stored at region_offset (32 bytes).
/// ```text
///  [0..8]   magic "WALHEAD\0"
///  [8..16]  next_lsn
///  [16..24] write_cursor
///  [24..32] read_cursor
///  [32..40] used
///  [40..48] last_checkpoint_lsn
///  [48..52] crc32
///  [52..64] reserved
/// ```
const WAL_HEADER_SIZE: u64 = 64;
const WAL_HEADER_MAGIC: [u8; 8] = *b"WALHEAD\0";

impl WriteAheadLog {
    /// Create a new WAL on the given region of a block device.
    /// Writes the initial header.
    pub fn create(
        dev: &dyn BlockDevice,
        region_offset: u64,
        region_size: u64,
    ) -> Result<Self, WalError> {
        trace!("wal::create region_offset={:#x} region_size={:#x}", region_offset, region_size);
        let wal = Self {
            region_offset,
            region_size,
            next_lsn: 1,
            write_cursor: 0,
            read_cursor: 0,
            used: 0,
            last_checkpoint_lsn: 0,
        };
        wal.write_header(dev)?;
        Ok(wal)
    }

    /// Open an existing WAL by reading its header.
    pub fn open(
        dev: &dyn BlockDevice,
        region_offset: u64,
        region_size: u64,
    ) -> Result<Self, WalError> {
        trace!("wal::open region_offset={:#x} region_size={:#x}", region_offset, region_size);
        let mut header = [0u8; WAL_HEADER_SIZE as usize];
        dev.read_at(region_offset, &mut header)?;

        if header[0..8] != WAL_HEADER_MAGIC {
            return Err(WalError::Corrupted {
                offset: region_offset,
                reason: "invalid WAL header magic".into(),
            });
        }

        let stored_crc = u32::from_le_bytes(header[48..52].try_into().unwrap());
        let computed_crc = crc32fast::hash(&header[0..48]);
        if stored_crc != computed_crc {
            return Err(WalError::Corrupted {
                offset: region_offset,
                reason: "WAL header CRC mismatch".into(),
            });
        }

        Ok(Self {
            region_offset,
            region_size,
            next_lsn: u64::from_le_bytes(header[8..16].try_into().unwrap()),
            write_cursor: u64::from_le_bytes(header[16..24].try_into().unwrap()),
            read_cursor: u64::from_le_bytes(header[24..32].try_into().unwrap()),
            used: u64::from_le_bytes(header[32..40].try_into().unwrap()),
            last_checkpoint_lsn: u64::from_le_bytes(header[40..48].try_into().unwrap()),
        })
    }

    /// Data area starts after the header.
    fn data_offset(&self) -> u64 {
        self.region_offset + WAL_HEADER_SIZE
    }

    /// Usable data capacity (region minus header).
    fn data_capacity(&self) -> u64 {
        self.region_size - WAL_HEADER_SIZE
    }

    /// Append an entry to the WAL. Returns the assigned LSN.
    pub fn append(
        &mut self,
        dev: &dyn BlockDevice,
        op_kind: WalOpKind,
        payload: &[u8],
    ) -> Result<u64, WalError> {
        trace!("wal::append op={:?} payload_len={} lsn={}", op_kind, payload.len(), self.next_lsn);
        if payload.len() > MAX_PAYLOAD_SIZE {
            return Err(WalError::EntryTooLarge {
                size: payload.len(),
                max: MAX_PAYLOAD_SIZE,
            });
        }

        let entry = WalEntry::new(self.next_lsn, op_kind, payload.to_vec());
        let bytes = entry.to_bytes();
        let entry_size = bytes.len() as u64;

        if entry_size > self.data_capacity() - self.used {
            return Err(WalError::Full {
                capacity: self.data_capacity(),
                used: self.used,
            });
        }

        // Write entry, handling wrap-around
        let data_cap = self.data_capacity();
        let data_off = self.data_offset();

        let first_chunk = ((data_cap - self.write_cursor) as usize).min(bytes.len());
        dev.write_at(data_off + self.write_cursor, &bytes[..first_chunk])?;
        if first_chunk < bytes.len() {
            dev.write_at(data_off, &bytes[first_chunk..])?;
        }

        self.write_cursor = (self.write_cursor + entry_size) % data_cap;
        self.used += entry_size;
        let lsn = self.next_lsn;
        self.next_lsn += 1;

        self.write_header(dev)?;

        Ok(lsn)
    }

    /// Read all entries from the WAL (from oldest to newest).
    pub fn read_all(&self, dev: &dyn BlockDevice) -> Result<Vec<WalEntry>, WalError> {
        if self.used == 0 {
            return Ok(vec![]);
        }

        // Read the entire data area into memory for simplicity
        let data = self.read_data_region(dev)?;
        let mut entries = Vec::new();
        let mut offset = self.read_cursor as usize;
        let mut remaining = self.used as usize;

        while remaining >= ENTRY_OVERHEAD {
            // Linearize the circular buffer at current offset
            let linearized = self.linearize_at(&data, offset, remaining);
            match WalEntry::from_bytes(&linearized) {
                Ok((entry, consumed)) => {
                    offset = (offset + consumed) % data.len();
                    remaining -= consumed;
                    entries.push(entry);
                }
                Err(reason) => {
                    return Err(WalError::Corrupted {
                        offset: self.data_offset() + offset as u64,
                        reason,
                    });
                }
            }
        }

        Ok(entries)
    }

    /// Read entries starting from a given LSN.
    pub fn read_from_lsn(
        &self,
        dev: &dyn BlockDevice,
        start_lsn: u64,
    ) -> Result<Vec<WalEntry>, WalError> {
        let all = self.read_all(dev)?;
        if !all.is_empty() && start_lsn < all[0].lsn {
            return Err(WalError::LsnNotFound {
                requested: start_lsn,
                oldest: all[0].lsn,
            });
        }
        Ok(all.into_iter().filter(|e| e.lsn >= start_lsn).collect())
    }

    /// Write a checkpoint marker and advance the read cursor past all checkpointed entries.
    pub fn checkpoint(&mut self, dev: &dyn BlockDevice) -> Result<u64, WalError> {
        trace!("wal::checkpoint lsn={}", self.next_lsn);
        let lsn = self.append(dev, WalOpKind::Checkpoint, &[])?;
        // Advance read cursor to write cursor (all entries are now checkpointed)
        self.read_cursor = self.write_cursor;
        self.used = 0;
        self.last_checkpoint_lsn = lsn;
        self.write_header(dev)?;
        Ok(lsn)
    }

    pub fn next_lsn(&self) -> u64 {
        self.next_lsn
    }

    pub fn last_checkpoint_lsn(&self) -> u64 {
        self.last_checkpoint_lsn
    }

    pub fn used_bytes(&self) -> u64 {
        self.used
    }

    fn write_header(&self, dev: &dyn BlockDevice) -> Result<(), WalError> {
        trace!("wal::write_header next_lsn={} used={}", self.next_lsn, self.used);
        let mut header = [0u8; WAL_HEADER_SIZE as usize];
        header[0..8].copy_from_slice(&WAL_HEADER_MAGIC);
        header[8..16].copy_from_slice(&self.next_lsn.to_le_bytes());
        header[16..24].copy_from_slice(&self.write_cursor.to_le_bytes());
        header[24..32].copy_from_slice(&self.read_cursor.to_le_bytes());
        header[32..40].copy_from_slice(&self.used.to_le_bytes());
        header[40..48].copy_from_slice(&self.last_checkpoint_lsn.to_le_bytes());
        let crc = crc32fast::hash(&header[0..48]);
        header[48..52].copy_from_slice(&crc.to_le_bytes());
        dev.write_at(self.region_offset, &header)?;
        Ok(())
    }

    fn read_data_region(&self, dev: &dyn BlockDevice) -> Result<Vec<u8>, WalError> {
        let cap = self.data_capacity() as usize;
        let mut data = vec![0u8; cap];
        dev.read_at(self.data_offset(), &mut data)?;
        Ok(data)
    }

    /// Produce a linearized view of `len` bytes starting at circular `offset`.
    fn linearize_at(&self, data: &[u8], offset: usize, len: usize) -> Vec<u8> {
        let cap = data.len();
        let actual_len = len.min(cap);
        let mut result = Vec::with_capacity(actual_len);
        let first = (cap - offset).min(actual_len);
        result.extend_from_slice(&data[offset..offset + first]);
        if first < actual_len {
            result.extend_from_slice(&data[..actual_len - first]);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mimisbrunnr_storage::FileBlockDevice;
    use tempfile::NamedTempFile;

    fn test_wal() -> (NamedTempFile, FileBlockDevice, WriteAheadLog) {
        let tmp = NamedTempFile::new().unwrap();
        let region_size = 64 * 1024; // 64 KiB for tests
        let dev = FileBlockDevice::open(tmp.path(), region_size).unwrap();
        let wal = WriteAheadLog::create(&dev, 0, region_size).unwrap();
        (tmp, dev, wal)
    }

    #[test]
    fn create_and_reopen() {
        let (_tmp, dev, wal) = test_wal();
        assert_eq!(wal.next_lsn(), 1);
        assert_eq!(wal.used_bytes(), 0);

        let wal2 = WriteAheadLog::open(&dev, 0, 64 * 1024).unwrap();
        assert_eq!(wal2.next_lsn(), 1);
    }

    #[test]
    fn append_and_read_back() {
        let (_tmp, dev, mut wal) = test_wal();

        let lsn1 = wal.append(&dev, WalOpKind::CreateObject, b"obj1").unwrap();
        let lsn2 = wal.append(&dev, WalOpKind::AddTag, b"tag_data").unwrap();

        assert_eq!(lsn1, 1);
        assert_eq!(lsn2, 2);

        let entries = wal.read_all(&dev).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].lsn, 1);
        assert_eq!(entries[0].op_kind, WalOpKind::CreateObject);
        assert_eq!(entries[0].payload, b"obj1");
        assert_eq!(entries[1].lsn, 2);
        assert_eq!(entries[1].op_kind, WalOpKind::AddTag);
    }

    #[test]
    fn read_from_lsn() {
        let (_tmp, dev, mut wal) = test_wal();

        wal.append(&dev, WalOpKind::CreateObject, b"a").unwrap();
        wal.append(&dev, WalOpKind::AddTag, b"b").unwrap();
        wal.append(&dev, WalOpKind::SetAttr, b"c").unwrap();

        let from2 = wal.read_from_lsn(&dev, 2).unwrap();
        assert_eq!(from2.len(), 2);
        assert_eq!(from2[0].lsn, 2);
        assert_eq!(from2[1].lsn, 3);
    }

    #[test]
    fn checkpoint_clears_used() {
        let (_tmp, dev, mut wal) = test_wal();

        wal.append(&dev, WalOpKind::CreateObject, b"data").unwrap();
        wal.append(&dev, WalOpKind::AddTag, b"more").unwrap();
        assert!(wal.used_bytes() > 0);

        let cp_lsn = wal.checkpoint(&dev).unwrap();
        assert_eq!(wal.used_bytes(), 0);
        assert_eq!(wal.last_checkpoint_lsn(), cp_lsn);
    }

    #[test]
    fn reopen_after_writes() {
        let (_tmp, dev, mut wal) = test_wal();
        let region_size = 64 * 1024;

        wal.append(&dev, WalOpKind::CreateObject, b"hello").unwrap();
        wal.append(&dev, WalOpKind::AddTag, b"world").unwrap();

        let wal2 = WriteAheadLog::open(&dev, 0, region_size).unwrap();
        let entries = wal2.read_all(&dev).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].payload, b"hello");
        assert_eq!(entries[1].payload, b"world");
    }

    #[test]
    fn many_entries() {
        let (_tmp, dev, mut wal) = test_wal();

        for i in 0..100u32 {
            wal.append(&dev, WalOpKind::AddTag, &i.to_le_bytes()).unwrap();
        }

        let entries = wal.read_all(&dev).unwrap();
        assert_eq!(entries.len(), 100);
        for (i, entry) in entries.iter().enumerate() {
            assert_eq!(entry.lsn, (i + 1) as u64);
            let val = u32::from_le_bytes(entry.payload.as_slice().try_into().unwrap());
            assert_eq!(val, i as u32);
        }
    }

    #[test]
    fn entry_too_large() {
        let (_tmp, dev, mut wal) = test_wal();
        let big = vec![0u8; MAX_PAYLOAD_SIZE + 1];
        assert!(matches!(
            wal.append(&dev, WalOpKind::WriteBlob, &big),
            Err(WalError::EntryTooLarge { .. })
        ));
    }

    #[test]
    fn wal_full() {
        let tmp = NamedTempFile::new().unwrap();
        let region_size = 256; // Tiny WAL
        let dev = FileBlockDevice::open(tmp.path(), region_size).unwrap();
        let mut wal = WriteAheadLog::create(&dev, 0, region_size).unwrap();

        // Fill it up
        let mut count = 0;
        loop {
            match wal.append(&dev, WalOpKind::AddTag, b"x") {
                Ok(_) => count += 1,
                Err(WalError::Full { .. }) => break,
                Err(e) => panic!("unexpected error: {e}"),
            }
        }
        assert!(count > 0);
    }
}
