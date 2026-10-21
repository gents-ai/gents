import Proofs.Triggers.Identity
import Proofs.Triggers.Types
import Proofs.Triggers.Queue
import Proofs.Goals

namespace Triggers.Durable


structure Fire where
  identity : Identity
  session : String
  serial : Bool
  emitOutcome : Bool
  goalBacked : Bool
  deriving DecidableEq, Repr

structure Request where
  fire : Fire
  running : Bool := false
  terminal : Bool := false
  assignmentReplaced : Bool := false
  goalAssignmentApplied : Bool := false
  deriving DecidableEq, Repr

structure GoalBinding where
  owner : String
  session : String
  assignment : Identity
  status : Goals.Status := .active
  deriving DecidableEq, Repr

structure State where
  receipts : List Identity := []
  requests : List Request := []
  outcomes : List Identity := []
  goals : List GoalBinding := []
  deriving DecidableEq, Repr

def admitted (state : State) (id : Identity) : Bool := decide (id ∈ state.receipts)

def admit (state : State) (fire : Fire) : State :=
  if admitted state fire.identity || !Triggers.outcomeSourceAllowed fire.identity.collection fire.emitOutcome then state
  else { state with
    receipts := state.receipts ++ [fire.identity]
    requests := state.requests ++ [{ fire := fire }] }

theorem admit_duplicate (state : State) (fire : Fire)
    (h : admitted state fire.identity = true) : admit state fire = state := by
  simp [admit, h]

theorem admit_idempotent (state : State) (fire : Fire) :
    admit (admit state fire) fire = admit state fire := by
  by_cases hv : Triggers.outcomeSourceAllowed fire.identity.collection fire.emitOutcome = true
  · by_cases h : fire.identity ∈ state.receipts
    · simp [admit, admitted, h, hv]
    · simp [admit, admitted, h, hv]
  · have hf : Triggers.outcomeSourceAllowed fire.identity.collection fire.emitOutcome = false := by
      simpa using hv
    simp [admit, hf]

theorem outcome_chain_admission_rejected (state : State) (fire : Fire)
    (hc : fire.identity.collection = "FireOutcome") (he : fire.emitOutcome = true) :
    admit state fire = state := by
  simp [admit, Triggers.outcomeSourceAllowed, hc, he]

theorem admission_has_request (state : State) (fire : Fire)
    (hv : Triggers.outcomeSourceAllowed fire.identity.collection fire.emitOutcome = true)
    (h : admitted state fire.identity = false) :
    (admit state fire).requests = state.requests ++ [{ fire := fire }] := by
  simp [admit, h, hv]

theorem admission_has_receipt (state : State) (fire : Fire)
    (hv : Triggers.outcomeSourceAllowed fire.identity.collection fire.emitOutcome = true) :
    admitted (admit state fire) fire.identity = true := by
  by_cases h : fire.identity ∈ state.receipts
  · simp [admit, admitted, h, hv]
  · simp [admit, admitted, h, hv]

def requestObservations (state : State) : List ClaimObservation :=
  state.requests.zipIdx.map fun (request, index) => {
    document := request.fire.identity.key
    owner := request.fire.identity.owner
    session := request.fire.session
    trigger := request.fire.identity.trigger
    serial := request.fire.serial
    receipt := true
    arrival := some (index + 1)
    running := request.running
    terminal := request.terminal }

def canClaim (state : State) (id : Identity) : Bool :=
  let rows := requestObservations state
  match rows.find? (fun row => row.document == id.key) with
  | none => false
  | some candidate => observedClaimAllowed candidate rows

theorem queued_claim_uses_native_observation_owner (state : State) (id : Identity)
    (candidate : ClaimObservation)
    (h : (requestObservations state).find? (fun row => row.document == id.key) = some candidate) :
    canClaim state id = observedClaimAllowed candidate (requestObservations state) := by
  simp [canClaim, h]

theorem admission_preserves_outcomes (state : State) (fire : Fire) :
    (admit state fire).outcomes = state.outcomes := by
  simp only [admit]
  split <;> rfl

def admitTransaction (state : State) (fire : Fire) (commit : Bool) : State :=
  if commit then admit state fire else state

theorem admission_crash_before_commit (state : State) (fire : Fire) :
    admitTransaction state fire false = state := rfl

theorem admission_crash_after_commit (state : State) (fire : Fire) :
    admitTransaction (admitTransaction state fire true) fire true =
      admitTransaction state fire true := admit_idempotent state fire

def goalEnded (status : Goals.Status) : Bool :=
  status == .complete || status == .blocked || status == .budgetLimited || status == .stopped

