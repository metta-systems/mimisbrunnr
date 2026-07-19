---
name: operations
description: Guide one incremental Mimisbrunnr work slice (storage, WAL, indices, engine, ontology, watch, pool, FUSE, CLI) using the four per-scope documents in docs/ (<scope>-design.md, <scope>-decisions.md, <scope>-implementation-plan.md, <scope>-completed.md); keep IMPLEMENTATION.md authoritative, gate on-disk format and spec decisions, use JJ and Justfile workflows, and leave the workspace green.
---

# Mimisbrunnr operations

## Start with the scope documents

- Load the `ponytail` skill (`.claude/skills/ponytail`) together with this one and follow it for the whole slice: the smallest change that fully solves the task, and a short reply that ends with what was skipped or not checked. It does not relax anything below: the spec, the decision gates, the tests and the documentation updates still apply in full.
- Load the `jj-vcs` skill before any version-control operation (required by `AGENTS.md`).
- Resolve every repository-relative path below against the **Mimisbrunnr repository root**, not this skill's directory (`.claude/skills/operations`).
- Every piece of work belongs to a **scope**: a named area of work such as `storage`, `btree`, `wal`, `snapshots`, `encryption` or `fuse`. Each scope has exactly four documents in `docs/`:

| Document | Holds | Never holds |
|---|---|---|
| `docs/<scope>-design.md` | The scope's selected design: goals, invariants, approved choices with their rationale, alternatives weighed, and links to the `IMPLEMENTATION.md` / `DESIGN.md` sections it realises. | Tasks, status, open questions. |
| `docs/<scope>-decisions.md` | **Unanswered** decisions only, plus a final **Future work** section. | Resolved decisions, recaps, implementation TODOs, audit chores. |
| `docs/<scope>-implementation-plan.md` | Checkbox tasks with stable IDs, prerequisites, acceptance criteria and validation commands, ordered by dependency. | Design rationale, finished items, narratives. |
| `docs/<scope>-completed.md` | One line per finished item: what exists and where it is exercised (paths, test names). | Narratives, dates, decision logs. |

- Always read **all four** documents for the selected scope before analysis or edits, plus the `docs/IMPLEMENTATION.md` and `docs/DESIGN.md` sections the design document links to. If the scope or its documents do not exist, say so and offer to create them (see "Creating a scope"); do not invent their contents.
- Read a neighbouring scope's documents when the slice crosses into it (for example `btree` work that changes how `index` persists). Link across scopes; never restate another scope's design or tasks.
- Preserve user and concurrent-agent edits to these documents. Make focused updates; never replace a document wholesale.

## Document rules

- **One home per fact.** Tasks live only in the implementation plan, open decisions only in the decisions document, selected design only in the design document. Other documents link to them instead of restating them.
- **Decisions lifecycle.** When the maintainer resolves a decision, record the choice in `<scope>-design.md` (and in `IMPLEMENTATION.md` / `DESIGN.md` when it changes the spec), add or adjust the dependent plan items, then delete the item from `<scope>-decisions.md`. Give decisions stable IDs (`<SCOPE>-D<n>`, e.g. `BTREE-D3`) and never reuse an ID.
- **Future work** (end of `<scope>-decisions.md`) is for ideas the project may or may not pursue. It needs no near-term decision: never present it as a blocker, never let it constrain current design or tests, never plan around it. Describe each idea only there; elsewhere at most a one-line link.
- **Finishing an item.** Remove it from the implementation plan and add a one-line marker to `<scope>-completed.md`. **NEVER extend or rewrite an existing marker** when later work builds on it: add a new marker. If an old marker became wrong, correct it in place with the smallest change and no narrative.
- **No history in documents.** No dated logs, "was/now", superseded text or change narratives anywhere: version control keeps the history.
- Distinguish selected design, proposals, open decisions and implementation status. Existing code and unchecked plan items are not approval.

### Creating a scope

