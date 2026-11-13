import Lean

namespace CompletionParameters.Merge
open Lean

partial def objects (left right : Json) : Json :=
  match left, right with
  | .obj left, .obj right =>
    .obj (right.fold (fun result key value =>
      result.insert compare key (match result.find compare key with
        | some previous => objects previous value
        | none => value)) left)
  | _, right => right

structure Input where
  left : Option String
  right : Option String
  deriving ToJson

/-- Additional parameters merge recursively only at objects. The later scalar,
array or null replaces the earlier value; a missing parameter set is neutral. -/
def merge (input : Input) : Option String :=
  let parse (value : String) := (Json.parse value).toOption.getD Json.null
  match input.left, input.right with
  | none, none => none
  | some value, none | none, some value => some (parse value).compress
  | some left, some right => some (objects (parse left) (parse right)).compress

end CompletionParameters.Merge
