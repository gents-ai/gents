import Proofs.Runtime.CanonicalOutput.Execution.DispatchObservationCases
import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.RequestExecutionLease
import Proofs.Conformance.Family

namespace Conformance.DispatchObservationContracts

open Lean
open CanonicalOutput.Execution.DispatchObservation
open CanonicalOutput.Execution.DispatchObservation.Cases

def observationName : Observation → String
  | .rejected => "rejected"
  | .unacknowledged => "unacknowledged"
  | .replay => "replay"
  | .fresh => "fresh"

def cases : List (String × List Input) :=
  [("lost_receipt_then_replay", lostThenReplay),
   ("won_dispatch_then_replay", wonThenReplay),
   ("rejected_then_won_dispatch", rejectedThenWon),
   ("policy_rejected_then_settled", policyRejected)]

structure StepWire where
  acknowledged : Bool
  policyAllows : Bool
  deriving ToJson

def StepWire.ofModel (input : Input) : StepWire := ⟨input.acknowledged, input.policyAllows⟩

def StepWire.toModel (step : StepWire) : Input := ⟨step.acknowledged, step.policyAllows⟩

structure ResultWire where
  observation : String
  mayInvoke : Bool
  running : Bool
  inFlight : Bool
  deriving ToJson

def ResultWire.ofModel (result : Result) : ResultWire :=
  ⟨observationName result.observation, result.mayInvoke, result.running, result.inFlight⟩

structure TraceInput where
  name : String
  inputs : List StepWire
  deriving ToJson

def TraceInput.ofCase (entry : String × List Input) : TraceInput :=
  ⟨entry.1, entry.2.map StepWire.ofModel⟩

def TraceInput.steps (input : TraceInput) : List Input := input.inputs.map StepWire.toModel

/-- A loop invokes the tool exactly when the gate's attempt authorizes invocation. -/
def invocations (result : Result) : Nat := if result.mayInvoke then 1 else 0

/-- A call that reached the dispatch election without being authorized (a lost receipt or a
replay) stops the loop; a gate rejection is delivered as a tool result instead. -/
def loopFails (result : Result) : Bool := result.observation != .rejected && !result.mayInvoke

/-- A call the gate rejects never reaches the dispatch election. -/
def elections (result : Result) : Nat := if result.observation == .rejected then 0 else 1

/-- A call the gate rejects is reported as an admission rejection. -/
def rejections (result : Result) : Nat := if result.observation == .rejected then 1 else 0

structure HookReceiptInput where
  name : String
  inputs : List StepWire
  completionProbeOutcome : String
  parentOutcome : String
  deriving ToJson

structure ParentFailureWire where
  running : Bool
  needsRecovery : Bool
  messageCount : Nat
  deriving ToJson

structure ParentRecoveryWire where
  state : String
  dispatchable : Bool
  terminalized : Nat
  deriving ToJson

structure HookReceiptOutput where
  expected : List ResultWire
  completionProbeAccepted : Bool
  afterParentFailure : ParentFailureWire
  afterParentRecovery : Option ParentRecoveryWire
  deriving ToJson

/-- The call hook's election is reached only by a call that passed tool admission, so its
scenario replays the scripts whose every attempt the tool policy allows; a policy-denied attempt
is settled by the loop before the hook (`ownedPolicySettlement`, `loopElection`). -/
def hookReceiptCases : List (String × List Input) :=
  cases.filter fun entry => entry.2.all (·.policyAllows)

@[conformance_family]
def hookReceipt : Family HookReceiptInput (Option HookReceiptOutput) where
  inputs := hookReceiptCases.map fun entry =>
    { name := entry.1, inputs := entry.2.map StepWire.ofModel
      completionProbeOutcome :=
        Conformance.RequestExecutionLeaseContracts.outcomeName completionProbeOutcome
      parentOutcome := Conformance.RequestExecutionLeaseContracts.outcomeName parentOutcome }
  model := fun i => do
    let steps := i.inputs.map StepWire.toModel
    let results ← run steps
    let accepted ← completionProbe steps
    let (running, _, needsRecovery, messages) ← afterParentFailure steps
    let recovery ← afterParentFailureRecovery steps
    pure { expected := results.map ResultWire.ofModel
           completionProbeAccepted := accepted
           afterParentFailure := ⟨running, needsRecovery, messages⟩
           afterParentRecovery := recovery.map fun r =>
             ⟨r.state.toDefraDB, r.dispatchable, r.terminalized⟩ }

structure OwnedReceiptInput where
  name : String
  acknowledged : Bool
  policyAllows : Bool
  deriving ToJson

structure OwnedReceiptOutput where
  invocations : Nat
  loopFailed : Bool
  deliveredResult : Bool
  stoppedWithRunningCall : Bool
  deriving ToJson

/-- The owned loop elects one provider tool call once, so its scenario builds only the first
attempt of a script, and only an attempt the tool policy allows: a policy-denied attempt is
settled before the election (`ownedPolicySettlement`). -/
def ownedReceiptCases : List OwnedReceiptInput :=
  cases.filterMap fun entry => match entry.2 with
    | first :: _ =>
        if first.policyAllows then
          some ⟨entry.1, first.acknowledged, first.policyAllows⟩
        else none
    | [] => none

