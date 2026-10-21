import Proofs.Basic

namespace Goals

inductive Status where
  | active
  | paused
  | blocked
  | usageLimited
  | budgetLimited
  | complete
  | stopped
  deriving DecidableEq, Repr

namespace Status

def toDefraDB : Status → String
  | .active => "active"
  | .paused => "paused"
  | .blocked => "blocked"
  | .usageLimited => "usage_limited"
  | .budgetLimited => "budget_limited"
  | .complete => "complete"
  | .stopped => "stopped"

def fromDefraDB? : String → Option Status
  | "active" => some .active
  | "paused" => some .paused
  | "blocked" => some .blocked
  | "usage_limited" => some .usageLimited
  | "budget_limited" => some .budgetLimited
  | "complete" => some .complete
  | "stopped" => some .stopped
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

inductive AuditObservation where
  | sameRequest
  | sameCondition
  | newCondition
  deriving DecidableEq, Repr

inductive Action where
  | pause
  | resume
  /-- Explicit removal ends autonomous continuation without asserting success.
  It preserves the current request and user queue; their existing owners retain
  cancellation authority. Historical goal state and accounting remain durable. -/
  | stop
  | complete
  | blockedAudit (observation : AuditObservation)
  | operatorBlock
  | usageLimit
  | budgetExhausted
  | wrapupFinished
  | wrapupAbandoned
  | cleanTurn
  deriving DecidableEq, Repr

def nextBlockedAudits (current : Nat) : AuditObservation → Nat
  | .sameRequest => current
  | .sameCondition => current + 1
  | .newCondition => 1

def step? (state : State) : Action → Option State
  | .pause =>
      if state.status = .active ∨ state.status = .budgetLimited then
        some { state with status := .paused }
      else none
  | .resume =>
      if state.status = .paused ∨ state.status = .blocked ∨
          state.status = .usageLimited then
        some { state with
          status := if state.wrapupRequested ∧ !state.wrapupCompleted
            then .budgetLimited else .active
          blockedAudits := 0 }
      else none
  | .complete =>
      if state.status = .complete ∨ state.status = .stopped then none
      else some { state with status := .complete, wrapupCompleted := true }
  | .stop =>
      if state.status = .complete ∨ state.status = .stopped then none
      else some { state with status := .stopped }
  | .blockedAudit observation =>
      if state.status = .active then
        let audits := nextBlockedAudits state.blockedAudits observation
        some { state with
          status := if audits ≥ 3 then .blocked else .active
          blockedAudits := audits }
      else none
  | .operatorBlock =>
      if state.status = .active then
        some { state with status := .blocked }
      else none
  | .usageLimit =>
      if state.status = .active then
        some { state with status := .usageLimited }
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
  | .wrapupAbandoned =>
      if state.status = .budgetLimited ∧ state.wrapupRequested ∧
          !state.wrapupCompleted then
        some { state with wrapupCompleted := true }
      else none
  | .cleanTurn =>
      if state.status = .active then
        some { state with blockedAudits := 0 }
      else none

theorem stop_preserves_history (state post : State)
    (h : step? state .stop = some post) :
    post.status = .stopped ∧ post.blockedAudits = state.blockedAudits ∧
      post.wrapupRequested = state.wrapupRequested ∧
      post.wrapupCompleted = state.wrapupCompleted := by
  simp only [step?] at h
  split at h
  · contradiction
  · cases h
    exact ⟨rfl, rfl, rfl, rfl⟩

theorem stopped_rejects_later_transitions (state : State) (action : Action)
    (h : state.status = .stopped) : step? state action = none := by
  cases action <;> simp [step?, h]

theorem interrupted_wrapup_does_not_retry
    (activity requestWrapup : Bool) (retries : Nat) :
    decide .budgetLimited .interrupted true false true activity requestWrapup retries
      true false ≠ .retry := by
  simp [decide]

theorem pause_resume_preserves_pending_wrapup (state paused resumed : State)
    (h_requested : state.wrapupRequested = true)
    (h_incomplete : state.wrapupCompleted = false)
    (h_pause : step? state .pause = some paused)
    (h_resume : step? paused .resume = some resumed) :
    resumed.status = .budgetLimited ∧ resumed.wrapupRequested = true ∧
      resumed.wrapupCompleted = false := by
  simp only [step?] at h_pause h_resume
  split at h_pause
  · cases h_pause
    simp [h_requested, h_incomplete] at h_resume
    cases h_resume
    simp [h_requested, h_incomplete]
  · simp at h_pause

theorem blocked_requires_three_audits
    (state post : State) (observation : AuditObservation)
    (h : step? state (.blockedAudit observation) = some post)
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
    (state post : State) (observation : AuditObservation)
    (h : step? state (.blockedAudit observation) = some post)
    (hbelow : post.blockedAudits < 3) :
    post.status = .active := by
  simp only [step?] at h
  split at h
  · cases h
    change nextBlockedAudits state.blockedAudits observation < 3 at hbelow
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
    simp_all
  · simp at h

theorem resume_resets_blocked_audits
    (state post : State)
    (h : step? state .resume = some post) :
    post.status = .active ∧ post.blockedAudits = 0 := by
  simp only [step?] at h
  split at h
  · cases h
    exact ⟨rfl, rfl⟩
  · simp at h

