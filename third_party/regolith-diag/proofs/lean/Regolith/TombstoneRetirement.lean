/-!
# TombstoneRetirement: dropping a tombstone nobody needs changes no read (E27)

This file backs the TLA+ model `proofs/tla/TombstoneRetirement.tla`,
invariants `SnapshotReadsKept` and `HeadKept` (configurations
`MC_TombstoneRetirement_Green`, `MC_TombstoneRetirement_Red_IgnoreSnapshots`
and `MC_TombstoneRetirement_Red_IgnoreDeeper`). The model checks two keys and
four writes; this file proves the law for any keys, writes and snapshots.

## The story, with a tiny example

A range tombstone says "every key from `lo` to `hi` is deleted, as of
sequence `s`". Dropping a column family writes one. If compaction carried it
forever, every later pass over its range would pay for it (E27).

A compaction pass merges two adjacent runs, a window `W`. It already drops
every put that a tombstone of `W` hides in the put's own snapshot stripe
(`Stripes::shadowed`). The fix also drops ("retires") a tombstone of `W`
when (1) no deeper run holds anything in its range, and (2) every live
snapshot is at or above its sequence (`Retirement::carried_tombstones`,
`src/engine/compaction/retire.rs`).

Example: key 7 was put at sequence 1, then a tombstone over 5..9 at
sequence 3. With no snapshot below 3 and nothing deeper, the pass drops the
put (hidden) and the tombstone (retired): key 7 reads absent before and
after. With a snapshot at 2, the put stays (that snapshot still sees it),
and so must the tombstone, or a read at the head would find the put again.

## What is proved, in plain words

1. `retire_keeps_reads`: for every key and every read point that is a live
   snapshot or the head, the put that wins the read is the same before and
   after the pass.
2. `retire_keeps_read_value`: so the value read is the same.
3. `ignore_snapshots_breaks_reads` and `ignore_deeper_breaks_reads`: drop
   either condition and a read at the head changes. These are the RED
   configurations as concrete counterexamples.

## What the proof assumes, and where the engine provides it

- Everything above the window (the memtables and newer runs, `A`) is newer
  than everything in it: entries enter at the top and a pass merges
  adjacent runs (`LsmOrder.lean`).
- A read point is a live snapshot, or the head, which is at or above every
  sequence.
-/

-- Everything below lives in this namespace, so its names do not clash.
namespace Regolith.TombstoneRetirement

/-- The two kinds of entry the argument is about. -/
inductive Kind where
  /-- A put of one key. -/
  | put
  /-- A range tombstone over the keys `lo ..= hi`. -/
  | rt
  -- Kinds can be compared, so `decide` can tell them apart
  deriving DecidableEq

/-- One entry of the tree. A put of key `k` has `lo = hi = k`. -/
structure Entry where
  /-- A put or a range tombstone. -/
  kind : Kind
  /-- The first key it holds or covers. -/
  lo : Nat
  /-- The last key it holds or covers. -/
  hi : Nat
  /-- The sequence that wrote it; larger is newer. -/
  seq : Nat

/-- A set of entries, as a yes-or-no test on each entry. -/
abbrev Entries := Entry → Prop

/-- `e` is a put of key `k`. -/
def PutOf (e : Entry) (k : Nat) : Prop := e.kind = .put ∧ e.lo = k ∧ e.hi = k

/-- `t` is a range tombstone covering key `k`. -/
def Covers (t : Entry) (k : Nat) : Prop := t.kind = .rt ∧ t.lo ≤ k ∧ k ≤ t.hi

