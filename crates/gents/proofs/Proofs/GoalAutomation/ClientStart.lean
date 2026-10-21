import Proofs.GoalAutomation
import Proofs.GoalAutomation.ClientControl
import Proofs.AgentSession
import Proofs.Session.CommandAuthority

namespace GoalAutomation.ClientStart

structure State where
  session : Option AgentSession.Document
  submission : SubmissionState
  deriving DecidableEq, Repr

structure Facts where
  authority : AgentSession.CommandAuthority.Facts
  hasPeer : Bool
  now : Nat
  expiresAt : Nat
  firstRequestValid : Bool
  unfinishedRequest : Bool
  currentSessionDocument : Option String
  expectedSessionDocument : Option String
  existingClaim : Option Fingerprint := none
  firstGoalId : Option String := some "initial-assignment"
  firstRoot : Option String := none
  deriving DecidableEq, Repr

def creationRequest (requested : AgentSession.Document) (objective : String)
    (budget : Option Int) : CreateRequest :=
  { caller := toString requested.scope.node
    currentSession := toString requested.scope.session
    requestedOwner := toString requested.scope.node
    requestedSession := toString requested.scope.session
    objective, objectiveNonempty := !objective.isEmpty, tokenBudget := budget
    goalTools := true, goalCreate := true }

def creationAllowed (requested : AgentSession.Document) (objective : String)
    (budget : Option Int) (claim : Option Fingerprint) : Bool :=
  match decideCreate (creationRequest requested objective budget) claim with
  | .fresh | .idempotent => true
  | _ => false

def firstBindingValid (facts : Facts) (goalId : String) : Bool :=
  facts.firstGoalId == some goalId && facts.firstRoot.isNone

/-- The first ordinary request has already passed the existing signature,
admission and configuration owners. A fresh client goal starts only while
the whole session is idle, so its usage baseline excludes earlier work.
The native transaction commits the reserved session, Goal, creation claim,
first request and signed command receipt together. -/
def start? (s : State) (requested : AgentSession.Document) (objective : String)
    (budget : Option Int) (facts : Facts) (commit : Bool)
    (goalId : String := "initial-assignment") : Option State := do
  if !commit ||
      !AgentSession.CommandAuthority.authorized facts.authority facts.hasPeer facts.now facts.expiresAt ||
      !facts.firstRequestValid || !firstBindingValid facts goalId || facts.unfinishedRequest ||
      facts.currentSessionDocument != facts.expectedSessionDocument ||
      s.submission.durableGoal || s.submission.runnableRequest ||
      !creationAllowed requested objective budget facts.existingClaim then none
  else
    let session ← AgentSession.ensure? s.session requested
    let stagedGoal := submissionStep s.submission .stageGoal
    let stagedRequest := submissionStep stagedGoal .stageRequest
    some ⟨some session, submissionStep stagedRequest .commit⟩

theorem discarded_start_is_not_published (s : State) (requested : AgentSession.Document)
    (objective : String) (budget : Option Int) (facts : Facts) (goalId : String) :
    start? s requested objective budget facts false goalId = none := by simp [start?]

theorem busy_session_cannot_start (s : State) (requested : AgentSession.Document)
    (objective : String) (budget : Option Int) (facts : Facts) (commit : Bool) (goalId : String)
    (h : facts.unfinishedRequest = true) :
    start? s requested objective budget facts commit goalId = none := by simp [start?, h]

theorem publication_has_reserved_session_and_goal (s post : State)
    (requested : AgentSession.Document) (objective : String) (budget : Option Int)
    (facts : Facts) (commit : Bool) (goalId : String)
    (h : start? s requested objective budget facts commit goalId = some post) :
    post.session.isSome = true ∧ post.submission.durableGoal = true ∧
      post.submission.runnableRequest = true := by
  unfold start? at h
  split at h
  · contradiction
  · cases hs : AgentSession.ensure? s.session requested with
    | none => simp [hs] at h
    | some session =>
      simp [hs, submissionStep] at h
      cases h
      exact ⟨rfl, rfl, rfl⟩

theorem wrong_goal_reference_cannot_start (s : State) (requested : AgentSession.Document)
    (objective : String) (budget : Option Int) (facts : Facts) (commit : Bool) (goalId : String)
    (wrong : facts.firstGoalId ≠ some goalId) :
    start? s requested objective budget facts commit goalId = none := by
  simp [start?, firstBindingValid, wrong]

theorem first_request_must_open_itself (s : State) (requested : AgentSession.Document)
    (objective : String) (budget : Option Int) (facts : Facts) (commit : Bool) (goalId : String)
    (bound : facts.firstRoot.isNone = false) :
    start? s requested objective budget facts commit goalId = none := by
  simp [start?, firstBindingValid, bound]

/-- Each logical assignment keeps its own immutable creation claim and opening
request. Retiring a terminal Goal uses DefraDB's retained tombstone/history;
its rows remain readable through the existing historical Goal owner. It does
not rewrite that assignment, its accounting, or its request ancestry. -/
structure Assignment where
  id : String
  creationKey : String
  snapshot : ClientControl.Snapshot
  rootRequest : String
  usageBaseline : Nat
  activeSeconds : Nat
  deriving DecidableEq, Repr

