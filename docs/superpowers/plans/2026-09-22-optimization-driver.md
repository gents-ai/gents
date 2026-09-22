# Optimization Driver (M6b) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land the impure half of `gents::optimization`: an `OptimizationJob` journal document, a `Proposer` seam with a scripted implementation, a pure structural gate, a driver that runs rounds of native `EvalRun`s and decides them with `PolicyV2`, the library functions `show`, `promote` and `revert` (the last two through the Track 0 compare-and-set), and a new unfrozen `eval::report` module holding the verdict-to-evidence projection both optimization and M4 consume.

**Architecture:** Four stacked PRs over one base the orchestrator supplies. PR 1 adds the `OptimizationJob` collection, the one-field closure target, and the typed, length-guarded journal that refines `Optimization.appendIf`. PR 2 adds the candidate pack materializer, the `Proposer` trait with `ScriptedProposer`, and the structural gate. PR 3 adds `eval::report`, then the driver: it freezes a baseline, runs a train `EvalRun`, asks the proposer, gates the candidate, runs a two-cell validation `EvalRun` on shared seeds (re-runs add pairs), calls `decide`, journals the result, and finalizes on the held-out split; then `show`, and the scripted matrix. PR 4 adds `promote` and a terminal `revert` over `DesiredStateApplyPlan::with_expected`, and one live accept plus one live reject, ignored until M5. No Lean is added.

**Tech Stack:** Rust 1.97.1, tokio, `async_trait`, DefraDB via `gents::defra_node::EmbeddedNode`, `serde_json`, `sha2`, `tempfile`, `tokio_util::sync::CancellationToken`.

**Spec:** `docs/superpowers/specs/2026-09-21-optimization-on-eval-design.md` (approved 2026-09-21; amended by orchestrator ruling R2 to one target field). Umbrella: `2026-09-21-eval-and-optimization-umbrella.md` sections 4 to 7, milestone M6. Contract: `2026-09-21-eval-core-contract-design.md`. This plan covers everything in spec 5 that M6a did not: M6a delivered `Proofs/Optimization.lean`, its conformance emitter, and `PolicyV2`, and none of that is reopened here. Orchestrator rulings R1–R9 and fixes F1–F12 are recorded in `.superpowers/sdd/m6b-plan-fixes.md` and applied throughout.

**Depends on:** the M6b base, defined below. This plan runs under `docs/superpowers/orchestration/2026-09-21-parallel-coordinators.md`.

---

## The M6b base

Finding F10: the M6b base is **`eval/13-runner-embedded` rebased onto the amended `eval/05-contract`**, built by the orchestrator at spin-up, with its commit filled in then. This plan pins no hash for it; earlier hashes of these branches are stale after the M1 amendment. The coordinator records the supplied commit in the ledger at first use, and every branch in this plan bases on it or on its predecessor in the stack.

The base must carry four things. The first task's first step verifies them and stops, messaging the orchestrator, if any is missing:

| Needed | Supplied by | Check |
|---|---|---|
| `gents::eval::runner` (`run`, `resume`, `RunRequest`, `CellSource`, `ScriptedExecutor`, `RunOptions`) and `CheckRegistry` | `eval/10..13` | `git grep -n "pub async fn resume" -- crates/gents/src/eval/runner/mod.rs` |
| `PROTECTED_DATASTORE_COLLECTIONS` with the `"OptimizationJob"` reserved literal | `eval/06-protected` | `git grep -n '"OptimizationJob"' -- crates/gents/src/document_config/write_tool.rs` |
| `DesiredStateApplyPlan::with_expected`, `DesiredStateExpectation`, `StaleExpectation`, `stale_expectation` | Track 0, `optimization/01..03` | `git grep -n "pub fn with_expected" -- crates/gents/src/config_client/desired_state.rs` |
| `PolicyV2`, `Evidence`, `decide`, `evidence_from_pairs`, `Mode`, `DecisionReport` | `optimization/10..12` | `git grep -n "pub fn evidence_from_pairs" -- crates/gents/src/optimization/policy.rs` |

Each check prints one line on a correct base and nothing on a wrong one. If the orchestrator's base arrives as a rebase rather than a merge, each PR in this plan stays a single-parent range against its immediate parent, which CLAUDE.md's stacked-PR rule requires.

## Deviations from the spec and from the brief, decided at planning

Orchestrator rulings (from `.superpowers/sdd/m6b-plan-fixes.md`):

- **R1 — The CLI is M4's.** `gents optimization show | promote | revert` are spec 4a's commands. M6b delivers the library functions they wrap: `show(access, owner, job_id) -> JobView` (PR 3 Task 6), `promote` and `revert` (PR 4 Task 1). No CLI task is in this plan.
- **R2 — One target field.** v1 optimizes exactly `AgentContext.system_prompt`. `TargetField` has one variant, the Task `prompt_template` target and its placeholder-set check are deferred, and the spec is amended accordingly (sections 1 and 3).
- **R3 — `eval::report` owns the verdict projection.** Latest-attempt-per-slot selection, `VerdictRecord → VerdictView`, per-run pairing and usage live in a new, unfrozen `crates/gents/src/eval/report/` module (PR 3 Task 1). `optimization` consumes it; M4 will too. Nothing new goes into the frozen `eval::scoring`.
- **R4 — `Reverted` is terminal**, as spec section 6 draws it. `JobState::Reverted` exists; a further promotion is a new job.
- **R5 — The subject pack is operator-supplied.** Spec section 1 says the baseline subject is the owner's configuration exported as a pack snapshot. Here the operator supplies the pack (`JobRequest::baseline_pack`, a directory), and freeze cross-checks that the pack's context `system_prompt` equals the live closure's text for the same context, refusing the job otherwise. The live closure is still what promotion guards; the pack is what trials run.
- **R6 — Verb signatures.** `promote` and `revert` take the job's identity `(access, owner, job_id)`, plus spec section 7's `digest` and `by`, the acting DID. `jobs_dir` and `behavior_id` are persisted in `JobOrigin` (`jobs_dir`, `subject.behavior_id`), so neither verb needs the original request. `by` is the launching home's identity supplied by the caller, the same pattern as the runner's `evaluator_did`, and is compared against `origin.owner`; an authenticated, enforced identity boundary is spec 2b's.
- **R7 — The live scenarios run in M5.** They take the M3 definition pack directory and the definition id from environment variables and stay `#[ignore]`.
- **R8 — Job directories** live under `<launching home>/eval/jobs/<job_id>/`. Retention is M4's `gents optimization rm`; there is no TTL.
- **R9 — No driver fault-injection seam.** Resume is covered by interrupting a job with a cancelled token and with an erroring proposer, then comparing its journal against an uninterrupted twin job (PR 3 Task 5).

Planning deviations, verified against the code:

- **`evidence_from_paired` is spelled `evidence_from_pairs`,** and it lives at `crate::optimization::policy::evidence_from_pairs` (finding F4); there is no `evidence_from_paired`.
- **`AgentContext` lives in `crates/gents/src/document_config/context.rs`,** not `agent_context.rs`; `system_prompt` is `Option<String>`.
- **The structural gate's closure check is pure.** `validate_desired_state_plan` (`desired_state.rs:233`) is `pub(crate)`, needs a transaction, and validates the operator's closure, which a candidate never touches until promotion. The gate applies the same rule to the candidate pack's own documents with `DesiredStateApplyPlan::from_pack_config` and `ConfigReferences::from_documents(...).validate()`; the live form runs inside `promote`'s transaction.
- **`run_job` takes a `&CheckRegistry`.** `gents::eval::runner::run` requires one, and `CheckRegistry` is neither `Clone` nor serializable, so it cannot ride on the request.
- **Stale baseline is tested twice.** At job start (`Failed(baseline_drifted)`, PR 3 Task 5) and at promote time (`PromotionRefused`, PR 4 Task 1).
- **The scripted matrix is a library test.** It needs `CheckRegistry::with` (`#[cfg(test)] pub(crate)`) and the `Launching` harness in `eval/runner/freeze.rs`'s `pub(crate) mod tests`, so it lives at `crates/gents/src/optimization/driver/matrix.rs`. It still runs under `cargo test -p gents`.
- **The closure excludes `EvalDefinition`** (finding F5). `Collection::ALL` includes it, so without the exclusion a definition edit would read as `baseline_drifted`; the driver also checks definition drift first.

## Global Constraints

Standing rules carried verbatim from `docs/superpowers/orchestration/2026-09-21-parallel-coordinators.md`:

1. **Interface freeze.** `gents::eval::{outcome, scoring, documents}` and `EvalDefinition` are frozen at the pinned commit. A task that needs a change there stops and messages the orchestrator.
2. **Build budget.** One cargo or lake invocation at a time per workspace, with `CARGO_BUILD_JOBS=4`. Foreground commands only: never background a command, never `sleep`, never poll. Long output goes to a log file that is then grepped.
3. **Git.** Commit with `git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com"`. End every commit message with `Co-Authored-By: <the implementing model> <noreply@anthropic.com>` for a Claude model, and `Co-Authored-By: Grok 4.7 <noreply@x.ai>` for Grok. Never push, never open a PR, never change git config. Worktrees are created with `make worktree BRANCH=<branch> DIR=<dir> BASE=<ref>` from the main checkout.
4. **Repo gates.** `cargo fmt --all --check` is CI. Lean imports are narrow: never `import Mathlib` or `import Mathlib.Tactic`; no `sorry`. Escape interpolated GraphQL; never emit `[]` in a mutation; `tracing`, never `println!`. The conformance test `generated_r5_cross_principal_cases_drive_production_dispatch` fails on unmodified `main` on this machine (libp2p dial timeout) and is not attributable to any task.
5. **Models.** Implementers and reviewers are dispatched with an explicit model, never inherited. Opus 5 is `opus`.
6. **Process.** `superpowers:subagent-driven-development`: ledger first, task brief, fresh implementer, review package, task review, fix rounds up to five, final whole-branch review. Rulings are recorded as `Ruling: <what> — <why> — <cost if wrong>` and never wait on a human.
7. **Reporting.** The ledger at `<worktree>/.superpowers/sdd/2026-09-22-optimization-driver/progress.md` is the record. Message the orchestrator only at: task complete, ruling made, blocked, plan defect found, plan complete.
8. **Scope.** Nothing outside the assigned worktree is read for editing or written.

Plus the constraints this milestone adds:

9. **No Lean is edited.** `Proofs/Optimization.lean` and `Proofs/Conformance/Optimization.lean` are complete for M6b. The conformance emitter `optimizationCasesJson` emits `params`, `decisions`, `gates` and `costs` only — it emits **no journal cases** (`Proofs/Conformance/Optimization.lean:83-94`), so no journal conformance consumer is written. The Rust journal refines the Lean model through the ordinary unit tests in PR 1 Task 4, which reproduce `appendIf_match`, `appendIf_stale_unchanged` and `appendIf_prefix` over the real document. If an implementer believes a Lean change is needed, stop and message the orchestrator.
10. **One field, one pack.** Every candidate is the baseline subject pack with exactly one field changed: `AgentContext.system_prompt` of the context the subject behavior names. It is materialized as a directory pack under the job's directory and handed to the runner as `CellSource::Directory(<launching home>/eval/jobs/<job_id>/rounds/<round>/candidate)`. No other candidate shape exists.
11. **The proposer holds no `ConfigAccess`.** `Proposer::propose` takes a `ProposalInput` value and returns a `Proposal` value. The trait's signature admits no node, no transaction and no path. `target.rs` and `subject.rs` build the patch deterministically from the returned text.
12. **Feedback is read only from the train run's verdicts.** The driver reads `feedback` from the `EvalVerdict` rows of the round's train run and from nowhere else. The runner already nulls `feedback` off the train split (`runner/mod.rs:529-534`) and `append_verdict` refuses it there (`documents.rs:525-527`); this constraint is the driver-side half of the same rule.
13. **Nothing protected reaches the proposer.** `ProposalInput` carries the current target text, per-check feedback strings and scores, and the rejection history. It carries no `case_id`, no stage prompt, no check params, no tier or split label, no `raw` verdict payload and no other case's body. A test pins the type's field list.
14. **`promote` uses `with_expected` on the frozen closure, including the baseline context's digest, and refuses on `StaleExpectation`.** The plan writes only the target document. There is no force flag. `revert` restores `previous_text` the same way, expecting the digest `promote` journaled, and ends the job.
15. **After freeze, only the origin decides** (finding F2). `run_job` reads policy, budgets, seed base, trials per case, inference profile, text cap and job directory from `JobOrigin`; a resume request that disagrees is refused.
16. **rustfmt is a CI gate.** `cargo fmt --all --check` exits 0 before every commit. Where a task says "add a module", place the `mod` line where rustfmt sorts it.
17. **No `unwrap`/`expect` on input-dependent paths outside tests.**
18. `OptimizationJob` stays out of the `Collection` enum, out of `BRANCHABLE_COLLECTION_NAMES`, and out of `CONVERSATION_COLLECTIONS`, `CLIENT_COLLECTIONS` and `CLIENT_TO_RUNTIME_COLLECTIONS` in `crates/gents/src/agent/p2p_reconcile/templates.rs`. It goes into `LOCAL_AUDIT_COLLECTION_NAMES`, because its journal holds candidate prompts. Job directories under `<launching home>/eval/jobs/` hold the same prompts on disk; their removal is M4's `gents optimization rm` (ruling R8).

## File Structure

| File | PR | Responsibility |
|---|---|---|
| `crates/gents-schemas/schemas/agent/optimization_job.graphql` | 1 | The SDL: frozen `origin`, append-only `journal`, guard field `journal_len`, derived `state` |
| `crates/gents-schemas/src/lib.rs` | 1 | `OPTIMIZATION_JOB{,_NAME}`, `ALL`, `ALL_COLLECTION_NAMES`, `LOCAL_AUDIT_COLLECTION_NAMES`, classification test |
| `crates/gents-protocol/src/schemas.rs` | 1 | The mirror: re-export plus `ALL` and `ALL_COLLECTION_NAMES` |
| `crates/gents-migration/src/registry.rs` | 1 | The `DEFAULT_BASELINE` pin |
| `crates/gents/src/document_config/write_tool.rs` | 1 | Replace the `"OptimizationJob"` literal with the schema constant |
| `crates/gents/src/optimization/target.rs` | 1 | `TargetField` (one variant), `Target`, `Closure`, `FrozenDocument`, `capture_closure` (without `EvalDefinition`), `closure_digests`, `current_text`, `apply_text`, `target_digest`, `expectations`, `target_plan` |
| `crates/gents/src/optimization/job.rs` | 1 | `JobOrigin`, `Budgets`, `JournalEntry`, `JobState` (with terminal `Reverted`), `JobRecord`, `derive_state`, `checkpoint`, `create_job`, `load_job`, `load_job_in_txn`, `append`, `append_in_txn`, `JournalConflict` |
| `crates/gents/src/optimization/subject.rs` | 2 | `MaterializedPack`, `materialize_pack`, `materialize_candidate`, `baseline_text` |
| `crates/gents/src/optimization/proposer.rs` | 2 | `ProposalInput`, `CheckFeedback`, `Rejection`, `Proposal`, `Proposer`, `ScriptedProposer` |
| `crates/gents/src/optimization/gate.rs` | 2 | `StructuralRejection`, `structural_gate` |
| `crates/gents/src/eval/report/{mod.rs,evidence.rs}` | 3 | `RunRows`, `load_run_rows`, `latest_attempts`, `cell_trial_scores`, `paired_evidence`, `concat_paired`, `cell_usage` |
| `crates/gents/src/optimization/evidence.rs` | 3 | `decision_evidence`, `token_totals`, `train_feedback`, `decision_seed`, the cell ids |
| `crates/gents/src/optimization/driver.rs` | 3 | `JobRequest`, job paths, run plans, the budget, `run_job` |
| `crates/gents/src/optimization/driver/matrix.rs` | 3 | The scripted end-to-end matrix and its `pub(crate)` harness |
| `crates/gents/src/optimization/show.rs` | 3 | `show`, `JobView`, `DecisionView` |
| `crates/gents/src/optimization/promote.rs` | 4 | `promote`, `revert`, `Promotion`, `PromoteRefused`, `promote_refused` |
| `crates/gents/tests/optimization_live.rs` | 4 | One live accept and one live reject, both `#[ignore]` until M5 |
| `crates/gents/src/optimization/mod.rs` | 1-4 | Module wiring and the crate's `pub use` surface |

---

## PR 1: The `OptimizationJob` collection and its journal

Branch: `optimization/20-job`, base: the M6b base. Deliverable: a registered, pinned, local-audit-classified collection that no datastore tool may name, and a typed append-only journal whose length guard refines `Optimization.appendIf`.

### Task 1: The schema, the catalogs, the mirror and the pin

**Files:**
- Create: `crates/gents-schemas/schemas/agent/optimization_job.graphql`
- Modify: `crates/gents-schemas/src/lib.rs` (constants after the `EVAL_VERDICT` pair at lines 109-110; `ALL` last element before its close at line 221; `ALL_COLLECTION_NAMES` last element before its close at line 290; `LOCAL_AUDIT_COLLECTION_NAMES` at lines 330-335; a new test beside `eval_placement_is_decided_per_collection` at line 599)
- Modify: `crates/gents-protocol/src/schemas.rs` (the `pub use gents_schemas::{...}` block that opens at line 9; `ALL` before its close at line 156; `ALL_COLLECTION_NAMES` before its close at line 231)
- Modify: `crates/gents-migration/src/registry.rs` (`DEFAULT_BASELINE`, after the `EVAL_VERDICT` entry that ends at line 604)

**Interfaces:**
- Consumes: `gents_schemas::{ALL, ALL_COLLECTION_NAMES, LOCAL_AUDIT_COLLECTION_NAMES, BRANCHABLE_COLLECTION_NAMES, is_local_audit_collection}`; the macro `baseline_entry!($name, $sdl, $version)` from `crates/gents-migration/src/registry.rs:220-229`.
- Produces:
  ```rust
  // crates/gents-schemas/src/lib.rs, re-exported from gents_protocol::schemas
  pub const OPTIMIZATION_JOB_NAME: &str = "OptimizationJob";
  pub const OPTIMIZATION_JOB: &str = include_str!("../schemas/agent/optimization_job.graphql");
  ```

- [ ] **Step 0: Verify the M6b base**

Run, from the worktree root:

```bash
git grep -n "pub async fn resume" -- crates/gents/src/eval/runner/mod.rs
git grep -n '"OptimizationJob"' -- crates/gents/src/document_config/write_tool.rs
git grep -n "pub fn with_expected" -- crates/gents/src/config_client/desired_state.rs
git grep -n "pub fn evidence_from_pairs" -- crates/gents/src/optimization/policy.rs
```

Expected: exactly one line from each command. If any prints nothing, the base is wrong: stop and message the orchestrator; do not rebuild it yourself.

- [ ] **Step 1: Write the failing test** — append to the `#[cfg(test)] mod tests` block in `crates/gents-schemas/src/lib.rs`, immediately after `eval_placement_is_decided_per_collection` (which ends at line 624):

```rust
    #[test]
    fn the_optimization_job_is_local_audit_and_never_bulk_synced() {
        assert!(ALL_COLLECTION_NAMES.contains(&OPTIMIZATION_JOB_NAME));
        assert!(
            is_local_audit_collection(OPTIMIZATION_JOB_NAME),
            "the job journal holds candidate prompts"
        );
        assert!(
            !BRANCHABLE_COLLECTION_NAMES.contains(&OPTIMIZATION_JOB_NAME),
            "a job is the driver's local notebook and is never bulk-synced"
        );
        // The guard field the append transaction filters on, and the derived
        // state that is deliberately not called `lifecycle_state`.
        assert!(OPTIMIZATION_JOB.contains("journal_len: Int"));
        assert!(OPTIMIZATION_JOB.contains("state: String @index"));
        assert!(
            !OPTIMIZATION_JOB.contains("lifecycle_state"),
            "a job is not a request and never grows a second request lifecycle"
        );
    }
```

- [ ] **Step 2: Run it to see it fail**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents-schemas the_optimization_job_is_local_audit_and_never_bulk_synced`
Expected: FAIL to compile, `cannot find value OPTIMIZATION_JOB_NAME in this scope`.

- [ ] **Step 3: Write the SDL**

Create `crates/gents-schemas/schemas/agent/optimization_job.graphql`:

```graphql
# Frozen origin and an append-only journal for one configuration optimization
# job (#1455). The driver's durable notebook: the runtime never claims it, and
# `state` is derived from the journal, never a second request lifecycle.
# Local-only: the journal carries candidate system prompts.
type OptimizationJob @index(fields: ["owner_agent_did", "job_id"], unique: true) {
    job_id: String @immutable
    owner_agent_did: String @index @immutable
    origin: JSON @immutable
    journal: String
    journal_len: Int
    state: String @index
    created_at: String @index @immutable
}
```

No `@branchable`: the collection is never bulk-synced, and `EthSubmission` (`schemas/agent/eth_submission.graphql`), the other local-audit journal, carries no `@branchable` either. `journal` is a `String` holding a JSON array rather than a `JSON` column, because the append transaction filters on `journal_len` and DefraDB gives no array-length predicate.

- [ ] **Step 4: Register it in `crates/gents-schemas/src/lib.rs`**

After the `EVAL_VERDICT` constants (lines 109-110) add:

```rust
pub const OPTIMIZATION_JOB_NAME: &str = "OptimizationJob";
pub const OPTIMIZATION_JOB: &str = include_str!("../schemas/agent/optimization_job.graphql");
```

Append `    OPTIMIZATION_JOB,` as the last element of `ALL` (after `EVAL_VERDICT,` at line 220) and `    OPTIMIZATION_JOB_NAME,` as the last element of `ALL_COLLECTION_NAMES` (after `EVAL_VERDICT_NAME,` at line 289). The two arrays are index-aligned and `collection_names_align_with_sdl_arrays` enforces it. Then add `    OPTIMIZATION_JOB_NAME,` as the last element of `LOCAL_AUDIT_COLLECTION_NAMES` (after `EVAL_VERDICT_NAME,` at line 334). Add nothing to `BRANCHABLE_COLLECTION_NAMES`.

- [ ] **Step 5: Run the catalog tests**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents-schemas`
Expected: PASS, including `all_contains_every_agent_schema_file` (it counts `.graphql` files in `schemas/agent/` against `ALL.len()`), `every_agent_schema_starts_with_type_declaration`, `collection_names_align_with_sdl_arrays`, `prompt_bearing_audit_facts_are_classified_and_not_bulk_synced` and the new test.

- [ ] **Step 6: Mirror it in `crates/gents-protocol/src/schemas.rs`**

Add `OPTIMIZATION_JOB, OPTIMIZATION_JOB_NAME,` to the `pub use gents_schemas::{...}` list that opens at line 9, in rustfmt's order. Append `    OPTIMIZATION_JOB,` as the last element of that file's `ALL` (after `EVAL_VERDICT,` at line 155) and `    OPTIMIZATION_JOB_NAME,` as the last element of its `ALL_COLLECTION_NAMES` (after `EVAL_VERDICT_NAME,` at line 230). `LOCAL_AUDIT_COLLECTION_NAMES` is re-exported wholesale at line 27, so it needs no edit.

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents-protocol`
Expected: PASS, including `all_contains_every_schema` and `collection_names_align_with_sdl_arrays`.

- [ ] **Step 7: Author the migration baseline pin**

Append to `DEFAULT_BASELINE` in `crates/gents-migration/src/registry.rs`, as the last entry, after the `EVAL_VERDICT` entry that ends at line 604:

```rust
    baseline_entry!(
        gents_protocol::schemas::OPTIMIZATION_JOB_NAME,
        gents_protocol::schemas::OPTIMIZATION_JOB,
        "PIN"
    ),
```

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents-migration --test phase_b_steps canonical_catalog_pins_for_authoring -- --nocapture`
Expected: FAIL with a mismatch line of the form `OptimizationJob: expected Some("PIN"), computed bafyrei<...>`. Copy the reported `bafyrei...` value over the `"PIN"` literal. The pin is a value read from the repo at implementation time; this test is the only place it exists.

- [ ] **Step 8: Run the migration suite**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents-migration`
Expected: PASS, including `default_baseline_matches_ordered_protocol_catalog` (which asserts `DEFAULT_BASELINE` follows the catalog order exactly, so the entry must be last) and `default_baseline_covers_every_protocol_collection_once` (which asserts every entry's `expected_version.is_some()`).

- [ ] **Step 9: Confirm it never replicates**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents-desktop-core the_desktop_does_not_replicate_plaintext_provider_bodies`
Expected: PASS. That test iterates `gents_protocol::schemas::LOCAL_AUDIT_COLLECTION_NAMES` and asserts none of them is in the desktop's `subscribed_collection_names()`, so it covers the new name without an edit.

Run: `grep -n "OptimizationJob" crates/gents/src/agent/p2p_reconcile/templates.rs`
Expected: no output. The three arrays `CONVERSATION_COLLECTIONS` (line 230), `CLIENT_COLLECTIONS` (line 271) and `CLIENT_TO_RUNTIME_COLLECTIONS` (line 310) must not name it.

- [ ] **Step 10: Commit**

```bash
git add crates/gents-schemas crates/gents-protocol crates/gents-migration
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" \
  commit -m "feat(schemas): local-only OptimizationJob collection

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

### Task 2: The protected-collection constant

**Files:**
- Modify: `crates/gents/src/document_config/write_tool.rs` (`PROTECTED_DATASTORE_COLLECTIONS` and its `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `gents_protocol::schemas::OPTIMIZATION_JOB_NAME` (PR 1 Task 1); the existing
  ```rust
  pub const PROTECTED_DATASTORE_COLLECTIONS: &[&str];
  pub(crate) fn reject_protected_collection_name(collection: &str) -> Result<()>;
  ```
- Produces: no new names. `PROTECTED_DATASTORE_COLLECTIONS` stops carrying a string literal.

- [ ] **Step 1: Write the failing test** — replace the second loop of `eval_and_optimization_collections_are_never_exposed` in the `#[cfg(test)] mod tests` at the bottom of `write_tool.rs` with a constant-driven one:

```rust
    #[test]
    fn eval_and_optimization_collections_are_never_exposed() {
        for name in super::PROTECTED_DATASTORE_COLLECTIONS {
            let error = super::reject_protected_collection_name(name).unwrap_err();
            assert!(format!("{error:#}").contains("protected"), "{error:#}");
        }
        for name in [
            gents_protocol::schemas::EVAL_DEFINITION_NAME,
            gents_protocol::schemas::EVAL_RUN_NAME,
            gents_protocol::schemas::EVAL_TRIAL_NAME,
            gents_protocol::schemas::EVAL_VERDICT_NAME,
            gents_protocol::schemas::OPTIMIZATION_JOB_NAME,
        ] {
            assert!(
                super::PROTECTED_DATASTORE_COLLECTIONS.contains(&name),
                "{name}"
            );
        }
        assert!(super::reject_protected_collection_name("Notes").is_ok());
        // The list is now entirely schema constants: a literal here would drift
        // from the catalog the moment a collection is renamed.
        assert!(
            super::PROTECTED_DATASTORE_COLLECTIONS
                .iter()
                .all(|name| gents_protocol::schemas::ALL_COLLECTION_NAMES.contains(name)),
            "every protected name must be a registered collection"
        );
    }
```

- [ ] **Step 2: Run it to see it fail**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents --lib document_config::write_tool`
Expected: FAIL. The literal `"OptimizationJob"` is not in `ALL_COLLECTION_NAMES`'s `&&str` comparison shape until the constant replaces it, and the final assertion reports `every protected name must be a registered collection`.

- [ ] **Step 3: Replace the literal**

In `write_tool.rs`, change the constant and drop the now-stale sentence from its doc comment:

```rust
/// Collections no datastore tool may read or write. They hold protected eval
/// material and promotion evidence; an agent in the launching home, including
/// the live version of a behavior under optimization, must not reach them.
pub const PROTECTED_DATASTORE_COLLECTIONS: &[&str] = &[
    gents_protocol::schemas::EVAL_DEFINITION_NAME,
    gents_protocol::schemas::EVAL_RUN_NAME,
    gents_protocol::schemas::EVAL_TRIAL_NAME,
    gents_protocol::schemas::EVAL_VERDICT_NAME,
    gents_protocol::schemas::OPTIMIZATION_JOB_NAME,
];
```

- [ ] **Step 4: Run the whole protected surface**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents --lib protected_collection`
Expected: PASS, including `surface_entries_cannot_name_a_protected_collection` (`document_config/surface_tool.rs`), `rejects_a_persisted_protected_collection_at_execution` (`defra_query/bounded.rs`) and `rejects_a_persisted_protected_collection_at_execution` (`defra_write/tests.rs`).

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents --lib document_config::write_tool`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/gents/src/document_config/write_tool.rs
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" \
  commit -m "refactor(config): protect OptimizationJob by its schema constant

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

### Task 3: The closure target

**Files:**
- Create: `crates/gents/src/optimization/target.rs`
- Modify: `crates/gents/src/optimization/mod.rs`

This file is the one the spec calls "`target.rs` carries over" (spec section 10). It is pure apart from one transactional read, and it is the only thing that builds a patch from a proposed text. It lands in PR 1 rather than PR 2 because `JobOrigin` in Task 4 is declared against `Target` and `FrozenDocument`; a forward declaration would be the alternative and this is cheaper.

Two rulings shape it. R2: v1 has exactly one target field, `AgentContext.system_prompt`, so `TargetField` has one variant and no placeholder-set check exists. F5: the frozen closure excludes `EvalDefinition` documents. `Collection::ALL` includes `EvalDefinition` (`crates/gents/src/collection.rs`, `ALL` array), so `ConfigReferences::load_in_txn` returns the owner's definitions too; a definition is the instrument, not the subject, and it is frozen separately in `JobOrigin::definition`. Without the exclusion, editing a definition would read as `baseline_drifted` and promotion would demand the definition be unchanged, when the spec calls the first `definition_changed` and never mentions the second.

**Interfaces:**
- Consumes:
  ```rust
  // crates/gents/src/config_client/desired_state.rs
  pub fn desired_state_document_digest(value: &Value) -> Result<String>;
  pub struct DesiredStateApplyDocument { pub collection: Collection, pub add: Value, pub update: Value }
  impl DesiredStateApplyPlan {
      pub fn new(documents: Vec<DesiredStateApplyDocument>) -> Result<Self>;
      pub fn with_expected(self, expected: Vec<DesiredStateExpectation>) -> Result<Self>;
  }
  pub struct DesiredStateExpectation { pub collection: Collection, pub owner: String,
      pub id: String, pub digest: Option<String> }
  // crates/gents/src/document_config/references.rs
  impl ConfigReferences {
      pub async fn load_in_txn(txn: &ConfigApplyTxn<'_>, agent_did: &str) -> Result<Self>;
      pub(crate) fn documents(&self) -> impl Iterator<Item = (&(Collection, String), &Value)>;
  }
  // crates/gents/src/collection.rs
  impl Collection { pub fn graphql_type(&self) -> &'static str; pub fn unique_field(&self) -> &'static str; }
  ```
- Produces:
  ```rust
  pub const MAX_TARGET_TEXT_BYTES: usize = 32 * 1024;
  pub enum TargetField { AgentContextSystemPrompt }
  impl TargetField {
      pub fn collection(&self) -> Collection;
      pub fn field_name(&self) -> &'static str;
  }
  pub struct Target { pub field: TargetField, pub owner: String, pub id: String }
  pub struct FrozenDocument { pub collection: Collection, pub owner: String, pub id: String, pub digest: String }
  pub type Closure = Vec<(Collection, Value)>;
  /// Every document `ConfigReferences::load_in_txn` returns except `EvalDefinition`.
  pub async fn capture_closure(txn: &ConfigApplyTxn<'_>, owner: &str) -> Result<Closure>;
  pub(crate) fn is_closure_collection(collection: Collection) -> bool;
  pub fn closure_digests(closure: &Closure) -> Result<Vec<FrozenDocument>>;
  pub fn current_text(closure: &Closure, target: &Target) -> Result<String>;
  pub fn apply_text(closure: &Closure, target: &Target, text: &str) -> Result<Closure>;
  pub fn target_digest(closure: &Closure, target: &Target) -> Result<String>;
  pub fn expectations(frozen: &[FrozenDocument]) -> Vec<DesiredStateExpectation>;
  pub fn target_plan(closure: &Closure, target: &Target, frozen: &[FrozenDocument]) -> Result<DesiredStateApplyPlan>;
  ```

- [ ] **Step 1: Write the failing test** — create `crates/gents/src/optimization/target.rs` holding only this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const OWNER: &str = "did:key:target-owner";

    fn context(prompt: &str) -> Value {
        json!({
            "context_id": "monitor-context",
            "agent_did": OWNER,
            "display_name": "Monitor",
            "system_prompt": prompt,
        })
    }

    fn closure(prompt: &str) -> Closure {
        vec![
            (
                Collection::AgentBehavior,
                json!({
                    "behavior_id": "monitor",
                    "agent_did": OWNER,
                    "context_id": "monitor-context",
                    "inference_profile_id": "local",
                }),
            ),
            (Collection::AgentContext, context(prompt)),
        ]
    }

    fn target() -> Target {
        Target {
            field: TargetField::AgentContextSystemPrompt,
            owner: OWNER.into(),
            id: "monitor-context".into(),
        }
    }

    #[test]
    fn the_patch_replaces_one_field_of_one_document_and_nothing_else() {
        let before = closure("Watch the mailbox.\n");
        assert_eq!(current_text(&before, &target()).unwrap(), "Watch the mailbox.\n");

        let after = apply_text(&before, &target(), "Watch the mailbox, and say why.\n").unwrap();
        assert_eq!(after.len(), before.len());
        assert_eq!(after[0], before[0], "the behavior is untouched");
        assert_eq!(
            current_text(&after, &target()).unwrap(),
            "Watch the mailbox, and say why.\n"
        );
        let mut masked = after[1].1.clone();
        masked["system_prompt"] = before[1].1["system_prompt"].clone();
        assert_eq!(masked, before[1].1, "no other field of the target moved");
        assert_ne!(
            target_digest(&after, &target()).unwrap(),
            target_digest(&before, &target()).unwrap()
        );
    }

    #[test]
    fn a_target_outside_the_closure_is_an_error_not_a_silent_insert() {
        let missing = Target {
            id: "no-such-context".into(),
            ..target()
        };
        let error = apply_text(&closure("a"), &missing, "b").unwrap_err();
        assert!(
            format!("{error:#}").contains("not in the baseline closure"),
            "{error:#}"
        );
        let foreign = Target {
            owner: "did:key:someone-else".into(),
            ..target()
        };
        assert!(current_text(&closure("a"), &foreign).is_err());
    }

    #[test]
    fn the_plan_writes_only_the_target_and_expects_the_whole_closure() {
        let before = closure("Watch the mailbox.\n");
        let frozen = closure_digests(&before).unwrap();
        assert_eq!(frozen.len(), 2);
        let after = apply_text(&before, &target(), "New text.\n").unwrap();
        let plan = target_plan(&after, &target(), &frozen).unwrap();
        assert_eq!(plan.documents().len(), 1, "only the target document is written");
        assert_eq!(plan.documents()[0].collection, Collection::AgentContext);
        assert_eq!(plan.documents()[0].add, plan.documents()[0].update);
        assert_eq!(plan.expected().len(), 2, "the whole frozen closure is expected");
        assert!(plan
            .expected()
            .iter()
            .all(|expectation| expectation.digest.is_some()));
    }

    /// Ruling R2: v1 has exactly one target field.
    #[test]
    fn the_one_target_field_is_the_context_system_prompt() {
        assert_eq!(
            TargetField::AgentContextSystemPrompt.collection(),
            Collection::AgentContext
        );
        assert_eq!(
            TargetField::AgentContextSystemPrompt.field_name(),
            "system_prompt"
        );
        assert_eq!(
            serde_json::to_value(TargetField::AgentContextSystemPrompt).unwrap(),
            json!("agent_context_system_prompt"),
            "the frozen wire form of the target field"
        );
        assert!(
            serde_json::from_value::<TargetField>(json!("task_prompt_template")).is_err(),
            "no second target field exists in v1"
        );
    }

    /// Finding F5: a definition is frozen in `JobOrigin::definition`, never in
    /// the closure, so editing it is `definition_changed` and not drift.
    #[test]
    fn eval_definitions_never_enter_the_closure() {
        assert!(!is_closure_collection(Collection::EvalDefinition));
        assert!(is_closure_collection(Collection::AgentContext));
        assert!(is_closure_collection(Collection::AgentBehavior));
    }
}
```

- [ ] **Step 2: Run it to see it fail**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents --lib optimization::target`
Expected: FAIL to compile, `cannot find type Target in this scope`.

- [ ] **Step 3: Write the implementation** — above the test module:

```rust
//! The target: the one field of the one document an optimization job may
//! change, and the closure that field sits in.
//!
//! Nothing here consults a proposer or a model. A proposed text arrives as a
//! value and this module turns it into a patch deterministically, which is why
//! `Proposer` never needs a `ConfigAccess`: the only writer of a patch is here.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config_client::{
    desired_state_document_digest, ConfigApplyTxn, DesiredStateApplyDocument, DesiredStateApplyPlan,
    DesiredStateExpectation,
};
use crate::Collection;

/// The cap the structural gate holds a proposed text to.
pub const MAX_TARGET_TEXT_BYTES: usize = 32 * 1024;

/// The one field an optimization job may change in v1 (ruling R2). A Task's
/// `prompt_template` is deferred; adding it is a new variant and a new
/// structural check, not a flag on this one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetField {
    AgentContextSystemPrompt,
}

