# defra-agent Lean Proofs

This directory contains the Lean 4 model for `defra-agent`.

The goal is not to prove math in isolation. The goal is to make the runtime
state machines explicit enough that:

- lifecycle invariants are written down once
- Rust changes can be checked against an executable model
- unresolved Rust/spec mismatches are isolated as deviations, while intentional
  product boundaries are documented explicitly

The proofs are strongest where the runtime is a state machine:

- request, process, and persistence lifecycle transitions
- inference-call lifecycle, cancellation transitions, and slot reconstruction
- scheduler and fleet slot accounting from persisted call rows
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

# Print the Rust conformance contract JSON used by Rust tests.
lake build Proofs.Conformance.Contracts
lake env lean --run Proofs/Conformance/Contracts.lean
```

## What Is Proven

The current proof suite covers nine practical areas:

1. Request/process/persistence state transitions
2. Inference-call lifecycle, request linkage, and cancellation terminality
3. Scheduler slot accounting and admission/release
4. Session recovery and retry/reissue semantics
5. Runtime reconcile generation publication and visibility
6. Desired-state apply ordering, reference closure, and apply/runtime field separation
7. Trigger dispatch for manual, schedule, and event-driven tasks
8. Client turn-state derivation from replicated request/response documents
9. Client-shell workflow rules for selection, submission, and transport decoupling

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
- disabled or serial triggers accepting work incorrectly
- "ready" or "completed" states that were not actually earned
- clients repairing replicated state from the render path

When the model cannot cover something, that boundary should be named explicitly
and either tested at the Rust boundary or treated as an external assumption.

## Structure

| File | Contents |
|------|----------|
| `Proofs/Basic.lean` | Shared opaque ids, `Time`, and terminal-state helpers |
| `Proofs/Process.lean` | Process lifecycle model plus executable `Action`, `step?`, and `replay?` |
| `Proofs/Request.lean` | Barrel for request state, transitions, executable semantics, and local properties |
| `Proofs/InferenceCall.lean` | Barrel for inference-call state, transitions, slot accounting, and cancellation properties |
| `Proofs/Persistence.lean` | Persistence lifecycle model plus executable `Action`, `step?`, and `replay?` |
| `Proofs/Composed.lean` | Cross-layer composition and guards |
| `Proofs/Scheduling.lean` | Scheduler/backend slot state |
| `Proofs/Fleet.lean` | Fleet-level scheduling and slot accounting |
| `Proofs/SessionRecovery.lean` | Retry/reissue model for session-linked requests |
| `Proofs/RuntimeReconcile.lean` | Barrel for runtime reconcile state, relational transitions, and executable semantics |
| `Proofs/ApplyReconcile.lean` | Barrel for desired-state apply, runtime bridge, and convergence |
| `Proofs/Triggers.lean` | Barrel for trigger types, dispatch, reachability, serial, latest-only, and lineage proofs |
| `Proofs/Client.lean` | Barrel for client turn-state derivation and client theorems |
| `Proofs/ClientShell.lean` | Barrel for multi-session shell workflow modules |
| `Proofs/Properties/Safety.lean` | Request/process/persistence safety properties S1-S6 |
| `Proofs/Properties/Liveness.lean` | Request/process liveness properties L1-L3 |
| `Proofs/Properties/SchedulingSafety.lean` | Scheduler/fleet safety properties S7-S9 |
| `Proofs/Properties/SchedulingLiveness.lean` | Scheduler/fleet liveness properties |
| `Proofs/Properties/Decidable.lean` | Finite-state exhaustive checks |
| `Proofs/Conformance/DefraAgent.lean` | Mapping from Lean state to Rust/DefraDB state |
| `Proofs/Conformance/Boundaries.lean` | Intentional product policies and external assumptions at the Rust/Lean boundary |
| `Proofs/Conformance/Deviations.lean` | Active unresolved Rust/spec mismatches; currently empty |
| `Proofs/Conformance/SchedulerConformance.lean` | Scheduler-specific conformance notes |
| `Proofs/Conformance/Contracts.lean` | Test-time JSON extraction surface for Rust vocabularies, finite state counts, transition tables, and legal/illegal transition pairs |
| `Proofs/Conformance/Triggers.lean` | Barrel for trigger lifecycle/materialization conformance |

Semantic submodules:

| Barrel | Submodules |
|--------|------------|
| `Proofs.Request` | `State`, `Transition`, `Executable`, `Properties` |
| `Proofs.InferenceCall` | `State`, `Transition`, `Executable`, `Properties`, `SlotAccounting` |
| `Proofs.RuntimeReconcile` | `State`, `Transition`, `Executable` |
| `Proofs.ApplyReconcile` | `Collections`, `Manifest`, `Diff`, `Apply`, `ApplyProperties`, `RuntimeBridge`, `Convergence` |
| `Proofs.Triggers` | `Types`, `Dispatch`, `Reachability`, `SerialSupport`, `Serial`, `LatestOnly`, `Lineage` |
| `Proofs.Triggers.SerialSupport` | `Counting`, `Preservation` |
| `Proofs.Client` | `Types`, `Lifecycle`, `Terminal`, `Replacement` |
| `Proofs.ClientShell` | `Types`, `Submission`, `Transition`, `Projection`, `Theorems` |
| `Proofs.Conformance.Triggers` | `Lifecycle`, `Materialization`, `Trace` |

The top-level barrel imports remain the stable entry points for downstream code.

Related implementation-facing doc:

- `client-state-machine.md`: client turn observation protocol for app implementers

## Rust Conformance Extraction

Rust conformance tests do not hand-maintain separate Lean parity tables for the
core executable machines. The test helper in `src/lean_vocab_test.rs` runs:

```bash
cd crates/defra-agent/proofs
lake build Proofs.Conformance.Contracts
lake env lean --run Proofs/Conformance/Contracts.lean
```

The emitted JSON is printed between `---BEGIN DEFRA LEAN CONTRACT JSON---` and
`---END DEFRA LEAN CONTRACT JSON---` sentinel lines so Rust can reject unrelated
stdout. It is generated from Lean constructors, `toDefraDB` functions, terminal
predicates, executable `step?` functions, and finite witness contexts. It
currently covers:

- `Request`
- `Process`
- `Persistence.failClosed`
- `Persistence.failOpen`
- `SessionRecovery`
- `InferenceCall`

The current `SessionRecovery` contract is intentionally narrow: it covers the
executable failed-latest-request reissue witness (`failed -> pending`) rather
than the whole request lifecycle vocabulary. Widen this contract when the
session-recovery executable model grows enough finite witnesses to represent the
larger space directly.

When a Lean vocabulary, terminal partition, action, or legal transition changes,
the generated JSON changes on the next Rust test run. The Rust tests then fail
unless the runtime behavior or the documented product-boundary assertions are
updated to match.

`RuntimeReconcile` is intentionally exposed only as a follow-up hook in the JSON
so this extraction stays scoped to the initial executable domains above. Add it
to `Proofs/Conformance/Contracts.lean` as another `StateMachineContract` when
the runtime-reconcile contract is ready to join the Rust conformance gate.

## Core Model

### Layer 1: Process Lifecycle

States:

- `uninitialized`
- `recovering`
- `ready`
- `shuttingDown`
- `shutdown`

Operational meaning:

- `recovering` means the runtime is not yet allowed to accept fresh work
- `ready` means the runtime passed startup validation and can accept work
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
- `inputRequired` is reserved for a blocked external-input cycle; current Rust
  runtime code does not emit it because autonomous tool calls run inline
- `dead` is persisted only for stale pre-claim TTL expiry; post-claim provider
  failure, retry exhaustion, tool failure, and deadline expiry are terminal
  `failed`
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
  outcomes are considered valid; Rust currently treats this as an operational
  storage boundary rather than a persisted per-token state document

### Layer 4: Inference Call Lifecycle

States:

- `queued`
- `running`
- `cancelled`
- `completed`
- `failed`

Operational meaning:

- `queued` is persisted before a backend semaphore permit is available
- `queued` does not hold a backend slot
- `running` owns exactly one backend permit and is waiting for or consuming provider work
- `cancelled` records terminal cancellation without provider completion,
  including request interrupts and backend lifecycle cancellation
- `cancelled`, `completed`, and `failed` release backend capacity and do not
  contribute to reconstructed slot counts
- terminal call states are `cancelled`, `completed`, and `failed`

The core request/process/persistence state space remains `9 x 5 x 4 = 180`
states. The call layer adds a separate 5-state persisted lifecycle linked to a
request by `request_id` and bound to a backend by `backend_id`.

## Plain-English Property Summary

### Request/Process Safety

| ID | Property | Why it matters | Theorem |
|----|----------|----------------|---------|
| S1 | Terminal requests stay terminal | A completed, failed, superseded, dead, or interrupted request cannot silently re-enter processing | `terminal_irreversibility` |
| S3 | `progressSeq` never decreases | Clients can treat progress as monotonic and avoid rewind bugs | `progress_monotonic` |
| S4 | Completion cannot be a hidden deadline violation | A request that reaches `completed` did not get there through deadline expiry | `completed_not_deadline_expired`, `deadline_structural_bound` |
| S5 | Recovery blocks claims | New work is not accepted while recovery is still repairing stuck state | `recovery_blocks_claims` |
| S6 | Completion implies persistence | The model does not allow `completed` without a committed durable state | `persistence_before_completion` |

The historical numbering skips `S2` in the current Lean files. There is no
separate theorem labeled `S2` today; the gap is intentional rather than a
missing build artifact.

Request-local field monotonicity uses local labels instead of scheduler safety
numbers: `R-Int` for `interrupt_monotonicity` and `R-TTL` for
`valid_until_monotonicity`.

Deadline and TTL conformance is now explicit on both sides: the request model
requires `ttlOpen` before claim (`claim_requires_ttl_open`,
`claim_with_ttl_bounds_time`), and session retry/reissue requires the source
request deadline to remain open (`reissue_source_deadline_open`,
`reissue_latest_deadline_open`). Rust mirrors this by converting stale
pre-claim requests to `dead/Stale` and by bounding inference retry sleeps and
stream waits by the claimed deadline. Once work is claimed, retry exhaustion
and deadline expiry remain ordinary terminal `failed` outcomes rather than
being reclassified as `dead`.

### Request/Process Liveness

| ID | Property | Why it matters | Theorem |
|----|----------|----------------|---------|
| L1 | Real current-product phase changes decrease a termination measure | The model rules out endless phase churn that never gets closer to terminal state | `phase_change_decreases_measure` |
| L2 | Claimed work has a constructive path to terminal state | A claimed request is not modeled as stuck forever before inference begins | `claimed_eventually_terminal` |
| L3 | Recovery converges | A finite set of stuck requests can be driven to terminal outcomes in finite steps | `recovery_convergence` |

### Scheduler Safety and Liveness

| ID | Property | Why it matters | Theorem |
|----|----------|----------------|---------|
| S7 | Capacity invariants are preserved | Running-slot counts stay within backend limits | `capacity_invariant_preserved`, `reconstructedSlotCount_bounded_by_max_concurrent` |
| S8 | Slot accounting is preserved | Scheduler running counts stay aligned with per-request admission state and persisted running call rows | `slot_accounting_preserved`, `scheduler_running_reconstructed_from_inference_calls` |
| S9 | Terminal work releases capacity; unavailable backends cannot acquire | Slots are not leaked and unrunnable backends do not accept new work | `terminal_implies_released`, `permitDrop_terminalization_not_counted`, `unavailable_blocks_acquire` |
| L | Capacity-available work can acquire | A waiting request is not artificially blocked when slots exist | `acquire_when_capacity_available` |
| L | Accepted work eventually releases | The model has a constructive path from accepted work to released capacity | `accepted_work_eventually_releases` |

The scheduling-liveness theorem was intentionally renamed to
`accepted_work_eventually_releases`; the old name used the previous acceptance
vocabulary and is not kept as an alias so the proof-tree hygiene search stays
unambiguous.

`Proofs/InferenceCall/SlotAccounting.lean` is the production-facing slot model:
queued rows contribute zero slots, running rows contribute one slot on their
`backend_id`, terminal rows contribute zero slots, permit-drop terminalization
cannot leave a row counted, and live linked queued/running calls have a model
path to a non-slot-holding terminal state.

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
It is executable in Lean through `Proofs/RuntimeReconcile/Executable.lean`,
which defines `Action`, `step?`, `replay?`, `step_sound`,
`transition_complete`, `replay_sound`, and `trace_complete`.
The same module exposes executable helper corollaries for generation
monotonicity, coherent preservation, publish well-formedness, request binding,
router observed-generation readiness/liveness, and in-flight retirement safety.

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
- `t_conv_runnable` is the apply-sensitive result: after a well-formed apply,
  every manifest behavior id is runnable
- `t_conv` and `t_conv_published` are coverage corollaries over the resolved and
  published snapshot carrier sets

This is the formal contract behind manifest diff/apply and per-agent manifest
roots.

### Triggers

`Proofs/Triggers.lean` models the trigger engine and proves:

- disabled triggers cannot accept work
- serial triggers accept at most one active request
- `T3_latest_only_convergence` proves latest-only supersession directly from
  `dispatchStep`; `latestOnlyFireTransition_convergence` is only the abstract
  relation unwrapping lemma
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

The core Lean layers are executable, not just relational. This includes
request, process, persistence, session recovery, fleet, and runtime reconcile:

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
- `crates/defra-agent/src/admission/tests.rs`
- `crates/defra-agent-protocol/src/client_protocol/tests.rs`
- `crates/defra-agent-cli/src/desired_state/tests.rs`
- `Proofs/Conformance/DefraAgent.lean`
- `Proofs/Conformance/Boundaries.lean`
- `Proofs/Conformance/SchedulerConformance.lean`
- `Proofs/Conformance/Triggers.lean`
- `Proofs/Conformance/Deviations.lean`

The Rust/Lean vocabulary checks compare Rust-visible strings against Lean
`toDefraDB` definitions for request lifecycle states, execution origins,
process lifecycle states, runtime reconcile phases, trigger kinds,
inference-call states, and the closed set of system-generated inference-call
terminal reasons.

Admission tests also reconstruct held backend slots from persisted
`InferenceCall` rows during contention, queueing, completion, failure,
cancellation, permit-drop, backend-gone, and queue-full paths. These tests
assert that only `call_state = "running"` holds capacity and that the
reconstructed count never exceeds backend `max_concurrent`.

## Decidable Exhaustive Checks

The finite-state checks currently establish:

- every active current-product non-terminal request state has at least one
  successor; reserved `inputRequired` remains vocabulary-only
- every non-terminal process state has at least one successor
- every non-terminal persistence state has at least one successor
- every non-terminal inference-call state has at least one successor
- admission-state invariants line up with request state
- state counts stay as expected: 9 request, 5 process, 4 persistence, 5 call,
  180 core composed

These checks are useful because they catch structural model regressions quickly,
even before theorem-level reasoning matters.

## Boundaries And Deviations

`Proofs/Conformance/Boundaries.lean` records intentional product policies,
reserved vocabulary, closed historical items, and external assumptions. These
are not deviations.

Current boundaries:

- `inputRequired` is reserved persisted/client vocabulary. Rust parses it and
  treats it as non-terminal if observed, but the runtime does not emit it today.
- `dead` is current product behavior only for stale pre-claim TTL expiry.
  Post-claim provider failure, retry exhaustion, tool failure, and deadline
  expiry remain terminal `failed`.
- Tool failures are permanent until tools expose retry-safe health,
  idempotency, and side-effect metadata.
- Fleet aggregate slot state is reconstructed from `InferenceCall` rows rather
  than persisted as a single `FleetState` document. Only rows with
  `call_state = "running"` hold slots; queued and terminal rows do not.
- `PersistenceState` is a proof abstraction over durable writes; DefraDB
  successful-mutation durability is an external storage assumption.

`Proofs/Conformance/Deviations.lean` is now reserved only for real unresolved
Rust/spec mismatches. There are currently no known active spec deviations.

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

`Proofs/InferenceCall.lean` models queued, running, cancelled, completed, and
failed call states. `Proofs/Composed.lean` proves
`ComposedState.interrupted_request_cancels_live_linked_call`: when a request is
interrupted, any queued or running call linked by `request_id` has a valid model
path to `cancelled`.

The broader `cancelled` call state is not interrupt-only. Rust also uses it for
backend-gone and controller-drain cases; those are modeled as ordinary terminal
call transitions rather than request-interrupt composition.

Rust covers this bridge at the admission/permit level and with a full
`BehaviorDaemon` mock-stream fixture: mid-stream interruption preserves partial
response content, persists the linked inference call as `cancelled`, and leaves
unrelated concurrent calls live.

System-generated `InferenceCall.failure_reason` values used by admission and
interrupt/drop paths are mirrored by `InferenceCallTerminalReason`; provider
error strings remain open and are not treated as a closed Lean vocabulary.

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
