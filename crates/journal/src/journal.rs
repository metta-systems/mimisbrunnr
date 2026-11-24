//! The journal: an ordered list of buckets (JOURNAL-DESIGN§1.1) holding write-once journal
//! sets (JOURNAL-DESIGN§1.2), recovered by walking valid consecutive sets (JOURNAL-DESIGN§1.5)
//! and trimmed by `last_seq` (JOURNAL-DESIGN§1.4).
//!
//! Each [`Wal::append`] writes one jset holding one entry and syncs it. TODO(JOURNAL10): group
//! commit seals many entries into one set.

use {
    log::trace,
    mimisbrunnr_storage::{BLOCK_SIZE, BlockDevice},
    mimisbrunnr_types::HybridTimestamp,
};

use crate::{
    entry::{WalOp, WalOpKind},
    error::WalError,
    jset::{self, JsetHeader, WalEntry},
};

/// Largest `seq` whose LSNs fit in 64 bits.
const MAX_SEQ: u64 = (1 << 48) - 1;

/// One bucket of the journal list.
#[derive(Clone, Copy, Debug)]
struct JournalBucket {
    bucket_no: u32,
    /// First and newest `seq` written in the bucket since it was last taken; `None` when it
    /// holds nothing replay could need.
    seqs: Option<(u64, u64)>,
}

/// The journal of one disk.
pub struct Wal {
    bucket_size_log2: u8,
    buckets: Vec<JournalBucket>,
    /// Index into `buckets` of the bucket being written.
    current: usize,
    /// Next unwritten sector of the current bucket.
    next_sector: u32,
    next_seq: u64,
    /// `last_seq` the next jset records: the seq of the newest committed checkpoint.
    trim_seq: u64,
    /// `last_seq` of the newest jset written; buckets wholly older than it are free.
    durable_last_seq: u64,
    read_only: bool,
}

impl Wal {
    fn new(bucket_size_log2: u8, bucket_numbers: &[u32], read_only: bool) -> Result<Self, WalError> {
        if bucket_numbers.is_empty() {
            return Err(WalError::InvalidGeometry("no journal buckets"));
        }
        if !(12..=63).contains(&bucket_size_log2) {
            return Err(WalError::InvalidGeometry("bucket size"));
        }
        Ok(Self {
            bucket_size_log2,
            buckets: bucket_numbers
                .iter()
                .map(|&bucket_no| JournalBucket { bucket_no, seqs: None })
                .collect(),
            current: 0,
            next_sector: 0,
            next_seq: 1,
            trim_seq: 0,
            durable_last_seq: 0,
            read_only,
        })
    }

    fn sectors_per_bucket(&self) -> u32 {
        ((1u64 << self.bucket_size_log2) / BLOCK_SIZE as u64).min(u32::MAX as u64) as u32
    }

    fn offset(&self, index: usize, sector: u32) -> u64 {
        ((self.buckets[index].bucket_no as u64) << self.bucket_size_log2)
            + sector as u64 * BLOCK_SIZE as u64
    }

    /// Start an empty journal in `bucket_numbers`, zeroing them so no earlier data reads as
    /// a jset.
    pub fn format(
        device: &dyn BlockDevice,
        bucket_size_log2: u8,
        bucket_numbers: &[u32],
    ) -> Result<Self, WalError> {
        trace!("journal::format buckets={bucket_numbers:?}");
        let wal = Self::new(bucket_size_log2, bucket_numbers, false)?;
        let zeros = vec![0u8; 1 << bucket_size_log2];
        for index in 0..wal.buckets.len() {
            device.write_at(wal.offset(index, 0), &zeros)?;
        }
        device.sync()?;
        Ok(wal)
    }

    /// Recover the journal in `bucket_numbers` (JOURNAL-DESIGN§1.5): read the first jset of
    /// every bucket, start at the bucket holding the newest one's `last_seq` and walk valid
    /// consecutive sets to the tail.
    pub fn open(
        device: &dyn BlockDevice,
        bucket_size_log2: u8,
        bucket_numbers: &[u32],
    ) -> Result<Self, WalError> {
        Self::recover(device, bucket_size_log2, bucket_numbers, false)
    }

    /// [`Wal::open`] for inspection: every mutating call fails with [`WalError::ReadOnly`].
    pub fn open_read_only(
        device: &dyn BlockDevice,
        bucket_size_log2: u8,
        bucket_numbers: &[u32],
    ) -> Result<Self, WalError> {
        Self::recover(device, bucket_size_log2, bucket_numbers, true)
    }

