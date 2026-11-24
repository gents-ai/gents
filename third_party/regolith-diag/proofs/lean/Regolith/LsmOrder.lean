/-!
# LsmOrder: the read order of the LSM tree, the L0 closure rule, flush order, ingest placement, a merge inside L0, overlap demotion, binary search

## The story, for a reader who has never seen the code

The database keeps its data in layers: a table in memory (the memtable),
then files in "level 0", newest first, then deeper levels. Nothing is
overwritten in place, so a key written three times has three versions,
possibly in three layers. A read does not look everywhere and compare. It
asks the layers in order, newest first, and stops at the first one that
holds the key (`RegolithEngine::lookup_in_view`, src/engine/mod.rs).

Tiny example of what can go wrong: key 5 was written at sequence 2 (now in
an old level-0 file) and again at sequence 4 (now in a newer level-0 file).
If a compaction moves only the newer file down to level 1, the next read of
key 5 meets the old file first and answers the version of sequence 2: the
database went back in time.

What is proved here: as long as every layer is "newer" than every layer
read after it, a read returns the newest version its snapshot may see; and
each step that moves data between layers (a write, a flush, a compaction
of a well-chosen set of files, an ingest, a merge of L0 files that lie next
to one another, a demotion) keeps that order.
The bad ways of doing each step are shown as small counterexamples.

## Which model this backs

This file backs the TLA+ model `proofs/tla/LsmOrder.tla`, invariant
`ReadNewest` (configurations `MC_LsmOrder_Green` and
`MC_LsmOrder_Red_Intersect`). Defect E1: `compact_range` at L0 must pick a
closed input set (`l0_inputs`, src/engine/compaction.rs). The same model's
flush order, ingest placement, overlap demotion (E14, D13) and binary
search configurations (`MC_LsmOrder_*_Flush*`, `*_Ingest*`, `*_Demote*`,
`*_NoDemotion`, and the invariant `BisectIsScan`) are backed by items 6 to
9 below, and its universal merge configurations
(`MC_LsmOrder_Green_L0Merge`, `MC_LsmOrder_Red_L0MergeGap`, defect E38) by
item 10. `BatchRead.tla` (one view per batch read, E6) is backed by
`Regolith/BatchView.lean`, which builds on the reads defined here.

TLC checks the model for a few keys and versions. The theorems here hold
for every number of keys, versions and L0 files.

What is proved, in plain words:

1. `read_newest`: a read asks the sources in read order (the memtable, then
   the L0 files newest first, then L1) and the first source that holds a
   version of the key the snapshot may see answers. That answer is the
   newest such version anywhere, provided every source is newer than every
   source after it (`Ordered`).
2. `put_ordered` and `flush_ordered`: a write and a flush keep the sources
   ordered. `flush_keeps_sources_disjoint`: a flush that installs its table
   and retires its memtable in one step leaves no version in two sources
   (`LsmOrder.tla`, invariant `NoDoubleSource`);
   `split_install_holds_a_memtable_twice` and
   `split_install_reads_an_operand_twice` are the RED case (defect E31,
   configuration `MC_LsmOrder_Red_SplitRetire`) of publishing the table
   before the memtable is retired.
3. `compact_closed_preserves_order` and `compact_closed_preserves_reads`:
   moving a closed set of L0 files into L1 keeps the sources ordered and
   changes no read.
4. `pickL0_closed`, `pickL0_covers` and `compact_range_reads_newest`: the
   closure picker returns a closed set that contains every L0 file meeting
   the range, so `compact_range` changes no read.
5. `intersect_only_breaks_reads`: picking only the L0 files that meet the
   range can make a read return an older version. This is the RED
   configuration, as a concrete counterexample.
6. `install_oldest_same_order`, `install_oldest_all` and
   `install_oldest_all_reads`: installing the oldest frozen memtables, one by
   one, as the newest L0 files (every flush off the commit path, E9,
   whichever thread runs it) leaves the read order and every read as it was;
   `install_newer_breaks_reads` is the RED case of installing a newer one
   first.
7. `ingest_ordered`: an ingested file, newer than everything, placed where
   no source read before it shares a key, keeps the sources ordered;
   `ingest_below_holder_breaks_reads` is the RED case.
8. `demote_reads_newest`: demoting every level from L1 down to an
   overlapping one to the end of L0, in age order, keeps the read order and
   every read; `noShared_level_ordered` says the overlapping level's tables
   are ordered in any order; `demote_level_only_breaks_reads` is the RED
   case of demoting the overlapping level alone.
9. `bsearch_eq_scan` and `level_read_bsearch`: on a level in key order
   whose tables do not overlap, binary search finds the table a linear scan
   finds, and reads what a linear read of the level reads.
10. `merge_window_ordered` and `merge_window_reads_newest`: merging L0
   files that lie next to one another into one file, put where they were,
   keeps the sources ordered and changes no read;
   `merge_gapped_breaks_reads` is the RED case (defect E38) of a merge that
   leaves out a file lying between the files it takes.

How to read the Lean: `def` defines a function or a property, `theorem`
states a fact and its proof follows `:= by`. Lines starting with `--` are
comments. Inside a proof, each step is commented with what it establishes.
-/

-- Everything below is named Regolith.LsmOrder.<name>.
namespace Regolith.LsmOrder

/-! ## The objects -/

/-
Keys and sequence numbers are both natural numbers (`Nat`).

* A user key is a `Nat`. The engine compares user keys bytewise; only their
  order matters here, so a natural number stands for one. Variables named
  `k`, `lo`, `hi` and lists named `hull` hold keys.
* A sequence number is a `Nat`. Every write takes a fresh, larger one, so a
  larger sequence means a newer version. Variables named `s`, `t`, `v`, `w`
  and `snap` hold sequences; `snap` is the snapshot a read is taken at.

They are kept as plain `Nat` rather than named aliases so that Lean's
arithmetic decision procedure (`omega`) sees them as numbers.
-/

/-- One version of a key: the pair `(key, sequence it was written at)`. -/
abbrev Version := Nat × Nat

/-- A source a read consults: the memtable, one L0 file, or the L1 run.
Here it is the list of versions it holds; the order inside it does not
matter to anything below. -/
abbrev Source := List Version

/-! ## One source's answer -/

