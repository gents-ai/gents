import Proofs.Basic

/-!
# Reasoning replay frontier derived from accepted captures

A provider input is modeled as a flat sequence of wire items: ordinary items
(the selected request context, message headers, text, tool calls and results)
and reasoning items (Claude `thinking`/`redacted_thinking` blocks, Responses
`reasoning` items). A reasoning item is located by its *anchor*: the number of
ordinary items before it.

External premise (Anthropic preserved thinking, verified 2026-09-25): a thinking
block's signature binds the ordinary prefix that produced it (system, tool set
and every earlier message, excluding thinking) plus a chain to the previous
thinking block. A replay is accepted iff the prefix before each replayed block
equals its producing prefix with a leading run of reasoning removed. Responses
encrypted reasoning has no such binding; the same rule is applied to it
conservatively, so one owner decides replay for every wire.

`prefix_is_leading_reasoning_removal` shows that two checks decide that rule
exactly: equal ordinary sequences, and the current anchored reasoning being a
suffix of the producing request's anchored reasoning. Both are computed from the
accepted request capture, so every rewrite (compaction, repair, strip-and-retry,
a tool or issuer change) becomes a frontier by construction: the next accepted
turn's capture no longer carries the earlier reasoning or no longer shares the
prefix. No retirement record is written.
-/

namespace PromptAssembly.ReplayFrontier

inductive FlatItem (O W : Type) where
  | ordinary (item : O)
  | reasoning (item : W)
  deriving DecidableEq, Repr

variable {O W : Type}

def ords : List (FlatItem O W) → List O
  | [] => []
  | .ordinary item :: rest => item :: ords rest
  | .reasoning _ :: rest => ords rest

def anchoredFrom (n : Nat) : List (FlatItem O W) → List (Nat × W)
  | [] => []
  | .ordinary _ :: rest => anchoredFrom (n + 1) rest
  | .reasoning item :: rest => (n, item) :: anchoredFrom n rest

def anchored (items : List (FlatItem O W)) : List (Nat × W) := anchoredFrom 0 items

/-- Rebuild a flat input from its ordinary sequence and anchored reasoning. -/
def weave : Nat → List O → List (Nat × W) → List (FlatItem O W)
  | _, [], rs => rs.map fun r => .reasoning r.2
  | n, o :: os, [] => .ordinary o :: weave (n + 1) os []
  | n, o :: os, (k, w) :: rs =>
      if k = n then .reasoning w :: weave n (o :: os) rs
      else .ordinary o :: weave (n + 1) os ((k, w) :: rs)
termination_by _ os rs => os.length + rs.length

/-- Remove the first `count` reasoning items, keeping every ordinary item. -/
def dropLeadingReasoning : Nat → List (FlatItem O W) → List (FlatItem O W)
  | 0, items => items
  | _ + 1, [] => []
  | count + 1, .reasoning _ :: rest => dropLeadingReasoning count rest
  | count + 1, .ordinary item :: rest => .ordinary item :: dropLeadingReasoning (count + 1) rest

theorem anchoredFrom_ge (n : Nat) (items : List (FlatItem O W)) :
    ∀ entry ∈ anchoredFrom n items, n ≤ entry.1 := by
  induction items generalizing n with
  | nil => simp [anchoredFrom]
  | cons item rest ih =>
      cases item with
      | ordinary o =>
          intro entry hentry
          have := ih (n + 1) entry (by simpa [anchoredFrom] using hentry)
          omega
      | reasoning w =>
          intro entry hentry
          simp only [anchoredFrom, List.mem_cons] at hentry
          rcases hentry with rfl | hrest
          · simp
          · exact ih n entry hrest

theorem weave_nil_ords (n : Nat) (rs : List (Nat × W)) :
    (weave n ([] : List O) rs) = rs.map fun r => .reasoning r.2 := by
  cases rs <;> simp [weave]

