import Proofs.EventDelivery.Contract
import Proofs.EventDelivery.Properties

open EventDelivery

namespace EventDelivery.EventSource

def eventSourceSrc : SourceInstance :=
  { name := "EventSource"
  , dedupePolicy := .monotoneOnce
  , rescanBoundedBy := 1
  }

theorem eventSourceSrc_rescanBoundedBy_pos :
    0 < eventSourceSrc.rescanBoundedBy := by
  decide

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
