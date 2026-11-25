import Proofs.Store.BackendPreflight
import Proofs.Conformance.Family

namespace Conformance.BackendPreflight

structure CopyInput where
  result : _root_.BackendPreflight.CopyResult
  deriving Lean.ToJson

@[conformance_family]
def copyCheck : Family CopyInput String where
  inputs := [.compatible, .incompatible, .unavailable].map fun result => { result }
  model := fun i => _root_.BackendPreflight.decision i.result

structure LineageInput where
  current : Nat
  names : List String
  beforeLock : Bool
  deriving Lean.ToJson

/-- Store directory listings around lineage `current`: its own marker, an
earlier and a later one and mixes of them in either order (a directory lists
in no fixed order), versions that cannot be read, and names that only resemble
a marker. -/
def lineageListings (current : Nat) : List (List String) :=
  let named (suffix : String) := _root_.BackendPreflight.markerPrefix ++ suffix
  let version (n : Nat) := named (toString n)
  [[], ["MANIFEST", ".gents-store-compatibility"],
    [version current], [version (current - 1)], [version (current + 1)],
    [version current, version (current + 1)], [version (current + 1), version current],
    [named "x", version current], [version (current - 1), version current],
    [version (current - 1), version (current + 1)], ["MANIFEST", version (current - 1)],
    [named "x"], [named ""], [named ("0" ++ toString current)], [named ("+" ++ toString current)],
    [named "18446744073709551616"], [version (current - 1), named "x"],
    [version current ++ ".tmp"],
    [".gents-private-backend", ".gents-private-backend-" ++ toString current]]

@[conformance_family]
def lineageRefusal : Family LineageInput (Option _root_.BackendPreflight.Refusal) where
  inputs := [1, 3].flatMap fun current => (lineageListings current).flatMap fun names =>
    [true, false].map fun beforeLock => { current, names, beforeLock }
  model := fun i =>
    _root_.BackendPreflight.refusal i.beforeLock (_root_.BackendPreflight.lineage i.current i.names)

end Conformance.BackendPreflight
