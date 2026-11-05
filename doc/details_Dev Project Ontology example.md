This is a great test case for the ontology system because source code projects have deep structure — files have roles, dependencies, build configurations, and lifecycle states that hierarchical folders flatten into lossy conventions like `src/`, `build/`, `dist/`.

## The Problem with Directories

A conventional project layout encodes multiple dimensions into a single tree:

```
project/
  src/           ← role (source)
    lib/          ← sub-role (library code)
    bin/          ← sub-role (executable entry points)
  tests/          ← role (test)
  build/          ← role (generated) + lifecycle (intermediate)
    debug/        ← configuration
      x86_64/     ← target
    release/      ← configuration
      x86_64/     ← target
      aarch64/    ← target
  dist/           ← role (generated) + lifecycle (final)
  docs/           ← role (documentation)
  assets/         ← role (resource)
```

This conflates role, lifecycle, target, and configuration into a path. You can't ask "show me everything related to the aarch64 build" without knowing the path convention. You can't ask "show me all generated files regardless of configuration" without globbing.

## The Ontology

```toml
# org.metta.dev.toml

[module]
id = "org.metta.dev"
version = "1.0.0"
name = "Software Development Ontology"
description = "Tags for source code projects, builds, and distribution"

[requires]
"org.metta.core" = ">=1.0"
```

### Dimension 1: Project Identity

Every file belongs to a project, and optionally to a component within it:

```toml
[[tags]]
name = "project"
semantics = "attribute"
value_type = "text"
description = "Project this file belongs to"
# project=vesper, project=mimisbrunnr

[[tags]]
name = "component"
semantics = "attribute"
value_type = "text"
description = "Component/crate/package within a project"
# component=kernel, component=hal, component=bootloader

[[tags]]
name = "workspace"
semantics = "grouping"
description = "Multi-project workspace"
# workspace=metta-systems
```

### Dimension 2: File Role

What _purpose_ does this file serve? This is the deepest hierarchy:

```toml
# ─── Root roles ───

[[tags]]
name = "source"
semantics = "label"
description = "Human-authored source material"

[[tags]]
name = "generated"
semantics = "label"
description = "Machine-produced output"

[[tags]]
name = "vendored"
semantics = "label"
description = "Third-party code copied into project"

# ─── Source sub-roles ───

[[tags]]
name = "implementation"
semantics = "label"
description = "Core implementation source"

[[tags]]
name = "interface"
semantics = "label"
description = "Public API / header / trait definitions"

[[tags]]
name = "test"
semantics = "label"
description = "Test source code"

[[tags]]
name = "bench"
semantics = "label"
description = "Benchmark source code"

[[tags]]
name = "build-script"
semantics = "label"
description = "Build system definitions"
# Cargo.toml, build.rs, Makefile, CMakeLists.txt, meson.build

[[tags]]
name = "config"
semantics = "label"
description = "Configuration files"
# .cargo/config.toml, rustfmt.toml, clippy.toml

[[tags]]
name = "documentation"
semantics = "label"
description = "Documentation source"

[[tags]]
name = "resource"
semantics = "label"
description = "Non-code assets consumed by the build or runtime"

[[tags]]
name = "spec"
semantics = "label"
description = "Specification or design document"

# ─── Generated sub-roles ───

[[tags]]
name = "object"
semantics = "label"
description = "Compiled object file (.o, .rlib)"

[[tags]]
name = "library"
semantics = "label"
description = "Linked library"

[[tags]]
name = "executable"
semantics = "label"
description = "Final linked binary"

[[tags]]
name = "bundle"
semantics = "label"
description = "Distribution package (tarball, .deb, .img, .iso)"

[[tags]]
name = "codegen"
semantics = "label"
description = "Generated source code (proto stubs, macro expansion, bindings)"

[[tags]]
name = "doc-output"
semantics = "label"
description = "Generated documentation (rustdoc, doxygen HTML)"

[[tags]]
name = "test-result"
semantics = "label"
description = "Test output, coverage reports, logs"

[[tags]]
name = "intermediate"
semantics = "label"
description = "Build intermediate (dep-info, incremental cache, stamps)"

# ─── Resource sub-types ───

[[tags]]
name = "icon"
semantics = "label"

[[tags]]
name = "font"
semantics = "label"

[[tags]]
name = "texture"
semantics = "label"

[[tags]]
name = "shader"
semantics = "label"

[[tags]]
name = "locale"
semantics = "label"
description = "Translation / i18n files"

[[tags]]
name = "schema"
semantics = "label"
description = "Database schemas, protobuf definitions, JSON schemas"

[[tags]]
name = "linker-script"
semantics = "label"
description = "Linker scripts (.ld, .lds)"

[[tags]]
name = "device-tree"
semantics = "label"
description = "Device tree sources and compiled blobs"
```

