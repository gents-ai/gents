# Agent State Machine Formal Verification

Lean 4 formal verification of an ideal agent state machine, with
conformance mapping to defra-agent.

## Quick Start

```bash
# Install Lean 4 (if not already installed)
curl https://elan.lean-lang.org/elan-init.sh -sSf | sh -s -- -y

# Build all proofs
cd proofs && lake build
```

## Structure

| File | Contents |
|------|----------|
| `Proofs/Basic.lean` | Shared types: Time, SessionId, RequestId, BehaviorId, HasTerminal typeclass |
| `Proofs/Process.lean` | Layer 1 process lifecycle plus executable `Action` / `step?` / `replay?` |
| `Proofs/Scheduling.lean` | Backend binding, admission, and scheduler state |
| `Proofs/Request.lean` | Layer 2 request lifecycle plus executable `Action` / `step?` / `replay?` |
| `Proofs/Persistence.lean` | Layer 3 persistence lifecycle plus executable `Action` / `step?` / `replay?` |
| `Proofs/Composed.lean` | Cross-layer composition with guards |
| `Proofs/Fleet.lean` | Fleet-level slot accounting plus executable scheduler actions |
| `Proofs/SessionRecovery.lean` | Session-level retry/reissue plus executable `Action` / `step?` / `replay?` |
| `Proofs/RuntimeReconcile.lean` | Runtime generation publication, behavior-pinned sessions, and reconcile preservation invariants |
| `Proofs/Properties/Safety.lean` | S1-S6 safety proofs |
| `Proofs/Properties/Liveness.lean` | L1, L3 bounded termination |
| `Proofs/Properties/Decidable.lean` | Finite-state exhaustive checks |
| `Proofs/Conformance/DefraAgent.lean` | State mapping to defra-agent |
| `Proofs/Conformance/Deviations.lean` | Current documented deviations |

## Three-Layer Model

**Layer 1 — Process Lifecycle** (5 states):
`uninitialized -> recovering -> ready -> shuttingDown -> shutdown`

**Layer 2 — Request Lifecycle** (8 states):
`pending -> claimed -> processing -> {completed, failed, superseded, dead}`
Plus `inputRequired` (blocked on external input) cycling with `processing`.

**Layer 3 — Persistence Lifecycle** (4 states):
`uncommitted -> committing -> {committed, lost}`
Parameterized by FailurePolicy (failOpen / failClosed).

Total composed state space: 8 x 5 x 4 = **160 states**.

## Properties Verified

### Safety (nothing bad happens)

| ID | Property | Theorem |
|----|----------|---------|
| S1 | Terminal state irreversibility | `terminal_irreversibility` |
| S3 | Monotonic progress (progressSeq) | `progress_monotonic` |
| S4 | Deadline structural bounding | `deadline_structural_bound`, `completed_not_deadline_expired` |
| S5 | Recovery exclusivity (recovering blocks claims) | `recovery_blocks_claims` |
| S6 | Persistence before completion (completed => committed) | `persistence_before_completion` |

### Liveness (something good eventually happens)

| ID | Property | Theorem |
|----|----------|---------|
| L1 | Bounded termination (phase changes decrease measure) | `phase_change_decreases_measure` |
| L3 | Recovery convergence (n stuck requests in n steps) | `recovery_convergence` |

### Decidable Checks

- Every non-terminal state has at least one distinct successor (no deadlocks)
- State counts verified: 8 request, 5 process, 4 persistence, 160 composed

## Executable Model

The Lean layers are no longer just relational proofs. Each state machine layer
now exposes:

- `Action`: the legal transition vocabulary for that layer
- `step?`: an executable one-step transition function
- `replay?`: bounded trace replay over a list of actions
- soundness/completeness theorems linking `step?` back to the relational
  `Transition`

This is the contract used by the Rust conformance suite: Lean is the source of
legal transitions, while Rust proves that the runtime refines those transitions
through DB-visible state changes.

## Testing Strategy

- Lean proofs define the legal transition system and trace structure.
- Rust integration tests under
  `crates/defra-agent/tests/state_machine_conformance.rs`
  drive the public lifecycle API and assert persisted DefraDB snapshots.
- Small helper-unit tests remain valuable for isolated pure logic.
- End-to-end inference tests should stay small and high-signal; they are smoke
  coverage, not the primary state-machine conformance mechanism.

## Findings

Current bugs found in defra-agent via conformance analysis:

1. **Deadline/retry bounding (S4):** The retry loop in `agent.rs` does not
   check `currentTime < deadline` before sleeping for backoff. Retries are
   bounded by `max_retries` count only, not by wall-clock deadline.

2. **Recovery/claim race (S5):** `recover_all()` runs inline in the daemon
   loop with no explicit `recovering` state. The watcher could deliver a
   request while recovery is still processing stuck requests.

Additional deviations documented in `Proofs/Conformance/Deviations.lean`:
- No `inputRequired` state (missing feature)
- No explicit persisted `dead` state (observability gap)
- No explicit persistence tracking (missing feature)
- Tool failures always permanent (design choice)
- Fleet scheduler counts are not persisted to DefraDB (observability gap)

## Design Spec

See `docs/superpowers/specs/2026-04-07-lean-formal-verification-design.md`
for the full design rationale and prior art survey.
