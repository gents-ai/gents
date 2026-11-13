import Proofs.Runtime.StartupBarrier
import Proofs.Conformance.Family

namespace Runtime.StartupBarrier
open Conformance

@[conformance_family]
def obligations : Family Input (List Observation) :=
  ⟨[⟨[], []⟩, ⟨["a"], []⟩] ++
    [Operation.ready, .demote, .supersede].flatMap (fun operation =>
      [⟨["b", "a"], [⟨operation, ⟨"a", 1⟩⟩, ⟨operation, ⟨"b", 1⟩⟩, ⟨operation, ⟨"a", 1⟩⟩]⟩,
       ⟨["a"], [⟨.register, ⟨"a", 2⟩⟩, ⟨operation, ⟨"a", 2⟩⟩, ⟨operation, ⟨"a", 1⟩⟩]⟩,
       ⟨[], [⟨operation, ⟨"a", 1⟩⟩, ⟨.register, ⟨"a", 1⟩⟩, ⟨.register, ⟨"a", 1⟩⟩,
         ⟨operation, ⟨"a", 2⟩⟩, ⟨operation, ⟨"a", 1⟩⟩]⟩]), run⟩

end Runtime.StartupBarrier
