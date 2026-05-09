//! Tests for the generic B+ tree machinery (R1a-core).

use std::sync::Mutex;

use {serde::{Deserialize, Serialize}, smallvec::SmallVec};

use super::*;
use crate::{
    block::{BLOCK_SIZE, BtreeKind},
    block_device::BlockDevice,
    btree_node::{
        BtreeNodeHeader, SORTED_RUN_FLAG_PACKED_KEYS, SORTED_RUN_MAGIC, SortedRunHeader,
    },
    error::StorageError,
};

// ---------- Test BlockDevice that records every write ----------

/// In-memory block device that records the byte-range of every write so we
/// can assert "only the header sector and the new run sectors got touched"
/// invariants.
struct MemoryBlockDevice {
    inner: Mutex<MemoryInner>,
    capacity: u64,
}

struct MemoryInner {
    bytes: Vec<u8>,
    /// Each entry is (offset, length).
    writes: Vec<(u64, u64)>,
}

impl MemoryBlockDevice {
    fn new(capacity: u64) -> Self {
        Self {
            inner: Mutex::new(MemoryInner {
                bytes: vec![0u8; capacity as usize],
                writes: Vec::new(),
            }),
            capacity,
        }
    }

    fn writes(&self) -> Vec<(u64, u64)> {
        self.inner.lock().unwrap().writes.clone()
    }

    fn clear_writes(&self) {
        self.inner.lock().unwrap().writes.clear();
    }

    fn snapshot(&self) -> Vec<u8> {
        self.inner.lock().unwrap().bytes.clone()
    }

    /// Force-flip a byte in the underlying storage (test corruption).
    fn corrupt(&self, offset: u64, value: u8) {
        let mut inner = self.inner.lock().unwrap();
        inner.bytes[offset as usize] = value;
    }
}

impl BlockDevice for MemoryBlockDevice {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), StorageError> {
        let inner = self.inner.lock().unwrap();
        let end = offset as usize + buf.len();
        if end > inner.bytes.len() {
            return Err(StorageError::OutOfBounds {
                offset,
                length: buf.len() as u64,
                capacity: self.capacity,
            });
        }
        buf.copy_from_slice(&inner.bytes[offset as usize..end]);
        Ok(())
    }

    fn write_at(&self, offset: u64, buf: &[u8]) -> Result<(), StorageError> {
        let mut inner = self.inner.lock().unwrap();
        let end = offset as usize + buf.len();
        if end > inner.bytes.len() {
            return Err(StorageError::OutOfBounds {
                offset,
                length: buf.len() as u64,
                capacity: self.capacity,
            });
        }
        inner.bytes[offset as usize..end].copy_from_slice(buf);
        inner.writes.push((offset, buf.len() as u64));
        Ok(())
    }

    fn capacity(&self) -> u64 {
        self.capacity
    }

    fn sync(&self) -> Result<(), StorageError> {
        Ok(())
    }
}

// Helper: a small region (16 KiB = 4 sectors) for fast tests. log2(16384) =
// 14.
const TEST_REGION_LOG2: u8 = 14;
const TEST_REGION_SIZE: u64 = 1 << 14;

fn fresh_node<K: Ord + Clone, V: Clone>(kind: BtreeKind) -> LoadedNode<K, V> {
    LoadedNode::new(kind, 0, TEST_REGION_LOG2)
}

// ---------- SortedRun tests ----------

#[test]
fn sorted_run_lookup_finds_inserted_keys() {
    let run = SortedRun::from_sorted(0, 0, vec![(1u32, 10u32), (3, 30), (5, 50)]);
    assert_eq!(run.lookup(&1), Some(&10));
    assert_eq!(run.lookup(&3), Some(&30));
    assert_eq!(run.lookup(&5), Some(&50));
}

#[test]
fn sorted_run_lookup_returns_none_for_missing() {
    let run = SortedRun::from_sorted(0, 0, vec![(1u32, 10u32), (3, 30)]);
    assert_eq!(run.lookup(&2), None);
    assert_eq!(run.lookup(&100), None);
}

#[test]
fn sorted_run_range_returns_in_bounds_entries() {
    let run = SortedRun::from_sorted(
        0,
        0,
        (0u32..10).map(|i| (i, i * 10)).collect::<Vec<_>>(),
    );
    let collected: Vec<_> = run.range(&3, &7).map(|(k, v)| (*k, *v)).collect();
    assert_eq!(collected, vec![(3, 30), (4, 40), (5, 50), (6, 60)]);
}

#[test]
fn sorted_run_iter_in_order() {
    let run = SortedRun::from_sorted(0, 0, vec![(1u32, 10u32), (2, 20), (3, 30)]);
    let collected: Vec<_> = run.iter().map(|(k, v)| (*k, *v)).collect();
    assert_eq!(collected, vec![(1, 10), (2, 20), (3, 30)]);
}

#[test]
fn sorted_run_empty_is_empty() {
    let run: SortedRun<u32, u32> = SortedRun::new(0, 0);
    assert!(run.is_empty());
    assert_eq!(run.len(), 0);
}

// ---------- LoadedNode lookup / overlay tests ----------

