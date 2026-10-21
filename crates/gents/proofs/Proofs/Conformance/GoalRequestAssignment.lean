import Proofs.GoalAutomation.RequestAssignment
import Proofs.Conformance.ContractTypes
import Lean

namespace Conformance.GoalRequestAssignmentContracts

open GoalAutomation.RequestAssignment Lean

structure Case where
  name : String
  assignment : Assignment := ⟨1, 10, 1, some 100⟩
  entry : Entry := ⟨100, 1, 10, some ⟨1, none⟩, true⟩
  observed : List Assignment := [⟨1, 10, 2, some 200⟩, ⟨1, 10, 1, some 100⟩]
  deriving DecidableEq, Repr

def cases : List Case :=
  [ { name := "first_request_binds_itself" }
  , { name := "ordinary_followup_keeps_goal", entry := ⟨101, 1, 10, some ⟨1, some 100⟩, true⟩ }
  , { name := "later_goal_does_not_capture_old_followup", assignment := ⟨1, 10, 2, some 200⟩,
      entry := ⟨101, 1, 10, some ⟨1, some 100⟩, true⟩ }
  , { name := "reused_goal_id_retains_old_assignment", assignment := ⟨1, 10, 1, some 200⟩,
      entry := ⟨101, 1, 10, some ⟨1, some 100⟩, true⟩,
      observed := [⟨1, 10, 1, some 200⟩, ⟨1, 10, 1, some 100⟩] }
  , { name := "current_followup_joins_new_assignment", assignment := ⟨1, 10, 2, some 200⟩,
      entry := ⟨201, 1, 10, some ⟨2, some 200⟩, true⟩ }
  , { name := "task_opening_request_uses_physical_binding", entry := ⟨100, 1, 10, none, true⟩ }
  , { name := "unbound_followup_cannot_join_pinned_goal", entry := ⟨101, 1, 10, none, true⟩ }
  , { name := "self_reference_cannot_impersonate_another_root", entry := ⟨101, 1, 10, some ⟨1, none⟩, true⟩ }
  , { name := "other_goal_same_root_cannot_join", entry := ⟨100, 1, 10, some ⟨2, some 100⟩, true⟩ }
  , { name := "unknown_reference_is_not_unassigned", entry := ⟨101, 1, 10, some ⟨9, some 900⟩, true⟩ }
  , { name := "wrong_owner", entry := ⟨101, 2, 10, some ⟨1, some 100⟩, true⟩ }
  , { name := "wrong_session", entry := ⟨101, 1, 11, some ⟨1, some 100⟩, true⟩ }
  , { name := "invalid_signature", entry := ⟨101, 1, 10, some ⟨1, some 100⟩, false⟩ }
  , { name := "unbound_legacy_head_keeps_existing_membership", assignment := ⟨1, 10, 1, none⟩,
      entry := ⟨100, 1, 10, none, true⟩, observed := [⟨1, 10, 1, none⟩] }
  , { name := "tagged_request_cannot_join_unbound_legacy_goal", assignment := ⟨1, 10, 1, none⟩ }
  , { name := "absent_goals_have_no_assignment", entry := ⟨100, 1, 10, none, true⟩, observed := [] }
  , { name := "missing_referenced_goal_is_unknown", observed := [] }
  , { name := "ambiguous_physical_binding_is_unknown", entry := ⟨100, 1, 10, none, true⟩,
      observed := [⟨1, 10, 1, some 100⟩, ⟨1, 10, 2, some 100⟩] }
  ]

def referenceJson (reference : Reference) : Json :=
  Json.mkObj [("goal", toJson reference.goal), ("root", toJson reference.root)]

def assignmentJson (a : Assignment) : Json :=
  Json.mkObj [("owner", toJson a.owner), ("session", toJson a.session),
    ("goal", toJson a.goal), ("root", toJson a.root)]

def entryJson (e : Entry) : Json :=
  Json.mkObj [("doc", toJson e.doc), ("owner", toJson e.owner),
    ("session", toJson e.session),
    ("reference", e.reference.map referenceJson |>.getD Json.null),
    ("signature_valid", toJson e.signatureValid)]

def observationJson : Observation → Json
  | .unassigned => Json.mkObj [("kind", toJson "unassigned")]
  | .unknown => Json.mkObj [("kind", toJson "unknown")]
  | .assigned goal root => Json.mkObj [("kind", toJson "assigned"),
      ("goal", toJson goal), ("root", toJson root)]

def caseJson (c : Case) : Json :=
  Json.mkObj [("name", toJson c.name), ("assignment", assignmentJson c.assignment),
    ("entry", entryJson c.entry), ("observed", toJson (c.observed.map assignmentJson)),
    ("expected_member", toJson (belongs c.assignment c.entry)),
    ("expected_observation", observationJson (observe c.observed c.entry))]

def casesJson : String := (toJson (cases.map caseJson)).compress

example : cases.length = 18 := by decide

end Conformance.GoalRequestAssignmentContracts
