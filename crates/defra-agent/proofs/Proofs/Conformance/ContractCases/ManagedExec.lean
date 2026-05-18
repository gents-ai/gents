import Proofs.ManagedExec
import Proofs.Conformance.ContractCases.Types

/-!
# Managed Exec Witness Cases

Finite rows for the native managed-exec process boundary. These rows connect
Lean liveness statements to Rust tests without persisting executor metadata on
`AgentToolCall`.
-/

namespace Conformance.ContractCases

def managedExecLivenessCases : List ManagedExecLivenessCase :=
  [ { name := "running_child_expired_deadline_kill_signaled"
    , trigger := "deadlineElapsed"
    , preExecState := ManagedExecState.toDefraDB .running
    , preToolState := ToolExecution.ToolCallState.toDefraDB .running
    , expectedExecState := ManagedExecState.toDefraDB .killSignaled
    , expectedToolState := ToolExecution.ToolCallState.toDefraDB .timedOut
    , maxSteps := ManagedExec.maxTimeoutSteps
    , killSignalRequired := true
    }
  , { name := "running_child_cancel_kill_signaled"
    , trigger := "cancelRequested"
    , preExecState := ManagedExecState.toDefraDB .running
    , preToolState := ToolExecution.ToolCallState.toDefraDB .running
    , expectedExecState := ManagedExecState.toDefraDB .killSignaled
    , expectedToolState := ToolExecution.ToolCallState.toDefraDB .cancelled
    , maxSteps := ManagedExec.maxTimeoutSteps
    , killSignalRequired := true
    }
  , { name := "fast_child_exit_completes_without_kill"
    , trigger := "observeExitSuccess"
    , preExecState := ManagedExecState.toDefraDB .running
    , preToolState := ToolExecution.ToolCallState.toDefraDB .running
    , expectedExecState := ManagedExecState.toDefraDB .exited
    , expectedToolState := ToolExecution.ToolCallState.toDefraDB .completed
    , maxSteps := 1
    , killSignalRequired := false
    }
  , { name := "nonzero_child_exit_fails_without_kill"
    , trigger := "observeExitFailure"
    , preExecState := ManagedExecState.toDefraDB .running
    , preToolState := ToolExecution.ToolCallState.toDefraDB .running
    , expectedExecState := ManagedExecState.toDefraDB .exited
    , expectedToolState := ToolExecution.ToolCallState.toDefraDB .failed
    , maxSteps := 1
    , killSignalRequired := false
    }
  , { name := "timeout_with_partial_stdout_preserves_metadata"
    , trigger := "deadlineElapsedPartialStdout"
    , preExecState := ManagedExecState.toDefraDB .running
    , preToolState := ToolExecution.ToolCallState.toDefraDB .running
    , expectedExecState := ManagedExecState.toDefraDB .killSignaled
    , expectedToolState := ToolExecution.ToolCallState.toDefraDB .timedOut
    , maxSteps := ManagedExec.maxTimeoutSteps
    , killSignalRequired := true
    }
  ]

end Conformance.ContractCases
