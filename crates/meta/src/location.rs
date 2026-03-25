/// Maps an object to its physical extent on disk, supporting multi-disk pools.
///
/// Binary layout (40 bytes, all little-endian):
/// ```text
///  [0..2]   disk_id (u16)
///  [2..10]  extent_offset (u64)
///  [10..18] extent_length (u64)
///  [18..19] replica_count (u8)
///  [19..40] replicas: 3 × ReplicaRef (7 bytes each = 21 bytes)
/// ```
pub const LOCATION_SIZE: usize = 40;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObjectLocation {
    pub disk_id: u16,
    pub extent_offset: u64,
    pub extent_length: u64,
    pub replica_count: u8,
    pub replicas: [ReplicaRef; 3],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ReplicaRef {
    pub disk_id: u16,
    pub offset: u64, // stored as 5 bytes to fit in 7 bytes total
}

impl ObjectLocation {
    pub fn new(disk_id: u16, extent_offset: u64, extent_length: u64) -> Self {
        Self {
            disk_id,
            extent_offset,
            extent_length,
            replica_count: 0,
            replicas: [ReplicaRef::default(); 3],
        }
    }

    pub fn to_bytes(&self) -> [u8; LOCATION_SIZE] {
        let mut buf = [0u8; LOCATION_SIZE];
        buf[0..2].copy_from_slice(&self.disk_id.to_le_bytes());
        buf[2..10].copy_from_slice(&self.extent_offset.to_le_bytes());
        buf[10..18].copy_from_slice(&self.extent_length.to_le_bytes());
        buf[18] = self.replica_count;
        for (i, r) in self.replicas.iter().enumerate() {
            let off = 19 + i * 7;
            buf[off..off + 2].copy_from_slice(&r.disk_id.to_le_bytes());
            // Store lower 5 bytes of offset
            let offset_bytes = r.offset.to_le_bytes();
            buf[off + 2..off + 7].copy_from_slice(&offset_bytes[..5]);
        }
        buf
    }

    pub fn from_bytes(buf: &[u8; LOCATION_SIZE]) -> Self {
        let disk_id = u16::from_le_bytes(buf[0..2].try_into().unwrap());
        let extent_offset = u64::from_le_bytes(buf[2..10].try_into().unwrap());
        let extent_length = u64::from_le_bytes(buf[10..18].try_into().unwrap());
        let replica_count = buf[18];
        let mut replicas = [ReplicaRef::default(); 3];
        for (i, r) in replicas.iter_mut().enumerate() {
            let off = 19 + i * 7;
            r.disk_id = u16::from_le_bytes(buf[off..off + 2].try_into().unwrap());
            let mut offset_bytes = [0u8; 8];
            offset_bytes[..5].copy_from_slice(&buf[off + 2..off + 7]);
            r.offset = u64::from_le_bytes(offset_bytes);
        }
        Self {
            disk_id,
            extent_offset,
            extent_length,
            replica_count,
            replicas,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_location() {
        let loc = ObjectLocation::new(0, 0x1000, 0x2000);
        assert_eq!(loc.disk_id, 0);
        assert_eq!(loc.extent_offset, 0x1000);
        assert_eq!(loc.extent_length, 0x2000);
        assert_eq!(loc.replica_count, 0);
    }

    #[test]
    fn round_trip() {
        let mut loc = ObjectLocation::new(3, 0xDEAD_BEEF, 0x1_0000);
        loc.replica_count = 2;
        loc.replicas[0] = ReplicaRef {
            disk_id: 1,
            offset: 0x5000,
        };
        loc.replicas[1] = ReplicaRef {
            disk_id: 2,
            offset: 0x6000,
        };

        let bytes = loc.to_bytes();
        assert_eq!(bytes.len(), LOCATION_SIZE);

        let loc2 = ObjectLocation::from_bytes(&bytes);
        assert_eq!(loc2.disk_id, 3);
        assert_eq!(loc2.extent_offset, 0xDEAD_BEEF);
        assert_eq!(loc2.extent_length, 0x1_0000);
        assert_eq!(loc2.replica_count, 2);
        assert_eq!(loc2.replicas[0].disk_id, 1);
        assert_eq!(loc2.replicas[0].offset, 0x5000);
        assert_eq!(loc2.replicas[1].disk_id, 2);
        assert_eq!(loc2.replicas[1].offset, 0x6000);
    }

    #[test]
    fn location_size_is_40() {
        assert_eq!(LOCATION_SIZE, 40);
    }
}
