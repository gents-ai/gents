# Native evaluation and configuration optimization: umbrella (issues #1515, #1455)

Status: design, approved section by section on 2026-09-21. Baseline: `main` at `0deb7659c`.

This document fixes the shared vocabulary, the component boundaries, the decisions that span more
than one sub-project, and the build order. Each sub-project has its own spec.

| # | Sub-project | Spec |
|---|---|---|
| 1 | Eval core contract | `2026-09-21-eval-core-contract-design.md` |
| 2 | Eval execution (runner) | not yet written; decisions already fixed are in section 4 |
| 3 | Checks and pack-described definitions | not yet written; section 4 |
| 4 | CLI and reporting | not yet written; section 4 |
| 5 | Optimization on the eval contract | `2026-09-21-optimization-on-eval-design.md` |
| 6 | Invalid-tool-call budgets; subjective grading | not yet written |

## 1. Why this replaces the earlier design

The first optimization spec (`2026-09-21-optimization-substrate-design.md`) treated evaluation as
someone else's contract behind an `Evaluator` trait, with a throwaway adapter over the #1512 test
harness. We now own #1515, so that premise is gone. Evaluation is designed here as a product, and
optimization becomes one of its consumers.

A research round surveyed Inspect AI and METR, the agent benchmark harnesses (SWE-bench,
Terminal-Bench and Harbor, tau-bench, Aider, OpenHands), the eval platforms (Braintrust, LangSmith,
Promptfoo and others), how harness vendors evaluate their own products, optimizer-coupled evaluation
and its statistics, and eval packaging and grader trust. A synthesis produced options for ten
decisions and a critic reviewed it against the repository. The decisions below cite what that round
found. The research documents are not part of this repository.

## 2. Vocabulary

| Term | Meaning |
|---|---|
| Definition | A document carried by a pack: cases, each case's stages, named checks with parameters, a reducer, split and tier labels, a comparability version. Identified by `(definition_id, comparability_version, content digest)` |
| Check | A Rust function registered by name and parameterized by the definition. It reads trial evidence and returns a raw verdict, a score in [0, 1] and optional feedback. Definitions never contain code |
| Subject | A pack snapshot, identified by digest, plus the `behavior_id` under evaluation, with inference bindings |
| Cell | One point in a run's matrix: a subject, an inference profile and settings. An optimization arm is a cell |
| Case | The independent unit. One fresh trial home. Its stages are internal to it |
| Stage | One ordered step inside a case: a prompt, a deadline and checks |
| Trial | One execution of one case in one cell. `trial_index` is the repetition |
| Run | One frozen invocation: a definition, a split, cells and a trial count, under an evaluator identity |
| Verdict | One check's result for one trial. Append-only |

These extend the #1512 harness's words (suite, case, stage, trial, cohort). A #1512 suite becomes a
definition; its prerequisite-chained cases become the stages of one case.

## 3. Decisions that span sub-projects

| Decision | Choice | Basis |
|---|---|---|
| Where run-level documents live | The home that launched the run. `gents eval run` defaults to a dedicated eval home; optimization launches from the operator home, so its journal and its runs share one database | Supported by the survey, on two conditions that the contract adopts: a trial reference can distinguish "gone" from "wrong" and is never a filesystem path; run-level documents hold verdicts, counts and references, never transcripts |
| Definition kinds | One kind: a document carried by a pack. Rust contributes named checks | METR had two runners compliant with one task standard and got different results. Two kinds would need a conformance test forever; one kind has no such problem |
| Subject | A pack snapshot plus `behavior_id`. A candidate is the same snapshot with one field changed through the existing self-config patch owner | No surveyed system binds a configuration document graph as the subject. A slot-path overlay language was rejected because it duplicates an addressing owner the repository already has |
| Where checks run | In the runner, from the launching home, under a distinct evaluator DID | Inside a trial the subject owns the database and has `self_config`, so authorization cannot protect a grader from it |
| Unit of independence | The case. A skipped stage's checks count as failed | Inspect's sample, Harbor's task and SWE-bench's instance isolate per unit. Chained cases are not independent samples and their skips are gameable missing data |
| Document model | Four collections holding facts only: `EvalDefinition`, `EvalRun`, `EvalTrial`, `EvalVerdict` | A status field on a trial would be a second request lifecycle |

## 4. Components

| Module | Owns | Spec |
|---|---|---|
| `gents::eval::contract` | The four documents, the outcome vocabulary, digests, the comparability version. No I/O beyond documents | 1 |
| `gents::eval::runner` | Freezing a run; one fresh home per case-trial; installing the subject pack onto the trial's DID through the existing pack install path; submitting stage prompts as ordinary `AgentRequest`s and awaiting terminal states; reading evidence read-only; running checks in its own process; writing trial and verdict rows to the launching home | 2 |
| `gents::eval::checks` | The named check registry. Checks are pure functions over an evidence view. Also the pack format for definitions | 3 |
| `gents::eval::report` | Reducers, statistics, comparison. A pure function from documents to a versioned report | 4 |
| CLI | `gents eval run \| list \| show \| trial \| compare \| invalidate \| rm \| cancel` and `gents optimization show \| promote \| revert`, as thin wrappers | 4, 5 |
| `gents::optimization` | Proposer, policy, job journal, promotion. A consumer of the eval contract | 5 |

