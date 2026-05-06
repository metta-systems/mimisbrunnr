//! Command handlers — one `pub fn` per CLI subcommand.
//!
//! All commands take a `&mut DiskEngine`, perform their work, and **never**
//! call `commit()` themselves; the caller (the binary, or a test) commits
//! after the dispatch.
//!
//! Output goes to a generic `&mut dyn Write` so unit tests can capture it. In
//! the binary we pass `std::io::stdout()` for stdout-bound output and
//! `std::io::stderr()` for diagnostics.

use std::io::Write;
use std::path::Path;

use mimisbrunnr::{
    engine::{DiskEngine, EngineError},
    ontology::{IdAllocator, OntologyError, OntologyModule},
    query::{QueryParser, explain as explain_query, to_sexpr},
    sql::{SqlEngine, SqlError, SqlOutput, explain as explain_sql},
    types::{
        Assertion, ChangeInterest, ObjectId, Query, TagDefinition, TagId, TagSemantics, Value,
        WatchEvent, value_hash,
    },
    unix::{Importer, build_path_attr},
    watch::Retention,
};

use crate::{
    oid::parse_oid,
    value_parse::{ValueKind, parse_value},
};

/// Public command error. Wrapping engine + parse + IO errors lets the binary
/// print a single line and exit non-zero.
#[derive(Debug)]
pub enum CommandError {
    Engine(EngineError),
    Sql(SqlError),
    Query(mimisbrunnr::query::QueryError),
    Ontology(OntologyError),
    Unix(mimisbrunnr::unix::UnixError),
    Io(std::io::Error),
    BadArg(String),
    NotFound(String),
    Unimplemented(&'static str),
}

impl std::fmt::Display for CommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Engine(e) => write!(f, "engine: {e}"),
            Self::Sql(e) => write!(f, "sql: {e}"),
            Self::Query(e) => write!(f, "query: {e}"),
            Self::Ontology(e) => write!(f, "ontology: {e}"),
            Self::Unix(e) => write!(f, "unix: {e}"),
            Self::Io(e) => write!(f, "io: {e}"),
            Self::BadArg(m) => write!(f, "bad argument: {m}"),
            Self::NotFound(m) => write!(f, "not found: {m}"),
            Self::Unimplemented(m) => write!(f, "TODO(rewrite-phase-N): {m}"),
        }
    }
}

impl std::error::Error for CommandError {}

impl From<EngineError> for CommandError {
    fn from(e: EngineError) -> Self {
        Self::Engine(e)
    }
}
impl From<SqlError> for CommandError {
    fn from(e: SqlError) -> Self {
        Self::Sql(e)
    }
}
impl From<mimisbrunnr::query::QueryError> for CommandError {
    fn from(e: mimisbrunnr::query::QueryError) -> Self {
        Self::Query(e)
    }
}
impl From<OntologyError> for CommandError {
    fn from(e: OntologyError) -> Self {
        Self::Ontology(e)
    }
}
impl From<mimisbrunnr::unix::UnixError> for CommandError {
    fn from(e: mimisbrunnr::unix::UnixError) -> Self {
        Self::Unix(e)
    }
}
impl From<std::io::Error> for CommandError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// Convenience alias for command return values.
pub type CommandResult<T = ()> = Result<T, CommandError>;

// =========================================================================
// Object operations.
// =========================================================================

/// `mimir tag obj:<oid> <tag1> <tag2> ...`
///
/// Tags missing from the ontology are auto-registered as `Label`-semantics
/// tags via [`mimisbrunnr::engine::Engine::register_tag`] before the assertion
/// is recorded.
pub fn run_tag<W: Write>(
    engine: &mut DiskEngine,
    out: &mut W,
    oid_arg: &str,
    tag_names: &[String],
) -> CommandResult {
    let oid = parse_oid(oid_arg).map_err(CommandError::BadArg)?;
    ensure_object(engine, oid)?;

    for tag_name in tag_names {
        let tag_id = engine
            .engine
            .resolve_tag_name(tag_name)
            .unwrap_or_else(|| engine.engine.register_tag(tag_name));
        engine.add_tag(oid, tag_id)?;
        writeln!(out, "tagged {oid} {tag_name}")?;
    }
    Ok(())
}