/-- **The read rule.** At read point `s`, sequence `p` wins the read of key
`k` in `E`: a put of `k` at `p` is there, visible at `s`, newest among the
visible puts of `k`, and no visible tombstone covering `k` is newer. This
is `ReadIn` of the TLA+ model, stated as who wins rather than as a value. -/
def Wins (E : Entries) (k s p : Nat) : Prop :=
  -- There is a put of `k` at sequence `p`,
  (∃ e, E e ∧ PutOf e k ∧ e.seq = p) ∧
  -- it is visible at `s`,
  p ≤ s ∧
  -- every visible put of `k` is no newer,
  (∀ e, E e → PutOf e k → e.seq ≤ s → e.seq ≤ p) ∧
  -- and no visible tombstone covering `k` is newer than it.
  (∀ t, E t → Covers t k → ¬ (p < t.seq ∧ t.seq ≤ s))

/-- The value a read returns: the winner's sequence, or `0` (absent) when
nothing wins. -/
def ReadsAs (E : Entries) (k s v : Nat) : Prop :=
  -- Either `v` wins,
  Wins E k s v ∨
  -- or nothing wins and the read is absent.
  (v = 0 ∧ ∀ p, ¬ Wins E k s p)

/-- At most one sequence wins a read, so the read has one value. -/
theorem wins_unique {E : Entries} {k s p q : Nat}
    -- `p` wins,
    (hp : Wins E k s p)
    -- and so does `q`.
    (hq : Wins E k s q) : p = q := by
  -- Name the put that makes `p` win, and its "newest" clause.
  obtain ⟨⟨ep, hEp, hPp, hsp⟩, hps, hpmax, _⟩ := hp
  -- And the one that makes `q` win.
  obtain ⟨⟨eq, hEq, hPq, hsq⟩, hqs, hqmax, _⟩ := hq
  -- `q` is newest among visible puts, so `p`'s put is no newer than `q`.
  have h1 := hqmax ep hEp hPp (by omega)
  -- And the other way round, using `p`'s "newest" clause.
  have h2 := hpmax eq hEq hPq (by omega)
  -- Both directions: equal.
  omega

/-! ## The pass -/

/-- `Stripes::shadowed`: put `e` of window `W` is hidden by a tombstone `t` of
`W` in `e`'s own snapshot stripe: `t` covers `e`'s key, is newer than `e`,
and is no newer than any live snapshot at or above `e` (the top of `e`'s
stripe). The pass drops it. -/
def Shadowed (W : Entries) (live : Nat → Prop) (e : Entry) : Prop :=
  -- `e` is a put,
  e.kind = .put ∧
  -- and some tombstone of the window
  ∃ t, W t ∧
    -- covers its key,
    Covers t e.lo ∧
    -- is newer than it,
    e.seq < t.seq ∧
    -- and lies in its stripe: no live snapshot at or above `e` is below `t`.
    ∀ l, live l → e.seq ≤ l → t.seq ≤ l

/-- A deeper entry `b` meets tombstone `t`'s range. -/
def Meets (b t : Entry) : Prop := b.lo ≤ t.hi ∧ t.lo ≤ b.hi

/-- **The fix's retirement rule.** A tombstone of the window retires when every
live snapshot is at or above it and nothing deeper meets its range. -/
def Retired (W B : Entries) (live : Nat → Prop) (t : Entry) : Prop :=
  -- It is a tombstone of the window,
  W t ∧ t.kind = .rt ∧
  -- no live snapshot is below it,
  (∀ l, live l → t.seq ≤ l) ∧
  -- and no deeper entry meets its range.
  ∀ b, B b → ¬ Meets b t

/-- The tree before the pass: newer entries `A`, the window `W`, deeper `B`. -/
def Before (A W B : Entries) : Entries := fun e => A e ∨ W e ∨ B e

/-- The tree after a pass that retires by rule `R`: the window loses what is
shadowed and what `R` retires. -/
def After (A W B : Entries) (live : Nat → Prop) (R : Entry → Prop) : Entries :=
  -- Kept above, kept in the window unless dropped, kept deeper
  fun e => A e ∨ (W e ∧ ¬ Shadowed W live e ∧ ¬ R e) ∨ B e

