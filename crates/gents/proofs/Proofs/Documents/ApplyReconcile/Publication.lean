import Proofs.Documents.ApplyReconcile.Manifest

namespace ApplyReconcile

/-- Ordinary references resolve to the same node_did; explicit foreign capability
and agent-request delegations retain their own ACP checks. No collection
ordering is imposed, so reference cycles are allowed. -/
def refsPresent (m : Manifest) (source : DocRef) : Option DesiredFields → Bool
  | none => false
  | some f => decide (∀ r ∈ f.refs, r.nodeDid = source.nodeDid ∧ (m.docs r).isSome = true)

def Manifest.referencesClosed (m : Manifest) : Bool :=
  decide (∀ d ∈ m.support, refsPresent m d (m.docs d) = true)

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

/-- A publication plan: the documents it writes and the documents it removes.
A removal is an explicit delete or the output of `prune`; the plan owner
refuses a removal of a document the same plan writes. -/
structure Plan where
  writes : Manifest
  removals : Finset DocRef

namespace Plan

def changed (p : Plan) : Finset DocRef := p.writes.support ∪ p.removals

/-- The plan merged over the stored documents: a written document takes the
plan's fields, a removed one is gone, and every document the plan does not name
keeps its stored fields. -/
def over (p : Plan) (stored : Manifest) : Manifest where
  docs d := if d ∈ p.removals then none else
    match p.writes.docs d with
    | some f => some f
    | none => stored.docs d
  support := (p.writes.support ∪ stored.support) \ p.removals
  support_iff := by
    intro d
    rw [Finset.mem_sdiff, Finset.mem_union, p.writes.support_iff, stored.support_iff]
    by_cases hr : d ∈ p.removals
    · simp [hr]
    · simp only [hr, if_false, not_false_eq_true, and_true]
      cases hw : p.writes.docs d <;> simp

theorem over_unnamed (p : Plan) (stored : Manifest) (d : DocRef) (h : d ∉ p.changed) :
    (p.over stored).docs d = stored.docs d := by
  simp only [changed, Finset.mem_union, not_or] at h
  have hw : p.writes.docs d = none := by
    cases hw : p.writes.docs d with
    | none => rfl
    | some f => exact False.elim (h.1 ((p.writes.support_iff d).mpr (by simp [hw])))
  simp [over, h.2, hw]

theorem over_written (p : Plan) (stored : Manifest) (d : DocRef) (h : d ∉ p.removals) :
    (p.over stored).docs d = (p.writes.docs d).or (stored.docs d) := by
  simp only [over, h, if_false]
  cases p.writes.docs d <;> rfl

theorem over_removed (p : Plan) (stored : Manifest) (d : DocRef) (h : d ∈ p.removals) :
    (p.over stored).docs d = none := by
  simp [over, h]

theorem over_idempotent (p : Plan) (stored : Manifest) :
    p.over (p.over stored) = p.over stored := by
  apply Manifest.ext
  funext d
  by_cases hr : d ∈ p.removals
  · simp [over, hr]
  · simp only [over, hr, if_false]
    cases p.writes.docs d <;> rfl

end Plan

/-- The plan owner re-checks a document when the plan writes or removes it, or
when one of its references names a document the plan writes or removes. -/
def affected (m : Manifest) (changed : Finset DocRef) (d : DocRef) : Bool :=
  decide (d ∈ changed) ||
    match m.docs d with
    | none => false
    | some f => decide (∃ r ∈ f.refs, r ∈ changed)

/-- Every affected document of the merged result has its references present in
that result: references resolve against the stored documents and the plan
together, less the plan's removals. -/
def admits (next : Manifest) (changed : Finset DocRef) : Bool :=
  decide (∀ d ∈ next.support, affected next changed d = true →
    refsPresent next d (next.docs d) = true)

/-- Apply a plan atomically through the database transaction owner: merge it
over the stored desired documents, then re-check the affected documents of the
result. Live observations keep their own owner. -/
def applyPlan (old : LiveState) (p : Plan) : LiveState :=
  if admits (p.over old.desired) p.changed then
    { desired := p.over old.desired, live := old.live }
  else old

theorem applyPlan_admitted (old : LiveState) (p : Plan)
    (h : admits (p.over old.desired) p.changed = true) :
    applyPlan old p = { desired := p.over old.desired, live := old.live } := by
  simp [applyPlan, h]

theorem rejected_application_unchanged (old : LiveState) (p : Plan)
    (h : admits (p.over old.desired) p.changed = false) : applyPlan old p = old := by
  simp [applyPlan, h]

theorem application_preserves_observations (old : LiveState) (p : Plan) :
    (applyPlan old p).live = old.live := by
  unfold applyPlan
  split <;> rfl

theorem application_all_or_nothing (old : LiveState) (p : Plan) :
    (applyPlan old p).desired = p.over old.desired ∨ applyPlan old p = old := by
  unfold applyPlan
  split
  · exact Or.inl rfl
  · exact Or.inr rfl

