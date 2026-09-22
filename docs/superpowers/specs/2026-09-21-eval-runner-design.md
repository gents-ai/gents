# Eval runner design (issue #1515, sub-project 2a)

Status: design, approved section by section on 2026-09-21, then self-reviewed (the self-review's
changes are marked *[self-review]*). Baseline: `main` at `0deb7659c`.

## What this spec is

This is spec 2 in the umbrella's plan index: the design for milestone **M2, "Runner MVP"**. The
umbrella (`2026-09-21-eval-and-optimization-umbrella.md`, section 6) fixes M2's deliverable:

> `TrialExecutor` and the scripted executor; run freezing; one fresh embedded home per case-trial;
> subject pack install onto the trial DID; stage requests; the write-once completion; verdict
> writing; NotEvidence retries; resume by attempt; the canary test. Needs M1, a minimal M3.

The module it designs is `gents::eval::runner` (umbrella section 4). It consumes the documents and
vocabulary of spec 1 (`2026-09-21-eval-core-contract-design.md`, milestone M1, being built now) and
is consumed by M4 (`gents eval run | cancel | invalidate`), by M6b (the optimization driver calls
`run` twice per round) and by spec 2b (a process-per-trial executor behind the same seam).

Each section below names the part of M2's deliverable it designs.

| Section | M2 deliverable | Downstream milestone it serves |
|---|---|---|
| 1. Shape and the seam | `TrialExecutor`, the scripted executor, the module layout | M6b's scripted matrix; spec 2b |
| 2. The trial lifecycle | one fresh embedded home per case-trial; pack install onto the trial DID; stage requests; evidence capture | M3's monitor cases run through it; #1515's file evidence and retention |
| 3. The run loop | run freezing; the write-once completion; verdict writing; NotEvidence retries; resume by attempt | M6b's driver; M4's `cancel` and `invalidate` |
| 4. Testing and phasing | the canary test; the PR stack | the M2 implementation plan |

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
| Trial isolation in 2a | Embedded-first. At freeze time the embedded executor refuses any subject whose tools configuration grants `Unrestricted` bash. No bash, the sandboxed workspace modes, and datastore and file tools under a workspace root inside the trial home are allowed | `BashMode::Unrestricted` runs commands unmodified with the runner's privileges and environment, including API keys. Only the workspace modes get `sandbox-exec`, and only on macOS. A candidate prompt is untrusted text that steers an agent, so "embedded is not a sandbox" has to be an enforced boundary, not a caveat |

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

## Deviation from #1515

Issue #1515 names "one independent Gents process and retained home/database/workspace per trial" as
the design direction, and records #1512's embedded databases in one test process as the current
state. Spec 2a keeps the retained home, database, workspace and identity per trial, but runs trials
in one process. A process or container executor is spec 2b.

The evidence for deferring it: the `monitor-mailbox` family is document-driven, forbids machine
inspection in its own prompt, and already runs on `EmbeddedNode`. It can seed about twenty independent
cases for M3 with a golden monitor that needs no bash. The `host-steward` and `host-maintenance`
families (18 cases) are host-only: the monitor runs `df`, `sha256sum`, `stat` and `wget` inside a
container, and every fault is container filesystem or permission state. Porting them needs spec 2b,
which is therefore the stated prerequisite of spec 3b. The wide executor seam exists so that 2b is a
third `TrialExecutor` and changes nothing in the runner.

Two #1515 requirements are not yet designed and belong to section 2: durable references to file
evidence (project snapshots, screenshots, external grader evidence), and the retained-home layout
that `gents eval rm` deletes.

No fixed monitor pack exists in the repository. Every #1512 suite has a configurator author the
monitor inside the trial. M3 creates the golden subject and chooses its tool ceiling.

## Section 1: shape and the seam (approved)

*Implements: M2's `TrialExecutor` and scripted executor; the layout of `gents::eval::runner`.*

