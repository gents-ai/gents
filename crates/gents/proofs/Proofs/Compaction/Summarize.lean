import Proofs.Compaction.Prefix
import Proofs.Compaction.Properties
import Proofs.PromptAssembly.ClaudeMap

/-!
# The real summarize reducer

Defect 1 of #993: the modelled reducer was `id`, so pair closure was proven
about a function that never dropped anything. Production's summarize path splits
the transcript on a token budget at an arbitrary index, and that index can land
between an assistant message carrying a `ToolCall` and the user message carrying
its `ToolResult`. The call is summarized away, the result stays in the retained
tail, and `sanitize_history_for_provider` drops the orphan at loop entry — the
tool's output is gone from the provider view while the summary describes only
the call.

`summarize` is parameterised over the budget index rather than pinning a token
function: what must be proven is that *whatever* index the budget picks, the
reducer stays sound. It does, because `pairSafeBoundary` lands on a complete
turn boundary. A normal straddling split retreats to retain the whole turn; if
that atomic tail itself exceeds the budget, production instead selects the next
complete boundary after it (possibly the end, summarizing every row).
`raw_split_can_orphan` is the counterexample showing that a pair-safe boundary
is load-bearing rather than decoration.
-/

namespace Compaction

open Transcript (MessageRow MessageKind ToolResultKey)
open PromptAssembly (ActiveBlockValid ActiveBlockValidFrom)

/-! ## Fold helpers -/

theorem foldrMax_eq_zero_or_mem :
    ∀ l : List Nat, l.foldr max 0 = 0 ∨ l.foldr max 0 ∈ l := by
  intro l
  induction l with
  | nil => exact Or.inl rfl
  | cons x rest ih =>
      rw [List.foldr_cons]
      rcases Nat.le_total (rest.foldr max 0) x with hle | hle
      · rw [max_eq_left hle]
        exact Or.inr (List.mem_cons_self _ _)
      · rw [max_eq_right hle]
        rcases ih with h | h
        · exact Or.inl h
        · exact Or.inr (List.mem_cons_of_mem _ h)

theorem foldrMax_le (n : Nat) :
    ∀ l : List Nat, (∀ x ∈ l, x ≤ n) → l.foldr max 0 ≤ n := by
  intro l
  induction l with
  | nil => intro _; exact Nat.zero_le n
  | cons x rest ih =>
      intro h
      rw [List.foldr_cons]
      exact max_le (h x (List.mem_cons_self _ _))
        (ih (fun y hy => h y (List.mem_cons_of_mem _ hy)))

/-! ## The pair-safe boundary -/

/-- The token-budget index production selects in `split_messages_for_summary`,
after advancing past an oversized atomic tail when retaining it cannot fit. -/
abbrev SplitPolicy := List MessageRow → Nat

/-- The greatest `j ≤ limit` at which no tool call is awaiting its result.

Production mirrors this in `compaction::history::pair_safe_boundary`: it walks
a candidate index back to the nearest turn boundary. The token splitter first
tries that conservative retreat. If the resulting atomic tail is itself over
budget, it submits the boundary after that complete tail instead. Both choices
enter this reducer only at a boundary where no call awaits its result. -/
def pairSafeBoundary (msgs : List MessageRow) (limit : Nat) : Nat :=
  ((List.range (min limit msgs.length + 1)).filter
    (fun j => decide (pendingAfter ∅ (msgs.take j) = ∅))).foldr max 0

theorem pairSafeBoundary_le (msgs : List MessageRow) (limit : Nat) :
    pairSafeBoundary msgs limit ≤ limit := by
  unfold pairSafeBoundary
  refine foldrMax_le limit _ ?_
  intro x hx
  have hrange : x ∈ List.range (min limit msgs.length + 1) := (List.mem_filter.mp hx).1
  exact (Nat.lt_succ_iff.mp (List.mem_range.mp hrange)).trans (Nat.min_le_left _ _)

theorem pairSafeBoundary_pending_empty (msgs : List MessageRow) (limit : Nat) :
    pendingAfter ∅ (msgs.take (pairSafeBoundary msgs limit)) = ∅ := by
  unfold pairSafeBoundary
  rcases foldrMax_eq_zero_or_mem _ with h | h
  · rw [h]; rfl
  · exact of_decide_eq_true (List.mem_filter.mp h).2

