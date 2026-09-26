import Proofs.Basic
import Proofs.Scheduling
import Proofs.RuntimeReconcile.State
import Proofs.CanonicalOutput.State

namespace Conformance.ContractCases

structure RuntimeReconcileCase where
  requestedBehavior : Option BehaviorId
  preDefaultBehavior : BehaviorId
  preSessionBehavior : Option BehaviorId
  preRunnable : List BehaviorId
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

structure ClientBehaviorReadinessCase where
  name : String
  observationPresent : Bool
  observationKind : String
  processState : String
  activeGeneration : Generation
  routerGeneration : Generation
  runnable : Bool
  unavailable : Bool
  startupDemoted : Bool
  runtimeUnavailableReason : String
  expectedState : String
  expectedReason : Option String
  expectedRuntimeAdmissible : Bool
  deriving Repr

structure EnrollmentTraceStep where
  action : String
  peerAdmissionDid : String
  offerId : String
  offerChallenge : String
  offerNetworkId : String
  offerAdminDid : String
  offerServerPeer : String
  offerOwnerAgent : String
  offerProfile : String
  challenge : String
  requestId : String
  requestDigest : String
  requestOfferId : String
  networkId : String
  adminDid : String
  serverPeer : String
  serverTicketPeer : String
  resolvedServerDid : String
  profile : String
  schemaCompatible : Bool
  offerAdminSigned : Bool
  offerFresh : Bool
  candidateDid : String
  candidatePeer : String
  observedCandidatePeer : String
  resolvedCandidateDid : String
  candidateTicketPeer : String
  ownerAgent : String
  clientNonce : String
  issuedAt : String
  expiresAt : String
  candidateSigned : Bool
  requestFresh : Bool
  decisionAuthorizationSequence : Nat
  decisionAuthorizationExpiresAt : String
  decisionSignerDid : String
  decisionKind : String
  decisionRequestId : String
  decisionRequestDigest : String
  decisionNetworkId : String
  decisionAdminDid : String
  decisionCandidateDid : String
  decisionCandidatePeer : String
  decisionOwnerAgent : String
  decisionAdminSigned : Bool
  decisionFresh : Bool
  revisionKind : String
  revisionSequence : Nat
  revisionAuthorizationExpiresAt : String
  revisionSignerDid : String
  revisionRequestId : String
  revisionRequestDigest : String
  revisionNetworkId : String
  revisionAdminDid : String
  revisionMemberDid : String
  revisionMemberPeer : String
  revisionOwnerAgent : String
  revisionAdminSigned : Bool
  receiptRequestId : String
  receiptRequestDigest : String
  receiptNetworkId : String
  receiptAdminDid : String
  receiptMemberDid : String
  receiptMemberPeer : String
  receiptServerPeer : String
  receiptOwnerAgent : String
  receiptAuthorizationSequence : Nat
  receiptAuthorizationExpiresAt : String
  receiptDirection : String
  receiptSignerDid : String
  receiptAdminSigned : Bool
  receiptApplied : Bool
  observedOfferCount : Nat
  adminPinCount : Nat
  challengeBindingCount : Nat
  requestBindingCount : Nat
  requestCount : Nat
  decisionCount : Nat
  authorizationCount : Nat
  membershipCount : Nat
  receiptCount : Nat
  routeCount : Nat
  requestAccepted : Bool
  decisionRecorded : Bool
  authorizationRecorded : Bool
  revisionRecorded : Bool
  receiptRecorded : Bool
  membershipPresent : Bool
  clientRoutePresent : Bool
  serverRoutePresent : Bool
  adminPinPresent : Bool
  adminPinConflict : Bool
  challengeBindingConflict : Bool
  requestBindingConflict : Bool
  currentApproval : Bool
  peerAdmitted : Bool
  ready : Bool
  clientHydrationAdmits : Bool
  serverHydrationAdmits : Bool
  deriving Repr