The runner is a library under `crates/gents/src/eval/runner/`. It holds no state of its own.
Everything it knows is in the launching home's documents.

| File | Owns | Pure |
|---|---|---|
| `freeze.rs` | Turns a `RunRequest` into a written `EvalRun`. Resolves the definition to `(id, comparability_version, digest)`, selects cases by split, resolves each cell's pack digest, `behavior_id` and inference binding, refuses OAuth-subscription backends, and refuses a subject that grants `Unrestricted` bash when the executor is embedded. Idempotent on `run_id` | I/O |
| `plan.rs` | `plan(origin, existing_trials) -> Vec<PlannedTrial>`: the cells x cases x trial-index matrix, minus trials that already have a completion, with the next `attempt` for the rest | pure |
| `executor.rs` | The trait, `TrialSpec`, `TrialEvidence`, `TrialLocator` | types |
| `scripted.rs` | `ScriptedExecutor`: a table of evidence keyed by `(cell label, case_id, trial_index, attempt)` | pure |
| `embedded.rs` | `EmbeddedExecutor`: fresh home, pack install, stages, capture | I/O |
| `grade.rs` | `grade(case, evidence, registry) -> Vec<VerdictRow>`. Stages that never ran get `skipped_prerequisite` rows | pure |
| `record.rs` | Provisions the `EvalTrial`, writes the completion once, appends verdicts | I/O |
| `mod.rs` | `run()` and `resume()`: the loop, bounded concurrency, the retry breaker | I/O |

**Resume is `plan()` run again.** There is no separate resume logic. A trial row with a null
completion is abandoned, and the plan emits the same slot with `attempt + 1`.

*[execution amendment, M2 ruling]* Every executed attempt is completed, NotEvidence included: a
null completion means only "the runner did not finish", so a crash and a provider outage stay
distinguishable in the row. `plan(origin, run_id, existing, max_infra_retries)` re-plans a slot
whose latest completed attempt classifies NotEvidence (no stages, or every stage that ran
classifies NotEvidence through `classify`) while `attempt <= max_infra_retries + 1`; a slot whose
latest completed attempt is Pass, Fail or Unknown is done. When the cap is hit, "latest completed
attempt wins" yields a NotEvidence trial and the denominator policy drops the pair.

**The seam.**

```rust
trait TrialExecutor {
    /// What this executor can isolate. `freeze` checks a subject's tool
    /// ceiling against it: `Embedded` refuses `Unrestricted` bash.
    fn isolation(&self) -> Isolation; // Embedded | Process
    /// Never returns Err. A home that fails to boot is evidence with
    /// `failure_kind: infrastructure`. Observes `cancel`.
    async fn execute(&self, spec: &TrialSpec, cancel: CancellationToken) -> TrialEvidence;
    // [execution amendment] `provision(spec) -> TrialLocator` precedes `execute`, so the `EvalTrial`
    // row is written from a real identity before execution, as the contract requires; the scripted
    // executor answers from its table, the embedded one creates the home. `wants_script_key()`
    // (default false) lets only the scripted executor receive a `case_id`-bearing key.
    /// Re-reads evidence from a retained trial home, for regrades. Takes
    /// the capture list because a locator alone does not say what to read.
    async fn recollect(&self, at: &TrialLocator, captures: &[Capture]) -> Option<TrialEvidence>;
}
```

*[self-review]* `isolation()` is how `freeze.rs` knows which refusals apply without knowing the
executor's type, and `recollect` takes the captures because the locator names a home, not a query.

`TrialSpec` holds only what a trial is allowed to contain: the subject pack, `behavior_id`, the
inference binding with `seed`, the fixtures, the ordered `(stage_id, prompt, deadline_secs)` list, and
the capture queries. The capture queries run after each stage, from outside the trial.

`TrialSpec` leaves out check names and parameters, tiers, splits, `case_id` and other cases. The
executor never receives the case, so it cannot leak it. Threat T1 is structural, not a convention.

