import Proofs.Automation.EventDelivery.Convergence
import Proofs.Conformance.Family

namespace Conformance.EventDelivery
open _root_.EventDelivery

@[conformance_family] def watcherConvergence : Conformance.Family ConvergenceInput (Option ConvergenceObservation) :=
  ⟨[convergenceTrace "watcher_persist_rescan_handle" "req-1"], convergenceObservation⟩

@[conformance_family] def eventSourceConvergence : Conformance.Family ConvergenceInput (Option ConvergenceObservation) :=
  ⟨[convergenceTrace "event_source_persist_rescan_handle" "doc-1",
    unacknowledgedTrace "doc-retry", seededUpdateTrace "doc-seeded",
    firstSeenUpdateTrace "doc-first"], convergenceObservation⟩

end Conformance.EventDelivery
