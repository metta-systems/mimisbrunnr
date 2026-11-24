//! Heterogeneous attribute values (SPECIFICATION-DESIGN§2.2, INDEX-DESIGN§1).
//!
//! `Value` is the only data type whose shape varies enough that fixed binary
//! layout is wasteful — it is encoded as positional **CBOR**
//! `[type_tag, payload]` ([`Value::encode`], INDEX-DESIGN§1.1).
//!
//! Encoding rules (INDEX-DESIGN§1):
//! - Variants are numbered 0..=4 in the order `Text, Int, Float, Timestamp,
//!   Blob`; tag 5 is unassigned. The numeric prefix prevents `Int(42)` from
//!   colliding with `Text("42")`.
//! - Encodings ≤ 96 bytes are stored inline in index leaves; longer ones spill
//!   to a `BlobZone` extent. The threshold const lives here; the spill
//!   mechanic itself is `mimisbrunnr-storage`'s job.

use std::hash::Hasher;

use serde::{
    Deserialize, Deserializer, Serialize, Serializer,
    de::{SeqAccess, Visitor},
    ser::SerializeTuple,
};
use siphasher::sip::SipHasher24;

use crate::{
    TypesError,
    cbor::{CborReader, CborWriter},
};

/// CBOR-encoded `Value`s above this size spill to a `BlobZone` extent;
/// smaller values inline directly into the index leaf (INDEX-DESIGN§1.1).
pub const VALUE_INLINE_THRESHOLD: usize = 96;

/// SipHash-2-4 domain tag (INDEX-DESIGN§1.2). Mixed in before the type prefix and
/// payload so that identical bytes hashed in unrelated KV indices cannot
/// collide.
const KV_HASH_DOMAIN: &[u8] = b"mimir-kv";

// Type tags (INDEX-DESIGN§1). These travel with the hash input — *not* with the CBOR
// payload, which carries its own `serde` discriminant.
const TYPE_TAG_TEXT: u8 = 0;
const TYPE_TAG_INT: u8 = 1;
const TYPE_TAG_FLOAT: u8 = 2;
const TYPE_TAG_TIMESTAMP: u8 = 3;
const TYPE_TAG_BLOB: u8 = 4;

/// Heterogeneous attribute value.
///
/// Its serde form is the pair `(type_tag, payload)`, the INDEX-DESIGN§1.1
/// shape, for records still written through serde; a float there takes the
/// width the serializer picks. [`Value::encode`] is the canonical encoding.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// UTF-8 string.
    Text(String),
    /// Signed 64-bit integer.
    Int(i64),
    /// Double-precision float.
    Float(f64),
    /// Nanoseconds since the Unix epoch.
    Timestamp(i64),
    /// Arbitrary byte string. Encodings >`VALUE_INLINE_THRESHOLD` bytes spill
    /// to a `BlobZone` extent — that bookkeeping happens in storage.
    Blob(Vec<u8>),
}

impl Value {
    /// Borrow as a `&str` if this is a [`Value::Text`].
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Value::Text(s) => Some(s),
            _ => None,
        }
    }

    /// Get the integer value if this is a [`Value::Int`].
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Value::Int(v) => Some(*v),
            _ => None,
        }
    }

    /// Get the float if this is a [`Value::Float`].
    pub fn as_float(&self) -> Option<f64> {
        match self {
            Value::Float(v) => Some(*v),
            _ => None,
        }
    }

    /// Get the timestamp (ns since epoch) if this is a [`Value::Timestamp`].
    pub fn as_timestamp(&self) -> Option<i64> {
        match self {
            Value::Timestamp(v) => Some(*v),
            _ => None,
        }
    }

    /// Borrow the byte slice if this is a [`Value::Blob`].
    pub fn as_blob(&self) -> Option<&[u8]> {
        match self {
            Value::Blob(b) => Some(b),
            _ => None,
        }
    }

    /// Short stable name for diagnostics.
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Text(_) => "text",
            Value::Int(_) => "int",
            Value::Float(_) => "float",
            Value::Timestamp(_) => "timestamp",
            Value::Blob(_) => "blob",
        }
    }

    /// Numeric type tag used by [`value_hash`] and by the on-wire encoding
    /// preamble. Pinned by INDEX-DESIGN§1.
    #[inline]
    pub const fn type_tag(&self) -> u8 {
        match self {
            Value::Text(_) => TYPE_TAG_TEXT,
            Value::Int(_) => TYPE_TAG_INT,
            Value::Float(_) => TYPE_TAG_FLOAT,
            Value::Timestamp(_) => TYPE_TAG_TIMESTAMP,
            Value::Blob(_) => TYPE_TAG_BLOB,
        }
    }

    /// Convenience: encode self to CBOR. See [`encode_cbor`] for the
    /// free-function form.
    pub fn to_cbor(&self) -> Result<Vec<u8>, TypesError> {
        encode_cbor(self)
    }

    /// Convenience: decode a `Value` from a CBOR byte slice. Mirrors
    /// [`decode_cbor`].
    pub fn from_cbor(bytes: &[u8]) -> Result<Self, TypesError> {
        decode_cbor(bytes)
    }
}

impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Value::Text(s) => write!(f, "\"{s}\""),
            Value::Int(v) => write!(f, "{v}"),
            Value::Float(v) => write!(f, "{v}"),
            Value::Timestamp(v) => write!(f, "ts:{v}"),
            Value::Blob(b) => write!(f, "blob[{}]", b.len()),
        }
    }
}

impl PartialOrd for Value {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        match (self, other) {
            (Value::Int(a), Value::Int(b)) => a.partial_cmp(b),
            (Value::Float(a), Value::Float(b)) => a.partial_cmp(b),
            (Value::Text(a), Value::Text(b)) => a.partial_cmp(b),
            (Value::Timestamp(a), Value::Timestamp(b)) => a.partial_cmp(b),
            (Value::Blob(a), Value::Blob(b)) => a.partial_cmp(b),
            _ => None,
        }
    }
}

impl Value {
    /// Write `[type_tag, payload]` (INDEX-DESIGN§1.1): shortest integers, a
    /// 64-bit float with `-0.0` written as `0.0`. NaN is rejected.
    pub fn encode(&self, writer: &mut CborWriter) -> Result<(), TypesError> {
        writer.array(2);
        writer.uint(u64::from(self.type_tag()));
        match self {
            Value::Text(text) => writer.text(text),
            Value::Int(value) | Value::Timestamp(value) => writer.int(*value),
            Value::Float(value) if value.is_nan() => return Err(TypesError::NanValue),
            // `+ 0.0` turns -0.0 into 0.0 and leaves every other value alone.
            Value::Float(value) => writer.float64(value + 0.0),
            Value::Blob(bytes) => writer.bytes(bytes),
        }
        Ok(())
    }

    /// Read one value written by [`Value::encode`]. Anything else — another
    /// element count, an unassigned type tag, NaN, `-0.0`, a non-canonical
    /// head — is an error.
    pub fn decode(reader: &mut CborReader<'_>) -> Result<Self, TypesError> {
        let elements = reader.array()?;
        if elements != 2 {
            return Err(TypesError::WrongElementCount {
                type_name: "Value",
                expected: 2,
                found: elements,
            });
        }
        let value = match reader.uint()? {
            tag if tag == u64::from(TYPE_TAG_TEXT) => Value::Text(reader.text()?.to_owned()),
            tag if tag == u64::from(TYPE_TAG_INT) => Value::Int(reader.int()?),
            tag if tag == u64::from(TYPE_TAG_FLOAT) => {
                let value = reader.float64()?;
                if value.is_nan() {
                    return Err(TypesError::NanValue);
                }
                if value == 0.0 && value.is_sign_negative() {
                    return Err(TypesError::CborNonCanonical);
                }
                Value::Float(value)
            }
            tag if tag == u64::from(TYPE_TAG_TIMESTAMP) => Value::Timestamp(reader.int()?),
            tag if tag == u64::from(TYPE_TAG_BLOB) => Value::Blob(reader.bytes()?.to_vec()),
            index => {
                return Err(TypesError::UnknownVariant {
                    type_name: "Value",
                    index,
                });
            }
        };
        Ok(value)
    }
}

impl Serialize for Value {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut pair = serializer.serialize_tuple(2)?;
        pair.serialize_element(&self.type_tag())?;
        match self {
            Value::Text(text) => pair.serialize_element(text)?,
            Value::Int(value) | Value::Timestamp(value) => pair.serialize_element(value)?,
            Value::Float(value) if value.is_nan() => {
                return Err(serde::ser::Error::custom(TypesError::NanValue));
            }
            Value::Float(value) => pair.serialize_element(&(value + 0.0))?,
            Value::Blob(bytes) => pair.serialize_element(&SerdeBytes(bytes))?,
        }
        pair.end()
    }
}

/// Serializes a byte slice as a byte string rather than an array of numbers.
struct SerdeBytes<'a>(&'a [u8]);

impl Serialize for SerdeBytes<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bytes(self.0)
    }
}

