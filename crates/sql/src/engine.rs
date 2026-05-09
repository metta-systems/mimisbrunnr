//! [`SqlEngine`] — user-facing driver tying parse → plan → execute → format.

use log::trace;
use mimisbrunnr_ontology::OntologyState;
use mimisbrunnr_query::QueryExecutor;
use mimisbrunnr_types::ObjectId;

use crate::{
    error::SqlError,
    parser::SqlParser,
    planner::{Aggregate, PlannedQuery, Projection, SqlPlanner},
};

/// Result of executing a SQL statement.
#[derive(Debug, Clone, PartialEq)]
pub enum SqlOutput {
    /// `SELECT *` — the matching object IDs (post LIMIT/OFFSET).
    Rows(Vec<ObjectId>),
    /// `SELECT COUNT(*)` — bitmap cardinality.
    Count(u64),
}

/// Driver for parsing, planning and executing SQL against a borrowed
/// [`QueryExecutor`].
pub struct SqlEngine<'a> {
    /// The bitmap-algebra evaluator.
    pub executor: QueryExecutor<'a>,
    /// Ontology used for tag-name resolution at plan time.
    pub ontology: &'a OntologyState,
}

impl<'a> SqlEngine<'a> {
    /// Convenience constructor.
    pub fn new(executor: QueryExecutor<'a>, ontology: &'a OntologyState) -> Self {
        Self { executor, ontology }
    }

    /// Parse, plan, and execute `sql`. Returns either a list of object IDs
    /// (for `SELECT *`) or a count (for `SELECT COUNT(*)`).
    pub fn execute(&self, sql: &str) -> Result<SqlOutput, SqlError> {
        trace!("sql::execute {sql}");
        let stmt = SqlParser::parse(sql)?;
        let planned = SqlPlanner::plan(&stmt, self.ontology)?;
        self.run(&planned)
    }

