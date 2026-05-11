//! IMPL §1.5.6 packed-key codec.
//!
//! A sorted run's keys are decomposed into a sequence of fixed-count `u64`
//! fields (plus an optional 0..=4 byte per-key header). For each field we
//! record `(bit_width, base, flags)` in a [`SortedRunKeyFormat`] descriptor;
//! per-key encoding writes only `bit_width[i]` bits of `(field[i] - base[i])`
//! into a tightly packed bit stream. Values share a leading
//! `common_value_prefix` of bytes that is recorded once in the descriptor
//! (both as a length and as the actual prefix bytes — see the trailing
//! `value_prefix` slot) and elided from each per-entry value tail.
//!
//! The wire layout of a packed sorted-run payload is:
//!
//! ```text
//! [SortedRunKeyFormat head (8 bytes)]
//! [FieldFormat * nr_fields                       (16 bytes each)]
//! [value_prefix bytes                            (common_value_prefix bytes)]
//! [packed_key_0 | value_tail_0 | packed_key_1 | value_tail_1 | ...]
//! ```
//!
//! Each `packed_key_i` is `key_header_bytes + ceil(sum_data_bit_width / 8)`
//! bytes long, where `sum_data_bit_width` is the sum of every field's
//! `bit_width` (i.e. the descriptor's `sum_bit_width` minus the per-key
//! header bits — `sum_bit_width` is informational only). Each `value_tail_i`
//! is `value_size - common_value_prefix` bytes long; the decoder
//! reconstructs entry *i*'s full value as `value_prefix || value_tail_i`
//! without any caller-supplied state.
//!
//! ## Critical invariant — direct compare on packed bytes
//!
//! Per the spec, binary search within a sorted run compares packed-key byte
//! slices **without decoding**. Base subtraction is strictly monotonic, so
//! the packed ordering matches the unpacked ordering. The codec preserves
//! this by:
//!
//! - Packing fields in **declaration order** (most-significant field first).
//! - Defaulting to MSB-first bit packing within each per-key byte run, so
//!   that comparing the byte slice lexicographically yields the same result
//!   as comparing the underlying field tuple.
//!
//! ## Variable-size values
//!
//! When a sorted run mixes entries with different value lengths the
//! encoder sets `head.value_size_kind = VALUE_SIZE_KIND_VARINT`, and each
//! per-entry value tail is preceded by an unsigned LEB128 varint giving
//! the *elided-tail* byte count. The decoder branches on the same field;
//! reconstructed values are byte-exact in both modes.
//!
//! [`select_format`] picks the mode automatically: it returns
//! `VALUE_SIZE_KIND_FIXED` when every entry's value has the same length
//! (the common case, byte-identical to the pre-amendment encoding) and
//! `VALUE_SIZE_KIND_VARINT` otherwise. Callers don't have to know in
//! advance whether their values are uniform.

use crate::btree_node::{
    FIELD_FORMAT_FLAG_MSB_FIRST, FIELD_FORMAT_FLAG_SIGNED, FieldFormat, SortedRunKeyFormat,
    VALUE_SIZE_KIND_FIXED, VALUE_SIZE_KIND_VARINT,
};

// ---------- PackError ----------

/// Errors emitted by the packed-key codec.
#[derive(Debug, thiserror::Error)]
pub enum PackError {
    /// More fields requested than the spec's hard limit (8).
    #[error("too many fields: {0} (max 8)")]
    TooManyFields(usize),

    /// The bit width recorded in the format descriptor cannot represent the
    /// caller's field value. Promotion to a wider format is required.
    #[error("field width overflow: value {value:#x} exceeds bit_width {bit_width}")]
    FieldOverflow { value: u64, bit_width: u8 },

    /// The packed payload is structurally malformed (truncated, oversize
    /// header, …).
    #[error("malformed packed payload: {0}")]
    Malformed(&'static str),

    /// The caller supplied a `common_value_prefix` longer than every value.
    #[error("invalid common value prefix: {0}")]
    InvalidPrefix(usize),

    /// `key_header_bytes` outside the spec-allowed 0..=4 range.
    #[error("invalid key_header_bytes: {0} (must be 0..=4)")]
    InvalidKeyHeaderBytes(u8),
}

// `From<PackError> for StorageError` is provided via thiserror's `#[from]`
// on `StorageError::Pack`.

// ---------- FieldHints ----------

/// Sign / byte-order hints for a single field, supplied by [`PackableKey`]
/// implementations to drive format selection. Mirrors the per-field flag bits
/// that end up in [`FieldFormat::flags`].
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FieldHints {
    /// Field is signed (two's complement). Format selection still operates
    /// on the bit-pattern u64; the flag is recorded so the decoder can
    /// reinterpret if needed.
    pub signed: bool,
    /// Pack this field MSB-first (big-endian) within the bit stream so that
    /// byte-wise compare matches the natural multi-byte encoding (e.g. for
    /// `NormalisedKey` UTF-8 prefixes). Default: clear (LE / monotonic
    /// integer).
    pub msb_first: bool,
}

impl FieldHints {
    pub const fn unsigned() -> Self {
        Self {
            signed: false,
            msb_first: false,
        }
    }
    pub const fn signed() -> Self {
        Self {
            signed: true,
            msb_first: false,
        }
    }
    pub const fn unsigned_msb() -> Self {
        Self {
            signed: false,
            msb_first: true,
        }
    }

    fn to_flags(self) -> u8 {
        let mut f = 0u8;
        if self.signed {
            f |= FIELD_FORMAT_FLAG_SIGNED;
        }
        if self.msb_first {
            f |= FIELD_FORMAT_FLAG_MSB_FIRST;
        }
        f
    }
}

// ---------- PackableKey ----------

/// Key types that can be encoded into the §1.5.6 packed-key format.
///
/// The trait describes a key's *shape* — how many `u64` fields it
/// decomposes into, an optional per-key 1..=4 byte type/flags discriminator,
/// and per-field sign / byte-order hints. The codec uses that shape to drive
/// format selection ([`select_format`]), encoding ([`encode_packed_run`]),
/// and decoding ([`decode_packed_run`]).
pub trait PackableKey: Sized {
    /// Number of fields this key decomposes into. Must be in `1..=8`.
    fn nr_fields() -> usize;

    /// Number of header bytes (`0..=4`). 0 means no per-key header.
    fn key_header_bytes() -> usize {
        0
    }

    /// Per-field sign / byte-order hints. Length must equal
    /// [`Self::nr_fields()`].
    fn field_hints() -> &'static [FieldHints];

    /// Per-key type / flags discriminator. Returned as a `u32` of which only
    /// the low [`Self::key_header_bytes()`] bytes are written.
    fn key_header(&self) -> u32 {
        0
    }

    /// Decompose `self` into [`Self::nr_fields()`] `u64` values, in field
    /// declaration order. `out.len()` is guaranteed to equal
    /// [`Self::nr_fields()`].
    ///
    /// Signed fields are conveyed as their two's-complement bit pattern
    /// (i.e. `i64::cast_unsigned()`); the encoder treats the value as an
    /// unsigned bit pattern for `(value - base)` arithmetic and the decoder
    /// reinterprets via [`Self::from_components`].
    fn field_values(&self, out: &mut [u64]);

    /// Reconstruct from the per-key header and field-value tuple. Inverse of
    /// [`Self::key_header`] + [`Self::field_values`].
    fn from_components(header: u32, fields: &[u64]) -> Result<Self, PackError>;
}

// ---- Primitive impls -----------------------------------------------------

const HINTS_1_UNSIGNED: [FieldHints; 1] = [FieldHints::unsigned()];
const HINTS_2_UNSIGNED: [FieldHints; 2] = [FieldHints::unsigned(), FieldHints::unsigned()];

impl PackableKey for u64 {
    fn nr_fields() -> usize {
        1
    }
    fn field_hints() -> &'static [FieldHints] {
        &HINTS_1_UNSIGNED
    }
    fn field_values(&self, out: &mut [u64]) {
        out[0] = *self;
    }
    fn from_components(_header: u32, fields: &[u64]) -> Result<Self, PackError> {
        if fields.len() != 1 {
            return Err(PackError::Malformed("u64: wrong field count"));
        }
        Ok(fields[0])
    }
}

