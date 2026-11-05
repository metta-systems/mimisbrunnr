# Mímisbrunnr — The Well of Knowledge
# Run recipes in order to explore all supported features.
#
# Quick start:
#   just demo          # run the full demo end-to-end
#   just clean-demo    # clean up demo artifacts

# ── Configuration ────────────────────────────────────────────────────

pool_dir    := justfile_directory() / ".demo-pool"
pool_toml   := pool_dir / "pool.toml"
disk_bytes  := "134217728"  # 128 MiB per disk
# Importing all of `crates/` overflows the WAL ring before mimir's single
# commit (no checkpoint cadence yet, JOURNAL4); the types crate fits.
fuse_dir    := justfile_directory() / "crates" / "types"
fuse_ctx    := "mimisbrunnr-types-src"
fuse_mount  := "/tmp/mbrunnr-test"
mimir       := "cargo run --quiet --bin mimir -- --pool " + pool_toml
brunnr      := "cargo run --quiet --bin brunnr --"
brunnr_fuse := "cargo run --quiet --features brunnr/fuse --bin brunnr --"
populate    := "cargo run --quiet --bin populate -- --pool " + pool_toml

_default:
    @just --list

# ── Build ────────────────────────────────────────────────────────────

# Build the entire workspace
build:
    cargo build --workspace

# Run all tests
test:
    cargo test --workspace

# Run clippy lints
clippy:
    cargo clippy --workspace

# ── Pool Management (brunnr) ─────────────────────────────────────────

# Create a fresh pool with three file-backed disks (hot/warm/cold)
pool-create:
    #!/usr/bin/env bash
    set -euo pipefail
    rm -rf "{{pool_dir}}"
    mkdir -p "{{pool_dir}}"
    {{brunnr}} create --config "{{pool_toml}}" \
        "{{pool_dir}}/nvme.mbrunnr:tier=hot:media=nvme:capacity={{disk_bytes}}" \
        "{{pool_dir}}/ssd.mbrunnr:tier=warm:media=ssd:capacity={{disk_bytes}}" \
        "{{pool_dir}}/hdd.mbrunnr:tier=cold:media=hdd:capacity={{disk_bytes}}"

# Show pool status — disks, tiers, capacities, checkpoint LSN
pool-status:
    {{brunnr}} status --config "{{pool_toml}}"

# Add a fourth disk to the pool
pool-add-disk:
    {{brunnr}} add-disk --config "{{pool_toml}}" \
        "{{pool_dir}}/archive.mbrunnr" \
        --tier glacier \
        --media hdd \
        --capacity {{disk_bytes}}

# Show status after adding a disk
pool-status-after-add: pool-status

# ── Ontology Setup (mimir) ──────────────────────────────────────────

# Install the demo ontology module
ontology-load:
    @echo "=== Installing ontology module ==="
    {{mimir}} ontology install demo-data/demo-labels.toml

# List installed ontology modules
ontology-list:
    @echo "=== Ontology ==="
    {{mimir}} ontology list

# Full ontology setup
ontology-setup: ontology-load ontology-list

# ── Object Operations (populate) ─────────────────────────────────────

# Create the music-collection objects (and generated content) from the manifest
populate-objects:
    {{populate}} demo-data/demo-objects.toml

# ── Queries (mimir) ──────────────────────────────────────────────────

# Run a series of queries demonstrating the query algebra
run-queries:
    #!/usr/bin/env bash
    set -euo pipefail

    run() {
        echo ""
        echo "=== Query: $1 ==="
        {{mimir}} query "$2"
    }

    run "all electronic" '(tag electronic)'
    run "electronic AND ambient" '(and (tag electronic) (tag ambient))'
    run "all favorites" '(tag favorite)'
    run "electronic AND NOT ambient" '(and (tag electronic) (not (tag ambient)))'
    run "electronic AND NOT discontinued" '(and (tag electronic) (not (tag discontinued)))'
    run "all media (implication: electronic→media, rock→media, …)" '(isa media)'
    run 'artist = "Aphex Twin"' '(attr artist = "Aphex Twin")'
    run "year = 1998" '(attr year = 1998)'

# ── Object Inspection ────────────────────────────────────────────────

# Show info on the manifest's six explicit objects — direct and materialized assertions
inspect-objects:
    #!/usr/bin/env bash
    set -euo pipefail

    for i in 1 2 3 4 5 6; do
        echo ""
        echo "=== Object obj:$i ==="
        {{mimir}} info "obj:$i"
    done

# ── Untagging ────────────────────────────────────────────────────────

# Remove a tag and show the effect
demo-untag:
    #!/usr/bin/env bash
    set -euo pipefail

    echo "=== Before untag: favorites ==="
    {{mimir}} query '(tag favorite)'

    echo ""
    echo "=== Removing 'favorite' from obj:3 (Pink Floyd) ==="
    {{mimir}} untag obj:3 favorite

    echo ""
    echo "=== After untag: favorites ==="
    {{mimir}} query '(tag favorite)'

# ── Project Import ───────────────────────────────────────────────────

# Import the mimisbrunnr-types source tree as a path projection
import-source:
    #!/usr/bin/env bash
    set -euo pipefail

    echo "=== Importing source tree ==="
    {{mimir}} project create-context {{fuse_ctx}}
    {{mimir}} project import "{{fuse_dir}}" --context {{fuse_ctx}}

# ── Full Demo ────────────────────────────────────────────────────────

# Run the complete demo: create pool → ontology → populate → query → inspect
demo: build pool-create pool-status ontology-setup populate-objects run-queries inspect-objects demo-untag import-source
    @echo ""
    @echo "═══════════════════════════════════════════════════"
    @echo "  Demo complete! Pool is at: {{pool_dir}}"
    @echo "  Try running queries yourself:"
    @echo "    {{mimir}} query '(and (tag electronic) (tag ambient))'"
    @echo "    {{mimir}} info obj:1"
    @echo "    {{mimir}} ontology list"
    @echo "    {{mimir}} sql"
    @echo "═══════════════════════════════════════════════════"

# Populate the demo pool and open the interactive SQL REPL
demo-sql: build pool-create ontology-setup populate-objects
    @echo ""
    @echo "═══════════════════════════════════════════════════"
    @echo "Entering Mimir SQL REPL. Type .help for info"
    @echo "═══════════════════════════════════════════════════"
    {{mimir}} sql

# Populate the demo pool and mount the imported source tree via FUSE (needs macFUSE / libfuse)
demo-fuse: build pool-create ontology-setup populate-objects import-source && demo-fuse-unmount
    @echo ""
    @echo "═══════════════════════════════════════════════════"
    @echo " cd {{fuse_mount}} and cd and ls around"
    @echo "═══════════════════════════════════════════════════"
    @echo ""
    # - `RUST_LOG=mimisbrunnr_engine=trace` — just engine operations
    # - `RUST_LOG=mimisbrunnr_fuse=trace` — just FUSE calls
    # - `RUST_LOG=mimisbrunnr_storage::file_device=trace` — just physical I/O
    # Mount it as a FUSE filesystem
    -env RUST_LOG=trace,mimisbrunnr_fuse=info,fuser=info {{brunnr_fuse}} mount-unix "{{fuse_mount}}" --config "{{pool_toml}}" --context {{fuse_ctx}}

# Unmount after exiting FUSE driver
demo-fuse-unmount:
    {{brunnr}} unmount "{{fuse_mount}}"

# Clean up demo artifacts
clean-demo:
    rm -rf "{{pool_dir}}"

# Clean everything (cargo + demo)
clean: clean-demo
    cargo clean
