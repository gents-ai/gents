import Proofs.Triggers
import Proofs.Request
import Proofs.Properties.Safety

/-!
# Conformance Mapping: Trigger Layer -> Request Lifecycle

Bridges the trigger-engine proof layer (`Proofs/Triggers.lean`) to the
request-lifecycle model (`Proofs/Request.lean`).

The trigger layer intentionally works with a thin request projection:

* `AgentRequest.causedBy`
* `AgentRequest.concurrency`
* `AgentRequest.isTerminal`
* `AgentRequest.executionOrigin`

This file relates that projection to the richer lifecycle model carried
by `RequestContext`. The key cross-layer relation is
`TriggerLifecycleCoherent`, which says:

* the trigger-layer terminal bit agrees with lifecycle terminality
* the trigger-layer execution origin matches the lifecycle origin

Together with `syncTriggerTerminal`, this gives a lightweight
"observational view" theorem: once a trigger-created request is related
to a lifecycle request, lifecycle transitions preserve the trigger
fields we care about.
-/

/-- Bool projection of lifecycle terminality into the trigger layer. -/
def requestStateToTriggerTerminal : RequestState → Bool
  | .completed => true
  | .failed => true
  | .superseded => true
  | .dead => true
  | .interrupted => true
  | .pending => false
  | .claimed => false
  | .processing => false
  | .inputRequired => false

/-- The Bool projection is definitionally coherent with `HasTerminal`. -/
theorem requestStateToTriggerTerminal_eq_true_iff (rs : RequestState) :
    requestStateToTriggerTerminal rs = true ↔ isTerminal rs := by
  cases rs <;> simp [requestStateToTriggerTerminal, HasTerminal.isTerminal, RequestState.instHasTerminal]

/-- The false branch is exactly non-terminality at the lifecycle layer. -/
theorem requestStateToTriggerTerminal_eq_false_iff (rs : RequestState) :
    requestStateToTriggerTerminal rs = false ↔ ¬ isTerminal rs := by
  cases rs <;> simp [requestStateToTriggerTerminal, HasTerminal.isTerminal, RequestState.instHasTerminal]

/--
Cross-layer coherence between a trigger-layer `AgentRequest` and a lifecycle
`RequestContext`.

This is intentionally thin: it only relates the fields the trigger layer
observes directly.
-/
def TriggerLifecycleCoherent (rTrig : AgentRequest) (rReq : RequestContext) : Prop :=
  rTrig.isTerminal = requestStateToTriggerTerminal rReq.state ∧
  rTrig.executionOrigin = rReq.origin

/-- Terminal observations in the trigger layer coincide with lifecycle terminality. -/
theorem triggerLifecycleCoherent_terminal_iff
    {rTrig : AgentRequest} {rReq : RequestContext}
    (h : TriggerLifecycleCoherent rTrig rReq) :
    rTrig.isTerminal = true ↔ isTerminal rReq.state := by
  rcases h with ⟨h_terminal, _⟩
  rw [h_terminal]
  exact requestStateToTriggerTerminal_eq_true_iff rReq.state

/-- The coherence relation directly exposes origin equality. -/
theorem triggerLifecycleCoherent_origin_eq
    {rTrig : AgentRequest} {rReq : RequestContext}
    (h : TriggerLifecycleCoherent rTrig rReq) :
    rTrig.executionOrigin = rReq.origin :=
  h.2

/--
The concrete request shape materialized by `dispatchStep` before any later
lifecycle evolution updates its terminal bit.

This mirrors the record assembled in `Proofs/Triggers.lean::dispatchStep`.
-/
def materializedTriggerRequest
    (state : SystemState)
    (intent : FireIntent)
    (seed : RequestSeed) : AgentRequest :=
  { id := s!"dispatched-{state.requests.length}"
  , causedBy :=
      match seed.causedByTriggerId with
      | none => none
      | some tid => some (tid, seed.causedByTriggerKind)
  , concurrency := intent.concurrency
  , isTerminal := false
  , executionOrigin :=
      match seed.causedByTriggerKind with
      | .manual => .interactive
      | .schedule | .event => .scheduled }

