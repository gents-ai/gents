import Proofs.PromptAssembly.Executable

namespace PromptAssembly

open Transcript (MessageRow MessageKind MessageRole)

def NonemptyAnnouncements : List MessageRow → Prop
  | [] => True
  | row :: rest =>
    match row.kind with
    | .assistantToolCalls callIds =>
        callIds ≠ ∅ ∧ NonemptyAnnouncements rest
    | _ => NonemptyAnnouncements rest

@[simp] theorem nonemptyAnnouncements_nil : NonemptyAnnouncements [] := trivial

theorem nonemptyAnnouncements_cons_assistant (row : MessageRow)
    (rest : List MessageRow) (callIds : Finset ToolExecution.ToolCallId)
    (h : row.kind = .assistantToolCalls callIds) :
    NonemptyAnnouncements (row :: rest) ↔
      callIds ≠ ∅ ∧ NonemptyAnnouncements rest := by
  simp only [NonemptyAnnouncements, h]

theorem nonemptyAnnouncements_cons_other (row : MessageRow)
    (rest : List MessageRow)
    (h : ∀ callIds, row.kind ≠ .assistantToolCalls callIds) :
    NonemptyAnnouncements (row :: rest) ↔ NonemptyAnnouncements rest := by
  cases hk : row.kind with
  | assistantToolCalls callIds => exact absurd hk (h callIds)
  | toolResult callId key => simp only [NonemptyAnnouncements, hk]
  | ordinary => simp only [NonemptyAnnouncements, hk]

theorem callsIn_dropOrphanedFrom (l : List MessageRow) :
    ∀ pending, callsIn (dropOrphanedFrom pending l) = callsIn l := by
  induction l with
  | nil => intro pending; rfl
  | cons row rest ih =>
    intro pending
    cases hk : row.kind with
    | toolResult callId key =>
      rw [dropOrphanedFrom_cons_result row rest pending callId key hk]
      by_cases hc : callId ∈ pending
      · rw [if_pos hc, callsIn_cons_result row _ callId key hk,
          callsIn_cons_result row rest callId key hk, ih (pending.erase callId)]
      · rw [if_neg hc, ih pending, callsIn_cons_result row rest callId key hk]
    | assistantToolCalls callIds =>
      rw [dropOrphanedFrom_cons_assistant row rest pending callIds hk,
        callsIn_cons_assistant row _ callIds hk,
        callsIn_cons_assistant row rest callIds hk, ih callIds]
    | ordinary =>
      rw [dropOrphanedFrom_cons_ordinary row rest pending hk,
        callsIn_cons_ordinary row _ hk, callsIn_cons_ordinary row rest hk,
        ih ∅]

theorem resolvedIn_dropOrphanedFrom_subset (l : List MessageRow) :
    ∀ pending, resolvedIn (dropOrphanedFrom pending l) ⊆ pending ∪ callsIn l := by
  induction l with
  | nil =>
    intro pending c hc
    simp at hc
  | cons row rest ih =>
    intro pending c hc
    cases hk : row.kind with
    | toolResult callId key =>
      rw [dropOrphanedFrom_cons_result row rest pending callId key hk] at hc
      rw [callsIn_cons_result row rest callId key hk]
      by_cases hmem : callId ∈ pending
      · rw [if_pos hmem] at hc
        rw [resolvedIn_cons_result row _ callId key hk] at hc
        rcases Finset.mem_insert.mp hc with hcEq | hcTail
        · subst hcEq
          exact Finset.mem_union_left _ hmem
        · exact (Finset.mem_union.mp ((ih (pending.erase callId)) hcTail)).elim
            (fun hp => Finset.mem_union_left _ (Finset.mem_of_mem_erase hp))
            (fun hcall => Finset.mem_union_right _ hcall)
      · rw [if_neg hmem] at hc
        exact (ih pending) hc
    | assistantToolCalls callIds =>
      rw [dropOrphanedFrom_cons_assistant row rest pending callIds hk] at hc
      rw [resolvedIn_cons_assistant row _ callIds hk] at hc
      rw [callsIn_cons_assistant row rest callIds hk]
      exact (Finset.mem_union.mp ((ih callIds) hc)).elim
        (fun hp => Finset.mem_union_right _ (Finset.mem_union_left _ hp))
        (fun hcall => Finset.mem_union_right _ (Finset.mem_union_right _ hcall))
    | ordinary =>
      rw [dropOrphanedFrom_cons_ordinary row rest pending hk] at hc
      rw [resolvedIn_cons_ordinary row _ hk] at hc
      rw [callsIn_cons_ordinary row rest hk]
      exact Finset.mem_union_right _
        ((Finset.mem_union.mp ((ih ∅) hc)).elim (by intro h; simp at h) id)

