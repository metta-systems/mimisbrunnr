//! `NodePtr` — the replicated pointer to a tree node. ENGINE-DESIGN§1.2
//! "Node pointers".
//!
//! A `NodePtr` is `R` [`BlockRef`]s (`R` = `metadata_replicas`, 1..=4)
//! naming the `R` copies of one node, each on a different disk, the fastest
//! tier first. On disk it is exactly `16 × R` bytes: the `BlockRef`s back to
//! back. `R` is not stored in the pointer; the reader knows it from the root
//! list.
//!
//! A node is written to every copy before any pointer to it is committed
//! ([`NodePtr::write_all`]); a read takes the first copy that verifies and
//! reports the others it tried so reconcile can repair them
//! ([`NodePtr::read_first_verified`]).

use {
    crate::{block_device::BlockDevice, error::StorageError, root_pointer::BlockRef},
    smallvec::SmallVec,
};

/// Largest `metadata_replicas`.
pub const MAX_METADATA_REPLICAS: usize = 4;

/// Encoded size of one copy.
pub const NODE_PTR_COPY_SIZE: usize = core::mem::size_of::<BlockRef>();

/// The `R` copies of one node. Invariants: `1 <= R <= 4`, every copy on a
/// different disk.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct NodePtr {
    copies: SmallVec<[BlockRef; MAX_METADATA_REPLICAS]>,
}

/// What [`NodePtr::read_first_verified`] found.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReplicaRead {
    /// Index into [`NodePtr::copies`] of the copy whose bytes are returned.
    pub copy_index: usize,
    /// Copies tried before it that failed to read or to verify, in order;
    /// reconcile rewrites them.
    pub failed: Vec<BlockRef>,
}

impl NodePtr {
    /// A pointer to `copies`, the fastest tier first. Fails with
    /// [`StorageError::InvalidReplicaCount`] unless `1 <= copies.len() <= 4`,
    /// and with [`StorageError::DuplicateReplicaDisk`] when two copies share
    /// a disk.
    pub fn new(copies: &[BlockRef]) -> Result<Self, StorageError> {
        check_replica_count(copies.len())?;
        for (index, copy) in copies.iter().enumerate() {
            if copies[..index].iter().any(|earlier| earlier.disk_id == copy.disk_id) {
                return Err(StorageError::DuplicateReplicaDisk { disk_id: copy.disk_id });
            }
        }
        Ok(Self {
            copies: copies.iter().copied().collect(),
        })
    }

    /// The copies, the fastest tier first.
    pub fn copies(&self) -> &[BlockRef] {
        &self.copies
    }

    /// `R`, the number of copies.
    pub fn replicas(&self) -> usize {
        self.copies.len()
    }

    /// Encoded size: `16 × R` bytes.
    pub fn encoded_len(&self) -> usize {
        NODE_PTR_COPY_SIZE * self.replicas()
    }

    /// Write the pointer into `out`, which must be exactly
    /// [`Self::encoded_len`] bytes ([`StorageError::NodePtrLengthMismatch`]
    /// otherwise).
    pub fn encode_into(&self, out: &mut [u8]) -> Result<(), StorageError> {
        if out.len() != self.encoded_len() {
            return Err(StorageError::NodePtrLengthMismatch {
                expected: self.encoded_len(),
                actual: out.len(),
            });
        }
        let (slots, _) = out.as_chunks_mut::<NODE_PTR_COPY_SIZE>();
        for (slot, copy) in slots.iter_mut().zip(&self.copies) {
            *slot = bytemuck::cast(*copy);
        }
        Ok(())
    }

