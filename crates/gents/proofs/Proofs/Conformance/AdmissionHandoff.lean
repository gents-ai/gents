import Proofs.Runtime.CanonicalOutput.Execution.AdmissionHandoffCases
import Proofs.Conformance.Family

namespace Conformance.AdmissionHandoff

open Lean CanonicalOutput.Execution.AdmissionHandoff CanonicalOutput.Execution.AdmissionHandoff.Cases

def decisionName : Decision → String
  | .run => "run" | .skip => "skip" | .terminate => "terminate"

def phaseName : Phase → String
  | .initial => "initial" | .waiting => "waiting" | .permitted => "permitted" | .closed => "closed"

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

structure DriveOutput where
  effect : String
  invoked : Bool
  deriving ToJson

/-- `drive_admissions` hands each hook reply to the waiting loop over a typed
oneshot: a stand-in loop can stage a delivered reply, a stop before or after
the reply, a closed reply channel and a skip that keeps a hook effect. Dispatch
linearization, lease and generation fencing and misaddressed replies have no
representation in that handoff. -/
def driveNames : List String :=
  ["accepted-before-invoke", "cancel-after-reply", "cancel-waiting-unknown",
   "lease-lost-waiting", "shutdown-waiting", "deadline-waiting", "dropped-channel",
   "skip-with-hook-effect"]

@[conformance_family]
def driveAdmissions : Family HandoffInput DriveOutput where
  inputs := inputsNamed driveNames
  model := fun i => match observe i.name with
    | some result => ⟨observationName result.effectObservation, result.invoked⟩
    | none => ⟨"", false⟩

/-- The owned loop scenario holds a hook inside admission and drops the loop:
only a stop while the reply is pending is reachable there. -/
@[conformance_family]
def ownedLoop : Family HandoffInput Bool where
  inputs := inputsNamed ["cancel-waiting-unknown"]
  model := fun i => ((observe i.name).map (·.invoked)).getD false

structure Observed where
  phase : String
  replies : Nat
  decision : Option String
  dispatch : String
  nativeDispatchCommitted : Option Bool
  effect : String
  invoked : Bool
  author : Option String
  deriving ToJson

/-- model-production-gap: production's typed oneshot handoff has no dispatch
linearization, lease or generation fence, duplicate or misaddressed reply,
unpublished header or terminate decision to stage, so these cases are replayed
by no production scenario. -/
@[conformance_family]
def unreached : Family HandoffInput Observed where
  inputs := (cases.filter fun c => !(driveNames.contains c.name)).map fun c => ⟨c.name, c.published⟩
  model := fun i => match observe i.name with
    | some result =>
      { phase := phaseName result.phase
        replies := result.acceptedReplies
        decision := result.decision.map decisionName
        dispatch := observationName result.dispatchObservation
        nativeDispatchCommitted := result.nativeDispatchCommitted
        effect := observationName result.effectObservation
        invoked := result.invoked
        author := author result }
    | none => ⟨"", 0, none, "", none, "", false, none⟩

example : ((observe "cancel-waiting-unknown").map fun r => observationName r.dispatchObservation) =
    some "unknown" := by native_decide

end Conformance.AdmissionHandoff