    /// Execute an already-planned query — exposed so callers that have a
    /// pre-built `PlannedQuery` (e.g. a future query cache) don't have to
    /// re-parse.
    pub fn run(&self, planned: &PlannedQuery) -> Result<SqlOutput, SqlError> {
        match planned.aggregate {
            Aggregate::Count => {
                let bitmap = self.executor.evaluate(&planned.query)?;
                // LIMIT/OFFSET on a COUNT(*) is unusual but we honour it: it
                // simulates "count of the first N rows after skipping M". This
                // mirrors what most SQL engines do.
                let card = match (planned.offset, planned.limit) {
                    (None, None) => bitmap.len(),
                    (offset, limit) => {
                        let total = bitmap.len();
                        let off = offset.unwrap_or(0) as u64;
                        let after_offset = total.saturating_sub(off);
                        match limit {
                            Some(l) => after_offset.min(l as u64),
                            None => after_offset,
                        }
                    }
                };
                debug_assert_eq!(planned.projection, Projection::Count);
                Ok(SqlOutput::Count(card))
            }
            Aggregate::None => {
                debug_assert_eq!(planned.projection, Projection::Star);
                let mut ids = self.executor.evaluate_full(&planned.query)?;
                ids.sort();
                let mut iter = ids.into_iter();
                if let Some(off) = planned.offset {
                    for _ in 0..off {
                        if iter.next().is_none() {
                            break;
                        }
                    }
                }
                let collected: Vec<ObjectId> = match planned.limit {
                    Some(l) => iter.take(l).collect(),
                    None => iter.collect(),
                };
                Ok(SqlOutput::Rows(collected))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use mimisbrunnr_index::{ForwardIndex, KvIndex, RangeIndex, TagIndex};
    use mimisbrunnr_ontology::{IdAllocator, OntologyModule, OntologyState};
    use mimisbrunnr_query::QueryExecutor;
    use mimisbrunnr_types::{ObjectId, TagDefinition, TagId, TagSemantics, Value};

    use super::*;

    fn label(name: &str) -> TagDefinition {
        TagDefinition {
            id: TagId::new(0),
            name: name.into(),
            semantics: TagSemantics::Label,
            implies: vec![],
            storage: None,
        }
    }

    fn ontology_with(names: &[&str]) -> OntologyState {
        let mut state = OntologyState::new();
        let mut alloc = IdAllocator::new();
        let module = OntologyModule {
            id: "test".into(),
            version: "0.1.0".into(),
            name: "test".into(),
            tags: names.iter().map(|n| label(n)).collect(),
            implications: vec![],
            relations: vec![],
        };
        state.install(module, &mut alloc).unwrap();
        state
    }

    fn oid(local: u64) -> ObjectId {
        ObjectId::from_parts(0, local)
    }

    struct Fixture {
        tag: TagIndex,
        kv: KvIndex,
        range: RangeIndex,
        fwd: ForwardIndex,
        ont: OntologyState,
    }

    impl Fixture {
        fn new(tag_names: &[&str]) -> Self {
            Self {
                tag: TagIndex::new(),
                kv: KvIndex::new(),
                range: RangeIndex::new(),
                fwd: ForwardIndex::new(),
                ont: ontology_with(tag_names),
            }
        }

        fn engine(&self) -> SqlEngine<'_> {
            let executor =
                QueryExecutor::new(&self.tag, &self.kv, &self.range, &self.fwd, &self.ont);
            SqlEngine::new(executor, &self.ont)
        }
    }

    #[test]
    fn end_to_end_tag_eq() {
        let mut f = Fixture::new(&["song"]);
        let song = f.ont.names["song"];
        for i in 1..=5u64 {
            f.tag.add_member(song, oid(i));
        }
        let out = f
            .engine()
            .execute("SELECT * FROM objects WHERE tag = 'song'")
            .unwrap();
        let SqlOutput::Rows(ids) = out else {
            panic!("expected Rows, got {out:?}");
        };
        let locals: Vec<u64> = ids.iter().map(|o| o.local_seq()).collect();
        assert_eq!(locals, vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn end_to_end_count_star() {
        let mut f = Fixture::new(&["electronics"]);
        let e = f.ont.names["electronics"];
        for i in 1..=7u64 {
            f.tag.add_member(e, oid(i));
        }
        let out = f
            .engine()
            .execute("SELECT COUNT(*) FROM objects WHERE tag = 'electronics'")
            .unwrap();
        assert_eq!(out, SqlOutput::Count(7));
    }

    #[test]
    fn end_to_end_attr_eq() {
        let mut f = Fixture::new(&["artist"]);
        let artist = f.ont.names["artist"];
        f.kv.insert(artist, &Value::Text("Aphex".into()), 1);
        f.kv.insert(artist, &Value::Text("Aphex".into()), 2);
        f.kv.insert(artist, &Value::Text("Eno".into()), 3);

        let out = f
            .engine()
            .execute("SELECT * FROM objects WHERE artist = 'Aphex'")
            .unwrap();
        let SqlOutput::Rows(ids) = out else {
            panic!("expected Rows");
        };
        let locals: Vec<u64> = ids.iter().map(|o| o.local_seq()).collect();
        assert_eq!(locals, vec![1, 2]);
    }

    #[test]
    fn end_to_end_limit_offset() {
        let mut f = Fixture::new(&["song"]);
        let song = f.ont.names["song"];
        for i in 1..=5u64 {
            f.tag.add_member(song, oid(i));
        }
        let out = f
            .engine()
            .execute("SELECT * FROM objects WHERE tag = 'song' LIMIT 2 OFFSET 1")
            .unwrap();
        let SqlOutput::Rows(ids) = out else {
            panic!("expected Rows");
        };
        let locals: Vec<u64> = ids.iter().map(|o| o.local_seq()).collect();
        assert_eq!(locals, vec![2, 3]);
    }

    #[test]
    fn end_to_end_unknown_tag_errors() {
        let f = Fixture::new(&["a"]);
        let err = f
            .engine()
            .execute("SELECT * FROM objects WHERE tag = 'nonexistent'")
            .unwrap_err();
        assert!(matches!(err, SqlError::UnknownTag(name) if name == "nonexistent"));
    }
}
