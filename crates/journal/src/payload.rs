//! Binary payload records (JOURNAL-DESIGN§1.3, FORMAT-DESIGN§1.6).
//!
//! A payload is its fixed fields in listed order, each at its natural alignment with zero
//! padding, the head padded to a multiple of 8 bytes, then at most one variable part
//! (its length or count sits in the head), padded with zeros to a multiple of 8.
//! [`PayloadWriter`] and [`PayloadReader`] place and check that padding; [`Field`] and
//! [`Tail`] give each field type its image.

use {
    mimisbrunnr_storage::{BlockRef, ChunkParamsRecord, RootPointer},
    mimisbrunnr_types::{Value, decode_value_cbor, encode_value_cbor},
};

use crate::error::WalError;

// ---------- Writer / reader ----------

/// Builds one payload record.
#[derive(Default)]
pub(crate) struct PayloadWriter {
    bytes: Vec<u8>,
}

impl PayloadWriter {
    pub(crate) fn pad_to(&mut self, alignment: usize) {
        let padded_length = self.bytes.len().next_multiple_of(alignment);
        self.bytes.resize(padded_length, 0);
    }

    fn put(&mut self, alignment: usize, bytes: &[u8]) {
        self.pad_to(alignment);
        self.bytes.extend_from_slice(bytes);
    }

    /// The variable part: starts on an 8-byte boundary after the head.
    pub(crate) fn tail(&mut self, bytes: &[u8]) {
        self.put(8, bytes);
    }

    /// The finished record, zero-padded to a multiple of 8 bytes.
    pub(crate) fn finish(mut self) -> Vec<u8> {
        self.pad_to(8);
        self.bytes
    }
}

/// Reads one payload record of `EntryHead.len × 8` bytes.
pub(crate) struct PayloadReader<'a> {
    bytes: &'a [u8],
    position: usize,
    op_kind: u8,
}

impl<'a> PayloadReader<'a> {
    pub(crate) fn new(op_kind: u8, bytes: &'a [u8]) -> Result<Self, WalError> {
        if !bytes.len().is_multiple_of(8) {
            return Err(WalError::PayloadNotWordAligned {
                op_kind,
                length: bytes.len(),
            });
        }
        Ok(Self {
            bytes,
            position: 0,
            op_kind,
        })
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], WalError> {
        let end = self
            .position
            .checked_add(length)
            .filter(|&end| end <= self.bytes.len())
            .ok_or(WalError::PayloadTruncated {
                op_kind: self.op_kind,
                needed: self.position.saturating_add(length),
                available: self.bytes.len(),
            })?;
        let taken = &self.bytes[self.position..end];
        self.position = end;
        Ok(taken)
    }

    fn check_zero(&self, bytes: &[u8], offset: usize) -> Result<(), WalError> {
        match bytes.iter().position(|&byte| byte != 0) {
            None => Ok(()),
            Some(index) => Err(WalError::PayloadPaddingNotZero {
                op_kind: self.op_kind,
                offset: offset + index,
            }),
        }
    }

    pub(crate) fn skip_to(&mut self, alignment: usize) -> Result<(), WalError> {
        let offset = self.position;
        let padding = self.take(offset.next_multiple_of(alignment) - offset)?;
        self.check_zero(padding, offset)
    }

    fn get<const N: usize>(&mut self, alignment: usize) -> Result<[u8; N], WalError> {
        self.skip_to(alignment)?;
        let mut out = [0u8; N];
        out.copy_from_slice(self.take(N)?);
        Ok(out)
    }

    /// The variable part's bytes, from the next 8-byte boundary.
    pub(crate) fn tail(&mut self, length: usize) -> Result<&'a [u8], WalError> {
        self.skip_to(8)?;
        self.take(length)
    }

    /// Everything after the record must be zero padding.
    pub(crate) fn finish(self) -> Result<(), WalError> {
        self.check_zero(&self.bytes[self.position..], self.position)
    }
}

// ---------- Fixed fields ----------