#[test]
fn loaded_node_empty_lookup_returns_none() {
    let node: LoadedNode<u32, u32> = fresh_node(BtreeKind::Forward);
    assert_eq!(node.lookup(&1), None);
}

#[test]
fn loaded_node_lookup_walks_newest_run_first() {
    let mut node: LoadedNode<u32, u32> = fresh_node(BtreeKind::Forward);
    // Older run says key 1 -> 10.
    node.sorted_runs
        .push(SortedRun::from_sorted(0, 0, vec![(1, 10)]));
    // Newer run says key 1 -> 999 (override).
    node.sorted_runs
        .push(SortedRun::from_sorted(1, 0, vec![(1, 999)]));
    assert_eq!(node.lookup(&1), Some(999));
}

#[test]
fn loaded_node_lookup_honours_pending_journal_override() {
    let mut node: LoadedNode<u32, u32> = fresh_node(BtreeKind::Forward);
    node.sorted_runs
        .push(SortedRun::from_sorted(0, 0, vec![(1, 10)]));
    node.insert(5, 1, 4242);
    assert_eq!(node.lookup(&1), Some(4242));
}

#[test]
fn loaded_node_lookup_honours_whiteout() {
    let mut node: LoadedNode<u32, u32> = fresh_node(BtreeKind::Forward);
    node.sorted_runs
        .push(SortedRun::from_sorted(0, 0, vec![(1, 10), (2, 20)]));
    node.push_journal(JournalEntry {
        lsn: 7,
        op: JournalOp::Whiteout(1u32),
    });
    assert_eq!(node.lookup(&1), None);
    // Other keys still visible.
    assert_eq!(node.lookup(&2), Some(20));
}

#[test]
fn loaded_node_lookup_honours_remove() {
    let mut node: LoadedNode<u32, u32> = fresh_node(BtreeKind::Forward);
    node.sorted_runs
        .push(SortedRun::from_sorted(0, 0, vec![(1, 10)]));
    node.remove(7, 1);
    assert_eq!(node.lookup(&1), None);
}

#[test]
fn loaded_node_lookup_caches_merged_view() {
    let mut node: LoadedNode<u32, u32> = fresh_node(BtreeKind::Forward);
    node.sorted_runs
        .push(SortedRun::from_sorted(0, 0, vec![(1, 10)]));
    // First lookup populates cache.
    assert_eq!(node.lookup(&1), Some(10));
    // Cache stays valid; second lookup hits cache.
    assert_eq!(node.lookup(&1), Some(10));
    // Mutation invalidates.
    node.insert(1, 1, 99);
    assert_eq!(node.lookup(&1), Some(99));
}

// ---------- merge_iter tests ----------

#[test]
fn merge_iter_produces_sorted_output_across_runs() {
    let mut node: LoadedNode<u32, u32> = fresh_node(BtreeKind::Forward);
    node.sorted_runs
        .push(SortedRun::from_sorted(0, 0, vec![(1, 10), (3, 30), (5, 50)]));
    node.sorted_runs
        .push(SortedRun::from_sorted(1, 0, vec![(2, 20), (4, 40)]));
    let collected: Vec<_> = node.merge_iter().map(|(k, v)| (*k, *v)).collect();
    assert_eq!(
        collected,
        vec![(1, 10), (2, 20), (3, 30), (4, 40), (5, 50)]
    );
}

#[test]
fn merge_iter_suppresses_whiteouts() {
    let mut node: LoadedNode<u32, u32> = fresh_node(BtreeKind::Forward);
    node.sorted_runs
        .push(SortedRun::from_sorted(0, 0, vec![(1, 10), (2, 20)]));
    node.push_journal(JournalEntry {
        lsn: 5,
        op: JournalOp::Whiteout(1),
    });
    let collected: Vec<_> = node.merge_iter().map(|(k, v)| (*k, *v)).collect();
    assert_eq!(collected, vec![(2, 20)]);
}

#[test]
fn merge_iter_pending_overrides_runs() {
    let mut node: LoadedNode<u32, u32> = fresh_node(BtreeKind::Forward);
    node.sorted_runs
        .push(SortedRun::from_sorted(0, 0, vec![(1, 10), (2, 20)]));
    node.insert(5, 1, 999);
    let collected: Vec<_> = node.merge_iter().map(|(k, v)| (*k, *v)).collect();
    assert_eq!(collected, vec![(1, 999), (2, 20)]);
}

#[test]
fn merge_iter_within_pending_newest_lsn_wins() {
    let mut node: LoadedNode<u32, u32> = fresh_node(BtreeKind::Forward);
    node.insert(1, 5, 100);
    node.insert(2, 5, 200);
    node.insert(3, 5, 300);
    let v = node.lookup(&5);
    assert_eq!(v, Some(300));
}

// ---------- BtreeRegion: write_full / read round-trip ----------

