import Lean

/-! A conformance family is a model function replayed on every input of its
finite domain. Its key, wire shape, JSON and snapshot entry are derived from
the function's own name and signature. -/

namespace Conformance

open Lean

/-- The wire shape of a family type. An enum carries each constructor with its
wire name: the name its `conformance_vocabulary` function gives it, or
`wireName` of the constructor when it has none. A struct carries each field's
Lean name with its `wireName`, computed when the family elaborates: the
generator encodes every case of every family, and recomputing the name per
field and case dominated its interpreted run. -/
inductive Schema where
  | bool
  | nat
  | int
  | string
  | json
  | option (item : Schema)
  | list (item : Schema)
  | enum (name : String) (values : List (String × String))
  | struct (name : String) (fields : List (String × String × Schema))

/-- A family whose inputs are chosen by hand because its domain is not finite.
Its key, wire shape, JSON and snapshot entry are derived like a function's. -/
structure Family (ι ο : Type) where
  inputs : List ι
  model : ι → ο

/-- camelCase to snake_case; a run of capitals is one word (`MCPHealth` is
`mcp_health`). -/
def wireName (leanName : String) : String :=
  let chars := leanName.toList
  let rec go (previous : Option Char) : List Char → List Char
    | [] => []
    | c :: rest =>
      let startsWord := c.isUpper && (match previous with
        | none => false
        | some p => p.isLower || p.isDigit || (p.isUpper && (rest.head?.map Char.isLower).getD false))
      (if startsWord then ['_', c.toLower] else [c.toLower]) ++ go (some c) rest
  String.mk (go none chars)

def wireKey (declaration : Name) : String :=
  "_".intercalate (declaration.components.map fun part => wireName part.toString)

