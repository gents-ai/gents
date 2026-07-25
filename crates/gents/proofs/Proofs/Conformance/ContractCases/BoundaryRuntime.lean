import Proofs.Persistence
import Proofs.StorageObservation
import Proofs.Conformance.ContractCases.Types

/-!
# Boundary Runtime Witness Cases

Finite executable cases for service-local decisions that sit next to external
boundaries. These rows deliberately classify only Rust-owned policy outcomes;
they do not claim DefraDB durability, event delivery, endpoint freshness, or
provider/network behavior.
-/

namespace Conformance.ContractCases

inductive MutationResult where
  | success
  | failure
  deriving DecidableEq, Repr

namespace MutationResult

def toContract : MutationResult -> String
  | .success => "success"
  | .failure => "failure"

end MutationResult

inductive BackendProbeStatus where
  | healthy
  | unhealthy
  | unknown
  | stale
  | rateLimited
  | circuitOpen
  deriving DecidableEq, Repr

namespace BackendProbeStatus

def toDefraDB : BackendProbeStatus -> String
  | .healthy => "healthy"
  | .unhealthy => "unhealthy"
  | .unknown => "unknown"
  | .stale => "stale"
  | .rateLimited => "rate_limited"
  | .circuitOpen => "circuit_open"

end BackendProbeStatus

def hookDecisionForFailure (policy : PersistenceState.FailurePolicy) : String :=
  match policy with
  | .failOpen => "continue"
  | .failClosed => "terminate"

def hookResultForMutation
    (policy : PersistenceState.FailurePolicy)
    (result : MutationResult) : String :=
  match result with
  | .success => "ok"
  | .failure =>
      match policy with
      | .failOpen => "ok"
      | .failClosed => "err"

def actionNameForMutationResult : MutationResult -> String
  | .success => "mutationSuccess"
  | .failure => "mutationFailure"

def storageObservationForMutation
    (policy : PersistenceState.FailurePolicy)
    (result : MutationResult) : StorageObservation :=
  match result with
  | .success => .successAcknowledged
  | .failure =>
      match policy with
      | .failOpen => .lostAcknowledged
      | .failClosed => .mutationFailed

def terminalWriteObservedBool (obs : StorageObservation) : Bool :=
  match obs with
  | .successAcknowledged => true
  | .readVisible => true
  | _ => false

def persistenceFailurePolicyCase
    (name : String)
    (policy : PersistenceState.FailurePolicy) :
    PersistenceFailurePolicyCase :=
  let postPersistence :=
    match PersistenceState.step? policy .committing .writeFail with
    | some state => state
    | none => .uncommitted
  let postObservation := storageObservationForMutation policy .failure
  { name := name
  , policy := policy.toDefraDB
  , action := "writeFail"
  , prePersistence := PersistenceState.toDefraDB .committing
  , postPersistence := postPersistence.toDefraDB
  , postStorageObservation := postObservation.toContract
  , hookDecision := hookDecisionForFailure policy
  , recordsFailure := true
  , recordsSuccess := false
  , externalDurabilityClaimed := false
  }

def persistenceFailurePolicyCases : List PersistenceFailurePolicyCase :=
  [ persistenceFailurePolicyCase
      "fail_closed_write_failure_terminates_without_success_ack"
      .failClosed
  , persistenceFailurePolicyCase
      "fail_open_write_failure_continues_as_lost_without_success_ack"
      .failOpen
  ]

def storageObservationRuntimeCase
    (name : String)
    (policy : PersistenceState.FailurePolicy)
    (result : MutationResult) : StorageObservationRuntimeCase :=
  let postObservation := storageObservationForMutation policy result
  { name := name
  , policy := policy.toDefraDB
  , action := actionNameForMutationResult result
  , preObservation := StorageObservation.toContract .inFlight
  , mutationResult := result.toContract
  , postObservation := postObservation.toContract
  , postPersistence := (StorageObservation.toPersistence postObservation).toDefraDB
  , hookResult := hookResultForMutation policy result
  , recordsFailure := decide (result = .failure)
  , recordsSuccess := decide (result = .success)
  , terminalWriteObserved := terminalWriteObservedBool postObservation
  , externalVisibilityClaimed := false
  }

