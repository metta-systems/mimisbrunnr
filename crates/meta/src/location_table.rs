//! In-memory mirror of the location table.
//!
//! ## Persistence
//!
//! On disk the table occupies one 256 KiB [`Region`] of
//! [`BtreeKind::LocationTable`] keyed by [`LocationTableKey`] (`oid`,
//! `snapshot`); values are the 48-byte [`ObjectLocation`] byte images
//! (header + 4 replica slots per METADATA-DESIGN§2.1).
//!
//! `snapshot` is always 0 for now — see the analogous note on
//! [`crate::object_table::ObjectTableKey`].
//!
//! TODO(METADATA7): the location table is removed; content locations move into
//! `ObjectRecord`'s content locator (METADATA-DESIGN§1.2).

use std::collections::{BTreeMap, btree_map};

use mimisbrunnr_storage::{
    BlockDevice, BtreeKind, StorageError,
    region::{FieldKind, Region, RegionKey, RegionParams, RunEntry},
};

use crate::{
    error::MetaError,
    location::{MAX_INLINE_REPLICAS, OBJECT_LOCATION_SIZE, ObjectLocation},
};

/// 256 KiB region size in bytes (matches the spec's 18-bit
/// `region_size_log2`). Engine code sizes the on-disk slot from this.
pub const LOCATION_TABLE_REGION_SIZE: u64 = 256 * 1024;
const REGION_SIZE_LOG2: u8 = 18;

// ---------- LocationTableKey (region key) ----------

/// Region key for the location table.
///
/// `oid` is the raw `ObjectId.to_u64()`. `snapshot` is the snapshot-aware
/// bkey position (SNAPSHOTS-DESIGN§1.2) and is always 0 in the current revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LocationTableKey {
    /// Raw `ObjectId.to_u64()`.
    pub oid: u64,
    /// Snapshot id; always 0 until SNAPSHOT2.
    pub snapshot: u32,
}

impl LocationTableKey {
    /// Construct a key for a given `oid` at `snapshot = 0`.
    pub const fn current(oid: u64) -> Self {
        Self { oid, snapshot: 0 }
    }
}

impl RegionKey for LocationTableKey {
    const FIELDS: &'static [FieldKind] = &[FieldKind::Unsigned, FieldKind::Unsigned];

    fn field_values(&self, out: &mut [u64]) {
        out[0] = self.oid;
        out[1] = self.snapshot as u64;
    }

    fn from_field_values(fields: &[u64]) -> Result<Self, StorageError> {
        let snapshot = u32::try_from(fields[1])
            .map_err(|_| StorageError::RegionCorrupt("location table snapshot field out of range"))?;
        Ok(Self {
            oid: fields[0],
            snapshot,
        })
    }
}

// ---------- LocationTable (in-memory mirror) ----------

/// Placeholder location table keyed by raw `ObjectId.to_u64()`.
#[derive(Debug, Default)]
pub struct LocationTable {
    locations: BTreeMap<u64, ObjectLocation>,
}

impl LocationTable {
    /// Empty table.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of locations currently held.
    pub fn len(&self) -> usize {
        self.locations.len()
    }

    /// `true` if empty.
    pub fn is_empty(&self) -> bool {
        self.locations.is_empty()
    }

    /// Insert (or overwrite) the location for `oid`.
    pub fn insert(&mut self, oid: u64, location: ObjectLocation) {
        self.locations.insert(oid, location);
    }

    /// Look up by raw object id.
    pub fn get(&self, oid: u64) -> Option<&ObjectLocation> {
        self.locations.get(&oid)
    }

    /// Mutable lookup.
    pub fn get_mut(&mut self, oid: u64) -> Option<&mut ObjectLocation> {
        self.locations.get_mut(&oid)
    }

    /// Remove the location for `oid`. Returns the removed value.
    pub fn remove(&mut self, oid: u64) -> Option<ObjectLocation> {
        self.locations.remove(&oid)
    }