impl TargetField {
    pub fn collection(&self) -> Collection {
        match self {
            Self::AgentContextSystemPrompt => Collection::AgentContext,
        }
    }

    pub fn field_name(&self) -> &'static str {
        match self {
            Self::AgentContextSystemPrompt => "system_prompt",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Target {
    pub field: TargetField,
    pub owner: String,
    pub id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrozenDocument {
    pub collection: Collection,
    pub owner: String,
    pub id: String,
    pub digest: String,
}

/// The owner's full desired configuration, in canonical projection form.
pub type Closure = Vec<(Collection, Value)>;

/// Whether documents of `collection` belong to the frozen closure. An eval
/// definition is the instrument, not the subject: it is frozen separately in
/// `JobOrigin::definition`, and editing it is `definition_changed`.
pub(crate) fn is_closure_collection(collection: Collection) -> bool {
    collection != Collection::EvalDefinition
}

/// Read the owner's configuration inside `txn`, ordered so two reads of the
/// same state produce the same closure.
pub async fn capture_closure(txn: &ConfigApplyTxn<'_>, owner: &str) -> Result<Closure> {
    let references = crate::ConfigReferences::load_in_txn(txn, owner).await?;
    let mut closure: Closure = references
        .documents()
        .filter(|((collection, _), _)| is_closure_collection(*collection))
        .map(|((collection, _), value)| (*collection, value.clone()))
        .collect();
    closure.sort_by_key(|(collection, value)| {
        (
            *collection,
            document_id(*collection, value).unwrap_or_default(),
        )
    });
    Ok(closure)
}

fn document_id(collection: Collection, value: &Value) -> Option<String> {
    value
        .get(collection.unique_field())?
        .as_str()
        .map(str::to_owned)
}

fn document_owner(value: &Value) -> Option<String> {
    value.get("agent_did")?.as_str().map(str::to_owned)
}

pub fn closure_digests(closure: &Closure) -> Result<Vec<FrozenDocument>> {
    closure
        .iter()
        .map(|(collection, value)| {
            Ok(FrozenDocument {
                collection: *collection,
                owner: document_owner(value).context("closure document has no agent_did")?,
                id: document_id(*collection, value).context("closure document has no logical ID")?,
                digest: desired_state_document_digest(value)?,
            })
        })
        .collect()
}

fn target_index(closure: &Closure, target: &Target) -> Result<usize> {
    closure
        .iter()
        .position(|(collection, value)| {
            *collection == target.field.collection()
                && document_id(*collection, value).as_deref() == Some(target.id.as_str())
                && document_owner(value).as_deref() == Some(target.owner.as_str())
        })
        .with_context(|| {
            format!(
                "target {} {:?}/{:?} is not in the baseline closure",
                target.field.collection().graphql_type(),
                target.owner,
                target.id
            )
        })
}

pub fn current_text(closure: &Closure, target: &Target) -> Result<String> {
    let (_, value) = &closure[target_index(closure, target)?];
    Ok(value
        .get(target.field.field_name())
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned())
}

/// The closure with exactly one field of exactly one document replaced.
pub fn apply_text(closure: &Closure, target: &Target, text: &str) -> Result<Closure> {
    let index = target_index(closure, target)?;
    let mut patched = closure.clone();
    patched[index]
        .1
        .as_object_mut()
        .context("target document is not an object")?
        .insert(
            target.field.field_name().to_owned(),
            Value::String(text.to_owned()),
        );
    Ok(patched)
}

pub fn target_digest(closure: &Closure, target: &Target) -> Result<String> {
    desired_state_document_digest(&closure[target_index(closure, target)?].1)
}

/// Every frozen document as a digest precondition. Promotion expects the whole
/// closure and writes one document of it.
pub fn expectations(frozen: &[FrozenDocument]) -> Vec<DesiredStateExpectation> {
    frozen
        .iter()
        .map(|document| DesiredStateExpectation {
            collection: document.collection,
            owner: document.owner.clone(),
            id: document.id.clone(),
            digest: Some(document.digest.clone()),
        })
        .collect()
}

/// A plan that writes only `target` out of `closure`, guarded by `frozen`.
pub fn target_plan(
    closure: &Closure,
    target: &Target,
    frozen: &[FrozenDocument],
) -> Result<DesiredStateApplyPlan> {
    let (collection, value) = &closure[target_index(closure, target)?];
    DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: *collection,
        add: value.clone(),
        update: value.clone(),
    }])?
    .with_expected(expectations(frozen))
}
```

- [ ] **Step 4: Wire the module** — in `crates/gents/src/optimization/mod.rs` add `pub mod target;` in rustfmt order and:

```rust
pub use target::{
    apply_text, capture_closure, closure_digests, current_text, expectations, target_digest,
    target_plan, Closure, FrozenDocument, Target, TargetField, MAX_TARGET_TEXT_BYTES,
};
```

- [ ] **Step 5: Run the tests**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents --lib optimization::target`
Expected: PASS, 5 tests.

- [ ] **Step 6: Commit**

```bash
git add crates/gents/src/optimization
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" \
  commit -m "feat(optimization): the closure target and its guarded patch (#1455)

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

### Task 4: The typed, length-guarded journal

**Files:**
- Create: `crates/gents/src/optimization/job.rs`
- Modify: `crates/gents/src/optimization/mod.rs`

**Interfaces:**
- Consumes:
  ```rust
  // crates/gents/src/config_client/mod.rs and txn.rs
  pub enum ConfigAccess { Graphql(String), Local(Arc<EmbeddedNode>) }
  impl ConfigAccess {
      pub async fn transact<'a, T, F>(&'a self, operation: &'static str, callback: F) -> Result<T>
      where T: Send + 'a,
            F: for<'txn> Fn(&'txn ConfigApplyTxn<'a>) -> BoxFuture<'txn, Result<T>> + Send + 'a;
  }
  // crates/gents/src/graphql.rs
  pub fn escape_graphql_string(value: &str) -> String;
  // crates/gents/src/optimization/policy.rs
  pub struct PolicyV2 { /* 12 pub fields */ }
  pub enum Decision { Accept, Reject(RejectReason), Inconclusive(InconclusiveReason) }
  pub enum Mode { Improve, Confirm }
  pub struct DecisionReport { pub decision: Decision, pub policy_version: String, pub improved: u32,
      pub tied: u32, pub worsened: u32, pub mean_diff_bp: Option<i64>, pub p_ppm: Option<u64>,
      pub alpha_effective_ppm: u64, pub cost_skipped: bool }
  // crates/gents/src/eval/documents.rs
  pub struct DefinitionRef { pub definition_id: String, pub comparability_version: i64, pub digest: String }
  pub struct SubjectRef { pub pack_digest: String, pub behavior_id: String }
  // crates/gents/src/document_config/eval_definition.rs
  pub enum EvalSplit { Train, Validation, HeldOut }
  ```
  ```rust
  // crates/gents/src/optimization/target.rs, PR 1 Task 3
  pub struct Target { pub field: TargetField, pub owner: String, pub id: String }
  pub struct FrozenDocument { pub collection: Collection, pub owner: String, pub id: String, pub digest: String }
  ```
- Produces:
  ```rust
  pub struct Budgets { pub max_rounds: u32, pub max_case_trials: u64, pub max_tokens: u64,
      pub deadline_unix_secs: Option<u64> }
  pub struct JobOrigin { pub target: Target, pub closure: Vec<FrozenDocument>, pub subject: SubjectRef,
      pub definition: DefinitionRef, pub policy: PolicyV2, pub trials_per_case: u32,
      pub budgets: Budgets, pub baseline_text: String, pub owner: String, pub seed_base: i64,
      pub inference_profile_id: String, pub max_text_bytes: usize, pub jobs_dir: PathBuf }
  pub enum JobState { Running, ReadyToPromote, NothingToPromote, Exhausted,
      Failed { reason: String }, Promoted, Stale, Reverted }
  impl JobState { pub fn label(&self) -> &'static str; }
  pub struct DriftedRef { pub collection: String, pub id: String }
  pub struct DecisionSummary { pub improved: u32, pub tied: u32, pub worsened: u32,
      pub mean_diff_bp: Option<i64>, pub p_ppm: Option<u64>, pub alpha_effective_ppm: u64,
      pub cost_skipped: bool }
  pub enum JournalEntry {
      Frozen,
      RunStarted { run_id: String, round: Option<u32>, split: EvalSplit },
      Proposed { round: u32, text: String, rationale: String, candidate_digest: String },
      StructuralReject { round: u32, diagnostics: String },
      Decided { round: Option<u32>, attempt: u32, run_ids: Vec<String>, mode: Mode,
          decision: Decision, policy_version: String, summary: DecisionSummary },
      BudgetExhausted { round: Option<u32>, reason: String },
      Finalized { state: JobState },
      Promoted { by: String, target_digest: String, previous_text: String, previous_digest: String },
      PromotionRefused { drifted: Vec<DriftedRef> },
      Reverted { by: String },
  }
  pub struct JobRecord { pub job_id: String, pub owner: String, pub origin: JobOrigin,
      pub journal: Vec<JournalEntry> }
  pub struct Checkpoint { pub round: u32, pub text: String, pub pack_digest: String }
  pub fn derive_state(journal: &[JournalEntry]) -> JobState;
  pub fn checkpoint(journal: &[JournalEntry]) -> Option<Checkpoint>;
  pub fn rounds_used(journal: &[JournalEntry]) -> u32;
  pub struct JournalConflict { pub expected: usize }
  pub fn journal_conflict(error: &anyhow::Error) -> Option<&JournalConflict>;
  pub async fn create_job(access: &ConfigAccess, job_id: &str, owner: &str, origin: &JobOrigin) -> Result<JobRecord>;
  pub async fn load_job(access: &ConfigAccess, owner: &str, job_id: &str) -> Result<Option<JobRecord>>;
  pub(crate) async fn load_job_in_txn(txn: &ConfigApplyTxn<'_>, owner: &str, job_id: &str) -> Result<Option<JobRecord>>;
  pub(crate) async fn append_in_txn(txn: &ConfigApplyTxn<'_>, job: &JobRecord, entry: &JournalEntry) -> Result<()>;
  pub async fn append(access: &ConfigAccess, job: &mut JobRecord, entry: JournalEntry) -> Result<()>;
  ```

- [ ] **Step 1: Write the failing test** — create `crates/gents/src/optimization/job.rs` holding only this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::optimization::policy::{InconclusiveReason, RejectReason};
    use crate::optimization::target::{Target, TargetField};
    use std::sync::Arc;

    async fn access() -> ConfigAccess {
        let node = Arc::new(crate::defra_node::EmbeddedNode::builder().build().await.unwrap());
        crate::schema::ensure_runtime_schemas(&node).await.unwrap();
        ConfigAccess::Local(node)
    }

    const OWNER: &str = "did:key:job-owner";

    fn origin() -> JobOrigin {
        JobOrigin {
            target: Target {
                field: TargetField::AgentContextSystemPrompt,
                owner: OWNER.into(),
                id: "monitor-context".into(),
            },
            closure: Vec::new(),
            subject: crate::eval::SubjectRef {
                pack_digest: "sha256:baseline".into(),
                behavior_id: "monitor".into(),
            },
            definition: crate::eval::DefinitionRef {
                definition_id: "monitor-findings".into(),
                comparability_version: 1,
                digest: "sha256:definition".into(),
            },
            policy: PolicyV2::uncalibrated(),
            trials_per_case: 2,
            budgets: Budgets {
                max_rounds: 3,
                max_case_trials: 1_000,
                max_tokens: 1_000_000,
                deadline_unix_secs: None,
            },
            baseline_text: "Watch the mailbox.\n".into(),
            owner: OWNER.into(),
            seed_base: 1_000,
            inference_profile_id: "local".into(),
            max_text_bytes: 32 * 1024,
            jobs_dir: std::path::PathBuf::from("/home/eval/jobs"),
        }
    }

    fn summary() -> DecisionSummary {
        DecisionSummary {
            improved: 6,
            tied: 0,
            worsened: 0,
            mean_diff_bp: Some(3_000),
            p_ppm: Some(15_625),
            alpha_effective_ppm: 16_666,
            cost_skipped: true,
        }
    }

    fn proposed(round: u32, text: &str) -> JournalEntry {
        JournalEntry::Proposed {
            round,
            text: text.into(),
            rationale: format!("round {round}"),
            candidate_digest: format!("sha256:candidate-{round}"),
        }
    }

    fn decided(round: u32, decision: Decision) -> JournalEntry {
        JournalEntry::Decided {
            round: Some(round),
            attempt: 0,
            run_ids: vec![format!("job-r{round}-v0")],
            mode: Mode::Improve,
            decision,
            policy_version: crate::optimization::POLICY_VERSION.to_owned(),
            summary: summary(),
        }
    }

    #[tokio::test]
    async fn a_job_round_trips_through_the_document() {
        let access = access().await;
        let mut job = create_job(&access, "job-1", OWNER, &origin()).await.unwrap();
        append(&access, &mut job, JournalEntry::Frozen).await.unwrap();
        append(
            &access,
            &mut job,
            JournalEntry::RunStarted {
                run_id: "job-1-r1-train".into(),
                round: Some(1),
                split: EvalSplit::Train,
            },
        )
        .await
        .unwrap();
        let loaded = load_job(&access, OWNER, "job-1").await.unwrap().unwrap();
        assert_eq!(loaded, job);
        assert_eq!(derive_state(&loaded.journal), JobState::Running);
    }

    /// `Optimization.appendIf_match` and `appendIf_prefix`: an append at the
    /// length the writer read extends the journal by exactly one entry, and
    /// every earlier entry is preserved in order.
    #[tokio::test]
    async fn an_append_at_the_expected_length_extends_the_journal_by_one() {
        let access = access().await;
        let mut job = create_job(&access, "job-2", OWNER, &origin()).await.unwrap();
        let entries = [JournalEntry::Frozen, proposed(1, "one"), decided(1, Decision::Accept)];
        for entry in entries.iter().cloned() {
            let before = job.journal.clone();
            append(&access, &mut job, entry.clone()).await.unwrap();
            assert_eq!(job.journal.len(), before.len() + 1);
            assert_eq!(&job.journal[..before.len()], &before[..], "a prefix is never rewritten");
            assert_eq!(job.journal.last(), Some(&entry));
        }
        let loaded = load_job(&access, OWNER, "job-2").await.unwrap().unwrap();
        assert_eq!(loaded.journal, entries.to_vec());
    }

    /// `Optimization.appendIf_stale_unchanged`: an append whose expected length
    /// does not match leaves the journal exactly as it was.
    #[tokio::test]
    async fn a_stale_writer_cannot_append_and_changes_nothing() {
        let access = access().await;
        let mut job = create_job(&access, "job-3", OWNER, &origin()).await.unwrap();
        let mut stale = job.clone();
        append(&access, &mut job, JournalEntry::Frozen).await.unwrap();

        let error = append(&access, &mut stale, proposed(1, "racing"))
            .await
            .unwrap_err();
        let conflict = journal_conflict(&error).unwrap_or_else(|| panic!("{error:#}"));
        assert_eq!(conflict.expected, 0);
        assert!(stale.journal.is_empty(), "a refused append never mutates the caller's copy");

        let loaded = load_job(&access, OWNER, "job-3").await.unwrap().unwrap();
        assert_eq!(loaded.journal, vec![JournalEntry::Frozen]);
        assert_eq!(derive_state(&loaded.journal), JobState::Running);
    }

    #[test]
    fn the_checkpoint_is_the_last_accepted_round_and_never_the_best_seen() {
        let journal = vec![
            JournalEntry::Frozen,
            proposed(1, "one"),
            decided(1, Decision::Accept),
            proposed(2, "two"),
            decided(2, Decision::Reject(RejectReason::NoImprovement)),
            proposed(3, "three"),
            decided(3, Decision::Inconclusive(InconclusiveReason::Insufficient)),
        ];
        let retained = checkpoint(&journal).expect("round 1 was accepted");
        assert_eq!((retained.round, retained.text.as_str()), (1, "one"));
        assert_eq!(retained.pack_digest, "sha256:candidate-1");
        assert_eq!(checkpoint(&journal[..2]), None, "a proposal alone is not a checkpoint");
        assert_eq!(rounds_used(&journal), 3, "every Proposed is a spent round");
    }

    #[test]
    fn the_state_is_derived_from_the_terminal_entries_in_order() {
        let mut journal = vec![JournalEntry::Frozen];
        assert_eq!(derive_state(&journal), JobState::Running);
        journal.push(JournalEntry::Finalized { state: JobState::ReadyToPromote });
        assert_eq!(derive_state(&journal), JobState::ReadyToPromote);
        journal.push(JournalEntry::PromotionRefused {
            drifted: vec![DriftedRef { collection: "AgentContext".into(), id: "monitor-context".into() }],
        });
        assert_eq!(derive_state(&journal), JobState::Stale);
        journal.push(JournalEntry::Promoted {
            by: OWNER.into(),
            target_digest: "sha256:after".into(),
            previous_text: "Watch the mailbox.\n".into(),
            previous_digest: "sha256:before".into(),
        });
        assert_eq!(derive_state(&journal), JobState::Promoted);
        journal.push(JournalEntry::Reverted { by: OWNER.into() });
        assert_eq!(
            derive_state(&journal),
            JobState::Reverted,
            "ruling R4: a revert is terminal; a further promotion is a new job"
        );
        assert_eq!(JobState::Reverted.label(), "reverted");
    }
}
```

- [ ] **Step 2: Run it to see it fail**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents --lib optimization::job`
Expected: FAIL to compile, `cannot find function create_job in this scope`.

- [ ] **Step 3: Write the implementation** — above the test module in `crates/gents/src/optimization/job.rs`:

```rust
//! The `OptimizationJob` document: a frozen origin, an append-only journal
//! guarded by its own length, and a state derived from the journal.
//!
//! This is the Rust refinement of `Optimization.appendIf`. The guard is the
//! `journal_len` predicate on the update mutation, so two writers who read the
//! same journal cannot both extend it: the loser matches no row, gets a
//! [`JournalConflict`], and the journal is left exactly as it was.
//!
//! The job is the driver's notebook, not a request. `state` is derived, never
//! claimed, and no runtime ever reconciles this collection.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::config_client::{ConfigAccess, ConfigApplyTxn};
use crate::document_config::EvalSplit;
use crate::eval::{DefinitionRef, SubjectRef};
use crate::graphql::escape_graphql_string;
use crate::optimization::policy::{Decision, Mode, PolicyV2};
use crate::optimization::target::{FrozenDocument, Target};

/// What a job may spend. `max_rounds` is also the Bonferroni divisor: the
/// driver refuses a job whose policy disagrees with it, so the significance
/// level a decision is judged at always matches the number of candidates the
/// job may try on one validation split.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Budgets {
    pub max_rounds: u32,
    /// Total case-trials across every run of the job, including the reserved
    /// held-out run.
    pub max_case_trials: u64,
    pub max_tokens: u64,
    pub deadline_unix_secs: Option<u64>,
}

/// Frozen at creation and never rewritten.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobOrigin {
    pub target: Target,
    /// The owner's full desired configuration at freeze, as digests.
    pub closure: Vec<FrozenDocument>,
    pub subject: SubjectRef,
    pub definition: DefinitionRef,
    pub policy: PolicyV2,
    pub trials_per_case: u32,
    pub budgets: Budgets,
    /// The target's text at freeze: the first checkpoint, and what `revert`
    /// would restore if nothing had been promoted since.
    pub baseline_text: String,
    pub owner: String,
    pub seed_base: i64,
    /// Frozen so a resumed job cannot silently change the model both arms run
    /// on; a resume that names another profile is refused.
    pub inference_profile_id: String,
    pub max_text_bytes: usize,
    /// `<launching home>/eval/jobs` (ruling R8). The job owns
    /// `<jobs_dir>/<job_id>/`; `promote` rebuilds the checkpoint from the
    /// baseline copy there, so the operator's verb needs only the job id.
    pub jobs_dir: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum JobState {
    Running,
    ReadyToPromote,
    NothingToPromote,
    Exhausted,
    Failed { reason: String },
    Promoted,
    Stale,
    /// Terminal (ruling R4). A further promotion is a new job.
    Reverted,
}

impl JobState {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::ReadyToPromote => "ready_to_promote",
            Self::NothingToPromote => "nothing_to_promote",
            Self::Exhausted => "exhausted",
            Self::Failed { .. } => "failed",
            Self::Promoted => "promoted",
            Self::Stale => "stale",
            Self::Reverted => "reverted",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriftedRef {
    pub collection: String,
    pub id: String,
}

/// The convenience numbers behind a decision. The authority is the referenced
/// runs' `EvalVerdict` rows; `optimization::show::show`
/// recomputes every decision from them and flags a mismatch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionSummary {
    pub improved: u32,
    pub tied: u32,
    pub worsened: u32,
    pub mean_diff_bp: Option<i64>,
    pub p_ppm: Option<u64>,
    pub alpha_effective_ppm: u64,
    pub cost_skipped: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum JournalEntry {
    Frozen,
    /// Written before the runner is asked for a run. One without a matching
    /// `Decided` means the run may be incomplete, and replay resumes it.
    RunStarted {
        run_id: String,
        round: Option<u32>,
        split: EvalSplit,
    },
    Proposed {
        round: u32,
        text: String,
        rationale: String,
        candidate_digest: String,
    },
    StructuralReject {
        round: u32,
        diagnostics: String,
    },
    Decided {
        round: Option<u32>,
        attempt: u32,
        run_ids: Vec<String>,
        mode: Mode,
        decision: Decision,
        policy_version: String,
        summary: DecisionSummary,
    },
    /// A candidate that was never evaluated is not a rejection.
    BudgetExhausted {
        round: Option<u32>,
        reason: String,
    },
    Finalized {
        state: JobState,
    },
    Promoted {
        by: String,
        target_digest: String,
        previous_text: String,
        previous_digest: String,
    },
    PromotionRefused {
        drifted: Vec<DriftedRef>,
    },
    Reverted {
        by: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobRecord {
    pub job_id: String,
    pub owner: String,
    pub origin: JobOrigin,
    pub journal: Vec<JournalEntry>,
}

/// The retained checkpoint: the candidate of the last round the policy
/// accepted. Never the best candidate the job ever saw.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Checkpoint {
    pub round: u32,
    pub text: String,
    pub pack_digest: String,
}

pub fn derive_state(journal: &[JournalEntry]) -> JobState {
    let mut state = JobState::Running;
    for entry in journal {
        match entry {
            JournalEntry::Finalized { state: finalized } => state = finalized.clone(),
            JournalEntry::Promoted { .. } => state = JobState::Promoted,
            JournalEntry::PromotionRefused { .. } => state = JobState::Stale,
            // Terminal, as spec section 6 draws it: `Promoted -> Reverted`.
            JournalEntry::Reverted { .. } => state = JobState::Reverted,
            _ => {}
        }
    }
    state
}

pub fn checkpoint(journal: &[JournalEntry]) -> Option<Checkpoint> {
    let mut retained = None;
    for entry in journal {
        let JournalEntry::Decided {
            round: Some(round),
            decision: Decision::Accept,
            ..
        } = entry
        else {
            continue;
        };
        retained = journal.iter().find_map(|candidate| match candidate {
            JournalEntry::Proposed {
                round: proposed_round,
                text,
                candidate_digest,
                ..
            } if proposed_round == round => Some(Checkpoint {
                round: *round,
                text: text.clone(),
                pack_digest: candidate_digest.clone(),
            }),
            _ => None,
        });
    }
    retained
}

/// `Optimization.roundsUsed`: one per `Proposed`, whatever it came to.
pub fn rounds_used(journal: &[JournalEntry]) -> u32 {
    journal
        .iter()
        .filter(|entry| matches!(entry, JournalEntry::Proposed { .. }))
        .count() as u32
}

/// The append lost the length guard: another writer extended the journal
/// first, so nothing was written.
#[derive(Debug)]
pub struct JournalConflict {
    pub expected: usize,
}

impl std::fmt::Display for JournalConflict {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "optimization job journal moved past length {}; another writer appended first",
            self.expected
        )
    }
}

impl std::error::Error for JournalConflict {}

pub fn journal_conflict(error: &anyhow::Error) -> Option<&JournalConflict> {
    error.downcast_ref::<JournalConflict>()
}

fn filter(owner: &str, job_id: &str) -> String {
    format!(
        r#"owner_agent_did: {{ _eq: "{}" }}, job_id: {{ _eq: "{}" }}"#,
        escape_graphql_string(owner),
        escape_graphql_string(job_id)
    )
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

pub async fn create_job(
    access: &ConfigAccess,
    job_id: &str,
    owner: &str,
    origin: &JobOrigin,
) -> Result<JobRecord> {
    let variables = json!({ "input": {
        "job_id": job_id,
        "owner_agent_did": owner,
        "origin": serde_json::to_value(origin)?,
        "journal": "[]",
        "journal_len": 0,
        "state": JobState::Running.label(),
        "created_at": now(),
    }});
    access
        .transact("optimization.create_job", |txn| {
            let variables = &variables;
            Box::pin(async move {
                txn.execute_with_variables(
                    "mutation($input: OptimizationJobMutationInputArg!) { create_OptimizationJob(input: $input) { _docID } }",
                    variables,
                )
                .await
                .map(|_| ())
            })
        })
        .await
        .context("create_OptimizationJob")?;
    tracing::info!(job_id, owner, "optimization job created");
    Ok(JobRecord {
        job_id: job_id.to_owned(),
        owner: owner.to_owned(),
        origin: origin.clone(),
        journal: Vec::new(),
    })
}

const JOB_FIELDS: &str = "job_id owner_agent_did origin journal journal_len state";

fn decode(row: &Value) -> Result<JobRecord> {
    Ok(JobRecord {
        job_id: row["job_id"].as_str().context("job_id")?.to_owned(),
        owner: row["owner_agent_did"]
            .as_str()
            .context("owner_agent_did")?
            .to_owned(),
        origin: serde_json::from_value(row["origin"].clone()).context("decoding job origin")?,
        journal: serde_json::from_str(row["journal"].as_str().unwrap_or("[]"))
            .context("decoding job journal")?,
    })
}

pub(crate) async fn load_job_in_txn(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    job_id: &str,
) -> Result<Option<JobRecord>> {
    let response = txn
        .execute(&format!(
            "{{ OptimizationJob(filter: {{ {} }}) {{ {JOB_FIELDS} }} }}",
            filter(owner, job_id)
        ))
        .await?;
    response["data"]["OptimizationJob"]
        .as_array()
        .and_then(|rows| rows.first())
        .map(decode)
        .transpose()
}

pub async fn load_job(
    access: &ConfigAccess,
    owner: &str,
    job_id: &str,
) -> Result<Option<JobRecord>> {
    let (owner, job_id) = (owner.to_owned(), job_id.to_owned());
    access
        .transact("optimization.load_job", |txn| {
            let (owner, job_id) = (&owner, &job_id);
            Box::pin(async move { load_job_in_txn(txn, owner, job_id).await })
        })
        .await
}

/// `Optimization.appendIf`. The update matches only while `journal_len` still
/// equals the length this writer read, so a stale writer changes nothing.
pub(crate) async fn append_in_txn(
    txn: &ConfigApplyTxn<'_>,
    job: &JobRecord,
    entry: &JournalEntry,
) -> Result<()> {
    let expected = job.journal.len();
    let mut journal = job.journal.clone();
    journal.push(entry.clone());
    let variables = json!({ "input": {
        "journal": serde_json::to_string(&journal)?,
        "journal_len": journal.len(),
        "state": derive_state(&journal).label(),
    }});
    let response = txn
        .execute_with_variables(
            &format!(
                "mutation($input: OptimizationJobMutationInputArg!) {{ update_OptimizationJob(filter: {{ {}, journal_len: {{ _eq: {expected} }} }}, input: $input) {{ _docID }} }}",
                filter(&job.owner, &job.job_id)
            ),
            &variables,
        )
        .await?;
    let matched = response["data"]["update_OptimizationJob"]
        .as_array()
        .is_some_and(|rows| !rows.is_empty());
    if matched {
        Ok(())
    } else {
        Err(anyhow::Error::new(JournalConflict { expected }))
    }
}

/// Append one entry, advancing `job` only when the write took.
pub async fn append(
    access: &ConfigAccess,
    job: &mut JobRecord,
    entry: JournalEntry,
) -> Result<()> {
    {
        let (current, entry) = (&*job, &entry);
        access
            .transact("optimization.append_journal", |txn| {
                Box::pin(async move { append_in_txn(txn, current, entry).await })
            })
            .await?;
    }
    job.journal.push(entry);
    Ok(())
}
```

- [ ] **Step 4: Wire the module** — in `crates/gents/src/optimization/mod.rs`, add `pub mod job;` in rustfmt order (before `pub mod policy;`) and extend the re-export block:

```rust
pub use job::{
    append, checkpoint, create_job, derive_state, journal_conflict, load_job, rounds_used, Budgets,
    Checkpoint, DecisionSummary, DriftedRef, JobOrigin, JobRecord, JobState, JournalConflict,
    JournalEntry,
};
```

- [ ] **Step 5: Run the tests**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents --lib optimization::job`
Expected: PASS, 5 tests.

- [ ] **Step 6: Commit**

```bash
git add crates/gents/src/optimization
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" \
  commit -m "feat(optimization): length-guarded OptimizationJob journal (#1455)

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

### PR 1 gate

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents-schemas && CARGO_BUILD_JOBS=4 cargo test -p gents-protocol && CARGO_BUILD_JOBS=4 cargo test -p gents-migration`
Run: `CARGO_BUILD_JOBS=4 cargo test -p gents --lib optimization`
Run: `CARGO_BUILD_JOBS=4 cargo check --workspace --all-targets 2>&1 | tee /tmp/pr1-check.log; grep -c "^error" /tmp/pr1-check.log`
Expected: all PASS; the grep reports 0.
Run: `cargo fmt --all --check`

---

## PR 2: The proposer seam, the candidate pack and the structural gate

Branch: `optimization/21-proposer`, base `optimization/20-job`. Deliverable: everything needed to turn a proposed string into a candidate pack and decide, without spending a single validation trial, whether that candidate is even admissible. Every function in this PR is pure except `materialize_pack`, which reads and writes files under the job's directory.

### Task 1: The candidate pack

**Files:**
- Create: `crates/gents/src/optimization/subject.rs`
- Modify: `crates/gents/src/optimization/mod.rs`

**Interfaces:**
- Consumes:
  ```rust
  // crates/gents/src/pack.rs
  pub fn declared_paths(manifest: &PackManifest) -> Vec<String>;
  pub fn digest_declared_assets<'a>(manifest: &PackManifest, asset: impl Fn(&str) -> Result<&'a [u8]>) -> Result<String>;
  // crates/gents/src/pack/loader.rs, re-exported as crate::pack::load_pack_config
  pub fn load_pack_config(
      manifest: &PackManifest,
      options: &PackInstallOptions,
      read_asset: &dyn Fn(&str) -> Result<Vec<u8>>,
      environment: &dyn Fn(&str) -> Option<String>,
  ) -> Result<PackConfig>;
  pub struct PackInstallOptions { pub agent_did: String }
  // crates/gents/src/document_config/pack_config.rs
  pub struct PackConfig { pub agent_principal: AgentPrincipal, pub agent_behaviors: Vec<AgentBehavior>,
      pub contexts: Vec<AgentContext>, pub tools: Vec<Tools>, /* … */ }
  // crates/gents/src/document_config/behavior.rs
  pub struct AgentBehavior { pub behavior_id: String, pub agent_did: String,
      pub context_id: Option<String>, pub inference_profile_id: String, /* … */ }
  // crates/gents/src/document_config/context.rs
  pub struct AgentContext { pub context_id: String, pub agent_did: String,
      pub system_prompt: Option<String>, pub tools_id: Option<String>, /* … */ }
  ```
  `load_pack_config` resolves a `./`-prefixed `AgentContext.system_prompt` into the sidecar's text (`pack/loader.rs:105-107`), which is why `subject.rs` reads the raw `pack_config.json` to find the sidecar path. Its one existing caller, `fn load_pack` in `crates/gents/src/eval/runner/freeze.rs`, is the shape copied below.
- Produces:
  ```rust
  pub struct MaterializedPack {
      pub dir: PathBuf,
      pub digest: String,
      pub config: PackConfig,
      pub manifest: PackManifest,
      /// Every declared asset, keyed by its path relative to `dir`.
      pub files: BTreeMap<String, Vec<u8>>,
      /// The context the subject behavior names.
      pub context_id: String,
      /// The declared asset the context reads its system prompt from, when the
      /// pack stores it as a sidecar rather than inline.
      pub prompt_asset: Option<String>,
  }
  pub fn materialize_pack(dir: &Path, owner: &str, behavior_id: &str) -> Result<MaterializedPack>;
  pub fn baseline_text(pack: &MaterializedPack) -> Result<String>;
  pub fn materialize_candidate(baseline: &MaterializedPack, owner: &str, text: &str, dir: &Path) -> Result<MaterializedPack>;
  // test fixtures in subject.rs's `pub(crate) mod tests`, consumed by the gate's tests
  pub(crate) const FIXTURE_PROMPT: &str = "Watch the mailbox.\n";
  pub(crate) fn write_fixture_pack(root: &Path);         // sidecar prompt
  pub(crate) fn write_inline_fixture_pack(root: &Path);  // inline prompt, no sidecar asset
  ```

- [ ] **Step 1: Write the failing test** — create `crates/gents/src/optimization/subject.rs` holding only this test module:

```rust
#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::json;

    const OWNER: &str = "did:key:subject-owner";
    pub(crate) const FIXTURE_PROMPT: &str = "Watch the mailbox.\n";

    /// The shape of the eval runner's own fixture pack: a manifest, a README,
    /// the canonical config bundle and one behavior sidecar.
    pub(crate) fn write_fixture_pack(root: &Path) {
        std::fs::create_dir_all(root.join("agent_behaviors/monitor")).unwrap();
        std::fs::write(root.join("README.md"), "# monitor fixture\n").unwrap();
        std::fs::write(root.join("agent_behaviors/monitor/system_prompt.md"), FIXTURE_PROMPT).unwrap();
        let manifest = json!({
            "manifest_version": 1,
            "name": "monitor_fixture",
            "version": "1.0.0",
            "description": "Optimization subject: one monitor behavior.",
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
                "host": {"files": {"mode": "ReadOnly"}},
            }],
        });
        std::fs::write(
            root.join("pack_config.json"),
            serde_json::to_vec_pretty(&config).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn a_candidate_is_the_baseline_pack_with_one_file_rewritten() {
        let dirs = tempfile::tempdir().unwrap();
        let baseline_dir = dirs.path().join("baseline");
        write_fixture_pack(&baseline_dir);
        let baseline = materialize_pack(&baseline_dir, OWNER, "monitor").unwrap();

        assert_eq!(baseline.context_id, "monitor-context");
        assert_eq!(
            baseline.prompt_asset.as_deref(),
            Some("agent_behaviors/monitor/system_prompt.md")
        );
        assert_eq!(baseline_text(&baseline).unwrap(), FIXTURE_PROMPT);
        assert!(baseline.digest.starts_with("sha256:"));

        let candidate_dir = dirs.path().join("candidate");
        let candidate =
            materialize_candidate(&baseline, OWNER, "Watch the mailbox, and say why.\n", &candidate_dir)
                .unwrap();

        assert_eq!(
            baseline.files.keys().collect::<Vec<_>>(),
            candidate.files.keys().collect::<Vec<_>>(),
            "a candidate declares exactly the baseline's assets"
        );
        let differing: Vec<&String> = baseline
            .files
            .iter()
            .filter(|(path, bytes)| candidate.files.get(*path) != Some(*bytes))
            .map(|(path, _)| path)
            .collect();
        assert_eq!(differing, vec!["agent_behaviors/monitor/system_prompt.md"]);
        assert_eq!(baseline_text(&candidate).unwrap(), "Watch the mailbox, and say why.\n");
        assert_ne!(candidate.digest, baseline.digest, "one changed byte is a new pack");
        assert!(candidate_dir.join("manifest.json").exists());
    }

    #[test]
    fn the_same_text_materializes_to_the_same_digest() {
        let dirs = tempfile::tempdir().unwrap();
        let baseline_dir = dirs.path().join("baseline");
        write_fixture_pack(&baseline_dir);
        let baseline = materialize_pack(&baseline_dir, OWNER, "monitor").unwrap();

        let one = materialize_candidate(&baseline, OWNER, "New text.\n", &dirs.path().join("one")).unwrap();
        let two = materialize_candidate(&baseline, OWNER, "New text.\n", &dirs.path().join("two")).unwrap();
        assert_eq!(one.digest, two.digest, "the digest is over content, not location");

        let same = materialize_candidate(&baseline, OWNER, FIXTURE_PROMPT, &dirs.path().join("same")).unwrap();
        assert_eq!(
            same.digest, baseline.digest,
            "rewriting the prompt with its own text is the baseline pack"
        );
    }

    #[test]
    fn a_behavior_the_pack_does_not_declare_is_an_error() {
        let dirs = tempfile::tempdir().unwrap();
        let baseline_dir = dirs.path().join("baseline");
        write_fixture_pack(&baseline_dir);
        let error = materialize_pack(&baseline_dir, OWNER, "no-such-behavior").unwrap_err();
        assert!(
            format!("{error:#}").contains("no-such-behavior"),
            "{error:#}"
        );
    }

    /// The same pack with the prompt inline in `pack_config.json` and no
    /// sidecar asset, for the structural gate's inline branch.
    pub(crate) fn write_inline_fixture_pack(root: &Path) {
        write_fixture_pack(root);
        std::fs::remove_file(root.join("agent_behaviors/monitor/system_prompt.md")).unwrap();
        let manifest_path = root.join("manifest.json");
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
        manifest["assets"] = json!(["README.md", "pack_config.json"]);
        std::fs::write(&manifest_path, serde_json::to_vec_pretty(&manifest).unwrap()).unwrap();
        let config_path = root.join("pack_config.json");
        let mut config: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
        config["contexts"][0]["system_prompt"] = json!(FIXTURE_PROMPT);
        std::fs::write(&config_path, serde_json::to_vec_pretty(&config).unwrap()).unwrap();
    }

    #[test]
    fn an_inline_prompt_is_rewritten_in_the_config_and_nowhere_else() {
        let dirs = tempfile::tempdir().unwrap();
        write_inline_fixture_pack(&dirs.path().join("baseline"));
        let baseline = materialize_pack(&dirs.path().join("baseline"), OWNER, "monitor").unwrap();
        assert_eq!(baseline.prompt_asset, None);
        assert_eq!(baseline_text(&baseline).unwrap(), FIXTURE_PROMPT);
        let candidate =
            materialize_candidate(&baseline, OWNER, "New text.\n", &dirs.path().join("c")).unwrap();
        assert_eq!(baseline_text(&candidate).unwrap(), "New text.\n");
        assert_ne!(candidate.digest, baseline.digest);
    }
}
```

- [ ] **Step 2: Run it to see it fail**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents --lib optimization::subject`
Expected: FAIL to compile, `cannot find function materialize_pack in this scope`.

- [ ] **Step 3: Write the implementation** — above the test module:

```rust
//! The baseline subject pack and the candidate packs derived from it.
//!
//! A candidate is the baseline pack with exactly one field changed: the system
//! prompt of the context the subject behavior names. When the pack keeps that
//! prompt in a sidecar asset — the shape every pack in this repository uses —
//! the change is one file's bytes and nothing else, which is what makes the
//! structural gate's "only the target moved" check a file comparison.
//!
//! Both packs are ordinary directory packs, so the runner takes them through
//! `CellSource::Directory` with no special case.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::Value;

use crate::document_config::PackConfig;
use crate::pack::{declared_paths, digest_declared_assets, PackInstallOptions, PackManifest};

/// One arm's pack: where it is, what it digests to, what it declares, and the
/// bytes that digest covers.
#[derive(Clone, Debug)]
pub struct MaterializedPack {
    pub dir: PathBuf,
    pub digest: String,
    pub config: PackConfig,
    pub manifest: PackManifest,
    pub files: BTreeMap<String, Vec<u8>>,
    pub context_id: String,
    pub prompt_asset: Option<String>,
}

/// Read the pack at `dir` as the subject of `behavior_id`.
pub fn materialize_pack(dir: &Path, owner: &str, behavior_id: &str) -> Result<MaterializedPack> {
    let manifest: PackManifest = serde_json::from_slice(
        &std::fs::read(dir.join("manifest.json"))
            .with_context(|| format!("reading {}/manifest.json", dir.display()))?,
    )
    .context("parsing pack manifest")?;

    let mut files = BTreeMap::new();
    for path in declared_paths(&manifest) {
        let bytes = std::fs::read(dir.join(&path))
            .with_context(|| format!("pack {} has no asset {path:?}", dir.display()))?;
        files.insert(path, bytes);
    }
    let asset = |path: &str| -> Result<&[u8]> {
        files
            .get(path)
            .map(Vec::as_slice)
            .with_context(|| format!("pack has no asset {path:?}"))
    };
    let digest = digest_declared_assets(&manifest, asset)?;
    let config = crate::pack::load_pack_config(
        &manifest,
        &PackInstallOptions {
            agent_did: owner.to_owned(),
        },
        &|path| asset(path).map(<[u8]>::to_vec),
        &|name| std::env::var(name).ok(),
    )?;

    let context_id = config
        .agent_behaviors
        .iter()
        .find(|behavior| behavior.behavior_id == behavior_id)
        .with_context(|| format!("pack declares no behavior {behavior_id:?}"))?
        .context_id
        .clone()
        .with_context(|| format!("behavior {behavior_id:?} names no context to optimize"))?;

    let prompt_asset = sidecar_prompt_asset(&manifest, &files, &context_id)?;

    Ok(MaterializedPack {
        dir: dir.to_path_buf(),
        digest,
        config,
        manifest,
        files,
        context_id,
        prompt_asset,
    })
}

/// The declared asset `context_id`'s `system_prompt` points at, when it points
/// at one. The raw `pack_config.json` is read rather than the loaded
/// [`PackConfig`], because the loader resolves a sidecar reference into the
/// text it holds and the reference itself is what has to be rewritten.
fn sidecar_prompt_asset(
    manifest: &PackManifest,
    files: &BTreeMap<String, Vec<u8>>,
    context_id: &str,
) -> Result<Option<String>> {
    let config_path = "pack_config.json";
    let Some(bytes) = files.get(config_path) else {
        return Ok(None);
    };
    let raw: Value = serde_json::from_slice(bytes).context("parsing pack_config.json")?;
    let Some(reference) = raw["contexts"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|context| context["context_id"].as_str() == Some(context_id))
        .and_then(|context| context["system_prompt"].as_str())
    else {
        return Ok(None);
    };
    let normalized = reference.trim_start_matches("./");
    Ok(declared_paths(manifest)
        .into_iter()
        .find(|path| path == normalized))
}

/// The subject behavior's current system prompt.
pub fn baseline_text(pack: &MaterializedPack) -> Result<String> {
    if let Some(path) = &pack.prompt_asset {
        let bytes = pack
            .files
            .get(path)
            .with_context(|| format!("pack has no asset {path:?}"))?;
        return String::from_utf8(bytes.clone())
            .with_context(|| format!("asset {path:?} is not UTF-8"));
    }
    Ok(pack
        .config
        .contexts
        .iter()
        .find(|context| context.context_id == pack.context_id)
        .and_then(|context| context.system_prompt.clone())
        .unwrap_or_default())
}

/// Write the baseline's declared assets into `dir` with the subject behavior's
/// system prompt replaced by `text`, and read the result back as a pack.
///
/// `dir` is created; an existing one is refused rather than written over, so a
/// round can never evaluate a directory another round left behind.
pub fn materialize_candidate(
    baseline: &MaterializedPack,
    owner: &str,
    text: &str,
    dir: &Path,
) -> Result<MaterializedPack> {
    anyhow::ensure!(
        !dir.exists(),
        "candidate directory {} already exists",
        dir.display()
    );
    let mut files = baseline.files.clone();
    match &baseline.prompt_asset {
        Some(path) => {
            files.insert(path.clone(), text.as_bytes().to_vec());
        }
        None => {
            let config_path = "pack_config.json";
            let bytes = files
                .get(config_path)
                .with_context(|| format!("pack has no asset {config_path:?}"))?;
            let mut raw: Value = serde_json::from_slice(bytes).context("parsing pack_config.json")?;
            let context = raw["contexts"]
                .as_array_mut()
                .into_iter()
                .flatten()
                .find(|context| context["context_id"].as_str() == Some(baseline.context_id.as_str()))
                .with_context(|| format!("pack declares no context {:?}", baseline.context_id))?;
            context["system_prompt"] = Value::String(text.to_owned());
            files.insert(config_path.to_owned(), serde_json::to_vec_pretty(&raw)?);
        }
    }
    for (path, bytes) in &files {
        let destination = dir.join(path);
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        std::fs::write(&destination, bytes)
            .with_context(|| format!("writing {}", destination.display()))?;
    }
    let behavior_id = baseline
        .config
        .agent_behaviors
        .iter()
        .find(|behavior| behavior.context_id.as_deref() == Some(baseline.context_id.as_str()))
        .map(|behavior| behavior.behavior_id.clone())
        .with_context(|| format!("no behavior names context {:?}", baseline.context_id))?;
    materialize_pack(dir, owner, &behavior_id)
}
```

- [ ] **Step 4: Wire the module** — in `crates/gents/src/optimization/mod.rs` add `pub mod subject;` in rustfmt order and:

```rust
pub use subject::{baseline_text, materialize_candidate, materialize_pack, MaterializedPack};
```

- [ ] **Step 5: Run the tests**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents --lib optimization::subject`
Expected: PASS, 4 tests.

- [ ] **Step 6: Commit**

```bash
git add crates/gents/src/optimization
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" \
  commit -m "feat(optimization): candidate packs are the baseline with one field changed (#1455)

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

### Task 2: The `Proposer` trait and the scripted proposer

**Files:**
- Create: `crates/gents/src/optimization/proposer.rs`
- Modify: `crates/gents/src/optimization/mod.rs`

**Interfaces:**
- Consumes: nothing from earlier tasks. This file deliberately imports no `ConfigAccess`, no `ConfigApplyTxn`, no `Path` and no eval type: a proposer receives data and returns a value.
- Produces:
  ```rust
  pub struct CheckFeedback { pub check: String, pub score_bp: Option<u32>, pub feedback: Option<String> }
  pub struct Rejection { pub round: u32, pub text: String, pub rationale: String, pub reason: String }
  pub struct ProposalInput {
      pub round: u32,
      pub current_text: String,
      pub feedback: Vec<CheckFeedback>,
      pub rejections: Vec<Rejection>,
      pub max_text_bytes: usize,
  }
  pub struct Proposal { pub text: String, pub rationale: String }
  #[async_trait::async_trait]
  pub trait Proposer: Send + Sync {
      async fn propose(&self, input: ProposalInput) -> Result<Proposal>;
  }
  pub struct ScriptedProposer { /* private */ pub calls: Mutex<Vec<ProposalInput>> }
  impl ScriptedProposer {
      pub fn new(script: Vec<(String, String)>) -> Self;
      pub fn echoing() -> Self;   // always returns the current text unchanged
  }
  ```

- [ ] **Step 1: Write the failing test** — create `crates/gents/src/optimization/proposer.rs` holding only this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn input(round: u32, current: &str) -> ProposalInput {
        ProposalInput {
            round,
            current_text: current.into(),
            feedback: vec![CheckFeedback {
                check: "captured_rows_count".into(),
                score_bp: Some(0),
                feedback: Some("more rows next time".into()),
            }],
            rejections: Vec::new(),
            max_text_bytes: 32 * 1024,
        }
    }

    #[tokio::test]
    async fn the_scripted_proposer_answers_by_round_and_records_what_it_was_given() {
        let proposer = ScriptedProposer::new(vec![
            ("first candidate".into(), "widen the scope".into()),
            ("second candidate".into(), "name the collection".into()),
        ]);
        let first = proposer.propose(input(1, "baseline")).await.unwrap();
        assert_eq!(
            (first.text.as_str(), first.rationale.as_str()),
            ("first candidate", "widen the scope")
        );
        let second = proposer.propose(input(2, "first candidate")).await.unwrap();
        assert_eq!(second.text, "second candidate");

        let exhausted = proposer.propose(input(3, "second candidate")).await.unwrap_err();
        assert!(format!("{exhausted:#}").contains("round 3"), "{exhausted:#}");

        let calls = proposer.calls.lock().unwrap();
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[1].current_text, "first candidate");
        assert_eq!(calls[0].feedback[0].feedback.as_deref(), Some("more rows next time"));
    }

    #[tokio::test]
    async fn the_echoing_proposer_returns_the_current_text_unchanged() {
        let proposer = ScriptedProposer::echoing();
        let proposal = proposer.propose(input(1, "baseline")).await.unwrap();
        assert_eq!(proposal.text, "baseline");
    }

    /// Constraint 13: the proposer sees texts, scores and check names. It never
    /// sees a case id, a stage prompt, a check's params, a tier, a split label
    /// or a raw verdict payload.
    #[test]
    fn the_proposal_input_carries_nothing_protected() {
        let json = serde_json::to_value(input(1, "baseline")).unwrap();
        let rendered = serde_json::to_string(&json).unwrap();
        for forbidden in [
            "case_id", "stage_id", "prompt", "params", "tier", "split", "raw", "trial",
        ] {
            assert!(
                !rendered.contains(forbidden),
                "ProposalInput must not carry {forbidden}: {rendered}"
            );
        }
        let keys: Vec<&String> = json.as_object().unwrap().keys().collect();
        assert_eq!(
            keys,
            vec!["current_text", "feedback", "max_text_bytes", "rejections", "round"],
            "a new field on ProposalInput is a decision about what reaches a model"
        );
    }
}
```

- [ ] **Step 2: Run it to see it fail**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents --lib optimization::proposer`
Expected: FAIL to compile, `cannot find type ProposalInput in this scope`.

- [ ] **Step 3: Write the implementation** — above the test module:

```rust
//! The proposer seam: what a proposer is told, and what it returns.
//!
//! A proposer receives data and returns a value. It holds no `ConfigAccess`,
//! opens no transaction and names no path, so it cannot read a check's
//! parameters, another case's body or the database it is being optimized
//! against; `target.rs` and `subject.rs` build the patch from the text it
//! returns.
//!
//! [`ProposalInput`] is therefore a boundary type, and its field list is the
//! statement of what an optimizer may learn. Feedback reaches it only from the
//! round's train run, and the eval runner has already refused to record
//! feedback on any other split.

use std::sync::Mutex;

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// One acceptance check's advice from the train run. No case, no stage, no
/// parameters: a check's name, what it scored, and what it had to say.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckFeedback {
    pub check: String,
    pub score_bp: Option<u32>,
    pub feedback: Option<String>,
}

/// A candidate the job already refused, and why. `Inconclusive` and
/// budget-exhausted rounds are never rejections and never appear here.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rejection {
    pub round: u32,
    pub text: String,
    pub rationale: String,
    pub reason: String,
}

/// Everything a proposer is told. Adding a field here is a decision about what
/// reaches a model, and a test pins the list.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProposalInput {
    pub round: u32,
    /// The retained checkpoint's text, which is what the train run evaluated.
    pub current_text: String,
    pub feedback: Vec<CheckFeedback>,
    pub rejections: Vec<Rejection>,
    pub max_text_bytes: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Proposal {
    pub text: String,
    pub rationale: String,
}

#[async_trait::async_trait]
pub trait Proposer: Send + Sync {
    async fn propose(&self, input: ProposalInput) -> Result<Proposal>;
}

/// A proposer that answers from a table instead of thinking, and records every
/// input it was given so a test can assert what did and did not reach it.
pub struct ScriptedProposer {
    script: Vec<(String, String)>,
    echo: bool,
    pub calls: Mutex<Vec<ProposalInput>>,
}

impl ScriptedProposer {
    /// One `(text, rationale)` per round, in round order.
    pub fn new(script: Vec<(String, String)>) -> Self {
        Self {
            script,
            echo: false,
            calls: Mutex::new(Vec::new()),
        }
    }

    /// Always proposes the text it was given, which the structural gate
    /// refuses as a duplicate of the checkpoint.
    pub fn echoing() -> Self {
        Self {
            script: Vec::new(),
            echo: true,
            calls: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait::async_trait]
impl Proposer for ScriptedProposer {
    async fn propose(&self, input: ProposalInput) -> Result<Proposal> {
        let round = input.round;
        let current = input.current_text.clone();
        if let Ok(mut calls) = self.calls.lock() {
            calls.push(input);
        }
        if self.echo {
            return Ok(Proposal {
                text: current,
                rationale: "unchanged".to_owned(),
            });
        }
        let index = (round as usize).checked_sub(1).unwrap_or(0);
        let (text, rationale) = self
            .script
            .get(index)
            .ok_or_else(|| anyhow::anyhow!("the scripted proposer has no answer for round {round}"))?;
        Ok(Proposal {
            text: text.clone(),
            rationale: rationale.clone(),
        })
    }
}
```

- [ ] **Step 4: Wire the module** — in `crates/gents/src/optimization/mod.rs` add `pub mod proposer;` in rustfmt order and:

```rust
pub use proposer::{CheckFeedback, Proposal, ProposalInput, Proposer, Rejection, ScriptedProposer};
```

- [ ] **Step 5: Run the tests**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents --lib optimization::proposer`
Expected: PASS, 3 tests.

- [ ] **Step 6: Commit**

```bash
git add crates/gents/src/optimization
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" \
  commit -m "feat(optimization): the Proposer seam and a scripted proposer (#1455)

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

### Task 3: The structural gate

**Files:**
- Create: `crates/gents/src/optimization/gate.rs`
- Modify: `crates/gents/src/optimization/mod.rs`

Ruling R2 removes the Task target, so the gate has no `unsupported_target` check and takes no `Target`. Finding F6 makes "the patch touches only the allowed field" exact in both pack shapes: for a sidecar prompt the one changed asset must hold exactly the proposed bytes; for an inline prompt both raw `pack_config.json` values must be equal once `contexts[<context_id>].system_prompt` is masked, and the candidate's unmasked value must be exactly the proposed text.

**Interfaces:**
- Consumes:
  ```rust
  // crates/gents/src/optimization/subject.rs, PR 2 Task 1
  pub struct MaterializedPack { pub dir: PathBuf, pub digest: String, pub config: PackConfig,
      pub manifest: PackManifest, pub files: BTreeMap<String, Vec<u8>>,
      pub context_id: String, pub prompt_asset: Option<String> }
  pub fn materialize_pack(dir: &Path, owner: &str, behavior_id: &str) -> Result<MaterializedPack>;
  pub fn materialize_candidate(baseline: &MaterializedPack, owner: &str, text: &str, dir: &Path) -> Result<MaterializedPack>;
  // subject.rs test module, PR 2 Task 1 (pub(crate))
  pub(crate) const FIXTURE_PROMPT: &str;
  pub(crate) fn write_fixture_pack(root: &Path);
  pub(crate) fn write_inline_fixture_pack(root: &Path);
  // crates/gents/src/config_client/desired_state.rs
  impl DesiredStateApplyPlan {
      pub fn from_pack_config(config: &PackConfig) -> Result<Self>;
      pub fn documents(&self) -> &[DesiredStateApplyDocument];
  }
  // crates/gents/src/document_config/references.rs
  impl ConfigReferences {
      pub fn from_documents(agent_did: &str, documents: impl IntoIterator<Item = (Collection, Value)>) -> Result<Self>;
      pub fn validate(&self) -> Result<()>;
  }
  ```
- Produces:
  ```rust
  pub struct StructuralRejection { pub reason: &'static str, pub detail: String }
  impl StructuralRejection { pub fn diagnostics(&self) -> String; }
  pub fn structural_gate(
      baseline: &MaterializedPack,
      candidate: &MaterializedPack,
      text: &str,
      max_text_bytes: usize,
      seen_digests: &[String],
      owner: &str,
  ) -> Result<(), StructuralRejection>;
  ```
  `reason` is one of `empty_text`, `text_too_long`, `unexpected_change`, `text_mismatch`, `invalid_closure`, `duplicate_candidate`.

- [ ] **Step 1: Write the failing test** — create `crates/gents/src/optimization/gate.rs` holding only this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::optimization::subject::tests::{
        write_fixture_pack, write_inline_fixture_pack, FIXTURE_PROMPT,
    };
    use crate::optimization::subject::{materialize_candidate, materialize_pack};

    const OWNER: &str = "did:key:gate-owner";
    const TEXT: &str = "Watch the mailbox, and say why.\n";

    struct Fixture {
        _dirs: tempfile::TempDir,
        root: std::path::PathBuf,
        baseline: MaterializedPack,
    }

    fn fixture(inline: bool) -> Fixture {
        let dirs = tempfile::tempdir().unwrap();
        let root = dirs.path().to_path_buf();
        if inline {
            write_inline_fixture_pack(&root.join("baseline"));
        } else {
            write_fixture_pack(&root.join("baseline"));
        }
        let baseline = materialize_pack(&root.join("baseline"), OWNER, "monitor").unwrap();
        Fixture {
            _dirs: dirs,
            root,
            baseline,
        }
    }

    fn candidate(fixture: &Fixture, text: &str, name: &str) -> MaterializedPack {
        materialize_candidate(&fixture.baseline, OWNER, text, &fixture.root.join(name)).unwrap()
    }

    fn gate(fixture: &Fixture, candidate: &MaterializedPack, text: &str) -> Result<(), StructuralRejection> {
        structural_gate(
            &fixture.baseline,
            candidate,
            text,
            32 * 1024,
            &[fixture.baseline.digest.clone()],
            OWNER,
        )
    }

    #[test]
    fn a_one_field_sidecar_candidate_of_a_new_digest_passes() {
        let fixture = fixture(false);
        let candidate = candidate(&fixture, TEXT, "c1");
        gate(&fixture, &candidate, TEXT).unwrap();
    }

    #[test]
    fn a_one_field_inline_candidate_of_a_new_digest_passes() {
        let fixture = fixture(true);
        assert_eq!(fixture.baseline.prompt_asset, None);
        let candidate = candidate(&fixture, TEXT, "c1");
        gate(&fixture, &candidate, TEXT).unwrap();
    }

    #[test]
    fn a_candidate_that_repeats_a_digest_is_rejected_as_a_duplicate() {
        let fixture = fixture(false);
        let candidate = candidate(&fixture, FIXTURE_PROMPT, "c2");
        let rejection = gate(&fixture, &candidate, FIXTURE_PROMPT).unwrap_err();
        assert_eq!(rejection.reason, "duplicate_candidate");
        assert!(rejection.diagnostics().starts_with("duplicate_candidate: "));
    }

    #[test]
    fn an_empty_or_oversized_text_is_rejected_before_anything_else() {
        let fixture = fixture(false);
        let empty = candidate(&fixture, "", "c3");
        assert_eq!(gate(&fixture, &empty, "").unwrap_err().reason, "empty_text");

        let long = "x".repeat(33);
        let oversized = candidate(&fixture, &long, "c4");
        let rejection = structural_gate(
            &fixture.baseline,
            &oversized,
            &long,
            32,
            &[fixture.baseline.digest.clone()],
            OWNER,
        )
        .unwrap_err();
        assert_eq!(rejection.reason, "text_too_long");
        assert!(rejection.detail.contains("33"), "{}", rejection.detail);
    }

    #[test]
    fn a_sidecar_candidate_that_changed_another_asset_is_rejected() {
        let fixture = fixture(false);
        let mut tampered = candidate(&fixture, TEXT, "c5");
        tampered
            .files
            .insert("README.md".into(), b"# tampered\n".to_vec());
        let rejection = gate(&fixture, &tampered, TEXT).unwrap_err();
        assert_eq!(rejection.reason, "unexpected_change");
        assert!(rejection.detail.contains("README.md"), "{}", rejection.detail);
    }

    /// F6: the one changed asset must hold exactly the proposed bytes.
    #[test]
    fn a_sidecar_whose_bytes_are_not_the_proposed_text_is_rejected() {
        let fixture = fixture(false);
        let candidate = candidate(&fixture, TEXT, "c6");
        let rejection = gate(&fixture, &candidate, "Some other text.\n").unwrap_err();
        assert_eq!(rejection.reason, "text_mismatch");
    }

    /// F6: in an inline pack the whole config may differ only at the target.
    #[test]
    fn an_inline_candidate_that_changed_another_config_field_is_rejected() {
        let fixture = fixture(true);
        let mut tampered = candidate(&fixture, TEXT, "c7");
        let mut raw: serde_json::Value =
            serde_json::from_slice(&tampered.files["pack_config.json"]).unwrap();
        raw["contexts"][0]["display_name"] = serde_json::json!("Renamed");
        tampered
            .files
            .insert("pack_config.json".into(), serde_json::to_vec_pretty(&raw).unwrap());
        let rejection = gate(&fixture, &tampered, TEXT).unwrap_err();
        assert_eq!(rejection.reason, "unexpected_change");
        assert!(
            rejection.detail.contains("besides contexts"),
            "{}",
            rejection.detail
        );

        let untampered = candidate(&fixture, TEXT, "c8");
        let rejection = gate(&fixture, &untampered, "Not what was written.\n").unwrap_err();
        assert_eq!(rejection.reason, "text_mismatch");
    }

    /// The pack loader reads an inline value beginning with `./` as a sidecar
    /// path, so such a text would silently become a different field's meaning.
    #[test]
    fn an_inline_text_that_reads_as_a_sidecar_path_is_rejected() {
        let fixture = fixture(true);
        let rejection = gate(&fixture, &fixture.baseline.clone(), "./README.md").unwrap_err();
        assert_eq!(rejection.reason, "unexpected_change");
        assert!(rejection.detail.contains("./"), "{}", rejection.detail);
    }

    #[test]
    fn a_candidate_whose_references_no_longer_resolve_is_rejected() {
        let fixture = fixture(false);
        let mut candidate = candidate(&fixture, TEXT, "c9");
        // The context points at a tools document the pack does not declare.
        candidate.config.contexts[0].tools_id = Some("no-such-tools".into());
        let rejection = gate(&fixture, &candidate, TEXT).unwrap_err();
        assert_eq!(rejection.reason, "invalid_closure");
        assert!(rejection.detail.contains("no-such-tools"), "{}", rejection.detail);
    }
}
```

- [ ] **Step 2: Run it to see it fail**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents --lib optimization::gate`
Expected: FAIL to compile, `cannot find function structural_gate in this scope`.

- [ ] **Step 3: Write the implementation** — above the test module in `gate.rs`:

```rust
//! The structural gate: everything that can be decided about a candidate
//! before a single validation trial is spent.
//!
//! The gate is pure. It compares two materialized packs byte for byte, so
//! "the patch touches only the allowed field" is a statement about files
//! rather than about intent, and it validates the candidate's own reference
//! closure the way `validate_desired_state_plan` validates a live one — the
//! live form of that check belongs to promotion, where a transaction exists.

use std::collections::BTreeSet;

use serde_json::Value;

use crate::config_client::DesiredStateApplyPlan;
use crate::optimization::subject::MaterializedPack;

const CONFIG_ASSET: &str = "pack_config.json";

/// Why a candidate never reached a validation run. `reason` is a closed
/// vocabulary for the journal; `detail` is diagnostics for an operator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StructuralRejection {
    pub reason: &'static str,
    pub detail: String,
}

impl StructuralRejection {
    pub fn diagnostics(&self) -> String {
        format!("{}: {}", self.reason, self.detail)
    }
}

impl std::fmt::Display for StructuralRejection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.diagnostics())
    }
}

