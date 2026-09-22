# Eval Definition and Checks (M3) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn the golden monitor subject and its twenty pre-work cases into the first decision-grade eval definition: stage-level captures in the runner, a case-sidecar rule in the pack loader, nine builtin checks, the `eval_monitor_findings` definition pack, and one live calibration run followed by one fix pass.

**Architecture:** Three stacked PRs. PR 1 (`eval/40-capture`) consumes the pre-landed M1 amendment: the runner copies each stage's `capture` into its `StageSpec`, reads the breaker threshold from the frozen origin and writes `evidence_digest` into `TrialCompletion`; the loader inlines `./` case sidecars of an `EvalDefinition`. PR 2 (`eval/41-checks`) adds nine pure checks over the `items` capture, one file each, over one shared-predicate module, and bumps the registry to `"2"`. PR 3 (`eval/42-definition`) adds the conversion script and its output, the definition pack, the subject pack, a loader test, a scripted-executor integration test, the env-gated calibration run and the fix pass.

**Tech Stack:** Rust 1.97.1, tokio, `serde_json`, DefraDB via `gents::defra_node::EmbeddedNode`, `tempfile`, `tracing`/`tracing-subscriber`; Python 3 standard library for the one-time conversion script.

**Spec:** `docs/superpowers/specs/2026-09-22-eval-definition-and-checks-design.md` (approved 2026-09-22). Umbrella: `2026-09-21-eval-and-optimization-umbrella.md`. Contract: `2026-09-21-eval-core-contract-design.md`. Runner: `2026-09-21-eval-runner-design.md`. Inputs: `eval/30-monitor-prework` at `0425499b1` (`packs/eval_monitor/`, `cases/*.json`, `CHECKS.md`).

**Depends on:** the M3 base: `eval/13-runner-embedded` rebased onto the amended `eval/05-contract` (hash filled in by the orchestrator at spin-up). This plan runs under `docs/superpowers/orchestration/2026-09-21-parallel-coordinators.md`.

## Given by the orchestrator, not implemented here

The orchestrator lands these as one commit on `eval/05-contract` before M3 starts. This plan consumes them and never edits `document_config/eval_definition.rs` or `eval/documents.rs` except the test literals Task 1 names.

- `EvalStage.capture: Vec<EvalCapture>`, where `EvalCapture` is `#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]` with `Documents { name: String, collection: String, filter: serde_json::Value, #[serde(default)] fields: Vec<String> }` and `File { name: String, glob: String }`, and `EvalCapture::name(&self) -> &str`. Default empty, omitted when empty.
- `EvalDefinition::validate` rejects duplicate `(stage, check)` refs (`"... has duplicate check <name>"`) and duplicate capture names on one stage, empty capture names, empty collections and empty globs.
- `RunOrigin.breaker_threshold: u32` with `#[serde(default = "default_breaker_threshold")]`, `pub fn default_breaker_threshold() -> u32 { 5 }`.
- `TrialCompletion.evidence_digest: Option<String>`, `#[serde(default, skip_serializing_if = "Option::is_none")]`.

### Preconditions the plan found missing from the in-progress amendment (read 2026-09-22 in `gents-eval-05-contract`)

- **P1 (blocks Task 2).** `EvalCapture` is not in the `pub use eval_definition::{…}` list at `crates/gents/src/document_config/mod.rs:42-45`, and `mod eval_definition;` is private, so `crate::document_config::EvalCapture` does not resolve from `eval::runner`. The amendment commit must add `EvalCapture` to that list. If it did not, Task 2 stops and messages the orchestrator (rule 1).
- **P2 (blocks Task 3 and the three `file-*` cases).** Spec section 2 authors fixture files inline (`"fixtures": {"files": [{"path", "contents"}]}`), and three pre-work cases do so. The frozen `EvalFixtures` has only `assets`, `documents`, `schemas`; `assets` are read from the *subject* pack (`fixtures(&cell.pack_dir, …)` in `runner/mod.rs`), which must stay test-free. The amendment needs `EvalFixtures.files: Vec<EvalFixtureFile>` (default empty, omitted when empty, `deserialize_default_on_null` like its siblings) and `pub struct EvalFixtureFile { pub path: String, pub contents: String }` (`deny_unknown_fields`), re-exported from `crate::document_config`. If the orchestrator rules otherwise, Task 3 is dropped and Task 10 stops before converting the three `file-*` cases.

## Deviations from the spec, decided at planning