/// `mimir untag obj:<oid> <tag>`
pub fn run_untag<W: Write>(
    engine: &mut DiskEngine,
    out: &mut W,
    oid_arg: &str,
    tag_name: &str,
) -> CommandResult {
    let oid = parse_oid(oid_arg).map_err(CommandError::BadArg)?;
    let tag_id = engine
        .engine
        .resolve_tag_name(tag_name)
        .ok_or_else(|| CommandError::NotFound(format!("tag {tag_name:?}")))?;
    engine.remove_tag(oid, tag_id)?;
    writeln!(out, "untagged {oid} {tag_name}")?;
    Ok(())
}

/// `mimir set obj:<oid> <key> <value> [--type <kind>]`
pub fn run_set<W: Write>(
    engine: &mut DiskEngine,
    out: &mut W,
    oid_arg: &str,
    key: &str,
    raw_value: &str,
    kind: ValueKind,
) -> CommandResult {
    let oid = parse_oid(oid_arg).map_err(CommandError::BadArg)?;
    ensure_object(engine, oid)?;
    let key_id = engine
        .engine
        .resolve_tag_name(key)
        .unwrap_or_else(|| engine.engine.register_tag(key));
    let value = parse_value(raw_value, kind).map_err(CommandError::BadArg)?;
    engine.set_attr(oid, key_id, value.clone())?;
    writeln!(out, "set {oid} {key}={value}")?;
    Ok(())
}

/// `mimir info obj:<oid>` — show every assertion attached to the object.
pub fn run_info<W: Write>(
    engine: &mut DiskEngine,
    out: &mut W,
    oid_arg: &str,
) -> CommandResult {
    let oid = parse_oid(oid_arg).map_err(CommandError::BadArg)?;
    let raw = oid.to_u64();
    let rec = engine
        .engine
        .object_table
        .get(raw)
        .ok_or_else(|| CommandError::NotFound(format!("{oid}")))?;
    writeln!(out, "object={oid}")?;
    writeln!(
        out,
        "state={:?} blob_length={} stored_size={} tag_count={} attr_count={} relation_count={}",
        rec.state().unwrap_or(mimisbrunnr::types::ObjectState::Active),
        { rec.blob_length },
        { rec.stored_size },
        { rec.tag_count },
        { rec.attr_count },
        { rec.relation_count },
    )?;

    // Snapshot assertions for this oid.
    let assertions: Vec<_> = engine.engine.forward_index.assertions_of(oid).to_vec();
    for (a, origin) in assertions {
        match a {
            Assertion::Tag(t) => {
                let name = name_or_id(engine, t);
                writeln!(out, "tag={name} origin={origin:?}")?;
            }
            Assertion::Attr { key, value } => {
                let name = name_or_id(engine, key);
                writeln!(out, "attr={name} value={value} origin={origin:?}")?;
            }
            Assertion::Relation { predicate, target } => {
                let name = name_or_id(engine, predicate);
                writeln!(
                    out,
                    "relation={name} target={target} origin={origin:?}"
                )?;
            }
        }
    }
    Ok(())
}

// =========================================================================
// Queries.
// =========================================================================

/// `mimir query "<sexpr>"` — DSL form.
pub fn run_query_sexpr<W: Write>(
    engine: &mut DiskEngine,
    out: &mut W,
    sexpr: &str,
    explain_only: bool,
) -> CommandResult {
    let query = parse_sexpr(engine, sexpr)?;
    if explain_only {
        write!(out, "{}", explain_query(&query))?;
        return Ok(());
    }
    let mut ids = engine.engine.query_full(&query)?;
    ids.sort();
    for oid in &ids {
        writeln!(out, "{oid}")?;
    }
    writeln!(out, "({} result{})", ids.len(), if ids.len() == 1 { "" } else { "s" })?;
    Ok(())
}

