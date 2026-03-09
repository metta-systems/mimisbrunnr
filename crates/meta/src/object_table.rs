use mimisbrunnr_storage::BlockDevice;
use mimisbrunnr_types::ObjectId;

use crate::error::MetaError;
use crate::record::{ObjectRecord, RECORD_SIZE};

/// An array-indexed object table backed by the metadata zone on disk.
///
/// Objects are looked up in O(1) by computing `zone_offset + local_id * RECORD_SIZE`.
/// The table is also maintained in-memory for fast access.
pub struct ObjectTable {
    /// In-memory copy of all records. `None` means the slot is unoccupied.
    records: Vec<Option<ObjectRecord>>,
    /// Offset on the block device where the table starts.
    zone_offset: u64,
    /// Maximum number of objects this table can hold.
    capacity: u64,
    /// Next local sequence to assign (monotonically increasing).
    next_local_seq: u64,
}

impl ObjectTable {
    /// Create a new empty object table.
    pub fn new(zone_offset: u64, zone_size: u64) -> Self {
        let capacity = zone_size / RECORD_SIZE as u64;
        Self {
            records: Vec::new(),
            zone_offset,
            capacity,
            next_local_seq: 0,
        }
    }

    /// Load the object table from disk, reading all non-zero records.
    pub fn load(dev: &dyn BlockDevice, zone_offset: u64, zone_size: u64) -> Result<Self, MetaError> {
        let capacity = zone_size / RECORD_SIZE as u64;
        let mut records = Vec::with_capacity(capacity as usize);
        let mut buf = [0u8; RECORD_SIZE];
        let mut next_local_seq = 0u64;

        for i in 0..capacity {
            let offset = zone_offset + i * RECORD_SIZE as u64;
            dev.read_at(offset, &mut buf)?;

            // Check if slot is all zeros (empty)
            if buf.iter().all(|&b| b == 0) {
                records.push(None);
            } else {
                match ObjectRecord::from_bytes(&buf) {
                    Ok(rec) => {
                        let local = ObjectId::from_raw(rec.id).local();
                        if local >= next_local_seq {
                            next_local_seq = local + 1;
                        }
                        records.push(Some(rec));
                    }
                    Err(reason) => {
                        return Err(MetaError::Corrupt { slot: i, reason });
                    }
                }
            }
        }

        Ok(Self {
            records,
            zone_offset,
            capacity,
            next_local_seq,
        })
    }

    /// Allocate a new object, returning its ObjectId.
    pub fn create(&mut self, node_id: u16) -> Result<ObjectId, MetaError> {
        let local = self.next_local_seq;
        if local >= self.capacity {
            return Err(MetaError::TableFull { capacity: self.capacity });
        }

        let oid = ObjectId::new(node_id, local);
        let rec = ObjectRecord::new(oid.raw());

        // Ensure records vec is large enough
        while self.records.len() <= local as usize {
            self.records.push(None);
        }
        self.records[local as usize] = Some(rec);
        self.next_local_seq = local + 1;

        Ok(oid)
    }

    /// Get a record by ObjectId (O(1) lookup).
    pub fn get(&self, oid: ObjectId) -> Option<&ObjectRecord> {
        let local = oid.local() as usize;
        self.records.get(local).and_then(|r| r.as_ref())
    }

    /// Get a mutable record by ObjectId.
    pub fn get_mut(&mut self, oid: ObjectId) -> Option<&mut ObjectRecord> {
        let local = oid.local() as usize;
        self.records.get_mut(local).and_then(|r| r.as_mut())
    }

    /// Flush a single record to disk.
    pub fn flush_record(&self, dev: &dyn BlockDevice, oid: ObjectId) -> Result<(), MetaError> {
        let local = oid.local();
        let offset = self.zone_offset + local * RECORD_SIZE as u64;
        match &self.records[local as usize] {
            Some(rec) => {
                let bytes = rec.to_bytes();
                dev.write_at(offset, &bytes)?;
            }
            None => {
                dev.write_at(offset, &[0u8; RECORD_SIZE])?;
            }
        }
        Ok(())
    }

    /// Flush all records to disk.
    pub fn flush_all(&self, dev: &dyn BlockDevice) -> Result<(), MetaError> {
        for (i, slot) in self.records.iter().enumerate() {
            let offset = self.zone_offset + (i as u64) * RECORD_SIZE as u64;
            match slot {
                Some(rec) => dev.write_at(offset, &rec.to_bytes())?,
                None => dev.write_at(offset, &[0u8; RECORD_SIZE])?,
            }
        }
        Ok(())
    }

