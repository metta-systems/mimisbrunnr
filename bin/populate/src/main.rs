//! Batch object population tool for Mímisbrunnr.
//!
//! Loads ontology modules and creates objects with tags/attributes from a
//! TOML manifest — all in a single engine session, avoiding the overhead
//! of parsing the disk pool for every individual operation.
//!
//! Designed for extension with generative population (templates, ranges,
//! procedural generation) in the future.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use clap::Parser;
use serde::Deserialize;

use mimisbrunnr::{
    engine::DiskEngine,
    ontology::OntologyModule,
    types::Value,
};

// ── CLI ──────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(
    name = "populate",
    about = "Batch-populate Mímisbrunnr objects from a manifest"
)]
struct Cli {
    /// Path to pool.toml (or directory containing it).
    #[arg(long)]
    pool: PathBuf,

    /// Path to the population manifest (TOML).
    manifest: PathBuf,

    /// Quiet mode — suppress per-object output.
    #[arg(short, long)]
    quiet: bool,
}

// ── Manifest format ──────────────────────────────────────────────────

/// Top-level manifest. All paths are resolved relative to the manifest file.
#[derive(Deserialize)]
struct Manifest {
    /// Ontology module files to load before creating objects.
    #[serde(default)]
    ontology: Vec<PathBuf>,

    /// Objects to create.
    #[serde(default)]
    objects: Vec<ObjectSpec>,

    /// Generative population templates (future).
    #[serde(default)]
    generate: Vec<GenerateSpec>,
}

/// A single object to create with its tags and attributes.
#[derive(Deserialize)]
struct ObjectSpec {
    /// Tags to apply.
    #[serde(default)]
    tags: Vec<String>,

    /// Key-value attributes.
    #[serde(default)]
    attrs: BTreeMap<String, toml::Value>,
}

/// Template for generative population (future).
///
/// Not yet implemented — the struct is accepted to allow manifests to
/// include a `[[generate]]` section that will become functional later.
#[derive(Deserialize)]
#[allow(dead_code)]
struct GenerateSpec {
    /// Human-readable label for this generator.
    name: Option<String>,

    /// Number of objects to generate.
    count: u64,

    /// Tags to apply to every generated object.
    #[serde(default)]
    tags: Vec<String>,

    /// Attribute templates. Values may contain `{seq}`, `{rand}`, etc.
    #[serde(default)]
    attrs: BTreeMap<String, toml::Value>,
}

// ── Main ─────────────────────────────────────────────────────────────

