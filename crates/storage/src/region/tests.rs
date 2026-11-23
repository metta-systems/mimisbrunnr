use {
    super::*,
    std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

/// An in-memory device that counts reads.
struct CountingDevice {
    bytes: Mutex<Vec<u8>>,
    reads: AtomicUsize,
}

impl CountingDevice {
    fn new(capacity: usize) -> Self {
        Self {
            bytes: Mutex::new(vec![0; capacity]),
            reads: AtomicUsize::new(0),
        }
    }

    fn reads(&self) -> usize {
        self.reads.load(Ordering::SeqCst)
    }

    fn bytes_at(&self, offset: usize, len: usize) -> Vec<u8> {
        self.bytes.lock().unwrap()[offset..offset + len].to_vec()
    }

    fn flip(&self, offset: usize) {
        self.bytes.lock().unwrap()[offset] ^= 0xff;
    }

    fn patch(&self, offset: usize, data: &[u8]) {
        self.bytes.lock().unwrap()[offset..offset + data.len()].copy_from_slice(data);
    }
}

impl BlockDevice for CountingDevice {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), StorageError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        let bytes = self.bytes.lock().unwrap();
        buf.copy_from_slice(&bytes[offset as usize..offset as usize + buf.len()]);
        Ok(())
    }

    fn write_at(&self, offset: u64, buf: &[u8]) -> Result<(), StorageError> {
        self.bytes.lock().unwrap()[offset as usize..offset as usize + buf.len()].copy_from_slice(buf);
        Ok(())
    }

    fn capacity(&self) -> u64 {
        self.bytes.lock().unwrap().len() as u64
    }

    fn sync(&self) -> Result<(), StorageError> {
        Ok(())
    }
}

const REGION_LOG2: u8 = 18;
const REGION_BYTES: usize = 1 << REGION_LOG2;

fn params(kind: BtreeKind) -> RegionParams {
    RegionParams {
        kind,
        level: 0,
        region_size_log2: REGION_LOG2,
        node_seq: 1,
        created_lsn: 2,
    }
}

fn fresh(kind: BtreeKind) -> (CountingDevice, Region) {
    let device = CountingDevice::new(REGION_BYTES * 2);
    let region = Region::create(&device, 0, params(kind), &[]).unwrap();
    (device, region)
}

fn values(entries: &[(u32, Vec<u8>)]) -> Vec<(u32, RunEntry<'_>)> {
    entries.iter().map(|(key, value)| (*key, RunEntry::Value(value))).collect()
}

#[test]
fn header_block_fixed_bytes() {
    let (device, _region) = fresh(BtreeKind::Postings);
    let block = device.bytes_at(0, BLOCK_SIZE);
    let mut expected = vec![
        b'B', b'R', b'E', b'G', 0x01, 0x00, 0x08, 0x00, // preamble: BREG, version 1, Postings
        0x01, 0, 0, 0, 0, 0, 0, 0, // node_seq
        0x02, 0, 0, 0, 0, 0, 0, 0, // created_lsn
        18, 0, LAYOUT_RUNS, 0, // region_size_log2, level, layout, flags
        0, 0, 0, 0, // logical_pages, kind_header_len
    ];
    expected.extend_from_slice(&[0; 24]);
    assert_eq!(&block[..56], expected.as_slice());
    let mut zeroed = block.clone();
    zeroed[56..64].fill(0);
    assert_eq!(&block[56..64], &checksum64(&zeroed).to_le_bytes());
    assert!(block[64..].iter().all(|&byte| byte == 0));
}

