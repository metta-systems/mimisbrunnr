//! In-memory placeholder for the location table.
//!
// TODO(rewrite-phase-N): replace with the §6.1 COW radix tree of large
// nodes (`BtreeKind::LocationTable`, leaf fanout 5440), tracking the object
// table slot-for-slot. Same machinery as the object table but parameterised
// for 48-byte `ObjectLocation` records.

use std::collections::{BTreeMap, btree_map};

use crate::location::ObjectLocation;

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
}
