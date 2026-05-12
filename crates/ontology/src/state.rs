//! [`OntologyState`] — the live registry plus install / upgrade / scrub /
//! policy-resolution machinery (DESIGN §3.5, §4.4–§4.6).
//!
//! ## Persistence (IMPL §10.1)
//!
//! The ontology is fully loaded into memory at mount and rewritten as a
//! single coherent [`OntologyImage`] (CBOR) on every checkpoint. On disk
//! the image lives as one sorted-run entry per snapshot inside the §1.5
//! B+ tree region of [`BtreeKind::Ontology`] — `RootPointer.ontology_root`
//! points at this region directly (no 4 KiB envelope block).
//!
//! Updates are batch-shaped (rare; one update touches many tags and
//! relations at once), so the on-disk form is optimised for batch rewrite
//! and compact serialised size — not per-element disk-resident lookup.
//! See `docs/IMPLEMENTATION.md` §10.1 for the rationale.

use std::collections::{BTreeSet, HashMap};

use {
    mimisbrunnr_storage::{BlockDevice, BtreeKind, BtreeRegion, LoadedNode, SortedRun},
    mimisbrunnr_types::{ModuleId, StoragePolicy, TagDefinition, TagId, TagRelation},
    serde::{Deserialize, Serialize},
};

use crate::{
    error::{OntologyError, StorageAxis},
    implication_dag::{DagSnapshot, ImplicationDag},
    materializer::Materializer,
    module::{IdAllocator, InstallResult, OntologyModule},
};

/// 256 KiB region size in bytes (matches the spec's 18-bit
/// `region_size_log2`). Engine code sizes the on-disk slot from this.
#[allow(dead_code)] // consumed by engine layout once R1b lands engine-side.
pub const ONTOLOGY_REGION_SIZE: u64 = 256 * 1024;
const REGION_SIZE_LOG2: u8 = 18;

/// Bookkeeping per installed module: lets us scrub later by knowing which
/// tags / implications / relations this module added.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModuleRecord {
    pub id: ModuleId,
    pub version: String,
    pub name: String,
    /// Tags this module *registered* (not pre-existing, not shared with
    /// another module at the time of install).
    pub installed_tags: Vec<TagId>,
    /// Implications this module added at install time.
    pub installed_implications: Vec<(TagId, TagId)>,
    /// Tag-to-tag relations (mutex / requires / alias) this module added
    /// at install time. `ImpliedBy` relations live in `installed_implications`.
    #[serde(default)]
    pub installed_relations: Vec<(TagId, TagRelation, TagId)>,
}

/// Live ontology registry. Owns the [`ImplicationDag`], the tag tables, the
/// tag-to-tag relation set, and a catalogue of installed modules.
#[derive(Debug, Clone, Default)]
pub struct OntologyState {
    pub tags: HashMap<TagId, TagDefinition>,
    pub names: HashMap<String, TagId>,
    pub dag: ImplicationDag,
    /// Tag-to-tag relations carrying `MutuallyExclusive` / `Requires` /
    /// `Alias` semantics (DESIGN §3.2). `ImpliedBy` relations are folded
    /// into `dag` and `TagDefinition.implies` rather than stored here.
    pub relations: Vec<(TagId, TagRelation, TagId)>,
    pub installed_modules: HashMap<ModuleId, ModuleRecord>,
}