@[conformance_family]
def ownedReceipt : Family OwnedReceiptInput (Option OwnedReceiptOutput) where
  inputs := ownedReceiptCases
  model := fun i => ((run [⟨i.acknowledged, i.policyAllows⟩]).bind (·.head?)).map fun r =>
    { invocations := invocations r, loopFailed := loopFails r, deliveredResult := r.mayInvoke
      stoppedWithRunningCall := loopFails r && r.running }

structure PolicySettlementWire where
  failed : Bool
  running : Bool
  started : Bool
  failureClass : Option String
  completionAccepted : Bool
  deriving ToJson

structure PolicySettlementOutput where
  electionCommitted : Bool
  afterPolicySettlement : PolicySettlementWire
  deriving ToJson

/-- The owned loop settles a policy-denied call by pre-dispatch failure, so its settlement
scenario replays the scripts whose every attempt is acknowledged and denied by the tool policy. -/
def policySettlementCases : List (String × List Input) :=
  cases.filter fun entry => entry.2.all fun input => input.acknowledged && !input.policyAllows

@[conformance_family]
def ownedPolicySettlement : Family TraceInput (Option PolicySettlementOutput) where
  inputs := policySettlementCases.map TraceInput.ofCase
  model := fun i => do
    let results ← run i.steps
    let settlement ← afterPolicySettlement i.steps
    pure { electionCommitted := results.any (·.running)
           afterPolicySettlement :=
             { failed := settlement.failed, running := settlement.running
               started := settlement.started
               failureClass := settlement.failureClass.map (·.toDefraDB)
               completionAccepted := settlement.completionAccepted } }

structure LoopElectionInput where
  name : String
  step : Nat
  acknowledged : Bool
  policyAllows : Bool
  /-- The recording hook's dispatch election, computed by the model: the hook elects the call
  exactly when the gate's attempt authorizes invocation (`DispatchObservation.mayInvoke`). The
  loop drives a recording hook, so the durable election is replayed by `hookReceipt`; this
  family observes what the loop does with the election. -/
  hookElects : Bool
  deriving ToJson

structure LoopElectionOutput where
  invocations : Nat
  loopFailed : Bool
  elections : Nat
  rejections : Nat
  deriving ToJson

/-- The loop runs each attempt of every script on a fresh loop with a recording hook. -/
def loopElectionInputs : List LoopElectionInput :=
  cases.flatMap fun entry =>
    let results := (run entry.2).getD []
    ((List.range entry.2.length).zip (entry.2.zip results)).map fun (step, input, result) =>
      { name := entry.1, step, acknowledged := input.acknowledged
        policyAllows := input.policyAllows, hookElects := result.mayInvoke }

@[conformance_family]
def loopElection : Family LoopElectionInput (Option LoopElectionOutput) where
  inputs := loopElectionInputs
  model := fun i => (((cases.lookup i.name).bind run).bind (·[i.step]?)).map fun r =>
    { invocations := invocations r, loopFailed := loopFails r, elections := elections r
      rejections := rejections r }

structure ControlReplayOutput where
  electedRunning : Bool
  replayMayInvoke : Bool
  deriving ToJson

/-- A control tool commits its effect inside the call hook's election, so a completion fault
after the effect is reached only by a script whose first acknowledged election is won and then
replayed. -/
def controlReplayCases : List (String × List Input) :=
  cases.filter fun entry => entry.1 ∈ ["won_dispatch_then_replay"]

@[conformance_family]
def controlReplay : Family TraceInput (Option ControlReplayOutput) where
  inputs := controlReplayCases.map TraceInput.ofCase
  model := fun i => (run i.steps).bind fun
    | [first, second] => some ⟨first.running, second.mayInvoke⟩
    | _ => none

structure SpawnedTargetInput where
  parentTool : String
  completionProbeOutcome : String
  deriving Lean.ToJson

structure SpawnedTargetOutput where
  failed : Bool
  started : Bool
  failureClass : Option String
  spawnedAdmitted : Bool
  completionAccepted : Bool
  deriving Lean.ToJson

/-- The parent `spawn_process` call is denied by its spawned target's policy
before the spawned call is admitted; the request is then completed. -/
@[conformance_family]
def spawnedTarget : Family SpawnedTargetInput (Option SpawnedTargetOutput) where
  inputs := [⟨"spawn_process",
    Conformance.RequestExecutionLeaseContracts.outcomeName completionProbeOutcome⟩]
  model := fun _ => afterSpawnedTargetRejection.map fun rejection =>
    { failed := rejection.failed, started := rejection.started
      failureClass := rejection.failureClass.map (·.toDefraDB)
      spawnedAdmitted := rejection.spawnedAdmitted
      completionAccepted := rejection.completionAccepted }

example : afterSpawnedTargetRejection.isSome = true := by native_decide

example : cases.all (fun entry => (run entry.2).isSome) = true := by native_decide

example : cases.all (fun entry => (completionProbe entry.2).isSome) = true := by native_decide

example : cases.all (fun entry => ((run entry.2).getD []).length = entry.2.length) = true := by
  native_decide

example : cases.all (fun entry => (((run entry.2).getD []).zip entry.2).all fun (result, input) =>
    input.policyAllows || (!result.mayInvoke && !result.running)) = true := by native_decide

end Conformance.DispatchObservationContracts
