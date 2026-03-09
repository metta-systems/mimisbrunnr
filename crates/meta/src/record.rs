use mimisbrunnr_types::{ObjectState, CompressionState, EncryptionState};

/// Fixed-size on-disk object record (128 bytes, cache-line aligned).
///
/// Binary layout (all little-endian):
/// ```text
///  [0..8]     id (ObjectId raw u64)
///  [8..12]    generation (u32)
///  [12..13]   state (u8)
///  [13..45]   content_hash (32 bytes, BLAKE3)
///  [45..53]   blob_offset (u64)
///  [53..61]   blob_length (u64)
///  [61..69]   created_ns (i64)
///  [69..77]   modified_ns (i64)
///  [77..79]   tag_count (u16)
///  [79..81]   attr_count (u16)
///  [81..113]  inline_tags (8 × u32 = 32 bytes)
///  [113..121] overflow_offset (u64)
///  [121..122] compression (u8)
///  [122..123] encryption (u8)
///  [123..128] stored_size truncated to 5 bytes — use full u64 below
/// ```
///
/// Note: We actually use a clean 128-byte layout with slight adjustments
/// to fit u64 stored_size properly.
pub const RECORD_SIZE: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectRecord {
    pub id: u64,
    pub generation: u32,
    pub state: ObjectState,
    pub content_hash: [u8; 32],
    pub blob_offset: u64,
    pub blob_length: u64,
    pub created_ns: i64,
    pub modified_ns: i64,
    pub tag_count: u16,
    pub attr_count: u16,
    pub inline_tags: [u32; 4],
    pub overflow_offset: u64,
    pub compression: CompressionState,
    pub encryption: EncryptionState,
    pub stored_size: u64,
}

impl ObjectRecord {
    pub fn new(id: u64) -> Self {
        Self {
            id,
            generation: 0,
            state: ObjectState::Active,
            content_hash: [0; 32],
            blob_offset: 0,
            blob_length: 0,
            created_ns: 0,
            modified_ns: 0,
            tag_count: 0,
            attr_count: 0,
            inline_tags: [0; 4],
            overflow_offset: 0,
            compression: CompressionState::None,
            encryption: EncryptionState::None,
            stored_size: 0,
        }
    }

    pub fn is_active(&self) -> bool {
        self.state == ObjectState::Active
    }

    pub fn is_cleared(&self) -> bool {
        self.state == ObjectState::Cleared
    }

    /// Serialize to a 128-byte buffer.
    pub fn to_bytes(&self) -> [u8; RECORD_SIZE] {
        let mut buf = [0u8; RECORD_SIZE];
        buf[0..8].copy_from_slice(&self.id.to_le_bytes());
        buf[8..12].copy_from_slice(&self.generation.to_le_bytes());
        buf[12] = self.state as u8;
        buf[13..45].copy_from_slice(&self.content_hash);
        buf[45..53].copy_from_slice(&self.blob_offset.to_le_bytes());
        buf[53..61].copy_from_slice(&self.blob_length.to_le_bytes());
        buf[61..69].copy_from_slice(&self.created_ns.to_le_bytes());
        buf[69..77].copy_from_slice(&self.modified_ns.to_le_bytes());
        buf[77..79].copy_from_slice(&self.tag_count.to_le_bytes());
        buf[79..81].copy_from_slice(&self.attr_count.to_le_bytes());
        for (i, tag) in self.inline_tags.iter().enumerate() {
            let off = 81 + i * 4;
            buf[off..off + 4].copy_from_slice(&tag.to_le_bytes());
        }
        // 81 + 16 = 97
        buf[97..105].copy_from_slice(&self.overflow_offset.to_le_bytes());
        buf[105] = self.compression as u8;
        buf[106] = self.encryption as u8;
        buf[107..115].copy_from_slice(&self.stored_size.to_le_bytes());
        // [115..128] reserved/padding
        buf
    }