#[test]
fn region_write_full_then_read_round_trip() {
    let device = MemoryBlockDevice::new(TEST_REGION_SIZE);
    let mut node: LoadedNode<u32, u32> = fresh_node(BtreeKind::Forward);
    node.sorted_runs
        .push(SortedRun::from_sorted(0, 5, vec![(1, 10), (3, 30)]));
    node.sorted_runs
        .push(SortedRun::from_sorted(1, 9, vec![(2, 20)]));
    BtreeRegion::write_full(&device, 0, &mut node).unwrap();

    let loaded: LoadedNode<u32, u32> =
        BtreeRegion::read(&device, 0, BtreeKind::Forward).unwrap();
    assert_eq!(loaded.sorted_runs.len(), 2);
    assert_eq!(loaded.sorted_runs[0].entries, vec![(1, 10), (3, 30)]);
    assert_eq!(loaded.sorted_runs[1].entries, vec![(2, 20)]);
    assert_eq!({ loaded.header.sorted_run_count }, 2);
    // last_persisted_lsn was not updated by write_full directly (caller's
    // job); but the runs carry their own journal_seq.
    assert_eq!(loaded.sorted_runs[1].journal_seq, 9);
}

#[test]
fn region_kind_mismatch_on_read_errors() {
    let device = MemoryBlockDevice::new(TEST_REGION_SIZE);
    let mut node: LoadedNode<u32, u32> = fresh_node(BtreeKind::Forward);
    node.sorted_runs
        .push(SortedRun::from_sorted(0, 0, vec![(1, 10)]));
    BtreeRegion::write_full(&device, 0, &mut node).unwrap();

    let err =
        BtreeRegion::read::<_, u32, u32>(&device, 0, BtreeKind::Backpointer).unwrap_err();
    assert!(matches!(err, StorageError::InvalidBtreeKind(_)));
}

// ---------- BtreeRegion: append_sorted_run ----------

#[test]
fn region_append_adds_run_and_only_touches_header_and_new_sectors() {
    let device = MemoryBlockDevice::new(TEST_REGION_SIZE);
    let mut node: LoadedNode<u32, u32> = fresh_node(BtreeKind::Forward);
    node.sorted_runs
        .push(SortedRun::from_sorted(0, 1, vec![(1, 10), (2, 20)]));
    BtreeRegion::write_full(&device, 0, &mut node).unwrap();

    // Snapshot post-full-write state.
    let bytes_before = device.snapshot();
    let payload_used_before = { node.header.payload_used };

    device.clear_writes();

    let new_run = SortedRun::from_sorted(1, 7, vec![(3u32, 30u32), (4, 40)]);
    let mut header_copy = node.header;
    BtreeRegion::append_sorted_run(&device, 0, &mut header_copy, &new_run).unwrap();

    // Header reflects the new run.
    assert_eq!({ header_copy.sorted_run_count }, 2);
    assert!({ header_copy.payload_used } > payload_used_before);
    assert_eq!({ header_copy.last_persisted_lsn }, 7);

    // Inspect writes: every write must be either:
    //  (a) within sector 0 (the header sector), OR
    //  (b) at offset >= BLOCK_SIZE + payload_used_before (the new run zone).
    let writes = device.writes();
    let new_run_zone_start = BLOCK_SIZE as u64 + payload_used_before as u64;
    for (offset, length) in &writes {
        let in_header_sector = *offset < BLOCK_SIZE as u64
            && *offset + *length <= BLOCK_SIZE as u64;
        let in_new_run_zone = *offset >= new_run_zone_start;
        assert!(
            in_header_sector || in_new_run_zone,
            "append_sorted_run wrote at offset={offset} length={length}, \
             outside header sector 0 and new-run zone (>= {new_run_zone_start})",
        );
    }

    // Sectors strictly between sector 0 and the new-run zone are bit-identical
    // before vs after.
    let bytes_after = device.snapshot();
    let preserved_start = BLOCK_SIZE;
    let preserved_end = new_run_zone_start as usize;
    assert_eq!(
        &bytes_before[preserved_start..preserved_end],
        &bytes_after[preserved_start..preserved_end],
        "old sorted-run bytes were modified during append_sorted_run",
    );

    // Read back: 2 runs visible.
    let loaded: LoadedNode<u32, u32> =
        BtreeRegion::read(&device, 0, BtreeKind::Forward).unwrap();
    assert_eq!(loaded.sorted_runs.len(), 2);
    assert_eq!(loaded.sorted_runs[0].entries, vec![(1, 10), (2, 20)]);
    assert_eq!(loaded.sorted_runs[1].entries, vec![(3, 30), (4, 40)]);
}

#[test]
fn region_append_only_overwrites_header_sector_byte_range() {
    let device = MemoryBlockDevice::new(TEST_REGION_SIZE);
    let mut node: LoadedNode<u32, u32> = fresh_node(BtreeKind::Forward);
    node.sorted_runs
        .push(SortedRun::from_sorted(0, 0, vec![(1, 10)]));
    BtreeRegion::write_full(&device, 0, &mut node).unwrap();

    // Read sector 1 (the run) before append.
    let mut sector1_before = vec![0u8; BLOCK_SIZE];
    device.read_at(BLOCK_SIZE as u64, &mut sector1_before).unwrap();

    let new_run = SortedRun::from_sorted(1, 5, vec![(2u32, 20u32)]);
    let mut header_copy = node.header;
    BtreeRegion::append_sorted_run(&device, 0, &mut header_copy, &new_run).unwrap();

    // Sector 1 unchanged.
    let mut sector1_after = vec![0u8; BLOCK_SIZE];
    device.read_at(BLOCK_SIZE as u64, &mut sector1_after).unwrap();
    assert_eq!(sector1_before, sector1_after, "sector 1 changed under append-only growth");
}

