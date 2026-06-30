import Proofs.EventDelivery.Contract
import Proofs.EventDelivery.Properties

open EventDelivery

namespace EventDelivery.SubagentSource

/-- SubagentSource instance.

Binding:
- `persistentSet` = running `AgentToolCall` rows with `child_request_id`
  set whose child `AgentRequest` row doesn't yet exist (the orphan
  condition).
- `processedSet` ⊇ tool-call ids already materialized in this process.
- `rescan` = the live periodic scan over running bridge rows. -/
def subagentSourceSrc : SourceInstance :=
  { name := "SubagentSource"
  , dedupePolicy := .monotoneOnce
  , rescanBoundedBy := 1
  }

/-- SubagentSource now binds to a positive live rescan cadence. -/
theorem subagentSourceSrc_rescanBoundedBy_pos :
    0 < subagentSourceSrc.rescanBoundedBy := by
  decide

/-- **O1 — Orphan-child materialization** (SubagentSource specialization).

If a running AgentToolCall row has `child_request_id = Some c` and `c` is
not yet present as an `AgentRequest` row, then under a fair trace `c`
eventually appears.

Stated unconditionally as a corollary of `D1_delivery_convergence` for the
SubagentSource instance. The positive bound is part of the binding, so this
is no longer guarded by an external non-vacuity hypothesis. -/
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
