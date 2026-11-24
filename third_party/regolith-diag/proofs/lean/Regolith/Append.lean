/-!
# Append: commit-ordered positions are dense, once per key, in commit order (plan 3.6)

This file backs two TLA+ models in `proofs/tla`:

* `CommitOrderedAppend.tla`, invariants `INV_Dense`, `INV_UniqueCursors`,
  `INV_AtMostOnce`, `INV_CommitOrder` and `INV_NoSkip` (configurations
  `MC_CommitOrderedAppend_Green_*`), and the REDs
  `MC_CommitOrderedAppend_Red_ViewOnlyHead`,
  `MC_CommitOrderedAppend_Red_AssignBeforeValidation` and
  `MC_CommitOrderedAppend_Red_Stamps`;
* `CommitOrderedAppendCrash.tla`, invariants `INV_Dense`,
  `INV_UniqueCursors` and `INV_AtMostOnce` after a crash, since recovery
  keeps a prefix of the commit order.

## The story, for a reader who has never seen the code

A log is a numbered list that only grows, and many transactions add to it
at once. `Transaction::append(log, entry, once_key)`
(src/transaction/append.rs) does not choose the entry's number: it only
queues the entry. The number is given out at the very end of the commit, in
the one step that handles one transaction at a time ("the ordered step",
`AppendOrder::number` in src/engine/commit/append.rs), after the
transaction's reads were checked. Each append gets the log's next position,
head + 1, unless its once key already holds a position.

Tiny example: writers A and B each append one entry while the head says 0.
In the ordered step A is numbered first and gets 1, and B, counting from
the head A left, gets 2. If A's commit is refused instead, A takes no
number and B gets 1.

What could go wrong: two entries with one number, a number nobody uses (a
hole), numbers that do not follow commit order, or one message appended
twice. TLC checks the interleavings for a few writers; the theorems here
show none of these can happen for any number of commits, appends and once
keys.

What is proved, in plain words:

1. `reachable_inv`: the ordered step keeps four facts about the log,
   whatever commits it decides and whichever of them fail validation.
2. `dense_from_one`: the positions, in the order they were assigned, are
   exactly 1, 2, ..., head.
3. `unique_positions`: no two rows share a position.
4. `at_most_once`: no two rows carry one once key, whether the two appends
   came from two transactions or from one.
5. `commit_order`: a smaller position belongs to an earlier commit, or to
   an earlier append of the same commit.
6. `snapshot_sees_prefix`: a snapshot sees the commits up to some point in
   commit order (visibility follows sequence order). With H the head it
   reads, it sees exactly the rows of the whole log at positions 1..H, and
   every one of them. A crash that keeps a prefix of the commit order
   leaves the same kind of state.
7. `view_only_head_duplicates`, `assign_before_validation_leaves_hole` and
   `stamps_leave_hole`: three REDs as concrete counterexamples.

Commits decided before the ones ahead of them are published need no
separate statement. Slots are decided one after another, each reading the
head and the once keys the earlier ones left, which is exactly folding the
ordered step over them; the first counterexample shows what goes wrong when
one does not.

How to read the Lean: `def` defines a function or a property, `theorem`
states a fact and its proof follows `:= by`. Lines starting with `--` are
comments. A once key and a position are plain natural numbers.
-/

-- Everything below is named Regolith.Append.<name>.
namespace Regolith.Append

/-! ## The objects -/

/-- One row of the log: an append that the ordered step gave a position. -/
structure Row where
  /-- The position assigned, starting at 1. -/
  pos : Nat
  /-- The commit it belongs to: commits are numbered 0, 1, 2, ... in the
  order the ordered step decided them, which is commit order. -/
  commit : Nat
  /-- Which of its commit's appends it is: 0, 1, 2, ... in the order the
  transaction called `append`. -/
  idx : Nat
  /-- The once key the append passed, or `none`. -/
  once : Option Nat
  -- Lean may decide whether two rows are equal by comparing their fields; the counterexamples at
  -- the end compute with that.
  deriving DecidableEq

/-- The log as the ordered step leaves it. -/
structure Log where
  /-- The head key: the newest position, 0 while the log is empty. -/
  head : Nat
  /-- The number the next committed transaction takes. -/
  seq : Nat
  /-- The rows, in the order they were assigned. -/
  rows : List Row
  -- The same for two logs.
  deriving DecidableEq

/-- The empty log, before any commit. -/
def init : Log := ⟨0, 0, []⟩

/-- `fresh rows o`: an append passing once key `o` gets a row. It does when
it passes no once key, or when no row carries that key yet. The rows
include those of commits decided earlier and not yet published, and earlier
appends of the same transaction, because they were assigned first. -/
def fresh (rows : List Row) : Option Nat → Bool
  -- No once key: always a new row.
  | none => true
  -- Once key `k`: a new row only if no row carries `k`.
  | some k => !(rows.any (fun r => r.once == some k))

