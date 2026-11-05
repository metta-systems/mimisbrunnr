//! Ontology engine for Mímisbrunnr.
//!
//! This crate owns the *engine*: implication DAG, materialiser, ontology
//! module loader / installer / upgrader / scrubber, and storage-policy
//! resolution. The logical types — [`TagDefinition`], [`TagSemantics`],
//! [`TagRelation`], [`ValueType`], [`StoragePolicy`] — live in
//! `mimisbrunnr-types` per `doc/agentic/operations-design.md` (type ownership).
//!
//! See SPECIFICATION-DESIGN§3 (Ontology layer) and SPECIFICATION-DESIGN§4 (Modules), and
//! ONTOLOGY-DESIGN§1 (on-disk persistence), all under `doc/agentic/`. State persists as one
//! CBOR `OntologyImage` per snapshot in the `BtreeKind::Ontology` B+ tree region.

#![forbid(unsafe_code)]

mod error;
mod implication_dag;
mod materializer;
mod module;
mod state;

pub use error::{OntologyError, StorageAxis};
pub use implication_dag::ImplicationDag;
pub use materializer::Materializer;
pub use module::{IdAllocator, InstallResult, OntologyModule};
pub use state::{ModuleRecord, OntologyState};

// Re-export the logical types so downstream callers can use the engine
// without an extra `mimisbrunnr-types` dep just for tag definitions.
pub use mimisbrunnr_types::{
    TagDefinition, TagId, TagRelation, TagSemantics, ValueType,
};
