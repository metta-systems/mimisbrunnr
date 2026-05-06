//! Radix-tree address translation for the Object / Location tables
//! (IMPL §5 "Address translation").
//!
//! Logical layout:
//!
//! - Each leaf holds [`LEAF_RECORDS`] `ObjectRecord`s (or [`LEAF_RECORDS_LOCATION`]
//!   `ObjectLocation`s in the location table).
//! - Each inner node holds [`INNER_FANOUT`] child pointers.
//! - The tree's *root level* equals the count of inner levels above the leaf;
//!   `MAX_LEVELS = 3` bounds this and covers the full 48-bit local-id space.
//!
//! The helpers in this module are pure; they produce no I/O. The live radix
//! tree (with COW write path and journal-reclaim integration) is a later
//! phase — see `TODO(rewrite-phase-N)` in the table modules.

use crate::error::MetaError;

/// Maximum number of inner levels above the leaf. Per IMPL §5, depth 3
/// (4 levels total) covers `2044 × 16380³ ≈ 9 P` objects.
pub const MAX_LEVELS: usize = 3;

/// `ObjectRecord` slots per leaf node (IMPL §5 "Node capacities").
pub const LEAF_RECORDS: usize = 2044;

/// `ObjectLocation` slots per location-table leaf node (IMPL §6.1).
pub const LEAF_RECORDS_LOCATION: usize = 5440;

/// Child-pointer slots per inner node (IMPL §5 "Node capacities").
pub const INNER_FANOUT: usize = 16380;

/// Decomposition of a local object id into a root-to-leaf radix path.
///
/// `child_path[0]` selects the first inner-level child *below the root*;
/// successive entries descend toward the leaf. Slots beyond `used_levels`
/// are zero and meaningless. `leaf_slot` is the final positional offset
/// inside the leaf node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RadixPath {
    /// Positional slot within the leaf node `[0..LEAF_RECORDS)`.
    pub leaf_slot: u16,
    /// Inner-level child indices, root-to-leaf. Entries `[used_levels..]` are 0.
    pub child_path: [u16; MAX_LEVELS],
    /// Number of meaningful entries in `child_path`. Equals the tree's
    /// `root_level`.
    pub used_levels: u8,
}

impl RadixPath {
    /// Construct a path from raw fields.
    pub const fn new(leaf_slot: u16, child_path: [u16; MAX_LEVELS], used_levels: u8) -> Self {
        Self {
            leaf_slot,
            child_path,
            used_levels,
        }
    }
}

/// Translate `oid_local` to a [`RadixPath`] for a tree whose root sits at
/// `root_level` inner levels above the leaf (`root_level == 0` means the
/// tree is leaf-only).
///
/// Uses [`LEAF_RECORDS`] (2044) as the leaf-fanout — i.e. this is the
/// **object table** translation. The location table leaf-fanout (5440) is
/// different; reuse [`oid_to_radix_path_with_leaf_fanout`] for that.
///
/// Returns `Err(MetaError::OidOutOfRange)` if `oid_local` exceeds the
/// tree's addressable range or `root_level > MAX_LEVELS`.
pub fn oid_to_radix_path(oid_local: u64, root_level: u8) -> Result<RadixPath, MetaError> {
    oid_to_radix_path_with_leaf_fanout(oid_local, root_level, LEAF_RECORDS as u64)
}

/// Variant of [`oid_to_radix_path`] for the location table. See
/// [`LEAF_RECORDS_LOCATION`].
pub fn oid_to_radix_path_location(
    oid_local: u64,
    root_level: u8,
) -> Result<RadixPath, MetaError> {
    oid_to_radix_path_with_leaf_fanout(oid_local, root_level, LEAF_RECORDS_LOCATION as u64)
}

/// Generic version: caller supplies the leaf fanout. Inner fanout is fixed
/// at [`INNER_FANOUT`] in both the object and location table designs.
pub fn oid_to_radix_path_with_leaf_fanout(
    oid_local: u64,
    root_level: u8,
    leaf_fanout: u64,
) -> Result<RadixPath, MetaError> {
    if root_level as usize > MAX_LEVELS {
        return Err(MetaError::OidOutOfRange {
            oid_local,
            root_level,
        });
    }

    let mut idx = oid_local;
    let leaf_slot_u64 = idx % leaf_fanout;
    idx /= leaf_fanout;

    let mut child_path = [0u16; MAX_LEVELS];
    for slot in child_path.iter_mut().take(root_level as usize) {
        *slot = (idx % INNER_FANOUT as u64) as u16;
        idx /= INNER_FANOUT as u64;
    }

    if idx != 0 {
        return Err(MetaError::OidOutOfRange {
            oid_local,
            root_level,
        });
    }

    Ok(RadixPath {
        leaf_slot: leaf_slot_u64 as u16,
        child_path,
        used_levels: root_level,
    })
}