- **The amendment is not PR 1's work.** Spec section 7 puts it in PR 1; it is pre-landed on the base. Task 1 makes the base compile against it and implements the runner side only.
- **The definition lives inline in `pack_config.json`.** Spec section 1 shows exactly that; the decision table's `eval_definitions/monitor-findings.json` is not created (its kebab-case file name would also fail `has_canonical_asset_spelling` in `crates/gents/src/pack_asset_path.rs`).
- **Case files are `cases/<case_id with "-" replaced by "_">.json`.** `has_canonical_asset_spelling` admits only `[a-z0-9_]` segments; `case_id` values stay kebab-case inside the files.
- **The capture filter is `{"requester_did": {"_eq": "$trial"}}`**, not the bare `{"requester_did": "$trial"}` of spec section 2 and the pre-work: DefraDB filter fields take operator blocks (every filter in the runner and its tests uses `_eq`). `fields` is `["title", "summary", "payload", "status"]`.
- **`split: "test"` converts to `"held_out"`** (the four pre-work cases use a value `EvalSplit` does not have) and **`axis` is dropped** (`EvalCase` is `deny_unknown_fields`); the axis is recorded in the definition pack's README.
- **Keyword matching is token-sequence matching** (spec section 3, "token or hyphen-stem"), superseding CHECKS.md's "substring". Text and keyword are split into lowercase runs of alphanumeric characters; a keyword matches when its runs occur consecutively in the text's runs. `api` matches `api-gateway`, `sync-worker` matches `sync_worker`, `88` matches `88%`; `disk` does not match `disks`.
- **`mailbox_text_excludes` reads `payload` through its parsed findings** (CHECKS.md "Row text ban"), falling back to the raw payload text only when it does not parse, so JSON keys such as `state` never trigger a ban.
- **One extra grader reason code, `missing_field`,** for a row that lacks a field the check reads (the capture did not select it). It closes CHECKS.md's open item: a capture without `title` can no longer make a ban pass silently. A field present as `null` is the subject's doing and is judged, never `missing_field`.
- **Every check's `raw` carries `rows`,** and `findings_match` failures carry the findings and per-matcher misses. `EvalVerdict` is the local-audit collection (`eval/documents.rs`, `TrialCompletion` doc comment), and the calibration report is read from these rows.
- **The breaker threshold is read from the origin only.** `RunOrigin` deserializes a missing threshold as `5`, so a "`run.json` fallback" for the threshold can never be reached through the typed row. `run.json` is still written (one release) and is read at resume only for the request-level captures; its absence no longer refuses a resume.
- **`TrialCompletion.evidence_digest` is `Some` when at least one stage ran, `None` when nothing did** (the amendment's doc: "absent when nothing was captured").
- **`TrialSpec.captures` moves to `StageSpec.captures`.** One owner per stage: the stage's own list, else the run's request-level list.
- **The definition pack is registered in `packs/catalog.json`,** because `crates/gents/build.rs` refuses a `packs/` directory with a `manifest.json` that the catalog omits; `pack::tests::every_configuration_pack_declares_slots_and_authors_no_inference_documents` gains an exemption for definition-only packs.
- **`packs/eval_monitor/cases/` is not carried.** The subject pack is copied from `0425499b1` byte-identically except that `cases/` (the retired shorthand) is removed and `CHECKS.md` (undeclared) is updated. The script reads the pre-work cases from `git archive 0425499b1`.
- **The calibration runs once per split** (three runs), because a run selects one split and the cases span train, validation and held-out.
- **The fix pass cannot edit the umbrella** (rule 8: the design worktree is read-only for coordinators). It records the digest in the ledger and messages the orchestrator, who records it.

## Global Constraints

- Stack: `eval/40-capture` on the M3 base → `eval/41-checks` → `eval/42-definition`. Each PR targets its parent. Worktrees from the main checkout: `make worktree BRANCH=<branch> DIR=/Users/iron-arch-mage/Repos/Source/gents-eval-<nn>-<name> BASE=<parent>`.
- **Interface freeze (rule 1).** `gents::eval::{outcome, scoring, documents}` and `EvalDefinition` are frozen at the M3 base. A task that needs a change there stops and messages the orchestrator.
- **Build budget (rule 2).** One cargo invocation at a time per workspace, `CARGO_BUILD_JOBS=4`. Foreground only: never background, never `sleep`, never poll. Long output goes to a log file that is then grepped: every run command below writes `"$LOG"` (set `LOG=$TMPDIR/m3.log`) and greps it.
- **Git (rule 3).** Commit with `git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" commit`, message ending with `$TRAILER`, where `TRAILER="Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"` for Opus and `TRAILER="Co-Authored-By: Grok 4.7 <noreply@x.ai>"` for Grok. Never push, never open a PR, never change git config.
- **Repo gates (rule 4).** `cargo fmt --all --check` exits 0 before every commit; `mod`/`use` lines in rustfmt order. Escape every interpolated GraphQL string with `graphql::escape_graphql_string()`. Never emit `[]` in a mutation. `tracing`, never `println!`. No `unwrap`/`expect` on input-dependent paths outside tests. Known unrelated failures on this machine: `generated_r5_cross_principal_cases_drive_production_dispatch` and `e2e_triggers::event_source_trigger_p2p_e2e::p2p_replicated_doc_fires_event_trigger` (libp2p dial timeouts).
- **Models, process, reporting, scope (rules 5–8).** Implementers and reviewers dispatched with an explicit model; subagent-driven development with ledger at `<worktree>/.superpowers/sdd/2026-09-22-eval-definition-and-checks/progress.md`; rulings as `Ruling: <what> — <why> — <cost if wrong>`; message the orchestrator only at task complete, ruling, blocked, plan defect, plan complete; nothing outside the assigned worktree is written.
- **Check contract (spec section 3).** Every new check is pure over one `StageEvidence`, reads the capture named `items`, has `version() == "1"`, and returns: `Passed` with `score_bp: Some(10000)` on a pass; `ModelAcceptance` with `score_bp: Some(0)` on a subject failure; `Grader` with `score_bp: None` for `bad_params`, `missing_capture` and `missing_field`. Every `raw` is an object with a closed `reason_code` and a `detail`. `CHECK_REGISTRY_VERSION = "2"`. `captured_rows_count` stays registered.
- **Nothing protected enters a `TrialSpec`:** no check names or params, tiers, splits, `case_id` (except `script_key` for the scripted executor), or other cases. Captures are not protected.
- `EvalRun.origin` and `EvalTrial.completion` carry only ids, closed enums, counts, digests and timestamps.
- The subject pack's declared assets are byte-identical to `0425499b1`; its digest must not move. `packs/eval_monitor/README.md` is a declared asset and is not edited.
- The definition: `definition_id: "monitor-findings"`, `comparability_version: 1`, every check `tier: "acceptance"`, `weight: 1`. The `expected` shorthand is retired.
- `run.json` and `evidence.json` are still written for one release.
- The conversion script is Python 3, standard library only, deterministic (same input, byte-identical output), run once and committed with its output.
- No Lean change: M3 adds grading predicates, not transitions or provider input.

## File Structure

| File | PR | Responsibility |
|---|---|---|
| `crates/gents/src/eval/runner/freeze.rs` | 1 | origin carries `breaker_threshold`; resume reads it from the origin; `run.json` optional at resume |
| `crates/gents/src/eval/runner/mod.rs` | 1 | `stage_specs` (stage captures with run-level fallback), inline fixture files, `completion` writes `evidence_digest` |
| `crates/gents/src/eval/runner/executor.rs` | 1 | `StageSpec.captures`; `impl From<&EvalCapture> for Capture`; `TrialSpec.captures` removed |
| `crates/gents/src/eval/runner/embedded/executor.rs` | 1 | a stage reads its own `captures` |
| `crates/gents/src/eval/runner/plan.rs`, `grade.rs` | 1 | test literals for the amended types |
| `crates/gents/src/pack/loader.rs`, `pack/loader/tests.rs` | 1 | `hydrate_eval_cases`: `./` case sidecars of an `EvalDefinition` |
| `crates/gents/src/eval/checks/mailbox.rs` | 2 | shared predicates: `items` rows, fields, payload parse, tokens, keyword match, verdict builders, params |
| `crates/gents/src/eval/checks/{mailbox_item_count, mailbox_open_row, mailbox_text_excludes}.rs` | 2 | row-level checks |
| `crates/gents/src/eval/checks/{payload_well_formed, finding_pairs_unique, finding_count, finding_state_counts}.rs` | 2 | payload-level checks |
| `crates/gents/src/eval/checks/{findings_match, finding_text_excludes}.rs` | 2 | finding-level checks |
| `crates/gents/src/eval/checks/mod.rs` | 2 | module list, registration, `CHECK_REGISTRY_VERSION = "2"` |
| `scripts/evals/convert-expected-to-checks.py`, `scripts/evals/test_convert_expected_to_checks.py` | 3 | one-time conversion and its test |
| `packs/eval_monitor_findings/{manifest.json, pack_config.json, README.md, cases/*.json}` | 3 | the definition pack |
| `packs/eval_monitor/**` (minus `cases/`), `packs/catalog.json` | 3 | the subject pack, copied; catalog registration |
| `crates/gents/src/pack.rs` | 3 | slot-test exemption for definition-only packs |
| `crates/gents/tests/eval_monitor_findings.rs` | 3 | loader test, scripted integration test, live calibration |

---

## PR 1: `eval/40-capture`

Branch `eval/40-capture`, base: the M3 base. Worktree `/Users/iron-arch-mage/Repos/Source/gents-eval-40-capture`.

### Task 1: Consume the amendment: origin breaker threshold and completion digest

**Files:**
- Modify: `crates/gents/src/eval/runner/freeze.rs` (the `RunOrigin {` literal in `freeze`; `FrozenRun` construction in `freeze`; the sidecar read and `FrozenRun` construction in `thaw`; the `RunOrigin {` literal in test `freeze_refuses_an_invalidated_run`; test `freeze_writes_the_run_materializes_the_pack_and_is_idempotent`)
- Modify: `crates/gents/src/eval/runner/mod.rs` (`completion`, doc of `write_evidence_sidecar`, tests)
- Modify: `crates/gents/src/eval/runner/plan.rs` (test fns `origin` and `completion`), `crates/gents/src/eval/runner/grade.rs` (test fn `case`, the `EvalStage {` literal)
- Modify: `crates/gents/tests/eval_runner_canary.rs` (the comment block above the `evidence_records` loop)

**Interfaces:**
- Consumes: `RunOrigin.breaker_threshold: u32`, `TrialCompletion.evidence_digest: Option<String>`, `EvalStage.capture: Vec<EvalCapture>` (given); `fn completion(case: &EvalCase, evidence: &TrialEvidence) -> TrialCompletion` (`runner/mod.rs`); `pub async fn resume(access: &ConfigAccess, owner: &str, run_id: &str, runs_dir: &Path, executor: &dyn TrialExecutor, registry: &CheckRegistry, cancel: CancellationToken, options: &RunOptions) -> Result<RunOutcome>`; `pub fn provider_down(error: &anyhow::Error) -> Option<&ProviderDown>` with `ProviderDown.consecutive_not_evidence: u32`; `gents::eval::load_run(access, owner, run_id) -> Result<Option<RunRecord>>`.
- Produces: `FrozenRun.breaker_threshold` always equals `record.origin.breaker_threshold`; `thaw` accepts a missing `run.json` (captures default to empty); `completion(..).evidence_digest == Some(evidence.evidence_digest)` iff `!evidence.stages.is_empty()`.

- [ ] **Step 1: Make the base compile against the amendment.** If the orchestrator's rebase already added these fields, skip the ones present. In `plan.rs` test fn `origin`, add `breaker_threshold: 5,` after `purpose: …`; in `plan.rs` test fn `completion`, add `evidence_digest: None,` after `anchor: …`. In `grade.rs` test fn `case`, inside the `EvalStage {` literal add `capture: Vec::new(),` after `checks: …`. In `freeze.rs` test `freeze_refuses_an_invalidated_run`, add `breaker_threshold: 5,` to the `RunOrigin {` literal. In `freeze.rs` `freeze`, add to the `RunOrigin {` literal, after `purpose: request.purpose.clone(),`:

```rust
        breaker_threshold: request.breaker_threshold,
```

In `runner/mod.rs` `completion`, add to the `TrialCompletion {` literal, after `anchor: evidence.anchor.clone(),`:

```rust
        evidence_digest: None,
```

- [ ] **Step 2: Write the failing tests** in `runner/mod.rs` `mod tests`. Change the import `use crate::eval::{invalidate_run, load_trials, load_verdicts, TrialRecord, VerdictRecord};` to `use crate::eval::{invalidate_run, load_run, load_trials, load_verdicts, TrialRecord, VerdictRecord};` and add:

```rust
    /// The breaker threshold is a fact of the frozen origin, so a run whose
    /// `run.json` is gone still stops where it was told to.
    #[tokio::test]
    async fn resume_reads_the_breaker_threshold_from_the_origin_without_run_json() {
        let (launching, pack) = launching("captured_rows_count").await;
        let registry = CheckRegistry::builtin();
        let mut request = request(&launching, &pack, "run-1");
        request.cells.truncate(1);
        request.trials_per_case = 1;
        request.max_infra_retries = 3;
        request.breaker_threshold = 2;
        let executor =
            ScriptedExecutor::new().with_default(ScriptedExecutor::not_evidence("did:key:x"));

        let error = run(
            &launching.access,
            &request,
            &executor,
            &registry,
            CancellationToken::new(),
            &options(),
        )
        .await
        .unwrap_err();
        assert!(provider_down(&error).is_some(), "{error:#}");
        let record = load_run(&launching.access, OWNER, "run-1")
            .await
            .unwrap()
            .expect("the frozen run");
        assert_eq!(record.origin.breaker_threshold, 2);

        std::fs::remove_file(launching.runs_dir().join("run-1").join("run.json")).unwrap();
        let error = resume(
            &launching.access,
            OWNER,
            "run-1",
            &launching.runs_dir(),
            &executor,
            &registry,
            CancellationToken::new(),
            &options(),
        )
        .await
        .unwrap_err();
        let down =
            provider_down(&error).unwrap_or_else(|| panic!("expected ProviderDown: {error:#}"));
        assert_eq!(down.consecutive_not_evidence, 2);
    }

    /// The digest is a record of what was observed; a trial that observed
    /// nothing has nothing to digest.
    #[test]
    fn a_completion_carries_the_evidence_digest_only_when_a_stage_ran() {
        let case: EvalCase =
            serde_json::from_value(case("case-a", "validation", "captured_rows_count")).unwrap();
        let ran = passed();
        assert_eq!(
            completion(&case, &ran).evidence_digest,
            Some(ran.evidence_digest.clone())
        );
        let nothing = ScriptedExecutor::not_evidence("did:key:x");
        assert_eq!(completion(&case, &nothing).evidence_digest, None);
    }
```

In `a_two_cell_run_completes_every_slot_and_writes_verdicts_before_completion`, replace the comment `// \`TrialCompletion\` has no field for the evidence digest, so the loop` / `// records it beside each trial's retained home.` with `// The digest is on the completion and, for one release, beside the home.` and add inside the `for trial in &trials` loop, after `assert_eq!(recorded["anchor"]["requests"], 1, "{recorded}");`:

```rust
            assert_eq!(
                trial
                    .completion
                    .as_ref()
                    .and_then(|completion| completion.evidence_digest.as_deref()),
                Some(passed().evidence_digest.as_str()),
                "the completion carries the digest evidence.json records"
            );
```

In `freeze.rs` test `freeze_writes_the_run_materializes_the_pack_and_is_idempotent`, after `assert_eq!(sidecar(&frozen.run_dir)["breaker_threshold"], 5);` add:

```rust
        assert_eq!(frozen.record.origin.breaker_threshold, 5);
        assert_eq!(frozen.breaker_threshold, 5);
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `LOG=$TMPDIR/m3.log; CARGO_BUILD_JOBS=4 cargo test -p gents --lib eval::runner > "$LOG" 2>&1; grep -E "^test .*(FAILED|ok)$|test result|panicked" "$LOG" | grep -E "FAILED|result|panicked"`
Expected: FAIL: `resume_reads_the_breaker_threshold_from_the_origin_without_run_json` panics with "expected ProviderDown: run run-1 has no …run.json"; `a_completion_carries_the_evidence_digest_only_when_a_stage_ran` and the two-cell test fail on `None` digests.

- [ ] **Step 4: Implement.** In `runner/mod.rs` `completion`, replace `evidence_digest: None,` with:

```rust
        // Nothing observed, nothing to digest: a trial that never ran carries
        // no evidence digest rather than the digest of an empty record.
        evidence_digest: (!evidence.stages.is_empty()).then(|| evidence.evidence_digest.clone()),
```

Replace the doc comment of `write_evidence_sidecar` with:

```rust
/// `<run dir>/trials/<trial_id>/evidence.json`: the trial's evidence digest
/// and the anchor it covers, beside the retained home.
///
/// `TrialCompletion.evidence_digest` is the record; this file is kept for one
/// release so tooling that reads it keeps working, then goes. Nothing the loop
/// decides reads it back, so a write that fails is reported and the trial
/// still counts. A spec with no home directory (the scripted executor's) has
/// nowhere to put it.
```

In `freeze.rs` `freeze`, replace the returned `Ok(FrozenRun { … })` so the threshold comes from the row:

```rust
    let breaker_threshold = record.origin.breaker_threshold;
    Ok(FrozenRun {
        record,
        run_dir,
        definition,
        cells: validated.into_iter().map(|(cell, _)| cell).collect(),
        captures: request.captures.clone(),
        breaker_threshold,
    })
```

In `thaw`, replace

```rust
    let sidecar = read_sidecar(&run_dir)?.ok_or_else(|| {
        refused(format!(
            "run {run_id} has no {}",
            sidecar_path(&run_dir).display()
        ))
    })?;
```

with

```rust
    // The origin is authoritative for the breaker. `run.json` is kept for one
    // release and read only for the request-level captures it still carries,
    // so a run without one resumes with none.
    let sidecar = read_sidecar(&run_dir)?;
    if let Some(found) = sidecar
        .as_ref()
        .filter(|found| found.breaker_threshold != record.origin.breaker_threshold)
    {
        tracing::warn!(
            run_id,
            run_json = found.breaker_threshold,
            origin = record.origin.breaker_threshold,
            "eval run.json disagrees with the frozen origin; the origin's breaker threshold wins"
        );
    }
```

and replace the returned `Ok(FrozenRun { … })` at the end of `thaw` with:

```rust
    let breaker_threshold = record.origin.breaker_threshold;
    Ok(FrozenRun {
        record,
        run_dir,
        definition,
        cells,
        captures: sidecar.map(|found| found.captures).unwrap_or_default(),
        breaker_threshold,
    })
```

Update the `RunSidecar` doc comment to: `/// Written beside the run for one release: the breaker threshold (now also on /// the origin, which wins) and the request-level captures, the fallback for a /// stage that declares none.` and the `FrozenRun.captures` field doc to `/// The request-level captures: what a stage reads when it declares none.`

In `crates/gents/tests/eval_runner_canary.rs`, replace the comment block that begins `// \`TrialCompletion\` carries no evidence digest, so the loop records each` (six lines) with:

```rust
    // Each trial's evidence digest is on its completion and, for one release,
    // in evidence.json beside its home. The digest covers the trial's request
    // ids and its messages' timestamps, both new in every run, so two runs of
    // one case digest differently by construction. What has to agree across
    // runs is the anchor the digest was taken over; what the digest itself pins
    // is that it follows the evidence, so two trials of one run differ in it.
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `LOG=$TMPDIR/m3.log; CARGO_BUILD_JOBS=4 cargo test -p gents --lib eval > "$LOG" 2>&1; grep -E "test result|FAILED|panicked" "$LOG"`
Expected: PASS, `test result: ok` with 0 failed.

Run: `LOG=$TMPDIR/m3.log; CARGO_BUILD_JOBS=4 cargo test -p gents --test eval_runner_canary > "$LOG" 2>&1; grep -E "test result|FAILED|panicked" "$LOG"`
Expected: PASS, 3 passed, 1 ignored.

- [ ] **Step 6: Commit**

```bash
cargo fmt --all --check
git add crates/gents/src/eval/runner crates/gents/tests/eval_runner_canary.rs
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" commit -m "feat(eval): breaker threshold from the origin; evidence digest on the completion" -m "$TRAILER"
```

### Task 2: Stage captures reach the trial

**Files:**
- Modify: `crates/gents/src/eval/runner/executor.rs` (`TrialSpec`, `TrialSpec::empty_for_tests`, `StageSpec`, new `From` impl)
- Modify: `crates/gents/src/eval/runner/mod.rs` (`trial_spec`, new `stage_specs`, tests)
- Modify: `crates/gents/src/eval/runner/embedded/executor.rs` (`run_stage`; test `a_stage_that_could_not_be_submitted_still_runs_its_captures`)

**Interfaces:**
- Consumes: `crate::document_config::EvalCapture` (P1), `EvalStage.capture`; `enum Capture { Documents { name, collection, filter: Value, fields: Vec<String> }, File { name, glob } }` (`executor.rs`); `FrozenRun.captures: Vec<Capture>`.
- Produces:
  ```rust
  pub struct StageSpec { pub stage_id: String, pub prompt: String, pub deadline_secs: u64, pub captures: Vec<Capture> }
  impl From<&EvalCapture> for Capture
  fn stage_specs(case: &EvalCase, fallback: &[Capture]) -> Vec<StageSpec>   // runner/mod.rs, private
  ```
  `TrialSpec` no longer has `captures`. The embedded executor reads `stage.captures` after each stage.

- [ ] **Step 1: Write the failing test** in `runner/mod.rs` `mod tests`:

```rust
    /// A stage reads what it declares; a stage that declares nothing reads what
    /// the run requested, which is how an M2 definition keeps its captures.
    #[test]
    fn a_stage_captures_what_it_declares_and_otherwise_what_the_run_requested() {
        let case: EvalCase = serde_json::from_value(json!({
            "case_id": "k",
            "split": "train",
            "stages": [
                {
                    "stage_id": "declared",
                    "prompt": "p",
                    "deadline_secs": 60,
                    "capture": [
                        {
                            "kind": "documents",
                            "name": "items",
                            "collection": "MailboxItem",
                            "filter": {"requester_did": {"_eq": "$trial"}},
                            "fields": ["title", "status"]
                        },
                        {"kind": "file", "name": "notes", "glob": "**/*.txt"}
                    ]
                },
                {"stage_id": "bare", "prompt": "p", "deadline_secs": 60}
            ]
        }))
        .unwrap();
        let fallback = vec![Capture::Documents {
            name: "rows".into(),
            collection: "Row".into(),
            filter: json!({}),
            fields: Vec::new(),
        }];

        let stages = stage_specs(&case, &fallback);

        assert_eq!(
            (stages[0].stage_id.as_str(), stages[0].deadline_secs),
            ("declared", 60)
        );
        assert_eq!(
            stages[0].captures,
            vec![
                Capture::Documents {
                    name: "items".into(),
                    collection: "MailboxItem".into(),
                    filter: json!({"requester_did": {"_eq": "$trial"}}),
                    fields: vec!["title".into(), "status".into()],
                },
                Capture::File {
                    name: "notes".into(),
                    glob: "**/*.txt".into(),
                },
            ]
        );
        assert_eq!(stages[1].captures, fallback);
    }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `LOG=$TMPDIR/m3.log; CARGO_BUILD_JOBS=4 cargo test -p gents --lib eval::runner::tests::a_stage_captures > "$LOG" 2>&1; grep -E "error\[|test result|FAILED" "$LOG"`
Expected: FAIL to compile: `cannot find function \`stage_specs\``.

- [ ] **Step 3: Implement.** In `executor.rs`, add `use crate::document_config::EvalCapture;` (rustfmt position among the `crate::` imports). Remove the field `pub captures: Vec<Capture>,` from `TrialSpec` and `captures: Vec::new(),` from `empty_for_tests`. Replace `StageSpec` with:

```rust
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StageSpec {
    pub stage_id: String,
    pub prompt: String,
    pub deadline_secs: u64,
    /// What to read out of the home when this stage ends: the stage's own
    /// captures, or the run's request-level list when the stage declares none.
    pub captures: Vec<Capture>,
}
```

and add after `enum Capture`:

```rust
/// A definition's capture as the runner reads it. The two types are one shape
/// on two sides of the freeze: the definition's is authored and validated,
/// the runner's is what a trial executes.
impl From<&EvalCapture> for Capture {
    fn from(capture: &EvalCapture) -> Self {
        match capture {
            EvalCapture::Documents {
                name,
                collection,
                filter,
                fields,
            } => Self::Documents {
                name: name.clone(),
                collection: collection.clone(),
                filter: filter.clone(),
                fields: fields.clone(),
            },
            EvalCapture::File { name, glob } => Self::File {
                name: name.clone(),
                glob: glob.clone(),
            },
        }
    }
}
```

In `runner/mod.rs` `trial_spec`, replace the `stages: case.stages.iter().map(|stage| StageSpec { … }).collect(),` expression and the `captures: frozen.captures.clone(),` line with the single field:

```rust
        stages: stage_specs(case, &frozen.captures),
```

and add after `trial_spec`:

```rust
/// The stages a trial submits, each with the captures read when it ends: the
/// stage's own `capture` list, or the run's request-level list when the stage
/// declares none.
fn stage_specs(case: &EvalCase, fallback: &[Capture]) -> Vec<StageSpec> {
    case.stages
        .iter()
        .map(|stage| StageSpec {
            stage_id: stage.stage_id.clone(),
            prompt: stage.prompt.clone(),
            deadline_secs: stage.deadline_secs,
            captures: if stage.capture.is_empty() {
                fallback.to_vec()
            } else {
                stage.capture.iter().map(Capture::from).collect()
            },
        })
        .collect()
}
```

In `embedded/executor.rs` `run_stage`, change the `run_captures(…)` call's last argument from `&spec.captures,` to `&stage.captures,`. In test `a_stage_that_could_not_be_submitted_still_runs_its_captures`, change `let mut spec = TrialSpec::empty_for_tests("t1");` to `let spec = TrialSpec::empty_for_tests("t1");`, delete the `spec.captures = vec![ … ];` statement, and build the stage with those two captures:

```rust
        let stage = StageSpec {
            stage_id: "only".to_string(),
            prompt: "hello".to_string(),
            deadline_secs: 1,
            captures: vec![
                Capture::Documents {
                    name: "requests".to_string(),
                    collection: "AgentRequest".to_string(),
                    filter: json!({"request_id": {"_eq": "seeded"}}),
                    fields: vec!["behavior_id".to_string()],
                },
                // No such collection in this home, so the read fails rather
                // than returning nothing.
                Capture::Documents {
                    name: "unreadable".to_string(),
                    collection: "NoSuchCollection".to_string(),
                    filter: json!({}),
                    fields: Vec::new(),
                },
            ],
        };
```

Then `grep -rn "captures" crates/gents/src/eval/runner` and confirm no remaining `spec.captures` or `TrialSpec { … captures` reference.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `LOG=$TMPDIR/m3.log; CARGO_BUILD_JOBS=4 cargo test -p gents --lib eval::runner > "$LOG" 2>&1; grep -E "test result|FAILED|panicked" "$LOG"`
Expected: PASS.

Run: `LOG=$TMPDIR/m3.log; CARGO_BUILD_JOBS=4 cargo test -p gents --test eval_runner_canary > "$LOG" 2>&1; grep -E "test result|FAILED|panicked" "$LOG"`
Expected: PASS (the canary's stages declare no capture, so they read `request.captures`).

- [ ] **Step 5: Commit**

```bash
cargo fmt --all --check
git add crates/gents/src/eval/runner
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" commit -m "feat(eval): each stage reads its own captures, the run's list as fallback" -m "$TRAILER"
```

### Task 3: Inline fixture files reach the trial workspace (requires P2)

**Files:**
- Modify: `crates/gents/src/eval/runner/mod.rs` (`fixtures`; test `fixtures_merge_the_case_after_the_definition_and_read_assets_from_the_pack`)

**Interfaces:**
- Consumes: `EvalFixtures.files: Vec<EvalFixtureFile>`, `EvalFixtureFile { path: String, contents: String }` (P2); `FixtureFile { path: String, contents: Vec<u8> }`; the embedded executor's `install_fixtures`, which writes every `TrialFixtures.files` entry through `workspace_path` (refuses non-`Normal` components).
- Produces: `fixtures(..)` appends each declared inline file, after that declaration's assets, definition first then case.

- [ ] **Step 1: Write the failing test.** In the test, add `use crate::document_config::EvalFixtureFile;` beside `use crate::document_config::EvalFixtureDocument;`, add `files: Vec::new(),` to the `shared` literal, and give `per_case` a file:

```rust
        let per_case = EvalFixtures {
            assets: Vec::new(),
            documents: vec![EvalFixtureDocument {
                collection: "A".into(),
                document: json!({"id": 1}),
            }],
            files: vec![EvalFixtureFile {
                path: "inventory/edge-07.json".into(),
                contents: "{\"disk\": \"84%\"}\n".into(),
            }],
            schemas: vec!["type B {}".into()],
        };
```

and replace the `assert_eq!(merged.files, [FixtureFile { path: "seed.json".into(), contents: b"{}".to_vec() }]);` assertion with:

```rust
        assert_eq!(
            merged.files,
            [
                FixtureFile {
                    path: "seed.json".into(),
                    contents: b"{}".to_vec(),
                },
                FixtureFile {
                    path: "inventory/edge-07.json".into(),
                    contents: b"{\"disk\": \"84%\"}\n".to_vec(),
                },
            ]
        );
```

- [ ] **Step 2: Run it to verify it fails**

Run: `LOG=$TMPDIR/m3.log; CARGO_BUILD_JOBS=4 cargo test -p gents --lib eval::runner::tests::fixtures_merge > "$LOG" 2>&1; grep -E "test result|FAILED|panicked" "$LOG"`
Expected: FAIL: `merged.files` holds only `seed.json`.

- [ ] **Step 3: Implement.** In `fixtures`, after the `for asset in &declared.assets { … }` loop and inside the `for declared in …` loop, add:

```rust
        // Inline files are authored with the case, so they never need the
        // subject pack to carry test data.
        fixtures
            .files
            .extend(declared.files.iter().map(|file| FixtureFile {
                path: file.path.clone(),
                contents: file.contents.clone().into_bytes(),
            }));
```

- [ ] **Step 4: Run to verify it passes**

Run: `LOG=$TMPDIR/m3.log; CARGO_BUILD_JOBS=4 cargo test -p gents --lib eval::runner > "$LOG" 2>&1; grep -E "test result|FAILED|panicked" "$LOG"`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all --check
git add crates/gents/src/eval/runner/mod.rs
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" commit -m "feat(eval): inline case fixture files reach the trial workspace" -m "$TRAILER"
```

### Task 4: The loader's case-sidecar rule

**Files:**
- Modify: `crates/gents/src/pack/loader.rs` (`decode_pack_config`; new `hydrate_eval_cases`)
- Test: `crates/gents/src/pack/loader/tests.rs`

**Interfaces:**
- Consumes: `pub fn decode_pack_config(value: Value, options: Option<&PackInstallOptions>, environment: &dyn Fn(&str) -> Option<String>, read_sidecar: &dyn Fn(crate::Collection, &str, &str) -> Result<String>) -> Result<PackConfig>`; `pub fn load_pack_config(manifest: &PackManifest, options: &PackInstallOptions, read_asset: &dyn Fn(&str) -> Result<Vec<u8>>, environment: &dyn Fn(&str) -> Option<String>) -> Result<PackConfig>`, whose sidecar closure calls `hydrate_sidecar` (path relative to the config file; refuses non-canonical paths with `"unsafe/non-canonical pack sidecar path: …"` and undeclared ones with `"undeclared pack sidecar: <path>"`); `crate::document_config::EvalCase`.
- Produces: `fn hydrate_eval_cases(root: &mut serde_json::Map<String, Value>, read_sidecar: &dyn Fn(crate::Collection, &str, &str) -> Result<String>) -> Result<()>`, called after interpolation and owner binding, before the canonical decode. A string entry of `eval_definitions[i].cases` must start with `./`, is read with `Collection::EvalDefinition` and the definition id, and is parsed as one `EvalCase`; object entries are left as they are.

- [ ] **Step 1: Write the failing tests** at the end of `pack/loader/tests.rs`:

```rust
fn eval_manifest(assets: &[&str]) -> PackManifest {
    serde_json::from_value(json!({
        "manifest_version": 1, "name": "example", "version": "1", "description": "Example",
        "kind": "documents", "authors": ["Example"], "assets": assets,
        "config": "config/bundle.json",
    }))
    .unwrap()
}

fn eval_config(cases: Value) -> Value {
    json!({"agent_principal": {}, "eval_definitions": [{
        "definition_id": "defn",
        "comparability_version": 1,
        "subject": {"kind": "behavior"},
        "cases": cases,
    }]})
}

/// A sidecar case whose prompt would fail interpolation if it were
/// interpolated: `NOT_INTERPOLATED` is unset.
const SIDECAR_CASE: &str = r#"{"case_id":"one","split":"train","stages":[{"stage_id":"s","prompt":"literal ${NOT_INTERPOLATED}","deadline_secs":60,"checks":[{"check":"finding_count","params":{"count":1},"tier":"acceptance"}]}]}"#;

fn load_eval(manifest: &PackManifest, config: Value) -> Result<PackConfig> {
    load_pack_config(
        manifest,
        &PackInstallOptions {
            agent_did: "did:key:owner".into(),
        },
        &|path| match path {
            "config/bundle.json" => Ok(serde_json::to_vec(&config)?),
            "config/cases/one.json" => Ok(SIDECAR_CASE.as_bytes().to_vec()),
            "config/cases/bad.json" => {
                Ok(br#"{"case_id":"bad","split":"train","stages":[],"surprise":1}"#.to_vec())
            }
            _ => anyhow::bail!("unexpected asset {path}"),
        },
        &|_| None,
    )
}

#[test]
fn eval_definition_case_sidecars_hydrate_as_literal_cases_beside_inline_ones() {
    let manifest = eval_manifest(&["README.md", "config/bundle.json", "config/cases/one.json"]);
    let inline = json!({"case_id": "two", "split": "validation", "stages": [{
        "stage_id": "s", "prompt": "p", "deadline_secs": 60,
        "checks": [{"check": "finding_count", "params": {"count": 0}, "tier": "acceptance"}],
    }]});
    let config = load_eval(&manifest, eval_config(json!(["./cases/one.json", inline]))).unwrap();
    let definition = &config.eval_definitions[0];
    assert_eq!(definition.agent_did, "did:key:owner");
    assert_eq!(
        definition
            .cases
            .iter()
            .map(|case| case.case_id.as_str())
            .collect::<Vec<_>>(),
        ["one", "two"]
    );
    assert_eq!(
        definition.cases[0].stages[0].prompt,
        "literal ${NOT_INTERPOLATED}",
        "sidecar contents are never interpolated"
    );
    definition.validate().unwrap();
}

#[test]
fn eval_definition_case_sidecars_refuse_undeclared_escaping_bare_and_malformed_paths() {
    let undeclared = load_eval(
        &eval_manifest(&["README.md", "config/bundle.json"]),
        eval_config(json!(["./cases/one.json"])),
    )
    .unwrap_err();
    assert!(
        format!("{undeclared:#}").contains("undeclared pack sidecar: config/cases/one.json"),
        "{undeclared:#}"
    );

    let manifest = eval_manifest(&[
        "README.md",
        "config/bundle.json",
        "config/cases/one.json",
        "config/cases/bad.json",
    ]);
    for path in ["./../escape.json", "./cases/../../escape.json", "cases/one.json"] {
        let error = load_eval(&manifest, eval_config(json!([path]))).unwrap_err();
        assert!(
            !format!("{error:#}").contains("unexpected asset"),
            "the reader must not receive {path}: {error:#}"
        );
    }
    let malformed = load_eval(&manifest, eval_config(json!(["./cases/bad.json"]))).unwrap_err();
    assert!(
        format!("{malformed:#}").contains("./cases/bad.json is not one EvalCase"),
        "{malformed:#}"
    );
}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `LOG=$TMPDIR/m3.log; CARGO_BUILD_JOBS=4 cargo test -p gents --lib pack::loader > "$LOG" 2>&1; grep -E "test result|FAILED|panicked" "$LOG"`
Expected: FAIL: both new tests fail with `decoding canonical pack configuration` (a string where an `EvalCase` is expected).

- [ ] **Step 3: Implement.** In `loader.rs`, change `use crate::document_config::PackConfig;` to `use crate::document_config::{EvalCase, PackConfig};`. In `decode_pack_config`, immediately before `let mut config: PackConfig =`, add:

```rust
    hydrate_eval_cases(root, read_sidecar)?;
```

and add after `bind_owner`:

```rust
/// A case of an eval definition may be authored as a `./` sidecar path: the
/// file holds one `EvalCase`, read through the caller's sidecar boundary after
/// interpolation, so its prompts stay literal like every other sidecar. The
/// installed definition carries the cases inline; the paths never reach it.
fn hydrate_eval_cases(
    root: &mut serde_json::Map<String, Value>,
    read_sidecar: &dyn Fn(crate::Collection, &str, &str) -> Result<String>,
) -> Result<()> {
    let Some(definitions) = root
        .get_mut("eval_definitions")
        .and_then(Value::as_array_mut)
    else {
        return Ok(());
    };
    for definition in definitions {
        let definition_id = definition
            .get("definition_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let Some(cases) = definition.get_mut("cases").and_then(Value::as_array_mut) else {
            continue;
        };
        for case in cases {
            let Some(reference) = case.as_str().map(str::to_owned) else {
                continue;
            };
            anyhow::ensure!(
                reference.starts_with("./"),
                "eval definition {definition_id:?} case {reference:?} must be an inline case or a ./ sidecar path"
            );
            let text = read_sidecar(crate::Collection::EvalDefinition, &definition_id, &reference)?;
            let parsed: EvalCase = serde_json::from_str(&text).with_context(|| {
                format!("eval definition {definition_id:?} case sidecar {reference} is not one EvalCase")
            })?;
            *case = serde_json::to_value(parsed)?;
        }
    }
    Ok(())
}
```

- [ ] **Step 4: Run to verify they pass**

Run: `LOG=$TMPDIR/m3.log; CARGO_BUILD_JOBS=4 cargo test -p gents --lib pack > "$LOG" 2>&1; grep -E "test result|FAILED|panicked" "$LOG"`
Expected: PASS (all `pack` tests, including the existing loader tests).

- [ ] **Step 5: Commit, then gate the branch**

```bash
cargo fmt --all --check
git add crates/gents/src/pack/loader.rs crates/gents/src/pack/loader/tests.rs
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" commit -m "feat(pack): eval definition cases may be ./ sidecars, parsed as one EvalCase" -m "$TRAILER"
```

Branch gate: `LOG=$TMPDIR/m3.log; CARGO_BUILD_JOBS=4 cargo check --workspace --all-targets > "$LOG" 2>&1; grep -E "^(error|warning: unused)" "$LOG"` (expected: nothing), then `CARGO_BUILD_JOBS=4 cargo test -p gents > "$LOG" 2>&1; grep -E "test result|FAILED" "$LOG"` (expected: only the two known libp2p failures).

---

## PR 2: `eval/41-checks`

Branch `eval/41-checks` on `eval/40-capture`. Worktree `/Users/iron-arch-mage/Repos/Source/gents-eval-41-checks`. Between Tasks 5 and 8 some `mailbox.rs` helpers are unused in non-test builds and warn; the warnings are gone at Task 8.

### Task 5: Shared predicates (`checks/mailbox.rs`)

**Files:**
- Create: `crates/gents/src/eval/checks/mailbox.rs`
- Modify: `crates/gents/src/eval/checks/mod.rs` (add `pub(crate) mod mailbox;` after `pub mod captured_rows_count;`)

**Interfaces:**
- Consumes: `CheckVerdict { kind: OutcomeKind, score_bp: Option<u32>, raw: Value, feedback: Option<String> }`; `StageEvidence.captures: BTreeMap<String, CaptureResult>`; `CaptureResult::{Documents { rows: Vec<Value> }, Files { .. }}`; `ScriptedExecutor::passed_evidence(did: &str, stage_id: &str, capture_name: &str, rows: Vec<Value>) -> TrialEvidence` (tests).
- Produces (all `pub(crate)`):
  ```rust
  const ITEMS: &str = "items";
  enum FindingState { Open, Resolved }                         // Serialize, Deserialize, snake_case
  struct Finding { correlation: String, condition: String, state: FindingState, detail: String } // Serialize
  impl Finding { fn text_tokens(&self) -> Vec<String> }       // tokens of condition + " " + detail
  enum PayloadDefect { Missing, NotString, NotJson, BadShape, WrongVersion, BadState }
  impl PayloadDefect { fn reason_code(self) -> &'static str }
  fn parse_payload(payload: &Value, version: Option<u64>) -> Result<Vec<Finding>, PayloadDefect>
  fn item_rows(stage: &StageEvidence) -> Result<&[Value], CheckVerdict>
  fn field<'a>(row: &'a Value, index: usize, name: &str) -> Result<&'a Value, CheckVerdict>
  fn text_field<'a>(row: &'a Value, index: usize, name: &str) -> Result<&'a str, CheckVerdict>
  fn all_findings(rows: &[Value]) -> Result<Vec<Finding>, CheckVerdict>
  fn parse_params<T: DeserializeOwned>(params: &Value) -> Result<T, CheckVerdict>
  struct NoParams {}
  fn tokens(text: &str) -> Vec<String>
  fn keyword_tokens(keywords: &[String]) -> Result<Vec<Vec<String>>, CheckVerdict>
  fn contains(text: &[String], keyword: &[String]) -> bool
  fn passed(reason_code: &str, detail: String, extra: Value) -> CheckVerdict
  fn failed(reason_code: &str, detail: String, extra: Value) -> CheckVerdict
  fn grader(reason_code: &str, detail: String) -> CheckVerdict
  #[cfg(test)] mod test_support { fn stage(rows: Vec<Value>) -> StageEvidence; fn no_items() -> StageEvidence;
      fn finding(correlation: &str, condition: &str, state: &str, detail: &str) -> Value;
      fn row(summary: &str, findings: Vec<Value>) -> Value;
      fn outcome(verdict: &CheckVerdict) -> (OutcomeKind, Option<u32>, String);
      fn pass(reason: &str) -> (…); fn fail(reason: &str) -> (…); fn grader(reason: &str) -> (…) }
  ```
  Reason codes produced here: `missing_capture`, `missing_field`, `bad_params` (grader); `payload_unreadable` (subject); `PayloadDefect::reason_code` yields `payload_missing`, `payload_not_string`, `payload_not_json`, `payload_bad_shape`, `payload_wrong_version`, `payload_bad_state`.

- [ ] **Step 1: Write the module with its tests first, bodies as `todo!()`.** Create `mailbox.rs` with the full test module below and every function signature from Interfaces with body `todo!()`, so the tests compile and fail.

```rust
#[cfg(test)]
pub(crate) mod test_support {
    use serde_json::{json, Value};

    use crate::eval::checks::CheckVerdict;
    use crate::eval::runner::executor::StageEvidence;
    use crate::eval::runner::scripted::ScriptedExecutor;
    use crate::eval::OutcomeKind;

    /// One completed stage whose `items` capture holds `rows`.
    pub(crate) fn stage(rows: Vec<Value>) -> StageEvidence {
        let mut evidence = ScriptedExecutor::passed_evidence("did:x", "s1", super::ITEMS, rows);
        evidence.stages.remove(0)
    }

    /// One completed stage that captured something, but not `items`.
    pub(crate) fn no_items() -> StageEvidence {
        let mut evidence = ScriptedExecutor::passed_evidence("did:x", "s1", "other", Vec::new());
        evidence.stages.remove(0)
    }

    pub(crate) fn finding(correlation: &str, condition: &str, state: &str, detail: &str) -> Value {
        json!({"correlation": correlation, "condition": condition, "state": state, "detail": detail})
    }

    /// An open mailbox row whose payload is the JSON text of `findings`.
    pub(crate) fn row(summary: &str, findings: Vec<Value>) -> Value {
        json!({
            "_docID": "bae-row",
            "title": "Monitor findings",
            "summary": summary,
            "status": "open",
            "payload": json!({"version": 1, "findings": findings}).to_string(),
        })
    }

    pub(crate) fn outcome(verdict: &CheckVerdict) -> (OutcomeKind, Option<u32>, String) {
        (
            verdict.kind,
            verdict.score_bp,
            verdict.raw["reason_code"].as_str().unwrap_or_default().to_owned(),
        )
    }

    pub(crate) fn pass(reason: &str) -> (OutcomeKind, Option<u32>, String) {
        (OutcomeKind::Passed, Some(10_000), reason.to_owned())
    }

    pub(crate) fn fail(reason: &str) -> (OutcomeKind, Option<u32>, String) {
        (OutcomeKind::ModelAcceptance, Some(0), reason.to_owned())
    }

    pub(crate) fn grader(reason: &str) -> (OutcomeKind, Option<u32>, String) {
        (OutcomeKind::Grader, None, reason.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::test_support::{self, finding, no_items, outcome, row, stage};
    use super::*;

    fn keyword(text: &str) -> Vec<String> {
        tokens(text)
    }

    #[test]
    fn tokens_are_lowercase_alphanumeric_runs() {
        assert_eq!(
            tokens("Disk_usage_HIGH: /var at 88%, api-gateway"),
            ["disk", "usage", "high", "var", "at", "88", "api", "gateway"]
        );
        assert!(tokens(" -%_/ ").is_empty());
    }

    #[test]
    fn a_keyword_matches_a_consecutive_run_of_tokens_never_a_substring() {
        let text = tokens("sync_worker lag at 4419 on api-gateway, disks full");
        assert!(contains(&text, &keyword("sync-worker")), "hyphen and underscore agree");
        assert!(contains(&text, &keyword("api")), "the stem of a hyphenated token");
        assert!(contains(&text, &keyword("4419")));
        assert!(!contains(&text, &keyword("disk")), "disk is not a token of disks");
        assert!(!contains(&text, &keyword("worker-sync")), "order matters");
        assert!(!contains(&text, &[]), "an empty keyword matches nothing");
    }

    #[test]
    fn keywords_without_letters_or_digits_are_bad_params() {
        assert_eq!(
            keyword_tokens(&["disk".into(), "sync-worker".into()]).unwrap(),
            vec![vec!["disk".to_owned()], vec!["sync".to_owned(), "worker".to_owned()]]
        );
        let verdict = keyword_tokens(&["--".into()]).unwrap_err();
        assert_eq!(outcome(&verdict), test_support::grader("bad_params"));
    }

    #[test]
    fn a_payload_parses_into_findings_and_every_defect_has_its_code() {
        let good = json!({"version": 1, "findings": [
            finding("c-1", "disk_usage_high", "open", "88%"),
            finding("c-1", "fan_stalled", "resolved", "spinning again"),
        ]})
        .to_string();
        let found = parse_payload(&json!(good), Some(1)).unwrap();
        assert_eq!(found.len(), 2);
        assert_eq!(found[1].state, FindingState::Resolved);
        assert_eq!(found[0].text_tokens(), ["disk", "usage", "high", "88"]);
        assert!(parse_payload(&json!(good), None).is_ok());

        let cases = [
            (json!(null), PayloadDefect::Missing, "payload_missing"),
            (json!({"version": 1}), PayloadDefect::NotString, "payload_not_string"),
            (json!("{"), PayloadDefect::NotJson, "payload_not_json"),
            (json!("[]"), PayloadDefect::BadShape, "payload_bad_shape"),
            (json!(r#"{"version": 1}"#), PayloadDefect::BadShape, "payload_bad_shape"),
            (json!(r#"{"version": 1, "findings": [{"state": "open"}]}"#), PayloadDefect::BadShape, "payload_bad_shape"),
            (json!(r#"{"version": 2, "findings": []}"#), PayloadDefect::WrongVersion, "payload_wrong_version"),
            (
                json!(r#"{"version": 1, "findings": [{"correlation": "c", "condition": "x", "state": "closed", "detail": "d"}]}"#),
                PayloadDefect::BadState,
                "payload_bad_state",
            ),
        ];
        for (payload, defect, code) in cases {
            assert_eq!(parse_payload(&payload, Some(1)).unwrap_err(), defect, "{payload}");
            assert_eq!(defect.reason_code(), code);
        }
        assert!(
            parse_payload(&json!(r#"{"version": 2, "findings": []}"#), None).is_ok(),
            "without a version the shape alone is checked"
        );
    }

    #[test]
    fn rows_come_from_the_items_capture_or_the_check_is_a_grader_outcome() {
        assert_eq!(item_rows(&stage(vec![json!({})])).unwrap().len(), 1);
        let verdict = item_rows(&no_items()).unwrap_err();
        assert_eq!(outcome(&verdict), test_support::grader("missing_capture"));
    }

    #[test]
    fn a_field_the_capture_did_not_select_is_missing_but_a_null_one_is_present() {
        let row = json!({"summary": null});
        assert_eq!(text_field(&row, 0, "summary").unwrap(), "");
        let verdict = field(&row, 3, "title").unwrap_err();
        assert_eq!(outcome(&verdict), test_support::grader("missing_field"));
        assert!(verdict.raw["detail"].as_str().unwrap_or_default().contains("row 3"));
    }

    #[test]
    fn all_findings_flattens_rows_and_an_unreadable_payload_fails_the_subject() {
        let rows = vec![
            row("a", vec![finding("c-1", "a", "open", "x")]),
            row("b", vec![finding("c-2", "b", "open", "y")]),
        ];
        assert_eq!(all_findings(&rows).unwrap().len(), 2);
        let broken = vec![rows[0].clone(), json!({"payload": "not json"})];
        let verdict = all_findings(&broken).unwrap_err();
        assert_eq!(outcome(&verdict), test_support::fail("payload_unreadable"));
        assert_eq!(verdict.raw["row"], 1);
        assert_eq!(verdict.raw["defect"], "payload_not_json");
        let verdict = all_findings(&[json!({})]).unwrap_err();
        assert_eq!(outcome(&verdict), test_support::grader("missing_field"));
    }

    #[test]
    fn null_params_read_as_an_empty_object_and_bad_params_carry_the_error() {
        let NoParams {} = parse_params(&Value::Null).unwrap();
        let verdict = parse_params::<NoParams>(&json!({"surprise": 1})).unwrap_err();
        assert_eq!(outcome(&verdict), test_support::grader("bad_params"));
        assert!(verdict.raw["detail"].as_str().unwrap_or_default().contains("surprise"));
    }

    #[test]
    fn verdicts_merge_extra_fields_beside_the_reason_code() {
        let verdict = passed("ok", "fine".into(), json!({"rows": 2}));
        assert_eq!(verdict.raw, json!({"reason_code": "ok", "detail": "fine", "rows": 2}));
        assert_eq!(outcome(&failed("no", "bad".into(), Value::Null)), test_support::fail("no"));
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `LOG=$TMPDIR/m3.log; CARGO_BUILD_JOBS=4 cargo test -p gents --lib eval::checks::mailbox > "$LOG" 2>&1; grep -E "test result|FAILED|panicked" "$LOG"`
Expected: FAIL: every test panics with `not yet implemented`.

- [ ] **Step 3: Implement** — the top of `mailbox.rs`, replacing the `todo!()` bodies:

```rust
//! Shared predicates for the checks over the monitor's `items` capture.
//!
//! Every mailbox check reads the `MailboxItem` rows a stage captured under
//! [`ITEMS`]. The rules they share live here once: how a row's `payload`
//! parses into findings, how a keyword matches text, how params and fields
//! are read, and how a verdict is shaped. `packs/eval_monitor/CHECKS.md` is
//! the inventory these rules come from.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::eval::checks::CheckVerdict;
use crate::eval::runner::executor::{CaptureResult, StageEvidence};
use crate::eval::OutcomeKind;

/// The capture every mailbox check reads.
pub(crate) const ITEMS: &str = "items";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FindingState {
    Open,
    Resolved,
}

/// One entry of a row's payload, as the subject wrote it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct Finding {
    pub(crate) correlation: String,
    pub(crate) condition: String,
    pub(crate) state: FindingState,
    pub(crate) detail: String,
}

impl Finding {
    /// The matchable text, `condition + " " + detail`, as tokens.
    pub(crate) fn text_tokens(&self) -> Vec<String> {
        tokens(&format!("{} {}", self.condition, self.detail))
    }
}

/// Why a payload is not the subject's output contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PayloadDefect {
    Missing,
    NotString,
    NotJson,
    BadShape,
    WrongVersion,
    BadState,
}

impl PayloadDefect {
    pub(crate) fn reason_code(self) -> &'static str {
        match self {
            Self::Missing => "payload_missing",
            Self::NotString => "payload_not_string",
            Self::NotJson => "payload_not_json",
            Self::BadShape => "payload_bad_shape",
            Self::WrongVersion => "payload_wrong_version",
            Self::BadState => "payload_bad_state",
        }
    }
}

