import Proofs.Session.ToolExecution.Executable

/-! A native filesystem traversal (`list_files`, `glob`, `grep`) runs in the
fs-runner child under the managed-exec process boundary, so the request
deadline stops the call even while one poll inside the child is blocked, and
the single-worker tool queue does not wait for that poll. -/

namespace ToolExecution
namespace NativeFilesystemBoundary

/-- The running call observed when the runner's blocked poll would return
(`blockerMs`), or at once when no poll blocks. -/
def running (innerPollBlocks : Bool) (requestDeadlineMs blockerMs : Nat) : ToolCallContext :=
  { callId := 0, requestId := 0, state := .running, operation := .nativeCommand
  , deadline := requestDeadlineMs
  , currentTime := if innerPollBlocks then blockerMs else requestDeadlineMs
  , persistence := .committed }

/-- The call times out when the request deadline passes before the blocked
poll returns, and otherwise completes with the runner's answer. -/
def terminal (innerPollBlocks : Bool) (requestDeadlineMs blockerMs : Nat) : ToolCallState :=
  let call := running innerPollBlocks requestDeadlineMs blockerMs
  match call.step? .timeout with
  | some post => post.state
  | none => ((call.step? .complete).map (·.state)).getD call.state

/-- The queue advances at the call's terminal, which precedes the blocked
poll's return exactly when the deadline ended the call. -/
def queueAdvancesBeforeBlockerReturns (innerPollBlocks : Bool)
    (requestDeadlineMs blockerMs : Nat) : Bool :=
  terminal innerPollBlocks requestDeadlineMs blockerMs == .timedOut

end NativeFilesystemBoundary
end ToolExecution
