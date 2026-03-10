use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};

use mimisbrunnr::{
    engine::{DiskEngine, Engine},
    ontology::{OntologyModule, TagDefinition, TagSemantics, ValueType},
    types::{Assertion, ObjectId, Value},
    unix::{Importer, PathContextManager},
};

use std::collections::HashMap;

#[derive(Parser)]
#[command(name = "mimir", about = "Mímisbrunnr query engine — ask the well")]
struct Cli {
    /// Path to pool.toml config file.
    #[arg(long, global = true)]
    pool: Option<PathBuf>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Create a new object. Returns its ID.
    Create {
        /// Number of objects to create.
        #[arg(default_value = "1")]
        count: u32,
    },

    /// Add tags to an object.
    Tag {
        /// Object ID (local sequence number).
        #[arg(long)]
        object: u64,

        /// Tags to add.
        tags: Vec<String>,
    },

    /// Remove a tag from an object.
    Untag {
        /// Object ID.
        #[arg(long)]
        object: u64,

        /// Tag to remove.
        tag: String,
    },

    /// Set an attribute on an object.
    Set {
        /// Object ID.
        #[arg(long)]
        object: u64,

        /// Key=value pair.
        attr: String,
    },

    /// Show all assertions on an object.
    Info {
        /// Object ID.
        #[arg(long)]
        object: u64,
    },

    /// Execute a query.
    Query {
        /// Query string (e.g., "electronic AND year:2024").
        query: String,
    },

    /// Ontology management.
    Ontology {
        #[command(subcommand)]
        action: OntologyAction,
    },

    /// Path projection management.
    Project {
        #[command(subcommand)]
        action: ProjectAction,
    },

    /// Execute a SQL query, or start an interactive SQL REPL if no query given.
    Sql {
        /// SQL query to execute. Omit to enter REPL mode.
        query: Option<String>,
    },
}

#[derive(Subcommand)]
enum OntologyAction {
    /// List registered tags.
    List,

    /// Register a new tag.
    Register {
        /// Tag name.
        name: String,

        /// Semantics: label, attribute, grouping, ordered, hierarchical.
        #[arg(long, default_value = "label")]
        semantics: String,

        /// Value type for attributes: text, int, float, timestamp, blob.
        #[arg(long)]
        value_type: Option<String>,
    },

    /// Add an implication (from implies to).
    Imply {
        /// Source tag name.
        from: String,

        /// Target tag name.
        to: String,
    },

    /// Load an ontology module from a TOML file.
    Load {
        /// Path to the ontology TOML file.
        file: PathBuf,
    },
}

#[derive(Subcommand)]
enum ProjectAction {
    /// Import a directory tree.
    Import {
        /// Directory to import.
        path: PathBuf,

        /// Context name.
        #[arg(long)]
        context: String,
    },

    /// Show a project tree.
    Tree {
        /// Context name.
        context: String,
    },
}

fn main() {
    let cli = Cli::parse();

    match cli.pool {
        Some(pool_path) => run_with_pool(&pool_path, cli.command),
        None => {
            eprintln!("warning: no --pool specified, using ephemeral in-memory engine");
            let mut engine = Engine::new(0);
            let mut ctx_mgr = PathContextManager::new();
            let mut blobs = HashMap::new();
            dispatch(&mut engine, &mut ctx_mgr, &mut blobs, cli.command);
        }
    }
}

fn run_with_pool(pool_path: &Path, command: Commands) {
    let pool_toml = if pool_path.is_dir() {
        pool_path.join("pool.toml")
    } else {
        pool_path.to_path_buf()
    };

    let mut disk_engine = match DiskEngine::open(&pool_toml) {
        Ok(de) => de,
        Err(e) => {
            eprintln!(
                "error: failed to open pool from {}: {e}",
                pool_toml.display()
            );
            std::process::exit(1);
        }
    };

    // Take fields out temporarily to avoid double borrow
    let mut ctx_mgr = std::mem::take(&mut disk_engine.context_mgr);
    let mut blobs = std::mem::take(&mut disk_engine.blobs);
    dispatch(disk_engine.engine_mut(), &mut ctx_mgr, &mut blobs, command);
    disk_engine.context_mgr = ctx_mgr;
    disk_engine.blobs = blobs;

    if let Err(e) = disk_engine.flush() {
        eprintln!("error: failed to flush to disk: {e}");
        std::process::exit(1);
    }
}

