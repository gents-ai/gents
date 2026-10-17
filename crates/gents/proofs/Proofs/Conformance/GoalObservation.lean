import Proofs.GoalAutomation.Observation
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

def caseJson (c : Case) : String :=
  let projected := project? c.actor c.scope c.unique c.goal
  let accepted := accepts current ⟨c.scope, c.goal⟩ c.verified
  "{\"actor\":" ++ jsonString c.actor ++ ",\"current\":" ++ scopeJson current ++
    ",\"scope\":" ++ scopeJson c.scope ++ ",\"unique\":" ++ (if c.unique then "true" else "false") ++
    ",\"goal\":" ++ goalJson c.goal ++ ",\"verified\":" ++ (if c.verified then "true" else "false") ++
    ",\"projected\":" ++ (if projected.isSome then "true" else "false") ++
    ",\"accepted\":" ++ (if accepted then "true" else "false") ++ "}"

def casesJson : String := jsonArray (cases.map caseJson)

example : cases.length = 288 := by native_decide

end Conformance.GoalObservationContracts
