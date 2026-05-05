## Framing

`unix-path` is not how the file _lives_ in Mímisbrunnr. It's metadata about how this file
_appears_ when projected onto a Unix system. The same object can have different
`unix-path` values for different contexts — a source file has one path in the project tree,
a different path in the installed system, maybe a third in a container image.

For example, a `bash` binary that appears at multiple Unix paths is just a single object
carrying multiple `unix-path` attributes:

```rust
// /usr/bin/bash and /bin/bash are the same object — multi-path.
Attr { key: unix-path, value: "/bin/bash" }
Attr { key: unix-path, value: "/usr/bin/bash" }
```

**No symlinks.** Unix needs symlinks because each inode can hold only one path; Mímisbrunnr
objects can hold many. An object that should appear at several paths simply asserts several
`unix-path` values — the hardlink semantics, applied uniformly. No `unix-symlink-target`,
no symlink kind, no special framing. When projecting *out* to a Unix tree, the projector
picks one of the paths as the real file and emits the others as hardlinks (or, if a target
filesystem doesn't support hardlinks, as duplicate copies).

## Per-context paths

Paths can be **scoped by context** so the same object has different placements in different
projections. A context is just a tag with `Grouping` semantics; scoping is done with
`Value::Scoped { context, inner }` (IMPLEMENTATION.md §4.3 / §10.3):

```rust
Attr { key: unix-path, value: Scoped(project-vesper, "src/kernel/main.rs") }
Attr { key: unix-path, value: Scoped(install-rpi4,   "/boot/vesper")        }
Attr { key: unix-path, value: Scoped(package-deb,    "usr/bin/vesper")      }
```

Per-context overrides for `unix-mode`, `unix-uid`, `unix-gid` use the same `Scoped` form.
Unscoped values (plain `Text(...)`) are the default — visible in every context.

## Ontology module

Path projections are defined by a small ontology module shipped with the FS:

```toml
# systems.metta.unix-interop.toml

[[tags]]
name = "unix-path-context"
semantics = "grouping"
description = "A named Unix filesystem projection"
# Examples: unix-path-context:project-vesper, unix-path-context:rpi4-sdcard,
#           unix-path-context:debian-package, unix-path-context:docker-image

[[tags]]
name = "unix-path"
semantics = "attribute"
value_type = "text"
description = "Path under which this object appears in a Unix projection. May appear multiple times on one object (multi-path / hardlink semantics) and may be scoped to a unix-path-context."

[[tags]]
name = "unix-mode"
semantics = "attribute"
value_type = "int"
description = "Optional Unix permission bits. May be scoped to a unix-path-context for per-context overrides."

[[tags]]
name = "unix-uid"
semantics = "attribute"
value_type = "int"
description = "Optional Unix owner uid. May be scoped to a unix-path-context."

[[tags]]
name = "unix-gid"
semantics = "attribute"
value_type = "int"
description = "Optional Unix owner gid. May be scoped to a unix-path-context."
```

Scoping is transparent to the ontology's `value_type` check — a `Scoped` value is validated
against its inner (so `unix-path` accepts both `Text("/usr/bin/bash")` and
`Scoped(rpi4, Text("/boot/vesper"))`). The validator additionally requires that the scope
`context` is a tag with `Grouping` semantics.

## What the data looks like

```
Object: vesper (the kernel binary)
  project=vesper  component=kernel
  executable  stripped  current
  target=aarch64-unknown-none

  unix-path = "/boot/vesper"           # default (unscoped)
  unix-mode = 0o755
  unix-uid  = 0
  unix-gid  = 0

  unix-path = Scoped(project-vesper, "target/aarch64-unknown-none/release/vesper")

  unix-path = Scoped(package-deb, "usr/lib/vesper/vesper")
  unix-mode = Scoped(package-deb, 0o755)

Object: bash
  executable  shell  system

  unix-path = "/bin/bash"              # both paths are first-class
  unix-path = "/usr/bin/bash"          # no symlink — same object, multiple paths
  unix-mode = 0o755
```

There is no directory object. When projecting to Unix, intermediate directories like
`/usr/bin/` are **virtual** — synthesised from the paths at projection time, not stored
in Mímisbrunnr.

## Generating a tarball

The primary export use case. Build the manifest by intersecting the context's tag bitmap
with `unix-path` assertions, then walk:

```rust
fn export_tarball(context: &str, output: &Path) -> Result<()> {
    let context_tag = resolve_tag(format!("unix-path-context:{}", context));

    // Members of this context, in user-defined order via the §8.3 Ordered tag-store.
    let members = mimir.tag_members_ordered(context_tag);

    // Synthesise virtual directories from the path set.
    let mut dirs = BTreeSet::new();
    let mut entries = Vec::new();

    for oid in members {
        // Pull this object's path (and per-context overrides) for this context.
        let path = mimir.get_attr_scoped(oid, "unix-path", context_tag)
                        .or_else(|| mimir.get_attr(oid, "unix-path"))
                        .expect("member with no unix-path");
        let mode = mimir.get_attr_scoped(oid, "unix-mode", context_tag)
                        .or_else(|| mimir.get_attr(oid, "unix-mode"))
                        .unwrap_or(0o644);
        // ...uid, gid likewise...

        // Collect parent directories.
        let mut cur = PathBuf::from(&path);
        while let Some(parent) = cur.parent() {
            if parent.as_os_str().is_empty() { break; }
            dirs.insert(parent.to_path_buf());
            cur = parent.to_path_buf();
        }
        entries.push((oid, path, mode /*, uid, gid */));
    }

    let mut tar = tar::Builder::new(File::create(output)?);

    // Directories first.
    for dir in &dirs { tar.append_directory(dir, 0o755)?; }

    // Files. An object with multiple unix-path values is emitted once as a regular
    // file (the lexicographically-first path wins) and the rest as tar hardlinks.
    let mut emitted: HashMap<ObjectId, &Path> = HashMap::new();
    for (oid, path, mode) in &entries {
        if let Some(real) = emitted.get(oid) {
            tar.append_link(path, real)?;          // hardlink
        } else {
            let blob = mimir.read_blob(*oid)?;
            tar.append_file(path, *mode, &blob)?;
            emitted.insert(*oid, path);
        }
    }

    tar.finish()?;
    Ok(())
}
```

## Importing from Unix

The reverse — ingesting a directory tree:

```rust
fn import_tree(source: &Path, context: &str, project: &str, auto_tag: bool) -> Result<ImportReport> {
    let context_tag = resolve_or_create_tag(format!("unix-path-context:{}", context));
    let mut report = ImportReport::new();

    for entry in WalkDir::new(source) {
        let entry = entry?;
        let rel_path = entry.path().strip_prefix(source)?.to_string_lossy().into_owned();

        // Dedup by content hash.
        let content = fs::read(entry.path())?;
        let hash = blake3::hash(&content);
        let oid = match mimir.find_by_content_hash(hash) {
            Some(existing) => { report.deduplicated += 1; existing }
            None           => { report.created += 1; mimir.create_object(&content)? }
        };

        // Tag with context, set scoped path attribute.
        mimir.tag(oid, context_tag);
        mimir.set_attr(oid, "unix-path",
            Value::Scoped { context: context_tag, inner: Box::new(Text(rel_path)) });
        // ...mode, uid, gid likewise scoped...

        if auto_tag { auto_tag_from_path(oid, entry.path(), project); }
    }

    Ok(report)
}
```

(`auto_tag_from_path` infers `lang=*`, `source` / `test` / `documentation` etc. from the
path and extension — orthogonal to the projection mechanism.)

## Multiple projections for one object

The whole point: the same object has completely different Unix layouts in different
contexts. Each context is just a tag; each scoped attribute is just a value:

```
Object: vesper-kernel-binary  (release build, stripped)

  unix-path = Scoped(project-tree,   "target/aarch64-unknown-none/release/vesper")
  unix-path = Scoped(rpi4-sdcard,    "/boot/kernel8.img")
  unix-mode = Scoped(rpi4-sdcard,    0o755)
  unix-path = Scoped(debian-package, "usr/lib/vesper/kernel")
  unix-mode = Scoped(debian-package, 0o755)
  unix-path = Scoped(docker-image,   "/vesper/kernel")
```

A debug-symbols build is a **different object** with a different content hash and its own
projections — never a "debug path on the release object":

```
Object: vesper-kernel-debug  (unstripped, with DWARF)

  unix-path = Scoped(jtag-debug, "/tftp/vesper.elf")
```

Generating any projection is reading the context's ordered manifest and emitting the
archive. No file copying, no directory restructuring — the same blob serves every
projection it appears in.

## FUSE bridge

For tools that refuse to work without real paths, mount a projection as a Unix filesystem.
The FUSE shim builds its `path → ObjectId` map by enumerating the context tag's members
and reading each member's `unix-path` attributes (scoped to the context, falling back to
unscoped). Reads and writes pass through to the underlying object; the path is just an
index into the projection.

