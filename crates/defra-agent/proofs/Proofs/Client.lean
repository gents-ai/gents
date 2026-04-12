import Proofs.Request

/-!
# Client Turn Observation

Formal model for how any client derives a deterministic view of a single
agent turn from observed documents.

The client projection is a pure function of document snapshots. It does
not depend on wall-clock time — server liveness proofs (L1, L3) guarantee
every request terminates. If a client perceives a "stall," that is a
transport problem, not a turn-state problem.

Imports `Proofs.Request` to reuse `RequestState` from the server model.
-/

/-- The 5 client-visible turn states. -/
inductive ClientTurnState where
  | waitingForClaim
  | streaming
  | completed
  | failed
  | superseded
  deriving DecidableEq, Repr

namespace ClientTurnState

/-- Client state ordering for monotonicity.
    Terminal states share rank 2 (incomparable). -/
def rank : ClientTurnState → Nat
  | .waitingForClaim => 0
  | .streaming       => 1
  | .completed       => 2
  | .failed          => 2
  | .superseded      => 2

/-- Whether a client turn state is terminal. -/
def isTerminal : ClientTurnState → Bool
  | .completed  => true
  | .failed     => true
  | .superseded => true
  | _           => false

instance : HasTerminal ClientTurnState where
  isTerminal s := s.isTerminal = true
  isTerminal_dec s := by
    cases s <;> simp [isTerminal] <;> infer_instance

end ClientTurnState

/-- Client-visible response status, read from AgentResponse.status. -/
inductive ResponseStatus where
  | streaming
  | complete
  | error
  deriving DecidableEq, Repr

/-- Snapshot of an AgentRequest as observed by the client.
    Only the fields that affect derivation are included. -/
structure RequestSnapshot where
  lifecycleState : RequestState
  isSuperseded : Bool
  deriving DecidableEq, Repr

/-- Snapshot of an AgentResponse as observed by the client.
    progressSeq is omitted — it orders response versions
    but does not affect the derivation result. -/
structure ResponseSnapshot where
  status : ResponseStatus
  deriving DecidableEq, Repr

/-- A single attempt observation: request + optional response. -/
structure AttemptView where
  request : RequestSnapshot
  response : Option ResponseSnapshot
  deriving DecidableEq, Repr

/-- Derive client turn state from a single attempt observation.

    Priority order:
    1. Supersession takes absolute precedence (cross-turn event).
    2. Server terminal lifecycle states override any response
       (terminal states are irreversible — proven in Request.lean).
    3. For non-terminal request states, response may be more current
       than the request under P2P replication lag. Trust the response.
    4. No response and non-terminal request → waitingForClaim. -/
def deriveAttempt : AttemptView → ClientTurnState
  | ⟨req, resp⟩ =>
    -- Supersession: cross-turn event, always takes precedence
    if req.isSuperseded then .superseded
    else match req.lifecycleState with
    -- Server terminal states override any stale response
    | .superseded    => .superseded
    | .completed     => .completed
    | .failed        => .failed
    | .dead          => .failed
    -- Non-terminal: response may be more current than request
    | .pending | .claimed | .processing | .inputRequired =>
      match resp with
      | some r =>
        match r.status with
        | .complete  => .completed
        | .error     => .failed
        | .streaming => .streaming
      | none => .waitingForClaim

/-- Derive client turn state from a full turn observation.

    The turn is a retry chain: a list of attempts ordered root-first,
    tip-last. The tip is the most recent attempt — the one the client
    should render.

    Returns `none` for empty observations (no turn exists). -/
def deriveTurn : List AttemptView → Option ClientTurnState
  | []          => none
  | [a]         => some (deriveAttempt a)
  | _ :: rest   => deriveTurn rest

/-- deriveTurn always returns the derivation of the last element. -/
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
      -- Now (head :: h' :: t') ++ [a] = head :: (h' :: t' ++ [a])
      -- and deriveTurn matches the `_ :: rest` case
      simp only [List.cons_append, deriveTurn]
      exact ih

/-! ## Theorem T4: Totality

    `deriveAttempt` is total by construction — it is a match expression
    with exhaustive coverage over `RequestState` and `Option ResponseSnapshot`.

    `deriveTurn` is total for non-empty attempt lists.
-/

/-- T4: deriveAttempt is total — defined for every possible AttemptView. -/
theorem deriveAttempt_total (view : AttemptView) :
    ∃ s : ClientTurnState, deriveAttempt view = s :=
  ⟨deriveAttempt view, rfl⟩