/// Inverse of [`oid_to_radix_path`]: rebuild the local id from a path.
pub fn radix_path_to_oid(path: &RadixPath, root_level: u8) -> u64 {
    radix_path_to_oid_with_leaf_fanout(path, root_level, LEAF_RECORDS as u64)
}

/// Inverse of [`oid_to_radix_path_location`].
pub fn radix_path_to_oid_location(path: &RadixPath, root_level: u8) -> u64 {
    radix_path_to_oid_with_leaf_fanout(path, root_level, LEAF_RECORDS_LOCATION as u64)
}

/// Generic inverse for caller-supplied leaf fanout.
pub fn radix_path_to_oid_with_leaf_fanout(
    path: &RadixPath,
    root_level: u8,
    leaf_fanout: u64,
) -> u64 {
    let used = root_level as usize;
    // Horner-style reconstruction, descending from the deepest inner level
    // to the leaf.
    let mut idx: u64 = 0;
    for level in (0..used).rev() {
        idx = idx * INNER_FANOUT as u64 + path.child_path[level] as u64;
    }
    idx * leaf_fanout + path.leaf_slot as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leaf_only_zero() {
        let p = oid_to_radix_path(0, 0).unwrap();
        assert_eq!(p.leaf_slot, 0);
        assert_eq!(p.child_path, [0, 0, 0]);
        assert_eq!(p.used_levels, 0);
    }

    #[test]
    fn leaf_only_max() {
        let p = oid_to_radix_path(2043, 0).unwrap();
        assert_eq!(p.leaf_slot, 2043);
        assert_eq!(p.child_path, [0, 0, 0]);
        assert_eq!(p.used_levels, 0);
    }

    #[test]
    fn first_oid_in_second_leaf() {
        // oid_local = 2044 with root_level=1: first slot of the second leaf,
        // i.e. child_path[0] == 1, leaf_slot == 0.
        let p = oid_to_radix_path(2044, 1).unwrap();
        assert_eq!(p.leaf_slot, 0);
        assert_eq!(p.child_path[0], 1);
        assert_eq!(p.used_levels, 1);
    }

    #[test]
    fn round_trip_object_table_samples() {
        // (oid, root_level) pairs spanning depths 0..=3.
        let cases: &[(u64, u8)] = &[
            (0, 0),
            (2043, 0),
            (2044, 1),
            (2044 + 17, 1),
            (2044 * (INNER_FANOUT as u64) - 1, 1),
            (2044 * (INNER_FANOUT as u64), 2),
            (123_456_789, 2),
            (1_234_567_890_123, 3),
        ];
        for (oid, lvl) in cases.iter().copied() {
            let p = oid_to_radix_path(oid, lvl).unwrap();
            let back = radix_path_to_oid(&p, lvl);
            assert_eq!(back, oid, "round-trip failed for oid={} lvl={}", oid, lvl);
        }
    }

    #[test]
    fn round_trip_location_table_samples() {
        let cases: &[(u64, u8)] = &[
            (0, 0),
            (5439, 0),
            (5440, 1),
            (5440 * (INNER_FANOUT as u64), 2),
            (1_000_000_000_000, 3),
        ];
        for (oid, lvl) in cases.iter().copied() {
            let p = oid_to_radix_path_location(oid, lvl).unwrap();
            let back = radix_path_to_oid_location(&p, lvl);
            assert_eq!(
                back, oid,
                "location round-trip failed for oid={} lvl={}",
                oid, lvl
            );
        }
    }

    #[test]
    fn out_of_range_at_depth_zero() {
        // Leaf-only tree can address at most LEAF_RECORDS items.
        let r = oid_to_radix_path(LEAF_RECORDS as u64, 0);
        assert!(matches!(r, Err(MetaError::OidOutOfRange { .. })));
    }

    #[test]
    fn out_of_range_at_depth_one() {
        // Depth-1 tree caps at LEAF_RECORDS * INNER_FANOUT.
        let cap = (LEAF_RECORDS as u64) * (INNER_FANOUT as u64);
        let r = oid_to_radix_path(cap, 1);
        assert!(matches!(r, Err(MetaError::OidOutOfRange { .. })));
    }

    #[test]
    fn rejects_root_level_above_max() {
        let r = oid_to_radix_path(0, (MAX_LEVELS as u8) + 1);
        assert!(matches!(r, Err(MetaError::OidOutOfRange { .. })));
    }
}
