This is essentially the package management problem applied to schemas. An app says "I need these tag types to exist with these semantics," another app says "I need some of the same tags plus some new ones," and when you remove an app you want to clean up what it brought without destroying what others share.

## Ontology Structure

First, ontologies aren't monolithic. They're composed from **modules** — self-contained units that declare tags, relations, implications, and constraints:

```rust
struct OntologyModule {
    // Identity
    id: ModuleId,           // "org.metta.music", "com.example.photos"
    version: SemVer,        // 2.1.0
    name: String,           // "Music Ontology"
    
    // What this module provides
    tags: Vec<TagDeclaration>,
    implications: Vec<Implication>,
    mutex_groups: Vec<MutexGroup>,
    constraints: Vec<Constraint>,
    
    // Dependencies: other modules this one builds upon
    requires: Vec<ModuleDependency>,
    
    // Who installed this module (for scrubbing)
    installed_by: Vec<AppId>,       // refcount of installers
    
    // Is this a core module that can't be removed?
    core: bool,
}

struct TagDeclaration {
    name: String,               // "playlist"
    semantics: TagSemantics,    // OrderedCollection { element_constraint: "audio" }
    description: String,
    
    // Which module owns this tag definition
    defined_in: ModuleId,
    
    // Can other modules extend this tag's implications?
    extensible: bool,
}

struct Implication {
    from: TagRef,               // "mp3"
    to: TagRef,                 // "audio"
    defined_in: ModuleId,       // who added this edge
}

struct ModuleDependency {
    module_id: ModuleId,
    version_req: VersionReq,    // ">=1.0, <3.0"
}
```

The key design decision: **tags are global, but their definitions are owned by modules, and implications can be contributed by any module**.

## The Module Hierarchy

```
┌─────────────────────────────────────────────────────────┐
│  Core Ontology (built-in, cannot be removed)            │
│                                                         │
│  Tags: file, directory, text, binary, image, audio,     │
│        video, document, archive, executable,            │
│        active, archived, favorite, trash                │
│                                                         │
│  Implications: jpeg → image → file                      │
│                png → image → file                       │
│                mp3 → audio → file                       │
│                pdf → document → file                    │
│                                                         │
│  Semantics: "playlist" pattern = OrderedCollection      │
│             "album" pattern = OrderedCollection          │
│             MIME types → tag mappings                    │
├─────────────────────────────────────────────────────────┤
│  org.metta.music (depends: core >=1.0)                  │
│                                                         │
│  Tags: artist, album, genre, track-number, bpm,         │
│        playlist:*, compilation, live-recording           │
│                                                         │
│  Implications: rock → genre                             │
│                electronic → genre                       │
│                jazz → genre                             │
│                flac → audio  (extending core's chain)   │
│                opus → audio                             │
│                                                         │
│  Constraints: track-number requires audio               │
│               bpm requires audio                        │
├─────────────────────────────────────────────────────────┤
│  com.djapp.mixing (depends: core >=1.0,                 │
│                             org.metta.music >=1.0)      │
│                                                         │
│  Tags: cue-point, beatgrid, key-signature,              │
│        energy-level, mixable-with                       │
│                                                         │
│  Implications: cue-point requires audio                 │
│                energy-level: Attribute { Int }           │
│                                                         │
│  Relations: mixable-with (symmetric)                    │
├─────────────────────────────────────────────────────────┤
│  com.photoapp.editor (depends: core >=1.0)              │
│                                                         │
│  Tags: raw-photo, edited, lens, focal-length,           │
│        iso, aperture, gps-location, face:*              │
│                                                         │
│  Implications: raw-photo → image                        │
│                cr2 → raw-photo                          │
│                nef → raw-photo                          │
│                dng → raw-photo                          │
└─────────────────────────────────────────────────────────┘
```

## Module Distribution

Ontology modules are shipped as small declarative files — think `.desktop` files on Linux or `Info.plist` on macOS. No code, just declarations:

```rust
// On-disk format: TOML (human-readable, easy to inspect)
// Stored in a well-known location inside the filesystem itself

// brunnr:/system/ontology/modules/org.metta.music.toml
```

