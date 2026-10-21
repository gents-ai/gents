import Proofs.GoalAutomation.DeliveryInput
import Proofs.Conformance.GoalRequestAssignment

namespace Conformance.GoalDeliveryInput

open GoalAutomation GoalAutomation.RequestAssignment Lean

structure Case where
  name : String
  plugin : Bool := false
  existing : Bool := true
  owned : Bool := true
  observed : Option Reference := some ⟨2, some 200⟩
  captured : Option Reference := none
  admitted : Option (Option Reference) := none
  remote : Bool := false
  observationPresent : Bool := true
  observationVerified : Bool := true
  foreignOwner : Bool := false
  foreignSession : Bool := false
  deriving DecidableEq, Repr

def cases : List Case :=
  [ {name := "trigger_existing_captures_destination"}
  , {name := "trigger_existing_absence", observed := none}
  , {name := "trigger_existing_legacy", observed := some ⟨2, none⟩}
  , {name := "trigger_fresh_ignores_orphan", existing := false}
  , {name := "trigger_prepared_goal_is_immutable", observed := some ⟨1, some 200⟩,
      captured := some ⟨1, none⟩}
  , {name := "trigger_admitted_reference_survives_replacement", admitted := some (some ⟨1, some 100⟩)}
  , {name := "trigger_admitted_absence_survives_new_goal", admitted := some none}
  , {name := "trigger_foreign_destination_refused", owned := false}
  , {name := "plugin_message_captures_destination", plugin := true}
  , {name := "plugin_message_absence", plugin := true, observed := none}
  , {name := "plugin_message_legacy", plugin := true, observed := some ⟨2, none⟩}
  , {name := "plugin_spawn_ignores_orphan", plugin := true, existing := false}
  , {name := "plugin_admitted_reference_survives_replacement", plugin := true,
      admitted := some (some ⟨1, some 100⟩)}
  , {name := "plugin_admitted_absence_survives_new_goal", plugin := true, admitted := some none}
  , {name := "plugin_remote_verified_assignment", plugin := true, remote := true}
  , {name := "plugin_remote_verified_absence", plugin := true, remote := true, observed := none}
  , {name := "plugin_remote_missing_observation", plugin := true, remote := true,
      observationPresent := false, observationVerified := false}
  , {name := "plugin_remote_tampered_observation", plugin := true, remote := true,
      observationVerified := false}
  , {name := "plugin_remote_foreign_owner", plugin := true, remote := true, foreignOwner := true}
  , {name := "plugin_remote_foreign_session", plugin := true, remote := true, foreignSession := true}
  ]

def caseJson (c : Case) : Json :=
  let identity : Triggers.Durable.Identity := ⟨"1", "delivery", "Task", c.name⟩
  let key : PluginJob.SpawnKey := ⟨c.name, "worker", 1⟩
  let destination := if c.plugin then some (DeliveryInput.pluginDestination key c.existing)
    else DeliveryInput.triggerDestination identity (if c.existing then some "10" else none) c.owned
  let facts : MessageInputFacts := {
    owner := if c.remote then 2 else 1, session := 10, sessionReadable := c.existing,
    observationVerified := c.observationVerified,
    observed := c.observed.map fun reference =>
      ⟨if c.remote && !c.foreignOwner then 2 else 1,
       if c.foreignSession then 11 else 10, reference.goal, reference.root⟩,
    callerReference := some ⟨9, some 900⟩ }
  let expected := (DeliveryInput.prepare destination c.admitted c.captured facts).map fun
    | .pending => Json.mkObj [("kind", toJson "pending")]
    | .prepared reference => Json.mkObj [("kind", toJson "prepared"),
        ("reference", reference.map GoalRequestAssignmentContracts.referenceJson |>.getD Json.null)]
  Json.mkObj [("name", toJson c.name), ("plugin", toJson c.plugin),
    ("existing", toJson c.existing), ("owned", toJson c.owned),
    ("remote", toJson c.remote), ("observation_present", toJson c.observationPresent),
    ("observation_verified", toJson c.observationVerified),
    ("foreign_owner", toJson c.foreignOwner), ("foreign_session", toJson c.foreignSession),
    ("observed", c.observed.map GoalRequestAssignmentContracts.referenceJson |>.getD Json.null),
    ("captured", c.captured.map GoalRequestAssignmentContracts.referenceJson |>.getD Json.null),
    ("admitted", toJson c.admitted.isSome),
    ("admitted_reference", (c.admitted.bind id).map GoalRequestAssignmentContracts.referenceJson |>.getD Json.null),
    ("session", destination.map (fun selected => toJson selected.session) |>.getD Json.null),
    ("expected", expected.getD Json.null)]

def json : String := (toJson (cases.map caseJson)).compress

example : cases.length = 20 := by decide

end Conformance.GoalDeliveryInput
