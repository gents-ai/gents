# Agent State Machine Formal Verification

This directory contains the Lean 4 model for `defra-agent`.

The goal is not “prove some math in isolation.” The goal is to make the runtime
state machine explicit enough that:

- lifecycle invariants are written down once
- Rust changes can be checked against an executable model
- failure modes are documented as deviations instead of hiding in code paths

The proofs are strongest where the runtime is a state machine:

- request lifecycle
- process lifecycle
- scheduler slot accounting
- session retry/reissue
- runtime reconcile generation publication

They do not prove storage guarantees, network delivery, or provider behavior.
Those are explicit model boundaries.

## Quick Start

```bash
# Install Lean 4 (if not already installed)
curl https://elan.lean-lang.org/elan-init.sh -sSf | sh -s -- -y

# Build all proofs
cd proofs && lake build
```

## What Is Proven

The current proof suite covers four practical areas:

1. Request/process/persistence state transitions
2. Scheduler slot accounting and admission/release
3. Session recovery and retry/reissue semantics
4. Runtime reconcile generation publication and visibility

The proof boundary matters:

- Lean proves invariants from the point where runtime state is visible to the
  model.
- Rust conformance tests check that the persisted DefraDB-visible states refine
  that model.
- External assumptions such as “DefraDB event arrived” or “provider streamed
  bytes” are not proven here.

## Why This Matters

The proof work is intended to prevent the class of bugs we have already hit in
practice:

- illegal lifecycle transitions
- recovery/claim races
- scheduler slot leaks
- broken retry/reissue semantics
- reconcile publication races
- “ready” or “completed” states that were not actually earned

When the model cannot cover something, that gap should be named explicitly and
either tested at the Rust boundary or treated as an external assumption.

## Structure

| File | Contents |
|------|----------|
| `Proofs/Basic.lean` | Shared types: `Time`, `SessionId`, `RequestId`, `BehaviorId`, and terminal-state helpers |
| `Proofs/Process.lean` | Process lifecycle model plus executable `Action`, `step?`, and `replay?` |
| `Proofs/Scheduling.lean` | Scheduler/backend slot state |
| `Proofs/Request.lean` | Request lifecycle model plus executable `Action`, `step?`, and `replay?` |
| `Proofs/Persistence.lean` | Persistence lifecycle model plus executable `Action`, `step?`, and `replay?` |
| `Proofs/Composed.lean` | Cross-layer composition and guards |
| `Proofs/Fleet.lean` | Fleet-level scheduling and slot accounting |
| `Proofs/SessionRecovery.lean` | Retry/reissue model for session-linked requests |
| `Proofs/RuntimeReconcile.lean` | Generation publication, session binding, and retire/drain invariants |
| `Proofs/Properties/Safety.lean` | Request/process/persistence safety properties S1-S6 |
| `Proofs/Properties/Liveness.lean` | Request/process liveness properties L1-L3 |
| `Proofs/Properties/SchedulingSafety.lean` | Scheduler/fleet safety properties S7-S9 |
| `Proofs/Properties/SchedulingLiveness.lean` | Scheduler/fleet liveness properties |
| `Proofs/Properties/Decidable.lean` | Finite-state exhaustive checks |
| `Proofs/Conformance/DefraAgent.lean` | Mapping from Lean state to Rust/DefraDB state |
| `Proofs/Conformance/Deviations.lean` | Known gaps between ideal model and implementation |
| `Proofs/Conformance/SchedulerConformance.lean` | Scheduler-specific conformance notes |

## Core Model

### Layer 1: Process Lifecycle

States:

- `uninitialized`
- `recovering`
- `ready`
- `shuttingDown`
- `shutdown`

Operational meaning:

- `recovering` means the runtime is not yet allowed to admit fresh work
- `ready` means the runtime passed startup validation and can admit work
- `shuttingDown` means no new work should enter and existing work is draining

### Layer 2: Request Lifecycle

States:

- `pending`
- `claimed`
- `processing`
- `inputRequired`
- `completed`
- `failed`
- `superseded`
- `dead`

Operational meaning:

- `pending` has not been claimed by a backend slot yet
- `claimed` owns admission but has not started inference
- `processing` is actively executing
- `inputRequired` models a blocked external-input cycle
- terminal states are `completed`, `failed`, `superseded`, `dead`

### Layer 3: Persistence Lifecycle

States:

- `uncommitted`
- `committing`
- `committed`
- `lost`

Operational meaning:

- this layer models whether durable state is actually recorded before terminal
  outcomes are considered valid

Total composed state space: `8 x 5 x 4 = 160` states.

## Plain-English Property Summary

### Request/Process Safety

| ID | Property | Why it matters | Theorem |
|----|----------|----------------|---------|
| S1 | Terminal requests stay terminal | A completed or failed request cannot silently re-enter processing | `terminal_irreversibility` |
| S3 | `progressSeq` never decreases | Clients can treat progress as monotonic and avoid rewind bugs | `progress_monotonic` |
| S4 | Completion cannot be a hidden deadline violation | A request that reaches `completed` did not get there through deadline expiry | `completed_not_deadline_expired`, `deadline_structural_bound` |
| S5 | Recovery blocks claims | New work is not admitted while recovery is still repairing stuck state | `recovery_blocks_claims` |
| S6 | Completion implies persistence | The model does not allow “completed” without a committed durable state | `persistence_before_completion` |

