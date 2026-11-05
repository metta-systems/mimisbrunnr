//! Manifest mode of `populate`: install ontology modules, create explicit
//! objects and generate synthetic content objects described by a TOML file.
//!
//! ```toml
//! ontology = ["demo-labels.toml"]          # paths relative to the manifest
//!
//! [[objects]]
//! tags = ["electronic", "ambient"]
//! [objects.attrs]
//! artist = "Aphex Twin"
//! year = 1992
//!
//! [[generate]]
//! name = "music tracks"
//! count = 3
//! content_type = "music"                    # music | video | text | binary
//! min_size = 10240
//! max_size = 51200
//! tags = ["electronic"]
//! [generate.attrs]
//! artist = "Synth Artist {seq}"             # {seq} and {type} are substituted
//! ```
//!
//! Every tag and attribute name must already be known to the ontology (from
//! the pool or the manifest's `ontology` modules); unknown names are errors.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use {
    rand::{RngExt, SeedableRng, rngs::StdRng},
    serde::Deserialize,
};

use mimisbrunnr::{
    engine::{DiskEngine, EngineError},
    ontology::OntologyError,
    types::{ObjectId, TagId, Value},
};

use crate::commands::install_ontology;

/// A population manifest.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// Ontology module files to install before creating objects, relative to
    /// the manifest's directory.
    #[serde(default)]
    pub ontology: Vec<PathBuf>,
    /// Objects to create one by one.
    #[serde(default)]
    pub objects: Vec<ObjectSpec>,
    /// Generators for objects with synthetic blob content.
    #[serde(default)]
    pub generate: Vec<GenerateSpec>,
}

/// One explicit object.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObjectSpec {
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub attrs: BTreeMap<String, toml::Value>,
}

/// A batch of generated objects sharing tags and attribute templates.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenerateSpec {
    /// Label used in progress output.
    pub name: Option<String>,
    /// Number of objects to generate.
    pub count: u64,
    /// `music`, `video`, `text` or `binary` (default).
    #[serde(default = "default_content_type")]
    pub content_type: String,
    /// Minimum content size in bytes; defaults per content type.
    pub min_size: Option<u64>,
    /// Maximum content size in bytes; defaults per content type.
    pub max_size: Option<u64>,
    #[serde(default)]
    pub tags: Vec<String>,
    /// Attribute templates; string values may contain `{seq}` and `{type}`.
    #[serde(default)]
    pub attrs: BTreeMap<String, toml::Value>,
}

fn default_content_type() -> String {
    "binary".to_string()
}

/// Totals of a manifest run.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ManifestReport {
    pub modules_installed: u64,
    pub objects: u64,
    pub tags_applied: u64,
    pub attrs_set: u64,
    pub blob_bytes: u64,
}

/// Parse a manifest from TOML text.
pub fn parse_manifest(text: &str) -> Result<Manifest, String> {
    toml::from_str(text).map_err(|error| format!("bad manifest: {error}"))
}

