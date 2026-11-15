//! Write-once B+ tree regions (BTREE-DESIGN§1.1–1.2, BTREE-DESIGN§1.6).
//!
//! Block 0 of a region holds its [`BtreeNodeHeader`], written once. Runs are
//! appended after it: an index block ([`RunHeader`], [`KeyFormat`], one
//! [`RunBlockEntry`] per data block, the fence key of each data block) and its
//! data blocks. Entry blocks are self-contained — fixed-width keys, entry
//! flags, value ends, 8-aligned values — and a value too large for one block
//! lives in value blocks of the same run. A lookup reads the run's index block,
//! which [`Region::open`] keeps, and one data block per run.
//!
//! Every checksum is [`checksum64`] over the block with its slot zeroed.

use {
    bytemuck::{Pod, Zeroable},
    static_assertions::const_assert_eq,
    std::collections::BTreeMap,
};

use crate::{
    block::{BLOCK_SIZE, BtreeKind},
    block_device::BlockDevice,
    checksum::checksum64,
    error::StorageError,
    preamble::{LAYOUT_VERSION, MAGIC_RUN_INDEX, Preamble, Structure},
};

// ---------- Region envelope (BTREE-DESIGN§1.1) ----------

/// Block 0 of every region, written once.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
pub struct BtreeNodeHeader {
    /// `[0..8]` magic `BREG`, kind ∈ [`BtreeKind`].
    pub pre: Preamble,
    /// `[8..16]` bumped each time the node moves to a fresh region.
    pub node_seq: u64,
    /// `[16..24]` journal LSN when the region was created.
    pub created_lsn: u64,
    /// `[24..25]` 18 = 256 KiB.
    pub region_size_log2: u8,
    /// `[25..26]` 0 = leaf, ≥ 1 = inner.
    pub level: u8,
    /// `[26..27]` [`LAYOUT_RUNS`] or [`LAYOUT_PAGES`].
    pub layout: u8,
    /// `[27..28]` `BTREE_NODE_FLAG_*`.
    pub flags: u8,
    /// `[28..30]` positional: pages in the record array.
    pub logical_pages: u16,
    /// `[30..32]` bytes of kind-specific header after this one.
    pub kind_header_len: u16,
    /// `[32..56]` zero.
    pub _reserved: [u8; 24],
    /// `[56..64]` of block 0, this field zeroed.
    pub checksum: u64,
}

const_assert_eq!(core::mem::size_of::<BtreeNodeHeader>(), 64);
const_assert_eq!(core::mem::offset_of!(BtreeNodeHeader, region_size_log2), 24);
const_assert_eq!(core::mem::offset_of!(BtreeNodeHeader, kind_header_len), 30);
const_assert_eq!(core::mem::offset_of!(BtreeNodeHeader, checksum), 56);

/// `BtreeNodeHeader.layout`: a sequence of keyed runs.
pub const LAYOUT_RUNS: u8 = 0;
/// `BtreeNodeHeader.layout`: positional pages (BTREE-DESIGN§1.7).
pub const LAYOUT_PAGES: u8 = 1;
/// First region of a positional chain (BTREE-DESIGN§1.7).
pub const BTREE_NODE_FLAG_HEAD_OF_CHAIN: u8 = 1 << 1;

const NODE_HEADER_CHECKSUM: core::ops::Range<usize> = 56..64;

// ---------- Runs and blocks (BTREE-DESIGN§1.2) ----------

/// Start of a run's index block.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
pub struct RunHeader {
    /// `[0..8]` magic `BIDX`, kind 0.
    pub pre: Preamble,
    /// `[8..12]` 1 for the first run, +1 per run.
    pub seq: u32,
    /// `[12..14]` data blocks after this index block.
    pub block_count: u16,
    /// `[14..16]` `RUN_FLAG_*`.
    pub flags: u16,
    /// `[16..24]` newest journal LSN merged into this run.
    pub journal_seq: u64,
    /// `[24..28]` entries in the run.
    pub entry_count: u32,
    /// `[28..32]` low 32 bits of block 0's checksum: the region this run
    /// belongs to.
    pub region_tag: u32,
    /// `[32..40]` of the index block, this field zeroed.
    pub checksum: u64,
}

const_assert_eq!(core::mem::size_of::<RunHeader>(), 40);
const RUN_HEADER_CHECKSUM: core::ops::Range<usize> = 32..40;

/// Data blocks encrypted. Never written until runs are encrypted
/// (ENCRYPTION-D6); a run carrying it fails to load.
pub const RUN_FLAG_ENCRYPTED: u16 = 1 << 0;

/// Fixed head of the key format, followed by `nr_fields` [`FieldFormat`]s.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug, Eq, PartialEq)]
pub struct KeyFormatHead {
    /// `[0..1]` 1..=8.
    pub nr_fields: u8,
    /// `[1..2]` bytes per encoded key: Σ field widths.
    pub key_width: u8,
    /// `[2..3]` [`VALUE_KIND_FIXED`] or [`VALUE_KIND_VARIABLE`].
    pub value_kind: u8,
    /// `[3..4]` zero.
    pub _pad: u8,
    /// `[4..8]` value bytes when `value_kind` is fixed.
    pub fixed_value_len: u32,
}

const_assert_eq!(core::mem::size_of::<KeyFormatHead>(), 8);

/// Every value in the run is `fixed_value_len` bytes.
pub const VALUE_KIND_FIXED: u8 = 0;
/// Values vary in length; each block records their ends.
pub const VALUE_KIND_VARIABLE: u8 = 1;

/// One key field's encoding.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug, Eq, PartialEq)]
pub struct FieldFormat {
    /// `[0..1]` 0, 1, 2, 4 or 8 bytes; 0 ⇒ constant (= base).
    pub width: u8,
    /// `[1..2]` `FIELD_FORMAT_FLAG_*`.
    pub flags: u8,
    /// `[2..8]` zero.
    pub _pad: [u8; 6],
    /// `[8..16]` subtracted from the field before encoding.
    pub base: u64,
}