/// A row's `payload`: JSON text of `{"version", "findings": [{correlation,
/// condition, state, detail}]}`, `state` in `{open, resolved}`. Extra keys are
/// tolerated. `version` is compared only when given.
pub(crate) fn parse_payload(
    payload: &Value,
    version: Option<u64>,
) -> Result<Vec<Finding>, PayloadDefect> {
    let text = match payload {
        Value::Null => return Err(PayloadDefect::Missing),
        Value::String(text) => text,
        _ => return Err(PayloadDefect::NotString),
    };
    let parsed: Value = serde_json::from_str(text).map_err(|_| PayloadDefect::NotJson)?;
    let object = parsed.as_object().ok_or(PayloadDefect::BadShape)?;
    let found = object
        .get("version")
        .and_then(Value::as_u64)
        .ok_or(PayloadDefect::BadShape)?;
    if version.is_some_and(|expected| expected != found) {
        return Err(PayloadDefect::WrongVersion);
    }
    object
        .get("findings")
        .and_then(Value::as_array)
        .ok_or(PayloadDefect::BadShape)?
        .iter()
        .map(finding)
        .collect()
}

fn finding(entry: &Value) -> Result<Finding, PayloadDefect> {
    let text = |key: &str| {
        entry
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or(PayloadDefect::BadShape)
    };
    let state = match text("state")?.as_str() {
        "open" => FindingState::Open,
        "resolved" => FindingState::Resolved,
        _ => return Err(PayloadDefect::BadState),
    };
    Ok(Finding {
        correlation: text("correlation")?,
        condition: text("condition")?,
        state,
        detail: text("detail")?,
    })
}

/// The rows of the `items` capture. A stage without one is a grader outcome:
/// no evidence was read, which says nothing about the subject.
pub(crate) fn item_rows(stage: &StageEvidence) -> Result<&[Value], CheckVerdict> {
    match stage.captures.get(ITEMS) {
        Some(CaptureResult::Documents { rows }) => Ok(rows),
        Some(CaptureResult::Files { .. }) => Err(grader(
            "missing_capture",
            format!("capture {ITEMS} holds files, not documents"),
        )),
        None => Err(grader(
            "missing_capture",
            format!("the stage produced no capture named {ITEMS}"),
        )),
    }
}

/// A field of row `index`. Absent means the capture did not select it — the
/// definition's fault — while `null` is present: the subject left it unset.
pub(crate) fn field<'a>(row: &'a Value, index: usize, name: &str) -> Result<&'a Value, CheckVerdict> {
    row.get(name).ok_or_else(|| {
        grader(
            "missing_field",
            format!("row {index} has no {name} field; the capture must select it"),
        )
    })
}

/// A text field of row `index`; `null` and non-strings read as empty text.
pub(crate) fn text_field<'a>(row: &'a Value, index: usize, name: &str) -> Result<&'a str, CheckVerdict> {
    Ok(field(row, index, name)?.as_str().unwrap_or_default())
}

/// Every finding of every row, in row order. A payload that is not the output
/// contract fails the claim of the check that asked (CHECKS.md, "Payload
/// parse"); a row without a `payload` field is the capture's fault.
pub(crate) fn all_findings(rows: &[Value]) -> Result<Vec<Finding>, CheckVerdict> {
    let mut all = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        match parse_payload(field(row, index, "payload")?, None) {
            Ok(found) => all.extend(found),
            Err(defect) => {
                return Err(failed(
                    "payload_unreadable",
                    format!("row {index}: {}", defect.reason_code()),
                    json!({"rows": rows.len(), "row": index, "defect": defect.reason_code()}),
                ))
            }
        }
    }
    Ok(all)
}

/// A check ref's params; an absent `params` reads as `{}`.
pub(crate) fn parse_params<T: DeserializeOwned>(params: &Value) -> Result<T, CheckVerdict> {
    let value = if params.is_null() {
        Value::Object(Map::new())
    } else {
        params.clone()
    };
    serde_json::from_value(value).map_err(|error| grader("bad_params", error.to_string()))
}

/// The params of a check that takes none.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NoParams {}

/// Lowercase runs of alphanumeric characters; everything else separates.
pub(crate) fn tokens(text: &str) -> Vec<String> {
    text.split(|character: char| !character.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// Each keyword's tokens. A keyword with none could match nothing, or
/// everything, so it is a mistake in the case.
pub(crate) fn keyword_tokens(keywords: &[String]) -> Result<Vec<Vec<String>>, CheckVerdict> {
    keywords
        .iter()
        .map(|keyword| {
            let found = tokens(keyword);
            if found.is_empty() {
                Err(grader(
                    "bad_params",
                    format!("keyword {keyword:?} has no letters or digits"),
                ))
            } else {
                Ok(found)
            }
        })
        .collect()
}

/// Whether `keyword`'s tokens occur consecutively in `text`'s tokens.
pub(crate) fn contains(text: &[String], keyword: &[String]) -> bool {
    !keyword.is_empty() && text.windows(keyword.len()).any(|window| window == keyword)
}

pub(crate) fn passed(reason_code: &str, detail: String, extra: Value) -> CheckVerdict {
    CheckVerdict {
        kind: OutcomeKind::Passed,
        score_bp: Some(10_000),
        raw: raw(reason_code, detail, extra),
        feedback: None,
    }
}

/// The subject did the wrong thing.
pub(crate) fn failed(reason_code: &str, detail: String, extra: Value) -> CheckVerdict {
    CheckVerdict {
        kind: OutcomeKind::ModelAcceptance,
        score_bp: Some(0),
        raw: raw(reason_code, detail, extra),
        feedback: None,
    }
}

/// The check could not reach a verdict: no evidence about the subject.
pub(crate) fn grader(reason_code: &str, detail: String) -> CheckVerdict {
    CheckVerdict {
        kind: OutcomeKind::Grader,
        score_bp: None,
        raw: raw(reason_code, detail, Value::Null),
        feedback: None,
    }
}

fn raw(reason_code: &str, detail: String, extra: Value) -> Value {
    let mut object = Map::new();
    object.insert("reason_code".into(), reason_code.into());
    object.insert("detail".into(), detail.into());
    if let Value::Object(extra) = extra {
        object.extend(extra);
    }
    Value::Object(object)
}
```

- [ ] **Step 4: Run to verify they pass**

Run: `LOG=$TMPDIR/m3.log; CARGO_BUILD_JOBS=4 cargo test -p gents --lib eval::checks::mailbox > "$LOG" 2>&1; grep -E "test result|FAILED|panicked" "$LOG"`
Expected: PASS, 9 tests.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all --check
git add crates/gents/src/eval/checks/mailbox.rs crates/gents/src/eval/checks/mod.rs
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" commit -m "feat(eval): shared mailbox check predicates: payload parse, token keywords, verdicts" -m "$TRAILER"
```

### Task 6: Row checks: `mailbox_item_count`, `mailbox_open_row`, `mailbox_text_excludes`

**Files:**
- Create: `crates/gents/src/eval/checks/mailbox_item_count.rs`, `mailbox_open_row.rs`, `mailbox_text_excludes.rs`
- Modify: `crates/gents/src/eval/checks/mod.rs` (add `pub mod mailbox_item_count;`, `pub mod mailbox_open_row;`, `pub mod mailbox_text_excludes;` in rustfmt order; do not register yet)

**Interfaces:**
- Consumes: `trait Check { fn name(&self) -> &'static str; fn version(&self) -> &'static str; fn evaluate(&self, params: &Value, stage: &StageEvidence) -> CheckVerdict; }`; Task 5's `mailbox` items.
- Produces: `pub struct MailboxItemCount;` (`"mailbox_item_count"`, params `{count: u64}`, codes `count_matches` / `count_differs`); `pub struct MailboxOpenRow;` (`"mailbox_open_row"`, params `{}`, codes `one_open_row` / `no_rows` / `several_rows` / `not_open`); `pub struct MailboxTextExcludes;` (`"mailbox_text_excludes"`, params `{keywords: [String]}`, codes `absent` / `keyword_present`). All also `bad_params`, `missing_capture`; open-row and text-excludes also `missing_field`. All `version() == "1"`, `raw` carries `rows`.

- [ ] **Step 1: Write the three files, each with `evaluate` as `todo!()` and the tests below.**

`mailbox_item_count.rs` tests:

```rust
#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::eval::checks::mailbox::test_support::{fail, grader, no_items, outcome, pass, row, stage};

    #[test]
    fn the_row_count_decides() {
        let one = stage(vec![row("s", vec![])]);
        assert_eq!(outcome(&MailboxItemCount.evaluate(&json!({"count": 1}), &one)), pass("count_matches"));
        assert_eq!(outcome(&MailboxItemCount.evaluate(&json!({"count": 0}), &stage(vec![]))), pass("count_matches"));
        let verdict = MailboxItemCount.evaluate(&json!({"count": 0}), &one);
        assert_eq!(outcome(&verdict), fail("count_differs"));
        assert_eq!((verdict.raw["rows"].clone(), verdict.raw["expected"].clone()), (json!(1), json!(0)));
    }

    #[test]
    fn bad_params_and_a_missing_capture_are_grader_outcomes() {
        let one = stage(vec![row("s", vec![])]);
        assert_eq!(outcome(&MailboxItemCount.evaluate(&json!({"count": "one"}), &one)), grader("bad_params"));
        assert_eq!(outcome(&MailboxItemCount.evaluate(&json!({"count": 1}), &no_items())), grader("missing_capture"));
        assert_eq!((MailboxItemCount.name(), MailboxItemCount.version()), ("mailbox_item_count", "1"));
    }
}
```

`mailbox_open_row.rs` tests:

```rust
#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::eval::checks::mailbox::test_support::{fail, grader, no_items, outcome, pass, row, stage};

    #[test]
    fn exactly_one_open_row_passes_and_everything_else_says_why_not() {
        let open = row("s", vec![]);
        assert_eq!(outcome(&MailboxOpenRow.evaluate(&json!({}), &stage(vec![open.clone()]))), pass("one_open_row"));
        assert_eq!(outcome(&MailboxOpenRow.evaluate(&Value::Null, &stage(vec![open.clone()]))), pass("one_open_row"));
        assert_eq!(outcome(&MailboxOpenRow.evaluate(&json!({}), &stage(vec![]))), fail("no_rows"));
        assert_eq!(outcome(&MailboxOpenRow.evaluate(&json!({}), &stage(vec![open.clone(), open.clone()]))), fail("several_rows"));
        let mut dismissed = open;
        dismissed["status"] = json!("dismissed");
        let verdict = MailboxOpenRow.evaluate(&json!({}), &stage(vec![dismissed]));
        assert_eq!(outcome(&verdict), fail("not_open"));
        assert_eq!(verdict.raw["status"], "dismissed");
    }

    #[test]
    fn a_row_without_status_bad_params_and_no_capture_are_grader_outcomes() {
        assert_eq!(outcome(&MailboxOpenRow.evaluate(&json!({}), &stage(vec![json!({"title": "t"})]))), grader("missing_field"));
        assert_eq!(outcome(&MailboxOpenRow.evaluate(&json!({"count": 1}), &stage(vec![]))), grader("bad_params"));
        assert_eq!(outcome(&MailboxOpenRow.evaluate(&json!({}), &no_items())), grader("missing_capture"));
    }
}
```

`mailbox_text_excludes.rs` tests:

```rust
#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::eval::checks::mailbox::test_support::{fail, finding, grader, no_items, outcome, pass, row, stage};

    fn rows() -> Vec<Value> {
        vec![row(
            "Disk usage high on archive-02",
            vec![finding("c-1", "sync_worker_lagging", "open", "4419 jobs behind")],
        )]
    }

    #[test]
    fn a_banned_word_absent_from_title_summary_and_findings_passes() {
        let verdict = MailboxTextExcludes.evaluate(&json!({"keywords": ["inode", "state"]}), &stage(rows()));
        assert_eq!(outcome(&verdict), pass("absent"), "payload JSON keys are not text: {}", verdict.raw);
        assert_eq!(verdict.raw["rows"], 1);
    }

    #[test]
    fn a_banned_word_in_the_summary_or_a_finding_fails_and_names_where() {
        let verdict = MailboxTextExcludes.evaluate(&json!({"keywords": ["disk", "sync-worker"]}), &stage(rows()));
        assert_eq!(outcome(&verdict), fail("keyword_present"));
        assert_eq!(
            verdict.raw["hits"],
            json!([
                {"row": 0, "field": "summary", "keyword": "disk"},
                {"row": 0, "field": "payload", "keyword": "sync-worker"},
            ])
        );
    }

    #[test]
    fn an_unreadable_payload_is_still_searched_as_text() {
        let broken = json!({"title": "t", "summary": null, "payload": "not json: pcap-export stuck"});
        let verdict = MailboxTextExcludes.evaluate(&json!({"keywords": ["pcap-export"]}), &stage(vec![broken]));
        assert_eq!(outcome(&verdict), fail("keyword_present"));
        assert_eq!(verdict.raw["hits"][0]["field"], "payload");
    }

    #[test]
    fn grader_outcomes() {
        let no_summary = json!({"title": "t", "payload": null});
        assert_eq!(outcome(&MailboxTextExcludes.evaluate(&json!({"keywords": ["x"]}), &stage(vec![no_summary]))), grader("missing_field"));
        assert_eq!(outcome(&MailboxTextExcludes.evaluate(&json!({"keywords": []}), &stage(rows()))), grader("bad_params"));
        assert_eq!(outcome(&MailboxTextExcludes.evaluate(&json!({"keywords": ["%%"]}), &stage(rows()))), grader("bad_params"));
        assert_eq!(outcome(&MailboxTextExcludes.evaluate(&json!({"keywords": ["x"]}), &no_items())), grader("missing_capture"));
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `LOG=$TMPDIR/m3.log; CARGO_BUILD_JOBS=4 cargo test -p gents --lib eval::checks::mailbox_ > "$LOG" 2>&1; grep -E "test result|FAILED|panicked" "$LOG"`
Expected: FAIL: every new test panics with `not yet implemented`.

- [ ] **Step 3: Implement.** `mailbox_item_count.rs`:

```rust
//! `mailbox_item_count`: how many mailbox rows the stage left.

use serde::Deserialize;
use serde_json::{json, Value};

use crate::eval::checks::mailbox::{failed, item_rows, parse_params, passed};
use crate::eval::checks::{Check, CheckVerdict};
use crate::eval::runner::executor::StageEvidence;

/// Params: `{"count": <u64>}`. Passes when the `items` capture holds exactly
/// `count` rows. Reason codes: `count_matches`, `count_differs`; grader:
/// `bad_params`, `missing_capture`.
pub struct MailboxItemCount;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Params {
    count: u64,
}

impl Check for MailboxItemCount {
    fn name(&self) -> &'static str {
        "mailbox_item_count"
    }

    fn version(&self) -> &'static str {
        "1"
    }

    fn evaluate(&self, params: &Value, stage: &StageEvidence) -> CheckVerdict {
        judge(params, stage).unwrap_or_else(|verdict| verdict)
    }
}

