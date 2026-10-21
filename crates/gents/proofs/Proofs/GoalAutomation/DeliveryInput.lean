import Proofs.GoalAutomation.RequestAssignment
import Proofs.Triggers.Durable
import Proofs.PluginJob.Keys

namespace GoalAutomation.DeliveryInput

open RequestAssignment

structure Destination where
  session : String
  existing : Bool
  deriving DecidableEq, Repr

def triggerDestination (identity : Triggers.Durable.Identity) (target : Option String)
    (owned : Bool) : Option Destination :=
  (Triggers.Durable.resolveSession identity target owned).map fun session =>
    ⟨session, target.isSome⟩

def pluginDestination (key : PluginJob.SpawnKey) (message : Bool) : Destination :=
  ⟨key.sessionId, message⟩

/-- A committed delivery keeps its signed input, including verified absence.
Only a new ordinary delivery to an existing destination captures that session's
assignment. Session selection and Goal association do not grant graph membership. -/
def prepare (destination : Option Destination) (admitted : Option (Option Reference))
    (captured : Option Reference) (facts : MessageInputFacts) : Option PreparedInput :=
  match admitted with
  | some reference => some (.prepared reference)
  | none => destination.map fun selected =>
      prepareSelectedInput selected.existing captured facts

theorem admitted_input_is_immutable (destination : Option Destination)
    (reference captured : Option Reference) (facts : MessageInputFacts) :
    prepare destination (some reference) captured facts = some (.prepared reference) := by
  rfl

theorem fresh_destination_has_no_inherited_assignment (session : String)
    (facts : MessageInputFacts) :
    prepare (some ⟨session, false⟩) none none facts = some (.prepared none) := by
  simp [prepare, prepareSelectedInput, prepareInput]

theorem captured_input_is_immutable (destination : Destination) (reference : Reference)
    (facts : MessageInputFacts) :
    prepare (some destination) none (some reference) facts = some (.prepared (some reference)) := by
  simp [prepare, prepareSelectedInput, prepareInput]

theorem foreign_trigger_destination_is_refused (identity : Triggers.Durable.Identity)
    (target : String) : triggerDestination identity (some target) false = none := by
  simp [triggerDestination, Triggers.Durable.resolveSession]

end GoalAutomation.DeliveryInput
