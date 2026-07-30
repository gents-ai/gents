import Proofs.EventDelivery.Contract
import Proofs.EventDelivery.Properties

open EventDelivery

namespace EventDelivery.SubagentSource

def subagentSourceSrc : SourceInstance :=
  { name := "SubagentSource"
  , dedupePolicy := .monotoneOnce
  , rescanBoundedBy := 1
  }

theorem subagentSourceSrc_rescanBoundedBy_pos :
    0 < subagentSourceSrc.rescanBoundedBy := by
  decide

theorem O1_orphan_child_materialization
    (w₀ : World) (d : DocId)
    (h_persisted : d ∈ w₀.persistentSet)
    (h_unprocessed : d ∉ w₀.processedSet) :
    ∃ (actions : List Action) (w' : World),
      TraceOf w₀ actions w' ∧
      Fair subagentSourceSrc actions ∧
      d ∈ w'.handled :=
  D1_delivery_convergence
    subagentSourceSrc w₀ d h_persisted h_unprocessed
    subagentSourceSrc_rescanBoundedBy_pos

end EventDelivery.SubagentSource
