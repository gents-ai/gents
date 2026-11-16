import Proofs.ToolCalls.PluginJob.Step
import Proofs.Runtime.RuntimeReconcile.JobDriverAdmission
import Proofs.Conformance.Family

namespace PluginJob
deriving instance Lean.ToJson for RowState
end PluginJob

namespace PluginJob.Step
deriving instance Lean.ToJson for Attempt
end PluginJob.Step

namespace Conformance.PluginJobStep

open Lean _root_.PluginJob _root_.PluginJob.Step

private def runningJob : Job :=
  { root := "row-1", rowDoc := "row-1", callerSession := "session-1", callerCwd := "/work"
  , handle := "flow-1", callerHop := 0
  , call := { argv := ["run"], args := none, script := none, agentBudget := none }
  , rootCall := none, resume := none, pin := none, children := [], spent := 0
  , observedTerminal := 0, consecutiveFailures := 0, state := .running }

private def noChildren : SpawnKey → Recovery.KillObservation := fun _ => .unresolved

inductive Lock where
  | free
  | beforeRead
  | afterPrecheck

deriving instance ToJson for Lock

structure PublicationInput where
  lock : Lock
  priorFailures : Nat
  deriving ToJson

structure PublicationOutput where
  attempt : Attempt
  rowState : RowState
  rowUnchanged : Bool
  consecutiveFailures : Nat
  afterRelease : Attempt
  deriving ToJson

/-- A running result job whose row has failed `priorFailures` times takes one
step while a plugin publication holds the store: from before the step reads
the published record, or from after it read it and before the guest runs. The
lock is then released and the row stepped again. -/
@[conformance_family]
def publication : Family PublicationInput PublicationOutput where
  inputs := [Lock.free, .beforeRead, .afterPrecheck].flatMap fun lock =>
    [0, 2].map fun priorFailures => { lock, priorFailures }
  model := fun i =>
    let job := { runningJob with consecutiveFailures := i.priorFailures }
    let attempt := match i.lock with
      | .free => resultAttempt job
      | .beforeRead | .afterPrecheck => .publicationBusy
    let after := afterAttempt noChildren job attempt
    { attempt
      rowState := after.state
      rowUnchanged := decide (after = job)
      consecutiveFailures := after.consecutiveFailures
      afterRelease := resultAttempt after }

inductive FirstStep where
  | committed
  | malformedOutput

deriving instance ToJson for FirstStep

structure GuestStateInput where
  stateBytes : Option Nat
  deriving ToJson

structure GuestStateOutput where
  firstStep : FirstStep
  storedGuest : Bool
  rowState : RowState
  failureCode : Option String
  deriving ToJson

private def runningRow : Row :=
  { job := runningJob, latched := [], progress := none, revision := 1, guest := none }

/-- A running job whose guest spawns one child and carries a state of
`stateBytes` serialized bytes takes one step, and the driver then steps it
until it stops retrying. -/
@[conformance_family]
def guestState : Family GuestStateInput GuestStateOutput where
  inputs := [none, some 32, some guestStateMaxBytes, some (guestStateMaxBytes + 1)].map
    fun stateBytes => { stateBytes }
  model := fun i =>
    if guestStateAdmitted i.stateBytes then
      { firstStep := .committed
        storedGuest := (applyGuest (i.stateBytes.map toString) runningRow).guest.isSome
        rowState := runningRow.job.state
        failureCode := none }
    else
      let (row, refusal) := failUntilRaised noChildren maxConsecutiveStepFailures runningRow.job
      { firstStep := .malformedOutput
        storedGuest := runningRow.guest.isSome
        rowState := row.state
        failureCode := refusal.bind Refusal.code }

inductive OpKind where
  | hold
  | releasePermits
  | retire

deriving instance ToJson for OpKind

structure DriverOp where
  kind : OpKind
  wake : Bool
  retireDuring : Bool
  deriving ToJson

structure DriverTrace where
  permitsTaken : Bool
  ops : List DriverOp
  deriving ToJson

structure DriverObservation where
  admitted : Bool
  retiredDuring : Option Bool
  stepping : Bool
  stepsRun : Nat
  deriving ToJson

private def toOp (op : DriverOp) : RuntimeReconcile.JobDriverAdmission.Op :=
  match op.kind with
  | .hold => .hold op.wake op.retireDuring
  | .releasePermits => .releasePermits
  | .retire => .retire

private def hold (wake retireDuring : Bool) : DriverOp := { kind := .hold, wake, retireDuring }
private def plain (kind : OpKind) : DriverOp := { kind, wake := false, retireDuring := false }

/-- Recovery holds, permit releases and operator retirements of one running
job row whose step commits, on a runtime that admits work until it retires. -/
@[conformance_family]
def driverAdmission : Family DriverTrace (List DriverObservation) where
  inputs :=
    [ { permitsTaken := true
        ops := [hold true true, plain .retire, plain .releasePermits, plain .retire,
          hold false false] }
    , { permitsTaken := false
        ops := [hold false true, hold true false, plain .retire, hold true false] }
    , { permitsTaken := true
        ops := [hold false false, plain .retire, plain .releasePermits, hold true false] } ]
  model := fun t =>
    let observed := RuntimeReconcile.JobDriverAdmission.run
      (RuntimeReconcile.JobDriverAdmission.idle t.permitsTaken) (t.ops.map toOp)
    observed.map fun o =>
      { admitted := o.admitted, retiredDuring := o.retiredDuring, stepping := o.stepping
        stepsRun := o.stepsRun }

end Conformance.PluginJobStep
