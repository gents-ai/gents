import Proofs.ScopeTemplates.State
import Mathlib.Data.List.Basic

namespace ScopeTemplates

def resolveTemplate (cat : Catalog) (id : TemplateId) : Option Template :=
  cat.find? (fun t => t.id = id)

theorem resolveTemplate_id_eq {cat : Catalog} {id : TemplateId} {t : Template}
    (h : resolveTemplate cat id = some t) : t.id = id := by
  unfold resolveTemplate at h
  have hp := List.find?_some h
  simpa using hp

theorem resolveTemplate_mem {cat : Catalog} {id : TemplateId} {t : Template}
    (h : resolveTemplate cat id = some t) : t ∈ cat := by
  unfold resolveTemplate at h
  exact List.mem_of_find?_eq_some h

theorem resolveTemplate_total {cat : Catalog} {id : TemplateId}
    (h : ∃ t ∈ cat, t.id = id) :
    ∃ t, resolveTemplate cat id = some t := by
  obtain ⟨t, ht_mem, ht_id⟩ := h
  unfold resolveTemplate
  cases hfind : cat.find? (fun t => t.id = id) with
  | some r => exact ⟨r, rfl⟩
  | none =>
      exfalso
      have hnone := List.find?_eq_none.mp hfind t ht_mem
      simp [ht_id] at hnone

theorem resolveTemplate_unknown {cat : Catalog} {id : TemplateId}
    (h : ∀ t ∈ cat, t.id ≠ id) :
    resolveTemplate cat id = none := by
  unfold resolveTemplate
  apply List.find?_eq_none.mpr
  intro t ht_mem
  simp only [decide_eq_true_eq]
  exact h t ht_mem

theorem resolveTemplate_isSome_iff {cat : Catalog} {id : TemplateId} :
    (resolveTemplate cat id).isSome ↔ ∃ t ∈ cat, t.id = id := by
  constructor
  · intro h
    obtain ⟨t, ht⟩ := Option.isSome_iff_exists.mp h
    exact ⟨t, resolveTemplate_mem ht, resolveTemplate_id_eq ht⟩
  · intro h
    obtain ⟨t, ht⟩ := resolveTemplate_total h
    rw [ht]
    rfl

def scopeFilter (scope : Scope) (collections : List String)
    (peerDid localDid : Did) : List CollectionScopeFilter :=
  match scope with
  | .peerDid field =>
      collections.map
        (fun c => { collection := c, field := field, value := peerDid })
  | .unscoped => []
  | .perCollection rules =>
      rules.map
        (fun r =>
          { collection := r.collection
          , field := r.field
          , value :=
              match r.source with
              | .localDid => localDid
              | .peerDid => peerDid
              | .homeDid => localDid })
  | .clientRoute => []

theorem scopeFilter_peerDid (f : String) (collections : List String)
    (peerDid localDid : Did) :
    scopeFilter (.peerDid f) collections peerDid localDid =
      collections.map
        (fun c => { collection := c, field := f, value := peerDid }) := rfl

theorem scopeFilter_unscoped (collections : List String) (peerDid localDid : Did) :
    scopeFilter .unscoped collections peerDid localDid = [] := rfl

def clientTranscriptPredicates (requesterDid ownerDid : Did) :
    List CollectionPredicate :=
  clientTranscriptCollections.map
    (fun collection =>
      { collection := collection
      , clauses :=
          [ { field := "requester_did", value := requesterDid }
          , { field := "node_did", value := ownerDid } ] })

def clientRouteFilters (direction : RouteDirection) (requesterDid ownerDid : Did) :
    List CollectionPredicate :=
  clientTranscriptPredicates requesterDid ownerDid ++
    [ { collection := "PeerEndpoint"
      , clauses :=
          [ { field := "did"
            , value :=
                match direction with
                | .clientToRuntime => requesterDid
                | .runtimeToClient => ownerDid } ] }
    , { collection := "SessionHydrationRequest"
      , clauses :=
          [ { field := "requester_did", value := requesterDid }
          , { field := "node_did", value := ownerDid } ] } ] ++
    match direction with
    | .clientToRuntime => []
    | .runtimeToClient =>
        [ { collection := "NodeReadiness"
          , clauses := [ { field := "node_did", value := ownerDid } ] } ]