```toml
# org.metta.music.toml

[module]
id = "org.metta.music"
version = "2.1.0"
name = "Music Ontology"
description = "Tags and relations for music libraries"
core = false

[requires]
"org.metta.core" = ">=1.0, <4.0"

[[tags]]
name = "artist"
semantics = "attribute"
value_type = "text"
description = "Performing artist or band"
extensible = true

[[tags]]
name = "album"
semantics = "ordered-collection"
element_constraint = "audio"
description = "Music album with track ordering"

[[tags]]
name = "genre"
semantics = "grouping"
description = "Music genre category"
extensible = true  # other modules can add sub-genres

[[tags]]
name = "bpm"
semantics = "attribute"
value_type = "int"
description = "Beats per minute"

[[tags]]
name = "playlist"
semantics = "ordered-collection"
element_constraint = "audio"
description = "User-created playlist"

# Implications: from → to
[[implications]]
from = "rock"
to = "genre"

[[implications]]
from = "electronic"
to = "genre"

[[implications]]
from = "jazz"
to = "genre"

[[implications]]
from = "flac"
to = "audio"

[[implications]]
from = "opus"
to = "audio"

# Constraints
[[constraints]]
tag = "bpm"
requires = "audio"

[[constraints]]
tag = "track-number"
requires = "audio"
```

### Distribution Channels

```
1. Built-in:     Core ontology compiled into Mímisbrunnr itself
2. App-bundled:   App ships a .toml module in its package
3. Repository:    Community ontology modules (like crates.io for schemas)
4. User-created:  mimir ontology create my-project-tags.toml
```

## Installation / Merge Protocol

When an app installs, it declares which ontology modules it needs. The merge is transactional:

```rust
struct OntologyManager {
    active: ComposedOntology,           // the live merged result
    modules: HashMap<ModuleId, InstalledModule>,
    module_dir: PathBuf,                // /system/ontology/modules/
}

struct InstalledModule {
    module: OntologyModule,
    installed_by: Vec<AppId>,           // who needs this module
    installed_at: HybridTimestamp,
}

struct ComposedOntology {
    tags: HashMap<String, ComposedTag>,
    implications: ImplicationDag,        // merged from all modules
    mutex_groups: Vec<MutexGroup>,
    constraints: Vec<Constraint>,
}

struct ComposedTag {
    definition: TagDeclaration,
    defined_in: ModuleId,               // which module owns the definition
    implications_from: Vec<ModuleId>,   // which modules add implications to this tag
    used_by_modules: HashSet<ModuleId>, // which modules reference this tag
}
```

### The Merge Algorithm

```rust
fn install_module(
    manager: &mut OntologyManager,
    module: OntologyModule,
    requester: AppId,
) -> Result<MergeReport> {
    let mut report = MergeReport::new();
    
    // 1. Check dependencies
    for dep in &module.requires {
        if !manager.modules.contains_key(&dep.module_id) {
            return Err(OntologyError::MissingDependency {
                needed: dep.module_id.clone(),
                needed_by: module.id.clone(),
            });
        }
        let installed = &manager.modules[&dep.module_id];
        if !dep.version_req.matches(&installed.module.version) {
            return Err(OntologyError::VersionMismatch {
                module: dep.module_id.clone(),
                required: dep.version_req.clone(),
                installed: installed.module.version.clone(),
            });
        }
    }
    
    // 2. Check for conflicts
    for tag in &module.tags {
        if let Some(existing) = manager.active.tags.get(&tag.name) {
            if existing.defined_in != module.id {
                // Tag already exists from another module
                if compatible_definitions(&existing.definition, tag) {
                    // Same semantics — OK, just add a reference
                    report.shared_tags.push(tag.name.clone());
                } else {
                    // Conflicting definition!
                    return Err(OntologyError::TagConflict {
                        tag: tag.name.clone(),
                        existing_module: existing.defined_in.clone(),
                        new_module: module.id.clone(),
                        existing_semantics: existing.definition.semantics,
                        new_semantics: tag.semantics,
                    });
                }
            }
        }
    }
    
    // 3. Check for implication cycles
    let mut test_dag = manager.active.implications.clone();
    for imp in &module.implications {
        let from_id = resolve_tag_id(&imp.from);
        let to_id = resolve_tag_id(&imp.to);
        test_dag.add_edge(from_id, to_id);
    }
    if test_dag.has_cycle() {
        return Err(OntologyError::CycleDetected {
            module: module.id.clone(),
            cycle: test_dag.find_cycle(),
        });
    }
    
    // 4. Check mutex group consistency
    for group in &module.mutex_groups {
        // Ensure no object currently has two tags from this mutex group
        // (or defer this check — just warn, don't block installation)
        report.mutex_warnings.extend(
            check_mutex_consistency(&manager.active, group)
        );
    }
    
    // 5. Apply — all checks passed
    let txn = begin_ontology_txn();
    
    // Register new tags
    for tag in &module.tags {
        let tag_id = allocate_or_reuse_tag_id(&tag.name);
        manager.active.tags.entry(tag.name.clone())
            .and_modify(|existing| {
                existing.used_by_modules.insert(module.id.clone());
            })
            .or_insert(ComposedTag {
                definition: tag.clone(),
                defined_in: module.id.clone(),
                implications_from: vec![],
                used_by_modules: hashset![module.id.clone()],
            });
        report.tags_added.push(tag.name.clone());
    }
    
    // Register implications and rematerialize
    let mut new_implications = Vec::new();
    for imp in &module.implications {
        manager.active.implications.add_edge(
            resolve_tag_id(&imp.from),
            resolve_tag_id(&imp.to),
        );
        new_implications.push((imp.from.clone(), imp.to.clone()));
    }
    
    // 6. Rematerialize affected tags
    if !new_implications.is_empty() {
        rematerialize_implications(&manager.active, &new_implications);
        report.rematerialized_count = count_affected_objects(&new_implications);
    }
    
    // 7. Record installation
    manager.modules.insert(module.id.clone(), InstalledModule {
        module,
        installed_by: vec![requester],
        installed_at: HybridTimestamp::now(),
    });
    
    // 8. Sync to cluster
    emit_sync_op(SyncOpKind::OntologyUpdate {
        version: manager.active.version + 1,
        delta: OntologyDelta::InstallModule { module_id: module.id.clone() },
    });
    
    txn.commit();
    Ok(report)
}
```

