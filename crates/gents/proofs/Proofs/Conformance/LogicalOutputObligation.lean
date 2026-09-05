import Proofs.CompletionRetry.LogicalOutputObligation
import Proofs.Conformance.Contracts.Json.Helpers

namespace Conformance.LogicalOutputObligationContracts
open CompletionRetry.OutputObligation CompletionRetry.OutputObligation.Logical
open GraphPipeline.LogicalInvocation Conformance.Contracts

structure Case where
  name : String
  automatedRoot : Bool
  requestScope : Bool
  authenticatedChild : Bool
  minimum : Nat
  expectedCount : Option Nat
  countValid : Bool
  writes : List Write
  expectedDecision : Decision
  deriving Repr

private def rows : List Attempt :=
  [⟨10,true,some .failed⟩,⟨20,false,none⟩,⟨30,false,some .completed⟩]
private def evaluate (c : Case) :=
  decision rows [⟨10,20,c.authenticatedChild⟩] 10 20 1
    (if c.requestScope then .request else .trigger) ⟨false,c.automatedRoot⟩
    ⟨c.minimum,0,c.expectedCount,c.countValid⟩ c.writes
private def parentWrite : Write := ⟨100,10,1,true⟩
private def childWrite : Write := ⟨200,20,1,true⟩
def cases : List Case :=
  [⟨"child_inherits_root_obligation",true,false,true,2,none,true,[parentWrite],.continue⟩
  ,⟨"root_child_writes_combine",true,false,true,2,none,true,[parentWrite,childWrite],.complete⟩
  ,⟨"unrelated_write_excluded",true,false,true,2,none,true,[parentWrite,⟨300,30,1,true⟩],.continue⟩
  ,⟨"failed_write_excluded",true,false,true,2,none,true,[parentWrite,⟨200,20,1,false⟩],.continue⟩
  ,⟨"duplicate_observation_not_second_write",true,false,true,2,none,true,[parentWrite,parentWrite],.continue⟩
  ,⟨"unauthenticated_child_rejected",true,false,false,2,none,true,[parentWrite,childWrite],.reject⟩
  ,⟨"nontrigger_scope_inactive",false,false,true,2,none,true,[],.complete⟩
  ,⟨"request_scope_still_active",false,true,true,2,none,true,[],.continue⟩
  ,⟨"dynamic_count_incomplete",true,false,true,1,some 3,true,[parentWrite,childWrite],.continue⟩
  ,⟨"dynamic_count_complete",true,false,true,1,some 2,true,[parentWrite,childWrite],.complete⟩
  ,⟨"inconsistent_counts_rejected",true,false,true,1,none,false,[parentWrite,childWrite],.reject⟩]

theorem cases_replay : ∀ c ∈ cases, evaluate c = c.expectedDecision := by decide

private def boolJson (v : Bool) := if v then "true" else "false"
private def decisionString : Decision → String
  | .continue => "continue" | .complete => "complete" | .reject => "reject"
private def writeJson (w : Write) :=
  "{\"call_doc\":" ++ toString w.callDoc ++ ",\"request_doc\":" ++ toString w.requestDoc ++
  ",\"tool\":" ++ toString w.tool ++ ",\"completed\":" ++ boolJson w.completed ++ "}"
private def caseJson (c : Case) :=
  "{\"name\":" ++ jsonString c.name ++ ",\"automated_root\":" ++ boolJson c.automatedRoot ++
  ",\"request_scope\":" ++ boolJson c.requestScope ++
  ",\"authenticated_child\":" ++ boolJson c.authenticatedChild ++
  ",\"minimum\":" ++ toString c.minimum ++
  ",\"expected_count\":" ++ (c.expectedCount.map toString).getD "null" ++
  ",\"count_valid\":" ++ boolJson c.countValid ++
  ",\"writes\":" ++ jsonArray (c.writes.map writeJson) ++
  ",\"expected_decision\":" ++ jsonString (decisionString c.expectedDecision) ++ "}"
def casesJson := jsonArray (cases.map caseJson)
end Conformance.LogicalOutputObligationContracts