inductive OutcomeReason where
  | requestTerminal
  | goalTerminal (status : Goals.Status)
  | superseded
  deriving DecidableEq, Repr

def boundGoal (state : State) (request : Request) : Option GoalBinding :=
  state.goals.find? fun goal => goal.owner == request.fire.identity.owner &&
    goal.session == request.fire.session && goal.assignment == request.fire.identity

def outcomeReason (state : State) (request : Request) : Option OutcomeReason :=
  if !request.fire.emitOutcome then none
  else if !request.fire.goalBacked || !request.goalAssignmentApplied then
    if request.terminal then some .requestTerminal else none
  else if request.assignmentReplaced then some .superseded
  else match boundGoal state request with
    | some goal => if goalEnded goal.status then some (.goalTerminal goal.status) else none
    | none => none

def retainedOutcomeBinding (request : Request) (retained current : Option GoalBinding) :
    Option GoalBinding :=
  match retained.filter (fun goal => goal.owner == request.fire.identity.owner &&
      goal.session == request.fire.session && goal.assignment == request.fire.identity) with
  | none => none
  | some goal =>
    if goalEnded goal.status then some goal
    else (current.filter fun next => next.owner == request.fire.identity.owner &&
      next.session == request.fire.session).orElse (fun _ => some goal)

def retainedOutcomeReason (request : Request) (retained current : Option GoalBinding) :
    Option OutcomeReason :=
  let binding := retainedOutcomeBinding request retained current
  let replaced := binding.any fun goal => goal.assignment != request.fire.identity
  outcomeReason { goals := binding.toList } { request with assignmentReplaced := replaced }

theorem stopped_assignment_has_terminal_outcome (state : State) (request : Request)
    (goal : GoalBinding) (emit : request.fire.emitOutcome = true)
    (backed : request.fire.goalBacked = true) (applied : request.goalAssignmentApplied = true)
    (replaced : request.assignmentReplaced = false) (bound : boundGoal state request = some goal)
    (stopped : goal.status = .stopped) :
    outcomeReason state request = some (.goalTerminal .stopped) := by
  simp [outcomeReason, emit, backed, applied, replaced, bound, stopped, goalEnded]

theorem missing_retained_assignment_is_unknown (request : Request) (current : Option GoalBinding) :
    retainedOutcomeBinding request none current = none := rfl

theorem terminal_retained_assignment_ignores_new_goal (request : Request) (goal : GoalBinding)
    (current : Option GoalBinding) (owner : goal.owner = request.fire.identity.owner)
    (session : goal.session = request.fire.session) (root : goal.assignment = request.fire.identity)
    (ended : goalEnded goal.status = true) :
    retainedOutcomeBinding request (some goal) current = some goal := by
  simp [retainedOutcomeBinding, Option.filter, owner, session, root, ended]

def outcomeDue (state : State) (request : Request) : Bool :=
  (outcomeReason state request).isSome

def publishOutcome (state : State) (request : Request) : State :=
  if outcomeDue state request && !(decide (request.fire.identity ∈ state.outcomes)) then
    { state with outcomes := state.outcomes ++ [request.fire.identity] }
  else state

def dueIdentities (state : State) : List Identity :=
  (state.requests.filter (outcomeDue state)).map (·.fire.identity)

def recoverOutcomes (state : State) : State :=
  { state with outcomes := state.outcomes ++
      (dueIdentities state).dedup.filter (fun id => !decide (id ∈ state.outcomes)) }

def terminalize (state : State) (id : Identity) : State :=
  { state with requests := state.requests.map fun request =>
    if request.fire.identity == id then { request with running := false, terminal := true }
    else request }

def setGoal (state : State) (id : Identity) (status : Goals.Status) : State :=
  { state with goals := state.goals.map fun goal =>
    if goal.assignment == id then { goal with status := status } else goal }

def sameGoalSession (left right : Request) : Bool :=
  left.fire.identity.owner == right.fire.identity.owner && left.fire.session == right.fire.session

def stageAssignment (before : State) (candidate : Request) : State :=
  let binding : GoalBinding := {
    owner := candidate.fire.identity.owner
    session := candidate.fire.session
    assignment := candidate.fire.identity }
  { before with
    goals := binding :: before.goals.filter (fun goal =>
        !(goal.owner == candidate.fire.identity.owner && goal.session == candidate.fire.session))
    requests := before.requests.map fun request =>
      if request.fire.identity == candidate.fire.identity then
        { request with goalAssignmentApplied := true }
      else if request.fire.goalBacked && request.goalAssignmentApplied && sameGoalSession candidate request then
        { request with assignmentReplaced := true }
      else request }