structure EnrollmentCase where
  name : String
  steps : List EnrollmentTraceStep
  deriving Repr

structure EnrollmentDurableProjectionCase where
  name : String
  documents : List EnrollmentTraceStep
  expectedCurrentApproval : Bool
  expectedCurrentRouteReceipt : Bool
  deriving Repr

structure EnrollmentEncodingCase where
  name : String
  value : String
  expectedFrame : String
  actualFrame : String
  frameMatches : Bool
  deriving Repr

structure EnrollmentDigestCase where
  name : String
  fields : List String
  expectedPayload : String
  actualPayload : String
  expectedDigest : String
  actualDigest : String
  payloadMatches : Bool
  digestMatches : Bool
  deriving Repr

structure AgentRequestAdmissionCase where
  name : String
  observationAvailable : Bool
  kind : String
  signatureValid : Bool
  signedFieldsMatch : Bool
  branchFieldsExact : Bool
  pendingDeadlineAbsent : Bool
  signerMatchesRequester : Bool
  requesterMatchesTarget : Bool
  signerMatchesTarget : Bool
  signerMatchesIssuer : Bool
  requesterMatchesIssuer : Bool
  currentApproval : Bool
  exactGeneration : Bool
  authorizationFresh : Bool
  runtimeEvidencePresent : Bool
  runtimeSourceKind : String
  targetRuntimeAttestationValid : Bool
  sourceBindingCurrent : Bool
  triggerConfigDocumentBindingCurrent : Bool
  sourceDocumentBindingCurrent : Bool
  targetPolicyAllows : Bool
  peerAuthorityAllows : Bool
  hopWithinBound : Bool
  expectedAdmitted : Bool
  expectedDisposition : String
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

structure ManagedExecToolBoundaryCase where
  name : String
  toolName : String
  workClass : String
  boundary : String
  killScope : String
  timeoutRequiresKill : Bool
  cancelRequiresKill : Bool
  descendantsInTerminationScope : Bool
  captureDrainBounded : Bool
  deriving Repr

structure PairingReconcileShutdownBoundaryCase where
  name : String
  supervisor : String
  workClass : String
  boundary : String
  perAdminCallTimeoutMs : Nat
  cancellationObservedInsideSweep : Bool
  currentAdminFutureDropped : Bool
  remainingPeersSkipped : Bool
  shutdownJoinBounded : Bool
  deriving Repr

structure PairingReconcileSweepRetryBoundaryCase where
  name : String
  supervisor : String
  workClass : String
  boundary : String
  failureScope : String
  failureTerminal : Bool
  retryTrigger : String
  cancellationPrioritized : Bool
  convergenceRetried : Bool
  deriving Repr

structure PairingReconcileSweepSchedulingCase where
  name : String
  supervisor : String
  workClass : String
  boundary : String
  maxConcurrentPeerPreparations : Nat
  peerPreparationBounded : Bool
  topologyMutationSerialized : Bool
  stalePeerBlocksReadyPeer : Bool
  everyPeerResultAccounted : Bool
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

/-- Host stop verdict witness: the owner's observation before and
    after its signal, and the verdict, signal admissibility and cancel reply
    computed by `ManagedExec`. -/
structure ProcessStopCase where
  name : String
  before : String
  after : String
  mayTerminate : Bool
  outcome : String
  cancelReply : String
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
  preservedForeignRequesterRequestIds : List RequestId := []
  preservedForeignOwnerRequestIds : List RequestId := []
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
  deadlineExpired : Option Bool := none
  parentLive : Option Bool := none
  parentInterrupted : Option Bool := none
  parentTerminal : Option Bool := none
  executionRegistered : Option Bool := none
  processOutcome : Option String := none
  ownerTaskDeleted : Option Bool := none
  recoveryCause : Option String := none
  notificationReason : Option String := none
  deriving DecidableEq, Repr