Decisions already fixed for specs 2 to 4, so they are not reopened:

- The runner takes an injectable `TrialExecutor`. A scripted executor produces deterministic trial
  evidence without a model. It serves eval's own tests and optimization's test matrix.
- Trials are embedded in-process homes first, as in the #1512 harness. One `gents serve` process per
  trial, with an enforced read-only evaluator DID, is spec 2b.
- The check vocabulary is closed and Rust-only in v1. WASM modules are the extension point for custom
  graders later, because Gents already sandboxes callback modules.
- The acceptance tier is deterministic checks only in v1. An `llm_judge` check is development-tier: a
  subject's output is untrusted input to a judge model, and #1515 wants subjective grading separate.
- `EvalDefinition` is not a `SelfConfigTarget` in v1. Definitions arrive by operator pack install
  with `--digest`. When the configurator authors definitions (spec 3b), that write path forces every
  check to the development tier.

## 5. Boundaries that hold everywhere

1. **Nothing protected enters a trial.** A trial home receives the subject, fixtures and stage
   prompts. It never receives check names or parameters, expected values, rubrics, tier or split
   labels, `case_id` values, or other cases.
2. **Eval documents have no lifecycle.** A trial's state is the referenced requests'
   `lifecycle_state`. A run's state is derived from its trials. Cancellation goes through the
   existing interrupt owner.
3. **Identity is never a path.** A trial is referenced as `(trial_agent_did, session_id)` plus a
   content anchor. The home's filesystem location is a locator hint.
4. **Optimization is a consumer, not a peer.** It uses the same runs, trials and verdicts as any other
   eval consumer.

## 6. Build order

Track 0 can start now and runs beside everything else: the `publishIf` Lean work and the Rust
compare-and-set (PRs 1 and 3 of `plans/2026-09-21-optimization-substrate-1-foundation.md`). Nothing in
this design changed them.

| # | Milestone | Spec | Delivers | Needs |
|---|---|---|---|---|
| M1 | Contract | 1 | Lean: the `ConfigDocuments` entry for `EvalDefinition` and the kind-to-class projection. The four schemas with `@branchable`, local-audit classification and migration pins. Contract types, digests, the comparability version. The reserved-collection rule for `DatastoreToolSurface`. No execution | — |
| M2 | Runner MVP | 2a | `TrialExecutor` and the scripted executor; run freezing; one fresh embedded home per case-trial; subject pack install onto the trial DID; stage requests; the write-once completion; verdict writing; NotEvidence retries; resume by attempt; the canary test | M1, a minimal M3 |
| M3 | First decision-grade definition | 3a | The named checks it needs, ported from the #1512 Rust graders. A golden monitor subject pack. A `monitor-findings` definition with independent cases on all three splits, at least 6 to 8 of them on validation | M1 |
| M4 | Report and CLI | 4a | `gents eval run \| list \| show \| trial \| cancel \| invalidate \| rm`; a versioned JSON report derived from documents; exposure counts; `compare` with the paired permutation test and improved, tied and worsened counts | M2 |
| M5 | A/A calibration gate | — | The monitor eval run against itself through `eval compare`. Measures between-case and within-case variance, sets trial and case counts, states the detectable effect size. The go/no-go for tuning the optimization policy | M3, M4 |
| M6 | Optimization on eval | 5 | `Optimization.lean` restated with conformance; the permutation policy; journal and driver over runs; `promote`, `show`, `revert`; the scripted matrix in `cargo test`; a live accept and reject | M1 for the pure parts, M2 for the driver, M5 for defaults |

After M1, M2 and M3 proceed side by side. M6's Lean and policy work can start once M1 lands.

**The largest unknown is M3.** The #1512 monitor suites have the configurator author the monitor
inside the trial, as one chained story. A decision-grade eval needs a fixed monitor subject and many
small independent cases, each with its own input and expected findings. Someone has to write roughly
twenty. This is the eval-supply gap the first research round named as the binding constraint. The
case count is M3's acceptance criterion.

**Coexistence.** `make live-configurator-eval` keeps working throughout. Each #1512 suite gets a
deletion handoff when it is ported: its Rust test entry and graders are removed in the PR that lands
its pack definition. Nothing is removed before its replacement runs.

**Deferred, with a home.** Spec 2b: a `gents serve` process per trial and an enforced read-only
evaluator DID. Spec 3b: configurator-authored definitions, WASM graders, porting the other three
suites. Spec 4b: the live scorecard and watcher, retention TTLs, a richer compare. Spec 6:
invalid-tool-call budgets and subjective grading as a separately versioned scoring source. After M6:
the LLM proposer and its evidence projection.

