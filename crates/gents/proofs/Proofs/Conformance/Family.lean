import Lean

/-! A conformance family is a model function replayed on every input of its
finite domain. Its key, wire shape, JSON and snapshot entry are derived from
the function's own name and signature. -/

namespace Conformance

open Lean

/-- The wire shape of a family type. An enum carries each constructor with its
wire name: the name its `conformance_vocabulary` function gives it, or
`wireName` of the constructor when it has none. -/
inductive Schema where
  | bool
  | option (item : Schema)
  | enum (name : String) (values : List (String × String))
  | struct (name : String) (fields : List (String × Schema))

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
  | .option item => Json.mkObj [("option", item.wire)]
  | .enum name values =>
      Json.mkObj [("enum", name), ("values", Json.arr (values.map (Json.str ·.2)).toArray)]
  | .struct name fields =>
      Json.mkObj [("struct", name), ("fields", Json.arr (fields.map fun (field, schema) =>
        Json.arr #[Json.str (wireName field), schema.wire]).toArray)]

/-- Rewrites derived `ToJson` output to wire names. -/
partial def Schema.encode : Schema → Json → Json
  | .option _, .null => .null
  | .option item, value => item.encode value
  | .enum _ values, .str value => Json.str ((values.lookup value).getD value)
  | .struct _ fields, value =>
      Json.mkObj (fields.map fun (field, schema) =>
        (wireName field, schema.encode ((value.getObjVal? field).toOption.getD Json.null)))
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
      | none => `(wireName $(quote constructor.getString!))
    `(($(quote constructor.getString!), $wire))
  `(Schema.enum $(quote name.getString!) [$values,*])

private partial def schemaTerm (type : Expr) : MetaM (TSyntax `term) := do
  let type ← whnfR type
  if type.isConstOf ``Bool then return ← `(Schema.bool)
  if type.isAppOfArity ``Option 1 then
    return ← `(Schema.option $(← schemaTerm type.appArg!))
  let (name, constructors) ← enumConstructors type
  enumSchemaTerm name constructors

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
      `(($(quote names[index]!.toString), $(← schemaTerm (← inferType parameter))))
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

/-- The snapshot entries of every `conformance_function` in the import closure. -/
elab "conformance_families%" : term => do
  let declarations := (functionExtension.getState (← getEnv)).qsort Name.lt
  let entries ← declarations.mapM fun declaration => do
    `(($(quote (wireKey declaration)), $(← familyTerm declaration)))
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