`TrialEvidence` holds `locator` (`trial_agent_did`, `session_id`, `home_hint`); `stages`, each with
`{stage_id, request_id, terminal_state, failure_kind, provider_reason, messages, tool_calls,
captures}`; `usage` (token sums, `null` when unknown); and `anchor`. The anchor keeps the contract's
definition: terminal states plus request and inference-call counts. An `evidence_digest` is added
beside it, which is additive because `completion` is JSON.

*[self-review]* `terminal_state` is `RequestLifecycleState`, `failure_kind` is `OutcomeKind` and
`provider_reason` is `ProviderReason`: the closed types M1's `documents.rs` uses, never strings.
`home_hint` is a path relative to the launching home's root, so a moved home keeps a valid hint.
`evidence_digest` is SHA-256 over the canonical JSON (sorted keys) of `stages`, `usage` and
`anchor`, excluding `locator`, so the digest is the same wherever the home sits.

Because `execute` never fails, the error taxonomy stays in one place: the outcome vocabulary.

**Lean.** M2 adds no Lean. Freezing, planning and retrying are scheduling. Every legal outcome and
projection the runner writes is already modeled in `Eval.lean` from M1. The rule "latest completed
attempt wins; abandoned attempts are `infrastructure`" becomes a conformance case on `plan.rs` and
`grade.rs`, not a new proof.

## Section 2: the trial lifecycle inside `EmbeddedExecutor` (approved)

*Implements: M2's one fresh embedded home per case-trial, subject pack install onto the trial DID, stage requests, and evidence capture; #1515's file-evidence references and retention.*

One trial is one call to `execute(spec)`. It runs these steps in order. Every failure in steps 1 to 3
returns evidence with `failure_kind: infrastructure` and no stages; nothing panics and nothing is
retried inside the executor. Retry is the runner's decision.

**1. Materialize the subject.** At freeze time the runner copies each cell's pack contents into the
run directory and records the pack digest in `origin`. The executor reads the pack from there and
verifies the digest before use. A candidate is therefore never installed into the launching home. It
exists only inside the run directory until `promote` writes its one changed field. The launching
home's installed packs are the source for a baseline cell; a candidate cell's source is the
optimization driver, which writes the modified pack into the run directory itself.

**2. Create the home.** A fresh DefraDB data directory and a fresh `KeyIdentity` at
`<run dir>/trials/<trial_id>/home/`, plus an empty `workspace/` beside it. The trial agent DID is
that identity. The runner is the only holder of the key, so it acts as the trial's operator: it
installs the pack with `DesiredStateApplyPlan::from_pack_config` and `apply_desired_state_plan`,
writes the inference binding (with `seed`) as an `InferenceBackend` document, installs a
`WorkspaceRoot` pointing at `workspace/`, and installs the fixtures, then boots `Gents` on that node
and starts the completion loop.

*[self-review]* Fixtures install by kind. Schemas are registered first, through the same schema
registration the runtime uses at startup. Configuration documents (members of the `Collection`
enum) go through the apply path. Input documents in runtime or fixture collections, such as a
`MonitorInput` row, are not apply-controlled; they are written by a direct create mutation through
the trial's `ConfigAccess`, with every interpolated string escaped. Files are copied into
`workspace/`. The `evaluator_did` recorded on the `EvalRun` is the launching home's identity.

**3. Open the session.** The executor creates one `AgentSession` for the trial, selecting
`behavior_id`. Every stage's request joins that session, so `(trial_agent_did, session_id)` names
all the evidence.

