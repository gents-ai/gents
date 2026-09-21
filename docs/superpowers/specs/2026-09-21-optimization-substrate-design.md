# Optimization substrate design (issue #1455)

Status: design, approved section by section on 2026-09-21. Baseline: `main` at `0deb7659c`.

## Goal

Build the substrate for #1455's configuration optimization loop without waiting for #1515's native
evaluation. The substrate freezes a baseline, takes candidate patches from a proposer, has them
evaluated in isolation, decides with a deterministic policy outside model control, and promotes
through a digest-guarded, operator-only transaction.

Evaluation sits behind one Rust trait. Two throwaway implementations serve until #1515 lands; a
native implementation then replaces them and nothing else in the substrate changes.

## Decisions

| Decision | Choice |
|---|---|
| Scope | The whole substrate in one spec; implementation phased as stacked PRs |
| Candidate existence | Isolated per-trial nodes only. The live node changes only at `promote` |
| Prototype evaluator | A scripted evaluator for substrate tests, plus an adapter over the existing host-steward monitor suite for the live demo |
| Proposer | A trait. A scripted implementation first; an LLM implementation in a later phase |
| Architecture | A library module owns the substrate; the evaluator is injected |
| Deadline outcomes | Classified `Fail`, as a substrate rule |
| Job record placement | Local-only, non-replicated, like `RenderedRequest` |
| Promotion precondition | Checks the entire frozen closure; writes only the target documents |

## Non-goals

- Live-node sibling materialization, round tags and sibling cleanup.
- Multi-candidate rounds, a Pareto archive, merge, or a judge pre-filter.
- Dollar-cost budgets.
- An ACP-based role lattice. Role separation here is by construction.
- Any optimization verb in the model-facing `config` tool.
- A second request or TaskRun lifecycle. The runtime never claims or executes a job.
- New eval case schemas. Case contents belong to the evaluator and, later, to #1515.

## 1. Module boundaries

New module `crates/gents/src/optimization/`. Each unit has one job.

| Unit | Job | Depends on |
|---|---|---|
| `outcome.rs` | `OutcomeClass` and `CaseTrialOutcome` | nothing |
| `evaluator.rs` | The `Evaluator` trait, `EvalRequest`, `EvalReport` | `outcome` |
| `proposer.rs` | The `Proposer` trait, `ProposalInput`, `Proposal` | `outcome` |
| `target.rs` | The `TargetField` allow-list, frozen-closure capture, deterministic one-field patch construction | `config_client` |
| `policy.rs` | Pure `decide(baseline, candidate, params) -> Decision` | `outcome` |
| `round.rs` | The `OptimizationJob` document and its append-only journal | `config_client`, schema |
| `driver.rs` | The job state machine over an injected `Evaluator` and `Proposer` | all of the above |
| `promote.rs` | Digest-guarded, operator-only promotion | `config_client`, `round` |

Throwaway and test code lives in `crates/gents/tests/`: `ScriptedEvaluator`, `ScriptedProposer`, and
`HarnessEvaluator`.

## 2. The evaluator seam

```rust
#[async_trait]
pub trait Evaluator: Send + Sync {
    async fn provenance(&self) -> Result<String>;
    async fn evaluate(&self, request: EvalRequest) -> Result<EvalReport>;
}
```

`provenance()` is read once at freeze. Every report repeats it, and the driver fails the job as
`evaluator_changed` when a report disagrees with the frozen value.

`EvalRequest`:

- `arm`: `Baseline` or `Candidate`.
- `plan`: a `DesiredStateApplyPlan` holding the configuration to evaluate. For a candidate it is the
  frozen closure with the patch applied.
- `split`: an opaque `SplitId` of `Train`, `Validation` or `HeldOut`. The evaluator owns which cases
  belong to each split. The substrate never sees case contents.
- `trials`: the trial count per case, and a `trial_offset` so re-runs append instead of colliding.
- an optional deadline.

`EvalReport`:

- `outcomes: Vec<CaseTrialOutcome>`.
- `provenance`: an opaque digest over graders, fixtures and cohort. It is frozen into the job so a
  change of evaluator mid-job is detectable.

`CaseTrialOutcome` holds `case_id`, `trial`, `class: OutcomeClass`, `raw_kind: String`, `feedback:
Option<String>`, `evidence_ref: String` and `usage`. `raw_kind` stays opaque so the evaluator's own
verdict is preserved, as #1515's findings require.

Rules that keep the seam cheap to swap:

1. The contract is a Rust trait, never a DefraDB schema. This satisfies #1455's rule against
   optimizer-specific eval schemas.
