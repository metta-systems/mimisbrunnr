use std::collections::HashMap;

use crate::{
    Error, ObjectId,
    projection::{PathProjection, ProjectedEntry},
};

/// Manages multiple path contexts (named projections) plus an unscoped projection.
///
/// Named contexts represent specific Unix tree projections (e.g. "project-vesper",
/// "rpi4-sdcard"). The unscoped projection holds unix-path attributes that aren't
/// tied to any particular context.
pub struct PathContextManager {
    contexts: HashMap<String, PathProjection>,
    /// Paths not tied to any named context.
    unscoped: PathProjection,
}

impl PathContextManager {
    pub fn new() -> Self {
        Self {
            contexts: HashMap::new(),
            unscoped: PathProjection::unscoped(),
        }
    }

    /// Access the unscoped (context-free) projection.
    pub fn unscoped(&self) -> &PathProjection {
        &self.unscoped
    }

    /// Mutably access the unscoped projection.
    pub fn unscoped_mut(&mut self) -> &mut PathProjection {
        &mut self.unscoped
    }

    /// Add or replace a path in the unscoped projection.
    pub fn set_unscoped_path(
        &mut self,
        _object: ObjectId,
        path: impl Into<String>,
        entry: ProjectedEntry,
    ) {
        let path = path.into();
        self.unscoped.remove(&path);
        self.unscoped.add(entry);
    }

    /// Remove a path from the unscoped projection.
    pub fn remove_unscoped_path(&mut self, path: &str) -> Result<(), Error> {
        if !self.unscoped.remove(path) {
            return Err(Error::PathNotFound {
                context: None,
                path: path.to_string(),
            });
        }
        Ok(())
    }

    pub fn create_context(&mut self, name: impl Into<String>) -> Result<(), Error> {
        let name = name.into();
        if self.contexts.contains_key(&name) {
            return Err(Error::ContextAlreadyExists(name));
        }
        self.contexts
            .insert(name.clone(), PathProjection::new(name));
        Ok(())
    }

    pub fn get_context(&self, name: &str) -> Result<&PathProjection, Error> {
        self.contexts
            .get(name)
            .ok_or_else(|| Error::ContextNotFound(name.to_string()))
    }

    pub fn get_context_mut(&mut self, name: &str) -> Result<&mut PathProjection, Error> {
        self.contexts
            .get_mut(name)
            .ok_or_else(|| Error::ContextNotFound(name.to_string()))
    }

    pub fn remove_context(&mut self, name: &str) -> Result<PathProjection, Error> {
        self.contexts
            .remove(name)
            .ok_or_else(|| Error::ContextNotFound(name.to_string()))
    }

    pub fn set_path(
        &mut self,
        context: &str,
        _object: ObjectId,
        path: impl Into<String>,
        entry: ProjectedEntry,
    ) -> Result<(), Error> {
        let proj = self.get_context_mut(context)?;
        let path = path.into();
        proj.remove(&path);
        proj.add(entry);
        Ok(())
    }

    pub fn remove_path(&mut self, context: &str, path: &str) -> Result<(), Error> {
        let proj = self.get_context_mut(context)?;
        if !proj.remove(path) {
            return Err(Error::PathNotFound {
                context: Some(context.to_string()),
                path: path.to_string(),
            });
        }
        Ok(())
    }

    pub fn list_contexts(&self) -> Vec<&str> {
        self.contexts.keys().map(|s| s.as_str()).collect()
    }

    pub fn context_count(&self) -> usize {
        self.contexts.len()
    }

    /// Find all contexts (and unscoped) that contain the given object.
    pub fn contexts_for_object(&self, object: ObjectId) -> Vec<(Option<&str>, &ProjectedEntry)> {
        let mut result = Vec::new();
        // Check unscoped
        for entry in &self.unscoped.entries {
            if entry.object == Some(object) {
                result.push((None, entry));
            }
        }
        // Check named contexts
        for (name, proj) in &self.contexts {
            for entry in &proj.entries {
                if entry.object == Some(object) {
                    result.push((Some(name.as_str()), entry));
                }
            }
        }
        result
    }
}