const_assert_eq!(core::mem::size_of::<FieldFormat>(), 16);

/// The field is a two's-complement signed integer.
pub const FIELD_FORMAT_FLAG_SIGNED: u8 = 1 << 0;
/// The field holds big-endian bytes compared lexicographically.
pub const FIELD_FORMAT_FLAG_MSB_FIRST: u8 = 1 << 1;

/// The index block's entry for one data block.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
pub struct RunBlockEntry {
    /// `[0..8]` of the data block.
    pub checksum: u64,
    /// `[8..10]` entries in the block; 0 for a value block.
    pub entry_count: u16,
    /// `[10..12]` `RUN_BLOCK_FLAG_*`.
    pub flags: u16,
    /// `[12..16]` snapshot-aware trees: smallest snapshot id in the block.
    pub min_snapshot: u32,
    /// `[16..20]` … and largest; 0 otherwise.
    pub max_snapshot: u32,
    /// `[20..24]` zero.
    pub _pad: u32,
}

const_assert_eq!(core::mem::size_of::<RunBlockEntry>(), 24);

/// Raw bytes of large values, no block head.
pub const RUN_BLOCK_FLAG_VALUE: u16 = 1 << 0;

/// Head of an entry block.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
pub struct DataBlockHead {
    /// `[0..2]` entries in the block.
    pub entry_count: u16,
    /// `[2..4]` offset of the first value, 8-aligned.
    pub value_area: u16,
    /// `[4..6]` zero.
    pub flags: u16,
    /// `[6..8]` zero.
    pub _reserved: u16,
}

const_assert_eq!(core::mem::size_of::<DataBlockHead>(), 8);

/// Delete; no value bytes.
pub const RUN_ENTRY_FLAG_WHITEOUT: u8 = 1 << 0;
/// The value is a [`LargeValueRef`]; its bytes live in value blocks.
pub const RUN_ENTRY_FLAG_LARGE: u8 = 1 << 1;

/// Where a large value's bytes are.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
pub struct LargeValueRef {
    /// `[0..2]` data-block index within the run.
    pub first_block: u16,
    /// `[2..4]` value blocks it occupies.
    pub block_count: u16,
    /// `[4..8]` value bytes.
    pub length: u32,
}

const_assert_eq!(core::mem::size_of::<LargeValueRef>(), 8);

// ---------- Keys (BTREE-DESIGN§1.6) ----------

/// How one key field orders and encodes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FieldKind {
    /// An unsigned integer.
    Unsigned,
    /// A two's-complement signed integer, carried as its bit pattern.
    Signed,
    /// Eight big-endian bytes compared lexicographically (a hash, a prefix).
    BigEndianBytes,
}

impl FieldKind {
    fn flags(self) -> u8 {
        match self {
            Self::Unsigned => 0,
            Self::Signed => FIELD_FORMAT_FLAG_SIGNED,
            Self::BigEndianBytes => FIELD_FORMAT_FLAG_MSB_FIRST,
        }
    }
}

/// A key a region stores: up to eight `u64` fields in declaration order.
/// `Ord` must agree with comparing the fields in that order (signed fields as
/// signed values).
pub trait RegionKey: Ord + Sized {
    /// The fields, 1..=8 of them.
    const FIELDS: &'static [FieldKind];
    /// Write the fields into `out` (`out.len() == FIELDS.len()`).
    fn field_values(&self, out: &mut [u64]);
    /// Rebuild the key from its fields.
    fn from_field_values(fields: &[u64]) -> Result<Self, StorageError>;
}

impl RegionKey for u32 {
    const FIELDS: &'static [FieldKind] = &[FieldKind::Unsigned];
    fn field_values(&self, out: &mut [u64]) {
        out[0] = *self as u64;
    }
    fn from_field_values(fields: &[u64]) -> Result<Self, StorageError> {
        u32::try_from(fields[0]).map_err(|_| StorageError::RegionCorrupt("u32 key field out of range"))
    }
}

impl RegionKey for u64 {
    const FIELDS: &'static [FieldKind] = &[FieldKind::Unsigned];
    fn field_values(&self, out: &mut [u64]) {
        out[0] = *self;
    }
    fn from_field_values(fields: &[u64]) -> Result<Self, StorageError> {
        Ok(fields[0])
    }
}

impl RegionKey for (u64, u32) {
    const FIELDS: &'static [FieldKind] = &[FieldKind::Unsigned, FieldKind::Unsigned];
    fn field_values(&self, out: &mut [u64]) {
        out[0] = self.0;
        out[1] = self.1 as u64;
    }
    fn from_field_values(fields: &[u64]) -> Result<Self, StorageError> {
        let second =
            u32::try_from(fields[1]).map_err(|_| StorageError::RegionCorrupt("u32 key field out of range"))?;
        Ok((fields[0], second))
    }
}

impl RegionKey for (u32, u32) {
    const FIELDS: &'static [FieldKind] = &[FieldKind::Unsigned, FieldKind::Unsigned];
    fn field_values(&self, out: &mut [u64]) {
        out[0] = self.0 as u64;
        out[1] = self.1 as u64;
    }
    fn from_field_values(fields: &[u64]) -> Result<Self, StorageError> {
        let field = |value: u64| {
            u32::try_from(value).map_err(|_| StorageError::RegionCorrupt("u32 key field out of range"))
        };
        Ok((field(fields[0])?, field(fields[1])?))
    }
}

/// A run's decoded key format.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KeyFormat {
    /// The fixed head.
    pub head: KeyFormatHead,
    /// One format per field.
    pub fields: Vec<FieldFormat>,
}

