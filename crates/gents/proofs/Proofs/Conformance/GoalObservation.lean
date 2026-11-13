import Proofs.Automation.GoalAutomation.Observation
import Proofs.Automation.GoalAutomation.ObservationRecovery
import Proofs.Conformance.ContractTypes
import Proofs.Conformance.Family

namespace Conformance.GoalObservationContracts

open GoalAutomation.Observation Conformance.Contracts

def current : Scope := ⟨"node", "agent", "session", "session-document", some "requester"⟩
def goal : GoalReference := ⟨"node", "session", "goal-document", "goal-commit"⟩

structure Case where
  actor : String
  scope : Scope
  unique : Bool
  goal : Option GoalReference
  verified : Bool
  deriving DecidableEq, Repr

def cases : List Case :=
  ["agent", "other-agent", "node"].flatMap fun actor =>
  [true, false].flatMap fun unique =>
  [true, false].flatMap fun verified =>
  [current, { current with node := "foreign-node" },
    { current with agent := "other-agent" },
    { current with session := "foreign-session" },
    { current with sessionDocument := "recreated-document" },
    { current with requester := some "foreign-requester" },
    { current with requester := none }].flatMap fun scope =>
  [none, some goal, some { goal with node := "foreign-node" },
    some { goal with session := "foreign-session" },
    some { goal with document := "" }, some { goal with composite := "" }].map fun goal =>
      ⟨actor, scope, unique, goal, verified⟩

def scopeJson (s : Scope) : String :=
  "{\"node\":" ++ jsonString s.node ++ ",\"agent\":" ++ jsonString s.agent ++ ",\"session\":" ++ jsonString s.session ++
    ",\"session_document\":" ++ jsonString s.sessionDocument ++
    ",\"requester\":" ++ (match s.requester with
      | none => "null" | some did => jsonString did) ++ "}"

def goalJson : Option GoalReference → String
  | none => "null"
  | some g => "{\"node\":" ++ jsonString g.node ++ ",\"session\":" ++ jsonString g.session ++
      ",\"document\":" ++ jsonString g.document ++ ",\"composite\":" ++ jsonString g.composite ++ "}"

private def recoveryJson (c : Case) : String :=
  if c.actor == current.agent && c.scope == current && c.unique && c.verified &&
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

example : cases.length = 504 := by native_decide

structure RetainedRootInput where
  retainedKeepsRoot : Bool
  substituteRoot : Bool
  deriving Lean.ToJson

structure RetainedRootOutput where
  retainedLoad : Option String
  refreshWrites : Bool
  refreshedRoot : Option String
  revisionKept : Bool
  rewroteOnRepeat : Bool
  finalLoaded : Bool
  deriving Lean.ToJson

def rootView : Option String → String
  | none => "absent"
  | some root => if root == "goal-root" then "goal_root" else "other"

/-- A retained observation signed by the selected Agent, optionally without its
assignment root, is projected again for a Goal whose revision is unchanged;
optionally a root is then substituted without signing. -/
@[conformance_family]
def retainedRoot : Conformance.Family RetainedRootInput RetainedRootOutput where
  inputs := [true, false].flatMap fun retainedKeepsRoot =>
    [false, true].map fun substituteRoot => { retainedKeepsRoot, substituteRoot }
  model := fun i =>
    let current : SignedGoal := ⟨"goal-revision", some "goal-root"⟩
    let retained : SignedGoal :=
      { current with root := if i.retainedKeepsRoot then current.root else none }
    let (refreshed, writes) := reproject (some retained) current
    let substituted := if i.substituteRoot then { refreshed with root := some "another-root" }
      else refreshed
    { retainedLoad := (loadRetained retained retained).map (rootView ·.root)
      refreshWrites := writes
      refreshedRoot := (loadRetained refreshed refreshed).map (rootView ·.root)
      revisionKept := refreshed.revision == retained.revision
      rewroteOnRepeat := (reproject (some refreshed) current).2
      finalLoaded := (loadRetained refreshed substituted).isSome }

end Conformance.GoalObservationContracts
