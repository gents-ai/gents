import Proofs.Request

inductive ClientTurnState where
  | waitingForClaim
  | running
  | completed
  | failed
  | superseded
  | interrupted
  deriving DecidableEq, Repr

namespace ClientTurnState

def rank : ClientTurnState → Nat
  | .waitingForClaim => 0
  | .running         => 1
  | .completed       => 2
  | .failed          => 2
  | .superseded      => 2
  | .interrupted     => 2

def isTerminal : ClientTurnState → Bool
  | .completed   => true
  | .failed      => true
  | .superseded  => true
  | .interrupted => true
  | _            => false

instance : HasTerminal ClientTurnState where
  isTerminal s := s.isTerminal = true
  isTerminal_dec s := by
    cases s <;> simp [isTerminal] <;> infer_instance

end ClientTurnState

structure RequestSnapshot where
  lifecycleState : RequestState
  isSuperseded : Bool
  deriving DecidableEq, Repr

structure AttemptView where
  request : RequestSnapshot
  deriving DecidableEq, Repr

/-- Retry identity is carried beside the request-only projection so the latter
    remains an exact lifecycle snapshot. `observationScope` names the caller's
    already authorized agent/requester/session observation scope. It is not a
    request document ID: every retry has its own physical request document.
    Lineage and graph validity remain premises established by the owner. -/
structure KeyedAttemptView where
  observationScope : String
  requestId : String
  retryParentRequest : Option String
  attempt : AttemptView
  deriving DecidableEq, Repr

def deriveAttempt : AttemptView → ClientTurnState
  | ⟨req⟩ =>
    if req.isSuperseded then .superseded
    else match req.lifecycleState with
    | .superseded    => .superseded
    | .completed     => .completed
    | .failed        => .failed
    | .dead          => .failed
    | .interrupted   => .interrupted
    | .workspaceBindingPending | .pending => .waitingForClaim
    | .claimed | .processing | .waitingForBackend | .resumingBackend => .running

structure ClientHeadProjection where
  turnState : ClientTurnState
  requestState : RequestState
  deriving DecidableEq, Repr

def projectHead (view : AttemptView) : ClientHeadProjection :=
  { turnState := deriveAttempt view
  , requestState := view.request.lifecycleState
  }

def ClientHeadProjection.isTerminal (head : ClientHeadProjection) : Bool :=
  head.turnState.isTerminal

def ClientHeadProjection.isActive (head : ClientHeadProjection) : Bool :=
  !head.isTerminal

theorem projectHead_turnState (view : AttemptView) :
    (projectHead view).turnState = deriveAttempt view := rfl

theorem projectHead_requestState (view : AttemptView) :
    (projectHead view).requestState = view.request.lifecycleState := rfl

theorem projectHead_terminal (view : AttemptView) :
    (projectHead view).isTerminal = (deriveAttempt view).isTerminal := rfl

/-- `attempts` is an already validated retry chain in oldest-to-newest order.
    This function deliberately models only selection from that ordered chain: it
    does not prove the store-level uniqueness of the retry tip. -/
def deriveTurn : List AttemptView → Option ClientTurnState
  | []          => none
  | [a]         => some (deriveAttempt a)
  | _ :: rest   => deriveTurn rest

theorem deriveTurn_append_singleton
    (attempts : List AttemptView)
    (a : AttemptView) :
    deriveTurn (attempts ++ [a]) = some (deriveAttempt a) := by
  induction attempts with
  | nil => rfl
  | cons head tail ih =>
    cases tail with
    | nil => rfl
    | cons h' t' =>
      simp only [List.cons_append, deriveTurn]
      exact ih

def attemptsInObservationScope
    (observationScope : String)
    (attempts : List KeyedAttemptView) : List KeyedAttemptView :=
  attempts.filter fun attempt => attempt.observationScope == observationScope

def retryParentIds (attempts : List KeyedAttemptView) : List String :=
  attempts.filterMap fun attempt => attempt.retryParentRequest

def retryTips
    (observationScope : String)
    (attempts : List KeyedAttemptView) : List KeyedAttemptView :=
  let scopedAttempts := attemptsInObservationScope observationScope attempts
  let parents := retryParentIds scopedAttempts
  scopedAttempts.filter fun attempt => !(parents.contains attempt.requestId)

