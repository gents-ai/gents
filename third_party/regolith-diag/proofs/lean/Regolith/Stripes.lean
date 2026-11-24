/-!
# Stripes: compaction under live snapshots (plan 4.8, E5)

This file backs the TLA+ model `proofs/tla/StripeCompaction.tla`,
invariants `SnapshotReadsKept`, `HeadKept` and `NewestKept`
(configurations `MC_StripeCompaction_*`).

## The story, for a reader who has never seen the code

The database never overwrites a value: every write adds a new version of
its key, stamped with a growing sequence number. A background job,
compaction, throws away the versions nobody can read any more. A snapshot
is a reader's frozen picture ("the database as of sequence 5"), and while
it is held, the version each key had at that moment must stay.

Tiny example: a key is written at sequences 3, 6 and 9, and a reader holds
a snapshot at 5. That reader reads version 3. A compaction that kept only
the newest version, 9, would change what the reader sees. Keeping 3 for the
reader and 9 for everyone newer is enough; only 6 can go.

What could go wrong: compaction drops or rewrites a version some live
reader still reads. What is proved here: with the engine's rule, no read at
a live snapshot, and no read without a snapshot, changes.

## The rule

Compaction reduces the versions of each key while snapshots are live. The
live snapshots cut the sequence line into stripes: two neighbouring
versions share a stripe when no live snapshot sits at or above the older
one and below the newer one. Inside a stripe only the newest state is
visible to any reader, so the engine (`Stripes::reduce_group` in
`src/engine/compaction/stripes.rs`) drops what is beneath the stripe's
newest value or deletion, folds the operands over it into one value, and
folds an operand-only stripe into one operand when the merge operator has
an exact `partial_merge`.

What is proved, in plain words:

1. `combine_reads`: replacing two neighbours of one stripe by the one
   entry they reduce to changes no read at a live snapshot or at the head,
   on top of whatever the older versions resolve to.
2. `reduce_reads`: so the whole reduction changes no such read.
   `live_snapshot_reads_kept`, `head_reads_kept` and `window_reads_kept`
   specialise it: a compaction of any window of a key's versions, with
   newer versions above it and older ones below, changes no read at a
   live snapshot, nor at the head.
3. `reduce_keeps_newest`: the newest version keeps its sequence, so commit
   validation sees the same newest write.
4. `inexact_fold_breaks_reads`: folding an operand-only stripe with a
   `partial_merge` that is not exact (a rounding sum, as with floating
   point) changes a read. This is the RED configuration
   `MC_StripeCompaction_Red_InexactFold`.
5. `unlisted_snapshot_breaks_reads`: a snapshot missing from the list the
   stripes are cut at loses the version it reads. That is what the E5
   capture window (`MC_StripeCompaction_Red_CaptureEarly`) and a
   stripe-blind reduction (`MC_StripeCompaction_Red_IgnoreStripes`) do.

The compaction filter (TLA+ `Filter = "Expire"`) is not a separate case
here: the reduction with a filter is the reduction without one applied to
the versions with the filter's decisions made in place, so `reduce_reads`
applies to that list.

How to read the Lean: `def` defines a function or a property, `theorem`
states a fact and its proof follows `:= by`. Lines starting with `--` are
comments.
-/

-- Everything below is named Regolith.Stripes.<name>.
namespace Regolith.Stripes

/-! ## Versions and reads -/

/-- What one version of a key does. Values and operands are natural
numbers; only how they combine matters. -/
inductive Op where
  /-- A put of a value. -/
  | put (v : Nat)
  /-- A deletion. -/
  | del
  /-- A merge operand. -/
  | merge (x : Nat)
  -- Lean may compare two of these for equality and print them; the counterexamples at the end
  -- compute with that.
  deriving DecidableEq, Repr

/-- One version of one key: the sequence it was written at and what it
does. A key's versions are listed newest first. -/
structure Entry where
  /-- The sequence the version was written at. -/
  seq : Nat
  /-- What it does. -/
  op : Op
  -- The same for versions.
  deriving DecidableEq, Repr

