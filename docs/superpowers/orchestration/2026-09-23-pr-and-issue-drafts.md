# PR and issue drafts for the MVP stack (NOT published; review first)

All PRs relate to #1515 (native eval) and, from PR 4 on, #1455 (native configuration optimization).
Only the last PR of the stack says "Closes"; the others say "Part of".

---

## PR 1 — `feat/guarded-publication` → `main`

**title:** Guarded desired-state publication: digest expectations with a Lean precondition

**Closes#:** Part of #1455 (promotion must refuse to overwrite a changed baseline).

**What**
A desired-state publication can carry expectations about the current digest of the documents it
touches, and refuses inside the write transaction when any of them drifted. This is the primitive a
later "promote an optimized configuration" step uses to never overwrite a baseline that changed
underneath it.

**What changes**
- `Proofs/ApplyReconcile/Publication.lean`: `publishIf`, the digest-guarded precondition, with its
  idempotence and refusal theorems; README proof map.
- Conformance: seven `publishIf` cases emitted into the contract snapshot and consumed by a
  Lean-driven Rust test (`lean_vocab_test/publish_if.rs`, ledger row flipped to consumer coverage).
- `config_client/desired_state.rs`: `DesiredStateApplyPlan::with_expected`,
  `DesiredStateExpectation`, a typed `StaleExpectation` error recovered by `stale_expectation`, and
  `ensure_expectations_hold` as the first statement of `apply_desired_state_plan`; the check runs
  on the same transaction handle as the writes on both `ConfigAccess` variants.
  `desired_state_document_digest` becomes `pub` so a caller can build an expectation.

**Verification**
`lake build`; `cargo test -p gents --lib config_client::desired_state` (17, including the
Lean-driven `guarded_publication_matches_lean_publish_if_cases` and a replay-idempotence witness);
conformance suite; `cargo test -p gents` on the tip (only the known libp2p timeout);
`cargo check --workspace --all-targets`; `cargo fmt --all --check`.

**Notes**
- Observation, unchanged here: `pack install --digest` compares its artifact digest outside the
  write transaction, so drift between that check and the install is undetected.
- Expectation digests must come from a live read of the normalized projection; for
  `ChainKeyBinding` the authored and live forms differ by design.

---

## PR 2 — `feat/eval-contract` → `feat/guarded-publication`

**title:** Eval core contract: outcome vocabulary, four fact-only collections, scoring and documents

**Closes#:** Part of #1515.

**What**
The shared contract every eval producer and consumer uses: a closed outcome vocabulary proven
against a Lean model, an `EvalDefinition` configuration collection carried by packs, three fact-only
runtime collections, integer scoring with paired evidence, typed documents with write-once
completion, and a rule that no agent tool may reach an eval collection.

**What changes**
- `Proofs/Eval.lean`: eleven outcome kinds, provider reasons, the projection onto
  Pass/Fail/NotEvidence/Unknown and `subjectCausable`; conformance emits all 33 kind×reason cases.
- `EvalDefinition` joins `Collection`, `PackConfig`, the schema catalogs and the migration baseline;
  local-audit, never replicated, not a `SelfConfigTarget`. Cases are embedded JSON.
- `EvalRun` (frozen `origin`, only `invalidated` mutable), `EvalTrial` (identity at provisioning,
  write-once `completion`, `attempt`, shared `seed`), `EvalVerdict` (append-only, `regrade_of`,
  `score_bp`, `weight`, feedback only on train). All `@branchable`, no lifecycle field.
- `gents::eval::{outcome, scoring, documents}`: `classify`; reducers `weighted_mean` (truncating),
  `all` (by class), `last_stage`; zero weight disables a check; pairing by `(case, trial_index)`
  with NotEvidence dropped and Unknown imputed worst-case for the candidate; write-once completion
  and invalidation inside one transaction; feedback refused off the train split.
- `PROTECTED_DATASTORE_COLLECTIONS`: surface validation, both bounded tools and
  `CollectionScope::ensure_allowed` refuse eval names and the reserved `OptimizationJob` under any
  scope, including the unrestricted query endpoint, MCP default scope and `gents query`.