### Rematerialization

When new implications are added, existing objects may need additional tags materialized. This is a bulk bitmap operation — efficient with roaring:

```rust
fn rematerialize_implications(
    ontology: &ComposedOntology,
    new_implications: &[(TagRef, TagRef)],
) {
    // For each new implication "A implies B":
    // Every object with tag A must also get tag B (and B's transitive closure)
    
    for (from, to) in new_implications {
        let from_id = resolve_tag_id(from);
        let to_id = resolve_tag_id(to);
        
        // All objects that have "from" tag
        let from_bitmap = tag_index.get(from_id);
        
        // Add them all to "to" tag's bitmap
        // This is a single bitmap OR — microseconds even for millions of objects
        tag_index.get_mut(to_id).or_assign(from_bitmap);
        
        // Transitively: "to" implies further tags
        for ancestor in ontology.implications.ancestors(to_id) {
            tag_index.get_mut(ancestor).or_assign(from_bitmap);
        }
        
        // Update forward indexes for affected objects
        for obj_id in from_bitmap.iter() {
            forward_index.add_materialized_tags(obj_id, to_id);
        }
    }
}
```

The bitmap OR is the key operation. Adding an implication "flac → audio" when 100K objects are tagged "flac" is a single `bitmap_audio |= bitmap_flac` — microseconds.

## Module Upgrade

An app ships a new version of its ontology module. The upgrade is a diff:

```rust
fn upgrade_module(
    manager: &mut OntologyManager,
    new_module: OntologyModule,
) -> Result<MergeReport> {
    let old_module = manager.modules.get(&new_module.id)
        .ok_or(OntologyError::NotInstalled)?;
    
    let diff = diff_modules(&old_module.module, &new_module);
    
    // Safe changes (always allowed):
    // - Adding new tags
    // - Adding new implications
    // - Relaxing constraints
    // - Adding new mutex groups
    
    // Requires migration:
    // - Changing tag semantics (Label → OrderedCollection)
    // - Removing implications (need to de-materialize)
    // - Tightening constraints (existing data may violate)
    
    // Forbidden without explicit --force:
    // - Removing tags that have data
    // - Changing value types on attributes with existing values
    
    let mut report = MergeReport::new();
    
    for change in &diff.changes {
        match change {
            OntologyChange::AddTag(tag) => {
                // Always safe
                apply_add_tag(manager, tag, &new_module.id);
            }
            OntologyChange::AddImplication(from, to) => {
                // Safe, needs rematerialization
                apply_add_implication(manager, from, to);
                report.needs_rematerialize = true;
            }
            OntologyChange::RemoveImplication(from, to) => {
                // Needs de-materialization: remove "to" from objects
                // that ONLY had it via this implication
                apply_remove_implication(manager, from, to);
                report.needs_rematerialize = true;
            }
            OntologyChange::RemoveTag(tag_name) => {
                let tag_id = resolve_tag_id(tag_name);
                let bitmap = tag_index.get(tag_id);
                if bitmap.len() > 0 {
                    // Tag has data! Can't just remove.
                    report.blocked_removals.push(BlockedRemoval {
                        tag: tag_name.clone(),
                        object_count: bitmap.len() as u64,
                        suggestion: "Use 'mimir ontology migrate' to reassign objects",
                    });
                } else {
                    apply_remove_tag(manager, tag_id);
                }
            }
            OntologyChange::ChangeSemantics(tag_name, old_sem, new_sem) => {
                // Requires migration plan
                report.migrations.push(MigrationPlan {
                    tag: tag_name.clone(),
                    from: old_sem.clone(),
                    to: new_sem.clone(),
                });
            }
        }
    }
    
    Ok(report)
}
```