impl std::error::Error for StructuralRejection {}

fn reject(reason: &'static str, detail: impl Into<String>) -> StructuralRejection {
    StructuralRejection {
        reason,
        detail: detail.into(),
    }
}

fn raw_config(pack: &MaterializedPack) -> Result<Value, StructuralRejection> {
    let bytes = pack
        .files
        .get(CONFIG_ASSET)
        .ok_or_else(|| reject("unexpected_change", format!("the pack has no {CONFIG_ASSET}")))?;
    serde_json::from_slice(bytes)
        .map_err(|error| reject("unexpected_change", format!("{CONFIG_ASSET} does not parse: {error}")))
}

/// The raw `system_prompt` of `context_id`, and the config with it masked.
fn split_prompt(mut raw: Value, context_id: &str) -> Result<(Value, Value), StructuralRejection> {
    let context = raw["contexts"]
        .as_array_mut()
        .into_iter()
        .flatten()
        .find(|context| context["context_id"].as_str() == Some(context_id))
        .ok_or_else(|| reject("unexpected_change", format!("no context {context_id:?}")))?;
    let prompt = context["system_prompt"].take();
    Ok((prompt, raw))
}

/// Decide whether `candidate` may be evaluated at all.
///
/// The checks run in the order a rejection is cheapest to explain, and the
/// first failure decides. `seen_digests` holds the checkpoint's digest and
/// every earlier candidate's, so a proposer that repeats itself spends nothing.
pub fn structural_gate(
    baseline: &MaterializedPack,
    candidate: &MaterializedPack,
    text: &str,
    max_text_bytes: usize,
    seen_digests: &[String],
    owner: &str,
) -> Result<(), StructuralRejection> {
    if text.trim().is_empty() {
        return Err(reject("empty_text", "a candidate prompt must say something"));
    }
    if text.len() > max_text_bytes {
        return Err(reject(
            "text_too_long",
            format!("{} bytes exceeds the {max_text_bytes} byte cap", text.len()),
        ));
    }
    if baseline.prompt_asset.is_none() && text.starts_with("./") {
        return Err(reject(
            "unexpected_change",
            "an inline prompt beginning with ./ is read by the pack loader as a sidecar path",
        ));
    }

    let baseline_paths: BTreeSet<&String> = baseline.files.keys().collect();
    let candidate_paths: BTreeSet<&String> = candidate.files.keys().collect();
    if baseline_paths != candidate_paths {
        let added: Vec<&&String> = candidate_paths.difference(&baseline_paths).collect();
        let removed: Vec<&&String> = baseline_paths.difference(&candidate_paths).collect();
        return Err(reject(
            "unexpected_change",
            format!("declared assets changed: added {added:?}, removed {removed:?}"),
        ));
    }
    let changed: Vec<&String> = baseline
        .files
        .iter()
        .filter(|(path, bytes)| candidate.files.get(*path) != Some(*bytes))
        .map(|(path, _)| path)
        .collect();
    let allowed = baseline
        .prompt_asset
        .clone()
        .unwrap_or_else(|| CONFIG_ASSET.to_owned());
    if changed.len() != 1 || changed[0] != &allowed {
        return Err(reject(
            "unexpected_change",
            format!("expected only {allowed:?} to change, but {changed:?} did"),
        ));
    }
    if candidate.context_id != baseline.context_id {
        return Err(reject(
            "unexpected_change",
            format!(
                "the subject behavior's context moved from {:?} to {:?}",
                baseline.context_id, candidate.context_id
            ),
        ));
    }

    match &baseline.prompt_asset {
        // Sidecar: the one changed asset holds exactly the proposed bytes.
        Some(asset) => {
            if candidate.files.get(asset).map(Vec::as_slice) != Some(text.as_bytes()) {
                return Err(reject(
                    "text_mismatch",
                    format!("{asset:?} does not hold the proposed text"),
                ));
            }
        }
        // Inline: the configs agree once the target is masked, and the
        // candidate's target is exactly the proposed text.
        None => {
            let (_, baseline_rest) = split_prompt(raw_config(baseline)?, &baseline.context_id)?;
            let (prompt, candidate_rest) =
                split_prompt(raw_config(candidate)?, &candidate.context_id)?;
            if baseline_rest != candidate_rest {
                return Err(reject(
                    "unexpected_change",
                    format!(
                        "{CONFIG_ASSET} changed besides contexts[{:?}].system_prompt",
                        baseline.context_id
                    ),
                ));
            }
            if prompt.as_str() != Some(text) {
                return Err(reject(
                    "text_mismatch",
                    "the inline system_prompt is not the proposed text",
                ));
            }
        }
    }

    // The same closure rule `validate_desired_state_plan` applies to a live
    // configuration, applied to the pack's own documents.
    let plan = DesiredStateApplyPlan::from_pack_config(&candidate.config)
        .map_err(|error| reject("invalid_closure", format!("{error:#}")))?;
    crate::ConfigReferences::from_documents(
        owner,
        plan.documents()
            .iter()
            .map(|document| (document.collection, document.add.clone())),
    )
    .and_then(|references| references.validate())
    .map_err(|error| reject("invalid_closure", format!("{error:#}")))?;

    if seen_digests.iter().any(|digest| digest == &candidate.digest) {
        return Err(reject(
            "duplicate_candidate",
            format!(
                "candidate digests to {}, which the job has already evaluated",
                candidate.digest
            ),
        ));
    }
    Ok(())
}
```

- [ ] **Step 4: Wire the module** — in `crates/gents/src/optimization/mod.rs` add `pub mod gate;` in rustfmt order and:

```rust
pub use gate::{structural_gate, StructuralRejection};
```

- [ ] **Step 5: Run the tests**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents --lib optimization::`
Expected: PASS, including the 9 gate tests, the 4 subject tests, the 3 proposer tests, the 5 target tests, the 5 job tests and M6a's 11 policy tests.

- [ ] **Step 6: Commit**

```bash
git add crates/gents/src/optimization
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" \
  commit -m "feat(optimization): the structural gate, before any validation spend (#1455)

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

### PR 2 gate

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents --lib optimization`
Run: `CARGO_BUILD_JOBS=4 cargo check --workspace --all-targets 2>&1 | tee /tmp/pr2-check.log; grep -c "^error" /tmp/pr2-check.log`
Expected: PASS; the grep reports 0.
Run: `cargo fmt --all --check`

---

## PR 3: The driver

Branch: `optimization/22-driver`, base `optimization/21-proposer`. Deliverable: the verdict projection in a new unfrozen `eval::report` module (ruling R3), `run_job` (the loop spec section 6 draws), `show` (ruling R1), and the scripted matrix covering every end-to-end case spec section 9 lists.

