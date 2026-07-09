import Proofs.ApplyReconcile.Collections

/-!
# Apply/Reconcile Manifest Model

Manifest, live-state projections, desired fields, and reference-closure predicates.
-/

namespace ApplyReconcile

/-- Operator-owned field payload for a document, paired with the set of
    cross-document references it declares. Concrete CLI structs pack
    their apply-owned fields into `content` and populate `refs` by
    projecting their reference fields (e.g. AgentBehavior's backend_id
    becomes a DocRef in `refs`). The Lean model treats `content`
    opaquely; all proof obligations concern `refs`. -/
structure DesiredFields where
  content : String
  refs    : Finset DocRef
  deriving DecidableEq

/-- Abstract runtime-owned field payload per document. Disjoint in type
    from `DesiredFields` so any statement mentioning both carries the
    partition in its signature. -/
abbrev LiveFields := String

/-- Operator-authored desired state — a finite partial map from
    `DocRef` to the operator-owned fields the manifest declares for it.
    The explicit `support` field carries finiteness so the apply `diff`
    has a concrete enumeration to drive; `support_iff` ties it to
    `docs`. -/
structure Manifest where
  docs        : DocRef → Option DesiredFields
  support     : Finset DocRef
  support_iff : ∀ d, d ∈ support ↔ (docs d).isSome = true

namespace Manifest

/-- Does the manifest declare this document? -/
def contains (m : Manifest) (d : DocRef) : Bool := (m.docs d).isSome

end Manifest

/-- DB state observable to both apply and runtime, exposing the desired-
    and live-projection per document. `liveOnly` documents are those with
    no manifest entry but nonzero live state. The CLI reports generic rows
    diagnostically unless prune is enabled; provenance-scoped,
    manifest-authoritative rows are retracted automatically. -/
structure LiveState where
  desired : DocRef → Option DesiredFields
  live    : DocRef → Option LiveFields

namespace LiveState

def contains (L : LiveState) (d : DocRef) : Bool := (L.desired d).isSome

end LiveState

/-- Cross-document references a desired-fields value declares.
    Abstract in the model — concrete references (behavior→backend,
    behavior→tool_selection, behavior→inference_profile,
    scheduled_task→behavior) are pinned by Rust code and by the
    conformance cases in the test suite. The proof only needs the
    predicate that a reference exists; the relation itself is abstracted
    via `referencesOf` and can be instantiated concretely per collection
    without re-editing theorems. -/
def referencesOf : DesiredFields → Finset DocRef := fun f => f.refs

/-- A manifest is well-formed when every reference target is itself in
    the manifest (ref-closure) **and** references go to strictly-lower-rank
    collections (the topological ordering invariant pinned by the spec:
    e.g. AgentBehavior's backend_id points to an InferenceBackend with
    `applyOrder = 0 < 2`). The second clause is consumed by the sort-order
    argument in `apply_preserves_wellFormed`. -/
def Manifest.WellFormed (m : Manifest) : Prop :=
  (∀ d : DocRef, ∀ f, m.docs d = some f →
    ∀ r ∈ referencesOf f, m.contains r = true) ∧
  (∀ d : DocRef, ∀ f, m.docs d = some f →
    ∀ r ∈ referencesOf f,
      r.collection.applyOrder < d.collection.applyOrder)

/-- A live state is reference-closed on its desired projection when every
    reference in a present document resolves to another present document,
    and references respect the strictly-lower-rank invariant. -/
def LiveState.WellFormed (L : LiveState) : Prop :=
  (∀ d : DocRef, ∀ f, L.desired d = some f →
    ∀ r ∈ referencesOf f, L.contains r = true) ∧
  (∀ d : DocRef, ∀ f, L.desired d = some f →
    ∀ r ∈ referencesOf f,
      r.collection.applyOrder < d.collection.applyOrder)


end ApplyReconcile