impl KeyFormat {
    /// The smallest format holding `keys`, chosen from those keys alone.
    fn for_keys<K: RegionKey>(keys: &[&K], value_kind: u8, fixed_value_len: u32) -> Self {
        let field_count = K::FIELDS.len();
        let mut values = vec![0u64; field_count];
        let mut minimum = vec![u64::MAX; field_count];
        let mut maximum = vec![0u64; field_count];
        let mut signed_minimum = vec![i64::MAX; field_count];
        let mut signed_maximum = vec![i64::MIN; field_count];
        for key in keys {
            key.field_values(&mut values);
            for (index, &value) in values.iter().enumerate() {
                minimum[index] = minimum[index].min(value);
                maximum[index] = maximum[index].max(value);
                signed_minimum[index] = signed_minimum[index].min(value as i64);
                signed_maximum[index] = signed_maximum[index].max(value as i64);
            }
        }
        let fields: Vec<FieldFormat> = K::FIELDS
            .iter()
            .enumerate()
            .map(|(index, kind)| {
                let (base, range) = match kind {
                    FieldKind::Signed => (
                        signed_minimum[index] as u64,
                        signed_maximum[index].wrapping_sub(signed_minimum[index]) as u64,
                    ),
                    _ => (minimum[index], maximum[index] - minimum[index]),
                };
                FieldFormat {
                    width: width_for(range),
                    flags: kind.flags(),
                    _pad: [0; 6],
                    base,
                }
            })
            .collect();
        let key_width = fields.iter().map(|field| field.width).sum();
        Self {
            head: KeyFormatHead {
                nr_fields: field_count as u8,
                key_width,
                value_kind,
                _pad: 0,
                fixed_value_len,
            },
            fields,
        }
    }

    fn encoded_len(&self) -> usize {
        core::mem::size_of::<KeyFormatHead>() + self.fields.len() * core::mem::size_of::<FieldFormat>()
    }

    fn key_width(&self) -> usize {
        self.head.key_width as usize
    }

    fn encode_key<K: RegionKey>(&self, key: &K, out: &mut [u8]) {
        let mut values = [0u64; 8];
        key.field_values(&mut values[..self.fields.len()]);
        let mut at = 0;
        for (field, &value) in self.fields.iter().zip(values.iter()) {
            let width = field.width as usize;
            let delta = value.wrapping_sub(field.base);
            if field.flags & FIELD_FORMAT_FLAG_MSB_FIRST != 0 {
                out[at..at + width].copy_from_slice(&delta.to_be_bytes()[8 - width..]);
            } else {
                out[at..at + width].copy_from_slice(&delta.to_le_bytes()[..width]);
            }
            at += width;
        }
    }

    fn decode_key<K: RegionKey>(&self, bytes: &[u8]) -> Result<K, StorageError> {
        let mut values = [0u64; 8];
        let mut at = 0;
        for (field, value) in self.fields.iter().zip(values.iter_mut()) {
            let width = field.width as usize;
            let mut raw = [0u8; 8];
            let delta = if field.flags & FIELD_FORMAT_FLAG_MSB_FIRST != 0 {
                raw[8 - width..].copy_from_slice(&bytes[at..at + width]);
                u64::from_be_bytes(raw)
            } else {
                raw[..width].copy_from_slice(&bytes[at..at + width]);
                u64::from_le_bytes(raw)
            };
            *value = delta.wrapping_add(field.base);
            at += width;
        }
        K::from_field_values(&values[..self.fields.len()])
    }

    /// Check a format read from disk against the key type it is read as.
    fn validate<K: RegionKey>(&self) -> Result<(), StorageError> {
        let matches_type = self.fields.len() == K::FIELDS.len()
            && self.fields.iter().zip(K::FIELDS).all(|(field, kind)| field.flags == kind.flags());
        let widths_valid = self.fields.iter().all(|field| matches!(field.width, 0 | 1 | 2 | 4 | 8));
        let width_sum: usize = self.fields.iter().map(|field| field.width as usize).sum();
        if !matches_type || !widths_valid || width_sum != self.key_width() {
            return Err(StorageError::RegionCorrupt("key format does not match the key type"));
        }
        if !matches!(self.head.value_kind, VALUE_KIND_FIXED | VALUE_KIND_VARIABLE) {
            return Err(StorageError::RegionCorrupt("unknown value kind"));
        }
        Ok(())
    }
}

/// The smallest of 0, 1, 2, 4, 8 bytes holding `range`.
fn width_for(range: u64) -> u8 {
    match range {
        0 => 0,
        1..=0xff => 1,
        0x100..=0xffff => 2,
        0x1_0000..=0xffff_ffff => 4,
        _ => 8,
    }
}

fn align8(value: usize) -> usize {
    value.div_ceil(8) * 8
}

// ---------- Writing ----------

/// What a run records for one key.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunEntry<'a> {
    /// The key's value bytes.
    Value(&'a [u8]),
    /// The key is deleted.
    Whiteout,
}

/// Parameters of a new region.
#[derive(Clone, Copy, Debug)]
pub struct RegionParams {
    /// The tree the region belongs to.
    pub kind: BtreeKind,
    /// 0 = leaf, ≥ 1 = inner.
    pub level: u8,
    /// 12..=19; 18 = 256 KiB.
    pub region_size_log2: u8,
    /// Bumped each time the node moves to a fresh region.
    pub node_seq: u64,
    /// Journal LSN when the region was created.
    pub created_lsn: u64,
}

/// An entry block being packed: its entries' indices in the run.
struct PackedBlock {
    first_entry: usize,
    entry_count: usize,
}

