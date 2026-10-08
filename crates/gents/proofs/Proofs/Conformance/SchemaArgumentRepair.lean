import Proofs.PromptAssembly.SchemaArgumentRepair

namespace Conformance.SchemaArgumentRepair
open Lean PromptAssembly.SchemaArgumentRepair

private def json (raw : String) : Json :=
  match Json.parse raw with
  | .ok value => value
  | .error error => panic! error

structure Case where
  name : String
  schema : Json
  input : Json
  overrides : List ParseObservation := []

private def objectSchema := json "{\"type\":\"object\"}"
private def arraySchema := json "{\"type\":\"array\",\"items\":{\"type\":\"object\"}}"

private def configSchema := json
  "{\"type\":\"object\",\"properties\":{\"argv\":{\"type\":\"array\",\"items\":{\"type\":\"string\"}},\"set\":{\"type\":\"object\",\"additionalProperties\":true},\"options\":{\"type\":\"object\",\"additionalProperties\":true}}}"

private def configInput := json
  "{\"argv\":[\"behavior\",\"update\"],\"set\":\"{\\\"description\\\":\\\"{\\\\\\\"keep\\\\\\\":true}\\\"}\",\"options\":\"{\\\"enabled\\\":true}\"}"

private def nestedSchema := json
  "{\"type\":\"object\",\"properties\":{\"items\":{\"type\":\"array\",\"items\":{\"type\":\"object\",\"properties\":{\"values\":{\"type\":\"array\",\"items\":{\"type\":\"object\"}}}}}}}"

