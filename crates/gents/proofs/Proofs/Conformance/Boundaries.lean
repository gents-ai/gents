import Proofs.Conformance.ContractTypes

namespace Conformance.Contracts

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

def boundaryFleetSlotAccountingDerivedViewId : String :=
  "boundary.fleet-slot-accounting.derived-view"

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

def boundarySessionRecoveryClientRetrySurfaceId : String :=
  "boundary.session-recovery.client-retry-surface"

def boundaryCoverageLedgerReviewDisciplineId : String :=
  "boundary.coverage-ledger.review-discipline"

def boundaryEventDeliveryFairSubstrateId : String :=
  "boundary.event-delivery.fair-substrate"

def boundaryEventDeliveryRescanDocCapId : String :=
  "boundary.event-delivery.rescan-doc-cap"

def boundaryStreamingResponseIdleTimeoutDeadlineId : String :=
  "boundary.streaming-response.idle-timeout-deadline"

def boundaryPromptAssemblyProviderInputSanitizationId : String :=
  "boundary.prompt-assembly.provider-input-sanitization"

def boundaryModelNatTypedIdsTimeId : String :=
  "boundary.model.nat-typed-ids-time"

def boundaryP2pBackpressureObligationModelId : String :=
  "boundary.p2p-backpressure.obligation-model"

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
  , { id := boundaryFleetSlotAccountingDerivedViewId
    , domain := "FleetSlotAccounting"
    , subject := "derived scheduler aggregate"
    , statement :=
        "FleetSlotAccounting is a derived proof view over request admission states projected to InferenceCall reconstruction rows; Rust does not persist or consume a separate FleetState aggregate."
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
        "Lean-generated cases and Rust hook tests cover local fail-closed termination versus fail-open lost-output continuation; Rust still does not expose per-token persistence documents."
    , acceptedFailureMode :=
        some "Fail-open acknowledges lost output by policy."
    }
  , { id := boundaryStorageObservationDaemonVisibleId
    , domain := "StorageObservation"
    , subject := "daemon-visible storage facts"
    , statement :=
        "Lean-generated cases and Rust hook tests cover local success/failure and stale/read-visible observation classification; daemon-visible storage facts remain observations, not proofs of DefraDB internals."
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
        "Lean-generated cases and Rust tests cover observed-document admission availability composed with the local runtime's measured probe health (Proofs/BackendHealth, #640: scheduled probes, K-failure demotion, single-success promotion). Provider behavior beyond the probed models endpoint, cross-runtime reachability divergence, and fleet-wide document freshness remain environmental."
    }
  , { id := boundarySessionRecoveryClientRetrySurfaceId
    , domain := "SessionRecovery"
    , subject := "client retry surface"
    , statement :=
        "Generated SessionRecovery cases cover DB-backed client retry/reissue and denial guards; gents startup recovery does not autonomously create successor requests."
    , acceptedFollowUp :=
        some "Add a separate contract if gents core gains autonomous server-side reissue."
    }
  , { id := boundaryCoverageLedgerReviewDisciplineId
    , domain := "CoverageLedger"
    , subject := "advisory contract guard"
    , statement :=
        "CoverageLedger is a checked index requiring every emitted domain to name a Rust or TypeScript consumer, accepted boundary, or follow-up; this boundary documents the ledger discipline as a whole."
    }
  , { id := boundaryEventDeliveryFairSubstrateId
    , domain := "event_delivery"
    , subject := "Fair substrate delivery"
    , statement :=
        "EventDelivery's Fair predicate assumes rescanTick actions occur with " ++
        "bounded gap. Substrate-level fairness (DefraDB gossip + libp2p delivery) " ++
        "is taken as an axiom; the substrate model lives in tla/ReversePairing.tla."
    , acceptedFollowUp :=
        some "Substrate fairness is proved separately in tla/ReversePairing.tla; see also #162."
    }
  , { id := boundaryEventDeliveryRescanDocCapId
    , domain := "event_delivery"
    , subject := "EventSource introspection rescan completeness"
    , statement :=
        "The Lean rescanTick transition models a complete rescan that surfaces " ++
        "every unprocessed persistent doc. The live EventSource rescan seeds and " ++
        "re-queries at most SEEN_DOCS_SEED_LIMIT (10_000) docs per source " ++
        "collection with no pagination, so for collections larger than that cap " ++
        "the tail beyond the first 10_000 docs is not surfaced by rescan and " ++
        "stays dependent on the lossy subscription path. v1 does not target " ++
        "catalog-scale source collections. SubagentSource's running-bridge rescan " ++
        "is not subject to this cap."
    , acceptedFailureMode := some "missed_event_observation"
    , acceptedFollowUp :=
        some "Paginate EventSource rescan past SEEN_DOCS_SEED_LIMIT to eliminate the residual missed_event_observation mode; tracked in #564."
    }
  , { id := boundaryStreamingResponseIdleTimeoutDeadlineId
    , domain := "StreamingResponse"
    , subject := "stream idle timeout deadline precondition"
    , statement :=
        "StreamingResponse streamIdleTimeout transitions assume the runtime only fires the timeout after the stream idle deadline has elapsed; Rust satisfies this with the configured liveness timeout rather than a persisted response-clock field."
    }
  , { id := boundaryPromptAssemblyProviderInputSanitizationId
    , domain := "PromptAssembly"
    , subject := "provider input sanitization"
    , statement :=
        "Durable transcripts may contain unpaired assistant tool-call rows while tool execution is interrupted, failed, or in flight; provider sends must narrow loaded history through sanitize_history_for_provider so no dangling tool call reaches the backend."
    }
  , { id := boundaryModelNatTypedIdsTimeId
    , domain := "CoreTypes"
    , subject := "Nat-typed IDs and Time"
    , statement :=
        "Core identifiers and Time are Nat abbreviations (Proofs/Basic and friends). Lifecycle and ordering proofs only need decidable equality and ordering. The abstraction deliberately omits wall-clock skew, UUID/string parse/serialize failures, ID-namespace collisions, and cross-node identity mismatches (AgentDid/PeerId/RequestId across deployments)."
    , acceptedFailureMode :=
        some "Collapsing distinct real identities to the same Nat equality could mask a cross-node identity bug class the distributed system can hit."
    , acceptedFollowUp :=
        some "Cross-node identity uniqueness is not claimed in Lean; load-bearing distributed identity/membership obligations live in tla/ (pairing/transport) and Rust integration tests. Targeted models only if a load-bearing collision class appears outside those fences (#558)."
    }
  , { id := boundaryP2pBackpressureObligationModelId
    , domain := "P2PBackpressure"
    , subject := "hub admission obligation model vs shipping flood safety"
    , statement :=
        "Proofs.P2PBackpressure and tla/P2PBackpressure are one-wave obligation models for success-ack backing, pending capacity, and timeout slot release. They do NOT refine the shipping p2p coordinator. The pinned DefraDB implementation now (a) admits compact jobs into a bounded item/byte queue before worker execution, coalesces and schedules per peer, and hands admission overflow to a persisted retry ladder, and (b) persists push-originated pending-DAG registrations before success acknowledgement and re-drives them after restart. Those multi-wave, store-before-ack, and restart properties remain outside the formal model and generated conformance contract."
    , acceptedFailureMode :=
        some "A future queue-admission, durable-retry, store-before-ack, or restart-recovery regression can pass this one-wave model; the separate pinned-struct observability and P2P end-to-end tests are implementation fences, not proofs of those properties."
    , acceptedFollowUp :=
        some "Extend the distributed model to bounded multi-wave queue admission plus durable retry and pending-DAG restart recovery, bind its witness rows to the pinned DefraDB adapter, and TLC-check MCP2PBackpressure*. Tracked under #630."
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
