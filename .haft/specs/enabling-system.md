# Enabling System Spec

The enabling system is the team, code, and infrastructure that builds and
operates Mímisbrunnr — the target system. Spec sections under this carrier
describe *how the project is built and verified*, not what Mímisbrunnr
itself does once running.

## ES.architecture.001 Enabling architecture: Spec → Implementation → Evidence

```yaml spec-section
id: ES.architecture.001
spec: enabling-system
kind: enabling.architecture
title: Three-layer enabling architecture (Spec, Implementation, Evidence)
owner: human
statement_type: definition
claim_layer: description
status: active
valid_until: 2026-11-15
depends_on: [TS.boundary.001]
supersedes: []
terms:
  - HarnessableProject
  - TargetSystemSpec
  - EnablingSystemSpec
  - SpecSection
  - Evidence
target_refs: []
doc_refs: []
evidence_required:
  - kind: manual
    description: "every active SpecSection under .haft/specs/ is treated as authority; implementation changes that contradict an active section are blocked until either the section is rebaselined or the change is reverted"
  - kind: L1
    description: "`cargo test --workspace` and `cargo clippy --workspace -- -D warnings` produce evidence artifacts the project treats as load-bearing against active SpecSections"
  - kind: manual
    description: "a code-only change (Implementation layer) cannot edit a SpecSection in the same change; spec changes go through the onboarding rebaseline / reopen path"
```

The enabling system has three layers. Each layer is a *responsibility*,
not a directory; the one-way dependency rule below is what makes the
layers meaningful.

### L0 — Spec

Authoritative claims about the target system and the enabling system
itself. Carriers:

- `docs/DESIGN.md`, `docs/IMPLEMENTATION.md`, and supporting design /
  rewrite docs under `docs/` — the technical authority for what
  Mímisbrunnr is.
- `.haft/specs/target-system.md`, `.haft/specs/enabling-system.md`,
  `.haft/specs/term-map.md` — the parseable Haft v7 spine; SpecSections
  with status `active` are the load-bearing claims.
- `AGENTS.md` — the agent-facing operating instructions for this
  repository.

### L1 — Implementation

The artifact that realises the Spec. Carriers:

- `crates/*` (17 library crates) — the Cargo workspace that compiles
  into Mímisbrunnr.
- `bin/mimir`, `bin/brunnr` — the operator-facing CLIs.
- `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml`, `justfile` —
  build configuration and toolchain pin (treated as part of
  Implementation, not a separate layer).
- Source-level tests (`#[test]` inside each crate) — colocated with
  the code that produces them.

### L2 — Evidence

The artifacts that show Implementation realises Spec. Carriers:

- `cargo test --workspace` output — ~451 tests today across the 17
  crates.
- `cargo clippy --workspace -- -D warnings` output — programming-
  consistency evidence.
- Workspace-level integration tests (e.g. `bin/brunnr` integration
  suite) and any E2E scenarios.
- `.haft/evidence/*` records produced by harness runs (when the
  harness is active against this project).
- Review notes, manual procedure outcomes, and operator sign-offs
  captured in `.haft/decisions/*` and `.haft/notes/*`.

### Dependency rule

Dependencies flow strictly downward: **L2 may depend on L1 and L0; L1
may depend on L0; L0 depends on nothing inside this enabling system.**

Concrete consequences:

- A change that edits implementation code (L1) must not edit a
  SpecSection (L0) in the same change. Spec evolution goes through the
  onboarding rebaseline / reopen path, not through opportunistic
  edits.
- Evidence artifacts (L2) divide into two kinds. **Workspace-level**
  evidence (`cargo test --workspace`, `cargo clippy --workspace -- -D
  warnings`, integration suites) counts as baseline evidence against
  every active SpecSection: a regression in workspace-level evidence
  invalidates the boundary for the whole target. **Section-specific**
  evidence (an E2E scenario or a manual procedure named in a
  particular `evidence_required` entry) backs only the SpecSection it
  is linked to.
