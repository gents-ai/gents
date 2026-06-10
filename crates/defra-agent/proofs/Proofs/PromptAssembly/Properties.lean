import Proofs.PromptAssembly.Executable

/-!
# PromptAssembly Properties (issue #448)

The provider-input narrowing theorems, in the labeling of the design:

- **T1 `sanitize_sound`** — under `UniqueCallIds` (call ids are uuids — the
  system invariant), `sanitize` always produces `ProviderValid` history.
  Quantified over arbitrary lists, so **T4 split-stability falls out as a
  corollary**: any pair-blind compaction split's recent window is just
  another list whose uniqueness is inherited from the whole.
- **T2 `sanitize_fixpoint`** — `sanitize` is the identity on already-valid
  history (with non-empty announcements, which `sanitize` itself always
  outputs). This is the preservation statement: nothing provider-valid is
  ever dropped.
- **T3 `sanitize_idempotent`** — T1 ∘ T2.
- **T4 `sanitize_split_stable`** — see T1.
- **T5 `threaded_turn_fixpoint`** — the owned loop's threaded turn shape
  (one assistant tool-call row followed by its results) is provider-valid
  and a fixpoint of `sanitize`. This is the formal justification for
  sanitizing ONLY the loaded history at the `run_loop_stream` entry
  chokepoint: the loop's own in-flight messages need no repair, and
  sanitizing them mid-flight would be wrong (a result can ride as the next
  turn's prompt).
- **Assembly lemmas** — the layer order of the assembled request is fixed
  by definition: preamble first, skill reminders before the summary
  reminder and conversation, the new prompt last.

The composition ORDER of `sanitize` (orphans first) is load-bearing: with
the swapped order, `[result A, call A]` keeps the call on the strength of
the result it then drops, and an unpaired call reaches the provider. The
swap was a live bug in the Rust sanitizer found while sketching T1; the
counterexample is pinned here (`sanitize_repairs_result_before_call`) and in
`crates/defra-agent/tests/prompt_assembly_conformance.rs` /
`compaction/tests.rs::sanitize_repairs_result_preceding_its_call`.
-/

namespace PromptAssembly

open Transcript (MessageRow MessageKind MessageRole)

/-- Every assistant tool-call row announces at least one call. The Rust side
only constructs tool-call messages from non-empty call lists; the
`.assistantToolCalls ∅` row is a modeling artifact excluded here. `sanitize`
re-establishes this unconditionally (`nonempty_filterCallsBy`). -/
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

/-! ## dropOrphanedResults lemmas -/

/-- Orphan-dropping never removes assistant rows, so the announced call set
is unchanged. (This stability is why orphan-drop is safe to run FIRST.) -/
theorem callsIn_dropOrphanedFrom (l : List MessageRow) :
    ∀ seen, callsIn (dropOrphanedFrom seen l) = callsIn l := by
  induction l with
  | nil => intro seen; rfl
  | cons row rest ih =>
    intro seen
    cases hk : row.kind with
    | toolResult callId key =>
      rw [dropOrphanedFrom_cons_result row rest seen callId key hk]
      by_cases hc : callId ∈ seen
      · rw [if_pos hc, callsIn_cons_result row _ callId key hk,
          callsIn_cons_result row rest callId key hk, ih seen]
      · rw [if_neg hc, ih seen, callsIn_cons_result row rest callId key hk]
    | assistantToolCalls callIds =>
      rw [dropOrphanedFrom_cons_assistant row rest seen callIds hk,
        callsIn_cons_assistant row _ callIds hk,
        callsIn_cons_assistant row rest callIds hk, ih (seen ∪ callIds)]
    | ordinary =>
      rw [dropOrphanedFrom_cons_ordinary row rest seen hk,
        callsIn_cons_ordinary row _ hk, callsIn_cons_ordinary row rest hk,
        ih seen]

theorem uniqueCallIds_dropOrphanedFrom (l : List MessageRow) :
    ∀ seen, UniqueCallIds l → UniqueCallIds (dropOrphanedFrom seen l) := by
  induction l with
  | nil => intro seen _; simp
  | cons row rest ih =>
    intro seen huniq
    cases hk : row.kind with
    | toolResult callId key =>
      rw [dropOrphanedFrom_cons_result row rest seen callId key hk]
      have hrest := (uniqueCallIds_cons_result row rest callId key hk).mp huniq
      by_cases hc : callId ∈ seen
      · rw [if_pos hc, uniqueCallIds_cons_result row _ callId key hk]
        exact ih seen hrest
      · rw [if_neg hc]; exact ih seen hrest
    | assistantToolCalls callIds =>
      rw [dropOrphanedFrom_cons_assistant row rest seen callIds hk]
      have h := (uniqueCallIds_cons_assistant row rest callIds hk).mp huniq
      rw [uniqueCallIds_cons_assistant row _ callIds hk]
      exact ⟨by rw [callsIn_dropOrphanedFrom rest (seen ∪ callIds)]; exact h.1,
        ih (seen ∪ callIds) h.2⟩
    | ordinary =>
      rw [dropOrphanedFrom_cons_ordinary row rest seen hk]
      have hrest := (uniqueCallIds_cons_ordinary row rest hk).mp huniq
      rw [uniqueCallIds_cons_ordinary row _ hk]
      exact ih seen hrest

theorem nonemptyAnnouncements_dropOrphanedFrom (l : List MessageRow) :
    ∀ seen, NonemptyAnnouncements l →
      NonemptyAnnouncements (dropOrphanedFrom seen l) := by
  induction l with
  | nil => intro seen _; simp
  | cons row rest ih =>
    intro seen hne
    cases hk : row.kind with
    | toolResult callId key =>
      rw [dropOrphanedFrom_cons_result row rest seen callId key hk]
      have hrest := (nonemptyAnnouncements_cons_other row rest
        (by intro callIds h; rw [hk] at h; exact MessageKind.noConfusion h)).mp hne
      by_cases hc : callId ∈ seen
      · rw [if_pos hc, nonemptyAnnouncements_cons_other row _
          (by intro callIds h; rw [hk] at h; exact MessageKind.noConfusion h)]
        exact ih seen hrest
      · rw [if_neg hc]; exact ih seen hrest
    | assistantToolCalls callIds =>
      rw [dropOrphanedFrom_cons_assistant row rest seen callIds hk]
      have h := (nonemptyAnnouncements_cons_assistant row rest callIds hk).mp hne
      rw [nonemptyAnnouncements_cons_assistant row _ callIds hk]
      exact ⟨h.1, ih (seen ∪ callIds) h.2⟩
    | ordinary =>
      rw [dropOrphanedFrom_cons_ordinary row rest seen hk]
      have hrest := (nonemptyAnnouncements_cons_other row rest
        (by intro callIds h; rw [hk] at h; exact MessageKind.noConfusion h)).mp hne
      rw [nonemptyAnnouncements_cons_other row _
        (by intro callIds h; rw [hk] at h; exact MessageKind.noConfusion h)]
      exact ih seen hrest

/-- Step A: orphan-dropping establishes results-follow-calls relative to the
accumulator, unconditionally. -/
theorem resultsFollow_dropOrphanedFrom (l : List MessageRow) :
    ∀ seen, ResultsFollowCallsFrom seen (dropOrphanedFrom seen l) := by
  induction l with
  | nil => intro seen; simp
  | cons row rest ih =>
    intro seen
    cases hk : row.kind with
    | toolResult callId key =>
      rw [dropOrphanedFrom_cons_result row rest seen callId key hk]
      by_cases hc : callId ∈ seen
      · rw [if_pos hc,
          resultsFollowFrom_cons_result row _ seen callId key hk]
        exact ⟨hc, ih seen⟩
      · rw [if_neg hc]; exact ih seen
    | assistantToolCalls callIds =>
      rw [dropOrphanedFrom_cons_assistant row rest seen callIds hk,
        resultsFollowFrom_cons_assistant row _ seen callIds hk]
      exact ih (seen ∪ callIds)
    | ordinary =>
      rw [dropOrphanedFrom_cons_ordinary row rest seen hk,
        resultsFollowFrom_cons_ordinary row _ seen hk]
      exact ih seen

/-! ## dropUnpairedCalls lemmas -/

/-- Call-filtering never touches result rows. -/
theorem resolvedIn_filterCallsBy (resolved : Finset ToolExecution.ToolCallId)
    (l : List MessageRow) :
    resolvedIn (filterCallsBy resolved l) = resolvedIn l := by
  induction l with
  | nil => rfl
  | cons row rest ih =>
    cases hk : row.kind with
    | assistantToolCalls callIds =>
      rw [filterCallsBy_cons_assistant row rest resolved callIds hk,
        resolvedIn_cons_assistant row rest callIds hk]
      by_cases hempty : callIds ∩ resolved = ∅
      · rw [if_pos hempty, ih]
      · rw [if_neg hempty,
          resolvedIn_cons_assistant (withKind row _) _ (callIds ∩ resolved)
            (withKind_kind row _), ih]
    | toolResult callId key =>
      rw [filterCallsBy_cons_result row rest resolved callId key hk,
        resolvedIn_cons_result row _ callId key hk,
        resolvedIn_cons_result row rest callId key hk, ih]
    | ordinary =>
      rw [filterCallsBy_cons_ordinary row rest resolved hk,
        resolvedIn_cons_ordinary row _ hk,
        resolvedIn_cons_ordinary row rest hk, ih]

/-- Call-filtering always outputs non-empty announcements (the empty-set
branch drops the row). -/
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

/-- Results-follow-calls survives call-filtering: a kept result's call id is
resolved by the result itself, so the announcing row keeps that id. -/
theorem resultsFollow_filterCallsBy (R : Finset ToolExecution.ToolCallId)
    (l : List MessageRow) :
    ∀ seen, ResultsFollowCallsFrom seen l → resolvedIn l ⊆ R →
      ResultsFollowCallsFrom (seen ∩ R) (filterCallsBy R l) := by
  induction l with
  | nil => intro seen _ _; simp
  | cons row rest ih =>
    intro seen hfollow hres
    cases hk : row.kind with
    | assistantToolCalls callIds =>
      have hfollow' :=
        (resultsFollowFrom_cons_assistant row rest seen callIds hk).mp hfollow
      have hres' : resolvedIn rest ⊆ R := by
        rwa [resolvedIn_cons_assistant row rest callIds hk] at hres
      rw [filterCallsBy_cons_assistant row rest R callIds hk]
      by_cases hempty : callIds ∩ R = ∅
      · rw [if_pos hempty]
        have := ih (seen ∪ callIds) hfollow' hres'
        rwa [Finset.union_inter_distrib_right, hempty,
          Finset.union_empty] at this
      · rw [if_neg hempty,
          resultsFollowFrom_cons_assistant (withKind row _) _ (seen ∩ R)
            (callIds ∩ R) (withKind_kind row _)]
        have := ih (seen ∪ callIds) hfollow' hres'
        rwa [Finset.union_inter_distrib_right] at this
    | toolResult callId key =>
      have hfollow' :=
        (resultsFollowFrom_cons_result row rest seen callId key hk).mp hfollow
      rw [resolvedIn_cons_result row rest callId key hk,
        Finset.insert_subset_iff] at hres
      rw [filterCallsBy_cons_result row rest R callId key hk,
        resultsFollowFrom_cons_result row _ (seen ∩ R) callId key hk]
      exact ⟨Finset.mem_inter.mpr ⟨hfollow'.1, hres.1⟩,
        ih seen hfollow'.2 hres.2⟩
    | ordinary =>
      have hfollow' :=
        (resultsFollowFrom_cons_ordinary row rest seen hk).mp hfollow
      have hres' : resolvedIn rest ⊆ R := by
        rwa [resolvedIn_cons_ordinary row rest hk] at hres
      rw [filterCallsBy_cons_ordinary row rest R hk,
        resultsFollowFrom_cons_ordinary row _ (seen ∩ R) hk]
      exact ih seen hfollow' hres'

/-- Calls-followed-by-results holds for the output of call-filtering on an
orphan-free, unique-call-id list. The carried context: `seen` is the calls
announced by the already-processed prefix, `extra ⊆ seen` the results the
prefix resolved (orphan-freedom: prefix results only resolve prefix calls),
and `Disjoint seen (callsIn l)` the uniqueness frontier. A kept call id must
then be resolved by a LATER result: it cannot be in `extra` (it was just
announced, and announcements are unique), so it is resolved in the tail. -/
theorem callsFollowed_filterCallsBy (l : List MessageRow) :
    ∀ (seen extra : Finset ToolExecution.ToolCallId),
      ResultsFollowCallsFrom seen l →
      UniqueCallIds l →
      Disjoint seen (callsIn l) →
      extra ⊆ seen →
      CallsFollowedByResults (filterCallsBy (extra ∪ resolvedIn l) l) := by
  induction l with
  | nil => intro seen extra _ _ _ _; simp
  | cons row rest ih =>
    intro seen extra hfollow huniq hdisj hextra
    cases hk : row.kind with
    | assistantToolCalls callIds =>
      have hfollow' :=
        (resultsFollowFrom_cons_assistant row rest seen callIds hk).mp hfollow
      have huniq' := (uniqueCallIds_cons_assistant row rest callIds hk).mp huniq
      have hdisjPair : Disjoint seen callIds ∧ Disjoint seen (callsIn rest) := by
        rw [callsIn_cons_assistant row rest callIds hk,
          Finset.disjoint_union_right] at hdisj
        exact hdisj
      have hdisj' : Disjoint (seen ∪ callIds) (callsIn rest) :=
        Finset.disjoint_union_left.mpr ⟨hdisjPair.2, huniq'.1⟩
      have hextra' : extra ⊆ seen ∪ callIds :=
        hextra.trans Finset.subset_union_left
      have hR : extra ∪ resolvedIn (row :: rest) = extra ∪ resolvedIn rest := by
        rw [resolvedIn_cons_assistant row rest callIds hk]
      rw [hR, filterCallsBy_cons_assistant row rest _ callIds hk]
      by_cases hempty : callIds ∩ (extra ∪ resolvedIn rest) = ∅
      · rw [if_pos hempty]
        exact ih (seen ∪ callIds) extra hfollow' huniq'.2 hdisj' hextra'
      · rw [if_neg hempty,
          callsFollowed_cons_assistant (withKind row _) _
            (callIds ∩ (extra ∪ resolvedIn rest)) (withKind_kind row _)]
        refine ⟨?_, ih (seen ∪ callIds) extra hfollow' huniq'.2 hdisj' hextra'⟩
        rw [resolvedIn_filterCallsBy]
        intro c hc
        have hcS := (Finset.mem_inter.mp hc).1
        have hcR := (Finset.mem_inter.mp hc).2
        rcases Finset.mem_union.mp hcR with hcExtra | hcRest
        · exact absurd hcS
            (Finset.disjoint_left.mp hdisjPair.1 (hextra hcExtra))
        · exact hcRest
    | toolResult callId key =>
      have hfollow' :=
        (resultsFollowFrom_cons_result row rest seen callId key hk).mp hfollow
      have huniq' := (uniqueCallIds_cons_result row rest callId key hk).mp huniq
      have hdisj' : Disjoint seen (callsIn rest) := by
        rwa [callsIn_cons_result row rest callId key hk] at hdisj
      have hextra' : insert callId extra ⊆ seen :=
        Finset.insert_subset_iff.mpr ⟨hfollow'.1, hextra⟩
      have hR : extra ∪ resolvedIn (row :: rest) =
          insert callId extra ∪ resolvedIn rest := by
        rw [resolvedIn_cons_result row rest callId key hk,
          Finset.union_insert, Finset.insert_union]
      rw [hR, filterCallsBy_cons_result row rest _ callId key hk,
        callsFollowed_cons_result row _ callId key hk]
      exact ih seen (insert callId extra) hfollow'.2 huniq' hdisj' hextra'
    | ordinary =>
      have hfollow' :=
        (resultsFollowFrom_cons_ordinary row rest seen hk).mp hfollow
      have huniq' := (uniqueCallIds_cons_ordinary row rest hk).mp huniq
      have hdisj' : Disjoint seen (callsIn rest) := by
        rwa [callsIn_cons_ordinary row rest hk] at hdisj
      have hR : extra ∪ resolvedIn (row :: rest) = extra ∪ resolvedIn rest := by
        rw [resolvedIn_cons_ordinary row rest hk]
      rw [hR, filterCallsBy_cons_ordinary row rest _ hk,
        callsFollowed_cons_ordinary row _ hk]
      exact ih seen extra hfollow' huniq' hdisj' hextra

/-! ## T1 + T4: soundness and split-stability -/

/-- **T1 (soundness).** Under unique call ids, `sanitize` always produces
provider-valid history — for ANY input list, including the recent window of
a pair-blind compaction split (T4) and arbitrarily corrupt loaded
transcripts. -/
theorem sanitize_sound {msgs : List MessageRow} (huniq : UniqueCallIds msgs) :
    ProviderValid (sanitize msgs) := by
  unfold sanitize dropUnpairedCalls dropOrphanedResults
  set orphanFree := dropOrphanedFrom ∅ msgs with horphan
  have hfollow : ResultsFollowCallsFrom ∅ orphanFree := by
    rw [horphan]; exact resultsFollow_dropOrphanedFrom msgs ∅
  have huniq' : UniqueCallIds orphanFree := by
    rw [horphan]; exact uniqueCallIds_dropOrphanedFrom msgs ∅ huniq
  constructor
  · have := resultsFollow_filterCallsBy (resolvedIn orphanFree) orphanFree ∅
      hfollow (Finset.Subset.refl _)
    rwa [Finset.empty_inter] at this
  · have := callsFollowed_filterCallsBy orphanFree ∅ ∅ hfollow huniq'
      (Finset.disjoint_empty_left _) (Finset.Subset.refl _)
    rwa [Finset.empty_union] at this

/-- **T4 (split-stability).** Any suffix window — in particular the recent
window of `split_messages_for_summary`, which is token-budgeted and
pair-blind — sanitizes to provider-valid history. A direct corollary of T1
plus uniqueness inheritance. -/
theorem sanitize_split_stable {old recent : List MessageRow}
    (huniq : UniqueCallIds (old ++ recent)) :
    ProviderValid (sanitize recent) :=
  sanitize_sound (UniqueCallIds.of_append_right huniq)

/-! ## T2 + T3: fixpoint and idempotence -/

theorem dropOrphanedFrom_eq_self (l : List MessageRow) :
    ∀ seen, ResultsFollowCallsFrom seen l → dropOrphanedFrom seen l = l := by
  induction l with
  | nil => intro seen _; rfl
  | cons row rest ih =>
    intro seen hfollow
    cases hk : row.kind with
    | toolResult callId key =>
      have h := (resultsFollowFrom_cons_result row rest seen callId key hk).mp
        hfollow
      rw [dropOrphanedFrom_cons_result row rest seen callId key hk,
        if_pos h.1, ih seen h.2]
    | assistantToolCalls callIds =>
      have h :=
        (resultsFollowFrom_cons_assistant row rest seen callIds hk).mp hfollow
      rw [dropOrphanedFrom_cons_assistant row rest seen callIds hk,
        ih (seen ∪ callIds) h]
    | ordinary =>
      have h := (resultsFollowFrom_cons_ordinary row rest seen hk).mp hfollow
      rw [dropOrphanedFrom_cons_ordinary row rest seen hk, ih seen h]

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
    ∀ (R : Finset ToolExecution.ToolCallId),
      CallsFollowedByResults l → NonemptyAnnouncements l →
      resolvedIn l ⊆ R → filterCallsBy R l = l := by
  induction l with
  | nil => intro R _ _ _; rfl
  | cons row rest ih =>
    intro R hfollowed hne hres
    have hres' : resolvedIn rest ⊆ R :=
      (resolvedIn_subset_cons row rest).trans hres
    cases hk : row.kind with
    | assistantToolCalls callIds =>
      have h := (callsFollowed_cons_assistant row rest callIds hk).mp hfollowed
      have hne' := (nonemptyAnnouncements_cons_assistant row rest callIds hk).mp
        hne
      have hsub : callIds ⊆ R := h.1.trans hres'
      have hinter : callIds ∩ R = callIds := Finset.inter_eq_left.mpr hsub
      rw [filterCallsBy_cons_assistant row rest R callIds hk, hinter,
        if_neg hne'.1, withKind_self row _ hk, ih R h.2 hne'.2 hres']
    | toolResult callId key =>
      have h := (callsFollowed_cons_result row rest callId key hk).mp hfollowed
      have hne' := (nonemptyAnnouncements_cons_other row rest
        (by intro c hc; rw [hk] at hc; exact MessageKind.noConfusion hc)).mp hne
      rw [filterCallsBy_cons_result row rest R callId key hk, ih R h hne' hres']
    | ordinary =>
      have h := (callsFollowed_cons_ordinary row rest hk).mp hfollowed
      have hne' := (nonemptyAnnouncements_cons_other row rest
        (by intro c hc; rw [hk] at hc; exact MessageKind.noConfusion hc)).mp hne
      rw [filterCallsBy_cons_ordinary row rest R hk, ih R h hne' hres']

