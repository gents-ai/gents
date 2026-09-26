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
  requester : Option String
  deriving DecidableEq, Repr

inductive BackgroundState where
  | pending | running | terminal
  deriving DecidableEq, Repr

inductive BackgroundOrigin where
  | spawned (parentToolDoc : Nat) (stableKey : String)
  | nonSpawned
  deriving DecidableEq, Repr

structure BackgroundTool where
  docId : Nat
  origin : BackgroundOrigin
  handle : String
  owner : String
  session : String
  requester : Option String
  state : BackgroundState
  deriving DecidableEq, Repr

inductive WaitResult where
  | timedOutRunning | other | argumentError | settledDiagnostic | malformed
  deriving DecidableEq, Repr

/-- One row represents a physical accepted control and its canonical invocation
reply. The native projection must reject missing/ambiguous accepted arguments or
replies rather than synthesize these fields from untrusted message text. It
enumerates accepted RequestExecution header bindings, not mutable tool-name
row indexes, before re-reading exact physical rows.
`timedOutRunning` requires both status=running and reason=wait_timeout;
`other` includes valid terminal, caller-interrupted, and caller-deadline envelopes.
`argumentError` is a canonical control error without a handle-bearing envelope.
`settledDiagnostic` is any terminal control other than completed: failed,
timed-out, or cancelled, including a pending control that request
terminalization cancelled before it ever started and which therefore has no
invocation reply (`replied = false`). It can neither suspend nor invalidate
Goal publication. `terminal = false` under a terminal parent is the handoff
uncertainty of a foreground control still running when its turn failed; it
is invalid evidence, which stops automation visibly. -/
structure WaitControl where
  docId : Nat
  parentRequestDoc : Nat
  owner : String
  session : String
  requester : Option String
  acceptedTool : String
  acceptedHandle : String
  replyHandle : String
  reply : WaitResult
  terminal : Bool
  replied : Bool
  deriving DecidableEq, Repr

structure PublicationObservation where
  waits : List WaitControl
  /-- `none` means the target rows were read but could not be decoded into an
  authoritative observation; it is not a proven empty scan. -/
  backgrounds : Option (List BackgroundTool)
  /-- A transaction read failed at the storage owner (conflict after its own
  retries, storage failure, or step timeout). Nothing about the evidence is
  known, so this is neither invalid nor absent. -/
  storageFailed : Bool
  deriving DecidableEq, Repr

inductive WaitAssessment where
  | absent | pending | invalid | unavailable
  deriving DecidableEq, Repr

def matchingWait (r : ClaimedRequest) (w : WaitControl) : Bool :=
  w.parentRequestDoc == r.binding.predecessorDoc &&
    w.owner == r.binding.owner && w.session == r.binding.session &&
    w.requester == r.requester && w.acceptedTool == "wait_process"

def targetCandidate (r : ClaimedRequest) (w : WaitControl)
    (b : BackgroundTool) : Bool :=
  b.owner == r.binding.owner && b.session == r.binding.session &&
    b.requester == r.requester && b.handle == w.acceptedHandle

def validBackgroundOrigin (b : BackgroundTool) : Bool :=
  match b.origin with
  | .spawned parent key =>
      b.handle == ("spawned:" ++ toString parent) &&
      key == (toString parent ++ ":spawned-background")
  | .nonSpawned => false

def matchingBackground (r : ClaimedRequest) (w : WaitControl)
    (b : BackgroundTool) : Bool :=
  targetCandidate r w b && validBackgroundOrigin b

/-- Evidence that a relevant control cannot be interpreted. Settled
diagnostics are exempt before any reply requirement is applied. -/
def malformedControl (w : WaitControl) : Bool :=
  !w.terminal ||
    (w.reply != .settledDiagnostic &&
      (!w.replied || w.reply == .malformed ||
        (w.reply != .argumentError && w.replyHandle != w.acceptedHandle)))

/-- A completed bounded wait is intentional only while its exact physical
background generation is running. A malformed same-parent canonical receipt
blocks publication; an unrelated wait or a mere background launch does not.
The native projection must bind spawned origin to the accepted physical
spawn_process parent, not trust a row's claimed handle or key alone. Ordinary
direct background calls have no immediate invocation receipt, so no legal
terminal predecessor can leave one running; generic tools use spawned children. -/
def assessWait (r : ClaimedRequest) (o : PublicationObservation) : WaitAssessment :=
  let relevant := o.waits.filter (matchingWait r)
  if o.storageFailed then .unavailable
  else if relevant.any malformedControl then .invalid
  else match o.backgrounds with
  | none => if relevant.any (fun w => w.reply == .timedOutRunning) then .invalid else .absent
  | some backgrounds =>
      if relevant.any fun w =>
          w.reply == .timedOutRunning &&
          ((backgrounds.filter (targetCandidate r w)).length > 1 ||
            (backgrounds.filter (targetCandidate r w)).any (fun b => !validBackgroundOrigin b))
        then .invalid
      else if relevant.any fun w =>
          w.reply == .timedOutRunning &&
          (backgrounds.filter (matchingBackground r w)).any (·.state == .running) then .pending
      else .absent