### De-materialization

When an implication is removed ("flac" no longer implies "audio"), you need to figure out which objects should lose the "audio" tag. The tricky part: an object tagged both "flac" and "mp3" should keep "audio" (because mp3 still implies audio). You can only remove materialized tags that have no other source:

```rust
fn dematerialize_implication(from_id: TagId, to_id: TagId) {
    // Objects that have "from" tag
    let from_bitmap = tag_index.get(from_id);
    
    // Objects that have "to" tag via OTHER implications
    // (not just the one being removed)
    let mut other_sources = RoaringBitmap::new();
    for other_from in ontology.tags_implying(to_id) {
        if other_from != from_id {
            other_sources |= tag_index.get(other_from);
        }
    }
    
    // Also: objects that were directly tagged "to" (not materialized)
    let directly_tagged = forward_index.objects_with_direct_tag(to_id);
    other_sources |= &directly_tagged;
    
    // Objects to remove "to" from: have it only via "from"
    let to_remove = from_bitmap.clone() - &other_sources;
    
    // Remove from bitmap
    *tag_index.get_mut(to_id) -= &to_remove;
    
    // Update forward indexes
    for obj_id in to_remove.iter() {
        forward_index.remove_materialized_tag(obj_id, to_id);
    }
}
```

This requires the forward index to distinguish **direct** tags (user explicitly tagged this object) from **materialized** tags (added by implication). A single bit per assertion:

```rust
enum TagOrigin {
    Direct,        // user/app explicitly tagged this object
    Materialized,  // added automatically by implication engine
}
```

## Scrubbing (App Removal)

When an app is removed, its ontology modules should be cleaned up. But only if no other app needs them. This is reference counting:

```rust
fn uninstall_app(
    manager: &mut OntologyManager, 
    app_id: AppId
) -> ScrubReport {
    let mut report = ScrubReport::new();
    
    // 1. Find all modules this app installed
    let app_modules: Vec<ModuleId> = manager.modules.iter()
        .filter(|(_, m)| m.installed_by.contains(&app_id))
        .map(|(id, _)| id.clone())
        .collect();
    
    for module_id in &app_modules {
        let module = manager.modules.get_mut(module_id).unwrap();
        
        // Remove this app from the installer list
        module.installed_by.retain(|id| id != &app_id);
        
        if module.installed_by.is_empty() {
            // No one else needs this module — candidate for removal
            report.orphaned_modules.push(module_id.clone());
        } else {
            report.shared_modules.push(SharedModule {
                id: module_id.clone(),
                still_needed_by: module.installed_by.clone(),
            });
        }
    }
    
    // 2. For orphaned modules, check reverse dependencies
    let removable = topological_removal_order(&report.orphaned_modules, manager);
    
    for module_id in &removable {
        let result = try_remove_module(manager, module_id);
        match result {
            RemoveResult::Clean => {
                report.removed.push(module_id.clone());
            }
            RemoveResult::HasData(data_report) => {
                report.has_data.push(DataRetention {
                    module: module_id.clone(),
                    tags_with_data: data_report,
                });
            }
        }
    }
    
    report
}
```

### The Data Question

An app installed the music ontology, tagged 50,000 files with "artist", "genre", "bpm". Now the app is removed. What happens to those tags?

Three strategies, presented as a choice to the user:

```rust
enum ScrubPolicy {
    // Keep all data. Module definition removed but tags remain
    // as "orphaned tags" — still queryable, just no module backing them.
    // Default and safest.
    Preserve,
    
    // Keep tags that are shared with other modules' tag definitions.
    // Remove tags unique to the scrubbed module.
    // E.g., "audio" stays (core module defines it), "bpm" goes.
    RemoveUnique,
    
    // Remove all tags defined by this module from all objects.
    // Destructive! Only for "I really want this data gone."
    Purge,
}
```

```rust
fn scrub_module(
    manager: &mut OntologyManager,
    module_id: &ModuleId,
    policy: ScrubPolicy,
) -> ScrubResult {
    let module = &manager.modules[module_id].module;
    
    match policy {
        ScrubPolicy::Preserve => {
            // Remove module definition but mark its tags as "orphaned"
            for tag in &module.tags {
                if let Some(composed) = manager.active.tags.get_mut(&tag.name) {
                    composed.used_by_modules.remove(module_id);
                    if composed.defined_in == *module_id {
                        composed.defined_in = ModuleId::orphaned();
                    }
                }
            }
            // Remove implications contributed by this module
            remove_module_implications(manager, module_id);
            
            manager.modules.remove(module_id);
            ScrubResult::Preserved
        }
        
        ScrubPolicy::RemoveUnique => {
            let mut removed_tags = Vec::new();
            let mut preserved_tags = Vec::new();
            
            for tag in &module.tags {
                let composed = &manager.active.tags[&tag.name];
                
                if composed.used_by_modules.len() <= 1 {
                    // Only this module uses this tag — remove it
                    let tag_id = resolve_tag_id(&tag.name);
                    let bitmap = tag_index.get(tag_id);
                    
                    // Remove tag from all objects
                    for obj_id in bitmap.iter() {
                        forward_index.remove_tag(obj_id, tag_id);
                    }
                    tag_index.remove(tag_id);
                    manager.active.tags.remove(&tag.name);
                    removed_tags.push(tag.name.clone());
                } else {
                    // Other modules also define/use this tag — keep it
                    preserved_tags.push(tag.name.clone());
                }
            }
            
            remove_module_implications(manager, module_id);
            manager.modules.remove(module_id);
            
            ScrubResult::Partial { removed_tags, preserved_tags }
        }
        
        ScrubPolicy::Purge => {
            // Nuclear option: remove everything this module brought
            for tag in &module.tags {
                let tag_id = resolve_tag_id(&tag.name);
                let bitmap = tag_index.remove(tag_id);
                
                for obj_id in bitmap.iter() {
                    forward_index.remove_tag(obj_id, tag_id);
                }
                manager.active.tags.remove(&tag.name);
            }
            
            remove_module_implications(manager, module_id);
            manager.modules.remove(module_id);
            
            ScrubResult::Purged
        }
    }
}
```

### The Orphan Tag UX

With `Preserve` policy, orphaned tags are still fully functional — you can query them, add them to objects, explore them. They just don't have a module backing their definition. The CLI can show this:

```
$ mimir ontology status

Modules:
  org.metta.core          v1.0.0   (built-in)       42 tags
  org.metta.music         v2.1.0   (DJ App, Player)  18 tags
  com.photoapp.editor     v1.3.0   (Photo Editor)    12 tags

Orphaned tags: 3
  bpm          (was: com.djapp.mixing v1.0.0, removed 2024-03-15)
  cue-point    (was: com.djapp.mixing v1.0.0, removed 2024-03-15)
  energy-level (was: com.djapp.mixing v1.0.0, removed 2024-03-15)

  These tags still have data on 12,450 objects.
  Run 'mimir ontology adopt' to assign them to another module,
  or 'mimir ontology scrub --purge' to remove them.
```

### Adopting Orphans

A user (or a new app) can adopt orphaned tags:

```
$ mimir ontology adopt bpm --into org.metta.music

Adopted tag 'bpm' into module 'org.metta.music'.
12,450 objects retain their 'bpm' values.
```

Or create a personal module for ad-hoc tags:

```
$ mimir ontology create-module my-tags "My Custom Tags"
$ mimir ontology adopt bpm cue-point energy-level --into my-tags
```

## Cluster Sync of Ontology Changes

Ontology changes are sync ops like everything else, but they need special handling because **all nodes must agree on the ontology before applying tag ops that depend on it**:

```rust
SyncOpKind::OntologyUpdate {
    version: u64,
    delta: OntologyDelta,
}

enum OntologyDelta {
    InstallModule {
        module: OntologyModule,    // full module definition
    },
    RemoveModule {
        module_id: ModuleId,
        policy: ScrubPolicy,
    },
    UpgradeModule {
        module_id: ModuleId,
        new_version: OntologyModule,
        migration: Vec<OntologyChange>,
    },
}
```

### Ordering Guarantee

Ontology ops must be applied **before** any tag ops that depend on the new tags. The HLC ordering handles this naturally — the module installation op has an earlier timestamp than any tag ops using the new tags (because you can't use tags that don't exist yet).

But there's an edge case: what if Node A installs a module and immediately tags objects, and Node B receives the tag ops before the module installation (out of order due to network routing)?

```rust
fn apply_sync_op(op: &SyncOp) -> SyncResult {
    match &op.op {
        SyncOpKind::AddTag { object, tag } => {
            if !ontology.tag_exists(*tag) {
                // Tag not known yet — buffer this op
                // It will be replayed when the ontology update arrives
                pending_ops.buffer(op.clone());
                return SyncResult::Buffered;
            }
            // Normal apply...
        }
        SyncOpKind::OntologyUpdate { version, delta } => {
            apply_ontology_delta(delta);
            
            // Replay any buffered ops that were waiting for this
            let replayable = pending_ops.drain_for_ontology(*version);
            for buffered_op in replayable {
                apply_sync_op(&buffered_op);
            }
        }
        // ...
    }
}
```

## CLI Summary

```
# Module management
mimir ontology list                          # show installed modules
mimir ontology show org.metta.music          # show module details
mimir ontology install ./music.toml          # install from file  
mimir ontology install org.metta.music       # install from repository
mimir ontology upgrade org.metta.music       # upgrade to latest
mimir ontology remove org.metta.music        # remove (preserve data)
mimir ontology remove --purge com.djapp      # remove + delete all tags

# Inspection
mimir ontology status                        # modules, orphans, health
mimir ontology graph                         # show implication DAG
mimir ontology graph --tag vehicle           # show implications for one tag
mimir ontology conflicts                     # check for issues

# Orphan management
mimir ontology orphans                       # list orphaned tags
mimir ontology adopt bpm --into my-tags      # adopt orphan into module
mimir ontology scrub --orphans               # remove all orphaned tags

# Creating custom modules
mimir ontology create-module my-project      # new empty module
mimir ontology add-tag my-project sprint     # add tag to module
mimir ontology add-implication               # add A → B
    --from sprint-task --to task 
    --module my-project
mimir ontology export my-project > out.toml  # export for sharing
```

## Complete Lifecycle

```
App installs
     │
     ▼
┌──────────────────┐
│ Load module TOML │
│ Check deps       │
│ Check conflicts  │
│ Check cycles     │
└────────┬─────────┘
         │ all OK
         ▼
┌──────────────────┐
│ Merge into       │
│ active ontology  │──── new tags get TagIds
│                  │──── new implications added to DAG
│                  │──── rematerialize affected bitmaps
└────────┬─────────┘
         │
         ▼
┌──────────────────┐
│ Sync to cluster  │  OntologyDelta::InstallModule
└────────┬─────────┘
         │
         │  ... time passes, app used, objects tagged ...
         │
         ▼
App removed
     │
     ▼
┌──────────────────┐
│ Decrement        │
│ refcount         │──── if other apps need this module: done
└────────┬─────────┘
         │ refcount = 0
         ▼
┌──────────────────┐
│ Choose policy    │
│                  │──── Preserve: tags become orphans (safe default)
│                  │──── RemoveUnique: scrub module-only tags
│                  │──── Purge: remove all tags + data
└────────┬─────────┘
         │
         ▼
┌──────────────────┐
│ Remove           │──── remove implications from DAG
│ implications     │──── de-materialize affected objects
│                  │──── bitmap set-difference operations
└────────┬─────────┘
         │
         ▼
┌──────────────────┐
│ Remove module    │
│ definition       │──── sync OntologyDelta::RemoveModule
│                  │──── orphaned tags remain queryable
└──────────────────┘
```

The core principle: **data outlives apps**. The default is always to preserve. Tags your music app created are still your data — they describe your files. Removing an app removes the _schema_, not the _data_, unless you explicitly ask for it.