/// Bytes an entry block's fixed parts take for `count` entries.
fn entry_block_head_len(count: usize, key_width: usize, variable: bool) -> usize {
    let value_ends = if variable { count * 2 } else { 0 };
    align8(core::mem::size_of::<DataBlockHead>() + count * key_width + count + value_ends)
}

/// Bytes a value takes in an entry block.
fn inline_value_len(entry: &RunEntry<'_>, large: bool, fixed: Option<usize>) -> usize {
    match (entry, large, fixed) {
        (_, true, _) => core::mem::size_of::<LargeValueRef>(),
        (_, false, Some(fixed_len)) => align8(fixed_len),
        (RunEntry::Whiteout, false, None) => 0,
        (RunEntry::Value(bytes), false, None) => align8(bytes.len()),
    }
}

// ---------- Reading ----------

/// One loaded run: its header, key format, block entries and fence keys.
#[derive(Clone, Debug)]
struct RunIndex {
    header: RunHeader,
    key_format: KeyFormat,
    blocks: Vec<RunBlockEntry>,
    fences: Vec<u8>,
    /// Region block number of the run's first data block.
    first_data_block: u32,
}

/// A value read from a region, held in an 8-aligned buffer.
#[derive(Clone, Debug)]
pub struct Value {
    buffer: Vec<u64>,
    start: usize,
    len: usize,
}

impl Value {
    /// The value's bytes; their start is 8-byte aligned.
    pub fn as_bytes(&self) -> &[u8] {
        &bytemuck::cast_slice::<u64, u8>(&self.buffer)[self.start..self.start + self.len]
    }

    /// The value's bytes, owned.
    pub fn to_vec(&self) -> Vec<u8> {
        self.as_bytes().to_vec()
    }
}

/// A region opened for reading and appending: block 0 and every run's index
/// block, kept in memory; data blocks are read on demand.
#[derive(Clone, Debug)]
pub struct Region {
    offset: u64,
    header: BtreeNodeHeader,
    kind_header: Vec<u8>,
    runs: Vec<RunIndex>,
    next_block: u32,
}

/// Read `count` blocks from `first_block` of the region at `offset` into an
/// 8-aligned buffer.
fn read_blocks<D: BlockDevice>(device: &D, offset: u64, first_block: u32, count: u32) -> Result<Vec<u64>, StorageError> {
    let mut buffer = vec![0u64; count as usize * BLOCK_SIZE / 8];
    device.read_at(
        offset + first_block as u64 * BLOCK_SIZE as u64,
        bytemuck::cast_slice_mut(&mut buffer),
    )?;
    Ok(buffer)
}

/// Checksum of `block` with the 8-byte slot at `slot` zeroed.
fn checksum_with_zeroed_slot(block: &[u8], slot: core::ops::Range<usize>) -> u64 {
    let mut copy = block.to_vec();
    copy[slot].fill(0);
    checksum64(&copy)
}

fn verify(expected: u64, actual: u64) -> Result<(), StorageError> {
    if expected == actual {
        Ok(())
    } else {
        Err(StorageError::ChecksumMismatch { expected, actual })
    }
}

impl Region {
    /// Write block 0 of a new, empty region at byte `offset`.
    pub fn create<D: BlockDevice>(
        device: &D,
        offset: u64,
        params: RegionParams,
        kind_header: &[u8],
    ) -> Result<Self, StorageError> {
        if !(12..=19).contains(&params.region_size_log2) {
            return Err(StorageError::RegionCorrupt("region size out of range"));
        }
        let header_room = BLOCK_SIZE - core::mem::size_of::<BtreeNodeHeader>();
        if kind_header.len() > header_room {
            return Err(StorageError::RegionCorrupt("kind header does not fit block 0"));
        }
        let mut header = BtreeNodeHeader {
            pre: Preamble::new(Structure::Region(params.kind)),
            node_seq: params.node_seq,
            created_lsn: params.created_lsn,
            region_size_log2: params.region_size_log2,
            level: params.level,
            layout: LAYOUT_RUNS,
            flags: 0,
            logical_pages: 0,
            kind_header_len: kind_header.len() as u16,
            _reserved: [0; 24],
            checksum: 0,
        };
        let mut block = vec![0u8; BLOCK_SIZE];
        block[..64].copy_from_slice(bytemuck::bytes_of(&header));
        block[64..64 + kind_header.len()].copy_from_slice(kind_header);
        header.checksum = checksum64(&block);
        block[NODE_HEADER_CHECKSUM].copy_from_slice(&header.checksum.to_le_bytes());
        device.write_at(offset, &block)?;
        Ok(Self {
            offset,
            header,
            kind_header: kind_header.to_vec(),
            runs: Vec::new(),
            next_block: 1,
        })
    }

    /// Rewrite the region at `offset` in place: zero all of it, then
    /// [`Self::create`].
    ///
    /// shortcut: a region is written once and replaced by a fresh one
    /// (BTREE-DESIGN§1.1); trees kept at fixed offsets rewrite in place until
    /// ENGINE3 places them in allocated regions (TODO(ENGINE3)). Rewritten in
    /// place with the same `node_seq` and `created_lsn`, block 0 — and so the
    /// region tag — would repeat; zeroing first keeps runs of the previous
    /// contents from being read as this region's.
    pub fn create_in_place<D: BlockDevice>(
        device: &D,
        offset: u64,
        params: RegionParams,
        kind_header: &[u8],
    ) -> Result<Self, StorageError> {
        if !(12..=19).contains(&params.region_size_log2) {
            return Err(StorageError::RegionCorrupt("region size out of range"));
        }
        device.write_at(offset, &vec![0u8; 1 << params.region_size_log2])?;
        Self::create(device, offset, params, kind_header)
    }

