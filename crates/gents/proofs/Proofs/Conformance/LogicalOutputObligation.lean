import Proofs.Runtime.CompletionRetry.LogicalOutputObligation
import Proofs.Conformance.Family

namespace CompletionRetry.OutputObligation
deriving instance Lean.ToJson for Decision
end CompletionRetry.OutputObligation

namespace CompletionRetry.OutputObligation.Logical
deriving instance Lean.ToJson for Write
end CompletionRetry.OutputObligation.Logical

namespace Conformance.LogicalOutputObligationContracts
open Lean CompletionRetry.OutputObligation CompletionRetry.OutputObligation.Logical
open GoalAutomation.RequestAncestry

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

structure GateInput where
  name : String
  automatedRoot : Bool
  requestScope : Bool
  authenticatedChild : Bool
  minimum : Nat
  expectedCount : Option Nat
  countValid : Bool
  writes : List Write
  deriving ToJson

private def rows : List Attempt :=
  [⟨10,true,some .failed⟩,⟨20,false,none⟩,⟨30,false,some .completed⟩]
private def evaluateInput (c : GateInput) :=
  decision rows [⟨10,20,c.authenticatedChild⟩] 10 20 1
    (if c.requestScope then .request else .trigger) ⟨false,c.automatedRoot⟩
    ⟨c.minimum,0,c.expectedCount,c.countValid⟩ c.writes
private def Case.input (c : Case) : GateInput :=
  { name := c.name, automatedRoot := c.automatedRoot, requestScope := c.requestScope
    authenticatedChild := c.authenticatedChild, minimum := c.minimum
    expectedCount := c.expectedCount, countValid := c.countValid, writes := c.writes }
private def evaluate (c : Case) := evaluateInput c.input
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

@[conformance_family]
def gate : Family GateInput Decision where
  inputs := cases.map Case.input
  model := evaluateInput

end Conformance.LogicalOutputObligationContracts
