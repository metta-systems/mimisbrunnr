//! [`PathProjection`] — a per-context view of how Mímisbrunnr objects map to
//! Unix paths under one path context (DESIGN §12.1).
//!
//! A projection is a pure data structure — no I/O, no engine access. The
//! engine layer materialises paths into a projection at query time, and
//! [`Exporter::plan`](crate::Exporter::plan) reads from it to drive an export
//! plan.

use std::collections::HashMap;
use std::path::PathBuf;

use mimisbrunnr_types::{ObjectId, TagId};
use serde::{Deserialize, Serialize};

use crate::error::UnixError;

/// Per-context Unix-path view of a set of objects.
///
/// `paths` maps each object id to its **relative** path within this context.
/// `root` is purely informational — it records the host-side directory the
/// projection was originally rooted at (for export hints) but is not enforced
/// when looking up paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathProjection {
    /// The `unix-path-context:*` Grouping tag this projection belongs to.
    pub context: TagId,
    /// Host-side root directory the projection was associated with at import
    /// or scan time. Informational; export targets a separately supplied
    /// directory.
    pub root: PathBuf,
    /// Object → relative path mapping within this context.
    pub paths: HashMap<ObjectId, String>,
}

impl PathProjection {
    /// Construct an empty projection for `context`, recording `root` as the
    /// informational host root.
    pub fn new(context: TagId, root: PathBuf) -> Self {
        Self {
            context,
            root,
            paths: HashMap::new(),
        }
    }

    /// Register a mapping `oid → path` within this context.
    ///
    /// `path` must be a relative UTF-8 Unix-style path. It is rejected if it
    /// is empty, absolute (`/...`), contains a `..` traversal component, or
    /// contains a NUL byte.
    pub fn add(&mut self, oid: ObjectId, path: String) -> Result<(), UnixError> {
        validate_relative_path(&path)?;
        self.paths.insert(oid, path);
        Ok(())
    }

    /// Drop the mapping for `oid` if present.
    pub fn remove(&mut self, oid: ObjectId) {
        self.paths.remove(&oid);
    }

    /// Look up the relative path for `oid`.
    pub fn lookup(&self, oid: ObjectId) -> Option<&str> {
        self.paths.get(&oid).map(String::as_str)
    }

    /// Linear scan: find the first object id whose path matches `path`.
    // TODO(rewrite-phase-N): replace with a path → oid index when the
    // path-projection B+ tree (IMPL §10.3) lands.
    pub fn reverse_lookup(&self, path: &str) -> Option<ObjectId> {
        self.paths
            .iter()
            .find(|(_, p)| p.as_str() == path)
            .map(|(oid, _)| *oid)
    }

    /// Iterate over `(ObjectId, path)` pairs.
    pub fn iter(&self) -> impl Iterator<Item = (ObjectId, &str)> {
        self.paths.iter().map(|(oid, p)| (*oid, p.as_str()))
    }

    /// Number of registered mappings.
    pub fn len(&self) -> usize {
        self.paths.len()
    }

    /// Whether the projection is empty.
    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }
}

/// Reject empty / absolute / parent-traversing / NUL-bearing paths.
///
/// Pure helper — does not touch the filesystem. Used by both
/// [`PathProjection::add`] and [`crate::storage::build_path_attr`].
pub(crate) fn validate_relative_path(path: &str) -> Result<(), UnixError> {
    if path.is_empty() {
        return Err(UnixError::EmptyPath);
    }
    if path.starts_with('/') {
        return Err(UnixError::AbsolutePath(PathBuf::from(path)));
    }
    if path.contains('\0') {
        return Err(UnixError::NulInPath(PathBuf::from(path)));
    }
    for component in path.split('/') {
        if component == ".." {
            return Err(UnixError::ParentTraversal(PathBuf::from(path)));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oid(local: u64) -> ObjectId {
        ObjectId::from_parts(0, local)
    }

    #[test]
    fn add_and_lookup_round_trip() {
        let mut p = PathProjection::new(TagId::new(7), PathBuf::from("/host/root"));
        p.add(oid(1), "src/main.rs".into()).unwrap();
        p.add(oid(2), "README.md".into()).unwrap();
        assert_eq!(p.lookup(oid(1)), Some("src/main.rs"));
        assert_eq!(p.lookup(oid(2)), Some("README.md"));
        assert_eq!(p.lookup(oid(3)), None);
        assert_eq!(p.len(), 2);
    }

    #[test]
    fn add_rejects_absolute_path() {
        let mut p = PathProjection::new(TagId::new(1), PathBuf::from("/r"));
        let err = p.add(oid(1), "/etc/passwd".into()).unwrap_err();
        assert!(matches!(err, UnixError::AbsolutePath(_)));
    }

    #[test]
    fn add_rejects_empty_path() {
        let mut p = PathProjection::new(TagId::new(1), PathBuf::from("/r"));
        let err = p.add(oid(1), String::new()).unwrap_err();
        assert!(matches!(err, UnixError::EmptyPath));
    }

    #[test]
    fn add_rejects_parent_traversal() {
        let mut p = PathProjection::new(TagId::new(1), PathBuf::from("/r"));
        let err = p.add(oid(1), "a/../b".into()).unwrap_err();
        assert!(matches!(err, UnixError::ParentTraversal(_)));
    }

    #[test]
    fn add_rejects_nul_byte() {
        let mut p = PathProjection::new(TagId::new(1), PathBuf::from("/r"));
        let err = p.add(oid(1), "a\0b".into()).unwrap_err();
        assert!(matches!(err, UnixError::NulInPath(_)));
    }

    #[test]
    fn remove_drops_mapping() {
        let mut p = PathProjection::new(TagId::new(1), PathBuf::from("/r"));
        p.add(oid(1), "a".into()).unwrap();
        assert!(p.lookup(oid(1)).is_some());
        p.remove(oid(1));
        assert!(p.lookup(oid(1)).is_none());
    }

    #[test]
    fn reverse_lookup_finds_oid() {
        let mut p = PathProjection::new(TagId::new(1), PathBuf::from("/r"));
        p.add(oid(42), "lib/x.so".into()).unwrap();
        assert_eq!(p.reverse_lookup("lib/x.so"), Some(oid(42)));
        assert_eq!(p.reverse_lookup("nope"), None);
    }

    #[test]
    fn iter_yields_all_pairs() {
        let mut p = PathProjection::new(TagId::new(1), PathBuf::from("/r"));
        p.add(oid(1), "a".into()).unwrap();
        p.add(oid(2), "b".into()).unwrap();
        let mut collected: Vec<(ObjectId, String)> =
            p.iter().map(|(o, s)| (o, s.to_string())).collect();
        collected.sort_by_key(|(o, _)| o.local_seq());
        assert_eq!(
            collected,
            vec![(oid(1), "a".into()), (oid(2), "b".into())]
        );
    }
}