impl PackableKey for u32 {
    fn nr_fields() -> usize {
        1
    }
    fn field_hints() -> &'static [FieldHints] {
        &HINTS_1_UNSIGNED
    }
    fn field_values(&self, out: &mut [u64]) {
        out[0] = *self as u64;
    }
    fn from_components(_header: u32, fields: &[u64]) -> Result<Self, PackError> {
        if fields.len() != 1 {
            return Err(PackError::Malformed("u32: wrong field count"));
        }
        if fields[0] > u32::MAX as u64 {
            return Err(PackError::Malformed("u32: field overflow on decode"));
        }
        Ok(fields[0] as u32)
    }
}

impl PackableKey for (u64, u32) {
    fn nr_fields() -> usize {
        2
    }
    fn field_hints() -> &'static [FieldHints] {
        &HINTS_2_UNSIGNED
    }
    fn field_values(&self, out: &mut [u64]) {
        out[0] = self.0;
        out[1] = self.1 as u64;
    }
    fn from_components(_header: u32, fields: &[u64]) -> Result<Self, PackError> {
        if fields.len() != 2 {
            return Err(PackError::Malformed("(u64, u32): wrong field count"));
        }
        if fields[1] > u32::MAX as u64 {
            return Err(PackError::Malformed("(u64, u32): u32 field overflow"));
        }
        Ok((fields[0], fields[1] as u32))
    }
}

impl PackableKey for (u32, u32) {
    fn nr_fields() -> usize {
        2
    }
    fn field_hints() -> &'static [FieldHints] {
        &HINTS_2_UNSIGNED
    }
    fn field_values(&self, out: &mut [u64]) {
        out[0] = self.0 as u64;
        out[1] = self.1 as u64;
    }
    fn from_components(_header: u32, fields: &[u64]) -> Result<Self, PackError> {
        if fields.len() != 2 {
            return Err(PackError::Malformed("(u32, u32): wrong field count"));
        }
        if fields[0] > u32::MAX as u64 || fields[1] > u32::MAX as u64 {
            return Err(PackError::Malformed("(u32, u32): u32 field overflow"));
        }
        Ok((fields[0] as u32, fields[1] as u32))
    }
}

// ---------- Format selection ----------

/// Inspect a sorted slice of `(K, V)` and choose the most-compact
/// [`SortedRunKeyFormat`] (descriptor head + field array + value prefix)
/// that can encode every entry without overflow.
///
/// For each field `i`:
///
/// - `base[i] = min(field_i)`
/// - `bit_width[i] = ceil(log2(max - min + 1))` (and `0` when `max == min`)
///
/// `common_value_prefix` is computed by counting leading bytes that are
/// bit-for-bit identical across every entry's value, bounded by the
/// shortest value's length and by the `u8` representable range
/// (`0..=255`). The matching prefix bytes themselves are returned as the
/// third tuple element so callers can persist them in the descriptor's
/// trailing `value_prefix` slot (IMPL §1.5.6).
///
/// **Edge cases:**
///
/// - Empty input returns an all-zero descriptor with the trait-derived
///   `nr_fields` / `key_header_bytes` and an empty prefix. Encode then
///   becomes a no-op.
/// - Single entry returns `bit_width = 0` for every field; one entry
///   encodes as zero packed bits with the entire value (capped at 255 B)
///   captured in the prefix.
pub fn select_format<K: PackableKey, V: AsRef<[u8]>>(
    entries: &[(K, V)],
) -> Result<(SortedRunKeyFormat, Vec<FieldFormat>, Vec<u8>), PackError> {
    select_format_with_hint(entries, false)
}

/// Variant of [`select_format`] that lets the caller force VARINT mode.
///
/// When `force_varint` is `true` the returned descriptor's
/// `value_size_kind` is always [`VALUE_SIZE_KIND_VARINT`], regardless of
/// whether the values happen to share a length. This is the right choice
/// for values whose logical shape is variable-length (e.g. IMPL §7.1's
/// `LeafEntry`, which switches between an inline `[PackedAssertion; N]`
/// body and a 16 B `BlockRef` spill ref): without the hint, the default
/// auto-detection forces FIXED for single-entry runs, and the FIXED
/// decoder needs the caller to know the value size up-front — which we
/// don't, in the variable-shape case.
pub fn select_format_with_hint<K: PackableKey, V: AsRef<[u8]>>(
    entries: &[(K, V)],
    force_varint: bool,
) -> Result<(SortedRunKeyFormat, Vec<FieldFormat>, Vec<u8>), PackError> {
    let nr = K::nr_fields();
    if nr == 0 || nr > 8 {
        return Err(PackError::TooManyFields(nr));
    }
    let header_bytes = K::key_header_bytes();
    if header_bytes > 4 {
        return Err(PackError::InvalidKeyHeaderBytes(header_bytes as u8));
    }
    let hints = K::field_hints();
    if hints.len() != nr {
        return Err(PackError::Malformed(
            "PackableKey: hints length != nr_fields",
        ));
    }

    // Compute per-field min/max.
    let mut mins = vec![u64::MAX; nr];
    let mut maxs = vec![0u64; nr];
    let mut scratch = vec![0u64; nr];
    let mut have_any = false;

    if entries.is_empty() {
        for m in mins.iter_mut() {
            *m = 0;
        }
    } else {
        for (k, _) in entries {
            k.field_values(&mut scratch);
            for i in 0..nr {
                if scratch[i] < mins[i] {
                    mins[i] = scratch[i];
                }
                if scratch[i] > maxs[i] {
                    maxs[i] = scratch[i];
                }
            }
            have_any = true;
        }
    }

    let mut fields = Vec::with_capacity(nr);
    let mut sum_bit_width: u32 = (header_bytes as u32) * 8;
    for i in 0..nr {
        let (bw, base) = if !have_any || mins[i] == maxs[i] {
            (0u8, if have_any { mins[i] } else { 0 })
        } else {
            let span = maxs[i] - mins[i];
            // bit_width = ceil(log2(span + 1)) — i.e. the number of bits
            // needed to represent values in [0, span] inclusive.
            // span >= 1 here (since min != max).
            let bw = 64 - span.leading_zeros();
            (bw as u8, mins[i])
        };
        sum_bit_width += bw as u32;
        fields.push(FieldFormat {
            bit_width: bw,
            flags: hints[i].to_flags(),
            _pad0: 0,
            base,
            _pad1: 0,
        });
    }

    // Common value prefix: count leading bytes shared across every entry,
    // bounded by the shortest value length and the u8 representable range.
    let (common_prefix_len, prefix_bytes) = if entries.is_empty() {
        (0u8, Vec::new())
    } else {
        let first = entries[0].1.as_ref();
        let max_len = entries
            .iter()
            .map(|(_, v)| v.as_ref().len())
            .min()
            .unwrap_or(0)
            .min(u8::MAX as usize);
        let mut count = 0usize;
        'outer: for (j, &b) in first.iter().enumerate().take(max_len) {
            for (_, v) in entries.iter().skip(1) {
                if v.as_ref()[j] != b {
                    break 'outer;
                }
            }
            count = j + 1;
        }
        (count as u8, first[..count].to_vec())
    };

    // Mode selection: fixed if every value has the same length (the common
    // case), varint otherwise. Empty input → fixed (degenerate). The
    // caller may force VARINT for value types whose logical shape is
    // variable-length (see [`select_format_with_hint`]'s docs).
    let value_size_kind = if force_varint && !entries.is_empty() {
        VALUE_SIZE_KIND_VARINT
    } else if entries.len() < 2 {
        VALUE_SIZE_KIND_FIXED
    } else {
        let first_len = entries[0].1.as_ref().len();
        if entries.iter().all(|(_, v)| v.as_ref().len() == first_len) {
            VALUE_SIZE_KIND_FIXED
        } else {
            VALUE_SIZE_KIND_VARINT
        }
    };

    let head = SortedRunKeyFormat {
        nr_fields: nr as u8,
        key_header_bytes: header_bytes as u8,
        common_value_prefix: common_prefix_len,
        value_size_kind,
        sum_bit_width,
    };
    Ok((head, fields, prefix_bytes))
}