/-- T4: deriveTurn is defined for every non-empty attempt list. -/
theorem deriveTurn_total
    {attempts : List AttemptView}
    (h : attempts ≠ []) :
    ∃ s : ClientTurnState, deriveTurn attempts = some s := by
  induction attempts with
  | nil => contradiction
  | cons head tail ih =>
    cases tail with
    | nil => exact ⟨deriveAttempt head, rfl⟩
    | cons h' t' =>
      simp [deriveTurn]
      exact ih (by simp)

/-! ## Theorem T2: Monotonicity

    If the server transitions a request forward (valid `Transition` from
    `Proofs.Request`) while the response is held fixed, the client rank
    never decreases.

    For response advances (none → some, or status change toward terminal),
    the client rank also never decreases when the request is held fixed
    and non-terminal.
-/

/-- Helper: for non-terminal lifecycle states, deriveAttempt result depends
    only on the response (not which specific non-terminal state). -/
theorem deriveAttempt_nonterminal_response_driven
    {req : RequestSnapshot}
    {resp : Option ResponseSnapshot}
    (h_not_super : req.isSuperseded = false)
    (h_state : req.lifecycleState = .pending ∨ req.lifecycleState = .claimed ∨
               req.lifecycleState = .processing ∨ req.lifecycleState = .inputRequired) :
    deriveAttempt ⟨req, resp⟩ = match resp with
      | some r => match r.status with
        | .complete => .completed
        | .error => .failed
        | .streaming => .streaming
      | none => .waitingForClaim := by
  simp [deriveAttempt, h_not_super]
  rcases h_state with h | h | h | h <;> simp [h]

/-- Helper: valid server lifecycle state transitions (projecting RequestContext.Transition
    down to just the state component). -/
def LifecycleTransition : RequestState → RequestState → Prop
  | .pending,        .claimed         => True  -- claim
  | .pending,        .superseded      => True  -- dedup_lose
  | .claimed,        .processing      => True  -- begin_inference
  | .processing,     .processing      => True  -- advance (progressSeq++)
  | .processing,     .inputRequired   => True  -- need_input
  | .inputRequired,  .processing      => True  -- input_received
  | .processing,     .completed       => True  -- finish
  | .processing,     .failed          => True  -- fail
  | .claimed,        .failed          => True  -- fail_before_stream
  | .inputRequired,  .failed          => True  -- input_timeout
  | .failed,         .dead            => True  -- exhaust
  | .processing,     .dead            => True  -- deadline_expire
  | _,               _                => False

/-- Project RequestContext.Transition to a LifecycleTransition at the state level.

    Every server transition induces exactly one of the 12 pre/post state pairs
    enumerated in LifecycleTransition. -/
theorem transition_implies_lifecycle
    {pre post : RequestContext}
    (h : RequestContext.Transition pre post) :
    LifecycleTransition pre.state post.state := by
  cases h with
  | claim h_state _ h_post =>
    subst h_post; simp [LifecycleTransition, h_state]
  | dedup_lose h_state _ h_post =>
    subst h_post; simp [LifecycleTransition, h_state]
  | begin_inference h_state _ h_post =>
    subst h_post; simp [LifecycleTransition, h_state]
  | advance h_state _ h_post =>
    subst h_post; simp [LifecycleTransition, h_state]
  | need_input h_state _ h_post =>
    subst h_post; simp [LifecycleTransition, h_state]
  | input_received h_state _ h_post =>
    subst h_post; simp [LifecycleTransition, h_state]
  | finish h_state _ h_post =>
    subst h_post; simp [LifecycleTransition, h_state]
  | fail h_state _ h_post =>
    subst h_post; simp [LifecycleTransition, h_state]
  | fail_before_stream h_state _ h_post =>
    subst h_post; simp [LifecycleTransition, h_state]
  | input_timeout h_state _ _ h_post =>
    subst h_post; simp [LifecycleTransition, h_state]
  | exhaust h_state _ h_post =>
    subst h_post; simp [LifecycleTransition, h_state]
  | deadline_expire h_state _ _ h_post =>
    subst h_post; simp [LifecycleTransition, h_state]