    /// The pointer as `16 × R` bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        self.copies
            .iter()
            .flat_map(|copy| bytemuck::bytes_of(copy).iter().copied())
            .collect()
    }

    /// Decode a pointer of `replicas` copies from `bytes`. Fails with
    /// [`StorageError::InvalidReplicaCount`] for `replicas` outside 1..=4,
    /// [`StorageError::NodePtrLengthMismatch`] unless `bytes` is
    /// `16 × replicas` long, [`StorageError::BlockRefReservedNonZero`] for a
    /// copy whose reserved word is set, and
    /// [`StorageError::DuplicateReplicaDisk`] when two copies share a disk.
    pub fn decode(bytes: &[u8], replicas: usize) -> Result<Self, StorageError> {
        check_replica_count(replicas)?;
        let expected = NODE_PTR_COPY_SIZE * replicas;
        if bytes.len() != expected {
            return Err(StorageError::NodePtrLengthMismatch {
                expected,
                actual: bytes.len(),
            });
        }
        let (encoded_copies, _) = bytes.as_chunks::<NODE_PTR_COPY_SIZE>();
        let copies: SmallVec<[BlockRef; MAX_METADATA_REPLICAS]> = encoded_copies
            .iter()
            .map(|encoded| bytemuck::cast::<[u8; NODE_PTR_COPY_SIZE], BlockRef>(*encoded))
            .collect();
        if let Some(copy) = copies.iter().find(|copy| copy._reserved != 0) {
            return Err(StorageError::BlockRefReservedNonZero { reserved: copy._reserved });
        }
        Self::new(&copies)
    }

    /// Write `node` to every copy. `resolve` maps a `disk_id` to its device
    /// and `bucket_size_log2`. Stops at the first failure: a node not on all
    /// `R` copies must not be pointed to.
    pub fn write_all<'device>(
        &self,
        resolve: impl Fn(u16) -> Result<(&'device dyn BlockDevice, u8), StorageError>,
        node: &[u8],
    ) -> Result<(), StorageError> {
        for copy in &self.copies {
            let (device, bucket_size_log2) = resolve(copy.disk_id)?;
            device.write_at(copy.byte_offset(bucket_size_log2), node)?;
        }
        Ok(())
    }

    /// Read the node into `buffer` from the first copy that reads and
    /// passes `verify`, trying copies in order (the fastest tier first).
    /// A copy whose disk does not resolve, whose read fails or whose bytes
    /// do not verify is listed in [`ReplicaRead::failed`]. Fails with
    /// [`StorageError::NoReplicaVerified`] when no copy verifies; `buffer`
    /// then holds the last copy's bytes or is partly written.
    pub fn read_first_verified<'device>(
        &self,
        resolve: impl Fn(u16) -> Result<(&'device dyn BlockDevice, u8), StorageError>,
        buffer: &mut [u8],
        verify: impl Fn(&[u8]) -> bool,
    ) -> Result<ReplicaRead, StorageError> {
        let mut failed = Vec::new();
        for (copy_index, copy) in self.copies.iter().enumerate() {
            let read = resolve(copy.disk_id)
                .and_then(|(device, bucket_size_log2)| device.read_at(copy.byte_offset(bucket_size_log2), buffer));
            if read.is_ok() && verify(buffer) {
                return Ok(ReplicaRead { copy_index, failed });
            }
            failed.push(*copy);
        }
        Err(StorageError::NoReplicaVerified {
            replicas: self.replicas(),
        })
    }
}

fn check_replica_count(replicas: usize) -> Result<(), StorageError> {
    if (1..=MAX_METADATA_REPLICAS).contains(&replicas) {
        Ok(())
    } else {
        Err(StorageError::InvalidReplicaCount { replicas })
    }
}

#[cfg(test)]
mod tests {
    use {super::*, crate::checksum::checksum64, std::sync::Mutex};

    /// A device held in memory.
    struct MemoryDevice {
        bytes: Mutex<Vec<u8>>,
    }

    impl MemoryDevice {
        fn new(capacity: usize) -> Self {
            Self {
                bytes: Mutex::new(vec![0; capacity]),
            }
        }

        fn range(&self, offset: u64, length: usize) -> Vec<u8> {
            let start = offset as usize;
            self.bytes.lock().unwrap()[start..start + length].to_vec()
        }

        fn corrupt(&self, offset: u64) {
            self.bytes.lock().unwrap()[offset as usize] ^= 0xff;
        }
    }

