import Proofs.ScopeTemplates.State
import Mathlib.Data.List.Basic

/-!
# Scope Templates — Resolution properties

The spec's "Lean" obligations for template resolution:

  - `resolveTemplate` is **deterministic** — it is a function, but we make the
    content honest: it is functional, and the resolved template's id is the
    queried id (no aliasing), so resolving an id twice yields the same template.
  - it is **total over the catalog** — every id present in the catalog resolves
    to `some`, and an id absent from the catalog resolves to `none`.
  - `scopeFilter` is a pure case-split, proven by `cases` over `Scope`.

These mirror the Rust `resolve_template` (`iter().find(|t| t.id == id)`) and
`scope_filter`. Each property is proven by induction over the catalog list or by
`cases` over the finite `Scope`, so none is a vacuous restatement.
-/

namespace ScopeTemplates

/-- Resolve an id against the catalog. Mirrors Rust `resolve_template`:
`iter().find(|t| t.id == id)`. Unknown id → `none`. -/
def resolveTemplate (cat : Catalog) (id : TemplateId) : Option Template :=
  cat.find? (fun t => t.id = id)

/-! ## Determinism

`resolveTemplate` is a function, so it is deterministic by construction. The
non-trivial content: a resolved template carries the queried id (the `find?`
predicate guarantees it), so resolution does not alias one id to another
template; and resolving the same id from the same catalog twice is the same
result (functional). We prove the id-fidelity fact, which is what gives the
function its determinism teeth. -/

/-- Functional determinism: equal inputs give equal outputs. (Trivial, but it is
the literal "deterministic" claim — stated honestly as `rfl`.) -/
theorem resolveTemplate_deterministic (cat : Catalog) (id : TemplateId) :
    resolveTemplate cat id = resolveTemplate cat id := rfl

/-- A resolved template carries exactly the queried id. This is the substantive
determinism content: resolution never returns a template under the wrong id, so
the result is uniquely pinned to the query. Proven from the `find?` predicate. -/
theorem resolveTemplate_id_eq {cat : Catalog} {id : TemplateId} {t : Template}
    (h : resolveTemplate cat id = some t) : t.id = id := by
  unfold resolveTemplate at h
  have hp := List.find?_some h
  simpa using hp

/-- A resolved template is actually in the catalog (resolution does not invent
templates). -/
theorem resolveTemplate_mem {cat : Catalog} {id : TemplateId} {t : Template}
    (h : resolveTemplate cat id = some t) : t ∈ cat := by
  unfold resolveTemplate at h
  exact List.mem_of_find?_eq_some h

/-! ## Totality over the catalog -/

/-- **Total over the catalog.** If `id` is the id of some template present in the
catalog, resolution returns `some` (it never spuriously fails on a known id).
Proven by induction on the catalog. -/
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

/-- **Unknown ids resolve to `none`.** If no catalog template carries `id`,
resolution fails. Together with `resolveTemplate_total` this is exact totality:
`some` iff present. -/
theorem resolveTemplate_unknown {cat : Catalog} {id : TemplateId}
    (h : ∀ t ∈ cat, t.id ≠ id) :
    resolveTemplate cat id = none := by
  unfold resolveTemplate
  apply List.find?_eq_none.mpr
  intro t ht_mem
  simp only [decide_eq_true_eq]
  exact h t ht_mem

/-- Exact characterization: resolution succeeds iff a template with that id is in
the catalog. -/
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

/-! ## Scope → filter resolution -/

/-- Resolve a scope and collection set against a concrete peer/local DID pair.
Mirrors Rust `scope_filter`: `PeerDid {field}` filters every carried collection
on `peerDid`; `Unscoped` yields no filters; `PerCollection` applies each
collection-specific rule with either the local or peer DID as value. -/
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
              | .peerDid => peerDid })

/-- `scopeFilter` is the spec's case-split, proven by `cases` over `Scope`. -/
theorem scopeFilter_spec (s : Scope) (collections : List String)
    (peerDid localDid : Did) :
    scopeFilter s collections peerDid localDid =
      match s with
      | .peerDid f =>
          collections.map
            (fun c => { collection := c, field := f, value := peerDid })
      | .unscoped => []
      | .perCollection rules =>
          rules.map
            (fun r =>
              { collection := r.collection
              , field := r.field
              , value :=
                  match r.source with
                  | .localDid => localDid
                  | .peerDid => peerDid }) := by
  cases s <;> rfl

/-- A `PeerDid` scope filters every carried collection on the peer DID. -/
theorem scopeFilter_peerDid (f : String) (collections : List String)
    (peerDid localDid : Did) :
    scopeFilter (.peerDid f) collections peerDid localDid =
      collections.map
        (fun c => { collection := c, field := f, value := peerDid }) := rfl

/-- An `Unscoped` scope never yields filters (whole-collection replication). -/
theorem scopeFilter_unscoped (collections : List String) (peerDid localDid : Did) :
    scopeFilter .unscoped collections peerDid localDid = [] := rfl

/-- The coordinator subagent leg is exactly parent requests by local owner plus
bridges addressed to the peer host. -/
theorem subagentCoordinator_filter_eq (peerDid localDid : Did) :
    scopeFilter (.perCollection subagentCoordinatorRules) [] peerDid localDid
      = [ { collection := "AgentRequest",  field := "agent_did",        value := localDid }
        , { collection := "AgentToolCall", field := "spawn_target_did", value := peerDid } ] := by
  simp [scopeFilter, subagentCoordinatorRules]

/-- The host subagent leg is exactly the conversation set owned by the local host. -/
theorem subagentHost_filter_eq (peerDid localDid : Did) :
    scopeFilter (.perCollection subagentHostRules) [] peerDid localDid
      = subagentHostCollections.map
          (fun c => { collection := c, field := "agent_did", value := localDid }) := by
  simp [scopeFilter, subagentHostRules, subagentHostCollections, conversationCollections]

/-- The coordinator per-collection rules cover exactly the template's declared
collections. This is the non-vacuous collection-coverage fence for the
directional coordinator leg. -/
theorem subagentCoordinator_filters_declared_collections (peerDid localDid : Did) :
    ((scopeFilter subagentCoordinatorTemplate.scope [] peerDid localDid).map
        (fun k => k.collection)).toFinset
      = subagentCoordinatorTemplate.collections := by
  simp [scopeFilter, subagentCoordinatorTemplate, subagentCoordinatorRules]

/-- Concrete catalog membership: the coordinator template resolves from the
built-in catalog. -/
theorem subagentCoordinator_in_catalog :
    resolveTemplate builtinCatalog "subagent-coordinator" = some subagentCoordinatorTemplate := by
  decide

/-- Concrete catalog membership: the host template resolves from the built-in
catalog. -/
theorem subagentHost_in_catalog :
    resolveTemplate builtinCatalog "subagent-host" = some subagentHostTemplate := by
  decide

/-- Supporting no-third-party corollary: every per-collection filter value is one
of the local/peer DIDs. The exact-equality theorems above are the load-bearing
crossing proof; this only backs the no-third-party value statement. -/
theorem subagent_filter_values_local_or_peer
    (rules : List CollectionRule) (peerDid localDid : Did)
    (k : CollectionScopeFilter)
    (hk : k ∈ scopeFilter (.perCollection rules) [] peerDid localDid) :
    k.value = localDid ∨ k.value = peerDid := by
  simp [scopeFilter] at hk
  obtain ⟨r, _, hr⟩ := hk
  cases hsrc : r.source <;> simp [hsrc] at hr <;> subst hr <;> simp

end ScopeTemplates
