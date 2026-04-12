# Client Turn Observation Protocol

Date: 2026-04-11

## Problem

The runtime has a formal model (Lean proofs) and a document model (DefraDB schemas), but client interpretation of agent turns is ad hoc. Amy (iOS) has a working `RequestLifecycleState.deriveFrom(request, response)` function, but it was built pragmatically — using `status` strings instead of the proven `lifecycle_state` enum, with no formal relationship to the server proofs, and with client-specific timing logic (`stalled` state) mixed into the derivation.

A second client (CLI, web, or desktop) implementing the same logic would have to reverse-engineer Amy's Swift code and hope the derivation rules match. Differences in how two clients interpret the same documents would be invisible until a user notices inconsistent behavior.

## What This Specifies

A formal Lean model for how any client derives a deterministic view of a single agent turn from observed documents, with proven properties (monotonicity, terminal coherence, convergence) that tie the client projection back to the existing server proofs.

Plus an informal protocol reference covering the parallel observation surfaces (tool calls, messages, conversation) and the subscription model that clients need to implement.

## What This Does Not Specify

- **Client transport/connection state** (disconnected, connecting, syncing). Each client owns its transport layer. Not a universal state machine.
- **P2P DAG/CRDT convergence**. That is defradb.rs scope. This spec explicitly assumes DefraDB delivers merged snapshots, not raw DAG commits.
- **`AgentRuntime` observation**. Clients read the document directly; no state machine needed.
- **Multi-turn session lifecycle**. Deferrable. One turn is the hard problem.
- **Reference implementation crate**. Validate the spec by retrofitting Amy first, not by building a new crate up front.

## Design Decisions

