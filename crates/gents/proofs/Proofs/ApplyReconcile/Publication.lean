import Proofs.ApplyReconcile.Manifest

namespace ApplyReconcile

/-- The candidate is the complete desired snapshot for the install scope.
Ordinary references resolve to the same node_did; explicit foreign capability
and agent-request delegations retain their own ACP checks. No collection
ordering is imposed. -/
def refsPresent (m : Manifest) (source : DocRef) : Option DesiredFields → Bool
  | none => false
  | some f => decide (∀ r ∈ f.refs, r.nodeDid = source.nodeDid ∧ (m.docs r).isSome = true)

def Manifest.referencesClosed (m : Manifest) : Bool :=
  decide (∀ d ∈ m.support, refsPresent m d (m.docs d) = true)

/-- Publish desired fields atomically through the existing database transaction
owner; live observations retain their existing owner. -/
def publish (old : LiveState) (candidate : Manifest) : LiveState :=
  if candidate.referencesClosed then
    { desired := candidate.docs, live := old.live }
  else old

theorem closed_lookup_same_owner (m : Manifest) (h : m.referencesClosed = true)
    (d : DocRef) (f : DesiredFields) (hd : m.docs d = some f)
    (r : DocRef) (hr : r ∈ f.refs) :
    r.nodeDid = d.nodeDid ∧ m.contains r = true := by
  have hm : d ∈ m.support := (m.support_iff d).mpr (by simp [hd])
  simp only [Manifest.referencesClosed, decide_eq_true_eq] at h
  have hf := h d hm
  simp only [hd, refsPresent, decide_eq_true_eq] at hf
  exact hf r hr

theorem closed_lookup (m : Manifest) (h : m.referencesClosed = true)
    (d : DocRef) (f : DesiredFields) (hd : m.docs d = some f)
    (r : DocRef) (hr : r ∈ f.refs) : m.contains r = true :=
  (closed_lookup_same_owner m h d f hd r hr).2

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

theorem foreign_reference_rejects (old : LiveState) (m : Manifest)
    (d r : DocRef) (f : DesiredFields)
    (hd : m.docs d = some f) (hr : r ∈ f.refs)
    (foreign : r.nodeDid ≠ d.nodeDid) : publish old m = old := by
  apply rejected_publication_unchanged
  cases h : m.referencesClosed with
  | false => rfl
  | true => exact False.elim (foreign (closed_lookup_same_owner m h d f hd r hr).1)

theorem publication_idempotent (old : LiveState) (m : Manifest) :
    publish (publish old m) m = publish old m := by
  unfold publish
  split <;> rfl

theorem publication_all_or_nothing (old : LiveState) (m : Manifest) :
    (publish old m).desired = m.docs ∨ publish old m = old := by
  unfold publish
  split
  · exact Or.inl rfl
  · exact Or.inr rfl

/-- Digest-guarded publication; an empty scope is exactly `publish`. -/
def expectationsHold (old : LiveState) (scope : List DocRef)
    (expected : DocRef → Option DesiredFields) : Bool :=
  decide (∀ d ∈ scope, old.desired d = expected d)

def publishIf (old : LiveState) (scope : List DocRef)
    (expected : DocRef → Option DesiredFields) (candidate : Manifest) : LiveState :=
  if expectationsHold old scope expected then publish old candidate else old

theorem publishIf_stale_unchanged (old : LiveState) (scope : List DocRef)
    (expected : DocRef → Option DesiredFields) (candidate : Manifest)
    (h : expectationsHold old scope expected = false) :
    publishIf old scope expected candidate = old := by
  simp [publishIf, h]

theorem publishIf_match_eq_publish (old : LiveState) (scope : List DocRef)
    (expected : DocRef → Option DesiredFields) (candidate : Manifest)
    (h : expectationsHold old scope expected = true) :
    publishIf old scope expected candidate = publish old candidate := by
  simp [publishIf, h]

theorem publishIf_empty_scope (old : LiveState)
    (expected : DocRef → Option DesiredFields) (candidate : Manifest) :
    publishIf old [] expected candidate = publish old candidate := by
  simp [publishIf, expectationsHold]

theorem publishIf_preserves_observations (old : LiveState) (scope : List DocRef)
    (expected : DocRef → Option DesiredFields) (candidate : Manifest) :
    (publishIf old scope expected candidate).live = old.live := by
  unfold publishIf
  split
  · exact publication_preserves_observations old candidate
  · rfl

theorem publishIf_all_or_nothing (old : LiveState) (scope : List DocRef)
    (expected : DocRef → Option DesiredFields) (candidate : Manifest) :
    (publishIf old scope expected candidate).desired = candidate.docs
      ∨ publishIf old scope expected candidate = old := by
  unfold publishIf
  split
  · exact publication_all_or_nothing old candidate
  · exact Or.inr rfl

theorem publishIf_idempotent (old : LiveState) (scope : List DocRef)
    (expected : DocRef → Option DesiredFields) (candidate : Manifest) :
    publishIf (publishIf old scope expected candidate) scope expected candidate
      = publishIf old scope expected candidate := by
  by_cases h : expectationsHold old scope expected = true
  · rw [publishIf_match_eq_publish old scope expected candidate h]
    unfold publishIf
    split
    · exact publication_idempotent old candidate
    · rfl
  · have hf : expectationsHold old scope expected = false := by simpa using h
    rw [publishIf_stale_unchanged old scope expected candidate hf]
    exact publishIf_stale_unchanged old scope expected candidate hf

end ApplyReconcile