fn judge(params: &Value, stage: &StageEvidence) -> Result<CheckVerdict, CheckVerdict> {
    let params: Params = parse_params(params)?;
    let found = item_rows(stage)?.len() as u64;
    let extra = json!({"rows": found, "expected": params.count});
    Ok(if found == params.count {
        passed("count_matches", format!("{found} rows"), extra)
    } else {
        failed(
            "count_differs",
            format!("{found} rows, expected {}", params.count),
            extra,
        )
    })
}
```

`mailbox_open_row.rs`:

```rust
//! `mailbox_open_row`: the subject keeps exactly one open record.

use serde_json::{json, Value};

use crate::eval::checks::mailbox::{failed, field, item_rows, parse_params, passed, NoParams};
use crate::eval::checks::{Check, CheckVerdict};
use crate::eval::runner::executor::StageEvidence;

/// Params: `{}`. Passes when the `items` capture holds exactly one row and its
/// `status` is `"open"`. Reason codes: `one_open_row`, `no_rows`,
/// `several_rows`, `not_open`; grader: `bad_params`, `missing_capture`,
/// `missing_field` (the capture did not select `status`).
pub struct MailboxOpenRow;

impl Check for MailboxOpenRow {
    fn name(&self) -> &'static str {
        "mailbox_open_row"
    }

    fn version(&self) -> &'static str {
        "1"
    }

    fn evaluate(&self, params: &Value, stage: &StageEvidence) -> CheckVerdict {
        judge(params, stage).unwrap_or_else(|verdict| verdict)
    }
}

fn judge(params: &Value, stage: &StageEvidence) -> Result<CheckVerdict, CheckVerdict> {
    let NoParams {} = parse_params(params)?;
    let rows = item_rows(stage)?;
    Ok(match rows {
        [] => failed(
            "no_rows",
            "the stage left no mailbox row".into(),
            json!({"rows": 0}),
        ),
        [row] => {
            let status = field(row, 0, "status")?;
            let extra = json!({"rows": 1, "status": status});
            if status.as_str() == Some("open") {
                passed("one_open_row", "one row, open".into(), extra)
            } else {
                failed("not_open", format!("the one row has status {status}"), extra)
            }
        }
        _ => failed(
            "several_rows",
            format!("{} rows, expected one open row", rows.len()),
            json!({"rows": rows.len()}),
        ),
    })
}
```

`mailbox_text_excludes.rs`:

```rust
//! `mailbox_text_excludes`: banned words appear nowhere in the mailbox rows.

use serde::Deserialize;
use serde_json::{json, Value};

use crate::eval::checks::mailbox::{
    contains, failed, field, grader, item_rows, keyword_tokens, parse_params, parse_payload,
    passed, text_field, tokens, Finding,
};
use crate::eval::checks::{Check, CheckVerdict};
use crate::eval::runner::executor::StageEvidence;

/// Params: `{"keywords": [<string>, …]}`, at least one. Passes when no keyword
/// occurs (token match) in any row's `title`, `summary` or `payload`. The
/// payload is read through its parsed findings' `condition + " " + detail`,
/// and as raw text only when it does not parse. This is the inherited #1512
/// three-field scope, kept deliberately: a chatty summary fails a ban (see
/// CHECKS.md "Gaps"). Reason codes: `absent`, `keyword_present`; grader:
/// `bad_params`, `missing_capture`, `missing_field`.
pub struct MailboxTextExcludes;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Params {
    keywords: Vec<String>,
}

impl Check for MailboxTextExcludes {
    fn name(&self) -> &'static str {
        "mailbox_text_excludes"
    }

    fn version(&self) -> &'static str {
        "1"
    }

    fn evaluate(&self, params: &Value, stage: &StageEvidence) -> CheckVerdict {
        judge(params, stage).unwrap_or_else(|verdict| verdict)
    }
}

fn judge(params: &Value, stage: &StageEvidence) -> Result<CheckVerdict, CheckVerdict> {
    let params: Params = parse_params(params)?;
    if params.keywords.is_empty() {
        return Err(grader("bad_params", "keywords names no keyword".into()));
    }
    let banned = keyword_tokens(&params.keywords)?;
    let rows = item_rows(stage)?;
    let mut hits = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let title = vec![tokens(text_field(row, index, "title")?)];
        let summary = vec![tokens(text_field(row, index, "summary")?)];
        let payload = field(row, index, "payload")?;
        let payload: Vec<Vec<String>> = match parse_payload(payload, None) {
            Ok(found) => found.iter().map(Finding::text_tokens).collect(),
            // Unreadable is still text the subject wrote.
            Err(_) => vec![tokens(payload.as_str().unwrap_or_default())],
        };
        for (name, texts) in [("title", title), ("summary", summary), ("payload", payload)] {
            for (keyword, wanted) in params.keywords.iter().zip(&banned) {
                if texts.iter().any(|text| contains(text, wanted)) {
                    hits.push(json!({"row": index, "field": name, "keyword": keyword}));
                }
            }
        }
    }
    Ok(if hits.is_empty() {
        passed(
            "absent",
            format!("none of {} keywords appears", params.keywords.len()),
            json!({"rows": rows.len()}),
        )
    } else {
        failed(
            "keyword_present",
            format!("{} banned keyword hits", hits.len()),
            json!({"rows": rows.len(), "hits": hits}),
        )
    })
}
```

- [ ] **Step 4: Run to verify they pass**

Run: `LOG=$TMPDIR/m3.log; CARGO_BUILD_JOBS=4 cargo test -p gents --lib eval::checks > "$LOG" 2>&1; grep -E "test result|FAILED|panicked" "$LOG"`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all --check
git add crates/gents/src/eval/checks
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" commit -m "feat(eval): mailbox_item_count, mailbox_open_row and mailbox_text_excludes checks" -m "$TRAILER"
```

### Task 7: Payload checks: `payload_well_formed`, `finding_pairs_unique`, `finding_count`, `finding_state_counts`

**Files:**
- Create: `crates/gents/src/eval/checks/payload_well_formed.rs`, `finding_pairs_unique.rs`, `finding_count.rs`, `finding_state_counts.rs`
- Modify: `crates/gents/src/eval/checks/mod.rs` (add the four `pub mod` lines in rustfmt order)

**Interfaces:**
- Consumes: Task 5's `mailbox` items; `Check`.
- Produces: `pub struct PayloadWellFormed;` (`"payload_well_formed"`, `{version: u64}`, codes `well_formed` and the six `payload_*` defect codes); `pub struct FindingPairsUnique;` (`"finding_pairs_unique"`, `{}`, `unique` / `duplicate_pair`); `pub struct FindingCount;` (`"finding_count"`, `{count: u64}`, `count_matches` / `count_differs`); `pub struct FindingStateCounts;` (`"finding_state_counts"`, `{open: u64, resolved: u64}`, `counts_match` / `counts_differ`). The three finding checks also emit `payload_unreadable`. All emit `bad_params`, `missing_capture`, `missing_field`; `version() == "1"`; `raw.rows`.

- [ ] **Step 1: Write the four files with `evaluate` as `todo!()` and these tests.**

`payload_well_formed.rs` tests:

```rust
#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::eval::checks::mailbox::test_support::{fail, finding, grader, no_items, outcome, pass, row, stage};

    fn with_payload(payload: Value) -> Value {
        json!({"title": "t", "summary": "s", "status": "open", "payload": payload})
    }

    #[test]
    fn every_row_in_the_contract_shape_passes_and_no_rows_pass_vacuously() {
        let rows = vec![row("s", vec![finding("c-1", "a", "open", "x"), finding("c-1", "b", "resolved", "y")])];
        let verdict = PayloadWellFormed.evaluate(&json!({"version": 1}), &stage(rows));
        assert_eq!(outcome(&verdict), pass("well_formed"));
        assert_eq!((verdict.raw["rows"].clone(), verdict.raw["findings"].clone()), (json!(1), json!(2)));
        assert_eq!(outcome(&PayloadWellFormed.evaluate(&json!({"version": 1}), &stage(vec![]))), pass("well_formed"));
    }

    #[test]
    fn each_defect_fails_with_its_own_code_and_names_the_row() {
        let cases = [
            (json!(null), "payload_missing"),
            (json!({"version": 1, "findings": []}), "payload_not_string"),
            (json!("{"), "payload_not_json"),
            (json!(r#"{"version": 1}"#), "payload_bad_shape"),
            (json!(r#"{"version": 2, "findings": []}"#), "payload_wrong_version"),
            (json!(r#"{"version": 1, "findings": [{"correlation": "c", "condition": "x", "state": "closed", "detail": "d"}]}"#), "payload_bad_state"),
        ];
        for (payload, code) in cases {
            let rows = vec![row("fine", vec![]), with_payload(payload.clone())];
            let verdict = PayloadWellFormed.evaluate(&json!({"version": 1}), &stage(rows));
            assert_eq!(outcome(&verdict), fail(code), "{payload}");
            assert_eq!(verdict.raw["row"], 1);
        }
    }

    #[test]
    fn grader_outcomes() {
        assert_eq!(outcome(&PayloadWellFormed.evaluate(&json!({"version": 1}), &stage(vec![json!({"title": "t"})]))), grader("missing_field"));
        assert_eq!(outcome(&PayloadWellFormed.evaluate(&json!({"version": "1"}), &stage(vec![]))), grader("bad_params"));
        assert_eq!(outcome(&PayloadWellFormed.evaluate(&json!({"version": 1}), &no_items())), grader("missing_capture"));
    }
}
```

`finding_pairs_unique.rs` tests:

```rust
#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::eval::checks::mailbox::test_support::{fail, finding, grader, no_items, outcome, pass, row, stage};

    #[test]
    fn a_pair_seen_twice_across_rows_fails_and_distinct_pairs_pass() {
        let distinct = vec![row("s", vec![finding("c-1", "fan", "open", "x"), finding("c-2", "fan", "open", "x")])];
        assert_eq!(outcome(&FindingPairsUnique.evaluate(&json!({}), &stage(distinct))), pass("unique"));
        let twice = vec![
            row("s", vec![finding("c-1", "fan", "open", "x")]),
            row("s", vec![finding("c-1", "fan", "resolved", "y")]),
        ];
        let verdict = FindingPairsUnique.evaluate(&Value::Null, &stage(twice));
        assert_eq!(outcome(&verdict), fail("duplicate_pair"));
        assert_eq!((verdict.raw["correlation"].clone(), verdict.raw["condition"].clone()), (json!("c-1"), json!("fan")));
    }

    #[test]
    fn unreadable_payloads_fail_and_graders_stay_graders() {
        let broken = vec![json!({"payload": "nope"})];
        assert_eq!(outcome(&FindingPairsUnique.evaluate(&json!({}), &stage(broken))), fail("payload_unreadable"));
        assert_eq!(outcome(&FindingPairsUnique.evaluate(&json!({}), &stage(vec![json!({})]))), grader("missing_field"));
        assert_eq!(outcome(&FindingPairsUnique.evaluate(&json!({"x": 1}), &stage(vec![]))), grader("bad_params"));
        assert_eq!(outcome(&FindingPairsUnique.evaluate(&json!({}), &no_items())), grader("missing_capture"));
    }
}
```

`finding_count.rs` tests:

```rust
#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::eval::checks::mailbox::test_support::{fail, finding, grader, no_items, outcome, pass, row, stage};

    #[test]
    fn findings_are_counted_across_rows() {
        let rows = vec![
            row("s", vec![finding("c-1", "a", "open", "x")]),
            row("s", vec![finding("c-2", "b", "open", "y")]),
        ];
        assert_eq!(outcome(&FindingCount.evaluate(&json!({"count": 2}), &stage(rows.clone()))), pass("count_matches"));
        let verdict = FindingCount.evaluate(&json!({"count": 1}), &stage(rows));
        assert_eq!(outcome(&verdict), fail("count_differs"));
        assert_eq!((verdict.raw["findings"].clone(), verdict.raw["expected"].clone()), (json!(2), json!(1)));
        assert_eq!(outcome(&FindingCount.evaluate(&json!({"count": 0}), &stage(vec![]))), pass("count_matches"));
    }

    #[test]
    fn failures_that_are_not_about_the_count() {
        assert_eq!(outcome(&FindingCount.evaluate(&json!({"count": 0}), &stage(vec![json!({"payload": null})]))), fail("payload_unreadable"));
        assert_eq!(outcome(&FindingCount.evaluate(&json!({"count": -1}), &stage(vec![]))), grader("bad_params"));
        assert_eq!(outcome(&FindingCount.evaluate(&json!({"count": 0}), &no_items())), grader("missing_capture"));
    }
}
```

`finding_state_counts.rs` tests:

```rust
#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::eval::checks::mailbox::test_support::{fail, finding, grader, no_items, outcome, pass, row, stage};

    #[test]
    fn open_and_resolved_are_tallied_separately() {
        let rows = vec![row("s", vec![finding("c-1", "a", "open", "x"), finding("c-1", "b", "resolved", "y")])];
        assert_eq!(outcome(&FindingStateCounts.evaluate(&json!({"open": 1, "resolved": 1}), &stage(rows.clone()))), pass("counts_match"));
        let verdict = FindingStateCounts.evaluate(&json!({"open": 0, "resolved": 2}), &stage(rows));
        assert_eq!(outcome(&verdict), fail("counts_differ"));
        assert_eq!((verdict.raw["open"].clone(), verdict.raw["resolved"].clone()), (json!(1), json!(1)));
    }

    #[test]
    fn failures_that_are_not_about_the_tally() {
        assert_eq!(outcome(&FindingStateCounts.evaluate(&json!({"open": 0, "resolved": 0}), &stage(vec![json!({"payload": "x"})]))), fail("payload_unreadable"));
        assert_eq!(outcome(&FindingStateCounts.evaluate(&json!({"open": 1}), &stage(vec![]))), grader("bad_params"));
        assert_eq!(outcome(&FindingStateCounts.evaluate(&json!({"open": 0, "resolved": 0}), &no_items())), grader("missing_capture"));
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `LOG=$TMPDIR/m3.log; CARGO_BUILD_JOBS=4 cargo test -p gents --lib eval::checks > "$LOG" 2>&1; grep -E "test result|FAILED|panicked" "$LOG"`
Expected: FAIL: the new tests panic with `not yet implemented`.

- [ ] **Step 3: Implement.** Each file keeps its tests and gets the `impl Check` shape of Task 6 (`name`, `version` returning `"1"`, `evaluate` calling `judge(params, stage).unwrap_or_else(|verdict| verdict)`).

`payload_well_formed.rs`:

```rust
//! `payload_well_formed`: every row's payload is the subject's output contract.

use serde::Deserialize;
use serde_json::{json, Value};

use crate::eval::checks::mailbox::{failed, field, item_rows, parse_params, parse_payload, passed};
use crate::eval::checks::{Check, CheckVerdict};
use crate::eval::runner::executor::StageEvidence;

/// Params: `{"version": <u64>}`. Passes when every row's `payload` is JSON text
/// of `{"version": <version>, "findings": [{correlation, condition, state,
/// detail}]}` with `state` in `{open, resolved}`; no rows pass vacuously.
/// Reason codes: `well_formed`, `payload_missing`, `payload_not_string`,
/// `payload_not_json`, `payload_bad_shape`, `payload_wrong_version`,
/// `payload_bad_state`; grader: `bad_params`, `missing_capture`,
/// `missing_field`.
pub struct PayloadWellFormed;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Params {
    version: u64,
}

impl Check for PayloadWellFormed {
    fn name(&self) -> &'static str {
        "payload_well_formed"
    }

    fn version(&self) -> &'static str {
        "1"
    }

    fn evaluate(&self, params: &Value, stage: &StageEvidence) -> CheckVerdict {
        judge(params, stage).unwrap_or_else(|verdict| verdict)
    }
}

fn judge(params: &Value, stage: &StageEvidence) -> Result<CheckVerdict, CheckVerdict> {
    let params: Params = parse_params(params)?;
    let rows = item_rows(stage)?;
    let mut findings = 0usize;
    for (index, row) in rows.iter().enumerate() {
        match parse_payload(field(row, index, "payload")?, Some(params.version)) {
            Ok(found) => findings += found.len(),
            Err(defect) => {
                return Ok(failed(
                    defect.reason_code(),
                    format!("row {index}: {}", defect.reason_code()),
                    json!({"rows": rows.len(), "row": index}),
                ))
            }
        }
    }
    Ok(passed(
        "well_formed",
        format!("{} rows, {findings} findings", rows.len()),
        json!({"rows": rows.len(), "findings": findings}),
    ))
}
```

`finding_pairs_unique.rs`:

```rust
//! `finding_pairs_unique`: one finding per (correlation, condition).

use std::collections::BTreeSet;

use serde_json::{json, Value};

use crate::eval::checks::mailbox::{all_findings, failed, item_rows, parse_params, passed, NoParams};
use crate::eval::checks::{Check, CheckVerdict};
use crate::eval::runner::executor::StageEvidence;

/// Params: `{}`. Passes when no two findings, across all rows, share
/// `(correlation, condition)`. Reason codes: `unique`, `duplicate_pair`,
/// `payload_unreadable`; grader: `bad_params`, `missing_capture`,
/// `missing_field`.
pub struct FindingPairsUnique;

impl Check for FindingPairsUnique {
    fn name(&self) -> &'static str {
        "finding_pairs_unique"
    }

    fn version(&self) -> &'static str {
        "1"
    }

    fn evaluate(&self, params: &Value, stage: &StageEvidence) -> CheckVerdict {
        judge(params, stage).unwrap_or_else(|verdict| verdict)
    }
}

fn judge(params: &Value, stage: &StageEvidence) -> Result<CheckVerdict, CheckVerdict> {
    let NoParams {} = parse_params(params)?;
    let rows = item_rows(stage)?;
    let found = all_findings(rows)?;
    let mut seen = BTreeSet::new();
    for finding in &found {
        if !seen.insert((finding.correlation.as_str(), finding.condition.as_str())) {
            return Ok(failed(
                "duplicate_pair",
                format!(
                    "({}, {}) appears more than once",
                    finding.correlation, finding.condition
                ),
                json!({"rows": rows.len(), "correlation": finding.correlation, "condition": finding.condition}),
            ));
        }
    }
    Ok(passed(
        "unique",
        format!("{} findings, each pair once", found.len()),
        json!({"rows": rows.len(), "findings": found.len()}),
    ))
}
```

`finding_count.rs`:

```rust
//! `finding_count`: how many findings the rows hold in total.

use serde::Deserialize;
use serde_json::{json, Value};

use crate::eval::checks::mailbox::{all_findings, failed, item_rows, parse_params, passed};
use crate::eval::checks::{Check, CheckVerdict};
use crate::eval::runner::executor::StageEvidence;

/// Params: `{"count": <u64>}`. Passes when the findings of all rows number
/// exactly `count`. Reason codes: `count_matches`, `count_differs`,
/// `payload_unreadable`; grader: `bad_params`, `missing_capture`,
/// `missing_field`.
pub struct FindingCount;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Params {
    count: u64,
}

impl Check for FindingCount {
    fn name(&self) -> &'static str {
        "finding_count"
    }

    fn version(&self) -> &'static str {
        "1"
    }

    fn evaluate(&self, params: &Value, stage: &StageEvidence) -> CheckVerdict {
        judge(params, stage).unwrap_or_else(|verdict| verdict)
    }
}

fn judge(params: &Value, stage: &StageEvidence) -> Result<CheckVerdict, CheckVerdict> {
    let params: Params = parse_params(params)?;
    let rows = item_rows(stage)?;
    let found = all_findings(rows)?.len() as u64;
    let extra = json!({"rows": rows.len(), "findings": found, "expected": params.count});
    Ok(if found == params.count {
        passed("count_matches", format!("{found} findings"), extra)
    } else {
        failed(
            "count_differs",
            format!("{found} findings, expected {}", params.count),
            extra,
        )
    })
}
```

`finding_state_counts.rs`:

```rust
//! `finding_state_counts`: how many findings are open and how many resolved.

use serde::Deserialize;
use serde_json::{json, Value};

use crate::eval::checks::mailbox::{all_findings, failed, item_rows, parse_params, passed, FindingState};
use crate::eval::checks::{Check, CheckVerdict};
use crate::eval::runner::executor::StageEvidence;

/// Params: `{"open": <u64>, "resolved": <u64>}`. Passes when the findings of
/// all rows tally exactly so. Reason codes: `counts_match`, `counts_differ`,
/// `payload_unreadable`; grader: `bad_params`, `missing_capture`,
/// `missing_field`.
pub struct FindingStateCounts;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Params {
    open: u64,
    resolved: u64,
}

impl Check for FindingStateCounts {
    fn name(&self) -> &'static str {
        "finding_state_counts"
    }

    fn version(&self) -> &'static str {
        "1"
    }

    fn evaluate(&self, params: &Value, stage: &StageEvidence) -> CheckVerdict {
        judge(params, stage).unwrap_or_else(|verdict| verdict)
    }
}

