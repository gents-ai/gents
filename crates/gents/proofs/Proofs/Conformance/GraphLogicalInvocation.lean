import Proofs.GraphPipeline.LogicalInvocation
import Proofs.Conformance.GraphFailureAttribution

namespace Conformance.GraphLogicalInvocationContracts
open GraphPipeline.LogicalInvocation Conformance.Contracts

structure Case where
  name : String
  rows : List Attempt
  edges : List Edge
  root : Doc := 10
  goal : Option GoalEvidence := none
  headBindingMatches : Bool := true
  resultSatisfied : Bool := false
  maximum : Nat := 10
  expected : Outcome
  physicalCount : Nat
  deriving DecidableEq, Repr

private def r : Attempt := ⟨10, true, some .failed⟩
private def c : Attempt := ⟨20, false, some .completed⟩
private def e : Edge := ⟨10,20,true⟩
private def active : GoalEvidence := ⟨.active,.unclaimed,false,false⟩
private def done : GoalEvidence := {active with status := .complete}

def cases : List Case :=
  [ {name := "active_goal_preclaim", rows := [r], edges := [], goal := some active,
      expected := .outstanding, physicalCount := 1}
  , {name := "claimed_publication_gap", rows := [r], edges := [],
      goal := some {active with phase := .claimed}, expected := .outstanding, physicalCount := 1}
  , {name := "pending_child", rows := [r, {c with terminal := none}], edges := [e],
      goal := some done, expected := .outstanding, physicalCount := 2}
  , {name := "successful_descendant_complete_goal", rows := [r,c], edges := [e],
      goal := some done, resultSatisfied := true, expected := .succeeded, physicalCount := 2}
  , {name := "paused_failed_tip_despite_result", rows := [r,{c with terminal := some .failed}], edges := [e],
      goal := some {active with status := .paused}, resultSatisfied := true,
      expected := .failed 20, physicalCount := 2}
  , {name := "failed_root_without_goal", rows := [r], edges := [], expected := .failed 10, physicalCount := 1}
  , {name := "unrelated_same_correlation", rows := [r,c], edges := [], resultSatisfied := true,
      expected := .failed 10, physicalCount := 1}
  , {name := "wrong_physical_parent_edge", rows := [r,c], edges := [{e with authenticated := false}],
      resultSatisfied := true, expected := .failed 10, physicalCount := 1}
  , {name := "wrong_child_owner", rows := [r,c], edges := [{e with authenticated := false}],
      resultSatisfied := true, expected := .failed 10, physicalCount := 1}
  , {name := "same_second_child_before_root", rows := [c,r], edges := [e],
      goal := some done, resultSatisfied := true, expected := .succeeded, physicalCount := 2}
  , {name := "historical_head_canonical_goal_mismatch", rows := [c,r], edges := [e],
      goal := some active, headBindingMatches := false, resultSatisfied := true,
      expected := .succeeded, physicalCount := 2}
  , {name := "physical_invocation_limit", rows := [r,c], edges := [e], maximum := 1,
      goal := some done, resultSatisfied := true, expected := .limitExceeded, physicalCount := 2}
  , {name := "branching_authenticated_tips", rows := [r,c,{c with doc := 30}], edges := [e,⟨10,30,true⟩],
      resultSatisfied := true, expected := .invalid, physicalCount := 3}
  , {name := "unfinished_budget_wrapup", rows := [r], edges := [],
      goal := some {active with status := .budgetLimited, wrapupRequested := true},
      expected := .outstanding, physicalCount := 1}
  , {name := "successful_tip_missing_result", rows := [r,c], edges := [e], goal := some done,
      expected := .failed 20, physicalCount := 2}
  ]

theorem cases_replay : ∀ c ∈ cases,
    projectLimited c.rows c.edges c.root (associatedGoal c.goal c.headBindingMatches) c.resultSatisfied c.maximum = c.expected ∧
    (members c.rows c.edges c.root).length = c.physicalCount := by decide

private def b (x : Bool) := if x then "true" else "false"
private def natOpt : Option Nat → String | none => "null" | some n => toString n
private def terminalJson : Option Goals.RequestTerminal → String
  | none => "null" | some .completed => "\"completed\"" | some .failed => "\"failed\""
  | some .dead => "\"dead\"" | some .interrupted => "\"interrupted\"" | some .superseded => "\"superseded\""
private def rowJson (a : Attempt) :=
  "{\"doc\":" ++ toString a.doc ++ ",\"pinned_root\":" ++ b a.pinnedRoot ++
  ",\"terminal\":" ++ terminalJson a.terminal ++ "}"
private def edgeJson (e : Edge) :=
  "{\"parent\":" ++ toString e.parent ++ ",\"child\":" ++ toString e.child ++
  ",\"authenticated\":" ++ b e.authenticated ++ "}"
