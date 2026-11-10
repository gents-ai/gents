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
  | .perCollection rules =>
      rules.map
        (fun r =>
          { collection := r.collection
          , field := r.field
          , value :=
              match r.source with
              | .localDid => localDid
              | .homeDid => localDid })
  | .clientRoute => []

def clientRouteFilters (direction : RouteDirection) (requesterDid ownerDid : Did) :
    List CollectionPredicate :=
  [{ collection := "PeerRegistry", clauses :=
      [{ field := "node_did", value := match direction with
        | .clientToRuntime => requesterDid
        | .runtimeToClient => ownerDid }] }]

def directionalScopeFilters (template : Template) (direction : RouteDirection)
    (requesterDid ownerDid : Did) : List CollectionPredicate :=
  match template.scope with
  | .clientRoute => clientRouteFilters direction requesterDid ownerDid
  | scope =>
      (scopeFilter scope (clientRouteCollections direction) requesterDid ownerDid).map
        (fun filter =>
          { collection := filter.collection
          , clauses := [{ field := filter.field, operator := filter.operator, value := filter.value }] })

/-- A route's fixed collections are always protocol collections; application
collections are never built into a route. -/
theorem builtin_routes_are_protocol_only (template : Template)
    (member : template ∈ builtinCatalog) (collection : String)
    (selected : collection ∈ template.collections) :
    collection ∈ Identity.replicatedProtocolCollections := by
  simp only [builtinCatalog, List.mem_cons, List.not_mem_nil, or_false] at member
  rcases member with rfl | rfl | rfl
  · have same : collection = "NodeDirectoryEntry" := by
      simpa [machineTemplate, machineCollections] using selected
    subst collection
    decide
  · have same : collection = "PeerRegistry" := by
      simpa [clientTemplate, clientCollections] using selected
    subst collection
    decide
  · simp [appCollectionsTemplate] at selected

theorem appCollections_resolve :
    resolveTemplate builtinCatalog "app-collections" = some appCollectionsTemplate := by
  decide

theorem appCollections_carry_no_fixed_filter (peerDid localDid : Did) (collections : List String) :
    scopeFilter appCollectionsTemplate.scope collections peerDid localDid = [] := rfl

theorem managed_collection_is_refused (facts : AppCollectionFacts) (managed : facts.managed = true) :
    decideAppCollection facts = some .managed := by
  simp [decideAppCollection, managed]

theorem view_is_refused (facts : AppCollectionFacts) (unmanaged : facts.managed = false)
    (view : facts.isView = true) :
    decideAppCollection facts = some .view := by
  simp [decideAppCollection, unmanaged, view]

theorem policy_collection_is_refused (facts : AppCollectionFacts) (unmanaged : facts.managed = false)
    (table : facts.isView = false) (guarded : facts.hasPolicy = true) :
    decideAppCollection facts = some .policyProtected := by
  simp [decideAppCollection, unmanaged, table, guarded]

theorem admitted_iff_unmanaged_unprotected_table (facts : AppCollectionFacts) :
    decideAppCollection facts = none ↔
      facts.managed = false ∧ facts.hasPolicy = false ∧ facts.isView = false := by
  cases facts with
  | mk managed hasPolicy isView =>
    cases managed <;> cases hasPolicy <;> cases isView <;> decide

theorem client_route_carries_only_registry (direction : RouteDirection) :
    clientRouteCollections direction = ["PeerRegistry"] := by
  cases direction <;> rfl

end ScopeTemplates
