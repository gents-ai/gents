import Proofs.Conformance.Triggers.Lifecycle

/-!
# Trigger Request Materialization Conformance

Creation-time shape and lifecycle embedding for trigger-created requests.
-/

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
trigger request is lineage-consistent.

This is the load-bearing lineage theorem for materialization: unlike
`T4_lineage_completeness`, which only unfolds the `consistentLineage`
predicate, this theorem connects actual `dispatch` output, manual lineage-id
normalization, and the execution origin written into the materialized request.
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
