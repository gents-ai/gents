import Lean

namespace Compaction.Checkpoint
open Lean

def listFields : List String :=
  ["constraints_and_preferences", "completed_work", "in_progress", "blockers",
   "current_work", "key_decisions", "errors_and_fixes", "verification",
   "uncertainties", "next_actions", "critical_context"]

/-- A plain-JSON provider may encode one list item as a string. This is a
representation repair only: preserve its exact text and reject other scalar
types or mixed arrays. Guided decoding retains the canonical array schema. -/
def stringList : Json → Option (List String)
  | .str text => some [text]
  | .arr items => items.toList.mapM (fun item => item.getStr?.toOption)
  | _ => none

theorem scalar_text_is_preserved (text : String) :
    stringList (.str text) = some [text] := rfl

theorem null_is_not_an_empty_list : stringList .null = none := rfl

/-- Rust's str::trim uses Unicode White_Space, whereas Lean's String.trim
recognizes only four ASCII characters. This set is the native classification
premise: https://doc.rust-lang.org/std/primitive.char.html#method.is_whitespace. -/
def blank (text : String) : Bool := text.toList.all fun character =>
  let code := character.val.toNat
  (9 ≤ code && code ≤ 13) || (0x2000 ≤ code && code ≤ 0x200A) ||
    [0x20, 0x85, 0xA0, 0x1680, 0x2028, 0x2029, 0x202F, 0x205F, 0x3000].contains code

def decode (fields : List (String × Json)) : Option Json := do
  let names := fields.map Prod.fst
  if names.eraseDups.length != names.length then none else do
  if names.any (fun name => name != "goal" && !(listFields.contains name)) then none else do
  let goal ← (fields.lookup "goal").bind (fun value => value.getStr?.toOption)
  if blank goal then none else do
  let lists ← listFields.mapM fun name => do
    let values ← match fields.lookup name with
      | none => some []
      | some value => stringList value
    pure (name, values)
  let next := (lists.lookup "next_actions").getD []
  if next.all blank then none else
    some (Json.mkObj (("goal", .str goal) :: lists.map (fun (name, values) => (name, toJson values))))

theorem absent_goal_is_rejected (fields : List (String × Json))
    (h : fields.lookup "goal" = none) : decode fields = none := by
  simp [decode, h]

theorem duplicate_goals_are_rejected :
    decode [("goal", .str "first"), ("goal", .str "second"),
      ("next_actions", .str "continue")] = none := by rfl

end Compaction.Checkpoint
