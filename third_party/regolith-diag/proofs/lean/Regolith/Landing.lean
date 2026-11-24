/-!
# Landing: whoever waits for a commit to land is told at most once

This file backs the TLA+ model `proofs/tla/Landing.tla`. TLC checks there
two publishers and two listeners, in every interleaving, and that every
listener is told. Here "at most once" is proved for every number of
publishers and listeners.

## The story, for a reader who has never seen the code

A commit lands when it becomes visible to readers, or when a rollback
discards it. Some things wait for that moment: the key locks of a
transaction that committed without waiting are released then. Each is a
listener on the commit's slot (`src/engine/commit/descriptor.rs`).

Think of a parcel counter. People who wait for a parcel put their names on
a sheet. Whoever brings the parcel in, any clerk, takes the sheet, closes it
for good with the same move, and calls the names on it. Someone who comes to
sign later finds the sheet closed, which says the parcel is there: they help
themselves. Several clerks may carry the same parcel in at once; one of them
gets the names, the others find the sheet empty.

Tiny example of what the single move prevents. Listener 1 is on the sheet.
Clerk A reads the sheet; clerk B reads it too, before A cleared it. Both
call listener 1, and its locks are released twice: the second time under a
transaction that had taken them since.

## What is proved, in plain words

1. `told_at_most_once`: no listener is told twice, whatever the publishers
   and listeners do (TLA+ `ToldAtMostOnce`).
2. `one_place`: a listener is in one place at most: told, in a publisher's
   hands, or on the sheet. So one that was told is in nobody's hands to be
   told again.
3. `closed_sheet_is_empty`: once the sheet is closed nobody is on it, so
   nobody waits for a call that will not come (TLA+ `ClosedSheetEmpty`).
4. The RED case, as a counterexample: reading the sheet and clearing it in
   two moves lets two publishers call the same listener
   (`read_then_clear_tells_twice`, TLA+ RED `ReadThenClear`).

## How to read the Lean

`def` defines a thing, `theorem` states a fact and its proof follows
`:= by`. Lines starting with `--` are comments, in plain words. A proof is a
list of *tactics*; each one changes the goal still to be shown, and the
comment above it says how. `simp` rewrites with known facts; `omega` solves
arithmetic over natural numbers; `cases` and `rcases` split on the ways a
fact could be true; `induction` proves a fact for every list, or every
number of steps, by proving it for none and then for one more.
-/

-- Everything below is named Regolith.Landing.<name>.
namespace Regolith.Landing

/-! ## Part 1. Counting -/

/-- How often `x` occurs in a list. -/
def occurs (x : Nat) : List Nat → Nat
  -- Never in an empty list.
  | [] => 0
  -- Once more when the first element is `x`, plus the rest's count.
  | y :: rest => (if y = x then 1 else 0) + occurs x rest

/-- The count over two lists joined is the sum of the two counts. -/
theorem occurs_append (x : Nat) (a b : List Nat) :
    -- The claim itself; its proof follows.
    occurs x (a ++ b) = occurs x a + occurs x b := by
  -- By the length of the first list.
  induction a with
  -- Case: an empty first list adds nothing.
  | nil => simp [occurs]
  -- Case: one more element in front, with the claim for the rest (ih) known.
  | cons y rest ih =>
    -- The front element counts the same on both sides; the rest is ih.
    simp [occurs, ih, Nat.add_assoc]

/-- A value that differs from every element of a list occurs in it no times. -/
theorem occurs_none (x : Nat) (l : List Nat) (h : ∀ y ∈ l, y ≠ x) : occurs x l = 0 := by
  -- By the length of the list.
  induction l with
  -- Case: the empty list.
  | nil => rfl
  -- Case: one more element in front, with the claim for the rest (ih) known.
  | cons y rest ih =>
    -- The front element is not `x`.
    have hy : y ≠ x := h y (by simp)
    -- Nor is any element of the rest, so the rest counts none.
    have hrest := ih (fun z hz => h z (by simp [hz]))
    -- Nothing for the front, nothing for the rest.
    simp [occurs, hy, hrest]

