namespace EventDelivery

structure DocId where
  raw : String
  deriving DecidableEq, Repr

inductive DedupePolicy where
  | ttlCooldown
  | monotoneOnce
  deriving DecidableEq, Repr

namespace DedupePolicy

def toContract : DedupePolicy → String
  | .ttlCooldown => "ttl_cooldown"
  | .monotoneOnce => "monotone_once"

def fromContract? : String → Option DedupePolicy
  | "ttl_cooldown" => some .ttlCooldown
  | "monotone_once" => some .monotoneOnce
  | _ => none

theorem fromContract_toContract (p : DedupePolicy) :
    fromContract? p.toContract = some p := by
  cases p <;> rfl

end DedupePolicy

structure World where
  persistentSet     : List DocId
  subscriptionQueue : List DocId
  processedSet      : List DocId
  handled           : List DocId
  deriving DecidableEq, Repr

def World.empty : World :=
  { persistentSet := []
  , subscriptionQueue := []
  , processedSet := []
  , handled := []
  }

inductive Action where
  | persist (d : DocId)
  | depersist (d : DocId)
  | enqueue (d : DocId)
  | drop (d : DocId)
  | deliverFromQueue (d : DocId)
  | rescanTick
  | handle (d : DocId)
  deriving DecidableEq, Repr

def Action.isRescan : Action → Bool
  | .rescanTick => true
  | _ => false

inductive Transition : World → Action → World → Prop where
  | persist (w : World) (d : DocId) :
      d ∉ w.persistentSet →
      Transition w (.persist d)
        { w with persistentSet := d :: w.persistentSet }
  | depersist (w : World) (d : DocId) :
      d ∈ w.persistentSet →
      Transition w (.depersist d)
        { w with persistentSet := w.persistentSet.erase d }
  | enqueue (w : World) (d : DocId) :
      d ∈ w.persistentSet →
      Transition w (.enqueue d)
        { w with subscriptionQueue := d :: w.subscriptionQueue }
  | drop (w : World) (d : DocId) :
      d ∈ w.subscriptionQueue →
      Transition w (.drop d)
        { w with subscriptionQueue := w.subscriptionQueue.erase d }
  | deliverFromQueue (w : World) (d : DocId) :
      d ∈ w.subscriptionQueue →
      Transition w (.deliverFromQueue d)
        { w with subscriptionQueue := w.subscriptionQueue.erase d }
  | rescanTick (w : World) :
      Transition w .rescanTick
        { w with subscriptionQueue :=
            (w.persistentSet.filter (fun d => d ∉ w.processedSet)) ++ w.subscriptionQueue }
  | handle (w : World) (d : DocId) :
      d ∈ w.subscriptionQueue →
      d ∉ w.processedSet →
      Transition w (.handle d)
        { w with handled := d :: w.handled
               , processedSet := d :: w.processedSet
               , subscriptionQueue := w.subscriptionQueue.erase d }

inductive Trace : World → World → Prop where
  | refl {w : World} : Trace w w
  | step {w₁ w₂ w₃ : World} {a : Action} :
      Transition w₁ a w₂ → Trace w₂ w₃ → Trace w₁ w₃

structure SourceInstance where
  name            : String
  dedupePolicy    : DedupePolicy
  rescanBoundedBy : Nat
  deriving Repr

def SourceInstance.unboundedRescan : Nat := 0

end EventDelivery
