//! Error type for the transform pipeline.

/// Errors raised by `mimisbrunnr-transform`.
#[derive(Debug, thiserror::Error)]
pub enum TransformError {
    /// A compression error reported by the underlying codec.
    #[error("compression error: {0}")]
    Compression(String),

    /// A decompression error reported by the underlying codec.
    #[error("decompression error: {0}")]
    Decompression(String),

    /// The chosen `CompressionAlgo` variant is recognised by the spec but
    /// not yet wired up in this build (e.g. `Lz4` before the `lz4_flex`
    /// dependency lands).
    #[error("unsupported algorithm: {0}")]
    UnsupportedAlgo(&'static str),

    /// Encryption was requested but the underlying ciphers are not yet
    /// integrated (Phase 2c placeholder).
    #[error("encryption disabled in this build")]
    EncryptionDisabled,

    /// `TransformPipeline::invert` was called with an `expected_hash` that
    /// does not match the BLAKE3 hash of the recovered plaintext.
    #[error("content hash mismatch")]
    HashMismatch,

    /// Generic I/O error bubbled up from a codec.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}