    /// Iterator over `(oid, location)` pairs in ascending id order.
    pub fn iter(&self) -> btree_map::Iter<'_, u64, ObjectLocation> {
        self.locations.iter()
    }

    /// Write the table as the 256 KiB region at byte `offset`, keyed by
    /// `(oid, snapshot = 0)`, each value a 48-byte [`ObjectLocation`].
    pub fn flush_to_region<D: BlockDevice>(
        &self,
        device: &D,
        offset: u64,
    ) -> Result<(), MetaError> {
        let mut region = Region::create_in_place(device, offset, region_params(), &[])?;
        let entries: Vec<(LocationTableKey, RunEntry<'_>)> = self
            .locations
            .iter()
            .map(|(oid, location)| {
                (
                    LocationTableKey::current(*oid),
                    RunEntry::Value(bytemuck::bytes_of(location)),
                )
            })
            .collect();
        region.append_entries(device, 0, &entries)?;
        Ok(())
    }

    /// Read the table from the region at byte `offset`; a never-written
    /// (all-zero) region is an empty table. Entries with `snapshot != 0`
    /// are skipped.
    ///
    /// Keys carry the full object id, node id included, so `_node_id` is
    /// not needed. TODO(METADATA7): the parameter goes with the table.
    pub fn load_from_region<D: BlockDevice>(
        device: &D,
        offset: u64,
        _node_id: u16,
    ) -> Result<Self, MetaError> {
        let mut probe = [0u8; 8];
        device.read_at(offset, &mut probe)?;
        if probe.iter().all(|&byte| byte == 0) {
            return Ok(Self::default());
        }
        let region = Region::open(device, offset, BtreeKind::LocationTable)?;
        let mut locations = BTreeMap::new();
        for (key, value) in region.entries::<D, LocationTableKey>(device)? {
            if key.snapshot != 0 {
                // TODO(SNAPSHOT3): merge snapshot history into the
                // sidecar `LocationHistory` btree.
                continue;
            }
            let location = bytemuck::try_pod_read_unaligned::<ObjectLocation>(&value).map_err(|_| {
                MetaError::BufferTooSmall {
                    needed: OBJECT_LOCATION_SIZE,
                    got: value.len(),
                }
            })?;
            let replica_count = location.header.replica_count;
            if replica_count as usize > MAX_INLINE_REPLICAS {
                return Err(MetaError::InvalidReplicaCount(replica_count));
            }
            locations.insert(key.oid, location);
        }
        Ok(Self { locations })
    }
}