def directionalScopeFilters (template : Template) (direction : RouteDirection)
    (requesterDid ownerDid : Did) : List CollectionPredicate :=
  match template.scope with
  | .clientRoute => clientRouteFilters direction requesterDid ownerDid
  | scope =>
      (scopeFilter scope (clientRouteCollections direction) requesterDid ownerDid).map
        (fun filter =>
          { collection := filter.collection
          , clauses :=
              [ { field := filter.field
                , operator := filter.operator
                , value := filter.value } ] })

/-- The durable client-route transcript row identifies its principal scope by
the originating requester and the owning node destination, matching the
immutable `requester_did` / `node_did` fields of the accepted SDL. -/
structure TranscriptIdentity where
  requesterDid : Did
  nodeDid : Did

def clientTranscriptMatches (requesterDid ownerDid : Did)
    (row : TranscriptIdentity) : Bool :=
  decide (row.requesterDid = requesterDid) && decide (row.nodeDid = ownerDid)

theorem conversation_filter_eq (peerDid localDid : Did) :
    scopeFilter (.perCollection conversationRules) [] peerDid localDid
      = [ { collection := "AgentRequest",    field := "requester_did", value := peerDid }
        , { collection := "AgentMessage",    field := "requester_did", value := peerDid }
        , { collection := "AgentToolCall",   field := "requester_did", value := peerDid }
        , { collection := "AgentOutputSegment", field := "requester_did", value := peerDid }
        , { collection := "AgentSession",    field := "requester_did", value := peerDid }
        , { collection := "CompactionEntry", field := "requester_did", value := peerDid } ] := by
  simp [scopeFilter, conversationRules]

theorem conversation_filters_requester_lineage (peerDid localDid : Did) :
    (scopeFilter conversationTemplate.scope [] peerDid localDid).all
      (fun k =>
        k.value = peerDid ∧ k.field = "requester_did") = true := by
  simp [scopeFilter, conversationTemplate, conversationRules]

theorem conversation_filters_exactly_transcript_collections (peerDid localDid : Did) :
    ((scopeFilter conversationTemplate.scope [] peerDid localDid).map
        (fun k => k.collection)).toFinset
      = transcriptCollections.toFinset := by
  simp [scopeFilter, conversationTemplate, conversationRules,
    transcriptCollections]

theorem conversation_config_is_unfiltered (peerDid localDid : Did) :
    agentConfigCollections.all (fun collection =>
      (scopeFilter conversationTemplate.scope [] peerDid localDid).all
        (fun filter => filter.collection ≠ collection)) = true := by
  simp [scopeFilter, conversationTemplate, conversationRules,
    agentConfigCollections, reachableConfigKinds, ConfigDocuments.Collection.collectionName, ConfigDocuments.documentSpec]

theorem conversation_grants_agent_config :
    agentConfigCollections.toFinset ⊆ conversationTemplate.collections := by
  simp only [conversationTemplate, conversationCollections, List.toFinset_append]
  exact Finset.subset_union_right

theorem conversation_request_crossing_is_peer_scoped (peerDid localDid : Did) :
    (scopeFilter conversationTemplate.scope [] peerDid localDid).find?
        (fun k => k.collection = "AgentRequest") =
      some { collection := "AgentRequest", field := "requester_did", value := peerDid } := by
  simp [scopeFilter, conversationTemplate, conversationRules]

theorem machine_filter_eq (peerDid homeDid : Did) :
    scopeFilter machineTemplate.scope [] peerDid homeDid =
      scopeFilter conversationTemplate.scope [] peerDid homeDid ++
        [ { collection := "MailboxItem"
          , field := "requester_did"
          , value := peerDid }
        , { collection := "SessionHydrationRequest"
          , field := "requester_did"
          , value := peerDid }
        , { collection := "NodeDirectoryEntry"
          , field := "source_did"
          , value := homeDid } ] := by
  simp [scopeFilter, machineTemplate, machineRules, conversationTemplate]

theorem machine_filters_transcript_and_directory (peerDid homeDid : Did) :
    ((scopeFilter machineTemplate.scope [] peerDid homeDid).map
        (fun k => k.collection)).toFinset
      = (transcriptCollections ++
          ["MailboxItem", "SessionHydrationRequest", "NodeDirectoryEntry"]).toFinset := by
  simp [scopeFilter, machineTemplate, machineRules, machineCollections,
    conversationRules, conversationCollections, transcriptCollections]

