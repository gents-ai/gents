import Proofs.Conformance.Triggers.Materialization
import Proofs.Properties.Safety

def syncTriggerTerminal (rTrig : AgentRequest) (rReq : RequestContext) : AgentRequest :=
  { rTrig with isTerminal := requestStateToTriggerTerminal rReq.state }

theorem syncTriggerTerminal_preserves_causedBy
    (rTrig : AgentRequest) (rReq : RequestContext) :
    (syncTriggerTerminal rTrig rReq).causedBy = rTrig.causedBy := by
  rfl

theorem syncTriggerTerminal_preserves_concurrency
    (rTrig : AgentRequest) (rReq : RequestContext) :
    (syncTriggerTerminal rTrig rReq).concurrency = rTrig.concurrency := by
  rfl

theorem syncTriggerTerminal_preserves_origin
    (rTrig : AgentRequest) (rReq : RequestContext) :
    (syncTriggerTerminal rTrig rReq).executionOrigin = rTrig.executionOrigin := by
  rfl

theorem syncTriggerTerminal_coherent
    (rTrig : AgentRequest)
    (rReq : RequestContext)
    (h_origin : rTrig.executionOrigin = rReq.origin) :
    TriggerLifecycleCoherent (syncTriggerTerminal rTrig rReq) rReq := by
  unfold TriggerLifecycleCoherent syncTriggerTerminal
  constructor
  · rfl
  · simpa using h_origin

theorem syncTriggerTerminal_idempotent
    (rTrig : AgentRequest)
    (mid post : RequestContext) :
    syncTriggerTerminal (syncTriggerTerminal rTrig mid) post =
      syncTriggerTerminal rTrig post := by
  cases rTrig
  rfl

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

theorem syncTriggerTerminal_monotone
    (rTrig : AgentRequest)
    {pre post : RequestContext}
    (h_trans : RequestContext.Transition pre post) :
    (syncTriggerTerminal rTrig pre).isTerminal = true →
    (syncTriggerTerminal rTrig post).isTerminal = true := by
  simpa [syncTriggerTerminal] using requestStateToTriggerTerminal_monotone h_trans

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