fn dispatch(
    engine: &mut Engine,
    ctx_mgr: &mut PathContextManager,
    blobs: &mut HashMap<u64, Vec<u8>>,
    command: Commands,
) {
    match command {
        Commands::Create { count } => cmd_create(engine, count),
        Commands::Tag { object, tags } => cmd_tag(engine, object, &tags),
        Commands::Untag { object, tag } => cmd_untag(engine, object, &tag),
        Commands::Set { object, attr } => cmd_set(engine, object, &attr),
        Commands::Info { object } => cmd_info(engine, object),
        Commands::Query { query } => cmd_query(engine, &query),
        Commands::Ontology { action } => cmd_ontology(engine, action),
        Commands::Project { action } => cmd_project(engine, ctx_mgr, blobs, action),
        Commands::Sql { query } => cmd_sql(engine, query.as_deref()),
    }
}

fn cmd_create(engine: &mut Engine, count: u32) {
    for _ in 0..count {
        match engine.create_object(now_ms()) {
            Ok(oid) => println!("{oid}"),
            Err(e) => {
                eprintln!("error: {e}");
                return;
            }
        }
    }
}

fn cmd_tag(engine: &mut Engine, object: u64, tags: &[String]) {
    let oid = ObjectId::new(engine.node_id(), object);

    for tag_name in tags {
        match engine.dag.lookup(tag_name) {
            Some(tag_id) => match engine.add_tag(oid, tag_id, now_ms()) {
                Ok(materialized) => {
                    println!("tagged {oid} with {tag_name}");
                    for m in &materialized {
                        if let Some(def) = engine.dag.get(*m) {
                            println!("  (materialized: {})", def.name);
                        }
                    }
                }
                Err(e) => eprintln!("error: {e}"),
            },
            None => eprintln!("error: unknown tag '{tag_name}'"),
        }
    }
}

fn cmd_untag(engine: &mut Engine, object: u64, tag_name: &str) {
    let oid = ObjectId::new(engine.node_id(), object);
    match engine.dag.lookup(tag_name) {
        Some(tag_id) => match engine.remove_tag(oid, tag_id, now_ms()) {
            Ok(_) => println!("untagged {oid} from {tag_name}"),
            Err(e) => eprintln!("error: {e}"),
        },
        None => eprintln!("error: unknown tag '{tag_name}'"),
    }
}

fn cmd_set(engine: &mut Engine, object: u64, attr: &str) {
    let oid = ObjectId::new(engine.node_id(), object);
    let Some((key, val)) = attr.split_once('=') else {
        eprintln!("error: attribute must be in key=value format");
        return;
    };

    let tag_id = match engine.dag.lookup(key) {
        Some(id) => id,
        None => {
            eprintln!("error: unknown attribute key '{key}'");
            return;
        }
    };

    // Try to parse as int, then float, then text
    let value = if let Ok(n) = val.parse::<i64>() {
        Value::Int(n)
    } else if let Ok(f) = val.parse::<f64>() {
        Value::Float(f)
    } else {
        Value::Text(val.to_string())
    };

    match engine.set_attr(oid, tag_id, value, now_ms()) {
        Ok(()) => println!("set {key}={val} on {oid}"),
        Err(e) => eprintln!("error: {e}"),
    }
}

fn cmd_info(engine: &Engine, object: u64) {
    let oid = ObjectId::new(engine.node_id(), object);
    match engine.get_object(oid) {
        Ok(rec) => {
            println!("Object {oid}");
            println!("  state:    {:?}", rec.state);
            println!("  blob:     {} bytes", rec.blob_length);
            println!("  tags:     {}", rec.tag_count);
            println!("  attrs:    {}", rec.attr_count);

            if let Ok(assertions) = engine.assertions(oid) {
                println!("  assertions:");
                for entry in assertions {
                    let origin = if entry.origin == mimisbrunnr::types::TagOrigin::Direct {
                        "direct"
                    } else {
                        "materialized"
                    };
                    match &entry.assertion {
                        Assertion::Tag(id) => {
                            let name = engine.dag.get(*id).map(|d| d.name.as_str()).unwrap_or("?");
                            println!("    tag:{name} ({origin})");
                        }
                        Assertion::Attr { key, value } => {
                            let name = engine.dag.get(*key).map(|d| d.name.as_str()).unwrap_or("?");
                            println!("    {name}={value} ({origin})");
                        }
                        Assertion::Relation { predicate, target } => {
                            let name = engine
                                .dag
                                .get(*predicate)
                                .map(|d| d.name.as_str())
                                .unwrap_or("?");
                            println!("    {name}→{target} ({origin})");
                        }
                    }
                }
            }
        }
        Err(e) => eprintln!("error: {e}"),
    }
}

