import Lean
import Proofs.Basic

namespace PromptAssembly.SchemaArgumentRepair
open Lean

inductive Container where
  | object
  | array
  deriving BEq, DecidableEq, Repr

/-- References, alternatives and conditional schemas need a schema evaluator.
The repair boundary does not choose an alternative or reinterpret a schema
that admits strings. Native validation remains authoritative after repair. -/
def unsupported : List String :=
  ["$ref", "$dynamicRef", "anyOf", "oneOf", "allOf", "not", "if", "then", "else",
   "prefixItems", "patternProperties", "dependentSchemas", "unevaluatedProperties"]

def typeContainer : Json → Option Container
  | .str "object" => some .object
  | .str "array" => some .array
  | .arr types =>
      let concrete := types.toList.filter (· != Json.str "null")
      match concrete with
      | [.str "object"] => some .object
      | [.str "array"] => some .array
      | _ => none
  | _ => none

def container (schema : Json) : Option Container :=
  if unsupported.any (fun key => (schema.getObjVal? key).isOk) then none
  else typeContainer (schema.getObjValD "type")

def hasContainer : Container → Json → Bool
  | .object, .obj _ => true
  | .array, .arr _ => true
  | _, _ => false

/-- Strict parsing and JSON representation belong to the native decoder. Each
observation records one raw string and that decoder's result; conformance checks
these observations against serde_json before replaying this owner. Lean's JSON
parser is not an assumption about native Unicode, numeric or nesting behavior. -/
structure ParseObservation where
  raw : String
  parsed : Option Json

def parsedValue (observations : List ParseObservation) (raw : String) : Option Json :=
  (observations.find? (fun observation => observation.raw == raw)).bind (·.parsed)

/-- One observed parse, only when the schema requires its resulting container.
A string containing a JSON string is never decoded twice. -/
def decode (observations : List ParseObservation) (kind : Container) (value : Json) : Json × Bool :=
  match value with
  | .str raw =>
      match parsedValue observations raw with
      | some parsed => if hasContainer kind parsed then (parsed, true) else (value, false)
      | none => (value, false)
  | _ => (value, false)

def pointerPart (key : String) : String :=
  key.replace "~" "~0" |>.replace "/" "~1"

structure Result where
  value : Json
  paths : List String := []

def propertySchema (schema : Json) (key : String) : Json :=
  match (schema.getObjValD "properties").getObjVal? key with
  | .ok child => child
  | .error _ => schema.getObjValD "additionalProperties"

/-- Depth is bounded at the provider/tool boundary, independently of JSON
parser limits. A subtree beyond the bound remains unchanged and goes through
the tool's ordinary validation. Only declared property and homogeneous item
schemas authorize recursive repair; untyped payloads remain opaque. -/
def repair (observations : List ParseObservation) : Nat → Json → Json → String → Result
  | 0, _, value, _ => ⟨value, []⟩
  | depth + 1, schema, value, path =>
      match container schema with
      | none => ⟨value, []⟩
      | some kind =>
          let (decoded, changed) := decode observations kind value
          let ownPaths := if changed then [path] else []
          match kind, decoded with
          | .object, .obj fields =>
              let children := fields.toArray.toList.map fun entry =>
                let child := repair observations depth (propertySchema schema entry.1) entry.2
                  (path ++ "/" ++ pointerPart entry.1)
                ((entry.1, child.value), child.paths)
              ⟨Json.mkObj (children.map Prod.fst), ownPaths ++ children.flatMap Prod.snd⟩
          | .array, .arr values =>
              let children := values.toList.zipIdx |>.map fun (value, index) =>
                repair observations depth (schema.getObjValD "items") value (path ++ "/" ++ toString index)
              ⟨.arr (children.map Result.value).toArray,
                ownPaths ++ children.flatMap Result.paths⟩
          | _, _ => ⟨decoded, ownPaths⟩

def maxDepth : Nat := 32

def run (observations : List ParseObservation) (schema value : Json) : Result :=
  repair observations maxDepth schema value ""

theorem unsupported_schema_unchanged (observations : List ParseObservation) (depth : Nat) (schema value : Json) (path : String)
    (h : container schema = none) :
    (repair observations depth schema value path).value = value ∧
      (repair observations depth schema value path).paths = [] := by
  cases depth <;> simp [repair, h]

theorem no_repair_beyond_depth (observations : List ParseObservation) (schema value : Json) (path : String) :
    repair observations 0 schema value path = ⟨value, []⟩ := rfl

theorem decode_only_required_container (observations : List ParseObservation) (kind : Container) (value : Json)
    (h : (decode observations kind value).2 = true) :
    hasContainer kind (decode observations kind value).1 = true := by
  cases value with
  | str raw =>
      cases parsed : parsedValue observations raw with
      | none => simp [decode, parsed] at h
      | some decoded =>
          simp only [decode, parsed] at h ⊢
          split at h <;> simp_all
  | _ => simp [decode] at h

theorem native_object_is_not_decoded (observations : List ParseObservation) (kind : Container) (fields : RBNode String (fun _ => Json)) :
    decode observations kind (.obj fields) = (.obj fields, false) := rfl

theorem native_array_is_not_decoded (observations : List ParseObservation) (kind : Container) (values : Array Json) :
    decode observations kind (.arr values) = (.arr values, false) := rfl

end PromptAssembly.SchemaArgumentRepair
