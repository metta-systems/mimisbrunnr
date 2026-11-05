//! `populate` — batch object generator for performance / functional testing.
//!
//! Two modes:
//!
//! - **Manifest** (`populate --pool <pool.toml> <manifest.toml>`): install
//!   the manifest's ontology modules, create its explicit objects and generate
//!   synthetic content objects. See `manifest.rs` for the format.
//! - **Synthetic** (no manifest): create `--count` objects with tags,
//!   attributes and blob payloads drawn from the `--*-pool` options. See
//!   `commands.rs` for the testable helpers.

mod commands;
mod manifest;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;

use crate::commands::{
    BlobDistribution, Config, parse_attr_pool, parse_blob_distribution, parse_range,
    parse_tag_pool, run,
};

#[derive(Parser, Debug)]
#[command(
    name = "populate",
    about = "Batch-populate Mímisbrunnr objects from a manifest or with synthetic data"
)]
struct Cli {
    /// Path to the pool's `pool.toml`.
    #[arg(long)]
    pool: PathBuf,

    /// Population manifest (TOML). When given, the synthetic-data options
    /// other than `--seed` are ignored.
    manifest: Option<PathBuf>,

    /// Manifest mode: suppress per-object output.
    #[arg(short, long)]
    quiet: bool,

    /// Number of objects to create.
    #[arg(long, default_value_t = 1000)]
    count: u64,

    /// Optional ontology module to install before populating.
    #[arg(long)]
    ontology: Option<PathBuf>,

    /// PRNG seed for reproducibility.
    #[arg(long, default_value_t = 0)]
    seed: u64,

    /// Blob size in bytes (only used when `--blob-distribution` != none).
    #[arg(long, default_value_t = 0)]
    blob_size: usize,

    /// Blob size distribution: `none`, `fixed`, or `gaussian` (TODO).
    #[arg(long, default_value = "none")]
    blob_distribution: String,

    /// Comma-separated tag pool (e.g. `a,b,c`).
    #[arg(long, default_value = "a,b,c,d,e")]
    tag_pool: String,

    /// Tags per object as a range, e.g. `2-5` or `3`.
    #[arg(long, default_value = "1-3")]
    tags_per_object: String,

    /// Comma-separated attr pool (e.g. `name=str,age=int`).
    #[arg(long, default_value = "")]
    attr_pool: String,

    /// Attrs per object as a range, e.g. `0-2`.
    #[arg(long, default_value = "0-2")]
    attrs_per_object: String,

    /// Commit every N objects.
    #[arg(long, default_value_t = 1000)]
    commit_every: u64,

    /// Print a progress line every N objects (to stderr).
    #[arg(long, default_value_t = 100)]
    report_every: u64,
}

fn main() -> ExitCode {
    env_logger::init();

    let cli = Cli::parse();

    if let Some(manifest_path) = &cli.manifest {
        return match manifest::run_manifest(&cli.pool, manifest_path, cli.seed, cli.quiet) {
            Ok(report) => {
                println!(
                    "populate: {} module(s), {} objects, {} tags, {} attrs, {} blob bytes",
                    report.modules_installed,
                    report.objects,
                    report.tags_applied,
                    report.attrs_set,
                    report.blob_bytes,
                );
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("populate: error: {error}");
                ExitCode::FAILURE
            }
        };
    }

    let blob_distribution = match parse_blob_distribution(&cli.blob_distribution) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("populate: --blob-distribution: {e}");
            return ExitCode::from(2);
        }
    };
    let tag_pool = parse_tag_pool(&cli.tag_pool);
    if tag_pool.is_empty() {
        eprintln!("populate: --tag-pool must contain at least one tag");
        return ExitCode::from(2);
    }
    let tags_per_object = match parse_range(&cli.tags_per_object) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("populate: --tags-per-object: {e}");
            return ExitCode::from(2);
        }
    };
    let attr_pool = match parse_attr_pool(&cli.attr_pool) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("populate: --attr-pool: {e}");
            return ExitCode::from(2);
        }
    };
    let attrs_per_object = match parse_range(&cli.attrs_per_object) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("populate: --attrs-per-object: {e}");
            return ExitCode::from(2);
        }
    };

    let config = Config {
        pool_path: cli.pool,
        count: cli.count,
        ontology: cli.ontology,
        seed: cli.seed,
        blob_size: cli.blob_size,
        blob_distribution,
        tag_pool,
        tags_per_object,
        attr_pool,
        attrs_per_object,
        commit_every: cli.commit_every,
        report_every: cli.report_every,
    };

    // Sanity: warn (don't fail) if blob_distribution != none but blob_size == 0.
    if config.blob_distribution != BlobDistribution::None && config.blob_size == 0 {
        eprintln!("populate: warning: --blob-distribution set but --blob-size is 0; no blobs written");
    }

    match run(&config) {
        Ok(report) => {
            let ops = report.objects + report.tags_applied + report.attrs_set;
            let secs = report.duration_secs.max(1e-9);
            println!(
                "populate: {} objects, {} tags, {} attrs, {} blob bytes",
                report.objects, report.tags_applied, report.attrs_set, report.blob_bytes,
            );
            println!(
                "populate: wall-clock {:.3}s, {:.0} ops/s",
                report.duration_secs,
                ops as f64 / secs,
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("populate: error: {e}");
            ExitCode::FAILURE
        }
    }
}