**4. Run the stages.** For each `(stage_id, prompt, deadline_secs)` in order, the executor writes an
`AgentRequest` into the session the way `gents request` does, then polls `lifecycle_state` until a
terminal state or the deadline. On the deadline it interrupts through the existing interrupt owner,
waits the existing 30-second grace, and records `deadline`. The request's terminal state is classified
by the rules `stages.rs` uses today, which move into `src/` with the code: `Completed` is
`passed` (the stage ran), `Failed` is `tool`, `runtime` or `provider` with its reason, an
interrupt on the deadline is `deadline`, and `Dead` or `Superseded` is `runtime`. After any stage
whose request did not reach `Completed`, the remaining stages are not submitted and are recorded
as `skipped_prerequisite`. Captures still run for the failed stage: partial work is evidence.

*[self-review]* `model_acceptance` is never assigned by the executor. It is a grading outcome:
`grade.rs` writes it when an acceptance check fails on a stage that ran. The executor never sees
checks, so it cannot know whether a completed stage "passed" in that sense, and a later stage is
therefore not skipped because an earlier stage's checks failed, only because the earlier stage did
not run to completion. A recovery case whose first stage reported the wrong findings still runs its
second stage, and its reducer decides the case.

Submission goes through the same library function the `gents request` command calls; the plan names
it. The capture `filter` is a DefraDB filter object passed verbatim in 2a; spec 3 may narrow it.

**5. Capture after each stage.** The executor runs the spec's capture queries against the trial
node, from the runner side, and stores the rows under each capture's `name`. Session records
(requests, messages, tool calls, inference calls, responses) are always collected for the stage.
A file capture is `{name, kind: "file", glob}` over `workspace/`; the evidence records
`{name, path, sha256, bytes}` per matched file and never the contents. Checks that need contents
read them through the locator. This is the durable file-evidence reference #1515 asks for.

**6. Close.** The executor stops the completion loop, shuts the node down, and returns
`TrialEvidence` with `usage` summed from `InferenceCall` rows (`null` if any total is absent), the
`anchor` (terminal states, request and inference-call counts, file digests count), and
`evidence_digest` over the canonical evidence. The home stays on disk; `home_hint` is its path.

**Retention.** The layout is `<launching home>/eval/runs/<run_id>/{cells/<cell_id>/pack/,
trials/<trial_id>/{home/,workspace/}}`. `gents eval rm <run_id>` (M4) deletes the run directory
after the documents. No cap and no TTL in 2a; `eval show` reports the directory size. A trial
whose home was deleted still has its documents; `recollect` returns `None` and a regrade of it is
refused with a clear error.

**Seed.** The binding's `seed` is `seed_base + trial_index`. Whether a provider honours it is
recorded, not assumed: the plan verifies D4F and OpenRouter and the report notes the finding. A
provider that ignores the seed still gets paired arms; only the variance reduction is lost.

**What moves from `tests/` into `src/`.** Three pieces of #1512 support become library code under
`eval::runner::embedded`: the home factory (`test_db_in`), the terminal-state wait with interrupt
(`observe_request`), and the session evidence read (`retain_request_evidence`), plus the stage
failure classifier from `stages.rs`. The #1512 tests then import them from `src/`, so nothing is
duplicated and `make live-configurator-eval` keeps working. The `tests/` copies are deleted in the
same PR.

## Section 3: the run loop (approved)

*Implements: M2's run freezing, write-once completion, verdict writing, NotEvidence retries, and resume by attempt. The API M6b's driver and M4's `cancel`/`invalidate` call.*

`run(access, request, executor, cancel) -> RunOutcome` and `resume(access, run_id, executor,
cancel) -> RunOutcome`. Both are one async call in the caller's process. `RunOutcome` is the `run_id` and counts
only: trials completed, abandoned, NotEvidence, and whether the breaker tripped. Reports are M4.

*[execution amendment, M2 rulings]* `RunRequest` carries `evaluator_did` (the launching home's
identity, supplied by the caller and never derived from `owner`), request-level `captures`
(persisted in `<run dir>/run.json` beside `breaker_threshold` until `EvalDefinition` gains a
case-level `capture` field, and part of the idempotence comparison), and `freeze` validates
`denominator_policy == DENOMINATOR_POLICY_V1` and `purpose ∈ {"eval", "optimization:<job_id>"}`.
Pack materialization stages into `pack.tmp` and renames after the last byte. `ProviderDown` carries
the partial `RunOutcome` with `breaker_tripped: true`.

