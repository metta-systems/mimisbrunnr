//! Per-record lifecycle / transform state enums (DESIGN §6.2).
//!
//! These are logical mirrors of the `u8`-discriminated fields in
//! `ObjectRecord` (which itself lives in `mimisbrunnr-meta`). Discriminants
//! are pinned by the spec and **must not** be renumbered.

use serde::{Deserialize, Serialize};

/// Object lifecycle state. Drives the four-phase deletion protocol
/// (DESIGN §7.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum ObjectState {
    /// Visible to queries.
    Active = 0,
    /// Marked deleted, invisible. A sync op has been emitted to peers.
    Tombstoned = 1,
    /// Indexes cleaned up, blob extents being reclaimed.
    BlobReclaim = 2,
    /// Slot zeroed. ID is never reused.
    Cleared = 3,
}

impl ObjectState {
    /// Decode from the on-disk discriminant.
    pub const fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Active),
            1 => Some(Self::Tombstoned),
            2 => Some(Self::BlobReclaim),
            3 => Some(Self::Cleared),
            _ => None,
        }
    }
}

/// Compression algorithm currently encoding an object's blob (DESIGN §6.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum CompressionState {
    None = 0,
    Zstd = 1,
    Lz4 = 2,
}

impl CompressionState {
    pub const fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::None),
            1 => Some(Self::Zstd),
            2 => Some(Self::Lz4),
            _ => None,
        }
    }
}

/// Encryption mode currently in effect for an object's blob (DESIGN §6.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum EncryptionState {
    None = 0,
    Hctr2Aes128 = 1,
    XtsAes256 = 2,
}

impl EncryptionState {
    pub const fn from_u8(v: u8) -> Option<Self> {
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
    fn object_state_discriminants_pinned() {
        assert_eq!(ObjectState::Active as u8, 0);
        assert_eq!(ObjectState::Tombstoned as u8, 1);
        assert_eq!(ObjectState::BlobReclaim as u8, 2);
        assert_eq!(ObjectState::Cleared as u8, 3);
    }

    #[test]
    fn object_state_round_trip() {
        for v in 0..=3u8 {
            assert_eq!(ObjectState::from_u8(v).unwrap() as u8, v);
        }
        assert!(ObjectState::from_u8(4).is_none());
    }

    #[test]
    fn compression_state_discriminants_pinned() {
        assert_eq!(CompressionState::None as u8, 0);
        assert_eq!(CompressionState::Zstd as u8, 1);
        assert_eq!(CompressionState::Lz4 as u8, 2);
    }

    #[test]
    fn encryption_state_discriminants_pinned() {
        assert_eq!(EncryptionState::None as u8, 0);
        assert_eq!(EncryptionState::Hctr2Aes128 as u8, 1);
        assert_eq!(EncryptionState::XtsAes256 as u8, 2);
    }
}
