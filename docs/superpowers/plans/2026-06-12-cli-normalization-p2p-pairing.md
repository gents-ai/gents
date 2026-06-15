# CLI Normalization + P2P Pairing Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Normalize the CLI grammar and fill read/CRUD gaps (Phase A); make P2P pairing a document-driven, Lean-fenced convergence loop with invite/join ergonomics (Phase B).

**Architecture:** Phase A is pure CLI-crate work: one shared output enum, one shared dual-ID resolver, an argv pre-scan for deprecation warnings, and per-noun list/show/rm gap fills modeled on the `config skill` group. Phase B extends the existing `Proofs/PairingReconcile` Lean model (replicators → ownership → connections), moves the reconcile engine from desktop-core into the runtime crate behind the `RemoteP2pAdmin` trait seam, runs it unconditionally in `run_agent`, and layers invite/join token commands on top.

**Tech Stack:** Rust (clap derive, tokio, async-trait), Lean 4 + mathlib (proofs), GraphQL against DefraDB, ciborium + bs58 (invite tokens — both already in Cargo.lock).

**Spec:** `docs/superpowers/specs/2026-06-12-cli-normalization-p2p-pairing-design.md`

---

## Execution notes (read first)

- **Worktree:** all work happens in `../defra-agent-cli-normalization` (branch `cli-normalization`). The mathlib build cache is already symlinked into `crates/defra-agent/proofs/.lake/packages/mathlib/.lake/build`; `lake build` from `crates/defra-agent/proofs/` should be fast. Never run `lake exe cache get` (crashes on macOS).
- **Gates:** `cargo test -p defra-agent` (FULL package — never `--lib` alone), `cargo test -p defra-agent-cli`, `lake build` for any Lean change. Zero `sorry`s.
- **Sharp edges:** always `graphql::escape_graphql_string()` for interpolated values; never emit `[]` in a DefraDB mutation (emit `null`); `tracing`, never `println` (CLI user-facing output via the existing output helpers is the exception).
- **Lean-first ordering is mandatory for Phase B:** B1→B4 land before B6→B9 touch the Rust reconciler. Phase A and Phase B are independent tracks.
- **Field lists:** when a task says "all schema fields", the source of truth is the collection's file under `crates/defra-agent-schemas/schemas/agent/*.graphql`. Select every scalar field in list/show queries.

---

# Phase A — CLI normalization

### Task A1: Shared `OutputFormat` enum and shared dual-ID resolver

**Files:**
- Create: `crates/defra-agent-cli/src/cli/output_format.rs`
- Modify: `crates/defra-agent-cli/src/cli/mod.rs` (add `pub(crate) mod output_format;`)
- Modify: `crates/defra-agent-cli/src/request_helpers.rs:497` (generalize `resolve_request_id`)
- Test: unit tests inline in both files

- [ ] **Step 1: Write failing tests for the shared enum and resolver**

In `crates/defra-agent-cli/src/cli/output_format.rs`:

```rust
//! One output-format vocabulary for every command.
//!
//! Commands declare their default and supported subset, but `--output`
//! values mean the same thing everywhere.

use clap::ValueEnum;

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub(crate) enum OutputFormat {
    Text,
    Table,
    Json,
    Tree,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn value_enum_spellings_are_lowercase() {
        use clap::ValueEnum;
        let names: Vec<String> = OutputFormat::value_variants()
            .iter()
            .map(|v| v.to_possible_value().unwrap().get_name().to_string())
            .collect();
        assert_eq!(names, vec!["text", "table", "json", "tree"]);
    }
}
```

In `request_helpers.rs`, rename `resolve_request_id` → `resolve_dual_id` taking a noun for messages, keep `resolve_request_id` as a thin wrapper (call sites unchanged this task):

```rust
pub(crate) fn resolve_dual_id(
    noun: &str,
    flag_name: &str,
    positional: Option<&str>,
    flag: Option<&str>,
) -> Result<String> {
    let positional = positional.map(str::trim).filter(|v| !v.is_empty());
    let flag = flag.map(str::trim).filter(|v| !v.is_empty());
    match (positional, flag) {
        (Some(p), Some(f)) if p != f => anyhow::bail!(
            "conflicting {noun} ids provided: positional={p} and {flag_name}={f}"
        ),
        (Some(id), _) | (_, Some(id)) => Ok(id.to_string()),
        (None, None) => anyhow::bail!("missing {noun} id"),
    }
}
```

Add tests: equal-both → Ok, conflict → Err, neither → Err, flag-only/positional-only → Ok.

