//! Order-preserving 16-byte encoding of [`Value`] for the range B+ tree
//! (IMPL §9.2).
//!
//! Layout (always 16 bytes):
//!
//! ```text
//! [0]    type tag (matches Value::type_tag)
//! [1..16] type-specific bytes (big-endian / sign-flipped trick)
//! ```
//!
//! The leading type tag enforces inter-type ordering ("Int < Text" etc.)
//! exactly as IMPL §9.2 prescribes ("values of different types are ordered by
//! type-tag prefix"). Within a single type the trailing 15 bytes are
//! lexicographically comparable as raw `[u8; 16]` and reproduce the natural
//! ordering for that variant.

use {
    mimisbrunnr_types::Value,
    serde::{Deserialize, Serialize},
};

/// Length in bytes of a normalised range-index key.
pub const NORMALISED_KEY_LEN: usize = 16;

/// Type-tag byte for [`Value::Text`].
const TYPE_TAG_TEXT: u8 = 0;
/// Type-tag byte for [`Value::Int`].
const TYPE_TAG_INT: u8 = 1;
/// Type-tag byte for [`Value::Float`].
const TYPE_TAG_FLOAT: u8 = 2;
/// Type-tag byte for [`Value::Timestamp`].
const TYPE_TAG_TIMESTAMP: u8 = 3;
/// Type-tag byte for [`Value::Blob`].
const TYPE_TAG_BLOB: u8 = 4;
/// Type-tag byte for [`Value::Scoped`].
const TYPE_TAG_SCOPED: u8 = 5;

/// Order-preserving fixed-size encoding of a [`Value`]; the byte-wise
/// `Ord` matches the spec range-index order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct NormalisedKey(pub [u8; NORMALISED_KEY_LEN]);

impl NormalisedKey {
    /// Build a normalised key from `value`. Long strings/blobs are truncated
    /// to the first 13 bytes; the trailing length byte and continuation flag
    /// disambiguate prefixes per IMPL §9.2.
    pub fn from_value(value: &Value) -> Self {
        let mut out = [0u8; NORMALISED_KEY_LEN];
        match value {
            Value::Int(v) => {
                out[0] = TYPE_TAG_INT;
                let unsigned = (*v as i128) - (i64::MIN as i128); // i64::MIN -> 0
                let bytes = (unsigned as u64).to_be_bytes();
                // 8 bytes of payload at [1..9]; remaining bytes left zero.
                out[1..9].copy_from_slice(&bytes);
            }
            Value::Timestamp(v) => {
                out[0] = TYPE_TAG_TIMESTAMP;
                let unsigned = (*v as i128) - (i64::MIN as i128);
                let bytes = (unsigned as u64).to_be_bytes();
                out[1..9].copy_from_slice(&bytes);
            }
            Value::Float(v) => {
                out[0] = TYPE_TAG_FLOAT;
                let bits = v.to_bits();
                // IEEE 754 sign-flip trick: positives flip top bit so they
                // sort above negatives; negatives flip every bit so larger
                // negatives sort below smaller negatives.
                let key = if (bits >> 63) & 1 == 0 {
                    bits ^ 0x8000_0000_0000_0000
                } else {
                    !bits
                };
                out[1..9].copy_from_slice(&key.to_be_bytes());
            }
            Value::Text(s) => {
                out[0] = TYPE_TAG_TEXT;
                let bytes = s.as_bytes();
                let take = bytes.len().min(13);
                out[1..1 + take].copy_from_slice(&bytes[..take]);
                // [14] continuation flag (0 if entire string fits).
                out[14] = if bytes.len() > 13 { 1 } else { 0 };
                // [15] length byte (saturated to 0xFF for very long strings).
                out[15] = bytes.len().min(255) as u8;
            }
            Value::Blob(b) => {
                out[0] = TYPE_TAG_BLOB;
                let take = b.len().min(13);
                out[1..1 + take].copy_from_slice(&b[..take]);
                out[14] = if b.len() > 13 { 1 } else { 0 };
                out[15] = b.len().min(255) as u8;
            }
            Value::Scoped { context, inner } => {
                // Scoped: leading type tag, then 4-byte context, then a
                // truncated 11-byte slice of the inner value's normalised
                // key payload. This gives prefix scans over a single
                // context. (Spec §9.2 covers the analogous range-index
                // shape; this `NormalisedKey` is a compact in-memory mirror
                // sufficient for the in-memory `RangeIndex` we ship in this
                // phase.)
                out[0] = TYPE_TAG_SCOPED;
                out[1..5].copy_from_slice(&context.raw().to_be_bytes());
                let inner_key = NormalisedKey::from_value(inner);
                // Skip the inner type tag byte; copy [1..12] (11 bytes).
                out[5..16].copy_from_slice(&inner_key.0[1..12]);
            }
        }
        NormalisedKey(out)
    }

