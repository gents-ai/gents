import Proofs.Runtime.ControlDebounce
import Proofs.Conformance.Family

namespace Conformance.ControlDebounce

open Lean

structure Input where
  firstWake : Nat
  now : Nat
  delay : Nat
  ceiling : Nat
  deriving ToJson

@[conformance_family]
def due : Family Input Nat where
  inputs := [⟨0, 0, 250, 5000⟩, ⟨0, 0, 5000, 5000⟩, ⟨0, 4000, 250, 5000⟩,
    ⟨0, 4900, 250, 5000⟩, ⟨0, 4900, 5000, 5000⟩, ⟨1000, 9000, 5000, 5000⟩]
  model := fun i => _root_.ControlDebounce.due i.firstWake i.now i.delay i.ceiling

end Conformance.ControlDebounce
