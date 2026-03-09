/// Lifecycle state of an object in the deletion protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ObjectState {
    /// Active and visible to queries.
    Active = 0,
    /// Marked deleted, invisible to queries, sync op emitted.
    Tombstoned = 1,
    /// Indexes cleaned up, blob extents being reclaimed.
    BlobReclaim = 2,
    /// Slot zeroed. ID never reused.
    Cleared = 3,
}

impl ObjectState {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Active),
            1 => Some(Self::Tombstoned),
            2 => Some(Self::BlobReclaim),
            3 => Some(Self::Cleared),
            _ => None,
        }
    }
}

/// Compression state stored in each object record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum CompressionState {
    None = 0,
    Zstd = 1,
    Lz4 = 2,
}

impl CompressionState {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::None),
            1 => Some(Self::Zstd),
            2 => Some(Self::Lz4),
            _ => None,
        }
    }
}

/// Encryption state stored in each object record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum EncryptionState {
    None = 0,
    Hctr2Aes128 = 1,
    XtsAes256 = 2,
}

impl EncryptionState {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::None),
            1 => Some(Self::Hctr2Aes128),
            2 => Some(Self::XtsAes256),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_state_round_trip() {
        for v in 0..=3u8 {
            let state = ObjectState::from_u8(v).unwrap();
            assert_eq!(state as u8, v);
        }
        assert!(ObjectState::from_u8(4).is_none());
    }

    #[test]
    fn compression_state_round_trip() {
        for v in 0..=2u8 {
            let state = CompressionState::from_u8(v).unwrap();
            assert_eq!(state as u8, v);
        }
        assert!(CompressionState::from_u8(3).is_none());
    }

    #[test]
    fn encryption_state_round_trip() {
        for v in 0..=2u8 {
            let state = EncryptionState::from_u8(v).unwrap();
            assert_eq!(state as u8, v);
        }
        assert!(EncryptionState::from_u8(3).is_none());
    }
}