/-- Startup restart-disposition witness (#937): one running `AgentToolCall`
    row shape and what `ToolCallLifecycle::recover_all` must do with it —
    terminalize with a pinned cause/terminal state, or leave it running.
    `disposition`, `cause`, `terminalState`, and the notification/wake fields
    are computed from `Recovery.restartDisposition`, never hand-written. -/
structure RestartDispositionCase where
  name : String
  rustFunction : String
  awaitMode : String
  sessionMessage : Bool
  parentObservation : String
  deadlineExpired : Bool
  processOutcome : String
  disposition : String
  cause : Option String
  terminalState : Option String
  notificationReason : Option String
  queueSource : Option String
  queueKeyPrefix : Option String
  theoremName : String
  deriving DecidableEq, Repr

/-- Interrupt disposition witness: one owned tool-call shape and the
    disposition and post-state computed by `Background.Interrupt.interruptTool`. -/
structure InterruptDispositionCase where
  name : String
  state : String
  awaitMode : String
  disposition : String
  postState : String
  postAwaitMode : String
  deriving DecidableEq, Repr

/-- Paging witness over the retained output window (#937): inputs plus the
    slice outputs, computed from `Background.ToolOutput.readSlice` — never
    hand-written. Consumed by the `background_tools` unit test against
    `read_retained_output_slice`. -/
structure ToolOutputPagingCase where
  name : String
  firstOffset : Nat
  retainedLen : Nat
  totalBytes : Nat
  offset : Nat
  maxBytes : Nat
  start : Nat
  sliceLen : Nat
  nextOffset : Nat
  firstAvailableOffset : Nat
  totalBytesOut : Nat
  hasMore : Bool
  theoremName : String
  deriving DecidableEq, Repr

structure R6BackgroundingCase where
  name : String
  group : String
  action : String
  legal : Bool
  preLiveCount : Nat
  maxBackgrounded : Nat
  awaitMode : String
  terminalState : String
  result : Option String
  reason : Option String
  errorCode : Option String
  queueSource : Option String
  queueKey : Option String
  retryCount : Option Nat := none
  maxRetries : Option Nat := none
  postRetryCount : Option Nat := none
  redriveSourceRequestId : Option Nat := none
  preDepth : Option Nat := none
  postDepth : Option Nat := none
  preParentRequestId : Option Nat := none
  postParentRequestId : Option Nat := none
  preExecutionDeadline : Option Nat := none
  postExecutionDeadline : Option Nat := none
  retryDelaySeconds : Option Nat := none
  isLatest : Option Bool := none
  goalStatus : Option String := none
  notificationPersisted : Option Bool := none
  wakeCreated : Option Bool := none
  redriveAllowed : Option Bool := none
  deriving Repr

structure BackgroundTheoremWitness where
  theoremName : String
  witnessKind : String
  scenario : String
  numericBound : Nat
  kindFields : List (String × String)
  deriving Repr

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

namespace R4cWitnesses

/-- One executable canonical-tool-source projection input and the result
    computed by `Background.ToolOutput.project`.  The native adapter supplies the
    fixed accepted physical tool binding and persists these exact segment
    facts; it does not infer output from flags. -/
structure ToolOutputProjectionCase where
  name : String
  document : Nat
  segments : List CanonicalOutput.Segment
  expectedState : Option String
  expectedPayload : Option (List UInt8)
  deriving Repr

/-- Canonical `read_tool_output`: open and closed reads use the same immutable
    physical tool source; missing/conflicting facts are rejection. -/
structure ReadToolOutputCanonicalSourceReconstruction where
  toolCallId : String
  canonicalSource : String
  cases : List ToolOutputProjectionCase
  deriving Repr

end R4cWitnesses

structure TranscriptCase where
  name : String
  group : String
  action : String
  actionCallIds : List Nat
  actionLogicalResultIds : List Nat
  actionPayloadHashes : List Nat
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
