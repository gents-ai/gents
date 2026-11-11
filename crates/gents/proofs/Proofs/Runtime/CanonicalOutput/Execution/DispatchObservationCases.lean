import Proofs.CanonicalOutput.Execution.DispatchObservation
import Proofs.CanonicalOutput.Execution.Examples
import Proofs.CanonicalOutput.Execution.GateCases
import Proofs.Recovery.Sweeps.ToolCalls

namespace CanonicalOutput.Execution.DispatchObservation.Cases

open Examples Gate

def accepted : Option World := do
  let held ← acquire (initial (world 5)) 1 true
  let published ← commit held 1 5
    (.accept 7 providerTurn providerMessage [foregroundAdmission])
  scheduling published 1 .release

structure Input where
  acknowledged : Bool
  policyAllows : Bool := true
  deriving DecidableEq, Repr

structure Result where
  observation : Observation
  mayInvoke : Bool
  running : Bool
  inFlight : Bool
  deriving DecidableEq, Repr

def runStep (world : World) (input : Input) : Option (World × Result) := do
  let held ← acquire world 1 true
  let (after, observation) := attempt held 1 5 7
    { permit with toolPolicyAllows := input.policyAllows } input.acknowledged
  let returned ← if after.gateSchedule.phase == .storage then
      scheduling after 1 .storageReturned else some after
  let released ← scheduling returned 1 .release
  pure (released, ⟨observation, DispatchObservation.mayInvoke observation,
    physicalRunning after 600, decide (600 ∈ after.transcript.inFlight)⟩)

def runWorld (inputs : List Input) : Option (World × List Result) := do
  let initial ← accepted
  inputs.foldlM (fun (state, results) input => do
    let (after, result) ← runStep state input
    pure (after, results ++ [result])) (initial, [])

def run (inputs : List Input) : Option (List Result) := (runWorld inputs).map Prod.snd

def lostThenReplay : List Input := [⟨false, true⟩, ⟨true, true⟩]
def wonThenReplay : List Input := [⟨true, true⟩, ⟨true, true⟩]
def rejectedThenWon : List Input := [⟨true, false⟩, ⟨true, true⟩]
def policyRejected : List Input := [⟨true, false⟩]

def parentOutcome : RequestExecutionLease.Outcome := .failed

def completionProbeOutcome : RequestExecutionLease.Outcome := .completed

def completionProbe (inputs : List Input) : Option Bool := do
  let (after, _) ← runWorld inputs
  let held ← acquire after 1 true
  pure (commit held 1 5 (.terminalize 7 completionProbeOutcome (.message 501))).isSome

def afterParentFailure (inputs : List Input) : Option (Bool × Bool × Bool × Nat) := do
  let (after, _) ← runWorld inputs
  let held ← acquire after 1 true
  let terminal ← commit held 1 5 (.terminalize 7 parentOutcome (.message 501))
  let tool ← ownedToolByDocument? terminal 600
  pure (physicalRunning terminal 600, decide (600 ∈ terminal.transcript.inFlight),
    tool.stuckSince.isSome, terminal.messages.length)

structure PolicySettlement where
  failed : Bool
  running : Bool
  started : Bool
  failureClass : Option ToolExecution.FailureClass
  completionAccepted : Bool
  deriving DecidableEq, Repr

def policySettlement : ToolExecution.ToolCallContext.Action :=
  .spawnFailed .policyDenied

def afterPolicySettlement (inputs : List Input) : Option PolicySettlement := do
  let (after, _) ← runWorld inputs
  let held ← acquire after 1 true
  let settled ← commit held 1 5 (.toolComplete 600 (.native policySettlement)
    Gate.Cases.toolOutputClose (Gate.Cases.foregroundResultMessage 1))
  let tool ← ownedToolByDocument? settled 600
  let released ← scheduling settled 1 .release
  let completionHeld ← acquire released 1 true
  pure ⟨tool.context.state == .failed, physicalRunning settled 600,
    tool.context.startedAt.isSome, tool.context.failureClass,
    (commit completionHeld 1 5 (.terminalize 7 completionProbeOutcome (.message 501))).isSome⟩

def spawnAccepted : Option World := do
  let held ← acquire (initial (world 5)) 1 true
  let published ← commit held 1 5 (.accept 7 Gate.Cases.spawnProviderTurn
    Gate.Cases.spawnProviderMessage [foregroundAdmission])
  scheduling published 1 .release

def spawnRunning : Option World := do
  let (running, result) ← runStep (← spawnAccepted) ⟨true, true⟩
  if result.mayInvoke then some running else none

structure SpawnedTargetRejection where
  failed : Bool
  started : Bool
  failureClass : Option ToolExecution.FailureClass
  spawnedAdmitted : Bool
  completionAccepted : Bool
  deriving DecidableEq, Repr

def spawnedTargetRejection : ToolExecution.ToolCallContext.Action :=
  .fail .policyDenied