/// Run `manifest_path` against the pool at `pool_path` and commit.
/// Per-object lines go to stdout unless `quiet`.
pub fn run_manifest(
    pool_path: &Path,
    manifest_path: &Path,
    seed: u64,
    quiet: bool,
) -> Result<ManifestReport, String> {
    let text = std::fs::read_to_string(manifest_path)
        .map_err(|error| format!("cannot read {}: {error}", manifest_path.display()))?;
    let manifest = parse_manifest(&text)?;
    let manifest_directory = manifest_path.parent().unwrap_or(Path::new("."));

    let mut engine = DiskEngine::open(pool_path)
        .map_err(|error| format!("cannot open pool {}: {error}", pool_path.display()))?;
    let mut report = ManifestReport::default();

    for module_path in &manifest.ontology {
        let resolved = manifest_directory.join(module_path);
        match install_ontology(&mut engine, &resolved) {
            Ok(()) => {
                report.modules_installed += 1;
                if !quiet {
                    println!("ontology {}", resolved.display());
                }
            }
            // The module is already in the pool (e.g. installed by
            // `mimir ontology install`): its tags are available, nothing to do.
            Err(EngineError::Ontology(OntologyError::ModuleAlreadyInstalled(module_id))) => {
                if !quiet {
                    println!("ontology {} (already installed as {module_id})", resolved.display());
                }
            }
            Err(error) => return Err(format!("ontology {}: {error}", resolved.display())),
        }
    }

    for spec in &manifest.objects {
        let oid = create_with(&mut engine, &spec.tags, &spec.attrs, &mut report, |value| {
            toml_to_value(value)
        })?;
        if !quiet {
            println!("{oid} {}", describe(&spec.tags, &spec.attrs));
        }
    }

    let mut rng = StdRng::seed_from_u64(seed);
    for generator in &manifest.generate {
        let label = generator.name.as_deref().unwrap_or(&generator.content_type);
        let (default_min, default_max) = default_size_range(&generator.content_type);
        let min_size = generator.min_size.unwrap_or(default_min);
        let max_size = generator.max_size.unwrap_or(default_max);
        let mut generated_bytes = 0u64;
        for sequence in 0..generator.count {
            let oid = create_with(
                &mut engine,
                &generator.tags,
                &generator.attrs,
                &mut report,
                |value| match value {
                    toml::Value::String(text) => {
                        Value::Text(substitute(text, sequence, &generator.content_type))
                    }
                    other => toml_to_value(other),
                },
            )?;
            let size = sample_size(&mut rng, min_size, max_size);
            let content = generate_content(&mut rng, &generator.content_type, size);
            engine
                .write_blob(oid, &content)
                .map_err(|error| format!("blob for {oid}: {error}"))?;
            generated_bytes += content.len() as u64;
        }
        report.blob_bytes += generated_bytes;
        if !quiet {
            println!(
                "generated {} '{label}' object(s), {}",
                generator.count,
                human_size(generated_bytes)
            );
        }
    }

    engine.commit().map_err(|error| format!("commit: {error}"))?;
    Ok(report)
}

/// Create one object and apply `tags` and `attrs` (values converted by
/// `to_value`).
fn create_with(
    engine: &mut DiskEngine,
    tags: &[String],
    attrs: &BTreeMap<String, toml::Value>,
    report: &mut ManifestReport,
    to_value: impl Fn(&toml::Value) -> Value,
) -> Result<ObjectId, String> {
    let oid = engine
        .create_object()
        .map_err(|error| format!("create object: {error}"))?;
    report.objects += 1;
    for tag_name in tags {
        let tag = resolve(engine, tag_name)?;
        engine
            .add_tag(oid, tag)
            .map_err(|error| format!("tag {oid} {tag_name}: {error}"))?;
        report.tags_applied += 1;
    }
    for (key, value) in attrs {
        let key_tag = resolve(engine, key)?;
        engine
            .set_attr(oid, key_tag, to_value(value))
            .map_err(|error| format!("set {oid} {key}: {error}"))?;
        report.attrs_set += 1;
    }
    Ok(oid)
}

fn resolve(engine: &DiskEngine, name: &str) -> Result<TagId, String> {
    engine
        .engine
        .resolve_tag_name(name)
        .ok_or_else(|| format!("unknown tag '{name}' (install an ontology module defining it)"))
}

fn describe(tags: &[String], attrs: &BTreeMap<String, toml::Value>) -> String {
    let mut parts: Vec<String> = tags.iter().map(|tag| format!("+{tag}")).collect();
    parts.extend(attrs.iter().map(|(key, value)| format!("{key}={value}")));
    parts.join(" ")
}

fn toml_to_value(value: &toml::Value) -> Value {
    match value {
        toml::Value::Integer(number) => Value::Int(*number),
        toml::Value::Float(number) => Value::Float(*number),
        toml::Value::String(text) => Value::Text(text.clone()),
        toml::Value::Boolean(flag) => Value::Int(i64::from(*flag)),
        other => Value::Text(other.to_string()),
    }
}