impl OntologyState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Install a module. Performs DESIGN §3.5's static invariant check before
    /// committing — refuses install on conflict, leaving state untouched.
    pub fn install(
        &mut self,
        module: OntologyModule,
        allocator: &mut IdAllocator,
    ) -> Result<InstallResult, OntologyError> {
        if self.installed_modules.contains_key(&module.id) {
            return Err(OntologyError::ModuleAlreadyInstalled(module.id));
        }

        // Snapshot the current state so we can roll back on failure.
        let backup = self.clone();

        match self.install_inner(module, allocator) {
            Ok(res) => Ok(res),
            Err(e) => {
                *self = backup;
                Err(e)
            }
        }
    }

    fn install_inner(
        &mut self,
        module: OntologyModule,
        allocator: &mut IdAllocator,
    ) -> Result<InstallResult, OntologyError> {
        let module_id = module.id.clone();
        let module_name = module.name.clone();
        let module_version = module.version.clone();

        let mut tags_registered = 0usize;
        let mut tags_skipped = 0usize;
        let mut newly_registered_tags: Vec<TagId> = Vec::new();

        // Phase 1: register tags. Skip if name already exists.
        for def in module.tags {
            if let Some(&existing) = self.names.get(&def.name) {
                tags_skipped += 1;
                allocator.observe(existing);
                continue;
            }
            let id = allocator.next_id();
            let mut new_def = def;
            new_def.id = id;
            self.dag.add_tag(id);
            self.names.insert(new_def.name.clone(), id);
            self.tags.insert(id, new_def);
            tags_registered += 1;
            newly_registered_tags.push(id);
        }

        // Phase 2: add implications by name.
        let mut implications_added = 0usize;
        let mut module_implications: Vec<(TagId, TagId)> = Vec::new();
        for (from_name, to_name) in module.implications {
            let from = *self
                .names
                .get(&from_name)
                .ok_or_else(|| OntologyError::UnknownTag(from_name.clone()))?;
            let to = *self
                .names
                .get(&to_name)
                .ok_or_else(|| OntologyError::UnknownTag(to_name.clone()))?;
            self.dag.add_implication(from, to)?;
            module_implications.push((from, to));
            implications_added += 1;
            // Also record on the TagDefinition for symmetry with DESIGN §3.1.
            if let Some(def) = self.tags.get_mut(&from)
                && !def.implies.contains(&to)
            {
                def.implies.push(to);
            }
        }

        // Phase 3: add tag-to-tag relations (mutex / requires / alias).
        // `ImpliedBy` would duplicate the dag edges built in phase 2 and is
        // rejected. References must resolve to known tag names.
        let mut module_relations: Vec<(TagId, TagRelation, TagId)> = Vec::new();
        for (from_name, kind, to_name) in module.relations {
            if matches!(kind, TagRelation::ImpliedBy) {
                return Err(OntologyError::ModuleParse(
                    "use [[implications]] instead of a relation of kind `implied-by`".into(),
                ));
            }
            let from = *self
                .names
                .get(&from_name)
                .ok_or_else(|| OntologyError::UnknownTag(from_name.clone()))?;
            let to = *self
                .names
                .get(&to_name)
                .ok_or_else(|| OntologyError::UnknownTag(to_name.clone()))?;
            let edge = (from, kind, to);
            if !self.relations.contains(&edge) {
                self.relations.push(edge);
            }
            module_relations.push(edge);
        }

        // Phase 4: static invariant — every storage axis must form a chain
        // among its declaring tags.
        self.check_axis_invariant()?;

        let record = ModuleRecord {
            id: module_id.clone(),
            version: module_version,
            name: module_name.clone(),
            installed_tags: newly_registered_tags,
            installed_implications: module_implications,
            installed_relations: module_relations,
        };
        self.installed_modules.insert(module_id.clone(), record);

        Ok(InstallResult {
            tags_registered,
            tags_skipped,
            implications_added,
            module_id,
            module_name,
        })
    }

    /// Upgrade a previously installed module to a new version (DESIGN §4.5).
    /// The diff is "safe-only": new tags and new implications are added.
    /// Removed tags / changed semantics require an explicit migration path
    /// not yet implemented; we error rather than silently destroy data.
    pub fn upgrade(&mut self, new: OntologyModule) -> Result<InstallResult, OntologyError> {
        let prev = self
            .installed_modules
            .get(&new.id)
            .ok_or_else(|| OntologyError::ModuleNotInstalled(new.id.clone()))?
            .clone();

        // Build the set of names registered by the previous module.
        let prev_names: BTreeSet<String> = prev
            .installed_tags
            .iter()
            .filter_map(|id| self.tags.get(id).map(|d| d.name.clone()))
            .collect();
        let new_names: BTreeSet<String> = new.tags.iter().map(|t| t.name.clone()).collect();

        // Refuse on tag removal (would orphan data with no migration path).
        if let Some(name) = prev_names.difference(&new_names).next() {
            return Err(OntologyError::ModuleParse(format!(
                "upgrade would remove tag `{name}`; explicit migration required"
            )));
        }

        // Walk new tags: skip ones already registered with same semantics,
        // error on semantics change.
        for t in &new.tags {
            if let Some(&existing_id) = self.names.get(&t.name) {
                let existing = &self.tags[&existing_id];
                if existing.semantics != t.semantics {
                    return Err(OntologyError::ModuleParse(format!(
                        "upgrade would change semantics of tag `{}`",
                        t.name
                    )));
                }
            }
        }

        // Backup, drop the old record, reinstall the additive parts.
        let backup = self.clone();
        // Remove the record so install() doesn't trip the duplicate check.
        let prev_record = self.installed_modules.remove(&new.id).unwrap();

        // Allocator that picks up where we are.
        let mut allocator = self.fresh_allocator();
        match self.install_inner(new, &mut allocator) {
            Ok(mut res) => {
                // Carry over previously installed_tags so scrub still
                // covers them, plus any newly added.
                let new_record = self.installed_modules.get_mut(&res.module_id).unwrap();
                let mut combined_tags = prev_record.installed_tags.clone();
                for t in &new_record.installed_tags {
                    if !combined_tags.contains(t) {
                        combined_tags.push(*t);
                    }
                }
                let mut combined_imps = prev_record.installed_implications.clone();
                for i in &new_record.installed_implications {
                    if !combined_imps.contains(i) {
                        combined_imps.push(*i);
                    }
                }
                let mut combined_rels = prev_record.installed_relations.clone();
                for r in &new_record.installed_relations {
                    if !combined_rels.contains(r) {
                        combined_rels.push(*r);
                    }
                }
                new_record.installed_tags = combined_tags;
                new_record.installed_implications = combined_imps;
                new_record.installed_relations = combined_rels;
                res.tags_skipped += prev_record.installed_tags.len();
                Ok(res)
            }
            Err(e) => {
                *self = backup;
                Err(e)
            }
        }
    }

    /// Remove a module (DESIGN §4.6 `RemoveUnique` policy). Tags exclusive to
    /// this module are removed; tags also registered by another installed
    /// module are kept. Implications added by this module are removed iff
    /// they aren't reproduced by another installed module's record.
    pub fn scrub(&mut self, module_id: &ModuleId) -> Result<usize, OntologyError> {
        let record = self
            .installed_modules
            .remove(module_id)
            .ok_or_else(|| OntologyError::ModuleNotInstalled(module_id.clone()))?;

        // Tags shared with any other installed module survive.
        let mut shared_tags: BTreeSet<TagId> = BTreeSet::new();
        for other in self.installed_modules.values() {
            shared_tags.extend(other.installed_tags.iter().copied());
        }

        // Implications also added by another module survive.
        let mut shared_imps: BTreeSet<(TagId, TagId)> = BTreeSet::new();
        for other in self.installed_modules.values() {
            shared_imps.extend(other.installed_implications.iter().copied());
        }

        // Relations also added by another module survive.
        let mut shared_rels: BTreeSet<(TagId, TagRelation, TagId)> = BTreeSet::new();
        for other in self.installed_modules.values() {
            shared_rels.extend(other.installed_relations.iter().copied());
        }

        // Remove implications first (so that tag removal doesn't have to
        // touch them via remove_tag's edge cleanup unnecessarily).
        for &(from, to) in &record.installed_implications {
            if !shared_imps.contains(&(from, to)) {
                self.dag.remove_implication(from, to);
                if let Some(def) = self.tags.get_mut(&from) {
                    def.implies.retain(|t| *t != to);
                }
            }
        }

        // Drop relations no longer claimed by any installed module.
        for edge in &record.installed_relations {
            if !shared_rels.contains(edge) {
                self.relations.retain(|e| e != edge);
            }
        }

        let mut removed = 0usize;
        for &tag in &record.installed_tags {
            if shared_tags.contains(&tag) {
                continue;
            }
            if let Some(def) = self.tags.remove(&tag) {
                self.names.remove(&def.name);
            }
            self.dag.remove_tag(tag);
            removed += 1;
        }

        Ok(removed)
    }

    /// Compute the materialised tag set for an object's direct tags.
    pub fn materialise(&self, direct: &[TagId]) -> Vec<TagId> {
        Materializer::new(&self.dag).materialise(direct)
    }

    /// Resolve effective storage policy for an object given its tag set
    /// (DESIGN §3.5). For each axis, picks the unique most-derived tag
    /// declaring it; falls back to `default` per axis.
    pub fn resolve_policy(&self, object_tags: &[TagId], default: &StoragePolicy) -> StoragePolicy {
        let mut chunking = default.chunking;
        let mut compression = default.compression;
        let mut encryption = default.encryption;

        // Materialised set so an axis declared on an ancestor still applies.
        let materialised = self.materialise(object_tags);

        if let Some(t) = self.most_derived(&materialised, |p| p.chunking.is_some()) {
            chunking = self.tags[&t].storage.as_ref().unwrap().chunking;
        }
        if let Some(t) = self.most_derived(&materialised, |p| p.compression.is_some()) {
            compression = self.tags[&t].storage.as_ref().unwrap().compression;
        }
        if let Some(t) = self.most_derived(&materialised, |p| p.encryption.is_some()) {
            encryption = self.tags[&t].storage.as_ref().unwrap().encryption;
        }

        StoragePolicy {
            chunking,
            compression,
            encryption,
        }
    }

    /// Among `candidates`, return the unique tag that declares the axis
    /// (per `axis_set`) and is implied by no other axis-declaring tag in the
    /// set. The static invariant guarantees uniqueness when one exists.
    fn most_derived<F>(&self, candidates: &[TagId], axis_set: F) -> Option<TagId>
    where
        F: Fn(&StoragePolicy) -> bool,
    {
        let declaring: Vec<TagId> = candidates
            .iter()
            .copied()
            .filter(|t| {
                self.tags
                    .get(t)
                    .and_then(|d| d.storage.as_ref())
                    .is_some_and(&axis_set)
            })
            .collect();
        if declaring.is_empty() {
            return None;
        }
        // Most-derived = no other declaring tag is its ancestor (i.e. no
        // other implies it via a path *to* it). We pick the tag that is not
        // implied (transitively) by any *other* declaring tag.
        for &candidate in &declaring {
            let mut dominated = false;
            for &other in &declaring {
                if other == candidate {
                    continue;
                }
                // candidate is implied by other ⇒ candidate is *more*
                // general (an ancestor) than other ⇒ candidate is dominated.
                if self.dag.is_a(other, candidate) {
                    dominated = true;
                    break;
                }
            }
            if !dominated {
                return Some(candidate);
            }
        }
        // Should never happen given the install-time invariant — but pick a
        // deterministic answer anyway.
        declaring.into_iter().min()
    }

    /// DESIGN §3.5 static invariant — for each axis, every pair of
    /// directly-declaring tags must form a chain in the implication DAG.
    fn check_axis_invariant(&self) -> Result<(), OntologyError> {
        type AxisPredicate = fn(&StoragePolicy) -> bool;
        let axes: [(StorageAxis, AxisPredicate); 3] = [
            (StorageAxis::Chunking, |p: &StoragePolicy| {
                p.chunking.is_some()
            }),
            (StorageAxis::Compression, |p: &StoragePolicy| {
                p.compression.is_some()
            }),
            (StorageAxis::Encryption, |p: &StoragePolicy| {
                p.encryption.is_some()
            }),
        ];
        for (axis, predicate) in axes {
            let mut declaring: Vec<TagId> = self
                .tags
                .iter()
                .filter_map(|(id, def)| def.storage.as_ref().filter(|p| predicate(p)).map(|_| *id))
                .collect();
            declaring.sort(); // deterministic conflict reporting
            for (i, &a) in declaring.iter().enumerate() {
                for &b in &declaring[i + 1..] {
                    let related = self.dag.is_a(a, b) || self.dag.is_a(b, a);
                    if !related {
                        let a_name = self.tags[&a].name.clone();
                        let b_name = self.tags[&b].name.clone();
                        return Err(OntologyError::AxisConflict {
                            axis,
                            a,
                            a_name,
                            b,
                            b_name,
                        });
                    }
                }
            }
        }
        Ok(())
    }

    fn fresh_allocator(&self) -> IdAllocator {
        let max = self
            .tags
            .keys()
            .map(|t| t.raw())
            .max()
            .map(|m| m + 1)
            .unwrap_or(1);
        IdAllocator::starting_at(max)
    }

    // ---------------------------------------------------------------------
    // CBOR persistence (IMPL §10.1).
    // ---------------------------------------------------------------------

    /// Serialise to a standalone CBOR(OntologyImage) blob (no §1.5 framing).
    /// Useful for tooling and tests; the on-disk path goes through
    /// [`Self::flush_to_region`].
    pub fn serialise(&self) -> Result<Vec<u8>, OntologyError> {
        let image = self.to_image();
        let mut buf = Vec::new();
        ciborium::ser::into_writer(&image, &mut buf)
            .map_err(|e| OntologyError::Cbor(e.to_string()))?;
        Ok(buf)
    }

    /// Restore a state produced by [`Self::serialise`].
    pub fn deserialise(bytes: &[u8]) -> Result<Self, OntologyError> {
        let image: OntologyImage =
            ciborium::de::from_reader(bytes).map_err(|e| OntologyError::Cbor(e.to_string()))?;
        image.into_state()
    }

    // ----------------------------------------------------------------
    // §1.5 region persistence (IMPL §10.1) — one CBOR(OntologyImage)
    // sorted-run entry per snapshot. R6 will populate non-zero
    // snapshots; today every write goes under `snapshot = 0`.
    // ----------------------------------------------------------------

    /// Build a [`LoadedNode`] containing a single sorted-run entry
    /// `(snapshot = 0, OntologyImage)` for the current state. Uses
    /// [`BtreeKind::Ontology`] and the spec's 18-bit (256 KiB) region
    /// size.
    pub fn to_loaded_node(&self) -> LoadedNode<u32, OntologyImage> {
        let image = self.to_image();
        let entries = vec![(0u32, image)];

        let mut node: LoadedNode<u32, OntologyImage> =
            LoadedNode::new(BtreeKind::Ontology, 0, REGION_SIZE_LOG2);
        let run = SortedRun::from_sorted(0, 0, entries);
        node.sorted_runs.push(run);
        node.header.sorted_run_count = 1;
        node
    }

    /// Restore the in-memory state from a [`LoadedNode`] read via
    /// [`BtreeRegion::read`]. Picks the latest entry (highest snapshot
    /// key) so newly-installed images shadow older ones; an empty node
    /// returns the default.
    pub fn from_loaded_node(node: &LoadedNode<u32, OntologyImage>) -> Result<Self, OntologyError> {
        let mut latest: Option<(u32, OntologyImage)> = None;
        for (k, v) in node.merge_iter() {
            if latest.as_ref().is_none_or(|(prev_k, _)| *k >= *prev_k) {
                latest = Some((*k, v.clone()));
            }
        }
        match latest {
            Some((_, image)) => image.into_state(),
            None => Ok(Self::default()),
        }
    }

    /// Write the in-memory state as a fresh 256 KiB region at byte
    /// `offset` on `device`.
    pub fn flush_to_region<D: BlockDevice>(
        &self,
        device: &mut D,
        offset: u64,
    ) -> Result<(), OntologyError> {
        let mut node = self.to_loaded_node();
        BtreeRegion::write_full::<u32, OntologyImage>(device, offset, &mut node)
            .map_err(|e| OntologyError::Cbor(e.to_string()))?;
        Ok(())
    }

    /// Read the in-memory state from the 256 KiB region at byte
    /// `offset` on `device`. An all-zero region returns
    /// [`Self::default`].
    pub fn load_from_region<D: BlockDevice>(
        device: &mut D,
        offset: u64,
    ) -> Result<Self, OntologyError> {
        let mut probe = [0u8; 8];
        device
            .read_at(offset, &mut probe)
            .map_err(|e| OntologyError::Cbor(e.to_string()))?;
        if probe.iter().all(|&b| b == 0) {
            return Ok(Self::default());
        }
        let node = BtreeRegion::read_as_loaded_node::<u32, OntologyImage>(device, offset)
            .map_err(|e| OntologyError::Cbor(e.to_string()))?;
        Self::from_loaded_node(&node)
    }

    fn to_image(&self) -> OntologyImage {
        let mut tags: Vec<TagDefinition> = self.tags.values().cloned().collect();
        tags.sort_by_key(|t| t.id);
        let mut modules: Vec<ModuleRecord> = self.installed_modules.values().cloned().collect();
        modules.sort_by(|a, b| a.id.cmp(&b.id));
        let mut relations = self.relations.clone();
        relations.sort();
        OntologyImage {
            format_version: ONTOLOGY_IMAGE_FORMAT_VERSION,
            tags,
            dag: self.dag.snapshot(),
            relations,
            modules,
        }
    }
}

