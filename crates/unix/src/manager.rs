//! [`PathContextManager`] — registry of per-context [`PathProjection`]s
//! (DESIGN §12.2).
//!
//! Holds many projections keyed by their `unix-path-context:*` Grouping tag
//! id. Validation against the live ontology happens at `create_context` time;
//! after that the manager is purely in-memory.
//!
//! ## Persistence
//!
//! [`PathContextManager::serialise`] / [`PathContextManager::deserialise`]
//! provide a CBOR-blob round-trip for now. The B+ tree shape from
//! IMPLEMENTATION.md §10.3 will replace this in a later phase.
// TODO(rewrite-phase-N): persist via the path-projection btree per IMPL §10.3.

use std::collections::HashMap;
use std::path::PathBuf;

use mimisbrunnr_ontology::OntologyState;
use mimisbrunnr_types::{ObjectId, TagId, TagSemantics};
use serde::{Deserialize, Serialize};

use crate::error::UnixError;
use crate::projection::PathProjection;

/// Registry of [`PathProjection`]s, one per `unix-path-context:*` tag.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathContextManager {
    /// Context tag id → projection.
    pub projections: HashMap<TagId, PathProjection>,
}

impl PathContextManager {
    /// Empty manager.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a new projection rooted at `root` for `context`.
    ///
    /// Validates against `ontology` that:
    /// 1. `context` is a tag the ontology knows.
    /// 2. The tag's semantics is [`TagSemantics::Grouping`] (DESIGN §12.2).
    ///
    /// Fails if the projection already exists.
    pub fn create_context(
        &mut self,
        ontology: &OntologyState,
        context: TagId,
        root: PathBuf,
    ) -> Result<(), UnixError> {
        if self.projections.contains_key(&context) {
            return Err(UnixError::ContextExists(context));
        }
        let def = ontology
            .tags
            .get(&context)
            .ok_or(UnixError::UnknownTag(context))?;
        if !matches!(def.semantics, TagSemantics::Grouping) {
            return Err(UnixError::NotAGroupingTag(context));
        }
        self.projections
            .insert(context, PathProjection::new(context, root));
        Ok(())
    }

    /// Drop a projection.
    pub fn drop_context(&mut self, context: TagId) -> Option<PathProjection> {
        self.projections.remove(&context)
    }

    /// Borrow the projection for `context`.
    pub fn get(&self, context: TagId) -> Option<&PathProjection> {
        self.projections.get(&context)
    }

    /// Mutably borrow the projection for `context`.
    pub fn get_mut(&mut self, context: TagId) -> Option<&mut PathProjection> {
        self.projections.get_mut(&context)
    }

    /// Convenience: project `oid` under `context`.
    pub fn project(&self, oid: ObjectId, context: TagId) -> Option<&str> {
        self.get(context).and_then(|p| p.lookup(oid))
    }

    /// Number of registered projections.
    pub fn len(&self) -> usize {
        self.projections.len()
    }

    /// Whether the manager is empty.
    pub fn is_empty(&self) -> bool {
        self.projections.is_empty()
    }

    /// CBOR-encode for persistence.
    // TODO(rewrite-phase-N): persist via path-projection btree per IMPL §10.3.
    pub fn serialise(&self) -> Result<Vec<u8>, UnixError> {
        let mut buf = Vec::new();
        ciborium::ser::into_writer(self, &mut buf).map_err(|e| UnixError::Cbor(e.to_string()))?;
        Ok(buf)
    }

