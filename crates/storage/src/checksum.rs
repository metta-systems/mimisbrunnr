//! The pool's block checksum (FORMAT-DESIGN§1.4): XXH3-64 with seed 0 over a
//! structure's bytes with its checksum slot zeroed. One algorithm serves every
//! slot — headers, blocks, regions, runs, root pointers, superblocks and the
//! journal.
//!
//! Structures whose layout still has a 32-bit checksum field store
//! `checksum64(..) as u32` until the item rewriting each gives it the 64-bit
//! slot (keyed regions already have it): positional regions in BTREE10, the superblock and root pointer in
//! ENGINE12, the journal in JOURNAL9, the pool state root in POOL3, the kv and
//! tag-bitmap pages in the term index (INDEX12–INDEX14), the zone map in ENGINE4.

use xxhash_rust::xxh3::Xxh3;

/// XXH3-64 (seed 0) of `bytes_with_zeroed_slot`.
pub fn checksum64(bytes_with_zeroed_slot: &[u8]) -> u64 {
    xxhash_rust::xxh3::xxh3_64(bytes_with_zeroed_slot)
}

/// Incremental form of [`checksum64`] for a structure checksummed in pieces
/// (a header followed by its payload); equal to `checksum64` over the pieces
/// concatenated.
#[derive(Clone, Default)]
pub struct Checksum64(Xxh3);

impl Checksum64 {
    /// Start an empty checksum.
    pub fn new() -> Self {
        Self(Xxh3::new())
    }

    /// Add the next piece.
    pub fn update(&mut self, piece: &[u8]) {
        self.0.update(piece);
    }

    /// The checksum of every piece so far.
    pub fn finish(&self) -> u64 {
        self.0.digest()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xxh3_64_known_vector() {
        // XXH3_64bits of empty input, seed 0 (xxHash reference).
        assert_eq!(checksum64(&[]), 0x2D06_8005_38D3_94C2);
    }

    #[test]
    fn incremental_equals_one_shot() {
        let header = [7u8; 64];
        let payload: Vec<u8> = (0..300_000u32).map(|i| (i * 31) as u8).collect();
        let mut incremental = Checksum64::new();
        incremental.update(&header);
        incremental.update(&payload);
        let whole: Vec<u8> = header.iter().chain(payload.iter()).copied().collect();
        assert_eq!(incremental.finish(), checksum64(&whole));
    }

    #[test]
    fn one_flipped_bit_changes_the_checksum() {
        let mut block = vec![0u8; 4096];
        let before = checksum64(&block);
        block[2048] ^= 0x10;
        assert_ne!(checksum64(&block), before);
    }
}