### Task 1: `eval::report` — verdicts to paired evidence

**Files:**
- Create: `crates/gents/src/eval/report/mod.rs`, `crates/gents/src/eval/report/evidence.rs`
- Modify: `crates/gents/src/eval/mod.rs` (add `pub mod report;` in rustfmt order, between `pub mod outcome;` and `pub mod runner;`)

Ruling R3: nothing in `gents::eval` turns `VerdictRecord` rows into `TrialScore` values — `scoring::case_trial_score` takes `VerdictView`, which carries `stage_index` while `VerdictRecord` carries `stage_id`, and the latest-attempt-per-slot rule the umbrella fixes for M4 (section 7) has no implementation. That projection is an eval concern, so it lives in a new, unfrozen `eval::report` module that optimization consumes now and M4's report will consume later. Nothing new goes into the frozen `eval::scoring`, and `eval::report` imports nothing from `optimization`.

Finding F1: a decision that reads several runs must *add* their pairs, not merge their slots. Two validation attempts share every `(case_id, trial_index)` key, and `pair_trials` reads a repeated key in one cell as `Unknown`, so feeding both runs' trials to one `pair_trials` call would turn every re-run into worst-case imputation. Slots are therefore keyed by `(run_id, case_id, trial_index)`; each run is paired on its own; and `concat_paired` sums the per-run results.

**Interfaces:**
- Consumes:
  ```rust
  // crates/gents/src/eval/documents.rs (frozen)
  pub struct RunRecord { pub run_id: String, pub owner: String, pub evaluator_did: String,
      pub origin: RunOrigin, pub created_at: String, pub invalidated: Option<Invalidation> }
  pub struct TrialRecord { pub identity: TrialIdentity, pub created_at: String, pub completion: Option<TrialCompletion> }
  pub struct TrialIdentity { pub trial_id: String, pub run_id: String, pub cell_id: String,
      pub case_id: String, pub trial_index: u32, pub attempt: u32, pub trial_agent_did: String,
      pub session_id: String, pub seed: i64, pub home_hint: Option<String> }
  pub struct TrialCompletion { pub ended_at: String, pub stages: Vec<StageCompletion>,
      pub usage: TrialUsage, pub anchor: Anchor }
  pub struct TrialUsage { pub input_tokens: Option<u64>, pub output_tokens: Option<u64> }
  pub type VerdictRecord = VerdictDraft; // verdict_id, run_id, trial_id, stage_id, check, check_version,
      // tier, kind, provider_reason, score_bp: Option<u32>, weight: u32, raw, feedback, regrade_of
  pub async fn load_run(access: &ConfigAccess, owner: &str, run_id: &str) -> Result<Option<RunRecord>>;
  pub async fn load_trials(access: &ConfigAccess, owner: &str, run_id: &str) -> Result<Vec<TrialRecord>>;
  pub async fn load_verdicts(access: &ConfigAccess, owner: &str, run_id: &str) -> Result<Vec<VerdictRecord>>;
  // crates/gents/src/eval/scoring.rs (frozen)
  pub struct VerdictView { pub verdict_id: String, pub stage_index: usize, pub check: String,
      pub tier: EvalTier, pub kind: OutcomeKind, pub provider_reason: Option<ProviderReason>,
      pub score_bp: Option<u32>, pub weight: u32, pub regrade_of: Option<String> }
  pub fn latest_verdicts(verdicts: Vec<VerdictView>) -> Vec<VerdictView>;
  pub fn case_trial_score(reducer: EvalReducer, verdicts: &[VerdictView]) -> CaseTrialScore;
  pub struct TrialScore { pub case_id: String, pub trial_index: u32, pub score: CaseTrialScore }
  pub struct PairedEvidence { pub pairs: Vec<Pair>, pub keys: usize, pub dropped_baseline: usize,
      pub dropped_candidate: usize } // derives Default
  pub fn pair_trials(baseline: &[TrialScore], candidate: &[TrialScore]) -> PairedEvidence;
  ```
- Produces:
  ```rust
  // crates/gents/src/eval/report/evidence.rs, re-exported from gents::eval::report
  pub struct RunRows { pub run_id: String, pub case_ids: Vec<String>, pub invalidated: bool,
      pub trials: Vec<TrialRecord>, pub verdicts: Vec<VerdictRecord> }
  pub async fn load_run_rows(access: &ConfigAccess, owner: &str, run_id: &str) -> Result<RunRows>;
  pub fn latest_attempts<'a>(trials: &'a [TrialRecord], cell_id: &str) -> Vec<&'a TrialRecord>;
  pub fn cell_trial_scores(definition: &EvalDefinition, rows: &RunRows, cell_id: &str) -> Vec<TrialScore>;
  pub fn paired_evidence(definition: &EvalDefinition, rows: &RunRows, baseline_cell: &str,
      candidate_cell: &str) -> PairedEvidence;
  pub fn concat_paired(parts: &[PairedEvidence]) -> PairedEvidence;
  pub struct CellUsage { pub tokens: u64, pub trials: u64, pub missing: u64 }
  pub fn cell_usage(rows: &RunRows, cell_id: &str) -> CellUsage;
  // test fixtures, pub(crate) in evidence.rs's `pub(crate) mod tests`
  pub(crate) fn one_case_definition() -> EvalDefinition;
  pub(crate) fn run_rows(run_id: &str, slots: &[Slot]) -> RunRows;
  pub(crate) struct Slot { pub cell: &'static str, pub trial_index: u32, pub attempt: u32,
      pub score_bp: Option<u32>, pub tokens: Option<u64>, pub feedback: Option<&'static str> }
  ```

- [ ] **Step 1: Write the failing test** — create `crates/gents/src/eval/report/evidence.rs` holding only this test module:

```rust
#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::document_config::EvalTier;
    use crate::eval::{Anchor, OutcomeKind, StageCompletion, TrialCompletion, TrialIdentity};
    use serde_json::json;

    pub(crate) fn one_case_definition() -> EvalDefinition {
        serde_json::from_value(json!({
            "definition_id": "monitor-findings",
            "agent_did": "did:key:o",
            "comparability_version": 1,
            "subject": {"kind": "behavior", "inference_slots": ["primary"]},
            "cases": [{
                "case_id": "disk-warning",
                "split": "validation",
                "stages": [{
                    "stage_id": "check",
                    "prompt": "Run the monitor.",
                    "deadline_secs": 600,
                    "checks": [{"check": "captured_rows_count", "params": {"name": "findings", "min": 1}, "tier": "acceptance"}],
                }],
            }],
        }))
        .unwrap()
    }

    /// One trial slot of case `disk-warning`. `score_bp: None` leaves the
    /// attempt unfinished (null completion, no verdict).
    pub(crate) struct Slot {
        pub cell: &'static str,
        pub trial_index: u32,
        pub attempt: u32,
        pub score_bp: Option<u32>,
        pub tokens: Option<u64>,
        pub feedback: Option<&'static str>,
    }

    pub(crate) fn slot(cell: &'static str, trial_index: u32, score_bp: u32) -> Slot {
        Slot {
            cell,
            trial_index,
            attempt: 1,
            score_bp: Some(score_bp),
            tokens: Some(10),
            feedback: None,
        }
    }

    pub(crate) fn run_rows(run_id: &str, slots: &[Slot]) -> RunRows {
        let mut rows = RunRows {
            run_id: run_id.into(),
            case_ids: vec!["disk-warning".into()],
            invalidated: false,
            trials: Vec::new(),
            verdicts: Vec::new(),
        };
        for slot in slots {
            let trial_id = format!("{run_id}-{}-{}-{}", slot.cell, slot.trial_index, slot.attempt);
            rows.trials.push(TrialRecord {
                identity: TrialIdentity {
                    trial_id: trial_id.clone(),
                    run_id: run_id.into(),
                    cell_id: slot.cell.into(),
                    case_id: "disk-warning".into(),
                    trial_index: slot.trial_index,
                    attempt: slot.attempt,
                    trial_agent_did: "did:key:trial".into(),
                    session_id: "session".into(),
                    seed: 1_000 + slot.trial_index as i64,
                    home_hint: None,
                },
                created_at: "2026-09-22T00:00:00Z".into(),
                completion: slot.score_bp.map(|_| TrialCompletion {
                    ended_at: "2026-09-22T00:01:00Z".into(),
                    stages: vec![StageCompletion {
                        stage_id: "check".into(),
                        request_id: None,
                        terminal_state: None,
                        failure_kind: None,
                        provider_reason: None,
                    }],
                    usage: TrialUsage {
                        input_tokens: slot.tokens,
                        output_tokens: slot.tokens,
                    },
                    anchor: Anchor {
                        terminal_states: Vec::new(),
                        requests: 1,
                        inference_calls: 1,
                    },
                }),
            });
            if let Some(score_bp) = slot.score_bp {
                rows.verdicts.push(VerdictRecord {
                    verdict_id: format!("{trial_id}-v"),
                    run_id: run_id.into(),
                    trial_id,
                    stage_id: "check".into(),
                    check: "captured_rows_count".into(),
                    check_version: "1".into(),
                    tier: EvalTier::Acceptance,
                    kind: if score_bp > 0 {
                        OutcomeKind::Passed
                    } else {
                        OutcomeKind::ModelAcceptance
                    },
                    provider_reason: None,
                    score_bp: Some(score_bp),
                    weight: 1,
                    raw: json!({"reason_code": "in_range"}),
                    feedback: slot.feedback.map(str::to_owned),
                    regrade_of: None,
                });
            }
        }
        rows
    }

    #[test]
    fn a_slot_is_read_at_its_latest_completed_attempt() {
        let mut later = slot("base", 0, 10_000);
        later.attempt = 2;
        let rows = run_rows("run", &[slot("base", 0, 0), later]);
        let scores = cell_trial_scores(&one_case_definition(), &rows, "base");
        assert_eq!(scores.len(), 1, "one slot, one score: {scores:?}");
        assert_eq!(scores[0].score, CaseTrialScore::Scored(10_000));
    }

    #[test]
    fn an_unfinished_attempt_contributes_nothing_and_falls_back_to_a_finished_one() {
        let open = Slot {
            attempt: 2,
            score_bp: None,
            ..slot("base", 0, 0)
        };
        let rows = run_rows("run", &[slot("base", 0, 10_000), open]);
        let scores = cell_trial_scores(&one_case_definition(), &rows, "base");
        assert_eq!(scores, vec![TrialScore {
            case_id: "disk-warning".into(),
            trial_index: 0,
            score: CaseTrialScore::Scored(10_000),
        }]);
        let only_open = run_rows("run", &[Slot { score_bp: None, ..slot("base", 0, 0) }]);
        assert!(cell_trial_scores(&one_case_definition(), &only_open, "base").is_empty());
    }

    /// F1: slots are keyed by run as well, so the same `(case, trial_index)` in
    /// two runs is two slots, never one ambiguous one.
    #[test]
    fn the_same_slot_in_two_runs_is_two_slots() {
        let mut first = run_rows("run-a", &[slot("base", 0, 10_000)]);
        let second = run_rows("run-b", &[slot("base", 0, 0)]);
        first.trials.extend(second.trials);
        assert_eq!(latest_attempts(&first.trials, "base").len(), 2);
    }

    #[test]
    fn a_run_is_paired_on_its_shared_seed() {
        let rows = run_rows(
            "run",
            &[
                slot("base", 0, 0),
                slot("base", 1, 0),
                slot("cand", 0, 10_000),
                slot("cand", 1, 10_000),
            ],
        );
        let paired = paired_evidence(&one_case_definition(), &rows, "base", "cand");
        assert_eq!(paired.keys, 2);
        assert_eq!(paired.pairs.len(), 2);
        assert!(paired
            .pairs
            .iter()
            .all(|pair| (pair.baseline_bp, pair.candidate_bp) == (0, 10_000)));
    }

    /// F1: a re-run adds pairs; it never replaces them and never makes a
    /// shared key ambiguous.
    #[test]
    fn concatenating_two_runs_adds_their_pairs() {
        let definition = one_case_definition();
        let slots = || {
            [
                slot("base", 0, 0),
                slot("base", 1, 0),
                slot("cand", 0, 10_000),
                slot("cand", 1, 10_000),
            ]
        };
        let one = paired_evidence(&definition, &run_rows("run-a", &slots()), "base", "cand");
        let two = paired_evidence(&definition, &run_rows("run-b", &slots()), "base", "cand");
        let both = concat_paired(&[one.clone(), two]);
        assert_eq!(both.pairs.len(), 4);
        assert_eq!(both.keys, 4);
        assert_eq!((both.dropped_baseline, both.dropped_candidate), (0, 0));
        assert!(both
            .pairs
            .iter()
            .all(|pair| pair.candidate_bp == 10_000), "no pair was imputed: {both:?}");
        assert_eq!(concat_paired(&[one.clone()]), one);
    }

    #[test]
    fn usage_counts_trials_and_the_ones_that_reported_nothing() {
        let rows = run_rows(
            "run",
            &[
                slot("base", 0, 10_000),
                Slot {
                    tokens: None,
                    ..slot("base", 1, 10_000)
                },
            ],
        );
        assert_eq!(
            cell_usage(&rows, "base"),
            CellUsage {
                tokens: 20,
                trials: 2,
                missing: 1,
            },
            "input and output are summed; a trial with neither is missing"
        );
    }
}
```

- [ ] **Step 2: Run it to see it fail**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents --lib eval::report`
Expected: FAIL to compile, `unresolved module report` until the module lines exist, then `cannot find function cell_trial_scores`.

- [ ] **Step 3: Write the module root** — create `crates/gents/src/eval/report/mod.rs`:

```rust
//! Pure projections from eval documents to comparable numbers.
//!
//! Unfrozen by design (ruling R3): spec 4a's versioned report grows here.
//! `gents::optimization` consumes it today and M4's CLI will consume it too,
//! so neither re-derives how a run's rows become paired evidence. Nothing in
//! this module imports from `optimization`.

pub mod evidence;

pub use evidence::{
    cell_trial_scores, cell_usage, concat_paired, latest_attempts, load_run_rows,
    paired_evidence, CellUsage, RunRows,
};
```

- [ ] **Step 4: Write the implementation** — above the test module in `evidence.rs`:

```rust
//! One run's rows, projected into paired evidence.
//!
//! Three judgements live here and nowhere else. A slot is
//! `(run_id, cell_id, case_id, trial_index)` and is read at its latest
//! *completed* attempt, because a resumed run writes a new row per attempt.
//! A verdict's `stage_id` becomes the `stage_index` its case declares, which
//! is what `case_trial_score` reduces over. And several runs are paired one at
//! a time and then concatenated, so a re-run adds pairs.

use std::collections::BTreeMap;

use anyhow::{Context, Result};

use crate::config_client::ConfigAccess;
use crate::document_config::EvalDefinition;
use crate::eval::{
    case_trial_score, latest_verdicts, load_run, load_trials, load_verdicts, pair_trials,
    CaseTrialScore, PairedEvidence, TrialRecord, TrialScore, TrialUsage, VerdictRecord,
    VerdictView,
};

/// Everything one run wrote, read once.
#[derive(Clone, Debug, PartialEq)]
pub struct RunRows {
    pub run_id: String,
    /// The run's frozen case list: the cases a decision over it expects.
    pub case_ids: Vec<String>,
    pub invalidated: bool,
    pub trials: Vec<TrialRecord>,
    pub verdicts: Vec<VerdictRecord>,
}

pub async fn load_run_rows(access: &ConfigAccess, owner: &str, run_id: &str) -> Result<RunRows> {
    let record = load_run(access, owner, run_id)
        .await?
        .with_context(|| format!("no eval run {run_id:?} for {owner}"))?;
    Ok(RunRows {
        run_id: run_id.to_owned(),
        case_ids: record.origin.case_ids.clone(),
        invalidated: record.invalidated.is_some(),
        trials: load_trials(access, owner, run_id).await?,
        verdicts: load_verdicts(access, owner, run_id).await?,
    })
}

/// One trial per `(run_id, case_id, trial_index)` slot of `cell_id`: the
/// completed attempt with the highest number. A slot whose every attempt is
/// still open contributes nothing, and pairing counts it as a dropped key.
pub fn latest_attempts<'a>(trials: &'a [TrialRecord], cell_id: &str) -> Vec<&'a TrialRecord> {
    let mut latest: BTreeMap<(&str, &str, u32), &TrialRecord> = BTreeMap::new();
    for record in trials
        .iter()
        .filter(|record| record.identity.cell_id == cell_id && record.completion.is_some())
    {
        let key = (
            record.identity.run_id.as_str(),
            record.identity.case_id.as_str(),
            record.identity.trial_index,
        );
        latest
            .entry(key)
            .and_modify(|held| {
                if record.identity.attempt > held.identity.attempt {
                    *held = record;
                }
            })
            .or_insert(record);
    }
    latest.into_values().collect()
}

/// One [`TrialScore`] per completed slot of `cell_id` in one run.
pub fn cell_trial_scores(
    definition: &EvalDefinition,
    rows: &RunRows,
    cell_id: &str,
) -> Vec<TrialScore> {
    let mut by_trial: BTreeMap<&str, Vec<&VerdictRecord>> = BTreeMap::new();
    for verdict in &rows.verdicts {
        by_trial
            .entry(verdict.trial_id.as_str())
            .or_default()
            .push(verdict);
    }
    let mut scores = Vec::new();
    for record in latest_attempts(&rows.trials, cell_id) {
        let case_id = record.identity.case_id.as_str();
        let Some(case) = definition.cases.iter().find(|case| case.case_id == case_id) else {
            continue;
        };
        let indices: BTreeMap<&str, usize> = case
            .stages
            .iter()
            .enumerate()
            .map(|(index, stage)| (stage.stage_id.as_str(), index))
            .collect();
        let views: Vec<VerdictView> = by_trial
            .get(record.identity.trial_id.as_str())
            .into_iter()
            .flatten()
            .filter_map(|verdict| {
                Some(VerdictView {
                    verdict_id: verdict.verdict_id.clone(),
                    stage_index: *indices.get(verdict.stage_id.as_str())?,
                    check: verdict.check.clone(),
                    tier: verdict.tier,
                    kind: verdict.kind,
                    provider_reason: verdict.provider_reason,
                    score_bp: verdict.score_bp,
                    weight: verdict.weight,
                    regrade_of: verdict.regrade_of.clone(),
                })
            })
            .collect();
        scores.push(TrialScore {
            case_id: case_id.to_owned(),
            trial_index: record.identity.trial_index,
            score: case_trial_score(case.reducer, &latest_verdicts(views)),
        });
    }
    scores
}

/// One run's two cells, paired on their shared seed.
pub fn paired_evidence(
    definition: &EvalDefinition,
    rows: &RunRows,
    baseline_cell: &str,
    candidate_cell: &str,
) -> PairedEvidence {
    pair_trials(
        &cell_trial_scores(definition, rows, baseline_cell),
        &cell_trial_scores(definition, rows, candidate_cell),
    )
}

/// Several runs' pairs as one body of evidence. Pairs are appended and the
/// counters summed: a re-run adds pairs and never replaces them.
pub fn concat_paired(parts: &[PairedEvidence]) -> PairedEvidence {
    let mut total = PairedEvidence::default();
    for part in parts {
        total.pairs.extend(part.pairs.iter().cloned());
        total.keys += part.keys;
        total.dropped_baseline += part.dropped_baseline;
        total.dropped_candidate += part.dropped_candidate;
    }
    total
}

/// Tokens reported by one cell's counted trials, and how many reported none.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CellUsage {
    pub tokens: u64,
    pub trials: u64,
    pub missing: u64,
}

fn trial_tokens(usage: &TrialUsage) -> Option<u64> {
    match (usage.input_tokens, usage.output_tokens) {
        (None, None) => None,
        (input, output) => Some(input.unwrap_or(0) + output.unwrap_or(0)),
    }
}

pub fn cell_usage(rows: &RunRows, cell_id: &str) -> CellUsage {
    let mut usage = CellUsage::default();
    for record in latest_attempts(&rows.trials, cell_id) {
        let Some(completion) = &record.completion else {
            continue;
        };
        usage.trials += 1;
        match trial_tokens(&completion.usage) {
            Some(tokens) => usage.tokens += tokens,
            None => usage.missing += 1,
        }
    }
    usage
}
```

`CaseTrialScore` is used only by the test module's assertions; if rustc reports it unused in the non-test build, move it into the test module's imports.

- [ ] **Step 5: Run the tests**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents --lib eval::report`
Expected: PASS, 6 tests.

- [ ] **Step 6: Commit**

```bash
git add crates/gents/src/eval
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" \
  commit -m "feat(eval): report projection from verdict rows to paired evidence (#1455)

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

### Task 2: Decision evidence, feedback and the seed

**Files:**
- Create: `crates/gents/src/optimization/evidence.rs`
- Modify: `crates/gents/src/optimization/mod.rs`

The optimization half of the projection: several runs' pairs into one `Evidence` (F1), the cost gate's `TokenTotals` with the missing-usage skip, what the proposer may read from a train run, and the Monte Carlo seed.

**Interfaces:**
- Consumes:
  ```rust
  // crates/gents/src/eval/report/evidence.rs, PR 3 Task 1
  pub struct RunRows { pub run_id: String, pub case_ids: Vec<String>, pub invalidated: bool,
      pub trials: Vec<TrialRecord>, pub verdicts: Vec<VerdictRecord> }
  pub fn paired_evidence(definition: &EvalDefinition, rows: &RunRows, baseline_cell: &str,
      candidate_cell: &str) -> PairedEvidence;
  pub fn concat_paired(parts: &[PairedEvidence]) -> PairedEvidence;
  pub struct CellUsage { pub tokens: u64, pub trials: u64, pub missing: u64 }
  pub fn cell_usage(rows: &RunRows, cell_id: &str) -> CellUsage;
  // crates/gents/src/optimization/policy.rs (finding F4: this is where it lives)
  pub fn evidence_from_pairs(paired: &PairedEvidence, expected_cases: &[String],
      tokens: Option<TokenTotals>) -> Evidence;
  pub struct TokenTotals { pub baseline_tokens: u64, pub baseline_trials: u64,
      pub candidate_tokens: u64, pub candidate_trials: u64 }
  // crates/gents/src/optimization/proposer.rs, PR 2 Task 2
  pub struct CheckFeedback { pub check: String, pub score_bp: Option<u32>, pub feedback: Option<String> }
  ```
- Produces:
  ```rust
  pub const BASELINE_CELL: &str = "baseline";
  pub const CANDIDATE_CELL: &str = "candidate";
  pub fn token_totals(runs: &[RunRows], max_missing_usage_bp: u64) -> Option<TokenTotals>;
  pub fn decision_evidence(definition: &EvalDefinition, runs: &[RunRows], max_missing_usage_bp: u64) -> Evidence;
  pub fn train_feedback(verdicts: &[VerdictRecord]) -> Vec<CheckFeedback>;
  pub fn decision_seed(run_ids: &[String]) -> u64;
  ```

- [ ] **Step 1: Write the failing test** — create `crates/gents/src/optimization/evidence.rs` holding only this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::report::evidence::tests::{one_case_definition, run_rows, slot, Slot};

    fn improving(run_id: &str) -> RunRows {
        run_rows(
            run_id,
            &[
                slot(BASELINE_CELL, 0, 0),
                slot(BASELINE_CELL, 1, 0),
                slot(CANDIDATE_CELL, 0, 10_000),
                slot(CANDIDATE_CELL, 1, 10_000),
            ],
        )
    }

    /// F1: a re-run adds pairs, so two identical runs double the case's pairs
    /// and sums instead of collapsing to worst-case imputation.
    #[test]
    fn a_rerun_adds_pairs_to_the_decision() {
        let definition = one_case_definition();
        let once = decision_evidence(&definition, &[improving("v0")], 2_000);
        let twice = decision_evidence(&definition, &[improving("v0"), improving("v1")], 2_000);
        assert_eq!(once.cases[0].pairs, 2);
        assert_eq!(twice.cases[0].pairs, 4);
        assert_eq!(twice.cases[0].sum_baseline_bp, 0);
        assert_eq!(twice.cases[0].sum_candidate_bp, 40_000);
        assert_eq!(twice.keys, 4);
        assert!(twice.cases_match);
    }

    #[test]
    fn the_cost_gate_is_skipped_when_too_many_trials_report_no_usage() {
        let runs = [improving("v0")];
        let totals = token_totals(&runs, 2_000).expect("every trial reported usage");
        assert_eq!((totals.baseline_tokens, totals.baseline_trials), (40, 2));
        assert_eq!((totals.candidate_tokens, totals.candidate_trials), (40, 2));

        let half = run_rows(
            "v0",
            &[
                slot(BASELINE_CELL, 0, 0),
                Slot { tokens: None, ..slot(CANDIDATE_CELL, 0, 10_000) },
            ],
        );
        assert_eq!(
            token_totals(&[half], 2_000),
            None,
            "one trial in two without usage is past a 20% tolerance"
        );
    }

    #[test]
    fn feedback_reaches_the_proposer_as_check_name_score_and_text_only() {
        let rows = run_rows(
            "train",
            &[
                Slot { feedback: Some("name the collection"), ..slot(BASELINE_CELL, 0, 0) },
                slot(BASELINE_CELL, 1, 10_000),
            ],
        );
        let feedback = train_feedback(&rows.verdicts);
        assert_eq!(feedback.len(), 2);
        assert_eq!(feedback[0].check, "captured_rows_count");
        assert!(feedback
            .iter()
            .any(|entry| entry.feedback.as_deref() == Some("name the collection")));
        let rendered = serde_json::to_string(&feedback).unwrap();
        for forbidden in ["reason_code", "trial", "disk-warning", "stage", "train-"] {
            assert!(!rendered.contains(forbidden), "{forbidden} leaked: {rendered}");
        }
    }

    #[test]
    fn the_decision_seed_follows_the_run_ids_and_nothing_else() {
        let one = decision_seed(&["job-r1-v0".to_owned()]);
        assert_eq!(one, decision_seed(&["job-r1-v0".to_owned()]));
        assert_ne!(one, decision_seed(&["job-r1-v1".to_owned()]));
        assert_ne!(
            one,
            decision_seed(&["job-r1-v0".to_owned(), "job-r1-v1".to_owned()]),
            "a re-run changes the seed, because it changes the evidence"
        );
    }
}
```

- [ ] **Step 2: Run it to see it fail**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents --lib optimization::evidence`
Expected: FAIL to compile, `cannot find function decision_evidence in this scope`.

- [ ] **Step 3: Write the implementation** — above the test module:

```rust
//! What a decision reads, and what a proposer may read.
//!
//! The runs of one decision are paired one at a time by `eval::report` and
//! concatenated, so a re-run adds pairs (finding F1). What reaches a proposer
//! is narrower still: a check's name, what it scored and what it said.

use sha2::{Digest, Sha256};

use crate::document_config::EvalDefinition;
use crate::eval::report::{cell_usage, concat_paired, paired_evidence, RunRows};
use crate::eval::VerdictRecord;
use crate::optimization::policy::{evidence_from_pairs, Evidence, TokenTotals};
use crate::optimization::proposer::CheckFeedback;

/// The two cell ids every optimization run uses. A train run has only
/// [`BASELINE_CELL`], the checkpoint; the held-out run compares the original
/// baseline against the final checkpoint under the same two ids.
pub const BASELINE_CELL: &str = "baseline";
pub const CANDIDATE_CELL: &str = "candidate";

/// Mean tokens per case-trial for both cells across `runs`, or `None` when
/// more than `max_missing_usage_bp` of the counted trials reported no usage.
pub fn token_totals(runs: &[RunRows], max_missing_usage_bp: u64) -> Option<TokenTotals> {
    let (mut baseline, mut candidate) = ((0u64, 0u64), (0u64, 0u64));
    let (mut counted, mut missing) = (0u128, 0u128);
    for rows in runs {
        let base = cell_usage(rows, BASELINE_CELL);
        let cand = cell_usage(rows, CANDIDATE_CELL);
        baseline = (baseline.0 + base.tokens, baseline.1 + base.trials);
        candidate = (candidate.0 + cand.tokens, candidate.1 + cand.trials);
        counted += u128::from(base.trials + cand.trials);
        missing += u128::from(base.missing + cand.missing);
    }
    if counted == 0 || missing * 10_000 > u128::from(max_missing_usage_bp) * counted {
        tracing::info!(
            counted = counted as u64,
            missing = missing as u64,
            "optimization cost gate skipped: too many trials reported no usage"
        );
        return None;
    }
    Some(TokenTotals {
        baseline_tokens: baseline.0,
        baseline_trials: baseline.1,
        candidate_tokens: candidate.0,
        candidate_trials: candidate.1,
    })
}

/// The evidence one decision reads: every run paired separately, the pairs
/// concatenated, over the cases the first run froze. All runs of one decision
/// share a definition and a split, so they froze the same case list.
pub fn decision_evidence(
    definition: &EvalDefinition,
    runs: &[RunRows],
    max_missing_usage_bp: u64,
) -> Evidence {
    let parts: Vec<_> = runs
        .iter()
        .map(|rows| paired_evidence(definition, rows, BASELINE_CELL, CANDIDATE_CELL))
        .collect();
    let expected = runs
        .first()
        .map(|rows| rows.case_ids.clone())
        .unwrap_or_default();
    evidence_from_pairs(
        &concat_paired(&parts),
        &expected,
        token_totals(runs, max_missing_usage_bp),
    )
}

/// What a proposer may read from the train run: a check's name, what it
/// scored, and what it said. Ordered so two reads produce the same input.
pub fn train_feedback(verdicts: &[VerdictRecord]) -> Vec<CheckFeedback> {
    let mut ordered: Vec<&VerdictRecord> = verdicts.iter().collect();
    ordered.sort_by(|left, right| {
        (&left.check, &left.verdict_id).cmp(&(&right.check, &right.verdict_id))
    });
    ordered
        .into_iter()
        .map(|verdict| CheckFeedback {
            check: verdict.check.clone(),
            score_bp: verdict.score_bp,
            feedback: verdict.feedback.clone(),
        })
        .collect()
}

/// The Monte Carlo seed, derived from the runs the decision reads, so a
/// recomputed decision is identical.
pub fn decision_seed(run_ids: &[String]) -> u64 {
    let digest = Sha256::digest(run_ids.join("\n").as_bytes());
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    u64::from_le_bytes(bytes)
}
```

- [ ] **Step 4: Wire the module** — in `crates/gents/src/optimization/mod.rs` add `pub mod evidence;` in rustfmt order and:

```rust
pub use evidence::{
    decision_evidence, decision_seed, token_totals, train_feedback, BASELINE_CELL, CANDIDATE_CELL,
};
```

- [ ] **Step 5: Run the tests**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents --lib optimization::evidence`
Expected: PASS, 4 tests.

- [ ] **Step 6: Commit**