def storageVisibilityRuntimeCase
    (name : String)
    (policy : PersistenceState.FailurePolicy)
    (preObservation : StorageObservation)
    (actionName : String)
    (action : StorageObservation.Action) : StorageObservationRuntimeCase :=
  let postObservation :=
    match StorageObservation.step? policy preObservation action with
    | some obs => obs
    | none => preObservation
  { name := name
  , policy := policy.toDefraDB
  , action := actionName
  , preObservation := preObservation.toContract
  , mutationResult := "notApplicable"
  , postObservation := postObservation.toContract
  , postPersistence := (StorageObservation.toPersistence postObservation).toDefraDB
  , hookResult := "notApplicable"
  , recordsFailure := false
  , recordsSuccess := false
  , terminalWriteObserved := terminalWriteObservedBool postObservation
  , externalVisibilityClaimed := false
  }

def storageObservationRuntimeCases : List StorageObservationRuntimeCase :=
  [ storageObservationRuntimeCase
      "fail_closed_success_ack_counts_local_commit"
      .failClosed
      .success
  , storageObservationRuntimeCase
      "fail_open_success_ack_counts_local_commit"
      .failOpen
      .success
  , storageObservationRuntimeCase
      "fail_closed_failure_reports_mutation_failed"
      .failClosed
      .failure
  , storageObservationRuntimeCase
      "fail_open_failure_acknowledges_lost_output"
      .failOpen
      .failure
  , storageVisibilityRuntimeCase
      "read_your_writes_success_ack_is_visible"
      .failClosed
      .successAcknowledged
      "readYourWrites"
      .readYourWrites
  , storageVisibilityRuntimeCase
      "stale_read_preserves_committed_observation"
      .failClosed
      .successAcknowledged
      "staleRead"
      .staleRead
  , storageVisibilityRuntimeCase
      "stale_event_preserves_committed_observation"
      .failClosed
      .successAcknowledged
      "staleEvent"
      .staleEvent
  , storageVisibilityRuntimeCase
      "event_after_stale_observation_is_visible"
      .failClosed
      .staleObserved
      "eventArrives"
      .eventArrives
  ]

def backendAvailable (enabled : Bool) (probeStatus : BackendProbeStatus) : Bool :=
  enabled && decide (probeStatus = .healthy)

def backendHealthAdmissionCase
    (name : String)
    (enabled : Bool)
    (probeStatus : BackendProbeStatus) : BackendHealthAdmissionCase :=
  let expectedAvailable := backendAvailable enabled probeStatus
  { name := name
  , enabled := enabled
  , probeStatus := probeStatus.toDefraDB
  , expectedAvailable := expectedAvailable
  , admissionDecision := if expectedAvailable then "available" else "unavailable"
  , observedDocumentOnly := true
  , externalEndpointFreshnessClaimed := false
  }

def backendHealthAdmissionCases : List BackendHealthAdmissionCase :=
  [ backendHealthAdmissionCase
      "enabled_healthy_backend_is_available_from_observed_document"
      true
      .healthy
  , backendHealthAdmissionCase
      "disabled_healthy_backend_is_unavailable_from_observed_document"
      false
      .healthy
  , backendHealthAdmissionCase
      "enabled_unhealthy_backend_is_unavailable_from_observed_document"
      true
      .unhealthy
  , backendHealthAdmissionCase
      "enabled_unknown_backend_is_unavailable_from_observed_document"
      true
      .unknown
  , backendHealthAdmissionCase
      "enabled_stale_backend_is_unavailable_from_observed_document"
      true
      .stale
  , backendHealthAdmissionCase
      "enabled_rate_limited_backend_is_unavailable_from_observed_document"
      true
      .rateLimited
  , backendHealthAdmissionCase
      "enabled_circuit_open_backend_is_unavailable_from_observed_document"
      true
      .circuitOpen
  ]

