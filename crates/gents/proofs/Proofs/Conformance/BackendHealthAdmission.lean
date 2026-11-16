import Proofs.Conformance.ContractCases.BoundaryRuntime
import Proofs.Conformance.Family
import Proofs.Runtime.BackendHealth.State

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

structure FirstProbeInput where
  enabled : Bool
  probeStatus : String
  measuredUnhealthy : Bool
  deriving ToJson

structure WaitingOutput where
  waiting : Bool
  deriving ToJson

@[conformance_family]
def firstProbe : Family FirstProbeInput WaitingOutput where
  inputs := [false, true].flatMap fun enabled =>
    Proofs.BackendHealth.HealthState.all.flatMap fun state =>
      [false, true].map fun measuredUnhealthy =>
        { enabled, probeStatus := state.toDefraDB, measuredUnhealthy }
  model := fun i =>
    { waiting := Proofs.BackendHealth.awaitingFirstProbe i.enabled i.probeStatus i.measuredUnhealthy }

structure ProbeRequestInput where
  awaitingProbe : Bool
  startupFailed : Bool
  deriving ToJson

@[conformance_family]
def probeRequest : Family ProbeRequestInput WaitingOutput where
  inputs := [false, true].flatMap fun awaitingProbe =>
    [false, true].map fun startupFailed => { awaitingProbe, startupFailed }
  model := fun i =>
    { waiting := Proofs.BackendHealth.awaitProbeRequest i.awaitingProbe i.startupFailed }

end Conformance.BackendHealthAdmission
