#[derive(Debug, thiserror::Error)]
pub enum TransformError {
    #[error("compression error: {0}")]
    Compression(String),

    #[error("decompression error: {0}")]
    Decompression(String),

    #[error("encryption error: {0}")]
    Encryption(String),

    #[error("decryption error: {0}")]
    Decryption(String),

    #[error("data integrity failure: expected hash {expected}, got {actual}")]
    IntegrityFailure { expected: String, actual: String },

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}