def applyAssignment (state : State) (candidate : Request) : State :=
  recoverOutcomes (stageAssignment (recoverOutcomes state) candidate)

def claim (state : State) (id : Identity) : State :=
  if canClaim state id then
    let claimed : State := { state with requests := state.requests.map fun r =>
      if r.fire.identity == id then { r with running := true } else r }
    match claimed.requests.find? (fun r => r.fire.identity == id) with
    | some request => if request.fire.goalBacked then applyAssignment claimed request else claimed
    | none => claimed
  else state

theorem blocked_claim_preserves_queue (state : State) (id : Identity)
    (h : canClaim state id = false) : claim state id = state := by
  simp [claim, h]

theorem terminal_transition_preserves_outcomes (state : State) (id : Identity) :
    (terminalize state id).outcomes = state.outcomes := rfl

theorem goal_transition_preserves_outcomes (state : State) (id : Identity) (status : Goals.Status) :
    (setGoal state id status).outcomes = state.outcomes := rfl

theorem queued_goal_no_outcome (state : State) (request : Request)
    (hq : request.goalAssignmentApplied = false) (ht : request.terminal = false) :
    outcomeDue state request = false := by
  simp [outcomeDue, outcomeReason, hq, ht]

theorem unapplied_terminal_goal_outcome (state : State) (request : Request)
    (he : request.fire.emitOutcome = true) (hq : request.goalAssignmentApplied = false)
    (ht : request.terminal = true) :
    outcomeReason state request = some .requestTerminal := by
  simp [outcomeReason, he, hq, ht]

theorem ordinary_goal_boundary_no_outcome (state : State) (request : Request) (goal : GoalBinding)
    (hg : request.fire.goalBacked = true) (ha : request.goalAssignmentApplied = true)
    (hr : request.assignmentReplaced = false) (hb : boundGoal state request = some goal)
    (hs : goal.status = .active) : outcomeDue state request = false := by
  simp [outcomeDue, outcomeReason, hg, ha, hr, hb, hs, goalEnded]

theorem opted_out_no_outcome (state : State) (request : Request)
    (h : request.fire.emitOutcome = false) : outcomeDue state request = false := by
  simp [outcomeDue, outcomeReason, h]

theorem outcome_idempotent (state : State) (request : Request) :
    publishOutcome (publishOutcome state request) request = publishOutcome state request := by
  simp only [publishOutcome]
  split
  · simp [List.mem_append]
  · rename_i h
    simp [h]

@[simp] theorem recovery_requests (state : State) :
    (recoverOutcomes state).requests = state.requests := rfl

@[simp] theorem recovery_due (state : State) (request : Request) :
    outcomeDue (recoverOutcomes state) request = outcomeDue state request := rfl

@[simp] theorem recovery_due_identities (state : State) :
    dueIdentities (recoverOutcomes state) = dueIdentities state := rfl

theorem recovery_membership (state : State) (id : Identity) :
    id ∈ (recoverOutcomes state).outcomes ↔ id ∈ state.outcomes ∨ id ∈ dueIdentities state := by
  simp only [recoverOutcomes, List.mem_append, List.mem_filter, List.mem_dedup, Bool.not_eq_true,
    decide_eq_false_iff_not]
  by_cases h : id ∈ state.outcomes <;> simp [h]

theorem recovery_complete (state : State) (request : Request)
    (hm : request ∈ state.requests) (hd : outcomeDue state request = true) :
    request.fire.identity ∈ (recoverOutcomes state).outcomes := by
  apply (recovery_membership _ _).mpr
  right
  simp only [dueIdentities, List.mem_map, List.mem_filter]
  exact ⟨request, ⟨hm, hd⟩, rfl⟩

theorem recovery_idempotent (state : State) :
    recoverOutcomes (recoverOutcomes state) = recoverOutcomes state := by
  have empty : (dueIdentities (recoverOutcomes state)).dedup.filter
      (fun id => !decide (id ∈ (recoverOutcomes state).outcomes)) = [] := by
    apply List.filter_eq_nil_iff.mpr
    intro id hi
    have hm : id ∈ (recoverOutcomes state).outcomes :=
      (recovery_membership state id).mpr (Or.inr (by simpa using hi))
    simp [hm]
  unfold recoverOutcomes at empty ⊢
  simp only [empty, List.append_nil]

theorem recovery_unique (state : State) (valid : state.outcomes.Nodup) :
    (recoverOutcomes state).outcomes.Nodup := by
  apply List.nodup_append.mpr
  refine ⟨valid, List.Nodup.filter _ (List.nodup_dedup _), ?_⟩
  simp only [List.disjoint_left, List.mem_filter]
  intro id present missing
  simpa [present] using missing.2