/// Format version of [`OntologyImage`]. Bumped on incompatible CBOR shape
/// changes; readers refuse newer versions they don't recognise.
pub const ONTOLOGY_IMAGE_FORMAT_VERSION: u16 = 1;

/// On-disk image of the ontology (IMPL §10.1). Serialised as the value of
/// each entry in the [`BtreeKind::Ontology`] §1.5 region keyed by
/// `snapshot: u32`, and also as the body of [`OntologyState::serialise`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OntologyImage {
    /// Format version — see [`ONTOLOGY_IMAGE_FORMAT_VERSION`].
    pub format_version: u16,
    /// Tag definitions, sorted by id for deterministic encoding.
    pub tags: Vec<TagDefinition>,
    /// Implication DAG.
    pub dag: DagSnapshot,
    /// Tag-to-tag relations (mutex / requires / alias), sorted for
    /// deterministic encoding. `ImpliedBy` semantics live in `dag`.
    #[serde(default)]
    pub relations: Vec<(TagId, TagRelation, TagId)>,
    /// Installed module bookkeeping records, sorted by id.
    pub modules: Vec<ModuleRecord>,
}

impl OntologyImage {
    fn into_state(self) -> Result<OntologyState, OntologyError> {
        if self.format_version > ONTOLOGY_IMAGE_FORMAT_VERSION {
            return Err(OntologyError::Cbor(format!(
                "ontology image format_version {} exceeds supported {}",
                self.format_version, ONTOLOGY_IMAGE_FORMAT_VERSION
            )));
        }
        let mut tags = HashMap::new();
        let mut names = HashMap::new();
        for def in self.tags {
            names.insert(def.name.clone(), def.id);
            tags.insert(def.id, def);
        }
        let dag = ImplicationDag::from_snapshot(self.dag)?;
        let mut installed_modules = HashMap::new();
        for record in self.modules {
            installed_modules.insert(record.id.clone(), record);
        }
        Ok(OntologyState {
            tags,
            names,
            dag,
            relations: self.relations,
            installed_modules,
        })
    }
}