// ---------- Varint (unsigned LEB128) helpers ----------

/// Append `value` as an unsigned LEB128 varint to `buf`. Standard
/// 7-data-bits-per-byte encoding with the MSB of each byte signalling
/// continuation.
fn write_varint_u64(buf: &mut Vec<u8>, mut value: u64) {
    loop {
        let byte = (value & 0x7F) as u8;
        value >>= 7;
        if value == 0 {
            buf.push(byte);
            return;
        }
        buf.push(byte | 0x80);
    }
}

/// Read an unsigned LEB128 varint starting at `*pos` from `buf`. Advances
/// `*pos` past the varint on success. Errors on truncation or on widths
/// beyond the `u64` representable range.
fn read_varint_u64(buf: &[u8], pos: &mut usize) -> Result<u64, PackError> {
    let mut result: u64 = 0;
    let mut shift: u32 = 0;
    loop {
        if *pos >= buf.len() {
            return Err(PackError::Malformed("varint: truncated"));
        }
        let byte = buf[*pos];
        *pos += 1;
        // Detect a final byte that would overflow u64. The varint is at
        // most 10 bytes long; the final byte may carry at most 1 bit.
        if shift >= 64 {
            return Err(PackError::Malformed("varint: too wide for u64"));
        }
        let chunk = (byte & 0x7F) as u64;
        if shift == 63 && chunk > 1 {
            return Err(PackError::Malformed("varint: too wide for u64"));
        }
        result |= chunk << shift;
        if byte & 0x80 == 0 {
            return Ok(result);
        }
        shift += 7;
    }
}

// ---------- Bit packing helpers ----------

/// Append `bit_width` bits of `value` (LSB-first within `value`) to a
/// big-endian-within-byte bit buffer at `*bit_pos`.
///
/// "Big-endian within byte" means bit position 0 (the first bit written)
/// occupies the **most-significant** bit of byte 0; this is what makes
/// byte-wise compare match the natural numeric ordering of the packed run.
/// The bit ordering within `value` itself is most-significant-bit first
/// (i.e. we take `value`'s top `bit_width` bits in MSB-first order).
fn pack_bits(buf: &mut Vec<u8>, bit_pos: &mut usize, value: u64, bit_width: u8) {
    if bit_width == 0 {
        return;
    }
    let bw = bit_width as usize;
    debug_assert!(bw <= 64);
    // Iterate from MSB of `value` (bit `bw - 1`) down to bit 0.
    for i in (0..bw).rev() {
        let bit = ((value >> i) & 1) as u8;
        let byte_idx = *bit_pos / 8;
        let bit_in_byte = 7 - (*bit_pos % 8); // big-endian within byte
        if byte_idx >= buf.len() {
            buf.push(0);
        }
        if bit != 0 {
            buf[byte_idx] |= 1 << bit_in_byte;
        }
        *bit_pos += 1;
    }
}

/// Read `bit_width` bits starting at `*bit_pos` from a big-endian-within-byte
/// bit buffer; advance `*bit_pos`.
fn read_bits(buf: &[u8], bit_pos: &mut usize, bit_width: u8) -> Result<u64, PackError> {
    if bit_width == 0 {
        return Ok(0);
    }
    let bw = bit_width as usize;
    debug_assert!(bw <= 64);
    let end_bit = *bit_pos + bw;
    if end_bit > buf.len() * 8 {
        return Err(PackError::Malformed("read_bits: out of range"));
    }
    let mut out: u64 = 0;
    for _ in 0..bw {
        let byte_idx = *bit_pos / 8;
        let bit_in_byte = 7 - (*bit_pos % 8);
        let bit = (buf[byte_idx] >> bit_in_byte) & 1;
        out = (out << 1) | (bit as u64);
        *bit_pos += 1;
    }
    Ok(out)
}

/// Helper: number of data bits per packed key (i.e. the sum of every field's
/// `bit_width`). Excludes the per-key header bytes.
fn data_bits_per_key(fields: &[FieldFormat]) -> usize {
    fields
        .iter()
        .map(|f| {
            let bw = { f.bit_width };
            bw as usize
        })
        .sum()
}

/// Helper: total per-key body length in bytes (header + packed fields rounded
/// up to a byte).
fn bytes_per_key(head: &SortedRunKeyFormat, fields: &[FieldFormat]) -> usize {
    let header_bytes = { head.key_header_bytes } as usize;
    let data_bits = data_bits_per_key(fields);
    header_bytes + data_bits.div_ceil(8)
}

// ---------- Encoder ----------