/-- Resolve an unordered observation set only when its parent-ID filter has one
    candidate tip. Zero-tip sets (including a closed cycle) and multiple-tip
    ambiguity are rejected. This does not independently validate graph topology. -/
def resolveRetryTip
    (observationScope : String)
    (attempts : List KeyedAttemptView) : Option KeyedAttemptView :=
  match retryTips observationScope attempts with
  | [tip] => some tip
  | _ => none

def deriveUnorderedTurn
    (observationScope : String)
    (attempts : List KeyedAttemptView) : Option ClientTurnState :=
  (resolveRetryTip observationScope attempts).map fun tip => deriveAttempt tip.attempt

/-! ### Selected-session delivery

A client that selects a session it has just sent into reads the node's
canonical `AgentSession` row, which the node creates when it claims the
request. Until that row replicates back, the read derives what the client
shows from four signals it already holds and nothing else: no clock, no
delivery receipt. A missing row is therefore a delivery state, not a read
failure; only a row that breaks the node/session scope is an error. -/

/-- Signal 2: whether the client-to-runtime pairing for the target node
    replicates, as the client sync owner reports it: the peer dialed and its
    client-to-runtime route is applied and matches the live replicator.
    Operator reads commit and read the request in the node's own database, so
    no pairing leg separates that request from the node; they supply
    `connected`. -/
inductive PairingSignal where
  | connected
  | notConnected
  deriving DecidableEq, Repr

/-- Signal 4: the canonical `AgentSession` rows a selected-session read found
    for its exact node/session scope. -/
inductive SessionRowObservation where
  | absent
  | canonical
  | duplicate
  | outOfScope
  deriving DecidableEq, Repr

/-- Signals 1 and 3: the `AgentRequest` this client wrote, whose presence
    means sending, with the node runtime's replicated `claimed_at`. After
    submission only the node runtime writes `claimed_at` or advances
    `lifecycleState`, so either one is the node's own evidence that it holds
    the request. -/
structure SubmittedAttempt extends AttemptView where
  claimed : Bool
  deriving DecidableEq, Repr

/-- The lifecycle states past the pending states a request is written in:
    exactly those `deriveAttempt` does not map to `waitingForClaim`
    (`lifecyclePastPending_iff_not_waiting`). -/
def lifecyclePastPending : RequestState → Bool
  | .workspaceBindingPending | .pending => false
  | _ => true

/-- Signal 3: the node runtime holds the request. -/
def SubmittedAttempt.nodeHasIt (attempt : SubmittedAttempt) : Bool :=
  attempt.claimed || lifecyclePastPending attempt.request.lifecycleState

structure SessionDeliveryView where
  submitted : Option SubmittedAttempt
  pairing : PairingSignal
  sessionRow : SessionRowObservation
  deriving DecidableEq, Repr

inductive SessionDelivery where
  | visible
  | accepted
  | delivering
  | notConnected
  | noRequest
  | integrityFault
  deriving DecidableEq, Repr

/-- The read reports an error instead of a delivery state. -/
def SessionDelivery.isError : SessionDelivery → Bool
  | .noRequest | .integrityFault => true
  | _ => false

/-- The canonical row decides first; without it, the node's own evidence,
    then the pairing, decide how far the local request has travelled. -/
def deriveSessionDelivery (view : SessionDeliveryView) : SessionDelivery :=
  match view.sessionRow, view.submitted with
  | .canonical, _ => .visible
  | .duplicate, _ => .integrityFault
  | .outOfScope, _ => .integrityFault
  | .absent, none => .noRequest
  | .absent, some attempt =>
    if attempt.nodeHasIt then .accepted
    else match view.pairing with
      | .connected => .delivering
      | .notConnected => .notConnected

/-- The four signals: whether a request was sent and, if so, whether the node
    holds it; the pairing; and the session row. -/
def SessionDeliveryView.signals (view : SessionDeliveryView) :
    Option Bool × PairingSignal × SessionRowObservation :=
  (view.submitted.map SubmittedAttempt.nodeHasIt, view.pairing, view.sessionRow)

theorem lifecyclePastPending_iff_not_waiting (state : RequestState) :
    lifecyclePastPending state = false ↔ deriveAttempt ⟨⟨state, false⟩⟩ = .waitingForClaim := by
  cases state <;> simp [lifecyclePastPending, deriveAttempt]