```bash
git add crates/gents/src/optimization
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" \
  commit -m "feat(optimization): decision evidence adds pairs across re-runs (#1455)

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

### Task 3: The eval-run bridge and the budget

**Files:**
- Create: `crates/gents/src/optimization/driver.rs` (this task writes the first half of the file)
- Modify: `crates/gents/src/optimization/mod.rs`

Finding F2 governs every function here: once a job is frozen, what a run *means* — its trials per case, its seed, its subject behavior, its inference profile, its definition — comes from `JobOrigin`, never from the request. The request contributes only operational settings that cannot change a result (concurrency, retries, the breaker, deadlines, directories, poll backoff).

**Interfaces:**
- Consumes:
  ```rust
  // crates/gents/src/eval/runner/freeze.rs
  pub enum CellSource { InstalledPack { name: String }, Directory(PathBuf) }
  pub struct CellRequest { pub cell_id: String, pub label: String, pub source: CellSource,
      pub behavior_id: String, pub inference_profile_id: String }
  pub struct RunRequest { pub run_id: String, pub owner: String, pub evaluator_did: String,
      pub definition_id: String, pub split: EvalSplit, pub case_ids: Option<Vec<String>>,
      pub cells: Vec<CellRequest>, pub trials_per_case: u32, pub seed_base: i64,
      pub deadline_secs: Option<u64>, pub concurrency: u32, pub max_infra_retries: u32,
      pub breaker_threshold: u32, pub purpose: String, pub source_commit: String,
      pub source_dirty: bool, pub captures: Vec<Capture>, pub runs_dir: PathBuf }
  // crates/gents/src/eval/runner/mod.rs
  pub async fn run(access: &ConfigAccess, request: &RunRequest, executor: &dyn TrialExecutor,
      registry: &CheckRegistry, cancel: CancellationToken, options: &RunOptions) -> Result<RunOutcome>;
  pub async fn resume(access: &ConfigAccess, owner: &str, run_id: &str, runs_dir: &Path,
      executor: &dyn TrialExecutor, registry: &CheckRegistry, cancel: CancellationToken,
      options: &RunOptions) -> Result<RunOutcome>;
  pub struct RunOptions { pub poll_backoff_base: Duration, pub poll_backoff_cap: Duration } // Default: 5 s, 60 s
  // crates/gents/src/config_client/mod.rs
  pub use desired_state::read_record as read_desired_state_record_in_txn;
  // i.e. pub async fn read_record(txn: &ConfigApplyTxn<'_>, collection: Collection, owner: &str,
  //     id: &str) -> Result<Option<(String, Value)>>
  // crates/gents/src/optimization/job.rs, PR 1 Task 4
  pub struct JobOrigin { /* … */ pub subject: SubjectRef, pub definition: DefinitionRef,
      pub trials_per_case: u32, pub seed_base: i64, pub inference_profile_id: String,
      pub jobs_dir: PathBuf, /* … */ }
  pub struct Budgets { pub max_rounds: u32, pub max_case_trials: u64, pub max_tokens: u64,
      pub deadline_unix_secs: Option<u64> }
  ```
- Produces:
  ```rust
  pub struct JobRequest { /* fields in the implementation below */ }
  pub fn job_dir(jobs_dir: &Path, job_id: &str) -> PathBuf;
  pub fn baseline_dir(jobs_dir: &Path, job_id: &str) -> PathBuf;
  pub fn candidate_dir(jobs_dir: &Path, job_id: &str, round: u32) -> PathBuf;
  pub(crate) fn candidate_staging_dir(jobs_dir: &Path, job_id: &str, round: u32) -> PathBuf;
  pub struct Spend { pub case_trials: u64, pub tokens: u64 }
  pub(crate) struct RunPlan { pub run_id: String, pub split: EvalSplit, pub round: Option<u32>,
      pub cells: Vec<(&'static str, PathBuf)>, pub seed_base: i64 }
  pub(crate) fn train_plan(job_id: &str, origin: &JobOrigin, round: u32, checkpoint: &Path) -> RunPlan;
  pub(crate) fn validation_plan(job_id: &str, origin: &JobOrigin, round: u32, attempt: u32,
      checkpoint: &Path, candidate: &Path) -> RunPlan;
  pub(crate) fn held_out_plan(job_id: &str, origin: &JobOrigin, baseline: &Path, checkpoint: &Path) -> RunPlan;
  pub(crate) fn run_request(request: &JobRequest, origin: &JobOrigin, plan: &RunPlan) -> RunRequest;
  pub(crate) async fn execute_run(access: &ConfigAccess, request: &JobRequest, origin: &JobOrigin,
      plan: &RunPlan, executor: &dyn TrialExecutor, registry: &CheckRegistry,
      cancel: CancellationToken) -> Result<RunOutcome>;
  pub fn split_case_count(definition: &EvalDefinition, split: EvalSplit) -> u64;
  pub fn run_cost(definition: &EvalDefinition, split: EvalSplit, trials_per_case: u32, cells: u64) -> u64;
  pub async fn spend_so_far(access: &ConfigAccess, owner: &str, journal: &[JournalEntry]) -> Result<Spend>;
  pub(crate) async fn load_definition(access: &ConfigAccess, owner: &str, definition_id: &str) -> Result<EvalDefinition>;
  pub(crate) fn definition_ref(definition: &EvalDefinition) -> Result<DefinitionRef>;
  pub(crate) fn now_unix_secs() -> u64;
  ```

- [ ] **Step 1: Write the failing test** — create `crates/gents/src/optimization/driver.rs` holding only this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::{DefinitionRef, SubjectRef};
    use crate::optimization::policy::PolicyV2;
    use crate::optimization::target::{Target, TargetField};
    use serde_json::json;
    use std::path::PathBuf;

    fn definition() -> EvalDefinition {
        let case = |case_id: &str, split: &str| {
            json!({
                "case_id": case_id,
                "split": split,
                "stages": [{
                    "stage_id": "check",
                    "prompt": "Run the monitor.",
                    "deadline_secs": 600,
                    "checks": [{"check": "captured_rows_count", "params": {"name": "findings", "min": 1}, "tier": "acceptance"}],
                }],
            })
        };
        serde_json::from_value(json!({
            "definition_id": "monitor-findings",
            "agent_did": "did:key:o",
            "comparability_version": 1,
            "subject": {"kind": "behavior", "inference_slots": ["primary"]},
            "cases": [
                case("train-a", "train"),
                case("val-a", "validation"),
                case("val-b", "validation"),
                case("held-a", "held_out"),
            ],
        }))
        .unwrap()
    }

    fn budgets() -> Budgets {
        Budgets {
            max_rounds: 3,
            max_case_trials: 1_000,
            max_tokens: 1_000_000,
            deadline_unix_secs: None,
        }
    }

    pub(super) fn request() -> JobRequest {
        JobRequest {
            job_id: "job-1".into(),
            owner: "did:key:o".into(),
            evaluator_did: "did:key:home".into(),
            behavior_id: "monitor".into(),
            definition_id: "monitor-findings".into(),
            inference_profile_id: "local".into(),
            baseline_pack: PathBuf::from("/tmp/baseline"),
            trials_per_case: 2,
            budgets: budgets(),
            max_text_bytes: 32 * 1024,
            seed_base: 1_000,
            jobs_dir: PathBuf::from("/home/eval/jobs"),
            runs_dir: PathBuf::from("/home/eval/runs"),
            source_commit: "0deb7659c".into(),
            source_dirty: false,
            concurrency: 1,
            max_infra_retries: 1,
            breaker_threshold: 5,
            deadline_secs: Some(600),
            run_options: RunOptions::default(),
        }
    }

    /// The origin `request()` would freeze.
    pub(super) fn origin() -> JobOrigin {
        JobOrigin {
            target: Target {
                field: TargetField::AgentContextSystemPrompt,
                owner: "did:key:o".into(),
                id: "monitor-context".into(),
            },
            closure: Vec::new(),
            subject: SubjectRef {
                pack_digest: "sha256:baseline".into(),
                behavior_id: "monitor".into(),
            },
            definition: DefinitionRef {
                definition_id: "monitor-findings".into(),
                comparability_version: 1,
                digest: "sha256:definition".into(),
            },
            policy: PolicyV2::uncalibrated(),
            trials_per_case: 2,
            budgets: budgets(),
            baseline_text: "Watch the mailbox.\n".into(),
            owner: "did:key:o".into(),
            seed_base: 1_000,
            inference_profile_id: "local".into(),
            max_text_bytes: 32 * 1024,
            jobs_dir: PathBuf::from("/home/eval/jobs"),
        }
    }

    #[test]
    fn a_train_run_has_one_cell_and_a_validation_run_has_two_on_one_seed() {
        let origin = origin();
        let baseline = baseline_dir(&origin.jobs_dir, "job-1");
        assert_eq!(baseline, PathBuf::from("/home/eval/jobs/job-1/baseline"));

        let train = train_plan("job-1", &origin, 1, &baseline);
        assert_eq!(train.split, EvalSplit::Train);
        assert_eq!(train.cells.len(), 1);
        assert_eq!(train.cells[0].0, BASELINE_CELL);
        assert_eq!(train.run_id, "job-1-r1-train");

        let candidate = candidate_dir(&origin.jobs_dir, "job-1", 1);
        assert_eq!(candidate, PathBuf::from("/home/eval/jobs/job-1/rounds/1/candidate"));
        let validation = validation_plan("job-1", &origin, 1, 0, &baseline, &candidate);
        assert_eq!(validation.run_id, "job-1-r1-v0");
        assert_eq!(
            validation.cells.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            vec![BASELINE_CELL, CANDIDATE_CELL]
        );

        let frozen = run_request(&request(), &origin, &validation);
        assert_eq!(frozen.purpose, "optimization:job-1");
        assert_eq!(frozen.cells.len(), 2);
        assert_eq!(frozen.cells[0].label, BASELINE_CELL, "labels are what scripts key on");
        assert_eq!(
            frozen.cells[0].inference_profile_id,
            frozen.cells[1].inference_profile_id,
            "both arms run the same inference binding"
        );
        assert!(frozen.captures.is_empty());
        assert_eq!(frozen.seed_base, validation.seed_base);
    }

    /// F2: a run's meaning comes from the frozen origin, never the request.
    #[test]
    fn a_run_request_reads_what_a_run_means_from_the_origin() {
        let mut drifted = request();
        drifted.trials_per_case = 5;
        drifted.seed_base = 9_999;
        drifted.behavior_id = "other".into();
        drifted.inference_profile_id = "other".into();
        drifted.definition_id = "other".into();
        let origin = origin();
        let plan = validation_plan("job-1", &origin, 1, 0, Path::new("a"), Path::new("b"));
        let frozen = run_request(&drifted, &origin, &plan);
        assert_eq!(frozen.trials_per_case, 2);
        assert_eq!(frozen.definition_id, "monitor-findings");
        assert!(frozen
            .cells
            .iter()
            .all(|cell| cell.behavior_id == "monitor" && cell.inference_profile_id == "local"));
        assert_eq!(
            plan.seed_base,
            validation_plan("job-1", &origin, 1, 0, Path::new("a"), Path::new("b")).seed_base
        );
        assert!(plan.seed_base < 9_999, "the seed follows origin.seed_base");
    }

    #[test]
    fn a_rerun_draws_a_new_seed_base_and_a_later_round_never_collides() {
        let origin = origin();
        let seed = |round, attempt| {
            validation_plan("job-1", &origin, round, attempt, Path::new("a"), Path::new("b")).seed_base
        };
        let (first, rerun, next_round) = (seed(1, 0), seed(1, 1), seed(2, 0));
        assert_ne!(first, rerun);
        assert_ne!(first, next_round);
        assert_ne!(rerun, next_round);
        // A trial draws `seed_base + trial_index`, so the gaps must be wider
        // than any run's trials per case.
        assert!(rerun - first >= 100, "{first} {rerun}");
        assert!(next_round - first >= 1_000, "{first} {next_round}");
    }

    #[test]
    fn a_run_costs_its_split_times_its_trials_times_its_cells() {
        let definition = definition();
        assert_eq!(split_case_count(&definition, EvalSplit::Validation), 2);
        assert_eq!(split_case_count(&definition, EvalSplit::HeldOut), 1);
        assert_eq!(run_cost(&definition, EvalSplit::Train, 2, 1), 2);
        assert_eq!(run_cost(&definition, EvalSplit::Validation, 2, 2), 8);
        assert_eq!(run_cost(&definition, EvalSplit::HeldOut, 2, 2), 4);
    }

    #[test]
    fn the_held_out_run_compares_the_original_baseline_with_the_final_checkpoint() {
        let origin = origin();
        let plan = held_out_plan(
            "job-1",
            &origin,
            &baseline_dir(&origin.jobs_dir, "job-1"),
            &candidate_dir(&origin.jobs_dir, "job-1", 2),
        );
        assert_eq!(plan.run_id, "job-1-held-out");
        assert_eq!(plan.split, EvalSplit::HeldOut);
        assert_eq!(plan.round, None);
        assert_eq!(plan.cells[0].1, PathBuf::from("/home/eval/jobs/job-1/baseline"));
        assert_eq!(
            plan.cells[1].1,
            PathBuf::from("/home/eval/jobs/job-1/rounds/2/candidate")
        );
    }
}
```

- [ ] **Step 2: Run it to see it fail**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents --lib optimization::driver`
Expected: FAIL to compile, `cannot find type JobRequest in this scope`.

- [ ] **Step 3: Write the implementation** — above the test module in `driver.rs`:

```rust
//! The driver: the loop that turns a frozen baseline into a checkpoint.
//!
//! Everything the driver decides comes from the job's frozen origin and its
//! journal (finding F2), so a resumed job and an uninterrupted one reach the
//! same state. The driver is the only writer of the job, as the owner DID; it
//! never claims a request, and no runtime reconciles what it writes.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use tokio_util::sync::CancellationToken;

use crate::config_client::{desired_state_document_digest, ConfigAccess};
use crate::document_config::{EvalDefinition, EvalSplit};
use crate::eval::checks::CheckRegistry;
use crate::eval::runner::{
    resume, run, CellRequest, CellSource, RunOptions, RunOutcome, RunRequest, TrialExecutor,
};
use crate::eval::{load_run, load_trials, DefinitionRef};
use crate::optimization::evidence::{BASELINE_CELL, CANDIDATE_CELL};
use crate::optimization::job::{Budgets, JobOrigin, JournalEntry};

/// Everything an operator chose about a job. Read in full only when the job
/// is created; a resume must repeat it, and `run_job` refuses one that does
/// not (finding F2).
#[derive(Clone, Debug)]
pub struct JobRequest {
    pub job_id: String,
    /// The principal the job and its runs belong to, and the only DID that may
    /// promote.
    pub owner: String,
    /// The DID of the home that launched the job, recorded on every run.
    pub evaluator_did: String,
    pub behavior_id: String,
    pub definition_id: String,
    pub inference_profile_id: String,
    /// The operator-supplied baseline subject pack (ruling R5). Copied into the
    /// job's directory at freeze, after its prompt is cross-checked against the
    /// live configuration; the copy is what every run reads.
    pub baseline_pack: PathBuf,
    pub trials_per_case: u32,
    pub budgets: Budgets,
    pub max_text_bytes: usize,
    pub seed_base: i64,
    /// `<launching home>/eval/jobs` (ruling R8). This job owns
    /// `<jobs_dir>/<job_id>/`; M4's `gents optimization rm` removes it.
    pub jobs_dir: PathBuf,
    /// `<launching home>/eval/runs`, handed to the runner unchanged.
    pub runs_dir: PathBuf,
    pub source_commit: String,
    pub source_dirty: bool,
    pub concurrency: u32,
    pub max_infra_retries: u32,
    pub breaker_threshold: u32,
    pub deadline_secs: Option<u64>,
    /// Not comparability data: how long the runner backs off between passes.
    pub run_options: RunOptions,
}

pub fn job_dir(jobs_dir: &Path, job_id: &str) -> PathBuf {
    jobs_dir.join(job_id)
}

pub fn baseline_dir(jobs_dir: &Path, job_id: &str) -> PathBuf {
    job_dir(jobs_dir, job_id).join("baseline")
}

pub fn candidate_dir(jobs_dir: &Path, job_id: &str, round: u32) -> PathBuf {
    job_dir(jobs_dir, job_id)
        .join("rounds")
        .join(round.to_string())
        .join("candidate")
}

/// Where a round's candidate is written before its `Proposed` entry exists
/// (finding F7). Only after the journal names it is it renamed into place.
pub(crate) fn candidate_staging_dir(jobs_dir: &Path, job_id: &str, round: u32) -> PathBuf {
    job_dir(jobs_dir, job_id)
        .join("rounds")
        .join(round.to_string())
        .join("candidate.staging")
}

/// What the job has spent so far, read from the runs its journal names.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Spend {
    pub case_trials: u64,
    pub tokens: u64,
}

/// One run the driver is about to ask the runner for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RunPlan {
    pub run_id: String,
    pub split: EvalSplit,
    pub round: Option<u32>,
    /// `(cell_id, pack directory)`, baseline first.
    pub cells: Vec<(&'static str, PathBuf)>,
    pub seed_base: i64,
}

/// Seeds are spaced so a re-run and a later round never draw the same trial
/// seeds: a trial's seed is `seed_base + trial_index`, and no run has 100
/// trials of one case.
fn seed_base_for(base: i64, round: u32, attempt: u32) -> i64 {
    base + i64::from(round) * 1_000 + i64::from(attempt) * 100
}

pub(crate) fn train_plan(job_id: &str, origin: &JobOrigin, round: u32, checkpoint: &Path) -> RunPlan {
    RunPlan {
        run_id: format!("{job_id}-r{round}-train"),
        split: EvalSplit::Train,
        round: Some(round),
        cells: vec![(BASELINE_CELL, checkpoint.to_path_buf())],
        seed_base: seed_base_for(origin.seed_base, round, 0),
    }
}

pub(crate) fn validation_plan(
    job_id: &str,
    origin: &JobOrigin,
    round: u32,
    attempt: u32,
    checkpoint: &Path,
    candidate: &Path,
) -> RunPlan {
    RunPlan {
        run_id: format!("{job_id}-r{round}-v{attempt}"),
        split: EvalSplit::Validation,
        round: Some(round),
        cells: vec![
            (BASELINE_CELL, checkpoint.to_path_buf()),
            (CANDIDATE_CELL, candidate.to_path_buf()),
        ],
        seed_base: seed_base_for(origin.seed_base, round, attempt),
    }
}

/// The one held-out run of a job: the original baseline against the final
/// checkpoint. Run once, never re-run.
pub(crate) fn held_out_plan(
    job_id: &str,
    origin: &JobOrigin,
    baseline: &Path,
    checkpoint: &Path,
) -> RunPlan {
    RunPlan {
        run_id: format!("{job_id}-held-out"),
        split: EvalSplit::HeldOut,
        round: None,
        cells: vec![
            (BASELINE_CELL, baseline.to_path_buf()),
            (CANDIDATE_CELL, checkpoint.to_path_buf()),
        ],
        seed_base: seed_base_for(origin.seed_base, 0, 0),
    }
}

/// What a run means comes from `origin`; how it is executed comes from
/// `request`.
pub(crate) fn run_request(request: &JobRequest, origin: &JobOrigin, plan: &RunPlan) -> RunRequest {
    RunRequest {
        run_id: plan.run_id.clone(),
        owner: origin.owner.clone(),
        evaluator_did: request.evaluator_did.clone(),
        definition_id: origin.definition.definition_id.clone(),
        split: plan.split,
        case_ids: None,
        cells: plan
            .cells
            .iter()
            .map(|(cell_id, pack)| CellRequest {
                cell_id: (*cell_id).to_owned(),
                label: (*cell_id).to_owned(),
                source: CellSource::Directory(pack.clone()),
                behavior_id: origin.subject.behavior_id.clone(),
                inference_profile_id: origin.inference_profile_id.clone(),
            })
            .collect(),
        trials_per_case: origin.trials_per_case,
        seed_base: plan.seed_base,
        deadline_secs: request.deadline_secs,
        concurrency: request.concurrency,
        max_infra_retries: request.max_infra_retries,
        breaker_threshold: request.breaker_threshold,
        purpose: format!("optimization:{}", request.job_id),
        source_commit: request.source_commit.clone(),
        source_dirty: request.source_dirty,
        // Grading reads stage evidence; an optimization run captures nothing
        // out of a trial home.
        captures: Vec::new(),
        runs_dir: request.runs_dir.clone(),
    }
}

/// Freeze and run `plan`, or resume it when its row already exists.
pub(crate) async fn execute_run(
    access: &ConfigAccess,
    request: &JobRequest,
    origin: &JobOrigin,
    plan: &RunPlan,
    executor: &dyn TrialExecutor,
    registry: &CheckRegistry,
    cancel: CancellationToken,
) -> Result<RunOutcome> {
    if load_run(access, &origin.owner, &plan.run_id).await?.is_some() {
        tracing::info!(run_id = %plan.run_id, "optimization run resumed");
        return resume(
            access,
            &origin.owner,
            &plan.run_id,
            &request.runs_dir,
            executor,
            registry,
            cancel,
            &request.run_options,
        )
        .await;
    }
    run(
        access,
        &run_request(request, origin, plan),
        executor,
        registry,
        cancel,
        &request.run_options,
    )
    .await
}

pub fn split_case_count(definition: &EvalDefinition, split: EvalSplit) -> u64 {
    definition
        .cases
        .iter()
        .filter(|case| case.split == split)
        .count() as u64
}

/// How many case-trials a run of `split` costs.
pub fn run_cost(
    definition: &EvalDefinition,
    split: EvalSplit,
    trials_per_case: u32,
    cells: u64,
) -> u64 {
    split_case_count(definition, split) * u64::from(trials_per_case) * cells
}

/// What the job has already spent: every trial row of every run its journal
/// started, and the tokens those trials reported.
pub async fn spend_so_far(
    access: &ConfigAccess,
    owner: &str,
    journal: &[JournalEntry],
) -> Result<Spend> {
    let mut spend = Spend::default();
    for entry in journal {
        let JournalEntry::RunStarted { run_id, .. } = entry else {
            continue;
        };
        for record in load_trials(access, owner, run_id).await? {
            spend.case_trials += 1;
            if let Some(completion) = &record.completion {
                spend.tokens += completion.usage.input_tokens.unwrap_or(0)
                    + completion.usage.output_tokens.unwrap_or(0);
            }
        }
    }
    Ok(spend)
}

/// Read the installed definition the way freezing does, so the driver and the
/// runner agree on what a case is.
pub(crate) async fn load_definition(
    access: &ConfigAccess,
    owner: &str,
    definition_id: &str,
) -> Result<EvalDefinition> {
    let found = access
        .transact("optimization.load_definition", |txn| {
            Box::pin(async move {
                crate::config_client::read_desired_state_record_in_txn(
                    txn,
                    crate::Collection::EvalDefinition,
                    owner,
                    definition_id,
                )
                .await
            })
        })
        .await?;
    let (_, value) =
        found.with_context(|| format!("no eval definition {definition_id:?} for {owner}"))?;
    serde_json::from_value(value)
        .with_context(|| format!("decoding eval definition {definition_id:?}"))
}

/// The same identity `eval::runner::freeze` records on a run's origin.
pub(crate) fn definition_ref(definition: &EvalDefinition) -> Result<DefinitionRef> {
    Ok(DefinitionRef {
        definition_id: definition.definition_id.clone(),
        comparability_version: definition.comparability_version,
        digest: desired_state_document_digest(&serde_json::to_value(definition)?)?,
    })
}

pub(crate) fn now_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs()
}
```

- [ ] **Step 4: Wire the module** — in `crates/gents/src/optimization/mod.rs` add `pub mod driver;` in rustfmt order and:

```rust
pub use driver::{
    baseline_dir, candidate_dir, job_dir, run_cost, spend_so_far, split_case_count, JobRequest,
    Spend,
};
```

- [ ] **Step 5: Run the tests**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents --lib optimization::driver`
Expected: PASS, 5 tests.

- [ ] **Step 6: Commit**

```bash
git add crates/gents/src/optimization
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" \
  commit -m "feat(optimization): eval-run plans from the frozen origin, and the budget (#1455)

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

### Task 4: `run_job`

**Files:**
- Modify: `crates/gents/src/optimization/driver.rs` (append to the file Task 3 created)
- Modify: `crates/gents/src/optimization/mod.rs`

What this task settles, finding by finding:

- **F2.** After freeze every decision reads `job.origin`: `policy`, `budgets`, `seed_base`, `trials_per_case`, `inference_profile_id`, `max_text_bytes`, `jobs_dir`. A resume whose request disagrees with the origin is `JobRefused` and writes nothing.
- **F5.** Definition drift is checked before closure drift, and the closure excludes `EvalDefinition` (PR 1 Task 3), so a definition edit is `Failed(definition_changed)` and never `baseline_drifted`.
- **F7.** A round's candidate is written into `candidate.staging` and renamed to `candidate` only after its `Proposed` entry is journaled; at the start of an unproposed round both paths are removed. `Exhausted` is derived from the journal, not from a local flag, so a resumed job finalizes the same way.
- **R5.** At freeze the operator-supplied pack's context prompt must equal the live closure's text for the same context, or the job is refused.
- **R9.** There is no fault-injection seam. A cancelled token makes `run_job` return `Ok` with the job still `Running` after the run in progress; an erroring proposer returns `Err` with nothing journaled for the round's proposal. Both are resumable, and PR 3 Task 5 compares such a job against an uninterrupted twin.

**Interfaces:**
- Consumes: everything PR 1, PR 2 and PR 3 Tasks 1 to 3 produced, plus
  ```rust
  // crates/gents/src/optimization/policy.rs
  pub fn decide(mode: Mode, policy: &PolicyV2, evidence: &Evidence, seed: u64) -> DecisionReport;
  pub enum Decision { Accept, Reject(RejectReason), Inconclusive(InconclusiveReason) }
  pub struct DecisionReport { pub decision: Decision, pub policy_version: String, pub improved: u32,
      pub tied: u32, pub worsened: u32, pub mean_diff_bp: Option<i64>, pub p_ppm: Option<u64>,
      pub alpha_effective_ppm: u64, pub cost_skipped: bool }
  // crates/gents/src/eval/report/evidence.rs
  pub async fn load_run_rows(access: &ConfigAccess, owner: &str, run_id: &str) -> Result<RunRows>;
  // crates/gents/src/eval/documents.rs
  pub async fn load_verdicts(access: &ConfigAccess, owner: &str, run_id: &str) -> Result<Vec<VerdictRecord>>;
  ```
- Produces:
  ```rust
  pub struct JobOutcome { pub job_id: String, pub state: JobState,
      pub checkpoint: Option<Checkpoint>, pub rounds_used: u32 }
  pub struct JobRefused(pub String);
  pub fn job_refused(error: &anyhow::Error) -> Option<&JobRefused>;
  pub async fn run_job(
      access: &ConfigAccess,
      request: &JobRequest,
      executor: &dyn TrialExecutor,
      proposer: &dyn Proposer,
      registry: &CheckRegistry,
      policy: &PolicyV2,
      cancel: CancellationToken,
  ) -> Result<JobOutcome>;
  pub(crate) fn check_policy(request: &JobRequest, policy: &PolicyV2) -> Result<()>;
  pub(crate) fn check_resume(request: &JobRequest, policy: &PolicyV2, origin: &JobOrigin) -> Result<()>;
  pub(crate) fn proposed_for(journal: &[JournalEntry], round: u32) -> Option<(String, String, String)>;
  pub(crate) fn round_is_closed(journal: &[JournalEntry], round: u32) -> bool;
  pub(crate) fn seen_digests(baseline_digest: &str, journal: &[JournalEntry], round: u32) -> Vec<String>;
  pub(crate) fn budget_exhausted(journal: &[JournalEntry]) -> bool;
  ```

- [ ] **Step 1: Write the failing test** — append to the `#[cfg(test)] mod tests` in `driver.rs`:

```rust
    #[test]
    fn a_policy_that_disagrees_with_the_round_budget_is_refused() {
        let mut policy = PolicyV2::uncalibrated();
        policy.max_rounds = 5;
        let error = check_policy(&request(), &policy).unwrap_err();
        let refusal = job_refused(&error).unwrap_or_else(|| panic!("{error:#}"));
        assert!(refusal.0.contains("max_rounds"), "{}", refusal.0);

        policy.max_rounds = 3;
        check_policy(&request(), &policy).unwrap();
    }

    /// F2: a resume must repeat the request the job was frozen from.
    #[test]
    fn a_resume_that_disagrees_with_the_origin_is_refused_by_field() {
        let policy = PolicyV2::uncalibrated();
        check_resume(&request(), &policy, &origin()).unwrap();

        let mut drifted = request();
        drifted.seed_base = 7;
        drifted.trials_per_case = 3;
        let error = check_resume(&drifted, &policy, &origin()).unwrap_err();
        let refusal = job_refused(&error).unwrap_or_else(|| panic!("{error:#}"));
        assert!(refusal.0.contains("seed_base"), "{}", refusal.0);
        assert!(refusal.0.contains("trials_per_case"), "{}", refusal.0);

        let other_policy = PolicyV2 {
            alpha_ppm: 10_000,
            ..PolicyV2::uncalibrated()
        };
        let error = check_resume(&request(), &other_policy, &origin()).unwrap_err();
        assert!(job_refused(&error).unwrap().0.contains("policy"));
    }

    #[test]
    fn a_round_is_replayed_from_its_journal_rather_than_reproposed() {
        let journal = vec![
            JournalEntry::Frozen,
            JournalEntry::RunStarted {
                run_id: "job-1-r1-train".into(),
                round: Some(1),
                split: EvalSplit::Train,
            },
            JournalEntry::Proposed {
                round: 1,
                text: "candidate one".into(),
                rationale: "widen".into(),
                candidate_digest: "sha256:one".into(),
            },
        ];
        let proposed = proposed_for(&journal, 1).expect("round 1 already proposed");
        assert_eq!((proposed.0.as_str(), proposed.2.as_str()), ("candidate one", "sha256:one"));
        assert!(!round_is_closed(&journal, 1), "no Decided and no StructuralReject");
        assert!(proposed_for(&journal, 2).is_none());
        assert!(!budget_exhausted(&journal));

        let mut closed = journal.clone();
        closed.push(JournalEntry::StructuralReject {
            round: 1,
            diagnostics: "duplicate_candidate: already evaluated".into(),
        });
        assert!(round_is_closed(&closed, 1));

        closed.push(JournalEntry::BudgetExhausted {
            round: Some(2),
            reason: "no budget".into(),
        });
        assert!(budget_exhausted(&closed), "F7: exhaustion is read from the journal");
    }

    #[test]
    fn the_seen_digests_are_the_baseline_and_every_earlier_candidate() {
        let proposed = |round: u32, digest: &str| JournalEntry::Proposed {
            round,
            text: String::new(),
            rationale: String::new(),
            candidate_digest: digest.into(),
        };
        let journal = vec![
            JournalEntry::Frozen,
            proposed(1, "sha256:one"),
            proposed(2, "sha256:two"),
        ];
        assert_eq!(
            seen_digests("sha256:baseline", &journal, 3),
            vec!["sha256:baseline", "sha256:one", "sha256:two"]
        );
        assert_eq!(
            seen_digests("sha256:baseline", &journal, 2),
            vec!["sha256:baseline", "sha256:one"],
            "a replayed round never compares itself against its own digest"
        );
    }
```

- [ ] **Step 2: Run it to see it fail**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents --lib optimization::driver`
Expected: FAIL to compile, `cannot find function check_policy in this scope`.

- [ ] **Step 3: Write the implementation** — append to `driver.rs`, above its test module:

```rust
/// A job that will not start or resume, and why. Distinct from a `Failed`
/// job: nothing was written.
#[derive(Debug)]
pub struct JobRefused(pub String);

impl std::fmt::Display for JobRefused {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for JobRefused {}

pub fn job_refused(error: &anyhow::Error) -> Option<&JobRefused> {
    error.downcast_ref::<JobRefused>()
}

fn refused(reason: impl Into<String>) -> anyhow::Error {
    anyhow::Error::from(JobRefused(reason.into()))
}

/// What one pass over a job produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobOutcome {
    pub job_id: String,
    /// `Running` when the pass stopped on cancellation; call again to resume.
    pub state: JobState,
    /// The retained checkpoint, never the best candidate the job ever saw.
    pub checkpoint: Option<Checkpoint>,
    pub rounds_used: u32,
}

/// `alpha_effective = alpha / max_rounds` is a Bonferroni correction for an
/// optimizer that tries several candidates on one validation split, so the
/// divisor has to be the number of candidates the budget allows.
pub(crate) fn check_policy(request: &JobRequest, policy: &PolicyV2) -> Result<()> {
    if policy.max_rounds == request.budgets.max_rounds {
        return Ok(());
    }
    Err(refused(format!(
        "policy max_rounds {} does not match the budget's max_rounds {}; the Bonferroni divisor must be the number of candidates the job may try",
        policy.max_rounds, request.budgets.max_rounds
    )))
}

/// Finding F2: a resume must repeat the request the job was frozen from. The
/// origin is authoritative; this only refuses a caller who believes otherwise.
pub(crate) fn check_resume(
    request: &JobRequest,
    policy: &PolicyV2,
    origin: &JobOrigin,
) -> Result<()> {
    let mut differs = Vec::new();
    if &origin.policy != policy {
        differs.push("policy");
    }
    if origin.budgets != request.budgets {
        differs.push("budgets");
    }
    if origin.trials_per_case != request.trials_per_case {
        differs.push("trials_per_case");
    }
    if origin.seed_base != request.seed_base {
        differs.push("seed_base");
    }
    if origin.subject.behavior_id != request.behavior_id {
        differs.push("behavior_id");
    }
    if origin.definition.definition_id != request.definition_id {
        differs.push("definition_id");
    }
    if origin.inference_profile_id != request.inference_profile_id {
        differs.push("inference_profile_id");
    }
    if origin.max_text_bytes != request.max_text_bytes {
        differs.push("max_text_bytes");
    }
    if origin.jobs_dir != request.jobs_dir {
        differs.push("jobs_dir");
    }
    if origin.owner != request.owner {
        differs.push("owner");
    }
    if differs.is_empty() {
        return Ok(());
    }
    Err(refused(format!(
        "job {} was frozen with a different {}; a resume must repeat the request the job was created from",
        request.job_id,
        differs.join(", ")
    )))
}

pub(crate) fn proposed_for(
    journal: &[JournalEntry],
    round: u32,
) -> Option<(String, String, String)> {
    journal.iter().find_map(|entry| match entry {
        JournalEntry::Proposed {
            round: proposed,
            text,
            rationale,
            candidate_digest,
        } if *proposed == round => Some((
            text.clone(),
            rationale.clone(),
            candidate_digest.clone(),
        )),
        _ => None,
    })
}

/// A round is closed once it has been decided, structurally rejected, or
/// abandoned on budget. Replay opens and closes rounds; nothing else does.
pub(crate) fn round_is_closed(journal: &[JournalEntry], round: u32) -> bool {
    journal.iter().any(|entry| match entry {
        JournalEntry::Decided {
            round: Some(decided),
            ..
        } => *decided == round,
        JournalEntry::StructuralReject {
            round: rejected, ..
        } => *rejected == round,
        JournalEntry::BudgetExhausted {
            round: Some(exhausted),
            ..
        } => *exhausted == round,
        _ => false,
    })
}

/// Finding F7: whether the job ran out of budget is a fact of the journal.
pub(crate) fn budget_exhausted(journal: &[JournalEntry]) -> bool {
    journal
        .iter()
        .any(|entry| matches!(entry, JournalEntry::BudgetExhausted { .. }))
}

/// Every digest this job has already evaluated, before `round`.
pub(crate) fn seen_digests(
    baseline_digest: &str,
    journal: &[JournalEntry],
    round: u32,
) -> Vec<String> {
    let mut digests = vec![baseline_digest.to_owned()];
    for entry in journal {
        if let JournalEntry::Proposed {
            round: proposed,
            candidate_digest,
            ..
        } = entry
        {
            if *proposed < round {
                digests.push(candidate_digest.clone());
            }
        }
    }
    digests
}

/// Every rejection the proposer is told about. `Inconclusive` and
/// budget-exhausted rounds are never negatives.
fn rejections(journal: &[JournalEntry]) -> Vec<Rejection> {
    let mut history = Vec::new();
    for entry in journal {
        let (round, reason) = match entry {
            JournalEntry::StructuralReject { round, diagnostics } => (*round, diagnostics.clone()),
            JournalEntry::Decided {
                round: Some(round),
                decision: Decision::Reject(reason),
                ..
            } => (*round, format!("{reason:?}")),
            _ => continue,
        };
        if let Some((text, rationale, _)) = proposed_for(journal, round) {
            history.push(Rejection {
                round,
                text,
                rationale,
                reason,
            });
        }
    }
    history
}

fn summary_of(report: &DecisionReport) -> DecisionSummary {
    DecisionSummary {
        improved: report.improved,
        tied: report.tied,
        worsened: report.worsened,
        mean_diff_bp: report.mean_diff_bp,
        p_ppm: report.p_ppm,
        alpha_effective_ppm: report.alpha_effective_ppm,
        cost_skipped: report.cost_skipped,
    }
}

fn run_started(journal: &[JournalEntry], run_id: &str) -> bool {
    journal.iter().any(|entry| {
        matches!(entry, JournalEntry::RunStarted { run_id: started, .. } if started == run_id)
    })
}

fn remove_if_present(dir: &Path) -> Result<()> {
    if dir.exists() {
        std::fs::remove_dir_all(dir).with_context(|| format!("removing {}", dir.display()))?;
    }
    Ok(())
}

/// Finding F7: the journaled round's candidate, in place and still digesting to
/// what `Proposed` recorded. A crash between the journal write and the rename
/// leaves only the staging directory, which is renamed here.
fn proposed_candidate(
    origin: &JobOrigin,
    job_id: &str,
    round: u32,
    digest: &str,
) -> Result<MaterializedPack> {
    let final_dir = candidate_dir(&origin.jobs_dir, job_id, round);
    let staging = candidate_staging_dir(&origin.jobs_dir, job_id, round);
    if !final_dir.exists() {
        std::fs::rename(&staging, &final_dir).with_context(|| {
            format!(
                "round {round} was proposed but neither {} nor {} holds its candidate",
                final_dir.display(),
                staging.display()
            )
        })?;
    }
    let candidate = materialize_pack(&final_dir, &origin.owner, &origin.subject.behavior_id)?;
    anyhow::ensure!(
        candidate.digest == digest,
        "round {round}'s candidate digests to {}, not the journaled {digest}",
        candidate.digest
    );
    Ok(candidate)
}

async fn read_closure(access: &ConfigAccess, owner: &str) -> Result<Closure> {
    let owner = owner.to_owned();
    access
        .transact("optimization.capture_closure", |txn| {
            let owner = &owner;
            Box::pin(async move { capture_closure(txn, owner).await })
        })
        .await
}

/// Create the job: copy the baseline pack in, cross-check its prompt against
/// the live configuration (ruling R5), freeze the origin, journal `Frozen`.
async fn freeze_job(
    access: &ConfigAccess,
    request: &JobRequest,
    policy: &PolicyV2,
) -> Result<JobRecord> {
    check_policy(request, policy)?;
    let owner = request.owner.as_str();
    let definition = load_definition(access, owner, &request.definition_id).await?;
    definition
        .validate()
        .map_err(|error| refused(format!("eval definition: {error:#}")))?;

    let source = materialize_pack(&request.baseline_pack, owner, &request.behavior_id)?;
    let baseline_path = baseline_dir(&request.jobs_dir, &request.job_id);
    // A directory with no job row is the leftover of a freeze that never
    // finished; the job does not exist, so nothing refers to it.
    remove_if_present(&baseline_path)?;
    let baseline = materialize_candidate(&source, owner, &baseline_text(&source)?, &baseline_path)?;

    let closure = read_closure(access, owner).await?;
    let target = Target {
        field: TargetField::AgentContextSystemPrompt,
        owner: request.owner.clone(),
        id: baseline.context_id.clone(),
    };
    let live_text = current_text(&closure, &target)?;
    let pack_text = baseline_text(&baseline)?;
    if live_text != pack_text {
        return Err(refused(format!(
            "the live AgentContext {:?} and the baseline pack disagree about the subject's system prompt; supply a pack exported from this configuration",
            target.id
        )));
    }

    let origin = JobOrigin {
        target,
        closure: closure_digests(&closure)?,
        subject: SubjectRef {
            pack_digest: baseline.digest.clone(),
            behavior_id: request.behavior_id.clone(),
        },
        definition: definition_ref(&definition)?,
        policy: policy.clone(),
        trials_per_case: request.trials_per_case,
        budgets: request.budgets.clone(),
        baseline_text: pack_text,
        owner: request.owner.clone(),
        seed_base: request.seed_base,
        inference_profile_id: request.inference_profile_id.clone(),
        max_text_bytes: request.max_text_bytes,
        jobs_dir: request.jobs_dir.clone(),
    };
    let mut job = create_job(access, &request.job_id, owner, &origin).await?;
    append(access, &mut job, JournalEntry::Frozen).await?;
    Ok(job)
}

