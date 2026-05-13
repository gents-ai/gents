import Proofs.EventDelivery.Contract
import Proofs.EventDelivery.Properties

open EventDelivery

namespace EventDelivery.EventSource

/-- EventSource instance.

Uses the `unboundedRescan` sentinel because the Rust EventSource has no
periodic rescan today. D1 holds vacuously: `Fair eventSourceSrc actions` is
unsatisfiable on non-trivial action lists (`rescanBoundedBy = 0` forces
every action to be `rescanTick`, which cannot include `persist`/`handle`).
The corresponding `Conformance/Deviations.lean` entry names the gap and the
follow-up issue that closes it.

Binding (operational mapping):
- `persistentSet` = `(collection, doc_id)` pairs in `desired_collections`
  not yet in `seen_docs`.
- `processedSet` is seeded at reconcile with every existing doc id, which
  enforces the forward-only semantic: pre-existing docs do not fire as
  "created" because their first observation finds them in `processedSet`,
  and `Transition.handle` requires `d ∉ processedSet`.
- `rescan` = the periodic introspection query this PR asks Rust to grow.

When Rust adds the periodic rescan, flip `rescanBoundedBy` to a positive
`Nat` and the substantive `D1_delivery_convergence` specialization becomes
provable for EventSource (mirroring `Proofs/EventDelivery/Watcher.lean`). -/
def eventSourceSrc : SourceInstance :=
  { name := "EventSource"
  , dedupePolicy := .monotoneOnce
  , rescanBoundedBy := SourceInstance.unboundedRescan
  }

/-- Sentinel record: EventSource currently uses the unbounded-rescan
    sentinel. Lands the binding's current state into conformance metadata
    without trying to prove a vacuous D1 specialization. The deviation
    entry in `Conformance/Deviations.lean` carries the load. -/
theorem eventSourceSrc_rescanBoundedBy_is_sentinel :
    eventSourceSrc.rescanBoundedBy = SourceInstance.unboundedRescan := rfl

end EventDelivery.EventSource
