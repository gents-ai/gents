import Mathlib.Data.List.Basic
namespace ToolPolicy.FieldRead
/-- DID/ACP and existing filters authorize the native read; document and field
checks only narrow it. Widths represent UTF-8 scalars. SHA-256 exact-byte
comparison relies on collision resistance, not a retained snapshot. -/
def page (widths : List Nat) (offset budget : Nat)
    (authorized declared exact hashPresent hashMatches : Bool) : Option Nat := Id.run do
  let boundaries := widths.scanl (· + ·) 0
  if !authorized || !declared || !exact || (offset > 0 && !hashPresent) ||
      (hashPresent && !hashMatches) || offset > widths.sum || !boundaries.contains offset then
    return none
  return some ((boundaries.filter (fun n => n >= offset && n <= offset + budget)).foldl max offset)
theorem unauthorized_rejected (widths : List Nat) (offset budget : Nat)
    (declared exact present hashOk : Bool) :
    page widths offset budget false declared exact present hashOk = none := by
  simp [page, Id.run]
theorem undeclared_rejected (widths : List Nat) (offset budget : Nat)
    (authorized exact present hashOk : Bool) :
    page widths offset budget authorized false exact present hashOk = none := by
  simp [page, Id.run]
theorem wrong_document_rejected (widths : List Nat) (offset budget : Nat)
    (authorized declared present hashOk : Bool) :
    page widths offset budget authorized declared false present hashOk = none := by
  simp [page, Id.run]
theorem changed_value_rejected (widths : List Nat) (offset budget : Nat)
    (authorized declared exact : Bool) :
    page widths offset budget authorized declared exact true false = none := by
  simp [page, Id.run]
end ToolPolicy.FieldRead
