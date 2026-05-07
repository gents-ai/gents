import Proofs.Conformance.ContractTypes

/-!
# Conformance Boundaries and Product Policies

This file records intentional product semantics and external assumptions at the
Rust/Lean boundary. These are not active spec deviations.

## Current Request Lifecycle Product Semantics

The persisted `AgentRequest.lifecycle_state` vocabulary has nine strings:
`pending`, `claimed`, `processing`, `inputRequired`, `completed`, `failed`,
`superseded`, `dead`, and `interrupted`.

The current core request transition machine uses these current-product paths:

* `pending -> claimed` for successful claim while `valid_until` is open.
* `pending -> superseded` for losing latest-only/deduplication arbitration.
* `claimed -> processing` when inference execution begins.
* `processing -> processing` for progress.
* `processing -> completed` for successful terminal completion.
* `processing -> failed` for provider failure, retry exhaustion, tool failure,
  or post-claim deadline expiry.
* `claimed -> failed` for failure before streaming starts.
* `pending -> dead` only for stale pre-claim TTL expiry.
* `pending | claimed | processing -> interrupted` for cancellation.

`inputRequired` is reserved persisted/client protocol vocabulary. Rust does not
currently emit it because there is no first-class approval or human-input loop;
autonomous tool calls run inline. Rust active runtime lifecycle filters are
limited to `pending`, `claimed`, and `processing`, so reserved `inputRequired`
rows are not interrupted or superseded by autonomous lifecycle code. Future
approval work should add an explicit extension module or widen the core
transition relation together with Rust writer tests.

`dead` is a real terminal persisted state only for stale pre-claim work. Once a
request is claimed, provider failures, retry exhaustion, tool failures, and
deadline expiry remain terminal `failed`.

## Intentional Product Policies

Tool-call failures are classified as permanent until tool metadata can prove
retry safety. Retrying tool calls without health, idempotency, and side-effect
metadata can repeat side effects, so the request transition system does not
model tool retries and Rust treats `StreamingError::Tool(_)` as a permanent
failure.

`Proofs.ToolExecution` is the initial local model for future MCP/tool execution
semantics. It currently proves only the service-local boundary Rust enforces:
unreachable services and invalid preflight schemas block dispatch, `list_tools`
transport retries are safe-read retries, and `call_tool` transport retries
require explicit idempotency evidence. Rust does not currently persist or
consume idempotency metadata for MCP tools, so `McpPool::call_tool` must not
implicitly retry after dispatch failure. Future tool retries should first extend
`ToolExecution.IdempotencyEvidence`, add a Rust contract for the advertised
metadata, and only then widen `McpPool::call_tool` retry behavior.

The scheduler's aggregate fleet slot state is reconstructed from
`InferenceCall` rows. A backend's held slot count is the derived count of rows
with `call_state = "running"`; queued rows are waiting for a semaphore permit
and terminal rows (`cancelled`, `completed`, `failed`) have released any permit.
There is intentionally no denormalized persisted `FleetState` document that
must carry the aggregate invariant.

The command-policy model covers local validation and selection logic for
`CommandExecutionMode`, `CommandNetworkMode`, argv allowed/forbidden prefixes,
read-only command allowlisting, sandbox labels, and filtered shell environment
keys. It does not prove that an invoked external binary is semantically
read-only, nor does it prove the host kernel's sandbox implementation. Rust
tests cover the parser/validator boundary and command metadata emitted by
`toolset/shared/command.rs`; the Lean model covers the fail-closed policy
ordering and sandbox/env invariants that those tests exercise.

The trigger-engine contract now has two layers. Lean emits finite executable
dispatch cases from `Proofs.Conformance.Triggers.Contracts`; Rust consumes them
in `trigger_engine::tests::trigger_engine_dispatch_matches_lean_generated_contract_cases`.
That generated contract covers manual fires with null trigger ids, schedule and
event enabled-gate reachability, tuple-sensitive serial gating, latest-only
supersession including the concrete `superseded_prior_ids`, parallel bypass of
in-flight gates, manual latest-only without a trigger key, materialized lineage,
and the interactive/scheduled execution-origin projection. A separate
deterministic Rust lock test covers the latest-only critical section without
using elapsed time as the only oracle.