/// Start or resume a job, run rounds, and finalize.
///
/// Calling this again on the same `job_id` resumes: the journal is replayed,
/// an unfinished run is resumed through the runner, and a proposed round is
/// never proposed again. A resumed job and an uninterrupted one reach the same
/// journal.
#[allow(clippy::too_many_arguments)]
pub async fn run_job(
    access: &ConfigAccess,
    request: &JobRequest,
    executor: &dyn TrialExecutor,
    proposer: &dyn Proposer,
    registry: &CheckRegistry,
    policy: &PolicyV2,
    cancel: CancellationToken,
) -> Result<JobOutcome> {
    let mut job = match load_job(access, &request.owner, &request.job_id).await? {
        Some(existing) => {
            check_resume(request, policy, &existing.origin)?;
            existing
        }
        None => freeze_job(access, request, policy).await?,
    };
    // From here on, only the origin decides (finding F2).
    let origin = job.origin.clone();
    let owner = origin.owner.as_str();
    let job_id = job.job_id.clone();

    let state = derive_state(&job.journal);
    if state != JobState::Running {
        return Ok(outcome(&job, state));
    }

    // Finding F5: the instrument first, then the subject.
    let definition = load_definition(access, owner, &origin.definition.definition_id).await?;
    if definition_ref(&definition)? != origin.definition {
        return finalize(access, &mut job, JobState::Failed {
            reason: "definition_changed".into(),
        })
        .await;
    }
    if closure_digests(&read_closure(access, owner).await?)? != origin.closure {
        return finalize(access, &mut job, JobState::Failed {
            reason: "baseline_drifted".into(),
        })
        .await;
    }
    let baseline_path = baseline_dir(&origin.jobs_dir, &job_id);
    let baseline = materialize_pack(&baseline_path, owner, &origin.subject.behavior_id)?;
    if baseline.digest != origin.subject.pack_digest {
        return Err(refused(format!(
            "the job's baseline copy at {} no longer digests to what it froze",
            baseline_path.display()
        )));
    }

    let policy = &origin.policy;
    let budgets = &origin.budgets;
    let held_out_reserve = run_cost(&definition, EvalSplit::HeldOut, origin.trials_per_case, 2);
    let train_cost = run_cost(&definition, EvalSplit::Train, origin.trials_per_case, 1);
    let validation_cost = run_cost(&definition, EvalSplit::Validation, origin.trials_per_case, 2);
    // Cancellation stops after the run in progress; the job stays `Running`
    // and the next call resumes it (ruling R9).
    let stopped = |job: &JobRecord| -> Result<JobOutcome> { Ok(outcome(job, JobState::Running)) };

    for round in 1..=budgets.max_rounds {
        if budget_exhausted(&job.journal) {
            break;
        }
        if round_is_closed(&job.journal, round) {
            continue;
        }
        let retained = checkpoint(&job.journal);
        let checkpoint_path = retained
            .as_ref()
            .map(|held| candidate_dir(&origin.jobs_dir, &job_id, held.round))
            .unwrap_or_else(|| baseline_path.clone());
        let checkpoint_text = retained
            .as_ref()
            .map(|held| held.text.clone())
            .unwrap_or_else(|| origin.baseline_text.clone());

        // A round starts only if it can finish beside the reserved held-out run.
        let spend = spend_so_far(access, owner, &job.journal).await?;
        if spend.case_trials + train_cost + validation_cost + held_out_reserve
            > budgets.max_case_trials
            || spend.tokens > budgets.max_tokens
            || past_deadline(budgets)
        {
            append(
                access,
                &mut job,
                JournalEntry::BudgetExhausted {
                    round: Some(round),
                    reason: "no budget for another round beside the reserved held-out run".into(),
                },
            )
            .await?;
            break;
        }

        // 1. Train run: one cell, the checkpoint, on the train split.
        let train = train_plan(&job_id, &origin, round, &checkpoint_path);
        if !run_started(&job.journal, &train.run_id) {
            let entry = JournalEntry::RunStarted {
                run_id: train.run_id.clone(),
                round: Some(round),
                split: EvalSplit::Train,
            };
            append(access, &mut job, entry).await?;
        }
        execute_run(access, request, &origin, &train, executor, registry, cancel.clone()).await?;
        if cancel.is_cancelled() {
            return stopped(&job);
        }

        // 2. Propose. Feedback comes from this run's verdicts and nowhere else.
        let (text, digest) = match proposed_for(&job.journal, round) {
            Some((text, _, digest)) => (text, digest),
            None => {
                // Finding F7: a directory with no `Proposed` is a leftover.
                remove_if_present(&candidate_staging_dir(&origin.jobs_dir, &job_id, round))?;
                remove_if_present(&candidate_dir(&origin.jobs_dir, &job_id, round))?;
                let feedback = train_feedback(&load_verdicts(access, owner, &train.run_id).await?);
                let proposal = proposer
                    .propose(ProposalInput {
                        round,
                        current_text: checkpoint_text.clone(),
                        feedback,
                        rejections: rejections(&job.journal),
                        max_text_bytes: origin.max_text_bytes,
                    })
                    .await?;
                let staged = materialize_candidate(
                    &baseline,
                    owner,
                    &proposal.text,
                    &candidate_staging_dir(&origin.jobs_dir, &job_id, round),
                )?;
                let entry = JournalEntry::Proposed {
                    round,
                    text: proposal.text.clone(),
                    rationale: proposal.rationale.clone(),
                    candidate_digest: staged.digest.clone(),
                };
                append(access, &mut job, entry).await?;
                (proposal.text, staged.digest)
            }
        };
        let candidate = proposed_candidate(&origin, &job_id, round, &digest)?;

        // 3. Structural gate, before any validation spend.
        if let Err(rejection) = structural_gate(
            &baseline,
            &candidate,
            &text,
            origin.max_text_bytes,
            &seen_digests(&baseline.digest, &job.journal, round),
            owner,
        ) {
            tracing::info!(round, reason = rejection.reason, "candidate rejected before any spend");
            let entry = JournalEntry::StructuralReject {
                round,
                diagnostics: rejection.diagnostics(),
            };
            append(access, &mut job, entry).await?;
            continue;
        }

        // 4. Validation runs, and 5. the decision. A re-run adds pairs and
        // never replaces them (finding F1).
        let mut run_ids = Vec::new();
        for attempt in 0..=policy.max_reruns {
            let plan = validation_plan(
                &job_id,
                &origin,
                round,
                attempt,
                &checkpoint_path,
                &candidate.dir,
            );
            if !run_started(&job.journal, &plan.run_id) {
                let spend = spend_so_far(access, owner, &job.journal).await?;
                if spend.case_trials + validation_cost + held_out_reserve > budgets.max_case_trials
                {
                    let entry = JournalEntry::BudgetExhausted {
                        round: Some(round),
                        reason: "no budget for another validation run".into(),
                    };
                    append(access, &mut job, entry).await?;
                    break;
                }
                let entry = JournalEntry::RunStarted {
                    run_id: plan.run_id.clone(),
                    round: Some(round),
                    split: EvalSplit::Validation,
                };
                append(access, &mut job, entry).await?;
            }
            execute_run(access, request, &origin, &plan, executor, registry, cancel.clone())
                .await?;
            if cancel.is_cancelled() {
                return stopped(&job);
            }
            run_ids.push(plan.run_id.clone());

            let mut runs = Vec::with_capacity(run_ids.len());
            for run_id in &run_ids {
                runs.push(load_run_rows(access, owner, run_id).await?);
            }
            let evidence = decision_evidence(&definition, &runs, policy.max_missing_usage_bp);
            let report = decide(Mode::Improve, policy, &evidence, decision_seed(&run_ids));
            if matches!(report.decision, Decision::Inconclusive(_)) && attempt < policy.max_reruns {
                continue;
            }
            let entry = JournalEntry::Decided {
                round: Some(round),
                attempt,
                run_ids: run_ids.clone(),
                mode: Mode::Improve,
                decision: report.decision,
                policy_version: report.policy_version.clone(),
                summary: summary_of(&report),
            };
            append(access, &mut job, entry).await?;
            break;
        }
    }

    // 6. Finalize. The held-out split is touched once, and only when a round
    // moved the checkpoint off the baseline.
    let Some(retained) = checkpoint(&job.journal) else {
        let state = if budget_exhausted(&job.journal) {
            JobState::Exhausted
        } else {
            JobState::NothingToPromote
        };
        return finalize(access, &mut job, state).await;
    };
    let plan = held_out_plan(
        &job_id,
        &origin,
        &baseline_path,
        &candidate_dir(&origin.jobs_dir, &job_id, retained.round),
    );
    let decision = match decided_held_out(&job.journal) {
        Some(decision) => decision,
        None => {
            if !run_started(&job.journal, &plan.run_id) {
                let entry = JournalEntry::RunStarted {
                    run_id: plan.run_id.clone(),
                    round: None,
                    split: EvalSplit::HeldOut,
                };
                append(access, &mut job, entry).await?;
            }
            execute_run(access, request, &origin, &plan, executor, registry, cancel.clone())
                .await?;
            if cancel.is_cancelled() {
                return stopped(&job);
            }
            let run_ids = vec![plan.run_id.clone()];
            let runs = vec![load_run_rows(access, owner, &plan.run_id).await?];
            let evidence = decision_evidence(&definition, &runs, policy.max_missing_usage_bp);
            let report = decide(Mode::Confirm, policy, &evidence, decision_seed(&run_ids));
            let entry = JournalEntry::Decided {
                round: None,
                attempt: 0,
                run_ids,
                mode: Mode::Confirm,
                decision: report.decision,
                policy_version: report.policy_version.clone(),
                summary: summary_of(&report),
            };
            append(access, &mut job, entry).await?;
            report.decision
        }
    };
    let state = match decision {
        Decision::Accept => JobState::ReadyToPromote,
        Decision::Reject(_) => JobState::Failed {
            reason: "held_out_regression".into(),
        },
        Decision::Inconclusive(_) => JobState::Failed {
            reason: "held_out_inconclusive".into(),
        },
    };
    finalize(access, &mut job, state).await
}

fn past_deadline(budgets: &Budgets) -> bool {
    budgets
        .deadline_unix_secs
        .is_some_and(|deadline| now_unix_secs() > deadline)
}

fn decided_held_out(journal: &[JournalEntry]) -> Option<Decision> {
    journal.iter().find_map(|entry| match entry {
        JournalEntry::Decided {
            round: None,
            mode: Mode::Confirm,
            decision,
            ..
        } => Some(*decision),
        _ => None,
    })
}

async fn finalize(access: &ConfigAccess, job: &mut JobRecord, state: JobState) -> Result<JobOutcome> {
    tracing::info!(job_id = %job.job_id, state = state.label(), "optimization job finalized");
    let entry = JournalEntry::Finalized {
        state: state.clone(),
    };
    append(access, job, entry).await?;
    Ok(outcome(job, state))
}

fn outcome(job: &JobRecord, state: JobState) -> JobOutcome {
    JobOutcome {
        job_id: job.job_id.clone(),
        state,
        checkpoint: checkpoint(&job.journal),
        rounds_used: rounds_used(&job.journal),
    }
}
```

Extend the file's `use` block with what this half needs, in rustfmt order:

```rust
use crate::eval::report::load_run_rows;
use crate::eval::{load_verdicts, SubjectRef};
use crate::optimization::evidence::{decision_evidence, decision_seed, train_feedback};
use crate::optimization::gate::structural_gate;
use crate::optimization::job::{
    append, checkpoint, create_job, derive_state, load_job, rounds_used, Checkpoint,
    DecisionSummary, JobRecord, JobState,
};
use crate::optimization::policy::{decide, Decision, DecisionReport, Mode, PolicyV2};
use crate::optimization::proposer::{ProposalInput, Proposer, Rejection};
use crate::optimization::subject::{
    baseline_text, materialize_candidate, materialize_pack, MaterializedPack,
};
use crate::optimization::target::{
    capture_closure, closure_digests, current_text, Closure, Target, TargetField,
};
```

- [ ] **Step 4: Wire the module** — in `crates/gents/src/optimization/mod.rs` extend the driver re-export:

```rust
pub use driver::{
    baseline_dir, candidate_dir, job_dir, job_refused, run_cost, run_job, spend_so_far,
    split_case_count, JobOutcome, JobRefused, JobRequest, Spend,
};
```

- [ ] **Step 5: Run the tests**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents --lib optimization::driver`
Expected: PASS, 9 tests.

- [ ] **Step 6: Commit**

```bash
git add crates/gents/src/optimization
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" \
  commit -m "feat(optimization): run_job drives rounds from the frozen origin (#1455)

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

### Task 5: The scripted matrix

**Files:**
- Create: `crates/gents/src/optimization/driver/matrix.rs`
- Modify: `crates/gents/src/optimization/driver.rs` (add `#[cfg(test)] pub(crate) mod matrix;` near the top, in rustfmt order)

Every end-to-end case spec section 9 lists runs here, deterministically, in `cargo test -p gents` (finding F9): accept; reject for `no_improvement`, `case_regression` and `cost_regression`; structural reject with zero validation runs; inconclusive, re-run, inconclusive; too few cases; budget exhaustion; held-out regression; held-out requested only at finalize; feedback only from the train run; baseline drift; definition drift; resume after interruption against an uninterrupted twin. The last §9 case, a decision on an invalidated run, needs `show` and is Task 6's.

The matrix uses the policy's defaults with `max_rounds: 3` (finding F11): `alpha_effective` is 50000 / 3 = 16666 ppm, and six aligned validation cases give p = 1/64 = 15625 ppm, which is inside it. Every scripted proposer repeats one text for all three rounds, so rounds 2 and 3 are structural duplicates and spend only their train runs.

**Interfaces:**
- Consumes:
  ```rust
  // crates/gents/src/eval/runner/freeze.rs, pub(crate) mod tests
  pub(crate) const OWNER: &str = "did:key:eval-owner";
  pub(crate) struct Launching { pub(crate) access: ConfigAccess, /* private */ }
  impl Launching {
      pub(crate) async fn new() -> Self;
      pub(crate) async fn install(&self, documents: Vec<(Collection, Value)>);
      pub(crate) fn runs_dir(&self) -> PathBuf;          // <scratch>/eval/runs
      pub(crate) fn evaluator_did(&self) -> String;
      pub(crate) fn pack(&self, name: &str, bash_mode: &str) -> PathBuf;
  }
  // crates/gents/src/eval/runner/scripted.rs
  pub struct ScriptKey { pub cell_label: String, pub case_id: String, pub trial_index: u32, pub attempt: u32 }
  impl ScriptedExecutor {
      pub fn new() -> Self;
      pub fn with(self, key: ScriptKey, evidence: TrialEvidence) -> Self;
      pub fn with_default(self, evidence: TrialEvidence) -> Self;
      pub fn passed_evidence(did: &str, stage_id: &str, capture_name: &str, rows: Vec<Value>) -> TrialEvidence;
      pub fn not_evidence(did: &str) -> TrialEvidence;
  }
  // crates/gents/src/eval/runner/executor.rs
  impl TrialEvidence { pub fn new(locator: TrialLocator, stages: Vec<StageEvidence>, usage: TrialUsage, anchor: Anchor) -> Self; }
  // crates/gents/src/eval/checks/mod.rs
  pub trait Check: Send + Sync { fn name(&self) -> &'static str; fn version(&self) -> &'static str;
      fn evaluate(&self, params: &Value, stage: &StageEvidence) -> CheckVerdict; }
  impl CheckRegistry { pub fn builtin() -> Self; #[cfg(test)] pub(crate) fn with(self, check: Box<dyn Check>) -> Self; }
  // crates/gents/src/eval/documents.rs
  pub async fn load_verdicts(access: &ConfigAccess, owner: &str, run_id: &str) -> Result<Vec<VerdictRecord>>;
  ```
  Facts this matrix relies on, each read from the code: a slot's first attempt is `1` (`eval/runner/plan.rs`, `highest.map_or(1, …)`); the runner fills `ScriptKey::cell_label` from `CellRequest::label`, which `run_request` sets equal to the cell id; `ScriptKey` carries no run id, so one script answers every run of a job; a pre-cancelled token makes the runner skip every queued slot without writing a trial row (`runner/mod.rs`, the `biased` select that sets `stop`); `max_infra_retries: 0` never plans a slot twice; and `Launching::pack` writes a sidecar prompt of `"Watch the mailbox.\n"` (`freeze.rs`, `fn write_fixture_pack`).
- Produces (`pub(crate)`, consumed by Task 6 and PR 4):
  ```rust
  pub(crate) const VALIDATION_CASES: [&str; 6];
  pub(crate) const HELD_OUT_CASES: [&str; 6];
  pub(crate) const BASELINE_PROMPT: &str;
  pub(crate) const CANDIDATE_PROMPT: &str;
  pub(crate) struct Harness;
  impl Harness {
      pub(crate) async fn new() -> Self;
      pub(crate) fn access(&self) -> &ConfigAccess;
      pub(crate) async fn install(&self, documents: Vec<(Collection, Value)>);
      pub(crate) fn request(&self, job_id: &str, definition_id: &str, budgets: Budgets) -> JobRequest;
  }
  pub(crate) fn pass() -> TrialEvidence;
  pub(crate) fn fail() -> TrialEvidence;
  pub(crate) fn script(executor: ScriptedExecutor, cell: &str, cases: &[&str],
      evidence: impl Fn(&str) -> TrialEvidence) -> ScriptedExecutor;
  pub(crate) fn base_executor() -> ScriptedExecutor;
  pub(crate) fn budgets(max_case_trials: u64) -> Budgets;
  pub(crate) fn policy() -> PolicyV2;
  pub(crate) fn repeating_proposer(text: &str) -> ScriptedProposer;
  pub(crate) async fn drive(harness: &Harness, request: &JobRequest, executor: &ScriptedExecutor,
      proposer: &dyn Proposer, registry: &CheckRegistry, cancel: CancellationToken) -> Result<JobOutcome>;
  pub(crate) async fn settle(harness: &Harness, request: &JobRequest, executor: &ScriptedExecutor,
      proposer: &dyn Proposer) -> JobOutcome;
  pub(crate) async fn journal(harness: &Harness, job_id: &str) -> Vec<JournalEntry>;
  pub(crate) async fn accepting_harness(job_id: &str) -> (Harness, JobRequest);
  pub(crate) async fn rejecting_harness(job_id: &str) -> (Harness, JobRequest);
  ```

- [ ] **Step 1: Write the matrix** — create `crates/gents/src/optimization/driver/matrix.rs`:

```rust
//! The scripted end-to-end matrix: no model, no network.
//!
//! Each case assembles a job the way `tests/eval_runner_canary.rs` assembles a
//! run — a launching home, an installed definition and inference documents, a
//! fixture pack as the subject — and replaces only the provider, with the
//! runner's `ScriptedExecutor`. Nothing here mocks the driver, the policy or
//! the journal: a failure is a defect in the stack.
//!
//! Scoring is binary by construction. `captured_rows_count` with `min: 1`
//! scores 10000 on a stage whose `findings` capture holds a row and 0 on one
//! that holds none, so a scripted arm's per-case difference is exactly
//! +10000, 0 or -10000 and every expected decision is arithmetic.

use std::path::PathBuf;

use anyhow::Result;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::config_client::ConfigAccess;
use crate::document_config::EvalSplit;
use crate::eval::checks::{Check, CheckRegistry, CheckVerdict};
use crate::eval::report::load_run_rows;
use crate::eval::runner::freeze::tests::{Launching, OWNER};
use crate::eval::runner::{
    RunOptions, ScriptKey, ScriptedExecutor, StageEvidence, TrialEvidence,
};
use crate::eval::{load_verdicts, OutcomeKind, TrialUsage};
use crate::optimization::driver::{run_job, spend_so_far, JobOutcome, JobRequest};
use crate::optimization::evidence::decision_evidence;
use crate::optimization::job::{load_job, Budgets, JobState, JournalEntry};
use crate::optimization::policy::{Decision, InconclusiveReason, PolicyV2, RejectReason};
use crate::optimization::proposer::{Proposal, ProposalInput, Proposer, ScriptedProposer};
use crate::Collection;

pub(crate) const VALIDATION_CASES: [&str; 6] = ["val-a", "val-b", "val-c", "val-d", "val-e", "val-f"];
pub(crate) const HELD_OUT_CASES: [&str; 6] = ["ho-a", "ho-b", "ho-c", "ho-d", "ho-e", "ho-f"];
const TRAIN_CASES: [&str; 1] = ["train-a"];
const TRIALS_PER_CASE: u32 = 2;
pub(crate) const BASELINE_PROMPT: &str = "Watch the mailbox.\n";
pub(crate) const CANDIDATE_PROMPT: &str = "Watch the mailbox, and name the collection.\n";

const DEFINITION: &str = "monitor-findings";
/// Five validation cases: `2^-5` is 31250 ppm, past 16666, so nothing can pass.
const SMALL_DEFINITION: &str = "monitor-small";
/// Every check is `feedback_check`, which always says something.
const FEEDBACK_DEFINITION: &str = "monitor-feedback";
const FEEDBACK: &str = "name the collection";

fn case(case_id: &str, split: &str, check: &str) -> Value {
    let params = if check == "captured_rows_count" {
        json!({"name": "findings", "min": 1})
    } else {
        json!({})
    };
    json!({
        "case_id": case_id,
        "split": split,
        "stages": [{
            "stage_id": "check",
            "prompt": "Run the monitor.",
            "deadline_secs": 600,
            "checks": [{"check": check, "params": params, "tier": "acceptance"}],
        }],
    })
}

fn definition(definition_id: &str, check: &str, validation: &[&str], version: i64) -> Value {
    let mut cases: Vec<Value> = TRAIN_CASES.iter().map(|id| case(id, "train", check)).collect();
    cases.extend(validation.iter().map(|id| case(id, "validation", check)));
    cases.extend(HELD_OUT_CASES.iter().map(|id| case(id, "held_out", check)));
    json!({
        "definition_id": definition_id,
        "agent_did": OWNER,
        "comparability_version": version,
        "subject": {"kind": "behavior", "inference_slots": ["primary"]},
        "cases": cases,
    })
}

/// The live configuration the job freezes and, in PR 4, promotes into.
fn behavior(display_name: &str) -> (Collection, Value) {
    (
        Collection::AgentBehavior,
        json!({
            "behavior_id": "monitor",
            "agent_did": OWNER,
            "display_name": display_name,
            "context_id": "monitor-context",
            "inference_profile_id": "local",
        }),
    )
}

fn context(prompt: &str) -> (Collection, Value) {
    (
        Collection::AgentContext,
        json!({
            "context_id": "monitor-context",
            "agent_did": OWNER,
            "display_name": "Monitor",
            "system_prompt": prompt,
        }),
    )
}

/// A stage whose `findings` capture holds one row: the check passes at 10000.
pub(crate) fn pass() -> TrialEvidence {
    ScriptedExecutor::passed_evidence("did:key:trial", "check", "findings", vec![json!({})])
}

/// A stage whose `findings` capture holds nothing: `below_min`, scored 0.
pub(crate) fn fail() -> TrialEvidence {
    ScriptedExecutor::passed_evidence("did:key:trial", "check", "findings", Vec::new())
}

/// The same evidence, reporting `tokens` of usage.
fn with_usage(evidence: TrialEvidence, tokens: u64) -> TrialEvidence {
    TrialEvidence::new(
        evidence.locator,
        evidence.stages,
        TrialUsage {
            input_tokens: Some(tokens),
            output_tokens: Some(0),
        },
        evidence.anchor,
    )
}

/// Script one cell's answer for every trial of `cases`, first attempt.
pub(crate) fn script(
    mut executor: ScriptedExecutor,
    cell: &str,
    cases: &[&str],
    evidence: impl Fn(&str) -> TrialEvidence,
) -> ScriptedExecutor {
    for case_id in cases {
        for trial_index in 0..TRIALS_PER_CASE {
            let key = ScriptKey {
                cell_label: cell.into(),
                case_id: (*case_id).into(),
                trial_index,
                attempt: 1,
            };
            executor = executor.with(key, evidence(case_id));
        }
    }
    executor
}

/// Every cell passes everywhere except where a test overrides it.
pub(crate) fn base_executor() -> ScriptedExecutor {
    ScriptedExecutor::new().with_default(pass())
}

pub(crate) fn budgets(max_case_trials: u64) -> Budgets {
    Budgets {
        max_rounds: 3,
        max_case_trials,
        max_tokens: u64::MAX,
        deadline_unix_secs: None,
    }
}

/// The uncalibrated defaults: `max_rounds: 3`, `max_reruns: 1`,
/// `alpha_ppm: 50000`, so `alpha_effective` is 16666 ppm.
pub(crate) fn policy() -> PolicyV2 {
    PolicyV2::uncalibrated()
}

/// The same text for all three rounds: rounds 2 and 3 are structural
/// duplicates of round 1.
pub(crate) fn repeating_proposer(text: &str) -> ScriptedProposer {
    ScriptedProposer::new(vec![
        (text.to_owned(), "name the collection the monitor should read".to_owned());
        3
    ])
}

/// Stands in for a proposer whose model is down.
struct ErroringProposer;

#[async_trait::async_trait]
impl Proposer for ErroringProposer {
    async fn propose(&self, _input: ProposalInput) -> Result<Proposal> {
        Err(anyhow::anyhow!("proposer unavailable"))
    }
}

/// Always passes, always has advice.
struct FeedbackCheck;

impl Check for FeedbackCheck {
    fn name(&self) -> &'static str {
        "feedback_check"
    }

    fn version(&self) -> &'static str {
        "1"
    }

    fn evaluate(&self, _params: &Value, _stage: &StageEvidence) -> CheckVerdict {
        CheckVerdict {
            kind: OutcomeKind::Passed,
            score_bp: Some(10_000),
            raw: json!({"reason_code": "ok"}),
            feedback: Some(FEEDBACK.into()),
        }
    }
}

pub(crate) struct Harness {
    launching: Launching,
    jobs_dir: PathBuf,
    pack: PathBuf,
}

impl Harness {
    pub(crate) async fn new() -> Self {
        let launching = Launching::new().await;
        launching
            .install(vec![
                (
                    Collection::EvalDefinition,
                    definition(DEFINITION, "captured_rows_count", &VALIDATION_CASES, 1),
                ),
                (
                    Collection::EvalDefinition,
                    definition(SMALL_DEFINITION, "captured_rows_count", &VALIDATION_CASES[..5], 1),
                ),
                (
                    Collection::EvalDefinition,
                    definition(FEEDBACK_DEFINITION, "feedback_check", &VALIDATION_CASES, 1),
                ),
                context(BASELINE_PROMPT),
                behavior("Monitor"),
            ])
            .await;
        let pack = launching.pack("subject", "Off");
        // `<launching home>/eval/jobs`, beside `eval/runs` (ruling R8).
        let jobs_dir = launching
            .runs_dir()
            .parent()
            .expect("runs_dir is <scratch>/eval/runs")
            .join("jobs");
        Self {
            launching,
            jobs_dir,
            pack,
        }
    }

    pub(crate) fn access(&self) -> &ConfigAccess {
        &self.launching.access
    }

    pub(crate) async fn install(&self, documents: Vec<(Collection, Value)>) {
        self.launching.install(documents).await;
    }

    pub(crate) fn request(&self, job_id: &str, definition_id: &str, budgets: Budgets) -> JobRequest {
        JobRequest {
            job_id: job_id.into(),
            owner: OWNER.into(),
            evaluator_did: self.launching.evaluator_did(),
            behavior_id: "monitor".into(),
            definition_id: definition_id.into(),
            inference_profile_id: "local".into(),
            baseline_pack: self.pack.clone(),
            trials_per_case: TRIALS_PER_CASE,
            budgets,
            max_text_bytes: 32 * 1024,
            seed_base: 1_000,
            jobs_dir: self.jobs_dir.clone(),
            runs_dir: self.launching.runs_dir(),
            source_commit: "matrix".into(),
            source_dirty: false,
            concurrency: 1,
            max_infra_retries: 0,
            breaker_threshold: 100,
            deadline_secs: Some(600),
            run_options: RunOptions {
                poll_backoff_base: std::time::Duration::from_millis(1),
                poll_backoff_cap: std::time::Duration::from_millis(2),
            },
        }
    }
}

pub(crate) async fn drive(
    harness: &Harness,
    request: &JobRequest,
    executor: &ScriptedExecutor,
    proposer: &dyn Proposer,
    registry: &CheckRegistry,
    cancel: CancellationToken,
) -> Result<JobOutcome> {
    run_job(harness.access(), request, executor, proposer, registry, &policy(), cancel).await
}

/// Drive a job to its end with the builtin checks.
pub(crate) async fn settle(
    harness: &Harness,
    request: &JobRequest,
    executor: &ScriptedExecutor,
    proposer: &dyn Proposer,
) -> JobOutcome {
    drive(
        harness,
        request,
        executor,
        proposer,
        &CheckRegistry::builtin(),
        CancellationToken::new(),
    )
    .await
    .unwrap()
}

pub(crate) async fn journal(harness: &Harness, job_id: &str) -> Vec<JournalEntry> {
    load_job(harness.access(), OWNER, job_id)
        .await
        .unwrap()
        .unwrap_or_else(|| panic!("no job {job_id}"))
        .journal
}

fn runs_started(journal: &[JournalEntry], split: EvalSplit) -> Vec<String> {
    journal
        .iter()
        .filter_map(|entry| match entry {
            JournalEntry::RunStarted { run_id, split: started, .. } if *started == split => {
                Some(run_id.clone())
            }
            _ => None,
        })
        .collect()
}

fn decisions(journal: &[JournalEntry]) -> Vec<(Option<u32>, Decision)> {
    journal
        .iter()
        .filter_map(|entry| match entry {
            JournalEntry::Decided { round, decision, .. } => Some((*round, *decision)),
            _ => None,
        })
        .collect()
}

fn structural_rejects(journal: &[JournalEntry]) -> usize {
    journal
        .iter()
        .filter(|entry| matches!(entry, JournalEntry::StructuralReject { .. }))
        .count()
}

/// A harness whose job accepted a candidate and reached `ReadyToPromote`:
/// the baseline fails every validation case and the candidate passes them.
pub(crate) async fn accepting_harness(job_id: &str) -> (Harness, JobRequest) {
    let harness = Harness::new().await;
    let executor = script(base_executor(), "baseline", &VALIDATION_CASES, |_| fail());
    let request = harness.request(job_id, DEFINITION, budgets(1_000));
    let outcome = settle(&harness, &request, &executor, &repeating_proposer(CANDIDATE_PROMPT)).await;
    assert_eq!(outcome.state, JobState::ReadyToPromote, "{outcome:#?}");
    (harness, request)
}

/// A harness whose job finished without accepting anything: three cases
/// improve and three tie, so p = 8/64 = 125000 ppm.
pub(crate) async fn rejecting_harness(job_id: &str) -> (Harness, JobRequest) {
    let harness = Harness::new().await;
    let executor = script(base_executor(), "baseline", &VALIDATION_CASES[..3], |_| fail());
    let request = harness.request(job_id, DEFINITION, budgets(1_000));
    let outcome = settle(&harness, &request, &executor, &repeating_proposer(CANDIDATE_PROMPT)).await;
    assert_eq!(outcome.state, JobState::NothingToPromote, "{outcome:#?}");
    (harness, request)
}

#[tokio::test]
async fn a_candidate_that_wins_every_case_is_accepted_and_reaches_ready_to_promote() {
    let (harness, request) = accepting_harness("accept").await;
    let journal = journal(&harness, &request.job_id).await;
    assert_eq!(
        decisions(&journal),
        vec![(Some(1), Decision::Accept), (None, Decision::Accept)],
        "one validation decision, one held-out confirmation: {journal:#?}"
    );
    assert_eq!(structural_rejects(&journal), 2, "rounds 2 and 3 repeat round 1's text");
    assert_eq!(
        runs_started(&journal, EvalSplit::HeldOut),
        vec!["accept-held-out".to_owned()],
        "the held-out split is run exactly once, at finalize"
    );
    let retained = crate::optimization::checkpoint(&journal).expect("round 1 was accepted");
    assert_eq!((retained.round, retained.text.as_str()), (1, CANDIDATE_PROMPT));
}

#[tokio::test]
async fn a_candidate_that_wins_only_half_the_cases_is_rejected_for_no_improvement() {
    let (harness, request) = rejecting_harness("no-improvement").await;
    let journal = journal(&harness, &request.job_id).await;
    assert_eq!(
        decisions(&journal),
        vec![(Some(1), Decision::Reject(RejectReason::NoImprovement))]
    );
    assert!(
        runs_started(&journal, EvalSplit::HeldOut).is_empty(),
        "the held-out split is never touched when no round accepted"
    );
}

#[tokio::test]
async fn one_broken_case_rejects_the_candidate_even_though_the_mean_improves() {
    let harness = Harness::new().await;
    // Five cases improve by +10000; `val-a` regresses by -10000, past the
    // 5000 bp per-case tolerance.
    let mut executor = script(base_executor(), "baseline", &VALIDATION_CASES[1..], |_| fail());
    executor = script(executor, "candidate", &VALIDATION_CASES[..1], |_| fail());
    let request = harness.request("case-regression", DEFINITION, budgets(1_000));
    let outcome = settle(&harness, &request, &executor, &repeating_proposer(CANDIDATE_PROMPT)).await;
    assert_eq!(outcome.state, JobState::NothingToPromote);
    assert_eq!(
        decisions(&journal(&harness, "case-regression").await),
        vec![(Some(1), Decision::Reject(RejectReason::CaseRegression))]
    );
}

#[tokio::test]
async fn a_candidate_that_costs_ten_times_the_tokens_is_rejected_for_cost() {
    let harness = Harness::new().await;
    // The candidate wins every case but spends 1000 tokens a trial against the
    // baseline's 100, past the 25% ceiling.
    let mut executor = script(base_executor(), "baseline", &VALIDATION_CASES, |_| {
        with_usage(fail(), 100)
    });
    executor = script(executor, "candidate", &VALIDATION_CASES, |_| {
        with_usage(pass(), 1_000)
    });
    let request = harness.request("cost-regression", DEFINITION, budgets(1_000));
    let outcome = settle(&harness, &request, &executor, &repeating_proposer(CANDIDATE_PROMPT)).await;
    assert_eq!(outcome.state, JobState::NothingToPromote);
    let journal = journal(&harness, "cost-regression").await;
    assert_eq!(
        decisions(&journal),
        vec![(Some(1), Decision::Reject(RejectReason::CostRegression))]
    );
    assert!(journal.iter().any(|entry| matches!(
        entry,
        JournalEntry::Decided { summary, .. } if !summary.cost_skipped
    )));
}

#[tokio::test]
async fn a_repeated_candidate_is_structurally_rejected_and_costs_no_validation_run() {
    let harness = Harness::new().await;
    let executor = script(base_executor(), "baseline", &VALIDATION_CASES, |_| fail());
    let request = harness.request("structural", DEFINITION, budgets(1_000));
    // The checkpoint's own text materializes to the checkpoint's own digest.
    let outcome = settle(&harness, &request, &executor, &ScriptedProposer::echoing()).await;
    assert_eq!(outcome.state, JobState::NothingToPromote);
    let journal = journal(&harness, "structural").await;
    assert_eq!(structural_rejects(&journal), 3);
    assert!(journal.iter().any(|entry| matches!(
        entry,
        JournalEntry::StructuralReject { round: 1, diagnostics } if diagnostics.starts_with("duplicate_candidate")
    )));
    assert!(
        runs_started(&journal, EvalSplit::Validation).is_empty(),
        "a structural rejection spends no validation run"
    );
    assert!(decisions(&journal).is_empty(), "and reaches no decision");
}

#[tokio::test]
async fn a_round_that_cannot_be_afforded_is_budget_exhausted_and_spends_nothing() {
    let harness = Harness::new().await;
    let executor = script(base_executor(), "baseline", &VALIDATION_CASES, |_| fail());
    // A round costs its train run (1 case x 2 trials x 1 cell = 2) and its
    // validation run (6 x 2 x 2 = 24), and 24 more are reserved for the
    // held-out run: 50 > 25, so round 1 is exhausted before it spends anything.
    let request = harness.request("exhausted", DEFINITION, budgets(25));
    let outcome = settle(&harness, &request, &executor, &repeating_proposer(CANDIDATE_PROMPT)).await;
    assert_eq!(outcome.state, JobState::Exhausted);
    let journal = journal(&harness, "exhausted").await;
    assert!(journal
        .iter()
        .any(|entry| matches!(entry, JournalEntry::BudgetExhausted { round: Some(1), .. })));
    assert!(decisions(&journal).is_empty(), "exhaustion is not a rejection");
    assert!(!journal
        .iter()
        .any(|entry| matches!(entry, JournalEntry::RunStarted { .. })));
}

#[tokio::test]
async fn an_inconclusive_round_is_rerun_on_a_new_seed_and_ends_inconclusive() {
    let harness = Harness::new().await;
    // The candidate produces no evidence on two cases: 4 of 12 keys dropped on
    // one side is past both the 20% not-evidence and the 10% asymmetry limits.
    let executor = script(base_executor(), "candidate", &VALIDATION_CASES[..2], |_| {
        ScriptedExecutor::not_evidence("did:key:trial")
    });
    let request = harness.request("inconclusive", DEFINITION, budgets(1_000));
    let outcome = settle(&harness, &request, &executor, &repeating_proposer(CANDIDATE_PROMPT)).await;
    assert_eq!(outcome.state, JobState::NothingToPromote);

    let journal = journal(&harness, "inconclusive").await;
    let decided = journal
        .iter()
        .find_map(|entry| match entry {
            JournalEntry::Decided { round: Some(1), attempt, run_ids, decision, .. } => {
                Some((*attempt, run_ids.clone(), *decision))
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("{journal:#?}"));
    assert_eq!(decided.0, 1, "one re-run");
    assert_eq!(
        decided.1,
        vec!["inconclusive-r1-v0".to_owned(), "inconclusive-r1-v1".to_owned()]
    );
    assert_eq!(decided.2, Decision::Inconclusive(InconclusiveReason::Insufficient));

    // Finding F1: the decision read both runs' pairs, added.
    let definition = crate::optimization::driver::load_definition(harness.access(), OWNER, DEFINITION)
        .await
        .unwrap();
    let mut runs = Vec::new();
    for run_id in &decided.1 {
        runs.push(load_run_rows(harness.access(), OWNER, run_id).await.unwrap());
    }
    let evidence = decision_evidence(&definition, &runs, policy().max_missing_usage_bp);
    let val_c = evidence
        .cases
        .iter()
        .find(|case| case.case_id == "val-c")
        .unwrap();
    assert_eq!(val_c.pairs, 4, "two trials in each of two runs");
}

#[tokio::test]
async fn five_validation_cases_can_never_be_accepted() {
    let harness = Harness::new().await;
    let executor = script(base_executor(), "baseline", &VALIDATION_CASES[..5], |_| fail());
    let request = harness.request("too-few", SMALL_DEFINITION, budgets(1_000));
    let outcome = settle(&harness, &request, &executor, &repeating_proposer(CANDIDATE_PROMPT)).await;
    assert_eq!(outcome.state, JobState::NothingToPromote);
    assert_eq!(
        decisions(&journal(&harness, "too-few").await),
        vec![(Some(1), Decision::Inconclusive(InconclusiveReason::TooFewCases))]
    );
}

#[tokio::test]
async fn a_checkpoint_that_regresses_on_held_out_fails_the_job() {
    let harness = Harness::new().await;
    let mut executor = script(base_executor(), "baseline", &VALIDATION_CASES, |_| fail());
    executor = script(executor, "candidate", &HELD_OUT_CASES, |_| fail());
    let request = harness.request("held-out-regression", DEFINITION, budgets(1_000));
    let outcome = settle(&harness, &request, &executor, &repeating_proposer(CANDIDATE_PROMPT)).await;
    assert_eq!(
        outcome.state,
        JobState::Failed { reason: "held_out_regression".into() }
    );
    let journal = journal(&harness, "held-out-regression").await;
    assert_eq!(
        decisions(&journal),
        vec![
            (Some(1), Decision::Accept),
            (None, Decision::Reject(RejectReason::CaseRegression))
        ]
    );
    assert_eq!(runs_started(&journal, EvalSplit::HeldOut).len(), 1);
}

#[tokio::test]
async fn feedback_reaches_the_proposer_only_from_the_train_run() {
    let harness = Harness::new().await;
    let registry = CheckRegistry::builtin().with(Box::new(FeedbackCheck));
    let proposer = repeating_proposer(CANDIDATE_PROMPT);
    let request = harness.request("feedback", FEEDBACK_DEFINITION, budgets(1_000));
    drive(
        &harness,
        &request,
        &base_executor(),
        &proposer,
        &registry,
        CancellationToken::new(),
    )
    .await
    .unwrap();

    let calls = proposer.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 3, "one proposal per round");
    for call in &calls {
        // One train case, two trials: two rows. A validation run has 24.
        assert_eq!(call.feedback.len(), TRAIN_CASES.len() * TRIALS_PER_CASE as usize);
        assert!(call
            .feedback
            .iter()
            .all(|entry| entry.feedback.as_deref() == Some(FEEDBACK)));
    }
    let validation = load_verdicts(harness.access(), OWNER, "feedback-r1-v0")
        .await
        .unwrap();
    assert!(!validation.is_empty());
    assert!(
        validation.iter().all(|verdict| verdict.feedback.is_none()),
        "the runner never records feedback off the train split"
    );
}

#[tokio::test]
async fn a_baseline_edited_after_the_freeze_fails_the_job_before_any_further_spend() {
    let harness = Harness::new().await;
    let executor = script(base_executor(), "baseline", &VALIDATION_CASES, |_| fail());
    let proposer = repeating_proposer(CANDIDATE_PROMPT);
    let request = harness.request("drifted", DEFINITION, budgets(1_000));

    // Interrupted before finalization: the pre-cancelled token freezes the
    // job and its first train run, launches no trial, and leaves it Running.
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    let first = drive(&harness, &request, &executor, &proposer, &CheckRegistry::builtin(), cancelled)
        .await
        .unwrap();
    assert_eq!(first.state, JobState::Running);

    // An operator edits a closure document the job does not target.
    harness.install(vec![behavior("Monitor, renamed")]).await;
    let outcome = settle(&harness, &request, &executor, &proposer).await;
    assert_eq!(
        outcome.state,
        JobState::Failed { reason: "baseline_drifted".into() },
        "{outcome:#?}"
    );
    let journal = journal(&harness, "drifted").await;
    assert!(!journal.iter().any(|entry| matches!(entry, JournalEntry::Proposed { .. })));
    assert_eq!(
        spend_so_far(harness.access(), OWNER, &journal).await.unwrap().case_trials,
        0,
        "no trial ran after the drift"
    );
}

#[tokio::test]
async fn a_definition_edited_after_the_freeze_fails_the_job_as_definition_changed() {
    let harness = Harness::new().await;
    let executor = script(base_executor(), "baseline", &VALIDATION_CASES, |_| fail());
    let proposer = repeating_proposer(CANDIDATE_PROMPT);
    let request = harness.request("definition-drift", DEFINITION, budgets(1_000));
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    drive(&harness, &request, &executor, &proposer, &CheckRegistry::builtin(), cancelled)
        .await
        .unwrap();

    // Finding F5: the definition is not in the closure, so this is not drift.
    harness
        .install(vec![(
            Collection::EvalDefinition,
            definition(DEFINITION, "captured_rows_count", &VALIDATION_CASES, 2),
        )])
        .await;
    let outcome = settle(&harness, &request, &executor, &proposer).await;
    assert_eq!(
        outcome.state,
        JobState::Failed { reason: "definition_changed".into() }
    );
}

/// Ruling R9: interrupt a job twice — a cancelled token, then a proposer
/// error — and it reaches the same journal as its uninterrupted twin.
#[tokio::test]
async fn a_resumed_job_reaches_the_journal_of_its_uninterrupted_twin() {
    let harness = Harness::new().await;
    let executor = script(base_executor(), "baseline", &VALIDATION_CASES, |_| fail());
    let proposer = repeating_proposer(CANDIDATE_PROMPT);
    let registry = CheckRegistry::builtin();

    let twin = harness.request("twin-a", DEFINITION, budgets(1_000));
    let uninterrupted = settle(&harness, &twin, &executor, &proposer).await;
    assert_eq!(uninterrupted.state, JobState::ReadyToPromote);

    let resumed = harness.request("twin-b", DEFINITION, budgets(1_000));
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    let stopped = drive(&harness, &resumed, &executor, &proposer, &registry, cancelled)
        .await
        .unwrap();
    assert_eq!(stopped.state, JobState::Running);
    let error = drive(
        &harness,
        &resumed,
        &executor,
        &ErroringProposer,
        &registry,
        CancellationToken::new(),
    )
    .await
    .unwrap_err();
    assert!(format!("{error:#}").contains("proposer unavailable"), "{error:#}");
    let finished = settle(&harness, &resumed, &executor, &proposer).await;
    assert_eq!(finished.state, JobState::ReadyToPromote);
    assert_eq!(finished.checkpoint.map(|held| held.text), uninterrupted.checkpoint.map(|held| held.text));

    let normalized = |journal: Vec<JournalEntry>, job_id: &str| -> Vec<String> {
        journal
            .iter()
            .map(|entry| serde_json::to_string(entry).unwrap().replace(job_id, "JOB"))
            .collect()
    };
    assert_eq!(
        normalized(journal(&harness, "twin-b").await, "twin-b"),
        normalized(journal(&harness, "twin-a").await, "twin-a"),
    );
}
```

