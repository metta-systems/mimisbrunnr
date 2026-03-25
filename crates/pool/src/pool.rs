use std::collections::HashMap;

use mimisbrunnr_types::DiskId;

use crate::{
    disk::{DiskDescriptor, DiskState},
    error::PoolError,
    placement::PlacementRule,
    tier::StorageTier,
};

/// Manages a pool of disks with semantic placement and tiering.
pub struct PoolManager {
    /// All disks in the pool.
    disks: HashMap<DiskId, DiskDescriptor>,
    /// Active placement rules.
    rules: Vec<PlacementRule>,
    /// Default tier for objects without matching placement rules.
    default_tier: StorageTier,
    /// Next disk ID to assign.
    next_disk_id: DiskId,
}

impl PoolManager {
    pub fn new() -> Self {
        Self {
            disks: HashMap::new(),
            rules: Vec::new(),
            default_tier: StorageTier::Warm,
            next_disk_id: 0,
        }
    }

    /// Add a disk to the pool. Returns the assigned DiskId.
    pub fn add_disk(&mut self, mut desc: DiskDescriptor) -> Result<DiskId, PoolError> {
        let id = desc.id;
        if self.disks.contains_key(&id) {
            return Err(PoolError::DiskAlreadyExists(id));
        }
        desc.state = DiskState::Online;
        self.disks.insert(id, desc);
        if id >= self.next_disk_id {
            self.next_disk_id = id + 1;
        }
        Ok(id)
    }

    /// Begin draining a disk for removal. No new writes, reads still served.
    pub fn begin_drain(&mut self, disk_id: DiskId) -> Result<(), PoolError> {
        if self.online_count() <= 1 {
            return Err(PoolError::LastDisk);
        }
        let disk = self
            .disks
            .get_mut(&disk_id)
            .ok_or(PoolError::DiskNotFound(disk_id))?;
        disk.state = DiskState::Draining;
        Ok(())
    }

    /// Complete removal of a drained disk.
    pub fn remove_disk(&mut self, disk_id: DiskId) -> Result<DiskDescriptor, PoolError> {
        let disk = self
            .disks
            .get(&disk_id)
            .ok_or(PoolError::DiskNotFound(disk_id))?;
        if disk.state != DiskState::Draining && disk.state != DiskState::Removed {
            return Err(PoolError::DiskNotFound(disk_id)); // Must drain first
        }
        Ok(self.disks.remove(&disk_id).unwrap())
    }

    /// Mark a disk as faulted.
    pub fn mark_faulted(&mut self, disk_id: DiskId) -> Result<(), PoolError> {
        let disk = self
            .disks
            .get_mut(&disk_id)
            .ok_or(PoolError::DiskNotFound(disk_id))?;
        disk.state = DiskState::Faulted;
        Ok(())
    }

    /// Get a disk descriptor.
    pub fn get_disk(&self, disk_id: DiskId) -> Option<&DiskDescriptor> {
        self.disks.get(&disk_id)
    }

    /// Get a mutable disk descriptor.
    pub fn get_disk_mut(&mut self, disk_id: DiskId) -> Option<&mut DiskDescriptor> {
        self.disks.get_mut(&disk_id)
    }

    /// Select the best disk for writing data to a given tier.
    ///
    /// Strategy: among writable disks in the target tier, pick the one with
    /// the most free space. Falls back to any writable disk if no tier match.
    pub fn select_disk_for_tier(&self, tier: StorageTier) -> Result<DiskId, PoolError> {
        // First try exact tier match
        let candidates: Vec<_> = self
            .disks
            .values()
            .filter(|d| d.is_writable() && d.tier == tier)
            .collect();

        if let Some(best) = candidates.iter().max_by_key(|d| d.free_space()) {
            return Ok(best.id);
        }

        // Fall back to nearest tier
        let nearest = self.nearest_writable_tier(tier)?;
        Ok(nearest)
    }