theorem machine_directory_crossing_is_home_scoped (peerDid homeDid : Did) :
    (scopeFilter machineTemplate.scope [] peerDid homeDid).find?
        (fun k => k.collection = "NodeDirectoryEntry") =
          some { collection := "NodeDirectoryEntry"
               , field := "source_did"
               , value := homeDid } := by
  simp [scopeFilter, machineTemplate, machineRules, conversationRules]

theorem client_in_catalog :
    resolveTemplate builtinCatalog "client" = some clientTemplate := by
  decide

theorem client_directional_resolution_is_total
    (direction : RouteDirection) (requesterDid ownerDid : Did) :
    directionalScopeFilters clientTemplate direction requesterDid ownerDid =
      clientRouteFilters direction requesterDid ownerDid := by
  rfl

theorem client_request_filter_conjoins_requester_and_destination
    (direction : RouteDirection) (requesterDid ownerDid : Did) :
    (clientRouteFilters direction requesterDid ownerDid).find?
        (fun predicate => predicate.collection = "AgentRequest") =
      some
        { collection := "AgentRequest"
        , clauses :=
            [ { field := "requester_did", value := requesterDid }
            , { field := "node_did", value := ownerDid } ] } := by
  cases direction <;>
    simp [clientRouteFilters, clientTranscriptPredicates,
      clientTranscriptCollections, transcriptCollections]

theorem client_hydration_request_conjoins_requester_and_destination
    (direction : RouteDirection) (requesterDid ownerDid : Did) :
    (clientRouteFilters direction requesterDid ownerDid).find?
        (fun predicate => predicate.collection = "SessionHydrationRequest") =
      some
        { collection := "SessionHydrationRequest"
        , clauses :=
            [ { field := "requester_did", value := requesterDid }
            , { field := "node_did", value := ownerDid } ] } := by
  cases direction <;>
    simp [clientRouteFilters, clientTranscriptPredicates,
      clientTranscriptCollections, transcriptCollections]

theorem client_transcript_match_iff_requester_and_destination
    (requesterDid ownerDid : Did) (row : TranscriptIdentity) :
    clientTranscriptMatches requesterDid ownerDid row = true ↔
      row.requesterDid = requesterDid ∧ row.nodeDid = ownerDid := by
  simp [clientTranscriptMatches]

theorem client_transcript_destination_scoped
    (requesterDid ownerDid : Did) (row : TranscriptIdentity)
    (hmatches : clientTranscriptMatches requesterDid ownerDid row = true) :
    row.nodeDid = ownerDid := by
  exact (client_transcript_match_iff_requester_and_destination
    requesterDid ownerDid row).mp hmatches |>.2

theorem client_transcript_rejects_another_destination
    (requesterDid ownerDid otherDid : Did) (different : otherDid ≠ ownerDid) :
    clientTranscriptMatches requesterDid ownerDid
      { requesterDid := requesterDid, nodeDid := otherDid } = false := by
  simp [clientTranscriptMatches, different]

theorem clientTranscriptPredicates_collections (requesterDid ownerDid : Did) :
    (clientTranscriptPredicates requesterDid ownerDid).map
        (fun predicate => predicate.collection) = clientTranscriptCollections := by
  simp only [clientTranscriptPredicates, List.map_map, Function.comp_apply]
  rfl

theorem client_filters_cover_exact_directional_projection
    (direction : RouteDirection) (requesterDid ownerDid : Did) :
    (clientRouteFilters direction requesterDid ownerDid).map
        (fun predicate => predicate.collection) =
      match direction with
      | .clientToRuntime => clientToRuntimeCollections
      | .runtimeToClient => clientToRuntimeCollections ++ clientOwnerProjectionCollections := by
  cases direction <;>
    simp [clientRouteFilters, clientTranscriptPredicates_collections,
      clientToRuntimeCollections, clientOwnerProjectionCollections]

theorem client_return_adds_exact_bounded_control_plane :
    clientRouteCollections .runtimeToClient =
      clientToRuntimeCollections ++ clientControlPlaneCollections ++
        clientOwnerProjectionCollections := by rfl

