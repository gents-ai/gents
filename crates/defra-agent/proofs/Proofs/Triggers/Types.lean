import Proofs.Basic
import Proofs.Scheduling
import Proofs.RuntimeReconcile

/-!
# Trigger Types

Shared trigger vocabulary and cross-request state used by dispatch and reachability proofs.
-/

/-- Category of the originating trigger for a fire intent. -/
inductive TriggerKind where
  | schedule
  | event
  | manual
  deriving DecidableEq, Repr

namespace TriggerKind

/-- String vocabulary persisted in `AgentRequest.caused_by_trigger_kind`. -/
def toDefraDB : TriggerKind → String
  | .schedule => "schedule"
  | .event => "event"
  | .manual => "manual"

/-- Parse the persisted `AgentRequest.caused_by_trigger_kind` vocabulary. -/
def fromDefraDB? : String → Option TriggerKind
  | "schedule" => some .schedule
  | "event" => some .event
  | "manual" => some .manual
  | _ => none

theorem fromDefraDB_toDefraDB (kind : TriggerKind) :
    fromDefraDB? kind.toDefraDB = some kind := by
  cases kind <;> rfl

end TriggerKind

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

/-- Concrete event-trigger record visible in the active snapshot.

Mirrors the Rust-side `ResolvedEventTrigger`: the trigger engine needs
more than `(triggerId, enabled)` to dispatch an event fire — it must
know which source collection and event kind the subscription is
watching, which task to render, and the declared concurrency mode.

This parallels `ActiveSchedule` in spirit; `ActiveSchedule` has stayed
minimal because the schedule-kind theorems so far only exercise the
`enabled` gate, while the event path already needs the richer shape for
the subscription join that `EventSource` performs in Rust. -/
structure ActiveEventTrigger where
  triggerId : String
  taskId : String
  sourceCollection : String
  eventKind : String
  enabled : Bool
  concurrency : ConcurrencyMode
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

namespace FireIntent

/--
Boundary well-formedness for trigger-engine inputs.

The runtime's manual source never constructs a manual fire intent with a
non-null `triggerId`; manual fires are operator initiated, not tied to a
trigger document. The proof model keeps raw `FireIntent` flexible, then uses
`WellFormed` to describe the admissible trace boundary.
-/
def WellFormed (intent : FireIntent) : Prop :=
  intent.triggerKind = .manual → intent.triggerId = none

instance (intent : FireIntent) : Decidable (FireIntent.WellFormed intent) := by
  unfold FireIntent.WellFormed
  infer_instance

/-- For a well-formed manual intent, the trigger id must be `none`. -/
theorem wellFormed_manual_triggerId_none
    {intent : FireIntent}
    (h_wf : intent.WellFormed)
    (h_manual : intent.triggerKind = .manual) :
    intent.triggerId = none :=
  h_wf h_manual

end FireIntent

/-- Minimal projection of a materialized `AgentRequest` carrying the
    lineage fields established by the trigger engine. -/
structure RequestSeed where
  causedByTriggerId : Option String
  causedByTriggerKind : TriggerKind
  deriving Repr


/-- A trigger is identified by `(triggerId, triggerKind)`. Pairing both
    avoids collisions between, e.g., a schedule and an event trigger
    that happen to share the same document id. -/
abbrev TriggerKey := String × TriggerKind

namespace FireIntent

/--
Boundary predicate saying that any fire intent targeting tuple `t` must use
`.serial` concurrency.

Intents that do not target `t` are unconstrained; the implication only becomes
load-bearing when both the trigger id and kind match `t`.
-/
def SerialForKey (t : TriggerKey) (intent : FireIntent) : Prop :=
  intent.triggerId = some t.1 → intent.triggerKind = t.2 → intent.concurrency = .serial

instance (t : TriggerKey) (intent : FireIntent) : Decidable (FireIntent.SerialForKey t intent) := by
  unfold FireIntent.SerialForKey
  infer_instance

/-- A `SerialForKey` intent targeting `t` must use `.serial` concurrency. -/
theorem serialForKey_target_is_serial
    {t : TriggerKey}
    {intent : FireIntent}
    (h_serial : intent.SerialForKey t)
    (h_triggerId : intent.triggerId = some t.1)
    (h_kind : intent.triggerKind = t.2) :
    intent.concurrency = .serial :=
  h_serial h_triggerId h_kind

end FireIntent

/-- Spec-layer projection of an AgentRequest sufficient to state the
    trigger-engine theorems. The real `AgentRequest` carries far more
    state; here we only track the fields the trigger engine reasons
    about. -/
structure AgentRequest where
  id : String
  causedBy : Option TriggerKey
  concurrency : ConcurrencyMode
  /-- Mirror of `RequestState`-level terminality without forcing the
      trigger layer to unfold the full lifecycle state.

      **Projection boundary (#605, deadline expiry):** the Rust gate maps a
      persisted `claimed`/`processing` row whose claim `deadline` (plus a
      fixed grace) has passed to `isTerminal = true`. This is sound because
      the owning loop enforces the same deadline in-memory
      (`await_with_request_deadline` aborts the attempt at the deadline), so
      a past-deadline row can only be a wedged orphan whose owner will never
      terminate it — the projection performs the `lifecycleTerminateStep`
      the dead owner cannot. Conformance:
      `scheduling.rs::serial_gate_ignores_expired_claims`. -/
  isTerminal : Bool
  /-- Execution origin inherited from the trigger engine. -/
  executionOrigin : ExecutionOrigin
  deriving Repr

/-- Aggregate system state observed by the trigger engine for
    cross-request reasoning.

    **Projection boundary (#605, agent scope):** `requests` is ONE agent's
    request view. `TriggerKey` is only unique per agent — replicated fleets
    share human-chosen schedule ids — so the Rust materialization of this
    state scopes every query by the dispatching behavior's `agent_did`;
    without that scope, T2's per-tuple bound would be enforced fleet-wide
    across unrelated agents (the observed #605 outage). Conformance:
    `scheduling.rs::serial_gate_is_scoped_by_agent_did` /
    `supersede_only_touches_own_agent_requests`. -/
structure SystemState where
  requests : List AgentRequest
  deriving Repr

/-- The initial empty system state used as the base case for `Reachable`. -/
def SystemState.empty : SystemState := { requests := [] }

/--
Count of non-terminal requests with matching `causedBy` tuple.
This is the quantity T2 bounds.
-/
def SystemState.nonTerminalCountFor
    (s : SystemState) (t : TriggerKey) : Nat :=
  (s.requests.filter (fun r => (r.causedBy == some t) && !r.isTerminal)).length