#[cfg(test)]
mod tests {
    use mimisbrunnr_types::{
        ChunkParams, ChunkingAlgo, CompressionAlgo, StoragePolicy, TagDefinition, TagSemantics,
    };

    use super::*;

    fn label(name: &str) -> TagDefinition {
        TagDefinition {
            id: TagId::new(0),
            name: name.into(),
            semantics: TagSemantics::Label,
            implies: vec![],
            storage: None,
        }
    }

    fn label_with_storage(name: &str, storage: StoragePolicy) -> TagDefinition {
        TagDefinition {
            id: TagId::new(0),
            name: name.into(),
            semantics: TagSemantics::Label,
            implies: vec![],
            storage: Some(storage),
        }
    }

    fn module(id: &str, tags: Vec<TagDefinition>, imps: &[(&str, &str)]) -> OntologyModule {
        OntologyModule {
            id: id.into(),
            version: "0.1.0".into(),
            name: id.into(),
            tags,
            implications: imps
                .iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect(),
            relations: Vec::new(),
        }
    }

    fn module_with_relations(
        id: &str,
        tags: Vec<TagDefinition>,
        imps: &[(&str, &str)],
        rels: &[(&str, TagRelation, &str)],
    ) -> OntologyModule {
        OntologyModule {
            id: id.into(),
            version: "0.1.0".into(),
            name: id.into(),
            tags,
            implications: imps
                .iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect(),
            relations: rels
                .iter()
                .map(|(a, k, b)| (a.to_string(), *k, b.to_string()))
                .collect(),
        }
    }

