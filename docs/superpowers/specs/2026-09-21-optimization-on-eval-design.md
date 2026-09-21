# Optimization on the eval contract (issue #1455, sub-project 5)

Status: design, approved on 2026-09-21. Baseline: `main` at `0deb7659c`.
Umbrella: `2026-09-21-eval-and-optimization-umbrella.md`.
Depends on: `2026-09-21-eval-core-contract-design.md`.
Supersedes: `2026-09-21-optimization-substrate-design.md`.

## Goal

Freeze a baseline configuration, take candidate patches from a proposer, evaluate each candidate
beside the current checkpoint through native eval runs, decide with a deterministic policy outside
model control, and promote through a digest-guarded, operator-only transaction.

## What changed from the superseded spec

| Removed | Replaced by |
|---|---|
| The `Evaluator` trait, `EvalRequest`, `EvalReport` | Native `EvalRun`s created through `gents::eval::runner` |
| `ScriptedEvaluator`, `HarnessEvaluator`, PR 7 | The runner's scripted `TrialExecutor` (eval spec 2) |
| Opaque `evidence_ref` strings; per-arm tallies copied into the journal | `run_id` references; decisions recomputed from `EvalVerdict` rows |
| `EvaluationStarted` and `Interrupted` journal entries | A null `completion` on `EvalTrial`, and attempts |
| Pooled Fisher exact test over binary outcomes, about 20 trials per arm | A paired sign-flip permutation test over per-case score differences, 2 to 3 trials per case |
| Checkpoint outcomes reused in later rounds | The checkpoint re-evaluated beside every candidate, on shared seeds |
| A private four-value `OutcomeClass` | The eval contract's kind-to-class projection |

Unchanged: `publishIf` and the Rust compare-and-set; the `Proposer` trait; the structural gate; the
length-guarded journal; `promote` and `show`; the rule that promotion checks the entire frozen closure
and writes only the target document; the test pinning that the model-facing `config` tool has no
optimization surface; the ordered three-gate decision and its Lean theorems.

## Non-goals

- Multi-candidate rounds, a Pareto archive, merge, or a judge pre-filter.
- Dollar-cost budgets. Token cost is in the policy; dollars are not.
- Any optimization verb in the model-facing `config` tool.
- A second request or TaskRun lifecycle. The runtime never claims or executes a job.
- Automatic revert. Recall is an operator decision.

## 1. Subject and target

- **Baseline subject.** The owner's configuration exported as a pack-shaped snapshot (the CLI's config
  export already builds one), naming the `behavior_id` under evaluation. Packs cannot carry inference
  backends, so the inference binding goes through slots. That is the isolation an eval subject should
  have: it carries no credentials.
- **Target.** That behavior's `AgentContext.system_prompt`, or the `prompt_template` of a Task bound
  to that behavior. `TargetField` allows nothing else.
- **Candidate.** The same snapshot with the one field patched, which gives a new pack digest. The
  digest is the dedup key against the checkpoint and every earlier candidate.
- **Frozen closure.** The owner's full desired configuration as `(collection, owner, id, digest)`
  entries, read through `ConfigReferences::load_in_txn`. Promotion checks it; the subject snapshot is
  derived from the same read.

## 2. The proposer

```rust
#[async_trait]
pub trait Proposer: Send + Sync {
    async fn propose(&self, input: ProposalInput) -> Result<Proposal>;
}
```

`ProposalInput` holds the current target text, the train-split verdicts of the current checkpoint with
their feedback, and the rejection history. `Proposal` holds the new text and a rationale. The proposer
receives data and returns a value. It never holds a `ConfigAccess`, and `target.rs` builds the patch
deterministically from the proposed text.

The first implementation is a scripted proposer. The LLM proposer and its sanitized evidence
projection follow M6: an ordinary `AgentRequest` on a dedicated behavior with no write grants, with
`RenderedRequest` excluded from its evidence.

## 3. A round

1. **Train run.** One `EvalRun` on the train split with one cell, the checkpoint, and `purpose:
   "optimization:<job_id>"`. Its verdicts carry the feedback the proposer reads.
2. **Propose.** The proposer returns a text and a rationale.
3. **Structural gate, before any validation spend.** The patch touches only the allowed
   `TargetField`. `validate_desired_state_plan` passes on the patched closure. A Task template keeps
   its placeholder set. The text is within the length cap. The candidate's digest differs from the
   checkpoint and from every earlier candidate. A structural failure is journaled as a rejection with
   diagnostics.
4. **Validation run.** One `EvalRun` on the validation split with two cells, the checkpoint and the
   candidate, on shared seeds.
5. **Decide.** The policy reads that run's `EvalVerdict` rows. On `Inconclusive`, with re-runs left,
   the driver starts another validation run with a new `seed_base`; the policy then reads both runs'
   pairs. Re-runs add pairs and never replace them.
6. **Checkpoint.** An `Accept` makes the candidate the retained checkpoint. The job returns the
   retained checkpoint, never the best candidate it ever saw.

