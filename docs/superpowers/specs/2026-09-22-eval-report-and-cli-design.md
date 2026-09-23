# Eval report and CLI design (issue #1515, spec 4a, milestone M4)

Status: design, approved 2026-09-22 after six decisions. Umbrella:
`2026-09-21-eval-and-optimization-umbrella.md`. Contract: `2026-09-21-eval-core-contract-design.md`.
Runner: `2026-09-21-eval-runner-design.md`. Optimization: `2026-09-21-optimization-on-eval-design.md`
(its planning amendments place verdict projection in `gents::eval::report` and the optimization
commands in this spec).

## Goal

Give the operator a window onto runs, trials, comparisons and optimization jobs: a versioned report
derived from documents, and thin commands over the library functions M2 and M6b provide.

## Decisions

| Question | Decision | Why |
|---|---|---|
| What the report is | Derived on demand by a pure function over the documents, with a `report_version`; never stored | A report can never disagree with the documents; a regrade changes it with no migration |
| What `compare` prints | Paired statistics always; a `PolicyV2` verdict only with `--policy`, labelled uncalibrated while defaults are placeholders | Operators judging a hand edit need the numbers; the optimizer is the only thing that decides by default |
| How trials are counted | The slot `(cell, case, trial_index)` is the unit, classified by its latest attempt; attempts are a separate total; `RunOutcome` is never printed | A NotEvidence retry must not inflate "completed"; a crash and an outage must stay distinguishable |
| Cross-process cancel | A marker file `<run dir>/cancel` the loop checks where it checks its own token | Works across processes on one machine with no control document; remote cancel stays 2b |
| What `rm` deletes | The run directory (homes, packs, sidecars); documents stay; refuses an unfinished run without `--force` | Disk is what the operator needs back; exposure counts and comparisons cite the documents |
| Commands in 4a | `eval run, resume, list, show, trial, compare, cancel, invalidate, rm`; `optimization run, show, promote, revert` | Everything the umbrella names plus the two entry points needed to drive a job from a shell |

## 1. The report module: `gents::eval::report`

Pure. No I/O. Extends the `report::evidence` submodule M6b creates (latest attempt per slot,
`VerdictRecord → VerdictView`, pairing, token totals) so the optimizer and the operator read the
same numbers.

```rust
pub fn build(run: &RunRecord, trials: &[TrialRecord], verdicts: &[VerdictRecord], definition: &EvalDefinition) -> Result<EvalReport>;
pub fn compare(baseline: &EvalReport, candidate: &EvalReport, baseline_cell: &str, candidate_cell: &str) -> Result<Comparison>;
```

`EvalReport { report_version: 1, run: RunSummary, cells: Vec<CellReport>, exposure: usize }`.
`RunSummary { run_id, definition: DefinitionRef, split, purpose, created_at, invalidated: Option<Invalidation>, trials_per_case, seed_base, concurrency, max_infra_retries, breaker_threshold, source_commit, source_dirty }`.
`CellReport { cell_id, label, subject, slots: Vec<SlotReport>, cases: Vec<CaseReport>, headline_bp: Option<u32>, usage: TrialUsage, attempts: u32, counts: SlotCounts }`.
`SlotReport { case_id, trial_index, class: SlotClass, attempts, score_bp: Option<u32>, latest: Option<AttemptSummary> }`,
`SlotClass { Pass, Fail, Unknown, NotEvidence, Abandoned, Planned }`,
`AttemptSummary { trial_id, attempt, trial_agent_did, session_id, home_hint, evidence_digest, stages: Vec<StageCompletion>, usage: TrialUsage }`.
`CaseReport { case_id, reducer, mean_bp: Option<u32>, counts: SlotCounts }`. `SlotCounts { pass, fail, unknown, not_evidence, abandoned, planned }`.

Slot classification: no row → Planned; latest row with null completion → Abandoned; otherwise the
case-trial class of the latest completed attempt through `case_trial_score` (NotEvidence, Unknown,
or Scored, which is Pass when every acceptance verdict classifies Pass and Fail otherwise). A slot
whose latest completed attempt is NotEvidence but whose attempt count is below the cap is still
NotEvidence in the report; the runner, not the report, decides whether it will be retried.

`Comparison { comparability: DefinitionRef, cases: Vec<CaseComparison>, improved: u32, tied: u32, worsened: u32, mean_diff_bp: i64, pairs: usize, dropped_baseline: usize, dropped_candidate: usize, imputed: usize, p_ppm: Option<u64>, policy: Option<PolicyOutcome> }`.
`p_ppm` is `optimization::policy::permutation_p_ppm` over the same pairs the policy would use.
`compare` refuses two reports whose `definition.digest` or `comparability_version` differ. Improved,
tied and worsened count cases by the sign of the per-case mean difference; zero-pair cases are
excluded and counted in `dropped_*`.

`PolicyOutcome` is `optimization::policy::decide`'s output plus `calibrated: bool`, false while the
policy in use equals the placeholder defaults.

## 2. Commands