fn main() {
    let cli = Cli::parse();

    // Load manifest
    let manifest_text = match std::fs::read_to_string(&cli.manifest) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: cannot read {}: {e}", cli.manifest.display());
            std::process::exit(1);
        }
    };
    let manifest: Manifest = match toml::from_str(&manifest_text) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("error: bad manifest: {e}");
            std::process::exit(1);
        }
    };

    let manifest_dir = cli
        .manifest
        .parent()
        .unwrap_or(Path::new("."))
        .to_path_buf();

    // Open pool
    let pool_toml = if cli.pool.is_dir() {
        cli.pool.join("pool.toml")
    } else {
        cli.pool.clone()
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

    let engine = disk_engine.engine_mut();

    // ── Load ontology modules ────────────────────────────────────────

    let mut ont_tags = 0u32;
    let mut ont_impls = 0u32;

    for ont_path in &manifest.ontology {
        let resolved = resolve_path(&manifest_dir, ont_path);
        let module = match OntologyModule::from_file(&resolved) {
            Ok(m) => m,
            Err(e) => {
                eprintln!(
                    "error: failed to load ontology {}: {e}",
                    resolved.display()
                );
                std::process::exit(1);
            }
        };

        let label = module
            .name
            .clone()
            .or_else(|| module.id.clone())
            .unwrap_or_else(|| resolved.display().to_string());

        match module.install(&mut engine.dag) {
            Ok(result) => {
                if !cli.quiet {
                    println!(
                        "ontology '{label}': {} tag(s), {} implication(s)",
                        result.tags_registered, result.implications_added
                    );
                }
                ont_tags += result.tags_registered as u32;
                ont_impls += result.implications_added as u32;
            }
            Err(e) => {
                eprintln!("error: failed to install ontology '{label}': {e}");
                std::process::exit(1);
            }
        }
    }

    // ── Create objects ───────────────────────────────────────────────

    let mut created = 0u64;
    let mut tagged = 0u64;
    let mut attrs_set = 0u64;
    let mut errors = 0u64;

    for (i, spec) in manifest.objects.iter().enumerate() {
        let oid = match engine.create_object(now_ms()) {
            Ok(oid) => oid,
            Err(e) => {
                eprintln!("error creating object {i}: {e}");
                errors += 1;
                continue;
            }
        };
        created += 1;

        // Apply tags
        for tag_name in &spec.tags {
            match engine.dag.lookup(tag_name) {
                Some(tag_id) => match engine.add_tag(oid, tag_id, now_ms()) {
                    Ok(materialized) => {
                        tagged += 1;
                        if !cli.quiet {
                            let extras: Vec<&str> = materialized
                                .iter()
                                .filter_map(|m| engine.dag.get(*m).map(|d| d.name.as_str()))
                                .collect();
                            if extras.is_empty() {
                                println!("  {oid} +{tag_name}");
                            } else {
                                println!(
                                    "  {oid} +{tag_name} (→ {})",
                                    extras.join(", ")
                                );
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("error tagging {oid} with '{tag_name}': {e}");
                        errors += 1;
                    }
                },
                None => {
                    eprintln!("error: unknown tag '{tag_name}' on object {i}");
                    errors += 1;
                }
            }
        }

        // Apply attributes
        for (key, toml_val) in &spec.attrs {
            let tag_id = match engine.dag.lookup(key) {
                Some(id) => id,
                None => {
                    eprintln!("error: unknown attribute '{key}' on object {i}");
                    errors += 1;
                    continue;
                }
            };

            let value = toml_to_value(toml_val);

            match engine.set_attr(oid, tag_id, value, now_ms()) {
                Ok(()) => {
                    attrs_set += 1;
                    if !cli.quiet {
                        println!("  {oid} {key}={toml_val}");
                    }
                }
                Err(e) => {
                    eprintln!("error setting {key} on {oid}: {e}");
                    errors += 1;
                }
            }
        }
    }

    // ── Handle generate section ──────────────────────────────────────

    if !manifest.generate.is_empty() {
        let total_gen: u64 = manifest.generate.iter().map(|g| g.count).sum();
        eprintln!(
            "warning: [[generate]] section present ({total_gen} objects requested) \
             but generative population is not yet implemented — skipping"
        );
    }

    // ── Flush ────────────────────────────────────────────────────────

    if let Err(e) = disk_engine.flush() {
        eprintln!("error: failed to flush to disk: {e}");
        std::process::exit(1);
    }

    // ── Summary ──────────────────────────────────────────────────────

    println!(
        "populated {created} object(s), {tagged} tag(s), {attrs_set} attr(s)",
    );
    if ont_tags > 0 || ont_impls > 0 {
        println!("  ontology: {ont_tags} tag(s), {ont_impls} implication(s)");
    }
    if errors > 0 {
        eprintln!("  {errors} error(s)");
        std::process::exit(1);
    }
}

// ── Helpers ──────────────────────────────────────────────────────────

/// Convert a TOML value to a Mímisbrunnr Value.
fn toml_to_value(v: &toml::Value) -> Value {
    match v {
        toml::Value::Integer(n) => Value::Int(*n),
        toml::Value::Float(f) => Value::Float(*f),
        toml::Value::String(s) => Value::Text(s.clone()),
        toml::Value::Boolean(b) => Value::Int(if *b { 1 } else { 0 }),
        other => Value::Text(other.to_string()),
    }
}

/// Resolve a path relative to a base directory; absolute paths pass through.
fn resolve_path(base: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
