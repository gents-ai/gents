import Proofs.Conformance.ContractCases.ManagedExec
import Proofs.Conformance.Functions.ToolCalls
import Proofs.Conformance.Family

namespace Conformance.ProcessStop

open Lean ManagedExec Background.ProcessControl Conformance.ContractCases

structure Case where
  name : String
  before : ProcessObservation
  after : ProcessObservation
  liveEvidence : Option LiveProcessEvidence
  workerReleased : Bool
  deriving ToJson

def observationOf (wire : String) : ProcessObservation :=
  (ProcessObservation.all.find? (·.toContract == wire)).getD .unowned

def evidenceOf : String → Option LiveProcessEvidence
  | "never_spawned" => some .neverSpawned
  | "spawned_unverified" => some .spawnedUnverified
  | "exited" => some .exited
  | _ => none

def caseOf (witness : ProcessStopCase) : Case :=
  { name := witness.name
  , before := observationOf witness.before
  , after := observationOf witness.after
  , liveEvidence := evidenceOf witness.liveEvidence
  , workerReleased := witness.workerReleased }

def allCases : List Case := processStopCases.map caseOf

example : processStopCases.all (fun witness =>
    (observationOf witness.before).toContract == witness.before &&
    (observationOf witness.after).toContract == witness.after &&
    ((evidenceOf witness.liveEvidence).isSome == !witness.liveEvidence.isEmpty)) = true := by
  native_decide

def named (names : List String) : List Case :=
  allCases.filter (·.name ∈ names)

def outcomeOf (c : Case) : StopOutcome :=
  match c.liveEvidence with
  | some evidence => settleLiveCancel c.workerReleased evidence
  | none => stopOutcome c.before c.after

def durableStateOf (c : Case) : ToolExecution.ToolCallState :=
  stopSettlement (outcomeOf c)

structure ObservationInput where
  name : String
  before : ProcessObservation
  after : ProcessObservation
  deriving ToJson

structure HostOutput where
  outcome : StopOutcome
  mayTerminate : Bool
  retainRecord : Bool
  cancelReply : CancelReply
  deriving ToJson

/-- The host owner's verdict on a stop from the observation before it and the
observation after it. -/
@[conformance_family]
def host : Family ObservationInput HostOutput where
  inputs := allCases.map fun c => { name := c.name, before := c.before, after := c.after }
  model := fun i =>
    { outcome := stopOutcome i.before i.after
    , mayTerminate := ManagedExec.mayTerminate i.before
    , retainRecord := retainProcessRecord i.after
    , cancelReply := cancelReply (stopOutcome i.before i.after) }

structure LiveInput where
  name : String
  liveEvidence : LiveProcessEvidence
  workerReleased : Bool
  deriving ToJson

structure LiveOutput where
  outcome : StopOutcome
  settleCompleted : Bool
  settleCancelled : Bool
  settleTimedOut : Bool
  settleFailed : Bool
  settlePanicked : Bool
  retireBeforeCommit : Bool
  retireAfterCommit : Bool
  deriving ToJson

/-- A cancelled live worker: what its evidence lets it settle and report. -/
@[conformance_family]
def liveSettlement : Family LiveInput LiveOutput where
  inputs := allCases.filterMap fun c =>
    c.liveEvidence.map fun evidence =>
      { name := c.name, liveEvidence := evidence, workerReleased := c.workerReleased }
  model := fun i =>
    { outcome := settleLiveCancel i.workerReleased i.liveEvidence
    , settleCompleted := maySettleTerminal .completed i.liveEvidence
    , settleCancelled := maySettleTerminal .cancelled i.liveEvidence
    , settleTimedOut := maySettleTerminal .timedOut i.liveEvidence
    , settleFailed := maySettleTerminal .failed i.liveEvidence
    , settlePanicked := maySettleTerminal .panicked i.liveEvidence
    , retireBeforeCommit := retireStopReceipt false
    , retireAfterCommit := retireStopReceipt true }

structure OwnedProcessOutput where
  outcome : StopOutcome
  cancelReply : CancelReply
  mayTerminate : Bool
  deriving ToJson

/-- A real test-owned process group reaches three observation pairs: a fresh
group stopped by its owner, the same group stopped again after it exited, and a
recorded start identity the running leader no longer matches. A group that
survives SIGKILL cannot be constructed on a test host, so `running` after a
stop is not reached here. -/
@[conformance_family]
def ownedProcess : Family ObservationInput OwnedProcessOutput where
  inputs := (named ["process_running_then_exited", "process_exited_then_exited",
      "process_unowned_then_unowned"]).map fun c =>
    { name := c.name, before := c.before, after := c.after }
  model := fun i =>
    { outcome := stopOutcome i.before i.after
    , cancelReply := cancelReply (stopOutcome i.before i.after)
    , mayTerminate := ManagedExec.mayTerminate i.before }

structure ReplyOutput where
  cancelReply : CancelReply
  durableState : ToolExecution.ToolCallState
  deriving ToJson

/-- A background stop with neither a live worker nor a process record has no
ownership evidence: the unowned observation. -/
@[conformance_family]
def unrecordedCancel : Family Case ReplyOutput where
  inputs := named ["process_unowned_then_unowned"]
  model := fun c => { cancelReply := cancelReply (outcomeOf c), durableState := durableStateOf c }

/-- An operator stop of a running invocation whose process exit is never
observed: the owner signals and still observes it running. -/
@[conformance_family]
def clientUnverified : Family Case ReplyOutput where
  inputs := named ["process_running_then_running"]
  model := fun c => { cancelReply := cancelReply (outcomeOf c), durableState := durableStateOf c }

