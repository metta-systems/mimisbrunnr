//! BLAKE3 content hashing (DESIGN §9.1, IMPL §1.4).
//!
//! The content hash is always taken over the **plaintext**, before
//! compression / padding / encryption. Dedup and integrity verification are
//! independent of storage format.

/// One-shot BLAKE3 hasher.
///
/// `ContentHasher` is a unit struct so it composes cleanly with the rest of
/// the pipeline (`TransformPipeline` keeps no hashing state).
#[derive(Debug, Default, Clone, Copy)]
pub struct ContentHasher;

impl ContentHasher {
    /// Construct a hasher. The struct is stateless; this exists for
    /// symmetry with the streaming `Hasher` API.
    pub const fn new() -> Self {
        Self
    }

    /// Compute the BLAKE3 hash of `data` (plaintext).
    pub fn hash(&self, data: &[u8]) -> [u8; 32] {
        *blake3::hash(data).as_bytes()
    }
}

/// Streaming BLAKE3 hasher.
///
/// Wraps `blake3::Hasher` so callers don't need to depend on `blake3`
/// directly. Use when the plaintext arrives in pieces (e.g. chunked uploads
/// or extent-by-extent reads).
#[derive(Debug, Clone, Default)]
pub struct Hasher {
    inner: blake3::Hasher,
}

impl Hasher {
    /// Create an empty streaming hasher.
    pub fn new() -> Self {
        Self {
            inner: blake3::Hasher::new(),
        }
    }

    /// Absorb another chunk of plaintext.
    pub fn update(&mut self, data: &[u8]) -> &mut Self {
        self.inner.update(data);
        self
    }

    /// Finalise and return the 32-byte digest.
    pub fn finalize(self) -> [u8; 32] {
        *self.inner.finalize().as_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Canonical empty-string BLAKE3 digest, from the BLAKE3 reference
    /// vectors. (`blake3 --no-names < /dev/null`.)
    const BLAKE3_EMPTY: [u8; 32] = [
        0xaf, 0x13, 0x49, 0xb9, 0xf5, 0xf9, 0xa1, 0xa6, 0xa0, 0x40, 0x4d, 0xea, 0x36, 0xdc, 0xc9,
        0x49, 0x9b, 0xcb, 0x25, 0xc9, 0xad, 0xc1, 0x12, 0xb7, 0xcc, 0x9a, 0x93, 0xca, 0xe4, 0x1f,
        0x32, 0x62,
    ];

    #[test]
    fn empty_is_canonical_blake3_vector() {
        assert_eq!(ContentHasher::new().hash(b""), BLAKE3_EMPTY);
    }

    #[test]
    fn deterministic() {
        let h = ContentHasher::new();
        let data = b"hello mimisbrunnr";
        assert_eq!(h.hash(data), h.hash(data));
    }

    #[test]
    fn different_data_different_hash() {
        let h = ContentHasher::new();
        assert_ne!(h.hash(b"foo"), h.hash(b"bar"));
    }

    #[test]
    fn streaming_matches_one_shot() {
        let parts: [&[u8]; 3] = [b"hello, ", b"mimis", b"brunnr"];
        let one_shot = ContentHasher::new().hash(b"hello, mimisbrunnr");
        let mut s = Hasher::new();
        for p in parts {
            s.update(p);
        }
        assert_eq!(s.finalize(), one_shot);
    }

    #[test]
    fn streaming_empty_is_canonical_vector() {
        assert_eq!(Hasher::new().finalize(), BLAKE3_EMPTY);
    }
}