/// `mimir query --sql "<sql>"`
pub fn run_query_sql<W: Write>(
    engine: &mut DiskEngine,
    out: &mut W,
    sql: &str,
    explain_only: bool,
) -> CommandResult {
    if explain_only {
        let plan = explain_sql(sql, &engine.engine.ontology)?;
        write!(out, "{plan}")?;
        return Ok(());
    }
    let executor = mimisbrunnr::query::QueryExecutor::new(
        &engine.engine.tag_index,
        &engine.engine.kv_index,
        &engine.engine.range_index,
        &engine.engine.forward_index,
        &engine.engine.ontology,
    );
    let sql_engine = SqlEngine::new(executor, &engine.engine.ontology);
    match sql_engine.execute(sql)? {
        SqlOutput::Rows(rows) => {
            for oid in &rows {
                writeln!(out, "{oid}")?;
            }
            writeln!(out, "({} row{})", rows.len(), if rows.len() == 1 { "" } else { "s" })?;
        }
        SqlOutput::Count(c) => writeln!(out, "count={c}")?,
    }
    Ok(())
}

/// `mimir explore <tag>` — faceted breakdown of objects carrying `tag`.
pub fn run_explore<W: Write>(
    engine: &mut DiskEngine,
    out: &mut W,
    tag_name: &str,
    max_facets: usize,
) -> CommandResult {
    let tag_id = engine
        .engine
        .resolve_tag_name(tag_name)
        .ok_or_else(|| CommandError::NotFound(format!("tag {tag_name:?}")))?;

    let executor = mimisbrunnr::query::QueryExecutor::new(
        &engine.engine.tag_index,
        &engine.engine.kv_index,
        &engine.engine.range_index,
        &engine.engine.forward_index,
        &engine.engine.ontology,
    );
    let explorer = mimisbrunnr::query::FacetedExplorer::new(&executor);
    let groups = explorer.explore(&Query::HasTag(tag_id), max_facets)?;
    writeln!(out, "selection={tag_name} facets={}", groups.len())?;
    for g in &groups {
        let name = name_or_id(engine, g.tag);
        writeln!(out, "{name}\t{}", g.count)?;
    }
    Ok(())
}

// =========================================================================
// Ontology.
// =========================================================================

/// `mimir ontology list` — installed modules.
pub fn run_ontology_list<W: Write>(
    engine: &mut DiskEngine,
    out: &mut W,
) -> CommandResult {
    let mut modules: Vec<_> = engine.engine.ontology.installed_modules.values().collect();
    modules.sort_by(|a, b| a.id.cmp(&b.id));
    writeln!(out, "modules={}", modules.len())?;
    for m in &modules {
        writeln!(
            out,
            "id={} version={} name={:?} tags={} implications={}",
            m.id,
            m.version,
            m.name,
            m.installed_tags.len(),
            m.installed_implications.len()
        )?;
    }
    Ok(())
}

/// `mimir ontology install <module-toml-path>`
pub fn run_ontology_install<W: Write>(
    engine: &mut DiskEngine,
    out: &mut W,
    path: &Path,
) -> CommandResult {
    let body = std::fs::read_to_string(path)?;
    let module = OntologyModule::from_toml(&body)?;
    let res = engine.install_ontology_module(module)?;
    writeln!(
        out,
        "installed module={} name={:?} tags_registered={} tags_skipped={} implications_added={}",
        res.module_id,
        res.module_name,
        res.tags_registered,
        res.tags_skipped,
        res.implications_added
    )?;
    Ok(())
}

/// `mimir ontology remove <module-id>`
pub fn run_ontology_remove<W: Write>(
    engine: &mut DiskEngine,
    out: &mut W,
    module_id: &str,
) -> CommandResult {
    let removed = engine.engine.ontology.scrub(&module_id.to_string())?;
    writeln!(out, "removed module={module_id} tags_dropped={removed}")?;
    Ok(())
}

