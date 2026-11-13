import Proofs.Session.Identity.SessionInputEdit
import Proofs.Conformance.Family

namespace Conformance.SessionInputEdit

open Lean Identity.SessionInputEdit

structure DiscoveryInput where
  actor : String
  deriving ToJson

structure DiscoveryOutput where
  visible : Bool
  selected : Bool
  deriving ToJson

@[conformance_family]
def nativeDiscovery : Family DiscoveryInput DiscoveryOutput where
  inputs := ["agent", "other"].map fun actor => { actor }
  model := fun i =>
    { visible := !(discover fixture [i.actor] "edit-doc").isEmpty
      selected := !(selectedForEffect (discover fixture [i.actor] "edit-doc") "agent").isEmpty }

end Conformance.SessionInputEdit
