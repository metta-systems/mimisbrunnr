//! SQL query layer for Mímisbrunnr.
//!
//! Compiles a SQL dialect (with extensions for tags, ontology, and relations)
//! into the existing bitmap query engine.
//!
//! # Supported syntax
//!
//! ```sql
//! -- Tag queries
//! SELECT id, name FROM objects WHERE HAS TAG 'electronic' AND year = 2024;
//!
//! -- Ontology-aware queries
//! SELECT id FROM objects WHERE IS A 'audio';
//!
//! -- Aggregation
//! SELECT artist, COUNT(*) FROM objects GROUP BY artist ORDER BY COUNT(*) DESC;
//!
//! -- Pagination
//! SELECT id FROM objects WHERE HAS TAG 'source' LIMIT 50 OFFSET 100;
//! ```

mod ast;
mod error;
mod executor;
mod parser;
mod planner;
mod types;

pub use ast::{
    AggregateFunc, CompareOp, OrderBy, Predicate, Projection, SelectQuery, SortDir, Statement,
};
pub use error::SqlError;
pub use executor::SqlExecutor;
pub use parser::parse_sql;
pub use planner::{plan, FilterPredicate, PhysicalOp};
pub use types::{GroupRow, QueryResult, Row};

use mimisbrunnr_index::{ForwardIndex, KvIndex, TagIndex};
use mimisbrunnr_ontology::ImplicationDag;

