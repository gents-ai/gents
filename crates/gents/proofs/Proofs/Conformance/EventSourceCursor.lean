import Proofs.Documents.EventSourceCursor
import Proofs.Conformance.Family

namespace Conformance.EventSourceCursor

open Lean _root_.EventSourceCursor

structure SeedInput where
  collectionExists : Bool
  head : Nat
  deriving ToJson

@[conformance_family]
def seed : Family SeedInput Nat where
  inputs := [false, true].flatMap fun collectionExists =>
    [0, 1, 5].map fun head => { collectionExists, head }
  model := fun i => seedAfter i.collectionExists i.head

end Conformance.EventSourceCursor