/-- Trigger-created requests enter the lifecycle as non-terminal observations. -/
theorem materializedTriggerRequest_nonterminal
    (state : SystemState) (intent : FireIntent) (seed : RequestSeed) :
    (materializedTriggerRequest state intent seed).isTerminal = false := by
  rfl

/-- The materialized trigger request's origin is determined solely by trigger kind. -/
theorem materializedTriggerRequest_origin
    (state : SystemState) (intent : FireIntent) (seed : RequestSeed) :
    (materializedTriggerRequest state intent seed).executionOrigin =
      match seed.causedByTriggerKind with
      | .manual => .interactive
      | .schedule | .event => .scheduled := by
  rfl

/--
If `dispatch` materializes `seed`, the origin assigned to the corresponding
trigger request is lineage-consistent in the sense of `T4`.

The manual case depends on `dispatch`'s normalization theorem that forces
manual lineage ids to `none`.
-/
theorem dispatch_materializedTriggerRequest_consistentLineage
    (state : SystemState)
    (snap : TriggerSnapshot)
    (intent : FireIntent)
    (seed : RequestSeed)
    (h_dispatch : dispatch snap intent = some seed) :
    consistentLineage seed (materializedTriggerRequest state intent seed).executionOrigin := by
  match h_kind : seed.causedByTriggerKind with
  | .manual =>
      have h_none : seed.causedByTriggerId = none :=
        dispatch_manual_lineage_id_is_none snap intent seed h_dispatch h_kind
      simp [consistentLineage, materializedTriggerRequest, h_kind, h_none]
  | .schedule =>
      simp [consistentLineage, materializedTriggerRequest, h_kind]
  | .event =>
      simp [consistentLineage, materializedTriggerRequest, h_kind]

/--
Creation-time coherence: a newly materialized trigger request is coherent with
any lifecycle context already known to be at `.claimed` with the same origin.

This matches the scheduler conformance story: trigger-created work enters the
request lifecycle as claimed work, with origin fixed by the trigger engine.
-/
theorem materializedTriggerRequest_coherent_with_claimed_context
    (state : SystemState)
    (intent : FireIntent)
    (seed : RequestSeed)
    (ctx : RequestContext)
    (h_state : ctx.state = .claimed)
    (h_origin : (materializedTriggerRequest state intent seed).executionOrigin = ctx.origin) :
    TriggerLifecycleCoherent (materializedTriggerRequest state intent seed) ctx := by
  unfold TriggerLifecycleCoherent
  constructor
  · simp [materializedTriggerRequest, requestStateToTriggerTerminal, h_state]
  · exact h_origin

/-- Non-trigger lifecycle fields needed to embed a materialized trigger request. -/
structure ClaimedEmbeddingInputs where
  backend : BackendId
  deadline : Time
  claimTime : Time
  currentTime : Time
  retryCount : Nat
  maxRetries : Nat
  progressSeq : Nat
  messageSeq : Nat
  isLatest : Bool
  persistence : PersistenceState
  interruptRequestedAt : Option Time
  validUntil : Option Time

/-- Canonical lifecycle context witnessing a claimed embedding for a trigger request. -/
def claimedEmbeddingContext
    (state : SystemState)
    (intent : FireIntent)
    (seed : RequestSeed)
    (inputs : ClaimedEmbeddingInputs) : RequestContext :=
  { state := .claimed
  , origin := (materializedTriggerRequest state intent seed).executionOrigin
  , backend := inputs.backend
  , admission := .waiting
  , deadline := inputs.deadline
  , claimTime := inputs.claimTime
  , currentTime := inputs.currentTime
  , retryCount := inputs.retryCount
  , maxRetries := inputs.maxRetries
  , progressSeq := inputs.progressSeq
  , messageSeq := inputs.messageSeq
  , isLatest := inputs.isLatest
  , persistence := inputs.persistence
  , interruptRequestedAt := inputs.interruptRequestedAt
  , validUntil := inputs.validUntil }

/--
Every materialized trigger request has a canonical embedding into the claimed
lifecycle state once the non-trigger fields are supplied.