theorem nonempty_filterCallsBy (resolved : Finset ToolExecution.ToolCallId)
    (l : List MessageRow) :
    NonemptyAnnouncements (filterCallsBy resolved l) := by
  induction l with
  | nil => simp
  | cons row rest ih =>
    cases hk : row.kind with
    | assistantToolCalls callIds =>
      rw [filterCallsBy_cons_assistant row rest resolved callIds hk]
      by_cases hempty : callIds ∩ resolved = ∅
      · rw [if_pos hempty]; exact ih
      · rw [if_neg hempty,
          nonemptyAnnouncements_cons_assistant (withKind row _) _
            (callIds ∩ resolved) (withKind_kind row _)]
        exact ⟨hempty, ih⟩
    | toolResult callId key =>
      rw [filterCallsBy_cons_result row rest resolved callId key hk,
        nonemptyAnnouncements_cons_other row _
          (by intro c h; rw [hk] at h; exact MessageKind.noConfusion h)]
      exact ih
    | ordinary =>
      rw [filterCallsBy_cons_ordinary row rest resolved hk,
        nonemptyAnnouncements_cons_other row _
          (by intro c h; rw [hk] at h; exact MessageKind.noConfusion h)]
      exact ih

theorem filterCallsBy_irrelevant (l : List MessageRow) :
    ∀ extra R, Disjoint extra (callsIn l) →
      filterCallsBy (extra ∪ R) l = filterCallsBy R l := by
  induction l with
  | nil => intro extra R _; rfl
  | cons row rest ih =>
    intro extra R hdisj
    cases hk : row.kind with
    | assistantToolCalls callIds =>
      have hpair : Disjoint extra callIds ∧ Disjoint extra (callsIn rest) := by
        rw [callsIn_cons_assistant row rest callIds hk,
          Finset.disjoint_union_right] at hdisj
        exact hdisj
      rw [filterCallsBy_cons_assistant row rest (extra ∪ R) callIds hk,
        filterCallsBy_cons_assistant row rest R callIds hk, ih extra R hpair.2]
      have hinter : callIds ∩ (extra ∪ R) = callIds ∩ R := by
        rw [Finset.inter_union_distrib_left,
          Finset.disjoint_iff_inter_eq_empty.mp hpair.1.symm, Finset.empty_union]
      rw [hinter]
    | toolResult callId key =>
      rw [filterCallsBy_cons_result row rest (extra ∪ R) callId key hk,
        filterCallsBy_cons_result row rest R callId key hk]
      have hrest : Disjoint extra (callsIn rest) := by
        rwa [callsIn_cons_result row rest callId key hk] at hdisj
      rw [ih extra R hrest]
    | ordinary =>
      rw [filterCallsBy_cons_ordinary row rest (extra ∪ R) hk,
        filterCallsBy_cons_ordinary row rest R hk]
      have hrest : Disjoint extra (callsIn rest) := by
        rwa [callsIn_cons_ordinary row rest hk] at hdisj
      rw [ih extra R hrest]

theorem erase_inter_insert_eq (pending R : Finset ToolExecution.ToolCallId)
    (callId : ToolExecution.ToolCallId) :
    (pending ∩ insert callId R).erase callId = pending.erase callId ∩ R := by
  apply Finset.ext
  intro c
  by_cases hEq : c = callId
  · subst hEq
    simp
  · simp [Finset.mem_erase, hEq, eq_comm]

