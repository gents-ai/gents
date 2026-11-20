import Proofs.Store.Persistence
import Proofs.Store.StorageObservation
import Proofs.Conformance.ContractCases.Types

namespace Conformance.ContractCases

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
  }

def nativeFilesystemBoundaryCases : List NativeFilesystemBoundaryCase :=
  [ nativeFilesystemBoundaryCase "list_files"
  , nativeFilesystemBoundaryCase "glob"
  , nativeFilesystemBoundaryCase "grep"
  ]

end Conformance.ContractCases