### Dimension 3: Language and Toolchain

```toml
[[tags]]
name = "lang"
semantics = "attribute"
value_type = "text"
description = "Programming language"
# lang=rust, lang=c, lang=asm, lang=python, lang=toml

[[tags]]
name = "toolchain"
semantics = "attribute"
value_type = "text"
description = "Toolchain that produced this artifact"
# toolchain=nightly-2024-01-15, toolchain=gcc-13.2
```

### Dimension 4: Build Configuration

```toml
[[tags]]
name = "profile"
semantics = "attribute"
value_type = "text"
description = "Build profile"
# profile=debug, profile=release, profile=release-with-debug

[[tags]]
name = "target"
semantics = "attribute"
value_type = "text"
description = "Target triple"
# target=aarch64-unknown-none, target=x86_64-unknown-linux-gnu

[[tags]]
name = "features"
semantics = "attribute"
value_type = "text"
description = "Enabled feature flags"
# features=no_std, features=verbose-boot

[[tags]]
name = "opt-level"
semantics = "attribute"
value_type = "int"
description = "Optimization level (0-3, s, z)"

[[tags]]
name = "debug-info"
semantics = "label"
description = "Contains debug information"

[[tags]]
name = "lto"
semantics = "label"
description = "Built with link-time optimization"

[[tags]]
name = "stripped"
semantics = "label"
description = "Debug symbols removed"
```

### Dimension 5: Lifecycle State

```toml
[[tags]]
name = "dirty"
semantics = "label"
description = "Generated file whose source has been modified since build"

[[tags]]
name = "stale"
semantics = "label"
description = "Generated file from an older build, superseded"

[[tags]]
name = "current"
semantics = "label"
description = "Up-to-date generated artifact"

[[tags]]
name = "released"
semantics = "label"
description = "Artifact that was published/distributed"

[[tags]]
name = "pinned"
semantics = "label"
description = "Artifact explicitly kept (not cleaned by gc)"
```

### Dimension 6: Dependencies and Derivation

This is where relations shine — tracking _what came from what_:

```toml
[[tags]]
name = "derived-from"
semantics = "relation"
description = "This artifact was produced from these sources"
# object:main.o derived-from source:main.rs
# object:main.o derived-from source:lib.rs  (multiple sources)

[[tags]]
name = "links"
semantics = "relation"
description = "This artifact links against this library"
# executable:vesper links library:libhal.a
# executable:vesper links library:libcore.rlib

[[tags]]
name = "depends-on"
semantics = "relation"
description = "Build dependency (source-level)"
# source:main.rs depends-on source:config.rs

[[tags]]
name = "bundles"
semantics = "relation"
description = "This package contains these artifacts"
# bundle:vesper-0.1.0.img bundles executable:vesper
# bundle:vesper-0.1.0.img bundles resource:device-tree.dtb
```

### Implication Graph

