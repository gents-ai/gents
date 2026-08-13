import Proofs.EventDelivery
import Proofs.Conformance.ContractTypes

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
  ]

def transitionCaseCount : Nat := transitionCases.length

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
  , { name := "SubagentSource"
    , dedupePolicy := DedupePolicy.toContract .monotoneOnce
    , rescanBoundedBy := SubagentSource.subagentSourceSrc.rescanBoundedBy
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

def subagentSourceTrace : ConvergenceTraceRow :=
  { name := "subagent_orphan_rescan_handle"
  , instanceName := "SubagentSource"
  , initialWorld := World.empty
  , actions :=
      [ .persist (doc "tool-call-1")
      , .rescanTick
      , .handle (doc "tool-call-1") ]
  , finalWorld := mkWorld [doc "tool-call-1"] [] [doc "tool-call-1"] [doc "tool-call-1"]
  , status := "substantive"
  }

def convergenceTraces : List ConvergenceTraceRow :=
  [ watcherTrace, eventSourceTrace, subagentSourceTrace ]

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

end Conformance.EventDelivery
