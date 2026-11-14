import Proofs.Session.SessionQueue.FoldCases
import Proofs.Session.SessionQueue.Management
import Proofs.Session.Enrollment.RequestInput
import Proofs.Conformance.Family
import Lean

namespace Conformance.GoalQueueAssignmentContracts

open SessionQueue Lean

deriving instance ToJson for Enrollment.GoalAssignmentInput

structure Case where
  name : String
  headReference : Option Enrollment.GoalAssignmentInput := some ⟨"goal-one", none⟩
  candidateReference : Option Enrollment.GoalAssignmentInput := some ⟨"goal-one", some "head-physical"⟩
  deriving ToJson

def normalized (input : Option Enrollment.GoalAssignmentInput) (doc : String) :
    Option (String × String) :=
  let continuation := Enrollment.continuationInput {goalAssignment := input}
    {source := .user, policy := .append} none doc
  continuation.goalAssignment.map fun reference =>
    (reference.goalId, reference.rootRequestDocId.getD doc)

def headEntry (c : Case) : QueueEntry :=
  { FoldCases.user 1 with goalAssignment := normalized c.headReference "head-physical" }

def candidateEntry (c : Case) : QueueEntry :=
  { FoldCases.user 2 (some 1) with
    delivery := .steer
    goalAssignment := normalized c.candidateReference "candidate-physical" }

def cases : List Case :=
  [ {name := "opening_self_matches_explicit_physical_root"}
  , {name := "independent_followups_share_opening_root",
      headReference := some ⟨"goal-one", some "opening-physical"⟩,
      candidateReference := some ⟨"goal-one", some "opening-physical"⟩}
  , {name := "new_goal_is_a_turn_boundary",
      candidateReference := some ⟨"goal-two", some "head-physical"⟩}
  , {name := "reused_goal_with_new_root_is_a_turn_boundary",
      candidateReference := some ⟨"goal-one", some "other-opening"⟩}
  , {name := "two_self_references_are_distinct_assignments",
      candidateReference := some ⟨"goal-one", none⟩}
  , {name := "unassigned_candidate_cannot_join_goal", candidateReference := none}
  , {name := "assigned_candidate_cannot_join_unassigned_head", headReference := none}
  , {name := "ordinary_unassigned_turns_keep_folding", headReference := none, candidateReference := none}
  , {name := "literal_goal_ids_are_not_normalized",
      candidateReference := some ⟨"GOAL-ONE", some "head-physical"⟩}
  , {name := "literal_physical_roots_are_not_normalized",
      headReference := some ⟨"goal-one", some " opening "⟩,
      candidateReference := some ⟨"goal-one", some "opening"⟩}
  ]

structure FoldOutput where
  fold : Bool
  deriving ToJson

structure SteerOutput where
  steer : Bool
  deriving ToJson

@[conformance_family]
def foldDecision : Family Case FoldOutput where
  inputs := cases
  model := fun c => { fold := decide ((headEntry c).foldsInto (candidateEntry c)) }

@[conformance_family]
def steerDecision : Family Case SteerOutput where
  inputs := cases
  model := fun c => { steer := decide (steersInto (headEntry c) (candidateEntry c)) }

example : cases.map (fun c => decide ((headEntry c).foldsInto (candidateEntry c))) =
    [true, true, false, false, false, false, false, true, false, false] := by native_decide

end Conformance.GoalQueueAssignmentContracts
