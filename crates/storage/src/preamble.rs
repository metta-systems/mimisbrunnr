//! The 8-byte `Preamble` that begins every persistent header, and the reader
//! that names a structure from it (FORMAT-DESIGN§1.3).

use {
    bytemuck::{Pod, Zeroable},
    static_assertions::const_assert_eq,
};

use crate::{block::BtreeKind, error::StorageError};

/// First field of every persistent header.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug, Default, Eq, PartialEq)]
pub struct Preamble {
    /// `[0..4]` which structure, little-endian ASCII (the `MAGIC_*` constants).
    pub magic: u32,
    /// `[4..6]` that structure's layout version.
    pub version: u16,
    /// `[6..8]` [`BtreeKind`] for a region; 0 for every other structure.
    pub kind: u16,
}

const_assert_eq!(core::mem::size_of::<Preamble>(), 8);
const_assert_eq!(core::mem::offset_of!(Preamble, version), 4);
const_assert_eq!(core::mem::offset_of!(Preamble, kind), 6);

/// `MIMR` — superblock (ENGINE-DESIGN§1.1).
pub const MAGIC_SUPERBLOCK: u32 = u32::from_le_bytes(*b"MIMR");
/// `BREG` — B+ tree or positional region, block 0 (BTREE-DESIGN§1.1).
pub const MAGIC_REGION: u32 = u32::from_le_bytes(*b"BREG");
/// `BIDX` — run index block (BTREE-DESIGN§1.2).
pub const MAGIC_RUN_INDEX: u32 = u32::from_le_bytes(*b"BIDX");
/// `BPGS` — page-run index block (BTREE-DESIGN§1.7).
pub const MAGIC_PAGE_RUN_INDEX: u32 = u32::from_le_bytes(*b"BPGS");
/// `JRNL` — journal set (JOURNAL-DESIGN§1.2).
pub const MAGIC_JOURNAL_SET: u32 = u32::from_le_bytes(*b"JRNL");

/// The layout version every structure is written with in this format.
pub const LAYOUT_VERSION: u16 = 1;

/// A structure named by its preamble.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Structure {
    /// Superblock.
    Superblock,
    /// Region block 0, with its tree's kind.
    Region(BtreeKind),
    /// Run index block.
    RunIndex,
    /// Page-run index block.
    PageRunIndex,
    /// Journal set.
    JournalSet,
}

impl Structure {
    /// The magic this structure's preamble carries.
    pub fn magic(self) -> u32 {
        match self {
            Self::Superblock => MAGIC_SUPERBLOCK,
            Self::Region(_) => MAGIC_REGION,
            Self::RunIndex => MAGIC_RUN_INDEX,
            Self::PageRunIndex => MAGIC_PAGE_RUN_INDEX,
            Self::JournalSet => MAGIC_JOURNAL_SET,
        }
    }
}

impl Preamble {
    /// The preamble a new structure is written with.
    pub fn new(structure: Structure) -> Self {
        let kind = match structure {
            Structure::Region(kind) => kind as u16,
            _ => 0,
        };
        Self {
            magic: structure.magic(),
            version: LAYOUT_VERSION,
            kind,
        }
    }

    /// Name the structure whose header begins `bytes`. An unknown magic,
    /// region kind or layout version, or a non-zero kind outside a region,
    /// is [`StorageError::UnknownStructure`]: a reader never guesses.
    pub fn identify(bytes: &[u8]) -> Result<Structure, StorageError> {
        let head = bytes.get(..core::mem::size_of::<Self>()).ok_or(StorageError::BufferTooSmall {
            need: core::mem::size_of::<Self>(),
            have: bytes.len(),
        })?;
        let preamble: Self = bytemuck::pod_read_unaligned(head);
        let unknown = StorageError::UnknownStructure {
            magic: preamble.magic,
            version: preamble.version,
            kind: preamble.kind,
        };
        if preamble.version != LAYOUT_VERSION {
            return Err(unknown);
        }
        let structure = match preamble.magic {
            MAGIC_REGION => Structure::Region(BtreeKind::from_u16(preamble.kind).map_err(|_| unknown)?),
            _ if preamble.kind != 0 => return Err(unknown),
            MAGIC_SUPERBLOCK => Structure::Superblock,
            MAGIC_RUN_INDEX => Structure::RunIndex,
            MAGIC_PAGE_RUN_INDEX => Structure::PageRunIndex,
            MAGIC_JOURNAL_SET => Structure::JournalSet,
            _ => return Err(unknown),
        };
        Ok(structure)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preamble_fixed_bytes() {
        let preamble = Preamble::new(Structure::Region(BtreeKind::Postings));
        assert_eq!(
            bytemuck::bytes_of(&preamble),
            &[b'B', b'R', b'E', b'G', 0x01, 0x00, 0x08, 0x00]
        );
        let superblock = Preamble::new(Structure::Superblock);
        assert_eq!(bytemuck::bytes_of(&superblock), b"MIMR\x01\x00\x00\x00");
    }

    #[test]
    fn identify_names_each_structure() {
        let structures = [
            Structure::Superblock,
            Structure::Region(BtreeKind::ObjectRoots),
            Structure::Region(BtreeKind::ReconcileScan),
            Structure::RunIndex,
            Structure::PageRunIndex,
            Structure::JournalSet,
        ];
        for structure in structures {
            let mut header = [0u8; 64];
            header[..8].copy_from_slice(bytemuck::bytes_of(&Preamble::new(structure)));
            assert_eq!(Preamble::identify(&header).unwrap(), structure);
        }
    }

    fn identify_raw(magic: &[u8; 4], version: u16, kind: u16) -> Result<Structure, StorageError> {
        let preamble = Preamble {
            magic: u32::from_le_bytes(*magic),
            version,
            kind,
        };
        Preamble::identify(bytemuck::bytes_of(&preamble))
    }

    #[test]
    fn identify_rejects_what_it_does_not_know() {
        let unknown = |result: Result<Structure, StorageError>| {
            matches!(result, Err(StorageError::UnknownStructure { .. }))
        };
        assert!(unknown(identify_raw(b"XXXX", 1, 0)), "unknown magic");
        assert!(unknown(identify_raw(b"BREG", 2, 4)), "newer region layout");
        assert!(unknown(identify_raw(b"BREG", 1, 27)), "unknown tree kind");
        assert!(unknown(identify_raw(b"JRNL", 1, 3)), "kind outside a region");
        assert!(unknown(identify_raw(b"MIMR", 0, 0)), "version 0");
        assert!(matches!(
            Preamble::identify(&[0u8; 7]),
            Err(StorageError::BufferTooSmall { need: 8, have: 7 })
        ));
    }
}
