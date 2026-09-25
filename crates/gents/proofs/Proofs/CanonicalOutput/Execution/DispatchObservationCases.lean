import Proofs.CanonicalOutput.Execution.DispatchObservation
import Proofs.CanonicalOutput.Execution.Examples

namespace CanonicalOutput.Execution.DispatchObservation.Cases

open Examples Gate

def accepted : Option World := do
  let held ← acquire (initial (world 5)) 1 true
  let published ← commit held 1 5
    (.accept 7 providerTurn providerMessage [] [foregroundAdmission])
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

def parentOutcome : RequestExecutionLease.Outcome := .failed

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

example : afterParentFailure lostThenReplay = some (true, false, true, 1) := by native_decide

example : run lostThenReplay = some
    [⟨.unacknowledged, false, true, true⟩, ⟨.replay, false, true, true⟩] := by native_decide

example : run wonThenReplay = some
    [⟨.fresh, true, true, true⟩, ⟨.replay, false, true, true⟩] := by native_decide

example : run rejectedThenWon = some
    [⟨.rejected, false, false, false⟩, ⟨.fresh, true, true, true⟩] := by native_decide

end CanonicalOutput.Execution.DispatchObservation.Cases
