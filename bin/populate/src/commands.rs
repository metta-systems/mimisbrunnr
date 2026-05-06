//! Core logic of the `populate` binary, factored out into testable
//! helpers. The binary's `main.rs` is a thin CLI shim around [`run`].

use std::{
    path::{Path, PathBuf},
    time::Instant,
};

use rand::{
    RngExt, SeedableRng,
    rngs::StdRng,
    seq::SliceRandom,
};

use mimisbrunnr::{
    engine::{DiskEngine, EngineError},
    ontology::OntologyModule,
    types::{ObjectId, TagId, Value},
};

// ── Configuration types ────────────────────────────────────────────────

/// Distribution shape for blob sizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlobDistribution {
    /// Skip blobs entirely (size == 0).
    None,
    /// Every object gets a blob of exactly the configured size.
    Fixed,
}

/// Type stored under an attribute key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttrType {
    Str,
    Int,
    Float,
}

/// Parsed `--attr-pool` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttrSpec {
    pub key: String,
    pub kind: AttrType,
}

/// Parameters that drive a single populate run.
#[derive(Debug, Clone)]
pub struct Config {
    pub pool_path: PathBuf,
    pub count: u64,
    pub ontology: Option<PathBuf>,
    pub seed: u64,
    pub blob_size: usize,
    pub blob_distribution: BlobDistribution,
    pub tag_pool: Vec<String>,
    pub tags_per_object: (u32, u32),
    pub attr_pool: Vec<AttrSpec>,
    pub attrs_per_object: (u32, u32),
    pub commit_every: u64,
    pub report_every: u64,
}

/// Final summary of a populate run.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RunReport {
    pub objects: u64,
    pub tags_applied: u64,
    pub attrs_set: u64,
    pub blob_bytes: u64,
    pub duration_secs: f64,
}

// ── Parsing helpers ────────────────────────────────────────────────────

/// Parse a range like `"2-5"` or single `"3"`.
pub fn parse_range(s: &str) -> Result<(u32, u32), String> {
    let s = s.trim();
    if let Some((a, b)) = s.split_once('-') {
        let lo: u32 = a
            .trim()
            .parse()
            .map_err(|e| format!("bad range low '{a}': {e}"))?;
        let hi: u32 = b
            .trim()
            .parse()
            .map_err(|e| format!("bad range high '{b}': {e}"))?;
        if lo > hi {
            return Err(format!("range low {lo} > high {hi}"));
        }
        Ok((lo, hi))
    } else {
        let v: u32 = s.parse().map_err(|e| format!("bad range '{s}': {e}"))?;
        Ok((v, v))
    }
}

/// Parse `--blob-distribution`.
pub fn parse_blob_distribution(s: &str) -> Result<BlobDistribution, String> {
    match s {
        "none" => Ok(BlobDistribution::None),
        "fixed" => Ok(BlobDistribution::Fixed),
        "gaussian" => Err(
            "gaussian distribution requires `rand_distr` crate (TODO: not in workspace deps)"
                .into(),
        ),
        other => Err(format!("unknown blob distribution '{other}'")),
    }
}

/// Parse a comma-separated `--tag-pool`.
pub fn parse_tag_pool(s: &str) -> Vec<String> {
    s.split(',')
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .collect()
}

/// Parse a comma-separated `--attr-pool`, e.g. `key1=str,key2=int`.
pub fn parse_attr_pool(s: &str) -> Result<Vec<AttrSpec>, String> {
    let mut out = Vec::new();
    for entry in s.split(',') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let (k, t) = entry
            .split_once('=')
            .ok_or_else(|| format!("attr-pool entry '{entry}' missing '=type'"))?;
        let kind = match t.trim() {
            "str" => AttrType::Str,
            "int" => AttrType::Int,
            "float" => AttrType::Float,
            other => return Err(format!("unknown attr type '{other}' (str|int|float)")),
        };
        out.push(AttrSpec {
            key: k.trim().to_string(),
            kind,
        });
    }
    Ok(out)
}

// ── Value generation ───────────────────────────────────────────────────