(Implementation lives in `crates/mimisbrunnr-fuse/`; design notes there.)

## CLI

```
# Create a projection (just creates the context tag).
mimir context create project-vesper \
    --description "Vesper kernel source tree"

# Add files (set scoped path attribute).
mimir context set-path obj:42 project-vesper "src/kernel/main.rs"
mimir context set-path obj:43 project-vesper "src/kernel/boot.S"

# Bulk: project source files using a template over their attributes.
mimir context auto-path project-vesper \
    --query "project=vesper AND source" \
    --template "src/{component}/{name}"

# Export.
mimir context export project-vesper  --format tar.gz -o vesper-src.tar.gz
mimir context export rpi4-sdcard     --format tar    -o sdcard.tar
mimir context export debian-package  --format deb    -o vesper_0.1.0_arm64.deb

# Import.
mimir context import ./vesper-checkout \
    --context project-vesper \
    --project vesper \
    --auto-tag

# Mount.
brunnr mount-unix --context project-vesper /mnt/vesper

# List.
mimir context list
  CONTEXT           OBJECTS  DESCRIPTION
  project-vesper    342      Vesper kernel source tree
  rpi4-sdcard       12       Raspberry Pi 4 SD card layout
  debian-package    28       Debian package contents
  docker-image      15       Docker container filesystem

# Show a projection's tree.
mimir context tree rpi4-sdcard
  /boot/
    config.txt         (obj:100, 2.1 KB, resource)
    kernel8.img        (obj:42,  2.3 MB, executable stripped)
    bcm2711.dtb        (obj:88,  24 KB,  device-tree generated)
    initrd.img         (obj:95,  1.1 MB, bundle)
```

## Storage

There is no bespoke "path context" data structure on disk. Everything above is encoded as
ordinary tag/attribute assertions: members of a context carry the `unix-path-context:*`
tag, paths are `Attr(unix-path, Value::Scoped { context, inner: Text(...) })` on the
forward index, and the ordered manifest reuses the existing §8.3 `Ordered` tag-store. See
`IMPLEMENTATION.md` §10.3 for the encoding contract.

The key principle: **Unix paths are a projection, not the truth.** The filesystem's
native addressing is tags and queries. Unix paths are an export format — like how a 3D
model is native geometry but can be projected onto a 2D blueprint for traditional
manufacturing.
