# Lean ↔ Rust conformance audit

Date: 2026-05-19

Branch: `design/conformance-audit-2026-05-19`

Scope: every Lean state machine in `crates/defra-agent/proofs/Proofs/` and the
emitted contract surface in `Proofs/Conformance/Contracts/`, mapped to its
Rust runtime counterpart (or absence thereof) and classified by binding
strength.

Predecessors: `docs/superpowers/audits/2026-05-15-lean-spec-gap-audit.md`
(structural template for the per-machine sections),
`docs/superpowers/audits/2026-05-14-conformance-pipeline-audit.md` (pipeline
shape). The May-15 punch list has closed: #193, #219, #220, #228, #237, #239,
#246, #247, #248 are all merged.

Binding-strength glossary:

- **Fully bound** — Rust calls `assert_state_machine_contract_is_complete(<name>)`
  AND has vocabulary checks for every emitted enum AND consumes the transition
  table (or, for case-only models, drives the runtime through every emitted
  case).
- **Case-only** — Rust consumes specific witness rows but does not assert full
  transition-table coverage.
- **Vocab-only** — Rust mirrors state vocabulary; no transition coverage.
- **Property-bound** — Rust enforces a Lean-proven property at runtime via a
  dedicated check; shape isn't a transition system.
- **Reference-only** — Lean has the model; Rust runtime has no matching shape.
- **Pending** — Rust runtime exists; conformance is missing entirely.
- **N/A** — Lean module is auxiliary composition glue.

## TL;DR