/-- An operator stop of a released native future with no process record: the
running invocation is observed stopped. -/
@[conformance_family]
def clientReleasedNative : Family Case ReplyOutput where
  inputs := named ["process_running_then_exited"]
  model := fun c => { cancelReply := cancelReply (outcomeOf c), durableState := durableStateOf c }

/-- A model `cancel_process` whose native worker released but spawned a process
whose exit it cannot prove. -/
@[conformance_family]
def modelCancelUnverified : Family Case ReplyOutput where
  inputs := named ["live_spawned_unverified_true"]
  model := fun c => { cancelReply := cancelReply (outcomeOf c), durableState := durableStateOf c }

structure DurableOutput where
  durableState : ToolExecution.ToolCallState
  deriving ToJson

/-- A stop while a workspace approval waits: the released worker never spawned
its process. -/
@[conformance_family]
def approvalWaitStop : Family Case DurableOutput where
  inputs := named ["live_never_spawned_true"]
  model := fun c => { durableState := durableStateOf c }

structure RowOutput where
  durableState : ToolExecution.ToolCallState
  retainRecord : Bool
  deriving ToJson

/-- A cancelled background worker whose process is still observed running
after the kill grace. -/
@[conformance_family]
def unverifiedWorker : Family Case RowOutput where
  inputs := named ["process_running_then_running"]
  model := fun c => { durableState := durableStateOf c, retainRecord := retainProcessRecord c.after }

/-- An interrupted foreground call whose process is still observed running is
handed to background control. -/
@[conformance_family]
def unverifiedForeground : Family Case RowOutput where
  inputs := named ["process_running_then_running"]
  model := fun c => { durableState := durableStateOf c, retainRecord := retainProcessRecord c.after }

structure RetainOutput where
  retainRecord : Bool
  deriving ToJson

/-- A recorded group whose leader is gone while the group itself is live. -/
@[conformance_family]
def goneLeader : Family Case RetainOutput where
  inputs := named ["process_running_then_unowned"]
  model := fun c => { retainRecord := retainProcessRecord c.after }

structure TerminalsOutput where
  preserveTimeout : Bool
  preserveFailure : Bool
  deriving ToJson

/-- A later operator stop of a worker that already timed out or failed and
whose process exited. -/
@[conformance_family]
def laterStop : Family Case TerminalsOutput where
  inputs := named ["live_exited_true"]
  model := fun _ =>
    { preserveTimeout := firstTerminalIntent (some .timedOut) .cancelled == .timedOut
    , preserveFailure := firstTerminalIntent (some .failed) .cancelled == .failed }

structure TimeoutOutput where
  preserveTimeout : Bool
  deriving ToJson

/-- A native deadline decided before cleanup and before a later cancellation. -/
@[conformance_family]
def timeoutBeforeCleanup : Family Case TimeoutOutput where
  inputs := named ["live_exited_true"]
  model := fun _ => { preserveTimeout := firstTerminalIntent (some .timedOut) .cancelled == .timedOut }

/-- The interrupt sweep over a retained foreground worker whose confirmed stop
carries a timeout. -/
@[conformance_family]
def interruptSweep : Family Case TimeoutOutput where
  inputs := named ["live_exited_true"]
  model := fun _ => { preserveTimeout := firstTerminalIntent (some .timedOut) .cancelled == .timedOut }

structure ReceiptOutput where
  retireBeforeCommit : Bool
  retainedCompletionNotified : Bool
  retiredCompletionNotified : Bool
  durableState : ToolExecution.ToolCallState
  deriving ToJson

/-- A confirmed stop of a worker that never spawned whose terminal write fails
once. -/
@[conformance_family]
def terminalWriteFailure : Family Case ReceiptOutput where
  inputs := named ["live_never_spawned_true"]
  model := fun c =>
    { retireBeforeCommit := retireStopReceipt false
    , retainedCompletionNotified := ownerCompletionNotified true
    , retiredCompletionNotified := ownerCompletionNotified false
    , durableState := durableStateOf c }

structure HandoffReceiptOutput where
  retireBeforeCommit : Bool
  deriving ToJson

/-- A failed recovery handoff whose confirmed stop's terminal write fails. -/
@[conformance_family]
def failedHandoffReceipt : Family Case HandoffReceiptOutput where
  inputs := named ["live_exited_true"]
  model := fun _ => { retireBeforeCommit := retireStopReceipt false }

structure PendingFailureOutput where
  pendingFailureBeatsCancel : Bool
  pendingFailureResolves : Bool
  deriving ToJson

/-- A native failure decided during cleanup, then a cancellation. -/
@[conformance_family]
def pendingFailure : Family Case PendingFailureOutput where
  inputs := named ["live_exited_true"]
  model := fun _ =>
    { pendingFailureBeatsCancel := observeIntent .failurePending .cancelled == .failurePending
    , pendingFailureResolves := observeIntent .failurePending .failed == .decided .failed }

structure EarlyConfirmationOutput where
  outcome : StopOutcome
  deriving ToJson

/-- A stop confirmed by observed exit before the worker releases. -/
@[conformance_family]
def earlyConfirmation : Family Case EarlyConfirmationOutput where
  inputs := named ["live_exited_true"]
  model := fun c => { outcome := outcomeOf c }

example : stopOutcome .running .exited = .stopped ∧
    cancelReply (stopOutcome .running .exited) = .cancelled := by decide

example : stopOutcome .exited .exited = .alreadyExited ∧
    ManagedExec.mayTerminate .exited = false := by decide

example : stopOutcome .unowned .unowned = .notOwned ∧
    cancelReply (stopOutcome .unowned .unowned) = .lost := by decide

example : retainProcessRecord .unowned = true := by decide

example : firstTerminalIntent (some .timedOut) .cancelled = .timedOut ∧
    firstTerminalIntent (some .failed) .cancelled = .failed := by decide

end Conformance.ProcessStop
