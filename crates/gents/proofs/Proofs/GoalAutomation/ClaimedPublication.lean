import Proofs.GoalAutomation.OperatorResume

/-! Transaction-boundary refinement of the existing claimed → childPresent
phase. Claim has already consumed the Goal sequence. `commit` means the SAME
native ConfigApplyTxn committed the Goal CAS write and signed AgentRequest;
false includes native conflict and discard. No new durable state or clock.
The Goal write must participate even though modeled Goal fields are unchanged.
-/
namespace GoalAutomation.OperatorResume

structure ClaimedRequest extends Request where
  expectedLastContinuedFrom : Option Nat
  deriving DecidableEq, Repr

def publishClaimed (s : Snapshot) (r : ClaimedRequest) (commit : Bool) :
    Snapshot × Outcome :=
  if !r.authorized || !r.parentBelongsToGoal then (s, .denied)
  else match s.children.find? (sameKey r.binding) with
  | some existing => if existing = r.binding then (s, .recovered) else (s, .conflict)
  | none =>
      if s.goal.status ≠ r.expectedStatus || s.sequence != r.expectedSequence ||
          s.lastContinuedFrom != r.expectedLastContinuedFrom then (s, .stale)
      else if (s.goal.status != .active && s.goal.status != .budgetLimited) ||
          !r.terminalParent || !r.sessionIdle ||
          r.binding.predecessor != s.latestRequest ||
          s.lastContinuedFrom != some r.binding.predecessor ||
          r.binding.sequence == 0 || r.binding.sequence != s.sequence then (s, .illegal)
      else if commit then
        ({ s with latestRequest := r.binding.child,
                  children := r.binding :: s.children }, .created)
      else (s, .rolledBack)

theorem publication_preserves_claim_and_budget (s : Snapshot) (r : ClaimedRequest)
    (commit : Bool) :
    (publishClaimed s r commit).1.goal = s.goal ∧
    (publishClaimed s r commit).1.sequence = s.sequence ∧
    (publishClaimed s r commit).1.lastContinuedFrom = s.lastContinuedFrom ∧
    (publishClaimed s r commit).1.tokensUsed = s.tokensUsed ∧
    (publishClaimed s r commit).1.tokenBudget = s.tokenBudget := by
  unfold publishClaimed
  split
  · simp
  · split
    · split <;> simp
    · split
      · simp
      · split
        · simp
        · split <;> simp

theorem discarded_claimed_publication_is_noop (s : Snapshot) (r : ClaimedRequest) :
    (publishClaimed s r false).1 = s := by
  unfold publishClaimed
  split
  · rfl
  · split
    · split <;> rfl
    · split
      · rfl
      · split <;> rfl

theorem created_requires_current_claim (s : Snapshot) (r : ClaimedRequest)
    (commit : Bool) (h : (publishClaimed s r commit).2 = .created) :
    s.goal.status = r.expectedStatus ∧ s.sequence = r.expectedSequence ∧
    s.lastContinuedFrom = r.expectedLastContinuedFrom ∧
    (s.goal.status = .active ∨ s.goal.status = .budgetLimited) ∧
    (publishClaimed s r commit).1.children = r.binding :: s.children := by
  unfold publishClaimed at *
  split at * <;> try simp_all
  split at *
  · split at * <;> simp_all
  · split at * <;> try simp_all
    split at * <;> try simp_all
    split at * <;> simp_all
    rename_i hguard hcommit
    have hn : r.expectedSequence ≠ 0 := by
      intro hz
      exact hguard.1.2 (hguard.2.trans hz)
    have hs := hguard.1.1.1.1.1.1
    by_cases ha : r.expectedStatus = .active
    · simp [ha, hn]
    · have hb := hs ha
      simp [hb, hn]

end GoalAutomation.OperatorResume
