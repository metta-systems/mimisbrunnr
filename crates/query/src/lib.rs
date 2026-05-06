//! `mimisbrunnr-query` — query algebra evaluator (DESIGN §2.3, §5).
//!
//! Walks the [`mimisbrunnr_types::Query`] tree, consulting the in-memory
//! mirrors exposed by `mimisbrunnr-index` and the
//! [`mimisbrunnr_ontology::OntologyState`] for `IsA` resolution. Result sets
//! are represented as `roaring::RoaringBitmap` while bitmap algebra runs;
//! [`QueryExecutor::evaluate_full`] reconstructs full [`ObjectId`] vectors
//! from a side mapping the engine maintains.
//!
//! ## Bitmap encoding
//!
//! Roaring bitmaps are 32-bit. We pack the *low 32 bits* of an
//! [`ObjectId`]'s 48-bit local sequence into the bitmap. For workloads with
//! up to ~33 M objects on a single node this is unambiguous;
//! [`mimisbrunnr-index`]'s `TagStore` uses the same convention. Cluster /
//! large-pool support requires either a wider bitmap or sharding by a
//! high-order key — see the TODO below.
//!
//! TODO(rewrite-phase-N): replace the 32-bit bitmap with a sharded encoding
//! that covers the full 48-bit local-id space and the 16-bit node prefix.
//!
//! ## DSL
//!
//! [`QueryParser`] parses a small bracketed S-expression form. See its
//! module documentation for the grammar. The richer SQL surface lives in
//! `mimisbrunnr-sql`.
//!
//! ## Public surface
//!
//! - [`QueryExecutor`] — bitmap-algebra evaluator.
//! - [`QueryError`] — error type for the whole crate.
//! - [`FacetedExplorer`] / [`FacetGroup`] — selection-driven facet rollup.
//! - [`QueryParser`] / [`to_sexpr`] — DSL round-trip.
//! - [`explain`] — tree-shaped human-readable plan dump.
//!
//! [`ObjectId`]: mimisbrunnr_types::ObjectId

#![forbid(unsafe_code)]

mod error;
mod executor;
mod explain;
mod facets;
mod parser;

pub use {
    error::QueryError,
    executor::QueryExecutor,
    explain::explain,
    facets::{FacetGroup, FacetedExplorer},
    parser::{QueryParser, to_sexpr},
};
