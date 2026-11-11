import Proofs.Session.Request.State

/-! Request operations shared by every entry point: CLI `request submit`,
`gents -p`, the shell, the desktop, the editor and the agent tool resolve
delivery, identify requests and admit reissues through these rules, so no
client keeps its own interpretation. -/

namespace SessionOperations

/-- What a sender asked for when the session may be busy. -/
inductive DeliveryChoice where
  | unspecified
  | queue
  | steer
  deriving DecidableEq, Repr

/-- How a message enters a busy session's queue. -/
inductive Delivery where
  | queue
  | steer
  deriving DecidableEq, Repr

/-- One rule for every entry point: a busy session queues the message unless
the sender asks to steer. Steering redirects a running turn, so it is never
inferred from an unspecified choice. -/
def resolveDelivery (choice : DeliveryChoice) : Delivery :=
  match choice with
  | .steer => .steer
  | .unspecified | .queue => .queue

theorem resolveDelivery_unspecified : resolveDelivery .unspecified = .queue := rfl

/-- Steering happens only when it was chosen. -/
theorem resolveDelivery_steer_iff (choice : DeliveryChoice) :
    resolveDelivery choice = .steer ↔ choice = .steer := by
  cases choice <;> decide

/-- How many requests carry the requested id within the node and requester
scope of the lookup. -/
inductive Matches where
  | none
  | one
  | many
  deriving DecidableEq, Repr

inductive Lookup where
  | notFound
  | found
  | ambiguous
  deriving DecidableEq, Repr

/-- A request is identified within its node and requester scope. A request id
that names more than one request in that scope is an error the caller resolves
by naming the requester; the lookup never silently takes the first row. -/
def requestLookup (found : Matches) : Lookup :=
  match found with
  | .none => .notFound
  | .one => .found
  | .many => .ambiguous

theorem requestLookup_found_iff (found : Matches) :
    requestLookup found = .found ↔ found = .one := by
  cases found <;> decide

/-- `retry` runs a failed turn again in its own session; `resend` sends a
stale-dead request again in a fresh session. -/
inductive ReissueKind where
  | retry
  | resend
  deriving DecidableEq, Repr

/-- The parent's lifecycle as a reissue sees it: `staleDead` is a dead request
whose failure reason is `Stale`; `otherTerminal` is any other terminal state;
`live` is a request that has not terminated. -/
inductive ReissueParent where
  | failed
  | staleDead
  | otherTerminal
  | live
  deriving DecidableEq, Repr

inductive ReissueDecision where
  | admit
  | refuseParentState
  | refuseNotInteractive
  | refuseBudgetExhausted
  | refuseDeadlineClosed
  | refuseNotLatest
  deriving DecidableEq, Repr

/-- The handler, not the client, decides a reissue and owns the retry budget
for both kinds. A retry needs a failed parent and then refuses, in this order,
a turn no person started, an exhausted budget, a closed deadline, and a turn
that is not its session's latest. A resend needs a stale-dead parent and only
the budget: it opens a fresh session, so interactivity, the parent's deadline
and its position in the old session do not apply. -/
def reissueDecision (kind : ReissueKind) (parent : ReissueParent)
    (interactive budgetRemains deadlineOpen latest : Bool) : ReissueDecision :=
  match kind, parent with
  | .retry, .failed =>
    if !interactive then .refuseNotInteractive
    else if !budgetRemains then .refuseBudgetExhausted
    else if !deadlineOpen then .refuseDeadlineClosed
    else if !latest then .refuseNotLatest
    else .admit
  | .resend, .staleDead => if budgetRemains then .admit else .refuseBudgetExhausted
  | _, _ => .refuseParentState

/-- Every admitted reissue had budget remaining; the other facts only refuse. -/
theorem reissueDecision_admit_budget (kind : ReissueKind) (parent : ReissueParent)
    (interactive budgetRemains deadlineOpen latest : Bool)
    (h : reissueDecision kind parent interactive budgetRemains deadlineOpen latest = .admit) :
    budgetRemains = true := by
  cases kind <;> cases parent <;> cases interactive <;> cases budgetRemains <;>
    cases deadlineOpen <;> cases latest <;> simp_all [reissueDecision]

theorem reissueDecision_retry_admit_iff (parent : ReissueParent)
    (interactive budgetRemains deadlineOpen latest : Bool) :
    reissueDecision .retry parent interactive budgetRemains deadlineOpen latest = .admit ↔
      parent = .failed ∧ interactive = true ∧ budgetRemains = true ∧
        deadlineOpen = true ∧ latest = true := by
  cases parent <;> cases interactive <;> cases budgetRemains <;>
    cases deadlineOpen <;> cases latest <;> simp [reissueDecision]

theorem reissueDecision_resend_admit_iff (parent : ReissueParent)
    (interactive budgetRemains deadlineOpen latest : Bool) :
    reissueDecision .resend parent interactive budgetRemains deadlineOpen latest = .admit ↔
      parent = .staleDead ∧ budgetRemains = true := by
  cases parent <;> cases budgetRemains <;> simp [reissueDecision]

