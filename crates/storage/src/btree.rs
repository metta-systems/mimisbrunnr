//! Generic B+ tree machinery — the bcachefs-style sorted-run model.
//!
//! Implements IMPL §1.5.2–§1.5.5 in-memory machinery on top of the existing
//! [`BtreeNodeHeader`] / [`SortedRunHeader`] structs from [`crate::btree_node`].
//!
//! What this module provides:
//!
//! * [`SortedRun`] — an in-memory sorted run (analogue of bcachefs's `bset`).
//! * [`LoadedNode`] — the in-memory mirror of a 256 KiB region: a stack of
//!   sorted runs plus a pending journal overlay (IMPL §3.4) and a lazily
//!   materialised merged view.
//! * [`JournalEntry`] / [`JournalOp`] — the per-key overlay used until the
//!   next flush converts pending entries into a fresh sorted run.
//! * [`BtreeRegion`] — read / full-rewrite / append-only-grow path against a
//!   [`BlockDevice`].
//! * [`should_compact`] / [`compact`] — IMPL §1.5.4 trigger rule plus
//!   in-memory k-way merge that produces a single-run [`LoadedNode`].
//!
//! ## Out of scope (intentional for R1a-core)
//!
//! * Packed-key codec (IMPL §1.5.6, `SortedRunKeyFormat` driving) —
//!   `TODO(rewrite-phase-R1a-pack)`. Sorted-run payloads are CBOR for now;
//!   keys round-trip verbatim.
//! * COW / fresh-region allocation. The actual *allocator* call lives at the
//!   index / pool layer (R1b); this module only knows how to read and write a
//!   region at a caller-supplied byte offset.
//! * Auxiliary search trees (Eytzinger). Sorted-run lookup is a plain
//!   binary search.
//! * Journal-reclaim driver thread. We accept a `pending_journal` overlay as
//!   input to compaction but do not schedule flushes from a WAL fill watermark.
//! * `FormatPromote` WAL op — that is part of R1a-pack, when widths actually
//!   matter.
//! * `BTREE_NODE_FLAG_HEAD_OF_CHAIN` chunk-list semantics.
//! * `BTREE_NODE_FLAG_COMPACTION_IN_PROGRESS` recovery handling: the flag is
//!   set during full compaction and cleared after, but recovery action on a
//!   crashed compaction is `TODO(rewrite-phase-R1b)`.
//!
//! ## On-disk layout (R1a-core)
//!
//! ```text
//! offset 0                                     -> BtreeNodeHeader (64 B)
//!   the rest of sector 0 is unused padding (256 KiB region has 64 sectors of 4 KiB).
//! offset 4096 (sector 1)                       -> first SortedRunHeader (32 B) || payload
//! offset 4096 + ceil((32+payload0)/4096)*4096  -> next SortedRunHeader || payload
//! ...
//! ```
//!
//! Each sorted run is sector-aligned: a sorted run starts at a 4 KiB sector
//! boundary, so the new-run write does not overwrite any sector belonging to
//! an earlier run, and the append-only invariant is preserved at sector
//! granularity. See [`BtreeRegion::append_sorted_run`].
//!
//! Sorted-run payload encoding (R1a-core): **`ciborium` CBOR** of
//! `Vec<(K, V)>`, with `K, V: Serialize + DeserializeOwned`. R1a-pack adds a
//! parallel **packed-key codec** path (see the `pack` submodule and
//! [`BtreeRegion::write_full_packed`] / [`BtreeRegion::read_packed`]) that
//! sets the on-disk flag [`SORTED_RUN_FLAG_PACKED_KEYS`] on the per-run
//! header. The CBOR path remains the default for variable-shape values and
//! key types that don't implement [`PackableKey`].

pub mod pack;

use std::cell::RefCell;

use {
    itertools::Itertools,
    serde::{Serialize, de::DeserializeOwned},
    smallvec::SmallVec,
};

use crate::{
    block::{BLOCK_SIZE, BtreeKind},
    block_device::BlockDevice,
    btree_node::{
        BTREE_NODE_FLAG_COMPACTION_IN_PROGRESS, BtreeNodeHeader, SORTED_RUN_FLAG_PACKED_KEYS,
        SORTED_RUN_MAGIC, SortedRunHeader,
    },
    error::StorageError,
};

// ---------- SortedRun ----------

/// One sorted run within a [`LoadedNode`]. The on-disk analogue is the bytes
/// `SortedRunHeader || payload` per IMPL §1.5.1 (bcachefs `bset`).
///
/// Entries are kept in ascending order by `K`. Equal keys within a single run
/// are not produced by this crate's flush path (full compaction collapses
/// duplicates); however, [`Self::lookup`] tolerates duplicates by returning
/// the first match.
#[derive(Clone, Debug)]
pub struct SortedRun<K, V> {
    /// Monotonic within the region. Mirrors `SortedRunHeader.seq`.
    pub seq: u32,
    /// Newest WAL LSN merged into this run. Mirrors `SortedRunHeader.journal_seq`.
    pub journal_seq: u64,
    /// `SORTED_RUN_FLAG_*`. R1a-core only sets `0`.
    pub flags: u32,
    /// Sorted entries.
    pub entries: Vec<(K, V)>,
}

