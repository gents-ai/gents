import Proofs.EventDelivery.Contract
import Proofs.EventDelivery.Properties

open EventDelivery

namespace EventDelivery.SubagentSource

/-- SubagentSource instance.

Same `unboundedRescan` sentinel as EventSource. The existing operational
recovery primitive `recover_orphan_subagent_children` runs only at
startup; lifting it to a periodic loop in the live process closes the
deviation entry filed in `Conformance/Deviations.lean`.

Binding:
- `persistentSet` = running `AgentToolCall` rows with `child_request_id`
  set whose child `AgentRequest` row doesn't yet exist (the orphan
  condition).
- `processedSet` ⊇ tool-call ids already materialized in this process.
- `rescan` = `recover_orphan_subagent_children` lifted to a periodic
  timer (Rust gap-fill named in the deviation entry). -/
def subagentSourceSrc : SourceInstance :=
  { name := "SubagentSource"
  , dedupePolicy := .monotoneOnce
  , rescanBoundedBy := SourceInstance.unboundedRescan
  }

/-- **O1 — Orphan-child materialization** (SubagentSource specialization).

If a running AgentToolCall row has `child_request_id = Some c` and `c` is
not yet present as an `AgentRequest` row, then under a fair trace `c`
eventually appears.

Stated unconditionally as a corollary of `D1_delivery_convergence` for the
SubagentSource instance. Substantive when `subagentSourceSrc.rescanBoundedBy
> 0`; vacuous today (deviation entry records the gap). The hypothesis
`h_inst_pos : 0 < subagentSourceSrc.rescanBoundedBy` is the explicit
witness that flips this property substantive when Rust adds the periodic
loop. -/
theorem O1_orphan_child_materialization
    (w₀ : World) (d : DocId)
    (h_persisted : d ∈ w₀.persistentSet)
    (h_unprocessed : d ∉ w₀.processedSet)
    (h_inst_pos : 0 < subagentSourceSrc.rescanBoundedBy) :
    ∃ (actions : List Action) (w' : World),
      TraceOf w₀ actions w' ∧
      Fair subagentSourceSrc actions ∧
      d ∈ w'.handled :=
  D1_delivery_convergence subagentSourceSrc w₀ d h_persisted h_unprocessed h_inst_pos

/-- Sentinel record: SubagentSource currently uses the unbounded-rescan
    sentinel. The `Conformance/Deviations.lean` entry names the
    live-rescan gap and the follow-up issue. -/
theorem subagentSourceSrc_rescanBoundedBy_is_sentinel :
    subagentSourceSrc.rescanBoundedBy = SourceInstance.unboundedRescan := rfl

end EventDelivery.SubagentSource
