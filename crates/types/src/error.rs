//! Crate-level error type.
//!
//! Per `docs/REWRITE_CONTRACT.md` §2, every crate exposes a single
//! `thiserror`-derived error enum. Public APIs return `Result<T, TypesError>`.

use thiserror::Error;

/// Errors produced by `mimisbrunnr-types`.
#[derive(Debug, Error)]
pub enum TypesError {
    /// CBOR encoding failure (e.g. ran out of memory while writing).
    #[error("CBOR encoding failed: {0}")]
    CborEncode(String),

    /// CBOR decoding failure (truncated input, type mismatch, etc.).
    #[error("CBOR decoding failed: {0}")]
    CborDecode(String),

    /// `Value::Scoped` whose `inner` is itself a `Scoped` — disallowed by
    /// IMPL §4.3 (nested scopes are a format error).
    #[error("Value::Scoped may not directly contain another Scoped value")]
    NestedScopedValue,
}

impl TypesError {
    pub(crate) fn cbor_encode<E: std::fmt::Display>(e: E) -> Self {
        Self::CborEncode(e.to_string())
    }

    pub(crate) fn cbor_decode<E: std::fmt::Display>(e: E) -> Self {
        Self::CborDecode(e.to_string())
    }
}