/// Generate a value of the requested type using `rng`.
pub fn gen_value(rng: &mut StdRng, kind: AttrType) -> Value {
    match kind {
        AttrType::Str => {
            let len = rng.random_range(8u32..=24u32) as usize;
            // ASCII printable letters/digits.
            const ALPHA: &[u8] =
                b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
            let mut s = String::with_capacity(len);
            for _ in 0..len {
                let idx = (rng.random::<u32>() as usize) % ALPHA.len();
                s.push(ALPHA[idx] as char);
            }
            Value::Text(s)
        }
        AttrType::Int => Value::Int(rng.random_range(-1_000_000i64..=1_000_000)),
        AttrType::Float => Value::Float(rng.random_range(0.0f64..1.0)),
    }
}

/// Sample `n` distinct items uniformly from `pool` without replacement.
fn sample_distinct<T: Clone>(rng: &mut StdRng, pool: &[T], n: usize) -> Vec<T> {
    let n = n.min(pool.len());
    if n == 0 {
        return Vec::new();
    }
    let mut idx: Vec<usize> = (0..pool.len()).collect();
    idx.shuffle(rng);
    idx.into_iter().take(n).map(|i| pool[i].clone()).collect()
}

/// Generate a deterministic blob of `size` bytes from `rng`.
pub fn gen_blob(rng: &mut StdRng, size: usize) -> Vec<u8> {
    let mut buf = vec![0u8; size];
    rng.fill(&mut buf[..]);
    buf
}

// ── Driver ─────────────────────────────────────────────────────────────

/// Run the populate driver against an existing pool.
pub fn run(config: &Config) -> Result<RunReport, EngineError> {
    let started = Instant::now();
    let mut engine = DiskEngine::open(&config.pool_path)?;

    // Optionally install an ontology module.
    if let Some(ref onto_path) = config.ontology {
        install_ontology(&mut engine, onto_path)?;
    }

    // Resolve / register tag pool.
    let tag_ids: Vec<TagId> = config
        .tag_pool
        .iter()
        .map(|name| engine.engine.register_tag(name))
        .collect();

    // Resolve / register attribute pool keys.
    let attr_ids: Vec<(TagId, AttrType)> = config
        .attr_pool
        .iter()
        .map(|spec| (engine.engine.register_tag(&spec.key), spec.kind))
        .collect();

    let mut rng = StdRng::seed_from_u64(config.seed);
    let mut report = RunReport::default();

    let report_every = config.report_every.max(1);
    let commit_every = config.commit_every.max(1);

    for i in 0..config.count {
        let oid: ObjectId = engine.create_object()?;
        report.objects += 1;

        // Tags.
        let n_tags = if config.tags_per_object.0 == config.tags_per_object.1 {
            config.tags_per_object.0
        } else {
            rng.random_range(config.tags_per_object.0..=config.tags_per_object.1)
        } as usize;
        let chosen = sample_distinct(&mut rng, &tag_ids, n_tags);
        for tid in chosen {
            engine.add_tag(oid, tid)?;
            report.tags_applied += 1;
        }

        // Attrs.
        if !attr_ids.is_empty() {
            let n_attrs = if config.attrs_per_object.0 == config.attrs_per_object.1 {
                config.attrs_per_object.0
            } else {
                rng.random_range(config.attrs_per_object.0..=config.attrs_per_object.1)
            } as usize;
            let attrs = sample_distinct(&mut rng, &attr_ids, n_attrs);
            for (key_id, kind) in attrs {
                let value = gen_value(&mut rng, kind);
                engine.set_attr(oid, key_id, value)?;
                report.attrs_set += 1;
            }
        }

        // Blob.
        if config.blob_distribution != BlobDistribution::None && config.blob_size > 0 {
            let payload = gen_blob(&mut rng, config.blob_size);
            engine.write_blob(oid, &payload)?;
            report.blob_bytes += payload.len() as u64;
        }

        let done = i + 1;
        if done.is_multiple_of(commit_every) {
            engine.commit()?;
        }
        if done.is_multiple_of(report_every) {
            let elapsed = started.elapsed().as_secs_f64().max(1e-9);
            let rate = done as f64 / elapsed;
            if report.blob_bytes > 0 {
                let bps = report.blob_bytes as f64 / elapsed;
                eprintln!(
                    "populate: {done}/{total} oids, ~{rate:.0} obj/s, ~{bps:.0} B/s",
                    total = config.count,
                );
            } else {
                eprintln!(
                    "populate: {done}/{total} oids, ~{rate:.0} obj/s",
                    total = config.count,
                );
            }
        }
    }

    // Final commit before exit.
    engine.commit()?;

    report.duration_secs = started.elapsed().as_secs_f64();
    Ok(report)
}

