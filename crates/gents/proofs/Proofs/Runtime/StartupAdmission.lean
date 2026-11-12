import Lean

namespace Runtime.StartupAdmission
open Lean

structure DemoteInput where
  pending : Bool
  registered : Bool
  durable : Bool
  deriving ToJson

structure DemoteObservation where
  applied : Option Bool
  pending : Bool
  gateOpen : Bool
  cancelled : Bool
  fatal : Option String
  committed : Bool
  deriving ToJson

/-- An exhausted Agent can release startup only after its demotion is durable.
Failure closes admission and hands the owning runtime the persistence failure
itself as the reason to stop. A stale generation
cannot change the current obligation or claim a committed demotion. -/
def demote (input : DemoteInput) : DemoteObservation :=
  if !input.pending || !input.registered then
    ⟨some false, input.pending, true, false, none, false⟩
  else if !input.durable then ⟨none, true, false, true, some "readinessWrite", false⟩
  else ⟨some true, false, true, false, none, true⟩

structure ReadyInput where
  closing : Bool
  retryFirst : Bool
  deriving ToJson

structure ReadyObservation where
  pendingBeforeWrite : Bool
  gateBeforeWrite : Bool
  applied : Bool
  gateAfterWrite : Bool
  process : String
  notifications : List String
  deriving ToJson

/-- The durable Ready acknowledgement is the gate-opening boundary. Once
shutdown owns the coordinator, a late startup completion cannot reopen it. -/
def ready (input : ReadyInput) : ReadyObservation :=
  if input.closing then ⟨false, false, false, false, "shuttingDown", ["shuttingDown"]⟩
  else ⟨true, false, true, true, "ready", ["ready"]⟩

end Runtime.StartupAdmission