Rejection memory is the journal. The proposer receives prior texts, rationales, decisions and reasons.
It never receives `Inconclusive` or budget-exhausted rounds as negatives.

## 4. The policy

`policy::decide(mode, params, pairs) -> Decision` is pure: no I/O, no clock, no unseeded randomness.
`PolicyV2` params are frozen into the job at start. `Decision` is `Accept`, `Reject(reason)` or
`Inconclusive(reason)`.

The unit is the case. Scores are quantized to basis points. For each case, `d_c` is the mean over
evidence pairs of the candidate's case score minus the checkpoint's. Pairing, exclusion of NotEvidence
and worst-case imputation of Unknown are the eval contract's denominator policy.

1. **Sufficiency, otherwise `Inconclusive`.** Both cells have the same cases. Each case has at least
   `min_pairs` evidence pairs. The NotEvidence share per cell is at most `max_not_evidence_bp`. The gap
   in dropped pairs between cells is at most `max_asymmetry_bp`, so a candidate that breaks the harness
   more often cannot win by exclusion. The test can reach significance: `2^-n_cases <=
   alpha_effective`, otherwise `Inconclusive(too_few_cases)`.
2. **Non-regression, otherwise `Reject`.** No case has `d_c` below `-case_tolerance_bp`
   (`case_regression`). The default tolerance is coarse: at 2 to 3 trials per case this catches a case
   that broke, not noise. The candidate's mean tokens per case-trial do not exceed the checkpoint's by
   more than `max_token_increase_bp` (`cost_regression`). If usage is null for more than
   `max_missing_usage_bp` of trials, the cost sub-gate is skipped and the skip is journaled.
3. **Improvement, otherwise `Reject(no_improvement)`.** A one-sided sign-flip permutation test on the
   `d_c` values gives `p`. It is exact by enumeration up to 20 cases and a seeded Monte Carlo above
   that, with the seed derived from the `run_id`. Accept needs `p <= alpha_effective` and mean `d_c >=
   min_effect_bp`. `alpha_effective = alpha / max_rounds`, a Bonferroni correction for an optimizer
   that tries several candidates on one validation split.

Consequences stated plainly. At `alpha = 0.05` and three rounds, a job needs at least six validation
cases to ever accept. The #1512 four-case suites cannot promote anything, and the policy says so.
Trials per case default to 2 to 3. Wall-clock budget goes into cases, because at fixed wall-clock ten
trials per case is roughly a threefold waste against three.

Two modes. `Improve` runs all three gates on a validation run. `Confirm` runs gates 1 and 2 on the one
held-out run. Parameter defaults are placeholders marked uncalibrated until the A/A calibration
(umbrella M5) sets them.

## 5. The job record

One local-only document, `OptimizationJob`, modeled on `CallbackInvocationDoc`: a frozen `origin`, an
append-only `journal` guarded by the expected journal length, and a derived `state` that is not named
`lifecycle_state`. It is listed in `LOCAL_AUDIT_COLLECTION_NAMES`, because its journal holds candidate
prompts. It stays out of the `Collection` enum and every P2P list, and a `DatastoreToolSurface` may
never name it.

`origin` freezes: the target; the closure digests; the baseline subject's pack digest and
`behavior_id`; the definition reference (`definition_id`, `comparability_version`, digest); the
`PolicyV2` params; `trials_per_case`; budgets (`max_rounds`, total case-trials, tokens, a wall-clock
deadline); the owner DID.

Journal entries: `Frozen`, `RunStarted { run_id, round, split }`, `Proposed`, `StructuralReject`,
`Decided { round, attempt, run_ids, mode, decision, policy_version, summary }`, `BudgetExhausted`,
`Finalized`, `Promoted { by, target_digest, previous_text, previous_digest }`, `PromotionRefused {
drifted }`, `Reverted { by }`.

`summary` holds the improved, tied and worsened case counts, the mean `d_c` and `p`. It is a
convenience. The authority is the `EvalVerdict` rows of the referenced runs: `optimization show`
recomputes every decision from them and flags a mismatch, and flags any decision whose run has been
invalidated.

## 6. The driver

```
Frozen -> round k: train run -> Proposed -> StructurallyRejected
                                         |  validation run(s) -> Accepted | Rejected | Inconclusive
  -> Finalizing -> ReadyToPromote | NothingToPromote | Exhausted | Failed
ReadyToPromote -> Promoted | Stale      (only through promote)
Promoted -> Reverted                    (only through revert)
```

- **Interruption.** On start the driver replays the journal. A `RunStarted` without a `Decided` means
  the run may be incomplete: the driver asks the runner to resume that run, which abandons trials with
  a null completion and re-executes them as new attempts. Replay is the only place a round is opened
  or closed, so a resumed job and an uninterrupted job reach the same state.
- **Baseline drift.** On every start the driver re-reads the closure. If its digests differ from
  `origin`, the job ends as `Failed(baseline_drifted)` before any further spend.