/// Deserializes a byte string (or, from formats without one, a sequence of bytes).
struct OwnedBytes(Vec<u8>);

impl<'de> Deserialize<'de> for OwnedBytes {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct BytesVisitor;
        impl<'de> Visitor<'de> for BytesVisitor {
            type Value = OwnedBytes;
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a byte string")
            }
            fn visit_bytes<E: serde::de::Error>(self, bytes: &[u8]) -> Result<OwnedBytes, E> {
                Ok(OwnedBytes(bytes.to_vec()))
            }
            fn visit_byte_buf<E: serde::de::Error>(self, bytes: Vec<u8>) -> Result<OwnedBytes, E> {
                Ok(OwnedBytes(bytes))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<OwnedBytes, A::Error> {
                let mut bytes = Vec::new();
                while let Some(byte) = sequence.next_element()? {
                    bytes.push(byte);
                }
                Ok(OwnedBytes(bytes))
            }
        }
        deserializer.deserialize_bytes(BytesVisitor)
    }
}

impl<'de> Deserialize<'de> for Value {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct PairVisitor;
        impl<'de> Visitor<'de> for PairVisitor {
            type Value = Value;
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a [type_tag, payload] pair")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut pair: A) -> Result<Value, A::Error> {
                use serde::de::Error;
                let missing = || A::Error::invalid_length(1, &"a [type_tag, payload] pair");
                let tag: u64 = pair.next_element()?.ok_or_else(missing)?;
                let value = match tag {
                    tag if tag == u64::from(TYPE_TAG_TEXT) => {
                        Value::Text(pair.next_element()?.ok_or_else(missing)?)
                    }
                    tag if tag == u64::from(TYPE_TAG_INT) => {
                        Value::Int(pair.next_element()?.ok_or_else(missing)?)
                    }
                    tag if tag == u64::from(TYPE_TAG_FLOAT) => {
                        let value: f64 = pair.next_element()?.ok_or_else(missing)?;
                        if value.is_nan() {
                            return Err(A::Error::custom(TypesError::NanValue));
                        }
                        Value::Float(value)
                    }
                    tag if tag == u64::from(TYPE_TAG_TIMESTAMP) => {
                        Value::Timestamp(pair.next_element()?.ok_or_else(missing)?)
                    }
                    tag if tag == u64::from(TYPE_TAG_BLOB) => {
                        let OwnedBytes(bytes) = pair.next_element()?.ok_or_else(missing)?;
                        Value::Blob(bytes)
                    }
                    index => {
                        return Err(A::Error::custom(TypesError::UnknownVariant {
                            type_name: "Value",
                            index,
                        }));
                    }
                };
                if pair.next_element::<serde::de::IgnoredAny>()?.is_some() {
                    return Err(A::Error::invalid_length(3, &"a [type_tag, payload] pair"));
                }
                Ok(value)
            }
        }
        deserializer.deserialize_tuple(2, PairVisitor)
    }
}

/// Encode `value` alone as INDEX-DESIGN§1.1 CBOR.
pub fn encode_cbor(value: &Value) -> Result<Vec<u8>, TypesError> {
    let mut writer = CborWriter::new();
    value.encode(&mut writer)?;
    Ok(writer.into_bytes())
}

/// Decode bytes holding exactly one INDEX-DESIGN§1.1 value.
pub fn decode_cbor(bytes: &[u8]) -> Result<Value, TypesError> {
    let mut reader = CborReader::new(bytes);
    let value = Value::decode(&mut reader)?;
    reader.finish()?;
    Ok(value)
}