/-! ## Part 2. The steps -/

/-- The slot's listeners at one moment. Listeners are numbered in the order
they come, so each number comes once. -/
structure State where
  /-- The listeners on the sheet: registered, not taken yet. -/
  sheet : List Nat
  /-- Whether the sheet is closed. -/
  shut : Bool
  /-- The listeners publishers took and have not told yet, all hands
  together. -/
  hands : List Nat
  /-- The listeners told, newest first. -/
  told : List Nat
  /-- The number the next listener gets. -/
  fresh : Nat

/-- The start: an open, empty sheet, and nobody told. -/
def init : State :=
  -- Nothing anywhere; the first listener is number 0.
  ⟨[], false, [], [], 0⟩

/-- The steps of `Descriptor::listen` and `Descriptor::land`, each one
atomic step there. -/
inductive Step : State → State → Prop
  /-- A listener signs the open sheet (`listen` returns `true`). -/
  | listen (s : State) (h : s.shut = false) :
      -- It is on the sheet, and the next listener gets the next number.
      Step s { s with sheet := s.fresh :: s.sheet, fresh := s.fresh + 1 }
  /-- A listener finds the sheet closed, and its own caller tells it
  (`listen` returns `false`). -/
  | refused (s : State) (h : s.shut = true) :
      -- It is told, and the next listener gets the next number.
      Step s { s with told := s.fresh :: s.told, fresh := s.fresh + 1 }
  /-- A publisher takes the sheet and closes it, in one swap. -/
  | take (s : State) :
      -- Every name on it is in a hand, the sheet is empty and closed.
      Step s { s with hands := s.sheet ++ s.hands, sheet := [], shut := true }
  /-- A publisher tells one listener it holds. -/
  | tell (s : State) (a b : List Nat) (l : Nat) (h : s.hands = a ++ l :: b) :
      -- It leaves the hands and is told.
      Step s { s with hands := a ++ b, told := l :: s.told }

/-- The states the steps can reach from the start. -/
inductive Reach : State → Prop
  /-- The start is reachable. -/
  | init : Reach init
  /-- One step from a reachable state reaches another. -/
  | step {s t : State} : Reach s → Step s t → Reach t

/-- The facts every reachable state has. -/
structure Inv (s : State) : Prop where
  /-- A listener is in one place at most: told, in a hand, or on the sheet. -/
  once : ∀ x, occurs x s.told + occurs x s.hands + occurs x s.sheet ≤ 1
  /-- Every listener anywhere has a number already given out. -/
  known : ∀ x, x ∈ s.told ∨ x ∈ s.hands ∨ x ∈ s.sheet → x < s.fresh
  /-- A closed sheet is empty. -/
  closed : s.shut = true → s.sheet = []

/-- A number not given out yet occurs nowhere. -/
theorem fresh_nowhere {s : State} (hs : Inv s) :
    -- The claim itself; its proof follows.
    occurs s.fresh s.told = 0 ∧ occurs s.fresh s.hands = 0 ∧ occurs s.fresh s.sheet = 0 := by
  -- Every listener in each list has a smaller number, so is not this one.
  refine ⟨?_, ?_, ?_⟩
  -- Among the told.
  · exact occurs_none _ _ (fun y hy => Nat.ne_of_lt (hs.known y (Or.inl hy)))
  -- In the hands.
  · exact occurs_none _ _ (fun y hy => Nat.ne_of_lt (hs.known y (Or.inr (Or.inl hy))))
  -- On the sheet.
  · exact occurs_none _ _ (fun y hy => Nat.ne_of_lt (hs.known y (Or.inr (Or.inr hy))))