/-- **T2 (fixpoint / preservation).** `sanitize` is the identity on
provider-valid history with non-empty announcements: nothing valid is ever
dropped or reordered. -/
theorem sanitize_fixpoint {msgs : List MessageRow} (hvalid : ProviderValid msgs)
    (hne : NonemptyAnnouncements msgs) : sanitize msgs = msgs := by
  unfold sanitize dropUnpairedCalls dropOrphanedResults
  rw [dropOrphanedFrom_eq_self msgs ∅ hvalid.resultsFollowCalls,
    filterCallsBy_eq_self msgs (resolvedIn msgs) hvalid.callsFollowedByResults
      hne (Finset.Subset.refl _)]

/-- `sanitize` always outputs non-empty announcements (the filter's
empty-set branch drops the row), so T2 applies to its own output. -/
theorem nonemptyAnnouncements_sanitize (msgs : List MessageRow) :
    NonemptyAnnouncements (sanitize msgs) :=
  nonempty_filterCallsBy _ _

/-- **T3 (idempotence).** One pass through the boundary is enough. -/
theorem sanitize_idempotent {msgs : List MessageRow}
    (huniq : UniqueCallIds msgs) :
    sanitize (sanitize msgs) = sanitize msgs :=
  sanitize_fixpoint (sanitize_sound huniq) (nonemptyAnnouncements_sanitize msgs)