fn judge(params: &Value, stage: &StageEvidence) -> Result<CheckVerdict, CheckVerdict> {
    let params: Params = parse_params(params)?;
    let rows = item_rows(stage)?;
    let found = all_findings(rows)?;
    let open = found.iter().filter(|finding| finding.state == FindingState::Open).count() as u64;
    let resolved = found.len() as u64 - open;
    let extra = json!({
        "rows": rows.len(),
        "open": open,
        "resolved": resolved,
        "expected_open": params.open,
        "expected_resolved": params.resolved,
    });
    Ok(if (open, resolved) == (params.open, params.resolved) {
        passed("counts_match", format!("{open} open, {resolved} resolved"), extra)
    } else {
        failed(
            "counts_differ",
            format!(
                "{open} open and {resolved} resolved, expected {} and {}",
                params.open, params.resolved
            ),
            extra,
        )
    })
}
```

- [ ] **Step 4: Run to verify they pass**

Run: `LOG=$TMPDIR/m3.log; CARGO_BUILD_JOBS=4 cargo test -p gents --lib eval::checks > "$LOG" 2>&1; grep -E "test result|FAILED|panicked" "$LOG"`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all --check
git add crates/gents/src/eval/checks
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" commit -m "feat(eval): payload_well_formed, finding_pairs_unique, finding_count and finding_state_counts" -m "$TRAILER"
```

### Task 8: Finding checks: `findings_match`, `finding_text_excludes`

**Files:**
- Create: `crates/gents/src/eval/checks/findings_match.rs`, `finding_text_excludes.rs`
- Modify: `crates/gents/src/eval/checks/mod.rs` (two `pub mod` lines in rustfmt order)

**Interfaces:**
- Consumes: Task 5's `mailbox` items (`Finding`, `FindingState`, `all_findings`, `keyword_tokens`, `contains`, …).
- Produces: `pub struct FindingsMatch;` (`"findings_match"`, params `{matchers: [{correlation, state, keywords_all}], exact: bool (default false)}`, codes `matched`, `unmatched_matcher`, `unexpected_finding`, `payload_unreadable`); raw on `unmatched_matcher`: `{rows, unmatched: [{matcher, correlation, state, keywords_all, near: [{finding, missing_keywords}]}], findings}`; on `unexpected_finding`: `{rows, unexpected, findings}`. `pub struct FindingTextExcludes;` (`"finding_text_excludes"`, `{correlation, keywords}`, codes `absent`, `keyword_present`, `payload_unreadable`). Both: `bad_params`, `missing_capture`, `missing_field`; `version() == "1"`.

- [ ] **Step 1: Write both files with `evaluate` as `todo!()` and these tests.**

`findings_match.rs` tests:

```rust
#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::eval::checks::mailbox::test_support::{fail, finding, grader, no_items, outcome, pass, row, stage};

    fn matcher(correlation: &str, state: &str, keywords: &[&str]) -> Value {
        json!({"correlation": correlation, "state": state, "keywords_all": keywords})
    }

    #[test]
    fn every_matcher_finds_its_own_finding_by_tokens() {
        let rows = vec![row("s", vec![
            finding("c-01-a", "disk_usage_high", "open", "/var at 88% and rising"),
            finding("c-18-a", "cert_renewer_stopped", "open", "cert-renewer is stopped"),
        ])];
        let params = json!({"matchers": [
            matcher("c-01-a", "open", &["disk", "88"]),
            matcher("c-18-a", "open", &["cert", "stopped"]),
        ], "exact": true});
        let verdict = FindingsMatch.evaluate(&params, &stage(rows));
        assert_eq!(outcome(&verdict), pass("matched"), "{}", verdict.raw);
    }

    #[test]
    fn the_assignment_is_one_to_one_and_finds_it_when_a_greedy_pass_would_not() {
        // m0 fits both findings, m1 only the first: greedy m0 -> f0 would
        // strand m1; the augmenting path moves m0 to f1.
        let rows = vec![row("s", vec![
            finding("c-1", "fan_stalled", "open", "fan 2 stalled at 0 rpm"),
            finding("c-1", "fan_stalled_psu", "open", "fan stalled"),
        ])];
        let params = json!({"matchers": [
            matcher("c-1", "open", &["fan", "stalled"]),
            matcher("c-1", "open", &["fan", "rpm"]),
        ], "exact": true});
        assert_eq!(outcome(&FindingsMatch.evaluate(&params, &stage(rows))), pass("matched"));

        let one = vec![row("s", vec![finding("c-1", "fan_stalled", "open", "stalled")])];
        let twice = json!({"matchers": [matcher("c-1", "open", &["fan"]), matcher("c-1", "open", &["fan"])]});
        let verdict = FindingsMatch.evaluate(&twice, &stage(one));
        assert_eq!(outcome(&verdict), fail("unmatched_matcher"));
        assert_eq!(verdict.raw["unmatched"][0]["matcher"], 1);
    }

    #[test]
    fn an_unmatched_matcher_reports_the_keywords_its_near_findings_lack() {
        let rows = vec![row("s", vec![finding("c-01-a", "disk_usage_high", "open", "/var nearly full")])];
        let params = json!({"matchers": [matcher("c-01-a", "open", &["disk", "88"])]});
        let verdict = FindingsMatch.evaluate(&params, &stage(rows));
        assert_eq!(outcome(&verdict), fail("unmatched_matcher"));
        assert_eq!(verdict.raw["unmatched"][0]["near"], json!([{"finding": 0, "missing_keywords": ["88"]}]));
        assert_eq!(verdict.raw["findings"][0]["condition"], "disk_usage_high");

        let wrong_state = vec![row("s", vec![finding("c-09-a", "replication_lag", "open", "lag caught up")])];
        let params = json!({"matchers": [matcher("c-09-a", "resolved", &["replication", "lag"])]});
        let verdict = FindingsMatch.evaluate(&params, &stage(wrong_state));
        assert_eq!(outcome(&verdict), fail("unmatched_matcher"));
        assert_eq!(verdict.raw["unmatched"][0]["near"], json!([]));
    }

    #[test]
    fn exact_refuses_a_leftover_finding_and_inexact_allows_it() {
        let rows = vec![row("s", vec![
            finding("c-1", "disk_usage_high", "open", "88%"),
            finding("c-1", "inode_usage_high", "open", "inodes"),
        ])];
        let one = json!([matcher("c-1", "open", &["disk"])]);
        let verdict = FindingsMatch.evaluate(&json!({"matchers": one, "exact": true}), &stage(rows.clone()));
        assert_eq!(outcome(&verdict), fail("unexpected_finding"));
        assert_eq!(verdict.raw["unexpected"][0]["condition"], "inode_usage_high");
        assert_eq!(outcome(&FindingsMatch.evaluate(&json!({"matchers": one}), &stage(rows))), pass("matched"));
        assert_eq!(
            outcome(&FindingsMatch.evaluate(&json!({"matchers": [], "exact": true}), &stage(vec![]))),
            pass("matched"),
            "no row and no matcher is the zero-condition expectation"
        );
    }

    #[test]
    fn bad_params_unreadable_payloads_and_missing_captures() {
        for params in [
            json!({"matchers": [matcher("c", "closed", &["x"])]}),
            json!({"matchers": [matcher("c", "open", &[])]}),
            json!({"matchers": [matcher("c", "open", &["--"])]}),
            json!({"matchers": [], "extra": 1}),
        ] {
            assert_eq!(outcome(&FindingsMatch.evaluate(&params, &stage(vec![]))), grader("bad_params"), "{params}");
        }
        let params = json!({"matchers": [matcher("c", "open", &["x"])]});
        assert_eq!(outcome(&FindingsMatch.evaluate(&params, &stage(vec![json!({"payload": "{"})]))), fail("payload_unreadable"));
        assert_eq!(outcome(&FindingsMatch.evaluate(&params, &stage(vec![json!({})]))), grader("missing_field"));
        assert_eq!(outcome(&FindingsMatch.evaluate(&params, &no_items())), grader("missing_capture"));
    }
}
```

`finding_text_excludes.rs` tests:

```rust
#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::eval::checks::mailbox::test_support::{fail, finding, grader, no_items, outcome, pass, row, stage};

    fn rows() -> Vec<Value> {
        vec![row("s", vec![
            finding("c-13-a", "api_errors", "open", "api-gateway returning 502"),
            finding("c-13-b", "worker_pool_exhausted", "open", "730 jobs queued"),
        ])]
    }

    #[test]
    fn a_ban_binds_only_its_own_correlation() {
        let params = json!({"correlation": "c-13-a", "keywords": ["worker"]});
        assert_eq!(outcome(&FindingTextExcludes.evaluate(&params, &stage(rows()))), pass("absent"));
        let params = json!({"correlation": "c-13-b", "keywords": ["worker-pool", "502"]});
        let verdict = FindingTextExcludes.evaluate(&params, &stage(rows()));
        assert_eq!(outcome(&verdict), fail("keyword_present"));
        assert_eq!(verdict.raw["hits"], json!([{"finding": 1, "condition": "worker_pool_exhausted", "keyword": "worker-pool"}]));
    }

    #[test]
    fn failures_that_are_not_about_the_ban() {
        let params = json!({"correlation": "c", "keywords": ["x"]});
        assert_eq!(outcome(&FindingTextExcludes.evaluate(&params, &stage(vec![json!({"payload": 3})]))), fail("payload_unreadable"));
        assert_eq!(outcome(&FindingTextExcludes.evaluate(&params, &stage(vec![json!({})]))), grader("missing_field"));
        assert_eq!(outcome(&FindingTextExcludes.evaluate(&json!({"correlation": "c", "keywords": []}), &stage(vec![]))), grader("bad_params"));
        assert_eq!(outcome(&FindingTextExcludes.evaluate(&json!({"keywords": ["x"]}), &stage(vec![]))), grader("bad_params"));
        assert_eq!(outcome(&FindingTextExcludes.evaluate(&params, &no_items())), grader("missing_capture"));
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `LOG=$TMPDIR/m3.log; CARGO_BUILD_JOBS=4 cargo test -p gents --lib eval::checks > "$LOG" 2>&1; grep -E "test result|FAILED|panicked" "$LOG"`
Expected: FAIL: the new tests panic with `not yet implemented`.

- [ ] **Step 3: Implement.** `findings_match.rs`:

```rust
//! `findings_match`: the findings the case expects are there, each once.

use std::collections::BTreeSet;

use serde::Deserialize;
use serde_json::{json, Value};

use crate::eval::checks::mailbox::{
    all_findings, contains, failed, grader, item_rows, keyword_tokens, parse_params, passed,
    Finding, FindingState,
};
use crate::eval::checks::{Check, CheckVerdict};
use crate::eval::runner::executor::StageEvidence;

/// Params: `{"matchers": [{"correlation", "state", "keywords_all": [..]}],
/// "exact": <bool, default false>}`. A matcher is satisfied by a finding with
/// its correlation and state whose `condition + " " + detail` contains every
/// keyword (token match). Passes when a one-to-one assignment satisfies every
/// matcher and, with `exact`, leaves no finding over. Reason codes: `matched`,
/// `unmatched_matcher`, `unexpected_finding`, `payload_unreadable`; grader:
/// `bad_params`, `missing_capture`, `missing_field`.
pub struct FindingsMatch;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Matcher {
    correlation: String,
    state: FindingState,
    keywords_all: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Params {
    matchers: Vec<Matcher>,
    #[serde(default)]
    exact: bool,
}

impl Check for FindingsMatch {
    fn name(&self) -> &'static str {
        "findings_match"
    }

    fn version(&self) -> &'static str {
        "1"
    }

    fn evaluate(&self, params: &Value, stage: &StageEvidence) -> CheckVerdict {
        judge(params, stage).unwrap_or_else(|verdict| verdict)
    }
}

fn judge(params: &Value, stage: &StageEvidence) -> Result<CheckVerdict, CheckVerdict> {
    let params: Params = parse_params(params)?;
    let mut wanted = Vec::with_capacity(params.matchers.len());
    for (index, matcher) in params.matchers.iter().enumerate() {
        if matcher.keywords_all.is_empty() {
            return Err(grader(
                "bad_params",
                format!("matcher {index} names no keyword"),
            ));
        }
        wanted.push(keyword_tokens(&matcher.keywords_all)?);
    }
    let rows = item_rows(stage)?;
    let found = all_findings(rows)?;
    let texts: Vec<Vec<String>> = found.iter().map(Finding::text_tokens).collect();
    let candidates: Vec<Vec<usize>> = params
        .matchers
        .iter()
        .zip(&wanted)
        .map(|(matcher, keywords)| {
            same_correlation_and_state(&found, matcher)
                .filter(|index| keywords.iter().all(|keyword| contains(&texts[*index], keyword)))
                .collect()
        })
        .collect();
    let assigned = assign(&candidates, found.len());

    let unmatched: Vec<Value> = assigned
        .iter()
        .enumerate()
        .filter(|(_, finding)| finding.is_none())
        .map(|(index, _)| {
            let matcher = &params.matchers[index];
            let near: Vec<Value> = same_correlation_and_state(&found, matcher)
                .map(|finding| {
                    let missing: Vec<&String> = matcher
                        .keywords_all
                        .iter()
                        .zip(&wanted[index])
                        .filter(|(_, keyword)| !contains(&texts[finding], keyword))
                        .map(|(keyword, _)| keyword)
                        .collect();
                    json!({"finding": finding, "missing_keywords": missing})
                })
                .collect();
            json!({
                "matcher": index,
                "correlation": matcher.correlation,
                "state": matcher.state,
                "keywords_all": matcher.keywords_all,
                "near": near,
            })
        })
        .collect();
    if !unmatched.is_empty() {
        return Ok(failed(
            "unmatched_matcher",
            format!(
                "{} of {} matchers found no finding",
                unmatched.len(),
                params.matchers.len()
            ),
            json!({"rows": rows.len(), "unmatched": unmatched, "findings": found}),
        ));
    }
    if params.exact {
        let used: BTreeSet<usize> = assigned.iter().flatten().copied().collect();
        let unexpected: Vec<&Finding> = found
            .iter()
            .enumerate()
            .filter(|(index, _)| !used.contains(index))
            .map(|(_, finding)| finding)
            .collect();
        if !unexpected.is_empty() {
            return Ok(failed(
                "unexpected_finding",
                format!("{} findings match no matcher", unexpected.len()),
                json!({"rows": rows.len(), "unexpected": unexpected, "findings": found}),
            ));
        }
    }
    Ok(passed(
        "matched",
        format!(
            "{} matchers matched among {} findings",
            params.matchers.len(),
            found.len()
        ),
        json!({"rows": rows.len(), "findings": found.len()}),
    ))
}

/// The indices of the findings a matcher could be about: its correlation and
/// state, whatever their text.
fn same_correlation_and_state<'a>(
    found: &'a [Finding],
    matcher: &'a Matcher,
) -> impl Iterator<Item = usize> + 'a {
    found
        .iter()
        .enumerate()
        .filter(move |(_, finding)| {
            finding.correlation == matcher.correlation && finding.state == matcher.state
        })
        .map(|(index, _)| index)
}

/// A maximum one-to-one assignment of matchers to findings (Kuhn's augmenting
/// paths), deterministic in matcher and finding order. `candidates[m]` lists
/// the findings matcher `m` accepts; the result gives each matcher's finding.
fn assign(candidates: &[Vec<usize>], findings: usize) -> Vec<Option<usize>> {
    let mut owner: Vec<Option<usize>> = vec![None; findings];
    for matcher in 0..candidates.len() {
        let mut seen = vec![false; findings];
        augment(matcher, candidates, &mut owner, &mut seen);
    }
    let mut assigned = vec![None; candidates.len()];
    for (finding, matcher) in owner.iter().enumerate() {
        if let Some(matcher) = matcher {
            assigned[*matcher] = Some(finding);
        }
    }
    assigned
}

fn augment(
    matcher: usize,
    candidates: &[Vec<usize>],
    owner: &mut [Option<usize>],
    seen: &mut [bool],
) -> bool {
    for &finding in &candidates[matcher] {
        if seen[finding] {
            continue;
        }
        seen[finding] = true;
        let current = owner[finding];
        if current.is_none_or(|other| augment(other, candidates, owner, seen)) {
            owner[finding] = Some(matcher);
            return true;
        }
    }
    false
}
```

`finding_text_excludes.rs`:

```rust
//! `finding_text_excludes`: one correlation's findings avoid banned words.

use serde::Deserialize;
use serde_json::{json, Value};

use crate::eval::checks::mailbox::{
    all_findings, contains, failed, grader, item_rows, keyword_tokens, parse_params, passed,
};
use crate::eval::checks::{Check, CheckVerdict};
use crate::eval::runner::executor::StageEvidence;

/// Params: `{"correlation": <string>, "keywords": [<string>, …]}`, at least one
/// keyword. Passes when no keyword occurs (token match) in the `condition` or
/// `detail` of any finding with that correlation: the negative
/// cross-correlation claim `mailbox_text_excludes` cannot express. Reason
/// codes: `absent`, `keyword_present`, `payload_unreadable`; grader:
/// `bad_params`, `missing_capture`, `missing_field`.
pub struct FindingTextExcludes;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Params {
    correlation: String,
    keywords: Vec<String>,
}

impl Check for FindingTextExcludes {
    fn name(&self) -> &'static str {
        "finding_text_excludes"
    }

    fn version(&self) -> &'static str {
        "1"
    }

    fn evaluate(&self, params: &Value, stage: &StageEvidence) -> CheckVerdict {
        judge(params, stage).unwrap_or_else(|verdict| verdict)
    }
}

fn judge(params: &Value, stage: &StageEvidence) -> Result<CheckVerdict, CheckVerdict> {
    let params: Params = parse_params(params)?;
    if params.keywords.is_empty() {
        return Err(grader("bad_params", "keywords names no keyword".into()));
    }
    let banned = keyword_tokens(&params.keywords)?;
    let rows = item_rows(stage)?;
    let found = all_findings(rows)?;
    let mut hits = Vec::new();
    for (index, finding) in found
        .iter()
        .enumerate()
        .filter(|(_, finding)| finding.correlation == params.correlation)
    {
        let text = finding.text_tokens();
        for (keyword, wanted) in params.keywords.iter().zip(&banned) {
            if contains(&text, wanted) {
                hits.push(json!({"finding": index, "condition": finding.condition, "keyword": keyword}));
            }
        }
    }
    Ok(if hits.is_empty() {
        passed(
            "absent",
            format!("no banned keyword on {}", params.correlation),
            json!({"rows": rows.len()}),
        )
    } else {
        failed(
            "keyword_present",
            format!("{} banned keyword hits on {}", hits.len(), params.correlation),
            json!({"rows": rows.len(), "hits": hits}),
        )
    })
}
```

- [ ] **Step 4: Run to verify they pass**

Run: `LOG=$TMPDIR/m3.log; CARGO_BUILD_JOBS=4 cargo test -p gents --lib eval::checks > "$LOG" 2>&1; grep -E "test result|FAILED|panicked" "$LOG"`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all --check
git add crates/gents/src/eval/checks
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" commit -m "feat(eval): findings_match with one-to-one assignment, and finding_text_excludes" -m "$TRAILER"
```

### Task 9: Register the nine checks; registry version 2

**Files:**
- Modify: `crates/gents/src/eval/checks/mod.rs` (imports, `CHECK_REGISTRY_VERSION`, `CheckRegistry::builtin`, test)

**Interfaces:**
- Consumes: the nine structs from Tasks 6–8 and `CapturedRowsCount`.
- Produces: `CHECK_REGISTRY_VERSION == "2"`; `CheckRegistry::builtin().names()` returns the ten names sorted.

- [ ] **Step 1: Write the failing test.** Replace `the_builtin_registry_holds_the_seed_check_under_its_own_name` with:

```rust
    #[test]
    fn the_builtin_registry_holds_every_check_under_its_own_name_at_version_one() {
        let registry = CheckRegistry::builtin();
        assert_eq!(
            registry.names(),
            vec![
                "captured_rows_count",
                "finding_count",
                "finding_pairs_unique",
                "finding_state_counts",
                "finding_text_excludes",
                "findings_match",
                "mailbox_item_count",
                "mailbox_open_row",
                "mailbox_text_excludes",
                "payload_well_formed",
            ]
        );
        for name in registry.names() {
            let check = registry.get(name).expect("a registered check");
            assert_eq!((check.name(), check.version()), (name, "1"));
        }
        assert!(registry.get("no_findings_for_correlation").is_none(), "not until a case needs it");
        assert_eq!(CHECK_REGISTRY_VERSION, "2");
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `LOG=$TMPDIR/m3.log; CARGO_BUILD_JOBS=4 cargo test -p gents --lib eval::checks::tests > "$LOG" 2>&1; grep -E "test result|FAILED|panicked" "$LOG"`
Expected: FAIL: `names()` is `["captured_rows_count"]`.

- [ ] **Step 3: Implement.** In `mod.rs`: set `pub const CHECK_REGISTRY_VERSION: &str = "2";` and extend its doc with `/// "2": the nine mailbox checks of M3 (spec 3a).` Add `use` lines for the nine structs (`crate::eval::checks::finding_count::FindingCount`, `finding_pairs_unique::FindingPairsUnique`, `finding_state_counts::FindingStateCounts`, `finding_text_excludes::FindingTextExcludes`, `findings_match::FindingsMatch`, `mailbox_item_count::MailboxItemCount`, `mailbox_open_row::MailboxOpenRow`, `mailbox_text_excludes::MailboxTextExcludes`, `payload_well_formed::PayloadWellFormed`) in rustfmt order, and replace the body of `builtin` with:

```rust
    pub fn builtin() -> Self {
        let mut registry = Self {
            checks: BTreeMap::new(),
        };
        let checks: [Box<dyn Check>; 10] = [
            Box::new(CapturedRowsCount),
            Box::new(FindingCount),
            Box::new(FindingPairsUnique),
            Box::new(FindingStateCounts),
            Box::new(FindingTextExcludes),
            Box::new(FindingsMatch),
            Box::new(MailboxItemCount),
            Box::new(MailboxOpenRow),
            Box::new(MailboxTextExcludes),
            Box::new(PayloadWellFormed),
        ];
        for check in checks {
            registry.register(check);
        }
        registry
    }
```

- [ ] **Step 4: Run to verify it passes, then gate the branch**

Run: `LOG=$TMPDIR/m3.log; CARGO_BUILD_JOBS=4 cargo test -p gents --lib eval > "$LOG" 2>&1; grep -E "test result|FAILED|panicked" "$LOG"`
Expected: PASS.

Run: `LOG=$TMPDIR/m3.log; CARGO_BUILD_JOBS=4 cargo check --workspace --all-targets > "$LOG" 2>&1; grep -E "^(error|warning)" "$LOG"`
Expected: no output (the Task 5 helper warnings are gone).

