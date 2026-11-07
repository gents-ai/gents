import Proofs.ScopeTemplates.AppFilters
import Proofs.Conformance.Contracts.Json.Core

namespace Conformance.AppPairingFilters

open ScopeTemplates.AppFilters Conformance.Contracts

private def authority : List FieldAuthority :=
  [{ collection := "Post", field := "to_host", immutable := true },
   { collection := "Post", field := "message", immutable := false }]

private def cases : List (String × List String × List FieldFilter) :=
  [ ("unfiltered", ["Post"], [])
  , ("immutable", ["Post"], [{ collection := "Post", field := "to_host", value := "node" }])
  , ("mutable", ["Post"], [{ collection := "Post", field := "message", value := "node" }])
  , ("unknown_field", ["Post"], [{ collection := "Post", field := "other", value := "node" }])
  , ("other_collection", [], [{ collection := "Post", field := "to_host", value := "node" }])
  , ("empty_value", ["Post"], [{ collection := "Post", field := "to_host", value := "" }])
  , ("duplicate", ["Post"], [{ collection := "Post", field := "to_host", value := "node" },
      { collection := "Post", field := "to_host", value := "other" }]) ]

private def filterJson (filter : FieldFilter) : String :=
  "{\"collection\":" ++ jsonString filter.collection ++ ",\"field\":" ++
  jsonString filter.field ++ ",\"value\":" ++ jsonString filter.value ++ "}"

private def authorityJson (field : FieldAuthority) : String :=
  "{\"collection\":" ++ jsonString field.collection ++ ",\"field\":" ++
  jsonString field.field ++ ",\"immutable\":" ++
  (if field.immutable then "true" else "false") ++ "}"

private def caseJson (value : String × List String × List FieldFilter) : String :=
  let (label, collections, filters) := value
  "{\"label\":" ++ jsonString label ++ ",\"collections\":" ++ jsonStringArray collections ++
  ",\"filters\":" ++ jsonArray (filters.map filterJson) ++ ",\"authority\":" ++
  jsonArray (authority.map authorityJson) ++ ",\"valid\":" ++
  (if (validate collections authority filters).isSome then "true" else "false") ++ "}"

def casesJson : String := jsonArray (cases.map caseJson)

private def routeCases : List (String × PairingMode × Option (List FieldFilter)) :=
  [ ("unfiltered", .unfiltered, none)
  , ("missing", .filtered, none)
  , ("empty", .filtered, some [])
  , ("valid", .filtered, some [{ collection := "Post", field := "to_host", value := "node" }])
  , ("mutable", .filtered, some [{ collection := "Post", field := "message", value := "node" }])
  , ("unexpected", .unfiltered, some [{ collection := "Post", field := "to_host", value := "node" }]) ]

private def routeCaseJson (value : String × PairingMode × Option (List FieldFilter)) : String :=
  let (label, mode, filters) := value
  "{\"label\":" ++ jsonString label ++ ",\"mode\":" ++
    jsonString (if mode == .filtered then "filtered" else "unfiltered") ++
    ",\"filters\":" ++ (match filters with
      | none => "null"
      | some values => jsonArray (values.map filterJson)) ++
    ",\"accepted\":" ++
    (if (resolve mode ["Post"] authority filters).isSome then "true" else "false") ++ "}"

def routeCasesJson : String := jsonArray (routeCases.map routeCaseJson)

end Conformance.AppPairingFilters
