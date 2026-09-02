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

/-- Optional durable-goal declaration carried by a reusable Task/demo-pack.
    A budget has meaning only when the Task also declares a goal objective. -/
structure TaskGoalDeclaration where
  goalObjective : Option String
  goalTokenBudget : Option Int
  deriving DecidableEq, Repr

def validTaskGoalDeclaration (declaration : TaskGoalDeclaration) : Bool :=
  match declaration.goalObjective with
  | none => declaration.goalTokenBudget.isNone
  | some objective =>
      !(canonicalObjective objective).isEmpty && validBudget declaration.goalTokenBudget

/-- A durable trigger fire must supply a stable fire key. The runtime-facing
    encoding is length-prefixed so the same principal/Task/fire tuple
    deterministically recovers the same session and request retry identities
    after a crash without colliding with another principal's Task. -/
structure TaskFireIdentity where
  sessionId : String
  requestId : String
  retryKey : String
  deriving DecidableEq, Repr

def taskFireScope (agentDid taskId fireKey : String) : String :=
  toString agentDid.length ++ ":" ++ agentDid ++ ":" ++
    toString taskId.length ++ ":" ++ taskId ++ ":" ++
    toString fireKey.length ++ ":" ++ fireKey

def taskFireIdentity (agentDid taskId fireKey : String) : TaskFireIdentity :=
  let scope := taskFireScope agentDid taskId fireKey
  { sessionId := "task-goal-session:" ++ scope
  , requestId := "task-goal-request:" ++ scope
  , retryKey := "task-goal-retry:" ++ scope }

/-- The persisted request binding is the durable recovery witness. Goal and
    GoalCreationClaim documents are creation-time state and may later be
    removed without making an already-published request undiscoverable. -/
structure TaskGoalRequestBinding where
  agentDid : String
  behaviorId : String
  sessionId : String
  requestId : String
  retryKey : String
  deriving DecidableEq, Repr

def expectedTaskGoalRequestBinding (agentDid behaviorId taskId fireKey : String) :
    TaskGoalRequestBinding :=
  let identity := taskFireIdentity agentDid taskId fireKey
  { agentDid, behaviorId
  , sessionId := identity.sessionId
  , requestId := identity.requestId
  , retryKey := identity.retryKey }

structure TaskFireRecoveryState where
  request : Option TaskGoalRequestBinding
  durableGoal : Bool
  creationClaim : Bool
  deriving DecidableEq, Repr

inductive TaskFireRecoveryDisposition where
  | absent
  | recovered
  | conflict
  deriving DecidableEq, Repr

structure TaskFireRecoveryDecision where
  disposition : TaskFireRecoveryDisposition
  recoveredRequestId : Option String
  checkpointable : Bool
  deriving DecidableEq, Repr

/-- Classify the request returned by the globally deterministic request-id
    lookup. Principal DID is part of the expected identity and binding, so a
    foreign or otherwise mismatched row conflicts rather than being recovered
    for the local principal. -/
def decideTaskFireRecovery (expected : TaskGoalRequestBinding)
    (state : TaskFireRecoveryState) : TaskFireRecoveryDecision :=
  match state.request with
  | none => ⟨.absent, none, false⟩
  | some observed =>
      if observed = expected then ⟨.recovered, some expected.requestId, true⟩
      else ⟨.conflict, none, false⟩

theorem matching_task_fire_request_is_recoverable
    (expected : TaskGoalRequestBinding) (goalPresent claimPresent : Bool) :
    let decision := decideTaskFireRecovery expected
      ⟨some expected, goalPresent, claimPresent⟩
    decision.disposition = .recovered ∧
      decision.recoveredRequestId = some expected.requestId ∧
      decision.checkpointable = true := by
  simp [decideTaskFireRecovery]

theorem matching_task_fire_request_survives_goal_metadata_deletion
    (expected : TaskGoalRequestBinding) :
    let decision := decideTaskFireRecovery expected ⟨some expected, false, false⟩
    decision.disposition = .recovered ∧ decision.checkpointable = true := by
  simp [decideTaskFireRecovery]

theorem mismatched_task_fire_binding_conflicts
    (expected observed : TaskGoalRequestBinding)
    (hmismatch : observed ≠ expected) (goalPresent claimPresent : Bool) :
    let decision := decideTaskFireRecovery expected
      ⟨some observed, goalPresent, claimPresent⟩
    decision.disposition = .conflict ∧ decision.checkpointable = false := by
  simp [decideTaskFireRecovery, hmismatch]

inductive TaskPublicationMode where
  | invalid
  | ordinary
  | atomicGoalBacked
  deriving DecidableEq, Repr

structure TaskPublication where
  mode : TaskPublicationMode
  published : Bool
  runnableRequest : Bool
  durableGoal : Bool
  sessionId : Option String
  requestId : Option String
  retryKey : Option String
  deriving DecidableEq, Repr

/-- Task publication is unchanged when no goal is declared. A valid goal
    declaration switches publication to the atomic goal+request boundary and
    carries deterministic identities for retry/reconciliation. -/
def decideTaskPublication (declaration : TaskGoalDeclaration)
    (agentDid taskId fireKey : String) : TaskPublication :=
  if !validTaskGoalDeclaration declaration then
    { mode := .invalid, published := false, runnableRequest := false, durableGoal := false
    , sessionId := none, requestId := none, retryKey := none }
  else match declaration.goalObjective with
  | none =>
      { mode := .ordinary, published := true, runnableRequest := true, durableGoal := false
      , sessionId := none, requestId := none, retryKey := none }
  | some _ =>
      let identity := taskFireIdentity agentDid taskId fireKey
      { mode := .atomicGoalBacked, published := true, runnableRequest := true, durableGoal := true
      , sessionId := some identity.sessionId, requestId := some identity.requestId
      , retryKey := some identity.retryKey }

theorem task_goal_backed_runnable_implies_durable_goal
    (declaration : TaskGoalDeclaration) (agentDid taskId fireKey : String)
    (hmode : (decideTaskPublication declaration agentDid taskId fireKey).mode =
      .atomicGoalBacked)
    (hrunnable :
      (decideTaskPublication declaration agentDid taskId fireKey).runnableRequest = true) :
    (decideTaskPublication declaration agentDid taskId fireKey).durableGoal = true := by
  by_cases hrun :
      (decideTaskPublication declaration agentDid taskId fireKey).runnableRequest = true
  · unfold decideTaskPublication at hmode ⊢
    split <;> simp_all
    split <;> simp_all
  · exact (hrun hrunnable).elim

theorem invalid_task_goal_declaration_cannot_publish
    (declaration : TaskGoalDeclaration) (agentDid taskId fireKey : String)
    (hinvalid : validTaskGoalDeclaration declaration = false) :
    (decideTaskPublication declaration agentDid taskId fireKey).published = false ∧
      (decideTaskPublication declaration agentDid taskId fireKey).runnableRequest = false := by
  simp [decideTaskPublication, hinvalid]

theorem task_without_goal_uses_ordinary_publication (agentDid taskId fireKey : String) :
    (decideTaskPublication ⟨none, none⟩ agentDid taskId fireKey).mode = .ordinary := by
  simp [decideTaskPublication, validTaskGoalDeclaration]

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
