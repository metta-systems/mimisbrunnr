Yes, it makes sense as an interop attribute, but it should be designed carefully to avoid reintroducing hierarchy through the back door.

## The Right Framing

`unix_path` is not how the file _lives_ in Mímisbrunnr. It's metadata about how this file _appears_ when projected onto a Unix system. The same object can have different unix_paths for different contexts — a source file has one path in the project tree, a different path in the installed system, maybe a third in a container image.

This means it's not one attribute, it's scoped:

```rust
// Not this:
Attr { key: unix_path, value: "/bin/bash" }
Attr { key: unix_path, value: "/usr/bin/bash" }

// This:
Attr { key: unix_path, value: "src/kernel/main.rs",     context: "project:vesper" }
Attr { key: unix_path, value: "/boot/vesper",            context: "install:rpi4" }
Attr { key: unix_path, value: "usr/bin/vesper",          context: "package:deb" }
```

Different projections of the same object into different Unix trees.

## Modeling It

A **path context** is itself a tag with `Grouping` semantics — it represents a specific Unix tree projection:

```toml
# In org.metta.unix-interop.toml

[[tags]]
name = "path-context"
semantics = "grouping"
description = "A named Unix filesystem projection"
# path-context:project-vesper, path-context:install-rpi4, path-context:package-deb

[[tags]]
name = "unix-path"
semantics = "attribute"
value_type = "text"
description = "Path within a specific unix tree projection. Scoped by path-context."
# Always paired with a path-context tag

[[tags]]
name = "unix-mode"
semantics = "attribute"
value_type = "int"
description = "Unix permission bits for this object in the projection"

[[tags]]
name = "unix-uid"
semantics = "attribute"
value_type = "int"

[[tags]]
name = "unix-gid"
semantics = "attribute"
value_type = "int"

[[tags]]
name = "symlink-target"
semantics = "attribute"
value_type = "text"
description = "If this entry is a symlink in the projection, what it points to"
```

The actual data on a file:

```
Object: vesper (the kernel binary)
  project=vesper  component=kernel
  executable  stripped  current
  target=aarch64-unknown-none

  path-context:project-vesper
    unix-path="target/aarch64-unknown-none/release/vesper"

  path-context:install-rpi4
    unix-path="/boot/vesper"
    unix-mode=0o755
    unix-uid=0
    unix-gid=0

  path-context:package-deb
    unix-path="usr/lib/vesper/vesper"
    unix-mode=0o755

Object: bash
  executable  shell  system
  
  path-context:fhs
    unix-path="/usr/bin/bash"
    unix-mode=0o755
  
  path-context:fhs
    symlink-target="/usr/bin/bash"
    unix-path="/bin/bash"          ← this is a symlink entry, not the file itself
```

Wait — that symlink case reveals a subtlety. In Unix, `/bin/bash` and `/usr/bin/bash` might be the same file (hardlink) or one might be a symlink. In Mímisbrunnr there's only one object. The projection needs to express this:

```rust
enum PathEntry {
    // This object appears at this path
    Direct {
        unix_path: String,
        mode: u32,
        uid: u32,
        gid: u32,
    },
    // A symlink at this path points to another path
    Symlink {
        unix_path: String,
        target: String,         // what the symlink points to
    },
    // A hardlink: same object appears at multiple paths
    Hardlink {
        unix_path: String,
        // Same object, multiple paths — natural in Mímisbrunnr
    },
}
```

But encoding these as attributes gets awkward. Better to make the path context a proper structure:

```rust
struct PathProjection {
    context: String,              // "install-rpi4", "package-deb"
    entries: Vec<ProjectedEntry>,
}

struct ProjectedEntry {
    object: ObjectId,
    path: String,
    entry_type: ProjectedEntryType,
}

enum ProjectedEntryType {
    File { mode: u32, uid: u32, gid: u32 },
    Symlink { target: String },
    Directory { mode: u32 },       // virtual — doesn't correspond to an object
}
```

