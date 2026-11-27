import Proofs.Store.StorageWriteGate
import Proofs.Conformance.Family

namespace Conformance.StorageWriteGateContracts
open StorageWriteGate

deriving instance Lean.ToJson for StorageWriteGate.Event

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
  consumerDrops : Bool
  deriving Lean.ToJson

structure FinalizationOutput where
  gateHeld : Bool
  guardDrops : Nat
  finalizerFinished : Bool
  deriving Lean.ToJson

def finalizationInputs : List FinalizationInput :=
  (cases.filter fun c => !c.rollbackOwnerScheduled).map fun c =>
    { name := c.name
      independentlyScheduled := c.independentlyScheduled
      storageReturns := c.storageReturns
      consumerDrops := false }

def consumerDropInput : FinalizationInput :=
  { name := "consumer_drop_aborts_owned_finalizer"
    independentlyScheduled := true
    storageReturns := false
    consumerDrops := true }

@[conformance_family]
def streamFinalization : Conformance.Family FinalizationInput FinalizationOutput where
  inputs := finalizationInputs ++ [consumerDropInput]
  model := fun i =>
    let c : Case := ⟨i.name, i.independentlyScheduled, i.storageReturns, false, false⟩
    let final :=
      if i.consumerDrops then run ⟨.storage, i.independentlyScheduled, false⟩ [.consumerDrop]
      else finalState c
    { gateHeld := held final
      guardDrops := guardDropCount final
      finalizerFinished := !i.consumerDrops && !held final }

structure LifecycleInput where
  name : String
  events : List Event
  deriving Lean.ToJson

structure LifecycleOutput where
  initialGateHeld : Bool
  gateHeldAfterStep : List Bool
  deriving Lean.ToJson

def lifecycleInitial : State := ⟨.storage, true, false⟩

def evalLifecycleSteps (state : State) : List Event → List Bool
  | [] => []
  | event :: rest =>
      let next := step state event
      held next :: evalLifecycleSteps next rest

def lifecycleInputs : List LifecycleInput :=
  [ { name := "storage_return_releases_gate"
      events := [.storageReturned, .release] }
  , { name := "elapsed_before_deadline_keeps_gate"
      events := [.elapsed, .storageReturned, .release] }
  , { name := "cleanup_return_releases_gate"
      events := [.deadlineObserved, .cleanupReturned, .release] }
  , { name := "drop_return_releases_gate"
      events := [.deadlineObserved, .cleanupDeadlineObserved, .dropReturned, .release] } ]

@[conformance_family]
def lifecycle : Conformance.Family LifecycleInput LifecycleOutput where
  inputs := lifecycleInputs
  model := fun i =>
    { initialGateHeld := held lifecycleInitial
      gateHeldAfterStep := evalLifecycleSteps lifecycleInitial i.events }

theorem lifecycle_cleanup_and_drop_hold_until_release :
    evalLifecycleSteps lifecycleInitial [.deadlineObserved, .cleanupReturned, .release] =
        [true, true, false] ∧
      evalLifecycleSteps lifecycleInitial
          [.deadlineObserved, .cleanupDeadlineObserved, .dropReturned, .release] =
        [true, true, true, false] := by
  decide

end Conformance.StorageWriteGateContracts
