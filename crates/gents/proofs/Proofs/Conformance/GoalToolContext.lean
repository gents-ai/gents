import Proofs.GoalAutomation.ToolContext
import Proofs.Conformance.GoalRequestAssignment

namespace Conformance.GoalToolContextContracts

open Lean GoalAutomation.RequestAssignment GoalAutomation.ToolContext
open Conformance.GoalRequestAssignmentContracts

structure Case where
  name : String
  versions : List Version := []
  entry : Entry := ⟨101, 1, 10, some ⟨1, some 100⟩, true⟩
  retired : List Nat := []
  current : Option Identity := none

def cases : List Case :=
  let first : Version := ⟨10, 1, 1, [], ⟨1, 10, 1, some 100⟩, .active⟩
  let stopped := {first with commit := 2, height := 2, parents := [1], status := .stopped}
  let next := {first with document := 20, commit := 3, assignment := ⟨1, 10, 2, some 200⟩}
  let reassigned := {first with commit := 3, height := 3, parents := [1], assignment := ⟨1, 10, 1, some 200⟩}
  [ {name := "retired_caller_reads_its_own_goal", versions := [first, stopped, next],
      retired := [10], current := some ⟨20, 2, 200⟩}
  , {name := "reused_goal_id_keeps_caller_root", versions := [first, reassigned], current := some ⟨10, 1, 200⟩}
  , {name := "current_assigned_followup", versions := [first], current := some ⟨10, 1, 100⟩}
  , {name := "current_replacement_assignment", versions := [next],
      entry := ⟨101, 1, 10, some ⟨2, some 200⟩, true⟩, current := some ⟨20, 2, 200⟩}
  , {name := "current_unreferenced_opening_request", versions := [first],
      entry := ⟨100, 1, 10, none, true⟩, current := some ⟨10, 1, 100⟩}
  , {name := "unassigned_without_goal", entry := ⟨101, 1, 10, none, true⟩}
  , {name := "unassigned_cannot_borrow_later_goal", versions := [next],
      entry := ⟨101, 1, 10, none, true⟩, current := some ⟨20, 2, 200⟩}
  , {name := "invalid_signed_caller", versions := [first],
      entry := ⟨101, 1, 10, some ⟨1, some 100⟩, false⟩, current := some ⟨10, 1, 100⟩}
  , {name := "missing_referenced_goal", entry := ⟨101, 1, 10, some ⟨9, some 900⟩, true⟩}
  , {name := "unbound_goal_is_unknown", versions := [{first with assignment := ⟨1, 10, 1, none⟩}],
      entry := ⟨101, 1, 10, none, true⟩, current := some ⟨10, 1, 100⟩}
  , {name := "ambiguous_physical_root", versions := [first, {next with assignment := ⟨1, 10, 2, some 100⟩}],
      entry := ⟨100, 1, 10, none, true⟩, current := some ⟨20, 2, 100⟩}
  ]

def identityJson (identity : Identity) : Json :=
  Json.mkObj [("document", toJson identity.document), ("goal", toJson identity.goal), ("root", toJson identity.root)]

def observationJson : GoalAutomation.ToolContext.Observation → Json
  | .unassigned => Json.mkObj [("kind", toJson "unassigned")]
  | .unknown => Json.mkObj [("kind", toJson "unknown")]
  | .assigned identity => Json.mkObj [("kind", toJson "assigned"), ("identity", identityJson identity)]

def caseJson (c : Case) : Json :=
  let observed := GoalAutomation.ToolContext.observe c.versions c.entry
  Json.mkObj [("name", toJson c.name), ("versions", toJson (c.versions.map versionJson)),
    ("entry", entryJson c.entry), ("retire_documents", toJson c.retired),
    ("current", c.current.map identityJson |>.getD Json.null),
    ("observation", observationJson observed),
    ("read_only", toJson (readOnly observed c.current)),
    ("can_create", toJson (canCreate observed c.current)), ("can_update", toJson (canWrite observed c.current))]

def casesJson : String := (toJson (cases.map caseJson)).compress

example : cases.length = 11 := by decide

end Conformance.GoalToolContextContracts
