import Proofs.Tools.ToolPolicy.FieldRead
import Proofs.Conformance.Family

namespace Conformance.DiscoveryFieldVisibility

open Lean ToolPolicy.FieldRead

structure FieldInput where
  collection : String
  field : String
  deriving ToJson

structure FieldOutput where
  listed : Bool
  deriving ToJson

@[conformance_family]
def listing : Family FieldInput FieldOutput where
  inputs := [("InferenceBackend", "backend_id"), ("InferenceBackend", "endpoint"),
      ("InferenceBackend", "auth")].map fun (collection, field) => { collection, field }
  model := fun i => { listed := listedInDiscovery i.collection i.field }

end Conformance.DiscoveryFieldVisibility