fn cmd_query(engine: &Engine, query_str: &str) {
    match engine.query_str(query_str) {
        Ok(results) => {
            println!("{} result(s):", results.len());
            for oid in &results {
                println!("  {oid}");
            }
        }
        Err(e) => eprintln!("error: {e}"),
    }
}

fn cmd_ontology(engine: &mut Engine, action: OntologyAction) {
    match action {
        OntologyAction::List => {
            let tags = engine.dag.all_tags();
            if tags.is_empty() {
                println!("No tags registered.");
            } else {
                println!("{} tag(s):", tags.len());
                for id in tags {
                    if let Some(def) = engine.dag.get(id) {
                        let implies = engine.dag.direct_implies(id);
                        let implies_str = if implies.is_empty() {
                            String::new()
                        } else {
                            let names: Vec<_> = implies
                                .iter()
                                .filter_map(|i| engine.dag.get(*i).map(|d| d.name.as_str()))
                                .collect();
                            format!(" → {}", names.join(", "))
                        };
                        println!("  {} ({:?}){implies_str}", def.name, def.semantics);
                    }
                }
            }
        }
        OntologyAction::Register {
            name,
            semantics,
            value_type,
        } => {
            let sem = match semantics.as_str() {
                "label" => TagSemantics::Label,
                "attribute" | "attr" => {
                    let vt = match value_type.as_deref().unwrap_or("text") {
                        "text" => ValueType::Text,
                        "int" => ValueType::Int,
                        "float" => ValueType::Float,
                        "timestamp" => ValueType::Timestamp,
                        "blob" => ValueType::Blob,
                        other => {
                            eprintln!("error: unknown value type '{other}'");
                            return;
                        }
                    };
                    TagSemantics::Attribute { value_type: vt }
                }
                "grouping" => TagSemantics::Grouping,
                "ordered" => TagSemantics::OrderedCollection {
                    element_constraint: None,
                },
                "hierarchical" => TagSemantics::Hierarchical,
                other => {
                    eprintln!("error: unknown semantics '{other}'");
                    return;
                }
            };

            let id = engine.dag.alloc_tag_id();
            let def = TagDefinition::new(id, &name, sem);
            match engine.register_tag(def) {
                Ok(_) => println!("registered tag '{name}' ({id})"),
                Err(e) => eprintln!("error: {e}"),
            }
        }
        OntologyAction::Imply { from, to } => {
            let from_id = match engine.dag.lookup(&from) {
                Some(id) => id,
                None => {
                    eprintln!("error: unknown tag '{from}'");
                    return;
                }
            };
            let to_id = match engine.dag.lookup(&to) {
                Some(id) => id,
                None => {
                    eprintln!("error: unknown tag '{to}'");
                    return;
                }
            };
            match engine.add_implication(from_id, to_id) {
                Ok(()) => println!("added implication: {from} → {to}"),
                Err(e) => eprintln!("error: {e}"),
            }
        }
        OntologyAction::Load { file } => {
            let module = match OntologyModule::from_file(&file) {
                Ok(m) => m,
                Err(e) => {
                    eprintln!("error: failed to load {}: {e}", file.display());
                    return;
                }
            };

            let label = module
                .name
                .clone()
                .or_else(|| module.id.clone())
                .unwrap_or_else(|| file.display().to_string());

            match module.install(&mut engine.dag) {
                Ok(result) => {
                    println!("Loaded ontology module '{label}'");
                    println!(
                        "  {} tag(s) registered, {} skipped, {} implication(s) added",
                        result.tags_registered, result.tags_skipped, result.implications_added
                    );
                }
                Err(e) => eprintln!("error: {e}"),
            }
        }
    }
}