theorem client_readiness_return_is_owner_scoped
    (requesterDid ownerDid : Did) :
    (clientRouteFilters .runtimeToClient requesterDid ownerDid).find?
        (fun predicate => predicate.collection = "NodeReadiness") =
      some
        { collection := "NodeReadiness"
        , clauses := [ { field := "node_did", value := ownerDid } ] } := by
  simp [clientRouteFilters, clientTranscriptPredicates, transcriptCollections,
    clientTranscriptCollections]

theorem client_return_control_plane_is_unfiltered
    (requesterDid ownerDid : Did) :
    clientControlPlaneCollections.all
      (fun collection =>
        (clientRouteFilters .runtimeToClient requesterDid ownerDid).all
          (fun predicate => predicate.collection ≠ collection)) = true := by
  simp [clientControlPlaneCollections, clientRouteFilters,
    clientTranscriptPredicates, transcriptCollections, agentConfigCollections,
    reachableConfigKinds, ConfigDocuments.Collection.collectionName,
    ConfigDocuments.documentSpec, clientTranscriptCollections]

theorem client_excludes_unbounded_and_secret_control_plane :
    "PeerPairingDesired" ∉ clientTemplate.collections ∧
    "DataPlanePairingDesired" ∉ clientTemplate.collections ∧
    "InferenceBackend" ∉ clientTemplate.collections ∧
    "OAuthCredential" ∉ clientTemplate.collections := by
  decide

/-- Operator configuration includes backend configuration. Permission to create
or use that route belongs to existing DID/ACP admission, outside this selection model. -/
theorem operator_config_selects_backend :
    "InferenceBackend" ∈ agentConfigTemplate.collections := by decide

/-- Ordinary client, transcript and agent-target templates never select credential
documents. Template names do not confer operator authorization. -/
theorem ordinary_routes_exclude_credentials
    (t : Template) (ht : t ∈ [clientTemplate, conversationTemplate, machineTemplate,
      clientIndexTemplate, agentTargetHostTemplate, agentTargetCallerTemplate])
    (c : String) (hc : c ∈ credentialCollections) : c ∉ t.collections := by
  simp only [credentialCollections, List.mem_cons, List.not_mem_nil, or_false] at ht hc
  rcases ht with rfl | rfl | rfl | rfl | rfl | rfl <;>
    rcases hc with rfl | rfl <;> decide

theorem agentTargetCaller_filter_eq (peerDid localDid : Did) :
    scopeFilter (.perCollection agentTargetCallerRules) [] peerDid localDid
      = [ { collection := "AgentRequest", field := "node_did", value := peerDid } ] := by
  simp [scopeFilter, agentTargetCallerRules]

theorem agentTargetHost_filter_eq (peerDid localDid : Did) :
    scopeFilter (.perCollection agentTargetHostRules) [] peerDid localDid
      = [ { collection := "AgentRequest",    field := "requester_did", value := peerDid }
        , { collection := "AgentSession",    field := "requester_did", value := peerDid }
        , { collection := "AgentOutputSegment", field := "requester_did", value := peerDid }
        , { collection := "AgentMessage",    field := "requester_did", value := peerDid } ] := by
  simp [scopeFilter, agentTargetHostRules]

theorem agentTargetHost_filters_requester_lineage (peerDid localDid : Did) :
    (scopeFilter agentTargetHostTemplate.scope [] peerDid localDid).all
      (fun k => k.field = "requester_did" ∧ k.value = peerDid) = true := by
  simp [scopeFilter, agentTargetHostTemplate, agentTargetHostRules]

/-- The caller → host leg carries exactly the Peer AgentRequest a remote
`agent_new` authors on the caller, selected by the target's `node_did`. -/
theorem agentTargetCaller_carries_exactly_target_request (peerDid localDid : Did) :
    agentTargetCallerTemplate.collections = {"AgentRequest"} ∧
    scopeFilter agentTargetCallerTemplate.scope [] peerDid localDid
      = [ { collection := "AgentRequest", field := "node_did", value := peerDid } ] := by
  refine ⟨by decide, ?_⟩
  simp [scopeFilter, agentTargetCallerTemplate, agentTargetCallerRules]

