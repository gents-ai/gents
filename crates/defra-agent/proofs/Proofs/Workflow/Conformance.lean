import Proofs.Workflow.FanOut

/-!
# Workflow conformance witnesses

Finite witness rows for the Rust barrier-projection fence.
-/

namespace Workflow
namespace Conformance

open ToolExecution

structure BarrierCase where
  name : String
  groupTerminalStates : List ToolCallState
  synthesisPresent : Bool
  legal : Bool

def workflowCases : List BarrierCase :=
  [ { name := "all_terminal_then_synthesis"
    , groupTerminalStates := [.completed, .completed, .cancelled]
    , synthesisPresent := true
    , legal := true
    }
  , { name := "failed_sibling_then_synthesis"
    , groupTerminalStates := [.completed, .failed, .completed]
    , synthesisPresent := true
    , legal := true
    }
  , { name := "pending_sibling_then_synthesis"
    , groupTerminalStates := [.completed, .running, .completed]
    , synthesisPresent := true
    , legal := false
    }
  , { name := "empty_group"
    , groupTerminalStates := []
    , synthesisPresent := true
    , legal := false
    }
  ]

end Conformance
end Workflow