/-- The budget a parent leaves its successor: the request model's retry
budget is exhausted once `retryCount ≥ maxRetries`. -/
def budgetRemains (retryCount maxRetries : Nat) : Bool :=
  decide (retryCount < maxRetries)

theorem budgetRemains_iff_not_exhausted (r : RequestContext) :
    budgetRemains r.retryCount r.maxRetries = true ↔ ¬ r.retriesExhausted := by
  simp [budgetRemains, RequestContext.retriesExhausted]

/-- Both reissue kinds charge the parent's budget: the successor carries one
more retry than its parent, whichever session it runs in. -/
def successorRetryCount (parent : Nat) : Nat := parent + 1

/-- One reissue of a parent holding `count` retries under the successor rule
`successor`: the successor's count when the budget admits it. -/
def reissueStep (successor : Nat → Nat) (maxRetries count : Nat) : Option Nat :=
  if budgetRemains count maxRetries then some (successor count) else none

/-- `n` successive reissues, each of the previous successor, starting from a
parent holding `count` retries; `none` once one is refused. -/
def reissueChain (successor : Nat → Nat) (maxRetries : Nat) : Nat → Nat → Option Nat
  | 0, count => some count
  | n + 1, count =>
    (reissueStep successor maxRetries count).bind (reissueChain successor maxRetries n)

theorem reissueChain_count (maxRetries : Nat) :
    ∀ (n count result : Nat),
      reissueChain successorRetryCount maxRetries n count = some result →
        result = count + n ∧ (0 < n → count + n ≤ maxRetries)
  | 0, count, result, h => by
    simp [reissueChain] at h
    omega
  | n + 1, count, result, h => by
    simp only [reissueChain, reissueStep] at h
    by_cases hb : budgetRemains count maxRetries = true
    · simp only [hb, if_true, Option.some_bind] at h
      have hlt : count < maxRetries := by simpa [budgetRemains] using hb
      have ih := reissueChain_count maxRetries n (successorRetryCount count) result h
      simp only [successorRetryCount] at ih
      constructor
      · omega
      · intro _
        cases n with
        | zero => omega
        | succ n => have := ih.2 (by omega); omega
    · simp [hb] at h

/-- After `n` admitted reissues from a fresh request the count is `n`, and a
fresh request admits at most `maxRetries` of them in a chain. -/
theorem reissueChain_bounded (maxRetries n result : Nat)
    (h : reissueChain successorRetryCount maxRetries n 0 = some result) :
    result = n ∧ n ≤ maxRetries := by
  have := reissueChain_count maxRetries n 0 result h
  constructor
  · omega
  · cases n with
    | zero => omega
    | succ n => have := this.2 (by omega); omega

/-- The bound is reached: every chain within the budget is admitted. -/
theorem reissueChain_admits_within_budget (maxRetries : Nat) :
    ∀ (n count : Nat), count + n ≤ maxRetries →
      reissueChain successorRetryCount maxRetries n count = some (count + n)
  | 0, count, _ => by simp [reissueChain]
  | n + 1, count, h => by
    have hb : budgetRemains count maxRetries = true := by
      simp [budgetRemains]; omega
    simp only [reissueChain, reissueStep, hb, if_true, Option.some_bind, successorRetryCount]
    rw [reissueChain_admits_within_budget maxRetries n (count + 1) (by omega)]
    congr 1
    omega

/-- Once the count reaches `maxRetries`, no reissue of either kind is admitted. -/
theorem exhausted_budget_refuses (kind : ReissueKind) (parent : ReissueParent)
    (interactive deadlineOpen latest : Bool) (count maxRetries : Nat)
    (h : maxRetries ≤ count) :
    reissueDecision kind parent interactive (budgetRemains count maxRetries)
      deadlineOpen latest ≠ .admit := by
  intro hadmit
  have := reissueDecision_admit_budget _ _ _ _ _ _ hadmit
  simp [budgetRemains] at this
  omega

/-- The divergent rule the CLI's `request resend` followed: every successor is
written with `retry_count: 0`. -/
def resetRetryCount (_parent : Nat) : Nat := 0

/-- Under the reset rule any positive budget admits every chain, however long,
so resends never exhaust it. `successorRetryCount` is the rule that bounds the
chain (`reissueChain_bounded`). -/
theorem reset_rule_never_refuses (maxRetries : Nat) (h : 0 < maxRetries) :
    ∀ n : Nat, reissueChain resetRetryCount maxRetries n 0 = some 0
  | 0 => rfl
  | n + 1 => by
    have hb : budgetRemains 0 maxRetries = true := by simp [budgetRemains, h]
    simp only [reissueChain, reissueStep, hb, if_true, Option.some_bind, resetRetryCount]
    exact reset_rule_never_refuses maxRetries h n

end SessionOperations
