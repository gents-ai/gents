import Proofs.Basic
import Proofs.Scheduling
import Proofs.RuntimeReconcile

inductive TriggerKind where
  | schedule
  | event
  | manual
  deriving DecidableEq, Repr

namespace TriggerKind

def toDefraDB : TriggerKind → String
  | .schedule => "schedule"
  | .event => "event"
  | .manual => "manual"

def fromDefraDB? : String → Option TriggerKind
  | "schedule" => some .schedule
  | "event" => some .event
  | "manual" => some .manual
  | _ => none

theorem fromDefraDB_toDefraDB (kind : TriggerKind) :
    fromDefraDB? kind.toDefraDB = some kind := by
  cases kind <;> rfl

end TriggerKind

inductive ConcurrencyMode where
  | parallel
  | serial
  | latestOnly
  deriving DecidableEq, Repr

/-- Omission uses one source-independent default, including graph delivery. -/
def resolveConcurrency (_source : TriggerKind) (configured : Option ConcurrencyMode) :
    ConcurrencyMode := configured.getD .parallel

def resolveEventKind (configured : Option String) : String := configured.getD "created"

theorem concurrency_default_parallel (source : TriggerKind) :
    resolveConcurrency source none = .parallel := rfl

theorem concurrency_source_independent (a b : TriggerKind) (c : Option ConcurrencyMode) :
    resolveConcurrency a c = resolveConcurrency b c := rfl

theorem event_kind_default_created : resolveEventKind none = "created" := rfl

/-- Resolved runtime projections, not authored schedule/event configuration. -/
structure ActiveSchedule where
  triggerId : String
  taskId : String
  enabled : Bool
  deriving DecidableEq, Repr

structure ActiveEventTrigger where
  triggerId : String
  taskId : String
  sourceCollection : String
  eventKind : String
  enabled : Bool
  concurrency : ConcurrencyMode
  deriving DecidableEq, Repr

structure TriggerSnapshot where
  generation : Generation
  activeSchedules : List ActiveSchedule
  activeEventTriggers : List ActiveEventTrigger
  deriving DecidableEq, Repr

namespace TriggerSnapshot

def findSchedule (snap : TriggerSnapshot) (triggerId : String) :
    Option ActiveSchedule :=
  snap.activeSchedules.find? (fun s => s.triggerId = triggerId)

def findEventTrigger (snap : TriggerSnapshot) (triggerId : String) :
    Option ActiveEventTrigger :=
  snap.activeEventTriggers.find? (fun t => t.triggerId = triggerId)

def hasSchedule (snap : TriggerSnapshot) (triggerId : String) : Bool :=
  (snap.findSchedule triggerId).isSome

def hasEventTrigger (snap : TriggerSnapshot) (triggerId : String) : Bool :=
  (snap.findEventTrigger triggerId).isSome

end TriggerSnapshot

structure FireIntent where
  triggerId : Option String
  triggerKind : TriggerKind
  taskId : String
  concurrency : ConcurrencyMode
  deriving Repr

namespace FireIntent

def WellFormed (intent : FireIntent) : Prop :=
  intent.triggerKind = .manual → intent.triggerId = none

instance (intent : FireIntent) : Decidable (FireIntent.WellFormed intent) := by
  unfold FireIntent.WellFormed
  infer_instance

theorem wellFormed_manual_triggerId_none
    {intent : FireIntent}
    (h_wf : intent.WellFormed)
    (h_manual : intent.triggerKind = .manual) :
    intent.triggerId = none :=
  h_wf h_manual

end FireIntent

/-- Selected task and lineage; execution resolves the task under the runtime principal. -/
structure RequestSeed where
  taskId : String
  causedByTriggerId : Option String
  causedByTriggerKind : TriggerKind
  deriving Repr

/-- Logical trigger identity within one runtime principal. Source kind selects a
source, not a concurrency namespace; TriggerWideKey models explicit owner scoping. -/
abbrev TriggerKey := String

namespace FireIntent

def SerialForKey (t : TriggerKey) (intent : FireIntent) : Prop :=
  intent.triggerId = some t → intent.concurrency = .serial

instance (t : TriggerKey) (intent : FireIntent) : Decidable (FireIntent.SerialForKey t intent) := by
  unfold FireIntent.SerialForKey
  infer_instance

theorem serialForKey_target_is_serial
    {t : TriggerKey}
    {intent : FireIntent}
    (h_serial : intent.SerialForKey t)
    (h_triggerId : intent.triggerId = some t) :
    intent.concurrency = .serial :=
  h_serial h_triggerId

end FireIntent

structure AgentRequest where
  id : String
  causedBy : Option TriggerKey
  concurrency : ConcurrencyMode
  isTerminal : Bool
  executionOrigin : ExecutionOrigin
  deriving Repr

structure SystemState where
  requests : List AgentRequest
  deriving Repr

def SystemState.empty : SystemState := { requests := [] }

def SystemState.nonTerminalCountFor
    (s : SystemState) (t : TriggerKey) : Nat :=
  (s.requests.filter (fun r => (r.causedBy == some t) && !r.isTerminal)).length
