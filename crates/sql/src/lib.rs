//! `mimisbrunnr-sql` — SQL surface for Mímisbrunnr queries.
//!
//! Object queries are expressed as SQL `SELECT` statements; the planner
//! compiles them into [`mimisbrunnr_types::Query`] trees that
//! [`mimisbrunnr_query::QueryExecutor`] evaluates. This crate is *glue*:
//! parse → plan → execute → format. The actual bitmap-algebra work lives in
//! the query crate.
//!
//! ## Phase 5a surface
//!
//! ```sql
//! SELECT * FROM objects WHERE tag = 'electronic';
//! SELECT * FROM objects WHERE tag IN ('electronic', 'portable');
//! SELECT * FROM objects WHERE artist = 'Aphex Twin';
//! SELECT * FROM objects WHERE year >= 2024 AND year < 2026;
//! SELECT * FROM objects WHERE name LIKE 'cargo%';   -- prefix
//! SELECT * FROM objects WHERE name LIKE '%lock';    -- contains
//! SELECT * FROM objects
//!   WHERE tag = 'electronics' AND tag = 'portable' AND year = 2024
//!     AND NOT tag = 'discontinued';
//! SELECT * FROM objects WHERE isa('vehicle');
//! SELECT * FROM objects WHERE related('member_of', 900);
//! SELECT * FROM objects WHERE tag = 'song' LIMIT 100 OFFSET 50;
//! SELECT COUNT(*) FROM objects WHERE tag = 'electronics';
//! ```
//!
//! Out of scope (return [`SqlError::Unsupported`]):
//! - JOIN, subqueries, GROUP BY, ORDER BY, SELECT DISTINCT, HAVING.
//! - INSERT / UPDATE / DELETE.
//! - LIKE patterns with internal `%` or `_` (return [`SqlError::UnsupportedPattern`]).
//!
//! ## Public surface
//!
//! - [`SqlParser`] — parse → `sqlparser::ast::Statement`.
//! - [`SqlPlanner`] — `Statement` → [`PlannedQuery`].
//! - [`SqlEngine`] — driver tying parse + plan + execute + format.
//! - [`SqlOutput`] — `Rows(Vec<ObjectId>)` or `Count(u64)`.
//! - [`explain`] — render a textual plan for `mimir query --sql --explain`.

#![forbid(unsafe_code)]

mod engine;
mod error;
mod explain;
mod parser;
mod planner;

pub use engine::{SqlEngine, SqlOutput};
pub use error::SqlError;
pub use explain::explain;
pub use parser::SqlParser;
pub use planner::{Aggregate, PlannedQuery, Projection, SqlPlanner};