/-- `newestIn src k snap` is one source's answer to a point read of key `k`
at snapshot `snap`: the sequence of the newest version of `k` in `src` whose
sequence is at most `snap`, or `none` when `src` holds no such version. -/
def newestIn : Source → Nat → Nat → Option Nat
  -- An empty source holds nothing.
  | [], _, _ => none
  -- A first version `(k', s)`, followed by the versions `rest`.
  | (k', s) :: rest, k, snap =>
    -- Is the first version one of `k`'s, and may the snapshot see it?
    if k' = k ∧ s ≤ snap then
      -- Yes: the answer is the larger of `s` and whatever `rest` answers.
      some (match newestIn rest k snap with
        | none => s
        | some w => max s w)
    -- Otherwise:
    else
      -- No: the answer is whatever `rest` answers.
      newestIn rest k snap

/-- A source answers `none` exactly when every version of `k` it holds is
newer than the snapshot, so the snapshot may see none of them. -/
theorem newestIn_eq_none {src : Source} {k : Nat} {snap : Nat} :
    -- The statement: the answer is `none` exactly when every version of k in the source has a
    -- sequence above the snapshot.
    newestIn src k snap = none ↔ ∀ s, (k, s) ∈ src → snap < s := by
  -- By induction on the list of versions.
  induction src with
  -- Case: an empty source.
  | nil =>
    -- An empty source answers `none` and holds no version: both sides hold.
    simp [newestIn]
  -- Case: a first version hd, then the versions rest, for which the claim (ih) holds.
  | cons hd rest ih =>
    -- Name the first version `(k', s)`.
    obtain ⟨k', s⟩ := hd
    -- Split on whether the first version is one of k's that the snapshot may see.
    by_cases h : k' = k ∧ s ≤ snap
    -- It is visible. The answer is `some`, so the left side is false; and that version is a visible
    -- one of `k`, so the right side is false too.
    ·
      -- Unfold the answer one step and take the "visible" branch.
      rw [newestIn, ite_eq_left h]
      -- Take the two facts apart: the key is k, and s is at most the snapshot.
      obtain ⟨hk, hs⟩ := h
      -- Replace k' by k.
      subst k'
      -- Two directions to show.
      constructor
      -- Left to right: suppose the answer is `none`.
      · intro hc
        -- But it is `some`: impossible.
        cases hc
      -- Right to left: suppose every version of k is above the snapshot.
      · intro hall
        -- Then the first version, (k, s), is above the snapshot.
        have := hall s List.mem_cons_self
        -- But s is at most the snapshot: arithmetic says no.
        omega
    -- The first version is not visible, so the answer is `rest`'s.
    ·
      -- Unfold one step, take the "not visible" branch, and use the claim for the rest.
      rw [newestIn, ite_eq_right h, ih]
      -- Two directions to show.
      constructor
      -- Left to right: every version of `k` in `rest` is too new; the first one is either another
      -- key's or too new as well.
      ·
        -- Take a version (k, t) of the whole source.
        intro hr t ht
        -- It is the first version, or it is in the rest.
        rcases List.mem_cons.mp ht with he | hm
        -- `(k, t)` is the first version `(k', s)`.
        ·
          -- Then k' is k and s is t.
          simp only [Prod.mk.injEq] at he
          -- Replace them.
          obtain ⟨rfl, rfl⟩ := he
          -- The first version is not visible although its key is k: so its sequence is above the
          -- snapshot.
          omega
        -- In the rest: too new, as assumed.
        · exact hr t hm
      -- Right to left: dropping the first version keeps the statement for `rest`.
      ·
        -- Take a version (k, t) of the rest.
        intro hall t ht
        -- It is also a version of the whole source, so it is too new.
        exact hall t (List.mem_cons_of_mem _ ht)

/-- When a source answers `some v`, it holds the version `(k, v)`, the
snapshot may see it (`v ≤ snap`), and every other version of `k` the
snapshot may see in that source is no newer. -/
theorem newestIn_eq_some {src : Source} {k : Nat} {snap v : Nat}
    -- The source answered `some v`.
    (h : newestIn src k snap = some v) :
    -- The statement: the source holds (k, v); v is at most the snapshot; and every version of k at
    -- or below the snapshot is at or below v.
    (k, v) ∈ src ∧ v ≤ snap ∧ ∀ s, (k, s) ∈ src → s ≤ snap → s ≤ v := by
  -- By induction on the list of versions, for every answer `v`.
  induction src generalizing v with
  -- Case: an empty source.
  | nil =>
    -- An empty source answers `none`, never `some v`.
    simp [newestIn] at h
  -- Case: a first version, then the rest, for which the claim (ih) holds.
  | cons hd rest ih =>
    -- Name the first version (k', s).
    obtain ⟨k', s⟩ := hd
    -- Split on whether it is one of k's that the snapshot may see.
    by_cases hv : k' = k ∧ s ≤ snap
    -- The first version `(k, s)` is visible.
    ·
      -- Unfold the answer one step and take the "visible" branch.
      rw [newestIn, ite_eq_left hv] at h
      -- Take the two facts apart: the key is k, and s is at most the snapshot.
      obtain ⟨hk, hs⟩ := hv
      -- Replace k' by k.
      subst k'
      -- What does `rest` answer?
      cases hr : newestIn rest k snap with
      -- Case: the rest answers nothing.
      | none =>
        -- `rest` holds nothing visible, so the answer is `s` itself.
        rw [hr] at h
        -- The answer is then `some s`, so v is s.
        simp only [Option.some.injEq] at h
        -- Replace v by s.
        subst h
        -- (k, s) is the first version, and s is at most the snapshot; the third part is left.
        refine ⟨List.mem_cons_self, hs, ?_⟩
        -- Take a version (k, t) of the source at or below the snapshot.
        intro t ht hts
        -- It is the first version, or it is in the rest.
        rcases List.mem_cons.mp ht with he | hm
        -- The first version: `t = s`.
        ·
          -- Its sequence is s.
          simp only [Prod.mk.injEq] at he
          -- So t is at most s.
          omega
        -- A version in `rest`: impossible, `rest` holds nothing visible.
        ·
          -- Every version of k in the rest is above the snapshot.
          have := (newestIn_eq_none.mp hr) t hm
          -- But t is at most the snapshot: arithmetic says no.
          omega
      -- Case: the rest answers w.
      | some w =>
        -- `rest` answers `w`, so the answer is `max s w`.
        rw [hr] at h
        -- The answer is then `some (max s w)`, so v is the larger of s and w.
        simp only [Option.some.injEq] at h
        -- The claim for the rest: it holds (k, w), w is at most the snapshot, and w tops the rest's
        -- visible versions.
        obtain ⟨hw, hwle, hwmax⟩ := ih hr
        -- Three parts: (k, v) is held (next); v is at most the snapshot (arithmetic); v tops
        -- everything (after).
        refine ⟨?_, by omega, ?_⟩
        -- `max s w` is one of the two, and both are held.
        ·
          -- Split on which of s and w is larger.
          by_cases hsw : w ≤ s
          -- w is at most s: then v is s,
          · have : v = s := by omega
            -- replace v by s,
            subst this
            -- and (k, s) is the first version.
            exact List.mem_cons_self
          -- s is below w: then v is w,
          · have : v = w := by omega
            -- replace v by w,
            subst this
            -- and (k, w) is in the rest.
            exact List.mem_cons_of_mem _ hw
        -- Every visible version is at most `s` or at most `w`.
        ·
          -- Take a version (k, t) at or below the snapshot.
          intro t ht hts
          -- It is the first version, or it is in the rest.
          rcases List.mem_cons.mp ht with he | hm
          -- The first: t is s,
          · simp only [Prod.mk.injEq] at he
            -- and s is at most v.
            omega
          -- In the rest: t is at most w,
          · have := hwmax t hm hts
            -- and w is at most v.
            omega
    -- The first version is not visible: the answer is `rest`'s.
    ·
      -- Unfold one step and take the "not visible" branch.
      rw [newestIn, ite_eq_right hv] at h
      -- The claim for the rest: it holds (k, v), v is at most the snapshot, and v tops the rest's
      -- visible versions.
      obtain ⟨hw, hwle, hwmax⟩ := ih h
      -- (k, v) is in the whole source too, and v is at most the snapshot; the third part is left.
      refine ⟨List.mem_cons_of_mem _ hw, hwle, ?_⟩
      -- Take a version (k, t) at or below the snapshot.
      intro t ht hts
      -- It is the first version, or it is in the rest.
      rcases List.mem_cons.mp ht with he | hm
      -- `(k, t)` is the first version, which is visible: a contradiction.
      ·
        -- Then k' is k and s is t.
        simp only [Prod.mk.injEq] at he
        -- Replace them.
        obtain ⟨rfl, rfl⟩ := he
        -- The first version would be visible, against this case.
        exact absurd ⟨rfl, hts⟩ hv
      -- In the rest: t is at most v, by the claim for the rest.
      · exact hwmax t hm hts

/-- Versions newer than the snapshot change nothing a snapshot reads:
putting versions that are all newer than `snap` in front of a source
leaves its answer at `snap` unchanged. This is what makes a write after a
snapshot invisible to it, here and in `Publication.lean`. -/
theorem newestIn_append_invisible {extra src : Source} {k : Nat} {snap : Nat}
    -- Every added version is newer than the snapshot.
    (hnew : ∀ e ∈ extra, snap < e.2) :
    -- The statement: the answer with the extra versions in front equals the answer without them.
    newestIn (extra ++ src) k snap = newestIn src k snap := by
  -- By induction on the added versions.
  induction extra with
  -- No added version: the same source.
  | nil => rfl
  -- A first added version, then the rest, for which the claim (ih) holds.
  | cons hd rest ih =>
    -- Name the first added version (k', s).
    obtain ⟨k', s⟩ := hd
    -- The first added version is too new to be visible.
    have hs : snap < s := hnew (k', s) List.mem_cons_self
    -- So it is not "k's and at or below the snapshot".
    have hnot : ¬ (k' = k ∧ s ≤ snap) := by omega
    -- So the answer is that of the rest, which the induction covers.
    rw [List.cons_append, newestIn, ite_eq_right hnot]
    -- The rest of the added versions are all too new as well.
    exact ih (fun e he => hnew e (List.mem_cons_of_mem _ he))

/-! ## The read order -/

/-- `Holds srcs v`: some source in `srcs` holds the version `v`. -/
def Holds (srcs : List Source) (v : Version) : Prop := ∃ src ∈ srcs, v ∈ src

/-- `IsNewest srcs k snap v`: `v` is the sequence of the newest version of
`k` that a snapshot at `snap` may see, across all of `srcs`. This is what a
correct read returns. -/
def IsNewest (srcs : List Source) (k : Nat) (snap v : Nat) : Prop :=
  -- Some source holds `(k, v)` ...
  Holds srcs (k, v) ∧
  -- ... the snapshot may see it ...
  v ≤ snap ∧
  -- ... and every version of `k` the snapshot may see is no newer.
  ∀ s, Holds srcs (k, s) → s ≤ snap → s ≤ v

/-- `read srcs k snap` is the engine's point read of `k` at snapshot
`snap`. It asks the sources in read order, and the first one holding a
version the snapshot may see answers with its newest such version. `none`
means no source holds one: the key reads as absent. -/
def read : List Source → Nat → Nat → Option Nat
  -- No source left: the key is absent at this snapshot.
  | [], _, _ => none
  -- Ask the next source; stop at its answer if it has one.
  | src :: rest, k, snap =>
    -- Ask the first source.
    match newestIn src k snap with
    -- It has an answer: that is the read's answer.
    | some v => some v
    -- It has none: ask the remaining sources.
    | none => read rest k snap

/-- `Newer a b`: for every key, every version `a` holds is newer than every
version of the same key `b` holds. The memtable is newer than every L0
file, a newer L0 file is newer than an older one, and every L0 file is
newer than L1. -/
def Newer (a b : Source) : Prop := ∀ k s t, (k, s) ∈ a → (k, t) ∈ b → t < s

/-- `Ordered srcs`: every source is `Newer` than every source after it in
read order. `List.Pairwise R l` means `R x y` for every `x` that comes
before `y` in `l`. -/
def Ordered (srcs : List Source) : Prop := srcs.Pairwise Newer

/-- `newerB a b` computes `Newer a b`: for every version in `a` and every
version in `b`, either the keys differ or `b`'s version is older. It lets
Lean check `Ordered` on concrete examples by evaluation. -/
def newerB (a b : Source) : Bool :=
  -- For every version x of a and every version y of b: the keys differ, or y's sequence is below
  -- x's.
  a.all fun x => b.all fun y => x.1 != y.1 || decide (y.2 < x.2)

/-- The computation `newerB` agrees with the definition `Newer`. -/
theorem newer_iff_newerB {a b : Source} : Newer a b ↔ newerB a b = true := by
  -- Turn the computation into the statement "for every `x` in `a` and `y`
  -- in `b`, the keys differ or `y.2 < x.2`".
  simp only [Newer, newerB, List.all_eq_true, Bool.or_eq_true, bne_iff_ne, ne_eq,
    decide_eq_true_eq]
  -- Two directions to show.
  constructor
  -- From `Newer`: two versions with the same key are ordered.
  ·
    -- Take a version x of a and a version y of b.
    intro h x hx y hy
    -- Split on whether their keys are equal.
    by_cases hk : x.1 = y.1
    -- Equal: show the second alternative, "y is older".
    · right
      -- Name x (k, s).
      obtain ⟨k, s⟩ := x
      -- Name y (k', t).
      obtain ⟨k', t⟩ := y
      -- The two keys are equal.
      simp only at hk
      -- Replace k' by k.
      subst hk
      -- `Newer` gives t < s.
      exact h k s t hx hy
    -- Different: show the first alternative.
    · left
      -- The keys differ.
      exact hk
  -- To `Newer`: the keys of `(k, s)` and `(k, t)` are equal, so the second alternative must hold.
  ·
    -- Take versions (k, s) of a and (k, t) of b.
    intro h k s t hs ht
    -- The computed statement says: the keys differ, or t < s.
    rcases h (k, s) hs (k, t) ht with hne | hlt
    -- The keys are the same key: the first alternative is impossible.
    · exact absurd rfl hne
    -- So t < s.
    · exact hlt

/-- `Newer` is decidable, through `newerB`. -/
instance : DecidableRel Newer := fun a b =>
  -- Decide the computed form, and carry the answer over by the lemma above.
  decidable_of_iff (newerB a b = true) newer_iff_newerB.symm

/-- A read answers `none` exactly when no source holds a version of `k`
the snapshot may see. This needs no ordering. -/
theorem read_eq_none {srcs : List Source} {k : Nat} {snap : Nat} :
    -- The statement: the read is `none` exactly when every version of k held by any source is above
    -- the snapshot.
    read srcs k snap = none ↔ ∀ s, Holds srcs (k, s) → snap < s := by
  -- Go through the sources one at a time.
  induction srcs with
  -- Case: no sources.
  | nil =>
    -- No sources: the read is `none` and nothing is held.
    simp [read, Holds]
  -- Case: a first source, then the rest, for which the claim (ih) holds.
  | cons src rest ih =>
    -- Split on the first source's answer.
    cases hn : newestIn src k snap with
    -- Case: the first source answers v.
    | some v =>
      -- It holds a visible version, so the read is `some` and the right
      -- side fails at that version.
      simp only [read, hn]
      -- That source holds (k, v), and v is at most the snapshot.
      obtain ⟨hv, hvle, -⟩ := newestIn_eq_some hn
      -- Two directions to show.
      constructor
      -- `some v = none` is impossible.
      ·
        -- Suppose the read were `none`.
        intro hc
        -- It is `some v`: impossible.
        cases hc
      -- `(k, v)` is held and visible, against the right side.
      ·
        -- Suppose every held version of k were above the snapshot.
        intro hall
        -- Then v would be above the snapshot.
        have := hall v ⟨src, List.mem_cons_self, hv⟩
        -- But v is at most the snapshot: arithmetic says no.
        omega
    -- Case: the first source answers nothing.
    | none =>
      -- It holds nothing visible: the read is `rest`'s.
      simp only [read, hn]
      -- Use the claim for the remaining sources.
      rw [ih]
      -- Two directions to show.
      constructor
      -- Nothing visible in `rest`, nor in the first source.
      ·
        -- Take a version (k, t) held by some source s'.
        intro hr t ⟨s', hs', ht⟩
        -- That source is the first one, or one of the rest.
        rcases List.mem_cons.mp hs' with rfl | hm
        -- The first: it holds nothing visible, so t is above the snapshot.
        · exact newestIn_eq_none.mp hn t ht
        -- One of the rest: t is above the snapshot, as assumed.
        · exact hr t ⟨s', hm, ht⟩
      -- Nothing visible anywhere means nothing visible in `rest`.
      ·
        -- Take a version (k, t) held by a source s' of the rest.
        intro hall t ⟨s', hs', ht⟩
        -- It is held by the whole list too, so t is above the snapshot.
        exact hall t ⟨s', List.mem_cons_of_mem _ hs', ht⟩

/-- **The read order returns the newest visible version.** On ordered
sources, a read that answers `some v` answers the newest version of `k` the
snapshot may see. -/
theorem read_some_newest {srcs : List Source} {k : Nat} {snap v : Nat}
    -- The sources are ordered: each newer than every one after it.
    (hord : Ordered srcs)
    -- The read answered `some v`.
    (h : read srcs k snap = some v) :
    -- The statement: v is the newest version of k the snapshot may see, across all the sources.
    IsNewest srcs k snap v := by
  -- Go through the sources one at a time.
  induction srcs with
  -- Case: no sources.
  | nil =>
    -- No sources answer `none`, never `some v`.
    simp [read] at h
  -- Case: a first source, then the rest, for which the claim (ih) holds.
  | cons src rest ih =>
    -- Ordered splits into: the first source is newer than each later one,
    -- and the later ones are ordered among themselves.
    unfold Ordered at hord
    -- "Pairwise" on a first source and the rest means: the first is newer than each of the rest,
    -- and the rest is pairwise.
    rw [List.pairwise_cons] at hord
    -- Name those two facts.
    obtain ⟨hhead, hrest⟩ := hord
    -- Split on the first source's answer.
    cases hn : newestIn src k snap with
    -- Case: it answers w.
    | some w =>
      -- The first source answered `w`, so the read is `w`.
      simp only [read, hn, Option.some.injEq] at h
      -- Replace v by w.
      subst h
      -- The first source holds (k, w), w is at most the snapshot, and w tops its visible versions
      -- of k.
      obtain ⟨hw, hwle, hwmax⟩ := newestIn_eq_some hn
      -- (k, w) is held, and w is at most the snapshot; left to show: w tops every visible version
      -- of k anywhere.
      refine ⟨⟨src, List.mem_cons_self, hw⟩, hwle, ?_⟩
      -- Take a version (k, s) at or below the snapshot, held by some source t.
      intro s ⟨t, ht, hs⟩ hsle
      -- That source is the first one, or one of the rest.
      rcases List.mem_cons.mp ht with rfl | hm
      -- Another version in the first source: no newer, by its answer.
      ·
        -- w tops the first source's visible versions.
        exact hwmax s hs hsle
      -- A version in a later source: older, since the first is newer.
      ·
        -- The first source is newer than that source, so s is below w.
        exact Nat.le_of_lt (hhead t hm k w s hw hs)
    -- Case: the first source answers nothing.
    | none =>
      -- The first source holds nothing visible: the read is `rest`'s.
      simp only [read, hn] at h
      -- The claim for the rest: some source t of it holds (k, v), v is at most the snapshot, and v
      -- tops the rest's visible versions.
      obtain ⟨⟨t, ht, hv⟩, hvle, hvmax⟩ := ih hrest h
      -- (k, v) is held by the whole list too, and v is at most the snapshot; left to show: v tops
      -- everything.
      refine ⟨⟨t, List.mem_cons_of_mem _ ht, hv⟩, hvle, ?_⟩
      -- Take a version (k, s) at or below the snapshot, held by some source t'.
      intro s ⟨t', ht', hs⟩ hsle
      -- That source is the first one, or one of the rest.
      rcases List.mem_cons.mp ht' with rfl | hm
      -- A visible version in the first source would contradict `none`.
      ·
        -- Every version of k in the first source is above the snapshot.
        have := newestIn_eq_none.mp hn s hs
        -- But s is at most the snapshot: arithmetic says no.
        omega
      -- One of the rest: s is at most v, by the claim for the rest.
      · exact hvmax s ⟨t', hm, hs⟩ hsle

/-- **`read_newest`.** On ordered sources a read answers `some v` exactly
when `v` is the newest version of `k` the snapshot may see. With
`read_eq_none` this pins the read down completely. -/
theorem read_newest {srcs : List Source} {k : Nat} {snap v : Nat}
    -- The sources are ordered.
    (hord : Ordered srcs) :
    -- The statement: the read answers `some v` exactly when v is the newest visible version.
    read srcs k snap = some v ↔ IsNewest srcs k snap v := by
  -- Two directions to show.
  constructor
  -- One direction is `read_some_newest`.
  ·
    -- A read that answers v answers the newest.
    exact read_some_newest hord
  -- The other direction: suppose v is the newest visible version.
  · intro hv
    -- The read is `none` or `some w`.
    cases hr : read srcs k snap with
    -- Case: the read answers nothing.
    | none =>
      -- `none` means nothing visible is held, but `(k, v)` is.
      have := read_eq_none.mp hr v hv.1
      -- v is at most the snapshot.
      have := hv.2.1
      -- But "nothing visible is held" puts v above the snapshot: arithmetic says no.
      omega
    -- Case: the read answers w.
    | some w =>
      -- `w` is the newest too, and the newest is unique: `v ≤ w ≤ v`.
      have hw := read_some_newest hord hr
      -- v tops every visible version, w included: w ≤ v.
      have h1 := hv.2.2 w hw.1 hw.2.1
      -- w tops every visible version, v included: v ≤ w.
      have h2 := hw.2.2 v hv.1 hv.2.1
      -- So w is v.
      have : w = v := by omega
      -- Replace w by v: the read answers v.
      rw [this]

/-- Two ordered read orders that hold the same versions answer every read
the same. This is how a compaction is shown to change no read: it only
moves versions between sources. -/
theorem read_eq_of_same_versions {a b : List Source}
    -- Both read orders are ordered.
    (ha : Ordered a) (hb : Ordered b)
    -- They hold exactly the same versions.
    (hsame : ∀ v, Holds a v ↔ Holds b v)
    -- (for any key and snapshot)
    (k : Nat) (snap : Nat) :
    -- The statement: the two read orders give the same read.
    read a k snap = read b k snap := by
  -- Split on what the first read order answers.
  cases hr : read a k snap with
  -- Case: nothing.
  | none =>
    -- Nothing visible in `a`, hence nothing visible in `b`.
    symm
    -- b holds nothing visible either, because every version b holds, a holds.
    exact read_eq_none.mpr fun s hs => read_eq_none.mp hr s ((hsame (k, s)).mpr hs)
  -- Case: v.
  | some v =>
    -- `v` is the newest in `a`; the same versions make it the newest in `b`.
    obtain ⟨hv, hvle, hvmax⟩ := read_some_newest ha hr
    -- Turn the goal round: b's read is `some v`.
    symm
    -- By `read_newest` for b, it is enough that v is the newest visible version in b:
    exact (read_newest hb).mpr
      -- b holds (k, v) because a does; v is at most the snapshot; and v tops b's visible versions
      -- because they are a's too.
      ⟨(hsame _).mp hv, hvle, fun s hs hsle => hvmax s ((hsame _).mpr hs) hsle⟩

/-! ## Writes and flushes keep the order -/

/-- A put of `(k, s)` into the memtable keeps the read order ordered, when
`s` is newer than every version already held: the engine hands out a
fresh, larger sequence for every write. -/
theorem put_ordered {mem : Source} {rest : List Source} {k : Nat} {s : Nat}
    -- The read order before the write is ordered.
    (hord : Ordered (mem :: rest))
    -- The new sequence is larger than every sequence already held.
    (hfresh : ∀ v, Holds (mem :: rest) v → v.2 < s) :
    -- The statement: with (k, s) added to the memtable, the read order is still ordered.
    Ordered (((k, s) :: mem) :: rest) := by
  -- Unfold "ordered" to "pairwise newer", in the assumption and the goal.
  unfold Ordered at *
  -- Split both into "the first source is newer than each later one" and "the later ones are
  -- pairwise newer".
  rw [List.pairwise_cons] at *
  -- Name the two facts of the assumption.
  obtain ⟨hhead, hrest⟩ := hord
  -- The later sources are untouched; left to show: the grown memtable is newer than each of them.
  refine ⟨?_, hrest⟩
  -- The grown memtable is newer than every later source.
  intro src hsrc k' s' t hs ht
  -- The memtable's version is the new one, or an old one.
  rcases List.mem_cons.mp hs with he | hm
  -- The new version: older versions are below `s` by freshness.
  ·
    -- Its key is k and its sequence is s.
    simp only [Prod.mk.injEq] at he
    -- Replace them.
    obtain ⟨rfl, rfl⟩ := he
    -- Every version already held is below s.
    exact hfresh (k', t) ⟨src, List.mem_cons_of_mem _ hsrc, ht⟩
  -- An old memtable version: newer than `src` as before.
  ·
    -- The old memtable was newer than src.
    exact hhead src hsrc k' s' t hm ht

/-- A flush turns the memtable into the newest L0 file and starts an empty
memtable. The order is kept: the empty memtable holds nothing, so it is
vacuously newer than everything. -/
theorem flush_ordered {mem : Source} {rest : List Source}
    -- The read order before the flush is ordered.
    (hord : Ordered (mem :: rest)) :
    -- The statement: an empty memtable in front of the old memtable and the rest is ordered.
    Ordered ([] :: mem :: rest) := by
  -- Unfold "ordered" to "pairwise newer".
  unfold Ordered at *
  -- The old read order is pairwise newer already; left to show: the empty memtable is newer than
  -- each source.
  refine List.Pairwise.cons ?_ hord
  -- The empty memtable holds no version, so there is nothing to compare.
  intro src _ k s t hs _
  -- The empty memtable holds no version: there is no case.
  cases hs

/-! ## A flush is one publication (E31)

The story. Before a flush writes a memtable out, the memtable is sealed: it
stops taking writes and joins the frozen memtables, which a read asks after
the active memtable and before L0. When the flush has written its table it
has two things to tell readers: "this table is now the newest L0 file" and
"this memtable is no longer frozen". The table holds exactly what the
memtable holds, version for version. If the flush says the first thing, and
only later the second, a reader that looks in between is handed the memtable
AND its table: every version twice.

A point read does not mind, because both copies agree. A read of a merged key
does: it gathers that key's operands from EVERY source and applies them all.
Example: merges "a" (sequence 1) and "b" (sequence 2) on key 1 sit in a frozen
memtable. The split flush publishes their table. A reader in the window finds
operands 1 and 2 in the memtable and 1 and 2 again in the table, and returns
"abab" where the answer is "ab".

The code says both things in one publication of the read view
(`VersionGuard::retire_with_version` in `src/engine/read_view.rs`), so the
window does not exist. The theorems below show that the one-step flush keeps
every version in one source only, for any number of memtables, files and
versions, and that the split one does not. The TLA+ configurations
`MC_LsmOrder_Green` and `MC_LsmOrder_Red_SplitRetire` check the same over
interleavings, as the invariant `NoDoubleSource`.

In the lists below `mem` is the active memtable, `imm` the frozen memtables
newer than the one being flushed, `f` the frozen memtable being flushed (the
oldest), `l0` the L0 files newest first, and `deeper` every level below. -/

/-- **`flush_same_sources`.** The oldest frozen memtable is the last source
read before L0; as the newest L0 file it is the first source read in L0.
That is the same place in the read order, so a one-step flush leaves the list
of sources exactly as it was. -/
theorem flush_same_sources (mem : Source) (imm l0 deeper : List Source) (f : Source) :
    -- Before: `f` is the oldest frozen memtable. After: `f` is the newest L0
    -- file. Same list.
    mem :: (imm ++ [f]) ++ l0 ++ deeper = mem :: imm ++ (f :: l0) ++ deeper := by
  -- Both sides are the same list once the appends are flattened.
  simp

/-- Two sources hold no version in common: nothing is in both `a` and `b`. -/
def NoSharedVersion (a b : Source) : Prop := ∀ v, v ∈ a → v ∈ b → False

/-- **No version in two sources.** Every source shares no version with any
source after it in the list, which covers every pair. This is what a read
that gathers from every source needs: each version turns up once. TLA+:
`NoDoubleSource`. -/
def NoDoubleSource (srcs : List Source) : Prop := srcs.Pairwise NoSharedVersion

/-- **`flush_keeps_sources_disjoint`.** If no version is in two sources
before a flush, none is after it, when the flush installs its table and
retires its memtable in one step: the oldest frozen memtable `f` becomes the
newest L0 file, and that is the very same list of sources. -/
theorem flush_keeps_sources_disjoint (mem : Source) (imm l0 deeper : List Source) (f : Source)
    -- Before the flush: `f` is the oldest frozen memtable, and no version is
    -- in two sources.
    (h : NoDoubleSource (mem :: (imm ++ [f]) ++ l0 ++ deeper)) :
    -- After it: `f` is the newest L0 file, and still none is.
    NoDoubleSource (mem :: imm ++ (f :: l0) ++ deeper) := by
  -- The list after is the list before (`flush_same_sources`), so rewrite the
  -- goal back into the "before" list.
  rw [← flush_same_sources]
  -- That is exactly what we were given.
  exact h

/-- **`split_install_holds_a_memtable_twice`.** The RED case, for every
size. A flush that publishes its table while its memtable `f` is still
frozen makes a view in which every version `v` of `f` is in two sources: the
frozen memtable and the new L0 file. So `NoDoubleSource` fails for that view,
whatever the other memtables, files and levels hold. -/
theorem split_install_holds_a_memtable_twice (mem : Source) (imm l0 deeper : List Source)
    -- The memtable being flushed, and one version.
    (f : Source) (v : Version)
    -- The flushed memtable holds at least this one version.
    (hv : v ∈ f) :
    -- The view between the two publications: `f` frozen AND `f` in L0.
    ¬ NoDoubleSource (mem :: (imm ++ [f]) ++ (f :: l0) ++ deeper) := by
  -- Suppose no version were in two sources of that view.
  intro h
  -- Regroup the list so the two copies of `f` stand side by side: first
  -- everything read before them, then `f`, `f`, then everything after.
  have hshape : mem :: (imm ++ [f]) ++ (f :: l0) ++ deeper
      = (mem :: imm) ++ (f :: f :: (l0 ++ deeper)) := by
    -- Both sides are the same list once the appends are flattened.
    simp
  -- Read the assumption on the regrouped list.
  rw [NoDoubleSource, hshape] at h
  -- A list is pairwise fine only if its second part is: keep that part,
  -- which starts with the two copies of `f`.
  have htail := (List.pairwise_append.mp h).2.1
  -- The first `f` must share no version with anything after it, and the
  -- second `f` comes right after it.
  have hff : NoSharedVersion f f := (List.pairwise_cons.mp htail).1 f (by simp)
  -- But `v` is in both copies: a contradiction.
  exact hff v hv hv

/-- What a read of key `k` gathers before it merges: the sequence of every
version of `k`, from every source in read order. An operand that turns up
twice here is applied twice. -/
def gathered (srcs : List Source) (k : Nat) : List Nat :=
  -- Lay the sources end to end, keep the versions of `k`, take their sequences.
  (srcs.flatten.filter fun v => v.1 == k).map Prod.snd

/-- **`together_gathers_the_same`.** A read gathers the same operands after a
one-step flush as before it, for every key: nothing is added and nothing is
lost. -/
theorem together_gathers_the_same (mem : Source) (imm l0 deeper : List Source) (f : Source)
    -- Any key.
    (k : Nat) :
    -- Gathered before the flush equals gathered after it.
    gathered (mem :: (imm ++ [f]) ++ l0 ++ deeper) k
      = gathered (mem :: imm ++ (f :: l0) ++ deeper) k := by
  -- The two source lists are the same list, so what is gathered is the same.
  rw [flush_same_sources]

/-- **`split_install_reads_an_operand_twice`.** The story above, in numbers.
Key 1 has two merge operands, at sequences 1 and 2, in one frozen memtable.
Before the flush a read gathers `[1, 2]`. After a one-step flush it still
gathers `[1, 2]`. In the split flush's window it gathers `[1, 2, 1, 2]`: each
operand twice, which is "abab" for "ab". -/
theorem split_install_reads_an_operand_twice :
    -- Before: an empty active memtable, the frozen one, nothing in L0.
    gathered ([] :: ([] ++ [[(1, 1), (1, 2)]]) ++ [] ++ []) 1 = [1, 2] ∧
    -- One-step flush: the frozen one is now the only L0 file.
    gathered ([] :: [] ++ ([(1, 1), (1, 2)] :: []) ++ []) 1 = [1, 2] ∧
    -- Split flush, in its window: still frozen, and already in L0.
    gathered ([] :: ([] ++ [[(1, 1), (1, 2)]]) ++ ([(1, 1), (1, 2)] :: []) ++ []) 1
      = [1, 2, 1, 2] := by
  -- Concrete lists: evaluate all three.
  decide

/-! ## Compaction of picked L0 files -/

/-- An L0 slot: one L0 file, and whether `compact_range` picked it as an
input. L0 is a list of slots, newest file first. -/
abbrev Slot := Source × Bool

/-- The keys a file holds. Its key range runs from the least to the
greatest of them. -/
def keysOf (f : Source) : List Nat := f.map Prod.fst

/-- Every key of every picked file. The picked set's key range, its hull,
runs from the least to the greatest of these. -/
def hullKeys (l0 : List Slot) : List Nat :=
  -- Take the picked slots, list each one's keys, and join the lists.
  ((l0.filter Prod.snd).map fun x => keysOf x.1).flatten

/-- `overlaps f hull`: the key range of file `f` meets the picked set's key
range. Two ranges `[a, b]` and `[c, d]` meet exactly when `a ≤ d` and
`c ≤ b`, so this asks for a key of `f` at or below some key of the hull,
and a key of the hull at or below some key of `f`. It is the interval test
`min f ≤ max hull ∧ min hull ≤ max f`, written so that an empty file or an
empty hull overlaps nothing. -/
def overlaps (f : Source) (hull : List Nat) : Bool :=
  -- Some key of f is at or below some key of the hull,
  (f.any fun e => hull.any fun d => decide (e.1 ≤ d)) &&
  -- and some key of the hull is at or below some key of f.
  (hull.any fun c => f.any fun e => decide (c ≤ e.1))

/-- A file holding a key that a picked file also holds overlaps the hull:
the shared key lies in both ranges. -/
theorem overlaps_of_shared {f : Source} {hull : List Nat} {k : Nat} {s : Nat}
    -- `f` holds a version of `k`.
    (hf : (k, s) ∈ f)
    -- `k` is a key of the picked set.
    (hk : k ∈ hull) :
    -- The statement: f overlaps the hull.
    overlaps f hull = true := by
  -- Unfold the test into its two "some key ... some key" halves.
  simp only [overlaps, Bool.and_eq_true, List.any_eq_true, decide_eq_true_eq]
  -- The shared key witnesses both comparisons, as `k ≤ k`.
  exact ⟨⟨(k, s), hf, k, hk, Nat.le_refl k⟩, ⟨k, hk, (k, s), hf, Nat.le_refl k⟩⟩

/-- A key of a picked file is a key of the hull. -/
theorem mem_hullKeys {l0 : List Slot} {x : Slot} {k : Nat} {s : Nat}
    -- `x` is an L0 slot ...
    (hx : x ∈ l0)
    -- ... that compaction picked ...
    (hpick : x.2 = true)
    -- ... and its file holds a version of `k`.
    (hk : (k, s) ∈ x.1) :
    -- The statement: k is one of the hull's keys.
    k ∈ hullKeys l0 := by
  -- Unfold: k is in the key list of some picked slot.
  simp only [hullKeys, keysOf, List.mem_flatten, List.mem_map, List.mem_filter]
  -- The list of `x`'s keys is one of the flattened lists, and holds `k`.
  exact ⟨_, ⟨x, ⟨hx, hpick⟩, rfl⟩, List.mem_map.mpr ⟨(k, s), hk, rfl⟩⟩

/-- The closure rule for one pair of slots, `newer` coming before `older`
in L0: if the newer file is picked and the older one is not, the older
file's key range must not meet the picked set's range `hull`. Otherwise
the older file could hold an older version of a key whose newer version
leaves for L1, and a read would find the older one first. -/
def ClosureRule (hull : List Nat) (newer older : Slot) : Prop :=
  -- If the newer slot is picked and the older one is not, the older file does not overlap the hull.
  newer.2 = true → older.2 = false → overlaps older.1 hull = false

/-- `Closed l0`: the picked set obeys the closure rule for every pair of L0
slots, newer before older. -/
def Closed (l0 : List Slot) : Prop := l0.Pairwise (ClosureRule (hullKeys l0))

/-- The L0 files a compaction leaves in place, in their L0 order. -/
def remaining (l0 : List Slot) : List Source := (l0.filter fun x => !x.2).map Prod.fst

/-- Every version of every picked file: what the compaction merges into L1. -/
def moved (l0 : List Slot) : Source := ((l0.filter Prod.snd).map Prod.fst).flatten

/-- The read order before the compaction: the memtable, the L0 files newest
first, then L1. -/
def before (mem : Source) (l0 : List Slot) (l1 : Source) : List Source :=
  -- The memtable, then every L0 file, then L1.
  mem :: (l0.map Prod.fst ++ [l1])

/-- The read order after the picked files are compacted into L1.

L1 is one sorted run. Merging the picked files with the L1 files they
overlap and writing the result back leaves L1 holding its old versions
plus the picked ones; which L1 files were rewritten is invisible to a
read. A compaction may also drop a version no registered snapshot can see;
the reads here are at snapshots whose versions are kept, so the model
keeps every version. -/
def after (mem : Source) (l0 : List Slot) (l1 : Source) : List Source :=
  -- The memtable, then the L0 files left in place, then L1 with the moved versions added.
  mem :: (remaining l0 ++ [l1 ++ moved l0])

/-- A remaining file is the file of an unpicked L0 slot. -/
theorem mem_remaining {l0 : List Slot} {f : Source} :
    -- The statement: f is a remaining file exactly when some unpicked slot of l0 holds f.
    f ∈ remaining l0 ↔ ∃ x ∈ l0, x.2 = false ∧ x.1 = f := by
  -- Unfold "remaining": the files of the slots whose flag is false.
  simp only [remaining, List.mem_map, List.mem_filter, Bool.not_eq_true']
  -- Both sides say: some slot of `l0`, unpicked, holds the file `f`.
  -- Two directions to show.
  constructor
  -- Left to right: take the slot the definition gives.
  · rintro ⟨x, ⟨hx, hun⟩, rfl⟩
    -- It is the witness wanted.
    exact ⟨x, hx, hun, rfl⟩
  -- Right to left: take the slot given.
  · rintro ⟨x, hx, hun, rfl⟩
    -- It is the witness the definition wants.
    exact ⟨x, ⟨hx, hun⟩, rfl⟩

/-- A moved version is a version of a picked L0 file. -/
theorem mem_moved {l0 : List Slot} {v : Version} :
    -- The statement: v is a moved version exactly when some picked slot of l0 holds v.
    v ∈ moved l0 ↔ ∃ x ∈ l0, x.2 = true ∧ v ∈ x.1 := by
  -- Both sides say: some slot of `l0`, picked, holds the version `v`.
  simp only [moved, List.mem_flatten, List.mem_map, List.mem_filter]
  -- Two directions to show.
  constructor
  -- Left to right: take the slot the definition gives.
  · rintro ⟨_, ⟨x, ⟨hx, hp⟩, rfl⟩, hv⟩
    -- It is the witness wanted.
    exact ⟨x, hx, hp, hv⟩
  -- Right to left: take the slot given.
  · rintro ⟨x, hx, hp, hv⟩
    -- It is the witness the definition wants.
    exact ⟨_, ⟨x, ⟨hx, hp⟩, rfl⟩, hv⟩

/-- Two distinct members of a list satisfying `Pairwise R` are related one
way or the other, depending on which comes first. -/
theorem pairwise_either {α : Type} {R : α → α → Prop} {a b : α} :
    -- The statement: in a pairwise list, two different members are related one way or the other.
    ∀ {l : List α}, l.Pairwise R → a ∈ l → b ∈ l → a ≠ b → R a b ∨ R b a
  -- The empty list has no member.
  | [], _, ha, _, _ => absurd ha List.not_mem_nil
  -- A list with first element x and rest xs.
  | x :: xs, hp, ha, hb, hne => by
    -- Pairwise on x :: xs means: x is related to every element of xs, and xs is pairwise.
    rw [List.pairwise_cons] at hp
    -- a is x, or a is in the rest.
    rcases List.mem_cons.mp ha with rfl | ha'
    -- a is x. Then b is x, or b is in the rest.
    · rcases List.mem_cons.mp hb with rfl | hb'
      -- `a` and `b` would both be the head: excluded.
      ·
        -- They are different by assumption.
        exact absurd rfl hne
      -- `a` is the head and `b` comes later.
      ·
        -- The head is related to everything after it.
        exact Or.inl (hp.1 b hb')
    -- a is in the rest. Then b is x, or b is in the rest.
    · rcases List.mem_cons.mp hb with rfl | hb'
      -- `b` is the head and `a` comes later.
      ·
        -- The head is related to everything after it.
        exact Or.inr (hp.1 a ha')
      -- Both are in the tail.
      ·
        -- The claim for the shorter list.
        exact pairwise_either hp.2 ha' hb' hne

/-- **Compacting a closed set keeps the read order ordered.** -/
theorem compact_closed_preserves_order {mem l1 : Source} {l0 : List Slot}
    -- The read order before the compaction is ordered.
    (hord : Ordered (before mem l0 l1))
    -- The picked set is closed.
    (hclosed : Closed l0) :
    -- The statement: the read order after the compaction is ordered.
    Ordered (after mem l0 l1) := by
  -- Unfold "ordered" and the read order before the compaction.
  unfold Ordered before at hord
  -- Split "pairwise" over "the memtable, then the L0 files followed by L1".
  rw [List.pairwise_cons, List.pairwise_append] at hord
  -- hmem: the memtable is newer than every L0 file and than L1.
  -- hl0: the L0 files are ordered among themselves.
  -- hl1: every L0 file is newer than L1.
  obtain ⟨hmem, hl0, -, hl1⟩ := hord
  -- State the L0 order per slot instead of per file.
  rw [List.pairwise_map] at hl0
  -- The L0 order carries both facts per pair: newer, and the closure rule.
  have hboth := hl0.and hclosed
  -- Every L0 file is newer than L1, stated per slot.
  have hfile_l1 : ∀ x ∈ l0, Newer x.1 l1 := fun x hx =>
    -- (an L0 slot's file is one of the L0 files, and L1 is the last source)
    hl1 x.1 (List.mem_map_of_mem hx) l1 List.mem_cons_self
  -- The memtable is newer than every L0 file, stated per slot.
  have hmem_file : ∀ x ∈ l0, Newer mem x.1 := fun x hx =>
    -- (an L0 slot's file is one of the sources after the memtable)
    hmem x.1 (List.mem_append_left _ (List.mem_map_of_mem hx))
  -- The memtable is newer than L1.
  have hmem_l1 : Newer mem l1 := hmem l1 (List.mem_append_right _ List.mem_cons_self)
  -- Unfold "ordered" and the read order after the compaction.
  unfold Ordered after
  -- Split "pairwise" over "the memtable, then the remaining files followed by the new L1".
  rw [List.pairwise_cons, List.pairwise_append]
  -- Three things left to show; a single source (the new L1) is pairwise on its own.
  refine ⟨?_, ?_, List.pairwise_singleton _ _, ?_⟩
  -- First: the memtable is newer than every remaining file and than the new L1.
  ·
    -- Take a source after the memtable, and versions (k, s) of the memtable and (k, t) of that
    -- source.
    intro src hsrc k s t hs ht
    -- The source is a remaining file, or the new L1.
    rcases List.mem_append.mp hsrc with hrem | hl1'
    -- A remaining file is an L0 file.
    ·
      -- Take its slot x.
      obtain ⟨x, hx, -, rfl⟩ := mem_remaining.mp hrem
      -- The memtable is newer than every L0 file.
      exact hmem_file x hx k s t hs ht
    -- The new L1 holds old L1 versions and moved L0 versions.
    ·
      -- The source is the new L1.
      rcases List.mem_singleton.mp hl1' with rfl
      -- (k, t) is an old L1 version, or a moved one.
      rcases List.mem_append.mp ht with ht1 | htm
      -- Old L1: the memtable is newer than L1.
      · exact hmem_l1 k s t hs ht1
      -- Moved: it comes from a picked L0 slot y.
      · obtain ⟨y, hy, -, hty⟩ := mem_moved.mp htm
        -- The memtable is newer than every L0 file.
        exact hmem_file y hy k s t hs hty
  -- Second: the remaining files keep their L0 order, a sub-list of an ordered one.
  ·
    -- Unfold "remaining".
    unfold remaining
    -- State the order per slot.
    rw [List.pairwise_map]
    -- Filtering a pairwise list leaves a pairwise list.
    exact hl0.sublist List.filter_sublist
  -- Third: every remaining file is newer than the new L1. This is where the closure rule is used.
  ·
    -- Take a remaining file and versions (k, s) of it and (k, t) of the new L1.
    intro src hsrc l1' hl1' k s t hs ht
    -- Take the file's slot x, which is unpicked.
    obtain ⟨x, hx, hxkeep, rfl⟩ := mem_remaining.mp hsrc
    -- The other source is the new L1.
    rcases List.mem_singleton.mp hl1' with rfl
    -- (k, t) is an old L1 version, or a moved one.
    rcases List.mem_append.mp ht with ht1 | htm
    -- An old L1 version: every L0 file is newer than L1.
    ·
      -- x is an L0 slot.
      exact hfile_l1 x hx k s t hs ht1
    -- A version `(k, t)` of a picked file `y`.
    ·
      -- Take that picked slot y.
      obtain ⟨y, hy, hypick, hty⟩ := mem_moved.mp htm
      -- `x` is kept and `y` is picked, so they are different slots.
      have hne : x ≠ y := by
        -- Suppose x and y were the same slot.
        intro hxy
        -- Then the unpicked x would be picked.
        rw [hxy, hypick] at hxkeep
        -- False is not true: impossible.
        cases hxkeep
      -- x and y are different L0 slots, so one comes before the other.
      rcases pairwise_either hboth hx hy hne with ⟨hxy, -⟩ | ⟨-, hrule⟩
      -- `x` comes before `y`: it is newer, so `t < s`.
      ·
        -- The L0 order says so.
        exact hxy k s t hs hty
      -- `y` comes before `x`: the closure rule says `x` does not overlap the hull. Yet `x` holds
      -- `k`, which picked `y` also holds: a contradiction, so this case cannot happen.
      ·
        -- The closure rule for the picked y and the unpicked x: x does not overlap the hull.
        have hov := hrule hypick hxkeep
        -- But x does overlap it:
        have : overlaps x.1 (hullKeys l0) = true :=
          -- x holds k, and k is a key of the picked slot y.
          overlaps_of_shared hs (mem_hullKeys hy hypick hty)
        -- So "true" would be "false".
        rw [hov] at this
        -- Impossible: no case.
        cases this

/-- A compaction moves versions and loses none: the read orders before and
after hold the same versions. -/
theorem compact_same_versions {mem l1 : Source} {l0 : List Slot} (v : Version) :
    -- The statement: v is held after the compaction exactly when it was held before.
    Holds (after mem l0 l1) v ↔ Holds (before mem l0 l1) v := by
  -- Unfold "held" and the two read orders.
  unfold Holds after before
  -- Two directions to show.
  constructor
  -- From after to before: find the source that held `v` before.
  ·
    -- Take the source that holds v after.
    rintro ⟨src, hsrc, hv⟩
    -- It is the memtable, or a later source.
    rcases List.mem_cons.mp hsrc with rfl | hsrc'
    -- The memtable is in both.
    ·
      -- The same source holds v before.
      exact ⟨src, List.mem_cons_self, hv⟩
    -- A later source: a remaining file, or the new L1.
    rcases List.mem_append.mp hsrc' with hrem | hl1'
    -- A remaining file is an L0 file.
    ·
      -- Take its slot x.
      obtain ⟨x, hx, -, rfl⟩ := mem_remaining.mp hrem
      -- Its file is one of the L0 files before.
      exact ⟨x.1, List.mem_cons_of_mem _ (List.mem_append_left _ (List.mem_map_of_mem hx)), hv⟩
    -- The source is the new L1.
    · rcases List.mem_singleton.mp hl1' with rfl
      -- v is an old L1 version, or a moved one.
      rcases List.mem_append.mp hv with hv1 | hvm
      -- An old L1 version is in the old L1.
      ·
        -- L1 is the last source before.
        exact ⟨l1, List.mem_cons_of_mem _ (List.mem_append_right _ List.mem_cons_self), hv1⟩
      -- A moved version is in its picked L0 file.
      ·
        -- Take the picked slot y that holds v.
        obtain ⟨y, hy, -, hvy⟩ := mem_moved.mp hvm
        -- Its file is one of the L0 files before.
        exact ⟨y.1, List.mem_cons_of_mem _ (List.mem_append_left _ (List.mem_map_of_mem hy)), hvy⟩
  -- From before to after: find the source that holds `v` after.
  ·
    -- Take the source that held v before.
    rintro ⟨src, hsrc, hv⟩
    -- It is the memtable, or a later source.
    rcases List.mem_cons.mp hsrc with rfl | hsrc'
    -- The memtable is in both.
    ·
      -- The same source holds v after.
      exact ⟨src, List.mem_cons_self, hv⟩
    -- A later source: an L0 file, or L1.
    rcases List.mem_append.mp hsrc' with hl0' | hl1'
    -- An L0 file either remains or moved into L1.
    ·
      -- Take its slot x.
      obtain ⟨x, hx, rfl⟩ := List.mem_map.mp hl0'
      -- Split on whether x was picked.
      cases hp : x.2 with
      -- Not picked:
      | false =>
        -- its file is one of the remaining files.
        exact ⟨x.1, List.mem_cons_of_mem _
          (List.mem_append_left _ (mem_remaining.mpr ⟨x, hx, hp, rfl⟩)), hv⟩
      -- Picked:
      | true =>
        -- its versions are among the moved ones, inside the new L1.
        exact ⟨l1 ++ moved l0, List.mem_cons_of_mem _ (List.mem_append_right _ List.mem_cons_self),
          List.mem_append_right _ (mem_moved.mpr ⟨x, hx, hp, hv⟩)⟩
    -- The old L1 is inside the new L1.
    ·
      -- The source is the old L1.
      rcases List.mem_singleton.mp hl1' with rfl
      -- The new L1 starts with the old L1's versions.
      exact ⟨src ++ moved l0, List.mem_cons_of_mem _ (List.mem_append_right _ List.mem_cons_self),
        List.mem_append_left _ hv⟩

/-- **Compacting a closed set changes no read.** For every key and every
snapshot, the read after the compaction answers what it answered before. -/
theorem compact_closed_preserves_reads {mem l1 : Source} {l0 : List Slot}
    -- The read order before the compaction is ordered.
    (hord : Ordered (before mem l0 l1))
    -- The picked set is closed.
    (hclosed : Closed l0) (k : Nat) (snap : Nat) :
    -- The statement: the read after the compaction equals the read before.
    read (after mem l0 l1) k snap = read (before mem l0 l1) k snap :=
  -- Both orders are ordered and hold the same versions.
  read_eq_of_same_versions (compact_closed_preserves_order hord hclosed) hord
    -- (the two orders hold the same versions, by the lemma above)
    compact_same_versions k snap

/-! ## The closure picker -/

/-- One pass of the closure rule over L0, newest file first. `seen` says
whether some newer slot is already picked. A slot becomes picked if it was,
or if a newer slot is picked and its file's range meets `hull`. -/
def stepWith (hull : List Nat) : Bool → List Slot → List Slot
  -- No slot left: nothing.
  | _, [] => []
  -- A slot with file f and flag b, then the rest.
  | seen, (f, b) :: rest =>
    -- f's new flag: picked before, or a newer slot is picked and f overlaps the hull. The rest is
    -- processed knowing whether a slot so far is picked.
    (f, b || (seen && overlaps f hull)) :: stepWith hull (seen || b) rest

/-- One closure step: a pass against the current picked set's hull. -/
def step (l0 : List Slot) : List Slot := stepWith (hullKeys l0) false l0

/-- Repeat closure steps until one changes nothing, at most `n` times. -/
def closeFuel : Nat → List Slot → List Slot
  -- No fuel left: stop.
  | 0, l0 => l0
  -- Otherwise: stop if a step changes nothing, else take the step and go on with less fuel.
  | n + 1, l0 => if step l0 = l0 then l0 else closeFuel n (step l0)

/-- The closure: enough steps to reach the fixpoint, since each step that
changes something picks at least one more file (`closeFuel_fixpoint`). -/
def close (l0 : List Slot) : List Slot := closeFuel (l0.length + 1) l0

/-- `intersects f lo hi`: file `f` holds a key in `[lo, hi]`, the range
`compact_range` was asked for. -/
def intersects (f : Source) (lo hi : Nat) : Bool :=
  -- Some version of f has its key between lo and hi.
  f.any fun e => decide (lo ≤ e.1 ∧ e.1 ≤ hi)

/-- The starting pick: the L0 files that intersect the range. This alone is
what the defective picker compacts. -/
def initialPick (lo hi : Nat) (files : List Source) : List Slot :=
  -- Pair each file with whether it intersects the range.
  files.map fun f => (f, intersects f lo hi)

/-- The fixed picker: the starting pick, closed under the closure rule. -/
def pickL0 (lo hi : Nat) (files : List Source) : List Slot :=
  -- Close the starting pick.
  close (initialPick lo hi files)

/-- A pass keeps the files and their order; it only changes flags. -/
theorem stepWith_files (hull : List Nat) :
    -- The statement: for every `seen` flag and slot list, the files after a pass are the files
    -- before, in order.
    ∀ (seen : Bool) (l : List Slot), (stepWith hull seen l).map Prod.fst = l.map Prod.fst
  -- No slot: nothing to compare.
  | _, [] => rfl
  -- A slot with file f and flag b, then the rest.
  | seen, (f, b) :: rest => by
    -- The head keeps its file `f`; the rest keeps its files by induction.
    simp only [stepWith, List.map_cons]
    -- The rest keeps its files (the claim for the shorter list).
    rw [stepWith_files hull (seen || b) rest]

/-- A pass keeps the number of slots. -/
theorem stepWith_length (hull : List Nat) (seen : Bool) (l : List Slot) :
    -- The statement: the pass leaves as many slots as it found.
    (stepWith hull seen l).length = l.length := by
  -- Same files, so same length.
  have := congrArg List.length (stepWith_files hull seen l)
  -- Equal lists of files have equal lengths.
  simpa using this

/-- A pass never unpicks a file. -/
theorem stepWith_keeps_picked (hull : List Nat) :
    -- The statement: a slot that was picked before a pass is picked after it.
    ∀ (seen : Bool) (l : List Slot) (f : Source), (f, true) ∈ l → (f, true) ∈ stepWith hull seen l
  -- The empty list holds no slot.
  | _, [], _, h => absurd h List.not_mem_nil
  -- A first slot with file g and flag b, then the rest.
  | seen, (g, b) :: rest, f, h => by
    -- Unfold one step of the pass.
    simp only [stepWith]
    -- The picked slot is the first one, or it is in the rest.
    rcases List.mem_cons.mp h with he | hm
    -- The picked slot is the head: its flag stays `true`.
    ·
      -- Its file is g and its flag b is true.
      simp only [Prod.mk.injEq] at he
      -- Replace them.
      obtain ⟨rfl, rfl⟩ := he
      -- "true or anything" is true: the head of the new list is that slot.
      simp
    -- In the rest: it stays picked there (the claim for the shorter list), so it is in the new
    -- list.
    · exact List.mem_cons_of_mem _ (stepWith_keeps_picked hull _ rest f hm)

/-- A pass that changes the list picks strictly more files; one that does
not change it picks at least as many. `countP Prod.snd` counts the picked
slots. -/
theorem stepWith_count (hull : List Nat) :
    -- The statement: for every `seen` flag and slot list,
    ∀ (seen : Bool) (l : List Slot),
      -- a pass picks at least as many slots as were picked,
      l.countP Prod.snd ≤ (stepWith hull seen l).countP Prod.snd ∧
      -- and strictly more when it changes the list.
      (stepWith hull seen l ≠ l → l.countP Prod.snd < (stepWith hull seen l).countP Prod.snd)
  -- No slot: nothing changes and both counts are 0.
  | _, [] => by simp [stepWith]
  -- A slot with file f and flag b, then the rest.
  | seen, (f, b) :: rest => by
    -- The claim for the rest of the list.
    obtain ⟨ih1, ih2⟩ := stepWith_count hull (seen || b) rest
    -- A picked head stays picked: `true || _` is `true`.
    have hmono : b = true → (b || (seen && overlaps f hull)) = true := by
      -- Suppose b is true.
      intro hb
      -- Replace b by true.
      rw [hb]
      -- "true or anything" computes to true.
      rfl
    -- Unfold one step of the pass.
    simp only [stepWith]
    -- Name the head's new flag `b'`.
    generalize (b || (seen && overlaps f hull)) = b' at *
    -- Four cases for the old flag `b` and the new flag `b'`.
    cases b <;> cases b'
    -- Unpicked before and after: any change is in the rest.
    ·
      -- Neither head counts; the lists differ exactly when the rests differ.
      simp only [List.countP_cons, Bool.false_eq_true, ite_false, ne_eq, List.cons.injEq,
        true_and]
      -- The rest picks at least as many, and strictly more when it changed.
      exact ⟨by omega, fun hne => by have := ih2 hne; omega⟩
    -- Newly picked: the count grows by one.
    ·
      -- The new head counts and the old one does not.
      simp only [List.countP_cons, Bool.false_eq_true, ite_false, ite_true, ne_eq,
        List.cons.injEq]
      -- So the count is strictly larger, whatever the rest did.
      exact ⟨by omega, fun _ => by omega⟩
    -- Picked before and unpicked after: impossible.
    ·
      -- A picked head stays picked, so "true = false" would hold.
      exact absurd (hmono rfl) (by decide)
    -- Picked before and after: any change is in the rest.
    ·
      -- Both heads count; the lists differ exactly when the rests differ.
      simp only [List.countP_cons, ite_true, ne_eq, List.cons.injEq, true_and]
      -- The rest picks at least as many, and strictly more when it changed.
      exact ⟨by omega, fun hne => by have := ih2 hne; omega⟩

/-- Enough fuel reaches a fixpoint: if the fuel `n` exceeds the number of
files not yet picked, `closeFuel n l` is a list a step leaves unchanged. -/
theorem closeFuel_fixpoint :
    -- The statement: for every amount of fuel n and slot list l, if the slots not yet picked number
    -- fewer than n,
    ∀ (n : Nat) (l : List Slot), l.length < l.countP Prod.snd + n →
      -- then a step leaves the result of closeFuel unchanged.
      step (closeFuel n l) = closeFuel n l
  -- Case: no fuel.
  | 0, l, h => by
    -- No fuel: impossible, as no more files can be picked than exist.
    have := List.countP_le_length (p := Prod.snd) (l := l)
    -- Picked slots never outnumber slots, so the assumption cannot hold with n = 0.
    omega
  -- Case: fuel n + 1.
  | n + 1, l, h => by
    -- Unfold one round of closeFuel.
    unfold closeFuel
    -- Split on whether a step changes l.
    by_cases hfix : step l = l
    -- Already a fixpoint: done.
    ·
      -- closeFuel returns l itself.
      rw [ite_eq_left hfix]
      -- And a step leaves l unchanged.
      exact hfix
    -- Not a fixpoint: the step picked one more file, so the remaining fuel still suffices.
    ·
      -- closeFuel goes on from the stepped list with fuel n.
      rw [ite_eq_right hfix]
      -- Use the claim for fuel n on the stepped list; its condition is left to show.
      apply closeFuel_fixpoint n (step l)
      -- The step changed the list, so it picked strictly more slots.
      have hcount := (stepWith_count (hullKeys l) false l).2 hfix
      -- And it kept the number of slots.
      have hlen := stepWith_length (hullKeys l) false l
      -- Unfold "step".
      unfold step
      -- Arithmetic: one more picked, one less fuel.
      omega

/-- The closure is a fixpoint of the closure step. -/
theorem close_fixpoint (l : List Slot) : step (close l) = close l :=
  -- The fuel is the number of slots plus one, which exceeds the unpicked slots.
  closeFuel_fixpoint _ l (by omega)

/-- The closure keeps the files and their order. -/
theorem closeFuel_files : ∀ (n : Nat) (l : List Slot), (closeFuel n l).map Prod.fst = l.map Prod.fst
  -- No fuel: the list itself.
  | 0, _ => rfl
  -- Fuel n + 1.
  | n + 1, l => by
    -- Unfold one round of closeFuel.
    unfold closeFuel
    -- Split on whether it stopped.
    split
    -- Stopped at a fixpoint: the list itself.
    ·
      -- Nothing to show.
      rfl
    -- One more step keeps the files, and the remaining steps do too.
    ·
      -- The remaining steps keep the files (the claim for less fuel).
      rw [closeFuel_files n (step l)]
      -- And one pass keeps the files.
      exact stepWith_files _ _ _

/-- The closure never unpicks a file. -/
theorem closeFuel_keeps_picked :
    -- The statement: a slot picked before closing is picked after it.
    ∀ (n : Nat) (l : List Slot) (f : Source), (f, true) ∈ l → (f, true) ∈ closeFuel n l
  -- No fuel: the list itself.
  | 0, _, _, h => h
  -- Fuel n + 1.
  | n + 1, l, f, h => by
    -- Unfold one round of closeFuel.
    unfold closeFuel
    -- Split on whether it stopped.
    split
    -- Stopped at a fixpoint: the list itself.
    ·
      -- The slot is in it, as given.
      exact h
    -- One more step keeps the pick, and the remaining steps do too.
    ·
      -- One pass keeps it picked, then the claim for less fuel.
      exact closeFuel_keeps_picked n _ f (stepWith_keeps_picked _ _ _ f h)

/-- A pass that changes nothing certifies the closure rule: every unpicked
slot after a picked one (or after `seen`) does not overlap `hull`. -/
theorem stepWith_fixed_closed (hull : List Nat) :
    -- The statement: for every `seen` flag and slot list a pass leaves unchanged: if `seen` is true
    -- then no unpicked slot of the list overlaps the hull, and the list obeys the closure rule pair
    -- by pair.
    ∀ (seen : Bool) (l : List Slot), stepWith hull seen l = l →
      -- (the first half of that statement; the second half is the next line)
      (seen = true → ∀ x ∈ l, x.2 = false → overlaps x.1 hull = false) ∧
      l.Pairwise (ClosureRule hull)
  -- An empty list has no slot and no pair.
  | _, [], _ => ⟨fun _ _ h => absurd h List.not_mem_nil, List.Pairwise.nil⟩
  -- A slot with file f and flag b, then the rest, unchanged by the pass.
  | seen, (f, b) :: rest, hfix => by
    -- An unchanged list is an unchanged head flag and an unchanged rest.
    simp only [stepWith, List.cons.injEq, Prod.mk.injEq, true_and] at hfix
    -- hflag: the head's flag did not change.
    -- hrest: the pass changed nothing in the rest.
    obtain ⟨hflag, hrest⟩ := hfix
    -- The claim for the rest, with the `seen` flag it was processed with.
    obtain ⟨ihseen, ihpair⟩ := stepWith_fixed_closed hull (seen || b) rest hrest
    -- Two things to show.
    refine ⟨?_, ?_⟩
    -- With a picked slot before it, an unpicked slot cannot overlap.
    ·
      -- Suppose `seen` is true, and take an unpicked slot x of the list.
      intro hseen x hx hxun
      -- x is the head, or it is in the rest.
      rcases List.mem_cons.mp hx with rfl | hm
      -- The head: unpicked, and its flag stayed unpicked, so the overlap test failed.
      ·
        -- The head's flag b is false.
        simp only at hxun
        -- Replace `seen` by true and b by false.
        subst hseen hxun
        -- The unchanged flag then reads "the overlap test is false".
        simpa using hflag
      -- In the rest: the claim for the rest applies, since `seen` stays true.
      · exact ihseen (by simp [hseen]) x hm hxun
    -- The head obeys the rule with every later slot; the rest by induction.
    ·
      -- The rest is pairwise already; left to show: the head against each later slot.
      refine List.Pairwise.cons ?_ ihpair
      -- Take a later slot y, with the head picked and y unpicked.
      intro y hy hbpick hyun
      -- The rest was processed with `seen` true (the head is picked), so y does not overlap.
      exact ihseen (by simp at hbpick; simp [hbpick]) y hy hyun

/-- A fixpoint of the closure step is closed. -/
theorem fixpoint_closed {l : List Slot} (hfix : step l = l) : Closed l :=
  -- A pass started with `seen` false that changes nothing gives the pairwise rule: take the second
  -- part.
  (stepWith_fixed_closed (hullKeys l) false l hfix).2

/-- **The closure picker returns a closed set.** -/
theorem pickL0_closed (lo hi : Nat) (files : List Source) : Closed (pickL0 lo hi files) :=
  -- The closure is a fixpoint, and a fixpoint is closed.
  fixpoint_closed (close_fixpoint _)

/-- The picker keeps the L0 files and their order. -/
theorem pickL0_files (lo hi : Nat) (files : List Source) :
    -- The statement: the slots' files are exactly the files given, in order.
    (pickL0 lo hi files).map Prod.fst = files := by
  -- Unfold the picker and the closure.
  unfold pickL0 close
  -- Closing keeps the files.
  rw [closeFuel_files]
  -- The starting pick pairs each file with a flag; dropping the flags
  -- gives the files back.
  simp [initialPick, Function.comp_def]

/-- **The picker covers the range.** Every L0 file that intersects
`[lo, hi]` is picked, so `compact_range` compacts what it was asked to. -/
theorem pickL0_covers {lo hi : Nat} {files : List Source} {f : Source}
    -- `f` is an L0 file ...
    (hf : f ∈ files)
    -- ... with a key in the range.
    (hint : intersects f lo hi = true) :
    -- The statement: f is a picked slot of the picker's result.
    (f, true) ∈ pickL0 lo hi files := by
  -- Closing never unpicks, so it is enough that f is picked in the starting pick.
  apply closeFuel_keeps_picked
  -- Unfold the starting pick: each file paired with its "intersects" flag.
  simp only [initialPick, List.mem_map]
  -- f is one of the files, and its flag is true.
  exact ⟨f, hf, by rw [hint]⟩

/-- **E1, `compact_range` at L0.** Start from any ordered read order (the
memtable, the L0 files newest first, L1). Compact the L0 files the closure
picker selects for `[lo, hi]` into L1. The result is ordered, so the next
write, flush or compaction starts from an ordered state again, and every
read at every snapshot answers what it answered before. -/
theorem compact_range_reads_newest {mem l1 : Source} {files : List Source} {lo hi : Nat}
    -- The read order before the compaction is ordered.
    (hord : Ordered (mem :: (files ++ [l1]))) :
    -- The statement: the read order after the compaction is ordered,
    Ordered (after mem (pickL0 lo hi files) l1) ∧
    -- and for every key and snapshot the read after it
    ∀ k snap, read (after mem (pickL0 lo hi files) l1) k snap =
      -- equals the read before it.
      read (mem :: (files ++ [l1])) k snap := by
  -- Before the compaction, the slots' files are exactly `files`.
  have hbefore : before mem (pickL0 lo hi files) l1 = mem :: (files ++ [l1]) := by
    -- Unfold the read order before the compaction.
    unfold before
    -- The slots' files are the files given.
    rw [pickL0_files]
  -- Restate the assumption and the goal in terms of the slots.
  rw [← hbefore] at hord ⊢
  -- Ordered: compacting a closed set keeps the order, and the picker's set is closed.
  exact ⟨compact_closed_preserves_order hord (pickL0_closed lo hi files),
    -- Reads: compacting a closed set changes no read.
    compact_closed_preserves_reads hord (pickL0_closed lo hi files)⟩

/-! ## The RED case: picking only the files that meet the range -/

/-- The newer L0 file of the counterexample: key 1 at sequence 3 and key 5
at sequence 4. Its range `[1, 5]` meets the compacted range `[1, 1]`. -/
def redNewer : Source := [(1, 3), (5, 4)]

/-- The older L0 file: key 5 at sequence 2. Its range `[5, 5]` misses
`[1, 1]`, but it overlaps the newer file's range. -/
def redOlder : Source := [(5, 2)]

/-- The counterexample's read order is ordered: the newer file's key 5 is
newer than the older file's. -/
theorem red_ordered : Ordered ([] :: ([redNewer, redOlder] ++ [[]])) := by
  -- Every pair of sources is compared by evaluating `newerB`.
  unfold Ordered
  -- Both sources lists are small and concrete: compute the comparison.
  decide

/-- **`intersect_only_breaks_reads`.** Picking only the L0 files that
intersect the range `[1, 1]` moves the newer file into L1 and leaves the
older one in L0. A read of key 5 then finds the older file first and
answers sequence 2, where it answered 4 before. The fixed picker also
picks the older file (`red_closure_picks_both`). -/
theorem intersect_only_breaks_reads :
    -- The statement: after compacting only the intersecting file, the read of key 5 answers
    -- sequence 2, where before the compaction it answered 4.
    read (after [] (initialPick 1 1 [redNewer, redOlder]) []) 5 10 = some 2 ∧
    read ([] :: ([redNewer, redOlder] ++ [[]])) 5 10 = some 4 := by
  -- Both reads are small computations: compute them.
  decide

/-- The closure picker picks both files for `[1, 1]`, and the read of key 5
still answers 4 after the compaction. -/
theorem red_closure_picks_both :
    -- The statement: the closure picker marks both files as picked, and after the compaction the
    -- read of key 5 still answers 4.
    pickL0 1 1 [redNewer, redOlder] = [(redNewer, true), (redOlder, true)] ∧
    read (after [] (pickL0 1 1 [redNewer, redOlder]) []) 5 10 = some 4 := by
  -- Both are small computations: compute them.
  decide

/-! ## Frozen memtables: install in memtable order

The story. A writer fills the memtable and seals it (rotation): the sealed
memtable joins the frozen ones, which a read asks after the active memtable,
newest first, and before L0. Since E9 the writer does not flush it; the
compaction worker does, or the next write when there is no worker, or a
rotation at the memtable cap, or an explicit flush. Several threads can be
flushing, so the code makes every flush take the `flushing` exclusion and the
OLDEST frozen memtable (`Flusher::flush_oldest` in `src/engine/flush.rs`).

What can go wrong: two frozen memtables, the newer holding key 1 at sequence
2, the older key 1 at sequence 1. If the newer one's flush finishes first, its
file goes into L0, behind the older frozen memtable that a read still asks
first, and the read of key 1 goes back from 2 to 1. The theorems below show
the oldest-first install changes no read, for any number of memtables and
flushes, and the newer-first install breaking one. The TLA+ configurations
`MC_LsmOrder_Green_Flush` and `MC_LsmOrder_Red_FlushAnyOrder` check the same
over interleavings. -/

/-- **Installing the oldest frozen memtable leaves the read order as it
was.** The oldest frozen memtable is the last one read before L0; as the
newest L0 file it is the first one read in L0: the same position. So the
order stays ordered and every read is unchanged. -/
theorem install_oldest_same_order (mem : Source) (imm l0 deeper : List Source) (f : Source) :
    -- Before: the active memtable, the frozen ones with `f` the oldest, L0,
    -- the deeper levels. After: `f` is the newest L0 file. Same list.
    mem :: (imm ++ [f]) ++ l0 ++ deeper = mem :: imm ++ (f :: l0) ++ deeper := by
  -- Both sides are the same list once the appends are flattened.
  simp

/-- **Flushing any number of frozen memtables, oldest first, changes no
read.** Moving the oldest `old` frozen memtables, one by one and oldest first,
to the front of L0 leaves the sources in the same order, so every read of
every key at every snapshot answers as before. This is every flush of E9 in
a row, whichever threads ran them. -/
theorem install_oldest_all (mem : Source) (imm old l0 deeper : List Source) :
    -- Before: `old` are the oldest frozen memtables, after `imm`. After:
    -- they are the newest L0 files, in the same order.
    mem :: (imm ++ old) ++ l0 ++ deeper = mem :: imm ++ (old ++ l0) ++ deeper := by
  -- Flatten the appends: both sides are the same list.
  simp

/-- The reads are unchanged too: a read of key `k` at snapshot `snap` gives
the same answer before and after the oldest-first flushes. -/
theorem install_oldest_all_reads (mem : Source) (imm old l0 deeper : List Source)
    -- (for any key and snapshot)
    (k snap : Nat) :
    -- The read before equals the read after.
    read (mem :: (imm ++ old) ++ l0 ++ deeper) k snap =
      read (mem :: imm ++ (old ++ l0) ++ deeper) k snap := by
  -- The two source lists are equal, so the reads are.
  rw [install_oldest_all]

/-- **`install_newer_breaks_reads`.** Two frozen memtables, the newer
holding key 1 at 2 and the older key 1 at 1. Installing the newer one
first puts it behind the older one, which is still read before L0: the
read of key 1 answers 1 where it answered 2. -/
theorem install_newer_breaks_reads :
    -- Before any flush: the read of key 1 finds 2 in the newer frozen one.
    read ([] :: [[(1, 2)], [(1, 1)]] ++ []) 1 10 = some 2 ∧
    -- After the newer one's file reaches L0 first: the older frozen one
    -- is asked first and answers 1.
    read ([] :: [[(1, 1)]] ++ ([(1, 2)] :: [])) 1 10 = some 1 := by
  -- Concrete lists: evaluate both reads.
  decide

/-! ## Ingest placement

An ingested file holds versions at one fresh sequence, newer than every
version in the tree. It may sit anywhere in the read order where every
source read before it holds none of its keys. The TLA+ configurations
`MC_LsmOrder_Green_Ingest` and `MC_LsmOrder_Red_IngestIgnoresUpper` check
the placement rule that guarantees this. -/

/-- `NoShared a b`: the sources `a` and `b` hold no version of a common
key. -/
def NoShared (a b : Source) : Prop := ∀ k s t, (k, s) ∈ a → (k, t) ∈ b → False

/-- Sources with no common key are `Newer` each way, vacuously. -/
theorem newer_of_noShared {a b : Source} (h : NoShared a b) : Newer a b :=
  -- Take two versions of one key, one in each: there are none, so anything follows.
  fun k s t hs ht => (h k s t hs ht).elim

/-- **`ingest_ordered`.** Put the file `f` between `pre` (the sources read
before it) and `post` (those read after). If no source of `pre` shares a
key with `f`, and every version of `f` is newer than every version already
held, the read order stays ordered. -/
theorem ingest_ordered {pre post : List Source} {f : Source}
    -- The read order before the ingest is ordered.
    (hord : Ordered (pre ++ post))
    -- Nothing read before the file holds one of its keys.
    (hpre : ∀ src ∈ pre, NoShared src f)
    -- The file's versions are newer than every version held.
    (hfresh : ∀ v ∈ f, ∀ w, Holds (pre ++ post) w → w.2 < v.2) :
    -- The statement: with f placed between `pre` and `post`, the read order is ordered.
    Ordered (pre ++ f :: post) := by
  -- Unfold "ordered" to "pairwise newer", in the assumptions and the goal.
  unfold Ordered at *
  -- Split "pairwise" over "pre followed by the rest", on both.
  rw [List.pairwise_append] at hord ⊢
  -- Name the three facts: pre is ordered, post is ordered, and every source of pre is newer than
  -- every source of post.
  obtain ⟨hp, hq, hpq⟩ := hord
  -- pre is ordered as before; two things are left: f against post, and pre against "f then post".
  refine ⟨hp, List.Pairwise.cons ?_ hq, ?_⟩
  -- The file is newer than every source after it.
  ·
    -- Take a source b after the file, and versions (k, s) of the file and (k, t) of b.
    intro b hb k s t hs ht
    -- The file's versions are newer than every version already held.
    exact hfresh (k, s) hs (k, t) ⟨b, List.mem_append_right _ hb, ht⟩
  -- A source before the file is newer than the file (no common key) and than every source after it
  -- (as before).
  ·
    -- Take a source a before the file, and a source b among "the file, then post".
    intro a ha b hb
    -- b is the file, or b is in post.
    rcases List.mem_cons.mp hb with rfl | hb'
    -- The file: a shares no key with it, so "newer" holds for lack of a case.
    · exact newer_of_noShared (hpre a ha)
    -- In post: a was newer than b before.
    · exact hpq a ha b hb'

/-- **`ingest_below_holder_breaks_reads`.** L1 holds key 1 at 1. An
ingested file holding key 1 at 2, placed in L2 below it, is read after
it: the read answers 1. Placed in front, it answers 2. -/
theorem ingest_below_holder_breaks_reads :
    -- The statement: with the file below the older version, the read answers 1; with the file in
    -- front, it answers 2.
    read [[], [(1, 1)], [(1, 2)]] 1 10 = some 1 ∧
    read [[], [(1, 2)], [(1, 1)]] 1 10 = some 2 := by
  -- Both reads are small computations: compute them.
  decide

/-! ## A merge inside L0 (E38)

The story. Under universal compaction nothing leaves L0: a merge takes some
L0 files, writes their versions as one file, and puts that file in L0 in
the place of the oldest file it took (`perform_compaction_to` with source
and target level 0, src/engine/compaction.rs). In the read order that is:
some sources `pre` read first (the memtables and the newer L0 files, a file
flushed while the merge ran among them), then the files merged, `win`, then
the sources `post` read after them (the older L0 files and the deeper
levels). The merged file is `win.flatten`: every version of every file
merged.

What can go wrong: key 1 is in three L0 files, at sequence 3 in the newest,
2 in the middle one, 1 in the oldest. A merge that takes the newest and the
oldest and leaves the middle one out puts sequence 3 below sequence 2, and
the read of key 1 goes back from 3 to 2. The engine picked like that when
it took the order of file ids for the order of L0 (E38): a merge's output
draws its id late and takes its oldest input's place, so a file flushed
while the merge ran has the smaller id and the newer place. It now takes
the files in the order L0 lists them, so the files merged lie next to one
another (`pick_and_run_universal`). The theorems below show a merge of
neighbours keeps the order and changes no read, for any number of files,
keys and versions, and the merge with a file left out breaking one. The
TLA+ configurations `MC_LsmOrder_Green_L0Merge` and
`MC_LsmOrder_Red_L0MergeGap` check the same over interleavings. -/

/-- The merged file holds exactly the versions the files merged held, so
the read order after the merge holds exactly the versions the read order
before it held. -/
theorem merge_window_same_versions (pre win post : List Source) (v : Version) :
    -- The statement: a version is held after the merge exactly when it was held before.
    Holds (pre ++ win.flatten :: post) v ↔ Holds (pre ++ win ++ post) v := by
  -- "Held" means: some source of the list holds it.
  unfold Holds
  -- Two directions to show.
  constructor
  -- Held after the merge, so held before: take the source `src` that holds v after.
  · rintro ⟨src, hsrc, hv⟩
    -- That source is read before the merged file, or is the merged file or one read after it.
    rcases List.mem_append.mp hsrc with hpre | hrest
    -- Read before: it was there before the merge too.
    · exact ⟨src, List.mem_append_left _ (List.mem_append_left _ hpre), hv⟩
    -- Otherwise it is the merged file, or a source read after it.
    · rcases List.mem_cons.mp hrest with rfl | hpost
      -- The merged file: v comes from one of the files merged, call it `w`,
      · obtain ⟨w, hw, hvw⟩ := List.mem_flatten.mp hv
        -- and that file held v before the merge.
        exact ⟨w, List.mem_append_left _ (List.mem_append_right _ hw), hvw⟩
      -- A source read after: it was there before the merge too.
      · exact ⟨src, List.mem_append_right _ hpost, hv⟩
  -- Held before the merge, so held after: take the source `src` that held v before.
  · rintro ⟨src, hsrc, hv⟩
    -- That source was read before the older sources, or is one of them.
    rcases List.mem_append.mp hsrc with hpw | hpost
    -- Read before them: it is read before the files merged, or is one of the files merged.
    · rcases List.mem_append.mp hpw with hpre | hw
      -- Read before the files merged: it is still there.
      · exact ⟨src, List.mem_append_left _ hpre, hv⟩
      -- One of the files merged: the merged file holds v.
      · exact ⟨win.flatten, List.mem_append_right _ List.mem_cons_self,
          List.mem_flatten.mpr ⟨src, hw, hv⟩⟩
    -- One of the older sources: it is still there.
    · exact ⟨src, List.mem_append_right _ (List.mem_cons_of_mem _ hpost), hv⟩

/-- **`merge_window_ordered`.** Merge the files `win`, which lie next to
one another in the read order, into one file put where they were. If the
read order was ordered, it still is: the merged file is older than
everything that was read before every file merged, and newer than
everything that was read after every one of them. -/
theorem merge_window_ordered {pre win post : List Source}
    -- The read order before the merge is ordered.
    (hord : Ordered (pre ++ win ++ post)) :
    -- The statement: with the merged file in the place of the files merged, the read order is
    -- ordered.
    Ordered (pre ++ win.flatten :: post) := by
  -- Unfold "ordered" to "pairwise newer", in the assumption and the goal.
  unfold Ordered at *
  -- Read the order before the merge as: pre, then "the files merged followed by post".
  rw [List.append_assoc] at hord
  -- Split "pairwise" over "pre followed by the rest", on both.
  rw [List.pairwise_append] at hord ⊢
  -- Name the three facts: pre is ordered, "the files merged, then post" is ordered, and every
  -- source of pre is newer than every source after it.
  obtain ⟨hp, hwp, hpre⟩ := hord
  -- Split "pairwise" once more, over "the files merged, followed by post".
  rw [List.pairwise_append] at hwp
  -- Name what that gives: post is ordered, and every file merged is newer than every source of
  -- post. (The order among the files merged is not needed.)
  obtain ⟨_, hq, hwq⟩ := hwp
  -- pre is ordered as before; two things are left: the merged file against post, and pre against
  -- "the merged file, then post".
  refine ⟨hp, List.Pairwise.cons ?_ hq, ?_⟩
  -- The merged file is newer than every source after it.
  ·
    -- Take a source b after it, and versions (k, s) of the merged file and (k, t) of b.
    intro b hb k s t hs ht
    -- (k, s) comes from one of the files merged, call it `src`,
    obtain ⟨src, hsrc, hin⟩ := List.mem_flatten.mp hs
    -- and that file was newer than b before the merge.
    exact hwq src hsrc b hb k s t hin ht
  -- A source before the merged file is newer than it and than every source after it.
  ·
    -- Take a source a of pre, and a source b among "the merged file, then post".
    intro a ha b hb
    -- b is the merged file, or b is in post.
    rcases List.mem_cons.mp hb with rfl | hb'
    -- The merged file: take versions (k, s) of a and (k, t) of the merged file.
    · intro k s t hs ht
      -- (k, t) comes from one of the files merged, call it `src`,
      obtain ⟨src, hsrc, hin⟩ := List.mem_flatten.mp ht
      -- and a was newer than that file before the merge.
      exact hpre a ha src (List.mem_append_left _ hsrc) k s t hs hin
    -- In post: a was newer than b before.
    · exact hpre a ha b (List.mem_append_right _ hb')

/-- **`merge_window_reads_newest`.** After a merge of files that lie next
to one another, every read of every key at every snapshot answers what it
answered before the merge: by `read_newest`, the newest visible version. -/
theorem merge_window_reads_newest {pre win post : List Source}
    -- The read order before the merge is ordered.
    (hord : Ordered (pre ++ win ++ post))
    -- (for any key and snapshot)
    (k snap : Nat) :
    -- The statement: the read after the merge equals the read before it.
    read (pre ++ win.flatten :: post) k snap = read (pre ++ win ++ post) k snap :=
  -- Both read orders are ordered,
  read_eq_of_same_versions (merge_window_ordered hord) hord
    -- and they hold the same versions: so they read the same.
    (merge_window_same_versions pre win post) k snap

/-- **`merge_gapped_breaks_reads`.** Three L0 files hold key 1: the newest
at sequence 3, the middle one at 2, the oldest at 1. The read order is
ordered and the read of key 1 answers 3. A merge that takes the newest and
the oldest file and leaves the middle one out puts its file in the oldest
one's place, behind the middle file: the read answers 2. This is defect
E38 and the TLA+ configuration `MC_LsmOrder_Red_L0MergeGap`. -/
theorem merge_gapped_breaks_reads :
    -- Before the merge the three files are ordered,
    Ordered [[(1, 3)], [(1, 2)], [(1, 1)]] ∧
    -- and the read of key 1 finds 3 in the newest file.
    read [[(1, 3)], [(1, 2)], [(1, 1)]] 1 10 = some 3 ∧
    -- After the merge of the newest and the oldest file, the middle file is asked first and
    -- answers 2.
    read [[(1, 2)], [(1, 3)] ++ [(1, 1)]] 1 10 = some 2 := by
  -- Unfold "ordered" to "pairwise newer", which Lean can compute on these lists.
  unfold Ordered
  -- Concrete lists: evaluate the comparison and both reads.
  decide

/-! ## Demotion of an overlapping level at open (E14, D13)

A tree's read order is its memtables, its L0 files newest first, then each
level's tables. Within a level whose tables do not overlap, at most one
table can hold a key, so reading the level's tables one after another
answers what reading the one table binary search finds answers
(`level_read_bsearch`, below). So a tree's read order is the flat list
`treeOrder`. A level whose recorded ranges overlap breaks the binary
search; the TLA+ configuration `MC_LsmOrder_Red_NoDemotion` shows the read
it gets wrong.

The engine demotes while it replays the manifest (`Repair::prepare`,
src/engine/manifest/legacy_overlap.rs): every level from 1 down to the
deepest overlapping one goes to the old end of L0, level after level, each
level's tables in the order the manifest lists them. Range tombstones are
not part of this file: what the open does with those of the tables it
moves is `Regolith/RangeDelete.lean`. -/

/-- The flat read order of a tree. -/
def treeOrder (mems l0 : List Source) (levels : List (List Source)) : List Source :=
  -- The memtables, then the L0 files, then every level's tables, level after level.
  mems ++ l0 ++ levels.flatten

/-- The L0 files after the open demotes levels `1..d`: the old L0 files,
then those levels' tables, level by level, each level in the order given.
The engine takes a level's tables in the order the manifest lists them;
`noShared_level_ordered` below is why any order of one level will do. -/
def demoteL0 (d : Nat) (l0 : List Source) (levels : List (List Source)) : List Source :=
  -- The old L0 files, then the tables of the first d levels, level after level.
  l0 ++ (levels.take d).flatten

/-- The levels after the open demotes levels `1..d`: those are empty, the
deeper ones unchanged. -/
def demoteLevels (d : Nat) (levels : List (List Source)) : List (List Source) :=
  -- The first d levels become empty; the deeper ones follow unchanged.
  (levels.take d).map (fun _ => []) ++ levels.drop d

/-- Demoting levels `1..d` leaves the flat read order exactly as it was. -/
theorem demote_same_order (mems l0 : List Source) (levels : List (List Source)) (d : Nat) :
    -- The statement: the flat read order after the demotion equals the one before.
    treeOrder mems (demoteL0 d l0 levels) (demoteLevels d levels) = treeOrder mems l0 levels := by
  -- The emptied levels contribute nothing, and the demoted tables sit
  -- exactly where they were: after L0 and before the deeper levels.
  unfold treeOrder demoteL0 demoteLevels
  -- The emptied levels, joined, are the empty list:
  have hnil : ((levels.take d).map (fun _ => ([] : List Source))).flatten = [] := by
    -- a joined list is empty exactly when every part is;
    rw [List.flatten_eq_nil_iff]
    -- take any part;
    intro l hl
    -- it is one of the emptied levels;
    obtain ⟨_, _, rfl⟩ := List.mem_map.mp hl
    -- so it is empty.
    rfl
  -- Join the new levels: the emptied part vanishes and the deeper levels remain.
  rw [List.flatten_append, hnil, List.nil_append]
  -- On the right, split the old levels into the first d and the rest, and join each part.
  conv => rhs; rw [← List.take_append_drop d levels, List.flatten_append]
  -- Both sides are now the same pieces appended in the same order.
  simp only [List.append_assoc]

/-- **`demote_reads_newest`.** If the tree's read order is ordered, with
the overlapping level's tables in age order, then after the open demotes
every level from L1 down to it, the read order is still ordered and every
read answers as before: by `read_newest`, the newest visible version. -/
theorem demote_reads_newest {mems l0 : List Source} {levels : List (List Source)} (d : Nat)
    -- The read order before the open is ordered.
    (hord : Ordered (treeOrder mems l0 levels)) :
    -- The statement: the read order after the demotion is ordered, and every read answers what it
    -- answered before.
    Ordered (treeOrder mems (demoteL0 d l0 levels) (demoteLevels d levels)) ∧
    ∀ k snap, read (treeOrder mems (demoteL0 d l0 levels) (demoteLevels d levels)) k snap =
      read (treeOrder mems l0 levels) k snap := by
  -- The read order after the demotion is the read order before.
  rw [demote_same_order]
  -- So it is ordered as before, and every read is literally the same.
  exact ⟨hord, fun _ _ => rfl⟩

/-- The tables of the overlapping level E14 leaves behind share no key:
0.1.x placed the ingested file by its point keys, and only its recorded
range overlaps. Tables sharing no key are ordered among themselves in
any order, so age order is ordered. -/
theorem noShared_level_ordered {T : List Source}
    -- No two tables of the level share a key.
    (h : T.Pairwise NoShared) : Ordered T :=
  -- Every pair of tables shares no key, so each is "newer" than the other for lack of a case.
  h.imp newer_of_noShared

/-- The defect: the open demotes only level `d`, moving it to the end of
L0, in front of levels `1..d-1`. -/
def demoteOnly (d : Nat) (l0 : List Source) (levels : List (List Source)) :
    -- (The result: the new L0 files and the new levels.)
    List Source × List (List Source) :=
  -- L0 gains level d's tables at its end, and level d becomes empty.
  (l0 ++ levels.getD (d - 1) [], levels.set (d - 1) [])

/-- **`demote_level_only_breaks_reads`.** L1 holds key 1 at 2; L2's tables
overlap and hold key 2 at 3 and key 1 at 1. Before the open, the read of
key 1 answers 2. Demoting L2 alone puts its tables in front of L1, and
the read answers 1. Demoting L1 with it keeps the answer 2. -/
theorem demote_level_only_breaks_reads :
    -- The statement: before the open, the read of key 1 answers 2; after demoting level 2 alone it
    -- answers 1; after demoting levels 1 and 2 together it answers 2.
    read (treeOrder [[]] [] [[[(1, 2)]], [[(2, 3)], [(1, 1)]]]) 1 10 = some 2 ∧
    read (treeOrder [[]] (demoteOnly 2 [] [[[(1, 2)]], [[(2, 3)], [(1, 1)]]]).1
      (demoteOnly 2 [] [[[(1, 2)]], [[(2, 3)], [(1, 1)]]]).2) 1 10 = some 1 ∧
    read (treeOrder [[]] (demoteL0 2 [] [[[(1, 2)]], [[(2, 3)], [(1, 1)]]])
      (demoteLevels 2 [[[(1, 2)]], [[(2, 3)], [(1, 1)]]])) 1 10 = some 2 := by
  -- All three reads are small computations: compute them.
  decide

/-! ## Binary search within a level

Below L0, a read of key `k` binary-searches a level's tables, which are in
key order, for the first table whose range ends at or above `k`, and reads
that table if its range starts at or below `k`. -/

/-- Binary search over positions `[a, b)`: the least position `i` with
`k ≤ hi i`, or `b` when there is none, halving the interval each step.
`hi i` is the greatest key table `i` records. -/
def bsearch (hi : Nat → Nat) (k : Nat) (a b : Nat) : Nat :=
  -- While the interval is not empty:
  if a < b then
    -- if the middle table ends below k, search to the right of the middle;
    if hi ((a + b) / 2) < k then bsearch hi k ((a + b) / 2 + 1) b
    -- otherwise search from a up to the middle.
    else bsearch hi k a ((a + b) / 2)
  -- An empty interval: its start is the answer.
  else a
-- This definition calls itself on a shorter interval each time; Lean is told to measure b - a.
termination_by b - a
-- Both recursive calls shorten the interval, by arithmetic.
decreasing_by all_goals omega

/-- The linear scan: the least position `i` in `[a, b)` with `k ≤ hi i`,
or `b`, trying each position in turn. -/
def scan (hi : Nat → Nat) (k : Nat) (a b : Nat) : Nat :=
  -- While the interval is not empty: a if table a reaches k, else go on from a + 1. An empty
  -- interval: its start.
  if a < b then (if k ≤ hi a then a else scan hi k (a + 1) b) else a
-- The interval gets shorter each time; Lean is told to measure b - a.
termination_by b - a

/-- `IsFirst hi k a b r`: `r` is the least position in `[a, b)` whose
`hi` reaches `k`, or `b` when none does. -/
def IsFirst (hi : Nat → Nat) (k a b r : Nat) : Prop :=
  -- r lies in the interval (its end included); every position before r ends below k; and if r is a
  -- real position, its table reaches k.
  a ≤ r ∧ r ≤ b ∧ (∀ i, a ≤ i → i < r → hi i < k) ∧ (r < b → k ≤ hi r)

/-- `MonoUpTo hi n`: `hi` never decreases over positions `0..n-1`, as the
greatest keys of a key-ordered level's tables do. -/
def MonoUpTo (hi : Nat → Nat) (n : Nat) : Prop := ∀ i j, i ≤ j → j < n → hi i ≤ hi j

/-- Two positions both first are the same position. -/
theorem isFirst_unique {hi : Nat → Nat} {k a b r1 r2 : Nat}
    -- (given that r1 and r2 are both first) The statement: they are equal.
    (h1 : IsFirst hi k a b r1) (h2 : IsFirst hi k a b r2) : r1 = r2 := by
  -- Take the four facts about r1.
  obtain ⟨ha1, hb1, hlt1, hge1⟩ := h1
  -- Take the four facts about r2.
  obtain ⟨ha2, hb2, hlt2, hge2⟩ := h2
  -- If one were smaller, it would lie below the other, where every `hi`
  -- is below `k`, yet its own `hi` reaches `k`.
  -- Either r1 is smaller, or they are equal, or r2 is smaller.
  rcases Nat.lt_trichotomy r1 r2 with h | h | h
  -- r1 smaller: it lies before r2, so its table ends below k;
  · have := hlt2 r1 ha1 h
    -- but r1 is a real position, so its table reaches k;
    have := hge1 (by omega)
    -- arithmetic says no.
    omega
  -- Equal: done.
  · exact h
  -- r2 smaller: it lies before r1, so its table ends below k;
  · have := hlt1 r2 ha2 h
    -- but r2 is a real position, so its table reaches k;
    have := hge2 (by omega)
    -- arithmetic says no.
    omega

/-- The linear scan finds the first position. -/
theorem scan_spec (hi : Nat → Nat) (k : Nat) (a b : Nat) (hab : a ≤ b) :
    -- The statement: the position the scan returns is the first.
    IsFirst hi k a b (scan hi k a b) := by
  -- Unfold the scan one step.
  rw [scan]
  -- Split on whether the interval is empty.
  split
  -- It is not; name that fact hlt.
  · rename_i hlt
    -- Split on whether table a reaches k.
    split
    -- Position `a` reaches `k`: it is first.
    ·
      -- Name that fact hk.
      rename_i hk
      -- a is in the interval, nothing lies before it, and its table reaches k.
      exact ⟨Nat.le_refl a, hab, fun i h1 h2 => absurd h2 (by omega), fun _ => hk⟩
    -- It does not: the first is further on.
    ·
      -- Name that fact hk.
      rename_i hk
      -- The claim for the shorter interval from a + 1.
      obtain ⟨h1, h2, h3, h4⟩ := scan_spec hi k (a + 1) b (by omega)
      -- The bounds carry over; left to show: every position before the answer ends below k.
      refine ⟨by omega, h2, fun i hi1 hi2 => ?_, h4⟩
      -- Such a position is a itself, or lies in the shorter interval.
      by_cases hia : i = a
      -- It is a:
      · subst hia
        -- table a ends below k, as this case says.
        omega
      -- It lies in the shorter interval: the claim for it.
      · exact h3 i (by omega) hi2
  -- An empty interval: the answer is its end.
  ·
    -- The start equals the end: no position lies before it, and it is not a real position.
    exact ⟨Nat.le_refl a, hab, fun i h1 h2 => absurd h2 (by omega), fun h => absurd h (by omega)⟩
-- The recursion is on a shorter interval; Lean is told to measure b - a.
termination_by b - a

/-- Binary search finds the first position, when `hi` never decreases. -/
theorem bsearch_spec (hi : Nat → Nat) (k : Nat) {n : Nat} (hmono : MonoUpTo hi n)
    -- (for positions a and b with a at or below b, and b within the first n)
    (a b : Nat) (hab : a ≤ b) (hbn : b ≤ n) :
    -- The statement: the position binary search returns is the first.
    IsFirst hi k a b (bsearch hi k a b) := by
  -- Unfold binary search one step.
  rw [bsearch]
  -- Split on whether the interval is empty.
  split
  -- It is not; name that fact hlt.
  · rename_i hlt
    -- Split on whether the middle table ends below k.
    split
    -- The middle is below `k`, and so is everything before it: the first lies after the middle.
    ·
      -- Name that fact hmid.
      rename_i hmid
      -- The claim for the right half.
      obtain ⟨h1, h2, h3, h4⟩ := bsearch_spec hi k hmono ((a + b) / 2 + 1) b (by omega) hbn
      -- The bounds carry over; left to show: every position before the answer ends below k.
      refine ⟨by omega, h2, fun i hi1 hi2 => ?_, h4⟩
      -- Such a position is at or before the middle, or after it.
      by_cases hi3 : i ≤ (a + b) / 2
      -- At or before the middle: its table ends no later than the middle's (the ends never
      -- decrease),
      · have := hmono i ((a + b) / 2) hi3 (by omega)
        -- which ends below k.
        omega
      -- After the middle: the claim for the right half.
      · exact h3 i (by omega) hi2
    -- The middle reaches `k`: the first is at or before it.
    ·
      -- Name that fact hmid.
      rename_i hmid
      -- The claim for the left half.
      obtain ⟨h1, h2, h3, h4⟩ := bsearch_spec hi k hmono a ((a + b) / 2) (by omega) (by omega)
      -- The bounds carry over; left to show: if the answer is a real position, its table reaches k.
      refine ⟨h1, by omega, h3, fun _ => ?_⟩
      -- The answer is the middle itself, or lies before it.
      by_cases heq : bsearch hi k a ((a + b) / 2) = (a + b) / 2
      -- It is the middle:
      · rw [heq]
        -- the middle's table reaches k, as this case says.
        omega
      -- It lies before the middle: the claim for the left half says its table reaches k.
      · exact h4 (by omega)
  -- An empty interval: the answer is its end.
  ·
    -- The start equals the end: no position lies before it, and it is not a real position.
    exact ⟨Nat.le_refl a, hab, fun i h1 h2 => absurd h2 (by omega), fun h => absurd h (by omega)⟩
-- The recursion is on a shorter interval; Lean is told to measure b - a.
termination_by b - a

/-- **`bsearch_eq_scan`.** On positions whose `hi` never decreases, binary
search finds the position the linear scan finds. -/
theorem bsearch_eq_scan (hi : Nat → Nat) (k : Nat) {n : Nat} (hmono : MonoUpTo hi n)
    -- (for positions a and b with a at or below b, and b within the first n)
    (a b : Nat) (hab : a ≤ b) (hbn : b ≤ n) :
    -- The statement: both searches return the same position.
    bsearch hi k a b = scan hi k a b :=
  -- Each returns the first position, and the first position is unique.
  isFirst_unique (bsearch_spec hi k hmono a b hab hbn) (scan_spec hi k a b hab)

/-- A level table: the key range the manifest records and the versions. -/
structure Table where
  /-- The least key the range records. -/
  lo : Nat
  /-- The greatest key the range records. -/
  hi : Nat
  /-- The versions the table holds. -/
  src : Source

/-- `SortedLevel T`: each table's range is a range, holds every key of
the table, and ends below where every later table's range begins: a
level in key order whose tables do not overlap. -/
def SortedLevel (T : List Table) : Prop :=
  -- Every table's range is a real range and holds every key of the table,
  (∀ t ∈ T, t.lo ≤ t.hi ∧ ∀ v ∈ t.src, t.lo ≤ v.1 ∧ v.1 ≤ t.hi) ∧
  -- and of two tables, the earlier one ends below where the later one begins.
  T.Pairwise (fun t u => t.hi < u.lo)

/-- The greatest key of table `i`, or 0 past the end. -/
def hiAt (T : List Table) (i : Nat) : Nat := (T[i]?.map Table.hi).getD 0

/-- In a sorted level the greatest keys never decrease. -/
theorem sorted_mono {T : List Table} (hT : SortedLevel T) : MonoUpTo (hiAt T) T.length := by
  -- Take the two parts of "sorted level".
  obtain ⟨hrange, hpair⟩ := hT
  -- Take positions i at or before j, with j a real position.
  intro i j hij hj
  -- Then i is a real position too.
  have hi' : i < T.length := by omega
  -- Unfold: the two values are the greatest keys of tables i and j.
  simp only [hiAt, List.getElem?_eq_getElem hi', List.getElem?_eq_getElem hj, Option.map_some,
    Option.getD_some]
  -- i is before j, or i is j.
  rcases Nat.lt_or_eq_of_le hij with h | h
  -- An earlier table ends below where a later one begins.
  ·
    -- Table i ends below where table j begins.
    have := List.pairwise_iff_getElem.mp hpair i j hi' hj h
    -- Table j begins at or before where it ends.
    have := (hrange T[j] (List.getElem_mem hj)).1
    -- So table i ends at or before table j's end.
    omega
  -- i is j:
  · subst h
    -- equal ends.
    exact Nat.le_refl _

/-- The read of a level through binary search: find the first table whose
range ends at or above `k`, and read it if its range starts at or below
`k`. -/
def levelReadB (T : List Table) (k snap : Nat) : Option Nat :=
  -- Look at the table binary search lands on, if there is one.
  match T[bsearch (hiAt T) k 0 T.length]? with
  -- One: read it when its range starts at or below k, else answer nothing.
  | some t => if t.lo ≤ k then newestIn t.src k snap else none
  -- None (the search ran off the end): nothing.
  | none => none

/-- A source holding no version of `k` answers nothing. -/
theorem newestIn_of_absent {src : Source} {k snap : Nat} (h : ∀ s, (k, s) ∉ src) :
    -- The statement: such a source answers `none`.
    newestIn src k snap = none :=
  -- Every version of k it holds is above the snapshot, for lack of any version of k.
  newestIn_eq_none.mpr fun s hs => absurd hs (h s)

/-- When every source but the one at position `r` holds no version of
`k`, a read answers what that one source answers. -/
theorem read_only_at {k snap : Nat} :
    -- The statement: for every list of sources l and position r,
    ∀ (l : List Source) (r : Nat),
      -- if no source at a position other than r holds a version of k,
      (∀ i (hi : i < l.length), i ≠ r → ∀ s, (k, s) ∉ l[i]) →
      -- then the read of l answers what the source at r answers (nothing when r is past the end).
      read l k snap = (match l[r]? with | some src => newestIn src k snap | none => none)
  -- No sources: both sides are `none`.
  | [], _, _ => rfl
  -- A first source and the rest, with r the first position.
  | src :: rest, 0, h => by
    -- Every later source holds nothing of `k`, so the read is the first's.
    have hrest : read rest k snap = none := by
      -- By `read_eq_none`: take a version of k held by some source of the rest.
      refine read_eq_none.mpr fun s ⟨src', hsrc', hs⟩ => ?_
      -- That source sits at some position i of the rest.
      obtain ⟨i, hi, rfl⟩ := List.getElem_of_mem hsrc'
      -- Which is position i + 1 of the whole list, not r: it holds no version of k.
      exact absurd hs (h (i + 1) (by simp; omega) (by omega) s)
    -- Unfold the read one step; the source at position 0 is the first.
    simp only [read, List.getElem?_cons_zero]
    -- Split on the first source's answer.
    cases newestIn src k snap with
    -- It answers v: both sides are `some v`.
    | some v => rfl
    -- It answers nothing: the read is the rest's, which is `none`.
    | none => exact hrest
  -- A first source and the rest, with r not the first position.
  | src :: rest, r + 1, h => by
    -- The first source holds nothing of `k`: the read is the rest's.
    have hfirst : newestIn src k snap = none :=
      -- (position 0 is not r, so the first source holds no version of k)
      newestIn_of_absent fun s => h 0 (by simp) (by omega) s
    -- Unfold the read one step: the first source answers nothing; position r + 1 of the whole list
    -- is position r of the rest.
    simp only [read, hfirst, List.getElem?_cons_succ]
    -- The claim for the rest: a position i of the rest is position i + 1 of the whole list.
    exact read_only_at rest r fun i hi hne s => h (i + 1) (by simp; omega) (by omega) s

/-- **`level_read_bsearch`.** On a level in key order whose tables do not
overlap, the read through binary search answers what a linear read of
every table in order answers. -/
theorem level_read_bsearch {T : List Table} (hT : SortedLevel T) (k snap : Nat) :
    -- The statement: the read through binary search equals the linear read of the level's tables.
    levelReadB T k snap = read (T.map Table.src) k snap := by
  -- Binary search returns the first table whose range ends at or above k.
  have hspec := bsearch_spec (hiAt T) k (sorted_mono hT) 0 T.length (Nat.zero_le _) (Nat.le_refl _)
  -- Name that position r.
  generalize hr : bsearch (hiAt T) k 0 T.length = r at hspec
  -- Take the facts about r: it is within the level; every table before it ends below k; and if r is
  -- a real position, its table reaches k.
  obtain ⟨-, hrle, hbelow, hat⟩ := hspec
  -- Take the two parts of "sorted level".
  obtain ⟨hrange, hpair⟩ := hT
  -- The greatest key of a table in the level, by position.
  have hiAt_eq : ∀ i (hi : i < T.length), hiAt T i = T[i].hi := by
    -- Take a real position i.
    intro i hi
    -- Unfold: the stored value is table i's greatest key.
    simp [hiAt, List.getElem?_eq_getElem hi]
  -- No table before `r` holds `k`: its keys end below `k`.
  have hbefore : ∀ i (hi : i < T.length), i < r → ∀ s, (k, s) ∉ T[i].src := by
    -- Take a table before r and a version (k, s) in it.
    intro i hi hir s hs
    -- That table ends below k.
    have h1 := hbelow i (Nat.zero_le _) hir
    -- State it with the table's own greatest key.
    rw [hiAt_eq i hi] at h1
    -- But the table's range holds every key of the table, k included.
    have := ((hrange T[i] (List.getElem_mem hi)).2 _ hs).2
    -- Simplify what "the key of (k, s)" means.
    simp only at this
    -- So k is at most the table's end, which is below k: arithmetic says no.
    omega
  -- No table after `r` holds `k`: its keys start above table `r`'s end,
  -- which reaches `k`.
  have hafter : ∀ i (hi : i < T.length), r < i → ∀ s, (k, s) ∉ T[i].src := by
    -- Take a table after r and a version (k, s) in it.
    intro i hi hri s hs
    -- Then r is a real position.
    have hrl : r < T.length := by omega
    -- So table r reaches k.
    have h1 := hat hrl
    -- State it with table r's own greatest key.
    rw [hiAt_eq r hrl] at h1
    -- Table r ends below where table i begins.
    have h2 := List.pairwise_iff_getElem.mp hpair r i hrl hi hri
    -- And table i's range holds k.
    have := ((hrange T[i] (List.getElem_mem hi)).2 _ hs).1
    -- Simplify what "the key of (k, s)" means.
    simp only at this
    -- So table i begins at or below k, yet above table r's end, which reaches k: arithmetic says
    -- no.
    omega
  -- So only table `r` can answer, and a linear read answers what it does.
  have honly : ∀ i (hi : i < (T.map Table.src).length), i ≠ r → ∀ s, (k, s) ∉ (T.map Table.src)[i] := by
    -- Take a position i other than r, and a sequence s.
    intro i hi hne s
    -- The list of sources is as long as the level.
    simp only [List.length_map] at hi
    -- Source i is table i's versions.
    simp only [List.getElem_map]
    -- i is before r, or after r.
    rcases Nat.lt_or_gt_of_ne hne with h | h
    -- Before: no version of k, as shown.
    · exact hbefore i hi h s
    -- After: no version of k, as shown.
    · exact hafter i hi h s
  -- So the linear read answers what source r answers.
  rw [read_only_at _ r honly]
  -- Unfold the read through binary search.
  unfold levelReadB
  -- Its position is r; source r is table r's versions.
  rw [hr, List.getElem?_map]
  -- Split on whether position r holds a table.
  cases hT : T[r]? with
  -- None: both sides answer nothing.
  | none => rfl
  -- One, t:
  | some t =>
    -- unfold both sides for that table.
    simp only [Option.map_some]
    -- Split on whether t's range starts at or below k.
    split
    -- It does: both sides read t.
    · rfl
    -- Table `r`'s range starts above `k`, so it holds no version of `k`.
    ·
      -- Name that fact hlo.
      rename_i hlo
      -- t is a table of the level.
      have hmem : t ∈ T := List.mem_of_getElem? hT
      -- The left side answers nothing, and so does reading t: suppose t held a version (k, s);
      exact (newestIn_of_absent fun s hs => by
        -- t's range holds k, so it starts at or below k;
        have := ((hrange t hmem).2 _ hs).1
        -- simplify what "the key of (k, s)" means;
        simp only at this
        -- but it starts above k: arithmetic says no.
        omega).symm

-- The end of this file's names.
end Regolith.LsmOrder