/-- T2: A valid server lifecycle state transition never decreases the client rank
    when the response and supersession flag are held fixed.

    Structured as an explicit 12-arm case analysis matching the constructors of
    `LifecycleTransition`, in the same style as `transition_implies_lifecycle`
    above and the `Transition` case-splits in `Request.lean`. The 52 invalid
    `(pre_state, post_state)` combinations are discharged up front by reducing
    `h_trans` to `False`; the 12 valid arms are then closed one at a time.

    Writing each arm explicitly keeps future breakage (changes to
    `deriveAttempt` or `LifecycleTransition`) localized to a specific arm
    rather than surfacing as a confusing failure inside an `all_goals` block.

    The 12 valid (pre_state, post_state) pairs:
      pending → claimed (claim): rank 0→0 (both non-terminal, response-driven)
      pending → superseded (dedup_lose): rank 0→2
      claimed → processing (begin_inference): rank 0→0 (both non-terminal)
      processing → processing (advance): identity (0→0 or 1→1)
      processing → inputRequired (need_input): rank 0→0 (both non-terminal)
      inputRequired → processing (input_received): rank 0→0 (both non-terminal)
      processing → completed (finish): rank ≤1 → 2
      processing → failed (fail): rank ≤1 → 2
      claimed → failed (fail_before_stream): rank 0→2
      inputRequired → failed (input_timeout): rank ≤1 → 2
      failed → dead (exhaust): rank 2→2 (dead maps to .failed)
      processing → dead (deadline_expire): rank ≤1 → 2
-/
theorem lifecycle_transition_monotonic
    {pre_state post_state : RequestState}
    (h_trans : LifecycleTransition pre_state post_state)
    (isSuperseded : Bool)
    (resp : Option ResponseSnapshot) :
    (deriveAttempt ⟨⟨post_state, isSuperseded⟩, resp⟩).rank ≥
    (deriveAttempt ⟨⟨pre_state, isSuperseded⟩, resp⟩).rank := by
  -- First dispose of the 52 invalid `(pre_state, post_state)` combinations
  -- by reducing `h_trans` to `False`. The 12 valid arms remain as named
  -- cases and are closed one by one below. Each arm uses the same tactic
  -- shape — split on `isSuperseded`, then on `resp` (and if `resp = some r`,
  -- on `r.status`) — but written explicitly so that future changes to
  -- `deriveAttempt` or `LifecycleTransition` produce a localized failure
  -- at the specific arm, not a confusing `all_goals` error.
  cases pre_state <;> cases post_state <;>
    try (simp [LifecycleTransition] at h_trans)
  case pending.claimed =>
    cases isSuperseded
    · cases resp with
      | none => simp [deriveAttempt, ClientTurnState.rank]
      | some r =>
        obtain ⟨status⟩ := r
        cases status <;> simp [deriveAttempt, ClientTurnState.rank]
    · simp [deriveAttempt, ClientTurnState.rank]
  case pending.superseded =>
    cases isSuperseded
    · cases resp with
      | none => simp [deriveAttempt, ClientTurnState.rank]
      | some r =>
        obtain ⟨status⟩ := r
        cases status <;> simp [deriveAttempt, ClientTurnState.rank]
    · simp [deriveAttempt, ClientTurnState.rank]
  case claimed.processing =>
    cases isSuperseded
    · cases resp with
      | none => simp [deriveAttempt, ClientTurnState.rank]
      | some r =>
        obtain ⟨status⟩ := r
        cases status <;> simp [deriveAttempt, ClientTurnState.rank]
    · simp [deriveAttempt, ClientTurnState.rank]
  case claimed.failed =>
    cases isSuperseded
    · cases resp with
      | none => simp [deriveAttempt, ClientTurnState.rank]
      | some r =>
        obtain ⟨status⟩ := r
        cases status <;> simp [deriveAttempt, ClientTurnState.rank]
    · simp [deriveAttempt, ClientTurnState.rank]
  case processing.processing =>
    cases isSuperseded
    · cases resp with
      | none => simp [deriveAttempt, ClientTurnState.rank]
      | some r =>
        obtain ⟨status⟩ := r
        cases status <;> simp [deriveAttempt, ClientTurnState.rank]
    · simp [deriveAttempt, ClientTurnState.rank]
  case processing.inputRequired =>
    cases isSuperseded
    · cases resp with
      | none => simp [deriveAttempt, ClientTurnState.rank]
      | some r =>
        obtain ⟨status⟩ := r
        cases status <;> simp [deriveAttempt, ClientTurnState.rank]
    · simp [deriveAttempt, ClientTurnState.rank]
  case processing.completed =>
    cases isSuperseded
    · cases resp with
      | none => simp [deriveAttempt, ClientTurnState.rank]
      | some r =>
        obtain ⟨status⟩ := r
        cases status <;> simp [deriveAttempt, ClientTurnState.rank]
    · simp [deriveAttempt, ClientTurnState.rank]
  case processing.failed =>
    cases isSuperseded
    · cases resp with
      | none => simp [deriveAttempt, ClientTurnState.rank]
      | some r =>
        obtain ⟨status⟩ := r
        cases status <;> simp [deriveAttempt, ClientTurnState.rank]
    · simp [deriveAttempt, ClientTurnState.rank]
  case processing.dead =>
    cases isSuperseded
    · cases resp with
      | none => simp [deriveAttempt, ClientTurnState.rank]
      | some r =>
        obtain ⟨status⟩ := r
        cases status <;> simp [deriveAttempt, ClientTurnState.rank]
    · simp [deriveAttempt, ClientTurnState.rank]
  case inputRequired.processing =>
    cases isSuperseded
    · cases resp with
      | none => simp [deriveAttempt, ClientTurnState.rank]
      | some r =>
        obtain ⟨status⟩ := r
        cases status <;> simp [deriveAttempt, ClientTurnState.rank]
    · simp [deriveAttempt, ClientTurnState.rank]
  case inputRequired.failed =>
    cases isSuperseded
    · cases resp with
      | none => simp [deriveAttempt, ClientTurnState.rank]
      | some r =>
        obtain ⟨status⟩ := r
        cases status <;> simp [deriveAttempt, ClientTurnState.rank]
    · simp [deriveAttempt, ClientTurnState.rank]
  case failed.dead =>
    cases isSuperseded
    · cases resp with
      | none => simp [deriveAttempt, ClientTurnState.rank]
      | some r =>
        obtain ⟨status⟩ := r
        cases status <;> simp [deriveAttempt, ClientTurnState.rank]
    · simp [deriveAttempt, ClientTurnState.rank]