**Freeze.** `run` validates the `RunRequest` before it writes anything: the definition exists and
its digest matches; every selected case is on the requested split; each cell resolves to a pack
(the launching home's installed pack for a baseline, a caller-supplied pack directory for a
candidate) and to an inference binding; the binding is not an OAuth subscription; no cell's tools
grant `Unrestricted` bash when the executor is embedded. Then it materializes each cell's pack into
the run directory, and writes the `EvalRun` with its frozen `origin` in one transaction. `run_id`
is caller-chosen. Freeze is idempotent: an existing run with the same `run_id` and an equal
`origin` is reused; an existing run with a different `origin` is refused. An invalidated run is
refused for both `run` and `resume`.

**Plan.** `plan(origin, existing_trials)` yields one `PlannedTrial` per
`(cell, case, trial_index)` that has no completed attempt, with `attempt` one above the highest
existing attempt for that slot. The order is `trial_index`, then `case_id`, then `cell_id`, so the
two arms of a pair are adjacent in the schedule and see the provider at nearly the same time. The
`trial_id` is a digest of `(run_id, cell_id, case_id, trial_index, attempt)`, so a resume that
plans the same slot twice cannot create two rows.

**Execute.** Planned trials run through `buffer_unordered(concurrency)`, default 1. Per trial, in
this order:

1. Write the `EvalTrial` row with a null `completion`. This happens before `execute`, so a crash
   leaves a row that says "attempted, not finished", which is the fact resume needs.
2. `executor.execute(spec)`.
3. `grade(case, evidence, registry)`, then `append_verdicts`.
4. `complete_trial`, the write-once completion. This is the commit point. Verdicts before
   completion means a crash between them leaves verdicts attached to a row that resume abandons,
   and those verdicts are keyed to the abandoned `trial_id`, so they never collide with the retry's.

**Retry and the breaker.** A trial whose evidence class is NotEvidence is re-planned as a new
attempt, up to `max_infra_retries` (frozen in `origin`, default 3), with exponential backoff
starting at 5 seconds and capped at 60. A run-level counter tracks consecutive NotEvidence trials
across all slots; at `breaker_threshold` (frozen in `origin` beside `max_infra_retries`, default 5,
*[self-review]* an additive key in the `origin` JSON) the loop stops launching, waits for in-flight trials, and
returns a typed `ProviderDown` error. The run stays resumable. A completed trial resets the
counter. A Fail never retries: it is the subject's result.

**Cancellation.** `cancel` is a token the caller owns. On cancel the loop stops launching,
interrupts each in-flight trial's current request through the existing interrupt owner, waits the
grace period, and returns. In-flight trials keep their null completion and are abandoned by the next
resume. Cancellation is in-process only in 2a: a run is hosted by the calling process, and no other
process holds the trial nodes. `gents eval cancel <run_id>` (M4) therefore signals the hosting
process; a cross-process cancel document is a 2b item beside the reconciler.

**Invalidation.** `invalidate_run(run_id, by, reason)` sets `invalidated` and nothing else. The
runner refuses to resume it. Reports, `compare`, and exposure counts exclude it. It is never
deleted by invalidation; `eval rm` is the only deletion.

**Two cells, one loop.** The optimization driver (M6b) calls the same `run` twice per round: a
train run with one cell, and a validation run with two cells sharing `seed_base`. It reads
`RunOutcome` to decide whether the round has enough evidence, then reads verdicts through the M1
document functions. The runner does not know it is being driven.

