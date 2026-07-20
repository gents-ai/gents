/-!
# EventDelivery Contract

Shared abstract contract for lossy-subscription + bounded-rescan event delivery.
Three runtime sources instantiate this contract: the request watcher, the
event-trigger source, and the subagent source. See
`docs/superpowers/specs/2026-05-13-event-drop-resync-lean-design.md` (removed from the tree; see git history) for the
full design and the operational mapping to Rust call sites.
-/

namespace EventDelivery

/-- Opaque document identifier. Each `SourceInstance` binds it (request_id,
    (collection, doc_id), or tool_call_id). -/
structure DocId where
  raw : String
  deriving DecidableEq, Repr

/-- Operational dedupe-set policy. Watcher uses `ttlCooldown`; EventSource and
    SubagentSource use `monotoneOnce`. -/
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

/-- The abstract world. Constructive (not tick-indexed): convergence is proved
    via reachability traces, not via wall-clock time. -/
structure World where
  persistentSet     : List DocId
  subscriptionQueue : List DocId
  processedSet      : List DocId
  handled           : List DocId
  deriving DecidableEq, Repr

/-- Empty initial world. -/
def World.empty : World :=
  { persistentSet := []
  , subscriptionQueue := []
  , processedSet := []
  , handled := []
  }

/-- Single observable step the source can take. -/
inductive Action where
  | persist (d : DocId)
  | depersist (d : DocId)
  | enqueue (d : DocId)
  | drop (d : DocId)
  | deliverFromQueue (d : DocId)
  | rescanTick
  | handle (d : DocId)
  deriving DecidableEq, Repr

/-- Is this action a rescanTick? (Used by the `Fair` predicate.) -/
def Action.isRescan : Action → Bool
  | .rescanTick => true
  | _ => false

/-- Step relation. Each constructor enforces preconditions on `World`. -/
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
      -- The rescan dumps every persistent doc not in processedSet into the
      -- subscription queue. Operationally: `pending_requests().await`
      -- (watcher) or the periodic introspection query (EventSource /
      -- SubagentSource — Rust gap-fill).
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

/-- Reflexive-transitive closure: a finite trace of valid transitions. -/
inductive Trace : World → World → Prop where
  | refl {w : World} : Trace w w
  | step {w₁ w₂ w₃ : World} {a : Action} :
      Transition w₁ a w₂ → Trace w₂ w₃ → Trace w₁ w₃

/-- `SourceInstance` binds the contract to a concrete runtime subsystem.
    `rescanBoundedBy : Nat` is the maximum number of non-rescanTick actions
    that may occur between two consecutive `rescanTick`s in a `Fair` sequence
    (see `Properties.lean`). Live source bindings that claim D1 must use a
    positive bound; `unboundedRescan = 0` is retained only as vocabulary for
    explicitly documented non-live or future deviation instances. -/
structure SourceInstance where
  name            : String
  dedupePolicy    : DedupePolicy
  rescanBoundedBy : Nat
  deriving Repr

/-- Concretely `0`; see `SourceInstance.rescanBoundedBy` for semantics. -/
def SourceInstance.unboundedRescan : Nat := 0

end EventDelivery
