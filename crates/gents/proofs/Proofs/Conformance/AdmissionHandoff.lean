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

/-- `drive_admissions` hands each hook reply to the waiting loop over a typed
oneshot: a stand-in loop can stage a delivered reply (with or without elapsed
time before the invocation), a stop before or after the reply, a closed reply
channel, a skip that keeps a hook effect and a terminate reply. The stop reason
and the lease clock have no representation in that handoff, so every stop
before or after the reply is staged by the same stop. Dispatch linearization,
lease and generation fencing and misaddressed replies have no representation
there either. -/
def driveNames : List String :=
  ["accepted-before-invoke", "lease-expires-after-reply-without-observed-stop",
   "cancel-after-reply", "lease-lost-after-reply", "shutdown-after-reply",
   "deadline-after-reply", "cancel-waiting-unknown", "lease-lost-waiting",
   "shutdown-waiting", "deadline-waiting", "dropped-channel", "skip-with-hook-effect",
   "terminate-after-dispatch"]

/-- Whether the stand-in loop invokes the guest after the handoff. -/
@[conformance_family]
def driveAdmissions : Family HandoffInput Bool where
  inputs := inputsNamed driveNames
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
unpublished header, invocation without a reply or effect observed after a stop
to stage, so these cases are replayed by no production scenario. -/
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
