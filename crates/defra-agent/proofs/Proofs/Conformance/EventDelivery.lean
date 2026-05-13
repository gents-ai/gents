import Proofs.EventDelivery

namespace Conformance.EventDelivery

open _root_.EventDelivery

/-! ## Family 1 — Transition cases -/

/-- A single (pre, action, post) transition witness with a name. -/
structure TransitionCase where
  name   : String
  pre    : World
  action : Action
  post   : World

/-- Helper to build a fresh DocId. -/
private def doc (s : String) : DocId := { raw := s }

/-- The empty initial world used by most cases. -/
private def w0 : World := World.empty

/-- World constructor with all four fields explicit. -/
private def mkWorld
    (ps : List DocId) (sq : List DocId) (proc : List DocId) (h : List DocId) : World :=
  { persistentSet := ps, subscriptionQueue := sq, processedSet := proc, handled := h }

/-- 13 witness rows exercising every Transition constructor + a few
    non-trivial variants. The `handle`-already-processed rejection is
    structurally enforced by the inductive (no row needed). -/
def transitionCases : List TransitionCase :=
  [ -- persist on empty world
    { name   := "persist_into_empty"
    , pre    := w0
    , action := .persist (doc "a")
    , post   := mkWorld [doc "a"] [] [] []
    }
  , -- persist after an existing doc
    { name   := "persist_extends_set"
    , pre    := mkWorld [doc "a"] [] [] []
    , action := .persist (doc "b")
    , post   := mkWorld [doc "b", doc "a"] [] [] []
    }
  , -- depersist
    { name   := "depersist_removes"
    , pre    := mkWorld [doc "a", doc "b"] [] [] []
    , action := .depersist (doc "a")
    , post   := mkWorld [doc "b"] [] [] []
    }
  , -- enqueue
    { name   := "enqueue_from_persistent"
    , pre    := mkWorld [doc "a"] [] [] []
    , action := .enqueue (doc "a")
    , post   := mkWorld [doc "a"] [doc "a"] [] []
    }
  , -- drop
    { name   := "drop_from_queue"
    , pre    := mkWorld [doc "a"] [doc "a"] [] []
    , action := .drop (doc "a")
    , post   := mkWorld [doc "a"] [] [] []
    }
  , -- deliverFromQueue
    { name   := "deliver_consumes_queue"
    , pre    := mkWorld [doc "a"] [doc "a"] [] []
    , action := .deliverFromQueue (doc "a")
    , post   := mkWorld [doc "a"] [] [] []
    }
  , -- rescanTick with empty persistent set
    { name   := "rescan_on_empty"
    , pre    := w0
    , action := .rescanTick
    , post   := w0
    }
  , -- rescanTick on one persistent, none processed → queue gets it
    { name   := "rescan_fills_queue"
    , pre    := mkWorld [doc "a"] [] [] []
    , action := .rescanTick
    , post   := mkWorld [doc "a"] [doc "a"] [] []
    }
  , -- rescanTick with mixed processed/unprocessed
    { name   := "rescan_skips_processed"
    , pre    := mkWorld [doc "a", doc "b"] [] [doc "a"] []
    , action := .rescanTick
    , post   := mkWorld [doc "a", doc "b"] [doc "b"] [doc "a"] []
    }
  , -- handle: legal path (queued + not processed)
    { name   := "handle_legal_drains_queue"
    , pre    := mkWorld [doc "a"] [doc "a"] [] []
    , action := .handle (doc "a")
    , post   := mkWorld [doc "a"] [] [doc "a"] [doc "a"]
    }
  , -- handle: idempotence (post-handle, processedSet contains d)
    { name   := "handle_marks_processed"
    , pre    := mkWorld [doc "a", doc "b"] [doc "a", doc "b"] [] []
    , action := .handle (doc "a")
    , post   := mkWorld [doc "a", doc "b"] [doc "b"] [doc "a"] [doc "a"]
    }
  , -- enqueue when queue already has the doc adds another instance
    { name   := "enqueue_twice_multiset"
    , pre    := mkWorld [doc "a"] [doc "a"] [] []
    , action := .enqueue (doc "a")
    , post   := mkWorld [doc "a"] [doc "a", doc "a"] [] []
    }
  , -- rescanTick prepends, not appends
    { name   := "rescan_prepends_to_queue"
    , pre    := mkWorld [doc "a"] [doc "z"] [] []
    , action := .rescanTick
    , post   := mkWorld [doc "a"] [doc "a", doc "z"] [] []
    }
  ]

def transitionCaseCount : Nat := transitionCases.length

/-! ## Family 2 — Source instance metadata -/

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
    , rescanBoundedBy := SourceInstance.unboundedRescan
    , deviation := some "event_source_lacks_periodic_rescan"
    }
  , { name := "SubagentSource"
    , dedupePolicy := DedupePolicy.toContract .monotoneOnce
    , rescanBoundedBy := SourceInstance.unboundedRescan
    , deviation := some "subagent_source_lacks_live_rescan"
    }
  ]

def sourceInstanceCount : Nat := sourceInstances.length

/-! ## Family 3 — Convergence traces -/

structure ConvergenceTraceRow where
  name           : String
  instanceName   : String
  initialWorld   : World
  actions        : List Action
  finalWorld     : World
  /-- "substantive" (D1 closes with real witness today) or "deviation" (D1
      vacuous; Rust should be in the documented deviation state). -/
  status         : String

/-- Worked convergence trace for the watcher: persist + rescanTick + handle
    drives a doc from persistent to handled. Substantive. -/
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

/-- EventSource trace today: persist event observed, subscription drops it,
    no live rescan. Rust consumer asserts the runtime is in this deviation
    state. -/
def eventSourceTrace : ConvergenceTraceRow :=
  { name := "event_source_drop_then_no_resync"
  , instanceName := "EventSource"
  , initialWorld := World.empty
  , actions :=
      [ .persist (doc "doc-1")
      , .enqueue (doc "doc-1")
      , .drop (doc "doc-1") ]
  , finalWorld := mkWorld [doc "doc-1"] [] [] []
  , status := "deviation"
  }

/-- SubagentSource trace today: orphan child persists, dropped event,
    no live rescan in this process. Rust consumer asserts deviation state. -/
def subagentSourceTrace : ConvergenceTraceRow :=
  { name := "subagent_orphan_no_live_rescan"
  , instanceName := "SubagentSource"
  , initialWorld := World.empty
  , actions :=
      [ .persist (doc "tool-call-1")
      , .enqueue (doc "tool-call-1")
      , .drop (doc "tool-call-1") ]
  , finalWorld := mkWorld [doc "tool-call-1"] [] [] []
  , status := "deviation"
  }

def convergenceTraces : List ConvergenceTraceRow :=
  [ watcherTrace, eventSourceTrace, subagentSourceTrace ]

def convergenceTraceCount : Nat := convergenceTraces.length

/-! ## JSON serializers (local; mirrored after `Conformance.TriggerContracts`) -/

def jsonString (s : String) : String := "\"" ++ s ++ "\""

def jsonArray (vs : List String) : String := "[" ++ String.intercalate "," vs ++ "]"

def jsonOptionString : Option String → String
  | none => "null"
  | some s => jsonString s

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
