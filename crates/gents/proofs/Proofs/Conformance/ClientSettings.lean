import Proofs.ClientShell.Settings
import Lean

open Lean ClientShell.Settings

private def revisionJson : Option Revision → Json
  | none => Json.null
  | some revision => Json.mkObj [
      ("document", toJson revision.document), ("composite", toJson revision.composite)]

def clientSettingsCases : Json :=
  let revisions : List (Option Revision) :=
    [none, some ⟨1, 1⟩, some ⟨1, 2⟩, some ⟨2, 1⟩, some ⟨2, 2⟩]
  Json.mkObj [
    ("revisions", toJson (revisions.flatMap fun expected => revisions.map fun current =>
      Json.mkObj [("expected", revisionJson expected), ("current", revisionJson current),
        ("matches", toJson (revisionMatches expected current))])),
    ("fields", toJson (SelfConfig.allTargets.map fun target => Json.mkObj [
      ("collection", toJson target.collectionName), ("editable", toJson (editableFields target))])),
    ("savedProjections", toJson ([false, true].flatMap fun read =>
      [false, true].flatMap fun physical => [false, true].map fun content =>
        Json.mkObj [("read", toJson read), ("physical", toJson physical), ("content", toJson content),
          ("adopt", toJson (adoptSavedProjection read physical content))]))]