- Create all four files together. The design document starts with the scope's goal and links to the governing spec sections; the plan starts with dependency-ordered sections; the decisions document starts with any open questions and an empty **Future work** section; the completed document starts empty.
- **Legacy plan documents** (`docs/R1_fix0*.md`, `docs/R1_sketch_*.md`, `docs/REWRITE_FOLLOWUP.md`) predate this layout. When a scope takes over part of them, move open items into the scope's plan, finished items into its completed file as one-line markers, open questions into its decisions file, then delete the moved text from the legacy document (leave a one-line link where readers would look for it). Do not keep two live copies of a task.
- `docs/REWRITE_CONTRACT.md` §1–§6 remain the workspace conventions (dependencies, cross-cutting choices, type ownership, on-disk struct discipline, CRC/magic, tests). Its phase-specific breakage policy (§7, §10) is obsolete: see "Validate and report".

## Specification authority

- **`docs/IMPLEMENTATION.md` is the authoritative on-disk and algorithmic spec**; `docs/DESIGN.md` is the authoritative system design. A scope design document realises them, never competes with them.
- **Spec compliance over shortcuts.** Implement the encoding, layout and algorithm the spec mandates even when a simpler alternative compiles and passes tests. When the spec-compliant path has a real gap, ship the compliant path, add a regression test for what works, and record the gap as a plan item with a `TODO(<scope>-<item-id>)` marker next to the code. Never silently swap encodings (for example packed-key sorted runs → CBOR runs).
- Treat disagreement between the spec, the scope design and the code as a discrepancy to resolve explicitly. A spec change is a decision (see below); never edit `IMPLEMENTATION.md` to match code without approval.
- `BlockKind`, `BtreeKind`, WAL op kinds, magic constants and other spec-pinned discriminants are never renumbered silently. An approved renumbering updates the spec first, then the code, layout assertions, `validation/` and tests together.
- `.haft/specs/` holds the Haft target/enabling-system specs. Scope documents do not restate them; when a slice produces evidence a SpecSection requires, mention it in the report.

## Keep the specifications current

When a slice changes or implements something `IMPLEMENTATION.md` or `DESIGN.md` describes, update the affected section in the same slice.

- **Write specification, not process.** No scope-document references, item or decision IDs, phase names (`R1c`, `Tier 2`), dates, "amended", "previously", "selected", "TODO" or editorial asides. State what the format or behaviour is.
- **Rewrite, don't append.** Replace stale text; never add correction notes or "formerly…" asides.
- **One home per fact.** Each layout, invariant and algorithm is defined once, in the section that owns it; other sections link to it.
- **Verify facts when touching a section.** Check struct sizes, field offsets (`// [a..b]`), magic values, discriminants, region sizes and zone offsets against the code and the `validation/` layout-check crate, and fix any error found anywhere in that section.
- Implementation gaps against the spec live in the plan and `TODO(<scope>-<item-id>)` markers, not in the spec text.

## No backwards compatibility

- Pools are disposable until the maintainer declares a format stable. An approved format change is a one-way break: recreate the pool. Add no migration readers, compatibility shims, legacy decode paths, deprecated aliases or format-version bumps.
- Update every consumer in the same slice: storage, owning crate, engine wiring, bins (`mimir`, `brunnr`, `populate`, `analyze`), tests, `validation/` and the spec. Do not leave the old representation alongside the new one.
- This does not authorise divergence from the spec: change the spec (with approval) first, then remove the old behaviour outright.

## Gate decisions

Before implementing a slice, name the decisions it depends on and confirm they are settled in `<scope>-design.md` or the spec. Leave unrelated decisions open in `<scope>-decisions.md` instead of blocking on them or guessing. Decisions that always need the maintainer:

- **On-disk format:** struct layouts and sizes, key and value encodings, codec choice (packed / VARINT / CBOR), region sizes and index-zone offsets, magic and discriminants, CRC coverage.
- **Recovery semantics:** WAL op kinds and payloads, checkpoint and replay rules, what is persisted versus rebuilt at boot.
- **Spec amendments** to `IMPLEMENTATION.md` or `DESIGN.md`.
- **Crate boundaries and type ownership** (`REWRITE_CONTRACT.md` §3), new workspace dependencies.
- **User-visible surface:** `mimir` / `brunnr` commands and flags, FUSE layout, SQL dialect, ontology module syntax.
- **Cluster semantics:** node identity, sync, conflict resolution, replication placement.

Present each decision as plain text in the conversation: the concrete question, lettered options with consequences, a recommendation, and the affected plan items. Do not use interactive selection forms; the maintainer cannot read truncated option text there. Then stop and wait for the typed reply.