/-- The host → caller leg carries exactly the caused request, its session and
its transcript, each selected by the caller's `requester_did`. -/
theorem agentTargetHost_carries_exactly_caused_transcript (peerDid localDid : Did) :
    agentTargetHostTemplate.collections =
      ["AgentRequest", "AgentSession", "AgentOutputSegment", "AgentMessage"].toFinset ∧
    (scopeFilter agentTargetHostTemplate.scope [] peerDid localDid).all
      (fun k => k.field = "requester_did" ∧ k.value = peerDid) = true := by
  exact ⟨rfl, agentTargetHost_filters_requester_lineage peerDid localDid⟩

theorem agentTargetCaller_filters_declared_collections (peerDid localDid : Did) :
    ((scopeFilter agentTargetCallerTemplate.scope [] peerDid localDid).map
        (fun k => k.collection)).toFinset
      = agentTargetCallerTemplate.collections := by
  simp [scopeFilter, agentTargetCallerTemplate, agentTargetCallerRules,
    agentTargetCallerCollections]

theorem agentTargetHost_filters_declared_collections (peerDid localDid : Did) :
    ((scopeFilter agentTargetHostTemplate.scope [] peerDid localDid).map
        (fun k => k.collection)).toFinset
      = agentTargetHostTemplate.collections := by
  simp [scopeFilter, agentTargetHostTemplate, agentTargetHostRules,
    agentTargetHostCollections]

/-- The host's AgentToolCall rows are host-local execution records: they, tool
results, compaction and configuration never ride either agent-target leg. -/
theorem agent_target_legs_exclude_host_local_execution :
    "AgentToolCall" ∉ agentTargetHostTemplate.collections ∧
    "AgentToolCall" ∉ agentTargetCallerTemplate.collections ∧
    "AgentToolResult" ∉ agentTargetHostTemplate.collections ∧
    "CompactionEntry" ∉ agentTargetHostTemplate.collections ∧
    "Agent" ∉ agentTargetHostTemplate.collections ∧
    "InferenceBackend" ∉ agentTargetHostTemplate.collections := by
  decide

/-- Whether the route a template resolves for `peerDid` selects a row of
`collection` whose `field` holds `rowDid`. -/
def routeSelects (t : Template) (collection field : String)
    (rowDid peerDid localDid : Did) : Bool :=
  (scopeFilter t.scope [] peerDid localDid).any
    (fun k => k.collection == collection && k.field == field && k.value == rowDid)

theorem agentTargetCaller_selects_request_iff_target
    (target peerDid localDid : Did) :
    routeSelects agentTargetCallerTemplate "AgentRequest" "node_did"
      target peerDid localDid = true ↔ peerDid = target := by
  unfold routeSelects
  rw [show agentTargetCallerTemplate.scope = .perCollection agentTargetCallerRules
    from rfl, agentTargetCaller_filter_eq]
  simp

theorem agentTargetHost_selects_request_iff_requester
    (requester peerDid localDid : Did) :
    routeSelects agentTargetHostTemplate "AgentRequest" "requester_did"
      requester peerDid localDid = true ↔ peerDid = requester := by
  unfold routeSelects
  rw [show agentTargetHostTemplate.scope = .perCollection agentTargetHostRules
    from rfl, agentTargetHost_filter_eq]
  simp

private theorem wave_selects_one (peers : List Did) (did : Did)
    (select : Did → Bool) (hsel : ∀ p, select p = true ↔ p = did)
    (hnodup : peers.Nodup) (hmem : did ∈ peers) :
    (peers.filter select).length = 1 := by
  have hfilter : peers.filter select = peers.filter (· == did) := by
    apply List.filter_congr
    intro p _
    by_cases h : p = did
    · simp [h, (hsel did).mpr rfl]
    · have hfalse : select p = false := by
        cases hs : select p
        · rfl
        · exact absurd ((hsel p).mp hs) h
      simp [h, hfalse]
  rw [hfilter, ← List.countP_eq_length_filter, ← List.count]
  exact List.count_eq_one_of_mem hnodup hmem