/// `mimir ontology orphans` — tags not registered by any installed module.
pub fn run_ontology_orphans<W: Write>(
    engine: &mut DiskEngine,
    out: &mut W,
) -> CommandResult {
    use std::collections::BTreeSet;
    let mut owned: BTreeSet<TagId> = BTreeSet::new();
    for m in engine.engine.ontology.installed_modules.values() {
        owned.extend(m.installed_tags.iter().copied());
    }
    let mut orphans: Vec<TagId> = engine
        .engine
        .ontology
        .tags
        .keys()
        .copied()
        .filter(|t| !owned.contains(t))
        .collect();
    orphans.sort();
    writeln!(out, "orphans={}", orphans.len())?;
    for t in &orphans {
        let name = engine
            .engine
            .ontology
            .tags
            .get(t)
            .map(|d| d.name.as_str())
            .unwrap_or("?");
        writeln!(out, "{name}\t{}", t.raw())?;
    }
    Ok(())
}

/// `mimir ontology show <tag>` — tag definition + implication closure.
pub fn run_ontology_show<W: Write>(
    engine: &mut DiskEngine,
    out: &mut W,
    name: &str,
) -> CommandResult {
    let id = engine
        .engine
        .resolve_tag_name(name)
        .ok_or_else(|| CommandError::NotFound(format!("tag {name:?}")))?;
    let def = engine
        .engine
        .ontology
        .tags
        .get(&id)
        .cloned()
        .ok_or_else(|| CommandError::NotFound(format!("tag-def {name:?}")))?;
    writeln!(
        out,
        "name={} id={} semantics={:?} storage={:?}",
        def.name,
        def.id.raw(),
        def.semantics,
        def.storage
    )?;
    let closure = engine.engine.ontology.materialise(&[id]);
    writeln!(out, "closure={}", closure.len())?;
    for t in &closure {
        let name = name_or_id(engine, *t);
        writeln!(out, "{name}\t{}", t.raw())?;
    }
    Ok(())
}

/// Stub for `mimir ontology adopt`. Phase 7a returns a TODO.
pub fn run_ontology_adopt<W: Write>(
    _engine: &mut DiskEngine,
    _out: &mut W,
    _orphan: &str,
    _module_id: &str,
) -> CommandResult {
    Err(CommandError::Unimplemented(
        "ontology adopt: needs module mutation in OntologyState",
    ))
}

// =========================================================================
// Subscriptions.
// =========================================================================

/// `mimir watch "<sexpr>" --name <n>` — register a subscription.
pub fn run_watch_register<W: Write>(
    engine: &mut DiskEngine,
    out: &mut W,
    name: &str,
    sexpr: &str,
) -> CommandResult {
    let query = parse_sexpr(engine, sexpr)?;
    let id = engine.subscribe(
        name.to_string(),
        query,
        ChangeInterest::ALL,
        Retention::default(),
    )?;
    writeln!(out, "subscription_id={id} name={name}")?;
    Ok(())
}

/// `mimir watch list`
pub fn run_watch_list<W: Write>(
    engine: &mut DiskEngine,
    out: &mut W,
) -> CommandResult {
    let mut subs: Vec<_> = engine
        .engine
        .subscriptions
        .subscriptions
        .values()
        .collect();
    subs.sort_by_key(|s| s.id);
    writeln!(out, "subscriptions={}", subs.len())?;
    for s in &subs {
        writeln!(
            out,
            "id={} name={:?} state={:?} cursor={} pending={} sexpr={}",
            s.id,
            s.name,
            s.state,
            s.cursor,
            engine
                .engine
                .subscriptions
                .pending_events
                .get(&s.id)
                .map(|q| q.len())
                .unwrap_or(0),
            to_sexpr(&s.query, |t| name_or_id(engine, t)),
        )?;
    }
    Ok(())
}

/// `mimir watch drain <name>` — drain pending events as JSON-ish lines.
pub fn run_watch_drain<W: Write>(
    engine: &mut DiskEngine,
    out: &mut W,
    name: &str,
) -> CommandResult {
    let id = sub_id_by_name(engine, name)
        .ok_or_else(|| CommandError::NotFound(format!("subscription {name:?}")))?;
    let events = engine.engine.subscriptions.drain(id);
    writeln!(out, "drained={}", events.len())?;
    for ev in &events {
        writeln!(out, "{}", format_event(ev))?;
    }
    Ok(())
}