/// Encode a sorted run's entries under the given format. Output is the
/// payload bytes that go into a sorted run's payload region (after the
/// `SortedRunHeader`). The format descriptor (header + fields +
/// `value_prefix`) is laid out at the **start** of the payload so the
/// decoder can read it without out-of-band state.
///
/// `value_prefix.len()` must equal `head.common_value_prefix`, and every
/// entry's value must begin with exactly those bytes. The encoder strips
/// those leading bytes from each value tail.
///
/// **Value-size mode** is selected by `head.value_size_kind`:
///
/// - `VALUE_SIZE_KIND_FIXED` — every entry's value must have the same
///   length; the value tail is written verbatim with no per-entry prefix.
/// - `VALUE_SIZE_KIND_VARINT` — values may have arbitrary, mixed lengths;
///   each entry is preceded by an unsigned LEB128 varint giving the
///   *elided-tail* byte count.
///
/// In both modes every entry must satisfy `value.len() ≥
/// common_value_prefix` and the leading prefix bytes must match
/// `value_prefix`.
pub fn encode_packed_run<K: PackableKey, V: AsRef<[u8]>>(
    entries: &[(K, V)],
    head: &SortedRunKeyFormat,
    fields: &[FieldFormat],
    value_prefix: &[u8],
) -> Result<Vec<u8>, PackError> {
    let nr = K::nr_fields();
    if fields.len() != nr {
        return Err(PackError::Malformed(
            "encode: format field count mismatch with PackableKey",
        ));
    }
    let header_bytes = { head.key_header_bytes } as usize;
    let prefix = { head.common_value_prefix } as usize;
    let value_size_kind = { head.value_size_kind };
    if value_prefix.len() != prefix {
        return Err(PackError::InvalidPrefix(value_prefix.len()));
    }
    if value_size_kind != VALUE_SIZE_KIND_FIXED && value_size_kind != VALUE_SIZE_KIND_VARINT {
        return Err(PackError::Malformed("encode: unknown value_size_kind"));
    }

    // Pre-allocate output. The format descriptor goes first (head + fields +
    // value_prefix tail).
    let mut out = Vec::new();
    head.serialise(fields, value_prefix, &mut out);

    if entries.is_empty() {
        return Ok(out);
    }

    // Fixed-mode invariant: every value has the same length.
    if value_size_kind == VALUE_SIZE_KIND_FIXED {
        let value_size = entries[0].1.as_ref().len();
        if entries.iter().any(|(_, v)| v.as_ref().len() != value_size) {
            return Err(PackError::Malformed(
                "encode: fixed mode requires uniform value lengths; \
                 select_format should have chosen VARINT",
            ));
        }
    }

    // Both modes: prefix length must not exceed any value, and the leading
    // bytes of every value must match `value_prefix`.
    for (_, v) in entries.iter() {
        let v = v.as_ref();
        if v.len() < prefix {
            return Err(PackError::InvalidPrefix(prefix));
        }
        if prefix > 0 && &v[..prefix] != value_prefix {
            return Err(PackError::Malformed(
                "encode: common_value_prefix does not match every value",
            ));
        }
    }

    let body_size = bytes_per_key(head, fields);
    // Reserve a sensible amount; varint mode is hard to size precisely.
    out.reserve(entries.len() * body_size);

    let mut field_buf = vec![0u64; nr];
    for (k, v) in entries {
        // Per-key header: low `header_bytes` of `k.key_header()` little-endian.
        if header_bytes > 0 {
            let h = k.key_header();
            for b in 0..header_bytes {
                out.push(((h >> (b * 8)) & 0xff) as u8);
            }
        }

        // Bit-packed fields.
        let mut packed = Vec::<u8>::new();
        let mut bit_pos = 0usize;
        k.field_values(&mut field_buf);
        for i in 0..nr {
            let f = &fields[i];
            let bw = { f.bit_width };
            if bw == 0 {
                // Constant field — base must equal the value.
                let base = { f.base };
                if field_buf[i] != base {
                    return Err(PackError::FieldOverflow {
                        value: field_buf[i],
                        bit_width: 0,
                    });
                }
                continue;
            }
            let base = { f.base };
            // Use wrapping_sub: signed fields rely on two's-complement
            // semantics where (value - base) is the unsigned bit pattern
            // that the bit packer consumes. This is monotonic for bases
            // chosen as min(field) over an ordered range.
            let delta = field_buf[i].wrapping_sub(base);
            // Range check.
            let max_repr: u64 = if bw == 64 { u64::MAX } else { (1u64 << bw) - 1 };
            if delta > max_repr {
                return Err(PackError::FieldOverflow {
                    value: field_buf[i],
                    bit_width: bw,
                });
            }
            pack_bits(&mut packed, &mut bit_pos, delta, bw);
        }
        // Pad packed buffer to declared bytes_per_key data length.
        let data_bytes = body_size - header_bytes;
        while packed.len() < data_bytes {
            packed.push(0);
        }
        out.extend_from_slice(&packed);

        // Value tail (prefix elided). In varint mode emit the length first.
        let tail = &v.as_ref()[prefix..];
        if value_size_kind == VALUE_SIZE_KIND_VARINT {
            write_varint_u64(&mut out, tail.len() as u64);
        }
        out.extend_from_slice(tail);
    }

    Ok(out)
}

// ---------- Decoder ----------

/// Output of [`decode_packed_run`]: per-entry `(key, value)` pairs (with
/// values fully reconstructed including the elided prefix), plus the
/// parsed format descriptor and field array.
pub type DecodedRun<K> = (Vec<(K, Vec<u8>)>, SortedRunKeyFormat, Vec<FieldFormat>);

/// Decode a packed sorted-run payload.
///
/// `value_size` is the *total* per-entry value size in bytes (before
/// prefix elision) for runs whose descriptor says `value_size_kind = 0`
/// (fixed). For varint-mode runs the per-entry length is on the wire and
/// `value_size` is ignored. `entry_count` matches
/// `SortedRunHeader.entry_count`.
///
/// The descriptor's `value_prefix` tail is read directly from the payload
/// and prepended to each entry's value tail, so reconstruction needs no
/// out-of-band state.
pub fn decode_packed_run<K: PackableKey>(
    payload: &[u8],
    value_size: usize,
    entry_count: u32,
) -> Result<DecodedRun<K>, PackError> {
    let (head, fields, value_prefix) = SortedRunKeyFormat::parse(payload)
        .map_err(|_| PackError::Malformed("decode: format header truncated"))?;
    let nr = { head.nr_fields } as usize;
    if nr != K::nr_fields() {
        return Err(PackError::Malformed(
            "decode: descriptor nr_fields mismatch with PackableKey",
        ));
    }
    let header_bytes = { head.key_header_bytes } as usize;
    let prefix = { head.common_value_prefix } as usize;
    let value_size_kind = { head.value_size_kind };
    debug_assert_eq!(value_prefix.len(), prefix);
    let body_size = bytes_per_key(&head, &fields);

    let after_format = head.total_size();
    let entries_bytes = &payload[after_format..];

    // Up-front bounds check is mode-specific:
    // - fixed: stride = body + (value_size − prefix) is constant.
    // - varint: per-entry length on the wire; checked entry-by-entry below.
    match value_size_kind {
        VALUE_SIZE_KIND_FIXED => {
            if prefix > value_size {
                return Err(PackError::InvalidPrefix(prefix));
            }
            let value_tail_size = value_size - prefix;
            let stride = body_size + value_tail_size;
            let needed = (entry_count as usize) * stride;
            if entries_bytes.len() < needed {
                return Err(PackError::Malformed("decode: entries truncated"));
            }
        }
        VALUE_SIZE_KIND_VARINT => {}
        _ => return Err(PackError::Malformed("decode: unknown value_size_kind")),
    }

    let mut out: Vec<(K, Vec<u8>)> = Vec::with_capacity(entry_count as usize);
    let mut cursor = 0usize;
    let mut field_vals = vec![0u64; nr];
    for _ in 0..entry_count {
        // Header.
        let data_bytes = body_size - header_bytes;
        if cursor + body_size > entries_bytes.len() {
            return Err(PackError::Malformed("decode: entry body truncated"));
        }
        let mut h: u32 = 0;
        for b in 0..header_bytes {
            h |= (entries_bytes[cursor + b] as u32) << (b * 8);
        }
        cursor += header_bytes;
        // Packed fields.
        let packed = &entries_bytes[cursor..cursor + data_bytes];
        cursor += data_bytes;
        let mut bit_pos = 0usize;
        for i in 0..nr {
            let f = &fields[i];
            let bw = { f.bit_width };
            let base = { f.base };
            let delta = read_bits(packed, &mut bit_pos, bw)?;
            field_vals[i] = base.wrapping_add(delta);
        }

        // Value tail. Length depends on mode.
        let tail_len = match value_size_kind {
            VALUE_SIZE_KIND_FIXED => value_size - prefix,
            VALUE_SIZE_KIND_VARINT => {
                read_varint_u64(entries_bytes, &mut cursor)? as usize
            }
            _ => unreachable!("value_size_kind validated above"),
        };
        if cursor + tail_len > entries_bytes.len() {
            return Err(PackError::Malformed("decode: value tail truncated"));
        }
        let value_tail = &entries_bytes[cursor..cursor + tail_len];
        cursor += tail_len;

        // Reconstruct full value: prefix bytes from the descriptor, then
        // the per-entry value tail.
        let mut full_value = Vec::with_capacity(prefix + tail_len);
        full_value.extend_from_slice(&value_prefix);
        full_value.extend_from_slice(value_tail);

        let key = K::from_components(h, &field_vals)?;
        out.push((key, full_value));
    }

    Ok((out, head, fields))
}

// ---------- Compare on packed bytes ----------

/// Compare two packed-key byte slices. The encoder's bit ordering
/// (most-significant first within each byte, fields packed in declaration
/// order) is chosen so this byte-wise lexicographic compare matches the
/// underlying field-tuple compare, *provided* every field uses the default
/// LE flag (i.e. `FIELD_FORMAT_FLAG_MSB_FIRST` clear) — for ordinary
/// monotonic integer fields, base subtraction keeps order intact and the
/// MSB-first packing makes the bytes sort the same way.
///
/// For `FIELD_FORMAT_FLAG_MSB_FIRST` fields (e.g. UTF-8 prefixes), the
/// caller is responsible for ensuring the field hint reflects the natural
/// big-endian layout; the codec then writes the bits in a byte order that
/// preserves byte-wise compare.
pub fn compare_packed_keys(a: &[u8], b: &[u8]) -> std::cmp::Ordering {
    a.cmp(b)
}

