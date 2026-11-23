import Proofs.ToolCalls.Background.ProcessControl
import Proofs.Conformance.Functions.ToolCalls
import Proofs.Conformance.Family

namespace Conformance.ProcessControlTerminal

open Lean Background.ProcessControl

structure RaceInput where
  cancelFirst : Bool
  deriving ToJson

structure RaceOutput where
  stopCancelled : Bool
  liveExecutionCancelled : Option Bool
  alreadyTerminalState : Option String
  completionAccepted : Bool
  completionRefusedTerminal : Bool
  terminalState : String
  deriving ToJson

@[conformance_family]
def order : Conformance.Family RaceInput RaceOutput where
  inputs := [{ cancelFirst := false }, { cancelFirst := true }]
  model := fun i =>
    { stopCancelled := stopCancelled i.cancelFirst
      liveExecutionCancelled := liveExecutionCancelled i.cancelFirst
      alreadyTerminalState := alreadyTerminalState i.cancelFirst
      completionAccepted := completionAccepted (completionObserves i.cancelFirst)
      completionRefusedTerminal := refusedAsTerminal (completionObserves i.cancelFirst)
      terminalState := (raceRow i.cancelFirst).toDefraDB }

structure ReleaseWaitInput where
  reread : ToolExecution.ToolCallState
  deriving ToJson

structure ReleaseWaitOutput where
  cancelReply : Option ManagedExec.CancelReply
  alreadyTerminalState : Option String
  durableState : ToolExecution.ToolCallState
  deriving ToJson

/-- A stop whose wait for the worker's release ends unreleased, after the
worker committed its cancellation, a completion, or nothing yet. -/
@[conformance_family]
def releaseWaitEnded : Conformance.Family ReleaseWaitInput ReleaseWaitOutput where
  inputs := [{ reread := .running }, { reread := .cancelled }, { reread := .completed }]
  model := fun i =>
    match signalledReply (settleLiveCancel false .spawnedUnverified) i.reread with
    | .reply reply =>
        { cancelReply := some reply, alreadyTerminalState := none, durableState := i.reread }
    | .alreadyTerminal state =>
        { cancelReply := none, alreadyTerminalState := some state.toDefraDB
          durableState := i.reread }

end Conformance.ProcessControlTerminal
