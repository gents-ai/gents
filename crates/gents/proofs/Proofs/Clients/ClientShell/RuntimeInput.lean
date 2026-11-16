import Lean

/-! Output-obligation reminders have a request-bound authored key. Their
classification does not inspect the model-facing text. -/
namespace ClientShell.RuntimeInput

def outputObligation (user : Bool) (request : Option String) (key : String) : Bool :=
  match request with
  | none => false
  | some doc =>
      let stem := "authored:" ++ doc ++ ":output-obligation:"
      if !user || doc.isEmpty || !key.startsWith stem then false
      else
        let suffix := key.drop stem.length
        match suffix.toNat? with
        | none => false
        | some n => n ≤ 4294967295 && toString n == suffix

example : outputObligation true (some "doc") "authored:doc:output-obligation:0" = true := by native_decide
example : outputObligation true (some "other") "authored:doc:output-obligation:0" = false := by native_decide
example : outputObligation false (some "doc") "authored:doc:output-obligation:0" = false := by native_decide
example : outputObligation true (some "doc") "authored:doc:output-obligation:01" = false := by native_decide

end ClientShell.RuntimeInput
