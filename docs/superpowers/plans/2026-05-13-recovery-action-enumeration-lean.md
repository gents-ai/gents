# Recovery-Action Enumeration in Lean - Implementation Plan

> Design approved by Jack on 2026-05-13 with one refinement: split the
> implemented tool-call startup sweep and the detached-bridge recovery
> obligation into separate registry rows.

**Goal:** Add a Lean-owned recovery-sweep registry that enumerates persisted startup recovery actions, proves each registered sweep drives stale rows to terminal in finite time, and emits conformance vectors for Rust consumers. This closes issue #189 at the model/contract layer and records the missing Rust obligations for deadline audit follow-ups #4 and #6.

**Architecture:** A new `Proofs.Recovery` module owns the contract type, concrete sweep registrations, finite convergence theorems, and conformance cases. Existing request L3 liveness remains unchanged. The conformance JSON gains `recovery_sweep_cases`, and `CoverageLedger` gains `recovery_sweep_cases` entries with implemented consumers or explicit follow-up coverage.

**Non-goals:**

- No Rust production recovery implementation.
- No edits to `Proofs/Session/Transcript.lean` or `Proofs/Session/State.lean`.
- No edits under `crates/defra-agent/proofs/tla/`.
- No full `AgentResponse` streaming lifecycle model beyond startup recovery from `streaming` to `error`.
- No `AgentConversation` terminalization contract unless Jack changes the approved design.

## Files

Create:

```text
crates/defra-agent/proofs/Proofs/Recovery/Contract.lean
crates/defra-agent/proofs/Proofs/Recovery/Sweeps.lean
crates/defra-agent/proofs/Proofs/Recovery/ContractCases.lean
crates/defra-agent/proofs/Proofs/Recovery.lean
```

Modify:

```text
crates/defra-agent/proofs/Proofs.lean
crates/defra-agent/proofs/Proofs/Conformance/ContractCases/Types.lean
crates/defra-agent/proofs/Proofs/Conformance/Contracts/Json.lean
crates/defra-agent/proofs/Proofs/Conformance/CoverageLedger.lean
crates/defra-agent/src/lean_vocab_test.rs
crates/defra-agent/tests/state_machine_conformance.rs
crates/defra-agent/tests/support/conformance_consumers.rs
```

The Rust files above are test/conformance harness code only, not production recovery code.

## Task 1: Confirm Approved Design Baseline

- [x] Confirm Jack approves the three design questions:
  - `InferenceCall` terminal mapping.
  - `AgentConversation` exclusion from v1.
  - `Proofs.lean` import placement.
- [x] Update the design doc status from `Draft pending Jack approval` to `Approved`.
- [x] Split tool-call recovery into two registry rows:
  - `tool_call_lifecycle_recover_all_running_calls` with `implemented` status.
  - `tool_call_lifecycle_recover_detached_bridge_rows` with `obligation` status.

Verification:

```bash
git diff -- docs/superpowers/specs/2026-05-13-recovery-action-enumeration-lean-design.md
```

## Task 2: Add Recovery Contract Core

Create `Proofs/Recovery/Contract.lean`.

- [x] Define `RecoveryCadence`, `RecoveryImplementationStatus`, and `PersistedRecoveryCollection`.
- [x] Define `PersistedRecoveryCollection.toContract`, `.all`, and completeness theorem over `.all`.
- [x] Define dependent `RecoverySweep`.
- [x] Define aggregate finite-list measure helpers.
- [x] Prove generic finite convergence:
  - single stale row recovery reaches terminal;
  - recovered stale row measure is zero;
  - finite list recovery reaches zero aggregate stale measure.

Verification:

```bash
cd crates/defra-agent/proofs
lake env lean Proofs/Recovery/Contract.lean
```

## Task 3: Register Concrete Sweeps

Create `Proofs/Recovery/Sweeps.lean`.

- [x] Request sweep:
  - row model over `RequestContext`;
  - stale states `claimed` and `processing`;
  - terminal mapping to conservative `failed/released`;
  - theorem tying the row-level terminal result to existing `isTerminal`.
- [x] Streaming response sweep:
  - minimal response status enum for `streaming`, `complete`, `error`;
  - stale `streaming`;
  - terminal mapping `error`.
- [x] Tool-call sweep:
  - row model over `ToolExecution.ToolCallContext` plus recovery cause;
  - include deadline, parent interrupted, parent terminal, and child terminal bridge causes;
  - exclude detached bridge rows from this implemented predicate;
  - prove every actionable running row maps to a terminal tool state;