/-- T2 (response direction): advancing the response from none to some never decreases
    rank when the request is held fixed at a non-terminal lifecycle state. -/
theorem response_advance_monotonic_none_to_some
    {req : RequestSnapshot}
    {resp : ResponseSnapshot}
    (h_not_super : req.isSuperseded = false)
    (h_nonterminal : req.lifecycleState = .pending ∨ req.lifecycleState = .claimed ∨
                     req.lifecycleState = .processing ∨ req.lifecycleState = .inputRequired) :
    (deriveAttempt ⟨req, some resp⟩).rank ≥
    (deriveAttempt ⟨req, none⟩).rank := by
  rw [deriveAttempt_nonterminal_response_driven h_not_super h_nonterminal,
      deriveAttempt_nonterminal_response_driven h_not_super h_nonterminal]
  cases resp.status <;> simp [ClientTurnState.rank]

/-- T2 (response direction): streaming → complete/error never decreases rank. -/
theorem response_advance_monotonic_streaming_to_terminal
    {req : RequestSnapshot}
    {resp_new : ResponseSnapshot}
    (h_not_super : req.isSuperseded = false)
    (h_nonterminal : req.lifecycleState = .pending ∨ req.lifecycleState = .claimed ∨
                     req.lifecycleState = .processing ∨ req.lifecycleState = .inputRequired)
    (h_terminal : resp_new.status = .complete ∨ resp_new.status = .error) :
    (deriveAttempt ⟨req, some resp_new⟩).rank ≥
    (deriveAttempt ⟨req, some ⟨.streaming⟩⟩).rank := by
  rw [deriveAttempt_nonterminal_response_driven h_not_super h_nonterminal,
      deriveAttempt_nonterminal_response_driven h_not_super h_nonterminal]
  rcases h_terminal with h | h <;> simp [h, ClientTurnState.rank]

/-! ## Theorem T3: Terminal Coherence

    The client view is terminal iff the server request is effectively
    terminal. "Effectively terminal" means:
    - The request is superseded (isSuperseded = true), OR
    - The lifecycle state is terminal (completed/failed/superseded/dead), OR
    - The response status is terminal (complete/error)

    The third disjunct captures replication-lag tolerance: when the
    response has advanced past the request, the client should still
    correctly identify the turn as terminal.
-/

/-- Whether a request/response pair is effectively terminal from the
    server's perspective, accounting for replication lag. -/
