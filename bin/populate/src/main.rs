//! Batch object population tool for Mímisbrunnr.
//!
//! Loads ontology modules and creates objects with tags/attributes from a
//! TOML manifest. Supports generative population with synthetic content
//! (music, video, text) with configurable size distributions.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use clap::Parser;
use rand::Rng;
use serde::Deserialize;

use mimisbrunnr::{
    engine::DiskEngine,
    ontology::OntologyModule,
    types::Value,
};
use mimisbrunnr_transform::{CompressionAlgo, EncryptionMode, TransformPipeline};

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

    /// Zstd compression level (0 = no compression).
    #[arg(long, default_value = "3")]
    compress_level: i32,
}

// ── Manifest format ──────────────────────────────────────────────────

#[derive(Deserialize)]
struct Manifest {
    /// Ontology module files to load before creating objects.
    #[serde(default)]
    ontology: Vec<PathBuf>,

    /// Objects to create.
    #[serde(default)]
    objects: Vec<ObjectSpec>,

    /// Generative population templates.
    #[serde(default)]
    generate: Vec<GenerateSpec>,
}

#[derive(Deserialize)]
struct ObjectSpec {
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    attrs: BTreeMap<String, toml::Value>,
}

#[derive(Deserialize)]
struct GenerateSpec {
    /// Human-readable label for this generator.
    name: Option<String>,

    /// Number of objects to generate.
    count: u64,

    /// Content type: "music", "video", "text", "binary".
    #[serde(default = "default_content_type")]
    content_type: String,

    /// Minimum content size in bytes.
    #[serde(default)]
    min_size: Option<u64>,

    /// Maximum content size in bytes.
    #[serde(default)]
    max_size: Option<u64>,

    /// Tags to apply to every generated object.
    #[serde(default)]
    tags: Vec<String>,

    /// Attribute templates. Values may contain `{seq}`, `{type}`.
    #[serde(default)]
    attrs: BTreeMap<String, toml::Value>,
}

fn default_content_type() -> String {
    "binary".to_string()
}

// ── Content generators ──────────────────────────────────────────────

/// Sample a size from a log-normal-ish distribution between min and max.
fn sample_size(rng: &mut impl Rng, min: u64, max: u64) -> usize {
    if min >= max {
        return min as usize;
    }
    let ln_min = (min as f64).ln();
    let ln_max = (max as f64).ln();
    let ln_size = rng.random_range(ln_min..ln_max);
    ln_size.exp() as usize
}

const WORD_LIST: &[&str] = &[
    "the", "be", "to", "of", "and", "a", "in", "that", "have", "I",
    "it", "for", "not", "on", "with", "he", "as", "you", "do", "at",
    "this", "but", "his", "by", "from", "they", "we", "say", "her", "she",
    "or", "an", "will", "my", "one", "all", "would", "there", "their", "what",
    "so", "up", "out", "if", "about", "who", "get", "which", "go", "me",
    "when", "make", "can", "like", "time", "no", "just", "him", "know", "take",
    "people", "into", "year", "your", "good", "some", "could", "them", "see", "other",
    "than", "then", "now", "look", "only", "come", "its", "over", "think", "also",
    "back", "after", "use", "two", "how", "our", "work", "first", "well", "way",
    "even", "new", "want", "because", "any", "these", "give", "day", "most", "us",
    "great", "between", "need", "large", "must", "home", "big", "still", "long",
    "music", "sound", "wave", "signal", "frequency", "amplitude", "rhythm", "melody",
    "harmony", "chord", "note", "beat", "tempo", "pitch", "tone", "resonance",
    "spectrum", "filter", "oscillator", "synthesizer", "sampler", "sequencer",
    "analog", "digital", "modular", "ambient", "electronic", "acoustic",
];

/// Generate text content from a word dictionary, forming sentences and paragraphs.
fn generate_text(rng: &mut impl Rng, size: usize) -> Vec<u8> {
    let mut buf = String::with_capacity(size);
    let mut words_in_sentence = 0;
    let mut sentences_in_para = 0;

    while buf.len() < size {
        let word = WORD_LIST[rng.random_range(0..WORD_LIST.len())];

        if words_in_sentence == 0 {
            // Capitalize first word
            let mut chars = word.chars();
            if let Some(c) = chars.next() {
                buf.extend(c.to_uppercase());
                buf.extend(chars);
            }
        } else {
            buf.push(' ');
            buf.push_str(word);
        }

        words_in_sentence += 1;

        // End sentence every 8-20 words
        if words_in_sentence >= rng.random_range(8..20) {
            buf.push_str(". ");
            words_in_sentence = 0;
            sentences_in_para += 1;

            // New paragraph every 3-7 sentences
            if sentences_in_para >= rng.random_range(3..7) {
                buf.push_str("\n\n");
                sentences_in_para = 0;
            }
        }
    }

    buf.truncate(size);
    buf.into_bytes()
}

