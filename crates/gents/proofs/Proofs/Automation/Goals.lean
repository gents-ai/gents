import Proofs.Shared.Basic

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

/-- A Goal's durable state. `blocked` and `budgetLimited` are pauses that
await the Goal's user: `blocked` after the working Agent reports it cannot go
on (or a person blocks it), `budgetLimited` after its token budget is
reached. The status is the reason the Goal paused; no separate reason field
restates it. The Agent's blocked reason is kept on the Goal for the user to
read and is never an input to a decision here. -/
structure State where
  status : Status
  wrapupRequested : Bool
  wrapupCompleted : Bool
  deriving DecidableEq, Repr

inductive Action where
  | pause
  | resume
  /-- Explicit removal ends autonomous continuation without asserting success.
  It preserves the current request and user queue; their existing owners retain
  cancellation authority. Historical goal state and accounting remain durable. -/
  | stop
  | complete
  /-- One blocked report: the working Agent's `update_goal` with status
  `blocked`, or a person's block. It carries no reason text, so how a reason
  is worded cannot change what happens. -/
  | block
  | usageLimit
  | budgetExhausted
  | wrapupFinished
  | wrapupAbandoned
  deriving DecidableEq, Repr

/-- Automatic re-prompt requests after a Failed or Dead terminal when the Goal's
Task declares no limit. Goal infrastructure retries create new requests and have
a separate budget from provider transport retries; the limit prevents an
unattended Goal from repeatedly consuming resources when the underlying failure
persists. A Goal pins the limit it was created with. -/
def maxInfrastructureRetries : Nat := 2

