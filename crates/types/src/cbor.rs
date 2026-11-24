//! Positional CBOR (FORMAT-DESIGN§1.6), hand-written.
//!
//! A struct is an array of its fields in pinned order; an enum is
//! `[variant_index, fields…]`; a top-level record starts with its `u16`
//! schema version, and fields are only ever appended, so a reader supplies
//! defaults for trailing fields an older version lacks. Integers use the
//! shortest form (RFC 8949 §4.2.1), so equal values encode to equal bytes;
//! the reader rejects any other form, indefinite lengths and floats that are
//! not 64-bit.

use crate::TypesError;

const MAJOR_UNSIGNED: u8 = 0;
const MAJOR_NEGATIVE: u8 = 1;
const MAJOR_BYTES: u8 = 2;
const MAJOR_TEXT: u8 = 3;
const MAJOR_ARRAY: u8 = 4;
const MAJOR_SIMPLE: u8 = 7;

const SIMPLE_FALSE: u8 = 20;
const SIMPLE_TRUE: u8 = 21;
const SIMPLE_NULL: u8 = 22;
const ADDITIONAL_FLOAT64: u8 = 27;

/// Writes positional CBOR into a byte vector.
#[derive(Debug, Default)]
pub struct CborWriter {
    bytes: Vec<u8>,
}

impl CborWriter {
    /// Empty writer.
    pub fn new() -> Self {
        Self::default()
    }

    /// The bytes written so far.
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    /// Head of an array of `length` elements.
    pub fn array(&mut self, length: usize) {
        self.head(MAJOR_ARRAY, length as u64);
    }

    /// Head of a top-level record: an array of the version and `field_count`
    /// fields, then the version.
    pub fn record(&mut self, version: u16, field_count: usize) {
        self.array(field_count + 1);
        self.uint(u64::from(version));
    }

    /// Head of an enum value: an array of the variant index and
    /// `field_count` fields, then the index.
    pub fn variant(&mut self, index: u64, field_count: usize) {
        self.array(field_count + 1);
        self.uint(index);
    }

    /// Unsigned integer, shortest form.
    pub fn uint(&mut self, value: u64) {
        self.head(MAJOR_UNSIGNED, value);
    }

    /// Signed integer, shortest form.
    pub fn int(&mut self, value: i64) {
        if value >= 0 {
            self.head(MAJOR_UNSIGNED, value as u64);
        } else {
            // CBOR stores −1 − n; for i64 that is !n reinterpreted.
            self.head(MAJOR_NEGATIVE, !value as u64);
        }
    }

    /// Boolean.
    pub fn bool(&mut self, value: bool) {
        self.bytes
            .push(MAJOR_SIMPLE << 5 | if value { SIMPLE_TRUE } else { SIMPLE_FALSE });
    }

    /// Null.
    pub fn null(&mut self) {
        self.bytes.push(MAJOR_SIMPLE << 5 | SIMPLE_NULL);
    }

    /// Float, always 64-bit.
    pub fn float64(&mut self, value: f64) {
        self.bytes.push(MAJOR_SIMPLE << 5 | ADDITIONAL_FLOAT64);
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    /// UTF-8 text string.
    pub fn text(&mut self, value: &str) {
        self.head(MAJOR_TEXT, value.len() as u64);
        self.bytes.extend_from_slice(value.as_bytes());
    }

    /// Byte string.
    pub fn bytes(&mut self, value: &[u8]) {
        self.head(MAJOR_BYTES, value.len() as u64);
        self.bytes.extend_from_slice(value);
    }

    fn head(&mut self, major: u8, argument: u64) {
        let major = major << 5;
        match argument {
            0..=23 => self.bytes.push(major | argument as u8),
            24..=0xff => self.bytes.extend_from_slice(&[major | 24, argument as u8]),
            0x100..=0xffff => {
                self.bytes.push(major | 25);
                self.bytes.extend_from_slice(&(argument as u16).to_be_bytes());
            }
            0x1_0000..=0xffff_ffff => {
                self.bytes.push(major | 26);
                self.bytes.extend_from_slice(&(argument as u32).to_be_bytes());
            }
            _ => {
                self.bytes.push(major | 27);
                self.bytes.extend_from_slice(&argument.to_be_bytes());
            }
        }
    }
}

/// Reads positional CBOR from a byte slice.
#[derive(Debug)]
pub struct CborReader<'a> {
    input: &'a [u8],
    position: usize,
}

