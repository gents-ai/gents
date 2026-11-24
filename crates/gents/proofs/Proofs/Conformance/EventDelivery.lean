import Proofs.Automation.EventDelivery
import Proofs.Conformance.Family
import Proofs.Conformance.EventDeliveryCooldown
import Proofs.Conformance.EventDeliveryTransitions
import Proofs.Conformance.EventDeliveryConvergence
import Proofs.Automation.EventDelivery.Instances
import Proofs.Conformance.EventGroups
import Proofs.Conformance.ContractTypes
import Proofs.Conformance.Triggers.Materializer

namespace Conformance.EventDelivery

open _root_.EventDelivery
open Conformance.Contracts

@[conformance_family] def sourceContracts : Conformance.Family SourceKind SourceContract :=
  ⟨[.watcher, .eventSource], sourceContract⟩

structure GroupEvaluationInput where
  members : Nat
  expected : Nat
  deriving Lean.ToJson

structure GroupEvaluationOutput where
  quiesced : Bool
  reason : Option String
  readsOnStart : Nat
  readsAfterRestart : Nat
  deriving Lean.ToJson

def quiesceReasonWire : _root_.EventDelivery.Group.CountQuiesceReason → String
  | .exceedsExpectedCount => "exceeds_expected_count"

@[conformance_family] def groupEvaluation : Conformance.Family GroupEvaluationInput GroupEvaluationOutput :=
  ⟨[⟨2, 1⟩, ⟨1, 2⟩, ⟨2, 2⟩], fun i =>
    let key : Triggers.Groups.EventGroupKey := ⟨"node", .trigger "trigger", "config", "run"⟩
    let after := (⟨key, 0, none⟩ : _root_.EventDelivery.Group.EventGroupState).evaluate i.members i.expected 1
    ⟨after.quiescedAt.isSome,
     (_root_.EventDelivery.Group.countQuiesceReason i.members i.expected).map quiesceReasonWire,
     _root_.EventDelivery.Group.membershipReads none,
     _root_.EventDelivery.Group.membershipReads (some after)⟩⟩

structure GroupClockCase where
  name : String
  clock : _root_.EventDelivery.Group.EventGroupState
  now : Nat
  timeout : Nat

def groupClockCases : List GroupClockCase :=
  let trigger : Triggers.Groups.EventGroupKey :=
    ⟨"did:agent:a", .trigger "same-id", "config-1", "run-1"⟩
  let callback := { trigger with consumer := .callbackBinding "same-id" }
  [ ⟨"trigger_first_quiescence", ⟨trigger, 100, none⟩, 130, 40⟩
  , ⟨"callback_first_quiescence", ⟨callback, 100, none⟩, 130, 40⟩
  , ⟨"existing_quiescence_is_not_reset", ⟨callback, 100, some 110⟩, 150, 40⟩
  , ⟨"durable_first_seen_sets_deadline", ⟨callback, 100, none⟩, 150, 40⟩ ]

structure ConsumerWire where
  kind : String
  triggerId : Option String
  bindingId : Option String
  deriving Lean.ToJson

structure KeyWire where
  nodeDid : String
  consumer : ConsumerWire
  consumerConfigKey : String
  correlation : String
  deriving Lean.ToJson

def ConsumerWire.ofModel : Triggers.Groups.EventConsumer → ConsumerWire
  | .trigger id => ⟨"trigger", some id, none⟩
  | .callbackBinding id => ⟨"callback_binding", none, some id⟩

def ConsumerWire.toModel (c : ConsumerWire) : Triggers.Groups.EventConsumer :=
  match c.triggerId with
  | some id => .trigger id
  | none => .callbackBinding (c.bindingId.getD "")

def KeyWire.ofModel (key : Triggers.Groups.EventGroupKey) : KeyWire :=
  { nodeDid := key.nodeDid, consumer := ConsumerWire.ofModel key.consumer
    consumerConfigKey := key.consumerConfigKey, correlation := key.correlation }

def KeyWire.toModel (key : KeyWire) : Triggers.Groups.EventGroupKey :=
  { nodeDid := key.nodeDid, consumer := key.consumer.toModel
    consumerConfigKey := key.consumerConfigKey, correlation := key.correlation }

structure ClockInput where
  name : String
  key : KeyWire
  firstSeen : Nat
  quiescedAt : Option Nat
  now : Nat
  timeout : Nat
  deriving Lean.ToJson

structure ClockOutput where
  firstSeenAfter : Nat
  quiescedAtAfter : Option Nat
  quiescedAtAgain : Option Nat
  elapsedBefore : Bool
  elapsedAfter : Bool
  deriving Lean.ToJson

def ClockInput.clock (i : ClockInput) : _root_.EventDelivery.Group.EventGroupState :=
  ⟨i.key.toModel, i.firstSeen, i.quiescedAt⟩

