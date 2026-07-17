import Proofs.Basic
import Proofs.Scheduling
import Proofs.RuntimeReconcile.State

/-!
# Finite Conformance Witness Types
-/

namespace Conformance.ContractCases

structure RuntimeReconcileCase where
  name : String
  action : String
  legal : Bool
  prePhase : String
  postPhase : String
  preActiveGeneration : Nat
  postActiveGeneration : Nat
  preRouterGeneration : Nat
  postRouterGeneration : Nat
  preReadyGenerationCount : Nat
  postReadyGenerationCount : Nat
  preLiveGenerationCount : Nat
  postLiveGenerationCount : Nat
  preInFlightCount : Nat
  postInFlightCount : Nat
  trackedRequestId : RequestId
  trackedSessionId : SessionId
  trackedRequestGeneration : Generation
  trackedRequestSession : SessionId
  trackedRequestBehavior : BehaviorId
  trackedSessionBehavior : BehaviorId
  deriving Repr

structure SessionRecoveryCase where
  name : String
  action : String
  legal : Bool
  preLatestState : String
  preFailedState : String
  postLatestState : String
  postFailedState : String
  postNewState : String
  preLatestAdmission : String
  postLatestAdmission : String
  preFailedAdmission : String
  postFailedAdmission : String
  postNewAdmission : String
  preOrigin : String
  postNewOrigin : String
  preBackend : String
  postNewBackend : String
  failedId : RequestId
  newId : RequestId
  preLatestId : RequestId
  postLatestId : RequestId
  preSessionId : SessionId
  postSessionId : SessionId
  preBehaviorId : BehaviorId
  postBehaviorId : BehaviorId
  preRequestCount : Nat
  postRequestCount : Nat
  preRetryCount : Nat
  postRetryCount : Nat
  maxRetries : Nat
  preDeadlineExceeded : Bool
  postDeadlineExceeded : Bool
  preFailedIsLatest : Bool
  postFailedIsLatest : Bool
  postNewIsLatest : Bool
  preRequestIds : List RequestId
  preFailedExists : Bool
  preLatestExists : Bool
  preNewRequestExists : Bool
  oldRequestRetained : Bool
  newRequestInserted : Bool
  originPreserved : Bool
  backendPreserved : Bool
  deriving Repr

structure InferenceSlotAccountingCase where
  name : String
  property : String
  backendId : String
  preState : String
  postState : String
  contribution : Nat
  expectedContribution : Nat
  preContribution : Nat
  postContribution : Nat
  releasedSlot : Bool
  permitDropTerminalization : Bool
  rowStates : List String
  rowBackendIds : List String
  reconstructedRunningCount : Nat
  maxConcurrent : Nat
  boundedByMaxConcurrent : Bool
  deriving Repr

structure FleetSlotAccountingCase where
  name : String
  property : String
  backendId : String
  requestState : String
  admissionState : String
  contribution : Nat
  expectedContribution : Nat
  activeCount : Nat
  schedulerRunning : Nat
  slotCount : Nat
  rowStates : List String
  rowBackendIds : List String
  reconstructedRunningCount : Nat
  maxConcurrent : Nat
  boundedByMaxConcurrent : Bool
  aggregateReconstructedNotPersisted : Bool
  deriving Repr

structure PersistenceFailurePolicyCase where
  name : String
  policy : String
  action : String
  prePersistence : String
  postPersistence : String
  postStorageObservation : String
  hookDecision : String
  recordsFailure : Bool
  recordsSuccess : Bool
  externalDurabilityClaimed : Bool
  deriving Repr

structure StorageObservationRuntimeCase where
  name : String
  policy : String
  action : String
  preObservation : String
  mutationResult : String
  postObservation : String
  postPersistence : String
  hookResult : String
  recordsFailure : Bool
  recordsSuccess : Bool
  terminalWriteObserved : Bool
  externalVisibilityClaimed : Bool
  deriving Repr

structure BackendHealthAdmissionCase where
  name : String
  enabled : Bool
  probeStatus : String
  expectedAvailable : Bool
  admissionDecision : String
  observedDocumentOnly : Bool
  externalEndpointFreshnessClaimed : Bool
  deriving Repr

structure NativeFilesystemBoundaryCase where
  name : String
  toolName : String
  workClass : String
  boundary : String
  innerPollBlocks : Bool
  requestDeadlineMs : Nat
  blockerMs : Nat
  expectedTerminal : String
  expectedFailureClass : Option String
  queueAdvancesBeforeBlockerReturns : Bool
  deriving Repr

structure ManagedExecLivenessCase where
  name : String
  trigger : String
  preExecState : String
  preToolState : String
  expectedExecState : String
  expectedToolState : String
  maxSteps : Nat
  killSignalRequired : Bool
  deriving Repr

structure LifecycleTransitionCase where
  name : String
  domain : String
  fromState : String
  toState : String
  classification : String
  action : Option String
  boundary : Option String
  deriving Repr