## Modeled Storage Observation Boundary

`PersistenceState` remains the abstract committed/uncommitted lifecycle. The
separate `StorageObservation` model records the daemon-visible storage facts
that justify moving through that lifecycle:

* an awaited mutation that returns success is treated as a committed write;
* a mutation error is not treated as committed;
* fail-closed storage errors return to retryable uncommitted state;
* fail-open storage errors acknowledge the output as lost;
* stale reads or missing/stale events may occur after a success ack, but they do
  not invalidate the commit assumption; and
* the daemon assumes a minimum visibility path: read-your-writes for local
  confirmation paths, or eventual observation after a stale read/event.

These are daemon storage assumptions, not proofs of DefraDB internals. Rust uses
`StreamBuffer`, `DefraSessionHook`, and hook failure policy around DefraDB
mutations; it does not persist a per-token `PersistenceState` or
`StorageObservation` document. Storage-engine crash windows, transport delivery,
global CRDT convergence, and event-bus delivery correctness remain external
DefraDB/environment assumptions.

This section is the modeled part of the former broad storage assumption; the
following section keeps the still-external DefraDB/environment assumptions
separate.

## External Assumptions

Backend health and availability observations are only as fresh as the backend
documents visible at admission time. Endpoint freshness and network/provider
behavior are environmental assumptions, not service-local state-machine facts.
The service-local proof and tests cover the consequence of an observed backend
configuration: reconstructed running call rows do not exceed that backend's
`max_concurrent`.

Trigger source delivery remains operational. Lean does not model the DefraDB
event bus, control-watcher debounce, schedule tick cadence, subscription
reconciliation timing, template-language parser behavior, or storage-engine
delivery guarantees. Rust conformance tests cover those surfaces with bounded
waits and persistence-shape assertions; the Lean-generated trigger contract
covers the pure dispatch/reachability/concurrency semantics once a source has
produced a `FireIntent`.

The generated `SessionRecovery` conformance contract currently covers the
finite failed-latest-request reissue witness (`failed -> pending`) instead of
the full request lifecycle vocabulary. It is a smoke contract for the executable
session boundary, not a complete request-state coverage claim. Future
session-recovery executable witnesses should widen that contract before Rust
depends on broader transition coverage from it.

## Coverage Ledger Policy

`Proofs.Conformance.CoverageLedger` is the checked index for the
`Proofs.Conformance.Contracts` JSON. Rust/TypeScript conformance coverage must
account for every emitted vocabulary, state machine, trigger dispatch case
group, runtime-reconcile witness group, session-recovery witness group,
inference-slot witness group, fleet-slot witness group, frontend/desktop
ClientShell witness group, tool-execution witness group, command-policy
validation/sandbox/env witness group, and follow-up hook; Rust checks that each
appears in that ledger exactly once.
Boundary and deviation metadata is emitted as structured review metadata and is
shape-checked separately; ledger `accepted_boundary` fields reference the stable
boundary ids emitted by this file.

A ledger entry is acceptable only when it names a Rust/TypeScript consumer, an
intentional product boundary recorded in this file, or an accepted follow-up.
Future executable trigger, runtime, session-recovery, slot/fleet,
`ClientShell`, `ToolExecution`, or `CommandPolicy` contracts should therefore
add both the emitted contract domain and its runtime consumer in the same
change. ClientShell rows should be assigned to the frontend list when they only
exercise React shell state and to the desktop list when they exercise the Rust
session-snapshot bridge. If the runtime consumer is deliberately deferred, the
ledger entry must point here to describe the boundary or carry a follow-up hook;
otherwise Rust will reject the generated contract as advisory-only.

## Closed Historical Items

These were previous conformance gaps and are now closed product/spec behavior:

* `recovering` is an explicit persisted process state. Startup publishes it
  before recovery work and only starts request watchers/routers after
  `recover_all()` and startup runtime publication complete.