theorem weave_roundtrip (n : Nat) (items : List (FlatItem O W)) :
    weave n (ords items) (anchoredFrom n items) = items := by
  induction items generalizing n with
  | nil => simp [ords, anchoredFrom, weave]
  | cons item rest ih =>
      cases item with
      | reasoning w =>
          cases hords : ords rest with
          | nil =>
              have hrest := ih n
              rw [hords, weave_nil_ords] at hrest
              simp [ords, anchoredFrom, hords, weave_nil_ords, hrest]
          | cons o os =>
              have hrest := ih n
              rw [hords] at hrest
              simp [ords, anchoredFrom, hords, weave, hrest]
      | ordinary o =>
          cases hanch : anchoredFrom (n + 1) rest with
          | nil =>
              have hrest := ih (n + 1)
              rw [hanch] at hrest
              simp [ords, anchoredFrom, hanch, weave, hrest]
          | cons entry rs =>
              obtain ⟨k, w⟩ := entry
              have hge := anchoredFrom_ge (n + 1) rest (k, w) (by simp [hanch])
              have hne : k ≠ n := by simp at hge; omega
              have hrest := ih (n + 1)
              rw [hanch] at hrest
              simp [ords, anchoredFrom, hanch, weave, hne, hrest]

theorem ords_dropLeadingReasoning (count : Nat) (items : List (FlatItem O W)) :
    ords (dropLeadingReasoning count items) = ords items := by
  induction items generalizing count with
  | nil => cases count <;> simp [dropLeadingReasoning]
  | cons item rest ih =>
      cases count with
      | zero => simp [dropLeadingReasoning]
      | succ count =>
          cases item <;> simp [dropLeadingReasoning, ords, ih]

theorem anchoredFrom_dropLeadingReasoning (n count : Nat) (items : List (FlatItem O W)) :
    anchoredFrom n (dropLeadingReasoning count items) = (anchoredFrom n items).drop count := by
  induction items generalizing n count with
  | nil => cases count <;> simp [dropLeadingReasoning, anchoredFrom]
  | cons item rest ih =>
      cases count with
      | zero => simp [dropLeadingReasoning]
      | succ count =>
          cases item <;> simp [dropLeadingReasoning, anchoredFrom, ih]

/-- The replay acceptance rule, decided by two capture-derived checks. -/
theorem prefix_is_leading_reasoning_removal (current captured : List (FlatItem O W))
    (hords : ords current = ords captured)
    (hsuffix : anchored current <:+ anchored captured) :
    current = dropLeadingReasoning
      ((anchored captured).length - (anchored current).length) captured := by
  obtain ⟨dropped, hsplit⟩ := hsuffix
  have hlen : (anchored captured).length - (anchored current).length = dropped.length := by
    rw [← hsplit, List.length_append]; omega
  have hkept : anchored current = (anchored captured).drop dropped.length := by
    rw [← hsplit]; simp
  rw [hlen]
  calc current = weave 0 (ords current) (anchored current) := (weave_roundtrip 0 current).symm
    _ = weave 0 (ords (dropLeadingReasoning dropped.length captured))
          (anchored (dropLeadingReasoning dropped.length captured)) := by
        rw [hords, hkept, ords_dropLeadingReasoning]
        simp [anchored, anchoredFrom_dropLeadingReasoning]
    _ = dropLeadingReasoning dropped.length captured := weave_roundtrip 0 _

/-- The two checks are also necessary, so the rule never rejects a valid replay. -/
theorem leading_reasoning_removal_passes_checks (count : Nat)
    (captured : List (FlatItem O W)) :
    ords (dropLeadingReasoning count captured) = ords captured ∧
      anchored (dropLeadingReasoning count captured) <:+ anchored captured := by
  refine ⟨ords_dropLeadingReasoning count captured, ?_⟩
  simp only [anchored, anchoredFrom_dropLeadingReasoning]
  exact List.drop_suffix count _

/-! ## Maximal admissible suffix

Candidates are ordered accepted turns. `ok before x` judges `x` given the kept
candidates before it. The kept set is always a suffix of the candidates, so
removal is a leading run. -/

section Chain

variable {α : Type}

def chainOk (ok : List α → α → Bool) : List α → List α → Bool
  | _, [] => true
  | before, x :: rest => ok before x && chainOk ok (before ++ [x]) rest

def admissible (ok : List α → α → Bool) (xs : List α) : Bool := chainOk ok [] xs

def maxAdmissibleSuffix (ok : List α → α → Bool) : List α → List α
  | [] => []
  | x :: rest =>
      if admissible ok (x :: rest) then x :: rest else maxAdmissibleSuffix ok rest

