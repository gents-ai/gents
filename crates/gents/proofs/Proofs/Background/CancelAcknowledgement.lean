import Proofs.Request.State

/-! Projection of the existing remote-cancellation acknowledgement owner.
The adapter supplies locally authenticated bridge ownership and observed child
facts. An interrupt acknowledgement is not proof that the child has stopped.
Only the bridge acknowledgement fields can change here. -/
namespace Subagent.CancelAcknowledgement

structure Row where
  pending : Bool := false
  intentAt : Option Time := none
  stuckSince : Option Time := none
  deriving DecidableEq, Repr

structure Observation where
  locallyOwned : Bool
  childState : Option RequestState
  childInterruptRequested : Bool
  now : Time
  deriving DecidableEq, Repr

inductive Outcome where
  | pending | stuck | acked
  deriving DecidableEq, Repr

structure Result where
  row : Row
  outcome : Outcome
  deriving DecidableEq, Repr

def childAcknowledged (observation : Observation) : Bool :=
  observation.childState.any (fun state => decide (isTerminal state)) ||
    observation.childInterruptRequested

/-- Missing/malformed native intent timestamps are represented by `none`.
A previously marked row remains marked without emitting another Stuck event;
acknowledgement clears both pending and stuck, exactly as the native owner. -/
def observe (threshold : Nat) (row : Row) (observation : Observation) :
    Option Result :=
  if !row.pending || !observation.locallyOwned then none else
  if childAcknowledged observation then
    some ⟨{ row with pending := false, stuckSince := none }, .acked⟩
  else if row.stuckSince.isNone && row.intentAt.any (fun intent =>
      intent ≤ observation.now && threshold ≤ observation.now - intent) then
    some ⟨{ row with stuckSince := some observation.now }, .stuck⟩
  else some ⟨row, .pending⟩

theorem foreign_observation_is_inert (threshold : Nat) (row : Row)
    (observation : Observation) (foreign : observation.locallyOwned = false) :
    observe threshold row observation = none := by
  simp [observe, foreign]

theorem acknowledged_row_is_no_longer_pending (threshold : Nat) (row : Row)
    (observation : Observation) (result : Result)
    (observed : observe threshold row observation = some result)
    (acked : result.outcome = .acked) :
    result.row.pending = false ∧ result.row.stuckSince = none := by
  unfold observe at observed
  split at observed
  · contradiction
  · split at observed
    · cases observed; exact ⟨rfl, rfl⟩
    · split at observed <;> cases observed <;> contradiction

theorem interrupted_but_running_child_can_acknowledge :
    observe 300 ⟨true, some 0, some 300⟩
      ⟨true, some .processing, true, 301⟩ =
        some ⟨⟨false, some 0, none⟩, .acked⟩ := by
  native_decide

theorem stuck_mark_is_retained_without_repeated_stuck_event :
    observe 300 ⟨true, some 0, some 300⟩
      ⟨true, some .processing, false, 360⟩ =
        some ⟨⟨true, some 0, some 300⟩, .pending⟩ := by
  native_decide

end Subagent.CancelAcknowledgement