/// A fixed-size field: written at `ALIGN`, read back with its padding checked.
pub(crate) trait Field: Sized {
    const ALIGN: usize;
    fn write(&self, writer: &mut PayloadWriter);
    fn read(reader: &mut PayloadReader<'_>) -> Result<Self, WalError>;
}

/// The largest of `alignments` (a record aligns to its widest field).
pub(crate) const fn max_alignment(alignments: &[usize]) -> usize {
    let mut widest = 1;
    let mut index = 0;
    while index < alignments.len() {
        if alignments[index] > widest {
            widest = alignments[index];
        }
        index += 1;
    }
    widest
}

macro_rules! integer_field {
    ($($ty:ty),*) => {$(
        impl Field for $ty {
            const ALIGN: usize = core::mem::size_of::<$ty>();
            fn write(&self, writer: &mut PayloadWriter) {
                writer.put(Self::ALIGN, &self.to_le_bytes());
            }
            fn read(reader: &mut PayloadReader<'_>) -> Result<Self, WalError> {
                Ok(<$ty>::from_le_bytes(reader.get(Self::ALIGN)?))
            }
        }
    )*};
}

integer_field!(u8, u16, u32, u64, i64, u128);

impl Field for bool {
    const ALIGN: usize = 1;
    fn write(&self, writer: &mut PayloadWriter) {
        u8::from(*self).write(writer);
    }
    fn read(reader: &mut PayloadReader<'_>) -> Result<Self, WalError> {
        match u8::read(reader)? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(WalError::PayloadFieldInvalid { field: "bool" }),
        }
    }
}

impl<T: Field + Copy + Default, const N: usize> Field for [T; N] {
    const ALIGN: usize = T::ALIGN;
    fn write(&self, writer: &mut PayloadWriter) {
        for element in self {
            element.write(writer);
        }
    }
    fn read(reader: &mut PayloadReader<'_>) -> Result<Self, WalError> {
        let mut out = [T::default(); N];
        for element in &mut out {
            *element = T::read(reader)?;
        }
        Ok(out)
    }
}

/// `BlockRef`: its 16-byte image (ENGINE-DESIGN§1.2), reserved word zero.
impl Field for BlockRef {
    const ALIGN: usize = 4;
    fn write(&self, writer: &mut PayloadWriter) {
        self.disk_id.write(writer);
        self.sector.write(writer);
        self.bucket_no.write(writer);
        self.generation.write(writer);
        0u32.write(writer);
    }
    fn read(reader: &mut PayloadReader<'_>) -> Result<Self, WalError> {
        let disk_id = u16::read(reader)?;
        let sector = u16::read(reader)?;
        let bucket_no = u32::read(reader)?;
        let generation = u32::read(reader)?;
        if u32::read(reader)? != 0 {
            return Err(WalError::PayloadFieldInvalid {
                field: "BlockRef reserved",
            });
        }
        Ok(BlockRef::new(disk_id, bucket_no, sector, generation))
    }
}

/// `ChunkParamsRecord`: its 16-byte image (INDEX-DESIGN§4.3).
impl Field for ChunkParamsRecord {
    const ALIGN: usize = 4;
    fn write(&self, writer: &mut PayloadWriter) {
        writer.put(Self::ALIGN, &self.bytes);
    }
    fn read(reader: &mut PayloadReader<'_>) -> Result<Self, WalError> {
        Ok(Self {
            bytes: reader.get(Self::ALIGN)?,
        })
    }
}

/// `RootPointer`: its image.
// TODO(ENGINE12): the root list (`RootListHead` + `RootEntry` × N) replaces it.
impl Field for RootPointer {
    const ALIGN: usize = 8;
    fn write(&self, writer: &mut PayloadWriter) {
        writer.put(Self::ALIGN, bytemuck::bytes_of(self));
    }
    fn read(reader: &mut PayloadReader<'_>) -> Result<Self, WalError> {
        let image: [u8; core::mem::size_of::<RootPointer>()] = reader.get(Self::ALIGN)?;
        let mut root_pointer = RootPointer::default();
        bytemuck::bytes_of_mut(&mut root_pointer).copy_from_slice(&image);
        Ok(root_pointer)
    }
}

// ---------- Variable parts ----------