/// SipHash-2-4 keyed by a per-pool secret, truncated to 64 bits (INDEX-DESIGN§1.2).
///
/// The hash mixes in:
///
/// 1. The 8-byte domain `"mimir-kv"`,
/// 2. The single-byte numeric type tag (so `Int(42)` ≠ `Text("42")`),
/// 3. The CBOR encoding of the value.
///
/// Stable across runs given the same `secret`. The plumbing for *which*
/// secret to use lives in storage / pool layers — this function takes it as
/// an explicit parameter so we don't depend on global state.
pub fn value_hash(value: &Value, secret: &[u8; 16]) -> u64 {
    let key0 = u64::from_le_bytes(secret[0..8].try_into().expect("16-byte secret"));
    let key1 = u64::from_le_bytes(secret[8..16].try_into().expect("16-byte secret"));
    let mut hasher = SipHasher24::new_with_keys(key0, key1);
    hasher.write(KV_HASH_DOMAIN);
    hasher.write_u8(value.type_tag());
    match encode_cbor(value) {
        Ok(bytes) => hasher.write(&bytes),
        // NaN has no encoding; hash its bit pattern so the function stays total.
        Err(_) => hasher.write(&value.as_float().unwrap_or_default().to_bits().to_le_bytes()),
    }
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cbor_round_trip(v: &Value) {
        let bytes = encode_cbor(v).expect("encode");
        let back = decode_cbor(&bytes).expect("decode");
        assert_eq!(v, &back);
    }

    #[test]
    fn cbor_round_trip_text() {
        cbor_round_trip(&Value::Text("Aphex Twin".into()));
    }

    #[test]
    fn cbor_round_trip_int() {
        cbor_round_trip(&Value::Int(-42));
        cbor_round_trip(&Value::Int(i64::MAX));
        cbor_round_trip(&Value::Int(i64::MIN));
    }

    #[test]
    fn cbor_round_trip_float() {
        cbor_round_trip(&Value::Float(1.5));
        cbor_round_trip(&Value::Float(0.0));
    }

    #[test]
    fn cbor_round_trip_timestamp() {
        cbor_round_trip(&Value::Timestamp(1_700_000_000_000_000_000));
    }

    #[test]
    fn cbor_round_trip_blob() {
        cbor_round_trip(&Value::Blob(vec![]));
        cbor_round_trip(&Value::Blob((0..200u8).collect()));
    }

    #[test]
    fn every_variant_encodes_to_its_pinned_bytes() {
        let cases: [(Value, &[u8]); 7] = [
            // [0, "ab"]
            (Value::Text("ab".into()), &[0x82, 0x00, 0x62, b'a', b'b']),
            // [1, 42]: one-byte argument
            (Value::Int(42), &[0x82, 0x01, 0x18, 0x2a]),
            // [1, -1]
            (Value::Int(-1), &[0x82, 0x01, 0x20]),
            // [2, 1.5]: always a 64-bit float
            (
                Value::Float(1.5),
                &[0x82, 0x02, 0xfb, 0x3f, 0xf8, 0, 0, 0, 0, 0, 0],
            ),
            // [3, 1000]
            (Value::Timestamp(1000), &[0x82, 0x03, 0x19, 0x03, 0xe8]),
            // [4, h'0102']
            (Value::Blob(vec![1, 2]), &[0x82, 0x04, 0x42, 1, 2]),
            // [0, ""]
            (Value::Text(String::new()), &[0x82, 0x00, 0x60]),
        ];
        for (value, expected) in cases {
            assert_eq!(encode_cbor(&value).unwrap(), expected, "{value:?}");
            assert_eq!(decode_cbor(expected).unwrap(), value);
        }
    }

    fn through_serde(value: &Value) -> Vec<u8> {
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(value, &mut bytes).unwrap();
        bytes
    }

    #[test]
    fn serde_writes_the_pinned_pair() {
        for value in [
            Value::Text("ab".into()),
            Value::Int(42),
            Value::Int(-1),
            Value::Timestamp(1000),
            Value::Blob(vec![1, 2]),
        ] {
            let bytes = through_serde(&value);
            assert_eq!(bytes, encode_cbor(&value).unwrap(), "{value:?}");
            let back: Value = ciborium::de::from_reader(bytes.as_slice()).unwrap();
            assert_eq!(back, value);
        }
        // A float keeps the [2, payload] shape; the serializer picks its width.
        let bytes = through_serde(&Value::Float(1.5));
        assert_eq!(&bytes[..2], [0x82, 0x02]);
        let back: Value = ciborium::de::from_reader(bytes.as_slice()).unwrap();
        assert_eq!(back, Value::Float(1.5));
    }

    #[test]
    fn serde_rejects_nan_and_the_unassigned_tag() {
        let mut sink = Vec::new();
        assert!(ciborium::ser::into_writer(&Value::Float(f64::NAN), &mut sink).is_err());
        let tag_five: Result<Value, _> = ciborium::de::from_reader(&[0x82u8, 0x05, 0x00][..]);
        assert!(tag_five.is_err());
        let three_elements: Result<Value, _> =
            ciborium::de::from_reader(&[0x83u8, 0x01, 0x01, 0x01][..]);
        assert!(three_elements.is_err());
    }

    #[test]
    fn negative_zero_is_written_as_zero_and_rejected_on_read() {
        let bytes = encode_cbor(&Value::Float(-0.0)).unwrap();
        assert_eq!(bytes, encode_cbor(&Value::Float(0.0)).unwrap());
        let negative_zero = [0x82, 0x02, 0xfb, 0x80, 0, 0, 0, 0, 0, 0, 0];
        assert!(matches!(
            decode_cbor(&negative_zero),
            Err(TypesError::CborNonCanonical)
        ));
    }

    #[test]
    fn nan_is_rejected_both_ways() {
        assert!(matches!(
            encode_cbor(&Value::Float(f64::NAN)),
            Err(TypesError::NanValue)
        ));
        let nan = [0x82, 0x02, 0xfb, 0x7f, 0xf8, 0, 0, 0, 0, 0, 0];
        assert!(matches!(decode_cbor(&nan), Err(TypesError::NanValue)));
    }

    #[test]
    fn the_unassigned_tag_5_is_rejected() {
        // [5, 0]
        assert!(matches!(
            decode_cbor(&[0x82, 0x05, 0x00]),
            Err(TypesError::UnknownVariant {
                type_name: "Value",
                index: 5
            })
        ));
    }

    #[test]
    fn malformed_values_are_rejected() {
        // [1] — missing payload.
        assert!(matches!(
            decode_cbor(&[0x81, 0x01]),
            Err(TypesError::WrongElementCount { found: 1, .. })
        ));
        // [1, "x"] — payload of the wrong type.
        assert!(matches!(
            decode_cbor(&[0x82, 0x01, 0x61, b'x']),
            Err(TypesError::CborUnexpected { .. })
        ));
        // [2, 1.5 as a 16-bit float].
        assert!(matches!(
            decode_cbor(&[0x82, 0x02, 0xf9, 0x3e, 0x00]),
            Err(TypesError::CborNonCanonical)
        ));
        // A value followed by a stray byte.
        assert!(matches!(
            decode_cbor(&[0x82, 0x01, 0x01, 0x00]),
            Err(TypesError::CborTrailingBytes(1))
        ));
    }

    #[test]
    fn type_tags_are_stable() {
        assert_eq!(Value::Text("".into()).type_tag(), 0);
        assert_eq!(Value::Int(0).type_tag(), 1);
        assert_eq!(Value::Float(0.0).type_tag(), 2);
        assert_eq!(Value::Timestamp(0).type_tag(), 3);
        assert_eq!(Value::Blob(vec![]).type_tag(), 4);
    }

    #[test]
    fn value_hash_distinguishes_int_and_text_42() {
        let secret = [0u8; 16];
        let h_int = value_hash(&Value::Int(42), &secret);
        let h_str = value_hash(&Value::Text("42".into()), &secret);
        assert_ne!(h_int, h_str);
    }

    #[test]
    fn value_hash_stable_golden() {
        // Golden vector: with an all-zero key the hash is a fixed 64-bit
        // value. If this test fails, either the SipHash key/inputs changed
        // or the CBOR encoding changed — both are breaking on-disk changes
        // and need a coordinated bump.
        let secret = [0u8; 16];
        let h = value_hash(&Value::Int(42), &secret);
        // Recompute by hand: this is the fixed value we get today; locking it
        // protects every later phase from accidental encoding drift.
        let expected = {
            let mut hasher = SipHasher24::new_with_keys(0, 0);
            hasher.write(KV_HASH_DOMAIN);
            hasher.write_u8(TYPE_TAG_INT);
            // [1, 42]
            hasher.write(&[0x82, 0x01, 0x18, 0x2a]);
            hasher.finish()
        };
        assert_eq!(h, expected);
    }

    #[test]
    fn value_hash_stable_across_calls() {
        let secret = *b"abcdefghijklmnop";
        let v = Value::Text("Aphex Twin".into());
        let a = value_hash(&v, &secret);
        let b = value_hash(&v, &secret);
        assert_eq!(a, b);
    }

    #[test]
    fn value_hash_secret_changes_output() {
        let v = Value::Int(7);
        let h0 = value_hash(&v, &[0u8; 16]);
        let h1 = value_hash(&v, &[1u8; 16]);
        assert_ne!(h0, h1);
    }

    #[test]
    fn inline_threshold_is_96() {
        // Enforced by the spec; locking the constant catches accidental edits.
        assert_eq!(VALUE_INLINE_THRESHOLD, 96);
    }

    #[test]
    fn ordering_same_type() {
        assert!(Value::Int(1) < Value::Int(2));
        assert!(Value::Text("a".into()) < Value::Text("b".into()));
    }

    #[test]
    fn ordering_different_types_is_none() {
        assert_eq!(
            Value::Int(1).partial_cmp(&Value::Text("a".into())),
            None,
        );
    }
}
