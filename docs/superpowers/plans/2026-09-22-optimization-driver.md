# Optimization Driver (M6b) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land the impure half of `gents::optimization`: an `OptimizationJob` journal document, a `Proposer` seam with a scripted implementation, a pure structural gate, a driver that runs rounds of native `EvalRun`s and decides them with `PolicyV2`, and operator-only `promote`/`revert` through the Track 0 compare-and-set.

**Architecture:** Four stacked PRs over one rebased base. PR 1 adds the `OptimizationJob` collection and the typed, length-guarded journal that refines `Optimization.appendIf`. PR 2 adds the pure candidate machinery: the closure target, the candidate pack materializer, the `Proposer` trait with `ScriptedProposer`, and the structural gate. PR 3 adds the driver: it freezes a baseline, runs a train `EvalRun`, asks the proposer, gates the candidate, runs a two-cell validation `EvalRun` on shared seeds, projects verdicts into `Evidence`, calls `decide`, journals the result, and finalizes on the held-out split. PR 4 adds `promote` and `revert` over `DesiredStateApplyPlan::with_expected`, and one live accept plus one live reject, both ignored by default. No Lean is added.

**Tech Stack:** Rust 1.97.1, tokio, `async_trait`, DefraDB via `gents::defra_node::EmbeddedNode`, `serde_json`, `sha2`, `tempfile`, `tokio_util::sync::CancellationToken`.

**Spec:** `docs/superpowers/specs/2026-09-21-optimization-on-eval-design.md` (all sections approved 2026-09-21). Umbrella: `2026-09-21-eval-and-optimization-umbrella.md` sections 4 to 7, milestone M6. Contract: `2026-09-21-eval-core-contract-design.md`. This plan covers everything in spec 5 that M6a did not: M6a delivered `Proofs/Optimization.lean`, its conformance emitter, and `PolicyV2`, and none of that is reopened here.

**Depends on:** the M6b base, defined under "The M6b base" below. This plan runs under `docs/superpowers/orchestration/2026-09-21-parallel-coordinators.md`.

---

## The M6b base

M6b consumes four landed branches that do not share a tip. Build the base once, before PR 1, in the coordinator's worktree:

| Branch | Tip | What M6b needs from it |
|---|---|---|
| `eval/13-runner-embedded` | `028aa4188` | `gents::eval::runner`: `run`, `resume`, `RunRequest`, `CellSource`, `ScriptedExecutor`, `CheckRegistry` |
| `eval/06-protected` | `9c8a590e0` | `PROTECTED_DATASTORE_COLLECTIONS` with the `"OptimizationJob"` reserved literal PR 1 replaces |
| `optimization/01-lean` → `02-conformance` → `03-cas` | `f3ebf6b9f` | `DesiredStateApplyPlan::with_expected`, `DesiredStateExpectation`, `StaleExpectation`, `stale_expectation` |
| `optimization/10-lean` → `11-conformance` → `12-policy` | `d0ab985ab` | `PolicyV2`, `Evidence`, `decide`, `evidence_from_pairs`, `Mode`, `DecisionReport` |

`eval/13-runner-embedded`, `eval/06-protected` and `optimization/10..12` all share `eval/05-contract` at `851db85d6`. `optimization/01..03` shares `main` at `0deb7659c`, which is an ancestor of `eval/05-contract` (verified: `git merge-base --is-ancestor 0deb7659c eval/05-contract` succeeds). So every branch rebases onto a descendant of its own base and no rebase replays work onto an unrelated tree.

**Build it by rebasing, not merging**, in this order, from the main checkout:

```bash
git rebase --onto eval/13-runner-embedded 851db85d6 eval/06-protected
git rebase --onto eval/06-protected 0deb7659c optimization/03-cas
git rebase --onto optimization/03-cas 851db85d6 optimization/12-policy
```

(`optimization/03-cas` and `optimization/12-policy` are the tips of their own three- and two-commit stacks; rebasing the tip carries the stack, because each stack is linear.)

**Why rebase and not merge.** These four lines become four GitHub pull requests that each target their immediate parent, per CLAUDE.md's stacked-PR rule. A merge commit gives a PR two parents, so GitHub's diff against the parent branch would carry the merge resolution into the descendant's review; a rebase keeps every PR a single-parent, reviewable range. It also keeps `git log --oneline optimization/23-promote --not <parent>` an exact statement of what each PR adds, which is how each PR description's "baseline" line is written.

**"The M6b base" is the resulting `optimization/12-policy` tip.** Record its commit in the ledger at first use; every branch in this plan bases on it or on its predecessor in the stack.

Two facts that this base makes true and that tasks below rely on:

- `crates/gents/src/document_config/write_tool.rs` contains `PROTECTED_DATASTORE_COLLECTIONS` with the literal `"OptimizationJob"` and the doc comment "replace it with the schema constant there". Without `eval/06-protected` in the base that literal does not exist anywhere in the tree (verified: `git grep OptimizationJob` is empty on `eval/13-runner-embedded`), and PR 1 Task 2 would have nothing to replace.
- `crates/gents/src/config_client/desired_state.rs` contains `DesiredStateApplyPlan::with_expected`. Without `optimization/03-cas` in the base it does not exist (verified: `git grep with_expected 028aa4188 -- crates/gents/src/config_client/desired_state.rs` is empty), and PR 4 could not be written at all.

## Deviations from the spec and from the brief, decided at planning

- **`evidence_from_paired` is spelled `evidence_from_pairs`.** `crates/gents/src/optimization/mod.rs` on `optimization/12-policy` exports `evidence_from_pairs`; there is no `evidence_from_paired`. Every task below uses the name the code has.
- **`AgentContext` lives in `crates/gents/src/document_config/context.rs`, not `agent_context.rs`.** Verified by reading the file: the struct is declared at `context.rs:10-55` and `system_prompt` is `Option<String>`.
- **The structural gate's `validate_desired_state_plan` check becomes a pure pack-closure check.** The spec's step 3 says "`validate_desired_state_plan` passes on the patched closure". That function is `pub(crate) async fn validate_desired_state_plan(txn: &ConfigApplyTxn<'_>, plan: &DesiredStateApplyPlan) -> Result<()>` (`desired_state.rs:233`): it needs a transaction, and it validates the *operator's* closure, which a candidate never touches until promotion. A candidate is evaluated as a pack in a throwaway trial home. So the gate validates the candidate's own reference closure purely, with `DesiredStateApplyPlan::from_pack_config` plus `ConfigReferences::from_documents(...).validate()` — the same rule `validate_desired_state_plan` applies, minus the live read. The operator-closure form of the check runs where it belongs: inside `promote`'s transaction in PR 4.
- **`TargetField` ships one variant in M6b.** The brief fixes every candidate as "the same pack with ONE field changed (`AgentContext.system_prompt` of the subject behavior's context)". `TargetField::TaskPromptTemplate` and its placeholder-set check are therefore not implemented here; the enum is declared with both variants so the frozen `origin` wire form does not change later, and the Task variant returns a `StructuralRejection` naming it unsupported. Recorded again under "Plan defects I could not resolve".
- **`run_job` takes a `&CheckRegistry`.** The brief's signature is `run_job(access, request, executor, proposer, policy, cancel)`. `gents::eval::runner::run` requires `registry: &CheckRegistry` (`runner/mod.rs:116-127`) and `CheckRegistry` is neither `Clone` nor serializable, so it cannot ride on the frozen `JobRequest`. It is added as a parameter beside `executor`.
- **"Stale baseline on promote" is split across PR 3 and PR 4.** The brief lists it in PR 3's matrix, but a promote-time `StaleExpectation` needs `promote`, which PR 4 introduces. PR 3's matrix therefore covers *stale baseline at job start* (`Failed(baseline_drifted)`, spec section 6 "Baseline drift"), and PR 4's matrix covers *stale closure at promote* (`PromotionRefused`, spec section 7). Both cases are tested; neither is dropped.
- **The scripted matrix is a library test, not an integration test.** The canary at `crates/gents/tests/eval_runner_canary.rs` is the shape being copied, but the matrix needs `CheckRegistry::with` (`checks/mod.rs:72`, `#[cfg(test)] pub(crate)`) to register a feedback-emitting check, and it reuses the `Launching` harness at `crates/gents/src/eval/runner/freeze.rs:839-985` (`pub(crate) mod tests`). Both are crate-internal, so the matrix lives at `crates/gents/src/optimization/driver/matrix.rs` under `#[cfg(test)]`. It still runs under `cargo test -p gents`, which is what spec section 9 requires. The eval runner's own end-to-end loop tests sit in the library for the same reason (`runner/mod.rs:602`).
- **The verdict-to-score projection is owned here and flagged for M4.** Nothing in `gents::eval` turns `VerdictRecord` rows into `TrialScore` values: `scoring::case_trial_score` takes `VerdictView`, which carries `stage_index` while `VerdictRecord` carries `stage_id`, and the latest-attempt-per-slot selection the umbrella fixes for M4 (section 7) has no implementation. PR 3 Task 1 writes it as `gents::optimization::evidence`. See "Plan defects I could not resolve" for the handoff.
- **`gents optimization show | promote | revert` CLI wrappers are not in this plan.** The brief scopes M6b to the library. PR 4 ships `promote`/`revert` as library functions with the whole refusal matrix; `show` ships as `recompute_decisions`, the library half of the "recompute every decision from the `EvalVerdict` rows and flag a mismatch" guarantee. The CLI is spec 4a's. Recorded under defects.

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

9. **No Lean is edited.** `Proofs/Optimization.lean` and `Proofs/Conformance/Optimization.lean` are complete for M6b. The conformance emitter `optimizationCasesJson` emits `params`, `decisions`, `gates` and `costs` only — it emits **no journal cases** (verified by reading `Proofs/Conformance/Optimization.lean:83-94`), so no journal conformance consumer is written. The Rust journal refines the Lean model through the ordinary unit test in PR 1 Task 3, which reproduces `appendIf_match`, `appendIf_stale_unchanged` and `appendIf_prefix` over the real document. If an implementer believes a Lean change is needed, stop and message the orchestrator.
10. **One field, one pack.** Every candidate is the baseline subject pack with exactly one field changed: `AgentContext.system_prompt` of the context the subject behavior names. It is materialized as a directory pack under the job's directory and handed to the runner as `CellSource::Directory(<job dir>/rounds/<round>/candidate)`. No other candidate shape exists.
11. **The proposer holds no `ConfigAccess`.** `Proposer::propose` takes a `ProposalInput` value and returns a `Proposal` value. The trait's signature admits no node, no transaction and no path. `target.rs` and `subject.rs` build the patch deterministically from the returned text.
12. **Feedback is read only from the train run's verdicts.** The driver reads `feedback` from the `EvalVerdict` rows of the round's train run and from nowhere else. The runner already nulls `feedback` off the train split (`runner/mod.rs:529-534`) and `append_verdict` refuses it there (`documents.rs:525-527`); this constraint is the driver-side half of the same rule.
13. **Nothing protected reaches the proposer.** `ProposalInput` carries the current target text, per-check feedback strings and scores, and the rejection history. It carries no `case_id`, no stage prompt, no check params, no tier or split label, no `raw` verdict payload and no other case's body. A test asserts the type has no such field.
14. **`promote` uses `with_expected` on the baseline context's digest and refuses on `StaleExpectation`.** The plan expects the entire frozen closure and writes only the target document. There is no force flag. `revert` restores `previous_text` the same way, expecting the digest `promote` journaled.
15. **rustfmt is a CI gate.** `cargo fmt --all --check` exits 0 before every commit. Where a task says "add a module", place the `mod` line where rustfmt sorts it.
16. **No `unwrap`/`expect` on input-dependent paths outside tests.**
17. `OptimizationJob` stays out of the `Collection` enum, out of `BRANCHABLE_COLLECTION_NAMES`, and out of `CONVERSATION_COLLECTIONS`, `CLIENT_COLLECTIONS` and `CLIENT_TO_RUNTIME_COLLECTIONS` in `crates/gents/src/agent/p2p_reconcile/templates.rs`. It goes into `LOCAL_AUDIT_COLLECTION_NAMES`, because its journal holds candidate prompts.

## File Structure