/// `mimir watch unsubscribe <name>`
pub fn run_watch_unsubscribe<W: Write>(
    engine: &mut DiskEngine,
    out: &mut W,
    name: &str,
) -> CommandResult {
    let id = sub_id_by_name(engine, name)
        .ok_or_else(|| CommandError::NotFound(format!("subscription {name:?}")))?;
    engine
        .engine
        .subscriptions
        .unregister(id)
        .map_err(|e| CommandError::BadArg(e.to_string()))?;
    writeln!(out, "unsubscribed name={name} id={id}")?;
    Ok(())
}

/// Stub for `mimir watch stream`. Phase 7a returns a TODO.
pub fn run_watch_stream<W: Write>(
    _engine: &mut DiskEngine,
    _out: &mut W,
    _name: &str,
) -> CommandResult {
    Err(CommandError::Unimplemented(
        "watch stream: continuous output deferred",
    ))
}

// =========================================================================
// Path projections.
// =========================================================================

/// Canonical name of the grouping tag created for path-context `name`.
pub fn context_tag_name(name: &str) -> String {
    format!("unix-path-context:{name}")
}

/// `mimir project create-context <name>`
pub fn run_project_create_context<W: Write>(
    engine: &mut DiskEngine,
    out: &mut W,
    name: &str,
) -> CommandResult {
    let canonical = context_tag_name(name);
    // Ensure there's a Grouping-semantics tag for the context. If not present,
    // register one through the ontology directly so we get the correct
    // semantics (engine.register_tag would default to Label).
    let tag_id = match engine.engine.ontology.names.get(&canonical) {
        Some(&existing) => {
            let def = engine.engine.ontology.tags.get(&existing).cloned();
            if !matches!(def.map(|d| d.semantics), Some(TagSemantics::Grouping)) {
                return Err(CommandError::BadArg(format!(
                    "tag {canonical:?} already exists with non-Grouping semantics"
                )));
            }
            existing
        }
        None => {
            let mut alloc = IdAllocator::starting_at(next_tag_id(engine));
            let id = alloc.next_id();
            let def = TagDefinition {
                id,
                name: canonical.clone(),
                semantics: TagSemantics::Grouping,
                implies: vec![],
                storage: None,
            };
            engine.engine.ontology.dag.add_tag(id);
            engine
                .engine
                .ontology
                .names
                .insert(canonical.clone(), id);
            engine.engine.ontology.tags.insert(id, def);
            id
        }
    };

    engine.engine.path_contexts.create_context(
        &engine.engine.ontology,
        tag_id,
        std::path::PathBuf::from("/"),
    )?;
    writeln!(out, "created context={name} tag={canonical} id={tag_id}")?;
    Ok(())
}

/// `mimir project list-contexts`
pub fn run_project_list_contexts<W: Write>(
    engine: &mut DiskEngine,
    out: &mut W,
) -> CommandResult {
    let mut rows: Vec<(String, TagId, usize)> = Vec::new();
    for (tag, proj) in &engine.engine.path_contexts.projections {
        let name = engine
            .engine
            .ontology
            .tags
            .get(tag)
            .map(|d| {
                d.name
                    .strip_prefix("unix-path-context:")
                    .unwrap_or(d.name.as_str())
                    .to_string()
            })
            .unwrap_or_else(|| format!("@{}", tag.raw()));
        rows.push((name, *tag, proj.len()));
    }
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    writeln!(out, "contexts={}", rows.len())?;
    for (name, tag, count) in &rows {
        writeln!(out, "name={name} tag={} entries={count}", tag.raw())?;
    }
    Ok(())
}