/-- Temporary storage trouble is not evidence: publication makes no Goal write
and a later reconciliation retries, as the readiness gate lets Goals ride out
temporary trouble without an operator. Only uninterpretable evidence stops.
Fail-closed evidence must stop automatic continuation visibly rather than
be retried silently forever. It uses the existing Goal transitions: an active
Goal pauses; a budget-limited Goal abandons its wrap-up. Native records the
reason in `last_failure`; either transition leaves the automatic candidate
set, so the write cannot re-trigger itself, and operator resume clears the
reason and starts a new epoch, so no retry prompt reads it. -/
def stopForInvalidEvidence (g : Goals.State) : Option Goals.State :=
  if g.status = .active then Goals.step? g .pause else Goals.step? g .wrapupAbandoned

def stopGoal (s : Snapshot) : Snapshot :=
  match stopForInvalidEvidence s.goal with
  | some post => { s with goal := post }
  | none => s

@[simp] theorem stopGoal_claim (s : Snapshot) :
    (stopGoal s).sequence = s.sequence ∧
    (stopGoal s).lastContinuedFrom = s.lastContinuedFrom ∧
    (stopGoal s).latestRequest = s.latestRequest ∧
    (stopGoal s).children = s.children ∧
    (stopGoal s).tokensUsed = s.tokensUsed ∧
    (stopGoal s).tokenBudget = s.tokenBudget := by
  unfold stopGoal; split <;> simp

def publishClaimed (s : Snapshot) (r : ClaimedRequest)
    (o : PublicationObservation) (commit : Bool) :
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
      else if assessWait r o == .unavailable then (s, .unavailable)
      else if assessWait r o == .invalid then
        (if commit then stopGoal s else s, .invalidEvidence)
      else if assessWait r o == .pending then (s, .deferred)
      else if commit then
        ({ s with latestRequest := r.binding.child,
                  children := r.binding :: s.children }, .created)
      else (s, .rolledBack)

theorem pending_wait_cannot_create (s : Snapshot) (r : ClaimedRequest)
    (o : PublicationObservation) (commit : Bool)
    (h : assessWait r o = .pending) :
    (publishClaimed s r o commit).2 ≠ .created := by
  unfold publishClaimed at *
  repeat' split at *
  all_goals simp_all

theorem invalid_wait_cannot_create (s : Snapshot) (r : ClaimedRequest)
    (o : PublicationObservation) (commit : Bool)
    (h : assessWait r o = .invalid) :
    (publishClaimed s r o commit).2 ≠ .created := by
  unfold publishClaimed at *
  repeat' split at *
  all_goals simp_all

theorem publication_preserves_claim_and_budget (s : Snapshot) (r : ClaimedRequest)
    (o : PublicationObservation) (commit : Bool) :
    (publishClaimed s r o commit).1.sequence = s.sequence ∧
    (publishClaimed s r o commit).1.lastContinuedFrom = s.lastContinuedFrom ∧
    (publishClaimed s r o commit).1.tokensUsed = s.tokensUsed ∧
    (publishClaimed s r o commit).1.tokenBudget = s.tokenBudget := by
  unfold publishClaimed at *
  repeat' split at *
  all_goals simp_all

/-- Only fail-closed wait evidence may change the Goal, and only through
`stopForInvalidEvidence`. -/
theorem publication_preserves_goal_unless_invalid (s : Snapshot) (r : ClaimedRequest)
    (o : PublicationObservation) (commit : Bool)
    (h : (publishClaimed s r o commit).2 ≠ .invalidEvidence) :
    (publishClaimed s r o commit).1.goal = s.goal := by
  unfold publishClaimed at *
  repeat' split at *
  all_goals simp_all

theorem invalid_evidence_result (s : Snapshot) (r : ClaimedRequest)
    (o : PublicationObservation) (commit : Bool)
    (h : (publishClaimed s r o commit).2 = .invalidEvidence) :
    (publishClaimed s r o commit).1 = if commit then stopGoal s else s := by
  unfold publishClaimed at *
  repeat' split at *
  all_goals simp_all

