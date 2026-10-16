import Proofs.Recovery.Sweeps.ToolCalls

namespace Recovery

open ToolExecution

/-! A session-message row settles on its caused request's terminal, or fails
closed when it cannot name that request (`ToolRecoveryCause.causedRequestUnbound`).
Accepted premise: a peer that never replicates its caused request's terminal
back leaves the row running; only a kill ends it (`killAction`). -/

def sessionMessageRecoverySweep : RecoverySweep :=
  toolCallRecoverySweepFor true
    "tool_call_lifecycle_recover_session_message_rows"
    "background_completion::settle_running_session_message_rows" .periodic

/-- What a kill (`cancel_process` or the operator kill) observes about the
    request a running session-message row caused. -/
inductive KillObservation where
  | causedTerminal
  | causedLiveLocal
  | causedLiveRemote
  | unresolved
  deriving DecidableEq, Repr

inductive KillAction where
  | settle
  | interruptCaused
  | interruptAndCancelRow
  | cancelRow
  deriving DecidableEq, Repr

def KillObservation.toContract : KillObservation → String
  | .causedTerminal => "causedTerminal"
  | .causedLiveLocal => "causedLiveLocal"
  | .causedLiveRemote => "causedLiveRemote"
  | .unresolved => "unresolved"

def KillAction.toContract : KillAction → String
  | .settle => "settle"
  | .interruptCaused => "interruptCaused"
  | .interruptAndCancelRow => "interruptAndCancelRow"
  | .cancelRow => "cancelRow"

def killAction : KillObservation → KillAction
  | .causedTerminal => .settle
  | .causedLiveLocal => .interruptCaused
  | .causedLiveRemote => .interruptAndCancelRow
  | .unresolved => .cancelRow

def KillAction.endsRowNow : KillAction → Bool
  | .interruptCaused => false
  | _ => true

/-- A kill never waits on a peer or on a request it cannot name. -/
theorem kill_waits_only_on_a_local_terminal (obs : KillObservation) :
    (killAction obs).endsRowNow = false ↔ obs = .causedLiveLocal := by
  cases obs <;> simp [killAction, KillAction.endsRowNow]

def killCases : List (KillObservation × KillAction) :=
  [.causedTerminal, .causedLiveLocal, .causedLiveRemote, .unresolved].map
    fun obs => (obs, killAction obs)

end Recovery
