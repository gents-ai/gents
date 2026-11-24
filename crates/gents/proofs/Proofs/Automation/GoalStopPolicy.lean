import Proofs.Automation.Goals

namespace GoalStopPolicy

/-- What one blocked report does, set per Goal by its user in the Goal's stop
policy document. -/
inductive OnBlocked where
  /-- The report pauses the Goal as `blocked`. -/
  | stopFirst
  /-- The first report gets one confirmation turn; a report in that turn
  pauses the Goal. -/
  | confirmOnce
  /-- The report is kept for the user and the Goal continues; the no-progress
  pause and any budget still bound it. -/
  | keepTrying
  deriving DecidableEq, Repr

inductive Outcome where
  | pause
  | confirm
  | continue
  deriving DecidableEq, Repr

/-- `confirming` holds when the reporting turn is the confirmation turn a
`confirmOnce` report was given. -/
def onBlockedReport : OnBlocked → (confirming : Bool) → Outcome
  | .stopFirst, _ => .pause
  | .confirmOnce, false => .confirm
  | .confirmOnce, true => .pause
  | .keepTrying, _ => .continue

/-- The user's policy for a Goal. A Goal without a policy document runs under
`defaults`: one blocked report pauses it and it has no ceiling, the Goal's own
budget being the only one. -/
structure Policy where
  onBlocked : OnBlocked
  tokenCeiling : Option Nat
  turnCeiling : Option Nat
  lifted : Bool
  deriving DecidableEq, Repr

def defaults : Policy := ⟨.stopFirst, none, none, false⟩

/-- The budget the harness enforces: the smaller of the Goal's own budget (the
Agent's or Task's request) and the user's ceiling when both are set; a lifted
policy enforces the Goal's own budget only. -/
def effectiveBudget (policy : Policy) (goalBudget : Option Nat) : Option Nat :=
  match goalBudget, (if policy.lifted then none else policy.tokenCeiling) with
  | none, ceiling => ceiling
  | budget, none => budget
  | some budget, some ceiling => some (min budget ceiling)

/-- The Goal's limit is reached when its charged tokens reach the enforced
budget or, unless the policy is lifted, its goal turns reach the user's turn
ceiling. Reaching it gives the Goal its one wrap-up turn, as a budget does. -/
def limitReached (policy : Policy) (goalBudget : Option Nat) (tokensUsed turns : Nat) : Bool :=
  Goals.budgetReached (effectiveBudget policy goalBudget) tokensUsed ||
    (!policy.lifted && policy.turnCeiling.any (· ≤ turns))

theorem no_policy_limit_is_the_goal_budget (goalBudget : Option Nat) (tokensUsed turns : Nat) :
    limitReached defaults goalBudget tokensUsed turns = Goals.budgetReached goalBudget tokensUsed := by
  cases goalBudget <;> simp [limitReached, defaults, effectiveBudget, Goals.budgetReached]

theorem stop_first_pauses_on_one_report (confirming : Bool) :
    onBlockedReport .stopFirst confirming = .pause := rfl

theorem confirm_once_pauses_by_the_second_report :
    onBlockedReport .confirmOnce false = .confirm ∧
      onBlockedReport .confirmOnce true = .pause := ⟨rfl, rfl⟩

theorem no_policy_pauses_on_one_report (confirming : Bool) :
    onBlockedReport defaults.onBlocked confirming = .pause := rfl

theorem no_policy_adds_no_ceiling (goalBudget : Option Nat) :
    effectiveBudget defaults goalBudget = goalBudget := by
  cases goalBudget <;> rfl

/-- The Agent cannot raise its budget past the user's ceiling by changing the
Goal's own budget, the one field of it the Agent writes. -/
theorem the_ceiling_bounds_every_goal_budget (policy : Policy) (goalBudget : Option Nat)
    (ceiling : Nat) (set : policy.tokenCeiling = some ceiling) (held : policy.lifted = false) :
    ∃ budget, effectiveBudget policy goalBudget = some budget ∧ budget ≤ ceiling := by
  cases goalBudget with
  | none => exact ⟨ceiling, by simp [effectiveBudget, set, held], Nat.le_refl _⟩
  | some budget =>
      exact ⟨min budget ceiling, by simp [effectiveBudget, set, held], Nat.min_le_right _ _⟩

end GoalStopPolicy