impl<'a> CborReader<'a> {
    /// Reader positioned at the start of `input`.
    pub fn new(input: &'a [u8]) -> Self {
        Self { input, position: 0 }
    }

    /// Fails unless every byte has been read.
    pub fn finish(self) -> Result<(), TypesError> {
        if self.position == self.input.len() {
            Ok(())
        } else {
            Err(TypesError::CborTrailingBytes(self.input.len() - self.position))
        }
    }

    /// Array head; returns its element count.
    pub fn array(&mut self) -> Result<usize, TypesError> {
        let length = self.head(MAJOR_ARRAY, "array")?;
        usize::try_from(length).map_err(|_| TypesError::CborIntegerRange)
    }

    /// Record head written by [`CborWriter::record`]. Fails with
    /// [`TypesError::RecordVersionTooNew`] for a version above `supported`.
    pub fn record(&mut self, supported: u16) -> Result<RecordFields, TypesError> {
        let length = self.array()?;
        if length == 0 {
            return Err(TypesError::CborUnexpected {
                expected: "record version",
                found: None,
            });
        }
        let version =
            u16::try_from(self.uint()?).map_err(|_| TypesError::CborIntegerRange)?;
        if version > supported {
            return Err(TypesError::RecordVersionTooNew { found: version, supported });
        }
        Ok(RecordFields {
            version,
            remaining: length - 1,
        })
    }

    /// Enum head written by [`CborWriter::variant`]: the variant index and
    /// the number of fields after it.
    pub fn variant(&mut self) -> Result<(u64, usize), TypesError> {
        let length = self.array()?;
        if length == 0 {
            return Err(TypesError::CborUnexpected {
                expected: "variant index",
                found: None,
            });
        }
        Ok((self.uint()?, length - 1))
    }

    /// Unsigned integer.
    pub fn uint(&mut self) -> Result<u64, TypesError> {
        self.head(MAJOR_UNSIGNED, "unsigned integer")
    }

    /// Signed integer in `i64` range.
    pub fn int(&mut self) -> Result<i64, TypesError> {
        match self.peek_major()? {
            MAJOR_UNSIGNED => {
                i64::try_from(self.uint()?).map_err(|_| TypesError::CborIntegerRange)
            }
            MAJOR_NEGATIVE => {
                let stored = self.head(MAJOR_NEGATIVE, "integer")?;
                let magnitude =
                    i64::try_from(stored).map_err(|_| TypesError::CborIntegerRange)?;
                Ok(!magnitude)
            }
            _ => Err(self.unexpected("integer")),
        }
    }

    /// Boolean.
    pub fn bool(&mut self) -> Result<bool, TypesError> {
        match self.peek_byte()? {
            byte if byte == MAJOR_SIMPLE << 5 | SIMPLE_FALSE => {
                self.position += 1;
                Ok(false)
            }
            byte if byte == MAJOR_SIMPLE << 5 | SIMPLE_TRUE => {
                self.position += 1;
                Ok(true)
            }
            _ => Err(self.unexpected("boolean")),
        }
    }

    /// Consumes a null if one is next; returns whether it did.
    pub fn null(&mut self) -> Result<bool, TypesError> {
        if self.peek_byte()? == MAJOR_SIMPLE << 5 | SIMPLE_NULL {
            self.position += 1;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// 64-bit float; a 16- or 32-bit float is non-canonical.
    pub fn float64(&mut self) -> Result<f64, TypesError> {
        let initial = self.peek_byte()?;
        if initial >> 5 != MAJOR_SIMPLE || !(25..=27).contains(&(initial & 0x1f)) {
            return Err(self.unexpected("float"));
        }
        if initial & 0x1f != ADDITIONAL_FLOAT64 {
            return Err(TypesError::CborNonCanonical);
        }
        self.position += 1;
        let bytes = self.take(8)?;
        Ok(f64::from_be_bytes(bytes.try_into().expect("took 8 bytes")))
    }

    /// UTF-8 text string, borrowed from the input.
    pub fn text(&mut self) -> Result<&'a str, TypesError> {
        let length = self.head(MAJOR_TEXT, "text")?;
        let bytes = self.take_length(length)?;
        std::str::from_utf8(bytes).map_err(|_| TypesError::CborInvalidUtf8)
    }

    /// Byte string, borrowed from the input.
    pub fn bytes(&mut self) -> Result<&'a [u8], TypesError> {
        let length = self.head(MAJOR_BYTES, "bytes")?;
        self.take_length(length)
    }