- **Definition drift.** If the installed definition's digest or `comparability_version` differs from
  `origin`, the job ends as `Failed(definition_changed)`.
- **Budgets** are checked before every run. The cost of the one held-out run is reserved at freeze. A
  candidate that was never evaluated is journaled `BudgetExhausted`, not rejected.
- **Finalization.** If the checkpoint differs from the baseline, one held-out run with two cells, the
  original baseline and the final checkpoint, judged in `Confirm` mode. The job ends as
  `ReadyToPromote`, `Failed(held_out_regression)` or `Failed(held_out_inconclusive)`. The held-out
  split is run once per job and never re-run. If no round accepted, held-out is never touched and the
  job ends as `NothingToPromote`, or `Exhausted` when the loop ended on budget.

Role separation is by construction. The proposer holds no `ConfigAccess`. Solvers run inside trial
homes under throwaway identities. Grading belongs to the eval runner. Promotion is the operator's CLI.
The driver is the only writer of the job, as the owner DID.

## 7. Promotion and revert

Lean and Rust for the compare-and-set are unchanged from
`plans/2026-09-21-optimization-substrate-1-foundation.md` PRs 1 and 3: `publishIf` as a precondition
on the one existing `publish`, and `expected` digests on `DesiredStateApplyPlan` with a typed
`StaleExpectation`.

`gents optimization promote <job> --digest D` runs one transaction: require `ReadyToPromote`; require
`D` to equal the checkpoint's digest; re-read the closure and compare it with `origin`; rebuild the
patch from the live target document and verify the rebuilt digest equals the journaled one; apply a
plan that writes only the target document and expects the entire frozen closure; append `Promoted`
with `previous_text` and `previous_digest`. On drift the transaction rolls back, a second transaction
journals `PromotionRefused`, and the job is `Stale`. There is no force flag. Only the job's owner DID
may promote.

`gents optimization revert <job> --digest D` writes `previous_text` back through the same
compare-and-set. It expects the target document to still equal what was promoted, and appends
`Reverted`. Operator-only; nothing reverts on its own.

A promoted prompt reaches requests claimed after the runtime's control watcher reconciles the changed
`AgentContext`, which has a 5 second debounce. Configuration replicates runtime-to-client only, so
under the one-active-runtime convention a local digest check is not bypassed by a late merge.

## 8. Lean

`Proofs/Optimization.lean` keeps imputation (now the eval contract's kind-to-class projection),
`decideGates` with its accept-iff and antitone theorems, and the length-guarded journal with bounded
rounds. The two integer gates are restated over per-case basis-point differences. The improvement test
stays an abstract `Bool`. `decideGates` gains the `cost_regression` reason inside gate 2. Conformance
cases are regenerated accordingly. Numeric properties of the permutation test are checked by Rust
property tests: replacing a candidate pair's score with a lower one never produces an `Accept` that
was not already there.

## 9. Testing

- Unit: the permutation test against hand-computed exact values; the too-few-cases rule; Bonferroni
  arithmetic; the cost gate including the missing-usage skip; totality.
- End to end, with the runner's scripted `TrialExecutor`, deterministic and part of `cargo test -p
  gents`: accept; reject as `no_improvement`, `case_regression` and `cost_regression`; structural
  reject with zero validation runs; inconclusive then re-run then final inconclusive; too few cases;
  budget exhaustion; held-out regression; held-out requested only at finalize; feedback reaches the
  proposer only from the train run; baseline drift; definition drift; resume after interruption at
  every run boundary yields the same journal as an uninterrupted job; a decision on an invalidated run
  is flagged by `show`.
- Promotion: promote once; stale closure refused with the live node unchanged; an edited target
  refused and the user's edit preserved; a foreign DID refused; a wrong digest refused; a job that is
  not ready refused; revert restores the previous text and is refused if the target moved.
- The model-facing `config` tool exposes no optimization resource or verb.
- Live, never in CI: the A/A calibration and one accept plus one reject on the `monitor-findings`
  definition (umbrella M3, M5, M6).

## 10. What happens to the written plans

| Plan | Status |
|---|---|
| `plans/…-1-foundation.md`, PR 1 Task 1 (`publishIf`) and PR 3 (compare-and-set) | Valid. Can start now |
| `plans/…-1-foundation.md`, PR 1 Task 2 (`Optimization.lean`) and PR 2 (conformance) | Partly valid. The integer gates and their generated cases are restated per section 8; re-plan with this spec |
| `plans/…-2-core-and-driver.md` | Superseded. `target.rs` and the journal mechanics carry over; the policy, the evaluator seam, the journal vocabulary and the driver do not |
| `plans/…-3-promotion-and-live-demo.md` | Superseded. `promote`, `show` and the CLI carry over and gain `revert`; PR 7 is deleted |

## 11. Implementation plans

- The pure half (Lean model, conformance, `PolicyV2`): `plans/2026-09-21-optimization-policy-and-lean.md`.
- The job record, driver, `promote`, `show` and `revert` wait for the eval runner's API (eval spec 2).
