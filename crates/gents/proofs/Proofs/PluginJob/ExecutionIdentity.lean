import Proofs.Identity.State

namespace PluginJob.ExecutionIdentity

/-- A wake carries its selected agent from the runtime's active agent snapshot
or the tool lifecycle. A document ID alone is not an actor: recovery and every
step must use the supplied agent to read and write, with no runtime fallback. -/
structure Work where
  document : String
  agent : Identity.DID
  deriving BEq, DecidableEq, Repr

/-- An in-flight step retains its original actor. A wake with another actor is
refused rather than changing the authority of an already scheduled step. -/
def acceptsWake (running wake : Work) : Bool :=
  running.document == wake.document && running.agent == wake.agent

def scheduledActor (work : Work) : Identity.DID := work.agent

theorem step_uses_selected_agent (work : Work) : scheduledActor work = work.agent := rfl

theorem actor_change_refused (running wake : Work) (different : running.agent ≠ wake.agent) :
    acceptsWake running wake = false := by
  simp [acceptsWake, different]

theorem accepted_wake_keeps_actor (running wake : Work)
    (accepted : acceptsWake running wake = true) : running.agent = wake.agent := by
  simp [acceptsWake] at accepted
  exact accepted.2

end PluginJob.ExecutionIdentity