    /// Reads one head of `major` and returns its argument, enforcing the
    /// shortest form and rejecting indefinite lengths.
    fn head(&mut self, major: u8, expected: &'static str) -> Result<u64, TypesError> {
        let initial = self.peek_byte()?;
        if initial >> 5 != major {
            return Err(self.unexpected(expected));
        }
        self.position += 1;
        let (argument, smallest_allowed) = match initial & 0x1f {
            additional @ 0..=23 => return Ok(u64::from(additional)),
            24 => (u64::from(self.take(1)?[0]), 24),
            25 => (u64::from(u16::from_be_bytes(self.array_of()?)), 0x100),
            26 => (u64::from(u32::from_be_bytes(self.array_of()?)), 0x1_0000),
            27 => (u64::from_be_bytes(self.array_of()?), 0x1_0000_0000),
            _ => return Err(TypesError::CborNonCanonical),
        };
        if argument < smallest_allowed {
            return Err(TypesError::CborNonCanonical);
        }
        Ok(argument)
    }

    fn array_of<const N: usize>(&mut self) -> Result<[u8; N], TypesError> {
        Ok(self.take(N)?.try_into().expect("took N bytes"))
    }

    fn take_length(&mut self, length: u64) -> Result<&'a [u8], TypesError> {
        self.take(usize::try_from(length).map_err(|_| TypesError::CborTruncated)?)
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8], TypesError> {
        let end = self
            .position
            .checked_add(count)
            .filter(|&end| end <= self.input.len())
            .ok_or(TypesError::CborTruncated)?;
        let bytes = &self.input[self.position..end];
        self.position = end;
        Ok(bytes)
    }

    fn peek_byte(&self) -> Result<u8, TypesError> {
        self.input
            .get(self.position)
            .copied()
            .ok_or(TypesError::CborTruncated)
    }

    fn peek_major(&self) -> Result<u8, TypesError> {
        Ok(self.peek_byte()? >> 5)
    }

    fn unexpected(&self, expected: &'static str) -> TypesError {
        TypesError::CborUnexpected {
            expected,
            found: self.input.get(self.position).copied(),
        }
    }
}

/// The fields of a record after its version, read in order. A field past
/// the end of an older version's array reads as `None`; the caller supplies
/// the default its format section documents.
#[derive(Debug)]
pub struct RecordFields {
    /// The record's schema version.
    pub version: u16,
    remaining: usize,
}

impl RecordFields {
    /// Reads the next field with `decode`, or returns `None` if the record
    /// has no more fields.
    pub fn field<'a, T>(
        &mut self,
        reader: &mut CborReader<'a>,
        decode: impl FnOnce(&mut CborReader<'a>) -> Result<T, TypesError>,
    ) -> Result<Option<T>, TypesError> {
        if self.remaining == 0 {
            return Ok(None);
        }
        self.remaining -= 1;
        decode(reader).map(Some)
    }

