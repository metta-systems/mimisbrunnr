# Tooling, tests and governance — design

Scope: `bin/analyze`, `bin/populate`, `validation/` (layout-check crate), the `Justfile`, workspace-level test targets, source `TODO` markers.

Plan: [tooling-implementation-plan.md](tooling-implementation-plan.md) · Open decisions: [tooling-decisions.md](tooling-decisions.md) · Completed: [tooling-completed.md](tooling-completed.md) · Order: [operations-plan.md](operations-plan.md)

## Goal

Every claim in the plans can be checked by a command: unit and integration tests, end-to-end CLI runs against real file-backed pools, on-disk invariant checks, and layout assertions against the spec.

## Selected design

- **Tooling is Rust.** `just` recipes are the entry points.
- **Evidence kinds** are defined in [operations-design.md](operations-design.md#evidence-kinds).
- **TODO markers name plan items:** `TODO(BTREE4)`. Every marker resolves to exactly one open item.
- `bin/analyze` opens pools read-only and is the inspection tool for on-disk state.
