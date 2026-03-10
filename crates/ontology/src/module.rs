//! Ontology module loader — parses declarative TOML ontology files.
//!
//! Supports the distribution format from the design doc (section 4.2):
//!
//! ```toml
//! [module]
//! id = "systems.metta.music"
//! version = "2.1.0"
//! name = "Music Ontology"
//!
//! [[tags]]
//! name = "artist"
//! semantics = "attribute"
//! value_type = "text"
//!
//! [[implications]]
//! from = "rock"
//! to = "genre"
//! ```

use serde::Deserialize;

use crate::{
    ImplicationDag,
    error::OntologyError,
    tag_def::{TagDefinition, TagSemantics, ValueType},
};

/// A parsed ontology module — ready to be installed into an ImplicationDag.
#[derive(Debug, Clone)]
pub struct OntologyModule {
    pub id: Option<String>,
    pub version: Option<String>,
    pub name: Option<String>,
    pub tags: Vec<TagDefinition>,
    pub implications: Vec<(String, String)>,
}

// -- TOML schema (serde) --

#[derive(Deserialize)]
struct TomlModule {
    module: Option<TomlModuleHeader>,
    #[serde(default)]
    tags: Vec<TomlTag>,
    #[serde(default)]
    implications: Vec<TomlImplication>,
}

#[derive(Deserialize)]
struct TomlModuleHeader {
    id: Option<String>,
    version: Option<String>,
    name: Option<String>,
}

#[derive(Deserialize)]
struct TomlTag {
    name: String,
    semantics: String,
    #[serde(default)]
    value_type: Option<String>,
    #[serde(default)]
    _element_constraint: Option<String>,
}

#[derive(Deserialize)]
struct TomlImplication {
    from: String,
    to: String,
}

impl OntologyModule {
    /// Parse an ontology module from a TOML string.
    pub fn from_toml(toml_str: &str) -> Result<Self, OntologyError> {
        let parsed: TomlModule =
            toml::from_str(toml_str).map_err(|e| OntologyError::ModuleParse(e.to_string()))?;

        let header = parsed.module.unwrap_or(TomlModuleHeader {
            id: None,
            version: None,
            name: None,
        });

        let mut tags = Vec::with_capacity(parsed.tags.len());
        for t in &parsed.tags {
            let semantics = parse_semantics(&t.semantics, t.value_type.as_deref())?;
            // Tag IDs will be allocated during installation, use placeholder 0.
            let def = TagDefinition::new(mimisbrunnr_types::TagId::new(0), &t.name, semantics);
            tags.push(def);
        }

        let implications: Vec<(String, String)> = parsed
            .implications
            .into_iter()
            .map(|i| (i.from, i.to))
            .collect();

        Ok(OntologyModule {
            id: header.id,
            version: header.version,
            name: header.name,
            tags,
            implications,
        })
    }

    /// Parse from a TOML file path.
    pub fn from_file(path: &std::path::Path) -> Result<Self, OntologyError> {
        let content =
            std::fs::read_to_string(path).map_err(|e| OntologyError::ModuleParse(e.to_string()))?;
        Self::from_toml(&content)
    }

    /// Install this module into an ImplicationDag.
    ///
    /// Registers all tags (allocating IDs) and adds all implications.
    /// Returns the number of tags registered and implications added.
    pub fn install(self, dag: &mut ImplicationDag) -> Result<InstallResult, OntologyError> {
        let mut tags_registered = 0u32;
        let mut tags_skipped = 0u32;
        let mut implications_added = 0u32;

        // Phase 1: Register all tags (skip duplicates).
        for def in &self.tags {
            if dag.lookup(&def.name).is_some() {
                tags_skipped += 1;
                continue;
            }
            let id = dag.alloc_tag_id();
            let new_def = TagDefinition::new(id, &def.name, def.semantics.clone());
            dag.register_tag(new_def)?;
            tags_registered += 1;
        }

        // Phase 2: Add implications (all tags must exist by now).
        for (from_name, to_name) in &self.implications {
            let from_id = dag
                .lookup(from_name)
                .ok_or_else(|| OntologyError::UnknownTag(from_name.clone()))?;
            let to_id = dag
                .lookup(to_name)
                .ok_or_else(|| OntologyError::UnknownTag(to_name.clone()))?;
            dag.add_implication(from_id, to_id)?;
            implications_added += 1;
        }

        Ok(InstallResult {
            tags_registered,
            tags_skipped,
            implications_added,
            module_id: self.id,
            module_name: self.name,
        })
    }
}

