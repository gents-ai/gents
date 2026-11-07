import Proofs.ScopeTemplates.AppFilters
import Proofs.Conformance.Contracts.Json.Core

namespace Conformance.AppPairingFilters

open ScopeTemplates.AppFilters Conformance.Contracts

private def authority : List FieldAuthority :=
  [{ collection := "Post", field := "to_host", immutable := true },
   { collection := "Post", field := "message", immutable := false },
   { collection := "Post", field := "unsupported", immutable := true, supported := false }]

private def cases : List (String × List String × List FieldFilter) :=
  [ ("unfiltered", ["Post"], [])
  , ("immutable", ["Post"], [{ collection := "Post", field := "to_host", value := "node" }])
  , ("mutable", ["Post"], [{ collection := "Post", field := "message", value := "node" }])
  , ("unsupported", ["Post"], [{ collection := "Post", field := "unsupported", value := "node" }])
  , ("unknown_field", ["Post"], [{ collection := "Post", field := "other", value := "node" }])
  , ("different_version", ["Post"], [{ collection := "Post", field := "to_host", value := "node", version := "v2" }])
  , ("other_collection", [], [{ collection := "Post", field := "to_host", value := "node" }])
  , ("empty_value", ["Post"], [{ collection := "Post", field := "to_host", value := "" }])
  , ("duplicate", ["Post"], [{ collection := "Post", field := "to_host", value := "node" },
      { collection := "Post", field := "to_host", value := "other" }]) ]

private def filterJson (filter : FieldFilter) : String :=
  "{\"collection\":" ++ jsonString filter.collection ++ ",\"field\":" ++
  jsonString filter.field ++ ",\"value\":" ++ jsonString filter.value ++
  ",\"version\":" ++ jsonString filter.version ++ "}"

private def authorityJson (field : FieldAuthority) : String :=
  "{\"collection\":" ++ jsonString field.collection ++ ",\"field\":" ++
  jsonString field.field ++ ",\"version\":" ++ jsonString field.version ++ ",\"immutable\":" ++
  (if field.immutable then "true" else "false") ++ ",\"supported\":" ++
  (if field.supported then "true" else "false") ++ "}"

private def caseJson (value : String × List String × List FieldFilter) : String :=
  let (label, collections, filters) := value
  "{\"label\":" ++ jsonString label ++ ",\"collections\":" ++ jsonStringArray collections ++
  ",\"filters\":" ++ jsonArray (filters.map filterJson) ++ ",\"authority\":" ++
  jsonArray (authority.map authorityJson) ++ ",\"valid\":" ++
  (if (validate collections authority filters).isSome then "true" else "false") ++ "}"

def casesJson : String := jsonArray (cases.map caseJson)

private def routeCases : List (String × List String × PairingMode × Option (List FieldFilter)) :=
  [ ("unfiltered", ["Post"], .unfiltered, none)
  , ("missing", ["Post"], .filtered, none)
  , ("empty", ["Post"], .filtered, some [])
  , ("valid", ["Post"], .filtered, some [{ collection := "Post", field := "to_host", value := "node" }])
  , ("mixed", ["Post", "Other"], .filtered, some [{ collection := "Post", field := "to_host", value := "node" }])
  , ("mutable", ["Post"], .filtered, some [{ collection := "Post", field := "message", value := "node" }])
  , ("unexpected", ["Post"], .unfiltered, some [{ collection := "Post", field := "to_host", value := "node" }]) ]

private def routeCaseJson (value : String × List String × PairingMode × Option (List FieldFilter)) : String :=
  let (label, collections, mode, filters) := value
  "{\"label\":" ++ jsonString label ++ ",\"mode\":" ++
    jsonString (if mode == .filtered then "filtered" else "unfiltered") ++
    ",\"filters\":" ++ (match filters with
      | none => "null"
      | some values => jsonArray (values.map filterJson)) ++
    ",\"collections\":" ++ jsonStringArray collections ++
    ",\"authority\":" ++ jsonArray (authority.map authorityJson) ++
    ",\"subscriptions\":" ++ (match resolve mode collections authority filters with
      | none => "null"
      | some values => jsonStringArray (subscriptions collections values)) ++
    ",\"accepted\":" ++
    (if (resolve mode collections authority filters).isSome then "true" else "false") ++ "}"

def routeCasesJson : String := jsonArray (routeCases.map routeCaseJson)

end Conformance.AppPairingFilters