    fn chunk_only() -> StoragePolicy {
        StoragePolicy {
            chunking: Some(ChunkParams {
                algo: ChunkingAlgo::FastCDC,
                min_size: 1024,
                avg_size: 4096,
                max_size: 16384,
            }),
            ..Default::default()
        }
    }

    fn compress_only(level: i32) -> StoragePolicy {
        StoragePolicy {
            compression: Some(CompressionAlgo::Zstd(level)),
            ..Default::default()
        }
    }

    #[test]
    fn install_basic() {
        let mut state = OntologyState::new();
        let mut alloc = IdAllocator::new();
        let m = module(
            "core",
            vec![label("file"), label("binary"), label("vehicle")],
            &[("binary", "file")],
        );
        let res = state.install(m, &mut alloc).unwrap();
        assert_eq!(res.tags_registered, 3);
        assert_eq!(res.tags_skipped, 0);
        assert_eq!(res.implications_added, 1);
        assert_eq!(state.tags.len(), 3);
        assert_eq!(state.dag.edge_count(), 1);
    }

    #[test]
    fn install_idempotent_re_install_skips() {
        let mut state = OntologyState::new();
        let mut alloc = IdAllocator::new();
        let m = module("core", vec![label("file"), label("binary")], &[]);
        state.install(m, &mut alloc).unwrap();

        // Second module redeclares one of the same tag names.
        let m2 = module("ext", vec![label("file"), label("media")], &[]);
        let res = state.install(m2, &mut alloc).unwrap();
        assert_eq!(res.tags_registered, 1);
        assert_eq!(res.tags_skipped, 1);
        // Total tags = file, binary, media
        assert_eq!(state.tags.len(), 3);
    }