    fn recover(
        device: &dyn BlockDevice,
        bucket_size_log2: u8,
        bucket_numbers: &[u32],
        read_only: bool,
    ) -> Result<Self, WalError> {
        let mut wal = Self::new(bucket_size_log2, bucket_numbers, read_only)?;
        let mut firsts = Vec::with_capacity(wal.buckets.len());
        for index in 0..wal.buckets.len() {
            firsts.push(wal.read_jset(device, index, 0)?.map(|(header, _)| header));
        }
        let valid = || firsts.iter().enumerate().filter_map(|(index, first)| Some((index, first.as_ref()?)));
        let Some((_, newest)) = valid().max_by_key(|(_, header)| header.seq) else {
            return Ok(wal);
        };
        let needed = newest.last_seq;
        let start = valid()
            .filter(|(_, header)| header.seq <= needed)
            .max_by_key(|(_, header)| header.seq)
            .or_else(|| valid().min_by_key(|(_, header)| header.seq))
            .map(|(index, _)| index)
            .expect("a valid first jset exists");

        let mut chain = Vec::new();
        let (tail_bucket, tail_sector, next_seq) = wal.walk(device, start, |index, sector, header, _| {
            chain.push((index, sector, *header));
            Ok(())
        })?;
        for &(index, sector, header) in &chain {
            let seqs = &mut wal.buckets[index].seqs;
            let first = if sector == 0 { header.seq } else { seqs.map_or(header.seq, |(first, _)| first) };
            *seqs = Some((first, header.seq));
        }
        let (_, _, last_header) = *chain.last().expect("the start jset is valid");
        wal.current = tail_bucket;
        wal.next_sector = tail_sector;
        wal.next_seq = next_seq;
        wal.trim_seq = last_header.last_seq;
        wal.durable_last_seq = last_header.last_seq;
        trace!(
            "journal::open start={start} tail=({tail_bucket}, {tail_sector}) next_seq={next_seq} last_seq={}",
            last_header.last_seq
        );
        Ok(wal)
    }

    /// Walk valid consecutive jsets from the start of bucket `start`, calling `visit` for each,
    /// and return the position after the last one and the seq the next would carry. A set
    /// that does not fit the rest of a bucket begins the next bucket in list order.
    fn walk(
        &self,
        device: &dyn BlockDevice,
        start: usize,
        mut visit: impl FnMut(usize, u32, &JsetHeader, &[u8]) -> Result<(), WalError>,
    ) -> Result<(usize, u32, u64), WalError> {
        let (mut index, mut sector) = (start, 0);
        let Some((first, _)) = self.read_jset(device, start, 0)? else {
            return Ok((start, 0, self.next_seq));
        };
        let mut expected = first.seq;
        loop {
            let found = match self.read_jset(device, index, sector)?.filter(|(header, _)| header.seq == expected) {
                Some(found) => Some(found),
                None => {
                    let next = (index + 1) % self.buckets.len();
                    let found = self.read_jset(device, next, 0)?.filter(|(header, _)| header.seq == expected);
                    if found.is_some() {
                        (index, sector) = (next, 0);
                    }
                    found
                }
            };
            let Some((header, bytes)) = found else {
                return Ok((index, sector, expected));
            };
            visit(index, sector, &header, &bytes)?;
            sector += (bytes.len() / BLOCK_SIZE) as u32;
            expected = header.seq + 1;
        }
    }

    /// The valid jset at `sector` of bucket `index` with its bytes, or `None`.
    fn read_jset(
        &self,
        device: &dyn BlockDevice,
        index: usize,
        sector: u32,
    ) -> Result<Option<(JsetHeader, Vec<u8>)>, WalError> {
        let room = self.sectors_per_bucket().saturating_sub(sector) as usize * BLOCK_SIZE;
        if room == 0 {
            return Ok(None);
        }
        let offset = self.offset(index, sector);
        let mut bytes = vec![0u8; BLOCK_SIZE];
        device.read_at(offset, &mut bytes)?;
        let Some((header, size)) = jset::header_and_size(&bytes) else {
            return Ok(None);
        };
        if size > room {
            return Ok(None);
        }
        bytes.resize(size, 0);
        if size > BLOCK_SIZE {
            device.read_at(offset + BLOCK_SIZE as u64, &mut bytes[BLOCK_SIZE..])?;
        }
        Ok(jset::checksum_holds(&bytes).then_some((header, bytes)))
    }

