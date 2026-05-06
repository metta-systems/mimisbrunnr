//! In-memory mirror of the object record table.
//!
//! ## Persistence (R1b-3)
//!
//! On disk the table occupies one 256 KiB §1.5 B+ tree region of
//! [`BtreeKind::ObjectTable`]. The mirror is materialised into a single
//! CBOR-encoded sorted run via [`BtreeRegion::write_full`] keyed by an
//! [`ObjectTableKey`] (`oid`, `snapshot`) pair ascending; values are
//! 128-byte [`ObjectRecord`] byte images carried in
//! [`ObjectTableValue`]'s `Vec<u8>` newtype.
//!
//! The `snapshot` field on the on-disk key is always 0 for now —
//! snapshot-aware bkey position per IMPL §11.2 lands in R6. The shape is
//! present today so the migration to snapshot-keyed records is a value
//! mutation rather than a key-shape mutation.
//!
//! TODO(rewrite-phase-R1d): swap to true positional-radix encoding per
//! IMPL §5 (`BtreeKind::ObjectTable` with positional addressing rather
//! than a sorted run keyed by `(oid, snapshot)`). Native encoding
//! requires the leaf occupancy bitmap and WAL-journalled positional
//! updates; the current sorted-run path lets the table persist while
//! those land.

use std::collections::{BTreeMap, btree_map};

use {
    mimisbrunnr_storage::{BlockDevice, BtreeKind, LoadedNode, SortedRun},
    serde::{Deserialize, Serialize},
};

use crate::{
    error::MetaError,
    record::{OBJECT_RECORD_SIZE, ObjectRecord},
};

/// 256 KiB region size in bytes (matches the spec's 18-bit
/// `region_size_log2`). Engine code sizes the on-disk slot from this.
pub const OBJECT_TABLE_REGION_SIZE: u64 = 256 * 1024;
const REGION_SIZE_LOG2: u8 = 18;

// ---------- ObjectTableKey / ObjectTableValue (B+ tree wire types) ----------

/// B+ tree key for the object record table.
///
/// `oid` is the raw `ObjectId.to_u64()`. `snapshot` mirrors the
/// snapshot-aware bkey position (IMPL §11.2) and is always 0 in the
/// current revision — the field is plumbed so the wire shape doesn't
/// move when R6 starts emitting non-zero snapshot ids.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ObjectTableKey {
    /// Raw `ObjectId.to_u64()`.
    pub oid: u64,
    /// Snapshot id; always 0 until R6.
    pub snapshot: u32,
}

impl ObjectTableKey {
    /// Construct a key for a given `oid` at `snapshot = 0`.
    pub const fn current(oid: u64) -> Self {
        Self { oid, snapshot: 0 }
    }
}

/// B+ tree value for the object record table: the 128-byte byte image of
/// an [`ObjectRecord`] (IMPL §5.1) carried as a length-tagged byte
/// vector. The wrapper exists so the `LoadedNode<ObjectTableKey,
/// ObjectTableValue>` type isn't generic over `ObjectRecord`'s own
/// `Serialize` impl, which lets future revisions evolve the wire form
/// (e.g. snapshot-aware compaction tombstones) without touching the
/// record's primary serde derives.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObjectTableValue(pub Vec<u8>);

impl ObjectTableValue {
    /// Serialise an `ObjectRecord` to its 128-byte byte image.
    pub fn from_record(record: &ObjectRecord) -> Self {
        Self(record.as_bytes().to_vec())
    }

    /// Decode the carried bytes into an `ObjectRecord`. Returns
    /// [`MetaError::BufferTooSmall`] if the byte image is the wrong
    /// length.
    pub fn to_record(&self) -> Result<ObjectRecord, MetaError> {
        if self.0.len() != OBJECT_RECORD_SIZE {
            return Err(MetaError::BufferTooSmall {
                needed: OBJECT_RECORD_SIZE,
                got: self.0.len(),
            });
        }
        let mut buf = [0u8; OBJECT_RECORD_SIZE];
        buf.copy_from_slice(&self.0);
        Ok(*ObjectRecord::ref_from_bytes(&buf))
    }
}