The pipeline is in its strongest state since the May-14 baseline. All three
drift bugs from `2026-05-14-conformance-pipeline-audit.md` are closed (#196
registration + #237 emit + identity ledger rows), and the cross-cutting drift
test at `crates/defra-agent/tests/state_machine_conformance/coverage.rs:391`
now enforces both directions of ledger ↔ snapshot agreement plus zero
unreferenced consumers. #193 landed: `identity_contracts` flipped to
`enforced := true` (`crates/defra-agent/proofs/Proofs/Identity/Conformance.lean:438`)
backed by the structural `AgentBehavior::principal : Arc<AgentPrincipal>`
back-ref in `crates/defra-agent/src/config.rs:31`. ApplyReconcile now fences
the failure-path post-abort externally observed state via
`expected_external_state_after_abort` (#246) consumed at
`crates/defra-agent-cli/src/config_import.rs:996`.

Top three actionable gaps:

1. **EventDelivery transition + convergence cases drive an in-test
   `InMemoryEventDeliverySource`** at
   `crates/defra-agent/tests/state_machine_conformance/event_delivery.rs:117`,
   not the production `DefraWatcher` / `EventSource` / `SubagentSource` loops.
   The ledger marks all three `event_delivery_cases` rows
   (`crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean:334-345`)
   as `consumerCoverage`, which overstates the binding. Only the source-
   instances row (`event_delivery_source_instances_match_runtime`) reads
   directly from production constants via the
   `EventDeliveryRuntimeContract` trait at
   `crates/defra-agent/src/event_delivery_contract.rs:14`.
2. **MCPHealth consumes only the K=1 slice of the emitted cases** and the
   K=1 driver mirrors inline logic instead of calling `run_health_check`
   directly (`crates/defra-agent/src/health_checker.rs:399-450`). The Lean
   K≥2 rows have no consumer; the ledger row at
   `crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean:346`
   is `consumerCoverage` for the whole `mcp_health_cases` domain.
3. **`identity_permission_cases` is still `consumerWithFollowUpCoverage`** at
   `crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean:317-321`.
   The Rust consumer drives the typed `AgentBehavior::principal` routing but
   does not call a runtime permission-decision function or a deployment
   hostability lookup; `expectedActorAllowed` / `expectedPeerAllowed` /
   `expectedActorHostable` / `expectedPeerHostable` are emitted-but-unused.
   This is the last open piece of #193.

The Background Properties subtree (six theorems in
`Proofs/Background/Properties/{Budget,Cancellation,Foreground,Projection,Structure,Unique}.lean`)
is Lean-only — no JSON row, no ledger entry, no Rust witness. Either emit
property witnesses or accept as Lean-only via a `followUpCoverage` row; today
the gap is invisible to the drift test. Recommended next-impl order: §6.

## 1. Request

### What Lean models today

Nine persisted states in `crates/defra-agent/proofs/Proofs/Request/State.lean:16`
with `toDefraDB`/`fromDefraDB?` round-trip at `:31` / `:55` and `HasTerminal`
at `:59`. Relational `Transition` constructors (`claim`, `dedup_lose`,
`begin_inference`, `advance`, `finish`, `fail`, `fail_before_stream`,
`expire`, `interrupt_*`) at `crates/defra-agent/proofs/Proofs/Request/Transition.lean:17`.
Executable `step?` in `Request/Executable.lean`. Local invariants —
`terminal_implies_released_local`, `backend_binding_preserved`,
`origin_preserved`, `transition_produces_coherent`, `claim_requires_ttl_open`,
`claim_with_ttl_bounds_time` — at
`crates/defra-agent/proofs/Proofs/Request/Properties.lean:11`/`:46`/`:63`/`:80`/`:131`/`:160`.

### What is emitted today

`requestMachine` at `crates/defra-agent/proofs/Proofs/Conformance/Contracts/Machines/Request.lean:75`
with 9 state names, terminal projection, 11-action vocabulary, and a
`transitionPairsFromSamples` partition over 15 `RequestContext` samples.
Registered in `stateMachines` at
`crates/defra-agent/proofs/Proofs/Conformance/Contracts/Machines/Catalog.lean:58`.

### Rust consumer state

`lifecycle::tests::request_state_machine_contract_is_complete` at
`crates/defra-agent/src/lifecycle.rs:268` calls
`assert_state_machine_contract_is_complete("Request")`. Vocabulary
fenced by `rust_request_lifecycle_state_vocabulary_matches_lean_model` at
`crates/defra-agent/src/lifecycle.rs:241`. Generated lifecycle cases driven by
`state_machine_conformance::generated_request_transition_cases_cover_lifecycle_policy`
in `tests/state_machine_conformance/request_lifecycle.rs`. Allowlist rows at
`crates/defra-agent/tests/support/conformance_consumers.rs:171` / `:185` /
`:262`.

### Coverage ledger row(s)

`consumerCoverage "state_machine" "Request" "lifecycle::tests::request_state_machine_contract_is_complete"`
at `crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean:138`.
Vocabulary row at `:65`. Lifecycle-case row at `:206`.

### Classification

Fully bound.

## 2. Process

### What Lean models today

Five-state inductive at `crates/defra-agent/proofs/Proofs/Process.lean:12`
with `HasTerminal` at `:43`, relational `Transition` at `:70`, executable
`Action`/`step?` at `:87`/`:96`, and soundness/completeness at `:131`/`:171`/`:210`.
`acceptsWork` decidable predicate at `:48`.

### What is emitted today

`processMachine` at `crates/defra-agent/proofs/Proofs/Conformance/Contracts/Machines/Process.lean:24`
from 5 states, terminal `shutdown`, 5 actions, and
`transitionPairsFromSamples` driven by `step?`. Registered at
`crates/defra-agent/proofs/Proofs/Conformance/Contracts/Machines/Catalog.lean:59`.

### Rust consumer state

`runtime_status::tests::rust_process_state_transitions_match_lean_contract`
at `crates/defra-agent/src/runtime_status/tests.rs:85` calls
`assert_state_machine_contract_is_complete("Process")`. Vocab fenced by
`rust_process_state_vocabulary_matches_lean_model` at `:67`. Generated
cases driven by `generated_process_transition_cases_match_runtime_status_policy`
at `:180` (asserts 5 legal / 20 illegal counts at `:228`). Allowlist rows
at `crates/defra-agent/tests/support/conformance_consumers.rs:234` / `:241` /
`:227`.

### Coverage ledger row(s)

`consumerCoverage "state_machine" "Process"` at
`crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean:142`.
Case row at `:209`.

### Classification

Fully bound.

## 3. RuntimeReconcile

### What Lean models today

`ReconcilePhase` 5-variant inductive at `crates/defra-agent/proofs/Proofs/RuntimeReconcile/State.lean:26`,
fuller `RuntimeState` record in the same file, transitions and step in
`RuntimeReconcile/Transition.lean` (e.g. `transition_generation_monotone` at
`:93`, `coherent_preserved` at `:135`) and `RuntimeReconcile/Executable.lean`.
No `Properties.lean` sibling — invariants live alongside the transition
relation.

### What is emitted today

`runtimeReconcileMachine` at `crates/defra-agent/proofs/Proofs/Conformance/Contracts/Machines/RuntimeReconcile.lean:72`
with 5 phase names (no terminals), 12 named actions, and
`transitionPairsFromSamples` over 6 sample states. Registered at
`crates/defra-agent/proofs/Proofs/Conformance/Contracts/Machines/Catalog.lean:64`.
6 `runtime_reconcile_cases` rows asserted at
`crates/defra-agent/tests/state_machine_conformance/coverage.rs:119`.

### Rust consumer state

`runtime_status::tests::rust_reconcile_phase_vocabulary_matches_lean_model`
at `crates/defra-agent/src/runtime_status/tests.rs:232` calls
`assert_state_machine_contract_is_complete("RuntimeReconcile")` at `:247` and
`assert_lean_transition_is_legal("RuntimeReconcile", "applying", "idle")` at
`:248`. Case driver `runtime_status_generation_updates_match_lean_runtime_reconcile_cases`
at `:358` consumes Lean cases via `lean_runtime_reconcile_case(...)`.
Allowlist rows at `crates/defra-agent/tests/support/conformance_consumers.rs:247`
/ `:220`.

### Coverage ledger row(s)

`consumerCoverage "state_machine" "RuntimeReconcile" "runtime_status::tests::rust_reconcile_phase_vocabulary_matches_lean_model"`
at `crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean:166`.
The consumer string points at the vocab test, which also calls the
transition-table assertion inline. Case row at `:217`.

### Classification

Fully bound.

### Smallest delta

Cosmetic only. The ledger consumer string at
`Proofs/Conformance/CoverageLedger.lean:166` is `rust_reconcile_phase_vocabulary_matches_lean_model`,
not the conventional `..._state_machine_contract_is_complete` form used by the
other bound machines. The binding is intact (both assertions are colocated)
but the ledger string is grep-inconsistent with the rest of the catalog.
Either rename the ledger consumer to a dedicated
`rust_reconcile_phase_state_machine_contract_is_complete` test, or split the
two assertions into two `#[test]` functions and update the ledger.

## 4. PairingReconcile

### What Lean models today

Core relational model at `crates/defra-agent/proofs/Proofs/PairingReconcile/State.lean:13-69`.
Coarse executable contract — `PairingPhase` (`idle`/`diverged`/`converged`/`crashed`)
at `crates/defra-agent/proofs/Proofs/PairingReconcile/Executable.lean:14`,
`TransitionKind` (`operatorWrite`, `reconcileInstall`, `reconcileTeardown`,
`crash`) at `:43`, `step?` at `:72`. Convergence theorems at
`crates/defra-agent/proofs/Proofs/PairingReconcile/Convergence.lean:21` / `:29`
/ `:34`.

### What is emitted today

`pairingReconcileMachine` at `crates/defra-agent/proofs/Proofs/Conformance/Contracts/Machines/PairingSession.lean:26`.
Note the file name mismatch: this catalog file is `PairingSession.lean` and
hosts both `pairingReconcileMachine` and `sessionRecoveryMachine`. Registered
at `crates/defra-agent/proofs/Proofs/Conformance/Contracts/Machines/Catalog.lean:65`.

### Rust consumer state

The May-14 audit's §4 ask is closed:
`agent::reconcile::tests::pairing_reconcile_state_machine_contract_is_complete`
at `crates/defra-agent/src/agent/reconcile/tests.rs:79` calls
`assert_state_machine_contract_is_complete("PairingReconcile")` at `:80`,
runs four runtime probes (`operator_write_diverges`, `install_converges`,
`teardown_converges`, `crash_restarts_slot`) at `:84`, reconstructs
`rust_legal_pairs` via `rust_pairing_reconcile_step` over the Lean state
and action vocabularies at `:91`, and asserts equality with
`machine.legal_transitions` and disjointness with `machine.illegal_transitions`
at `:111`/`:115`. Allowlist row at
`crates/defra-agent/tests/support/conformance_consumers.rs:108`. The previous
generic check via `lean_executable_contracts_cover_initial_domains`
(`tests/state_machine_conformance/coverage.rs:12`) still runs but no longer
solely carries the binding.

### Coverage ledger row(s)

`consumerCoverage "state_machine" "PairingReconcile" "agent::reconcile::tests::pairing_reconcile_state_machine_contract_is_complete"`
at `crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean:170`.

### Classification

Fully bound.

### Smallest delta

Cosmetic: rename `Proofs/Conformance/Contracts/Machines/PairingSession.lean`
to `Proofs/Conformance/Contracts/Machines/PairingReconcile.lean` and split the
`sessionRecoveryMachine` definition into a sibling `SessionRecovery.lean`,
so the file/machine mapping is grep-stable. Not a binding gap.

## 5. InferenceCall

### What Lean models today

Five persisted states at `crates/defra-agent/proofs/Proofs/InferenceCall/State.lean:10`
with terminal partition via `HasTerminal` at `:20`, closed-vocab terminal
reasons at `:78`, `InferenceCall` record at `:109`. Relational `Transition`
at `crates/defra-agent/proofs/Proofs/InferenceCall/Transition.lean:27`,
executable in `Executable.lean`. Properties (`transition_preserves_requestId`,
`cancelled_has_no_outgoing`, `trace_preserves_*`) at
`crates/defra-agent/proofs/Proofs/InferenceCall/Properties.lean:11-120`. Slot
accounting model at `crates/defra-agent/proofs/Proofs/InferenceCall/SlotAccounting.lean:16-225`.

### What is emitted today

`inferenceCallMachine` at `crates/defra-agent/proofs/Proofs/Conformance/Contracts/Machines/InferenceCall.lean:33`
with 5 states, terminal projection, 4 actions, partition from `step?` at
`:39`. Registered at
`crates/defra-agent/proofs/Proofs/Conformance/Contracts/Machines/Catalog.lean:67`.
11 `inference_slot_accounting_cases` rows (per coverage.rs:124).

### Rust consumer state

`admission::tests::rust_inference_call_transition_table_matches_lean_contract`
at `crates/defra-agent/src/admission/tests.rs:342` calls
`assert_state_machine_contract_is_complete("InferenceCall")` at `:343` plus
five legal/two illegal pin-checks at `:344-350`. State + terminal-reason
vocab fenced at `:322`/`:332`. Slot-accounting cases driven by
`generated_inference_slot_accounting_cases_match_admission_reconstruction_logic`
at `:354`. Allowlist rows at
`crates/defra-agent/tests/support/conformance_consumers.rs:80` / `:87` /
`:94` / `:65`.

### Coverage ledger row(s)

`consumerCoverage "state_machine" "InferenceCall"` at
`crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean:178`.
Vocab rows at `:95` / `:99`. Slot-case row at `:229`.

### Classification

Fully bound.

## 6. ToolCall

### What Lean models today

Six persisted tool-call states at `crates/defra-agent/proofs/Proofs/ToolExecution/State.lean:20`,
terminal partition via `HasTerminal` at `:61`. `CancelCause` vocabulary
(`interrupted`, `deadline`, `userCancelled`) added by #247 at
`crates/defra-agent/proofs/Proofs/ToolExecution/CancelCause.lean:13`, with
`toDefraDB`/`fromDefraDB?` round-trip and `all_complete` proofs at `:21-40`.
`ToolCallContext` and relational `Transition` in
`Proofs/ToolExecution/Transition.lean`; executable `step?` in `Executable.lean`.
Properties — `terminal_irreversible` (`Properties.lean:15`),
`cancellable_iff_non_terminal` (`:37`), `completed_implies_committed` (`:70`),
`live_call_reaches_terminal` (`:94`), `transition_preserves_{requestId,callId}`
(`:143`/`:165`).

### What is emitted today

`toolCallMachine` at `crates/defra-agent/proofs/Proofs/Conformance/Contracts/Machines/ToolCall.lean:121`
from `ToolCallState.all`, an 11-action vocabulary at `:28` (including
`cancelBeforeDispatch_<cause>` / `cancelDuringRun_<cause>` generated by
`flatMap` over `CancelCause.all` at `:23`), partition from `ToolCallContext.step?`,
plus a 10-row `namedTransitions` table at `:78` covering native-only
edges (`complete_native`/`fail_native`), mode flips, and bridge edges.
Registered at `crates/defra-agent/proofs/Proofs/Conformance/Contracts/Machines/Catalog.lean:68`.
`CancelCause` vocab in catalog at `:42`.

### Rust consumer state

`tool_call_lifecycle::tests::tool_call_state_machine_contract_is_complete` at
`crates/defra-agent/src/tool_call_lifecycle.rs:569` calls
`assert_state_machine_contract_is_complete("ToolCall")` at `:570`. Three
vocab tests: state at `:527`, `CancelCause::ALL` at `:541` (the #247
addition), `FailureClass::ALL` at `:555`. Terminal partition cross-check at
`:574`. Allowlist rows at
`crates/defra-agent/tests/support/conformance_consumers.rs:417` / `:423`
(CancelCause) / `:429` / `:437`.

### Coverage ledger row(s)

`consumerCoverage "state_machine" "ToolCall"` at
`crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean:182`.
Vocab rows at `:107` (state), `:111` (CancelCause), `:119` (FailureClass).

### Classification

Fully bound. #247's `CancelCause` is wired end-to-end on both sides.

## 7. ManagedExec

### What Lean models today

Seven persisted executor states at `crates/defra-agent/proofs/Proofs/ManagedExec/State.lean:12`
with terminal partition via `HasTerminal` at `:63` and `ManagedExecState.all`
at `:50`. `ManagedExecContext` (deadline, now, killSignaledAt, exitCode) at
`:79` with decidable `deadlineExceeded` at `:90`. Cross-machine composition
in `crates/defra-agent/proofs/Proofs/ManagedExec/Composed.lean:14`;
operational theorems `running_tool_times_out_after_deadline_bounded` at `:54`
and `running_tool_cancelled_in_bounded_steps` at `:78`. Whole subtree
landed via #229 (commit `78c78ab`).

### What is emitted today

`managedExecMachine` at `crates/defra-agent/proofs/Proofs/Conformance/Contracts/Machines/ManagedExec.lean:35`
from `ManagedExecState.all`, 8 actions at `:16`. Registered at
`crates/defra-agent/proofs/Proofs/Conformance/Contracts/Machines/Catalog.lean:69`.
5 `managed_exec_liveness_cases` rows asserted at
`crates/defra-agent/tests/state_machine_conformance.rs:161`.

### Rust consumer state

`managed_exec::tests::managed_exec_state_machine_contract_is_complete` at
`crates/defra-agent/src/managed_exec/tests.rs:27` calls
`assert_state_machine_contract_is_complete("ManagedExec")` at `:29` plus
three `assert_lean_transition_is_legal` pin-checks at `:30-32`. State vocab
at `:13`. Liveness cases driven by
`state_machine_conformance::managed_exec_liveness_cases_pin_native_process_boundary`
at `crates/defra-agent/tests/state_machine_conformance.rs:137`. Allowlist
rows at `crates/defra-agent/tests/support/conformance_consumers.rs:206` /
`:213` / `:332`.

#248 ("soak closeout gate", commit `eb79341`) is Rust-only — it adds
`managed_exec_deadline_kills_process_group` and monotonic-age regression
checks in `crates/defra-agent/src/managed_exec/tests.rs:37` but no Lean
spec rows. It does not regress the existing binding.

### Coverage ledger row(s)

`consumerCoverage "state_machine" "ManagedExec"` at
`crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean:186`.
Vocab row at `:115`. Liveness-case row at `:257`.

### Classification

Fully bound.

## 8. Background

### What Lean models today

Composed-state model derived from `ToolCall` + `Request` plus four
vocabularies. `BridgedState` and its `Transition` across
`crates/defra-agent/proofs/Proofs/Background/State.lean:13`,
`Background/Bridge.lean:27`, `Background/Transition.lean:14`. Vocabularies:
`BackgroundedKind` at `State.lean:17`, `ChildTerminal` at `:24`, `AwaitMode`
at `:49`, `CancelPolicy` (with `maxBackgroundedPerParent := 8`) at `:115`.
Six Properties theorems with no Rust witness: `backgrounded_budget_bounded`
(`Background/Properties/Budget.lean:31`), `foreground_blocks_parent_advance`
+ `subagent_depth_bounded` + `bridge_link_symmetric`
(`Background/Properties/Foreground.lean:14`/`:88`/`:98`),
`bridged_child_completion_propagates` + `bridged_child_failure_projects`
(`Background/Properties/Projection.lean:19`/`:89`),
`inv_depth` + `inv_link` (`Background/Properties/Structure.lean:111`/`:197`),
`cascade_cancels_child` + `detach_does_not_cancel_child`
(`Background/Properties/Cancellation.lean:22`/`:190`),
`bridgedUniqueCallIds_preserved` (`Background/Properties/Unique.lean:198`).

Background itself is not in the `stateMachines` catalog at
`Proofs/Conformance/Contracts/Machines/Catalog.lean:57` — only the three
vocabularies (`AwaitMode`/`CancelPolicy`/`ChildTerminal`) are registered as
catalog machines via `Subagent.lean:17`/`:30`/`:60`.

### What is emitted today

Two case families, both via the new BackgroundWork JSON split:

- `r6_backgrounding_cases` at `crates/defra-agent/proofs/Proofs/Conformance/Contracts/Json/Snapshot.lean:98`,
  serialized at `crates/defra-agent/proofs/Proofs/Conformance/Contracts/Json/BackgroundWork.lean:166`.
- `r4c_background_work_cases` at `crates/defra-agent/proofs/Proofs/Conformance/Contracts/Json/Snapshot.lean:96`,
  serialized at `Json/BackgroundWork.lean:155`.

No Properties theorem is reified as JSON.

### Rust consumer state

Two case consumers, both `#[test]` (sync, no DB):

- `generated_r6_backgrounding_cases_pin_tool_backgrounding_contract` at
  `crates/defra-agent/tests/state_machine_conformance/transcript_background.rs:356`
  pins the seven case names, `max_backgrounded == 8`, `await_mode == "background"`,
  `cancel_policy == "cascade"`, terminal-state strings, error codes, and
  `queue_key` strings. No runtime spawn admission or budget call.
- `generated_r4c_background_work_cases_pin_observable_shapes` at
  `crates/defra-agent/tests/state_machine_conformance/transcript_background.rs:434`
  exhaustively pattern-matches each Lean witness variant and asserts hard-coded
  string fields.

AwaitMode/CancelPolicy/ChildTerminal vocabularies are pinned by
`lean_emits_await_mode_vocabulary` (and two siblings) at
`crates/defra-agent/tests/state_machine_conformance/tool_call.rs:35`. No
Rust call site references any Background Properties theorem name.

### Coverage ledger row(s)

`r6_background_cases` at `crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean:301`.
`r4c_background_work_cases` at `:305`. AwaitMode/CancelPolicy/ChildTerminal
double-rows (vocabulary + state_machine) at `:123-134` / `:190-201`.

### Classification

Case-only for the two case families (data witnesses, not runtime drives).
Vocab-only for AwaitMode/CancelPolicy/ChildTerminal. The six Properties
theorems are Lean-only — not bound, not declared as deferred.

### Smallest delta

Two distinct gaps. First, the case-only rows: extend `r6_backgrounding_cases`
with a runtime drive (call the actual background spawn admission and observe
the resulting `terminal_state` against the Lean projection). Second, the
Properties theorems: either (a) extend
`Proofs/Conformance/ContractCases/R6Background.lean` to emit one witness row
per theorem (so the ledger can promote them from invisible to "property-bound")
or (b) add a `followUpCoverage` row recording the decision to keep them
Lean-only. Today the audit reader cannot distinguish "deliberately formal-only"
from "we forgot to wire them."

## 9. Identity

### What Lean models today

The trinity `Principal`/`Behavior`/`Deployment` and `World.WellFormed` at
`crates/defra-agent/proofs/Proofs/Identity/State.lean:17`/`:23`/`:30`/`:37`
(unchanged from 2026-05-15). Permission engine: `GrantStore`, `Decide`,
`RespectsPrincipal`, `canonicalDecide`, and
`canonicalDecide_respectsPrincipal` at
`crates/defra-agent/proofs/Proofs/Identity/Permission.lean:17`/`:20`/`:25`/`:31`/`:35`.
Properties (shared-principal sharing, isolation, no-escalation,
behavior-id-determines-principal, `Deployment.canHostBehavior`) at
`crates/defra-agent/proofs/Proofs/Identity/Properties.lean:15`/`:25`/`:37`/`:46`/`:57`.
Identity remains absent from the `stateMachines` catalog.

### What is emitted today

Three domains in `Proofs/Conformance/Contracts/Json/Snapshot.lean:124-129`:

- `identity_structural_cases` from `structuralCasesJson` at
  `crates/defra-agent/proofs/Proofs/Identity/Conformance.lean:386`.
- `identity_permission_cases` (new since 2026-05-15, closes #219) from
  `identityPermissionCasesJson` at
  `crates/defra-agent/proofs/Proofs/Identity/Conformance.lean:418`. Row
  structure declared at `:130`; four named cases produced at `:282`.
- `identity_contracts` from `identityContractsJson` at
  `crates/defra-agent/proofs/Proofs/Identity/Conformance.lean:451`. The
  single row is `identity.respects_principal_boundary` with
  **`enforced := true`** at
  `crates/defra-agent/proofs/Proofs/Identity/Conformance.lean:438`. The
  2026-05-15 audit had this at `false`.

### Rust consumer state

#193 landed (commit `3d76af9`). Runtime carries the typed split:

- `AgentPrincipal` at `crates/defra-agent/src/identity.rs:38`.
- `AgentBehavior` with `pub principal: Arc<AgentPrincipal>` back-ref at
  `crates/defra-agent/src/config.rs:31`. Doc comment at `:22-27` explicitly
  cites the Lean theorem and notes the back-ref makes
  `behavior_id_determines_principal` structural-at-the-type-level.
- `DefraAgent.principal: Arc<AgentPrincipal>` at
  `crates/defra-agent/src/agent.rs:92` (replacing the pre-#193 conflated
  shape).
- `crates/defra-agent/src/document_config/{principal,behavior,event_trigger,task,schedule,inference_profile}.rs`
  per-collection apply files exist.

Tests in `crates/defra-agent/tests/identity_conformance.rs`:

- `identity_structural_cases_match_lean_verdicts` at `:139` consumes structural
  cases via the Rust `rust_well_formed` mirror.
- `identity_permission_cases_pin_runtime_permission_contract_shape` at `:177`
  consumes the four Lean permission cases through
  `build_runtime_behaviors_from_lean_case` at `:26`, asserts
  `actor.principal.agent_did == case.expected_actor_principal` and that
  `actor.principal.agent_did == peer.principal.agent_did <=> case.same_principal`.
  Does NOT drive a runtime decide function or a deployment hostability lookup;
  `expectedActorAllowed` / `expectedPeerAllowed` / `expectedActorHostable` /
  `expectedPeerHostable` are emitted-but-unused.
- `identity_respects_principal_contract_enforced_by_runtime_routing` at `:268`
  asserts `target.enforced == true` at `:283` and loops over every Lean
  permission case asserting the routing-witness equalities at `:311-339`.
  Enforcement is structural-by-type: the runtime cannot construct an
  `AgentBehavior` without `Arc<AgentPrincipal>`, so two behaviors sharing a
  principal Arc share `principal.agent_did` by construction.

### Coverage ledger row(s)

`identity_structural_cases` at `crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean:313`.
`identity_permission_cases` (consumerWithFollowUpCoverage) at `:317-321` with
follow-up "Issue #193 replaces the Rust mirror... with the runtime permission
decision module and deployment hostability lookup."
`identity_contracts` at `:322` (consumer name flipped from
`identity_respects_principal_contract_is_declared` to
`identity_respects_principal_contract_enforced_by_runtime_routing`).

### Classification

- `identity_structural_cases` and `identity_contracts`: Fully bound for the
  boundary they cover. Enforcement is via the typed `Arc<AgentPrincipal>`
  back-ref, not a Rust decide function.
- `identity_permission_cases`: Case-only / partial. Lean emits decision and
  hostability fields; Rust uses only the routing equality.

### Smallest delta

Introduce a runtime permission-decision entry point and a deployment
hostability lookup, then consume `expected_actor_allowed` /
`expected_peer_allowed` / `expected_actor_hostable` / `expected_peer_hostable`
in `crates/defra-agent/tests/identity_conformance.rs:177`. Once both runtime
modules exist and pass the four named Lean cases, promote
`crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean:317` from
`consumerWithFollowUpCoverage` to `consumerCoverage`. Lean is already shape-
complete; no Lean delta is needed.

## 10. MCPHealth

### What Lean models today

Four-state lifecycle (`healthy`/`degraded`/`evicted`/`reconnecting`) and
`ServiceModel { state, failureCount }` at
`crates/defra-agent/proofs/Proofs/MCPHealth/State.lean:26`/`:67`; events
`probeSuccess`/`probeFail`/`backoffExpiry`/`registryAbsent` at
`State.lean:79`. K-parameterized transition function at
`crates/defra-agent/proofs/Proofs/MCPHealth/Transition.lean:14-98`.
Properties at `Proofs/MCPHealth/Properties.lean:15-356`. Four bridging
lemmas C1–C4 to `ToolExecution.preflight` at
`crates/defra-agent/proofs/Proofs/MCPHealth/Coupling.lean:36`/`:42`/`:48`/`:58`.
Executable enumeration over `K ∈ {1, 2, 3} × HealthState × startCount × Event`
produces `transitionCases` at
`crates/defra-agent/proofs/Proofs/MCPHealth/Executable.lean:73`, with K=1
subset `k1ProjectionCases` at `:79` and partition theorem `k1_k2_partition`
at `:87`. MCPHealth is NOT in the state-machine catalog at
`Proofs/Conformance/Contracts/Machines/Catalog.lean:57-73`.

### What is emitted today

`mcp_health_cases` emitted at
`crates/defra-agent/proofs/Proofs/Conformance/Contracts/Json/Snapshot.lean:110`
from the full `transitionCases` list (all K values). Row JSON at
`crates/defra-agent/proofs/Proofs/Conformance/Contracts/Json/ToolExecution.lean:69`.
No state-machine row, no Properties theorem, no Coupling theorem reified.

### Rust consumer state

`generated_mcp_health_k1_cases_match_health_checker_transitions` at
`crates/defra-agent/src/health_checker.rs:434`. It filters Lean rows to K=1
only via `lean_mcp_health_k1_cases()` and asserts each row's
`rust_projection` matches a Rust simulator `rust_simulated_next` at
`health_checker.rs:400`. The simulator mirrors the inline logic at
`health_checker.rs:247-308` (per the doc comment at `:399`) — it does not
call `run_health_check` directly. Comment at `:407` records the K≥2 gap:
"Today's Rust has no backoffExpiry behavior — backoff is not armed at K=1."
Allowlist row at `crates/defra-agent/tests/support/conformance_consumers.rs:444`.

### Coverage ledger row(s)

`consumerCoverage "mcp_health_cases" "MCPHealthCases" "health_checker::tests::generated_mcp_health_k1_cases_match_health_checker_transitions"`
at `crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean:346-349`.
No follow-up annotation, even though the consumer only handles K=1.

### Classification

Case-only (K=1 slice). The full transition table is not fenced via
`assert_state_machine_contract_is_complete`. K≥2 rows have no consumer; the
Coupling.lean C1–C4 theorems are Lean-only.

### Smallest delta

Two stages. Stage one: change the K=1 consumer at
`crates/defra-agent/src/health_checker.rs:434` from "simulator mirror" to
"drive `run_health_check`" so a regression in the production probe-decision
branch flips Lean from green to red. Stage two: when the K≥2 refactor lands,
drop the `lean_mcp_health_k1_cases()` filter and consume the full
`mcp_health_cases` list. In the meantime, demote the ledger row at
`Proofs/Conformance/CoverageLedger.lean:346` from `consumerCoverage` to
`consumerWithFollowUpCoverage` with the K≥2 gap as the follow-up text —
today's row overstates the binding.

## 11. Compaction

### What Lean models today

`Compaction/{State,Transition,Executable,Properties}.lean`. Structural
reducer model with `CompactionReducerCase` at
`crates/defra-agent/proofs/Proofs/Compaction/Executable.lean:18`, ten named
cases starting at `:32`, count theorem `compactionReducerCases.length = 10`
at `:145`. Doc-comment at `:6` notes cases pin structural contract;
behavioral coverage stays in `crates/defra-agent/src/compaction/tests.rs`.
Not in the state-machine catalog.

### What is emitted today

`compaction_reducer_cases` at
`crates/defra-agent/proofs/Proofs/Conformance/Contracts/Json/Snapshot.lean:107`,
row JSON at
`crates/defra-agent/proofs/Proofs/Conformance/Contracts/Json/ClientRuntime.lean:55`.
The PR-#202 emit gap from the 2026-05-14 audit is closed by #237.

### Rust consumer state

`generated_compaction_reducer_cases_pin_contract` at
`crates/defra-agent/tests/state_machine_conformance/streaming_compaction.rs:460`
asserts case count 10 and the ten names, then loops `drive_compaction_reducer_case`
per case. `drive_compaction_reducer_case` at `:491` builds the input messages
from `pre_message_count`, calls `apply_compaction_reducer` at `:502`, then
asserts `post_message_count`, `preserves_pairs`, `preserves_order`, and
`reducer_is_identity` against the runtime outcome. `apply_compaction_reducer`
at `:563` dispatches the Lean `reducer` discriminator to runtime code:
`defra_agent::compaction::strip_tool_results(input).0` for
`strip_tool_results` and `any_valid` reducers at `:569-570`. The
`strict_idempotence` case at `:537` re-applies the runtime
`strip_tool_results` and verifies the structural projection is fixed.
Allowlist row at `crates/defra-agent/tests/support/conformance_consumers.rs:311`.

### Coverage ledger row(s)

`consumerCoverage "compaction_reducer_cases" "CompactionReducerCases"` at
`crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean:330`. The
2026-05-15 audit had this as `consumerWithFollowUpCoverage`; the follow-up
note is gone.

### Classification

Fully bound (structural projection). The full reducer state machine is not
catalog-fenced via `assert_state_machine_contract_is_complete`, but the
ten cases drive `strip_tool_results` end-to-end. Per the Lean
`Executable.lean:6` doc-comment this is intentional: behavioral coverage
(stub-text formatting, file-activity extraction) stays in
`crates/defra-agent/src/compaction/tests.rs`.

## 12. StreamingResponse

### What Lean models today

`StreamingResponse/{State,Transition,Executable,Properties}.lean` plus
`Properties/{Lifecycle,LiveTail,Liveness}.lean`. Status vocabulary at
`crates/defra-agent/proofs/Proofs/StreamingResponse/State.lean:17`,
`ErrorReason` at `:54`, `HasTerminal Status` at `:40`.
`ResponseTransitionCase` row structure at
`crates/defra-agent/proofs/Proofs/StreamingResponse/Executable.lean:13`
with twelve named cases at `:31`–`:229`; list `responseTransitionCases` at
`:247`. Not in state-machine catalog.

### What is emitted today

`streaming_response_cases` at
`crates/defra-agent/proofs/Proofs/Conformance/Contracts/Json/Snapshot.lean:104`,
row JSON at
`crates/defra-agent/proofs/Proofs/Conformance/Contracts/Json/ClientRuntime.lean:32`.
The PR-#199 emit gap from the 2026-05-14 audit is closed by #237.

### Rust consumer state

`generated_streaming_response_cases_pin_lifecycle_contract` at
`crates/defra-agent/tests/state_machine_conformance/streaming_compaction.rs:15`
(async). Asserts case count 12 and the twelve names, then loops
`drive_streaming_response_case` per case. The driver at `:48` constructs a
real DefraDB-backed `test_db`, calls `create_request` and
`create_manual_response`, constructs a real `DefraStreamWriter` at `:73`,
seeds a streaming tail, then dispatches the Lean `action` discriminator to
runtime entry points: `writer.begin`, `writer.write_tokens` /
`writer.flush_pending` (`:103-107`), `writer.write_reasoning` (`:110`),
`writer.flush_pending` (`:120`), `writer.reset_tail` (`:123`),
`writer.finalize` with `StreamStatus::Complete`/`::Error` (`:129-144`),
`RequestLifecycle::recover_all(&db.node, AGENT_DID)` (`:147`), idempotent
re-finalize (`:154`), `writer.write_interrupted_at` (`:162`). Post-state
verified via `assert_streaming_response_shape` (`:183`) and
`assert_request_bridge_shape` (`:174`). Allowlist row at
`crates/defra-agent/tests/support/conformance_consumers.rs:304`.

### Coverage ledger row(s)

`consumerCoverage "streaming_response_cases" "ResponseTransitionCases"` at
`crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean:326`. The
2026-05-15 audit had this as `consumerWithFollowUpCoverage`; the follow-up
note is gone.

### Classification

Fully bound (lifecycle projection). End-to-end through `DefraStreamWriter`
and the recovery sweep. Not catalog-fenced as a full state machine, but the
twelve cases cover all named transitions.

## 13. Transcript

### What Lean models today

`TranscriptState` with abstract `MessageKind` and `ToolResultKey` at
`crates/defra-agent/proofs/Proofs/Transcript/State.lean:1`/`:21`. Relational
`Transcript.Transition` at `Transcript/Transition.lean:13` (`append_user`,
`begin_assistant_tool_call`, `persist_assistant`, `complete_tool_with_result`,
`observe_duplicate_tool_result`, `append_distinct_tool_result`,
`cancel_in_flight`, `fail_in_flight`). Properties + Dedupe in
`Transcript/Properties.lean` and `Transcript/Dedupe.lean`. Six executable
`TranscriptCase` rows at `Transcript/Executable.lean:12` aggregated as
`transcriptConformanceCases` at `:145`.

### What is emitted today

`transcript_conformance_cases` at
`crates/defra-agent/proofs/Proofs/Conformance/Contracts/Json/Snapshot.lean:101`,
row JSON at
`crates/defra-agent/proofs/Proofs/Conformance/Contracts/Json/BackgroundWork.lean:185`.
Not in state-machine catalog.

### Rust consumer state

`generated_transcript_cases_pin_agent_message_ordering_contract` at
`crates/defra-agent/tests/state_machine_conformance/transcript_background.rs:589`
consumes all six cases via `lean_transcript_case(name)`. Despite the "pin"
in the function name, the consumer drives the production `DefraSessionHook`
per case: `PromptHook::on_completion_call`, `on_tool_call`,
`persist_message`, `on_tool_result`, and `cancel_in_flight_tool_calls` at
`:594` / `:629` / `:667` / `:709` / `:729` / `:781`. Post-state read back
from DefraDB via `fetch_message_snapshots_for_session`,
`fetch_tool_call_snapshots_for_session`, and `defra_agent::load_history` at
`:91`, compared against case expectations in `assert_transcript_post_state`
at `:170`. Allowlist row at
`crates/defra-agent/tests/support/conformance_consumers.rs:297`.

The 2026-05-14 audit's ⚠️ Partial verdict is stale — this is now a runtime
drive, not a shape-pin.

### Coverage ledger row(s)

`consumerCoverage "transcript_cases" "TranscriptConformanceCases"` at
`crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean:309`.

### Classification

Fully bound (case-only family, runtime-driven against `DefraSessionHook` +
DefraDB).

### Smallest delta

Cosmetic: rename the consumer from `generated_transcript_cases_pin_...` to
`generated_transcript_cases_drive_...` so it matches the post-#239 naming
convention (compare to `generated_recovery_sweep_cases_drive_startup_recovery_contract`
at `Proofs/Conformance/CoverageLedger.lean:297`). The runtime drive already
exists; the discrepancy is purely lexical.

## 14. Triggers

### What Lean models today

Core dispatch in `crates/defra-agent/proofs/Proofs/Triggers/Dispatch.lean`
with `dispatch` at `:40` and `dispatch_manual_lineage_id_is_none` at `:77`.
Property theorems: `T1_enabled_gate` / `T1_manual_unconditional` at
`Dispatch.lean:199`/`:273`, `T2_serial_at_most_one` (plus variants) at
`Triggers/Serial.lean:23`/`:69`/`:79`/`:92`, `T3_latest_only_convergence` at
`Triggers/LatestOnly.lean:58`, `T4_lineage_completeness` at
`Triggers/Lineage.lean:39`. Reachability + counting/preservation helpers in
`Triggers/Reachability.lean`, `Triggers/SerialSupport/{Counting,Preservation}.lean`.
Barrel at `Proofs/Triggers.lean:1`.

### What is emitted today

A single `trigger_dispatch_cases` array (with count sentinel) at
`crates/defra-agent/proofs/Proofs/Conformance/Contracts/Json/Snapshot.lean:37`,
fed by `triggerDispatchCasesJson` at
`crates/defra-agent/proofs/Proofs/Conformance/Triggers/Contracts.lean:268`.
Case list `triggerDispatchScenarios` at
`crates/defra-agent/proofs/Proofs/Conformance/Triggers/Contracts.lean:197`
covers serial / parallel / latest-only / lineage branches. T1–T4 theorems
and the LatestOnly / Lineage / Serial / Reachability / SerialSupport
sub-models are reference-only inside Lean.

### Rust consumer state

`trigger_engine_dispatch_matches_lean_generated_contract_cases` in
`crates/defra-agent/src/trigger_engine/tests/dispatch_contract.rs:3`. Invokes
the real `TriggerEngine::new(...).dispatch(intent)` at `:67-81`; asserts
`FireResult` shape (`:87`), materialize delta + trigger-id + trigger-kind +
rendered prompt + execution origin + caused-by lineage (`:101-150`),
supersede-call list and superseded request-ids (`:161-177`), target
non-terminal count after dispatch (`:179`). Entry point at
`crates/defra-agent/src/trigger_engine/tests/mod.rs:484`. Allowlist row at
`crates/defra-agent/tests/support/conformance_consumers.rs:409`.

### Coverage ledger row(s)

Single row: `consumerCoverage "trigger_cases" "TriggerDispatch"` at
`crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean:213`. No
independent ledger entries for LatestOnly / Lineage / Reachability / Serial /
SerialSupport — they are property-bound inside Lean and transitively
covered through `triggerDispatchScenarios`.

### Classification

Case-only (runtime-driven for `dispatch`). T1–T4 are Property-bound inside
Lean only.

## 15. EventDelivery

### What Lean models today

Abstract contract in `crates/defra-agent/proofs/Proofs/EventDelivery/Contract.lean:1`
with `World` at `:45` and `DedupePolicy` at `:21`. Three instances in
`Watcher.lean`, `EventSource.lean`, `SubagentSource.lean`. Properties (Fair
predicate, D1 bounded-rescan convergence) in
`Proofs/EventDelivery/Properties.lean`.

### What is emitted today

Three JSON keys driven through `Proofs/Conformance/EventDelivery.lean`:

- `event_delivery_transition_cases` + count sentinel at
  `crates/defra-agent/proofs/Proofs/Conformance/Contracts/Json/Snapshot.lean:114`,
  from `transitionCases` at
  `crates/defra-agent/proofs/Proofs/Conformance/EventDelivery.lean:30`.
- `event_delivery_source_instances` at `Snapshot.lean:118`, from
  `sourceInstances` at `Proofs/Conformance/EventDelivery.lean:121` (three
  runtime sources with `dedupePolicy`/`rescanBoundedBy`/optional `deviation`).
- `event_delivery_convergence_traces` at `Snapshot.lean:120`, from
  `convergenceTraces` at `Proofs/Conformance/EventDelivery.lean:196`
  (substantive + deviation traces).

The #240 P2P deviation is emitted at
`crates/defra-agent/proofs/Proofs/Conformance/Deviations.lean:49`;
`EventSource` and `SubagentSource` carry `event_source_lacks_periodic_rescan`
and `subagent_source_lacks_live_rescan` at `Deviations.lean:26`/`:37`.

### Rust consumer state

Mixed. The source-instances consumer is a runtime drive; the other two are
in-test simulators.

- `crates/defra-agent/src/event_delivery_contract.rs:14` defines the
  `EventDeliverySourceContract` trait and a 3-element runtime instance
  table at `:18`. Production constants:
  - `DefraWatcher::EVENT_DELIVERY_CONTRACT` (`rescan_bounded_by: 1`) at
    `crates/defra-agent/src/watcher.rs:111`.
  - `EventSource::EVENT_DELIVERY_CONTRACT` (`rescan_bounded_by: 0`) at
    `crates/defra-agent/src/trigger_engine/event_source.rs:755`.
  - `SubagentSource::EVENT_DELIVERY_CONTRACT` (`rescan_bounded_by: 0`) at
    `crates/defra-agent/src/trigger_engine/subagent_source.rs:475`.
- `event_delivery_source_instances_match_runtime` at
  `crates/defra-agent/tests/state_machine_conformance/event_delivery.rs:24`
  asserts the three production constants match the Lean rows, including
  `deviation` text.
- `event_delivery_transition_cases_match_contract` at
  `tests/state_machine_conformance/event_delivery.rs:3` and
  `event_delivery_convergence_traces_match_runtime_or_deviation` at `:45`
  run an `InMemoryEventDeliverySource` simulator defined at `:117`. They
  replay Lean actions in-test; they do NOT exercise the production
  `DefraWatcher` / `EventSource` / `SubagentSource` event loops.

Allowlist rows at `crates/defra-agent/tests/support/conformance_consumers.rs:360`
/ `:367` / `:374`.

### Coverage ledger row(s)

Three rows at `crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean:334`
(transition cases), `:338` (source instances), `:342` (convergence traces).
All marked `consumerCoverage`.

### Classification

Mixed: source-instances row is Fully bound (production constants read via
`EventDeliveryRuntimeContract` trait). Transition cases and convergence
traces are Case-only (in-memory simulator), matching the 2026-05-14 audit's
⚠️ Partial verdict — the production source loops are not exercised. The
ledger's `consumerCoverage` classification for the latter two overstates
the binding. The #240 / `defradb_rs_p2p_subscription_state_not_durable`
deviation is correctly wired as a `Deviation` row.

### Smallest delta

Replace `InMemoryEventDeliverySource` at
`crates/defra-agent/tests/state_machine_conformance/event_delivery.rs:117`
with a thin driver that posts the Lean `LeanEventDeliveryAction` events
against the production `DefraWatcher` / `EventSource` / `SubagentSource`
loops and reads back observed deliveries, so convergence traces actually
drive production code paths. Until then, demote the two
ledger rows at `Proofs/Conformance/CoverageLedger.lean:334`/`:342` from
`consumerCoverage` to `consumerWithFollowUpCoverage`. Subscription
persistence (#240 / `defradb_rs_p2p_subscription_state_not_durable`) stays
a deviation until `sourcenetwork/defradb.rs#957` lands.

## 16. CommandPolicy

### What Lean models today

Types in `crates/defra-agent/proofs/Proofs/CommandPolicy/Types.lean`,
validation rules in `Validation.lean`, sandbox selection in `Sandbox.lean`,
env-filtering in `Env.lean`, theorems in `Theorems.lean`, finite executable
rows in `Cases.lean`.

### What is emitted today

Three case families:

- `command_policy_cases` (45 rows) at
  `crates/defra-agent/proofs/Proofs/Conformance/Contracts/Json/Snapshot.lean:82`.
- `command_sandbox_cases` (4 rows) at `Snapshot.lean:84`.
- `command_env_cases` (14 rows) at `Snapshot.lean:86`.

Counts asserted at `crates/defra-agent/tests/state_machine_conformance/coverage.rs:170-172`.

### Rust consumer state

- `generated_command_policy_cases_match_rust_validation` at
  `crates/defra-agent/src/toolset/tests.rs:892` drives the production
  `CommandExecutionPolicy` builder at `:906` and calls
  `validate_command_policy(&case.command, &case.args, &policy)` at `:911`.
  Matches `case.decision` plus per-reason argument/subcommand fields via
  `assert_command_denial_matches` at `:1144`.
- `generated_command_sandbox_cases_match_rust_selection` at `:1078` invokes
  the real `select_sandbox_for_policy(mode, case.workspace_write_sandbox_enforced)`
  at `:1081`.
- `generated_command_env_cases_match_rust_filtering` at `:1113` calls real
  `build_shell_env_from_vars(...)` at `:1120`.
- Coverage sentinel: `generated_command_policy_cases_cover_read_only_safety_matrix`
  at `:933` enumerates 35 expected case names.

Allowlist rows at `crates/defra-agent/tests/support/conformance_consumers.rs:381`
/ `:388` / `:395`.

### Coverage ledger row(s)

Three rows at `crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean:281`
/ `:285` / `:289`.

### Classification

Fully bound (case-only family). Production policy entry points are driven
per case; production host-execution assumptions are tracked separately as
`boundary.command-policy.host-execution-assumptions` at
`crates/defra-agent/tests/state_machine_conformance/coverage.rs:263`.

## 17. SessionRecovery

### What Lean models today

`Proofs/SessionRecovery.lean` — top-level reissue module, the `reissueFailed`
transition on the session's latest-request lifecycle. Session-queue model in
`Proofs/Session/{State,Transition,Executable,Properties}.lean` plus four
property sub-modules `Coalescing.lean` / `Drain.lean` / `Executable.lean` /
`Ordering.lean`. E.g., `pendingAfterDrain_preserves_createdOrdered` at
`crates/defra-agent/proofs/Proofs/Session/Properties/Ordering.lean:111`,
`pendingAfterDrain_preserves_uniqueCoalescedQueueKeys` at
`Proofs/Session/Properties/Coalescing.lean:109`.

### What is emitted today

Full state-machine row `SessionRecovery` via `sessionRecoveryMachine` at
`crates/defra-agent/proofs/Proofs/Conformance/Contracts/Machines/PairingSession.lean:45`
with `requestStateNames` as states and `["reissueFailed"]` as the action.
Registered at
`crates/defra-agent/proofs/Proofs/Conformance/Contracts/Machines/Catalog.lean:66`.
Vocabulary `SessionRecoveryLatestRequestState = RequestState` at `Catalog.lean:30`.
`session_recovery_cases` (18 rows) at
`crates/defra-agent/proofs/Proofs/Conformance/Contracts/Json/Snapshot.lean:53`,
JSON via `sessionRecoveryCaseJson` at
`crates/defra-agent/proofs/Proofs/Conformance/Contracts/Json/Runtime.lean:40`.
The `Proofs/Session/Properties/{Coalescing,Drain,Ordering}` modules are not
separately emitted — they feed `queueDeadlineConformanceCases` (via
`import Proofs.Session.Executable` at
`crates/defra-agent/proofs/Proofs/Conformance/ContractCases/QueueDeadline.lean:3`)
and the `Transcript` model.

### Rust consumer state

`assert_state_machine_contract_is_complete("SessionRecovery")` is called via
`lean_executable_contracts_cover_initial_domains` at
`crates/defra-agent/tests/state_machine_conformance/coverage.rs:13`-`:16`,
plus targeted edge pin-checks at `:92-96`.
`generated_session_recovery_cases_drive_db_backed_reissue_contract` at
`crates/defra-agent/tests/state_machine_conformance/session_recovery.rs:103`
seeds DefraDB via `seed_session_recovery_case` at `:202` and runs
`reissue_failed_request_for_contract` at `:332` — a real DefraDB-backed
write path. Post-conditions checked at `assert_legal_reissue_postconditions`
(`:449`). No Rust consumer for `Session/Properties/{Coalescing,Drain,Ordering}`
individually; they are transitively bound via `queue_deadline_cases` and
`transcript_cases`. Allowlist row at
`crates/defra-agent/tests/support/conformance_consumers.rs:255`.

### Coverage ledger row(s)

Full machine: `consumerCoverage "state_machine" "SessionRecovery"` at
`crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean:174`.
Case family: `consumerCoverage "session_recovery_cases" "SessionRecoveryCases"`
at `:225`. Same consumer covers both rows.

### Classification

Fully bound for SessionRecovery. `Proofs/Session/Properties/*` sub-modules
are Property-bound inside Lean and transitively bound to Rust via
QueueDeadline and Transcript.

## 18. Recovery

### What Lean models today

`Proofs/Recovery/Contract.lean` and `Proofs/Recovery/ContractCases.lean`
define the recovery-sweep framework. Five sweep families: `DetachedBridge`,
`Inference`, `Registry`, `RequestResponse`, `ToolCalls` under
`crates/defra-agent/proofs/Proofs/Recovery/Sweeps/`, barreled via
`Proofs/Recovery/Sweeps.lean`. Each case carries `sweep_id`, `cadence`
(startup), `implementation_status`, `measure_before/after`, `terminal_state`,
`deadline_audit_ref`.

### What is emitted today

`recovery_sweep_cases` (19 rows) at
`crates/defra-agent/proofs/Proofs/Conformance/Contracts/Json/Snapshot.lean:93`.
Five distinct `sweep_id`s asserted by the consumer at
`crates/defra-agent/tests/state_machine_conformance/recovery_sweeps.rs:14`:
`request_lifecycle_recover_all_requests`,
`request_lifecycle_recover_all_streaming_responses`,
`tool_call_lifecycle_recover_all_running_calls`,
`tool_call_lifecycle_recover_detached_bridge_rows`,
`inference_call_recover_all_stale_calls`. `Sweeps/Registry.lean` is the
registry-meta module; it does not emit a distinct `sweep_id`.

### Rust consumer state

`generated_recovery_sweep_cases_drive_startup_recovery_contract` at
`crates/defra-agent/tests/state_machine_conformance/recovery_sweeps.rs:6`
dispatches each case by `case.collection` to:

- `drive_request_recovery_case` (`:79`) — calls
  `RequestLifecycle::recover_all(&db.node, AGENT_DID)` at `:93`.
- `drive_response_recovery_case` (`:111`) — same recover_all, asserts
  `responses_recovered == 1` at `:127`.
- `drive_tool_call_recovery_case` (`:142`) — calls
  `ToolCallLifecycle::recover_all(&db.node, AGENT_DID)` at `:156`. Covers
  both `tool_call_lifecycle_recover_all_running_calls` (running /
  parent-interrupted / deadline-exceeded / subagent-child-completed variants)
  and `tool_call_lifecycle_recover_detached_bridge_rows` (detached-bridge
  cases including `detached_bridge_deadline_exceeded_to_timed_out`).
- `drive_inference_call_recovery_case` (`:187`) — calls
  `InferenceCall::recover_all(&db.node, AGENT_DID)` at `:217` and re-runs
  `reconstructed_running_slot_count` at `:235`.

All four sweep families exercise production `*::recover_all` entry points.
`assert_recovery_case_metadata` at `:38` requires
`case.implementation_status == "implemented"` for every emitted row —
no remaining `obligation` placeholders. Allowlist row at
`crates/defra-agent/tests/support/conformance_consumers.rs:276`.

### Coverage ledger row(s)

`consumerCoverage "recovery_sweep_cases" "RecoverySweepCases"` at
`crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean:297`.
Promoted by #239 (commit `454de05`) from `consumerWithFollowUpCoverage` to
`consumerCoverage` with the consumer renamed from `..._pin_...` to
`..._drive_...`.

### Classification

Fully bound. #248 ("soak closeout gate", commit `eb79341`) is Rust-only and
does not add a Lean row.

## 19. Fleet

### What Lean models today

`FleetState` aggregating per-backend slot accounting at
`crates/defra-agent/proofs/Proofs/Fleet/State.lean:15`, `slotContribution`
at `:41`, `slotCountFor` at `:45`, `slotAccountingInvariant` at `:49`,
decidable guards `CanAcquire` / `CanBegin` / `CanRelease` at `:53`/`:67`/`:78`.
Five-constructor relational machine at
`crates/defra-agent/proofs/Proofs/Fleet/Transition.lean:6`. Executable
mirror plus round-trip theorems at
`crates/defra-agent/proofs/Proofs/Fleet/Executable.lean:15`. Slot-accounting
lemmas in `Properties.lean:5`. Decomposition from `Fleet.lean` into the four
subfiles (#241) is confirmed.

### What is emitted today

`fleet_slot_accounting_cases` at
`crates/defra-agent/proofs/Proofs/Conformance/Contracts/Json/Snapshot.lean:57`,
five rows built by `fleetSlotAccountingCases` at
`crates/defra-agent/proofs/Proofs/Conformance/ContractCases/SlotAccounting.lean:285`.
Each row carries `slotCount`, `reconstructedRunningCount`, `maxConcurrent`,
`boundedByMaxConcurrent`, and `aggregateReconstructedNotPersisted := true`
at `ContractCases/SlotAccounting.lean:282`. NOT in the state-machine
catalog — `crates/defra-agent/proofs/Proofs/Conformance/Contracts/Machines/Catalog.lean:57`
omits Fleet by design.

### Rust consumer state

`generated_slot_accounting_fleet_cases_match_admission_runtime_boundary` at
`crates/defra-agent/src/admission/tests.rs:425`. Pulls named cases via
`lean_fleet_slot_accounting_case(...)` at `:426`, reconciles
`AdmissionRegistry` against the Lean backend id at `:440`, drives real
`acquire_current_call` futures across waiting / acquired / executing /
released states under `scope_request`, asserts each row matches a real
`InferenceCall` row via `assert_fleet_case_matches_call_row` at `:459` /
`:473` / `:484`, and exercises the `max_concurrent` bound on a separate
node at `:486`. Boundary statement at
`crates/defra-agent/proofs/Proofs/Conformance/Boundaries.lean:289`
(`boundaryFleetSlotAccountingDerivedViewId`) declares aggregate slot state
is reconstructed from `InferenceCall` rows. Allowlist row at
`crates/defra-agent/tests/support/conformance_consumers.rs:73`.

### Coverage ledger row(s)

`boundaryCoverage "fleet_cases" "FleetSlotAccounting" boundaryFleetSlotAccountingDerivedViewId "admission::tests::generated_slot_accounting_fleet_cases_match_admission_runtime_boundary"`
at `crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean:233`.

### Classification

Property-bound at the derived-view boundary. Lean exports a full relational +
executable Fleet machine; the emitted contract is the 5 boundary
slot-accounting witnesses, not a machine vocab/transition table, because
Rust intentionally never persists a `FleetState` aggregate.

## 20. Persistence

### What Lean models today

Four-state lifecycle `uncommitted -> committing -> {committed, lost}` with
`accumulate` self-loop on `uncommitted` at
`crates/defra-agent/proofs/Proofs/Persistence.lean:12`. `FailurePolicy` at
`:50`. Five transitions parameterized by policy at `:75`. Executable
`Action`/`step?`/`replay?` at `:96`. Round-trip proofs at `:136-225`.
Vocab parse/print round-trips at `:36`/`:68`.

### What is emitted today

Two `state_machines` rows + one `persistence_failure_policy_cases` array:

- `Persistence.failClosed` and `Persistence.failOpen` via `persistenceMachine`
  at `Proofs/Conformance/Contracts/Machines/Persistence.lean:23`, listed at
  `Catalog.lean:60-61`.
- `persistence_failure_policy_cases` (2 rows) at
  `Proofs/Conformance/Contracts/Json/Snapshot.lean:59`.
- Vocab: `PersistenceState` and `PersistenceFailurePolicy` at `Catalog.lean:25-26`.

### Rust consumer state

State-machine rows: `lean_executable_contracts_cover_initial_domains` at
`crates/defra-agent/tests/state_machine_conformance/coverage.rs:3` calls
`assert_state_machine_contract_is_complete("Persistence.failClosed")` and
`("Persistence.failOpen")` (domain loop at `:4-16`), pins specific
transitions at `:25-26`.

Cases: `generated_persistence_failure_policy_cases_match_hook_decisions`
at `crates/defra-agent/src/hook/tests.rs:184`. Calls
`decide_persistence_outcome(failure_policy_from_contract(&case.policy), ...)`
at `:191`, asserts `decision == case.hook_decision` at `:204`, pins counter
movements at `:206-216`, post-state pair at `:223-232`, and asserts
`!case.external_durability_claimed` at `:217`. Allowlist rows at
`crates/defra-agent/tests/support/conformance_consumers.rs:136` / `:325`.

### Coverage ledger row(s)

`boundaryCoverage "state_machine" "Persistence.failClosed"` and `.failOpen`
at `Proofs/Conformance/CoverageLedger.lean:146`/`:151`. Cases:
`boundaryCoverage "persistence_policy_cases" "PersistenceFailurePolicyCases"`
at `:238`. Boundary `boundaryStorageHookFailurePolicyId` at
`Proofs/Conformance/Boundaries.lean:231` (statement at `:314-320`).

### Classification

Fully bound (at the deliberate storage-hook boundary). Dual classification —
state-machine boundary rows for the abstract lifecycle plus a
`persistence_policy_cases` boundary row for the hook decision projection —
is the intentional shape per `boundaryPersistenceAbstractLifecycleId`
(no per-token persisted state) and `boundaryStorageHookFailurePolicyId`
(hook fail-open/fail-closed is the runtime surface).

## 21. StorageObservation

### What Lean models today

Seven-state daemon-visible observation lifecycle at
`crates/defra-agent/proofs/Proofs/StorageObservation.lean:18`. `toPersistence`
projection at `:78`. Ten transitions parameterized by failure policy at
`:104`. Executable `step?` at `:130`. Refinement theorems
(`begin_refines_persistence`, `success_refines_persistence`,
`failure_failClosed_refines_persistence`, `failure_failOpen_refines_persistence`)
at `:158-176`. Visibility-path theorems at `:208`/`:218`/`:232`/`:245`/`:258`/`:270`.
`terminalWriteObserved` + `terminal_write_observed_committed` at `:193`.

### What is emitted today

Two `state_machines` rows (`StorageObservation.failClosed`/`failOpen`) at
`Catalog.lean:62-63`; `storage_observation_runtime_cases` (8 rows) at
`Json/Snapshot.lean:62`; vocab `StorageObservation` at `Catalog.lean:29`.

### Rust consumer state

State machines: same `lean_executable_contracts_cover_initial_domains` at
`crates/defra-agent/tests/state_machine_conformance/coverage.rs:3` covers
both, with edge pins at `:27-72` (legal `noMutation -> inFlight`,
`mutationFailed -> noMutation` for failClosed, `mutationFailed ->
lostAcknowledged` for failOpen, and the cross-policy negative cases at
`:63-72`).

Runtime cases: `generated_storage_observation_cases_match_hook_runtime_classification`
at `crates/defra-agent/src/hook/tests.rs:238`. Creates real
`DefraSessionHook::with_identity(..., failure_policy_from_contract(...))`
at `:249`, drives `hook.apply_persistence_policy(...)` with success/failure
mutations at `:263`, asserts counter movements at `:273-283` and observation
post-state invariants at `:291-337`. Asserts `!case.external_visibility_claimed`
at `:285`. Boundary `boundaryStorageObservationDaemonVisibleId` at
`Proofs/Conformance/Boundaries.lean:234` (statement at `:321`). Allowlist
rows at `crates/defra-agent/tests/support/conformance_consumers.rs:143` /
`:325`.

### Coverage ledger row(s)

`boundaryCoverage "state_machine" "StorageObservation.failClosed"` and
`.failOpen` at `Proofs/Conformance/CoverageLedger.lean:156`/`:161`. Cases:
`boundaryCoverage "storage_observation_cases" "StorageObservationRuntimeCases"`
at `:243`.

### Classification

Fully bound (at the daemon-visible storage boundary). Refinement theorems
and visibility-path theorems are internal Lean-side closure facts justifying
the projection into `PersistenceState`; they are not contract-emitted but
underwrite the dual classification with `persistence_policy_cases`.

## 22. ApplyReconcile

### What Lean models today

Complete executable reference model in `Proofs/ApplyReconcile/*`:

- `Collection` + `applyOrder` at `ApplyReconcile/Collections.lean:17` / `:31`.
- `DesiredFields` / `LiveFields` / `Manifest` / `LiveState` / `Manifest.WellFormed`
  at `ApplyReconcile/Manifest.lean:17` / `:66`.
- `ApplyStep` (create/update only), `diff` at `ApplyReconcile/Diff.lean:15`
  / `:51`.
- `applyOne` / `applyAll` desired-only at `ApplyReconcile/Apply.lean:11`;
  `apply_preserves_live` at `ApplyReconcile/ApplyProperties.lean:17`.
- Runtime bridge at `ApplyReconcile/RuntimeBridge.lean:61`.
- Convergence theorems at `ApplyReconcile/Convergence.lean:16` / `:70` /
  `:80` / `:106`.
- Case-emission types decomposed by #246 into `ContractCases/{Diff,Fixtures,Json,Types}.lean`.
  `ApplyReconcileCase` declared at
  `crates/defra-agent/proofs/Proofs/ApplyReconcile/ContractCases/Types.lean:47`
  carrying `expectedExternalStateAfterAbort : List ContractLiveDoc` at `:52`
  (the #246 addition).

### What is emitted today

`apply_reconcile_cases` at
`crates/defra-agent/proofs/Proofs/Conformance/Contracts/Json/Snapshot.lean:51`.
Each case emits `expected_external_state_after_abort` at
`crates/defra-agent/proofs/Proofs/ApplyReconcile/ContractCases/Json.lean:66-67`.
Seven cases per `crates/defra-agent/tests/state_machine_conformance/coverage.rs:122`.
Per-case fields include `expectedSelectedCreateDocs`, `expectedSelectedUpdateDocs`,
`expectedSelectedWrites`, `expectedWriteOrder`,
`productionPrefixesReferrersClosed`, `writeOrderPrefixSafe`
(`ContractCases/Types.lean:58-73`). ApplyReconcile is NOT in the catalog.

### Rust consumer state

Two consumers; only the production write-boundary is the registered ledger
consumer:

1. Reference-model in `crates/defra-agent/tests/apply_conformance.rs` —
   imports `defra_agent::apply_model`, runs the reference `diff` /
   `apply_all` against the seven generated cases. Not in the consumer
   registry; useful as an internal check.
2. Production write-boundary:
   `generated_apply_reconcile_cases_fence_production_apply_write_boundary`
   at `crates/defra-agent-cli/src/config_import.rs:885` (module
   `lean_apply_write_boundary_tests` at `:793`). For each case:
   - Asserts `write_order_prefix_safe` and `production_prefixes_referrers_closed`
     at `:896-897`.
   - Builds `desired_state::export_bundle_from_manifest` at `:901` and drives
     `apply_desired_state_changes(&txn, &desired_bundle, &planned)` against
     a recording fake at `:914`.
   - Asserts committed mutation sequence equals Lean's
     `expected_selected_writes` doc-for-doc at `:936`.
   - Success path: `(begin, commit, discard) == (1, 1, 0)` at `:945`.
   - Failure path (when `case.prefix_len > 0`): seeds `pre_live` as initial
     committed state at `:953`, installs `install_fail_at("0", case.prefix_len)`
     at `:968`, runs apply, asserts `result.is_err()` at `:971`, asserts
     `(begin, commit, discard) == (1, 0, 1)` at `:982` (fencing #228's
     transactional discard), and asserts
     `recorder.committed_state() == case.expected_external_state_after_abort`
     doc-for-doc at `:1000` (fencing #246).

Allowlist row at `crates/defra-agent/tests/support/conformance_consumers.rs:115`.

### Coverage ledger row(s)

`consumerCoverage "apply_reconcile_cases" "ApplyReconcileCases" "config_import::lean_apply_write_boundary_tests::generated_apply_reconcile_cases_fence_production_apply_write_boundary"`
at `crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean:221`.

### Classification

Fully bound (case-only — no state-machine row; ApplyReconcile is a
pure-functional diff + write-boundary projection, not a persisted machine).

### Smallest delta

None for the current create/update/no-op semantics. Open items track
future work:

- #57 delete semantics: Lean must extend `ApplyStep` with a delete
  constructor and emit cases where `expected_external_state_after_abort`
  diverges from `pre_live`.
- #56 lands fully once the failure-path discard assertion is green across
  every case; today it is wired through but as one of the production-boundary
  projections rather than its own contract row.

## 23. Scheduling

### What Lean models today

`Proofs/Scheduling.lean` is a vocabulary + scheduler-state module, not a
state machine: `ExecutionOrigin` at `:10`, `BackendId` at `:35`,
`BackendState` at `:40`, `AdmissionState` at `:46` with `holdsSlot` at `:56`,
aggregate `SchedulerState` at `:75` with `capacityInvariant` at `:94`.

S7-S9 scheduling-safety theorems are proved over `FleetState.Transition`,
not a `Scheduling` machine: `capacity_invariant_preserved` and
`slot_accounting_preserved` at
`crates/defra-agent/proofs/Proofs/Properties/SchedulingSafety.lean:9` / `:42`.

### What is emitted today

No `Scheduling` machine in the `stateMachines` catalog. Scheduling-flavored
contract rows are spread across siblings:

- `ExecutionOrigin` vocab at `Catalog.lean:22`.
- `slot_cases` (`InferenceCallSlotAccounting`) at `Proofs/Conformance/CoverageLedger.lean:229`.
- `fleet_cases` (the aggregate slot view; see §19) at `:233`.
- `backend_health_cases` (5 cases) at `:248`.
- `queue_deadline_cases` (5 cases) at `:294`.

No per-scheduler-step transition table is emitted — scheduler steps fold
into the `InferenceCall` state machine and the `Fleet` derived view.

### Rust consumer state

- `slot_cases` → `generated_inference_slot_accounting_cases_match_admission_reconstruction_logic`
  at `crates/defra-agent/src/admission/tests.rs:354`.
- `fleet_cases` → `generated_slot_accounting_fleet_cases_match_admission_runtime_boundary`
  at `:425`.
- `backend_health_cases` → `generated_backend_health_admission_cases_match_registry_and_admission_policy`
  at `crates/defra-agent/src/backend_registry/tests.rs:91`.
- `queue_deadline_cases` → `generated_queue_deadline_cases_pin_r4a_contract_rows`
  at `crates/defra-agent/tests/state_machine_conformance/tooling_slots_queue_command.rs:276`.

S7-S9 theorems are Lean-internal — they discharge an obligation that the
InferenceCall + Fleet transitions preserve `capacityInvariant` and
`slotAccountingInvariant`, which the Rust consumers above sample at the
runtime boundary.

### Coverage ledger row(s)

Four rows: `:229` (slot), `:233` (fleet), `:248` (backend_health), `:294`
(queue_deadline) in `Proofs/Conformance/CoverageLedger.lean`.

### Classification

Property-bound (vocab-only at the module level; case-bound across four
sibling ledger rows). The decomposition is intentional: `Scheduling.lean`
provides types, `Fleet/*` proves aggregate properties, `InferenceCall`
carries the per-row transitions, `queue_deadline_cases` fences
claim/deadline behavior.

## 24. Client / ClientShell

### What Lean models today

**Client** (`Proofs/Client/*`): pure derivation of a client-visible turn state
from replicated AgentRequest/AgentResponse snapshots. `ClientTurnState`
(6 states) at `crates/defra-agent/proofs/Proofs/Client/Types.lean:10`.
`RequestSnapshot`/`ResponseSnapshot`/`AttemptView` at `:55`/`:68`/`:74`.
`deriveAttempt` at `:88` and `deriveTurn` at `:117`. T-series theorems in
`Client/Lifecycle.lean` (T4 totality at `:18`/`:23`; T2 monotonicity at `:36`),
`Client/Terminal.lean`, `Client/Replacement.lean`.

**ClientShell** (`Proofs/ClientShell/*`): React-shell state machine layered
above Client. `SessionObservation` at
`crates/defra-agent/proofs/Proofs/ClientShell/Types.lean:25`. `Selection`,
`SubmissionWorkflow`, `ShellState` at `:64`/`:83`/`:92`. `TransportHealth`
at `:56`. C1–C4', C9 theorems in
`crates/defra-agent/proofs/Proofs/ClientShell/Theorems.lean:19`. `projectChat`
/`projectChatShell` at `Proofs/ClientShell/Projection.lean:36`.

### What is emitted today

Client itself emits no contract domain. ClientShell emits two:

- `frontend_client_shell_cases` (15 rows) at
  `crates/defra-agent/proofs/Proofs/Conformance/Contracts/Json/Snapshot.lean:43`,
  built by `frontendClientShellCases` at
  `crates/defra-agent/proofs/Proofs/Conformance/ClientShell/Contracts/Json.lean:75`.
  Frontend fields at `:38-64`.
- `desktop_client_shell_cases` (12 rows) at `Snapshot.lean:47`, from
  `desktopClientShellCases` (filter `desktopSelectedSessionId.isSome`) at
  `ClientShell/Contracts/Json.lean:84`. Desktop fields at `:65-73`.

Counts pinned at `crates/defra-agent/tests/state_machine_conformance/coverage.rs:157`/`:165`.

### Rust consumer state

- Frontend: TS test `projectChatShell matches generated Lean ClientShell projection contracts`
  at `apps/desktop-tauri/src/lib/chat-shell.test.ts:304`. Loads
  `loadLeanClientShellCases()` at `:305`, drives `projectChatShell({...})`
  across all 15 cases, asserts `projection.workflow`, `activeRequestId`,
  `turnState`, `sendStatus` against Lean-expected fields at `:320-334`.
- Desktop: `session_snapshot_projection_consumes_generated_client_shell_contract_cases`
  at `apps/desktop-tauri/src-tauri/src/bridge/snapshot/tests/session_state.rs:254`.
  Consumes `lean_desktop_client_shell_cases()` at `:255`, builds
  `client_shell_contract_store(case)` at `:264`, drives
  `build_session_snapshot_from_store(...)` at `:271`, asserts
  `snapshot.{latest_request_id,turn_state,pending_turn}` at `:287-305`. The
  desktop test references `Proofs.ClientShell.C9` in a comment at `:247`.

Client has NO direct Rust consumer; `deriveAttempt`/`deriveTurn` are
transitively consumed because ClientShell rows carry `frontend_session_turn_state`
/ `desktop_observed_turn_state` whose values are computed in Lean via
`deriveTurn`.

### Coverage ledger row(s)

- `frontend_client_shell_cases` at `Proofs/Conformance/CoverageLedger.lean:261`.
- `desktop_client_shell_cases` at `:265`.
- `live_overlay_cases` at `:269` covers a different concern (single-attempt
  overlay) via `live_overlay_conformance::live_overlay_cases_match_lean_table`
  at `crates/defra-agent/tests/live_overlay_conformance.rs:62`.

No separate ledger row for Client.

### Classification

ClientShell: Fully bound (dual frontend TS + desktop Rust consumers).
Client: Reference-only — turn derivation is encoded into the ClientShell
case fields rather than fenced by a sibling Client-specific surface.

## 25. ReversePairingHandlers

### What Lean models today

A handler-shape model, not a state machine, at
`crates/defra-agent/proofs/Proofs/ReversePairingHandlers.lean:1` (50 lines).
`Collection` is reused from ApplyReconcile (`:16`). `ReceiverState` holds
`subscribed : Finset Collection` at `:19`. Two pure handler effects:
`applyInstall` at `:23` and `applyTeardown` at `:27`. Four idempotency /
commutativity theorems at `:30`/`:34`/`:38`/`:42`. The file header (`:5-11`)
states this discharges the "handler idempotency obligation surfaced by the
TLA+ reverse-pairing model." Substrate fairness lives separately in
`tla/ReversePairing.tla` (referenced from
`crates/defra-agent/proofs/Proofs/Conformance/Boundaries.lean:361`).

### What is emitted today

Nothing. `ReversePairingHandlers` is not imported by
`Proofs/Conformance/Contracts.lean` or its JSON snapshot chain. The only
`reverse_pairing` Lean references are a deviation entry at
`Proofs/Conformance/Deviations.lean:50` and the boundary at
`Proofs/Conformance/Boundaries.lean:355`, both pointing at substrate-level
fairness.

### Rust consumer state

None.

### Coverage ledger row(s)

None. No contract domain is emitted.

### Classification

N/A (handler-shape proofs, not a binding-eligible state machine).

## 26. CrossMachineComposed

### What Lean models today

Composition barrel + state and theorems combining Process / Request /
Persistence / InferenceCall / ToolExecution / ManagedExec into one
`ComposedState`. `Proofs/CrossMachineComposed.lean:1` imports State,
ToolTermination, Foreground, UniqueCallIds. `ComposedState` at
`Proofs/CrossMachineComposed/State.lean:22`; composed `Transition` at `:92`.
`Coherent` predicate at `:62`. `invFG` foreground-blocking invariant at
`Proofs/CrossMachineComposed/Foreground.lean:21`. C1/C1'/C2/C3 composition
theorems referenced from `Proofs/ToolExecution/Properties.lean:7`.
`CrossMachineComposed` is the substrate for `Proofs/Properties/Safety.lean`
and `Proofs/Properties/Liveness.lean`.

### What is emitted today

Nothing. Not imported by `Proofs/Conformance/Contracts.lean` or the
snapshot chain.

### Rust consumer state

None directly. Cross-machine properties are sampled transitively through
the per-machine bindings (Request, Process, Persistence.*, StorageObservation.*,
InferenceCall, ToolCall, ManagedExec are all bound). There is no
`composed_state_cases` ledger row.

### Coverage ledger row(s)

None.

### Classification

N/A (composition glue).

## Cross-cutting analysis

### Coverage ledger accuracy

The ledger has 72 entries (`grep -c '...Coverage' Proofs/Conformance/CoverageLedger.lean`)
and the consumer registry has 55 entries (`grep -c 'ConformanceConsumer::'
tests/support/conformance_consumers.rs`); the count delta is real and
expected — some consumers cover multiple ledger rows (e.g.,
`state_machine_conformance::generated_session_recovery_cases_drive_db_backed_reissue_contract`
covers both the `state_machine` row at `:174` and the `session_recovery_cases`
row at `:225`).

Spot-checked entries against the named consumers (all verified existing
verbatim in the registry):

- `lifecycle::tests::request_state_machine_contract_is_complete` (ledger `:138`)
  → registry `:171`. Resolves to `#[test]` at `crates/defra-agent/src/lifecycle.rs:268`.
- `tool_call_lifecycle::tests::rust_cancel_cause_vocabulary_matches_lean_model`
  (ledger `:114`) → registry `:423`. The #247 addition.
- `agent::reconcile::tests::pairing_reconcile_state_machine_contract_is_complete`
  (ledger `:170`) → registry `:108`. Resolves to `#[tokio::test]` at
  `crates/defra-agent/src/agent/reconcile/tests.rs:79`.
- `identity_conformance::identity_respects_principal_contract_enforced_by_runtime_routing`
  (ledger `:322`) → registry `:163`. The #193 rename.
- `state_machine_conformance::generated_compaction_reducer_cases_pin_contract`
  (ledger `:330`) → registry `:311`. Resolves to
  `crates/defra-agent/tests/state_machine_conformance/streaming_compaction.rs:460`.

The cross-check enforcement at
`crates/defra-agent/tests/state_machine_conformance/coverage.rs:718-736`
asserts (a) every emitted domain has a ledger entry, (b) every ledger
consumer resolves to a registered consumer, (c) zero registered consumers
are unreferenced. Three drift bugs from the 2026-05-14 audit are closed.

### Conformance pipeline freshness

`crates/defra-agent/tests/state_machine_conformance.rs` (246 lines) is still
the central runner, dispatching into eleven sub-modules under
`tests/state_machine_conformance/`. The decomposition is healthy — every
`#[test]` / `#[tokio::test]` at the top level either calls into a sub-module
or is a small inline check (e.g., `managed_exec_liveness_cases_pin_native_process_boundary`
at `:137`). No orphaned snapshot fields: every field in `LeanContractSnapshot`
at `crates/defra-agent/src/lean_vocab_test.rs:27-79` has an accessor at
`:217-489`, and the drift test at `coverage.rs:391` enumerates every one.

`valid_categories` at `coverage.rs:629-662` (32 categories) covers every
emitted domain — the 2026-05-14 audit's §1 / §2 / §3 drift bugs are gone.

### Documentary hypotheses

`grep -rn "documentary" crates/defra-agent/proofs/` returns no hits. The
May-15 audit's stale-documentary follow-up is closed; #247 cleaned up C1'/C2
in `Proofs/CrossMachineComposed/` per the commit message.

### New Lean machines and decompositions since 2026-05-15

Commits touching `crates/defra-agent/proofs/` between 2026-05-15 and
2026-05-19 (via `git log --since=2026-05-15 -- crates/defra-agent/proofs/`):

- **#247** (`0191870`, 2026-05-19): ToolCall CancelCause split; new file
  `Proofs/ToolExecution/CancelCause.lean`; ToolCall machine actions now
  generated via `flatMap` over `CancelCause.all` at
  `Proofs/Conformance/Contracts/Machines/ToolCall.lean:23`. **Wired** on
  both sides at landing (`tool_call_lifecycle.rs:541`, registry `:423`).
- **#246** (`c7d8a72`, 2026-05-19): ApplyReconcile case decomposition into
  `ContractCases/{Diff,Fixtures,Json,Types}.lean`; new field
  `expectedExternalStateAfterAbort` at `ContractCases/Types.lean:52` consumed
  at `config_import.rs:996/1002`. **Wired** at landing.
- **#240** (`904de34`, 2026-05-19): defradb.rs P2P subscription persistence
  recorded as Lean deviation
  `defradb_rs_p2p_subscription_state_not_durable` at
  `Proofs/Conformance/Deviations.lean:49`. **Tracked as deviation by design.**
- **#229** (`78c78ab`, 2026-05-19): full `Proofs/ManagedExec/*` subtree.
  **Wired** at landing (`managed_exec/tests.rs:27`, registry `:213`).
- **#239** (`454de05`, 2026-05-18): Recovery sweep promoted from "pin" to
  "drive"; ledger consumer renamed to
  `generated_recovery_sweep_cases_drive_startup_recovery_contract`. **Wired.**
- **#238** (`7c9c523`, 2026-05-18): queue/deadline cases driven through
  runtime — `generated_queue_deadline_cases_pin_r4a_contract_rows`
  consumer at `tests/state_machine_conformance/tooling_slots_queue_command.rs:276`.
  **Wired.**
- **#237** (`74057fc`, 2026-05-18): streaming + compaction emitters added.
  Closes both 2026-05-14 dangling-PR rows simultaneously. **Wired** on both
  sides; ledger flipped from `consumerWithFollowUpCoverage` to
  `consumerCoverage`.
- **#193 / #227** (`3d76af9`, 2026-05-18): DefraAgent refactor to typed
  `AgentPrincipal` + `AgentBehavior`; `identity_contracts.enforced` flipped
  to `true`; new `identity_permission_cases` ledger row (with explicit
  follow-up for the remaining decision/hostability work). **Wired** for
  routing; permission-decision + hostability still pending.

Background also has new properties files (`Background/Properties/{Budget,Cancellation,Foreground,Projection,Structure,Unique}.lean`)
landed via the same window — these are NOT wired to Rust witnesses (see §8).

### Stale or weak ledger classifications

Three ledger rows currently overstate their binding strength:

1. `Proofs/Conformance/CoverageLedger.lean:334` (event_delivery_transition_cases)
   and `:342` (event_delivery_convergence_traces) — `consumerCoverage` but
   consumer uses an in-test simulator at
   `tests/state_machine_conformance/event_delivery.rs:117`, not the
   production loops. Demote to `consumerWithFollowUpCoverage`.
2. `Proofs/Conformance/CoverageLedger.lean:346` (mcp_health_cases) —
   `consumerCoverage` for the whole domain but Rust only handles K=1; K≥2
   rows are unconsumed. Demote to `consumerWithFollowUpCoverage`.

Two ledger rows now understate their binding:

3. `Proofs/Conformance/CoverageLedger.lean:309` (transcript_cases) — consumer
   name says `..._pin_...` but is now a runtime drive of `DefraSessionHook`
   per case (see §13). Cosmetic rename only.
4. `Proofs/Conformance/CoverageLedger.lean:166` (RuntimeReconcile
   state_machine) — consumer string is the vocab test, not the
   conventionally-named transition-table test. Cosmetic rename only.

## Recommended next-impl order

Force-ranked to five. Each entry: rationale, smallest delta, Lean-first /
Rust-first / coupled.

1. **EventDelivery: drive production sources, not the simulator.** Rust-first.
   The three `event_delivery_cases` ledger rows at
   `Proofs/Conformance/CoverageLedger.lean:334`/`:338`/`:342` were the
   2026-05-14 audit's #6 ranked gap and still misrepresent runtime drive.
   Replace `InMemoryEventDeliverySource` at
   `tests/state_machine_conformance/event_delivery.rs:117` with a thin
   driver that posts Lean actions against the real `DefraWatcher` /
   `EventSource` / `SubagentSource` loops. Until that lands, flip the two
   simulator-backed ledger rows from `consumerCoverage` to
   `consumerWithFollowUpCoverage` so the audit honestly states the gap.
   **Small enough to dispatch as a single issue + PR** once the in-memory
   subscription mock pattern is agreed (the source-instances row already
   shows the production-trait shape).

2. **MCPHealth: drive `run_health_check` and consume K≥2.** Coupled. The
   K=1 simulator at `crates/defra-agent/src/health_checker.rs:400` mirrors
   inline logic instead of calling production code, and Lean's K≥2 rows
   (~2/3 of the emitted cases) are entirely unconsumed. Stage one (Rust-first):
   change the K=1 consumer to drive `run_health_check`. Stage two
   (coupled): land the K≥2 backoff behavior in Rust and drop the
   `lean_mcp_health_k1_cases()` filter. Until then, demote the ledger row
   at `Proofs/Conformance/CoverageLedger.lean:346` to
   `consumerWithFollowUpCoverage`. **Stage one is small enough to dispatch
   as a single issue + PR; stage two needs its own design pass.**

3. **Identity permission decision + hostability.** Rust-first (Lean is
   already shape-complete). Last open piece of #193: introduce a runtime
   permission-decision entry point and a deployment hostability lookup,
   then consume `expected_actor_allowed` / `expected_peer_allowed` /
   `expected_actor_hostable` / `expected_peer_hostable` in
   `crates/defra-agent/tests/identity_conformance.rs:177`. When both pass,
   promote `Proofs/Conformance/CoverageLedger.lean:317` from
   `consumerWithFollowUpCoverage` to `consumerCoverage`. **Wants its own
   design pass** because deployment placement is a new runtime concern.

4. **Background properties: emit witness rows or accept as Lean-only.**
   Lean-first. The six theorems in
   `Proofs/Background/Properties/{Budget,Cancellation,Foreground,Projection,Structure,Unique}.lean`
   have no Rust witness and no ledger entry — they are invisible to the
   drift test. Either extend `Proofs/Conformance/ContractCases/R6Background.lean`
   with a `theorem_witness` discriminator emitting one row per theorem and
   wiring a runtime drive (preferred for `cascade_cancels_child` and
   `backgrounded_budget_bounded`, which are operationally testable), or
   add `followUpCoverage` rows recording that the remaining theorems stay
   Lean-only. **Small enough to dispatch as a single issue + PR for the
   ledger-row half; the runtime-drive half wants its own design pass.**

5. **Cosmetic ledger / file cleanup (bundle).** Lean-first, no design
   pass. Three small fixes:
   - Rename `crates/defra-agent/proofs/Proofs/Conformance/Contracts/Machines/PairingSession.lean`
     to `PairingReconcile.lean` and split `sessionRecoveryMachine` into a
     sibling file so file/machine mapping is grep-stable.
   - Rename `state_machine_conformance::generated_transcript_cases_pin_agent_message_ordering_contract`
     to `..._drive_...` so it matches the post-#239 naming convention (see
     §13).
   - Either rename `runtime_status::tests::rust_reconcile_phase_vocabulary_matches_lean_model`
     in the ledger consumer string at
     `Proofs/Conformance/CoverageLedger.lean:166` to a dedicated
     `..._state_machine_contract_is_complete` test, or split the colocated
     assertions into two `#[test]` functions and update the ledger
     accordingly. **Single bundled PR.**

Items 1, 4, and 5 are small enough to ship as single issues + PRs. Items 2
and 3 each want their own design pass before implementation. Out of scope
for this audit: the #155 TLA+ / P2P track, the `#57` delete semantics track
(both deferred icebox).
