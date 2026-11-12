import Proofs.Automation.EventDelivery
import Proofs.Conformance.Family
import Proofs.Conformance.EventDeliveryTransitions
import Proofs.Automation.EventDelivery.Instances
import Proofs.Conformance.EventGroups
import Proofs.Conformance.ContractTypes

namespace Conformance.EventDelivery

open _root_.EventDelivery
open Conformance.Contracts

private def doc (s : String) : DocId := ⟨s⟩
private def mkWorld (ps sq proc h : List DocId) : World := ⟨ps, sq, proc, h⟩

@[conformance_family] def sourceContracts : Conformance.Family SourceKind SourceContract :=
  ⟨[.watcher, .eventSource], sourceContract⟩

structure ConvergenceTraceRow where
  name           : String
  instanceName   : String
  initialWorld   : World
  actions        : List Action
  finalWorld     : World
  status         : String

def watcherTrace : ConvergenceTraceRow :=
  { name := "watcher_persist_rescan_handle"
  , instanceName := "Watcher"
  , initialWorld := World.empty
  , actions :=
      [ .persist (doc "req-1")
      , .rescanTick
      , .handle (doc "req-1") ]
  , finalWorld := mkWorld [doc "req-1"] [] [doc "req-1"] [doc "req-1"]
  , status := "substantive"
  }

def eventSourceTrace : ConvergenceTraceRow :=
  { name := "event_source_persist_rescan_handle"
  , instanceName := "EventSource"
  , initialWorld := World.empty
  , actions :=
      [ .persist (doc "doc-1")
      , .rescanTick
      , .handle (doc "doc-1") ]
  , finalWorld := mkWorld [doc "doc-1"] [] [doc "doc-1"] [doc "doc-1"]
  , status := "substantive"
  }

def convergenceTraces : List ConvergenceTraceRow :=
  [ watcherTrace, eventSourceTrace ]

def convergenceTraceCount : Nat := convergenceTraces.length

def jsonOptionString : Option String → String := jsonOptionalString

def docIdJson (d : DocId) : String := jsonString d.raw

def docIdListJson (ds : List DocId) : String :=
  jsonArray (ds.map docIdJson)

def worldJson (w : World) : String :=
  "{"
    ++ "\"persistent_set\":" ++ docIdListJson w.persistentSet ++ ","
    ++ "\"subscription_queue\":" ++ docIdListJson w.subscriptionQueue ++ ","
    ++ "\"processed_set\":" ++ docIdListJson w.processedSet ++ ","
    ++ "\"handled\":" ++ docIdListJson w.handled
    ++ "}"

def actionJson : Action → String
  | .persist d => "{\"kind\":\"persist\",\"doc\":" ++ docIdJson d ++ "}"
  | .depersist d => "{\"kind\":\"depersist\",\"doc\":" ++ docIdJson d ++ "}"
  | .enqueue d => "{\"kind\":\"enqueue\",\"doc\":" ++ docIdJson d ++ "}"
  | .drop d => "{\"kind\":\"drop\",\"doc\":" ++ docIdJson d ++ "}"
  | .deliverFromQueue d =>
      "{\"kind\":\"deliver_from_queue\",\"doc\":" ++ docIdJson d ++ "}"
  | .rescanTick => "{\"kind\":\"rescan_tick\"}"
  | .handle d => "{\"kind\":\"handle\",\"doc\":" ++ docIdJson d ++ "}"
  | .release d => "{\"kind\":\"release\",\"doc\":" ++ docIdJson d ++ "}"

def convergenceTraceRowJson (r : ConvergenceTraceRow) : String :=
  "{"
    ++ "\"name\":" ++ jsonString r.name ++ ","
    ++ "\"instance_name\":" ++ jsonString r.instanceName ++ ","
    ++ "\"initial_world\":" ++ worldJson r.initialWorld ++ ","
    ++ "\"actions\":" ++ jsonArray (r.actions.map actionJson) ++ ","
    ++ "\"final_world\":" ++ worldJson r.finalWorld ++ ","
    ++ "\"status\":" ++ jsonString r.status
    ++ "}"

def convergenceTracesJson : String :=
  jsonArray (convergenceTraces.map convergenceTraceRowJson)



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
