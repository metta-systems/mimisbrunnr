//! `mimir` — query / mutation CLI for a Mímisbrunnr pool.
//!
//! Phase 7a entry point: parse args with `clap`, open the [`DiskEngine`],
//! dispatch to a command handler in `mimir::commands`, and `commit()` on the
//! way out so the WAL is durable.

#![forbid(unsafe_code)]

use std::{
    io::{self, Write},
    path::PathBuf,
    process::ExitCode,
};

use clap::{Args, Parser, Subcommand};
use log::LevelFilter;

use mimir::{
    commands::{self, CommandError},
    value_parse::ValueKind,
};
use mimisbrunnr::engine::DiskEngine;

#[derive(Parser, Debug)]
#[command(
    name = "mimir",
    version,
    about = "Mímisbrunnr query engine — ask the well",
    long_about = "\
mimir drives a Mímisbrunnr pool's DiskEngine.

Tag-name resolution: subcommands that take tag names auto-register any \
tag that the ontology doesn't yet know as a Label-semantics tag. To get \
richer semantics (Attribute, Grouping, …), install an ontology module \
via `mimir ontology install <module.toml>` first."
)]
struct Cli {
    /// Path to `pool.toml` (or a directory containing one).
    #[arg(long, default_value = "pool.toml", global = true)]
    pool: PathBuf,