/-- The `i`-th append of commit `c`, passing once key `o`. If it is fresh,
it takes position head + 1, and the head moves there (the commit writes
`head_key = p`); otherwise the log is unchanged. -/
def assignOne (c i : Nat) (s : Log) (o : Option Nat) : Log :=
  -- If the append gets a row:
  if fresh s.rows o then
    -- the head moves up by one, and the row "position head + 1, commit c, append i, once key o"
    -- joins the end of the rows.
    { s with head := s.head + 1, rows := s.rows ++ [⟨s.head + 1, c, i, o⟩] }
  -- Otherwise nothing changes.
  else s

/-- The appends of commit `c` from the `i`-th on, in append order. -/
def assignFrom (c : Nat) : Nat → List (Option Nat) → Log → Log
  -- No append left.
  | _, [], s => s
  -- Number the next append, then the rest.
  | i, o :: os, s => assignFrom c (i + 1) os (assignOne c i s o)

/-- One decision of the ordered step. `d.1` says whether the transaction
passed validation, and `d.2` lists the once keys of its appends. Only a
transaction that passed takes a commit number and positions; one that
failed takes nothing, so it leaves no hole. -/
def orderedStep (s : Log) (d : Bool × List (Option Nat)) : Log :=
  -- A transaction that passed takes commit number `seq`, the next number becomes seq + 1, and its
  -- appends are numbered from index 0; one that failed changes nothing.
  if d.1 then assignFrom s.seq 0 d.2 { s with seq := s.seq + 1 } else s

/-- The log after the ordered step decides the transactions `ds`, in
order, from the empty log. -/
def process (ds : List (Bool × List (Option Nat))) : Log := ds.foldl orderedStep init

/-! ## The invariant -/

/-- `Before a b`: row `a` comes before row `b` in commit order: an earlier
commit, or the same commit and an earlier append. -/
def Before (a b : Row) : Prop := a.commit < b.commit ∨ (a.commit = b.commit ∧ a.idx < b.idx)

/-- `Ordered a b`, for a row `a` assigned before row `b`: `a` has the
smaller position and comes first in commit order. -/
def Ordered (a b : Row) : Prop := a.pos < b.pos ∧ Before a b

/-- `OnceApart a b`: rows `a` and `b` do not share a once key. -/
def OnceApart (a b : Row) : Prop := a.once = none ∨ a.once ≠ b.once

/-- The ordered step's invariant. `List.Pairwise R l` means `R x y` for
every `x` that comes before `y` in `l`. -/
structure Inv (s : Log) : Prop where
  /-- The positions in assignment order are 1, 2, ..., head. -/
  dense : s.rows.map Row.pos = List.range' 1 s.head
  /-- A row assigned earlier has a smaller position and an earlier place
  in commit order. -/
  ordered : s.rows.Pairwise Ordered
  /-- No two rows share a once key. -/
  once_apart : s.rows.Pairwise OnceApart
  /-- Every row belongs to a commit already numbered. -/
  below_seq : ∀ r ∈ s.rows, r.commit < s.seq

/-- The invariant while commit `c` is having its appends numbered, with
`i` the next append index: every row so far belongs to an earlier commit,
or to an earlier append of `c`. -/
structure Mid (c i : Nat) (s : Log) : Prop where
  /-- As `Inv.dense`. -/
  dense : s.rows.map Row.pos = List.range' 1 s.head
  /-- As `Inv.ordered`. -/
  ordered : s.rows.Pairwise Ordered
  /-- As `Inv.once_apart`. -/
  once_apart : s.rows.Pairwise OnceApart
  /-- Every row so far comes before `c`'s `i`-th append. -/
  before : ∀ r ∈ s.rows, r.commit < c ∨ (r.commit = c ∧ r.idx < i)