- [ ] **Step 2: Run `cargo test -p defra-agent-cli output_format resolve_dual` — expect FAIL (module not wired / fn missing)**
- [ ] **Step 3: Wire the module, implement, make existing call sites compile (wrapper keeps behavior)**
- [ ] **Step 4: Run `cargo test -p defra-agent-cli` — expect PASS, no behavior change**
- [ ] **Step 5: Migrate the per-command output enums** (`McpProbeOutput`, `BackgroundOutputFormat`, `SubagentListOutput`, `SubagentCancelOutput`, `RequestInterruptOutputFormat`, and the others `rg -n "ValueEnum" src/cli/args.rs` finds) to `OutputFormat`, preserving each command's default and rejecting unsupported variants at dispatch with a clear error (e.g. `subagent list` keeps default `tree`; `mcp probe` keeps default `text`). Delete the dead enums.
- [ ] **Step 6: Run `cargo test -p defra-agent-cli` — expect PASS; fix snapshot/help fallout in `tests/cli_help.rs`**
- [ ] **Step 7: Commit** — `feat(cli): shared OutputFormat and dual-ID resolver`

### Task A2: Deprecation pre-scan

**Files:**
- Create: `crates/defra-agent-cli/src/cli/deprecations.rs`
- Modify: `crates/defra-agent-cli/src/main.rs` (call before clap parse)
- Test: inline unit tests

- [ ] **Step 1: Write failing tests**

```rust
//! Deprecated-spelling detection. clap aliases route correctly but never
//! tell the handler which spelling was used, so we pre-scan argv.

/// (deprecated subcommand path, replacement) — extend as Phase A renames land.
const DEPRECATED: &[(&[&str], &str)] = &[
    (&["config", "task"], "task"),
    (&["p2p", "unpair"], "p2p pairings rm"),
    (&["p2p", "pairings", "remove"], "p2p pairings rm"),
    (&["show", "request"], "request show"),
    (&["show", "response"], "response show"),
];

/// Returns the warning line to print to stderr, if argv starts with a
/// deprecated subcommand path (flags before the subcommand are skipped).
pub(crate) fn deprecation_warning(argv: &[String]) -> Option<String> {
    let words: Vec<&str> = argv
        .iter()
        .skip(1)
        .map(String::as_str)
        .filter(|a| !a.starts_with('-'))
        .collect();
    DEPRECATED.iter().find_map(|(path, replacement)| {
        words.starts_with(path).then(|| {
            format!(
                "warning: `{}` is deprecated; use `{}`",
                path.join(" "),
                replacement
            )
        })
    })
}
```

Tests: `config task run x` warns, `task run x` doesn't, `--home h config task` still warns, unknown commands return None.

- [ ] **Step 2: Run `cargo test -p defra-agent-cli deprecation` — expect FAIL**
- [ ] **Step 3: Implement; in `main.rs` print to stderr (`eprintln!` is correct here — operator-facing warning, pre-tracing-init) before `Cli::parse()`**
- [ ] **Step 4: Run `cargo test -p defra-agent-cli` — expect PASS**
- [ ] **Step 5: Commit** — `feat(cli): argv pre-scan deprecation warnings`

### Task A3: Grammar moves — `task`/`config task` merge, `p2p unpair` fold, `show` aliases

**Files:**
- Modify: `crates/defra-agent-cli/src/cli/args.rs:982-986` (config task), `:1691-1704` (p2p unpair), `:74-78` + `:519-526` (show)
- Test: `crates/defra-agent-cli/tests/cli_help.rs` + new alias-routing tests

- [ ] **Step 1: Write failing parse tests** (in `args.rs` test module): `config task list`, `p2p unpair --peer x`, `show request ID` all still parse; `task list` parses; help for `config task` is hidden.

```rust
#[test]
fn deprecated_spellings_still_parse() {
    use clap::Parser;
    for argv in [
        vec!["defra-agent", "config", "task", "list"],
        vec!["defra-agent", "p2p", "unpair", "--peer", "p1"],
        vec!["defra-agent", "show", "request", "req-1"],
    ] {
        assert!(Cli::try_parse_from(&argv).is_ok(), "{argv:?}");
    }
}
```

- [ ] **Step 2: Run — expect PASS already (they exist); now make the moves without breaking the test:**
  - `Config Task`: mark `#[command(hide = true)]`; both enums already share `TaskListArgs`/`TaskShowArgs`/`ConfigTaskRunArgs` — delete `ConfigTaskCommand`, reuse `TaskCommand` in the `Config` enum.
  - `P2p Unpair`: mark `hide = true`; its handler already shares `P2pPairingRefArgs` with `pairings remove` — route both to the same function.
  - `Show`: mark `hide = true`; keep variants delegating to the same handlers as `request show`/`response show` (`ShowCommand::Request` already uses `RequestShowArgs`).
- [ ] **Step 3: Run `cargo test -p defra-agent-cli` — expect PASS (update `cli_help.rs` snapshots: hidden commands drop from help)**
- [ ] **Step 4: Commit** — `refactor(cli): hide deprecated command spellings, dedupe routing`

### Task A4: `config backend|behavior|tools|profile` — list/show/rm