// ---------- CRC handling on read ----------

#[test]
fn read_torn_last_run_is_dropped_no_error() {
    let device = MemoryBlockDevice::new(TEST_REGION_SIZE);
    let mut node: LoadedNode<u32, u32> = fresh_node(BtreeKind::Forward);
    node.sorted_runs
        .push(SortedRun::from_sorted(0, 0, vec![(1, 10)]));
    BtreeRegion::write_full(&device, 0, &mut node).unwrap();
    // Append a second run.
    let new_run = SortedRun::from_sorted(1, 0, vec![(2u32, 20u32)]);
    let mut header_copy = node.header;
    BtreeRegion::append_sorted_run(&device, 0, &mut header_copy, &new_run).unwrap();

    // Corrupt a payload byte of the *second* (last) run. The second run
    // starts at BLOCK_SIZE + first run's footprint. The first run's payload
    // is small (~30 bytes CBOR), so 2 KiB into the region is well past the
    // first run's payload — but might still hit the next sector if first run
    // is sector-aligned past that. To be safe, corrupt at the very end of
    // the region's known-written area.
    let payload_used_after = { header_copy.payload_used };
    let corrupt_offset =
        BLOCK_SIZE as u64 + payload_used_after as u64 - 1; // last byte of the new run zone
    // Find a byte within the second run's payload that we can flip without
    // accidentally landing in tail-pad zeroes. Walk back until we hit a
    // non-zero byte (the CBOR contains key/value bytes).
    let mut probe = corrupt_offset;
    let snapshot = device.snapshot();
    while probe > BLOCK_SIZE as u64 + 32 && snapshot[probe as usize] == 0 {
        probe -= 1;
    }
    device.corrupt(probe, snapshot[probe as usize] ^ 0xFF);

    // Read the region back: first run intact, second run dropped.
    let loaded: LoadedNode<u32, u32> =
        BtreeRegion::read(&device, 0, BtreeKind::Forward).unwrap();
    assert_eq!(loaded.sorted_runs.len(), 1);
    assert_eq!(loaded.sorted_runs[0].entries, vec![(1, 10)]);
}

#[test]
fn read_torn_first_run_drops_all_runs() {
    let device = MemoryBlockDevice::new(TEST_REGION_SIZE);
    let mut node: LoadedNode<u32, u32> = fresh_node(BtreeKind::Forward);
    node.sorted_runs
        .push(SortedRun::from_sorted(0, 0, vec![(1, 10), (2, 20)]));
    node.sorted_runs
        .push(SortedRun::from_sorted(1, 0, vec![(3, 30)]));
    BtreeRegion::write_full(&device, 0, &mut node).unwrap();

    // Corrupt a byte inside the first run's payload. First run header is at
    // BLOCK_SIZE; payload starts at BLOCK_SIZE + 32.
    let snapshot = device.snapshot();
    let probe_start = BLOCK_SIZE as u64 + 32;
    // Find a non-zero byte to flip.
    let mut probe = probe_start;
    while snapshot[probe as usize] == 0 {
        probe += 1;
    }
    device.corrupt(probe, snapshot[probe as usize] ^ 0xFF);

    let loaded: LoadedNode<u32, u32> =
        BtreeRegion::read(&device, 0, BtreeKind::Forward).unwrap();
    // First run torn -> parsing stops there. Subsequent runs (even if intact)
    // are dropped because the parser walks sequentially.
    assert_eq!(loaded.sorted_runs.len(), 0);
}

#[test]
fn read_torn_run_magic_drops_remainder() {
    let device = MemoryBlockDevice::new(TEST_REGION_SIZE);
    let mut node: LoadedNode<u32, u32> = fresh_node(BtreeKind::Forward);
    node.sorted_runs
        .push(SortedRun::from_sorted(0, 0, vec![(1, 10)]));
    BtreeRegion::write_full(&device, 0, &mut node).unwrap();
    // Append a second run.
    let new_run = SortedRun::from_sorted(1, 0, vec![(2u32, 20u32)]);
    let mut header_copy = node.header;
    BtreeRegion::append_sorted_run(&device, 0, &mut header_copy, &new_run).unwrap();

    // Corrupt the magic of the second run header. Second run header starts at
    // sector boundary after first run.
    let region_after = device.snapshot();
    // Locate the second run header by scanning sector boundaries for "BSET" magic.
    let mut found_bset_offsets = Vec::new();
    for sector in 1..(TEST_REGION_SIZE / BLOCK_SIZE as u64) {
        let off = sector * BLOCK_SIZE as u64;
        if off as usize + 4 > region_after.len() {
            break;
        }
        if &region_after[off as usize..off as usize + 4] == b"BSET" {
            found_bset_offsets.push(off);
        }
    }
    assert!(found_bset_offsets.len() >= 2, "expected at least two BSET magics");
    let second_run_offset = found_bset_offsets[1];
    device.corrupt(second_run_offset, b'X');

    let loaded: LoadedNode<u32, u32> =
        BtreeRegion::read(&device, 0, BtreeKind::Forward).unwrap();
    assert_eq!(loaded.sorted_runs.len(), 1);
    assert_eq!(loaded.sorted_runs[0].entries, vec![(1, 10)]);
}

