//! In-memory mirror of the object record table.
//!
//! ## Persistence
//!
//! On disk the table occupies one 256 KiB [`Region`] of
//! [`BtreeKind::ObjectTable`] keyed by [`ObjectTableKey`] (`oid`, `snapshot`);
//! values are 128-byte [`ObjectRecord`] byte images.
//!
//! The `snapshot` field on the on-disk key is always 0 for now —
//! snapshot-aware bkey position per SNAPSHOTS-DESIGN§1.2 lands with SNAPSHOT2. The shape is
//! present today so the migration to snapshot-keyed records is a value
//! mutation rather than a key-shape mutation.
//!
//! TODO(BTREE10): the object table is a positional radix tree whose leaves
//! are page regions (METADATA-DESIGN§1, BTREE-DESIGN§1.7); until page regions
//! land it is one keyed-run region.

use std::collections::{BTreeMap, btree_map};

use mimisbrunnr_storage::{
    BlockDevice, BtreeKind, StorageError,
    region::{FieldKind, Region, RegionKey, RegionParams, RunEntry},
};

use crate::{
    error::MetaError,
    record::{OBJECT_RECORD_SIZE, ObjectRecord},
};

/// 256 KiB region size in bytes (matches the spec's 18-bit
/// `region_size_log2`). Engine code sizes the on-disk slot from this.
pub const OBJECT_TABLE_REGION_SIZE: u64 = 256 * 1024;
const REGION_SIZE_LOG2: u8 = 18;

// ---------- ObjectTableKey (region key) ----------

/// Region key for the object record table.
///
/// `oid` is the raw `ObjectId.to_u64()`. `snapshot` mirrors the
/// snapshot-aware bkey position (SNAPSHOTS-DESIGN§1.2) and is always 0 in the
/// current revision — the field is plumbed so the key shape doesn't
/// move when SNAPSHOT2 starts emitting non-zero snapshot ids.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ObjectTableKey {
    /// Raw `ObjectId.to_u64()`.
    pub oid: u64,
    /// Snapshot id; always 0 until SNAPSHOT2.
    pub snapshot: u32,
}

impl ObjectTableKey {
    /// Construct a key for a given `oid` at `snapshot = 0`.
    pub const fn current(oid: u64) -> Self {
        Self { oid, snapshot: 0 }
    }
}

impl RegionKey for ObjectTableKey {
    const FIELDS: &'static [FieldKind] = &[FieldKind::Unsigned, FieldKind::Unsigned];

    fn field_values(&self, out: &mut [u64]) {
        out[0] = self.oid;
        out[1] = self.snapshot as u64;
    }

    fn from_field_values(fields: &[u64]) -> Result<Self, StorageError> {
        let snapshot = u32::try_from(fields[1])
            .map_err(|_| StorageError::RegionCorrupt("object table snapshot field out of range"))?;
        Ok(Self {
            oid: fields[0],
            snapshot,
        })
    }
}

// ---------- ObjectTable (in-memory mirror) ----------

/// Placeholder object table keyed by raw `ObjectId.to_u64()`.
///
/// The in-memory map drops the `snapshot` axis — the engine only
/// operates on the live (snapshot=0) view today. Persistence
/// nonetheless threads the full `(oid, snapshot)` key, so the snapshot
/// axis is a free upgrade once SNAPSHOT2 lands.
#[derive(Debug, Default)]
pub struct ObjectTable {
    records: BTreeMap<u64, ObjectRecord>,
}

impl ObjectTable {
    /// Empty table.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of records currently held.
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// `true` if the table is empty.
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Insert (or overwrite) the record for `record.id`.
    pub fn insert(&mut self, record: ObjectRecord) {
        let id = record.id;
        self.records.insert(id, record);
    }

    /// Look up by raw object id.
    pub fn get(&self, oid: u64) -> Option<&ObjectRecord> {
        self.records.get(&oid)
    }

    /// Mutable lookup.
    pub fn get_mut(&mut self, oid: u64) -> Option<&mut ObjectRecord> {
        self.records.get_mut(&oid)
    }

    /// Remove the record for `oid`. Returns the removed record.
    pub fn remove(&mut self, oid: u64) -> Option<ObjectRecord> {
        self.records.remove(&oid)
    }