    #[test]
    fn install_duplicate_module_id_errors() {
        let mut state = OntologyState::new();
        let mut alloc = IdAllocator::new();
        state
            .install(module("core", vec![label("a")], &[]), &mut alloc)
            .unwrap();
        let err = state
            .install(module("core", vec![label("b")], &[]), &mut alloc)
            .unwrap_err();
        assert!(matches!(err, OntologyError::ModuleAlreadyInstalled(_)));
    }

    #[test]
    fn axis_conflict_blocks_install() {
        // archive and log both declare chunking, neither implies the other.
        let mut state = OntologyState::new();
        let mut alloc = IdAllocator::new();
        let m = module(
            "core",
            vec![
                label_with_storage("archive", chunk_only()),
                label_with_storage("log", chunk_only()),
            ],
            &[],
        );
        let err = state.install(m, &mut alloc).unwrap_err();
        match err {
            OntologyError::AxisConflict {
                axis,
                a_name,
                b_name,
                ..
            } => {
                assert_eq!(axis, StorageAxis::Chunking);
                let names = (a_name.as_str(), b_name.as_str());
                assert!(names == ("archive", "log") || names == ("log", "archive"));
            }
            other => panic!("expected AxisConflict, got {other:?}"),
        }
        // State unchanged on rollback
        assert!(state.tags.is_empty());
        assert!(state.installed_modules.is_empty());
    }

    #[test]
    fn axis_chain_install_succeeds() {
        // file → binary → vm-disk all declare chunking; vm-disk wins.
        let mut state = OntologyState::new();
        let mut alloc = IdAllocator::new();
        let m = module(
            "chain",
            vec![
                label_with_storage("file", chunk_only()),
                label_with_storage("binary", chunk_only()),
                label_with_storage("vm-disk", chunk_only()),
            ],
            &[("binary", "file"), ("vm-disk", "binary")],
        );
        let res = state.install(m, &mut alloc);
        assert!(res.is_ok(), "got {res:?}");
    }

    #[test]
    fn resolve_policy_inherits_from_ancestor() {
        let mut state = OntologyState::new();
        let mut alloc = IdAllocator::new();
        let m = module(
            "x",
            vec![
                label_with_storage("file", compress_only(9)),
                label("binary"),
            ],
            &[("binary", "file")],
        );
        state.install(m, &mut alloc).unwrap();
        let binary = state.names["binary"];
        let resolved = state.resolve_policy(&[binary], &StoragePolicy::default());
        assert_eq!(resolved.compression, Some(CompressionAlgo::Zstd(9)));
    }

    #[test]
    fn resolve_policy_descendant_overrides() {
        let mut state = OntologyState::new();
        let mut alloc = IdAllocator::new();
        let m = module(
            "x",
            vec![
                label_with_storage("file", compress_only(9)),
                label_with_storage(
                    "binary",
                    StoragePolicy {
                        compression: Some(CompressionAlgo::Lz4),
                        ..Default::default()
                    },
                ),
            ],
            &[("binary", "file")],
        );
        state.install(m, &mut alloc).unwrap();
        let binary = state.names["binary"];
        let resolved = state.resolve_policy(&[binary], &StoragePolicy::default());
        assert_eq!(resolved.compression, Some(CompressionAlgo::Lz4));
    }

    #[test]
    fn resolve_policy_falls_back_to_default() {
        let mut state = OntologyState::new();
        let mut alloc = IdAllocator::new();
        state
            .install(module("x", vec![label("plain")], &[]), &mut alloc)
            .unwrap();
        let plain = state.names["plain"];
        let default = StoragePolicy {
            compression: Some(CompressionAlgo::Lz4),
            ..Default::default()
        };
        let resolved = state.resolve_policy(&[plain], &default);
        assert_eq!(resolved.compression, Some(CompressionAlgo::Lz4));
    }

    #[test]
    fn scrub_removes_exclusive_keeps_shared() {
        let mut state = OntologyState::new();
        let mut alloc = IdAllocator::new();
        // module A registers `media` and `audio`
        state
            .install(
                module("A", vec![label("media"), label("audio")], &[]),
                &mut alloc,
            )
            .unwrap();
        // module B re-declares `media` (will be skipped) and adds `video`
        state
            .install(
                module("B", vec![label("media"), label("video")], &[]),
                &mut alloc,
            )
            .unwrap();

        // We need module B to *also* count `media` as installed_by-it for the
        // shared-tag scrub semantics to kick in. Simulate by replicating B's
        // record to claim `media`.
        let media_id = state.names["media"];
        state
            .installed_modules
            .get_mut("B")
            .unwrap()
            .installed_tags
            .push(media_id);

        let removed = state.scrub(&"A".to_string()).unwrap();
        // `audio` was exclusive to A → removed; `media` now claimed by B too → stays.
        assert_eq!(removed, 1);
        assert!(state.names.contains_key("media"));
        assert!(!state.names.contains_key("audio"));
        assert!(state.names.contains_key("video"));
    }

