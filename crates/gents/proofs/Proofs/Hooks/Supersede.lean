import Proofs.Hooks.Claim

namespace Hooks

/-- One pending request a plugin asks core to supersede, with the request
that supersedes it and the reason core records. -/
structure StaleRequest where
  doc : String
  request : String
  successor : String
  successorDoc : String
  reason : String
  deriving DecidableEq, Repr

/-- `request.pending.supersede`: core supersedes each named pending request in
order through `retire`, the pending-supersede path `request.claim.gate`'s
`retire` verdict uses (with its `request.terminal.stage`), or performs nothing.

Decision 2026-11-11, maintainer-approved: AgentRequest rows are core state, so
retiring the pending Goal continuations a new Task assignment makes stale is a
core operation the Goals plugin requests with the stale list as data, not a
write it performs. The list is bounded by the requesting plugin's manifest
bound. -/
def supersedeAll {Core : Type} (retire : StaleRequest → Core → Option Core) (bound : Nat)
    (stale : List StaleRequest) (core : Core) : Option Core :=
  if stale.length ≤ bound then stale.foldlM (fun current entry => retire entry current) core
  else none

/-- The chain of pending retirements `supersedeAll` performed. -/
inductive Retired {Core : Type} (retire : StaleRequest → Core → Option Core) :
    List StaleRequest → Core → Core → Prop where
  | nil (core : Core) : Retired retire [] core core
  | cons {first mid last : Core} (entry : StaleRequest) (rest : List StaleRequest)
      (step : retire entry first = some mid) (tail : Retired retire rest mid last) :
      Retired retire (entry :: rest) first last

variable {Core : Type}

theorem supersede_over_bound_performs_nothing (retire : StaleRequest → Core → Option Core)
    (bound : Nat) (stale : List StaleRequest) (core : Core) (over : bound < stale.length) :
    supersedeAll retire bound stale core = none := by
  simp [supersedeAll, Nat.not_le.mpr over]

private theorem fold_retired (retire : StaleRequest → Core → Option Core) (stale : List StaleRequest)
    (core next : Core)
    (performed : stale.foldlM (fun current entry => retire entry current) core = some next) :
    Retired retire stale core next := by
  induction stale generalizing core with
  | nil =>
    simp only [List.foldlM_nil, pure, Option.some.injEq] at performed
    subst performed
    exact .nil core
  | cons entry rest ih =>
    simp only [List.foldlM_cons, bind, Option.bind] at performed
    cases step : retire entry core with
    | none => simp [step] at performed
    | some mid =>
      simp only [step] at performed
      exact .cons entry rest step (ih mid performed)

/-- Every request the operation superseded went through `retire`, in the
order named; nothing else moved core state. -/
theorem superseded_requests_are_pending_retires (retire : StaleRequest → Core → Option Core)
    (bound : Nat) (stale : List StaleRequest) (core next : Core)
    (performed : supersedeAll retire bound stale core = some next) :
    Retired retire stale core next := by
  unfold supersedeAll at performed
  split at performed
  · exact fold_retired retire stale core next performed
  · cases performed

end Hooks