    /// Select a disk for writing with `needed` bytes, respecting placement rules.
    pub fn select_disk(
        &self,
        needed: u64,
        preferred_tier: Option<StorageTier>,
    ) -> Result<DiskId, PoolError> {
        let tier = preferred_tier.unwrap_or(self.default_tier);

        // Among writable disks with enough space and matching tier
        let candidates: Vec<_> = self
            .disks
            .values()
            .filter(|d| d.is_writable() && d.free_space() >= needed && d.tier == tier)
            .collect();

        if let Some(best) = candidates.iter().max_by_key(|d| d.free_space()) {
            return Ok(best.id);
        }

        // Fall back to any writable disk with enough space
        let any: Vec<_> = self
            .disks
            .values()
            .filter(|d| d.is_writable() && d.free_space() >= needed)
            .collect();

        if let Some(best) = any.iter().max_by_key(|d| d.free_space()) {
            return Ok(best.id);
        }

        Err(PoolError::NoSuitableDisk(tier))
    }

    /// Record that bytes were written to a disk.
    pub fn record_write(&mut self, disk_id: DiskId, bytes: u64) -> Result<(), PoolError> {
        let disk = self
            .disks
            .get_mut(&disk_id)
            .ok_or(PoolError::DiskNotFound(disk_id))?;
        if !disk.is_writable() {
            return Err(PoolError::DiskDraining(disk_id));
        }
        disk.used += bytes;
        Ok(())
    }

    /// Record that bytes were freed on a disk.
    pub fn record_free(&mut self, disk_id: DiskId, bytes: u64) -> Result<(), PoolError> {
        let disk = self
            .disks
            .get_mut(&disk_id)
            .ok_or(PoolError::DiskNotFound(disk_id))?;
        disk.used = disk.used.saturating_sub(bytes);
        Ok(())
    }

    /// Add a placement rule.
    pub fn add_rule(&mut self, rule: PlacementRule) {
        self.rules.push(rule);
    }

    /// Get all placement rules.
    pub fn rules(&self) -> &[PlacementRule] {
        &self.rules
    }

    /// Set the default tier for objects without matching rules.
    pub fn set_default_tier(&mut self, tier: StorageTier) {
        self.default_tier = tier;
    }

    /// All disks in the pool.
    pub fn all_disks(&self) -> impl Iterator<Item = &DiskDescriptor> {
        self.disks.values()
    }

    /// Number of online (writable) disks.
    pub fn online_count(&self) -> usize {
        self.disks
            .values()
            .filter(|d| d.state == DiskState::Online)
            .count()
    }

    /// Total capacity across all online disks.
    pub fn total_capacity(&self) -> u64 {
        self.disks
            .values()
            .filter(|d| d.state == DiskState::Online)
            .map(|d| d.capacity)
            .sum()
    }

    /// Total used across all online disks.
    pub fn total_used(&self) -> u64 {
        self.disks
            .values()
            .filter(|d| d.state == DiskState::Online)
            .map(|d| d.used)
            .sum()
    }

    /// Disks in a specific tier.
    pub fn disks_in_tier(&self, tier: StorageTier) -> Vec<&DiskDescriptor> {
        self.disks
            .values()
            .filter(|d| d.tier == tier && d.state == DiskState::Online)
            .collect()
    }

    /// Plan objects that need migration from a draining disk.
    /// Returns a list of (object_extent_offset, suggested_destination_disk).
    pub fn plan_drain(&self, draining_disk: DiskId) -> Result<Vec<DiskId>, PoolError> {
        let disk = self
            .disks
            .get(&draining_disk)
            .ok_or(PoolError::DiskNotFound(draining_disk))?;
        if disk.state != DiskState::Draining {
            return Err(PoolError::DiskNotFound(draining_disk));
        }

        // Find other online disks to migrate to, preferring same tier
        let targets: Vec<_> = self
            .disks
            .values()
            .filter(|d| d.id != draining_disk && d.is_writable())
            .collect();

        if targets.is_empty() {
            return Err(PoolError::EmptyPool);
        }

        // Return available target disk IDs
        Ok(targets.iter().map(|d| d.id).collect())
    }

    fn nearest_writable_tier(&self, preferred: StorageTier) -> Result<DiskId, PoolError> {
        // Try tiers in order of distance from preferred
        let all_tiers = [
            StorageTier::Hot,
            StorageTier::Warm,
            StorageTier::Cold,
            StorageTier::Glacier,
        ];
        let pref_idx = all_tiers.iter().position(|&t| t == preferred).unwrap_or(1);

        // Expand outward from preferred
        for distance in 0..all_tiers.len() {
            for dir in [0isize, -1, 1] {
                let idx = pref_idx as isize + dir * distance as isize;
                if idx >= 0 && (idx as usize) < all_tiers.len() {
                    let tier = all_tiers[idx as usize];
                    if let Some(best) = self
                        .disks
                        .values()
                        .filter(|d| d.is_writable() && d.tier == tier)
                        .max_by_key(|d| d.free_space())
                    {
                        return Ok(best.id);
                    }
                }
            }
        }

        Err(PoolError::EmptyPool)
    }
}

