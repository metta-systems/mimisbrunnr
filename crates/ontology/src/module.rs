//! Ontology module loader / serialiser (DESIGN §4).
//!
//! TOML is the human-edited distribution format; the on-disk form is CBOR
//! (DESIGN §4 + IMPL §10.1). At install time TOML is parsed into
//! [`OntologyModule`]; the engine then converts to CBOR before writing.

use std::collections::BTreeMap;

use mimisbrunnr_types::{
    ChunkParams, ChunkingAlgo, CompressionAlgo, EncryptionMode, ModuleId, StoragePolicy,
    TagDefinition, TagId, TagRelation, TagSemantics, ValueType,
};
use serde::{Deserialize, Serialize};

use crate::error::OntologyError;

/// In-memory representation of a parsed ontology module.
///
/// Tag IDs in `tags` are placeholders set to `TagId::new(0)` until install
/// time, when [`crate::OntologyState::install`] allocates real IDs through an
/// [`IdAllocator`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OntologyModule {
    pub id: ModuleId,
    pub version: String,
    pub name: String,
    pub tags: Vec<TagDefinition>,
    /// `(from_name, to_name)` pairs. Names are resolved to IDs at install
    /// time; this avoids the TOML having to spell out numeric IDs.
    pub implications: Vec<(String, String)>,
    /// `(from_name, kind, to_name)` triples for the non-implication relations
    /// (`MutuallyExclusive` / `Requires` / `Alias`). `ImpliedBy` belongs in
    /// `implications`; install rejects it here.
    #[serde(default)]
    pub relations: Vec<(String, TagRelation, String)>,
}

/// Result of installing an [`OntologyModule`] into an
/// [`crate::OntologyState`].
#[derive(Debug, Clone, PartialEq)]
pub struct InstallResult {
    pub tags_registered: usize,
    pub tags_skipped: usize,
    pub implications_added: usize,
    pub module_id: ModuleId,
    pub module_name: String,
}

/// Allocates fresh [`TagId`]s during module install. Reused across installs
/// so IDs never collide.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IdAllocator {
    next: u32,
}

impl IdAllocator {
    /// New allocator starting at `1` (id `0` is reserved as the "unallocated"
    /// sentinel).
    pub fn new() -> Self {
        Self { next: 1 }
    }

    /// Allocator that begins issuing IDs at `start`.
    pub fn starting_at(start: u32) -> Self {
        Self {
            next: start.max(1),
        }
    }

    pub fn next_id(&mut self) -> TagId {
        let id = TagId::new(self.next);
        self.next += 1;
        id
    }

    /// Bump the allocator past `id` so future allocations don't collide.
    pub fn observe(&mut self, id: TagId) {
        if id.raw() >= self.next {
            self.next = id.raw() + 1;
        }
    }
}