theorem recovery_eventual_if_run (state : State) (request : Request) (trace : Nat → State)
    (hm : request ∈ state.requests) (hd : outcomeDue state request = true)
    (scheduled : ∃ n, trace n = recoverOutcomes state) :
    ∃ n, request.fire.identity ∈ (trace n).outcomes := by
  obtain ⟨n, hn⟩ := scheduled
  exact ⟨n, hn ▸ recovery_complete state request hm hd⟩

theorem recovery_preserves_published (state : State) (id : Identity)
    (h : id ∈ state.outcomes) : id ∈ (recoverOutcomes state).outcomes :=
  (recovery_membership state id).mpr (Or.inl h)

theorem replacement_binding_is_unique (before : State) (candidate : Request) (goal : GoalBinding)
    (member : goal ∈ (stageAssignment before candidate).goals)
    (owner : goal.owner = candidate.fire.identity.owner)
    (session : goal.session = candidate.fire.session) :
    goal.assignment = candidate.fire.identity := by
  simp only [stageAssignment, List.mem_cons, List.mem_filter] at member
  rcases member with equal | ⟨_, retained⟩
  · cases equal
    rfl
  · simp [owner, session] at retained

theorem replacement_cannot_cross_owner (candidate other : Request)
    (different : candidate.fire.identity.owner ≠ other.fire.identity.owner) :
    sameGoalSession candidate other = false := by
  simp [sameGoalSession, different]

theorem replacement_preserves_prior_terminal_outcome (state : State) (candidate old : Request)
    (hm : old ∈ state.requests) (hd : outcomeDue state old = true) :
    old.fire.identity ∈ (applyAssignment state candidate).outcomes := by
  apply recovery_preserves_published
  exact recovery_complete state old hm hd

theorem replacement_publishes_prior_applied (state : State) (candidate old : Request)
    (hm : old ∈ state.requests) (hne : old.fire.identity ≠ candidate.fire.identity)
    (hg : old.fire.goalBacked = true) (ha : old.goalAssignmentApplied = true)
    (he : old.fire.emitOutcome = true) (hs : sameGoalSession candidate old = true) :
    old.fire.identity ∈ (applyAssignment state candidate).outcomes := by
  let replaced := { old with assignmentReplaced := true }
  have member : replaced ∈ (stageAssignment (recoverOutcomes state) candidate).requests := by
    simp only [stageAssignment, recovery_requests, List.mem_map]
    refine ⟨old, hm, ?_⟩
    simp [hne, hg, ha, hs, replaced]
  have due : outcomeDue (stageAssignment (recoverOutcomes state) candidate) replaced = true := by
    simp [outcomeDue, outcomeReason, replaced, he, hg, ha]
  exact recovery_complete _ replaced member due

theorem unapplied_replacement_is_not_assignment_termination (candidate old : Request)
    (hne : old.fire.identity ≠ candidate.fire.identity)
    (ha : old.goalAssignmentApplied = false) :
    (if old.fire.identity == candidate.fire.identity then { old with goalAssignmentApplied := true }
      else if old.fire.goalBacked && old.goalAssignmentApplied && sameGoalSession candidate old then
        { old with assignmentReplaced := true } else old) = old := by
  simp [hne, ha]

theorem unmatched_goal_cannot_terminate_assignment (state : State) (request : Request)
    (hg : request.fire.goalBacked = true) (ha : request.goalAssignmentApplied = true)
    (hr : request.assignmentReplaced = false) (hb : boundGoal state request = none) :
    outcomeDue state request = false := by
  simp [outcomeDue, outcomeReason, hg, ha, hr, hb]

theorem resumable_goal_stop_has_no_outcome (state : State) (request : Request) (goal : GoalBinding)
    (hg : request.fire.goalBacked = true) (ha : request.goalAssignmentApplied = true)
    (hr : request.assignmentReplaced = false) (hb : boundGoal state request = some goal)
    (hs : goal.status = .paused ∨ goal.status = .usageLimited) :
    outcomeDue state request = false := by
  rcases hs with hs | hs <;>
    simp [outcomeDue, outcomeReason, hg, ha, hr, hb, goalEnded, hs]

theorem recovered_due_outcome_exactly_once (state : State) (request : Request)
    (valid : state.outcomes.Nodup) (hm : request ∈ state.requests)
    (hd : outcomeDue state request = true) :
    ((recoverOutcomes state).outcomes.count request.fire.identity) = 1 :=
  List.count_eq_one_of_mem (recovery_unique state valid) (recovery_complete state request hm hd)

