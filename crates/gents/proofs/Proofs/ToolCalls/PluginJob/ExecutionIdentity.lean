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

/-- A wake or immediate step meets the step in flight on its row. Another row's
work is independent; the same actor coalesces into one re-run of the in-flight
step; any other actor is refused and leaves the in-flight step unchanged. -/
inductive WakeOutcome where
  | independent
  | coalesced
  | refused
  deriving BEq, DecidableEq, Repr

def WakeOutcome.toContract : WakeOutcome → String
  | .independent => "independent"
  | .coalesced => "coalesced"
  | .refused => "refused"

def wakeOutcome (running wake : Work) : WakeOutcome :=
  if running.document != wake.document then .independent
  else if acceptsWake running wake then .coalesced else .refused

theorem running_row_actor_change_refused (running wake : Work)
    (same : running.document = wake.document) (different : running.agent ≠ wake.agent) :
    wakeOutcome running wake = .refused := by
  simp [wakeOutcome, same, actor_change_refused running wake different]

theorem coalesced_wake_keeps_actor (running wake : Work)
    (coalesced : wakeOutcome running wake = .coalesced) : running.agent = wake.agent := by
  unfold wakeOutcome at coalesced
  split at coalesced
  · cases coalesced
  · split at coalesced
    · exact accepted_wake_keeps_actor running wake (by assumption)
    · cases coalesced

end PluginJob.ExecutionIdentity
