import Proofs.Tools.DatastoreWrite
import Proofs.Conformance.Family

namespace Conformance.DatastoreWrite

open Lean _root_.DatastoreWrite

deriving instance ToJson for Fill
deriving instance ToJson for Field
deriving instance ToJson for Outcome
deriving instance ToJson for Stored

structure Input where
  collection : String
  fields : List Field
  supplied : List String
  surface : Option String
  correlation : Bool
  captured : List String
  deriving ToJson

structure Output where
  wellFormed : Bool
  exposed : List String
  outcome : Outcome
  written : Option (List Stored)
  deriving ToJson

private def bounded : List Field :=
  [⟨"drift_sig", true, .caller⟩, ⟨"summary", true, .caller⟩, ⟨"status", false, .caller⟩]

private def stamped : List Field :=
  [⟨"summary", true, .caller⟩, ⟨"run_id", false, .correlation⟩,
   ⟨"expected_total", false, .sourceField⟩]

private def sourced : List Field :=
  [⟨"summary", true, .caller⟩, ⟨"expected_total", false, .sourceField⟩]

@[conformance_family]
def toolCall : Family Input Output where
  inputs :=
    (protectedCollections ++ ["ActionRequest"]).map (fun collection =>
      ⟨collection, bounded, ["drift_sig", "summary", "status"], none, false, []⟩) ++
    [ ⟨"ActionRequest", bounded, ["drift_sig", "summary"], none, false, []⟩
    , ⟨"ActionRequest", stamped, ["summary", "run_id"], none, true, ["expected_total"]⟩
    , ⟨"ActionRequest", stamped, ["summary", "run_id"], some "results", true, ["expected_total"]⟩
    , ⟨"ActionRequest", stamped, ["summary", "expected_total"], none, true, ["expected_total"]⟩
    , ⟨"ActionRequest", stamped, ["summary"], none, true, ["expected_total"]⟩
    , ⟨"ActionRequest", stamped, ["summary"], none, false, ["expected_total"]⟩
    , ⟨"ActionRequest", stamped, [], none, true, ["expected_total"]⟩
    , ⟨"ActionRequest", sourced, ["summary"], none, true, []⟩
    , ⟨"ActionRequest", sourced, ["summary"], none, true, ["expected_total"]⟩ ]
  model := fun i =>
    let request : Request := ⟨i.correlation, i.captured⟩
    { wellFormed := wellFormed i.collection
      exposed := exposed i.fields
      outcome := _root_.DatastoreWrite.call i.collection i.fields i.supplied request
      written := written i.collection i.fields i.supplied request }

end Conformance.DatastoreWrite
