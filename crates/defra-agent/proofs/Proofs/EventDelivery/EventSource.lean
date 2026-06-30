import Proofs.EventDelivery.Contract
import Proofs.EventDelivery.Properties

open EventDelivery

namespace EventDelivery.EventSource

/-- EventSource instance.

Binding (operational mapping):
- `persistentSet` = `(collection, doc_id)` pairs in `desired_collections`
  not yet in `seen_docs`.
- `processedSet` is seeded at reconcile with every existing doc id, which
  enforces the forward-only semantic: pre-existing docs do not fire as
  "created" because their first observation finds them in `processedSet`,
  and `Transition.handle` requires `d ∉ processedSet`.
- `rescan` = the live periodic introspection query over desired collections.

The bound is intentionally `1` in the executable model: the emitted
conformance witness `persist → rescanTick → handle` contains a checked
two-action fairness window on each side of the rescan. -/
def eventSourceSrc : SourceInstance :=
  { name := "EventSource"
  , dedupePolicy := .monotoneOnce
  , rescanBoundedBy := 1
  }

/-- EventSource now binds to a positive live rescan cadence. -/
theorem eventSourceSrc_rescanBoundedBy_pos :
    0 < eventSourceSrc.rescanBoundedBy := by
  decide

/-- EventSource specialization of D1, now substantive for the live binding. -/
theorem E1_event_source_delivery_convergence
    (w₀ : World) (d : DocId)
    (h_persisted : d ∈ w₀.persistentSet)
    (h_unprocessed : d ∉ w₀.processedSet) :
    ∃ (actions : List Action) (w' : World),
      TraceOf w₀ actions w' ∧
      Fair eventSourceSrc actions ∧
      d ∈ w'.handled :=
  D1_delivery_convergence
    eventSourceSrc w₀ d h_persisted h_unprocessed
    eventSourceSrc_rescanBoundedBy_pos

end EventDelivery.EventSource
