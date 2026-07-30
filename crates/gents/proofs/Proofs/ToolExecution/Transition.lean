import Proofs.ToolExecution.CancelCause
import Proofs.ToolExecution.State

namespace ToolExecution
namespace ToolCallContext

inductive Transition : ToolCallContext → ToolCallContext → Prop where

  | dispatch {pre post : ToolCallContext}
      (h_state : pre.state = .pending)
      (h_post  : post = { pre with state := .running
                                 , startedAt := some pre.currentTime })
      : Transition pre post

  | spawnFailed {pre post : ToolCallContext} (failure : FailureClass)
      (h_state : pre.state = .pending)
      (h_post  : post = { pre with state := .failed
                                 , failureClass := some failure })
      : Transition pre post

  | complete {pre post : ToolCallContext}
      (h_state   : pre.state = .running)
      (h_persist : pre.persistence = .committed)
      (h_native  : pre.childRequestId = none)
      (h_post    : post = { pre with state := .completed })
      : Transition pre post

  | fail {pre post : ToolCallContext} (failure : FailureClass)
      (h_state : pre.state = .running)
      (h_post  : post = { pre with state := .failed
                                 , failureClass := some failure })
      : Transition pre post

  | timeout {pre post : ToolCallContext}
      (h_state    : pre.state = .running)
      (h_deadline : pre.deadlineExceeded)
      (h_post     : post = { pre with state := .timedOut })
      : Transition pre post

  | cancelBeforeDispatch {pre post : ToolCallContext} (cause : CancelCause)
      (h_state : pre.state = .pending)
      (h_post  : post = { pre with state := .cancelled })
      : Transition pre post

  | cancelDuringRun {pre post : ToolCallContext} (cause : CancelCause)
      (h_state : pre.state = .running)
      (h_post  : post = { pre with state := .cancelled })
      : Transition pre post

  | holdForApproval {pre post : ToolCallContext}
      (h_state : pre.state = .pending)
      (h_post  : post = { pre with state := .awaitingApproval })
      : Transition pre post

  | recordApproval {pre post : ToolCallContext} (decision : ApprovalDecision)
      (h_state : pre.state = .awaitingApproval)
      (h_none  : pre.approval = none)
      (h_post  : post = { pre with approval := some decision })
      : Transition pre post

  | approve {pre post : ToolCallContext}
      (h_state    : pre.state = .awaitingApproval)
      (h_evidence : pre.approval = some .approved)
      (h_post     : post = { pre with state := .running
                                    , startedAt := some pre.currentTime })
      : Transition pre post

  | deny {pre post : ToolCallContext}
      (h_state    : pre.state = .awaitingApproval)
      (h_evidence : pre.approval = some .denied)
      (h_post     : post = { pre with state := .failed
                                    , failureClass := some .approvalDenied })
      : Transition pre post

  | cancelWhileHeld {pre post : ToolCallContext} (cause : CancelCause)
      (h_state : pre.state = .awaitingApproval)
      (h_post  : post = { pre with state := .cancelled })
      : Transition pre post

  | timeoutWhileHeld {pre post : ToolCallContext}
      (h_state    : pre.state = .awaitingApproval)
      (h_deadline : pre.deadlineExceeded)
      (h_post     : post = { pre with state := .timedOut })
      : Transition pre post

  | background {pre post : ToolCallContext}
      (h_state : pre.state = .running)
      (h_mode  : pre.awaitMode = .foreground)
      (h_post  : post = { pre with awaitMode := .background })
      : Transition pre post

  | foreground {pre post : ToolCallContext}
      (h_state : pre.state = .running)
      (h_mode  : pre.awaitMode = .background)
      (h_post  : post = { pre with awaitMode := .foreground })
      : Transition pre post

  | detach {pre post : ToolCallContext}
      (h_live  : pre.state = .pending ∨ pre.state = .running)
      (h_pol   : pre.cancelPolicy = .cascade)
      (h_post  : post = { pre with cancelPolicy := .detach })
      : Transition pre post

  | timeAdvance {pre post : ToolCallContext} (t : Time)
      (h_le   : pre.currentTime ≤ t)
      (h_post : post = { pre with currentTime := t })
      : Transition pre post

  | persistenceStep {pre post : ToolCallContext}
      (policy : PersistenceState.FailurePolicy)
      (next : PersistenceState)
      (h_p_step : PersistenceState.Transition policy pre.persistence next)
      (h_post   : post = { pre with persistence := next })
      : Transition pre post

inductive Trace : ToolCallContext → ToolCallContext → Prop where
  | refl {c : ToolCallContext} : Trace c c
  | step {c₁ c₂ c₃ : ToolCallContext} :
      Transition c₁ c₂ → Trace c₂ c₃ → Trace c₁ c₃

end ToolCallContext
end ToolExecution