/-- One step keeps every fact. -/
theorem inv_step {s t : State} (hs : Inv s) (hst : Step s t) : Inv t := by
  -- One case per step.
  cases hst with
  -- Case: a listener signs the open sheet.
  | listen hopen =>
    -- Its number occurs nowhere yet.
    obtain ⟨h1, h2, h3⟩ := fresh_nowhere hs
    -- The facts, one at a time.
    refine ⟨?_, ?_, ?_⟩
    -- One place at most: split on whether `x` is the new listener.
    · intro x
      -- Is it?
      by_cases hx : s.fresh = x
      -- It is: it was nowhere, and is on the sheet once.
      · subst hx; simp [occurs, h1, h2, h3]
      -- It is not: the new name adds nothing to its count.
      · simpa [occurs, hx] using hs.once x
    -- Known numbers: the new one is the last given out; the others as before.
    · intro x hx
      -- Where is `x`?
      rcases hx with hx | hx | hx
      -- Told: known before, and the bound only grew.
      · have := hs.known x (Or.inl hx); simp only; omega
      -- In a hand: the same.
      · have := hs.known x (Or.inr (Or.inl hx)); simp only; omega
      -- On the sheet: the new listener, or one that was there.
      · simp only [List.mem_cons] at hx
        -- Split on which.
        rcases hx with hx | hx
        -- The new one: its number is the one just given out.
        · simp only; omega
        -- An old one: known before.
        · have := hs.known x (Or.inr (Or.inr hx)); simp only; omega
    -- The sheet is open, so the last fact asks nothing.
    · intro hh; simp [hopen] at hh
  -- Case: a listener finds the sheet closed and is told by its caller.
  | refused hshut =>
    -- Its number occurs nowhere yet.
    obtain ⟨h1, h2, h3⟩ := fresh_nowhere hs
    -- The facts, one at a time.
    refine ⟨?_, ?_, ?_⟩
    -- One place at most: split on whether `x` is the new listener.
    · intro x
      -- Is it?
      by_cases hx : s.fresh = x
      -- It is: it was nowhere, and is told once.
      · subst hx; simp [occurs, h1, h2, h3]
      -- It is not: the new name adds nothing to its count.
      · simpa [occurs, hx] using hs.once x
    -- Known numbers: the new one is the last given out; the others as before.
    · intro x hx
      -- Where is `x`?
      rcases hx with hx | hx | hx
      -- Told: the new listener, or one told before.
      · simp only [List.mem_cons] at hx
        -- Split on which.
        rcases hx with hx | hx
        -- The new one: its number is the one just given out.
        · simp only; omega
        -- An old one: known before.
        · have := hs.known x (Or.inl hx); simp only; omega
      -- In a hand: known before, and the bound only grew.
      · have := hs.known x (Or.inr (Or.inl hx)); simp only; omega
      -- On the sheet: the same.
      · have := hs.known x (Or.inr (Or.inr hx)); simp only; omega
    -- The sheet and its flag are unchanged.
    · exact hs.closed
  -- Case: a publisher takes the sheet and closes it.
  | take =>
    -- The facts, one at a time.
    refine ⟨?_, ?_, ?_⟩
    -- One place at most: the names moved from the sheet to the hands.
    · intro x
      -- The count before the move.
      have := hs.once x
      -- The count over the joined list is the sum, and an empty sheet
      -- counts nothing.
      simp only [occurs_append, occurs]
      -- The same three numbers, added in another order.
      omega
    -- Known numbers: everybody was somewhere before.
    · intro x hx
      -- Where is `x`?
      rcases hx with hx | hx | hx
      -- Told: as before.
      · exact hs.known x (Or.inl hx)
      -- In a hand: taken from the sheet now, or in a hand before.
      · rcases List.mem_append.mp hx with hx | hx
        -- From the sheet.
        · exact hs.known x (Or.inr (Or.inr hx))
        -- In a hand before.
        · exact hs.known x (Or.inr (Or.inl hx))
      -- On the sheet: it is empty.
      · simp at hx
    -- The sheet is empty.
    · intro _; rfl
  -- Case: a publisher tells listener l, which it holds.
  | tell a b l hh =>
    -- The facts, one at a time.
    refine ⟨?_, ?_, ?_⟩
    -- One place at most: the listener moved from a hand to the told.
    · intro x
      -- The count before the move ...
      have := hs.once x
      -- ... with the hands spelled out.
      rw [hh] at this
      -- Sums over the joined lists, on both sides.
      simp only [occurs_append, occurs] at this ⊢
      -- Whether `l` is `x` adds the same number on both sides: name it.
      generalize (if l = x then 1 else 0) = k at this ⊢
      -- The same numbers, added in another order.
      omega
    -- Known numbers: everybody was somewhere before.
    · intro x hx
      -- Where is `x`?
      rcases hx with hx | hx | hx
      -- Told: the listener told now, or one told before.
      · simp only [List.mem_cons] at hx
        -- Split on which.
        rcases hx with hx | hx
        -- The one told now: it was in the hands.
        · exact hs.known x (Or.inr (Or.inl (by rw [hh, hx]; simp)))
        -- One told before.
        · exact hs.known x (Or.inl hx)
      -- In a hand: it was in the hands before, on one side of `l`.
      · refine hs.known x (Or.inr (Or.inl ?_))
        -- The hands before, spelled out.
        rw [hh]
        -- Which side?
        rcases List.mem_append.mp hx with hx | hx
        -- Before `l`.
        · exact List.mem_append.mpr (Or.inl hx)
        -- After `l`.
        · exact List.mem_append.mpr (Or.inr (List.mem_cons_of_mem _ hx))
      -- On the sheet: as before.
      · exact hs.known x (Or.inr (Or.inr hx))
    -- The sheet and its flag are unchanged.
    · exact hs.closed

