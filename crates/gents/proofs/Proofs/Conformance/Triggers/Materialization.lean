import Proofs.Conformance.Triggers.Lifecycle

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

theorem materializedTriggerRequest_nonterminal
    (state : SystemState) (intent : FireIntent) (seed : RequestSeed) :
    (materializedTriggerRequest state intent seed).isTerminal = false := by
  rfl

theorem materializedTriggerRequest_origin
    (state : SystemState) (intent : FireIntent) (seed : RequestSeed) :
    (materializedTriggerRequest state intent seed).executionOrigin =
      match seed.causedByTriggerKind with
      | .manual => .interactive
      | .schedule | .event => .scheduled := by
  rfl

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

theorem materializedTriggerRequest_claimed_embedding_request_coherent
    (state : SystemState)
    (intent : FireIntent)
    (seed : RequestSeed)
    (inputs : ClaimedEmbeddingInputs) :
    RequestContext.coherent
      (claimedEmbeddingContext state intent seed inputs) := by
  simp [claimedEmbeddingContext, RequestContext.coherent, RequestContext.coherentStateAdmission]