theorem canonical_row_wins
    (submitted : Option SubmittedAttempt) (pairing : PairingSignal) :
    deriveSessionDelivery ⟨submitted, pairing, .canonical⟩ = .visible := by
  cases submitted <;> rfl

theorem integrity_fault_is_an_error
    (submitted : Option SubmittedAttempt) (pairing : PairingSignal)
    (row : SessionRowObservation)
    (h : row = .duplicate ∨ row = .outOfScope) :
    deriveSessionDelivery ⟨submitted, pairing, row⟩ = .integrityFault := by
  rcases h with rfl | rfl <;> cases submitted <;> rfl

theorem sent_without_row_is_never_an_error
    (attempt : SubmittedAttempt) (pairing : PairingSignal) :
    (deriveSessionDelivery ⟨some attempt, pairing, .absent⟩).isError = false := by
  cases h : attempt.nodeHasIt <;> cases pairing <;>
    simp [deriveSessionDelivery, h, SessionDelivery.isError]

theorem unclaimed_without_row_is_in_flight
    (attempt : SubmittedAttempt) (pairing : PairingSignal)
    (h : attempt.nodeHasIt = false) :
    deriveSessionDelivery ⟨some attempt, pairing, .absent⟩ = .delivering ∨
      deriveSessionDelivery ⟨some attempt, pairing, .absent⟩ = .notConnected := by
  cases pairing <;> simp [deriveSessionDelivery, h]

theorem pairing_down_is_not_connected
    (attempt : SubmittedAttempt) (h : attempt.nodeHasIt = false) :
    deriveSessionDelivery ⟨some attempt, .notConnected, .absent⟩ = .notConnected := by
  simp [deriveSessionDelivery, h]

theorem pairing_up_is_delivering
    (attempt : SubmittedAttempt) (h : attempt.nodeHasIt = false) :
    deriveSessionDelivery ⟨some attempt, .connected, .absent⟩ = .delivering := by
  simp [deriveSessionDelivery, h]

theorem node_evidence_is_accepted
    (attempt : SubmittedAttempt) (pairing : PairingSignal)
    (h : attempt.nodeHasIt = true) :
    deriveSessionDelivery ⟨some attempt, pairing, .absent⟩ = .accepted := by
  simp [deriveSessionDelivery, h]

/-- The delivery states refine the request-only `waitingForClaim`: an
    unsuperseded, unclaimed request still waiting for its claim, with no
    session row, is in flight and never reported as an error. -/
theorem waiting_for_claim_without_row_is_in_flight
    (attempt : SubmittedAttempt) (pairing : PairingSignal)
    (h_unsuperseded : attempt.request.isSuperseded = false)
    (h_unclaimed : attempt.claimed = false)
    (h_waiting : deriveAttempt attempt.toAttemptView = .waitingForClaim) :
    deriveSessionDelivery ⟨some attempt, pairing, .absent⟩ = .delivering ∨
      deriveSessionDelivery ⟨some attempt, pairing, .absent⟩ = .notConnected := by
  apply unclaimed_without_row_is_in_flight
  obtain ⟨⟨⟨state, isSuperseded⟩⟩, claimed⟩ := attempt
  simp only at h_unsuperseded h_unclaimed
  subst h_unsuperseded h_unclaimed
  have h_pending : lifecyclePastPending state = false :=
    (lifecyclePastPending_iff_not_waiting state).mpr h_waiting
  simp [SubmittedAttempt.nodeHasIt, h_pending]

/-- The classification is a function of the four signals alone: views that
    agree on them agree on the result, whatever else their requests carry. -/
theorem deriveSessionDelivery_determined_by_signals
    {a b : SessionDeliveryView} (h : a.signals = b.signals) :
    deriveSessionDelivery a = deriveSessionDelivery b := by
  obtain ⟨sa, pa, ra⟩ := a
  obtain ⟨sb, pb, rb⟩ := b
  simp only [SessionDeliveryView.signals, Prod.mk.injEq] at h
  obtain ⟨hs, rfl, rfl⟩ := h
  cases ra
  · cases sa <;> cases sb <;> simp_all [deriveSessionDelivery]
  all_goals rfl
