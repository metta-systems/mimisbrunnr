//! In-memory mirror of the location table.
//!
//! ## Persistence (R1b-3)
//!
//! On disk the table occupies one 256 KiB §1.5 B+ tree region of
//! [`BtreeKind::LocationTable`]. The mirror is materialised into a single
//! CBOR-encoded sorted run via [`BtreeRegion::write_full`] keyed by an
//! [`LocationTableKey`] (`oid`, `snapshot`) pair ascending; values are
//! variable-length serialised [`ObjectLocation`] byte images carried in
//! [`LocationTableValue`]'s `Vec<u8>` newtype (header + N replicas per
//! IMPL §6.1).
//!
//! `snapshot` is always 0 for now — see the analogous note on
//! [`crate::object_table::ObjectTableKey`].
//!
//! TODO(rewrite-phase-R1d): swap to true positional-radix encoding per
//! IMPL §6.1 (leaf fanout 5440 for 48-byte records). Same migration shape
//! as the object table.

use std::collections::{BTreeMap, btree_map};

use {
    mimisbrunnr_storage::{BlockDevice, BtreeKind, BtreeRegion, LoadedNode, SortedRun},
    serde::{Deserialize, Serialize},
};

use crate::{error::MetaError, location::ObjectLocation};

/// 256 KiB region size in bytes (matches the spec's 18-bit
/// `region_size_log2`). Engine code sizes the on-disk slot from this.
pub const LOCATION_TABLE_REGION_SIZE: u64 = 256 * 1024;
const REGION_SIZE_LOG2: u8 = 18;

// ---------- LocationTableKey / LocationTableValue (B+ tree wire types) ----------

/// B+ tree key for the location table.
///
/// `oid` is the raw `ObjectId.to_u64()`. `snapshot` is the snapshot-aware
/// bkey position (IMPL §11.2) and is always 0 in the current revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct LocationTableKey {
    /// Raw `ObjectId.to_u64()`.
    pub oid: u64,
    /// Snapshot id; always 0 until R6.
    pub snapshot: u32,
}

impl LocationTableKey {
    /// Construct a key for a given `oid` at `snapshot = 0`.
    pub const fn current(oid: u64) -> Self {
        Self { oid, snapshot: 0 }
    }
}

/// B+ tree value for the location table: the variable-length
/// `LocationHeader || N × ReplicaRef` byte image (IMPL §6.1) carried as a
/// length-tagged byte vector. We can't use the fixed-shape POD form here
/// because the payload length depends on `replica_count`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocationTableValue(pub Vec<u8>);

impl LocationTableValue {
    /// Serialise an `ObjectLocation` to its variable-length byte image.
    pub fn from_location(location: &ObjectLocation) -> Self {
        Self(location.serialize())
    }