/-- A reduction may summarize no farther than the first independently required
Claude replay row. The retention target selects a preferred boundary; it is
not the full provider context ceiling. Returning `none` means there is no
useful protected prefix to summarize before provider IO. -/
def protectedPairSafeBoundary (msgs : List MessageRow) (rawIndex maxPrefix : Nat) : Option Nat :=
  let split := min (pairSafeBoundary msgs rawIndex) (pairSafeBoundary msgs maxPrefix)
  if split == 0 then none else some split

theorem protectedPairSafeBoundary_bounded (msgs : List MessageRow)
    (rawIndex maxPrefix split : Nat)
    (h : protectedPairSafeBoundary msgs rawIndex maxPrefix = some split) :
    split ≤ maxPrefix := by
  dsimp [protectedPairSafeBoundary] at h
  split_ifs at h
  cases h
  exact (Nat.min_le_right _ _).trans (pairSafeBoundary_le msgs maxPrefix)

theorem protectedPairSafeBoundary_pair_closed (msgs : List MessageRow)
    (rawIndex maxPrefix split : Nat)
    (h : protectedPairSafeBoundary msgs rawIndex maxPrefix = some split) :
    pendingAfter ∅ (msgs.take split) = ∅ := by
  dsimp [protectedPairSafeBoundary] at h
  split_ifs at h
  cases h
  rcases Nat.le_total (pairSafeBoundary msgs rawIndex)
    (pairSafeBoundary msgs maxPrefix) with hle | hle
  · simpa [Nat.min_eq_left hle] using
      pairSafeBoundary_pending_empty msgs rawIndex
  · simpa [Nat.min_eq_right hle] using
      pairSafeBoundary_pending_empty msgs maxPrefix

inductive ProtectedReplaySplitError where
  | cannotFit
  | misalignedAssistantProjection
  | replay (error : PromptAssembly.ClaudeMap.MapError)
  deriving DecidableEq, Repr

/-- A positive client-summary split changes the prefix of every previously
signed Claude block. The protected split first verifies its exact association;
the same checkpoint then retires provider replay without editing audit rows. -/
def prepareProtectedReplayCheckpoint (msgs : List MessageRow)
    (rawIndex maxPrefix : Nat)
    (required : List PromptAssembly.ClaudeMap.ReplayTag)
    (rows : List PromptAssembly.ClaudeMap.TaggedReplayRow) :
    Except ProtectedReplaySplitError PromptAssembly.ClaudeMap.ReplayCheckpoint :=
  if rows.length != (msgs.filter (fun row => row.role == .assistant)).length then
    .error .misalignedAssistantProjection
  else
    match protectedPairSafeBoundary msgs rawIndex maxPrefix with
    | none => .error .cannotFit
    | some split =>
        let assistantSplit := (msgs.take split |>.filter (fun row => row.role == .assistant)).length
        ((PromptAssembly.ClaudeMap.prepareReplayCheckpoint required rows assistantSplit).mapError
          .replay).map PromptAssembly.ClaudeMap.retireForPrefixRewrite

theorem preparedProtectedReplayCheckpoint_uses_replay_owner
    (msgs : List MessageRow) (rawIndex maxPrefix : Nat)
    (required : List PromptAssembly.ClaudeMap.ReplayTag)
    (rows : List PromptAssembly.ClaudeMap.TaggedReplayRow)
    (checkpoint : PromptAssembly.ClaudeMap.ReplayCheckpoint)
    (h : prepareProtectedReplayCheckpoint msgs rawIndex maxPrefix
      required rows = .ok checkpoint) :
    ∃ split, protectedPairSafeBoundary msgs rawIndex maxPrefix = some split ∧
      split ≤ maxPrefix ∧
      rows.length = (msgs.filter (fun row => row.role == .assistant)).length ∧
      ∃ prepared, PromptAssembly.ClaudeMap.prepareReplayCheckpoint required rows
        (msgs.take split |>.filter (fun row => row.role == .assistant)).length =
          .ok prepared ∧
          PromptAssembly.ClaudeMap.retireForPrefixRewrite prepared = checkpoint := by
  unfold prepareProtectedReplayCheckpoint at h
  split_ifs at h with haligned
  cases hs : protectedPairSafeBoundary msgs rawIndex maxPrefix with
    | none => simp [hs] at h
    | some split =>
        simp only [hs] at h
        cases hp : PromptAssembly.ClaudeMap.prepareReplayCheckpoint required rows
            (msgs.take split |>.filter (fun row => row.role == .assistant)).length with
        | error error => simp [hp, Except.mapError, Except.map] at h
        | ok prepared =>
            simp [hp, Except.mapError, Except.map] at h
            cases h
            exact ⟨split, rfl,
              protectedPairSafeBoundary_bounded msgs rawIndex maxPrefix split hs,
              by simpa only [bne_iff_ne, ne_eq, not_not] using haligned,
              prepared, hp, rfl⟩

