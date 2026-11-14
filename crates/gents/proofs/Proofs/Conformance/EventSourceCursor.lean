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

structure FirstArrivalInput where
  arrivals : Nat
  deriving ToJson

structure FirstArrivalOutput where
  seeded : Nat
  afterPublish : Nat
  pending : List Nat
  deriving ToJson

@[conformance_family]
def firstArrival : Family FirstArrivalInput FirstArrivalOutput where
  inputs := [0, 1, 2].map fun arrivals => { arrivals }
  model := fun i =>
    { seeded := seedAfter false 0
      afterPublish := afterTriggerPublication (seedAfter false 0)
      pending := pendingAfter (seedAfter false 0) (arrivalPositions i.arrivals) }

end Conformance.EventSourceCursor