    /// Fails if the record carries fields this reader does not know.
    pub fn finish(self) -> Result<(), TypesError> {
        if self.remaining == 0 {
            Ok(())
        } else {
            Err(TypesError::RecordExtraFields(self.remaining))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn written(write: impl FnOnce(&mut CborWriter)) -> Vec<u8> {
        let mut writer = CborWriter::new();
        write(&mut writer);
        writer.into_bytes()
    }

    #[test]
    fn integers_use_the_shortest_form() {
        assert_eq!(written(|w| w.uint(23)), [0x17]);
        assert_eq!(written(|w| w.uint(24)), [0x18, 24]);
        assert_eq!(written(|w| w.uint(0x100)), [0x19, 0x01, 0x00]);
        assert_eq!(written(|w| w.uint(0x1_0000)), [0x1a, 0, 1, 0, 0]);
        assert_eq!(written(|w| w.uint(1 << 32)), [0x1b, 0, 0, 0, 1, 0, 0, 0, 0]);
        assert_eq!(written(|w| w.int(-1)), [0x20]);
        assert_eq!(written(|w| w.int(-25)), [0x38, 24]);
        assert_eq!(
            written(|w| w.int(i64::MIN)),
            [0x3b, 0x7f, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]
        );
    }

    #[test]
    fn integers_round_trip_at_the_edges() {
        for value in [0, 1, 23, 24, 255, 256, -1, -24, -25, -256, -257, i64::MAX, i64::MIN] {
            let bytes = written(|w| w.int(value));
            let mut reader = CborReader::new(&bytes);
            assert_eq!(reader.int().unwrap(), value);
            reader.finish().unwrap();
        }
    }

    #[test]
    fn a_longer_head_than_needed_is_rejected() {
        // 5 written with a one-byte argument.
        let mut reader = CborReader::new(&[0x18, 5]);
        assert!(matches!(reader.uint(), Err(TypesError::CborNonCanonical)));
        // An indefinite-length array.
        let mut reader = CborReader::new(&[0x9f]);
        assert!(matches!(reader.array(), Err(TypesError::CborNonCanonical)));
    }

    #[test]
    fn an_integer_outside_i64_is_rejected() {
        let bytes = written(|w| w.uint(u64::MAX));
        assert!(matches!(
            CborReader::new(&bytes).int(),
            Err(TypesError::CborIntegerRange)
        ));
    }

    #[test]
    fn floats_are_always_64_bit() {
        assert_eq!(
            written(|w| w.float64(1.5)),
            [0xfb, 0x3f, 0xf8, 0, 0, 0, 0, 0, 0]
        );
        // 1.5 as a 16-bit float.
        let mut reader = CborReader::new(&[0xf9, 0x3e, 0x00]);
        assert!(matches!(reader.float64(), Err(TypesError::CborNonCanonical)));
    }

    #[test]
    fn strings_and_simple_values_round_trip() {
        let bytes = written(|w| {
            w.array(4);
            w.text("Aphex Twin");
            w.bytes(&[1, 2, 3]);
            w.bool(true);
            w.null();
        });
        let mut reader = CborReader::new(&bytes);
        assert_eq!(reader.array().unwrap(), 4);
        assert_eq!(reader.text().unwrap(), "Aphex Twin");
        assert_eq!(reader.bytes().unwrap(), [1, 2, 3]);
        assert!(reader.bool().unwrap());
        assert!(reader.null().unwrap());
        reader.finish().unwrap();
    }

    #[test]
    fn truncated_and_trailing_input_are_errors() {
        let mut reader = CborReader::new(&[0x63, b'a', b'b']);
        assert!(matches!(reader.text(), Err(TypesError::CborTruncated)));
        let mut reader = CborReader::new(&[0x01, 0x02]);
        reader.uint().unwrap();
        assert!(matches!(reader.finish(), Err(TypesError::CborTrailingBytes(1))));
    }

    #[test]
    fn records_default_missing_trailing_fields_and_reject_newer_versions() {
        // Version 1 with one field; the reader knows two.
        let bytes = written(|w| {
            w.record(1, 1);
            w.uint(7);
        });
        let mut reader = CborReader::new(&bytes);
        let mut fields = reader.record(2).unwrap();
        assert_eq!(fields.version, 1);
        assert_eq!(fields.field(&mut reader, CborReader::uint).unwrap(), Some(7));
        let second = fields.field(&mut reader, CborReader::uint).unwrap();
        assert_eq!(second.unwrap_or(42), 42);
        fields.finish().unwrap();
        reader.finish().unwrap();

        let mut reader = CborReader::new(&bytes);
        assert!(matches!(
            reader.record(0),
            Err(TypesError::RecordVersionTooNew { found: 1, supported: 0 })
        ));
    }

    #[test]
    fn a_record_with_unknown_fields_is_rejected() {
        let bytes = written(|w| {
            w.record(1, 2);
            w.uint(1);
            w.uint(2);
        });
        let mut reader = CborReader::new(&bytes);
        let mut fields = reader.record(1).unwrap();
        fields.field(&mut reader, CborReader::uint).unwrap();
        assert!(matches!(fields.finish(), Err(TypesError::RecordExtraFields(1))));
    }

    #[test]
    fn variants_carry_their_index_and_field_count() {
        let bytes = written(|w| {
            w.variant(3, 2);
            w.uint(10);
            w.text("x");
        });
        let mut reader = CborReader::new(&bytes);
        assert_eq!(reader.variant().unwrap(), (3, 2));
    }
}
