/// BLAKE3-based content hashing.
///
/// Content hash is always computed on original plaintext — dedup and
/// integrity verification are independent of storage format.
pub struct ContentHasher;

impl ContentHasher {
    /// Compute the BLAKE3 hash of the given data.
    pub fn hash(data: &[u8]) -> [u8; 32] {
        *blake3::hash(data).as_bytes()
    }

    /// Compute the BLAKE3 hash and return it as a hex string.
    pub fn hash_hex(data: &[u8]) -> String {
        blake3::hash(data).to_hex().to_string()
    }

    /// Verify that data matches an expected hash.
    pub fn verify(data: &[u8], expected: &[u8; 32]) -> bool {
        &Self::hash(data) == expected
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic() {
        let data = b"hello mimisbrunnr";
        let h1 = ContentHasher::hash(data);
        let h2 = ContentHasher::hash(data);
        assert_eq!(h1, h2);
    }

    #[test]
    fn different_data_different_hash() {
        let h1 = ContentHasher::hash(b"foo");
        let h2 = ContentHasher::hash(b"bar");
        assert_ne!(h1, h2);
    }

    #[test]
    fn empty_data() {
        let h = ContentHasher::hash(b"");
        assert_ne!(h, [0u8; 32]); // BLAKE3 of empty is non-zero
    }

    #[test]
    fn verify_correct() {
        let data = b"test data";
        let hash = ContentHasher::hash(data);
        assert!(ContentHasher::verify(data, &hash));
    }

    #[test]
    fn verify_incorrect() {
        let data = b"test data";
        let wrong = [0u8; 32];
        assert!(!ContentHasher::verify(data, &wrong));
    }

    #[test]
    fn hex_format() {
        let hex = ContentHasher::hash_hex(b"test");
        assert_eq!(hex.len(), 64); // 32 bytes = 64 hex chars
        assert!(hex.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn large_data() {
        let data = vec![0xABu8; 1024 * 1024]; // 1 MiB
        let h = ContentHasher::hash(&data);
        assert!(ContentHasher::verify(&data, &h));
    }
}