theorem bound_goal_uses_exact_assignment (state : State) (request : Request) (goal : GoalBinding)
    (h : boundGoal state request = some goal) : goal.assignment = request.fire.identity := by
  have predicate := List.find?_some h
  simp only [Bool.and_eq_true, beq_iff_eq] at predicate
  exact predicate.2

theorem other_assignment_status_cannot_end_fire (state : State) (request : Request) (goal : GoalBinding)
    (bindings : state.goals = [goal]) (different : goal.assignment ≠ request.fire.identity)
    (hg : request.fire.goalBacked = true) (ha : request.goalAssignmentApplied = true)
    (hr : request.assignmentReplaced = false) : outcomeDue state request = false := by
  apply unmatched_goal_cannot_terminate_assignment state request hg ha hr
  simp [boundGoal, bindings, different]

theorem terminal_publication_gap (state : State) (id : Identity)
    (missing : id ∉ state.outcomes) : id ∉ (terminalize state id).outcomes := missing

def resolveSession (id : Identity) (target : Option String)
    (ownedSameAgent : Bool) : Option String :=
  match target with
  | none => some id.sessionId
  | some value => if value.isEmpty || !ownedSameAgent then none else some value

def isCurrent (callerOwner callerSession listedOwner listedSession : String) : Bool :=
  callerOwner == listedOwner && callerSession == listedSession

theorem current_session_marks_self (owner session : String) :
    isCurrent owner session owner session = true := by
  simp [isCurrent]

theorem foreign_owner_is_not_current (callerOwner listedOwner session : String)
    (h : callerOwner ≠ listedOwner) :
    isCurrent callerOwner session listedOwner session = false := by
  simp [isCurrent, h]

theorem concurrent_session_is_not_current (owner caller listed : String)
    (h : caller ≠ listed) : isCurrent owner caller owner listed = false := by
  simp [isCurrent, h]

theorem existing_session_retained (id : Identity) (session : String)
    (h : session.isEmpty = false) : resolveSession id (some session) true = some session := by
  simp [resolveSession, h]

theorem foreign_session_rejected (id : Identity) (session : String) :
    resolveSession id (some session) false = none := by
  simp [resolveSession]

structure GoalAssignment where
  state : Goals.State
  epoch : Nat
  usageBaseline : Nat := 0
  deriving DecidableEq, Repr

def assignGoal (previous : Option GoalAssignment) (sessionUsage : Nat) : GoalAssignment :=
  let fresh : Goals.State := { status := .active, blockedAudits := 0, wrapupRequested := false, wrapupCompleted := false }
  match previous with
  | none => { state := fresh, epoch := 0, usageBaseline := sessionUsage }
  | some old =>
    { state := if old.state.status == .active then old.state else fresh
      epoch := old.epoch + 1
      usageBaseline := if old.state.status == .active then old.usageBaseline else sessionUsage }

theorem task_assignment_active (previous : Option GoalAssignment) (usage : Nat) :
    (assignGoal previous usage).state.status = .active := by
  cases previous with
  | none => rfl
  | some old =>
    simp only [assignGoal]
    split
    · rename_i h
      simpa using h
    · rfl

theorem task_assignment_invalidates_previous_controller (previous : GoalAssignment) (usage : Nat) :
    (assignGoal (some previous) usage).epoch ≠ previous.epoch := by
  simp [assignGoal]

theorem new_assignment_usage_baseline (usage : Nat) :
    (assignGoal none usage).usageBaseline = usage := rfl

/-- Destination admission and replacement are one transaction. A rejected
destination or replay cannot cancel previously admitted work. Execution
revocation preserves its canonical output through the existing terminal owner. -/
def admitLatest (state : State) (fire : Fire) (destinationAllowed : Bool) : State :=
  if !destinationAllowed || admitted state fire.identity ||
      !Triggers.outcomeSourceAllowed fire.identity.collection fire.emitOutcome then state
  else recoverOutcomes { state with
    receipts := state.receipts ++ [fire.identity]
    requests := (state.requests.map fun request =>
      if request.fire.identity.owner == fire.identity.owner &&
          request.fire.identity.trigger == fire.identity.trigger && !request.terminal then
        { request with running := false, terminal := true }
      else request) ++ [{ fire := fire }] }

theorem rejected_latest_preserves_work (state : State) (fire : Fire) :
    admitLatest state fire false = state := by simp [admitLatest]

theorem replayed_latest_preserves_work (state : State) (fire : Fire)
    (h : admitted state fire.identity = true) :
    admitLatest state fire true = state := by simp [admitLatest, h]


end Triggers.Durable