fn substitute(template: &str, sequence: u64, content_type: &str) -> String {
    template
        .replace("{seq}", &sequence.to_string())
        .replace("{type}", content_type)
}

// ── Content generators ────────────────────────────────────────────────

/// Sample a size log-uniformly between `min` and `max`.
fn sample_size(rng: &mut StdRng, min: u64, max: u64) -> usize {
    if min >= max {
        return min as usize;
    }
    let log_size = rng.random_range((min as f64).ln()..(max as f64).ln());
    log_size.exp() as usize
}

/// Default `(min, max)` size for a content type.
fn default_size_range(content_type: &str) -> (u64, u64) {
    match content_type {
        "music" => (1024 * 1024, 15 * 1024 * 1024),
        "video" => (100 * 1024 * 1024, 1024 * 1024 * 1024),
        "text" => (1024, 15 * 1024),
        _ => (1024, 1024 * 1024),
    }
}

fn generate_content(rng: &mut StdRng, content_type: &str, size: usize) -> Vec<u8> {
    match content_type {
        "text" => generate_text(rng, size),
        "music" => generate_music(rng, size),
        "video" => generate_video(rng, size),
        _ => generate_binary(rng, size),
    }
}

const WORDS: &[&str] = &[
    "the", "be", "to", "of", "and", "a", "in", "that", "have", "it", "for", "not", "on", "with",
    "as", "you", "do", "at", "this", "but", "by", "from", "they", "we", "say", "or", "an", "will",
    "one", "all", "would", "there", "what", "so", "up", "out", "if", "about", "who", "get",
    "which", "go", "when", "make", "can", "like", "time", "no", "just", "know", "take", "people",
    "into", "year", "good", "some", "could", "see", "other", "than", "then", "now", "look", "only",
    "come", "over", "think", "also", "back", "after", "use", "two", "how", "work", "first", "well",
    "way", "even", "new", "want", "because", "any", "these", "give", "day", "most", "great",
    "between", "need", "large", "home", "still", "long", "music", "sound", "wave", "signal",
    "frequency", "amplitude", "rhythm", "melody", "harmony", "chord", "note", "beat", "tempo",
    "pitch", "tone", "resonance", "spectrum", "filter", "oscillator", "synthesizer", "sampler",
    "sequencer", "analog", "digital", "modular", "ambient", "electronic", "acoustic",
];

/// Sentences of 8–20 words in paragraphs of 3–7 sentences.
fn generate_text(rng: &mut StdRng, size: usize) -> Vec<u8> {
    let mut text = String::with_capacity(size);
    let mut words_in_sentence = 0;
    let mut sentences_in_paragraph = 0;
    while text.len() < size {
        let word = WORDS[rng.random_range(0..WORDS.len())];
        if words_in_sentence == 0 {
            let mut characters = word.chars();
            if let Some(first) = characters.next() {
                text.extend(first.to_uppercase());
                text.extend(characters);
            }
        } else {
            text.push(' ');
            text.push_str(word);
        }
        words_in_sentence += 1;
        if words_in_sentence >= rng.random_range(8..20) {
            text.push_str(". ");
            words_in_sentence = 0;
            sentences_in_paragraph += 1;
            if sentences_in_paragraph >= rng.random_range(3..7) {
                text.push_str("\n\n");
                sentences_in_paragraph = 0;
            }
        }
    }
    text.truncate(size);
    text.into_bytes()
}