/// Generate music-like content: repeating frames with periodic mutations.
/// This produces moderately compressible data (repeating structure with variation).
fn generate_music(rng: &mut impl Rng, size: usize) -> Vec<u8> {
    let mut buf = Vec::with_capacity(size);

    // Write a fake header
    buf.extend_from_slice(b"MBRMUSIC");
    let sample_rate: u32 = 44100;
    let channels: u16 = 2;
    buf.extend_from_slice(&sample_rate.to_le_bytes());
    buf.extend_from_slice(&channels.to_le_bytes());

    // Generate audio frames: 1024-sample blocks with repeating waveforms
    let frame_size = 1024;
    let mut base_frame = vec![0u8; frame_size];
    rng.fill(&mut base_frame[..]);

    while buf.len() < size {
        // Every ~16 frames, generate a new base pattern (like a new section)
        if rng.random_range(0..16u32) == 0 {
            rng.fill(&mut base_frame[..]);
        }

        // Copy base frame with small mutations (like audio variation)
        let remaining = size - buf.len();
        let write_len = remaining.min(frame_size);
        let start = buf.len();
        buf.extend_from_slice(&base_frame[..write_len]);

        // Apply mutations to ~5% of bytes (instrument variation, dynamics)
        let mutations = write_len / 20;
        for _ in 0..mutations {
            let pos = start + rng.random_range(0..write_len);
            buf[pos] = buf[pos].wrapping_add(rng.random_range(0..32u8));
        }
    }

    buf.truncate(size);
    buf
}

/// Generate video-like content: I-frames (random) with P-frames (delta from previous).
/// This produces variably compressible data.
fn generate_video(rng: &mut impl Rng, size: usize) -> Vec<u8> {
    let mut buf = Vec::with_capacity(size);

    // Fake container header
    buf.extend_from_slice(b"MBRVIDEO");
    let width: u16 = 1920;
    let height: u16 = 1080;
    buf.extend_from_slice(&width.to_le_bytes());
    buf.extend_from_slice(&height.to_le_bytes());

    let frame_size = 4096; // Simulated compressed frame size
    let mut prev_frame = vec![0u8; frame_size];
    rng.fill(&mut prev_frame[..]);

    let mut frame_num = 0u32;
    while buf.len() < size {
        let remaining = size - buf.len();
        let write_len = remaining.min(frame_size + 8);

        if frame_num % 30 == 0 {
            // I-frame: fully random (keyframe)
            buf.push(b'I');
            buf.extend_from_slice(&frame_num.to_le_bytes());
            buf.extend_from_slice(&[0, 0, 0]); // padding
            let data_len = (write_len).saturating_sub(8).min(frame_size);
            rng.fill(&mut prev_frame[..data_len]);
            buf.extend_from_slice(&prev_frame[..data_len]);
        } else {
            // P-frame: delta from previous (more compressible)
            buf.push(b'P');
            buf.extend_from_slice(&frame_num.to_le_bytes());
            buf.extend_from_slice(&[0, 0, 0]);
            let data_len = (write_len).saturating_sub(8).min(frame_size);
            // Only change ~10% of the frame
            let changes = data_len / 10;
            for _ in 0..changes {
                let pos = rng.random_range(0..data_len);
                prev_frame[pos] = rng.random::<u8>();
            }
            buf.extend_from_slice(&prev_frame[..data_len]);
        }
        frame_num += 1;
    }

    buf.truncate(size);
    buf
}

/// Generate pure random binary content (incompressible).
fn generate_binary(rng: &mut impl Rng, size: usize) -> Vec<u8> {
    let mut buf = vec![0u8; size];
    rng.fill(&mut buf[..]);
    buf
}

/// Generate content of the given type and size.
fn generate_content(rng: &mut impl Rng, content_type: &str, size: usize) -> Vec<u8> {
    match content_type {
        "text" => generate_text(rng, size),
        "music" => generate_music(rng, size),
        "video" => generate_video(rng, size),
        _ => generate_binary(rng, size),
    }
}

/// Default size range for a content type.
fn default_size_range(content_type: &str) -> (u64, u64) {
    match content_type {
        "music" => (1024 * 1024, 15 * 1024 * 1024),           // 1 MB – 15 MB
        "video" => (100 * 1024 * 1024, 1024 * 1024 * 1024),   // 100 MB – 1 GB
        "text"  => (1024, 15 * 1024),                          // 1 KB – 15 KB
        _       => (1024, 1024 * 1024),                        // 1 KB – 1 MB
    }
}

/// Substitute template variables in a string value.
fn substitute(s: &str, seq: u64, content_type: &str) -> String {
    s.replace("{seq}", &seq.to_string())
     .replace("{type}", content_type)
}

// ── Main ─────────────────────────────────────────────────────────────

