//! `explain(sql, ontology)` — textual plan for `mimir query --sql --explain`.

use mimisbrunnr_ontology::OntologyState;

use crate::{
    error::SqlError,
    parser::SqlParser,
    planner::{Aggregate, PlannedQuery, Projection, SqlPlanner},
};

/// Render a textual plan: the original SQL, the [`PlannedQuery`] metadata, and
/// the bitmap-algebra tree below it.
pub fn explain(sql: &str, ontology: &OntologyState) -> Result<String, SqlError> {
    let stmt = SqlParser::parse(sql)?;
    let planned = SqlPlanner::plan(&stmt, ontology)?;
    Ok(format_plan(sql, &planned))
}

fn format_plan(sql: &str, planned: &PlannedQuery) -> String {
    let mut out = String::new();
    out.push_str("SQL:\n  ");
    out.push_str(sql.trim());
    out.push('\n');
    out.push_str(&format!(
        "Projection: {}\nAggregate: {}\n",
        projection_str(planned.projection),
        aggregate_str(planned.aggregate)
    ));
    if let Some(l) = planned.limit {
        out.push_str(&format!("Limit: {l}\n"));
    }
    if let Some(o) = planned.offset {
        out.push_str(&format!("Offset: {o}\n"));
    }
    out.push_str("Query:\n");
    let inner = mimisbrunnr_query::explain(&planned.query);
    for line in inner.lines() {
        out.push_str("  ");
        out.push_str(line);
        out.push('\n');
    }
    out
}

fn projection_str(p: Projection) -> &'static str {
    match p {
        Projection::Star => "*",
        Projection::Count => "COUNT(*)",
    }
}

fn aggregate_str(a: Aggregate) -> &'static str {
    match a {
        Aggregate::None => "none",
        Aggregate::Count => "count",
    }
}

#[cfg(test)]
mod tests {
    use mimisbrunnr_ontology::{IdAllocator, OntologyModule, OntologyState};
    use mimisbrunnr_types::{TagDefinition, TagId, TagSemantics};

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

    #[test]
    fn explain_renders_all_pieces() {
        let ont = ontology_with(&["song"]);
        let plan = explain(
            "SELECT * FROM objects WHERE tag = 'song' LIMIT 5 OFFSET 10",
            &ont,
        )
        .unwrap();
        assert!(plan.contains("SQL:"));
        assert!(plan.contains("Projection: *"));
        assert!(plan.contains("Aggregate: none"));
        assert!(plan.contains("Limit: 5"));
        assert!(plan.contains("Offset: 10"));
        assert!(plan.contains("Query:"));
        assert!(plan.contains("HasTag"));
    }

    #[test]
    fn explain_count_star() {
        let ont = ontology_with(&["song"]);
        let plan = explain("SELECT COUNT(*) FROM objects WHERE tag = 'song'", &ont).unwrap();
        assert!(plan.contains("Projection: COUNT(*)"));
        assert!(plan.contains("Aggregate: count"));
    }
}