theorem activeBlockValid_filterOrphanedFrom (l : List MessageRow) :
    ∀ pending,
      UniqueCallIds l →
      Disjoint pending (callsIn l) →
      ActiveBlockValidFrom (pending ∩ resolvedIn (dropOrphanedFrom pending l))
        (filterCallsBy (resolvedIn (dropOrphanedFrom pending l))
          (dropOrphanedFrom pending l)) := by
  induction l with
  | nil =>
    intro pending _ _
    simp [ActiveBlockValidFrom]
  | cons row rest ih =>
    intro pending huniq hdisj
    cases hk : row.kind with
    | toolResult callId key =>
      have huniq' := (uniqueCallIds_cons_result row rest callId key hk).mp huniq
      have hdisj' : Disjoint pending (callsIn rest) := by
        rwa [callsIn_cons_result row rest callId key hk] at hdisj
      rw [dropOrphanedFrom_cons_result row rest pending callId key hk]
      by_cases hmem : callId ∈ pending
      · rw [if_pos hmem]
        let tail := dropOrphanedFrom (pending.erase callId) rest
        let Rtail := resolvedIn tail
        have hnotCall : callId ∉ callsIn rest := by
          intro hcall
          exact Finset.disjoint_left.mp hdisj' hmem hcall
        have hfilter :
            filterCallsBy (insert callId Rtail) tail = filterCallsBy Rtail tail := by
          have hirrel : Disjoint {callId} (callsIn tail) := by
            rw [callsIn_dropOrphanedFrom rest (pending.erase callId)]
            exact Finset.disjoint_singleton_left.mpr hnotCall
          simpa using
            filterCallsBy_irrelevant tail {callId} Rtail hirrel
        rw [resolvedIn_cons_result row tail callId key hk,
          filterCallsBy_cons_result row tail (insert callId Rtail) callId key hk,
          activeBlockValidFrom_cons_result row (filterCallsBy (insert callId Rtail) tail)
            (pending ∩ insert callId Rtail) callId key hk]
        refine ⟨?_, ?_⟩
        · exact Finset.mem_inter.mpr ⟨hmem, Finset.mem_insert_self _ _⟩
        · rw [hfilter, erase_inter_insert_eq pending Rtail callId]
          exact ih (pending.erase callId) huniq' (by
            rw [Finset.disjoint_left]
            intro c hc hcall
            exact Finset.disjoint_left.mp hdisj'
              (Finset.mem_of_mem_erase hc) hcall)
      · rw [if_neg hmem]
        exact ih pending huniq' hdisj'
    | assistantToolCalls callIds =>
      have huniqPair := (uniqueCallIds_cons_assistant row rest callIds hk).mp huniq
      have hdisjPair : Disjoint pending callIds ∧ Disjoint pending (callsIn rest) := by
        rw [callsIn_cons_assistant row rest callIds hk,
          Finset.disjoint_union_right] at hdisj
        exact hdisj
      rw [dropOrphanedFrom_cons_assistant row rest pending callIds hk]
      let tail := dropOrphanedFrom callIds rest
      let Rtail := resolvedIn tail
      have hRsub : Rtail ⊆ callIds ∪ callsIn rest :=
        resolvedIn_dropOrphanedFrom_subset rest callIds
      have hstart : pending ∩ Rtail = ∅ :=
        Finset.disjoint_iff_inter_eq_empty.mp
          ((Finset.disjoint_union_right.mpr
            ⟨hdisjPair.1, hdisjPair.2⟩).mono_right hRsub)
      rw [resolvedIn_cons_assistant row tail callIds hk,
        filterCallsBy_cons_assistant row tail Rtail callIds hk]
      by_cases hempty : callIds ∩ Rtail = ∅
      · rw [if_pos hempty, hstart]
        have htail := ih callIds huniqPair.2 huniqPair.1
        change callIds ∩ resolvedIn (dropOrphanedFrom callIds rest) = ∅ at hempty
        rwa [hempty] at htail
      · rw [if_neg hempty, hstart,
          activeBlockValidFrom_cons_assistant (withKind row _) _ ∅
            (callIds ∩ Rtail) (withKind_kind row _)]
        exact ⟨rfl, ih callIds huniqPair.2 huniqPair.1⟩
    | ordinary =>
      have huniq' := (uniqueCallIds_cons_ordinary row rest hk).mp huniq
      have hdisj' : Disjoint pending (callsIn rest) := by
        rwa [callsIn_cons_ordinary row rest hk] at hdisj
      rw [dropOrphanedFrom_cons_ordinary row rest pending hk]
      let tail := dropOrphanedFrom ∅ rest
      let Rtail := resolvedIn tail
      have hRsub : Rtail ⊆ callsIn rest := by
        simpa using resolvedIn_dropOrphanedFrom_subset rest ∅
      have hstart : pending ∩ Rtail = ∅ :=
        Finset.disjoint_iff_inter_eq_empty.mp (hdisj'.mono_right hRsub)
      rw [resolvedIn_cons_ordinary row tail hk,
        filterCallsBy_cons_ordinary row tail Rtail hk, hstart,
        activeBlockValidFrom_cons_ordinary row _ ∅ hk]
      exact ⟨rfl, ih ∅ huniq' (Finset.disjoint_empty_left _)⟩

theorem sanitize_sound {msgs : List MessageRow} (huniq : UniqueCallIds msgs) :
    ProviderValid (sanitize msgs) := by
  unfold sanitize dropUnpairedCalls dropOrphanedResults
  constructor
  simpa using
    activeBlockValid_filterOrphanedFrom msgs ∅ huniq (Finset.disjoint_empty_left _)

theorem sanitize_split_stable {old recent : List MessageRow}
    (huniq : UniqueCallIds (old ++ recent)) :
    ProviderValid (sanitize recent) :=
  sanitize_sound (UniqueCallIds.of_append_right huniq)

theorem dropOrphanedFrom_eq_self (l : List MessageRow) :
    ∀ pending, ActiveBlockValidFrom pending l → dropOrphanedFrom pending l = l := by
  induction l with
  | nil => intro pending hvalid; rfl
  | cons row rest ih =>
    intro pending hvalid
    cases hk : row.kind with
    | toolResult callId key =>
      have h := (activeBlockValidFrom_cons_result row rest pending callId key hk).mp hvalid
      rw [dropOrphanedFrom_cons_result row rest pending callId key hk,
        if_pos h.1, ih (pending.erase callId) h.2]
    | assistantToolCalls callIds =>
      have h := (activeBlockValidFrom_cons_assistant row rest pending callIds hk).mp hvalid
      rw [dropOrphanedFrom_cons_assistant row rest pending callIds hk,
        ih callIds h.2]
    | ordinary =>
      have h := (activeBlockValidFrom_cons_ordinary row rest pending hk).mp hvalid
      rw [dropOrphanedFrom_cons_ordinary row rest pending hk, ih ∅ h.2]

theorem activeBlockValid_pending_subset_resolved (l : List MessageRow) :
    ∀ pending, ActiveBlockValidFrom pending l → pending ⊆ resolvedIn l := by
  induction l with
  | nil =>
    intro pending hvalid
    rw [(activeBlockValidFrom_nil pending).mp hvalid]
    simp
  | cons row rest ih =>
    intro pending hvalid c hc
    cases hk : row.kind with
    | toolResult callId key =>
      have h := (activeBlockValidFrom_cons_result row rest pending callId key hk).mp hvalid
      rw [resolvedIn_cons_result row rest callId key hk]
      by_cases hEq : c = callId
      · subst hEq
        exact Finset.mem_insert_self _ _
      · exact Finset.mem_insert_of_mem (ih (pending.erase callId) h.2
          (Finset.mem_erase.mpr ⟨hEq, hc⟩))
    | assistantToolCalls callIds =>
      have h := (activeBlockValidFrom_cons_assistant row rest pending callIds hk).mp hvalid
      rw [h.1] at hc
      simp at hc
    | ordinary =>
      have h := (activeBlockValidFrom_cons_ordinary row rest pending hk).mp hvalid
      rw [h.1] at hc
      simp at hc

theorem resolvedIn_subset_cons (row : MessageRow) (rest : List MessageRow) :
    resolvedIn rest ⊆ resolvedIn (row :: rest) := by
  cases hk : row.kind with
  | toolResult callId key =>
    rw [resolvedIn_cons_result row rest callId key hk]
    exact Finset.subset_insert _ _
  | assistantToolCalls callIds =>
    rw [resolvedIn_cons_assistant row rest callIds hk]
  | ordinary => rw [resolvedIn_cons_ordinary row rest hk]

theorem filterCallsBy_eq_self (l : List MessageRow) :
    ∀ (R pending : Finset ToolExecution.ToolCallId),
      ActiveBlockValidFrom pending l → NonemptyAnnouncements l →
      resolvedIn l ⊆ R → filterCallsBy R l = l := by
  induction l with
  | nil => intro R pending _ _ _; rfl
  | cons row rest ih =>
    intro R pending hvalid hne hres
    have hres' : resolvedIn rest ⊆ R :=
      (resolvedIn_subset_cons row rest).trans hres
    cases hk : row.kind with
    | assistantToolCalls callIds =>
      have h := (activeBlockValidFrom_cons_assistant row rest pending callIds hk).mp hvalid
      have hne' := (nonemptyAnnouncements_cons_assistant row rest callIds hk).mp hne
      have hsub : callIds ⊆ R :=
        (activeBlockValid_pending_subset_resolved rest callIds h.2).trans hres'
      have hinter : callIds ∩ R = callIds := Finset.inter_eq_left.mpr hsub
      rw [filterCallsBy_cons_assistant row rest R callIds hk, hinter,
        if_neg hne'.1, withKind_self row _ hk,
        ih R callIds h.2 hne'.2 hres']
    | toolResult callId key =>
      have h := (activeBlockValidFrom_cons_result row rest pending callId key hk).mp hvalid
      have hne' := (nonemptyAnnouncements_cons_other row rest
        (by intro c hc; rw [hk] at hc; exact MessageKind.noConfusion hc)).mp hne
      rw [filterCallsBy_cons_result row rest R callId key hk,
        ih R (pending.erase callId) h.2 hne' hres']
    | ordinary =>
      have h := (activeBlockValidFrom_cons_ordinary row rest pending hk).mp hvalid
      have hne' := (nonemptyAnnouncements_cons_other row rest
        (by intro c hc; rw [hk] at hc; exact MessageKind.noConfusion hc)).mp hne
      rw [filterCallsBy_cons_ordinary row rest R hk,
        ih R ∅ h.2 hne' hres']

