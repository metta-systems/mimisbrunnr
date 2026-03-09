#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    #[error("tag {0} not found in index")]
    TagNotFound(mimisbrunnr_types::TagId),

    #[error("object {0} not found in forward index")]
    ObjectNotFound(mimisbrunnr_types::ObjectId),

    #[error("duplicate key-value entry for tag {tag}, value hash {value_hash:#x}")]
    DuplicateKv { tag: mimisbrunnr_types::TagId, value_hash: u64 },
}
