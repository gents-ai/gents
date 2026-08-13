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

/-- One trigger waiting for a source field cannot prevent a different ready
trigger on the same physical document from being handled.  EventSource models
those as distinct delivery identities, so handling the ready identity leaves
the pending identity eligible and otherwise unchanged. -/
theorem E2_ready_trigger_independent_of_pending_trigger
    (w : World) (ready pending : DocId)
    (h_ready_queued : ready ∈ w.subscriptionQueue)
    (h_ready_unprocessed : ready ∉ w.processedSet)
    (h_pending_persisted : pending ∈ w.persistentSet) :
    let w' :=
      { w with handled := ready :: w.handled
             , processedSet := ready :: w.processedSet
             , subscriptionQueue := w.subscriptionQueue.erase ready }
    Transition w (.handle ready) w' ∧ pending ∈ w'.persistentSet := by
  constructor
  · exact Transition.handle w ready h_ready_queued h_ready_unprocessed
  · exact h_pending_persisted

end EventDelivery.EventSource