```toml
# Role hierarchy
[[implications]]
from = "implementation"
to = "source"

[[implications]]
from = "interface"
to = "source"

[[implications]]
from = "test"
to = "source"

[[implications]]
from = "bench"
to = "source"

[[implications]]
from = "build-script"
to = "source"

[[implications]]
from = "config"
to = "source"

[[implications]]
from = "documentation"
to = "source"

[[implications]]
from = "spec"
to = "source"

[[implications]]
from = "resource"
to = "source"

[[implications]]
from = "object"
to = "generated"

[[implications]]
from = "library"
to = "generated"

[[implications]]
from = "executable"
to = "generated"

[[implications]]
from = "bundle"
to = "generated"

[[implications]]
from = "codegen"
to = "generated"

[[implications]]
from = "doc-output"
to = "generated"

[[implications]]
from = "test-result"
to = "generated"

[[implications]]
from = "intermediate"
to = "generated"

# Resource sub-types
[[implications]]
from = "icon"
to = "resource"

[[implications]]
from = "font"
to = "resource"

[[implications]]
from = "texture"
to = "resource"

[[implications]]
from = "shader"
to = "resource"

[[implications]]
from = "locale"
to = "resource"

[[implications]]
from = "schema"
to = "resource"

[[implications]]
from = "linker-script"
to = "resource"

[[implications]]
from = "device-tree"
to = "resource"

# Language → file type chains
[[implications]]
from = "lang:rust"
to = "source"

[[implications]]
from = "lang:c"
to = "source"

[[implications]]
from = "lang:asm"
to = "source"

# Lifecycle mutual exclusion
[[mutex_groups]]
tags = ["dirty", "current", "stale"]

[[mutex_groups]]
tags = ["debug-info", "stripped"]

# Constraints
[[constraints]]
tag = "profile"
requires = "generated"

[[constraints]]
tag = "target"
requires = "generated"

[[constraints]]
tag = "opt-level"
requires = "generated"

[[constraints]]
tag = "lto"
requires = "generated"

[[constraints]]
tag = "derived-from"
requires = "generated"

[[constraints]]
tag = "links"
requires = "generated"

[[constraints]]
tag = "dirty"
requires = "generated"

[[constraints]]
tag = "stale"
requires = "generated"

[[constraints]]
tag = "current"
requires = "generated"
```

## The Implication DAG Visualized

```
                              file (core ontology)
                            ╱          ╲
                     source              generated
                   ╱  │  │  ╲          ╱   │    │    ╲
           implement- │  │ resource  object │  library executable  bundle
           ation      │  │  ╱│╲        codegen  │
                   inter-│ icon│shader     doc-output
                   face  │  font│locale       test-result
                      test  texture           intermediate
                      bench   schema
                   build-script
                      config
                   documentation
                        spec
                      linker-script
                      device-tree
```

## What a Real Project Looks Like

A Vesper kernel build, fully tagged:

```
Object: boot.S
  project=vesper  component=bootloader  lang=asm
  implementation  interface
  name="boot.S"

Object: main.rs
  project=vesper  component=kernel  lang=rust
  implementation
  name="main.rs"

Object: kernel_api.rs
  project=vesper  component=kernel  lang=rust
  implementation  interface
  name="kernel_api.rs"

Object: Cargo.toml
  project=vesper  component=kernel  lang=toml
  build-script
  name="Cargo.toml"

Object: vesper.ld
  project=vesper  component=kernel
  linker-script
  name="vesper.ld"

Object: bcm2711-rpi-4-b.dts
  project=vesper  component=hal
  device-tree  source
  target=aarch64-unknown-none
  name="bcm2711-rpi-4-b.dts"

Object: bcm2711-rpi-4-b.dtb
  project=vesper  component=hal
  device-tree  generated  current
  target=aarch64-unknown-none
  derived-from→ bcm2711-rpi-4-b.dts
  name="bcm2711-rpi-4-b.dtb"

Object: main.o
  project=vesper  component=kernel
  object  current
  target=aarch64-unknown-none  profile=release  opt-level=2  lto
  toolchain=nightly-2024-01-15
  derived-from→ main.rs
  derived-from→ kernel_api.rs
  name="main.o"

Object: libhal.rlib
  project=vesper  component=hal
  library  current
  target=aarch64-unknown-none  profile=release  opt-level=2
  toolchain=nightly-2024-01-15
  name="libhal.rlib"

Object: vesper (ELF)
  project=vesper  component=kernel
  executable  current  debug-info
  target=aarch64-unknown-none  profile=release  opt-level=2  lto
  toolchain=nightly-2024-01-15
  derived-from→ main.o
  links→ libhal.rlib
  name="vesper"

Object: vesper.stripped
  project=vesper  component=kernel
  executable  current  stripped
  target=aarch64-unknown-none  profile=release  opt-level=2  lto
  derived-from→ vesper (ELF)
  name="vesper.stripped"

Object: vesper-0.1.0-rpi4.img
  project=vesper
  bundle  current  released
  target=aarch64-unknown-none  profile=release
  bundles→ vesper.stripped
  bundles→ bcm2711-rpi-4-b.dtb
  bundles→ config.txt
  name="vesper-0.1.0-rpi4.img"
```

## Queries That Become Trivial

These would require complex globbing, `find` pipelines, or build system introspection in a traditional filesystem. Here they're single queries:

```
"All source files in the vesper kernel"
  project=vesper AND component=kernel AND source
  → immediate bitmap intersection

"All generated artifacts for aarch64 release"
  project=vesper AND generated AND target=aarch64-unknown-none AND profile=release
  → 4-way bitmap intersection

"Everything that went into the distribution image"
  Follow bundles relation from vesper-0.1.0-rpi4.img, recursively
  → transitive closure over derived-from and bundles

"What source files does the final executable depend on?" (full provenance)
  vesper.stripped
    derived-from→ vesper (ELF)
      derived-from→ main.o
        derived-from→ main.rs, kernel_api.rs
      links→ libhal.rlib
        derived-from→ ... (hal sources)

"All stale build artifacts I can clean"
  project=vesper AND (stale OR dirty) AND generated AND NOT pinned

"Object files built with an older toolchain"
  project=vesper AND object AND toolchain=nightly-2024-01-10

"Every file related to the HAL component, regardless of role"
  project=vesper AND component=hal
  → sources, objects, libraries, device trees, tests — everything

"All Rust source across all projects"
  lang=rust AND implementation

"What links against libhal?"
  Follow links relation (reverse) from libhal.rlib
  → all executables and libraries that depend on it

"Show me the full dependency graph of the release build"
  Start: project=vesper AND bundle AND profile=release
  Traverse: bundles, derived-from, links (reverse all)
  → complete build graph as a set of objects with relations
```

## Build System Integration

A build system (cargo, make, ninja) emits tagging operations as part of the build:

```rust
// Build system hook: after compiling a file
fn on_compile_complete(
    source_files: &[ObjectId],
    output: ObjectId, 
    build_ctx: &BuildContext,
) {
    // Tag the output with its build configuration
    mimir.tag(output, "object");
    mimir.tag(output, "current");
    mimir.set_attr(output, "target", &build_ctx.target);
    mimir.set_attr(output, "profile", &build_ctx.profile);
    mimir.set_attr(output, "opt-level", build_ctx.opt_level);
    mimir.set_attr(output, "toolchain", &build_ctx.toolchain);
    mimir.set_attr(output, "project", &build_ctx.project);
    mimir.set_attr(output, "component", &build_ctx.component);
    
    if build_ctx.lto { mimir.tag(output, "lto"); }
    if build_ctx.debug_info { mimir.tag(output, "debug-info"); }
    
    // Record provenance
    for src in source_files {
        mimir.add_relation(output, "derived-from", *src);
    }
    
    // Mark previous build of same source as stale
    let previous = mimir.query(And(vec![
        HasTag(tag("object")),
        HasAttr { key: "component", op: Eq, value: build_ctx.component },
        HasTag(tag("current")),
    ]));
    for old in previous.iter() {
        if old != output {
            mimir.remove_tag(old, "current");
            mimir.tag(old, "stale");
        }
    }
}
```

