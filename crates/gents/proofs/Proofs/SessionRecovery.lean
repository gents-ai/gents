import Proofs.Request
import Proofs.AgentSession
import Mathlib.Data.Finset.Basic

namespace SessionRetry

structure Request where
  state : RequestState
  origin : ExecutionOrigin
  admission : AdmissionState
  deadline : Option Time
  currentTime : Time
  retryCount : Nat
  maxRetries : Nat

def Request.deadlineExceeded (r : Request) : Prop :=
  ∃ deadline, r.deadline = some deadline ∧ deadline < r.currentTime

instance (r : Request) : Decidable r.deadlineExceeded := by
  unfold Request.deadlineExceeded
  cases r.deadline <;> simp <;> infer_instance

end SessionRetry

structure SessionState where
  sessionId : SessionId
  agentId : AgentId
  requestIds : Finset RequestId
  ctx : RequestId → SessionRetry.Request
  latest : RequestId

namespace SessionState

/-- Projection of fresh RequestSpec creation: no claim clocks, execution
budgets, lineage resets, or tool state are invented here. -/
def reissuedContext (ctx : SessionRetry.Request) : SessionRetry.Request :=
  { ctx with
    state := .pending
    admission := .released
    deadline := none
    retryCount := ctx.retryCount + 1 }

def CanReissue (pre : SessionState) (failedId newId : RequestId) : Prop :=
  failedId = pre.latest ∧ failedId ∈ pre.requestIds ∧ newId ∉ pre.requestIds ∧
    (pre.ctx failedId).state = .failed ∧ (pre.ctx failedId).admission = .released ∧
    (pre.ctx failedId).origin = .interactive ∧
    (pre.ctx failedId).retryCount < (pre.ctx failedId).maxRetries ∧
    ¬ (pre.ctx failedId).deadlineExceeded

instance (pre : SessionState) (failedId newId : RequestId) :
    Decidable (CanReissue pre failedId newId) := by
  unfold CanReissue
  infer_instance

inductive Action where
  | reissueFailed (failedId newId : RequestId)
  deriving DecidableEq, Repr

/-- Publishes a fresh successor within the same session. The transaction owner
resolves concurrent candidates through `claimRetry`; signatures and physical
retry-key transactions are adapter boundaries. -/
def step? (pre : SessionState) : Action → Option SessionState
  | .reissueFailed failedId newId =>
    if CanReissue pre failedId newId then
      some { pre with
        requestIds := insert newId pre.requestIds
        latest := newId
        ctx := Function.update pre.ctx newId (reissuedContext (pre.ctx failedId)) }
    else none

theorem reissue_requires_eligible_parent {pre post : SessionState} {failedId newId : RequestId}
    (h : step? pre (.reissueFailed failedId newId) = some post) :
    CanReissue pre failedId newId := by
  simp only [step?, Option.ite_none_right_eq_some, Option.some.injEq] at h
  exact h.1

theorem reissue_publishes_successor {pre post : SessionState} {failedId newId : RequestId}
    (h : step? pre (.reissueFailed failedId newId) = some post) :
    post.sessionId = pre.sessionId ∧ post.agentId = pre.agentId ∧
    post.latest = newId ∧ newId ∈ post.requestIds ∧ failedId ∈ post.requestIds ∧
    (post.ctx newId).state = .pending ∧ (post.ctx newId).admission = .released ∧
    (post.ctx newId).deadline = none ∧
    (post.ctx newId).origin = .interactive ∧
    (post.ctx newId).retryCount = (pre.ctx failedId).retryCount + 1 ∧
    (post.ctx newId).retryCount ≤ (post.ctx newId).maxRetries ∧
    (post.ctx failedId).state = .failed := by
  simp only [step?, Option.ite_none_right_eq_some, Option.some.injEq] at h
  obtain ⟨hc, rfl⟩ := h
  obtain ⟨_, hm, hn, hf, _, ho, hb, _⟩ := hc
  have hne : failedId ≠ newId := by intro he; exact hn (he ▸ hm)
  simp [reissuedContext, Function.update_of_ne hne,
    hm, hf, ho, Nat.succ_le_of_lt hb]

