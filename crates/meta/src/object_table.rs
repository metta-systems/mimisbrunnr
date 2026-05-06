//! In-memory placeholder for the object record table.
//!
// TODO(rewrite-phase-N): replace with the §5 COW radix tree of large nodes
// (`BtreeKind::ObjectTable`) backed by mimisbrunnr-storage's btree node
// cache, with WAL-journalled positional updates. The interface here is
// intentionally minimal — just enough for upstream callers to wire reads
// and writes through a typed entry-point.

use std::collections::{BTreeMap, btree_map};

use crate::record::ObjectRecord;

/// Placeholder object table keyed by raw `ObjectId.to_u64()`.
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
}