    /// Whether bucket `index` may take new jsets: it is not being written and holds nothing
    /// as new as the durable `last_seq`.
    fn is_free(&self, index: usize) -> bool {
        index != self.current
            && self.buckets[index].seqs.is_none_or(|(_, newest)| newest < self.durable_last_seq)
    }

    // ---------- State ----------

    /// Whether this journal was opened read-only.
    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// LSN the next appended entry gets.
    pub fn next_lsn(&self) -> u64 {
        jset::lsn(self.next_seq, 0)
    }

    /// LSN of the first entry of the jset holding the newest committed checkpoint.
    pub fn last_checkpoint_lsn(&self) -> u64 {
        jset::lsn(self.trim_seq, 0)
    }

    /// Buckets in the journal list.
    pub fn bucket_count(&self) -> usize {
        self.buckets.len()
    }

    /// Buckets holding jsets replay may still need.
    pub fn live_bucket_count(&self) -> usize {
        (0..self.buckets.len())
            .filter(|&index| self.buckets[index].seqs.is_some() && !self.is_free(index))
            .count()
    }

    // ---------- Append ----------

    /// Append `op` as one durable jset and return its LSN.
    pub fn append(
        &mut self,
        device: &dyn BlockDevice,
        op: &WalOp,
        timestamp: HybridTimestamp,
    ) -> Result<u64, WalError> {
        let (op_kind, payload) = op.encode()?;
        self.append_raw(device, op_kind, &payload, timestamp)
    }

    fn append_raw(
        &mut self,
        device: &dyn BlockDevice,
        op_kind: WalOpKind,
        payload: &[u8],
        timestamp: HybridTimestamp,
    ) -> Result<u64, WalError> {
        if self.read_only {
            return Err(WalError::ReadOnly);
        }
        let seq = self.next_seq;
        if seq > MAX_SEQ {
            return Err(WalError::LsnOverflow);
        }
        let bytes = jset::encode_single(seq, self.trim_seq, op_kind, payload, timestamp)?;
        let sectors = (bytes.len() / BLOCK_SIZE) as u32;
        if sectors > self.sectors_per_bucket() {
            return Err(WalError::InvalidGeometry("jset larger than a bucket"));
        }
        if self.next_sector + sectors > self.sectors_per_bucket() {
            let next = (self.current + 1) % self.buckets.len();
            if !self.is_free(next) {
                return Err(WalError::JournalFull { buckets: self.buckets.len() });
            }
            self.current = next;
            self.next_sector = 0;
            self.buckets[next].seqs = None;
        }
        device.write_at(self.offset(self.current, self.next_sector), &bytes)?;
        device.sync()?;

        let bucket = &mut self.buckets[self.current];
        bucket.seqs = Some((bucket.seqs.map_or(seq, |(first, _)| first), seq));
        self.next_sector += sectors;
        self.next_seq = seq + 1;
        self.durable_last_seq = self.trim_seq;
        trace!("journal::append seq={seq} kind={op_kind:?} payload_len={}", payload.len());
        Ok(jset::lsn(seq, 0))
    }

    // ---------- Checkpoint and replay ----------

    /// Record that the checkpoint entry at `lsn` is committed: later jsets name its jset as
    /// the oldest replay needs, and buckets wholly older than it are freed once one of them is
    /// durable.
    pub fn checkpoint(&mut self, lsn: u64) -> Result<(), WalError> {
        if self.read_only {
            return Err(WalError::ReadOnly);
        }
        self.trim_seq = self.trim_seq.max(lsn >> 16);
        Ok(())
    }

    /// Every entry from `start_lsn` on that the journal still holds, in LSN order.
    pub fn entries_from(&self, device: &dyn BlockDevice, start_lsn: u64) -> Result<Vec<WalEntry>, WalError> {
        let oldest = (0..self.buckets.len())
            .filter(|&index| !self.is_free(index))
            .filter_map(|index| Some((index, self.buckets[index].seqs?.0)))
            .min_by_key(|&(_, first)| first);
        let Some((start, _)) = oldest else {
            return Ok(Vec::new());
        };
        let mut entries = Vec::new();
        self.walk(device, start, |_, _, header, bytes| {
            if jset::lsn(header.seq, u16::MAX) >= start_lsn {
                entries.extend(jset::decode_entries(header, bytes)?.into_iter().filter(|entry| entry.lsn >= start_lsn));
            }
            Ok(())
        })?;
        Ok(entries)
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::entry::{AddTag, AppendChunks, CreateObject},
        mimisbrunnr_storage::{FileBlockDevice, StorageError},
        std::sync::Mutex,
        tempfile::NamedTempFile,
    };

