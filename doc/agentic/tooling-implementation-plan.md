# Tooling, tests and governance — implementation plan

Design: [tooling-design.md](tooling-design.md) · Decisions: [tooling-decisions.md](tooling-decisions.md) · Order: [operations-plan.md](operations-plan.md)

## Plan hygiene


## Evidence

- [ ] **TOOLING3 — End-to-end CLI test target (`L3`).** A workspace test target that runs `brunnr create`, `mimir tag`, `mimir query` (and later snapshot and placement scenarios) against `FileBlockDevice` pools in a temporary directory and asserts the returned ObjectId sets. Hosts the end-to-end [acceptance scenarios](operations-design.md#acceptance-scenarios).
- [ ] **TOOLING4 — On-disk invariant checker (`DB`).** An `analyze` mode that walks a pool and verifies superblock copies, CRCs, region kinds, every `RootPointer` slot resolving to a live bucket with matching generation, and chain integrity; non-zero exit on violation.
- [ ] **TOOLING5 — Spec anchor check.** A Rust check that every `SCOPE-DESIGN§n` reference in `doc/`, `crates/`, `bin/` and `validation/` resolves to a numbered heading in `doc/agentic/<scope>-design.md`, run after any renumbering.

## analyze and populate

- [ ] **TOOLING6 — Implementation health metrics in `analyze`.** Pool Overview shows: percentage of `RootPointer` slots populated, live tree depth per index, WAL fill and journal pin pressure, dirty-node LSN lag, blob live bytes versus allocated, bucket fragmentation, reconcile queue depth by `WorkKind`, subscription drain backlog, snapshot depth and count, encryption coverage. Each metric lands with the scope that makes it meaningful.
- [ ] **TOOLING7 — Non-primary disk superblock view in `analyze`.** Needs a public superblock reader for secondary disks.
- [ ] **TOOLING8 — Gaussian blob sizes in `populate`.** Prerequisite: TOOLING-D1.
