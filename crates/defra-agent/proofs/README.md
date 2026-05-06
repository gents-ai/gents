# defra-agent Lean Proofs

This directory contains the Lean 4 model for `defra-agent`.

The goal is not to prove math in isolation. The goal is to make the runtime
state machines explicit enough that:

- lifecycle invariants are written down once
- Rust changes can be checked against an executable model
- failure modes are documented as deviations instead of hiding in code paths

The proofs are strongest where the runtime is a state machine:

- request, process, and persistence lifecycle transitions
- scheduler and fleet slot accounting
- session retry/reissue
- runtime reconcile generation publication
- desired-state apply ordering and field ownership
- task, schedule, and event-trigger dispatch
- client turn projection and desktop shell workflow state

They do not prove storage guarantees, network delivery, provider behavior, UI
rendering, or external tool behavior. Those are explicit model boundaries.

## Quick Start

```bash
# Install Lean 4 if needed.
curl https://elan.lean-lang.org/elan-init.sh -sSf | sh -s -- -y

# Build all proofs.
cd crates/defra-agent/proofs
lake build
```

## What Is Proven

The current proof suite covers eight practical areas:

1. Request/process/persistence state transitions
2. Scheduler slot accounting and admission/release
3. Session recovery and retry/reissue semantics
4. Runtime reconcile generation publication and visibility
5. Desired-state apply ordering, reference closure, and apply/runtime field separation
6. Trigger dispatch for manual, schedule, and event-driven tasks
7. Client turn-state derivation from replicated request/response documents
8. Client-shell workflow rules for selection, submission, and transport decoupling

The proof boundary matters:

- Lean proves invariants from the point where runtime state is visible to the
  model.
- Rust conformance tests check that persisted DefraDB-visible states refine
  that model.
- External assumptions such as "DefraDB event arrived" or "provider streamed
  bytes" are not proven here.

## Why This Matters

The proof work is intended to prevent the class of bugs we have already hit in
practice:

- illegal lifecycle transitions
- recovery/claim races
- scheduler slot leaks
- broken retry/reissue semantics
- reconcile publication races
- apply operations clobbering runtime-owned fields
- disabled or serial triggers admitting work incorrectly
- "ready" or "completed" states that were not actually earned
- clients repairing replicated state from the render path

When the model cannot cover something, that gap should be named explicitly and
either tested at the Rust boundary or treated as an external assumption.

## Structure

| File | Contents |
|------|----------|
| `Proofs/Basic.lean` | Shared opaque ids, `Time`, and terminal-state helpers |
| `Proofs/Process.lean` | Process lifecycle model plus executable `Action`, `step?`, and `replay?` |
| `Proofs/Request.lean` | Request lifecycle, admission state, retry/deadline/progress fields, executable `Action`, `step?`, and `replay?` |
| `Proofs/Persistence.lean` | Persistence lifecycle model plus executable `Action`, `step?`, and `replay?` |
| `Proofs/Composed.lean` | Cross-layer composition and guards |
| `Proofs/Scheduling.lean` | Scheduler/backend slot state |
| `Proofs/Fleet.lean` | Fleet-level scheduling and slot accounting |
| `Proofs/SessionRecovery.lean` | Retry/reissue model for session-linked requests |
| `Proofs/RuntimeReconcile.lean` | Generation publication, session binding, and retire/drain invariants |
| `Proofs/ApplyReconcile.lean` | Desired-state apply model, collection ordering, reference closure, and field ownership |
| `Proofs/Triggers.lean` | Trigger dispatch model for manual, schedule, and event triggers |
| `Proofs/Client.lean` | Client turn-state derivation from request/response observations |
| `Proofs/ClientShell.lean` | Multi-session shell workflow above the client turn projection |
| `Proofs/Properties/Safety.lean` | Request/process/persistence safety properties S1-S6 |
| `Proofs/Properties/Liveness.lean` | Request/process liveness properties L1-L3 |
| `Proofs/Properties/SchedulingSafety.lean` | Scheduler/fleet safety properties S7-S9 |
| `Proofs/Properties/SchedulingLiveness.lean` | Scheduler/fleet liveness properties |
| `Proofs/Properties/Decidable.lean` | Finite-state exhaustive checks |
| `Proofs/Conformance/DefraAgent.lean` | Mapping from Lean state to Rust/DefraDB state |
| `Proofs/Conformance/Deviations.lean` | Known gaps between ideal model and implementation |
| `Proofs/Conformance/SchedulerConformance.lean` | Scheduler-specific conformance notes |
| `Proofs/Conformance/Triggers.lean` | Trigger-specific conformance notes |