// ---------- ObjectTable (in-memory mirror) ----------

/// Placeholder object table keyed by raw `ObjectId.to_u64()`.
///
/// The in-memory map drops the `snapshot` axis — the engine only
/// operates on the live (snapshot=0) view today. Persistence
/// nonetheless threads the full `(oid, snapshot)` key per the wire
/// type, so the snapshot axis is a free upgrade once R6 lands.
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

    // ----------------------------------------------------------------
    // R1b-3: §1.5 B+ tree persistence (CBOR-encoded sorted run).
    // ----------------------------------------------------------------

    /// Build a [`LoadedNode`] containing every record as a single CBOR
    /// sorted run, sorted by `(oid, snapshot=0)`. The node uses
    /// [`BtreeKind::ObjectTable`] and the spec's 18-bit (256 KiB) region
    /// size.
    pub fn to_loaded_node(&self) -> LoadedNode<ObjectTableKey, ObjectTableValue> {
        // BTreeMap iteration is already sorted ascending by oid, and snapshot
        // is 0 for every entry, so the resulting tuple ordering is monotonic.
        let entries: Vec<(ObjectTableKey, ObjectTableValue)> = self
            .records
            .iter()
            .map(|(oid, rec)| {
                (
                    ObjectTableKey::current(*oid),
                    ObjectTableValue::from_record(rec),
                )
            })
            .collect();

        let mut node: LoadedNode<ObjectTableKey, ObjectTableValue> =
            LoadedNode::new(BtreeKind::ObjectTable, 0, REGION_SIZE_LOG2);
        if !entries.is_empty() {
            let run = SortedRun::from_sorted(0, 0, entries);
            node.sorted_runs.push(run);
            node.header.sorted_run_count = 1;
        }
        node
    }

    /// Restore the in-memory state from a [`LoadedNode`] read via
    /// [`BtreeRegion::read`]. Records whose `snapshot` is non-zero are
    /// skipped — the engine only consumes the live view today.
    pub fn from_loaded_node(
        node: &LoadedNode<ObjectTableKey, ObjectTableValue>,
    ) -> Result<Self, MetaError> {
        let mut records: BTreeMap<u64, ObjectRecord> = BTreeMap::new();
        for (k, v) in node.merge_iter() {
            if k.snapshot != 0 {
                // TODO(rewrite-phase-R6): merge snapshot history into the
                // sidecar `ObjectHistory` btree.
                continue;
            }
            records.insert(k.oid, v.to_record()?);
        }
        Ok(Self { records })
    }

    /// Write the in-memory state as a fresh 256 KiB region at byte `offset`
    /// on `device`.
    ///
    /// **R1c-A2**: writes via the native positional radix-leaf format
    /// per IMPL §5 (see [`crate::ObjectLeaf`]) — *no longer* via the
    /// sorted-run B+ tree path. Leaf-only trees: any `oid_local ≥
    /// LEAF_RECORDS (2044)` returns
    /// [`MetaError::OidOutOfRange`]; multi-level descent + tree growth
    /// land in a follow-up.
    ///
    /// The previous sorted-run [`Self::to_loaded_node`] /
    /// [`Self::from_loaded_node`] helpers stay live for now (they
    /// shipped under R1b-3 and may still be useful for tooling /
    /// dump-and-diff workflows), but are no longer called by the
    /// engine's persistence path.
    pub fn flush_to_region<D: BlockDevice>(
        &self,
        device: &D,
        offset: u64,
    ) -> Result<(), MetaError> {
        let mut leaf = crate::object_leaf::ObjectLeaf::new();
        for (oid, record) in self.records.iter() {
            // ObjectId.local is the bottom 48 bits; for leaf-only trees
            // we further restrict to the first 2044 ids. Larger oids
            // need multi-level descent (TODO follow-up).
            let oid_local = oid & ((1u64 << 48) - 1);
            leaf.set(oid_local, *record)?;
        }
        leaf.write(device, offset)?;
        Ok(())
    }

    /// Read the in-memory state from the 256 KiB region at byte `offset`
    /// on `device`. An all-zero region returns [`Self::default`].
    ///
    /// **R1c-A2**: reads via the native positional radix-leaf format.
    pub fn load_from_region<D: BlockDevice>(
        device: &D,
        offset: u64,
    ) -> Result<Self, MetaError> {
        let leaf = crate::object_leaf::ObjectLeaf::read(device, offset)?;
        let mut records: BTreeMap<u64, ObjectRecord> = BTreeMap::new();
        for (oid_local, record) in leaf.iter() {
            // Leaf only carries `oid_local`; the high 16 bits (node_id)
            // are *not* persisted in the leaf itself. Read them back
            // from the record's own `id` field, which carries the full
            // raw `ObjectId.to_u64()` (see `ObjectRecord::new`).
            let full_oid = { record.id };
            let _ = oid_local; // sanity: low 48 bits should match.
            records.insert(full_oid, *record);
        }
        Ok(Self { records })
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
    fn value_round_trip_via_helpers() {
        let mut r = ObjectRecord::new(0xdead_beef);
        r.generation = 7;
        r.tag_count = 3;
        r.last_modify_lsn = 42;
        let v = ObjectTableValue::from_record(&r);
        assert_eq!(v.0.len(), OBJECT_RECORD_SIZE);
        let back = v.to_record().unwrap();
        assert_eq!({ back.id }, r.id);
        assert_eq!({ back.generation }, r.generation);
        assert_eq!({ back.tag_count }, r.tag_count);
        assert_eq!({ back.last_modify_lsn }, r.last_modify_lsn);
    }

    #[test]
    fn value_decode_rejects_wrong_size() {
        let v = ObjectTableValue(vec![0u8; 10]);
        assert!(matches!(
            v.to_record(),
            Err(MetaError::BufferTooSmall {
                needed: OBJECT_RECORD_SIZE,
                got: 10
            })
        ));
    }

    // ----- B+ tree region round-trip (R1b-3) -----

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
    fn empty_loaded_node_round_trip() {
        let t = ObjectTable::new();
        let node = t.to_loaded_node();
        assert_eq!(node.sorted_runs.len(), 0);
        let back = ObjectTable::from_loaded_node(&node).unwrap();
        assert!(back.is_empty());
    }

    #[test]
    fn loaded_node_round_trip_preserves_records() {
        let mut t = ObjectTable::new();
        for i in 1u64..=10 {
            t.insert(rec(i));
        }
        let node = t.to_loaded_node();
        assert_eq!(node.sorted_runs.len(), 1);
        let back = ObjectTable::from_loaded_node(&node).unwrap();
        assert_eq!(back.len(), 10);
        for i in 1u64..=10 {
            assert_record_eq(back.get(i).unwrap(), &rec(i));
        }
    }

    #[test]
    fn object_table_region_round_trip_empty() {
        let (_dir, dev) = fresh_device();
        let t = ObjectTable::load_from_region(&dev, 0).unwrap();
        assert!(t.is_empty());
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
    fn object_table_region_load_kind_mismatch_fails() {
        // Write a Backpointer-kind region into the slot and ensure
        // `load_from_region` refuses it via the kind check.
        use mimisbrunnr_storage::BtreeRegion;
        let (_dir, dev) = fresh_device();
        let mut node: LoadedNode<u64, Vec<u8>> =
            LoadedNode::new(BtreeKind::Backpointer, 0, REGION_SIZE_LOG2);
        // Sole entry to force a non-trivial header.
        let run = SortedRun::from_sorted(0, 0, vec![(1u64, vec![0u8; 8])]);
        node.sorted_runs.push(run);
        node.header.sorted_run_count = 1;
        BtreeRegion::write_full::<_, u64, Vec<u8>>(&dev, 0, &mut node).unwrap();

        let err = ObjectTable::load_from_region(&dev, 0).unwrap_err();
        match err {
            MetaError::Storage(_) => {}
            other => panic!("expected MetaError::Storage, got {other:?}"),
        }
    }
}