Directories are interesting. In Mímisbrunnr there's no directory object. But when projecting to Unix, you need `/usr/bin/` to exist for `/usr/bin/bash` to make sense. These are **virtual entries** — synthesized from the paths, not backed by real objects.

## Storage Design

A path context is a first-class object in the filesystem — it's the projection definition itself:

```
Object: path-context:install-rpi4
  path-context
  name="install-rpi4"
  description="Raspberry Pi 4 installed system layout"
  base-path="/"
  
  # The mapping is stored as a relation:
  # (path-context) --projects--> (object, path, metadata)
```

The actual path-to-object mapping can be stored two ways:

**Option A: As assertions on each object** (distributed)

```
Object: vesper-binary
  ...all existing tags...
  unix-path:install-rpi4="/boot/vesper"
  unix-mode:install-rpi4=0o755
```

Advantage: the path travels with the object across cluster sync. Disadvantage: querying "give me the full tree for install-rpi4" requires scanning all objects in that context.

**Option B: As an ordered collection on the context object** (centralized)

```
Object: path-context:install-rpi4
  path-context
  entries: [
    { object: vesper-binary,    path: "/boot/vesper",        mode: 0o755 },
    { object: config-txt,       path: "/boot/config.txt",    mode: 0o644 },
    { object: dtb-file,         path: "/boot/bcm2711.dtb",   mode: 0o644 },
    { object: vesper-initrd,    path: "/boot/initrd.img",    mode: 0o644 },
  ]
```

Advantage: generating a tarball is a sequential read of one object. Disadvantage: the context object becomes large for big projections.

**Option C: Both** (the right answer)

The path context object holds the manifest. Each object also carries its path assertion for that context. They're kept in sync by the same mutation — when you add an object to a projection, both are updated atomically.

```rust
fn add_to_projection(
    context: &str,
    object: ObjectId,
    path: &str,
    mode: u32,
    uid: u32,
    gid: u32,
) {
    let context_id = resolve_tag(format!("path-context:{}", context));
    
    // 1. Tag the object with its path in this context
    mimir.set_attr(object, 
        &format!("unix-path:{}", context), 
        path);
    mimir.set_attr(object,
        &format!("unix-mode:{}", context),
        mode);
    
    // 2. Add to the context's manifest (ordered collection)
    mimir.add_to_collection(context_id, object, ManifestEntry {
        path: path.to_string(),
        mode, uid, gid,
        entry_type: EntryType::File,
    });
}
```

## Generating a Tarball

This is the primary use case — export a projection as a Unix-compatible archive:

```rust
fn export_tarball(context: &str, output: &Path) -> Result<()> {
    let context_obj = mimir.query_one(
        And(vec![
            HasTag(tag("path-context")),
            HasAttr { key: "name", op: Eq, value: Text(context) },
        ])
    )?;
    
    // Get the manifest — ordered list of (object, path, metadata)
    let manifest = mimir.get_collection_ordered(context_obj);
    
    // Synthesize directories from paths
    let mut dirs = BTreeSet::new();
    for entry in &manifest {
        let mut current = PathBuf::from(&entry.path);
        while let Some(parent) = current.parent() {
            if parent.as_os_str().is_empty() { break; }
            dirs.insert(parent.to_path_buf());
            current = parent.to_path_buf();
        }
    }
    
    let mut tar = tar::Builder::new(File::create(output)?);
    
    // Write directories first
    for dir in &dirs {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Directory);
        header.set_path(dir)?;
        header.set_mode(0o755);
        header.set_size(0);
        tar.append(&header, &[] as &[u8])?;
    }
    
    // Write files
    for entry in &manifest {
        match entry.entry_type {
            EntryType::File => {
                let blob = mimir.read_blob(entry.object)?;
                let mut header = tar::Header::new_gnu();
                header.set_path(&entry.path)?;
                header.set_mode(entry.mode);
                header.set_uid(entry.uid as u64);
                header.set_gid(entry.gid as u64);
                header.set_size(blob.len() as u64);
                tar.append(&header, blob.as_slice())?;
            }
            EntryType::Symlink { ref target } => {
                let mut header = tar::Header::new_gnu();
                header.set_entry_type(tar::EntryType::Symlink);
                header.set_path(&entry.path)?;
                header.set_link_name(target)?;
                tar.append(&header, &[] as &[u8])?;
            }
        }
    }
    
    tar.finish()?;
    Ok(())
}
```