/-- A retired tombstone `t` hides no surviving put of a key it covers that is
older than it: such a put is not newer (`A` is newer than the window), not
deeper (nothing deeper meets `t`), and not in the window (`t` shadows it
there). The heart of the argument. -/
theorem retired_leaves_nothing_under {A W B : Entries} {live : Nat → Prop}
    -- A tombstone t, a surviving entry e, and a key k
    {t e : Entry} {k : Nat}
    -- Everything above the window is newer than everything in it,
    (hA : ∀ a w, A a → W w → w.seq < a.seq)
    -- `t` is retired,
    (hR : Retired W B live t)
    -- and covers `k`,
    (hc : Covers t k)
    -- `e` survives the pass,
    (he : After A W B live (Retired W B live) e)
    -- is a put of `k`,
    (hp : PutOf e k)
    -- and is older than `t`.
    (hlt : e.seq < t.seq) : False := by
  -- Unpack the retirement: in the window, a tombstone, above every live
  -- snapshot, and clear of everything deeper.
  obtain ⟨hWt, hrt, hlive, hclear⟩ := hR
  -- Where does the surviving put live?
  rcases he with hAe | ⟨hWe, hns, _⟩ | hBe
  -- Above the window: newer than `t`, which is in the window.
  ·
    -- Everything above the window is newer than everything in it, t included.
    have := hA e t hAe hWt
    -- But it is older than `t`: impossible.
    omega
  -- In the window and not shadowed. But `t` shadows it:
  ·
    -- It is enough to show that t shadows it, since it was said not to be shadowed.
    apply hns
    -- it is a put, `t` is in the window, covers its key, is newer, and every
    -- live snapshot is at or above `t`.
    refine ⟨hp.1, t, hWt, ?_, hlt, fun l hl _ => hlive l hl⟩
    -- `t` covers `e.lo`, which is `k`.
    rw [hp.2.1]; exact hc
  -- Deeper: then it meets `t`'s range, which retirement forbids.
  ·
    -- The retirement condition says no deeper entry meets t's range; it is left to show that this
    -- one does.
    apply hclear e hBe
    -- `e` holds only `k`, and `t` covers `k`.
    exact ⟨by rw [hp.2.1]; exact hc.2.2, by rw [hp.2.2]; exact hc.2.1⟩