Related implementation-facing doc:

- `client-state-machine.md`: client turn observation protocol for app implementers

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
- `interrupted`

Operational meaning:

- `pending` has not been claimed by a backend slot yet
- `claimed` owns admission but has not started inference
- `processing` is actively executing
- `inputRequired` models a blocked external-input cycle
- `interrupted` models operator cancellation and releases admission
- terminal states are `completed`, `failed`, `superseded`, `dead`, and `interrupted`

### Layer 3: Persistence Lifecycle

States:

- `uncommitted`
- `committing`
- `committed`
- `lost`

Operational meaning:

- this layer models whether durable state is actually recorded before terminal
  outcomes are considered valid

Total single-execution composed state space: `9 x 5 x 4 = 180` states.

## Plain-English Property Summary

### Request/Process Safety

| ID | Property | Why it matters | Theorem |
|----|----------|----------------|---------|
| S1 | Terminal requests stay terminal | A completed, failed, superseded, dead, or interrupted request cannot silently re-enter processing | `terminal_irreversibility` |
| S3 | `progressSeq` never decreases | Clients can treat progress as monotonic and avoid rewind bugs | `progress_monotonic` |
| S4 | Completion cannot be a hidden deadline violation | A request that reaches `completed` did not get there through deadline expiry | `completed_not_deadline_expired`, `deadline_structural_bound` |
| S5 | Recovery blocks claims | New work is not admitted while recovery is still repairing stuck state | `recovery_blocks_claims` |
| S6 | Completion implies persistence | The model does not allow `completed` without a committed durable state | `persistence_before_completion` |

The historical numbering skips `S2` in the current Lean files. There is no
separate theorem labeled `S2` today; the gap is intentional rather than a
missing build artifact.

### Request/Process Liveness

| ID | Property | Why it matters | Theorem |
|----|----------|----------------|---------|
| L1 | Real phase changes decrease a termination measure | The model rules out endless phase churn that never gets closer to terminal state | `phase_change_decreases_measure` |
| L2 | Claimed work has a constructive path to terminal state | A claimed request is not modeled as stuck forever before inference begins | `claimed_eventually_terminal` |
| L3 | Recovery converges | A finite set of stuck requests can be driven to terminal outcomes in finite steps | `recovery_convergence` |

### Scheduler Safety and Liveness

| ID | Property | Why it matters | Theorem |
|----|----------|----------------|---------|
| S7 | Capacity invariants are preserved | Running-slot counts stay within backend limits | `capacity_invariant_preserved` |
| S8 | Slot accounting is preserved | Scheduler running counts stay aligned with per-request admission state | `slot_accounting_preserved` |
| S9 | Terminal work releases capacity; unavailable backends cannot acquire | Slots are not leaked and unrunnable backends do not admit new work | `terminal_implies_released`, `unavailable_blocks_acquire` |
| L | Capacity-available work can acquire | A waiting request is not artificially blocked when slots exist | `acquire_when_capacity_available` |
| L | Admitted work eventually releases | The model has a constructive path from admitted work to released capacity | `admitted_work_eventually_releases` |

### Session Recovery

`Proofs/SessionRecovery.lean` proves that retry/reissue behavior preserves the
session boundary:

- reissued requests stay in the same session
- behavior identity is preserved
- latest-request semantics are updated coherently
- retry counts advance monotonically and stay bounded

This is the formal version of "retry creates a new request without corrupting
session history."

### Runtime Reconcile

`Proofs/RuntimeReconcile.lean` is the model for live runtime generation swaps.

The key guarantees are:

- generations only move forward
- sessions stay pinned by behavior identity, not by mutable default selection
- publication is separate from resolution
- a generation is not retired while in-flight work still depends on it
- coherent snapshots stay coherent across transitions

This is the formal reason Rust separates resolved snapshots from active
snapshots.

### Apply/Reconcile

`Proofs/ApplyReconcile.lean` models the operator/CLI apply path:

- collection apply order is explicit
- desired-state references must be closed and point to earlier apply ranks
- apply steps write only `DesiredFields`
- runtime-owned `LiveFields` are structurally untouched by apply
- `T-Conv` composes a completed apply pass with runtime reconcile publication

