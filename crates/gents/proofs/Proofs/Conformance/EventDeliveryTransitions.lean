import Proofs.Automation.EventDelivery.Examples
import Proofs.Conformance.Family

namespace Conformance.EventDelivery
open _root_.EventDelivery

inductive DeliveryOperation where
  | rescan | handle | release
  deriving Lean.ToJson

structure DeliveryInput where
  name : String
  pre : World
  operation : DeliveryOperation
  document : String
  deriving Lean.ToJson

def DeliveryInput.action (i : DeliveryInput) : Action :=
  match i.operation with
  | .rescan => .rescanTick
  | .handle => .handle ⟨i.document⟩
  | .release => .release ⟨i.document⟩

def deliveryInput? (c : StepInput) : Option DeliveryInput :=
  match c.action with
  | .rescanTick => some ⟨c.name, c.pre, .rescan, ""⟩
  | .handle d => some ⟨c.name, c.pre, .handle, d.raw⟩
  | .release d => some ⟨c.name, c.pre, .release, d.raw⟩
  | _ => none

def watcherEmission (i : DeliveryInput) : List String :=
  let post := (step? i.pre i.action).getD i.pre
  ((match i.operation with
    | .rescan => post.subscriptionQueue.take (post.subscriptionQueue.length - i.pre.subscriptionQueue.length)
    | .handle => post.handled.take (post.handled.length - i.pre.handled.length)
    | .release => []) : List DocId).map (·.raw)

@[conformance_family] def watcherTransitions : Conformance.Family DeliveryInput (List String) :=
  ⟨(transitionInputs.filterMap deliveryInput?).filter (fun i =>
    (match i.operation with | .release => false | _ => true) &&
    i.name != "handle_ready_trigger_preserves_pending_sibling" &&
    i.name != "rescan_on_empty"), watcherEmission⟩

structure ReleaseObservation where
  retained : List String
  released : List String
  deriving Lean.ToJson

def releaseObservation (i : DeliveryInput) : ReleaseObservation :=
  let post := (step? i.pre i.action).getD i.pre
  ⟨post.processedSet.map (·.raw),
    (i.pre.processedSet.filter (fun d => d ∉ post.processedSet)).map (·.raw)⟩

@[conformance_family] def watcherReleases : Conformance.Family DeliveryInput ReleaseObservation :=
  ⟨(transitionInputs.filterMap deliveryInput?).filter (fun i =>
    match i.operation with | .release => true | _ => false), releaseObservation⟩

structure SiblingObservation where
  handled : List String
  pending : List String
  deriving Lean.ToJson

def siblingObservation (i : DeliveryInput) : SiblingObservation :=
  let post := (step? i.pre i.action).getD i.pre
  ⟨post.handled.map (·.raw), post.subscriptionQueue.map (·.raw)⟩

@[conformance_family] def eventSourceSiblings : Conformance.Family DeliveryInput SiblingObservation :=
  ⟨(transitionInputs.filterMap deliveryInput?).filter (fun i =>
    i.name == "handle_ready_trigger_preserves_pending_sibling"), siblingObservation⟩

end Conformance.EventDelivery