    /// Decode the carried bytes into an `ObjectLocation`. Wraps
    /// [`ObjectLocation::parse`].
    pub fn to_location(&self) -> Result<ObjectLocation, MetaError> {
        let (loc, _consumed) = ObjectLocation::parse(&self.0)?;
        Ok(loc)
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

    // ----------------------------------------------------------------
    // R1b-3: §1.5 B+ tree persistence (CBOR-encoded sorted run).
    // ----------------------------------------------------------------

    /// Build a [`LoadedNode`] containing every entry as a single CBOR
    /// sorted run, sorted by `(oid, snapshot=0)`. The node uses
    /// [`BtreeKind::LocationTable`] and the spec's 18-bit (256 KiB) region
    /// size.
    pub fn to_loaded_node(&self) -> LoadedNode<LocationTableKey, LocationTableValue> {
        let entries: Vec<(LocationTableKey, LocationTableValue)> = self
            .locations
            .iter()
            .map(|(oid, loc)| {
                (
                    LocationTableKey::current(*oid),
                    LocationTableValue::from_location(loc),
                )
            })
            .collect();

        let mut node: LoadedNode<LocationTableKey, LocationTableValue> =
            LoadedNode::new(BtreeKind::LocationTable, 0, REGION_SIZE_LOG2);
        if !entries.is_empty() {
            let run = SortedRun::from_sorted(0, 0, entries);
            node.sorted_runs.push(run);
            node.header.sorted_run_count = 1;
        }
        node
    }

    /// Restore the in-memory state from a [`LoadedNode`] read via
    /// [`BtreeRegion::read`]. Entries with `snapshot != 0` are skipped.
    pub fn from_loaded_node(
        node: &LoadedNode<LocationTableKey, LocationTableValue>,
    ) -> Result<Self, MetaError> {
        let mut locations: BTreeMap<u64, ObjectLocation> = BTreeMap::new();
        for (k, v) in node.merge_iter() {
            if k.snapshot != 0 {
                // TODO(rewrite-phase-R6): merge snapshot history into the
                // sidecar `LocationHistory` btree.
                continue;
            }
            locations.insert(k.oid, v.to_location()?);
        }
        Ok(Self { locations })
    }

    /// Write the in-memory state as a fresh 256 KiB region at byte `offset`
    /// on `device`.
    pub fn flush_to_region<D: BlockDevice>(
        &self,
        device: &D,
        offset: u64,
    ) -> Result<(), MetaError> {
        let mut node = self.to_loaded_node();
        BtreeRegion::write_full::<D, LocationTableKey, LocationTableValue>(
            device, offset, &mut node,
        )
        .map_err(MetaError::from)?;
        Ok(())
    }

    /// Read the in-memory state from the 256 KiB region at byte `offset`
    /// on `device`. An all-zero region returns [`Self::default`].
    pub fn load_from_region<D: BlockDevice>(
        device: &D,
        offset: u64,
    ) -> Result<Self, MetaError> {
        let mut probe = [0u8; 8];
        device.read_at(offset, &mut probe).map_err(MetaError::from)?;
        if probe.iter().all(|&b| b == 0) {
            return Ok(Self::default());
        }
        let node = BtreeRegion::read::<D, LocationTableKey, LocationTableValue>(
            device,
            offset,
            BtreeKind::LocationTable,
        )
        .map_err(MetaError::from)?;
        Self::from_loaded_node(&node)
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

    #[test]
    fn value_round_trip_via_helpers() {
        let r = ReplicaRef { disk_id: 1, sector_offset: 7, bucket_no: 9 };
        let loc = ObjectLocation::new(0xdead, &[r]).unwrap();
        let v = LocationTableValue::from_location(&loc);
        let back = v.to_location().unwrap();
        assert_eq!({ back.header.extent_length }, 0xdead);
        assert_eq!(back.active_replicas(), &[r]);
    }

    // ----- B+ tree region round-trip (R1b-3) -----

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
    fn empty_loaded_node_round_trip() {
        let t = LocationTable::new();
        let node = t.to_loaded_node();
        assert_eq!(node.sorted_runs.len(), 0);
        let back = LocationTable::from_loaded_node(&node).unwrap();
        assert!(back.is_empty());
    }

    #[test]
    fn location_table_region_round_trip_empty() {
        let (_dir, dev) = fresh_device();
        let t = LocationTable::load_from_region(&dev, 0).unwrap();
        assert!(t.is_empty());
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
        let back = LocationTable::load_from_region(&dev, 0).unwrap();
        assert_eq!(back.len(), 51);
        for i in 1u64..=50 {
            let got = back.get(i).expect("oid missing");
            assert_eq!(got.header.replica_count, 1);
            assert_eq!({ got.header.extent_length }, i * 0x1000);
            assert_eq!(got.active_replicas()[0].disk_id, (i & 0xff) as u16);
        }
        let big = back.get(99).unwrap();
        assert_eq!(big.header.replica_count, 4);
        assert_eq!(big.active_replicas().len(), 4);
        assert_eq!(big.active_replicas()[3].bucket_no, 3);
    }

    #[test]
    fn location_table_region_round_trip_single_entry() {
        let (_dir, dev) = fresh_device();
        let mut t = LocationTable::new();
        let r = ReplicaRef { disk_id: 1, sector_offset: 2, bucket_no: 3 };
        t.insert(7, make_loc(0x4000, &[r]));
        t.flush_to_region(&dev, 0).unwrap();
        let back = LocationTable::load_from_region(&dev, 0).unwrap();
        assert_eq!(back.len(), 1);
        let got = back.get(7).unwrap();
        assert_eq!({ got.header.extent_length }, 0x4000);
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

        let back = LocationTable::load_from_region(&dev, 0).unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!({ back.get(99).unwrap().header.extent_length }, 0x999);
        assert!(back.get(1).is_none());
    }

    #[test]
    fn location_table_region_load_kind_mismatch_fails() {
        use mimisbrunnr_storage::BtreeRegion;
        let (_dir, dev) = fresh_device();
        let mut node: LoadedNode<u64, Vec<u8>> =
            LoadedNode::new(BtreeKind::ObjectTable, 0, REGION_SIZE_LOG2);
        let run = SortedRun::from_sorted(0, 0, vec![(1u64, vec![0u8; 8])]);
        node.sorted_runs.push(run);
        node.header.sorted_run_count = 1;
        BtreeRegion::write_full::<_, u64, Vec<u8>>(&dev, 0, &mut node).unwrap();

        let err = LocationTable::load_from_region(&dev, 0).unwrap_err();
        match err {
            MetaError::Storage(_) => {}
            other => panic!("expected MetaError::Storage, got {other:?}"),
        }
    }
}