/-- Every reachable state has every fact. -/
theorem inv_reach {s : State} (h : Reach s) : Inv s := by
  -- By how the state was reached.
  induction h with
  -- Case: the start.
  | init =>
    -- Three empty lists and an open sheet.
    refine ⟨?_, ?_, ?_⟩
    -- Nothing occurs anywhere.
    · intro x; simp [init, occurs]
    -- Nobody is anywhere.
    · intro x hx; simp [init] at hx
    -- The sheet is open.
    · intro hh; simp [init] at hh
  -- Case: a reachable state, with every fact (ih), then one step, which keeps them.
  | step _ hst ih => exact inv_step ih hst

/-! ## Part 3. The rules -/

/-- **`one_place`.** A listener is in one place at most: told, in a
publisher's hands, or on the sheet. -/
theorem one_place {s : State} (h : Reach s) :
    -- The claim itself; its proof follows.
    ∀ x, occurs x s.told + occurs x s.hands + occurs x s.sheet ≤ 1 :=
  -- One of the facts every reachable state has.
  (inv_reach h).once

/-- **`told_at_most_once`.** No listener is told twice. -/
theorem told_at_most_once {s : State} (h : Reach s) : ∀ x, occurs x s.told ≤ 1 := by
  -- Take a listener.
  intro x
  -- It is in one place at most, and the told are one of the places.
  have := (inv_reach h).once x
  -- So its count among the told is at most one.
  omega

/-- **`closed_sheet_is_empty`.** Once the sheet is closed nobody is on it:
whoever signed was taken by the move that closed it, and nobody signs
afterwards. -/
theorem closed_sheet_is_empty {s : State} (h : Reach s) : s.shut = true → s.sheet = [] :=
  -- One of the facts every reachable state has.
  (inv_reach h).closed

/-! ## Part 4. The RED case -/

/-- **`read_then_clear_tells_twice`.** The sheet read in one move and
cleared in another (bug `ReadThenClear`). Listener 1 is on the sheet; two
publishers read it before either clears it, so both hold listener 1, and
both tell it: it is told twice. -/
theorem read_then_clear_tells_twice :
    -- What two publishers hold after both read the sheet [1].
    let bothRead : List Nat := [1] ++ [1]
    -- What the told list is once each has told what it holds.
    let toldAfter : List Nat := bothRead
    -- Listener 1 was told twice.
    occurs 1 toldAfter = 2 := by
  -- Count the two entries.
  simp [occurs]

-- The end of this file's names.
end Regolith.Landing