**Verification**
`lake build`; conformance 269/269 (the consumer drives every emitted case through production
code); `cargo test -p gents --lib eval` (33), `document_config`, `defra_query`, `gents-schemas`,
`gents-migration` (pins authored by the pin test); `cargo test -p gents` on the tip;
`cargo check --workspace --all-targets`; `cargo check -p gents --features typescript`.

**Notes**
- `EvalRun` and `EvalTrial` carry no message bodies; no P2P profile subscribes them in this PR.
  Their JSON payloads are closed types; the only free text is operator-authored (`CellSpec.label`,
  `Invalidation.reason`).
- Follow-ups recorded: `graph_pipeline` capability ports validate collections as identifiers only;
  Lean case-class witnesses when the reducers gain a conformance consumer.

---

## PR 3 — `feat/eval-runner` → `feat/eval-contract`

**title:** Eval runner: one fresh embedded home per trial, resumable runs, deterministic grading

**Closes#:** Part of #1515.

**What**
`gents::eval::runner`: freeze a run, plan its slots, execute each case-trial in a fresh embedded
DefraDB home through an injectable `TrialExecutor`, capture evidence from outside the trial, grade
it with named checks, and write trial and verdict rows. Crash-safe by construction: the documents
are the only state, and `resume` re-plans from them.

**What changes**
- The #1512 test support (trial homes, terminal-state wait with interrupt, session evidence,
  failure classification) moves into the library; the tests keep thin wrappers.
- `TrialExecutor` (`isolation`, `provision`, `execute`, `recollect`, `discard`), `TrialSpec` (which
  structurally excludes checks, tiers, splits and case ids), `TrialEvidence` with a per-trial
  integrity digest; `ScriptedExecutor` for tests; `plan` with deterministic trial ids and
  pair-adjacent ordering; `grade` with `skipped_prerequisite` and `infrastructure` rows; the seed
  check registry.
- `freeze` (validation, pack materialization by digest, frozen origin, idempotent on `run_id`),
  `run`/`resume` (verdicts before the write-once completion, NotEvidence retries under a cap,
  abandoned attempts outside it, a consecutive-NotEvidence breaker returning `ProviderDown`,
  cancellation), the `Recorder` seam for crash tests.
- `EmbeddedExecutor`: fresh home and identity, pack install with inference-slot binding, the
  inference binding with the pair's seed, workspace root, fixtures, one session, stages as ordinary
  `AgentRequest`s with deadline interrupt, document and file captures, node shutdown on every path.
  Under the embedded executor a subject may grant no host bash at all.
- The canary: a real embedded trial on a scripted model, both freeze refusals, `recollect`.