/-- A document the plan neither writes nor removes keeps its stored fields,
whether the plan is admitted or refused. -/
theorem application_keeps_unnamed (old : LiveState) (p : Plan) (d : DocRef)
    (h : d ∉ p.changed) : (applyPlan old p).desired.docs d = old.desired.docs d := by
  unfold applyPlan
  split
  · exact Plan.over_unnamed p old.desired d h
  · rfl

theorem application_idempotent (old : LiveState) (p : Plan) :
    applyPlan (applyPlan old p) p = applyPlan old p := by
  by_cases h : admits (p.over old.desired) p.changed = true
  · rw [applyPlan_admitted old p h]
    unfold applyPlan
    simp only [Plan.over_idempotent, h, if_true]
  · have hf : admits (p.over old.desired) p.changed = false := by simpa using h
    rw [rejected_application_unchanged old p hf, rejected_application_unchanged old p hf]

/-- The safety property: an application never leaves a dangling or foreign
reference. Stored documents the plan leaves unaffected keep references that
resolved before, because no target they name is written or removed. -/
theorem application_preserves_closure (old : LiveState) (p : Plan)
    (hold : old.desired.referencesClosed = true) :
    (applyPlan old p).desired.referencesClosed = true := by
  unfold applyPlan
  split
  · rename_i hadmit
    simp only [Manifest.referencesClosed, decide_eq_true_eq]
    intro d hd
    by_cases ha : affected (p.over old.desired) p.changed d = true
    · simp only [admits, decide_eq_true_eq] at hadmit
      exact hadmit d hd ha
    · simp only [affected, Bool.or_eq_true, decide_eq_true_eq, not_or] at ha
      have hdocs := Plan.over_unnamed p old.desired d ha.1
      cases hf : old.desired.docs d with
      | none =>
        have : (p.over old.desired).docs d = none := hdocs.trans hf
        have := ((p.over old.desired).support_iff d).mp hd
        simp_all
      | some f =>
        have hnext : (p.over old.desired).docs d = some f := hdocs.trans hf
        rw [hnext] at ha
        simp only [decide_eq_true_eq, not_exists, not_and] at ha
        simp only [hnext, refsPresent, decide_eq_true_eq]
        intro r hr
        have hold' := closed_lookup_same_owner old.desired hold d f hf r hr
        refine ⟨hold'.1, ?_⟩
        rw [Plan.over_unnamed p old.desired r (ha.2 r hr)]
        exact hold'.2
  · exact hold

/-- A written document naming a document of another node is refused. -/
theorem written_foreign_reference_rejects (old : LiveState) (p : Plan)
    (d r : DocRef) (f : DesiredFields)
    (hd : p.writes.docs d = some f) (hnot : d ∉ p.removals) (hr : r ∈ f.refs)
    (foreign : r.nodeDid ≠ d.nodeDid) : applyPlan old p = old := by
  apply rejected_application_unchanged
  cases h : admits (p.over old.desired) p.changed with
  | false => rfl
  | true =>
    exfalso
    have hnext : (p.over old.desired).docs d = some f := by
      rw [Plan.over_written p old.desired d hnot, hd]; rfl
    have hsupp : d ∈ (p.over old.desired).support :=
      ((p.over old.desired).support_iff d).mpr (by simp [hnext])
    have hch : d ∈ p.changed :=
      Finset.mem_union_left _ ((p.writes.support_iff d).mpr (by simp [hd]))
    simp only [admits, decide_eq_true_eq] at h
    have hp := h d hsupp (by simp [affected, hch])
    simp only [hnext, refsPresent, decide_eq_true_eq] at hp
    exact foreign (hp r hr).1

/-- Removing a document that a retained, unwritten document references is
refused, so a removal never leaves its referrer dangling. -/
theorem removal_of_referenced_rejects (old : LiveState) (p : Plan)
    (e r : DocRef) (f : DesiredFields)
    (he : old.desired.docs e = some f) (hkept : e ∉ p.changed)
    (hr : r ∈ f.refs) (hremoved : r ∈ p.removals) : applyPlan old p = old := by
  apply rejected_application_unchanged
  cases h : admits (p.over old.desired) p.changed with
  | false => rfl
  | true =>
    exfalso
    have hnext : (p.over old.desired).docs e = some f :=
      (Plan.over_unnamed p old.desired e hkept).trans he
    have hsupp : e ∈ (p.over old.desired).support :=
      ((p.over old.desired).support_iff e).mpr (by simp [hnext])
    have hch : r ∈ p.changed := Finset.mem_union_right _ hremoved
    simp only [admits, decide_eq_true_eq] at h
    have hp := h e hsupp (by
      simp only [affected, hnext, Bool.or_eq_true, decide_eq_true_eq]
      exact Or.inr ⟨r, hr, hch⟩)
    simp only [hnext, refsPresent, decide_eq_true_eq] at hp
    have := (hp r hr).2
    rw [Plan.over_removed p old.desired r hremoved] at this
    simp at this

/-- No other stored document references `d`. -/
def removalSafe (live : Manifest) (d : DocRef) : Bool :=
  decide (∀ e ∈ live.support, e ≠ d →
    (match live.docs e with
     | none => true
     | some f => decide (d ∉ f.refs)) = true)