impl<K: Ord, V> SortedRun<K, V> {
    /// Build a fresh empty run with default flags.
    pub fn new(seq: u32, journal_seq: u64) -> Self {
        Self {
            seq,
            journal_seq,
            flags: 0,
            entries: Vec::new(),
        }
    }

    /// Build from already-sorted entries. Caller is responsible for sortedness.
    pub fn from_sorted(seq: u32, journal_seq: u64, entries: Vec<(K, V)>) -> Self {
        debug_assert!(
            entries.windows(2).all(|w| w[0].0 <= w[1].0),
            "SortedRun::from_sorted called with unsorted entries"
        );
        Self {
            seq,
            journal_seq,
            flags: 0,
            entries,
        }
    }

    /// Binary-search lookup of `key`.
    pub fn lookup(&self, key: &K) -> Option<&V> {
        match self.entries.binary_search_by(|(k, _)| k.cmp(key)) {
            Ok(idx) => Some(&self.entries[idx].1),
            Err(_) => None,
        }
    }

    /// Iterate `(k, v)` pairs whose key is in `[lo, hi)`.
    pub fn range<'a>(&'a self, lo: &'a K, hi: &'a K) -> impl Iterator<Item = (&'a K, &'a V)> + 'a {
        let lo_idx = self
            .entries
            .partition_point(|(k, _)| k < lo);
        let hi_idx = self
            .entries
            .partition_point(|(k, _)| k < hi);
        self.entries[lo_idx..hi_idx].iter().map(|(k, v)| (k, v))
    }

    /// Iterate every `(k, v)` in order.
    pub fn iter(&self) -> impl Iterator<Item = (&K, &V)> {
        self.entries.iter().map(|(k, v)| (k, v))
    }

    /// Number of entries in this run.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// `true` if this run has no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

// ---------- JournalEntry / JournalOp ----------

/// Per-key overlay carried on top of a [`LoadedNode`]'s sorted runs until the
/// next flush converts pending entries into a fresh sorted run. Mirrors the
/// pending-journal slice of IMPL §3.4's `DirtyNode`.
#[derive(Clone, Debug)]
pub struct JournalEntry<K, V> {
    /// WAL LSN of the underlying mutation. Used for `journal_seq` accounting
    /// when the entry is folded into a new sorted run.
    pub lsn: u64,
    pub op: JournalOp<K, V>,
}

/// Logical mutation kind in the pending-journal overlay. Whiteouts mask
/// inserts in older runs so that a `Remove` issued after a flush does not
/// resurrect on subsequent reads.
#[derive(Clone, Debug)]
pub enum JournalOp<K, V> {
    /// Insert or overwrite `(K, V)`.
    Insert(K, V),
    /// Remove the key. Equivalent to `Whiteout` for R1a-core; the type
    /// distinction exists for forward compatibility with snapshot-aware
    /// btrees in R6.
    Remove(K),
    /// bcachefs whiteout — overrides earlier inserts in older runs.
    Whiteout(K),
}

impl<K, V> JournalOp<K, V> {
    /// Borrow the key.
    pub fn key(&self) -> &K {
        match self {
            Self::Insert(k, _) => k,
            Self::Remove(k) | Self::Whiteout(k) => k,
        }
    }

    /// `true` for `Remove` / `Whiteout`.
    pub fn is_tombstone(&self) -> bool {
        matches!(self, Self::Remove(_) | Self::Whiteout(_))
    }
}

// ---------- LoadedNode ----------

/// In-memory mirror of a 256 KiB region, per IMPL §1.5.3.
///
/// Lookups consult sorted runs newest-first then check the pending-journal
/// overlay for an override. The lazy `merged_view` short-circuits subsequent
/// lookups; mutations clear it.
#[derive(Debug)]
pub struct LoadedNode<K, V> {
    pub header: BtreeNodeHeader,
    /// Newest run last (i.e. at index `len - 1`); index 0 is oldest. Lookup
    /// walks back-to-front.
    pub sorted_runs: SmallVec<[SortedRun<K, V>; 4]>,
    /// Lazy merged view for fast point lookup (sorted by key). Cleared on
    /// mutation. Built by [`LoadedNode::lookup`] on first call.
    merged_view: RefCell<Option<Vec<(K, V)>>>,
    /// Journal entries past `header.last_persisted_lsn`. Folded into a new
    /// sorted run on the next flush.
    pub pending_journal: Vec<JournalEntry<K, V>>,
    pub dirty: bool,
}

impl<K: Ord + Clone, V: Clone> LoadedNode<K, V> {
    /// Build a fresh empty node. The header's `level` and `region_size_log2`
    /// must come from the caller; `seq` and `payload_used` start at 0.
    pub fn new(kind: BtreeKind, level: u8, region_size_log2: u8) -> Self {
        let header = BtreeNodeHeader::new(kind, 1, level, region_size_log2);
        Self {
            header,
            sorted_runs: SmallVec::new(),
            merged_view: RefCell::new(None),
            pending_journal: Vec::new(),
            dirty: false,
        }
    }

    /// Drop the cached merged view (call after mutation).
    pub fn invalidate_cache(&mut self) {
        self.merged_view.borrow_mut().take();
    }

    /// Append a pending journal entry. Marks the node dirty and invalidates
    /// the cache.
    pub fn push_journal(&mut self, entry: JournalEntry<K, V>) {
        self.pending_journal.push(entry);
        self.dirty = true;
        self.invalidate_cache();
    }

