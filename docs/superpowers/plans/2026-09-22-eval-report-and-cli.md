# Eval Report and CLI (M4) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give the operator a window onto eval runs, trials, comparisons and optimization jobs: a versioned report derived on demand from the documents (`gents::eval::report::{build, compare}` and the 4b breakdowns), the runner changes the operator surface needs (cancel marker, `progress.json`, the abandoned-attempt fix), and thin `gents eval` and `gents optimization` commands over the library.

**Architecture:** Five stacked PRs. PR 1 extends the unfrozen `eval::report` module M6b created with a pure `build` (slot classification) and a pure `compare` (the optimizer's own pairing and `decide` statistics), adds `report::store` as the one reader that gathers a run's documents, and changes `eval::runner`: freeze writes `<run dir>/definition.json` (which the report reads), the `<run dir>/cancel` marker (also seen mid-batch), the abandoned-attempt bound (spec §9), and `<run dir>/progress.json` with a heartbeat. PR 2 adds `gents eval` (nine commands) as thin bodies that take an injected executor, so the tests drive them with `ScriptedExecutor` against an embedded home. PR 3 adds `gents optimization` (five commands, `rm` by ruling U1). PR 4 adds `eval watch`, `eval gc` and the size column. PR 5 adds the compare breakdowns.

**Tech Stack:** Rust 1.97.1, tokio (`signal`, `time`), `tokio_util::sync::CancellationToken`, clap 4 derive, serde/serde_json, chrono, tempfile, DefraDB via `gents::defra_node::EmbeddedNode` and `gents::eval::runner::embedded::EmbeddedHome`.

**Spec:** `docs/superpowers/specs/2026-09-22-eval-report-and-cli-design.md` (spec 4a approved 2026-09-22; its "Spec 4b" section, approved the same day, adds sections 6 to 9 and PRs 4 and 5). Umbrella: `2026-09-21-eval-and-optimization-umbrella.md`. Contract: `2026-09-21-eval-core-contract-design.md`. Runner: `2026-09-21-eval-runner-design.md`. Optimization: `2026-09-21-optimization-on-eval-design.md`. M6b plan (whose library this consumes): `docs/superpowers/plans/2026-09-22-optimization-driver.md`.

## The M4 base

Ruling U11/U12: **the M4 base is M6b's FINAL tip — `optimization/23-promote` after M6b's plan completes.** All five M4 PRs stack on it, one after another; there are no cherry-picks and no second base. The orchestrator supplies the hash at spin-up and the coordinator records it in the ledger as `<M4-BASE>`; every command below that says `<M4-BASE>` means that hash. The code this plan cites was read at `optimization/22-driver` @ `e4f3373b5` ("docs(optimization): token_totals states how missing usage feeds the cost gate"), which 23-promote contains; M6b PR 4 adds `promote`, `revert`, `Promotion`, `PromoteRefused` and `promote_refused` on top; ruling F11 records that `optimization/23-promote` @ `144be2916` ("feat(optimization): operator-only promote and a terminal revert") matches Task 12's interface block, and `JobState`'s variants (`Running`, `ReadyToPromote`, `NothingToPromote`, `Exhausted`, `Failed { reason }`, `Promoted`, `Stale`, `Reverted`) and the three post-`execute_run` sites in `driver.rs` Task 4a edits were read at that branch (`/Users/iron-arch-mage/Repos/Source/gents-optimization-23-promote`, tip `75e753741`). Task 1 Step 0 and Task 12 Step 0 still re-verify at dispatch.

| PR | Branch | Base |
|---|---|---|
| 1 | `eval/50-report` | `<M4-BASE>` (M6b's final `optimization/23-promote`) |
| 2 | `eval/51-cli` | `eval/50-report` |
| 3 | `eval/52-optimization-cli` | `eval/51-cli` |
| 4 | `eval/53-watch-gc` | `eval/52-optimization-cli` |
| 5 | `eval/54-compare-breakdowns` | `eval/53-watch-gc` |

**Depends on:** this plan runs under `docs/superpowers/orchestration/2026-09-21-parallel-coordinators.md`.

---

## Deviations from the spec and from the brief, decided at planning

Orchestrator directions (2026-09-22 addendum):

- **O1 — `progress.json` moves to PR 1.** Spec 4b's phasing table puts "`progress.json` in the loop" in PR 4. The orchestrator placed it in PR 1 because it lives in the loop. PR 4 keeps only its reader (`eval watch`).
- **O2 — The spec §9 runner fix is in PR 1**, with a table test in `plan.rs` and a loop test that a cancelled slot is re-planned under `max_infra_retries: 0`.
- **O3 — PRs 4 and 5 are part of M4**, in this plan, on the single base stated in "The M4 base".

Orchestrator rulings on the planning draft (`.superpowers/sdd/m4-plan-rulings.md`), applied:

- **U1 — `gents optimization rm <job_id> [--force]`** (Task 12a) deletes `<origin.jobs_dir>/<job_id>/`; which jobs it removes without `--force` is ruling F2's set. `eval gc --jobs` (Task 14) removes the same set's directories older than the threshold.
- **U2 — Freeze materializes the definition** (Task 2a): `<run dir>/definition.json`, verified by digest on resume; `report::store` builds from it and reports `definition_changed` from the installed one (Task 3); runs without the file fall back to the installed definition only while it matches.
- **U3 — `optimization revert <job_id> --digest D`**; spec amended (D11).
- **U4 — `progress.json` entries carry `pid` and `written_at`** (Task 6), refreshed on the loop's timer (Task 6a); liveness by pid per ruling F3 (`host_alive`), staleness by age as the fallback (Task 13).
- **U5 — The marker is seen mid-batch** on the `marker_poll` timer inside the batch `select!` (Task 6a), with a test that a long trial is interrupted by a marker written mid-trial.
- **U6 — Accepted:** the embedded path is covered by the canary and one CLI binary smoke test (Task 10c).
- **U7 — Accepted:** the CLI fixture duplicates the crate-private harnesses; `commands/eval/testing.rs` names the files that move with it.
- **U8 — `eval run` on an existing run id clears the marker as `resume` does**, in `runner::run` (Task 4).
- **U9 — Spec wording amended:** `build`, `compare`, `by_*` are pure; `report::store` is the module's one I/O boundary beside `load_run_rows` (module docs in Tasks 1 and 3).
- **U10 — Accepted for now:** `list` and `gc` build one report per run. Follow-up: a header-only projection.
- **U11/U12 — Single base:** M6b's final `optimization/23-promote` tip; all five PRs stack on it; no cherry-picks ("The M4 base"; Task 1 Step 0 and Task 12 Step 0 re-verify).
- **U13 — Accepted as inherited behavior**, noted in `eval run --registry`'s doc (Task 10b).
- **D1, D2, D3, O1, D12 accepted.**

Orchestrator rulings on the plan review (`.superpowers/sdd/m4-plan-review-rulings.md`), applied:

- **F1 — `RunOutcome.cancelled`** (Task 4) is set when the token or the marker stopped the pass. The optimizer treats it after `execute_run` as "stopped, no decision" and returns the job `Running` (Task 4a, `driver.rs`). `eval cancel` on a run whose purpose is `optimization:<job>` proceeds and says the job stops at that run (Task 8). `eval run`'s stopped notice reads `cancelled` (Task 10b).
- **F2 — Removable job states** are `NothingToPromote`, `Exhausted`, `Failed`, `Stale`, `Promoted`, `Reverted` (`optimization::references::removable`, Task 4b). `optimization rm` and `gc --jobs` keep a `ReadyToPromote` job; their tests remove a promoted-then-reverted one (Tasks 12a, 14). This closes the draft's R1.
- **F3 — `host_alive(pid)`** via `libc::kill(pid, 0)` on unix, `true` elsewhere (Task 6); `is_fresh` combines it with age; the watcher's live test entry is this process (Task 13). This closes the draft's R2.
- **F4 — The comparison seed is `decision_seed` over the distinct run ids** (Task 2); a 21-case test pins it to what `optimization show` computes (Task 2b).
- **F5 — `optimization::evidence::totals`** is the one two-cell cost rule, called by `token_totals` and by `Comparison`; `CellReport.cell_usage` stores `evidence::cell_usage` (Tasks 1, 1b, 2).
- **F6 — The backoff between passes** watches the marker on the `marker_poll` interval (Task 6a).
- **F7 — "Finished" for `gc` and `rm` without `--force`** is `runner::plan(…)` empty and no fresh `progress.json` entry (`slots_owed`, `running_elsewhere`, `run_finished`, Task 6b; used in Tasks 8, 14). This widens spec 4b §7's and spec §2's "no Planned or Abandoned slots": a slot that spent its retry cap or its ten-abandonment bound owes nothing although the report shows it NotEvidence or Abandoned, and a run a live process is on is unfinished although the report may show no Planned slot.
- **F8 — `eval rm` without `--force` refuses a run a non-removable job names** (`held_runs`, Task 4b; Task 8; CLI test in Task 12a). `optimization::references` moved into PR 1.
- **F9 — A run frozen before `definition.json`** reports while the installed definition matches (test in Task 3).
- **F10 — Split tasks:** 1 / 1b, 2 / 2b, 7 / 7b, 10 / 10b; each has its own test cycle. (The binary smoke test is now Task 10c.)
- **F11 — `optimization/23-promote` @ `144be2916` matches Task 12's interfaces** ("The M4 base"); the draft's R6 is closed.
- **F12 — No agent tool surface reaches the eval operator verbs** (test beside `the_model_facing_config_tool_has_no_optimization_surface`, Task 8).
- **F13 — `--interval` refuses under 250 ms**; `gc --older-than` has its own parser that allows zero (Task 13).
- **F14 — Default ids are `<definition>-<unix ms>-<4 random hex>`** (`default_id`, Task 10); `--run-id`/`--job-id` are how an operator gets idempotent reuse (Tasks 10b, 11).
- **F15 — `resume` prints about the marker only after `thaw` let it run** (Task 10b).
- **F16 — The heartbeat writes at most every `max(marker_poll, 250 ms)`** (Tasks 6, 6a).
- `counted_slots` is at `evidence.rs:74` (Task 1b).

Planning deviations, verified against the code at `e4f3373b5` (contained in `<M4-BASE>`):

- **D1 — `build` takes the owner's run headers.** Spec §1 gives `build(run, trials, verdicts, definition)` but `EvalReport.exposure` counts other runs (`eval::scoring::exposure(runs: &[RunHeader], …)`), which that signature cannot see. The plan's signature is `build(run, trials, verdicts, definition, peers: &[RunHeader])`; `peers` includes the run itself.
- **D2 — Additive report fields.** `SlotReport.counted: SlotScore` (the slot's pairing score, which `compare` needs and `SlotClass` cannot carry: an Abandoned slot may still have an earlier completed attempt); `CellReport.cell_usage` (M6b's `evidence::CellUsage`, now `Serialize`: the cost gate's tokens, counted trials and missing usage, ruling F5); `Comparison.{baseline_run, baseline_cell, candidate_run, candidate_cell}` and four `#[serde(skip)]` fields holding the pairing state `with_policy` needs; `Comparison.mean_diff_bp` is `Option<i64>` (it is `decide`'s `mean_diff_bp`, `None` with no pairs or no common scale). PR 5 adds `SlotReport.verdicts` and `Comparison.trials` (spec 4b §8, "the `Comparison` JSON carries the per-verdict data"). `report_version` stays 1: every change is an added field.
- **D3 — The report reads the optimizer's pure statistics.** Spec §1 says `p_ppm` is `optimization::policy::permutation_p_ppm` over the pairs the policy would use. `permutation_p_ppm` takes scaled per-case differences whose scaling (`scaled_case_differences`) is private to `policy.rs`, so `compare` calls `decide(Mode::Improve, policy, &evidence, seed)` and reads `p_ppm`, `improved`, `tied`, `worsened` and `mean_diff_bp` from it; a test pins `p_ppm` to `permutation_p_ppm` on hand-computed differences. The seed is `optimization::evidence::decision_seed(&[baseline_run, candidate_run])`. The module doc in `report/mod.rs` that says "nothing in this module imports from `optimization`" is rewritten to "nothing here reads or writes an optimization document"; `optimization::policy` and `decision_seed` are pure.
- **D4 — An Abandoned slot keeps the score of an earlier completed attempt.** The class follows the latest row (spec §1); the score follows the latest *completed* attempt through `report::evidence::counted_slots`, which is the selection the optimizer pairs on. Both are shown.
- **D5 — `load_runs` lives in `eval::report::store`.** `eval::documents` has no list function and is frozen (standing rule 1). `store::load_runs` queries `EvalRun` for the owner's `run_id`s and calls the frozen `load_run` for each, so row decoding stays in one place.
- **D6 — Superseded by ruling U2.** Freeze writes `<run dir>/definition.json` and the report reads that copy (Tasks 2a, 3); `build` refuses only for a run frozen before the file existed (or whose directory was removed) whose installed definition no longer digests to the run's.
- **D7 — Where the cancel marker is checked.** The loop checks `<run dir>/cancel` at the top of each pass, before each launch, after each pass, and (ruling U5, Task 6a) on a `marker_poll` timer while a batch is in flight — every place it consults its token except the check right after a trial returns, where the evidence is already in hand. Seeing the marker cancels a **child** of the caller's token, which interrupts in-flight executors and never the optimization job hosting the run. `runner::resume` and, for an existing run id, `runner::run` remove the marker before planning (spec §3, ruling U8).
- **D8 — `progress.json` needs a stage hook on `TrialSpec`.** The loop never sees stage boundaries: `TrialExecutor::execute` runs every stage. Spec 4b §6's "stage start, stage end" therefore needs a handle the executor calls. `TrialSpec` gains `#[serde(skip)] pub progress: StageProgress` (no-op by default, ignored by equality); `EmbeddedExecutor::run_stages` calls it; the loop records slot start and slot end. This touches `executor.rs`, `embedded/executor.rs`, `mod.rs` and a new `progress.rs`.
- **D9 — Command bodies take their executor.** `dispatch` builds an `EmbeddedExecutor` (as spec §2 says) and hands every body a `Deps { executor: &dyn TrialExecutor, registry, cancel, options }`. CLI tests call the same bodies with `ScriptedExecutor`. What was found: the CLI has no seam for a scripted executor, and the canary's `MockStreamingBackend` lives in `crates/gents/tests/support/streaming_backend.rs`, which `gents-cli` cannot import; so `eval run` on `EmbeddedExecutor` is not exercised by any CLI test (U6). The in-crate harnesses the tests would reuse (`eval::runner::freeze::tests::Launching`, `optimization::driver::matrix::Harness`) are `#[cfg(test)] pub(crate)` inside `gents`, so the CLI crate carries its own small fixture (U7).
- **D10 — Flags the spec does not list.** `--profile <cell>=<profile_id>` (repeatable; spec §2 says "`--profile` per cell"); `eval run --max-infra-retries` (default 1) and `--registry`; `optimization run --profile`, `--max-case-trials` (default 10000), `--max-tokens` (default unlimited), `--registry`, `--json`; `eval watch --once`. Defaults the spec leaves open: `--seed-base 1000`, `--run-id <definition_id>-<UTC yyyymmddThhmmssZ>`, `--job-id` likewise. A cell with no `:<behavior>` uses the pack's only inference-slot behavior and is refused when it declares several.
- **D11 — `optimization revert <job_id> --digest D`** (ruling U3; the spec text is amended to match). M6b's `revert(access, owner, job_id, digest, by)` (ruling R6) confirms the promoted target digest.
- **D12 — Output goes through `&mut dyn std::io::Write`.** Repository rule: `tracing`, never `println!`. Bodies write to the writer `dispatch` passes (a locked stdout); diagnostics go to `tracing`. Tests capture a `Vec<u8>`.
- **D13 — Exit status.** A library refusal (`ReportRefused`, `FreezeRefused`, `AlreadyInvalidated`, `ProviderDown`, `JobRefused`, `PromoteRefused`) is re-raised with exactly its `Display` text (`surface_refusal`), and `main` exits 1. A malformed `--cell`, `--subject`, `--policy`, `--proposer`, `--split` or `--interval` is a clap value-parser error: exit 2. A usage problem only visible after reading documents (a multi-cell run with no `--baseline-cell`) exits 1.
- **D14 — Job references live in a new `optimization::references`** (Task 4b, in PR 1 by ruling F8): spec 4b §7 needs every `OptimizationJob` journal's run ids and `optimization::job` has only `load_job` by id. The file also holds `removable` (ruling F2) and `held_runs` (ruling F8); it adds a file to M6b's module and edits none.
- **D15 — Breakdowns read verdict scores, not imputed slot scores.** `by_check` and `by_stage` pair a key only when both slots are paired by `pair_trials` (Scored or Unknown) and then compare each side's acceptance, weighted, scored verdicts for that check or stage; a verdict with no `score_bp` contributes nothing.

## Global Constraints

Standing rules carried verbatim from `docs/superpowers/orchestration/2026-09-21-parallel-coordinators.md`:

1. **Interface freeze.** `gents::eval::{outcome, scoring, documents}` and `EvalDefinition` are frozen at the pinned commit. A task that needs a change there stops and messages the orchestrator.
2. **Build budget.** One cargo or lake invocation at a time per workspace, with `CARGO_BUILD_JOBS=4`. Foreground commands only: never background a command, never `sleep`, never poll. Long output goes to a log file that is then grepped.
3. **Git.** Commit with `git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com"`. End every commit message with `Co-Authored-By: <the implementing model> <noreply@anthropic.com>` for a Claude model, and `Co-Authored-By: Grok 4.7 <noreply@x.ai>` for Grok. (Correction 2026-09-22: commits made under the earlier wording carry `Grok 4.7 <noreply@anthropic.com>`; they are rewritten with `git rebase -x` before any push, since nothing is pushed.) Never push, never open a PR, never change git config. Worktrees are created with `make worktree BRANCH=<branch> DIR=<dir> BASE=<ref>` from the main checkout.
   **Superseded in part by the user's rule of 2026-09-22: commit messages carry NO `Co-Authored-By` trailer.** Every commit in this plan is written without one; the rest of rule 3 stands.
4. **Repo gates.** `cargo fmt --all --check` is CI. Lean imports are narrow: never `import Mathlib` or `import Mathlib.Tactic`; no `sorry`. Escape interpolated GraphQL; never emit `[]` in a mutation; `tracing`, never `println!`. The conformance test `generated_r5_cross_principal_cases_drive_production_dispatch` fails on unmodified `main` on this machine (libp2p dial timeout) and is not attributable to any task.
5. **Models.** Implementers and reviewers are dispatched with an explicit model, never inherited. Opus 5 is `opus`. Grok runs as a Herdr agent (`herdr agent start <name> --kind grok`) in a sibling pane and is driven with `herdr agent prompt … --wait`; its brief file is its whole interface, and it writes its report to the path the brief names.
6. **Process.** `superpowers:subagent-driven-development` for plans: ledger first, task brief, fresh implementer, review package, task review, fix rounds up to five, final whole-branch review. Rulings are recorded as `Ruling: <what> — <why> — <cost if wrong>` and never wait on a human.
7. **Reporting.** The ledger at `<worktree>/.superpowers/sdd/<plan>/progress.md` is the record. The coordinator messages the orchestrator (cross-session `SendMessage`; the orchestrator is the Claude session in pane `w3:p1`) only at: task complete, ruling made, blocked, plan defect found, plan complete. Messages carry no user authority in either direction.
8. **Scope.** Nothing outside the assigned worktree is read for editing or written. The design worktree `gents-design-optimization-substrate` is read-only for coordinators except their own `.superpowers/sdd/` directory.

Plus the constraints this milestone adds:

9. **No Lean is edited.** Everything here is plumbing that preserves semantics (CLAUDE.md "Foundation"). The §9 runner fix changes which slots the runner plans, not a modeled transition: `Proofs/Eval.lean` models outcome classes and scoring, not the retry cap (check `crates/gents/proofs/README.md`'s proof map; if it lists the retry cap, stop and message the orchestrator).
10. **The report is pure and never stored.** `report::build`, `report::compare` and PR 5's breakdowns do no I/O. `report::store` is the only I/O in `eval::report` besides M6b's `evidence::load_run_rows`. Nothing writes a report anywhere.
11. **One selection.** Slot scores come from `report::evidence::counted_slots` (made `pub(crate)` in Task 1), pairing from `eval::scoring::pair_trials`, decisions from `optimization::policy::decide`. No task re-derives any of them.
12. **CLI output never uses `println!`.** Bodies write to the `&mut dyn Write` they are given and return `anyhow::Result<()>`.
13. **Refusals are printed verbatim.** Never wrap a library refusal in `.context(…)` inside a command body; `surface_refusal` must be able to recover its exact text.
14. **Escape every interpolated GraphQL string** with `crate::graphql::escape_graphql_string`, and pass values as variables where the existing code does.
15. **rustfmt is a CI gate.** `cargo fmt --all --check` exits 0 before every commit. Where a task says "add a module", place the `mod` line where rustfmt sorts it.
16. **No `unwrap`/`expect` on input-dependent paths outside tests.**
17. **The marker, `progress.json` and run directories are not documents** and carry no identity. `rm` and `gc` delete directories and never documents.

## How to run things

Every cargo command below runs from the PR's worktree root, foreground, logging to the task's ledger directory:

```bash
LOG=.superpowers/sdd/2026-09-22-eval-report-and-cli/logs; mkdir -p "$LOG"
CARGO_BUILD_JOBS=4 cargo test -p gents --lib <filter> > "$LOG/<task>.log" 2>&1; grep -E "^test |test result|panicked|error(\[|:)" "$LOG/<task>.log" | tail -40
```

CLI unit tests live in the `gents_server` library (`crates/gents-cli/src/lib.rs`), so their filter form is `cargo test -p gents-cli --lib commands::eval`.

---
## PR 1: The report, and the loop changes the operator needs

Branch `eval/50-report`, base `<M4-BASE>`. Worktree: `make worktree BRANCH=eval/50-report DIR=../gents-eval-50-report BASE=<M4-BASE>`.

| File | Responsibility |
|---|---|
| `crates/gents/src/eval/report/mod.rs` | Module wiring, `ReportRefused`, the `pub use` surface |
| `crates/gents/src/eval/report/evidence.rs` | One visibility change: `counted_slots` becomes `pub(crate)` |
| `crates/gents/src/eval/report/build.rs` | `EvalReport` and its parts, `SlotClass`, `SlotScore`, `build` |
| `crates/gents/src/eval/report/compare.rs` | `Comparison`, `CaseComparison`, `PolicyOutcome`, `GateView`, `compare`, `Comparison::with_policy` |
| `crates/gents/src/eval/report/store.rs` | `load_runs`, `run_header`, `load_report_among`, `load_report` |
| `crates/gents/src/eval/report/fixtures.rs` | `#[cfg(test)]` hand-built documents for `build` and `compare` tests |
| `crates/gents/src/eval/runner/freeze.rs` | `definition.json`: written at freeze, read and verified by `thaw` and the report |
| `crates/gents/src/eval/runner/mod.rs` | The cancel marker (at launches and on a timer mid-batch), `run`/`resume` clearing it, the progress writer and heartbeat wired into the loop |
| `crates/gents/src/eval/runner/plan.rs` | The abandoned-attempt bound |
| `crates/gents/src/eval/runner/progress.rs` | `progress.json`: `Progress`, `InFlight`, `StageProgress`, `read_progress` |
| `crates/gents/src/eval/runner/executor.rs` | `TrialSpec.progress` |
| `crates/gents/src/eval/runner/embedded/executor.rs` | Stage start and end reported from `run_stages` |
| `crates/gents/src/optimization/driver/matrix.rs` | Two doc comments made true again by the §9 fix |

### Task 1: The report's types and the test fixtures

**Files:**
- Create: `crates/gents/src/eval/report/build.rs` (the types; Task 1b adds `build`)
- Create: `crates/gents/src/eval/report/fixtures.rs`
- Modify: `crates/gents/src/eval/report/mod.rs` (whole file, 13 lines today)
- Modify: `crates/gents/src/eval/report/evidence.rs` (`CellUsage` derives `serde::Serialize`)

**Interfaces:**
- Consumes (read at `e4f3373b5`, contained in `<M4-BASE>`):
  ```rust
  // crates/gents/src/eval/report/evidence.rs
  pub struct RunRows { pub run_id: String, pub case_ids: Vec<String>, pub invalidated: bool,
      pub trials: Vec<TrialRecord>, pub verdicts: Vec<VerdictRecord> }
  fn counted_slots<'a>(definition: &'a EvalDefinition, rows: &'a RunRows, cell_id: &str)
      -> Vec<(&'a TrialRecord, &'a EvalCase, Vec<VerdictView>)>;   // made pub(crate) here
  // crates/gents/src/eval/scoring.rs (frozen)
  pub fn case_trial_score(reducer: EvalReducer, verdicts: &[VerdictView]) -> CaseTrialScore;
  pub enum CaseTrialScore { NotEvidence, Unknown, Scored(u32) }
  pub struct TrialScore { pub case_id: String, pub trial_index: u32, pub score: CaseTrialScore }
  pub fn case_means_bp(trials: &[TrialScore]) -> BTreeMap<String, u32>;
  pub fn headline_bp(trials: &[TrialScore]) -> Option<u32>;
  pub struct RunHeader { pub definition_id: String, pub comparability_version: i64, pub split: EvalSplit, pub invalidated: bool }
  pub fn exposure(runs: &[RunHeader], definition_id: &str, comparability_version: i64, split: EvalSplit) -> usize;
  // crates/gents/src/eval/outcome.rs (frozen)
  pub fn classify(kind: OutcomeKind, reason: Option<ProviderReason>) -> EvidenceClass;
  // crates/gents/src/eval/documents.rs (frozen)
  pub struct RunRecord { pub run_id, pub owner, pub evaluator_did, pub origin: RunOrigin, pub created_at: String, pub invalidated: Option<Invalidation> }
  pub struct TrialRecord { pub identity: TrialIdentity, pub created_at: String, pub completion: Option<TrialCompletion> }
  pub struct TrialCompletion { pub ended_at, pub stages: Vec<StageCompletion>, pub usage: TrialUsage, pub anchor: Anchor, pub evidence_digest: Option<String> }
  pub type VerdictRecord = VerdictDraft;  // verdict_id, run_id, trial_id, stage_id, check, check_version, tier, kind, provider_reason, score_bp, weight, raw, feedback, regrade_of
  // crates/gents/src/eval/runner/freeze.rs
  pub(crate) fn definition_ref(definition: &EvalDefinition) -> Result<DefinitionRef>;
  ```
- Produces:
  ```rust
  // crates/gents/src/eval/report/mod.rs
  pub struct ReportRefused(pub String);                       // Display = the String
  pub fn report_refused(error: &anyhow::Error) -> Option<&ReportRefused>;
  pub(crate) fn refused(reason: impl Into<String>) -> anyhow::Error;
  // crates/gents/src/eval/report/build.rs, re-exported from eval::report
  pub const REPORT_VERSION: u32 = 1;
  pub struct EvalReport { pub report_version: u32, pub run: RunSummary, pub cells: Vec<CellReport>, pub exposure: usize, pub definition_changed: bool }
  pub struct RunSummary { run_id, definition: DefinitionRef, split: EvalSplit, purpose, created_at,
      invalidated: Option<Invalidation>, trials_per_case: u32, seed_base: i64, concurrency: u32,
      max_infra_retries: u32, breaker_threshold: u32, source_commit: String, source_dirty: bool }
  pub struct CellReport { cell_id, label, subject: SubjectRef, slots: Vec<SlotReport>, cases: Vec<CaseReport>,
      headline_bp: Option<u32>, usage: TrialUsage, cell_usage: CellUsage, attempts: u32, counts: SlotCounts }
  pub struct SlotReport { case_id, trial_index: u32, class: SlotClass, attempts: u32, score_bp: Option<u32>,
      counted: SlotScore, latest: Option<AttemptSummary> }
  pub enum SlotClass { Pass, Fail, Unknown, NotEvidence, Abandoned, Planned }
  pub enum SlotScore { Absent, NotEvidence, Unknown, Scored(u32) }
  impl SlotScore { pub fn trial_score(self) -> Option<CaseTrialScore> }
  pub struct AttemptSummary { trial_id, attempt: u32, trial_agent_did, session_id, home_hint: Option<String>,
      evidence_digest: Option<String>, stages: Vec<StageCompletion>, usage: TrialUsage }
  pub struct CaseReport { case_id, reducer: EvalReducer, mean_bp: Option<u32>, counts: SlotCounts }
  pub struct SlotCounts { pass: u32, fail: u32, unknown: u32, not_evidence: u32, abandoned: u32, planned: u32 }
  // crates/gents/src/eval/report/fixtures.rs (#[cfg(test)] pub(crate))
  pub(crate) const RUN: &str = "run";
  pub(crate) fn definition(cases: &[&str]) -> EvalDefinition;
  pub(crate) fn record(run_id: &str, definition: &EvalDefinition, cells: &[&str], trials_per_case: u32) -> RunRecord;
  pub(crate) struct At { pub cell: &'static str, pub case: &'static str, pub index: u32, pub attempt: u32 }
  pub(crate) fn at(cell: &'static str, case: &'static str, index: u32, attempt: u32) -> At;
  pub(crate) enum Outcome { Open, Verdict(OutcomeKind, Option<u32>) }
  pub(crate) fn pass() -> Outcome;  pub(crate) fn fail() -> Outcome;
  pub(crate) fn trial_id(run_id: &str, at: At) -> String;
  pub(crate) struct Rows { pub trials: Vec<TrialRecord>, pub verdicts: Vec<VerdictRecord> }
  impl Rows { pub(crate) fn add(self, run_id: &str, at: At, outcome: Outcome) -> Self; pub(crate) fn unmetered(self) -> Self }
  ```
  `CellUsage` (M6b's `evidence.rs`, `{ tokens: u64, trials: u64, missing: u64 }`) gains `serde::Serialize` here.
  All report types derive `Clone, Debug, PartialEq, Eq, Serialize`; `SlotClass`, `SlotScore` and `SlotCounts` are also `Copy`, and `SlotCounts` is `Default`. Enums serialize `snake_case`; `SlotScore` is `#[serde(tag = "kind", content = "bp")]`.

- [ ] **Step 0: Verify the base**

From the worktree root:

```bash
git merge-base --is-ancestor e4f3373b5 HEAD && echo base-ok
git grep -n "pub async fn promote" -- crates/gents/src/optimization/promote.rs
git grep -n "pub async fn revert" -- crates/gents/src/optimization/promote.rs
git grep -n "pub mod evidence" -- crates/gents/src/eval/report/mod.rs
git grep -n "fn counted_slots" -- crates/gents/src/eval/report/evidence.rs
git grep -n "pub fn permutation_p_ppm" -- crates/gents/src/optimization/policy.rs
git grep -n "pub async fn show" -- crates/gents/src/optimization/show.rs
git grep -n "pub async fn resume" -- crates/gents/src/eval/runner/mod.rs
```

Expected: `base-ok`, then exactly one line from each `git grep`. If any prints nothing, stop and message the orchestrator.

- [ ] **Step 1: Write the fixtures and the failing tests**

Create `crates/gents/src/eval/report/fixtures.rs`:

```rust
//! Hand-built eval documents for the report's unit tests: a definition of
//! one-stage cases, a frozen run over it, and trials with one acceptance
//! verdict each, built slot by slot.

use serde_json::json;

use crate::document_config::{EvalDefinition, EvalSplit, EvalTier};
use crate::eval::runner::freeze::definition_ref;
use crate::eval::{
    Anchor, CellSpec, OutcomeKind, RunOrigin, RunRecord, StageCompletion, SubjectRef,
    TrialCompletion, TrialIdentity, TrialRecord, TrialUsage, VerdictRecord,
    DENOMINATOR_POLICY_V1, TAXONOMY_VERSION,
};

pub(crate) const RUN: &str = "run";

/// One validation case per id, each one stage `check` with one acceptance
/// `captured_rows_count` check.
pub(crate) fn definition(cases: &[&str]) -> EvalDefinition {
    let cases: Vec<serde_json::Value> = cases
        .iter()
        .map(|case_id| {
            json!({
                "case_id": case_id,
                "split": "validation",
                "stages": [{
                    "stage_id": "check",
                    "prompt": "Run the monitor.",
                    "deadline_secs": 600,
                    "checks": [{
                        "check": "captured_rows_count",
                        "params": {"name": "findings", "min": 1},
                        "tier": "acceptance",
                    }],
                }],
            })
        })
        .collect();
    serde_json::from_value(json!({
        "definition_id": "report-def",
        "agent_did": "did:key:owner",
        "comparability_version": 1,
        "subject": {"kind": "behavior", "inference_slots": ["primary"]},
        "cases": cases,
    }))
    .expect("the fixture definition parses")
}

/// A run frozen over every case of `definition`, one row per cell id.
pub(crate) fn record(
    run_id: &str,
    definition: &EvalDefinition,
    cells: &[&str],
    trials_per_case: u32,
) -> RunRecord {
    let mut case_ids: Vec<String> = definition
        .cases
        .iter()
        .map(|case| case.case_id.clone())
        .collect();
    case_ids.sort();
    RunRecord {
        run_id: run_id.into(),
        owner: "did:key:owner".into(),
        evaluator_did: "did:key:owner".into(),
        origin: RunOrigin {
            definition: definition_ref(definition).expect("the fixture definition digests"),
            split: EvalSplit::Validation,
            case_ids,
            cells: cells
                .iter()
                .map(|cell_id| CellSpec {
                    cell_id: (*cell_id).into(),
                    label: (*cell_id).into(),
                    subject: SubjectRef {
                        pack_digest: "sha256:pack".into(),
                        behavior_id: "monitor".into(),
                    },
                    inference_profile_id: "local".into(),
                })
                .collect(),
            trials_per_case,
            seed_base: 1_000,
            deadline_secs: None,
            concurrency: 1,
            denominator_policy: DENOMINATOR_POLICY_V1.into(),
            taxonomy_version: TAXONOMY_VERSION.into(),
            max_infra_retries: 1,
            check_registry_version: "checks-1".into(),
            source_commit: "fixture".into(),
            source_dirty: false,
            purpose: "eval".into(),
            breaker_threshold: 5,
        },
        created_at: "2026-09-22T00:00:00Z".into(),
        invalidated: None,
    }
}

/// Where one attempt sits.
#[derive(Clone, Copy, Debug)]
pub(crate) struct At {
    pub cell: &'static str,
    pub case: &'static str,
    pub index: u32,
    pub attempt: u32,
}

pub(crate) fn at(cell: &'static str, case: &'static str, index: u32, attempt: u32) -> At {
    At {
        cell,
        case,
        index,
        attempt,
    }
}

/// What one attempt came to.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Outcome {
    /// Launched and never completed: a null completion and no verdict.
    Open,
    /// Completed with one acceptance verdict of this kind and score.
    Verdict(OutcomeKind, Option<u32>),
}

pub(crate) fn pass() -> Outcome {
    Outcome::Verdict(OutcomeKind::Passed, Some(10_000))
}

pub(crate) fn fail() -> Outcome {
    Outcome::Verdict(OutcomeKind::ModelAcceptance, Some(0))
}

pub(crate) fn trial_id(run_id: &str, at: At) -> String {
    format!(
        "{run_id}-{}-{}-{}-{}",
        at.cell, at.case, at.index, at.attempt
    )
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Rows {
    pub trials: Vec<TrialRecord>,
    pub verdicts: Vec<VerdictRecord>,
}

impl Rows {
    /// One attempt; a completed one reports ten input and ten output tokens.
    pub(crate) fn add(mut self, run_id: &str, at: At, outcome: Outcome) -> Self {
        let trial_id = trial_id(run_id, at);
        let completion = match outcome {
            Outcome::Open => None,
            Outcome::Verdict(..) => Some(TrialCompletion {
                ended_at: "2026-09-22T00:01:00Z".into(),
                stages: vec![StageCompletion {
                    stage_id: "check".into(),
                    request_id: Some(format!("{trial_id}-request")),
                    terminal_state: None,
                    failure_kind: None,
                    provider_reason: None,
                }],
                usage: TrialUsage {
                    input_tokens: Some(10),
                    output_tokens: Some(10),
                },
                anchor: Anchor {
                    terminal_states: Vec::new(),
                    requests: 1,
                    inference_calls: 1,
                },
                evidence_digest: Some(format!("sha256:{trial_id}")),
            }),
        };
        self.trials.push(TrialRecord {
            identity: TrialIdentity {
                trial_id: trial_id.clone(),
                run_id: run_id.into(),
                cell_id: at.cell.into(),
                case_id: at.case.into(),
                trial_index: at.index,
                attempt: at.attempt,
                trial_agent_did: "did:key:trial".into(),
                session_id: format!("session-{trial_id}"),
                seed: 1_000 + i64::from(at.index),
                home_hint: Some(format!("{run_id}/trials/{trial_id}")),
            },
            created_at: "2026-09-22T00:00:00Z".into(),
            completion,
        });
        if let Outcome::Verdict(kind, score_bp) = outcome {
            self.verdicts.push(VerdictRecord {
                verdict_id: format!("{trial_id}-v"),
                run_id: run_id.into(),
                trial_id,
                stage_id: "check".into(),
                check: "captured_rows_count".into(),
                check_version: "1".into(),
                tier: EvalTier::Acceptance,
                kind,
                provider_reason: None,
                score_bp,
                weight: 1,
                raw: json!({"reason_code": "fixture"}),
                feedback: None,
                regrade_of: None,
            });
        }
        self
    }

    /// The attempt added last reported no usage at all.
    pub(crate) fn unmetered(mut self) -> Self {
        if let Some(completion) = self
            .trials
            .last_mut()
            .and_then(|trial| trial.completion.as_mut())
        {
            completion.usage = TrialUsage::default();
        }
        self
    }
}
```

Create `crates/gents/src/eval/report/build.rs` holding only its test module:

```rust
#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::eval::report::fixtures::{
        at, definition, fail, pass, record, trial_id, Outcome, Rows, RUN,
    };

    #[test]
    fn slot_scores_map_to_what_pairing_reads_and_serialize_by_name() {
        assert_eq!(SlotScore::Absent.trial_score(), None);
        assert_eq!(
            SlotScore::NotEvidence.trial_score(),
            Some(CaseTrialScore::NotEvidence)
        );
        assert_eq!(SlotScore::Unknown.trial_score(), Some(CaseTrialScore::Unknown));
        assert_eq!(
            SlotScore::Scored(7).trial_score(),
            Some(CaseTrialScore::Scored(7))
        );
        assert_eq!(
            serde_json::to_value(SlotScore::Scored(10_000)).unwrap(),
            json!({"kind": "scored", "bp": 10_000})
        );
        assert_eq!(
            serde_json::to_value(SlotScore::Absent).unwrap(),
            json!({"kind": "absent"})
        );
        assert_eq!(
            serde_json::to_value(SlotClass::NotEvidence).unwrap(),
            json!("not_evidence")
        );
        let mut counts = SlotCounts::default();
        for class in [
            SlotClass::Pass,
            SlotClass::Fail,
            SlotClass::Unknown,
            SlotClass::NotEvidence,
            SlotClass::Abandoned,
            SlotClass::Planned,
        ] {
            counts.add(class);
        }
        assert_eq!(
            counts,
            SlotCounts {
                pass: 1,
                fail: 1,
                unknown: 1,
                not_evidence: 1,
                abandoned: 1,
                planned: 1,
            }
        );
    }

    #[test]
    fn the_fixtures_build_one_row_per_attempt_and_one_verdict_per_completion() {
        let rows = Rows::default()
            .add(RUN, at("base", "disk", 0, 1), fail())
            .add(RUN, at("base", "disk", 0, 2), pass())
            .unmetered()
            .add(RUN, at("base", "disk", 1, 1), Outcome::Open);
        assert_eq!(rows.trials.len(), 3);
        assert_eq!(rows.verdicts.len(), 2);
        assert_eq!(
            rows.verdicts[1].trial_id,
            trial_id(RUN, at("base", "disk", 0, 2))
        );
        let usage = &rows.trials[1].completion.as_ref().unwrap().usage;
        assert_eq!((usage.input_tokens, usage.output_tokens), (None, None));
        assert!(rows.trials[2].completion.is_none());
        let definition = definition(&["disk"]);
        let run = record(RUN, &definition, &["base"], 1);
        assert_eq!(run.origin.case_ids, vec!["disk".to_owned()]);
    }
}
```

Replace `crates/gents/src/eval/report/mod.rs` with:

```rust
//! Pure projections from eval documents to comparable numbers.
//!
//! Unfrozen by design (ruling R3). `evidence` is the verdict-to-evidence
//! projection the optimizer and the operator share; `build` is spec 4a's
//! versioned report over it. `build`, `compare` and the breakdowns are pure;
//! `store` (Task 3) is the module's one I/O boundary, loading rows for an
//! owner, alongside `evidence::load_run_rows` (ruling U9). Reports are derived on demand and never stored,
//! so a regrade changes a report with no migration. `compare` (Task 2)
//! reuses the optimizer's pure statistics so an operator's p-value is the
//! optimizer's; nothing here reads or writes an optimization document.

pub mod build;
pub mod evidence;
#[cfg(test)]
pub(crate) mod fixtures;

pub use build::{
    AttemptSummary, CaseReport, CellReport, EvalReport, RunSummary, SlotClass, SlotCounts,
    SlotReport, SlotScore, REPORT_VERSION,
};
pub use evidence::{
    cell_trial_scores, cell_usage, concat_paired, counted_verdicts, latest_attempts, load_run_rows,
    paired_evidence, CellUsage, RunRows,
};

/// Documents a report cannot be built or compared from, and why.
#[derive(Debug)]
pub struct ReportRefused(pub String);

impl std::fmt::Display for ReportRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ReportRefused {}

pub fn report_refused(error: &anyhow::Error) -> Option<&ReportRefused> {
    error.downcast_ref::<ReportRefused>()
}

pub(crate) fn refused(reason: impl Into<String>) -> anyhow::Error {
    anyhow::Error::from(ReportRefused(reason.into()))
}
```

In `crates/gents/src/eval/report/evidence.rs`, change `#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]` on `pub struct CellUsage` to `#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]`.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents --lib eval::report::build > "$LOG/t1.log" 2>&1; grep -E "error(\[|:)|test result" "$LOG/t1.log" | head -20`
Expected: compile errors `cannot find type `SlotScore``, `SlotClass`, `SlotCounts`.

- [ ] **Step 3: Write the implementation**

Insert above the test module in `crates/gents/src/eval/report/build.rs`:

```rust
//! `build`: one run's documents as the operator reads them (spec 4a §1).
//!
//! Pure. The unit is the slot `(cell, case, trial_index)`, classified by its
//! latest attempt; attempts are a separate total, so a retry never inflates
//! what completed. A slot's score is read at its latest *completed* attempt
//! through `evidence::counted_slots`, the selection the optimizer pairs on,
//! so a report and a decision never disagree about what a slot scored.

use serde::Serialize;

use crate::document_config::{EvalReducer, EvalSplit};
use crate::eval::report::evidence::CellUsage;
use crate::eval::{
    CaseTrialScore, DefinitionRef, Invalidation, StageCompletion, SubjectRef, TrialUsage,
};

/// Bumped when a field's meaning changes; an added field does not bump it.
pub const REPORT_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct EvalReport {
    pub report_version: u32,
    pub run: RunSummary,
    pub cells: Vec<CellReport>,
    /// Non-invalidated runs of this definition version on this split,
    /// this one included.
    pub exposure: usize,
    /// The installed definition no longer digests to the one the run froze
    /// (or is gone). `build` never sets it; `report::store` does, from the
    /// run's frozen copy (ruling U2).
    pub definition_changed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RunSummary {
    pub run_id: String,
    pub definition: DefinitionRef,
    pub split: EvalSplit,
    pub purpose: String,
    pub created_at: String,
    pub invalidated: Option<Invalidation>,
    pub trials_per_case: u32,
    pub seed_base: i64,
    pub concurrency: u32,
    pub max_infra_retries: u32,
    pub breaker_threshold: u32,
    pub source_commit: String,
    pub source_dirty: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CellReport {
    pub cell_id: String,
    pub label: String,
    pub subject: SubjectRef,
    pub slots: Vec<SlotReport>,
    pub cases: Vec<CaseReport>,
    pub headline_bp: Option<u32>,
    /// Summed over each slot's counted attempt.
    pub usage: TrialUsage,
    /// `evidence::cell_usage` for this cell: the numbers the cost gate reads
    /// (ruling F5), so `compare` and the optimizer share one tally.
    pub cell_usage: CellUsage,
    /// Trial rows of this cell, every attempt included.
    pub attempts: u32,
    pub counts: SlotCounts,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SlotReport {
    pub case_id: String,
    pub trial_index: u32,
    pub class: SlotClass,
    pub attempts: u32,
    pub score_bp: Option<u32>,
    /// What pairing reads for this slot.
    pub counted: SlotScore,
    /// The latest row, completed or not.
    pub latest: Option<AttemptSummary>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SlotClass {
    Pass,
    Fail,
    Unknown,
    NotEvidence,
    /// The latest row has a null completion: crashed or cancelled.
    Abandoned,
    /// No row yet.
    Planned,
}

/// A slot's counted case-trial score, or `Absent` when no attempt completed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "bp")]
pub enum SlotScore {
    Absent,
    NotEvidence,
    Unknown,
    Scored(u32),
}

impl SlotScore {
    fn of(score: Option<CaseTrialScore>) -> Self {
        match score {
            None => Self::Absent,
            Some(CaseTrialScore::NotEvidence) => Self::NotEvidence,
            Some(CaseTrialScore::Unknown) => Self::Unknown,
            Some(CaseTrialScore::Scored(bp)) => Self::Scored(bp),
        }
    }

    /// The score `pair_trials` reads, or `None` for a slot it never saw.
    pub fn trial_score(self) -> Option<CaseTrialScore> {
        match self {
            Self::Absent => None,
            Self::NotEvidence => Some(CaseTrialScore::NotEvidence),
            Self::Unknown => Some(CaseTrialScore::Unknown),
            Self::Scored(bp) => Some(CaseTrialScore::Scored(bp)),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AttemptSummary {
    pub trial_id: String,
    pub attempt: u32,
    pub trial_agent_did: String,
    pub session_id: String,
    /// Relative to `<launching home>/eval/runs`. A locator, never identity.
    pub home_hint: Option<String>,
    pub evidence_digest: Option<String>,
    /// Empty while the attempt is open.
    pub stages: Vec<StageCompletion>,
    pub usage: TrialUsage,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CaseReport {
    pub case_id: String,
    pub reducer: EvalReducer,
    pub mean_bp: Option<u32>,
    pub counts: SlotCounts,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct SlotCounts {
    pub pass: u32,
    pub fail: u32,
    pub unknown: u32,
    pub not_evidence: u32,
    pub abandoned: u32,
    pub planned: u32,
}

impl SlotCounts {
    fn add(&mut self, class: SlotClass) {
        match class {
            SlotClass::Pass => self.pass += 1,
            SlotClass::Fail => self.fail += 1,
            SlotClass::Unknown => self.unknown += 1,
            SlotClass::NotEvidence => self.not_evidence += 1,
            SlotClass::Abandoned => self.abandoned += 1,
            SlotClass::Planned => self.planned += 1,
        }
    }
}

```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents --lib eval::report > "$LOG/t1.log" 2>&1; grep -E "^test |test result|panicked" "$LOG/t1.log" | tail -30`
Expected: the two type tests pass, and M6b's `evidence::tests` still pass.

- [ ] **Step 5: Format and commit**

```bash
cargo fmt --all && cargo fmt --all --check
git add crates/gents/src/eval/report/
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" commit -m "feat(eval): the report's types and hand-built eval documents for its tests (#1515)"
```

### Task 1b: `report::build` and the slot classification

**Files:**
- Modify: `crates/gents/src/eval/report/build.rs` (imports; `build`, `cell_report`, `slot_class`, `attempt_summary`, `sum`; tests)
- Modify: `crates/gents/src/eval/report/mod.rs` (add `build` to the `pub use build::{…}` list)
- Modify: `crates/gents/src/eval/report/evidence.rs:74` (`fn counted_slots<'a>(` → `pub(crate) fn counted_slots<'a>(`)

**Interfaces:**
- Consumes: Task 1's types and fixtures; `evidence::{counted_slots, cell_usage, RunRows}`; the frozen `eval::scoring` and `eval::outcome` functions and `freeze::definition_ref`, as listed in Task 1.
- Produces:
  ```rust
  pub fn build(run: &RunRecord, trials: &[TrialRecord], verdicts: &[VerdictRecord],
      definition: &EvalDefinition, peers: &[RunHeader]) -> Result<EvalReport>;
  ```

- [ ] **Step 1: Write the failing tests**

Append inside the test module in `crates/gents/src/eval/report/build.rs` (created in Task 1; it already imports `super::*` and the fixtures, so only these imports are new):

```rust
    use crate::document_config::EvalSplit;
    use crate::eval::report::evidence::CellUsage;
    use crate::eval::report::report_refused;
    use crate::eval::{OutcomeKind, RunHeader, TrialUsage, VerdictRecord};

    fn classes(report: &EvalReport) -> Vec<SlotClass> {
        report.cells[0].slots.iter().map(|slot| slot.class).collect()
    }

    fn refusal(result: Result<EvalReport>) -> String {
        let error = result.expect_err("expected a refusal");
        report_refused(&error)
            .unwrap_or_else(|| panic!("expected a ReportRefused, got {error:#}"))
            .0
            .clone()
    }

    #[test]
    fn every_slot_class_is_reached_from_the_documents() {
        let definition = definition(&["disk"]);
        let run = record(RUN, &definition, &["base"], 6);
        let rows = Rows::default()
            .add(RUN, at("base", "disk", 0, 1), pass())
            .add(RUN, at("base", "disk", 1, 1), fail())
            .add(
                RUN,
                at("base", "disk", 2, 1),
                Outcome::Verdict(OutcomeKind::Grader, None),
            )
            .add(
                RUN,
                at("base", "disk", 3, 1),
                Outcome::Verdict(OutcomeKind::Infrastructure, None),
            )
            .add(RUN, at("base", "disk", 4, 1), Outcome::Open);
        let report = build(&run, &rows.trials, &rows.verdicts, &definition, &[]).unwrap();

        assert_eq!(report.report_version, REPORT_VERSION);
        assert_eq!(
            classes(&report),
            vec![
                SlotClass::Pass,
                SlotClass::Fail,
                SlotClass::Unknown,
                SlotClass::NotEvidence,
                SlotClass::Abandoned,
                SlotClass::Planned,
            ]
        );
        let cell = &report.cells[0];
        let one_each = SlotCounts {
            pass: 1,
            fail: 1,
            unknown: 1,
            not_evidence: 1,
            abandoned: 1,
            planned: 1,
        };
        assert_eq!(cell.counts, one_each);
        assert_eq!(cell.cases[0].counts, one_each);
        assert_eq!(cell.attempts, 5, "the planned slot has no row");
        // An unknown counts as a failure in a mean; a not-evidence slot and
        // slots with no completed attempt are left out: (10000 + 0 + 0) / 3.
        assert_eq!(cell.cases[0].mean_bp, Some(3_333));
        assert_eq!(cell.headline_bp, Some(3_333));
        assert_eq!(
            cell.slots.iter().map(|slot| slot.counted).collect::<Vec<_>>(),
            vec![
                SlotScore::Scored(10_000),
                SlotScore::Scored(0),
                SlotScore::Unknown,
                SlotScore::NotEvidence,
                SlotScore::Absent,
                SlotScore::Absent,
            ]
        );
        let abandoned = cell.slots[4].latest.as_ref().expect("the open row");
        assert!(abandoned.stages.is_empty() && abandoned.evidence_digest.is_none());
        assert!(cell.slots[5].latest.is_none());
    }

    #[test]
    fn a_scored_slot_passes_only_when_every_acceptance_verdict_passes() {
        let definition = definition(&["disk"]);
        let run = record(RUN, &definition, &["base"], 1);
        let mut rows = Rows::default().add(RUN, at("base", "disk", 0, 1), pass());
        let second = VerdictRecord {
            verdict_id: "second".into(),
            check: "second_check".into(),
            kind: OutcomeKind::ModelAcceptance,
            score_bp: Some(0),
            ..rows.verdicts[0].clone()
        };
        rows.verdicts.push(second);
        let report = build(&run, &rows.trials, &rows.verdicts, &definition, &[]).unwrap();
        let slot = &report.cells[0].slots[0];
        assert_eq!(
            (slot.class, slot.score_bp),
            (SlotClass::Fail, Some(5_000)),
            "a weighted mean of 5000 is still a failing slot"
        );
    }

    #[test]
    fn a_regrade_changes_the_slots_score_and_is_not_an_attempt() {
        let definition = definition(&["disk"]);
        let run = record(RUN, &definition, &["base"], 1);
        let mut rows = Rows::default().add(RUN, at("base", "disk", 0, 1), fail());
        let before = build(&run, &rows.trials, &rows.verdicts, &definition, &[]).unwrap();
        let slot = &before.cells[0].slots[0];
        assert_eq!((slot.class, slot.score_bp), (SlotClass::Fail, Some(0)));

        let original = rows.verdicts[0].clone();
        rows.verdicts.push(VerdictRecord {
            verdict_id: "regrade".into(),
            kind: OutcomeKind::Passed,
            score_bp: Some(10_000),
            regrade_of: Some(original.verdict_id.clone()),
            ..original
        });
        let after = build(&run, &rows.trials, &rows.verdicts, &definition, &[]).unwrap();
        let slot = &after.cells[0].slots[0];
        assert_eq!((slot.class, slot.score_bp), (SlotClass::Pass, Some(10_000)));
        assert_eq!(after.cells[0].attempts, 1);
    }

    #[test]
    fn attempts_and_slots_are_counted_separately() {
        let definition = definition(&["disk"]);
        let run = record(RUN, &definition, &["base"], 2);
        let rows = Rows::default()
            .add(
                RUN,
                at("base", "disk", 0, 1),
                Outcome::Verdict(OutcomeKind::Infrastructure, None),
            )
            .add(RUN, at("base", "disk", 0, 2), pass())
            .add(RUN, at("base", "disk", 1, 1), pass())
            .add(RUN, at("base", "disk", 1, 2), Outcome::Open);
        let report = build(&run, &rows.trials, &rows.verdicts, &definition, &[]).unwrap();
        let cell = &report.cells[0];
        assert_eq!(cell.attempts, 4);
        assert_eq!((cell.counts.pass, cell.counts.abandoned), (1, 1));
        assert_eq!(
            (cell.slots[0].class, cell.slots[0].attempts),
            (SlotClass::Pass, 2),
            "a retry that produced evidence is one slot, not two completions"
        );
        let abandoned = &cell.slots[1];
        assert_eq!(abandoned.class, SlotClass::Abandoned);
        assert_eq!(
            (abandoned.counted, abandoned.score_bp),
            (SlotScore::Scored(10_000), Some(10_000)),
            "the score is the latest completed attempt's, as the optimizer pairs it"
        );
        assert_eq!(
            abandoned.latest.as_ref().map(|latest| latest.attempt),
            Some(2)
        );
        assert_eq!(cell.cases[0].mean_bp, Some(10_000));
    }

    #[test]
    fn usage_sums_the_counted_attempts_and_counts_the_unmetered() {
        let definition = definition(&["disk"]);
        let run = record(RUN, &definition, &["base"], 2);
        let rows = Rows::default()
            .add(RUN, at("base", "disk", 0, 1), pass())
            .add(RUN, at("base", "disk", 1, 1), pass())
            .unmetered();
        let report = build(&run, &rows.trials, &rows.verdicts, &definition, &[]).unwrap();
        let cell = &report.cells[0];
        assert_eq!(
            cell.usage,
            TrialUsage {
                input_tokens: Some(10),
                output_tokens: Some(10),
            }
        );
        assert_eq!(
            cell.cell_usage,
            CellUsage {
                tokens: 20,
                trials: 2,
                missing: 1,
            }
        );
    }

    #[test]
    fn exposure_counts_the_non_invalidated_runs_of_this_definition_and_split() {
        let definition = definition(&["disk"]);
        let run = record(RUN, &definition, &["base"], 1);
        let header = |split, invalidated| RunHeader {
            definition_id: "report-def".into(),
            comparability_version: 1,
            split,
            invalidated,
        };
        let peers = [
            header(EvalSplit::Validation, false),
            header(EvalSplit::Validation, false),
            header(EvalSplit::Validation, true),
            header(EvalSplit::Train, false),
        ];
        let report = build(&run, &[], &[], &definition, &peers).unwrap();
        assert_eq!(report.exposure, 2);
        assert_eq!(classes(&report), vec![SlotClass::Planned]);
    }

    #[test]
    fn build_refuses_documents_it_cannot_read_against_the_run() {
        let definition = definition(&["disk"]);
        let run = record(RUN, &definition, &["base"], 1);

        let mut edited = definition.clone();
        edited.comparability_version = 2;
        assert!(refusal(build(&run, &[], &[], &edited, &[])).contains("no longer digests"));

        let foreign = Rows::default().add("other", at("base", "disk", 0, 1), pass());
        let reason = refusal(build(&run, &foreign.trials, &[], &definition, &[]));
        assert!(reason.contains("belongs to run other"), "{reason}");

        let mut rows = Rows::default().add(RUN, at("base", "disk", 0, 1), pass());
        rows.verdicts[0].run_id = "other".into();
        let reason = refusal(build(&run, &rows.trials, &rows.verdicts, &definition, &[]));
        assert!(
            reason.starts_with("verdict ") && reason.contains("belongs to run other"),
            "{reason}"
        );

        let ghost = Rows::default().add(RUN, at("ghost", "disk", 0, 1), pass());
        let reason = refusal(build(&run, &ghost.trials, &[], &definition, &[]));
        assert!(reason.contains("did not freeze cell \"ghost\""), "{reason}");

        let elsewhere = Rows::default().add(RUN, at("base", "elsewhere", 0, 1), pass());
        let reason = refusal(build(&run, &elsewhere.trials, &[], &definition, &[]));
        assert!(reason.contains("did not freeze case \"elsewhere\""), "{reason}");
        assert_eq!(
            trial_id(RUN, at("base", "elsewhere", 0, 1)),
            elsewhere.trials[0].identity.trial_id
        );
    }
```

Add `build` to the `pub use build::{…}` list in `crates/gents/src/eval/report/mod.rs`, and in `crates/gents/src/eval/report/evidence.rs` change `fn counted_slots<'a>(` to `pub(crate) fn counted_slots<'a>(`.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents --lib eval::report::build > "$LOG/t1b.log" 2>&1; grep -E "error(\[|:)|test result" "$LOG/t1b.log" | head -20`
Expected: compile errors `cannot find function `build``.

- [ ] **Step 3: Write the implementation**

In `crates/gents/src/eval/report/build.rs`, replace the `use` block from Task 1 with:

```rust
use std::collections::BTreeMap;

use anyhow::Result;
use serde::Serialize;

use crate::document_config::{EvalDefinition, EvalReducer, EvalSplit, EvalTier};
use crate::eval::report::evidence::{cell_usage, counted_slots, CellUsage, RunRows};
use crate::eval::report::refused;
use crate::eval::runner::freeze::definition_ref;
use crate::eval::{
    case_means_bp, case_trial_score, classify, exposure, headline_bp, CaseTrialScore, CellSpec,
    DefinitionRef, EvidenceClass, Invalidation, RunHeader, RunRecord, StageCompletion,
    SubjectRef, TrialRecord, TrialScore, TrialUsage, VerdictRecord,
};
```

and insert, after the `impl SlotCounts { … }` block and above the test module:

```rust
/// Build `run`'s report from its rows.
///
/// `peers` are the owner's run headers, this run's included; they feed only
/// `exposure`. Refused, never guessed at: rows of another run, a trial naming
/// a cell or case the run did not freeze, and a definition that no longer
/// digests to what the run froze (its stage ids could mean something else).
pub fn build(
    run: &RunRecord,
    trials: &[TrialRecord],
    verdicts: &[VerdictRecord],
    definition: &EvalDefinition,
    peers: &[RunHeader],
) -> Result<EvalReport> {
    let run_id = run.run_id.as_str();
    let origin = &run.origin;
    if definition_ref(definition)?.digest != origin.definition.digest {
        return Err(refused(format!(
            "eval definition {:?} no longer digests to what run {run_id} froze; its report cannot be read against the installed definition",
            origin.definition.definition_id
        )));
    }
    for trial in trials {
        let identity = &trial.identity;
        if identity.run_id != run_id {
            return Err(refused(format!(
                "trial {} belongs to run {}, not {run_id}",
                identity.trial_id, identity.run_id
            )));
        }
        if !origin
            .cells
            .iter()
            .any(|cell| cell.cell_id == identity.cell_id)
        {
            return Err(refused(format!(
                "run {run_id} did not freeze cell {:?} (trial {})",
                identity.cell_id, identity.trial_id
            )));
        }
        if !origin.case_ids.contains(&identity.case_id) {
            return Err(refused(format!(
                "run {run_id} did not freeze case {:?} (trial {})",
                identity.case_id, identity.trial_id
            )));
        }
    }
    if let Some(verdict) = verdicts.iter().find(|verdict| verdict.run_id != run_id) {
        return Err(refused(format!(
            "verdict {} belongs to run {}, not {run_id}",
            verdict.verdict_id, verdict.run_id
        )));
    }

    let rows = RunRows {
        run_id: run_id.to_owned(),
        case_ids: origin.case_ids.clone(),
        invalidated: run.invalidated.is_some(),
        trials: trials.to_vec(),
        verdicts: verdicts.to_vec(),
    };
    let cells = origin
        .cells
        .iter()
        .map(|cell| cell_report(definition, &rows, cell, origin.trials_per_case))
        .collect();
    Ok(EvalReport {
        report_version: REPORT_VERSION,
        run: RunSummary {
            run_id: run_id.to_owned(),
            definition: origin.definition.clone(),
            split: origin.split,
            purpose: origin.purpose.clone(),
            created_at: run.created_at.clone(),
            invalidated: run.invalidated.clone(),
            trials_per_case: origin.trials_per_case,
            seed_base: origin.seed_base,
            concurrency: origin.concurrency,
            max_infra_retries: origin.max_infra_retries,
            breaker_threshold: origin.breaker_threshold,
            source_commit: origin.source_commit.clone(),
            source_dirty: origin.source_dirty,
        },
        cells,
        exposure: exposure(
            peers,
            &origin.definition.definition_id,
            origin.definition.comparability_version,
            origin.split,
        ),
        definition_changed: false,
    })
}

/// A slot's counted attempt: its score, whether every weighted acceptance
/// verdict passed, and the row itself.
struct Counted<'a> {
    score: CaseTrialScore,
    all_pass: bool,
    record: &'a TrialRecord,
}

fn cell_report(
    definition: &EvalDefinition,
    rows: &RunRows,
    cell: &CellSpec,
    trials_per_case: u32,
) -> CellReport {
    let counted: BTreeMap<(&str, u32), Counted<'_>> =
        counted_slots(definition, rows, &cell.cell_id)
            .into_iter()
            .map(|(record, case, views)| {
                let all_pass = views
                    .iter()
                    .filter(|view| view.tier == EvalTier::Acceptance && view.weight > 0)
                    .all(|view| classify(view.kind, view.provider_reason) == EvidenceClass::Pass);
                (
                    (record.identity.case_id.as_str(), record.identity.trial_index),
                    Counted {
                        score: case_trial_score(case.reducer, &views),
                        all_pass,
                        record,
                    },
                )
            })
            .collect();
    let in_cell: Vec<&TrialRecord> = rows
        .trials
        .iter()
        .filter(|trial| trial.identity.cell_id == cell.cell_id)
        .collect();

    let mut report = CellReport {
        cell_id: cell.cell_id.clone(),
        label: cell.label.clone(),
        subject: cell.subject.clone(),
        slots: Vec::new(),
        cases: Vec::new(),
        headline_bp: None,
        usage: TrialUsage::default(),
        cell_usage: cell_usage(rows, &cell.cell_id),
        attempts: in_cell.len() as u32,
        counts: SlotCounts::default(),
    };
    let mut scores = Vec::new();
    for case_id in &rows.case_ids {
        let reducer = definition
            .cases
            .iter()
            .find(|case| &case.case_id == case_id)
            .map(|case| case.reducer)
            .unwrap_or_default();
        let mut case_counts = SlotCounts::default();
        let mut case_scores = Vec::new();
        for trial_index in 0..trials_per_case {
            let attempts: Vec<&TrialRecord> = in_cell
                .iter()
                .copied()
                .filter(|trial| {
                    &trial.identity.case_id == case_id && trial.identity.trial_index == trial_index
                })
                .collect();
            let latest = attempts
                .iter()
                .copied()
                .max_by_key(|trial| trial.identity.attempt);
            let counted_here = counted.get(&(case_id.as_str(), trial_index));
            let class = slot_class(latest, counted_here);
            case_counts.add(class);
            report.counts.add(class);
            if let Some(counted_here) = counted_here {
                case_scores.push(TrialScore {
                    case_id: case_id.clone(),
                    trial_index,
                    score: counted_here.score,
                });
                if let Some(completion) = &counted_here.record.completion {
                    let usage = &completion.usage;
                    report.usage = TrialUsage {
                        input_tokens: sum(report.usage.input_tokens, usage.input_tokens),
                        output_tokens: sum(report.usage.output_tokens, usage.output_tokens),
                    };
                }
            }
            let score = counted_here.map(|counted_here| counted_here.score);
            report.slots.push(SlotReport {
                case_id: case_id.clone(),
                trial_index,
                class,
                attempts: attempts.len() as u32,
                score_bp: match score {
                    Some(CaseTrialScore::Scored(bp)) => Some(bp),
                    _ => None,
                },
                counted: SlotScore::of(score),
                latest: latest.map(attempt_summary),
            });
        }
        report.cases.push(CaseReport {
            case_id: case_id.clone(),
            reducer,
            mean_bp: case_means_bp(&case_scores).get(case_id).copied(),
            counts: case_counts,
        });
        scores.extend(case_scores);
    }
    report.headline_bp = headline_bp(&scores);
    report
}

/// Spec 4a §1: no row is Planned, an open latest row is Abandoned, and
/// otherwise the case-trial class of the latest completed attempt.
fn slot_class(latest: Option<&TrialRecord>, counted: Option<&Counted<'_>>) -> SlotClass {
    let Some(latest) = latest else {
        return SlotClass::Planned;
    };
    if latest.completion.is_none() {
        return SlotClass::Abandoned;
    }
    match counted.map(|counted| (counted.score, counted.all_pass)) {
        Some((CaseTrialScore::NotEvidence, _)) => SlotClass::NotEvidence,
        Some((CaseTrialScore::Scored(_), true)) => SlotClass::Pass,
        Some((CaseTrialScore::Scored(_), false)) => SlotClass::Fail,
        // `build` has checked every case against the definition, so a
        // completed slot always has a counted attempt; without one it would
        // say nothing either way.
        Some((CaseTrialScore::Unknown, _)) | None => SlotClass::Unknown,
    }
}

fn attempt_summary(record: &TrialRecord) -> AttemptSummary {
    let identity = &record.identity;
    let completion = record.completion.as_ref();
    AttemptSummary {
        trial_id: identity.trial_id.clone(),
        attempt: identity.attempt,
        trial_agent_did: identity.trial_agent_did.clone(),
        session_id: identity.session_id.clone(),
        home_hint: identity.home_hint.clone(),
        evidence_digest: completion.and_then(|completion| completion.evidence_digest.clone()),
        stages: completion
            .map(|completion| completion.stages.clone())
            .unwrap_or_default(),
        usage: completion
            .map(|completion| completion.usage.clone())
            .unwrap_or_default(),
    }
}

/// Two token counts, absent only when both are.
fn sum(total: Option<u64>, one: Option<u64>) -> Option<u64> {
    match (total, one) {
        (None, None) => None,
        (total, one) => Some(total.unwrap_or(0) + one.unwrap_or(0)),
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents --lib eval::report > "$LOG/t1b.log" 2>&1; grep -E "^test |test result|panicked" "$LOG/t1b.log" | tail -30`
Expected: the seven classification tests and Task 1's two pass; M6b's `evidence::tests` still pass.

- [ ] **Step 5: Format and commit**

```bash
cargo fmt --all && cargo fmt --all --check
git add crates/gents/src/eval/report/
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" commit -m "feat(eval): report::build classifies every slot from the documents (#1515)"
```

### Task 2: `report::compare`

**Files:**
- Create: `crates/gents/src/eval/report/compare.rs`
- Modify: `crates/gents/src/eval/report/mod.rs` (add `pub mod compare;` after `pub mod build;` and a `pub use compare::{…}` line)
- Modify: `crates/gents/src/optimization/evidence.rs` (ruling F5: `token_totals` delegates to a new pure `totals`; a test)
- Modify: `crates/gents/src/optimization/mod.rs` (`totals` in the `pub use evidence::{…}` list)

**Interfaces:**
- Consumes:
  ```rust
  // Task 1 / 1b
  pub struct EvalReport { pub run: RunSummary, pub cells: Vec<CellReport>, .. }
  pub struct CellReport { pub cell_id, pub slots: Vec<SlotReport>, pub cases: Vec<CaseReport>, pub cell_usage: CellUsage, .. }
  impl SlotScore { pub fn trial_score(self) -> Option<CaseTrialScore> }
  pub(crate) fn refused(reason: impl Into<String>) -> anyhow::Error;
  // crates/gents/src/eval/scoring.rs (frozen)
  pub fn pair_trials(baseline: &[TrialScore], candidate: &[TrialScore]) -> PairedEvidence;
  pub struct PairedEvidence { pub pairs: Vec<Pair>, pub keys: usize, pub dropped_baseline: usize, pub dropped_candidate: usize }
  // crates/gents/src/optimization/policy.rs (M6a)
  pub fn evidence_from_pairs(paired: &PairedEvidence, expected_cases: &[String], tokens: Option<TokenTotals>) -> Evidence;
  pub fn decide(mode: Mode, policy: &PolicyV2, evidence: &Evidence, seed: u64) -> DecisionReport;
  pub fn permutation_p_ppm(diffs: &[i128], samples: u32, seed: u64) -> u64;   // test only
  pub struct DecisionReport { pub decision: Decision, pub policy_version: String, pub improved: u32, pub tied: u32,
      pub worsened: u32, pub mean_diff_bp: Option<i64>, pub p_ppm: Option<u64>, pub alpha_effective_ppm: u64, pub cost_skipped: bool }
  pub struct CaseEvidence { pub case_id: String, pub pairs: u64, pub sum_baseline_bp: u64, pub sum_candidate_bp: u64 }
  pub struct TokenTotals { pub baseline_tokens: u64, pub baseline_trials: u64, pub candidate_tokens: u64, pub candidate_trials: u64 }
  impl PolicyV2 { pub fn uncalibrated() -> Self }   // PolicyV2: PartialEq; fields max_missing_usage_bp: u64, monte_carlo_samples: u32
  // crates/gents/src/optimization/evidence.rs (M6b)
  pub fn decision_seed(run_ids: &[String]) -> u64;
  pub fn token_totals(runs: &[RunRows], max_missing_usage_bp: u64) -> Option<TokenTotals>;   // refactored here onto `totals`
  pub struct CellUsage { pub tokens: u64, pub trials: u64, pub missing: u64 }             // eval::report::evidence
  ```
- Produces:
  ```rust
  // crates/gents/src/optimization/evidence.rs
  /// The cost gate's totals for two cells' usage: unknown usage is not free; past
  /// the tolerated missing share, or with a cell that reported nothing, `None`.
  pub fn totals(baseline: CellUsage, candidate: CellUsage, max_missing_usage_bp: u64) -> Option<TokenTotals>;
  // crates/gents/src/eval/report/compare.rs
  pub struct Comparison {
      pub comparability: DefinitionRef,
      pub baseline_run: String, pub baseline_cell: String, pub candidate_run: String, pub candidate_cell: String,
      pub cases: Vec<CaseComparison>,
      pub improved: u32, pub tied: u32, pub worsened: u32, pub mean_diff_bp: Option<i64>,
      pub pairs: usize, pub dropped_baseline: usize, pub dropped_candidate: usize, pub imputed: usize,
      pub p_ppm: Option<u64>,
      /* #[serde(skip)] private: paired, case_ids, usage: [CellUsage; 2], seed */
  }
  pub struct CaseComparison { pub case_id: String, pub pairs: u64, pub baseline_mean_bp: Option<u32>,
      pub candidate_mean_bp: Option<u32>, pub diff_bp: Option<i64> }
  pub fn compare(baseline: &EvalReport, candidate: &EvalReport, baseline_cell: &str, candidate_cell: &str) -> Result<Comparison>;
  impl Comparison {
      pub fn evidence(&self, policy: &PolicyV2) -> Evidence;   // pairs, the baseline's case list, `totals` of the two cells
      pub fn seed(&self) -> u64;                                // decision_seed over the DISTINCT run ids (ruling F4)
      pub(crate) fn restate(&mut self, policy: &PolicyV2) -> DecisionReport;
  }
  ```
  Both structs derive `Clone, Debug, PartialEq, Eq, Serialize`. Task 2b adds `policy: Option<PolicyOutcome>` and `with_policy`.

- [ ] **Step 1: Write the failing tests**

Create `crates/gents/src/eval/report/compare.rs` holding only this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::document_config::EvalDefinition;
    use crate::eval::report::build::build;
    use crate::eval::report::evidence::{cell_trial_scores, RunRows};
    use crate::eval::report::fixtures::{at, definition, fail, pass, record, Outcome, Rows, RUN};
    use crate::eval::report::report_refused;
    use crate::eval::OutcomeKind;
    use crate::optimization::policy::permutation_p_ppm;

    fn report(definition: &EvalDefinition, rows: &Rows, cells: &[&str], trials: u32) -> EvalReport {
        let run = record(RUN, definition, cells, trials);
        build(&run, &rows.trials, &rows.verdicts, definition, &[]).unwrap()
    }

    fn run_rows(definition: &EvalDefinition, rows: &Rows) -> RunRows {
        let mut case_ids: Vec<String> = definition
            .cases
            .iter()
            .map(|case| case.case_id.clone())
            .collect();
        case_ids.sort();
        RunRows {
            run_id: RUN.into(),
            case_ids,
            invalidated: false,
            trials: rows.trials.clone(),
            verdicts: rows.verdicts.clone(),
        }
    }

    fn refusal(result: Result<Comparison>) -> String {
        let error = result.expect_err("expected a refusal");
        report_refused(&error)
            .unwrap_or_else(|| panic!("expected a ReportRefused, got {error:#}"))
            .0
            .clone()
    }

    /// a improves, b ties, c worsens, d's baseline is unknown (imputed a
    /// pass, so it ties), and e's baseline is not evidence (no pair).
    fn five_cases() -> (EvalDefinition, Rows) {
        let definition = definition(&["a", "b", "c", "d", "e"]);
        let rows = Rows::default()
            .add(RUN, at("baseline", "a", 0, 1), fail())
            .add(RUN, at("candidate", "a", 0, 1), pass())
            .add(RUN, at("baseline", "b", 0, 1), pass())
            .add(RUN, at("candidate", "b", 0, 1), pass())
            .add(RUN, at("baseline", "c", 0, 1), pass())
            .add(RUN, at("candidate", "c", 0, 1), fail())
            .add(
                RUN,
                at("baseline", "d", 0, 1),
                Outcome::Verdict(OutcomeKind::Grader, None),
            )
            .add(RUN, at("candidate", "d", 0, 1), pass())
            .add(
                RUN,
                at("baseline", "e", 0, 1),
                Outcome::Verdict(OutcomeKind::Infrastructure, None),
            )
            .add(RUN, at("candidate", "e", 0, 1), pass());
        (definition, rows)
    }

    #[test]
    fn cases_are_counted_by_the_sign_of_their_mean_difference() {
        let (definition, rows) = five_cases();
        let report = report(&definition, &rows, &["baseline", "candidate"], 1);
        let comparison = compare(&report, &report, "baseline", "candidate").unwrap();
        assert_eq!(
            (comparison.improved, comparison.tied, comparison.worsened),
            (1, 2, 1)
        );
        assert_eq!(comparison.mean_diff_bp, Some(0));
        assert_eq!(
            (
                comparison.pairs,
                comparison.dropped_baseline,
                comparison.dropped_candidate,
                comparison.imputed
            ),
            (4, 1, 0, 1)
        );
        let case = |id: &str| {
            comparison
                .cases
                .iter()
                .find(|case| case.case_id == id)
                .unwrap()
                .clone()
        };
        assert_eq!((case("e").pairs, case("e").diff_bp), (0, None));
        assert_eq!(
            (
                case("d").baseline_mean_bp,
                case("d").candidate_mean_bp,
                case("d").diff_bp
            ),
            (Some(10_000), Some(10_000), Some(0)),
            "an unknown baseline is imputed a pass"
        );
        assert_eq!(case("a").diff_bp, Some(10_000));
        assert!(comparison.policy.is_none());
    }

    #[test]
    fn pairs_match_pair_trials_and_p_matches_the_permutation_test() {
        let (definition, rows) = five_cases();
        let report = report(&definition, &rows, &["baseline", "candidate"], 1);
        let comparison = compare(&report, &report, "baseline", "candidate").unwrap();

        let documents = run_rows(&definition, &rows);
        let paired = pair_trials(
            &cell_trial_scores(&definition, &documents, "baseline"),
            &cell_trial_scores(&definition, &documents, "candidate"),
        );
        assert_eq!(
            (
                comparison.pairs,
                comparison.dropped_baseline,
                comparison.dropped_candidate
            ),
            (
                paired.pairs.len(),
                paired.dropped_baseline,
                paired.dropped_candidate
            )
        );
        // One pair per case, so the common scale is 1 and the per-case
        // differences in case order a, b, c, d are the raw ones. One run:
        // its id once, as `optimization show` seeds a one-run decision (F4).
        let seed = decision_seed(&[RUN.to_owned()]);
        assert_eq!(comparison.seed(), seed);
        assert_eq!(
            comparison.p_ppm,
            Some(permutation_p_ppm(
                &[10_000, 0, -10_000, 0],
                PolicyV2::uncalibrated().monte_carlo_samples,
                seed
            ))
        );
    }

    #[test]
    fn compare_refuses_runs_that_are_not_comparable_and_cells_that_do_not_exist() {
        let (definition, rows) = five_cases();
        let report = report(&definition, &rows, &["baseline", "candidate"], 1);

        let mut other_version = report.clone();
        other_version.run.definition.comparability_version = 2;
        let reason = refusal(compare(&report, &other_version, "baseline", "candidate"));
        assert!(reason.contains("one comparability version"), "{reason}");

        let mut other_digest = report.clone();
        other_digest.run.definition.digest = "sha256:elsewhere".into();
        let reason = refusal(compare(&report, &other_digest, "baseline", "candidate"));
        assert!(reason.contains("different digests"), "{reason}");

        let reason = refusal(compare(&report, &report, "ghost", "candidate"));
        assert_eq!(reason, "run run has no cell \"ghost\"");
        let reason = refusal(compare(&report, &report, "baseline", "ghost"));
        assert_eq!(reason, "run run has no cell \"ghost\"");
    }
}
```

Append inside `mod tests` in `crates/gents/src/optimization/evidence.rs`:

```rust
    #[test]
    fn totals_skip_the_cost_gate_past_the_missing_share_and_for_a_silent_cell() {
        let usage = |tokens, trials, missing| CellUsage {
            tokens,
            trials,
            missing,
        };
        assert_eq!(
            totals(usage(100, 10, 0), usage(120, 10, 0), 2000),
            Some(TokenTotals {
                baseline_tokens: 100,
                baseline_trials: 10,
                candidate_tokens: 120,
                candidate_trials: 10,
            })
        );
        assert_eq!(
            totals(usage(100, 10, 3), usage(120, 10, 2), 2000),
            None,
            "5 of 20 trials missing is past 2000 bp"
        );
        assert_eq!(
            totals(usage(0, 2, 2), usage(120, 10, 0), 5000),
            None,
            "a cell whose every trial is unmetered has no mean"
        );
        assert_eq!(totals(usage(0, 0, 0), usage(0, 0, 0), 2000), None);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents --lib eval::report::compare > "$LOG/t2.log" 2>&1; grep -E "error(\[|:)|test result" "$LOG/t2.log" | head -20` (after adding `pub mod compare;` to `report/mod.rs`)
Expected: compile errors: `compare`, `Comparison` and `totals` not found.

- [ ] **Step 3: Write the implementation**

In `crates/gents/src/optimization/evidence.rs`, replace `token_totals` and `reported` with (the doc comment on `token_totals` stays as it is):

```rust
pub fn token_totals(runs: &[RunRows], max_missing_usage_bp: u64) -> Option<TokenTotals> {
    let (mut baseline, mut candidate) = (CellUsage::default(), CellUsage::default());
    for rows in runs {
        add(&mut baseline, cell_usage(rows, BASELINE_CELL));
        add(&mut candidate, cell_usage(rows, CANDIDATE_CELL));
    }
    totals(baseline, candidate, max_missing_usage_bp)
}

fn add(total: &mut CellUsage, one: CellUsage) {
    total.tokens += one.tokens;
    total.trials += one.trials;
    total.missing += one.missing;
}

/// The cost gate's token totals for two cells, by the rule [`token_totals`]
/// documents. Pure: `eval::report::compare` calls it too, so an operator's
/// comparison and the optimizer skip the cost gate on exactly the same data
/// (ruling F5).
pub fn totals(
    baseline: CellUsage,
    candidate: CellUsage,
    max_missing_usage_bp: u64,
) -> Option<TokenTotals> {
    let counted = u128::from(baseline.trials + candidate.trials);
    let missing = u128::from(baseline.missing + candidate.missing);
    if counted == 0 || missing * 10_000 > u128::from(max_missing_usage_bp) * counted {
        tracing::info!(
            counted = counted as u64,
            missing = missing as u64,
            "optimization cost gate skipped: too many trials reported no usage"
        );
        return None;
    }
    let (baseline_reported, candidate_reported) = (reported(&baseline), reported(&candidate));
    if baseline_reported == 0 || candidate_reported == 0 {
        tracing::info!(
            baseline_reported,
            candidate_reported,
            "optimization cost gate skipped: a cell has no trial that reported usage"
        );
        return None;
    }
    Some(TokenTotals {
        baseline_tokens: baseline.tokens,
        baseline_trials: baseline_reported,
        candidate_tokens: candidate.tokens,
        candidate_trials: candidate_reported,
    })
}

/// The trials of one cell that reported usage: the mean's denominator.
fn reported(usage: &CellUsage) -> u64 {
    usage.trials.saturating_sub(usage.missing)
}
```

and add `totals` to the `pub use evidence::{…}` list in `crates/gents/src/optimization/mod.rs`.

Insert above the test module in `crates/gents/src/eval/report/compare.rs`:

```rust
//! `compare`: two cells' paired statistics (spec 4a §1).
//!
//! The pairs are the optimizer's: each report's counted slot scores joined by
//! `pair_trials` on `(case_id, trial_index)`. The statistics are
//! `optimization::policy::decide`'s own numbers on those pairs, seeded as
//! `optimization show` seeds a decision over the same runs, so the p-value an
//! operator reads is the one the optimizer would compute. Without a policy
//! they are computed under the placeholder defaults, whose only influence on
//! them is the Monte Carlo sample count above twenty cases.

use std::collections::BTreeMap;

use anyhow::Result;
use serde::Serialize;

use crate::eval::report::build::{CellReport, EvalReport};
use crate::eval::report::evidence::CellUsage;
use crate::eval::report::refused;
use crate::eval::{pair_trials, CaseTrialScore, DefinitionRef, PairedEvidence, TrialScore};
use crate::optimization::evidence::{decision_seed, totals};
use crate::optimization::policy::{
    decide, evidence_from_pairs, CaseEvidence, DecisionReport, Evidence, Mode, PolicyV2,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CaseComparison {
    pub case_id: String,
    pub pairs: u64,
    pub baseline_mean_bp: Option<u32>,
    pub candidate_mean_bp: Option<u32>,
    /// Mean paired difference, candidate minus baseline, truncated.
    pub diff_bp: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Comparison {
    pub comparability: DefinitionRef,
    pub baseline_run: String,
    pub baseline_cell: String,
    pub candidate_run: String,
    pub candidate_cell: String,
    pub cases: Vec<CaseComparison>,
    pub improved: u32,
    pub tied: u32,
    pub worsened: u32,
    pub mean_diff_bp: Option<i64>,
    pub pairs: usize,
    pub dropped_baseline: usize,
    pub dropped_candidate: usize,
    /// Pairs in which an unknown side was imputed worst-case.
    pub imputed: usize,
    pub p_ppm: Option<u64>,
    #[serde(skip)]
    paired: PairedEvidence,
    #[serde(skip)]
    case_ids: Vec<String>,
    #[serde(skip)]
    usage: [CellUsage; 2],
    #[serde(skip)]
    seed: u64,
}

/// Compare `baseline_cell` of one report with `candidate_cell` of another,
/// or of the same one. Refused across comparability versions or definition
/// digests, and for a cell a report does not have.
pub fn compare(
    baseline: &EvalReport,
    candidate: &EvalReport,
    baseline_cell: &str,
    candidate_cell: &str,
) -> Result<Comparison> {
    let (left, right) = (&baseline.run, &candidate.run);
    if left.definition.definition_id != right.definition.definition_id
        || left.definition.comparability_version != right.definition.comparability_version
    {
        return Err(refused(format!(
            "run {} froze {} comparability version {} and run {} froze {} comparability version {}; runs compare only within one comparability version",
            left.run_id,
            left.definition.definition_id,
            left.definition.comparability_version,
            right.run_id,
            right.definition.definition_id,
            right.definition.comparability_version,
        )));
    }
    if left.definition.digest != right.definition.digest {
        return Err(refused(format!(
            "runs {} and {} froze different digests of eval definition {:?}; they are not comparable",
            left.run_id, right.run_id, left.definition.definition_id
        )));
    }
    let base = cell(baseline, baseline_cell)?;
    let cand = cell(candidate, candidate_cell)?;
    let (base_scores, cand_scores) = (trial_scores(base), trial_scores(cand));
    let paired = pair_trials(&base_scores, &cand_scores);
    // Ruling F4: the distinct run ids, as `optimization show` seeds a
    // decision over the runs it read.
    let mut seed_runs = vec![left.run_id.clone()];
    if right.run_id != left.run_id {
        seed_runs.push(right.run_id.clone());
    }
    let mut comparison = Comparison {
        comparability: left.definition.clone(),
        baseline_run: left.run_id.clone(),
        baseline_cell: baseline_cell.to_owned(),
        candidate_run: right.run_id.clone(),
        candidate_cell: candidate_cell.to_owned(),
        cases: Vec::new(),
        improved: 0,
        tied: 0,
        worsened: 0,
        mean_diff_bp: None,
        pairs: paired.pairs.len(),
        dropped_baseline: paired.dropped_baseline,
        dropped_candidate: paired.dropped_candidate,
        imputed: imputed(&base_scores, &cand_scores),
        p_ppm: None,
        paired,
        case_ids: base.cases.iter().map(|case| case.case_id.clone()).collect(),
        usage: [base.cell_usage, cand.cell_usage],
        seed: decision_seed(&seed_runs),
    };
    let defaults = PolicyV2::uncalibrated();
    comparison.cases = comparison
        .evidence(&defaults)
        .cases
        .iter()
        .map(case_comparison)
        .collect();
    comparison.restate(&defaults);
    Ok(comparison)
}

impl Comparison {
    /// The evidence `decide` reads under `policy`: the pairs, the baseline
    /// run's case list, and the two cells' token totals by
    /// `optimization::evidence::totals`.
    pub fn evidence(&self, policy: &PolicyV2) -> Evidence {
        evidence_from_pairs(
            &self.paired,
            &self.case_ids,
            totals(self.usage[0], self.usage[1], policy.max_missing_usage_bp),
        )
    }

    /// The Monte Carlo seed: `decision_seed` over the distinct run ids.
    pub fn seed(&self) -> u64 {
        self.seed
    }

    /// Decide under `policy` and restate the statistics from that decision.
    pub(crate) fn restate(&mut self, policy: &PolicyV2) -> DecisionReport {
        let report = decide(Mode::Improve, policy, &self.evidence(policy), self.seed);
        self.improved = report.improved;
        self.tied = report.tied;
        self.worsened = report.worsened;
        self.mean_diff_bp = report.mean_diff_bp;
        self.p_ppm = report.p_ppm;
        report
    }
}

fn cell<'a>(report: &'a EvalReport, cell_id: &str) -> Result<&'a CellReport> {
    report
        .cells
        .iter()
        .find(|cell| cell.cell_id == cell_id)
        .ok_or_else(|| refused(format!("run {} has no cell {cell_id:?}", report.run.run_id)))
}

fn trial_scores(cell: &CellReport) -> Vec<TrialScore> {
    cell.slots
        .iter()
        .filter_map(|slot| {
            slot.counted.trial_score().map(|score| TrialScore {
                case_id: slot.case_id.clone(),
                trial_index: slot.trial_index,
                score,
            })
        })
        .collect()
}

/// Keys both cells hold as evidence where at least one side is unknown:
/// `pair_trials` imputes those worst-case.
fn imputed(baseline: &[TrialScore], candidate: &[TrialScore]) -> usize {
    let index = |scores: &[TrialScore]| {
        scores
            .iter()
            .map(|score| ((score.case_id.clone(), score.trial_index), score.score))
            .collect::<BTreeMap<_, _>>()
    };
    let (left, right) = (index(baseline), index(candidate));
    left.iter()
        .filter(|&(key, base)| {
            right.get(key).is_some_and(|cand| {
                *base != CaseTrialScore::NotEvidence
                    && *cand != CaseTrialScore::NotEvidence
                    && (*base == CaseTrialScore::Unknown || *cand == CaseTrialScore::Unknown)
            })
        })
        .count()
}

fn case_comparison(case: &CaseEvidence) -> CaseComparison {
    let paired = case.pairs > 0;
    let mean = |sum: u64| paired.then(|| (sum / case.pairs) as u32);
    CaseComparison {
        case_id: case.case_id.clone(),
        pairs: case.pairs,
        baseline_mean_bp: mean(case.sum_baseline_bp),
        candidate_mean_bp: mean(case.sum_candidate_bp),
        diff_bp: paired.then(|| {
            (case.sum_candidate_bp as i64 - case.sum_baseline_bp as i64) / case.pairs as i64
        }),
    }
}
```

In `crates/gents/src/eval/report/mod.rs`, add `pub mod compare;` after `pub mod build;`, and after the `pub use build::{…};` block:

```rust
pub use compare::{compare, CaseComparison, Comparison};
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents --lib eval::report > "$LOG/t2.log" 2>&1; grep -E "^test |test result|panicked" "$LOG/t2.log" | tail -30` then `CARGO_BUILD_JOBS=4 cargo test -p gents --lib optimization > "$LOG/t2-opt.log" 2>&1; grep -E "test result|FAILED|panicked" "$LOG/t2-opt.log"`
Expected: three `compare::tests` pass; the `totals` test and every existing optimization test (the `token_totals` tests and the matrix) still pass.

- [ ] **Step 5: Format and commit**

```bash
cargo fmt --all && cargo fmt --all --check
git add crates/gents/src/eval/report/ crates/gents/src/optimization/evidence.rs crates/gents/src/optimization/mod.rs
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" commit -m "feat(eval): report::compare reads the optimizer's pairs, totals and seed (#1515)"
```

### Task 2a: Freeze materializes the definition (ruling U2)

**Files:**
- Modify: `crates/gents/src/eval/runner/freeze.rs` (`DEFINITION_FILE`, `write_frozen_definition`, `read_frozen_definition`; `freeze` writes the file after `write_sidecar`; `thaw` reads it; tests)
- Modify: `crates/gents/src/eval/runner/mod.rs` (re-export `read_frozen_definition`, `DEFINITION_FILE`)

**Interfaces:**
- Consumes:
  ```rust
  // crates/gents/src/eval/runner/freeze.rs
  pub async fn freeze(access: &ConfigAccess, request: &RunRequest, isolation: Isolation) -> Result<FrozenRun>;
  //   loads `definition` via load_definition, computes `run_dir`, then materializes packs and calls write_sidecar(&run_dir, &sidecar)?
  pub(crate) async fn thaw(access, owner, run_id, runs_dir, isolation) -> Result<FrozenRun>;
  //   today reads the live definition with read_document and refuses "eval definition {id:?} changed since run {run_id} froze it"
  pub(crate) fn definition_ref(definition: &EvalDefinition) -> Result<DefinitionRef>;
  pub(crate) fn refused(reason: impl Into<String>) -> anyhow::Error;
  // tests: pub(crate) struct Launching; fn request(&self, run_id, pack) -> RunRequest (private to freeze tests); pub(crate) fn pack(..)
  ```
- Produces:
  ```rust
  pub const DEFINITION_FILE: &str = "definition.json";
  /// The definition `run_dir` froze, verified against the run's recorded digest; `Ok(None)` for a run frozen before this file existed.
  pub fn read_frozen_definition(run_dir: &Path, frozen: &DefinitionRef) -> Result<Option<EvalDefinition>>;   // FreezeRefused on a digest mismatch
  ```
  Behavior: `freeze` writes `<run dir>/definition.json`, the `EvalDefinition` exactly as it loaded it, whose digest is `origin.definition.digest`. `thaw` (so `resume`) uses the file when present after verifying its digest against the origin, and falls back to today's live-definition check when it is absent. The optimizer is unaffected: `run_job` refuses a drifted definition itself before it resumes a run (`driver.rs`, reason `definition_changed`).

- [ ] **Step 1: Write the failing tests**

Append inside `pub(crate) mod tests` in `crates/gents/src/eval/runner/freeze.rs`:

```rust
    #[tokio::test]
    async fn freeze_writes_the_definition_it_froze_and_resume_reads_it_back() {
        let launching = Launching::new().await;
        let pack = launching.pack("pack", "Off");
        let frozen = freeze(&launching.access, &launching.request("run-def", &pack), Isolation::Embedded)
            .await
            .unwrap();
        let path = frozen.run_dir.join(DEFINITION_FILE);
        assert!(path.is_file());
        let read = read_frozen_definition(&frozen.run_dir, &frozen.record.origin.definition)
            .unwrap()
            .expect("the file is there");
        assert_eq!(read, frozen.definition);

        // The live definition changes; the run keeps the one it froze.
        let mut edited = serde_json::to_value(&frozen.definition).unwrap();
        edited["title"] = json!("edited after the freeze");
        launching
            .install(vec![(Collection::EvalDefinition, edited)])
            .await;
        let thawed = thaw(&launching.access, OWNER, "run-def", &launching.runs_dir(), Isolation::Embedded)
            .await
            .unwrap();
        assert_eq!(thawed.definition, frozen.definition);

        // A file that no longer digests to the origin is refused.
        let mut tampered = serde_json::to_value(&frozen.definition).unwrap();
        tampered["comparability_version"] = json!(99);
        std::fs::write(&path, serde_json::to_vec(&tampered).unwrap()).unwrap();
        let error = read_frozen_definition(&frozen.run_dir, &frozen.record.origin.definition)
            .unwrap_err();
        assert!(refusal(&error).contains("no longer digests"), "{error:#}");
        let error = thaw(&launching.access, OWNER, "run-def", &launching.runs_dir(), Isolation::Embedded)
            .await
            .unwrap_err();
        assert!(refusal(&error).contains("no longer digests"), "{error:#}");
    }

    #[tokio::test]
    async fn a_run_frozen_without_the_file_falls_back_to_the_live_definition() {
        let launching = Launching::new().await;
        let pack = launching.pack("pack", "Off");
        let frozen = freeze(&launching.access, &launching.request("run-old", &pack), Isolation::Embedded)
            .await
            .unwrap();
        std::fs::remove_file(frozen.run_dir.join(DEFINITION_FILE)).unwrap();
        assert_eq!(
            read_frozen_definition(&frozen.run_dir, &frozen.record.origin.definition).unwrap(),
            None
        );
        let thawed = thaw(&launching.access, OWNER, "run-old", &launching.runs_dir(), Isolation::Embedded)
            .await
            .unwrap();
        assert_eq!(thawed.definition, frozen.definition);
    }
```

(`Launching::request` is the private helper in the same test module; `refusal`, `json`, `Collection` and `OWNER` are already in scope there.)

- [ ] **Step 2: Run the tests to verify they fail**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents --lib eval::runner::freeze > "$LOG/t2a.log" 2>&1; grep -E "error(\[|:)|test result" "$LOG/t2a.log" | head`
Expected: compile errors: `DEFINITION_FILE`, `read_frozen_definition` not found.

- [ ] **Step 3: Write the implementation**

In `crates/gents/src/eval/runner/freeze.rs`, after `fn write_sidecar(…) { … }` add:

```rust
/// `<run dir>/definition.json`: the definition exactly as freezing loaded it
/// (ruling U2). A report and a resume read this copy, so a later edit of the
/// installed definition cannot change what a finished run means.
pub const DEFINITION_FILE: &str = "definition.json";

fn write_frozen_definition(run_dir: &Path, definition: &EvalDefinition) -> Result<()> {
    std::fs::create_dir_all(run_dir).with_context(|| format!("creating {}", run_dir.display()))?;
    let path = run_dir.join(DEFINITION_FILE);
    std::fs::write(&path, serde_json::to_vec_pretty(definition)?)
        .with_context(|| format!("writing {}", path.display()))
}

/// The definition `run_dir` froze, verified against the digest its run
/// recorded. `Ok(None)` for a run frozen before the file existed; the caller
/// then falls back to the installed definition while it still matches.
pub fn read_frozen_definition(
    run_dir: &Path,
    frozen: &DefinitionRef,
) -> Result<Option<EvalDefinition>> {
    let path = run_dir.join(DEFINITION_FILE);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
    };
    let definition: EvalDefinition =
        serde_json::from_slice(&bytes).with_context(|| format!("parsing {}", path.display()))?;
    if definition_ref(&definition)?.digest != frozen.digest {
        return Err(refused(format!(
            "{} no longer digests to the definition its run froze ({})",
            path.display(),
            frozen.digest
        )));
    }
    Ok(Some(definition))
}
```

In `freeze`, after `write_sidecar(&run_dir, &sidecar)?;` add `write_frozen_definition(&run_dir, &definition)?;`.

In `thaw`, replace the block from `let definition_id = &record.origin.definition.definition_id;` through the closing `}` of `if definition_ref(&definition)?.digest != record.origin.definition.digest { … }` with:

```rust
    let definition = match read_frozen_definition(&run_dir, &record.origin.definition)? {
        Some(definition) => definition,
        None => {
            let definition_id = &record.origin.definition.definition_id;
            let definition: EvalDefinition =
                read_document(access, Collection::EvalDefinition, owner, definition_id)
                    .await?
                    .ok_or_else(|| {
                        refused(format!(
                            "run {run_id} names no eval definition {definition_id:?}"
                        ))
                    })?;
            if definition_ref(&definition)?.digest != record.origin.definition.digest {
                return Err(refused(format!(
                    "eval definition {definition_id:?} changed since run {run_id} froze it"
                )));
            }
            definition
        }
    };
```

In `crates/gents/src/eval/runner/mod.rs`, extend `pub use freeze::{…}` with `read_frozen_definition, DEFINITION_FILE`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents --lib eval::runner > "$LOG/t2a.log" 2>&1; grep -E "^test |test result|panicked" "$LOG/t2a.log" | tail -40`
Expected: both new tests and every existing runner test pass.

- [ ] **Step 5: Format and commit**

```bash
cargo fmt --all && cargo fmt --all --check
git add crates/gents/src/eval/runner/
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" commit -m "feat(eval): freeze writes the run's definition beside it and resume reads that copy (#1515)"
```

### Task 2b: `Comparison::with_policy` and the policy outcome

**Files:**
- Modify: `crates/gents/src/eval/report/compare.rs` (`GateView`, `PolicyOutcome`, the `policy` field, `with_policy`, tests)
- Modify: `crates/gents/src/eval/report/mod.rs` (extend the `compare` re-export)

**Interfaces:**
- Consumes (Task 2, and `optimization::policy`):
  ```rust
  impl Comparison { pub fn evidence(&self, policy: &PolicyV2) -> Evidence; pub(crate) fn restate(&mut self, policy: &PolicyV2) -> DecisionReport }
  pub fn sufficient(policy: &PolicyV2, evidence: &Evidence) -> bool;
  pub fn no_case_regression(policy: &PolicyV2, evidence: &Evidence) -> bool;
  pub fn cost_ok(policy: &PolicyV2, tokens: &TokenTotals) -> bool;
  pub fn decision_evidence(definition: &EvalDefinition, runs: &[RunRows], max_missing_usage_bp: u64) -> Evidence;   // test only
  pub const BASELINE_CELL: &str = "baseline";  pub const CANDIDATE_CELL: &str = "candidate";                        // test only
  ```
- Produces:
  ```rust
  pub struct PolicyOutcome { pub report: DecisionReport, pub gates: GateView, pub calibrated: bool }
  pub struct GateView { pub sufficient: bool, pub no_case_regression: bool, pub cost_ok: Option<bool>,
      pub significant: bool, pub min_effect: bool }
  // Comparison gains `pub policy: Option<PolicyOutcome>` after `p_ppm`
  impl Comparison { pub fn with_policy(self, policy: &PolicyV2) -> Self }
  ```

- [ ] **Step 1: Write the failing tests**

Append inside `mod tests` in `crates/gents/src/eval/report/compare.rs`:

```rust
    use crate::optimization::evidence::{decision_evidence, BASELINE_CELL, CANDIDATE_CELL};
    use crate::optimization::policy::Decision;

    const SIX: [&str; 6] = ["a", "b", "c", "d", "e", "f"];

    /// Twenty-one case ids, for the Monte Carlo branch above twenty cases.
    const MANY: [&str; 21] = [
        "c00", "c01", "c02", "c03", "c04", "c05", "c06", "c07", "c08", "c09", "c10", "c11", "c12",
        "c13", "c14", "c15", "c16", "c17", "c18", "c19", "c20",
    ];

    /// `cases`, two trials each: the baseline fails and the candidate passes
    /// every one. The last `unmetered` candidate trials report no usage.
    fn improving(cases: &[&'static str], unmetered: usize) -> (EvalDefinition, Rows) {
        let definition = definition(cases);
        let mut rows = Rows::default();
        let mut left = cases.len() * 2 - unmetered;
        for &case in cases {
            for index in 0..2 {
                rows = rows.add(RUN, at(BASELINE_CELL, case, index, 1), fail());
                rows = rows.add(RUN, at(CANDIDATE_CELL, case, index, 1), pass());
                if left == 0 {
                    rows = rows.unmetered();
                } else {
                    left -= 1;
                }
            }
        }
        (definition, rows)
    }

    /// The comparison's policy outcome and `decide` on the optimizer's own
    /// evidence for the same documents.
    fn against_the_optimizer(
        cases: &[&'static str],
        unmetered: usize,
        policy: &PolicyV2,
    ) -> (Comparison, DecisionReport) {
        let (definition, rows) = improving(cases, unmetered);
        let report = report(&definition, &rows, &[BASELINE_CELL, CANDIDATE_CELL], 2);
        let comparison = compare(&report, &report, BASELINE_CELL, CANDIDATE_CELL)
            .unwrap()
            .with_policy(policy);
        let expected = decide(
            Mode::Improve,
            policy,
            &decision_evidence(
                &definition,
                &[run_rows(&definition, &rows)],
                policy.max_missing_usage_bp,
            ),
            decision_seed(&[RUN.to_owned()]),
        );
        (comparison, expected)
    }

    #[test]
    fn the_policy_outcome_is_decide_on_the_optimizers_evidence() {
        let policy = PolicyV2::uncalibrated();
        let (comparison, expected) = against_the_optimizer(&SIX, 0, &policy);
        let outcome = comparison.policy.as_ref().expect("with_policy sets it");
        assert_eq!(outcome.report, expected);
        assert_eq!(expected.decision, Decision::Accept);
        assert!(!expected.cost_skipped);
        assert!(!outcome.calibrated, "the placeholder defaults are uncalibrated");
        assert_eq!(
            outcome.gates,
            GateView {
                sufficient: true,
                no_case_regression: true,
                cost_ok: Some(true),
                significant: true,
                min_effect: true,
            }
        );
        assert_eq!(comparison.p_ppm, expected.p_ppm);
    }

    #[test]
    fn the_cost_gate_is_skipped_exactly_when_the_optimizer_skips_it() {
        // Five of 24 trials unmetered is past the 2000 bp tolerance.
        let policy = PolicyV2::uncalibrated();
        let (comparison, expected) = against_the_optimizer(&SIX, 5, &policy);
        let outcome = comparison.policy.as_ref().unwrap();
        assert_eq!(outcome.report, expected);
        assert!(expected.cost_skipped);
        assert_eq!(outcome.gates.cost_ok, None);
    }

    #[test]
    fn a_policy_other_than_the_defaults_is_calibrated() {
        let policy = PolicyV2 {
            min_pairs: 1,
            ..PolicyV2::uncalibrated()
        };
        let (comparison, expected) = against_the_optimizer(&SIX, 0, &policy);
        let outcome = comparison.policy.unwrap();
        assert_eq!(outcome.report, expected);
        assert!(outcome.calibrated);
    }

    /// Ruling F4: above twenty cases `decide` samples, so the seed matters.
    /// The comparison of one run's two cells seeds as `optimization show`
    /// recomputes that run's decision: over the run id once.
    #[test]
    fn above_twenty_cases_the_p_value_is_the_one_optimization_show_computes() {
        let policy = PolicyV2::uncalibrated();
        let (comparison, expected) = against_the_optimizer(&MANY, 0, &policy);
        assert_eq!(comparison.policy.as_ref().unwrap().report, expected);
        assert_eq!(comparison.p_ppm, expected.p_ppm);
        assert!(expected.p_ppm.is_some());
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents --lib eval::report::compare > "$LOG/t2b.log" 2>&1; grep -E "error(\[|:)|test result" "$LOG/t2b.log" | head -20`
Expected: compile errors: no method `with_policy`; `GateView` not found.

- [ ] **Step 3: Write the implementation**

In `crates/gents/src/eval/report/compare.rs`, extend the `crate::optimization::policy::{…}` import with `cost_ok, no_case_regression, sufficient`; add after `CaseComparison`:

```rust
/// Each gate `decide` applies, as far as it can be read from outside it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct GateView {
    pub sufficient: bool,
    pub no_case_regression: bool,
    /// `None` when too many trials reported no usage and the gate is skipped.
    pub cost_ok: Option<bool>,
    /// `p_ppm <= alpha_effective_ppm`.
    pub significant: bool,
    /// `mean_diff_bp >= min_effect_bp`.
    pub min_effect: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PolicyOutcome {
    pub report: DecisionReport,
    pub gates: GateView,
    /// False while the policy equals the placeholder defaults, which only the
    /// A/A calibration (M5) replaces.
    pub calibrated: bool,
}
```

add the field after `pub p_ppm: Option<u64>,` in `Comparison`:

```rust
    /// Only after [`Comparison::with_policy`]: the optimizer decides by
    /// default, never a comparison.
    pub policy: Option<PolicyOutcome>,
```

add `policy: None,` after `p_ppm: None,` in `compare`'s constructor, and in `impl Comparison`:

```rust
    /// Decide under `policy` and restate the statistics under it.
    pub fn with_policy(mut self, policy: &PolicyV2) -> Self {
        let evidence = self.evidence(policy);
        let report = self.restate(policy);
        let gates = GateView {
            sufficient: sufficient(policy, &evidence),
            no_case_regression: no_case_regression(policy, &evidence),
            cost_ok: evidence.tokens.as_ref().map(|tokens| cost_ok(policy, tokens)),
            significant: report
                .p_ppm
                .is_some_and(|p| p <= report.alpha_effective_ppm),
            min_effect: report.mean_diff_bp.is_some_and(|mean| {
                mean >= i64::try_from(policy.min_effect_bp).unwrap_or(i64::MAX)
            }),
        };
        self.policy = Some(PolicyOutcome {
            report,
            gates,
            calibrated: *policy != PolicyV2::uncalibrated(),
        });
        self
    }
```

In `crates/gents/src/eval/report/mod.rs`, change the compare re-export to `pub use compare::{compare, CaseComparison, Comparison, GateView, PolicyOutcome};`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents --lib eval::report > "$LOG/t2b.log" 2>&1; grep -E "^test |test result|panicked" "$LOG/t2b.log" | tail -30`
Expected: the four policy tests pass with Task 2's three.

- [ ] **Step 5: Format and commit**

```bash
cargo fmt --all && cargo fmt --all --check
git add crates/gents/src/eval/report/
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" commit -m "feat(eval): Comparison::with_policy decides on the optimizer's evidence, labelled (#1515)"
```

### Task 3: `report::store`, the one reader

**Files:**
- Create: `crates/gents/src/eval/report/store.rs`
- Modify: `crates/gents/src/eval/report/mod.rs` (add `pub mod store;` last among the `mod` lines and a `pub use store::{…}` line)

**Interfaces:**
- Consumes:
  ```rust
  // crates/gents/src/eval/documents.rs (frozen)
  pub async fn load_run(access: &ConfigAccess, owner: &str, run_id: &str) -> Result<Option<RunRecord>>;
  pub async fn load_trials(access: &ConfigAccess, owner: &str, run_id: &str) -> Result<Vec<TrialRecord>>;
  pub async fn load_verdicts(access: &ConfigAccess, owner: &str, run_id: &str) -> Result<Vec<VerdictRecord>>;
  pub async fn create_run(access: &ConfigAccess, run_id: &str, owner: &str, evaluator_did: &str, origin: &RunOrigin) -> Result<RunRecord>;  // test only
  pub async fn invalidate_run(access: &ConfigAccess, owner: &str, run_id: &str, by: &str, reason: &str) -> Result<()>;  // test only
  // crates/gents/src/eval/runner/freeze.rs
  pub(crate) async fn load_definition(access: &ConfigAccess, owner: &str, definition_id: &str) -> Result<EvalDefinition>;
  pub(crate) fn definition_ref(definition: &EvalDefinition) -> Result<DefinitionRef>;
  pub fn read_frozen_definition(run_dir: &Path, frozen: &DefinitionRef) -> Result<Option<EvalDefinition>>;   // Task 2a
  pub const DEFINITION_FILE: &str = "definition.json";                                                    // Task 2a
  // ConfigAccess::transact(&self, operation: &'static str, callback) -> Result<T>; ConfigApplyTxn::execute(&self, query: &str) -> Result<Value>
  // crates/gents/src/eval/runner/freeze.rs #[cfg(test)] pub(crate) mod tests
  pub(crate) const OWNER: &str = "did:key:eval-owner";
  pub(crate) struct Launching { pub(crate) access: ConfigAccess, .. }
  impl Launching { pub(crate) async fn new() -> Self; pub(crate) fn pack(&self, name: &str, bash_mode: &str) -> PathBuf;
      pub(crate) fn runs_dir(&self) -> PathBuf; pub(crate) fn evaluator_did(&self) -> String; }
  // Launching installs definition "monitor-findings" (validation case "disk-warning", train case "train-case"),
  // whose captured_rows_count check has no `min`: every graded trial is a Grader verdict, class Unknown.
  ```
- Produces:
  ```rust
  pub async fn load_runs(access: &ConfigAccess, owner: &str) -> Result<Vec<RunRecord>>;   // ordered by (created_at, run_id)
  pub fn run_header(record: &RunRecord) -> RunHeader;
  pub async fn load_report_among(access: &ConfigAccess, owner: &str, runs_dir: &Path, record: &RunRecord, peers: &[RunHeader]) -> Result<EvalReport>;
  pub async fn load_report(access: &ConfigAccess, owner: &str, runs_dir: &Path, run_id: &str) -> Result<EvalReport>;  // ReportRefused for an unknown run
  ```
  Definition choice (ruling U2): the run's `<runs_dir>/<run_id>/definition.json` when present (verified by digest), with `EvalReport.definition_changed` set when the installed definition is missing or digests differently; otherwise (a run frozen before Task 2a, or a directory `rm`/`gc` removed) the installed definition, which `build` refuses unless it still digests to the run's.

- [ ] **Step 1: Write the failing tests**

Create `crates/gents/src/eval/report/store.rs` holding only:

```rust
#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::Duration;

    use serde_json::json;
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::document_config::EvalSplit;
    use crate::eval::checks::CheckRegistry;
    use crate::eval::report::{report_refused, SlotClass};
    use crate::eval::runner::freeze::tests::{Launching, OWNER};
    use crate::eval::runner::{
        run, CellRequest, CellSource, RunOptions, RunRequest, ScriptedExecutor,
    };
    use crate::eval::{create_run, invalidate_run};

    fn request(launching: &Launching, pack: &Path, run_id: &str) -> RunRequest {
        RunRequest {
            run_id: run_id.into(),
            owner: OWNER.into(),
            evaluator_did: launching.evaluator_did(),
            definition_id: "monitor-findings".into(),
            split: EvalSplit::Validation,
            case_ids: None,
            cells: vec![CellRequest {
                cell_id: "baseline".into(),
                label: "baseline".into(),
                source: CellSource::Directory(pack.to_path_buf()),
                behavior_id: "monitor".into(),
                inference_profile_id: "local".into(),
            }],
            trials_per_case: 2,
            seed_base: 1_000,
            deadline_secs: Some(600),
            concurrency: 1,
            max_infra_retries: 0,
            breaker_threshold: 5,
            purpose: "eval".into(),
            source_commit: "store-test".into(),
            source_dirty: false,
            captures: Vec::new(),
            runs_dir: launching.runs_dir(),
        }
    }

    async fn scripted(launching: &Launching, request: &RunRequest) {
        let executor = ScriptedExecutor::new().with_default(ScriptedExecutor::passed_evidence(
            "did:key:trial",
            "check",
            "findings",
            vec![json!({})],
        ));
        run(
            &launching.access,
            request,
            &executor,
            &CheckRegistry::builtin(),
            CancellationToken::new(),
            &RunOptions {
                poll_backoff_base: Duration::from_millis(1),
                poll_backoff_cap: Duration::from_millis(2),
            },
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn a_report_is_built_from_what_a_run_wrote() {
        let launching = Launching::new().await;
        let pack = launching.pack("pack", "Off");
        scripted(&launching, &request(&launching, &pack, "run-1")).await;

        let report = load_report(&launching.access, OWNER, &launching.runs_dir(), "run-1").await.unwrap();
        let cell = &report.cells[0];
        // Launching's check has no `min`, so every trial is graded by a
        // grader error: Unknown, never a failure of the subject.
        assert_eq!(
            cell.slots.iter().map(|slot| slot.class).collect::<Vec<_>>(),
            vec![SlotClass::Unknown; 2]
        );
        assert_eq!((cell.attempts, report.exposure), (2, 1));
        assert_eq!(report.run.source_commit, "store-test");
        assert!(!report.definition_changed);
    }

    #[tokio::test]
    async fn runs_are_listed_per_owner_and_exposure_skips_invalidated_runs() {
        let launching = Launching::new().await;
        let pack = launching.pack("pack", "Off");
        scripted(&launching, &request(&launching, &pack, "run-1")).await;
        scripted(&launching, &request(&launching, &pack, "run-2")).await;
        invalidate_run(&launching.access, OWNER, "run-1", OWNER, "fixture was broken")
            .await
            .unwrap();
        let first = load_run(&launching.access, OWNER, "run-1")
            .await
            .unwrap()
            .unwrap();
        create_run(
            &launching.access,
            "foreign",
            "did:key:someone-else",
            "did:key:someone-else",
            &first.origin,
        )
        .await
        .unwrap();

        let runs = load_runs(&launching.access, OWNER).await.unwrap();
        assert_eq!(
            runs.iter().map(|run| run.run_id.as_str()).collect::<Vec<_>>(),
            vec!["run-1", "run-2"]
        );
        assert!(run_header(&runs[0]).invalidated);
        let report = load_report(&launching.access, OWNER, &launching.runs_dir(), "run-2").await.unwrap();
        assert_eq!(report.exposure, 1, "the invalidated run is not exposure");
    }

    #[tokio::test]
    async fn a_report_reads_the_frozen_definition_after_the_installed_one_changes() {
        let launching = Launching::new().await;
        let pack = launching.pack("pack", "Off");
        scripted(&launching, &request(&launching, &pack, "run-1")).await;
        let installed = load_definition(&launching.access, OWNER, "monitor-findings")
            .await
            .unwrap();
        let mut edited = serde_json::to_value(&installed).unwrap();
        edited["comparability_version"] = json!(2);
        launching
            .install(vec![(crate::Collection::EvalDefinition, edited)])
            .await;

        let report = load_report(&launching.access, OWNER, &launching.runs_dir(), "run-1")
            .await
            .unwrap();
        assert!(report.definition_changed);
        assert_eq!(report.run.definition.comparability_version, 1);
        assert_eq!(report.cells[0].slots.len(), 2);

        // Without the frozen copy, the installed definition no longer matches.
        std::fs::remove_file(
            launching
                .runs_dir()
                .join("run-1")
                .join(crate::eval::runner::DEFINITION_FILE),
        )
        .unwrap();
        let error = load_report(&launching.access, OWNER, &launching.runs_dir(), "run-1")
            .await
            .unwrap_err();
        assert!(
            report_refused(&error).is_some_and(|refusal| refusal.0.contains("no longer digests")),
            "{error:#}"
        );
    }

    /// Ruling F9: a run frozen before `definition.json` existed still
    /// reports while the installed definition digests to the run's.
    #[tokio::test]
    async fn a_run_without_the_frozen_file_reports_from_a_matching_installed_definition() {
        let launching = Launching::new().await;
        let pack = launching.pack("pack", "Off");
        scripted(&launching, &request(&launching, &pack, "run-old")).await;
        std::fs::remove_file(
            launching
                .runs_dir()
                .join("run-old")
                .join(crate::eval::runner::DEFINITION_FILE),
        )
        .unwrap();
        let report = load_report(&launching.access, OWNER, &launching.runs_dir(), "run-old")
            .await
            .unwrap();
        assert!(!report.definition_changed);
        assert_eq!(report.cells[0].slots.len(), 2);
    }

    #[tokio::test]
    async fn an_unknown_run_is_refused() {
        let launching = Launching::new().await;
        let error = load_report(&launching.access, OWNER, &launching.runs_dir(), "absent")
            .await
            .unwrap_err();
        let reason = &report_refused(&error).expect("a ReportRefused").0;
        assert_eq!(reason, &format!("no eval run \"absent\" for {OWNER}"));
    }
}
```

In `crates/gents/src/eval/report/mod.rs` add `pub mod store;` after `pub mod evidence;` (rustfmt order: `build`, `compare`, `evidence`, `fixtures`, `store`).

- [ ] **Step 2: Run the tests to verify they fail**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents --lib eval::report::store > "$LOG/t3.log" 2>&1; grep -E "error(\[|:)|test result" "$LOG/t3.log" | head -20`
Expected: compile errors `cannot find function `load_report``, `load_runs`, `run_header`.

- [ ] **Step 3: Write the implementation**

Insert above the test module in `crates/gents/src/eval/report/store.rs`:

```rust
//! Gathering a run's documents for a report: the module's one I/O boundary
//! (loading rows for an owner, and the run's frozen definition), alongside
//! M6b's `evidence::load_run_rows`. `build`, `compare` and the breakdowns are
//! pure (ruling U9).
//!
//! `eval::documents` is frozen and has no list read, so listing asks
//! `EvalRun` for the owner's run ids and reads each through `load_run`,
//! keeping the row decoding in one place.

use std::path::Path;

use anyhow::Result;

use crate::config_client::ConfigAccess;
use crate::eval::report::build::{build, EvalReport};
use crate::eval::report::refused;
use crate::eval::runner::freeze::{definition_ref, load_definition, read_frozen_definition};
use crate::eval::{load_run, load_trials, load_verdicts, RunHeader, RunRecord};
use crate::graphql::escape_graphql_string;

/// Every run `owner` has, oldest first (`created_at`, then `run_id`).
pub async fn load_runs(access: &ConfigAccess, owner: &str) -> Result<Vec<RunRecord>> {
    let query = format!(
        r#"{{ EvalRun(filter: {{ owner_agent_did: {{ _eq: "{owner}" }} }}) {{ run_id }} }}"#,
        owner = escape_graphql_string(owner),
    );
    let response = access
        .transact("eval.report.load_runs", |txn| {
            let query = &query;
            Box::pin(async move { txn.execute(query).await })
        })
        .await?;
    let mut run_ids: Vec<String> = response["data"]["EvalRun"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|row| row["run_id"].as_str().map(str::to_owned))
        .collect();
    run_ids.sort();
    run_ids.dedup();
    let mut runs = Vec::with_capacity(run_ids.len());
    for run_id in run_ids {
        if let Some(record) = load_run(access, owner, &run_id).await? {
            runs.push(record);
        }
    }
    runs.sort_by(|left, right| {
        (&left.created_at, &left.run_id).cmp(&(&right.created_at, &right.run_id))
    });
    Ok(runs)
}

/// What `eval::scoring::exposure` counts about a run.
pub fn run_header(record: &RunRecord) -> RunHeader {
    RunHeader {
        definition_id: record.origin.definition.definition_id.clone(),
        comparability_version: record.origin.definition.comparability_version,
        split: record.origin.split,
        invalidated: record.invalidated.is_some(),
    }
}

/// `record`'s report, with exposure counted over `peers`. For a caller that
/// already listed the owner's runs. `runs_dir` is `<launching home>/eval/runs`.
///
/// The definition is the one the run froze (ruling U2); the installed one is
/// read only to say whether it has changed since.
pub async fn load_report_among(
    access: &ConfigAccess,
    owner: &str,
    runs_dir: &Path,
    record: &RunRecord,
    peers: &[RunHeader],
) -> Result<EvalReport> {
    let frozen = &record.origin.definition;
    // The run id came through freeze, which holds it to one path component.
    let run_dir = runs_dir.join(&record.run_id);
    let live = load_definition(access, owner, &frozen.definition_id).await;
    let (definition, definition_changed) = match read_frozen_definition(&run_dir, frozen)? {
        Some(definition) => {
            let changed = match &live {
                Ok(live) => definition_ref(live)?.digest != frozen.digest,
                Err(_) => true,
            };
            (definition, changed)
        }
        // A run frozen before the file existed, or whose directory was
        // removed: the installed definition, which `build` refuses unless it
        // still digests to the run's.
        None => (live?, false),
    };
    let trials = load_trials(access, owner, &record.run_id).await?;
    let verdicts = load_verdicts(access, owner, &record.run_id).await?;
    let mut report = build(record, &trials, &verdicts, &definition, peers)?;
    report.definition_changed = definition_changed;
    Ok(report)
}

/// `run_id`'s report, read fresh from the documents.
pub async fn load_report(
    access: &ConfigAccess,
    owner: &str,
    runs_dir: &Path,
    run_id: &str,
) -> Result<EvalReport> {
    let record = load_run(access, owner, run_id)
        .await?
        .ok_or_else(|| refused(format!("no eval run {run_id:?} for {owner}")))?;
    let peers: Vec<RunHeader> = load_runs(access, owner)
        .await?
        .iter()
        .map(run_header)
        .collect();
    load_report_among(access, owner, runs_dir, &record, &peers).await
}
```

Add to `crates/gents/src/eval/report/mod.rs` after the `pub use evidence::{…};` block:

```rust
pub use store::{load_report, load_report_among, load_runs, run_header};
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents --lib eval::report > "$LOG/t3.log" 2>&1; grep -E "^test |test result|panicked" "$LOG/t3.log" | tail -30`
Expected: five `store::tests` pass; the rest of `eval::report` still passes.

- [ ] **Step 5: Format and commit**

```bash
cargo fmt --all && cargo fmt --all --check
git add crates/gents/src/eval/report/
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" commit -m "feat(eval): report::store lists runs and loads a report from the documents (#1515)"
```

### Task 4: The cancel marker

**Files:**
- Modify: `crates/gents/src/eval/runner/mod.rs` — imports (line 31 `use std::path::Path;`), new items after `pub fn provider_down` (lines 95-97), `resume` (lines 137-150), `execute_frozen` (lines 171-289), `execute_trial` (line 304), and the `tests` module (from line 602)

**Interfaces:**
- Consumes:
  ```rust
  // crates/gents/src/eval/runner/freeze.rs
  pub(crate) fn directory_name(kind: &str, value: &str) -> Result<()>;   // FreezeRefused "{kind} {value:?} must be one ordinary path component"
  pub(crate) fn refused(reason: impl Into<String>) -> anyhow::Error;       // a FreezeRefused
  pub fn freeze_refused(error: &anyhow::Error) -> Option<&FreezeRefused>;
  pub(crate) async fn thaw(access, owner, run_id, runs_dir, isolation) -> Result<FrozenRun>;   // FrozenRun { run_dir: PathBuf, .. }
  // crates/gents/src/eval/runner/mod.rs tests module (existing helpers)
  async fn launching(check: &str) -> (Launching, PathBuf);
  fn request(launching: &Launching, pack: &Path, run_id: &str) -> RunRequest;   // cells "base" and "cand", cases case-a and case-b, trials 2
  fn one_slot(request: &mut RunRequest);
  fn options() -> RunOptions;
  fn passed() -> TrialEvidence;
  ```
- Produces:
  ```rust
  pub const CANCEL_MARKER: &str = "cancel";
  pub fn run_dir(runs_dir: &Path, run_id: &str) -> Result<PathBuf>;          // FreezeRefused for a run id that is not one path component
  pub fn request_cancel(runs_dir: &Path, run_id: &str) -> Result<PathBuf>;   // FreezeRefused "run {run_id} has no directory {dir}"
  // RunOutcome gains (ruling F1):
  pub cancelled: bool,   // the caller's token or the marker stopped this pass
  ```
  Behavior: the loop treats the marker as cancellation at the top of each pass, before each launch and after each pass (Task 6a adds a timer while a batch is in flight); a marker cancels a child of the caller's token; `resume` and `run` remove the marker before planning (spec §3, ruling U8).

- [ ] **Step 1: Write the failing tests**

Append inside `mod tests` in `crates/gents/src/eval/runner/mod.rs`, after `cancel_stops_launching_and_leaves_in_flight_rows_null`:

```rust
    /// Writes its run's cancel marker the first time it executes a trial,
    /// then answers like the scripted executor: another process running
    /// `gents eval cancel` while this one works.
    struct MarkOnFirstExecute {
        inner: ScriptedExecutor,
        runs_dir: PathBuf,
        run_id: String,
        marked: AtomicBool,
    }

    #[async_trait::async_trait]
    impl TrialExecutor for MarkOnFirstExecute {
        fn isolation(&self) -> Isolation {
            self.inner.isolation()
        }

        fn wants_script_key(&self) -> bool {
            self.inner.wants_script_key()
        }

        async fn provision(&self, spec: &TrialSpec) -> TrialLocator {
            self.inner.provision(spec).await
        }

        async fn execute(&self, spec: &TrialSpec, cancel: CancellationToken) -> TrialEvidence {
            if !self.marked.swap(true, Ordering::SeqCst) {
                request_cancel(&self.runs_dir, &self.run_id).unwrap();
            }
            self.inner.execute(spec, cancel).await
        }

        async fn recollect(
            &self,
            at: &TrialLocator,
            captures: &[Capture],
        ) -> Option<TrialEvidence> {
            self.inner.recollect(at, captures).await
        }
    }

    #[tokio::test]
    async fn a_cancel_marker_stops_the_loop_at_its_next_launch_and_resume_clears_it() {
        let (launching, pack) = launching("captured_rows_count").await;
        let mut request = request(&launching, &pack, "run-marker");
        request.cells.truncate(1);
        request.trials_per_case = 1;
        let caller = CancellationToken::new();
        let executor = MarkOnFirstExecute {
            inner: ScriptedExecutor::new().with_default(passed()),
            runs_dir: launching.runs_dir(),
            run_id: "run-marker".into(),
            marked: AtomicBool::new(false),
        };
        let outcome = run(
            &launching.access,
            &request,
            &executor,
            &CheckRegistry::builtin(),
            caller.clone(),
            &options(),
        )
        .await
        .unwrap();
        assert_eq!(
            (outcome.completed, outcome.abandoned),
            (1, 0),
            "the trial that was running finishes; the next is never launched"
        );
        assert!(outcome.cancelled, "the pass says why it stopped (F1)");
        assert!(
            !caller.is_cancelled(),
            "the marker stops this run, never the caller's other work"
        );
        let marker = launching.runs_dir().join("run-marker").join(CANCEL_MARKER);
        assert!(marker.exists());
        assert_eq!(
            load_trials(&launching.access, OWNER, "run-marker")
                .await
                .unwrap()
                .len(),
            1
        );

        let resumed = resume(
            &launching.access,
            OWNER,
            "run-marker",
            &launching.runs_dir(),
            &ScriptedExecutor::new().with_default(passed()),
            &CheckRegistry::builtin(),
            CancellationToken::new(),
            &options(),
        )
        .await
        .unwrap();
        assert_eq!(resumed.completed, 1);
        assert!(!marker.exists(), "resume removes the marker before it plans");
        assert_eq!(
            load_trials(&launching.access, OWNER, "run-marker")
                .await
                .unwrap()
                .len(),
            2
        );
    }

    /// `run` and `resume` clear the marker before the loop, so the loop is
    /// driven directly to see a marker that is already there.
    #[tokio::test]
    async fn a_marker_written_before_the_loop_starts_launches_nothing() {
        let (launching, pack) = launching("captured_rows_count").await;
        let mut request = request(&launching, &pack, "run-early");
        one_slot(&mut request);
        let frozen = freeze(&launching.access, &request, Isolation::Embedded)
            .await
            .unwrap();
        request_cancel(&launching.runs_dir(), "run-early").unwrap();
        let outcome = execute_frozen(
            &frozen,
            &DocumentRecorder(&launching.access),
            &ScriptedExecutor::new().with_default(passed()),
            &CheckRegistry::builtin(),
            CancellationToken::new(),
            &options(),
        )
        .await
        .unwrap();
        assert_eq!(
            outcome,
            RunOutcome {
                run_id: "run-early".into(),
                cancelled: true,
                ..RunOutcome::default()
            }
        );
        assert!(load_trials(&launching.access, OWNER, "run-early")
            .await
            .unwrap()
            .is_empty());
    }

    /// Ruling U8: `run` on a run id that already exists reuses the run, and
    /// clears a leftover marker exactly as `resume` does.
    #[tokio::test]
    async fn run_on_an_existing_run_id_clears_the_marker_like_resume() {
        let (launching, pack) = launching("captured_rows_count").await;
        let mut request = request(&launching, &pack, "run-again");
        one_slot(&mut request);
        freeze(&launching.access, &request, Isolation::Embedded)
            .await
            .unwrap();
        let marker = request_cancel(&launching.runs_dir(), "run-again").unwrap();
        let outcome = run(
            &launching.access,
            &request,
            &ScriptedExecutor::new().with_default(passed()),
            &CheckRegistry::builtin(),
            CancellationToken::new(),
            &options(),
        )
        .await
        .unwrap();
        assert_eq!(outcome.completed, 1);
        assert!(!outcome.cancelled);
        assert!(!marker.exists());
    }

    #[test]
    fn request_cancel_refuses_a_missing_run_directory_and_a_path_for_an_id() {
        let dir = tempfile::tempdir().unwrap();
        let missing = request_cancel(dir.path(), "absent").unwrap_err();
        assert!(
            freeze_refused(&missing).is_some_and(|refusal| refusal.0.contains("has no directory")),
            "{missing:#}"
        );
        let escaping = request_cancel(dir.path(), "../elsewhere").unwrap_err();
        assert!(
            freeze_refused(&escaping)
                .is_some_and(|refusal| refusal.0.contains("one ordinary path component")),
            "{escaping:#}"
        );
        assert_eq!(
            run_dir(dir.path(), "run-1").unwrap(),
            dir.path().join("run-1")
        );
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents --lib eval::runner::tests > "$LOG/t4.log" 2>&1; grep -E "error(\[|:)|test result" "$LOG/t4.log" | head -20`
Expected: compile errors `cannot find function `request_cancel``, `run_dir`, `CANCEL_MARKER`.

- [ ] **Step 3: Write the implementation**

In `crates/gents/src/eval/runner/mod.rs`:

1. Replace `use std::path::Path;` with `use std::path::{Path, PathBuf};`.

2. After `pub fn provider_down(…) { … }` insert:

```rust
/// The file whose presence asks a run to stop: `<run dir>/cancel` (spec 4a
/// §3). Another process on this machine writes it with [`request_cancel`];
/// the loop checks for it wherever it checks its own cancellation token, and
/// [`resume`] removes it. It carries no state and is not a document, so a
/// remote host never sees it; remote cancel is spec 2b's.
pub const CANCEL_MARKER: &str = "cancel";

/// `<runs_dir>/<run_id>`, for a run id that is one ordinary path component.
pub fn run_dir(runs_dir: &Path, run_id: &str) -> Result<PathBuf> {
    crate::eval::runner::freeze::directory_name("run_id", run_id)?;
    Ok(runs_dir.join(run_id))
}

/// Ask the process hosting `run_id` to stop launching at its next check.
pub fn request_cancel(runs_dir: &Path, run_id: &str) -> Result<PathBuf> {
    let dir = run_dir(runs_dir, run_id)?;
    if !dir.is_dir() {
        return Err(crate::eval::runner::freeze::refused(format!(
            "run {run_id} has no directory {}",
            dir.display()
        )));
    }
    let marker = dir.join(CANCEL_MARKER);
    std::fs::write(&marker, b"").with_context(|| format!("writing {}", marker.display()))?;
    tracing::warn!(run_id, marker = %marker.display(), "eval run cancel requested");
    Ok(marker)
}

/// Whether the loop should stop: its token is cancelled, or the marker is
/// present, in which case the token is cancelled so every in-flight trial and
/// every later check reads the same answer.
fn cancel_requested(run_dir: &Path, cancel: &CancellationToken) -> bool {
    if cancel.is_cancelled() {
        return true;
    }
    if run_dir.join(CANCEL_MARKER).exists() {
        tracing::warn!(
            run_dir = %run_dir.display(),
            "eval run cancel marker found; the run stops launching"
        );
        cancel.cancel();
        return true;
    }
    false
}

/// Remove the marker a cancelled run left, if any.
fn clear_cancel(run_dir: &Path) -> Result<()> {
    let marker = run_dir.join(CANCEL_MARKER);
    match std::fs::remove_file(&marker) {
        Ok(()) => {
            tracing::info!(marker = %marker.display(), "eval run cancel marker cleared for resume");
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("removing {}", marker.display())),
    }
}
```

3. In `resume`, between `let frozen = thaw(…).await?;` and `let recorder = DocumentRecorder(access);` insert, and in `run`, between `let frozen = freeze(…).await?;` and `let recorder = DocumentRecorder(access);` insert the same line (ruling U8: `run` on an existing run id reuses the run and clears a leftover marker as `resume` does; a fresh run has none):

```rust
    clear_cancel(&frozen.run_dir)?;
```

4. In `execute_frozen`, as the first statement of the body (before `let owner = …`) insert:

```rust
    // A child of the caller's token: a cancel marker stops this run and never
    // the work that hosts it (an optimization job runs several runs).
    let cancel = cancel.child_token();
```

   As the first statement inside `loop {` (before `let existing = recorder.load_trials(…)`) insert:

```rust
        if cancel_requested(&frozen.run_dir, &cancel) {
            break;
        }
```

   Replace, after the `if let Some(consecutive_not_evidence) = tripped { … }` block,

```rust
        if cancel.is_cancelled() {
            break;
        }
```

   with

```rust
        if cancel_requested(&frozen.run_dir, &cancel) {
            break;
        }
```

5. Ruling F1: add to `RunOutcome`, after `breaker_tripped`,

```rust
    /// The caller's token or a cancel marker stopped this pass before the run
    /// owed nothing. A caller that decides on the run (the optimizer) must not
    /// read a cancelled pass as a finished one.
    pub cancelled: bool,
```

   set it at the end of `execute_frozen`, just before the closing `tracing::info!(…, "eval run pass finished")`:

```rust
    outcome.cancelled = cancel.is_cancelled();
```

   and in the existing test `a_two_cell_run_completes_every_slot_and_writes_verdicts_before_completion`, add `cancelled: false,` to its `RunOutcome { … }` literal.

6. In `execute_trial`, replace `if stop.load(Ordering::Relaxed) || cancel.is_cancelled() {` with `if stop.load(Ordering::Relaxed) || cancel_requested(&frozen.run_dir, cancel) {`. Leave the `if cancel.is_cancelled()` after `executor.execute(…)` as it is: by then the trial's evidence is in hand and only the token decides whether it is kept (deviation D7).

- [ ] **Step 4: Run the tests to verify they pass**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents --lib eval::runner > "$LOG/t4.log" 2>&1; grep -E "^test |test result|panicked" "$LOG/t4.log" | tail -40`
Expected: the four new tests pass, and every existing runner test, including `cancel_stops_launching_and_leaves_in_flight_rows_null`, still passes.

- [ ] **Step 5: Format and commit**

```bash
cargo fmt --all && cargo fmt --all --check
git add crates/gents/src/eval/runner/mod.rs
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" commit -m "feat(eval): a cancel marker in the run directory stops the loop at its next check (#1515)"
```

### Task 4a: The optimizer stops at a cancelled run (ruling F1)

**Files:**
- Modify: `crates/gents/src/optimization/driver.rs` (the three `execute_run(` call sites in `run_job`)
- Modify: `crates/gents/src/optimization/driver/matrix.rs` (imports; one executor wrapper; one test)

**Interfaces:**
- Consumes (read at `optimization/23-promote`, tip `75e753741`):
  ```rust
  pub(crate) async fn execute_run(access, request: &JobRequest, origin: &JobOrigin, plan: &RunPlan, executor: &dyn TrialExecutor,
      registry: &CheckRegistry, cancel: CancellationToken) -> Result<RunOutcome>;   // driver.rs:255
  // run_job calls it at driver.rs:1111 (train), 1222 (validation) and 1312 (held-out); each call is
  //   `execute_run(…, cancel.clone()).await?;` followed by `if cancel.is_cancelled() { return stopped(&job); }`
  let stopped = |job: &JobRecord| -> Result<JobOutcome> { Ok(outcome(job, JobState::Running)) };   // driver.rs:1056
  pub struct RunOutcome { .., pub cancelled: bool }                                                 // Task 4
  // matrix.rs (#[cfg(test)]): Harness::new, Harness::request(job_id, definition_id, budgets), script, base_executor, fail,
  //   budgets, repeating_proposer, drive, settle, journal, VALIDATION_CASES, DEFINITION, CANDIDATE_PROMPT
  // Validation and held-out runs have cells labelled "baseline" and "candidate"; a train run has only "baseline".
  // A trial's `TrialSpec::home_dir` is `<run dir>/trials/<trial_id>`.
  ```
- Produces: `run_job` treats `RunOutcome::cancelled` after `execute_run` as "stopped, no decision": it journals nothing further and returns the job `Running`, so a resume continues. `eval cancel` on a job's run therefore stops the job at that run.

- [ ] **Step 1: Write the failing test**

In `crates/gents/src/optimization/driver/matrix.rs`, change `use std::path::PathBuf;` to `use std::path::{Path, PathBuf};`, add `use std::sync::atomic::{AtomicBool, Ordering};`, and add `CANCEL_MARKER` to the `crate::eval::runner::{…}` import. Then add, after `struct CancellingExecutor`'s `impl`:

```rust
/// Writes its run's cancel marker the first time it runs a candidate trial:
/// an operator's `gents eval cancel` on the job's first validation run.
struct CancelsFirstValidationRun<'a> {
    inner: &'a ScriptedExecutor,
    marked: AtomicBool,
}

#[async_trait::async_trait]
impl TrialExecutor for CancelsFirstValidationRun<'_> {
    fn isolation(&self) -> Isolation {
        self.inner.isolation()
    }

    fn wants_script_key(&self) -> bool {
        self.inner.wants_script_key()
    }

    async fn provision(&self, spec: &TrialSpec) -> TrialLocator {
        self.inner.provision(spec).await
    }

    async fn discard(&self, trial_id: &str) {
        self.inner.discard(trial_id).await;
    }

    async fn execute(&self, spec: &TrialSpec, cancel: CancellationToken) -> TrialEvidence {
        let candidate = spec
            .script_key
            .as_ref()
            .is_some_and(|key| key.cell_label == "candidate");
        if candidate && !self.marked.swap(true, Ordering::SeqCst) {
            let run_dir = spec
                .home_dir
                .parent()
                .and_then(Path::parent)
                .expect("a trial home is <run dir>/trials/<trial_id>");
            std::fs::write(run_dir.join(CANCEL_MARKER), b"").unwrap();
        }
        self.inner.execute(spec, cancel).await
    }

    async fn recollect(&self, at: &TrialLocator, captures: &[Capture]) -> Option<TrialEvidence> {
        self.inner.recollect(at, captures).await
    }
}
```

and, among the tests:

```rust
/// Ruling F1: the marker stops only the run's own child token, so before
/// the fix the driver decided on half a validation run.
#[tokio::test]
async fn a_cancelled_validation_run_journals_no_decision_and_the_job_resumes() {
    let harness = Harness::new().await;
    let executor = script(base_executor(), "baseline", &VALIDATION_CASES, |_| fail());
    let request = harness.request("job-cancelled", DEFINITION, budgets(1_000));
    let marking = CancelsFirstValidationRun {
        inner: &executor,
        marked: AtomicBool::new(false),
    };
    let stopped = drive(
        &harness,
        &request,
        &marking,
        &repeating_proposer(CANDIDATE_PROMPT),
        &CheckRegistry::builtin(),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(stopped.state, JobState::Running);
    let entries = journal(&harness, "job-cancelled").await;
    assert!(
        entries.iter().any(|entry| matches!(
            entry,
            JournalEntry::RunStarted {
                split: EvalSplit::Validation,
                ..
            }
        )),
        "{entries:#?}"
    );
    assert!(
        !entries
            .iter()
            .any(|entry| matches!(entry, JournalEntry::Decided { .. })),
        "a cancelled run is never decided on: {entries:#?}"
    );

    let resumed = settle(
        &harness,
        &request,
        &executor,
        &repeating_proposer(CANDIDATE_PROMPT),
    )
    .await;
    assert_eq!(resumed.state, JobState::ReadyToPromote, "{resumed:#?}");
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents --lib optimization::driver::matrix::a_cancelled_validation_run > "$LOG/t4a.log" 2>&1; grep -E "^test |test result|panicked" "$LOG/t4a.log"`
Expected: FAIL on "a cancelled run is never decided on": the driver decided on the partial run.

- [ ] **Step 3: Write the implementation**

In `crates/gents/src/optimization/driver.rs`, at each of the three call sites in `run_job` (train at line 1111, validation at 1222, held-out at 1312), bind the outcome and widen the check that follows. Each site reads

```rust
        execute_run(
            access,
            request,
            &origin,
            &<plan>,
            executor,
            registry,
            cancel.clone(),
        )
        .await?;
        if cancel.is_cancelled() {
            return stopped(&job);
        }
```

(indentation varies by site; `<plan>` is `train` or `plan`). Change `execute_run(` to `let ran = execute_run(` and the `if` to:

```rust
        // Ruling F1: a run its cancel marker stopped is unfinished. The job
        // stops at it, journals nothing more, and a resume continues it.
        if cancel.is_cancelled() || ran.cancelled {
            return stopped(&job);
        }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents --lib optimization > "$LOG/t4a.log" 2>&1; grep -E "test result|FAILED|panicked" "$LOG/t4a.log"`
Expected: the new test and the whole matrix (including every resume twin) pass.

- [ ] **Step 5: Format and commit**

```bash
cargo fmt --all && cargo fmt --all --check
git add crates/gents/src/optimization/driver.rs crates/gents/src/optimization/driver/matrix.rs
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" commit -m "fix(optimization): a run stopped by its cancel marker leaves the job running and undecided (#1515)"
```

### Task 4b: `optimization::references` (rulings F2, F8)

Moved into PR 1 (ruling F8) so PR 2's `eval rm` can refuse a run a live job still needs.

**Files:**
- Create: `crates/gents/src/optimization/references.rs`
- Modify: `crates/gents/src/optimization/mod.rs` (`pub mod references;` between `pub mod proposer;` and `pub mod show;`, and a `pub use references::{…};` line)

**Interfaces:**
- Consumes (M6b, at `<M4-BASE>`):
  ```rust
  pub async fn load_job(access: &ConfigAccess, owner: &str, job_id: &str) -> Result<Option<JobRecord>>;
  pub fn derive_state(journal: &[JournalEntry]) -> JobState;
  pub enum JobState { Running, ReadyToPromote, NothingToPromote, Exhausted, Failed { reason: String }, Promoted, Stale, Reverted }   // Clone, PartialEq, Debug
  pub enum JournalEntry { RunStarted { run_id: String, round: Option<u32>, split: EvalSplit }, Decided { round, attempt, run_ids: Vec<String>, mode,
      decision, policy_version: String, summary: DecisionSummary }, Frozen, Reverted { by: String }, .. }
  // the OptimizationJob collection's owner field is `owner_agent_did` (job.rs `filter`)
  // crates/gents/src/optimization/driver/matrix.rs (#[cfg(test)] pub(crate))
  pub(crate) async fn accepting_harness(job_id: &str) -> (Harness, JobRequest);   // Harness::access(&self) -> &ConfigAccess; the job ends ReadyToPromote
  ```
- Produces (re-exported from `gents::optimization`):
  ```rust
  pub fn journal_run_ids(journal: &[JournalEntry]) -> BTreeSet<String>;
  pub async fn job_ids(access: &ConfigAccess, owner: &str) -> Result<Vec<String>>;
  pub async fn referenced_run_ids(access: &ConfigAccess, owner: &str) -> Result<BTreeSet<String>>;
  pub fn removable(state: &JobState) -> bool;   // NothingToPromote | Exhausted | Failed | Stale | Promoted | Reverted
  pub async fn held_runs(access: &ConfigAccess, owner: &str) -> Result<BTreeMap<String, (String, JobState)>>;   // run_id -> (job_id, state) of non-removable jobs
  ```

- [ ] **Step 1: Write the failing tests**

Create `crates/gents/src/optimization/references.rs` with only:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::document_config::EvalSplit;
    use crate::eval::runner::freeze::tests::OWNER;
    use crate::optimization::driver::matrix::accepting_harness;
    use crate::optimization::job::{load_job, DecisionSummary, JobState};
    use crate::optimization::policy::{Decision, Mode};

    #[test]
    fn a_journal_names_every_run_it_started_or_decided_on() {
        let journal = vec![
            JournalEntry::Frozen,
            JournalEntry::RunStarted {
                run_id: "train".into(),
                round: Some(1),
                split: EvalSplit::Train,
            },
            JournalEntry::Decided {
                round: Some(1),
                attempt: 0,
                run_ids: vec!["val-a".into(), "val-b".into()],
                mode: Mode::Improve,
                decision: Decision::Accept,
                policy_version: "v2".into(),
                summary: DecisionSummary {
                    improved: 0,
                    tied: 0,
                    worsened: 0,
                    mean_diff_bp: None,
                    p_ppm: None,
                    alpha_effective_ppm: 0,
                    cost_skipped: true,
                },
            },
            JournalEntry::Reverted {
                by: "did:key:owner".into(),
            },
        ];
        assert_eq!(
            journal_run_ids(&journal),
            BTreeSet::from(["train".to_owned(), "val-a".to_owned(), "val-b".to_owned()])
        );
    }

    #[tokio::test]
    async fn every_run_of_an_owners_jobs_is_referenced() {
        let (harness, request) = accepting_harness("refs").await;
        let referenced = referenced_run_ids(harness.access(), OWNER).await.unwrap();
        let job = load_job(harness.access(), OWNER, &request.job_id)
            .await
            .unwrap()
            .unwrap();
        assert!(!referenced.is_empty());
        assert_eq!(referenced, journal_run_ids(&job.journal));
        assert_eq!(job_ids(harness.access(), OWNER).await.unwrap(), vec!["refs".to_owned()]);
        assert!(referenced_run_ids(harness.access(), "did:key:someone-else")
            .await
            .unwrap()
            .is_empty());
    }

    #[test]
    fn removable_states_are_the_settled_ones_that_need_nothing_more() {
        let failed = JobState::Failed {
            reason: "held_out_regression".into(),
        };
        for state in [
            JobState::NothingToPromote,
            JobState::Exhausted,
            failed,
            JobState::Stale,
            JobState::Promoted,
            JobState::Reverted,
        ] {
            assert!(removable(&state), "{state:?}");
        }
        assert!(!removable(&JobState::Running));
        assert!(
            !removable(&JobState::ReadyToPromote),
            "promote rebuilds the checkpoint from the job's directory"
        );
    }

    #[tokio::test]
    async fn a_ready_jobs_runs_are_held_and_a_foreign_owner_holds_none() {
        let (harness, request) = accepting_harness("held").await;
        let held = held_runs(harness.access(), OWNER).await.unwrap();
        let job = load_job(harness.access(), OWNER, &request.job_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            held.keys().cloned().collect::<BTreeSet<_>>(),
            journal_run_ids(&job.journal)
        );
        assert!(held
            .values()
            .all(|(job_id, state)| job_id == "held" && *state == JobState::ReadyToPromote));
        assert!(held_runs(harness.access(), "did:key:someone-else")
            .await
            .unwrap()
            .is_empty());
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents --lib optimization::references > "$LOG/t4b.log" 2>&1; grep -E "error(\[|:)|test result" "$LOG/t4b.log" | head` (after adding `pub mod references;` to `optimization/mod.rs`)
Expected: compile errors: `journal_run_ids`, `referenced_run_ids`, `job_ids`, `removable`, `held_runs` not found.

- [ ] **Step 3: Write the implementation**

Insert above the test module in `crates/gents/src/optimization/references.rs`:

```rust
//! Which eval runs an optimization job's journal names: its train,
//! validation, re-run and held-out runs. `gents eval gc` keeps them (spec 4b
//! §7), so a job's evidence and M5's calibration never lose their homes to
//! a side effect.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;

use crate::config_client::ConfigAccess;
use crate::graphql::escape_graphql_string;
use crate::optimization::job::{derive_state, load_job, JobState, JournalEntry};

/// Every run `journal` started and every run one of its decisions read.
pub fn journal_run_ids(journal: &[JournalEntry]) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    for entry in journal {
        match entry {
            JournalEntry::RunStarted { run_id, .. } => {
                ids.insert(run_id.clone());
            }
            JournalEntry::Decided { run_ids, .. } => ids.extend(run_ids.iter().cloned()),
            _ => {}
        }
    }
    ids
}

/// The ids of every job `owner` has, sorted.
pub async fn job_ids(access: &ConfigAccess, owner: &str) -> Result<Vec<String>> {
    let query = format!(
        r#"{{ OptimizationJob(filter: {{ owner_agent_did: {{ _eq: "{owner}" }} }}) {{ job_id }} }}"#,
        owner = escape_graphql_string(owner),
    );
    let response = access
        .transact("optimization.job_ids", |txn| {
            let query = &query;
            Box::pin(async move { txn.execute(query).await })
        })
        .await?;
    let mut ids: Vec<String> = response["data"]["OptimizationJob"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|row| row["job_id"].as_str().map(str::to_owned))
        .collect();
    ids.sort();
    ids.dedup();
    Ok(ids)
}

/// Every run any of `owner`'s jobs names.
pub async fn referenced_run_ids(access: &ConfigAccess, owner: &str) -> Result<BTreeSet<String>> {
    let mut ids = BTreeSet::new();
    for job_id in job_ids(access, owner).await? {
        if let Some(job) = load_job(access, owner, &job_id).await? {
            ids.extend(journal_run_ids(&job.journal));
        }
    }
    Ok(ids)
}

/// Ruling F2: a job whose directory, and the directories of whose runs, may
/// go without `--force`. `ReadyToPromote` is not: `promote` rebuilds the
/// checkpoint from the baseline copy in the job's directory.
pub fn removable(state: &JobState) -> bool {
    matches!(
        state,
        JobState::NothingToPromote
            | JobState::Exhausted
            | JobState::Failed { .. }
            | JobState::Stale
            | JobState::Promoted
            | JobState::Reverted
    )
}

/// Ruling F8: every run a job that is not [`removable`] still names, with
/// that job's id and state. `gents eval rm` refuses these without `--force`.
pub async fn held_runs(
    access: &ConfigAccess,
    owner: &str,
) -> Result<BTreeMap<String, (String, JobState)>> {
    let mut held = BTreeMap::new();
    for job_id in job_ids(access, owner).await? {
        let Some(job) = load_job(access, owner, &job_id).await? else {
            continue;
        };
        let state = derive_state(&job.journal);
        if removable(&state) {
            continue;
        }
        for run_id in journal_run_ids(&job.journal) {
            held.entry(run_id)
                .or_insert_with(|| (job.job_id.clone(), state.clone()));
        }
    }
    Ok(held)
}
```

In `crates/gents/src/optimization/mod.rs` add `pub mod references;` between `pub mod proposer;` and `pub mod show;`, and after the `pub use proposer::{…};` line:

```rust
pub use references::{held_runs, job_ids, journal_run_ids, referenced_run_ids, removable};
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents --lib optimization::references > "$LOG/t4b.log" 2>&1; grep -E "^test |test result|panicked" "$LOG/t4b.log"`
Expected: four tests pass.

- [ ] **Step 5: Format and commit**

```bash
cargo fmt --all && cargo fmt --all --check
git add crates/gents/src/optimization/
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" commit -m "feat(optimization): which runs a job names, and which jobs still hold them (#1515)"
```

### Task 5: Abandoned attempts spend no infrastructure retry (spec §9)

**Files:**
- Modify: `crates/gents/src/eval/runner/plan.rs` (doc of `plan` at lines 97-102, loop body at lines 116-142, tests)
- Modify: `crates/gents/src/eval/runner/mod.rs` (tests module)
- Modify: `crates/gents/src/optimization/driver/matrix.rs` (module doc lines 19-25; the `CancellingExecutor` doc comment)

**Interfaces:**
- Consumes:
  ```rust
  pub fn plan(origin: &RunOrigin, run_id: &str, existing: &[TrialRecord], max_infra_retries: u32) -> Vec<PlannedTrial>;
  pub fn completion_is_not_evidence(completion: &TrialCompletion) -> bool;
  // plan.rs tests: fn origin(cells, cases, trials_per_case) -> RunOrigin; fn record(cell, case, index, attempt, completed: bool) -> TrialRecord;
  //                fn no_evidence(cell, case, attempt) -> TrialRecord  (index 0)
  // mod.rs tests: struct CancelOnFirstExecute { run: CancellationToken }  — cancels the run on its first execute
  ```
- Produces:
  ```rust
  pub const MAX_ABANDONED_ATTEMPTS: u32 = 10;   // in plan.rs, re-exported from eval::runner
  ```
  Rule: a slot is planned again while its latest completed attempt is not evidence (or it has none), its not-evidence completions number at most `max_infra_retries`, and its abandoned (null-completion) attempts number fewer than `MAX_ABANDONED_ATTEMPTS`. The next attempt is one past the highest attempt number.

- [ ] **Step 1: Write the failing tests**

Append to `mod tests` in `crates/gents/src/eval/runner/plan.rs`:

```rust
    /// Spec 4b §9 (M6b ruling T35-1): the retry cap counts attempts that
    /// finished without evidence. An abandoned attempt, a crash or a cancel,
    /// is not a verdict on the provider and is bounded separately.
    #[test]
    fn abandoned_attempts_do_not_spend_infrastructure_retries() {
        let origin = origin(&["base"], &["a-case"], 1);
        let abandoned = |attempt: u32| record("base", "a-case", 0, attempt, false);
        let cases: Vec<(&str, Vec<TrialRecord>, u32, Option<u32>)> = vec![
            (
                "a cancelled first attempt is planned again with no retries",
                vec![abandoned(1)],
                0,
                Some(2),
            ),
            (
                "an attempt that finished without evidence spends the only try",
                vec![no_evidence("base", "a-case", 1)],
                0,
                None,
            ),
            (
                "abandoned attempts around a no-evidence one leave the cap unspent",
                vec![
                    abandoned(1),
                    no_evidence("base", "a-case", 2),
                    abandoned(3),
                ],
                1,
                Some(4),
            ),
            (
                "the cap counts no-evidence completions only",
                vec![
                    abandoned(1),
                    no_evidence("base", "a-case", 2),
                    no_evidence("base", "a-case", 3),
                ],
                1,
                None,
            ),
            (
                "nine abandoned attempts still plan a tenth",
                (1..=9).map(abandoned).collect(),
                0,
                Some(10),
            ),
            (
                "ten abandoned attempts stop the slot",
                (1..=10).map(abandoned).collect(),
                0,
                None,
            ),
            (
                "a completed attempt ends the slot however many were abandoned",
                (1..=10)
                    .map(abandoned)
                    .chain(std::iter::once(record("base", "a-case", 0, 11, true)))
                    .collect(),
                0,
                None,
            ),
        ];
        for (why, existing, max_infra_retries, expected) in cases {
            let next = plan(&origin, "r", &existing, max_infra_retries)
                .first()
                .map(|planned| planned.attempt);
            assert_eq!(next, expected, "{why}");
        }
        assert_eq!(MAX_ABANDONED_ATTEMPTS, 10);
    }
```

In the existing test `a_slot_is_replanned_while_its_latest_completed_attempt_is_not_evidence`, replace the message `"an abandoned row and a finished one that learned nothing both count as attempts"` with `"an abandoned row takes an attempt number but spends no retry"` (the asserted value, `Some(3)`, is unchanged).

Append to `mod tests` in `crates/gents/src/eval/runner/mod.rs`:

```rust
    /// Spec 4b §9: before the fix, a slot cancelled mid-trial under
    /// `max_infra_retries: 0` was never planned again and its pair was lost.
    #[tokio::test]
    async fn a_cancelled_slot_is_planned_again_under_no_infrastructure_retries() {
        let (launching, pack) = launching("captured_rows_count").await;
        let mut request = request(&launching, &pack, "run-abandoned");
        one_slot(&mut request);
        request.max_infra_retries = 0;
        let cancel = CancellationToken::new();
        let outcome = run(
            &launching.access,
            &request,
            &CancelOnFirstExecute {
                run: cancel.clone(),
            },
            &CheckRegistry::builtin(),
            cancel,
            &options(),
        )
        .await
        .unwrap();
        assert_eq!((outcome.completed, outcome.abandoned), (0, 1));

        let resumed = resume(
            &launching.access,
            OWNER,
            "run-abandoned",
            &launching.runs_dir(),
            &ScriptedExecutor::new().with_default(passed()),
            &CheckRegistry::builtin(),
            CancellationToken::new(),
            &options(),
        )
        .await
        .unwrap();
        assert_eq!(resumed.completed, 1, "the abandoned attempt spent no retry");
        let mut attempts: Vec<(u32, bool)> = load_trials(&launching.access, OWNER, "run-abandoned")
            .await
            .unwrap()
            .iter()
            .map(|trial| (trial.identity.attempt, trial.completion.is_some()))
            .collect();
        attempts.sort();
        assert_eq!(attempts, vec![(1, false), (2, true)]);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents --lib eval::runner > "$LOG/t5.log" 2>&1; grep -E "error(\[|:)|FAILED|panicked|test result" "$LOG/t5.log" | head -20`
Expected: a compile error `cannot find value `MAX_ABANDONED_ATTEMPTS``. Comment out its one `assert_eq!` line temporarily and rerun: `abandoned_attempts_do_not_spend_infrastructure_retries` fails on "a cancelled first attempt is planned again with no retries" (`None` vs `Some(2)`), and `a_cancelled_slot_is_planned_again_under_no_infrastructure_retries` fails on `resumed.completed` (0 vs 1). Restore the line.

- [ ] **Step 3: Write the implementation**

In `crates/gents/src/eval/runner/plan.rs`, insert above `pub fn plan(`:

```rust
/// How many abandoned attempts (a null completion: the host crashed or the
/// run was cancelled mid-trial) a slot may accumulate before it is planned no
/// more. Abandonment is not a verdict on the subject or the provider, so it
/// spends no `max_infra_retries`; this bound only stops a trial that kills
/// its host every time from looping forever (spec 4b §9, ruling T35-1).
pub const MAX_ABANDONED_ATTEMPTS: u32 = 10;
```

Replace the doc comment of `plan` (the paragraph starting "A slot is done once an attempt finished with evidence") with:

```rust
/// The trials `run_id` still owes, given what it has already written.
///
/// A slot is done once an attempt finished with evidence about the subject.
/// A slot whose latest completed attempt is not evidence, and a slot whose
/// attempts were all abandoned, are planned again at one past their highest
/// attempt, so the new row never collides with an old one. Two bounds stop
/// that: `max_infra_retries + 1` attempts that finished without evidence, and
/// [`MAX_ABANDONED_ATTEMPTS`] attempts that never finished. An abandoned
/// attempt counts only toward the second, so a cancel never costs a slot its
/// pair.
```

Replace the per-slot body, from `let mut highest = None;` through the `if attempt > max_infra_retries.saturating_add(1) { continue; }` block, with:

```rust
                let mut highest = None;
                let mut abandoned = 0u32;
                let mut not_evidence = 0u32;
                let mut latest_completed: Option<(u32, &TrialCompletion)> = None;
                for record in attempts {
                    highest = highest.max(Some(record.identity.attempt));
                    let Some(completion) = &record.completion else {
                        abandoned += 1;
                        continue;
                    };
                    if completion_is_not_evidence(completion) {
                        not_evidence += 1;
                    }
                    if latest_completed
                        .is_none_or(|(attempt, _)| record.identity.attempt >= attempt)
                    {
                        latest_completed = Some((record.identity.attempt, completion));
                    }
                }
                if latest_completed
                    .is_some_and(|(_, completion)| !completion_is_not_evidence(completion))
                {
                    continue;
                }
                if not_evidence > max_infra_retries || abandoned >= MAX_ABANDONED_ATTEMPTS {
                    continue;
                }
                let attempt = highest.map_or(1, |attempt| attempt + 1);
```

In `crates/gents/src/eval/runner/mod.rs`, change `pub use plan::{completion_is_not_evidence, not_evidence_slots, plan, trial_id_for, PlannedTrial};` to

```rust
pub use plan::{
    completion_is_not_evidence, not_evidence_slots, plan, trial_id_for, PlannedTrial,
    MAX_ABANDONED_ATTEMPTS,
};
```

In `crates/gents/src/optimization/driver/matrix.rs`, replace the module-doc lines

```rust
//! and undecided: the validation run, the re-run, the held-out run. The runner
//! abandons that trial and plans its slot again only under an infrastructure
//! retry, so those twins run with `max_infra_retries: 1`. Under
//! `max_infra_retries: 0` an abandoned slot is never planned again, and its
//! pair is lost to the resume.
```

with

```rust
//! and undecided: the validation run, the re-run, the held-out run. The runner
//! abandons that trial and plans its slot again at the next attempt; since
//! spec 4b §9 an abandoned attempt spends no infrastructure retry, so a
//! cancelled slot keeps its pair under any `max_infra_retries`. These twins
//! run with `max_infra_retries: 1` and script attempt 2 as attempt 1.
```

and in the doc comment of `struct CancellingExecutor` (lines 250-254), replace

```rust
/// skipped, and the run is left open with no decision. The abandoned slot is
/// planned again only if the run allows an infrastructure retry, so the twins
/// that use this set `max_infra_retries: 1` and script attempt 2 as attempt 1.
```

with

```rust
/// skipped, and the run is left open with no decision. The abandoned slot is
/// planned again at attempt 2, which spends no infrastructure retry (spec 4b
/// §9); the twins that use this script attempt 2 as attempt 1.
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents --lib eval::runner > "$LOG/t5.log" 2>&1; grep -E "^test |test result|panicked" "$LOG/t5.log" | tail -40`
Expected: all runner tests pass, including the two new ones.

Then: `CARGO_BUILD_JOBS=4 cargo test -p gents --lib optimization > "$LOG/t5-opt.log" 2>&1; grep -E "test result|FAILED|panicked" "$LOG/t5-opt.log" | tail -20`
Expected: the matrix and every optimization test still pass (the twins' scripts already cover attempt 2).

- [ ] **Step 5: Format and commit**

```bash
cargo fmt --all && cargo fmt --all --check
git add crates/gents/src/eval/runner/plan.rs crates/gents/src/eval/runner/mod.rs crates/gents/src/optimization/driver/matrix.rs
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" commit -m "fix(eval): an abandoned attempt spends no infrastructure retry; ten abandonments stop a slot (#1515)"
```

### Task 6: `<run dir>/progress.json`

**Files:**
- Create: `crates/gents/src/eval/runner/progress.rs`
- Modify: `crates/gents/src/eval/runner/mod.rs` (`pub mod progress;`, `pub use progress::{…}`, `execute_frozen`, `execute_trial`, `trial_spec`, tests)
- Modify: `crates/gents/src/eval/runner/executor.rs` (`TrialSpec` gains `progress`; `empty_for_tests` sets it)
- Modify: `crates/gents/src/eval/runner/embedded/executor.rs:664-681` (`run_stages` reports stage start and end)

**Interfaces:**
- Consumes:
  ```rust
  // crates/gents/src/eval/runner/executor.rs
  #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
  pub struct TrialSpec { pub trial_id, pub pack_dir, pub pack_digest, pub behavior_id, pub inference, pub fixtures,
      pub stages: Vec<StageSpec>, pub captures, pub home_dir, pub script_key: Option<ScriptKey> }
  // Every TrialSpec literal outside mod.rs's `trial_spec` uses `..TrialSpec::empty_for_tests(..)`
  // (embedded/executor.rs tests at lines 1417, 1457, 1572, 1625), so only two constructors change.
  // crates/gents/src/eval/runner/mod.rs
  async fn execute_trial(frozen: &FrozenRun, planned: &PlannedTrial, recorder: &dyn Recorder, executor: &dyn TrialExecutor,
      registry: &CheckRegistry, cancel: &CancellationToken, stop: &AtomicBool) -> Result<Slot>;
  pub struct PlannedTrial { pub trial_id, pub cell_id, pub cell_label, pub case_id, pub trial_index: u32, pub attempt: u32, pub seed: i64 }
  ```
- Produces:
  ```rust
  pub const PROGRESS_FILE: &str = "progress.json";
  #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
  pub struct InFlight { pub cell_id: String, pub case_id: String, pub trial_index: u32, pub attempt: u32,
      pub stage_id: Option<String>, pub started_at: String, pub pid: u32, pub written_at: String }
  #[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
  pub struct Progress { pub slots: BTreeMap<String, InFlight> }       // keyed by trial id
  pub fn read_progress(run_dir: &Path) -> Option<Progress>;             // None when absent or unreadable
  pub fn host_alive(pid: u32) -> bool;                                   // ruling F3: libc::kill(pid, 0) on unix; true elsewhere
  pub fn is_fresh(slot: &InFlight, window: std::time::Duration) -> bool; // host alive and written_at within `window`
  #[derive(Clone, Default)] pub struct StageProgress { .. }              // Debug; PartialEq/Eq always equal
  impl StageProgress { pub fn stage_started(&self, stage_id: &str); pub fn stage_ended(&self, stage_id: &str) }
  // TrialSpec gains: #[serde(skip)] pub progress: StageProgress
  ```
  File contract (spec 4b §6): written atomically (temp file in the run directory, then rename) when a slot starts, at each stage start and end, and when the slot ends; the ended slot's entry is removed. Created by every pass of `run` and `resume` (a new pass starts from an empty file, which also clears a crashed process's entries). Never read by the runner, never evidence, never a digest input. A failed write is a `tracing::warn!`, never an error.

- [ ] **Step 1: Write the failing tests**

Create `crates/gents/src/eval/runner/progress.rs` holding only:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn in_flight(stage_id: Option<&str>) -> InFlight {
        InFlight {
            cell_id: "base".into(),
            case_id: "case-a".into(),
            trial_index: 0,
            attempt: 1,
            stage_id: stage_id.map(str::to_owned),
            started_at: "2026-09-22T00:00:00Z".into(),
            pid: 0,
            written_at: String::new(),
        }
    }

    #[test]
    fn a_writer_records_slots_and_stages_and_forgets_ended_slots() {
        let dir = tempfile::tempdir().unwrap();
        let writer = ProgressWriter::new(dir.path());
        assert_eq!(read_progress(dir.path()), Some(Progress::default()));

        writer.slot_started("t1", in_flight(None));
        let started = read_progress(dir.path()).unwrap().slots["t1"].clone();
        assert_eq!(started.pid, std::process::id(), "slot_started stamps the host");
        assert!(!started.written_at.is_empty());
        let stages = StageProgress::for_trial(&writer, "t1");
        stages.stage_started("check");
        let during = read_progress(dir.path()).unwrap();
        assert_eq!(during.slots["t1"].stage_id.as_deref(), Some("check"));

        stages.stage_ended("check");
        assert_eq!(read_progress(dir.path()).unwrap().slots["t1"].stage_id, None);

        writer.slot_ended("t1");
        assert!(read_progress(dir.path()).unwrap().slots.is_empty());
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(leftovers, vec![std::ffi::OsString::from(PROGRESS_FILE)]);
    }

    #[test]
    fn a_default_handle_reports_nothing_and_is_equal_to_any_other() {
        let dir = tempfile::tempdir().unwrap();
        let writer = ProgressWriter::new(dir.path());
        StageProgress::default().stage_started("check");
        assert_eq!(read_progress(dir.path()), Some(Progress::default()));
        assert_eq!(StageProgress::default(), StageProgress::for_trial(&writer, "t1"));
        assert_eq!(read_progress(&dir.path().join("absent")), None);
    }

    #[test]
    fn this_process_is_alive_and_an_impossible_pid_is_not() {
        assert!(host_alive(std::process::id()));
        assert!(!host_alive(0));
        assert!(!host_alive(u32::MAX), "never kill(-1, 0)");
        let mut slot = in_flight(None);
        slot.pid = std::process::id();
        slot.written_at = now_millis();
        assert!(is_fresh(&slot, Duration::from_secs(3)));
        slot.written_at = "2026-01-01T00:00:00.000Z".into();
        assert!(!is_fresh(&slot, Duration::from_secs(3)), "too old");
        slot.written_at = "not a time".into();
        assert!(!is_fresh(&slot, Duration::from_secs(3)));
    }

    #[test]
    fn a_heartbeat_writes_at_most_once_per_gap() {
        let dir = tempfile::tempdir().unwrap();
        let writer = ProgressWriter::new(dir.path());
        writer.slot_started("t1", in_flight(None));
        let written = || read_progress(dir.path()).unwrap().slots["t1"].written_at.clone();
        writer.heartbeat(Duration::from_secs(60));
        let first = written();
        std::thread::sleep(Duration::from_millis(5));
        writer.heartbeat(Duration::from_secs(60));
        assert_eq!(written(), first, "inside the gap nothing is written");
        writer.heartbeat(Duration::ZERO);
        assert!(written() > first);
    }
}
```

Append inside `mod tests` in `crates/gents/src/eval/runner/mod.rs`:

```rust
    /// Reports its one stage the way the embedded executor does and records
    /// what `progress.json` said while the trial was inside it.
    struct StageReporting {
        run_dir: PathBuf,
        seen: Mutex<Vec<Option<Progress>>>,
    }

    #[async_trait::async_trait]
    impl TrialExecutor for StageReporting {
        fn isolation(&self) -> Isolation {
            Isolation::Embedded
        }

        async fn provision(&self, _spec: &TrialSpec) -> TrialLocator {
            TrialLocator {
                trial_agent_did: "did:key:trial".into(),
                session_id: "session".into(),
                home_hint: None,
            }
        }

        async fn execute(&self, spec: &TrialSpec, _cancel: CancellationToken) -> TrialEvidence {
            spec.progress.stage_started("check");
            let during = read_progress(&self.run_dir);
            spec.progress.stage_ended("check");
            self.seen.lock().unwrap().push(during);
            passed()
        }

        async fn recollect(
            &self,
            _at: &TrialLocator,
            _captures: &[Capture],
        ) -> Option<TrialEvidence> {
            None
        }
    }

    #[tokio::test]
    async fn progress_names_the_stage_an_in_flight_slot_is_in_and_forgets_it_at_the_end() {
        let (launching, pack) = launching("captured_rows_count").await;
        let mut request = request(&launching, &pack, "run-progress");
        one_slot(&mut request);
        let run_dir = launching.runs_dir().join("run-progress");
        let executor = StageReporting {
            run_dir: run_dir.clone(),
            seen: Mutex::new(Vec::new()),
        };
        run(
            &launching.access,
            &request,
            &executor,
            &CheckRegistry::builtin(),
            CancellationToken::new(),
            &options(),
        )
        .await
        .unwrap();

        let seen = executor.seen.lock().unwrap().clone();
        let during = seen[0]
            .as_ref()
            .expect("progress.json exists while the trial runs");
        let (trial_id, slot) = during.slots.iter().next().expect("one slot in flight");
        assert_eq!(trial_id, &trial_id_for("run-progress", "base", "case-a", 0, 1));
        assert_eq!(
            (
                slot.cell_id.as_str(),
                slot.case_id.as_str(),
                slot.trial_index,
                slot.attempt,
                slot.stage_id.as_deref()
            ),
            ("base", "case-a", 0, 1, Some("check"))
        );
        let after = read_progress(&run_dir).expect("the file stays after the run");
        assert!(after.slots.is_empty(), "{after:?}");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents --lib eval::runner > "$LOG/t6.log" 2>&1; grep -E "error(\[|:)|test result" "$LOG/t6.log" | head -20`
Expected: compile errors: `ProgressWriter`, `read_progress`, `Progress`, `StageProgress` not found; no field `progress` on `TrialSpec`.

- [ ] **Step 3: Write the implementation**

Insert above the test module in `crates/gents/src/eval/runner/progress.rs`:

```rust
//! `<run dir>/progress.json`: which slots are in flight and at which stage
//! (spec 4b §6). Ephemeral and advisory: the loop and, through
//! [`StageProgress`], an executor write it; only `gents eval watch` reads it.
//! It is never evidence, never an anchor or digest input and never a
//! document. Every write replaces the file atomically, so a reader sees one
//! whole state or the previous one.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const PROGRESS_FILE: &str = "progress.json";

/// One slot a process is running now.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InFlight {
    pub cell_id: String,
    pub case_id: String,
    pub trial_index: u32,
    pub attempt: u32,
    /// The stage the trial is in; `None` before its first stage and between
    /// stages.
    pub stage_id: Option<String>,
    /// RFC 3339: when the slot, or its current stage, started.
    pub started_at: String,
    /// The process hosting the slot (ruling U4).
    pub pid: u32,
    /// RFC 3339 with milliseconds: when this entry was last rewritten. The
    /// loop refreshes it on its marker timer (Task 6a), so an entry that stops
    /// being refreshed belongs to a process that stopped.
    pub written_at: String,
}

/// The whole file: the in-flight slots, keyed by trial id.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Progress {
    pub slots: BTreeMap<String, InFlight>,
}

/// The file under `run_dir`, or `None` when it is absent or unreadable; the
/// watcher then shows finished slots only.
pub fn read_progress(run_dir: &Path) -> Option<Progress> {
    let bytes = std::fs::read(run_dir.join(PROGRESS_FILE)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Ruling F3: whether process `pid` still exists. Signal 0 delivers nothing
/// and only checks; `EPERM` means it exists under another user.
#[cfg(unix)]
pub fn host_alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    if pid <= 0 {
        return false;
    }
    // SAFETY: `kill` with signal 0 sends no signal; it only checks `pid`.
    let result = unsafe { libc::kill(pid, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Without a portable liveness check, age alone decides (ruling F3).
#[cfg(not(unix))]
pub fn host_alive(_pid: u32) -> bool {
    true
}

/// An entry a live process refreshed within `window` (rulings U4, F3, F7).
/// An unreadable `written_at` is not fresh; one from the future is.
pub fn is_fresh(slot: &InFlight, window: Duration) -> bool {
    let Ok(written) = chrono::DateTime::parse_from_rfc3339(&slot.written_at) else {
        return false;
    };
    let age = chrono::Utc::now().signed_duration_since(written.with_timezone(&chrono::Utc));
    host_alive(slot.pid) && age.to_std().map_or(true, |age| age <= window)
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn now_millis() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// The loop's writer: one per pass over a run, shared by its trials.
#[derive(Debug)]
pub(crate) struct ProgressWriter {
    path: PathBuf,
    state: Mutex<Progress>,
    last_heartbeat: Mutex<Option<Instant>>,
}

impl ProgressWriter {
    /// Start the pass with an empty file.
    pub(crate) fn new(run_dir: &Path) -> Arc<Self> {
        let writer = Arc::new(Self {
            path: run_dir.join(PROGRESS_FILE),
            state: Mutex::new(Progress::default()),
            last_heartbeat: Mutex::new(None),
        });
        writer.update(|_| {});
        writer
    }

    /// Record a slot this process now hosts; stamps `pid` and `written_at`.
    pub(crate) fn slot_started(&self, trial_id: &str, mut in_flight: InFlight) {
        in_flight.pid = std::process::id();
        in_flight.written_at = now_millis();
        self.update(|progress| {
            progress.slots.insert(trial_id.to_owned(), in_flight);
        });
    }

    /// Refresh every entry's `written_at`: the loop is alive and its slots
    /// are still in flight. Called on the loop's marker timer (Task 6a); it
    /// writes at most once per `min_gap` (ruling F16).
    pub(crate) fn heartbeat(&self, min_gap: Duration) {
        {
            let mut last = self
                .last_heartbeat
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if last.is_some_and(|at| at.elapsed() < min_gap) {
                return;
            }
            *last = Some(Instant::now());
        }
        let idle = self
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .slots
            .is_empty();
        if idle {
            return;
        }
        self.update(|progress| {
            let written_at = now_millis();
            for slot in progress.slots.values_mut() {
                slot.written_at = written_at.clone();
            }
        });
    }

    pub(crate) fn slot_ended(&self, trial_id: &str) {
        self.update(|progress| {
            progress.slots.remove(trial_id);
        });
    }

    fn stage(&self, trial_id: &str, stage_id: Option<&str>) {
        self.update(|progress| {
            if let Some(slot) = progress.slots.get_mut(trial_id) {
                slot.stage_id = stage_id.map(str::to_owned);
                slot.started_at = now();
                slot.written_at = now_millis();
            }
        });
    }

    /// Apply `change` and rewrite the file while holding the lock, so two
    /// trials' writes never interleave.
    fn update(&self, change: impl FnOnce(&mut Progress)) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        change(&mut state);
        if let Err(error) = write_atomically(&self.path, &state) {
            tracing::warn!(
                error = %format!("{error:#}"),
                path = %self.path.display(),
                "eval run progress was not recorded"
            );
        }
    }
}

fn write_atomically(path: &Path, progress: &Progress) -> Result<()> {
    let dir = path
        .parent()
        .context("progress.json has a run directory")?;
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let mut staged = tempfile::NamedTempFile::new_in(dir)
        .with_context(|| format!("staging {}", path.display()))?;
    serde_json::to_writer_pretty(&mut staged, progress).context("encoding progress")?;
    staged
        .persist(path)
        .with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

/// What an executor holds to report stage boundaries. The default reports
/// nothing, so executors and specs built outside the loop need no writer.
#[derive(Clone, Default)]
pub struct StageProgress {
    sink: Option<(Arc<ProgressWriter>, String)>,
}

impl StageProgress {
    pub(crate) fn for_trial(writer: &Arc<ProgressWriter>, trial_id: &str) -> Self {
        Self {
            sink: Some((writer.clone(), trial_id.to_owned())),
        }
    }

    pub fn stage_started(&self, stage_id: &str) {
        if let Some((writer, trial_id)) = &self.sink {
            writer.stage(trial_id, Some(stage_id));
        }
    }

    pub fn stage_ended(&self, _stage_id: &str) {
        if let Some((writer, trial_id)) = &self.sink {
            writer.stage(trial_id, None);
        }
    }
}

impl std::fmt::Debug for StageProgress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StageProgress")
    }
}

/// Reporting is not part of what a trial is: two specs that differ only in
/// where they report are the same spec.
impl PartialEq for StageProgress {
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}

impl Eq for StageProgress {}
```

In `crates/gents/src/eval/runner/executor.rs`:
- add `use crate::eval::runner::progress::StageProgress;` after `use crate::eval::runner::scripted::ScriptKey;`;
- add as the last field of `TrialSpec`:

```rust
    /// Stage boundaries for `<run dir>/progress.json` (spec 4b §6). Not
    /// part of what a trial may know: skipped by serde, ignored by equality,
    /// and a no-op unless the loop attached a writer.
    #[serde(skip)]
    pub progress: StageProgress,
```

- add `progress: StageProgress::default(),` as the last field in `empty_for_tests`.

In `crates/gents/src/eval/runner/embedded/executor.rs`, replace the loop in `run_stages`

```rust
    for stage in &spec.stages {
        let evidence = run_stage(spec, cancel, home, locator, workspace, stage).await;
```

with

```rust
    for stage in &spec.stages {
        spec.progress.stage_started(&stage.stage_id);
        let evidence = run_stage(spec, cancel, home, locator, workspace, stage).await;
        spec.progress.stage_ended(&stage.stage_id);
```

In `crates/gents/src/eval/runner/mod.rs`:
- add `pub mod progress;` between `pub mod plan;` and `pub mod record;`, and after the `pub use plan::{…};` block:

```rust
pub use progress::{
    host_alive, is_fresh, read_progress, InFlight, Progress, StageProgress, PROGRESS_FILE,
};
```

- add `use std::sync::Arc;` beside `use std::sync::atomic::{AtomicBool, Ordering};`, and `use crate::eval::runner::progress::ProgressWriter;` beside `use crate::eval::runner::freeze::thaw;`;
- in `execute_frozen`, after `let mut consecutive = 0u32;` insert

```rust
    // One writer per pass: a resumed run starts from an empty file, which
    // also clears whatever a crashed process left in flight.
    let progress = ProgressWriter::new(&frozen.run_dir);
```

  and change the call `execute_trial(frozen, slot, recorder, executor, registry, &cancel, &stop)` to `execute_trial(frozen, slot, recorder, executor, registry, &cancel, &stop, &progress)`;
- give `execute_trial` a `#[allow(clippy::too_many_arguments)]` attribute and a last parameter `progress: &Arc<ProgressWriter>`; change `let spec = trial_spec(…)?;` to `let mut spec = trial_spec(…)?;`; and right after the `if let Err(error) = recorder.create_trial(owner, &identity).await { … }` block insert

```rust
    progress.slot_started(
        &planned.trial_id,
        InFlight {
            cell_id: planned.cell_id.clone(),
            case_id: planned.case_id.clone(),
            trial_index: planned.trial_index,
            attempt: planned.attempt,
            stage_id: None,
            started_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            // Stamped by `slot_started`.
            pid: 0,
            written_at: String::new(),
        },
    );
    // Removes the entry however this trial leaves: completed, abandoned, or
    // an error writing its rows.
    let _in_flight = InFlightGuard {
        progress,
        trial_id: &planned.trial_id,
    };
    spec.progress = StageProgress::for_trial(progress, &planned.trial_id);
```

- add below `execute_trial`:

```rust
/// Ends a slot's `progress.json` entry when the trial's turn ends.
struct InFlightGuard<'a> {
    progress: &'a ProgressWriter,
    trial_id: &'a str,
}

impl Drop for InFlightGuard<'_> {
    fn drop(&mut self) {
        self.progress.slot_ended(self.trial_id);
    }
}
```

- in `trial_spec`, add `progress: StageProgress::default(),` as the last field of the `TrialSpec { … }` literal.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents --lib eval::runner > "$LOG/t6.log" 2>&1; grep -E "^test |test result|panicked" "$LOG/t6.log" | tail -50`
Expected: the two `progress::tests` and the new loop test pass; every existing runner and embedded-executor test still passes.

Then the canary, which drives `run_stages` for real: `CARGO_BUILD_JOBS=4 cargo test -p gents --test eval_runner_canary > "$LOG/t6-canary.log" 2>&1; grep -E "test result|panicked" "$LOG/t6-canary.log"`
Expected: `3 passed; 0 failed; 1 ignored`.

- [ ] **Step 5: Format and commit**

```bash
cargo fmt --all && cargo fmt --all --check
git add crates/gents/src/eval/runner/
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" commit -m "feat(eval): the loop keeps an atomic progress.json of in-flight slots and stages (#1515)"
```

### Task 6a: The marker and the heartbeat while a batch is in flight or backing off (rulings U4, U5, F6, F16)

**Files:**
- Modify: `crates/gents/src/eval/runner/mod.rs` (`marker_poll`; the `tokio::select!` in `execute_frozen`; tests)

**Interfaces:**
- Consumes:
  ```rust
  // Task 4
  fn cancel_requested(run_dir: &Path, cancel: &CancellationToken) -> bool;   // cancels `cancel` when the marker is present
  pub fn request_cancel(runs_dir: &Path, run_id: &str) -> Result<PathBuf>;
  // Task 6
  pub(crate) fn ProgressWriter::heartbeat(&self);                           // refreshes `written_at` of every in-flight entry
  pub fn read_progress(run_dir: &Path) -> Option<Progress>;
  // the batch loop in execute_frozen: `tokio::select! { biased; _ = cancel.cancelled() => { … continue; } next = running.next() => next }`
  pub struct RunOptions { pub poll_backoff_base: Duration, pub poll_backoff_cap: Duration }
  ```
- Produces: `fn marker_poll(options: &RunOptions) -> Duration` (the backoff base, capped at one second, at least one millisecond). While a batch is in flight the loop wakes every `marker_poll`, refreshes `progress.json` (at most once per `max(marker_poll, 250 ms)`, ruling F16) and checks the marker; while it waits out a backoff between passes it checks the marker on the same interval (ruling F6); seeing it cancels the child token, so in-flight executors are interrupted mid-request and their trials are abandoned for a resume.

- [ ] **Step 1: Write the failing tests**

Append inside `mod tests` in `crates/gents/src/eval/runner/mod.rs`:

```rust
    /// Writes its run's marker, then waits inside the trial for its own
    /// cancellation, the way a real stage waits on a request.
    struct MarksThenWaits {
        runs_dir: PathBuf,
        run_id: String,
        interrupted: AtomicBool,
    }

    #[async_trait::async_trait]
    impl TrialExecutor for MarksThenWaits {
        fn isolation(&self) -> Isolation {
            Isolation::Embedded
        }

        async fn provision(&self, _spec: &TrialSpec) -> TrialLocator {
            TrialLocator {
                trial_agent_did: "did:key:trial".into(),
                session_id: "session".into(),
                home_hint: None,
            }
        }

        async fn execute(&self, _spec: &TrialSpec, cancel: CancellationToken) -> TrialEvidence {
            request_cancel(&self.runs_dir, &self.run_id).unwrap();
            tokio::select! {
                _ = cancel.cancelled() => self.interrupted.store(true, Ordering::SeqCst),
                _ = tokio::time::sleep(Duration::from_secs(30)) => {}
            }
            passed()
        }

        async fn recollect(
            &self,
            _at: &TrialLocator,
            _captures: &[Capture],
        ) -> Option<TrialEvidence> {
            None
        }
    }

    #[tokio::test]
    async fn a_marker_written_mid_trial_interrupts_the_trial_in_flight() {
        let (launching, pack) = launching("captured_rows_count").await;
        let mut request = request(&launching, &pack, "run-mid");
        one_slot(&mut request);
        let executor = MarksThenWaits {
            runs_dir: launching.runs_dir(),
            run_id: "run-mid".into(),
            interrupted: AtomicBool::new(false),
        };
        let outcome = run(
            &launching.access,
            &request,
            &executor,
            &CheckRegistry::builtin(),
            CancellationToken::new(),
            &options(),
        )
        .await
        .unwrap();
        assert!(
            executor.interrupted.load(Ordering::SeqCst),
            "the trial saw its token cancelled, not its 30 s timeout"
        );
        assert_eq!((outcome.completed, outcome.abandoned), (0, 1));
        let trials = load_trials(&launching.access, OWNER, "run-mid").await.unwrap();
        assert_eq!(trials.len(), 1);
        assert!(trials[0].completion.is_none(), "left open for a resume");
    }

    /// Reads its own `progress.json` entry twice, 600 ms apart: longer than
    /// the 250 ms floor on the heartbeat (ruling F16).
    struct WatchesHeartbeat {
        run_dir: PathBuf,
        seen: Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl TrialExecutor for WatchesHeartbeat {
        fn isolation(&self) -> Isolation {
            Isolation::Embedded
        }

        async fn provision(&self, _spec: &TrialSpec) -> TrialLocator {
            TrialLocator {
                trial_agent_did: "did:key:trial".into(),
                session_id: "session".into(),
                home_hint: None,
            }
        }

        async fn execute(&self, _spec: &TrialSpec, _cancel: CancellationToken) -> TrialEvidence {
            let written_at = || {
                read_progress(&self.run_dir)
                    .and_then(|progress| progress.slots.values().next().cloned())
                    .map(|slot| slot.written_at)
                    .unwrap_or_default()
            };
            let first = written_at();
            tokio::time::sleep(Duration::from_millis(600)).await;
            let second = written_at();
            self.seen.lock().unwrap().extend([first, second]);
            passed()
        }

        async fn recollect(
            &self,
            _at: &TrialLocator,
            _captures: &[Capture],
        ) -> Option<TrialEvidence> {
            None
        }
    }

    /// Ruling F6: a marker written while the loop waits out a 30 s backoff
    /// ends the wait at the next `marker_poll` tick.
    #[tokio::test]
    async fn a_marker_written_during_the_backoff_ends_the_wait() {
        let (launching, pack) = launching("captured_rows_count").await;
        let mut request = request(&launching, &pack, "run-backoff");
        one_slot(&mut request);
        let slow = RunOptions {
            poll_backoff_base: Duration::from_secs(30),
            poll_backoff_cap: Duration::from_secs(30),
        };
        let runs_dir = launching.runs_dir();
        let access = &launching.access;
        // Writes the marker once the slot's first (not-evidence) attempt is
        // recorded, which is when the loop starts its backoff.
        let operator = async {
            loop {
                let trials = load_trials(access, OWNER, "run-backoff").await.unwrap();
                if trials.iter().any(|trial| trial.completion.is_some()) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            request_cancel(&runs_dir, "run-backoff").unwrap();
        };
        let started = std::time::Instant::now();
        let (outcome, ()) = tokio::join!(
            run(
                access,
                &request,
                &ScriptedExecutor::new().with_default(ScriptedExecutor::not_evidence("did:key:trial")),
                &CheckRegistry::builtin(),
                CancellationToken::new(),
                &slow,
            ),
            operator,
        );
        let outcome = outcome.unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the 30 s backoff was cut short: {:?}",
            started.elapsed()
        );
        assert!(outcome.cancelled);
        assert_eq!(outcome.completed, 1);
    }

    #[tokio::test]
    async fn the_loop_refreshes_in_flight_entries_while_a_trial_runs() {
        let (launching, pack) = launching("captured_rows_count").await;
        let mut request = request(&launching, &pack, "run-beat");
        one_slot(&mut request);
        let executor = WatchesHeartbeat {
            run_dir: launching.runs_dir().join("run-beat"),
            seen: Mutex::new(Vec::new()),
        };
        run(
            &launching.access,
            &request,
            &executor,
            &CheckRegistry::builtin(),
            CancellationToken::new(),
            &options(),
        )
        .await
        .unwrap();
        let seen = executor.seen.lock().unwrap().clone();
        assert!(!seen[0].is_empty(), "{seen:?}");
        assert!(seen[1] > seen[0], "written_at advanced while the trial ran: {seen:?}");
        assert_eq!(marker_poll(&options()), Duration::from_millis(1));
        assert_eq!(marker_poll(&RunOptions::default()), Duration::from_secs(1));
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents --lib eval::runner::tests > "$LOG/t6a.log" 2>&1; grep -E "error(\[|:)|FAILED|panicked|test result" "$LOG/t6a.log" | head -20`
Expected: a compile error `cannot find function `marker_poll``. With that assertion commented out, `a_marker_written_mid_trial_interrupts_the_trial_in_flight` fails after its 30 s timeout (`interrupted` is false) and the heartbeat test fails on `seen[1] > seen[0]`. Restore the line.

- [ ] **Step 3: Write the implementation**

In `crates/gents/src/eval/runner/mod.rs`, below `fn clear_cancel`, add:

```rust
/// How often the loop, while a batch is in flight, checks the cancel marker
/// and refreshes `progress.json` (rulings U4, U5): the backoff base, capped at
/// one second and never zero.
fn marker_poll(options: &RunOptions) -> Duration {
    options
        .poll_backoff_base
        .min(Duration::from_secs(1))
        .max(Duration::from_millis(1))
}
```

In `execute_frozen`, replace the wait between passes (ruling F6)

```rust
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = tokio::time::sleep(backoff) => {}
            }
```

with

```rust
            // Ruling F6: the marker is looked for during the wait too.
            let wait = tokio::time::sleep(backoff);
            tokio::pin!(wait);
            let mut watch = tokio::time::interval(marker_poll(options));
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    _ = watch.tick() => {
                        if cancel_requested(&frozen.run_dir, &cancel) {
                            break;
                        }
                    }
                    _ = &mut wait => break,
                }
            }
            if cancel.is_cancelled() {
                break;
            }
```

and replace

```rust
            let mut observed_cancel = false;
            loop {
                let next = if observed_cancel {
                    running.next().await
                } else {
                    tokio::select! {
                        biased;
                        _ = cancel.cancelled() => {
                            observed_cancel = true;
                            stop.store(true, Ordering::Relaxed);
                            continue;
                        }
                        next = running.next() => next,
                    }
                };
```

with

```rust
            let mut observed_cancel = false;
            let mut watch = tokio::time::interval(marker_poll(options));
            watch.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            // Ruling F16: the file is rewritten at most this often.
            let heartbeat_gap = marker_poll(options).max(Duration::from_millis(250));
            loop {
                let next = if observed_cancel {
                    running.next().await
                } else {
                    tokio::select! {
                        biased;
                        _ = cancel.cancelled() => {
                            observed_cancel = true;
                            stop.store(true, Ordering::Relaxed);
                            continue;
                        }
                        _ = watch.tick() => {
                            // The in-flight entries are alive (U4); a marker
                            // written mid-batch cancels the child token, which
                            // interrupts the executors' current requests (U5).
                            progress.heartbeat(heartbeat_gap);
                            cancel_requested(&frozen.run_dir, &cancel);
                            continue;
                        }
                        next = running.next() => next,
                    }
                };
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents --lib eval::runner > "$LOG/t6a.log" 2>&1; grep -E "^test |test result|panicked" "$LOG/t6a.log" | tail -50`
Expected: the three new tests pass (the mid-trial and backoff ones well under ten seconds), and every earlier runner test still passes, including Task 4's `a_cancel_marker_stops_the_loop_at_its_next_launch_and_resume_clears_it` (its scripted trial returns within the same poll that wrote the marker, before any tick can observe it).

Then `CARGO_BUILD_JOBS=4 cargo test -p gents --lib optimization > "$LOG/t6a-opt.log" 2>&1; grep -E "test result|FAILED|panicked" "$LOG/t6a-opt.log"` — the matrix still passes.

- [ ] **Step 5: Format and commit**

```bash
cargo fmt --all && cargo fmt --all --check
git add crates/gents/src/eval/runner/mod.rs
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" commit -m "feat(eval): the loop sees a cancel marker mid-batch and keeps in-flight progress fresh (#1515)"
```

### Task 6b: What "finished" means for `rm` and `gc` (ruling F7)

**Files:**
- Modify: `crates/gents/src/eval/runner/mod.rs` (`slots_owed`, `running_elsewhere`, `run_finished`; imports; a test)

**Interfaces:**
- Consumes:
  ```rust
  pub fn plan(origin: &RunOrigin, run_id: &str, existing: &[TrialRecord], max_infra_retries: u32) -> Vec<PlannedTrial>;   // Task 5's rule
  fn marker_poll(options: &RunOptions) -> Duration;               // Task 6a; one second for RunOptions::default()
  pub fn read_progress(run_dir: &Path) -> Option<Progress>;      // Task 6
  pub fn is_fresh(slot: &InFlight, window: Duration) -> bool;    // Task 6
  pub(crate) struct ProgressWriter; fn new(run_dir) -> Arc<Self>; fn slot_started(&self, trial_id, InFlight)   // Task 6
  ```
- Produces:
  ```rust
  /// How many slots the run still owes, by the planner's own rule.
  pub fn slots_owed(record: &RunRecord, trials: &[TrialRecord]) -> usize;
  /// A live process refreshed one of the run's slots within three runner poll periods.
  pub fn running_elsewhere(run_dir: &Path) -> bool;
  /// Finished for `rm` without `--force` and for `gc`: nothing owed and nothing fresh in flight.
  pub fn run_finished(record: &RunRecord, trials: &[TrialRecord], run_dir: &Path) -> bool;
  ```
  This replaces spec 4b §7's "no Planned or Abandoned slots" (see Deviations, F7): a slot whose not-evidence attempts spent the retry cap owes nothing, and an abandoned slot past the ten-abandonment bound owes nothing, though both show in the report.

- [ ] **Step 1: Write the failing test**

Append inside `mod tests` in `crates/gents/src/eval/runner/mod.rs` (adding `use crate::eval::load_run;`):

```rust
    #[tokio::test]
    async fn a_run_is_finished_when_it_owes_nothing_and_no_live_process_holds_a_slot() {
        let (launching, pack) = launching("captured_rows_count").await;
        let mut request = request(&launching, &pack, "run-fin");
        one_slot(&mut request);
        let frozen = freeze(&launching.access, &request, Isolation::Embedded)
            .await
            .unwrap();
        let record = load_run(&launching.access, OWNER, "run-fin")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(slots_owed(&record, &[]), 1);
        assert!(!run_finished(&record, &[], &frozen.run_dir));

        run(
            &launching.access,
            &request,
            &ScriptedExecutor::new().with_default(passed()),
            &CheckRegistry::builtin(),
            CancellationToken::new(),
            &options(),
        )
        .await
        .unwrap();
        let trials = load_trials(&launching.access, OWNER, "run-fin").await.unwrap();
        assert_eq!(slots_owed(&record, &trials), 0);
        assert!(run_finished(&record, &trials, &frozen.run_dir));

        // This process holds a slot of the run: not finished.
        let writer = ProgressWriter::new(&frozen.run_dir);
        writer.slot_started(
            "held",
            InFlight {
                cell_id: "base".into(),
                case_id: "case-a".into(),
                trial_index: 0,
                attempt: 2,
                stage_id: None,
                started_at: String::new(),
                pid: 0,
                written_at: String::new(),
            },
        );
        assert!(running_elsewhere(&frozen.run_dir));
        assert!(!run_finished(&record, &trials, &frozen.run_dir));

        // The same entry from a process that no longer exists does not hold it.
        let mut progress = read_progress(&frozen.run_dir).unwrap();
        progress.slots.get_mut("held").unwrap().pid = u32::MAX;
        std::fs::write(
            frozen.run_dir.join(PROGRESS_FILE),
            serde_json::to_vec(&progress).unwrap(),
        )
        .unwrap();
        assert!(!running_elsewhere(&frozen.run_dir));
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents --lib eval::runner::tests::a_run_is_finished > "$LOG/t6b.log" 2>&1; grep -E "error(\[|:)|test result" "$LOG/t6b.log" | head`
Expected: compile errors: `slots_owed`, `running_elsewhere`, `run_finished` not found.

- [ ] **Step 3: Write the implementation**

In `crates/gents/src/eval/runner/mod.rs`, add `RunRecord, TrialRecord` to the `use crate::eval::{…}` import, and below `fn marker_poll`:

```rust
/// How many slots the run still owes, by the planner's own rule (ruling
/// F7): a slot that spent its retries or its abandonment bound owes nothing.
pub fn slots_owed(record: &RunRecord, trials: &[TrialRecord]) -> usize {
    plan(
        &record.origin,
        &record.run_id,
        trials,
        record.origin.max_infra_retries,
    )
    .len()
}

/// A live process refreshed one of the run's slots within three runner
/// poll periods (ruling F7).
pub fn running_elsewhere(run_dir: &Path) -> bool {
    let window = marker_poll(&RunOptions::default()) * 3;
    read_progress(run_dir).is_some_and(|progress| {
        progress
            .slots
            .values()
            .any(|slot| is_fresh(slot, window))
    })
}

/// Finished, for `gents eval rm` without `--force` and for `gents eval gc`:
/// the run owes nothing and no live process is running a slot of it.
pub fn run_finished(record: &RunRecord, trials: &[TrialRecord], run_dir: &Path) -> bool {
    slots_owed(record, trials) == 0 && !running_elsewhere(run_dir)
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents --lib eval::runner > "$LOG/t6b.log" 2>&1; grep -E "^test |test result|panicked" "$LOG/t6b.log" | tail -20`
Expected: the new test passes with every runner test.

- [ ] **Step 5: Format and commit**

```bash
cargo fmt --all && cargo fmt --all --check
git add crates/gents/src/eval/runner/mod.rs
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" commit -m "feat(eval): a run is finished when it owes nothing and no live process holds a slot (#1515)"
```

### PR 1 gate

- [ ] Run, one at a time: `CARGO_BUILD_JOBS=4 cargo test -p gents --lib eval`, `CARGO_BUILD_JOBS=4 cargo test -p gents --lib optimization`, `CARGO_BUILD_JOBS=4 cargo test -p gents --test eval_runner_canary`, `CARGO_BUILD_JOBS=4 cargo check --workspace --all-targets`, `cargo fmt --all --check`, each logged and grepped. Expected: all pass; record counts in the ledger.
- [ ] Run `CARGO_BUILD_JOBS=4 cargo test -p gents` once, logged. Expected: only the known libp2p dial timeouts fail (standing rule 4).

---

## PR 2: `gents eval`

Branch `eval/51-cli`, base `eval/50-report`. Worktree: `make worktree BRANCH=eval/51-cli DIR=../gents-eval-51-cli BASE=eval/50-report`.

What was found in the CLI (`crates/gents-cli`, library crate `gents_server`):

- Subcommands are registered in the `Command` enum in `crates/gents-cli/src/cli/args.rs` (every argument struct of every command lives in that one file, re-exported through `crate::cli::*`), and dispatched in `async_main` in `crates/gents-cli/src/lib.rs`. Command modules live in `crates/gents-cli/src/commands/` and are declared in `commands/mod.rs` as `pub(crate) mod …;` in alphabetical order. **`eval` goes in `crates/gents-cli/src/commands/eval/` (declared between `diagnose` and `fleet`) and `optimization` in `crates/gents-cli/src/commands/optimization/` (declared between `native_fs_runner` and `p2p`).**
- The launching home and access are resolved by `crate::resolve_config_access(home: Option<&Path>, explicit_graphql: Option<&str>) -> Result<(ConfigAccess, PathBuf)>` (lib.rs): an explicit `--graphql`, else the running server's endpoint from `runtime.json`, else an embedded node on `<home>/data` opened with the home's stored identity. Identity is `crate::resolve_agent_did(home: Option<&Path>, explicit: Option<&str>) -> Result<String>` (home_state.rs), which reads `runtime.json` or `init.json`. `session list` (`commands/session.rs`) is the pattern: `--home`, `--graphql`, then one access resolution.
- Output: existing commands use `crate::print_json` (which calls `println!`) or `--output table|json` through `cli::output_format::OutputFormat`. The spec names `--json`, and the repository rule forbids `println!`, so these commands take `--json` and write through `&mut dyn Write` (D12).
- Tests: in-crate `#[cfg(test)]` modules build `ConfigAccess::Local(Arc<EmbeddedNode>)` directly (`commands/config/crud.rs` `canonical_reads_keep_shared_labels_in_the_selected_owner`). There is no process-level harness that could swap the executor, so the bodies take one (D9).
- Packs: `commands/pack/mod.rs` resolves a name with `resolve_pack_source(name, registry)` (compiled in, else the registry) and materializes into the home's pack cache with `materialize_cached_pack(home, &PackSource)`; `eval run --cell` and `optimization run --subject` reuse both (Task 10).

| File | Responsibility |
|---|---|
| `crates/gents-cli/Cargo.toml` | `tokio-util` dependency |
| `crates/gents-cli/src/cli/args.rs` | `Command::Eval`, `EvalCommand`, the argument structs and value parsers |
| `crates/gents-cli/src/lib.rs` | One dispatch arm |
| `crates/gents-cli/src/commands/mod.rs` | `pub(crate) mod eval;` |
| `crates/gents-cli/src/commands/eval/mod.rs` | `EvalContext`, `Deps`, `dispatch`, `execute`, `surface_refusal`, `write_json`, `load_policy` |
| `crates/gents-cli/src/commands/eval/inspect.rs` | `list`, `show`, `trial` |
| `crates/gents-cli/src/commands/eval/manage.rs` | `cancel`, `invalidate`, `rm`, `dir_size` |
| `crates/gents-cli/src/commands/eval/compare.rs` | `compare` |
| `crates/gents-cli/src/commands/eval/run.rs` | `run`, `resume`, progress lines, the run request |
| `crates/gents-cli/src/commands/eval/render.rs` | Text tables |
| `crates/gents-cli/src/commands/eval/testing.rs` | `#[cfg(test)]` launching-home fixture |
| `crates/gents-cli/src/commands/pack/mod.rs` | `SubjectPack`, `resolve_subject_pack` |

### Task 7: The `eval` arguments, context and test fixture

**Files:**
- Modify: `crates/gents-cli/Cargo.toml` (after `tokio.workspace = true` in `[dependencies]` add `tokio-util.workspace = true`; the workspace root `Cargo.toml` already declares `tokio-util = "0.7"`)
- Modify: `crates/gents-cli/src/cli/args.rs` (new items appended before the final `#[cfg(test)]\nmod tests;`)
- Modify: `crates/gents-cli/src/commands/mod.rs` (`pub(crate) mod eval;` after `pub(crate) mod diagnose;`)
- Create: `crates/gents-cli/src/commands/eval/mod.rs`, `crates/gents-cli/src/commands/eval/testing.rs`

**Interfaces:**
- Consumes:
  ```rust
  pub fn gents::eval::report::report_refused(error: &anyhow::Error) -> Option<&ReportRefused>;   // ReportRefused(pub String)
  pub fn gents::eval::runner::freeze_refused(error) -> Option<&FreezeRefused>;
  pub fn gents::eval::already_invalidated(error) -> Option<&AlreadyInvalidated>;
  pub fn gents::eval::runner::provider_down(error) -> Option<&ProviderDown>;
  pub async fn gents::eval::runner::run(access, request: &RunRequest, executor: &dyn TrialExecutor, registry: &CheckRegistry,
      cancel: CancellationToken, options: &RunOptions) -> Result<RunOutcome>;
  pub async fn gents::eval::runner::embedded::EmbeddedHome::create_temp(prefix: &str) -> Result<EmbeddedHome>;  // .node: Arc<EmbeddedNode>, .did() -> &str
  pub async fn gents::ensure_agent_principal(node: &EmbeddedNode, agent_did: &str) -> Result<..>;
  gents::config_client::{apply_desired_state_plan, DesiredStateApplyDocument, DesiredStateApplyPlan};
  pub(crate) async fn crate::resolve_config_access(home: Option<&Path>, explicit_graphql: Option<&str>) -> Result<(ConfigAccess, PathBuf)>;
  pub(crate) fn crate::resolve_agent_did(home: Option<&Path>, explicit: Option<&str>) -> Result<String>;
  ```
- Produces:
  ```rust
  // crates/gents-cli/src/cli/args.rs
  pub(crate) struct EvalScopeArgs { pub(crate) home: Option<PathBuf>, pub(crate) graphql: Option<String> }
  pub(crate) enum EvalCommand { List(EvalListArgs), Show(EvalShowArgs), Trial(EvalTrialArgs) }   // grows in Tasks 8-10, 13, 14
  impl EvalCommand { pub(crate) fn scope(&self) -> &EvalScopeArgs }
  // crates/gents-cli/src/commands/eval/mod.rs
  pub(crate) struct EvalContext { pub(crate) access: ConfigAccess, pub(crate) home_dir: PathBuf, pub(crate) owner: String }
  impl EvalContext { pub(crate) async fn resolve(scope: &EvalScopeArgs) -> Result<Self>; pub(crate) fn runs_dir(&self) -> PathBuf }
  pub(crate) fn surface_refusal(error: anyhow::Error) -> anyhow::Error;
  pub(crate) fn write_json<T: Serialize>(out: &mut dyn Write, value: &T) -> Result<()>;
  // crates/gents-cli/src/commands/eval/testing.rs (#[cfg(test)])
  pub(crate) const DEFINITION: &str = "cli-def";
  pub(crate) const TRAIN_CASES: [&str; 1]; pub(crate) const VALIDATION_CASES: [&str; 6]; pub(crate) const HELD_OUT_CASES: [&str; 6];
  pub(crate) const TRIALS: u32 = 2; pub(crate) const BASELINE_PROMPT: &str; pub(crate) const CANDIDATE_PROMPT: &str;
  pub(crate) struct Fixture { pub(crate) ctx: EvalContext, pub(crate) pack: PathBuf, .. }
  impl Fixture { pub(crate) async fn new() -> Self; pub(crate) fn pack_arg(&self) -> String;
      pub(crate) fn request(&self, run_id: &str) -> RunRequest;
      pub(crate) async fn scripted_run(&self, run_id: &str, executor: &ScriptedExecutor, cancel: CancellationToken) -> RunOutcome; }
  pub(crate) fn fast() -> RunOptions; pub(crate) fn pass() -> TrialEvidence; pub(crate) fn fail() -> TrialEvidence;
  pub(crate) fn executor(failing_baseline_cases: &[&str]) -> ScriptedExecutor;
  ```

- [ ] **Step 1: Add the arguments, the fixture and the failing tests**

Append to `crates/gents-cli/src/cli/args.rs`, before the final `#[cfg(test)]` / `mod tests;` lines:

```rust
/// `--home` and `--graphql`, resolved the way every home-reading command
/// resolves them.
#[derive(clap::Args, Clone, Debug, Default)]
pub(crate) struct EvalScopeArgs {
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long)]
    pub(crate) graphql: Option<String>,
}

#[derive(Subcommand)]
pub(crate) enum EvalCommand {
    #[command(about = "List eval runs; invalidated runs only with --all")]
    List(EvalListArgs),
    #[command(about = "Show a run's report: slot counts, case means, headline, usage, exposure")]
    Show(EvalShowArgs),
    #[command(about = "Show one slot's latest attempt with its stages and verdicts")]
    Trial(EvalTrialArgs),
}

impl EvalCommand {
    pub(crate) fn scope(&self) -> &EvalScopeArgs {
        match self {
            Self::List(args) => &args.scope,
            Self::Show(args) => &args.scope,
            Self::Trial(args) => &args.scope,
        }
    }
}

#[derive(clap::Args)]
pub(crate) struct EvalListArgs {
    /// Only runs of this eval definition.
    #[arg(long)]
    pub(crate) definition: Option<String>,
    /// Include invalidated runs.
    #[arg(long)]
    pub(crate) all: bool,
    #[arg(long)]
    pub(crate) json: bool,
    #[command(flatten)]
    pub(crate) scope: EvalScopeArgs,
}

#[derive(clap::Args)]
pub(crate) struct EvalShowArgs {
    pub(crate) run_id: String,
    #[arg(long)]
    pub(crate) json: bool,
    #[command(flatten)]
    pub(crate) scope: EvalScopeArgs,
}

#[derive(clap::Args)]
pub(crate) struct EvalTrialArgs {
    pub(crate) run_id: String,
    pub(crate) cell: String,
    pub(crate) case_id: String,
    /// Defaults to 0.
    pub(crate) trial_index: Option<u32>,
    #[arg(long)]
    pub(crate) json: bool,
    #[command(flatten)]
    pub(crate) scope: EvalScopeArgs,
}
```

Create `crates/gents-cli/src/commands/eval/testing.rs`:

```rust
//! A launching home for the `gents eval` and `gents optimization` command
//! tests. `gents::eval::runner::freeze::tests::Launching` and the optimizer's
//! matrix `Harness` are `#[cfg(test)] pub(crate)` inside `gents`, so this
//! crate builds its own from the same public pieces: an embedded home, the
//! documents a run and a job read, and a directory pack whose behavior's
//! prompt matches the installed context (the optimizer's freeze checks that).
//!
//! Ruling U7: this file duplicates `write_fixture_pack` and `Launching` in
//! `crates/gents/src/eval/runner/freeze.rs` (its `tests` module) and the
//! definition shape of `crates/gents/src/optimization/driver/matrix.rs`.
//! Those files and this one move together: a change to the fixture pack's
//! layout or the definition's case shape there is made here in the same PR.

use std::path::{Path, PathBuf};
use std::time::Duration;

use gents::config_client::{
    apply_desired_state_plan, DesiredStateApplyDocument, DesiredStateApplyPlan,
};
use gents::document_config::EvalSplit;
use gents::eval::checks::CheckRegistry;
use gents::eval::runner::embedded::EmbeddedHome;
use gents::eval::runner::{
    run, CellRequest, CellSource, RunOptions, RunOutcome, RunRequest, ScriptKey,
    ScriptedExecutor, TrialEvidence,
};
use gents::{Collection, ConfigAccess};
use serde_json::{json, Value};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

use super::EvalContext;

pub(crate) const DEFINITION: &str = "cli-def";
pub(crate) const TRAIN_CASES: [&str; 1] = ["train-a"];
pub(crate) const VALIDATION_CASES: [&str; 6] =
    ["val-a", "val-b", "val-c", "val-d", "val-e", "val-f"];
pub(crate) const HELD_OUT_CASES: [&str; 6] = ["ho-a", "ho-b", "ho-c", "ho-d", "ho-e", "ho-f"];
pub(crate) const TRIALS: u32 = 2;
pub(crate) const BASELINE_PROMPT: &str = "Watch the mailbox.\n";
pub(crate) const CANDIDATE_PROMPT: &str = "Watch the mailbox, and name the collection.\n";

pub(crate) struct Fixture {
    pub(crate) ctx: EvalContext,
    pub(crate) pack: PathBuf,
    _home: EmbeddedHome,
    _dirs: TempDir,
}

impl Fixture {
    pub(crate) async fn new() -> Self {
        let home = EmbeddedHome::create_temp("cli-eval").await.unwrap();
        let owner = home.did().to_string();
        gents::ensure_agent_principal(home.node.as_ref(), &owner)
            .await
            .unwrap();
        let dirs = tempfile::tempdir().unwrap();
        let pack = dirs.path().join("subject");
        write_pack(&pack);
        let ctx = EvalContext {
            access: ConfigAccess::Local(home.node.clone()),
            home_dir: dirs.path().join("home"),
            owner: owner.clone(),
        };
        install(&ctx.access, documents(&owner)).await;
        Self {
            ctx,
            pack,
            _home: home,
            _dirs: dirs,
        }
    }

    pub(crate) fn pack_arg(&self) -> String {
        self.pack.display().to_string()
    }

    /// Two cells over the six validation cases, two trials each: 24 slots.
    pub(crate) fn request(&self, run_id: &str) -> RunRequest {
        RunRequest {
            run_id: run_id.into(),
            owner: self.ctx.owner.clone(),
            evaluator_did: self.ctx.owner.clone(),
            definition_id: DEFINITION.into(),
            split: EvalSplit::Validation,
            case_ids: None,
            cells: ["baseline", "candidate"]
                .into_iter()
                .map(|cell_id| CellRequest {
                    cell_id: cell_id.into(),
                    label: cell_id.into(),
                    source: CellSource::Directory(self.pack.clone()),
                    behavior_id: "monitor".into(),
                    inference_profile_id: "local".into(),
                })
                .collect(),
            trials_per_case: TRIALS,
            seed_base: 1_000,
            deadline_secs: None,
            concurrency: 1,
            max_infra_retries: 1,
            breaker_threshold: 5,
            purpose: "eval".into(),
            source_commit: "cli-test".into(),
            source_dirty: false,
            captures: Vec::new(),
            runs_dir: self.ctx.runs_dir(),
        }
    }

    /// A run produced by the library with a scripted executor.
    pub(crate) async fn scripted_run(
        &self,
        run_id: &str,
        executor: &ScriptedExecutor,
        cancel: CancellationToken,
    ) -> RunOutcome {
        run(
            &self.ctx.access,
            &self.request(run_id),
            executor,
            &CheckRegistry::builtin(),
            cancel,
            &fast(),
        )
        .await
        .unwrap()
    }
}

pub(crate) fn fast() -> RunOptions {
    RunOptions {
        poll_backoff_base: Duration::from_millis(1),
        poll_backoff_cap: Duration::from_millis(2),
    }
}

/// A stage whose `findings` capture holds a row: `in_range`, 10000.
pub(crate) fn pass() -> TrialEvidence {
    ScriptedExecutor::passed_evidence("did:key:trial", "check", "findings", vec![json!({})])
}

/// A stage whose `findings` capture is empty: `below_min`, 0.
pub(crate) fn fail() -> TrialEvidence {
    ScriptedExecutor::passed_evidence("did:key:trial", "check", "findings", Vec::new())
}

/// Every trial passes, except the `baseline` cell's first attempts on
/// `failing_baseline_cases`.
pub(crate) fn executor(failing_baseline_cases: &[&str]) -> ScriptedExecutor {
    let mut executor = ScriptedExecutor::new().with_default(pass());
    for case_id in failing_baseline_cases {
        for trial_index in 0..TRIALS {
            executor = executor.with(
                ScriptKey {
                    cell_label: "baseline".into(),
                    case_id: (*case_id).into(),
                    trial_index,
                    attempt: 1,
                },
                fail(),
            );
        }
    }
    executor
}

fn documents(owner: &str) -> Vec<(Collection, Value)> {
    let case = |case_id: &str, split: &str| {
        json!({
            "case_id": case_id,
            "split": split,
            "stages": [{
                "stage_id": "check",
                "prompt": "Run the monitor.",
                "deadline_secs": 600,
                "checks": [{
                    "check": "captured_rows_count",
                    "params": {"name": "findings", "min": 1},
                    "tier": "acceptance",
                }],
            }],
        })
    };
    let cases: Vec<Value> = TRAIN_CASES
        .iter()
        .map(|id| case(id, "train"))
        .chain(VALIDATION_CASES.iter().map(|id| case(id, "validation")))
        .chain(HELD_OUT_CASES.iter().map(|id| case(id, "held_out")))
        .collect();
    vec![
        (
            Collection::EvalDefinition,
            json!({
                "definition_id": DEFINITION,
                "agent_did": owner,
                "comparability_version": 1,
                "subject": {"kind": "behavior", "inference_slots": ["primary"]},
                "cases": cases,
            }),
        ),
        (
            Collection::InferenceBackend,
            json!({
                "agent_did": owner,
                "backend_id": "backend",
                "name": "Workstation",
                "provider_kind": "OpenAiCompatible",
                "endpoint": "http://127.0.0.1:8000/v1",
                "auth": {"kind": "unauthenticated"},
            }),
        ),
        (
            Collection::InferenceSampling,
            json!({"agent_did": owner, "sampling_id": "sampling", "temperature": 0.0}),
        ),
        (
            Collection::InferenceProfile,
            json!({
                "agent_did": owner,
                "profile_id": "local",
                "backend_id": "backend",
                "model_name": "test-model",
                "sampling_id": "sampling",
            }),
        ),
        (
            Collection::AgentContext,
            json!({
                "context_id": "monitor-context",
                "agent_did": owner,
                "display_name": "Monitor",
                "system_prompt": BASELINE_PROMPT,
            }),
        ),
        (
            Collection::AgentBehavior,
            json!({
                "behavior_id": "monitor",
                "agent_did": owner,
                "display_name": "Monitor",
                "context_id": "monitor-context",
                "inference_profile_id": "local",
            }),
        ),
    ]
}

async fn install(access: &ConfigAccess, documents: Vec<(Collection, Value)>) {
    let plan = DesiredStateApplyPlan::new(
        documents
            .into_iter()
            .map(|(collection, value)| DesiredStateApplyDocument {
                collection,
                add: value.clone(),
                update: value,
            })
            .collect(),
    )
    .unwrap();
    access
        .transact("cli.eval.test_fixture", |txn| {
            let plan = &plan;
            Box::pin(async move { apply_desired_state_plan(txn, plan).await.map(|_| ()) })
        })
        .await
        .unwrap();
}

/// The shape of `gents::eval::runner::freeze::tests::write_fixture_pack`:
/// one `monitor` behavior whose context prompt is [`BASELINE_PROMPT`].
fn write_pack(root: &Path) {
    std::fs::create_dir_all(root.join("agent_behaviors/monitor")).unwrap();
    std::fs::write(root.join("README.md"), "# monitor fixture\n").unwrap();
    std::fs::write(
        root.join("agent_behaviors/monitor/system_prompt.md"),
        BASELINE_PROMPT,
    )
    .unwrap();
    let manifest = json!({
        "manifest_version": 1,
        "name": "monitor_fixture",
        "version": "1.0.0",
        "description": "CLI eval fixture: one monitor behavior.",
        "authors": ["gents-ai contributors"],
        "kind": "documents",
        "assets": [
            "README.md",
            "agent_behaviors/monitor/system_prompt.md",
            "pack_config.json",
        ],
        "config": "pack_config.json",
        "inference_slots": [{
            "name": "primary",
            "description": "Runs the monitor behavior.",
            "behaviors": ["monitor"],
        }],
    });
    std::fs::write(
        root.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
    let config = json!({
        "agent_principal": {},
        "agent_behaviors": [{
            "behavior_id": "monitor",
            "display_name": "Monitor",
            "context_id": "monitor-context",
            "inference_profile_id": "gents:inference-slot:primary",
        }],
        "contexts": [{
            "context_id": "monitor-context",
            "display_name": "Monitor",
            "system_prompt": "./agent_behaviors/monitor/system_prompt.md",
            "tools_id": "monitor-tools",
        }],
        "tools": [{
            "tools_id": "monitor-tools",
            "display_name": "Monitor tools",
            "host": {"bash": {"mode": "Off"}},
        }],
    });
    std::fs::write(
        root.join("pack_config.json"),
        serde_json::to_vec_pretty(&config).unwrap(),
    )
    .unwrap();
}
```

In `crates/gents-cli/src/commands/mod.rs` add `pub(crate) mod eval;` after `pub(crate) mod diagnose;`.

Create `crates/gents-cli/src/commands/eval/mod.rs` with the module head and tests only (Step 3 adds the items):

```rust
//! `gents eval`: thin commands over `gents::eval`. Each resolves the
//! launching home the way the other commands do, calls one library function,
//! and writes a table or, with `--json`, the library's own structure.
//!
//! Refusals the library returns are re-raised with exactly their text
//! ([`surface_refusal`]); `main` prints them and exits 1. Clap exits 2 on a
//! usage error.

#[cfg(test)]
pub(crate) mod testing;
#[cfg(test)]
mod tests {
    use clap::Parser;
    use tokio_util::sync::CancellationToken;

    use super::testing::{executor, Fixture, VALIDATION_CASES};
    use super::*;
    use crate::cli::EvalCommand;

    /// `EvalCommand` alone; Task 7b hangs it under `gents eval`.
    #[derive(Parser)]
    struct Probe {
        #[command(subcommand)]
        command: EvalCommand,
    }

    #[test]
    fn the_read_commands_parse_with_their_scope() {
        let probe = Probe::try_parse_from(["probe", "list", "--all", "--json", "--home", "/tmp/h"])
            .unwrap_or_else(|error| panic!("{error}"));
        let EvalCommand::List(args) = &probe.command else {
            panic!("not list");
        };
        assert!(args.all && args.json);
        assert_eq!(
            probe.command.scope().home.as_deref(),
            Some(std::path::Path::new("/tmp/h"))
        );
        let probe = Probe::try_parse_from(["probe", "trial", "r1", "baseline", "val-a"])
            .unwrap_or_else(|error| panic!("{error}"));
        let EvalCommand::Trial(args) = &probe.command else {
            panic!("not trial");
        };
        assert_eq!((args.cell.as_str(), args.trial_index), ("baseline", None));
    }

    #[test]
    fn a_refusal_is_surfaced_with_exactly_its_own_text() {
        use anyhow::Context as _;
        let refusal = Err::<(), _>(anyhow::Error::from(gents::eval::report::ReportRefused(
            "no eval run \"x\"".into(),
        )))
        .context("loading the report")
        .unwrap_err();
        assert_eq!(surface_refusal(refusal).to_string(), "no eval run \"x\"");
        assert_eq!(
            surface_refusal(anyhow::anyhow!("plain failure")).to_string(),
            "plain failure"
        );
    }

    #[tokio::test]
    async fn the_fixture_home_runs_a_scripted_two_cell_run() {
        let fixture = Fixture::new().await;
        let outcome = fixture
            .scripted_run("r1", &executor(&VALIDATION_CASES[..3]), CancellationToken::new())
            .await;
        assert_eq!(outcome.completed, 24);
        assert!(fixture.ctx.runs_dir().join("r1").is_dir());
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents-cli --lib commands::eval > "$LOG/t7.log" 2>&1; grep -E "error(\[|:)|test result" "$LOG/t7.log" | head -20`
Expected: compile errors: `EvalContext`, `surface_refusal` not found.

- [ ] **Step 3: Write the implementation**

Insert between the module doc and `#[cfg(test)] pub(crate) mod testing;` in `crates/gents-cli/src/commands/eval/mod.rs`:

```rust
use std::io::Write;
use std::path::PathBuf;

use anyhow::Result;
use gents::ConfigAccess;
use serde::Serialize;

use crate::cli::EvalScopeArgs;

/// The launching home a command acts for.
pub(crate) struct EvalContext {
    pub(crate) access: ConfigAccess,
    pub(crate) home_dir: PathBuf,
    /// The home's identity: owner of the runs it launches, and `by` on what
    /// it invalidates.
    pub(crate) owner: String,
}

impl EvalContext {
    pub(crate) async fn resolve(scope: &EvalScopeArgs) -> Result<Self> {
        let (access, home_dir) =
            crate::resolve_config_access(scope.home.as_deref(), scope.graphql.as_deref()).await?;
        let owner = crate::resolve_agent_did(Some(&home_dir), None)?;
        Ok(Self {
            access,
            home_dir,
            owner,
        })
    }

    /// `<home>/eval/runs`, where the runner freezes every run.
    pub(crate) fn runs_dir(&self) -> PathBuf {
        self.home_dir.join("eval").join("runs")
    }
}

/// A refusal the library returned, re-raised with exactly its own text
/// whatever context was added above it; any other error unchanged.
pub(crate) fn surface_refusal(error: anyhow::Error) -> anyhow::Error {
    let verbatim = gents::eval::report::report_refused(&error)
        .map(ToString::to_string)
        .or_else(|| gents::eval::runner::freeze_refused(&error).map(ToString::to_string))
        .or_else(|| gents::eval::already_invalidated(&error).map(ToString::to_string))
        .or_else(|| gents::eval::runner::provider_down(&error).map(ToString::to_string));
    match verbatim {
        Some(text) => anyhow::anyhow!(text),
        None => error,
    }
}

pub(crate) fn write_json<T: Serialize>(out: &mut dyn Write, value: &T) -> Result<()> {
    serde_json::to_writer_pretty(&mut *out, value)?;
    writeln!(out)?;
    Ok(())
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents-cli --lib commands::eval > "$LOG/t7.log" 2>&1; grep -E "^test |test result|panicked" "$LOG/t7.log" | tail -20`
Expected: the three tests pass.

- [ ] **Step 5: Format and commit**

```bash
cargo fmt --all && cargo fmt --all --check
git add crates/gents-cli/
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" commit -m "feat(cli): the gents eval arguments, launching-home context and test fixture (#1515)"
```

### Task 7b: `eval list`, `show` and `trial`

**Files:**
- Modify: `crates/gents-cli/src/cli/args.rs` (a `Command::Eval` variant after `Subagent`)
- Modify: `crates/gents-cli/src/lib.rs` (`async_main`: one arm before `Command::NativeFsRunner(_) => unreachable!(…)`)
- Modify: `crates/gents-cli/src/commands/eval/mod.rs` (`mod inspect;`, `pub(crate) mod render;`, `dispatch`, `execute`)
- Modify: `crates/gents-cli/src/commands/eval/testing.rs` (`eval_command`, `eval`, `row`)
- Create: `crates/gents-cli/src/commands/eval/inspect.rs`, `crates/gents-cli/src/commands/eval/render.rs`

**Interfaces:**
- Consumes:
  ```rust
  // gents (PR 1)
  pub async fn gents::eval::report::load_runs(access: &ConfigAccess, owner: &str) -> Result<Vec<RunRecord>>;
  pub fn gents::eval::report::run_header(record: &RunRecord) -> RunHeader;
  pub async fn gents::eval::report::load_report_among(access, owner, runs_dir: &Path, record: &RunRecord, peers: &[RunHeader]) -> Result<EvalReport>;
  pub async fn gents::eval::report::load_report(access: &ConfigAccess, owner: &str, runs_dir: &Path, run_id: &str) -> Result<EvalReport>;
  pub async fn gents::eval::load_verdicts(access, owner, run_id) -> Result<Vec<VerdictRecord>>;
  // Task 7: EvalContext, surface_refusal, write_json, testing::{Fixture, executor, VALIDATION_CASES}
  ```
- Produces:
  ```rust
  pub(crate) async fn dispatch(command: EvalCommand) -> Result<()>;
  pub(crate) async fn execute(ctx: &EvalContext, command: EvalCommand, out: &mut dyn Write) -> Result<()>;   // Task 10b adds `deps`
  // crates/gents-cli/src/commands/eval/render.rs
  pub(crate) fn percent(bp: Option<u32>) -> String;          // "50.00%" or "-"
  pub(crate) fn wire<T: Serialize>(value: &T) -> String;      // serde's string form
  pub(crate) fn counts_inline(counts: &SlotCounts) -> String;
  pub(crate) fn report_table(report: &EvalReport, out: &mut dyn Write) -> io::Result<()>;
  pub(crate) fn list_table(rows: &[ListRow], out: &mut dyn Write) -> io::Result<()>;
  pub(crate) fn trial_text(view: &TrialView, out: &mut dyn Write) -> io::Result<()>;
  // crates/gents-cli/src/commands/eval/testing.rs
  pub(crate) fn eval_command(argv: &[&str]) -> EvalCommand;
  pub(crate) async fn eval(fixture: &Fixture, argv: &[&str]) -> anyhow::Result<String>;
  pub(crate) fn row<'a>(output: &'a str, first: &str, width: usize) -> Vec<&'a str>;
  ```

- [ ] **Step 1: Wire the command and write the failing tests**

In the `Command` enum in the same file, after the `Subagent { … }` variant, add:

```rust
    #[command(about = "Run, inspect, compare and clean up evals")]
    Eval {
        #[command(subcommand)]
        command: EvalCommand,
    },
```

In `crates/gents-cli/src/lib.rs` `async_main`, before `Command::NativeFsRunner(_) => unreachable!(…),` add:

```rust
        Command::Eval { command } => commands::eval::dispatch(command).await,
```

Add to `crates/gents-cli/src/commands/eval/testing.rs`: `use clap::Parser;` among the external imports, `use super::execute;` and `use crate::cli::{Cli, Command, EvalCommand};` among the crate imports, and before `fn documents`:

```rust
/// `gents eval <argv…>` as clap parses it.
pub(crate) fn eval_command(argv: &[&str]) -> EvalCommand {
    let cli = Cli::try_parse_from(["gents", "eval"].into_iter().chain(argv.iter().copied()))
        .unwrap_or_else(|error| panic!("{error}"));
    match cli.command {
        Command::Eval { command } => command,
        _ => panic!("not an eval command"),
    }
}

/// Run `gents eval <argv…>` against the fixture and return what it wrote.
pub(crate) async fn eval(fixture: &Fixture, argv: &[&str]) -> anyhow::Result<String> {
    let mut out = Vec::new();
    execute(&fixture.ctx, eval_command(argv), &mut out).await?;
    Ok(String::from_utf8(out)?)
}

/// The whitespace-split row of `output` that starts with `first` and has
/// exactly `width` columns.
pub(crate) fn row<'a>(output: &'a str, first: &str, width: usize) -> Vec<&'a str> {
    output
        .lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>())
        .find(|words| words.first() == Some(&first) && words.len() == width)
        .unwrap_or_else(|| panic!("no {width}-column row for {first} in\n{output}"))
}
```

In `crates/gents-cli/src/commands/eval/mod.rs` add `mod inspect;` and `pub(crate) mod render;` before `#[cfg(test)] pub(crate) mod testing;`, add `EvalCommand` to the `crate::cli` import, and below `impl EvalContext`:

```rust
pub(crate) async fn dispatch(command: EvalCommand) -> Result<()> {
    let ctx = EvalContext::resolve(command.scope()).await?;
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    execute(&ctx, command, &mut out).await
}

pub(crate) async fn execute(
    ctx: &EvalContext,
    command: EvalCommand,
    out: &mut dyn Write,
) -> Result<()> {
    let result = match command {
        EvalCommand::List(args) => inspect::list(ctx, &args, out).await,
        EvalCommand::Show(args) => inspect::show(ctx, &args, out).await,
        EvalCommand::Trial(args) => inspect::trial(ctx, &args, out).await,
    };
    result.map_err(surface_refusal)
}
```

Create an empty `crates/gents-cli/src/commands/eval/render.rs`.

Create `crates/gents-cli/src/commands/eval/inspect.rs` with only its tests:

```rust
#[cfg(test)]
mod tests {
    use tokio_util::sync::CancellationToken;

    use super::super::testing::{eval, executor, row, Fixture, VALIDATION_CASES};

    #[tokio::test]
    async fn show_prints_six_slot_counts_per_cell_and_json_is_the_report() {
        let fixture = Fixture::new().await;
        fixture
            .scripted_run("r1", &executor(&VALIDATION_CASES[..3]), CancellationToken::new())
            .await;

        let table = eval(&fixture, &["show", "r1"]).await.unwrap();
        assert_eq!(
            row(&table, "baseline", 10),
            vec!["baseline", "6", "6", "0", "0", "0", "0", "12", "50.00%", "-/-"]
        );
        assert_eq!(
            row(&table, "candidate", 10),
            vec!["candidate", "12", "0", "0", "0", "0", "0", "12", "100.00%", "-/-"]
        );
        assert!(table.contains("exposure 1 "), "{table}");

        let json: serde_json::Value =
            serde_json::from_str(&eval(&fixture, &["show", "r1", "--json"]).await.unwrap())
                .unwrap();
        assert_eq!(json["report_version"], 1);
        assert_eq!(json["cells"][0]["cell_id"], "baseline");
        assert_eq!(json["cells"][0]["counts"]["fail"], 6);
        assert_eq!(json["cells"][0]["slots"][0]["class"], "fail");
        assert_eq!(json["cells"][1]["headline_bp"], 10_000);
    }

    #[tokio::test]
    async fn list_hides_invalidated_runs_unless_asked_and_counts_slots_per_cell() {
        let fixture = Fixture::new().await;
        let scripted = executor(&VALIDATION_CASES[..3]);
        fixture
            .scripted_run("r1", &scripted, CancellationToken::new())
            .await;
        fixture
            .scripted_run("r2", &scripted, CancellationToken::new())
            .await;
        gents::eval::invalidate_run(
            &fixture.ctx.access,
            &fixture.ctx.owner,
            "r1",
            &fixture.ctx.owner,
            "fixture was broken",
        )
        .await
        .unwrap();

        let listed = eval(&fixture, &["list"]).await.unwrap();
        assert!(!listed.lines().any(|line| line.starts_with("r1 ")), "{listed}");
        let r2 = listed
            .lines()
            .find(|line| line.starts_with("r2 "))
            .unwrap_or_else(|| panic!("{listed}"));
        assert!(
            r2.contains("baseline[pass 6 fail 6 unknown 0 not_evidence 0 abandoned 0 planned 0]"),
            "{r2}"
        );

        let all = eval(&fixture, &["list", "--all"]).await.unwrap();
        let r1 = all
            .lines()
            .find(|line| line.starts_with("r1 "))
            .unwrap_or_else(|| panic!("{all}"));
        assert!(r1.contains(" yes "), "{r1}");

        let json: serde_json::Value =
            serde_json::from_str(&eval(&fixture, &["list", "--all", "--json"]).await.unwrap())
                .unwrap();
        assert_eq!(json.as_array().map(Vec::len), Some(2));
        assert_eq!(json[0]["run_id"], "r1");
        assert_eq!(json[0]["invalidated"]["reason"], "fixture was broken");

        let other = eval(&fixture, &["list", "--definition", "other"])
            .await
            .unwrap();
        assert_eq!(other.lines().count(), 1, "only the header: {other}");
    }

    #[tokio::test]
    async fn trial_prints_the_latest_attempt_with_its_verdicts() {
        let fixture = Fixture::new().await;
        fixture
            .scripted_run("r1", &executor(&VALIDATION_CASES[..3]), CancellationToken::new())
            .await;

        let text = eval(&fixture, &["trial", "r1", "baseline", "val-a", "1"])
            .await
            .unwrap();
        assert!(text.contains("class fail"), "{text}");
        assert!(text.contains("stage check terminal "), "{text}");
        assert!(
            text.contains(
                "verdict captured_rows_count acceptance model_acceptance score 0 reason below_min"
            ),
            "{text}"
        );

        let json: serde_json::Value = serde_json::from_str(
            &eval(&fixture, &["trial", "r1", "candidate", "val-a", "--json"])
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(json["slot"]["class"], "pass");
        assert_eq!(json["slot"]["trial_index"], 0);
        assert_eq!(json["verdicts"][0]["reason_code"], "in_range");

        let cancelled = CancellationToken::new();
        cancelled.cancel();
        fixture
            .scripted_run("r2", &executor(&[]), cancelled)
            .await;
        let error = eval(&fixture, &["trial", "r2", "baseline", "val-a"])
            .await
            .unwrap_err();
        assert!(error.to_string().contains("has no attempt yet"), "{error:#}");
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents-cli --lib commands::eval > "$LOG/t7b.log" 2>&1; grep -E "error(\[|:)|test result" "$LOG/t7b.log" | head -20`
Expected: compile errors `cannot find function `list` in module `inspect`` (and `show`, `trial`).

- [ ] **Step 3: Write the implementation**

Insert above the test module in `crates/gents-cli/src/commands/eval/inspect.rs`:

```rust
//! `list`, `show` and `trial`: reads of the report `gents::eval::report`
//! builds from a run's documents.

use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, Result};
use gents::document_config::{EvalSplit, EvalTier};
use gents::eval::report::{
    load_report, load_report_among, load_runs, run_header, SlotCounts, SlotReport,
};
use gents::eval::{load_verdicts, Invalidation, OutcomeKind, ProviderReason, RunHeader, VerdictRecord};
use serde::Serialize;

use super::{render, write_json, EvalContext};
use crate::cli::{EvalListArgs, EvalShowArgs, EvalTrialArgs};

/// One run as `eval list` shows it.
#[derive(Debug, Serialize)]
pub(crate) struct ListRow {
    pub(crate) run_id: String,
    pub(crate) definition_id: String,
    pub(crate) comparability_version: i64,
    pub(crate) split: EvalSplit,
    pub(crate) purpose: String,
    pub(crate) created_at: String,
    pub(crate) invalidated: Option<Invalidation>,
    pub(crate) cells: Vec<ListCell>,
    /// Why the run's report could not be built, when it could not.
    pub(crate) report_error: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ListCell {
    pub(crate) cell_id: String,
    pub(crate) counts: SlotCounts,
}

pub(super) async fn list(
    ctx: &EvalContext,
    args: &EvalListArgs,
    out: &mut dyn Write,
) -> Result<()> {
    let runs = load_runs(&ctx.access, &ctx.owner).await?;
    let peers: Vec<RunHeader> = runs.iter().map(run_header).collect();
    let mut rows = Vec::new();
    for record in &runs {
        if args
            .definition
            .as_deref()
            .is_some_and(|id| id != record.origin.definition.definition_id)
        {
            continue;
        }
        if record.invalidated.is_some() && !args.all {
            continue;
        }
        let report = load_report_among(&ctx.access, &ctx.owner, &ctx.runs_dir(), record, &peers).await;
        let cells = report
            .as_ref()
            .map(|report| {
                report
                    .cells
                    .iter()
                    .map(|cell| ListCell {
                        cell_id: cell.cell_id.clone(),
                        counts: cell.counts,
                    })
                    .collect()
            })
            .unwrap_or_default();
        rows.push(ListRow {
            run_id: record.run_id.clone(),
            definition_id: record.origin.definition.definition_id.clone(),
            comparability_version: record.origin.definition.comparability_version,
            split: record.origin.split,
            purpose: record.origin.purpose.clone(),
            created_at: record.created_at.clone(),
            invalidated: record.invalidated.clone(),
            cells,
            report_error: report.err().map(|error| format!("{error:#}")),
        });
    }
    if args.json {
        write_json(out, &rows)
    } else {
        Ok(render::list_table(&rows, out)?)
    }
}

pub(super) async fn show(
    ctx: &EvalContext,
    args: &EvalShowArgs,
    out: &mut dyn Write,
) -> Result<()> {
    let report = load_report(&ctx.access, &ctx.owner, &ctx.runs_dir(), &args.run_id).await?;
    if args.json {
        write_json(out, &report)
    } else {
        Ok(render::report_table(&report, out)?)
    }
}

/// One verdict row of the slot's latest attempt.
#[derive(Debug, Serialize)]
pub(crate) struct TrialVerdict {
    pub(crate) verdict_id: String,
    pub(crate) stage_id: String,
    pub(crate) check: String,
    pub(crate) tier: EvalTier,
    pub(crate) kind: OutcomeKind,
    pub(crate) provider_reason: Option<ProviderReason>,
    pub(crate) score_bp: Option<u32>,
    pub(crate) weight: u32,
    pub(crate) reason_code: Option<String>,
    pub(crate) regrade_of: Option<String>,
}

impl From<VerdictRecord> for TrialVerdict {
    fn from(verdict: VerdictRecord) -> Self {
        Self {
            reason_code: verdict
                .raw
                .get("reason_code")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
            verdict_id: verdict.verdict_id,
            stage_id: verdict.stage_id,
            check: verdict.check,
            tier: verdict.tier,
            kind: verdict.kind,
            provider_reason: verdict.provider_reason,
            score_bp: verdict.score_bp,
            weight: verdict.weight,
            regrade_of: verdict.regrade_of,
        }
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct TrialView {
    pub(crate) run_id: String,
    pub(crate) cell_id: String,
    pub(crate) slot: SlotReport,
    /// The retained trial home: `<home>/eval/runs/<home_hint>`.
    pub(crate) home: Option<PathBuf>,
    /// Every verdict row of the latest attempt, regrades included.
    pub(crate) verdicts: Vec<TrialVerdict>,
}

pub(super) async fn trial(
    ctx: &EvalContext,
    args: &EvalTrialArgs,
    out: &mut dyn Write,
) -> Result<()> {
    let report = load_report(&ctx.access, &ctx.owner, &ctx.runs_dir(), &args.run_id).await?;
    let trial_index = args.trial_index.unwrap_or(0);
    let cell = report
        .cells
        .iter()
        .find(|cell| cell.cell_id == args.cell)
        .with_context(|| format!("run {} has no cell {:?}", args.run_id, args.cell))?;
    let slot = cell
        .slots
        .iter()
        .find(|slot| slot.case_id == args.case_id && slot.trial_index == trial_index)
        .with_context(|| {
            format!(
                "run {} cell {} has no slot for case {:?} trial {trial_index}",
                args.run_id, args.cell, args.case_id
            )
        })?;
    let latest = slot.latest.as_ref().with_context(|| {
        format!(
            "case {:?} trial {trial_index} of cell {} has no attempt yet: it is planned",
            args.case_id, args.cell
        )
    })?;
    let mut verdicts: Vec<TrialVerdict> = load_verdicts(&ctx.access, &ctx.owner, &args.run_id)
        .await?
        .into_iter()
        .filter(|verdict| verdict.trial_id == latest.trial_id)
        .map(TrialVerdict::from)
        .collect();
    verdicts.sort_by(|left, right| {
        (&left.stage_id, &left.check, &left.verdict_id).cmp(&(
            &right.stage_id,
            &right.check,
            &right.verdict_id,
        ))
    });
    let view = TrialView {
        run_id: args.run_id.clone(),
        cell_id: args.cell.clone(),
        home: latest
            .home_hint
            .as_ref()
            .map(|hint| ctx.runs_dir().join(hint)),
        slot: slot.clone(),
        verdicts,
    };
    if args.json {
        write_json(out, &view)
    } else {
        Ok(render::trial_text(&view, out)?)
    }
}
```

Write `crates/gents-cli/src/commands/eval/render.rs`:

```rust
//! Plain-text tables for `gents eval`. The `--json` form is the library's
//! own structure; these are for a terminal.

use std::io::{self, Write};

use gents::eval::report::{EvalReport, SlotCounts};
use gents::eval::TrialUsage;
use serde::Serialize;

use super::inspect::{ListRow, TrialView};

/// Basis points as a percentage with two decimals, or `-`.
pub(crate) fn percent(bp: Option<u32>) -> String {
    bp.map_or_else(
        || "-".to_owned(),
        |bp| format!("{}.{:02}%", bp / 100, bp % 100),
    )
}

/// A value's serde form: an enum's wire name, a string's text.
pub(crate) fn wire<T: Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(text)) => text,
        Ok(other) => other.to_string(),
        Err(_) => "?".to_owned(),
    }
}

fn count(value: Option<u64>) -> String {
    value.map_or_else(|| "-".to_owned(), |value| value.to_string())
}

fn tokens(usage: &TrialUsage) -> String {
    format!("{}/{}", count(usage.input_tokens), count(usage.output_tokens))
}

pub(crate) fn counts_inline(counts: &SlotCounts) -> String {
    format!(
        "pass {} fail {} unknown {} not_evidence {} abandoned {} planned {}",
        counts.pass,
        counts.fail,
        counts.unknown,
        counts.not_evidence,
        counts.abandoned,
        counts.planned
    )
}

pub(crate) fn report_table(report: &EvalReport, out: &mut dyn Write) -> io::Result<()> {
    let run = &report.run;
    writeln!(
        out,
        "run {} definition {} v{} split {} purpose {}",
        run.run_id,
        run.definition.definition_id,
        run.definition.comparability_version,
        wire(&run.split),
        run.purpose
    )?;
    writeln!(
        out,
        "created {} trials_per_case {} seed_base {} concurrency {} source {}{}",
        run.created_at,
        run.trials_per_case,
        run.seed_base,
        run.concurrency,
        run.source_commit,
        if run.source_dirty { " (dirty)" } else { "" }
    )?;
    if let Some(invalidation) = &run.invalidated {
        writeln!(
            out,
            "invalidated {} by {}: {}",
            invalidation.at, invalidation.by, invalidation.reason
        )?;
    }
    if report.definition_changed {
        writeln!(
            out,
            "the installed eval definition has changed since this run froze it; this report reads the run's frozen copy"
        )?;
    }
    writeln!(out)?;
    writeln!(
        out,
        "{:<16} {:>5} {:>5} {:>7} {:>12} {:>9} {:>7} {:>8} {:>9} {:>13}",
        "cell",
        "pass",
        "fail",
        "unknown",
        "not_evidence",
        "abandoned",
        "planned",
        "attempts",
        "headline",
        "tokens_in/out"
    )?;
    for cell in &report.cells {
        let counts = &cell.counts;
        writeln!(
            out,
            "{:<16} {:>5} {:>5} {:>7} {:>12} {:>9} {:>7} {:>8} {:>9} {:>13}",
            cell.cell_id,
            counts.pass,
            counts.fail,
            counts.unknown,
            counts.not_evidence,
            counts.abandoned,
            counts.planned,
            cell.attempts,
            percent(cell.headline_bp),
            tokens(&cell.usage)
        )?;
    }
    writeln!(out)?;
    writeln!(
        out,
        "{:<16} {:<24} {:<14} {:>9}  counts",
        "cell", "case", "reducer", "mean"
    )?;
    for cell in &report.cells {
        for case in &cell.cases {
            writeln!(
                out,
                "{:<16} {:<24} {:<14} {:>9}  {}",
                cell.cell_id,
                case.case_id,
                wire(&case.reducer),
                percent(case.mean_bp),
                counts_inline(&case.counts)
            )?;
        }
    }
    writeln!(out)?;
    writeln!(
        out,
        "exposure {} (non-invalidated runs of {} v{} on the {} split)",
        report.exposure,
        run.definition.definition_id,
        run.definition.comparability_version,
        wire(&run.split)
    )
}

pub(crate) fn list_table(rows: &[ListRow], out: &mut dyn Write) -> io::Result<()> {
    writeln!(
        out,
        "{:<28} {:<24} {:<11} {:<24} {:<21} {:<11} cells",
        "run_id", "definition", "split", "purpose", "created", "invalidated"
    )?;
    for row in rows {
        let cells = match &row.report_error {
            Some(error) => format!("report unavailable: {error}"),
            None => row
                .cells
                .iter()
                .map(|cell| format!("{}[{}]", cell.cell_id, counts_inline(&cell.counts)))
                .collect::<Vec<_>>()
                .join(" "),
        };
        writeln!(
            out,
            "{:<28} {:<24} {:<11} {:<24} {:<21} {:<11} {}",
            row.run_id,
            format!("{}@v{}", row.definition_id, row.comparability_version),
            wire(&row.split),
            row.purpose,
            row.created_at,
            if row.invalidated.is_some() { "yes" } else { "no" },
            cells
        )?;
    }
    Ok(())
}

pub(crate) fn trial_text(view: &TrialView, out: &mut dyn Write) -> io::Result<()> {
    let slot = &view.slot;
    writeln!(
        out,
        "run {} cell {} case {} trial {} class {} attempts {}",
        view.run_id,
        view.cell_id,
        slot.case_id,
        slot.trial_index,
        wire(&slot.class),
        slot.attempts
    )?;
    if let Some(latest) = &slot.latest {
        writeln!(
            out,
            "attempt {} trial_id {} agent {} session {}",
            latest.attempt, latest.trial_id, latest.trial_agent_did, latest.session_id
        )?;
        writeln!(
            out,
            "home {}",
            view.home
                .as_ref()
                .map_or_else(|| "-".to_owned(), |home| home.display().to_string())
        )?;
        writeln!(
            out,
            "evidence_digest {}",
            latest.evidence_digest.as_deref().unwrap_or("-")
        )?;
        for stage in &latest.stages {
            writeln!(
                out,
                "stage {} terminal {} kind {}",
                stage.stage_id,
                stage
                    .terminal_state
                    .as_ref()
                    .map_or_else(|| "-".to_owned(), |state| wire(state)),
                stage
                    .failure_kind
                    .map_or_else(|| "-".to_owned(), |kind| kind.as_str().to_owned())
            )?;
        }
    }
    for verdict in &view.verdicts {
        writeln!(
            out,
            "verdict {} {} {} score {} reason {}{}",
            verdict.check,
            wire(&verdict.tier),
            verdict.kind.as_str(),
            verdict
                .score_bp
                .map_or_else(|| "-".to_owned(), |score| score.to_string()),
            verdict.reason_code.as_deref().unwrap_or("-"),
            verdict
                .regrade_of
                .as_ref()
                .map_or_else(String::new, |id| format!(" (regrades {id})"))
        )?;
    }
    Ok(())
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents-cli --lib commands::eval > "$LOG/t7b.log" 2>&1; grep -E "^test |test result|panicked" "$LOG/t7b.log" | tail -20`
Expected: the three `inspect::tests` and Task 7's three pass.

Then `CARGO_BUILD_JOBS=4 cargo test -p gents-cli --lib cli::args > "$LOG/t7b-args.log" 2>&1; grep -E "test result|panicked" "$LOG/t7b-args.log"`. Expected: the existing argument tests (`crates/gents-cli/src/cli/args/tests.rs`) still pass; none of them lists the top-level commands, so adding `eval` changes no expectation.

- [ ] **Step 5: Format and commit**

```bash
cargo fmt --all && cargo fmt --all --check
git add crates/gents-cli/
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" commit -m "feat(cli): gents eval list, show and trial over the derived report (#1515)"
```

### Task 8: `cancel`, `invalidate` and `rm`

**Files:**
- Modify: `crates/gents-cli/src/cli/args.rs` (three `EvalCommand` variants, their `scope()` arms, three argument structs)
- Modify: `crates/gents-cli/src/commands/eval/mod.rs` (`mod manage;`, three `execute` arms)
- Create: `crates/gents-cli/src/commands/eval/manage.rs`
- Modify: `crates/gents/src/optimization/promote.rs` (ruling F12: a sibling of `the_model_facing_config_tool_has_no_optimization_surface`)

**Interfaces:**
- Consumes:
  ```rust
  pub fn gents::eval::runner::request_cancel(runs_dir: &Path, run_id: &str) -> Result<PathBuf>;   // Task 4
  pub fn gents::eval::runner::run_dir(runs_dir: &Path, run_id: &str) -> Result<PathBuf>;          // Task 4
  pub const gents::eval::runner::CANCEL_MARKER: &str;                                             // Task 4
  pub async fn gents::eval::invalidate_run(access, owner, run_id, by: &str, reason: &str) -> Result<()>;  // AlreadyInvalidated on a second call
  pub async fn gents::eval::load_run(access, owner, run_id) -> Result<Option<RunRecord>>;         // test only
  pub async fn gents::eval::load_trials(access, owner, run_id) -> Result<Vec<TrialRecord>>;      // test only
  pub async fn gents::eval::report::load_report(access, owner, runs_dir: &Path, run_id) -> Result<EvalReport>;
  pub fn gents::eval::runner::{slots_owed, running_elsewhere}(…);   // Task 6b: slots_owed(&RunRecord, &[TrialRecord]) -> usize; running_elsewhere(&Path) -> bool
  pub async fn gents::optimization::held_runs(access, owner) -> Result<BTreeMap<String, (String, JobState)>>;   // Task 4b
  impl JobState { pub fn label(&self) -> &'static str }
  pub const crate::self_config::SELF_CONFIG_TOOL_NAMES: [&str; 6];   // gents; the agent-facing config tools
  // Task 7: EvalContext, surface_refusal, testing::{Fixture, eval, executor, row, fast, pass, VALIDATION_CASES}
  ```
- Produces:
  ```rust
  pub(crate) struct EvalRunIdArgs { pub(crate) run_id: String, pub(crate) scope: EvalScopeArgs }
  pub(crate) struct EvalInvalidateArgs { pub(crate) run_id: String, pub(crate) reason: String, pub(crate) scope: EvalScopeArgs }
  pub(crate) struct EvalRmArgs { pub(crate) run_id: String, pub(crate) force: bool, pub(crate) scope: EvalScopeArgs }
  // EvalCommand gains Cancel(EvalRunIdArgs), Invalidate(EvalInvalidateArgs), Rm(EvalRmArgs)
  // crates/gents-cli/src/commands/eval/manage.rs
  pub(crate) fn cancel(runs_dir: &Path, run_id: &str, purpose: Option<&str>, out: &mut dyn Write) -> Result<()>;
  pub(super) async fn cancel_run(ctx: &EvalContext, args: &EvalRunIdArgs, out: &mut dyn Write) -> Result<()>;   // reads the run's purpose (F1)
  pub(super) async fn invalidate(ctx: &EvalContext, args: &EvalInvalidateArgs, out: &mut dyn Write) -> Result<()>;
  pub(super) async fn rm(ctx: &EvalContext, args: &EvalRmArgs, out: &mut dyn Write) -> Result<()>;
  pub(crate) fn dir_size(path: &Path) -> Result<u64>;
  ```

- [ ] **Step 1: Add the arguments and the failing tests**

In `crates/gents-cli/src/cli/args.rs`, add to `EvalCommand`:

```rust
    #[command(about = "Ask the process hosting a run to stop at its next check")]
    Cancel(EvalRunIdArgs),
    #[command(about = "Invalidate a run as this home; its documents stay")]
    Invalidate(EvalInvalidateArgs),
    #[command(about = "Delete a run's directory (homes, packs, sidecars); its documents stay")]
    Rm(EvalRmArgs),
```

to `EvalCommand::scope`:

```rust
            Self::Cancel(args) => &args.scope,
            Self::Invalidate(args) => &args.scope,
            Self::Rm(args) => &args.scope,
```

and after `EvalTrialArgs`:

```rust
#[derive(clap::Args)]
pub(crate) struct EvalRunIdArgs {
    pub(crate) run_id: String,
    #[command(flatten)]
    pub(crate) scope: EvalScopeArgs,
}

#[derive(clap::Args)]
pub(crate) struct EvalInvalidateArgs {
    pub(crate) run_id: String,
    /// Operator-authored; replicates with the run.
    #[arg(long)]
    pub(crate) reason: String,
    #[command(flatten)]
    pub(crate) scope: EvalScopeArgs,
}

#[derive(clap::Args)]
pub(crate) struct EvalRmArgs {
    pub(crate) run_id: String,
    /// Delete even when the run still owes slots, a live process runs it, or
    /// a job that is not settled still needs it.
    #[arg(long)]
    pub(crate) force: bool,
    #[command(flatten)]
    pub(crate) scope: EvalScopeArgs,
}
```

Create `crates/gents-cli/src/commands/eval/manage.rs` with only its tests:

```rust
#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};

    use gents::eval::checks::CheckRegistry;
    use gents::eval::runner::{
        run, Capture, Isolation, ScriptedExecutor, TrialEvidence, TrialExecutor, TrialLocator,
        TrialSpec, CANCEL_MARKER,
    };
    use tokio_util::sync::CancellationToken;

    use super::super::testing::{eval, executor, fast, row, Fixture};
    use super::cancel;

    /// Runs `gents eval cancel` for its run the first time it executes a
    /// trial: a second process cancelling a loop this test is running.
    struct CancelsFromAnotherProcess {
        inner: ScriptedExecutor,
        runs_dir: PathBuf,
        run_id: String,
        done: AtomicBool,
    }

    #[async_trait::async_trait]
    impl TrialExecutor for CancelsFromAnotherProcess {
        fn isolation(&self) -> Isolation {
            self.inner.isolation()
        }

        fn wants_script_key(&self) -> bool {
            self.inner.wants_script_key()
        }

        async fn provision(&self, spec: &TrialSpec) -> TrialLocator {
            self.inner.provision(spec).await
        }

        async fn execute(&self, spec: &TrialSpec, cancel_token: CancellationToken) -> TrialEvidence {
            if !self.done.swap(true, Ordering::SeqCst) {
                let mut confirmation = Vec::new();
                cancel(&self.runs_dir, &self.run_id, None, &mut confirmation).unwrap();
                assert!(String::from_utf8(confirmation)
                    .unwrap()
                    .contains("stops launching at its next check"));
            }
            self.inner.execute(spec, cancel_token).await
        }

        async fn recollect(&self, at: &TrialLocator, captures: &[Capture]) -> Option<TrialEvidence> {
            self.inner.recollect(at, captures).await
        }
    }

    #[tokio::test]
    async fn cancel_is_observed_by_a_loop_running_in_the_test() {
        let fixture = Fixture::new().await;
        let executor = CancelsFromAnotherProcess {
            inner: executor(&[]),
            runs_dir: fixture.ctx.runs_dir(),
            run_id: "r-cancel".into(),
            done: AtomicBool::new(false),
        };
        let outcome = run(
            &fixture.ctx.access,
            &fixture.request("r-cancel"),
            &executor,
            &CheckRegistry::builtin(),
            CancellationToken::new(),
            &fast(),
        )
        .await
        .unwrap();
        assert_eq!((outcome.completed, outcome.abandoned), (1, 0));
        assert!(fixture
            .ctx
            .runs_dir()
            .join("r-cancel")
            .join(CANCEL_MARKER)
            .exists());
        let table = eval(&fixture, &["show", "r-cancel"]).await.unwrap();
        assert_eq!(row(&table, "baseline", 10)[6], "11", "{table}");
        assert_eq!(row(&table, "candidate", 10)[6], "12", "{table}");

        let error = eval(&fixture, &["cancel", "absent"]).await.unwrap_err();
        assert!(
            error.to_string().starts_with("run absent has no directory"),
            "{error:#}"
        );
    }

    /// Ruling F1: cancelling a job's run proceeds and says the job stops there.
    #[tokio::test]
    async fn cancelling_a_jobs_run_says_the_job_stops_at_it() {
        let fixture = Fixture::new().await;
        let mut request = fixture.request("r-job");
        request.purpose = "optimization:job-9".into();
        let interrupted = CancellationToken::new();
        interrupted.cancel();
        run(
            &fixture.ctx.access,
            &request,
            &executor(&[]),
            &CheckRegistry::builtin(),
            interrupted,
            &fast(),
        )
        .await
        .unwrap();
        let said = eval(&fixture, &["cancel", "r-job"]).await.unwrap();
        assert!(
            said.contains("this run belongs to job job-9; the job stops at this run"),
            "{said}"
        );
        assert!(fixture.ctx.runs_dir().join("r-job").join(CANCEL_MARKER).exists());
    }

    #[tokio::test]
    async fn invalidate_confirms_once_and_then_prints_already_invalidated() {
        let fixture = Fixture::new().await;
        fixture
            .scripted_run("r1", &executor(&[]), CancellationToken::new())
            .await;
        let confirmed = eval(&fixture, &["invalidate", "r1", "--reason", "grader bug"])
            .await
            .unwrap();
        assert_eq!(
            confirmed.trim_end(),
            format!("invalidated run r1 as {}: grader bug", fixture.ctx.owner)
        );
        let again = eval(&fixture, &["invalidate", "r1", "--reason", "twice"])
            .await
            .unwrap_err();
        assert_eq!(
            again.to_string(),
            "eval run invalidation is write-once and is already set"
        );
    }

    #[tokio::test]
    async fn rm_refuses_an_unfinished_run_then_forces_and_keeps_the_documents() {
        let fixture = Fixture::new().await;
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        fixture
            .scripted_run("r-stopped", &executor(&[]), cancelled)
            .await;
        let dir = fixture.ctx.runs_dir().join("r-stopped");
        assert!(dir.is_dir());

        let refused = eval(&fixture, &["rm", "r-stopped"]).await.unwrap_err();
        assert!(
            refused
                .to_string()
                .contains("is unfinished: it still owes 24 slots"),
            "{refused:#}"
        );
        assert!(dir.is_dir(), "a refusal deletes nothing");

        let removed = eval(&fixture, &["rm", "r-stopped", "--force"]).await.unwrap();
        assert!(removed.contains("bytes reclaimed"), "{removed}");
        assert!(!dir.exists());
        assert!(
            gents::eval::load_run(&fixture.ctx.access, &fixture.ctx.owner, "r-stopped")
                .await
                .unwrap()
                .is_some(),
            "the run's documents stay"
        );

        fixture
            .scripted_run("r-done", &executor(&[]), CancellationToken::new())
            .await;
        let removed = eval(&fixture, &["rm", "r-done"]).await.unwrap();
        assert!(removed.starts_with("removed "), "{removed}");
    }
}
```

In `crates/gents-cli/src/commands/eval/mod.rs` add `pub(crate) mod manage;` after `mod inspect;` (`pub(crate)` so `gents optimization rm` can reuse `dir_size`), and to `execute`'s match:

```rust
        EvalCommand::Cancel(args) => manage::cancel_run(ctx, &args, out).await,
        EvalCommand::Invalidate(args) => manage::invalidate(ctx, &args, out).await,
        EvalCommand::Rm(args) => manage::rm(ctx, &args, out).await,
```

Append to the tests in `crates/gents/src/optimization/promote.rs`, after `the_model_facing_config_tool_has_no_optimization_surface` (ruling F12):

```rust
    /// Ruling F12: cancelling, invalidating and deleting runs, and collecting
    /// their directories, are the operator's verbs; no agent tool reaches the
    /// functions behind `gents eval cancel | invalidate | rm | gc`.
    #[test]
    fn no_agent_tool_surface_reaches_the_eval_operator_verbs() {
        fn sources(dir: &std::path::Path, found: &mut Vec<std::path::PathBuf>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    sources(&path, found);
                } else if path.extension().and_then(|extension| extension.to_str()) == Some("rs") {
                    found.push(path);
                }
            }
        }
        for name in crate::self_config::SELF_CONFIG_TOOL_NAMES {
            for verb in ["cancel", "invalidate", "rm", "gc"] {
                assert!(
                    !(name.contains("eval") && name.contains(verb)),
                    "the self-config tool set names {name}"
                );
            }
        }
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        for surface in [
            "self_config",
            "toolset",
            "meta_tools",
            "background_tools",
            "tool_surface",
        ] {
            sources(&root.join(surface), &mut files);
        }
        for module in ["toolset.rs", "meta_tools.rs", "background_tools.rs", "tool_control.rs"] {
            files.push(root.join(module));
        }
        assert!(files.len() > 10, "expected the agent tool surfaces under {}", root.display());
        for path in files {
            let source = std::fs::read_to_string(&path).unwrap();
            for word in [
                "request_cancel",
                "CANCEL_MARKER",
                "invalidate_run",
                "eval::runner::run_dir",
                "run_finished",
                "held_runs",
                "referenced_run_ids",
            ] {
                assert!(
                    !source.contains(word),
                    "{} mentions {word}; the eval operator verbs are not an agent's",
                    path.display()
                );
            }
        }
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents-cli --lib commands::eval > "$LOG/t8.log" 2>&1; grep -E "error(\[|:)|test result" "$LOG/t8.log" | head -20`
Expected: compile errors: `cancel`, `invalidate`, `rm` not found in `manage`.

- [ ] **Step 3: Write the implementation**

Insert above the test module in `crates/gents-cli/src/commands/eval/manage.rs`:

```rust
//! `cancel`, `invalidate` and `rm`: the operator's three acts on a run.
//! Documents stay in every case: a marker is not a document, invalidation is
//! the run's one write-once mutation, and `rm` deletes only the directory.

use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};
use gents::eval::runner::{request_cancel, run_dir, running_elsewhere, slots_owed};
use gents::eval::{invalidate_run, load_run, load_trials};

use super::EvalContext;
use crate::cli::{EvalInvalidateArgs, EvalRmArgs, EvalRunIdArgs};

/// Write the marker. `purpose` is the run's origin purpose when known: a
/// job's run is cancelled like any other, and the job stops at it (F1).
pub(crate) fn cancel(
    runs_dir: &Path,
    run_id: &str,
    purpose: Option<&str>,
    out: &mut dyn Write,
) -> Result<()> {
    let marker = request_cancel(runs_dir, run_id)?;
    writeln!(
        out,
        "wrote {}; the process hosting run {run_id} stops launching at its next check, and `gents eval resume {run_id}` continues it",
        marker.display()
    )?;
    if let Some(job_id) = purpose.and_then(|purpose| purpose.strip_prefix("optimization:")) {
        writeln!(
            out,
            "this run belongs to job {job_id}; the job stops at this run, and `gents optimization run … --job-id {job_id}` resumes it"
        )?;
    }
    Ok(())
}

pub(super) async fn cancel_run(
    ctx: &EvalContext,
    args: &EvalRunIdArgs,
    out: &mut dyn Write,
) -> Result<()> {
    let purpose = load_run(&ctx.access, &ctx.owner, &args.run_id)
        .await?
        .map(|record| record.origin.purpose);
    cancel(&ctx.runs_dir(), &args.run_id, purpose.as_deref(), out)
}

pub(super) async fn invalidate(
    ctx: &EvalContext,
    args: &EvalInvalidateArgs,
    out: &mut dyn Write,
) -> Result<()> {
    invalidate_run(&ctx.access, &ctx.owner, &args.run_id, &ctx.owner, &args.reason).await?;
    writeln!(
        out,
        "invalidated run {} as {}: {}",
        args.run_id, ctx.owner, args.reason
    )?;
    Ok(())
}

pub(super) async fn rm(ctx: &EvalContext, args: &EvalRmArgs, out: &mut dyn Write) -> Result<()> {
    let dir = run_dir(&ctx.runs_dir(), &args.run_id)?;
    anyhow::ensure!(
        dir.is_dir(),
        "run {} has no directory {}",
        args.run_id,
        dir.display()
    );
    if !args.force {
        let record = load_run(&ctx.access, &ctx.owner, &args.run_id)
            .await?
            .with_context(|| format!("no eval run {:?} for {}", args.run_id, ctx.owner))?;
        let trials = load_trials(&ctx.access, &ctx.owner, &args.run_id).await?;
        // Ruling F7: finished means nothing owed and no live process on it.
        let owed = slots_owed(&record, &trials);
        anyhow::ensure!(
            owed == 0,
            "run {} is unfinished: it still owes {owed} slots; `gents eval resume {}` finishes it, or pass --force to delete its directory anyway",
            args.run_id,
            args.run_id
        );
        anyhow::ensure!(
            !running_elsewhere(&dir),
            "run {} is being run by a live process right now (see `gents eval watch {}`); pass --force to delete its directory anyway",
            args.run_id,
            args.run_id
        );
        // Ruling F8: a job that still needs the run keeps it.
        if let Some((job_id, state)) = gents::optimization::held_runs(&ctx.access, &ctx.owner)
            .await?
            .get(&args.run_id)
        {
            anyhow::bail!(
                "run {} is evidence of optimization job {job_id}, which is {}; pass --force to delete its directory anyway",
                args.run_id,
                state.label()
            );
        }
    }
    let bytes = dir_size(&dir)?;
    std::fs::remove_dir_all(&dir).with_context(|| format!("removing {}", dir.display()))?;
    tracing::warn!(run_id = %args.run_id, bytes, "eval run directory removed; its documents stay");
    writeln!(
        out,
        "removed {} ({bytes} bytes reclaimed); the run's documents stay",
        dir.display()
    )?;
    Ok(())
}

/// Bytes under `path`, not following symbolic links.
pub(crate) fn dir_size(path: &Path) -> Result<u64> {
    let metadata =
        std::fs::symlink_metadata(path).with_context(|| format!("reading {}", path.display()))?;
    if !metadata.is_dir() {
        return Ok(metadata.len());
    }
    let mut total = 0;
    for entry in std::fs::read_dir(path).with_context(|| format!("listing {}", path.display()))? {
        total += dir_size(&entry?.path())?;
    }
    Ok(total)
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents-cli --lib commands::eval > "$LOG/t8.log" 2>&1; grep -E "^test |test result|panicked" "$LOG/t8.log" | tail -20`
Expected: the four `manage::tests` and the earlier `commands::eval` tests pass. Then `CARGO_BUILD_JOBS=4 cargo test -p gents --lib optimization::promote > "$LOG/t8-surface.log" 2>&1; grep -E "^test |test result|panicked" "$LOG/t8-surface.log"` — the surface test passes (it guards; it has no failing phase).

- [ ] **Step 5: Format and commit**

```bash
cargo fmt --all && cargo fmt --all --check
git add crates/gents-cli/
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" commit -m "feat(cli): gents eval cancel, invalidate and rm (#1515)"
```

### Task 9: `compare`

`compare` stands alone rather than with two neighbors: its `--policy` flag, the banner and the gate lines are one reviewable unit.

**Files:**
- Modify: `crates/gents-cli/src/cli/args.rs` (`EvalCommand::Compare`, its `scope()` arm, `EvalCompareArgs`, `PolicyArg`, `parse_policy`)
- Modify: `crates/gents-cli/src/commands/eval/mod.rs` (`mod compare;`, one `execute` arm, `UNCALIBRATED_BANNER`, `load_policy`)
- Create: `crates/gents-cli/src/commands/eval/compare.rs`
- Modify: `crates/gents-cli/src/commands/eval/render.rs` (`comparison_table`, `signed_percent`, `decision_label`)

**Interfaces:**
- Consumes:
  ```rust
  pub fn gents::eval::report::compare(baseline: &EvalReport, candidate: &EvalReport, baseline_cell: &str, candidate_cell: &str) -> Result<Comparison>;
  impl Comparison { pub fn with_policy(self, policy: &PolicyV2) -> Self }
  pub struct Comparison { comparability, baseline_run, baseline_cell, candidate_run, candidate_cell, cases: Vec<CaseComparison>,
      improved, tied, worsened, mean_diff_bp: Option<i64>, pairs, dropped_baseline, dropped_candidate, imputed,
      p_ppm: Option<u64>, policy: Option<PolicyOutcome> }
  pub struct PolicyOutcome { pub report: DecisionReport, pub gates: GateView, pub calibrated: bool }
  pub enum gents::optimization::Decision { Accept, Reject(RejectReason), Inconclusive(InconclusiveReason) }
  impl gents::optimization::PolicyV2 { pub fn uncalibrated() -> Self }   // Deserialize: every field required
  ```
- Produces:
  ```rust
  pub(crate) enum PolicyArg { Defaults, File(PathBuf) }
  pub(crate) fn parse_policy(raw: &str) -> Result<PolicyArg, String>;
  pub(crate) struct EvalCompareArgs { baseline_run, candidate_run, baseline_cell: Option<String>, candidate_cell: Option<String>,
      policy: Option<PolicyArg>, json: bool, scope }
  pub(crate) const UNCALIBRATED_BANNER: &str = "policy defaults are uncalibrated until the A/A calibration (M5)";
  pub(crate) fn load_policy(arg: &PolicyArg) -> Result<PolicyV2>;
  pub(crate) fn render::comparison_table(comparison: &Comparison, out: &mut dyn Write) -> io::Result<()>;
  pub(crate) fn render::signed_percent(bp: Option<i64>) -> String;   // "+50.00%", "-12.34%", "-"
  pub(crate) fn render::decision_label(decision: &Decision) -> String;   // "accept", "reject(no_improvement)", "inconclusive(too_few_cases)"
  ```

- [ ] **Step 1: Add the arguments and the failing tests**

In `crates/gents-cli/src/cli/args.rs`, add to `EvalCommand`:

```rust
    #[command(about = "Paired statistics between two cells; a policy verdict only with --policy")]
    Compare(EvalCompareArgs),
```

to `scope()`: `Self::Compare(args) => &args.scope,` and after `EvalRmArgs`:

```rust
/// `--policy defaults` or a path to a `PolicyV2` JSON document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PolicyArg {
    Defaults,
    File(PathBuf),
}

pub(crate) fn parse_policy(raw: &str) -> Result<PolicyArg, String> {
    match raw.trim() {
        "" => Err("--policy takes `defaults` or a path to a policy JSON file".to_owned()),
        "defaults" => Ok(PolicyArg::Defaults),
        path => Ok(PolicyArg::File(PathBuf::from(path))),
    }
}

#[derive(clap::Args)]
pub(crate) struct EvalCompareArgs {
    pub(crate) baseline_run: String,
    pub(crate) candidate_run: String,
    /// Required when the baseline run has more than one cell.
    #[arg(long)]
    pub(crate) baseline_cell: Option<String>,
    /// Required when the candidate run has more than one cell.
    #[arg(long)]
    pub(crate) candidate_cell: Option<String>,
    #[arg(long, value_parser = parse_policy)]
    pub(crate) policy: Option<PolicyArg>,
    #[arg(long)]
    pub(crate) json: bool,
    #[command(flatten)]
    pub(crate) scope: EvalScopeArgs,
}
```

Create `crates/gents-cli/src/commands/eval/compare.rs` with only its tests:

```rust
#[cfg(test)]
mod tests {
    use tokio_util::sync::CancellationToken;

    use super::super::testing::{eval, executor, Fixture, VALIDATION_CASES};
    use super::super::UNCALIBRATED_BANNER;

    const CELLS: [&str; 4] = ["--baseline-cell", "baseline", "--candidate-cell", "candidate"];

    #[tokio::test]
    async fn compare_prints_paired_statistics_and_the_banner_with_default_policy() {
        let fixture = Fixture::new().await;
        fixture
            .scripted_run("r1", &executor(&VALIDATION_CASES[..3]), CancellationToken::new())
            .await;

        let mut argv = vec!["compare", "r1", "r1"];
        argv.extend(CELLS);
        let plain = eval(&fixture, &argv).await.unwrap();
        assert!(!plain.contains(UNCALIBRATED_BANNER), "{plain}");
        assert!(
            plain.contains(
                "improved 3 tied 3 worsened 0 mean_diff +50.00% pairs 12 dropped_baseline 0 dropped_candidate 0 imputed 0 p 125000ppm"
            ),
            "{plain}"
        );
        assert!(plain.contains("val-a"), "{plain}");

        argv.extend(["--policy", "defaults"]);
        let decided = eval(&fixture, &argv).await.unwrap();
        assert_eq!(decided.lines().next(), Some(UNCALIBRATED_BANNER));
        assert!(
            decided.contains("policy v2: reject(no_improvement) (alpha 16666ppm, calibrated no)"),
            "{decided}"
        );
        assert!(
            decided.contains(
                "gates sufficient yes no_case_regression yes cost_ok skipped significant no min_effect yes"
            ),
            "{decided}"
        );

        argv.push("--json");
        let json: serde_json::Value =
            serde_json::from_str(&eval(&fixture, &argv).await.unwrap()).unwrap();
        assert_eq!(json["p_ppm"], 125_000);
        assert_eq!(json["policy"]["calibrated"], false);
        assert_eq!(json["policy"]["report"]["decision"]["decision"], "reject");
    }

    #[tokio::test]
    async fn compare_needs_a_cell_for_a_run_with_several_and_reads_a_policy_file() {
        let fixture = Fixture::new().await;
        fixture
            .scripted_run("r1", &executor(&VALIDATION_CASES[..3]), CancellationToken::new())
            .await;
        let error = eval(&fixture, &["compare", "r1", "r1"]).await.unwrap_err();
        assert_eq!(
            error.to_string(),
            "run r1 has 2 cells; name one with --baseline-cell"
        );

        let path = fixture.ctx.home_dir.join("policy.json");
        std::fs::create_dir_all(&fixture.ctx.home_dir).unwrap();
        let policy = gents::optimization::PolicyV2 {
            min_pairs: 1,
            ..gents::optimization::PolicyV2::uncalibrated()
        };
        std::fs::write(&path, serde_json::to_vec(&policy).unwrap()).unwrap();
        let path_arg = path.display().to_string();
        let mut argv = vec!["compare", "r1", "r1", "--policy", path_arg.as_str()];
        argv.extend(CELLS);
        let decided = eval(&fixture, &argv).await.unwrap();
        assert!(!decided.contains(UNCALIBRATED_BANNER), "{decided}");
        assert!(decided.contains("calibrated yes"), "{decided}");
    }
}
```

In `crates/gents-cli/src/commands/eval/mod.rs` add `mod compare;` (first among the `mod` lines), the `execute` arm

```rust
        EvalCommand::Compare(args) => compare::compare(ctx, &args, out).await,
```

and, below `write_json`:

```rust
/// Printed above a policy verdict computed with the placeholder defaults.
pub(crate) const UNCALIBRATED_BANNER: &str =
    "policy defaults are uncalibrated until the A/A calibration (M5)";

pub(crate) fn load_policy(arg: &crate::cli::PolicyArg) -> Result<gents::optimization::PolicyV2> {
    use anyhow::Context;
    match arg {
        crate::cli::PolicyArg::Defaults => Ok(gents::optimization::PolicyV2::uncalibrated()),
        crate::cli::PolicyArg::File(path) => {
            let bytes = std::fs::read(path)
                .with_context(|| format!("reading policy {}", path.display()))?;
            serde_json::from_slice(&bytes)
                .with_context(|| format!("parsing policy {}", path.display()))
        }
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents-cli --lib commands::eval::compare > "$LOG/t9.log" 2>&1; grep -E "error(\[|:)|test result" "$LOG/t9.log" | head -20`
Expected: compile error `cannot find function `compare` in module `compare``.

- [ ] **Step 3: Write the implementation**

Insert above the test module in `crates/gents-cli/src/commands/eval/compare.rs`:

```rust
//! `compare`: the paired statistics between two cells, of one run or two,
//! and a policy verdict only when `--policy` asks for one.

use std::io::Write;

use anyhow::Result;
use gents::eval::report::{compare as compare_reports, load_report, EvalReport};

use super::{load_policy, render, write_json, EvalContext};
use crate::cli::EvalCompareArgs;

pub(super) async fn compare(
    ctx: &EvalContext,
    args: &EvalCompareArgs,
    out: &mut dyn Write,
) -> Result<()> {
    let baseline = load_report(&ctx.access, &ctx.owner, &ctx.runs_dir(), &args.baseline_run).await?;
    let candidate = if args.candidate_run == args.baseline_run {
        baseline.clone()
    } else {
        load_report(&ctx.access, &ctx.owner, &ctx.runs_dir(), &args.candidate_run).await?
    };
    let baseline_cell = pick_cell(&baseline, args.baseline_cell.as_deref(), "--baseline-cell")?;
    let candidate_cell =
        pick_cell(&candidate, args.candidate_cell.as_deref(), "--candidate-cell")?;
    let mut comparison = compare_reports(&baseline, &candidate, &baseline_cell, &candidate_cell)?;
    if let Some(policy) = &args.policy {
        comparison = comparison.with_policy(&load_policy(policy)?);
    }
    if args.json {
        write_json(out, &comparison)
    } else {
        Ok(render::comparison_table(&comparison, out)?)
    }
}

/// The named cell, or the run's only one.
fn pick_cell(report: &EvalReport, named: Option<&str>, flag: &str) -> Result<String> {
    match (named, report.cells.as_slice()) {
        (Some(cell), _) => Ok(cell.to_owned()),
        (None, [only]) => Ok(only.cell_id.clone()),
        (None, cells) => anyhow::bail!(
            "run {} has {} cells; name one with {flag}",
            report.run.run_id,
            cells.len()
        ),
    }
}
```

Append to `crates/gents-cli/src/commands/eval/render.rs` (and add `use gents::eval::report::Comparison;` and `use gents::optimization::Decision;` to its imports):

```rust
/// Signed basis points as a percentage, or `-`.
pub(crate) fn signed_percent(bp: Option<i64>) -> String {
    bp.map_or_else(
        || "-".to_owned(),
        |bp| {
            let sign = if bp < 0 { "-" } else { "+" };
            let magnitude = bp.unsigned_abs();
            format!("{sign}{}.{:02}%", magnitude / 100, magnitude % 100)
        },
    )
}

pub(crate) fn decision_label(decision: &Decision) -> String {
    match decision {
        Decision::Accept => "accept".to_owned(),
        Decision::Reject(reason) => format!("reject({})", wire(reason)),
        Decision::Inconclusive(reason) => format!("inconclusive({})", wire(reason)),
    }
}

fn yes(value: bool) -> &'static str {
    if value {
        "yes"
    } else {
        "no"
    }
}

pub(crate) fn comparison_table(comparison: &Comparison, out: &mut dyn Write) -> io::Result<()> {
    if comparison
        .policy
        .as_ref()
        .is_some_and(|policy| !policy.calibrated)
    {
        writeln!(out, "{}", super::UNCALIBRATED_BANNER)?;
    }
    writeln!(
        out,
        "compare {} v{}: baseline {}/{} candidate {}/{}",
        comparison.comparability.definition_id,
        comparison.comparability.comparability_version,
        comparison.baseline_run,
        comparison.baseline_cell,
        comparison.candidate_run,
        comparison.candidate_cell
    )?;
    writeln!(
        out,
        "{:<24} {:>5} {:>9} {:>9} {:>9}",
        "case", "pairs", "baseline", "candidate", "diff"
    )?;
    for case in &comparison.cases {
        writeln!(
            out,
            "{:<24} {:>5} {:>9} {:>9} {:>9}",
            case.case_id,
            case.pairs,
            percent(case.baseline_mean_bp),
            percent(case.candidate_mean_bp),
            signed_percent(case.diff_bp)
        )?;
    }
    writeln!(
        out,
        "improved {} tied {} worsened {} mean_diff {} pairs {} dropped_baseline {} dropped_candidate {} imputed {} p {}",
        comparison.improved,
        comparison.tied,
        comparison.worsened,
        signed_percent(comparison.mean_diff_bp),
        comparison.pairs,
        comparison.dropped_baseline,
        comparison.dropped_candidate,
        comparison.imputed,
        comparison
            .p_ppm
            .map_or_else(|| "-".to_owned(), |p| format!("{p}ppm"))
    )?;
    if let Some(policy) = &comparison.policy {
        let gates = &policy.gates;
        writeln!(
            out,
            "policy {}: {} (alpha {}ppm, calibrated {})",
            policy.report.policy_version,
            decision_label(&policy.report.decision),
            policy.report.alpha_effective_ppm,
            yes(policy.calibrated)
        )?;
        writeln!(
            out,
            "gates sufficient {} no_case_regression {} cost_ok {} significant {} min_effect {}",
            yes(gates.sufficient),
            yes(gates.no_case_regression),
            gates.cost_ok.map_or("skipped", yes),
            yes(gates.significant),
            yes(gates.min_effect)
        )?;
    }
    Ok(())
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents-cli --lib commands::eval > "$LOG/t9.log" 2>&1; grep -E "^test |test result|panicked" "$LOG/t9.log" | tail -20`
Expected: the two `compare::tests` pass with the rest.

- [ ] **Step 5: Format and commit**

```bash
cargo fmt --all && cargo fmt --all --check
git add crates/gents-cli/
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" commit -m "feat(cli): gents eval compare with an opt-in, labelled policy verdict (#1515)"
```

### Task 10: Subject packs, cell arguments and default ids

**Files:**
- Modify: `crates/gents-cli/src/cli/args.rs` (`CellArg`, `parse_cell`, `parse_assignment`, `parse_split`)
- Modify: `crates/gents-cli/src/commands/pack/mod.rs` (`SubjectPack`, `resolve_subject_pack`, after `materialize_named_pack`)
- Modify: `crates/gents-cli/src/commands/eval/mod.rs` (`source_commit`, `source_dirty`, `default_id`; tests)

**Interfaces:**
- Consumes:
  ```rust
  // crates/gents-cli/src/commands/pack/mod.rs (existing, private)
  enum PackSource { Bundled(ResolvedPack), Registry(registry::RegistryPack) }   // fn manifest(&self) -> &PackManifest
  async fn resolve_pack_source(name: &str, registry_override: Option<&str>) -> Result<PackSource>;
  fn materialize_cached_pack(home: &Path, pack: &PackSource) -> Result<(PathBuf, std::fs::File)>;   // shared-locked cache entry
  // gents::pack::PackManifest { name, metadata: PackMetadata { inference_slots: Vec<PackInferenceSlot { behaviors: Vec<String>, .. }>, .. }, .. }
  pub enum gents::eval::runner::CellSource { InstalledPack { name: String }, Directory(PathBuf) }   // Clone
  // crates/gents-cli/build.rs sets GENTS_BUILD_GIT_SHA and GENTS_BUILD_GIT_DIRTY ("true"/"false") for this crate; `uuid` and `chrono` are dependencies
  ```
- Produces:
  ```rust
  pub(crate) struct CellArg { pub(crate) cell_id: String, pub(crate) pack: String, pub(crate) behavior: Option<String> }
  pub(crate) fn parse_cell(raw: &str) -> Result<CellArg, String>;                 // "<id>=<pack>[:<behavior>]", split at the first ':'
  pub(crate) fn parse_assignment(raw: &str) -> Result<(String, String), String>;   // "<cell>=<profile_id>"
  pub(crate) fn parse_split(raw: &str) -> Result<EvalSplit, String>;              // train | validation | held_out
  pub(crate) struct SubjectPack { pub(crate) source: CellSource, pub(crate) manifest: PackManifest, /* private lease */ }
  impl SubjectPack { pub(crate) fn directory(&self) -> Option<&Path>; pub(crate) fn default_behavior(&self) -> Result<String> }
  pub(crate) async fn resolve_subject_pack(home: &Path, spec: &str, registry: Option<&str>, directory: bool) -> Result<SubjectPack>;
  pub(crate) fn source_commit() -> String;  pub(crate) fn source_dirty() -> bool;
  pub(crate) fn default_id(definition_id: &str) -> String;   // ruling F14
  ```

- [ ] **Step 1: Write the failing tests**

Append inside `mod tests` in `crates/gents-cli/src/commands/eval/mod.rs` (adding `use crate::cli::{parse_cell, CellArg};`):

```rust
    #[test]
    fn a_cell_names_its_pack_and_optionally_a_behavior_with_colons() {
        assert_eq!(
            parse_cell("base=monitor:did:key:z6M:default").unwrap(),
            CellArg {
                cell_id: "base".into(),
                pack: "monitor".into(),
                behavior: Some("did:key:z6M:default".into()),
            }
        );
        assert_eq!(parse_cell("base=/packs/monitor").unwrap().behavior, None);
        assert!(parse_cell("no-equals").is_err());
        assert!(parse_cell("base=").is_err());
        assert!(parse_cell("base=monitor:").is_err());
    }

    #[test]
    fn a_default_id_names_its_definition_and_never_repeats() {
        let (first, second) = (default_id("cli-def"), default_id("cli-def"));
        assert_ne!(first, second);
        let parts: Vec<&str> = first.rsplitn(3, '-').collect();
        assert_eq!(parts.len(), 3, "{first}");
        assert_eq!(parts[2], "cli-def");
        assert!(parts[1].parse::<i64>().is_ok(), "{first}");
        assert!(
            parts[0].len() == 4 && parts[0].bytes().all(|byte| byte.is_ascii_hexdigit()),
            "{first}"
        );
    }

    #[tokio::test]
    async fn a_pack_name_resolves_like_pack_install_and_a_directory_names_its_behavior() {
        let fixture = Fixture::new().await;
        let directory = crate::commands::pack::resolve_subject_pack(
            &fixture.ctx.home_dir,
            &fixture.pack_arg(),
            None,
            false,
        )
        .await
        .unwrap();
        assert_eq!(directory.directory(), Some(fixture.pack.as_path()));
        assert_eq!(directory.default_behavior().unwrap(), "monitor");

        let bundled = gents::pack::pack_catalog().unwrap()[0].name.clone();
        let named = crate::commands::pack::resolve_subject_pack(
            &fixture.ctx.home_dir,
            &bundled,
            None,
            false,
        )
        .await
        .unwrap();
        assert!(
            matches!(&named.source, gents::eval::runner::CellSource::InstalledPack { name } if *name == bundled),
            "a compiled-in pack is handed to the runner by name"
        );
        let materialized = crate::commands::pack::resolve_subject_pack(
            &fixture.ctx.home_dir,
            &bundled,
            None,
            true,
        )
        .await
        .unwrap();
        assert!(materialized
            .directory()
            .is_some_and(|dir| dir.join("manifest.json").is_file()));
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents-cli --lib commands::eval > "$LOG/t10.log" 2>&1; grep -E "error(\[|:)|test result" "$LOG/t10.log" | head -20`
Expected: compile errors: `parse_cell`, `CellArg`, `default_id`, `resolve_subject_pack` not found.

- [ ] **Step 3: Write the implementation**

Append to `crates/gents-cli/src/cli/args.rs`, after `EvalCompareArgs`:

```rust
/// One `--cell <id>=<pack>[:<behavior>]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CellArg {
    pub(crate) cell_id: String,
    /// A directory, or a pack name resolved like `gents pack install`.
    pub(crate) pack: String,
    pub(crate) behavior: Option<String>,
}

/// Split at the first `:` after the `=`: behavior ids may hold colons
/// (`did:key:…:default`), pack names and ordinary paths do not.
pub(crate) fn parse_cell(raw: &str) -> Result<CellArg, String> {
    let (cell_id, rest) = raw
        .split_once('=')
        .ok_or_else(|| format!("--cell {raw:?} must be <id>=<pack>[:<behavior>]"))?;
    let (pack, behavior) = match rest.split_once(':') {
        Some((pack, behavior)) => (pack, Some(behavior)),
        None => (rest, None),
    };
    if cell_id.trim().is_empty()
        || pack.trim().is_empty()
        || behavior.is_some_and(|behavior| behavior.trim().is_empty())
    {
        return Err(format!("--cell {raw:?} has an empty id, pack or behavior"));
    }
    Ok(CellArg {
        cell_id: cell_id.trim().to_owned(),
        pack: pack.trim().to_owned(),
        behavior: behavior.map(|behavior| behavior.trim().to_owned()),
    })
}

/// `<key>=<value>`, both non-empty.
pub(crate) fn parse_assignment(raw: &str) -> Result<(String, String), String> {
    match raw.split_once('=') {
        Some((key, value)) if !key.trim().is_empty() && !value.trim().is_empty() => {
            Ok((key.trim().to_owned(), value.trim().to_owned()))
        }
        _ => Err(format!("{raw:?} must be <cell>=<profile_id>")),
    }
}

pub(crate) fn parse_split(raw: &str) -> Result<gents::document_config::EvalSplit, String> {
    serde_json::from_value(serde_json::Value::String(raw.trim().to_owned()))
        .map_err(|_| format!("unknown split {raw:?}; expected train, validation or held_out"))
}
```

Append to `crates/gents-cli/src/commands/pack/mod.rs`, after `materialize_named_pack`:

```rust
/// A subject pack for `gents eval run` and `gents optimization run`: a
/// directory on disk, or a name resolved the way `gents pack install`
/// resolves one (compiled in, else the registry).
pub(crate) struct SubjectPack {
    pub(crate) source: gents::eval::runner::CellSource,
    pub(crate) manifest: PackManifest,
    /// The shared lock on a materialized cache entry, held while it is read.
    _lease: Option<std::fs::File>,
}

impl SubjectPack {
    pub(crate) fn directory(&self) -> Option<&std::path::Path> {
        match &self.source {
            gents::eval::runner::CellSource::Directory(dir) => Some(dir),
            gents::eval::runner::CellSource::InstalledPack { .. } => None,
        }
    }

    /// The pack's one inference-slot behavior; refused when it declares
    /// none or several.
    pub(crate) fn default_behavior(&self) -> Result<String> {
        let mut behaviors: Vec<&String> = self
            .manifest
            .metadata
            .inference_slots
            .iter()
            .flat_map(|slot| slot.behaviors.iter())
            .collect();
        behaviors.sort();
        behaviors.dedup();
        match behaviors.as_slice() {
            [only] => Ok((*only).clone()),
            _ => anyhow::bail!(
                "pack {} declares {} behaviors in its inference slots; name one as <pack>:<behavior>",
                self.manifest.name,
                behaviors.len()
            ),
        }
    }
}

/// Resolve `spec`. A directory is used in place. A compiled-in pack is
/// handed to the runner by name unless `directory` asks for a directory; a
/// registry pack, or a compiled-in one when `directory` is set, is
/// materialized into `<home>/packs/<name>/<digest>`.
pub(crate) async fn resolve_subject_pack(
    home: &std::path::Path,
    spec: &str,
    registry: Option<&str>,
    directory: bool,
) -> Result<SubjectPack> {
    let path = std::path::Path::new(spec);
    if path.is_dir() {
        let manifest_path = path.join("manifest.json");
        let manifest: PackManifest = serde_json::from_slice(
            &std::fs::read(&manifest_path)
                .with_context(|| format!("reading {}", manifest_path.display()))?,
        )
        .with_context(|| format!("parsing {}", manifest_path.display()))?;
        return Ok(SubjectPack {
            source: gents::eval::runner::CellSource::Directory(path.to_path_buf()),
            manifest,
            _lease: None,
        });
    }
    let pack = resolve_pack_source(spec, registry).await?;
    let manifest = pack.manifest().clone();
    if !directory && matches!(pack, PackSource::Bundled(_)) {
        return Ok(SubjectPack {
            source: gents::eval::runner::CellSource::InstalledPack {
                name: spec.to_owned(),
            },
            manifest,
            _lease: None,
        });
    }
    let (root, lease) = materialize_cached_pack(home, &pack)?;
    Ok(SubjectPack {
        source: gents::eval::runner::CellSource::Directory(root),
        manifest,
        _lease: Some(lease),
    })
}
```

In `crates/gents-cli/src/commands/eval/mod.rs`, add below `write_json`:

```rust
/// The commit this binary was built from, recorded on every run it freezes.
pub(crate) fn source_commit() -> String {
    option_env!("GENTS_BUILD_GIT_SHA")
        .unwrap_or("unknown")
        .to_owned()
}

pub(crate) fn source_dirty() -> bool {
    option_env!("GENTS_BUILD_GIT_DIRTY") == Some("true")
}

/// Ruling F14: `<definition>-<unix ms>-<4 random hex>`, unique per call. An
/// operator who wants a run or job reused (the idempotent freeze, a resume)
/// names it with `--run-id` or `--job-id`.
pub(crate) fn default_id(definition_id: &str) -> String {
    let random = uuid::Uuid::new_v4().simple().to_string();
    format!(
        "{definition_id}-{}-{}",
        chrono::Utc::now().timestamp_millis(),
        &random[..4]
    )
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents-cli --lib commands::eval > "$LOG/t10.log" 2>&1; grep -E "^test |test result|panicked" "$LOG/t10.log" | tail -20` then `CARGO_BUILD_JOBS=4 cargo test -p gents-cli --lib commands::pack > "$LOG/t10-pack.log" 2>&1; grep -E "test result|panicked" "$LOG/t10-pack.log"`
Expected: the three new tests pass; the existing pack tests still pass.

- [ ] **Step 5: Format and commit**

```bash
cargo fmt --all && cargo fmt --all --check
git add crates/gents-cli/
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" commit -m "feat(cli): subject packs resolved like pack install, cell arguments and fresh default ids (#1515)"
```

### Task 10b: `run` and `resume`, with Ctrl-C

**Files:**
- Modify: `crates/gents-cli/src/cli/args.rs` (`EvalCommand::{Run, Resume}`, their `scope()` arms, `EvalRunArgs`, `EvalResumeArgs`)
- Modify: `crates/gents-cli/src/commands/eval/mod.rs` (`mod run;`, `Deps`, `cancel_on_ctrl_c`; `dispatch` and `execute` take `Deps`)
- Modify: `crates/gents-cli/src/commands/eval/testing.rs` (`deps`, `eval_with`; `eval` passes default `Deps`)
- Create: `crates/gents-cli/src/commands/eval/run.rs`

**Interfaces:**
- Consumes:
  ```rust
  pub async fn gents::eval::runner::run(access, request: &RunRequest, executor: &dyn TrialExecutor, registry: &CheckRegistry,
      cancel: CancellationToken, options: &RunOptions) -> Result<RunOutcome>;
  #[allow(clippy::too_many_arguments)]
  pub async fn gents::eval::runner::resume(access, owner: &str, run_id: &str, runs_dir: &Path, executor: &dyn TrialExecutor,
      registry: &CheckRegistry, cancel: CancellationToken, options: &RunOptions) -> Result<RunOutcome>;   // removes the cancel marker (Task 4)
  pub struct gents::eval::runner::RunRequest { run_id, owner, evaluator_did, definition_id, split: EvalSplit, case_ids: Option<Vec<String>>,
      cells: Vec<CellRequest>, trials_per_case: u32, seed_base: i64, deadline_secs: Option<u64>, concurrency: u32, max_infra_retries: u32,
      breaker_threshold: u32, purpose: String, source_commit: String, source_dirty: bool, captures: Vec<Capture>, runs_dir: PathBuf }
  pub struct gents::eval::runner::CellRequest { cell_id, label, source: CellSource, behavior_id, inference_profile_id }
  pub enum gents::eval::runner::CellSource { InstalledPack { name: String }, Directory(PathBuf) }
  pub struct gents::eval::runner::RunOutcome { pub run_id: String, pub completed: u32, pub abandoned: u32, pub not_evidence: u32, pub breaker_tripped: bool }
  pub struct gents::eval::runner::embedded::EmbeddedExecutor;  impl { pub fn new(runtime_options: DocumentRuntimeOptions, runs_dir: PathBuf) -> Self }
  gents::DocumentRuntimeOptions: Default;  gents::eval::checks::CheckRegistry::builtin() -> CheckRegistry
  pub fn gents::eval::documents::default_breaker_threshold() -> u32;   // 5
  pub fn gents::default_behavior_id_for_agent(agent_did: &str) -> String;                  // "{did}:default"
  pub fn gents::default_inference_profile_id_for_behavior(behavior_id: &str) -> String;    // "{behavior_id}-profile"
  // crates/gents-cli/src/commands/pack/mod.rs (existing, private)
  enum PackSource { Bundled(ResolvedPack), Registry(registry::RegistryPack) }   // fn manifest(&self) -> &PackManifest
  async fn resolve_pack_source(name: &str, registry_override: Option<&str>) -> Result<PackSource>;
  fn materialize_cached_pack(home: &Path, pack: &PackSource) -> Result<(PathBuf, std::fs::File)>;   // shared-locked cache entry
  // gents::pack::PackManifest { name, metadata: PackMetadata { inference_slots: Vec<PackInferenceSlot { name, description, behaviors: Vec<String> }>, .. }, .. }
  // crates/gents-cli/build.rs sets GENTS_BUILD_GIT_SHA and GENTS_BUILD_GIT_DIRTY ("true"/"false") for this crate
  ```
- Already produced by Task 10 and only consumed here: `CellArg`, `parse_cell`, `parse_assignment`, `parse_split`, `SubjectPack`, `resolve_subject_pack`, `source_commit`, `source_dirty`, `default_id` (the block below repeats their signatures for reference).
- Produces:
  ```rust
  // crates/gents-cli/src/cli/args.rs
  pub(crate) struct CellArg { pub(crate) cell_id: String, pub(crate) pack: String, pub(crate) behavior: Option<String> }
  pub(crate) fn parse_cell(raw: &str) -> Result<CellArg, String>;                 // "<id>=<pack>[:<behavior>]", split at the first ':'
  pub(crate) fn parse_assignment(raw: &str) -> Result<(String, String), String>;   // "<cell>=<profile_id>"
  pub(crate) fn parse_split(raw: &str) -> Result<EvalSplit, String>;              // train | validation | held_out
  pub(crate) struct EvalRunArgs { definition_id, cells: Vec<CellArg>, profiles: Vec<(String, String)>, split: EvalSplit, trials: u32,
      seed_base: i64, concurrency: u32, purpose: String, run_id: Option<String>, max_infra_retries: u32, registry: Option<String>, json: bool, scope }
  pub(crate) struct EvalResumeArgs { run_id, json: bool, scope }
  // crates/gents-cli/src/commands/pack/mod.rs
  pub(crate) struct SubjectPack { pub(crate) source: CellSource, pub(crate) manifest: PackManifest, /* private lease */ }
  impl SubjectPack { pub(crate) fn directory(&self) -> Option<&Path>; pub(crate) fn default_behavior(&self) -> Result<String> }
  pub(crate) async fn resolve_subject_pack(home: &Path, spec: &str, registry: Option<&str>, directory: bool) -> Result<SubjectPack>;
  // crates/gents-cli/src/commands/eval/mod.rs
  pub(crate) struct Deps<'a> { pub(crate) executor: &'a dyn TrialExecutor, pub(crate) registry: &'a CheckRegistry,
      pub(crate) cancel: CancellationToken, pub(crate) options: RunOptions }
  pub(crate) async fn execute(ctx: &EvalContext, command: EvalCommand, deps: &Deps<'_>, out: &mut dyn Write) -> Result<()>;
  pub(crate) fn cancel_on_ctrl_c() -> CancellationToken;
  pub(crate) fn source_commit() -> String;  pub(crate) fn source_dirty() -> bool;
  // crates/gents-cli/src/commands/eval/testing.rs
  pub(crate) fn deps<'a>(executor: &'a dyn TrialExecutor, registry: &'a CheckRegistry, cancel: CancellationToken) -> Deps<'a>;
  pub(crate) async fn eval_with(fixture: &Fixture, argv: &[&str], deps: &Deps<'_>) -> anyhow::Result<String>;
  ```

- [ ] **Step 1: Add the arguments, `Deps`, the fixture helpers and the failing tests**

In `crates/gents-cli/src/cli/args.rs`, add to `EvalCommand` (first, so `gents eval --help` lists it first):

```rust
    #[command(about = "Freeze and run an eval over one or more cells; Ctrl-C cancels")]
    Run(EvalRunArgs),
    #[command(about = "Continue a run from what it already wrote")]
    Resume(EvalResumeArgs),
```

to `scope()`: `Self::Run(args) => &args.scope,` and `Self::Resume(args) => &args.scope,`; and after `parse_split` (Task 10):

```rust
#[derive(clap::Args)]
pub(crate) struct EvalRunArgs {
    pub(crate) definition_id: String,
    /// `<id>=<pack>[:<behavior>]`, once per cell.
    #[arg(long = "cell", value_parser = parse_cell, required = true)]
    pub(crate) cells: Vec<CellArg>,
    /// `<cell>=<inference_profile_id>`; a cell without one uses the home's
    /// default profile.
    #[arg(long = "profile", value_parser = parse_assignment)]
    pub(crate) profiles: Vec<(String, String)>,
    #[arg(long, value_parser = parse_split, default_value = "validation")]
    pub(crate) split: gents::document_config::EvalSplit,
    #[arg(long, default_value_t = 2)]
    pub(crate) trials: u32,
    #[arg(long, default_value_t = 1000)]
    pub(crate) seed_base: i64,
    #[arg(long, default_value_t = 1)]
    pub(crate) concurrency: u32,
    #[arg(long, default_value = "eval")]
    pub(crate) purpose: String,
    /// Defaults to `<definition_id>-<unix ms>-<4 random hex>`, fresh every
    /// time; name a run with `--run-id` to reuse it (the idempotent freeze).
    #[arg(long)]
    pub(crate) run_id: Option<String>,
    #[arg(long, default_value_t = 1)]
    pub(crate) max_infra_retries: u32,
    /// The pack registry to fall back to for a pack not compiled in. As with
    /// `gents pack install`, a registry download is cached under the default
    /// home, not `--home` (inherited behavior, ruling U13).
    #[arg(long)]
    pub(crate) registry: Option<String>,
    #[arg(long)]
    pub(crate) json: bool,
    #[command(flatten)]
    pub(crate) scope: EvalScopeArgs,
}

#[derive(clap::Args)]
pub(crate) struct EvalResumeArgs {
    pub(crate) run_id: String,
    #[arg(long)]
    pub(crate) json: bool,
    #[command(flatten)]
    pub(crate) scope: EvalScopeArgs,
}
```

In `crates/gents-cli/src/commands/eval/mod.rs`, add these imports and items, add `mod run;` after `mod manage;`, and replace `dispatch` and `execute`:

```rust
use gents::eval::checks::CheckRegistry;
use gents::eval::runner::embedded::EmbeddedExecutor;
use gents::eval::runner::{RunOptions, TrialExecutor};
use tokio_util::sync::CancellationToken;

/// What a command body runs trials with. `dispatch` supplies the embedded
/// executor; tests supply a scripted one.
pub(crate) struct Deps<'a> {
    pub(crate) executor: &'a dyn TrialExecutor,
    pub(crate) registry: &'a CheckRegistry,
    pub(crate) cancel: CancellationToken,
    pub(crate) options: RunOptions,
}

pub(crate) async fn dispatch(command: EvalCommand) -> Result<()> {
    let ctx = EvalContext::resolve(command.scope()).await?;
    let executor = EmbeddedExecutor::new(gents::DocumentRuntimeOptions::default(), ctx.runs_dir());
    let registry = CheckRegistry::builtin();
    // Only a command that hosts a loop replaces the default interrupt: a
    // read-only command stays killable by Ctrl-C.
    let cancel = if matches!(command, EvalCommand::Run(_) | EvalCommand::Resume(_)) {
        cancel_on_ctrl_c()
    } else {
        CancellationToken::new()
    };
    let deps = Deps {
        executor: &executor,
        registry: &registry,
        cancel,
        options: RunOptions::default(),
    };
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    execute(&ctx, command, &deps, &mut out).await
}

pub(crate) async fn execute(
    ctx: &EvalContext,
    command: EvalCommand,
    deps: &Deps<'_>,
    out: &mut dyn Write,
) -> Result<()> {
    let result = match command {
        EvalCommand::Run(args) => run::run(ctx, &args, deps, out).await,
        EvalCommand::Resume(args) => run::resume(ctx, &args, deps, out).await,
        EvalCommand::List(args) => inspect::list(ctx, &args, out).await,
        EvalCommand::Show(args) => inspect::show(ctx, &args, out).await,
        EvalCommand::Trial(args) => inspect::trial(ctx, &args, out).await,
        EvalCommand::Compare(args) => compare::compare(ctx, &args, out).await,
        EvalCommand::Cancel(args) => manage::cancel_run(ctx, &args, out).await,
        EvalCommand::Invalidate(args) => manage::invalidate(ctx, &args, out).await,
        EvalCommand::Rm(args) => manage::rm(ctx, &args, out).await,
    };
    result.map_err(surface_refusal)
}

/// A token Ctrl-C cancels. The loop then stops launching, leaves in-flight
/// trials open for a resume, and the command prints how to resume.
pub(crate) fn cancel_on_ctrl_c() -> CancellationToken {
    let token = CancellationToken::new();
    let cancel = token.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            tracing::warn!("interrupt: the run stops launching; resume it to continue");
            cancel.cancel();
        }
    });
    token
}
```

In `crates/gents-cli/src/commands/eval/testing.rs`, add `use gents::eval::runner::TrialExecutor;` and `use super::Deps;`, and replace `eval` with:

```rust
pub(crate) fn deps<'a>(
    executor: &'a dyn TrialExecutor,
    registry: &'a CheckRegistry,
    cancel: CancellationToken,
) -> Deps<'a> {
    Deps {
        executor,
        registry,
        cancel,
        options: fast(),
    }
}

/// Run `gents eval <argv…>` with `deps` and return what it wrote.
pub(crate) async fn eval_with(
    fixture: &Fixture,
    argv: &[&str],
    deps: &Deps<'_>,
) -> anyhow::Result<String> {
    let mut out = Vec::new();
    execute(&fixture.ctx, eval_command(argv), deps, &mut out).await?;
    Ok(String::from_utf8(out)?)
}

/// Run `gents eval <argv…>` with a scripted executor where every trial passes.
pub(crate) async fn eval(fixture: &Fixture, argv: &[&str]) -> anyhow::Result<String> {
    let executor = executor(&[]);
    let registry = CheckRegistry::builtin();
    eval_with(fixture, argv, &deps(&executor, &registry, CancellationToken::new())).await
}
```

Create `crates/gents-cli/src/commands/eval/run.rs` with only its tests:

```rust
#[cfg(test)]
mod tests {
    use gents::eval::checks::CheckRegistry;
    use tokio_util::sync::CancellationToken;

    use super::super::testing::{
        deps, eval, eval_with, executor, row, Fixture, DEFINITION, VALIDATION_CASES,
    };

    fn run_argv<'a>(run_id: &'a str, cells: &'a [String]) -> Vec<&'a str> {
        let mut argv = vec!["run", DEFINITION, "--run-id", run_id];
        for cell in cells {
            argv.extend(["--cell", cell.as_str()]);
        }
        argv.extend(["--profile", "baseline=local", "--profile", "candidate=local"]);
        argv
    }

    /// Ruling F15: a refused resume prints nothing about the marker.
    #[tokio::test]
    async fn a_refused_resume_says_nothing_about_the_marker() {
        let fixture = Fixture::new().await;
        let interrupted = CancellationToken::new();
        interrupted.cancel();
        fixture.scripted_run("r1", &executor(&[]), interrupted).await;
        eval(&fixture, &["cancel", "r1"]).await.unwrap();
        eval(&fixture, &["invalidate", "r1", "--reason", "broken fixture"])
            .await
            .unwrap();
        let mut out = Vec::new();
        let scripted = executor(&[]);
        let registry = CheckRegistry::builtin();
        let error = super::super::execute(
            &fixture.ctx,
            super::super::testing::eval_command(&["resume", "r1"]),
            &deps(&scripted, &registry, CancellationToken::new()),
            &mut out,
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("invalidated"), "{error:#}");
        assert!(out.is_empty(), "{}", String::from_utf8_lossy(&out));
        assert!(fixture
            .ctx
            .runs_dir()
            .join("r1")
            .join(gents::eval::runner::CANCEL_MARKER)
            .exists());
    }

    fn cells(pack: &str) -> Vec<String> {
        vec![
            format!("baseline={pack}"),
            format!("candidate={pack}:monitor"),
        ]
    }

    #[tokio::test]
    async fn run_prints_one_line_per_landed_slot_and_the_show_table() {
        let fixture = Fixture::new().await;
        let pack = fixture.pack_arg();
        let cells = cells(&pack);
        let scripted = executor(&VALIDATION_CASES[..3]);
        let registry = CheckRegistry::builtin();
        let output = eval_with(
            &fixture,
            &run_argv("r1", &cells),
            &deps(&scripted, &registry, CancellationToken::new()),
        )
        .await
        .unwrap();
        assert_eq!(
            output.lines().filter(|line| line.starts_with("landed ")).count(),
            24,
            "{output}"
        );
        assert!(output.contains("landed baseline val-a #0 attempt 1"), "{output}");
        assert_eq!(
            row(&output, "baseline", 10),
            vec!["baseline", "6", "6", "0", "0", "0", "0", "12", "50.00%", "-/-"]
        );
        assert!(!output.contains("stopped before it finished"), "{output}");

        let mut json_argv = run_argv("r2", &cells);
        json_argv.push("--json");
        let json: serde_json::Value = serde_json::from_str(
            &eval_with(
                &fixture,
                &json_argv,
                &deps(&scripted, &registry, CancellationToken::new()),
            )
            .await
            .unwrap(),
        )
        .unwrap();
        assert_eq!(json["run"]["run_id"], "r2");

        // Ruling F14: without --run-id, a fresh id every time.
        let fresh: Vec<&str> = run_argv("unused", &cells)
            .into_iter()
            .filter(|arg| *arg != "--run-id" && *arg != "unused")
            .collect();
        let first = eval_with(&fixture, &fresh, &deps(&scripted, &registry, CancellationToken::new()))
            .await
            .unwrap();
        assert!(first.contains(&format!("run {DEFINITION}-")), "{first}");
        assert_eq!(json["run"]["source_commit"], super::super::source_commit());
    }

    #[tokio::test]
    async fn a_cancelled_run_says_how_to_resume_and_resume_clears_the_marker_and_finishes() {
        let fixture = Fixture::new().await;
        let pack = fixture.pack_arg();
        let cells = cells(&pack);
        let scripted = executor(&[]);
        let registry = CheckRegistry::builtin();
        // What Ctrl-C does: cancel the token the command was given.
        let interrupted = CancellationToken::new();
        interrupted.cancel();
        let stopped = eval_with(
            &fixture,
            &run_argv("r1", &cells),
            &deps(&scripted, &registry, interrupted),
        )
        .await
        .unwrap();
        assert!(
            stopped.contains(
                "run r1 stopped before it finished (0 completed, 0 abandoned this pass); `gents eval resume r1` continues it"
            ),
            "{stopped}"
        );
        assert_eq!(row(&stopped, "baseline", 10)[6], "12");

        eval(&fixture, &["cancel", "r1"]).await.unwrap();
        let resumed = eval(&fixture, &["resume", "r1"]).await.unwrap();
        assert!(resumed.contains("cleared the cancel marker "), "{resumed}");
        assert_eq!(
            resumed.lines().filter(|line| line.starts_with("landed ")).count(),
            24,
            "{resumed}"
        );
        assert_eq!(row(&resumed, "candidate", 10)[1], "12", "{resumed}");
        assert!(!fixture
            .ctx
            .runs_dir()
            .join("r1")
            .join(gents::eval::runner::CANCEL_MARKER)
            .exists());
    }

    #[tokio::test]
    async fn a_cell_without_a_profile_uses_the_homes_default_profile() {
        let fixture = Fixture::new().await;
        let pack = fixture.pack_arg();
        let cell = format!("baseline={pack}");
        let error = eval(&fixture, &["run", DEFINITION, "--cell", cell.as_str(), "--run-id", "r1"])
            .await
            .unwrap_err();
        let profile = gents::default_inference_profile_id_for_behavior(
            &gents::default_behavior_id_for_agent(&fixture.ctx.owner),
        );
        assert_eq!(
            error.to_string(),
            format!("cell \"baseline\" names no inference profile {profile:?}")
        );

        let stray = eval(
            &fixture,
            &["run", DEFINITION, "--cell", cell.as_str(), "--profile", "other=local"],
        )
        .await
        .unwrap_err();
        assert_eq!(
            stray.to_string(),
            "--profile names cell \"other\", which no --cell declares"
        );
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents-cli --lib commands::eval > "$LOG/t10b.log" 2>&1; grep -E "error(\[|:)|test result" "$LOG/t10b.log" | head -20`
Expected: compile errors: `run::run` and `run::resume` not found.

- [ ] **Step 3: Write the implementation**

Insert above the test module in `crates/gents-cli/src/commands/eval/run.rs`:

```rust
//! `run` and `resume`: the runner's loop, followed from the terminal. A line
//! per slot as it lands, then the `show` table. Ctrl-C cancels the token the
//! command was given (`cancel_on_ctrl_c`).

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::io::Write;
use std::time::Duration;

use anyhow::Result;
use gents::eval::documents::default_breaker_threshold;
use gents::eval::load_trials;
use gents::eval::report::load_report;
use gents::eval::runner::{self, CellRequest, RunOutcome, RunRequest, CANCEL_MARKER};
use gents::{default_behavior_id_for_agent, default_inference_profile_id_for_behavior};

use super::{default_id, render, source_commit, source_dirty, write_json, Deps, EvalContext};
use crate::cli::{EvalResumeArgs, EvalRunArgs};
use crate::commands::pack::{resolve_subject_pack, SubjectPack};

/// How often the documents are read for slots that landed.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(500);

pub(super) async fn run(
    ctx: &EvalContext,
    args: &EvalRunArgs,
    deps: &Deps<'_>,
    out: &mut dyn Write,
) -> Result<()> {
    // The packs are held until the run ends: a cache entry stays locked.
    let (request, _packs) = run_request(ctx, args).await?;
    let run_id = request.run_id.clone();
    let result = follow(
        ctx,
        &run_id,
        BTreeSet::new(),
        !args.json,
        out,
        runner::run(
            &ctx.access,
            &request,
            deps.executor,
            deps.registry,
            deps.cancel.clone(),
            &deps.options,
        ),
    )
    .await;
    finish(ctx, &run_id, args.json, result, out).await
}

pub(super) async fn resume(
    ctx: &EvalContext,
    args: &EvalResumeArgs,
    deps: &Deps<'_>,
    out: &mut dyn Write,
) -> Result<()> {
    // Ruling F15: `runner::resume` thaws the run before it touches the
    // marker, so a refused resume (an invalidated run, a changed pack) says
    // nothing about it; the notice is printed only once the resume ran.
    let marker = runner::run_dir(&ctx.runs_dir(), &args.run_id)?.join(CANCEL_MARKER);
    let had_marker = marker.exists();
    let landed: BTreeSet<String> = load_trials(&ctx.access, &ctx.owner, &args.run_id)
        .await?
        .into_iter()
        .filter(|trial| trial.completion.is_some())
        .map(|trial| trial.identity.trial_id)
        .collect();
    let result = follow(
        ctx,
        &args.run_id,
        landed,
        !args.json,
        out,
        runner::resume(
            &ctx.access,
            &ctx.owner,
            &args.run_id,
            &ctx.runs_dir(),
            deps.executor,
            deps.registry,
            deps.cancel.clone(),
            &deps.options,
        ),
    )
    .await;
    if result.is_ok() && had_marker && !args.json {
        writeln!(out, "cleared the cancel marker {}", marker.display())?;
    }
    finish(ctx, &args.run_id, args.json, result, out).await
}

async fn run_request(ctx: &EvalContext, args: &EvalRunArgs) -> Result<(RunRequest, Vec<SubjectPack>)> {
    let mut profiles: BTreeMap<&str, &str> = BTreeMap::new();
    for (cell_id, profile_id) in &args.profiles {
        anyhow::ensure!(
            args.cells.iter().any(|cell| &cell.cell_id == cell_id),
            "--profile names cell {cell_id:?}, which no --cell declares"
        );
        profiles.insert(cell_id, profile_id);
    }
    let default_profile =
        default_inference_profile_id_for_behavior(&default_behavior_id_for_agent(&ctx.owner));
    let mut cells = Vec::with_capacity(args.cells.len());
    let mut packs = Vec::with_capacity(args.cells.len());
    for cell in &args.cells {
        let pack =
            resolve_subject_pack(&ctx.home_dir, &cell.pack, args.registry.as_deref(), false).await?;
        let behavior_id = match &cell.behavior {
            Some(behavior) => behavior.clone(),
            None => pack.default_behavior()?,
        };
        cells.push(CellRequest {
            cell_id: cell.cell_id.clone(),
            label: cell.cell_id.clone(),
            source: pack.source.clone(),
            behavior_id,
            inference_profile_id: profiles
                .get(cell.cell_id.as_str())
                .map_or_else(|| default_profile.clone(), |profile| (*profile).to_owned()),
        });
        packs.push(pack);
    }
    let request = RunRequest {
        run_id: args
            .run_id
            .clone()
            .unwrap_or_else(|| default_id(&args.definition_id)),
        owner: ctx.owner.clone(),
        evaluator_did: ctx.owner.clone(),
        definition_id: args.definition_id.clone(),
        split: args.split,
        case_ids: None,
        cells,
        trials_per_case: args.trials,
        seed_base: args.seed_base,
        deadline_secs: None,
        concurrency: args.concurrency,
        max_infra_retries: args.max_infra_retries,
        breaker_threshold: default_breaker_threshold(),
        purpose: args.purpose.clone(),
        source_commit: source_commit(),
        source_dirty: source_dirty(),
        captures: Vec::new(),
        runs_dir: ctx.runs_dir(),
    };
    Ok((request, packs))
}

/// Drive `running` to its end, printing a line for each slot that lands.
async fn follow<F>(
    ctx: &EvalContext,
    run_id: &str,
    mut landed: BTreeSet<String>,
    print: bool,
    out: &mut dyn Write,
    running: F,
) -> Result<RunOutcome>
where
    F: Future<Output = Result<RunOutcome>>,
{
    tokio::pin!(running);
    let mut ticker = tokio::time::interval(PROGRESS_INTERVAL);
    let result = loop {
        tokio::select! {
            result = &mut running => break result,
            _ = ticker.tick(), if print => report_landed(ctx, run_id, &mut landed, out).await,
        }
    };
    if print {
        report_landed(ctx, run_id, &mut landed, out).await;
    }
    result
}

/// Print every completed attempt not printed yet. A failed read only delays
/// the lines; it never stops the run.
async fn report_landed(
    ctx: &EvalContext,
    run_id: &str,
    landed: &mut BTreeSet<String>,
    out: &mut dyn Write,
) {
    let trials = match load_trials(&ctx.access, &ctx.owner, run_id).await {
        Ok(trials) => trials,
        Err(error) => {
            tracing::warn!(run_id, error = %format!("{error:#}"), "could not read the run's trials for progress");
            return;
        }
    };
    let mut new: Vec<_> = trials
        .into_iter()
        .filter(|trial| trial.completion.is_some() && !landed.contains(&trial.identity.trial_id))
        .collect();
    new.sort_by(|left, right| {
        let key = |trial: &gents::eval::TrialRecord| {
            (
                trial.identity.cell_id.clone(),
                trial.identity.case_id.clone(),
                trial.identity.trial_index,
                trial.identity.attempt,
            )
        };
        key(left).cmp(&key(right))
    });
    for trial in new {
        let identity = &trial.identity;
        if let Err(error) = writeln!(
            out,
            "landed {} {} #{} attempt {}",
            identity.cell_id, identity.case_id, identity.trial_index, identity.attempt
        ) {
            tracing::warn!(error = %error, "could not write a progress line");
            return;
        }
        landed.insert(identity.trial_id.clone());
    }
}

async fn finish(
    ctx: &EvalContext,
    run_id: &str,
    json: bool,
    result: Result<RunOutcome>,
    out: &mut dyn Write,
) -> Result<()> {
    let outcome = result?;
    // Ruling F1: the pass says whether the token or a marker stopped it.
    let stopped = outcome.cancelled || outcome.abandoned > 0;
    if stopped {
        let note = format!(
            "run {run_id} stopped before it finished ({} completed, {} abandoned this pass); `gents eval resume {run_id}` continues it",
            outcome.completed, outcome.abandoned
        );
        if json {
            tracing::warn!("{note}");
        } else {
            writeln!(out, "{note}")?;
        }
    }
    let report = load_report(&ctx.access, &ctx.owner, &ctx.runs_dir(), run_id).await?;
    if json {
        write_json(out, &report)
    } else {
        Ok(render::report_table(&report, out)?)
    }
}
```

Note `CellSource` must be `Clone` for `pack.source.clone()`; it derives `Clone, Debug` at `crates/gents/src/eval/runner/freeze.rs:36`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents-cli --lib commands::eval > "$LOG/t10b.log" 2>&1; grep -E "^test |test result|panicked" "$LOG/t10b.log" | tail -30`
Expected: the four `run::tests` pass, and every earlier `commands::eval` test still passes.

- [ ] **Step 5: Format and commit**

```bash
cargo fmt --all && cargo fmt --all --check
git add crates/gents-cli/
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" commit -m "feat(cli): gents eval run and resume follow the loop and cancel on Ctrl-C (#1515)"
```

### Task 10c: A binary smoke test of `eval run` (ruling U6)

The embedded path (`dispatch` building `EmbeddedExecutor`) is covered by the canary for the runner and by this one test for the CLI wiring; no CLI test runs a trial on an embedded home.

**Files:**
- Create: `crates/gents-cli/tests/suites/cli_eval.rs`
- Modify: `crates/gents-cli/tests/cli_offline.rs` (register the suite)

**Interfaces:**
- Consumes (`crates/gents-cli/tests/support/process.rs`): `run_cli_text(home_dir: &Path, args: &[&str]) -> Result<String>`, `run_init_json(home_dir: &Path, args: &[&str]) -> Result<Value>`, `run_cli_json(home_dir: &Path, args: &[&str]) -> Result<Value>`, `run_cli_failure_stderr(home_dir: &Path, args: &[&str]) -> Result<String>`; each sets `HOME` to `home_dir`, so the gents home is `<home_dir>/.gents`. `gents pack list` prints `{"packs": [{"name": …}, …]}`. Freeze refuses a missing definition with `no eval definition "<id>" for <owner>` before it validates any cell or creates the run directory.
- Produces: `eval_run_help_and_a_missing_definition_refuses_before_any_run_directory_exists`.

- [ ] **Step 1: Write the test**

Create `crates/gents-cli/tests/suites/cli_eval.rs`:

```rust
use crate::support::*;

use anyhow::{Context, Result};

#[test]
fn eval_run_help_and_a_missing_definition_refuses_before_any_run_directory_exists() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    std::fs::create_dir_all(&home_dir)?;

    let help = run_cli_text(&home_dir, &["eval", "run", "--help"])?;
    assert!(help.contains("--cell"), "{help}");

    run_init_json(&home_dir, &[])?;
    let packs = run_cli_json(&home_dir, &["pack", "list"])?;
    let pack = packs["packs"][0]["name"]
        .as_str()
        .context("a bundled pack name")?
        .to_owned();
    let cell = format!("baseline={pack}:monitor");
    let stderr = run_cli_failure_stderr(
        &home_dir,
        &[
            "eval", "run", "missing-def", "--cell", &cell, "--profile", "baseline=local",
            "--run-id", "smoke",
        ],
    )?;
    assert!(stderr.contains("no eval definition \"missing-def\""), "{stderr}");
    assert!(
        !home_dir.join(".gents/eval/runs/smoke").exists(),
        "refused before any run directory or trial home is created"
    );
    Ok(())
}
```

In `crates/gents-cli/tests/cli_offline.rs`, after the `cli_config_validate` module lines, add:

```rust
#[path = "suites/cli_eval.rs"]
mod cli_eval;
```

- [ ] **Step 2: Run it**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents-cli --test cli_offline cli_eval:: > "$LOG/t10c.log" 2>&1; grep -E "^test |test result|panicked" "$LOG/t10c.log"`
Expected: it passes on the Task 10b code (this task adds coverage, not behavior). If it fails, the failure is a Task 10b defect: fix it there.

- [ ] **Step 3: Format and commit**

```bash
cargo fmt --all && cargo fmt --all --check
git add crates/gents-cli/tests/
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" commit -m "test(cli): eval run's binary refuses a missing definition before creating anything (#1515)"
```

### PR 2 gate

- [ ] Run, one at a time and logged: `CARGO_BUILD_JOBS=4 cargo test -p gents-cli --lib commands::eval`, `CARGO_BUILD_JOBS=4 cargo test -p gents-cli --lib`, `CARGO_BUILD_JOBS=4 cargo test -p gents-cli --test cli_offline`, `CARGO_BUILD_JOBS=4 cargo check --workspace --all-targets`, `cargo fmt --all --check`. Expected: all pass; record counts in the ledger.
- [ ] Record in the ledger that Task 10c's binary smoke test passed, and that `cargo run -p gents-cli --bin gents -- eval run x --cell bad` exits 2 with the `--cell` usage message.

---

## PR 3: `gents optimization`

Branch `eval/52-optimization-cli`, base `eval/51-cli` (which stacks on `<M4-BASE>`, so M6b's `promote` and `revert` are already there). Worktree: `make worktree BRANCH=eval/52-optimization-cli DIR=../gents-eval-52-optimization-cli BASE=eval/51-cli`. Task 12 Step 0 re-verifies the PR 4 signatures against that code before any CLI code is written (ruling U11/U12).

| File | Responsibility |
|---|---|
| `crates/gents-cli/src/cli/args.rs` | `Command::Optimization`, `OptimizationCommand`, argument structs, `SubjectArg`, `ProposerArg` and parsers |
| `crates/gents-cli/src/lib.rs` | One dispatch arm |
| `crates/gents-cli/src/commands/mod.rs` | `pub(crate) mod optimization;` |
| `crates/gents-cli/src/commands/eval/mod.rs` | `EvalContext::jobs_dir` |
| `crates/gents-cli/src/commands/optimization/mod.rs` | `dispatch`, `execute`, `surface_refusal`, `run`, `show`, `promote`, `revert`, `rm` |
| `crates/gents-cli/src/commands/optimization/render.rs` | `journal_line`, `job_table` |
| `crates/gents-cli/src/commands/optimization/testing.rs` | `#[cfg(test)]` proposer file and command helpers |

### Task 11: `optimization run` and `optimization show`

**Files:**
- Modify: `crates/gents-cli/src/cli/args.rs`, `crates/gents-cli/src/lib.rs`, `crates/gents-cli/src/commands/mod.rs`, `crates/gents-cli/src/commands/eval/mod.rs`
- Create: `crates/gents-cli/src/commands/optimization/mod.rs`, `render.rs`, `testing.rs`

**Interfaces:**
- Consumes:
  ```rust
  // gents::optimization (M6b; read at e4f3373b5, unchanged in <M4-BASE>)
  pub async fn run_job(access: &ConfigAccess, request: &JobRequest, executor: &dyn TrialExecutor, proposer: &dyn Proposer,
      registry: &CheckRegistry, policy: &PolicyV2, cancel: CancellationToken) -> Result<JobOutcome>;
  pub struct JobRequest { job_id, owner, evaluator_did, behavior_id, definition_id, inference_profile_id, baseline_pack: PathBuf,
      trials_per_case: u32, budgets: Budgets, max_text_bytes: usize, seed_base: i64, jobs_dir: PathBuf, runs_dir: PathBuf,
      source_commit: String, source_dirty: bool, concurrency: u32, max_infra_retries: u32, breaker_threshold: u32,
      deadline_secs: Option<u64>, run_options: RunOptions }
  pub struct Budgets { pub max_rounds: u32, pub max_case_trials: u64, pub max_tokens: u64, pub deadline_unix_secs: Option<u64> }
  pub struct JobOutcome { pub job_id: String, pub state: JobState, pub checkpoint: Option<Checkpoint>, pub rounds_used: u32 }
  pub enum JobState { Running, ReadyToPromote, NothingToPromote, Exhausted, Failed { reason: String }, Promoted, Stale, Reverted }
  impl JobState { pub fn label(&self) -> &'static str }
  pub async fn show(access: &ConfigAccess, owner: &str, job_id: &str) -> Result<JobView>;
  pub struct JobView { pub job: JobRecord, pub state: JobState, pub checkpoint: Option<Checkpoint>, pub rounds_used: u32,
      pub definition_changed: bool, pub decisions: Vec<DecisionView> }     // Serialize
  pub struct DecisionView { pub round: Option<u32>, pub attempt: u32, pub mode: Mode, pub run_ids: Vec<String>,
      pub journaled: Decision, pub recomputed: Decision, pub mismatch: bool, pub invalidated: bool }
  pub struct JobRecord { pub job_id: String, pub owner: String, pub origin: JobOrigin, pub journal: Vec<JournalEntry> }
  pub struct Checkpoint { pub round: u32, pub text: String, pub pack_digest: String }
  pub enum JournalEntry { Frozen, RunStarted { run_id, round: Option<u32>, split }, Proposed { round, text, rationale, candidate_digest },
      StructuralReject { round, diagnostics }, Decided { round: Option<u32>, attempt: u32, run_ids, mode, decision, policy_version, summary: DecisionSummary },
      BudgetExhausted { round: Option<u32>, reason }, Finalized { state }, Promoted { by, target_digest, previous_text, previous_digest },
      PromotionRefused { drifted: Vec<DriftedRef> }, Reverted { by } }
  pub struct DecisionSummary { improved, tied, worsened, mean_diff_bp: Option<i64>, p_ppm: Option<u64>, alpha_effective_ppm: u64, cost_skipped: bool }
  pub async fn load_job(access, owner, job_id) -> Result<Option<JobRecord>>;
  pub struct ScriptedProposer; impl ScriptedProposer { pub fn new(script: Vec<(String, String)>) -> Self }   // one (text, rationale) per round
  pub struct Proposal { pub text: String, pub rationale: String }   // Deserialize
  pub const MAX_TARGET_TEXT_BYTES: usize;
  pub struct JobRefused(pub String); pub fn job_refused(error: &anyhow::Error) -> Option<&JobRefused>;
  // run_job refuses a policy whose max_rounds differs from budgets.max_rounds (driver.rs check_policy)
  // PR 2
  crate::commands::eval::{EvalContext, Deps, cancel_on_ctrl_c, load_policy, surface_refusal, write_json, source_commit, source_dirty};
  crate::commands::eval::render::{wire, signed_percent, decision_label};
  crate::commands::pack::resolve_subject_pack(home, spec, registry, directory: true) -> Result<SubjectPack>;
  crate::commands::eval::testing::{Fixture, executor, fast, deps, VALIDATION_CASES, CANDIDATE_PROMPT, DEFINITION};
  ```
- Produces:
  ```rust
  pub(crate) struct SubjectArg { pub(crate) pack: String, pub(crate) behavior: Option<String> }
  pub(crate) fn parse_subject(raw: &str) -> Result<SubjectArg, String>;
  pub(crate) struct ProposerArg { pub(crate) script: PathBuf }
  pub(crate) fn parse_proposer(raw: &str) -> Result<ProposerArg, String>;   // only "scripted:<file>"
  pub(crate) enum OptimizationCommand { Run(OptimizationRunArgs), Show(OptimizationShowArgs) }   // Task 12 adds Promote, Revert
  impl OptimizationCommand { pub(crate) fn scope(&self) -> &EvalScopeArgs }
  pub(crate) async fn crate::commands::optimization::dispatch(command: OptimizationCommand) -> Result<()>;
  pub(crate) async fn crate::commands::optimization::execute(ctx: &EvalContext, command: OptimizationCommand, deps: &Deps<'_>, out: &mut dyn Write) -> Result<()>;
  pub(crate) fn render::journal_line(entry: &JournalEntry) -> String;
  pub(crate) fn render::job_table(view: &JobView, out: &mut dyn Write) -> io::Result<()>;
  impl EvalContext { pub(crate) fn jobs_dir(&self) -> PathBuf }   // <home>/eval/jobs (ruling R8)
  // testing: pub(crate) fn proposer_file(fixture: &Fixture) -> String ("scripted:<path>");
  //          pub(crate) async fn optimization_with(fixture, argv, deps) -> anyhow::Result<String>;
  //          pub(crate) async fn optimization(fixture, argv) -> anyhow::Result<String>;
  //          pub(crate) async fn accepted_job(fixture: &Fixture, job_id: &str) -> String   // the run's output
  ```
  The proposer file is a JSON array of `Proposal`s: `[{"text": "…", "rationale": "…"}, …]`, one per round.

- [ ] **Step 1: Add the arguments and the failing tests**

Append to `crates/gents-cli/src/cli/args.rs` (before the final test-module lines):

```rust
/// `--subject <pack>[:<behavior>]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SubjectArg {
    pub(crate) pack: String,
    pub(crate) behavior: Option<String>,
}

pub(crate) fn parse_subject(raw: &str) -> Result<SubjectArg, String> {
    let (pack, behavior) = match raw.split_once(':') {
        Some((pack, behavior)) => (pack, Some(behavior)),
        None => (raw, None),
    };
    if pack.trim().is_empty() || behavior.is_some_and(|behavior| behavior.trim().is_empty()) {
        return Err(format!("--subject {raw:?} must be <pack>[:<behavior>]"));
    }
    Ok(SubjectArg {
        pack: pack.trim().to_owned(),
        behavior: behavior.map(|behavior| behavior.trim().to_owned()),
    })
}

/// `--proposer scripted:<file>`: the only proposer until the LLM one (M7).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProposerArg {
    pub(crate) script: PathBuf,
}

pub(crate) fn parse_proposer(raw: &str) -> Result<ProposerArg, String> {
    match raw.strip_prefix("scripted:") {
        Some(path) if !path.trim().is_empty() => Ok(ProposerArg {
            script: PathBuf::from(path.trim()),
        }),
        _ => Err(format!(
            "unknown proposer {raw:?}; the only proposer until M7 is scripted:<file>"
        )),
    }
}

#[derive(Subcommand)]
pub(crate) enum OptimizationCommand {
    #[command(about = "Run an optimization job over a subject pack; Ctrl-C stops it for a resume")]
    Run(OptimizationRunArgs),
    #[command(about = "Show a job's journal and every decision recomputed from its runs")]
    Show(OptimizationShowArgs),
}

impl OptimizationCommand {
    pub(crate) fn scope(&self) -> &EvalScopeArgs {
        match self {
            Self::Run(args) => &args.scope,
            Self::Show(args) => &args.scope,
        }
    }
}

#[derive(clap::Args)]
pub(crate) struct OptimizationRunArgs {
    pub(crate) definition_id: String,
    #[arg(long, value_parser = parse_subject)]
    pub(crate) subject: SubjectArg,
    #[arg(long, default_value_t = 3)]
    pub(crate) rounds: u32,
    #[arg(long, default_value_t = 2)]
    pub(crate) trials: u32,
    #[arg(long, default_value_t = 1000)]
    pub(crate) seed_base: i64,
    /// `defaults` (with max_rounds set to --rounds) when absent.
    #[arg(long, value_parser = parse_policy)]
    pub(crate) policy: Option<PolicyArg>,
    /// Defaults to `<definition_id>-<unix ms>-<4 random hex>`, fresh every
    /// time (ruling F14); name the job with `--job-id` and repeat the same
    /// flags to resume it.
    #[arg(long)]
    pub(crate) job_id: Option<String>,
    #[arg(long, value_parser = parse_proposer)]
    pub(crate) proposer: Option<ProposerArg>,
    /// The inference profile both arms run on; the home's default when absent.
    #[arg(long)]
    pub(crate) profile: Option<String>,
    #[arg(long, default_value_t = 10_000)]
    pub(crate) max_case_trials: u64,
    #[arg(long, default_value_t = u64::MAX)]
    pub(crate) max_tokens: u64,
    #[arg(long)]
    pub(crate) registry: Option<String>,
    #[arg(long)]
    pub(crate) json: bool,
    #[command(flatten)]
    pub(crate) scope: EvalScopeArgs,
}

#[derive(clap::Args)]
pub(crate) struct OptimizationShowArgs {
    pub(crate) job_id: String,
    #[arg(long)]
    pub(crate) json: bool,
    #[command(flatten)]
    pub(crate) scope: EvalScopeArgs,
}
```

In the `Command` enum, after `Eval { … }`:

```rust
    #[command(about = "Run, inspect, promote and revert configuration optimization jobs")]
    Optimization {
        #[command(subcommand)]
        command: OptimizationCommand,
    },
```

In `crates/gents-cli/src/lib.rs` `async_main`, after the `Command::Eval` arm: `Command::Optimization { command } => commands::optimization::dispatch(command).await,`. In `commands/mod.rs` add `pub(crate) mod optimization;` after `pub(crate) mod native_fs_runner;`. In `commands/eval/mod.rs` add to `impl EvalContext`:

```rust
    /// `<home>/eval/jobs` (ruling R8): each job owns `<jobs_dir>/<job_id>/`.
    pub(crate) fn jobs_dir(&self) -> PathBuf {
        self.home_dir.join("eval").join("jobs")
    }
```

Create `crates/gents-cli/src/commands/optimization/testing.rs`:

```rust
//! Helpers for the `gents optimization` tests, over the eval fixture.

use clap::Parser;
use gents::eval::checks::CheckRegistry;
use tokio_util::sync::CancellationToken;

use super::execute;
use crate::cli::{Cli, Command, OptimizationCommand};
use crate::commands::eval::testing::{
    deps, executor, Fixture, CANDIDATE_PROMPT, DEFINITION, VALIDATION_CASES,
};
use crate::commands::eval::Deps;

pub(crate) fn optimization_command(argv: &[&str]) -> OptimizationCommand {
    let cli = Cli::try_parse_from(
        ["gents", "optimization"]
            .into_iter()
            .chain(argv.iter().copied()),
    )
    .unwrap_or_else(|error| panic!("{error}"));
    match cli.command {
        Command::Optimization { command } => command,
        _ => panic!("not an optimization command"),
    }
}

pub(crate) async fn optimization_with(
    fixture: &Fixture,
    argv: &[&str],
    deps: &Deps<'_>,
) -> anyhow::Result<String> {
    let mut out = Vec::new();
    execute(&fixture.ctx, optimization_command(argv), deps, &mut out).await?;
    Ok(String::from_utf8(out)?)
}

/// With a scripted executor where every trial passes.
pub(crate) async fn optimization(fixture: &Fixture, argv: &[&str]) -> anyhow::Result<String> {
    let executor = executor(&[]);
    let registry = CheckRegistry::builtin();
    optimization_with(fixture, argv, &deps(&executor, &registry, CancellationToken::new())).await
}

/// `scripted:<file>` proposing [`CANDIDATE_PROMPT`] in each of three rounds.
pub(crate) fn proposer_file(fixture: &Fixture) -> String {
    std::fs::create_dir_all(&fixture.ctx.home_dir).unwrap();
    let path = fixture.ctx.home_dir.join("proposals.json");
    let proposals: Vec<serde_json::Value> = (0..3)
        .map(|_| {
            serde_json::json!({
                "text": CANDIDATE_PROMPT,
                "rationale": "name the collection the monitor should read",
            })
        })
        .collect();
    std::fs::write(&path, serde_json::to_vec(&proposals).unwrap()).unwrap();
    format!("scripted:{}", path.display())
}

/// Run a job whose baseline fails every validation case and whose candidate
/// passes them: it reaches `ready_to_promote`. Returns what the run wrote.
pub(crate) async fn accepted_job(fixture: &Fixture, job_id: &str) -> String {
    let scripted = executor(&VALIDATION_CASES);
    let registry = CheckRegistry::builtin();
    let pack = fixture.pack_arg();
    let proposer = proposer_file(fixture);
    optimization_with(
        fixture,
        &[
            "run",
            DEFINITION,
            "--subject",
            pack.as_str(),
            "--profile",
            "local",
            "--proposer",
            proposer.as_str(),
            "--job-id",
            job_id,
        ],
        &deps(&scripted, &registry, CancellationToken::new()),
    )
    .await
    .unwrap()
}
```

Create `crates/gents-cli/src/commands/optimization/mod.rs` with only the module lines and tests:

```rust
mod render;
#[cfg(test)]
pub(crate) mod testing;

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::testing::{accepted_job, optimization};
    use crate::cli::Cli;
    use crate::commands::eval::testing::{Fixture, DEFINITION};

    #[tokio::test]
    async fn run_refuses_without_a_proposer_and_parses_only_the_scripted_one() {
        let fixture = Fixture::new().await;
        let pack = fixture.pack_arg();
        let error = optimization(&fixture, &["run", DEFINITION, "--subject", pack.as_str()])
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "optimization run needs a proposer: until the LLM proposer lands (M7), pass --proposer scripted:<file>"
        );
        let usage = match Cli::try_parse_from([
            "gents",
            "optimization",
            "run",
            DEFINITION,
            "--subject",
            "monitor",
            "--proposer",
            "llm",
        ]) {
            Ok(_) => panic!("an unknown proposer must not parse"),
            Err(error) => error,
        };
        assert!(
            usage.to_string().contains("the only proposer until M7 is scripted:<file>"),
            "{usage}"
        );
        assert_eq!(usage.exit_code(), 2);
    }

    #[tokio::test]
    async fn a_scripted_job_runs_to_ready_to_promote_and_show_recomputes_its_decisions() {
        let fixture = Fixture::new().await;
        let output = accepted_job(&fixture, "job-1").await;
        assert!(output.lines().any(|line| line == "frozen"), "{output}");
        assert!(output.contains("finalized ready_to_promote"), "{output}");
        assert!(output.contains("job job-1 state ready_to_promote"), "{output}");

        let shown = optimization(&fixture, &["show", "job-1"]).await.unwrap();
        assert!(shown.contains("state ready_to_promote"), "{shown}");
        assert!(shown.contains("checkpoint round 1 pack "), "{shown}");
        let accepted = shown
            .lines()
            .find(|line| line.contains("journaled accept"))
            .unwrap_or_else(|| panic!("{shown}"));
        assert!(accepted.contains("recomputed accept"), "{accepted}");
        assert!(!shown.contains("MISMATCH"), "{shown}");

        let json: serde_json::Value = serde_json::from_str(
            &optimization(&fixture, &["show", "job-1", "--json"])
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(json["state"]["state"], "ready_to_promote");
        assert_eq!(json["job"]["job_id"], "job-1");
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents-cli --lib commands::optimization > "$LOG/t11.log" 2>&1; grep -E "error(\[|:)|test result" "$LOG/t11.log" | head -20`
Expected: compile errors: `execute` and `dispatch` not found in `commands::optimization`.

- [ ] **Step 3: Write the implementation**

Replace the head of `crates/gents-cli/src/commands/optimization/mod.rs` (everything above `#[cfg(test)] mod tests`) with:

```rust
//! `gents optimization`: thin commands over `gents::optimization`. A job is
//! driven only by a scripted proposer supplied as a file until the LLM
//! proposer exists (M7).

use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};
use gents::eval::checks::CheckRegistry;
use gents::eval::documents::default_breaker_threshold;
use gents::eval::runner::embedded::EmbeddedExecutor;
use gents::eval::runner::RunOptions;
use gents::optimization::{
    job_refused, load_job, run_job, show as show_job, Budgets, JobOutcome, JobRequest, JobState,
    PolicyV2, Proposal, ScriptedProposer, MAX_TARGET_TEXT_BYTES,
};
use gents::{default_behavior_id_for_agent, default_inference_profile_id_for_behavior};
use tokio_util::sync::CancellationToken;

use crate::cli::{OptimizationCommand, OptimizationRunArgs, OptimizationShowArgs};
use crate::commands::eval::{
    cancel_on_ctrl_c, default_id, load_policy, source_commit, source_dirty, write_json, Deps,
    EvalContext,
};
use crate::commands::pack::resolve_subject_pack;

mod render;
#[cfg(test)]
pub(crate) mod testing;

/// How often the journal is read for entries that landed.
const PROGRESS_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);

pub(crate) async fn dispatch(command: OptimizationCommand) -> Result<()> {
    let ctx = EvalContext::resolve(command.scope()).await?;
    let executor = EmbeddedExecutor::new(gents::DocumentRuntimeOptions::default(), ctx.runs_dir());
    let registry = CheckRegistry::builtin();
    let cancel = if matches!(command, OptimizationCommand::Run(_)) {
        cancel_on_ctrl_c()
    } else {
        CancellationToken::new()
    };
    let deps = Deps {
        executor: &executor,
        registry: &registry,
        cancel,
        options: RunOptions::default(),
    };
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    execute(&ctx, command, &deps, &mut out).await
}

pub(crate) async fn execute(
    ctx: &EvalContext,
    command: OptimizationCommand,
    deps: &Deps<'_>,
    out: &mut dyn Write,
) -> Result<()> {
    let result = match command {
        OptimizationCommand::Run(args) => run(ctx, &args, deps, out).await,
        OptimizationCommand::Show(args) => show(ctx, &args, out).await,
    };
    result.map_err(surface_refusal)
}

/// The driver's refusals verbatim, then the eval layer's.
fn surface_refusal(error: anyhow::Error) -> anyhow::Error {
    match job_refused(&error).map(ToString::to_string) {
        Some(text) => anyhow::anyhow!(text),
        None => crate::commands::eval::surface_refusal(error),
    }
}

fn scripted_proposer(path: &Path) -> Result<ScriptedProposer> {
    let bytes = std::fs::read(path)
        .with_context(|| format!("reading proposer script {}", path.display()))?;
    let proposals: Vec<Proposal> = serde_json::from_slice(&bytes)
        .with_context(|| format!("parsing proposer script {}", path.display()))?;
    Ok(ScriptedProposer::new(
        proposals
            .into_iter()
            .map(|proposal| (proposal.text, proposal.rationale))
            .collect(),
    ))
}

async fn run(
    ctx: &EvalContext,
    args: &OptimizationRunArgs,
    deps: &Deps<'_>,
    out: &mut dyn Write,
) -> Result<()> {
    let proposer_arg = args.proposer.as_ref().context(
        "optimization run needs a proposer: until the LLM proposer lands (M7), pass --proposer scripted:<file>",
    )?;
    let proposer = scripted_proposer(&proposer_arg.script)?;
    let subject =
        resolve_subject_pack(&ctx.home_dir, &args.subject.pack, args.registry.as_deref(), true)
            .await?;
    let baseline_pack = subject
        .directory()
        .context("the subject pack did not resolve to a directory")?
        .to_path_buf();
    let behavior_id = match &args.subject.behavior {
        Some(behavior) => behavior.clone(),
        None => subject.default_behavior()?,
    };
    // Bonferroni: the divisor must be the number of candidates the budget
    // allows, and the driver refuses a policy that disagrees.
    let policy = match &args.policy {
        Some(arg) => load_policy(arg)?,
        None => PolicyV2 {
            max_rounds: args.rounds,
            ..PolicyV2::uncalibrated()
        },
    };
    let job_id = args
        .job_id
        .clone()
        .unwrap_or_else(|| default_id(&args.definition_id));
    let request = JobRequest {
        job_id: job_id.clone(),
        owner: ctx.owner.clone(),
        evaluator_did: ctx.owner.clone(),
        behavior_id,
        definition_id: args.definition_id.clone(),
        inference_profile_id: args.profile.clone().unwrap_or_else(|| {
            default_inference_profile_id_for_behavior(&default_behavior_id_for_agent(&ctx.owner))
        }),
        baseline_pack,
        trials_per_case: args.trials,
        budgets: Budgets {
            max_rounds: args.rounds,
            max_case_trials: args.max_case_trials,
            max_tokens: args.max_tokens,
            deadline_unix_secs: None,
        },
        max_text_bytes: MAX_TARGET_TEXT_BYTES,
        seed_base: args.seed_base,
        jobs_dir: ctx.jobs_dir(),
        runs_dir: ctx.runs_dir(),
        source_commit: source_commit(),
        source_dirty: source_dirty(),
        concurrency: 1,
        max_infra_retries: 1,
        breaker_threshold: default_breaker_threshold(),
        deadline_secs: None,
        run_options: deps.options.clone(),
    };
    let outcome = follow(
        ctx,
        &job_id,
        !args.json,
        out,
        run_job(
            &ctx.access,
            &request,
            deps.executor,
            &proposer,
            deps.registry,
            &policy,
            deps.cancel.clone(),
        ),
    )
    .await?;
    if outcome.state == JobState::Running {
        let note = format!(
            "job {job_id} stopped before it finished; run the same command with --job-id {job_id} to resume it"
        );
        if args.json {
            tracing::warn!("{note}");
        } else {
            writeln!(out, "{note}")?;
        }
    }
    let view = show_job(&ctx.access, &ctx.owner, &job_id).await?;
    if args.json {
        write_json(out, &view)
    } else {
        Ok(render::job_table(&view, out)?)
    }
}

/// Drive the job, printing each journal entry as it lands.
async fn follow<F>(
    ctx: &EvalContext,
    job_id: &str,
    print: bool,
    out: &mut dyn Write,
    running: F,
) -> Result<JobOutcome>
where
    F: std::future::Future<Output = Result<JobOutcome>>,
{
    tokio::pin!(running);
    let mut printed = 0usize;
    let mut ticker = tokio::time::interval(PROGRESS_INTERVAL);
    let result = loop {
        tokio::select! {
            result = &mut running => break result,
            _ = ticker.tick(), if print => print_new_entries(ctx, job_id, &mut printed, out).await,
        }
    };
    if print {
        print_new_entries(ctx, job_id, &mut printed, out).await;
    }
    result
}

async fn print_new_entries(ctx: &EvalContext, job_id: &str, printed: &mut usize, out: &mut dyn Write) {
    let journal = match load_job(&ctx.access, &ctx.owner, job_id).await {
        Ok(Some(job)) => job.journal,
        Ok(None) => return,
        Err(error) => {
            tracing::warn!(job_id, error = %format!("{error:#}"), "could not read the job's journal for progress");
            return;
        }
    };
    for entry in journal.iter().skip(*printed) {
        if let Err(error) = writeln!(out, "{}", render::journal_line(entry)) {
            tracing::warn!(error = %error, "could not write a progress line");
            return;
        }
        *printed += 1;
    }
}

async fn show(ctx: &EvalContext, args: &OptimizationShowArgs, out: &mut dyn Write) -> Result<()> {
    let view = show_job(&ctx.access, &ctx.owner, &args.job_id).await?;
    if args.json {
        write_json(out, &view)
    } else {
        Ok(render::job_table(&view, out)?)
    }
}
```

`crate::commands::eval::testing` must be reachable from this module's tests: it is already `#[cfg(test)] pub(crate) mod testing;` in `commands/eval/mod.rs`. `crate::commands::eval::render` is `pub(crate) mod render;` there as well.

Create `crates/gents-cli/src/commands/optimization/render.rs`:

```rust
//! Plain text for `gents optimization`: one line per journal entry, and the
//! job table `run` and `show` end with.

use std::io::{self, Write};

use gents::optimization::{JobState, JobView, JournalEntry};

use crate::commands::eval::render::{decision_label, signed_percent, wire};

fn round_label(round: Option<u32>) -> String {
    round.map_or_else(|| "final".to_owned(), |round| round.to_string())
}

fn state_label(state: &JobState) -> String {
    match state {
        JobState::Failed { reason } => format!("failed ({reason})"),
        other => other.label().to_owned(),
    }
}

pub(crate) fn journal_line(entry: &JournalEntry) -> String {
    match entry {
        JournalEntry::Frozen => "frozen".to_owned(),
        JournalEntry::RunStarted {
            run_id,
            round,
            split,
        } => format!(
            "round {} run started {run_id} split {}",
            round_label(*round),
            wire(split)
        ),
        JournalEntry::Proposed {
            round,
            rationale,
            candidate_digest,
            ..
        } => format!("round {round} proposed {candidate_digest}: {rationale}"),
        JournalEntry::StructuralReject { round, diagnostics } => {
            format!("round {round} structurally rejected: {diagnostics}")
        }
        JournalEntry::Decided {
            round,
            attempt,
            run_ids,
            mode,
            decision,
            summary,
            ..
        } => format!(
            "round {} attempt {attempt} {} {}: improved {} tied {} worsened {} mean_diff {} p {} alpha {}ppm{} runs {}",
            round_label(*round),
            wire(mode),
            decision_label(decision),
            summary.improved,
            summary.tied,
            summary.worsened,
            signed_percent(summary.mean_diff_bp),
            summary
                .p_ppm
                .map_or_else(|| "-".to_owned(), |p| format!("{p}ppm")),
            summary.alpha_effective_ppm,
            if summary.cost_skipped { " cost skipped" } else { "" },
            run_ids.join(",")
        ),
        JournalEntry::BudgetExhausted { round, reason } => {
            format!("round {} budget exhausted: {reason}", round_label(*round))
        }
        JournalEntry::Finalized { state } => format!("finalized {}", state_label(state)),
        JournalEntry::Promoted {
            by,
            target_digest,
            previous_digest,
            ..
        } => format!("promoted by {by}: target {target_digest} (was {previous_digest})"),
        JournalEntry::PromotionRefused { drifted } => format!(
            "promotion refused: {} documents drifted since the freeze",
            drifted.len()
        ),
        JournalEntry::Reverted { by } => format!("reverted by {by}"),
    }
}

pub(crate) fn job_table(view: &JobView, out: &mut dyn Write) -> io::Result<()> {
    writeln!(
        out,
        "job {} state {} rounds_used {}",
        view.job.job_id,
        state_label(&view.state),
        view.rounds_used
    )?;
    match &view.checkpoint {
        Some(checkpoint) => writeln!(
            out,
            "checkpoint round {} pack {}",
            checkpoint.round, checkpoint.pack_digest
        )?,
        None => writeln!(out, "checkpoint none")?,
    }
    if view.definition_changed {
        writeln!(
            out,
            "the eval definition changed since the job froze it: the recomputations below read a different instrument"
        )?;
    }
    writeln!(out, "journal:")?;
    for (index, entry) in view.job.journal.iter().enumerate() {
        writeln!(out, "{:>4}. {}", index + 1, journal_line(entry))?;
    }
    writeln!(out, "decisions:")?;
    for decision in &view.decisions {
        writeln!(
            out,
            "  round {} attempt {} {} journaled {} recomputed {}{}{} runs {}",
            round_label(decision.round),
            decision.attempt,
            wire(&decision.mode),
            decision_label(&decision.journaled),
            decision_label(&decision.recomputed),
            if decision.mismatch { " MISMATCH" } else { "" },
            if decision.invalidated {
                " (a run it read is invalidated)"
            } else {
                ""
            },
            decision.run_ids.join(",")
        )?;
    }
    Ok(())
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents-cli --lib commands::optimization > "$LOG/t11.log" 2>&1; grep -E "^test |test result|panicked" "$LOG/t11.log" | tail -20`
Expected: both tests pass. The accepted job runs about 60 scripted trials; if the test exceeds a minute, record the time in the ledger (it is not a hang: every trial is scripted).

- [ ] **Step 5: Format and commit**

```bash
cargo fmt --all && cargo fmt --all --check
git add crates/gents-cli/
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" commit -m "feat(cli): gents optimization run with a scripted proposer, and show (#1515)"
```

### Task 12: `optimization promote` and `optimization revert`

**Files:**
- Modify: `crates/gents-cli/src/cli/args.rs` (`OptimizationCommand::{Promote, Revert}`, their `scope()` arms, `OptimizationDigestArgs`)
- Modify: `crates/gents-cli/src/commands/optimization/mod.rs` (two `execute` arms, `promote`, `revert`, `PromoteRefused` in `surface_refusal`, tests)

**Interfaces:**
- Consumes (M6b PR 4, in `<M4-BASE>`; the signatures below are M6b plan PR 4 Task 1's, re-verified in Step 0):
  ```rust
  pub struct Promotion { pub target_digest: String, pub previous_text: String, pub previous_digest: String }
  pub struct PromoteRefused { pub reason: &'static str, pub detail: String }   // Display: "{reason}: {detail}"
  pub fn promote_refused(error: &anyhow::Error) -> Option<&PromoteRefused>;
  pub async fn promote(access: &ConfigAccess, owner: &str, job_id: &str, digest: &str, by: &str) -> Result<Promotion>;
  pub async fn revert(access: &ConfigAccess, owner: &str, job_id: &str, digest: &str, by: &str) -> Result<()>;
  // reason ∈ unknown_job, foreign_did, not_ready, not_promoted, no_checkpoint, wrong_digest, rebuild_mismatch, stale_closure, target_moved
  // promote's `digest` is the checkpoint's pack_digest; revert's is the target_digest its promotion journaled.
  // Task 11: accepted_job, optimization, show_job (gents::optimization::show)
  ```
- Produces:
  ```rust
  pub(crate) struct OptimizationDigestArgs { pub(crate) job_id: String, pub(crate) digest: String, pub(crate) scope: EvalScopeArgs }
  // OptimizationCommand gains Promote(OptimizationDigestArgs), Revert(OptimizationDigestArgs)
  ```
  `revert` takes `--digest` (deviation D11). `by` is the launching home's identity (`EvalContext::owner`), ruling R6.

- [ ] **Step 0: Verify the base**

```bash
git grep -n "pub async fn promote" -- crates/gents/src/optimization/promote.rs
git grep -n "pub async fn revert" -- crates/gents/src/optimization/promote.rs
git grep -n "promote_refused" -- crates/gents/src/optimization/mod.rs
git merge-base --is-ancestor eval/51-cli HEAD && echo stacked-ok
```

Expected: one line from each `git grep`, then `stacked-ok`. Then read `crates/gents/src/optimization/promote.rs` and compare `promote`, `revert`, `Promotion`, `PromoteRefused` (its `Display` as `"{reason}: {detail}"`) and the reason vocabulary with this task's Interfaces block (ruling U11/U12). Any difference is a plan defect: stop and message the orchestrator before writing CLI code.

- [ ] **Step 1: Add the arguments and the failing tests**

In `crates/gents-cli/src/cli/args.rs`, add to `OptimizationCommand`:

```rust
    #[command(about = "Promote a ready job's checkpoint into the live configuration")]
    Promote(OptimizationDigestArgs),
    #[command(about = "Restore the text a promotion replaced; Reverted is final")]
    Revert(OptimizationDigestArgs),
```

to its `scope()`: `Self::Promote(args) | Self::Revert(args) => &args.scope,` and after `OptimizationShowArgs`:

```rust
#[derive(clap::Args)]
pub(crate) struct OptimizationDigestArgs {
    pub(crate) job_id: String,
    /// promote: the checkpoint's pack digest (`optimization show`);
    /// revert: the target digest the promotion printed.
    #[arg(long)]
    pub(crate) digest: String,
    #[command(flatten)]
    pub(crate) scope: EvalScopeArgs,
}
```

Append inside `mod tests` in `crates/gents-cli/src/commands/optimization/mod.rs`:

```rust
    #[tokio::test]
    async fn promote_refuses_a_wrong_digest_then_promotes_and_revert_closes_the_job() {
        let fixture = Fixture::new().await;
        accepted_job(&fixture, "job-1").await;
        let view = gents::optimization::show(&fixture.ctx.access, &fixture.ctx.owner, "job-1")
            .await
            .unwrap();
        let digest = view.checkpoint.expect("an accepted job retains a checkpoint").pack_digest;

        let wrong = optimization(&fixture, &["promote", "job-1", "--digest", "sha256:wrong"])
            .await
            .unwrap_err();
        assert!(wrong.to_string().starts_with("wrong_digest: "), "{wrong:#}");

        let promoted = optimization(&fixture, &["promote", "job-1", "--digest", digest.as_str()])
            .await
            .unwrap();
        assert!(promoted.starts_with("promoted job job-1: the target now digests to "), "{promoted}");
        let target = gents::optimization::show(&fixture.ctx.access, &fixture.ctx.owner, "job-1")
            .await
            .unwrap()
            .job
            .journal
            .iter()
            .rev()
            .find_map(|entry| match entry {
                gents::optimization::JournalEntry::Promoted { target_digest, .. } => {
                    Some(target_digest.clone())
                }
                _ => None,
            })
            .expect("a promotion is journaled");
        assert!(promoted.contains(&format!("--digest {target}")), "{promoted}");

        let again = optimization(&fixture, &["promote", "job-1", "--digest", digest.as_str()])
            .await
            .unwrap_err();
        assert!(again.to_string().starts_with("not_ready: "), "{again:#}");

        let wrong = optimization(&fixture, &["revert", "job-1", "--digest", "sha256:wrong"])
            .await
            .unwrap_err();
        assert!(wrong.to_string().starts_with("wrong_digest: "), "{wrong:#}");
        let reverted = optimization(&fixture, &["revert", "job-1", "--digest", target.as_str()])
            .await
            .unwrap();
        assert_eq!(
            reverted.trim_end(),
            "reverted job job-1; Reverted is final, and a further promotion is a new job"
        );
        let shown = optimization(&fixture, &["show", "job-1"]).await.unwrap();
        assert!(shown.contains("state reverted"), "{shown}");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents-cli --lib commands::optimization > "$LOG/t12.log" 2>&1; grep -E "error(\[|:)|test result" "$LOG/t12.log" | head -20`
Expected: a non-exhaustive `match` error in `execute` (`Promote` and `Revert` not covered).

- [ ] **Step 3: Write the implementation**

In `crates/gents-cli/src/commands/optimization/mod.rs`, add `promote_refused` to the `gents::optimization::{…}` import and `OptimizationDigestArgs` to the `crate::cli::{…}` import; add to `execute`:

```rust
        OptimizationCommand::Promote(args) => promote(ctx, &args, out).await,
        OptimizationCommand::Revert(args) => revert(ctx, &args, out).await,
```

replace `surface_refusal` with:

```rust
/// The driver's and the promotion's refusals verbatim, then the eval layer's.
fn surface_refusal(error: anyhow::Error) -> anyhow::Error {
    let verbatim = job_refused(&error)
        .map(ToString::to_string)
        .or_else(|| promote_refused(&error).map(ToString::to_string));
    match verbatim {
        Some(text) => anyhow::anyhow!(text),
        None => crate::commands::eval::surface_refusal(error),
    }
}
```

and add below `show`:

```rust
async fn promote(
    ctx: &EvalContext,
    args: &OptimizationDigestArgs,
    out: &mut dyn Write,
) -> Result<()> {
    let promotion = gents::optimization::promote(
        &ctx.access,
        &ctx.owner,
        &args.job_id,
        &args.digest,
        &ctx.owner,
    )
    .await?;
    writeln!(
        out,
        "promoted job {}: the target now digests to {} (was {}); `gents optimization revert {} --digest {}` undoes it",
        args.job_id,
        promotion.target_digest,
        promotion.previous_digest,
        args.job_id,
        promotion.target_digest
    )?;
    Ok(())
}

async fn revert(
    ctx: &EvalContext,
    args: &OptimizationDigestArgs,
    out: &mut dyn Write,
) -> Result<()> {
    gents::optimization::revert(
        &ctx.access,
        &ctx.owner,
        &args.job_id,
        &args.digest,
        &ctx.owner,
    )
    .await?;
    writeln!(
        out,
        "reverted job {}; Reverted is final, and a further promotion is a new job",
        args.job_id
    )?;
    Ok(())
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents-cli --lib commands::optimization > "$LOG/t12.log" 2>&1; grep -E "^test |test result|panicked" "$LOG/t12.log" | tail -20`
Expected: three tests pass. A second `promote` is refused `not_ready` because M6b's `job_in_state` (optimization-driver plan, PR 4 Task 1) answers `not_ready` for any state but the expected one when that state is `ReadyToPromote`, and `revert`'s wrong digest is checked after its state check, so it reads `wrong_digest`.

- [ ] **Step 5: Format and commit**

```bash
cargo fmt --all && cargo fmt --all --check
git add crates/gents-cli/
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" commit -m "feat(cli): gents optimization promote and revert print refusals verbatim (#1515)"
```

### Task 12a: `optimization rm` (rulings U1, F2, F8)

**Files:**
- Modify: `crates/gents-cli/src/cli/args.rs` (`OptimizationCommand::Rm`, its `scope()` arm, `OptimizationRmArgs`)
- Modify: `crates/gents-cli/src/commands/optimization/mod.rs` (`rm`, one `execute` arm, tests)

**Interfaces:**
- Consumes:
  ```rust
  pub async fn gents::optimization::load_job(access, owner, job_id) -> Result<Option<JobRecord>>;
  pub fn gents::optimization::derive_state(journal: &[JournalEntry]) -> JobState;
  pub fn gents::optimization::job_dir(jobs_dir: &Path, job_id: &str) -> PathBuf;   // <jobs_dir>/<job_id>
  pub fn gents::optimization::removable(state: &JobState) -> bool;                 // Task 4b: NothingToPromote|Exhausted|Failed|Stale|Promoted|Reverted
  pub async fn gents::optimization::promote(access, owner, job_id, digest: &str, by: &str) -> Result<Promotion>;   // Promotion { target_digest, .. }
  pub async fn gents::optimization::revert(access, owner, job_id, digest: &str, by: &str) -> Result<()>;
  pub struct JobOrigin { pub jobs_dir: PathBuf, .. }                                // ruling R8: the job's own jobs_dir
  pub(crate) fn crate::commands::eval::manage::dir_size(path: &Path) -> Result<u64>;   // Task 8, module pub(crate)
  // Task 11: testing::{optimization, optimization_with, accepted_job, proposer_file}; eval testing::{Fixture, deps, executor, eval, DEFINITION}
  ```
- Produces:
  ```rust
  pub(crate) struct OptimizationRmArgs { pub(crate) job_id: String, pub(crate) force: bool, pub(crate) scope: EvalScopeArgs }
  ```
  `gents optimization rm <job_id> [--force]` deletes `<origin.jobs_dir>/<job_id>/` (baseline copy, candidate packs, staging); the job's documents and runs stay. Without `--force` it refuses a job that `removable` does not accept: `Running`, and `ReadyToPromote` (whose promotion rebuilds from that directory).

- [ ] **Step 1: Add the arguments and the failing tests**

In `crates/gents-cli/src/cli/args.rs`, add to `OptimizationCommand`:

```rust
    #[command(about = "Delete a settled job's directory (candidate packs, staging); its documents stay")]
    Rm(OptimizationRmArgs),
```

to `scope()`: `Self::Rm(args) => &args.scope,` and:

```rust
#[derive(clap::Args)]
pub(crate) struct OptimizationRmArgs {
    pub(crate) job_id: String,
    /// Delete even while the job is running or waiting to be promoted.
    #[arg(long)]
    pub(crate) force: bool,
    #[command(flatten)]
    pub(crate) scope: EvalScopeArgs,
}
```

Append inside `mod tests` in `crates/gents-cli/src/commands/optimization/mod.rs` (adding `use gents::eval::checks::CheckRegistry; use tokio_util::sync::CancellationToken; use super::testing::{optimization_with, proposer_file}; use crate::commands::eval::testing::{deps, eval, executor};`):

```rust
    #[tokio::test]
    async fn rm_keeps_running_and_ready_jobs_and_removes_a_reverted_one() {
        let fixture = Fixture::new().await;
        // What Ctrl-C leaves: a job frozen and still running.
        let pack = fixture.pack_arg();
        let proposer = proposer_file(&fixture);
        let scripted = executor(&[]);
        let registry = CheckRegistry::builtin();
        let interrupted = CancellationToken::new();
        interrupted.cancel();
        optimization_with(
            &fixture,
            &[
                "run", DEFINITION, "--subject", pack.as_str(), "--profile", "local",
                "--proposer", proposer.as_str(), "--job-id", "job-running",
            ],
            &deps(&scripted, &registry, interrupted),
        )
        .await
        .unwrap();
        let running_dir = fixture.ctx.jobs_dir().join("job-running");
        assert!(running_dir.is_dir(), "freeze copied the baseline into the job directory");
        let refused = optimization(&fixture, &["rm", "job-running"]).await.unwrap_err();
        assert_eq!(
            refused.to_string(),
            "job job-running is running: only a job that is nothing_to_promote, exhausted, failed, stale, promoted or reverted is removed without --force"
        );
        let forced = optimization(&fixture, &["rm", "job-running", "--force"]).await.unwrap();
        assert!(forced.contains("bytes reclaimed"), "{forced}");
        assert!(!running_dir.exists());

        // Ruling F2: a job waiting to be promoted keeps its directory.
        accepted_job(&fixture, "job-1").await;
        let kept = optimization(&fixture, &["rm", "job-1"]).await.unwrap_err();
        assert!(kept.to_string().starts_with("job job-1 is ready_to_promote: "), "{kept:#}");

        // Ruling F8: `eval rm` refuses a run a job still needs.
        let held = gents::optimization::held_runs(&fixture.ctx.access, &fixture.ctx.owner)
            .await
            .unwrap();
        let (run_id, _) = held.iter().next().expect("the ready job holds its runs");
        let refused = eval(&fixture, &["rm", run_id.as_str()]).await.unwrap_err();
        assert_eq!(
            refused.to_string(),
            format!(
                "run {run_id} is evidence of optimization job job-1, which is ready_to_promote; pass --force to delete its directory anyway"
            )
        );

        // Promoted, then reverted: removable.
        let view = gents::optimization::show(&fixture.ctx.access, &fixture.ctx.owner, "job-1")
            .await
            .unwrap();
        let digest = view.checkpoint.expect("a retained checkpoint").pack_digest;
        let promotion = gents::optimization::promote(
            &fixture.ctx.access,
            &fixture.ctx.owner,
            "job-1",
            &digest,
            &fixture.ctx.owner,
        )
        .await
        .unwrap();
        gents::optimization::revert(
            &fixture.ctx.access,
            &fixture.ctx.owner,
            "job-1",
            &promotion.target_digest,
            &fixture.ctx.owner,
        )
        .await
        .unwrap();
        let removed = optimization(&fixture, &["rm", "job-1"]).await.unwrap();
        assert!(removed.starts_with("removed "), "{removed}");
        assert!(!fixture.ctx.jobs_dir().join("job-1").exists());
        let shown = optimization(&fixture, &["show", "job-1"]).await.unwrap();
        assert!(shown.contains("state reverted"), "the documents stay: {shown}");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents-cli --lib commands::optimization > "$LOG/t12a.log" 2>&1; grep -E "error(\[|:)|test result" "$LOG/t12a.log" | head`
Expected: a non-exhaustive `match` error in `execute` (`Rm` not covered).

- [ ] **Step 3: Write the implementation**

In `crates/gents-cli/src/commands/optimization/mod.rs`, add `derive_state, job_dir, removable` to the `gents::optimization::{…}` import and `OptimizationRmArgs` to the `crate::cli::{…}` import; add the arm `OptimizationCommand::Rm(args) => rm(ctx, &args, out).await,`; and add:

```rust
async fn rm(ctx: &EvalContext, args: &OptimizationRmArgs, out: &mut dyn Write) -> Result<()> {
    let job = load_job(&ctx.access, &ctx.owner, &args.job_id)
        .await?
        .with_context(|| format!("no optimization job {:?} for {}", args.job_id, ctx.owner))?;
    let dir = job_dir(&job.origin.jobs_dir, &job.job_id);
    anyhow::ensure!(
        dir.is_dir(),
        "job {} has no directory {}",
        job.job_id,
        dir.display()
    );
    let state = derive_state(&job.journal);
    if !args.force && !removable(&state) {
        anyhow::bail!(
            "job {} is {}: only a job that is nothing_to_promote, exhausted, failed, stale, promoted or reverted is removed without --force",
            job.job_id,
            state.label()
        );
    }
    let bytes = crate::commands::eval::manage::dir_size(&dir)?;
    std::fs::remove_dir_all(&dir).with_context(|| format!("removing {}", dir.display()))?;
    tracing::warn!(job_id = %job.job_id, bytes, "optimization job directory removed; its documents stay");
    writeln!(
        out,
        "removed {} ({bytes} bytes reclaimed); the job's documents and runs stay",
        dir.display()
    )?;
    Ok(())
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents-cli --lib commands::optimization > "$LOG/t12a.log" 2>&1; grep -E "^test |test result|panicked" "$LOG/t12a.log" | tail -20`
Expected: four tests pass.

- [ ] **Step 5: Format and commit**

```bash
cargo fmt --all && cargo fmt --all --check
git add crates/gents-cli/
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" commit -m "feat(cli): gents optimization rm deletes a settled job's directory (#1515)"
```

### PR 3 gate

- [ ] Run, one at a time and logged: `CARGO_BUILD_JOBS=4 cargo test -p gents-cli --lib commands`, `CARGO_BUILD_JOBS=4 cargo test -p gents --lib optimization`, `CARGO_BUILD_JOBS=4 cargo check --workspace --all-targets`, `cargo fmt --all --check`. Expected: all pass.

---

## PR 4: `eval watch`, `eval gc` and the size column

Branch `eval/53-watch-gc`, base `eval/52-optimization-cli`. Worktree: `make worktree BRANCH=eval/53-watch-gc DIR=../gents-eval-53-watch-gc BASE=eval/52-optimization-cli`.

### Task 13: `eval watch`

**Files:**
- Modify: `crates/gents-cli/src/cli/args.rs` (`EvalCommand::Watch`, its `scope()` arm, `EvalWatchArgs`, `parse_interval`)
- Modify: `crates/gents-cli/src/commands/eval/mod.rs` (`mod watch;`, one `execute` arm)
- Create: `crates/gents-cli/src/commands/eval/watch.rs`
- Modify: `crates/gents-cli/src/commands/eval/render.rs` (`in_flight`)

**Interfaces:**
- Consumes:
  ```rust
  pub fn gents::eval::runner::read_progress(run_dir: &Path) -> Option<Progress>;      // Task 6
  pub struct gents::eval::runner::Progress { pub slots: BTreeMap<String, InFlight> }  // Task 6
  pub struct gents::eval::runner::InFlight { cell_id, case_id, trial_index, attempt, stage_id: Option<String>, started_at: String, pid: u32, written_at: String }
  pub const gents::eval::runner::PROGRESS_FILE: &str = "progress.json";
  pub fn gents::eval::runner::run_dir(runs_dir: &Path, run_id: &str) -> Result<PathBuf>;
  pub(crate) fn crate::request_helpers::parse_duration_suffix(raw: &str) -> Result<Duration>;   // "2s", "5m", "14d"
  crate::commands::eval::render::report_table(report, out) -> io::Result<()>;
  ```
- Produces:
  ```rust
  pub(crate) fn parse_interval(raw: &str) -> Result<Duration, String>;   // refuses under 250 ms (ruling F13)
  pub(crate) fn parse_age(raw: &str) -> Result<Duration, String>;        // `gc --older-than`; zero allowed
  pub(crate) struct EvalWatchArgs { pub(crate) run_id: String, pub(crate) interval: Duration, pub(crate) once: bool, pub(crate) scope: EvalScopeArgs }
  pub(super) async fn watch::watch(ctx: &EvalContext, args: &EvalWatchArgs, out: &mut dyn Write) -> Result<()>;
  pub(crate) fn render::in_flight(progress: Option<&Progress>, now: chrono::DateTime<chrono::Utc>, stale_after: std::time::Duration, out: &mut dyn Write) -> io::Result<()>;
  pub(crate) fn render::is_stale(slot: &InFlight, stale_after: std::time::Duration) -> bool;   // !runner::is_fresh (ruling F3)
  ```
  Behavior (spec 4b §6): each render is a `--- <UTC time>` line, the `show` table, then the in-flight lines (`in flight: none`, or one line per slot with its stage and elapsed seconds, suffixed `stale: pid <pid> is not running` when its host process is gone (ruling F3, `host_alive`) or `stale: pid <pid> has not refreshed it for <n>s` when its `written_at` is older than three intervals (ruling U4; the loop refreshes live entries at most every `max(marker_poll, 250 ms)`, Task 6a); `--interval` refuses anything under 250 ms (ruling F13), or `no progress file: showing finished slots only`). It returns after one render with `--once`, and otherwise when the run has no planned or abandoned slot and no fresh entry in flight; Ctrl-C ends it (no handler is installed).

- [ ] **Step 1: Add the arguments and the failing tests**

In `crates/gents-cli/src/cli/args.rs`, add to `EvalCommand`:

```rust
    #[command(about = "Re-render a run's report and its in-flight slots until it finishes")]
    Watch(EvalWatchArgs),
```

to `scope()`: `Self::Watch(args) => &args.scope,` and:

```rust
/// `--interval`: at least 250 ms (ruling F13), so a watcher never spins.
pub(crate) fn parse_interval(raw: &str) -> Result<std::time::Duration, String> {
    let interval =
        crate::request_helpers::parse_duration_suffix(raw).map_err(|error| error.to_string())?;
    if interval < std::time::Duration::from_millis(250) {
        return Err(format!("--interval {raw:?} is under 250ms"));
    }
    Ok(interval)
}

/// `--older-than`: any duration, zero included.
pub(crate) fn parse_age(raw: &str) -> Result<std::time::Duration, String> {
    crate::request_helpers::parse_duration_suffix(raw).map_err(|error| error.to_string())
}

#[derive(clap::Args)]
pub(crate) struct EvalWatchArgs {
    pub(crate) run_id: String,
    #[arg(long, value_parser = parse_interval, default_value = "2s")]
    pub(crate) interval: std::time::Duration,
    /// Render once and return.
    #[arg(long)]
    pub(crate) once: bool,
    #[command(flatten)]
    pub(crate) scope: EvalScopeArgs,
}
```

Create `crates/gents-cli/src/commands/eval/watch.rs` with only its tests:

```rust
#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use clap::Parser;
    use gents::eval::runner::{InFlight, Progress, PROGRESS_FILE};
    use tokio_util::sync::CancellationToken;

    use super::super::testing::{eval, executor, row, Fixture};

    #[tokio::test]
    async fn watch_renders_the_report_and_what_is_in_flight() {
        let fixture = Fixture::new().await;
        fixture
            .scripted_run("r1", &executor(&[]), CancellationToken::new())
            .await;
        let run_dir = fixture.ctx.runs_dir().join("r1");

        // A finished run returns after one render even without --once.
        let finished = eval(&fixture, &["watch", "r1", "--interval", "1s"]).await.unwrap();
        assert!(finished.starts_with("--- "), "{finished}");
        assert_eq!(row(&finished, "baseline", 10)[1], "12");
        assert!(finished.contains("in flight: none"), "{finished}");

        let ago = |seconds: i64| {
            (chrono::Utc::now() - chrono::Duration::seconds(seconds))
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        };
        let slot = |case_id: &str, pid: u32, written_at: String| InFlight {
            cell_id: "baseline".into(),
            case_id: case_id.into(),
            trial_index: 0,
            attempt: 2,
            stage_id: Some("check".into()),
            started_at: ago(5),
            pid,
            written_at,
        };
        let progress = Progress {
            slots: BTreeMap::from([
                // This test's own process: alive, and just refreshed (F3).
                ("trial-1".to_owned(), slot("val-a", std::process::id(), ago(0))),
                // Not refreshed for a minute: its host stopped (ruling U4).
                ("trial-2".to_owned(), slot("val-b", 4242, ago(60))),
            ]),
        };
        std::fs::write(
            run_dir.join(PROGRESS_FILE),
            serde_json::to_vec(&progress).unwrap(),
        )
        .unwrap();
        let live = eval(&fixture, &["watch", "r1", "--once"]).await.unwrap();
        let line = live
            .lines()
            .find(|line| line.trim_start().starts_with("baseline val-a #0 attempt 2 stage check for "))
            .unwrap_or_else(|| panic!("{live}"));
        assert!(line.ends_with("(trial-1)"), "{line}");
        let stale = live
            .lines()
            .find(|line| line.trim_start().starts_with("baseline val-b #0"))
            .unwrap_or_else(|| panic!("{live}"));
        assert!(
            stale.contains("(trial-2) stale: pid 4242 "),
            "{stale}"
        );

        assert!(
            crate::cli::Cli::try_parse_from(["gents", "eval", "watch", "r1", "--interval", "0s"])
                .is_err(),
            "a zero interval is refused (F13)"
        );

        std::fs::remove_file(run_dir.join(PROGRESS_FILE)).unwrap();
        let absent = eval(&fixture, &["watch", "r1", "--once"]).await.unwrap();
        assert!(
            absent.contains("no progress file: showing finished slots only"),
            "{absent}"
        );
    }
}
```

In `crates/gents-cli/src/commands/eval/mod.rs` add `mod watch;` (last among the `mod` lines except `testing`) and the `execute` arm `EvalCommand::Watch(args) => watch::watch(ctx, &args, out).await,`.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents-cli --lib commands::eval::watch > "$LOG/t13.log" 2>&1; grep -E "error(\[|:)|test result" "$LOG/t13.log" | head -20`
Expected: compile error `cannot find function `watch` in module `watch``.

- [ ] **Step 3: Write the implementation**

Insert above the test module in `crates/gents-cli/src/commands/eval/watch.rs`:

```rust
//! `watch`: the derived report for finished slots plus `progress.json` for
//! what is in flight (spec 4b §6). One writer (the loop), one reader (this);
//! no daemon, and the trial homes are never opened.

use std::io::Write;

use anyhow::Result;
use gents::eval::report::load_report;
use gents::eval::runner::{read_progress, run_dir};

use super::{render, EvalContext};
use crate::cli::EvalWatchArgs;

pub(super) async fn watch(ctx: &EvalContext, args: &EvalWatchArgs, out: &mut dyn Write) -> Result<()> {
    let dir = run_dir(&ctx.runs_dir(), &args.run_id)?;
    loop {
        let report = load_report(&ctx.access, &ctx.owner, &ctx.runs_dir(), &args.run_id).await?;
        let progress = read_progress(&dir);
        let now = chrono::Utc::now();
        writeln!(
            out,
            "--- {}",
            now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        )?;
        render::report_table(&report, out)?;
        // Rulings U4, F3: stale when its host is gone or it was not
        // refreshed within three intervals.
        let stale_after = args.interval * 3;
        render::in_flight(progress.as_ref(), now, stale_after, out)?;
        out.flush()?;
        let unfinished = report
            .cells
            .iter()
            .any(|cell| cell.counts.planned > 0 || cell.counts.abandoned > 0);
        let in_flight = progress.as_ref().is_some_and(|progress| {
            progress
                .slots
                .values()
                .any(|slot| !render::is_stale(slot, stale_after))
        });
        if args.once || (!unfinished && !in_flight) {
            return Ok(());
        }
        tokio::time::sleep(args.interval).await;
    }
}
```

Append to `crates/gents-cli/src/commands/eval/render.rs` (adding `use gents::eval::runner::{InFlight, Progress};`):

```rust
/// The in-flight lines under a watched report.
/// Seconds since `timestamp`, or `None` when it does not parse.
fn age(timestamp: &str, now: chrono::DateTime<chrono::Utc>) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(timestamp)
        .ok()
        .map(|then| (now - then.with_timezone(&chrono::Utc)).num_seconds().max(0))
}

/// Ruling F3: stale when the host process is gone or the entry was not
/// refreshed within `stale_after` (`gents::eval::runner::is_fresh`).
pub(crate) fn is_stale(slot: &InFlight, stale_after: std::time::Duration) -> bool {
    !gents::eval::runner::is_fresh(slot, stale_after)
}

/// The in-flight lines under a watched report.
/// Seconds since `timestamp`, or `None` when it does not parse.
fn age(timestamp: &str, now: chrono::DateTime<chrono::Utc>) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(timestamp)
        .ok()
        .map(|then| (now - then.with_timezone(&chrono::Utc)).num_seconds().max(0))
}

/// An entry its host stopped refreshing (ruling U4): `written_at` older than
/// `stale_after`, or unreadable. Liveness by pid is not checked: neither
/// `nix` nor `sysinfo` is a dependency of this crate.
pub(crate) fn is_stale(
    slot: &InFlight,
    now: chrono::DateTime<chrono::Utc>,
    stale_after: chrono::Duration,
) -> bool {
    age(&slot.written_at, now).is_none_or(|seconds| seconds > stale_after.num_seconds())
}

/// The in-flight lines under a watched report.
pub(crate) fn in_flight(
    progress: Option<&Progress>,
    now: chrono::DateTime<chrono::Utc>,
    stale_after: std::time::Duration,
    out: &mut dyn Write,
) -> io::Result<()> {
    let Some(progress) = progress else {
        return writeln!(out, "no progress file: showing finished slots only");
    };
    if progress.slots.is_empty() {
        return writeln!(out, "in flight: none");
    }
    writeln!(out, "in flight:")?;
    for (trial_id, slot) in &progress.slots {
        let elapsed = age(&slot.started_at, now)
            .map_or_else(|| "?".to_owned(), |seconds| format!("{seconds}s"));
        let staleness = if !is_stale(slot, stale_after) {
            String::new()
        } else if !gents::eval::runner::host_alive(slot.pid) {
            format!(" stale: pid {} is not running", slot.pid)
        } else {
            format!(
                " stale: pid {} has not refreshed it for {}",
                slot.pid,
                age(&slot.written_at, now)
                    .map_or_else(|| "an unknown time".to_owned(), |seconds| format!("{seconds}s"))
            )
        };
        writeln!(
            out,
            "  {} {} #{} attempt {} stage {} for {} ({trial_id}){staleness}",
            slot.cell_id,
            slot.case_id,
            slot.trial_index,
            slot.attempt,
            slot.stage_id.as_deref().unwrap_or("-"),
            elapsed
        )?;
    }
    Ok(())
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents-cli --lib commands::eval > "$LOG/t13.log" 2>&1; grep -E "^test |test result|panicked" "$LOG/t13.log" | tail -20`
Expected: the watch test passes with the rest.

- [ ] **Step 5: Format and commit**

```bash
cargo fmt --all && cargo fmt --all --check
git add crates/gents-cli/
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" commit -m "feat(cli): gents eval watch reads the report and progress.json (#1515)"
```

### Task 14: `eval gc` and the size column

**Files:**
- Modify: `crates/gents-cli/src/cli/args.rs` (`EvalCommand::Gc`, its `scope()` arm, `EvalGcArgs`)
- Modify: `crates/gents-cli/src/commands/eval/mod.rs` (one `execute` arm)
- Modify: `crates/gents-cli/src/commands/eval/manage.rs` (`gc`, tests)
- Modify: `crates/gents-cli/src/commands/eval/inspect.rs` (`ListRow.size_bytes`, set in `list`)
- Modify: `crates/gents-cli/src/commands/eval/render.rs` (`human_bytes`; a `size` column in `list_table`)

**Interfaces:**
- Consumes:
  ```rust
  // PR 1
  pub async fn gents::optimization::{job_ids, referenced_run_ids}(access, owner);   // Task 4b
  pub fn gents::optimization::removable(state: &JobState) -> bool;                  // Task 4b (ruling F2)
  pub fn gents::optimization::{job_dir, derive_state, load_job};
  pub fn gents::eval::runner::{run_dir, slots_owed, running_elsewhere};            // Tasks 4, 6b (ruling F7)
  pub async fn gents::eval::{load_trials};  gents::eval::report::load_runs;
  // PR 2/3
  crate::commands::eval::manage::dir_size(path: &Path) -> Result<u64>;
  crate::commands::optimization::testing::accepted_job(fixture: &Fixture, job_id: &str) -> String;   // Task 11
  pub(crate) fn crate::cli::parse_age(raw: &str) -> Result<Duration, String>;                         // Task 13
  pub async fn gents::optimization::{promote, revert, show};                                          // test only
  ```
- Produces:
  ```rust
  pub(crate) struct EvalGcArgs { pub(crate) older_than: Duration, pub(crate) definition: Option<String>, pub(crate) dry_run: bool, pub(crate) jobs: bool, pub(crate) scope: EvalScopeArgs }
  pub(super) async fn manage::gc(ctx: &EvalContext, args: &EvalGcArgs, out: &mut dyn Write) -> Result<()>;
  pub(crate) fn render::human_bytes(bytes: u64) -> String;   // "512B", "1.5KiB", "3.0MiB"
  // ListRow gains `pub(crate) size_bytes: Option<u64>`: the run directory's size, `None` when it has none
  ```
  `gc` candidates (spec 4b §7 as ruled): run directory present; `created_at` at or before now minus `--older-than` (default `14d`, zero allowed); not named by any job journal; **finished** by ruling F7 (`slots_owed == 0` and not `running_elsewhere`). It prints each candidate with its size and, without `--dry-run`, deletes the directories and prints bytes reclaimed. It prints `kept <run_id>: …` for an old run it keeps. With `--jobs` (ruling U1) it also removes the directories of jobs `gents::optimization::removable` accepts (ruling F2) whose directory was last modified before the cutoff (a job document has no creation time), and prints `kept job <id>: <state>` for the rest. Nothing deletes on its own; no TTL is stored.

- [ ] **Step 1: Write the failing tests**

Append inside `mod tests` in `crates/gents-cli/src/commands/eval/manage.rs` (adding `use crate::commands::optimization::testing::accepted_job;` to its imports):

```rust
    #[tokio::test]
    async fn gc_removes_only_old_finished_unreferenced_run_directories() {
        let fixture = Fixture::new().await;
        fixture
            .scripted_run("r-done", &executor(&[]), CancellationToken::new())
            .await;
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        fixture
            .scripted_run("r-stopped", &executor(&[]), cancelled)
            .await;
        accepted_job(&fixture, "job-1").await;
        let referenced =
            gents::optimization::referenced_run_ids(&fixture.ctx.access, &fixture.ctx.owner)
                .await
                .unwrap();
        assert!(!referenced.is_empty());

        let young = eval(&fixture, &["gc"]).await.unwrap();
        assert_eq!(young.trim_end(), "reclaimed 0B from 0 runs; their documents stay");

        let dry = eval(&fixture, &["gc", "--older-than", "0s", "--dry-run"])
            .await
            .unwrap();
        assert!(dry.contains("would remove r-done "), "{dry}");
        assert!(dry.contains("kept r-stopped: unfinished (it still owes 24 slots)"), "{dry}");
        for run_id in &referenced {
            assert!(
                dry.contains(&format!("kept {run_id}: an optimization job references it")),
                "{dry}"
            );
        }
        assert!(dry.contains("dry run: 1 runs, "), "{dry}");
        assert!(fixture.ctx.runs_dir().join("r-done").is_dir());

        let other = eval(
            &fixture,
            &["gc", "--older-than", "0s", "--definition", "other", "--dry-run"],
        )
        .await
        .unwrap();
        assert!(other.starts_with("dry run: 0 runs"), "{other}");

        let removed = eval(&fixture, &["gc", "--older-than", "0s"]).await.unwrap();
        assert!(removed.contains(" from 1 runs; their documents stay"), "{removed}");
        assert!(!fixture.ctx.runs_dir().join("r-done").exists());
        assert!(fixture.ctx.runs_dir().join("r-stopped").is_dir());
        for run_id in &referenced {
            assert!(fixture.ctx.runs_dir().join(run_id).is_dir(), "{run_id}");
        }
        assert!(
            gents::eval::load_run(&fixture.ctx.access, &fixture.ctx.owner, "r-done")
                .await
                .unwrap()
                .is_some(),
            "documents stay"
        );
        assert!(fixture.ctx.jobs_dir().join("job-1").is_dir(), "no --jobs, no job");

        // Ruling F2: a job waiting to be promoted keeps its directory.
        let kept = eval(&fixture, &["gc", "--older-than", "0s", "--jobs", "--dry-run"])
            .await
            .unwrap();
        assert!(kept.contains("kept job job-1: ready_to_promote"), "{kept}");

        // Promoted, then reverted: collected.
        let view = gents::optimization::show(&fixture.ctx.access, &fixture.ctx.owner, "job-1")
            .await
            .unwrap();
        let digest = view.checkpoint.expect("a retained checkpoint").pack_digest;
        let promotion = gents::optimization::promote(
            &fixture.ctx.access,
            &fixture.ctx.owner,
            "job-1",
            &digest,
            &fixture.ctx.owner,
        )
        .await
        .unwrap();
        gents::optimization::revert(
            &fixture.ctx.access,
            &fixture.ctx.owner,
            "job-1",
            &promotion.target_digest,
            &fixture.ctx.owner,
        )
        .await
        .unwrap();
        let jobs = eval(&fixture, &["gc", "--older-than", "0s", "--jobs", "--dry-run"])
            .await
            .unwrap();
        assert!(jobs.contains("would remove job job-1 "), "{jobs}");
        eval(&fixture, &["gc", "--older-than", "0s", "--jobs"]).await.unwrap();
        assert!(!fixture.ctx.jobs_dir().join("job-1").exists());
        for run_id in &referenced {
            assert!(fixture.ctx.runs_dir().join(run_id).is_dir(), "{run_id}");
        }
    }

    #[tokio::test]
    async fn list_shows_each_runs_size_on_disk() {
        let fixture = Fixture::new().await;
        fixture
            .scripted_run("r1", &executor(&[]), CancellationToken::new())
            .await;
        let size = |listed: &str| {
            listed
                .lines()
                .find(|line| line.starts_with("r1 "))
                .and_then(|line| line.split_whitespace().nth(6))
                .map(str::to_owned)
                .unwrap_or_else(|| panic!("{listed}"))
        };
        let before = size(&eval(&fixture, &["list"]).await.unwrap());
        assert!(before.ends_with('B') && before != "-", "{before}");
        eval(&fixture, &["rm", "r1"]).await.unwrap();
        assert_eq!(size(&eval(&fixture, &["list"]).await.unwrap()), "-");
    }
```

In `crates/gents-cli/src/cli/args.rs`, add to `EvalCommand`:

```rust
    #[command(about = "Delete the directories of old, finished runs no optimization job references")]
    Gc(EvalGcArgs),
```

to `scope()`: `Self::Gc(args) => &args.scope,` and:

```rust
#[derive(clap::Args)]
pub(crate) struct EvalGcArgs {
    #[arg(long, value_parser = parse_age, default_value = "14d")]
    pub(crate) older_than: std::time::Duration,
    #[arg(long)]
    pub(crate) definition: Option<String>,
    /// List what would be removed; remove nothing.
    #[arg(long)]
    pub(crate) dry_run: bool,
    /// Also remove the directories of settled optimization jobs older than
    /// the threshold (ruling U1).
    #[arg(long)]
    pub(crate) jobs: bool,
    #[command(flatten)]
    pub(crate) scope: EvalScopeArgs,
}
```

In `crates/gents-cli/src/commands/eval/mod.rs` add the arm `EvalCommand::Gc(args) => manage::gc(ctx, &args, out).await,`.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents-cli --lib commands::eval > "$LOG/t14.log" 2>&1; grep -E "error(\[|:)|test result" "$LOG/t14.log" | head`
Expected: compile errors: `manage::gc` and `ListRow.size_bytes` not found.

- [ ] **Step 3: Write the implementation**

Append to `crates/gents-cli/src/commands/eval/manage.rs` above its tests (extending its imports with `use std::path::PathBuf;`, `use gents::eval::report::load_runs;`, `use super::render::human_bytes;` and `EvalGcArgs` in the `crate::cli` import; `load_trials`, `slots_owed`, `running_elsewhere` and `run_dir` are already imported by Task 8):

```rust
pub(super) async fn gc(ctx: &EvalContext, args: &EvalGcArgs, out: &mut dyn Write) -> Result<()> {
    let cutoff = chrono::Utc::now()
        - chrono::Duration::from_std(args.older_than).context("--older-than is too large")?;
    let referenced = gents::optimization::referenced_run_ids(&ctx.access, &ctx.owner).await?;
    let runs = load_runs(&ctx.access, &ctx.owner).await?;
    let mut doomed: Vec<(PathBuf, u64)> = Vec::new();
    for record in &runs {
        if args
            .definition
            .as_deref()
            .is_some_and(|id| id != record.origin.definition.definition_id)
        {
            continue;
        }
        let dir = run_dir(&ctx.runs_dir(), &record.run_id)?;
        if !dir.is_dir() {
            continue;
        }
        let created = match chrono::DateTime::parse_from_rfc3339(&record.created_at) {
            Ok(created) => created.with_timezone(&chrono::Utc),
            Err(error) => {
                writeln!(
                    out,
                    "kept {}: its created_at {:?} does not parse ({error})",
                    record.run_id, record.created_at
                )?;
                continue;
            }
        };
        if created > cutoff {
            continue;
        }
        if referenced.contains(&record.run_id) {
            writeln!(out, "kept {}: an optimization job references it", record.run_id)?;
            continue;
        }
        // Ruling F7: finished means nothing owed and no live process on it.
        let trials = load_trials(&ctx.access, &ctx.owner, &record.run_id).await?;
        let owed = slots_owed(record, &trials);
        if owed > 0 {
            writeln!(
                out,
                "kept {}: unfinished (it still owes {owed} slots)",
                record.run_id
            )?;
            continue;
        }
        if running_elsewhere(&dir) {
            writeln!(out, "kept {}: a live process is running it", record.run_id)?;
            continue;
        }
        let size = dir_size(&dir)?;
        writeln!(
            out,
            "{} {} {} created {}",
            if args.dry_run { "would remove" } else { "removing" },
            record.run_id,
            human_bytes(size),
            record.created_at
        )?;
        doomed.push((dir, size));
    }
    if args.jobs {
        for job_id in gents::optimization::job_ids(&ctx.access, &ctx.owner).await? {
            let Some(job) = gents::optimization::load_job(&ctx.access, &ctx.owner, &job_id).await?
            else {
                continue;
            };
            let dir = gents::optimization::job_dir(&job.origin.jobs_dir, &job.job_id);
            if !dir.is_dir() {
                continue;
            }
            // A job has no creation time of its own; its directory's does.
            let modified: chrono::DateTime<chrono::Utc> = std::fs::metadata(&dir)
                .and_then(|metadata| metadata.modified())
                .with_context(|| format!("reading {}", dir.display()))?
                .into();
            if modified > cutoff {
                continue;
            }
            let state = gents::optimization::derive_state(&job.journal);
            if !gents::optimization::removable(&state) {
                writeln!(out, "kept job {}: {}", job.job_id, state.label())?;
                continue;
            }
            let size = dir_size(&dir)?;
            writeln!(
                out,
                "{} job {} {}",
                if args.dry_run { "would remove" } else { "removing" },
                job.job_id,
                human_bytes(size)
            )?;
            doomed.push((dir, size));
        }
    }
    let total: u64 = doomed.iter().map(|(_, size)| size).sum();
    if args.dry_run {
        writeln!(
            out,
            "dry run: {} runs, {} would be reclaimed",
            doomed.len(),
            human_bytes(total)
        )?;
        return Ok(());
    }
    for (dir, _) in &doomed {
        std::fs::remove_dir_all(dir).with_context(|| format!("removing {}", dir.display()))?;
    }
    tracing::warn!(
        runs = doomed.len(),
        bytes = total,
        "eval gc removed run directories; their documents stay"
    );
    writeln!(
        out,
        "reclaimed {} from {} runs; their documents stay",
        human_bytes(total),
        doomed.len()
    )?;
    Ok(())
}
```

Append to `crates/gents-cli/src/commands/eval/render.rs`:

```rust
/// A byte count for a person: `512B`, `1.5KiB`, `3.0MiB`.
pub(crate) fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KiB", "MiB", "GiB", "TiB"];
    if bytes < 1024 {
        return format!("{bytes}B");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1}{}", UNITS[unit])
}
```

and in `list_table` add a `size` column after `invalidated`: the header format becomes `"{:<28} {:<24} {:<11} {:<24} {:<21} {:<11} {:<9} cells"` with `"size"` after `"invalidated"`, and the row format the same with `row.size_bytes.map_or_else(|| "-".to_owned(), human_bytes)` after the invalidated cell.

In `crates/gents-cli/src/commands/eval/inspect.rs`, add to `ListRow` (after `invalidated`):

```rust
    /// The run directory's size; `None` once `rm` or `gc` removed it.
    pub(crate) size_bytes: Option<u64>,
```

and in `list`, when building each row:

```rust
            size_bytes: gents::eval::runner::run_dir(&ctx.runs_dir(), &record.run_id)
                .ok()
                .filter(|dir| dir.is_dir())
                .and_then(|dir| super::manage::dir_size(&dir).ok()),
```

(`manage` must be visible to `inspect`: both are children of `eval`, so `super::manage::dir_size` resolves; `dir_size` is `pub(crate)`.)

- [ ] **Step 4: Run the tests to verify they pass**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents-cli --lib commands::eval > "$LOG/t14-cli.log" 2>&1; grep -E "^test |test result|panicked" "$LOG/t14-cli.log" | tail -30`.
Expected: both new CLI tests and every earlier `commands::eval` test pass (the Task 7 list test reads columns by content, not position, so the size column does not disturb it).

- [ ] **Step 5: Format and commit**

```bash
cargo fmt --all && cargo fmt --all --check
git add crates/gents-cli/
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" commit -m "feat(cli): gents eval gc keeps job-referenced and unfinished runs; eval list shows size on disk (#1515)"
```

### PR 4 gate

- [ ] Run, one at a time and logged: `CARGO_BUILD_JOBS=4 cargo test -p gents --lib optimization`, `CARGO_BUILD_JOBS=4 cargo test -p gents-cli --lib commands`, `CARGO_BUILD_JOBS=4 cargo check --workspace --all-targets`, `cargo fmt --all --check`. Expected: all pass.

---

## PR 5: Compare breakdowns

Branch `eval/54-compare-breakdowns`, base `eval/53-watch-gc`. Worktree: `make worktree BRANCH=eval/54-compare-breakdowns DIR=../gents-eval-54-compare-breakdowns BASE=eval/53-watch-gc`.

### Task 15: Per-verdict data in the report, and `by_check`, `by_stage`, `case_view`

**Files:**
- Modify: `crates/gents/src/eval/report/build.rs` (`SlotVerdict`; `SlotReport.verdicts`; `Counted` carries them; a test)
- Modify: `crates/gents/src/eval/report/compare.rs` (`SideTrial`, `PairedTrial`; `Comparison.trials`)
- Create: `crates/gents/src/eval/report/breakdown.rs`
- Modify: `crates/gents/src/eval/report/mod.rs` (`pub mod breakdown;` first among the `mod` lines; the `pub use` lines)

**Interfaces:**
- Consumes:
  ```rust
  // Task 1
  fn cell_report(definition, rows: &RunRows, cell: &CellSpec, trials_per_case: u32) -> CellReport;   // builds `counted` from counted_slots
  struct Counted<'a> { score: CaseTrialScore, all_pass: bool, record: &'a TrialRecord }
  pub struct SlotReport { case_id, trial_index, class, attempts, score_bp, counted: SlotScore, latest }
  // Task 2
  pub fn compare(baseline: &EvalReport, candidate: &EvalReport, baseline_cell: &str, candidate_cell: &str) -> Result<Comparison>;
  pub struct CaseComparison { case_id, pairs: u64, baseline_mean_bp, candidate_mean_bp, diff_bp: Option<i64> }
  // VerdictView (frozen scoring) carries verdict_id, stage_index, check, tier, kind, provider_reason, score_bp, weight, regrade_of — no stage_id, no raw
  ```
- Produces:
  ```rust
  pub struct SlotVerdict { pub verdict_id: String, pub stage_id: String, pub check: String, pub tier: EvalTier, pub kind: OutcomeKind,
      pub provider_reason: Option<ProviderReason>, pub score_bp: Option<u32>, pub weight: u32, pub reason_code: Option<String> }
  // SlotReport gains `pub verdicts: Vec<SlotVerdict>`: the counted attempt's verdicts after regrade supersession, in (stage, check) order
  pub struct SideTrial { pub class: SlotClass, pub score: SlotScore, pub verdicts: Vec<SlotVerdict> }
  pub struct PairedTrial { pub case_id: String, pub trial_index: u32, pub baseline: Option<SideTrial>, pub candidate: Option<SideTrial> }
  // Comparison gains `pub trials: Vec<PairedTrial>`: every (case, trial_index) either cell has, in that order
  pub struct CheckDiff { pub check: String, pub cases: u32, pub pairs: u64, pub mean_diff_bp: Option<i64>, pub improved: u32, pub tied: u32, pub worsened: u32 }
  pub struct StageDiff { pub stage_id: String, pub cases: u32, pub pairs: u64, pub mean_diff_bp: Option<i64>, pub improved: u32, pub tied: u32, pub worsened: u32 }
  pub struct CaseView { pub case_id: String, pub summary: CaseComparison, pub trials: Vec<PairedTrial> }
  pub fn by_check(comparison: &Comparison) -> Vec<CheckDiff>;       // sorted by check
  pub fn by_stage(comparison: &Comparison) -> Vec<StageDiff>;       // sorted by stage_id
  pub fn case_view(comparison: &Comparison, case_id: &str) -> Result<CaseView>;   // ReportRefused "the comparison has no case {case_id:?}"
  ```
  All derive `Clone, Debug, PartialEq, Eq, Serialize`. Rule (D15): a key enters a breakdown only when `pair_trials` pairs it (both sides `Scored` or `Unknown`); per group, each side's score is the weighted mean of its acceptance verdicts that carry weight and a `score_bp`; a group present on one side only is left out; per-case means are truncating integer divisions, as in `eval::scoring`.

- [ ] **Step 1: Write the failing tests**

Append inside `mod tests` in `crates/gents/src/eval/report/build.rs`:

```rust
    #[test]
    fn a_slot_carries_the_verdicts_that_count_for_it() {
        let definition = definition(&["disk"]);
        let run = record(RUN, &definition, &["base"], 2);
        let mut rows = Rows::default().add(RUN, at("base", "disk", 0, 1), fail());
        let original = rows.verdicts[0].clone();
        rows.verdicts.push(VerdictRecord {
            verdict_id: "regrade".into(),
            kind: OutcomeKind::Passed,
            score_bp: Some(10_000),
            regrade_of: Some(original.verdict_id.clone()),
            ..original
        });
        let report = build(&run, &rows.trials, &rows.verdicts, &definition, &[]).unwrap();
        let verdicts = &report.cells[0].slots[0].verdicts;
        assert_eq!(
            verdicts
                .iter()
                .map(|verdict| (verdict.verdict_id.as_str(), verdict.kind, verdict.reason_code.as_deref()))
                .collect::<Vec<_>>(),
            vec![("regrade", OutcomeKind::Passed, Some("fixture"))],
            "the superseded row is not counted"
        );
        assert_eq!(verdicts[0].stage_id, "check");
        assert!(report.cells[0].slots[1].verdicts.is_empty(), "a planned slot has none");
    }
```

Create `crates/gents/src/eval/report/breakdown.rs` with only:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::report::build::{build, SlotClass};
    use crate::eval::report::compare::compare;
    use crate::eval::report::fixtures::{at, definition, fail, pass, record, trial_id, Outcome, Rows, RUN};
    use crate::eval::report::report_refused;
    use crate::eval::{OutcomeKind, VerdictRecord};

    fn comparison_of(cases: &[&str], rows: &Rows) -> Comparison {
        let definition = definition(cases);
        let run = record(RUN, &definition, &["baseline", "candidate"], 1);
        let report = build(&run, &rows.trials, &rows.verdicts, &definition, &[]).unwrap();
        compare(&report, &report, "baseline", "candidate").unwrap()
    }

    /// Case a: `captured_rows_count` improves and `second` ties at 10000.
    /// Case b: `captured_rows_count` ties.
    fn two_checks() -> Comparison {
        let mut rows = Rows::default()
            .add(RUN, at("baseline", "a", 0, 1), fail())
            .add(RUN, at("candidate", "a", 0, 1), pass())
            .add(RUN, at("baseline", "b", 0, 1), pass())
            .add(RUN, at("candidate", "b", 0, 1), pass());
        for cell in ["baseline", "candidate"] {
            let trial = trial_id(RUN, at(cell, "a", 0, 1));
            let template = rows
                .verdicts
                .iter()
                .find(|verdict| verdict.trial_id == trial)
                .unwrap()
                .clone();
            rows.verdicts.push(VerdictRecord {
                verdict_id: format!("{trial}-second"),
                check: "second".into(),
                kind: OutcomeKind::Passed,
                score_bp: Some(10_000),
                ..template
            });
        }
        comparison_of(&["a", "b"], &rows)
    }

    #[test]
    fn by_check_aggregates_the_paired_difference_per_check_across_cases() {
        assert_eq!(
            by_check(&two_checks()),
            vec![
                CheckDiff {
                    check: "captured_rows_count".into(),
                    cases: 2,
                    pairs: 2,
                    mean_diff_bp: Some(5_000),
                    improved: 1,
                    tied: 1,
                    worsened: 0,
                },
                CheckDiff {
                    check: "second".into(),
                    cases: 1,
                    pairs: 1,
                    mean_diff_bp: Some(0),
                    improved: 0,
                    tied: 1,
                    worsened: 0,
                },
            ]
        );
    }

    #[test]
    fn by_stage_compares_each_stages_weighted_mean() {
        // Case a: baseline (0 + 10000) / 2 against candidate 10000; case b ties.
        assert_eq!(
            by_stage(&two_checks()),
            vec![StageDiff {
                stage_id: "check".into(),
                cases: 2,
                pairs: 2,
                mean_diff_bp: Some(2_500),
                improved: 1,
                tied: 1,
                worsened: 0,
            }]
        );
    }

    #[test]
    fn a_side_that_is_not_evidence_is_left_out_as_pair_trials_leaves_it_out() {
        let rows = Rows::default()
            .add(
                RUN,
                at("baseline", "a", 0, 1),
                Outcome::Verdict(OutcomeKind::Infrastructure, None),
            )
            .add(RUN, at("candidate", "a", 0, 1), pass());
        let comparison = comparison_of(&["a"], &rows);
        assert!(by_check(&comparison).is_empty());
        assert!(by_stage(&comparison).is_empty());
    }

    #[test]
    fn case_view_shows_both_sides_and_refuses_an_unknown_case() {
        let comparison = two_checks();
        let view = case_view(&comparison, "a").unwrap();
        assert_eq!(view.summary.diff_bp, Some(5_000));
        assert_eq!(view.trials.len(), 1);
        let baseline = view.trials[0].baseline.as_ref().unwrap();
        let candidate = view.trials[0].candidate.as_ref().unwrap();
        assert_eq!((baseline.class, candidate.class), (SlotClass::Fail, SlotClass::Pass));
        assert_eq!(
            baseline
                .verdicts
                .iter()
                .map(|verdict| (verdict.check.as_str(), verdict.score_bp))
                .collect::<Vec<_>>(),
            vec![("captured_rows_count", Some(0)), ("second", Some(10_000))]
        );
        let error = case_view(&comparison, "zzz").unwrap_err();
        assert_eq!(
            report_refused(&error).expect("a ReportRefused").0,
            "the comparison has no case \"zzz\""
        );
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents --lib eval::report > "$LOG/t15.log" 2>&1; grep -E "error(\[|:)|test result" "$LOG/t15.log" | head -20` (after adding `pub mod breakdown;` to `report/mod.rs`)
Expected: compile errors: no field `verdicts` on `SlotReport`; `by_check`, `by_stage`, `case_view`, `CheckDiff`, `StageDiff` not found.

- [ ] **Step 3: Write the implementation**

In `crates/gents/src/eval/report/build.rs`:

1. Extend the imports: `use crate::eval::{…, OutcomeKind, ProviderReason, …};` (add both to the existing `crate::eval` list).
2. Add after `SlotReport`:

```rust
/// One verdict that counts for a slot: of its counted attempt, after regrade
/// supersession.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SlotVerdict {
    pub verdict_id: String,
    pub stage_id: String,
    pub check: String,
    pub tier: EvalTier,
    pub kind: OutcomeKind,
    pub provider_reason: Option<ProviderReason>,
    pub score_bp: Option<u32>,
    pub weight: u32,
    /// `raw.reason_code`, the check's own contract.
    pub reason_code: Option<String>,
}

fn slot_verdict(record: &VerdictRecord) -> SlotVerdict {
    SlotVerdict {
        verdict_id: record.verdict_id.clone(),
        stage_id: record.stage_id.clone(),
        check: record.check.clone(),
        tier: record.tier,
        kind: record.kind,
        provider_reason: record.provider_reason,
        score_bp: record.score_bp,
        weight: record.weight,
        reason_code: record
            .raw
            .get("reason_code")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
    }
}
```

3. Add the field to `SlotReport`, after `counted`:

```rust
    /// The counted attempt's verdicts, in (stage, check) order; empty when
    /// no attempt completed.
    pub verdicts: Vec<SlotVerdict>,
```

4. Add `verdicts: Vec<SlotVerdict>,` to `struct Counted`. In `cell_report`, before `let counted`, add

```rust
    let by_id: BTreeMap<&str, &VerdictRecord> = rows
        .verdicts
        .iter()
        .map(|verdict| (verdict.verdict_id.as_str(), verdict))
        .collect();
```

   and in the `Counted { … }` constructor add

```rust
                        verdicts: views
                            .iter()
                            .filter_map(|view| by_id.get(view.verdict_id.as_str()))
                            .map(|record| slot_verdict(record))
                            .collect(),
```

   (`views` is only borrowed here and by `case_trial_score`, so the order of the two fields does not matter). In the `SlotReport { … }` constructor add

```rust
                verdicts: counted_here
                    .map(|counted_here| counted_here.verdicts.clone())
                    .unwrap_or_default(),
```

In `crates/gents/src/eval/report/compare.rs`:

1. Import `use crate::eval::report::build::{CellReport, EvalReport, SlotClass, SlotReport, SlotScore, SlotVerdict};`.
2. Add:

```rust
/// One cell's side of a trial key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SideTrial {
    pub class: SlotClass,
    pub score: SlotScore,
    pub verdicts: Vec<SlotVerdict>,
}

/// Both cells at one `(case_id, trial_index)`, as the breakdowns read them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PairedTrial {
    pub case_id: String,
    pub trial_index: u32,
    pub baseline: Option<SideTrial>,
    pub candidate: Option<SideTrial>,
}

fn paired_trials(baseline: &CellReport, candidate: &CellReport) -> Vec<PairedTrial> {
    let side = |slot: &SlotReport| SideTrial {
        class: slot.class,
        score: slot.counted,
        verdicts: slot.verdicts.clone(),
    };
    let mut trials: BTreeMap<(String, u32), PairedTrial> = BTreeMap::new();
    for slot in &baseline.slots {
        trials
            .entry((slot.case_id.clone(), slot.trial_index))
            .or_insert_with(|| PairedTrial {
                case_id: slot.case_id.clone(),
                trial_index: slot.trial_index,
                baseline: None,
                candidate: None,
            })
            .baseline = Some(side(slot));
    }
    for slot in &candidate.slots {
        trials
            .entry((slot.case_id.clone(), slot.trial_index))
            .or_insert_with(|| PairedTrial {
                case_id: slot.case_id.clone(),
                trial_index: slot.trial_index,
                baseline: None,
                candidate: None,
            })
            .candidate = Some(side(slot));
    }
    trials.into_values().collect()
}
```

3. Add `pub trials: Vec<PairedTrial>,` to `Comparison` after `policy`, and `trials: paired_trials(base, cand),` to its constructor in `compare`.

Create the body of `crates/gents/src/eval/report/breakdown.rs` above its tests:

```rust
//! Compare breakdowns (spec 4b §8): which check or stage a candidate moved,
//! and one case's trials side by side. Pure functions over a `Comparison`;
//! no new serialization.

use std::collections::BTreeMap;

use anyhow::Result;
use serde::Serialize;

use crate::document_config::EvalTier;
use crate::eval::report::build::{SlotScore, SlotVerdict};
use crate::eval::report::compare::{CaseComparison, Comparison, PairedTrial};
use crate::eval::report::refused;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CheckDiff {
    pub check: String,
    pub cases: u32,
    pub pairs: u64,
    pub mean_diff_bp: Option<i64>,
    pub improved: u32,
    pub tied: u32,
    pub worsened: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct StageDiff {
    pub stage_id: String,
    pub cases: u32,
    pub pairs: u64,
    pub mean_diff_bp: Option<i64>,
    pub improved: u32,
    pub tied: u32,
    pub worsened: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CaseView {
    pub case_id: String,
    pub summary: CaseComparison,
    pub trials: Vec<PairedTrial>,
}

pub fn by_check(comparison: &Comparison) -> Vec<CheckDiff> {
    tally(comparison, |verdict| verdict.check.clone())
        .into_iter()
        .map(|(check, tally)| CheckDiff {
            check,
            cases: tally.cases,
            pairs: tally.pairs,
            mean_diff_bp: tally.mean_diff_bp,
            improved: tally.improved,
            tied: tally.tied,
            worsened: tally.worsened,
        })
        .collect()
}

pub fn by_stage(comparison: &Comparison) -> Vec<StageDiff> {
    tally(comparison, |verdict| verdict.stage_id.clone())
        .into_iter()
        .map(|(stage_id, tally)| StageDiff {
            stage_id,
            cases: tally.cases,
            pairs: tally.pairs,
            mean_diff_bp: tally.mean_diff_bp,
            improved: tally.improved,
            tied: tally.tied,
            worsened: tally.worsened,
        })
        .collect()
}

pub fn case_view(comparison: &Comparison, case_id: &str) -> Result<CaseView> {
    let summary = comparison
        .cases
        .iter()
        .find(|case| case.case_id == case_id)
        .cloned()
        .ok_or_else(|| refused(format!("the comparison has no case {case_id:?}")))?;
    Ok(CaseView {
        case_id: case_id.to_owned(),
        summary,
        trials: comparison
            .trials
            .iter()
            .filter(|trial| trial.case_id == case_id)
            .cloned()
            .collect(),
    })
}

struct Tally {
    cases: u32,
    pairs: u64,
    mean_diff_bp: Option<i64>,
    improved: u32,
    tied: u32,
    worsened: u32,
}

/// Per group: the per-case mean paired difference, then the mean over cases.
fn tally(comparison: &Comparison, group: impl Fn(&SlotVerdict) -> String) -> BTreeMap<String, Tally> {
    // (group, case) -> (sum of candidate-minus-baseline, pairs)
    let mut per_case: BTreeMap<(String, String), (i64, u64)> = BTreeMap::new();
    for trial in &comparison.trials {
        let (Some(baseline), Some(candidate)) = (&trial.baseline, &trial.candidate) else {
            continue;
        };
        if !paired(baseline.score) || !paired(candidate.score) {
            continue;
        }
        let candidate_groups = group_scores(&candidate.verdicts, &group);
        for (name, baseline_bp) in group_scores(&baseline.verdicts, &group) {
            if let Some(candidate_bp) = candidate_groups.get(&name) {
                let entry = per_case
                    .entry((name, trial.case_id.clone()))
                    .or_default();
                entry.0 += i64::from(*candidate_bp) - i64::from(baseline_bp);
                entry.1 += 1;
            }
        }
    }
    let mut groups: BTreeMap<String, (Vec<i64>, u64)> = BTreeMap::new();
    for ((name, _case), (sum, pairs)) in per_case {
        let entry = groups.entry(name).or_default();
        entry.0.push(sum / pairs as i64);
        entry.1 += pairs;
    }
    groups
        .into_iter()
        .map(|(name, (means, pairs))| {
            let count = |keep: fn(i64) -> bool| means.iter().filter(|mean| keep(**mean)).count() as u32;
            let cases = means.len() as i64;
            let tally = Tally {
                cases: means.len() as u32,
                pairs,
                mean_diff_bp: (cases > 0).then(|| means.iter().sum::<i64>() / cases),
                improved: count(|mean| mean > 0),
                tied: count(|mean| mean == 0),
                worsened: count(|mean| mean < 0),
            };
            (name, tally)
        })
        .collect()
}

/// A key `pair_trials` pairs: both sides scored or unknown.
fn paired(score: SlotScore) -> bool {
    matches!(score, SlotScore::Scored(_) | SlotScore::Unknown)
}

/// Per group, the weight-weighted mean of the acceptance verdicts that carry
/// weight and a score, truncated.
fn group_scores(
    verdicts: &[SlotVerdict],
    group: &impl Fn(&SlotVerdict) -> String,
) -> BTreeMap<String, u32> {
    let mut sums: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    for verdict in verdicts
        .iter()
        .filter(|verdict| verdict.tier == EvalTier::Acceptance && verdict.weight > 0)
    {
        let Some(score) = verdict.score_bp else {
            continue;
        };
        let entry = sums.entry(group(verdict)).or_default();
        entry.0 += u64::from(score) * u64::from(verdict.weight);
        entry.1 += u64::from(verdict.weight);
    }
    sums.into_iter()
        .map(|(name, (total, weight))| (name, (total / weight) as u32))
        .collect()
}
```

In `crates/gents/src/eval/report/mod.rs`: add `pub mod breakdown;` before `pub mod build;`, extend the `build` re-export with `SlotVerdict`, the `compare` re-export with `PairedTrial, SideTrial`, and add:

```rust
pub use breakdown::{by_check, by_stage, case_view, CaseView, CheckDiff, StageDiff};
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents --lib eval::report > "$LOG/t15.log" 2>&1; grep -E "^test |test result|panicked" "$LOG/t15.log" | tail -40`
Expected: the new build test and four breakdown tests pass; every earlier report test still passes.

- [ ] **Step 5: Format and commit**

```bash
cargo fmt --all && cargo fmt --all --check
git add crates/gents/src/eval/report/
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" commit -m "feat(eval): report breakdowns by check, by stage and one case side by side (#1515)"
```

### Task 16: `compare --by check|stage` and `compare --case`

**Files:**
- Modify: `crates/gents-cli/src/cli/args.rs` (`BreakdownArg`; two fields on `EvalCompareArgs`)
- Modify: `crates/gents-cli/src/commands/eval/compare.rs` (the breakdown branch; tests)
- Modify: `crates/gents-cli/src/commands/eval/render.rs` (`Breakdown`, `breakdown_table`, `case_view_text`)

**Interfaces:**
- Consumes (Task 15):
  ```rust
  pub fn gents::eval::report::by_check(comparison: &Comparison) -> Vec<CheckDiff>;
  pub fn gents::eval::report::by_stage(comparison: &Comparison) -> Vec<StageDiff>;
  pub fn gents::eval::report::case_view(comparison: &Comparison, case_id: &str) -> Result<CaseView>;
  pub struct CaseView { case_id, summary: CaseComparison, trials: Vec<PairedTrial> }
  pub struct PairedTrial { case_id, trial_index, baseline: Option<SideTrial>, candidate: Option<SideTrial> }
  pub struct SideTrial { class: SlotClass, score: SlotScore, verdicts: Vec<SlotVerdict> }
  ```
- Produces:
  ```rust
  #[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)] pub(crate) enum BreakdownArg { Check, Stage }
  // EvalCompareArgs gains: by: Option<BreakdownArg> (conflicts with --case), case_id: Option<String> (flag --case)
  #[derive(Serialize)] #[serde(tag = "by", content = "rows", rename_all = "snake_case")]
  pub(crate) enum render::Breakdown { Check(Vec<CheckDiff>), Stage(Vec<StageDiff>) }   // JSON {"by": "check", "rows": [...]}
  pub(crate) fn render::breakdown_table(breakdown: &Breakdown, out: &mut dyn Write) -> io::Result<()>;
  pub(crate) fn render::case_view_text(view: &CaseView, out: &mut dyn Write) -> io::Result<()>;
  ```
  Output: without `--json`, the comparison table and then the breakdown; with `--json`, only the breakdown structure (`{"by", "rows"}` or the `CaseView`), as spec 4b §8 says.

- [ ] **Step 1: Add the arguments and the failing tests**

In `crates/gents-cli/src/cli/args.rs`, before `EvalCompareArgs`:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub(crate) enum BreakdownArg {
    Check,
    Stage,
}
```

and inside `EvalCompareArgs`, after `policy`:

```rust
    /// Aggregate the paired difference by check name or stage id.
    #[arg(long, value_enum, conflicts_with = "case_id")]
    pub(crate) by: Option<BreakdownArg>,
    /// Print this case's trials side by side with each verdict.
    #[arg(long = "case")]
    pub(crate) case_id: Option<String>,
```

Append inside `mod tests` in `crates/gents-cli/src/commands/eval/compare.rs` (adding `row` to the `testing` import and `use clap::Parser; use crate::cli::Cli;`):

```rust
    #[tokio::test]
    async fn by_check_and_by_stage_aggregate_and_case_prints_both_sides() {
        let fixture = Fixture::new().await;
        fixture
            .scripted_run("r1", &executor(&VALIDATION_CASES[..3]), CancellationToken::new())
            .await;
        let with = |extra: &[&'static str]| {
            let mut argv = vec!["compare", "r1", "r1"];
            argv.extend(CELLS);
            argv.extend_from_slice(extra);
            argv
        };

        let checks = eval(&fixture, &with(&["--by", "check"])).await.unwrap();
        assert_eq!(
            row(&checks, "captured_rows_count", 7),
            vec!["captured_rows_count", "6", "12", "+50.00%", "3", "3", "0"]
        );
        let stages = eval(&fixture, &with(&["--by", "stage"])).await.unwrap();
        assert_eq!(
            row(&stages, "check", 7),
            vec!["check", "6", "12", "+50.00%", "3", "3", "0"]
        );

        let json: serde_json::Value = serde_json::from_str(
            &eval(&fixture, &with(&["--by", "check", "--json"])).await.unwrap(),
        )
        .unwrap();
        assert_eq!(json["by"], "check");
        assert_eq!(json["rows"][0]["mean_diff_bp"], 5_000);

        let case = eval(&fixture, &with(&["--case", "val-a"])).await.unwrap();
        for expected in [
            "case val-a pairs 2 baseline 0.00% candidate 100.00% diff +100.00%",
            "#0 baseline fail 0.00% candidate pass 100.00%",
            "  baseline check/captured_rows_count model_acceptance score 0 reason below_min",
            "  candidate check/captured_rows_count passed score 10000 reason in_range",
        ] {
            assert!(case.lines().any(|line| line == expected), "{expected:?} in\n{case}");
        }

        let missing = eval(&fixture, &with(&["--case", "zzz"])).await.unwrap_err();
        assert_eq!(missing.to_string(), "the comparison has no case \"zzz\"");
        assert!(Cli::try_parse_from(
            ["gents", "eval", "compare", "r1", "r1", "--by", "check", "--case", "val-a"]
        )
        .is_err());
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents-cli --lib commands::eval::compare > "$LOG/t16.log" 2>&1; grep -E "error(\[|:)|FAILED|panicked|test result" "$LOG/t16.log" | head -20`
Expected: the new test fails: `no 7-column row for captured_rows_count` (the flags parse but nothing prints a breakdown yet).

- [ ] **Step 3: Write the implementation**

In `crates/gents-cli/src/commands/eval/compare.rs`, extend the imports:

```rust
use gents::eval::report::{by_check, by_stage, case_view};

use crate::cli::BreakdownArg;
```

and replace the final `if args.json { … } else { … }` of `compare` with:

```rust
    if let Some(by) = args.by {
        let breakdown = match by {
            BreakdownArg::Check => render::Breakdown::Check(by_check(&comparison)),
            BreakdownArg::Stage => render::Breakdown::Stage(by_stage(&comparison)),
        };
        if args.json {
            return write_json(out, &breakdown);
        }
        render::comparison_table(&comparison, out)?;
        return Ok(render::breakdown_table(&breakdown, out)?);
    }
    if let Some(case_id) = &args.case_id {
        let view = case_view(&comparison, case_id)?;
        if args.json {
            return write_json(out, &view);
        }
        render::comparison_table(&comparison, out)?;
        return Ok(render::case_view_text(&view, out)?);
    }
    if args.json {
        write_json(out, &comparison)
    } else {
        Ok(render::comparison_table(&comparison, out)?)
    }
```

Append to `crates/gents-cli/src/commands/eval/render.rs` (adding `use gents::eval::report::{CaseView, CheckDiff, SideTrial, SlotScore, StageDiff};`):

```rust
/// A compare breakdown, as `--json` prints it: `{"by": …, "rows": […]}`.
#[derive(Debug, Serialize)]
#[serde(tag = "by", content = "rows", rename_all = "snake_case")]
pub(crate) enum Breakdown {
    Check(Vec<CheckDiff>),
    Stage(Vec<StageDiff>),
}

pub(crate) fn breakdown_table(breakdown: &Breakdown, out: &mut dyn Write) -> io::Result<()> {
    // One row shape for both: a stage row is a check row named by stage id.
    let (label, rows): (&str, Vec<CheckDiff>) = match breakdown {
        Breakdown::Check(rows) => ("check", rows.clone()),
        Breakdown::Stage(rows) => (
            "stage",
            rows.iter()
                .map(|row| CheckDiff {
                    check: row.stage_id.clone(),
                    cases: row.cases,
                    pairs: row.pairs,
                    mean_diff_bp: row.mean_diff_bp,
                    improved: row.improved,
                    tied: row.tied,
                    worsened: row.worsened,
                })
                .collect(),
        ),
    };
    writeln!(out)?;
    writeln!(
        out,
        "{:<28} {:>5} {:>5} {:>9} {:>8} {:>4} {:>8}",
        label, "cases", "pairs", "mean_diff", "improved", "tied", "worsened"
    )?;
    for row in &rows {
        writeln!(
            out,
            "{:<28} {:>5} {:>5} {:>9} {:>8} {:>4} {:>8}",
            row.check,
            row.cases,
            row.pairs,
            signed_percent(row.mean_diff_bp),
            row.improved,
            row.tied,
            row.worsened
        )?;
    }
    Ok(())
}

fn slot_score(score: SlotScore) -> String {
    match score {
        SlotScore::Absent => "absent".to_owned(),
        SlotScore::NotEvidence => "not_evidence".to_owned(),
        SlotScore::Unknown => "unknown".to_owned(),
        SlotScore::Scored(bp) => percent(Some(bp)),
    }
}

fn side_label(side: Option<&SideTrial>) -> String {
    side.map_or_else(
        || "absent".to_owned(),
        |side| format!("{} {}", wire(&side.class), slot_score(side.score)),
    )
}

pub(crate) fn case_view_text(view: &CaseView, out: &mut dyn Write) -> io::Result<()> {
    let summary = &view.summary;
    writeln!(out)?;
    writeln!(
        out,
        "case {} pairs {} baseline {} candidate {} diff {}",
        view.case_id,
        summary.pairs,
        percent(summary.baseline_mean_bp),
        percent(summary.candidate_mean_bp),
        signed_percent(summary.diff_bp)
    )?;
    for trial in &view.trials {
        writeln!(
            out,
            "#{} baseline {} candidate {}",
            trial.trial_index,
            side_label(trial.baseline.as_ref()),
            side_label(trial.candidate.as_ref())
        )?;
        for (arm, side) in [("baseline", &trial.baseline), ("candidate", &trial.candidate)] {
            for verdict in side.iter().flat_map(|side| &side.verdicts) {
                writeln!(
                    out,
                    "  {arm} {}/{} {} score {} reason {}",
                    verdict.stage_id,
                    verdict.check,
                    verdict.kind.as_str(),
                    verdict
                        .score_bp
                        .map_or_else(|| "-".to_owned(), |score| score.to_string()),
                    verdict.reason_code.as_deref().unwrap_or("-")
                )?;
            }
        }
    }
    Ok(())
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents-cli --lib commands::eval > "$LOG/t16.log" 2>&1; grep -E "^test |test result|panicked" "$LOG/t16.log" | tail -30`
Expected: the new test passes with every earlier one.

- [ ] **Step 5: Format and commit**

```bash
cargo fmt --all && cargo fmt --all --check
git add crates/gents-cli/
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" commit -m "feat(cli): gents eval compare --by check|stage and --case (#1515)"
```

### PR 5 gate, and the integrated stack

- [ ] On `eval/54-compare-breakdowns`, run one at a time and logged: `CARGO_BUILD_JOBS=4 cargo test -p gents` (only the known libp2p dial timeouts may fail, standing rule 4), `CARGO_BUILD_JOBS=4 cargo test -p gents --test eval_runner_canary`, `CARGO_BUILD_JOBS=4 cargo test -p gents-cli --lib`, `CARGO_BUILD_JOBS=4 cargo test -p gents-cli --test cli_offline`, `CARGO_BUILD_JOBS=4 cargo check --workspace --all-targets`, `cargo fmt --all --check`. Record every count in the ledger.
- [ ] No `lake build` is needed: no Lean changed (constraint 9). Confirm with `git diff --stat <M4-BASE>..HEAD -- crates/gents/proofs` printing nothing.

### Task 17: The five PR descriptions

**Files:**
- Create: `.superpowers/sdd/2026-09-22-eval-report-and-cli/pr-descriptions.md` (the coordinator's own directory; not committed)

**Interfaces:**
- Consumes: the ledger, the gate logs, this plan's Deviations.
- Produces: one description per PR, for the orchestrator, who opens the PRs (standing rule 3).

- [ ] **Step 1: Write the descriptions**

For each PR, in this shape (CLAUDE.md: "Keep its description current with baseline, owners, deletions and validation"):

```markdown
## <PR n>: <branch> → <parent branch>

Issue #1515, spec 4a/4b (`docs/superpowers/specs/2026-09-22-eval-report-and-cli-design.md`), plan `docs/superpowers/plans/2026-09-22-eval-report-and-cli.md`.

**Baseline:** <parent branch> @ <hash from `git rev-parse --short <parent>`>.
**What it adds:** <one line per task in this PR, from the ledger>.
**Owners it extends:** <PR 1: `eval::report` (M6b's unfrozen module), `eval::runner` loop and plan; PR 1 also `optimization::driver` (F1) and `optimization::references` (F8); PR 2/3/4/5: `gents-cli` commands over those owners>.
**Deletions:** none. **Frozen interfaces touched:** none (`eval::{outcome, scoring, documents}`, `EvalDefinition` untouched: `git diff --stat <parent>..<branch> -- crates/gents/src/eval/outcome.rs crates/gents/src/eval/scoring.rs crates/gents/src/eval/documents.rs crates/gents/src/document_config/eval_definition.rs` prints nothing).
**Deviations:** <the D-items this PR carries, by number>.
**Validation:** <the gate commands and their counts from the ledger>.

🤖 Generated with [Claude Code](https://claude.com/claude-code)
```

- [ ] **Step 2: Check the frozen-interface claim for every PR**

Run, for each (branch, parent) pair: `git diff --stat <parent>..<branch> -- crates/gents/src/eval/outcome.rs crates/gents/src/eval/scoring.rs crates/gents/src/eval/documents.rs crates/gents/src/document_config/eval_definition.rs`
Expected: no output for any pair. If one prints, stop and message the orchestrator (standing rule 1).

- [ ] **Step 3: Message the orchestrator** "plan complete" with the five branch heads and the path of the descriptions file (standing rule 7). No commit.

---

## Self-review

**Spec coverage.**

| Spec item | Task |
|---|---|
| §1 `build`, `EvalReport`, `RunSummary`, `CellReport`, `SlotReport`, `SlotClass`, `AttemptSummary`, `CaseReport`, `SlotCounts`, classification | 1 |
| §1 `compare`, `Comparison`, `CaseComparison`, refusals on digest and comparability version, improved/tied/worsened by case sign, zero-pair cases excluded, `p_ppm` from the policy's test, `PolicyOutcome` with `calibrated` | 2 |
| §2 `load_runs` | 3 (D5) |
| §2 `eval run` (freeze + run, per-slot lines, show table, Ctrl-C, `--profile` per cell with the home default) | 10 |
| §2 `eval resume` (marker removed, then `runner::resume`) | 4 (library), 10 (command) |
| §2 `eval list` (invalidated hidden without `--all`), `show`, `trial` | 7 |
| §2 `eval compare` (+ `--policy`, banner) | 9 |
| §2 `eval cancel`, `invalidate` (`AlreadyInvalidated` on a second call), `rm` (refuses unfinished without `--force`, bytes reclaimed) | 8 |
| §2 `optimization run` (scripted proposer only), `show` | 11 |
| §2 `optimization promote`, `revert` | 12 (D11) |
| §2 exit status 0 / 1 / 2, refusals verbatim | 7 (`surface_refusal`), 11, 12; D13 |
| §3 the cancel marker | 4 |
| §4 unit tests: every `SlotClass`; regrade; attempts vs slots; exposure | 1 |
| §4 unit tests: compare signs; dropped and imputed vs `pair_trials`; `p_ppm` vs `permutation_p_ppm`; mismatched digest and version refused; `--policy` equals `decide` | 2 |
| §4 CLI tests: each command's table and `--json`; cancel observed by a running loop; `rm` refuses then forces; `compare --policy defaults` banner; `optimization show` after a scripted job | 7, 8, 9, 10, 11, 12 |
| §5 phasing, PRs 1-3 | PR sections 1-3 |
| 4b §6 `progress.json` (atomic, stage start/end, slot end, created by run/resume, deleted with the directory) | 6 (O1, D8); `rm` 8 and `gc` 14 delete it with the directory |
| 4b §6 `eval watch --interval` | 13 |
| 4b §7 `eval gc --older-than --definition --dry-run`, job-reference check, size column | 14 (D14) |
| 4b §8 `by_check`, `by_stage`, `case_view`, the flags, per-verdict data in the `Comparison` JSON | 15, 16 (D2, D15) |
| 4b §9 abandoned attempts spend no retry; ten-abandonment bound; table test; loop test under `max_infra_retries: 0` | 5 (O2) |
| 4b phasing, PRs 4-5 | PR sections 4-5 |
| Ruling U1 `optimization rm`, `gc --jobs` | 12a, 14 |
| Ruling U2 frozen `definition.json`, report reads it, resume verifies it | 2a, 3 (and `definition_changed` in 1, 7b) |
| Ruling U3 `revert --digest` | 12 |
| Ruling U4 `pid`/`written_at`, heartbeat, stale marking | 6, 6a, 13 |
| Ruling U5 marker seen mid-batch, long-trial test | 6a |
| Ruling U6 binary smoke test | 10c |
| Ruling U7 files that move together | 7 (`testing.rs` doc) |
| Ruling U8 `run` clears the marker | 4 |
| Ruling U9 purity wording | 1, 3 (module docs) |
| Ruling U11/U12 single base, signatures re-verified | "The M4 base", 1 Step 0, 12 Step 0 |
| Ruling U13 registry note | 10b (`--registry` doc) |
| F1 `RunOutcome.cancelled`, driver stops undecided, job-run cancel notice | 4, 4a, 8, 10b |
| F2 removable job states, flipped tests | 4b, 12a, 14 |
| F3 `host_alive`, `is_fresh` | 6, 13 |
| F4 distinct-run seed, >20-case test | 2, 2b |
| F5 shared `totals`, `CellReport.cell_usage` | 1, 1b, 2 |
| F6 marker during backoff | 6a |
| F7 finished = nothing owed and nothing fresh | 6b, 8, 14 |
| F8 `rm` refuses runs held by live jobs; references in PR 1 | 4b, 8, 12a |
| F9 pre-file run still reports | 3 |
| F10 task splits | 1/1b, 2/2b, 7/7b, 10/10b |
| F11 base signatures recorded | "The M4 base" |
| F12 agent tool surfaces free of the operator verbs | 8 |
| F13 `--interval` ≥ 250 ms | 13 |
| F14 default ids | 10, 10b, 11 |
| F15 resume prints after thaw | 10b |
| F16 heartbeat rate | 6, 6a |

No spec item or ruling is without a task.

**Placeholder scan.** Searched the plan for "TBD", "TODO", "implement later", "appropriate", "similar to Task", "fill in": none. Every code step shows its code; every command shows its expected result. Two steps name a value to read from the repository rather than assume it, and say exactly where: Task 12 Step 0 (`git grep` for M6b's `promote`) and Task 17 (branch hashes from `git rev-parse`).

**Type consistency.** Checked across tasks, after the F-rulings: `RunOutcome { …, cancelled }` (4) is read by the driver (4a) and `eval run`/`resume` (10b), and Task 4's `a_two_cell_run…` literal gains the field; `CellReport.cell_usage: CellUsage` (1) feeds `Comparison.usage: [CellUsage; 2]` and `optimization::evidence::totals(CellUsage, CellUsage, u64)` (2); `removable(&JobState)` and `held_runs(access, owner)` (4b) are called by Tasks 8, 12a and 14 with those signatures; `host_alive`/`is_fresh(slot, Duration)` (6) by 6b and 13; `heartbeat(min_gap)` (6) by 6a; `slots_owed`, `running_elsewhere` (6b) by 8 and 14; `manage::cancel(runs_dir, run_id, purpose, out)` and `cancel_run` (8) match the Task 10b `execute` arm and the Task 8 test executor; `default_id` (10) by 10b and 11; `parse_interval`/`parse_age` (13) by 13 and 14. Earlier checks: `build(run, trials, verdicts, definition, peers)` (1) is called by `store::load_report_among` (3) with that order; `load_report(access, owner, runs_dir, run_id)` and `load_report_among(access, owner, runs_dir, record, peers)` (3) are called with `&ctx.runs_dir()` in Tasks 7, 8, 9, 10, 13 and 14 and with `&launching.runs_dir()` in Task 3's tests; `read_frozen_definition(run_dir, &DefinitionRef)` and `DEFINITION_FILE` (2a) are what `thaw` and `store` use; `InFlight { …, pid, written_at }` (6) is built with both fields in Tasks 6 and 13 and refreshed by `ProgressWriter::heartbeat` (6, called in 6a); `render::{in_flight(progress, now, stale_after, out), is_stale}` (13) match their one caller; `removable(journal)` (12a) is what Task 14's `gc --jobs` calls; `manage` is `pub(crate)` (8) so Task 12a can call `dir_size`; `SlotScore::trial_score` (1) is what `compare::trial_scores` (2) reads; `Comparison::{with_policy, evidence, seed}` (2) are the names Task 9 and the tests use; `run_dir`, `request_cancel`, `CANCEL_MARKER` (4) are the names Tasks 8, 10, 13 and 14 import; `read_progress`, `Progress`, `InFlight`, `PROGRESS_FILE`, `StageProgress` (6) match Task 13; `MAX_ABANDONED_ATTEMPTS` (5) is re-exported from `eval::runner`; `EvalContext::{runs_dir, jobs_dir}` (7, 11), `Deps` (10), `execute(ctx, command, deps, out)` (10, and both `testing` helpers), `load_policy` (9, 11), `source_commit`/`source_dirty` (10, 11), `resolve_subject_pack(home, spec, registry, directory)` (10, 11), `render::{percent, wire, signed_percent, decision_label, human_bytes}` (7, 9, 14) are spelled the same everywhere they appear; `SlotVerdict`, `SideTrial`, `PairedTrial` (15) are what Task 16 renders.

## Plan defects I could not resolve

- **R1 — `thaw` prefers the frozen definition.** Ruling U2 says resume "verifies the file's digest against the origin"; the plan also makes resume *use* the verified copy (Task 2a), so a run whose installed definition was edited can still be resumed where it was refused before. The optimizer is unaffected (it refuses a drifted definition itself before resuming a run). If resume should keep refusing a live edit, Task 2a's `thaw` change is one `if` away.
- **R2 — `gc --jobs` ages a job by its directory's modification time.** An `OptimizationJob` has no creation timestamp; a directory touched by a later round looks younger than the job.
- **R3 — U10's follow-up is open.** `eval list` and `eval gc` build per-run reports or read every run's trials; the header-only projection is recorded, not planned.
- **R4 — The heartbeat stops once the loop has observed a cancel.** While in-flight executors wind down (up to the embedded executor's grace period) their entries stop being refreshed; with a live host they read stale only after three watch intervals, and `running_elsewhere` treats them as not running after three runner poll periods, so `rm`/`gc` could act on a run whose last trial is still unwinding. The window is the grace period (30 s).
- **R5 — The F6 backoff test is timing-shaped.** It writes the marker once the first attempt is recorded; if the marker lands before the pass's own after-pass check, the test still passes without exercising the backoff branch. The 30 s backoff makes that unlikely, not impossible.
