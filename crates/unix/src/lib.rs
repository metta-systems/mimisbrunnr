//! Unix interoperability for Mímisbrunnr (SPECIFICATION-DESIGN§12).
//!
//! Unix paths are not the source of truth — they are *projections* of
//! Mímisbrunnr objects onto a Unix-style tree (SPECIFICATION-DESIGN§12.1). An
//! object's paths are stored as `Attr(unix-path, Value::Text(path))`
//! (UNIX-DESIGN§1); exports are built by applications from queries
//! (UNIX-DESIGN "Export").
//!
//! This crate is intentionally **side-effect-free where possible**:
//! [`PathProjection`] is a pure data structure and the [`Importer`] only
//! walks the host filesystem and produces [`ImportEntry`]s.
//!
//! The engine layer calls into this crate; this crate does **not**
//! depend on the engine.

#![forbid(unsafe_code)]

mod error;
mod import;
mod projection;
mod storage;

pub use error::UnixError;
pub use import::{ImportEntry, ImportKind, Importer};
pub use projection::PathProjection;
pub use storage::{UNIX_PATH_TAG_NAME, build_path_attr};