// ---------- Compaction policy & execution ----------

#[test]
fn should_compact_triggers_on_payload_used_threshold() {
    let mut header = BtreeNodeHeader::new(BtreeKind::Forward, 1, 0, TEST_REGION_LOG2);
    header.payload_used = ((TEST_REGION_SIZE * 76) / 100) as u32;
    header.sorted_run_count = 1;
    assert!(should_compact(&header, TEST_REGION_SIZE as u32));

    header.payload_used = ((TEST_REGION_SIZE * 50) / 100) as u32;
    assert!(!should_compact(&header, TEST_REGION_SIZE as u32));
}

#[test]
fn should_compact_triggers_on_sorted_run_count() {
    let mut header = BtreeNodeHeader::new(BtreeKind::Forward, 1, 0, TEST_REGION_LOG2);
    header.payload_used = 0;
    header.sorted_run_count = 5;
    assert!(should_compact(&header, TEST_REGION_SIZE as u32));

    header.sorted_run_count = 4;
    assert!(!should_compact(&header, TEST_REGION_SIZE as u32));
}

#[test]
fn compact_produces_one_sorted_run() {
    let mut node: LoadedNode<u32, u32> = fresh_node(BtreeKind::Forward);
    node.sorted_runs
        .push(SortedRun::from_sorted(0, 0, vec![(1, 10), (3, 30)]));
    node.sorted_runs
        .push(SortedRun::from_sorted(1, 0, vec![(2, 20), (4, 40)]));
    node.insert(7, 5, 50);
    let result = compact(&node);
    assert_eq!(result.sorted_runs.len(), 1);
    assert_eq!(
        result.sorted_runs[0].entries,
        vec![(1, 10), (2, 20), (3, 30), (4, 40), (5, 50)],
    );
    assert!(result.pending_journal.is_empty());
}

#[test]
fn compact_drops_whiteouts() {
    let mut node: LoadedNode<u32, u32> = fresh_node(BtreeKind::Forward);
    node.sorted_runs
        .push(SortedRun::from_sorted(0, 0, vec![(1, 10), (2, 20)]));
    node.push_journal(JournalEntry {
        lsn: 5,
        op: JournalOp::Whiteout(1),
    });
    let result = compact(&node);
    assert_eq!(result.sorted_runs.len(), 1);
    assert_eq!(result.sorted_runs[0].entries, vec![(2, 20)]);
}

#[test]
fn compact_empty_node_yields_zero_runs() {
    let node: LoadedNode<u32, u32> = fresh_node(BtreeKind::Forward);
    let result = compact(&node);
    assert_eq!(result.sorted_runs.len(), 0);
    assert_eq!({ result.header.sorted_run_count }, 0);
}

#[test]
fn compact_preserves_max_lsn() {
    let mut node: LoadedNode<u32, u32> = fresh_node(BtreeKind::Forward);
    node.sorted_runs
        .push(SortedRun::from_sorted(0, 100, vec![(1, 10)]));
    node.insert(250, 2, 20);
    node.insert(150, 3, 30);
    let result = compact(&node);
    assert_eq!(result.sorted_runs[0].journal_seq, 250);
    assert_eq!({ result.header.last_persisted_lsn }, 250);
}

// ---------- flush_to_run ----------

#[test]
fn flush_to_run_creates_new_run_from_pending() {
    let mut node: LoadedNode<u32, u32> = fresh_node(BtreeKind::Forward);
    node.insert(5, 1, 10);
    node.insert(7, 2, 20);
    node.flush_to_run().unwrap();
    assert_eq!(node.sorted_runs.len(), 1);
    assert_eq!(node.sorted_runs[0].entries, vec![(1, 10), (2, 20)]);
    assert_eq!(node.sorted_runs[0].journal_seq, 7);
    assert!(node.pending_journal.is_empty());
}

#[test]
fn flush_to_run_rejects_tombstones_in_r1a_core() {
    let mut node: LoadedNode<u32, u32> = fresh_node(BtreeKind::Forward);
    node.sorted_runs
        .push(SortedRun::from_sorted(0, 0, vec![(1, 10)]));
    node.remove(5, 1);
    let err = node.flush_to_run().unwrap_err();
    assert!(matches!(err, StorageError::CborEncode(_)));
}

// ---------- Generic over types ----------