* Claim and inference retry waits are bounded by submitter TTL and claimed
  request deadlines; stale pre-claim rows become `dead/Stale`.
* Interrupting a request has an end-to-end path to cancelling queued/running
  linked `InferenceCall` rows.
-/

namespace Conformance.Contracts

-- Rust pins the complete id set in
-- state_machine_conformance::lean_boundary_metadata_is_typed_and_reviewable.
-- That duplicated list is the deliberate review gate for boundary drift.
structure Boundary where
  id : String
  domain : String
  subject : String
  statement : String
  acceptedFailureMode : Option String := none
  acceptedFollowUp : Option String := none
  deriving Repr

def boundaryRequestInputRequiredReservedId : String :=
  "boundary.request.input-required-reserved"

def boundaryRequestDeadPreclaimOnlyId : String :=
  "boundary.request.dead-preclaim-only"

def boundaryToolCallPermanentWithoutRetryEvidenceId : String :=
  "boundary.tool-call.permanent-without-retry-evidence"

def boundaryMcpCallToolDispatchRetryEvidenceId : String :=
  "boundary.mcp.call-tool-dispatch-retry-evidence"

def boundaryInferenceSlotsRunningRowDerivedId : String :=
  "boundary.inference-slots.running-row-derived"

def boundaryCommandPolicyHostExecutionAssumptionsId : String :=
  "boundary.command-policy.host-execution-assumptions"

def boundaryTriggerDispatchSourceDeliveryId : String :=
  "boundary.trigger.dispatch-source-delivery"

def boundaryPersistenceAbstractLifecycleId : String :=
  "boundary.persistence.abstract-lifecycle"

def boundaryStorageHookFailurePolicyId : String :=
  "boundary.storage.hook-failure-policy"

def boundaryStorageObservationDaemonVisibleId : String :=
  "boundary.storage.observation-daemon-visible"

def boundaryStorageMinimumVisibilityPathId : String :=
  "boundary.storage.minimum-visibility-path"

def boundaryBackendHealthAdmissionFreshnessId : String :=
  "boundary.backend-health.admission-freshness"

def boundarySessionRecoveryFailedLatestSmokeId : String :=
  "boundary.session-recovery.failed-latest-smoke"

def boundaryCoverageLedgerReviewDisciplineId : String :=
  "boundary.coverage-ledger.review-discipline"

