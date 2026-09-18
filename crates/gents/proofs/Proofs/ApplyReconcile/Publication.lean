import Proofs.ApplyReconcile.Manifest
import Proofs.Configuration

namespace ApplyReconcile

/-- The candidate is the complete desired snapshot for the install scope,
including retained installed documents. Lookup/ACP checks precede this boundary.
Ordinary references resolve to the same agent_did. Explicit foreign capability
and subagent delegations are not ordinary references and retain their own ACP checks.
No collection ordering is imposed: subagent reference cycles are legitimate. -/
def refsPresent (m : Manifest) (source : DocRef) : Option DesiredFields → Bool
  | none => false
  | some f => decide (∀ r ∈ f.refs, r.agentDid = source.agentDid ∧ (m.docs r).isSome = true)

def Manifest.referencesClosed (m : Manifest) : Bool :=
  decide (∀ d ∈ m.support, refsPresent m d (m.docs d) = true)

/-- The publication owner receives the manifest together with the canonical
typed projection used to validate behavior-owned scope. Keeping the projection
in the candidate makes the transaction decision indivisible: callers cannot
publish documents and validate their ownership graph in separate steps. -/
structure ScopedCandidate where
  manifest : Manifest
  scopeGraph : Configuration.ScopedConfigRegistry
  /-- Set only by the canonical decoder that produced both projections from the
  same candidate documents. -/
  projectionMatches : Bool

def ScopedCandidate.valid (candidate : ScopedCandidate) : Bool :=
  candidate.manifest.referencesClosed &&
    candidate.projectionMatches &&
    Configuration.behaviorScopesValid candidate.scopeGraph

/-- Publish desired fields atomically through the existing database transaction
owner. Live observations retain their existing owner. Failed validation leaves
both desired and live state unchanged. This is the target contract, not a claim
that the current per-document Rust installer already implements it. -/
def publish (old : LiveState) (candidate : Manifest) : LiveState :=
  if candidate.referencesClosed then
    { desired := candidate.docs, live := old.live }
  else old

/-- Publish a behavior closure only when ordinary references and the canonical
scope graph both validate. The graph accepts complete legacy/unscoped closures;
scoped closures reject mixed ownership, cross-binding, and unreachable
documents. -/
def publishScoped (old : LiveState) (candidate : ScopedCandidate) : LiveState :=
  if candidate.valid then
    { desired := candidate.manifest.docs, live := old.live }
  else old

theorem closed_lookup_same_owner (m : Manifest) (h : m.referencesClosed = true)
    (d : DocRef) (f : DesiredFields) (hd : m.docs d = some f)
    (r : DocRef) (hr : r ∈ f.refs) :
    r.agentDid = d.agentDid ∧ m.contains r = true := by
  have hm : d ∈ m.support := (m.support_iff d).mpr (by simp [hd])
  simp only [Manifest.referencesClosed, decide_eq_true_eq] at h
  have hf := h d hm
  simp only [hd, refsPresent, decide_eq_true_eq] at hf
  exact hf r hr

theorem closed_lookup (m : Manifest) (h : m.referencesClosed = true)
    (d : DocRef) (f : DesiredFields) (hd : m.docs d = some f)
    (r : DocRef) (hr : r ∈ f.refs) : m.contains r = true :=
  (closed_lookup_same_owner m h d f hd r hr).2

/-- The accepted snapshot realizes the entire manifest, including legal cycles. -/
theorem publish_realizes (old : LiveState) (m : Manifest)
    (h : m.referencesClosed = true) : (publish old m).desired = m.docs := by
  simp [publish, h]

theorem publication_preserves_observations (old : LiveState) (m : Manifest) :
    (publish old m).live = old.live := by
  unfold publish
  split <;> rfl

theorem rejected_publication_unchanged (old : LiveState) (m : Manifest)
    (h : m.referencesClosed = false) : publish old m = old := by
  simp [publish, h]

theorem accepted_publication_has_no_dangling_refs (old : LiveState) (m : Manifest)
    (h : m.referencesClosed = true) (d : DocRef) (f : DesiredFields)
    (hd : (publish old m).desired d = some f) (r : DocRef) (hr : r ∈ f.refs) :
    (publish old m).contains r = true := by
  simp only [publish, h, Bool.true_eq, ↓reduceIte] at hd ⊢
  exact closed_lookup m h d f hd r hr

