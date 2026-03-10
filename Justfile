# Mímisbrunnr — The Well of Knowledge
# Run recipes in order to explore all supported features.
#
# Quick start:
#   just demo          # run the full demo end-to-end
#   just clean-demo    # clean up demo artifacts

# ── Configuration ────────────────────────────────────────────────────

pool_dir   := justfile_directory() / ".demo-pool"
pool_toml  := pool_dir / "pool.toml"
fuse_dir   := justfile_directory() / "crates"
fuse_ctx   := "mimisbrunnr-types-src"
mimir      := "cargo run --quiet --bin mimir -- --pool " + pool_toml
brunnr     := "cargo run --quiet --bin brunnr --"
populate   := "cargo run --quiet --bin populate -- --pool " + pool_toml

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
    {{brunnr}} create \
        "{{pool_dir}}/nvme.mbrunnr" \
        "{{pool_dir}}/ssd.mbrunnr" \
        "{{pool_dir}}/hdd.mbrunnr" \
        --size-mib 128 \
        --tier hot --tier warm --tier cold

# Show pool status — zone layout, capacities, checkpoint LSN
pool-status:
    {{brunnr}} status \
        "{{pool_dir}}/nvme.mbrunnr" \
        "{{pool_dir}}/ssd.mbrunnr" \
        "{{pool_dir}}/hdd.mbrunnr"

# Add a fourth disk to the pool
pool-add-disk:
    {{brunnr}} add-disk \
        "{{pool_dir}}/archive.mbrunnr" \
        --pool-disk "{{pool_dir}}/nvme.mbrunnr" \
        --size-mib 128 \
        --tier glacier

# Show status after adding a disk
pool-status-after-add:
    {{brunnr}} status \
        "{{pool_dir}}/nvme.mbrunnr" \
        "{{pool_dir}}/ssd.mbrunnr" \
        "{{pool_dir}}/hdd.mbrunnr" \
        "{{pool_dir}}/archive.mbrunnr"

# ── Ontology Setup (mimir) ──────────────────────────────────────────

# Load ontology from TOML module file
ontology-load:
    @echo "=== Loading ontology module ==="
    {{mimir}} ontology load demo-data/demo-labels.toml

# List all registered tags and their implications
ontology-list:
    @echo "=== Ontology ==="
    {{mimir}} ontology list

# Full ontology setup
ontology-setup: ontology-load ontology-list

# ── Object Operations (mimir) ───────────────────────────────────────

# Use manifest to create objects and tag them — simulates adding music tracks
populate-objects:
    {{populate}} demo-data/demo-objects.toml

# ── Queries (mimir) ──────────────────────────────────────────────────

# Run a series of queries demonstrating the query algebra
run-queries:
    #!/usr/bin/env bash
    set -euo pipefail

    echo ""
    echo "=== Query: all electronic ==="
    {{mimir}} query "electronic"

    echo ""
    echo "=== Query: electronic AND ambient ==="
    {{mimir}} query "electronic AND ambient"

    echo ""
    echo "=== Query: all favorites ==="
    {{mimir}} query "favorite"

    echo ""
    echo "=== Query: electronic AND NOT ambient (non-ambient electronic) ==="
    {{mimir}} query "electronic AND NOT ambient"

    echo ""
    echo "=== Query: electronic AND NOT discontinued ==="
    {{mimir}} query "electronic AND NOT discontinued"

    echo ""
    echo "=== Query: all media (tests implication: electronic→media, rock→media, etc.) ==="
    {{mimir}} query "media"

    echo ""
    echo "=== Query: artist=\"Aphex Twin\" ==="
    {{mimir}} query 'artist="Aphex Twin"'

    echo ""
    echo "=== Query: year=1998 ==="
    {{mimir}} query "year:1998"

# ── Object Inspection ────────────────────────────────────────────────

# Show info on each object — all assertions, direct and materialized
inspect-objects:
    #!/usr/bin/env bash
    set -euo pipefail

    for i in 0 1 2 3 4 5; do
        echo ""
        echo "=== Object $i ==="
        {{mimir}} info --object $i
    done

# ── Untagging ────────────────────────────────────────────────────────

# Remove a tag and show the effect
demo-untag:
    #!/usr/bin/env bash
    set -euo pipefail

    echo "=== Before untag: favorites ==="
    {{mimir}} query "favorite"

    echo ""
    echo "=== Removing 'favorite' from object 2 (Pink Floyd) ==="
    {{mimir}} untag --object 2 favorite

    echo ""
    echo "=== After untag: favorites ==="
    {{mimir}} query "favorite"

# ── Project Import ───────────────────────────────────────────────────

# Import the mimisbrunnr source tree as a path projection
# Note: project tree is in same process as import (context is in-memory for now)
import-source:
    #!/usr/bin/env bash
    set -euo pipefail

    echo "=== Importing source tree ==="
    {{mimir}} project import {{fuse_dir}} --context {{fuse_ctx}}

# ── Full Demo ────────────────────────────────────────────────────────

# Run the complete demo: create pool → ontology → populate → query → inspect
demo: build pool-create pool-status ontology-setup populate-objects run-queries inspect-objects demo-untag import-source
    @echo ""
    @echo "═══════════════════════════════════════════════════"
    @echo "  Demo complete! Pool is at: {{pool_dir}}"
    @echo "  Try running queries yourself:"
    @echo "    {{mimir}} query \"electronic AND ambient\""
    @echo "    {{mimir}} info --object 0"
    @echo "    {{mimir}} ontology list"
    @echo "═══════════════════════════════════════════════════"

demo-sql: build pool-create ontology-setup populate-objects
    @echo ""
    @echo "═══════════════════════════════════════════════════"
    @echo "Entering Mimir SQL REPL. Type .help for info"
    @echo "═══════════════════════════════════════════════════"
    {{mimir}} sql

demo-fuse: build pool-create ontology-setup populate-objects import-source
    @echo ""
    @echo "═══════════════════════════════════════════════════"
    @echo " cd /tmp/mbrunnr-test and cd and ls around"
    @echo "═══════════════════════════════════════════════════"
    @echo ""
    # Mount it as a FUSE filesystem
    -{{brunnr}} mount-unix /tmp/mbrunnr-test --pool {{pool_toml}} --context {{fuse_ctx}}
    # Unmount after exiting FUSE driver
    diskutil unmount /tmp/mbrunnr-test

# Clean up demo artifacts
clean-demo:
    rm -rf "{{pool_dir}}"

# Clean everything (cargo + demo)
clean: clean-demo
    cargo clean