The four nouns follow one pattern; `config skill` (`src/commands/config/skill.rs`, `SkillCommand` args.rs:1035) is the template. Do backend fully, then repeat for the other three in the same task (same shapes, different collection names/fields).

**Files:**
- Modify: `crates/defra-agent-cli/src/cli/args.rs:1014-1032,1350-1353` (the four command enums)
- Modify: `crates/defra-agent-cli/src/commands/config/{backend,behavior,tools,profile}.rs`
- Schemas (field lists): `crates/defra-agent-schemas/schemas/agent/` — `inference_backend.graphql`, `agent_behavior.graphql`, `tool_selection.graphql`, `inference_profile.graphql`
- Test: extend `crates/defra-agent-cli/tests/cli_config_backend.rs`, `cli_config_tools.rs`; create `tests/cli_config_behavior.rs`, `tests/cli_config_profile.rs` (mirror the backend test file's harness usage from `tests/support/`)

- [ ] **Step 1: Write failing integration test for backend list/show/rm** (pattern from `cli_config_backend.rs`'s existing `set` tests): seed a backend via `config backend set`, assert `config backend list --output json` contains it, `config backend show <id>` returns full doc, `config backend rm <id>` deletes and a second `rm` fails with not-found.
- [ ] **Step 2: Run `cargo test -p defra-agent-cli --test cli_config_backend` — expect FAIL (unknown subcommand)**
- [ ] **Step 3: Extend the enum:**

```rust
#[derive(Subcommand)]
pub(crate) enum BackendCommand {
    #[command(name = "set")]
    Set(BackendUpsertArgs),
    #[command(name = "discover-models")]
    DiscoverModels(BackendDiscoverModelsArgs),
    #[command(name = "list", about = "List InferenceBackend documents")]
    List(ConfigListArgs),
    #[command(name = "show", about = "Show an InferenceBackend document")]
    Show(ConfigShowArgs),
    #[command(name = "rm", about = "Delete an InferenceBackend document", alias = "remove")]
    Rm(ConfigShowArgs),
}
```

with shared arg structs (new, in args.rs — used by all four nouns):

```rust
#[derive(clap::Args)]
pub(crate) struct ConfigListArgs {
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long)]
    pub(crate) graphql: Option<String>,
    #[arg(long, value_enum, default_value_t = OutputFormat::Table)]
    pub(crate) output: OutputFormat,
}

#[derive(clap::Args)]
pub(crate) struct ConfigShowArgs {
    #[arg(long = "id", value_name = "ID")]
    pub(crate) id_flag: Option<String>,
    #[arg(value_name = "ID")]
    pub(crate) id: Option<String>,
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long)]
    pub(crate) graphql: Option<String>,
    #[arg(long, value_enum, default_value_t = OutputFormat::Json)]
    pub(crate) output: OutputFormat,
}
```

- [ ] **Step 4: Implement handlers in `commands/config/backend.rs`** following `skill.rs`'s list/show/rm shape: resolve `ConfigAccess` via `resolve_config_access(home, graphql, ...)`, query with every scalar field from the schema file, ID via `resolve_dual_id("backend", "--id", ...)`, all values through `escape_graphql_string`. **rm goes through the same Lean-fenced delete path `config apply --prune` uses** (the diff_prune delete model from #57/#387) — find it via `rg -n "diff_prune|fn delete_document" crates/defra-agent-cli/src` and reuse; do not hand-roll a raw delete mutation.
- [ ] **Step 5: Run the backend test — expect PASS. Commit** — `feat(cli): config backend list/show/rm`
- [ ] **Step 6–8: Repeat steps 1–5 for behavior, tools, profile** (one commit each; `BehaviorCommand`/`ToolSelectionCommand`/`InferenceProfileCommand` each gain the same three variants reusing `ConfigListArgs`/`ConfigShowArgs`; handlers in their respective files; ID fields per schema — behaviors key on `behavior_id`, tool selections on their selection id, profiles on `profile_id`; check the schema file for the exact unique key before writing the show/rm filter).

### Task A5: `config trigger`, `config schedule`, `config mcp` — list/show (read-only)

**Files:**
- Modify: `crates/defra-agent-cli/src/cli/args.rs:955-996` (three new `ConfigCommand` variants + enums reusing `ConfigListArgs`/`ConfigShowArgs`)
- Create: `crates/defra-agent-cli/src/commands/config/{trigger,schedule,mcp}.rs`
- Schemas: `event_trigger.graphql`, `schedule.graphql`, the MCP service registry collection (find with `rg -l -i "mcp" crates/defra-agent-schemas/schemas/`)
- Test: create `tests/cli_config_trigger.rs` (seed via `config apply` manifest fixture — copy a minimal manifest from `tests/cli_config_apply_local.rs`'s fixtures), assert list/show; same file covers schedule + mcp

- [ ] **Step 1: Write failing test** (seed trigger+schedule via apply, `config trigger list` shows it, `config trigger show <id>` full doc; repeat for schedule, mcp)
- [ ] **Step 2: Run — expect FAIL**
- [ ] **Step 3: Implement** (same list/show pattern as Task A4; no `set`, no `rm` — writes stay manifest-first per spec)
- [ ] **Step 4: Run `cargo test -p defra-agent-cli --test cli_config_trigger` — expect PASS**
- [ ] **Step 5: Commit** — `feat(cli): read commands for trigger/schedule/mcp registries`

### Task A6: `session list` / `session show`

**Files:**
- Modify: `crates/defra-agent-cli/src/cli/args.rs:2181-2184`
- Modify: `crates/defra-agent-cli/src/commands/` session module (find with `rg -ln "SessionCommand" src/commands`)
- Schema: `agent_session.graphql`
- Test: extend the session/fork integration test file (find with `rg -l "fork" tests/`)

- [ ] **Step 1: Failing test:** submit a request (creates a session), `session list` includes it, `session show <id>` returns the doc with its request count.
- [ ] **Step 2–4: Implement** `List(ConfigListArgs)` + `Show(ConfigShowArgs)` exactly as A4's pattern; sessions are runtime documents, not config, but the access/query layer is identical.
- [ ] **Step 5: Commit** — `feat(cli): session list/show`

### Task A7: Alias-compatibility sweep + docs

**Files:**
- Modify: `crates/defra-agent-cli/tests/cli_help.rs`
- Modify: `docs/operations.md`, `docs/demo.md` (any deprecated spellings in docs move to canonical)
- Test: extend the `deprecated_spellings_still_parse` test from A3 to cover EVERY entry in `deprecations.rs::DEPRECATED`, generated from the table itself:

- [ ] **Step 1: Write the table-driven test** (iterate `DEPRECATED`, append a plausible trailing arg per entry, assert parse Ok + warning Some)
- [ ] **Step 2: Run full `cargo test -p defra-agent-cli` — expect PASS**
- [ ] **Step 3: Update docs to canonical spellings; commit** — `test(cli): alias-compat sweep; docs to canonical grammar`

---

# Phase B — P2P pairing

Order is mandatory: B1 → B2 → B3 (Lean) → B4 (conformance) before B5–B9 (Rust). B10–B13 after B8.

### Task B1: Lean — replicator dimension (closes the existing model↔code gap)

**Files:**
- Modify: `crates/defra-agent/proofs/Proofs/PairingReconcile/State.lean`
- Modify: `crates/defra-agent/proofs/Proofs/PairingReconcile/Transition.lean`
- Modify: `crates/defra-agent/proofs/Proofs/PairingReconcile/Convergence.lean`
- Modify: `crates/defra-agent/proofs/Proofs/PairingReconcile/Executable.lean`

- [ ] **Step 1: Extend state** — `PairingDesired`/`PairingActual` gain `replicators : Finset String` (replicator addresses, mirroring Rust `PairingDesired.replicator_addresses`, diff.rs:11). `DiffOp` gains `installReplicator`/`teardownReplicator`. `converged` becomes equality on both components:

```lean
structure PairingDesired where
  collections : Finset String
  replicators : Finset String
  deriving DecidableEq

inductive DiffOp where
  | installCollection (c : String)
  | teardownCollection (c : String)
  | installReplicator (r : String)
  | teardownReplicator (r : String)
  deriving DecidableEq, Repr
```

- [ ] **Step 2: Extend transitions** — `reconcileInstallReplicator`/`reconcileTeardownReplicator` mirroring the collection pair (membership guards, insert/erase on `actual.replicators`); re-prove `crash_preserves_desired_actual` and add `reconcileInstallReplicator_adds_target` / `reconcileTeardownReplicator_removes_target` (same `Finset.mem_insert_self` / `Finset.not_mem_erase` shape as Transition.lean:42-54).
- [ ] **Step 3: Update Convergence.lean** — whatever measure it uses over collection symmetric difference extends to the sum over both components (read the existing proof first; the extension is mechanical: `card (desired.collections ∆ actual.collections) + card (desired.replicators ∆ actual.replicators)` decreases on every reconcile transition).
- [ ] **Step 4: Update Executable.lean** — `TransitionKind` gains `reconcileInstallReplicator`/`reconcileTeardownReplicator` with `toContract`/`fromContract?` round-trip theorem extended (`cases ... <;> rfl` still discharges).
- [ ] **Step 5: `cd crates/defra-agent/proofs && lake build` — expect success, zero sorrys. Commit** — `proof(pairing): replicator dimension`

### Task B2: Lean — applied (managed) set + unknown-desired

**Files:**
- Modify: same four PairingReconcile files

- [ ] **Step 1: Extend state with ownership and read-uncertainty:**

```lean
/-- What the reconciler itself introduced. Persisted (PeerPairingApplied);
    survives desired-row deletion. -/
structure PairingApplied where
  collections : Finset String
  replicators : Finset String
  deriving DecidableEq

structure ReconcileState where
  peer : PeerId
  /-- none = desired row read failed (unknown); some ∅ = positive absence. -/
  desired : Option PairingDesired
  actual : PairingActual
  applied : PairingApplied
  pairing : List PairingCollectionStatus
  deriving DecidableEq
```

- [ ] **Step 2: Rewrite transitions with ownership guards:**
  - `readFailure`: `post = { pre with desired := none }` (models supervisor.rs:426's error path — but now it carries no teardown power).
  - install ops require `desired = some d`, target `∈ d`, `∉ actual`; post inserts into BOTH `actual` and `applied`.
  - teardown ops require `desired = some d` (positive read), target `∉ d`, target `∈ actual`, **and target `∈ applied`**; post erases from both.
  - `operatorWrite` sets `desired := some newDesired`; `operatorDelete` sets `desired := some ⟨∅, ∅⟩` (positive absence — row gone after a successful read).
- [ ] **Step 3: Prove the ownership theorems (the point of this task):**

```lean
/-- Unmanaged invariance: no transition removes an actual object the
    reconciler did not introduce. -/
theorem unmanaged_wiring_survives
    {pre post : ReconcileState} (h : Transition pre post)
    {c : String} (hc : c ∈ pre.actual.collections)
    (hun : c ∉ pre.applied.collections) :
    c ∈ post.actual.collections

/-- Read failure is a no-op on live state. -/
theorem read_failure_preserves_actual
    {pre post : ReconcileState}
    (h : Transition pre post) (hrf : post.desired = none → ...) :
    -- state the readFailure case precisely; actual and applied unchanged
```

(State both precisely against the final transition shapes; discharge by `cases` on the transition. Also re-prove convergence: target is now `actual ⊇ desired ∧ (applied \ desired) = ∅` when `desired = some d`; no progress obligation when `desired = none`.)
- [ ] **Step 4: Executable.lean** — `TransitionKind` gains `readFailure`/`operatorDelete`; round-trip theorem updated.
- [ ] **Step 5: `lake build` — zero sorrys. Commit** — `proof(pairing): applied-set ownership + unknown-desired no-op`

### Task B3: Lean — connection dimension

**Files:**
- Modify: same four files

- [ ] **Step 1:** `PairingActual` gains `connected : Bool`; transitions `dial` (desired = some d, d nonempty, ¬connected → connected), `peerDisconnected` (environment event: connected → ¬connected, desired/applied unchanged), with install ops guarded on `connected = true`.
- [ ] **Step 2:** Liveness: under fairness (dial eventually succeeds after disconnect), convergence still reached — follow the liveness scaffolding pattern from the #473 sweep work (`rg -n "liveness" proofs/Proofs/` for the established shape).
- [ ] **Step 3:** `lake build` — zero sorrys. Commit — `proof(pairing): connection dimension + redial liveness`

### Task B4: Conformance mirror update

**Files:**
- Modify: `crates/defra-agent/tests/conformance/pairing_reconcile.rs`
- Modify: `crates/defra-agent/tests/support/pairing_conformance/` (invariants.rs, scenario.rs, runner.rs)
- Modify: `crates/defra-agent/tests/fixtures/pairing_scenarios/` (new scenario JSONs)
- Modify: the structure fence + coverage ledger (`tests/conformance/structure.rs`, `tests/conformance/coverage.rs`, and `proofs/.../CoverageLedger.lean` if transition ids are ledgered — rename BOTH sides together, per the #463 convention)

- [ ] **Step 1: Write failing scenario fixtures** for the new model surface:
  - `replicator_install_teardown.json` — desired replicators converge.
  - `read_failure_noop.json` — inject desired-read failure mid-scenario; safety invariant asserts actual state unchanged across the failed tick.
  - `unmanaged_survival.json` — pre-seed actual wiring NOT in applied; assert it survives full reconcile + unpair.
  - `delete_after_restart.json` — desired row deleted, crash/restart step, reconciler tears down exactly the applied set.
- [ ] **Step 2: Extend `invariants.rs::check_safety`** to assert unmanaged invariance and read-failure no-op from observation history; extend `ObservedSnapshot` with replicators/applied/connected as needed.
- [ ] **Step 3: Run `cargo test -p defra-agent --test conformance pairing` — expect FAIL** (harness/invariants don't know new fields; scenarios fail until B8's engine lands — mark the engine-dependent scenarios `#[ignore = "until p2p_reconcile engine (B8)"]` and leave the invariant/parsing tests green; the structure fence must show the new Lean transitions mapped, gaps loud).
- [ ] **Step 4: Commit** — `test(pairing): conformance mirror for replicators/ownership/connection`

### Task B5: `PeerPairingApplied` schema + `profiles` field + migration

**Files:**
- Create: `crates/defra-agent-schemas/schemas/agent/peer_pairing_applied.graphql`
- Modify: `crates/defra-agent-schemas/schemas/agent/peer_pairing_desired.graphql`
- Modify: `crates/defra-agent/src/migration.rs` (`ensure_peer_pairing_desired_migrations`, startup.rs:56 vehicle)
- Test: migration tests alongside the existing ones in migration.rs

- [ ] **Step 1: Schemas:**

```graphql
type PeerPairingApplied {
    peer_id: String @index(unique: true)
    collections: [String!]!
    replicator_addresses: [String!]!
    created_at: DateTime @index(direction: DESC)
    updated_at: DateTime @index(direction: DESC)
}
```

and add `profiles: [String!]` (nillable) to `PeerPairingDesired`. **Empty-list sharp edge:** every mutation writing `profiles` emits `null` when empty, never `[]`.
- [ ] **Step 2: Failing migration test:** fresh node + `ensure_peer_pairing_desired_migrations` → both collections exist with expected fields; re-run is idempotent; pre-existing desired rows survive the profiles addition.
- [ ] **Step 3: Implement; run `cargo test -p defra-agent migration` — PASS. Commit** — `feat(schema): PeerPairingApplied + desired profiles`

### Task B6: Move the reconcile seam into the runtime crate

Pure move, no behavior change. desktop-core already depends on defra-agent (`desktop-core/Cargo.toml:14`), so the direction is legal.

**Files:**
- Create: `crates/defra-agent/src/agent/p2p_reconcile/{mod.rs,trait_def.rs,http_impl.rs,diff.rs,error_class.rs}` (moved from `crates/defra-agent-desktop-core/src/remote_admin/`)
- Modify: `crates/defra-agent/src/agent/mod.rs` (export), `crates/defra-agent-desktop-core/src/remote_admin/mod.rs` → re-export shim (`pub use defra_agent::agent::p2p_reconcile::*;`) then delete the moved files
- Modify: imports in `desktop-core/src/client/core/supervisor.rs:16-29`

- [ ] **Step 1:** `git mv` the five files; fix module paths; desktop-core keeps a one-line re-export module so its callers don't churn in this task.
- [ ] **Step 2:** Check http_impl's dependencies (reqwest etc.) exist in the runtime crate's Cargo.toml; add any missing with workspace versions.
- [ ] **Step 3:** `cargo test -p defra-agent -p defra-agent-desktop-core` — expect PASS, zero behavior change. Commit — `refactor: move RemoteP2pAdmin seam + diff into runtime crate`

### Task B7: Embedded-node `RemoteP2pAdmin` adapter

**Files:**
- Create: `crates/defra-agent/src/agent/p2p_reconcile/embedded_impl.rs`
- Test: inline + extend `tests/support/pairing_conformance/runner.rs` to construct it

- [ ] **Step 1: Failing test:** against a single in-process node (the pairing-conformance harness already boots nodes — reuse its node constructor), `add_p2p_collections` + `list_p2p_collections` round-trips; `add_replicator` + `list_replicators` round-trips.
- [ ] **Step 2: Implement** `EmbeddedP2pAdmin(Arc<Node>)` implementing all `RemoteP2pAdmin` methods by calling the same defra-node APIs the HTTP handlers wrap (find each by reading what defradb.rs `crates/http/src/handlers/p2p/*.rs` calls on the node — e.g. the coordinator's replicator create/delete/list and subscription add/remove; the CLI's local-mode p2p commands in `defra-agent-cli/src/commands/p2p/access.rs` already do local node access and are the reference for obtaining the node handle).
- [ ] **Step 3: `cargo test -p defra-agent p2p_reconcile` — PASS. Commit** — `feat(pairing): embedded-node RemoteP2pAdmin adapter`

### Task B8: Runtime reconciler daemon (the conformance fence closes here)

**Files:**
- Create: `crates/defra-agent/src/agent/p2p_reconcile/engine.rs`
- Modify: `crates/defra-agent/src/agent/runtime/startup.rs` (spawn in the `background_tasks.spawn` block family, startup.rs:274-367)
- Modify: `tests/support/pairing_conformance/runner.rs` (drive the real engine; un-ignore B4 scenarios)

- [ ] **Step 1: Engine core — one tick, pure orchestration over the trait:**

```rust
pub struct PairingTickOutcome {
    pub peer_id: String,
    pub ops_applied: Vec<DiffOp>,
    pub desired_read_failed: bool,
}

/// One reconcile tick for one peer. Mirrors Lean Transition exactly:
/// - desired read failure => no-op (never default to empty desired)
/// - teardown restricted to the applied set
/// - applied set persisted after every successful op
pub async fn reconcile_peer_tick(
    node: &dyn RemoteP2pAdmin,
    store: &dyn PairingStateStore, // load_desired -> Result<Option<PairingDesired>>, load/save applied
    peer_id: &str,
) -> PairingTickOutcome
```

`PairingStateStore` is a small trait over the GraphQL reads/writes (desired row, applied row) so the engine is unit-testable without a node; the production impl writes `PeerPairingApplied` after each successful install/teardown (crash between op and persist = re-install on next tick, which is idempotent — same argument as the Lean crash transition).
- [ ] **Step 2: Failing unit tests** (mock both traits): read-failure → zero ops + flag; teardown only for applied∩actual∖desired; install updates applied; desired-absent + applied-present → full managed teardown then applied row delete.
- [ ] **Step 3: Implement the diff-with-ownership** (extend `compute_pairing_diff` or wrap it: teardown candidates intersect applied; passing `desired: Option<_>` — `None` short-circuits).
- [ ] **Step 4: Spawn loop in startup.rs:** iterate desired rows on a sweep interval (constant module-level `PAIRING_SWEEP_INTERVAL: Duration = Duration::from_secs(30)`; wire into the #477 sweep registry when that lands — leave a `// TODO(#477)` only in the registry-wiring sense, the loop itself is complete), plus subscribe to `PeerDisconnected` events for immediate redial nudges (event names per defradb.rs research: PeerConnected/PeerDisconnected on the transport). **Delete `pairing_reconcile_enabled()` / `DEFRA_AGENT_PAIRING_RECONCILE`** — the runtime always reconciles.
- [ ] **Step 5: Un-ignore the B4 scenarios; `cargo test -p defra-agent` (FULL) — expect PASS including conformance. Commit** — `feat(pairing): runtime pairing reconciler, env flag removed`

### Task B9: Desktop supervisor reuses the engine

**Files:**
- Modify: `crates/defra-agent-desktop-core/src/client/core/supervisor.rs:144-150,375-395,465-560` (`run_pairing_reconcile_for_peer` and the legacy path)
- Delete: `desktop-core/src/remote_admin/` re-export shim (point callers at `defra_agent::agent::p2p_reconcile`)

- [ ] **Step 1:** Replace the supervisor's inline diff/op-execution body with a call to `reconcile_peer_tick` using the HTTP adapter; keep the desktop-specific retry/stuck bookkeeping (`ensure_pairing_status`, supervisor.rs:623) wrapped around the outcome.
- [ ] **Step 2:** `cargo test -p defra-agent-desktop-core` + the existing `pairing_reconcile_tests` module (supervisor.rs:674) — expect PASS (update tests that asserted the old teardown-extras behavior: they now require applied-set membership).
- [ ] **Step 3: Commit** — `refactor(desktop): supervisor delegates to runtime pairing engine`

### Task B10: Profiles slice (atomic)

**Files:**
- Modify: `crates/defra-agent-cli/src/commands/p2p/pairings.rs:49-60` (write path stores `profiles` AND flattened `collections`)
- Modify: `crates/defra-agent/src/agent/p2p_reconcile/engine.rs` (desired load resolves profiles → collections at tick time; profile definitions read from the same source `expand_p2p_collection_args` uses — move that profile→collections table into the runtime crate so CLI and engine share it)
- Modify: desktop bootstrap (`supervisor.rs:331-360` region) to pass profiles through
- Test: engine unit test — desired row with `profiles: ["chat-requests"]` and empty collections resolves to that profile's collection set at tick time; changing the profile table changes the next tick's diff

- [ ] **Step 1: Failing tests (CLI write includes profiles; engine resolves)**
- [ ] **Step 2: Implement; empty `profiles` writes `null`, never `[]`**
- [ ] **Step 3: Full `cargo test -p defra-agent -p defra-agent-cli` — PASS. Commit** — `feat(pairing): profile intent persisted and resolved at reconcile time`

### Task B11: `p2p invite` / `p2p join`

**Files:**
- Modify: `crates/defra-agent-cli/src/cli/args.rs:1667-1725` (two new `P2pCommand` variants)
- Create: `crates/defra-agent-cli/src/commands/p2p/invite.rs`, `join.rs`
- Modify: `crates/defra-agent-cli/Cargo.toml` (add `ciborium`, `bs58` — already in the lockfile via other deps)
- Test: `tests/cli_p2p.rs` (token round-trip unit tests inline in invite.rs)

- [ ] **Step 1: Token envelope + failing round-trip test:**

```rust
/// Versioned pairing-invite envelope. CBOR-encoded, bs58-encoded, prefixed.
#[derive(Serialize, Deserialize, PartialEq, Debug)]
pub(crate) struct InviteToken {
    pub v: u8,                    // = 1
    pub ticket: String,           // iroh EndpointTicket (peer id + addrs)
    pub peer_id: String,
    pub did: String,
    pub profiles: Vec<String>,    // offered collection profiles
}

pub(crate) const TOKEN_PREFIX: &str = "dapair1-";

pub(crate) fn encode(t: &InviteToken) -> Result<String>;   // prefix + bs58(cbor)
pub(crate) fn decode(s: &str) -> Result<InviteToken>;      // strict: prefix, v==1
```

Tests: round-trip; wrong prefix rejected; v=2 rejected with "newer defra-agent required"; truncated b58 rejected.
- [ ] **Step 2: `p2p invite`** (args: `P2pAccessArgs` + repeated `--profile`, default `chat-requests`): read own peer_id/listen addresses/DID via the same sources `p2p status` uses (`commands/p2p/output.rs:145-186` — runtime state + identity), build the EndpointTicket string from the shareable address (defradb.rs addr.rs:16 shows ticket = `EndpointTicket::from(endpoint_addr)`; the shareable address may already BE a ticket — if the listen address parses as a ticket, pass it through), print the token plus a one-line "run `defra-agent p2p join <token>` on the other node".
- [ ] **Step 3: `p2p join <TOKEN>`** (positional token; `--profile` to narrow accepted profiles; `--wait` + `--timeout`): decode → upsert `PeerPairingDesired` (peer_id, did, addresses=[ticket], profiles; reuse `upsert_pairing_mutation`, pairings.rs:55) → print the **reciprocal token** (own invite, same code path as Step 2) with "paste back on the first node" — unless the desired row for that peer already exists (then it's the second leg; print converging status instead). `--wait` polls pairing health (B12's status source) until live or timeout.
- [ ] **Step 4: Integration test in `cli_p2p.rs`:** two harness nodes, invite on A, join on B, join reciprocal on A, assert both `PeerPairingDesired` rows exist and (with the B8 engine running) `p2p status` on both shows the peer connected.
- [ ] **Step 5: Full CLI suite — PASS. Commit** — `feat(p2p): invite/join token pairing`

### Task B12: `p2p pair` rework + pairings health

**Files:**
- Modify: `crates/defra-agent-cli/src/commands/p2p/pair.rs` (imperative triple-call → desired-row write + optional wait)
- Modify: `crates/defra-agent-cli/src/commands/p2p/pairings.rs` (list gains live-health columns)
- Modify: args.rs `P2pPairArgs` (+`--wait`, `--timeout`), `P2P_AFTER_HELP` texts (the desktop-env-flag notes die with the flag)
- Test: `tests/cli_p2p.rs`

- [ ] **Step 1: Failing tests:** `p2p pair --peer <addr>` writes a desired row (no immediate imperative calls); `p2p pairings list --output table` shows columns `PEER / DID / PROFILES / CONNECTED / SUBSCRIBED / REPLICATING` sourced by joining desired rows against `p2p status` live data + `PeerPairingApplied`.
- [ ] **Step 2: Implement; delete the stale `PAIRINGS_RECONCILE_NOTE` and pair/unpair after_help references to the env flag (args.rs:1696-1723, pairings.rs:15).**
- [ ] **Step 3: CLI suite — PASS. Commit** — `feat(p2p): pair is document-driven; pairings list shows live health`

### Task B13: End-to-end safety integration tests

**Files:**
- Modify: `crates/defra-agent/tests/conformance/pairing_reconcile.rs` (or a sibling integration file if the harness fits better)
- Test fixtures: from B4

- [ ] **Step 1:** Ensure the four B4 scenarios run against the REAL engine end-to-end (two nodes): read-failure no-op (kill the desired-state read via the store trait's failure injection), unmanaged survival (manually `p2p collections add` something, reconcile, unpair — still there), delete-then-restart teardown (rm desired row, restart node, applied set torn down exactly), restart-reconverge (kill + restart → pairing returns with no operator action).
- [ ] **Step 2:** `cargo test -p defra-agent` (FULL) + `cargo test -p defra-agent-cli` + `lake build` — all green, no ignores left, no flakes tolerated (capture/file/fix).
- [ ] **Step 3:** Update `docs/operations.md:57-82` — the manual bring-up section becomes the invite/join flow; keep the low-level commands documented as surgery tools.
- [ ] **Step 4: Commit** — `test(pairing): end-to-end ownership/no-op/restart safety suite`

---

## Self-review notes

- Spec coverage: A-tasks cover grammar rules 1–5 + all gap-fill rows; B-tasks cover Lean extension (B1–B3), conformance (B4, B13), schema (B5), placement+seam (B6–B7), reconciler+flag-removal (B8), desktop reuse (B9), profiles slice (B10), invite/join (B11), pair rework + health (B12). Spec's "rm only where the Lean delete model covers it" honored in A4 step 4 (reuse diff_prune path).
- Deliberately deferred (matches spec open questions): `--remote-graphql` one-shot join (held for #180), trigger/schedule `set` (manifest-only).
- Type consistency: `OutputFormat` (A1) used by A4–A6 arg structs; `ConfigListArgs`/`ConfigShowArgs` defined once in A4, reused A5–A6; `InviteToken`/`TOKEN_PREFIX` defined B11 and used only there; `reconcile_peer_tick`/`PairingStateStore` defined B8, reused B9.