- [ ] **Step 2: Declare the module**

In `crates/gents/src/optimization/driver.rs`, add near the top, in rustfmt order:

```rust
#[cfg(test)]
pub(crate) mod matrix;
```

`mod matrix;` beside `driver.rs` resolves to `crates/gents/src/optimization/driver/matrix.rs`, the 2018-edition layout the repository already uses (`eval/runner/mod.rs` with `runner/embedded/`).

- [ ] **Step 3: Run the matrix**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents --lib optimization::driver::matrix 2>&1 | tee /tmp/matrix.log; grep -E "^test |test result" /tmp/matrix.log`
Expected: 13 tests PASS.

If a scenario reaches an unexpected decision, check first that the script's keys were the ones the runner asked for: `ScriptedExecutor::calls` records every key. Log it with `tracing::info!(calls = ?executor.calls.lock().unwrap())` and `--nocapture`, and compare `cell_label` (`baseline`/`candidate`) and `attempt` (`1`) against `script`. Remove the line before committing.

- [ ] **Step 4: Commit**

```bash
git add crates/gents/src/optimization
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" \
  commit -m "test(optimization): the scripted driver matrix (#1455)

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

### Task 6: `show`

**Files:**
- Create: `crates/gents/src/optimization/show.rs`
- Modify: `crates/gents/src/optimization/mod.rs`

Ruling R1: `gents optimization show` is M4's CLI; M6b delivers the library function it wraps. Spec section 5 makes the journal's `summary` a convenience and the referenced runs' `EvalVerdict` rows the authority: `show` recomputes every decision from those rows, flags a mismatch, and flags any decision whose run has since been invalidated. This closes the last spec section 9 end-to-end case.

**Interfaces:**
- Consumes:
  ```rust
  // crates/gents/src/eval/report/evidence.rs
  pub async fn load_run_rows(access: &ConfigAccess, owner: &str, run_id: &str) -> Result<RunRows>; // RunRows::invalidated
  // crates/gents/src/optimization/evidence.rs
  pub fn decision_evidence(definition: &EvalDefinition, runs: &[RunRows], max_missing_usage_bp: u64) -> Evidence;
  pub fn decision_seed(run_ids: &[String]) -> u64;
  // crates/gents/src/optimization/driver.rs
  pub(crate) async fn load_definition(access: &ConfigAccess, owner: &str, definition_id: &str) -> Result<EvalDefinition>;
  pub(crate) fn definition_ref(definition: &EvalDefinition) -> Result<DefinitionRef>;
  // crates/gents/src/eval/documents.rs
  pub async fn invalidate_run(access: &ConfigAccess, owner: &str, run_id: &str, by: &str, reason: &str) -> Result<()>;
  // crates/gents/src/optimization/driver/matrix.rs (tests)
  pub(crate) async fn accepting_harness(job_id: &str) -> (Harness, JobRequest);
  ```
- Produces:
  ```rust
  pub struct DecisionView { pub round: Option<u32>, pub attempt: u32, pub mode: Mode,
      pub run_ids: Vec<String>, pub journaled: Decision, pub recomputed: Decision,
      pub mismatch: bool, pub invalidated: bool }
  pub struct JobView { pub job: JobRecord, pub state: JobState, pub checkpoint: Option<Checkpoint>,
      pub rounds_used: u32, pub definition_changed: bool, pub decisions: Vec<DecisionView> }
  pub async fn show(access: &ConfigAccess, owner: &str, job_id: &str) -> Result<JobView>;
  ```

- [ ] **Step 1: Write the failing test** — create `crates/gents/src/optimization/show.rs` holding only this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::invalidate_run;
    use crate::eval::runner::freeze::tests::OWNER;
    use crate::optimization::driver::matrix::accepting_harness;

    #[tokio::test]
    async fn every_journaled_decision_recomputes_from_its_verdicts() {
        let (harness, request) = accepting_harness("show-clean").await;
        let view = show(harness.access(), OWNER, &request.job_id).await.unwrap();
        assert_eq!(view.state, JobState::ReadyToPromote);
        assert!(!view.definition_changed);
        assert_eq!(view.decisions.len(), 2, "{:#?}", view.decisions);
        for decision in &view.decisions {
            assert_eq!(decision.journaled, decision.recomputed, "{decision:#?}");
            assert!(!decision.mismatch && !decision.invalidated, "{decision:#?}");
        }
    }

    #[tokio::test]
    async fn a_decision_on_an_invalidated_run_is_flagged() {
        let (harness, request) = accepting_harness("show-invalidated").await;
        invalidate_run(
            harness.access(),
            OWNER,
            "show-invalidated-r1-v0",
            OWNER,
            "a fixture in this run was broken",
        )
        .await
        .unwrap();
        let view = show(harness.access(), OWNER, &request.job_id).await.unwrap();
        let round_one = view
            .decisions
            .iter()
            .find(|decision| decision.round == Some(1))
            .unwrap();
        assert!(round_one.invalidated, "{round_one:#?}");
        let held_out = view
            .decisions
            .iter()
            .find(|decision| decision.round.is_none())
            .unwrap();
        assert!(!held_out.invalidated, "only the invalidated run's decision is flagged");
    }
}
```

- [ ] **Step 2: Run it to see it fail**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents --lib optimization::show`
Expected: FAIL to compile, `cannot find function show in this scope`.

- [ ] **Step 3: Write the implementation** — above the test module:

```rust
//! `show`: a job as its journal states it, with every decision recomputed
//! from the `EvalVerdict` rows of the runs it names. The journal's summary is
//! a convenience; these rows are the authority.

use anyhow::Result;

use crate::config_client::ConfigAccess;
use crate::eval::report::load_run_rows;
use crate::optimization::driver::{definition_ref, load_definition};
use crate::optimization::evidence::{decision_evidence, decision_seed};
use crate::optimization::job::{
    checkpoint, derive_state, load_job, rounds_used, Checkpoint, JobRecord, JobState,
    JournalEntry,
};
use crate::optimization::policy::{decide, Decision, Mode};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecisionView {
    pub round: Option<u32>,
    pub attempt: u32,
    pub mode: Mode,
    pub run_ids: Vec<String>,
    pub journaled: Decision,
    pub recomputed: Decision,
    /// The rows no longer support what the journal says.
    pub mismatch: bool,
    /// A run this decision read has since been invalidated.
    pub invalidated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobView {
    pub job: JobRecord,
    pub state: JobState,
    pub checkpoint: Option<Checkpoint>,
    pub rounds_used: u32,
    /// The installed definition no longer matches the one the job froze, so
    /// the recomputations below read a different instrument.
    pub definition_changed: bool,
    pub decisions: Vec<DecisionView>,
}

pub async fn show(access: &ConfigAccess, owner: &str, job_id: &str) -> Result<JobView> {
    let job = load_job(access, owner, job_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("no optimization job {job_id:?} for {owner}"))?;
    let origin = &job.origin;
    let definition = load_definition(access, owner, &origin.definition.definition_id).await?;
    let definition_changed = definition_ref(&definition)? != origin.definition;

    let mut decisions = Vec::new();
    for entry in &job.journal {
        let JournalEntry::Decided {
            round,
            attempt,
            run_ids,
            mode,
            decision,
            ..
        } = entry
        else {
            continue;
        };
        let mut runs = Vec::with_capacity(run_ids.len());
        for run_id in run_ids {
            runs.push(load_run_rows(access, owner, run_id).await?);
        }
        let evidence =
            decision_evidence(&definition, &runs, origin.policy.max_missing_usage_bp);
        let recomputed = decide(*mode, &origin.policy, &evidence, decision_seed(run_ids)).decision;
        decisions.push(DecisionView {
            round: *round,
            attempt: *attempt,
            mode: *mode,
            run_ids: run_ids.clone(),
            journaled: *decision,
            recomputed,
            mismatch: recomputed != *decision,
            invalidated: runs.iter().any(|rows| rows.invalidated),
        });
    }
    Ok(JobView {
        state: derive_state(&job.journal),
        checkpoint: checkpoint(&job.journal),
        rounds_used: rounds_used(&job.journal),
        definition_changed,
        decisions,
        job,
    })
}
```

- [ ] **Step 4: Wire the module** — in `crates/gents/src/optimization/mod.rs` add `pub mod show;` in rustfmt order and:

```rust
pub use show::{show, DecisionView, JobView};
```