/// Parse and execute a SQL query against Mímisbrunnr's index layer.
///
/// This is the main entry point for the SQL engine.
pub fn execute(
    sql: &str,
    tag_index: &TagIndex,
    kv_index: &KvIndex,
    forward_index: &ForwardIndex,
    dag: &ImplicationDag,
) -> Result<QueryResult, SqlError> {
    let statement = parse_sql(sql)?;
    let Statement::Select(query) = &statement;
    let physical_plan = plan(query)?;
    let executor = SqlExecutor::new(tag_index, kv_index, forward_index, dag);
    executor.execute(&physical_plan)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mimisbrunnr_index::TagIndex;
    use mimisbrunnr_ontology::{TagDefinition, TagSemantics, ValueType};
    use mimisbrunnr_types::{Assertion, ObjectId, TagId, TagOrigin, Value};

    fn tag(id: u32) -> TagId {
        TagId::new(id)
    }

    fn label(id: u32, name: &str) -> TagDefinition {
        TagDefinition::new(tag(id), name, TagSemantics::Label)
    }

    fn attr_def(id: u32, name: &str, vt: ValueType) -> TagDefinition {
        TagDefinition::new(tag(id), name, TagSemantics::Attribute { value_type: vt })
    }

    struct TestDb {
        tag_index: TagIndex,
        kv_index: KvIndex,
        forward_index: ForwardIndex,
        dag: ImplicationDag,
    }

    impl TestDb {
        fn new() -> Self {
            Self {
                tag_index: TagIndex::new(),
                kv_index: KvIndex::new(),
                forward_index: ForwardIndex::new(),
                dag: ImplicationDag::new(),
            }
        }

        fn register_tag(&mut self, id: u32, name: &str) -> TagId {
            self.dag.register_tag(label(id, name)).unwrap()
        }

        fn register_attr(&mut self, id: u32, name: &str, vt: ValueType) -> TagId {
            self.dag.register_tag(attr_def(id, name, vt)).unwrap()
        }

        fn add_tag(&mut self, obj_local: u32, tag_id: TagId) {
            self.tag_index.tag_object(tag_id, obj_local);
            let oid = ObjectId::new(0, obj_local as u64);
            self.forward_index
                .add(oid, Assertion::Tag(tag_id), TagOrigin::Direct);
        }

        fn set_attr(&mut self, obj_local: u32, key: TagId, value: Value) {
            let oid = ObjectId::new(0, obj_local as u64);
            self.kv_index.insert(key, &value, obj_local);
            self.forward_index
                .add(oid, Assertion::Attr { key, value }, TagOrigin::Direct);
        }

        fn query(&self, sql: &str) -> Result<QueryResult, SqlError> {
            execute(
                sql,
                &self.tag_index,
                &self.kv_index,
                &self.forward_index,
                &self.dag,
            )
        }
    }

    fn music_db() -> TestDb {
        let mut db = TestDb::new();

        let electronic = db.register_tag(1, "electronic");
        let ambient = db.register_tag(2, "ambient");
        let name = db.register_attr(10, "name", ValueType::Text);
        let artist = db.register_attr(11, "artist", ValueType::Text);
        let year = db.register_attr(12, "year", ValueType::Int);
        let bpm = db.register_attr(13, "bpm", ValueType::Int);

        // Object 1: electronic, ambient
        db.add_tag(1, electronic);
        db.add_tag(1, ambient);
        db.set_attr(1, name, Value::Text("Selected Ambient Works".into()));
        db.set_attr(1, artist, Value::Text("Aphex Twin".into()));
        db.set_attr(1, year, Value::Int(1992));
        db.set_attr(1, bpm, Value::Int(120));

        // Object 2: electronic
        db.add_tag(2, electronic);
        db.set_attr(2, name, Value::Text("Syro".into()));
        db.set_attr(2, artist, Value::Text("Aphex Twin".into()));
        db.set_attr(2, year, Value::Int(2014));
        db.set_attr(2, bpm, Value::Int(140));

        // Object 3: electronic
        db.add_tag(3, electronic);
        db.set_attr(3, name, Value::Text("Music Has the Right to Children".into()));
        db.set_attr(3, artist, Value::Text("Boards of Canada".into()));
        db.set_attr(3, year, Value::Int(1998));
        db.set_attr(3, bpm, Value::Int(100));

        // Object 4: ambient
        db.add_tag(4, ambient);
        db.set_attr(4, name, Value::Text("Ambient 1: Music for Airports".into()));
        db.set_attr(4, artist, Value::Text("Brian Eno".into()));
        db.set_attr(4, year, Value::Int(1978));
        db.set_attr(4, bpm, Value::Int(80));

        db
    }

    #[test]
    fn end_to_end_select_star() {
        let db = music_db();
        let result = db.query("SELECT * FROM objects WHERE HAS TAG 'electronic'").unwrap();
        assert_eq!(result.row_count(), 3);
    }

    #[test]
    fn end_to_end_select_columns() {
        let db = music_db();
        let result = db
            .query("SELECT id, name FROM objects WHERE HAS TAG 'electronic'")
            .unwrap();
        match result {
            QueryResult::Select { columns, rows } => {
                assert_eq!(columns, vec!["id", "name"]);
                assert_eq!(rows.len(), 3);
            }
            _ => panic!("expected Select"),
        }
    }

    #[test]
    fn end_to_end_and_query() {
        let db = music_db();
        let result = db
            .query("SELECT name FROM objects WHERE HAS TAG 'electronic' AND HAS TAG 'ambient'")
            .unwrap();
        assert_eq!(result.row_count(), 1); // Only "Selected Ambient Works"
    }

    #[test]
    fn end_to_end_or_query() {
        let db = music_db();
        let result = db
            .query("SELECT name FROM objects WHERE HAS TAG 'electronic' OR HAS TAG 'ambient'")
            .unwrap();
        assert_eq!(result.row_count(), 4); // All 4 objects
    }

    #[test]
    fn end_to_end_attr_eq() {
        let db = music_db();
        let result = db
            .query("SELECT name FROM objects WHERE artist = 'Aphex Twin'")
            .unwrap();
        assert_eq!(result.row_count(), 2);
    }

    #[test]
    fn end_to_end_count() {
        let db = music_db();
        let result = db
            .query("SELECT COUNT(*) FROM objects WHERE HAS TAG 'electronic'")
            .unwrap();
        assert_eq!(result, QueryResult::Scalar(Value::Int(3)));
    }

    #[test]
    fn end_to_end_limit() {
        let db = music_db();
        let result = db
            .query("SELECT * FROM objects WHERE HAS TAG 'electronic' LIMIT 2")
            .unwrap();
        assert_eq!(result.row_count(), 2);
    }

    #[test]
    fn end_to_end_group_by() {
        let db = music_db();
        let result = db
            .query(
                "SELECT artist, COUNT(*) as tracks FROM objects WHERE HAS TAG 'electronic' GROUP BY artist",
            )
            .unwrap();
        match result {
            QueryResult::Aggregate { rows, .. } => {
                // Aphex Twin: 2 tracks, Boards of Canada: 1 track
                assert_eq!(rows.len(), 2);
                let aphex = rows
                    .iter()
                    .find(|r| r.key == Value::Text("Aphex Twin".into()))
                    .unwrap();
                let count = aphex
                    .columns
                    .iter()
                    .find(|(k, _)| k == "tracks")
                    .map(|(_, v)| v)
                    .unwrap();
                assert_eq!(count, &Value::Int(2));
            }
            _ => panic!("expected Aggregate"),
        }
    }

    #[test]
    fn end_to_end_not_has_tag() {
        let db = music_db();
        let result = db
            .query("SELECT name FROM objects WHERE HAS TAG 'ambient' AND NOT HAS TAG 'electronic'")
            .unwrap();
        // Only "Ambient 1: Music for Airports" (object 4: ambient but not electronic)
        assert_eq!(result.row_count(), 1);
    }

    #[test]
    fn end_to_end_select_no_where() {
        let db = music_db();
        let result = db.query("SELECT * FROM objects").unwrap();
        assert_eq!(result.row_count(), 4);
    }

    #[test]
    fn parse_error() {
        let db = music_db();
        let result = db.query("INVALID SQL");
        assert!(result.is_err());
    }

    #[test]
    fn end_to_end_avg() {
        let db = music_db();
        let result = db
            .query("SELECT AVG(bpm) FROM objects WHERE HAS TAG 'electronic'")
            .unwrap();
        match result {
            QueryResult::Scalar(Value::Float(avg)) => {
                // (120 + 140 + 100) / 3 = 120.0
                assert!((avg - 120.0).abs() < 0.001);
            }
            _ => panic!("expected Scalar Float, got {:?}", result),
        }
    }
}
