import Proofs.EventDelivery.Contract
import Proofs.EventDelivery.Properties

open EventDelivery

namespace EventDelivery.Watcher

/-- Watcher instance.

`rescanBoundedBy = 1`: the contract definition counts non-rescanTick actions
between rescanTicks. The Rust `next_request` loop (`watcher.rs:88`) runs
`pending_requests()` on every iteration, so at most one non-rescan action
(e.g. a `handle` of the previous iteration's pickup) can occur between
rescans. The 30s `GOSSIP_FALLBACK_POLL` is the upper bound on
subscription-quiet idle, not the rescan-action gap. -/
def watcherSrc : SourceInstance :=
  { name := "Watcher"
  , dedupePolicy := .ttlCooldown
  , rescanBoundedBy := 1
  }

/-- D1 specialized to the watcher instance. Substantive: rescanBoundedBy = 1 > 0. -/
theorem watcher_pending_eventually_observed
    (w₀ : World) (d : DocId)
    (h_persisted : d ∈ w₀.persistentSet)
    (h_unprocessed : d ∉ w₀.processedSet) :
    ∃ (actions : List Action) (w' : World),
      TraceOf w₀ actions w' ∧
      Fair watcherSrc actions ∧
      d ∈ w'.handled :=
  D1_delivery_convergence watcherSrc w₀ d h_persisted h_unprocessed (by decide)

/-- C1 specialized: while a request id is in the watcher's processedSet
    (within cooldown), no duplicate handle fires for it. -/
theorem watcher_cooldown_excludes_handle
    (w : World) (d : DocId) (a : Action) (w' : World)
    (h_processed : d ∈ w.processedSet)
    (h : Transition w a w') :
    a ≠ .handle d :=
  C1_processed_set_excludes_handle w d a w' h_processed h

end EventDelivery.Watcher