/-- **Retiring keeps every read.** For every key `k` and every read point `s`
that is a live snapshot or the head, the same sequence wins the read of `k`
before and after a pass that drops what its window shadows and retires by
the fix's rule. This rules out a snapshot reading a deleted put back. -/
theorem retire_keeps_reads {A W B : Entries} {live : Nat → Prop} {k s : Nat}
    -- Everything above the window is newer than everything in it,
    (hA : ∀ a w, A a → W w → w.seq < a.seq)
    -- and `s` is a live snapshot or the head.
    (hs : live s ∨ ∀ e, Before A W B e → e.seq ≤ s) :
    -- Claim: the same p wins before and after
    ∀ p, Wins (Before A W B) k s p ↔ Wins (After A W B live (Retired W B live)) k s p := by
  -- Take any sequence `p`.
  intro p
  -- A tombstone of the window that is newer than a put at `q ≤ s` and in
  -- that put's stripe is visible at `s`: the snapshot `s` caps the stripe,
  -- and the head is above everything.
  have visible : ∀ (q : Nat) (t : Entry), W t → q ≤ s →
      (∀ l, live l → q ≤ l → t.seq ≤ l) → t.seq ≤ s := by -- claim: such a tombstone is visible at s
    -- Take the put's sequence `q`, the tombstone `t`, and the facts.
    intro q t hWt hq hstripe
    -- Split on the kind of read point.
    rcases hs with hl | hhead
    -- A live snapshot at or above `q`: the stripe rule bounds `t` by it.
    ·
      -- The stripe rule, applied to the snapshot s, gives exactly this.
      exact hstripe s hl hq
    -- The head: at or above everything, `t` included.
    ·
      -- The head is at or above every entry there was before the pass, and t is one of them.
      exact hhead t (Or.inr (Or.inl hWt))
  -- Two directions.
  constructor
  -- Before to after. Unpack the win before.
  ·
    -- Name the parts: the winning put e0; p is visible at s; no visible put of k is newer; no
    -- visible tombstone hides it.
    intro ⟨⟨e0, hE0, hP0, hs0⟩, hps, hmax, hnot⟩
    -- Four clauses to show after.
    refine ⟨⟨e0, ?_, hP0, hs0⟩, hps, ?_, ?_⟩
    -- The winning put survives. Where was it?
    ·
      -- Above the window, in it, or deeper.
      rcases hE0 with h | h | h
      -- Above the window: kept.
      ·
        -- "Above" is the first case of "after".
        exact Or.inl h
      -- In the window: kept unless shadowed or retired.
      ·
        -- The second case of "after"; "not shadowed" and "not retired" are left.
        refine Or.inr (Or.inl ⟨h, ?_, ?_⟩)
        -- Shadowed would mean a covering tombstone newer than it and visible at `s`, which its win
        -- rules out.
        ·
          -- Suppose it is shadowed, by a tombstone t of the window.
          intro ⟨_, t, hWt, hc, hlt, hstripe⟩
          -- That tombstone is visible at `s`.
          have hts := visible e0.seq t hWt (by omega) hstripe
          -- And it covers `k`, the put's key.
          rw [hP0.2.1] at hc
          -- The win says no such tombstone exists.
          exact hnot t (Or.inr (Or.inl hWt)) hc ⟨by omega, hts⟩
        -- Retired needs a tombstone; the winner is a put.
        ·
          -- Suppose it is retired; then its kind is "tombstone".
          intro ⟨_, hrt, _⟩
          -- `put = rt` is false.
          rw [hP0.1] at hrt
          -- Distinct constructors.
          exact absurd hrt (by decide)
      -- Deeper: kept.
      ·
        -- "Deeper" is the third case of "after".
        exact Or.inr (Or.inr h)
    -- Every put after was there before, so none is newer.
    ·
      -- Take a visible put e of k that is there after the pass.
      intro e he hP hes
      -- Turn "after" into "before".
      apply hmax e _ hP hes
      -- Each case of "after" is a case of "before".
      rcases he with h | ⟨h, _, _⟩ | h
      -- Above.
      ·
        -- The first case of "before".
        exact Or.inl h
      -- In the window.
      ·
        -- The second case of "before".
        exact Or.inr (Or.inl h)
      -- Deeper.
      ·
        -- The third case of "before".
        exact Or.inr (Or.inr h)
    -- Every tombstone after was there before, so none hides the winner.
    ·
      -- Take a tombstone t that is there after the pass and covers k.
      intro t ht hc
      -- Turn "after" into "before".
      apply hnot t _ hc
      -- Each case of "after" is a case of "before".
      rcases ht with h | ⟨h, _, _⟩ | h
      -- Above.
      ·
        -- The first case of "before".
        exact Or.inl h
      -- In the window.
      ·
        -- The second case of "before".
        exact Or.inr (Or.inl h)
      -- Deeper.
      ·
        -- The third case of "before".
        exact Or.inr (Or.inr h)
  -- After to before. Unpack the win after.
  ·
    -- Name the parts: the winning put e0; p is visible at s; no visible put of k is newer; no
    -- visible tombstone hides it.
    intro ⟨⟨e0, hE0, hP0, hs0⟩, hps, hmax, hnot⟩
    -- The winning put was there before.
    have hB0 : Before A W B e0 := by
      -- Each case of "after" is a case of "before".
      rcases hE0 with h | ⟨h, _, _⟩ | h
      -- Above.
      ·
        -- The first case of "before".
        exact Or.inl h
      -- In the window.
      ·
        -- The second case of "before".
        exact Or.inr (Or.inl h)
      -- Deeper.
      ·
        -- The third case of "before".
        exact Or.inr (Or.inr h)
    -- A covering tombstone newer than the winner and visible at `s` is
    -- impossible before too: kept, the win after rules it out; dropped, it
    -- was retired, and then nothing under it survives, the winner included.
    have no_hider : ∀ t, Before A W B t → Covers t k → p < t.seq → t.seq ≤ s → False := by
      -- Take such a tombstone.
      intro t hBt hc hpt hts
      -- Was it kept by the pass?
      by_cases hkept : After A W B live (Retired W B live) t
      -- Kept: the win after says it does not hide the winner.
      ·
        -- The win after the pass, applied to this kept tombstone.
        exact hnot t hkept hc ⟨hpt, hts⟩
      -- Dropped: it was in the window, and shadowed or retired.
      ·
        -- It was in the window:
        have hWt : W t := by
          -- Above or deeper would have kept it.
          rcases hBt with h | h | h
          -- Above: kept, contradiction.
          ·
            -- "Above" is a case of "after", yet it was not kept.
            exact absurd (Or.inl h) hkept
          -- In the window: that is the claim.
          ·
            -- Exactly what is asked.
            exact h
          -- Deeper: kept, contradiction.
          ·
            -- "Deeper" is a case of "after", yet it was not kept.
            exact absurd (Or.inr (Or.inr h)) hkept
        -- Shadowed needs a put; `t` is a tombstone. So it was retired.
        have hret : Retired W B live t := by
          -- Ask whether it was retired.
          by_cases hr : Retired W B live t
          -- It was: done.
          ·
            -- Exactly what is asked.
            exact hr
          -- Not retired and not shadowed would have kept it.
          ·
            -- Aim for a contradiction.
            exfalso
            -- Show it was kept, contradicting `hkept`.
            apply hkept
            -- In the window, not shadowed, not retired.
            refine Or.inr (Or.inl ⟨hWt, ?_, hr⟩)
            -- Shadowed would make it a put, but it covers keys: a tombstone.
            intro ⟨hput, _⟩
            -- `rt = put` is false.
            rw [hc.1] at hput
            -- Distinct constructors.
            exact absurd hput (by decide)
        -- Nothing under a retired tombstone survives, yet the winner does.
        exact retired_leaves_nothing_under hA hret hc hE0 hP0 (by omega)
    -- Four clauses to show before.
    refine ⟨⟨e0, hB0, hP0, hs0⟩, hps, ?_, ?_⟩
    -- No visible put of `k` before is newer than the winner.
    ·
      -- Take a visible put e of k that was there before.
      intro e hBe hP hes
      -- Suppose it were newer.
      by_cases hle : e.seq ≤ p
      -- It is not: done.
      ·
        -- Exactly what is asked.
        exact hle
      -- It is: was it kept?
      ·
        -- Aim for a contradiction.
        exfalso
        -- Kept or dropped.
        by_cases hkept : After A W B live (Retired W B live) e
        -- Kept: the win after says it is no newer.
        ·
          -- The win after the pass says e is no newer than the winner, against the assumption.
          exact hle (hmax e hkept hP hes)
        -- Dropped: it was in the window and shadowed (retired needs a tombstone).
        ·
          -- It was in the window:
          have hWe : W e := by
            -- Above or deeper would have kept it.
            rcases hBe with h | h | h
            -- Above: kept, contradiction.
            ·
              -- "Above" is a case of "after", yet it was not kept.
              exact absurd (Or.inl h) hkept
            -- In the window: that is the claim.
            ·
              -- Exactly what is asked.
              exact h
            -- Deeper: kept, contradiction.
            ·
              -- "Deeper" is a case of "after", yet it was not kept.
              exact absurd (Or.inr (Or.inr h)) hkept
          -- Shadowed, since it was dropped and is no tombstone.
          have hsh : Shadowed W live e := by
            -- Ask whether it was shadowed.
            by_cases h : Shadowed W live e
            -- It was: done.
            ·
              -- Exactly what is asked.
              exact h
            -- Not shadowed and not retired would have kept it.
            ·
              -- Aim for a contradiction.
              exfalso
              -- Show it was kept, contradicting `hkept`.
              apply hkept
              -- In the window, not shadowed, not retired (a put).
              refine Or.inr (Or.inl ⟨hWe, h, ?_⟩)
              -- Retired would make it a tombstone.
              intro ⟨_, hrt, _⟩
              -- But it is a put.
              rw [hP.1] at hrt
              -- Distinct constructors.
              exact absurd hrt (by decide)
          -- The shadowing tombstone covers `k`, is newer than `e`, so newer
          -- than the winner, and visible at `s`.
          obtain ⟨_, t, hWt, hc, het, hstripe⟩ := hsh
          -- It is visible at `s`.
          have hts := visible e.seq t hWt hes hstripe
          -- It covers `k`, `e`'s key.
          rw [hP.2.1] at hc
          -- Such a tombstone is impossible.
          exact no_hider t (Or.inr (Or.inl hWt)) hc (by omega) hts
    -- No visible tombstone before hides the winner.
    ·
      -- Take a tombstone t that was there before, covers k, is newer than the winner and is visible
      -- at s.
      intro t hBt hc ⟨hpt, hts⟩
      -- That is `no_hider`.
      exact no_hider t hBt hc hpt hts

