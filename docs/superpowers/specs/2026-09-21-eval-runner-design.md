# Eval runner design (issue #1515, sub-project 2a)

Status: **DRAFT, brainstorm in progress (2026-09-21).** Section 1 is presented and awaits approval.
Sections 2 to 4 are not yet written. Baseline: `main` at `0deb7659c`.
Umbrella: `2026-09-21-eval-and-optimization-umbrella.md`. Contract: `2026-09-21-eval-core-contract-design.md`.

## Decisions taken so far

Fixed by the umbrella and not reopened: an injectable `TrialExecutor`; embedded in-process homes first
(one `gents serve` per trial is spec 2b); one fresh home per case-trial; checks run in the runner under
an evaluator DID; run-level documents live in the launching home; resume by `attempt`.

Decided in this brainstorm:

| Question | Decision | Why |
|---|---|---|
| Who hosts a run while it executes | A library call in the caller's process. `gents eval run`, `cargo test` and the optimization driver each call `eval::runner::run(...)`. If the process dies, the documents left behind are the whole state and `--resume <run_id>` continues | No daemon, no claim rule, no new lifecycle. It matches the boundary "eval documents have no lifecycle" |
| NotEvidence trials | A per-trial attempt cap (default 3, with backoff) plus a run-level breaker: K consecutive NotEvidence trials (default 5) stop the launch, leave the run resumable and return a typed provider-down error | A provider outage must not burn every trial's retries, and must not be read as a subject failure |
| Where the `TrialExecutor` seam sits | Wide: `execute(spec) -> TrialEvidence`, plus `recollect(locator)` for regrades. Evidence is plain data | Checks become pure functions over evidence, which spec 3 assumes. The scripted executor is a lookup table. Spec 2b becomes a third executor with no runner change |
| How evidence gets the documents a subject produced | Definition-declared capture: a case declares `capture: [{name, collection, filter}]`, held runner-side and never sent into the trial. Session records are always captured | Evidence stays small, check inputs are explicit, and scripted evidence is easy to write. The field is additive to the case shape and lands with M3 |
| How a trial home reaches a model | The runner copies the cell's frozen inference binding into the trial home. Credentials resolve from the runner's process environment, as #1512 does. OAuth-subscription backends are refused at freeze time | It is the smallest change and already proven. An `OAuthCredential` is scoped to one agent DID, and N homes would race on refresh. A runner-hosted inference proxy is a 2b candidate |

Facts from the harness scout that shape the design:

- Embedded homes coexist in one process, and #1512 already fans trials out with `buffer_unordered`.
  Concurrency is a bounded `--concurrency`, default 1.
- The reusable bootstrap lives under `tests/` (`test_db_in`, `retained_trial_db`, `observe_request`,
  `retain_request_evidence`). Moving its non-test core into `src/` is the main code cost of M2.
- `MockStreamingBackend` is a real local HTTP double, injected by pointing an `InferenceBackend`
  document at it. It gives deterministic model output beneath a real runtime.
- Waiting is polling everywhere. The existing timeout path interrupts the request and allows 30
  seconds of grace.
- Usage is token counts on `InferenceCall`. No dollar field exists, so the optimization cost gate sums
  tokens.
- `EvalRun` and `EvalTrial` replicate to paired desktops by design. `EvalVerdict` does not. So
  `EvalRun.origin` and `EvalTrial.completion` must never carry free text such as a provider error
  string or a transcript excerpt. They carry ids, enum strings and counts only. Detail belongs in
  `EvalVerdict.raw` or stays in the trial home.

## Section 1: shape and the seam (awaiting approval)

The runner is a library under `crates/gents/src/eval/runner/`. It holds no state of its own.
Everything it knows is in the launching home's documents.

| File | Owns | Pure |
|---|---|---|
| `freeze.rs` | Turns a `RunRequest` into a written `EvalRun`. Resolves the definition to `(id, comparability_version, digest)`, selects cases by split, resolves each cell's pack digest, `behavior_id` and inference binding, and refuses OAuth-subscription backends. Idempotent on `run_id` | I/O |
| `plan.rs` | `plan(origin, existing_trials) -> Vec<PlannedTrial>`: the cells x cases x trial-index matrix, minus trials that already have a completion, with the next `attempt` for the rest | pure |
| `executor.rs` | The trait, `TrialSpec`, `TrialEvidence`, `TrialLocator` | types |
| `scripted.rs` | `ScriptedExecutor`: a table of evidence keyed by `(cell label, case_id, trial_index, attempt)` | pure |
| `embedded.rs` | `EmbeddedExecutor`: fresh home, pack install, stages, capture | I/O |
| `grade.rs` | `grade(case, evidence, registry) -> Vec<VerdictRow>`. Stages that never ran get `skipped_prerequisite` rows | pure |
| `record.rs` | Provisions the `EvalTrial`, writes the completion once, appends verdicts | I/O |
| `mod.rs` | `run()` and `resume()`: the loop, bounded concurrency, the retry breaker | I/O |

**Resume is `plan()` run again.** There is no separate resume logic. A trial row with a null
completion is abandoned, and the plan emits the same slot with `attempt + 1`.

**The seam.**

```rust
trait TrialExecutor {
    /// Never returns Err. A home that fails to boot is evidence with
    /// `failure_kind: infrastructure`.
    async fn execute(&self, spec: &TrialSpec) -> TrialEvidence;
    /// Re-reads evidence from a retained trial home, for regrades.
    async fn recollect(&self, at: &TrialLocator) -> Option<TrialEvidence>;
}
```

`TrialSpec` holds only what a trial is allowed to contain: the subject pack, `behavior_id`, the
inference binding with `seed`, the fixtures, the ordered `(stage_id, prompt, deadline_secs)` list, and
the capture queries. The capture queries run after each stage, from outside the trial.

`TrialSpec` leaves out check names and parameters, tiers, splits, `case_id` and other cases. The
executor never receives the case, so it cannot leak it. Threat T1 is structural, not a convention.

`TrialEvidence` holds `locator` (`trial_agent_did`, `session_id`, `home_hint`); `stages`, each with
`{stage_id, request_id, terminal_state, failure_kind, messages, tool_calls, captures}`; `usage` (token
sums, `null` when unknown); and `anchor`. The anchor keeps the contract's definition: terminal states
plus request and inference-call counts. An `evidence_digest` is added beside it, which is additive
because `completion` is JSON.

Because `execute` never fails, the error taxonomy stays in one place: the outcome vocabulary.

**Lean.** M2 adds no Lean. Freezing, planning and retrying are scheduling. Every legal outcome and
projection the runner writes is already modeled in `Eval.lean` from M1. The rule "latest completed
attempt wins; abandoned attempts are `infrastructure`" becomes a conformance case on `plan.rs` and
`grade.rs`, not a new proof.

## Sections still to come

2. The trial lifecycle inside `EmbeddedExecutor`: home creation, pack install, stage submission,
   deadline and interrupt, capture, retention on disk.
3. The run loop: freezing, concurrency, the retry breaker, write ordering, cancellation,
   invalidation.
4. Testing and phasing: the scripted matrix, the `MockStreamingBackend` canary test, the PR stack.
