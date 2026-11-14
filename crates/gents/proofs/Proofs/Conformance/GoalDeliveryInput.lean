import Proofs.Automation.GoalAutomation.DeliveryInput
import Proofs.Conformance.GoalRequestAssignment
import Proofs.Conformance.Family

namespace Conformance.GoalDeliveryInput

open GoalAutomation GoalAutomation.RequestAssignment Lean

/-- A delivery case under the supported topology: trigger and plugin-job
deliveries stage into this runtime's own sessions, whose Goal is read in the
staging transaction (`observationVerified` is that canonical read). Remote
destinations and their replicated Goal observations are not a supported
delivery (`Recovery.KillObservation`); cross-node ownership is deferred to
gents-ai/gents#3591. -/
structure Case where
  name : String
  plugin : Bool := false
  existing : Bool := true
  owned : Bool := true
  observed : Option Reference := some ⟨2, some 200⟩
  captured : Option Reference := none
  admitted : Option (Option Reference) := none
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
  ]

structure DeliveryCase where
  name : String
  plugin : Bool
  existing : Bool
  owned : Bool
  observed : Option Reference
  captured : Option Reference
  admitted : Bool
  admittedReference : Option Reference
  deriving ToJson

def DeliveryCase.ofCase (c : Case) : DeliveryCase :=
  ⟨c.name, c.plugin, c.existing, c.owned, c.observed, c.captured, c.admitted.isSome,
    c.admitted.bind id⟩

def DeliveryCase.destination (i : DeliveryCase) : Option GoalAutomation.DeliveryInput.Destination :=
  if i.plugin then some (GoalAutomation.DeliveryInput.pluginDestination ⟨i.name, "worker", 1⟩ i.existing)
  else GoalAutomation.DeliveryInput.triggerDestination ⟨"1", "delivery", "Task", i.name⟩
    (if i.existing then some "10" else none) i.owned

structure PreparedWire where
  kind : String
  reference : Option Reference
  deriving ToJson

def PreparedWire.ofModel : PreparedInput → PreparedWire
  | .pending => ⟨"pending", none⟩
  | .prepared reference => ⟨"prepared", reference⟩

def DeliveryCase.expected (i : DeliveryCase) : Option PreparedWire :=
  let facts : MessageInputFacts := {
    owner := 1, session := 10, sessionReadable := i.existing,
    observationVerified := true,
    observed := i.observed.map fun reference => ⟨1, 10, reference.goal, reference.root⟩,
    callerReference := some ⟨9, some 900⟩ }
  (GoalAutomation.DeliveryInput.prepare i.destination (if i.admitted then some i.admittedReference else none)
    i.captured facts).map PreparedWire.ofModel

structure DeliveryOutput where
  session : Option String
  expected : Option PreparedWire
  deriving ToJson

@[conformance_family]
def delivery : Family DeliveryCase DeliveryOutput where
  inputs := cases.map DeliveryCase.ofCase
  model := fun i => { session := i.destination.map (·.session), expected := i.expected }

/-- A manual Task run is the client's trigger delivery into a destination this
runtime owns; plugin deliveries and foreign destinations never reach it. -/
@[conformance_family]
def manualTaskRun : Family DeliveryCase (Option PreparedWire) where
  inputs := (cases.filter fun c => !c.plugin && c.owned).map DeliveryCase.ofCase
  model := DeliveryCase.expected

example : cases.length = 14 := by decide

example : (cases.filter fun c => !c.plugin && c.owned).length = 7 := by decide

end Conformance.GoalDeliveryInput