private def phaseJson : GoalAutomation.ContinuationPhase → String
  | .unclaimed => "unclaimed" | .claimed => "claimed" | .childPresent => "child_present"
private def goalJson : Option GoalEvidence → String
  | none => "null"
  | some g => "{\"status\":" ++ jsonString g.status.toDefraDB ++
    ",\"phase\":" ++ jsonString (phaseJson g.phase) ++
    ",\"wrapup_requested\":" ++ b g.wrapupRequested ++
    ",\"wrapup_completed\":" ++ b g.wrapupCompleted ++ "}"
private def outcomeName : Outcome → String
  | .outstanding => "outstanding" | .succeeded => "succeeded" | .failed _ => "failed"
  | .invalid => "invalid" | .limitExceeded => "limit_exceeded"
private def tip : Outcome → Option Nat | .failed n => some n | _ => none
private def caseJson (c : Case) :=
  "{\"name\":" ++ jsonString c.name ++ ",\"rows\":" ++ jsonArray (c.rows.map rowJson) ++
  ",\"edges\":" ++ jsonArray (c.edges.map edgeJson) ++ ",\"root\":" ++ toString c.root ++
  ",\"head_binding_matches\":" ++ b c.headBindingMatches ++
  ",\"goal\":" ++ goalJson c.goal ++ ",\"result_satisfied\":" ++ b c.resultSatisfied ++
  ",\"max_invocations\":" ++ toString c.maximum ++ ",\"expected\":{\"outcome\":" ++
  jsonString (outcomeName c.expected) ++ ",\"tip\":" ++ natOpt (tip c.expected) ++
  ",\"physical_count\":" ++ toString c.physicalCount ++ "}}"
def casesJson := jsonArray (cases.map caseJson)

structure PublicationCase where
  name : String
  events : List PublicationEvent
  expected : List PublicationState
  deriving DecidableEq, Repr
private def initial : PublicationState := ⟨GraphPipeline.FailureAttribution.initial,0⟩
private def state (generation children : Nat) (cancel : Bool := false)
    (primary : Option Nat := none) (status : GraphPipeline.RunStatus := .running) : PublicationState :=
  {graph := {initial.graph with generation := generation, primary := primary, run := {initial.graph.run with cancellationRequested := cancel, status := status}}, children := children}
def publicationCases : List PublicationCase :=
  [ ⟨"publication_wins_stale_terminal", [.publish 0, .finish 0 90], [state 1 1, state 1 1]⟩
  , ⟨"terminal_wins_publication", [.finish 0 90, .publish 1],
      [state 1 0 false (some 90) .failed, state 1 0 false (some 90) .failed]⟩
  , ⟨"latched_failure_blocks_publication", [.capture 0 90, .publish 1],
      [state 1 0 false (some 90), state 1 0 false (some 90)]⟩
  , ⟨"cancellation_blocks_publication", [.cancel, .publish 1], [state 1 0 true, state 1 0 true]⟩
  , ⟨"publication_invalidates_stale_capture", [.publish 0, .capture 0 90], [state 1 1, state 1 1]⟩
  ]
private def trace (s : PublicationState) : List PublicationEvent → List PublicationState
  | [] => []
  | e::es => let next := publicationStep s e; next :: trace next es

theorem publication_cases_replay : ∀ c ∈ publicationCases,
    trace initial c.events = c.expected := by decide

private def eventJson : PublicationEvent → String
  | .cancel => "{\"kind\":\"cancel\"}"
  | .publish n => "{\"kind\":\"publish\",\"expected_generation\":" ++ toString n ++ "}"
  | .capture n cause => "{\"kind\":\"capture\",\"expected_generation\":" ++ toString n ++ ",\"cause\":" ++ toString cause ++ "}"
  | .finish n cause => "{\"kind\":\"finish\",\"expected_generation\":" ++ toString n ++ ",\"cause\":" ++ toString cause ++ "}"
private def publicationStateJson (s : PublicationState) :=
  let observation := GraphFailureAttributionContracts.observationJson (GraphFailureAttributionContracts.observe s.graph)
  String.mk (observation.toList.take (observation.length - 1)) ++
    ",\"children\":" ++ toString s.children ++ "}"
private def publicationCaseJson (c : PublicationCase) :=
  "{\"name\":" ++ jsonString c.name ++ ",\"initial\":" ++ publicationStateJson initial ++
  ",\"events\":" ++ jsonArray (c.events.map eventJson) ++
  ",\"expected\":" ++ jsonArray (c.expected.map publicationStateJson) ++ "}"
def publicationCasesJson := jsonArray (publicationCases.map publicationCaseJson)
end Conformance.GraphLogicalInvocationContracts