    /// Number of active (non-None) records.
    pub fn count(&self) -> usize {
        self.records.iter().filter(|r| r.is_some()).count()
    }

    /// Maximum capacity.
    pub fn capacity(&self) -> u64 {
        self.capacity
    }

    /// Iterate over all active records.
    pub fn iter(&self) -> impl Iterator<Item = &ObjectRecord> {
        self.records.iter().filter_map(|r| r.as_ref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mimisbrunnr_storage::FileBlockDevice;
    use mimisbrunnr_types::ObjectState;
    use tempfile::NamedTempFile;

    fn test_table() -> ObjectTable {
        // 1 MiB zone = 8192 record slots
        ObjectTable::new(0, 1024 * 1024)
    }

    #[test]
    fn create_objects() {
        let mut table = test_table();
        let oid1 = table.create(1).unwrap();
        let oid2 = table.create(1).unwrap();

        assert_eq!(oid1.node(), 1);
        assert_eq!(oid1.local(), 0);
        assert_eq!(oid2.local(), 1);
        assert_eq!(table.count(), 2);
    }

    #[test]
    fn get_record() {
        let mut table = test_table();
        let oid = table.create(1).unwrap();

        let rec = table.get(oid).unwrap();
        assert_eq!(rec.id, oid.raw());
        assert!(rec.is_active());
    }

    #[test]
    fn modify_record() {
        let mut table = test_table();
        let oid = table.create(1).unwrap();

        let rec = table.get_mut(oid).unwrap();
        rec.tag_count = 5;
        rec.blob_length = 1024;

        let rec = table.get(oid).unwrap();
        assert_eq!(rec.tag_count, 5);
        assert_eq!(rec.blob_length, 1024);
    }

    #[test]
    fn get_nonexistent_returns_none() {
        let table = test_table();
        let oid = ObjectId::new(1, 999);
        assert!(table.get(oid).is_none());
    }

    #[test]
    fn flush_and_reload() {
        let tmp = NamedTempFile::new().unwrap();
        let zone_size = 1024 * 1024u64;
        let dev = FileBlockDevice::open(tmp.path(), zone_size).unwrap();

        let mut table = ObjectTable::new(0, zone_size);
        let oid1 = table.create(1).unwrap();
        let oid2 = table.create(1).unwrap();

        // Modify a record
        table.get_mut(oid1).unwrap().content_hash = [0xAA; 32];
        table.get_mut(oid2).unwrap().state = ObjectState::Tombstoned;

        table.flush_all(&dev).unwrap();

        // Reload
        let table2 = ObjectTable::load(&dev, 0, zone_size).unwrap();
        assert_eq!(table2.count(), 2);

        let rec1 = table2.get(oid1).unwrap();
        assert_eq!(rec1.content_hash, [0xAA; 32]);

        let rec2 = table2.get(oid2).unwrap();
        assert_eq!(rec2.state, ObjectState::Tombstoned);
    }

    #[test]
    fn flush_single_record() {
        let tmp = NamedTempFile::new().unwrap();
        let zone_size = 1024 * 1024u64;
        let dev = FileBlockDevice::open(tmp.path(), zone_size).unwrap();

        let mut table = ObjectTable::new(0, zone_size);
        let oid = table.create(1).unwrap();
        table.get_mut(oid).unwrap().blob_length = 42;
        table.flush_record(&dev, oid).unwrap();

        // Read just that record back
        let mut buf = [0u8; RECORD_SIZE];
        dev.read_at(0, &mut buf).unwrap();
        let rec = ObjectRecord::from_bytes(&buf).unwrap();
        assert_eq!(rec.blob_length, 42);
    }

    #[test]
    fn capacity_matches_zone_size() {
        let table = ObjectTable::new(0, RECORD_SIZE as u64 * 100);
        assert_eq!(table.capacity(), 100);
    }

    #[test]
    fn iterate_records() {
        let mut table = test_table();
        table.create(1).unwrap();
        table.create(1).unwrap();
        table.create(1).unwrap();

        let ids: Vec<u64> = table.iter().map(|r| r.id).collect();
        assert_eq!(ids.len(), 3);
    }
}
