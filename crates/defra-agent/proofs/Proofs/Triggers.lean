import Proofs.Basic
import Proofs.Scheduling
import Proofs.RuntimeReconcile

/-!
# Layer 5: Trigger Engine

Abstract model of the trigger engine (schedule / event / manual fires)
that feeds the request lifecycle. This file introduces the shared
vocabulary: `TriggerKind`, `ConcurrencyMode`, `FireIntent`, `RequestSeed`
and an abstract `dispatch` that a fire goes through before it becomes a
materialized `AgentRequest`.

The theorems (`T1`..`T4`) state the high-level safety properties the
trigger engine must preserve:

* **T1** — a fire only produces a seed if the corresponding trigger is
  active and enabled in the currently published runtime snapshot.
* **T2** — at most one non-terminal request exists per serial trigger.
* **T3** — after a `latestOnly` fire materializes a new request, every
  prior non-terminal request for the same trigger reaches `Superseded`.
* **T4** — every materialized `RequestSeed.causedBy` tuple is consistent
  with its scheduler `ExecutionOrigin`.

Keeping these at the proofs layer lets the Rust runtime evolve its
internal data structures without drifting from the spec.
-/

/-- Category of the originating trigger for a fire intent. -/
inductive TriggerKind where
  | schedule
  | event
  | manual
  deriving DecidableEq, Repr

/-- Concurrency policy declared on a task definition. -/
inductive ConcurrencyMode where
  | parallel
  | serial
  | latestOnly
  deriving DecidableEq, Repr

/-- Abstract schedule record visible in the active snapshot. -/
structure ActiveSchedule where
  triggerId : String
  enabled : Bool
  deriving DecidableEq, Repr

/-- Abstract event-trigger record visible in the active snapshot. -/
structure ActiveEventTrigger where
  triggerId : String
  enabled : Bool
  deriving DecidableEq, Repr

/-- Trigger-layer view of the active runtime snapshot. The reconcile
    layer already owns `ActiveRuntimeSnapshot`; the trigger engine needs
    a richer projection that includes active schedules and event
    triggers. Keeping it as a separate structure avoids churn in the
    reconcile proof while still letting us state theorems over a
    published runtime generation. -/
structure TriggerSnapshot where
  generation : Generation
  activeSchedules : List ActiveSchedule
  activeEventTriggers : List ActiveEventTrigger
  deriving DecidableEq, Repr

namespace TriggerSnapshot

/-- Lookup a schedule by trigger id. -/
def findSchedule (snap : TriggerSnapshot) (triggerId : String) :
    Option ActiveSchedule :=
  snap.activeSchedules.find? (fun s => s.triggerId = triggerId)

/-- Lookup an event trigger by trigger id. -/
def findEventTrigger (snap : TriggerSnapshot) (triggerId : String) :
    Option ActiveEventTrigger :=
  snap.activeEventTriggers.find? (fun t => t.triggerId = triggerId)

/-- Whether a schedule trigger id is active in this snapshot. -/
def hasSchedule (snap : TriggerSnapshot) (triggerId : String) : Bool :=
  (snap.findSchedule triggerId).isSome

/-- Whether an event trigger id is active in this snapshot. -/
def hasEventTrigger (snap : TriggerSnapshot) (triggerId : String) : Bool :=
  (snap.findEventTrigger triggerId).isSome

end TriggerSnapshot

/-- Render-time input to the trigger dispatcher. The render inputs
    themselves are abstracted away; only the fields relevant to
    admissibility are modeled. -/
structure FireIntent where
  triggerId : Option String
  triggerKind : TriggerKind
  taskId : String
  concurrency : ConcurrencyMode
  deriving Repr

/-- Minimal projection of a materialized `AgentRequest` carrying the
    lineage fields established by the trigger engine. -/
structure RequestSeed where
  causedByTriggerId : Option String
  causedByTriggerKind : TriggerKind
  deriving Repr

/-- Abstract dispatch function. Produces a `RequestSeed` iff the intent
    is admissible given the snapshot. The concrete rules are filled in
    by the theorems below; `sorry` is used as a placeholder so the
    statements typecheck without committing to one operational
    definition. -/
def dispatch (snap : TriggerSnapshot) (intent : FireIntent) :
    Option RequestSeed :=
  sorry