theorem reissue_preserves_parent_and_ceiling {pre post : SessionState} {failedId newId : RequestId}
    (h : step? pre (.reissueFailed failedId newId) = some post) :
    post.ctx failedId = pre.ctx failedId ∧
    (post.ctx newId).maxRetries = (pre.ctx failedId).maxRetries := by
  simp only [step?, Option.ite_none_right_eq_some, Option.some.injEq] at h
  obtain ⟨hc, rfl⟩ := h
  have hne : failedId ≠ newId := by
    intro he
    exact hc.2.2.1 (he ▸ hc.2.1)
  simp [Function.update_of_ne hne, reissuedContext]

theorem reissue_preserves_unrelated_request {pre post : SessionState} {failedId newId rid : RequestId}
    (h : step? pre (.reissueFailed failedId newId) = some post)
    (_hf : rid ≠ failedId) (hn : rid ≠ newId) : post.ctx rid = pre.ctx rid := by
  simp only [step?, Option.ite_none_right_eq_some, Option.some.injEq] at h
  obtain ⟨_, rfl⟩ := h
  simp [Function.update_of_ne, hn]

/-- Selection and successor creation run in one transaction: recheck actual rows
under exact requester scope and physical parent identity, never cached UI state. -/
def retryFromRows? (pre : SessionState) (session : AgentSession.Document)
    (rows : List AgentSession.RequestFact) (parentDoc failedId newId : Nat) : Option SessionState :=
  match AgentSession.latest rows session.scope.node session.scope.session
      (some session.scope.requester) with
  | none => none
  | some parent =>
    if parent.observed.docId = parentDoc ∧ parent.observed.requestId = failedId ∧
        parent.agent = session.agent ∧ pre.agentId = session.agent ∧
        pre.sessionId = session.scope.session ∧ parent.observed.state = (pre.ctx failedId).state ∧
        rows.all (fun row => row.observed.requestId != newId) then
      step? { pre with latest := parent.observed.requestId } (.reissueFailed failedId newId)
    else none

theorem retry_rejects_existing_id (pre : SessionState) (session : AgentSession.Document)
    (rows : List AgentSession.RequestFact) (parentDoc failedId newId : Nat)
    (existing : AgentSession.RequestFact) (hm : existing ∈ rows)
    (hid : existing.observed.requestId = newId) :
    retryFromRows? pre session rows parentDoc failedId newId = none := by
  have hnot : rows.all (fun row => row.observed.requestId != newId) = false := by
    simp only [List.all_eq_false]
    exact ⟨existing, hm, by simp [hid]⟩
  unfold retryFromRows?
  split <;> simp [hnot]

theorem observation_does_not_authorize_retry (pre : SessionState)
    (session : AgentSession.Document) (observation : Option AgentSession.Observation)
    (rows : List AgentSession.RequestFact) (parentDoc failedId newId : Nat) :
    retryFromRows? pre { session with observation } rows parentDoc failedId newId =
      retryFromRows? pre session rows parentDoc failedId newId := rfl

/-- The desktop owner keys a retry intent by the physical parent document, not a
new host/session identity. -/
structure RetryIntentState where
  successor : RequestId → Option RequestId

/-- Claim the one durable successor slot for `parent`: the first candidate wins;
later callers observe and return that same winner. -/
def claimRetry
    (state : RetryIntentState)
    (parent candidate : RequestId) : RetryIntentState × RequestId :=
  match state.successor parent with
  | some winner => (state, winner)
  | none =>
      ({ successor := Function.update state.successor parent (some candidate) }, candidate)

theorem claimRetry_records_winner
    (state : RetryIntentState)
    (parent candidate : RequestId) :
    (claimRetry state parent candidate).1.successor parent =
      some (claimRetry state parent candidate).2 := by
  simp [claimRetry]
  split <;> simp_all

theorem claimRetry_idempotent
    (state : RetryIntentState)
    (parent firstCandidate laterCandidate : RequestId) :
    claimRetry (claimRetry state parent firstCandidate).1 parent laterCandidate =
      ((claimRetry state parent firstCandidate).1,
        (claimRetry state parent firstCandidate).2) := by
  cases h_existing : state.successor parent with
  | none => simp [claimRetry, h_existing]
  | some winner => simp [claimRetry, h_existing]

end SessionState
