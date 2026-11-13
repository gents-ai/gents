import Proofs.ToolCalls.Background.ProcessControl
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

end Conformance.ProcessControlTerminal
