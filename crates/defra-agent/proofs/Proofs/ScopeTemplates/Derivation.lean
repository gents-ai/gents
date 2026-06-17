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

/-- Resolve a scope against a concrete peer DID into an optional filter key.
Mirrors Rust `scope_filter`: `PeerDid {field}` → equality on `field == peer_did`;
`Unscoped` → no filter. -/
def scopeFilter : Scope → Did → Option ScopeFilterKey
  | .peerDid f, did => some ⟨f, did⟩
  | .unscoped, _ => none

/-- Resolve a scope and collection set against a concrete peer DID into
per-collection filter entries. This is the model shape corresponding to Rust
`PairingFilters`: a `PeerDid` scope filters every collection on the peer DID,
while `Unscoped` yields the empty filter map. -/
noncomputable def scopeFilters (s : Scope) (collections : Finset String) (did : Did) :
    Finset CollectionScopeFilter :=
  match s with
  | .peerDid f =>
      (collections.toList.map
        (fun c => ({ collection := c, field := f, value := did } : CollectionScopeFilter))).toFinset
  | .unscoped => ∅

/-- `scopeFilter` is the spec's case-split, proven by `cases` over `Scope`. -/
theorem scopeFilter_spec (s : Scope) (did : Did) :
    scopeFilter s did =
      match s with
      | .peerDid f => some ⟨f, did⟩
      | .unscoped => none := by
  cases s <;> rfl

/-- A `PeerDid` scope always yields a filter, keyed on its field and the peer. -/
theorem scopeFilter_peerDid (f : String) (did : Did) :
    scopeFilter (.peerDid f) did = some ⟨f, did⟩ := rfl

/-- An `Unscoped` scope never yields a filter (whole-collection replication). -/
theorem scopeFilter_unscoped (did : Did) :
    scopeFilter .unscoped did = none := rfl

/-- Per-collection form: a `PeerDid` scope creates exactly one filter entry for
each carried collection. -/
theorem scopeFilters_peerDid_mem_iff
    (f : String) (collections : Finset String) (did : Did) (k : CollectionScopeFilter) :
    k ∈ scopeFilters (.peerDid f) collections did ↔
      ∃ c ∈ collections, k = ⟨c, f, did⟩ := by
  simp [scopeFilters]
  constructor
  · intro h
    rcases h with ⟨c, hc, hk⟩
    exact ⟨c, hc, hk.symm⟩
  · intro h
    rcases h with ⟨c, hc, hk⟩
    exact ⟨c, hc, hk.symm⟩

/-- Per-collection form: an `Unscoped` scope yields no filters. -/
theorem scopeFilters_unscoped (collections : Finset String) (did : Did) :
    scopeFilters .unscoped collections did = ∅ := rfl

/-- A scope yields a filter iff it is scoped to a peer DID. Exact, by `cases`. -/
theorem scopeFilter_isSome_iff (s : Scope) (did : Did) :
    (scopeFilter s did).isSome ↔ ∃ f, s = .peerDid f := by
  cases s with
  | peerDid f => simp [scopeFilter]
  | unscoped => simp [scopeFilter]

/-! ## Push ⇔ scoped (the conversation-template relation)

The spec invites relating `Push` delivery to a scoped template "if it falls out
cleanly". We do not bake it into the catalog type (the model permits any
delivery/scope pairing, matching the Rust types), but we expose it as a
*definitional predicate* a caller can require of a catalog, plus the leaf fact
that under such a well-formed template, `Push` delivery resolves to a real
filter. This keeps the conversation-template invariant checkable without forcing
it on every template. -/

/-- A template is "scope-coherent" when `Push` delivery is paired with a scoped
filter (the conversation-template shape: filtered push, no whole-collection
gossip). `Replicate` is unconstrained (it may be scoped or unscoped). -/
def scopeCoherent (t : Template) : Prop :=
  t.delivery = .push → ∃ f, t.scope = .peerDid f

/-- Under a scope-coherent template, a `Push` delivery always resolves to a
concrete filter key against any peer DID — i.e. push is never silently
unfiltered. -/
theorem push_template_has_filter {t : Template} (h : scopeCoherent t)
    (hpush : t.delivery = .push) (did : Did) :
    (scopeFilter t.scope did).isSome := by
  obtain ⟨f, hf⟩ := h hpush
  rw [hf]
  rfl

/-- Per-collection version of `push_template_has_filter`: under a scope-coherent
`Push` template, EVERY one of the template's collections gets a concrete
peer-DID filter entry — the template→filter derivation never leaves a `Push`
collection unfiltered.

SCOPE: this is a property of the template→filter DERIVATION (`scopeFilters`)
ALONE. It does NOT model the Rust merged Layer-1/Layer-2 install (one replicator
carrying unfiltered control collections alongside peer-DID-filtered conversation
collections in a single per-collection filter map): that union is assembled at
install time and is fenced by the `engine.rs` `merge_desired` unit tests
(`merge_desired_unions_control_and_data_plane_state`,
`data_plane_only_desired_is_replicator_only`), not by this theorem. What this
theorem guarantees the merge can rely on is the per-collection-completeness of
each push template's derived filter set. -/
theorem push_template_filters_every_collection {t : Template} (h : scopeCoherent t)
    (hpush : t.delivery = .push) (did : Did) {c : String}
    (hc : c ∈ t.collections) :
    ∃ k ∈ scopeFilters t.scope t.collections did,
      k.collection = c ∧ k.value = did := by
  obtain ⟨f, hf⟩ := h hpush
  rw [hf]
  refine ⟨⟨c, f, did⟩, ?_, rfl, rfl⟩
  simp [scopeFilters, hc]

end ScopeTemplates