private def nestedInput := Json.mkObj [("items", Json.str
  (Json.arr #[Json.str (Json.mkObj [("values", Json.str
    (Json.arr #[Json.str "{\"done\":true}"]).compress)]).compress]).compress)]

private def nestedExpected := json "{\"items\":[{\"values\":[{\"done\":true}]}]}"

private def deepSchema : Nat → Json
  | 0 => objectSchema
  | n + 1 => Json.mkObj [("type", .str "object"),
      ("properties", Json.mkObj [("next", deepSchema n)])]

private def deepInput : Nat → Json
  | 0 => .str "{}"
  | n + 1 => Json.mkObj [("next", deepInput n)]

private def deepEncoded (depth : Nat) : String := (deepInput depth).compress

private def parseObserved (overrides : List ParseObservation) (raw : String) : Option Json :=
  match overrides.find? (fun observation => observation.raw == raw) with
  | some observation => observation.parsed
  | none => (Json.parse raw).toOption

private def observations (overrides : List ParseObservation) : Nat → Json → List ParseObservation
  | 0, _ => []
  | depth + 1, .str raw =>
      let parsed := parseObserved overrides raw
      ⟨raw, parsed⟩ :: (parsed.toList.flatMap (observations overrides depth))
  | depth + 1, .obj fields =>
      fields.toArray.toList.flatMap (fun entry => observations overrides depth entry.2)
  | depth + 1, .arr values => values.toList.flatMap (observations overrides depth)
  | _, _ => []

private def caseObservations (c : Case) : List ParseObservation :=
  (observations c.overrides 128 c.input).foldl (fun seen observation =>
    if seen.any (fun prior => prior.raw == observation.raw) then seen else seen ++ [observation]) []

def cases : List Case :=
  [ ⟨"config-set-and-options", configSchema, configInput, []⟩
  , ⟨"nested-arrays-and-objects", nestedSchema, nestedInput, []⟩
  , ⟨"native-object", objectSchema, json "{\"text\":\"{\\\"keep\\\":true}\"}", []⟩
  , ⟨"array-elements", arraySchema, json "[\"{}\",{},\"[]\",\"broken\"]", []⟩
  , ⟨"nullable-object", json "{\"type\":[\"object\",\"null\"]}", .str "{}", []⟩
  , ⟨"nullable-array", json "{\"type\":[\"null\",\"array\"]}", .str "[]", []⟩
  , ⟨"null-stays-null", json "{\"type\":[\"object\",\"null\"]}", .null, []⟩
  , ⟨"malformed-json", objectSchema, .str "{bad}", []⟩
  , ⟨"wrong-container", objectSchema, .str "[]", []⟩
  , ⟨"scalar-is-not-container", objectSchema, .str "123", []⟩
  , ⟨"json-null-is-not-container", objectSchema, .str "null", []⟩
  , ⟨"double-encoded-string", objectSchema, .str (Json.str "{}").compress, []⟩
  , ⟨"string-schema", json "{\"type\":\"string\"}", .str "{}", []⟩
  , ⟨"string-union", json "{\"type\":[\"object\",\"string\"]}", .str "{}", []⟩
  , ⟨"container-union", json "{\"type\":[\"object\",\"array\"]}", .str "{}", []⟩
  , ⟨"any-of", json "{\"anyOf\":[{\"type\":\"object\"},{\"type\":\"string\"}]}", .str "{}", []⟩
  , ⟨"one-of", json "{\"type\":\"object\",\"oneOf\":[{\"required\":[\"x\"]}]}", .str "{}", []⟩
  , ⟨"all-of", json "{\"type\":\"object\",\"allOf\":[{\"required\":[\"x\"]}]}", .str "{}", []⟩
  , ⟨"reference", json "{\"type\":\"object\",\"$ref\":\"#/$defs/Object\"}", .str "{}", []⟩
  , ⟨"untyped-schema", json "{}", .str "{}", []⟩
  , ⟨"untyped-children", objectSchema, json "{\"x\":\"{}\"}", []⟩
  , ⟨"typed-additional-properties", json "{\"type\":\"object\",\"additionalProperties\":{\"type\":\"array\"}}",
      json "{\"a/b~c\":\"[]\",\"invalid\":\"false\"}", []⟩
  , ⟨"typed-validation-still-required", json "{\"type\":\"object\",\"properties\":{\"payload\":{\"type\":\"object\",\"required\":[\"count\"],\"properties\":{\"count\":{\"type\":\"integer\"}}}}}",
      json "{\"payload\":\"{\\\"count\\\":\\\"wrong\\\"}\"}", []⟩
  , ⟨"wrapped-last-repairable-depth", deepSchema 30, deepInput 30, []⟩
  , ⟨"direct-last-repairable-depth", deepSchema 31, deepInput 31, []⟩
  , ⟨"direct-first-preserved-depth", deepSchema 32, deepInput 32, []⟩
  , ⟨"depth-bound", deepSchema 34, deepInput 34, []⟩
  , { name := "unicode-surrogate-pair", schema := objectSchema,
      input := .str "{\"text\":\"\\uD83D\\uDE00\"}",
      overrides := [⟨"{\"text\":\"\\uD83D\\uDE00\"}", some (Json.mkObj [("text", .str (String.singleton (Char.ofNat 0x1F600)))])⟩] }
  , { name := "unpaired-high-surrogate", schema := objectSchema,
      input := .str "{\"text\":\"\\uD83D\"}",
      overrides := [⟨"{\"text\":\"\\uD83D\"}", none⟩] }
  , { name := "unpaired-low-surrogate", schema := objectSchema,
      input := .str "{\"text\":\"\\uDE00\"}",
      overrides := [⟨"{\"text\":\"\\uDE00\"}", none⟩] }
  , ⟨"literal-backslash-u", objectSchema, .str "{\"text\":\"\\\\uD83D\\\\uDE00\"}", []⟩
  , { name := "number-overflow", schema := objectSchema,
      input := .str "{\"number\":1e400}", overrides := [⟨"{\"number\":1e400}", none⟩] }
  , { name := "native-parser-depth-limit", schema := objectSchema,
      input := .str (deepEncoded 130), overrides := [⟨deepEncoded 130, none⟩] } ]

private def callSchema (c : Case) : Json := Json.mkObj
  [("type", .str "object"), ("properties", Json.mkObj [("value", c.schema)]),
   ("required", .arr #[.str "value"]), ("additionalProperties", .bool false)]

private def callInput (c : Case) : Json := Json.mkObj [("value", c.input)]

theorem nested_containers_are_repaired :
    ((run (observations [] 128 nestedInput) nestedSchema nestedInput).value == nestedExpected) = true := by native_decide

theorem config_payload_strings_are_preserved :
    (((run (observations [] 128 configInput) configSchema configInput).value.getObjValD "set" |>.getObjValD "description") ==
      Json.str "{\"keep\":true}") = true := by native_decide

theorem generated_cases_are_idempotent :
    cases.all (fun c =>
      let parsed := caseObservations c
      let once := run parsed c.schema c.input
      let twice := run parsed c.schema once.value
      twice.value == once.value && twice.paths.isEmpty) = true := by native_decide

private def caseJson (c : Case) : Json :=
  let parsed := caseObservations c
  let result := run parsed c.schema c.input
  let native := run parsed (callSchema c) (callInput c)
  Json.mkObj [("name", toJson c.name), ("schema", c.schema), ("input", c.input),
    ("expected", result.value), ("paths", toJson result.paths),
    ("parses", toJson (parsed.map (fun observation => Json.mkObj
      [("raw", toJson observation.raw), ("accepted", toJson observation.parsed.isSome),
       ("parsed", toJson observation.parsed)]))),
    ("native_schema", callSchema c), ("native_input", callInput c),
    ("native_expected", native.value), ("native_paths", toJson native.paths)]

def casesJson : String := (toJson (cases.map caseJson)).compress

end Conformance.SchemaArgumentRepair