// -------------------------------------------------------------------------
// TOML schema (serde-shaped wire types).
// -------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TomlModule {
    module: TomlHeader,
    #[serde(default)]
    tags: Vec<TomlTag>,
    #[serde(default)]
    implications: Vec<TomlImplication>,
    #[serde(default)]
    relations: Vec<TomlRelation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TomlHeader {
    id: String,
    version: String,
    name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TomlTag {
    name: String,
    semantics: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    value_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    element_constraint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    storage: Option<TomlStoragePolicy>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct TomlStoragePolicy {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    chunking: Option<TomlChunkParams>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    compression: Option<TomlCompression>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    encryption: Option<TomlEncryption>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TomlChunkParams {
    algo: String,
    min_size: u32,
    avg_size: u32,
    max_size: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
enum TomlCompression {
    Plain(String),
    Levelled { algo: String, level: i32 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
enum TomlEncryption {
    Plain(String),
    Hctr2 { algo: String, object_id: u64 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TomlImplication {
    from: String,
    to: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TomlRelation {
    from: String,
    /// One of: `mutex` / `mutually-exclusive`, `requires`, `alias`.
    /// `implied-by` is rejected here — use `[[implications]]` instead.
    kind: String,
    to: String,
}

// -------------------------------------------------------------------------
// TOML <-> OntologyModule conversions.
// -------------------------------------------------------------------------

impl OntologyModule {
    /// Parse from a TOML string. Tag IDs are left as placeholder zeros.
    pub fn from_toml(s: &str) -> Result<Self, OntologyError> {
        let parsed: TomlModule =
            toml::from_str(s).map_err(|e| OntologyError::ModuleParse(e.to_string()))?;

        // Optional pre-pass: collect a name → placeholder map so an
        // `element_constraint` referenced by name can be turned into a TagId
        // *within the module*. We use sentinel `TagId(0)` placeholders and
        // resolve by name lookup at install time using the names list.
        let name_index: BTreeMap<&str, ()> =
            parsed.tags.iter().map(|t| (t.name.as_str(), ())).collect();

        let mut tags = Vec::with_capacity(parsed.tags.len());
        for t in &parsed.tags {
            let semantics = parse_semantics(&t.semantics, t.value_type.as_deref())?;
            // OrderedCollection.element_constraint references must be
            // resolvable at install time, but we only have names here. Leave
            // as `None` for now — the engine resolves via a second pass on
            // install when a real ID exists. (DESIGN §4 only requires
            // `element_constraint` be optional.)
            let semantics = if let TagSemantics::OrderedCollection { .. } = semantics {
                if let Some(name) = &t.element_constraint {
                    if !name_index.contains_key(name.as_str()) {
                        return Err(OntologyError::UnknownTag(name.clone()));
                    }
                    // Resolved at install time; encode the placeholder.
                    semantics
                } else {
                    semantics
                }
            } else {
                semantics
            };
            let storage = parse_storage(t.storage.as_ref())?;
            tags.push(TagDefinition {
                id: TagId::new(0),
                name: t.name.clone(),
                semantics,
                implies: Vec::new(),
                storage,
            });
        }

        let implications: Vec<(String, String)> = parsed
            .implications
            .into_iter()
            .map(|i| (i.from, i.to))
            .collect();

        let mut relations = Vec::with_capacity(parsed.relations.len());
        for r in parsed.relations {
            let kind = parse_relation_kind(&r.kind)?;
            relations.push((r.from, kind, r.to));
        }

        Ok(OntologyModule {
            id: parsed.module.id,
            version: parsed.module.version,
            name: parsed.module.name,
            tags,
            implications,
            relations,
        })
    }

    /// Serialise back to TOML. The `id` field on each [`TagDefinition`] is
    /// dropped (TOML uses names, not IDs).
    pub fn to_toml(&self) -> Result<String, OntologyError> {
        let header = TomlHeader {
            id: self.id.clone(),
            version: self.version.clone(),
            name: self.name.clone(),
        };
        let tags: Vec<TomlTag> = self
            .tags
            .iter()
            .map(|t| {
                let (semantics, value_type, element_constraint) =
                    encode_semantics(&t.semantics, &self.tags);
                TomlTag {
                    name: t.name.clone(),
                    semantics,
                    value_type,
                    element_constraint,
                    storage: encode_storage(t.storage.as_ref()),
                }
            })
            .collect();
        let implications = self
            .implications
            .iter()
            .map(|(f, t)| TomlImplication {
                from: f.clone(),
                to: t.clone(),
            })
            .collect();

        let relations = self
            .relations
            .iter()
            .map(|(f, kind, t)| TomlRelation {
                from: f.clone(),
                kind: relation_kind_to_str(*kind).into(),
                to: t.clone(),
            })
            .collect();

        let module = TomlModule {
            module: header,
            tags,
            implications,
            relations,
        };
        toml::to_string_pretty(&module).map_err(|e| OntologyError::ModuleSerialise(e.to_string()))
    }
}

fn parse_semantics(s: &str, value_type: Option<&str>) -> Result<TagSemantics, OntologyError> {
    match s {
        "label" => Ok(TagSemantics::Label),
        "attribute" | "attr" => {
            let vt = match value_type.unwrap_or("text") {
                "text" => ValueType::Text,
                "int" => ValueType::Int,
                "float" => ValueType::Float,
                "timestamp" => ValueType::Timestamp,
                "blob" => ValueType::Blob,
                other => {
                    return Err(OntologyError::ModuleParse(format!(
                        "unknown value type `{other}`"
                    )));
                }
            };
            Ok(TagSemantics::Attribute { value_type: vt })
        }
        "grouping" => Ok(TagSemantics::Grouping),
        "ordered-collection" | "ordered" => Ok(TagSemantics::OrderedCollection {
            element_constraint: None,
        }),
        "hierarchical" => Ok(TagSemantics::Hierarchical),
        other => Err(OntologyError::ModuleParse(format!(
            "unknown semantics `{other}`"
        ))),
    }
}

fn encode_semantics(
    sem: &TagSemantics,
    _all_tags: &[TagDefinition],
) -> (String, Option<String>, Option<String>) {
    match sem {
        TagSemantics::Label => ("label".into(), None, None),
        TagSemantics::Attribute { value_type } => {
            let vt = match value_type {
                ValueType::Text => "text",
                ValueType::Int => "int",
                ValueType::Float => "float",
                ValueType::Timestamp => "timestamp",
                ValueType::Blob => "blob",
            };
            ("attribute".into(), Some(vt.into()), None)
        }
        TagSemantics::Grouping => ("grouping".into(), None, None),
        TagSemantics::OrderedCollection { .. } => ("ordered-collection".into(), None, None),
        TagSemantics::Hierarchical => ("hierarchical".into(), None, None),
    }
}

fn parse_relation_kind(s: &str) -> Result<TagRelation, OntologyError> {
    match s {
        "mutex" | "mutually-exclusive" => Ok(TagRelation::MutuallyExclusive),
        "requires" => Ok(TagRelation::Requires),
        "alias" => Ok(TagRelation::Alias),
        "implies" | "implied-by" | "is-a" => Err(OntologyError::ModuleParse(format!(
            "use [[implications]] for relation kind `{s}`"
        ))),
        other => Err(OntologyError::ModuleParse(format!(
            "unknown relation kind `{other}`"
        ))),
    }
}

fn relation_kind_to_str(kind: TagRelation) -> &'static str {
    match kind {
        TagRelation::MutuallyExclusive => "mutually-exclusive",
        TagRelation::Requires => "requires",
        TagRelation::Alias => "alias",
        // ImpliedBy doesn't appear in the TOML relations list (it lives in
        // [[implications]]), but be defensive: emit the most common spelling
        // so a round-trip via `to_toml` followed by `from_toml` surfaces a
        // clear "use [[implications]]" error rather than a silent drop.
        TagRelation::ImpliedBy => "implied-by",
    }
}

fn parse_storage(s: Option<&TomlStoragePolicy>) -> Result<Option<StoragePolicy>, OntologyError> {
    let Some(s) = s else {
        return Ok(None);
    };
    let chunking = match &s.chunking {
        None => None,
        Some(c) => {
            let algo = match c.algo.as_str() {
                "none" => ChunkingAlgo::None,
                "fixed" | "fixed-size" => ChunkingAlgo::FixedSize,
                "fastcdc" | "cdc" => ChunkingAlgo::FastCDC,
                other => {
                    return Err(OntologyError::ModuleParse(format!(
                        "unknown chunking algo `{other}`"
                    )));
                }
            };
            Some(ChunkParams {
                algo,
                min_size: c.min_size,
                avg_size: c.avg_size,
                max_size: c.max_size,
            })
        }
    };
    let compression = match &s.compression {
        None => None,
        Some(TomlCompression::Plain(name)) => Some(match name.as_str() {
            "none" => CompressionAlgo::None,
            "lz4" => CompressionAlgo::Lz4,
            "zstd" => CompressionAlgo::Zstd(3),
            other => {
                return Err(OntologyError::ModuleParse(format!(
                    "unknown compression algo `{other}`"
                )));
            }
        }),
        Some(TomlCompression::Levelled { algo, level }) => Some(match algo.as_str() {
            "zstd" => CompressionAlgo::Zstd(*level),
            other => {
                return Err(OntologyError::ModuleParse(format!(
                    "compression algo `{other}` does not take a level"
                )));
            }
        }),
    };
    let encryption = match &s.encryption {
        None => None,
        Some(TomlEncryption::Plain(name)) => Some(match name.as_str() {
            "none" => EncryptionMode::None,
            "xts" => EncryptionMode::Xts,
            "aes-gcm" | "gcm" => EncryptionMode::AesGcm,
            "chacha20" | "chacha20-poly1305" => EncryptionMode::ChaCha20Poly1305,
            other => {
                return Err(OntologyError::ModuleParse(format!(
                    "unknown encryption mode `{other}`"
                )));
            }
        }),
        Some(TomlEncryption::Hctr2 { algo, object_id }) => Some(match algo.as_str() {
            "hctr2" => EncryptionMode::Hctr2 {
                object_id: *object_id,
            },
            other => {
                return Err(OntologyError::ModuleParse(format!(
                    "encryption mode `{other}` does not take an object_id"
                )));
            }
        }),
    };
    Ok(Some(StoragePolicy {
        chunking,
        compression,
        encryption,
    }))
}

fn encode_storage(p: Option<&StoragePolicy>) -> Option<TomlStoragePolicy> {
    let p = p?;
    let chunking = p.chunking.map(|c| TomlChunkParams {
        algo: match c.algo {
            ChunkingAlgo::None => "none".into(),
            ChunkingAlgo::FixedSize => "fixed".into(),
            ChunkingAlgo::FastCDC => "fastcdc".into(),
        },
        min_size: c.min_size,
        avg_size: c.avg_size,
        max_size: c.max_size,
    });
    let compression = p.compression.map(|c| match c {
        CompressionAlgo::None => TomlCompression::Plain("none".into()),
        CompressionAlgo::Lz4 => TomlCompression::Plain("lz4".into()),
        CompressionAlgo::Zstd(level) => TomlCompression::Levelled {
            algo: "zstd".into(),
            level,
        },
    });
    let encryption = p.encryption.map(|e| match e {
        EncryptionMode::None => TomlEncryption::Plain("none".into()),
        EncryptionMode::Xts => TomlEncryption::Plain("xts".into()),
        EncryptionMode::AesGcm => TomlEncryption::Plain("aes-gcm".into()),
        EncryptionMode::ChaCha20Poly1305 => TomlEncryption::Plain("chacha20-poly1305".into()),
        EncryptionMode::Hctr2 { object_id } => TomlEncryption::Hctr2 {
            algo: "hctr2".into(),
            object_id,
        },
    });
    Some(TomlStoragePolicy {
        chunking,
        compression,
        encryption,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_allocator_starts_at_one() {
        let mut a = IdAllocator::new();
        assert_eq!(a.next_id().raw(), 1);
        assert_eq!(a.next_id().raw(), 2);
    }

    #[test]
    fn id_allocator_observe_advances() {
        let mut a = IdAllocator::new();
        a.observe(TagId::new(42));
        assert_eq!(a.next_id().raw(), 43);
    }

    #[test]
    fn parse_design_42_example() {
        let src = r#"
[module]
id = "systems.metta.music"
version = "2.1.0"
name = "Music Ontology"

[[tags]]
name = "artist"
semantics = "attribute"
value_type = "text"

[[tags]]
name = "playlist"
semantics = "ordered-collection"

[[tags]]
name = "genre"
semantics = "grouping"

[[implications]]
from = "rock"
to = "genre"

[[implications]]
from = "flac"
to = "audio"
"#;
        let m = OntologyModule::from_toml(src).unwrap();
        assert_eq!(m.id, "systems.metta.music");
        assert_eq!(m.version, "2.1.0");
        assert_eq!(m.tags.len(), 3);
        assert_eq!(m.implications.len(), 2);
        assert_eq!(m.tags[0].name, "artist");
        assert!(matches!(
            m.tags[0].semantics,
            TagSemantics::Attribute {
                value_type: ValueType::Text
            }
        ));
        assert!(matches!(
            m.tags[1].semantics,
            TagSemantics::OrderedCollection { .. }
        ));
        assert_eq!(m.tags[2].semantics, TagSemantics::Grouping);
    }

    #[test]
    fn round_trip_via_toml() {
        let module = OntologyModule {
            id: "test.example".into(),
            version: "0.1.0".into(),
            name: "Test".into(),
            tags: vec![
                TagDefinition {
                    id: TagId::new(0),
                    name: "file".into(),
                    semantics: TagSemantics::Label,
                    implies: vec![],
                    storage: Some(StoragePolicy {
                        chunking: Some(ChunkParams {
                            algo: ChunkingAlgo::FastCDC,
                            min_size: 1024,
                            avg_size: 4096,
                            max_size: 16384,
                        }),
                        compression: Some(CompressionAlgo::Zstd(9)),
                        encryption: Some(EncryptionMode::Xts),
                    }),
                },
                TagDefinition {
                    id: TagId::new(0),
                    name: "binary".into(),
                    semantics: TagSemantics::Label,
                    implies: vec![],
                    storage: None,
                },
            ],
            implications: vec![("binary".into(), "file".into())],
            relations: Vec::new(),
        };
        let s = module.to_toml().unwrap();
        let back = OntologyModule::from_toml(&s).unwrap();
        assert_eq!(module, back);
    }

    #[test]
    fn parse_relations_section() {
        let src = r#"
[module]
id = "x"
version = "0.0.1"
name = "x"

[[tags]]
name = "active"
semantics = "label"

[[tags]]
name = "discontinued"
semantics = "label"

[[tags]]
name = "usb-c"
semantics = "label"

[[tags]]
name = "electronics"
semantics = "label"

[[relations]]
from = "active"
kind = "mutually-exclusive"
to = "discontinued"

[[relations]]
from = "usb-c"
kind = "requires"
to = "electronics"
"#;
        let m = OntologyModule::from_toml(src).unwrap();
        assert_eq!(m.relations.len(), 2);
        assert_eq!(m.relations[0].1, TagRelation::MutuallyExclusive);
        assert_eq!(m.relations[1].1, TagRelation::Requires);
    }

    #[test]
    fn parse_relation_kind_implies_redirects() {
        let src = r#"
[module]
id = "x"
version = "0.0.1"
name = "x"

[[tags]]
name = "a"
semantics = "label"

[[relations]]
from = "a"
kind = "implies"
to = "a"
"#;
        let err = OntologyModule::from_toml(src).unwrap_err();
        let msg = format!("{err:?}");
        assert!(msg.contains("[[implications]]"), "got {msg}");
    }

    #[test]
    fn round_trip_via_toml_with_relations() {
        let module = OntologyModule {
            id: "rel.example".into(),
            version: "0.1.0".into(),
            name: "Rel".into(),
            tags: vec![
                TagDefinition {
                    id: TagId::new(0),
                    name: "active".into(),
                    semantics: TagSemantics::Label,
                    implies: vec![],
                    storage: None,
                },
                TagDefinition {
                    id: TagId::new(0),
                    name: "discontinued".into(),
                    semantics: TagSemantics::Label,
                    implies: vec![],
                    storage: None,
                },
            ],
            implications: vec![],
            relations: vec![(
                "active".into(),
                TagRelation::MutuallyExclusive,
                "discontinued".into(),
            )],
        };
        let s = module.to_toml().unwrap();
        let back = OntologyModule::from_toml(&s).unwrap();
        assert_eq!(module, back);
    }

    #[test]
    fn unknown_semantics_errors() {
        let src = r#"
[module]
id = "x"
version = "0.0.1"
name = "x"

[[tags]]
name = "x"
semantics = "quantum"
"#;
        assert!(OntologyModule::from_toml(src).is_err());
    }

    #[test]
    fn missing_module_header_errors() {
        let src = r#"
[[tags]]
name = "x"
semantics = "label"
"#;
        assert!(OntologyModule::from_toml(src).is_err());
    }
}