fn cmd_project(
    engine: &mut Engine,
    ctx_mgr: &mut PathContextManager,
    blobs: &mut HashMap<u64, Vec<u8>>,
    action: ProjectAction,
) {
    match action {
        ProjectAction::Import { path, context } => {
            let ext_tags = HashMap::new();
            match Importer::import_directory(engine, ctx_mgr, &path, &context, &ext_tags, now_ms())
            {
                Ok(result) => {
                    println!(
                        "Imported {} file(s) into context '{}'",
                        result.objects_created, context
                    );
                    if result.objects_deduped > 0 {
                        println!(
                            "  ({} deduplicated by content hash)",
                            result.objects_deduped
                        );
                    }
                    println!("  Total bytes: {}", result.total_bytes);

                    // Store blob data for imported files so FUSE can serve them
                    if let Ok(proj) = ctx_mgr.get_context(&context) {
                        for entry in &proj.entries {
                            if let Some(oid) = entry.object
                                && let Ok(content) = std::fs::read(path.join(&entry.path))
                            {
                                blobs.insert(oid.raw(), content);
                            }
                        }
                    }
                }
                Err(e) => eprintln!("error: {e}"),
            }
        }
        ProjectAction::Tree { context } => match ctx_mgr.get_context(&context) {
            Ok(proj) => {
                let with_dirs = proj.with_synthesized_dirs();
                let mut paths: Vec<_> = with_dirs.entries.iter().map(|e| &e.path).collect();
                paths.sort();
                println!("Context '{context}' ({} entries):", proj.len());
                for p in paths {
                    println!("  {p}");
                }
            }
            Err(e) => eprintln!("error: {e}"),
        },
    }
}

fn cmd_sql(engine: &Engine, query: Option<&str>) {
    match query {
        Some(sql) => execute_sql_query(engine, sql),
        None => sql_repl(engine),
    }
}

fn sql_repl(engine: &Engine) {
    println!("mimir sql — interactive SQL REPL");
    println!("Type SQL queries, or .help for commands. End queries with ;");
    println!();

    let stdin = std::io::stdin();
    let mut buf = String::new();

    loop {
        let prompt = if buf.is_empty() { "sql> " } else { "  -> " };
        eprint!("{prompt}");

        let mut line = String::new();
        match stdin.read_line(&mut line) {
            Ok(0) => break, // EOF
            Ok(_) => {}
            Err(e) => {
                eprintln!("error reading input: {e}");
                break;
            }
        }

        let trimmed = line.trim();

        // Dot-commands (only at the start of input, not mid-statement)
        if buf.is_empty() {
            match trimmed {
                ".quit" | ".exit" | ".q" => break,
                ".help" | ".h" => {
                    print_sql_help();
                    continue;
                }
                ".tables" => {
                    println!("objects  (the only table — all objects in the store)");
                    continue;
                }
                ".tags" => {
                    let tags = engine.dag.all_tags();
                    if tags.is_empty() {
                        println!("No tags registered.");
                    } else {
                        for id in tags {
                            if let Some(def) = engine.dag.get(id) {
                                println!("  {} ({:?})", def.name, def.semantics);
                            }
                        }
                    }
                    continue;
                }
                ".schema" => {
                    println!("-- Mímisbrunnr is schemaless. Attributes are dynamic.");
                    println!("-- Registered attributes:");
                    for id in engine.dag.all_tags() {
                        if let Some(def) = engine.dag.get(id) {
                            if let mimisbrunnr::ontology::TagSemantics::Attribute { value_type } =
                                &def.semantics
                            {
                                println!("  {} {:?}", def.name, value_type);
                            }
                        }
                    }
                    continue;
                }
                "" => continue,
                _ => {}
            }
        }

        buf.push_str(&line);

        // Check if the statement is complete (ends with ;)
        let trimmed_buf = buf.trim();
        if trimmed_buf.ends_with(';') {
            let sql = trimmed_buf.trim_end_matches(';').trim();
            if !sql.is_empty() {
                execute_sql_query(engine, sql);
            }
            buf.clear();
        }
    }
}