    #[test]
    fn scrub_unknown_module_errors() {
        let mut state = OntologyState::new();
        let err = state.scrub(&"ghost".to_string()).unwrap_err();
        assert!(matches!(err, OntologyError::ModuleNotInstalled(_)));
    }

    #[test]
    fn upgrade_adds_new_tags() {
        let mut state = OntologyState::new();
        let mut alloc = IdAllocator::new();
        state
            .install(module("m", vec![label("a")], &[]), &mut alloc)
            .unwrap();
        let upgrade = OntologyModule {
            id: "m".into(),
            version: "0.2.0".into(),
            name: "m".into(),
            tags: vec![label("a"), label("b")],
            implications: vec![("b".into(), "a".into())],
            relations: Vec::new(),
        };
        let res = state.upgrade(upgrade).unwrap();
        assert_eq!(res.tags_registered, 1); // only `b` is new
        assert!(state.names.contains_key("b"));
        assert_eq!(state.dag.edge_count(), 1);
    }

    #[test]
    fn upgrade_unknown_module_errors() {
        let mut state = OntologyState::new();
        let upgrade = module("ghost", vec![label("x")], &[]);
        assert!(matches!(
            state.upgrade(upgrade),
            Err(OntologyError::ModuleNotInstalled(_))
        ));
    }

    #[test]
    fn cbor_round_trip() {
        let mut state = OntologyState::new();
        let mut alloc = IdAllocator::new();
        state
            .install(
                module(
                    "core",
                    vec![
                        label_with_storage("file", compress_only(9)),
                        label("binary"),
                    ],
                    &[("binary", "file")],
                ),
                &mut alloc,
            )
            .unwrap();

        let bytes = state.serialise().unwrap();
        let back = OntologyState::deserialise(&bytes).unwrap();
        assert_eq!(state.tags.len(), back.tags.len());
        assert_eq!(state.dag.edge_count(), back.dag.edge_count());
        assert_eq!(state.installed_modules.len(), back.installed_modules.len());
        let bin = back.names["binary"];
        let resolved = back.resolve_policy(&[bin], &StoragePolicy::default());
        assert_eq!(resolved.compression, Some(CompressionAlgo::Zstd(9)));
    }

    // ----- B+ tree region round-trip (R1b-8) -----

    use {mimisbrunnr_storage::FileBlockDevice, tempfile::TempDir};

