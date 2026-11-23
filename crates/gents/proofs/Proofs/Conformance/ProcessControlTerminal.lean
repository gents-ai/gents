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

structure UnownedStopInput where
  completionFirst : Bool
  deriving ToJson

structure UnownedStopOutput where
  cancelReply : Option ManagedExec.CancelReply
  alreadyTerminalState : Option String
  terminalState : ToolExecution.ToolCallState
  deriving ToJson

/-- A stop that loaded the row running and then found no live execution:
the owner's completion committed and released between the two
(`completionFirst`), or nothing ended the row. -/
@[conformance_family]
def unownedStop : Conformance.Family UnownedStopInput UnownedStopOutput where
  inputs := [{ completionFirst := false }, { completionFirst := true }]
  model := fun i =>
    let durable : ToolExecution.ToolCallState := if i.completionFirst then .completed else .running
    let won := durable == .running
    let terminal := if won then stopSettlement .notOwned else durable
    match unownedStopReply .notOwned won durable with
    | .reply reply =>
        { cancelReply := some reply, alreadyTerminalState := none, terminalState := terminal }
    | .alreadyTerminal state =>
        { cancelReply := none, alreadyTerminalState := some state.toDefraDB
          terminalState := terminal }

end Conformance.ProcessControlTerminal