/-- **The value read is the same.** Retiring keeps the winner, so it keeps
what the read returns, absent included. -/
theorem retire_keeps_read_value {A W B : Entries} {live : Nat → Prop} {k s : Nat}
    -- Everything above the window is newer than everything in it,
    (hA : ∀ a w, A a → W w → w.seq < a.seq)
    -- and `s` is a live snapshot or the head.
    (hs : live s ∨ ∀ e, Before A W B e → e.seq ≤ s) :
    -- Claim: the read returns the same value before and after
    ∀ v, ReadsAs (Before A W B) k s v ↔ ReadsAs (After A W B live (Retired W B live)) k s v := by
  -- Take the value `v`.
  intro v
  -- The winners agree.
  have h := retire_keeps_reads (live := live) (k := k) hA hs
  -- Rewrite both sides through that agreement.
  unfold ReadsAs
  -- Either `v` wins on both sides, or nothing wins on either.
  constructor
  -- Before to after.
  ·
    -- Either v wins before, or v is "absent" and nothing wins before.
    rintro (hw | ⟨hv, hnone⟩)
    -- `v` wins before, so after.
    ·
      -- The winners agree, in the forward direction.
      exact Or.inl ((h v).1 hw)
    -- Nothing wins before, so nothing after.
    ·
      -- v is "absent"; anything that won after would have won before.
      exact Or.inr ⟨hv, fun p hp => hnone p ((h p).2 hp)⟩
  -- After to before.
  ·
    -- Either v wins after, or v is "absent" and nothing wins after.
    rintro (hw | ⟨hv, hnone⟩)
    -- `v` wins after, so before.
    ·
      -- The winners agree, in the backward direction.
      exact Or.inl ((h v).2 hw)
    -- Nothing wins after, so nothing before.
    ·
      -- v is "absent"; anything that won before would win after.
      exact Or.inr ⟨hv, fun p hp => hnone p ((h p).1 hp)⟩