/-- A visible foreign-owned document cannot satisfy an ordinary config reference. -/
theorem foreign_reference_rejects (old : LiveState) (m : Manifest)
    (d r : DocRef) (f : DesiredFields)
    (hd : m.docs d = some f) (hr : r ∈ f.refs)
    (foreign : r.agentDid ≠ d.agentDid) : publish old m = old := by
  apply rejected_publication_unchanged
  cases h : m.referencesClosed with
  | false => rfl
  | true => exact False.elim (foreign (closed_lookup_same_owner m h d f hd r hr).1)

/-- Repeating an installation converges immediately without replaying writes. -/
theorem publication_idempotent (old : LiveState) (m : Manifest) :
    publish (publish old m) m = publish old m := by
  unfold publish
  split <;> rfl

/-- Atomic publication exposes either the complete candidate or the old state,
never an intermediate prefix. -/
theorem publication_all_or_nothing (old : LiveState) (m : Manifest) :
    (publish old m).desired = m.docs ∨ publish old m = old := by
  unfold publish
  split
  · exact Or.inl rfl
  · exact Or.inr rfl

theorem scoped_publication_realizes (old : LiveState) (candidate : ScopedCandidate)
    (h : candidate.valid = true) :
    (publishScoped old candidate).desired = candidate.manifest.docs := by
  simp [publishScoped, h]

theorem scoped_publication_preserves_observations
    (old : LiveState) (candidate : ScopedCandidate) :
    (publishScoped old candidate).live = old.live := by
  unfold publishScoped
  split <;> rfl

theorem rejected_scoped_publication_unchanged
    (old : LiveState) (candidate : ScopedCandidate)
    (h : candidate.valid = false) : publishScoped old candidate = old := by
  simp [publishScoped, h]

theorem scoped_publication_requires_closed_references
    (old : LiveState) (candidate : ScopedCandidate)
    (h : (publishScoped old candidate).desired = candidate.manifest.docs)
    (distinct : old.desired ≠ candidate.manifest.docs) :
    candidate.manifest.referencesClosed = true := by
  by_contra invalid
  have hv : candidate.valid = false := by
    simp [ScopedCandidate.valid, Bool.eq_false_iff, invalid]
  have unchanged := rejected_scoped_publication_unchanged old candidate hv
  exact distinct (by simpa [unchanged] using h)

theorem scoped_publication_requires_valid_ownership
    (old : LiveState) (candidate : ScopedCandidate)
    (h : (publishScoped old candidate).desired = candidate.manifest.docs)
    (distinct : old.desired ≠ candidate.manifest.docs) :
    Configuration.behaviorScopesValid candidate.scopeGraph = true := by
  by_contra invalid
  have hv : candidate.valid = false := by
    simp [ScopedCandidate.valid, Bool.eq_false_iff, invalid]
  have unchanged := rejected_scoped_publication_unchanged old candidate hv
  exact distinct (by simpa [unchanged] using h)

theorem scoped_publication_requires_matching_projection
    (old : LiveState) (candidate : ScopedCandidate)
    (h : (publishScoped old candidate).desired = candidate.manifest.docs)
    (distinct : old.desired ≠ candidate.manifest.docs) :
    candidate.projectionMatches = true := by
  by_contra invalid
  have hv : candidate.valid = false := by
    simp [ScopedCandidate.valid, Bool.eq_false_iff, invalid]
  have unchanged := rejected_scoped_publication_unchanged old candidate hv
  exact distinct (by simpa [unchanged] using h)

theorem scoped_publication_idempotent (old : LiveState)
    (candidate : ScopedCandidate) :
    publishScoped (publishScoped old candidate) candidate =
      publishScoped old candidate := by
  unfold publishScoped
  split <;> rfl

theorem scoped_publication_all_or_nothing (old : LiveState)
    (candidate : ScopedCandidate) :
    (publishScoped old candidate).desired = candidate.manifest.docs ∨
      publishScoped old candidate = old := by
  unfold publishScoped
  split
  · exact Or.inl rfl
  · exact Or.inr rfl

end ApplyReconcile
