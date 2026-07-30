import Proofs.Conformance.ContractCases.Types
import Proofs.CrossMachineComposed.Reachability

namespace Conformance.ContractCases

open ComposedState
open ComposedState.ReachabilityWitness

private def c1BasePath : List String :=
  [ "process_step.startup_clean"
  , "request_step.claim"
  , "slot_acquire"
  , "request_step.begin_inference"
  , "tool_spawn"
  ]

def composedInvariantWitnesses : List ComposedInvariantWitness :=
  [ { theoremName :=
        "ComposedState.deadline_exceeded_request_cancels_pending_tools_from_initial"
    , witnessKind := "reachable_domain"
    , scenario := "pending_tool_deadline_exceeded_cancels_before_dispatch"
    , rustPath := "ToolCallLifecycle::cancel_before_dispatch"
    , traceStepCount := 6
    , transitionPath := c1BasePath ++ ["clock_advance"]
    , preRequestState := RequestState.toDefraDB withExpiredPendingTool.request.state
    , preRequestAdmission := admissionName withExpiredPendingTool.request.admission
    , toolPreState := ToolExecution.ToolCallState.toDefraDB expiredPendingTool.state
    , toolPostState := ToolExecution.ToolCallState.toDefraDB .cancelled
    , requestId := withExpiredPendingTool.requestId
    , toolRequestId := expiredPendingTool.requestId
    , toolCallId := expiredPendingTool.callId
    , requestDeadline := withExpiredPendingTool.request.deadline
    , requestCurrentTime := withExpiredPendingTool.request.currentTime
    , toolDeadline := expiredPendingTool.deadline
    , toolCurrentTime := expiredPendingTool.currentTime
    , deadlineExceeded := decide withExpiredPendingTool.request.deadlineExceeded
    , wellFormedSource := "ComposedState.wellFormed_from_initial"
    , preToolPersisted := false
    , cancelCause := some (ToolExecution.CancelCause.toDefraDB .deadline)
    }
  , { theoremName :=
        "ComposedState.deadline_exceeded_request_timesOut_running_tools_from_initial"
    , witnessKind := "reachable_domain"
    , scenario := "running_tool_deadline_exceeded_times_out_on_recovery"
    , rustPath := "ToolCallLifecycle::recover_all"
    , traceStepCount := 7
    , transitionPath := c1BasePath ++ ["tool_step.dispatch", "clock_advance"]
    , preRequestState := RequestState.toDefraDB withExpiredRunningTool.request.state
    , preRequestAdmission := admissionName withExpiredRunningTool.request.admission
    , toolPreState := ToolExecution.ToolCallState.toDefraDB expiredRunningTool.state
    , toolPostState := ToolExecution.ToolCallState.toDefraDB .timedOut
    , requestId := withExpiredRunningTool.requestId
    , toolRequestId := expiredRunningTool.requestId
    , toolCallId := expiredRunningTool.callId
    , requestDeadline := withExpiredRunningTool.request.deadline
    , requestCurrentTime := withExpiredRunningTool.request.currentTime
    , toolDeadline := expiredRunningTool.deadline
    , toolCurrentTime := expiredRunningTool.currentTime
    , deadlineExceeded := decide withExpiredRunningTool.request.deadlineExceeded
    , wellFormedSource := "ComposedState.wellFormed_from_initial"
    , preToolPersisted := true
    , cancelCause := some (ToolExecution.CancelCause.toDefraDB .deadline)
    }
  , { theoremName :=
        "ComposedState.interrupted_request_cancels_live_linked_tools_from_initial"
    , witnessKind := "reachable_domain"
    , scenario := "interrupted_request_cancels_live_pending_tool"
    , rustPath := "ToolCallLifecycle::cancel_before_dispatch"
    , traceStepCount := 7
    , transitionPath :=
        c1BasePath ++ ["request_interrupt", "request_step.interrupt_processing"]
    , preRequestState := RequestState.toDefraDB interruptedWithTool.request.state
    , preRequestAdmission := admissionName interruptedWithTool.request.admission
    , toolPreState := ToolExecution.ToolCallState.toDefraDB pendingTool.state
    , toolPostState := ToolExecution.ToolCallState.toDefraDB .cancelled
    , requestId := interruptedWithTool.requestId
    , toolRequestId := pendingTool.requestId
    , toolCallId := pendingTool.callId
    , requestDeadline := interruptedWithTool.request.deadline
    , requestCurrentTime := interruptedWithTool.request.currentTime
    , toolDeadline := pendingTool.deadline
    , toolCurrentTime := pendingTool.currentTime
    , deadlineExceeded := decide interruptedWithTool.request.deadlineExceeded
    , wellFormedSource := "ComposedState.wellFormed_from_initial"
    , preToolPersisted := false
    , cancelCause := some (ToolExecution.CancelCause.toDefraDB .interrupted)
    }
  , { theoremName :=
        "ComposedState.interrupted_request_cancels_live_linked_tools_from_initial"
    , witnessKind := "reachable_domain"
    , scenario := "interrupted_request_cancels_live_running_tool"
    , rustPath := "ToolCallLifecycle::cancel_during_run"
    , traceStepCount := 8
    , transitionPath :=
        c1BasePath ++
          ["tool_step.dispatch", "request_interrupt", "request_step.interrupt_processing"]
    , preRequestState := RequestState.toDefraDB interruptedWithRunningTool.request.state
    , preRequestAdmission := admissionName interruptedWithRunningTool.request.admission
    , toolPreState := ToolExecution.ToolCallState.toDefraDB runningTool.state
    , toolPostState := ToolExecution.ToolCallState.toDefraDB .cancelled
    , requestId := interruptedWithRunningTool.requestId
    , toolRequestId := runningTool.requestId
    , toolCallId := runningTool.callId
    , requestDeadline := interruptedWithRunningTool.request.deadline
    , requestCurrentTime := interruptedWithRunningTool.request.currentTime
    , toolDeadline := runningTool.deadline
    , toolCurrentTime := runningTool.currentTime
    , deadlineExceeded := decide interruptedWithRunningTool.request.deadlineExceeded
    , wellFormedSource := "ComposedState.wellFormed_from_initial"
    , preToolPersisted := true
    , cancelCause := some (ToolExecution.CancelCause.toDefraDB .interrupted)
    }
  ]

end Conformance.ContractCases
