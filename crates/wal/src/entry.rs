/// The kind of operation recorded in a WAL entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum WalOpKind {
    /// Create a new object.
    CreateObject = 1,
    /// Delete (tombstone) an object.
    DeleteObject = 2,
    /// Add a tag to an object.
    AddTag = 3,
    /// Remove a tag from an object.
    RemoveTag = 4,
    /// Set an attribute on an object.
    SetAttr = 5,
    /// Remove an attribute from an object.
    RemoveAttr = 6,
    /// Add a relation between objects.
    AddRelation = 7,
    /// Remove a relation between objects.
    RemoveRelation = 8,
    /// Write blob data.
    WriteBlob = 9,
    /// Checkpoint marker — all preceding entries are flushed.
    Checkpoint = 10,
}

impl WalOpKind {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            1 => Some(Self::CreateObject),
            2 => Some(Self::DeleteObject),
            3 => Some(Self::AddTag),
            4 => Some(Self::RemoveTag),
            5 => Some(Self::SetAttr),
            6 => Some(Self::RemoveAttr),
            7 => Some(Self::AddRelation),
            8 => Some(Self::RemoveRelation),
            9 => Some(Self::WriteBlob),
            10 => Some(Self::Checkpoint),
            _ => None,
        }
    }
}

/// A single WAL entry.
///
/// On-disk format (all little-endian):
/// ```text
///  [0..8]    lsn (log sequence number)
///  [8..9]    op_kind
///  [9..13]   payload_length (u32)
///  [13..]    payload (variable)
///  [..+4]    crc32 of [0..payload_end]
/// ```
///
/// Header is 13 bytes, trailer is 4 bytes (CRC).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalEntry {
    pub lsn: u64,
    pub op_kind: WalOpKind,
    pub payload: Vec<u8>,
}

pub const ENTRY_HEADER_SIZE: usize = 13;
pub const ENTRY_TRAILER_SIZE: usize = 4;
pub const ENTRY_OVERHEAD: usize = ENTRY_HEADER_SIZE + ENTRY_TRAILER_SIZE;

/// Maximum payload size per entry (64 KiB).
pub const MAX_PAYLOAD_SIZE: usize = 64 * 1024;

impl WalEntry {
    pub fn new(lsn: u64, op_kind: WalOpKind, payload: Vec<u8>) -> Self {
        Self {
            lsn,
            op_kind,
            payload,
        }
    }

    /// Total on-disk size of this entry.
    pub fn disk_size(&self) -> usize {
        ENTRY_OVERHEAD + self.payload.len()
    }

    /// Serialize to bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(self.disk_size());
        buf.extend_from_slice(&self.lsn.to_le_bytes());
        buf.push(self.op_kind as u8);
        buf.extend_from_slice(&(self.payload.len() as u32).to_le_bytes());
        buf.extend_from_slice(&self.payload);
        let crc = crc32fast::hash(&buf);
        buf.extend_from_slice(&crc.to_le_bytes());
        buf
    }

    /// Deserialize from bytes. Returns the entry and the number of bytes consumed.
    pub fn from_bytes(buf: &[u8]) -> Result<(Self, usize), String> {
        if buf.len() < ENTRY_HEADER_SIZE + ENTRY_TRAILER_SIZE {
            return Err("buffer too small for WAL entry".into());
        }

        let lsn = u64::from_le_bytes(buf[0..8].try_into().unwrap());
        let op_kind = WalOpKind::from_u8(buf[8]).ok_or("invalid op kind")?;
        let payload_len = u32::from_le_bytes(buf[9..13].try_into().unwrap()) as usize;

        let total = ENTRY_HEADER_SIZE + payload_len + ENTRY_TRAILER_SIZE;
        if buf.len() < total {
            return Err(format!(
                "buffer too small: need {total}, have {}",
                buf.len()
            ));
        }

        let payload = buf[ENTRY_HEADER_SIZE..ENTRY_HEADER_SIZE + payload_len].to_vec();
        let data_end = ENTRY_HEADER_SIZE + payload_len;
        let stored_crc = u32::from_le_bytes(buf[data_end..data_end + 4].try_into().unwrap());
        let computed_crc = crc32fast::hash(&buf[..data_end]);

        if stored_crc != computed_crc {
            return Err(format!(
                "CRC mismatch: stored={stored_crc:#x}, computed={computed_crc:#x}"
            ));
        }

        Ok((
            Self {
                lsn,
                op_kind,
                payload,
            },
            total,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let entry = WalEntry::new(42, WalOpKind::AddTag, vec![1, 2, 3, 4]);
        let bytes = entry.to_bytes();
        let (decoded, consumed) = WalEntry::from_bytes(&bytes).unwrap();
        assert_eq!(decoded, entry);
        assert_eq!(consumed, bytes.len());
    }

    #[test]
    fn empty_payload() {
        let entry = WalEntry::new(1, WalOpKind::Checkpoint, vec![]);
        let bytes = entry.to_bytes();
        let (decoded, _) = WalEntry::from_bytes(&bytes).unwrap();
        assert_eq!(decoded, entry);
    }

    #[test]
    fn corrupted_data_detected() {
        let entry = WalEntry::new(1, WalOpKind::CreateObject, vec![0xFF; 10]);
        let mut bytes = entry.to_bytes();
        bytes[5] ^= 0xFF; // Flip a byte
        assert!(WalEntry::from_bytes(&bytes).is_err());
    }

    #[test]
    fn truncated_buffer() {
        let entry = WalEntry::new(1, WalOpKind::AddTag, vec![1, 2, 3]);
        let bytes = entry.to_bytes();
        assert!(WalEntry::from_bytes(&bytes[..5]).is_err());
    }

    #[test]
    fn disk_size() {
        let entry = WalEntry::new(0, WalOpKind::AddTag, vec![0; 100]);
        assert_eq!(entry.disk_size(), ENTRY_OVERHEAD + 100);
    }

    #[test]
    fn all_op_kinds_round_trip() {
        for v in 1..=10u8 {
            let op = WalOpKind::from_u8(v).unwrap();
            assert_eq!(op as u8, v);
        }
        assert!(WalOpKind::from_u8(0).is_none());
        assert!(WalOpKind::from_u8(11).is_none());
    }
}