/-- Prune names the stored documents the authored state leaves out, for the
plan's removals: a DefaultAgentSelection is never pruned, a document is pruned
only when no other stored document references it, and a stored state whose
references do not close is refused. -/
def prune (live authored : Manifest) : Option (Finset DocRef) :=
  if live.referencesClosed then
    some (live.support.filter fun d =>
      authored.contains d = false ∧ d.collection ≠ .defaultAgentSelection ∧
        removalSafe live d = true)
  else none

theorem over_removals_only (live : Manifest) (R : Finset DocRef) (x : DocRef) (hx : x ∉ R) :
    (Plan.over { writes := Manifest.empty, removals := R } live).docs x = live.docs x := by
  simp only [Plan.over, if_neg hx]
  rfl

/-- Removing documents no other stored document references keeps a closed
stored state closed. -/
theorem safe_removals_keep_closure (live : Manifest) (R : Finset DocRef)
    (hclosed : live.referencesClosed = true)
    (hsafe : ∀ r ∈ R, removalSafe live r = true) :
    (Plan.over { writes := Manifest.empty, removals := R } live).referencesClosed = true := by
  simp only [Manifest.referencesClosed, decide_eq_true_eq]
  intro e he
  have hsupp := he
  simp only [Plan.over, Finset.mem_sdiff, Finset.mem_union] at hsupp
  have heNot : e ∉ R := hsupp.2
  have hlive : e ∈ live.support := by
    rcases hsupp.1 with h1 | h1
    · simp [Manifest.empty] at h1
    · exact h1
  rw [over_removals_only live R e heNot]
  cases hf : live.docs e with
  | none =>
    have := (live.support_iff e).mp hlive
    simp [hf] at this
  | some f =>
    simp only [refsPresent, decide_eq_true_eq]
    intro r hr
    have hold := closed_lookup_same_owner live hclosed e f hf r hr
    refine ⟨hold.1, ?_⟩
    have hrNot : r ∉ R := by
      intro hrIn
      have hs := hsafe r hrIn
      simp only [removalSafe, decide_eq_true_eq] at hs
      have hne : e ≠ r := by
        intro heq
        subst heq
        exact heNot hrIn
      have := hs e hlive hne
      simp only [hf, decide_eq_true_eq] at this
      exact this hr
    rw [over_removals_only live R r hrNot]
    exact hold.2

/-- Removing everything prune names leaves every retained reference resolved,
so the plan owner's re-check admits the removals on their own. -/
theorem pruned_removals_keep_closure (live authored : Manifest) (removals : Finset DocRef)
    (h : prune live authored = some removals) :
    (Plan.over { writes := Manifest.empty, removals } live).referencesClosed = true := by
  unfold prune at h
  split at h
  · rename_i hclosed
    simp only [Option.some.injEq] at h
    subst h
    exact safe_removals_keep_closure live _ hclosed fun r hr => (Finset.mem_filter.mp hr).2.2.2
  · simp at h

/-- Digest-guarded application; an empty scope is exactly `applyPlan`. -/
def expectationsHold (old : LiveState) (scope : List DocRef)
    (expected : DocRef → Option DesiredFields) : Bool :=
  decide (∀ d ∈ scope, old.desired.docs d = expected d)

def applyPlanIf (old : LiveState) (scope : List DocRef)
    (expected : DocRef → Option DesiredFields) (p : Plan) : LiveState :=
  if expectationsHold old scope expected then applyPlan old p else old

theorem applyPlanIf_stale_unchanged (old : LiveState) (scope : List DocRef)
    (expected : DocRef → Option DesiredFields) (p : Plan)
    (h : expectationsHold old scope expected = false) :
    applyPlanIf old scope expected p = old := by
  simp [applyPlanIf, h]

theorem applyPlanIf_match_eq_applyPlan (old : LiveState) (scope : List DocRef)
    (expected : DocRef → Option DesiredFields) (p : Plan)
    (h : expectationsHold old scope expected = true) :
    applyPlanIf old scope expected p = applyPlan old p := by
  simp [applyPlanIf, h]

theorem applyPlanIf_empty_scope (old : LiveState)
    (expected : DocRef → Option DesiredFields) (p : Plan) :
    applyPlanIf old [] expected p = applyPlan old p := by
  simp [applyPlanIf, expectationsHold]

theorem applyPlanIf_preserves_observations (old : LiveState) (scope : List DocRef)
    (expected : DocRef → Option DesiredFields) (p : Plan) :
    (applyPlanIf old scope expected p).live = old.live := by
  unfold applyPlanIf
  split
  · exact application_preserves_observations old p
  · rfl

theorem applyPlanIf_all_or_nothing (old : LiveState) (scope : List DocRef)
    (expected : DocRef → Option DesiredFields) (p : Plan) :
    (applyPlanIf old scope expected p).desired = p.over old.desired
      ∨ applyPlanIf old scope expected p = old := by
  unfold applyPlanIf
  split
  · exact application_all_or_nothing old p
  · exact Or.inr rfl

end ApplyReconcile
