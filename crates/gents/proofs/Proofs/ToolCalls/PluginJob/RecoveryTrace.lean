import Proofs.PluginJob.Admission
import Proofs.Recovery.Sweeps.PluginJob

namespace PluginJob.RecoveryTrace

/-- Native reads provide lifecycle, progress revision, time and availability.
Availability is measured before session-stop precedence is applied. Database
read atomicity and wall-clock accuracy remain premises of the native adapter. -/
structure DurableRead where
  sessionStopped : Bool
  nowMillis : Nat
  available : Bool
  progressRevision : Nat
  stepInFlight : Bool
  deriving DecidableEq, Repr

def project (job : Job) (lifecycleRunning : Bool) (read : DurableRead) : Recovery.PluginJobRow :=
  { running := lifecycleRunning
  , terminalChildren := terminalCount job
  , observed := job.observedTerminal
  , live := liveCount job
  , sessionStopped := read.sessionStopped
  , deadlineExpired := match job.pin with
      | none => false
      | some pin => decide (pin.adoptedAt + pin.ceilingAtPin.maxWallSecs * 1000 ≤ read.nowMillis)
  , available := !read.sessionStopped && read.available
  , stepped := decide (read.progressRevision > 1)
  , stepInFlight := read.stepInFlight
  , stepNew := 0 }

/-- Admission refusal does not commit its synthetic closed row. A crash before
its separate kill transaction leaves the original durable row for re-admission.
After the kill commits, the terminal row is durable. -/
inductive Interruption where
  | beforeKill
  | afterKill
  deriving DecidableEq, Repr

def durableAfterStep (ctx : StepContext) (job : Job) (output : StepOutput)
    (children : SpawnKey → Recovery.KillObservation) (phase : Interruption) : Job :=
  match admitStep ctx job output with
  | .ok admitted => admitted
  | .error _ => match phase with
      | .beforeKill => job
      | .afterKill => (settleStep ctx job output children).1

def afterStep (ctx : StepContext) (job : Job) (output : StepOutput)
    (children : SpawnKey → Recovery.KillObservation) (phase : Interruption) (read : DurableRead) :
    Recovery.PluginJobRow :=
  let durable := durableAfterStep ctx job output children phase
  project durable (durable.state == .running) read

def recoverAfterStep (ctx : StepContext) (job : Job) (output : StepOutput)
    (children : SpawnKey → Recovery.KillObservation) (phase : Interruption) (read : DurableRead) :
    Recovery.PluginJobRow :=
  Recovery.pluginJobRecoveryTick (afterStep ctx job output children phase read)

theorem refusal_before_kill_preserves_durable_row (ctx : StepContext) (job : Job)
    (output : StepOutput) (children : SpawnKey → Recovery.KillObservation)
    (refusal : Refusal) (refused : admitStep ctx job output = .error refusal) :
    durableAfterStep ctx job output children .beforeKill = job ∧
      admitStep ctx (durableAfterStep ctx job output children .beforeKill) output = .error refusal := by
  simp [durableAfterStep, refused]

theorem refused_step_after_kill_cannot_be_redriven (ctx : StepContext) (job : Job)
    (output : StepOutput) (children : SpawnKey → Recovery.KillObservation) (read : DurableRead)
    (refusal : Refusal) (refused : admitStep ctx job output = .error refusal)
    (nonterminal : refusal ≠ .terminal) :
    Recovery.pluginJobStepInvocationAllowed
      (afterStep ctx job output children .afterKill read).running read.sessionStopped = false := by
  have running : job.state = .running := by
    by_contra stopped
    have terminal : admitStep ctx job output = .error .terminal := by
      simp only [admitStep, if_pos stopped]
    exact nonterminal (Except.error.inj (refused.symm.trans terminal))
  have settled : (settleStep ctx job output children).1.state = .failed := by
    unfold settleStep
    rw [refused]
    cases refusal <;> simp_all only [ne_eq, not_true_eq_false, terminalizeRow,
      if_pos running, ite_true]
  simp [afterStep, durableAfterStep, refused, project, settled,
    Recovery.pluginJobStepInvocationAllowed]

theorem settled_recovery_has_no_stale_row (ctx : StepContext) (job : Job) (output : StepOutput)
    (children : SpawnKey → Recovery.KillObservation) (phase : Interruption) (read : DurableRead)
    (stale : Recovery.pluginJobStale (afterStep ctx job output children phase read) ∨
      Recovery.pluginJobStoppedSessionStale (afterStep ctx job output children phase read)) :
    ¬ (Recovery.pluginJobStale (recoverAfterStep ctx job output children phase read) ∨
      Recovery.pluginJobStoppedSessionStale (recoverAfterStep ctx job output children phase read)) :=
  Recovery.pluginJobRecoveryTick_not_stale (afterStep ctx job output children phase read) stale

/-- A kill transaction observes the committed admission row, including spawns
that committed after an earlier recovery read. It must not use that earlier
read as its child snapshot. Atomic transaction validation is a native premise. -/
def killAfterCommit (ctx : StepContext) (job : Job) (output : StepOutput)
    (control : Control) (children : SpawnKey → Recovery.KillObservation) :=
  match admitStep ctx job output with
  | .ok committed => Control.kill committed control children
  | .error _ => Control.kill job control children

theorem kill_after_committed_step_reaches_each_live_child
    (ctx : StepContext) (job committed : Job) (output : StepOutput)
    (control : Control) (children : SpawnKey → Recovery.KillObservation)
    (admitted : admitStep ctx job output = .ok committed)
    (running : committed.state = .running) (terminal : RowState)
    (ends : control.ends = some terminal) (child : Child)
    (member : child ∈ liveOwnChildren committed) :
    (child.key, Recovery.killAction (children child.key)) ∈
      (killAfterCommit ctx job output control children).2 := by
  simp only [killAfterCommit, admitted, Control.kill, ends, terminalizeRow,
    if_pos running, ite_true]
  exact List.mem_map.mpr ⟨child, member, rfl⟩

end PluginJob.RecoveryTrace
