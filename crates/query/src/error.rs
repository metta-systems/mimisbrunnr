//! Errors emitted by the query crate.

use thiserror::Error;

/// All errors emitted by the query crate's public surface.
#[derive(Debug, Error)]
pub enum QueryError {
    /// `Query::HasAttr` was given a [`mimisbrunnr_types::CmpOp`] that the
    /// requested index can't service. Today this is only `Contains` (until a
    /// substring index lands) — `Eq`, `Ne`, `Lt`, `Le`, `Gt`, `Ge`, and
    /// `Prefix` are all serviceable.
    #[error("comparison operator `{op}` is not yet supported (TODO(rewrite-phase-N))")]
    UnsupportedCmpOp { op: &'static str },

    /// The bitmap encoding only carries the low 32 bits of an `ObjectId`; if
    /// the engine ever asks for an `ObjectId` whose local sequence exceeds
    /// `u32::MAX` we surface this rather than silently aliasing two IDs.
    #[error(
        "ObjectId local sequence {local} exceeds 32 bits (TODO(rewrite-phase-N): wider bitmap)"
    )]
    ObjectIdTooLarge { local: u64 },

    /// The DSL parser could not consume the input.
    #[error("parse error at byte {position}: {message}")]
    Parse {
        /// Byte offset within the input.
        position: usize,
        /// Human-readable description.
        message: String,
    },

    /// The DSL parser encountered a tag name that the resolver could not
    /// translate into a [`mimisbrunnr_types::TagId`].
    #[error("unknown tag name `{name}`")]
    UnknownTag {
        /// The unresolvable name.
        name: String,
    },

    /// The DSL parser encountered an operator it doesn't know about.
    #[error("unknown operator `{op}`")]
    UnknownOp {
        /// The unresolvable operator.
        op: String,
    },
}