/// Repeating 1 KiB frames with ~5 % mutations: moderately compressible.
fn generate_music(rng: &mut StdRng, size: usize) -> Vec<u8> {
    const FRAME_SIZE: usize = 1024;
    let mut content = Vec::with_capacity(size);
    content.extend_from_slice(b"MBRMUSIC");
    content.extend_from_slice(&44_100u32.to_le_bytes());
    content.extend_from_slice(&2u16.to_le_bytes());
    let mut base_frame = vec![0u8; FRAME_SIZE];
    rng.fill(&mut base_frame[..]);
    while content.len() < size {
        if rng.random_range(0..16u32) == 0 {
            rng.fill(&mut base_frame[..]);
        }
        let write_length = (size - content.len()).min(FRAME_SIZE);
        let start = content.len();
        content.extend_from_slice(&base_frame[..write_length]);
        for _ in 0..write_length / 20 {
            let position = start + rng.random_range(0..write_length);
            content[position] = content[position].wrapping_add(rng.random_range(0..32u8));
        }
    }
    content.truncate(size);
    content
}

/// Random keyframes every 30 frames, 10 %-delta frames between them.
fn generate_video(rng: &mut StdRng, size: usize) -> Vec<u8> {
    const FRAME_SIZE: usize = 4096;
    let mut content = Vec::with_capacity(size);
    content.extend_from_slice(b"MBRVIDEO");
    content.extend_from_slice(&1920u16.to_le_bytes());
    content.extend_from_slice(&1080u16.to_le_bytes());
    let mut previous_frame = vec![0u8; FRAME_SIZE];
    rng.fill(&mut previous_frame[..]);
    let mut frame_number = 0u32;
    while content.len() < size {
        let data_length = (size - content.len()).saturating_sub(8).min(FRAME_SIZE);
        let keyframe = frame_number.is_multiple_of(30);
        content.push(if keyframe { b'I' } else { b'P' });
        content.extend_from_slice(&frame_number.to_le_bytes());
        content.extend_from_slice(&[0, 0, 0]);
        if keyframe {
            rng.fill(&mut previous_frame[..data_length]);
        } else if data_length > 0 {
            for _ in 0..data_length / 10 {
                let position = rng.random_range(0..data_length);
                previous_frame[position] = rng.random::<u8>();
            }
        }
        content.extend_from_slice(&previous_frame[..data_length]);
        frame_number += 1;
    }
    content.truncate(size);
    content
}

/// Incompressible random bytes.
fn generate_binary(rng: &mut StdRng, size: usize) -> Vec<u8> {
    let mut content = vec![0u8; size];
    rng.fill(&mut content[..]);
    content
}

