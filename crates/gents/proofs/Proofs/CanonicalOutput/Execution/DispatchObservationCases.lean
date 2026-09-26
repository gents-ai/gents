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

/-- Inputs are executed against the same world left by the preceding call,
including after lost receipts. Gate release observes storage completion even
when the attempted commit was rejected. -/
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

/-- Probe the actual terminal owner before handoff. A committed foreground
dispatch remains a completion obligation even when its receipt was lost;
the caller's volatile handle map is not evidence that the obligation vanished. -/
def completionProbe (inputs : List Input) : Option Bool := do
  let (after, _) ← runWorld inputs
  let held ← acquire after 1 true
  pure (commit held 1 5 (.terminalize 7 completionProbeOutcome (.message 501))).isSome

/-- Request failure hands the unacknowledged running call to recovery. It does
not invent tool completion, remove committed intent, or retain the parent's
foreground claim. No fact here asserts that an external process stopped. -/
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

/-- A policy rejection of a direct call leaves the exact accepted call Pending,
so the rejected call must still be settled with a result. Settlement reuses the
existing pre-dispatch failure of that lifecycle: close and result delivery
commit together, the call never records a start, and the request may then
complete. A call that already won Running cannot be settled this way; a
`spawn_process` whose target is denied is such a call (`afterSpawnedTargetRejection`). -/
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

/-- The accepted `spawn_process` call after it won its own dispatch election. -/
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

/-- `spawn_process` evaluates its target only after it won its own dispatch,
so a target denied by its command policy is a Running failure of the parent,
not a pre-dispatch settlement. The parent fails with `policyDenied` and its
result is delivered before any spawned lifecycle is admitted; once the parent
is terminal, no spawned lifecycle can be admitted for it. -/
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

/-- Whether a spawned lifecycle may still be admitted for the parent: before
the target rejection it may, after it it may not. -/
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

/-- The Running parent cannot take the pre-dispatch settlement of a direct call. -/
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

/-- The sweep's view of an owned tool once its parent failed. -/
def terminalParentRow (tool : ToolExecution.ToolCallContext) : Recovery.TerminalParentToolRow :=
  { call := tool, parentTerminal := true, parentInterrupted := false }

/-- Eventual settlement of a call handed off by parent failure. The existing
terminal-parent sweep owns it, projected from the exact handed-off context.
Its premise is that a foreground executor lives only inside the owning loop,
which has ended once the parent is terminal; it does not observe that any
external effect ran or stopped. The settled context is terminal, so neither a
lost receipt nor a replay can reopen it for dispatch. `none` means the sweep
has nothing to settle. -/
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