/// Install an ontology module from a TOML file at `path` into `engine`.
pub fn install_ontology(engine: &mut DiskEngine, path: &Path) -> Result<(), EngineError> {
    let text = std::fs::read_to_string(path).map_err(EngineError::Io)?;
    let module =
        OntologyModule::from_toml(&text).map_err(mimisbrunnr::engine::EngineError::Ontology)?;
    engine.install_ontology_module(module)?;
    Ok(())
}

// ── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use mimisbrunnr::{
        engine::DiskEngine,
        pool::{DiskConfigEntry, PoolConfig},
        types::{MediaType, Query, StorageTier, Value},
    };
    use tempfile::TempDir;

    fn make_pool(tmp: &TempDir) -> PathBuf {
        let disk = tmp.path().join("disk0.img");
        let cfg = PoolConfig {
            node_id: 1,
            disks: vec![DiskConfigEntry {
                id: 0,
                path: disk,
                media_type: MediaType::NVMe,
                tier: StorageTier::Hot,
                capacity_bytes: 32 * 1024 * 1024,
            }],
        };
        let cfg_path = tmp.path().join("pool.toml");
        let de = DiskEngine::create(cfg, cfg_path.clone()).unwrap();
        drop(de);
        cfg_path
    }

    fn small_config(pool: PathBuf) -> Config {
        Config {
            pool_path: pool,
            count: 10,
            ontology: None,
            seed: 0xC0FFEE,
            blob_size: 0,
            blob_distribution: BlobDistribution::None,
            tag_pool: vec!["a".into(), "b".into(), "c".into(), "d".into()],
            tags_per_object: (2, 2),
            attr_pool: vec![],
            attrs_per_object: (0, 0),
            commit_every: 5,
            report_every: 100,
        }
    }

    // ── parse_range ────────────────────────────────────────────────────
    #[test]
    fn parse_range_pair() {
        assert_eq!(parse_range("2-5").unwrap(), (2, 5));
    }

    #[test]
    fn parse_range_single() {
        assert_eq!(parse_range("3").unwrap(), (3, 3));
    }

    #[test]
    fn parse_range_invalid_order() {
        assert!(parse_range("5-2").is_err());
    }

    #[test]
    fn parse_range_garbage() {
        assert!(parse_range("abc").is_err());
    }

    // ── parse_blob_distribution ────────────────────────────────────────
    #[test]
    fn blob_distribution_known() {
        assert_eq!(parse_blob_distribution("none").unwrap(), BlobDistribution::None);
        assert_eq!(parse_blob_distribution("fixed").unwrap(), BlobDistribution::Fixed);
    }

    #[test]
    fn blob_distribution_gaussian_rejected() {
        let err = parse_blob_distribution("gaussian").unwrap_err();
        assert!(err.contains("rand_distr") || err.contains("TODO"));
    }

    // ── parse_attr_pool ────────────────────────────────────────────────
    #[test]
    fn attr_pool_parses_known_types() {
        let parsed = parse_attr_pool("name=str,age=int,weight=float").unwrap();
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[0].kind, AttrType::Str);
        assert_eq!(parsed[1].kind, AttrType::Int);
        assert_eq!(parsed[2].kind, AttrType::Float);
    }

    #[test]
    fn attr_pool_rejects_unknown_type() {
        assert!(parse_attr_pool("foo=bigint").is_err());
    }

    // ── gen_value type round-trip ──────────────────────────────────────
    #[test]
    fn gen_value_str_returns_text() {
        let mut rng = StdRng::seed_from_u64(1);
        match gen_value(&mut rng, AttrType::Str) {
            Value::Text(s) => assert!(s.len() >= 8 && s.len() <= 24),
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[test]
    fn gen_value_int_returns_int() {
        let mut rng = StdRng::seed_from_u64(1);
        for _ in 0..32 {
            match gen_value(&mut rng, AttrType::Int) {
                Value::Int(n) => assert!((-1_000_000..=1_000_000).contains(&n)),
                other => panic!("expected Int, got {other:?}"),
            }
        }
    }

    #[test]
    fn gen_value_float_returns_float() {
        let mut rng = StdRng::seed_from_u64(1);
        for _ in 0..32 {
            match gen_value(&mut rng, AttrType::Float) {
                Value::Float(f) => assert!((0.0..1.0).contains(&f)),
                other => panic!("expected Float, got {other:?}"),
            }
        }
    }

    // ── round-trip: populate 10 with 2 tags from pool of 4 → 20 ────────
    #[test]
    fn round_trip_tag_count_adds_up() {
        let tmp = TempDir::new().unwrap();
        let pool = make_pool(&tmp);
        let cfg = small_config(pool.clone());
        let report = run(&cfg).unwrap();
        assert_eq!(report.objects, 10);
        assert_eq!(report.tags_applied, 20);

        // Re-open and query each tag, summing membership.
        let de = DiskEngine::open(&pool).unwrap();
        let mut total = 0u64;
        for name in &cfg.tag_pool {
            let tag = de.engine.resolve_tag_name(name).expect("tag");
            let bm = de.engine.query(&Query::HasTag(tag)).unwrap();
            total += bm.len();
        }
        assert_eq!(total, 20);
    }

    // ── determinism ────────────────────────────────────────────────────
    #[test]
    fn deterministic_run_with_same_seed() {
        let tmp1 = TempDir::new().unwrap();
        let tmp2 = TempDir::new().unwrap();
        let pool1 = make_pool(&tmp1);
        let pool2 = make_pool(&tmp2);

        let mut cfg1 = small_config(pool1.clone());
        cfg1.seed = 42;
        let mut cfg2 = small_config(pool2.clone());
        cfg2.seed = 42;

        let r1 = run(&cfg1).unwrap();
        let r2 = run(&cfg2).unwrap();
        assert_eq!(r1.objects, r2.objects);
        assert_eq!(r1.tags_applied, r2.tags_applied);

        // Same tag distribution (per-tag membership counts) across runs.
        let de1 = DiskEngine::open(&pool1).unwrap();
        let de2 = DiskEngine::open(&pool2).unwrap();
        for name in &cfg1.tag_pool {
            let t1 = de1.engine.resolve_tag_name(name).unwrap();
            let t2 = de2.engine.resolve_tag_name(name).unwrap();
            let n1 = de1.engine.query(&Query::HasTag(t1)).unwrap().len();
            let n2 = de2.engine.query(&Query::HasTag(t2)).unwrap().len();
            assert_eq!(n1, n2, "tag '{name}' membership differs between runs");
        }
    }

    // ── ontology install ──────────────────────────────────────────────
    #[test]
    fn ontology_install_makes_implied_tags_query_to_implied() {
        let tmp = TempDir::new().unwrap();
        let pool = make_pool(&tmp);

        // Tiny module: car → vehicle, and we use `car` as our tag pool.
        let onto_path = tmp.path().join("vehicles.toml");
        std::fs::write(
            &onto_path,
            r#"
[module]
id = "test.vehicles"
version = "0.1.0"
name = "vehicles"

[[tags]]
name = "vehicle"
semantics = "label"

[[tags]]
name = "car"
semantics = "label"

[[implications]]
from = "car"
to = "vehicle"
"#,
        )
        .unwrap();

        let mut cfg = small_config(pool.clone());
        cfg.ontology = Some(onto_path);
        cfg.tag_pool = vec!["car".into()];
        cfg.tags_per_object = (1, 1);
        cfg.count = 5;
        let _ = run(&cfg).unwrap();

        // Reopen, verify `vehicle` materialises to the same 5 oids.
        let de = DiskEngine::open(&pool).unwrap();
        let vehicle = de.engine.resolve_tag_name("vehicle").unwrap();
        let bm = de.engine.query(&Query::HasTag(vehicle)).unwrap();
        assert_eq!(bm.len(), 5);
    }

    // ── blob skipped when size == 0 ────────────────────────────────────
    #[test]
    fn blob_size_zero_skips_blob_writes() {
        let tmp = TempDir::new().unwrap();
        let pool = make_pool(&tmp);
        let cfg = small_config(pool);
        let report = run(&cfg).unwrap();
        assert_eq!(report.blob_bytes, 0);
    }

    // ── blob fixed-size run ────────────────────────────────────────────
    #[test]
    fn blob_size_writes_blobs_for_each_object() {
        let tmp = TempDir::new().unwrap();
        let pool = make_pool(&tmp);
        let mut cfg = small_config(pool.clone());
        cfg.blob_size = 1024;
        cfg.blob_distribution = BlobDistribution::Fixed;
        cfg.count = 4;
        let report = run(&cfg).unwrap();
        assert_eq!(report.blob_bytes, 4 * 1024);

        let de = DiskEngine::open(&pool).unwrap();
        assert_eq!(de.engine.object_count(), 4);
    }
}
