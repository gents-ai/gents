import Proofs.Basic

/-!
# Durable Goal Controller

The model covers the durable status machine, the three-turn blocked audit,
session-idle continuation gate, infrastructure outcome matrix, exactly-once
parent latch, and one-shot budget wrap-up.
-/

namespace Goals

inductive Status where
  | active
  | paused
  | blocked
  | usageLimited
  | budgetLimited
  | complete
  deriving DecidableEq, Repr

namespace Status

def toDefraDB : Status → String
  | .active => "active"
  | .paused => "paused"
  | .blocked => "blocked"
  | .usageLimited => "usage_limited"
  | .budgetLimited => "budget_limited"
  | .complete => "complete"

def fromDefraDB? : String → Option Status
  | "active" => some .active
  | "paused" => some .paused
  | "blocked" => some .blocked
  | "usage_limited" => some .usageLimited
  | "budget_limited" => some .budgetLimited
  | "complete" => some .complete
  | _ => none

theorem fromDefraDB_toDefraDB (status : Status) :
    fromDefraDB? status.toDefraDB = some status := by
  cases status <;> rfl

end Status

structure State where
  status : Status
  blockedAudits : Nat
  wrapupRequested : Bool
  wrapupCompleted : Bool
  deriving DecidableEq, Repr

inductive Action where
  | pause
  | resume
  | complete
  | blockedAudit (sameCondition : Bool)
  | budgetExhausted
  | wrapupFinished
  deriving DecidableEq, Repr

def nextBlockedAudits (current : Nat) (sameCondition : Bool) : Nat :=
  if sameCondition then current + 1 else 1

def step? (state : State) : Action → Option State
  | .pause =>
      if state.status = .active then
        some { state with status := .paused }
      else none
  | .resume =>
      if state.status = .paused ∨ state.status = .blocked ∨
          state.status = .usageLimited then
        some { state with status := .active, blockedAudits := 0 }
      else none
  | .complete =>
      if state.status = .complete then none
      else some { state with status := .complete, wrapupCompleted := true }
  | .blockedAudit same =>
      if state.status = .active then
        let audits := nextBlockedAudits state.blockedAudits same
        some { state with
          status := if audits ≥ 3 then .blocked else .active
          blockedAudits := audits }
      else none
  | .budgetExhausted =>
      if state.status = .active ∧ !state.wrapupRequested then
        some { state with status := .budgetLimited, wrapupRequested := true }
      else none
  | .wrapupFinished =>
      if state.status = .budgetLimited ∧ state.wrapupRequested ∧
          !state.wrapupCompleted then
        some { state with wrapupCompleted := true }
      else none

theorem blocked_requires_three_audits
    (state post : State) (same : Bool)
    (h : step? state (.blockedAudit same) = some post)
    (hblocked : post.status = .blocked) :
    post.blockedAudits ≥ 3 := by
  simp only [step?] at h
  split at h
  · cases h
    simp only
    split at hblocked
    · omega
    · simp at hblocked
  · simp at h

theorem blocked_audit_below_threshold_stays_active
    (state post : State) (same : Bool)
    (h : step? state (.blockedAudit same) = some post)
    (hbelow : post.blockedAudits < 3) :
    post.status = .active := by
  simp only [step?] at h
  split at h
  · cases h
    change nextBlockedAudits state.blockedAudits same < 3 at hbelow
    simp only
    split <;> rename_i hthreshold
    · exact False.elim ((Nat.not_lt_of_ge hthreshold) hbelow)
    · rfl
  · simp at h

theorem budget_transition_sets_wrapup_latch
    (state post : State)
    (h : step? state .budgetExhausted = some post) :
    post.status = .budgetLimited ∧ post.wrapupRequested = true := by
  simp only [step?] at h
  split at h
  · cases h
    exact ⟨rfl, rfl⟩
  · simp at h

inductive RequestTerminal where
  | completed
  | failed
  | dead
  | interrupted
  | superseded
  deriving DecidableEq, Repr

inductive Decision where
  | none
  | continue
  | retry
  | pause
  | wrapup
  deriving DecidableEq, Repr

def decide
    (status : Status)
    (terminal : RequestTerminal)
    (sessionIdle childExists budgetReached hasActivity : Bool)
    (infrastructureRetries : Nat)
    (wrapupRequested wrapupCompleted : Bool) : Decision :=
  if !sessionIdle ∨ childExists then .none
  else match status with
  | .active =>
      match terminal with
      | .interrupted | .superseded => .pause
      | .failed | .dead => if infrastructureRetries < 2 then .retry else .pause
      | .completed =>
          if !hasActivity then .pause
          else if budgetReached then .wrapup else .continue
  | .budgetLimited =>
      if wrapupRequested ∧ !wrapupCompleted then .wrapup else .none
  | .paused | .blocked | .usageLimited | .complete => .none

theorem busy_session_never_continues
    (status : Status) (terminal : RequestTerminal) (child budget activity : Bool)
    (retries : Nat) (requested completed : Bool) :
    decide status terminal false child budget activity retries requested completed = .none := by
  simp [decide]

theorem existing_child_never_duplicates
    (status : Status) (terminal : RequestTerminal) (idle budget activity : Bool)
    (retries : Nat) (requested completed : Bool) :
    decide status terminal idle true budget activity retries requested completed = .none := by
  cases idle <;> simp [decide]

theorem inactive_goal_never_continues
    (status : Status) (terminal : RequestTerminal) (idle child budget activity : Bool)
    (retries : Nat) (requested completed : Bool)
    (hinactive : status ≠ .active ∧ status ≠ .budgetLimited) :
    decide status terminal idle child budget activity retries requested completed = .none := by
  rcases hinactive with ⟨ha, hb⟩
  cases status <;> simp_all [decide]

theorem completed_wrapup_is_one_shot
    (terminal : RequestTerminal) (activity : Bool) (retries : Nat) :
    decide .budgetLimited terminal true false true activity retries true true = .none := by
  simp [decide]

def chargedTokens (prompt output cachedRead : Nat) : Nat :=
  prompt - cachedRead + output

theorem cached_reads_are_not_charged
    (prompt output cachedRead : Nat)
    (h : cachedRead ≤ prompt) :
    chargedTokens prompt output cachedRead + cachedRead = prompt + output := by
  simp only [chargedTokens]
  omega

end Goals
