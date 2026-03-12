use std::{collections::HashMap, path::Path};

use {
    mimisbrunnr_engine::Engine,
    mimisbrunnr_types::{ObjectId, PathContextManager, ProjectedEntry, TagId},
};

use crate::error::UnixError;

/// Imports a Unix directory tree into the engine, optionally into a named context.
pub struct Importer;

/// Result of importing a directory tree.
#[derive(Debug)]
pub struct ImportResult {
    /// Number of objects created.
    pub objects_created: usize,
    /// Number of objects deduplicated (same content hash).
    pub objects_deduped: usize,
    /// Total bytes imported.
    pub total_bytes: u64,
    /// The context name, if any.
    pub context: Option<String>,
}

impl Importer {
    /// Import a directory tree, creating objects and populating path entries.
    ///
    /// Walks the directory recursively, creating an object for each regular file.
    /// Deduplicates by content hash. Auto-tags based on file extension if
    /// tag IDs are provided in `extension_tags`.
    ///
    /// If `context_name` is `Some`, entries are added to a named context.
    /// If `None`, entries are added to the unscoped projection.
    pub fn import_directory(
        engine: &mut Engine,
        context_mgr: &mut PathContextManager,
        root: &Path,
        context_name: Option<&str>,
        extension_tags: &HashMap<String, TagId>,
        now_ms: u64,
    ) -> Result<ImportResult, UnixError> {
        // Create context if needed
        if let Some(name) = context_name {
            if context_mgr.get_context(name).is_err() {
                context_mgr.create_context(name)?;
            }
        }

        let mut result = ImportResult {
            objects_created: 0,
            objects_deduped: 0,
            total_bytes: 0,
            context: context_name.map(String::from),
        };

        // Track content hashes for dedup
        let mut hash_to_oid: HashMap<[u8; 32], ObjectId> = HashMap::new();

        Self::walk_dir(
            engine,
            context_mgr,
            root,
            root,
            context_name,
            extension_tags,
            &mut hash_to_oid,
            &mut result,
            now_ms,
        )?;

        Ok(result)
    }

    #[expect(clippy::too_many_arguments)]
    fn walk_dir(
        engine: &mut Engine,
        context_mgr: &mut PathContextManager,
        root: &Path,
        dir: &Path,
        context_name: Option<&str>,
        extension_tags: &HashMap<String, TagId>,
        hash_to_oid: &mut HashMap<[u8; 32], ObjectId>,
        result: &mut ImportResult,
        now_ms: u64,
    ) -> Result<(), UnixError> {
        let entries = std::fs::read_dir(dir)?;

        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            let metadata = entry.metadata()?;

            if metadata.is_dir() {
                Self::walk_dir(
                    engine,
                    context_mgr,
                    root,
                    &path,
                    context_name,
                    extension_tags,
                    hash_to_oid,
                    result,
                    now_ms,
                )?;
            } else if metadata.is_file() {
                let relative = path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .to_string();

                let content = std::fs::read(&path)?;
                let hash = mimisbrunnr_transform::ContentHasher::hash(&content);
                result.total_bytes += content.len() as u64;

                let oid = if let Some(&existing) = hash_to_oid.get(&hash) {
                    result.objects_deduped += 1;
                    existing
                } else {
                    let oid = engine.create_object(now_ms).map_err(UnixError::Engine)?;
                    engine
                        .write_blob(oid, &content, now_ms)
                        .map_err(UnixError::Engine)?;
                    hash_to_oid.insert(hash, oid);
                    result.objects_created += 1;

                    // Auto-tag by extension
                    if let Some(ext) = path.extension().and_then(|e| e.to_str())
                        && let Some(&tag_id) = extension_tags.get(ext)
                    {
                        let _ = engine.add_tag(oid, tag_id, now_ms);
                    }

                    oid
                };

                // Add to path context or unscoped
                #[cfg(unix)]
                let mode = {
                    use std::os::unix::fs::PermissionsExt;
                    metadata.permissions().mode()
                };
                #[cfg(not(unix))]
                let mode = 0o644u32;

                let proj_entry = ProjectedEntry::file_with_mode(oid, &relative, mode);
                match context_name {
                    Some(name) => context_mgr.set_path(name, oid, &relative, proj_entry)?,
                    None => context_mgr.set_unscoped_path(oid, &relative, proj_entry),
                }
            }
            // Skip symlinks and other special files for now
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use {super::*, std::fs, tempfile::TempDir};

    fn setup() -> (Engine, PathContextManager) {
        (Engine::new(0), PathContextManager::new())
    }

    fn create_test_tree(dir: &Path) {
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("README.md"), "# Hello").unwrap();
        fs::write(dir.join("Cargo.toml"), "[package]\nname = \"test\"").unwrap();
        fs::write(dir.join("src/main.rs"), "fn main() {}").unwrap();
        fs::write(dir.join("src/lib.rs"), "pub fn hello() {}").unwrap();
    }

