import Proofs.CanonicalOutput.Execution.Gate
import Proofs.CanonicalOutput.Execution.Properties

namespace CanonicalOutput.Execution.DispatchObservation

/-- A successful durable replay is not a newly won right to invoke a tool.
Receipt loss may leave the row Running while conveying no execution authority.
Native refinement must obtain the pre-state under the same transaction/gate as
the dispatch election; a caller's cached Pending handle is not this witness. -/
inductive Observation where
  | rejected
  | unacknowledged
  | replay
  | fresh
  deriving DecidableEq, Repr

def mayInvoke : Observation → Bool
  | .fresh => true
  | _ => false

/-- Compose the existing durable dispatch with its caller-visible receipt.
No new lifecycle or recovery transition is introduced. A rejected transaction
keeps its input world; a lost receipt retains the committed world. -/
def attempt (before : World) (actor : Gate.Actor) (now : Time)
    (generation : Generation) (permit : DispatchPermit) (acknowledged : Bool) :
    World × Observation :=
  match Gate.commit before actor now (.dispatch generation permit) with
  | none => (before, .rejected)
  | some after =>
      (after, if !acknowledged then .unacknowledged
        else if physicalRunning before permit.call then .replay else .fresh)

theorem lost_receipt_never_authorizes (before : World) (actor : Gate.Actor)
    (now : Time) (generation : Generation) (permit : DispatchPermit) :
    mayInvoke (attempt before actor now generation permit false).2 = false := by
  unfold attempt
  split <;> rfl

theorem running_never_authorizes (before : World) (actor : Gate.Actor)
    (now : Time) (generation : Generation) (permit : DispatchPermit)
    (acknowledged : Bool) (hrunning : physicalRunning before permit.call = true) :
    mayInvoke (attempt before actor now generation permit acknowledged).2 = false := by
  unfold attempt
  split
  · rfl
  · cases acknowledged <;> simp [hrunning, mayInvoke]

theorem committed_world_survives_receipt_loss (before after : World)
    (actor : Gate.Actor) (now : Time) (generation : Generation) (permit : DispatchPermit)
    (hcommit : Gate.commit before actor now (.dispatch generation permit) = some after) :
    attempt before actor now generation permit false = (after, .unacknowledged) := by
  simp [attempt, hcommit]

theorem fresh_requires_acknowledged_election (before : World) (actor : Gate.Actor)
    (now : Time) (generation : Generation) (permit : DispatchPermit) (acknowledged : Bool)
    (h : mayInvoke (attempt before actor now generation permit acknowledged).2 = true) :
    acknowledged = true ∧ physicalRunning before permit.call = false ∧
      ∃ after, Gate.commit before actor now (.dispatch generation permit) = some after := by
  unfold attempt at h
  split at h
  · contradiction
  · rename_i after hcommit
    cases acknowledged <;> simp [mayInvoke] at h
    cases hrunning : physicalRunning before permit.call <;> simp [hrunning, mayInvoke] at h
    exact ⟨rfl, rfl, after, hcommit⟩

theorem committed_dispatch_is_running (before after : World) (actor : Gate.Actor)
    (now : Time) (generation : Generation) (permit : DispatchPermit)
    (hcommit : Gate.commit before actor now (.dispatch generation permit) = some after) :
    physicalRunning after permit.call = true := by
  obtain ⟨execution, heval, rfl⟩ := Gate.commit_reads_current_world
    before after actor now (.dispatch generation permit) hcommit
  have hcore := Gate.evaluate_success_core (.dispatch generation permit)
    (Gate.atTime before now) execution heval
  have hdispatch := mapError_success Gate.Error.execution _ _ hcore
  exact (dispatch_requires_committed_intent_and_marks_running
    (Gate.atTime before now) execution generation permit hdispatch).1

/-- A receipt lost after a successful commit cannot be repaired into a second
execution permit by releasing/reacquiring the gate and receiving a replay ACK.
Only gate scheduling fields vary here; no tool lifecycle evidence is assumed. -/
theorem replay_after_commit_never_authorizes (before after : World)
    (actor nextActor : Gate.Actor) (now later : Time) (generation : Generation)
    (permit : DispatchPermit) (owner : Option Gate.Actor)
    (schedule : StorageWriteGate.State) (acknowledged : Bool)
    (hcommit : Gate.commit before actor now (.dispatch generation permit) = some after) :
    mayInvoke (attempt { after with gateOwner := owner, gateSchedule := schedule }
      nextActor later generation permit acknowledged).2 = false := by
  apply running_never_authorizes
  exact committed_dispatch_is_running before after actor now generation permit hcommit

end CanonicalOutput.Execution.DispatchObservation