/-! ## Pair closure of the retained tail -/

theorem announcementsAreAssistant_drop {msgs : List MessageRow} (n : Nat)
    (h : PromptView.AnnouncementsAreAssistant msgs) :
    PromptView.AnnouncementsAreAssistant (msgs.drop n) := by
  intro row hmem callIds hkind
  exact h row (List.drop_subset n msgs hmem) callIds hkind

theorem strictlyIncreasing_drop (n : Nat) :
    ∀ l : List MessageRow,
      Transcript.StrictlyIncreasingMessages l →
        Transcript.StrictlyIncreasingMessages (l.drop n) := by
  induction n with
  | zero => intro l h; simpa using h
  | succ m ih =>
      intro l h
      cases l with
      | nil => simpa using h
      | cons row rest => rw [List.drop_succ_cons]; exact ih rest h.2

/-- Every retained tool result has its announcement inside the same window.

The generalisation over `seen` is what carries the induction: the pending set is
always backed by an assistant announcement already walked past, and once the
window starts with an empty pending set that backing must come from inside the
window itself. -/
theorem activeBlockValidFrom_pairs_closed_aux :
    ∀ (l : List MessageRow) (pending : Finset ToolExecution.ToolCallId)
      (seen : List MessageRow),
      ActiveBlockValidFrom pending l →
      PromptView.AnnouncementsAreAssistant l →
      (∀ c ∈ pending, ∃ caller, caller ∈ seen ∧ caller.role = .assistant ∧
        ∃ S, caller.kind = .assistantToolCalls S ∧ c ∈ S) →
      ∀ row, row ∈ l → ∀ callId key, row.kind = .toolResult callId key →
        ∃ caller, caller ∈ seen ++ l ∧ caller.role = .assistant ∧
          ∃ S, caller.kind = .assistantToolCalls S ∧ callId ∈ S := by
  intro l
  induction l with
  | nil => intro _ _ _ _ _ row hmem; exact absurd hmem (List.not_mem_nil row)
  | cons r rest ih =>
      intro pending seen hblock hroles hseen row hmem callId key hkind
      have hroles' : PromptView.AnnouncementsAreAssistant rest := fun x hx =>
        hroles x (List.mem_cons_of_mem r hx)
      have hshift : ∀ x, x ∈ seen ++ [r] ++ rest → x ∈ seen ++ r :: rest := by
        intro x hx
        simpa [List.append_assoc] using hx
      cases hk : r.kind with
      | toolResult c k =>
          have hsplit := (PromptAssembly.activeBlockValidFrom_cons_result r rest pending c k hk).mp
            hblock
          rcases List.mem_cons.mp hmem with hEq | hrest
          · subst hEq
            rw [hk] at hkind
            injection hkind with hcEq _
            obtain ⟨caller, hcaller, hrole, hann⟩ := hseen c hsplit.1
            exact ⟨caller, List.mem_append_left _ hcaller, hrole, hcEq ▸ hann⟩
          · have hseen' : ∀ c' ∈ pending.erase c, ∃ caller, caller ∈ seen ++ [r] ∧
                caller.role = .assistant ∧
                ∃ S, caller.kind = .assistantToolCalls S ∧ c' ∈ S := by
              intro c' hc'
              obtain ⟨caller, hcaller, hrole, hann⟩ := hseen c' (Finset.mem_of_mem_erase hc')
              exact ⟨caller, List.mem_append_left _ hcaller, hrole, hann⟩
            obtain ⟨caller, hcaller, hrole, hann⟩ :=
              ih (pending.erase c) (seen ++ [r]) hsplit.2 hroles' hseen' row hrest callId key hkind
            exact ⟨caller, hshift caller hcaller, hrole, hann⟩
      | assistantToolCalls S =>
          have hsplit :=
            (PromptAssembly.activeBlockValidFrom_cons_assistant r rest pending S hk).mp hblock
          rcases List.mem_cons.mp hmem with hEq | hrest
          · subst hEq
            rw [hk] at hkind
            exact absurd hkind (by simp)
          · have hseen' : ∀ c' ∈ S, ∃ caller, caller ∈ seen ++ [r] ∧
                caller.role = .assistant ∧
                ∃ T, caller.kind = .assistantToolCalls T ∧ c' ∈ T := by
              intro c' hc'
              exact ⟨r, List.mem_append_right _ (List.mem_cons_self _ _),
                hroles r (List.mem_cons_self _ _) S hk, S, hk, hc'⟩
            obtain ⟨caller, hcaller, hrole, hann⟩ :=
              ih S (seen ++ [r]) hsplit.2 hroles' hseen' row hrest callId key hkind
            exact ⟨caller, hshift caller hcaller, hrole, hann⟩
      | ordinary =>
          have hsplit := (PromptAssembly.activeBlockValidFrom_cons_ordinary r rest pending hk).mp
            hblock
          rcases List.mem_cons.mp hmem with hEq | hrest
          · subst hEq
            rw [hk] at hkind
            exact absurd hkind (by simp)
          · have hseen' : ∀ c' ∈ (∅ : Finset ToolExecution.ToolCallId),
                ∃ caller, caller ∈ seen ++ [r] ∧ caller.role = .assistant ∧
                  ∃ T, caller.kind = .assistantToolCalls T ∧ c' ∈ T := by
              intro c' hc'
              exact absurd hc' (Finset.not_mem_empty c')
            obtain ⟨caller, hcaller, hrole, hann⟩ :=
              ih ∅ (seen ++ [r]) hsplit.2 hroles' hseen' row hrest callId key hkind
            exact ⟨caller, hshift caller hcaller, hrole, hann⟩

