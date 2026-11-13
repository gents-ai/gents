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
  cached : Option Nat
  now : Nat
  timeout : Nat

def groupClockCases : List GroupClockCase :=
  let trigger : Triggers.Groups.EventGroupKey :=
    ⟨"did:agent:a", .trigger "same-id", "config-1", "run-1"⟩
  let callback := { trigger with consumer := .callbackBinding "same-id" }
  [ ⟨"trigger_first_quiescence", ⟨trigger, 100, none⟩, none, 130, 40⟩
  , ⟨"callback_first_quiescence", ⟨callback, 100, none⟩, none, 130, 40⟩
  , ⟨"existing_quiescence_is_not_reset", ⟨callback, 100, some 110⟩, none, 150, 40⟩
  , ⟨"consistent_cache_preserves_deadline", ⟨callback, 100, none⟩, some 100, 150, 40⟩
  , ⟨"evicted_cache_preserves_deadline", ⟨callback, 100, none⟩, none, 150, 40⟩ ]

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
  cachedFirstSeen : Option Nat
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

/-- model-production-gap: the durable group clock (`load_or_create_group`,
`quiesce_group` and `evaluate_group` in gents-automation) stamps first-seen and
quiescence with the wall clock inside a served transaction and keeps no cached
first-seen observation, so no production path takes `now`, `timeout` or a
cached first-seen as an input. -/
@[conformance_family]
def groupClockUnreached : Family ClockInput ClockOutput where
  inputs := groupClockCases.map fun c =>
    { name := c.name, key := KeyWire.ofModel c.clock.key, firstSeen := c.clock.firstSeen
      quiescedAt := c.clock.quiescedAt, cachedFirstSeen := c.cached, now := c.now
      timeout := c.timeout }
  model := fun i =>
    let after := i.clock.quiesce i.now
    { firstSeenAfter := after.firstSeen
      quiescedAtAfter := after.quiescedAt
      quiescedAtAgain := (after.quiesce (i.now + 1)).quiescedAt
      elapsedBefore := (i.clock.observe i.cachedFirstSeen).elapsed i.now i.timeout
      elapsedAfter := (after.observe i.cachedFirstSeen).elapsed i.now i.timeout }


def eventGroupCaptureCases :
    List (String × _root_.EventDelivery.Group.EventGroupState × Triggers.Groups.Candidate) :=
  let key : Triggers.Groups.EventGroupKey :=
    ⟨"did:agent:a", .callbackBinding "summarize", "config-1", "run-1"⟩
  let group : _root_.EventDelivery.Group.EventGroupState := ⟨key, 100, none⟩
  let candidate : Triggers.Groups.Candidate :=
    ⟨key, 2, some 2, 1, false, true⟩
  let trigger := { key with consumer := .trigger "summarize" }
  [ ("eligible_callback", group, candidate)
    , ("quiesced_callback_is_not_delivered", { group with quiescedAt := some 120 }, candidate)
    , ("ineligible_callback", group, { candidate with actualCount := 1 })
    , ("different_config_key", group,
        { candidate with key := { key with consumerConfigKey := "config-2" } })
    , ("trigger_cannot_capture_callback", { group with key := trigger },
        { candidate with key := trigger }) ]

structure CandidateWire where
  key : KeyWire
  actualCount : Nat
  expectedCount : Option Nat
  minimumCount : Nat
  timedOut : Bool
  wellFormed : Bool
  deriving Lean.ToJson

def CandidateWire.toModel (c : CandidateWire) : Triggers.Groups.Candidate :=
  ⟨c.key.toModel, c.actualCount, c.expectedCount, c.minimumCount, c.timedOut, c.wellFormed⟩

structure CaptureInput where
  name : String
  groupKey : KeyWire
  firstSeen : Nat
  quiescedAt : Option Nat
  offered : CandidateWire
  input : String
  deriving Lean.ToJson

structure CaptureOutput where
  eligible : Bool
  materialized : Bool
  markerCountAfter : Nat
  captured : Bool
  capturedOwner : Option String
  capturedInput : Option String
  capturedGroupKey : Option KeyWire
  deriving Lean.ToJson

/-- model-production-gap: production captures a callback group only inside the
served callback scan (`materialize_group` in gents-automation), which loads the
group by the delivery's own key, so a candidate whose key differs from its
group, or a trigger group offered to a callback, has no production path; the
eligible and quiesced cases need a served callback-binding scenario. -/
@[conformance_family]
def groupCaptureUnreached : Family CaptureInput CaptureOutput where
  inputs := eventGroupCaptureCases.map fun (name, clock, c) =>
    { name, groupKey := KeyWire.ofModel clock.key, firstSeen := clock.firstSeen
      quiescedAt := clock.quiescedAt
      offered := { key := KeyWire.ofModel c.key, actualCount := c.actualCount
                   expectedCount := c.expectedCount, minimumCount := c.minimumCount
                   timedOut := c.timedOut, wellFormed := c.wellFormed }
      input := "[\"b\",\"a\"]" }
  model := fun i =>
    let candidate := i.offered.toModel
    let after := Triggers.Groups.reconcile ⟨[]⟩ candidate
    let captured := _root_.EventDelivery.Group.captureCallback
      ⟨i.groupKey.toModel, i.firstSeen, i.quiescedAt⟩ candidate "inv-capture" i.input
      "did:key:agent"
    { eligible := candidate.eligible
      materialized := after.has candidate.key
      markerCountAfter := after.materialized.length
      captured := captured.isSome
      capturedOwner := captured.map (·.ownerNodeDid)
      capturedInput := captured.map (·.input)
      capturedGroupKey := (captured.bind (·.originGroupKey)).map KeyWire.ofModel }

end Conformance.EventDelivery
