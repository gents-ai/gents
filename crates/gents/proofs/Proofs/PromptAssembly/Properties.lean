import Proofs.PromptAssembly.Executable

/-!
# PromptAssembly Properties (issue #448)

The provider-input narrowing theorems, in the labeling of the design:

- **T1 `sanitize_sound`** — under `UniqueCallIds` (call ids are uuids — the
  system invariant), `sanitize` always produces active-block-valid provider
  history.
- **T2 `sanitize_fixpoint`** — `sanitize` is the identity on already-valid
  history (with non-empty announcements, which `sanitize` itself always
  outputs).
- **T3 `sanitize_idempotent`** — T1 ∘ T2.
- **T4 `sanitize_split_stable`** — a suffix of a unique transcript inherits
  uniqueness, so T1 applies to pair-blind compaction windows.
- **T5 `threaded_turn_fixpoint`** — the owned loop's threaded turn shape
  (one assistant tool-call row followed by one result row per announced
  call — the syntactic `ResultBlock` shape, bridged to validity by
  `ResultBlock.activeBlockValid`) is a fixpoint of `sanitize`.

The provider-valid shape is stricter than "some earlier call exists":
ordinary conversation or a new assistant turn closes the active tool-call
block. A late result for an older call is stale and must be dropped before
unpaired assistant calls are filtered.
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

/-! ## Basic sanitizer-preservation lemmas -/

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

/-- Surviving tool results resolve either the active pending block or calls
announced later in the scanned suffix. -/
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

/-! ## dropUnpairedCalls lemmas -/

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

/-! ## T1 + T4: soundness and split-stability -/

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

/-- **T1 (soundness).** Under unique call ids, `sanitize` always produces
provider-valid history — for ANY input list, including the recent window of
a pair-blind compaction split (T4) and arbitrarily corrupt loaded
transcripts. -/
theorem sanitize_sound {msgs : List MessageRow} (huniq : UniqueCallIds msgs) :
    ProviderValid (sanitize msgs) := by
  unfold sanitize dropUnpairedCalls dropOrphanedResults
  constructor
  simpa using
    activeBlockValid_filterOrphanedFrom msgs ∅ huniq (Finset.disjoint_empty_left _)

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

/-- **T2 (fixpoint / preservation).** `sanitize` is the identity on
provider-valid history with non-empty announcements: nothing valid is ever
dropped or reordered.

The guarantee is exactly as wide as `ProviderValid`, and the active-block
predicate is deliberately NARROWER than "paired somewhere": a result that
is paired but arrives after its block closed (`[call A, ordinary,
result A]`) is not provider-valid and IS dropped, together with its
now-unpaired call — see the second pinned counterexample at the bottom of
this file. That encodes the strict provider contract (Anthropic and OpenAI
reject results that do not immediately close the announcing turn); lenient
backends would have accepted the wider shape, and narrowing to the strict
contract is the chosen tradeoff. -/
theorem sanitize_fixpoint {msgs : List MessageRow} (hvalid : ProviderValid msgs)
    (hne : NonemptyAnnouncements msgs) : sanitize msgs = msgs := by
  unfold sanitize dropUnpairedCalls dropOrphanedResults
  rw [dropOrphanedFrom_eq_self msgs ∅ hvalid.activeBlockValid]
  exact filterCallsBy_eq_self msgs (resolvedIn msgs) ∅ hvalid.activeBlockValid
    hne (Finset.Subset.refl _)

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

/-- The syntactic shape of the result block the owned loop threads after an
assistant turn announcing `S`: one tool-result row per executed call, each
closing a distinct pending id, jointly draining `S`. Defined WITHOUT
reference to the validity predicate — this is what `loop_stream.rs` emits
(every call gets exactly one result; even a vetoed call threads the
rejection reason as its result). The bridge lemmas below connect this shape
to provider validity; T5 must take the shape, not validity itself, as its
hypothesis — otherwise the theorem assumes its conclusion and a loop change
that stops draining `S` would make it vacuous. -/
inductive ResultBlock : Finset ToolExecution.ToolCallId → List MessageRow → Prop
  | nil : ResultBlock ∅ []
  | cons {S : Finset ToolExecution.ToolCallId} {row : MessageRow}
      {rest : List MessageRow} (callId : ToolExecution.ToolCallId)
      (key : Transcript.ToolResultKey)
      (hk : row.kind = .toolResult callId key) (hmem : callId ∈ S)
      (hrest : ResultBlock (S.erase callId) rest) :
      ResultBlock S (row :: rest)

/-- Bridge: the loop's threaded shape is an active result block. -/
theorem ResultBlock.activeBlockValid {S : Finset ToolExecution.ToolCallId}
    {l : List MessageRow} (h : ResultBlock S l) : ActiveBlockValidFrom S l := by
  induction h with
  | nil => simp
  | cons callId key hk hmem hrest ih =>
    rw [activeBlockValidFrom_cons_result _ _ _ callId key hk]
    exact ⟨hmem, ih⟩

/-- Bridge: a result block contains no assistant rows, so announcements are
trivially non-empty. -/
theorem ResultBlock.nonemptyAnnouncements {S : Finset ToolExecution.ToolCallId}
    {l : List MessageRow} (h : ResultBlock S l) : NonemptyAnnouncements l := by
  induction h with
  | nil => simp
  | cons callId key hk hmem hrest ih =>
    rw [nonemptyAnnouncements_cons_other _ _
      (by intro c hc; rw [hk] at hc; exact MessageKind.noConfusion hc)]
    exact ih

/-- **T5 (loop-threading validity).** The canonical turn the owned loop
threads — one assistant tool-call row, then one result row per announced
call (`ResultBlock`, the loop's syntactic emission shape) — is
provider-valid and a fixpoint of `sanitize`. Formal justification for the
`run_loop_stream` entry chokepoint sanitizing ONLY the loaded history: the
loop's own in-flight messages need no repair. -/
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

/-! ## Counterexamples, repaired

Pinned as executable evaluations so a semantics change here is a
Lean-breaking change. Each has a Rust mirror so the fence holds on both
sides of the conformance boundary.

1. **Composition order.** `[result A, call A]` (a result PRECEDING its
   call — backfill ordering or a P2P-merged transcript) must sanitize to
   the empty list: the result is orphaned, and with it gone the call is
   unpaired. The swapped composition (unpaired-drop first) kept the call —
   a live Rust bug found while sketching T1. Rust mirrors:
   `compaction/tests.rs::sanitize_repairs_result_preceding_its_call`,
   conformance `t1_composition_order_result_before_call_sanitizes_to_empty`.

2. **Ordinary conversation closes the block.** `[call A, ordinary,
   result A]`: under the active-block contract the late result is stale,
   so it is dropped and the call is then unpaired; only the ordinary row
   survives. Rust mirrors:
   `compaction/tests.rs::sanitize_history_for_provider_drops_stale_result_and_now_unpaired_call`,
   conformance `t1_result_after_conversation_resumes_sanitizes_to_plain_history`.
-/

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
