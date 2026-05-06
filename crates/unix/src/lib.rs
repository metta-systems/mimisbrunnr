//! Unix interoperability for Mímisbrunnr (DESIGN §12).
//!
//! Unix paths are not the source of truth — they are *projections* of
//! Mímisbrunnr objects onto a Unix-style tree (DESIGN §12.1). The same object
//! can appear at multiple paths under multiple **path contexts**; a context is
//! just a `Grouping` tag, and each per-context path is stored as
//! `Attr(unix-path, Value::Scoped { context, inner: Text(path) })` per
//! IMPLEMENTATION.md §10.3 / §4.3.
//!
//! This crate is intentionally **side-effect-free where possible**:
//! [`PathProjection`] / [`PathContextManager`] are pure data structures, the
//! [`Importer`] only walks the host filesystem and produces [`ImportEntry`]s,
//! and [`Exporter::plan`] returns an `ExportEntry` plan that the engine layer
//! actually executes. [`Exporter::execute`] is the one place we touch the
//! host filesystem on the export path, and even then it takes a content
//! provider callback so we don't depend on any specific blob source.
//!
//! The engine layer (Phase 5) calls into this crate; this crate does **not**
//! depend on the engine.

#![forbid(unsafe_code)]

mod error;
mod export;
mod import;
mod manager;
mod projection;
mod storage;

pub use error::UnixError;
pub use export::{ExportAction, ExportEntry, Exporter};
pub use import::{ImportEntry, ImportKind, Importer};
pub use manager::PathContextManager;
pub use projection::PathProjection;
pub use storage::{UNIX_PATH_TAG_NAME, build_path_attr};
