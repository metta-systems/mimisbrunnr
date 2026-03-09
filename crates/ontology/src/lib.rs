mod tag_def;
mod implication_dag;
mod materializer;
mod error;

pub use tag_def::{TagDefinition, TagSemantics, TagRelation, ValueType};
pub use implication_dag::ImplicationDag;
pub use materializer::Materializer;
pub use error::OntologyError;
