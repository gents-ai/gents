import Proofs.Session.Request.Executable

namespace RequestContext

theorem dead_transition_is_expired_preclaim {pre post : RequestContext}
    (h : Transition pre post) (dead : post.state = .dead) :
    pre.state = .pending ∧ ∃ t, pre.validUntil = some t ∧ pre.currentTime > t := by
  cases h <;> simp_all

/-- A pending row's `valid_until` as the request TTL parser reads it
(`parse_valid_until`): absent, a deadline not yet passed, a passed deadline,
or text that is not an RFC 3339 time. -/
inductive TtlObservation where
  | notSet
  | live
  | expired
  | malformed
  deriving DecidableEq, Repr

/-- What a pending row's TTL settles before it is claimed or refused. -/
inductive PendingDisposition where
  | expired
  | open
  deriving DecidableEq, Repr

/-- The settlement a pending row owes from its TTL, which every pending
refusal takes before writing its own terminal.

Decision: a passed TTL ends the row dead (Stale) whichever path observes it,
the claim (`transition_pending_to_dead_stale`) or a refusal, so both settle
the same row the same way (`claim_and_refusal_agree_on_settled_rows`).

Premise: `valid_until` is immutable on `AgentRequest`, so nothing races the
refusal's read of it. None of the concurrent writers of a pending row (a
claim, an interrupt, another refusal) writes `valid_until`; each moves
`lifecycle_state` by compare-and-set on `pending`. The refusal reads the row
by `_docID` inside the transaction that writes its terminal, by the same
compare-and-set; that compare-and-set is the fence, so the race needs no
model of its own.

Decision: a malformed TTL settles nothing; it proves neither expiry nor
freshness. The claim, which would start work, fails closed on it
(`claimPrecheck`). A refusal starts no work, so its own terminal stands
rather than leaving a row no runtime can claim pending, refused again on
every route. -/
def pendingDisposition (ttl : TtlObservation) : PendingDisposition :=
  match ttl with
  | .expired => .expired
  | .notSet | .live | .malformed => .open

namespace PendingDisposition

/-- The state a pending refusal writes: the claim's dead for a passed TTL,
or the refusal's own `failed`. -/
def refusalState : PendingDisposition → RequestState
  | .expired => .dead
  | .open => .failed

/-- Whether the refusal's session projection
(`project_refused_request_session_in_txn`) runs. It runs only with the
refusal's own `failed` terminal; a passed TTL projects none, as the claim's
dead path projects none. Whether it may create a session is not decided
here. -/
def refusalProjectsSession : PendingDisposition → Bool
  | .open => true
  | .expired => false

/-- The executable request action a pending refusal performs. -/
def refusalAction : PendingDisposition → Action
  | .expired => .expire
  | .open => .admissionReject

end PendingDisposition

/-- The claim path's decision on a pending row's TTL before its claim
transaction (`claim_pending_durable_with_inputs`). -/
inductive ClaimPrecheck where
  | expire
  | failClosed
  | claim
  deriving DecidableEq, Repr

/-- A passed TTL ends the row dead, a malformed one fails closed and leaves
it pending, any other is claimed. -/
def claimPrecheck (ttl : TtlObservation) : ClaimPrecheck :=
  match ttl with
  | .expired => .expire
  | .malformed => .failClosed
  | .notSet | .live => .claim

def ClaimPrecheck.terminal? : ClaimPrecheck → Option RequestState
  | .expire => some .dead
  | .failClosed | .claim => none

/-- The TTL observation of a modelled request, whose deadline is always a
time; a malformed TTL exists only on the wire. -/
def ttlObservation (r : RequestContext) : TtlObservation :=
  match r.validUntil with
  | none => .notSet
  | some t => if r.currentTime > t then .expired else .live

theorem refusal_of_an_expired_request_is_stale :
    (pendingDisposition .expired).refusalState = .dead := rfl

theorem malformed_ttl_settles_nothing : pendingDisposition .malformed = .open := rfl

/-- Only the refusal's own failure reaches the session projection; an
expired outcome projects no session. -/
theorem refusal_projects_only_its_failure (d : PendingDisposition) :
    d.refusalProjectsSession = true ↔ d.refusalState = .failed := by
  cases d <;> simp [PendingDisposition.refusalState, PendingDisposition.refusalProjectsSession]

/-- A claim settles a row exactly when a refusal would, and to the same
terminal. -/
theorem claim_and_refusal_agree_on_settled_rows (ttl : TtlObservation) :
    (claimPrecheck ttl).terminal? =
      if pendingDisposition ttl = .open then none
      else some (pendingDisposition ttl).refusalState := by
  cases ttl <;> rfl

/-- A refusal of an observed pending row is a legal request transition, and
it reaches the state the refusal writes. -/
theorem refusal_is_a_request_transition (r : RequestContext)
    (pending : r.state = .pending) (released : r.admission = .released) :
    ∃ post, step? r (pendingDisposition (ttlObservation r)).refusalAction = some post ∧
      post.state = (pendingDisposition (ttlObservation r)).refusalState := by
  cases ttl : r.validUntil with
  | none =>
      simp [pendingDisposition, ttlObservation, PendingDisposition.refusalAction,
        PendingDisposition.refusalState, step?, pending, released, ttl]
  | some t =>
      by_cases passed : r.currentTime > t
      · simp [pendingDisposition, ttlObservation, PendingDisposition.refusalAction,
          PendingDisposition.refusalState, step?, pending, released, ttl, passed]
      · simp [pendingDisposition, ttlObservation, PendingDisposition.refusalAction,
          PendingDisposition.refusalState, step?, pending, released, ttl, passed]

end RequestContext