    impl BlockDevice for MemoryDevice {
        fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), StorageError> {
            let bytes = self.bytes.lock().unwrap();
            let start = offset as usize;
            let Some(source) = bytes.get(start..start + buf.len()) else {
                return Err(StorageError::OutOfBounds {
                    offset,
                    length: buf.len() as u64,
                    capacity: bytes.len() as u64,
                });
            };
            buf.copy_from_slice(source);
            Ok(())
        }

        fn write_at(&self, offset: u64, buf: &[u8]) -> Result<(), StorageError> {
            let mut bytes = self.bytes.lock().unwrap();
            let capacity = bytes.len() as u64;
            let start = offset as usize;
            let Some(target) = bytes.get_mut(start..start + buf.len()) else {
                return Err(StorageError::OutOfBounds {
                    offset,
                    length: buf.len() as u64,
                    capacity,
                });
            };
            target.copy_from_slice(buf);
            Ok(())
        }

        fn capacity(&self) -> u64 {
            self.bytes.lock().unwrap().len() as u64
        }

        fn sync(&self) -> Result<(), StorageError> {
            Ok(())
        }
    }

    /// 64 KiB buckets on 1 MiB devices.
    const BUCKET_LOG2: u8 = 16;
    const NODE_SIZE: usize = 8192;

    /// A node whose last 8 bytes are the checksum of the rest.
    fn node_bytes() -> Vec<u8> {
        let mut node: Vec<u8> = (0..NODE_SIZE).map(|index| (index * 7) as u8).collect();
        let checksum = checksum64(&node[..NODE_SIZE - 8]);
        node[NODE_SIZE - 8..].copy_from_slice(&checksum.to_le_bytes());
        node
    }

    fn verifies(node: &[u8]) -> bool {
        let (body, stored) = node.split_at(node.len() - 8);
        checksum64(body).to_le_bytes() == stored
    }

    fn two_disk_pointer() -> NodePtr {
        NodePtr::new(&[BlockRef::new(1, 3, 2, 9), BlockRef::new(2, 5, 0, 4)]).unwrap()
    }

    #[test]
    fn replicated_node_is_written_to_both_disks_and_read_from_the_survivor() {
        let first_disk = MemoryDevice::new(1 << 20);
        let second_disk = MemoryDevice::new(1 << 20);
        let resolve = |disk_id: u16| -> Result<(&dyn BlockDevice, u8), StorageError> {
            match disk_id {
                1 => Ok((&first_disk, BUCKET_LOG2)),
                2 => Ok((&second_disk, BUCKET_LOG2)),
                _ => Err(StorageError::UnknownDisk { disk_id }),
            }
        };
        let pointer = two_disk_pointer();
        let node = node_bytes();
        pointer.write_all(resolve, &node).unwrap();

        let first_offset = pointer.copies()[0].byte_offset(BUCKET_LOG2);
        let second_offset = pointer.copies()[1].byte_offset(BUCKET_LOG2);
        assert_eq!(first_offset, 3 * 65_536 + 2 * 4096);
        assert_eq!(second_offset, 5 * 65_536);
        assert_eq!(first_disk.range(first_offset, NODE_SIZE), node);
        assert_eq!(second_disk.range(second_offset, NODE_SIZE), node);

        let mut buffer = vec![0u8; NODE_SIZE];
        let clean = pointer.read_first_verified(resolve, &mut buffer, verifies).unwrap();
        assert_eq!(clean, ReplicaRead { copy_index: 0, failed: vec![] });

        first_disk.corrupt(first_offset + 100);
        buffer.fill(0);
        let repaired = pointer.read_first_verified(resolve, &mut buffer, verifies).unwrap();
        assert_eq!(
            repaired,
            ReplicaRead {
                copy_index: 1,
                failed: vec![pointer.copies()[0]]
            }
        );
        assert_eq!(buffer, node);

        second_disk.corrupt(second_offset + 100);
        assert!(matches!(
            pointer.read_first_verified(resolve, &mut buffer, verifies),
            Err(StorageError::NoReplicaVerified { replicas: 2 })
        ));
    }

    #[test]
    fn a_copy_on_an_unknown_disk_counts_as_failed() {
        let second_disk = MemoryDevice::new(1 << 20);
        let resolve = |disk_id: u16| -> Result<(&dyn BlockDevice, u8), StorageError> {
            match disk_id {
                2 => Ok((&second_disk, BUCKET_LOG2)),
                _ => Err(StorageError::UnknownDisk { disk_id }),
            }
        };
        let pointer = two_disk_pointer();
        let node = node_bytes();
        second_disk.write_at(pointer.copies()[1].byte_offset(BUCKET_LOG2), &node).unwrap();
        let mut buffer = vec![0u8; NODE_SIZE];
        let read = pointer.read_first_verified(resolve, &mut buffer, verifies).unwrap();
        assert_eq!(read.copy_index, 1);
        assert_eq!(read.failed, vec![pointer.copies()[0]]);
        assert!(matches!(
            pointer.write_all(resolve, &node),
            Err(StorageError::UnknownDisk { disk_id: 1 })
        ));
    }

    #[test]
    fn two_copy_pointer_encodes_to_fixed_bytes() {
        let pointer = two_disk_pointer();
        let expected: [u8; 32] = [
            0x01, 0x00, 0x02, 0x00, 0x03, 0x00, 0x00, 0x00, // disk 1, sector 2, bucket 3
            0x09, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // generation 9, reserved
            0x02, 0x00, 0x00, 0x00, 0x05, 0x00, 0x00, 0x00, // disk 2, sector 0, bucket 5
            0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // generation 4, reserved
        ];
        assert_eq!(pointer.encoded_len(), 32);
        assert_eq!(pointer.to_bytes(), expected);
        let mut encoded = [0u8; 32];
        pointer.encode_into(&mut encoded).unwrap();
        assert_eq!(encoded, expected);
        assert_eq!(NodePtr::decode(&expected, 2).unwrap(), pointer);
    }

    #[test]
    fn decode_rejects_bad_counts_lengths_reserved_words_and_duplicate_disks() {
        let encoded = two_disk_pointer().to_bytes();
        for replicas in [0, 5] {
            assert!(matches!(
                NodePtr::decode(&encoded, replicas),
                Err(StorageError::InvalidReplicaCount { replicas: rejected }) if rejected == replicas
            ));
        }
        assert!(matches!(
            NodePtr::decode(&encoded, 1),
            Err(StorageError::NodePtrLengthMismatch { expected: 16, actual: 32 })
        ));
        assert!(matches!(
            NodePtr::decode(&encoded[..31], 2),
            Err(StorageError::NodePtrLengthMismatch { expected: 32, actual: 31 })
        ));
        let mut reserved_set = encoded.clone();
        reserved_set[12] = 1;
        assert!(matches!(
            NodePtr::decode(&reserved_set, 2),
            Err(StorageError::BlockRefReservedNonZero { reserved: 1 })
        ));
        let mut same_disk = encoded.clone();
        same_disk[16] = 0x01;
        assert!(matches!(
            NodePtr::decode(&same_disk, 2),
            Err(StorageError::DuplicateReplicaDisk { disk_id: 1 })
        ));
        let mut short = [0u8; 16];
        assert!(matches!(
            two_disk_pointer().encode_into(&mut short),
            Err(StorageError::NodePtrLengthMismatch { expected: 32, actual: 16 })
        ));
    }

    #[test]
    fn new_rejects_empty_and_oversized_copy_lists() {
        assert!(matches!(NodePtr::new(&[]), Err(StorageError::InvalidReplicaCount { replicas: 0 })));
        let five: Vec<BlockRef> = (0..5).map(|disk_id| BlockRef::new(disk_id, 1, 0, 1)).collect();
        assert!(matches!(NodePtr::new(&five), Err(StorageError::InvalidReplicaCount { replicas: 5 })));
        let four = NodePtr::new(&five[..4]).unwrap();
        assert_eq!(four.replicas(), 4);
        assert_eq!(four.encoded_len(), 64);
    }
}