/-! ## The RED cases -/

/-- A put of key `k` at sequence `s`. -/
def put (k s : Nat) : Entry := ⟨.put, k, k, s⟩

/-- A tombstone over `lo ..= hi` at sequence `s`. -/
def tomb (lo hi s : Nat) : Entry := ⟨.rt, lo, hi, s⟩

/-- **RED `IgnoreSnapshots`.** Retire whatever the snapshots: the window holds
a put of key 7 at sequence 1 and a tombstone over 5..9 at sequence 3, and a
snapshot at 2 is live. The put is not shadowed (the snapshot at 2 still
sees it), the tombstone is retired anyway, and the head (sequence 3) now
reads the put back where it read absent. -/
theorem ignore_snapshots_breaks_reads :
    -- Nothing above, nothing deeper.
    let A : Entries := fun _ => False
    -- Nothing deeper either.
    let B : Entries := fun _ => False
    -- The window: the put and the tombstone.
    let W : Entries := fun e => e = put 7 1 ∨ e = tomb 5 9 3
    -- One live snapshot, at 2.
    let live : Nat → Prop := fun l => l = 2
    -- The mutant rule: a tombstone of the window, snapshots ignored.
    let R : Entry → Prop := fun t => W t ∧ t.kind = .rt
    -- Before, nothing wins the head read of 7; after, sequence 1 does.
    (¬ Wins (Before A W B) 7 3 1) ∧ Wins (After A W B live R) 7 3 1 := by
  -- Unfold the `let`s into the goal.
  intro A B W live R
  -- Two claims.
  constructor
  -- Before: the tombstone at 3 covers 7 and is newer than the put.
  ·
    -- Suppose sequence 1 wins before; take its "no newer tombstone" clause.
    intro ⟨_, _, _, hnot⟩
    -- Apply the "no newer tombstone" clause to it.
    exact hnot (tomb 5 9 3) (Or.inr (Or.inl (Or.inr rfl)))
      -- It covers 7, is newer than 1, and is visible at 3
      ⟨rfl, by decide, by decide⟩ ⟨by decide, by decide⟩
  -- After: the put survives and the tombstone is gone.
  ·
    -- The winner is the put of 7 at 1, visible at 3; three parts are left: it survives, no put is
    -- newer, no tombstone hides it.
    refine ⟨⟨put 7 1, ?_, ⟨rfl, rfl, rfl⟩, rfl⟩, by decide, ?_, ?_⟩
    -- The put is in the window, not shadowed, not retired.
    ·
      -- The second case of "after"; "not shadowed" and "not retired" are left.
      refine Or.inr (Or.inl ⟨Or.inl rfl, ?_, ?_⟩)
      -- Shadowed would need the tombstone at 3 below the snapshot at 2.
      ·
        -- Suppose a tombstone t of the window shadows it.
        intro ⟨_, t, hWt, _, hlt, hstripe⟩
        -- The snapshot at 2 is live and at or above the put.
        have := hstripe 2 rfl (by decide)
        -- The window's members: the put itself, or the tombstone.
        rcases hWt with rfl | rfl
        -- The put is not newer than itself.
        ·
          -- "1 < 1" is false.
          exact absurd hlt (by decide)
        -- The tombstone at 3 is not at or below 2.
        ·
          -- "3 ≤ 2" is false.
          exact absurd this (by decide)
      -- Retired would make the put a tombstone.
      ·
        -- Suppose the put is retired; then its kind would be "tombstone".
        intro ⟨_, hk⟩
        -- `put = rt` is false.
        exact absurd hk (by decide)
    -- The only put after is the put itself.
    ·
      -- Take a put e of 7 that is there after the pass.
      intro e he hP _
      -- Find it among the survivors.
      rcases he with h | ⟨hW, _, _⟩ | h
      -- Nothing above.
      ·
        -- "Above" is empty.
        exact absurd h id
      -- In the window: the put or the tombstone.
      ·
        -- Split on which.
        rcases hW with rfl | rfl
        -- The put: sequence 1, no newer.
        ·
          -- 1 ≤ 1.
          exact Nat.le_refl 1
        -- The tombstone is not a put of 7: its kind is `rt`.
        ·
          -- "rt = put" is false.
          exact absurd hP.1 (by decide)
      -- Nothing deeper.
      ·
        -- "Deeper" is empty.
        exact absurd h id
    -- No tombstone survives.
    ·
      -- Take a tombstone t that is there after the pass and covers 7.
      intro t ht hc
      -- Find it among the survivors.
      rcases ht with h | ⟨hW, _, hR⟩ | h
      -- Nothing above.
      ·
        -- "Above" is empty.
        exact absurd h id
      -- In the window: the put or the tombstone.
      ·
        -- Split on which.
        rcases hW with rfl | rfl
        -- The put covers nothing: its kind is `put`.
        ·
          -- "put = rt" is false.
          exact absurd hc.1 (by decide)
        -- The tombstone was retired by the mutant rule.
        ·
          -- It is in the window and a tombstone, so the mutant rule retired it: it cannot have
          -- survived.
          exact absurd ⟨Or.inr rfl, rfl⟩ hR
      -- Nothing deeper.
      ·
        -- "Deeper" is empty.
        exact absurd h id

