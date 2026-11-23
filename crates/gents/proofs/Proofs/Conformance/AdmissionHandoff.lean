import Proofs.Runtime.CanonicalOutput.Execution.AdmissionHandoffCases
import Proofs.Conformance.Family

namespace Conformance.AdmissionHandoff

open Lean CanonicalOutput.Execution.AdmissionHandoff CanonicalOutput.Execution.AdmissionHandoff.Cases

def observationName : Observation → String
  | .notStarted => "not_started" | .unknown => "unknown"
  | .committed => "committed" | .rejected => "rejected"

structure HandoffInput where
  name : String
  published : Bool
  deriving ToJson

def observe (name : String) : Option State :=
  (cases.find? (·.name == name)).map fun c => run (initial c.published) c.actions

def inputsNamed (names : List String) : List HandoffInput :=
  (cases.filter fun c => names.contains c.name).map fun c => ⟨c.name, c.published⟩

/-- Whether the stand-in loop invokes the guest after the handoff. -/
@[conformance_family]
def driveAdmissions : Family HandoffInput Bool where
  inputs := cases.map fun c => ⟨c.name, c.published⟩
  model := fun i => ((observe i.name).map (·.invoked)).getD false

/-- Only the skip scenario's hook performs an effect before it replies; the
handoff keeps it. -/
@[conformance_family]
def hookEffect : Family HandoffInput String where
  inputs := inputsNamed ["skip-with-hook-effect"]
  model := fun i => ((observe i.name).map (observationName ·.effectObservation)).getD ""

/-- The owned loop scenario holds a hook inside admission and drops the loop:
only a stop while the reply is pending is reachable there. -/
@[conformance_family]
def ownedLoop : Family HandoffInput Bool where
  inputs := inputsNamed ["cancel-waiting-unknown"]
  model := fun i => ((observe i.name).map (·.invoked)).getD false

example : ((observe "cancel-waiting-unknown").map fun r => observationName r.dispatchObservation) =
    some "unknown" := by native_decide

end Conformance.AdmissionHandoff