partial def Schema.wire : Schema → Json
  | .bool => "bool"
  | .nat => "nat"
  | .int => "int"
  | .string => "string"
  | .json => "json"
  | .option item => Json.mkObj [("option", item.wire)]
  | .list item => Json.mkObj [("list", item.wire)]
  | .enum name values =>
      Json.mkObj [("enum", name), ("values", Json.arr (values.map (Json.str ·.2)).toArray)]
  | .struct name fields =>
      Json.mkObj [("struct", name), ("fields", Json.arr (fields.map fun (_, wire, schema) =>
        Json.arr #[Json.str wire, schema.wire]).toArray)]

/-- A number in a `json` field crosses the wire as itself when it is an
integer within `i64` or `u64`, and otherwise as `{"$number": token}`. Lean
prints a `JsonNumber` normalised (`1.0` as `1`), while families that carry raw
wire values must tell `1` from `1.0`; native decoders also read these
integers exactly but other numbers only as `f64`. The token is the exact
decimal: the integer part, then exactly `exponent` fraction digits (`⟨10, 1⟩`
is `1.0`). Two numbers are therefore equal when both are such integers of
equal value, and otherwise only when their tokens are equal, so `1` and `1.0`
differ. An object whose only key is `$number` is reserved for this encoding. -/
def jsonNumberWire (number : JsonNumber) : Json :=
  let integer := number.exponent == 0 &&
    decide (-(2 ^ 63 : Int) ≤ number.mantissa) && decide (number.mantissa < (2 ^ 64 : Int))
  if integer then .num number else
    let scale := 10 ^ number.exponent
    let magnitude := number.mantissa.natAbs
    let fraction := toString (magnitude % scale)
    let fraction := if number.exponent == 0 then "" else
      "." ++ String.mk (List.replicate (number.exponent - fraction.length) '0') ++ fraction
    let sign := if number.mantissa < 0 then "-" else ""
    Json.mkObj [("$number", .str (sign ++ toString (magnitude / scale) ++ fraction))]

partial def jsonWire : Json → Json
  | .num number => jsonNumberWire number
  | .arr items => .arr (items.map jsonWire)
  | .obj fields => Json.mkObj (fields.toArray.toList.map fun entry => (entry.1, jsonWire entry.2))
  | other => other

/-- Rewrites derived `ToJson` output to wire names. Derived `ToJson` omits an
absent optional field; here it is an explicit `null`, so a decoder can require
every field. -/
partial def Schema.encode : Schema → Json → Json
  | .option item, value => if value.isNull then value else item.encode value
  | .list item, .arr values => Json.arr (values.map item.encode)
  | .json, value => jsonWire value
  | .enum _ values, .str value => Json.str ((values.lookup value).getD value)
  | .struct _ fields, value =>
      Json.mkObj (fields.map fun (field, wire, schema) =>
        (wire, schema.encode ((value.getObjVal? field).toOption.getD Json.null)))
  | _, value => value

open Meta Elab Term

private def enumConstructors (type : Expr) : MetaM (Name × List Name) := do
  let .const name _ := type | throwError "conformance type {type} is not a finite enum"
  let some (.inductInfo induct) := (← getEnv).find? name
    | throwError "conformance type {name} is not a finite enum"
  for constructor in induct.ctors do
    let some (.ctorInfo info) := (← getEnv).find? constructor
      | throwError "missing constructor {constructor}"
    unless info.numFields == 0 do
      throwError "conformance enum {name} has a constructor with fields: {constructor}"
  pure (name, induct.ctors)

initialize vocabularyExtension :
    SimplePersistentEnvExtension (Name × Name) (Array (Name × Name)) ←
  registerSimplePersistentEnvExtension
    { addEntryFn := Array.push
    , addImportedFn := fun imported => imported.foldl (· ++ ·) #[] }

syntax (name := conformanceVocabulary) "conformance_vocabulary" : attr

initialize registerBuiltinAttribute {
  name := `conformanceVocabulary
  descr := "this function gives each constructor of its enum the wire name production uses"
  add := fun declaration _ _ => do
    let .forallE _ (.const type _) (.const ``String _) _ := (← getConstInfo declaration).type
      | throwError "a conformance vocabulary maps an enum to String: {declaration}"
    modifyEnv (vocabularyExtension.addEntry · (type, declaration)) }

private def enumSchemaTerm (name : Name) (constructors : List Name) : MetaM (TSyntax `term) := do
  let vocabulary := (vocabularyExtension.getState (← getEnv)).find? (·.1 == name)
  let values ← constructors.toArray.mapM fun constructor => do
    let wire ← match vocabulary with
      | some (_, function) => `($(mkIdent function) $(mkIdent constructor))
      | none => pure (quote (wireName constructor.getString!))
    `(($(quote constructor.getString!), $wire))
  `(Schema.enum $(quote name.getString!) [$values,*])

private partial def schemaTerm (type : Expr) : MetaM (TSyntax `term) := do
  let type ← whnfR type
  match type with
  | .const ``Bool _ => `(Schema.bool)
  | .const ``Nat _ => `(Schema.nat)
  | .const ``Int _ => `(Schema.int)
  | .const ``String _ => `(Schema.string)
  | .const ``Json _ => `(Schema.json)
  | .app (.const ``Option _) item => do `(Schema.option $(← schemaTerm item))
  | .app (.const ``List _) item => do `(Schema.list $(← schemaTerm item))
  | .const name _ =>
      if isStructure (← getEnv) name then
        let constructor := getStructureCtor (← getEnv) name
        let fields ← forallTelescope constructor.type fun parameters _ =>
          parameters.mapM fun parameter => do
            let field := (← parameter.fvarId!.getUserName).toString
            `(($(quote field), $(quote (wireName field)), $(← schemaTerm (← inferType parameter))))
        `(Schema.struct $(quote name.getString!) [$fields,*])
      else
        let (name, constructors) ← enumConstructors type
        enumSchemaTerm name constructors
  | other => throwError "conformance type {other} has no wire shape"

/-- Every value of a finite type, in declaration order. -/
private def allTerm (type : Expr) : MetaM (TSyntax `term) := do
  let type ← whnfR type
  if type.isConstOf ``Bool then return ← `([false, true])
  let (_, constructors) ← enumConstructors type
  `([$(constructors.toArray.map mkIdent),*])

initialize functionExtension : SimplePersistentEnvExtension Name (Array Name) ←
  registerSimplePersistentEnvExtension
    { addEntryFn := Array.push
    , addImportedFn := fun imported => imported.foldl (· ++ ·) #[] }

syntax (name := conformanceFunction) "conformance_function" : attr

initialize registerBuiltinAttribute {
  name := `conformanceFunction
  descr := "replays this model function against its production owner"
  add := fun declaration _ _ => modifyEnv (functionExtension.addEntry · declaration) }

/-- One family: the function's parameters are the input fields, its result is
the expectation, and the cases are its whole domain. -/
private def familyTerm (declaration : Name) : TermElabM (TSyntax `term) := do
  forallTelescope (← getConstInfo declaration).type fun parameters result => do
    -- An equation-style definition leaves its parameters unnamed; such a
    -- parameter takes the name of its type.
    let names ← parameters.mapM fun parameter => do
      let name ← parameter.fvarId!.getUserName
      unless name.hasMacroScopes do return name
      let .const type _ ← whnfR (← inferType parameter)
        | throwError "{declaration}: an unnamed parameter needs a named type"
      let typeName := type.getString!
      pure (Name.mkSimple (typeName.take 1 |>.toLower |>.append (typeName.drop 1)))
    unless names.toList.Nodup do
      throwError "{declaration}: parameters {names} do not have distinct names"
    let idents := names.map mkIdent
    let fields ← parameters.mapIdxM fun index parameter => do
      let field := names[index]!.toString
      `(($(quote field), $(quote (wireName field)), $(← schemaTerm (← inferType parameter))))
    let values ← idents.mapIdxM fun index ident =>
      `(($(quote names[index]!.toString), toJson $ident))
    let input ← `(Schema.struct "Input" [$fields,*])
    let output ← schemaTerm result
    let mut cases ← `([Json.mkObj
      [ ("input", Schema.encode $input (Json.mkObj [$values,*]))
      , ("expected", Schema.encode $output (toJson ($(mkIdent declaration) $idents*))) ]])
    for index in (List.range parameters.size).reverse do
      cases ← `(List.flatMap (fun $(idents[index]!) => $cases)
        $(← allTerm (← inferType parameters[index]!)))
    `(Json.mkObj
      [ ("input", Schema.wire $input)
      , ("output", Schema.wire $output)
      , ("cases", Json.arr (List.toArray $cases)) ])

def Family.json {ι ο : Type} [ToJson ι] [ToJson ο]
    (input output : Schema) (family : Family ι ο) : Json :=
  Json.mkObj
    [ ("input", input.wire)
    , ("output", output.wire)
    , ("cases", Json.arr (family.inputs.map fun i => Json.mkObj
        [ ("input", input.encode (toJson i))
        , ("expected", output.encode (toJson (family.model i))) ]).toArray) ]

initialize familyExtension : SimplePersistentEnvExtension Name (Array Name) ←
  registerSimplePersistentEnvExtension
    { addEntryFn := Array.push
    , addImportedFn := fun imported => imported.foldl (· ++ ·) #[] }

syntax (name := conformanceFamily) "conformance_family" : attr

initialize registerBuiltinAttribute {
  name := `conformanceFamily
  descr := "replays this hand-chosen family against its production owner"
  add := fun declaration _ _ => modifyEnv (familyExtension.addEntry · declaration) }

private def explicitFamilyTerm (declaration : Name) : TermElabM (TSyntax `term) := do
  let type ← whnfR (← getConstInfo declaration).type
  let (``Family, #[input, output]) := type.getAppFnArgs
    | throwError "{declaration} is not a conformance family"
  `(Family.json $(← schemaTerm input) $(← schemaTerm output) $(mkIdent declaration))

/-- The snapshot entries of every `conformance_function` and
`conformance_family` in the import closure. A family's key drops a leading
`Conformance` namespace. -/
elab "conformance_families%" : term => do
  let declarations := (functionExtension.getState (← getEnv)).qsort Name.lt
  let functions ← declarations.mapM fun declaration => do
    `(($(quote (wireKey declaration)), $(← familyTerm declaration)))
  let explicit ← ((familyExtension.getState (← getEnv)).qsort Name.lt).mapM fun declaration => do
    let key := wireKey (declaration.replacePrefix `Conformance .anonymous)
    `(($(quote key), $(← explicitFamilyTerm declaration)))
  let entries := functions ++ explicit
  elabTerm (← `(Json.mkObj [$entries,*])) (some (mkConst ``Json))

/-- The wire names of every enum with a `conformance_vocabulary`. The key ends
in `_vocabulary`: `ManagedExec.StopOutcome` and `ManagedExec.stopOutcome` would
otherwise share one. -/
elab "conformance_vocabularies%" : term => do
  let vocabularies := (vocabularyExtension.getState (← getEnv)).qsort (Name.lt ·.1 ·.1)
  let entries ← vocabularies.mapM fun (type, _) => do
    let (name, constructors) ← enumConstructors (mkConst type)
    `(($(quote (wireKey type ++ "_vocabulary")), Schema.wire $(← enumSchemaTerm name constructors)))
  elabTerm (← `(Json.mkObj [$entries,*])) (some (mkConst ``Json))

end Conformance