    /// Borrow as a byte slice.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mimisbrunnr_types::TagId;

    #[test]
    fn int_ordering_preserved() {
        let a = NormalisedKey::from_value(&Value::Int(1));
        let b = NormalisedKey::from_value(&Value::Int(2));
        let c = NormalisedKey::from_value(&Value::Int(100));
        assert!(a < b);
        assert!(b < c);
    }

    #[test]
    fn negative_ints_sort_below_positive() {
        let neg = NormalisedKey::from_value(&Value::Int(-5));
        let zero = NormalisedKey::from_value(&Value::Int(0));
        let pos = NormalisedKey::from_value(&Value::Int(5));
        assert!(neg < zero);
        assert!(zero < pos);
    }

    #[test]
    fn text_ordering_preserved() {
        let a = NormalisedKey::from_value(&Value::Text("a".into()));
        let b = NormalisedKey::from_value(&Value::Text("b".into()));
        assert!(a < b);
    }

    #[test]
    fn different_types_ordered_by_type_tag() {
        let text = NormalisedKey::from_value(&Value::Text("zzz".into()));
        let int = NormalisedKey::from_value(&Value::Int(0));
        let float = NormalisedKey::from_value(&Value::Float(0.0));
        // type tags: Text=0 < Int=1 < Float=2
        assert!(text < int);
        assert!(int < float);
    }

    #[test]
    fn float_negative_sorts_below_positive() {
        let neg = NormalisedKey::from_value(&Value::Float(-1.0));
        let zero = NormalisedKey::from_value(&Value::Float(0.0));
        let pos = NormalisedKey::from_value(&Value::Float(1.0));
        assert!(neg < zero);
        assert!(zero < pos);
    }

    #[test]
    fn long_text_continuation_flag_set() {
        let short = NormalisedKey::from_value(&Value::Text("hi".into()));
        let long = NormalisedKey::from_value(&Value::Text("a".repeat(50)));
        assert_eq!(short.0[14], 0);
        assert_eq!(long.0[14], 1);
    }

    #[test]
    fn timestamp_ordering_preserved() {
        let early = NormalisedKey::from_value(&Value::Timestamp(100));
        let late = NormalisedKey::from_value(&Value::Timestamp(200));
        assert!(early < late);
    }

    #[test]
    fn scoped_keeps_context_prefix() {
        let inner = Value::Int(42);
        let scoped =
            Value::scoped(TagId::new(7), inner.clone()).unwrap();
        let key = NormalisedKey::from_value(&scoped);
        assert_eq!(key.0[0], TYPE_TAG_SCOPED);
        // Context is stored at bytes [1..5] big-endian.
        let ctx = u32::from_be_bytes(key.0[1..5].try_into().unwrap());
        assert_eq!(ctx, 7);
    }

    #[test]
    fn key_size_is_16() {
        assert_eq!(NORMALISED_KEY_LEN, 16);
        assert_eq!(core::mem::size_of::<NormalisedKey>(), 16);
    }
}