    /// Bump log level. Repeat for trace.
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    verbose: u8,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Allocate a fresh object id and print it. (Helper — not in DESIGN §A;
    /// exists so callers have a way to mint ids they can `tag` / `set` /
    /// `info` against.)
    Create,

    /// Add tags to an object.
    Tag {
        /// `obj:<n>` reference.
        oid: String,
        /// One or more tag names.
        #[arg(required = true)]
        tags: Vec<String>,
    },
    /// Remove a single tag from an object.
    Untag { oid: String, tag: String },
    /// Set an attribute on an object.
    Set {
        oid: String,
        key: String,
        value: String,
        /// Force a specific value type (int|float|text|timestamp|blob).
        #[arg(long, value_name = "KIND")]
        r#type: Option<String>,
    },
    /// Show every assertion attached to an object.
    Info { oid: String },

    /// Run a query against the pool.
    Query(QueryArgs),

    /// Faceted exploration around `<tag>`.
    Explore {
        tag: String,
        #[arg(long, default_value = "16")]
        max_facets: usize,
    },

    /// Ontology management.
    Ontology {
        #[command(subcommand)]
        action: OntologyAction,
    },

    /// Subscription management.
    Watch {
        #[command(subcommand)]
        action: WatchAction,
    },

    /// Path-projection management.
    Project {
        #[command(subcommand)]
        action: ProjectAction,
    },
}

#[derive(Args, Debug)]
struct QueryArgs {
    /// Query body. Either an S-expression DSL string or, with `--sql`, a SQL
    /// SELECT statement.
    query: String,
    /// Treat `query` as SQL.
    #[arg(long)]
    sql: bool,
    /// Print the plan instead of executing.
    #[arg(long)]
    explain: bool,
}

#[derive(Subcommand, Debug)]
enum OntologyAction {
    /// List installed modules.
    List,
    /// Install a module from `<file.toml>`.
    Install { file: PathBuf },
    /// Remove an installed module by id.
    Remove { module_id: String },
    /// Show tags not registered by any installed module.
    Orphans,
    /// Print a tag definition + its implication closure.
    Show { tag: String },
    /// Adopt an orphan tag into a module. (Phase 7a: TODO)
    Adopt { orphan: String, into: String },
}

#[derive(Subcommand, Debug)]
enum WatchAction {
    /// Register a subscription. Query is an S-expression DSL string.
    Register {
        #[arg(long)]
        name: String,
        sexpr: String,
    },
    /// List all registered subscriptions.
    List,
    /// Drain pending events from a subscription as JSON-ish lines.
    Drain { name: String },
    /// Tear down a subscription.
    Unsubscribe { name: String },
    /// Continuously stream events. (Phase 7a: TODO)
    Stream { name: String },
}

#[derive(Subcommand, Debug)]
enum ProjectAction {
    /// Create a path-context grouping tag and an empty projection.
    CreateContext { name: String },
    /// List registered path contexts.
    ListContexts,
    /// Record `path` for `oid` under `ctx`.
    SetPath {
        oid: String,
        ctx: String,
        path: String,
    },
    /// Print the projected tree for `ctx`.
    Tree { ctx: String },
    /// Walk a host directory and create one object per file. Phase 7a does
    /// not read file contents into the blob store.
    Import {
        dir: PathBuf,
        #[arg(long)]
        context: String,
    },
    /// Export blob bytes from a context. With `--oid`, writes a single
    /// object's blob to `--output`; without, the full directory-tree export
    /// is still deferred.
    Export {
        ctx: String,
        #[arg(short, long)]
        output: PathBuf,
        #[arg(long)]
        oid: Option<String>,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    init_logging(cli.verbose);

    match dispatch(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            let _ = writeln!(io::stderr(), "error: {e}");
            ExitCode::from(1)
        }
    }
}

fn init_logging(verbose: u8) {
    let level = match verbose {
        0 => LevelFilter::Warn,
        1 => LevelFilter::Info,
        2 => LevelFilter::Debug,
        _ => LevelFilter::Trace,
    };
    let _ = env_logger::Builder::from_default_env()
        .filter_level(level)
        .try_init();
}

fn resolve_pool_toml(p: &std::path::Path) -> PathBuf {
    if p.is_dir() {
        p.join("pool.toml")
    } else {
        p.to_path_buf()
    }
}

fn dispatch(cli: Cli) -> Result<(), CommandError> {
    let pool_toml = resolve_pool_toml(&cli.pool);
    let mut engine = DiskEngine::open(&pool_toml).map_err(CommandError::Engine)?;
    let mut stdout = io::stdout().lock();

    let mutated = run(&mut engine, &mut stdout, cli.command)?;
    drop(stdout);

    if mutated {
        engine.commit().map_err(CommandError::Engine)?;
    }
    Ok(())
}

/// Run a single command. Returns whether the command performed a durable
/// mutation that should trigger a `commit()` before exit.
fn run<W: Write>(
    engine: &mut DiskEngine,
    out: &mut W,
    cmd: Commands,
) -> Result<bool, CommandError> {
    match cmd {
        Commands::Create => {
            commands::run_create(engine, out)?;
            Ok(true)
        }
        Commands::Tag { oid, tags } => {
            commands::run_tag(engine, out, &oid, &tags)?;
            Ok(true)
        }
        Commands::Untag { oid, tag } => {
            commands::run_untag(engine, out, &oid, &tag)?;
            Ok(true)
        }
        Commands::Set {
            oid,
            key,
            value,
            r#type,
        } => {
            let kind = match r#type.as_deref() {
                None => ValueKind::Auto,
                Some(s) => ValueKind::parse_kind(s).map_err(CommandError::BadArg)?,
            };
            commands::run_set(engine, out, &oid, &key, &value, kind)?;
            Ok(true)
        }
        Commands::Info { oid } => {
            commands::run_info(engine, out, &oid)?;
            Ok(false)
        }

        Commands::Query(QueryArgs {
            query,
            sql,
            explain,
        }) => {
            if sql {
                commands::run_query_sql(engine, out, &query, explain)?;
            } else {
                commands::run_query_sexpr(engine, out, &query, explain)?;
            }
            Ok(false)
        }

        Commands::Explore { tag, max_facets } => {
            commands::run_explore(engine, out, &tag, max_facets)?;
            Ok(false)
        }

        Commands::Ontology { action } => match action {
            OntologyAction::List => {
                commands::run_ontology_list(engine, out)?;
                Ok(false)
            }
            OntologyAction::Install { file } => {
                commands::run_ontology_install(engine, out, &file)?;
                Ok(true)
            }
            OntologyAction::Remove { module_id } => {
                commands::run_ontology_remove(engine, out, &module_id)?;
                Ok(true)
            }
            OntologyAction::Orphans => {
                commands::run_ontology_orphans(engine, out)?;
                Ok(false)
            }
            OntologyAction::Show { tag } => {
                commands::run_ontology_show(engine, out, &tag)?;
                Ok(false)
            }
            OntologyAction::Adopt { orphan, into } => {
                commands::run_ontology_adopt(engine, out, &orphan, &into)?;
                Ok(false)
            }
        },

        Commands::Watch { action } => match action {
            WatchAction::Register { name, sexpr } => {
                commands::run_watch_register(engine, out, &name, &sexpr)?;
                Ok(true)
            }
            WatchAction::List => {
                commands::run_watch_list(engine, out)?;
                Ok(false)
            }
            WatchAction::Drain { name } => {
                commands::run_watch_drain(engine, out, &name)?;
                Ok(true)
            }
            WatchAction::Unsubscribe { name } => {
                commands::run_watch_unsubscribe(engine, out, &name)?;
                Ok(true)
            }
            WatchAction::Stream { name } => {
                commands::run_watch_stream(engine, out, &name)?;
                Ok(false)
            }
        },

        Commands::Project { action } => match action {
            ProjectAction::CreateContext { name } => {
                commands::run_project_create_context(engine, out, &name)?;
                Ok(true)
            }
            ProjectAction::ListContexts => {
                commands::run_project_list_contexts(engine, out)?;
                Ok(false)
            }
            ProjectAction::SetPath { oid, ctx, path } => {
                commands::run_project_set_path(engine, out, &oid, &ctx, &path)?;
                Ok(true)
            }
            ProjectAction::Tree { ctx } => {
                commands::run_project_tree(engine, out, &ctx)?;
                Ok(false)
            }
            ProjectAction::Import { dir, context } => {
                commands::run_project_import(engine, out, &dir, &context)?;
                Ok(true)
            }
            ProjectAction::Export { ctx, output, oid } => {
                commands::run_project_export(engine, out, &ctx, &output, oid.as_deref())?;
                Ok(false)
            }
        },
    }
}