structure QueueDeadlineConformanceCase where
  name : String
  group : String
  action : String
  sessionId : SessionId
  legal : Bool
  preActiveRequestId : Option RequestId
  postActiveRequestId : Option RequestId
  prePendingRequestIds : List RequestId
  postPendingRequestIds : List RequestId
  claimedRequestId : Option RequestId
  blockedByActive : Bool
  supersededRequestIds : List RequestId
  queueKey : Option String
  postCoalescedPendingCount : Nat
  automatedDrainedRequestIds : List RequestId
  preservedUserPendingRequestIds : List RequestId
  postTerminalRequestIds : List RequestId
  preRequestDeadline : Option Time
  synthesizedClaimDeadline : Option Time
  postDeadline : Option Time
  explicitDeadlinePreserved : Bool
  deriving Repr

structure RecoverySweepCase where
  name : String
  sweepId : String
  collection : String
  rustFunction : String
  cadence : String
  implementationStatus : String
  preState : String
  terminalState : String
  measureBefore : Nat
  measureAfter : Nat
  deadlineAuditRef : String
  deriving DecidableEq, Repr

/-- Witness for the outcome/report layer (#693): how many docs a stale session
    carries, how many writes the store accepts, and what the sweep must
    therefore REPORT. `targetSelector` pins the write addressing mode — a
    `session_id` filter matches every duplicate and is refused by DefraDB, so
    the contract demands `_docID`. -/
structure RecoveryOutcomeCase where
  name : String
  sweepId : String
  collection : String
  rustFunction : String
  /-- Docs sharing the session_id (>1 = the #693 duplicate store). -/
  docCount : Nat
  duplicated : Bool
  /-- Whether the store accepts the recovery write for this group. -/
  writeSucceeds : Bool
  /-- What the sweep must report. `recovered` counts SUCCESSES, never attempts. -/
  expectedRecovered : Nat
  expectedFailed : Nat
  /-- Stale docs left behind (0 once recovered; unchanged when the write failed). -/
  measureAfter : Nat
  /-- Recovery must address docs by `_docID`, never by a `session_id` filter. -/
  targetSelector : String
  theoremName : String
  deriving DecidableEq, Repr

structure RecoveryEquivalenceCase where
  name : String
  sourceSweepCase : String
  sweepId : String
  collection : String
  rustFunction : String
  cadence : String
  preState : String
  recoveredState : String
  uninterruptedState : String
  equivalent : Bool
  reexecutes : Bool
  canHang : Bool
  theoremName : String
  aggregateTheoremName : String
  deriving DecidableEq, Repr

structure R6BackgroundingCase where
  name : String
  group : String
  action : String
  legal : Bool
  preLiveCount : Nat
  maxBackgrounded : Nat
  awaitMode : String
  cancelPolicy : String
  childRequestId : Option String
  terminalState : String
  result : Option String
  reason : Option String
  errorCode : Option String
  queueSource : Option String
  queueKey : Option String
  deriving Repr

structure R5CrossDeploymentCase where
  name : String
  route : String
  action : String
  parentDeployment : String
  childDeployment : String
  parentRequestId : String
  parentToolCallId : String
  childRequestId : String
  targetBehaviorId : String
  awaitMode : String
  cancelPolicy : String
  parentTriggerPersisted : Bool
  childMaterialized : Bool
  childOwnedByTargetDeployment : Bool
  causedByParentRequestIdMatches : Bool
  causedByParentToolCallIdMatches : Bool
  causedByTriggerKind : String
  crossDeploymentRoutingFired : Bool
  singleDeploymentFallback : Bool
  unclaimedDeadlineSet : Bool
  deriving Repr

structure CancelPropagationCase where
  name : String
  route : String
  action : String
  parentDeployment : String
  childDeployment : String
  parentRequestId : String
  parentToolCallId : String
  childRequestId : String
  bridgeCollection : String
  childRequestCollection : String
  cancelIntentWrittenOnBridge : Bool
  bridgeCancelReplicatesToHost : Bool
  hostInterruptsChild : Bool
  childTerminalReplicatesToCoordinator : Bool
  cancelAckReturnsToCoordinator : Bool
  noThirdPartyRows : Bool
  deriving Repr

/-- Runtime witness row for an operationally-driven Background Properties theorem. -/
structure BackgroundTheoremWitness where
  theoremName : String
  witnessKind : String
  scenario : String
  numericBound : Nat
  kindFields : List (String × String)
  deriving Repr

/-- Runtime witness row for CrossMachineComposed reachable-state theorem
    domains. These rows are finite projections of proved Lean witnesses; Rust
    consumes them without re-deriving the proof. -/
structure ComposedInvariantWitness where
  theoremName : String
  witnessKind : String
  scenario : String
  rustPath : String
  traceStepCount : Nat
  transitionPath : List String
  preRequestState : String
  preRequestAdmission : String
  toolPreState : String
  toolPostState : String
  requestId : Nat
  toolRequestId : Nat
  toolCallId : Nat
  requestDeadline : Nat
  requestCurrentTime : Nat
  toolDeadline : Nat
  toolCurrentTime : Nat
  deadlineExceeded : Bool
  wellFormedSource : String
  preToolPersisted : Bool
  cancelCause : Option String
  deriving Repr

structure SubagentDelegationGraphCase where
  name : String
  theoremName : String
  property : String
  witnessKind : String
  maxDepth : Nat
  pathLength : Nat
  parentDepth : Nat
  terminalDepth : Nat
  cascadePath : Bool
  acyclic : Bool
  bounded : Bool
  cascadeCovered : Bool
  edgeTheorem : String
  cascadeEdgeTheorem : Option String
  deriving Repr

namespace R4cWitnesses

structure ListSubagentsLineageRejects where
  callerRequestId : String
  siblingRequestId : String
  siblingChildId : String
  callerSeesSiblingChild : Bool
  deriving Repr

structure ReadTranscriptCursorAdvances where
  childSessionId : String
  firstSinceSequence : Nat
  firstThroughSequence : Nat
  firstNextSequence : Nat
  secondSinceSequence : Nat
  secondThroughSequence : Nat
  noGap : Bool
  noOverlap : Bool
  deriving Repr

structure ReadTranscriptHidesBridgeRows where
  childSessionId : String
  bridgeCallId : String
  renderedTranscript : String
  deriving Repr

structure ReadToolOutputDispatchesByState where
  toolCallId : String
  runningSource : String
  terminalSource : String
  runningPayload : String
  staleRunningPayload : String
  terminalPayload : String
  deriving Repr

structure SteerAppendPreservesLineage where
  callerRequestId : String
  childSessionId : String
  queuedRequestId : String
  causedByParentRequestId : String
  queueSource : String
  queuePolicy : String
  deriving Repr

structure SteerInterruptComposes where
  callerRequestId : String
  childSessionId : String
  interruptedActiveRequestId : String
  drainedWakeUpRequestIds : List String
  drainedWakeUpQueueKey : String
  queuedRequestId : String
  queueInterruptedRequestId : String
  deriving Repr

/-- #593: after a successful BACKGROUND spawn receipt the returned
`child_request_id` must never disappear from the parent's control plane,
even while the child `AgentRequest` is not yet materialized (spawn
convergence #377 materializes it asynchronously via `SubagentSource`, and a
cross-deployment child may replicate later or never be claimed).

Boundary note: `awaiting_child_materialization` is a read-side PROJECTION of
(bridge `await_mode = background` ∧ bridge non-terminal ∧ child row absent).
It is never persisted as a bridge `lifecycle_state` and adds no transition
to the Background bridge state machine (`Proofs/Background/Transition.lean`);
once the child materializes, the projection collapses back to the bridge
lifecycle state and the happy path is unchanged. -/
structure UnmaterializedChildVisible where
  callerRequestId : String
  bridgeToolCallId : String
  childRequestId : String
  /-- The child `AgentRequest` row is absent in this scenario. -/
  childMaterialized : Bool
  /-- Persisted bridge state stays `running`; the projection never mutates it. -/
  bridgeLifecycleState : String
  /-- `list_subagents` entry status projected for the missing child. -/
  listedStatus : String
  /-- The handle must be visible under `status="all"`. -/
  listedUnderAllFilter : Bool
  /-- The projection is non-terminal, so the default `running` filter shows it. -/
  listedUnderRunningFilter : Bool
  /-- `read_subagent` reports the projection instead of not-authorized. -/
  readLifecycleState : String
  /-- Never fake a terminal outcome for an unmaterialized child. -/
  readTerminal : Bool
  /-- `wait_subagent` explains the bridge state with a retryable payload. -/
  waitRetryable : Bool
  deriving Repr

end R4cWitnesses

structure TranscriptCase where
  name : String
  group : String
  action : String
  legal : Bool
  preMessageCount : Nat
  postMessageCount : Nat
  preToolCallCount : Nat
  postToolCallCount : Nat
  preInFlightCount : Nat
  postInFlightCount : Nat
  assistantSequence : Nat
  resultSequence : Nat
  logicalResultId : Nat
  payloadHash : Nat
  expectedPairClosed : Bool
  expectedOrdered : Bool
  expectedDuplicateReusedSequence : Bool
  expectedStrongDrain : Bool
  deriving Repr

def boolString (value : Bool) : String :=
  if value then "true" else "false"

def contractBackend : BackendId :=
  { val := "contract-backend" }

def admissionName : AdmissionState → String
  | .released => "released"
  | .waiting => "waiting"
  | .acquired => "acquired"
  | .executing => "executing"

end Conformance.ContractCases