/-- A merge operator. -/
structure MergeOp where
  /-- `step base x`: apply operand `x` to the value beneath it, `none`
  when there is none (a deletion or nothing). `full_merge` is this applied
  to the operands one at a time, oldest first. -/
  step : Option Nat → Nat → Nat
  /-- `partial_merge`, when the operator has one: combine an older operand
  `x` and a newer operand `y` into one. -/
  pm : Option (Nat → Nat → Nat)

/-- `Exact m`: the operator's `partial_merge`, if it has one, agrees with
applying the two operands one at a time, on top of every base. An
operator that is not exact must omit it (plan 3.3). -/
def Exact (m : MergeOp) : Prop :=
  -- For every partial merge p the operator has, every base b and operands x and y: applying p x y
  -- to b equals applying x and then y.
  ∀ p, m.pm = some p → ∀ (b : Option Nat) (x y : Nat), m.step b (p x y) = m.step (some (m.step b x)) y

/-- `applyOn m es r`: what a read gets from the versions `es` (newest
first) on top of the base `r` that older versions resolve to. A put or a
deletion ends the read; an operand applies to what lies beneath it. -/
def applyOn (m : MergeOp) : List Entry → Option Nat → Option Nat
  -- No version: the base.
  | [], r => r
  -- A put answers its value, whatever lies beneath.
  | ⟨_, .put v⟩ :: _, _ => some v
  -- A deletion answers "absent", whatever lies beneath.
  | ⟨_, .del⟩ :: _, _ => none
  -- An operand applies to what the older versions resolve to.
  | ⟨_, .merge x⟩ :: rest, r => some (m.step (applyOn m rest r) x)

/-- The versions a snapshot at `s` sees: those written at or below it. -/
def vis (s : Nat) (es : List Entry) : List Entry := es.filter fun e => decide (e.seq ≤ s)

/-- A point read at snapshot `s` of a key whose versions are `es`. -/
def read (m : MergeOp) (s : Nat) (es : List Entry) : Option Nat := applyOn m (vis s es) none

/-- A read of `xs ++ ys` is a read of `xs` on top of the read of `ys`:
older versions matter only through what they resolve to. -/
theorem applyOn_append (m : MergeOp) :
    -- The statement: for all version lists xs and ys and every base r, reading xs ++ ys on r is
    -- reading xs on top of what ys gives on r.
    ∀ (xs ys : List Entry) (r : Option Nat), applyOn m (xs ++ ys) r = applyOn m xs (applyOn m ys r)
  -- No newer versions: both sides are the read of ys.
  | [], _, _ => rfl
  -- A put on top: both sides answer its value.
  | ⟨_, .put _⟩ :: _, _, _ => rfl
  -- A deletion on top: both sides answer "absent".
  | ⟨_, .del⟩ :: _, _, _ => rfl
  -- An operand on top, then the rest.
  | ⟨_, .merge x⟩ :: rest, ys, r => by
    -- The operand applies to the same thing on both sides, by induction.
    -- Unfold the read one step on both sides.
    simp only [List.cons_append, applyOn]
    -- The rest reads alike on both sides (the claim for the shorter list).
    rw [applyOn_append m rest ys r]

/-- Two lists of versions that resolve alike on every base still do after
the same version is put in front of both. -/
theorem applyOn_cons_congr (m : MergeOp) (a : Entry) {xs ys : List Entry}
    -- `xs` and `ys` resolve alike on every base.
    (h : ∀ r, applyOn m xs r = applyOn m ys r) (r : Option Nat) :
    -- The statement: with a in front of both, the two lists still resolve alike.
    applyOn m (a :: xs) r = applyOn m (a :: ys) r := by
  -- Take a apart: its sequence s and what it does, o.
  obtain ⟨s, o⟩ := a
  -- Split on what a does.
  cases o with
  -- A put: both sides answer its value.
  | put v => rfl
  -- A deletion: both sides answer "absent".
  | del => rfl
  -- An operand:
  | merge x =>
    -- The operand applies to what each side resolves to, which agree.
    -- unfold the read one step on both sides;
    simp only [applyOn]
    -- what lies beneath resolves alike, so the operand is applied to the same thing.
    rw [h r]

