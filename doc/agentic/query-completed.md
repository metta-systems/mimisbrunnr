# Query and SQL — completed

- Bitmap-algebra executor, query parser and faceted explorer — `crates/query`.
- Read-only SQL layer (parser, planner, executor) over the query executor — `crates/sql`.
- `mimir sql [<statement>]`: one-shot statement, or an interactive REPL (multi-line statements ending with `;`, `.help`, `.tables`, `.tags`, `.schema`, `.quit`) — `bin/mimir/src/commands.rs` `run_sql_repl`, test `sql_repl_runs_multiline_statements_and_survives_errors`.