    /// Load the region at `offset`, which must belong to `kind`: block 0 and
    /// each run's index block. Loading stops at the first block that is not
    /// the next valid run, and the newest run is dropped if any of its data
    /// blocks fails its checksum — a torn append loses only that run.
    pub fn open<D: BlockDevice>(device: &D, offset: u64, kind: BtreeKind) -> Result<Self, StorageError> {
        let block_zero = read_blocks(device, offset, 0, 1)?;
        let bytes: &[u8] = bytemuck::cast_slice(&block_zero);
        let header: BtreeNodeHeader = bytemuck::pod_read_unaligned(&bytes[..64]);
        match Preamble::identify(bytes)? {
            Structure::Region(found) if found == kind => {}
            Structure::Region(found) => {
                return Err(StorageError::RegionKindMismatch {
                    expected: kind as u16,
                    actual: found as u16,
                });
            }
            _ => return Err(StorageError::RegionCorrupt("block 0 is not a region header")),
        }
        verify(header.checksum, checksum_with_zeroed_slot(bytes, NODE_HEADER_CHECKSUM))?;
        if header.layout != LAYOUT_RUNS {
            return Err(StorageError::RegionCorrupt("not a keyed-run region"));
        }
        if !(12..=19).contains(&header.region_size_log2) {
            return Err(StorageError::RegionCorrupt("region size out of range"));
        }
        let kind_header_len = header.kind_header_len as usize;
        if kind_header_len > BLOCK_SIZE - 64 {
            return Err(StorageError::RegionCorrupt("kind header overruns block 0"));
        }
        let kind_header = bytes[64..64 + kind_header_len].to_vec();
        let mut region = Self {
            offset,
            header,
            kind_header,
            runs: Vec::new(),
            next_block: 1,
        };
        let total_blocks = region.total_blocks();
        while region.next_block < total_blocks {
            let Some(run) = region.read_run_index(device, region.next_block)? else {
                break;
            };
            let after = run.first_data_block + run.header.block_count as u32;
            if after > total_blocks {
                break;
            }
            region.next_block = after;
            region.runs.push(run);
        }
        if let Some(newest) = region.runs.last() {
            let data = read_blocks(device, offset, newest.first_data_block, newest.blocks.len() as u32)?;
            let data: &[u8] = bytemuck::cast_slice(&data);
            let torn = newest
                .blocks
                .iter()
                .enumerate()
                .any(|(index, entry)| checksum64(&data[index * BLOCK_SIZE..(index + 1) * BLOCK_SIZE]) != entry.checksum);
            if torn {
                let dropped = region.runs.pop().expect("newest run exists");
                region.next_block = dropped.first_data_block - 1;
            }
        }
        Ok(region)
    }

    /// The index block at region block `block_no`, if it is the next valid run.
    fn read_run_index<D: BlockDevice>(&self, device: &D, block_no: u32) -> Result<Option<RunIndex>, StorageError> {
        let block = read_blocks(device, self.offset, block_no, 1)?;
        let bytes: &[u8] = bytemuck::cast_slice(&block);
        let header: RunHeader = bytemuck::pod_read_unaligned(&bytes[..40]);
        let expected_seq = self.runs.len() as u32 + 1;
        if header.pre.magic != MAGIC_RUN_INDEX
            || header.pre.version != LAYOUT_VERSION
            || header.pre.kind != 0
            || header.seq != expected_seq
            || header.region_tag != self.region_tag()
            || header.checksum != checksum_with_zeroed_slot(bytes, RUN_HEADER_CHECKSUM)
        {
            return Ok(None);
        }
        if header.flags & RUN_FLAG_ENCRYPTED != 0 {
            return Err(StorageError::RunEncrypted);
        }
        let format_head: KeyFormatHead = bytemuck::pod_read_unaligned(&bytes[40..48]);
        let field_count = format_head.nr_fields as usize;
        if !(1..=8).contains(&field_count) {
            return Err(StorageError::RegionCorrupt("key field count out of range"));
        }
        let fields_end = 48 + field_count * 16;
        let fields = (0..field_count)
            .map(|index| bytemuck::pod_read_unaligned(&bytes[48 + index * 16..64 + index * 16]))
            .collect();
        let key_format = KeyFormat {
            head: format_head,
            fields,
        };
        let block_count = header.block_count as usize;
        let entries_end = fields_end + block_count * 24;
        let fences_end = entries_end + block_count * key_format.key_width();
        if fences_end > BLOCK_SIZE {
            return Err(StorageError::RegionCorrupt("run index overruns its block"));
        }
        let blocks = (0..block_count)
            .map(|index| bytemuck::pod_read_unaligned(&bytes[fields_end + index * 24..fields_end + (index + 1) * 24]))
            .collect();
        Ok(Some(RunIndex {
            header,
            key_format,
            blocks,
            fences: bytes[entries_end..fences_end].to_vec(),
            first_data_block: block_no + 1,
        }))
    }

    /// The tag every run of this region carries: the low 32 bits of block 0's
    /// checksum (BTREE-DESIGN§1.2).
    pub fn region_tag(&self) -> u32 {
        self.header.checksum as u32
    }

    /// The region's header.
    pub fn header(&self) -> &BtreeNodeHeader {
        &self.header
    }

    /// The kind-specific header after the node header in block 0.
    pub fn kind_header(&self) -> &[u8] {
        &self.kind_header
    }

    /// Runs in the region.
    pub fn run_count(&self) -> usize {
        self.runs.len()
    }

    /// The first unwritten block.
    pub fn next_block(&self) -> u32 {
        self.next_block
    }

    /// Blocks in the region.
    pub fn total_blocks(&self) -> u32 {
        1 << (self.header.region_size_log2 - 12)
    }

    /// Byte offset of the region on its device.
    pub fn offset(&self) -> u64 {
        self.offset
    }

