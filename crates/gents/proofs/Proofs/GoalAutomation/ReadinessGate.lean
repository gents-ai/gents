import Proofs.Goals

/-! Behavior-readiness gate and infrastructure-retry accounting for durable
Goal continuation (#1345). The existing `Goals.decide` owner still chooses the
continuation; this layer only decides whether that choice may publish a child
now and which terminals may spend the bounded infrastructure retry budget.
-/
namespace GoalAutomation.ReadinessGate

open Goals

/-- Readiness of the continuation's behavior, projected from the canonical
runtime-authored readiness row (`project_behavior_readiness`). The projection
already folds process state, generation alignment, explicit unavailability and
startup demotion through the same predicate that request routing uses. -/
inductive Observation where
  | ready
  /-- `backend_temporarily_unavailable`: measured backend health owns recovery
  (#897), so this never settles into a configuration verdict. -/
  | backendRecovering
  /-- Any other runtime-authored unavailability reason, including startup
  demotion. -/
  | unavailable
  /-- An aligned, ready readiness row that does not assign this behavior. -/
  | unassigned
  /-- Missing, malformed or unsupported row, process not ready, or router
  generation skew. -/
  | unknown
  deriving DecidableEq, Repr

inductive Readiness where
  | ready
  | waiting
  | unavailable
  deriving DecidableEq, Repr

/-- `settled` is the runtime reconcile phase being idle after a reconcile that
did not fail (#1756). Before then an unavailable verdict may describe control
documents that are still arriving, so it only defers the continuation. Once
settled, the verdict reflects configuration that only an operator edit
changes, so the Goal reports it instead of waiting without bound. -/
def observe : Observation → Bool → Readiness
  | .ready, _ => .ready
  | .backendRecovering, _ => .waiting
  | .unknown, _ => .waiting
  | .unavailable, settled => if settled then .unavailable else .waiting
  | .unassigned, settled => if settled then .unavailable else .waiting

/-- How the observed terminal request relates to execution.
`behaviorUnavailable` is a routing admission rejection because the behavior was
unavailable: the request was never claimed and nothing executed. Every other
terminal, including other admission rejections, keeps the existing bounded
accounting. -/
inductive Cause where
  | attempt
  | behaviorUnavailable
  deriving DecidableEq, Repr

structure Input where
  status : Status
  terminal : RequestTerminal
  sessionIdle : Bool
  childExists : Bool
  budgetReached : Bool
  hasActivity : Bool
  requestIsWrapup : Bool
  retries : Nat
  wrapupRequested : Bool
  wrapupCompleted : Bool
  deriving DecidableEq, Repr

/-- A rejected-before-execution request did no work, so the existing owner
decides as though the previous turn had ended normally: the same continuation
(or pending wrap-up) is issued again rather than a charged recovery. -/
def baseDecision (cause : Cause) (i : Input) : Decision :=
  match cause with
  | .attempt =>
      Goals.decide i.status i.terminal i.sessionIdle i.childExists i.budgetReached
        i.hasActivity i.requestIsWrapup i.retries i.wrapupRequested i.wrapupCompleted
  | .behaviorUnavailable =>
      Goals.decide i.status .completed i.sessionIdle i.childExists i.budgetReached
        true false i.retries i.wrapupRequested i.wrapupCompleted

def publishes : Decision → Bool
  | .continue | .retry | .wrapup => true
  | .none | .pause | .abandonWrapup => false

inductive Gated where
  | decided (decision : Decision)
  /-- Publish nothing and change no Goal field; a later scan re-evaluates. -/
  | awaitReadiness
  /-- Settled unavailability: end automatic continuation with that reason. -/
  | behaviorUnavailable
  deriving DecidableEq, Repr

def gate (readiness : Readiness) (cause : Cause) (i : Input) : Gated :=
  let base := baseDecision cause i
  if publishes base then
    match readiness with
    | .ready => .decided base
    | .waiting => .awaitReadiness
    | .unavailable => .behaviorUnavailable
  else .decided base

def maxInfrastructureRetries : Nat := 2

/-- Persisted `infrastructure_retry_count` after GoalSource applies a gated
decision. A clean completed attempt clears the count; everything else that
does not retry leaves it unchanged. -/
def nextRetries (cause : Cause) (i : Input) : Gated → Nat
  | .decided .retry => i.retries + 1
  | .decided .continue | .decided .wrapup =>
      if cause = .attempt ∧ i.terminal = .completed then 0 else i.retries
  | _ => i.retries

/-- Legal Goal transition for a settled-unavailable behavior. Only Goals whose
existing decision would publish reach it: active Goals pause, and a pending
budget wrap-up is abandoned rather than retried. -/
def resolveUnavailable (state : State) : Option State :=
  if state.status = .active then step? state .pause else step? state .wrapupAbandoned

theorem decide_retry_within_budget
    (status : Status) (terminal : RequestTerminal)
    (idle child budget activity wrapup : Bool) (retries : Nat)
    (requested completed : Bool)
    (h : Goals.decide status terminal idle child budget activity wrapup retries requested completed
      = .retry) :
    retries < maxInfrastructureRetries := by
  unfold Goals.decide at h
  unfold maxInfrastructureRetries
  split at h
  · cases h
  · cases status <;> cases terminal <;> (try simp only at h) <;> (repeat' split at h) <;>
      first | omega | cases h

theorem decide_publishes_only_for_open_goals
    (status : Status) (terminal : RequestTerminal)
    (idle child budget activity wrapup : Bool) (retries : Nat)
    (requested completed : Bool)
    (h : publishes
      (Goals.decide status terminal idle child budget activity wrapup retries requested completed)
      = true) :
    status = .active ∨
      (status = .budgetLimited ∧ requested = true ∧ completed = false) := by
  unfold Goals.decide at h
  split at h
  · simp [publishes] at h
  · cases requested <;> cases completed <;> cases status <;> simp_all [publishes]

theorem unavailable_behavior_never_retries (i : Input) :
    baseDecision .behaviorUnavailable i ≠ .retry := by
  intro h
  obtain ⟨status, terminal, idle, child, budget, activity, wrapup, retries, requested,
    completed⟩ := i
  simp only [baseDecision] at h
  cases status <;> cases idle <;> cases child <;> cases budget <;> cases requested <;>
    cases completed <;> simp [Goals.decide] at h

/-- The existing decision owner is unchanged for a ready behavior and an
attempt that ran (or failed admission for a reason other than readiness). -/
theorem ready_attempt_refines_existing_decision (i : Input) :
    gate .ready .attempt i =
      .decided (Goals.decide i.status i.terminal i.sessionIdle i.childExists i.budgetReached
        i.hasActivity i.requestIsWrapup i.retries i.wrapupRequested i.wrapupCompleted) := by
  simp only [gate, baseDecision]
  split <;> rfl

/-- No continuation child is published unless the behavior is ready. -/
theorem publication_requires_ready_behavior
    (readiness : Readiness) (cause : Cause) (i : Input) (decision : Decision)
    (h : gate readiness cause i = .decided decision)
    (hpublish : publishes decision = true) :
    readiness = .ready := by
  simp only [gate] at h
  split at h
  · cases readiness <;> simp_all
  · cases h
    simp_all

/-- While the behavior is not ready, the retry budget is untouched. -/
theorem unready_behavior_preserves_retry_budget
    (readiness : Readiness) (cause : Cause) (i : Input)
    (hready : readiness ≠ .ready) :
    nextRetries cause i (gate readiness cause i) = i.retries := by
  simp only [gate]
  split
  · cases readiness
    · exact absurd rfl hready
    · rfl
    · rfl
  · rename_i hpublish
    generalize hbase : baseDecision cause i = base at hpublish
    cases base <;> simp_all [publishes, nextRetries]

/-- A request rejected because its behavior was unavailable never spends the
retry budget, whatever readiness is observed afterwards. -/
theorem unavailable_behavior_rejection_is_never_charged
    (readiness : Readiness) (i : Input) :
    nextRetries .behaviorUnavailable i (gate readiness .behaviorUnavailable i) = i.retries := by
  have hno := unavailable_behavior_never_retries i
  simp only [gate]
  split
  · cases readiness <;> simp only [nextRetries]
    generalize hbase : baseDecision .behaviorUnavailable i = base at hno
    cases base <;> simp_all [nextRetries]
  · generalize hbase : baseDecision .behaviorUnavailable i = base at hno
    cases base <;> simp_all [nextRetries]

/-- Retries are charged only for an attempt observed against a ready behavior. -/
theorem charge_requires_ready_attempt
    (readiness : Readiness) (cause : Cause) (i : Input)
    (h : i.retries < nextRetries cause i (gate readiness cause i)) :
    readiness = .ready ∧ cause = .attempt := by
  constructor
  · cases readiness
    · rfl
    all_goals
      rw [unready_behavior_preserves_retry_budget _ cause i (by intro h; cases h)] at h
      omega
  · cases cause
    · rfl
    · rw [unavailable_behavior_rejection_is_never_charged] at h
      omega

theorem gate_decided_is_base
    (readiness : Readiness) (cause : Cause) (i : Input) (decision : Decision)
    (h : gate readiness cause i = .decided decision) :
    baseDecision cause i = decision := by
  simp only [gate] at h
  split at h
  · cases readiness <;> simp_all
  · cases h
    rfl

/-- The persisted count never exceeds the bound. -/
theorem retry_budget_is_bounded
    (readiness : Readiness) (cause : Cause) (i : Input)
    (hbound : i.retries ≤ maxInfrastructureRetries) :
    nextRetries cause i (gate readiness cause i) ≤ maxInfrastructureRetries := by
  cases hg : gate readiness cause i with
  | decided decision =>
      have hbase := gate_decided_is_base readiness cause i decision hg
      cases decision with
      | retry =>
          simp only [nextRetries]
          cases cause
          · have := decide_retry_within_budget _ _ _ _ _ _ _ _ _ _ hbase
            omega
          · exact absurd hbase (unavailable_behavior_never_retries i)
      | «continue» =>
          simp only [nextRetries]
          split <;> omega
      | wrapup =>
          simp only [nextRetries]
          split <;> omega
      | none => simpa only [nextRetries] using hbound
      | pause => simpa only [nextRetries] using hbound
      | abandonWrapup => simpa only [nextRetries] using hbound
  | awaitReadiness => simpa only [nextRetries] using hbound
  | behaviorUnavailable => simpa only [nextRetries] using hbound

/-- A settled-unavailable verdict always has a legal Goal transition, so the
Goal leaves automatic continuation instead of waiting without bound. -/
theorem settled_unavailable_resolution_is_legal
    (readiness : Readiness) (cause : Cause) (i : Input) (state : State)
    (h : gate readiness cause i = .behaviorUnavailable)
    (hstatus : state.status = i.status)
    (hrequested : state.wrapupRequested = i.wrapupRequested)
    (hcompleted : state.wrapupCompleted = i.wrapupCompleted) :
    (resolveUnavailable state).isSome := by
  simp only [gate] at h
  split at h
  · rename_i hpublish
    have hopen : i.status = .active ∨
        (i.status = .budgetLimited ∧ i.wrapupRequested = true ∧ i.wrapupCompleted = false) := by
      cases cause <;> simp only [baseDecision] at hpublish <;>
        exact decide_publishes_only_for_open_goals _ _ _ _ _ _ _ _ _ _ hpublish
    rcases hopen with hactive | ⟨hlimited, hreq, hcomp⟩
    · simp [resolveUnavailable, step?, hstatus, hactive]
    · simp [resolveUnavailable, step?, hstatus, hlimited, hrequested, hcompleted, hreq, hcomp]
  · cases h

theorem unsettled_observation_never_ends_continuation (observation : Observation) :
    observe observation false ≠ .unavailable := by
  cases observation <;> simp [observe]

theorem backend_recovery_always_waits (settled : Bool) :
    observe .backendRecovering settled = .waiting := rfl

/-- Existing exactly-once suppression is preserved by the gate. -/
theorem existing_child_is_never_duplicated
    (readiness : Readiness) (cause : Cause) (i : Input) (hchild : i.childExists = true) :
    gate readiness cause i = .decided .none := by
  cases cause <;> simp only [gate, baseDecision, hchild, existing_child_never_duplicates] <;> rfl

/-- A scan sequence folds the persisted count through gated decisions. -/
structure Scan where
  readiness : Readiness
  cause : Cause
  input : Input
  deriving DecidableEq, Repr

def applyScan (retries : Nat) (scan : Scan) : Nat :=
  let i := { scan.input with retries }
  nextRetries scan.cause i (gate scan.readiness scan.cause i)

/-- Any interleaving of unready observations and readiness rejections leaves
the retry budget exactly where it was. -/
theorem readiness_trace_preserves_budget (retries : Nat) (scans : List Scan)
    (h : ∀ scan ∈ scans,
      scan.readiness ≠ .ready ∨ scan.cause = .behaviorUnavailable) :
    scans.foldl applyScan retries = retries := by
  induction scans generalizing retries with
  | nil => rfl
  | cons scan rest ih =>
      simp only [List.foldl]
      have hscan : applyScan retries scan = retries := by
        simp only [applyScan]
        rcases h scan (List.mem_cons_self _ _) with hready | hcause
        · exact unready_behavior_preserves_retry_budget _ _ _ hready
        · rw [hcause]
          exact unavailable_behavior_rejection_is_never_charged _ _
      rw [hscan]
      exact ih retries (fun s hs => h s (List.mem_cons_of_mem _ hs))

/-- Every scan sequence keeps the persisted count within the bound. -/
theorem trace_retry_budget_is_bounded (retries : Nat) (scans : List Scan)
    (hbound : retries ≤ maxInfrastructureRetries) :
    scans.foldl applyScan retries ≤ maxInfrastructureRetries := by
  induction scans generalizing retries with
  | nil => exact hbound
  | cons scan rest ih =>
      simp only [List.foldl]
      apply ih
      exact retry_budget_is_bounded _ _ _ hbound

end GoalAutomation.ReadinessGate
