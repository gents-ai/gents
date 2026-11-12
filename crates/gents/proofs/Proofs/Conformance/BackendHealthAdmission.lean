import Proofs.Conformance.ContractCases.BoundaryRuntime
import Proofs.Conformance.Family

namespace Conformance.BackendHealthAdmission

open Lean Conformance.ContractCases

deriving instance ToJson for BackendProbeStatus

structure ObservedInput where
  enabled : Bool
  probeStatus : BackendProbeStatus
  deriving ToJson

structure AdmissionOutput where
  available : Bool
  deriving ToJson

structure DisplayOutput where
  available : Bool
  deriving ToJson

def observedInputs : List ObservedInput :=
  [false, true].flatMap fun enabled =>
    [BackendProbeStatus.healthy, .unhealthy, .unknown, .stale, .rateLimited, .circuitOpen].map
      fun probeStatus => { enabled, probeStatus }

@[conformance_family]
def admission : Family ObservedInput AdmissionOutput where
  inputs := observedInputs
  model := fun i => { available := backendAvailable i.enabled i.probeStatus }

@[conformance_family]
def display : Family ObservedInput DisplayOutput where
  inputs := observedInputs
  model := fun i => { available := backendAvailable i.enabled i.probeStatus }

example : backendHealthAdmissionCases.all fun c =>
    c.admissionDecision == if c.expectedAvailable then "available" else "unavailable" := by
  native_decide

example : backendHealthAdmissionCases.all (·.observedDocumentOnly) := by decide

example : backendHealthAdmissionCases.all (!·.externalEndpointFreshnessClaimed) := by decide

end Conformance.BackendHealthAdmission
