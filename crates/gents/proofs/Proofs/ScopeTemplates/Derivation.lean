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

theorem builtin_routes_are_protocol_only (template : Template)
    (member : template ∈ builtinCatalog) (collection : String)
    (selected : collection ∈ template.collections) :
    collection ∈ Identity.replicatedProtocolCollections := by
  simp only [builtinCatalog, List.mem_cons, List.not_mem_nil, or_false] at member
  rcases member with rfl | rfl
  · have same : collection = "NodeDirectoryEntry" := by
      simpa [machineTemplate, machineCollections] using selected
    subst collection
    decide
  · have same : collection = "PeerRegistry" := by
      simpa [clientTemplate, clientCollections] using selected
    subst collection
    decide

theorem client_route_carries_only_registry (direction : RouteDirection) :
    clientRouteCollections direction = ["PeerRegistry"] := by
  cases direction <;> rfl

theorem no_application_replication (protocol requested : Finset String) :
    admitAppCollections protocol requested = none := rfl

theorem no_replica_session_index : clientIndexCollections = [] := rfl

theorem removed_templates_do_not_resolve :
    (["conversation", "agent-config", "backup", "agent-target-caller",
      "agent-target-host", "app-collections", "client-index"].map
      (resolveTemplate builtinCatalog)) = List.replicate 7 none := by
  native_decide

end ScopeTemplates