def nativeFilesystemBoundaryCase (toolName : String) : NativeFilesystemBoundaryCase :=
  { name := toolName ++ "_single_poll_blocker_times_out_and_queue_advances"
  , toolName := toolName
  , workClass := "filesystemTraversal"
  , boundary := "managedExecProcessGroupBoundary"
  , innerPollBlocks := true
  , requestDeadlineMs := 15
  , blockerMs := 200
  , expectedTerminal := "timedOut"
  , expectedFailureClass := some "external"
  , queueAdvancesBeforeBlockerReturns := true
  }

def nativeFilesystemBoundaryCases : List NativeFilesystemBoundaryCase :=
  [ nativeFilesystemBoundaryCase "list_files"
  , nativeFilesystemBoundaryCase "glob"
  , nativeFilesystemBoundaryCase "grep"
  ]

def managedExecToolBoundaryCase
    (toolName workClass : String) : ManagedExecToolBoundaryCase :=
  { name := toolName ++ "_routes_through_managed_exec_process_tree_boundary"
  , toolName := toolName
  , workClass := workClass
  , boundary := "managedExecProcessGroupBoundary"
  , killScope := "processTree"
  , timeoutRequiresKill := true
  , cancelRequiresKill := true
  , descendantsInTerminationScope := true
  , captureDrainBounded := true
  }

/-- Every native tool that can launch an OS subprocess must consume the managed
    executor boundary. Keeping this inventory in the Lean-owned contract makes
    a raw `Command::spawn` route a conformance failure instead of an implicit
    implementation convention. -/
def managedExecToolBoundaryCases : List ManagedExecToolBoundaryCase :=
  [ managedExecToolBoundaryCase "list_files" "filesystemTraversal"
  , managedExecToolBoundaryCase "glob" "filesystemTraversal"
  , managedExecToolBoundaryCase "grep" "filesystemTraversal"
  , managedExecToolBoundaryCase "bash" "shellCommand"
  , managedExecToolBoundaryCase "bash_unrestricted" "shellCommand"
  ]

/-- `PairingReconcile.Transition.crash` abstracts away the implementation's
    async supervisor boundary. This witness fences that refinement: shutdown
    must be observable while a sweep is awaiting remote admin work, must drop
    the current wait and the rest of the peer loop, and must therefore let the
    runtime's background-task join finish without multiplying the per-RPC
    timeout by the number of stale peers. -/
def pairingReconcileShutdownBoundaryCases :
    List PairingReconcileShutdownBoundaryCase :=
  [ { name := "shutdown_preempts_in_flight_pairing_reconcile_sweep"
    , supervisor := "pairingReconciler"
    , workClass := "p2pReconcileSweep"
    , boundary := "pairingReconcileSupervisorBoundary"
    , perAdminCallTimeoutMs := 10000
    , cancellationObservedInsideSweep := true
    , currentAdminFutureDropped := true
    , remainingPeersSkipped := true
    , shutdownJoinBounded := true
    }
  ]

/-- The single-peer `PairingReconcile` machine does not model how a runtime
    sweep schedules several independent peers. This witness fences that
    refinement: slow discovery/dial preparation is bounded and concurrent, so
    one stale peer cannot head-of-line block a ready phone, while topology
    mutations remain serialized and every peer still updates supervisor
    accounting exactly once. -/
def pairingReconcileSweepSchedulingCases :
    List PairingReconcileSweepSchedulingCase :=
  [ { name := "stale_peer_dial_does_not_block_ready_peer"
    , supervisor := "pairingReconciler"
    , workClass := "p2pReconcileSweep"
    , boundary := "pairingReconcilePeerPreparationBoundary"
    , maxConcurrentPeerPreparations := 8
    , peerPreparationBounded := true
    , topologyMutationSerialized := true
    , stalePeerBlocksReadyPeer := false
    , everyPeerResultAccounted := true
    }
  ]

end Conformance.ContractCases
