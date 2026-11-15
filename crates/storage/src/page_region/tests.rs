use {
    super::*,
    std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

/// An in-memory device that counts the bytes written to it.
struct RecordingDevice {
    bytes: Mutex<Vec<u8>>,
    bytes_written: AtomicUsize,
}

impl RecordingDevice {
    fn new(capacity: usize) -> Self {
        Self {
            bytes: Mutex::new(vec![0; capacity]),
            bytes_written: AtomicUsize::new(0),
        }
    }

    fn bytes_written(&self) -> usize {
        self.bytes_written.load(Ordering::SeqCst)
    }

    fn snapshot(&self, offset: usize, len: usize) -> Vec<u8> {
        self.bytes.lock().unwrap()[offset..offset + len].to_vec()
    }

    fn flip(&self, offset: usize) {
        self.bytes.lock().unwrap()[offset] ^= 0xff;
    }
}

impl BlockDevice for RecordingDevice {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), StorageError> {
        let bytes = self.bytes.lock().unwrap();
        buf.copy_from_slice(&bytes[offset as usize..offset as usize + buf.len()]);
        Ok(())
    }

    fn write_at(&self, offset: u64, buf: &[u8]) -> Result<(), StorageError> {
        self.bytes_written.fetch_add(buf.len(), Ordering::SeqCst);
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

fn params(node_seq: u64) -> RegionParams {
    RegionParams {
        kind: BtreeKind::ObjectTable,
        level: 0,
        region_size_log2: REGION_LOG2,
        node_seq,
        created_lsn: node_seq * 10,
    }
}

fn page_filled_with(byte: u8) -> Page {
    let mut page = zeroed_page();
    bytemuck::cast_slice_mut::<u64, u8>(page.as_mut_slice()).fill(byte);
    page
}

fn first_byte(page: &Page) -> u8 {
    bytemuck::cast_slice::<u64, u8>(page.as_slice())[0]
}

#[test]
fn a_256_kib_region_holds_48_pages() {
    assert_eq!(PageRegion::default_logical_pages(18), 48);
}

#[test]
fn changing_one_record_of_a_full_leaf_writes_8_kib() {
    let device = RecordingDevice::new(REGION_BYTES);
    let mut region = PageRegion::create(&device, 0, params(1), 48, true, &[]).unwrap();
    let pages: Vec<Page> = (0..48).map(|page_no| page_filled_with(page_no as u8)).collect();
    let all: Vec<(u16, &Page)> = pages.iter().enumerate().map(|(page_no, page)| (page_no as u16, page)).collect();
    region.write_pages(&device, 1, &all).unwrap();

    let before = device.bytes_written();
    let changed = page_filled_with(0xee);
    region.write_pages(&device, 2, &[(17, &changed)]).unwrap();
    assert_eq!(device.bytes_written() - before, 8 * 1024, "one index block and one page");

    let reopened = PageRegion::open(&device, 0, BtreeKind::ObjectTable).unwrap();
    assert_eq!(reopened.run_count(), 2);
    assert_eq!(first_byte(&reopened.read_page(&device, 17).unwrap()), 0xee);
    assert_eq!(first_byte(&reopened.read_page(&device, 16).unwrap()), 16);
}

#[test]
fn a_never_written_page_reads_as_zeros() {
    let device = RecordingDevice::new(REGION_BYTES);
    let mut region = PageRegion::create(&device, 0, params(1), 48, true, &[]).unwrap();
    region.write_pages(&device, 1, &[(3, &page_filled_with(7))]).unwrap();
    let reopened = PageRegion::open(&device, 0, BtreeKind::ObjectTable).unwrap();
    assert!(reopened.read_page(&device, 4).unwrap().iter().all(|&word| word == 0));
    assert_eq!(first_byte(&reopened.read_page(&device, 3).unwrap()), 7);
    assert!(matches!(
        reopened.read_page(&device, 48),
        Err(StorageError::RegionCorrupt(_))
    ));
}

#[test]
fn a_chain_of_three_regions_appends_without_rewriting_the_first_two() {
    let device = RecordingDevice::new(REGION_BYTES * 3);
    let offsets = [0u64, REGION_BYTES as u64, 2 * REGION_BYTES as u64];
    let mut first = PageRegion::create(&device, offsets[0], params(1), 48, true, &[]).unwrap();
    first.write_pages(&device, 1, &[(0, &page_filled_with(1))]).unwrap();
    let first_bytes = device.snapshot(0, REGION_BYTES);

    // Each later region names its predecessor in its kind header.
    let mut second =
        PageRegion::create(&device, offsets[1], params(2), 48, false, &offsets[0].to_le_bytes()).unwrap();
    second.write_pages(&device, 2, &[(0, &page_filled_with(2))]).unwrap();
    let second_bytes = device.snapshot(REGION_BYTES, REGION_BYTES);

    let mut third =
        PageRegion::create(&device, offsets[2], params(3), 48, false, &offsets[1].to_le_bytes()).unwrap();
    third.write_pages(&device, 3, &[(0, &page_filled_with(3))]).unwrap();

    assert_eq!(device.snapshot(0, REGION_BYTES), first_bytes);
    assert_eq!(device.snapshot(REGION_BYTES, REGION_BYTES), second_bytes);

    // Walk back from the newest region to the head.
    let mut offset = offsets[2];
    let mut contents = Vec::new();
    loop {
        let region = PageRegion::open(&device, offset, BtreeKind::ObjectTable).unwrap();
        contents.push(first_byte(&region.read_page(&device, 0).unwrap()));
        if region.is_head_of_chain() {
            break;
        }
        offset = u64::from_le_bytes(region.kind_header().try_into().unwrap());
    }
    assert_eq!(contents, vec![3, 2, 1]);
}

#[test]
fn a_torn_page_run_is_dropped_and_its_pages_read_their_previous_copy() {
    let device = RecordingDevice::new(REGION_BYTES);
    let mut region = PageRegion::create(&device, 0, params(1), 48, true, &[]).unwrap();
    region.write_pages(&device, 1, &[(5, &page_filled_with(1))]).unwrap();
    let torn_index_block = region.next_block();
    region.write_pages(&device, 2, &[(5, &page_filled_with(2)), (6, &page_filled_with(2))]).unwrap();
    device.flip((region.next_block() as usize - 1) * BLOCK_SIZE + 9);

    let reopened = PageRegion::open(&device, 0, BtreeKind::ObjectTable).unwrap();
    assert_eq!(reopened.run_count(), 1);
    assert_eq!(reopened.next_block(), torn_index_block);
    assert_eq!(first_byte(&reopened.read_page(&device, 5).unwrap()), 1);
    assert!(reopened.read_page(&device, 6).unwrap().iter().all(|&word| word == 0));
}

#[test]
fn a_page_run_left_by_an_earlier_region_is_not_read() {
    let device = RecordingDevice::new(REGION_BYTES);
    let mut earlier = PageRegion::create(&device, 0, params(1), 48, true, &[]).unwrap();
    earlier.write_pages(&device, 1, &[(0, &page_filled_with(1))]).unwrap();
    earlier.write_pages(&device, 2, &[(1, &page_filled_with(2))]).unwrap();
    // A new region over the same space, nothing zeroed; its first run has the
    // same shape as the earlier first run.
    let mut later = PageRegion::create(&device, 0, params(2), 48, true, &[]).unwrap();
    later.write_pages(&device, 3, &[(0, &page_filled_with(9))]).unwrap();
    let reopened = PageRegion::open(&device, 0, BtreeKind::ObjectTable).unwrap();
    assert_eq!(reopened.run_count(), 1);
    assert!(reopened.read_page(&device, 1).unwrap().iter().all(|&word| word == 0));
}

#[test]
fn a_full_region_compacts_into_a_fresh_one_with_the_newest_pages() {
    let device = RecordingDevice::new(REGION_BYTES * 2);
    let mut region = PageRegion::create(&device, 0, params(1), 48, true, &[7, 7]).unwrap();
    let mut round = 0u8;
    loop {
        round += 1;
        let page = page_filled_with(round);
        match region.write_pages(&device, round as u64, &[((round % 4) as u16, &page)]) {
            Ok(()) => continue,
            Err(StorageError::RegionFull { .. }) => break,
            Err(other) => panic!("unexpected error {other:?}"),
        }
    }
    let fresh = region.compact_into(&device, REGION_BYTES as u64, params(2)).unwrap();
    assert_eq!(fresh.run_count(), 1);
    assert_eq!(fresh.kind_header(), &[7, 7]);
    assert!(fresh.is_head_of_chain());
    for page_no in 0..4u16 {
        assert_eq!(
            first_byte(&fresh.read_page(&device, page_no).unwrap()),
            first_byte(&region.read_page(&device, page_no).unwrap()),
        );
    }
    assert_eq!(fresh.next_block(), 1 + 1 + 4, "one run of the four live pages");
}

#[test]
fn a_keyed_region_does_not_open_as_positional() {
    let device = RecordingDevice::new(REGION_BYTES);
    crate::region::Region::create(&device, 0, params(1), &[]).unwrap();
    assert!(matches!(
        PageRegion::open(&device, 0, BtreeKind::ObjectTable),
        Err(StorageError::RegionCorrupt(_))
    ));
}

#[test]
fn page_run_header_fixed_bytes() {
    let device = RecordingDevice::new(REGION_BYTES);
    let mut region = PageRegion::create(&device, 0, params(1), 48, true, &[]).unwrap();
    let page = page_filled_with(0xab);
    region.write_pages(&device, 0x0102, &[(9, &page)]).unwrap();
    let index = device.snapshot(BLOCK_SIZE, BLOCK_SIZE);
    let mut expected = vec![b'B', b'P', b'G', b'S', 1, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 0x02, 0x01, 0, 0, 0, 0, 0, 0];
    expected.extend_from_slice(&region.region_tag().to_le_bytes());
    expected.extend_from_slice(&[0, 0, 0, 0]);
    assert_eq!(&index[..32], expected.as_slice());
    let mut zeroed = index.clone();
    zeroed[32..40].fill(0);
    assert_eq!(&index[32..40], &checksum64(&zeroed).to_le_bytes());
    // Page entry: page 9, then the page's checksum.
    assert_eq!(&index[40..48], &[9, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(&index[48..56], &checksum64(&device.snapshot(2 * BLOCK_SIZE, BLOCK_SIZE)).to_le_bytes());
}