/-- Dropping earlier kept candidates never invalidates a later one. -/
def SuffixClosed (ok : List α → α → Bool) : Prop :=
  ∀ dropped before x, ok (dropped ++ before) x = true → ok before x = true

theorem maxAdmissibleSuffix_suffix (ok : List α → α → Bool) (xs : List α) :
    maxAdmissibleSuffix ok xs <:+ xs := by
  induction xs with
  | nil => simp [maxAdmissibleSuffix]
  | cons x rest ih =>
      unfold maxAdmissibleSuffix
      split
      · exact List.suffix_refl _
      · exact ih.trans (List.suffix_cons x rest)

theorem maxAdmissibleSuffix_admissible (ok : List α → α → Bool) (xs : List α) :
    admissible ok (maxAdmissibleSuffix ok xs) = true := by
  induction xs with
  | nil => simp [maxAdmissibleSuffix, admissible, chainOk]
  | cons x rest ih =>
      unfold maxAdmissibleSuffix
      split
      · assumption
      · exact ih

/-- No longer admissible suffix exists. -/
theorem maxAdmissibleSuffix_maximal (ok : List α → α → Bool) (xs s : List α)
    (hs : s <:+ xs) (hadm : admissible ok s = true) :
    s.length ≤ (maxAdmissibleSuffix ok xs).length := by
  induction xs with
  | nil => simp [List.suffix_nil.mp hs]
  | cons x rest ih =>
      unfold maxAdmissibleSuffix
      split
      · exact hs.length_le
      · rename_i hnot
        rcases List.suffix_cons_iff.mp hs with rfl | hrest
        · exact absurd hadm hnot
        · exact ih hrest

theorem chainOk_drop_prefix (ok : List α → α → Bool) (hclosed : SuffixClosed ok)
    (dropped : List α) (before xs : List α)
    (h : chainOk ok (dropped ++ before) xs = true) : chainOk ok before xs = true := by
  induction xs generalizing before with
  | nil => simp [chainOk]
  | cons x rest ih =>
      simp only [chainOk, Bool.and_eq_true] at h ⊢
      exact ⟨hclosed dropped before x h.1, ih (before ++ [x]) (by simpa using h.2)⟩

/-- Admissibility is downward closed over suffixes, so a scan from the front
(or a binary search over the drop count) finds the maximal suffix exactly. -/
theorem admissible_tail (ok : List α → α → Bool) (hclosed : SuffixClosed ok)
    (x : α) (rest : List α) (h : admissible ok (x :: rest) = true) :
    admissible ok rest = true := by
  simp only [admissible, chainOk, Bool.and_eq_true, List.nil_append] at h
  exact chainOk_drop_prefix ok hclosed [x] [] rest (by simpa using h.2)

theorem admissible_of_suffix (ok : List α → α → Bool) (hclosed : SuffixClosed ok)
    (s xs : List α) (hs : s <:+ xs) (h : admissible ok xs = true) :
    admissible ok s = true := by
  induction xs with
  | nil => rw [List.suffix_nil.mp hs]; exact h
  | cons x rest ih =>
      rcases List.suffix_cons_iff.mp hs with rfl | hrest
      · exact h
      · exact ih hrest (admissible_tail ok hclosed x rest h)

theorem chainOk_split (ok : List α → α → Bool) (seen before : List α) (x : α)
    (after : List α) (h : chainOk ok seen (before ++ x :: after) = true) :
    ok (seen ++ before) x = true := by
  induction before generalizing seen with
  | nil => simp only [chainOk, Bool.and_eq_true, List.nil_append] at h; simpa using h.1
  | cons y rest ih =>
      simp only [chainOk, Bool.and_eq_true, List.cons_append] at h
      simpa using ih (seen ++ [y]) h.2

/-- Every kept candidate was judged against exactly the kept candidates before it. -/
theorem admissible_split (ok : List α → α → Bool) (before : List α) (x : α)
    (after : List α) (h : admissible ok (before ++ x :: after) = true) :
    ok before x = true := by
  simpa using chainOk_split ok [] before x after h

end Chain

/-! ## Accepted turns -/