/-! ## T5: the owned loop's threaded turn is a fixpoint -/

/-- Every row is a tool result resolving a call in `S` — the shape of the
result block the owned loop threads after an assistant turn. -/
def AllResultsIn (S : Finset ToolExecution.ToolCallId) :
    List MessageRow → Prop
  | [] => True
  | row :: rest =>
    (∃ callId key, row.kind = .toolResult callId key ∧ callId ∈ S) ∧
      AllResultsIn S rest

theorem AllResultsIn.resultsFollow {S : Finset ToolExecution.ToolCallId} :
    ∀ {l : List MessageRow}, AllResultsIn S l → ResultsFollowCallsFrom S l := by
  intro l
  induction l with
  | nil => intro _; simp
  | cons row rest ih =>
    intro h
    obtain ⟨⟨callId, key, hk, hmem⟩, hrest⟩ := h
    rw [resultsFollowFrom_cons_result row rest S callId key hk]
    exact ⟨hmem, ih hrest⟩

theorem AllResultsIn.callsFollowed {S : Finset ToolExecution.ToolCallId} :
    ∀ {l : List MessageRow}, AllResultsIn S l → CallsFollowedByResults l := by
  intro l
  induction l with
  | nil => intro _; simp
  | cons row rest ih =>
    intro h
    obtain ⟨⟨callId, key, hk, _⟩, hrest⟩ := h
    rw [callsFollowed_cons_result row rest callId key hk]
    exact ih hrest