theorem activeBlockValid_pairs_closed {l : List MessageRow}
    (hblock : ActiveBlockValid l) (hroles : PromptView.AnnouncementsAreAssistant l) :
    PromptView.PairsClosedInMessages l := by
  intro row hmem callId key hkind
  have h := activeBlockValidFrom_pairs_closed_aux l ∅ [] hblock hroles
    (fun c hc => absurd hc (Finset.not_mem_empty c)) row hmem callId key hkind
  simpa using h

/-- **Pair closure over the real reducer.** Dropping at a pending-empty index of
a provider-valid transcript cannot orphan a tool result. -/
theorem drop_pairs_closed_of_pending_empty {msgs : List MessageRow} {n : Nat}
    (hblock : ActiveBlockValid msgs)
    (hroles : PromptView.AnnouncementsAreAssistant msgs)
    (hpend : pendingAfter ∅ (msgs.take n) = ∅) :
    PromptView.PairsClosedInMessages (msgs.drop n) := by
  have hblock' : ActiveBlockValid (msgs.drop n) := by
    have h := activeBlockValidFrom_append (msgs.take n) (msgs.drop n) ∅
      (by rw [List.take_append_drop]; exact hblock)
    rwa [hpend] at h
  exact activeBlockValid_pairs_closed hblock' (announcementsAreAssistant_drop n hroles)

theorem drop_blockValid_of_pending_empty {msgs : List MessageRow} {n : Nat}
    (hblock : ActiveBlockValid msgs) (hpend : pendingAfter ∅ (msgs.take n) = ∅) :
    ActiveBlockValid (msgs.drop n) := by
  have h := activeBlockValidFrom_append (msgs.take n) (msgs.drop n) ∅
    (by rw [List.take_append_drop]; exact hblock)
  rwa [hpend] at h

/-! ## The reducer -/

/-- Retain the tail from the pair-safe boundary and record a summary handle for
everything dropped. Identity below the gate (nothing to summarize) and identity
while the modelled `safeToReduce` gate is closed. -/
def summarize (policy : SplitPolicy) (handle : SummaryHandle) :
    TranscriptReducer := fun v =>
  if 0 < pairSafeBoundary v.messages (policy v.messages) ∧ PromptView.safeToReduce v then
    { v with
      messages := v.messages.drop (pairSafeBoundary v.messages (policy v.messages))
      summary := some handle }
  else v

theorem summarize_messages_suffix (policy : SplitPolicy) (handle : SummaryHandle)
    (v : PromptView) : (summarize policy handle v).messages <:+ v.messages := by
  unfold summarize
  split
  · exact List.drop_suffix _ _
  · exact List.suffix_refl _