impl Default for PoolManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use {super::*, crate::disk::MediaType, mimisbrunnr_types::Query};

    fn nvme(id: DiskId, cap: u64) -> DiskDescriptor {
        DiskDescriptor::new(id, cap, MediaType::NVMe)
    }

    fn ssd(id: DiskId, cap: u64) -> DiskDescriptor {
        DiskDescriptor::new(id, cap, MediaType::Ssd)
    }

    fn hdd(id: DiskId, cap: u64) -> DiskDescriptor {
        DiskDescriptor::new(id, cap, MediaType::Hdd)
    }

    #[test]
    fn add_and_get_disk() {
        let mut pool = PoolManager::new();
        pool.add_disk(nvme(0, 1_000_000)).unwrap();

        let disk = pool.get_disk(0).unwrap();
        assert_eq!(disk.id, 0);
        assert_eq!(disk.tier, StorageTier::Hot);
        assert_eq!(pool.online_count(), 1);
    }

    #[test]
    fn duplicate_disk_rejected() {
        let mut pool = PoolManager::new();
        pool.add_disk(nvme(0, 1000)).unwrap();
        assert!(pool.add_disk(nvme(0, 2000)).is_err());
    }

    #[test]
    fn select_disk_by_tier() {
        let mut pool = PoolManager::new();
        pool.add_disk(nvme(0, 1_000_000)).unwrap();
        pool.add_disk(ssd(1, 5_000_000)).unwrap();
        pool.add_disk(hdd(2, 10_000_000)).unwrap();

        assert_eq!(pool.select_disk_for_tier(StorageTier::Hot).unwrap(), 0);
        assert_eq!(pool.select_disk_for_tier(StorageTier::Warm).unwrap(), 1);
        assert_eq!(pool.select_disk_for_tier(StorageTier::Cold).unwrap(), 2);
    }

    #[test]
    fn select_disk_fallback() {
        let mut pool = PoolManager::new();
        pool.add_disk(ssd(0, 1_000_000)).unwrap(); // Warm only

        // Requesting Hot should fall back to Warm
        let disk = pool.select_disk_for_tier(StorageTier::Hot).unwrap();
        assert_eq!(disk, 0);
    }

    #[test]
    fn select_disk_with_capacity() {
        let mut pool = PoolManager::new();
        pool.add_disk(ssd(0, 1_000_000)).unwrap();
        pool.add_disk(ssd(1, 5_000_000)).unwrap();

        // Should pick disk with more space
        let disk = pool.select_disk(100, Some(StorageTier::Warm)).unwrap();
        assert_eq!(disk, 1); // 5M > 1M
    }

    #[test]
    fn select_disk_insufficient_space() {
        let mut pool = PoolManager::new();
        let mut d = ssd(0, 1000);
        d.used = 1000;
        pool.add_disk(d).unwrap();

        assert!(pool.select_disk(100, None).is_err());
    }

    #[test]
    fn record_write_and_free() {
        let mut pool = PoolManager::new();
        pool.add_disk(ssd(0, 10_000)).unwrap();

        pool.record_write(0, 3000).unwrap();
        assert_eq!(pool.get_disk(0).unwrap().used, 3000);

        pool.record_free(0, 1000).unwrap();
        assert_eq!(pool.get_disk(0).unwrap().used, 2000);
    }

    #[test]
    fn drain_and_remove() {
        let mut pool = PoolManager::new();
        pool.add_disk(ssd(0, 1000)).unwrap();
        pool.add_disk(ssd(1, 1000)).unwrap();

        pool.begin_drain(0).unwrap();
        assert_eq!(pool.get_disk(0).unwrap().state, DiskState::Draining);
        assert!(!pool.get_disk(0).unwrap().is_writable());

        let removed = pool.remove_disk(0).unwrap();
        assert_eq!(removed.id, 0);
        assert!(pool.get_disk(0).is_none());
    }

    #[test]
    fn cannot_drain_last_disk() {
        let mut pool = PoolManager::new();
        pool.add_disk(ssd(0, 1000)).unwrap();

        assert!(matches!(pool.begin_drain(0), Err(PoolError::LastDisk)));
    }

    #[test]
    fn cannot_remove_online_disk() {
        let mut pool = PoolManager::new();
        pool.add_disk(ssd(0, 1000)).unwrap();

        // Must drain before removing
        assert!(pool.remove_disk(0).is_err());
    }

    #[test]
    fn plan_drain_targets() {
        let mut pool = PoolManager::new();
        pool.add_disk(ssd(0, 1000)).unwrap();
        pool.add_disk(ssd(1, 2000)).unwrap();
        pool.add_disk(hdd(2, 5000)).unwrap();

        pool.begin_drain(0).unwrap();
        let targets = pool.plan_drain(0).unwrap();
        // Should suggest disks 1 and 2
        assert!(targets.contains(&1));
        assert!(targets.contains(&2));
        assert!(!targets.contains(&0));
    }

    #[test]
    fn faulted_disk() {
        let mut pool = PoolManager::new();
        pool.add_disk(ssd(0, 1000)).unwrap();

        pool.mark_faulted(0).unwrap();
        assert_eq!(pool.get_disk(0).unwrap().state, DiskState::Faulted);
        assert!(!pool.get_disk(0).unwrap().is_writable());
    }

    #[test]
    fn total_capacity_and_used() {
        let mut pool = PoolManager::new();
        pool.add_disk(ssd(0, 1000)).unwrap();
        pool.add_disk(ssd(1, 2000)).unwrap();

        pool.record_write(0, 500).unwrap();
        pool.record_write(1, 300).unwrap();

        assert_eq!(pool.total_capacity(), 3000);
        assert_eq!(pool.total_used(), 800);
    }

    #[test]
    fn disks_in_tier() {
        let mut pool = PoolManager::new();
        pool.add_disk(nvme(0, 1000)).unwrap();
        pool.add_disk(ssd(1, 2000)).unwrap();
        pool.add_disk(ssd(2, 3000)).unwrap();
        pool.add_disk(hdd(3, 5000)).unwrap();

        assert_eq!(pool.disks_in_tier(StorageTier::Hot).len(), 1);
        assert_eq!(pool.disks_in_tier(StorageTier::Warm).len(), 2);
        assert_eq!(pool.disks_in_tier(StorageTier::Cold).len(), 1);
        assert_eq!(pool.disks_in_tier(StorageTier::Glacier).len(), 0);
    }

    #[test]
    fn placement_rules() {
        let mut pool = PoolManager::new();
        pool.add_rule(PlacementRule::Pin {
            query: Query::HasTag(mimisbrunnr_types::TagId::new(1)),
            tier: StorageTier::Hot,
        });
        pool.add_rule(PlacementRule::AutoTier {
            hot_threshold_days: 7,
            warm_threshold_days: 30,
            cold_after: 90,
        });

        assert_eq!(pool.rules().len(), 2);
    }

    #[test]
    fn write_to_draining_fails() {
        let mut pool = PoolManager::new();
        pool.add_disk(ssd(0, 1000)).unwrap();
        pool.add_disk(ssd(1, 1000)).unwrap();

        pool.begin_drain(0).unwrap();
        assert!(matches!(
            pool.record_write(0, 100),
            Err(PoolError::DiskDraining(_))
        ));
    }

    #[test]
    fn empty_pool_select_fails() {
        let pool = PoolManager::new();
        assert!(pool.select_disk(100, None).is_err());
    }

    #[test]
    fn multi_tier_pool() {
        let mut pool = PoolManager::new();
        pool.add_disk(nvme(0, 100_000).with_perf(3000, 500_000, 10))
            .unwrap();
        pool.add_disk(ssd(1, 500_000).with_perf(550, 100_000, 50))
            .unwrap();
        pool.add_disk(hdd(2, 2_000_000).with_perf(150, 200, 5000))
            .unwrap();

        // Hot data goes to NVMe
        assert_eq!(pool.select_disk_for_tier(StorageTier::Hot).unwrap(), 0);
        // Warm to SSD
        assert_eq!(pool.select_disk_for_tier(StorageTier::Warm).unwrap(), 1);
        // Cold to HDD
        assert_eq!(pool.select_disk_for_tier(StorageTier::Cold).unwrap(), 2);
    }
}