    const BUCKET_LOG2: u8 = 16; // 64 KiB buckets: 16 sectors
    const BUCKETS: [u32; 4] = [3, 4, 5, 6];

    /// Records every write after it is armed.
    struct TrackingDevice {
        inner: FileBlockDevice,
        writes: Mutex<Option<Vec<(u64, usize)>>>,
    }

    impl BlockDevice for TrackingDevice {
        fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), StorageError> {
            self.inner.read_at(offset, buf)
        }
        fn write_at(&self, offset: u64, buf: &[u8]) -> Result<(), StorageError> {
            if let Some(writes) = self.writes.lock().unwrap().as_mut() {
                writes.push((offset, buf.len()));
            }
            self.inner.write_at(offset, buf)
        }
        fn capacity(&self) -> u64 {
            self.inner.capacity()
        }
        fn sync(&self) -> Result<(), StorageError> {
            self.inner.sync()
        }
    }

    fn device() -> (NamedTempFile, TrackingDevice) {
        let file = NamedTempFile::new().unwrap();
        let inner = FileBlockDevice::open(file.path(), 8 << BUCKET_LOG2).unwrap();
        (file, TrackingDevice { inner, writes: Mutex::new(None) })
    }

    fn timestamp() -> HybridTimestamp {
        HybridTimestamp::new(1_700_000_000_000_000_000, 0, 1)
    }

    fn create_object(oid: u64) -> WalOp {
        WalOp::CreateObject(CreateObject { oid, created_ns: 12345, state: 0 })
    }

    fn format(device: &dyn BlockDevice) -> Wal {
        Wal::format(device, BUCKET_LOG2, &BUCKETS).unwrap()
    }

    fn reopen(device: &dyn BlockDevice) -> Wal {
        Wal::open(device, BUCKET_LOG2, &BUCKETS).unwrap()
    }

    fn oids(wal: &Wal, device: &dyn BlockDevice) -> Vec<u64> {
        wal.entries_from(device, 0)
            .unwrap()
            .iter()
            .map(|entry| match WalOp::decode(WalOpKind::from_u8(entry.op_kind).unwrap(), &entry.payload).unwrap() {
                WalOp::CreateObject(create) => create.oid,
                other => panic!("unexpected {other:?}"),
            })
            .collect()
    }

    #[test]
    fn format_then_open_is_empty() {
        let (_file, device) = device();
        format(&device);
        let wal = reopen(&device);
        assert_eq!(wal.next_lsn(), jset::lsn(1, 0));
        assert_eq!(wal.live_bucket_count(), 0);
        assert!(wal.entries_from(&device, 0).unwrap().is_empty());
    }

    #[test]
    fn lsns_are_seq_shifted_and_survive_reopen() {
        let (_file, device) = device();
        let mut wal = format(&device);
        for oid in 0..5 {
            assert_eq!(wal.append(&device, &create_object(oid), timestamp()).unwrap(), jset::lsn(oid + 1, 0));
        }
        let reopened = reopen(&device);
        assert_eq!(reopened.next_lsn(), jset::lsn(6, 0));
        assert_eq!(oids(&reopened, &device), [0, 1, 2, 3, 4]);
        let entry = &reopened.entries_from(&device, jset::lsn(3, 0)).unwrap()[0];
        assert_eq!((entry.lsn, entry.timestamp), (jset::lsn(3, 0), timestamp()));
    }

    #[test]
    fn an_add_tag_entry_occupies_24_bytes() {
        let (_file, device) = device();
        let mut wal = format(&device);
        wal.append(&device, &WalOp::AddTag(AddTag { oid: 1, tag: 2 }), timestamp()).unwrap();
        let mut block = vec![0u8; BLOCK_SIZE];
        device.read_at(wal.offset(0, 0), &mut block).unwrap();
        let (header, _) = jset::header_and_size(&block).unwrap();
        assert_eq!((header.payload_len, header.entry_count), (24, 1));
    }

    #[test]
    fn a_torn_last_jset_loses_only_its_entries() {
        let (_file, device) = device();
        let mut wal = format(&device);
        for oid in 0..3 {
            wal.append(&device, &create_object(oid), timestamp()).unwrap();
        }
        // The third set's write tore: its block reached the device half written.
        device.write_at(wal.offset(0, 2) + 2048, &[0x5a; 2048]).unwrap();
        let mut reopened = reopen(&device);
        assert_eq!(oids(&reopened, &device), [0, 1]);
        assert_eq!(reopened.next_lsn(), jset::lsn(3, 0));
        // The next set takes the torn one's place and seq.
        assert_eq!(reopened.append(&device, &create_object(9), timestamp()).unwrap(), jset::lsn(3, 0));
        assert_eq!(oids(&reopen(&device), &device), [0, 1, 9]);
    }

    #[test]
    fn a_jset_is_never_rewritten_and_a_full_journal_refuses() {
        let (_file, device) = device();
        let mut wal = format(&device);
        *device.writes.lock().unwrap() = Some(Vec::new());
        let capacity = BUCKETS.len() as u64 * 16;
        for oid in 0..capacity {
            wal.append(&device, &create_object(oid), timestamp()).unwrap();
        }
        assert!(matches!(
            wal.append(&device, &create_object(capacity), timestamp()),
            Err(WalError::JournalFull { buckets: 4 })
        ));
        let mut writes = device.writes.lock().unwrap().take().unwrap();
        assert_eq!(writes.len() as u64, capacity);
        writes.sort();
        for pair in writes.windows(2) {
            assert!(pair[0].0 + pair[0].1 as u64 <= pair[1].0, "overlapping writes {pair:?}");
        }
        assert_eq!(oids(&reopen(&device), &device), (0..capacity).collect::<Vec<_>>());
    }

    #[test]
    fn checkpoints_free_buckets_for_reuse_across_reopen() {
        let (_file, device) = device();
        let mut wal = format(&device);
        // Three times round the four buckets, checkpointing every 8 sets.
        for oid in 0..200u64 {
            let lsn = wal.append(&device, &create_object(oid), timestamp()).unwrap();
            if oid % 8 == 7 {
                wal.checkpoint(lsn).unwrap();
            }
        }
        let mut reopened = reopen(&device);
        assert_eq!(reopened.next_lsn(), jset::lsn(201, 0));
        // The checkpoint at seq 200 is recorded by the next jset; seq 200 carries 192's.
        assert_eq!(reopened.last_checkpoint_lsn(), jset::lsn(192, 0));
        let held = oids(&reopened, &device);
        // Replay starts at the bucket holding seq 192: seqs 177..=200.
        assert_eq!(held, (176..200).collect::<Vec<_>>());
        let from_checkpoint = reopened.entries_from(&device, jset::lsn(200, 0)).unwrap();
        assert_eq!(from_checkpoint.len(), 1);
        // The reopened journal keeps going round.
        for oid in 200..300u64 {
            let lsn = reopened.append(&device, &create_object(oid), timestamp()).unwrap();
            reopened.checkpoint(lsn).unwrap();
        }
        assert_eq!(oids(&reopen(&device), &device).last(), Some(&299));
    }

    #[test]
    fn a_2000_chunk_append_is_one_entry_that_survives_reopen() {
        let file = NamedTempFile::new().unwrap();
        let device = FileBlockDevice::open(file.path(), 8 << 20).unwrap();
        let mut wal = Wal::format(&device, 20, &[1, 2]).unwrap();
        let op = WalOp::AppendChunks(AppendChunks {
            oid: 7,
            start_offset: 0,
            new_length: 2000 * 65536,
            chunks: (0..2000).map(crate::entry::tests::chunk_spec).collect(),
        });
        let lsn = wal.append(&device, &op, timestamp()).unwrap();
        let entries = Wal::open(&device, 20, &[1, 2]).unwrap().entries_from(&device, 0).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].lsn, lsn);
        assert_eq!(WalOp::decode(WalOpKind::AppendChunks, &entries[0].payload).unwrap(), op);
    }

    #[test]
    fn open_read_only_rejects_mutations() {
        let (_file, device) = device();
        format(&device);
        let mut wal = Wal::open_read_only(&device, BUCKET_LOG2, &BUCKETS).unwrap();
        assert!(wal.is_read_only());
        assert!(matches!(wal.append(&device, &create_object(1), timestamp()), Err(WalError::ReadOnly)));
        assert!(matches!(wal.checkpoint(1), Err(WalError::ReadOnly)));
    }

    #[test]
    fn invalid_geometry_is_refused() {
        let (_file, device) = device();
        assert!(matches!(Wal::format(&device, BUCKET_LOG2, &[]), Err(WalError::InvalidGeometry(_))));
        assert!(matches!(Wal::format(&device, 11, &BUCKETS), Err(WalError::InvalidGeometry(_))));
    }
}