**Verification**
`cargo test -p gents --lib eval` (105); `cargo test -p gents --test eval_runner_canary`
(3 passed, 1 ignored live smoke); `cargo test -p gents` on the tip; `cargo test -p gents
--test e2e_configurator --no-run` (the #1512 harness still compiles); `cargo check --workspace
--all-targets`.

**Notes**
- Cancellation is in-process here; the cross-process marker arrives with the CLI PR.
- Deviation from #1515's "one process per trial": embedded first, process-per-trial designed as a
  later executor behind the same seam.
- `breaker_threshold` and `evidence_digest` are carried in the run directory here and become
  contract fields in a later commit of this stack.

---

## PR 4 — `feat/optimization-policy` → `feat/eval-runner`

**title:** Optimization policy: Lean-modeled gates and the paired sign-flip permutation test

**Closes#:** Part of #1455.

**What**
`PolicyV2`, the pure decision function an optimizer applies to paired eval evidence: sufficiency,
non-regression with a token-cost bound, and a one-sided sign-flip permutation test with Bonferroni
correction across rounds. Integer arithmetic throughout, proven equal to `Proofs/Optimization.lean`.

**What changes**
- `Proofs/Optimization.lean`: the integer gates over per-case basis-point sums, the ordered
  decision with its cost sub-gate, `accept_monotone`, and the length-guarded journal model.
- Conformance emits `optimization_cases` (params, decisions, gates, costs); a Rust consumer drives
  every case through `decide`.
- `gents::optimization::policy`: `PolicyV2` (all defaults marked uncalibrated),
  `evidence_from_pairs`, `permutation_p_ppm` (exact enumeration up to 20 cases, seeded Monte Carlo
  above, checked arithmetic with a typed `Inconclusive` on overflow), `decide`.

**Verification**
`lake build`; conformance (structure, coverage, feature matrix); `cargo test -p gents --lib
optimization` (11 plus a proptest); `cargo test -p gents` on the tip; `cargo check --workspace
--all-targets`.

**Notes**
- Defaults are placeholders until an A/A calibration sets them; at alpha 50 000 ppm and three
  rounds a job needs at least six cases to ever accept.
- `max_missing_usage_bp` is consumed by the driver, not by `decide`.

---

## PR 5 — `feat/optimization-driver` → `feat/optimization-policy`

**title:** Optimization driver: job journal, proposer and gate, rounds over eval runs, promote and revert

**Closes#:** Part of #1455.

**What**
The loop: an `OptimizationJob` document with an append-only journal, a `Proposer` trait with a
scripted implementation, a structural gate, a driver that runs train and validation runs per round
through the eval runner and decides with `PolicyV2`, and `promote`/`revert` through the guarded
publication.

**What changes**
- `OptimizationJob` collection (local-audit, protected), typed `JobOrigin` and ten journal entry
  kinds refining the Lean journal model with a length guard.
- `Proposer`/`ScriptedProposer`, candidate materialization as a directory pack differing from the
  baseline in exactly one field (`AgentContext.system_prompt`), the structural gate.
- `run_job`: freeze the baseline with a closure check, one train run, propose, gate, a two-cell
  validation run with shared seeds, `decide`, journal; held-out confirmation on accept; resume from
  the journal, refusing a request that disagrees with the frozen origin; the scripted matrix
  (accept, three reject kinds, structural reject, budget exhaustion, stale baseline, resume twins
  at every run boundary, invalidated runs). `eval::report::evidence`: latest attempt per slot,
  pairing, token totals.
- `promote`/`revert` over `with_expected`, refusing a stale baseline, a wrong digest, a foreign
  owner or a job that is not ready; two live scenarios, ignored by default.

**Verification**
`cargo test -p gents --lib optimization` (119), `--lib eval` (114); conformance 269/269;
`cargo test -p gents` on the tip; `cargo check --workspace --all-targets`.

**Notes**
- The proposer holds no database access and sees only per-check feedback from the train split.
- Captures passed on the job request are a request-level fallback; the definition's stage captures
  supersede them.
- Open item: a concurrent edit to a read-only closure document during `promote` is caught only if
  DefraDB detects read-write conflicts at commit; the target itself is always digest-guarded.

---

## PR 6 — `feat/eval-cli` → `feat/optimization-driver`

**title:** Eval reports and CLI: `gents eval` and `gents optimization` over derived reports

**Closes#:** Closes #1515. Part of #1455.

**What**
The operator surface. A versioned report derived on demand from the documents, a paired
comparison with an optional policy verdict, and thin commands over the library: `gents eval run |
resume | list | show | trial | compare | cancel | invalidate | rm | watch | gc` and `gents
optimization run | show | promote | revert | rm`.

**What changes**
- `eval::report`: `build` (slots classified by their latest attempt: pass, fail, unknown,
  not_evidence, abandoned, planned), `compare` (improved/tied/worsened, mean difference,
  dropped/imputed pairs, the policy's own p-value; `--policy` runs `decide` with an uncalibrated
  banner), breakdowns by check, stage and case; `report::store` as the module's I/O boundary.
- Runner additions: freeze materializes `definition.json` so a report survives a later edit of the
  definition; a cross-process cancel marker checked on a timer; `progress.json` with heartbeat and
  liveness for the watcher; `RunOutcome.cancelled`; abandoned attempts outside the retry cap.
- The commands, with table and `--json` output, Ctrl-C handling, exit codes, and refusals printed
  verbatim. `rm` and `gc` delete run directories only, never documents, and never a run a live job
  holds. Operator-facing text names no internal milestones.

**Verification**
`cargo test -p gents` 3336 passed / 0 failed; `cargo test -p gents-cli` 1107 passed with 20
known environmental failures (18 missing wasm toolchains, one live endpoint, one pre-existing
flake); `cargo test -p gents --test eval_runner_canary`; conformance 269/269;
`cargo check --workspace --all-targets`; `cargo fmt --all --check`.

**Notes**
- No live model is required by any test in this stack; the live tests are `#[ignore]`.
- Follow-ups recorded as issues: the reader-blocks-while-unpolled hazard (contained in the CLI),
  `validate_job_id` visibility, a `PolicyV2` placeholder predicate.

---

## Side branch — `test/monitor-findings-eval` (HELD: not a PR; the user keeps it as the base for the self-improving loops that follow. Draft retained for later.)

**title:** Monitor-findings eval: subject pack, twenty cases, nine checks and the definition pack

**Closes#:** Part of #1515.

**What**
The first decision-grade definition: a fixed, bash-free monitor subject; twenty independent cases
on 8/6/6 splits; nine deterministic checks; a definition pack whose cases are sidecar files. The
definition is loader-validated and scripted-tested; `comparability_version` 1 is provisional until a
calibration run on a live model.

**What changes**
- Contract additions: `EvalStage.capture` (per-stage document and file captures with an explicit
  `fields` list), inline `fixtures.files`, duplicate check-ref validation; the runner reads stage
  captures and inline files; the trial's Tools root resolves to its workspace.
- `eval::checks`: `mailbox_item_count`, `mailbox_open_row`, `payload_well_formed`,
  `finding_pairs_unique`, `finding_count`, `finding_state_counts`, `findings_match`,
  `mailbox_text_excludes`, `finding_text_excludes`; registry version 2.
- `packs/eval_monitor` (the subject) and `packs/eval_monitor_findings` (the definition; cases as
  `./cases/<id>.json` sidecars, the loader rule, the conversion script and its test).

**Verification**
`cargo test -p gents --lib eval` (checks and loader); the scripted-executor integration test
asserting exact verdict rows on two cases; the conversion reproduces the committed cases byte for
byte; `cargo test -p gents` on the tip (only the known libp2p tests).

**Notes**
- The calibration run and its fix pass are deferred to a machine with a live model; the handoff
  brief is in the design notes.

---

# Issue drafts (What / Plan), all to be linked to #1515

## I1 — (DROPPED by the user: pre-existing on `main`, untouched by this work, out of scope) libp2p dial timeouts fail two P2P e2e tests on macOS
**What:** `generated_r5_cross_principal_cases_drive_production_dispatch` (conformance) and
`e2e_triggers::event_source_trigger_p2p_e2e::p2p_replicated_doc_fires_event_trigger` fail on
unmodified `main` on a macOS dev machine with `Transport(timeout dialing …)`, and pass elsewhere.
**Plan:** Reproduce with libp2p debug logging; check whether the dial targets a loopback address the
listener never binds on macOS; either fix the transport setup in the test support or mark the two
tests as requiring a network capability, with a skip that names the reason.

## I2 — DefraDB "directory is already locked" flake in `cli_config_tools`
**What:** `tools_set_persists_host_root_and_export_round_trips_it` fails intermittently in the full
`gents-cli` run with a DefraDB lock error and passes alone: two tests share a data directory or one
does not release the store before the next opens it.
**Plan:** Give each test its own tempdir home; audit the CLI test fixture for shared paths; add a
typed store-lock error so the message names the holder.

## I3 — Timing flakes under load in `e2e_triggers` and `cli_runtime`
**What:** `event_trigger_fires_on_source_doc_create_end_to_end` timed out "waiting for runtime
snapshot" once under a full parallel run; `cli_chat::chat_buffers_final_response_and_shows_tool_progress`
failed once posting to a local port during an overlapping build. Both pass on rerun.
**Plan:** Raise the snapshot wait to a load-tolerant bound or poll to a condition; make the CLI
test choose a free port rather than a fixed one; run both in the sequential lane if the harness has one.

## I4 — CLI scenario sidecar closure has no `..` check
**What:** The `gents-cli` pack scenario sidecar closure resolves paths without rejecting `..`
components. It predates the eval work, but eval definition sidecars now pass through it.
**Plan:** Reuse the library loader's canonical-asset-path check in the CLI closure; add a test with
a `../` path refused.

## I5 — A reader of the launching home's documents can block while the runner future is unpolled
**What:** Both CLI follow loops hit a hang when reading documents while the runner or driver future
was not being polled; contained by restructuring the loops, root cause unconfirmed (suspected: the
local `MutationWriteGate` held across an await).
**Plan:** Reproduce with a minimal test; trace the gate's acquire/release around `transact`;
either make reads bypass the gate or make the gate an async mutex released across awaits.

## I6 — Small optimization API follow-ups
**What:** `optimization::driver::validate_job_id` is private, so the CLI duplicates a blank check;
`PolicyV2` has no "is placeholder" predicate, so the report's calibrated rule and the job banner can
disagree; `empty_activation_state` in `gents-cli` `http/router.rs` is dead in non-test builds.
**Plan:** Make `validate_job_id` public; add `PolicyV2::is_placeholder()` ignoring `max_rounds`
and use it in both places; remove or gate the dead function.

## I7 — Calibrate the monitor-findings definition on a live model
**What:** The definition is static and scripted-tested; no trial has run on a real model. Unknown:
whether a no-condition input yields an empty-findings row or no row, and whether matcher keywords
survive the subject's own `condition` spellings. `comparability_version` 1 is provisional.
**Plan:** On a machine with a live provider, run the deferred calibration (one cell, one trial per
case, three runs by split), apply the fix pass (the two `mailbox_item_count` assertions, any
brittle matcher), record the final pack digest, and mark version 1 final.

## I8 — A/A calibration for the eval policy
**What:** `PolicyV2`'s thresholds are placeholders. The eval has never been run against itself.
**Plan:** Run `monitor-findings` with the baseline in both cells; confirm `compare --policy`
rejects; measure between-case and within-case variance; set trials per case, case counts and the
detectable effect; commit the calibrated defaults with the run ids as evidence.

## I9 — First live optimization job
**What:** The loop has only run on scripted evidence; the two live scenarios are ignored.
**Plan:** After I7 and I8, run one job with a scripted proposer producing one accept and one reject,
promote the accept, verify the guarded write, and record the journal.

## I10 — Verify promotion against concurrent closure edits
**What:** A concurrent edit to a read-only, non-target closure document during `promote` is caught
only if DefraDB detects read-write conflicts at commit; the target itself is always digest-guarded.
**Plan:** Write a test that edits a closure document between the read and the commit; if DefraDB
does not refuse, extend the expectation set to the closure documents.

## I11 — Are `graph_pipeline` capability ports a datastore surface for eval collections?
**What:** The protected-collection rule covers every agent-reachable datastore tool and query
scope. `graph_pipeline` capability ports validate collections as identifiers only and are outside
that surface.
**Plan:** Decide whether the port path is trusted; if not, apply `reject_protected_collection_name`
there with a test.

## I12 — Process-per-trial executor and enforced evaluator identity
**What:** Trials run as embedded homes in the runner's process; the evaluator DID is recorded, not
enforced; subjects may have no host bash. Porting the host-dependent #1512 suites needs a
process-or-container executor behind the same `TrialExecutor` seam, a runner-hosted inference
proxy, and cross-process cancel.
**Plan:** Design the executor over the existing `gents serve` + host-control recipe; enforce the
evaluator DID through ACP; re-admit sandboxed workspace bash modes where enforcement is provable.