#[test]
fn index_and_data_block_fixed_bytes() {
    let (device, mut region) = fresh(BtreeKind::BucketAlloc);
    let entries = vec![(10u32, vec![1u8; 8]), (11, vec![2; 8]), (12, vec![3; 8])];
    region.append_run(&device, 7, &values(&entries)).unwrap();

    let index = device.bytes_at(BLOCK_SIZE, BLOCK_SIZE);
    let data = device.bytes_at(2 * BLOCK_SIZE, BLOCK_SIZE);

    // Data block: 3 entries, keys 10..12 as 1-byte deltas from base 10,
    // fixed 8-byte values from offset align8(8 + 3 + 3) = 16.
    let mut expected_data = vec![3, 0, 16, 0, 0, 0, 0, 0, 0, 1, 2, 0, 0, 0, 0, 0];
    expected_data.extend_from_slice(&[1; 8]);
    expected_data.extend_from_slice(&[2; 8]);
    expected_data.extend_from_slice(&[3; 8]);
    assert_eq!(&data[..40], expected_data.as_slice());
    assert!(data[40..].iter().all(|&byte| byte == 0));

    let mut expected_index = vec![
        b'B', b'I', b'D', b'X', 0x01, 0x00, 0x00, 0x00, // preamble
        1, 0, 0, 0, // seq
        1, 0, 0, 0, // block_count, flags
        7, 0, 0, 0, 0, 0, 0, 0, // journal_seq
        3, 0, 0, 0, // entry_count
    ];
    // region_tag: the low 32 bits of block 0's checksum.
    let block_zero = device.bytes_at(0, BLOCK_SIZE);
    expected_index.extend_from_slice(&block_zero[56..60]);
    assert_eq!(region.region_tag().to_le_bytes(), block_zero[56..60]);
    let header_checksum = &index[32..40];
    expected_index.extend_from_slice(header_checksum);
    expected_index.extend_from_slice(&[1, 1, VALUE_KIND_FIXED, 0, 8, 0, 0, 0]); // key format head
    expected_index.extend_from_slice(&[1, 0, 0, 0, 0, 0, 0, 0, 10, 0, 0, 0, 0, 0, 0, 0]); // field: width 1, base 10
    expected_index.extend_from_slice(&checksum64(&data).to_le_bytes()); // block entry
    expected_index.extend_from_slice(&[3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    expected_index.push(0); // fence key: 10 - base
    assert_eq!(&index[..expected_index.len()], expected_index.as_slice());
    let mut zeroed = index.clone();
    zeroed[32..40].fill(0);
    assert_eq!(header_checksum, &checksum64(&zeroed).to_le_bytes());
}

#[test]
fn lookup_reads_at_most_one_data_block_per_run() {
    let (device, mut region) = fresh(BtreeKind::Forward);
    for run in 0..3u32 {
        let entries: Vec<(u32, Vec<u8>)> =
            (0..2000).map(|index| (run * 10_000 + index, index.to_le_bytes().to_vec())).collect();
        region.append_run(&device, run as u64, &values(&entries)).unwrap();
    }
    let region = Region::open(&device, 0, BtreeKind::Forward).unwrap();
    assert_eq!(region.run_count(), 3);
    for key in [5u32, 15_000, 21_999, 99_999] {
        let before = device.reads();
        let found = region.lookup(&device, &key).unwrap();
        assert!(device.reads() - before <= 3, "key {key}: one data block per run at most");
        let expected = (key % 10_000 < 2000 && key < 30_000).then(|| (key % 10_000).to_le_bytes().to_vec());
        assert_eq!(found.map(|value| value.to_vec()), expected);
    }
    // A key in the newest run is found with one read.
    let before = device.reads();
    region.lookup(&device, &20_001u32).unwrap().unwrap();
    assert_eq!(device.reads() - before, 1);
}

#[test]
fn an_8_kib_value_is_read_8_byte_aligned() {
    let (device, mut region) = fresh(BtreeKind::Postings);
    let large: Vec<u8> = (0..8192u32).map(|index| (index * 7) as u8).collect();
    let entries = vec![(1u32, vec![9u8; 3]), (2, large.clone()), (3, vec![4u8; 20])];
    region.append_run(&device, 1, &values(&entries)).unwrap();
    let region = Region::open(&device, 0, BtreeKind::Postings).unwrap();
    let value = region.lookup(&device, &2u32).unwrap().unwrap();
    assert_eq!(value.as_bytes(), large.as_slice());
    assert_eq!(value.as_bytes().as_ptr() as usize % 8, 0);
    let small = region.lookup(&device, &3u32).unwrap().unwrap();
    assert_eq!(small.as_bytes(), &[4u8; 20]);
    assert_eq!(small.as_bytes().as_ptr() as usize % 8, 0);
    assert_eq!(region.entries::<_, u32>(&device).unwrap().len(), 3);
}

#[test]
fn a_run_torn_in_its_last_data_block_is_dropped() {
    let (device, mut region) = fresh(BtreeKind::Ontology);
    let first: Vec<(u32, Vec<u8>)> = (0..10).map(|key| (key, vec![key as u8; 16])).collect();
    region.append_run(&device, 1, &values(&first)).unwrap();
    let torn_run_index_block = region.next_block();
    let second: Vec<(u32, Vec<u8>)> = (0..600).map(|key| (key + 100, vec![1; 24])).collect();
    region.append_run(&device, 2, &values(&second)).unwrap();
    let last_block = region.next_block() - 1;
    device.flip(last_block as usize * BLOCK_SIZE + 100);

    let mut reopened = Region::open(&device, 0, BtreeKind::Ontology).unwrap();
    assert_eq!(reopened.run_count(), 1);
    assert_eq!(reopened.next_block(), torn_run_index_block);
    let read_back = reopened.entries::<_, u32>(&device).unwrap();
    assert_eq!(read_back, first);
    // The next run is written where the torn one was.
    reopened.append_run(&device, 3, &values(&[(500u32, vec![5; 8])])).unwrap();
    let again = Region::open(&device, 0, BtreeKind::Ontology).unwrap();
    assert_eq!(again.run_count(), 2);
    assert_eq!(again.lookup(&device, &500u32).unwrap().unwrap().to_vec(), vec![5; 8]);
}

#[test]
fn whiteouts_in_newer_runs_hide_older_values() {
    let (device, mut region) = fresh(BtreeKind::Forward);
    region.append_run(&device, 1, &values(&[(5u32, vec![1; 4]), (6, vec![2; 4])])).unwrap();
    region
        .append_run(&device, 2, &[(5u32, RunEntry::Whiteout), (7, RunEntry::Value(&[3; 4]))])
        .unwrap();
    let region = Region::open(&device, 0, BtreeKind::Forward).unwrap();
    assert!(region.lookup(&device, &5u32).unwrap().is_none());
    assert_eq!(
        region.entries::<_, u32>(&device).unwrap(),
        vec![(6, vec![2; 4]), (7, vec![3; 4])]
    );
}

#[test]
fn an_encrypted_run_fails_to_load() {
    let (device, mut region) = fresh(BtreeKind::Forward);
    region.append_run(&device, 1, &values(&[(1u32, vec![1; 8])])).unwrap();
    let mut index = device.bytes_at(BLOCK_SIZE, BLOCK_SIZE);
    index[14..16].copy_from_slice(&RUN_FLAG_ENCRYPTED.to_le_bytes());
    index[32..40].fill(0);
    let checksum = checksum64(&index);
    index[32..40].copy_from_slice(&checksum.to_le_bytes());
    device.patch(BLOCK_SIZE, &index);
    assert!(matches!(
        Region::open(&device, 0, BtreeKind::Forward),
        Err(StorageError::RunEncrypted)
    ));
}

#[test]
fn opening_as_another_tree_or_a_damaged_header_fails() {
    let (device, _region) = fresh(BtreeKind::Forward);
    assert!(matches!(
        Region::open(&device, 0, BtreeKind::Postings),
        Err(StorageError::RegionKindMismatch { expected: 8, actual: 4 })
    ));
    device.flip(20);
    assert!(matches!(
        Region::open(&device, 0, BtreeKind::Forward),
        Err(StorageError::ChecksumMismatch { .. })
    ));
}

#[test]
fn a_damaged_data_block_in_an_older_run_fails_its_lookup() {
    let (device, mut region) = fresh(BtreeKind::Forward);
    region.append_run(&device, 1, &values(&[(1u32, vec![1; 8])])).unwrap();
    region.append_run(&device, 2, &values(&[(2u32, vec![2; 8])])).unwrap();
    device.flip(2 * BLOCK_SIZE + 9);
    let region = Region::open(&device, 0, BtreeKind::Forward).unwrap();
    assert_eq!(region.run_count(), 2, "only the newest run is checked at load");
    assert!(matches!(
        region.lookup(&device, &1u32),
        Err(StorageError::ChecksumMismatch { .. })
    ));
}

#[test]
fn many_entries_split_into_runs_and_a_full_region_is_reported() {
    let (device, mut region) = fresh(BtreeKind::Backpointer);
    let entries: Vec<(u64, Vec<u8>)> = (0..20_000u64).map(|key| (key * 3, key.to_le_bytes().to_vec())).collect();
    let refs: Vec<(u64, RunEntry<'_>)> = entries.iter().map(|(key, value)| (*key, RunEntry::Value(value))).collect();
    region.append_entries(&device, 1, &refs).unwrap();
    let region = Region::open(&device, 0, BtreeKind::Backpointer).unwrap();
    assert_eq!(region.entries::<_, u64>(&device).unwrap(), entries);
    let mut full = region;
    let too_many: Vec<(u64, Vec<u8>)> = (0..100_000u64).map(|key| (key, vec![0; 8])).collect();
    let too_many_refs: Vec<(u64, RunEntry<'_>)> =
        too_many.iter().map(|(key, value)| (*key, RunEntry::Value(value))).collect();
    assert!(matches!(
        full.append_entries(&device, 2, &too_many_refs),
        Err(StorageError::RegionFull { .. })
    ));
}

#[test]
fn rewriting_in_place_never_reads_the_previous_runs() {
    let (device, mut region) = fresh(BtreeKind::BucketAlloc);
    // Two runs: the second's index block sits right after the first run.
    region.append_run(&device, 1, &values(&[(1u32, vec![1; 8])])).unwrap();
    region.append_run(&device, 2, &values(&[(2u32, vec![2; 8])])).unwrap();
    // Rewrite with one run of the same shape: its end is where run 2 began.
    let mut rewritten = Region::create_in_place(&device, 0, params(BtreeKind::BucketAlloc), &[]).unwrap();
    rewritten.append_run(&device, 3, &values(&[(9u32, vec![9; 8])])).unwrap();
    let reopened = Region::open(&device, 0, BtreeKind::BucketAlloc).unwrap();
    assert_eq!(reopened.run_count(), 1);
    assert_eq!(reopened.entries::<_, u32>(&device).unwrap(), vec![(9, vec![9; 8])]);
}

#[test]
fn a_run_left_by_an_earlier_region_is_not_read_as_this_regions() {
    // Reused space, nothing zeroed: an earlier region's second run sits
    // exactly where the new region's second run would start.
    let device = CountingDevice::new(REGION_BYTES);
    let mut earlier = Region::create(&device, 0, params(BtreeKind::Forward), &[]).unwrap();
    earlier.append_run(&device, 1, &values(&[(1u32, vec![1; 8])])).unwrap();
    earlier.append_run(&device, 2, &values(&[(2u32, vec![2; 8])])).unwrap();

    let later_params = RegionParams {
        node_seq: 2,
        created_lsn: 9,
        ..params(BtreeKind::Forward)
    };
    let mut later = Region::create(&device, 0, later_params, &[]).unwrap();
    assert_ne!(later.region_tag(), earlier.region_tag());
    later.append_run(&device, 3, &values(&[(9u32, vec![9; 8])])).unwrap();

    let reopened = Region::open(&device, 0, BtreeKind::Forward).unwrap();
    assert_eq!(reopened.run_count(), 1, "the earlier region's run 2 has another tag");
    assert_eq!(reopened.entries::<_, u32>(&device).unwrap(), vec![(9, vec![9; 8])]);
}

#[test]
fn a_value_larger_than_the_region_is_reported_as_a_full_region() {
    let (device, mut region) = fresh(BtreeKind::Ontology);
    let huge = vec![7u8; REGION_BYTES * 2];
    assert!(matches!(
        region.append_entries(&device, 1, &[(0u32, RunEntry::Value(&huge))]),
        Err(StorageError::RegionFull { .. })
    ));
}

#[test]
fn unsorted_entries_are_refused() {
    let (device, mut region) = fresh(BtreeKind::Forward);
    let result = region.append_run(&device, 1, &[(2u32, RunEntry::Whiteout), (1, RunEntry::Whiteout)]);
    assert!(matches!(result, Err(StorageError::UnsortedRunEntries)));
}

/// A 16-byte hash key: two big-endian fields, ordered as bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct HashKey([u8; 16]);

impl RegionKey for HashKey {
    const FIELDS: &'static [FieldKind] = &[FieldKind::BigEndianBytes, FieldKind::BigEndianBytes];
    fn field_values(&self, out: &mut [u64]) {
        out[0] = u64::from_be_bytes(self.0[..8].try_into().unwrap());
        out[1] = u64::from_be_bytes(self.0[8..].try_into().unwrap());
    }
    fn from_field_values(fields: &[u64]) -> Result<Self, StorageError> {
        let mut bytes = [0u8; 16];
        bytes[..8].copy_from_slice(&fields[0].to_be_bytes());
        bytes[8..].copy_from_slice(&fields[1].to_be_bytes());
        Ok(Self(bytes))
    }
}

/// A signed key, ordered as signed values.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct SignedKey(i64);

impl RegionKey for SignedKey {
    const FIELDS: &'static [FieldKind] = &[FieldKind::Signed];
    fn field_values(&self, out: &mut [u64]) {
        out[0] = self.0 as u64;
    }
    fn from_field_values(fields: &[u64]) -> Result<Self, StorageError> {
        Ok(Self(fields[0] as i64))
    }
}

#[test]
fn byte_and_signed_keys_round_trip_in_order() {
    let (device, mut region) = fresh(BtreeKind::ChunkIndex);
    let mut hashes: Vec<HashKey> = (0..500u32)
        .map(|seed| {
            let mut bytes = [0u8; 16];
            bytes[..8].copy_from_slice(&checksum64(&seed.to_le_bytes()).to_be_bytes());
            bytes[8..].copy_from_slice(&checksum64(&seed.to_be_bytes()).to_be_bytes());
            HashKey(bytes)
        })
        .collect();
    hashes.sort();
    let entries: Vec<(HashKey, RunEntry<'_>)> = hashes.iter().map(|hash| (*hash, RunEntry::Value(&hash.0))).collect();
    region.append_run(&device, 1, &entries).unwrap();
    for hash in &hashes {
        assert_eq!(region.lookup(&device, hash).unwrap().unwrap().as_bytes(), &hash.0);
    }

    let (device, mut region) = fresh(BtreeKind::Sequences);
    let signed = [-5_000_000_000i64, -3, 0, 7, 9_000_000_000];
    let entries: Vec<(SignedKey, RunEntry<'_>)> =
        signed.iter().map(|&value| (SignedKey(value), RunEntry::Value(&[1]))).collect();
    region.append_run(&device, 1, &entries).unwrap();
    let read_back: Vec<i64> = region
        .entries::<_, SignedKey>(&device)
        .unwrap()
        .into_iter()
        .map(|(key, _)| key.0)
        .collect();
    assert_eq!(read_back, signed);
    assert!(region.lookup(&device, &SignedKey(-4)).unwrap().is_none());
}

#[test]
fn reading_with_the_wrong_key_type_fails() {
    let (device, mut region) = fresh(BtreeKind::Forward);
    region.append_run(&device, 1, &values(&[(1u32, vec![1; 8])])).unwrap();
    assert!(matches!(
        region.lookup(&device, &(1u64, 0u32)),
        Err(StorageError::RegionCorrupt(_))
    ));
}
