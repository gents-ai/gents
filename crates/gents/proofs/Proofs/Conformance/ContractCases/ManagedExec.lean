import Proofs.ManagedExec
import Proofs.Conformance.ContractCases.Types

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
  , { name := "deadline_timeout_preserves_partial_stdout_metadata"
    , trigger := "deadlineElapsed"
    , preExecState := ManagedExecState.toDefraDB .running
    , preToolState := ToolExecution.ToolCallState.toDefraDB .running
    , expectedExecState := ManagedExecState.toDefraDB .killSignaled
    , expectedToolState := ToolExecution.ToolCallState.toDefraDB .timedOut
    , maxSteps := ManagedExec.maxTimeoutSteps
    , killSignalRequired := true
    }
  ]

/-- Every (before, after) observation pair, with its verdict computed by the
    ownership model rather than written by hand. -/
def processStopCases : List ProcessStopCase :=
  ManagedExec.ProcessObservation.all.flatMap fun before =>
    ManagedExec.ProcessObservation.all.map fun after =>
      let outcome := ManagedExec.stopOutcome before after
      { name := "process_" ++ before.toContract ++ "_then_" ++ after.toContract
      , before := before.toContract
      , after := after.toContract
      , mayTerminate := ManagedExec.mayTerminate before
      , outcome := outcome.toContract
      , cancelReply := (ManagedExec.cancelReply outcome).toContract
      }

theorem processStopCases_cancelled_only_after_observed_stop :
    ∀ witness ∈ processStopCases,
      witness.cancelReply = "cancelled" →
        witness.before = "running" ∧ witness.after = "exited" := by
  native_decide

end Conformance.ContractCases
