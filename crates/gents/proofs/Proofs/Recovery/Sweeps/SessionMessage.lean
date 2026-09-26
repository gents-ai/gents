import Proofs.Recovery.Sweeps.ToolCalls

namespace Recovery

open ToolExecution

/-- A `create_session`/`send_message` row ends only on its own deadline or the
    terminal of the request it caused: no parent terminal is a cause, because
    the started session is an ordinary agent's session, not a subordinate. -/
inductive SessionMessageRecoveryCause where
  | deadlineExceeded
  | requestCompleted
  | requestFailed
  | requestDead
  | requestInterrupted
  | requestSuperseded
  deriving DecidableEq, Repr

namespace SessionMessageRecoveryCause

def toContract : SessionMessageRecoveryCause → String
  | .deadlineExceeded => "deadlineExceeded"
  | .requestCompleted => "requestCompleted"
  | .requestFailed => "requestFailed"
  | .requestDead => "requestDead"
  | .requestInterrupted => "requestInterrupted"
  | .requestSuperseded => "requestSuperseded"

def terminalState : SessionMessageRecoveryCause → ToolCallState
  | .deadlineExceeded => .timedOut
  | .requestCompleted => .completed
  | .requestFailed => .failed
  | .requestDead => .failed
  | .requestInterrupted => .cancelled
  | .requestSuperseded => .failed

theorem terminalState_terminal (cause : SessionMessageRecoveryCause) :
    isTerminal cause.terminalState := by
  cases cause <;>
    simp [terminalState, HasTerminal.isTerminal, ToolCallState.instHasTerminal]

end SessionMessageRecoveryCause

structure SessionMessageRecoveryRow where
  call : ToolCallContext
  cause : SessionMessageRecoveryCause
  deriving Repr

def sessionMessageRecoveryStale (row : SessionMessageRecoveryRow) : Prop :=
  row.call.state = .running ∧ isSessionMessageCall row.call

instance (row : SessionMessageRecoveryRow) : Decidable (sessionMessageRecoveryStale row) := by
  unfold sessionMessageRecoveryStale
  infer_instance

def sessionMessageRecover (row : SessionMessageRecoveryRow) : SessionMessageRecoveryRow :=
  { row with call := { row.call with state := row.cause.terminalState } }

def sessionMessageRecoveryMeasure (row : SessionMessageRecoveryRow) : Nat :=
  if sessionMessageRecoveryStale row then 1 else 0

theorem sessionMessageRecovery_stale_positive :
    ∀ row, sessionMessageRecoveryStale row → sessionMessageRecoveryMeasure row > 0 := by
  intro row h_stale
  simp [sessionMessageRecoveryMeasure, h_stale]

theorem sessionMessageRecover_terminal :
    ∀ row, sessionMessageRecoveryStale row → isTerminal (sessionMessageRecover row).call.state := by
  intro row _h_stale
  rcases row with ⟨call, cause⟩
  cases cause <;>
    simp [sessionMessageRecover, SessionMessageRecoveryCause.terminalState,
      HasTerminal.isTerminal, ToolCallState.instHasTerminal]

theorem sessionMessageRecover_zero :
    ∀ row, sessionMessageRecoveryStale row → sessionMessageRecoveryMeasure (sessionMessageRecover row) = 0 := by
  intro row _h_stale
  have h_terminal_not_running : row.cause.terminalState ≠ .running := by
    cases row.cause <;> simp [SessionMessageRecoveryCause.terminalState]
  have h_not : ¬ sessionMessageRecoveryStale (sessionMessageRecover row) := by
    intro h_stale
    rcases h_stale with ⟨h_running, _h_session⟩
    simp [sessionMessageRecover] at h_running
    exact h_terminal_not_running h_running
  simp [sessionMessageRecoveryMeasure, h_not]

def sessionMessageRecoverySweep : RecoverySweep :=
  { Row := SessionMessageRecoveryRow
  , collection := .agentToolCall
  , sweepId := "tool_call_lifecycle_recover_session_message_rows"
  , rustFunction := "ToolCallLifecycle::recover_all"
  , cadence := .startup
  , implementationStatus := .implemented
  , stale := sessionMessageRecoveryStale
  , recover := sessionMessageRecover
  , terminal := fun row => isTerminal row.call.state
  , measure := sessionMessageRecoveryMeasure
  , h_stale_positive := sessionMessageRecovery_stale_positive
  , h_recover_terminal := sessionMessageRecover_terminal
  , h_recover_zero := sessionMessageRecover_zero
  }

end Recovery