/-- `retained` is the read projection of historical Goal documents, not another
writable assignment registry. The transaction replaces only the current head. -/
structure Assignments where
  current : Assignment
  retained : List Assignment
  deriving DecidableEq, Repr

structure Replacement where
  expected : ClientControl.Revision
  nextId : String
  creationKey : String
  nextRevision : ClientControl.Revision
  firstRequest : String
  sessionUsage : Nat
  deriving DecidableEq, Repr

def replaceable (status : Goals.Status) : Bool :=
  status == .complete || status == .stopped

/-- Replacement is an explicit, revision-fenced first request in an idle
session. A stopped assignment stays terminal; a fresh identity and claim own
new counters and ancestry. The existing start owner authorizes and atomically
publishes the session, fresh Goal, request and receipt. -/
def replace? (s : State) (assignments : Assignments) (replacement : Replacement)
    (requested : AgentSession.Document) (objective : String) (budget : Option Int)
    (facts : Facts) (commit : Bool) : Option (State × Assignments) := do
  let current := assignments.current
  if !s.submission.durableGoal ||
      !replaceable current.snapshot.execution.goal.status ||
      current.snapshot.revision != replacement.expected ||
      replacement.nextId.isEmpty || replacement.creationKey.isEmpty ||
      replacement.firstRequest.isEmpty || replacement.nextRevision.document.isEmpty ||
      replacement.nextRevision.composite.isEmpty ||
      replacement.nextRevision.document = current.snapshot.revision.document ||
      (current :: assignments.retained).any (fun prior =>
        prior.id == replacement.nextId || prior.creationKey == replacement.creationKey ||
          prior.rootRequest == replacement.firstRequest ||
          prior.snapshot.revision.document == replacement.nextRevision.document) then none
  else
    let post ← start? { s with submission := ⟨false, false, false, false⟩ }
      requested objective budget facts commit replacement.nextId
    let fresh : Assignment :=
      { id := replacement.nextId, creationKey := replacement.creationKey
        snapshot :=
          { revision := replacement.nextRevision, objective := canonicalObjective objective
            execution :=
              { goal := ⟨.active, 0, false, false⟩, sequence := 0
                lastContinuedFrom := none, latestRequest := 0, children := []
                tokensUsed := 0, tokenBudget := budget.map Int.toNat } }
        rootRequest := replacement.firstRequest, usageBaseline := replacement.sessionUsage
        activeSeconds := 0 }
    some (post, ⟨fresh, current :: assignments.retained⟩)

theorem replacement_keeps_prior_assignments (s : State) (assignments : Assignments)
    (replacement : Replacement) (requested : AgentSession.Document)
    (objective : String) (budget : Option Int) (facts : Facts) (commit : Bool)
    (post : State × Assignments)
    (h : replace? s assignments replacement requested objective budget facts commit = some post) :
    post.2.retained = assignments.current :: assignments.retained := by
  dsimp only [replace?] at h
  split at h
  · contradiction
  · cases hs : start? { s with submission := ⟨false, false, false, false⟩ }
        requested objective budget facts commit replacement.nextId with
    | none => simp [hs] at h
    | some started =>
      simp only [hs, Option.bind_some, Option.some.injEq] at h
      cases h
      simp

theorem replacement_resets_only_new_accounting (s : State) (assignments : Assignments)
    (replacement : Replacement) (requested : AgentSession.Document)
    (objective : String) (budget : Option Int) (facts : Facts) (commit : Bool)
    (post : State × Assignments)
    (h : replace? s assignments replacement requested objective budget facts commit = some post) :
    post.2.current.snapshot.execution.tokensUsed = 0 ∧
      post.2.current.activeSeconds = 0 ∧
      post.2.current.usageBaseline = replacement.sessionUsage ∧
      post.2.current.rootRequest = replacement.firstRequest := by
  dsimp only [replace?] at h
  split at h
  · contradiction
  · cases hs : start? { s with submission := ⟨false, false, false, false⟩ }
        requested objective budget facts commit replacement.nextId with
    | none => simp [hs] at h
    | some started =>
      simp only [hs, Option.bind_some, Option.some.injEq] at h
      cases h
      simp

theorem stale_replacement_cannot_publish (s : State) (assignments : Assignments)
    (replacement : Replacement) (requested : AgentSession.Document)
    (objective : String) (budget : Option Int) (facts : Facts) (commit : Bool)
    (h : assignments.current.snapshot.revision ≠ replacement.expected) :
    replace? s assignments replacement requested objective budget facts commit = none := by
  simp [replace?, h]

theorem running_goal_cannot_be_replaced (s : State) (assignments : Assignments)
    (replacement : Replacement) (requested : AgentSession.Document)
    (objective : String) (budget : Option Int) (facts : Facts) (commit : Bool)
    (h : replaceable assignments.current.snapshot.execution.goal.status = false) :
    replace? s assignments replacement requested objective budget facts commit = none := by
  simp [replace?, h]

theorem discarded_replacement_cannot_publish (s : State) (assignments : Assignments)
    (replacement : Replacement) (requested : AgentSession.Document)
    (objective : String) (budget : Option Int) (facts : Facts) :
    replace? s assignments replacement requested objective budget facts false = none := by
  simp [replace?, discarded_start_is_not_published]

end GoalAutomation.ClientStart
