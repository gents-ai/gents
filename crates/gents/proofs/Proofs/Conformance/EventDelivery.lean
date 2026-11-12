import Proofs.Automation.EventDelivery
import Proofs.Conformance.EventGroups
import Proofs.Conformance.ContractTypes
import Proofs.Conformance.Family

namespace Conformance.EventDelivery

open _root_.EventDelivery
open Conformance.Contracts

structure TransitionCase where
  name   : String
  pre    : World
  action : Action
  post   : World

private def doc (s : String) : DocId := { raw := s }

private def w0 : World := World.empty

private def mkWorld
    (ps : List DocId) (sq : List DocId) (proc : List DocId) (h : List DocId) : World :=
  { persistentSet := ps, subscriptionQueue := sq, processedSet := proc, handled := h }

def transitionCases : List TransitionCase :=
  [
    { name   := "persist_into_empty"
    , pre    := w0
    , action := .persist (doc "a")
    , post   := mkWorld [doc "a"] [] [] []
    }
  ,
    { name   := "persist_extends_set"
    , pre    := mkWorld [doc "a"] [] [] []
    , action := .persist (doc "b")
    , post   := mkWorld [doc "b", doc "a"] [] [] []
    }
  ,
    { name   := "depersist_removes"
    , pre    := mkWorld [doc "a", doc "b"] [] [] []
    , action := .depersist (doc "a")
    , post   := mkWorld [doc "b"] [] [] []
    }
  ,
    { name   := "enqueue_from_persistent"
    , pre    := mkWorld [doc "a"] [] [] []
    , action := .enqueue (doc "a")
    , post   := mkWorld [doc "a"] [doc "a"] [] []
    }
  ,
    { name   := "drop_from_queue"
    , pre    := mkWorld [doc "a"] [doc "a"] [] []
    , action := .drop (doc "a")
    , post   := mkWorld [doc "a"] [] [] []
    }
  ,
    { name   := "deliver_consumes_queue"
    , pre    := mkWorld [doc "a"] [doc "a"] [] []
    , action := .deliverFromQueue (doc "a")
    , post   := mkWorld [doc "a"] [] [] []
    }
  ,
    { name   := "rescan_on_empty"
    , pre    := w0
    , action := .rescanTick
    , post   := w0
    }
  ,
    { name   := "rescan_fills_queue"
    , pre    := mkWorld [doc "a"] [] [] []
    , action := .rescanTick
    , post   := mkWorld [doc "a"] [doc "a"] [] []
    }
  ,
    { name   := "rescan_skips_processed"
    , pre    := mkWorld [doc "a", doc "b"] [] [doc "a"] []
    , action := .rescanTick
    , post   := mkWorld [doc "a", doc "b"] [doc "b"] [doc "a"] []
    }
  ,
    { name   := "handle_legal_drains_queue"
    , pre    := mkWorld [doc "a"] [doc "a"] [] []
    , action := .handle (doc "a")
    , post   := mkWorld [doc "a"] [] [doc "a"] [doc "a"]
    }
  ,
    { name   := "handle_marks_processed"
    , pre    := mkWorld [doc "a", doc "b"] [doc "a", doc "b"] [] []
    , action := .handle (doc "a")
    , post   := mkWorld [doc "a", doc "b"] [doc "b"] [doc "a"] [doc "a"]
    }
  ,
    { name   := "enqueue_twice_multiset"
    , pre    := mkWorld [doc "a"] [doc "a"] [] []
    , action := .enqueue (doc "a")
    , post   := mkWorld [doc "a"] [doc "a", doc "a"] [] []
    }
  ,
    { name   := "rescan_prepends_to_queue"
    , pre    := mkWorld [doc "a"] [doc "z"] [] []
    , action := .rescanTick
    , post   := mkWorld [doc "a"] [doc "a", doc "z"] [] []
    }
  ,


    { name   := "handle_ready_trigger_preserves_pending_sibling"
    , pre    :=
        mkWorld
          [doc "trigger-ready:doc-a", doc "trigger-pending:doc-a"]
          [doc "trigger-ready:doc-a", doc "trigger-pending:doc-a"]
          []
          []
    , action := .handle (doc "trigger-ready:doc-a")
    , post   :=
        mkWorld
          [doc "trigger-ready:doc-a", doc "trigger-pending:doc-a"]
          [doc "trigger-pending:doc-a"]
          [doc "trigger-ready:doc-a"]
          [doc "trigger-ready:doc-a"]
    }
  ,


    { name   := "release_returns_overtaken_doc"
    , pre    := mkWorld [doc "goal-cont-3"] [] [doc "goal-cont-3"] [doc "goal-cont-3"]
    , action := .release (doc "goal-cont-3")
    , post   := mkWorld [doc "goal-cont-3"] [] [] [doc "goal-cont-3"]
    }
  ,
    { name   := "release_keeps_other_marks"
    , pre    := mkWorld [doc "a", doc "b"] [] [doc "a", doc "b"] [doc "b", doc "a"]
    , action := .release (doc "a")
    , post   := mkWorld [doc "a", doc "b"] [] [doc "b"] [doc "b", doc "a"]
    }
  ]

structure SourceInstanceRow where
  name             : String
  dedupePolicy     : String
  rescanBoundedBy  : Nat
  deviation        : Option String



def sourceInstances : List SourceInstanceRow :=
  [ { name := "Watcher"
    , dedupePolicy := DedupePolicy.toContract .ttlCooldown
    , rescanBoundedBy := 1
    , deviation := none
    }
  , { name := "EventSource"
    , dedupePolicy := DedupePolicy.toContract .monotoneOnce
    , rescanBoundedBy := EventSource.eventSourceSrc.rescanBoundedBy
    , deviation := none
    }
  ]

def sourceInstanceCount : Nat := sourceInstances.length

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

def transitionCaseJson (c : TransitionCase) : String :=
  "{"
    ++ "\"name\":" ++ jsonString c.name ++ ","
    ++ "\"pre\":" ++ worldJson c.pre ++ ","
    ++ "\"action\":" ++ actionJson c.action ++ ","
    ++ "\"post\":" ++ worldJson c.post
    ++ "}"

def transitionCasesJson : String :=
  jsonArray (transitionCases.map transitionCaseJson)

def sourceInstanceRowJson (r : SourceInstanceRow) : String :=
  "{"
    ++ "\"name\":" ++ jsonString r.name ++ ","
    ++ "\"dedupe_policy\":" ++ jsonString r.dedupePolicy ++ ","
    ++ "\"rescan_bounded_by\":" ++ toString r.rescanBoundedBy ++ ","
    ++ "\"deviation\":" ++ jsonOptionString r.deviation
    ++ "}"

def sourceInstancesJson : String :=
  jsonArray (sourceInstances.map sourceInstanceRowJson)

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

structure KeyWire where
  nodeDid : String
  consumerKind : String
  consumerId : String
  consumerConfigKey : String
  correlation : String
  deriving Lean.ToJson

def KeyWire.ofModel (key : Triggers.Groups.EventGroupKey) : KeyWire :=
  { nodeDid := key.nodeDid, consumerKind := key.consumer.kind, consumerId := key.consumer.id
    consumerConfigKey := key.consumerConfigKey, correlation := key.correlation }

def KeyWire.toModel (key : KeyWire) : Triggers.Groups.EventGroupKey :=
  { nodeDid := key.nodeDid
    consumer := if key.consumerKind == "trigger" then .trigger key.consumerId
      else .callbackBinding key.consumerId
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

end Conformance.EventDelivery