    /// Iterator over `(oid, record)` pairs in ascending id order.
    pub fn iter(&self) -> btree_map::Iter<'_, u64, ObjectRecord> {
        self.records.iter()
    }

    /// Write the table as the 256 KiB region at byte `offset`, keyed by
    /// `(oid, snapshot = 0)`, each value a 128-byte [`ObjectRecord`].
    pub fn flush_to_region<D: BlockDevice>(
        &self,
        device: &D,
        offset: u64,
    ) -> Result<(), MetaError> {
        let mut region = Region::create_in_place(device, offset, region_params(), &[])?;
        let entries: Vec<(ObjectTableKey, RunEntry<'_>)> = self
            .records
            .iter()
            .map(|(oid, record)| {
                (
                    ObjectTableKey::current(*oid),
                    RunEntry::Value(record.as_bytes().as_slice()),
                )
            })
            .collect();
        region.append_entries(device, 0, &entries)?;
        Ok(())
    }

    /// Read the table from the region at byte `offset`; a never-written
    /// (all-zero) region is an empty table. Records whose `snapshot` is
    /// non-zero are skipped — the engine only consumes the live view today.
    pub fn load_from_region<D: BlockDevice>(
        device: &D,
        offset: u64,
    ) -> Result<Self, MetaError> {
        let mut probe = [0u8; 8];
        device.read_at(offset, &mut probe)?;
        if probe.iter().all(|&byte| byte == 0) {
            return Ok(Self::default());
        }
        let region = Region::open(device, offset, BtreeKind::ObjectTable)?;
        let mut records = BTreeMap::new();
        for (key, value) in region.entries::<D, ObjectTableKey>(device)? {
            if key.snapshot != 0 {
                // TODO(SNAPSHOT3): merge snapshot history into the
                // sidecar `ObjectHistory` btree.
                continue;
            }
            let record = bytemuck::try_pod_read_unaligned::<ObjectRecord>(&value).map_err(|_| {
                MetaError::BufferTooSmall {
                    needed: OBJECT_RECORD_SIZE,
                    got: value.len(),
                }
            })?;
            records.insert(key.oid, record);
        }
        Ok(Self { records })
    }
}

