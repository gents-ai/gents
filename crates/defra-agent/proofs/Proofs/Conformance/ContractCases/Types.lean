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