    /// Decode from CBOR.
    // TODO(rewrite-phase-N): persist via path-projection btree per IMPL §10.3.
    pub fn deserialise(bytes: &[u8]) -> Result<Self, UnixError> {
        ciborium::de::from_reader(bytes).map_err(|e| UnixError::Cbor(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use mimisbrunnr_ontology::{IdAllocator, OntologyModule};
    use mimisbrunnr_types::{TagDefinition, TagSemantics};

    use super::*;

    fn ontology_with(name: &str, semantics: TagSemantics) -> (OntologyState, TagId) {
        let mut state = OntologyState::new();
        let mut alloc = IdAllocator::new();
        let module = OntologyModule {
            id: "unix-interop".into(),
            version: "0.1.0".into(),
            name: "unix-interop".into(),
            tags: vec![TagDefinition {
                id: TagId::new(0),
                name: name.into(),
                semantics,
                implies: vec![],
                storage: None,
            }],
            implications: vec![],
        };
        state.install(module, &mut alloc).unwrap();
        let tag = state.names[name];
        (state, tag)
    }

    fn oid(local: u64) -> ObjectId {
        ObjectId::from_parts(0, local)
    }

    #[test]
    fn create_and_drop_context() {
        let (ont, ctx) =
            ontology_with("unix-path-context:rpi4", TagSemantics::Grouping);
        let mut mgr = PathContextManager::new();
        mgr.create_context(&ont, ctx, PathBuf::from("/host/rpi4")).unwrap();
        assert!(mgr.get(ctx).is_some());
        assert_eq!(mgr.len(), 1);

        let dropped = mgr.drop_context(ctx).unwrap();
        assert_eq!(dropped.context, ctx);
        assert!(mgr.is_empty());
    }

    #[test]
    fn create_context_rejects_duplicate() {
        let (ont, ctx) =
            ontology_with("unix-path-context:dup", TagSemantics::Grouping);
        let mut mgr = PathContextManager::new();
        mgr.create_context(&ont, ctx, PathBuf::from("/r")).unwrap();
        let err = mgr
            .create_context(&ont, ctx, PathBuf::from("/r2"))
            .unwrap_err();
        assert!(matches!(err, UnixError::ContextExists(_)));
    }

    #[test]
    fn create_context_rejects_non_grouping() {
        let (ont, ctx) = ontology_with("not-grouping", TagSemantics::Label);
        let mut mgr = PathContextManager::new();
        let err = mgr
            .create_context(&ont, ctx, PathBuf::from("/r"))
            .unwrap_err();
        assert!(matches!(err, UnixError::NotAGroupingTag(_)));
    }

    #[test]
    fn create_context_rejects_unknown_tag() {
        let ont = OntologyState::new();
        let mut mgr = PathContextManager::new();
        let err = mgr
            .create_context(&ont, TagId::new(999), PathBuf::from("/r"))
            .unwrap_err();
        assert!(matches!(err, UnixError::UnknownTag(_)));
    }

    #[test]
    fn project_returns_path() {
        let (ont, ctx) =
            ontology_with("unix-path-context:p", TagSemantics::Grouping);
        let mut mgr = PathContextManager::new();
        mgr.create_context(&ont, ctx, PathBuf::from("/r")).unwrap();
        mgr.get_mut(ctx).unwrap().add(oid(1), "a/b.txt".into()).unwrap();
        assert_eq!(mgr.project(oid(1), ctx), Some("a/b.txt"));
        assert_eq!(mgr.project(oid(2), ctx), None);
    }

    #[test]
    fn cbor_round_trip() {
        let (ont, ctx) =
            ontology_with("unix-path-context:rt", TagSemantics::Grouping);
        let mut mgr = PathContextManager::new();
        mgr.create_context(&ont, ctx, PathBuf::from("/host/rt")).unwrap();
        mgr.get_mut(ctx)
            .unwrap()
            .add(oid(1), "src/main.rs".into())
            .unwrap();
        mgr.get_mut(ctx)
            .unwrap()
            .add(oid(2), "README.md".into())
            .unwrap();

        let bytes = mgr.serialise().unwrap();
        let back = PathContextManager::deserialise(&bytes).unwrap();
        assert_eq!(mgr, back);
        assert_eq!(back.project(oid(1), ctx), Some("src/main.rs"));
    }
}