- The build/toolchain configuration lives at L1 (Implementation), not
  at a separate "Build" layer — Cargo and the rust-toolchain pin are
  treated as part of the artifact that realises the spec.

## ES.work_methods.001 How load-bearing artifacts are produced

```yaml spec-section
id: ES.work_methods.001
spec: enabling-system
kind: enabling.work_methods
title: Production methods for SpecSections, doc anchors, DecisionRecords, WorkCommissions, RuntimeRuns, and Evidence
owner: human
statement_type: duty
claim_layer: work
status: active
valid_until: 2026-11-18
depends_on: [ES.architecture.001]
supersedes: []
terms:
  - SpecSection
  - DecisionRecord
  - WorkCommission
  - RuntimeRun
  - Evidence
target_refs: []
doc_refs: []
evidence_required:
  - kind: manual
    description: "operator can name, for every load-bearing artifact kind in this section, who produced the most recent instance, what triggered it, and which closing check confirmed it"
  - kind: L1
    description: "for every active SpecSection whose `doc_refs` is non-empty, every listed anchor (e.g. DESIGN#5.2, IMPLEMENTATION#7.1) resolves to an existing numbered heading in the referenced document"
  - kind: manual
    description: "after any edit to docs/DESIGN.md or docs/IMPLEMENTATION.md that adds, removes, splits, merges, or renumbers a numbered subsection, the operator re-verifies every SpecSection's doc_refs against the new anchor set and rebaselines affected sections"
```

Each artifact kind below names: actor, trigger, and the deterministic
check that closes the step. Actors split into **operator** (the human
running the project) and **agent** (Claude Code or Codex, executing
under operator direction).

### SpecSection

- Actor: operator drafts and approves; agent assists with research and
  YAML wording.
- Trigger: `haft spec onboard` returns a non-terminal phase, or
  `haft spec check` reports `spec_section_drifted` for an existing
  section.
- Closing check: `haft spec check` clean **and** operator runs
  `haft spec onboard --approve <id>` to record a baseline **and** any
  affected `docs/DESIGN.md` / `docs/IMPLEMENTATION.md` sections are
  updated in the same change.

### DESIGN.md / IMPLEMENTATION.md doc section

- Actor: operator owns; agent drafts edits.
- Trigger: a SpecSection is created, rebaselined, or reopened, and
  its `doc_refs` point at sections in these docs; or those docs are
  edited and a SpecSection's `doc_refs` may have gone stale.
- Closing check: every active TargetSystemSpec SpecSection's
  `doc_refs` list resolves to an existing numbered heading in the
  referenced document (anchor format `DESIGN#X.Y` or
  `IMPLEMENTATION#X.Y`), and the prose at those anchors does not
  contradict the SpecSection.

The doc-anchor maintenance duty: whenever `docs/DESIGN.md` or
`docs/IMPLEMENTATION.md` is edited in a way that adds, removes, splits,
merges, or renumbers a numbered subsection, the operator must walk every
active SpecSection's `doc_refs`, fix references against the new anchor
set, and rebaseline the affected SpecSections via `haft spec onboard
--rebaseline <id> --reason "doc anchor drift: <summary>"`.

EnablingSystemSpec sections may carry an empty `doc_refs` list (the
enabling system has no DESIGN/IMPLEMENTATION counterpart); the duty
applies to TargetSystemSpec sections specifically.

### DecisionRecord

- Actor: operator frames via `/h-frame` → `/h-decide`; agent drafts
  variants and comparisons under operator direction.
- Trigger: an active SpecSection reports `uncovered` in
  `haft spec coverage`, or a problem in `.haft/problems/` needs
  resolution.
- Closing check: a DecisionRecord exists under `.haft/decisions/`
  citing the SpecSections it advances, and `haft spec coverage` flips
  the cited section to `reasoned`.