Every command resolves the launching home the way existing `gents` commands do, calls one library
function, and prints either a table or, with `--json`, the report structure. Exit status is 0 on
success, 1 on a refusal the library returned (printed verbatim), 2 on usage.

| Command | Library call | Output |
|---|---|---|
| `eval run <definition_id> --cell <id>=<pack>[:<behavior>] … [--split validation] [--trials 2] [--seed-base N] [--concurrency 1] [--purpose eval] [--run-id ID]` | `runner::freeze` then `runner::run` on `EmbeddedExecutor` | One line per completed slot as it lands; the `show` table at the end. Ctrl-C cancels. `--profile <inference_profile_id>` per cell, default the home's default profile. |
| `eval resume <run_id>` | removes `<run dir>/cancel`, then `runner::resume` | As `run`. |
| `eval list [--definition ID] [--all]` | `load_runs` | `run_id`, definition, split, purpose, created, invalidated, slot counts per cell. Invalidated runs hidden without `--all`. |
| `eval show <run_id> [--json]` | `report::build` | The six slot counts per cell, per-case means, headline, usage totals, attempts, exposure. |
| `eval trial <run_id> <cell> <case_id> [<trial_index>] [--json]` | `report::build` + `load_verdicts` | The latest attempt: stages with terminal state and kind, every verdict with check, kind, score, reason code, the home path, `evidence_digest`. |
| `eval compare <baseline_run> <candidate_run> [--baseline-cell ID] [--candidate-cell ID] [--policy defaults\|<path>] [--json]` | `report::compare` (+ `decide`) | The `Comparison`; with `--policy`, the gate-by-gate result and a banner `policy defaults are uncalibrated until the A/A calibration (M5)` when `calibrated` is false. |
| `eval cancel <run_id>` | writes `<run dir>/cancel` | Confirms the marker; says the hosting process stops at its next check. |
| `eval invalidate <run_id> --reason TEXT` | `invalidate_run` with `by` = the home's identity | Confirms; a second call prints `AlreadyInvalidated`. |
| `eval rm <run_id> [--force]` | deletes `<run dir>` | Refuses when the run has Planned or Abandoned slots unless `--force`; prints bytes reclaimed. |
| `optimization run <definition_id> --subject <pack>[:<behavior>] [--rounds 3] [--trials 2] [--seed-base N] [--policy defaults\|<path>] [--job-id ID]` | `optimization::run_job` with `ScriptedProposer` only when `--proposer scripted:<file>` is given, otherwise refused until an LLM proposer exists | Round lines as they land; the `show` table at the end. |
| `optimization show <job_id> [--json]` | `optimization::show` | Journal in order, state, per-round decision with the numbers behind it. |
| `optimization promote <job_id> --digest D` | `optimization::promote` | Confirms or prints the refusal (`StaleExpectation`, wrong digest, foreign owner, not ready). |
| `optimization revert <job_id>` | `optimization::revert` | Confirms; `Reverted` is final. |

`optimization run` in 4a therefore drives a job with a scripted proposer supplied as a file; that is
enough for the live accept and reject scenarios and for M5. The LLM proposer is M7.

## 3. The cancel marker

A one-file change to the runner: at every point the loop consults its cancel token (before each
launch, between trials), it also checks `<run dir>/cancel` and treats its presence as cancellation.
`resume` deletes the marker before planning. `eval run` installs a Ctrl-C handler that cancels the
token. The marker is not a document and carries no state; a remote host cannot see it (2b).

## 4. Testing

- Unit tests on `report::build`: every `SlotClass` reached from hand-built documents; a regrade
  (appended verdict with `regrade_of`) changes the slot's score; attempts and slots counted
  separately; exposure counts non-invalidated runs only.
- Unit tests on `report::compare`: improved/tied/worsened on hand-built pairs; dropped and imputed
  counts match `pair_trials`; `p_ppm` equals `permutation_p_ppm` on the same pairs; mismatched
  digest or comparability version refused; `--policy` output equals `decide` on the same evidence.
- CLI tests in `crates/gents-cli` against an embedded home with a run produced by
  `ScriptedExecutor`: each command's table and `--json` output; `cancel` observed by a loop running
  in the test; `rm` refuses then forces; `compare --policy defaults` prints the banner;
  `optimization show` after a scripted job. No live model.

## 5. Phasing

| PR | Branch | Content |
|---|---|---|
| 1 | `eval/50-report` on M6b's `optimization/22-driver` tip | `report::{build, compare}` and types; the cancel marker in the runner; unit tests |
| 2 | `eval/51-cli` on `eval/50-report` | the nine `gents eval` commands and their tests |
| 3 | `eval/52-optimization-cli` on M6b's `optimization/23-promote` tip rebased over `eval/51-cli` | the four `gents optimization` commands and their tests |

## Out of scope, with a home

Live scorecard and watcher, retention TTLs, a richer compare with per-stage breakdowns (spec 4b).
Remote cancel (2b). The LLM proposer behind `optimization run` (M7).
