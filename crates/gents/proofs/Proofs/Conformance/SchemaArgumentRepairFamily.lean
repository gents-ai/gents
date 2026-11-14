import Proofs.Session.PromptAssembly.SchemaArgumentRepair
import Proofs.Conformance.Family

namespace Conformance.SchemaArgumentRepair
open Lean PromptAssembly.SchemaArgumentRepair

private def parsed (raw : String) : Json :=
  match Json.parse raw with
  | .ok value => value
  | .error error => panic! error

/-- Production decodes argument strings with its own JSON parser; these cases
use only text on which it and `Json.parse` agree. -/
private def parses : Nat → Json → List ParseObservation
  | 0, _ => []
  | depth + 1, .str raw =>
      let value := (Json.parse raw).toOption
      ⟨raw, value⟩ :: value.toList.flatMap (parses depth)
  | depth + 1, .obj fields => fields.toArray.toList.flatMap fun entry => parses depth entry.2
  | depth + 1, .arr values => values.toList.flatMap (parses depth)
  | _, _ => []

structure RepairInput where
  name : String
  schema : Json
  arguments : Json
  deriving ToJson

structure Repaired where
  arguments : Json
  paths : List String
  deriving ToJson

private def objectProperty (key : String) (schema : String) : Json :=
  parsed ("{\"type\":\"object\",\"properties\":{\"" ++ key ++ "\":" ++ schema ++ "}}")

private def nestedArguments : Json :=
  Json.mkObj [("config", .str (Json.mkObj [("steps", Json.arr
    #[.str (parsed r#"{"n":-3}"#).compress, parsed r#"{"keep":null}"#])]).compress)]

@[conformance_family]
def arguments : Family RepairInput Repaired where
  inputs :=
    [ ⟨"nested_objects_and_arrays",
        objectProperty "config"
          r#"{"type":"object","properties":{"steps":{"type":"array","items":{"type":"object"}}}}"#,
        nestedArguments⟩
    , ⟨"null_values",
        parsed r#"{"type":"object","additionalProperties":{"type":["object","null"]}}"#,
        parsed r#"{"absent":null,"present":"{\"inner\":null}","text":"null"}"#⟩
    , ⟨"negative_integers", objectProperty "limits" r#"{"type":"object"}"#,
        parsed r#"{"offset":-42,"limits":"{\"min\":-9223372036854775808,\"step\":-1}"}"#⟩
    , ⟨"integer_one_and_decimal_one", objectProperty "parsed" r#"{"type":"object"}"#,
        parsed r#"{"whole":1,"decimal":1.0,"parsed":"{\"whole\":1,\"decimal\":1.0}"}"#⟩
    , ⟨"large_integers", objectProperty "ids" r#"{"type":"array"}"#,
        parsed r#"{"max":18446744073709551615,"ids":"[9007199254740993,-9223372036854775808,18446744073709551615]"}"#⟩
    , ⟨"decimal_fractions", objectProperty "ratio" r#"{"type":"object"}"#,
        parsed r#"{"fixed":12.75,"ratio":"{\"value\":-2.5,\"scale\":0.125,\"half\":-0.5}"}"#⟩
    , ⟨"array_root", parsed r#"{"type":"array","items":{"type":"object"}}"#,
        parsed r#"["{\"x\":[]}",{},"[]","null",[1,1.0]]"#⟩
    , ⟨"empty_containers",
        parsed r#"{"type":"object","additionalProperties":{"type":["array","null"]}}"#,
        parsed r#"{"a":"[]","b":"[[],{}]","c":null,"d":{}}"#⟩
    ]
  model := fun input =>
    let result := run (parses 128 input.arguments) input.schema input.arguments
    ⟨result.value, result.paths⟩

example : arguments.inputs.length = 8 := by native_decide

example : (jsonWire (parsed "[1,1.0,-0.5,-9223372036854775808,18446744073709551616]")).compress =
    r#"[1,{"$number":"1.0"},{"$number":"-0.5"},-9223372036854775808,{"$number":"18446744073709551616"}]"# := by
  native_decide

example : (run (parses 128 (parsed r#"{"parsed":"{\"decimal\":1.0}"}"#))
    (objectProperty "parsed" r#"{"type":"object"}"#)
    (parsed r#"{"parsed":"{\"decimal\":1.0}"}"#)).paths = ["/parsed"] := by native_decide

end Conformance.SchemaArgumentRepair
