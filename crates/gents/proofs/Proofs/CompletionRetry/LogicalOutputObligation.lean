import Proofs.CompletionRetry.OutputObligation
import Proofs.GraphPipeline.LogicalInvocation

/-!
Membership reuses the existing finite physical ancestry projection.
`Attempt.pinnedRoot` means an admitted invocation entry in this instantiation;
non-graph automated roots are supported. This does not require a GraphRun.
The runtime must verify existing request signatures/physical links; booleans
are validation facts, not a new ACL or a cryptographic proof.
-/
namespace CompletionRetry.OutputObligation.Logical
open GraphPipeline.LogicalInvocation

structure Write where
  callDoc : Nat
  requestDoc : Nat
  tool : Nat
  completed : Bool
  deriving DecidableEq, Repr

/-- Union by physical completed tool-call identity, not result-document identity.
Two distinct completed calls updating the same result remain two writes, exactly
as in the existing owner. This does not prove output-document uniqueness. -/
def completedIds (rows : List Attempt) (edges : List Edge) (root tool : Nat)
    (writes : List Write) : List Nat :=
  ((writes.filter fun w => w.completed && w.tool == tool &&
      roots rows edges w.requestDoc == [root]).map Write.callDoc).eraseDups

/-- Activation inherits the authenticated entry rather than the continuation's `goal` trigger kind. The contract is the runtime's current configured tool
surface, shared as one gate instance by loop and model hook; no root config pin.
Expected count and
countValid retain the existing count-field parser/consistency semantics, now
observed over exactly this same union of completed writes. -/
def decision (rows : List Attempt) (edges : List Edge) (root current tool : Nat)
    (scope : Scope) (entry : ActivationContext) (contract : State)
    (writes : List Write) : Decision :=
  if roots rows edges current != [root] then .reject
  else if active scope entry then
    decideTerminal { contract with completedWrites :=
      (completedIds rows edges root tool writes).length }
  else .complete

/-- Model-requested Goal completion uses the same observed decision as the owned
completion loop, then existing status/sequence CAS. Explicit operator lifecycle
control is unchanged. No new transaction or status writer. This model makes no
atomic phantom/document-witness or immutable-configuration guarantee. -/
def completeGoal (goal : Goals.State) (gate : Decision) : Option Goals.State :=
  if gate == .complete then Goals.step? goal .complete else none

theorem unmet_blocks_goal_completion (goal : Goals.State) :
    completeGoal goal .continue = none := rfl

theorem invalid_blocks_goal_completion (goal : Goals.State) :
    completeGoal goal .reject = none := rfl

theorem satisfied_uses_existing_goal_owner (goal : Goals.State) :
    completeGoal goal .complete = Goals.step? goal .complete := rfl

theorem unrelated_write_no_contribution
    (rows : List Attempt) (edges : List Edge) (root tool : Nat)
    (writes : List Write) (w : Write)
    (h : roots rows edges w.requestDoc ≠ [root]) :
    completedIds rows edges root tool (w :: writes) =
      completedIds rows edges root tool writes := by
  simp [completedIds, List.filter_cons, h]

theorem duplicate_observation_no_inflation
    (rows : List Attempt) (edges : List Edge) (root tool : Nat)
    (writes : List Write) (w : Write) :
    completedIds rows edges root tool (w :: w :: writes) =
      completedIds rows edges root tool (w :: writes) := by
  simp only [completedIds, List.filter_cons]
  split <;> simp_all [List.eraseDups, List.eraseDups.loop]

private def parent : Attempt := ⟨10, true, some .failed⟩
private def child : Attempt := ⟨20, false, none⟩
private def physicalEdge : Edge := ⟨10,20,true⟩
private def entry : ActivationContext := ⟨false,true⟩
private def contract : State := ⟨2,0,none,true⟩
private def first : Write := ⟨100,10,1,true⟩
private def second : Write := ⟨200,20,1,true⟩

/-- No graph record required: triggered entry plus authenticated Goal child. -/
theorem continuation_inherits_trigger_obligation :
    decision [parent,child] [physicalEdge] 10 20 1 .trigger entry contract
      [first] = .continue := by decide

theorem completed_writes_combine_across_members :
    decision [parent,child] [physicalEdge] 10 20 1 .trigger entry contract
      [first,second] = .complete := by decide

theorem invalid_edge_cannot_discharge :
    decision [parent,child] [⟨10,20,false⟩] 10 20 1 .trigger entry contract
      [first,second] = .reject := by decide

theorem conflicting_expected_counts_reject :
    decision [parent,child] [physicalEdge] 10 20 1 .trigger entry
      {contract with countValid := false} [first,second] = .reject := by decide

private def goal : Goals.State := ⟨.active,0,false,false⟩

theorem unmet_chain_cannot_complete_goal :
    completeGoal goal (decision [parent,child] [physicalEdge] 10 20 1 .trigger
      entry contract [first]) = none := by decide

theorem satisfied_chain_completes_via_goal_transition :
    completeGoal goal (decision [parent,child] [physicalEdge] 10 20 1 .trigger
      entry contract [first,second]) =
      some {goal with status := .complete, wrapupCompleted := true} := by decide

theorem unrelated_and_failed_writes_do_not_discharge :
    decision [parent,child,⟨30,false,some .completed⟩] [physicalEdge] 10 20 1 .trigger
      entry contract [first,⟨300,30,1,true⟩,⟨400,20,1,false⟩] = .continue := by decide

theorem duplicate_call_observation_not_second_write :
    decision [parent,child] [physicalEdge] 10 20 1 .trigger entry contract
      [first,first] = .continue := by decide

theorem nontrigger_root_preserves_existing_trigger_scope :
    decision [parent,child] [physicalEdge] 10 20 1 .trigger ⟨false,false⟩
      contract [] = .complete := by decide

theorem request_scope_still_applies_without_automation :
    decision [parent,child] [physicalEdge] 10 20 1 .request ⟨false,false⟩
      contract [] = .continue := by decide

end CompletionRetry.OutputObligation.Logical