    /// Convenience: queue an insert.
    pub fn insert(&mut self, lsn: u64, key: K, value: V) {
        self.push_journal(JournalEntry {
            lsn,
            op: JournalOp::Insert(key, value),
        });
    }

    /// Convenience: queue a remove.
    pub fn remove(&mut self, lsn: u64, key: K) {
        self.push_journal(JournalEntry {
            lsn,
            op: JournalOp::Remove(key),
        });
    }

    /// Look up `key`. Walks pending-journal newest-first, then sorted runs
    /// newest-first. A tombstone in any layer yields `None`.
    ///
    /// On first call, materialises the merged view (k-way merge across runs
    /// and pending journal); subsequent lookups hit the cache via binary
    /// search. The cache is dropped on any mutation.
    pub fn lookup(&self, key: &K) -> Option<V> {
        // Fast path: pending-journal newest-first override of any cached view.
        for entry in self.pending_journal.iter().rev() {
            match &entry.op {
                JournalOp::Insert(k, v) if k == key => return Some(v.clone()),
                JournalOp::Remove(k) | JournalOp::Whiteout(k) if k == key => return None,
                _ => {}
            }
        }
        // Cache hit?
        {
            let cache = self.merged_view.borrow();
            if let Some(view) = cache.as_ref() {
                return match view.binary_search_by(|(k, _)| k.cmp(key)) {
                    Ok(idx) => Some(view[idx].1.clone()),
                    Err(_) => None,
                };
            }
        }
        // Build cache: full merge view (without pending — we already
        // short-circuited above).
        let mut cache = self.merged_view.borrow_mut();
        let view: Vec<(K, V)> = self
            .merge_iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let result = match view.binary_search_by(|(k, _)| k.cmp(key)) {
            Ok(idx) => Some(view[idx].1.clone()),
            Err(_) => None,
        };
        *cache = Some(view);
        result
    }

    /// Range scan `[lo, hi)`. Honours whiteouts and pending-journal overrides.
    /// Yields owned `(K, V)` pairs in ascending key order; deduplicated so
    /// that the newest version of a key wins.
    pub fn range(&self, lo: &K, hi: &K) -> Vec<(K, V)> {
        self.merge_iter()
            .filter(|(k, _)| *k >= lo && *k < hi)
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    /// K-way merge across all sorted runs, with whiteout suppression and
    /// pending-journal overlay. Yields each key once with the newest live
    /// value. This is the iterator full compaction consumes.
    pub fn merge_iter(&self) -> MergeIter<'_, K, V> {
        MergeIter::new(self)
    }
}

impl<K: Ord + Clone, V: Clone> LoadedNode<K, V> {
    /// Convert the pending journal into a fresh sorted run and append it.
    /// The new run carries the highest LSN seen in `pending_journal` as
    /// `journal_seq`. Whiteouts in pending journal that have no matching
    /// older live key still surface here as zero-length entries — we filter
    /// those out (a whiteout against nothing is a no-op).
    ///
    /// Whiteouts that mask an entry in an older run are encoded as a
    /// `Remove`-style flag on the new run by simply *not emitting* the
    /// matching key — but doing so loses information when older runs still
    /// hold the key. To preserve correctness without packed-key tombstone
    /// encoding (R1a-pack), we instead keep whiteout semantics in the
    /// **in-memory** model only and require a full compaction to truly
    /// drop the key. For R1a-core therefore, `flush_to_run` rejects nodes
    /// whose pending journal contains tombstones; callers must invoke
    /// [`compact`] in that case.
    pub fn flush_to_run(&mut self) -> Result<(), StorageError> {
        if self.pending_journal.is_empty() {
            return Ok(());
        }
        // R1a-core limitation: tombstones require a full compaction so they
        // can be resolved against older runs. (R1a-pack will encode whiteouts
        // in the run itself — TODO(rewrite-phase-R1a-pack).)
        if self
            .pending_journal
            .iter()
            .any(|e| e.op.is_tombstone())
        {
            return Err(StorageError::CborEncode(
                "flush_to_run: tombstones require compact() in R1a-core".to_string(),
            ));
        }
        // Collect inserts; later inserts on the same key win.
        let mut by_key: std::collections::BTreeMap<K, V> = std::collections::BTreeMap::new();
        let mut max_lsn: u64 = 0;
        for entry in self.pending_journal.drain(..) {
            if entry.lsn > max_lsn {
                max_lsn = entry.lsn;
            }
            if let JournalOp::Insert(k, v) = entry.op {
                by_key.insert(k, v);
            }
        }
        let entries: Vec<(K, V)> = by_key.into_iter().collect();
        let next_seq = self
            .sorted_runs
            .iter()
            .map(|r| r.seq)
            .max()
            .map_or(0, |s| s + 1);
        let run = SortedRun::from_sorted(next_seq, max_lsn, entries);
        self.sorted_runs.push(run);
        self.header.sorted_run_count = self.sorted_runs.len() as u8;
        if max_lsn > { self.header.last_persisted_lsn } {
            self.header.last_persisted_lsn = max_lsn;
        }
        self.dirty = true;
        self.invalidate_cache();
        Ok(())
    }
}

/// K-way merge iterator over a [`LoadedNode`]'s sorted runs, layered with
/// pending-journal overrides.
pub struct MergeIter<'a, K, V> {
    items: std::vec::IntoIter<(&'a K, &'a V)>,
}

