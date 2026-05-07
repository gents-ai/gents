import Proofs.Client.Types

/-!
# Client Lifecycle Monotonicity

Projection of server request lifecycle transitions into monotonic client ranks.
-/

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
  cases req with
  | mk lifecycleState isSuperseded =>
    rcases h_state with h | h | h | h <;>
      cases h <;> cases h_not_super <;> rfl

/-- Helper: valid server lifecycle state transitions (projecting RequestContext.Transition
    down to just the state component). -/
def LifecycleTransition : RequestState → RequestState → Prop
  | .pending,        .claimed         => True  -- claim
  | .pending,        .superseded      => True  -- dedup_lose
  | .claimed,        .processing      => True  -- begin_inference
  | .processing,     .processing      => True  -- advance (progressSeq++)
  | .processing,     .completed       => True  -- finish
  | .processing,     .failed          => True  -- fail
  | .claimed,        .failed          => True  -- fail_before_stream
  | .pending,        .dead            => True  -- expire (TTL)
  | .pending,        .interrupted     => True  -- interrupt_before_claim
  | .claimed,        .interrupted     => True  -- interrupt_claimed
  | .processing,     .interrupted     => True  -- interrupt_processing
  | _,               _                => False

/-- Project RequestContext.Transition to a LifecycleTransition at the state level.

    Every server transition induces exactly one of the 11 pre/post state pairs
    enumerated in LifecycleTransition. -/
theorem transition_implies_lifecycle
    {pre post : RequestContext}
    (h : RequestContext.Transition pre post) :
    LifecycleTransition pre.state post.state := by
  cases h with
  | claim h_state _ _ h_post =>
    subst h_post; simp [LifecycleTransition, h_state]
  | dedup_lose h_state _ h_post =>
    subst h_post; simp [LifecycleTransition, h_state]
  | begin_inference h_state _ h_post =>
    subst h_post; simp [LifecycleTransition, h_state]
  | advance h_state _ h_post =>
    subst h_post; simp [LifecycleTransition, h_state]
  | finish h_state _ h_post =>
    subst h_post; simp [LifecycleTransition, h_state]
  | fail h_state _ h_post =>
    subst h_post; simp [LifecycleTransition, h_state]
  | fail_before_stream h_state _ h_post =>
    subst h_post; simp [LifecycleTransition, h_state]
  | expire h_state _ _ _ h_post =>
    subst h_post; simp [LifecycleTransition, h_state]
  | interrupt_before_claim h_state _ _ h_post =>
    subst h_post; simp [LifecycleTransition, h_state]
  | interrupt_claimed h_state _ _ h_post =>
    subst h_post; simp [LifecycleTransition, h_state]
  | interrupt_processing h_state _ _ h_post =>
    subst h_post; simp [LifecycleTransition, h_state]

/-- T2: A valid server lifecycle state transition never decreases the client rank
    when the response and supersession flag are held fixed.

    The 11 valid current-product (pre_state, post_state) pairs:
      pending → claimed (claim): rank 0→0 (both non-terminal, response-driven)
      pending → superseded (dedup_lose): rank 0→2
      claimed → processing (begin_inference): rank 0→0 (both non-terminal)
      processing → processing (advance): identity (0→0 or 1→1)
      processing → completed (finish): rank ≤1 → 2
      processing → failed (fail): rank ≤1 → 2
      claimed → failed (fail_before_stream): rank 0→2
      pending → dead (expire): rank 0→2
      pending → interrupted (interrupt_before_claim): rank 0→2
      claimed → interrupted (interrupt_claimed): rank 0→2
      processing → interrupted (interrupt_processing): rank ≤1→2

    `inputRequired` remains valid persisted vocabulary but has no current
    product transition in `LifecycleTransition`; clients still parse it and
    derive a non-terminal view if they encounter a reserved row.
-/
theorem lifecycle_transition_monotonic
    {pre_state post_state : RequestState}
    (h_trans : LifecycleTransition pre_state post_state)
    (isSuperseded : Bool)
    (resp : Option ResponseSnapshot) :
    (deriveAttempt ⟨⟨post_state, isSuperseded⟩, resp⟩).rank ≥
    (deriveAttempt ⟨⟨pre_state, isSuperseded⟩, resp⟩).rank := by
  cases pre_state <;> cases post_state <;>
    try (simp [LifecycleTransition] at h_trans)
  all_goals
    cases isSuperseded
    · cases resp with
      | none => simp [deriveAttempt, ClientTurnState.rank]
      | some r =>
        obtain ⟨status, _⟩ := r
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
    (deriveAttempt ⟨req, some ⟨.streaming, false⟩⟩).rank := by
  rw [deriveAttempt_nonterminal_response_driven h_not_super h_nonterminal,
      deriveAttempt_nonterminal_response_driven h_not_super h_nonterminal]
  rcases h_terminal with h | h <;> simp [h, ClientTurnState.rank]