/// `mimir project set-path obj:<oid> <ctx> <path>`
pub fn run_project_set_path<W: Write>(
    engine: &mut DiskEngine,
    out: &mut W,
    oid_arg: &str,
    context: &str,
    path: &str,
) -> CommandResult {
    let oid = parse_oid(oid_arg).map_err(CommandError::BadArg)?;
    ensure_object(engine, oid)?;
    let canonical = context_tag_name(context);
    let context_id = engine
        .engine
        .resolve_tag_name(&canonical)
        .ok_or_else(|| CommandError::NotFound(format!("context {context:?}")))?;

    // Ensure the projection exists (the user might have set-path without
    // calling create-context first; that's not allowed here).
    if engine.engine.path_contexts.get(context_id).is_none() {
        return Err(CommandError::NotFound(format!(
            "no projection registered for context {context:?}"
        )));
    }

    // 1. Update the in-memory projection.
    let proj = engine
        .engine
        .path_contexts
        .get_mut(context_id)
        .expect("just checked");
    proj.add(oid, path.to_string())?;

    // 2. Persist as `Attr(unix-path, Scoped { context, Text(path) })`.
    let unix_path_id = engine
        .engine
        .resolve_tag_name(mimisbrunnr::unix::UNIX_PATH_TAG_NAME)
        .unwrap_or_else(|| {
            engine
                .engine
                .register_tag(mimisbrunnr::unix::UNIX_PATH_TAG_NAME)
        });
    let assertion = build_path_attr(unix_path_id, context_id, path)?;
    if let Assertion::Attr { key, value } = assertion {
        engine.set_attr(oid, key, value)?;
    }

    writeln!(out, "set-path oid={oid} context={context} path={path:?}")?;
    Ok(())
}

/// `mimir project tree <ctx>`
pub fn run_project_tree<W: Write>(
    engine: &mut DiskEngine,
    out: &mut W,
    context: &str,
) -> CommandResult {
    let canonical = context_tag_name(context);
    let context_id = engine
        .engine
        .resolve_tag_name(&canonical)
        .ok_or_else(|| CommandError::NotFound(format!("context {context:?}")))?;
    let proj = engine
        .engine
        .path_contexts
        .get(context_id)
        .ok_or_else(|| CommandError::NotFound(format!("projection for {context:?}")))?;

    let mut rows: Vec<(String, ObjectId)> =
        proj.iter().map(|(oid, p)| (p.to_string(), oid)).collect();
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    writeln!(out, "context={context} entries={}", rows.len())?;
    for (path, oid) in &rows {
        writeln!(out, "{path}\t{oid}")?;
    }
    Ok(())
}

/// `mimir project import <dir> --context <ctx>`
///
/// Phase 7a: walks the host tree via [`Importer`] and creates one object
/// per file with the path attribute attached, but **does not** read file
/// contents into the blob store. Use the populate tool for that.
pub fn run_project_import<W: Write>(
    engine: &mut DiskEngine,
    out: &mut W,
    dir: &Path,
    context: &str,
) -> CommandResult {
    let canonical = context_tag_name(context);
    let context_id = engine
        .engine
        .resolve_tag_name(&canonical)
        .ok_or_else(|| CommandError::NotFound(format!("context {context:?}")))?;
    if engine.engine.path_contexts.get(context_id).is_none() {
        return Err(CommandError::NotFound(format!(
            "no projection registered for context {context:?}"
        )));
    }
    let unix_path_id = engine
        .engine
        .resolve_tag_name(mimisbrunnr::unix::UNIX_PATH_TAG_NAME)
        .unwrap_or_else(|| {
            engine
                .engine
                .register_tag(mimisbrunnr::unix::UNIX_PATH_TAG_NAME)
        });

    let importer = Importer::new(dir, context_id);
    let entries = importer.scan()?;

    let mut created = 0usize;
    let mut skipped = 0usize;
    for entry in &entries {
        if !matches!(entry.kind, mimisbrunnr::unix::ImportKind::File) {
            skipped += 1;
            continue;
        }
        let oid = engine.create_object()?;
        let assertion = build_path_attr(unix_path_id, context_id, &entry.relative_path)?;
        if let Assertion::Attr { key, value } = assertion {
            engine.set_attr(oid, key, value)?;
        }
        let proj = engine
            .engine
            .path_contexts
            .get_mut(context_id)
            .expect("checked above");
        proj.add(oid, entry.relative_path.clone())?;
        created += 1;
    }
    writeln!(
        out,
        "import dir={} context={context} created={created} skipped={skipped} (blob ingestion deferred)",
        dir.display()
    )?;
    Ok(())
}

/// Stub for `mimir project export`. Phase 7a returns a TODO.
pub fn run_project_export<W: Write>(
    _engine: &mut DiskEngine,
    _out: &mut W,
    _context: &str,
    _output: &Path,
) -> CommandResult {
    Err(CommandError::Unimplemented(
        "project export: needs blob fetching plumbing (Phase 7+)",
    ))
}