/-! ## Per-turn resolution agrees with global resolution

`filterCallsBy` credits an announcement from the global resolved set;
`filterCallsByTurn` credits it only from its own turn. They coincide under
`UniqueCallIds`, which every theorem here already assumes — so the per-turn
model production implements is fenced by the same results. -/

/-- Every result closes a call owned by the nearest preceding announcement.
This is `ActiveBlockValidFrom` without the `pending = ∅` obligations, i.e.
exactly the shape `dropOrphanedFrom` leaves behind. -/
def ResultsOwnedFrom (pending : Finset ToolExecution.ToolCallId) :
    List MessageRow → Prop
  | [] => True
  | row :: rest =>
    match row.kind with
    | .toolResult callId _ =>
        callId ∈ pending ∧ ResultsOwnedFrom (pending.erase callId) rest
    | .assistantToolCalls callIds => ResultsOwnedFrom callIds rest
    | .ordinary => ResultsOwnedFrom ∅ rest

theorem resultsOwnedFrom_dropOrphanedFrom (l : List MessageRow) :
    ∀ pending, ResultsOwnedFrom pending (dropOrphanedFrom pending l) := by
  induction l with
  | nil => intro pending; trivial
  | cons row rest ih =>
    intro pending
    cases hk : row.kind with
    | toolResult callId key =>
      rw [dropOrphanedFrom_cons_result row rest pending callId key hk]
      by_cases hmem : callId ∈ pending
      · rw [if_pos hmem]
        show ResultsOwnedFrom pending (row :: _)
        unfold ResultsOwnedFrom
        rw [hk]
        exact ⟨hmem, ih (pending.erase callId)⟩
      · rw [if_neg hmem]; exact ih pending
    | assistantToolCalls callIds =>
      rw [dropOrphanedFrom_cons_assistant row rest pending callIds hk]
      show ResultsOwnedFrom pending (row :: _)
      unfold ResultsOwnedFrom
      rw [hk]
      exact ih callIds
    | ordinary =>
      rw [dropOrphanedFrom_cons_ordinary row rest pending hk]
      show ResultsOwnedFrom pending (row :: _)
      unfold ResultsOwnedFrom
      rw [hk]
      exact ih ∅