/-- One accepted provider turn carrying reasoning. `prefixOrds` and `items`
come from the assembled request body (this turn's ordinary prefix and its own
anchored reasoning items); `captured` is the flattened body of the accepted
request that produced the turn, `none` when that request or its capture is
verifiably absent, out of scope or undecodable. A failed store read is not
`none`: it says nothing about the turn, so the adapter forms no input and the
request fails instead of cutting the frontier there. `base` is the per-turn
provenance and payload decision (issuer, completion, witness, replayable
payload). -/
structure Turn (O W : Type) where
  base : Bool
  prefixOrds : List O
  items : List (Nat × W)
  captured : Option (List (FlatItem O W))
  deriving Repr

def keptItems (before : List (Turn O W)) : List (Nat × W) :=
  before.flatMap (·.items)

def turnOk [DecidableEq O] [DecidableEq W] (before : List (Turn O W)) (x : Turn O W) : Bool :=
  x.base &&
    match x.captured with
    | none => false
    | some captured =>
        decide (ords captured = x.prefixOrds) &&
          (keptItems before).isSuffixOf (anchored captured)

theorem turnOk_suffixClosed [DecidableEq O] [DecidableEq W] :
    SuffixClosed (turnOk (O := O) (W := W)) := by
  intro dropped before x h
  unfold turnOk at h ⊢
  simp only [Bool.and_eq_true] at h ⊢
  refine ⟨h.1, ?_⟩
  cases hcap : x.captured with
  | none => simp [hcap] at h
  | some captured =>
      simp only [hcap, Bool.and_eq_true, decide_eq_true_eq] at h ⊢
      refine ⟨h.2.1, ?_⟩
      have hsuffix := List.isSuffixOf_iff_suffix.mp h.2.2
      apply List.isSuffixOf_iff_suffix.mpr
      simp only [keptItems, List.flatMap_append] at hsuffix
      exact (List.suffix_append _ _).trans hsuffix

/-- A kept turn's assembled prefix is its accepted producing prefix with a
leading run of reasoning removed: the provider acceptance rule. -/
theorem kept_turn_prefix_is_leading_removal [DecidableEq O] [DecidableEq W]
    (before : List (Turn O W)) (x : Turn O W) (captured : List (FlatItem O W))
    (hok : turnOk before x = true) (hcap : x.captured = some captured) :
    ∃ count, weave 0 x.prefixOrds (keptItems before) =
      dropLeadingReasoning count captured := by
  unfold turnOk at hok
  simp only [hcap, Bool.and_eq_true, decide_eq_true_eq] at hok
  obtain ⟨dropped, hsplit⟩ := List.isSuffixOf_iff_suffix.mp hok.2.2
  refine ⟨dropped.length, ?_⟩
  have hkept : keptItems before = (anchored captured).drop dropped.length := by
    rw [← hsplit]; simp
  rw [← hok.2.1, hkept]
  have := weave_roundtrip 0 (dropLeadingReasoning dropped.length captured)
  simpa [ords_dropLeadingReasoning, anchored, anchoredFrom_dropLeadingReasoning] using this

/-- No resurrection: a reasoning item absent from a kept turn's accepted
capture is never replayed ahead of that turn, and if that turn is dropped every
earlier turn is dropped with it (the kept set is a suffix). -/
theorem kept_before_items_in_capture [DecidableEq O] [DecidableEq W]
    (before : List (Turn O W)) (x : Turn O W) (after : List (Turn O W))
    (captured : List (FlatItem O W))
    (hadm : admissible turnOk (before ++ x :: after) = true)
    (hcap : x.captured = some captured) :
    ∀ y ∈ before, ∀ item ∈ y.items, item ∈ anchored captured := by
  have hok := admissible_split turnOk before x after hadm
  unfold turnOk at hok
  simp only [hcap, Bool.and_eq_true] at hok
  have hsuffix := List.isSuffixOf_iff_suffix.mp hok.2.2
  intro y hy item hitem
  exact hsuffix.subset (List.mem_flatMap.mpr ⟨y, hy, hitem⟩)

theorem kept_turns_are_suffix [DecidableEq O] [DecidableEq W] (turns : List (Turn O W)) :
    ∃ dropped, turns = dropped ++ maxAdmissibleSuffix turnOk turns := by
  obtain ⟨dropped, h⟩ := maxAdmissibleSuffix_suffix turnOk turns
  exact ⟨dropped, h.symm⟩

end PromptAssembly.ReplayFrontier