## 7. Open items for the planning stage

- Two in-repo sources disagree on document ACP for the embedded node. The comment in
  `rendered_request.graphql` says ACP is blocked on defradb.rs#1318 because `EmbeddedNode` exposes no
  policy or relationship API. The test `embedded_transaction_carries_the_supplied_did_to_document_acp`
  in `config_client/txn_owner_tests.rs` exercises that API. The test is code, so the comment is
  probably stale. Until that is settled, ACP on eval collections is defense in depth and no guarantee
  in these specs depends on it.
- Whether a reserved-collection list for `DatastoreToolSurface` already exists.
- CLAUDE.md's "Configuration refactor stack" section describes #1430 as in flight. It merged on
  2026-09-11, so adding a `PackConfig` root is not blocked by it.

- Added after the M1 final review (2026-09-22):
  - The protected-collection rule refuses eval names on every agent-reachable datastore path and
    also on the operator paths `gents query` and the MCP default scope. M4's report and CLI read
    eval rows through `gents::eval::documents` over `ConfigAccess`, never through `run_defra_query`.
  - The runner writes eval documents through `gents::eval::documents` directly against the node,
    never through a scoped tool. By design: the runner writes what agents may not.
  - `graph_pipeline` capability ports validate collections as identifiers only. They are outside
    the datastore-tool surface; whether that path is trusted is a spec 3 decision.
  - `RunOrigin.denominator_policy` and `purpose` are unvalidated strings in M1; M2's `freeze`
    validates them (`DENOMINATOR_POLICY_V1`; `eval` or `optimization:<job_id>`).
  - M1 amendment request, two fields: `RunOrigin.breaker_threshold` (M2 carries it in the run
    directory's `run.json`) and `TrialCompletion.evidence_digest` (M2 writes an `evidence.json`
    sidecar per trial). Both are additive JSON keys on frozen typed structs.
  - `grade` on empty evidence yields `infrastructure` rows with a null score (M2 final review C1);
    the plan's "absent stage → skipped_prerequisite" rule applies only when at least one stage ran.
  - Lean proves `caseClass`/`rank` monotone but emits no witnesses; emit case-class cases when the
    reducers gain a conformance consumer.
  - From the M3 pre-work (spec 3a inputs): the case container layout; the MailboxItem capture
    `fields` union (`_docID`, `title`, `summary`, `payload`); a `StageInput::Document` stage kind and
    the trigger-engine overlap behavior that keeps it out of 2a; the keyword-matcher brittleness class.
  - `EvalDefinition::validate` does not reject two refs to the same check on one stage;
    `latest_verdicts` keys on `(stage_index, check)`, so such refs collapse. Spec 3a decides whether
    to forbid duplicates in validation or key verdicts by ref index (M1 is frozen; M2 plan minor).
  - Captures are request-level in M2 (`RunRequest.captures`, persisted in `run.json`) because
    `EvalDefinition` has no case-level `capture` field yet; M3's field supersedes them (M2 ruling R18).
  - For M4 (spec 4a): a report selects the LATEST attempt per `(cell, case, trial_index)` slot, never
    "trials with a completion", because a NotEvidence attempt is completed and carries
    `skipped_prerequisite` verdict rows at score 0. `RunOutcome.completed` includes NotEvidence
    attempts and `not_evidence` is computed only on a planning pass (0 on the breaker and cancel
    error paths); spec 4a settles the count semantics the CLI prints.
  - Integration order (2026-09-22): M3 (`eval/40..42`) and M6b (`optimization/19..23`) both root at
    `eval/13`. M3's PR 1 makes the runner read `EvalStage.capture` per stage; M6b's driver passes
    request-level captures as a fallback. At integration, stage captures win; `freeze.rs` is touched
    by M3 PR 1, M6b, and M4 PR 1 (`definition.json`), so expect one conflict resolution there.
  - Promotion (M6b T41-4): a concurrent edit to a read-only, non-target closure document during the
    promote transaction is caught only if DefraDB detects read-write conflicts at commit; the target
    itself is always digest-guarded. Verify in M5 or as a Track 0 follow-up test.
  - A second libp2p dial-timeout failure on this machine:
    `e2e_triggers::event_source_trigger_p2p_e2e::p2p_replicated_doc_fires_event_trigger`, same class
    as the r5 case. Filing is the user's call.

## 8. Implementation plans

| Milestone | Plan |
|---|---|
| Track 0 | `plans/2026-09-21-optimization-substrate-1-foundation.md`, following its Track 0 execution guide |
| M1 | `plans/2026-09-21-eval-core-contract.md` |
| M6, pure half | `plans/2026-09-21-optimization-policy-and-lean.md` |
| M2, M3, M4, the rest of M6 | not yet planned; each waits for its spec |