Run: `LOG=$TMPDIR/m3.log; CARGO_BUILD_JOBS=4 cargo test -p gents --test eval_runner_canary > "$LOG" 2>&1; grep -E "test result|FAILED" "$LOG"`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all --check
git add crates/gents/src/eval/checks/mod.rs
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" commit -m "feat(eval): register the nine mailbox checks; check registry version 2" -m "$TRAILER"
```

---

## PR 3: `eval/42-definition`

Branch `eval/42-definition` on `eval/41-checks`. Worktree `/Users/iron-arch-mage/Repos/Source/gents-eval-42-definition`.

### Task 10: The conversion script and the twenty converted cases

**Files:**
- Create: `scripts/evals/convert-expected-to-checks.py`, `scripts/evals/test_convert_expected_to_checks.py`
- Create (script output): `packs/eval_monitor_findings/cases/*.json` (twenty files)

**Interfaces:**
- Consumes: the pre-work cases at `0425499b1:packs/eval_monitor/cases/*.json` (keys `case_id, axis, split, reducer, [fixtures], stages[{stage_id, prompt, deadline_secs, capture, expected}]`); the `EvalCase` shape (with P2's `fixtures.files`).
- Produces: `convert_case(legacy: dict) -> dict`, `convert_expected(expected: dict, where: str) -> list`, `main(argv) -> int`; CLI `--input DIR --output DIR`, printing each written file name; file `cases/<case_id.replace("-", "_")>.json`, `json.dumps(case, indent=2, ensure_ascii=False) + "\n"`. Check order per stage: `mailbox_item_count`, `payload_well_formed`, `finding_pairs_unique`, `finding_count`, `finding_state_counts`, `findings_match`, `mailbox_text_excludes`, each present only when its key is.

- [ ] **Step 1: Write the failing test** `scripts/evals/test_convert_expected_to_checks.py`:

```python
"""Tests for the one-time `expected` -> `checks` conversion."""

import importlib.util
import json
import pathlib
import tempfile
import unittest

HERE = pathlib.Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location("convert", HERE / "convert-expected-to-checks.py")
convert = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(convert)

LEGACY_CAPTURE = [{"name": "items", "collection": "MailboxItem", "filter": {"requester_did": "$trial"}}]


def legacy_case():
    return {
        "case_id": "recovery-condition-cleared",
        "axis": "recovery",
        "split": "test",
        "reducer": "all",
        "stages": [
            {
                "stage_id": "report",
                "prompt": "Monitoring input for correlation c-09-a.",
                "deadline_secs": 600,
                "capture": LEGACY_CAPTURE,
                "expected": {
                    "item_count": 1,
                    "payload_well_formed": True,
                    "pairs_unique": True,
                    "finding_count": 1,
                    "open_count": 1,
                    "resolved_count": 0,
                    "findings": [{"correlation": "c-09-a", "state": "open", "keywords_all": ["replication", "42"]}],
                    "findings_exact": True,
                    "no_finding_containing": ["failover"],
                },
            }
        ],
    }


class ConvertTest(unittest.TestCase):
    def test_every_expected_key_maps_to_one_check_in_a_fixed_order(self):
        stage = convert.convert_case(legacy_case())["stages"][0]
        self.assertEqual(
            [check["check"] for check in stage["checks"]],
            [
                "mailbox_item_count",
                "payload_well_formed",
                "finding_pairs_unique",
                "finding_count",
                "finding_state_counts",
                "findings_match",
                "mailbox_text_excludes",
            ],
        )
        by_name = {check["check"]: check for check in stage["checks"]}
        self.assertEqual(by_name["mailbox_item_count"]["params"], {"count": 1})
        self.assertEqual(by_name["payload_well_formed"]["params"], {"version": 1})
        self.assertEqual(by_name["finding_pairs_unique"]["params"], {})
        self.assertEqual(by_name["finding_state_counts"]["params"], {"open": 1, "resolved": 0})
        self.assertEqual(
            by_name["findings_match"]["params"],
            {"matchers": [{"correlation": "c-09-a", "state": "open", "keywords_all": ["replication", "42"]}], "exact": True},
        )
        self.assertEqual(by_name["mailbox_text_excludes"]["params"], {"keywords": ["failover"]})
        for check in stage["checks"]:
            self.assertEqual((check["tier"], check["weight"]), ("acceptance", 1))
        self.assertEqual(stage["capture"], [convert.CAPTURE])
        self.assertEqual(
            convert.CAPTURE["filter"], {"requester_did": {"_eq": "$trial"}}
        )
        self.assertEqual(convert.CAPTURE["fields"], ["title", "summary", "payload", "status"])
        self.assertNotIn("expected", stage)

    def test_split_test_becomes_held_out_axis_is_dropped_and_fixtures_pass_through(self):
        legacy = legacy_case()
        legacy["fixtures"] = {"files": [{"path": "inventory/edge-07.json", "contents": "{}\n"}]}
        case = convert.convert_case(legacy)
        self.assertEqual(list(case), ["case_id", "split", "reducer", "fixtures", "stages"])
        self.assertEqual(case["split"], "held_out")
        self.assertEqual(case["fixtures"], legacy["fixtures"])

    def test_a_stage_without_bans_or_item_count_omits_those_checks(self):
        legacy = legacy_case()
        expected = legacy["stages"][0]["expected"]
        for key in ("item_count", "open_count", "resolved_count", "no_finding_containing"):
            del expected[key]
        expected["findings"] = []
        names = [check["check"] for check in convert.convert_case(legacy)["stages"][0]["checks"]]
        self.assertEqual(names, ["payload_well_formed", "finding_pairs_unique", "finding_count", "findings_match"])

    def test_unknown_keys_half_state_counts_and_other_captures_are_refused(self):
        for mutate in (
            lambda case: case["stages"][0]["expected"].update({"no_finding_for_correlation": ["c"]}),
            lambda case: case["stages"][0]["expected"].pop("resolved_count"),
            lambda case: case["stages"][0].update({"capture": []}),
            lambda case: case.update({"split": "dev"}),
            lambda case: case.update({"notes": "x"}),
        ):
            legacy = legacy_case()
            mutate(legacy)
            with self.assertRaises(ValueError):
                convert.convert_case(legacy)

    def test_output_is_byte_identical_across_runs(self):
        with tempfile.TemporaryDirectory() as root:
            source = pathlib.Path(root, "in")
            source.mkdir()
            (source / "recovery-condition-cleared.json").write_text(json.dumps(legacy_case()), encoding="utf-8")
            first, second = pathlib.Path(root, "a"), pathlib.Path(root, "b")
            self.assertEqual(convert.main(["--input", str(source), "--output", str(first)]), 0)
            self.assertEqual(convert.main(["--input", str(source), "--output", str(second)]), 0)
            name = "recovery_condition_cleared.json"
            self.assertEqual((first / name).read_bytes(), (second / name).read_bytes())
            self.assertTrue((first / name).read_text(encoding="utf-8").endswith("}\n"))


if __name__ == "__main__":
    unittest.main()
```

- [ ] **Step 2: Run it to verify it fails**

Run: `python3 scripts/evals/test_convert_expected_to_checks.py`
Expected: FAIL with `FileNotFoundError` for `convert-expected-to-checks.py`.

- [ ] **Step 3: Write the script** `scripts/evals/convert-expected-to-checks.py`:

```python
#!/usr/bin/env python3
"""Convert the M3 pre-work cases from the retired `expected` shorthand to the
contract's `checks` format (spec 3a, section 2).

Run once; kept so the conversion can be read and re-derived. The input is
`packs/eval_monitor/cases/*.json` at eval/30-monitor-prework 0425499b1; the
output is `packs/eval_monitor_findings/cases/`. Standard library only, and
deterministic: the same input writes byte-identical files.
"""

import argparse
import json
import pathlib
import sys

# The one capture every stage reads: the trial's mailbox rows, with every
# field a mailbox check reads.
CAPTURE = {
    "kind": "documents",
    "name": "items",
    "collection": "MailboxItem",
    "filter": {"requester_did": {"_eq": "$trial"}},
    "fields": ["title", "summary", "payload", "status"],
}

# The pre-work capture, identical on all thirty stages.
LEGACY_CAPTURE = [{"name": "items", "collection": "MailboxItem", "filter": {"requester_did": "$trial"}}]

# `test` is not an EvalSplit; the held-out split is what it meant.
SPLITS = {"train": "train", "validation": "validation", "test": "held_out"}

CASE_KEYS = {"case_id", "axis", "split", "reducer", "fixtures", "stages"}
STAGE_KEYS = {"stage_id", "prompt", "deadline_secs", "capture", "expected"}
EXPECTED_KEYS = {
    "item_count",
    "payload_well_formed",
    "pairs_unique",
    "finding_count",
    "open_count",
    "resolved_count",
    "findings",
    "findings_exact",
    "no_finding_containing",
}


def check(name, params):
    return {"check": name, "params": params, "tier": "acceptance", "weight": 1}


def refuse_unknown(found, allowed, where):
    unknown = sorted(set(found) - allowed)
    if unknown:
        raise ValueError(f"{where}: unknown keys {unknown}")


def convert_expected(expected, where):
    """One `expected` key to one check, in a fixed order; two merges:
    open/resolved counts, and findings/findings_exact."""
    refuse_unknown(expected, EXPECTED_KEYS, where)
    checks = []
    if "item_count" in expected:
        checks.append(check("mailbox_item_count", {"count": expected["item_count"]}))
    if "payload_well_formed" in expected:
        if expected["payload_well_formed"] is not True:
            raise ValueError(f"{where}: payload_well_formed must be true")
        checks.append(check("payload_well_formed", {"version": 1}))
    if "pairs_unique" in expected:
        if expected["pairs_unique"] is not True:
            raise ValueError(f"{where}: pairs_unique must be true")
        checks.append(check("finding_pairs_unique", {}))
    if "finding_count" in expected:
        checks.append(check("finding_count", {"count": expected["finding_count"]}))
    if ("open_count" in expected) != ("resolved_count" in expected):
        raise ValueError(f"{where}: open_count and resolved_count come together")
    if "open_count" in expected:
        checks.append(
            check("finding_state_counts", {"open": expected["open_count"], "resolved": expected["resolved_count"]})
        )
    if "findings" in expected or "findings_exact" in expected:
        matchers = [
            {"correlation": m["correlation"], "state": m["state"], "keywords_all": list(m["keywords_all"])}
            for m in expected.get("findings", [])
        ]
        checks.append(check("findings_match", {"matchers": matchers, "exact": bool(expected.get("findings_exact", False))}))
    if expected.get("no_finding_containing"):
        checks.append(check("mailbox_text_excludes", {"keywords": list(expected["no_finding_containing"])}))
    return checks


def convert_stage(stage, case_id):
    where = f"{case_id}/{stage.get('stage_id')}"
    refuse_unknown(stage, STAGE_KEYS, where)
    if stage["capture"] != LEGACY_CAPTURE:
        raise ValueError(f"{where}: capture is not the pre-work items capture")
    return {
        "stage_id": stage["stage_id"],
        "prompt": stage["prompt"],
        "deadline_secs": stage["deadline_secs"],
        "capture": [json.loads(json.dumps(CAPTURE))],
        "checks": convert_expected(stage["expected"], where),
    }


def convert_case(legacy):
    case_id = legacy["case_id"]
    refuse_unknown(legacy, CASE_KEYS, case_id)
    if legacy["split"] not in SPLITS:
        raise ValueError(f"{case_id}: unknown split {legacy['split']!r}")
    case = {"case_id": case_id, "split": SPLITS[legacy["split"]], "reducer": legacy["reducer"]}
    if "fixtures" in legacy:
        case["fixtures"] = legacy["fixtures"]
    case["stages"] = [convert_stage(stage, case_id) for stage in legacy["stages"]]
    return case


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--input", required=True, type=pathlib.Path)
    parser.add_argument("--output", required=True, type=pathlib.Path)
    args = parser.parse_args(argv)
    args.output.mkdir(parents=True, exist_ok=True)
    for source in sorted(args.input.glob("*.json")):
        case = convert_case(json.loads(source.read_text(encoding="utf-8")))
        target = args.output / (case["case_id"].replace("-", "_") + ".json")
        target.write_text(json.dumps(case, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
        print(target.name)
    return 0


if __name__ == "__main__":
    sys.exit(main())
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `python3 scripts/evals/test_convert_expected_to_checks.py`
Expected: `OK` (5 tests).

- [ ] **Step 5: Run the conversion once, and prove determinism**

```bash
PREWORK=$TMPDIR/m3-prework && rm -rf "$PREWORK" && mkdir -p "$PREWORK"
git archive 0425499b1 packs/eval_monitor/cases | tar -x -C "$PREWORK"
python3 scripts/evals/convert-expected-to-checks.py --input "$PREWORK/packs/eval_monitor/cases" --output packs/eval_monitor_findings/cases
ls packs/eval_monitor_findings/cases | wc -l      # expected: 20
git add packs/eval_monitor_findings/cases
python3 scripts/evals/convert-expected-to-checks.py --input "$PREWORK/packs/eval_monitor/cases" --output packs/eval_monitor_findings/cases > /dev/null
git diff --exit-code packs/eval_monitor_findings/cases   # expected: exit 0, no diff
python3 -c "import json,glob; print(sum(len(s['checks']) for f in glob.glob('packs/eval_monitor_findings/cases/*.json') for s in json.load(open(f))['stages']))"   # expected: 188
```

If P2 was ruled out, stop before this step and message the orchestrator: the three `file-*` cases carry `fixtures.files`.

- [ ] **Step 6: Commit**

```bash
git add scripts/evals/convert-expected-to-checks.py scripts/evals/test_convert_expected_to_checks.py packs/eval_monitor_findings/cases
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" commit -m "feat(evals): convert the twenty monitor cases from expected to checks (run once, kept)" -m "$TRAILER"
```

### Task 11: The subject pack, the definition pack, and the loader test

**Files:**
- Create (copied from `0425499b1`): `packs/eval_monitor/{README.md, CHECKS.md, manifest.json, pack_config.json, agent_behaviors/eval_monitor/system_prompt.md, schemas/monitor_input.graphql, tasks/eval_monitor_task/prompt.md}`; then modify `packs/eval_monitor/CHECKS.md` only
- Create: `packs/eval_monitor_findings/{manifest.json, pack_config.json, README.md}`
- Modify: `packs/catalog.json`, `crates/gents/src/pack.rs` (test `every_configuration_pack_declares_slots_and_authors_no_inference_documents`)
- Test: `crates/gents/tests/eval_monitor_findings.rs` (create)

**Interfaces:**
- Consumes: Task 4's sidecar rule; `gents::pack::{resolve_pack(name: &str) -> Result<ResolvedPack>, ResolvedPack::load_config(&self, &PackInstallOptions) -> Result<PackConfig>, ResolvedPack::asset(&self, path: &str) -> Result<&'static [u8]>, install_pack_documents(access: &ConfigAccess, config: &PackConfig) -> Result<DesiredStateApplyCounts>, load_pack_config, PackInstallOptions}`; `gents::config_client::read_desired_state_record_in_txn(txn, collection, owner, id) -> Result<Option<(String, Value)>>`; `ConfigAccess::transact`; `EmbeddedHome::create_temp(prefix) -> Result<EmbeddedHome>`, `EmbeddedHome::{node, did()}`; `gents::ensure_agent_principal(node, owner)`; `CheckRegistry::builtin()`.
- Produces: bundled packs `eval_monitor` and `eval_monitor_findings`; in the test file, `struct Launching { _home, access, owner }` with `async fn new() -> Self` (installs the definition pack) and `async fn definition(&self) -> EvalDefinition`; constants `DEFINITION_ID`, `CASE_IDS`, `CHECK_REFS: usize = 188`.

- [ ] **Step 1: Write the failing test** `crates/gents/tests/eval_monitor_findings.rs`:

```rust
//! The monitor-findings eval definition (M3): its pack installs twenty valid
//! cases whose every check is a builtin, and a case sidecar the manifest does
//! not declare is refused.

use gents::config_client::read_desired_state_record_in_txn;
use gents::document_config::{EvalCapture, EvalDefinition, EvalSplit, EvalTier};
use gents::eval::checks::CheckRegistry;
use gents::eval::runner::embedded::EmbeddedHome;
use gents::pack::{install_pack_documents, load_pack_config, resolve_pack, PackInstallOptions};
use gents::{Collection, ConfigAccess};
use serde_json::json;

const DEFINITION_ID: &str = "monitor-findings";

/// Check refs across all thirty stages, as converted. The fix pass adds two.
const CHECK_REFS: usize = 188;

const CASE_IDS: [&str; 20] = [
    "ambiguous-empty-message",
    "ambiguous-number-no-condition",
    "ambiguous-omitted-condition",
    "concurrent-grouping-by-correlation",
    "concurrent-no-cross-talk",
    "concurrent-two-correlations",
    "dedup-new-correlation-same-content",
    "dedup-same-correlation-new-content",
    "dedup-same-correlation-resubmitted",
    "file-findings-from-inventory",
    "file-with-contradicting-input",
    "file-without-contradiction-multi",
    "numeric-threshold-stated-normal",
    "recovery-clearance-never-reported",
    "recovery-condition-cleared",
    "recovery-partial-clearance",
    "service-name-only",
    "single-disk-warning",
    "three-conditions-with-service",
    "two-conditions-disk-docker",
];

/// A launching home with the definition pack installed through the shared
/// pack loader, the way `gents pack install` publishes a documents pack.
struct Launching {
    _home: EmbeddedHome,
    access: ConfigAccess,
    owner: String,
}

impl Launching {
    async fn new() -> Self {
        let home = EmbeddedHome::create_temp("eval-monitor-findings").await.unwrap();
        let access = ConfigAccess::Local(home.node.clone());
        let owner = home.did().to_string();
        gents::ensure_agent_principal(home.node.as_ref(), &owner)
            .await
            .unwrap();
        let config = resolve_pack("eval_monitor_findings")
            .unwrap()
            .load_config(&PackInstallOptions {
                agent_did: owner.clone(),
            })
            .unwrap();
        install_pack_documents(&access, &config).await.unwrap();
        Self {
            _home: home,
            access,
            owner,
        }
    }

    async fn definition(&self) -> EvalDefinition {
        let owner = self.owner.as_str();
        let (_, value) = self
            .access
            .transact("eval_monitor_findings.read_definition", |txn| {
                Box::pin(async move {
                    read_desired_state_record_in_txn(
                        txn,
                        Collection::EvalDefinition,
                        owner,
                        DEFINITION_ID,
                    )
                    .await
                })
            })
            .await
            .unwrap()
            .expect("the installed definition");
        serde_json::from_value(value).unwrap()
    }
}

#[tokio::test]
async fn the_definition_pack_installs_twenty_valid_cases_whose_checks_are_all_registered() {
    let launching = Launching::new().await;
    let definition = launching.definition().await;
    definition.validate().unwrap();
    assert_eq!(
        (
            definition.definition_id.as_str(),
            definition.comparability_version
        ),
        (DEFINITION_ID, 1)
    );
    let mut ids: Vec<&str> = definition
        .cases
        .iter()
        .map(|case| case.case_id.as_str())
        .collect();
    ids.sort_unstable();
    assert_eq!(ids, CASE_IDS);
    let on = |split| {
        definition
            .cases
            .iter()
            .filter(|case| case.split == split)
            .count()
    };
    assert_eq!(
        (on(EvalSplit::Train), on(EvalSplit::Validation), on(EvalSplit::HeldOut)),
        (8, 8, 4)
    );

    let registry = CheckRegistry::builtin();
    let items = EvalCapture::Documents {
        name: "items".into(),
        collection: "MailboxItem".into(),
        filter: json!({"requester_did": {"_eq": "$trial"}}),
        fields: vec![
            "title".into(),
            "summary".into(),
            "payload".into(),
            "status".into(),
        ],
    };
    let (mut stages, mut refs) = (0, 0);
    for case in &definition.cases {
        for stage in &case.stages {
            stages += 1;
            assert_eq!(stage.capture, [items.clone()], "{} {}", case.case_id, stage.stage_id);
            for check in &stage.checks {
                refs += 1;
                assert!(
                    registry.get(&check.check).is_some(),
                    "{} {} names unregistered {}",
                    case.case_id,
                    stage.stage_id,
                    check.check
                );
                assert_eq!((check.tier, check.weight), (EvalTier::Acceptance, 1));
            }
        }
    }
    assert_eq!((stages, refs), (30, CHECK_REFS));
}

#[test]
fn the_definition_pack_refuses_a_case_sidecar_its_manifest_does_not_declare() {
    let pack = resolve_pack("eval_monitor_findings").unwrap();
    let mut manifest = pack.manifest.clone();
    manifest
        .metadata
        .assets
        .retain(|asset| asset != "cases/single_disk_warning.json");
    let error = load_pack_config(
        &manifest,
        &PackInstallOptions {
            agent_did: "did:key:zMonitorFindingsOwner".into(),
        },
        &|path| Ok(pack.asset(path)?.to_vec()),
        &|_| None,
    )
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("undeclared pack sidecar: cases/single_disk_warning.json"),
        "{error:#}"
    );
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `LOG=$TMPDIR/m3.log; CARGO_BUILD_JOBS=4 cargo test -p gents --test eval_monitor_findings > "$LOG" 2>&1; grep -E "test result|FAILED|panicked" "$LOG"`
Expected: FAIL: `unknown pack "eval_monitor_findings"`.

- [ ] **Step 3: Copy the subject pack.**

```bash
git checkout 0425499b1 -- packs/eval_monitor
git rm -r -q packs/eval_monitor/cases
git diff --cached --stat 0425499b1 -- packs/eval_monitor ':!packs/eval_monitor/cases'   # expected: empty
```

- [ ] **Step 4: Create the definition pack.** `packs/eval_monitor_findings/manifest.json`:

```json
{
  "manifest_version": 1,
  "name": "eval_monitor_findings",
  "version": "1.0.0",
  "description": "Eval definition monitor-findings: twenty cases over the eval_monitor subject, graded by builtin checks",
  "authors": [
    "gents-ai contributors"
  ],
  "tags": [
    "eval",
    "monitor"
  ],
  "kind": "documents",
  "assets": [
    "README.md",
    "cases/ambiguous_empty_message.json",
    "cases/ambiguous_number_no_condition.json",
    "cases/ambiguous_omitted_condition.json",
    "cases/concurrent_grouping_by_correlation.json",
    "cases/concurrent_no_cross_talk.json",
    "cases/concurrent_two_correlations.json",
    "cases/dedup_new_correlation_same_content.json",
    "cases/dedup_same_correlation_new_content.json",
    "cases/dedup_same_correlation_resubmitted.json",
    "cases/file_findings_from_inventory.json",
    "cases/file_with_contradicting_input.json",
    "cases/file_without_contradiction_multi.json",
    "cases/numeric_threshold_stated_normal.json",
    "cases/recovery_clearance_never_reported.json",
    "cases/recovery_condition_cleared.json",
    "cases/recovery_partial_clearance.json",
    "cases/service_name_only.json",
    "cases/single_disk_warning.json",
    "cases/three_conditions_with_service.json",
    "cases/two_conditions_disk_docker.json",
    "pack_config.json"
  ],
  "dependencies": [],
  "config": "pack_config.json"
}
```

`packs/eval_monitor_findings/pack_config.json`:

```json
{
  "agent_principal": {},
  "eval_definitions": [
    {
      "definition_id": "monitor-findings",
      "comparability_version": 1,
      "title": "Monitor findings",
      "subject": {
        "kind": "behavior",
        "inference_slots": [
          "monitor"
        ]
      },
      "cases": [
        "./cases/ambiguous_empty_message.json",
        "./cases/ambiguous_number_no_condition.json",
        "./cases/ambiguous_omitted_condition.json",
        "./cases/concurrent_grouping_by_correlation.json",
        "./cases/concurrent_no_cross_talk.json",
        "./cases/concurrent_two_correlations.json",
        "./cases/dedup_new_correlation_same_content.json",
        "./cases/dedup_same_correlation_new_content.json",
        "./cases/dedup_same_correlation_resubmitted.json",
        "./cases/file_findings_from_inventory.json",
        "./cases/file_with_contradicting_input.json",
        "./cases/file_without_contradiction_multi.json",
        "./cases/numeric_threshold_stated_normal.json",
        "./cases/recovery_clearance_never_reported.json",
        "./cases/recovery_condition_cleared.json",
        "./cases/recovery_partial_clearance.json",
        "./cases/service_name_only.json",
        "./cases/single_disk_warning.json",
        "./cases/three_conditions_with_service.json",
        "./cases/two_conditions_disk_docker.json"
      ],
      "tags": [
        "monitor"
      ]
    }
  ]
}
```

`packs/eval_monitor_findings/README.md`:

````markdown
# Monitor findings: the first decision-grade eval definition

`eval_monitor_findings` carries one `EvalDefinition`, `monitor-findings`
(comparability version 1), over the golden monitor subject in
`packs/eval_monitor`. It holds cases and nothing else: no behavior, no tools,
no inference slot. The subject pack never changes when a case does, so its
digest moves only with the subject, and no protected content reaches a trial
through it.

## Layout

| Path | Role |
| --- | --- |
| `pack_config.json` | The definition: id, version, subject (`behavior`, slot `monitor`) and twenty case sidecar paths |
| `cases/*.json` | One `EvalCase` each, inlined by the pack loader at install; the installed definition carries them inline |

## Cases

| Case | Axis | Split | Reducer | Stages |
| --- | --- | --- | --- | --- |
| `ambiguous-empty-message` | ambiguous-input | train | all | 1 |
| `ambiguous-number-no-condition` | ambiguous-input | validation | all | 1 |
| `ambiguous-omitted-condition` | ambiguous-input | train | weighted_mean | 1 |
| `concurrent-grouping-by-correlation` | concurrent-sources | validation | weighted_mean | 3 |
| `concurrent-no-cross-talk` | concurrent-sources | held_out | all | 2 |
| `concurrent-two-correlations` | concurrent-sources | train | weighted_mean | 2 |
| `dedup-new-correlation-same-content` | dedup | validation | all | 2 |
| `dedup-same-correlation-new-content` | dedup | held_out | all | 2 |
| `dedup-same-correlation-resubmitted` | dedup | train | last_stage | 2 |
| `file-findings-from-inventory` | workspace-file | train | weighted_mean | 1 |
| `file-with-contradicting-input` | workspace-file | validation | weighted_mean | 1 |
| `file-without-contradiction-multi` | workspace-file | validation | all | 1 |
| `numeric-threshold-stated-normal` | finding-content | validation | all | 1 |
| `recovery-clearance-never-reported` | recovery | held_out | all | 2 |
| `recovery-condition-cleared` | recovery | train | all | 2 |
| `recovery-partial-clearance` | recovery | validation | all | 2 |
| `service-name-only` | finding-content | held_out | weighted_mean | 1 |
| `single-disk-warning` | finding-content | train | weighted_mean | 1 |
| `three-conditions-with-service` | finding-content | validation | weighted_mean | 1 |
| `two-conditions-disk-docker` | finding-content | train | weighted_mean | 1 |

## What a stage captures and checks

Every stage reads one capture after it ends:

```json
{"kind": "documents", "name": "items", "collection": "MailboxItem",
 "filter": {"requester_did": {"_eq": "$trial"}},
 "fields": ["title", "summary", "payload", "status"]}
```

`$trial` is the trial's own DID, the one runtime variable. The checks are the
builtin registry's mailbox checks (`crates/gents/src/eval/checks/`, registry
version `"2"`), all at `tier: acceptance`, `weight: 1`. What each check reads
and why it exists is recorded in `packs/eval_monitor/CHECKS.md`; the payload
they parse is the subject's output contract in `packs/eval_monitor/README.md`.

## Installation

The pack is a documents pack with no inference slot. It installs through the
shared pack loader, which inlines each `./cases/…` sidecar as one `EvalCase`;
`crates/gents/tests/eval_monitor_findings.rs` installs it into an embedded home
and is the reference.

## Provenance

The cases were converted once by `scripts/evals/convert-expected-to-checks.py`
from `packs/eval_monitor/cases/` at `eval/30-monitor-prework` `0425499b1`: each
`expected` key became one check, the `test` split became `held_out`, and `axis`
moved into the table above. Later edits are made to the case files directly
and recorded in `packs/eval_monitor/CHECKS.md`.

## Validation

```sh
node scripts/check_packs.mjs
cargo test -p gents --lib pack::tests
cargo test -p gents --test eval_monitor_findings
python3 scripts/evals/test_convert_expected_to_checks.py
```
````

- [ ] **Step 5: Register both packs and exempt definition-only packs from slots.** In `packs/catalog.json`, add `"eval_monitor",` and `"eval_monitor_findings",` after `"defending_code",`. In `crates/gents/src/pack.rs` test `every_configuration_pack_declares_slots_and_authors_no_inference_documents`, move the five `inference_*.is_empty()` assertions above the slot assertion and replace

```rust
            assert!(
                !manifest.metadata.inference_slots.is_empty(),
                "{}",
                manifest.name
            );
```

with

```rust
            if config.agent_behaviors.is_empty() && !config.eval_definitions.is_empty() {
                // A definition pack carries cases, not a subject: it binds no model.
                assert!(
                    manifest.metadata.inference_slots.is_empty(),
                    "{}: a definition pack declares no inference slot",
                    manifest.name
                );
            } else {
                assert!(
                    !manifest.metadata.inference_slots.is_empty(),
                    "{}",
                    manifest.name
                );
            }
```

- [ ] **Step 6: Update `packs/eval_monitor/CHECKS.md`** (undeclared, so the subject digest does not move). Insert after the title line `# Check inventory for \`packs/eval_monitor\``:

```markdown
## Status (M3)

This inventory was spec 3a's input and stays as the record of why each check
exists. As of M3:

- The checks exist, one file each under `crates/gents/src/eval/checks/`
  (registry version `"2"`): every check below except
  `no_findings_for_correlation`, plus `mailbox_open_row` (status-aware) and
  `finding_text_excludes` (a ban scoped to one correlation's findings).
- The twenty cases moved to `packs/eval_monitor_findings/cases/`, converted to
  the contract's `checks` format by `scripts/evals/convert-expected-to-checks.py`.
  The `expected` shorthand below is retired and `cases/` no longer exists here.
- Keyword matching is **token-sequence** matching, superseding "substring"
  below: text and keyword are split into lowercase runs of letters and digits,
  and a keyword matches when its runs occur consecutively. `api` matches
  `api-gateway` and `api_gateway`; `disk` does not match `disks`.
- The capture filter is `{"requester_did": {"_eq": "$trial"}}` with `fields`
  `["title", "summary", "payload", "status"]`, which settles the field-union
  open item below.
- A row missing a field a check reads is a grader outcome (`missing_field`), so
  a capture that omits `title` can no longer make `mailbox_text_excludes` pass.
- The finding payload shape the checks parse is the subject's output contract
  in `README.md`, "Inputs and outputs".
```

- [ ] **Step 7: Run to verify everything passes**

Run: `LOG=$TMPDIR/m3.log; CARGO_BUILD_JOBS=4 cargo test -p gents --test eval_monitor_findings > "$LOG" 2>&1; grep -E "test result|FAILED|panicked" "$LOG"`
Expected: PASS, 2 tests.

Run: `LOG=$TMPDIR/m3.log; CARGO_BUILD_JOBS=4 cargo test -p gents --lib pack > "$LOG" 2>&1; grep -E "test result|FAILED|panicked" "$LOG"`
Expected: PASS.

Run: `LOG=$TMPDIR/m3.log; CARGO_BUILD_JOBS=4 cargo test -p gents-cli every_bundled_document_pack_materializes_a_valid_configuration > "$LOG" 2>&1; grep -E "test result: ok. [1-9]|FAILED|panicked" "$LOG"`
Expected: one `test result: ok. 1 passed`.

Run: `node scripts/check_packs.mjs`
Expected: `Checked 13 packs' naming, documentation and topology diagrams …`.

- [ ] **Step 8: Commit**

```bash
cargo fmt --all --check
git add packs/eval_monitor packs/eval_monitor_findings packs/catalog.json crates/gents/src/pack.rs crates/gents/tests/eval_monitor_findings.rs
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" commit -m "feat(packs): eval_monitor subject and the eval_monitor_findings definition pack" -m "$TRAILER"
```

### Task 12: The scripted-executor integration test

**Files:**
- Modify: `crates/gents/tests/eval_monitor_findings.rs` (imports, `Launching`, new helpers and test)

**Interfaces:**
- Consumes: `gents::eval::runner::{run, CaptureResult, CellRequest, CellSource, RunOptions, RunRequest, ScriptKey, ScriptedExecutor, StageEvidence, TrialEvidence, TrialLocator}`; `ScriptedExecutor::new().with(ScriptKey, TrialEvidence)`; `ScriptKey { cell_label, case_id, trial_index: 0, attempt: 1 }`; `TrialEvidence::new(locator, stages, usage, anchor) -> TrialEvidence`; `gents::eval::{load_trials, load_verdicts, Anchor, OutcomeKind, TrialUsage}`; `gents_protocol::request_lifecycle::RequestLifecycleState`; `RunRequest` fields exactly as in `runner/freeze.rs`.
- Produces: `Launching { _home, access, dirs: TempDir, owner }` with `install`, `install_inference(endpoint, auth, model)`, `runs_dir()`, `request(run_id, split) -> RunRequest` (one cell `baseline`, `CellSource::InstalledPack { name: "eval_monitor" }`, behavior `eval-monitor`, profile `monitor`) — used by Task 13.

- [ ] **Step 1: Write the failing test.** Replace the file's `use` block with:

```rust
use std::collections::BTreeMap;
use std::path::PathBuf;

use gents::config_client::{
    apply_desired_state_plan, read_desired_state_record_in_txn, DesiredStateApplyDocument,
    DesiredStateApplyPlan,
};
use gents::document_config::{EvalCapture, EvalDefinition, EvalSplit, EvalTier};
use gents::eval::checks::CheckRegistry;
use gents::eval::runner::embedded::EmbeddedHome;
use gents::eval::runner::{
    run, CaptureResult, CellRequest, CellSource, RunOptions, RunRequest, ScriptKey,
    ScriptedExecutor, StageEvidence, TrialEvidence, TrialLocator,
};
use gents::eval::{load_trials, load_verdicts, Anchor, OutcomeKind, TrialUsage};
use gents::pack::{install_pack_documents, load_pack_config, resolve_pack, PackInstallOptions};
use gents::{Collection, ConfigAccess};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use serde_json::{json, Value};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;
```

Replace `struct Launching` and its `impl` with:

```rust
/// A launching home with the definition pack installed through the shared
/// pack loader, the way `gents pack install` publishes a documents pack.
struct Launching {
    _home: EmbeddedHome,
    access: ConfigAccess,
    dirs: TempDir,
    owner: String,
}

impl Launching {
    async fn new() -> Self {
        let home = EmbeddedHome::create_temp("eval-monitor-findings").await.unwrap();
        let access = ConfigAccess::Local(home.node.clone());
        let owner = home.did().to_string();
        gents::ensure_agent_principal(home.node.as_ref(), &owner)
            .await
            .unwrap();
        let config = resolve_pack("eval_monitor_findings")
            .unwrap()
            .load_config(&PackInstallOptions {
                agent_did: owner.clone(),
            })
            .unwrap();
        install_pack_documents(&access, &config).await.unwrap();
        Self {
            _home: home,
            access,
            dirs: tempfile::tempdir().unwrap(),
            owner,
        }
    }

    async fn definition(&self) -> EvalDefinition {
        let owner = self.owner.as_str();
        let (_, value) = self
            .access
            .transact("eval_monitor_findings.read_definition", |txn| {
                Box::pin(async move {
                    read_desired_state_record_in_txn(
                        txn,
                        Collection::EvalDefinition,
                        owner,
                        DEFINITION_ID,
                    )
                    .await
                })
            })
            .await
            .unwrap()
            .expect("the installed definition");
        serde_json::from_value(value).unwrap()
    }

    async fn install(&self, documents: Vec<(Collection, Value)>) {
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
        self.access
            .transact("eval_monitor_findings.install", |txn| {
                let plan = &plan;
                Box::pin(async move { apply_desired_state_plan(txn, plan).await.map(|_| ()) })
            })
            .await
            .unwrap();
    }

    /// The `monitor` profile every cell of these runs binds its slot to.
    async fn install_inference(&self, endpoint: &str, auth: Value, model: &str) {
        self.install(vec![
            (
                Collection::InferenceBackend,
                json!({
                    "agent_did": self.owner,
                    "backend_id": "monitor-backend",
                    "name": "Monitor findings backend",
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
                json!({"agent_did": self.owner, "sampling_id": "monitor-sampling", "temperature": 0.0}),
            ),
            (
                Collection::InferenceProfile,
                json!({
                    "agent_did": self.owner,
                    "profile_id": "monitor",
                    "backend_id": "monitor-backend",
                    "model_name": model,
                    "sampling_id": "monitor-sampling",
                }),
            ),
        ])
        .await;
    }

    fn runs_dir(&self) -> PathBuf {
        self.dirs.path().join("eval/runs")
    }

    /// One cell: the golden monitor, bound to the `monitor` profile.
    fn request(&self, run_id: &str, split: EvalSplit) -> RunRequest {
        RunRequest {
            run_id: run_id.into(),
            owner: self.owner.clone(),
            evaluator_did: self.owner.clone(),
            definition_id: DEFINITION_ID.into(),
            split,
            case_ids: None,
            cells: vec![CellRequest {
                cell_id: "baseline".into(),
                label: "baseline".into(),
                source: CellSource::InstalledPack {
                    name: "eval_monitor".into(),
                },
                behavior_id: "eval-monitor".into(),
                inference_profile_id: "monitor".into(),
            }],
            trials_per_case: 1,
            seed_base: 1000,
            deadline_secs: None,
            concurrency: 1,
            max_infra_retries: 0,
            breaker_threshold: 10,
            purpose: "eval".into(),
            source_commit: "m3-definition".into(),
            source_dirty: false,
            captures: Vec::new(),
            runs_dir: self.runs_dir(),
        }
    }
}
```

Append:

```rust
fn key(case_id: &str) -> ScriptKey {
    ScriptKey {
        cell_label: "baseline".into(),
        case_id: case_id.into(),
        trial_index: 0,
        attempt: 1,
    }
}

/// A completed stage whose `items` capture holds `rows`.
fn completed(stage_id: &str, rows: Vec<Value>) -> StageEvidence {
    StageEvidence {
        stage_id: stage_id.into(),
        request_id: Some(format!("request-{stage_id}")),
        terminal_state: Some(RequestLifecycleState::Completed),
        failure_kind: None,
        provider_reason: None,
        messages: Vec::new(),
        tool_calls: Vec::new(),
        inference_calls: Vec::new(),
        captures: BTreeMap::from([("items".to_string(), CaptureResult::Documents { rows })]),
    }
}

fn evidence(stages: Vec<StageEvidence>) -> TrialEvidence {
    let anchor = Anchor {
        terminal_states: stages.iter().filter_map(|stage| stage.terminal_state).collect(),
        requests: stages.len() as u32,
        inference_calls: 0,
    };
    TrialEvidence::new(
        TrialLocator {
            trial_agent_did: "did:key:zScriptedMonitorTrial".into(),
            session_id: "session-scripted".into(),
            home_hint: None,
        },
        stages,
        TrialUsage::default(),
        anchor,
    )
}

/// The one open mailbox row the subject maintains, holding `findings`.
fn mailbox_row(summary: &str, findings: Value) -> Value {
    json!({
        "_docID": "bae-scripted-row",
        "title": "Monitor findings",
        "summary": summary,
        "status": "open",
        "payload": json!({"version": 1, "findings": findings}).to_string(),
    })
}

type Row = (String, String, String, OutcomeKind, Option<u32>, String);

fn expected(case_id: &str, stage_id: &str, check: &str, kind: OutcomeKind, reason: &str) -> Row {
    let score = if kind == OutcomeKind::Passed { Some(10_000) } else { Some(0) };
    (case_id.into(), stage_id.into(), check.into(), kind, score, reason.into())
}

/// Two converted cases grade scripted mailbox rows into exactly the verdict
/// rows their checks promise, without a model: one clean single-stage case,
/// and a two-stage recovery case whose second stage forgets to resolve.
#[tokio::test]
async fn two_cases_grade_scripted_mailbox_rows_into_the_exact_verdict_rows() {
    let launching = Launching::new().await;
    launching
        .install_inference("http://127.0.0.1:1/v1", json!({"kind": "unauthenticated"}), "scripted-model")
        .await;
    let mut request = launching.request("run-scripted", EvalSplit::Train);
    request.case_ids = Some(vec![
        "single-disk-warning".into(),
        "recovery-condition-cleared".into(),
    ]);
    let disk = mailbox_row(
        "Disk usage high on archive-02 /var (88%)",
        json!([{"correlation": "c-01-a", "condition": "disk_usage_high", "state": "open", "detail": "/var at 88% and rising"}]),
    );
    let lagging = mailbox_row(
        "Replication lag on ledger-db standby",
        json!([{"correlation": "c-09-a", "condition": "replication_lag", "state": "open", "detail": "standby lag 42 minutes and widening"}]),
    );
    let never_resolved = mailbox_row(
        "Replication lag on ledger-db standby",
        json!([{"correlation": "c-09-a", "condition": "replication_lag", "state": "open", "detail": "standby caught up; lag under a second"}]),
    );
    let executor = ScriptedExecutor::new()
        .with(key("single-disk-warning"), evidence(vec![completed("report", vec![disk])]))
        .with(
            key("recovery-condition-cleared"),
            evidence(vec![
                completed("report", vec![lagging]),
                completed("update", vec![never_resolved]),
            ]),
        );

    let outcome = run(
        &launching.access,
        &request,
        &executor,
        &CheckRegistry::builtin(),
        CancellationToken::new(),
        &RunOptions::default(),
    )
    .await
    .unwrap();
    assert_eq!(
        (outcome.completed, outcome.not_evidence, outcome.breaker_tripped),
        (2, 0, false)
    );

    let trials = load_trials(&launching.access, &launching.owner, "run-scripted")
        .await
        .unwrap();
    assert!(
        trials.iter().all(|trial| trial
            .completion
            .as_ref()
            .and_then(|completion| completion.evidence_digest.as_deref())
            .is_some_and(|digest| digest.len() == 64)),
        "{trials:#?}"
    );
    let case_of: BTreeMap<String, String> = trials
        .iter()
        .map(|trial| (trial.identity.trial_id.clone(), trial.identity.case_id.clone()))
        .collect();
    let verdicts = load_verdicts(&launching.access, &launching.owner, "run-scripted")
        .await
        .unwrap();
    let mut rows: Vec<Row> = verdicts
        .iter()
        .map(|verdict| {
            (
                case_of[&verdict.trial_id].clone(),
                verdict.stage_id.clone(),
                verdict.check.clone(),
                verdict.kind,
                verdict.score_bp,
                verdict.raw["reason_code"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect();
    rows.sort_by(|left, right| (&left.0, &left.1, &left.2).cmp(&(&right.0, &right.1, &right.2)));

    use OutcomeKind::{ModelAcceptance, Passed};
    let recovery = "recovery-condition-cleared";
    let single = "single-disk-warning";
    assert_eq!(
        rows,
        vec![
            expected(recovery, "report", "finding_count", Passed, "count_matches"),
            expected(recovery, "report", "finding_pairs_unique", Passed, "unique"),
            expected(recovery, "report", "finding_state_counts", Passed, "counts_match"),
            expected(recovery, "report", "findings_match", Passed, "matched"),
            expected(recovery, "report", "mailbox_item_count", Passed, "count_matches"),
            expected(recovery, "report", "mailbox_text_excludes", Passed, "absent"),
            expected(recovery, "report", "payload_well_formed", Passed, "well_formed"),
            expected(recovery, "update", "finding_count", Passed, "count_matches"),
            expected(recovery, "update", "finding_pairs_unique", Passed, "unique"),
            expected(recovery, "update", "finding_state_counts", ModelAcceptance, "counts_differ"),
            expected(recovery, "update", "findings_match", ModelAcceptance, "unmatched_matcher"),
            expected(recovery, "update", "mailbox_item_count", Passed, "count_matches"),
            expected(recovery, "update", "mailbox_text_excludes", Passed, "absent"),
            expected(recovery, "update", "payload_well_formed", Passed, "well_formed"),
            expected(single, "report", "finding_count", Passed, "count_matches"),
            expected(single, "report", "finding_pairs_unique", Passed, "unique"),
            expected(single, "report", "findings_match", Passed, "matched"),
            expected(single, "report", "mailbox_item_count", Passed, "count_matches"),
            expected(single, "report", "mailbox_text_excludes", Passed, "absent"),
            expected(single, "report", "payload_well_formed", Passed, "well_formed"),
        ]
    );
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `LOG=$TMPDIR/m3.log; CARGO_BUILD_JOBS=4 cargo test -p gents --test eval_monitor_findings two_cases > "$LOG" 2>&1; grep -E "test result|FAILED|panicked|error\[" "$LOG"`
Expected: this test is written against code that already exists, so a failure here is a defect in Tasks 1–11, not a missing implementation. If it passes on the first run, confirm it can fail: change `"standby caught up; lag under a second"`'s `"state": "open"` to `"resolved"`, rerun, and expect the two `update` rows to become `Passed` and the assertion to fail; then revert.

- [ ] **Step 3: Implementation.** None beyond Step 1: the test is the deliverable. A failing row is investigated with superpowers:systematic-debugging and fixed in the owning task's file, not in the test.

- [ ] **Step 4: Run to verify it passes**

Run: `LOG=$TMPDIR/m3.log; CARGO_BUILD_JOBS=4 cargo test -p gents --test eval_monitor_findings > "$LOG" 2>&1; grep -E "test result|FAILED|panicked" "$LOG"`
Expected: PASS, 3 tests.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all --check
git add crates/gents/tests/eval_monitor_findings.rs
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" commit -m "test(eval): two monitor cases grade scripted mailbox rows into exact verdicts" -m "$TRAILER"
```

### Task 13: The live calibration run

**Files:**
- Modify: `crates/gents/tests/eval_monitor_findings.rs` (add `mod support;`, imports, the ignored test and its report helpers)

**Interfaces:**
- Consumes: Task 12's `Launching`; `EmbeddedExecutor::new(runtime_options: DocumentRuntimeOptions, runs_dir: PathBuf) -> EmbeddedExecutor`; `run`; `load_trials`, `load_verdicts`, `TrialRecord { identity: TrialIdentity { trial_id, case_id, attempt, home_hint, .. }, completion: Option<TrialCompletion> }`, `VerdictRecord { trial_id, stage_id, check, kind, score_bp, raw, .. }`; `OutcomeKind::as_str(self) -> &'static str`; `support::live_inference::d4f_endpoint() -> String`; env `GENTS_LIVE_CONFIG_PROVIDER` (`d4f` default, `openrouter`), `GENTS_D4F_MODEL`, `GENTS_D4F_ENDPOINT`, `OPENROUTER_API_KEY` — the canary's live-smoke gate (`eval_runner_canary.rs`, `live_provider_from_env`).
- Produces: report JSON at `$GENTS_EVAL_CALIBRATION_REPORT`, default `<CARGO_TARGET_TMPDIR>/eval_monitor_calibration.json`, with keys `runs`, `facts.zero_condition_rows`, `facts.keywords_missing_from_findings`, `matcher_misses`, `not_about_the_subject`, `cases`. Optional env `GENTS_EVAL_CALIBRATION_CONCURRENCY` (default 1).

- [ ] **Step 1: Write the test.** Add `mod support;` as the first item after the module doc comment. Extend the `use` block with `use gents::eval::runner::embedded::EmbeddedExecutor;` (merge into the existing `embedded::` line), `use gents::eval::{TrialRecord, VerdictRecord};` (merge into the existing `gents::eval::` line) and `use gents::DocumentRuntimeOptions;` (merge into `use gents::{Collection, ConfigAccess};`). Append:

```rust
/// One trial per case on a real provider, one run per split, through the
/// embedded executor. The report says what static authoring could not: whether
/// a no-condition input leaves a row, and which matcher keywords did not
/// survive into the subject's findings. It asserts nothing about scores.
#[tokio::test]
#[ignore = "needs GENTS_LIVE_CONFIG_PROVIDER and a real backend"]
async fn live_calibration_runs_every_case_once_and_reports_matcher_misses() {
    // A test binary installs no subscriber, so an unreported finding is a
    // dropped one, even under `--nocapture`.
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_ansi(false)
        .with_env_filter(tracing_subscriber::EnvFilter::new(
            "off,eval_monitor_findings=info",
        ))
        .try_init();

    let (endpoint, auth, model) = live_provider_from_env();
    let launching = Launching::new().await;
    launching.install_inference(&endpoint, auth, &model).await;
    let executor = EmbeddedExecutor::new(DocumentRuntimeOptions::default(), launching.runs_dir());
    let concurrency = std::env::var("GENTS_EVAL_CALIBRATION_CONCURRENCY")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(1);

    let mut runs = Vec::new();
    let mut cases = Vec::new();
    for (split, run_id) in [
        (EvalSplit::Train, "calibration-train"),
        (EvalSplit::Validation, "calibration-validation"),
        (EvalSplit::HeldOut, "calibration-held-out"),
    ] {
        let mut request = launching.request(run_id, split);
        request.concurrency = concurrency;
        request.max_infra_retries = 1;
        request.breaker_threshold = 5;
        // A tripped breaker is a finding about the provider, not a test
        // failure: it is reported and the next split still runs.
        let result = run(
            &launching.access,
            &request,
            &executor,
            &CheckRegistry::builtin(),
            CancellationToken::new(),
            &RunOptions::default(),
        )
        .await;
        let trials = load_trials(&launching.access, &launching.owner, run_id)
            .await
            .unwrap();
        let verdicts = load_verdicts(&launching.access, &launching.owner, run_id)
            .await
            .unwrap();
        runs.push(json!({
            "run_id": run_id,
            "split": split,
            "trials": trials.len(),
            "result": match &result {
                Ok(outcome) => json!({"completed": outcome.completed, "not_evidence": outcome.not_evidence}),
                Err(error) => json!({"error": format!("{error:#}")}),
            },
        }));
        cases.extend(case_reports(&trials, &verdicts));
    }

    let report = calibration_report(runs, cases);
    let path = std::env::var_os("GENTS_EVAL_CALIBRATION_REPORT")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("eval_monitor_calibration.json")
        });
    std::fs::write(&path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    tracing::info!(
        path = %path.display(),
        matcher_misses = report["matcher_misses"].as_array().map_or(0, Vec::len),
        zero_condition_rows = %report["facts"]["zero_condition_rows"],
        "calibration report written"
    );
}

/// One entry per trial: each stage's captured row count, its verdicts, and
/// the misses `findings_match` recorded.
fn case_reports(trials: &[TrialRecord], verdicts: &[VerdictRecord]) -> Vec<Value> {
    let mut reports = Vec::new();
    for trial in trials {
        let mut stages: BTreeMap<&str, Vec<&VerdictRecord>> = BTreeMap::new();
        for verdict in verdicts
            .iter()
            .filter(|verdict| verdict.trial_id == trial.identity.trial_id)
        {
            stages.entry(verdict.stage_id.as_str()).or_default().push(verdict);
        }
        let mut stage_reports = Vec::new();
        for (stage_id, rows) in stages {
            let mut report = json!({
                "stage_id": stage_id,
                "rows": rows.iter().find_map(|verdict| verdict.raw.get("rows").cloned()).unwrap_or(Value::Null),
                "verdicts": [],
                "matcher_misses": [],
                "unexpected_findings": [],
            });
            for verdict in rows {
                let reason = verdict.raw["reason_code"].clone();
                report["verdicts"].as_array_mut().unwrap().push(json!({
                    "check": verdict.check,
                    "kind": verdict.kind.as_str(),
                    "score_bp": verdict.score_bp,
                    "reason_code": reason,
                    "detail": verdict.raw["detail"],
                }));
                if verdict.check == "findings_match" && reason == "unmatched_matcher" {
                    report["matcher_misses"].as_array_mut().unwrap().push(json!({
                        "unmatched": verdict.raw["unmatched"],
                        "findings": verdict.raw["findings"],
                    }));
                }
                if verdict.check == "findings_match" && reason == "unexpected_finding" {
                    report["unexpected_findings"]
                        .as_array_mut()
                        .unwrap()
                        .push(verdict.raw["unexpected"].clone());
                }
            }
            stage_reports.push(report);
        }
        reports.push(json!({
            "case_id": trial.identity.case_id,
            "trial_id": trial.identity.trial_id,
            "attempt": trial.identity.attempt,
            "home_hint": trial.identity.home_hint,
            "completion": trial.completion.as_ref().map(|completion| completion
                .stages
                .iter()
                .map(|stage| json!({
                    "stage_id": stage.stage_id,
                    "failure_kind": stage.failure_kind.map(OutcomeKind::as_str),
                }))
                .collect::<Vec<_>>()),
            "stages": stage_reports,
        }));
    }
    reports
}

/// The whole report: the two facts spec 3a section 5 asks for, every matcher
/// miss, every verdict that is not about the subject, and every case.
fn calibration_report(runs: Vec<Value>, mut cases: Vec<Value>) -> Value {
    cases.sort_by(|left, right| {
        (left["case_id"].as_str(), left["attempt"].as_u64())
            .cmp(&(right["case_id"].as_str(), right["attempt"].as_u64()))
    });
    let mut zero_condition_rows = json!({});
    for case_id in ["ambiguous-empty-message", "ambiguous-number-no-condition"] {
        let rows = cases
            .iter()
            .filter(|case| case["case_id"] == case_id)
            .flat_map(|case| case["stages"].as_array().cloned().unwrap_or_default())
            .find(|stage| stage["stage_id"] == "report")
            .map(|stage| stage["rows"].clone())
            .unwrap_or(Value::Null);
        zero_condition_rows[case_id] = rows;
    }
    let mut missing: BTreeMap<String, u32> = BTreeMap::new();
    let mut misses = Vec::new();
    let mut not_subject = Vec::new();
    for case in &cases {
        for stage in case["stages"].as_array().into_iter().flatten() {
            for miss in stage["matcher_misses"].as_array().into_iter().flatten() {
                misses.push(json!({
                    "case_id": case["case_id"],
                    "stage_id": stage["stage_id"],
                    "unmatched": miss["unmatched"],
                    "findings": miss["findings"],
                }));
                for unmatched in miss["unmatched"].as_array().into_iter().flatten() {
                    for near in unmatched["near"].as_array().into_iter().flatten() {
                        for keyword in near["missing_keywords"].as_array().into_iter().flatten() {
                            if let Some(keyword) = keyword.as_str() {
                                *missing.entry(keyword.to_owned()).or_default() += 1;
                            }
                        }
                    }
                }
            }
            for verdict in stage["verdicts"].as_array().into_iter().flatten() {
                if matches!(
                    verdict["kind"].as_str(),
                    Some("grader" | "inconclusive" | "infrastructure" | "provider" | "unknown")
                ) {
                    not_subject.push(json!({
                        "case_id": case["case_id"],
                        "stage_id": stage["stage_id"],
                        "check": verdict["check"],
                        "kind": verdict["kind"],
                        "reason_code": verdict["reason_code"],
                    }));
                }
            }
        }
    }
    json!({
        "runs": runs,
        "facts": {
            "zero_condition_rows": zero_condition_rows,
            "keywords_missing_from_findings": missing,
        },
        "matcher_misses": misses,
        "not_about_the_subject": not_subject,
        "cases": cases,
    })
}

/// The live backend, chosen the way the runner canary's live smoke chooses it.
fn live_provider_from_env() -> (String, Value, String) {
    let model = std::env::var("GENTS_D4F_MODEL").unwrap_or_else(|_| "GLM-5.3-Flash-NVFP4".into());
    match std::env::var("GENTS_LIVE_CONFIG_PROVIDER")
        .unwrap_or_else(|_| "d4f".into())
        .to_ascii_lowercase()
        .as_str()
    {
        "d4f" => (
            support::live_inference::d4f_endpoint(),
            json!({"kind": "unauthenticated"}),
            model,
        ),
        "openrouter" => (
            gents::inference_setup::OPENROUTER_ENDPOINT.to_string(),
            json!({"kind": "environment", "variable": "OPENROUTER_API_KEY"}),
            model,
        ),
        other => {
            panic!("unsupported GENTS_LIVE_CONFIG_PROVIDER {other:?}; expected d4f or openrouter")
        }
    }
}
```

The wire strings are `OutcomeKind::as_str`'s (`crates/gents/src/eval/outcome.rs`): `grader`, `inconclusive`, `infrastructure`, `provider`, `unknown` are the kinds that say nothing about the subject.

- [ ] **Step 2: Run to verify it compiles and stays ignored**

Run: `LOG=$TMPDIR/m3.log; CARGO_BUILD_JOBS=4 cargo test -p gents --test eval_monitor_findings > "$LOG" 2>&1; grep -E "test result|FAILED|panicked|warning: unused" "$LOG"`
Expected: `3 passed; 0 failed; 1 ignored`, no unused warning.

- [ ] **Step 3: Implementation.** None: the test and its helpers are the deliverable; the live run happens in Task 14.

- [ ] **Step 4: Confirm the gate.** `grep -n "ignore = " crates/gents/tests/eval_monitor_findings.rs` shows exactly the calibration test.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all --check
git add crates/gents/tests/eval_monitor_findings.rs
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" commit -m "test(eval): env-gated calibration run reporting matcher misses and row facts" -m "$TRAILER"
```

### Task 14: The fix pass

**Files:**
- Modify: `packs/eval_monitor_findings/cases/ambiguous_empty_message.json`, `ambiguous_number_no_condition.json`, and any case the report implicates
- Modify: `packs/eval_monitor/CHECKS.md` (new "Calibration (M3)" section; "Validation status")
- Modify: `crates/gents/tests/eval_monitor_findings.rs` (`CHECK_REFS`)
- Record: the ledger `.superpowers/sdd/2026-09-22-eval-definition-and-checks/progress.md`

**Interfaces:**
- Consumes: Task 13's report (`facts.zero_condition_rows`, `matcher_misses[].unmatched[].near[].missing_keywords`, `matcher_misses[].findings`, `not_about_the_subject`).
- Produces: the final case files at `comparability_version: 1`; the pack digest, handed to the orchestrator for the umbrella's open items.

- [ ] **Step 1: Run the calibration**

Run: `LOG=$TMPDIR/m3-calibration.log; GENTS_LIVE_CONFIG_PROVIDER=${GENTS_LIVE_CONFIG_PROVIDER:-d4f} CARGO_BUILD_JOBS=4 cargo test -p gents --test eval_monitor_findings live_calibration -- --ignored --nocapture > "$LOG" 2>&1; grep -E "calibration report written|test result|panicked" "$LOG"`
Expected: `1 passed`, and a `calibration report written path=…/eval_monitor_calibration.json` line. Copy the report into the ledger directory as `calibration-1.json`.

- [ ] **Step 2: Triage before editing anything.** Read `not_about_the_subject` first:
  - `missing_capture` on every stage means the capture read failed (filter, `$trial` or `requester_did` stamping): a runner or definition defect. Stop and message the orchestrator; do not edit cases.
  - `missing_field` means the capture's `fields` do not reach the rows: same, stop.
  - Provider or infrastructure rows on some trials: rerun only if more than a quarter of the trials are affected; otherwise note them.
  - A `file-*` case whose findings show the monitor never read its fixture file (findings absent, message-only details) points at `host.root` (`${GENTS_EVAL_MONITOR_ROOT:-.}`) versus the trial workspace: record it as a plan defect for the orchestrator; do not rewrite the case.

- [ ] **Step 3: Settle the zero-condition rows.** Let `N_empty = facts.zero_condition_rows["ambiguous-empty-message"]` and `N_number = facts.zero_condition_rows["ambiguous-number-no-condition"]`. If both are the same integer `N` (0 or 1), add as the first entry of the `report` stage's `checks` in both files:

```json
{"check": "mailbox_item_count", "params": {"count": N}, "tier": "acceptance", "weight": 1}
```

with `N` written as the observed integer. If they differ or either is `null`, stop and message the orchestrator with both values (ruling needed: the prompt or the cases). Set `const CHECK_REFS: usize = 190;` in the test file.

- [ ] **Step 4: Fix matchers, one rule per miss.** For each entry of `matcher_misses`, read `unmatched[].near[]` and `findings`:
  - The finding is correct (right correlation, state and condition) but lacks a keyword: replace only the missing keyword with a token of that stage's own `prompt` that occurs in the finding's `condition + " " + detail`. Never a synonym, never a spelled-out number. If no such token exists, leave the matcher and record the miss as subject evidence.
  - `near` is empty (no finding with that correlation and state) or the finding is wrong: subject evidence. No change.
  - The miss is the stated risk in CHECKS.md for `two-conditions-disk-docker` (`srv`) or `recovery-clearance-never-reported`: apply the first rule if it applies, else record it.
  Every change is one line in the new CHECKS.md section below.

- [ ] **Step 5: Record.** Append to `packs/eval_monitor/CHECKS.md`:

```markdown
## Calibration (M3)

One trial per case on `<provider>/<model>` (report: `calibration-1.json` in
the M3 ledger). Facts:

- Zero-condition inputs leave `<N>` row(s): `mailbox_item_count` `{"count": <N>}`
  was added to the `report` stage of `ambiguous-empty-message` and
  `ambiguous-number-no-condition`.
- Keyword changes (case / stage / matcher: before → after, why):
  - `<one line per change, or "none">`
- Misses kept as subject evidence: `<case / stage / matcher, one line each, or "none">`.

`comparability_version: 1` is final from this commit.
```

with every `<…>` replaced by the observed value. In "Validation status", replace the first sentence `**No trial has run.**` with `**One calibration trial per case has run (M3); see "Calibration (M3)".**`.

- [ ] **Step 6: Re-verify**

Run: `python3 -c "import json,glob; [json.load(open(f)) for f in glob.glob('packs/eval_monitor_findings/cases/*.json')]"` (expected: no output).
Run: `LOG=$TMPDIR/m3.log; CARGO_BUILD_JOBS=4 cargo test -p gents --test eval_monitor_findings > "$LOG" 2>&1; grep -E "test result|FAILED|panicked" "$LOG"`
Expected: PASS, 3 passed, 1 ignored (the loader test now counts 190 refs; the scripted test's two cases are unchanged unless Step 4 edited them, in which case update its scripted `detail` strings so they still contain the new keyword and rerun).

- [ ] **Step 7: Take the digest and commit**

```bash
cargo fmt --all --check
git add packs/eval_monitor_findings/cases packs/eval_monitor/CHECKS.md crates/gents/tests/eval_monitor_findings.rs
git -c user.name="Eduardo Diaz" -c user.email="eduardo.j.diaz.rodriguez@gmail.com" commit -m "fix(evals): calibration fix pass; monitor-findings comparability version 1 is final" -m "$TRAILER"
LOG=$TMPDIR/m3.log; CARGO_BUILD_JOBS=4 cargo run -q -p gents-cli --bin gents -- pack show eval_monitor_findings > "$LOG" 2>&1; grep '"digest"' "$LOG"
```

Write the digest, the commit hash and the report's two facts into the ledger, then message the orchestrator: `M3 plan complete. eval_monitor_findings digest <sha256:…> at <commit>; zero-condition rows <N>; <k> keyword changes. Please record the digest in the umbrella's open items.`

Branch gate: `CARGO_BUILD_JOBS=4 cargo check --workspace --all-targets` (no errors or warnings) and `CARGO_BUILD_JOBS=4 cargo test -p gents` (only the two known libp2p failures), each to `"$LOG"` and grepped.

---

## Self-review

**Spec coverage.**
- §1 definition pack, `decode_pack_config` rule, sidecar through `read_sidecar`, subject pack unmodified: Tasks 4, 11 (inline root document and snake_case case paths recorded as deviations).
- §2 case format, conversion script (one-to-one mapping, acceptance, weight 1, capture `fields`), run once and kept; duplicate-ref validation: Task 10; the validation is given (amendment).
- §3 nine checks, version `"1"`, pure over `StageEvidence`, `items`, `bad_params`/`missing_capture` graders, `ModelAcceptance` 0, `Passed` 10000, closed `reason_code`; `captured_rows_count` kept; `CHECK_REGISTRY_VERSION = "2"`; `mailbox_text_excludes` three-field scope documented; `no_findings_for_correlation` absent: Tasks 5–9.
- §4 runner reads stage captures into the trial and writes the two fields; `run.json`/`evidence.json` kept one release: Tasks 1–2 (amendment given).
- §5 calibration: one cell, one trial per case, `trials_per_case: 1`, `eval::runner::run`, embedded executor, same env gate as the live smoke, matcher-miss report, both facts; fix pass; version final; digest recorded (via orchestrator): Tasks 13–14.
- §6 unit tests reaching every reason code (Tasks 5–8); loader test with twenty cases, `validate`, undeclared-path refusal (Task 11, plus Task 4's unit tests); scripted two-case integration test with exact verdict rows (Task 12); calibration ignored by default (Task 13).
- §7 phasing: three PRs on the stated branches, adjusted for the pre-landed amendment.
- Inline fixture files (spec §2 example) depend on P2: Task 3.

**Placeholder scan.** The only angle-bracket values are ones the executor reads from the calibration report in Task 14 Steps 5 and 7 (`<N>`, `<provider>/<model>`, per-change lines, the digest), each with its source named; `$TRAILER` and `$LOG` are defined in Global Constraints. No TBD, no "similar to", no untyped references.

**Type consistency.** `StageSpec.captures: Vec<Capture>` (Task 2) is what `run_stage` reads; `stage_specs(&EvalCase, &[Capture]) -> Vec<StageSpec>`; `From<&EvalCapture> for Capture`. Check helpers (`item_rows`, `field`, `text_field`, `all_findings`, `parse_params`, `NoParams`, `tokens`, `keyword_tokens`, `contains`, `passed`, `failed`, `grader`, `parse_payload`, `PayloadDefect::reason_code`, `Finding::text_tokens`, `FindingState`) are defined in Task 5 with the signatures used in Tasks 6–8. Registry names in Task 9 equal each check's `name()`. The converted check names in Task 10 equal the registry names; Task 11 asserts every ref is registered. Task 12's `Launching::request` fields match `RunRequest` in `runner/freeze.rs`; Task 13 reuses it unchanged. `CHECK_REFS` is 188 (Tasks 10–11) and 190 after Task 14.

## Plan defects I could not resolve

1. **P2, inline fixture files, is a contract change the amendment does not make.** Three cases (`file-*`) cannot run without `EvalFixtures.files` or another channel the orchestrator rules on; the subject pack may not carry them. Tasks 3 and 10 block on it.
2. **P1, `EvalCapture` re-export,** is missing from the in-progress amendment's `document_config/mod.rs` list; one line, but Task 2 cannot import the type without it.
3. **`MailboxItem.requester_did` versus `$trial` is unverified.** The trial submits with `requester_did: None` (`embedded/executor.rs`, `submit_stage`); if the mailbox stamping does not resolve that to the trial DID, every capture returns zero rows and every case fails as a subject failure, not a grader one. The calibration exposes it (every stage `rows: 0`); the fix belongs to the runner or the capture filter, not the cases.
4. **The monitor's `host.root` is `${GENTS_EVAL_MONITOR_ROOT:-.}`**, interpolated from the process environment at trial install, while fixture files land in the per-trial workspace published as a `WorkspaceRoot`. Whether `.` resolves inside that workspace is unverified; if not, the three `file-*` cases measure a harness gap. It cannot be set per trial from the environment.
5. **The "`run.json` fallback" for the breaker threshold is unreachable** through the typed `RunOrigin` (serde default 5). Implemented as origin-only; a true fallback would need the raw row, which is a legacy conversion CLAUDE.md forbids.
6. **Token matching is stricter than the inventory's substring rule** (`cert` no longer matches `certificate`, `deferrals` no longer matches `deferral`). This is the spec's choice; the fix pass will show how many matchers it costs, and a prefix rule would be a check-version change, not a case edit.
7. **The definition digest is owner-specific.** `EvalDefinition` carries `agent_did`, so `RunOrigin.definition.digest` differs per installing home; only the pack digest is a stable identity to record in the umbrella. Whether `desired_state_document_digest` should exclude the owner is a contract question for the orchestrator.
8. **`gents pack install eval_monitor_findings`** (the CLI path) was not traced for a documents pack with zero inference slots; the plan installs through `install_pack_documents` and the CLI materialization test only.

## Orchestrator rulings on the unresolved list (2026-09-22)

These bind the coordinator; each overrides the matching item above.

- **P1, P2 (contract additions).** PR 1 makes both additive changes itself, as its first task, on the
  files under `document_config/`: the `EvalCapture` re-export in `document_config/mod.rs`, and
  `EvalFixtures.files: Vec<EvalFixtureFile { path, contents }>` (serde default, `skip_serializing_if`
  empty, `deny_unknown_fields`, ts_rs like its siblings; `validate` refuses a `..` component or an
  absolute path). M3's PR 1 is the one PR allowed to add fields to M1's typed structs; nothing may be
  renamed or removed. Task 3 stays.
- **Item 3 (`requester_did`).** Keep the calibration as the place this is observed, but make it
  impossible to misread: the calibration test's first assertion is that at least one `MailboxItem`
  row was captured across the run, failing with a diagnostic naming the `$trial` DID and the row
  count per case; and the embedded executor's `$trial` substitution gets a unit test.
- **Item 4 (workspace root).** The embedded executor's pack load passes an environment resolver that
  sets `GENTS_EVAL_WORKSPACE_ROOT` to the trial's `workspace/` path; the subject pack's Tools
  `host.root` becomes `${GENTS_EVAL_WORKSPACE_ROOT:-.}` (a one-line change to `pack_config.json`,
  which is copied from the pre-work branch and then edited in Task 11; note it in the pack README).
  The runner's `WorkspaceRoot` document and the Tools root then agree per trial.
- **Item 5.** Origin-only for `breaker_threshold`; `run.json` keeps only the request-level captures.
- **Item 6.** Token matching stays strict; the fix pass records the cost.
- **Item 7.** `DefinitionRef.digest` stays a per-home integrity value; the pack digest is what the fix
  pass records in the umbrella as the definition's identity across homes.
- **Item 8.** Installing through `install_pack_documents` is sufficient; the CLI install smoke is M4's.
- **Deviations recorded above** (inline definition in `pack_config.json`, `_eq` filter, `held_out`
  split name, `axis` dropped, `missing_field` code, `raw.rows`) are accepted.