def afterSpawnedTargetRejection : Option SpawnedTargetRejection := do
  let held ← acquire (← spawnRunning) 1 true
  let settled ← commit held 1 5 (.toolComplete 600 (.native spawnedTargetRejection)
    Gate.Cases.toolOutputClose (Gate.Cases.foregroundResultMessage 1))
  let tool ← ownedToolByDocument? settled 600
  let released ← scheduling settled 1 .release
  let probeHeld ← acquire released 1 true
  pure ⟨tool.context.state == .failed, tool.context.startedAt.isSome,
    tool.context.failureClass,
    settled.toolContexts.any (fun spawned => spawned.provenance == .spawnedBackground 600),
    (commit probeHeld 1 5 (.terminalize 7 completionProbeOutcome (.message 501))).isSome⟩

def spawnedAdmissionAccepted (rejected : Bool) : Option Bool := do
  let running ← spawnRunning
  let world ← if rejected then do
      let held ← acquire running 1 true
      let settled ← commit held 1 5 (.toolComplete 600 (.native spawnedTargetRejection)
        Gate.Cases.toolOutputClose (Gate.Cases.foregroundResultMessage 1))
      scheduling settled 1 .release
    else some running
  let held ← acquire world 1 true
  pure (commit held 1 5 (.admitSpawned 7 Gate.Cases.spawnedAdmission)).isSome

def spawnedPreDispatchSettlementAccepted : Option Bool := do
  let held ← acquire (← spawnRunning) 1 true
  pure (commit held 1 5 (.toolComplete 600 (.native policySettlement)
    Gate.Cases.toolOutputClose (Gate.Cases.foregroundResultMessage 1))).isSome

example : afterSpawnedTargetRejection =
    some ⟨true, true, some .policyDenied, false, true⟩ := by native_decide

example : spawnedAdmissionAccepted false = some true := by native_decide
example : spawnedAdmissionAccepted true = some false := by native_decide
example : spawnedPreDispatchSettlementAccepted = some false := by native_decide

example : run policyRejected = some [⟨.rejected, false, false, false⟩] := by native_decide

example : afterPolicySettlement policyRejected =
    some ⟨true, false, false, some .policyDenied, true⟩ := by native_decide

example : afterPolicySettlement wonThenReplay = none := by native_decide

example : afterPolicySettlement lostThenReplay = none := by native_decide

structure ParentFailureRecovery where
  state : ToolExecution.ToolCallState
  dispatchable : Bool
  terminalized : Nat
  deriving DecidableEq, Repr

def terminalParentRow (tool : ToolExecution.ToolCallContext) : Recovery.TerminalParentToolRow :=
  { call := tool, parentTerminal := true, parentInterrupted := false }

def afterParentFailureRecovery (inputs : List Input) : Option (Option ParentFailureRecovery) := do
  let (after, _) ← runWorld inputs
  let held ← acquire after 1 true
  let terminal ← commit held 1 5 (.terminalize 7 parentOutcome (.message 501))
  let tool ← ownedToolByDocument? terminal 600
  let row := terminalParentRow tool.context
  if Recovery.terminalParentToolStale row then
    let recovered := Recovery.terminalParentToolRecover row
    pure (some ⟨recovered.call.state,
      (ToolExecution.ToolCallContext.step? recovered.call .dispatch).isSome,
      (terminal.toolContexts.filter (fun owned =>
        Recovery.terminalParentToolStale (terminalParentRow owned.context))).length⟩)
  else pure none

example : afterParentFailureRecovery lostThenReplay = some (some ⟨.failed, false, 1⟩) := by
  native_decide

example : afterParentFailureRecovery wonThenReplay = some (some ⟨.failed, false, 1⟩) := by
  native_decide

example : afterParentFailureRecovery policyRejected = some none := by native_decide

example : afterParentFailure lostThenReplay = some (true, false, true, 1) := by native_decide

example : completionProbe lostThenReplay = some false := by native_decide
example : completionProbe wonThenReplay = some false := by native_decide
example : completionProbe rejectedThenWon = some false := by native_decide
example : completionProbe [] = some true := by native_decide

example : (runWorld lostThenReplay).map (fun (world, _) =>
    (terminalSelectionValid world (.message 501),
     terminalReplayPresent world 7 .completed (.message 501),
     world.terminalSelection.isNone,
     normalCompletionToolsReady world 7)) = some (true, false, true, false) := by
  native_decide

example : run lostThenReplay = some
    [⟨.unacknowledged, false, true, true⟩, ⟨.replay, false, true, true⟩] := by native_decide

example : run wonThenReplay = some
    [⟨.fresh, true, true, true⟩, ⟨.replay, false, true, true⟩] := by native_decide

example : run rejectedThenWon = some
    [⟨.rejected, false, false, false⟩, ⟨.fresh, true, true, true⟩] := by native_decide

end CanonicalOutput.Execution.DispatchObservation.Cases