/-- A Goal's budget is reached when its charged tokens (`chargedTokens`: prompt
tokens not read from cache, plus output) reach its token budget. A Goal's
budget is exactly the one set for it: by the person who creates or edits it,
by the Task that fires it, or by the Agent for its own Goal. No budget is
supplied by default (owner's decision), so a Goal with no budget, never set or
lifted, has no token ceiling. Goal usage counts the Goal session's normal and
access-review requests and excludes title work, so this reads only what goal
usage already counts. -/
def budgetReached (tokenBudget : Option Nat) (tokensUsed : Nat) : Bool :=
  tokenBudget.any (· ≤ tokensUsed)

theorem unset_budget_is_never_reached (tokensUsed : Nat) :
    budgetReached none tokensUsed = false := rfl

theorem set_budget_is_reached_exactly_at_it (budget tokensUsed : Nat) :
    budgetReached (some budget) tokensUsed = decide (budget ≤ tokensUsed) := rfl

def step? (state : State) (action : Action) : Option State :=
  match action with
  | .pause =>
      if state.status = .active ∨ state.status = .budgetLimited then
        some { state with status := .paused }
      else none
  | .resume =>
      if state.status = .paused ∨ state.status = .blocked ∨
          state.status = .usageLimited ∨
          (state.status = .budgetLimited ∧ state.wrapupCompleted) then
        let owed := state.wrapupRequested && !state.wrapupCompleted
        some { status := if owed then .budgetLimited else .active
               wrapupRequested := owed
               wrapupCompleted := false }
      else none
  | .complete =>
      if state.status = .complete ∨ state.status = .stopped then none
      else some { state with status := .complete, wrapupCompleted := true }
  | .stop =>
      if state.status = .complete ∨ state.status = .stopped then none
      else some { state with status := .stopped }
  | .block =>
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

theorem stop_preserves_history (state post : State)
    (h : step? state .stop = some post) :
    post.status = .stopped ∧
      post.wrapupRequested = state.wrapupRequested ∧
      post.wrapupCompleted = state.wrapupCompleted := by
  simp only [step?] at h
  split at h
  · contradiction
  · cases h
    exact ⟨rfl, rfl, rfl⟩

theorem stopped_rejects_later_transitions (state : State) (action : Action)
    (h : state.status = .stopped) : step? state action = none := by
  cases action <;> simp [step?, h]

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
    simp
  · simp at h_pause

/-- One blocked report pauses an active Goal awaiting its user. -/
theorem one_blocked_report_blocks (state : State) (h : state.status = .active) :
    (step? state .block).map (·.status) = some .blocked := by
  simp [step?, h]

/-- A paused Goal is never prevented from stopping: every Goal that has not
ended can be stopped, whatever paused it. -/
theorem pause_never_prevents_stop (state : State)
    (h : state.status ≠ .complete ∧ state.status ≠ .stopped) :
    (step? state .stop).isSome := by
  simp [step?, h.1, h.2]

/-- Only a resume takes a blocked Goal back to work; a resume is a user's
action (their next message, or the resume operation), and the runtime's
continuation decision (`decide`) never asks for one. -/
theorem only_resume_leaves_blocked_for_work (state post : State) (action : Action)
    (hblocked : state.status = .blocked) (h : step? state action = some post)
    (hwork : post.status = .active ∨ post.status = .budgetLimited) :
    action = .resume := by
  cases action with
  | resume => rfl
  | stop | complete =>
      simp only [step?] at h
      split at h
      · simp at h
      · cases h
        simp at hwork
  | _ => simp [step?, hblocked] at h

theorem budget_transition_sets_wrapup_latch
    (state post : State)
    (h : step? state .budgetExhausted = some post) :
    post.status = .budgetLimited ∧ post.wrapupRequested = true := by
  simp only [step?] at h
  split at h
  · cases h
    simp_all
  · simp at h

/-- Resuming a Goal whose wrap-up finished starts a fresh budget run: the
latch is cleared, so reaching the budget again owes one more wrap-up. -/
theorem resume_after_wrapup_clears_the_latch (state post : State)
    (hcompleted : state.wrapupCompleted = true)
    (h : step? state .resume = some post) :
    post.status = .active ∧ post.wrapupRequested = false ∧ post.wrapupCompleted = false := by
  simp only [step?] at h
  split at h
  · cases h
    simp [hcompleted]
  · simp at h

/-- A user's message into a blocked Goal's session answers the block: the
Goal resumes and the message's turn is the Goal's next turn. A user's message
is a request admitted from a client that nothing caused (no trigger, parent
request or parent tool call), so an Agent's session message does not resume
it. A Goal paused
for any other reason waits for its own resume: a halt's pause for the session
resume, a budget pause for the budget to be raised or lifted. -/
def resumeOnMessage (state : State) : Option State :=
  if state.status = .blocked then step? state .resume else none

theorem message_resumes_only_blocked (state post : State)
    (h : resumeOnMessage state = some post) : state.status = .blocked := by
  unfold resumeOnMessage at h
  split at h
  · assumption
  · simp at h

theorem message_resumes_blocked (state : State) (h : state.status = .blocked) :
    (resumeOnMessage state).isSome := by
  simp [resumeOnMessage, step?, h]

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

/-- `maxRetries` is the Goal's pinned infrastructure retry limit
(`maxInfrastructureRetries` when its Task declared none). -/
def decide
    (status : Status)
    (terminal : RequestTerminal)
    (sessionIdle childExists budgetReached hasActivity requestIsWrapup : Bool)
    (infrastructureRetries : Nat)
    (wrapupRequested wrapupCompleted : Bool)
    (maxRetries : Nat := maxInfrastructureRetries) : Decision :=
  if !sessionIdle ∨ childExists then .none
  else match status with
  | .active =>
      match terminal with
      | .interrupted | .superseded => .pause
      | .failed | .dead =>
          if budgetReached then .wrapup
          else if infrastructureRetries < maxRetries then .retry else .pause
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
          if infrastructureRetries < maxRetries then .retry else .abandonWrapup
  | .paused | .blocked | .usageLimited | .complete | .stopped => .none

theorem stopped_never_continues
    (terminal : RequestTerminal) (idle child budget activity wrapup : Bool)
    (retries maxRetries : Nat) (requested completed : Bool) :
    decide .stopped terminal idle child budget activity wrapup retries requested completed maxRetries = .none := by
  cases idle <;> cases child <;> simp [decide]

theorem exhausted_active_never_retries
    (terminal : RequestTerminal) (idle child activity wrapup : Bool)
    (retries maxRetries : Nat) (requested completed : Bool) :
    decide .active terminal idle child true activity wrapup retries requested completed maxRetries ≠ .retry := by
  cases terminal <;> cases idle <;> cases child <;> cases activity <;> simp [decide]

theorem exhausted_failure_requests_wrapup
    (terminal : RequestTerminal) (activity wrapup : Bool)
    (retries maxRetries : Nat) (requested completed : Bool)
    (hfailure : terminal = .failed ∨ terminal = .dead) :
    decide .active terminal true false true activity wrapup retries requested completed maxRetries = .wrapup := by
  rcases hfailure with rfl | rfl <;> simp [decide]

theorem busy_session_never_continues
    (status : Status) (terminal : RequestTerminal) (child budget activity wrapup : Bool)
    (retries maxRetries : Nat) (requested completed : Bool) :
    decide status terminal false child budget activity wrapup retries requested completed maxRetries = .none := by
  simp [decide]

theorem existing_child_never_duplicates
    (status : Status) (terminal : RequestTerminal) (idle budget activity wrapup : Bool)
    (retries maxRetries : Nat) (requested completed : Bool) :
    decide status terminal idle true budget activity wrapup retries requested completed maxRetries = .none := by
  cases idle <;> simp [decide]

theorem inactive_goal_never_continues
    (status : Status) (terminal : RequestTerminal) (idle child budget activity wrapup : Bool)
    (retries maxRetries : Nat) (requested completed : Bool)
    (hinactive : status ≠ .active ∧ status ≠ .budgetLimited) :
    decide status terminal idle child budget activity wrapup retries requested completed maxRetries = .none := by
  rcases hinactive with ⟨ha, hb⟩
  cases status <;> simp_all [decide]

/-- After one blocked report the Goal issues no continuation turn, whatever
its last request's terminal and however the session stands. -/
theorem blocked_report_stops_continuation (state post : State)
    (h : step? state .block = some post) (terminal : RequestTerminal)
    (idle child budget activity wrapup : Bool) (retries maxRetries : Nat) :
    decide post.status terminal idle child budget activity wrapup retries
      post.wrapupRequested post.wrapupCompleted maxRetries = .none := by
  simp only [step?] at h
  split at h
  · cases h
    cases idle <;> cases child <;> simp [decide]
  · simp at h

/-- A Goal whose budget is reached is never continued: its last turn over the
budget is followed at most by the one wrap-up turn (and that turn's bounded
infrastructure retries), never by ordinary work. -/
theorem over_budget_never_continues (status : Status) (terminal : RequestTerminal)
    (idle child activity wrapup : Bool) (retries maxRetries : Nat) (requested completed : Bool) :
    decide status terminal idle child true activity wrapup retries requested completed
      maxRetries ≠ .continue := by
  unfold decide
  repeat' split
  all_goals simp_all

theorem completed_wrapup_is_one_shot
    (terminal : RequestTerminal) (activity : Bool) (retries maxRetries : Nat) :
    decide .budgetLimited terminal true false true activity true retries true true maxRetries = .none := by
  simp [decide]

/-- A completed wrap-up turn ends the wrap-up before its `wrapupCompleted`
latch is written. The Goal source writes that latch in a later transaction of
the step that observes the completed turn (and only while it is polled), so a
reconciler that reads the Goal in between still sees `wrapupCompleted = false`;
the decision must not depend on that write. `tla/GoalWrapup.tla`
(`MCGoalWrapupNoWrapupGuard`) shows the second wrap-up when it does (#3810). -/
theorem completed_wrapup_child_never_repeats
    (budget activity : Bool) (retries maxRetries : Nat) (requested completed : Bool) :
    decide .budgetLimited .completed true false budget activity true retries
      requested completed maxRetries = .none := by
  cases requested <;> cases completed <;> simp [decide]

/-- One exhaustion yields at most one wrap-up decision: either the over-budget
turn of an active Goal, or a budget-limited Goal whose latest request is not
the wrap-up and whose wrap-up is still owed. Together with the claim CAS and
the keyed child of `GoalAutomation.ClaimedPublication`, this is the
`OneWrapupPerParent` and `NeverAfterCompletedWrapup` premise of
`tla/GoalWrapup.tla`. -/
theorem wrapup_requires_owed_exhaustion
    (status : Status) (terminal : RequestTerminal)
    (idle child budget activity requestWrapup : Bool) (retries maxRetries : Nat)
    (requested completed : Bool)
    (h : decide status terminal idle child budget activity requestWrapup retries
      requested completed maxRetries = .wrapup) :
    idle = true ∧ child = false ∧
      ((status = .active ∧ budget = true) ∨
        (status = .budgetLimited ∧ requested = true ∧ completed = false ∧
          requestWrapup = false)) := by
  unfold decide at h
  repeat' (split at h)
  all_goals (cases idle <;> cases child <;> simp_all)

/-- The budget latch is one-shot: an exhausted Goal cannot be exhausted again,
and a finished wrap-up cannot finish again. -/
theorem budget_exhaustion_latches_once (state post : State)
    (h : step? state .budgetExhausted = some post) :
    step? post .budgetExhausted = none := by
  simp only [step?] at h
  split at h
  · cases h
    simp [step?]
  · simp at h

theorem wrapup_finishes_once (state post : State)
    (h : step? state .wrapupFinished = some post) :
    step? post .wrapupFinished = none := by
  simp only [step?] at h
  split at h
  · cases h
    simp [step?]
  · simp at h

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

theorem interrupted_wrapup_does_not_retry
    (activity requestWrapup : Bool) (retries maxRetries : Nat) :
    (decide Status.budgetLimited RequestTerminal.interrupted true false true activity
      requestWrapup retries true false maxRetries) ≠ Decision.retry := by
  cases requestWrapup <;> simp [Goals.decide]

end Goals
