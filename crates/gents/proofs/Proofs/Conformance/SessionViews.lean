import Proofs.Session.Views
import Proofs.Conformance.Family

namespace Conformance

@[conformance_family]
def sessionOrdering : Family (List SessionViews.OrderingRow) (List String) where
  inputs :=
    let a : SessionViews.OrderingRow :=
      ⟨"view-order-a", "2026-01-01T00:00:00Z", "2026-01-01T00:00:00Z", "node", "reader"⟩
    let b : SessionViews.OrderingRow :=
      ⟨"view-order-b", "2026-01-01T00:00:00Z", "2026-01-01T00:00:00Z", "node", "reader"⟩
    let c : SessionViews.OrderingRow :=
      ⟨"view-order-c", "2025-01-01T00:00:00Z", "2027-01-01T00:00:00Z", "node", "reader"⟩
    [[a, b, c], [a, c, b], [b, a, c], [b, c, a], [c, a, b], [c, b, a]]
  model := SessionViews.orderedSessionIds

end Conformance
