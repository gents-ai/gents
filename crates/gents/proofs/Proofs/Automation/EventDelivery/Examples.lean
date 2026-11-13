import Proofs.Automation.EventDelivery.Executable
import Lean

namespace EventDelivery

structure StepInput where
  name : String
  pre : World
  action : Action

private def doc (s : String) : DocId := ⟨s⟩
private def w0 : World := World.empty
private def mkWorld (ps sq proc h : List DocId) : World := ⟨ps, sq, proc, h⟩

def transitionInputs : List StepInput :=
  [
    { name   := "persist_into_empty"
    , pre    := w0
    , action := .persist (doc "a")
    }
  ,
    { name   := "persist_extends_set"
    , pre    := mkWorld [doc "a"] [] [] []
    , action := .persist (doc "b")
    }
  ,
    { name   := "depersist_removes"
    , pre    := mkWorld [doc "a", doc "b"] [] [] []
    , action := .depersist (doc "a")
    }
  ,
    { name   := "enqueue_from_persistent"
    , pre    := mkWorld [doc "a"] [] [] []
    , action := .enqueue (doc "a")
    }
  ,
    { name   := "drop_from_queue"
    , pre    := mkWorld [doc "a"] [doc "a"] [] []
    , action := .drop (doc "a")
    }
  ,
    { name   := "deliver_consumes_queue"
    , pre    := mkWorld [doc "a"] [doc "a"] [] []
    , action := .deliverFromQueue (doc "a")
    }
  ,
    { name   := "rescan_on_empty"
    , pre    := w0
    , action := .rescanTick
    }
  ,
    { name   := "rescan_fills_queue"
    , pre    := mkWorld [doc "a"] [] [] []
    , action := .rescanTick
    }
  ,
    { name   := "rescan_skips_processed"
    , pre    := mkWorld [doc "a", doc "b"] [] [doc "a"] []
    , action := .rescanTick
    }
  ,
    { name   := "handle_legal_drains_queue"
    , pre    := mkWorld [doc "a"] [doc "a"] [] []
    , action := .handle (doc "a")
    }
  ,
    { name   := "handle_marks_processed"
    , pre    := mkWorld [doc "a", doc "b"] [doc "a", doc "b"] [] []
    , action := .handle (doc "a")
    }
  ,
    { name   := "enqueue_twice_multiset"
    , pre    := mkWorld [doc "a"] [doc "a"] [] []
    , action := .enqueue (doc "a")
    }
  ,
    { name   := "rescan_prepends_to_queue"
    , pre    := mkWorld [doc "a"] [doc "z"] [] []
    , action := .rescanTick
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
    }
  ,


    { name   := "release_returns_overtaken_doc"
    , pre    := mkWorld [doc "goal-cont-3"] [] [doc "goal-cont-3"] [doc "goal-cont-3"]
    , action := .release (doc "goal-cont-3")
    }
  ,
    { name   := "release_keeps_other_marks"
    , pre    := mkWorld [doc "a", doc "b"] [] [doc "a", doc "b"] [doc "b", doc "a"]
    , action := .release (doc "a")
    }
  ]


theorem transition_inputs_enabled :
    transitionInputs.all (fun c => (step? c.pre c.action).isSome) = true := by decide

end EventDelivery