## Importing from Unix

The reverse — ingesting a tarball or a directory tree into Mímisbrunnr:

```rust
fn import_tree(
    source: &Path,
    context: &str,
    project: &str,
    auto_tag: bool,       // infer tags from path/extension
) -> Result<ImportReport> {
    let mut report = ImportReport::new();
    
    for entry in WalkDir::new(source) {
        let entry = entry?;
        let rel_path = entry.path().strip_prefix(source)?;
        
        // Create or find existing object (by content hash for dedup)
        let content = fs::read(entry.path())?;
        let hash = blake3::hash(&content);
        
        let obj_id = if let Some(existing) = find_by_hash(hash) {
            // Same content already exists — reuse object
            report.deduplicated += 1;
            existing
        } else {
            let obj = mimir.create_object(&content)?;
            report.created += 1;
            obj
        };
        
        // Set unix path for this context
        add_to_projection(context, obj_id, 
            rel_path.to_str().unwrap(),
            entry.metadata()?.mode(),
            entry.metadata()?.uid(),
            entry.metadata()?.gid(),
        );
        
        // Auto-tag based on path conventions and extension
        if auto_tag {
            auto_tag_from_path(obj_id, rel_path, project);
        }
    }
    
    Ok(report)
}

fn auto_tag_from_path(obj_id: ObjectId, path: &Path, project: &str) {
    mimir.set_attr(obj_id, "project", project);
    
    // Extension → language tag
    match path.extension().and_then(|e| e.to_str()) {
        Some("rs") => { 
            mimir.set_attr(obj_id, "lang", "rust");
            mimir.tag(obj_id, "implementation");
        }
        Some("c" | "h") => {
            mimir.set_attr(obj_id, "lang", "c");
            if path.extension() == Some("h".as_ref()) {
                mimir.tag(obj_id, "interface");
            } else {
                mimir.tag(obj_id, "implementation");
            }
        }
        Some("toml") if path.file_name() == Some("Cargo.toml".as_ref()) => {
            mimir.tag(obj_id, "build-script");
            mimir.set_attr(obj_id, "lang", "toml");
        }
        Some("S" | "s" | "asm") => {
            mimir.set_attr(obj_id, "lang", "asm");
            mimir.tag(obj_id, "implementation");
        }
        Some("ld" | "lds") => {
            mimir.tag(obj_id, "linker-script");
        }
        Some("md" | "txt" | "rst") => {
            mimir.tag(obj_id, "documentation");
        }
        _ => {}
    }
    
    // Path conventions → role tags
    let path_str = path.to_str().unwrap_or("");
    if path_str.starts_with("src/") { mimir.tag(obj_id, "source"); }
    if path_str.starts_with("test") { mimir.tag(obj_id, "test"); }
    if path_str.starts_with("bench") { mimir.tag(obj_id, "bench"); }
    if path_str.starts_with("doc") { mimir.tag(obj_id, "documentation"); }
    if path_str.contains("/target/") || path_str.starts_with("build/") {
        mimir.tag(obj_id, "generated");
    }
    
    // Name-based conventions
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if name == "build.rs" { mimir.tag(obj_id, "build-script"); }
    if name.starts_with('.') { mimir.tag(obj_id, "config"); }
    if name == "README.md" { mimir.tag(obj_id, "documentation"); }
    if name == "LICENSE" { mimir.tag(obj_id, "documentation"); }
}
```

## Multiple Projections for One Project

This is where the design becomes powerful. The same set of objects can have completely different Unix layouts for different purposes:

