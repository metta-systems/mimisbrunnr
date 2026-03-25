use bytemuck::{Pod, Zeroable};
use mimisbrunnr_types::{CompressionState, EncryptionState, ObjectState};

/// Fixed-size on-disk object record (128 bytes, cache-line aligned).
///
/// Layout is `#[repr(C)]` with naturally aligned fields — no hidden padding,
/// directly castable via `bytemuck` for zero-copy mmap access.
///
/// Binary layout (all little-endian, naturally aligned):
/// ```text
///  [0..8]     id (ObjectId raw u64)
///  [8..16]    blob_offset (u64)
///  [16..24]   blob_length (u64)
///  [24..32]   stored_size (u64)
///  [32..40]   overflow_offset (u64)
///  [40..48]   created_ns (i64)
///  [48..56]   modified_ns (i64)
///  [56..60]   generation (u32)
///  [60..76]   inline_tags (4 × u32)
///  [76..78]   tag_count (u16)
///  [78..80]   attr_count (u16)
///  [80..81]   state (u8 → ObjectState)
///  [81..82]   compression (u8 → CompressionState)
///  [82..83]   encryption (u8 → EncryptionState)
///  [83..84]   _pad (u8)
///  [84..116]  content_hash (32 bytes, BLAKE3)
///  [116..128] _reserved (12 bytes)
/// ```
pub const RECORD_SIZE: usize = 128;

#[derive(Debug, Clone, Copy, Pod, Zeroable)]
#[repr(C)]
pub struct ObjectRecord {
    pub id: u64,
    pub blob_offset: u64,
    pub blob_length: u64,
    pub stored_size: u64,
    pub overflow_offset: u64,
    pub created_ns: i64,
    pub modified_ns: i64,
    pub generation: u32,
    pub inline_tags: [u32; 4],
    pub tag_count: u16,
    pub attr_count: u16,
    /// Raw ObjectState discriminant. Use [`state()`] / [`set_state()`] for typed access.
    pub state: u8,
    /// Raw CompressionState discriminant. Use [`compression()`] / [`set_compression()`].
    pub compression: u8,
    /// Raw EncryptionState discriminant. Use [`set_encryption()`].
    pub encryption: u8,
    pub _pad: u8,
    pub content_hash: [u8; 32],
    pub _reserved: [u8; 12],
}

const _: () = assert!(size_of::<ObjectRecord>() == RECORD_SIZE);

impl PartialEq for ObjectRecord {
    fn eq(&self, other: &Self) -> bool {
        // Compare all meaningful fields, ignoring padding/reserved
        self.id == other.id
            && self.blob_offset == other.blob_offset
            && self.blob_length == other.blob_length
            && self.stored_size == other.stored_size
            && self.overflow_offset == other.overflow_offset
            && self.created_ns == other.created_ns
            && self.modified_ns == other.modified_ns
            && self.generation == other.generation
            && self.inline_tags == other.inline_tags
            && self.tag_count == other.tag_count
            && self.attr_count == other.attr_count
            && self.state == other.state
            && self.compression == other.compression
            && self.encryption == other.encryption
            && self.content_hash == other.content_hash
    }
}

impl Eq for ObjectRecord {}

impl ObjectRecord {
    pub fn new(id: u64) -> Self {
        let mut rec = Self::zeroed();
        rec.id = id;
        // state 0 = Active, compression 0 = None, encryption 0 = None
        rec
    }

    /// Typed access to the object state.
    pub fn state(&self) -> ObjectState {
        ObjectState::from_u8(self.state).unwrap_or(ObjectState::Active)
    }

    pub fn set_state(&mut self, s: ObjectState) {
        self.state = s as u8;
    }

    pub fn is_active(&self) -> bool {
        self.state == ObjectState::Active as u8
    }

    pub fn is_cleared(&self) -> bool {
        self.state == ObjectState::Cleared as u8
    }