theorem summarize_sessionId (policy : SplitPolicy) (handle : SummaryHandle) (v : PromptView) :
    (summarize policy handle v).sessionId = v.sessionId := by
  unfold summarize; split <;> rfl

theorem summarize_coherent (policy : SplitPolicy) (handle : SummaryHandle) (v : PromptView)
    (h : PromptView.ViewCoherent v) : PromptView.ViewCoherent (summarize policy handle v) := by
  unfold summarize
  split
  · refine ⟨?_, ?_, ?_, ?_⟩
    · exact drop_pairs_closed_of_pending_empty h.blockValid h.announcementsAssistant
        (pairSafeBoundary_pending_empty _ _)
    · exact strictlyIncreasing_drop _ _ h.ordered
    · exact drop_blockValid_of_pending_empty h.blockValid (pairSafeBoundary_pending_empty _ _)
    · exact announcementsAreAssistant_drop _ h.announcementsAssistant
  · exact h

instance instIsValidReducerSummarize (policy : SplitPolicy)
    (handle : SummaryHandle) : IsValidReducer (summarize policy handle) where
  gate v := 0 < pairSafeBoundary v.messages (policy v.messages)
  decGate v := Nat.decLt _ _
  preservesCoherent := summarize_coherent policy handle
  preservesOrder := by
    intro v h
    unfold summarize
    split
    · exact strictlyIncreasing_drop _ _ h
    · exact h
  preservesSession := summarize_sessionId policy handle
  identityBelowGate := by
    intro v hgate
    unfold summarize
    rw [if_neg (fun hc => hgate hc.1)]
  identityUnlessSafe := by
    intro v hsafe
    unfold summarize
    rw [if_neg (fun hc => hsafe hc.2)]

/-! ## Why the boundary retreat is load-bearing -/

/-- The unadjusted budget index is unsound: it can land between an assistant
announcement and its result, orphaning the result in the retained tail.

This is the counterexample the acceptance criterion asks for — reverting
`pairSafeBoundary` in production must fail a proof, and this is the proof that
depends on it. -/
theorem raw_split_can_orphan :
    ∃ (msgs : List MessageRow) (k : Nat),
      ActiveBlockValid msgs ∧
        PromptView.AnnouncementsAreAssistant msgs ∧
        PromptView.PairsClosedInMessages msgs ∧
        ¬ PromptView.PairsClosedInMessages (msgs.drop k) := by
  refine ⟨[⟨0, 0, 0, .assistant, .assistantToolCalls {1}⟩,
           ⟨1, 0, 1, .user, .toolResult 1 ⟨0, 0, 0⟩⟩], 1, ?_, ?_, ?_, ?_⟩
  · refine ⟨rfl, ?_⟩
    refine ⟨by decide, ?_⟩
    simp [ActiveBlockValidFrom]
  · intro row hmem callIds hkind
    rcases List.mem_cons.mp hmem with hEq | hmem
    · subst hEq; rfl
    · rcases List.mem_cons.mp hmem with hEq | hmem
      · subst hEq; exact absurd hkind (by simp)
      · exact absurd hmem (List.not_mem_nil _)
  · intro row hmem callId key hkind
    rcases List.mem_cons.mp hmem with hEq | hmem
    · subst hEq; exact absurd hkind (by simp)
    · rcases List.mem_cons.mp hmem with hEq | hmem
      · subst hEq
        have hinj : MessageKind.toolResult 1 (⟨0, 0, 0⟩ : ToolResultKey)
            = .toolResult callId key := hkind
        injection hinj with hcEq _
        subst hcEq
        exact ⟨⟨0, 0, 0, .assistant, .assistantToolCalls {1}⟩, List.mem_cons_self _ _, rfl,
          {1}, rfl, by decide⟩
      · exact absurd hmem (List.not_mem_nil _)
  · intro hclosed
    obtain ⟨caller, hmem, hrole, _⟩ :=
      hclosed ⟨1, 0, 1, .user, .toolResult 1 ⟨0, 0, 0⟩⟩ (by simp) 1 ⟨0, 0, 0⟩ rfl
    have : caller = ⟨1, 0, 1, .user, .toolResult 1 ⟨0, 0, 0⟩⟩ := by simpa using hmem
    subst this
    exact absurd hrole (by decide)

end Compaction