**Failure of the launching home.** A write to the launching home that fails (the node is down,
the disk is full) is not a trial outcome. `run` returns the error, the trial that was in flight
keeps its null completion, and resume repairs it. The runner never converts its own I/O failures
into `infrastructure` verdicts, because that would count the harness's fault against the subject.

## Section 4: testing and phasing (approved)

*Implements: M2's canary test, and the PR stack the M2 implementation plan is written from.*

**Four test layers, cheapest first.**

1. *Pure unit tests* on `plan.rs`, `grade.rs` and `ScriptedExecutor`: the trial matrix, resume
   numbering, deterministic `trial_id`, pair-adjacent ordering, `skipped_prerequisite` rows for
   stages that never ran, and the rule "latest completed attempt wins; an abandoned attempt is
   `infrastructure`" as one table-driven test. No Lean is added; the outcome vocabulary these tests
   write is already proven in M1.
2. *Runner tests on `ScriptedExecutor`* against an embedded launching home: freeze idempotence and
   refusal on a changed origin; a simulated crash after step 1 and after step 3 of the per-trial
   order, then resume (*[self-review]* simulated by a `ConfigAccess` wrapper that fails the
   chosen write, which section 3 already defines as "return the error, leave the row null"); the breaker tripping at the threshold and the run resuming after; cancel
   abandoning in-flight trials; an invalidated run refusing resume; verdicts written before the
   completion; two cells sharing `seed_base`; `concurrency = 4` producing the same documents as
   `concurrency = 1`. This layer is what M6b's scripted matrix reuses.
3. *The canary test* on `EmbeddedExecutor` with `MockStreamingBackend`: one real embedded trial
   home, a scripted model, one document fixture and one file fixture, two stages where the second
   is skipped in the failing variant, a document capture and a file capture, a stable
   `evidence_digest` across two identical runs, and the freeze-time refusals for `Unrestricted`
   bash and an OAuth binding. The canary uses a hand-built `TrialSpec`, so it does not wait for
   M3's `capture` field on `EvalDefinition`.
4. *A live smoke test*, ignored by default and gated by the same environment variables #1512 uses:
   one trial on D4F or OpenRouter. It records whether the provider honoured `seed` and prints the
   finding.

**The minimal M3 that M2 needs.** The umbrella lists "a minimal M3" as M2's dependency. It is
one named check so the canary can grade: `captured_rows_count {name, min}` over a document
capture, registered in `eval::checks` as the seed of spec 3's registry. Spec 3 owns everything
after that.

**PR stack.** Each PR targets its parent and passes `cargo test -p gents` and
`cargo check --workspace --all-targets` on its own.

| PR | Content | Risk |
|---|---|---|
| 1 | Move the #1512 support into `src/`: the home factory, the wait with interrupt, the session evidence read, the stage failure classifier, under `eval::runner::embedded`. The #1512 tests import them; the `tests/` copies are deleted. No behavior change | Largest diff, lowest risk. `make live-configurator-eval` is the check |
| 2 | `executor.rs` types, `ScriptedExecutor`, `plan.rs`, `grade.rs`, with layer-1 tests | Pure code |
| 3 | `freeze.rs`, `record.rs`, `mod.rs` with `run` and `resume`, the retry breaker and cancel, with layer-2 tests | The runner's logic; the crash-resume tests are the gate |
| 4 | `EmbeddedExecutor`, the `captured_rows_count` check, the canary and the live smoke | The only PR that boots a runtime |

**M2 acceptance.** `run` on `ScriptedExecutor` survives every crash point with resume; the canary
passes on a clean checkout with no network; a live smoke trial completes on one real provider;
`make live-configurator-eval` still passes after PR 1.

**Out of scope, with a home.** The CLI and reports (M4). The `capture` field on
`EvalDefinition` and every check beyond the seed (M3, spec 3). A process-per-trial executor, an
enforced evaluator DID, a runner-hosted inference proxy and cross-process cancel (spec 2b).
Retention TTLs (spec 4b).