// =========================================================================
// Internal helpers.
// =========================================================================

fn ensure_object(engine: &DiskEngine, oid: ObjectId) -> CommandResult {
    if engine.engine.object_table.get(oid.to_u64()).is_none() {
        return Err(CommandError::NotFound(format!(
            "object {oid} (use `mimir create` first)"
        )));
    }
    Ok(())
}

/// `mimir create` — allocate one fresh object id and print it. Not in the
/// DESIGN §A surface but we keep it as the only way for callers to mint an
/// id they can then `tag`, `set`, etc.
pub fn run_create<W: Write>(engine: &mut DiskEngine, out: &mut W) -> CommandResult {
    let oid = engine.create_object()?;
    writeln!(out, "{oid}")?;
    Ok(())
}

fn name_or_id(engine: &DiskEngine, tag: TagId) -> String {
    engine
        .engine
        .ontology
        .tags
        .get(&tag)
        .map(|d| d.name.clone())
        .unwrap_or_else(|| format!("@{}", tag.raw()))
}

fn parse_sexpr(engine: &mut DiskEngine, sexpr: &str) -> CommandResult<Query> {
    // Snapshot the names map so the resolver closure doesn't borrow `engine`
    // for the duration of the parse.
    let names = engine.engine.ontology.names.clone();
    QueryParser::parse(sexpr, |n| names.get(n).copied()).map_err(CommandError::Query)
}

fn sub_id_by_name(engine: &DiskEngine, name: &str) -> Option<u64> {
    engine
        .engine
        .subscriptions
        .subscriptions
        .values()
        .find(|s| s.name == name)
        .map(|s| s.id)
}

fn next_tag_id(engine: &DiskEngine) -> u32 {
    engine
        .engine
        .ontology
        .tags
        .keys()
        .map(|t| t.raw())
        .max()
        .map(|m| m + 1)
        .unwrap_or(1)
}

/// Render a [`WatchEvent`] as a single line of `key=value` pairs. Trivial
/// JSON-ish output that's easy to grep / parse downstream without pulling in
/// `serde_json` (not in the workspace deps).
fn format_event(ev: &WatchEvent) -> String {
    match ev {
        WatchEvent::Entered { oid, timestamp } => format!(
            r#"{{"kind":"Entered","oid":"{oid}","ts_ns":{}}}"#,
            timestamp.physical_ns
        ),
        WatchEvent::Exited { oid, timestamp } => format!(
            r#"{{"kind":"Exited","oid":"{oid}","ts_ns":{}}}"#,
            timestamp.physical_ns
        ),
        WatchEvent::TagAdded { oid, tag, timestamp } => format!(
            r#"{{"kind":"TagAdded","oid":"{oid}","tag":{},"ts_ns":{}}}"#,
            tag.raw(),
            timestamp.physical_ns
        ),
        WatchEvent::TagRemoved { oid, tag, timestamp } => format!(
            r#"{{"kind":"TagRemoved","oid":"{oid}","tag":{},"ts_ns":{}}}"#,
            tag.raw(),
            timestamp.physical_ns
        ),
        WatchEvent::ContentChanged { oid, timestamp } => format!(
            r#"{{"kind":"ContentChanged","oid":"{oid}","ts_ns":{}}}"#,
            timestamp.physical_ns
        ),
        WatchEvent::Created { oid, timestamp } => format!(
            r#"{{"kind":"Created","oid":"{oid}","ts_ns":{}}}"#,
            timestamp.physical_ns
        ),
        WatchEvent::Deleted { oid, timestamp } => format!(
            r#"{{"kind":"Deleted","oid":"{oid}","ts_ns":{}}}"#,
            timestamp.physical_ns
        ),
    }
}

// `value_hash` is re-exported in `mimisbrunnr::types`, but not used yet —
// suppress the unused-import without disabling the convenience.
#[doc(hidden)]
pub fn _force_use_value_hash(v: &Value) -> u64 {
    value_hash(v, &[0u8; 16])
}