2. Only the evaluator maps its concrete kinds onto `OutcomeClass`. #1515 can rename kinds without
   touching the policy or the Lean model.
3. Every evaluator maps a deadline to `Fail`. Under comparable settings, running out of time is
   behavior of the configuration.
4. Feedback is only meaningful on `Train`. The driver discards feedback on any other split, so the
   rule does not depend on the evaluator behaving.
5. `SplitId::HeldOut` is constructible only in the driver's finalize step.

### Migration when #1515 lands

1. Add a native `Evaluator` implementation in the library.
2. Delete `HarnessEvaluator`, its make target and the A/A calibration entry.
3. Add a CLI `run` verb.

`ScriptedEvaluator` stays as the permanent test double. Nothing else changes.

## 3. The proposer seam

```rust
#[async_trait]
pub trait Proposer {
    async fn propose(&self, input: ProposalInput) -> Result<Proposal>;
}
```

`ProposalInput` holds the current target text, the `Train` evidence (outcome classes and feedback),
and the rejection history. `Proposal` holds the new target text and a rationale.

The proposer receives data and returns a value. It never holds a `ConfigAccess`, and it never writes
a document. `target.rs` builds the actual patch deterministically from the proposed text.

Phase 1 ships `ScriptedProposer`, which returns literal known-good and known-bad texts. The LLM
proposer and its sanitized evidence projection are a later phase of this spec (see section 9).

## 4. Outcome vocabulary and promotion policy

`policy::decide(baseline, candidate, params) -> Decision` is a pure function with no I/O, no clock
and no randomness. `params` is a `PolicyV1` struct frozen into the job at start. A rule change is a
new version, never an edit. `Decision` is `Accept`, `Reject(reason)` or `Inconclusive(reason)`.

| Class | Meaning | Treatment |
|---|---|---|
| `Pass`, `Fail` | The evaluator reached a verdict | Evidence |
| `NotEvidence` | Infrastructure or provider trouble, or a skipped prerequisite | Excluded, subject to the caps in gate 1 |
| `Unknown` | The evaluator could not classify the trial | Worst-case imputation: `Fail` in the candidate arm, `Pass` in the baseline arm |

The decision is three ordered gates. The first failing gate decides.

1. **Sufficiency, otherwise `Inconclusive`.** Both arms have the same cases, and each case has the
   same number of trials in both arms. Each case has at least `min_trials` evidence outcomes per arm. The `NotEvidence` fraction per arm is at most
   `max_not_evidence`. The gap in `NotEvidence` between arms is at most `max_asymmetry`, so a
   candidate that breaks the harness more often cannot win by exclusion.
2. **Per-case non-regression, otherwise `Reject(case_regression)`.** For every case, the candidate's
   pass rate is at least the baseline's minus `case_tolerance`.
3. **Improvement, otherwise `Reject(no_improvement)`.** A one-sided Fisher exact test on pooled
   pass/fail counts by arm reaches `alpha`, and the pooled difference is at least `min_effect`. Ties
   are rejects. Gate 1 forces equal trial counts per case, so pooling is unbiased.

Re-runs append trials to both arms equally and never replace earlier trials. After `max_reruns`, an
`Inconclusive` decision is final. It is journaled as inconclusive and never reaches the proposer as
negative evidence.

Two modes. `Improve` runs all three gates on `Validation` every round. `Confirm` runs gates 1 and 2
once on `HeldOut` at finalization, comparing the retained checkpoint against the original baseline.
A gate 2 failure there ends the job as `Failed(held_out_regression)`. A gate 1 failure ends it as
`Failed(held_out_inconclusive)`: the held-out split is evaluated once and is never re-run.

All fractions are integer basis points and the significance level is parts per million, so Lean and
Rust compute the two integer gates identically. `alpha`, `min_effect`, `min_trials` and the caps are
parameters. Their defaults are placeholders
marked uncalibrated until the A/A calibration run sets them.

## 5. The job record

One document, `OptimizationJob`, modeled on `CallbackInvocationDoc`, which keeps a frozen input and
an action journal on a single document.

- `origin`, frozen at creation and never rewritten: the target (`collection`, `id`, `TargetField`);
  the baseline closure as `(collection, owner, id, digest)` entries from
  `desired_state_document_digest`. The closure is the owner's full desired configuration, read
  through `ConfigReferences::load_in_txn`. That is a safe superset of the documents the target
  reaches, and the repository has no generic reference walker to narrow it; the `PolicyV1` params; the evaluator provenance digest; trial
  counts; budgets (`max_rounds`, total trials, tokens, a wall-clock deadline); the owner DID.
