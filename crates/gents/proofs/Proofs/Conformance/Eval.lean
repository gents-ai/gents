import Proofs.Eval
import Proofs.Conformance.ContractTypes

namespace Conformance.Eval

open Conformance.Contracts
open _root_.Eval

private def boolJson (b : Bool) : String := if b then "true" else "false"

private def reasonJson : Option ProviderReason → String
  | none => "null"
  | some r => jsonString r.toDefraDB

private def row (k : OutcomeKind) (r : Option ProviderReason) : String :=
  "{\"kind\":" ++ jsonString k.toDefraDB
    ++ ",\"provider_reason\":" ++ reasonJson r
    ++ ",\"class\":" ++ jsonString (classify k r).toDefraDB
    ++ ",\"subject_causable\":" ++ boolJson (subjectCausable k r) ++ "}"

def evalOutcomeCasesJson : String :=
  jsonArray (allKinds.flatMap fun k =>
    ([none] ++ allReasons.map some).map fun r => row k r)

private def captureRow (collection : String) (fields filterFields : List String)
    (nested : Bool) : String :=
  let filter := "{" ++ String.intercalate "," (filterFields.map fun field =>
    jsonString field ++ ":{\"_eq\":\"$trial\"}") ++ "}"
  let filter := if nested then "{\"_or\":[" ++ filter ++ "]}" else filter
  "{\"collection\":" ++ jsonString collection ++
    ",\"fields\":" ++ jsonStringArray fields ++
    ",\"filter_fields\":" ++ jsonStringArray filterFields ++
    ",\"filter\":" ++ filter ++
    ",\"allowed\":" ++ boolJson (documentCaptureAllowed collection fields filterFields) ++ "}"

def evalDocumentCaptureCasesJson : String := jsonArray <|
  (["Task", "Agent"] ++ ScopeTemplates.credentialCollections).flatMap fun collection =>
    [[], ["backend_id"], ["backend_id", "endpoint", "enabled", "max_concurrent"],
      ["auth"], ["backend_id", "auth"], ["credential_id"], ["access_token"],
      ["refresh_token"], ["id_token"]].flatMap fun fields =>
        [[], ["node_did"], ["auth"], ["node_did", "auth"], ["access_token"],
          ["refresh_token"], ["id_token"]].flatMap fun filterFields =>
            [false, true].map fun nested => captureRow collection fields filterFields nested

end Conformance.Eval
