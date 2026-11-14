import Proofs.Clients.ClientShell.Settings
import Proofs.Conformance.Family

namespace Conformance.ClientSettings

open Lean ClientShell.Settings

deriving instance ToJson for Revision

structure RevisionInput where
  expected : Option Revision
  current : Option Revision
  deriving ToJson

structure RevisionOutput where
  «matches» : Bool
  deriving ToJson

@[conformance_family]
def revisions : Family RevisionInput RevisionOutput where
  inputs :=
    let revisions : List (Option Revision) :=
      [none, some ⟨1, 1⟩, some ⟨1, 2⟩, some ⟨2, 1⟩, some ⟨2, 2⟩]
    revisions.flatMap fun expected => revisions.map fun current => { expected, current }
  model := fun i => { «matches» := revisionMatches i.expected i.current }

structure FieldsInput where
  collection : String
  deriving ToJson

structure FieldsOutput where
  editable : List String
  deriving ToJson

@[conformance_family]
def fields : Family FieldsInput FieldsOutput where
  inputs := SelfConfig.allTargets.map fun target => { collection := target.collectionName }
  model := fun i =>
    { editable := match SelfConfig.allTargets.find? (·.collectionName == i.collection) with
        | some target => editableFields target
        | none => [] }

end Conformance.ClientSettings