#[test]
fn round_trip_string_and_vec_bytes() {
    let device = MemoryBlockDevice::new(TEST_REGION_SIZE);
    let mut node: LoadedNode<String, Vec<u8>> = fresh_node(BtreeKind::Forward);
    node.sorted_runs.push(SortedRun::from_sorted(
        0,
        0,
        vec![
            ("alpha".to_string(), vec![1, 2, 3]),
            ("beta".to_string(), vec![4, 5]),
            ("gamma".to_string(), vec![6]),
        ],
    ));
    BtreeRegion::write_full(&device, 0, &mut node).unwrap();
    let loaded: LoadedNode<String, Vec<u8>> =
        BtreeRegion::read(&device, 0, BtreeKind::Forward).unwrap();
    assert_eq!(loaded.sorted_runs.len(), 1);
    assert_eq!(
        loaded.sorted_runs[0].entries,
        vec![
            ("alpha".to_string(), vec![1, 2, 3]),
            ("beta".to_string(), vec![4, 5]),
            ("gamma".to_string(), vec![6]),
        ],
    );
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
struct CompositeKey {
    primary: u64,
    secondary: u32,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct CompositeValue {
    flags: u8,
    blob: Vec<u8>,
}

#[test]
fn round_trip_custom_struct_key_value() {
    let device = MemoryBlockDevice::new(TEST_REGION_SIZE);
    let mut node: LoadedNode<CompositeKey, CompositeValue> =
        fresh_node(BtreeKind::Forward);
    node.sorted_runs.push(SortedRun::from_sorted(
        0,
        0,
        vec![
            (
                CompositeKey { primary: 1, secondary: 0 },
                CompositeValue { flags: 1, blob: vec![1, 2, 3] },
            ),
            (
                CompositeKey { primary: 1, secondary: 1 },
                CompositeValue { flags: 2, blob: vec![] },
            ),
            (
                CompositeKey { primary: 2, secondary: 0 },
                CompositeValue { flags: 0, blob: vec![9, 9] },
            ),
        ],
    ));
    BtreeRegion::write_full(&device, 0, &mut node).unwrap();
    let loaded: LoadedNode<CompositeKey, CompositeValue> =
        BtreeRegion::read(&device, 0, BtreeKind::Forward).unwrap();
    assert_eq!(loaded.sorted_runs.len(), 1);
    let v = loaded.lookup(&CompositeKey { primary: 1, secondary: 1 });
    assert_eq!(v, Some(CompositeValue { flags: 2, blob: vec![] }));
    assert_eq!(loaded.lookup(&CompositeKey { primary: 99, secondary: 0 }), None);
}

#[test]
fn round_trip_u64_u32_pairs() {
    let device = MemoryBlockDevice::new(TEST_REGION_SIZE);
    let mut node: LoadedNode<u64, u32> = fresh_node(BtreeKind::BucketAlloc);
    node.sorted_runs.push(SortedRun::from_sorted(
        0,
        0,
        (0u64..50).map(|i| (i, i as u32 * 3)).collect::<Vec<_>>(),
    ));
    BtreeRegion::write_full(&device, 0, &mut node).unwrap();
    let loaded: LoadedNode<u64, u32> =
        BtreeRegion::read(&device, 0, BtreeKind::BucketAlloc).unwrap();
    assert_eq!(loaded.sorted_runs[0].entries.len(), 50);
    assert_eq!(loaded.lookup(&25), Some(75));
}

// ---------- Range query on LoadedNode ----------

#[test]
fn loaded_node_range_returns_sorted_in_bounds() {
    let mut node: LoadedNode<u32, u32> = fresh_node(BtreeKind::Forward);
    node.sorted_runs
        .push(SortedRun::from_sorted(0, 0, vec![(1, 10), (3, 30), (5, 50)]));
    node.sorted_runs
        .push(SortedRun::from_sorted(1, 0, vec![(2, 20), (4, 40)]));
    let r = node.range(&2, &5);
    assert_eq!(r, vec![(2, 20), (3, 30), (4, 40)]);
}

// ---------- PACKED_KEYS rejection ----------

#[test]
fn read_rejects_packed_keys_flag() {
    let device = MemoryBlockDevice::new(TEST_REGION_SIZE);
    // Hand-craft a region where a sorted run advertises PACKED_KEYS.
    let mut header = BtreeNodeHeader::new(BtreeKind::Forward, 1, 0, TEST_REGION_LOG2);
    header.sorted_run_count = 1;
    header.payload_used = BLOCK_SIZE as u32;
    let mut header_sector = vec![0u8; BLOCK_SIZE];
    header_sector[..std::mem::size_of::<BtreeNodeHeader>()]
        .copy_from_slice(header.as_bytes());
    device.write_at(0, &header_sector).unwrap();

    // Sorted run header with PACKED_KEYS flag.
    let mut run_header = SortedRunHeader {
        magic: SORTED_RUN_MAGIC,
        seq: 0,
        journal_seq: 0,
        entry_count: 0,
        payload_length: 0,
        flags: SORTED_RUN_FLAG_PACKED_KEYS,
        crc: 0,
    };
    run_header.crc =
        SortedRunHeader::compute_crc(bytemuck::bytes_of(&run_header), &[]);
    device
        .write_at(BLOCK_SIZE as u64, bytemuck::bytes_of(&run_header))
        .unwrap();

    let err = BtreeRegion::read::<_, u32, u32>(&device, 0, BtreeKind::Forward).unwrap_err();
    assert!(matches!(err, StorageError::UnsupportedFormatVersion(_)));
}

// ---------- Header round-trip via append_sorted_run preserves seq monotonicity ----------

#[test]
fn append_sorted_run_bumps_seq_monotonically() {
    let device = MemoryBlockDevice::new(TEST_REGION_SIZE);
    let mut node: LoadedNode<u32, u32> = fresh_node(BtreeKind::Forward);
    node.sorted_runs
        .push(SortedRun::from_sorted(0, 0, vec![(1, 10)]));
    BtreeRegion::write_full(&device, 0, &mut node).unwrap();
    let seq_after_full = { node.header.seq };

    let mut header_copy = node.header;
    let new_run = SortedRun::from_sorted(1, 0, vec![(2u32, 20u32)]);
    BtreeRegion::append_sorted_run(&device, 0, &mut header_copy, &new_run).unwrap();
    assert!({ header_copy.seq } > seq_after_full);
}

// ---------- SmallVec usage compiles ----------

#[test]
fn loaded_node_uses_smallvec_for_runs() {
    let node: LoadedNode<u32, u32> = fresh_node(BtreeKind::Forward);
    let runs: &SmallVec<[SortedRun<u32, u32>; 4]> = &node.sorted_runs;
    assert!(runs.is_empty());
}

// ---------- Packed-key region-level round-trips (R1a-pack) ----------

mod packed_region {
    use super::*;
    use crate::btree::pack;

    /// Build a `LoadedNode` of `(u64, u32)` keys -> 4-byte values and persist
    /// it via `write_full_packed`, then reload via `read_packed`. The values
    /// share a 2-byte common prefix that the codec elides; the descriptor
    /// carries the prefix bytes so the reader reconstructs values without
    /// any caller-supplied template.
    #[test]
    fn write_full_packed_then_read_round_trip() {
        let device = MemoryBlockDevice::new(TEST_REGION_SIZE);

        let entries: Vec<((u64, u32), Vec<u8>)> = (0u64..20)
            .map(|i| ((i, 7u32), vec![0xAA, 0xBB, (i & 0xff) as u8, 0]))
            .collect();
        let mut node: LoadedNode<(u64, u32), Vec<u8>> = fresh_node(BtreeKind::Forward);
        node.sorted_runs
            .push(SortedRun::from_sorted(0, 0, entries.clone()));

        BtreeRegion::write_full_packed(&device, 0, &mut node, 4).unwrap();

        let reloaded: LoadedNode<(u64, u32), Vec<u8>> =
            BtreeRegion::read_packed(&device, 0, BtreeKind::Forward, 4).unwrap();
        assert_eq!(reloaded.sorted_runs.len(), 1);
        let run = &reloaded.sorted_runs[0];
        assert_eq!(run.entries.len(), entries.len());
        for (orig, dec) in entries.iter().zip(run.entries.iter()) {
            assert_eq!(orig.0, dec.0);
            assert_eq!(orig.1, dec.1);
        }
        // Flag is set on the reloaded run.
        assert!(run.flags & SORTED_RUN_FLAG_PACKED_KEYS != 0);
    }

    /// Two packed sorted runs in one region with **different** value
    /// prefixes: write_full_packed handles multiple runs, and read_packed
    /// reconstructs each entry from its own descriptor's prefix slot
    /// without any caller-supplied template.
    #[test]
    fn write_full_packed_multiple_runs() {
        let device = MemoryBlockDevice::new(TEST_REGION_SIZE);
        let entries_a: Vec<(u64, Vec<u8>)> = (0u64..5).map(|i| (i, vec![1, 2, 3, 4])).collect();
        let entries_b: Vec<(u64, Vec<u8>)> =
            (10u64..15).map(|i| (i, vec![5, 6, 7, 8])).collect();
        let mut node: LoadedNode<u64, Vec<u8>> = fresh_node(BtreeKind::Forward);
        node.sorted_runs
            .push(SortedRun::from_sorted(0, 0, entries_a.clone()));
        node.sorted_runs
            .push(SortedRun::from_sorted(1, 0, entries_b.clone()));

        BtreeRegion::write_full_packed(&device, 0, &mut node, 4).unwrap();

        let header_sector = {
            let mut buf = vec![0u8; BLOCK_SIZE];
            device.read_at(0, &mut buf).unwrap();
            buf
        };
        let header = BtreeNodeHeader::parse(&header_sector).unwrap();
        assert_eq!({ header.sorted_run_count }, 2);

        let reloaded: LoadedNode<u64, Vec<u8>> =
            BtreeRegion::read_packed(&device, 0, BtreeKind::Forward, 4).unwrap();
        assert_eq!(reloaded.sorted_runs.len(), 2);
        assert_eq!(reloaded.sorted_runs[0].entries, entries_a);
        assert_eq!(reloaded.sorted_runs[1].entries, entries_b);
    }

    /// `append_sorted_run_packed` mutates the header sector while leaving
    /// existing runs intact.
    #[test]
    fn append_sorted_run_packed_round_trip() {
        let device = MemoryBlockDevice::new(TEST_REGION_SIZE);

        let entries_a: Vec<(u64, Vec<u8>)> = (0u64..3).map(|i| (i, vec![1, 2, 3, 4])).collect();
        let mut node: LoadedNode<u64, Vec<u8>> = fresh_node(BtreeKind::Forward);
        node.sorted_runs
            .push(SortedRun::from_sorted(0, 0, entries_a.clone()));
        BtreeRegion::write_full_packed(&device, 0, &mut node, 4).unwrap();

        // Append a second run with the same prefix.
        let entries_b: Vec<(u64, Vec<u8>)> = (10u64..13).map(|i| (i, vec![1, 2, 7, 8])).collect();
        let new_run = SortedRun::from_sorted(1, 0, entries_b);
        let mut header_copy = node.header;
        BtreeRegion::append_sorted_run_packed(&device, 0, &mut header_copy, &new_run, 4)
            .unwrap();
        assert_eq!({ header_copy.sorted_run_count }, 2);
    }

    /// Packed-keys flag is set on packed runs; CBOR runs leave it clear.
    #[test]
    fn packed_keys_flag_round_trip() {
        let device = MemoryBlockDevice::new(TEST_REGION_SIZE);

        let entries: Vec<(u64, Vec<u8>)> = (0u64..3).map(|i| (i, vec![1, 2, 3, 4])).collect();
        let mut node: LoadedNode<u64, Vec<u8>> = fresh_node(BtreeKind::Forward);
        node.sorted_runs
            .push(SortedRun::from_sorted(0, 0, entries));
        BtreeRegion::write_full_packed(&device, 0, &mut node, 4).unwrap();

        // Read the run header directly and verify the flag.
        let mut run_header_buf = [0u8; std::mem::size_of::<SortedRunHeader>()];
        device.read_at(BLOCK_SIZE as u64, &mut run_header_buf).unwrap();
        let run_header: SortedRunHeader = *bytemuck::from_bytes(&run_header_buf);
        assert!(({ run_header.flags } & SORTED_RUN_FLAG_PACKED_KEYS) != 0);
    }

    /// Mixed encoding: a CBOR run plus a packed run in the same region.
    /// `read_packed` dispatches on the per-run flag.
    #[test]
    fn mixed_cbor_and_packed_runs() {
        let device = MemoryBlockDevice::new(TEST_REGION_SIZE);

        // Step 1: write a CBOR run via write_full.
        let cbor_entries: Vec<(u64, Vec<u8>)> =
            (0u64..3).map(|i| (i, vec![9, 8, 7, 6])).collect();
        let mut node: LoadedNode<u64, Vec<u8>> = fresh_node(BtreeKind::Forward);
        node.sorted_runs
            .push(SortedRun::from_sorted(0, 0, cbor_entries.clone()));
        BtreeRegion::write_full(&device, 0, &mut node).unwrap();

        // Step 2: append a packed run via append_sorted_run_packed.
        let packed_entries: Vec<(u64, Vec<u8>)> = (10u64..13)
            .map(|i| (i, vec![9, 8, (i & 0xff) as u8, 0]))
            .collect();
        let new_run = SortedRun::from_sorted(1, 0, packed_entries.clone());
        let mut header_copy = node.header;
        BtreeRegion::append_sorted_run_packed(&device, 0, &mut header_copy, &new_run, 4)
            .unwrap();

        // Step 3: read with read_packed. The CBOR run carries its own
        // payload bytes that decode_packed would fail on — but read_packed
        // dispatches on flag.
        // Note: V::from(Vec<u8>) is required for packed-decoded values; for
        // CBOR-encoded values, ciborium decodes directly to V. We use
        // Vec<u8> as V here, where Vec<u8>: From<Vec<u8>> is the identity.
        let reloaded: LoadedNode<u64, Vec<u8>> =
            BtreeRegion::read_packed(&device, 0, BtreeKind::Forward, 4).unwrap();
        assert_eq!(reloaded.sorted_runs.len(), 2);
        // Run 0 = CBOR; run 1 = packed.
        let run0 = &reloaded.sorted_runs[0];
        assert_eq!(run0.flags & SORTED_RUN_FLAG_PACKED_KEYS, 0);
        assert_eq!(run0.entries, cbor_entries);

        let run1 = &reloaded.sorted_runs[1];
        assert!(run1.flags & SORTED_RUN_FLAG_PACKED_KEYS != 0);
        assert_eq!(run1.entries.len(), packed_entries.len());
        for (orig, dec) in packed_entries.iter().zip(run1.entries.iter()) {
            assert_eq!(orig.0, dec.0);
            assert_eq!(orig.1, dec.1);
        }
    }

    /// Sanity: the FormatPromoteEvent type carries the new format and is
    /// constructible from the storage layer.
    #[test]
    fn format_promote_event_constructible() {
        let entries: Vec<(u64, Vec<u8>)> = vec![(0u64, vec![]), (255u64, vec![])];
        let (head, fields, _prefix) = pack::select_format(&entries).unwrap();
        let evt = pack::FormatPromoteEvent {
            sorted_run_seq: 7,
            new_format: head,
            new_fields: fields,
        };
        assert_eq!(evt.sorted_run_seq, 7);
        assert_eq!({ evt.new_format.nr_fields }, 1);
    }
}
