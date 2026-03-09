#[derive(Debug, thiserror::Error)]
pub enum FuseError {
    #[error("engine error: {0}")]
    Engine(#[from] mimisbrunnr_engine::EngineError),

    #[error("types error: {0}")]
    Types(#[from] mimisbrunnr_types::Error),

    #[error("inode not found: {0}")]
    InodeNotFound(u64),

    #[error("not a directory: inode {0}")]
    NotADirectory(u64),

    #[error("not a file: inode {0}")]
    NotAFile(u64),

    #[error("permission denied")]
    PermissionDenied,

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}