/-- The durable group clock (`GroupClock` in gents-automation, which
`quiesce_group` and `evaluate_group` stamp with the wall clock). Production
keeps no cached first-seen observation, so the timeout is observed from the
durable first-seen; `timeout_cache_eviction_preserves_deadline` shows a
consistent cache would observe the same deadline. -/
@[conformance_family]
def groupClock : Family ClockInput ClockOutput where
  inputs := groupClockCases.map fun c =>
    { name := c.name, key := KeyWire.ofModel c.clock.key, firstSeen := c.clock.firstSeen
      quiescedAt := c.clock.quiescedAt, now := c.now, timeout := c.timeout }
  model := fun i =>
    let after := i.clock.quiesce i.now
    { firstSeenAfter := after.firstSeen
      quiescedAtAfter := after.quiescedAt
      quiescedAtAgain := (after.quiesce (i.now + 1)).quiescedAt
      elapsedBefore := (i.clock.observe none).elapsed i.now i.timeout
      elapsedAfter := (after.observe none).elapsed i.now i.timeout }

def callbackGroupKey : Triggers.Groups.EventGroupKey :=
  ⟨"did:agent:a", .callbackBinding "summarize", "config-1", "run-1"⟩

/-- Callback groups the served callback scan offers to `materialize_group`:
complete, already quiesced, and still short of its expected count. -/
def eventGroupCaptureCases :
    List (String × _root_.EventDelivery.Group.EventGroupState × Triggers.Groups.Candidate) :=
  let group : _root_.EventDelivery.Group.EventGroupState := ⟨callbackGroupKey, 100, none⟩
  let candidate : Triggers.Groups.Candidate := ⟨callbackGroupKey, 2, some 2, 1, false, true⟩
  [ ("eligible_callback", group, candidate)
  , ("quiesced_callback_is_not_delivered", { group with quiescedAt := some 120 }, candidate)
  , ("ineligible_callback", group, { candidate with actualCount := 1 }) ]

structure CaptureInput where
  name : String
  quiesced : Bool
  actualCount : Nat
  expectedCount : Option Nat
  deriving Lean.ToJson

/-- `ownedByGroupNode` and `originIsGroup` are whether the captured invocation
belongs to the group's node and names the group as its origin. -/
structure CaptureOutput where
  captured : Bool
  ownedByGroupNode : Bool
  originIsGroup : Bool
  deriving Lean.ToJson

private def captureCase (name : String) :
    _root_.EventDelivery.Group.EventGroupState × Triggers.Groups.Candidate :=
  ((eventGroupCaptureCases.find? (·.1 == name)).map (·.2)).getD
    (⟨callbackGroupKey, 0, none⟩, ⟨callbackGroupKey, 0, none, 0, false, false⟩)

/-- Callback group capture (`captureCallback`), replayed against the callback
scan's `materialize_group`. -/
@[conformance_family]
def groupCapture : Family CaptureInput CaptureOutput where
  inputs := eventGroupCaptureCases.map fun (name, group, candidate) =>
    { name, quiesced := group.quiescedAt.isSome, actualCount := candidate.actualCount,
      expectedCount := candidate.expectedCount }
  model := fun i =>
    let (group, candidate) := captureCase i.name
    let captured := _root_.EventDelivery.Group.captureCallback group candidate "inv-capture"
      "[]" "did:key:agent"
    { captured := captured.isSome
      ownedByGroupNode := captured.any (·.ownerNodeDid == group.key.nodeDid)
      originIsGroup := captured.any (·.originGroupKey == some group.key) }

structure MarkerInput where
  name : String
  priorMaterialized : Bool
  deriving Lean.ToJson

structure MarkerOutput where
  materialized : Bool
  markerCountAfter : Nat
  deriving Lean.ToJson

def triggerGroupKey : Triggers.Groups.EventGroupKey :=
  ⟨"did:agent:a", .trigger "group-trigger", "config-1", "run-1"⟩

/-- Per-group trigger dispatch of a complete group, with and without a request
already materialized for it, replayed against `TriggerEngine::dispatch`. -/
@[conformance_family]
def groupMarker : Family MarkerInput MarkerOutput where
  inputs := [⟨"unmarked_group", false⟩, ⟨"marked_group", true⟩]
  model := fun i =>
    let candidate : Triggers.Groups.Candidate := ⟨triggerGroupKey, 2, some 2, 1, false, true⟩
    let prior : Triggers.Groups.MarkerState :=
      ⟨if i.priorMaterialized then [triggerGroupKey] else []⟩
    let after := _root_.EventDelivery.Group.markGroup ⟨triggerGroupKey, 100, none⟩ prior candidate
    { materialized := after.materialized.length > prior.materialized.length
      markerCountAfter := after.materialized.length }

end Conformance.EventDelivery