| File | PR | Responsibility |
|---|---|---|
| `crates/gents-schemas/schemas/agent/optimization_job.graphql` | 1 | The SDL: frozen `origin`, append-only `journal`, guard field `journal_len`, derived `state` |
| `crates/gents-schemas/src/lib.rs` | 1 | `OPTIMIZATION_JOB{,_NAME}`, `ALL`, `ALL_COLLECTION_NAMES`, `LOCAL_AUDIT_COLLECTION_NAMES`, classification test |
| `crates/gents-protocol/src/schemas.rs` | 1 | The mirror: re-export plus `ALL` and `ALL_COLLECTION_NAMES` |
| `crates/gents-migration/src/registry.rs` | 1 | The `DEFAULT_BASELINE` pin |
| `crates/gents/src/document_config/write_tool.rs` | 1 | Replace the `"OptimizationJob"` literal with the schema constant |
| `crates/gents/src/optimization/job.rs` | 1 | `JobOrigin`, `Budgets`, `JournalEntry`, `JobState`, `JobRecord`, `derive_state`, `checkpoint`, `create_job`, `load_job`, `load_job_in_txn`, `append`, `append_in_txn`, `JournalConflict` |
| `crates/gents/src/optimization/target.rs` | 1 | `TargetField`, `Target`, `Closure`, `FrozenDocument`, `capture_closure`, `closure_digests`, `current_text`, `apply_text`, `target_digest`, `expectations`, `target_plan` |
| `crates/gents/src/optimization/subject.rs` | 2 | `MaterializedPack`, `materialize_pack`, `materialize_candidate`, `prompt_asset` |
| `crates/gents/src/optimization/proposer.rs` | 2 | `ProposalInput`, `CheckFeedback`, `Rejection`, `Proposal`, `Proposer`, `ScriptedProposer` |
| `crates/gents/src/optimization/gate.rs` | 2 | `StructuralRejection`, `structural_gate` |
| `crates/gents/src/optimization/evidence.rs` | 3 | `cell_trial_scores`, `token_totals`, `train_feedback`, `decision_seed`, `recompute_decisions` |
| `crates/gents/src/optimization/driver.rs` | 3 | `JobRequest`, `JobOutcome`, `Checkpoint`, `JobRefused`, `run_job`, round loop, finalization |
| `crates/gents/src/optimization/driver/matrix.rs` | 3 | The scripted end-to-end matrix |
| `crates/gents/src/optimization/promote.rs` | 4 | `promote`, `revert`, `Promotion`, `PromoteRefused`, `promote_refused` |
| `crates/gents/tests/optimization_live.rs` | 4 | One live accept and one live reject, both `#[ignore]` |
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
  pub enum TargetField { AgentContextSystemPrompt, TaskPromptTemplate }
  impl TargetField {
      pub fn collection(&self) -> Collection;
      pub fn field_name(&self) -> &'static str;
  }
  pub struct Target { pub field: TargetField, pub owner: String, pub id: String }
  pub struct FrozenDocument { pub collection: Collection, pub owner: String, pub id: String, pub digest: String }
  pub type Closure = Vec<(Collection, Value)>;
  pub async fn capture_closure(txn: &ConfigApplyTxn<'_>, owner: &str) -> Result<Closure>;
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

    #[test]
    fn the_field_names_are_the_document_fields_the_spec_allows() {
        assert_eq!(
            TargetField::AgentContextSystemPrompt.collection(),
            Collection::AgentContext
        );
        assert_eq!(
            TargetField::AgentContextSystemPrompt.field_name(),
            "system_prompt"
        );
        assert_eq!(TargetField::TaskPromptTemplate.collection(), Collection::Task);
        assert_eq!(TargetField::TaskPromptTemplate.field_name(), "prompt_template");
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

/// The only fields an optimization job may change. `TaskPromptTemplate` is
/// declared so the frozen wire form does not change when it is implemented;
/// M6b evaluates only [`Self::AgentContextSystemPrompt`], and the structural
/// gate refuses the other variant by name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetField {
    AgentContextSystemPrompt,
    TaskPromptTemplate,
}

impl TargetField {
    pub fn collection(&self) -> Collection {
        match self {
            Self::AgentContextSystemPrompt => Collection::AgentContext,
            Self::TaskPromptTemplate => Collection::Task,
        }
    }

    pub fn field_name(&self) -> &'static str {
        match self {
            Self::AgentContextSystemPrompt => "system_prompt",
            Self::TaskPromptTemplate => "prompt_template",
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

/// Read the owner's whole configuration inside `txn`, ordered so two reads of
/// the same state produce the same closure.
pub async fn capture_closure(txn: &ConfigApplyTxn<'_>, owner: &str) -> Result<Closure> {
    let references = crate::ConfigReferences::load_in_txn(txn, owner).await?;
    let mut closure: Closure = references
        .documents()
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
Expected: PASS, 4 tests.

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
      pub budgets: Budgets, pub baseline_text: String, pub owner: String, pub seed_base: i64 }
  pub enum JobState { Running, ReadyToPromote, NothingToPromote, Exhausted,
      Failed { reason: String }, Promoted, Stale }
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
        assert_eq!(derive_state(&journal), JobState::ReadyToPromote, "a revert restores the offer");
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
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriftedRef {
    pub collection: String,
    pub id: String,
}

/// The convenience numbers behind a decision. The authority is the referenced
/// runs' `EvalVerdict` rows; `optimization::evidence::recompute_decisions`
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
            // A revert returns the job to the offer it was before the promote:
            // the checkpoint still stands and an operator may promote again.
            JournalEntry::Reverted { .. } => state = JobState::ReadyToPromote,
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
  pub(crate) fn load_pack_config(
      manifest: &PackManifest,
      options: &PackInstallOptions,
      asset: &dyn Fn(&str) -> Result<Vec<u8>>,
      env: &dyn Fn(&str) -> Option<String>,
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
  The exact parameter list of `load_pack_config` is read from its one existing call site, `crates/gents/src/eval/runner/freeze.rs` in `fn load_pack`, inside the `CellSource::Directory` arm. Copy that call verbatim.
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
  ```

- [ ] **Step 1: Write the failing test** — create `crates/gents/src/optimization/subject.rs` holding only this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const OWNER: &str = "did:key:subject-owner";
    const PROMPT: &str = "Watch the mailbox.\n";

    /// The shape of the eval runner's own fixture pack: a manifest, a README,
    /// the canonical config bundle and one behavior sidecar.
    fn write_pack(root: &Path) {
        std::fs::create_dir_all(root.join("agent_behaviors/monitor")).unwrap();
        std::fs::write(root.join("README.md"), "# monitor fixture\n").unwrap();
        std::fs::write(root.join("agent_behaviors/monitor/system_prompt.md"), PROMPT).unwrap();
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
        write_pack(&baseline_dir);
        let baseline = materialize_pack(&baseline_dir, OWNER, "monitor").unwrap();

        assert_eq!(baseline.context_id, "monitor-context");
        assert_eq!(
            baseline.prompt_asset.as_deref(),
            Some("agent_behaviors/monitor/system_prompt.md")
        );
        assert_eq!(baseline_text(&baseline).unwrap(), PROMPT);
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
        write_pack(&baseline_dir);
        let baseline = materialize_pack(&baseline_dir, OWNER, "monitor").unwrap();

        let one = materialize_candidate(&baseline, OWNER, "New text.\n", &dirs.path().join("one")).unwrap();
        let two = materialize_candidate(&baseline, OWNER, "New text.\n", &dirs.path().join("two")).unwrap();
        assert_eq!(one.digest, two.digest, "the digest is over content, not location");

        let same = materialize_candidate(&baseline, OWNER, PROMPT, &dirs.path().join("same")).unwrap();
        assert_eq!(
            same.digest, baseline.digest,
            "rewriting the prompt with its own text is the baseline pack"
        );
    }

    #[test]
    fn a_behavior_the_pack_does_not_declare_is_an_error() {
        let dirs = tempfile::tempdir().unwrap();
        let baseline_dir = dirs.path().join("baseline");
        write_pack(&baseline_dir);
        let error = materialize_pack(&baseline_dir, OWNER, "no-such-behavior").unwrap_err();
        assert!(
            format!("{error:#}").contains("no-such-behavior"),
            "{error:#}"
        );
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
Expected: PASS, 3 tests.

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

**Interfaces:**
- Consumes:
  ```rust
  // crates/gents/src/optimization/subject.rs, PR 2 Task 1
  pub struct MaterializedPack { pub dir: PathBuf, pub digest: String, pub config: PackConfig,
      pub manifest: PackManifest, pub files: BTreeMap<String, Vec<u8>>,
      pub context_id: String, pub prompt_asset: Option<String> }
  // crates/gents/src/optimization/target.rs, PR 1 Task 3
  pub enum TargetField { AgentContextSystemPrompt, TaskPromptTemplate }
  pub struct Target { pub field: TargetField, pub owner: String, pub id: String }
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
      target: &Target,
      text: &str,
      max_text_bytes: usize,
      seen_digests: &[String],
      owner: &str,
  ) -> Result<(), StructuralRejection>;
  ```

- [ ] **Step 1: Write the failing test** — create `crates/gents/src/optimization/gate.rs` holding only this test module. It reuses the fixture writer from Task 1's tests through a small crate-internal helper, so the fixture exists in one place:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::optimization::subject::tests::{write_fixture_pack, FIXTURE_PROMPT};
    use crate::optimization::subject::{materialize_candidate, materialize_pack};
    use crate::optimization::target::{Target, TargetField};

    const OWNER: &str = "did:key:gate-owner";

    fn target() -> Target {
        Target {
            field: TargetField::AgentContextSystemPrompt,
            owner: OWNER.into(),
            id: "monitor-context".into(),
        }
    }

    struct Fixture {
        _dirs: tempfile::TempDir,
        root: std::path::PathBuf,
        baseline: MaterializedPack,
    }

    fn fixture() -> Fixture {
        let dirs = tempfile::tempdir().unwrap();
        let root = dirs.path().to_path_buf();
        write_fixture_pack(&root.join("baseline"));
        let baseline = materialize_pack(&root.join("baseline"), OWNER, "monitor").unwrap();
        Fixture {
            _dirs: dirs,
            root,
            baseline,
        }
    }

    #[test]
    fn a_one_field_candidate_of_a_new_digest_passes() {
        let fixture = fixture();
        let text = "Watch the mailbox, and say why.\n";
        let candidate =
            materialize_candidate(&fixture.baseline, OWNER, text, &fixture.root.join("c1")).unwrap();
        structural_gate(
            &fixture.baseline,
            &candidate,
            &target(),
            text,
            32 * 1024,
            &[fixture.baseline.digest.clone()],
            OWNER,
        )
        .unwrap();
    }

    #[test]
    fn a_candidate_that_repeats_a_digest_is_rejected_as_a_duplicate() {
        let fixture = fixture();
        let candidate = materialize_candidate(
            &fixture.baseline,
            OWNER,
            FIXTURE_PROMPT,
            &fixture.root.join("c2"),
        )
        .unwrap();
        let rejection = structural_gate(
            &fixture.baseline,
            &candidate,
            &target(),
            FIXTURE_PROMPT,
            32 * 1024,
            &[fixture.baseline.digest.clone()],
            OWNER,
        )
        .unwrap_err();
        assert_eq!(rejection.reason, "duplicate_candidate");
        assert!(rejection.diagnostics().contains("duplicate_candidate"));
    }

    #[test]
    fn a_text_over_the_cap_is_rejected_before_anything_else() {
        let fixture = fixture();
        let long = "x".repeat(33);
        let candidate =
            materialize_candidate(&fixture.baseline, OWNER, &long, &fixture.root.join("c3")).unwrap();
        let rejection = structural_gate(
            &fixture.baseline,
            &candidate,
            &target(),
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
    fn an_empty_text_is_rejected() {
        let fixture = fixture();
        let candidate =
            materialize_candidate(&fixture.baseline, OWNER, "", &fixture.root.join("c4")).unwrap();
        let rejection = structural_gate(
            &fixture.baseline,
            &candidate,
            &target(),
            "",
            32 * 1024,
            &[fixture.baseline.digest.clone()],
            OWNER,
        )
        .unwrap_err();
        assert_eq!(rejection.reason, "empty_text");
    }

    #[test]
    fn a_candidate_that_changed_anything_else_is_rejected() {
        let fixture = fixture();
        let text = "Watch the mailbox, and say why.\n";
        let candidate =
            materialize_candidate(&fixture.baseline, OWNER, text, &fixture.root.join("c5")).unwrap();
        // An edit the materializer would never make: the second changed asset
        // stands in for any patch that reaches past the target field.
        let mut tampered = candidate.clone();
        tampered
            .files
            .insert("README.md".into(), b"# tampered\n".to_vec());
        let rejection = structural_gate(
            &fixture.baseline,
            &tampered,
            &target(),
            text,
            32 * 1024,
            &[fixture.baseline.digest.clone()],
            OWNER,
        )
        .unwrap_err();
        assert_eq!(rejection.reason, "unexpected_change");
        assert!(rejection.detail.contains("README.md"), "{}", rejection.detail);
    }

    #[test]
    fn the_task_prompt_template_target_is_refused_by_name() {
        let fixture = fixture();
        let text = "Watch the mailbox, and say why.\n";
        let candidate =
            materialize_candidate(&fixture.baseline, OWNER, text, &fixture.root.join("c6")).unwrap();
        let rejection = structural_gate(
            &fixture.baseline,
            &candidate,
            &Target {
                field: TargetField::TaskPromptTemplate,
                ..target()
            },
            text,
            32 * 1024,
            &[],
            OWNER,
        )
        .unwrap_err();
        assert_eq!(rejection.reason, "unsupported_target");
    }

    #[test]
    fn a_candidate_whose_references_no_longer_resolve_is_rejected() {
        let fixture = fixture();
        let text = "Watch the mailbox, and say why.\n";
        let mut candidate =
            materialize_candidate(&fixture.baseline, OWNER, text, &fixture.root.join("c7")).unwrap();
        // The context points at a tools document the pack does not declare.
        candidate.config.contexts[0].tools_id = Some("no-such-tools".into());
        let rejection = structural_gate(
            &fixture.baseline,
            &candidate,
            &target(),
            text,
            32 * 1024,
            &[],
            OWNER,
        )
        .unwrap_err();
        assert_eq!(rejection.reason, "invalid_closure");
        assert!(
            rejection.detail.contains("no-such-tools"),
            "{}",
            rejection.detail
        );
    }
}
```

- [ ] **Step 2: Make the fixture writer visible** — in `crates/gents/src/optimization/subject.rs`, change the test module header to `pub(crate) mod tests`, rename `write_pack` to `pub(crate) fn write_fixture_pack`, promote the prompt constant to `pub(crate) const FIXTURE_PROMPT: &str = "Watch the mailbox.\n";`, and update the three call sites inside that module. Nothing else changes.

- [ ] **Step 3: Run it to see it fail**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents --lib optimization::gate`
Expected: FAIL to compile, `cannot find function structural_gate in this scope`.

- [ ] **Step 4: Write the implementation** — above the test module in `gate.rs`:

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

use crate::config_client::DesiredStateApplyPlan;
use crate::optimization::subject::MaterializedPack;
use crate::optimization::target::{Target, TargetField};

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

/// Decide whether `candidate` may be evaluated at all.
///
/// The checks run in the order a rejection is cheapest to explain, and the
/// first failure decides. `seen_digests` holds the checkpoint's digest and
/// every earlier candidate's, so a proposer that repeats itself spends nothing.
pub fn structural_gate(
    baseline: &MaterializedPack,
    candidate: &MaterializedPack,
    target: &Target,
    text: &str,
    max_text_bytes: usize,
    seen_digests: &[String],
    owner: &str,
) -> Result<(), StructuralRejection> {
    if target.field != TargetField::AgentContextSystemPrompt {
        return Err(reject(
            "unsupported_target",
            format!(
                "{:?} is declared but not implemented; M6b optimizes AgentContext.system_prompt only",
                target.field
            ),
        ));
    }
    if text.trim().is_empty() {
        return Err(reject("empty_text", "a candidate prompt must say something"));
    }
    if text.len() > max_text_bytes {
        return Err(reject(
            "text_too_long",
            format!("{} bytes exceeds the {max_text_bytes} byte cap", text.len()),
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
        .unwrap_or_else(|| "pack_config.json".to_owned());
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

- [ ] **Step 5: Wire the module** — in `crates/gents/src/optimization/mod.rs` add `pub mod gate;` in rustfmt order and:

```rust
pub use gate::{structural_gate, StructuralRejection};
```

- [ ] **Step 6: Run the tests**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents --lib optimization::`
Expected: PASS, including the 7 gate tests, the 3 subject tests, the 3 proposer tests, the 4 target tests, the 5 job tests and M6a's 11 policy tests.

- [ ] **Step 7: Commit**

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

Branch: `optimization/22-driver`, base `optimization/21-proposer`. Deliverable: `run_job`, the loop the spec's section 6 diagram describes, and the scripted matrix that proves it.

### Task 1: Verdicts to evidence

**Files:**
- Create: `crates/gents/src/optimization/evidence.rs`
- Modify: `crates/gents/src/optimization/mod.rs`

Nothing in `gents::eval` turns `VerdictRecord` rows into `TrialScore` values: `scoring::case_trial_score` takes `VerdictView`, which carries `stage_index`, while `VerdictRecord` carries `stage_id`, and the latest-attempt-per-slot selection the umbrella fixes for M4 has no implementation anywhere. This module is that projection.

**Interfaces:**
- Consumes:
  ```rust
  // crates/gents/src/eval/documents.rs
  pub struct TrialRecord { pub identity: TrialIdentity, pub created_at: String, pub completion: Option<TrialCompletion> }
  pub struct TrialIdentity { pub trial_id: String, pub run_id: String, pub cell_id: String,
      pub case_id: String, pub trial_index: u32, pub attempt: u32, pub trial_agent_did: String,
      pub session_id: String, pub seed: i64, pub home_hint: Option<String> }
  pub struct TrialCompletion { pub ended_at: String, pub stages: Vec<StageCompletion>,
      pub usage: TrialUsage, pub anchor: Anchor }
  pub struct TrialUsage { pub input_tokens: Option<u64>, pub output_tokens: Option<u64> }
  pub struct VerdictDraft { pub verdict_id: String, pub run_id: String, pub trial_id: String,
      pub stage_id: String, pub check: String, pub check_version: String, pub tier: EvalTier,
      pub kind: OutcomeKind, pub provider_reason: Option<ProviderReason>, pub score_bp: Option<u32>,
      pub weight: u32, pub raw: Value, pub feedback: Option<String>, pub regrade_of: Option<String> }
  pub type VerdictRecord = VerdictDraft;
  pub async fn load_run(access: &ConfigAccess, owner: &str, run_id: &str) -> Result<Option<RunRecord>>;
  pub async fn load_trials(access: &ConfigAccess, owner: &str, run_id: &str) -> Result<Vec<TrialRecord>>;
  pub async fn load_verdicts(access: &ConfigAccess, owner: &str, run_id: &str) -> Result<Vec<VerdictRecord>>;
  // crates/gents/src/eval/scoring.rs
  pub struct VerdictView { pub verdict_id: String, pub stage_index: usize, pub check: String,
      pub tier: EvalTier, pub kind: OutcomeKind, pub provider_reason: Option<ProviderReason>,
      pub score_bp: Option<u32>, pub weight: u32, pub regrade_of: Option<String> }
  pub fn latest_verdicts(verdicts: Vec<VerdictView>) -> Vec<VerdictView>;
  pub fn case_trial_score(reducer: EvalReducer, verdicts: &[VerdictView]) -> CaseTrialScore;
  pub struct TrialScore { pub case_id: String, pub trial_index: u32, pub score: CaseTrialScore }
  pub fn pair_trials(baseline: &[TrialScore], candidate: &[TrialScore]) -> PairedEvidence;
  // crates/gents/src/optimization/policy.rs
  pub fn evidence_from_pairs(paired: &PairedEvidence, expected_cases: &[String], tokens: Option<TokenTotals>) -> Evidence;
  pub struct TokenTotals { pub baseline_tokens: u64, pub baseline_trials: u64,
      pub candidate_tokens: u64, pub candidate_trials: u64 }
  pub fn decide(mode: Mode, policy: &PolicyV2, evidence: &Evidence, seed: u64) -> DecisionReport;
  ```
- Produces:
  ```rust
  pub fn latest_attempts<'a>(trials: &'a [TrialRecord], cell_id: &str) -> Vec<&'a TrialRecord>;
  pub fn cell_trial_scores(definition: &EvalDefinition, trials: &[TrialRecord],
      verdicts: &[VerdictRecord], cell_id: &str) -> Vec<TrialScore>;
  pub fn token_totals(trials: &[TrialRecord], baseline_cell: &str, candidate_cell: &str,
      max_missing_usage_bp: u64) -> Option<TokenTotals>;
  pub fn train_feedback(verdicts: &[VerdictRecord]) -> Vec<CheckFeedback>;
  pub fn decision_seed(run_ids: &[String]) -> u64;
  pub fn paired_evidence(definition: &EvalDefinition, trials: &[TrialRecord],
      verdicts: &[VerdictRecord], baseline_cell: &str, candidate_cell: &str,
      expected_cases: &[String], max_missing_usage_bp: u64) -> Evidence;
  pub struct DecisionMismatch { pub round: Option<u32>, pub run_ids: Vec<String>,
      pub journaled: Decision, pub recomputed: Decision, pub invalidated: bool }
  pub async fn recompute_decisions(access: &ConfigAccess, job: &JobRecord) -> Result<Vec<DecisionMismatch>>;
  ```

- [ ] **Step 1: Write the failing test** — create `crates/gents/src/optimization/evidence.rs` holding only this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::{Anchor, StageCompletion, TrialIdentity};
    use serde_json::json;

    fn definition() -> EvalDefinition {
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

    fn trial(cell: &str, index: u32, attempt: u32, tokens: Option<u64>, completed: bool) -> TrialRecord {
        TrialRecord {
            identity: TrialIdentity {
                trial_id: format!("{cell}-{index}-{attempt}"),
                run_id: "run".into(),
                cell_id: cell.into(),
                case_id: "disk-warning".into(),
                trial_index: index,
                attempt,
                trial_agent_did: "did:key:trial".into(),
                session_id: "session".into(),
                seed: 1_000 + index as i64,
                home_hint: None,
            },
            created_at: "2026-09-22T00:00:00Z".into(),
            completion: completed.then(|| TrialCompletion {
                ended_at: "2026-09-22T00:01:00Z".into(),
                stages: vec![StageCompletion {
                    stage_id: "check".into(),
                    request_id: None,
                    terminal_state: None,
                    failure_kind: None,
                    provider_reason: None,
                }],
                usage: TrialUsage {
                    input_tokens: tokens,
                    output_tokens: tokens,
                },
                anchor: Anchor {
                    terminal_states: Vec::new(),
                    requests: 1,
                    inference_calls: 1,
                },
            }),
        }
    }

    fn verdict(trial_id: &str, score_bp: u32, feedback: Option<&str>) -> VerdictRecord {
        VerdictRecord {
            verdict_id: format!("{trial_id}-v"),
            run_id: "run".into(),
            trial_id: trial_id.into(),
            stage_id: "check".into(),
            check: "captured_rows_count".into(),
            check_version: "1".into(),
            tier: EvalTier::Acceptance,
            kind: OutcomeKind::Passed,
            provider_reason: None,
            score_bp: Some(score_bp),
            weight: 1,
            raw: json!({"reason_code": "in_range"}),
            feedback: feedback.map(str::to_owned),
            regrade_of: None,
        }
    }

    #[test]
    fn a_slot_is_read_at_its_latest_attempt_only() {
        let trials = [
            trial("base", 0, 0, Some(10), true),
            trial("base", 0, 1, Some(10), true),
        ];
        let verdicts = [
            verdict("base-0-0", 0, None),
            verdict("base-0-1", 10_000, None),
        ];
        let scores = cell_trial_scores(&definition(), &trials, &verdicts, "base");
        assert_eq!(scores.len(), 1, "one slot, one score: {scores:?}");
        assert_eq!(scores[0].score, CaseTrialScore::Scored(10_000));
    }

    #[test]
    fn an_unfinished_attempt_contributes_nothing_and_falls_back_to_a_finished_one() {
        let trials = [
            trial("base", 0, 0, Some(10), true),
            trial("base", 0, 1, None, false),
        ];
        let verdicts = [verdict("base-0-0", 10_000, None)];
        let scores = cell_trial_scores(&definition(), &trials, &verdicts, "base");
        assert_eq!(scores.len(), 1);
        assert_eq!(scores[0].score, CaseTrialScore::Scored(10_000));

        let only_open = [trial("base", 0, 0, None, false)];
        assert!(cell_trial_scores(&definition(), &only_open, &[], "base").is_empty());
    }

    #[test]
    fn the_cost_gate_is_skipped_when_too_many_trials_report_no_usage() {
        let with_usage = [
            trial("base", 0, 0, Some(100), true),
            trial("cand", 0, 0, Some(150), true),
        ];
        let totals = token_totals(&with_usage, "base", "cand", 2_000).expect("usage is present");
        assert_eq!(
            (totals.baseline_tokens, totals.baseline_trials),
            (200, 1),
            "input and output are summed"
        );
        assert_eq!((totals.candidate_tokens, totals.candidate_trials), (300, 1));

        let half_missing = [
            trial("base", 0, 0, Some(100), true),
            trial("cand", 0, 0, None, true),
        ];
        assert_eq!(
            token_totals(&half_missing, "base", "cand", 2_000),
            None,
            "half the trials without usage is past a 20% tolerance"
        );
    }

    #[test]
    fn feedback_reaches_the_proposer_as_check_name_score_and_text_only() {
        let verdicts = [
            verdict("base-0-0", 0, Some("name the collection")),
            verdict("base-1-0", 10_000, None),
        ];
        let feedback = train_feedback(&verdicts);
        assert_eq!(feedback.len(), 2);
        assert_eq!(feedback[0].check, "captured_rows_count");
        assert_eq!(feedback[0].score_bp, Some(0));
        assert_eq!(feedback[0].feedback.as_deref(), Some("name the collection"));
        // The raw payload and the trial reference stay behind.
        let rendered = serde_json::to_string(&feedback).unwrap();
        for forbidden in ["reason_code", "trial", "disk-warning", "stage"] {
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

    #[test]
    fn paired_evidence_pairs_the_two_cells_on_the_shared_seed() {
        let trials = [
            trial("base", 0, 0, Some(10), true),
            trial("base", 1, 0, Some(10), true),
            trial("cand", 0, 0, Some(10), true),
            trial("cand", 1, 0, Some(10), true),
        ];
        let verdicts = [
            verdict("base-0-0", 0, None),
            verdict("base-1-0", 0, None),
            verdict("cand-0-0", 10_000, None),
            verdict("cand-1-0", 10_000, None),
        ];
        let evidence = paired_evidence(
            &definition(),
            &trials,
            &verdicts,
            "base",
            "cand",
            &["disk-warning".to_owned()],
            2_000,
        );
        assert!(evidence.cases_match);
        assert_eq!(evidence.cases.len(), 1);
        assert_eq!(evidence.cases[0].pairs, 2);
        assert_eq!(evidence.cases[0].sum_baseline_bp, 0);
        assert_eq!(evidence.cases[0].sum_candidate_bp, 20_000);
        assert_eq!(evidence.keys, 2);
        assert_eq!((evidence.dropped_baseline, evidence.dropped_candidate), (0, 0));
    }
}
```

- [ ] **Step 2: Run it to see it fail**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents --lib optimization::evidence`
Expected: FAIL to compile, `cannot find function cell_trial_scores in this scope`.

- [ ] **Step 3: Write the implementation** — above the test module:

```rust
//! Turning a run's rows into the [`Evidence`] the policy decides on.
//!
//! Three judgements live here and nowhere else. A slot is read at its latest
//! *completed* attempt, because a resumed run writes a new row per attempt and
//! reading both would make the slot ambiguous. A verdict's `stage_id` becomes
//! the `stage_index` its case declares, which is what `case_trial_score`
//! reduces over. And the cost gate is skipped when too many trials report no
//! usage, rather than averaging over a fabricated zero.
//!
//! What reaches a proposer is narrower still: a check's name, what it scored
//! and what it said. Never a case, a stage, a raw payload or a trial.

use std::collections::BTreeMap;

use anyhow::Result;
use sha2::{Digest, Sha256};

use crate::config_client::ConfigAccess;
use crate::document_config::{EvalDefinition, EvalTier};
use crate::eval::{
    case_trial_score, evidence_from_pairs, latest_verdicts, load_run, load_trials, load_verdicts,
    pair_trials, CaseTrialScore, OutcomeKind, ProviderReason, TrialRecord, TrialScore,
    TrialUsage, VerdictRecord, VerdictView,
};
use crate::optimization::job::{JobRecord, JournalEntry};
use crate::optimization::policy::{decide, Decision, Evidence, Mode, TokenTotals};
use crate::optimization::proposer::CheckFeedback;

/// One trial per `(case_id, trial_index)` slot of `cell_id`: the completed
/// attempt with the highest number. A slot whose every attempt is still open
/// contributes nothing, and the pairing counts it as a dropped key.
pub fn latest_attempts<'a>(trials: &'a [TrialRecord], cell_id: &str) -> Vec<&'a TrialRecord> {
    let mut latest: BTreeMap<(&str, u32), &TrialRecord> = BTreeMap::new();
    for record in trials
        .iter()
        .filter(|record| record.identity.cell_id == cell_id && record.completion.is_some())
    {
        let key = (
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

fn stage_indices(definition: &EvalDefinition, case_id: &str) -> Option<BTreeMap<String, usize>> {
    let case = definition
        .cases
        .iter()
        .find(|case| case.case_id == case_id)?;
    Some(
        case.stages
            .iter()
            .enumerate()
            .map(|(index, stage)| (stage.stage_id.clone(), index))
            .collect(),
    )
}

/// One [`TrialScore`] per completed slot of `cell_id`.
pub fn cell_trial_scores(
    definition: &EvalDefinition,
    trials: &[TrialRecord],
    verdicts: &[VerdictRecord],
    cell_id: &str,
) -> Vec<TrialScore> {
    let mut by_trial: BTreeMap<&str, Vec<&VerdictRecord>> = BTreeMap::new();
    for verdict in verdicts {
        by_trial
            .entry(verdict.trial_id.as_str())
            .or_default()
            .push(verdict);
    }
    let mut scores = Vec::new();
    for record in latest_attempts(trials, cell_id) {
        let case_id = record.identity.case_id.as_str();
        let Some(case) = definition.cases.iter().find(|case| case.case_id == case_id) else {
            continue;
        };
        let Some(indices) = stage_indices(definition, case_id) else {
            continue;
        };
        let views: Vec<VerdictView> = by_trial
            .get(record.identity.trial_id.as_str())
            .into_iter()
            .flatten()
            .filter_map(|verdict| {
                Some(VerdictView {
                    verdict_id: verdict.verdict_id.clone(),
                    stage_index: *indices.get(&verdict.stage_id)?,
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

fn trial_tokens(usage: &TrialUsage) -> Option<u64> {
    match (usage.input_tokens, usage.output_tokens) {
        (None, None) => None,
        (input, output) => Some(input.unwrap_or(0) + output.unwrap_or(0)),
    }
}

/// Mean tokens per case-trial for both cells, or `None` when more than
/// `max_missing_usage_bp` of the counted trials reported no usage at all.
pub fn token_totals(
    trials: &[TrialRecord],
    baseline_cell: &str,
    candidate_cell: &str,
    max_missing_usage_bp: u64,
) -> Option<TokenTotals> {
    let mut totals = TokenTotals {
        baseline_tokens: 0,
        baseline_trials: 0,
        candidate_tokens: 0,
        candidate_trials: 0,
    };
    let (mut counted, mut missing) = (0u128, 0u128);
    for (cell, tokens, count) in [
        (baseline_cell, &mut totals.baseline_tokens, &mut totals.baseline_trials),
        (candidate_cell, &mut totals.candidate_tokens, &mut totals.candidate_trials),
    ] {
        for record in latest_attempts(trials, cell) {
            let Some(completion) = &record.completion else {
                continue;
            };
            counted += 1;
            *count += 1;
            match trial_tokens(&completion.usage) {
                Some(seen) => *tokens += seen,
                None => missing += 1,
            }
        }
    }
    if counted == 0 || missing * 10_000 > max_missing_usage_bp as u128 * counted {
        tracing::info!(
            counted = counted as u64,
            missing = missing as u64,
            "optimization cost gate skipped: too many trials reported no usage"
        );
        return None;
    }
    Some(totals)
}

/// What a proposer may read from the train run. A check's name, what it
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
    u64::from_le_bytes(digest[..8].try_into().unwrap_or([0; 8]))
}

/// The evidence one decision reads: both cells' slots paired on their shared
/// seed, over `expected_cases`.
#[allow(clippy::too_many_arguments)]
pub fn paired_evidence(
    definition: &EvalDefinition,
    trials: &[TrialRecord],
    verdicts: &[VerdictRecord],
    baseline_cell: &str,
    candidate_cell: &str,
    expected_cases: &[String],
    max_missing_usage_bp: u64,
) -> Evidence {
    let baseline = cell_trial_scores(definition, trials, verdicts, baseline_cell);
    let candidate = cell_trial_scores(definition, trials, verdicts, candidate_cell);
    let paired = pair_trials(&baseline, &candidate);
    let tokens = token_totals(trials, baseline_cell, candidate_cell, max_missing_usage_bp);
    evidence_from_pairs(&paired, expected_cases, tokens)
}

/// A journaled decision that does not match what its runs say today.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecisionMismatch {
    pub round: Option<u32>,
    pub run_ids: Vec<String>,
    pub journaled: Decision,
    pub recomputed: Decision,
    /// One of the referenced runs has since been invalidated.
    pub invalidated: bool,
}

/// Recompute every journaled decision from the `EvalVerdict` rows of the runs
/// it names, and report the ones that no longer agree. The summary in the
/// journal is a convenience; these rows are the authority.
pub async fn recompute_decisions(
    access: &ConfigAccess,
    job: &JobRecord,
) -> Result<Vec<DecisionMismatch>> {
    let owner = job.origin.owner.as_str();
    let definition_id = job.origin.definition.definition_id.as_str();
    let mut mismatches = Vec::new();
    for entry in &job.journal {
        let JournalEntry::Decided {
            round,
            run_ids,
            mode,
            decision,
            ..
        } = entry
        else {
            continue;
        };
        let (mut trials, mut verdicts, mut cases, mut invalidated) =
            (Vec::new(), Vec::new(), Vec::new(), false);
        for run_id in run_ids {
            let Some(record) = load_run(access, owner, run_id).await? else {
                continue;
            };
            invalidated |= record.invalidated.is_some();
            cases = record.origin.case_ids.clone();
            trials.extend(load_trials(access, owner, run_id).await?);
            verdicts.extend(load_verdicts(access, owner, run_id).await?);
        }
        let definition = definition_of(access, owner, definition_id).await?;
        let evidence = paired_evidence(
            &definition,
            &trials,
            &verdicts,
            BASELINE_CELL,
            CANDIDATE_CELL,
            &cases,
            job.origin.policy.max_missing_usage_bp,
        );
        let recomputed = decide(
            *mode,
            &job.origin.policy,
            &evidence,
            decision_seed(run_ids),
        )
        .decision;
        if recomputed != *decision || invalidated {
            mismatches.push(DecisionMismatch {
                round: *round,
                run_ids: run_ids.clone(),
                journaled: *decision,
                recomputed,
                invalidated,
            });
        }
    }
    Ok(mismatches)
}

/// The cell ids every optimization run uses. `Confirm` mode compares the
/// original baseline against the final checkpoint under the same two ids, so
/// one projection serves both modes.
pub const BASELINE_CELL: &str = "baseline";
pub const CANDIDATE_CELL: &str = "candidate";

async fn definition_of(
    access: &ConfigAccess,
    owner: &str,
    definition_id: &str,
) -> Result<EvalDefinition> {
    crate::optimization::driver::load_definition(access, owner, definition_id).await
}
```

Two notes for the implementer. `EvalTier`, `OutcomeKind` and `ProviderReason` are imported for the test module's literals; if rustc reports them unused in the non-test build, move those three into the `use super::*` of the test module. `definition_of` forwards to PR 3 Task 3's `driver::load_definition`; until that task lands, `recompute_decisions` does not compile, so **implement `recompute_decisions` and its test in Task 3, not here** — Steps 1 and 3 of this task ship everything above it, and Task 3 appends `recompute_decisions`, `DecisionMismatch`, `definition_of` and the cell constants.

- [ ] **Step 4: Move the deferred half out of this task**

Delete `DecisionMismatch`, `recompute_decisions` and `definition_of` from the file for now, keeping `BASELINE_CELL` and `CANDIDATE_CELL`. Task 3 restores them verbatim once `driver::load_definition` exists.

- [ ] **Step 5: Wire the module** — in `crates/gents/src/optimization/mod.rs` add `pub mod evidence;` in rustfmt order and:

```rust
pub use evidence::{
    cell_trial_scores, decision_seed, latest_attempts, paired_evidence, token_totals,
    train_feedback, BASELINE_CELL, CANDIDATE_CELL,
};
```

- [ ] **Step 6: Run the tests**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents --lib optimization::evidence`
Expected: PASS, 6 tests.

- [ ] **Step 7: Commit**

```bash
git add crates/gents/src/optimization
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" \
  commit -m "feat(optimization): project eval verdicts into policy evidence (#1455)

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

### Task 2: The eval-run bridge and the budget

**Files:**
- Create: `crates/gents/src/optimization/driver.rs` (this task writes the first half of the file)
- Modify: `crates/gents/src/optimization/mod.rs`

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
  pub struct RunOutcome { pub run_id: String, pub completed: u32, pub abandoned: u32,
      pub not_evidence: u32, pub breaker_tripped: bool }
  pub struct RunOptions { pub poll_backoff_base: Duration, pub poll_backoff_cap: Duration }
  ```
- Produces:
  ```rust
  pub struct JobRequest { /* fields listed in the implementation below */ }
  pub struct Spend { pub case_trials: u64, pub tokens: u64 }
  pub(crate) struct RunPlan { pub run_id: String, pub split: EvalSplit, pub round: Option<u32>,
      pub cells: Vec<(&'static str, PathBuf)>, pub seed_base: i64 }
  pub(crate) fn train_plan(request: &JobRequest, round: u32, checkpoint: &Path) -> RunPlan;
  pub(crate) fn validation_plan(request: &JobRequest, origin: &JobOrigin, round: u32, attempt: u32,
      checkpoint: &Path, candidate: &Path) -> RunPlan;
  pub(crate) fn held_out_plan(request: &JobRequest, origin: &JobOrigin, baseline: &Path,
      checkpoint: &Path) -> RunPlan;
  pub(crate) fn run_request(request: &JobRequest, plan: &RunPlan) -> RunRequest;
  pub(crate) async fn execute_run(access: &ConfigAccess, request: &JobRequest, plan: &RunPlan,
      executor: &dyn TrialExecutor, registry: &CheckRegistry, cancel: CancellationToken) -> Result<RunOutcome>;
  pub fn split_case_count(definition: &EvalDefinition, split: EvalSplit) -> u64;
  pub fn run_cost(definition: &EvalDefinition, split: EvalSplit, trials_per_case: u32, cells: u64) -> u64;
  pub async fn spend_so_far(access: &ConfigAccess, owner: &str, journal: &[JournalEntry]) -> Result<Spend>;
  ```

- [ ] **Step 1: Write the failing test** — create `crates/gents/src/optimization/driver.rs` holding only this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
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

    fn request() -> JobRequest {
        JobRequest {
            job_id: "job-1".into(),
            owner: "did:key:o".into(),
            evaluator_did: "did:key:home".into(),
            behavior_id: "monitor".into(),
            definition_id: "monitor-findings".into(),
            inference_profile_id: "local".into(),
            baseline_pack: PathBuf::from("/tmp/baseline"),
            trials_per_case: 2,
            budgets: Budgets {
                max_rounds: 3,
                max_case_trials: 1_000,
                max_tokens: 1_000_000,
                deadline_unix_secs: None,
            },
            max_text_bytes: 32 * 1024,
            seed_base: 1_000,
            jobs_dir: PathBuf::from("/tmp/jobs"),
            runs_dir: PathBuf::from("/tmp/runs"),
            source_commit: "0deb7659c".into(),
            source_dirty: false,
            concurrency: 1,
            max_infra_retries: 1,
            breaker_threshold: 5,
            deadline_secs: Some(600),
        }
    }

    #[test]
    fn a_train_run_has_one_cell_and_a_validation_run_has_two_on_one_seed() {
        let train = train_plan(&request(), 1, Path::new("/tmp/jobs/job-1/baseline"));
        assert_eq!(train.split, EvalSplit::Train);
        assert_eq!(train.cells.len(), 1);
        assert_eq!(train.cells[0].0, BASELINE_CELL);
        assert_eq!(train.run_id, "job-1-r1-train");

        let validation = validation_plan(
            &request(),
            1,
            0,
            Path::new("/tmp/jobs/job-1/baseline"),
            Path::new("/tmp/jobs/job-1/rounds/1/candidate"),
        );
        assert_eq!(validation.run_id, "job-1-r1-v0");
        assert_eq!(validation.split, EvalSplit::Validation);
        assert_eq!(
            validation.cells.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            vec![BASELINE_CELL, CANDIDATE_CELL]
        );

        let frozen = run_request(&request(), &validation);
        assert_eq!(frozen.purpose, "optimization:job-1");
        assert_eq!(frozen.cells.len(), 2);
        assert_eq!(
            frozen.cells[0].inference_profile_id,
            frozen.cells[1].inference_profile_id,
            "both arms run the same inference binding"
        );
        assert!(frozen.captures.is_empty());
        // One seed_base per run is how the runner shares a seed across cells.
        assert_eq!(frozen.seed_base, validation.seed_base);
    }

    #[test]
    fn a_rerun_draws_a_new_seed_base_and_a_later_round_never_collides() {
        let first = validation_plan(&request(), 1, 0, Path::new("a"), Path::new("b")).seed_base;
        let rerun = validation_plan(&request(), 1, 1, Path::new("a"), Path::new("b")).seed_base;
        let next_round = validation_plan(&request(), 2, 0, Path::new("a"), Path::new("b")).seed_base;
        assert_ne!(first, rerun);
        assert_ne!(first, next_round);
        assert_ne!(rerun, next_round);
        // A run's trials draw `seed_base + trial_index`, so the gaps must be
        // wider than any run's trial count.
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
        let plan = held_out_plan(
            &request(),
            Path::new("/tmp/jobs/job-1/baseline"),
            Path::new("/tmp/jobs/job-1/rounds/2/candidate"),
        );
        assert_eq!(plan.run_id, "job-1-held-out");
        assert_eq!(plan.split, EvalSplit::HeldOut);
        assert_eq!(plan.round, None);
        assert_eq!(plan.cells[0].1, PathBuf::from("/tmp/jobs/job-1/baseline"));
        assert_eq!(
            plan.cells[1].1,
            PathBuf::from("/tmp/jobs/job-1/rounds/2/candidate")
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
//! journal, so a resumed job and an uninterrupted one reach the same state.
//! The driver is the only writer of the job, as the owner DID; it never claims
//! a request, and no runtime reconciles what it writes.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use tokio_util::sync::CancellationToken;

use crate::config_client::ConfigAccess;
use crate::document_config::{EvalDefinition, EvalSplit};
use crate::eval::checks::CheckRegistry;
use crate::eval::runner::{
    resume, run, CellRequest, CellSource, RunOptions, RunOutcome, RunRequest, TrialExecutor,
};
use crate::eval::{load_run, load_trials};
use crate::optimization::evidence::{BASELINE_CELL, CANDIDATE_CELL};
use crate::optimization::job::{Budgets, JournalEntry};

/// Everything an operator chose about a job before any of it is checked.
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
    /// The exported baseline subject pack. Copied into the job's directory at
    /// freeze; the copy is what every run reads.
    pub baseline_pack: PathBuf,
    pub trials_per_case: u32,
    pub budgets: Budgets,
    pub max_text_bytes: usize,
    pub seed_base: i64,
    /// `<launching home>/optimization/jobs`. This job owns `<jobs_dir>/<job_id>`.
    pub jobs_dir: PathBuf,
    /// `<launching home>/eval/runs`, handed to the runner unchanged.
    pub runs_dir: PathBuf,
    pub source_commit: String,
    pub source_dirty: bool,
    pub concurrency: u32,
    pub max_infra_retries: u32,
    pub breaker_threshold: u32,
    pub deadline_secs: Option<u64>,
}

impl JobRequest {
    pub fn job_dir(&self) -> PathBuf {
        self.jobs_dir.join(&self.job_id)
    }

    pub fn baseline_dir(&self) -> PathBuf {
        self.job_dir().join("baseline")
    }

    pub fn candidate_dir(&self, round: u32) -> PathBuf {
        self.job_dir().join("rounds").join(round.to_string()).join("candidate")
    }
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

pub(crate) fn train_plan(request: &JobRequest, round: u32, checkpoint: &Path) -> RunPlan {
    RunPlan {
        run_id: format!("{}-r{round}-train", request.job_id),
        split: EvalSplit::Train,
        round: Some(round),
        cells: vec![(BASELINE_CELL, checkpoint.to_path_buf())],
        seed_base: seed_base_for(request.seed_base, round, 0),
    }
}

pub(crate) fn validation_plan(
    request: &JobRequest,
    round: u32,
    attempt: u32,
    checkpoint: &Path,
    candidate: &Path,
) -> RunPlan {
    RunPlan {
        run_id: format!("{}-r{round}-v{attempt}", request.job_id),
        split: EvalSplit::Validation,
        round: Some(round),
        cells: vec![
            (BASELINE_CELL, checkpoint.to_path_buf()),
            (CANDIDATE_CELL, candidate.to_path_buf()),
        ],
        seed_base: seed_base_for(request.seed_base, round, attempt),
    }
}

/// The one held-out run of a job: the original baseline against the final
/// checkpoint. It is run once and never re-run.
pub(crate) fn held_out_plan(
    request: &JobRequest,
    baseline: &Path,
    checkpoint: &Path,
) -> RunPlan {
    RunPlan {
        run_id: format!("{}-held-out", request.job_id),
        split: EvalSplit::HeldOut,
        round: None,
        cells: vec![
            (BASELINE_CELL, baseline.to_path_buf()),
            (CANDIDATE_CELL, checkpoint.to_path_buf()),
        ],
        seed_base: seed_base_for(request.seed_base, 0, 0),
    }
}

pub(crate) fn run_request(request: &JobRequest, plan: &RunPlan) -> RunRequest {
    RunRequest {
        run_id: plan.run_id.clone(),
        owner: request.owner.clone(),
        evaluator_did: request.evaluator_did.clone(),
        definition_id: request.definition_id.clone(),
        split: plan.split,
        case_ids: None,
        cells: plan
            .cells
            .iter()
            .map(|(cell_id, pack)| CellRequest {
                cell_id: (*cell_id).to_owned(),
                label: (*cell_id).to_owned(),
                source: CellSource::Directory(pack.clone()),
                behavior_id: request.behavior_id.clone(),
                inference_profile_id: request.inference_profile_id.clone(),
            })
            .collect(),
        trials_per_case: request.trials_per_case,
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

fn run_options() -> RunOptions {
    RunOptions::default()
}

/// Freeze and run `plan`, or resume it when its row already exists.
pub(crate) async fn execute_run(
    access: &ConfigAccess,
    request: &JobRequest,
    plan: &RunPlan,
    executor: &dyn TrialExecutor,
    registry: &CheckRegistry,
    cancel: CancellationToken,
) -> Result<RunOutcome> {
    if load_run(access, &request.owner, &plan.run_id).await?.is_some() {
        tracing::info!(run_id = %plan.run_id, "optimization run resumed");
        return resume(
            access,
            &request.owner,
            &plan.run_id,
            &request.runs_dir,
            executor,
            registry,
            cancel,
            &run_options(),
        )
        .await;
    }
    run(
        access,
        &run_request(request, plan),
        executor,
        registry,
        cancel,
        &run_options(),
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
    let (_, value) = found
        .with_context(|| format!("no eval definition {definition_id:?} for {owner}"))?;
    serde_json::from_value(value).with_context(|| format!("decoding eval definition {definition_id:?}"))
}

/// Unused until Task 3; declared here so the module's one timing helper lives
/// beside the budget it serves.
pub(crate) fn now_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs()
}
```

- [ ] **Step 4: Wire the module** — in `crates/gents/src/optimization/mod.rs` add `pub mod driver;` in rustfmt order and:

```rust
pub use driver::{run_cost, spend_so_far, split_case_count, JobRequest, Spend};
```

- [ ] **Step 5: Run the tests**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents --lib optimization::driver`
Expected: PASS, 4 tests.

- [ ] **Step 6: Commit**

```bash
git add crates/gents/src/optimization
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" \
  commit -m "feat(optimization): eval-run plans and the job budget (#1455)

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

### Task 3: `run_job`

**Files:**
- Modify: `crates/gents/src/optimization/driver.rs` (append to the file Task 2 created)
- Modify: `crates/gents/src/optimization/evidence.rs` (restore `DecisionMismatch`, `recompute_decisions` and `definition_of`)
- Modify: `crates/gents/src/optimization/mod.rs`

**Interfaces:**
- Consumes: everything PR 1, PR 2 and PR 3 Tasks 1 and 2 produced, plus
  ```rust
  // crates/gents/src/optimization/policy.rs
  pub fn decide(mode: Mode, policy: &PolicyV2, evidence: &Evidence, seed: u64) -> DecisionReport;
  pub enum Decision { Accept, Reject(RejectReason), Inconclusive(InconclusiveReason) }
  // crates/gents/src/config_client/desired_state.rs
  pub fn desired_state_document_digest(value: &Value) -> Result<String>;
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
  // crates/gents/src/optimization/evidence.rs
  pub struct DecisionMismatch { pub round: Option<u32>, pub run_ids: Vec<String>,
      pub journaled: Decision, pub recomputed: Decision, pub invalidated: bool }
  pub async fn recompute_decisions(access: &ConfigAccess, job: &JobRecord) -> Result<Vec<DecisionMismatch>>;
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
        assert!(refusal.0.contains('5') && refusal.0.contains('3'), "{}", refusal.0);

        policy.max_rounds = 3;
        check_policy(&request(), &policy).unwrap();
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
        assert_eq!(proposed.0, "candidate one");
        assert_eq!(proposed.2, "sha256:one");
        assert!(!round_is_closed(&journal, 1), "no Decided and no StructuralReject");
        assert!(proposed_for(&journal, 2).is_none());

        let mut closed = journal.clone();
        closed.push(JournalEntry::StructuralReject {
            round: 1,
            diagnostics: "duplicate_candidate: …".into(),
        });
        assert!(round_is_closed(&closed, 1));
    }

    #[test]
    fn the_seen_digests_are_the_baseline_and_every_earlier_candidate() {
        let journal = vec![
            JournalEntry::Frozen,
            JournalEntry::Proposed {
                round: 1,
                text: "one".into(),
                rationale: String::new(),
                candidate_digest: "sha256:one".into(),
            },
            JournalEntry::Proposed {
                round: 2,
                text: "two".into(),
                rationale: String::new(),
                candidate_digest: "sha256:two".into(),
            },
        ];
        assert_eq!(
            seen_digests("sha256:baseline", &journal, 3),
            vec![
                "sha256:baseline".to_owned(),
                "sha256:one".to_owned(),
                "sha256:two".to_owned()
            ]
        );
        assert_eq!(
            seen_digests("sha256:baseline", &journal, 2),
            vec!["sha256:baseline".to_owned(), "sha256:one".to_owned()],
            "a replayed round never compares itself against its own digest"
        );
    }
```

Add `use crate::optimization::policy::PolicyV2;` to the test module's imports.

- [ ] **Step 2: Run it to see it fail**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents --lib optimization::driver`
Expected: FAIL to compile, `cannot find function check_policy in this scope`.

- [ ] **Step 3: Write the implementation** — append to `driver.rs`, above its test module:

```rust
/// A job that will not start, and why. Distinct from a `Failed` job: nothing
/// was written, so there is no journal to read.
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
    pub state: JobState,
    /// The retained checkpoint, never the best candidate the job ever saw.
    pub checkpoint: Option<Checkpoint>,
    pub rounds_used: u32,
}

/// `alpha_effective = alpha / max_rounds` is a Bonferroni correction for an
/// optimizer that tries several candidates on one validation split, so the
/// divisor has to be the number of candidates the budget actually allows.
pub(crate) fn check_policy(request: &JobRequest, policy: &PolicyV2) -> Result<()> {
    if policy.max_rounds == request.budgets.max_rounds {
        return Ok(());
    }
    Err(refused(format!(
        "policy max_rounds {} does not match the budget's max_rounds {}; the Bonferroni divisor must be the number of candidates the job may try",
        policy.max_rounds, request.budgets.max_rounds
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

/// The pack a directory already holds, or a fresh candidate written into it.
fn candidate_pack(
    baseline: &MaterializedPack,
    owner: &str,
    text: &str,
    dir: &Path,
) -> Result<MaterializedPack> {
    if dir.exists() {
        let behavior_id = baseline
            .config
            .agent_behaviors
            .iter()
            .find(|behavior| behavior.context_id.as_deref() == Some(baseline.context_id.as_str()))
            .map(|behavior| behavior.behavior_id.clone())
            .with_context(|| format!("no behavior names context {:?}", baseline.context_id))?;
        return materialize_pack(dir, owner, &behavior_id);
    }
    materialize_candidate(baseline, owner, text, dir)
}

/// Freeze a baseline, run rounds against it, and finalize.
///
/// Calling this again on the same `job_id` resumes: the journal is replayed,
/// an unfinished run is resumed through the runner, and a round that was
/// already proposed is never proposed again. A resumed job and an uninterrupted
/// one reach the same journal.
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
    check_policy(request, policy)?;
    let owner = request.owner.as_str();
    let definition = load_definition(access, owner, &request.definition_id).await?;
    definition
        .validate()
        .map_err(|error| refused(format!("eval definition: {error:#}")))?;
    let definition_ref = DefinitionRef {
        definition_id: definition.definition_id.clone(),
        comparability_version: definition.comparability_version,
        digest: desired_state_document_digest(&serde_json::to_value(&definition)?)?,
    };

    // The job owns its own copy of the baseline pack, so a later edit of the
    // exported directory cannot change what an earlier round meant.
    let source = materialize_pack(&request.baseline_pack, owner, &request.behavior_id)?;
    let baseline_dir = request.baseline_dir();
    let baseline = if baseline_dir.exists() {
        materialize_pack(&baseline_dir, owner, &request.behavior_id)?
    } else {
        materialize_candidate(&source, owner, &baseline_text(&source)?, &baseline_dir)?
    };
    let target = Target {
        field: TargetField::AgentContextSystemPrompt,
        owner: request.owner.clone(),
        id: baseline.context_id.clone(),
    };

    let closure = {
        let owner = request.owner.clone();
        access
            .transact("optimization.capture_closure", |txn| {
                let owner = &owner;
                Box::pin(async move { capture_closure(txn, owner).await })
            })
            .await?
    };
    let frozen = closure_digests(&closure)?;
    let live_text = current_text(&closure, &target)?;
    let pack_text = baseline_text(&baseline)?;
    if live_text != pack_text {
        return Err(refused(format!(
            "the live {} {:?} and the baseline pack disagree about the subject's system prompt; export the subject from this configuration before optimizing it",
            target.field.collection().graphql_type(),
            target.id
        )));
    }

    let mut job = match load_job(access, owner, &request.job_id).await? {
        Some(existing) => existing,
        None => {
            let origin = JobOrigin {
                target: target.clone(),
                closure: frozen.clone(),
                subject: SubjectRef {
                    pack_digest: baseline.digest.clone(),
                    behavior_id: request.behavior_id.clone(),
                },
                definition: definition_ref.clone(),
                policy: policy.clone(),
                trials_per_case: request.trials_per_case,
                budgets: request.budgets.clone(),
                baseline_text: pack_text.clone(),
                owner: request.owner.clone(),
                seed_base: request.seed_base,
            };
            let mut created = create_job(access, &request.job_id, owner, &origin).await?;
            append(access, &mut created, JournalEntry::Frozen).await?;
            created
        }
    };

    let state = derive_state(&job.journal);
    if state != JobState::Running {
        return Ok(outcome(&job, state));
    }

    // Drift is checked on every start, before any further spend.
    if job.origin.closure != frozen {
        return finalize(access, &mut job, JobState::Failed {
            reason: "baseline_drifted".into(),
        })
        .await;
    }
    if job.origin.definition != definition_ref {
        return finalize(access, &mut job, JobState::Failed {
            reason: "definition_changed".into(),
        })
        .await;
    }

    let held_out_reserve = run_cost(&definition, EvalSplit::HeldOut, request.trials_per_case, 2);
    let train_cost = run_cost(&definition, EvalSplit::Train, request.trials_per_case, 1);
    let validation_cost = run_cost(&definition, EvalSplit::Validation, request.trials_per_case, 2);
    let mut exhausted = false;

    for round in 1..=request.budgets.max_rounds {
        if round_is_closed(&job.journal, round) {
            continue;
        }
        let retained = checkpoint(&job.journal);
        let checkpoint_dir = retained
            .as_ref()
            .map(|held| request.candidate_dir(held.round))
            .unwrap_or_else(|| baseline_dir.clone());
        let checkpoint_text = retained
            .as_ref()
            .map(|held| held.text.clone())
            .unwrap_or_else(|| job.origin.baseline_text.clone());

        let spend = spend_so_far(access, owner, &job.journal).await?;
        if spend.case_trials + train_cost + validation_cost + held_out_reserve
            > request.budgets.max_case_trials
            || spend.tokens > request.budgets.max_tokens
            || past_deadline(&job.origin.budgets)
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
            exhausted = true;
            break;
        }

        // 1. Train run: one cell, the checkpoint, on the train split.
        let train = train_plan(request, round, &checkpoint_dir);
        if !run_started(&job.journal, &train.run_id) {
            append(
                access,
                &mut job,
                JournalEntry::RunStarted {
                    run_id: train.run_id.clone(),
                    round: Some(round),
                    split: EvalSplit::Train,
                },
            )
            .await?;
        }
        execute_run(access, request, &train, executor, registry, cancel.clone()).await?;

        // 2. Propose. Feedback comes from this run's verdicts and nowhere else.
        let (text, rationale, digest) = match proposed_for(&job.journal, round) {
            Some(replayed) => replayed,
            None => {
                let feedback =
                    train_feedback(&load_verdicts(access, owner, &train.run_id).await?);
                let proposal = proposer
                    .propose(ProposalInput {
                        round,
                        current_text: checkpoint_text.clone(),
                        feedback,
                        rejections: rejections(&job.journal),
                        max_text_bytes: request.max_text_bytes,
                    })
                    .await?;
                let candidate = candidate_pack(
                    &baseline,
                    owner,
                    &proposal.text,
                    &request.candidate_dir(round),
                )?;
                let entry = JournalEntry::Proposed {
                    round,
                    text: proposal.text.clone(),
                    rationale: proposal.rationale.clone(),
                    candidate_digest: candidate.digest.clone(),
                };
                append(access, &mut job, entry).await?;
                (proposal.text, proposal.rationale, candidate.digest)
            }
        };
        let _ = rationale;

        // 3. Structural gate, before any validation spend.
        let candidate = candidate_pack(&baseline, owner, &text, &request.candidate_dir(round))?;
        if let Err(rejection) = structural_gate(
            &baseline,
            &candidate,
            &target,
            &text,
            request.max_text_bytes,
            &seen_digests(&baseline.digest, &job.journal, round),
            owner,
        ) {
            tracing::info!(round, reason = rejection.reason, "candidate rejected before any spend");
            append(
                access,
                &mut job,
                JournalEntry::StructuralReject {
                    round,
                    diagnostics: rejection.diagnostics(),
                },
            )
            .await?;
            continue;
        }
        debug_assert_eq!(digest, candidate.digest);

        // 4. Validation runs, and 5. the decision. A re-run adds pairs and
        // never replaces them, so every attempt's rows are read together.
        let mut run_ids = Vec::new();
        for attempt in 0..=policy.max_reruns {
            let spend = spend_so_far(access, owner, &job.journal).await?;
            if spend.case_trials + validation_cost + held_out_reserve
                > request.budgets.max_case_trials
            {
                append(
                    access,
                    &mut job,
                    JournalEntry::BudgetExhausted {
                        round: Some(round),
                        reason: "no budget for the validation run".into(),
                    },
                )
                .await?;
                exhausted = true;
                break;
            }
            let plan = validation_plan(
                request,
                round,
                attempt,
                &checkpoint_dir,
                &request.candidate_dir(round),
            );
            if !run_started(&job.journal, &plan.run_id) {
                append(
                    access,
                    &mut job,
                    JournalEntry::RunStarted {
                        run_id: plan.run_id.clone(),
                        round: Some(round),
                        split: EvalSplit::Validation,
                    },
                )
                .await?;
            }
            execute_run(access, request, &plan, executor, registry, cancel.clone()).await?;
            run_ids.push(plan.run_id.clone());

            let (trials, verdicts, cases) = read_runs(access, owner, &run_ids).await?;
            let evidence = paired_evidence(
                &definition,
                &trials,
                &verdicts,
                BASELINE_CELL,
                CANDIDATE_CELL,
                &cases,
                policy.max_missing_usage_bp,
            );
            let report = decide(
                Mode::Improve,
                policy,
                &evidence,
                decision_seed(&run_ids),
            );
            let last = attempt == policy.max_reruns;
            if matches!(report.decision, Decision::Inconclusive(_)) && !last {
                continue;
            }
            append(
                access,
                &mut job,
                JournalEntry::Decided {
                    round: Some(round),
                    attempt,
                    run_ids: run_ids.clone(),
                    mode: Mode::Improve,
                    decision: report.decision,
                    policy_version: report.policy_version.clone(),
                    summary: summary_of(&report),
                },
            )
            .await?;
            break;
        }
        if exhausted {
            break;
        }
    }

    // 6. Finalize. The held-out split is touched once, and only when a round
    // moved the checkpoint off the baseline.
    let Some(retained) = checkpoint(&job.journal) else {
        let state = if exhausted {
            JobState::Exhausted
        } else {
            JobState::NothingToPromote
        };
        return finalize(access, &mut job, state).await;
    };
    let plan = held_out_plan(request, &baseline_dir, &request.candidate_dir(retained.round));
    let decision = match decided_held_out(&job.journal) {
        Some(decision) => decision,
        None => {
            if !run_started(&job.journal, &plan.run_id) {
                append(
                    access,
                    &mut job,
                    JournalEntry::RunStarted {
                        run_id: plan.run_id.clone(),
                        round: None,
                        split: EvalSplit::HeldOut,
                    },
                )
                .await?;
            }
            execute_run(access, request, &plan, executor, registry, cancel.clone()).await?;
            let run_ids = vec![plan.run_id.clone()];
            let (trials, verdicts, cases) = read_runs(access, owner, &run_ids).await?;
            let evidence = paired_evidence(
                &definition,
                &trials,
                &verdicts,
                BASELINE_CELL,
                CANDIDATE_CELL,
                &cases,
                policy.max_missing_usage_bp,
            );
            let report = decide(Mode::Confirm, policy, &evidence, decision_seed(&run_ids));
            append(
                access,
                &mut job,
                JournalEntry::Decided {
                    round: None,
                    attempt: 0,
                    run_ids,
                    mode: Mode::Confirm,
                    decision: report.decision,
                    policy_version: report.policy_version.clone(),
                    summary: summary_of(&report),
                },
            )
            .await?;
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

/// The trials, verdicts and expected cases of every run a decision reads.
async fn read_runs(
    access: &ConfigAccess,
    owner: &str,
    run_ids: &[String],
) -> Result<(Vec<TrialRecord>, Vec<VerdictRecord>, Vec<String>)> {
    let (mut trials, mut verdicts, mut cases) = (Vec::new(), Vec::new(), Vec::new());
    for run_id in run_ids {
        if let Some(record) = load_run(access, owner, run_id).await? {
            cases = record.origin.case_ids.clone();
        }
        trials.extend(load_trials(access, owner, run_id).await?);
        verdicts.extend(load_verdicts(access, owner, run_id).await?);
    }
    Ok((trials, verdicts, cases))
}

async fn finalize(
    access: &ConfigAccess,
    job: &mut JobRecord,
    state: JobState,
) -> Result<JobOutcome> {
    tracing::info!(job_id = %job.job_id, state = state.label(), "optimization job finalized");
    append(
        access,
        job,
        JournalEntry::Finalized {
            state: state.clone(),
        },
    )
    .await?;
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

Extend the file's `use` block with the names this half needs, in rustfmt order:

```rust
use crate::config_client::desired_state_document_digest;
use crate::eval::{load_verdicts, DefinitionRef, SubjectRef, TrialRecord, VerdictRecord};
use crate::optimization::evidence::{decision_seed, paired_evidence, train_feedback};
use crate::optimization::gate::structural_gate;
use crate::optimization::job::{
    append, checkpoint, create_job, derive_state, load_job, rounds_used, Checkpoint,
    DecisionSummary, JobOrigin, JobRecord, JobState,
};
use crate::optimization::policy::{decide, Decision, DecisionReport, Mode, PolicyV2};
use crate::optimization::proposer::{ProposalInput, Proposer, Rejection};
use crate::optimization::subject::{
    baseline_text, materialize_candidate, materialize_pack, MaterializedPack,
};
use crate::optimization::target::{capture_closure, closure_digests, current_text, Target, TargetField};
```

- [ ] **Step 4: Restore the recompute half of `evidence.rs`**

Paste `DecisionMismatch`, `recompute_decisions` and `definition_of` back into `crates/gents/src/optimization/evidence.rs`, exactly as Task 1 Step 3 wrote them, and extend that file's `use` block with `use crate::optimization::job::{JobRecord, JournalEntry};` and `use crate::optimization::policy::{decide, Decision, Mode};`.

- [ ] **Step 5: Wire the module** — in `crates/gents/src/optimization/mod.rs` extend the re-exports:

```rust
pub use driver::{
    job_refused, run_cost, run_job, spend_so_far, split_case_count, JobOutcome, JobRefused,
    JobRequest, Spend,
};
pub use evidence::{recompute_decisions, DecisionMismatch};
```

- [ ] **Step 6: Run the tests**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents --lib optimization`
Expected: PASS, including the 3 new driver tests.

- [ ] **Step 7: Commit**

```bash
git add crates/gents/src/optimization
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" \
  commit -m "feat(optimization): run_job drives rounds over native eval runs (#1455)

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

### Task 4: The scripted matrix

**Files:**
- Create: `crates/gents/src/optimization/driver/matrix.rs`
- Modify: `crates/gents/src/optimization/driver.rs` (add `#[cfg(test)] mod matrix;` at the top of the file, in rustfmt order)

**Interfaces:**
- Consumes:
  ```rust
  // crates/gents/src/eval/runner/freeze.rs, pub(crate) mod tests
  pub(crate) const OWNER: &str = "did:key:eval-owner";
  pub(crate) struct Launching { pub(crate) access: ConfigAccess, /* private */ }
  impl Launching {
      pub(crate) async fn new() -> Self;
      pub(crate) async fn install(&self, documents: Vec<(Collection, Value)>);
      pub(crate) fn runs_dir(&self) -> PathBuf;
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
  }
  // crates/gents/src/eval/checks/mod.rs
  impl CheckRegistry { pub fn builtin() -> Self; #[cfg(test)] pub(crate) fn with(self, check: Box<dyn Check>) -> Self; }
  ```
  `Launching::pack` writes the fixture pack whose context sidecar is `agent_behaviors/monitor/system_prompt.md` holding `"Watch the mailbox.\n"`; read `crates/gents/src/eval/runner/freeze.rs` `fn write_fixture_pack` to confirm the bytes before asserting on the baseline text.
- Produces: tests only.

- [ ] **Step 1: Write the matrix** — create `crates/gents/src/optimization/driver/matrix.rs`:

```rust
//! The scripted end-to-end matrix: six jobs, no model, no network.
//!
//! Each case assembles a run the way `tests/eval_runner_canary.rs` does — a
//! launching home, an installed definition and inference documents, a fixture
//! pack as the subject — and replaces only the provider, with the runner's
//! `ScriptedExecutor`. Nothing here mocks the driver, the policy or the
//! journal: a failure is a defect in the stack.
//!
//! Scoring is binary by construction. `captured_rows_count` with `min: 1`
//! scores 10000 on a stage whose `findings` capture holds a row and 0 on one
//! that holds none, so a scripted arm's per-case difference is exactly
//! +10000, 0 or -10000 and every expected decision is arithmetic.

use std::path::PathBuf;

use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::document_config::EvalSplit;
use crate::eval::checks::CheckRegistry;
use crate::eval::runner::freeze::tests::{Launching, OWNER};
use crate::eval::runner::{ScriptKey, ScriptedExecutor, TrialEvidence};
use crate::optimization::driver::{run_job, JobRequest, JobOutcome};
use crate::optimization::job::{load_job, Budgets, JobState, JournalEntry};
use crate::optimization::policy::{Decision, PolicyV2, RejectReason};
use crate::optimization::proposer::ScriptedProposer;
use crate::Collection;

/// Six validation cases is the floor Bonferroni imposes: at `alpha = 0.05`
/// and three rounds, `alpha_effective` is 16666 ppm and the smallest
/// achievable p is `2^-n`, so five cases could never be accepted.
const VALIDATION_CASES: [&str; 6] = ["val-a", "val-b", "val-c", "val-d", "val-e", "val-f"];
const HELD_OUT_CASES: [&str; 6] = ["ho-a", "ho-b", "ho-c", "ho-d", "ho-e", "ho-f"];
const TRAIN_CASES: [&str; 1] = ["train-a"];
const TRIALS_PER_CASE: u32 = 2;
const BASELINE_PROMPT: &str = "Watch the mailbox.\n";
const CANDIDATE_PROMPT: &str = "Watch the mailbox, and name the collection.\n";

fn case(case_id: &str, split: &str) -> Value {
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
}

fn definition() -> Value {
    let mut cases: Vec<Value> = TRAIN_CASES.iter().map(|id| case(id, "train")).collect();
    cases.extend(VALIDATION_CASES.iter().map(|id| case(id, "validation")));
    cases.extend(HELD_OUT_CASES.iter().map(|id| case(id, "held_out")));
    json!({
        "definition_id": "monitor-findings",
        "agent_did": OWNER,
        "comparability_version": 1,
        "subject": {"kind": "behavior", "inference_slots": ["primary"]},
        "cases": cases,
    })
}

/// The live configuration the job freezes and, in PR 4, promotes into.
fn live_configuration() -> Vec<(Collection, Value)> {
    vec![
        (
            Collection::AgentContext,
            json!({
                "context_id": "monitor-context",
                "agent_did": OWNER,
                "display_name": "Monitor",
                "system_prompt": BASELINE_PROMPT,
            }),
        ),
        (
            Collection::AgentBehavior,
            json!({
                "behavior_id": "monitor",
                "agent_did": OWNER,
                "display_name": "Monitor",
                "context_id": "monitor-context",
                "inference_profile_id": "local",
            }),
        ),
    ]
}

/// A stage whose `findings` capture holds one row: the check passes at 10000.
fn pass() -> TrialEvidence {
    ScriptedExecutor::passed_evidence("did:key:trial", "check", "findings", vec![json!({})])
}

/// A stage whose `findings` capture holds nothing: `below_min`, scored 0.
fn fail() -> TrialEvidence {
    ScriptedExecutor::passed_evidence("did:key:trial", "check", "findings", Vec::new())
}

fn key(cell: &str, case_id: &str, trial_index: u32) -> ScriptKey {
    ScriptKey {
        cell_label: cell.into(),
        case_id: case_id.into(),
        trial_index,
        attempt: 1,
    }
}

/// Script one cell's answer for every trial of `cases`.
fn script(
    mut executor: ScriptedExecutor,
    cell: &str,
    cases: &[&str],
    evidence: impl Fn(&str) -> TrialEvidence,
) -> ScriptedExecutor {
    for case_id in cases {
        for trial_index in 0..TRIALS_PER_CASE {
            executor = executor.with(key(cell, case_id, trial_index), evidence(case_id));
        }
    }
    executor
}

struct Harness {
    launching: Launching,
    jobs_dir: PathBuf,
    pack: PathBuf,
}

impl Harness {
    async fn new() -> Self {
        let launching = Launching::new().await;
        launching
            .install(
                [(Collection::EvalDefinition, definition())]
                    .into_iter()
                    .chain(live_configuration())
                    .collect(),
            )
            .await;
        let pack = launching.pack("subject", "Off");
        let jobs_dir = launching.runs_dir().parent().unwrap().join("jobs");
        Self {
            launching,
            jobs_dir,
            pack,
        }
    }

    fn request(&self, job_id: &str, budgets: Budgets) -> JobRequest {
        JobRequest {
            job_id: job_id.into(),
            owner: OWNER.into(),
            evaluator_did: self.launching.evaluator_did(),
            behavior_id: "monitor".into(),
            definition_id: "monitor-findings".into(),
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
        }
    }
}

fn budgets(max_case_trials: u64) -> Budgets {
    Budgets {
        max_rounds: 1,
        max_case_trials,
        max_tokens: u64::MAX,
        deadline_unix_secs: None,
    }
}

fn policy() -> PolicyV2 {
    PolicyV2 {
        max_rounds: 1,
        max_reruns: 0,
        ..PolicyV2::uncalibrated()
    }
}

fn one_round_proposer() -> ScriptedProposer {
    ScriptedProposer::new(vec![(
        CANDIDATE_PROMPT.to_owned(),
        "name the collection the monitor should read".to_owned(),
    )])
}

/// Every cell passes everywhere except where the caller overrides it.
fn base_executor() -> ScriptedExecutor {
    ScriptedExecutor::new().with_default(pass())
}

async fn drive(
    harness: &Harness,
    request: &JobRequest,
    executor: &ScriptedExecutor,
    proposer: &ScriptedProposer,
) -> JobOutcome {
    run_job(
        &harness.launching.access,
        request,
        executor,
        proposer,
        &CheckRegistry::builtin(),
        &policy(),
        CancellationToken::new(),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn a_candidate_that_wins_every_case_is_accepted_and_reaches_ready_to_promote() {
    let harness = Harness::new().await;
    // The baseline fails every validation case; the candidate passes them.
    // Six differences of +10000 give p = 1/64 = 15625 ppm, inside the 16666
    // ppm Bonferroni-corrected alpha of a one-round job.
    let executor = script(base_executor(), "baseline", &VALIDATION_CASES, |_| fail());
    let request = harness.request("accept", budgets(1_000));
    let outcome = drive(&harness, &request, &executor, &one_round_proposer()).await;

    assert_eq!(outcome.state, JobState::ReadyToPromote, "{outcome:#?}");
    let retained = outcome.checkpoint.expect("round 1 was accepted");
    assert_eq!(retained.round, 1);
    assert_eq!(retained.text, CANDIDATE_PROMPT);

    let job = load_job(&harness.launching.access, OWNER, "accept")
        .await
        .unwrap()
        .unwrap();
    let decisions: Vec<&JournalEntry> = job
        .journal
        .iter()
        .filter(|entry| matches!(entry, JournalEntry::Decided { .. }))
        .collect();
    assert_eq!(decisions.len(), 2, "one validation decision and one held-out: {decisions:#?}");
    // The summary is a convenience; the verdict rows are the authority.
    let mismatches = crate::optimization::recompute_decisions(&harness.launching.access, &job)
        .await
        .unwrap();
    assert!(mismatches.is_empty(), "{mismatches:#?}");
}

#[tokio::test]
async fn a_candidate_that_wins_only_half_the_cases_is_rejected_for_no_improvement() {
    let harness = Harness::new().await;
    // Three cases improve by +10000 and three tie, so the observed sum is
    // reached by 8 of 64 sign vectors: p = 125000 ppm, well past alpha.
    let executor = script(base_executor(), "baseline", &VALIDATION_CASES[..3], |_| fail());
    let request = harness.request("no-improvement", budgets(1_000));
    let outcome = drive(&harness, &request, &executor, &one_round_proposer()).await;

    assert_eq!(outcome.state, JobState::NothingToPromote);
    assert!(outcome.checkpoint.is_none());
    let job = load_job(&harness.launching.access, OWNER, "no-improvement")
        .await
        .unwrap()
        .unwrap();
    assert!(
        job.journal.iter().any(|entry| matches!(
            entry,
            JournalEntry::Decided { decision: Decision::Reject(RejectReason::NoImprovement), .. }
        )),
        "{:#?}",
        job.journal
    );
    assert!(
        !job.journal.iter().any(|entry| matches!(
            entry,
            JournalEntry::RunStarted { split: EvalSplit::HeldOut, .. }
        )),
        "the held-out split is never touched when no round accepted"
    );
}

#[tokio::test]
async fn one_broken_case_rejects_the_candidate_even_though_the_mean_improves() {
    let harness = Harness::new().await;
    let broken = VALIDATION_CASES[0];
    // The baseline fails five cases and passes the sixth; the candidate is the
    // mirror image, so five cases improve by +10000 and one regresses by
    // -10000, past the 5000 bp per-case tolerance.
    let mut executor = script(base_executor(), "baseline", &VALIDATION_CASES[1..], |_| fail());
    executor = script(executor, "candidate", &[broken], |_| fail());
    let request = harness.request("case-regression", budgets(1_000));
    let outcome = drive(&harness, &request, &executor, &one_round_proposer()).await;

    assert_eq!(outcome.state, JobState::NothingToPromote);
    let job = load_job(&harness.launching.access, OWNER, "case-regression")
        .await
        .unwrap()
        .unwrap();
    assert!(
        job.journal.iter().any(|entry| matches!(
            entry,
            JournalEntry::Decided { decision: Decision::Reject(RejectReason::CaseRegression), .. }
        )),
        "{:#?}",
        job.journal
    );
}

#[tokio::test]
async fn a_repeated_candidate_is_structurally_rejected_and_costs_no_validation_run() {
    let harness = Harness::new().await;
    let executor = script(base_executor(), "baseline", &VALIDATION_CASES, |_| fail());
    let request = harness.request("structural", budgets(1_000));
    // The echoing proposer returns the checkpoint's own text, which
    // materializes to the checkpoint's own pack digest.
    let outcome = drive(&harness, &request, &executor, &ScriptedProposer::echoing()).await;

    assert_eq!(outcome.state, JobState::NothingToPromote);
    let job = load_job(&harness.launching.access, OWNER, "structural")
        .await
        .unwrap()
        .unwrap();
    let rejection = job
        .journal
        .iter()
        .find_map(|entry| match entry {
            JournalEntry::StructuralReject { round, diagnostics } => Some((*round, diagnostics.clone())),
            _ => None,
        })
        .unwrap_or_else(|| panic!("{:#?}", job.journal));
    assert_eq!(rejection.0, 1);
    assert!(rejection.1.starts_with("duplicate_candidate"), "{}", rejection.1);
    assert!(
        !job.journal.iter().any(|entry| matches!(
            entry,
            JournalEntry::RunStarted { split: EvalSplit::Validation, .. }
        )),
        "a structural rejection spends no validation run: {:#?}",
        job.journal
    );
    assert!(
        !job.journal
            .iter()
            .any(|entry| matches!(entry, JournalEntry::Decided { .. })),
        "and reaches no decision"
    );
}

#[tokio::test]
async fn a_candidate_that_cannot_be_afforded_is_budget_exhausted_and_never_rejected() {
    let harness = Harness::new().await;
    let executor = script(base_executor(), "baseline", &VALIDATION_CASES, |_| fail());
    // Train is 1 x 6? No: train has one case, two trials, one cell = 2.
    // Validation is 6 cases x 2 trials x 2 cells = 24 and the reserved
    // held-out run is another 24, so 25 buys the train run and nothing else.
    let request = harness.request("exhausted", budgets(25));
    let outcome = drive(&harness, &request, &executor, &one_round_proposer()).await;

    assert_eq!(outcome.state, JobState::Exhausted);
    assert!(outcome.checkpoint.is_none());
    let job = load_job(&harness.launching.access, OWNER, "exhausted")
        .await
        .unwrap()
        .unwrap();
    assert!(
        job.journal
            .iter()
            .any(|entry| matches!(entry, JournalEntry::BudgetExhausted { round: Some(1), .. })),
        "{:#?}",
        job.journal
    );
    assert!(
        !job.journal
            .iter()
            .any(|entry| matches!(entry, JournalEntry::Decided { .. })),
        "a candidate that was never evaluated is not a rejection"
    );
}

#[tokio::test]
async fn a_baseline_edited_after_the_freeze_fails_the_job_before_any_further_spend() {
    let harness = Harness::new().await;
    let executor = script(base_executor(), "baseline", &VALIDATION_CASES, |_| fail());
    // A budget that buys the train run and nothing else, so the first pass
    // stops on budget with the job still Running.
    let request = harness.request("drifted", budgets(25));
    let first = drive(&harness, &request, &executor, &one_round_proposer()).await;
    assert_eq!(first.state, JobState::Exhausted);

    // An operator edits a document of the frozen closure, then the job is
    // driven again with room to spend.
    harness
        .launching
        .install(vec![(
            Collection::AgentContext,
            json!({
                "context_id": "monitor-context",
                "agent_did": OWNER,
                "display_name": "Monitor, renamed",
                "system_prompt": BASELINE_PROMPT,
            }),
        )])
        .await;
    let generous = harness.request("drifted", budgets(1_000));
    let outcome = drive(&harness, &generous, &executor, &one_round_proposer()).await;
    assert_eq!(
        outcome.state,
        JobState::Failed { reason: "baseline_drifted".into() },
        "{outcome:#?}"
    );
}
```

- [ ] **Step 2: Declare the module**

In `crates/gents/src/optimization/driver.rs`, add near the top, in rustfmt order:

```rust
#[cfg(test)]
mod matrix;
```

For `mod matrix;` to resolve beside a `driver.rs`, the file must be at `crates/gents/src/optimization/driver/matrix.rs`. That is the 2018-edition layout the repository already uses (`crates/gents/src/eval/runner/mod.rs` with `runner/embedded/`), so no rename of `driver.rs` is needed.

- [ ] **Step 3: Run the matrix, expecting failures to be real**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents --lib optimization::driver::matrix 2>&1 | tee /tmp/matrix.log; tail -40 /tmp/matrix.log`
Expected: 6 tests PASS. If the accept case reports `Inconclusive(TooFewCases)`, the definition lost a validation case; if it reports `Reject(NoImprovement)`, check `policy().max_rounds` is 1, because `alpha_effective` is `alpha_ppm / max_rounds` and a three-round policy needs p ≤ 16666 where six aligned cases give exactly 15625.

- [ ] **Step 4: Prove the scripted keys are the ones the runner asked for**

The `ScriptedExecutor` records every key it was called with. If a scenario's expected decision does not appear, the first thing to check is whether the script's keys matched: add, temporarily, `tracing::info!(?executor.calls)` after `drive`, run with `--nocapture`, and compare `attempt` (the runner's first attempt is `1`) and `cell_label` (`baseline` and `candidate`, the `CellRequest::label` values `run_request` sets) against `key`. Remove the line before committing; `println!` is never used.

- [ ] **Step 5: Commit**

```bash
git add crates/gents/src/optimization
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" \
  commit -m "test(optimization): the scripted driver matrix (#1455)

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

### PR 3 gate

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents --lib optimization 2>&1 | tee /tmp/pr3-lib.log; tail -20 /tmp/pr3-lib.log`
Run: `CARGO_BUILD_JOBS=4 cargo test -p gents --lib eval`
Run: `CARGO_BUILD_JOBS=4 cargo check --workspace --all-targets 2>&1 | tee /tmp/pr3-check.log; grep -c "^error" /tmp/pr3-check.log`
Expected: all PASS; the grep reports 0.
Run: `cargo fmt --all --check`

---

## PR 4: Promotion, revert and the live scenarios

Branch: `optimization/23-promote`, base `optimization/22-driver`. Deliverable: the operator's two verbs, the whole refusal matrix, and the two live scenarios the milestone is judged by.

### Task 1: `promote` and `revert`

**Files:**
- Create: `crates/gents/src/optimization/promote.rs`
- Modify: `crates/gents/src/optimization/mod.rs`

**Interfaces:**
- Consumes:
  ```rust
  // crates/gents/src/config_client/desired_state.rs
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
  ```
- Produces:
  ```rust
  pub struct Promotion { pub target_digest: String, pub previous_text: String, pub previous_digest: String }
  pub struct PromoteRefused { pub reason: &'static str, pub detail: String }
  pub fn promote_refused(error: &anyhow::Error) -> Option<&PromoteRefused>;
  pub async fn promote(access: &ConfigAccess, request: &JobRequest, digest: &str, by: &str) -> Result<Promotion>;
  pub async fn revert(access: &ConfigAccess, request: &JobRequest, digest: &str, by: &str) -> Result<()>;
  ```

- [ ] **Step 1: Expose the matrix harness**

In `crates/gents/src/optimization/driver.rs`, change `mod matrix;` to `pub(crate) mod matrix;`. In `crates/gents/src/optimization/driver/matrix.rs`, mark `Harness`, `BASELINE_PROMPT`, `CANDIDATE_PROMPT`, `VALIDATION_CASES`, `budgets`, `policy`, `one_round_proposer`, `base_executor`, `script`, `fail` and `drive` as `pub(crate)`, add two accessors to `impl Harness`:

```rust
    pub(crate) fn access(&self) -> &ConfigAccess {
        &self.launching.access
    }

    pub(crate) async fn install(&self, documents: Vec<(Collection, Value)>) {
        self.launching.install(documents).await;
    }
```

and add the two job drivers the promotion tests need:

```rust
/// A harness whose job accepted a candidate and reached `ReadyToPromote`.
pub(crate) async fn accepting_harness(job_id: &str) -> (Harness, JobRequest) {
    let harness = Harness::new().await;
    let executor = script(base_executor(), "baseline", &VALIDATION_CASES, |_| fail());
    let request = harness.request(job_id, budgets(1_000));
    let outcome = drive(&harness, &request, &executor, &one_round_proposer()).await;
    assert_eq!(outcome.state, JobState::ReadyToPromote, "{outcome:#?}");
    (harness, request)
}

/// A harness whose job finished without accepting anything.
pub(crate) async fn rejecting_harness(job_id: &str) -> (Harness, JobRequest) {
    let harness = Harness::new().await;
    let executor = script(base_executor(), "baseline", &VALIDATION_CASES[..3], |_| fail());
    let request = harness.request(job_id, budgets(1_000));
    let outcome = drive(&harness, &request, &executor, &one_round_proposer()).await;
    assert_eq!(outcome.state, JobState::NothingToPromote, "{outcome:#?}");
    (harness, request)
}
```

Rewrite the bodies of `a_candidate_that_wins_every_case_is_accepted_and_reaches_ready_to_promote` and `a_candidate_that_wins_only_half_the_cases_is_rejected_for_no_improvement` to obtain their harness from these helpers rather than repeating the setup, keeping every assertion they already make.

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents --lib optimization::driver::matrix`
Expected: PASS, still 6 tests.

- [ ] **Step 2: Write the failing test** — create `crates/gents/src/optimization/promote.rs` holding only this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::runner::freeze::tests::OWNER;
    use crate::optimization::driver::matrix::{
        accepting_harness, rejecting_harness, Harness, BASELINE_PROMPT, CANDIDATE_PROMPT,
    };
    use crate::optimization::job::{load_job, JobState};
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

    async fn checkpoint_digest(harness: &Harness, request: &JobRequest) -> String {
        let job = load_job(harness.access(), OWNER, &request.job_id)
            .await
            .unwrap()
            .unwrap();
        checkpoint(&job.journal)
            .expect("the harness drove an accepting job")
            .pack_digest
    }

    #[tokio::test]
    async fn a_promotion_writes_only_the_target_and_records_what_it_replaced() {
        let (harness, request) = accepting_harness("promote-once").await;
        let digest = checkpoint_digest(&harness, &request).await;
        let promotion = promote(harness.access(), &request, &digest, OWNER)
            .await
            .unwrap();

        assert_eq!(promotion.previous_text, BASELINE_PROMPT);
        assert_ne!(promotion.target_digest, promotion.previous_digest);
        assert_eq!(
            live_prompt(harness.access()).await.as_deref(),
            Some(CANDIDATE_PROMPT)
        );
        let job = load_job(harness.access(), OWNER, &request.job_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(derive_state(&job.journal), JobState::Promoted);
    }

    #[tokio::test]
    async fn a_revert_restores_the_previous_text_and_is_refused_once_the_target_moved() {
        let (harness, request) = accepting_harness("revert-once").await;
        let digest = checkpoint_digest(&harness, &request).await;
        let promotion = promote(harness.access(), &request, &digest, OWNER)
            .await
            .unwrap();

        revert(harness.access(), &request, &promotion.target_digest, OWNER)
            .await
            .unwrap();
        assert_eq!(
            live_prompt(harness.access()).await.as_deref(),
            Some(BASELINE_PROMPT)
        );
        let job = load_job(harness.access(), OWNER, &request.job_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            derive_state(&job.journal),
            JobState::ReadyToPromote,
            "a reverted job may be promoted again"
        );

        // Promote again, then edit the target by hand: the revert is refused
        // and the operator's edit survives.
        let second = promote(harness.access(), &request, &digest, OWNER)
            .await
            .unwrap();
        harness
            .install(vec![(
                Collection::AgentContext,
                json!({
                    "context_id": "monitor-context",
                    "agent_did": OWNER,
                    "display_name": "Monitor",
                    "system_prompt": "An operator wrote this by hand.\n",
                }),
            )])
            .await;
        let error = revert(harness.access(), &request, &second.target_digest, OWNER)
            .await
            .unwrap_err();
        assert_eq!(promote_refused(&error).unwrap().reason, "target_moved");
        assert_eq!(
            live_prompt(harness.access()).await.as_deref(),
            Some("An operator wrote this by hand.\n"),
            "the user's edit is preserved"
        );
    }

    #[tokio::test]
    async fn a_closure_edited_after_the_freeze_refuses_the_promotion_and_marks_the_job_stale() {
        let (harness, request) = accepting_harness("promote-stale").await;
        let digest = checkpoint_digest(&harness, &request).await;
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

        let error = promote(harness.access(), &request, &digest, OWNER)
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
        let job = load_job(harness.access(), OWNER, &request.job_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(derive_state(&job.journal), JobState::Stale);
    }

    #[tokio::test]
    async fn a_foreign_did_a_wrong_digest_and_an_unready_job_are_each_refused() {
        let (harness, request) = accepting_harness("promote-refusals").await;
        let digest = checkpoint_digest(&harness, &request).await;

        let foreign = promote(harness.access(), &request, &digest, "did:key:someone-else")
            .await
            .unwrap_err();
        assert_eq!(promote_refused(&foreign).unwrap().reason, "foreign_did");

        let wrong = promote(harness.access(), &request, "sha256:not-the-checkpoint", OWNER)
            .await
            .unwrap_err();
        assert_eq!(promote_refused(&wrong).unwrap().reason, "wrong_digest");

        assert_eq!(
            live_prompt(harness.access()).await.as_deref(),
            Some(BASELINE_PROMPT),
            "a refused promotion writes nothing"
        );

        let (rejecting, unready) = rejecting_harness("unready").await;
        let error = promote(rejecting.access(), &unready, "sha256:anything", OWNER)
            .await
            .unwrap_err();
        assert_eq!(promote_refused(&error).unwrap().reason, "not_ready");
    }

    /// Spec section 9: optimization is an operator's verb, never a model's.
    #[test]
    fn the_model_facing_config_tool_has_no_optimization_surface() {
        for name in crate::self_config::SELF_CONFIG_TOOL_NAMES {
            assert!(!name.contains("optim"), "the self-config tool set names {name}");
        }
        let source = include_str!("../self_config.rs");
        for word in ["optimization", "OptimizationJob"] {
            assert!(
                !source.contains(word),
                "self_config.rs mentions {word}; the optimization surface is the operator CLI's"
            );
        }
    }
}
```

If `self_config` is a directory module, change the `include_str!` path to `"../self_config/mod.rs"`; confirm by looking for `crates/gents/src/self_config.rs` versus `crates/gents/src/self_config/mod.rs`, and read that file for the exact spelling of `SELF_CONFIG_TOOL_NAMES` (it is referenced from `document_config/write_tool.rs:is_reserved_builtin_tool_name` as `crate::self_config::SELF_CONFIG_TOOL_NAMES`).

- [ ] **Step 3: Run it to see it fail**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents --lib optimization::promote`
Expected: FAIL to compile, `cannot find function promote in this scope`.

- [ ] **Step 4: Write the implementation** — above the test module in `promote.rs`:

```rust
//! Promotion and revert: the operator's two verbs.
//!
//! Both run in one transaction that expects the entire frozen closure and
//! writes only the target document. There is no force flag: on drift the
//! transaction rolls back with the live node untouched, a second transaction
//! journals the refusal, and the job is `Stale`. Nothing reverts on its own.

use anyhow::{Context, Result};

use crate::config_client::{apply_desired_state_plan, stale_expectation, ConfigAccess};
use crate::optimization::driver::JobRequest;
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
    let mut drifted = Vec::new();
    for document in frozen {
        let unchanged = live.iter().any(|candidate| {
            candidate.collection == document.collection
                && candidate.id == document.id
                && candidate.digest == document.digest
        });
        if !unchanged {
            drifted.push(DriftedRef {
                collection: document.collection.graphql_type().to_owned(),
                id: document.id.clone(),
            });
        }
    }
    for document in live {
        let known = frozen.iter().any(|candidate| {
            candidate.collection == document.collection && candidate.id == document.id
        });
        if !known {
            drifted.push(DriftedRef {
                collection: document.collection.graphql_type().to_owned(),
                id: document.id.clone(),
            });
        }
    }
    drifted
}

/// Load the job and check the two things both verbs require: who is asking,
/// and what state the job is in.
async fn job_in_state(
    access: &ConfigAccess,
    request: &JobRequest,
    by: &str,
    expected: JobState,
) -> Result<JobRecord> {
    let job = load_job(access, &request.owner, &request.job_id)
        .await?
        .ok_or_else(|| refused("unknown_job", format!("no job {:?}", request.job_id)))?;
    if by != job.origin.owner {
        return Err(refused(
            "foreign_did",
            format!("{by:?} does not own job {:?}", request.job_id),
        ));
    }
    let state = derive_state(&job.journal);
    if state != expected {
        let reason = if expected == JobState::Promoted {
            "not_promoted"
        } else {
            "not_ready"
        };
        return Err(refused(
            reason,
            format!("job {:?} is {}", request.job_id, state.label()),
        ));
    }
    Ok(job)
}

/// Write the retained checkpoint's text onto the live target, guarded by the
/// whole frozen closure.
pub async fn promote(
    access: &ConfigAccess,
    request: &JobRequest,
    digest: &str,
    by: &str,
) -> Result<Promotion> {
    let mut job = job_in_state(access, request, by, JobState::ReadyToPromote).await?;
    let retained = checkpoint(&job.journal).ok_or_else(|| {
        refused(
            "no_checkpoint",
            format!("job {:?} is ready but retained nothing", request.job_id),
        )
    })?;
    if retained.pack_digest != digest {
        return Err(refused(
            "wrong_digest",
            format!(
                "job {:?} retains {}, not {digest}",
                request.job_id, retained.pack_digest
            ),
        ));
    }

    // Rebuild the candidate from the job's own baseline pack and check it
    // still digests to what the journal recorded.
    let baseline = materialize_pack(&request.baseline_dir(), &request.owner, &request.behavior_id)?;
    let rebuild_dir = request.job_dir().join("promote-rebuild");
    if rebuild_dir.exists() {
        std::fs::remove_dir_all(&rebuild_dir)
            .with_context(|| format!("clearing {}", rebuild_dir.display()))?;
    }
    let rebuilt = materialize_candidate(&baseline, &request.owner, &retained.text, &rebuild_dir)?;
    if rebuilt.digest != retained.pack_digest {
        return Err(refused(
            "rebuild_mismatch",
            format!(
                "rebuilding the checkpoint gives {}, not the journaled {}",
                rebuilt.digest, retained.pack_digest
            ),
        ));
    }

    let (owner, job_id, text) = (
        request.owner.clone(),
        request.job_id.clone(),
        retained.text.clone(),
    );
    let applied = access
        .transact("optimization.promote", |txn| {
            let (owner, job_id, text) = (&owner, &job_id, &text);
            Box::pin(async move {
                let job = load_job_in_txn(txn, owner, job_id)
                    .await?
                    .context("the job disappeared mid-promotion")?;
                let closure = capture_closure(txn, owner).await?;
                let live = closure_digests(&closure)?;
                let drifted = between(&job.origin.closure, &live);
                if !drifted.is_empty() {
                    return Err(anyhow::Error::new(ClosureDrift(drifted)));
                }
                let target = &job.origin.target;
                let previous_text = current_text(&closure, target)?;
                let previous_digest = target_digest(&closure, target)?;
                let patched = apply_text(&closure, target, text)?;
                let plan = target_plan(&patched, target, &job.origin.closure)?;
                apply_desired_state_plan(txn, &plan).await?;
                let promotion = Promotion {
                    target_digest: target_digest(&patched, target)?,
                    previous_text,
                    previous_digest,
                };
                append_in_txn(
                    txn,
                    &job,
                    &JournalEntry::Promoted {
                        by: job.origin.owner.clone(),
                        target_digest: promotion.target_digest.clone(),
                        previous_text: promotion.previous_text.clone(),
                        previous_digest: promotion.previous_digest.clone(),
                    },
                )
                .await?;
                Ok(promotion)
            })
        })
        .await;

    match applied {
        Ok(promotion) => {
            tracing::info!(job_id = %request.job_id, by, "optimization checkpoint promoted");
            Ok(promotion)
        }
        Err(error) => {
            let Some(drifted) = drift_of(&error) else {
                return Err(error);
            };
            // The write rolled back; a second transaction records why.
            append(
                access,
                &mut job,
                JournalEntry::PromotionRefused {
                    drifted: drifted.clone(),
                },
            )
            .await?;
            tracing::warn!(job_id = %request.job_id, ?drifted, "promotion refused; the job is stale");
            Err(refused("stale_closure", format!("{drifted:?}")))
        }
    }
}

/// Write `previous_text` back, expecting the target to still hold exactly what
/// the promotion wrote.
pub async fn revert(
    access: &ConfigAccess,
    request: &JobRequest,
    digest: &str,
    by: &str,
) -> Result<()> {
    let job = job_in_state(access, request, by, JobState::Promoted).await?;
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
            format!(
                "job {:?} promoted {}, not {digest}",
                request.job_id, promoted.0
            ),
        ));
    }

    let (owner, job_id) = (request.owner.clone(), request.job_id.clone());
    let result = access
        .transact("optimization.revert", |txn| {
            let (owner, job_id, promoted) = (&owner, &job_id, &promoted);
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
                        format!(
                            "the target now digests to {live}, not the promoted {}",
                            promoted.0
                        ),
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
                append_in_txn(
                    txn,
                    &job,
                    &JournalEntry::Reverted {
                        by: job.origin.owner.clone(),
                    },
                )
                .await?;
                Ok(())
            })
        })
        .await;

    match result {
        Ok(()) => {
            tracing::warn!(job_id = %request.job_id, by, "promoted prompt reverted");
            Ok(())
        }
        Err(error) if stale_expectation(&error).is_some() => {
            Err(refused("target_moved", format!("{error:#}")))
        }
        Err(error) => Err(error),
    }
}
```

- [ ] **Step 5: Wire the module** — in `crates/gents/src/optimization/mod.rs` add `pub mod promote;` in rustfmt order and:

```rust
pub use promote::{promote, promote_refused, revert, PromoteRefused, Promotion};
```

- [ ] **Step 6: Run the tests**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents --lib optimization::promote 2>&1 | tee /tmp/promote.log; tail -40 /tmp/promote.log`
Expected: 5 tests PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/gents/src/optimization
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" \
  commit -m "feat(optimization): operator-only promote and revert (#1455)

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

### Task 2: The live accept and reject

**Files:**
- Create: `crates/gents/tests/optimization_live.rs`
- Modify: `crates/gents/tests/support/live_inference.rs`, `crates/gents/tests/eval_runner_canary.rs`

Both tests are `#[ignore]` and never run in CI. They use the real embedded runner and a real provider; only the proposer is scripted, because M6b ships no LLM proposer. An accept and a reject are produced by choosing which text the proposer offers, not by scripting the model.

**Interfaces:**
- Consumes:
  ```rust
  // gents::eval::runner::embedded
  impl EmbeddedExecutor { pub fn new(options: DocumentRuntimeOptions, runs_dir: PathBuf) -> Self; }
  impl EmbeddedHome { pub async fn create_temp(prefix: &str) -> Result<Self>; pub fn did(&self) -> &str; }
  // gents::optimization
  pub async fn run_job(access: &ConfigAccess, request: &JobRequest, executor: &dyn TrialExecutor,
      proposer: &dyn Proposer, registry: &CheckRegistry, policy: &PolicyV2,
      cancel: CancellationToken) -> Result<JobOutcome>;
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
//! provider. Never in CI.
//!
//! The subject is M3's `monitor-findings` pack and definition, which land on
//! `eval/30-monitor-prework`. Both are supplied by environment variable, so
//! this file compiles without that branch and runs only where it is present.
//! The provider is real and the runner is the embedded one.

mod support;

use std::path::{Path, PathBuf};

use gents::config_client::{
    apply_desired_state_plan, DesiredStateApplyDocument, DesiredStateApplyPlan,
};
use gents::eval::checks::CheckRegistry;
use gents::eval::runner::embedded::{EmbeddedExecutor, EmbeddedHome};
use gents::optimization::{
    run_job, Budgets, JobOutcome, JobRequest, JobState, PolicyV2, ScriptedProposer,
};
use gents::{Collection, ConfigAccess, DocumentRuntimeOptions};
use serde_json::{json, Value};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

/// The M3 subject pack directory: `eval/30-monitor-prework` builds one under
/// `packs/`. Point this at it.
const PACK_VAR: &str = "GENTS_OPTIMIZATION_LIVE_PACK";
/// A JSON file holding the `monitor-findings` `EvalDefinition` document.
const DEFINITION_VAR: &str = "GENTS_OPTIMIZATION_LIVE_DEFINITION";
/// The behavior inside that pack whose context is optimized.
const BEHAVIOR: &str = "monitor";
/// The deliberately thin baseline the job starts from, so there is room to
/// improve on a real provider.
const THIN_PROMPT: &str = "Look at the mailbox and say something.\n";

#[tokio::test]
#[ignore = "needs GENTS_LIVE_CONFIG_PROVIDER, GENTS_OPTIMIZATION_LIVE_PACK and a real backend"]
async fn live_accept_the_golden_prompt_beats_a_thin_one_and_reaches_ready_to_promote() {
    let fixture = Fixture::new("live-accept").await;
    // The candidate is the pack's own golden prompt, which M3 wrote to pass
    // these cases; the baseline is the thin one.
    let proposer = ScriptedProposer::new(vec![(
        fixture.golden_prompt.clone(),
        "restore the full monitor instructions".into(),
    )]);
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
#[ignore = "needs GENTS_LIVE_CONFIG_PROVIDER, GENTS_OPTIMIZATION_LIVE_PACK and a real backend"]
async fn live_reject_a_prompt_that_ignores_the_task_never_reaches_ready_to_promote() {
    let fixture = Fixture::new("live-reject").await;
    // A prompt that removes the output contract the checks grade.
    let proposer = ScriptedProposer::new(vec![(
        "Answer in one word. Do not use any tools.\n".into(),
        "shorter is better".into(),
    )]);
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
            std::env::var(PACK_VAR)
                .unwrap_or_else(|_| panic!("{PACK_VAR} must name the M3 monitor subject pack")),
        );
        let definition: Value = serde_json::from_slice(
            &std::fs::read(
                std::env::var(DEFINITION_VAR)
                    .unwrap_or_else(|_| panic!("{DEFINITION_VAR} must name the definition JSON")),
            )
            .expect("reading the definition"),
        )
        .expect("parsing the definition");

        let home = EmbeddedHome::create_temp("optimization-live").await.unwrap();
        let access = ConfigAccess::Local(home.node.clone());
        let owner = home.did().to_string();
        gents::ensure_agent_principal(home.node.as_ref(), &owner)
            .await
            .unwrap();
        let dirs = tempfile::tempdir().unwrap();

        let sidecar = Path::new("agent_behaviors").join(BEHAVIOR).join("system_prompt.md");
        let golden_prompt =
            std::fs::read_to_string(pack.join(&sidecar)).expect("the pack's behavior sidecar");
        let thin_pack = dirs.path().join("thin-pack");
        copy_tree(&pack, &thin_pack);
        std::fs::write(thin_pack.join(&sidecar), THIN_PROMPT).unwrap();

        let (endpoint, auth, model) = support::live_inference::live_provider_from_env();
        install(
            &access,
            vec![
                (Collection::EvalDefinition, with_owner(definition, &owner)),
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
                (
                    Collection::AgentContext,
                    json!({
                        "context_id": "monitor-context",
                        "agent_did": owner,
                        "display_name": "Monitor",
                        "system_prompt": THIN_PROMPT,
                    }),
                ),
                (
                    Collection::AgentBehavior,
                    json!({
                        "behavior_id": BEHAVIOR,
                        "agent_did": owner,
                        "display_name": "Monitor",
                        "context_id": "monitor-context",
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
            behavior_id: BEHAVIOR.into(),
            definition_id: "monitor-findings".into(),
            inference_profile_id: "live".into(),
            baseline_pack: thin_pack,
            trials_per_case: 2,
            budgets: Budgets {
                max_rounds: 1,
                max_case_trials: 1_000,
                max_tokens: u64::MAX,
                deadline_unix_secs: None,
            },
            max_text_bytes: 32 * 1024,
            seed_base: 7_000,
            jobs_dir: dirs.path().join("optimization/jobs"),
            runs_dir: dirs.path().join("eval/runs"),
            source_commit: "live".into(),
            source_dirty: false,
            concurrency: 2,
            max_infra_retries: 1,
            breaker_threshold: 8,
            deadline_secs: Some(900),
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
        let policy = PolicyV2 {
            max_rounds: 1,
            max_reruns: 1,
            ..PolicyV2::uncalibrated()
        };
        run_job(
            &self.access,
            &self.request,
            &executor,
            proposer,
            &CheckRegistry::builtin(),
            &policy,
            CancellationToken::new(),
        )
        .await
        .unwrap()
    }
}

fn with_owner(mut definition: Value, owner: &str) -> Value {
    definition["agent_did"] = Value::String(owner.to_owned());
    definition
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

- [ ] **Step 3: Prove it compiles and stays out of CI**

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents --test optimization_live --no-run`
Expected: compiles.

Run: `CARGO_BUILD_JOBS=4 cargo test -p gents --test optimization_live`
Expected: `2 ignored`, 0 run.

- [ ] **Step 4: Commit**

```bash
git add crates/gents/tests
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" \
  commit -m "test(optimization): live accept and reject scenarios, ignored by default

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

### Task 3: The four PR descriptions

**Files:**
- Create: `<worktree>/.superpowers/sdd/2026-09-22-optimization-driver/pr-descriptions.md`

The coordinator never pushes and never opens a PR; it writes the descriptions the orchestrator uses. One section per PR, each carrying baseline, owners, deletions and validation, per CLAUDE.md's stacked-PR rule.

**Interfaces:**
- Consumes: the four branch heads produced by PRs 1 to 4, and the M6b base commit recorded in the ledger at first use.
- Produces: the file itself.

- [ ] **Step 1: Write the file**

````markdown
# M6b pull request descriptions

## PR 1 — `optimization/20-job` → the M6b base

**Baseline.** The M6b base: `eval/13-runner-embedded` (`028aa4188`) with
`eval/06-protected` (`9c8a590e0`), `optimization/01..03` (`f3ebf6b9f`) and
`optimization/10..12` (`d0ab985ab`) rebased on top, in that order. Record the
resulting commit here.

**What it adds.** The `OptimizationJob` collection — SDL, both catalogs, the
protocol mirror, the migration baseline pin, local-audit classification — the
closure target, and the typed journal whose `journal_len` guard refines
`Optimization.appendIf`.

**Owners.** The journal is owned by `gents::optimization::job` and written only
by the driver, as the owner DID. `state` is derived; it is not a second request
lifecycle and no runtime reconciles it. The collection stays out of the
`Collection` enum, out of `BRANCHABLE_COLLECTION_NAMES`, and out of all three
P2P arrays in `agent/p2p_reconcile/templates.rs`.

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

**What it adds.** `MaterializedPack` and the candidate materializer, the
`Proposer` trait with `ScriptedProposer`, and the pure structural gate.

**Owners.** The proposer holds no `ConfigAccess`; `target.rs` and `subject.rs`
are the only builders of a patch. `ProposalInput`'s field list is pinned by a
test, because it is the statement of what an optimizer may learn. The gate
validates the candidate pack's own reference closure with the rule
`validate_desired_state_plan` applies to a live one; the live form runs inside
`promote`'s transaction in PR 4.

**Deletions.** None.

**Validation.** `cargo test -p gents --lib optimization`;
`cargo check --workspace --all-targets`; `cargo fmt --all --check`.

🤖 Generated with [Claude Code](https://claude.com/claude-code)

## PR 3 — `optimization/22-driver` → `optimization/21-proposer`

**Baseline.** PR 2's head.

**What it adds.** The verdict-to-evidence projection, the eval-run plans and
the budget, `run_job`, and the scripted matrix: accept, reject for no
improvement, reject for a case regression, structural reject with zero
validation runs, budget exhaustion, and baseline drift.

**Owners.** The driver is the sole writer of the job. Runs are ordinary
`EvalRun`s with `purpose: "optimization:<job_id>"`; grading, seeding, resume
and the NotEvidence breaker stay the runner's. The latest-attempt-per-slot
projection in `optimization::evidence` is the one M4's report should reuse
rather than re-derive.

**Deletions.** None.

**Validation.** `cargo test -p gents --lib optimization`;
`cargo test -p gents --lib eval`; `cargo check --workspace --all-targets`;
`cargo fmt --all --check`.

🤖 Generated with [Claude Code](https://claude.com/claude-code)

## PR 4 — `optimization/23-promote` → `optimization/22-driver`

**Baseline.** PR 3's head.

**What it adds.** `promote` and `revert` over the Track 0 compare-and-set, the
refusal matrix (stale closure, moved target, foreign DID, wrong digest, unready
job), the test that the model-facing `config` tool has no optimization surface,
and the two live scenarios, both `#[ignore]`.

**Owners.** Promotion expects the entire frozen closure and writes only the
target document, through `DesiredStateApplyPlan::with_expected`. There is no
force flag and nothing reverts on its own. This is the first non-test caller of
`with_expected`.

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

No `lake build` is required: this plan adds and edits no Lean. The whole stack is validated on PR 4's head, and the stack merges in order, PR 1 first.

---

## Self-review

### 1. Spec coverage

| Spec section | Requirement | Where |
|---|---|---|
| 1 | Baseline subject is a pack snapshot naming `behavior_id`; no credentials | PR 2 Task 1, `materialize_pack`; inference rides on the profile the `CellRequest` names, never in the pack |
| 1 | Target is `AgentContext.system_prompt` or a Task's `prompt_template`; `TargetField` allows nothing else | PR 1 Task 3, `TargetField`. The Task variant is declared and refused — defect 2 |
| 1 | Candidate is the same snapshot with one field patched, giving a new pack digest; the digest is the dedup key | PR 2 Task 1 `materialize_candidate`; PR 2 Task 3 `duplicate_candidate` |
| 1 | Frozen closure is `(collection, owner, id, digest)` read through `ConfigReferences::load_in_txn` | PR 1 Task 3 `capture_closure`, `closure_digests`, `FrozenDocument` |
| 2 | `Proposer` trait, `ProposalInput`, `Proposal`; proposer never holds `ConfigAccess`; `target.rs` builds the patch | PR 2 Task 2; Global Constraint 11 |
| 2 | The first implementation is a scripted proposer | PR 2 Task 2 `ScriptedProposer` |
| 3.1 | Train run: one `EvalRun`, one cell, the checkpoint, `purpose: "optimization:<job_id>"` | PR 3 Task 2 `train_plan`, `run_request` |
| 3.2 | Propose returns a text and a rationale | PR 3 Task 3, `JournalEntry::Proposed` |
| 3.3 | Structural gate before any validation spend; failure journaled with diagnostics | PR 2 Task 3; PR 3 Task 3; matrix `a_repeated_candidate_is_structurally_rejected_and_costs_no_validation_run` |
| 3.4 | Validation run: two cells, shared seeds | PR 3 Task 2 `validation_plan`; one `seed_base` per run is how the runner shares a seed |
| 3.5 | Decide from `EvalVerdict` rows; on `Inconclusive` re-run with a new `seed_base`; re-runs add pairs | PR 3 Task 1 `paired_evidence`; PR 3 Task 3's attempt loop reads every attempt's rows together |
| 3.6 | Accept makes the candidate the checkpoint; the job returns the retained checkpoint | PR 1 Task 4 `checkpoint`; its test asserts "never the best seen" |
| 3 | Rejection memory is the journal; `Inconclusive` and budget-exhausted rounds are never negatives | PR 3 Task 3 `rejections` |
| 4 | `decide` is pure, `PolicyV2` frozen into the job | M6a; PR 1 Task 4 `JobOrigin::policy` |
| 4 | `alpha_effective = alpha / max_rounds` | M6a `alpha_effective_ppm`; PR 3 Task 3 `check_policy` binds the divisor to the budget |
| 4 | Cost sub-gate skipped and journaled when usage is missing | PR 3 Task 1 `token_totals` (`tracing::info!`); `DecisionSummary::cost_skipped` |
| 4 | Two modes: `Improve` on validation, `Confirm` on held-out | PR 3 Task 3 |
| 5 | One local-only `OptimizationJob`: frozen `origin`, append-only `journal` guarded by length, derived `state` not named `lifecycle_state` | PR 1 Tasks 1 and 4 |
| 5 | In `LOCAL_AUDIT_COLLECTION_NAMES`; out of `Collection` and every P2P list; no `DatastoreToolSurface` may name it | PR 1 Task 1 Steps 4 and 9; PR 1 Task 2 |
| 5 | `origin` freezes target, closure digests, subject pack digest and `behavior_id`, definition ref, policy, `trials_per_case`, budgets, owner DID | PR 1 Task 4 `JobOrigin` |
| 5 | The ten journal entries | PR 1 Task 4 `JournalEntry` |
| 5 | `summary` is a convenience; decisions recompute from verdicts and a mismatch is flagged | PR 3 Tasks 1 and 3 `recompute_decisions`; matrix asserts no mismatch on the accepting job |
| 6 | The state diagram | PR 1 Task 4 `JobState`, `derive_state`; PR 3 Task 3 |
| 6 | Interruption: replay the journal; a `RunStarted` without a `Decided` resumes that run | PR 3 Task 2 `execute_run` resumes an existing run; PR 3 Task 3 `round_is_closed`, `proposed_for`, `run_started` |
| 6 | Baseline drift → `Failed(baseline_drifted)` before further spend | PR 3 Task 3; matrix `a_baseline_edited_after_the_freeze_fails_the_job_before_any_further_spend` |
| 6 | Definition drift → `Failed(definition_changed)` | PR 3 Task 3 |
| 6 | Budgets checked before every run; held-out cost reserved at freeze; an unevaluated candidate is `BudgetExhausted` | PR 3 Tasks 2 and 3; matrix `a_candidate_that_cannot_be_afforded_is_budget_exhausted_and_never_rejected` |
| 6 | Finalization: one held-out run in `Confirm` mode; `ReadyToPromote`/`Failed(held_out_*)`; never touched when no round accepted; `NothingToPromote` or `Exhausted` otherwise | PR 3 Task 3; matrix asserts the held-out split is untouched when nothing accepted |
| 6 | The driver is the only writer of the job, as the owner DID | PR 3 Task 3; every append goes through `job::append` |
| 7 | `promote`: require `ReadyToPromote`, the digest, a re-read closure, a rebuilt patch, a plan that writes only the target and expects the whole closure, then `Promoted` | PR 4 Task 1 |
| 7 | On drift the transaction rolls back, a second transaction journals `PromotionRefused`, the job is `Stale`; no force flag; owner DID only | PR 4 Task 1 and its tests |
| 7 | `revert` writes `previous_text` back through the same compare-and-set, expecting what was promoted | PR 4 Task 1 `revert` |
| 8 | Lean keeps its model; no Lean change in M6b | Global Constraint 9; `Proofs/Conformance/Optimization.lean` emits no journal cases, so no consumer is written |
| 9 | Unit: permutation test, too-few-cases, Bonferroni, cost gate with the missing-usage skip, totality | M6a's 11 policy tests, unchanged |
| 9 | End to end, deterministic, in `cargo test -p gents`: accept; `no_improvement`; `case_regression`; structural reject with zero validation runs; budget exhaustion; baseline drift; feedback only from the train run; held-out only at finalize | PR 3 Task 4 matrix (6 tests); PR 3 Task 1 `feedback_reaches_the_proposer_as_check_name_score_and_text_only` |
| 9 | `cost_regression`; inconclusive-then-rerun-then-inconclusive; too few cases; held-out regression; definition drift; resume at every boundary; a decision on an invalidated run flagged | Partly. Defects 5, 6 and 7 |
| 9 | Promotion: promote once; stale closure refused with the live node unchanged; an edited target refused with the user's edit preserved; a foreign DID, a wrong digest, an unready job refused; revert restores and is refused if the target moved | PR 4 Task 1's four tests |
| 9 | The model-facing `config` tool exposes no optimization resource or verb | PR 4 Task 1 `the_model_facing_config_tool_has_no_optimization_surface`; PR 1 Task 2 for the datastore surface |
| 9 | Live, never in CI: one accept and one reject on `monitor-findings` | PR 4 Task 2, both `#[ignore]`. Defect 3 |

### 2. Placeholder scan

Searched the plan for "TBD", "TODO", "implement later", "fill in details", "add appropriate error handling", "handle edge cases", "write tests for the above" and "similar to Task": no occurrences. Every code step carries a code block. Three places ask the implementer to read a value out of the repo rather than stating it, each naming the file and what to look for:

- The `OptimizationJob` migration pin (PR 1 Task 1 Step 7): the `bafyrei…` VersionID exists only as the output of `canonical_catalog_pins_for_authoring`, and the plan gives the command and the exact assertion text to read it from.
- `load_pack_config`'s parameter list (PR 2 Task 1): the plan names `crates/gents/src/eval/runner/freeze.rs`, `fn load_pack`, `CellSource::Directory` arm, and says to copy the call verbatim.
- `SELF_CONFIG_TOOL_NAMES` and whether `self_config` is a file or a directory module (PR 4 Task 1 Step 2): the plan names both candidate paths and the existing reference in `write_tool.rs`.

Two forward references were found and fixed rather than left: `target.rs` moved from PR 2 into PR 1 Task 3, because `JobOrigin` is declared against `Target` and `FrozenDocument`; and `recompute_decisions` is written in PR 3 Task 3 rather than Task 1, because it calls `driver::load_definition`, with Task 1 Step 4 saying explicitly what to remove and Task 3 Step 4 saying what to restore.

### 3. Type consistency

- `evidence_from_pairs`, not `evidence_from_paired`: used consistently in PR 3 Task 1 and recorded under Deviations.
- `Checkpoint` is defined once (PR 1 Task 4) and consumed by `JobOutcome` (PR 3 Task 3) and `promote` (PR 4 Task 1); it carries `round`, `text` and `pack_digest` in all three.
- `BASELINE_CELL` and `CANDIDATE_CELL` are defined in `evidence.rs` (PR 3 Task 1) and used by `driver.rs`'s plans (Task 2), `paired_evidence` (Task 1) and `recompute_decisions` (Task 3). The matrix scripts the same two labels, because `run_request` sets `CellRequest::label` equal to `cell_id`.
- `DecisionSummary` (PR 1 Task 4) is built by `summary_of` from `DecisionReport` (M6a) in PR 3 Task 3; every field of the former has a same-named source on the latter.
- `DriftedRef` is defined in `job.rs` (PR 1 Task 4) and produced in `promote.rs` (PR 4 Task 1) from both `ClosureDrift` and `StaleExpectation::drifted`.
- `JobRequest::baseline_dir`, `candidate_dir` and `job_dir` are defined in PR 3 Task 2 and used by PR 3 Task 3 and PR 4 Task 1; `promote` reads the baseline through `baseline_dir()`, the same directory `run_job` wrote.
- `MaterializedPack::prompt_asset` is `Option<String>` everywhere; `structural_gate`'s `allowed` defaults to `"pack_config.json"` when it is `None`, matching `materialize_candidate`'s inline branch.
- `Target::field.collection()` is used for both the promotion plan's collection and the revert expectation's, so the two cannot disagree.
- `run_job`'s parameter order — `access, request, executor, proposer, registry, policy, cancel` — is identical in PR 3 Task 3, the matrix's `drive`, and the live fixture's `drive`.

---

## Plan defects I could not resolve

For the orchestrator. Each is a spec requirement with no clean home in M6b, or a decision above the coordinator's authority.

1. **The `gents optimization show | promote | revert` CLI has no owner here.** Spec section 7 states both verbs as CLI commands and section 5 gives `optimization show` the job of recomputing decisions and flagging mismatches. The brief scopes M6b to the library, and the umbrella puts every CLI in spec 4a (M4). This plan ships `promote`, `revert` and `recompute_decisions` as library functions with their whole refusal matrix, and no `gents` subcommand. Someone must decide whether the wrappers land in M4's CLI PR or in a fifth M6b PR. Until then no operator can promote anything without writing Rust.

2. **`TargetField::TaskPromptTemplate` is declared and refused.** Spec section 1 allows a Task's `prompt_template` as a target and section 3 requires the gate to check that a Task template keeps its placeholder set. The brief fixes every M6b candidate as one changed `AgentContext.system_prompt`, and a Task target is not a pack field at all — a Task lives in the owner's closure, not in the subject pack — so the candidate-pack construction this plan is built on does not extend to it. `structural_gate` returns `unsupported_target`. The placeholder-set check is unwritten and unowned.

3. **The live scenarios depend on M3, which is not in the M6b base.** Spec section 9 names `monitor-findings`; that pack and definition are on `eval/30-monitor-prework` (`0425499b1`), a branch off `eval/03-definition`, not in this stack. PR 4 Task 2 takes both by environment variable so the file compiles and stays ignored without them, but nobody can actually run the accept or the reject until M3 and M6b meet. The orchestrator should decide whether M6b rebases M3 in, whether M3 merges first, or whether the live scenarios move to a follow-up that sits above both.

4. **Policy defaults are uncalibrated and the matrix pins `max_rounds: 1` to work around it.** Spec section 4 says the defaults are placeholders until M5's A/A calibration sets them, and states plainly that at `alpha = 0.05` with three rounds a job needs at least six validation cases. The matrix therefore uses a one-round policy, where six cases give p = 15625 ppm against an `alpha_effective` of 50000 ppm. A three-round job needs eight validation cases to accept anything at that scoring, and the fixture has six. The matrix proves the mechanism, not the calibration. M5 has not run.

5. **Three of spec section 9's end-to-end cases are not in the matrix.** `cost_regression` needs the scripted executor to report differing `TrialUsage`, which `ScriptedExecutor::passed_evidence` does not set (it builds `TrialUsage::default()`), so scripting a cost regression needs either a new constructor on the scripted executor — a change to `gents::eval::runner`, which standing rule 1 freezes for `documents` but not for `scripted`, so it is a coordinator ruling — or a hand-built `TrialEvidence`. `inconclusive → re-run → inconclusive` needs a slot that produces no evidence, which also trips the runner's retry ladder and its NotEvidence breaker; the interaction between `max_infra_retries`, `breaker_threshold` and `max_reruns` was not designed here. `too_few_cases` needs a definition with fewer than six validation cases, which is a second fixture. All three are unit-tested in M6a's policy suite; what is missing is the end-to-end form.

6. **"Resume at every run boundary yields the same journal" is only partly covered.** Spec section 9 asks for a resume test at every boundary. The matrix's drift scenario drives the same job twice and proves replay does not re-propose, and `execute_run` resumes a run whose row exists, but there is no fault-injection seam on the driver comparable to the runner's `Recorder`/`FaultingRecorder`. Adding one means a trait around `job::append` and `execute_run`, which is a design decision this plan did not want to make speculatively.

7. **"A decision on an invalidated run is flagged" has the mechanism but no test.** `recompute_decisions` returns `DecisionMismatch::invalidated`, and the matrix asserts the accepting job reports no mismatches. Nothing calls `gents::eval::invalidate_run` on one of a job's runs and asserts the flag comes back true. That test belongs beside `optimization show`, which does not exist here (defect 1).

8. **`optimization::evidence` duplicates a projection M4 will need.** The latest-attempt-per-slot rule and the `VerdictRecord → VerdictView` mapping are exactly what the umbrella's M4 note (section 7) specifies for the report, and `gents::eval::report` is spec 4a's module. M6b needs them now and cannot wait. Either M4 reuses `gents::optimization::evidence` — which inverts the layering, since optimization is meant to be a consumer of eval, not a supplier to it — or the functions move into `gents::eval::scoring` later, which is a frozen module. The orchestrator should pick the destination before M4 writes its own copy.

9. **The M6b base is four unmerged branches deep and will move.** Track 0 (`optimization/01..03`) targets `main` and may merge independently; `eval/06-protected` is M1's and may merge with the rest of M1. Every such merge invalidates the rebase recipe at the top of this plan. Nothing in the plan detects that, and the ledger's recorded base commit is the only record. Whoever rebases must re-run PR 1's catalog and migration tests, because a moved catalog changes the pin ordering that `default_baseline_matches_ordered_protocol_catalog` enforces.

10. **The job's directory has no retention owner.** Every round writes a candidate pack under `<jobs_dir>/<job_id>/rounds/<round>/candidate`, and `promote` writes another under `promote-rebuild`. The runner's own run directories have the same property and the umbrella defers retention TTLs to spec 4b. Nothing in M6b deletes any of it, and a job's directory holds candidate prompts, which is why the collection is local-audit. The filesystem side of that classification is unowned.