/-- Invalid evidence on an active Goal pauses it when the transaction commits,
so the rejection is durable and visible instead of a silent rescan loop. -/
theorem invalid_evidence_pauses_active_goal (s : Snapshot) (r : ClaimedRequest)
    (o : PublicationObservation)
    (hactive : s.goal.status = .active)
    (h : (publishClaimed s r o true).2 = .invalidEvidence) :
    (publishClaimed s r o true).1.goal.status = .paused := by
  rw [invalid_evidence_result s r o true h]
  simp [stopGoal, stopForInvalidEvidence, Goals.step?, hactive]

/-- Anti-suppression: a deferral exists only because a matching accepted
wait_process timed out on a matching, correctly-originated, running target. -/
theorem pending_requires_matching_running_wait (r : ClaimedRequest)
    (o : PublicationObservation) (h : assessWait r o = .pending) :
    ∃ w ∈ o.waits, matchingWait r w = true ∧ w.reply = .timedOutRunning ∧
      ∃ backgrounds, o.backgrounds = some backgrounds ∧
        ∃ b ∈ backgrounds, matchingBackground r w b = true ∧ b.state = .running := by
  unfold assessWait at h
  dsimp only at h
  by_cases hs : o.storageFailed = true
  · rw [if_pos hs] at h; cases h
  rw [if_neg hs] at h
  by_cases hm : (o.waits.filter (matchingWait r)).any malformedControl = true
  · rw [if_pos hm] at h; cases h
  rw [if_neg hm] at h
  cases hb : o.backgrounds with
  | none =>
    rw [hb] at h
    dsimp only at h
    split at h <;> cases h
  | some backgrounds =>
    rw [hb] at h
    dsimp only at h
    split at h
    · cases h
    · split at h
      · rename_i hpend
        simp only [List.any_eq_true, List.mem_filter, Bool.and_eq_true, beq_iff_eq] at hpend
        obtain ⟨w, ⟨hw, hm⟩, hreply, b, ⟨hb', hmb⟩, hrun⟩ := hpend
        exact ⟨w, hw, hm, hreply, backgrounds, rfl, b, hb', hmb, hrun⟩
      · cases h

/-- A storage failure during the evidence reads never transitions the Goal
and never publishes; the claim stays for a later reconciliation. -/
theorem storage_failure_is_retryable_noop (s : Snapshot) (r : ClaimedRequest)
    (o : PublicationObservation) (commit : Bool) (h : o.storageFailed = true) :
    (publishClaimed s r o commit).1 = s ∧
      (publishClaimed s r o commit).2 ≠ .created ∧
      (publishClaimed s r o commit).2 ≠ .invalidEvidence := by
  have ha : assessWait r o = .unavailable := by simp [assessWait, h]
  unfold publishClaimed
  repeat' split
  all_goals simp_all

/-- A background launch without an accepted matching wait never defers. -/
theorem launch_without_wait_never_defers (r : ClaimedRequest)
    (o : PublicationObservation) (h : ∀ w ∈ o.waits, matchingWait r w = false) :
    assessWait r o ≠ .pending := by
  intro hp
  obtain ⟨w, hw, hm, _⟩ := pending_requires_matching_running_wait r o hp
  simp [h w hw] at hm

theorem discarded_claimed_publication_is_noop (s : Snapshot) (r : ClaimedRequest)
    (o : PublicationObservation) :
    (publishClaimed s r o false).1 = s := by
  unfold publishClaimed at *
  repeat' split at *
  all_goals simp_all

theorem created_requires_current_claim (s : Snapshot) (r : ClaimedRequest)
    (o : PublicationObservation) (commit : Bool)
    (h : (publishClaimed s r o commit).2 = .created) :
    s.goal.status = r.expectedStatus ∧ s.sequence = r.expectedSequence ∧
    s.lastContinuedFrom = r.expectedLastContinuedFrom ∧
    (s.goal.status = .active ∨ s.goal.status = .budgetLimited) ∧
    (publishClaimed s r o commit).1.children = r.binding :: s.children := by
  unfold publishClaimed at *
  split at * <;> try simp_all
  all_goals split at * <;> try simp_all
  all_goals split at * <;> try simp_all
  all_goals split at * <;> try simp_all
  all_goals split at * <;> try simp_all
  all_goals split at * <;> try simp_all
  all_goals split at * <;> try simp_all
  all_goals split at * <;> try simp_all
  all_goals have hn : r.expectedSequence ≠ 0 := by omega
  all_goals cases hstatus : r.expectedStatus <;> simp_all [hn, hstatus]

end GoalAutomation.OperatorResume
