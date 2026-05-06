//! Serde helper for fixed-size POD byte images used by the R1b sorted-run
//! CBOR persistence path.
//!
//! `ObjectRecord` (128 B), `ObjectLocation` (48 B), and `BackpointerKey` /
//! `BackpointerValue` (8 / 24 B) are `#[repr(C)]` / `#[repr(C, packed)]`
//! `Pod` types whose on-disk layout is spec-pinned by IMPL §5/§6. We
//! serialise their byte image (via `bytemuck::bytes_of`) rather than
//! field-by-field — the CBOR wire form then carries the exact POD bytes
//! and round-trips back through `bytemuck::from_bytes`.
//!
//! ciborium emits byte arrays via `serialize_bytes`, but other formats
//! (e.g. JSON) emit them as a sequence of integers. The `Visitor` here
//! accepts both forms so the same impls work whichever serialiser the
//! caller picks.

use serde::{
    Deserializer,
    de::{Error, SeqAccess, Visitor},
};

/// Deserialise a length-unspecified byte string via either
/// `serialize_bytes` or a sequence of `u8`. The caller validates length.
pub fn deserialize_bytes<'de, D: Deserializer<'de>>(de: D) -> Result<Vec<u8>, D::Error> {
    struct V;
    impl<'de> Visitor<'de> for V {
        type Value = Vec<u8>;
        fn expecting(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
            f.write_str("byte string")
        }
        fn visit_bytes<E: Error>(self, v: &[u8]) -> Result<Self::Value, E> {
            Ok(v.to_vec())
        }
        fn visit_byte_buf<E: Error>(self, v: Vec<u8>) -> Result<Self::Value, E> {
            Ok(v)
        }
        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
            let mut out = Vec::new();
            while let Some(b) = seq.next_element::<u8>()? {
                out.push(b);
            }
            Ok(out)
        }
    }
    de.deserialize_bytes(V)
}