fn human_size(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    let bytes_float = bytes as f64;
    if bytes_float >= KIB * KIB * KIB {
        format!("{:.1} GiB", bytes_float / (KIB * KIB * KIB))
    } else if bytes_float >= KIB * KIB {
        format!("{:.1} MiB", bytes_float / (KIB * KIB))
    } else if bytes_float >= KIB {
        format!("{:.1} KiB", bytes_float / KIB)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use mimisbrunnr::{
        pool::{DiskConfigEntry, PoolConfig},
        types::{MediaType, Query, StorageTier},
    };
    use tempfile::TempDir;

    fn make_pool(directory: &TempDir) -> PathBuf {
        let config = PoolConfig {
            node_id: 0,
            disks: vec![DiskConfigEntry {
                id: 0,
                path: directory.path().join("disk0.img"),
                media_type: MediaType::NVMe,
                tier: StorageTier::Hot,
                capacity_bytes: 32 * 1024 * 1024,
            }],
        };
        let config_path = directory.path().join("pool.toml");
        DiskEngine::create(config, config_path.clone()).unwrap();
        config_path
    }

    const MODULE: &str = r#"
[module]
id = "test.music"
version = "0.1.0"
name = "test"

[[tags]]
name = "electronic"
semantics = "label"

[[tags]]
name = "media"
semantics = "label"

[[tags]]
name = "artist"
semantics = "attribute"
value_type = "text"

[[tags]]
name = "year"
semantics = "attribute"
value_type = "int"

[[implications]]
from = "electronic"
to = "media"
"#;

    const MANIFEST: &str = r#"
ontology = ["module.toml"]

[[objects]]
tags = ["electronic"]
[objects.attrs]
artist = "Aphex Twin"
year = 1992

[[objects]]
tags = ["media"]

[[generate]]
count = 2
content_type = "text"
min_size = 100
max_size = 200
tags = ["electronic"]
[generate.attrs]
artist = "Artist {seq}"
"#;

    #[test]
    fn manifest_installs_ontology_creates_objects_and_survives_reopen() {
        let directory = TempDir::new().unwrap();
        let pool = make_pool(&directory);
        std::fs::write(directory.path().join("module.toml"), MODULE).unwrap();
        let manifest_path = directory.path().join("manifest.toml");
        std::fs::write(&manifest_path, MANIFEST).unwrap();

        let report = run_manifest(&pool, &manifest_path, 7, true).unwrap();
        assert_eq!(
            report,
            ManifestReport {
                modules_installed: 1,
                objects: 4,
                tags_applied: 4,
                attrs_set: 4,
                blob_bytes: report.blob_bytes,
            }
        );
        assert!((200..=400).contains(&report.blob_bytes));

        let reopened = DiskEngine::open(&pool).unwrap();
        let electronic = reopened.engine.resolve_tag_name("electronic").unwrap();
        let artist = reopened.engine.resolve_tag_name("artist").unwrap();
        let year = reopened.engine.resolve_tag_name("year").unwrap();
        let media = reopened.engine.resolve_tag_name("media").unwrap();
        assert_eq!(reopened.engine.query_full(&Query::HasTag(electronic)).unwrap().len(), 3);
        // Implication electronic → media plus the explicit media object.
        assert_eq!(reopened.engine.query_full(&Query::IsA(media)).unwrap().len(), 4);
        let aphex = Query::HasAttr {
            key: artist,
            op: mimisbrunnr::types::CmpOp::Eq,
            value: Value::Text("Aphex Twin".into()),
        };
        assert_eq!(reopened.engine.query_full(&aphex).unwrap().len(), 1);
        let year_1992 = Query::HasAttr {
            key: year,
            op: mimisbrunnr::types::CmpOp::Eq,
            value: Value::Int(1992),
        };
        assert_eq!(reopened.engine.query_full(&year_1992).unwrap().len(), 1);
    }

    #[test]
    fn manifest_skips_an_already_installed_module() {
        let directory = TempDir::new().unwrap();
        let pool = make_pool(&directory);
        std::fs::write(directory.path().join("module.toml"), MODULE).unwrap();
        let manifest_path = directory.path().join("manifest.toml");
        std::fs::write(&manifest_path, MANIFEST).unwrap();

        let first = run_manifest(&pool, &manifest_path, 7, true).unwrap();
        let second = run_manifest(&pool, &manifest_path, 7, true).unwrap();
        assert_eq!(first.modules_installed, 1);
        assert_eq!(second.modules_installed, 0);
        assert_eq!(second.objects, 4);
    }

    #[test]
    fn unknown_tag_is_an_error() {
        let directory = TempDir::new().unwrap();
        let pool = make_pool(&directory);
        let manifest_path = directory.path().join("manifest.toml");
        std::fs::write(&manifest_path, "[[objects]]\ntags = [\"nope\"]\n").unwrap();
        let error = run_manifest(&pool, &manifest_path, 0, true).unwrap_err();
        assert!(error.contains("unknown tag 'nope'"), "{error}");
    }

    #[test]
    fn unknown_manifest_keys_are_rejected() {
        let error = parse_manifest("default_compression = \"zstd:3\"\n").unwrap_err();
        assert!(error.contains("default_compression"), "{error}");
    }

    #[test]
    fn generators_produce_requested_sizes() {
        let mut rng = StdRng::seed_from_u64(1);
        for content_type in ["text", "music", "video", "binary"] {
            for size in [0, 1, 17, 5000] {
                assert_eq!(generate_content(&mut rng, content_type, size).len(), size);
            }
        }
    }
}
