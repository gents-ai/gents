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
`settledDiagnostic` is a canonical failed/timed-out/cancelled control result,
which need not have a JSON wait envelope and cannot suspend Goal publication. -/
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
  completed : Bool
  deriving DecidableEq, Repr

structure PublicationObservation where
  waits : List WaitControl
  /-- `none` means an authoritative target observation could not be assembled,
  including a failed query or undecodable row; it is not a proven empty scan. -/
  backgrounds : Option (List BackgroundTool)
  deriving DecidableEq, Repr

inductive WaitAssessment where
  | absent | pending | invalid
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

/-- A completed bounded wait is intentional only while its exact physical
background generation is running. A malformed same-parent canonical receipt
blocks publication; an unrelated wait or a mere background launch does not.
The native projection must bind spawned origin to the accepted physical
spawn_process parent, not trust a row's claimed handle or key alone. Ordinary
direct background calls have no immediate invocation receipt, so no legal
terminal predecessor can leave one running; generic tools use spawned children. -/
def assessWait (r : ClaimedRequest) (o : PublicationObservation) : WaitAssessment :=
  let relevant := o.waits.filter (matchingWait r)
  if relevant.any fun w =>
      !w.completed || w.reply == .malformed ||
        (w.reply != .settledDiagnostic && w.reply != .argumentError &&
          w.replyHandle != w.acceptedHandle) then .invalid
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
      else if assessWait r o == .invalid then (s, .invalidEvidence)
      else if assessWait r o == .pending then (s, .deferred)
      else if commit then
        ({ s with latestRequest := r.binding.child,
                  children := r.binding :: s.children }, .created)
      else (s, .rolledBack)

theorem pending_wait_cannot_create (s : Snapshot) (r : ClaimedRequest)
    (o : PublicationObservation) (commit : Bool)
    (h : assessWait r o = .pending) :
    (publishClaimed s r o commit).2 ≠ .created := by
  unfold publishClaimed
  split <;> try simp_all [h]
  all_goals split <;> try simp_all [h]
  all_goals split <;> try simp_all [h]
  all_goals split <;> try simp_all [h]
  all_goals simp_all [h]

theorem invalid_wait_cannot_create (s : Snapshot) (r : ClaimedRequest)
    (o : PublicationObservation) (commit : Bool)
    (h : assessWait r o = .invalid) :
    (publishClaimed s r o commit).2 ≠ .created := by
  unfold publishClaimed
  split <;> try simp_all [h]
  all_goals split <;> try simp_all [h]
  all_goals split <;> try simp_all [h]
  all_goals split <;> try simp_all [h]
  all_goals simp_all [h]

theorem publication_preserves_claim_and_budget (s : Snapshot) (r : ClaimedRequest)
    (o : PublicationObservation) (commit : Bool) :
    (publishClaimed s r o commit).1.goal = s.goal ∧
    (publishClaimed s r o commit).1.sequence = s.sequence ∧
    (publishClaimed s r o commit).1.lastContinuedFrom = s.lastContinuedFrom ∧
    (publishClaimed s r o commit).1.tokensUsed = s.tokensUsed ∧
    (publishClaimed s r o commit).1.tokenBudget = s.tokenBudget := by
  unfold publishClaimed
  split <;> try simp_all
  split <;> try simp_all
  all_goals split <;> try simp_all
  all_goals cases commit <;> simp_all
  all_goals split <;> try simp_all
  all_goals split <;> try simp_all
  all_goals split <;> try simp_all

theorem discarded_claimed_publication_is_noop (s : Snapshot) (r : ClaimedRequest)
    (o : PublicationObservation) :
    (publishClaimed s r o false).1 = s := by
  unfold publishClaimed
  split <;> try rfl
  split <;> try rfl
  all_goals split <;> try rfl
  all_goals split <;> try rfl
  all_goals split <;> try rfl
  all_goals split <;> rfl

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
  all_goals have hn : r.expectedSequence ≠ 0 := by omega
  all_goals cases hstatus : r.expectedStatus <;> simp_all [hn, hstatus]

end GoalAutomation.OperatorResume