/// Layered (key, value, kind, layer, lsn) tuple used during k-way merge.
/// Layer numbers older→newer (sorted runs first, then pending journal).
type MergeRow<'a, K, V> = (&'a K, usize, JournalKind, Option<&'a V>, u64);

impl<'a, K: Ord + Clone, V: Clone> MergeIter<'a, K, V> {
    fn new(node: &'a LoadedNode<K, V>) -> Self {
        // Layers: 0..N = sorted_runs (oldest at 0, newest at N-1), layer N
        // = pending-journal overlay. Each sorted run is already key-sorted; we
        // pre-sort the pending-journal slice into the same shape, then k-way
        // merge by key (ties broken by layer ascending, then lsn ascending —
        // so the newest entry of each key-group lands last).
        let pending_layer = node.sorted_runs.len();

        // Pending-journal pre-sort.
        let mut pending: Vec<MergeRow<'a, K, V>> =
            Vec::with_capacity(node.pending_journal.len());
        for entry in &node.pending_journal {
            match &entry.op {
                JournalOp::Insert(k, v) => {
                    pending.push((k, pending_layer, JournalKind::Live, Some(v), entry.lsn));
                }
                JournalOp::Remove(k) | JournalOp::Whiteout(k) => {
                    pending.push((k, pending_layer, JournalKind::Tombstone, None, entry.lsn));
                }
            }
        }
        pending.sort_by(|a, b| a.0.cmp(b.0).then_with(|| a.4.cmp(&b.4)));

        // Per-run iterator (each already sorted by key). Use trait objects to
        // unify with the pending iterator under one `kmerge_by` call.
        let mut sources: Vec<Box<dyn Iterator<Item = MergeRow<'a, K, V>> + 'a>> =
            Vec::with_capacity(node.sorted_runs.len() + 1);
        for (layer, run) in node.sorted_runs.iter().enumerate() {
            sources.push(Box::new(
                run.iter()
                    .map(move |(k, v)| (k, layer, JournalKind::Live, Some(v), 0u64)),
            ));
        }
        sources.push(Box::new(pending.into_iter()));

        // k-way merge: ascending by (key, layer, lsn) so equal-keys land
        // older-first, with the newest as the last in each group.
        let merged = sources.into_iter().kmerge_by(
            |a: &MergeRow<'a, K, V>, b: &MergeRow<'a, K, V>| {
                a.0.cmp(b.0)
                    .then_with(|| a.1.cmp(&b.1))
                    .then_with(|| a.4.cmp(&b.4))
                    == std::cmp::Ordering::Less
            },
        );

        // Group-by-key (newest version wins); drop tombstones.
        let mut deduped: Vec<(&'a K, &'a V)> = Vec::new();
        let mut iter = merged.peekable();
        while let Some(first) = iter.next() {
            let key = first.0;
            let mut newest = first;
            while iter.peek().is_some_and(|next| next.0 == key) {
                newest = iter.next().unwrap();
            }
            if matches!(newest.2, JournalKind::Live)
                && let Some(v) = newest.3
            {
                deduped.push((newest.0, v));
            }
        }
        Self {
            items: deduped.into_iter(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum JournalKind {
    Live,
    Tombstone,
}

impl<'a, K, V> Iterator for MergeIter<'a, K, V> {
    type Item = (&'a K, &'a V);

    fn next(&mut self) -> Option<Self::Item> {
        self.items.next()
    }
}

// ---------- BtreeRegion serialisation ----------

/// Region-level reader / writer.
pub struct BtreeRegion;

impl BtreeRegion {
    /// Compute the size in bytes of the region described by `header`.
    pub fn region_size(header: &BtreeNodeHeader) -> u64 {
        let log2 = { header.region_size_log2 } as u64;
        1u64 << log2
    }

    /// Round up `value` to the next multiple of [`BLOCK_SIZE`].
    fn align_up_to_sector(value: u64) -> u64 {
        value.div_ceil(BLOCK_SIZE as u64) * BLOCK_SIZE as u64
    }

    /// Read a region starting at byte `offset` from `device`, parse its
    /// header and every sorted run.
    ///
    /// Per IMPL §1.5.1, a torn write of a partial sorted run fails its CRC
    /// and is discarded — earlier sorted runs remain valid. This function
    /// stops at the first corrupted run and returns the sorted runs that
    /// preceded it (logging a warning for the corrupt-and-trailing runs).
    pub fn read<D: BlockDevice, K, V>(
        device: &D,
        offset: u64,
        kind: BtreeKind,
    ) -> Result<LoadedNode<K, V>, StorageError>
    where
        K: DeserializeOwned + Ord + Clone,
        V: DeserializeOwned + Clone,
    {
        // 1. Read the header sector.
        let mut header_sector = vec![0u8; BLOCK_SIZE];
        device.read_at(offset, &mut header_sector)?;
        let header = BtreeNodeHeader::parse(&header_sector)?;
        // Validate kind matches.
        let header_kind = { header.pre.kind };
        if header_kind != kind as u16 {
            return Err(StorageError::InvalidBtreeKind(header_kind));
        }

        let region_size = Self::region_size(&header);
        let payload_used = { header.payload_used } as u64;
        if payload_used > region_size {
            return Err(StorageError::RegionFull {
                used: payload_used,
                size: region_size,
            });
        }

        // 2. Walk sorted runs from sector 1 onward. The first run starts at
        //    BLOCK_SIZE; subsequent runs are sector-aligned at
        //    `prev_run_end_aligned`.
        let mut sorted_runs: SmallVec<[SortedRun<K, V>; 4]> = SmallVec::new();
        let mut cursor: u64 = BLOCK_SIZE as u64;
        let header_payload_end = BLOCK_SIZE as u64 + payload_used;
        let expected_count = { header.sorted_run_count } as usize;
        let mut runs_read = 0usize;
        while cursor < header_payload_end && runs_read < expected_count {
            // Read run header.
            let mut run_header_buf = [0u8; std::mem::size_of::<SortedRunHeader>()];
            device.read_at(offset + cursor, &mut run_header_buf)?;
            let run_header: SortedRunHeader =
                *bytemuck::from_bytes(&run_header_buf);
            let magic = { run_header.magic };
            if magic != SORTED_RUN_MAGIC {
                log::warn!(
                    "btree region at offset {offset:#x}: torn sorted-run magic at \
                     cursor {cursor:#x} (expected {SORTED_RUN_MAGIC:#x}, got {magic:#x}); \
                     dropping this and trailing runs",
                );
                break;
            }
            let payload_length = { run_header.payload_length } as usize;
            let flags = { run_header.flags };
            if flags & SORTED_RUN_FLAG_PACKED_KEYS != 0 {
                // R1a-pack only. TODO(rewrite-phase-R1a-pack).
                return Err(StorageError::UnsupportedFormatVersion(0));
            }
            // Read payload.
            let mut payload = vec![0u8; payload_length];
            let payload_offset =
                offset + cursor + std::mem::size_of::<SortedRunHeader>() as u64;
            device.read_at(payload_offset, &mut payload)?;
            // Verify CRC.
            if let Err(e) = run_header.verify(&payload) {
                log::warn!(
                    "btree region at offset {offset:#x}: torn sorted-run CRC at \
                     cursor {cursor:#x}: {e}; dropping this and trailing runs",
                );
                break;
            }
            // Decode CBOR -> Vec<(K, V)>.
            let entries: Vec<(K, V)> = ciborium::de::from_reader(payload.as_slice())
                .map_err(|e| StorageError::CborDecode(e.to_string()))?;
            sorted_runs.push(SortedRun {
                seq: { run_header.seq },
                journal_seq: { run_header.journal_seq },
                flags,
                entries,
            });
            runs_read += 1;
            // Advance cursor to next sector boundary.
            let run_total = std::mem::size_of::<SortedRunHeader>() as u64 + payload_length as u64;
            cursor = Self::align_up_to_sector(cursor + run_total);
        }

        Ok(LoadedNode {
            header,
            sorted_runs,
            merged_view: RefCell::new(None),
            pending_journal: Vec::new(),
            dirty: false,
        })
    }

    /// Encode a sorted run as `SortedRunHeader || cbor_payload`. The header's
    /// `payload_length` and `crc` are populated; remaining fields come from
    /// `run`.
    fn encode_run<K, V>(run: &SortedRun<K, V>) -> Result<(SortedRunHeader, Vec<u8>), StorageError>
    where
        K: Serialize,
        V: Serialize,
    {
        // CBOR-encode entries.
        let entries: &[(K, V)] = &run.entries;
        let mut payload = Vec::new();
        ciborium::ser::into_writer(&entries, &mut payload)
            .map_err(|e| StorageError::CborEncode(e.to_string()))?;
        let mut header = SortedRunHeader {
            magic: SORTED_RUN_MAGIC,
            seq: run.seq,
            journal_seq: run.journal_seq,
            entry_count: run.entries.len() as u32,
            payload_length: payload.len() as u32,
            flags: run.flags,
            crc: 0,
        };
        let crc = SortedRunHeader::compute_crc(bytemuck::bytes_of(&header), &payload);
        header.crc = crc;
        Ok((header, payload))
    }

    /// Write a freshly built region. Used by full compaction and tree growth.
    /// This rewrites every sector — *not* the append-only path.
    ///
    /// Sectors are written sequentially: sector 0 holds the header, sector 1+
    /// hold the sorted runs, each starting at a sector boundary.
    pub fn write_full<D: BlockDevice, K, V>(
        device: &D,
        offset: u64,
        node: &mut LoadedNode<K, V>,
    ) -> Result<(), StorageError>
    where
        K: Serialize,
        V: Serialize,
    {
        let region_size = Self::region_size(&node.header);
        let mut cursor: u64 = BLOCK_SIZE as u64;

        for run in &node.sorted_runs {
            let (run_header, payload) = Self::encode_run(run)?;
            let run_total = std::mem::size_of::<SortedRunHeader>() as u64 + payload.len() as u64;
            if cursor + run_total > region_size {
                return Err(StorageError::RegionFull {
                    used: cursor + run_total,
                    size: region_size,
                });
            }
            device.write_at(offset + cursor, bytemuck::bytes_of(&run_header))?;
            device.write_at(
                offset + cursor + std::mem::size_of::<SortedRunHeader>() as u64,
                &payload,
            )?;
            cursor = Self::align_up_to_sector(cursor + run_total);
        }
        // payload_used is the high-water mark relative to the start of the
        // sorted-run area (i.e. cursor minus header sector).
        let payload_used = cursor - BLOCK_SIZE as u64;
        if payload_used > u32::MAX as u64 {
            return Err(StorageError::RegionFull {
                used: payload_used,
                size: region_size,
            });
        }
        node.header.payload_used = payload_used as u32;
        node.header.sorted_run_count = node.sorted_runs.len() as u8;
        node.header.seq = { node.header.seq } + 1;

        // Write header sector last (so a partial write doesn't reference
        // unwritten runs).
        let mut header_sector = vec![0u8; BLOCK_SIZE];
        header_sector[..std::mem::size_of::<BtreeNodeHeader>()]
            .copy_from_slice(node.header.as_bytes());
        device.write_at(offset, &header_sector)?;
        node.dirty = false;
        Ok(())
    }

    /// Append `new_run` at the existing region's end-of-payload, then rewrite
    /// **only** the header sector. Other sectors (and other sorted runs) are
    /// untouched.
    ///
    /// `existing_header` is mutated to reflect the appended run:
    /// `sorted_run_count`, `payload_used`, `last_persisted_lsn`, `seq` all
    /// updated.
    pub fn append_sorted_run<D: BlockDevice, K, V>(
        device: &D,
        offset: u64,
        existing_header: &mut BtreeNodeHeader,
        new_run: &SortedRun<K, V>,
    ) -> Result<(), StorageError>
    where
        K: Serialize,
        V: Serialize,
    {
        let region_size = Self::region_size(existing_header);
        let payload_used = { existing_header.payload_used } as u64;
        let cursor = BLOCK_SIZE as u64 + payload_used;
        // Sorted runs always start at sector boundary; payload_used is
        // maintained sector-aligned by this writer (full rewrite ditto).
        debug_assert_eq!(cursor % BLOCK_SIZE as u64, 0);

        let (run_header, payload) = Self::encode_run(new_run)?;
        let run_total = std::mem::size_of::<SortedRunHeader>() as u64 + payload.len() as u64;
        if cursor + run_total > region_size {
            return Err(StorageError::RegionFull {
                used: cursor + run_total,
                size: region_size,
            });
        }

        // Write run header + payload at cursor (these are *new* sectors —
        // never touched before).
        device.write_at(offset + cursor, bytemuck::bytes_of(&run_header))?;
        device.write_at(
            offset + cursor + std::mem::size_of::<SortedRunHeader>() as u64,
            &payload,
        )?;
        // Advance to the next sector boundary so future appends start aligned.
        let new_cursor = Self::align_up_to_sector(cursor + run_total);
        let new_payload_used = new_cursor - BLOCK_SIZE as u64;
        if new_payload_used > u32::MAX as u64 {
            return Err(StorageError::RegionFull {
                used: new_payload_used,
                size: region_size,
            });
        }

        // Mutate header fields.
        existing_header.payload_used = new_payload_used as u32;
        existing_header.sorted_run_count = { existing_header.sorted_run_count } + 1;
        existing_header.seq = { existing_header.seq } + 1;
        if new_run.journal_seq > { existing_header.last_persisted_lsn } {
            existing_header.last_persisted_lsn = new_run.journal_seq;
        }

        // Rewrite header sector only (4 KiB).
        let mut header_sector = vec![0u8; BLOCK_SIZE];
        header_sector[..std::mem::size_of::<BtreeNodeHeader>()]
            .copy_from_slice(existing_header.as_bytes());
        device.write_at(offset, &header_sector)?;
        Ok(())
    }
}

// ---------- Packed-key path (R1a-pack) ----------

impl BtreeRegion {
    /// Encode a sorted run using the packed-key codec (IMPL §1.5.6). Returns
    /// the per-run header (with `SORTED_RUN_FLAG_PACKED_KEYS` set, CRC
    /// computed) and the payload bytes.
    fn encode_run_packed<K, V>(
        run: &SortedRun<K, V>,
        value_size: usize,
    ) -> Result<(SortedRunHeader, Vec<u8>), StorageError>
    where
        K: pack::PackableKey,
        V: AsRef<[u8]>,
    {
        let (head, fields) = pack::select_format(&run.entries)?;
        let payload = pack::encode_packed_run(&run.entries, &head, &fields)?;
        let _ = value_size; // value_size is encoded in the entries' length; recorded for symmetry.
        let mut header = SortedRunHeader {
            magic: SORTED_RUN_MAGIC,
            seq: run.seq,
            journal_seq: run.journal_seq,
            entry_count: run.entries.len() as u32,
            payload_length: payload.len() as u32,
            flags: run.flags | SORTED_RUN_FLAG_PACKED_KEYS,
            crc: 0,
        };
        let crc = SortedRunHeader::compute_crc(bytemuck::bytes_of(&header), &payload);
        header.crc = crc;
        Ok((header, payload))
    }

    /// Write a freshly built region using the packed-key codec for every
    /// sorted run. Sets [`SORTED_RUN_FLAG_PACKED_KEYS`] on each per-run
    /// header. The CBOR fallback is the existing [`Self::write_full`].
    pub fn write_full_packed<D: BlockDevice, K, V>(
        device: &D,
        offset: u64,
        node: &mut LoadedNode<K, V>,
        value_size: usize,
    ) -> Result<(), StorageError>
    where
        K: pack::PackableKey,
        V: AsRef<[u8]>,
    {
        let region_size = Self::region_size(&node.header);
        let mut cursor: u64 = BLOCK_SIZE as u64;

        for run in &node.sorted_runs {
            let (run_header, payload) = Self::encode_run_packed(run, value_size)?;
            let run_total = std::mem::size_of::<SortedRunHeader>() as u64 + payload.len() as u64;
            if cursor + run_total > region_size {
                return Err(StorageError::RegionFull {
                    used: cursor + run_total,
                    size: region_size,
                });
            }
            device.write_at(offset + cursor, bytemuck::bytes_of(&run_header))?;
            device.write_at(
                offset + cursor + std::mem::size_of::<SortedRunHeader>() as u64,
                &payload,
            )?;
            cursor = Self::align_up_to_sector(cursor + run_total);
        }
        let payload_used = cursor - BLOCK_SIZE as u64;
        if payload_used > u32::MAX as u64 {
            return Err(StorageError::RegionFull {
                used: payload_used,
                size: region_size,
            });
        }
        node.header.payload_used = payload_used as u32;
        node.header.sorted_run_count = node.sorted_runs.len() as u8;
        node.header.seq = { node.header.seq } + 1;

        let mut header_sector = vec![0u8; BLOCK_SIZE];
        header_sector[..std::mem::size_of::<BtreeNodeHeader>()]
            .copy_from_slice(node.header.as_bytes());
        device.write_at(offset, &header_sector)?;
        node.dirty = false;
        Ok(())
    }

    /// Append a packed sorted run, mirroring [`Self::append_sorted_run`] for
    /// the CBOR path. Sets [`SORTED_RUN_FLAG_PACKED_KEYS`] on the new run.
    pub fn append_sorted_run_packed<D: BlockDevice, K, V>(
        device: &D,
        offset: u64,
        existing_header: &mut BtreeNodeHeader,
        new_run: &SortedRun<K, V>,
        value_size: usize,
    ) -> Result<(), StorageError>
    where
        K: pack::PackableKey,
        V: AsRef<[u8]>,
    {
        let region_size = Self::region_size(existing_header);
        let payload_used = { existing_header.payload_used } as u64;
        let cursor = BLOCK_SIZE as u64 + payload_used;
        debug_assert_eq!(cursor % BLOCK_SIZE as u64, 0);

        let (run_header, payload) = Self::encode_run_packed(new_run, value_size)?;
        let run_total = std::mem::size_of::<SortedRunHeader>() as u64 + payload.len() as u64;
        if cursor + run_total > region_size {
            return Err(StorageError::RegionFull {
                used: cursor + run_total,
                size: region_size,
            });
        }

        device.write_at(offset + cursor, bytemuck::bytes_of(&run_header))?;
        device.write_at(
            offset + cursor + std::mem::size_of::<SortedRunHeader>() as u64,
            &payload,
        )?;
        let new_cursor = Self::align_up_to_sector(cursor + run_total);
        let new_payload_used = new_cursor - BLOCK_SIZE as u64;
        if new_payload_used > u32::MAX as u64 {
            return Err(StorageError::RegionFull {
                used: new_payload_used,
                size: region_size,
            });
        }
        existing_header.payload_used = new_payload_used as u32;
        existing_header.sorted_run_count = { existing_header.sorted_run_count } + 1;
        existing_header.seq = { existing_header.seq } + 1;
        if new_run.journal_seq > { existing_header.last_persisted_lsn } {
            existing_header.last_persisted_lsn = new_run.journal_seq;
        }

        let mut header_sector = vec![0u8; BLOCK_SIZE];
        header_sector[..std::mem::size_of::<BtreeNodeHeader>()]
            .copy_from_slice(existing_header.as_bytes());
        device.write_at(offset, &header_sector)?;
        Ok(())
    }

    /// Read a region whose sorted runs may be a mix of packed-key and CBOR
    /// encodings. The caller supplies the fixed `value_size` (in bytes) and
    /// the expected `prefix_template` for packed runs (passed to the
    /// decoder so full values can be reconstructed). CBOR runs ignore both
    /// arguments.
    ///
    /// Use this rather than [`Self::read`] when the tree was written with
    /// the packed codec for any of its runs.
    pub fn read_packed<D: BlockDevice, K, V>(
        device: &D,
        offset: u64,
        kind: BtreeKind,
        value_size: usize,
        prefix_template: &[u8],
    ) -> Result<LoadedNode<K, V>, StorageError>
    where
        K: pack::PackableKey + DeserializeOwned + Ord + Clone,
        V: From<Vec<u8>> + DeserializeOwned + Clone,
    {
        let mut header_sector = vec![0u8; BLOCK_SIZE];
        device.read_at(offset, &mut header_sector)?;
        let header = BtreeNodeHeader::parse(&header_sector)?;
        let header_kind = { header.pre.kind };
        if header_kind != kind as u16 {
            return Err(StorageError::InvalidBtreeKind(header_kind));
        }

        let region_size = Self::region_size(&header);
        let payload_used = { header.payload_used } as u64;
        if payload_used > region_size {
            return Err(StorageError::RegionFull {
                used: payload_used,
                size: region_size,
            });
        }

        let mut sorted_runs: SmallVec<[SortedRun<K, V>; 4]> = SmallVec::new();
        let mut cursor: u64 = BLOCK_SIZE as u64;
        let header_payload_end = BLOCK_SIZE as u64 + payload_used;
        let expected_count = { header.sorted_run_count } as usize;
        let mut runs_read = 0usize;
        while cursor < header_payload_end && runs_read < expected_count {
            let mut run_header_buf = [0u8; std::mem::size_of::<SortedRunHeader>()];
            device.read_at(offset + cursor, &mut run_header_buf)?;
            let run_header: SortedRunHeader = *bytemuck::from_bytes(&run_header_buf);
            let magic = { run_header.magic };
            if magic != SORTED_RUN_MAGIC {
                log::warn!(
                    "btree region at offset {offset:#x}: torn sorted-run magic at \
                     cursor {cursor:#x} (expected {SORTED_RUN_MAGIC:#x}, got {magic:#x}); \
                     dropping this and trailing runs",
                );
                break;
            }
            let payload_length = { run_header.payload_length } as usize;
            let flags = { run_header.flags };
            let mut payload = vec![0u8; payload_length];
            let payload_offset =
                offset + cursor + std::mem::size_of::<SortedRunHeader>() as u64;
            device.read_at(payload_offset, &mut payload)?;
            if let Err(e) = run_header.verify(&payload) {
                log::warn!(
                    "btree region at offset {offset:#x}: torn sorted-run CRC at \
                     cursor {cursor:#x}: {e}; dropping this and trailing runs",
                );
                break;
            }

            let entries: Vec<(K, V)> = if flags & SORTED_RUN_FLAG_PACKED_KEYS != 0 {
                // Packed path.
                let entry_count = { run_header.entry_count };
                let decoded = pack::decode_packed_run_with_prefix::<K>(
                    &payload,
                    value_size,
                    entry_count,
                    prefix_template,
                )?;
                decoded
                    .into_iter()
                    .map(|(k, v)| (k, V::from(v)))
                    .collect()
            } else {
                // CBOR fallback.
                ciborium::de::from_reader(payload.as_slice())
                    .map_err(|e| StorageError::CborDecode(e.to_string()))?
            };

            sorted_runs.push(SortedRun {
                seq: { run_header.seq },
                journal_seq: { run_header.journal_seq },
                flags,
                entries,
            });
            runs_read += 1;
            let run_total = std::mem::size_of::<SortedRunHeader>() as u64 + payload_length as u64;
            cursor = Self::align_up_to_sector(cursor + run_total);
        }

        Ok(LoadedNode {
            header,
            sorted_runs,
            merged_view: RefCell::new(None),
            pending_journal: Vec::new(),
            dirty: false,
        })
    }
}

// ---------- Compaction ----------

/// Compaction trigger per IMPL §1.5.4: `payload_used > 75%` of region OR
/// `sorted_run_count > 4`.
pub fn should_compact(header: &BtreeNodeHeader, region_size: u32) -> bool {
    let used = { header.payload_used } as u64;
    let count = { header.sorted_run_count } as u32;
    let threshold = (region_size as u64 * 3) / 4;
    used > threshold || count > 4
}

/// In-memory full compaction. Produces a fresh `LoadedNode` with exactly one
/// sorted run that is the k-way merge of all existing runs plus the
/// pending-journal overlay, with whiteouts dropped.
///
/// **Whiteout handling for R1a-core.** Whiteouts in the pending journal are
/// dropped during compaction (the resolved key simply does not appear in the
/// output). This is correct in the absence of snapshots — there is no older
/// view that could need to see "this key was removed at LSN X". Snapshots
/// (R6) will need a snapshot-aware variant that retains tombstones until the
/// oldest live snapshot moves past them.
///
/// The compaction sets `BTREE_NODE_FLAG_COMPACTION_IN_PROGRESS` on the source
/// header (in case a caller wants to mirror that flag onto an on-disk header
/// before persisting), but the in-memory output already has the flag cleared
/// — the resulting node is the post-compaction state, not the in-progress
/// snapshot.
pub fn compact<K, V>(node: &LoadedNode<K, V>) -> LoadedNode<K, V>
where
    K: Ord + Clone,
    V: Clone,
{
    let merged: Vec<(K, V)> = node
        .merge_iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

    let mut new_header = node.header;
    // Bump seq; clear the in-progress flag (the result is the *post*-
    // compaction node).
    new_header.seq = { new_header.seq } + 1;
    new_header.flags = { new_header.flags } & !BTREE_NODE_FLAG_COMPACTION_IN_PROGRESS;
    new_header.sorted_run_count = if merged.is_empty() { 0 } else { 1 };
    // payload_used will be set by `BtreeRegion::write_full` when this node is
    // persisted; the in-memory value is best-effort 0 here.
    new_header.payload_used = 0;

    // Take the highest LSN we've folded in.
    let max_lsn = node
        .pending_journal
        .iter()
        .map(|e| e.lsn)
        .chain(node.sorted_runs.iter().map(|r| r.journal_seq))
        .max()
        .unwrap_or(0);

    let mut sorted_runs = SmallVec::new();
    if !merged.is_empty() {
        sorted_runs.push(SortedRun::from_sorted(0, max_lsn, merged));
    }

    if max_lsn > { new_header.last_persisted_lsn } {
        new_header.last_persisted_lsn = max_lsn;
    }

    LoadedNode {
        header: new_header,
        sorted_runs,
        merged_view: RefCell::new(None),
        pending_journal: Vec::new(),
        dirty: true,
    }
}

#[cfg(test)]
mod tests;