/// Result of installing an ontology module.
#[derive(Debug)]
pub struct InstallResult {
    pub tags_registered: u32,
    pub tags_skipped: u32,
    pub implications_added: u32,
    pub module_id: Option<String>,
    pub module_name: Option<String>,
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
                        "unknown value type: {other}"
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
            "unknown semantics: {other}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_module() {
        let toml = r#"
[[tags]]
name = "electronic"
semantics = "label"
"#;
        let module = OntologyModule::from_toml(toml).unwrap();
        assert_eq!(module.tags.len(), 1);
        assert_eq!(module.tags[0].name, "electronic");
        assert_eq!(module.tags[0].semantics, TagSemantics::Label);
        assert!(module.implications.is_empty());
    }

    #[test]
    fn parse_with_header() {
        let toml = r#"
[module]
id = "systems.metta.music"
version = "2.1.0"
name = "Music Ontology"

[[tags]]
name = "artist"
semantics = "attribute"
value_type = "text"

[[implications]]
from = "rock"
to = "genre"
"#;
        let module = OntologyModule::from_toml(toml).unwrap();
        assert_eq!(module.id.as_deref(), Some("systems.metta.music"));
        assert_eq!(module.version.as_deref(), Some("2.1.0"));
        assert_eq!(module.name.as_deref(), Some("Music Ontology"));
        assert_eq!(module.tags.len(), 1);
        assert_eq!(module.implications.len(), 1);
        assert_eq!(module.implications[0], ("rock".into(), "genre".into()));
    }

    #[test]
    fn parse_attribute_types() {
        let toml = r#"
[[tags]]
name = "year"
semantics = "attribute"
value_type = "int"

[[tags]]
name = "score"
semantics = "attribute"
value_type = "float"

[[tags]]
name = "data"
semantics = "attribute"
value_type = "blob"
"#;
        let module = OntologyModule::from_toml(toml).unwrap();
        assert_eq!(module.tags.len(), 3);
        assert!(matches!(
            module.tags[0].semantics,
            TagSemantics::Attribute {
                value_type: ValueType::Int
            }
        ));
        assert!(matches!(
            module.tags[1].semantics,
            TagSemantics::Attribute {
                value_type: ValueType::Float
            }
        ));
        assert!(matches!(
            module.tags[2].semantics,
            TagSemantics::Attribute {
                value_type: ValueType::Blob
            }
        ));
    }

    #[test]
    fn parse_all_semantics() {
        let toml = r#"
[[tags]]
name = "t1"
semantics = "label"

[[tags]]
name = "t2"
semantics = "grouping"

[[tags]]
name = "t3"
semantics = "ordered-collection"

[[tags]]
name = "t4"
semantics = "hierarchical"
"#;
        let module = OntologyModule::from_toml(toml).unwrap();
        assert_eq!(module.tags[0].semantics, TagSemantics::Label);
        assert_eq!(module.tags[1].semantics, TagSemantics::Grouping);
        assert!(matches!(
            module.tags[2].semantics,
            TagSemantics::OrderedCollection { .. }
        ));
        assert_eq!(module.tags[3].semantics, TagSemantics::Hierarchical);
    }

    #[test]
    fn parse_error_unknown_semantics() {
        let toml = r#"
[[tags]]
name = "bad"
semantics = "quantum"
"#;
        let result = OntologyModule::from_toml(toml);
        assert!(result.is_err());
    }

    #[test]
    fn install_into_dag() {
        let toml = r#"
[[tags]]
name = "electronic"
semantics = "label"

[[tags]]
name = "ambient"
semantics = "label"

[[tags]]
name = "media"
semantics = "label"

[[tags]]
name = "artist"
semantics = "attribute"
value_type = "text"

[[implications]]
from = "electronic"
to = "media"

[[implications]]
from = "ambient"
to = "electronic"
"#;
        let module = OntologyModule::from_toml(toml).unwrap();
        let mut dag = ImplicationDag::new();
        let result = module.install(&mut dag).unwrap();

        assert_eq!(result.tags_registered, 4);
        assert_eq!(result.tags_skipped, 0);
        assert_eq!(result.implications_added, 2);

        // Verify tags exist
        assert!(dag.lookup("electronic").is_some());
        assert!(dag.lookup("ambient").is_some());
        assert!(dag.lookup("media").is_some());
        assert!(dag.lookup("artist").is_some());

        // Verify implications
        let electronic = dag.lookup("electronic").unwrap();
        let media = dag.lookup("media").unwrap();
        let implies = dag.direct_implies(electronic);
        assert!(implies.contains(&media));
    }

    #[test]
    fn install_skips_existing_tags() {
        let toml = r#"
[[tags]]
name = "existing"
semantics = "label"

[[tags]]
name = "new"
semantics = "label"
"#;
        let module = OntologyModule::from_toml(toml).unwrap();
        let mut dag = ImplicationDag::new();

        // Pre-register "existing"
        let id = dag.alloc_tag_id();
        dag.register_tag(TagDefinition::new(id, "existing", TagSemantics::Label))
            .unwrap();

        let result = module.install(&mut dag).unwrap();
        assert_eq!(result.tags_registered, 1);
        assert_eq!(result.tags_skipped, 1);
    }

    #[test]
    fn install_error_on_unknown_implication_target() {
        let toml = r#"
[[tags]]
name = "a"
semantics = "label"

[[implications]]
from = "a"
to = "nonexistent"
"#;
        let module = OntologyModule::from_toml(toml).unwrap();
        let mut dag = ImplicationDag::new();
        let result = module.install(&mut dag);
        assert!(result.is_err());
    }

    #[test]
    fn parse_demo_labels_format() {
        let toml = r#"
[[tags]]
name = "electronic"
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
        let module = OntologyModule::from_toml(toml).unwrap();
        assert_eq!(module.tags.len(), 3);
        assert_eq!(module.implications.len(), 1);
    }
}
