mod error;
mod location;
mod object_table;
mod record;

pub use {
    error::MetaError,
    location::ObjectLocation,
    object_table::ObjectTable,
    record::{ObjectRecord, RECORD_SIZE},
};