/-- In a dense log, every row's position is between 1 and the head. -/
theorem pos_le_head {s : Log}
    -- The positions are 1, ..., head.
    (hdense : s.rows.map Row.pos = List.range' 1 s.head)
    -- (for any row r)
    {r : Row}
    -- `r` is a row of the log.
    (hr : r ∈ s.rows) :
    -- The statement: r's position is at least 1 and at most the head.
    1 ≤ r.pos ∧ r.pos ≤ s.head := by
  -- `r.pos` is in the list of positions, which is `1, ..., head`.
  -- r's position is one of the positions of the log.
  have hmem : r.pos ∈ s.rows.map Row.pos := List.mem_map_of_mem hr
  -- Those positions are 1, ..., head: so r's position lies in that range.
  rw [hdense, List.mem_range'_1] at hmem
  -- Arithmetic turns "in the range" into the two bounds.
  omega

/-- **Numbering one append keeps the invariant.** It moves on to the next
append index. -/
theorem assignOne_mid {c i : Nat} {s : Log} {o : Option Nat}
    -- The invariant holds before the `i`-th append of `c`.
    (h : Mid c i s) :
    -- The statement: the invariant holds before the (i + 1)-th append, on the log after numbering
    -- the i-th.
    Mid c (i + 1) (assignOne c i s o) := by
  -- Unfold what numbering one append does.
  unfold assignOne
  -- Split on whether the append gets a row.
  split
  -- The append is fresh: a new row at head + 1. Four facts to show.
  ·
    -- Name the fact "it is fresh" hfresh.
    rename_i hfresh
    -- One goal per fact of the invariant.
    refine ⟨?_, ?_, ?_, ?_⟩
    -- Dense: the positions were 1, ..., head; now head + 1 follows.
    ·
      -- The positions of the old rows followed by the new one, written as a range with one more
      -- number at its end.
      simp only [List.map_append, List.map_cons, List.map_nil, h.dense, List.range'_concat]
      -- Both sides are ranges from 1; it remains to compare their lengths.
      congr 2
      -- Arithmetic: the new length is the new head.
      omega
    -- Ordered: every earlier row has a smaller position (at most head) and comes before the new row
    -- in commit order (`Mid.before`).
    ·
      -- "Pairwise over old rows followed by the new one" splits into three parts.
      rw [List.pairwise_append]
      -- The old rows were ordered; a single row is ordered on its own; what is left is each old row
      -- against the new one.
      refine ⟨h.ordered, List.pairwise_singleton _ _, ?_⟩
      -- Take an old row a, and b in the one-row list.
      intro a ha b hb
      -- Then b is the new row.
      rw [List.mem_singleton] at hb
      -- Replace b by it.
      subst hb
      -- a's position is between 1 and the head.
      have hpos := pos_le_head h.dense ha
      -- So a's position is below head + 1, by arithmetic. One part left.
      refine ⟨by simp only; omega, ?_⟩
      -- And a comes before the new row in commit order: that is what `Mid.before` says.
      exact h.before a ha
    -- Once keys: no earlier row carries the new row's once key, since it was fresh.
    ·
      -- Split the pairwise fact into three parts, as above.
      rw [List.pairwise_append]
      -- The old rows share no once key; a single row needs nothing; what is left is each old row
      -- against the new one.
      refine ⟨h.once_apart, List.pairwise_singleton _ _, ?_⟩
      -- Take an old row a, and b in the one-row list.
      intro a ha b hb
      -- Then b is the new row.
      rw [List.mem_singleton] at hb
      -- Replace b by it.
      subst hb
      -- Split on whether the new row has a once key.
      cases o with
      -- Case: it has none.
      | none =>
        -- The new row has no once key, so it shares none with `a`.
        -- The goal, spelled out: a has no once key, or a's once key is not "none".
        show a.once = none ∨ a.once ≠ none
        -- Split on a's once key.
        cases hao : a.once with
        -- a has none: the first alternative.
        | none => exact Or.inl rfl
        -- a has one: it is not "none", the second alternative.
        | some j => exact Or.inr (by simp)
      -- Case: its once key is k.
      | some k =>
        -- `fresh` says no row carries `k`, `a` included.
        -- Show the second alternative: a's once key differs from k.
        right
        -- Unfold "fresh": no row carries k.
        simp only [fresh, Bool.not_eq_eq_eq_not, Bool.not_true, List.any_eq_false,
          beq_iff_eq] at hfresh
        -- In particular a does not.
        exact hfresh a ha
    -- The bound: the new row is `c`'s `i`-th append, and the earlier rows came before its `i`-th,
    -- so before its `(i + 1)`-th too.
    ·
      -- Take any row r of the new log.
      intro r hr
      -- It is an old row or the new one.
      rcases List.mem_append.mp hr with hr | hr
      -- An old row: it belongs to an earlier commit, or to c with an index below i.
      · rcases h.before r hr with h1 | ⟨h1, h2⟩
        -- An earlier commit: still so.
        · exact Or.inl h1
        -- Commit c with an index below i: then below i + 1 too.
        · exact Or.inr ⟨h1, by omega⟩
      -- The new row:
      · rw [List.mem_singleton] at hr
        -- replace r by it;
        subst hr
        -- it belongs to c and has index i, which is below i + 1.
        exact Or.inr ⟨rfl, by simp only; omega⟩
  -- The once key is taken: nothing changes, and the bound on append indices only loosens. Three
  -- facts carry over; the bound is left to show.
  ·
    -- Dense, ordered and once keys are as before; one goal left.
    refine ⟨h.dense, h.ordered, h.once_apart, ?_⟩
    -- Take any row r.
    intro r hr
    -- It belongs to an earlier commit, or to c with an index below i.
    rcases h.before r hr with h1 | ⟨h1, h2⟩
    -- An earlier commit: still so.
    · exact Or.inl h1
    -- Commit c with an index below i: then below i + 1 too.
    · exact Or.inr ⟨h1, by omega⟩

/-- Numbering the rest of a commit's appends keeps the invariant. -/
theorem assignFrom_mid {c : Nat} :
    -- The statement: for every list of once keys, index i and log s, if the invariant holds before
    -- the i-th append, it holds after all of them, at index i plus their number.
    ∀ (os : List (Option Nat)) {i : Nat} {s : Log},
      -- The invariant holds before the `i`-th append.
      Mid c i s →
      Mid c (i + os.length) (assignFrom c i os s)
  -- Case: no append left.
  | [], i, s, h => by
    -- No append: nothing changes.
    simpa [assignFrom] using h
  -- Case: an append with once key o, then the rest os.
  | o :: os, i, s, h => by
    -- One append, then the rest by induction.
    -- Number this append (the invariant moves to i + 1), then the rest by the claim for the shorter
    -- list.
    have := assignFrom_mid os (assignOne_mid (o := o) h)
    -- Unfold one step of the numbering, and the length of the list.
    simp only [assignFrom, List.length_cons]
    -- i + (length + 1) is the same number as (i + 1) + length.
    rw [show i + (os.length + 1) = i + 1 + os.length by omega]
    -- Which is the fact we have.
    exact this

/-- Numbering appends never changes the next commit number. -/
theorem assignFrom_seq {c : Nat} :
    -- The statement: after numbering any appends, the next commit number is what it was.
    ∀ (os : List (Option Nat)) (i : Nat) (s : Log), (assignFrom c i os s).seq = s.seq
  -- No append: the log is unchanged.
  | [], _, _ => rfl
  -- An append, then the rest.
  | o :: os, i, s => by
    -- `assignOne` leaves `seq` alone, either way.
    -- Unfold one step of the numbering.
    simp only [assignFrom]
    -- The rest leaves the number alone (the claim for the shorter list).
    rw [assignFrom_seq os (i + 1) (assignOne c i s o)]
    -- Unfold what numbering one append does.
    unfold assignOne
    -- Whether it got a row or not, the next commit number was not touched.
    split <;> rfl

/-- **Every decision of the ordered step keeps the invariant**, whether the
transaction passed validation or not. -/
theorem orderedStep_inv {s : Log} (d : Bool × List (Option Nat))
    -- The invariant holds before the decision.
    (h : Inv s) :
    -- The statement: the invariant holds after the decision.
    Inv (orderedStep s d) := by
  -- Unfold the decision.
  unfold orderedStep
  -- Split on whether the transaction passed validation.
  split
  -- Passed: it takes commit number `seq` and its appends are numbered. Before that, every row
  -- belongs to an earlier commit.
  ·
    -- The in-between invariant holds at index 0:
    have hmid : Mid s.seq 0 { s with seq := s.seq + 1 } :=
      -- dense, ordered and once keys as before, and every row belongs to a commit below `seq`.
      ⟨h.dense, h.ordered, h.once_apart, fun r hr => Or.inl (h.below_seq r hr)⟩
    -- So it holds after all the appends are numbered.
    have hend := assignFrom_mid d.2 hmid
    -- And numbering did not change the next commit number, seq + 1.
    have hseq := assignFrom_seq (c := s.seq) d.2 0 { s with seq := s.seq + 1 }
    -- Dense, ordered and once keys come from the in-between invariant; one goal left.
    refine ⟨hend.dense, hend.ordered, hend.once_apart, ?_⟩
    -- Afterwards every row belongs to commit `seq` or an earlier one, and
    -- the next number is `seq + 1`.
    -- Take any row r of the new log.
    intro r hr
    -- The next commit number is seq + 1.
    rw [hseq]
    -- r belongs to an earlier commit, or to this one.
    rcases hend.before r hr with h1 | ⟨h1, -⟩
    -- An earlier commit: its number is below seq,
    · simp only
      -- so below seq + 1.
      omega
    -- This commit: its number is seq,
    · simp only
      -- which is below seq + 1.
      omega
  -- Failed validation: nothing changes.
  ·
    -- The invariant held before, and the log is the same.
    exact h

/-- The empty log satisfies the invariant. -/
theorem init_inv : Inv init :=
  -- No rows, head 0: the positions are the empty list `range' 1 0`.
  ⟨rfl, List.Pairwise.nil, List.Pairwise.nil, fun _ h => absurd h List.not_mem_nil⟩

/-- Any number of decisions keeps the invariant. -/
theorem foldl_inv :
    -- The statement: for every list of decisions and every log satisfying the invariant, the log
    -- after all of them satisfies it.
    ∀ (ds : List (Bool × List (Option Nat))) {s : Log}, Inv s → Inv (ds.foldl orderedStep s)
  -- No decision: the same log.
  | [], _, h => h
  -- A decision, then the rest: the first keeps the invariant, and the rest follows by the claim for
  -- the shorter list.
  | d :: ds, _, h => foldl_inv ds (orderedStep_inv d h)

/-- **Every log the ordered step reaches satisfies the invariant.** -/
theorem reachable_inv (ds : List (Bool × List (Option Nat))) : Inv (process ds) :=
  -- Start from the empty log, which satisfies the invariant, and apply the decisions.
  foldl_inv ds init_inv

/-! ## The laws -/

/-- Two members of a list with a pairwise relation are equal, or related
one way or the other. -/
theorem pairwise_mem {α : Type} {R : α → α → Prop} :
    -- The statement: in a list where every earlier element is R-related to every later one, any two
    -- members are equal or related one way or the other.
    ∀ {l : List α}, l.Pairwise R → ∀ {a b : α}, a ∈ l → b ∈ l → a = b ∨ R a b ∨ R b a
  -- The empty list has no member.
  | [], _, _, _, ha, _ => absurd ha List.not_mem_nil
  -- A list with first element x and rest xs.
  | x :: xs, hp, a, b, ha, hb => by
    -- Pairwise on x :: xs means: x is related to every element of xs, and xs is pairwise.
    rw [List.pairwise_cons] at hp
    -- Each of `a` and `b` is the head `x` or in the tail.
    -- a is x, or a is in the rest.
    rcases List.mem_cons.mp ha with rfl | ha'
    -- a is x. Then b is x, or b is in the rest.
    · rcases List.mem_cons.mp hb with rfl | hb'
      -- Both are x: equal.
      · exact Or.inl rfl
      -- a is x and b comes later: a is related to b.
      · exact Or.inr (Or.inl (hp.1 b hb'))
    -- a is in the rest. Then b is x, or b is in the rest.
    · rcases List.mem_cons.mp hb with rfl | hb'
      -- b is x and a comes later: b is related to a.
      · exact Or.inr (Or.inr (hp.1 a ha'))
      -- Both are in the rest: the claim for the shorter list.
      · exact pairwise_mem hp.2 ha' hb'

/-- **Dense from 1.** The positions of any reachable log, in the order they
were assigned, are exactly 1, 2, ..., head. TLA+: `INV_Dense`. -/
theorem dense_from_one (ds : List (Bool × List (Option Nat))) :
    -- The statement: the positions in assignment order are 1, ..., head.
    (process ds).rows.map Row.pos = List.range' 1 (process ds).head :=
  -- That is the first fact of the invariant, which every reachable log has.
  (reachable_inv ds).dense

/-- **Unique positions.** Two rows of a reachable log at one position are
the same row. TLA+: `INV_UniqueCursors`. -/
theorem unique_positions (ds : List (Bool × List (Option Nat))) {a b : Row}
    -- Both are rows of the log ...
    (ha : a ∈ (process ds).rows) (hb : b ∈ (process ds).rows)
    -- ... at one position.
    (hpos : a.pos = b.pos) :
    -- The statement: they are the same row.
    a = b := by
  -- Otherwise one was assigned first and has the smaller position.
  -- The two rows are equal, or a was assigned first, or b was.
  rcases pairwise_mem (reachable_inv ds).ordered ha hb with h | h | h
  -- Equal: done.
  · exact h
  -- a first: then a's position is smaller, against "the same position".
  · exact absurd h.1 (by omega)
  -- b first: then b's position is smaller, against "the same position".
  · exact absurd h.1 (by omega)

/-- **At most once per once key.** Two rows of a reachable log carrying
once key `k` are the same row: whether the appends came from two
transactions, two members of one group, or one transaction twice, only the
first got a row. TLA+: `INV_AtMostOnce`. -/
theorem at_most_once (ds : List (Bool × List (Option Nat))) {a b : Row} {k : Nat}
    -- Both are rows of the log ...
    (ha : a ∈ (process ds).rows) (hb : b ∈ (process ds).rows)
    -- ... carrying once key `k`.
    (hak : a.once = some k) (hbk : b.once = some k) :
    -- The statement: they are the same row.
    a = b := by
  -- Otherwise the one assigned first would be `OnceApart` from the other.
  -- The two rows are equal, or a was assigned first, or b was.
  rcases pairwise_mem (reachable_inv ds).once_apart ha hb with h | h | h
  -- Equal: done.
  · exact h
  -- a first: then a has no once key, or a's differs from b's.
  · rcases h with h | h
    -- a has none: but it carries k,
    · rw [hak] at h
      -- a contradiction.
      cases h
    -- They differ: but both carry k.
    · exact absurd (hak.trans hbk.symm) h
  -- b first: then b has no once key, or b's differs from a's.
  · rcases h with h | h
    -- b has none: but it carries k,
    · rw [hbk] at h
      -- a contradiction.
      cases h
    -- They differ: but both carry k.
    · exact absurd (hbk.trans hak.symm) h

/-- **Commit order.** In a reachable log, a row at a smaller position
belongs to an earlier commit, or to an earlier append of the same commit.
TLA+: `INV_CommitOrder`. -/
theorem commit_order (ds : List (Bool × List (Option Nat))) {a b : Row}
    -- Both are rows of the log ...
    (ha : a ∈ (process ds).rows) (hb : b ∈ (process ds).rows)
    -- ... and `a` has the smaller position.
    (hlt : a.pos < b.pos) :
    -- The statement: a comes before b in commit order.
    Before a b := by
  -- The one assigned first has the smaller position, so it is `a`.
  -- The two rows are equal, or a was assigned first, or b was.
  rcases pairwise_mem (reachable_inv ds).ordered ha hb with h | h | h
  -- Equal: replace b by a.
  · subst h
    -- Then a's position would be below itself: impossible.
    exact absurd hlt (Nat.lt_irrefl _)
  -- a first: then a comes before b in commit order.
  · exact h.2
  -- b first: then b's position is smaller, against "a has the smaller position".
  · exact absurd h.1 (by omega)

/-- The ordered step only appends rows above the head it started from,
and never lowers the head. -/
theorem assignFrom_extends {c : Nat} :
    -- The statement: for every list of once keys, index and log,
    ∀ (os : List (Option Nat)) (i : Nat) (s : Log),
      -- the rows afterwards are the old rows followed by some extra rows, each above the old head,
      -- and the head did not go down.
      ∃ extra, (assignFrom c i os s).rows = s.rows ++ extra ∧
        (∀ r ∈ extra, s.head < r.pos) ∧ s.head ≤ (assignFrom c i os s).head
  -- No append: no extra row, and the head is unchanged.
  | [], _, s => ⟨[], by simp [assignFrom], by simp, by simp [assignFrom]⟩
  -- An append, then the rest.
  | o :: os, i, s => by
    -- First the `i`-th append, which adds at most one row at head + 1.
    have h1 : ∃ e1, (assignOne c i s o).rows = s.rows ++ e1 ∧
        (∀ r ∈ e1, s.head < r.pos) ∧ s.head ≤ (assignOne c i s o).head := by
      -- Unfold what numbering one append does.
      unfold assignOne
      -- Split on whether it gets a row.
      split
      -- It does: the extra row is the one at head + 1, and the head did not go down. One part left.
      · refine ⟨[⟨s.head + 1, c, i, o⟩], rfl, ?_, by simp only; omega⟩
        -- Take a row of that one-row list.
        intro r hr
        -- It is the new row.
        rw [List.mem_singleton] at hr
        -- Replace it.
        subst hr
        -- Its position is head + 1,
        simp only
        -- which is above the head.
        omega
      -- It does not: no extra row, the same head.
      · exact ⟨[], by simp, by simp, Nat.le_refl _⟩
    -- Name the pieces of that fact.
    obtain ⟨e1, hr1, hp1, hh1⟩ := h1
    -- Then the rest, above the new head, which is at least the old one.
    obtain ⟨e2, hr2, hp2, hh2⟩ := assignFrom_extends os (i + 1) (assignOne c i s o)
    -- The extra rows are those of the first append followed by those of the rest. Three parts.
    refine ⟨e1 ++ e2, ?_, ?_, ?_⟩
    -- The rows: unfold one step of the numbering.
    · simp only [assignFrom]
      -- Old rows, then the first append's, then the rest's; joining lists does not depend on
      -- grouping.
      rw [hr2, hr1, List.append_assoc]
    -- Above the head: take any extra row.
    · intro r hr
      -- It comes from the first append or from the rest.
      rcases List.mem_append.mp hr with hr | hr
      -- From the first: above the old head.
      · exact hp1 r hr
      -- From the rest: above the head after the first append,
      · have := hp2 r hr
        -- which is at least the old head.
        omega
    -- The head: unfold one step of the numbering.
    · simp only [assignFrom]
      -- It did not go down in either part, so not overall.
      omega

/-- Any number of decisions only appends rows above the head they started
from. -/
theorem foldl_extends :
    -- The statement: for every list of decisions and every log,
    ∀ (ds : List (Bool × List (Option Nat))) (s : Log),
      -- the rows afterwards are the old rows followed by some extra rows, each above the old head.
      ∃ extra, (ds.foldl orderedStep s).rows = s.rows ++ extra ∧
        ∀ r ∈ extra, s.head < r.pos
  -- No decision: no extra row.
  | [], s => ⟨[], by simp, by simp⟩
  -- A decision, then the rest.
  | d :: ds, s => by
    -- The first decision adds rows above `s.head` and does not lower it ...
    have h1 : ∃ e1, (orderedStep s d).rows = s.rows ++ e1 ∧
        (∀ r ∈ e1, s.head < r.pos) ∧ s.head ≤ (orderedStep s d).head := by
      -- Unfold the decision.
      unfold orderedStep
      -- Split on whether the transaction passed.
      split
      -- Passed: its appends only add rows above the head (the lemma above).
      · exact assignFrom_extends d.2 0 { s with seq := s.seq + 1 }
      -- Failed: no extra row, the same head.
      · exact ⟨[], by simp, by simp, Nat.le_refl _⟩
    -- Name the pieces of that fact.
    obtain ⟨e1, hr1, hp1, hh1⟩ := h1
    -- ... and the rest add rows above the head after it.
    obtain ⟨e2, hr2, hp2⟩ := foldl_extends ds (orderedStep s d)
    -- The extra rows are the first decision's followed by the rest's. Two parts.
    refine ⟨e1 ++ e2, ?_, ?_⟩
    -- The rows: unfold one step of the fold.
    · simp only [List.foldl_cons]
      -- Old rows, then the first decision's, then the rest's.
      rw [hr2, hr1, List.append_assoc]
    -- Above the head: take any extra row.
    · intro r hr
      -- It comes from the first decision or from the rest.
      rcases List.mem_append.mp hr with hr | hr
      -- From the first: above the old head.
      · exact hp1 r hr
      -- From the rest: above the head after the first decision,
      · have := hp2 r hr
        -- which is at least the old head.
        omega

/-- **A snapshot sees exactly rows 1..H.** Visibility follows sequence
order, so a snapshot sees the first `k` decisions' commits and no later
one; let H be the head in it. Then, among all the rows the log will ever
hold, those at positions up to H are exactly the snapshot's rows, and the
snapshot's rows sit at positions 1, ..., H, every one of them. A reader
bounded by its snapshot's head therefore never skips a row: no later
commit adds one at or below H. A crash that keeps a prefix of the commit
order leaves such a state. TLA+: `INV_Dense` and `INV_NoSkip`. -/
theorem snapshot_sees_prefix (ds : List (Bool × List (Option Nat))) (k : Nat) :
    -- The statement, part 1: of all rows of the whole log, those at or below the snapshot's head
    -- are exactly the snapshot's rows;
    (process ds).rows.filter (fun r => decide (r.pos ≤ (process (ds.take k)).head)) =
        (process (ds.take k)).rows ∧
      -- part 2: the snapshot's positions are 1, ..., its head.
      (process (ds.take k)).rows.map Row.pos = List.range' 1 (process (ds.take k)).head := by
  -- Part 2 is `dense_from_one` for the first k decisions. Part 1 is left.
  refine ⟨?_, dense_from_one (ds.take k)⟩
  -- Name the snapshot's log `p`.
  generalize hp : process (ds.take k) = p
  -- The whole log is `p` followed by the later decisions.
  have hsplit : process ds = (ds.drop k).foldl orderedStep p := by
    -- Put the snapshot's log back in place of p.
    rw [← hp]
    -- Unfold "process": a fold over the decisions.
    unfold process
    -- Folding the first k and then the rest is folding all of them.
    rw [← List.foldl_append, List.take_append_drop]
  -- Those only add rows above `p.head`.
  obtain ⟨extra, hrows, habove⟩ := foldl_extends (ds.drop k) p
  -- The whole log's rows are p's rows followed by the extra rows; filter each part on its own.
  rw [hsplit, hrows, List.filter_append]
  -- Every row of `p` is at or below its head (it is dense) ...
  have hdense : p.rows.map Row.pos = List.range' 1 p.head := by
    -- Put the snapshot's log back in place of p.
    rw [← hp]
    -- It is dense, by `dense_from_one`.
    exact dense_from_one (ds.take k)
  -- So the filter keeps every row of `p`.
  have hkeep : p.rows.filter (fun r => decide (r.pos ≤ p.head)) = p.rows := by
    -- A filter keeps everything exactly when every element passes it.
    rw [List.filter_eq_self]
    -- Take any row of p.
    intro r hr
    -- Its position is between 1 and p's head.
    have := pos_le_head hdense hr
    -- "Passes the filter" means: its position is at most p's head.
    simp only [decide_eq_true_eq]
    -- Which it is.
    omega
  -- ... and every later row is above it.
  have hdrop : extra.filter (fun r => decide (r.pos ≤ p.head)) = [] := by
    -- A filter keeps nothing exactly when no element passes it.
    rw [List.filter_eq_nil_iff]
    -- Take any extra row.
    intro r hr
    -- Its position is above p's head.
    have := habove r hr
    -- "Fails the filter" means: its position is not at most p's head.
    simp only [decide_eq_true_eq]
    -- Which is so.
    omega
  -- p's rows kept, the extra rows dropped: what remains is p's rows.
  rw [hkeep, hdrop, List.append_nil]

/-! ## The RED cases, as counterexamples -/

/-- The RED ViewOnlyHead: a group whose every member numbers its appends
from the head the group started with, not the head the earlier members
left. -/
def groupViewOnly (start : Log) : Log → List (List (Option Nat)) → Log
  -- No member left.
  | s, [] => s
  -- The next member takes a commit number, resets the head, and numbers.
  | s, apps :: rest =>
    -- Number the member's appends on a log whose next commit number moved up but whose head was
    -- reset to the group's starting head; then go on with the rest.
    groupViewOnly start (assignFrom s.seq 0 apps { s with seq := s.seq + 1, head := start.head }) rest

/-- **`view_only_head_duplicates`.** Two members appending once each, in
one group: read from the view only, both take position 1. The ordered step
of the design, folding over the members, gives them 1 and 2. -/
theorem view_only_head_duplicates :
    -- The statement: read from the view only, the two positions are 1 and 1;
    (groupViewOnly init init [[none], [none]]).rows.map Row.pos = [1, 1] ∧
      -- with the design they are 1 and 2.
      (process [(true, [none]), (true, [none])]).rows.map Row.pos = [1, 2] := by
  -- Both sides are small computations: compute them.
  decide

/-- The RED AssignBeforeValidation: positions are counted before
validation, so a transaction that then fails still moves the head past the
positions it would have taken. -/
def orderedStepEarly (s : Log) (d : Bool × List (Option Nat)) : Log :=
  -- A transaction that passed is decided as in the design; one that failed still moves the head to
  -- where numbering its appends would have put it.
  if d.1 then orderedStep s d else { s with head := (assignFrom s.seq 0 d.2 s).head }

/-- One transaction fails validation after taking position 1, then one
commits. -/
def earlyLog : Log := [(false, [none]), (true, [none])].foldl orderedStepEarly init

/-- **`assign_before_validation_leaves_hole`.** The committed append sits
at position 2 under head 2, and no row holds position 1: the log is not
dense. -/
theorem assign_before_validation_leaves_hole :
    -- The statement: the head is 2, the only row is at position 2,
    earlyLog.head = 2 ∧ earlyLog.rows.map Row.pos = [2] ∧
      -- and so the positions are not 1, 2.
      earlyLog.rows.map Row.pos ≠ List.range' 1 earlyLog.head := by
  -- All three are small computations: compute them.
  decide

/-- The RED Stamps (plan D1): an appended row's position is its commit's
sequence number, 1, 2, 3, ..., and every commit takes one, appending or
not. A reader's bound is the newest visible sequence, kept in `head`.
(At most one append per commit, as stamps had.) -/
def stampStep (s : Log) (d : Bool × List (Option Nat)) : Log :=
  -- A commit that passed:
  if d.1 then
    -- the bound and the next number both become seq + 1,
    { head := s.seq + 1, seq := s.seq + 1,
      -- and its first append, if any, gets a row at position seq + 1.
      rows := s.rows ++ (d.2.take 1).map (fun o => ⟨s.seq + 1, s.seq, 0, o⟩) }
  -- A commit that failed changes nothing.
  else s

/-- A commit that appends nothing, then a commit that appends once. -/
def stamped : Log := [(true, []), (true, [none])].foldl stampStep init

/-- **`stamps_leave_hole`.** Under a bound of 2 the log holds position 2
only: the commit that appended nothing consumed position 1. -/
theorem stamps_leave_hole :
    -- The statement: the bound is 2, the only row is at position 2,
    stamped.head = 2 ∧ stamped.rows.map Row.pos = [2] ∧
      -- and so the positions are not 1, 2.
      stamped.rows.map Row.pos ≠ List.range' 1 stamped.head := by
  -- All three are small computations: compute them.
  decide

-- The end of this file's names.
end Regolith.Append
