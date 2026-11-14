import Proofs.Inference.InferenceCall.DroppedPermit
import Proofs.Inference.InferenceCall.CallKinds
import Proofs.Conformance.Family

namespace Conformance.InferenceDroppedPermit
open InferenceCall.DroppedPermit

deriving instance Lean.ToJson for DropOutcome, RecordedReason

structure DropInput where
  cancelled : Bool
  observed : Option String
  deriving Lean.ToJson

structure DropOutput where
  outcome : DropOutcome
  callState : String
  failureReason : RecordedReason
  slotsWhileHeld : Nat
  slotsAfterDrop : Nat
  deriving Lean.ToJson

@[conformance_family]
def outcomes : Conformance.Family DropInput DropOutput where
  inputs :=
    [ { cancelled := true, observed := none }
    , { cancelled := true, observed := some "stream liveness timeout: no data received for 1800s" }
    , { cancelled := false, observed := none }
    , { cancelled := false, observed := some "stream liveness timeout: no data received for 1800s" }
    ]
  model := fun i =>
    let dropped := outcome i.cancelled i.observed
    { outcome := dropped
      callState := (recordedState dropped).toDefraDB
      failureReason := recordedReason dropped
      slotsWhileHeld := slotsHeld .running
      slotsAfterDrop := slotsHeld (recordedState dropped) }

namespace Kinds
open InferenceCall.CallKinds

deriving instance Lean.ToJson for CallKind

structure CallsInput where
  capacity : Nat
  queueDepth : Nat
  kinds : List CallKind
  deriving Lean.ToJson

structure HeldRow where
  kind : String
  callState : String
  deriving Lean.ToJson

structure FinishedRow where
  kind : String
  callState : String
  queueDepthAtEnqueue : Nat
  deriving Lean.ToJson

structure CallsOutput where
  held : List HeldRow
  slotsWhileHeld : Nat
  finished : List FinishedRow
  slotsAfterFinish : Nat
  deriving Lean.ToJson

def allKinds : List CallKind := [.inference, .compaction, .scheduled, .oneOff]

/-- Calls on one backend: each asks for a slot while every earlier call still
holds or waits, then every call finishes in order. -/
@[conformance_family]
def calls : Conformance.Family CallsInput CallsOutput where
  inputs :=
    (allKinds.map fun kind => { capacity := 1, queueDepth := 1, kinds := [kind] }) ++
    ([1, 2].flatMap fun capacity => allKinds.flatMap fun first => allKinds.map fun second =>
      { capacity, queueDepth := 1, kinds := [first, second] })
  model := fun i =>
    let placed := place (pool i.capacity i.queueDepth) i.kinds
    { held := placed.map fun p => { kind := p.kind.toDefraDB, callState := p.state.toDefraDB }
      slotsWhileHeld := slotsHeld (placed.map (·.state))
      finished := placed.map fun p =>
        { kind := p.kind.toDefraDB, callState := p.finished.toDefraDB
          queueDepthAtEnqueue := p.queueDepthAtEnqueue }
      slotsAfterFinish := slotsHeld (placed.map (·.finished)) }

end Kinds

end Conformance.InferenceDroppedPermit
