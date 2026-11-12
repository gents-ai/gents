import Proofs.Store.StorageWriteGate
import Proofs.Conformance.Family

namespace Conformance.StorageWriteGateContracts
open StorageWriteGate

structure Case where
  name : String
  independentlyScheduled : Bool
  storageReturns : Bool
  rollbackOwnerScheduled : Bool := false
  cleanupTimesOut : Bool := false

def cases : List Case :=
  [ ⟨"shared_task_completion_suspended", false, true, false, false⟩
  , ⟨"independent_completion", true, true, false, false⟩
  , ⟨"elapsed_not_completion", true, false, false, false⟩
  , ⟨"cancelled_rollback_timeout_releases_canonical_gate", false, false, true, true⟩ ]

def finalState (c : Case) : State :=
  if c.rollbackOwnerScheduled then
    run ⟨.storage, c.independentlyScheduled, true⟩
      (if c.cleanupTimesOut then
        [.rollbackOwnerScheduled, .cleanupDeadlineObserved, .dropReturned, .release]
       else [.rollbackOwnerScheduled, .cleanupReturned, .release])
  else
    run ⟨.storage, c.independentlyScheduled, false⟩
      (if c.storageReturns then [.siblingWait, .storageReturned, .release]
       else [.siblingWait, .elapsed])

def gateHeldAfterOwnerHandoff (c : Case) : Bool :=
  held (if c.rollbackOwnerScheduled then
    step ⟨.storage, c.independentlyScheduled, true⟩ .rollbackOwnerScheduled
  else ⟨.storage, c.independentlyScheduled, false⟩)

structure HandoffInput where
  name : String
  independentlyScheduled : Bool
  cleanupTimesOut : Bool
  deriving Lean.ToJson

structure HandoffOutput where
  gateHeldAfterOwnerHandoff : Bool
  gateHeld : Bool
  deriving Lean.ToJson

@[conformance_family]
def handoff : Conformance.Family HandoffInput HandoffOutput where
  inputs := (cases.filter fun c => c.rollbackOwnerScheduled).map fun c =>
    { name := c.name
      independentlyScheduled := c.independentlyScheduled
      cleanupTimesOut := c.cleanupTimesOut }
  model := fun i =>
    let c : Case := ⟨i.name, i.independentlyScheduled, false, true, i.cleanupTimesOut⟩
    { gateHeldAfterOwnerHandoff := gateHeldAfterOwnerHandoff c
      gateHeld := held (finalState c) }

structure FinalizationInput where
  name : String
  independentlyScheduled : Bool
  storageReturns : Bool
  deriving Lean.ToJson

@[conformance_family]
def streamFinalization : Conformance.Family FinalizationInput Bool where
  inputs := (cases.filter fun c => !c.rollbackOwnerScheduled).map fun c =>
    { name := c.name
      independentlyScheduled := c.independentlyScheduled
      storageReturns := c.storageReturns }
  model := fun i =>
    held (finalState ⟨i.name, i.independentlyScheduled, i.storageReturns, false, false⟩)

end Conformance.StorageWriteGateContracts
