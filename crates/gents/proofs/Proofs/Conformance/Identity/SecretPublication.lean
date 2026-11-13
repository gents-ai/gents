import Proofs.Store.Identity.Operations
import Proofs.Conformance.Family

namespace Conformance.IdentitySecretPublication

open Lean Identity.Household Identity.Operations

structure CreationInput where
  name : String
  nodeRegistered : Bool
  deriving ToJson

structure Creation where
  created : Bool
  deriving ToJson

/-- A Node that is not registered cannot become the secret's parent, so the
parent grant is not staged and the creation aborts. -/
@[conformance_family]
def creation : Family CreationInput Creation where
  inputs := [⟨"missing-node", false⟩, ⟨"registered-node", true⟩]
  model := fun i =>
    let before : State := { objects := [], relations := [], enrolled := [] }
    let after := publishSecret before "user" "runtime" "secret" "node" true i.nodeRegistered true true
    { created := after.objects.any fun object =>
        object.resource == "secret_configuration" && object.id == "secret" }

end Conformance.IdentitySecretPublication