**Never assume an answer the maintainer has not explicitly given.** Record a decision only for questions the reply names or unambiguously answers. A reply to a related point, a follow-up question, an "ok" to something else, or silence answers nothing and does not select the recommended option. When a reply covers some questions, record those, keep the rest open, and ask again for the remainder. If it is unclear whether a reply answers a question, ask.

## Standing practices

These apply to every slice and are never plan checkboxes.

- **One design authority per scope.** Record the maintainer's choices in `<scope>-design.md`; never start a second design document for the same scope.
- **Keep sketches labeled.** Proposals, sketches and unimplemented operations are clearly labeled as such in code and documents. When the code is reconciled, remove diagrams and comments that now conflict with it and look authoritative, subject to "Preserve design-intent comments".
- **TODO markers point at plan items.** Use `TODO(<scope>-<item-id>)` so every marker resolves to one plan item. Remove the marker when the item completes. Convert legacy `TODO(rewrite-phase-*)` markers in code you touch.
- **No silent stubs.** Never return empty data, fake success or default state from a persistence path to make code compile. An unimplemented path returns a defined error and has a plan item.

## Pick one incremental slice

1. Read the user's request, the scope's four documents and the linked spec sections.
2. Follow the real call chain: CLI (`bin/`) → `crates/engine` (`Engine`, `DiskEngine`) → owning crate (`index`, `meta`, `ontology`, `watch`, `pool`, `unix`…) → `crates/storage` (B+ tree regions, allocator, superblock) and `crates/wal`. Use Pathfinder tools when available (see `AGENTS.md`).
3. Distinguish code that is wired into production paths from code exercised only by tests. Presence of a function does not mean the engine calls it.
4. Select **one plan item** consistent with the request. Identify its prerequisites, decisions, acceptance criteria and affected crates. Do not execute the backlog unprompted; respect the plan's dependency order.
5. If the workspace does not build or its tests fail before you start, report it. Restoring a green workspace is the first slice unless the user directs otherwise.
6. After approval of any design change, update the spec and `<scope>-design.md` first, keep the plan consistent, then change code. Review-only work changes no checkboxes.
7. Implement the slice coherently across every affected crate: no half-migrated call sites, no two encodings of the same structure.
8. Validate, audit the diff, and mark only verified work complete. Leave partial or blocked items in the plan with the blocker recorded; never mark unrun tests as passed.

## Preserve design-intent comments

- Preserve existing comments and documentation describing intended behaviour, including sketches, diagrams, spec cross-references and commented-out pseudocode, even when the implementation is incomplete.
- Do not erase or rewrite them because the behaviour is unsupported, a decision is open, or the code differs. Do not replace intent with a stub warning or move it solely into another document.
- When clarification is needed, keep the original text and add a separate, clearly labeled status note. Preserving intent does not promote it to approved design.
- Ask before removing or substantively changing pre-existing design-intent comments. In the final diff audit, check for accidental comment loss and restore it.

## Correctness before representation optimization

- Get identity, durability, crash consistency and spec conformance right first. Do not keep current struct sizes, inline representations or region caps at the expense of a correct design; add fields, records or regions when needed (with format approval).
- Defer size and performance work to a separate, measured slice.
- Whenever a layout changes, update together: the `bytemuck` POD struct and its `// [a..b]` offsets, size assertions (`const _: () = assert!(size_of::<T>() == N)`), the `validation/` crate, region sizes and index-zone offset constants, CRC coverage, allocator and footprint accounting, every reader and writer, `bin/analyze`, and the spec tables. Never disable an assertion to make a change fit.

## Keep responsibilities separate

- **`types`:** pure logical types and small pure codecs (`Value`, `value_hash`, IDs). No I/O.
- **`storage`:** block device, superblock, zones, bucket allocation, B+ tree region envelopes and the packed-key codec. Mechanism only; it knows no index semantics.
- **`wal`:** log format, append, checkpoint, replay, journal pins.
- **Owning crates (`meta`, `index`, `ontology`, `watch`, `pool`, `unix`, `transform`):** their wire structs, key/value types and invariant-preserving operations, persisted through `storage` regions.
- **`query`, `sql`:** evaluation over indices; no persistence.
- **`engine`:** orchestration: WAL-first mutation, commit, flush, boot-time load and rebuild of derived views. It does not hide another crate's invariants.
- **`fuse`, `bin/`:** presentation and CLI. `anyhow` and `println!` belong here only.

