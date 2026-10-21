import Proofs.Session.FoldCases
import Proofs.Session.Management
import Proofs.Enrollment.RequestInput
import Lean

namespace Conformance.GoalQueueAssignmentContracts

open SessionQueue Lean

structure Case where
  name : String
  head : Option Enrollment.GoalAssignmentInput := some ⟨"goal-one", none⟩
  candidate : Option Enrollment.GoalAssignmentInput := some ⟨"goal-one", some "head-physical"⟩

def normalized (input : Option Enrollment.GoalAssignmentInput) (doc : String) :
    Option (String × String) :=
  let continuation := Enrollment.continuationInput {goalAssignment := input}
    {source := .user, policy := .append} none doc
  continuation.goalAssignment.map fun reference =>
    (reference.goalId, reference.rootRequestDocId.getD doc)

def headEntry (c : Case) : QueueEntry :=
  { FoldCases.user 1 with goalAssignment := normalized c.head "head-physical" }

def candidateEntry (c : Case) : QueueEntry :=
  { FoldCases.user 2 (some 1) with
    delivery := .steer
    goalAssignment := normalized c.candidate "candidate-physical" }

def cases : List Case :=
  [ {name := "opening_self_matches_explicit_physical_root"}
  , {name := "independent_followups_share_opening_root",
      head := some ⟨"goal-one", some "opening-physical"⟩,
      candidate := some ⟨"goal-one", some "opening-physical"⟩}
  , {name := "new_goal_is_a_turn_boundary", candidate := some ⟨"goal-two", some "head-physical"⟩}
  , {name := "reused_goal_with_new_root_is_a_turn_boundary",
      candidate := some ⟨"goal-one", some "other-opening"⟩}
  , {name := "two_self_references_are_distinct_assignments", candidate := some ⟨"goal-one", none⟩}
  , {name := "unassigned_candidate_cannot_join_goal", candidate := none}
  , {name := "assigned_candidate_cannot_join_unassigned_head", head := none}
  , {name := "ordinary_unassigned_turns_keep_folding", head := none, candidate := none}
  , {name := "literal_goal_ids_are_not_normalized", candidate := some ⟨"GOAL-ONE", some "head-physical"⟩}
  , {name := "literal_physical_roots_are_not_normalized",
      head := some ⟨"goal-one", some " opening "⟩,
      candidate := some ⟨"goal-one", some "opening"⟩}
  ]

def referenceJson (reference : Enrollment.GoalAssignmentInput) : Json :=
  Json.mkObj [("goal_id", toJson reference.goalId),
    ("root_request_doc_id", toJson reference.rootRequestDocId)]

def caseJson (c : Case) : Json :=
  Json.mkObj [("name", toJson c.name),
    ("head_reference", c.head.map referenceJson |>.getD Json.null),
    ("candidate_reference", c.candidate.map referenceJson |>.getD Json.null),
    ("expected_fold", toJson (decide ((headEntry c).foldsInto (candidateEntry c)))),
    ("expected_steer", toJson (decide (steersInto (headEntry c) (candidateEntry c))))]

def casesJson : String := (toJson (cases.map caseJson)).compress

example : cases.map (fun c => decide ((headEntry c).foldsInto (candidateEntry c))) =
    [true, true, false, false, false, false, false, true, false, false] := by native_decide

end Conformance.GoalQueueAssignmentContracts