    /// Deserialize from a 128-byte buffer.
    pub fn from_bytes(buf: &[u8; RECORD_SIZE]) -> Result<Self, String> {
        let id = u64::from_le_bytes(buf[0..8].try_into().unwrap());
        let generation = u32::from_le_bytes(buf[8..12].try_into().unwrap());
        let state = ObjectState::from_u8(buf[12]).ok_or("invalid object state")?;
        let mut content_hash = [0u8; 32];
        content_hash.copy_from_slice(&buf[13..45]);
        let blob_offset = u64::from_le_bytes(buf[45..53].try_into().unwrap());
        let blob_length = u64::from_le_bytes(buf[53..61].try_into().unwrap());
        let created_ns = i64::from_le_bytes(buf[61..69].try_into().unwrap());
        let modified_ns = i64::from_le_bytes(buf[69..77].try_into().unwrap());
        let tag_count = u16::from_le_bytes(buf[77..79].try_into().unwrap());
        let attr_count = u16::from_le_bytes(buf[79..81].try_into().unwrap());
        let mut inline_tags = [0u32; 4];
        for (i, tag) in inline_tags.iter_mut().enumerate() {
            let off = 81 + i * 4;
            *tag = u32::from_le_bytes(buf[off..off + 4].try_into().unwrap());
        }
        let overflow_offset = u64::from_le_bytes(buf[97..105].try_into().unwrap());
        let compression =
            CompressionState::from_u8(buf[105]).ok_or("invalid compression state")?;
        let encryption =
            EncryptionState::from_u8(buf[106]).ok_or("invalid encryption state")?;
        let stored_size = u64::from_le_bytes(buf[107..115].try_into().unwrap());

        Ok(Self {
            id,
            generation,
            state,
            content_hash,
            blob_offset,
            blob_length,
            created_ns,
            modified_ns,
            tag_count,
            attr_count,
            inline_tags,
            overflow_offset,
            compression,
            encryption,
            stored_size,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_record_defaults() {
        let rec = ObjectRecord::new(42);
        assert_eq!(rec.id, 42);
        assert!(rec.is_active());
        assert!(!rec.is_cleared());
        assert_eq!(rec.generation, 0);
        assert_eq!(rec.blob_length, 0);
    }

    #[test]
    fn round_trip() {
        let mut rec = ObjectRecord::new(12345);
        rec.generation = 7;
        rec.content_hash = [0xAB; 32];
        rec.blob_offset = 0x1000;
        rec.blob_length = 0x2000;
        rec.created_ns = 1_700_000_000;
        rec.modified_ns = 1_700_001_000;
        rec.tag_count = 5;
        rec.attr_count = 3;
        rec.inline_tags = [100, 200, 300, 400];
        rec.overflow_offset = 0x5000;
        rec.compression = CompressionState::Zstd;
        rec.encryption = EncryptionState::Hctr2Aes128;
        rec.stored_size = 0x1800;

        let bytes = rec.to_bytes();
        assert_eq!(bytes.len(), RECORD_SIZE);

        let rec2 = ObjectRecord::from_bytes(&bytes).unwrap();
        assert_eq!(rec, rec2);
    }

    #[test]
    fn cleared_record() {
        let mut rec = ObjectRecord::new(0);
        rec.state = ObjectState::Cleared;
        assert!(rec.is_cleared());
        assert!(!rec.is_active());

        let bytes = rec.to_bytes();
        let rec2 = ObjectRecord::from_bytes(&bytes).unwrap();
        assert!(rec2.is_cleared());
    }

    #[test]
    fn all_states_round_trip() {
        for state_val in 0..=3u8 {
            let state = ObjectState::from_u8(state_val).unwrap();
            let mut rec = ObjectRecord::new(state_val as u64);
            rec.state = state;
            let bytes = rec.to_bytes();
            let rec2 = ObjectRecord::from_bytes(&bytes).unwrap();
            assert_eq!(rec2.state, state);
        }
    }

    #[test]
    fn record_size_is_128() {
        assert_eq!(RECORD_SIZE, 128);
    }
}