// ---------- Format fit / promotion ----------

/// Outcome of [`check_fit`].
#[derive(Clone, Debug)]
pub enum FormatFit {
    /// The new key fits the existing format. Append normally.
    Fits,
    /// The new key needs at least one field widened (and/or a new base).
    /// The existing run can continue under the existing format (older
    /// entries unchanged); the next flush should choose between rewriting
    /// under a wider format or triggering full compaction.
    NeedsPromotion(SortedRunKeyFormat, Vec<FieldFormat>),
}

/// Check whether `key` fits the existing `(head, fields)` format. Returns
/// [`FormatFit::Fits`] if so, or [`FormatFit::NeedsPromotion`] with a
/// suggested wider format that subsumes the existing format and accepts
/// the new key.
pub fn check_fit<K: PackableKey>(
    head: &SortedRunKeyFormat,
    fields: &[FieldFormat],
    key: &K,
) -> Result<FormatFit, PackError> {
    let nr = K::nr_fields();
    if fields.len() != nr {
        return Err(PackError::Malformed(
            "check_fit: format field count mismatch with PackableKey",
        ));
    }
    if K::key_header_bytes() != ({ head.key_header_bytes } as usize) {
        return Err(PackError::InvalidKeyHeaderBytes(head.key_header_bytes));
    }
    let mut buf = vec![0u64; nr];
    key.field_values(&mut buf);

    let mut needs = false;
    let mut new_fields = fields.to_vec();
    for i in 0..nr {
        let f = &fields[i];
        let bw = { f.bit_width };
        let base = { f.base };
        let value = buf[i];
        // Compute new base/width if needed: the new format must accept
        // every value previously representable AND the new key. The cheap
        // strategy: keep base, widen bit_width to fit max(value - base,
        // existing range). Lower the base only if value < base.
        let (new_base, range_max) = if value < base {
            // Lower base to value; existing range upper bound was base + (1 << bw) - 1.
            let old_upper = if bw == 0 {
                base
            } else if bw == 64 {
                u64::MAX
            } else {
                base + (1u64 << bw) - 1
            };
            (value, old_upper - value)
        } else {
            // Keep base; widen if value - base exceeds current max_repr.
            let cur_max: u64 = if bw == 0 {
                0
            } else if bw == 64 {
                u64::MAX
            } else {
                (1u64 << bw) - 1
            };
            let delta = value - base;
            if delta > cur_max {
                (base, delta)
            } else {
                continue; // fits
            }
        };

        // Compute required bit width for `range_max`.
        let req_bw = if range_max == 0 {
            0u8
        } else {
            (64 - range_max.leading_zeros()) as u8
        };
        if req_bw > bw || new_base != base {
            needs = true;
            new_fields[i] = FieldFormat {
                bit_width: req_bw.max(bw),
                flags: { f.flags },
                _pad0: 0,
                base: new_base,
                _pad1: 0,
            };
        }
    }

    if !needs {
        return Ok(FormatFit::Fits);
    }
    let header_bytes = { head.key_header_bytes };
    let sum_bw: u32 = (header_bytes as u32) * 8
        + new_fields
            .iter()
            .map(|f| {
                let bw = { f.bit_width };
                bw as u32
            })
            .sum::<u32>();
    let new_head = SortedRunKeyFormat {
        nr_fields: nr as u8,
        key_header_bytes: header_bytes,
        common_value_prefix: { head.common_value_prefix },
        value_size_kind: VALUE_SIZE_KIND_FIXED,
        sum_bit_width: sum_bw,
    };
    Ok(FormatFit::NeedsPromotion(new_head, new_fields))
}

// ---------- FormatPromote event ----------

/// Storage-layer projection of the `FormatPromote` WAL op (IMPL §3.3).
/// Emitted by the storage layer when a flush widens a sorted run's format;
/// the engine layer is responsible for journalling this as a WAL op so
/// recovery can reconstruct the in-memory sorted run state. The WAL op
/// itself lives in `mimisbrunnr-wal` — this is the shape the storage
/// crate produces for the engine to consume.
#[derive(Clone, Debug)]
pub struct FormatPromoteEvent {
    pub sorted_run_seq: u32,
    pub new_format: SortedRunKeyFormat,
    pub new_fields: Vec<FieldFormat>,
}

// ---------- Tests ----------

#[cfg(test)]
mod tests {
    use super::*;

    // --- PackableKey impls round-trip ---

    #[test]
    fn packable_u64_round_trip() {
        let mut buf = [0u64];
        let k = 0xdead_beefu64;
        PackableKey::field_values(&k, &mut buf);
        assert_eq!(buf[0], 0xdead_beef);
        let r = <u64 as PackableKey>::from_components(0, &buf).unwrap();
        assert_eq!(r, k);
    }

    #[test]
    fn packable_u64_u32_round_trip() {
        let mut buf = [0u64; 2];
        let k: (u64, u32) = (0xcafe_babeu64, 42u32);
        PackableKey::field_values(&k, &mut buf);
        assert_eq!(buf, [0xcafe_babeu64, 42]);
        let r = <(u64, u32) as PackableKey>::from_components(0, &buf).unwrap();
        assert_eq!(r, k);
    }

    #[test]
    fn packable_u32_u32_round_trip() {
        let mut buf = [0u64; 2];
        let k: (u32, u32) = (10, 20);
        PackableKey::field_values(&k, &mut buf);
        let r = <(u32, u32) as PackableKey>::from_components(0, &buf).unwrap();
        assert_eq!(r, k);
    }

    // --- A custom test struct exercising header bytes + multi-field ---

    /// `(kind: u8, scope: u16, oid: u64)` with a 1-byte per-key header
    /// carrying the kind discriminator. Demonstrates the
    /// `key_header_bytes > 0` branch.
    #[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
    struct TestKey {
        kind: u8,
        scope: u16,
        oid: u64,
    }

