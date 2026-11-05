import Lean

namespace Identity.OptimizationPromotion
open Lean

/-- The Node DID selects the job's scope; the authenticated transaction actor
owns journal provenance. Native target and journal update permissions and the
journal compare-and-set must all succeed before either write commits. -/
structure State where
  target : String
  journal : List (String × String)
  deriving Repr, DecidableEq

def transition (before : State) (actor : Option String) (action target : String)
    (targetAllowed journalAllowed expectedLength commit : Bool) : State :=
  match actor with
  | none => before
  | some principal =>
      if targetAllowed && journalAllowed && expectedLength && commit then
        ⟨target, before.journal ++ [(action, principal)]⟩
      else before

theorem denied_journal_rolls_back_target (s : State) (actor : Option String)
    (action target : String) (targetAllowed expectedLength commit : Bool) :
    transition s actor action target targetAllowed false expectedLength commit = s := by
  cases actor <;> simp [transition]

theorem stale_journal_rolls_back_target (s : State) (actor : Option String)
    (action target : String) (targetAllowed journalAllowed commit : Bool) :
    transition s actor action target targetAllowed journalAllowed false commit = s := by
  cases actor <;> simp [transition]

theorem committed_transition_records_the_actual_principal (s : State) (actor action target : String) :
    transition s (some actor) action target true true true true =
      ⟨target, s.journal ++ [(action, actor)]⟩ := by rfl

theorem no_actor_leaves_no_record (s : State) (action target : String)
    (targetAllowed journalAllowed expectedLength commit : Bool) :
    transition s none action target targetAllowed journalAllowed expectedLength commit = s := by rfl

structure Case where
  name : String
  actor : Option String
  action : String
  targetAllowed : Bool
  journalAllowed : Bool
  expectedLength : Bool
  commit : Bool

def cases : List Case :=
  [ ⟨"held-user-promote", some "user", "promoted", true, true, true, true⟩
  , ⟨"held-user-revert", some "user", "reverted", true, true, true, true⟩
  , ⟨"reader-cannot-update-job", some "reader", "promoted", true, false, true, true⟩
  , ⟨"reader-cannot-update-target", some "reader", "promoted", false, true, true, true⟩
  , ⟨"stale-journal", some "user", "promoted", true, true, false, true⟩
  , ⟨"commit-failed", some "user", "promoted", true, true, true, false⟩
  , ⟨"missing-actor", none, "promoted", true, true, true, true⟩ ]

def caseJson (c : Case) : Json :=
  let before : State := ⟨"before", []⟩
  let after := transition before c.actor c.action "after" c.targetAllowed c.journalAllowed c.expectedLength c.commit
  Json.mkObj [("name", toJson c.name), ("actor", toJson c.actor), ("action", toJson c.action),
    ("target_allowed", toJson c.targetAllowed), ("journal_allowed", toJson c.journalAllowed),
    ("expected_length", toJson c.expectedLength), ("commit", toJson c.commit),
    ("expected_target", toJson after.target), ("expected_journal", toJson after.journal)]

def casesJson : String := (toJson (cases.map caseJson)).compress
end Identity.OptimizationPromotion