def effectivelyTerminal (view : AttemptView) : Prop :=
  view.request.isSuperseded = true ∨
  view.request.lifecycleState = .completed ∨
  view.request.lifecycleState = .failed ∨
  view.request.lifecycleState = .superseded ∨
  view.request.lifecycleState = .dead ∨
  (∃ r, view.response = some r ∧ (r.status = .complete ∨ r.status = .error))

instance (view : AttemptView) : Decidable (effectivelyTerminal view) := by
  unfold effectivelyTerminal
  infer_instance

/-- T3: The client view is terminal iff the attempt is effectively terminal. -/
theorem terminal_coherence (view : AttemptView) :
    (deriveAttempt view).isTerminal = true ↔ effectivelyTerminal view := by
  obtain ⟨req, resp⟩ := view
  constructor
  · -- Forward: client terminal → effectively terminal
    intro h_client_term
    unfold effectivelyTerminal
    -- Case split directly on the isSuperseded boolean
    cases h_super : req.isSuperseded
    · -- isSuperseded = false
      simp [deriveAttempt, h_super] at h_client_term
      cases h_lc : req.lifecycleState
      · -- pending: consult response
        simp [h_lc] at h_client_term
        right; right; right; right; right
        cases resp with
        | none => simp [ClientTurnState.isTerminal] at h_client_term
        | some r =>
          refine ⟨r, rfl, ?_⟩
          cases h_status : r.status
          · simp [h_status, ClientTurnState.isTerminal] at h_client_term
          · exact Or.inl rfl
          · exact Or.inr rfl
      · -- claimed: consult response
        simp [h_lc] at h_client_term
        right; right; right; right; right
        cases resp with
        | none => simp [ClientTurnState.isTerminal] at h_client_term
        | some r =>
          refine ⟨r, rfl, ?_⟩
          cases h_status : r.status
          · simp [h_status, ClientTurnState.isTerminal] at h_client_term
          · exact Or.inl rfl
          · exact Or.inr rfl
      · -- processing: consult response
        simp [h_lc] at h_client_term
        right; right; right; right; right
        cases resp with
        | none => simp [ClientTurnState.isTerminal] at h_client_term
        | some r =>
          refine ⟨r, rfl, ?_⟩
          cases h_status : r.status
          · simp [h_status, ClientTurnState.isTerminal] at h_client_term
          · exact Or.inl rfl
          · exact Or.inr rfl
      · -- inputRequired: consult response
        simp [h_lc] at h_client_term
        right; right; right; right; right
        cases resp with
        | none => simp [ClientTurnState.isTerminal] at h_client_term
        | some r =>
          refine ⟨r, rfl, ?_⟩
          cases h_status : r.status
          · simp [h_status, ClientTurnState.isTerminal] at h_client_term
          · exact Or.inl rfl
          · exact Or.inr rfl
      · -- completed: terminal lifecycle
        right; left; rfl
      · -- failed: terminal lifecycle
        right; right; left; rfl
      · -- superseded: terminal lifecycle
        right; right; right; left; rfl
      · -- dead: terminal lifecycle
        right; right; right; right; left; rfl
    · -- isSuperseded = true
      exact Or.inl rfl
  · -- Backward: effectively terminal → client terminal
    intro h_eff
    cases h_super : req.isSuperseded
    · -- isSuperseded = false
      rcases h_eff with h_super' | h_lc_comp | h_lc_fail | h_lc_super | h_lc_dead | ⟨r, h_resp, h_status⟩
      · -- isSuperseded = true contradicts h_super = false
        simp only at h_super'
        rw [h_super] at h_super'
        exact absurd h_super' (by simp)
      · simp only at h_lc_comp
        simp [deriveAttempt, h_super, h_lc_comp, ClientTurnState.isTerminal]
      · simp only at h_lc_fail
        simp [deriveAttempt, h_super, h_lc_fail, ClientTurnState.isTerminal]
      · simp only at h_lc_super
        simp [deriveAttempt, h_super, h_lc_super, ClientTurnState.isTerminal]
      · simp only at h_lc_dead
        simp [deriveAttempt, h_super, h_lc_dead, ClientTurnState.isTerminal]
      · -- Response terminal: case on lifecycle state
        simp only at h_resp
        simp only [deriveAttempt, h_super, if_false]
        cases h_lc : req.lifecycleState <;> simp [h_lc, ClientTurnState.isTerminal] <;>
          (rw [h_resp]; rcases h_status with h | h <;> simp [h, ClientTurnState.isTerminal])
    · -- isSuperseded = true
      simp [deriveAttempt, h_super, ClientTurnState.isTerminal]
