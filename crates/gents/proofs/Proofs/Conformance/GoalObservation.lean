import Proofs.GoalAutomation.Observation
import Proofs.GoalAutomation.ObservationRecovery
import Proofs.Conformance.ContractTypes

namespace Conformance.GoalObservationContracts

open GoalAutomation.Observation Conformance.Contracts

def current : Scope := ⟨"node", "session", "session-document", some "requester"⟩
def goal : GoalReference := ⟨"node", "session", "goal-document", "goal-commit"⟩

structure Case where
  actor : String
  scope : Scope
  unique : Bool
  goal : Option GoalReference
  verified : Bool
  deriving DecidableEq, Repr

def cases : List Case :=
  ["node", "foreign-node"].flatMap fun actor =>
  [true, false].flatMap fun unique =>
  [true, false].flatMap fun verified =>
  [current, { current with node := "foreign-node" },
    { current with session := "foreign-session" },
    { current with sessionDocument := "recreated-document" },
    { current with requester := some "foreign-requester" },
    { current with requester := none }].flatMap fun scope =>
  [none, some goal, some { goal with node := "foreign-node" },
    some { goal with session := "foreign-session" },
    some { goal with document := "" }, some { goal with composite := "" }].map fun goal =>
      ⟨actor, scope, unique, goal, verified⟩

def scopeJson (s : Scope) : String :=
  "{\"node\":" ++ jsonString s.node ++ ",\"session\":" ++ jsonString s.session ++
    ",\"session_document\":" ++ jsonString s.sessionDocument ++
    ",\"requester\":" ++ (match s.requester with
      | none => "null" | some did => jsonString did) ++ "}"

def goalJson : Option GoalReference → String
  | none => "null"
  | some g => "{\"node\":" ++ jsonString g.node ++ ",\"session\":" ++ jsonString g.session ++
      ",\"document\":" ++ jsonString g.document ++ ",\"composite\":" ++ jsonString g.composite ++ "}"

private def recoveryJson (c : Case) : String :=
  if c.actor == current.node && c.scope == current && c.unique && c.verified &&
      c.goal.all (goalMatches current) then
    let previous := some (⟨current,
      some { goal with composite := "previous-commit" }⟩ : Observation)
    let schedule : Background.Observation.State := ⟨30, true⟩
    let input : Background.Observation.Input := ⟨29, false, 60, .quiet⟩
    let before := GoalAutomation.ObservationRecovery.observe schedule input
      c.actor c.scope c.unique c.goal previous
    let after := GoalAutomation.ObservationRecovery.observe schedule { input with now := 30 }
      c.actor c.scope c.unique c.goal previous
    "{\"before\":" ++ goalJson (before.bind (·.goal)) ++
      ",\"after\":" ++ goalJson (after.bind (·.goal)) ++ "}"
  else "null"

def caseJson (c : Case) : String :=
  let projected := project? c.actor c.scope c.unique c.goal
  let accepted := accepts current ⟨c.scope, c.goal⟩ c.verified
  "{\"actor\":" ++ jsonString c.actor ++ ",\"current\":" ++ scopeJson current ++
    ",\"scope\":" ++ scopeJson c.scope ++ ",\"unique\":" ++ (if c.unique then "true" else "false") ++
    ",\"goal\":" ++ goalJson c.goal ++ ",\"verified\":" ++ (if c.verified then "true" else "false") ++
    ",\"projected\":" ++ (if projected.isSome then "true" else "false") ++
    ",\"accepted\":" ++ (if accepted then "true" else "false") ++
    ",\"recovery\":" ++ recoveryJson c ++ "}"

def casesJson : String := jsonArray (cases.map caseJson)

example : cases.length = 288 := by native_decide

end Conformance.GoalObservationContracts