/-! ## Stripes and the reduction -/

/-- `sameStripe L a b`: the newer version `a` and the older version `b`
share a stripe under the live snapshots `L`: `b` is older, and no live
snapshot is at or above `b`'s sequence and below `a`'s. -/
def sameStripe (L : List Nat) (a b : Entry) : Bool :=
  -- b is older than a, and for every live snapshot l it is not the case that b's sequence is at or
  -- below l while l is below a's.
  decide (b.seq < a.seq) && L.all fun l => !(decide (b.seq ≤ l) && decide (l < a.seq))

/-- What `sameStripe` says, as two facts. -/
theorem sameStripe_spec {L : List Nat} {a b : Entry} (h : sameStripe L a b = true) :
    -- The statement: b is older than a, and no live snapshot l has b.seq ≤ l < a.seq.
    b.seq < a.seq ∧ ∀ l ∈ L, ¬ (b.seq ≤ l ∧ l < a.seq) := by
  -- Unfold the definition, turning the true-or-false computation into plain statements.
  simp only [sameStripe, Bool.and_eq_true, decide_eq_true_eq, List.all_eq_true,
    Bool.not_eq_true', Bool.and_eq_false_iff, decide_eq_false_iff_not] at h
  -- Take the two parts: "b is older" and "every live snapshot fails one comparison".
  obtain ⟨hlt, hall⟩ := h
  -- The first part is given; for the second, take a live snapshot l with both comparisons true.
  refine ⟨hlt, fun l hl ⟨h1, h2⟩ => ?_⟩
  -- Each live snapshot fails one of the two comparisons.
  -- The second part says one of the two fails for l.
  rcases hall l hl with h | h
  -- The first fails: a contradiction with h1.
  · exact h h1
  -- The second fails: a contradiction with h2.
  · exact h h2

/-- `combine m a b`: the one version that replaces the newer `a` and the
older `b` of one stripe, when the reduction folds them; `none` when it
keeps both.

* A put or deletion on top hides `b`: it stays, `b` goes.
* An operand on a put or a deletion folds into a put of the result, at
  the operand's sequence (`full_merge` with a base).
* Two operands fold into one through `partial_merge`, when the operator
  has one; otherwise both stay. -/
def combine (m : MergeOp) (a b : Entry) : Option Entry :=
  -- Split on what the newer version a does.
  match a with
  -- A put: a alone stays.
  | ⟨_, .put _⟩ => some a
  -- A deletion: a alone stays.
  | ⟨_, .del⟩ => some a
  -- An operand y at sequence s: it depends on what the older version b does.
  | ⟨s, .merge y⟩ =>
    -- Split on b.
    match b with
    -- b is a put of v: one put, at a's sequence, of y applied to v.
    | ⟨_, .put v⟩ => some ⟨s, .put (m.step (some v) y)⟩
    -- b is a deletion: one put, at a's sequence, of y applied to nothing.
    | ⟨_, .del⟩ => some ⟨s, .put (m.step none y)⟩
    -- b is an operand x:
    | ⟨_, .merge x⟩ =>
      -- it depends on whether the operator has a partial merge.
      match m.pm with
      -- It has one, p: one operand, at a's sequence, holding p x y.
      | some p => some ⟨s, .merge (p x y)⟩
      -- It has none: both stay.
      | none => none

/-- The folded version keeps the newer version's sequence. -/
-- (given that a and b fold into c) The statement: c's sequence is a's.
theorem combine_seq {m : MergeOp} {a b c : Entry} (h : combine m a b = some c) : c.seq = a.seq := by
  -- Take a apart: its sequence and what it does.
  obtain ⟨sa, oa⟩ := a
  -- Take b apart the same way.
  obtain ⟨sb, ob⟩ := b
  -- Go through every pair of kinds (put, deletion, operand) for a and b, and unfold the fold in
  -- each.
  cases oa <;> cases ob <;> simp only [combine, Option.some.injEq] at h
  -- In each of the nine cases, one of two closings works:
  all_goals first
    -- c is literally the folded version, whose sequence is a's;
    | (subst h; rfl)
    -- or, for two operands, split on whether a partial merge exists (with none, no fold happened;
    -- with one,
    | (cases hp : m.pm <;> simp only [hp, reduceCtorEq, Option.some.injEq] at h
       -- c is again the folded version at a's sequence).
       subst h; rfl)

/-- **One fold changes no read.** Let `a` and `b` share a stripe and fold
into `c`, with an exact operator. A read at a live snapshot `s`, or at any
`s` at or above `a`'s sequence (the head), gets the same from `c` as from
`a` then `b`, whatever follows and whatever base lies beneath. -/
theorem combine_reads {m : MergeOp} (hm : Exact m) {L : List Nat} {a b c : Entry} {s : Nat}
    -- `a` and `b` share a stripe.
    (hss : sameStripe L a b = true)
    -- They fold into `c`.
    (hc : combine m a b = some c)
    -- The read is at a live snapshot, or at or above `a`.
    (hs : s ∈ L ∨ a.seq ≤ s) (rest : List Entry) (r : Option Nat) :
    -- The statement: reading c followed by rest, as seen at s, equals reading a, b, rest, as seen
    -- at s.
    applyOn m (vis s (c :: rest)) r = applyOn m (vis s (a :: b :: rest)) r := by
  -- From "same stripe": b is older than a, and no live snapshot sits between them.
  obtain ⟨hba, hno⟩ := sameStripe_spec hss
  -- c has a's sequence.
  have hcs := combine_seq hc
  -- No live snapshot falls between `b` and `a`, so the read sees both or
  -- neither.
  -- The read sees neither (s is below b) or both (s is at or above a):
  have hcase : s < b.seq ∨ a.seq ≤ s := by
    -- split on why s qualifies.
    rcases hs with hsL | hle
    -- s is a live snapshot: it does not sit between b and a,
    · have := hno s hsL
      -- so it is below b or at or above a, by arithmetic.
      omega
    -- s is at or above a: the second alternative.
    · exact Or.inr hle
  -- Split on those two alternatives.
  rcases hcase with hlt | hle
  -- Neither is visible, nor is `c`, which has `a`'s sequence.
  ·
    -- c is not visible at s.
    have h1 : ¬ c.seq ≤ s := by omega
    -- a is not visible at s.
    have h2 : ¬ a.seq ≤ s := by omega
    -- b is not visible at s.
    have h3 : ¬ b.seq ≤ s := by omega
    -- Drop all three from both sides: what remains is the same list.
    simp only [vis, List.filter_cons, h1, h2, h3, decide_false, Bool.false_eq_true, ite_false]
  -- All three are visible: compare what each side resolves to.
  ·
    -- c is visible at s.
    have h1 : c.seq ≤ s := by omega
    -- b is visible at s.
    have h3 : b.seq ≤ s := by omega
    -- Keep all three in the visible lists.
    simp only [vis, List.filter_cons, h1, hle, h3, decide_true, ite_true]
    -- Name the visible part of the rest R; it is the same on both sides.
    generalize List.filter (fun e => decide (e.seq ≤ s)) rest = R
    -- Take a apart.
    obtain ⟨sa, oa⟩ := a
    -- Take b apart.
    obtain ⟨sb, ob⟩ := b
    -- Split on what a does.
    cases oa with
    -- Case: a is a put of v.
    | put v =>
      -- A put on top: `c` is `a`, and both read its value.
      -- The fold kept a: c is a.
      simp only [combine, Option.some.injEq] at hc
      -- Replace c by a.
      subst hc
      -- Both sides read a's value.
      rfl
    -- Case: a is a deletion.
    | del =>
      -- A deletion on top: `c` is `a`, and both read "absent".
      -- The fold kept a: c is a.
      simp only [combine, Option.some.injEq] at hc
      -- Replace c by a.
      subst hc
      -- Both sides read "absent".
      rfl
    -- Case: a is an operand y.
    | merge y =>
      -- Split on what b does.
      cases ob with
      -- b is a put of v.
      | put v =>
        -- The operand folded onto the put: both read `step (some v) y`.
        -- The fold made c the put of y applied to v.
        simp only [combine, Option.some.injEq] at hc
        -- Replace c by it.
        subst hc
        -- Both sides read that value.
        rfl
      -- b is a deletion.
      | del =>
        -- The operand folded onto the deletion: both read `step none y`.
        -- The fold made c the put of y applied to nothing.
        simp only [combine, Option.some.injEq] at hc
        -- Replace c by it.
        subst hc
        -- Both sides read that value.
        rfl
      -- b is an operand x.
      | merge x =>
        -- Two operands folded by `partial_merge`: exactness says applying
        -- `p x y` equals applying `x` then `y`, on the same base.
        -- Split on whether the operator has a partial merge.
        cases hp : m.pm with
        -- None: then nothing was folded, against the assumption.
        | none => simp only [combine, hp, reduceCtorEq] at hc
        -- One, p:
        | some p =>
          -- the fold made c the operand p x y.
          simp only [combine, hp, Option.some.injEq] at hc
          -- Replace c by it.
          subst hc
          -- Unfold the read on both sides: p x y applied once, against x and then y.
          simp only [applyOn]
          -- Exactness says those are equal.
          rw [hm p hp]

/-- `push m L a out`: put the newer version `a` on top of the already
reduced older versions `out`, folding it into the first of them while
they share a stripe and fold. -/
def push (m : MergeOp) (L : List Nat) (a : Entry) : List Entry → List Entry
  -- Nothing reduced yet: a alone.
  | [] => [a]
  -- A reduced older version b on top of the rest:
  | b :: out =>
    -- when a and b share a stripe,
    if sameStripe L a b then
      -- try to fold them;
      match combine m a b with
      -- folded into c: push c onto the rest instead;
      | some c => push m L c out
      -- not folded: a goes on top.
      | none => a :: b :: out
    -- Different stripes: a goes on top.
    else a :: b :: out

/-- `reduce m L es`: the reduction of a key's versions `es` (newest
first), built from the oldest up. Within each stripe this drops what lies
beneath the newest put or deletion, folds the operands over it into one
put, and folds an operand-only stripe into one operand when `partial_merge`
exists, which is what `Stripes::reduce_stripe` does. -/
def reduce (m : MergeOp) (L : List Nat) : List Entry → List Entry
  -- No version: nothing.
  | [] => []
  -- Newest version a over the rest: reduce the rest, then push a onto it.
  | a :: rest => push m L a (reduce m L rest)

/-- Pushing a version changes no read at a live snapshot or at or above
that version, whatever base lies beneath. -/
theorem push_reads {m : MergeOp} (hm : Exact m) {L : List Nat} {s : Nat} :
    -- The statement: for every reduced list out and version a, if s is a live snapshot or at or
    -- above a,
    ∀ (out : List Entry) (a : Entry), (s ∈ L ∨ a.seq ≤ s) →
      -- then for every base, reading "a pushed onto out" at s equals reading a followed by out at
      -- s.
      ∀ r, applyOn m (vis s (push m L a out)) r = applyOn m (vis s (a :: out)) r
  -- Nothing reduced yet: pushing a gives a alone, the same list.
  | [], _, _, _ => rfl
  -- A reduced version b on top of out.
  | b :: out, a, hs, r => by
    -- Unfold one step of push.
    simp only [push]
    -- Split on whether a and b share a stripe.
    split
    -- They do; name that fact hss.
    · rename_i hss
      -- Split on whether they fold.
      split
      -- `a` and `b` fold into `c`: push `c` on, then undo the fold.
      ·
        -- Name the folded version c and the fact hc.
        rename_i c hc
        -- c has a's sequence.
        have hcs := combine_seq hc
        -- So the condition on s holds for c as it did for a.
        have hs' : s ∈ L ∨ c.seq ≤ s := by rw [hcs]; exact hs
        -- Pushing c changes no read (the claim for the shorter list).
        rw [push_reads hm out c hs' r]
        -- And reading c equals reading a then b (one fold changes no read).
        exact combine_reads hm hss hc hs out r
      -- No fold: the lists are the same.
      ·
        -- Nothing to show.
        rfl
    -- Different stripes: the lists are the same.
    ·
      -- Nothing to show.
      rfl

/-- **The reduction changes no read at a live snapshot or at the head.**
For every snapshot `s` in the live list `L`, and every `s` at or above
every version, the reduced versions read on top of any base exactly as
the versions did. -/
theorem reduce_reads {m : MergeOp} (hm : Exact m) {L : List Nat} {s : Nat} :
    -- The statement: for every version list es, if s is a live snapshot or at or above every
    -- version, then on every base the reduced versions read at s exactly as the versions did.
    ∀ (es : List Entry), (s ∈ L ∨ ∀ e ∈ es, e.seq ≤ s) →
      ∀ r, applyOn m (vis s (reduce m L es)) r = applyOn m (vis s es) r
  -- No version: nothing was reduced.
  | [], _, _ => rfl
  -- Newest version a over the rest.
  | a :: rest, hs, r => by
    -- The condition holds for `a` and for the older versions.
    have ha : s ∈ L ∨ a.seq ≤ s := by
      -- Split on why s qualifies.
      rcases hs with h | h
      -- A live snapshot: it qualifies for a too.
      · exact Or.inl h
      -- At or above every version: in particular at or above a.
      · exact Or.inr (h a List.mem_cons_self)
    -- The condition also holds for the older versions:
    have hrest : s ∈ L ∨ ∀ e ∈ rest, e.seq ≤ s := by
      -- split on why s qualifies.
      rcases hs with h | h
      -- A live snapshot: still one.
      · exact Or.inl h
      -- At or above every version: in particular those of the rest.
      · exact Or.inr fun e he => h e (List.mem_cons_of_mem _ he)
    -- The older versions reduce without changing a read, by induction.
    -- The claim for the older versions.
    have ih := reduce_reads hm rest hrest
    -- Pushing `a` changes no read either.
    -- Unfold one step of the reduction: a pushed onto the reduced rest.
    simp only [reduce]
    -- Pushing a changes no read: read a followed by the reduced rest instead.
    rw [push_reads hm _ a ha r]
    -- With `a` in front of both, what follows reads alike, so the whole does.
    -- Split on whether a is visible at s.
    by_cases hv : a.seq ≤ s
    -- Visible: a stays in front on both sides.
    · simp only [vis, List.filter_cons, hv, decide_true, ite_true]
      -- With a in front of two lists that resolve alike, the wholes resolve alike.
      exact applyOn_cons_congr m a ih r
    -- Not visible: a drops out on both sides.
    · simp only [vis, List.filter_cons, hv, decide_false, Bool.false_eq_true, ite_false]
      -- What remains is the claim for the older versions.
      exact ih r

/-- At a live snapshot, a read of the reduced versions answers what it
answered before. -/
theorem live_snapshot_reads_kept {m : MergeOp} (hm : Exact m) {L : List Nat} {s : Nat}
    -- `s` is a live snapshot.
    (hs : s ∈ L) (es : List Entry) :
    -- The statement: the read at s is the same before and after the reduction.
    read m s (reduce m L es) = read m s es :=
  -- The reduction theorem, with "s is a live snapshot" and nothing beneath.
  reduce_reads hm es (Or.inl hs) none

/-- At the head (any `s` at or above every version), a read of the
reduced versions answers what it answered before. -/
theorem head_reads_kept {m : MergeOp} (hm : Exact m) (L : List Nat) {s : Nat} {es : List Entry}
    -- `s` is at or above every version.
    (hs : ∀ e ∈ es, e.seq ≤ s) :
    -- The statement: the read at s is the same before and after the reduction.
    read m s (reduce m L es) = read m s es :=
  -- The reduction theorem, with "s is at or above every version" and nothing beneath.
  reduce_reads hm es (Or.inr hs) none

/-- **A compaction window.** The inputs of a compaction hold a run `mid`
of a key's versions; newer versions `above` (memtable, newer files) and
older ones `below` (deeper files) lie outside it. Reducing `mid` changes
no read at a live snapshot, nor at any `s` at or above every input. This
covers an operand-only stripe whose base lies below the window. -/
theorem window_reads_kept {m : MergeOp} (hm : Exact m) {L : List Nat} {s : Nat}
    -- (for any newer versions, any window and any older versions)
    (above mid below : List Entry)
    -- A live snapshot, or a read at or above every input.
    (hs : s ∈ L ∨ ∀ e ∈ mid, e.seq ≤ s) :
    -- The statement: a read of the three parts is the same with the window reduced.
    read m s (above ++ reduce m L mid ++ below) = read m s (above ++ mid ++ below) := by
  -- A read of the three parts is a read of `above` on top of a read of
  -- the window on top of a read of `below`; only the middle changed.
  -- Reducing the window changes no read on top of the base the older versions resolve to.
  have h := reduce_reads hm mid hs (applyOn m (vis s below) none)
  -- Split both reads into the three parts: `above` on top of the window on top of `below`.
  simp only [read, vis, List.filter_append, applyOn_append] at h ⊢
  -- Only the middle differs, and it reads alike.
  rw [h]

/-- Pushing a version leaves a list headed by a version of the same
sequence. -/
theorem push_head (m : MergeOp) (L : List Nat) :
    -- The statement: for every reduced list and version a, the result of pushing a starts with a
    -- version at a's sequence.
    ∀ (out : List Entry) (a : Entry), ∃ e tl, push m L a out = e :: tl ∧ e.seq = a.seq
  -- Nothing reduced yet: the result is a alone.
  | [], a => ⟨a, [], rfl, rfl⟩
  -- A reduced version b on top of out.
  | b :: out, a => by
    -- Unfold one step of push.
    simp only [push]
    -- Split on whether a and b share a stripe.
    split
    -- They do: split on whether they fold.
    · split
      -- Folded into `c`, which keeps `a`'s sequence.
      ·
        -- Name the folded version c and the fact hc.
        rename_i c hc
        -- Pushing c starts with a version at c's sequence (the claim for the shorter list),
        obtain ⟨e, tl, he, hseq⟩ := push_head m L out c
        -- which is a's sequence.
        exact ⟨e, tl, he, hseq.trans (combine_seq hc)⟩
      -- Not folded: a is on top.
      · exact ⟨a, b :: out, rfl, rfl⟩
    -- Different stripes: a is on top.
    · exact ⟨a, b :: out, rfl, rfl⟩

/-- **The newest version keeps its sequence.** The reduction of a key's
versions, newest first, starts with a version at the newest one's
sequence. -/
theorem reduce_keeps_newest (m : MergeOp) (L : List Nat) (a : Entry) (rest : List Entry) :
    -- The statement: the reduced list starts with a version at a's sequence.
    ∃ e tl, reduce m L (a :: rest) = e :: tl ∧ e.seq = a.seq :=
  -- The reduction pushes a last, and pushing a leaves a version at a's sequence on top.
  push_head m L _ a

/-! ## The RED cases -/

/-- A sum that rounds to an even number after every addition, as a
floating-point sum rounds: adding 1 to an even total is lost. Its
`partial_merge` adds the operands first, which is not exact. -/
def roundingSum : MergeOp where
  -- Applying x to base b: add x to the base (0 for none) and round down to an even number.
  step b x := 2 * ((b.getD 0 + x) / 2)
  -- Its partial merge: the plain sum of the two operands.
  pm := some fun x y => x + y

/-- The rounding sum's `partial_merge` is not exact: on nothing, `1` then
`1` gives `0`, but `1 + 1` gives `2`. -/
theorem roundingSum_not_exact : ¬ Exact roundingSum := by
  -- Suppose it were exact.
  intro h
  -- Use exactness on its partial merge with no base and the operands 1 and 1.
  have := h (fun x y => x + y) rfl none 1 1
  -- Computing both sides gives 2 = 0: impossible.
  simp [roundingSum] at this

/-- **`inexact_fold_breaks_reads`.** Two operands of 1 over nothing, with
no live snapshot between them: the head reads `0` (each addition rounds
away). Folding the operand-only stripe with the inexact `partial_merge`
leaves one operand of `2`, and the head reads `2`. -/
theorem inexact_fold_breaks_reads :
    -- The statement: before the reduction the head reads 0;
    read roundingSum 2 [⟨2, .merge 1⟩, ⟨1, .merge 1⟩] = some 0 ∧
    -- the reduction leaves one operand of 2;
    reduce roundingSum [] [⟨2, .merge 1⟩, ⟨1, .merge 1⟩] = [⟨2, .merge 2⟩] ∧
    -- and after it the head reads 2.
    read roundingSum 2 (reduce roundingSum [] [⟨2, .merge 1⟩, ⟨1, .merge 1⟩]) = some 2 := by
  -- All three are small computations: compute them.
  decide

/-- The same operator with no `partial_merge` (plan 3.3: an operator that
is not exact omits it) is exact vacuously, and its operand-only stripe
stays as written. -/
theorem roundingSum_without_pm :
    -- The statement: without a partial merge the operator counts as exact, and the reduction leaves
    -- the two operands as they were.
    Exact { roundingSum with pm := none } ∧
    reduce { roundingSum with pm := none } [] [⟨2, .merge 1⟩, ⟨1, .merge 1⟩] =
      [⟨2, .merge 1⟩, ⟨1, .merge 1⟩] := by
  -- Two parts: exactness (shown next) and the reduction (computed).
  refine ⟨fun p hp => ?_, by decide⟩
  -- There is no `partial_merge` to be inexact.
  -- The operator has no partial merge, so "it has the partial merge p" is impossible.
  cases hp

/-- A plain sum whose `partial_merge` adds the operands: exact. -/
def plainSum : MergeOp where
  -- Applying x to base b: add x to the base (0 for none).
  step b x := b.getD 0 + x
  -- Its partial merge: the plain sum of the two operands.
  pm := some fun x y => x + y

/-- The plain sum's `partial_merge` is exact. -/
theorem plainSum_exact : Exact plainSum := by
  -- Take its partial merge p, a base and two operands.
  intro p hp b x y
  -- p is the plain sum.
  simp only [plainSum, Option.some.injEq] at hp
  -- Replace p by it.
  subst hp
  -- (b + x) + y equals b + (x + y).
  simp [plainSum, Nat.add_assoc]

/-- **`unlisted_snapshot_breaks_reads`.** A snapshot at 1 reads the put of
5. Reduced under a list that misses it (the E5 window, or a reduction
blind to stripes), the put of 7 hides the put of 5 and the snapshot reads
"absent". Under a list that holds it, the put of 5 stays. -/
theorem unlisted_snapshot_breaks_reads :
    -- The statement: the snapshot at 1 reads 5;
    read plainSum 1 [⟨2, .put 7⟩, ⟨1, .put 5⟩] = some 5 ∧
    -- reduced with an empty list of live snapshots, it reads nothing;
    read plainSum 1 (reduce plainSum [] [⟨2, .put 7⟩, ⟨1, .put 5⟩]) = none ∧
    -- reduced with the list [1], it still reads 5.
    read plainSum 1 (reduce plainSum [1] [⟨2, .put 7⟩, ⟨1, .put 5⟩]) = some 5 := by
  -- All three are small computations: compute them.
  decide

-- The end of this file's names.
end Regolith.Stripes