    /// Typed access to the compression state.
    pub fn compression(&self) -> CompressionState {
        CompressionState::from_u8(self.compression).unwrap_or(CompressionState::None)
    }

    pub fn set_compression(&mut self, c: CompressionState) {
        self.compression = c as u8;
    }

    /// Typed access to the encryption state.
    pub fn encryption(&self) -> EncryptionState {
        EncryptionState::from_u8(self.encryption).unwrap_or(EncryptionState::None)
    }

    pub fn set_encryption(&mut self, e: EncryptionState) {
        self.encryption = e as u8;
    }

    /// Serialize to a 128-byte buffer (zero-copy).
    pub fn to_bytes(&self) -> [u8; RECORD_SIZE] {
        let bytes = bytemuck::bytes_of(self);
        bytes.try_into().unwrap()
    }

    /// Deserialize from a 128-byte buffer (zero-copy cast + validation).
    pub fn from_bytes(buf: &[u8; RECORD_SIZE]) -> Result<Self, String> {
        let rec: &Self = bytemuck::from_bytes(buf);
        // Validate enum discriminants
        ObjectState::from_u8(rec.state).ok_or("invalid object state")?;
        CompressionState::from_u8(rec.compression).ok_or("invalid compression state")?;
        EncryptionState::from_u8(rec.encryption).ok_or("invalid encryption state")?;
        Ok(*rec)
    }

    /// Borrow a record directly from a byte slice (true zero-copy).
    /// Caller must ensure the buffer lives long enough.
    pub fn ref_from_bytes(buf: &[u8; RECORD_SIZE]) -> &Self {
        bytemuck::from_bytes(buf)
    }

    /// Cast a slice of bytes into a slice of records (zero-copy, for mmap).
    pub fn slice_from_bytes(buf: &[u8]) -> &[Self] {
        bytemuck::cast_slice(buf)
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
        rec.set_compression(CompressionState::Zstd);
        rec.set_encryption(EncryptionState::Hctr2Aes128);
        rec.stored_size = 0x1800;

        let bytes = rec.to_bytes();
        assert_eq!(bytes.len(), RECORD_SIZE);

        let rec2 = ObjectRecord::from_bytes(&bytes).unwrap();
        assert_eq!(rec, rec2);
    }

    #[test]
    fn cleared_record() {
        let mut rec = ObjectRecord::new(0);
        rec.set_state(ObjectState::Cleared);
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
            rec.set_state(state);
            let bytes = rec.to_bytes();
            let rec2 = ObjectRecord::from_bytes(&bytes).unwrap();
            assert_eq!(rec2.state(), state);
        }
    }

    #[test]
    fn record_size_is_128() {
        assert_eq!(RECORD_SIZE, 128);
        assert_eq!(size_of::<ObjectRecord>(), 128);
    }

    #[test]
    fn zero_copy_ref() {
        let mut rec = ObjectRecord::new(99);
        rec.blob_length = 512;
        let bytes = rec.to_bytes();
        let ref_rec = ObjectRecord::ref_from_bytes(&bytes);
        assert_eq!(ref_rec.id, 99);
        assert_eq!(ref_rec.blob_length, 512);
    }

    #[test]
    fn slice_cast() {
        let rec1 = ObjectRecord::new(1);
        let rec2 = ObjectRecord::new(2);
        let mut buf = vec![0u8; RECORD_SIZE * 2];
        buf[..RECORD_SIZE].copy_from_slice(&rec1.to_bytes());
        buf[RECORD_SIZE..].copy_from_slice(&rec2.to_bytes());

        let records = ObjectRecord::slice_from_bytes(&buf);
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].id, 1);
        assert_eq!(records[1].id, 2);
    }

    #[test]
    fn natural_alignment() {
        // Verify the struct has no hidden padding by checking field offsets
        assert_eq!(align_of::<ObjectRecord>(), 8);
        assert_eq!(size_of::<ObjectRecord>(), 128);
    }
}
