import Proofs.PluginJob.TargetResolution
import Proofs.Recovery.Sweeps.PluginJob

namespace PluginJob.RecoveryTrace

/-- Facts owned by the sweep's durable read and the driver, rather than by
guest output. `stepped` records a committed invocation, not its notification.
The row supplied below is the result of the actual admission/settlement owner. -/
structure Observation where
  sessionStopped : Bool
  deadlineExpired : Bool
  available : Bool
  stepped : Bool
  stepInFlight : Bool
  stepNew : Nat
  deriving DecidableEq, Repr

def project (job : Job) (observation : Observation) : Recovery.PluginJobRow :=
  { running := job.state == .running
  , terminalChildren := terminalCount job
  , observed := job.observedTerminal
  , live := liveCount job
  , sessionStopped := observation.sessionStopped
  , deadlineExpired := observation.deadlineExpired
  , available := observation.available
  , stepped := observation.stepped
  , stepInFlight := observation.stepInFlight
  , stepNew := observation.stepNew }

def afterStep (ctx : StepContext) (job : Job) (output : StepOutput)
    (children : SpawnKey → Recovery.KillObservation) (observation : Observation) :
    Recovery.PluginJobRow :=
  project (settleStep ctx job output children).1 observation

def recoverAfterStep (ctx : StepContext) (job : Job) (output : StepOutput)
    (children : SpawnKey → Recovery.KillObservation) (observation : Observation) :
    Recovery.PluginJobRow :=
  Recovery.pluginJobRecoveryTick (afterStep ctx job output children observation)

theorem refused_step_cannot_be_redriven (ctx : StepContext) (job : Job) (output : StepOutput)
    (children : SpawnKey → Recovery.KillObservation) (observation : Observation)
    (refusal : Refusal) (refused : admitStep ctx job output = .error refusal)
    (nonterminal : refusal ≠ .terminal) :
    Recovery.pluginJobStepInvocationAllowed
      (afterStep ctx job output children observation).running observation.sessionStopped = false := by
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
  simp only [afterStep, project, settled, Recovery.pluginJobStepInvocationAllowed]
  rfl

theorem settled_recovery_has_no_stale_row (ctx : StepContext) (job : Job) (output : StepOutput)
    (children : SpawnKey → Recovery.KillObservation) (observation : Observation)
    (stale : Recovery.pluginJobStale (afterStep ctx job output children observation) ∨
      Recovery.pluginJobStoppedSessionStale (afterStep ctx job output children observation)) :
    ¬ Recovery.pluginJobStale (recoverAfterStep ctx job output children observation) := by
  intro remains
  exact Recovery.pluginJobRecoveryTick_not_stale
    (afterStep ctx job output children observation) stale (Or.inl remains)

end PluginJob.RecoveryTrace