fn execute_sql_query(engine: &Engine, sql: &str) {
    use mimisbrunnr::sql::{self, QueryResult};

    let result = sql::execute(
        sql,
        &engine.tag_index,
        &engine.kv_index,
        &engine.forward_index,
        &engine.dag,
    );

    match result {
        Ok(QueryResult::Select { columns, rows }) => {
            if rows.is_empty() {
                println!("(0 rows)");
                return;
            }

            // Determine column widths
            let mut widths: Vec<usize> = columns.iter().map(|c| c.len()).collect();
            for row in &rows {
                for (i, col_name) in columns.iter().enumerate() {
                    let val = row
                        .columns
                        .iter()
                        .find(|(k, _)| k == col_name)
                        .map(|(_, v)| format_value(v))
                        .unwrap_or_default();
                    if i < widths.len() {
                        widths[i] = widths[i].max(val.len());
                    }
                }
            }

            // Print header
            let header: Vec<String> = columns
                .iter()
                .enumerate()
                .map(|(i, c)| format!("{:<width$}", c, width = widths[i]))
                .collect();
            println!("{}", header.join(" | "));
            let separator: Vec<String> = widths.iter().map(|w| "-".repeat(*w)).collect();
            println!("{}", separator.join("-+-"));

            // Print rows
            for row in &rows {
                let vals: Vec<String> = columns
                    .iter()
                    .enumerate()
                    .map(|(i, col_name)| {
                        let val = row
                            .columns
                            .iter()
                            .find(|(k, _)| k == col_name)
                            .map(|(_, v)| format_value(v))
                            .unwrap_or_default();
                        format!("{:<width$}", val, width = widths[i])
                    })
                    .collect();
                println!("{}", vals.join(" | "));
            }
            println!("({} row{})", rows.len(), if rows.len() == 1 { "" } else { "s" });
        }

        Ok(QueryResult::Aggregate { columns, rows }) => {
            if rows.is_empty() {
                println!("(0 rows)");
                return;
            }

            let mut widths: Vec<usize> = columns.iter().map(|c| c.len()).collect();
            for row in &rows {
                for (i, col_name) in columns.iter().enumerate() {
                    let val = row
                        .columns
                        .iter()
                        .find(|(k, _)| k == col_name)
                        .map(|(_, v)| format_value(v))
                        .unwrap_or_default();
                    if i < widths.len() {
                        widths[i] = widths[i].max(val.len());
                    }
                }
            }

            let header: Vec<String> = columns
                .iter()
                .enumerate()
                .map(|(i, c)| format!("{:<width$}", c, width = widths[i]))
                .collect();
            println!("{}", header.join(" | "));
            let separator: Vec<String> = widths.iter().map(|w| "-".repeat(*w)).collect();
            println!("{}", separator.join("-+-"));

            for row in &rows {
                let vals: Vec<String> = columns
                    .iter()
                    .enumerate()
                    .map(|(i, col_name)| {
                        let val = row
                            .columns
                            .iter()
                            .find(|(k, _)| k == col_name)
                            .map(|(_, v)| format_value(v))
                            .unwrap_or_default();
                        format!("{:<width$}", val, width = widths[i])
                    })
                    .collect();
                println!("{}", vals.join(" | "));
            }
            println!("({} row{})", rows.len(), if rows.len() == 1 { "" } else { "s" });
        }

        Ok(QueryResult::Scalar(value)) => {
            println!("{}", format_value(&value));
        }

        Err(e) => {
            eprintln!("error: {e}");
        }
    }
}

fn format_value(v: &Value) -> String {
    match v {
        Value::Text(s) => s.clone(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => format!("{:.2}", f),
        Value::Timestamp(t) => t.to_string(),
        Value::Blob(b) => format!("<blob:{} bytes>", b.len()),
    }
}

fn print_sql_help() {
    println!("Mímisbrunnr SQL REPL commands:");
    println!();
    println!("  .help     Show this help");
    println!("  .quit     Exit the REPL");
    println!("  .tables   List available tables");
    println!("  .tags     List registered tags");
    println!("  .schema   Show registered attributes");
    println!();
    println!("SQL syntax:");
    println!("  SELECT id, name FROM objects WHERE HAS TAG 'electronic';");
    println!("  SELECT * FROM objects WHERE IS A 'audio' AND year > 2000;");
    println!("  SELECT artist, COUNT(*) FROM objects GROUP BY artist;");
    println!("  SELECT * FROM objects WHERE HAS TAG 'source' LIMIT 10;");
    println!();
    println!("Extensions:");
    println!("  HAS TAG 'x'              Tag membership");
    println!("  HAS ALL TAGS ('x', 'y')  AND of tags");
    println!("  HAS ANY TAG ('x', 'y')   OR of tags");
    println!("  IS A 'x'                 Ontology-aware (follows implications)");
    println!("  NOT HAS TAG 'x'          Exclusion");
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