/// The variable part of a payload: its head length or count, and its bytes.
pub(crate) trait Tail: Sized {
    /// `true` when the head holds a `u16` length (strings), else a `u32`.
    const SHORT_LENGTH: bool = false;
    /// The head value and the bytes, checking at most `max` elements or bytes.
    fn encode_tail(&self, field: &'static str, max: usize) -> Result<(usize, Vec<u8>), WalError>;
    /// The tail for head value `raw_length`, read from the next 8-byte boundary.
    fn decode_tail(
        reader: &mut PayloadReader<'_>,
        raw_length: usize,
        field: &'static str,
        max: usize,
    ) -> Result<Self, WalError>;
}

fn check_length(field: &'static str, length: usize, max: usize) -> Result<(), WalError> {
    if length > max {
        return Err(WalError::PayloadFieldTooLong { field, length, max });
    }
    Ok(())
}

/// `[T; N]` with N not fixed by the type: a `u32` count and the records; `bytes` is
/// `Vec<u8>`, a `u32` length and the bytes.
impl<T: Field> Tail for Vec<T> {
    fn encode_tail(&self, field: &'static str, max: usize) -> Result<(usize, Vec<u8>), WalError> {
        check_length(field, self.len(), max)?;
        let mut writer = PayloadWriter::default();
        for element in self {
            element.write(&mut writer);
        }
        Ok((self.len(), writer.bytes))
    }
    fn decode_tail(
        reader: &mut PayloadReader<'_>,
        raw_length: usize,
        field: &'static str,
        max: usize,
    ) -> Result<Self, WalError> {
        check_length(field, raw_length, max)?;
        reader.skip_to(8)?;
        let mut elements = Vec::new();
        for _ in 0..raw_length {
            elements.push(T::read(reader)?);
        }
        Ok(elements)
    }
}

/// `Value`: a `u32` length and its INDEX-DESIGN§1.1 CBOR encoding.
impl Tail for Value {
    fn encode_tail(&self, field: &'static str, max: usize) -> Result<(usize, Vec<u8>), WalError> {
        let encoded = encode_value_cbor(self)?;
        check_length(field, encoded.len(), max)?;
        Ok((encoded.len(), encoded))
    }
    fn decode_tail(
        reader: &mut PayloadReader<'_>,
        raw_length: usize,
        field: &'static str,
        max: usize,
    ) -> Result<Self, WalError> {
        check_length(field, raw_length, max)?;
        Ok(decode_value_cbor(reader.tail(raw_length)?)?)
    }
}

/// `Value?`: no value is length 0 (a value's encoding is never empty).
impl Tail for Option<Value> {
    fn encode_tail(&self, field: &'static str, max: usize) -> Result<(usize, Vec<u8>), WalError> {
        match self {
            None => Ok((0, Vec::new())),
            Some(value) => value.encode_tail(field, max),
        }
    }
    fn decode_tail(
        reader: &mut PayloadReader<'_>,
        raw_length: usize,
        field: &'static str,
        max: usize,
    ) -> Result<Self, WalError> {
        if raw_length == 0 {
            return Ok(None);
        }
        Value::decode_tail(reader, raw_length, field, max).map(Some)
    }
}

/// `Option<String>`: a `u16` length, `u16::MAX` for `None`, and the UTF-8.
impl Tail for Option<String> {
    const SHORT_LENGTH: bool = true;
    fn encode_tail(&self, field: &'static str, max: usize) -> Result<(usize, Vec<u8>), WalError> {
        match self {
            None => Ok((usize::from(u16::MAX), Vec::new())),
            Some(text) => {
                check_length(field, text.len(), max.min(usize::from(u16::MAX) - 1))?;
                Ok((text.len(), text.as_bytes().to_vec()))
            }
        }
    }
    fn decode_tail(
        reader: &mut PayloadReader<'_>,
        raw_length: usize,
        field: &'static str,
        max: usize,
    ) -> Result<Self, WalError> {
        if raw_length == usize::from(u16::MAX) {
            return Ok(None);
        }
        check_length(field, raw_length, max)?;
        let text = core::str::from_utf8(reader.tail(raw_length)?)
            .map_err(|_| WalError::PayloadFieldInvalid { field })?;
        Ok(Some(text.to_owned()))
    }
}

// ---------- Record macros ----------

/// A fixed record: its fields at natural alignment, padded to its widest field.
macro_rules! record {
    (
        $(#[$meta:meta])*
        $name:ident { $($(#[$field_meta:meta])* $field:ident : $ty:ty),* $(,)? }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq)]
        pub struct $name {
            $($(#[$field_meta])* pub $field: $ty,)*
        }

        impl $crate::payload::Field for $name {
            const ALIGN: usize =
                $crate::payload::max_alignment(&[$(<$ty as $crate::payload::Field>::ALIGN),*]);
            fn write(&self, writer: &mut $crate::payload::PayloadWriter) {
                writer.pad_to(Self::ALIGN);
                $($crate::payload::Field::write(&self.$field, writer);)*
                writer.pad_to(Self::ALIGN);
            }
            fn read(
                reader: &mut $crate::payload::PayloadReader<'_>,
            ) -> Result<Self, $crate::error::WalError> {
                reader.skip_to(Self::ALIGN)?;
                $(let $field = <$ty as $crate::payload::Field>::read(reader)?;)*
                reader.skip_to(Self::ALIGN)?;
                Ok(Self { $($field),* })
            }
        }
    };
}

/// An op payload: fixed fields, then optionally one variable part
/// (`; name: Type, max N` — at most N elements or bytes).
macro_rules! op {
    (
        $(#[$meta:meta])*
        $name:ident { $($(#[$field_meta:meta])* $field:ident : $ty:ty),* $(,)? }
    ) => {
        record! { $(#[$meta])* $name { $($(#[$field_meta])* $field: $ty),* } }

        impl $name {
            /// The payload record's bytes.
            pub fn encode(&self) -> Result<Vec<u8>, $crate::error::WalError> {
                let mut writer = $crate::payload::PayloadWriter::default();
                $crate::payload::Field::write(self, &mut writer);
                Ok(writer.finish())
            }

            /// Read the payload record from `reader`, padding included.
            pub(crate) fn decode(
                mut reader: $crate::payload::PayloadReader<'_>,
            ) -> Result<Self, $crate::error::WalError> {
                let record = <Self as $crate::payload::Field>::read(&mut reader)?;
                reader.finish()?;
                Ok(record)
            }
        }
    };
    (
        $(#[$meta:meta])*
        $name:ident {
            $($(#[$field_meta:meta])* $field:ident : $ty:ty),* ;
            $(#[$tail_meta:meta])* $tail:ident : $tail_ty:ty, max $max:expr
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq)]
        pub struct $name {
            $($(#[$field_meta])* pub $field: $ty,)*
            $(#[$tail_meta])* pub $tail: $tail_ty,
        }

        impl $name {
            /// The payload record's bytes.
            pub fn encode(&self) -> Result<Vec<u8>, $crate::error::WalError> {
                use $crate::payload::{Field, Tail};
                let mut writer = $crate::payload::PayloadWriter::default();
                $(self.$field.write(&mut writer);)*
                let (raw_length, tail_bytes) =
                    self.$tail.encode_tail(stringify!($tail), $max)?;
                let too_long = $crate::error::WalError::PayloadFieldTooLong {
                    field: stringify!($tail),
                    length: raw_length,
                    max: $max,
                };
                if <$tail_ty as Tail>::SHORT_LENGTH {
                    u16::try_from(raw_length).map_err(|_| too_long)?.write(&mut writer);
                } else {
                    u32::try_from(raw_length).map_err(|_| too_long)?.write(&mut writer);
                }
                writer.tail(&tail_bytes);
                Ok(writer.finish())
            }

            /// Read the payload record from `reader`, padding included.
            pub(crate) fn decode(
                mut reader: $crate::payload::PayloadReader<'_>,
            ) -> Result<Self, $crate::error::WalError> {
                use $crate::payload::{Field, Tail};
                $(let $field = <$ty as Field>::read(&mut reader)?;)*
                let raw_length = if <$tail_ty as Tail>::SHORT_LENGTH {
                    usize::from(u16::read(&mut reader)?)
                } else {
                    u32::read(&mut reader)? as usize
                };
                let $tail = <$tail_ty as Tail>::decode_tail(
                    &mut reader,
                    raw_length,
                    stringify!($tail),
                    $max,
                )?;
                reader.finish()?;
                Ok(Self { $($field,)* $tail })
            }
        }
    };
}

pub(crate) use {op, record};