- `journal`, append-only. Each append is a transaction that checks the expected journal length.
- `state`, a summary derived from the last journal entry. It is not named `lifecycle_state`.

The collection is local-only and non-replicated, because the journal contains candidate prompts. It
is listed in `LOCAL_AUDIT_COLLECTION_NAMES`, stays out of the `Collection` enum (it is a runtime
document, not configuration), and stays out of every P2P collection list.
Empty lists are written as `null`, per the repository rule.

`TargetField` allows exactly `AgentContext.system_prompt` and `Task.prompt_template`.

### Journal entries

`Frozen`, `EvaluationStarted`, `Evaluated`, `Interrupted`, `Proposed`, `StructuralReject`, `Decided`,
`BudgetExhausted`, `Finalized`, `Promoted`, `PromotionRefused`.

`EvaluationStarted` is written before every evaluator call, so an unmatched one identifies an
interrupted evaluation. `Evaluated` records one arm on one split: per-case tallies, opaque
`evidence_ref`s and resource use. A re-run is further `Evaluated` entries followed by another
`Decided` with a higher `attempt`. Each `Decided` entry, including the held-out confirmation, stores
the two merged tallies it judged. Those are exactly the input to `policy::decide`, so every decision
is recomputable from the journal alone. Feedback text is never journaled.

## 6. The driver state machine

```
Frozen -> BaselineEvaluated
  -> round k: Proposed -> StructurallyRejected
                       |  Evaluated -> Accepted | Rejected | Inconclusive
  -> Finalizing -> ReadyToPromote | NothingToPromote | Exhausted | Failed
ReadyToPromote -> Promoted | Stale      (only through promote)
```

- **Train evidence.** Each round first evaluates the current checkpoint on `Train`. Those outcomes,
  with their feedback, are the proposer's evidence. They are held in memory only, so a resumed round
  evaluates `Train` again.
- **Baseline drift.** On every start the driver re-reads the closure. If its digests differ from
  `origin`, the job ends as `Failed(baseline_drifted)` before any further evaluation spend.
- **Retained checkpoint.** It starts as the baseline. An `Accept` replaces it. Later rounds patch the
  checkpoint and compare against it. The job returns the retained checkpoint, never the best
  candidate it ever saw.
- **Structural gate before any evaluation spend.** The patch touches only the allowed `TargetField`.
  `validate_desired_state_plan` passes on the patched closure. A Task template keeps its placeholder
  set. The text is within the length cap. The candidate's digest differs from the checkpoint and from
  every previously rejected candidate. A structural failure is journaled as a rejection with
  diagnostics.
- **Rejection memory** is the journal. The proposer receives prior patches, rationales, decisions and
  reasons. It never receives `Inconclusive` or `BudgetExhausted` entries as negatives.
- **Budgets** are checked before every evaluation. The cost of the one finalization evaluation is
  reserved at freeze, so round spending can never consume it. Exhaustion ends the round loop, and a
  candidate that was never evaluated is journaled `BudgetExhausted`, not rejected.
- **Finalization** happens when the round loop ends, whether by `max_rounds` or by exhaustion. If the
  checkpoint differs from the baseline, the driver runs one `Confirm` evaluation on `HeldOut` and the
  job ends as `ReadyToPromote`, `Failed(held_out_regression)` or `Failed(held_out_inconclusive)`. If
  no round accepted, `HeldOut`
  is never touched and the job ends as `NothingToPromote`, or as `Exhausted` when the loop ended on
  budget.
- **Interruption.** On start the driver replays the journal and resumes after the last complete
  entry. An evaluation that was in flight is journaled `Interrupted` and its partial outcomes are
  discarded. If the evaluator's provenance digest differs on resume, the job ends as
  `Failed(evaluator_changed)`.

### Role separation

| Role | How it is confined |
|---|---|
| Proposer | Receives data, returns a value; holds no `ConfigAccess` |
| Solver | Runs inside the evaluator's per-trial nodes under throwaway identities |
| Grader | Belongs to the evaluator |
| Promoter | The operator, through the CLI |
| Driver | The only writer of the job, as the owner DID |