fn main() {
    env_logger::init();
    let cli = Cli::parse();

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

    // Configure engine's transform pipeline
    {
        let compression = if cli.compress_level > 0 {
            CompressionAlgo::Zstd(cli.compress_level)
        } else {
            CompressionAlgo::None
        };
        let pipeline = TransformPipeline::new(compression, EncryptionMode::None, [0u8; 32]);
        disk_engine.engine_mut().set_transform(pipeline);
    }

    // ── Load ontology modules ────────────────────────────────────────

    let mut ont_tags = 0u32;
    let mut ont_impls = 0u32;

    {
        let engine = disk_engine.engine_mut();
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
    }

    // ── Create explicit objects ───────────────────────────────────────

    let mut created = 0u64;
    let mut tagged = 0u64;
    let mut attrs_set = 0u64;
    let mut errors = 0u64;

    {
        let engine = disk_engine.engine_mut();
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
    }

    // ── Generative population ────────────────────────────────────────

    let mut gen_created = 0u64;
    let mut gen_bytes = 0u64;
    let mut rng = rand::rng();

    for gen_spec in &manifest.generate {
        let label = gen_spec
            .name
            .as_deref()
            .unwrap_or(&gen_spec.content_type);

        let (default_min, default_max) = default_size_range(&gen_spec.content_type);
        let min_size = gen_spec.min_size.unwrap_or(default_min);
        let max_size = gen_spec.max_size.unwrap_or(default_max);

        if !cli.quiet {
            println!(
                "generating {count} '{label}' objects ({min}–{max})...",
                count = gen_spec.count,
                min = human_size(min_size),
                max = human_size(max_size),
            );
        }

        for seq in 0..gen_spec.count {
            let size = sample_size(&mut rng, min_size, max_size);
            let content = generate_content(&mut rng, &gen_spec.content_type, size);

            // Create object and write blob through engine
            let engine = disk_engine.engine_mut();
            let oid = match engine.create_object(now_ms()) {
                Ok(oid) => oid,
                Err(e) => {
                    eprintln!("error creating generated object {seq}: {e}");
                    errors += 1;
                    continue;
                }
            };

            // Write blob through transform pipeline (updates record metadata)
            if let Err(e) = engine.write_blob(oid, &content, now_ms()) {
                eprintln!("error writing blob for {oid}: {e}");
                errors += 1;
                continue;
            }

            // Store plaintext blob for FUSE access
            disk_engine.store_blob(oid.raw_value(), content.clone());

            // Apply tags
            {
                let engine = disk_engine.engine_mut();
                for tag_name in &gen_spec.tags {
                    match engine.dag.lookup(tag_name) {
                        Some(tag_id) => {
                            if let Err(e) = engine.add_tag(oid, tag_id, now_ms()) {
                                eprintln!("error tagging {oid} with '{tag_name}': {e}");
                                errors += 1;
                            } else {
                                tagged += 1;
                            }
                        }
                        None => {
                            eprintln!("error: unknown tag '{tag_name}' in generator '{label}'");
                            errors += 1;
                        }
                    }
                }

                // Apply template attributes
                for (key, toml_val) in &gen_spec.attrs {
                    let tag_id = match engine.dag.lookup(key) {
                        Some(id) => id,
                        None => {
                            eprintln!("error: unknown attribute '{key}' in generator '{label}'");
                            errors += 1;
                            continue;
                        }
                    };

                    let value = match toml_val {
                        toml::Value::String(s) => {
                            Value::Text(substitute(s, seq, &gen_spec.content_type))
                        }
                        other => toml_to_value(other),
                    };

                    if let Err(e) = engine.set_attr(oid, tag_id, value, now_ms()) {
                        eprintln!("error setting {key} on {oid}: {e}");
                        errors += 1;
                    } else {
                        attrs_set += 1;
                    }
                }
            }

            gen_created += 1;
            gen_bytes += size as u64;
            created += 1;

            if !cli.quiet && (seq + 1) % 100 == 0 {
                println!("  ... {}/{} ({} so far)", seq + 1, gen_spec.count, human_size(gen_bytes));
            }
        }

        if !cli.quiet {
            println!(
                "  {label}: {gen_created} objects, {} content",
                human_size(gen_bytes),
            );
        }
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
    if gen_bytes > 0 {
        println!("  generated content: {}", human_size(gen_bytes));
    }
    if ont_tags > 0 || ont_impls > 0 {
        println!("  ontology: {ont_tags} tag(s), {ont_impls} implication(s)");
    }
    if errors > 0 {
        eprintln!("  {errors} error(s)");
        std::process::exit(1);
    }
}

// ── Helpers ──────────────────────────────────────────────────────────

fn toml_to_value(v: &toml::Value) -> Value {
    match v {
        toml::Value::Integer(n) => Value::Int(*n),
        toml::Value::Float(f) => Value::Float(*f),
        toml::Value::String(s) => Value::Text(s.clone()),
        toml::Value::Boolean(b) => Value::Int(if *b { 1 } else { 0 }),
        other => Value::Text(other.to_string()),
    }
}

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

fn human_size(bytes: u64) -> String {
    if bytes >= 1024 * 1024 * 1024 {
        format!("{:.1} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    } else if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}