### WorkCommission

- Actor: operator authors via `haft commission`; agent may propose
  scope.
- Trigger: an active DecisionRecord authorises change and harness
  execution is wanted.
- Closing check: a commission YAML exists with scope, lockset, and
  evidence requirement; the cited SpecSection flips to `commissioned`
  in `haft spec coverage`.

### RuntimeRun

- Actor: agent (via Claude Code or Codex) under a WorkCommission;
  operator launches and supervises.
- Trigger: operator runs `haft harness run` against an open
  commission.
- Closing check: the run produces a diff and evidence artifacts; the
  harness exits success and writes a record under `.haft/evidence/`;
  workspace-level evidence (`cargo test --workspace` and `cargo clippy
  --workspace -- -D warnings`) is re-verified as part of the run.

### Evidence — workspace-level

- Actor: agent or operator invokes the toolchain.
- Trigger: every RuntimeRun. There is no CI or pre-commit hook in
  this project today; the RuntimeRun is the trigger.
- Closing check: `cargo test --workspace` and `cargo clippy
  --workspace -- -D warnings` both pass. Failure invalidates the
  RuntimeRun and any evidence it would have produced.

### Evidence — section-specific

- Actor: operator runs manual procedures; agent runs E2E scenarios
  under a commission.
- Trigger: named in a SpecSection's `evidence_required` entry;
  refreshed on RuntimeRun or as `valid_until` approaches.
- Closing check: the named E2E or manual procedure runs to success
  and is recorded in `.haft/evidence/` linked to the cited SpecSection.

## ES.effect_boundaries.001 Effect boundaries: who may write what