- **Lean-first.** The client turn projection is a state machine. State machines in this repo start in Lean. The protocol doc explains the Lean model; it is not the source of truth.
- **Pure projection, no clock.** The server liveness proofs (L1 bounded termination, L3 recovery convergence) guarantee every request reaches a terminal state. If a client perceives a "stall," that is a transport/sync problem, not a turn-state problem. Therefore `derive` is a pure function of document observations — no `currentTime`, no client-local timing. This makes the formal contract stronger: every client state corresponds to a server-committed fact.
- **Turn identity = retry chain root.** `TurnId` is `retry_root_request_id` (or `request_id` if no retry parent). All retry attempts for a user's original message collapse into one logical turn. Supersession is a cross-turn event.
- **Response can be more current than request (non-terminal only).** Under P2P replication lag, the client may observe `AgentResponse.status = complete` before `AgentRequest.lifecycle_state` transitions to `completed`. The derivation trusts the response **only when the request's lifecycle is non-terminal**. Once the request lifecycle is terminal (`completed`, `failed`, `superseded`, or `dead`), it always wins over any response — this preserves monotonicity (T2) against stale streaming responses arriving alongside a terminal request. This matches Amy's working behavior for the common case.
- **5 client states, no ideal-only states in v1.** `awaitingInput` and `dead` are annotated in the derivation as mapping to existing states (`waitingForClaim` and `failed` respectively). When the runtime implements those (Deviations #2 and #3), clients can introduce additional states without breaking the existing projection.
- **Trivial merge, nontrivial derive.** The merge function (accepting incoming document snapshots into the observation store) is trivial under the DefraDB merged-snapshot assumption. The formal content lives in the derivation and its 5 theorems. This is honest — we explicitly defer P2P merge proofs to defradb.rs.
- **Stalled is a UI affordance, not a turn state.** If a client wants to show "checking server..." after N seconds of no updates on a non-terminal turn, that is a per-client UX decision layered on top of the projection. Each client owns its own staleness heuristic.

## Lean Model: `Proofs/Client.lean`

Imports `Proofs.Request` to reuse `RequestState` and `AdmissionState`.

### Client Turn State

```lean
inductive ClientTurnState where
  | waitingForClaim   -- request observed, no response content yet
  | streaming         -- response observed with partial content
  | completed         -- response terminal success
  | failed            -- latest attempt terminal failure, no successor
  | superseded        -- a later request replaced this turn
```

### Client DAG Ordering

```lean
def ClientTurnState.rank : ClientTurnState -> Nat
  | .waitingForClaim => 0
  | .streaming       => 1
  | .completed       => 2
  | .failed          => 2
  | .superseded      => 2
```

Terminal states share rank 2. They are incomparable (you don't go from `completed` to `failed`). Monotonicity means rank never decreases.

### Response Status

```lean
inductive ResponseStatus where
  | streaming | complete | error
```

### Observation Types

```lean
structure RequestSnapshot where
  lifecycleState : RequestState
  supersededBy : Option String

structure ResponseSnapshot where
  status : ResponseStatus
  progressSeq : Nat

structure AttemptView where
  request : RequestSnapshot
  response : Option ResponseSnapshot
```

### Turn Identity

```lean
def TurnId := String

structure TurnObservation where
  turnId : TurnId
  attempts : List AttemptView
```

`TurnId` is `retry_root_request_id`. All attempts sharing the same root are part of one turn. The tip of the retry chain is the attempt whose `request_id` is not referenced as `retry_parent_request` by any other attempt in the observation.

### Derivation: Two Layers

**Layer 1 — single attempt:**

`deriveAttempt : AttemptView -> ClientTurnState`

Priority order:
1. `isSuperseded = true` or `lifecycleState = .superseded` -> `.superseded`
2. `lifecycleState = .completed` -> `.completed`
3. `lifecycleState = .failed` or `lifecycleState = .dead` -> `.failed`
4. (lifecycle is now known non-terminal: pending/claimed/processing/inputRequired)
5. Response exists with `.complete` -> `.completed`
6. Response exists with `.error` -> `.failed`
7. Response exists with `.streaming` -> `.streaming`
8. No response -> `.waitingForClaim`

Rules 2-3 checking server terminal states BEFORE response prevents stale
streaming responses from demoting a terminally failed/completed request.
This corrects the original spec ordering and is required for monotonicity
(T2) to hold.

The fall-through from the server lifecycle check to the response arm (rules
5-7) is where the "response can be more current than request" design
decision applies — but only when the lifecycle is non-terminal. Once the
server lifecycle is terminal, rules 2-3 fire and the response is ignored.

Rule 8 mapping `inputRequired` to `waitingForClaim` (handled in the
non-terminal fall-through when no response exists) is the Deviation #2
annotation. When the runtime persists `inputRequired`, clients can add a
6th state here.

**Layer 2 — full turn:**

`deriveTurn : TurnObservation -> Option ClientTurnState`

1. Find the tip attempt (no successor in the retry chain).
2. Apply `deriveAttempt` to the tip.
3. Return `none` if the turn has zero attempts.

### Merge

```lean
def mergeRequest : TurnObservation -> RequestSnapshot -> TurnObservation
def mergeResponse : TurnObservation -> String -> ResponseSnapshot -> TurnObservation
```

Under the merged-snapshot assumption (DefraDB delivers the CRDT-merged latest per document), merge is a map update keyed by `request_id`. Each incoming event replaces the stored snapshot for that request.

### Deviation Annotations

| Server state | Client mapping | Deviation |
|---|---|---|
| `inputRequired` | `.waitingForClaim` | #2: no persisted inputRequired path yet |
| `dead` | `.failed` | #3: clients derive exhaustion from failed + retry_count externally |

When the runtime catches up, the Lean model gains new `ClientTurnState` constructors and the conformance tests start enforcing them.

## Theorems

### T1: Convergence

Any permutation of the same set of document snapshots yields the same `TurnObservation`.

Trivially true under the merged-snapshot assumption (map keyed by `request_id`, each entry is the latest merged value). States the assumption explicitly so it can be strengthened later if we model raw DAG commits.

### T2: Monotonicity

If the server transitions a request forward (a valid `Transition` from `Proofs.Request`) or a response advances (`progressSeq` increases or `status` moves to terminal), `deriveTurn` rank never decreases.

This is the core safety property. It guarantees clients never see "completed then streaming again" or "streaming then waitingForClaim." Proof depends on the server's own monotonicity (proven in `Request.lean`).

### T3: Terminal Coherence

`deriveTurn(obs).isTerminal` iff the tip attempt's `lifecycleState` is terminal in the server model (completed, failed, superseded, or dead).

Exception: response-driven terminal. If the response says `complete` but the request hasn't transitioned yet (replication lag), the client shows `completed` before the request is terminal. The theorem accounts for this by defining "tip is effectively terminal" as "tip lifecycle is terminal OR tip response status is terminal."

### T4: Totality

`deriveTurn` is defined for every `TurnObservation` with at least one attempt. No reachable observation state is unhandled.

Pre-observation ("no turn yet") is handled by the caller, not the projection.

### T5: Turn Replacement

Adding a new attempt with `retry_parent_request = tip.request_id` changes the tip. The new `deriveTurn` result either equals the previous result or has equal-or-higher rank. Specifically:

- If the old tip was `failed` and a new attempt arrives as `pending`, the turn transitions from `failed` (rank 2) to `waitingForClaim` (rank 0). **This is the one allowed rank decrease** — retry restart. The theorem permits this specific case (old tip terminal failure, new tip is a retry) and prohibits all other decreases.

Similarly, when `superseded_by_request` is set on the tip, the derived state transitions to `.superseded` (rank 2), which is monotonic from any non-terminal state.

## Conformance Tests: `tests/client_state_conformance.rs`

Mirrors the existing `state_machine_conformance.rs` pattern.

- **Derivation table coverage**: every `(RequestState, Option ResponseStatus)` combination produces the expected `ClientTurnState`.
- **Out-of-order observation**: response arrives before request update; projection still correct.
- **Retry chain progression**: new attempt added, tip changes, derived state advances (or restarts per T5).
- **Supersession cutover**: `superseded_by_request` set, derived state transitions to `.superseded`.
- **Monotonicity spot checks**: no sequence of valid server transitions produces an impermissible rank decrease.
- **Terminal coherence spot checks**: client terminal iff server effectively terminal.

## Protocol Doc: `docs/protocols/client-state-machine.md`

### Structure

1. **Purpose** — what this protocol specifies and who it is for (CLI, mobile, web, desktop implementers).
2. **Formal model reference** — "the source of truth is `Proofs/Client.lean`; this document explains it."
3. **Derivation table** — human-readable mapping from document fields to client states, with examples.
4. **Current deviations** — references `Proofs/Conformance/Deviations.lean`, explains how the derivation handles each.
5. **Parallel observation surfaces** (informal):
   - `AgentToolCall` — filter by `session_id`, render inline by `message_sequence` ordering, `status` tracks tool execution lifecycle.
   - `AgentToolResult` — full output for completed tools, keyed by `session_id` + `tool_name`.
   - `AgentMessage` — ordered transcript by `sequence` field, used for scroll-back history. Not on the critical streaming path.
6. **Subscription model** — which collections a client must watch:
   - `AgentRequest` by `session_id` (or `retry_root_request` for turn-scoped view)
   - `AgentResponse` by `request_id` (the active request in the turn)
   - `AgentToolCall` by `session_id`
   - `AgentToolResult` by `session_id`
   - `AgentMessage` by `session_id`
   - `AgentConversation` by `agent_did`
7. **Reference pseudocode** — `deriveAttempt` in Swift, TypeScript, and Rust (~20 lines each).

## Implementation Order

1. **Lean model** — `crates/defra-agent/proofs/Proofs/Client.lean`. Define types, write `deriveAttempt` and `deriveTurn`, prove T1-T5.
2. **Conformance tests** — `tests/client_state_conformance.rs`. Bridge Lean semantics to Rust with concrete document shapes.
3. **Protocol doc** — `docs/protocols/client-state-machine.md`. Explain the model, add parallel surfaces and subscription guidance.
4. **(Future, separate issue)** Retrofit Amy's `RequestLifecycle.swift` to align with the formal derivation. Currently Amy uses `status` strings; the formal model uses `lifecycle_state` from the proven enum.

## Out of Scope

- Client transport/connection state (per-client concern, not a universal protocol)
- P2P DAG convergence proofs (defradb.rs territory)
- Runtime reconcile observation (read `AgentRuntime` directly, no projection needed)
- Multi-turn session state machine (one turn is the hard part; sessions are future work)
- `awaitingInput` as a distinct client state (blocked on Deviation #2 in the runtime)
- `dead` as a distinct client state (blocked on Deviation #3 in the runtime)
- Manifest validate/diff/apply workflow (#8)
- MCP health-driven tool surface changes (orthogonal to turn observation)