The trigger layer does not own backend selection, persistence bookkeeping, or
timing values, so those remain parameters here. What the theorem fixes is the
cross-layer shape: claimed state, waiting admission, matching origin, and a
non-terminal trigger observation.
-/
theorem materializedTriggerRequest_has_claimed_embedding
    (state : SystemState)
    (intent : FireIntent)
    (seed : RequestSeed)
    (inputs : ClaimedEmbeddingInputs) :
    TriggerLifecycleCoherent
      (materializedTriggerRequest state intent seed)
      (claimedEmbeddingContext state intent seed inputs) := by
  simp [TriggerLifecycleCoherent, claimedEmbeddingContext, materializedTriggerRequest,
    requestStateToTriggerTerminal]

/--
The canonical claimed embedding is itself a coherent lifecycle context:
claimed requests sit in waiting admission, which is one of the admissible
claimed-state admission modes from `Request.lean`.
-/
theorem materializedTriggerRequest_claimed_embedding_request_coherent
    (state : SystemState)
    (intent : FireIntent)
    (seed : RequestSeed)
    (inputs : ClaimedEmbeddingInputs) :
    RequestContext.coherent
      (claimedEmbeddingContext state intent seed inputs) := by
  simp [claimedEmbeddingContext, RequestContext.coherent, RequestContext.coherentStateAdmission]

/--
Update the trigger-layer terminal bit from a lifecycle state while preserving
all other trigger-managed fields.

This is the minimal "mirror" needed to talk about lifecycle evolution without
reconstructing trigger metadata from scratch.
-/
def syncTriggerTerminal (rTrig : AgentRequest) (rReq : RequestContext) : AgentRequest :=
  { rTrig with isTerminal := requestStateToTriggerTerminal rReq.state }

/-- Syncing terminality leaves the trigger lineage untouched. -/
theorem syncTriggerTerminal_preserves_causedBy
    (rTrig : AgentRequest) (rReq : RequestContext) :
    (syncTriggerTerminal rTrig rReq).causedBy = rTrig.causedBy := by
  rfl

/-- Syncing terminality leaves the declared concurrency untouched. -/
theorem syncTriggerTerminal_preserves_concurrency
    (rTrig : AgentRequest) (rReq : RequestContext) :
    (syncTriggerTerminal rTrig rReq).concurrency = rTrig.concurrency := by
  rfl

/-- Syncing terminality does not mutate the trigger-assigned execution origin. -/
theorem syncTriggerTerminal_preserves_origin
    (rTrig : AgentRequest) (rReq : RequestContext) :
    (syncTriggerTerminal rTrig rReq).executionOrigin = rTrig.executionOrigin := by
  rfl

/-- A synchronized trigger view is coherent with the lifecycle context it mirrors. -/
theorem syncTriggerTerminal_coherent
    (rTrig : AgentRequest)
    (rReq : RequestContext)
    (h_origin : rTrig.executionOrigin = rReq.origin) :
    TriggerLifecycleCoherent (syncTriggerTerminal rTrig rReq) rReq := by
  unfold TriggerLifecycleCoherent syncTriggerTerminal
  constructor
  · rfl
  · simpa using h_origin

/-- Synchronizing terminality twice is equivalent to synchronizing once from the final state. -/
theorem syncTriggerTerminal_idempotent
    (rTrig : AgentRequest)
    (mid post : RequestContext) :
    syncTriggerTerminal (syncTriggerTerminal rTrig mid) post =
      syncTriggerTerminal rTrig post := by
  cases rTrig
  rfl

/--
Lifecycle transitions preserve the trigger/lifecycle coherence relation after
the trigger view synchronizes its terminal bit from the post-state.

This theorem is load-bearing on `RequestContext.origin_preserved`: if a future
lifecycle transition mutates `origin`, this statement must be revisited.
-/
theorem triggerLifecycleCoherent_preserved_by_lifecycle_transition
    {rTrig : AgentRequest}
    {pre post : RequestContext}
    (h_coh : TriggerLifecycleCoherent rTrig pre)
    (h_trans : RequestContext.Transition pre post) :
    TriggerLifecycleCoherent (syncTriggerTerminal rTrig post) post := by
  apply syncTriggerTerminal_coherent
  calc
    rTrig.executionOrigin = pre.origin := h_coh.2
    _ = post.origin := (RequestContext.origin_preserved h_trans).symm