/-- A request wave over distinct peers reaches one host: of a caller's routes
to its peers, only the route to the request's target `node_did` carries it. -/
theorem agentTargetCaller_wave_reaches_one_target
    (peers : List Did) (target localDid : Did)
    (hnodup : peers.Nodup) (hmem : target ∈ peers) :
    (peers.filter (fun p => routeSelects agentTargetCallerTemplate
      "AgentRequest" "node_did" target p localDid)).length = 1 :=
  wave_selects_one peers target _
    (fun p => agentTargetCaller_selects_request_iff_target target p localDid)
    hnodup hmem

/-- Of a host's routes to distinct callers, only the route to the caused
request's `requester_did` returns it. -/
theorem agentTargetHost_wave_returns_to_one_requester
    (peers : List Did) (requester localDid : Did)
    (hnodup : peers.Nodup) (hmem : requester ∈ peers) :
    (peers.filter (fun p => routeSelects agentTargetHostTemplate
      "AgentRequest" "requester_did" requester p localDid)).length = 1 :=
  wave_selects_one peers requester _
    (fun p => agentTargetHost_selects_request_iff_requester requester p localDid)
    hnodup hmem

theorem agentTargetCaller_in_catalog :
    resolveTemplate builtinCatalog "agent-target-caller" = some agentTargetCallerTemplate := by
  decide

theorem agentTargetHost_in_catalog :
    resolveTemplate builtinCatalog "agent-target-host" = some agentTargetHostTemplate := by
  decide

theorem machine_in_catalog :
    resolveTemplate builtinCatalog "machine" = some machineTemplate := by
  decide

theorem appCollections_in_catalog :
    resolveTemplate builtinCatalog "app-collections" = some appCollectionsTemplate := by
  decide

theorem appCollections_collections_empty :
    appCollectionsTemplate.collections = (∅ : Finset String) := rfl

theorem appCollections_unscoped_no_filter (collections : List String) (peerDid localDid : Did) :
    scopeFilter appCollectionsTemplate.scope collections peerDid localDid = [] := rfl

theorem appCollections_admission_sound
    (protocolCatalog requested admitted : Finset String)
    (h : admitAppCollections protocolCatalog requested = some admitted) :
    admitted = requested ∧ Disjoint admitted protocolCatalog := by
  unfold admitAppCollections at h
  split at h
  case isTrue valid =>
    simp only [Option.some.injEq] at h
    subst admitted
    exact ⟨rfl, valid.2⟩
  case isFalse => contradiction

theorem appCollections_protocol_overlap_rejected
    (protocolCatalog requested : Finset String)
    (overlap : ¬ Disjoint requested protocolCatalog) :
    admitAppCollections protocolCatalog requested = none := by
  simp [admitAppCollections, overlap]

theorem appCollections_empty_rejected (protocolCatalog : Finset String) :
    admitAppCollections protocolCatalog ∅ = none := by
  simp [admitAppCollections]

theorem clientIndex_in_catalog :
    resolveTemplate builtinCatalog "client-index" = some clientIndexTemplate := by
  decide

theorem clientIndex_filter_eq (peerDid localDid : Did) :
    scopeFilter (.perCollection clientIndexRules) [] peerDid localDid
      = [ { collection := "AgentSession", field := "requester_did", value := peerDid }
        , { collection := "MailboxItem",  field := "requester_did", value := peerDid } ] := by
  simp [scopeFilter, clientIndexRules]

theorem clientIndex_filters_requester_lineage (peerDid localDid : Did) :
    (scopeFilter clientIndexTemplate.scope [] peerDid localDid).all
      (fun k => k.value = peerDid ∧ k.field = "requester_did") = true := by
  simp [scopeFilter, clientIndexTemplate, clientIndexRules]

/-- The eager index contains exactly the session and mailbox documents. -/
theorem clientIndex_covers_exactly_literal_index_collections :
    clientIndexTemplate.collections =
      ["AgentSession", "MailboxItem"].toFinset := by
  decide

theorem agent_target_filter_values_local_or_peer
    (rules : List CollectionRule) (peerDid localDid : Did)
    (k : CollectionScopeFilter)
    (hk : k ∈ scopeFilter (.perCollection rules) [] peerDid localDid) :
    k.value = localDid ∨ k.value = peerDid := by
  simp [scopeFilter] at hk
  obtain ⟨r, _, hr⟩ := hk
  cases hsrc : r.source <;> simp [hsrc] at hr <;> subst hr <;> simp

end ScopeTemplates