/-- Owned results only ever name pending or later-announced calls. -/
theorem resolvedIn_subset_of_resultsOwned (l : List MessageRow) :
    ∀ pending, ResultsOwnedFrom pending l → resolvedIn l ⊆ pending ∪ callsIn l := by
  induction l with
  | nil => intro pending _ c hc; simp at hc
  | cons row rest ih =>
    intro pending howned c hc
    unfold ResultsOwnedFrom at howned
    cases hk : row.kind with
    | toolResult callId key =>
      rw [hk] at howned
      rw [resolvedIn_cons_result row rest callId key hk] at hc
      rw [callsIn_cons_result row rest callId key hk]
      rcases Finset.mem_insert.mp hc with rfl | htail
      · exact Finset.mem_union_left _ howned.1
      · exact (Finset.mem_union.mp (ih (pending.erase callId) howned.2 htail)).elim
          (fun hp => Finset.mem_union_left _ (Finset.mem_of_mem_erase hp))
          (fun hcall => Finset.mem_union_right _ hcall)
    | assistantToolCalls callIds =>
      rw [hk] at howned
      rw [resolvedIn_cons_assistant row rest callIds hk] at hc
      rw [callsIn_cons_assistant row rest callIds hk]
      exact (Finset.mem_union.mp (ih callIds howned hc)).elim
        (fun hp => Finset.mem_union_right _ (Finset.mem_union_left _ hp))
        (fun hcall => Finset.mem_union_right _ (Finset.mem_union_right _ hcall))
    | ordinary =>
      rw [hk] at howned
      rw [resolvedIn_cons_ordinary row rest hk] at hc
      rw [callsIn_cons_ordinary row rest hk]
      exact Finset.mem_union_right _
        ((Finset.mem_union.mp (ih ∅ howned hc)).elim (by intro h; simp at h) id)

