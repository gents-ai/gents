import Proofs.Tools.DatastoreUpsert
import Proofs.Conformance.Family

namespace DatastoreUpsert

deriving instance Lean.ToJson for Existing
deriving instance Lean.ToJson for Outcome

@[conformance_function]
def write (keyed : Bool) (existing : Existing) : Outcome :=
  call keyed existing

structure Declaration where
  fields : List String
  keys : List String
  deriving Lean.ToJson

@[conformance_family]
def declaration : Conformance.Family Declaration Bool where
  inputs :=
    [ ⟨["owner", "repo", "theme"], ["owner"]⟩,
      ⟨["owner", "repo", "theme"], ["owner", "repo"]⟩,
      ⟨["owner", "theme"], []⟩,
      ⟨["owner", "theme"], ["repo"]⟩,
      ⟨["owner", "theme"], ["owner", "repo"]⟩,
      ⟨["owner", "theme"], ["owner", "owner"]⟩ ]
  model := fun declaration => wellFormed declaration.fields declaration.keys

end DatastoreUpsert