### Clean Operations

Build cleaning becomes a query, not a path glob:

```
# Clean all generated files for debug aarch64
mimir query "project=vesper AND generated AND profile=debug 
             AND target=aarch64-unknown-none"
  | mimir delete --batch

# Clean only stale artifacts (keep current build)
mimir query "project=vesper AND stale AND NOT pinned"
  | mimir delete --batch

# Clean intermediates but keep final artifacts
mimir query "project=vesper AND intermediate"
  | mimir delete --batch

# Nuclear: clean all generated, all targets, all profiles
mimir query "project=vesper AND generated"
  | mimir delete --batch
```

### Rebuild Detection

Instead of filesystem timestamps (which break on clock skew, NFS, containers), use the content hash and derived-from relations:

```rust
fn needs_rebuild(artifact: ObjectId) -> bool {
    let artifact_record = object_table.get(artifact);
    
    // Walk derived-from to find all source inputs
    let sources = transitive_closure(artifact, "derived-from");
    
    for source in sources {
        let source_record = object_table.get(source);
        // If any source was modified after the artifact was built
        if source_record.modified_ns > artifact_record.modified_ns {
            return true;
        }
    }
    false
}

// Or more precisely: check if source content hash changed
fn needs_rebuild_hash(artifact: ObjectId) -> bool {
    let build_inputs = get_attr(artifact, "build-input-hashes");
    // Stored at build time: hash of each source that went into this artifact
    
    let current_sources = transitive_closure(artifact, "derived-from");
    for source in current_sources {
        let current_hash = object_table.get(source).content_hash;
        if build_inputs.get(source) != Some(current_hash) {
            return true;
        }
    }
    false
}
```

## Placement Rules for Dev Projects

The ontology naturally feeds into storage tiering:

```toml
# In pool configuration:

# Source code: always on fast storage, replicated
[[placement]]
query = "source AND NOT vendored"
rule = "pin"
tier = "hot"

[[placement]]
query = "source AND NOT vendored"
rule = "replicate"
min_replicas = 2

# Build intermediates: fast storage, no replication, evictable
[[placement]]
query = "intermediate OR object"
rule = "pin"
tier = "hot"

[[placement]]
query = "intermediate OR stale"
rule = "auto-evict"
days_unused = 7

# Release bundles: replicated, can be on warm storage
[[placement]]
query = "bundle AND released"
rule = "replicate"
min_replicas = 3

[[placement]]
query = "bundle AND released"
rule = "prefer"
tier = "warm"

# Vendored deps: cold storage, one copy is fine
[[placement]]
query = "vendored"
rule = "prefer"
tier = "cold"

# Test results: warm, auto-evict after 30 days
[[placement]]
query = "test-result"
rule = "auto-evict"
days_unused = 30
```

## Cross-Project Queries

Because the ontology is shared across projects, cross-project queries work naturally:

```
"All Rust libraries across all projects"
  lang=rust AND library
  
"All aarch64 executables I've ever built"
  executable AND target=aarch64-unknown-none

"Every project that depends on libcore"
  Follow links relation (reverse) from any object named "libcore*"
  → group by project attribute

"Total disk used by stale build artifacts across all projects"
  generated AND stale → sum(stored_size)

"Which projects have I been working on this week?"
  source AND modified > 7-days-ago → distinct(project)
```

- $ This last query — "what have I been working on recently?" — is something no traditional filesystem can answer without scanning every file's mtime recursively. In Mímisbrunnr it's a bitmap intersection with a range query on the modification timestamp.