def boundaries : List Boundary :=
  [ { id := boundaryRequestInputRequiredReservedId
    , domain := "RequestLifecycle"
    , subject := "inputRequired vocabulary"
    , statement :=
        "inputRequired is reserved persisted and client protocol vocabulary; Rust does not emit it until a first-class approval or human-input loop exists."
    , acceptedFollowUp :=
        some "Future approval work should extend the core transition relation and Rust writer tests."
    }
  , { id := boundaryRequestDeadPreclaimOnlyId
    , domain := "RequestLifecycle"
    , subject := "dead terminal state"
    , statement :=
        "dead is terminal only for stale pre-claim work; post-claim provider, retry, tool, and deadline failures remain failed."
    }
  , { id := boundaryToolCallPermanentWithoutRetryEvidenceId
    , domain := "ToolExecution"
    , subject := "tool-call failure retry policy"
    , statement :=
        "Tool-call failures are permanent unless metadata proves retry safety; the request machine does not model tool retries."
    , acceptedFollowUp :=
        some "Future retries need health, idempotency, and side-effect metadata before widening Rust behavior."
    }
  , { id := boundaryMcpCallToolDispatchRetryEvidenceId
    , domain := "ToolExecution"
    , subject := "McpPool call_tool retry boundary"
    , statement :=
        "Rust does not persist MCP idempotency metadata, so McpPool::call_tool must not implicitly retry after dispatch failure."
    , acceptedFollowUp :=
        some "Extend ToolExecution.IdempotencyEvidence and add a Rust metadata contract before adding call_tool transport retries."
    }
  , { id := boundaryInferenceSlotsRunningRowDerivedId
    , domain := "InferenceCall"
    , subject := "fleet slot accounting"
    , statement :=
        "Backend held slots are derived from InferenceCall rows with call_state running; no denormalized FleetState document carries the aggregate invariant."
    }
  , { id := boundaryCommandPolicyHostExecutionAssumptionsId
    , domain := "CommandPolicy"
    , subject := "host command semantics"
    , statement :=
        "The command-policy model proves fail-closed policy ordering and sandbox/environment invariants, not external binary read-only semantics or host kernel sandbox correctness."
    }
  , { id := boundaryTriggerDispatchSourceDeliveryId
    , domain := "TriggerDispatch"
    , subject := "trigger source delivery"
    , statement :=
        "Lean covers pure dispatch and concurrency semantics once a FireIntent exists; DefraDB event delivery, schedule ticks, debounce, and template parsing remain operational assumptions."
    }
  , { id := boundaryPersistenceAbstractLifecycleId
    , domain := "Persistence"
    , subject := "abstract persistence lifecycle"
    , statement :=
        "PersistenceState is an abstract committed/uncommitted lifecycle; Rust does not persist a per-token PersistenceState document."
    }
  , { id := boundaryStorageHookFailurePolicyId
    , domain := "Persistence"
    , subject := "hook storage failure policy"
    , statement :=
        "Rust hook policy observes storage failure as fail-closed retry or fail-open lost output instead of exposing per-token persistence documents."
    , acceptedFailureMode :=
        some "Fail-open acknowledges lost output by policy."
    }
  , { id := boundaryStorageObservationDaemonVisibleId
    , domain := "StorageObservation"
    , subject := "daemon-visible storage facts"
    , statement :=
        "StorageObservation records daemon-visible mutation and read facts that justify lifecycle movement; it is not a proof of DefraDB internals."
    }
  , { id := boundaryStorageMinimumVisibilityPathId
    , domain := "StorageObservation"
    , subject := "minimum visibility path"
    , statement :=
        "After a successful mutation ack, stale reads or events may occur, but the daemon assumes read-your-writes for local confirmation or eventual later observation."
    , acceptedFailureMode :=
        some "Stale reads or missing events after success acknowledgement do not invalidate the commit assumption."
    }
  , { id := boundaryBackendHealthAdmissionFreshnessId
    , domain := "Admission"
    , subject := "backend health freshness"
    , statement :=
        "Backend health and availability observations are only as fresh as backend documents visible at admission time; provider and network behavior are environmental."
    }
  , { id := boundarySessionRecoveryFailedLatestSmokeId
    , domain := "SessionRecovery"
    , subject := "finite failed-latest witness"
    , statement :=
        "The generated SessionRecovery contract covers failed -> pending reissue as an executable smoke boundary, not full request-state transition coverage."
    , acceptedFollowUp :=
        some "Future executable witnesses should widen the contract before Rust depends on broader transition coverage."
    }
  , { id := boundaryCoverageLedgerReviewDisciplineId
    , domain := "CoverageLedger"
    , subject := "advisory contract guard"
    , statement :=
        "CoverageLedger is a checked index requiring every emitted domain to name a Rust or TypeScript consumer, accepted boundary, or follow-up; this boundary documents the ledger discipline as a whole."
    }
  ]

def Boundary.toJson (boundary : Boundary) : String :=
  "{"
    ++ "\"id\":" ++ jsonString boundary.id ++ ","
    ++ "\"domain\":" ++ jsonString boundary.domain ++ ","
    ++ "\"subject\":" ++ jsonString boundary.subject ++ ","
    ++ "\"statement\":" ++ jsonString boundary.statement ++ ","
    ++ "\"accepted_failure_mode\":"
      ++ jsonOptionalString boundary.acceptedFailureMode ++ ","
    ++ "\"accepted_follow_up\":"
      ++ jsonOptionalString boundary.acceptedFollowUp
    ++ "}"

def boundariesJson : String :=
  jsonArray (boundaries.map Boundary.toJson)

end Conformance.Contracts