    fn fresh_device() -> (TempDir, FileBlockDevice) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("ontology.bin");
        let dev = FileBlockDevice::open(&path, 1 << 20).unwrap();
        (dir, dev)
    }

    #[test]
    fn ontology_region_round_trip_empty_returns_default() {
        let (_dir, dev) = fresh_device();
        let state = OntologyState::load_from_region(&dev, 0).unwrap();
        assert!(state.tags.is_empty());
        assert!(state.installed_modules.is_empty());
        assert_eq!(state.dag.edge_count(), 0);
    }

    #[test]
    fn ontology_region_round_trip_preserves_state() {
        let (_dir, dev) = fresh_device();
        let mut state = OntologyState::new();
        let mut alloc = IdAllocator::new();
        state
            .install(
                module(
                    "core",
                    vec![
                        label_with_storage("file", compress_only(9)),
                        label("binary"),
                        label("vehicle"),
                    ],
                    &[("binary", "file")],
                ),
                &mut alloc,
            )
            .unwrap();
        state
            .install(
                module("ext", vec![label("media"), label("audio")], &[]),
                &mut alloc,
            )
            .unwrap();

        state.flush_to_region(&dev, 0).unwrap();
        let back = OntologyState::load_from_region(&dev, 0).unwrap();
        assert_eq!(back.tags.len(), state.tags.len());
        assert_eq!(back.dag.edge_count(), state.dag.edge_count());
        assert_eq!(back.installed_modules.len(), state.installed_modules.len());
        // Policy resolution is preserved (validates DAG round-trip).
        let bin = back.names["binary"];
        let resolved = back.resolve_policy(&[bin], &StoragePolicy::default());
        assert_eq!(resolved.compression, Some(CompressionAlgo::Zstd(9)));
        // Module bookkeeping is preserved.
        assert!(back.installed_modules.contains_key("core"));
        assert!(back.installed_modules.contains_key("ext"));
    }

    #[test]
    fn ontology_region_overwrite_replaces_state() {
        let (_dir, dev) = fresh_device();
        let mut first = OntologyState::new();
        let mut alloc = IdAllocator::new();
        first
            .install(module("a", vec![label("x"), label("y")], &[]), &mut alloc)
            .unwrap();
        first.flush_to_region(&dev, 0).unwrap();

        let mut second = OntologyState::new();
        let mut alloc2 = IdAllocator::new();
        second
            .install(module("b", vec![label("z")], &[]), &mut alloc2)
            .unwrap();
        second.flush_to_region(&dev, 0).unwrap();

        let back = OntologyState::load_from_region(&dev, 0).unwrap();
        assert!(back.installed_modules.contains_key("b"));
        assert!(!back.installed_modules.contains_key("a"));
        assert!(back.names.contains_key("z"));
        assert!(!back.names.contains_key("x"));
    }

    // ----- Tag relations (mutex / requires / alias) -----

    #[test]
    fn install_records_relations() {
        let mut state = OntologyState::new();
        let mut alloc = IdAllocator::new();
        let m = module_with_relations(
            "core",
            vec![
                label("active"),
                label("discontinued"),
                label("usb-c"),
                label("electronics"),
            ],
            &[],
            &[
                ("active", TagRelation::MutuallyExclusive, "discontinued"),
                ("usb-c", TagRelation::Requires, "electronics"),
            ],
        );
        state.install(m, &mut alloc).unwrap();
        assert_eq!(state.relations.len(), 2);
        let active = state.names["active"];
        let discontinued = state.names["discontinued"];
        let usb_c = state.names["usb-c"];
        let electronics = state.names["electronics"];
        assert!(
            state
                .relations
                .contains(&(active, TagRelation::MutuallyExclusive, discontinued))
        );
        assert!(
            state
                .relations
                .contains(&(usb_c, TagRelation::Requires, electronics))
        );

        let rec = &state.installed_modules["core"];
        assert_eq!(rec.installed_relations.len(), 2);
    }

    #[test]
    fn install_rejects_implied_by_in_relations() {
        let mut state = OntologyState::new();
        let mut alloc = IdAllocator::new();
        let m = module_with_relations(
            "x",
            vec![label("car"), label("vehicle")],
            &[],
            &[("car", TagRelation::ImpliedBy, "vehicle")],
        );
        let err = state.install(m, &mut alloc).unwrap_err();
        assert!(matches!(err, OntologyError::ModuleParse(_)));
        // Rolled back — no tags installed either.
        assert!(state.tags.is_empty());
    }

    #[test]
    fn install_relation_unknown_tag_errors() {
        let mut state = OntologyState::new();
        let mut alloc = IdAllocator::new();
        let m = module_with_relations(
            "x",
            vec![label("a")],
            &[],
            &[("a", TagRelation::Requires, "ghost")],
        );
        let err = state.install(m, &mut alloc).unwrap_err();
        assert!(matches!(err, OntologyError::UnknownTag(_)));
        assert!(state.tags.is_empty());
    }

    #[test]
    fn ontology_region_round_trip_preserves_relations() {
        let (_dir, dev) = fresh_device();
        let mut state = OntologyState::new();
        let mut alloc = IdAllocator::new();
        state
            .install(
                module_with_relations(
                    "core",
                    vec![
                        label("active"),
                        label("discontinued"),
                        label("laptop"),
                        label("notebook"),
                    ],
                    &[],
                    &[
                        ("active", TagRelation::MutuallyExclusive, "discontinued"),
                        ("laptop", TagRelation::Alias, "notebook"),
                    ],
                ),
                &mut alloc,
            )
            .unwrap();

        state.flush_to_region(&dev, 0).unwrap();
        let back = OntologyState::load_from_region(&dev, 0).unwrap();
        assert_eq!(back.relations.len(), 2);
        let active = back.names["active"];
        let discontinued = back.names["discontinued"];
        let laptop = back.names["laptop"];
        let notebook = back.names["notebook"];
        assert!(
            back.relations
                .contains(&(active, TagRelation::MutuallyExclusive, discontinued))
        );
        assert!(
            back.relations
                .contains(&(laptop, TagRelation::Alias, notebook))
        );
        assert_eq!(back.installed_modules["core"].installed_relations.len(), 2);
    }

    #[test]
    fn cbor_round_trip_preserves_relations() {
        let mut state = OntologyState::new();
        let mut alloc = IdAllocator::new();
        state
            .install(
                module_with_relations(
                    "x",
                    vec![label("a"), label("b")],
                    &[],
                    &[("a", TagRelation::Requires, "b")],
                ),
                &mut alloc,
            )
            .unwrap();
        let bytes = state.serialise().unwrap();
        let back = OntologyState::deserialise(&bytes).unwrap();
        assert_eq!(state.relations, back.relations);
    }

    #[test]
    fn scrub_removes_exclusive_relations_keeps_shared() {
        let mut state = OntologyState::new();
        let mut alloc = IdAllocator::new();
        state
            .install(
                module_with_relations(
                    "A",
                    vec![label("a"), label("b"), label("c")],
                    &[],
                    &[
                        ("a", TagRelation::Requires, "b"),
                        ("a", TagRelation::Requires, "c"),
                    ],
                ),
                &mut alloc,
            )
            .unwrap();
        // Pretend module B was installed with the (a, Requires, b) relation too.
        let a = state.names["a"];
        let b = state.names["b"];
        let b_record = ModuleRecord {
            id: "B".into(),
            version: "0.1.0".into(),
            name: "B".into(),
            installed_tags: vec![],
            installed_implications: vec![],
            installed_relations: vec![(a, TagRelation::Requires, b)],
        };
        state.installed_modules.insert("B".into(), b_record);

        state.scrub(&"A".to_string()).unwrap();
        // (a, Requires, b) is shared → kept; (a, Requires, c) was exclusive → gone.
        assert_eq!(state.relations.len(), 1);
        assert_eq!(state.relations[0], (a, TagRelation::Requires, b));
    }
}