impl Default for PathContextManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use {super::*, arbitrary_int::u48};

    fn oid(n: u64) -> ObjectId {
        ObjectId::new(0, u48::from_u64(n))
    }

    #[test]
    fn create_and_list_contexts() {
        let mut mgr = PathContextManager::new();
        mgr.create_context("project-vesper").unwrap();
        mgr.create_context("rpi4-sdcard").unwrap();

        assert_eq!(mgr.context_count(), 2);
        let contexts = mgr.list_contexts();
        assert!(contexts.contains(&"project-vesper"));
        assert!(contexts.contains(&"rpi4-sdcard"));
    }

    #[test]
    fn duplicate_context_rejected() {
        let mut mgr = PathContextManager::new();
        mgr.create_context("ctx").unwrap();
        assert!(mgr.create_context("ctx").is_err());
    }

    #[test]
    fn set_and_get_path() {
        let mut mgr = PathContextManager::new();
        mgr.create_context("ctx").unwrap();

        let entry = ProjectedEntry::file(oid(1), "src/main.rs");
        mgr.set_path("ctx", oid(1), "src/main.rs", entry).unwrap();

        let proj = mgr.get_context("ctx").unwrap();
        assert_eq!(proj.len(), 1);
        assert_eq!(proj.get("src/main.rs").unwrap().object, Some(oid(1)));
    }

    #[test]
    fn remove_path() {
        let mut mgr = PathContextManager::new();
        mgr.create_context("ctx").unwrap();

        let entry = ProjectedEntry::file(oid(1), "a.txt");
        mgr.set_path("ctx", oid(1), "a.txt", entry).unwrap();
        mgr.remove_path("ctx", "a.txt").unwrap();

        assert!(mgr.get_context("ctx").unwrap().is_empty());
    }

    #[test]
    fn remove_nonexistent_path() {
        let mut mgr = PathContextManager::new();
        mgr.create_context("ctx").unwrap();

        assert!(mgr.remove_path("ctx", "ghost.txt").is_err());
    }

    #[test]
    fn remove_context() {
        let mut mgr = PathContextManager::new();
        mgr.create_context("ctx").unwrap();

        let removed = mgr.remove_context("ctx").unwrap();
        assert_eq!(removed.context.as_deref(), Some("ctx"));
        assert_eq!(mgr.context_count(), 0);
    }

    #[test]
    fn contexts_for_object() {
        let mut mgr = PathContextManager::new();
        mgr.create_context("project").unwrap();
        mgr.create_context("sdcard").unwrap();

        let kernel = oid(42);
        let e1 = ProjectedEntry::file(kernel, "target/release/vesper");
        mgr.set_path("project", kernel, "target/release/vesper", e1)
            .unwrap();

        let e2 = ProjectedEntry::file_with_mode(kernel, "boot/kernel8.img", 0o755);
        mgr.set_path("sdcard", kernel, "boot/kernel8.img", e2)
            .unwrap();

        let contexts = mgr.contexts_for_object(kernel);
        assert_eq!(contexts.len(), 2);
    }

    #[test]
    fn nonexistent_context_error() {
        let mgr = PathContextManager::new();
        assert!(mgr.get_context("nope").is_err());
    }

    #[test]
    fn unscoped_paths() {
        let mut mgr = PathContextManager::new();

        let entry = ProjectedEntry::file(oid(1), "/usr/bin/bash");
        mgr.set_unscoped_path(oid(1), "/usr/bin/bash", entry);

        let entry = ProjectedEntry::file(oid(1), "/bin/bash");
        mgr.set_unscoped_path(oid(1), "/bin/bash", entry);

        assert_eq!(mgr.unscoped().len(), 2);
        assert!(mgr.unscoped().get("/usr/bin/bash").is_some());
        assert!(mgr.unscoped().get("/bin/bash").is_some());
    }

    #[test]
    fn unscoped_remove() {
        let mut mgr = PathContextManager::new();

        let entry = ProjectedEntry::file(oid(1), "a.txt");
        mgr.set_unscoped_path(oid(1), "a.txt", entry);
        mgr.remove_unscoped_path("a.txt").unwrap();

        assert!(mgr.unscoped().is_empty());
        assert!(mgr.remove_unscoped_path("nope").is_err());
    }

    #[test]
    fn contexts_for_object_includes_unscoped() {
        let mut mgr = PathContextManager::new();
        mgr.create_context("ctx").unwrap();

        let obj = oid(10);
        let e1 = ProjectedEntry::file(obj, "/canonical/path");
        mgr.set_unscoped_path(obj, "/canonical/path", e1);

        let e2 = ProjectedEntry::file(obj, "project/path");
        mgr.set_path("ctx", obj, "project/path", e2).unwrap();

        let found = mgr.contexts_for_object(obj);
        assert_eq!(found.len(), 2);
        // One should be unscoped (None), one named (Some)
        assert!(found.iter().any(|(ctx, _)| ctx.is_none()));
        assert!(found.iter().any(|(ctx, _)| *ctx == Some("ctx")));
    }
}