```
Object: vesper-kernel-binary

  path-context:project-tree
    unix-path="target/aarch64-unknown-none/release/vesper"
    
  path-context:rpi4-sdcard
    unix-path="/boot/kernel8.img"
    unix-mode=0o755
    
  path-context:debian-package
    unix-path="usr/lib/vesper/kernel"
    unix-mode=0o755
    
  path-context:docker-image
    unix-path="/vesper/kernel"
    
  path-context:jtag-debug
    unix-path="/tftp/vesper.elf"        ← with debug symbols, different object!
```

Each context is an independent projection. Generating any of these is just reading the manifest and writing the archive. No file copying, no directory restructuring — the same blob on disk serves all projections.

## FUSE Bridge

If you ever need a live Unix view (for tools that refuse to work without paths), the FUSE shim reads from a path context:

```rust
// Mount a projection as a Unix filesystem
// brunnr mount-unix --context project-vesper /mnt/vesper-src

struct FuseProjection {
    context: String,
    // Path → ObjectId lookup (built from manifest at mount time)
    path_to_object: HashMap<PathBuf, ObjectId>,
    // Directory listings (synthesized)
    directories: HashMap<PathBuf, Vec<DirEntry>>,
}

impl fuser::Filesystem for FuseProjection {
    fn lookup(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: ReplyEntry) {
        let parent_path = self.inode_to_path(parent);
        let child_path = parent_path.join(name);
        
        if let Some(&obj_id) = self.path_to_object.get(&child_path) {
            let record = object_table.get(obj_id);
            reply.entry(&TTL, &make_attr(record, obj_id), 0);
        } else if self.directories.contains_key(&child_path) {
            reply.entry(&TTL, &make_dir_attr(&child_path), 0);
        } else {
            reply.error(ENOENT);
        }
    }
    
    fn read(&mut self, _req: &Request, ino: u64, offset: i64, size: u32, ...) {
        let obj_id = self.inode_to_object(ino);
        let data = mimir.read_blob_range(obj_id, offset as u64, size as u64);
        reply.data(&data);
    }
    
    // Write goes through Mímisbrunnr — blob updated, tags preserved
    fn write(&mut self, _req: &Request, ino: u64, offset: i64, data: &[u8], ...) {
        let obj_id = self.inode_to_object(ino);
        mimir.write_blob_range(obj_id, offset as u64, data);
        reply.written(data.len() as u32);
    }
}
```

## CLI

```
# Create a projection
mimir project create-context project-vesper \
    --description "Vesper kernel source tree"

# Add files to it (one at a time or from query)
mimir project set-path obj:42 project-vesper "src/kernel/main.rs"
mimir project set-path obj:43 project-vesper "src/kernel/boot.S"

# Bulk: project all source files using their name attribute
mimir project auto-path project-vesper \
    --query "project=vesper AND source" \
    --template "src/{component}/{name}"

# Generate tarball
mimir project export project-vesper --format tar.gz -o vesper-src.tar.gz
mimir project export rpi4-sdcard --format tar -o sdcard.tar
mimir project export debian-package --format deb -o vesper_0.1.0_arm64.deb

# Import from existing tree
mimir project import ./vesper-checkout \
    --context project-vesper \
    --project vesper \
    --auto-tag

# Mount as FUSE
brunnr mount-unix --context project-vesper /mnt/vesper

# List all projections
mimir project contexts
  CONTEXT           OBJECTS  DESCRIPTION
  project-vesper    342      Vesper kernel source tree
  rpi4-sdcard       12       Raspberry Pi 4 SD card layout
  debian-package    28       Debian package contents
  docker-image      15       Docker container filesystem

# Show a projection's tree
mimir project tree rpi4-sdcard
  /boot/
    config.txt         (obj:100, 2.1KB, resource)
    kernel8.img        (obj:42,  2.3MB, executable stripped)
    bcm2711.dtb        (obj:88,  24KB, device-tree generated)
    initrd.img         (obj:95,  1.1MB, bundle)
```

The key principle: **Unix paths are a projection, not the truth.** The filesystem's native addressing is tags and queries. Unix paths are an export format — like how a 3D model is native geometry but can be projected onto a 2D blueprint for traditional manufacturing.
