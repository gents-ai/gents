# Recovery-Action Enumeration in Lean - Design

**Status:** Approved
**Date:** 2026-05-13
**Tracks:** issue #189; refs #183 and deadline audit #172 follow-ups #4 and #6
**Scope:** Lean recovery-sweep contract and conformance vectors for persisted startup recovery. No Rust production recovery implementation changes.

## Background

The current L3 theorem, `Proofs/Properties/Liveness.lean` `recovery_convergence`, proves that a finite list of stuck requests reaches terminal request states. It does not enumerate the concrete startup sweeps that Rust runs today:

- `RequestLifecycle::recover_all`
- streaming `AgentResponse` recovery inside `RequestLifecycle::recover_all`
- `ToolCallLifecycle::recover_all`
- the missing `InferenceCall::recover_all` obligation

The 2026-05-13 formal coverage audit ranks this as gap #6 because the model has no closed list of persisted collections that must participate in startup recovery. The 2026-05-12 deadline audit shows the practical failures: stale `InferenceCall.call_state` rows can survive restart, and detached subagent bridge tool rows are skipped by tool-call recovery.

## Brainstorming Decisions

1. **Use a closed registry, not a typeclass.** A typeclass is open-world: adding a new persisted collection without an instance would not necessarily fail a central theorem. The contract should be a finite Lean list plus coverage theorems over a sealed collection enum.
2. **Put the contract on the recovery sweep/function, not only the collection.** The Rust surface is a sweep entry point with one or more row mappings. `RequestLifecycle::recover_all`, for example, covers request rows and streaming response rows. The registry should name both the Rust sweep and the modeled persisted collection clause.
3. **Model startup persisted sweeps only.** Live observers such as interrupt polling are not `#189` recovery actions unless they are later promoted to a periodic persisted-row sweep. The v1 cadence is `startup`.
4. **Missing Rust paths are obligations.** `InferenceCall::recover_all` and detached bridge terminalization are represented as Lean obligations and emitted conformance rows. PR E and the bridge terminal wiring follow-up will satisfy them in Rust later.
5. **Detached bridge rows are not an allowed skip.** A detached subagent tool row with a terminalizing cause is stale under the contract. The current Rust skip must be made visible as an implementation gap, not encoded as an accepted exception.
6. **Keep response recovery narrow.** This design models only the recovery clause `AgentResponse.status = streaming -> error`. It does not add the full server-side response lifecycle from follow-up #190.
7. **Do not model `AgentConversation` in v1.** `recover_stuck_conversations` is operational repair inside `RequestLifecycle::recover_all`, but it is not a terminalizing persisted state machine in the current Lean conformance registry. Adding it would broaden #189 beyond the audit's named recovery gaps.
8. **Keep implementation status binary by splitting tool-call predicates.** The existing tool-call startup sweep and the missing detached-bridge recovery path are separate registry rows. Each row represents one stale-row predicate, so `RecoveryImplementationStatus` only needs `implemented` and `obligation`.

## Lean Contract Shape

Add a recovery module under `crates/defra-agent/proofs/Proofs/Recovery/`.

The core shape is a dependent record so each sweep can use the row type that already matches its model:

```lean
inductive RecoveryCadence
  | startup

inductive RecoveryImplementationStatus
  | implemented
  | obligation

inductive PersistedRecoveryCollection
  | agentRequest
  | agentResponse
  | agentToolCall
  | inferenceCall

structure RecoverySweep where
  Row : Type
  collection : PersistedRecoveryCollection
  sweepId : String
  rustFunction : String
  cadence : RecoveryCadence
  implementationStatus : RecoveryImplementationStatus
  stale : Row -> Prop
  recover : Row -> Row
  terminal : Row -> Prop
  measure : Row -> Nat
  h_stale_positive : forall row, stale row -> measure row > 0
  h_recover_terminal : forall row, stale row -> terminal (recover row)
  h_recover_zero : forall row, stale row -> measure (recover row) = 0
```

The aggregate progress measure for a finite sweep input is the sum of row measures. Generic theorems prove:

- recovering one stale row strictly decreases the aggregate measure;
- mapping `recover` over a finite list drives the aggregate measure to zero;
- every recovered stale row is terminal under that sweep's `terminal` predicate.

The existing L3 theorem is not weakened. The request instance can cite the same stuck-row shape and terminal mapping that L3 already proves, but the generic recovery registry proves its own finite-list theorem over the registered sweep row.

## Coverage Registry