A type lives in exactly one crate (`REWRITE_CONTRACT.md` §3); if two crates need it, it moves to the dependency below both.

## Safety and durability requirements

- `unsafe` only through `bytemuck`; any other `unsafe` block needs a one-line invariant comment and the maintainer's agreement.
- No `unwrap`/`expect` in library code except documented invariants. Corrupt or unexpected on-disk input (bad magic, CRC mismatch, out-of-range offsets, truncated runs) returns a defined `<Crate>Error`, never a panic or unchecked index.
- Never report a mutation as durable before its WAL entry is written; never let WAL trimming pass entries a pending node still needs.
- Failure atomicity: on error before commit, leave in-memory and on-disk state consistent. Never silently lose or duplicate objects, tags, bitmap members or allocations; deletes (tombstones/whiteouts) must survive flush.
- A reopened pool must reproduce exactly the state that was committed; every persisted structure gets a round-trip test that proves it.

## Rust and code style

- Readable names that read as prose (`zone_layout`, not `zl`). Prefer modern std APIs (`div_ceil`, etc.).
- Nightly features are allowed when they genuinely help. Tell the user about each new `#![feature(...)]` gate: the feature, why it is needed, and toolchain impact. Ask before changing the toolchain.
- Workspace crates depend on each other as `{ workspace = true }`, never by relative `path`. New dependencies go into the root `[workspace.dependencies]` first, and are a decision.
- Respect the cross-cutting choices in `REWRITE_CONTRACT.md` §2: sync only, `ciborium` CBOR, no JSON on disk, CRC32C, BLAKE3 for content, little-endian, `thiserror` per crate, `log` for logging.

## Use project tooling

- **Write tooling in Rust.** No Python, Ruby, JavaScript or their runtimes for helper tools, generators, test scaffolding or log parsing, even one-off. `just` recipes and minimal shell orchestration are the entry points.
- **Never use Perl, Python or `sed`** to edit files or do search-and-replace. Use the Edit/Write tools. Read-only `sed -n` inspection is fine; prefer the Read tool.
- **Version control:** JJ only, never raw Git mutations. Follow the working-copy workflow in `AGENTS.md` through the `jj-vcs` skill. Never push, move bookmarks, abandon, squash or rewrite existing changes.
- Run recipes from the repository root. Read the current `Justfile` first; use `just --list` and `just --dry-run <recipe>` to inspect. Demo recipes (`demo`, `pool-create`, `import-source`, FUSE mounts) create pools and mount filesystems: they are not validation and must not be run as such.
- Prefix shell commands with `rtk` to compress output (see `AGENTS.md`).

## Tests

- Write focused tests that prove each claim of the slice: they are the trail of proof and the regression guard. First check whether an existing test can take the new assertion.
- Persistence tests use `FileBlockDevice` with `tempfile::TempDir`: empty region → default, multi-entry round trip, single-entry guard, overwrite replaces state, delete survives reopen.
- Layout tests assert sizes and offsets in code and in `validation/`. Corruption tests flip a byte and expect the defined error.
- Never `#[ignore]` a failing test to get green. An ignored test names its plan item in a `TODO(<scope>-<item-id>)` comment.

## Validate and report

- **Every slice ends with the workspace green**: `just build`, `just test`, `just clippy`, and `cargo clippy --workspace --all-targets` (the recipe skips test targets), plus `cargo build --manifest-path validation/Cargo.toml` when layouts changed. A downstream crate left broken is a failed slice, not deferred work.
- During development narrow to `cargo test -p mimisbrunnr-<crate>`; narrower checks do not imply the workspace passed.
- Bound long-running commands with timeouts. Report exact commands and results; a command you could not run is a validation blocker, not a pass.
- Before completion, reconcile the spec sections, `<scope>-design.md`, `<scope>-decisions.md`, the plan and `<scope>-completed.md` with the code and tests. Documentation alone does not complete an implementation item.
- Report: the item addressed, affected paths, validation run and its results, remaining blockers, and the next prerequisite. Do not start another slice, and do not end with prompting questions (`AGENTS.md`).