The historical numbering skips `S2` in the current Lean files. There is no
separate theorem labeled `S2` today; the gap is intentional rather than a
missing build artifact.

### Request/Process Liveness

| ID | Property | Why it matters | Theorem |
|----|----------|----------------|---------|
| L1 | Real phase changes decrease a termination measure | The model rules out endless phase churn that never gets closer to terminal state | `phase_change_decreases_measure` |
| L2 | Claimed work has a constructive path to terminal state | A claimed request is not modeled as “stuck forever before inference begins” | `claimed_eventually_terminal` |
| L3 | Recovery converges | A finite set of stuck requests can be driven to terminal outcomes in finite steps | `recovery_convergence` |

### Scheduler Safety

| ID | Property | Why it matters | Theorem |
|----|----------|----------------|---------|
| S7 | Capacity invariants are preserved | Running-slot counts stay within backend limits | `capacity_invariant_preserved` |
| S8 | Slot accounting is preserved | The scheduler’s running counts stay aligned with per-request admission state | `slot_accounting_preserved` |
| S9 | Terminal work releases capacity; unavailable backends cannot acquire | Slots are not leaked and unrunnable backends do not admit new work | `terminal_implies_released`, `unavailable_blocks_acquire` |

### Scheduler Liveness

| Property | Why it matters | Theorem |
|----------|----------------|---------|
| Capacity-available work can acquire | A waiting request is not artificially blocked when slots exist | `acquire_when_capacity_available` |
| Admitted work eventually releases | The model has a constructive path from admitted work to released capacity | `admitted_work_eventually_releases` |

### Session Recovery

`Proofs/SessionRecovery.lean` makes retry/reissue behavior explicit.

Operationally, it proves things like:

- a reissued request stays in the same session
- behavior identity is preserved
- latest-request semantics are updated coherently
- retry counts advance monotonically and stay bounded

This is the formal version of “retry creates a new request without corrupting
session history.”

### Runtime Reconcile

`Proofs/RuntimeReconcile.lean` is the model for live runtime generation swaps.

The key guarantees are:

- generations only move forward
- sessions stay pinned by behavior identity, not by mutable default selection
- publication is separate from resolution
- a generation is not retired while in-flight work still depends on it
- coherent snapshots stay coherent across transitions

This is the formal reason we separate resolved snapshots from active snapshots
in Rust.

## Executable Model

The Lean layers are executable, not just relational:

- `Action`: legal transition vocabulary
- `step?`: executable one-step transition
- `replay?`: bounded trace replay over actions
- soundness/completeness theorems connecting `step?` back to `Transition`

That gives Rust a crisp contract: legal transitions come from Lean, and Rust
must refine them through DB-visible state updates.

## Rust Conformance Strategy

- Lean defines the legal state machine and trace structure.
- Rust tests assert that persisted DefraDB state matches those legal traces.
- Small unit tests still cover isolated pure helpers.
- Binary E2E tests are useful smoke coverage, but they are not the primary
  state-machine proof boundary.

The main conformance files are:

- `crates/defra-agent/tests/state_machine_conformance.rs`
- `Proofs/Conformance/DefraAgent.lean`
- `Proofs/Conformance/SchedulerConformance.lean`
- `Proofs/Conformance/Deviations.lean`

## Decidable Exhaustive Checks

The finite-state checks currently establish:

- every non-terminal request state has at least one successor
- every non-terminal process state has at least one successor
- every non-terminal persistence state has at least one successor
- admission-state invariants line up with request state
- state counts stay as expected: 8 request, 5 process, 4 persistence, 160 composed

These checks are useful because they catch structural model regressions quickly,
even before theorem-level reasoning matters.

## Current Deviations

Known gaps are documented in `Proofs/Conformance/Deviations.lean`.

Examples:

- no explicit `inputRequired` feature in the Rust runtime yet
- no explicit persisted `dead` state
- no first-class persisted persistence-lifecycle tracking
- some observability gaps remain at fleet level

That file should stay honest. If the implementation diverges from the model,
the deviation should be named there instead of silently tolerated.

## Known Limitations

### Apply atomicity

`defra-agent-cli config apply` today is best-effort: if a write fails
partway through the ordered apply sequence, the database is left in a
partially-updated state and there is no rollback. The `T-Conv` theorem in
`Proofs/ApplyReconcile.lean` assumes apply runs to completion — it does
not cover crash-mid-apply. Operators must retry `apply` after a failure
and should treat a partial-apply state as manually inconsistent until
resolved.

Tracking issue: I-2 (make apply transactional).

## What Is Not Proven

These proofs do not establish:

- DefraDB read-your-writes semantics
- event-delivery guarantees from DefraDB watchers
- network reliability
- provider/model correctness
- MCP or external tool availability

Those are handled through explicit assumptions, Rust integration tests, or
operational diagnostics.
