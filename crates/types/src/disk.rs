//! Disk topology types (DESIGN §8.1) — the **logical** view.
//!
//! The on-disk `DiskDescriptorOnDisk` (per IMPL §10.4) lives in the `pool`
//! crate; this struct is what the rest of the system carries in memory and
//! shows in CLI / API output.

use serde::{Deserialize, Serialize};

use crate::ids::DiskId;

/// Physical media class. Drives placement defaults (DESIGN §8.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MediaType {
    NVMe,
    Ssd,
    Hdd,
    SmrHdd,
    /// Network-attached / object-store backend (S3, IPFS, …).
    Remote,
}

/// Storage tier. Discriminants are pinned by DESIGN §8.1 and used as array
/// indices into placement-priority tables — do not renumber.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[repr(u8)]
pub enum StorageTier {
    Hot = 0,
    Warm = 1,
    Cold = 2,
    Glacier = 3,
}

impl StorageTier {
    pub const fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Hot),
            1 => Some(Self::Warm),
            2 => Some(Self::Cold),
            3 => Some(Self::Glacier),
            _ => None,
        }
    }
}

/// Operational state of a disk (DESIGN §8.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DiskState {
    /// Reads and writes accepted normally.
    Online,
    /// Reads only; data is being migrated off.
    Draining,
    /// Drained; safe to detach physically.
    Removed,
    /// Failed; awaits resilver.
    Faulted,
}

/// Logical descriptor of a single disk in a pool. Mirrors the user-visible
/// shape from DESIGN §8.1; the on-disk encoded form is owned by the `pool`
/// crate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiskDescriptor {
    pub id: DiskId,
    pub capacity: u64,
    pub used: u64,
    pub media_type: MediaType,
    pub tier: StorageTier,
    pub state: DiskState,
    pub seq_read_mbps: u32,
    pub random_iops: u32,
    pub latency_us: u32,
    /// Path to the file backing this disk for file-based devices. `None`
    /// for raw block devices and remote backends.
    pub path: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_tier_discriminants() {
        assert_eq!(StorageTier::Hot as u8, 0);
        assert_eq!(StorageTier::Warm as u8, 1);
        assert_eq!(StorageTier::Cold as u8, 2);
        assert_eq!(StorageTier::Glacier as u8, 3);
    }

    #[test]
    fn storage_tier_orders_naturally() {
        assert!(StorageTier::Hot < StorageTier::Warm);
        assert!(StorageTier::Warm < StorageTier::Cold);
        assert!(StorageTier::Cold < StorageTier::Glacier);
    }

    #[test]
    fn descriptor_round_trips_via_cbor() {
        let d = DiskDescriptor {
            id: 3,
            capacity: 1 << 40,
            used: 1 << 35,
            media_type: MediaType::Ssd,
            tier: StorageTier::Warm,
            state: DiskState::Online,
            seq_read_mbps: 500,
            random_iops: 80_000,
            latency_us: 100,
            path: Some("/dev/loop3".into()),
        };
        let mut buf = Vec::new();
        ciborium::ser::into_writer(&d, &mut buf).unwrap();
        let back: DiskDescriptor = ciborium::de::from_reader(buf.as_slice()).unwrap();
        assert_eq!(d, back);
    }
}