```yaml spec-section
id: ES.effect_boundaries.001
spec: enabling-system
kind: enabling.effect_boundaries
title: Mutation authority across operator, agent, and harness
owner: human
statement_type: admissibility
claim_layer: description
status: active
valid_until: 2026-11-18
depends_on: [ES.architecture.001, ES.work_methods.001]
supersedes: []
terms:
  - SpecSection
  - DecisionRecord
  - WorkCommission
  - RuntimeRun
  - Evidence
target_refs:
  - "law:effect-boundary-defined-by-this-section-and-haft-cli-methods"
  - "admissibility:operator-agent-harness-with-commission-gate-on-agent-and-harness-edits"
  - "deontics:harness-only-within-active-commission-scope-and-lockset"
  - "evidence:diffs-show-only-commission-scoped-paths-changed-and-spec-carriers-only-via-haft-cli"
doc_refs: []
evidence_required:
  - kind: manual
    description: "a review of any agent- or harness-authored diff confirms it touches only paths within the active WorkCommission's scope; out-of-scope edits are rejected and the run is failed"
  - kind: manual
    description: "no `.haft/specs/*`, `.haft/decisions/*`, or `.haft/commissions/*` carrier is edited by the harness or by an agent outside an explicit operator-driven `haft spec onboard` / `haft commission` invocation"
  - kind: L1
    description: "every commit that touches `crates/*`, `bin/*`, `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml`, or `justfile` and was authored by the harness cites an active WorkCommission id in its message"
```

Three actor classes are recognised:

- **Operator** — the human running the project. Has write authority over
  every resource listed below.
- **Agent** — Claude Code or Codex acting in this repo. May mutate
  resources only as permitted by the rows below; mutation of
  Implementation-layer or build-configuration files requires an active
  WorkCommission citing the relevant paths in its scope (the
  "commission gate").
- **Harness** — `haft harness run` driving the agent under an active
  WorkCommission. Mutation is restricted to the commission's scope and
  lockset.

The following table records who may write which resource. "Read-only"
means the actor may inspect the resource freely but never produces a
mutation.

| Resource | Operator | Agent | Harness |
|---|---|---|---|
| `.haft/specs/*` (SpecSections, term-map) | write — only through `haft spec onboard` (draft → approve → rebaseline / reopen) | read-only outside an operator-driven `haft spec onboard` invocation | read-only |
| `.haft/decisions/*` (DecisionRecords) | write — through `/h-frame`, `/h-decide`, or `haft` decision tooling | may draft variants in conversation; never finalises a record alone | read-only |
| `.haft/commissions/*` (WorkCommissions) | write — through `haft commission` | may propose scope text in conversation; never authorises a commission | read-only |
| `.haft/evidence/*` | append manual evidence records | append manual evidence records under operator direction | append run-produced evidence as part of a RuntimeRun |
| `docs/DESIGN.md`, `docs/IMPLEMENTATION.md` | write | write under operator direction (kept in sync with SpecSection `doc_refs` per `ES.work_methods.001`) | read-only |
| `crates/*`, `bin/*` (Implementation L1) | write | write **only within an active WorkCommission's scope and lockset** | write **only within an active WorkCommission's scope and lockset** |
| `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml`, `justfile` | write | write **only if explicitly listed in the active WorkCommission's scope** — dependency or toolchain changes are always non-trivial and never implicit | write under the same rule |
| `AGENTS.md`, `CLAUDE.md`, `.zed/settings.json`, `.mcp.json`, `.claude/**` | write | write under operator direction; commission gate does not apply (these govern the agent itself) | read-only |
| `.jj/` (VCS state) — `jj` operations, branches, commits | write — operator drives `jj` per the `jj-vcs` skill | read-only `jj` inspection only; never runs mutating `jj` or `git` | read-only |
| `.haft/refresh/`, `.haft/notes/`, `.haft/problems/` | write freely | append notes and file problem records under operator direction | append harness-generated notes |

Authorization rule (the closing gate for any actor-produced diff):

- Any harness-authored or agent-authored change to a row that says
  "write only within an active WorkCommission's scope and lockset"
  must be traceable to a specific WorkCommission id. The diff must
  touch only paths within that commission's scope; any out-of-scope
  edit fails the run.
- Carriers under `.haft/specs/`, `.haft/decisions/`, and
  `.haft/commissions/` are mutable only by operator-driven Haft CLI
  methods, never by ad-hoc file edits inside an agent or harness run.

## ES.agent_policy.001 Supported host agents and autonomy bounds

```yaml spec-section
id: ES.agent_policy.001
spec: enabling-system
kind: enabling.agent_policy
title: Claude Code is the only supported host agent; commission-gated mutation, free read and network
owner: human
statement_type: admissibility
claim_layer: description
status: active
valid_until: 2026-11-18
depends_on: [ES.effect_boundaries.001, ES.work_methods.001]
supersedes: []
terms:
  - WorkCommission
  - RuntimeRun
  - SpecSection
target_refs:
  - "law:agent-policy-defined-by-this-section"
  - "admissibility:claude-code-only-with-commission-gate-on-mutation"
  - "deontics:operator-states-goal-and-scope-and-reviews-every-diff"
  - "evidence:harness-runs-cite-commission-id-and-operator-review-recorded-in-decisions-or-notes"
doc_refs: []
evidence_required:
  - kind: manual
    description: "no harness run or agent-authored diff produced under a Codex or other-host-agent surface enters the working tree; only Claude Code surfaces are admitted"
  - kind: manual
    description: "every harness-authored mutation diff is reviewed by the operator before it lands; agent-authored mutations outside a commission do not exist on main"
  - kind: manual
    description: "SpecSection status flips and baselines are recorded only via operator-driven `haft spec onboard` invocations, never by agent self-action"
```

### Supported host agents

Claude Code is the only host agent admitted by this project. Other MCP
clients (Codex, Cursor, Gemini CLI, JetBrains Air, etc.) are not
supported; their diffs and runtime artifacts are not admissible against
this enabling system.

### Autonomy bounds — what the agent may do

| Action class | Without commission | Under active commission |
|---|---|---|
| Read any file; run read-only commands (`jj st`, `cargo check`, `grep`, language servers) | allowed | allowed |
| Network access (`WebFetch`, `WebSearch`, fetching crate docs and similar context-only operations) | allowed | allowed |
| Propose YAML / edits / patches in conversation | allowed | allowed |
| Write `crates/*`, `bin/*` (Implementation L1) | **not allowed** | allowed within commission scope and lockset |
| Edit `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml`, `justfile` | **not allowed** | allowed only if explicitly listed in commission scope |
| Edit `docs/DESIGN.md`, `docs/IMPLEMENTATION.md` | **not allowed** | allowed if listed in commission scope |
| Edit any `.haft/specs/*`, `.haft/decisions/*`, `.haft/commissions/*` carrier directly | **never** (only via operator-driven Haft CLI methods) | **never** |
| Run mutating `jj` or `git` (commit, rebase, push, branch ops) | **never** | **never** (operator drives VCS per the `jj-vcs` skill) |
| Launch a RuntimeRun (`haft harness run`) | **never** | n/a — operator launches |
| Install or modify system packages, change global config outside this repo | **never** | **never** |

### Operator's delegation duties

When delegating work to the agent, the operator:

- States the goal and the scope in writing before the agent begins;
  scope is not inferred from prior turns alone.
- Reviews every harness-authored diff before it lands. The harness is
  not a merge gate.
- Never rebaselines or reopens a SpecSection on the agent's
  recommendation alone; the rebaseline `--reason` records operator
  judgment.
- Treats agent-produced architectural arguments as drafts to verify
  against `docs/DESIGN.md` and existing baselined SpecSections — not
  as authority.

### Human-decision gates (not automatable)

- Approving a new SpecSection (`status: draft` → `status: active`) and
  recording its baseline.
- Authorizing a WorkCommission.
- Accepting Evidence as satisfying an `evidence_required` entry.
- Rebaselining or reopening a SpecSection after drift.
- Any mutation of VCS history (`jj` / `git` write operations).

## ES.commission_policy.001 WorkCommission creation, scoping, and retirement

```yaml spec-section
id: ES.commission_policy.001
spec: enabling-system
kind: enabling.commission_policy
title: Operator-authored commissions; explicit scope; 30-day default; tandem-crate justification required
owner: human
statement_type: admissibility
claim_layer: description
status: active
valid_until: 2026-11-27
depends_on: [ES.work_methods.001, ES.effect_boundaries.001, ES.agent_policy.001]
supersedes: []
terms:
  - WorkCommission
  - DecisionRecord
  - SpecSection
  - Evidence
target_refs:
  - "law:commission-policy-defined-by-this-section-and-haft-commission-cli"
  - "admissibility:operator-only-creation-with-non-empty-allowed-paths-and-active-decision-record"
  - "deontics:operator-records-tandem-reason-and-satisfaction-or-rejection-at-retirement"
  - "evidence:commission-yaml-shows-non-empty-allowed-paths-and-cited-decision-id-and-tandem-reason-when-multi-crate"
doc_refs: []
evidence_required:
  - kind: manual
    description: "every active commission in `.haft/commissions/` cites at least one active DecisionRecord id, which in turn cites at least one active SpecSection id; orphan commissions are rejected at retirement review"
  - kind: manual
    description: "for any commission whose `allowed_paths` covers more than one crate under `crates/*` or `bin/*`, the commission carries a non-empty `tandem_reason` field explaining why the crates must change together"
  - kind: L1
    description: "no harness diff touches a path matched by the implicit `forbidden_paths` set (`.haft/specs/**`, `.haft/decisions/**`, `.haft/commissions/**`, `.jj/**`, `.claude/**`, `AGENTS.md`, `CLAUDE.md`, anything outside this repo)"
```

### Creation

- A WorkCommission is created only by the **operator**, via
  `haft commission`. Agent and harness may propose scope text in
  conversation but never author a commission.
- The commission must cite at least one active DecisionRecord; that
  DecisionRecord must in turn cite at least one active SpecSection.
- Title and `--reason` are mandatory.

### Scope — `allowed_paths`

- There is no default-allow. A commission whose `allowed_paths` list
  is empty is rejected at creation.
- Common patterns: `crates/<crate-name>/**`, `bin/<bin-name>/**`,
  `docs/IMPLEMENTATION.md` (only when a SpecSection's `doc_refs`
  change requires it).
- **Multi-crate commissions are allowed** (a single commission may
  list paths under more than one crate or binary) **but require a
  non-empty `tandem_reason` field** explaining why those crates need
  to change together — e.g., a shared trait whose signature is
  evolving, or a cross-crate refactor of a single concept. The
  `tandem_reason` is reviewed at retirement; missing or hand-wavy
  tandem reasons block satisfaction.

### Implicit `forbidden_paths` (cannot be overridden)

The following paths are forbidden to harness and agent mutation
regardless of `allowed_paths`. A diff matching any of these fails the
run:

- `.haft/specs/**` — spec carriers mutate only via
  `haft spec onboard`.
- `.haft/decisions/**`, `.haft/commissions/**` — managed only by the
  operator-driven Haft CLI methods.
- `.jj/**` — VCS state is operator-only per the `jj-vcs` skill.
- `.claude/**`, `AGENTS.md`, `CLAUDE.md` — agent and operator
  configuration is operator-only.
- Anything outside this repository.

### Freshness gates (all must pass before harness execution starts)

1. `haft spec check` is clean (no L0 / L1 / L1.5 findings).
2. Every SpecSection cited by the commission (via its DecisionRecord
   chain) is `status: active` and not drifted.
3. The workspace builds against the current `HEAD`:
   `cargo build --workspace` succeeds.
4. The commission's `evidence_required` entries name reachable check
   kinds (no dangling `kind:` strings, no references to manual
   procedures without an operator owner).

### `valid_until` default and renewal

- Default `valid_until` for a new commission is **30 days** after
  creation. Operator may override at creation but never beyond 90
  days.
- An unexecuted commission past `valid_until` is **expired**; the
  operator may renew by authoring a new commission citing the same
  DecisionRecord, or drop it.

### Retirement

A commission leaves the active set in one of three states:

- **Satisfied** — a harness run produced a diff that landed, the
  cited `evidence_required` is met, the `tandem_reason` (if any)
  matches the actual diff shape, and the operator records
  satisfaction in `.haft/decisions/` or `.haft/evidence/`.
  Commission status flips to `closed`.
- **Rejected** — the harness diff failed review, scope was wrong, or
  evidence did not satisfy the requirement. The operator closes the
  commission with a `--reason` recording what failed; the underlying
  DecisionRecord stays open until a new commission is authored.
- **Expired** — `valid_until` reached without execution. No
  retirement record is required beyond the natural expiry; renewal
  produces a new commission id.

## ES.runtime_policy.001 RuntimeRun lifecycle, isolation, and observability

```yaml spec-section
id: ES.runtime_policy.001
spec: enabling-system
kind: enabling.runtime_policy
title: Operator-owned lifecycle via `haft harness run`; per-worktree isolation; at-most-one active run
owner: human
statement_type: duty
claim_layer: work
status: active
valid_until: 2026-11-27
depends_on: [ES.agent_policy.001, ES.commission_policy.001]
supersedes: []
terms:
  - RuntimeRun
  - WorkCommission
  - Evidence
target_refs:
  - "law:runtime-policy-defined-by-this-section-and-haft-harness-cli"
  - "admissibility:operator-only-launch-and-stop-via-foreground-haft-harness-run"
  - "deontics:operator-reviews-diff-and-evidence-before-any-merge-into-main-tree"
  - "evidence:per-run-record-under-.haft/evidence/run-id-with-diff-logs-and-check-exit-status"
doc_refs: []
evidence_required:
  - kind: manual
    description: "every RuntimeRun is started in the foreground by the operator via `haft harness run`; there is no background daemon, no scheduler, and no MCP-initiated run"
  - kind: manual
    description: "every RuntimeRun executes in a fresh `jj` (or `git`) worktree distinct from the operator's main working tree; surviving worktrees with changes are listed in the run record for operator review and auto-removed when empty"
  - kind: L1
    description: "for any RuntimeRun whose record lives under `.haft/evidence/<run-id>/`, the record contains commission id, start/stop timestamps, captured stdout/stderr, the produced diff, and the exit status of every closing-check command (`cargo build --workspace`, `cargo test --workspace`, `cargo clippy --workspace -- -D warnings`)"
  - kind: manual
    description: "no two RuntimeRuns are active in this repository at the same time; concurrent runs are rejected at launch"
```

### Lifecycle ownership

The **operator** starts and stops every RuntimeRun via `haft harness
run` from a terminal. Claude Code (the MCP host agent surface) does not
own runtime lifecycle. The agent may:

- Propose a run in conversation.
- Draft the commission text the run will execute under.
- Execute work *inside* a run that the operator launched.

The agent never initiates a run, never terminates one, and never
schedules one in the background. There is no daemon process, no cron,
no implicit run trigger. A RuntimeRun exists only while
`haft harness run` is in the operator's foreground.

### Isolation

Each RuntimeRun executes inside an isolated `jj` worktree (or a `git`
worktree if jj is degraded), created by Haft at run start under a path
Haft chooses (e.g. `.jj/run-<run-id>/`). The operator's main working
tree is never the run substrate.

- Worktrees that produced no changes are auto-cleaned at run end.
- Worktrees that survive (because the run produced a diff) are listed
  in the run record for operator review; the operator decides when
  they are merged or discarded.
- Network access during a run follows the agent-policy table — read
  and context-only network calls are allowed; package installs and
  system-level changes are not.
- No containers, VMs, or process-namespace isolation are required.
  This is a solo-developer project on a dev machine; isolation is
  per-worktree, not per-namespace.

### Observability

For every RuntimeRun:

- Run logs stream to the operator's terminal in real time.
- A run record is written to `.haft/evidence/<run-id>/` containing:
  the commission id, start and stop timestamps, captured stdout and
  stderr, the produced diff, and the exit status of every
  freshness-gate and closing-check command.
- The diff is presented to the operator before being merged into the
  main working tree. Merge is a separate operator action, not part
  of the run.
- A run that fails any closing check writes the failing command's
  output verbatim into the run record; the partial diff is retained
  for forensic review but is not auto-applied.

### Concurrency

At most one RuntimeRun is active in this repository at any time. A
launch attempted while another run is active is rejected; the operator
must stop the active run before starting a new one.

## ES.evidence_policy.001 Admissible evidence, minimum congruence, refresh triggers

```yaml spec-section
id: ES.evidence_policy.001
spec: enabling-system
kind: enabling.evidence_policy
title: Evidence kinds, per-class minimums, and freshness rules
owner: human
statement_type: duty
claim_layer: evidence
status: active
valid_until: 2026-11-27
depends_on: [ES.work_methods.001, ES.runtime_policy.001]
supersedes: []
terms:
  - Evidence
  - SpecSection
target_refs:
  - "law:evidence-policy-defined-by-this-section"
  - "admissibility:only-listed-kinds-may-appear-in-evidence_required"
  - "deontics:operator-refreshes-evidence-when-any-listed-trigger-fires"
  - "evidence:per-section-evidence_required-resolves-against-admissible-kinds-and-passes-freshness-rules"
doc_refs: []
evidence_required:
  - kind: manual
    description: "every active SpecSection's `evidence_required` entries use only the admissible kinds listed in this section (`type`, `L1`, `L2`, `L3`, `L4`, `DB`, `manual`); unknown kinds are rejected"
  - kind: manual
    description: "operator review confirms that every TargetSystemSpec section carries at least one workspace-level kind (`L1` or `L2`) AND at least one E2E or manual kind (`L3` or `manual`); every EnablingSystemSpec section carries at least one `manual` entry"
  - kind: L1
    description: "for every SpecSection whose `evidence_required` cites a reserved kind (`L4` or `DB`) and no suite exists, the project's coverage view reports the section as `commissioned` at best — never `verified` — until the suite is in place"
```

### Admissible evidence kinds

The following `kind:` values may appear in a SpecSection's
`evidence_required` list. Kinds not listed below are rejected.

| Kind | Meaning | Producer |
|---|---|---|
| `type` | the Rust type system rules out the failure mode | `cargo check --workspace` |
| `L1` | unit / property tests inside the same crate as the code under test | `cargo test -p <crate>` |
| `L2` | integration tests that wire two or more crates together | `cargo test --workspace` (integration test directories) |
| `L3` | end-to-end tests exercising binaries (`brunnr`, `mimir`) against real `FileBlockDevice` storage | `cargo test --workspace --features e2e` or equivalent |
| `L4` | multi-node convergence E2E | dedicated suite (reserved; suite not yet implemented) |
| `DB` | on-disk invariant check against persisted state (CRCs, structural validators) | dedicated tooling against a pool snapshot (reserved; not yet implemented) |
| `manual` | operator runs a procedure and records the outcome under `.haft/evidence/` | operator |

`L4` and `DB` are reserved kinds. A SpecSection may cite them in
`evidence_required` before the suite exists, but such a section cannot
flip past `commissioned` in `haft spec coverage` until the suite lands.

### Minimum congruence per claim class

A SpecSection's effective congruence `R_eff` is the *minimum* across
its required-evidence entries, not the average. Project-level minimums:

- **TargetSystemSpec sections (`TS.*`)** — at least one
  workspace-level kind (`L1` or `L2`) AND at least one E2E **or**
  `manual` kind (`L3` or `manual`). The workspace-level row guards
  against regressions; the E2E / manual row binds the claim to an
  observable in the target environment.
- **EnablingSystemSpec sections (`ES.*`)** — at least one `manual`
  entry. Most enabling claims are process duties whose evidence is
  operator-curated; automatable checks (`L1`) are added where they
  exist (e.g., the doc-anchor resolver check in
  `ES.work_methods.001`).
- **Term-map** — no evidence required. The term-map is a vocabulary
  carrier; congruence is curated by the operator and reviewed during
  onboarding.

### Refresh triggers (when evidence becomes stale)

Evidence backing a SpecSection becomes stale and must be re-produced
when any of the following triggers fires:

1. The SpecSection's `valid_until` is within 30 days.
2. The SpecSection is rebaselined via
   `haft spec onboard --rebaseline`.
3. A path that produced the evidence is edited — for example, a test
   file is modified, or the manual procedure's referenced doc anchor
   moves.
4. The workspace toolchain pin changes (`rust-toolchain.toml` is
   edited).
5. A commit lands on `main` that touches any path inside one of the
   SpecSection's `doc_refs` source ranges.

A stale evidence entry blocks the SpecSection from reporting
`verified` in `haft spec coverage` until the operator re-runs the
underlying check and records the refreshed artifact.

## ES.placeholder.001 Enabling system placeholder (superseded)

```yaml spec-section
id: ES.placeholder.001
spec: enabling-system
kind: creator-role
title: Enabling system placeholder
statement_type: explanation
claim_layer: carrier
owner: human
status: superseded
valid_until: 2026-11-15
depends_on: []
supersedes: []
terms: []
target_refs: []
evidence_required: []
```

Retained as a parseable carrier for traceability; superseded by
`ES.architecture.001`.
