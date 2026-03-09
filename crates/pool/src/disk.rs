use mimisbrunnr_types::DiskId;

use crate::StorageTier;

/// Physical media type of a disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaType {
    NVMe,
    Ssd,
    Hdd,
    SmrHdd,
    Remote,
}

impl MediaType {
    /// Suggest a default storage tier based on media type.
    pub fn default_tier(self) -> StorageTier {
        match self {
            Self::NVMe => StorageTier::Hot,
            Self::Ssd => StorageTier::Warm,
            Self::Hdd | Self::SmrHdd => StorageTier::Cold,
            Self::Remote => StorageTier::Glacier,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::NVMe => "nvme",
            Self::Ssd => "ssd",
            Self::Hdd => "hdd",
            Self::SmrHdd => "smr_hdd",
            Self::Remote => "remote",
        }
    }
}

/// Lifecycle state of a disk in the pool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiskState {
    /// Normal operation: reads and writes.
    Online,
    /// Being drained for removal: reads served, no new writes.
    Draining,
    /// Removed from pool, data migrated away.
    Removed,
    /// Disk has failed, needs resilver.
    Faulted,
}

/// Describes a disk in the pool.
#[derive(Debug, Clone)]
pub struct DiskDescriptor {
    pub id: DiskId,
    pub capacity: u64,
    pub used: u64,
    pub media_type: MediaType,
    pub tier: StorageTier,
    pub state: DiskState,
    /// Sequential read throughput in MB/s.
    pub seq_read_mbps: u32,
    /// Random IOPS.
    pub random_iops: u32,
    /// Average latency in microseconds.
    pub latency_us: u32,
    /// Filesystem path (for file-backed devices).
    pub path: Option<String>,
}

impl DiskDescriptor {
    pub fn new(id: DiskId, capacity: u64, media_type: MediaType) -> Self {
        let tier = media_type.default_tier();
        Self {
            id,
            capacity,
            used: 0,
            media_type,
            tier,
            state: DiskState::Online,
            seq_read_mbps: 0,
            random_iops: 0,
            latency_us: 0,
            path: None,
        }
    }

    pub fn with_tier(mut self, tier: StorageTier) -> Self {
        self.tier = tier;
        self
    }

    pub fn with_path(mut self, path: impl Into<String>) -> Self {
        self.path = Some(path.into());
        self
    }

    pub fn with_perf(mut self, seq_read_mbps: u32, random_iops: u32, latency_us: u32) -> Self {
        self.seq_read_mbps = seq_read_mbps;
        self.random_iops = random_iops;
        self.latency_us = latency_us;
        self
    }

    /// Available space on this disk.
    pub fn free_space(&self) -> u64 {
        self.capacity.saturating_sub(self.used)
    }

    /// Usage ratio (0.0 to 1.0).
    pub fn usage_ratio(&self) -> f64 {
        if self.capacity == 0 {
            return 1.0;
        }
        self.used as f64 / self.capacity as f64
    }

    /// Whether the disk can accept new writes.
    pub fn is_writable(&self) -> bool {
        self.state == DiskState::Online && self.free_space() > 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_disk() {
        let d = DiskDescriptor::new(0, 1_000_000_000, MediaType::NVMe);
        assert_eq!(d.id, 0);
        assert_eq!(d.tier, StorageTier::Hot);
        assert_eq!(d.state, DiskState::Online);
        assert_eq!(d.free_space(), 1_000_000_000);
        assert!(d.is_writable());
    }

    #[test]
    fn with_builders() {
        let d = DiskDescriptor::new(1, 500_000_000, MediaType::Ssd)
            .with_tier(StorageTier::Warm)
            .with_path("/dev/sda")
            .with_perf(550, 100_000, 50);

        assert_eq!(d.tier, StorageTier::Warm);
        assert_eq!(d.path.as_deref(), Some("/dev/sda"));
        assert_eq!(d.seq_read_mbps, 550);
    }

    #[test]
    fn usage_tracking() {
        let mut d = DiskDescriptor::new(0, 1000, MediaType::Hdd);
        d.used = 400;
        assert_eq!(d.free_space(), 600);
        assert!((d.usage_ratio() - 0.4).abs() < 0.01);
    }

    #[test]
    fn full_disk_not_writable() {
        let mut d = DiskDescriptor::new(0, 1000, MediaType::Ssd);
        d.used = 1000;
        assert!(!d.is_writable());
    }

    #[test]
    fn draining_disk_not_writable() {
        let mut d = DiskDescriptor::new(0, 1000, MediaType::Ssd);
        d.state = DiskState::Draining;
        assert!(!d.is_writable());
    }

    #[test]
    fn media_default_tiers() {
        assert_eq!(MediaType::NVMe.default_tier(), StorageTier::Hot);
        assert_eq!(MediaType::Ssd.default_tier(), StorageTier::Warm);
        assert_eq!(MediaType::Hdd.default_tier(), StorageTier::Cold);
        assert_eq!(MediaType::SmrHdd.default_tier(), StorageTier::Cold);
        assert_eq!(MediaType::Remote.default_tier(), StorageTier::Glacier);
    }
}