- [x] Detached bridge obligation sweep:
  - row model over `ToolExecution.ToolCallContext` plus detached bridge terminalizing cause;
  - mark the sweep `RecoveryImplementationStatus.obligation`;
  - prove detached rows with a terminalizing cause are stale and map to a terminal state;
  - terminal mapping follows the bridge contract: child completed -> `completed`, child interrupted -> `cancelled`, other child terminal/terminal parent -> `failed`, deadline exceeded -> `timedOut`.
- [x] Inference-call sweep obligation:
  - row model over `InferenceCall` plus stale cause;
  - map stale rows to terminal states per approved design;
  - prove terminal rows contribute zero backend slots.
- [x] Define `registeredRecoverySweeps`.
- [x] Prove `registered_sweeps_cover_persisted_collections`.

Verification:

```bash
cd crates/defra-agent/proofs
lake env lean Proofs/Recovery/Sweeps.lean
```

## Task 4: Emit Conformance Cases

Create `Proofs/Recovery/ContractCases.lean`.

- [x] Define `RecoverySweepCase` rows with stable strings:
  - sweep id;
  - collection;
  - Rust function;
  - cadence;
  - implementation status;
  - pre state;
  - terminal state;
  - measure before and after;
  - deadline audit reference.
- [x] Emit cases for:
  - request `claimed` and `processing`;
  - response `streaming`;
  - implemented tool sweep: timed out, parent interrupted, parent terminal, child completed, child failed, child interrupted;
  - detached bridge obligation sweep: detached child completed, detached child failed, detached child interrupted, detached terminal parent, detached deadline exceeded;
  - inference queued stale, running stale, interrupted-parent stale.
- [x] Add a theorem that every emitted case corresponds to a registered sweep collection.

Verification:

```bash
cd crates/defra-agent/proofs
lake env lean Proofs/Recovery/ContractCases.lean
```

## Task 5: Wire Lean Imports and JSON

- [x] Add `import Proofs.Recovery` to `Proofs.lean` after `Proofs.Properties.Liveness`.
- [x] Add `RecoverySweepCase` to `Proofs/Conformance/ContractCases/Types.lean`.
- [x] Import `Proofs.Recovery.ContractCases` in `Proofs/Conformance/Contracts/Json.lean`.
- [x] Add `recoverySweepCaseJson`.
- [x] Add `recovery_sweep_cases` to `snapshotJson`.
- [x] Add `recovery_sweep_cases` coverage rows in `CoverageLedger.lean`.

Verification:

```bash
cd crates/defra-agent/proofs
lake env lean Proofs.lean
lake env lean --run Proofs/Conformance/Contracts.lean
```

## Task 6: Update Rust Conformance Harness

No production Rust changes.

- [x] Add `LeanRecoverySweepCase` parsing in `crates/defra-agent/src/lean_vocab_test.rs`.
- [x] Add accessors for recovery sweep cases.
- [x] Add a state-machine conformance test that:
  - asserts all expected sweep ids are present;
  - asserts `implementation_status = obligation` for missing Rust paths;
  - asserts detached bridge cases are represented and not marked as skipped;
  - asserts inference terminal cases reconstruct zero slots when mapped through existing slot-accounting helpers.
- [x] Register the new test consumer in `tests/support/conformance_consumers.rs`.

Verification:

```bash
cargo test -p defra-agent state_machine_conformance::generated_recovery_sweep_cases_pin_startup_recovery_contract
cargo test -p defra-agent state_machine_conformance::coverage_ledger_domains_are_consumed_or_explicitly_accepted
```

## Task 7: Full Verification

Run the focused Lean and Rust checks first, then the broader proof command.

```bash
cd crates/defra-agent/proofs
lake env lean Proofs.lean
lake env lean --run Proofs/Conformance/Contracts.lean
cd ../../..
cargo test -p defra-agent state_machine_conformance
cargo test -p defra-agent admission::tests::generated_inference_slot_accounting_cases_match_admission_reconstruction_logic
```

No `sorry` is allowed:

```bash
rg -n "sorry|admit" crates/defra-agent/proofs/Proofs/Recovery crates/defra-agent/proofs/Proofs/Conformance
```

## Task 8: PR

Open PR:

```text
Add recovery-action enumeration contract in Lean
```

PR body must include:

- `Closes #189`
- `Refs #183`
- `Refs #172`
- contract type name: `RecoverySweep`;
- registered sweep instances and finiteness witnesses;
- deadline audit rows closed or made contractual: stale `InferenceCall` startup sweep and detached subagent bridge terminal lifetime;
- implementation obligations for PR E and the bridge terminal wiring follow-up.
