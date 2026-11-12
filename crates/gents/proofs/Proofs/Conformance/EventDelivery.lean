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

def quiesceReasonWire : _root_.EventDelivery.Group.QuiesceReason → String
  | .exceedsExpectedCount => "exceeds_expected_count"

@[conformance_family] def groupEvaluation : Conformance.Family GroupEvaluationInput GroupEvaluationOutput :=
  ⟨[⟨2, 1⟩, ⟨1, 2⟩, ⟨2, 2⟩], fun i =>
    let key : Triggers.Groups.EventGroupKey := ⟨"node", .trigger "trigger", "config", "run"⟩
    let after := (⟨key, 0, none⟩ : _root_.EventDelivery.Group.EventGroupState).evaluate i.members i.expected 1
    ⟨after.quiescedAt.isSome,
     (_root_.EventDelivery.Group.quiesceReason i.members i.expected).map quiesceReasonWire,
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

def eventGroupClockCasesJson : String :=
  jsonArray (groupClockCases.map fun c =>
    let after := c.clock.quiesce c.now
    let again := after.quiesce (c.now + 1)
    "{\"name\":" ++ jsonString c.name
      ++ ",\"key\":" ++ Conformance.EventGroupContracts.keyJson c.clock.key
      ++ ",\"first_seen\":" ++ toString c.clock.firstSeen
      ++ ",\"quiesced_at\":" ++ Conformance.EventGroupContracts.jsonOptionNat c.clock.quiescedAt
      ++ ",\"cached_first_seen\":" ++ Conformance.EventGroupContracts.jsonOptionNat c.cached
      ++ ",\"now\":" ++ toString c.now ++ ",\"timeout\":" ++ toString c.timeout
      ++ ",\"first_seen_after\":" ++ toString after.firstSeen
      ++ ",\"quiesced_at_after\":" ++ Conformance.EventGroupContracts.jsonOptionNat after.quiescedAt
      ++ ",\"quiesced_at_again\":" ++ Conformance.EventGroupContracts.jsonOptionNat again.quiescedAt
      ++ ",\"elapsed_before\":" ++ toString ((c.clock.observe c.cached).elapsed c.now c.timeout)
      ++ ",\"elapsed_after\":" ++ toString ((after.observe c.cached).elapsed c.now c.timeout) ++ "}")

def eventGroupClockCaseCount : Nat := groupClockCases.length


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

def eventGroupCaptureCasesJson : String :=
  jsonArray (eventGroupCaptureCases.map fun (name, clock, c) =>
    let input := "[\"b\",\"a\"]"
    let captured := _root_.EventDelivery.Group.captureCallback clock c "inv-capture" input "did:key:agent"
    "{\"name\":" ++ jsonString name
      ++ ",\"group_key\":" ++ Conformance.EventGroupContracts.keyJson clock.key
      ++ ",\"quiesced_at\":" ++ Conformance.EventGroupContracts.jsonOptionNat clock.quiescedAt
      ++ ",\"candidate\":" ++ Conformance.EventGroupContracts.scenarioJson
        ⟨name, ⟨[]⟩, c⟩
      ++ ",\"input\":" ++ jsonString input
      ++ ",\"captured\":" ++ toString captured.isSome
      ++ ",\"captured_owner\":" ++ jsonOptionalString (captured.map (·.ownerNodeDid))
      ++ ",\"captured_input\":" ++ jsonOptionalString (captured.map (·.input))
      ++ ",\"captured_group_key\":" ++
        ((captured.bind (·.originGroupKey)).map Conformance.EventGroupContracts.keyJson).getD "null"
      ++ "}")

def eventGroupCaptureCaseCount : Nat := eventGroupCaptureCases.length

end Conformance.EventDelivery