Add a sealed collection list:

```lean
def PersistedRecoveryCollection.all : List PersistedRecoveryCollection :=
  [ .agentRequest, .agentResponse, .agentToolCall, .inferenceCall ]
```

Add:

```lean
def registeredRecoverySweeps : List RecoverySweep := [...]

theorem registered_sweeps_cover_persisted_collections :
  forall c, c in PersistedRecoveryCollection.all ->
    exists sweep, sweep in registeredRecoverySweeps /\ sweep.collection = c
```

This is the Lean failure point for "new modeled persisted state machine added, no recovery sweep registered." A future collection must be added to `PersistedRecoveryCollection.all`; the coverage theorem fails until a sweep is registered.

Conformance JSON should add a `recovery_sweep_cases` array. Each row includes:

- `sweep_id`
- `collection`
- `rust_function`
- `cadence`
- `implementation_status`
- `pre_state`
- `terminal_state`
- `measure_before`
- `measure_after`
- `deadline_audit_ref`

Add a `CoverageLedger` category `recovery_sweep_cases`. Implemented Rust sweeps can have concrete consumers now. Missing Rust sweeps should use follow-up coverage pointing at PR E and the bridge terminal wiring follow-up until those PRs implement the obligations.

## Registered Sweeps

| Sweep id | Collection | Rust function | Status | Stale rows | Terminal mapping | Finiteness witness |
| --- | --- | --- | --- | --- | --- | --- |
| `request_lifecycle_recover_all_requests` | `AgentRequest` | `RequestLifecycle::recover_all` | implemented | request state `claimed` or `processing` | `failed` in the conservative contract; Rust may map complete response rows to `completed` | count of stuck request rows |
| `request_lifecycle_recover_all_streaming_responses` | `AgentResponse` | `RequestLifecycle::recover_all` | implemented | response status `streaming` | `error` | count of streaming response rows |
| `tool_call_lifecycle_recover_all_running_calls` | `AgentToolCall` | `ToolCallLifecycle::recover_all` | implemented | non-detached running tool calls with deadline exceeded, interrupted parent, terminal parent, or terminal child bridge observation | `timedOut`, `cancelled`, `failed`, or `completed` by cause | count of actionable non-detached running tool rows |
| `tool_call_lifecycle_recover_detached_bridge_rows` | `AgentToolCall` | `ToolCallLifecycle::recover_detached_bridge_rows` | obligation | detached subagent bridge tool row with a terminalizing cause | terminal mapping per bridge contract: child completed -> `completed`, child interrupted -> `cancelled`, other child terminal/terminal parent -> `failed`, deadline exceeded -> `timedOut` | count of stale detached bridge rows |
| `inference_call_recover_all_stale_calls` | `InferenceCall` | `InferenceCall::recover_all` | obligation | `queued` or `running` calls from dead runtime, terminal parent, interrupted parent, or expired request context | proposed: `queued -> cancelled`; `running -> failed`; interrupted parent -> `cancelled` | count of stale inference-call rows; terminal rows contribute zero slots |

Tool-call recovery treats background subagent rows with no terminalizing cause as not stale. Detached rows with a terminalizing cause are stale under the sibling obligation sweep; they are not filtered out of the modeled contract.

## Import Coordination

Planned Lean files:

```text
crates/defra-agent/proofs/Proofs/Recovery/Contract.lean
crates/defra-agent/proofs/Proofs/Recovery/Sweeps.lean
crates/defra-agent/proofs/Proofs/Recovery/ContractCases.lean
crates/defra-agent/proofs/Proofs/Recovery.lean
```

`Proofs/Recovery/Sweeps.lean` imports `Proofs.Properties.Liveness`, `Proofs.InferenceCall`, `Proofs.ToolExecution`, and `Proofs.Subagent`.

`Proofs.lean` should import `Proofs.Recovery` after `Proofs.Properties.Liveness`. This is the only expected import-list coordination point with the #191 and #188 agents.

`Proofs/Conformance/Contracts/Json.lean` should import `Proofs.Recovery.ContractCases` to emit `recovery_sweep_cases`.

## Approved Questions

1. `InferenceCall` terminal mapping is approved: stale `queued -> cancelled`, stale `running -> failed`, and any interrupted-parent row -> `cancelled`.
2. `AgentConversation` is excluded from v1 because it is not a terminalizing persisted state machine in the current conformance registry.
3. `Proofs.lean` may import `Proofs.Recovery` after `Proofs.Properties.Liveness`.