theorem AllResultsIn.nonempty {S : Finset ToolExecution.ToolCallId} :
    ∀ {l : List MessageRow}, AllResultsIn S l → NonemptyAnnouncements l := by
  intro l
  induction l with
  | nil => intro _; simp
  | cons row rest ih =>
    intro h
    obtain ⟨⟨callId, key, hk, _⟩, hrest⟩ := h
    rw [nonemptyAnnouncements_cons_other row rest
      (by intro c hc; rw [hk] at hc; exact MessageKind.noConfusion hc)]
    exact ih hrest

/-- **T5 (loop-threading validity).** The canonical turn the owned loop
threads — one assistant tool-call row, then its results — is provider-valid
and a fixpoint of `sanitize`. Formal justification for the `run_loop_stream`
entry chokepoint sanitizing ONLY the loaded history: the loop's own
in-flight messages need no repair. -/
theorem threaded_turn_fixpoint {row : MessageRow}
    {S : Finset ToolExecution.ToolCallId} {results : List MessageRow}
    (hrow : row.kind = .assistantToolCalls S) (hne : S ≠ ∅)
    (hresults : AllResultsIn S results) (hresolved : S ⊆ resolvedIn results) :
    ProviderValid (row :: results) ∧ sanitize (row :: results) = row :: results := by
  have hvalid : ProviderValid (row :: results) := by
    constructor
    · rw [ResultsFollowCalls, resultsFollowFrom_cons_assistant row results ∅ S hrow,
        Finset.empty_union]
      exact hresults.resultsFollow
    · rw [callsFollowed_cons_assistant row results S hrow]
      exact ⟨hresolved, hresults.callsFollowed⟩
  refine ⟨hvalid, sanitize_fixpoint hvalid ?_⟩
  rw [nonemptyAnnouncements_cons_assistant row results S hrow]
  exact ⟨hne, hresults.nonempty⟩

/-! ## Assembly order -/

/-- The assembled request, fully unfolded: preamble first, then skill
reminders, then the optional summary reminder, then the conversation, then
the new prompt. Any reordering of `prompt.rs` / `daemon/request.rs` /
`loop_stream.rs` layer composition breaks this `rfl`. -/
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

/-! ## The composition-order counterexample, repaired

`[result A, call A]` (a result PRECEDING its call — backfill ordering or a
P2P-merged transcript) must sanitize to the empty list: the result is
orphaned, and with it gone the call is unpaired. The swapped composition
(unpaired-drop first) kept the call — the live Rust bug found while
sketching T1. Mirrored in
`compaction/tests.rs::sanitize_repairs_result_preceding_its_call`. -/
example :
    sanitize
      [⟨0, 0, 0, .user, .toolResult 1 ⟨0, 0, 0⟩⟩,
        ⟨1, 0, 1, .assistant, .assistantToolCalls {1}⟩] = [] := by
  rfl

end PromptAssembly
