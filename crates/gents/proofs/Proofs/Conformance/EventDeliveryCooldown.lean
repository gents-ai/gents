import Proofs.Automation.EventDelivery.Cooldown
import Proofs.Conformance.Family

namespace Conformance.EventDelivery
open _root_.EventDelivery.Cooldown

@[conformance_family] def cooldownPruning : Conformance.Family Cohort Nat :=
  ⟨[⟨1, 0⟩, ⟨1, lifetimeMillis - 1⟩, ⟨1, lifetimeMillis⟩,
    ⟨1, lifetimeMillis + 1⟩, ⟨capacity, 0⟩, ⟨capacity + 1, 0⟩], retained⟩

end Conformance.EventDelivery