/-- **The crux.** A turn's announcements can only be resolved inside that turn:
anything later belongs to a later announcement, which `UniqueCallIds` keeps
disjoint. -/
theorem inter_resolvedIn_eq_inter_resolvedInTurn (l : List MessageRow) :
    ∀ callIds, ResultsOwnedFrom callIds l → Disjoint callIds (callsIn l) →
      callIds ∩ resolvedIn l = callIds ∩ resolvedInTurn l := by
  induction l with
  | nil => intro callIds _ _; simp
  | cons row rest ih =>
    intro callIds howned hdisj
    unfold ResultsOwnedFrom at howned
    cases hk : row.kind with
    | toolResult callId key =>
      rw [hk] at howned
      have hdisj' : Disjoint callIds (callsIn rest) := by
        rwa [callsIn_cons_result row rest callId key hk] at hdisj
      rw [resolvedIn_cons_result row rest callId key hk,
        resolvedInTurn_cons_result row rest callId key hk]
      -- Both sides insert the same id; the tails agree by induction after
      -- erasing it, and `callId` itself is in `callIds` either way.
      apply Finset.ext
      intro c
      by_cases hc : c = callId
      · subst hc; simp [howned.1]
      · have htail := ih (callIds.erase callId) howned.2 (by
          rw [Finset.disjoint_left]
          intro x hx hcall
          exact Finset.disjoint_left.mp hdisj' (Finset.mem_of_mem_erase hx) hcall)
        have hmem : ∀ (S : Finset ToolExecution.ToolCallId),
            (c ∈ callIds ∩ insert callId S) ↔ (c ∈ callIds.erase callId ∩ S) := by
          intro S
          simp [Finset.mem_inter, Finset.mem_insert, Finset.mem_erase, hc]
        rw [hmem (resolvedIn rest), hmem (resolvedInTurn rest), htail]
    | assistantToolCalls callIds' =>
      rw [hk] at howned
      rw [resolvedIn_cons_assistant row rest callIds' hk,
        resolvedInTurn_cons_assistant row rest callIds' hk, Finset.inter_empty]
      -- A new announcement ends the turn. Everything the tail resolves is owned
      -- by that announcement or a later one, all inside `callsIn rest`, which
      -- `UniqueCallIds` keeps disjoint from `callIds`.
      have hsub : resolvedIn rest ⊆ callIds' ∪ callsIn rest :=
        resolvedIn_subset_of_resultsOwned rest callIds' howned
      have hdisjPair : Disjoint callIds callIds' ∧ Disjoint callIds (callsIn rest) := by
        rw [callsIn_cons_assistant row rest callIds' hk,
          Finset.disjoint_union_right] at hdisj
        exact hdisj
      exact Finset.disjoint_iff_inter_eq_empty.mp
        ((Finset.disjoint_union_right.mpr ⟨hdisjPair.1, hdisjPair.2⟩).mono_right hsub)
    | ordinary =>
      rw [hk] at howned
      rw [resolvedIn_cons_ordinary row rest hk,
        resolvedInTurn_cons_ordinary row rest hk, Finset.inter_empty]
      have hdisj' : Disjoint callIds (callsIn rest) := by
        rwa [callsIn_cons_ordinary row rest hk] at hdisj
      have hsub : resolvedIn rest ⊆ callsIn rest := by
        simpa using resolvedIn_subset_of_resultsOwned rest ∅ howned
      exact Finset.disjoint_iff_inter_eq_empty.mp (hdisj'.mono_right hsub)

theorem uniqueCallIds_dropOrphanedFrom (l : List MessageRow) :
    ∀ pending, UniqueCallIds l → UniqueCallIds (dropOrphanedFrom pending l) := by
  induction l with
  | nil => intro pending _; simp
  | cons row rest ih =>
    intro pending huniq
    cases hk : row.kind with
    | toolResult callId key =>
      have huniq' := (uniqueCallIds_cons_result row rest callId key hk).mp huniq
      rw [dropOrphanedFrom_cons_result row rest pending callId key hk]
      by_cases hmem : callId ∈ pending
      · rw [if_pos hmem, uniqueCallIds_cons_result row _ callId key hk]
        exact ih (pending.erase callId) huniq'
      · rw [if_neg hmem]; exact ih pending huniq'
    | assistantToolCalls callIds =>
      have hpair := (uniqueCallIds_cons_assistant row rest callIds hk).mp huniq
      rw [dropOrphanedFrom_cons_assistant row rest pending callIds hk,
        uniqueCallIds_cons_assistant row _ callIds hk]
      refine ⟨?_, ih callIds hpair.2⟩
      rw [callsIn_dropOrphanedFrom rest callIds]
      exact hpair.1
    | ordinary =>
      have huniq' := (uniqueCallIds_cons_ordinary row rest hk).mp huniq
      rw [dropOrphanedFrom_cons_ordinary row rest pending hk,
        uniqueCallIds_cons_ordinary row _ hk]
      exact ih ∅ huniq'

/-- Per-turn and global resolution produce the same list on owned, unique-id
input. `R` is the ambient global set; the hypothesis says it decides exactly
`l`'s own announcements. -/
theorem filterCallsByTurn_eq_filterCallsBy (l : List MessageRow) :
    ∀ (R pending : Finset ToolExecution.ToolCallId),
      ResultsOwnedFrom pending l →
      UniqueCallIds l →
      Disjoint pending (callsIn l) →
      (∀ c ∈ callsIn l, (c ∈ R ↔ c ∈ resolvedIn l)) →
      filterCallsByTurn l = filterCallsBy R l := by
  induction l with
  | nil => intro R pending _ _ _ _; rfl
  | cons row rest ih =>
    intro R pending howned huniq hdisj hR
    unfold ResultsOwnedFrom at howned
    cases hk : row.kind with
    | assistantToolCalls callIds =>
      rw [hk] at howned
      have hpair := (uniqueCallIds_cons_assistant row rest callIds hk).mp huniq
      have hdisjPair : Disjoint pending callIds ∧ Disjoint pending (callsIn rest) := by
        rw [callsIn_cons_assistant row rest callIds hk,
          Finset.disjoint_union_right] at hdisj
        exact hdisj
      have hresolvedEq : resolvedIn (row :: rest) = resolvedIn rest :=
        resolvedIn_cons_assistant row rest callIds hk
      have hcallsSub : callIds ⊆ callsIn (row :: rest) := by
        rw [callsIn_cons_assistant row rest callIds hk]
        exact Finset.subset_union_left
      -- `R` decides this row's announcements exactly as the global set does,
      -- and the crux confines them to this turn.
      have hinter : callIds ∩ R = callIds ∩ resolvedInTurn rest := by
        have hstep : callIds ∩ R = callIds ∩ resolvedIn rest := by
          apply Finset.ext; intro c
          simp only [Finset.mem_inter]
          constructor
          · rintro ⟨hcall, hr⟩
            exact ⟨hcall, hresolvedEq ▸ (hR c (hcallsSub hcall)).mp hr⟩
          · rintro ⟨hcall, hres⟩
            exact ⟨hcall, (hR c (hcallsSub hcall)).mpr (hresolvedEq ▸ hres)⟩
        rw [hstep]
        exact inter_resolvedIn_eq_inter_resolvedInTurn rest callIds howned hpair.1
      have hRrest : ∀ c ∈ callsIn rest, (c ∈ R ↔ c ∈ resolvedIn rest) := by
        intro c hc
        rw [← hresolvedEq]
        exact hR c (by
          rw [callsIn_cons_assistant row rest callIds hk]
          exact Finset.mem_union_right _ hc)
      rw [filterCallsByTurn_cons_assistant row rest callIds hk,
        filterCallsBy_cons_assistant row rest R callIds hk, hinter,
        ih R callIds howned hpair.2 hpair.1 hRrest]
    | toolResult callId key =>
      rw [hk] at howned
      have huniq' := (uniqueCallIds_cons_result row rest callId key hk).mp huniq
      have hdisj' : Disjoint pending (callsIn rest) := by
        rwa [callsIn_cons_result row rest callId key hk] at hdisj
      -- The closed call belongs to an earlier announcement, so `UniqueCallIds`
      -- keeps it out of everything the tail announces.
      have hnotCall : callId ∉ callsIn rest :=
        fun hc => Finset.disjoint_left.mp hdisj' howned.1 hc
      have hRrest : ∀ c ∈ callsIn rest, (c ∈ R ↔ c ∈ resolvedIn rest) := by
        intro c hc
        have hne : c ≠ callId := fun h => hnotCall (h ▸ hc)
        have := hR c (by rwa [callsIn_cons_result row rest callId key hk])
        rw [resolvedIn_cons_result row rest callId key hk] at this
        simpa [Finset.mem_insert, hne] using this
      rw [filterCallsByTurn_cons_result row rest callId key hk,
        filterCallsBy_cons_result row rest R callId key hk,
        ih R (pending.erase callId) howned.2 huniq'
          (by
            rw [Finset.disjoint_left]
            intro x hx hcall
            exact Finset.disjoint_left.mp hdisj' (Finset.mem_of_mem_erase hx) hcall)
          hRrest]
    | ordinary =>
      rw [hk] at howned
      have huniq' := (uniqueCallIds_cons_ordinary row rest hk).mp huniq
      have hRrest : ∀ c ∈ callsIn rest, (c ∈ R ↔ c ∈ resolvedIn rest) := by
        intro c hc
        have := hR c (by rwa [callsIn_cons_ordinary row rest hk])
        rwa [resolvedIn_cons_ordinary row rest hk] at this
      rw [filterCallsByTurn_cons_ordinary row rest hk,
        filterCallsBy_cons_ordinary row rest R hk,
        ih R ∅ howned huniq' (Finset.disjoint_empty_left _) hRrest]

/-- **The alignment.** The per-turn sanitizer production implements and the
global-resolution model the pairing theorems are stated over are the same
function on unique-id transcripts. -/
theorem sanitizeTurn_eq_sanitize {msgs : List MessageRow}
    (huniq : UniqueCallIds msgs) : sanitizeTurn msgs = sanitize msgs := by
  unfold sanitizeTurn sanitize dropUnpairedCallsTurn dropUnpairedCalls dropOrphanedResults
  exact filterCallsByTurn_eq_filterCallsBy (dropOrphanedFrom ∅ msgs)
    (resolvedIn (dropOrphanedFrom ∅ msgs)) ∅
    (resultsOwnedFrom_dropOrphanedFrom msgs ∅)
    (uniqueCallIds_dropOrphanedFrom msgs ∅ huniq)
    (Finset.disjoint_empty_left _)
    (fun _ _ => Iff.rfl)

theorem sanitize_fixpoint {msgs : List MessageRow} (hvalid : ProviderValid msgs)
    (hne : NonemptyAnnouncements msgs) : sanitize msgs = msgs := by
  unfold sanitize dropUnpairedCalls dropOrphanedResults
  rw [dropOrphanedFrom_eq_self msgs ∅ hvalid.activeBlockValid]
  exact filterCallsBy_eq_self msgs (resolvedIn msgs) ∅ hvalid.activeBlockValid
    hne (Finset.Subset.refl _)

theorem nonemptyAnnouncements_sanitize (msgs : List MessageRow) :
    NonemptyAnnouncements (sanitize msgs) :=
  nonempty_filterCallsBy _ _

theorem sanitize_idempotent {msgs : List MessageRow}
    (huniq : UniqueCallIds msgs) :
    sanitize (sanitize msgs) = sanitize msgs :=
  sanitize_fixpoint (sanitize_sound huniq) (nonemptyAnnouncements_sanitize msgs)

inductive ResultBlock : Finset ToolExecution.ToolCallId → List MessageRow → Prop
  | nil : ResultBlock ∅ []
  | cons {S : Finset ToolExecution.ToolCallId} {row : MessageRow}
      {rest : List MessageRow} (callId : ToolExecution.ToolCallId)
      (key : Transcript.ToolResultKey)
      (hk : row.kind = .toolResult callId key) (hmem : callId ∈ S)
      (hrest : ResultBlock (S.erase callId) rest) :
      ResultBlock S (row :: rest)

theorem ResultBlock.activeBlockValid {S : Finset ToolExecution.ToolCallId}
    {l : List MessageRow} (h : ResultBlock S l) : ActiveBlockValidFrom S l := by
  induction h with
  | nil => simp
  | cons callId key hk hmem hrest ih =>
    rw [activeBlockValidFrom_cons_result _ _ _ callId key hk]
    exact ⟨hmem, ih⟩

theorem ResultBlock.nonemptyAnnouncements {S : Finset ToolExecution.ToolCallId}
    {l : List MessageRow} (h : ResultBlock S l) : NonemptyAnnouncements l := by
  induction h with
  | nil => simp
  | cons callId key hk hmem hrest ih =>
    rw [nonemptyAnnouncements_cons_other _ _
      (by intro c hc; rw [hk] at hc; exact MessageKind.noConfusion hc)]
    exact ih

theorem threaded_turn_fixpoint {row : MessageRow}
    {S : Finset ToolExecution.ToolCallId} {results : List MessageRow}
    (hrow : row.kind = .assistantToolCalls S) (hne : S ≠ ∅)
    (hresults : ResultBlock S results) :
    ProviderValid (row :: results) ∧ sanitize (row :: results) = row :: results := by
  have hvalid : ProviderValid (row :: results) := by
    constructor
    rw [ActiveBlockValid, activeBlockValidFrom_cons_assistant row results ∅ S hrow]
    exact ⟨rfl, hresults.activeBlockValid⟩
  refine ⟨hvalid, sanitize_fixpoint hvalid ?_⟩
  rw [nonemptyAnnouncements_cons_assistant row results S hrow]
  exact ⟨hne, hresults.nonemptyAnnouncements⟩

theorem assemble_spec (skillCount summaryCount conversationLen : Nat) :
    assemble skillCount summaryCount conversationLen =
      Slot.preamble ::
        ((List.range skillCount).map Slot.skillReminder ++
          ((if summaryCount = 0 then [] else [Slot.summaryReminder]) ++
            (List.range conversationLen).map Slot.conversation)) ++
        [Slot.prompt] := rfl

theorem assemble_head (skillCount summaryCount conversationLen : Nat) :
    (assemble skillCount summaryCount conversationLen).head? =
      some Slot.preamble := rfl

theorem assemble_last (skillCount summaryCount conversationLen : Nat) :
    (assemble skillCount summaryCount conversationLen).getLast? =
      some Slot.prompt := by
  rw [assemble_spec, ← List.cons_append, List.getLast?_concat]

example :
    sanitize
      [⟨0, 0, 0, .user, .toolResult 1 ⟨0, 0, 0⟩⟩,
        ⟨1, 0, 1, .assistant, .assistantToolCalls {1}⟩] = [] := by
  rfl

example :
    sanitize
      [⟨0, 0, 0, .assistant, .assistantToolCalls {1}⟩,
        ⟨1, 0, 1, .user, .ordinary⟩,
        ⟨2, 0, 2, .user, .toolResult 1 ⟨0, 0, 0⟩⟩] =
      [⟨1, 0, 1, .user, .ordinary⟩] := by
  rfl

end PromptAssembly