    #[test]
    fn import_directory() {
        let tmp = TempDir::new().unwrap();
        create_test_tree(tmp.path());

        let (mut engine, mut ctx_mgr) = setup();
        let ext_tags = HashMap::new();

        let result = Importer::import_directory(
            &mut engine,
            &mut ctx_mgr,
            tmp.path(),
            Some("test-project"),
            &ext_tags,
            1000,
        )
        .unwrap();

        assert_eq!(result.objects_created, 4);
        assert_eq!(result.objects_deduped, 0);
        assert!(result.total_bytes > 0);

        // Verify context was created with entries
        let proj = ctx_mgr.get_context("test-project").unwrap();
        assert_eq!(proj.len(), 4);
        assert!(proj.get("README.md").is_some());
        assert!(proj.get("src/main.rs").is_some());
    }

    #[test]
    fn import_with_dedup() {
        let tmp = TempDir::new().unwrap();
        // Two files with identical content
        fs::write(tmp.path().join("a.txt"), "same content").unwrap();
        fs::write(tmp.path().join("b.txt"), "same content").unwrap();
        fs::write(tmp.path().join("c.txt"), "different").unwrap();

        let (mut engine, mut ctx_mgr) = setup();
        let ext_tags = HashMap::new();

        let result = Importer::import_directory(
            &mut engine,
            &mut ctx_mgr,
            tmp.path(),
            Some("ctx"),
            &ext_tags,
            1000,
        )
        .unwrap();

        assert_eq!(result.objects_created, 2); // a.txt + c.txt
        assert_eq!(result.objects_deduped, 1); // b.txt is same as a.txt
    }

    #[test]
    fn import_with_auto_tag() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("code.rs"), "fn main() {}").unwrap();
        fs::write(tmp.path().join("data.json"), "{}").unwrap();

        let (mut engine, mut ctx_mgr) = setup();

        // Register tags first
        use mimisbrunnr_ontology::{TagDefinition, TagSemantics};
        let rs_tag = TagId::new(100);
        engine
            .register_tag(TagDefinition::new(rs_tag, "rust", TagSemantics::Label))
            .unwrap();

        let mut ext_tags = HashMap::new();
        ext_tags.insert("rs".to_string(), rs_tag);

        let result = Importer::import_directory(
            &mut engine,
            &mut ctx_mgr,
            tmp.path(),
            Some("ctx"),
            &ext_tags,
            1000,
        )
        .unwrap();

        assert_eq!(result.objects_created, 2);

        // The .rs file should have been tagged
        let proj = ctx_mgr.get_context("ctx").unwrap();
        let rs_entry = proj.get("code.rs").unwrap();
        let oid = rs_entry.object.unwrap();
        let tags = engine.tags(oid).unwrap();
        assert!(tags.contains(&rs_tag));
    }

    #[test]
    fn import_empty_directory() {
        let tmp = TempDir::new().unwrap();

        let (mut engine, mut ctx_mgr) = setup();
        let ext_tags = HashMap::new();

        let result = Importer::import_directory(
            &mut engine,
            &mut ctx_mgr,
            tmp.path(),
            Some("empty"),
            &ext_tags,
            1000,
        )
        .unwrap();

        assert_eq!(result.objects_created, 0);
        assert_eq!(result.total_bytes, 0);
    }

    #[test]
    fn import_nested_directories() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join("a/b/c")).unwrap();
        fs::write(tmp.path().join("a/b/c/deep.txt"), "deep file").unwrap();
        fs::write(tmp.path().join("a/top.txt"), "top file").unwrap();

        let (mut engine, mut ctx_mgr) = setup();
        let ext_tags = HashMap::new();

        let result = Importer::import_directory(
            &mut engine,
            &mut ctx_mgr,
            tmp.path(),
            Some("nested"),
            &ext_tags,
            1000,
        )
        .unwrap();

        assert_eq!(result.objects_created, 2);

        let proj = ctx_mgr.get_context("nested").unwrap();
        assert!(proj.get("a/b/c/deep.txt").is_some());
        assert!(proj.get("a/top.txt").is_some());
    }

    #[test]
    fn import_unscoped() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("file.txt"), "hello").unwrap();

        let (mut engine, mut ctx_mgr) = setup();
        let ext_tags = HashMap::new();

        let result = Importer::import_directory(
            &mut engine,
            &mut ctx_mgr,
            tmp.path(),
            None,
            &ext_tags,
            1000,
        )
        .unwrap();

        assert_eq!(result.objects_created, 1);
        assert!(result.context.is_none());

        // Should be in unscoped projection
        assert!(ctx_mgr.unscoped().get("file.txt").is_some());
        assert_eq!(ctx_mgr.context_count(), 0); // no named context created
    }
}
