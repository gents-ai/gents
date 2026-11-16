import Proofs.Automation.Triggers.Lineage
import Proofs.Session.Enrollment.RequestAdmission

/-! # Lineage of a runtime-materialized request
The runtime materializes a fired Trigger's request itself, signed by the
Agent that will run it. Before anything is written, the fire's provenance must
be coherent: only an Event fire names the document that caused it, and an
Event fire always does. A coherent lineage is persisted on the request as
given, so the request can be found by the Trigger that caused it. -/

namespace MaterializedLineage

inductive ProvenanceRefusal where | eventWithoutSource | sourceWithoutEvent
  deriving DecidableEq, Repr

def provenanceRefusal (kind : Option TriggerKind) (sourceDocument : Bool) :
    Option ProvenanceRefusal :=
  match kind, sourceDocument with
  | some .event, true => none
  | some .event, false => some .eventWithoutSource
  | _, true => some .sourceWithoutEvent
  | _, false => none

structure Fire where
  kind : Option TriggerKind
  sourceDocument : Bool
  deriving DecidableEq, Repr

structure Materialized where
  refusal : Option ProvenanceRefusal
  causedByTriggerKind : Option TriggerKind
  admission : Option Enrollment.AgentRequestAdmissionKind
  deriving DecidableEq, Repr

/-- The runtime writes the request under the Agent's own signature, so its
admission is `localSelf`; a refused provenance writes no request. -/
def materialize (fire : Fire) : Materialized :=
  match provenanceRefusal fire.kind fire.sourceDocument with
  | some refusal => { refusal := some refusal, causedByTriggerKind := none, admission := none }
  | none =>
    { refusal := none, causedByTriggerKind := fire.kind,
      admission := some .localSelf }

def fires : List Fire :=
  [none, some .manual, some .schedule, some .event].flatMap fun kind =>
    [false, true].map fun sourceDocument => { kind, sourceDocument }

theorem refused_provenance_writes_no_request : ∀ fire ∈ fires,
    provenanceRefusal fire.kind fire.sourceDocument ≠ none →
      (materialize fire).admission = none := by decide

theorem an_event_fire_always_names_its_source : ∀ fire ∈ fires,
    (materialize fire).causedByTriggerKind = some .event → fire.sourceDocument = true := by
  decide

theorem scheduled_materialization_persists_trigger_lineage :
    materialize { kind := some .schedule, sourceDocument := false } =
      { refusal := none, causedByTriggerKind := some .schedule,
        admission := some .localSelf } := by decide

end MaterializedLineage