- [ ] **Step 5: Run the tests**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents --lib optimization::show`
Expected: PASS, 2 tests.

- [ ] **Step 6: Commit**

```bash
git add crates/gents/src/optimization
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" \
  commit -m "feat(optimization): show recomputes every decision from its verdicts (#1455)

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

### PR 3 gate

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents --lib optimization 2>&1 | tee /tmp/pr3-lib.log; grep "test result" /tmp/pr3-lib.log`
Run: `CARGO_BUILD_JOBS=4 cargo test -p gents --lib eval`
Run: `CARGO_BUILD_JOBS=4 cargo check --workspace --all-targets 2>&1 | tee /tmp/pr3-check.log; grep -c "^error" /tmp/pr3-check.log`
Expected: all PASS; the grep reports 0.
Run: `cargo fmt --all --check`

---

## PR 4: Promotion, revert and the live scenarios

Branch: `optimization/23-promote`, base `optimization/22-driver`. Deliverable: the operator's two verbs as library functions (ruling R1), the whole refusal matrix, and the two live scenarios M5 will run (ruling R7).

### Task 1: `promote` and `revert`

**Files:**
- Create: `crates/gents/src/optimization/promote.rs`
- Modify: `crates/gents/src/optimization/mod.rs`

Ruling R6: both verbs take the job's identity `(access, owner, job_id)`, plus the spec section 7 `digest` the operator confirms and `by`, the acting DID. `jobs_dir` and `behavior_id` come from `JobOrigin`, so the operator needs nothing but the job id. `by` is the launching home's identity supplied by the caller — the same pattern as the runner's `evaluator_did` — and is compared against `origin.owner`; an authenticated, enforced identity boundary is spec 2b's. Ruling R4: `Reverted` is terminal, so a reverted job can never be promoted again.

**Interfaces:**
- Consumes:
  ```rust
  // crates/gents/src/config_client/desired_state.rs (optimization/03-cas)
  pub async fn apply_desired_state_plan(txn: &ConfigApplyTxn<'_>, plan: &DesiredStateApplyPlan) -> Result<DesiredStateApplyCounts>;
  pub struct DriftedDocument { pub collection: Collection, pub owner: String, pub id: String,
      pub expected: Option<String>, pub found: Option<String> }
  pub struct StaleExpectation { pub drifted: Vec<DriftedDocument> }
  pub fn stale_expectation(error: &anyhow::Error) -> Option<&StaleExpectation>;
  // crates/gents/src/optimization/target.rs
  pub async fn capture_closure(txn: &ConfigApplyTxn<'_>, owner: &str) -> Result<Closure>;
  pub fn closure_digests(closure: &Closure) -> Result<Vec<FrozenDocument>>;
  pub fn current_text(closure: &Closure, target: &Target) -> Result<String>;
  pub fn apply_text(closure: &Closure, target: &Target, text: &str) -> Result<Closure>;
  pub fn target_digest(closure: &Closure, target: &Target) -> Result<String>;
  pub fn target_plan(closure: &Closure, target: &Target, frozen: &[FrozenDocument]) -> Result<DesiredStateApplyPlan>;
  // crates/gents/src/optimization/job.rs
  pub(crate) async fn load_job_in_txn(txn: &ConfigApplyTxn<'_>, owner: &str, job_id: &str) -> Result<Option<JobRecord>>;
  pub(crate) async fn append_in_txn(txn: &ConfigApplyTxn<'_>, job: &JobRecord, entry: &JournalEntry) -> Result<()>;
  // crates/gents/src/optimization/driver.rs
  pub fn baseline_dir(jobs_dir: &Path, job_id: &str) -> PathBuf;
  pub fn job_dir(jobs_dir: &Path, job_id: &str) -> PathBuf;
  // crates/gents/src/optimization/driver/matrix.rs (tests)
  pub(crate) async fn accepting_harness(job_id: &str) -> (Harness, JobRequest);
  pub(crate) async fn rejecting_harness(job_id: &str) -> (Harness, JobRequest);
  pub(crate) const BASELINE_PROMPT: &str;
  pub(crate) const CANDIDATE_PROMPT: &str;
  // crates/gents/src/self_config/mod.rs
  pub const SELF_CONFIG_TOOL_NAMES: [&str; 6];
  ```
- Produces:
  ```rust
  pub struct Promotion { pub target_digest: String, pub previous_text: String, pub previous_digest: String }
  pub struct PromoteRefused { pub reason: &'static str, pub detail: String }
  pub fn promote_refused(error: &anyhow::Error) -> Option<&PromoteRefused>;
  pub async fn promote(access: &ConfigAccess, owner: &str, job_id: &str, digest: &str, by: &str) -> Result<Promotion>;
  pub async fn revert(access: &ConfigAccess, owner: &str, job_id: &str, digest: &str, by: &str) -> Result<()>;
  ```
  `reason` is one of `unknown_job`, `foreign_did`, `not_ready`, `not_promoted`, `no_checkpoint`, `wrong_digest`, `rebuild_mismatch`, `stale_closure`, `target_moved`.

- [ ] **Step 1: Write the failing test** — create `crates/gents/src/optimization/promote.rs` holding only this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::runner::freeze::tests::OWNER;
    use crate::optimization::driver::matrix::{
        accepting_harness, rejecting_harness, Harness, BASELINE_PROMPT, CANDIDATE_PROMPT,
    };
    use crate::optimization::job::load_job;
    use crate::Collection;
    use serde_json::json;

    /// The live target's text, read the way an operator would.
    async fn live_prompt(access: &ConfigAccess) -> Option<String> {
        let closure = access
            .transact("test.read_closure", |txn| {
                Box::pin(async move { capture_closure(txn, OWNER).await })
            })
            .await
            .unwrap();
        closure.iter().find_map(|(collection, value)| {
            (*collection == Collection::AgentContext && value["context_id"] == "monitor-context")
                .then(|| value["system_prompt"].as_str().unwrap_or_default().to_owned())
        })
    }

    async fn state(harness: &Harness, job_id: &str) -> JobState {
        let job = load_job(harness.access(), OWNER, job_id).await.unwrap().unwrap();
        derive_state(&job.journal)
    }

    async fn checkpoint_digest(harness: &Harness, job_id: &str) -> String {
        let job = load_job(harness.access(), OWNER, job_id).await.unwrap().unwrap();
        checkpoint(&job.journal)
            .expect("the harness drove an accepting job")
            .pack_digest
    }

    fn operator_edit() -> (Collection, serde_json::Value) {
        (
            Collection::AgentContext,
            json!({
                "context_id": "monitor-context",
                "agent_did": OWNER,
                "display_name": "Monitor",
                "system_prompt": "An operator wrote this by hand.\n",
            }),
        )
    }

    #[tokio::test]
    async fn a_promotion_writes_only_the_target_and_records_what_it_replaced() {
        let (harness, request) = accepting_harness("promote-once").await;
        let digest = checkpoint_digest(&harness, &request.job_id).await;
        let promotion = promote(harness.access(), OWNER, &request.job_id, &digest, OWNER)
            .await
            .unwrap();
        assert_eq!(promotion.previous_text, BASELINE_PROMPT);
        assert_ne!(promotion.target_digest, promotion.previous_digest);
        assert_eq!(live_prompt(harness.access()).await.as_deref(), Some(CANDIDATE_PROMPT));
        assert_eq!(state(&harness, &request.job_id).await, JobState::Promoted);
    }

    /// Ruling R4: a revert restores the previous text and ends the job.
    #[tokio::test]
    async fn a_revert_restores_the_previous_text_and_is_terminal() {
        let (harness, request) = accepting_harness("revert-once").await;
        let digest = checkpoint_digest(&harness, &request.job_id).await;
        let promotion = promote(harness.access(), OWNER, &request.job_id, &digest, OWNER)
            .await
            .unwrap();

        revert(harness.access(), OWNER, &request.job_id, &promotion.target_digest, OWNER)
            .await
            .unwrap();
        assert_eq!(live_prompt(harness.access()).await.as_deref(), Some(BASELINE_PROMPT));
        assert_eq!(state(&harness, &request.job_id).await, JobState::Reverted);

        let again = promote(harness.access(), OWNER, &request.job_id, &digest, OWNER)
            .await
            .unwrap_err();
        assert_eq!(
            promote_refused(&again).unwrap().reason,
            "not_ready",
            "a further promotion is a new job"
        );
        assert_eq!(live_prompt(harness.access()).await.as_deref(), Some(BASELINE_PROMPT));
    }

    #[tokio::test]
    async fn a_revert_is_refused_once_the_target_moved_and_the_edit_survives() {
        let (harness, request) = accepting_harness("revert-moved").await;
        let digest = checkpoint_digest(&harness, &request.job_id).await;
        let promotion = promote(harness.access(), OWNER, &request.job_id, &digest, OWNER)
            .await
            .unwrap();
        harness.install(vec![operator_edit()]).await;

        let error = revert(harness.access(), OWNER, &request.job_id, &promotion.target_digest, OWNER)
            .await
            .unwrap_err();
        assert_eq!(promote_refused(&error).unwrap().reason, "target_moved");
        assert_eq!(
            live_prompt(harness.access()).await.as_deref(),
            Some("An operator wrote this by hand.\n"),
            "the user's edit is preserved"
        );
        assert_eq!(state(&harness, &request.job_id).await, JobState::Promoted);
    }

    #[tokio::test]
    async fn a_closure_edited_after_the_freeze_refuses_the_promotion_and_marks_the_job_stale() {
        let (harness, request) = accepting_harness("promote-stale").await;
        let digest = checkpoint_digest(&harness, &request.job_id).await;
        // A document of the frozen closure that the promotion does not write.
        harness
            .install(vec![(
                Collection::AgentBehavior,
                json!({
                    "behavior_id": "monitor",
                    "agent_did": OWNER,
                    "display_name": "Monitor, renamed",
                    "context_id": "monitor-context",
                    "inference_profile_id": "local",
                }),
            )])
            .await;

        let error = promote(harness.access(), OWNER, &request.job_id, &digest, OWNER)
            .await
            .unwrap_err();
        let refusal = promote_refused(&error).unwrap_or_else(|| panic!("{error:#}"));
        assert_eq!(refusal.reason, "stale_closure");
        assert!(refusal.detail.contains("AgentBehavior"), "{}", refusal.detail);
        assert_eq!(
            live_prompt(harness.access()).await.as_deref(),
            Some(BASELINE_PROMPT),
            "the live node is unchanged"
        );
        assert_eq!(state(&harness, &request.job_id).await, JobState::Stale);
    }

    #[tokio::test]
    async fn a_foreign_did_a_wrong_digest_and_an_unready_job_are_each_refused() {
        let (harness, request) = accepting_harness("promote-refusals").await;
        let digest = checkpoint_digest(&harness, &request.job_id).await;

        let foreign = promote(harness.access(), OWNER, &request.job_id, &digest, "did:key:someone-else")
            .await
            .unwrap_err();
        assert_eq!(promote_refused(&foreign).unwrap().reason, "foreign_did");

        let wrong = promote(harness.access(), OWNER, &request.job_id, "sha256:not-it", OWNER)
            .await
            .unwrap_err();
        assert_eq!(promote_refused(&wrong).unwrap().reason, "wrong_digest");
        assert_eq!(
            live_prompt(harness.access()).await.as_deref(),
            Some(BASELINE_PROMPT),
            "a refused promotion writes nothing"
        );
        assert_eq!(state(&harness, &request.job_id).await, JobState::ReadyToPromote);

        let (rejecting, unready) = rejecting_harness("unready").await;
        let error = promote(rejecting.access(), OWNER, &unready.job_id, "sha256:anything", OWNER)
            .await
            .unwrap_err();
        assert_eq!(promote_refused(&error).unwrap().reason, "not_ready");
    }

    /// Spec section 9: optimization is an operator's verb, never a model's.
    /// `self_config` is a directory module, so every file in it is scanned.
    #[test]
    fn the_model_facing_config_tool_has_no_optimization_surface() {
        for name in crate::self_config::SELF_CONFIG_TOOL_NAMES {
            assert!(!name.contains("optim"), "the self-config tool set names {name}");
        }
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/self_config");
        let mut scanned = 0;
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("rs") {
                continue;
            }
            let source = std::fs::read_to_string(&path).unwrap();
            for word in ["optimization", "OptimizationJob", "promote", "revert"] {
                assert!(
                    !source.contains(word),
                    "{} mentions {word}; the optimization surface is the operator's",
                    path.display()
                );
            }
            scanned += 1;
        }
        assert!(scanned >= 2, "expected mod.rs and its siblings in {}", dir.display());
    }
}
```

- [ ] **Step 2: Run it to see it fail**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents --lib optimization::promote`
Expected: FAIL to compile, `cannot find function promote in this scope`.

- [ ] **Step 3: Write the implementation** — above the test module:

```rust
//! Promotion and revert: the operator's two verbs.
//!
//! Both run in one transaction that writes only the target document. There
//! is no force flag: on drift the transaction rolls back with the live node
//! untouched, a second transaction journals the refusal, and the job is
//! `Stale`. Nothing reverts on its own, and a revert ends the job.

use anyhow::{Context, Result};

use crate::config_client::{apply_desired_state_plan, stale_expectation, ConfigAccess};
use crate::optimization::driver::{baseline_dir, job_dir};
use crate::optimization::job::{
    append, append_in_txn, checkpoint, derive_state, load_job, load_job_in_txn, DriftedRef,
    JobRecord, JobState, JournalEntry,
};
use crate::optimization::subject::{materialize_candidate, materialize_pack};
use crate::optimization::target::{
    apply_text, capture_closure, closure_digests, current_text, target_digest, target_plan,
    FrozenDocument,
};

/// What a promotion did, and what it replaced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Promotion {
    pub target_digest: String,
    pub previous_text: String,
    pub previous_digest: String,
}

/// Why an operator's verb did not run. `reason` is a closed vocabulary.
#[derive(Debug)]
pub struct PromoteRefused {
    pub reason: &'static str,
    pub detail: String,
}

impl std::fmt::Display for PromoteRefused {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.reason, self.detail)
    }
}

impl std::error::Error for PromoteRefused {}

pub fn promote_refused(error: &anyhow::Error) -> Option<&PromoteRefused> {
    error.downcast_ref::<PromoteRefused>()
}

fn refused(reason: &'static str, detail: impl Into<String>) -> anyhow::Error {
    anyhow::Error::from(PromoteRefused {
        reason,
        detail: detail.into(),
    })
}

/// The closure moved under the job. Carried out of the transaction so the
/// refusal can be journaled after the rollback.
#[derive(Debug)]
struct ClosureDrift(Vec<DriftedRef>);

impl std::fmt::Display for ClosureDrift {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "the frozen closure moved: {:?}", self.0)
    }
}

impl std::error::Error for ClosureDrift {}

/// Both ways a promotion learns the closure moved: the explicit comparison,
/// which also catches a document added since the freeze, and the transaction's
/// own digest preconditions, which catch a concurrent edit.
fn drift_of(error: &anyhow::Error) -> Option<Vec<DriftedRef>> {
    if let Some(drift) = error.downcast_ref::<ClosureDrift>() {
        return Some(drift.0.clone());
    }
    stale_expectation(error).map(|stale| {
        stale
            .drifted
            .iter()
            .map(|document| DriftedRef {
                collection: document.collection.graphql_type().to_owned(),
                id: document.id.clone(),
            })
            .collect()
    })
}

fn between(frozen: &[FrozenDocument], live: &[FrozenDocument]) -> Vec<DriftedRef> {
    let reference = |document: &FrozenDocument| DriftedRef {
        collection: document.collection.graphql_type().to_owned(),
        id: document.id.clone(),
    };
    let mut drifted: Vec<DriftedRef> = frozen
        .iter()
        .filter(|document| {
            !live.iter().any(|candidate| {
                candidate.collection == document.collection
                    && candidate.id == document.id
                    && candidate.digest == document.digest
            })
        })
        .map(reference)
        .collect();
    drifted.extend(
        live.iter()
            .filter(|document| {
                !frozen.iter().any(|candidate| {
                    candidate.collection == document.collection && candidate.id == document.id
                })
            })
            .map(reference),
    );
    drifted
}

/// Load the job and check what both verbs require: who is asking, and what
/// state the job is in. `by` is the launching home's DID as its caller states
/// it; an enforced, authenticated boundary is spec 2b's.
async fn job_in_state(
    access: &ConfigAccess,
    owner: &str,
    job_id: &str,
    by: &str,
    expected: JobState,
) -> Result<JobRecord> {
    let job = load_job(access, owner, job_id)
        .await?
        .ok_or_else(|| refused("unknown_job", format!("no job {job_id:?} for {owner}")))?;
    if by != job.origin.owner {
        return Err(refused(
            "foreign_did",
            format!("{by:?} does not own job {job_id:?}"),
        ));
    }
    let state = derive_state(&job.journal);
    if state != expected {
        let reason = if expected == JobState::Promoted {
            "not_promoted"
        } else {
            "not_ready"
        };
        return Err(refused(reason, format!("job {job_id:?} is {}", state.label())));
    }
    Ok(job)
}

/// Write the retained checkpoint's text onto the live target, guarded by the
/// whole frozen closure.
pub async fn promote(
    access: &ConfigAccess,
    owner: &str,
    job_id: &str,
    digest: &str,
    by: &str,
) -> Result<Promotion> {
    let mut job = job_in_state(access, owner, job_id, by, JobState::ReadyToPromote).await?;
    let retained = checkpoint(&job.journal).ok_or_else(|| {
        refused(
            "no_checkpoint",
            format!("job {job_id:?} is ready but retained nothing"),
        )
    })?;
    if retained.pack_digest != digest {
        return Err(refused(
            "wrong_digest",
            format!("job {job_id:?} retains {}, not {digest}", retained.pack_digest),
        ));
    }

    // Rebuild the candidate from the job's own baseline copy, located through
    // the origin (ruling R6), and check it still digests to the journal's.
    let origin = &job.origin;
    let baseline = materialize_pack(
        &baseline_dir(&origin.jobs_dir, job_id),
        owner,
        &origin.subject.behavior_id,
    )?;
    let rebuild_dir = job_dir(&origin.jobs_dir, job_id).join("promote-rebuild");
    if rebuild_dir.exists() {
        std::fs::remove_dir_all(&rebuild_dir)
            .with_context(|| format!("clearing {}", rebuild_dir.display()))?;
    }
    let rebuilt = materialize_candidate(&baseline, owner, &retained.text, &rebuild_dir)?;
    if rebuilt.digest != retained.pack_digest {
        return Err(refused(
            "rebuild_mismatch",
            format!(
                "rebuilding the checkpoint gives {}, not the journaled {}",
                rebuilt.digest, retained.pack_digest
            ),
        ));
    }

    let (owner_owned, job_id_owned, text) = (owner.to_owned(), job_id.to_owned(), retained.text);
    let applied = access
        .transact("optimization.promote", |txn| {
            let (owner, job_id, text) = (&owner_owned, &job_id_owned, &text);
            Box::pin(async move {
                let job = load_job_in_txn(txn, owner, job_id)
                    .await?
                    .context("the job disappeared mid-promotion")?;
                let closure = capture_closure(txn, owner).await?;
                let drifted = between(&job.origin.closure, &closure_digests(&closure)?);
                if !drifted.is_empty() {
                    return Err(anyhow::Error::new(ClosureDrift(drifted)));
                }
                let target = &job.origin.target;
                let previous_text = current_text(&closure, target)?;
                let previous_digest = target_digest(&closure, target)?;
                let patched = apply_text(&closure, target, text)?;
                // Writes only the target; expects the entire frozen closure.
                let plan = target_plan(&patched, target, &job.origin.closure)?;
                apply_desired_state_plan(txn, &plan).await?;
                let promotion = Promotion {
                    target_digest: target_digest(&patched, target)?,
                    previous_text,
                    previous_digest,
                };
                let entry = JournalEntry::Promoted {
                    by: job.origin.owner.clone(),
                    target_digest: promotion.target_digest.clone(),
                    previous_text: promotion.previous_text.clone(),
                    previous_digest: promotion.previous_digest.clone(),
                };
                append_in_txn(txn, &job, &entry).await?;
                Ok(promotion)
            })
        })
        .await;

    match applied {
        Ok(promotion) => {
            tracing::info!(job_id, by, "optimization checkpoint promoted");
            Ok(promotion)
        }
        Err(error) => {
            let Some(drifted) = drift_of(&error) else {
                return Err(error);
            };
            // The write rolled back; a second transaction records why.
            let entry = JournalEntry::PromotionRefused {
                drifted: drifted.clone(),
            };
            append(access, &mut job, entry).await?;
            tracing::warn!(job_id, ?drifted, "promotion refused; the job is stale");
            Err(refused("stale_closure", format!("{drifted:?}")))
        }
    }
}

/// Write `previous_text` back, expecting the target to still hold exactly what
/// the promotion wrote. Terminal (ruling R4).
pub async fn revert(
    access: &ConfigAccess,
    owner: &str,
    job_id: &str,
    digest: &str,
    by: &str,
) -> Result<()> {
    let job = job_in_state(access, owner, job_id, by, JobState::Promoted).await?;
    let promoted = job
        .journal
        .iter()
        .rev()
        .find_map(|entry| match entry {
            JournalEntry::Promoted {
                target_digest,
                previous_text,
                ..
            } => Some((target_digest.clone(), previous_text.clone())),
            _ => None,
        })
        .ok_or_else(|| refused("not_promoted", "the job has no promotion to revert"))?;
    if promoted.0 != digest {
        return Err(refused(
            "wrong_digest",
            format!("job {job_id:?} promoted {}, not {digest}", promoted.0),
        ));
    }

    let (owner_owned, job_id_owned) = (owner.to_owned(), job_id.to_owned());
    let result = access
        .transact("optimization.revert", |txn| {
            let (owner, job_id, promoted) = (&owner_owned, &job_id_owned, &promoted);
            Box::pin(async move {
                let job = load_job_in_txn(txn, owner, job_id)
                    .await?
                    .context("the job disappeared mid-revert")?;
                let closure = capture_closure(txn, owner).await?;
                let target = &job.origin.target;
                let live = target_digest(&closure, target)?;
                if live != promoted.0 {
                    return Err(refused(
                        "target_moved",
                        format!("the target now digests to {live}, not the promoted {}", promoted.0),
                    ));
                }
                let restored = apply_text(&closure, target, &promoted.1)?;
                let expectation = vec![FrozenDocument {
                    collection: target.field.collection(),
                    owner: target.owner.clone(),
                    id: target.id.clone(),
                    digest: promoted.0.clone(),
                }];
                let plan = target_plan(&restored, target, &expectation)?;
                apply_desired_state_plan(txn, &plan).await?;
                let entry = JournalEntry::Reverted {
                    by: job.origin.owner.clone(),
                };
                append_in_txn(txn, &job, &entry).await?;
                Ok(())
            })
        })
        .await;

    match result {
        Ok(()) => {
            tracing::warn!(job_id, by, "promoted prompt reverted; the job is closed");
            Ok(())
        }
        Err(error) if stale_expectation(&error).is_some() => {
            Err(refused("target_moved", format!("{error:#}")))
        }
        Err(error) => Err(error),
    }
}
```

- [ ] **Step 4: Wire the module** — in `crates/gents/src/optimization/mod.rs` add `pub mod promote;` in rustfmt order and:

```rust
pub use promote::{promote, promote_refused, revert, PromoteRefused, Promotion};
```

- [ ] **Step 5: Run the tests**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents --lib optimization::promote 2>&1 | tee /tmp/promote.log; grep -E "^test |test result" /tmp/promote.log`
Expected: 6 tests PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/gents/src/optimization
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" \
  commit -m "feat(optimization): operator-only promote and a terminal revert (#1455)

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

### Task 2: The live accept and reject

**Files:**
- Create: `crates/gents/tests/optimization_live.rs`
- Modify: `crates/gents/tests/support/live_inference.rs`, `crates/gents/tests/eval_runner_canary.rs`

Ruling R7: both scenarios take the M3 definition pack directory and the definition id from environment variables and stay `#[ignore]`; they run in M5, once M3's pack and M5's calibration exist. They use the real embedded runner and a real provider. Only the proposer is scripted, because M6b ships no LLM proposer: an accept and a reject are produced by choosing which text the proposer offers, not by scripting the model.

**Interfaces:**
- Consumes:
  ```rust
  // gents::eval::runner::embedded
  impl EmbeddedExecutor { pub fn new(options: DocumentRuntimeOptions, runs_dir: PathBuf) -> Self; }
  impl EmbeddedHome { pub async fn create_temp(prefix: &str) -> Result<Self>; pub fn did(&self) -> &str; }
  // gents::optimization
  pub fn materialize_pack(dir: &Path, owner: &str, behavior_id: &str) -> Result<MaterializedPack>;
  pub async fn run_job(access: &ConfigAccess, request: &JobRequest, executor: &dyn TrialExecutor,
      proposer: &dyn Proposer, registry: &CheckRegistry, policy: &PolicyV2,
      cancel: CancellationToken) -> Result<JobOutcome>;
  // gents::document_config::PackConfig
  pub eval_definitions: Vec<EvalDefinition>,   // pack_config.rs:197
  ```
- Produces:
  ```rust
  // crates/gents/tests/support/live_inference.rs
  pub fn live_provider_from_env() -> (String, serde_json::Value, String);
  ```

- [ ] **Step 1: Move the provider helper into the shared support module**

`live_provider_from_env` is a private function at the bottom of `crates/gents/tests/eval_runner_canary.rs` (it reads `GENTS_LIVE_CONFIG_PROVIDER` and `GENTS_D4F_MODEL` and returns `(endpoint, auth, model)`). Move its body verbatim into `crates/gents/tests/support/live_inference.rs` as `pub fn live_provider_from_env() -> (String, serde_json::Value, String)`, dropping the `support::` prefix from its internal `live_inference::d4f_endpoint()` call, and replace the canary's copy with `support::live_inference::live_provider_from_env()`. This is a move, not a change.

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents --test eval_runner_canary`
Expected: unchanged — 3 passed, 1 ignored.

- [ ] **Step 2: Write the live scenarios** — create `crates/gents/tests/optimization_live.rs`:

```rust
//! The two live scenarios M6 is judged by: one accept and one reject on a real
//! provider. Never in CI; run in M5 (ruling R7).
//!
//! The subject and its definition come from M3's pack, named by environment
//! variable, so this file compiles and stays ignored without that branch.

mod support;

use std::path::{Path, PathBuf};

use gents::config_client::{
    apply_desired_state_plan, DesiredStateApplyDocument, DesiredStateApplyPlan,
};
use gents::eval::checks::CheckRegistry;
use gents::eval::runner::embedded::{EmbeddedExecutor, EmbeddedHome};
use gents::eval::runner::RunOptions;
use gents::optimization::{
    materialize_pack, run_job, Budgets, JobOutcome, JobRequest, JobState, PolicyV2,
    ScriptedProposer,
};
use gents::{Collection, ConfigAccess, DocumentRuntimeOptions};
use serde_json::{json, Value};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

/// The M3 pack directory: the monitor subject and its eval definition.
const PACK_VAR: &str = "GENTS_OPTIMIZATION_LIVE_PACK";
/// The id of the definition inside that pack (M3 names it `monitor-findings`).
const DEFINITION_VAR: &str = "GENTS_OPTIMIZATION_LIVE_DEFINITION_ID";
/// The behavior whose context is optimized; `monitor` unless overridden.
const BEHAVIOR_VAR: &str = "GENTS_OPTIMIZATION_LIVE_BEHAVIOR";
/// A deliberately thin baseline, so a real provider has room to improve.
const THIN_PROMPT: &str = "Look at the mailbox and say something.\n";

#[tokio::test]
#[ignore = "M5: needs GENTS_LIVE_CONFIG_PROVIDER, GENTS_OPTIMIZATION_LIVE_PACK, GENTS_OPTIMIZATION_LIVE_DEFINITION_ID and a real backend"]
async fn live_accept_the_golden_prompt_beats_a_thin_one_and_reaches_ready_to_promote() {
    let fixture = Fixture::new("live-accept").await;
    // The candidate is the pack's own golden prompt, which M3 wrote to pass
    // these cases; the baseline is the thin one.
    let proposer = ScriptedProposer::new(vec![
        (fixture.golden_prompt.clone(), "restore the full monitor instructions".into());
        3
    ]);
    let outcome = fixture.drive(&proposer).await;
    tracing::info!(state = outcome.state.label(), "live accept scenario");
    assert_eq!(
        outcome.state,
        JobState::ReadyToPromote,
        "the golden prompt did not beat a thin one: {outcome:#?}"
    );
    assert_eq!(
        outcome.checkpoint.expect("a checkpoint").text,
        fixture.golden_prompt
    );
}

#[tokio::test]
#[ignore = "M5: needs GENTS_LIVE_CONFIG_PROVIDER, GENTS_OPTIMIZATION_LIVE_PACK, GENTS_OPTIMIZATION_LIVE_DEFINITION_ID and a real backend"]
async fn live_reject_a_prompt_that_ignores_the_task_never_reaches_ready_to_promote() {
    let fixture = Fixture::new("live-reject").await;
    // A prompt that removes the output contract the checks grade.
    let proposer = ScriptedProposer::new(vec![
        ("Answer in one word. Do not use any tools.\n".into(), "shorter is better".into());
        3
    ]);
    let outcome = fixture.drive(&proposer).await;
    tracing::info!(state = outcome.state.label(), "live reject scenario");
    assert_ne!(
        outcome.state,
        JobState::ReadyToPromote,
        "a prompt that ignores the task was promoted: {outcome:#?}"
    );
    assert!(outcome.checkpoint.is_none(), "{outcome:#?}");
}

/// A launching home with the M3 definition, a live inference binding, and a
/// thinned copy of the M3 pack as the baseline subject.
struct Fixture {
    _home: EmbeddedHome,
    _dirs: TempDir,
    access: ConfigAccess,
    request: JobRequest,
    golden_prompt: String,
}

impl Fixture {
    async fn new(job_id: &str) -> Self {
        // A test binary installs no subscriber, so an unreported finding is a
        // dropped one, even under `--nocapture`.
        let _ = tracing_subscriber::fmt()
            .with_test_writer()
            .with_ansi(false)
            .with_env_filter(tracing_subscriber::EnvFilter::new(
                "off,optimization_live=info,gents::optimization=info",
            ))
            .try_init();

        let pack = PathBuf::from(
            std::env::var(PACK_VAR).unwrap_or_else(|_| panic!("{PACK_VAR} must name the M3 pack")),
        );
        let definition_id = std::env::var(DEFINITION_VAR)
            .unwrap_or_else(|_| panic!("{DEFINITION_VAR} must name the definition in that pack"));
        let behavior = std::env::var(BEHAVIOR_VAR).unwrap_or_else(|_| "monitor".into());

        let home = EmbeddedHome::create_temp("optimization-live").await.unwrap();
        let access = ConfigAccess::Local(home.node.clone());
        let owner = home.did().to_string();
        gents::ensure_agent_principal(home.node.as_ref(), &owner)
            .await
            .unwrap();
        let dirs = tempfile::tempdir().unwrap();

        // The pack carries both the subject and the definition.
        let golden = materialize_pack(&pack, &owner, &behavior).expect("the M3 pack loads");
        let golden_prompt = gents::optimization::baseline_text(&golden).unwrap();
        let definition = golden
            .config
            .eval_definitions
            .iter()
            .find(|definition| definition.definition_id == definition_id)
            .unwrap_or_else(|| panic!("the pack declares no definition {definition_id:?}"));
        let sidecar = golden
            .prompt_asset
            .clone()
            .expect("the M3 pack keeps its prompt in a sidecar");
        let thin_pack = dirs.path().join("thin-pack");
        copy_tree(&pack, &thin_pack);
        std::fs::write(thin_pack.join(&sidecar), THIN_PROMPT).unwrap();

        let (endpoint, auth, model) = support::live_inference::live_provider_from_env();
        install(
            &access,
            vec![
                (Collection::EvalDefinition, serde_json::to_value(definition).unwrap()),
                (
                    Collection::InferenceBackend,
                    json!({
                        "agent_did": owner,
                        "backend_id": "live",
                        "name": "Live optimization backend",
                        "provider_kind": "OpenAiCompatible",
                        "openai_wire_api": "chat_completions",
                        "endpoint": endpoint,
                        "auth": auth,
                        "max_concurrent": 4,
                        "max_queue_depth": 100,
                    }),
                ),
                (
                    Collection::InferenceSampling,
                    json!({"agent_did": owner, "sampling_id": "live", "temperature": 0.0}),
                ),
                (
                    Collection::InferenceProfile,
                    json!({
                        "agent_did": owner,
                        "profile_id": "live",
                        "backend_id": "live",
                        "model_name": model,
                        "sampling_id": "live",
                    }),
                ),
                // Ruling R5: the live context must hold the pack's prompt.
                (
                    Collection::AgentContext,
                    json!({
                        "context_id": golden.context_id,
                        "agent_did": owner,
                        "display_name": "Monitor",
                        "system_prompt": THIN_PROMPT,
                    }),
                ),
                (
                    Collection::AgentBehavior,
                    json!({
                        "behavior_id": behavior,
                        "agent_did": owner,
                        "display_name": "Monitor",
                        "context_id": golden.context_id,
                        "inference_profile_id": "live",
                    }),
                ),
            ],
        )
        .await;

        let request = JobRequest {
            job_id: job_id.into(),
            owner: owner.clone(),
            evaluator_did: owner,
            behavior_id: behavior,
            definition_id,
            inference_profile_id: "live".into(),
            baseline_pack: thin_pack,
            trials_per_case: 2,
            budgets: Budgets {
                max_rounds: 3,
                max_case_trials: 1_000,
                max_tokens: u64::MAX,
                deadline_unix_secs: None,
            },
            max_text_bytes: 32 * 1024,
            seed_base: 7_000,
            jobs_dir: dirs.path().join("eval/jobs"),
            runs_dir: dirs.path().join("eval/runs"),
            source_commit: "live".into(),
            source_dirty: false,
            concurrency: 2,
            max_infra_retries: 1,
            breaker_threshold: 8,
            deadline_secs: Some(900),
            run_options: RunOptions::default(),
        };

        Self {
            _home: home,
            _dirs: dirs,
            access,
            request,
            golden_prompt,
        }
    }

    async fn drive(&self, proposer: &ScriptedProposer) -> JobOutcome {
        let executor = EmbeddedExecutor::new(
            DocumentRuntimeOptions::default(),
            self.request.runs_dir.clone(),
        );
        run_job(
            &self.access,
            &self.request,
            &executor,
            proposer,
            &CheckRegistry::builtin(),
            &PolicyV2::uncalibrated(),
            CancellationToken::new(),
        )
        .await
        .unwrap()
    }
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
        .transact("optimization_live.install", |txn| {
            let plan = &plan;
            Box::pin(async move { apply_desired_state_plan(txn, plan).await.map(|_| ()) })
        })
        .await
        .unwrap();
}

fn copy_tree(source: &Path, destination: &Path) {
    std::fs::create_dir_all(destination).unwrap();
    for entry in std::fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let target = destination.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}
```

`PolicyV2::uncalibrated()` has `max_rounds: 3`, matching the budget, so `check_policy` passes. Its parameters are placeholders until M5's A/A calibration sets them, which is exactly when these scenarios run.

- [ ] **Step 3: Prove it compiles and stays out of CI**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents --test optimization_live --no-run`
Expected: compiles.

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents --test optimization_live`
Expected: `2 ignored`, 0 run.

- [ ] **Step 4: Commit**

```bash
git add crates/gents/tests
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" \
  commit -m "test(optimization): live accept and reject scenarios for M5, ignored by default

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

### Task 3: The four PR descriptions

**Files:**
- Create: `<worktree>/.superpowers/sdd/2026-09-22-optimization-driver/pr-descriptions.md`

The coordinator never pushes and never opens a PR; it writes the descriptions the orchestrator uses. One section per PR, each carrying baseline, owners, deletions and validation, per CLAUDE.md's stacked-PR rule.

**Interfaces:**
- Consumes: the four branch heads produced by PRs 1 to 4, and the M6b base commit the orchestrator supplied at spin-up.
- Produces: the file itself.

- [ ] **Step 1: Write the file**

````markdown
# M6b pull request descriptions

## PR 1 — `optimization/20-job` → the M6b base

**Baseline.** The M6b base: `eval/13-runner-embedded` rebased onto the amended
`eval/05-contract`, carrying `eval/06-protected`, `optimization/01..03` and
`optimization/10..12`. Commit supplied by the orchestrator at spin-up: record it here.

**What it adds.** The `OptimizationJob` collection — SDL, both catalogs, the
protocol mirror, the migration baseline pin, local-audit classification — the
one-field closure target, and the typed journal whose `journal_len` guard
refines `Optimization.appendIf`.

**Owners.** The journal is owned by `gents::optimization::job` and written only
by the driver, as the owner DID. `state` is derived; it is not a second request
lifecycle and no runtime reconciles it. `Reverted` is terminal. The collection
stays out of the `Collection` enum, out of `BRANCHABLE_COLLECTION_NAMES`, and
out of all three P2P arrays in `agent/p2p_reconcile/templates.rs`. The frozen
closure excludes `EvalDefinition`, which the job freezes separately.

**Deletions.** The string literal `"OptimizationJob"` in
`PROTECTED_DATASTORE_COLLECTIONS`, replaced by
`gents_protocol::schemas::OPTIMIZATION_JOB_NAME`, with the doc comment that
asked for the replacement removed.

**Validation.** `cargo test -p gents-schemas`, `-p gents-protocol`,
`-p gents-migration`; `cargo test -p gents --lib optimization`;
`cargo test -p gents-desktop-core the_desktop_does_not_replicate_plaintext_provider_bodies`;
`cargo check --workspace --all-targets`; `cargo fmt --all --check`.

🤖 Generated with [Claude Code](https://claude.com/claude-code)

## PR 2 — `optimization/21-proposer` → `optimization/20-job`

**Baseline.** PR 1's head.

**What it adds.** `MaterializedPack` and the candidate materializer (sidecar
and inline prompts), the `Proposer` trait with `ScriptedProposer`, and the pure
structural gate.

**Owners.** The proposer holds no `ConfigAccess`; `target.rs` and `subject.rs`
are the only builders of a patch. `ProposalInput`'s field list is pinned by a
test, because it is the statement of what an optimizer may learn. The gate
proves the candidate differs from the baseline only at the subject context's
`system_prompt` and holds exactly the proposed text, and validates the pack's
own reference closure; the live form of that check runs inside `promote`.

**Deletions.** None.

**Validation.** `cargo test -p gents --lib optimization`;
`cargo check --workspace --all-targets`; `cargo fmt --all --check`.

🤖 Generated with [Claude Code](https://claude.com/claude-code)

## PR 3 — `optimization/22-driver` → `optimization/21-proposer`

**Baseline.** PR 2's head.

**What it adds.** A new, unfrozen `gents::eval::report` module with the
verdict-to-evidence projection (latest attempt per `(run, case, trial_index)`
slot; runs paired separately and their pairs concatenated), the optimization
decision evidence, the eval-run plans and budget, `run_job`, `show`, and the
scripted matrix covering every end-to-end case spec section 9 lists.

**Owners.** `eval::report` is the projection's owner; M4's report consumes it
rather than re-deriving it, and nothing new enters the frozen `eval::scoring`.
The driver is the sole writer of the job and decides only from its frozen
origin. Runs are ordinary `EvalRun`s with `purpose: "optimization:<job_id>"`;
grading, seeding, resume and the NotEvidence breaker stay the runner's. Job
directories live under `<launching home>/eval/jobs/<job_id>/`; M4's
`gents optimization rm` owns their removal.

**Deletions.** None.

**Validation.** `cargo test -p gents --lib optimization`;
`cargo test -p gents --lib eval`; `cargo check --workspace --all-targets`;
`cargo fmt --all --check`.

🤖 Generated with [Claude Code](https://claude.com/claude-code)

## PR 4 — `optimization/23-promote` → `optimization/22-driver`

**Baseline.** PR 3's head.

**What it adds.** `promote(access, owner, job_id, digest, by)` and
`revert(access, owner, job_id, digest, by)` over the Track 0 compare-and-set,
the refusal matrix (stale closure, moved target, foreign DID, wrong digest,
unready job, a second promotion after a revert), the test that the
model-facing `config` tool has no optimization surface, and the two live
scenarios, both `#[ignore]` until M5.

**Owners.** Promotion expects the entire frozen closure and writes only the
target document, through `DesiredStateApplyPlan::with_expected`. There is no
force flag, nothing reverts on its own, and a revert ends the job. `by` is the
caller-stated launching-home DID compared against the job's owner; the enforced
identity boundary is spec 2b's. This is the first non-test caller of
`with_expected`. The `gents optimization show | promote | revert` CLI wrappers
are M4's.

**Deletions.** `live_provider_from_env` moves out of
`crates/gents/tests/eval_runner_canary.rs` into
`crates/gents/tests/support/live_inference.rs`; the canary keeps a call, not a
copy.

**Validation.** `cargo test -p gents --lib optimization`;
`cargo test -p gents --test optimization_live` (2 ignored);
`cargo test -p gents --test eval_runner_canary`; `cargo test -p gents`;
`cargo check --workspace --all-targets`; `cargo fmt --all --check`.

🤖 Generated with [Claude Code](https://claude.com/claude-code)
````

- [ ] **Step 2: Commit**

```bash
git add .superpowers/sdd/2026-09-22-optimization-driver/pr-descriptions.md
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" \
  commit -m "docs(sdd): M6b pull request descriptions

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

### PR 4 gate, and the integrated stack

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents 2>&1 | tee /tmp/pr4-full.log; grep -E "^(test result|failures:)" /tmp/pr4-full.log`
Expected: every suite passes except the two known libp2p dial timeouts
(`generated_r5_cross_principal_cases_drive_production_dispatch` and
`e2e_triggers::event_source_trigger_p2p_e2e::p2p_replicated_doc_fires_event_trigger`),
which fail on unmodified `main` on this machine and are not attributable here.

Run: `CARGO_BUILD_JOBS=4 cargo check --workspace --all-targets 2>&1 | tee /tmp/pr4-check.log; grep -c "^error" /tmp/pr4-check.log`
Expected: 0.

Run: `cargo fmt --all --check`
Expected: exits 0.

No `lake build` is required: this plan adds and edits no Lean. The whole stack is validated on PR 4's head, and the stack merges in order, PR 1 first; the non-buildable base must not merge alone.

---

## Self-review

Re-run after applying rulings R1–R9 and fixes F1–F12.

### 1. Spec coverage

| Spec section | Requirement | Where |
|---|---|---|
| 1 | Baseline subject is a pack snapshot naming `behavior_id`; no credentials | PR 2 Task 1 `materialize_pack`; operator-supplied and cross-checked against the live closure at freeze (R5, PR 3 Task 4 `freeze_job`); inference rides on the profile the `CellRequest` names |
| 1 | Target is `AgentContext.system_prompt`; `TargetField` allows nothing else | PR 1 Task 3, one variant (R2; spec amended) |
| 1 | Candidate is the same snapshot with one field patched, giving a new pack digest; the digest is the dedup key | PR 2 Task 1 `materialize_candidate`; PR 2 Task 3 `duplicate_candidate` |
| 1 | Frozen closure is `(collection, owner, id, digest)` read through `ConfigReferences::load_in_txn` | PR 1 Task 3 `capture_closure`, `closure_digests`, `FrozenDocument`; excludes `EvalDefinition` (F5) |
| 2 | `Proposer` trait, `ProposalInput`, `Proposal`; the proposer never holds `ConfigAccess`; `target.rs` builds the patch | PR 2 Task 2; Global Constraint 11 |
| 2 | The first implementation is scripted | PR 2 Task 2 `ScriptedProposer` |
| 3.1 | Train run: one cell, the checkpoint, `purpose: "optimization:<job_id>"` | PR 3 Task 3 `train_plan`, `run_request` |
| 3.2 | Propose returns a text and a rationale | PR 3 Task 4, `JournalEntry::Proposed` |
| 3.3 | Structural gate before any validation spend: only the target field moved; closure validates; length cap; digest novelty; failures journaled with diagnostics | PR 2 Task 3 (F6: exact in sidecar and inline packs); PR 3 Task 4; matrix `a_repeated_candidate_is_structurally_rejected_and_costs_no_validation_run`. The Task placeholder-set check is removed with the Task target (R2) |
| 3.4 | Validation run: two cells, shared seeds | PR 3 Task 3 `validation_plan`; one `seed_base` per run |
| 3.5 | Decide from `EvalVerdict` rows; on `Inconclusive` re-run with a new `seed_base`; re-runs add pairs | PR 3 Tasks 1 and 2 (`concat_paired`, `decision_evidence`, F1); matrix `an_inconclusive_round_is_rerun_on_a_new_seed_and_ends_inconclusive` asserts 4 pairs per case over two runs |
| 3.6 | Accept makes the candidate the checkpoint; the job returns the retained checkpoint | PR 1 Task 4 `checkpoint`, tested "never the best seen" |
| 3 | Rejection memory is the journal; `Inconclusive` and budget-exhausted rounds are never negatives | PR 3 Task 4 `rejections` |
| 4 | `decide` pure; `PolicyV2` frozen into the job | M6a; PR 1 Task 4 `JobOrigin::policy`; F2 `check_resume` |
| 4 | `alpha_effective = alpha / max_rounds` | M6a `alpha_effective_ppm`; `check_policy` binds the divisor to the budget; matrix runs at `max_rounds: 3` (F11) |
| 4 | Cost sub-gate, skipped and journaled when usage is missing | PR 3 Task 2 `token_totals`; `DecisionSummary::cost_skipped`; matrix `a_candidate_that_costs_ten_times_the_tokens_is_rejected_for_cost` |
| 4 | `Improve` on validation, `Confirm` on held-out | PR 3 Task 4 |
| 5 | Local-only `OptimizationJob`: frozen `origin`, length-guarded `journal`, derived `state` not named `lifecycle_state` | PR 1 Tasks 1 and 4 |
| 5 | In `LOCAL_AUDIT_COLLECTION_NAMES`; out of `Collection` and every P2P list; no `DatastoreToolSurface` may name it | PR 1 Task 1 Steps 4 and 9; PR 1 Task 2 |
| 5 | `origin` freezes target, closure digests, subject digest and `behavior_id`, definition ref, policy, `trials_per_case`, budgets, owner DID | PR 1 Task 4 `JobOrigin` (plus `seed_base`, `inference_profile_id`, `max_text_bytes`, `jobs_dir` per F2 and R6) |
| 5 | The ten journal entries | PR 1 Task 4 `JournalEntry` |
| 5 | `summary` is a convenience; `show` recomputes every decision and flags mismatches and invalidated runs | PR 3 Task 6 `show` (R1) and its two tests |
| 6 | The state diagram, with `Reverted` terminal | PR 1 Task 4 `JobState`, `derive_state` (R4); PR 3 Task 4 |
| 6 | Interruption: replay the journal; a `RunStarted` without a `Decided` resumes that run; resumed and uninterrupted jobs reach the same state | PR 3 Tasks 3 and 4 (`execute_run`, `round_is_closed`, `proposed_for`, F7 staging); matrix `a_resumed_job_reaches_the_journal_of_its_uninterrupted_twin` (R9) |
| 6 | Baseline drift → `Failed(baseline_drifted)` before further spend | PR 3 Task 4; matrix `a_baseline_edited_after_the_freeze_fails_the_job_before_any_further_spend` (F3) |
| 6 | Definition drift → `Failed(definition_changed)` | PR 3 Task 4, checked first (F5); matrix `a_definition_edited_after_the_freeze_fails_the_job_as_definition_changed` |
| 6 | Budgets checked before every run; held-out reserved at freeze; an unevaluated candidate is `BudgetExhausted` | PR 3 Tasks 3 and 4; matrix `a_round_that_cannot_be_afforded_is_budget_exhausted_and_spends_nothing` |
| 6 | Finalization: one held-out `Confirm` run; `ReadyToPromote` / `Failed(held_out_*)`; held-out never touched when nothing accepted | PR 3 Task 4; matrix accept (exactly one held-out run), `no_improvement` (none), `a_checkpoint_that_regresses_on_held_out_fails_the_job` |
| 6 | The driver is the only writer, as the owner DID | PR 3 Task 4; every append goes through `job::append` |
| 7 | `promote`: `ReadyToPromote`, the digest, a re-read closure, a rebuilt patch, write only the target expecting the whole closure, journal `Promoted` | PR 4 Task 1 (R6 signature) |
| 7 | On drift: rollback, a second transaction journals `PromotionRefused`, the job is `Stale`; no force flag; owner DID only | PR 4 Task 1 and its tests |
| 7 | `revert` writes `previous_text` back through the same compare-and-set, expecting what was promoted | PR 4 Task 1 `revert`, terminal (R4) |
| 8 | Lean unchanged in M6b | Global Constraint 9 |
| 9 | Unit: permutation test, too-few-cases, Bonferroni, cost gate with the missing-usage skip, totality | M6a's 11 policy tests |
| 9 | End to end, deterministic, in `cargo test -p gents`: accept; `no_improvement`; `case_regression`; `cost_regression`; structural reject with zero validation runs; inconclusive → re-run → inconclusive; too few cases; budget exhaustion; held-out regression; held-out only at finalize; feedback only from train; baseline drift; definition drift; resume at interruption; decision on an invalidated run | PR 3 Task 5 (13 tests) and Task 6 (`a_decision_on_an_invalidated_run_is_flagged`) — every case (F9) |
| 9 | Promotion: once; stale closure refused with the live node unchanged; edited target refused with the user's edit preserved; foreign DID, wrong digest, unready job refused; revert restores and is refused if the target moved | PR 4 Task 1's five async tests |
| 9 | The model-facing `config` tool exposes no optimization resource or verb | PR 4 Task 1 (scans every file of the `self_config` directory module, F11); PR 1 Task 2 for the datastore surface |
| 9 | Live, never in CI: one accept and one reject on `monitor-findings` | PR 4 Task 2, `#[ignore]`, run in M5 (R7) |

### 2. Placeholder scan

Searched for "TBD", "TODO", "implement later", "fill in details", "add appropriate error handling", "handle edge cases", "write tests for the above" and "similar to Task": none. Every code step carries a code block. Two values are read at execution time, each with the command that produces it: the `OptimizationJob` migration pin (PR 1 Task 1 Step 7, from `canonical_catalog_pins_for_authoring`), and the M6b base commit (supplied by the orchestrator; PR 1 Task 1 Step 0 verifies its contents). The M3 pack path and definition id in PR 4 Task 2 are runtime inputs to an ignored test, not plan gaps.

Consumed signatures re-verified against the code for this revision: `crate::pack::load_pack_config(manifest, options, read_asset, environment)` is `pub` in `pack/loader.rs` and re-exported from `pack.rs:15` (the earlier plan said `pub(crate)` with other parameter names); the loader resolves a `./` `system_prompt` sidecar (`pack/loader.rs:105-107`); a slot's first attempt is `1` (`eval/runner/plan.rs`); a pre-cancelled token skips every queued slot (`eval/runner/mod.rs`, the biased select setting `stop`); `completion` copies `evidence.usage` into `TrialCompletion::usage` (`eval/runner/mod.rs`, `fn completion`), so scripted usage reaches the cost gate; `RunOptions::default()` backs off 5 s, which is why the matrix passes 1 ms; `self_config` is a directory module whose `mod.rs` declares `SELF_CONFIG_TOOL_NAMES: [&str; 6]`, and no file in it mentions `optimization`, `promote` or `revert`; `PackConfig::eval_definitions` exists (`pack_config.rs:197`); `invalidate_run(access, owner, run_id, by, reason)`.

### 3. Type consistency

- `TargetField` has one variant everywhere: `target.rs`, `JobOrigin::target`, `freeze_job`, the gate no longer takes a `Target`.
- `JobOrigin` gains `inference_profile_id`, `max_text_bytes` and `jobs_dir`; they are set in `freeze_job`, compared in `check_resume`, read by `run_request`, `run_job` and `promote`, and present in every test fixture (`job.rs` and `driver.rs` tests).
- `JobState::Reverted` is produced by `derive_state` and asserted in `job.rs`'s test and in `a_revert_restores_the_previous_text_and_is_terminal`; `label()` covers it.
- `RunRows` (Task 1) is the only run-rows type; `decision_evidence`, `run_job` and `show` all take `&[RunRows]` loaded by `load_run_rows`.
- `BASELINE_CELL`/`CANDIDATE_CELL` are defined once in `optimization/evidence.rs` and used by the plans, `decision_evidence`, and the matrix scripts (`"baseline"`/`"candidate"`, because `run_request` sets the label to the cell id).
- `structural_gate(baseline, candidate, text, max_text_bytes, seen_digests, owner)` has the same six parameters in PR 2 Task 3 and in `run_job`.
- `promote`/`revert` take `(access, owner, job_id, digest, by)` in PR 4 Task 1, its tests, and the PR description.
- `run_job(access, request, executor, proposer, registry, policy, cancel)` has the same order in PR 3 Task 4, the matrix's `drive`, and the live fixture.
- `JobRequest::run_options` is set by the matrix (1 ms), the live fixture (`RunOptions::default()`) and the driver-test fixture.
- Task counts in the "Expected" lines were recomputed: target 5, job 5, subject 4, proposer 3, gate 9, report 6, optimization evidence 4, driver 9, matrix 13, show 2, promote 6.

---

## Plan defects I could not resolve

After rulings R1–R9 one item remains, and it is a dependency rather than a gap in the plan:

1. **The live scenarios need M3's pack.** PR 4 Task 2 reads the `monitor-findings` definition and the golden monitor subject from the pack named by `GENTS_OPTIMIZATION_LIVE_PACK` (M3's `eval/30-monitor-prework`). Per ruling R7 the tests are `#[ignore]` and run in M5, so M6b compiles and passes without that branch; nobody can run the accept or the reject until M3's pack exists alongside M6b. The test also expects that pack to keep its prompt in a sidecar asset and to declare the definition in `eval_definitions`; if M3 lands a different shape, the fixture's two `expect` lines say which assumption failed.
