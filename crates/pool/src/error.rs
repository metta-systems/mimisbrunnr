#[derive(Debug, thiserror::Error)]
pub enum PoolError {
    #[error("storage error: {0}")]
    Storage(#[from] mimisbrunnr_storage::StorageError),

    #[error("disk not found: {0}")]
    DiskNotFound(mimisbrunnr_types::DiskId),

    #[error("disk already in pool: {0}")]
    DiskAlreadyExists(mimisbrunnr_types::DiskId),

    #[error("disk is draining and cannot accept writes: {0}")]
    DiskDraining(mimisbrunnr_types::DiskId),

    #[error("no suitable disk for tier {0:?}")]
    NoSuitableDisk(crate::StorageTier),

    #[error("pool is empty — no disks available")]
    EmptyPool,

    #[error("cannot remove last disk from pool")]
    LastDisk,

    #[error("pool config error: {0}")]
    ConfigError(String),

    #[error("pool config not found near disk: {0}")]
    ConfigNotFound(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}