/--
Lifecycle traces preserve the trigger/lifecycle coherence relation after the
trigger view synchronizes its terminal bit from the final lifecycle state.
-/
theorem triggerLifecycleCoherent_preserved_by_lifecycle_trace
    {rTrig : AgentRequest}
    {pre post : RequestContext}
    (h_coh : TriggerLifecycleCoherent rTrig pre)
    (h_trace : RequestContext.Trace pre post) :
    TriggerLifecycleCoherent (syncTriggerTerminal rTrig post) post := by
  induction h_trace generalizing rTrig with
  | @refl s =>
      simpa using syncTriggerTerminal_coherent rTrig s h_coh.2
  | @step s₁ s₂ s₃ h_trans h_tail ih =>
      have h_mid :
          TriggerLifecycleCoherent (syncTriggerTerminal rTrig s₂) s₂ :=
        triggerLifecycleCoherent_preserved_by_lifecycle_transition h_coh h_trans
      have h_post :
          TriggerLifecycleCoherent
            (syncTriggerTerminal (syncTriggerTerminal rTrig s₂) s₃) s₃ :=
        ih h_mid
      simpa [syncTriggerTerminal_idempotent] using h_post

/--
Lifecycle terminality is monotone in the trigger-observable Bool projection.

Once a lifecycle state is terminal, every later state reached by a valid
request transition remains terminal, and therefore the synchronized trigger
view remains terminal as well.
-/
theorem requestStateToTriggerTerminal_monotone
    {pre post : RequestContext}
    (h_trans : RequestContext.Transition pre post) :
    requestStateToTriggerTerminal pre.state = true →
    requestStateToTriggerTerminal post.state = true := by
  intro h_pre_terminal
  have h_pre_isTerminal : isTerminal pre.state :=
    (requestStateToTriggerTerminal_eq_true_iff pre.state).mp h_pre_terminal
  have h_post_isTerminal : isTerminal post.state :=
    terminal_irreversibility h_pre_isTerminal h_trans
  exact (requestStateToTriggerTerminal_eq_true_iff post.state).mpr h_post_isTerminal

/-- The synchronized trigger view preserves terminal observations monotonically. -/
theorem syncTriggerTerminal_monotone
    (rTrig : AgentRequest)
    {pre post : RequestContext}
    (h_trans : RequestContext.Transition pre post) :
    (syncTriggerTerminal rTrig pre).isTerminal = true →
    (syncTriggerTerminal rTrig post).isTerminal = true := by
  simpa [syncTriggerTerminal] using requestStateToTriggerTerminal_monotone h_trans

/--
Concrete trace-level conformance theorem for trigger-created requests.

Starting from the canonical claimed embedding of a materialized trigger request,
any valid lifecycle trace yields a final lifecycle context whose synchronized
trigger view remains coherent with that final context.
-/
theorem materializedTriggerRequest_coherent_along_trace
    (state : SystemState)
    (intent : FireIntent)
    (seed : RequestSeed)
    (inputs : ClaimedEmbeddingInputs)
    {post : RequestContext}
    (h_trace :
      RequestContext.Trace
        (claimedEmbeddingContext state intent seed inputs)
        post) :
    TriggerLifecycleCoherent
      (syncTriggerTerminal (materializedTriggerRequest state intent seed) post)
      post := by
  apply triggerLifecycleCoherent_preserved_by_lifecycle_trace
  · exact materializedTriggerRequest_has_claimed_embedding state intent seed inputs
  · exact h_trace

/--
Step-level conformance entry point over an admissibility-constrained trigger
trace.

If `state` already lies on a `ReachableUnder P` trigger trace, `intent`
satisfies the same boundary predicate, and `dispatch` materializes a seed, then:

* the next trigger state stays on the same admissible trace
* the materialized seed/origin pair satisfies `consistentLineage`
* the created request shape admits the canonical claimed lifecycle embedding
-/
theorem reachableUnder_dispatch_materializedTriggerRequest_conforms
    (P : FireIntent → Prop)
    (state : SystemState)
    (snap : TriggerSnapshot)
    (intent : FireIntent)
    (seed : RequestSeed)
    (inputs : ClaimedEmbeddingInputs)
    (h_reach : ReachableUnder P state)
    (h_intent : P intent)
    (h_dispatch : dispatch snap intent = some seed) :
    ReachableUnder P (dispatchStep state snap intent) ∧
    consistentLineage seed (materializedTriggerRequest state intent seed).executionOrigin ∧
    TriggerLifecycleCoherent
      (materializedTriggerRequest state intent seed)
      (claimedEmbeddingContext state intent seed inputs) := by
  refine ⟨ReachableUnder.step state snap intent h_intent h_reach, ?_, ?_⟩
  · exact dispatch_materializedTriggerRequest_consistentLineage state snap intent seed h_dispatch
  · exact materializedTriggerRequest_has_claimed_embedding state intent seed inputs

/--
`WellFormedReachable` specialization of the step-level creation-side conformance
entry point.
-/
theorem wellFormedReachable_dispatch_materializedTriggerRequest_conforms
    (state : SystemState)
    (snap : TriggerSnapshot)
    (intent : FireIntent)
    (seed : RequestSeed)
    (inputs : ClaimedEmbeddingInputs)
    (h_reach : WellFormedReachable state)
    (h_intent : intent.WellFormed)
    (h_dispatch : dispatch snap intent = some seed) :
    WellFormedReachable (dispatchStep state snap intent) ∧
    consistentLineage seed (materializedTriggerRequest state intent seed).executionOrigin ∧
    TriggerLifecycleCoherent
      (materializedTriggerRequest state intent seed)
      (claimedEmbeddingContext state intent seed inputs) :=
  reachableUnder_dispatch_materializedTriggerRequest_conforms
    FireIntent.WellFormed
    state
    snap
    intent
    seed
    inputs
    h_reach
    h_intent
    h_dispatch

/--
Step-level conformance entry point that continues from trigger materialization
into an arbitrary valid lifecycle trace.

This is the place where the strengthened trigger trace boundary and the
lifecycle trace theorem surface meet explicitly.
-/
theorem reachableUnder_dispatch_materializedTriggerRequest_coherent_along_trace
    (P : FireIntent → Prop)
    (state : SystemState)
    (snap : TriggerSnapshot)
    (intent : FireIntent)
    (seed : RequestSeed)
    (inputs : ClaimedEmbeddingInputs)
    (h_reach : ReachableUnder P state)
    (h_intent : P intent)
    (h_dispatch : dispatch snap intent = some seed)
    {post : RequestContext}
    (h_trace :
      RequestContext.Trace
        (claimedEmbeddingContext state intent seed inputs)
        post) :
    ReachableUnder P (dispatchStep state snap intent) ∧
    consistentLineage seed (materializedTriggerRequest state intent seed).executionOrigin ∧
    TriggerLifecycleCoherent
      (syncTriggerTerminal (materializedTriggerRequest state intent seed) post)
      post := by
  refine ⟨ReachableUnder.step state snap intent h_intent h_reach, ?_, ?_⟩
  · exact dispatch_materializedTriggerRequest_consistentLineage state snap intent seed h_dispatch
  · exact materializedTriggerRequest_coherent_along_trace state intent seed inputs h_trace

/--
`WellFormedReachable` specialization of the step-level trace conformance entry
point.
-/
theorem wellFormedReachable_dispatch_materializedTriggerRequest_coherent_along_trace
    (state : SystemState)
    (snap : TriggerSnapshot)
    (intent : FireIntent)
    (seed : RequestSeed)
    (inputs : ClaimedEmbeddingInputs)
    (h_reach : WellFormedReachable state)
    (h_intent : intent.WellFormed)
    (h_dispatch : dispatch snap intent = some seed)
    {post : RequestContext}
    (h_trace :
      RequestContext.Trace
        (claimedEmbeddingContext state intent seed inputs)
        post) :
    WellFormedReachable (dispatchStep state snap intent) ∧
    consistentLineage seed (materializedTriggerRequest state intent seed).executionOrigin ∧
    TriggerLifecycleCoherent
      (syncTriggerTerminal (materializedTriggerRequest state intent seed) post)
      post :=
  reachableUnder_dispatch_materializedTriggerRequest_coherent_along_trace
    FireIntent.WellFormed
    state
    snap
    intent
    seed
    inputs
    h_reach
    h_intent
    h_dispatch
    h_trace