    impl PackableKey for TestKey {
        fn nr_fields() -> usize {
            2
        }
        fn key_header_bytes() -> usize {
            1
        }
        fn field_hints() -> &'static [FieldHints] {
            const H: [FieldHints; 2] = [FieldHints::unsigned(), FieldHints::unsigned()];
            &H
        }
        fn key_header(&self) -> u32 {
            self.kind as u32
        }
        fn field_values(&self, out: &mut [u64]) {
            out[0] = self.scope as u64;
            out[1] = self.oid;
        }
        fn from_components(header: u32, fields: &[u64]) -> Result<Self, PackError> {
            if fields.len() != 2 {
                return Err(PackError::Malformed("TestKey: wrong field count"));
            }
            Ok(Self {
                kind: header as u8,
                scope: fields[0] as u16,
                oid: fields[1],
            })
        }
    }

    #[test]
    fn packable_test_key_round_trip() {
        let k = TestKey {
            kind: 3,
            scope: 99,
            oid: 0x1234,
        };
        let mut buf = [0u64; 2];
        k.field_values(&mut buf);
        let r = TestKey::from_components(k.key_header(), &buf).unwrap();
        assert_eq!(r, k);
    }

    // --- select_format ---

    #[test]
    fn select_format_empty() {
        let entries: Vec<(u64, Vec<u8>)> = Vec::new();
        let (head, fields, prefix) = select_format(&entries).unwrap();
        assert_eq!({ head.nr_fields }, 1);
        assert_eq!({ head.common_value_prefix }, 0);
        assert!(prefix.is_empty());
        assert_eq!(fields.len(), 1);
        assert_eq!({ fields[0].bit_width }, 0);
        assert_eq!({ fields[0].base }, 0);
    }

    #[test]
    fn select_format_single_entry_zero_widths() {
        let entries: Vec<(u64, Vec<u8>)> = vec![(42u64, vec![1, 2, 3])];
        let (_head, fields, _prefix) = select_format(&entries).unwrap();
        assert_eq!({ fields[0].bit_width }, 0);
        assert_eq!({ fields[0].base }, 42);
    }

    #[test]
    fn select_format_growing_widths() {
        let mut entries = Vec::new();
        for i in 0u64..255 {
            entries.push((i, vec![]));
        }
        let (_head, fields, _prefix) = select_format(&entries).unwrap();
        // Range 0..=254 -> bit_width = 8.
        assert_eq!({ fields[0].bit_width }, 8);
        assert_eq!({ fields[0].base }, 0);
    }

    #[test]
    fn select_format_random_values_falls_back_to_64() {
        let entries: Vec<(u64, Vec<u8>)> = vec![
            (0u64, vec![]),
            (u64::MAX, vec![]),
        ];
        let (_head, fields, _prefix) = select_format(&entries).unwrap();
        assert_eq!({ fields[0].bit_width }, 64);
        assert_eq!({ fields[0].base }, 0);
    }

    #[test]
    fn select_format_constant_field_zero_width() {
        let entries: Vec<((u64, u32), Vec<u8>)> = vec![
            ((0u64, 7u32), vec![]),
            ((1u64, 7u32), vec![]),
            ((100u64, 7u32), vec![]),
        ];
        let (_head, fields, _prefix) = select_format(&entries).unwrap();
        assert_eq!({ fields[1].bit_width }, 0);
        assert_eq!({ fields[1].base }, 7);
        // Field 0 spans 0..=100 — bit_width = 7.
        assert_eq!({ fields[0].bit_width }, 7);
    }

    #[test]
    fn select_format_common_value_prefix() {
        let entries: Vec<(u64, Vec<u8>)> = vec![
            (1u64, vec![0xAB, 0xCD, 0x01, 0x02]),
            (2u64, vec![0xAB, 0xCD, 0x03, 0x04]),
            (3u64, vec![0xAB, 0xCD, 0x05, 0x06]),
        ];
        let (head, _fields, _prefix) = select_format(&entries).unwrap();
        assert_eq!({ head.common_value_prefix }, 2);
    }

    #[test]
    fn select_format_no_common_value_prefix() {
        let entries: Vec<(u64, Vec<u8>)> = vec![
            (1u64, vec![0x01, 0xCD]),
            (2u64, vec![0x02, 0xCD]),
        ];
        let (head, _fields, _prefix) = select_format(&entries).unwrap();
        assert_eq!({ head.common_value_prefix }, 0);
    }

    #[test]
    fn select_format_value_prefix_capped_at_u8_max() {
        // Two identical 300-byte values share their full content. The
        // descriptor's `common_value_prefix` is a u8, so the prefix is
        // capped at 255 even though all 300 bytes match.
        let v = vec![0xAA; 300];
        let entries: Vec<(u64, Vec<u8>)> = vec![
            (1u64, v.clone()),
            (2u64, v.clone()),
        ];
        let (head, _fields, prefix) = select_format(&entries).unwrap();
        assert_eq!({ head.common_value_prefix }, u8::MAX);
        assert_eq!(prefix.len(), u8::MAX as usize);
    }

    #[test]
    fn select_format_long_shared_prefix_below_cap() {
        // 64-byte shared values: prefix is now captured in full (no
        // artificial 24-byte cap as in earlier R1 iterations).
        let v = vec![0xAA; 64];
        let entries: Vec<(u64, Vec<u8>)> = vec![
            (1u64, v.clone()),
            (2u64, v.clone()),
        ];
        let (head, _fields, prefix) = select_format(&entries).unwrap();
        assert_eq!({ head.common_value_prefix }, 64);
        assert_eq!(prefix.len(), 64);
    }

    // --- encode/decode round-trips ---

    #[test]
    fn encode_decode_round_trip_u64_u32() {
        let entries: Vec<((u64, u32), Vec<u8>)> = (0u64..50)
            .map(|i| ((i * 3, 7u32), vec![0xAA, 0xBB, (i & 0xff) as u8, 0]))
            .collect();
        let (head, fields, prefix) = select_format(&entries).unwrap();
        let payload = encode_packed_run(&entries, &head, &fields, &prefix).unwrap();

        let value_size = 4;
        let (decoded, _head, _fields) =
            decode_packed_run::<(u64, u32)>(&payload, value_size, entries.len() as u32)
                .unwrap();
        assert_eq!(decoded.len(), entries.len());
        for (orig, dec) in entries.iter().zip(decoded.iter()) {
            assert_eq!(orig.0, dec.0);
            assert_eq!(orig.1, dec.1);
        }
    }

    #[test]
    fn encode_empty() {
        let entries: Vec<(u64, Vec<u8>)> = Vec::new();
        let (head, fields, prefix) = select_format(&entries).unwrap();
        let payload = encode_packed_run(&entries, &head, &fields, &prefix).unwrap();
        // Just the format header.
        assert_eq!(payload.len(), head.total_size());
    }

    #[test]
    fn encode_with_test_key_header_bytes() {
        let entries: Vec<(TestKey, Vec<u8>)> = (0u64..10)
            .map(|i| {
                (
                    TestKey {
                        kind: 5,
                        scope: 1,
                        oid: i,
                    },
                    vec![1u8, 2, 3, 4],
                )
            })
            .collect();
        let (head, fields, prefix) = select_format(&entries).unwrap();
        assert_eq!({ head.key_header_bytes }, 1);
        let payload = encode_packed_run(&entries, &head, &fields, &prefix).unwrap();
        let (decoded, _head, _fields) =
            decode_packed_run::<TestKey>(&payload, 4, entries.len() as u32).unwrap();
        for (orig, dec) in entries.iter().zip(decoded.iter()) {
            assert_eq!(orig.0, dec.0);
            assert_eq!(orig.1, dec.1);
        }
    }

    // --- byte-wise compare invariant (the critical one) ---

    #[test]
    fn packed_byte_compare_matches_unpacked_compare() {
        // Build a sorted slice of (u64, u32) keys; encode each individually
        // (so we can byte-slice them), and verify that byte-wise compare
        // matches the original tuple compare.
        let mut keys: Vec<(u64, u32)> = vec![
            (0, 0),
            (0, 1),
            (1, 0),
            (5, 100),
            (5, 200),
            (10, 0),
            (10, 5),
            (1000, 1),
        ];
        keys.sort();

        let entries: Vec<((u64, u32), Vec<u8>)> =
            keys.iter().map(|k| (*k, vec![])).collect();
        let (head, fields, prefix) = select_format(&entries).unwrap();
        let body_size = bytes_per_key(&head, &fields);
        let payload = encode_packed_run(&entries, &head, &fields, &prefix).unwrap();
        let entries_start = head.total_size();
        let value_tail_size = 0;
        let stride = body_size + value_tail_size;

        let per_key_bytes: Vec<&[u8]> = (0..entries.len())
            .map(|i| &payload[entries_start + i * stride..entries_start + i * stride + body_size])
            .collect();

        // Already encoded in sorted-key order; verify byte-wise compare
        // produces the same order.
        for w in per_key_bytes.windows(2) {
            assert!(
                compare_packed_keys(w[0], w[1]) != std::cmp::Ordering::Greater,
                "packed byte order does not match unpacked order"
            );
        }
    }

    #[test]
    fn compare_packed_keys_matches_unpacked() {
        // For 100 random pairs, encode then byte-compare equals tuple-compare.
        let pairs: Vec<((u32, u32), (u32, u32))> = (0u32..100)
            .map(|i| {
                let a = (i, (i.wrapping_mul(13)) & 0xff);
                let b = ((i.wrapping_mul(7)) & 0xff, i);
                (a, b)
            })
            .collect();

        for (a, b) in &pairs {
            let entries = vec![(*a, vec![]), (*b, vec![])];
            let mut sorted = entries.clone();
            sorted.sort_by_key(|x| x.0);
            let (head, fields, prefix) = select_format(&sorted).unwrap();
            let body_size = bytes_per_key(&head, &fields);
            let payload = encode_packed_run(&sorted, &head, &fields, &prefix).unwrap();
            let start = head.total_size();
            let stride = body_size;
            let p0 = &payload[start..start + body_size];
            let p1 = &payload[start + stride..start + stride + body_size];
            let packed_ord = compare_packed_keys(p0, p1);
            let unpacked_ord = sorted[0].0.cmp(&sorted[1].0);
            assert_eq!(packed_ord, unpacked_ord);
        }
    }

    // --- check_fit ---

    #[test]
    fn check_fit_in_range_returns_fits() {
        let entries: Vec<(u64, Vec<u8>)> = vec![(0u64, vec![]), (255u64, vec![])];
        let (head, fields, _prefix) = select_format(&entries).unwrap();
        let r = check_fit::<u64>(&head, &fields, &100u64).unwrap();
        assert!(matches!(r, FormatFit::Fits));
    }

    #[test]
    fn check_fit_overflow_returns_promotion() {
        let entries: Vec<(u64, Vec<u8>)> = vec![(0u64, vec![]), (255u64, vec![])];
        let (head, fields, _prefix) = select_format(&entries).unwrap();
        let r = check_fit::<u64>(&head, &fields, &10_000u64).unwrap();
        match r {
            FormatFit::NeedsPromotion(_new_head, new_fields) => {
                let bw = { new_fields[0].bit_width };
                assert!(bw >= 14, "promoted bw should fit 10_000: got {bw}");
            }
            _ => panic!("expected NeedsPromotion"),
        }
    }

    #[test]
    fn check_fit_below_base_returns_promotion() {
        let entries: Vec<(u64, Vec<u8>)> = vec![(100u64, vec![]), (200u64, vec![])];
        let (head, fields, _prefix) = select_format(&entries).unwrap();
        let r = check_fit::<u64>(&head, &fields, &50u64).unwrap();
        match r {
            FormatFit::NeedsPromotion(_, new_fields) => {
                assert_eq!({ new_fields[0].base }, 50);
            }
            _ => panic!("expected NeedsPromotion"),
        }
    }

    // --- field overflow on encode ---

    #[test]
    fn encode_field_overflow_errors() {
        let mut entries: Vec<(u64, Vec<u8>)> = vec![(0u64, vec![]), (10u64, vec![])];
        let (head, fields, prefix) = select_format(&entries).unwrap();
        // Now sneak a key whose field exceeds the format.
        entries.push((1_000u64, vec![]));
        let err = encode_packed_run(&entries, &head, &fields, &prefix).unwrap_err();
        assert!(matches!(err, PackError::FieldOverflow { .. }));
    }

    // --- bit-width arithmetic / alignment ---

    #[test]
    fn bytes_per_key_alignment() {
        // bit_width = [3, 5, 7] => 15 bits => ceil(15/8) = 2 data bytes.
        // No header.
        let head = SortedRunKeyFormat {
            nr_fields: 3,
            key_header_bytes: 0,
            common_value_prefix: 0,
            value_size_kind: VALUE_SIZE_KIND_FIXED,
            sum_bit_width: 15,
        };
        let fields = vec![
            FieldFormat {
                bit_width: 3,
                flags: 0,
                _pad0: 0,
                base: 0,
                _pad1: 0,
            },
            FieldFormat {
                bit_width: 5,
                flags: 0,
                _pad0: 0,
                base: 0,
                _pad1: 0,
            },
            FieldFormat {
                bit_width: 7,
                flags: 0,
                _pad0: 0,
                base: 0,
                _pad1: 0,
            },
        ];
        assert_eq!(bytes_per_key(&head, &fields), 2);
    }

    #[test]
    fn bytes_per_key_zero_widths_no_body() {
        let head = SortedRunKeyFormat {
            nr_fields: 1,
            key_header_bytes: 0,
            common_value_prefix: 0,
            value_size_kind: VALUE_SIZE_KIND_FIXED,
            sum_bit_width: 0,
        };
        let fields = vec![FieldFormat {
            bit_width: 0,
            flags: 0,
            _pad0: 0,
            base: 5,
            _pad1: 0,
        }];
        assert_eq!(bytes_per_key(&head, &fields), 0);
    }

    #[test]
    fn constant_field_zero_cost_encoding() {
        // All keys identical => bit_width = 0 for every field => zero data
        // bytes per entry; payload is just the format descriptor + value
        // tails (or none if values empty).
        let entries: Vec<(u64, Vec<u8>)> = vec![(7u64, vec![]); 5];
        let (head, fields, prefix) = select_format(&entries).unwrap();
        assert_eq!({ fields[0].bit_width }, 0);
        let payload = encode_packed_run(&entries, &head, &fields, &prefix).unwrap();
        assert_eq!(payload.len(), head.total_size());
    }

    // --- common value prefix elision ---

    #[test]
    fn common_value_prefix_elision_round_trip() {
        let prefix_template = vec![0xAB, 0xCD, 0xEF, 0x01];
        let entries: Vec<(u64, Vec<u8>)> = (0u64..3)
            .map(|i| {
                let mut v = prefix_template.clone();
                v.push(i as u8);
                v.push((i + 100) as u8);
                (i, v)
            })
            .collect();
        let (head, fields, prefix) = select_format(&entries).unwrap();
        assert_eq!({ head.common_value_prefix }, 4);
        assert_eq!(prefix, prefix_template);
        let payload = encode_packed_run(&entries, &head, &fields, &prefix).unwrap();
        // Decoder reads the prefix bytes directly from the descriptor;
        // no caller-supplied template is needed.
        let (decoded, _head, _fields) =
            decode_packed_run::<u64>(&payload, 6, entries.len() as u32).unwrap();
        for (orig, dec) in entries.iter().zip(decoded.iter()) {
            assert_eq!(orig.0, dec.0);
            assert_eq!(orig.1, dec.1);
        }
    }

    // --- single-entry round-trip (the C1 spec amendment guarantee) ---

    /// Single-entry sorted runs are the case where `select_format` saturates
    /// `common_value_prefix` to the value's full length: every byte is
    /// trivially shared. Pre-amendment, this stripped the entire value with
    /// no on-disk record of the prefix; the reader couldn't reconstruct.
    /// Post-amendment, the descriptor's `value_prefix` slot carries the
    /// bytes and the reader rebuilds the value byte-exact.
    #[test]
    fn single_entry_run_round_trips_byte_exact() {
        let entries: Vec<(u64, Vec<u8>)> = vec![(42u64, vec![0xCA, 0xFE, 0xBA, 0xBE])];
        let (head, fields, prefix) = select_format(&entries).unwrap();
        // For one entry every byte is "shared" — the encoder lifts the full
        // value into the prefix.
        assert_eq!({ head.common_value_prefix }, 4);
        assert_eq!(prefix, vec![0xCA, 0xFE, 0xBA, 0xBE]);
        let payload = encode_packed_run(&entries, &head, &fields, &prefix).unwrap();
        let (decoded, _head, _fields) =
            decode_packed_run::<u64>(&payload, 4, 1).unwrap();
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].0, 42);
        assert_eq!(decoded[0].1, vec![0xCA, 0xFE, 0xBA, 0xBE]);
    }

    // --- variable-size values (varint mode) ---

    #[test]
    fn select_format_chooses_varint_mode_for_mixed_lengths() {
        // Two entries whose values differ in both length AND content (no
        // shared prefix beyond the discriminator).
        let entries: Vec<(u64, Vec<u8>)> = vec![
            (0u64, vec![1, 2, 3]),
            (1u64, vec![5, 6]),
        ];
        let (head, _fields, _prefix) = select_format(&entries).unwrap();
        assert_eq!({ head.value_size_kind }, VALUE_SIZE_KIND_VARINT);
    }

    #[test]
    fn select_format_chooses_fixed_mode_for_uniform_lengths() {
        let entries: Vec<(u64, Vec<u8>)> = vec![
            (0u64, vec![1, 2, 3, 4]),
            (1u64, vec![1, 2, 3, 5]),
        ];
        let (head, _fields, _prefix) = select_format(&entries).unwrap();
        assert_eq!({ head.value_size_kind }, VALUE_SIZE_KIND_FIXED);
    }

    #[test]
    fn varint_mode_round_trips_mixed_length_values() {
        let entries: Vec<(u64, Vec<u8>)> = vec![
            (0u64, vec![0xA, 0xB, 0xC]),
            (1u64, vec![0xA, 0xB]),
            (2u64, vec![0xA, 0xB, 0xC, 0xD, 0xE]),
            (3u64, vec![0xA]),
        ];
        let (head, fields, prefix) = select_format(&entries).unwrap();
        assert_eq!({ head.value_size_kind }, VALUE_SIZE_KIND_VARINT);
        // Shared prefix is bounded by the shortest value (1 byte).
        assert_eq!({ head.common_value_prefix }, 1);
        assert_eq!(prefix, vec![0xA]);

        let payload = encode_packed_run(&entries, &head, &fields, &prefix).unwrap();
        // value_size argument is ignored in varint mode; pass a dummy.
        let (decoded, _head, _fields) =
            decode_packed_run::<u64>(&payload, /* ignored */ 0, entries.len() as u32)
                .unwrap();
        assert_eq!(decoded.len(), entries.len());
        for (orig, dec) in entries.iter().zip(decoded.iter()) {
            assert_eq!(orig.0, dec.0);
            assert_eq!(orig.1, dec.1, "value bytes differ for key {}", orig.0);
        }
    }

    #[test]
    fn varint_mode_handles_empty_values() {
        let entries: Vec<(u64, Vec<u8>)> = vec![
            (0u64, vec![]),
            (1u64, vec![0xFF, 0xFE]),
        ];
        let (head, fields, prefix) = select_format(&entries).unwrap();
        assert_eq!({ head.value_size_kind }, VALUE_SIZE_KIND_VARINT);
        // No shared prefix possible when one value is empty.
        assert_eq!({ head.common_value_prefix }, 0);
        let payload = encode_packed_run(&entries, &head, &fields, &prefix).unwrap();
        let (decoded, _head, _fields) =
            decode_packed_run::<u64>(&payload, 0, 2).unwrap();
        assert_eq!(decoded[0].1, Vec::<u8>::new());
        assert_eq!(decoded[1].1, vec![0xFF, 0xFE]);
    }

    #[test]
    fn fixed_mode_payload_is_unchanged_by_amendment() {
        // Fixed-mode runs encode byte-identically to the pre-C2 layout —
        // i.e. no per-entry length prefix, just the value tail. We verify
        // by encoding under each mode (the FIXED kind value is `0`, same
        // bit pattern as the pre-amendment `_pad` byte).
        let entries: Vec<(u64, Vec<u8>)> = vec![
            (0u64, vec![1, 2, 3, 4]),
            (1u64, vec![1, 2, 3, 5]),
            (2u64, vec![1, 2, 3, 6]),
        ];
        let (head, fields, prefix) = select_format(&entries).unwrap();
        assert_eq!({ head.value_size_kind }, VALUE_SIZE_KIND_FIXED);
        let payload = encode_packed_run(&entries, &head, &fields, &prefix).unwrap();
        // Each entry's wire size = body + (value_size - prefix). No varint.
        let descriptor_size = head.total_size();
        let body_size = bytes_per_key(&head, &fields);
        let value_tail_size = 4 - { head.common_value_prefix } as usize;
        let expected_total =
            descriptor_size + entries.len() * (body_size + value_tail_size);
        assert_eq!(payload.len(), expected_total);
    }

    // --- varint helpers ---

    #[test]
    fn varint_round_trip_small_values() {
        for v in [0u64, 1, 42, 127, 128, 200, 16_383, 16_384, u32::MAX as u64] {
            let mut buf = Vec::new();
            write_varint_u64(&mut buf, v);
            let mut pos = 0;
            assert_eq!(read_varint_u64(&buf, &mut pos).unwrap(), v);
            assert_eq!(pos, buf.len());
        }
    }

    #[test]
    fn varint_round_trip_full_u64() {
        let mut buf = Vec::new();
        write_varint_u64(&mut buf, u64::MAX);
        assert_eq!(buf.len(), 10); // LEB128 needs 10 bytes for u64::MAX
        let mut pos = 0;
        assert_eq!(read_varint_u64(&buf, &mut pos).unwrap(), u64::MAX);
    }

    #[test]
    fn varint_truncated_errors() {
        let buf = [0x80u8]; // continuation bit set, but no follow-up byte
        let mut pos = 0;
        assert!(matches!(
            read_varint_u64(&buf, &mut pos),
            Err(PackError::Malformed(_))
        ));
    }

    #[test]
    fn varint_overlong_errors() {
        // 11-byte varint: more than the u64 maximum encoding length.
        let buf = [0x80u8; 11];
        let mut pos = 0;
        assert!(matches!(
            read_varint_u64(&buf, &mut pos),
            Err(PackError::Malformed(_))
        ));
    }

    // --- pack_bits / read_bits round-trip ---

    #[test]
    fn pack_read_bits_round_trip() {
        let mut buf = Vec::new();
        let mut bp = 0usize;
        pack_bits(&mut buf, &mut bp, 0b1011, 4);
        pack_bits(&mut buf, &mut bp, 0b110, 3);
        pack_bits(&mut buf, &mut bp, 0b1, 1);
        // 4 + 3 + 1 = 8 bits => one byte: 1011_110_1 = 0xBD.
        assert_eq!(buf, vec![0b1011_1101]);

        let mut rp = 0usize;
        assert_eq!(read_bits(&buf, &mut rp, 4).unwrap(), 0b1011);
        assert_eq!(read_bits(&buf, &mut rp, 3).unwrap(), 0b110);
        assert_eq!(read_bits(&buf, &mut rp, 1).unwrap(), 0b1);
    }

    #[test]
    fn pack_bits_64_bit_value() {
        let mut buf = Vec::new();
        let mut bp = 0usize;
        pack_bits(&mut buf, &mut bp, u64::MAX, 64);
        assert_eq!(buf, vec![0xff; 8]);
        let mut rp = 0usize;
        assert_eq!(read_bits(&buf, &mut rp, 64).unwrap(), u64::MAX);
    }

    // --- signed / msb_first flags propagate through select_format ---

    #[test]
    fn field_hints_to_flags() {
        struct SignedKey;
        impl PackableKey for SignedKey {
            fn nr_fields() -> usize {
                2
            }
            fn field_hints() -> &'static [FieldHints] {
                const H: [FieldHints; 2] = [FieldHints::signed(), FieldHints::unsigned_msb()];
                &H
            }
            fn field_values(&self, out: &mut [u64]) {
                out[0] = 0;
                out[1] = 0;
            }
            fn from_components(_: u32, _: &[u64]) -> Result<Self, PackError> {
                Ok(SignedKey)
            }
        }
        let entries: Vec<(SignedKey, Vec<u8>)> = vec![(SignedKey, vec![])];
        let (_head, fields, _prefix) = select_format(&entries).unwrap();
        assert!({ fields[0].flags } & FIELD_FORMAT_FLAG_SIGNED != 0);
        assert!({ fields[1].flags } & FIELD_FORMAT_FLAG_MSB_FIRST != 0);
    }
}
