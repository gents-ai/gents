import Proofs.GoalAutomation
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

/-- The first ordinary request has already passed the existing signature,
admission and configuration owners. A fresh client goal starts only while
the whole session is idle, so its usage baseline excludes earlier work.
The native transaction commits the reserved session, Goal, creation claim,
first request and signed command receipt together. -/
def start? (s : State) (requested : AgentSession.Document) (objective : String)
    (budget : Option Int) (facts : Facts) (commit : Bool) : Option State := do
  if !commit ||
      !AgentSession.CommandAuthority.authorized facts.authority facts.hasPeer facts.now facts.expiresAt ||
      !facts.firstRequestValid || facts.unfinishedRequest ||
      facts.currentSessionDocument != facts.expectedSessionDocument ||
      s.submission.durableGoal || s.submission.runnableRequest ||
      !creationAllowed requested objective budget facts.existingClaim then none
  else
    let session ← AgentSession.ensure? s.session requested
    let stagedGoal := submissionStep s.submission .stageGoal
    let stagedRequest := submissionStep stagedGoal .stageRequest
    some ⟨some session, submissionStep stagedRequest .commit⟩

theorem discarded_start_is_not_published (s : State) (requested : AgentSession.Document)
    (objective : String) (budget : Option Int) (facts : Facts) :
    start? s requested objective budget facts false = none := by simp [start?]

theorem busy_session_cannot_start (s : State) (requested : AgentSession.Document)
    (objective : String) (budget : Option Int) (facts : Facts) (commit : Bool)
    (h : facts.unfinishedRequest = true) :
    start? s requested objective budget facts commit = none := by simp [start?, h]

theorem publication_has_reserved_session_and_goal (s post : State)
    (requested : AgentSession.Document) (objective : String) (budget : Option Int)
    (facts : Facts) (commit : Bool)
    (h : start? s requested objective budget facts commit = some post) :
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

end GoalAutomation.ClientStart