fn region_params() -> RegionParams {
    RegionParams {
        kind: BtreeKind::ObjectTable,
        level: 0,
        region_size_log2: REGION_SIZE_LOG2,
        node_seq: 0,
        created_lsn: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_get_remove() {
        let mut t = ObjectTable::new();
        assert!(t.is_empty());
        let r = ObjectRecord::new(0xabcd_0000_0001_0000);
        t.insert(r);
        assert_eq!(t.len(), 1);
        let got = t.get(r.id).unwrap();
        assert_eq!({ got.id }, r.id);
        let removed = t.remove(r.id).unwrap();
        assert_eq!({ removed.id }, r.id);
        assert!(t.is_empty());
    }

    #[test]
    fn iter_ascending() {
        let mut t = ObjectTable::new();
        t.insert(ObjectRecord::new(3));
        t.insert(ObjectRecord::new(1));
        t.insert(ObjectRecord::new(2));
        let ids: Vec<u64> = t.iter().map(|(id, _)| *id).collect();
        assert_eq!(ids, vec![1, 2, 3]);
    }

    #[test]
    fn key_orders_by_oid_then_snapshot() {
        let a = ObjectTableKey { oid: 1, snapshot: 0 };
        let b = ObjectTableKey { oid: 1, snapshot: 1 };
        let c = ObjectTableKey { oid: 2, snapshot: 0 };
        assert!(a < b);
        assert!(b < c);
    }

    #[test]
    fn key_round_trips_through_its_fields() {
        let key = ObjectTableKey { oid: 0xabcd_0000_0000_0042, snapshot: 7 };
        let mut fields = [0u64; 2];
        key.field_values(&mut fields);
        assert_eq!(fields, [0xabcd_0000_0000_0042, 7]);
        assert_eq!(ObjectTableKey::from_field_values(&fields).unwrap(), key);
        assert!(ObjectTableKey::from_field_values(&[1, u64::MAX]).is_err());
    }

    // ----- B+ tree region round-trip -----

    use mimisbrunnr_storage::FileBlockDevice;
    use tempfile::TempDir;

    fn fresh_device() -> (TempDir, FileBlockDevice) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("object_table.bin");
        let dev = FileBlockDevice::open(&path, 1 << 20).unwrap();
        (dir, dev)
    }

    fn rec(id: u64) -> ObjectRecord {
        let mut r = ObjectRecord::new(id);
        r.generation = (id & 0xff) as u32;
        r.record_version = 1;
        r.content_hash = [(id & 0xff) as u8; 32];
        r.blob_offset = id * 0x1000;
        r.blob_length = 4096;
        r.created_ns = 1_700_000_000_000_000_000;
        r.modified_ns = 1_700_001_000_000_000_000;
        r.tag_count = 2;
        r.attr_count = 1;
        r.relation_count = 0;
        r.inline_tags = [10, 20, 30, 40];
        r.last_modify_lsn = id;
        r
    }

    fn assert_record_eq(a: &ObjectRecord, b: &ObjectRecord) {
        assert_eq!(a.as_bytes(), b.as_bytes());
    }

    #[test]
    fn object_table_region_round_trip_empty() {
        let (_dir, dev) = fresh_device();
        let t = ObjectTable::load_from_region(&dev, 0).unwrap();
        assert!(t.is_empty());
    }

    #[test]
    fn empty_table_flushes_to_a_region_without_runs() {
        let (_dir, dev) = fresh_device();
        ObjectTable::new().flush_to_region(&dev, 0).unwrap();
        let region = Region::open(&dev, 0, BtreeKind::ObjectTable).unwrap();
        assert_eq!(region.run_count(), 0);
        let back = ObjectTable::load_from_region(&dev, 0).unwrap();
        assert!(back.is_empty());
    }

    #[test]
    fn object_table_region_stores_record_images_under_oid_snapshot_keys() {
        let (_dir, dev) = fresh_device();
        let mut t = ObjectTable::new();
        for i in 1u64..=10 {
            t.insert(rec(i));
        }
        t.flush_to_region(&dev, 0).unwrap();
        let region = Region::open(&dev, 0, BtreeKind::ObjectTable).unwrap();
        assert_eq!(region.run_count(), 1);
        let value = region.lookup(&dev, &ObjectTableKey::current(7)).unwrap().unwrap();
        assert_eq!(value.as_bytes(), rec(7).as_bytes().as_slice());
        assert!(region.lookup(&dev, &ObjectTableKey::current(11)).unwrap().is_none());
    }

    #[test]
    fn object_table_region_round_trip_preserves_records() {
        let (_dir, dev) = fresh_device();
        let mut t = ObjectTable::new();
        for i in 1u64..=50 {
            t.insert(rec(i));
        }
        t.flush_to_region(&dev, 0).unwrap();
        let back = ObjectTable::load_from_region(&dev, 0).unwrap();
        assert_eq!(back.len(), 50);
        for i in 1u64..=50 {
            let original = rec(i);
            let loaded = back.get(i).expect("oid missing");
            assert_record_eq(loaded, &original);
        }
    }

    #[test]
    fn object_table_region_round_trip_spans_many_data_blocks() {
        // Node id in the high 16 bits; 1 500 records fill most of the region.
        let (_dir, dev) = fresh_device();
        let node_high = 0x0003u64 << 48;
        let mut t = ObjectTable::new();
        for local in 0u64..1500 {
            t.insert(rec(node_high | local));
        }
        t.flush_to_region(&dev, 0).unwrap();
        let back = ObjectTable::load_from_region(&dev, 0).unwrap();
        assert_eq!(back.len(), 1500);
        for local in [0u64, 1, 777, 1499] {
            assert_record_eq(back.get(node_high | local).unwrap(), &rec(node_high | local));
        }
    }

    #[test]
    fn object_table_region_round_trip_single_record() {
        let (_dir, dev) = fresh_device();
        let mut t = ObjectTable::new();
        t.insert(rec(42));
        t.flush_to_region(&dev, 0).unwrap();
        let back = ObjectTable::load_from_region(&dev, 0).unwrap();
        assert_eq!(back.len(), 1);
        assert_record_eq(back.get(42).unwrap(), &rec(42));
    }

    #[test]
    fn object_table_region_overwrite_replaces_state() {
        let (_dir, dev) = fresh_device();
        let mut first = ObjectTable::new();
        first.insert(rec(1));
        first.insert(rec(2));
        first.flush_to_region(&dev, 0).unwrap();

        let mut second = ObjectTable::new();
        second.insert(rec(99));
        second.flush_to_region(&dev, 0).unwrap();

        let back = ObjectTable::load_from_region(&dev, 0).unwrap();
        assert_eq!(back.len(), 1);
        assert_record_eq(back.get(99).unwrap(), &rec(99));
        assert!(back.get(1).is_none());
    }

    #[test]
    fn object_table_region_load_skips_snapshot_versions() {
        let (_dir, dev) = fresh_device();
        let mut region = Region::create_in_place(&dev, 0, region_params(), &[]).unwrap();
        let live = rec(5);
        let older = rec(6);
        region
            .append_entries(
                &dev,
                0,
                &[
                    (ObjectTableKey::current(5), RunEntry::Value(live.as_bytes().as_slice())),
                    (ObjectTableKey { oid: 5, snapshot: 3 }, RunEntry::Value(older.as_bytes().as_slice())),
                ],
            )
            .unwrap();
        let back = ObjectTable::load_from_region(&dev, 0).unwrap();
        assert_eq!(back.len(), 1);
        assert_record_eq(back.get(5).unwrap(), &live);
    }

    #[test]
    fn object_table_region_rejects_wrong_value_size() {
        let (_dir, dev) = fresh_device();
        let mut region = Region::create_in_place(&dev, 0, region_params(), &[]).unwrap();
        let short_value = [0u8; 10];
        region
            .append_entries(&dev, 0, &[(ObjectTableKey::current(1), RunEntry::Value(&short_value))])
            .unwrap();
        assert!(matches!(
            ObjectTable::load_from_region(&dev, 0),
            Err(MetaError::BufferTooSmall {
                needed: OBJECT_RECORD_SIZE,
                got: 10
            })
        ));
    }

    #[test]
    fn object_table_region_load_kind_mismatch_fails() {
        // Write a Backpointer-kind region into the slot and ensure
        // `load_from_region` refuses it via the kind check.
        let (_dir, dev) = fresh_device();
        let mut backpointers = crate::BackpointerTable::new();
        backpointers.insert(crate::BackpointerKey::new(1, 2, 3), crate::BackpointerValue::default());
        backpointers.flush_to_region(&dev, 0).unwrap();

        let err = ObjectTable::load_from_region(&dev, 0).unwrap_err();
        assert!(matches!(
            err,
            MetaError::Storage(StorageError::RegionKindMismatch { expected, actual })
                if expected == BtreeKind::ObjectTable as u16 && actual == BtreeKind::Backpointer as u16
        ));
    }
}