fn region_params() -> RegionParams {
    RegionParams {
        kind: BtreeKind::LocationTable,
        level: 0,
        region_size_log2: REGION_SIZE_LOG2,
        node_seq: 0,
        created_lsn: 0,
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::location::{ObjectLocation, ReplicaRef},
    };

    #[test]
    fn insert_get_remove() {
        let mut t = LocationTable::new();
        assert!(t.is_empty());
        let r = ReplicaRef {
            disk_id: 0,
            sector_offset: 0,
            bucket_no: 1,
        };
        let loc = ObjectLocation::new(0x100, &[r]).unwrap();
        t.insert(42, loc);
        assert_eq!(t.len(), 1);
        let got = t.get(42).unwrap();
        assert_eq!(got.header.replica_count, 1);
        assert_eq!(t.remove(42).unwrap().header.replica_count, 1);
        assert!(t.is_empty());
    }

    #[test]
    fn key_orders_by_oid_then_snapshot() {
        let a = LocationTableKey { oid: 1, snapshot: 0 };
        let b = LocationTableKey { oid: 1, snapshot: 1 };
        let c = LocationTableKey { oid: 2, snapshot: 0 };
        assert!(a < b);
        assert!(b < c);
    }

    // ----- B+ tree region round-trip -----

    use mimisbrunnr_storage::FileBlockDevice;
    use tempfile::TempDir;

    fn fresh_device() -> (TempDir, FileBlockDevice) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("location_table.bin");
        let dev = FileBlockDevice::open(&path, 1 << 20).unwrap();
        (dir, dev)
    }

    fn make_loc(extent_length: u64, replicas: &[ReplicaRef]) -> ObjectLocation {
        ObjectLocation::new(extent_length, replicas).unwrap()
    }

    #[test]
    fn empty_table_flushes_to_a_region_without_runs() {
        let (_dir, dev) = fresh_device();
        LocationTable::new().flush_to_region(&dev, 0).unwrap();
        let region = Region::open(&dev, 0, BtreeKind::LocationTable).unwrap();
        assert_eq!(region.run_count(), 0);
        let back = LocationTable::load_from_region(&dev, 0, 0).unwrap();
        assert!(back.is_empty());
    }

    #[test]
    fn location_table_region_round_trip_empty() {
        let (_dir, dev) = fresh_device();
        let t = LocationTable::load_from_region(&dev, 0, 0).unwrap();
        assert!(t.is_empty());
    }

    #[test]
    fn location_table_region_stores_location_images() {
        let (_dir, dev) = fresh_device();
        let r = ReplicaRef { disk_id: 1, sector_offset: 7, bucket_no: 9 };
        let mut t = LocationTable::new();
        t.insert(5, make_loc(0xdead, &[r]));
        t.flush_to_region(&dev, 0).unwrap();
        let region = Region::open(&dev, 0, BtreeKind::LocationTable).unwrap();
        let value = region.lookup(&dev, &LocationTableKey::current(5)).unwrap().unwrap();
        assert_eq!(value.as_bytes(), bytemuck::bytes_of(t.get(5).unwrap()));
        let back = LocationTable::load_from_region(&dev, 0, 0).unwrap();
        let loaded = back.get(5).unwrap();
        assert_eq!(loaded.header.extent_length, 0xdead);
        assert_eq!(loaded.active_replicas(), &[r]);
    }

    #[test]
    fn location_table_region_round_trip_preserves_locations() {
        let (_dir, dev) = fresh_device();
        let mut t = LocationTable::new();
        for i in 1u64..=50 {
            let r = ReplicaRef {
                disk_id: (i & 0xff) as u16,
                sector_offset: (i * 7) as u16,
                bucket_no: i as u32 * 100,
            };
            t.insert(i, make_loc(i * 0x1000, &[r]));
        }
        // One with 4 replicas (max inline).
        let r4 = [
            ReplicaRef { disk_id: 0, sector_offset: 0, bucket_no: 0 },
            ReplicaRef { disk_id: 1, sector_offset: 1, bucket_no: 1 },
            ReplicaRef { disk_id: 2, sector_offset: 2, bucket_no: 2 },
            ReplicaRef { disk_id: 3, sector_offset: 3, bucket_no: 3 },
        ];
        t.insert(99, make_loc(0xabcd, &r4));

        t.flush_to_region(&dev, 0).unwrap();
        let back = LocationTable::load_from_region(&dev, 0, 0).unwrap();
        assert_eq!(back.len(), 51);
        for i in 1u64..=50 {
            let got = back.get(i).expect("oid missing");
            assert_eq!(got.header.replica_count, 1);
            assert_eq!(got.header.extent_length, i * 0x1000);
            assert_eq!(got.active_replicas()[0].disk_id, (i & 0xff) as u16);
        }
        let big = back.get(99).unwrap();
        assert_eq!(big.header.replica_count, 4);
        assert_eq!(big.active_replicas().len(), 4);
        assert_eq!(big.active_replicas()[3].bucket_no, 3);
    }

    #[test]
    fn location_table_region_keeps_node_id_bits() {
        let (_dir, dev) = fresh_device();
        let r = ReplicaRef { disk_id: 0, sector_offset: 0, bucket_no: 1 };
        let oid = (0x0007u64 << 48) | 12;
        let mut t = LocationTable::new();
        t.insert(oid, make_loc(0x100, &[r]));
        t.flush_to_region(&dev, 0).unwrap();
        let back = LocationTable::load_from_region(&dev, 0, 7).unwrap();
        assert_eq!({ back.get(oid).unwrap().header.extent_length }, 0x100);
    }

    #[test]
    fn location_table_region_round_trip_single_entry() {
        let (_dir, dev) = fresh_device();
        let mut t = LocationTable::new();
        let r = ReplicaRef { disk_id: 1, sector_offset: 2, bucket_no: 3 };
        t.insert(7, make_loc(0x4000, &[r]));
        t.flush_to_region(&dev, 0).unwrap();
        let back = LocationTable::load_from_region(&dev, 0, 0).unwrap();
        assert_eq!(back.len(), 1);
        let got = back.get(7).unwrap();
        assert_eq!(got.header.extent_length, 0x4000);
        assert_eq!(got.active_replicas()[0].bucket_no, 3);
    }

    #[test]
    fn location_table_region_overwrite_replaces_state() {
        let (_dir, dev) = fresh_device();
        let r = ReplicaRef { disk_id: 0, sector_offset: 0, bucket_no: 1 };
        let mut first = LocationTable::new();
        first.insert(1, make_loc(0x100, &[r]));
        first.insert(2, make_loc(0x200, &[r]));
        first.flush_to_region(&dev, 0).unwrap();

        let mut second = LocationTable::new();
        second.insert(99, make_loc(0x999, &[r]));
        second.flush_to_region(&dev, 0).unwrap();

        let back = LocationTable::load_from_region(&dev, 0, 0).unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!({ back.get(99).unwrap().header.extent_length }, 0x999);
        assert!(back.get(1).is_none());
    }

    #[test]
    fn location_table_region_rejects_invalid_replica_count() {
        let (_dir, dev) = fresh_device();
        let mut location = make_loc(0x100, &[]);
        location.header.replica_count = 5;
        let mut region = Region::create_in_place(&dev, 0, region_params(), &[]).unwrap();
        region
            .append_entries(
                &dev,
                0,
                &[(LocationTableKey::current(1), RunEntry::Value(bytemuck::bytes_of(&location)))],
            )
            .unwrap();
        assert!(matches!(
            LocationTable::load_from_region(&dev, 0, 0),
            Err(MetaError::InvalidReplicaCount(5))
        ));
    }

    #[test]
    fn location_table_region_load_kind_mismatch_fails() {
        let (_dir, dev) = fresh_device();
        let mut objects = crate::ObjectTable::new();
        objects.insert(crate::ObjectRecord::new(1));
        objects.flush_to_region(&dev, 0).unwrap();

        let err = LocationTable::load_from_region(&dev, 0, 0).unwrap_err();
        assert!(matches!(
            err,
            MetaError::Storage(StorageError::RegionKindMismatch { expected, actual })
                if expected == BtreeKind::LocationTable as u16 && actual == BtreeKind::ObjectTable as u16
        ));
    }
}