This meets #1455's separate-scoping requirement through configuration owners and construction.
Document ACP is not attached to config collections today (blocked upstream on defradb.rs#1318).

## 7. Promotion

### Lean

A precondition on the one existing `publish` in `Proofs/ApplyReconcile/Publication.lean`, not a
second publication model:

```lean
def publishIf (old : LiveState) (scope : List DocRef)
    (expected : DocRef → Option DesiredFields) (candidate : Manifest) : LiveState :=
  if ∀ d ∈ scope, old.desired d = expected d then publish old candidate else old
```

Theorems, without `sorry`: a stale expectation leaves state unchanged; a matching expectation equals
`publish`; an empty scope equals `publish`, so existing callers are unaffected; observation
preservation, all-or-nothing and idempotence carry over. Digest equality stands in for field equality
at the refinement boundary, and the proof map says so.

New `Proofs/Optimization.lean` models imputation, the two integer gates, the ordered decision and the
job journal. The improvement test is an abstract `Bool`. It proves that a candidate `Unknown` is
never a pass and is treated exactly like `Fail`, that a candidate `Pass` is the best outcome a trial
can have, that `Accept` holds exactly when every consulted gate holds, that weakening any gate never
produces an `Accept`, that the length-guarded journal append preserves every prefix, and that rounds
are bounded by `max_rounds`. Lean functions are total, so totality needs no theorem.

An earlier draft of this spec claimed that turning any candidate outcome into `Fail` never produces
an `Accept`. That is false: turning a `NotEvidence` into a `Fail` adds evidence and can lift a job
from `Inconclusive` to `Accept`, which is correct behavior. The true statement is about replacing a
`Pass`. Its numeric form over the concrete gates is checked by a Rust property test, not in Lean.

### Rust

- `DesiredStateApplyPlan` gains `expected: Vec<(Collection, owner, id, Option<digest>)>`. A `None`
  digest means the document must not exist.
- `apply_desired_state_plan` checks expectations first, inside the caller's transaction, before any
  write. A mismatch returns a typed `StaleExpectation { drifted }`. Transaction closures already
  re-run on conflict retry, so the check re-runs with them.
- The `#[cfg(test)]` `verify_existing_desired_state_plan` is generalized into this precondition.
- `cleanup remove --digest` stays as it is. It is already a compare-and-set inside one transaction,
  but its contract is a single combined digest over the whole target set, which does not fit
  per-document expectations without changing that contract.

### What promote checks and writes

- It checks the **entire frozen closure**. If any closure document changed during the job, the
  evidence no longer describes the live configuration.
- It writes **only the target documents**, each being the frozen version with the one allowed field
  replaced. Because the closure check proves each target still equals its frozen version, the
  full-replacement `update` cannot overwrite an intervening user edit.
- It trusts no stored plan. It rebuilds the patch from `origin` plus the journal's accepted
  checkpoint and verifies that the rebuilt digest equals the journaled `candidate_digest`.

### Operator surface

- `gents optimization show <job>` prints the field diff, the recomputable decision summary and the
  checkpoint digest.
- `gents optimization promote <job> --digest D` runs one `ConfigAccess::transact`: re-read the job
  and require `ReadyToPromote`; apply the plan with its expectations (existing reference validation
  runs after staged writes); append `Promoted`.
- On `StaleExpectation` the transaction rolls back. A second small transaction journals
  `PromotionRefused { drifted }` and sets the job to `Stale`. There is no force flag; the remedy is a
  new job.
- A promote by any DID other than the job's owner is refused.

The model-facing `config` tool gets no optimization verbs, and a test asserts this against its
command table. With nothing exposed to the model, no new grant category is needed.

## 8. Testing

1. **Unit, pure.** `policy`: table-driven cases; property tests for totality, unknown-never-pass and
   monotonicity; the Fisher implementation against known hypergeometric values. `target`: the
   allow-list, placeholder-set preservation, deterministic patch and digest. `round`: append, replay,
   derived state.
2. **Conformance.** Lean-generated cases for `publishIf` and the policy decision structure, consumed
   by Rust tests alongside the existing conformance suites.
3. **Substrate end-to-end.** `ScriptedEvaluator` and `ScriptedProposer` against an embedded node, no
   model, deterministic, part of `cargo test -p gents`.
4. **Live, ignored, never in CI.** `make live-optimization-eval` drives `HarnessEvaluator` over the
   host-steward monitor suite with a scripted proposer supplying one known-good and one known-bad
   literal prompt. This is #1455's first delivery: one accept and one reject with retained evidence.
   An A/A calibration entry evaluates the baseline against itself repeatedly and reports the
   false-accept rate. It bypasses the driver, because a candidate identical to the baseline is
   structurally rejected by design.

End-to-end matrix for layer 3:

- accept;
- reject as `no_improvement` and as `case_regression`;
- structural reject, asserting zero evaluator calls;
- inconclusive, then re-run, then final inconclusive;
- budget exhaustion, with the candidate journaled `BudgetExhausted`;
- held-out regression;
- stale closure: edit any closure document mid-job, promote is refused, and the live node is
  unchanged afterward;
- unauthorized promote from a foreign DID;
- interruption after every journal entry, asserting the resumed journal equals the uninterrupted one
  apart from `Interrupted` entries;
- evaluator provenance change ends in `Failed`;
- `HeldOut` is never requested before finalize (the scripted evaluator records requested splits);
- feedback on a non-train split is discarded;
- the `config` tool's command table has no optimization verbs.

## 9. Phasing

Stacked PRs, each targeting its parent, in the repository's Lean, conformance, Rust order.

| PR | Content | Validation |
|---|---|---|
| 1 | Lean: `publishIf`; `Proofs/Optimization.lean`; proof-map README | `lake build`, no `sorry` |
| 2 | Conformance case emission and Rust consumers | conformance suite |
| 3 | Rust compare-and-set: `expected` on the plan, `StaleExpectation`, cleanup migration if it fits | `cargo test -p gents` |
| 4 | Pure core: `outcome`, `policy`, `target`, the two traits. Independent of PR 3 | unit, conformance |
| 5 | `OptimizationJob` schema and document, journal, driver, scripted doubles, the matrix except promote | end-to-end |
| 6 | `promote`, CLI `show` and `promote`, the stale, unauthorized and tool-surface tests | end-to-end, CLI suite |
| 7 | Throwaway: a golden monitor fixture, `HarnessEvaluator`, the live demo target, the A/A calibration | live run, manual |
| 8 | LLM proposer and sanitized evidence projection | detailed when reached |

PR 7 caveats. The existing monitor suites have the configurator model author the monitor inside each
trial, so no fixed baseline exists; PR 7 captures one as a fixture and re-homes it onto each trial
node's owner DID. Its three splits reuse the same two chained monitor cases, so its held-out split is
not independent. That is acceptable for a demo and is the eval-supply gap #1515 owns.

Implementation plans: `docs/superpowers/plans/2026-09-21-optimization-substrate-{1-foundation,2-core-and-driver,3-promotion-and-live-demo}.md`.

PR 8 constraints already fixed by this spec: the proposer runs as an ordinary `AgentRequest` on a
dedicated behavior with no write grants; its evidence is a bounded projection over `InferenceCall`
and `AgentToolCall` plus `Train` feedback; `RenderedRequest` is excluded.

Before each push: `cargo test -p gents` and `cargo check --workspace --all-targets`; `lake build` for
PRs 1 and 2; the CLI suite for PR 6.

## 10. Known limitations and open questions

- **P2P replication, resolved during planning.** Config collections replicate from the runtime to
  paired clients (`CLIENT_COLLECTIONS`), but the client-to-runtime leg
  (`CLIENT_TO_RUNTIME_COLLECTIONS`) carries no configuration. A paired device therefore cannot merge
  a config edit in after a promote commits. A second runtime for the same principal could, and the
  repository's convention of one active runtime per principal excludes it.
- **Live sessions, resolved during planning.** `system_prompt` is not re-read per request. A control
  watcher re-resolves configuration on document changes with a 5 second debounce and swaps the
  behavior's in-memory slot. A promoted prompt reaches requests claimed after that reconcile.
- **`pack install --digest`, observed, out of scope.** It compares its artifact digest outside the
  write transaction, so live drift between the check and the install is not detected.
- **Statistical power.** Reviewers estimate that ten trials per arm at temperature 1 cannot support a
  decision. The A/A calibration decides whether any `PolicyV1` defaults are defensible on today's
  cases. If they are not, the remedy is more independent cases, which is evaluation work.
- **Checkpoint reuse.** A later round compares against the checkpoint using the outcomes that got it
  accepted, which carries a selection bias. The held-out `Confirm` against the original baseline is
  the guard. Re-evaluating the checkpoint each round is the alternative if the bias proves material.
- **Surface churn.** PR #1550 is open against the self-config surface. This design avoids that
  surface: it adds no `config` verbs and does not extend `apply_persona_request`.

## 11. Recommended follow-up outside this spec

Amend #1455's text to record: the definitions of revision (a digest set over the reachable closure)
and promotion (a digest-guarded patch into the baseline); the outcome-class rules, including
deadlines as failures; the operator surface being CLI-only; that separate scoping is met through
configuration owners while document ACP is upstream-blocked; and that the first delivery is a
prototype against the existing harness, with the #1515 acceptance checkboxes left open.
