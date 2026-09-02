import Proofs.Goals

namespace GoalAutomation

abbrev Did := String
abbrev SessionId := String

def maxTokenBudget : Int := 9223372036854775807

def validBudget : Option Int → Bool
  | none => true
  | some value => decide (0 < value ∧ value ≤ maxTokenBudget)

def canonicalObjective (objective : String) : String := objective.trim

structure Fingerprint where
  owner : Did
  session : SessionId
  objective : String
  tokenBudget : Option Int
  deriving DecidableEq, Repr

structure CreateRequest where
  caller : Did
  currentSession : SessionId
  requestedOwner : Did
  requestedSession : SessionId
  objective : String
  objectiveNonempty : Bool
  tokenBudget : Option Int
  goalTools : Bool
  goalCreate : Bool
  deriving DecidableEq, Repr

inductive CreateDisposition where
  | denied
  | invalid
  | fresh
  | idempotent
  | conflict
  deriving DecidableEq, Repr

def createFingerprint (request : CreateRequest) : Fingerprint :=
  { owner := request.caller
  , session := request.currentSession
  , objective := canonicalObjective request.objective
  , tokenBudget := request.tokenBudget }

def decideCreate (request : CreateRequest) (existing : Option Fingerprint) : CreateDisposition :=
  if !request.goalTools || !request.goalCreate then .denied
  else if request.requestedOwner != request.caller ||
      request.requestedSession != request.currentSession then .denied
  else if !request.objectiveNonempty || request.objective.trim.isEmpty ||
      !validBudget request.tokenBudget then .invalid
  else match existing with
  | none => .fresh
  | some fingerprint =>
      if fingerprint = createFingerprint request then .idempotent else .conflict

theorem create_never_crosses_owner
    (request : CreateRequest) (existing : Option Fingerprint)
    (h : request.requestedOwner ≠ request.caller) :
    decideCreate request existing = .denied := by
  simp [decideCreate, h]

theorem create_never_crosses_session
    (request : CreateRequest) (existing : Option Fingerprint)
    (h : request.requestedSession ≠ request.currentSession) :
    decideCreate request existing = .denied := by
  by_cases ht : request.goalTools = true <;>
    cases request.goalTools <;> simp_all [decideCreate]

theorem create_requires_both_capabilities
    (request : CreateRequest) (existing : Option Fingerprint)
    (h : request.goalTools = false ∨ request.goalCreate = false) :
    decideCreate request existing = .denied := by
  rcases h with h | h <;> simp [decideCreate, h]

theorem duplicate_create_is_idempotent (request : CreateRequest)
    (hcaps : request.goalTools = true ∧ request.goalCreate = true)
    (howner : request.requestedOwner = request.caller)
    (hsession : request.requestedSession = request.currentSession)
    (hobjective : request.objectiveNonempty = true)
    (htrimmed : request.objective.trim.isEmpty = false)
    (hbudget : validBudget request.tokenBudget = true) :
    decideCreate request (some (createFingerprint request)) = .idempotent := by
  rcases hcaps with ⟨hgoal, hcreate⟩
  simp [decideCreate, hgoal, hcreate, howner, hsession, hobjective, htrimmed, hbudget]

theorem invalid_budget_never_creates (request : CreateRequest)
    (existing : Option Fingerprint)
    (hbudget : validBudget request.tokenBudget = false) :
    decideCreate request existing ≠ .fresh := by
  unfold decideCreate
  split <;> simp_all
  split <;> simp_all

structure SubmissionState where
  durableGoal : Bool
  runnableRequest : Bool
  stagedGoal : Bool
  stagedRequest : Bool
  deriving DecidableEq, Repr

inductive SubmissionAction where
  | stageGoal
  | stageRequest
  | commit
  | abort
  | crash
  deriving DecidableEq, Repr

def submissionStep (state : SubmissionState) : SubmissionAction → SubmissionState
  | .stageGoal => { state with stagedGoal := true }
  | .stageRequest =>
      if state.stagedGoal ∨ state.durableGoal then { state with stagedRequest := true } else state
  | .commit =>
      if state.stagedGoal ∧ state.stagedRequest then
        { durableGoal := true, runnableRequest := true,
          stagedGoal := false, stagedRequest := false }
      else state
  | .abort | .crash => { state with stagedGoal := false, stagedRequest := false }

def submissionSafe (state : SubmissionState) : Prop :=
  state.runnableRequest = true → state.durableGoal = true

theorem submission_step_preserves_safety
    (state : SubmissionState) (action : SubmissionAction)
    (hsafe : submissionSafe state) : submissionSafe (submissionStep state action) := by
  cases action <;> simp only [submissionStep]
  all_goals unfold submissionSafe at *
  all_goals (try split) <;> simp_all

theorem failure_between_writes_rolls_back (state : SubmissionState) :
    let staged := submissionStep (submissionStep state .stageGoal) .stageRequest
    (submissionStep staged .crash).durableGoal = state.durableGoal ∧
      (submissionStep staged .crash).runnableRequest = state.runnableRequest := by
  simp [submissionStep]

inductive ContinuationPhase where
  | unclaimed
  | claimed
  | childPresent
  deriving DecidableEq, Repr

inductive ContinuationAction where
  | claim (eligible : Bool)
  | materialize
  | reconcile
  | crash
  deriving DecidableEq, Repr

def continuationStep : ContinuationPhase → ContinuationAction → ContinuationPhase
  | .unclaimed, .claim true => .claimed
  | .unclaimed, .reconcile => .unclaimed
  | .claimed, .materialize | .claimed, .reconcile => .childPresent
  | phase, .crash => phase
  | phase, _ => phase

theorem continuation_restart_converges_from_claim :
    continuationStep .claimed .reconcile = .childPresent := rfl

theorem continuation_reconcile_is_idempotent :
    continuationStep .childPresent .reconcile = .childPresent := rfl

theorem ineligible_continuation_is_not_claimed :
    continuationStep .unclaimed (.claim false) = .unclaimed := rfl

end GoalAutomation