theorem same_request_does_not_increment
    (state post : State)
    (h : step? state (.blockedAudit .sameRequest) = some post) :
    post.blockedAudits = state.blockedAudits := by
  simp only [step?] at h
  split at h
  · cases h
    rfl
  · simp at h

theorem clean_turn_resets_blocked_audits
    (state post : State)
    (h : step? state .cleanTurn = some post) :
    post.status = .active ∧ post.blockedAudits = 0 := by
  simp only [step?] at h
  split at h
  · cases h
    simp_all
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
  | abandonWrapup
  deriving DecidableEq, Repr

def decide
    (status : Status)
    (terminal : RequestTerminal)
    (sessionIdle childExists budgetReached hasActivity requestIsWrapup : Bool)
    (infrastructureRetries : Nat)
    (wrapupRequested wrapupCompleted : Bool) : Decision :=
  if !sessionIdle ∨ childExists then .none
  else match status with
  | .active =>
      match terminal with
      | .interrupted | .superseded => .pause
      | .failed | .dead =>
          if budgetReached then .wrapup
          else if infrastructureRetries < 2 then .retry else .pause
      | .completed =>
          if !hasActivity then .pause
          else if budgetReached then .wrapup else .continue
  | .budgetLimited =>
      if !wrapupRequested ∨ wrapupCompleted then .none
      else if !requestIsWrapup then .wrapup
      else match terminal with
      | .completed => .none
      | .interrupted | .superseded => .pause
      | .failed | .dead =>
          if infrastructureRetries < 2 then .retry else .abandonWrapup
  | .paused | .blocked | .usageLimited | .complete | .stopped => .none

theorem stopped_never_continues
    (terminal : RequestTerminal) (idle child budget activity wrapup : Bool)
    (retries : Nat) (requested completed : Bool) :
    decide .stopped terminal idle child budget activity wrapup retries requested completed = .none := by
  cases idle <;> cases child <;> simp [decide]

theorem exhausted_active_never_retries
    (terminal : RequestTerminal) (idle child activity wrapup : Bool)
    (retries : Nat) (requested completed : Bool) :
    decide .active terminal idle child true activity wrapup retries requested completed ≠ .retry := by
  cases terminal <;> cases idle <;> cases child <;> cases activity <;> simp [decide]

theorem exhausted_failure_requests_wrapup
    (terminal : RequestTerminal) (activity wrapup : Bool)
    (retries : Nat) (requested completed : Bool)
    (hfailure : terminal = .failed ∨ terminal = .dead) :
    decide .active terminal true false true activity wrapup retries requested completed = .wrapup := by
  rcases hfailure with rfl | rfl <;> simp [decide]

theorem busy_session_never_continues
    (status : Status) (terminal : RequestTerminal) (child budget activity wrapup : Bool)
    (retries : Nat) (requested completed : Bool) :
    decide status terminal false child budget activity wrapup retries requested completed = .none := by
  simp [decide]

theorem existing_child_never_duplicates
    (status : Status) (terminal : RequestTerminal) (idle budget activity wrapup : Bool)
    (retries : Nat) (requested completed : Bool) :
    decide status terminal idle true budget activity wrapup retries requested completed = .none := by
  cases idle <;> simp [decide]

theorem inactive_goal_never_continues
    (status : Status) (terminal : RequestTerminal) (idle child budget activity wrapup : Bool)
    (retries : Nat) (requested completed : Bool)
    (hinactive : status ≠ .active ∧ status ≠ .budgetLimited) :
    decide status terminal idle child budget activity wrapup retries requested completed = .none := by
  rcases hinactive with ⟨ha, hb⟩
  cases status <;> simp_all [decide]

theorem completed_wrapup_is_one_shot
    (terminal : RequestTerminal) (activity : Bool) (retries : Nat) :
    decide .budgetLimited terminal true false true activity true retries true true = .none := by
  simp [decide]

def chargedTokens (prompt output cachedRead : Nat) : Nat :=
  prompt - cachedRead + output

theorem cached_reads_are_not_charged
    (prompt output cachedRead : Nat)
    (h : cachedRead ≤ prompt) :
    chargedTokens prompt output cachedRead + cachedRead = prompt + output := by
  simp only [chargedTokens]
  omega

/-- A Task assignment binds its physical root when it wins request claim.
The existing authenticated continuation ancestry owner supplies observedRoot;
this predicate grants no signature or ancestry authority. A missing assignment
root retains ordinary tool-created Goal semantics. Rebinding leaves old signed
ancestry intact but excludes it from current heads and queued continuation claims. -/
def assignmentAllows (assignmentRoot observedRoot : Option String) : Bool :=
  match assignmentRoot with
  | none => true
  | some root => observedRoot == some root

theorem unbound_assignment_preserves_heads (observed : Option String) :
    assignmentAllows none observed = true := rfl

theorem bound_assignment_rejects_missing (root : String) :
    assignmentAllows (some root) none = false := by simp [assignmentAllows]

theorem reassignment_rejects_old_root (old next : String) (different : old ≠ next) :
    assignmentAllows (some next) (some old) = false := by
  simp [assignmentAllows, different]

end Goals
