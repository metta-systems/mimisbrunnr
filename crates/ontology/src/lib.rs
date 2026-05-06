//! Ontology engine for Mímisbrunnr.
//!
//! This crate owns the *engine*: implication DAG, materialiser, ontology
//! module loader / installer / upgrader / scrubber, and storage-policy
//! resolution. The logical types — [`TagDefinition`], [`TagSemantics`],
//! [`TagRelation`], [`ValueType`], [`StoragePolicy`] — live in
//! `mimisbrunnr-types` per `docs/REWRITE_CONTRACT.md` §3.
//!
//! See `docs/DESIGN.md` §3 (Ontology layer) and §4 (Modules), and
//! `docs/IMPLEMENTATION.md` §10.1 (on-disk persistence). The on-disk B+ tree
//! shape is implemented in a later phase; this crate currently persists
//! state as a CBOR blob (marked `TODO(rewrite-phase-N)`).

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
