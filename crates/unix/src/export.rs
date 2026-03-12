use std::{collections::HashMap, path::Path};

use mimisbrunnr_types::{PathProjection, ProjectedEntryType};

use crate::error::UnixError;

/// Exports a path projection to a directory on disk.
pub struct Exporter;

/// Result of exporting a projection.
#[derive(Debug)]
pub struct ExportResult {
    /// Number of files written.
    pub files_written: usize,
    /// Number of symlinks created.
    pub symlinks_created: usize,
    /// Number of directories created.
    pub dirs_created: usize,
    /// Total bytes written.
    pub total_bytes: u64,
}

impl Exporter {
    /// Export a projection to `output_dir`, creating files from blob data.
    ///
    /// Synthesizes directories from paths automatically. Writes files using
    /// blob data looked up by `ObjectId::raw_value()`. Creates symlinks where
    /// the projection specifies them.
    pub fn export_directory(
        projection: &PathProjection,
        blobs: &HashMap<u64, Vec<u8>>,
        output_dir: &Path,
    ) -> Result<ExportResult, UnixError> {
        let proj = projection.with_synthesized_dirs();
        let mut result = ExportResult {
            files_written: 0,
            symlinks_created: 0,
            dirs_created: 0,
            total_bytes: 0,
        };

        // Sort entries so directories come before their contents
        let mut entries: Vec<_> = proj.entries.iter().collect();
        entries.sort_by_key(|e| &e.path);

        for entry in &entries {
            let target_path = output_dir.join(&entry.path);

            match &entry.entry_type {
                ProjectedEntryType::Directory { .. } => {
                    std::fs::create_dir_all(&target_path)?;
                    result.dirs_created += 1;
                }
                ProjectedEntryType::File { mode, .. } => {
                    // Ensure parent exists
                    if let Some(parent) = target_path.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    if let Some(oid) = entry.object {
                        if let Some(data) = blobs.get(&oid.raw_value()) {
                            std::fs::write(&target_path, data)?;
                            result.total_bytes += data.len() as u64;
                            result.files_written += 1;

                            // Set permissions on unix
                            #[cfg(unix)]
                            {
                                use std::os::unix::fs::PermissionsExt;
                                let perms = std::fs::Permissions::from_mode(*mode);
                                let _ = std::fs::set_permissions(&target_path, perms);
                            }
                        }
                    }
                }
                ProjectedEntryType::Symlink { target } => {
                    if let Some(parent) = target_path.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    #[cfg(unix)]
                    {
                        std::os::unix::fs::symlink(target, &target_path)?;
                        result.symlinks_created += 1;
                    }
                    #[cfg(not(unix))]
                    {
                        let _ = target;
                    }
                }
            }
        }

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        arbitrary_int::u48,
        mimisbrunnr_types::{ObjectId, ProjectedEntry},
    };

    fn oid(n: u64) -> ObjectId {
        ObjectId::new(0, u48::from_u64(n))
    }

    #[test]
    fn export_simple_tree() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut proj = PathProjection::new("test");

        let o1 = oid(1);
        let o2 = oid(2);
        proj.add(ProjectedEntry::file(o1, "readme.txt"));
        proj.add(ProjectedEntry::file(o2, "src/main.rs"));

        let mut blobs = HashMap::new();
        blobs.insert(o1.raw_value(), b"# Hello".to_vec());
        blobs.insert(o2.raw_value(), b"fn main() {}".to_vec());

        let result = Exporter::export_directory(&proj, &blobs, tmp.path()).unwrap();
        assert_eq!(result.files_written, 2);
        assert!(result.total_bytes > 0);

        assert_eq!(
            std::fs::read_to_string(tmp.path().join("readme.txt")).unwrap(),
            "# Hello"
        );
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("src/main.rs")).unwrap(),
            "fn main() {}"
        );
    }

    #[test]
    fn export_creates_directories() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut proj = PathProjection::new("test");

        let o1 = oid(1);
        proj.add(ProjectedEntry::file(o1, "a/b/c/deep.txt"));

        let mut blobs = HashMap::new();
        blobs.insert(o1.raw_value(), b"deep".to_vec());

        let result = Exporter::export_directory(&proj, &blobs, tmp.path()).unwrap();
        assert_eq!(result.files_written, 1);
        assert!(tmp.path().join("a/b/c/deep.txt").exists());
    }

    #[test]
    fn export_empty_projection() {
        let tmp = tempfile::TempDir::new().unwrap();
        let proj = PathProjection::new("empty");
        let blobs = HashMap::new();

        let result = Exporter::export_directory(&proj, &blobs, tmp.path()).unwrap();
        assert_eq!(result.files_written, 0);
        assert_eq!(result.total_bytes, 0);
    }

    #[cfg(unix)]
    #[test]
    fn export_symlink() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut proj = PathProjection::new("test");

        let o1 = oid(1);
        proj.add(ProjectedEntry::file(o1, "bin/bash"));
        proj.add(ProjectedEntry::symlink("usr/bin/bash", "../../bin/bash"));

        let mut blobs = HashMap::new();
        blobs.insert(o1.raw_value(), b"#!/bin/bash".to_vec());

        let result = Exporter::export_directory(&proj, &blobs, tmp.path()).unwrap();
        assert_eq!(result.files_written, 1);
        assert_eq!(result.symlinks_created, 1);

        let link = tmp.path().join("usr/bin/bash");
        assert!(link.is_symlink());
        assert_eq!(std::fs::read_link(&link).unwrap().to_str().unwrap(), "../../bin/bash");
    }

    #[test]
    fn export_unscoped_projection() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut proj = PathProjection::unscoped();

        let o1 = oid(1);
        proj.add(ProjectedEntry::file(o1, "file.txt"));

        let mut blobs = HashMap::new();
        blobs.insert(o1.raw_value(), b"content".to_vec());

        let result = Exporter::export_directory(&proj, &blobs, tmp.path()).unwrap();
        assert_eq!(result.files_written, 1);
        assert!(proj.context.is_none());
    }
}