    /// Append one run holding `entries` — ascending, distinct keys — after the
    /// last written block. Data blocks are written before the index block, so
    /// a torn append leaves no valid index. [`StorageError::RunTooLarge`] when
    /// one index block cannot describe the run, [`StorageError::RegionFull`]
    /// when the region has no room for it.
    pub fn append_run<D: BlockDevice, K: RegionKey>(
        &mut self,
        device: &D,
        journal_seq: u64,
        entries: &[(K, RunEntry<'_>)],
    ) -> Result<(), StorageError> {
        if entries.is_empty() {
            return Ok(());
        }
        if entries.windows(2).any(|pair| pair[0].0 >= pair[1].0) {
            return Err(StorageError::UnsortedRunEntries);
        }
        let first_value_len = entries.iter().find_map(|(_, entry)| match entry {
            RunEntry::Value(bytes) => Some(bytes.len()),
            RunEntry::Whiteout => None,
        });
        let all_same_len = entries.iter().all(|(_, entry)| match entry {
            RunEntry::Value(bytes) => Some(bytes.len()) == first_value_len,
            RunEntry::Whiteout => true,
        });
        let fixed = match first_value_len {
            Some(len) if all_same_len && len <= 256 => Some(len),
            None => Some(0),
            _ => None,
        };
        let keys: Vec<&K> = entries.iter().map(|(key, _)| key).collect();
        let key_format = match fixed {
            Some(len) => KeyFormat::for_keys(&keys, VALUE_KIND_FIXED, len as u32),
            None => KeyFormat::for_keys(&keys, VALUE_KIND_VARIABLE, 0),
        };
        let key_width = key_format.key_width();
        let variable = fixed.is_none();

        // A value is large when it cannot share a block with even one other key.
        let single_entry_room = BLOCK_SIZE - entry_block_head_len(1, key_width, variable);
        let large: Vec<bool> = entries
            .iter()
            .map(|(_, entry)| match entry {
                RunEntry::Value(bytes) => fixed.is_none() && align8(bytes.len()) > single_entry_room,
                RunEntry::Whiteout => false,
            })
            .collect();

        // Pack entry blocks greedily.
        let mut packed: Vec<PackedBlock> = Vec::new();
        let mut first_entry = 0;
        while first_entry < entries.len() {
            let mut count = 0;
            let mut values_len = 0;
            while first_entry + count < entries.len() {
                let index = first_entry + count;
                let value_len = inline_value_len(&entries[index].1, large[index], fixed);
                let needed = entry_block_head_len(count + 1, key_width, variable) + values_len + value_len;
                if needed > BLOCK_SIZE || count == u16::MAX as usize {
                    break;
                }
                count += 1;
                values_len += value_len;
            }
            if count == 0 {
                return Err(StorageError::RunTooLarge);
            }
            packed.push(PackedBlock {
                first_entry,
                entry_count: count,
            });
            first_entry += count;
        }

        // Value blocks follow the entry blocks.
        let entry_block_count = packed.len();
        let mut large_refs: BTreeMap<usize, LargeValueRef> = BTreeMap::new();
        let mut value_block_count = 0usize;
        for (index, (_, entry)) in entries.iter().enumerate() {
            if let (true, RunEntry::Value(bytes)) = (large[index], entry) {
                let blocks = bytes.len().div_ceil(BLOCK_SIZE);
                large_refs.insert(
                    index,
                    LargeValueRef {
                        first_block: (entry_block_count + value_block_count) as u16,
                        block_count: blocks as u16,
                        length: bytes.len() as u32,
                    },
                );
                value_block_count += blocks;
            }
        }
        let block_count = entry_block_count + value_block_count;
        let index_len = core::mem::size_of::<RunHeader>()
            + key_format.encoded_len()
            + block_count * (core::mem::size_of::<RunBlockEntry>() + key_width);
        if index_len > BLOCK_SIZE || block_count > u16::MAX as usize {
            return Err(StorageError::RunTooLarge);
        }
        let run_blocks = 1 + block_count as u32;
        if self.next_block + run_blocks > self.total_blocks() {
            return Err(StorageError::RegionFull {
                used: self.next_block as u64 * BLOCK_SIZE as u64,
                size: self.total_blocks() as u64 * BLOCK_SIZE as u64,
            });
        }

        // Data blocks.
        let mut data = vec![0u8; block_count * BLOCK_SIZE];
        let mut block_entries = Vec::with_capacity(block_count);
        let mut fences = vec![0u8; block_count * key_width];
        for (block_index, block) in packed.iter().enumerate() {
            let out = &mut data[block_index * BLOCK_SIZE..(block_index + 1) * BLOCK_SIZE];
            let count = block.entry_count;
            let value_area = entry_block_head_len(count, key_width, variable);
            let head = DataBlockHead {
                entry_count: count as u16,
                value_area: value_area as u16,
                flags: 0,
                _reserved: 0,
            };
            out[..8].copy_from_slice(bytemuck::bytes_of(&head));
            let keys_at = 8;
            let flags_at = keys_at + count * key_width;
            let ends_at = flags_at + count;
            let mut value_at = value_area;
            for slot in 0..count {
                let index = block.first_entry + slot;
                let (key, entry) = &entries[index];
                key_format.encode_key(key, &mut out[keys_at + slot * key_width..keys_at + (slot + 1) * key_width]);
                let mut flags = 0u8;
                let value_start = value_at;
                match (entry, large[index]) {
                    (RunEntry::Whiteout, _) => flags |= RUN_ENTRY_FLAG_WHITEOUT,
                    (RunEntry::Value(_), true) => {
                        flags |= RUN_ENTRY_FLAG_LARGE;
                        out[value_start..value_start + 8].copy_from_slice(bytemuck::bytes_of(&large_refs[&index]));
                    }
                    (RunEntry::Value(bytes), false) => {
                        out[value_start..value_start + bytes.len()].copy_from_slice(bytes);
                    }
                }
                out[flags_at + slot] = flags;
                let value_len = inline_value_len(entry, large[index], fixed);
                let value_end = match (entry, large[index], fixed) {
                    (_, true, _) => value_start + 8,
                    (RunEntry::Value(bytes), false, None) => value_start + bytes.len(),
                    _ => value_start,
                };
                if variable {
                    out[ends_at + slot * 2..ends_at + slot * 2 + 2].copy_from_slice(&(value_end as u16).to_le_bytes());
                }
                value_at = value_start + value_len;
            }
            key_format.encode_key(
                &entries[block.first_entry].0,
                &mut fences[block_index * key_width..(block_index + 1) * key_width],
            );
            block_entries.push(RunBlockEntry {
                checksum: checksum64(out),
                entry_count: count as u16,
                flags: 0,
                // shortcut: snapshot ranges are written as 0 — the layer does
                // not know which key field is the snapshot; set them when the
                // snapshot-aware trees key by snapshot.
                min_snapshot: 0,
                max_snapshot: 0,
                _pad: 0,
            });
        }
        for (&index, value_ref) in &large_refs {
            let RunEntry::Value(bytes) = entries[index].1 else {
                unreachable!("large entries are values")
            };
            let first = value_ref.first_block as usize;
            data[first * BLOCK_SIZE..first * BLOCK_SIZE + bytes.len()].copy_from_slice(bytes);
            for block_index in first..first + value_ref.block_count as usize {
                let out = &data[block_index * BLOCK_SIZE..(block_index + 1) * BLOCK_SIZE];
                key_format.encode_key(
                    &entries[index].0,
                    &mut fences[block_index * key_width..(block_index + 1) * key_width],
                );
                block_entries.push(RunBlockEntry {
                    checksum: checksum64(out),
                    entry_count: 0,
                    flags: RUN_BLOCK_FLAG_VALUE,
                    min_snapshot: 0,
                    max_snapshot: 0,
                    _pad: 0,
                });
            }
        }

        // Index block.
        let mut header = RunHeader {
            pre: Preamble::new(Structure::RunIndex),
            seq: self.runs.len() as u32 + 1,
            block_count: block_count as u16,
            flags: 0,
            journal_seq,
            entry_count: entries.len() as u32,
            region_tag: self.region_tag(),
            checksum: 0,
        };
        let mut index_block = vec![0u8; BLOCK_SIZE];
        index_block[..40].copy_from_slice(bytemuck::bytes_of(&header));
        index_block[40..48].copy_from_slice(bytemuck::bytes_of(&key_format.head));
        for (field_index, field) in key_format.fields.iter().enumerate() {
            index_block[48 + field_index * 16..64 + field_index * 16].copy_from_slice(bytemuck::bytes_of(field));
        }
        let entries_at = 40 + key_format.encoded_len();
        for (block_index, entry) in block_entries.iter().enumerate() {
            index_block[entries_at + block_index * 24..entries_at + (block_index + 1) * 24]
                .copy_from_slice(bytemuck::bytes_of(entry));
        }
        let fences_at = entries_at + block_count * 24;
        index_block[fences_at..fences_at + fences.len()].copy_from_slice(&fences);
        header.checksum = checksum64(&index_block);
        index_block[RUN_HEADER_CHECKSUM].copy_from_slice(&header.checksum.to_le_bytes());

        let index_block_no = self.next_block;
        let block_offset = |block: u32| self.offset + block as u64 * BLOCK_SIZE as u64;
        device.write_at(block_offset(index_block_no + 1), &data)?;
        device.write_at(block_offset(index_block_no), &index_block)?;

        self.runs.push(RunIndex {
            header,
            key_format,
            blocks: block_entries,
            fences,
            first_data_block: index_block_no + 1,
        });
        self.next_block = index_block_no + run_blocks;
        Ok(())
    }

    /// Append `entries` as one run, or as several when one index block cannot
    /// describe them all.
    pub fn append_entries<D: BlockDevice, K: RegionKey>(
        &mut self,
        device: &D,
        journal_seq: u64,
        entries: &[(K, RunEntry<'_>)],
    ) -> Result<(), StorageError> {
        match self.append_run(device, journal_seq, entries) {
            Err(StorageError::RunTooLarge) if entries.len() > 1 => {
                let (older, newer) = entries.split_at(entries.len() / 2);
                self.append_entries(device, journal_seq, older)?;
                self.append_entries(device, journal_seq, newer)
            }
            other => other,
        }
    }

    /// The value stored for `key`, reading the newest run's index block (in
    /// memory) and one data block per run until a run holds the key; `None`
    /// when absent or whited out.
    pub fn lookup<D: BlockDevice, K: RegionKey>(&self, device: &D, key: &K) -> Result<Option<Value>, StorageError> {
        for run in self.runs.iter().rev() {
            run.key_format.validate::<K>()?;
            let Some(block_index) = run.block_for(key)? else {
                continue;
            };
            let buffer = self.read_data_block(device, run, block_index)?;
            if let Some(found) = self.find_in_block(device, run, buffer, key)? {
                return Ok(found);
            }
        }
        Ok(None)
    }

    /// Every live entry, newest run winning, whiteouts dropped, in key order.
    pub fn entries<D: BlockDevice, K: RegionKey>(&self, device: &D) -> Result<Vec<(K, Vec<u8>)>, StorageError> {
        let mut merged: BTreeMap<K, Option<Vec<u8>>> = BTreeMap::new();
        for run in &self.runs {
            run.key_format.validate::<K>()?;
            for (block_index, block) in run.blocks.iter().enumerate() {
                if block.flags & RUN_BLOCK_FLAG_VALUE != 0 {
                    continue;
                }
                let buffer = self.read_data_block(device, run, block_index)?;
                for (key, value) in self.block_entries::<D, K>(device, run, &buffer)? {
                    merged.insert(key, value.map(|value| value.to_vec()));
                }
            }
        }
        Ok(merged
            .into_iter()
            .filter_map(|(key, value)| value.map(|value| (key, value)))
            .collect())
    }

    fn read_data_block<D: BlockDevice>(&self, device: &D, run: &RunIndex, block_index: usize) -> Result<Vec<u64>, StorageError> {
        let buffer = read_blocks(device, self.offset, run.first_data_block + block_index as u32, 1)?;
        verify(run.blocks[block_index].checksum, checksum64(bytemuck::cast_slice(&buffer)))?;
        Ok(buffer)
    }

    /// Decode every entry of an entry block: key and value, `None` for a whiteout.
    fn block_entries<D: BlockDevice, K: RegionKey>(
        &self,
        device: &D,
        run: &RunIndex,
        buffer: &[u64],
    ) -> Result<Vec<(K, Option<Value>)>, StorageError> {
        let bytes: &[u8] = bytemuck::cast_slice(buffer);
        let head: DataBlockHead = bytemuck::pod_read_unaligned(&bytes[..8]);
        let count = head.entry_count as usize;
        let key_width = run.key_format.key_width();
        let variable = run.key_format.head.value_kind == VALUE_KIND_VARIABLE;
        let value_area = head.value_area as usize;
        if value_area != entry_block_head_len(count, key_width, variable) || value_area > BLOCK_SIZE {
            return Err(StorageError::RegionCorrupt("entry block head disagrees with its count"));
        }
        let flags_at = 8 + count * key_width;
        let ends_at = flags_at + count;
        let fixed_len = run.key_format.head.fixed_value_len as usize;
        let mut out = Vec::with_capacity(count);
        let mut value_start = value_area;
        for slot in 0..count {
            let key = run.key_format.decode_key::<K>(&bytes[8 + slot * key_width..8 + (slot + 1) * key_width])?;
            let flags = bytes[flags_at + slot];
            let value_end = if variable {
                u16::from_le_bytes([bytes[ends_at + slot * 2], bytes[ends_at + slot * 2 + 1]]) as usize
            } else if flags & RUN_ENTRY_FLAG_LARGE != 0 {
                value_start + 8
            } else {
                value_start + fixed_len
            };
            if value_end < value_start || value_end > BLOCK_SIZE {
                return Err(StorageError::RegionCorrupt("value end outside its block"));
            }
            let value = if flags & RUN_ENTRY_FLAG_WHITEOUT != 0 {
                None
            } else if flags & RUN_ENTRY_FLAG_LARGE != 0 {
                let value_ref: LargeValueRef = bytemuck::pod_read_unaligned(&bytes[value_start..value_start + 8]);
                Some(self.read_large_value(device, run, value_ref)?)
            } else {
                Some(Value {
                    buffer: buffer.to_vec(),
                    start: value_start,
                    len: value_end - value_start,
                })
            };
            out.push((key, value));
            value_start = if variable {
                align8(value_end)
            } else {
                value_start + inline_value_len(&RunEntry::Whiteout, flags & RUN_ENTRY_FLAG_LARGE != 0, Some(fixed_len))
            };
        }
        Ok(out)
    }

    /// The entry for `key` in a loaded entry block: `Some(None)` for a
    /// whiteout, `None` when the block does not hold the key.
    fn find_in_block<D: BlockDevice, K: RegionKey>(
        &self,
        device: &D,
        run: &RunIndex,
        buffer: Vec<u64>,
        key: &K,
    ) -> Result<Option<Option<Value>>, StorageError> {
        // shortcut: decodes the block's entries and searches them; a binary
        // search over the encoded keys in place would skip decoding values.
        let entries = self.block_entries::<D, K>(device, run, &buffer)?;
        Ok(entries
            .binary_search_by(|(candidate, _)| candidate.cmp(key))
            .ok()
            .map(|position| entries[position].1.clone()))
    }

    fn read_large_value<D: BlockDevice>(&self, device: &D, run: &RunIndex, value_ref: LargeValueRef) -> Result<Value, StorageError> {
        let first = value_ref.first_block as usize;
        let count = value_ref.block_count as usize;
        if first + count > run.blocks.len() || value_ref.length as usize > count * BLOCK_SIZE {
            return Err(StorageError::RegionCorrupt("large value outside its run"));
        }
        let buffer = read_blocks(device, self.offset, run.first_data_block + first as u32, count as u32)?;
        let bytes: &[u8] = bytemuck::cast_slice(&buffer);
        for block in 0..count {
            verify(
                run.blocks[first + block].checksum,
                checksum64(&bytes[block * BLOCK_SIZE..(block + 1) * BLOCK_SIZE]),
            )?;
        }
        Ok(Value {
            buffer,
            start: 0,
            len: value_ref.length as usize,
        })
    }
}

impl RunIndex {
    /// The entry block that can hold `key`: the last whose fence is ≤ `key`.
    fn block_for<K: RegionKey>(&self, key: &K) -> Result<Option<usize>, StorageError> {
        let key_width = self.key_format.key_width();
        let mut chosen = None;
        let entry_blocks: Vec<usize> = (0..self.blocks.len())
            .filter(|&index| self.blocks[index].flags & RUN_BLOCK_FLAG_VALUE == 0)
            .collect();
        let (mut low, mut high) = (0usize, entry_blocks.len());
        while low < high {
            let middle = (low + high) / 2;
            let block = entry_blocks[middle];
            let fence: K = self
                .key_format
                .decode_key(&self.fences[block * key_width..(block + 1) * key_width])?;
            if fence <= *key {
                chosen = Some(block);
                low = middle + 1;
            } else {
                high = middle;
            }
        }
        Ok(chosen)
    }
}

#[cfg(test)]
mod tests;