/-- **RED `IgnoreDeeper`.** Retire with the snapshots clear but a deeper run
still holding a put of the range: the window holds a tombstone over 5..9 at
sequence 2, a deeper run a put of 7 at sequence 1, no snapshot is live.
The tombstone is retired, and the head (sequence 2) reads the deeper put
back where it read absent. -/
theorem ignore_deeper_breaks_reads :
    -- Nothing above.
    let A : Entries := fun _ => False
    -- The window: the tombstone.
    let W : Entries := fun e => e = tomb 5 9 2
    -- Deeper: the put.
    let B : Entries := fun e => e = put 7 1
    -- No live snapshot.
    let live : Nat → Prop := fun _ => False
    -- The mutant rule: snapshots clear, deeper runs ignored.
    let R : Entry → Prop := fun t => W t ∧ t.kind = .rt ∧ ∀ l, live l → t.seq ≤ l
    -- Before, nothing wins the head read of 7; after, sequence 1 does.
    (¬ Wins (Before A W B) 7 2 1) ∧ Wins (After A W B live R) 7 2 1 := by
  -- Unfold the `let`s into the goal.
  intro A W B live R
  -- Two claims.
  constructor
  -- Before: the tombstone at 2 covers 7 and is newer than the put.
  ·
    -- Suppose sequence 1 wins before; take its "no newer tombstone" clause.
    intro ⟨_, _, _, hnot⟩
    -- Apply the "no newer tombstone" clause to it.
    exact hnot (tomb 5 9 2) (Or.inr (Or.inl rfl)) ⟨rfl, by decide, by decide⟩
      -- It is newer than 1 and visible at 2
      ⟨by decide, by decide⟩
  -- After: the deeper put survives and the tombstone is gone.
  ·
    -- The winner is the deeper put of 7 at 1, visible at 2; two parts are left: no put is newer, no
    -- tombstone hides it.
    refine ⟨⟨put 7 1, Or.inr (Or.inr rfl), ⟨rfl, rfl, rfl⟩, rfl⟩, by decide, ?_, ?_⟩
    -- The only put after is the deeper one.
    ·
      -- Take a put e of 7 that is there after the pass.
      intro e he hP _
      -- Find it among the survivors.
      rcases he with h | ⟨hW, _, _⟩ | h
      -- Nothing above.
      ·
        -- "Above" is empty.
        exact absurd h id
      -- The window holds only the tombstone, which is no put.
      ·
        -- Replace e by the window's only entry, the tombstone.
        subst hW
        -- Its kind is `rt`.
        exact absurd hP.1 (by decide)
      -- The deeper put: sequence 1.
      ·
        -- Replace e by the deeper put.
        subst h
        -- No newer than itself.
        exact Nat.le_refl 1
    -- No tombstone survives.
    ·
      -- Take a tombstone t that is there after the pass and covers 7.
      intro t ht hc
      -- Find it among the survivors.
      rcases ht with h | ⟨hW, _, hR⟩ | h
      -- Nothing above.
      ·
        -- "Above" is empty.
        exact absurd h id
      -- The tombstone was retired by the mutant rule.
      ·
        -- Replace t by the window's tombstone.
        subst hW
        -- It is in the window, a tombstone, and no snapshot is live.
        exact absurd ⟨rfl, rfl, fun _ h => absurd h id⟩ hR
      -- The deeper put covers nothing.
      ·
        -- Replace t by the deeper put.
        subst h
        -- Its kind is `put`.
        exact absurd hc.1 (by decide)

-- End of the namespace opened at the top.
end Regolith.TombstoneRetirement
