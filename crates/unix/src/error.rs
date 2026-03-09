#[derive(Debug, thiserror::Error)]
pub enum UnixError {
    #[error("engine error: {0}")]
    Engine(#[from] mimisbrunnr_engine::EngineError),

    #[error("types error: {0}")]
    Types(#[from] mimisbrunnr_types::Error),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}