This is the formal contract behind manifest diff/apply and per-agent manifest
roots.

### Triggers

`Proofs/Triggers.lean` models the trigger engine and proves:

- disabled triggers cannot admit work
- serial triggers admit at most one active request
- `latest_only` converges to the latest fire
- materialized requests carry complete trigger lineage

`Proofs/Conformance/Triggers.lean` records the Rust/DefraDB shape used by the
runtime trigger implementation.

### Client Turn Projection

`Proofs/Client.lean` models how clients derive a turn state from replicated
`AgentRequest` and `AgentResponse` snapshots:

- derivation is total for every non-empty attempt chain
- server lifecycle and response advances do not decrease client rank
- terminal client states line up with effectively terminal server observations
- retry replacement derives from the new tip, with retry restart as the one
  allowed rank decrease

The implementation-facing version is `client-state-machine.md`.

### Client Shell Workflow

`Proofs/ClientShell.lean` sits above the per-turn projection and models the
desktop-style multi-session shell:

- snapshots never mutate the user's selected deployment/session
- transport health is a non-mutating input
- local session switching is transport-independent
- follow-up submission safety is independent from transport health
- an awaiting submission only retires after the matching tip is observed

This is the formal guard against render-time "repair" logic corrupting local UI
state.

## Executable Model

The core Lean layers are executable, not just relational:

- `Action`: legal transition vocabulary
- `step?`: executable one-step transition
- `replay?`: bounded trace replay over actions
- soundness/completeness theorems connecting `step?` back to `Transition`

That gives Rust a crisp contract: legal transitions come from Lean, and Rust
must refine them through DB-visible state updates.

## Rust Conformance Strategy

- Lean defines the legal state machines and trace structure.
- Rust tests assert that persisted DefraDB state matches those legal traces.
- Small unit tests still cover isolated pure helpers.
- Binary E2E tests are useful smoke coverage, but they are not the primary
  state-machine proof boundary.

The main conformance files are:

- `crates/defra-agent/tests/state_machine_conformance.rs`
- `crates/defra-agent-protocol/src/client_protocol/tests.rs`
- `crates/defra-agent-cli/src/desired_state/tests.rs`
- `Proofs/Conformance/DefraAgent.lean`
- `Proofs/Conformance/SchedulerConformance.lean`
- `Proofs/Conformance/Triggers.lean`
- `Proofs/Conformance/Deviations.lean`

## Decidable Exhaustive Checks

The finite-state checks currently establish:

- every non-terminal request state has at least one successor
- every non-terminal process state has at least one successor
- every non-terminal persistence state has at least one successor
- admission-state invariants line up with request state
- state counts stay as expected: 9 request, 5 process, 4 persistence, 180 composed

These checks are useful because they catch structural model regressions quickly,
even before theorem-level reasoning matters.

## Current Deviations

Known gaps are documented in `Proofs/Conformance/Deviations.lean`.

Examples:

- no explicit `recovering` process state in Rust startup
- no explicit `inputRequired` feature in the Rust runtime yet
- no explicit persisted `dead` state
- no first-class persisted persistence-lifecycle tracking
- deadline accounting does not yet bound retries
- fleet scheduler persistence remains partly observational

That file should stay honest. If the implementation diverges from the model,
the deviation should be named there instead of silently tolerated.

## Known Limitations

### Apply Atomicity

`defra-agent-cli config apply` today is best-effort: if a write fails partway
through the ordered apply sequence, the database is left in a partially updated
state and there is no rollback. The `T-Conv` theorem in
`Proofs/ApplyReconcile.lean` assumes apply runs to completion. It does not cover
crash-mid-apply.

Operators must retry `apply` after a failure and should treat a partial-apply
state as manually inconsistent until resolved.

### Interrupted Inference Calls

`Proofs/Composed.lean` contains a `True` placeholder documenting the intended
cross-layer property that an interrupted request eventually cancels linked
inference calls. Rust has cancellation paths for pre-stream and mid-stream
interrupts, but the Lean proof needs a first-class `InferenceCall` state machine
before that property can be closed formally.

## What Is Not Proven

These proofs do not establish:

- DefraDB read-your-writes semantics
- DefraDB CRDT merge or event-delivery guarantees
- network reliability
- provider/model correctness
- MCP or external tool availability
- desktop rendering correctness
- OS sandbox behavior

Those are handled through explicit assumptions, Rust integration tests,
operational diagnostics, or platform-specific tests.
