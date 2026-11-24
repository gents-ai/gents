/-!
# SnapshotRegistry: per-thread slots, counted entries, announce, sample, confirm

THE STORY. A database keeps old versions of a key while a reader still
needs them. A reader takes a snapshot: it writes down the newest commit
number it may see (the horizon) and reads only versions at or below it. A
compaction throws away versions nobody can see, so it must find every live
snapshot. Tiny example of the danger: the horizon is 0, reader A reads 0, a
commit makes the horizon 1, a compaction finds no snapshot and deletes the
version at 0, then A registers at 0 and reads a version that is gone.

THE CODE (`src/engine/snapshot_registry.rs` and
`src/engine/snapshot_registry/chain.rs`). No lock. Every
thread number owns a slot; a slot has entries; an entry holds one sequence
and a count of the pins on it. A reader announces the horizon `v` it read in
an entry of its slot (a new entry, or one more count on an entry that holds
`v`), then a fence, then reads the horizon again: still `v` means live at
`v`, otherwise it takes its count back and tries the new value. A copy of a
snapshot adds one count to the original's entry. A release takes one count
off the entry its pin recorded, whatever thread drops it. A compaction
fences (its sample), then reads every entry.

WHAT IS PROVED HERE, for every interleaving, by naming the moment of each
access (a moment is a natural number; the horizon at moment `t` is `H t`
and only rises):

1. `held_keeps_seq`: an entry's sequence does not change while some pin
   holds it.
2. `scan_respects_live`: a scan's minimum is at most a live snapshot, and
   the snapshot is either in what the scan read or at or above the sampled
   horizon. Backs `MinBelowLive` and `LiveCovered` in
   `proofs/tla/SnapshotRegistry.tla`.
3. `clone_respects_live`: the same for a copy that joined the original's
   entry, from the moment the original was announced.
4. `pins_exact`: an entry's count always equals the live pins recorded at
   it, whichever thread released each one. Backs `PinsExact`.
5. The RED cases as counterexamples: `no_confirm_breaks_min`,
   `fresh_clone_breaks_cover`, `release_elsewhere_breaks_count`.

Memory ordering is sequentially consistent here: each access happens at one
moment. The real accesses get that from two SeqCst fences, which loom checks
(`tests/loom_snapshots.rs`).
-/

-- Everything below lives under this name, so it cannot clash with other files.
namespace Regolith.SnapshotRegistry

/-- `Rises H`: the horizon never goes down. A later moment never shows a
smaller horizon: commits only add. -/
def Rises (H : Nat → Nat) : Prop := ∀ i j, i ≤ j → H i ≤ H j

/-- One entry of one slot, over time: `chain::Entry`.

* `seq t` is the sequence it holds at moment `t` (`Entry::seq`).
* `pins t` is how many pins hold it at moment `t` (`Entry::pins`).

The code's rule E1: while a pin holds the entry, nobody writes its
sequence. Here: if at least one pin holds it at `t`, the sequence at the
next moment is the same. -/
structure Entry where
  /-- The sequence the entry holds at each moment. -/
  seq : Nat → Nat
  /-- How many pins hold the entry at each moment. -/
  pins : Nat → Nat
  /-- E1: a held entry keeps its sequence for the next moment. -/
  stable : ∀ t, 1 ≤ pins t → seq (t + 1) = seq t

/-- What a scan that reads the entry at moment `t` sees: its sequence when
some pin holds it, nothing when it is free (`Chunk::each_announced` skips a
count of 0). -/
def Entry.reads (e : Entry) (t : Nat) : Option Nat :=
  -- Held: the scan lists the sequence. Free: the scan skips it.
  if 1 ≤ e.pins t then some (e.seq t) else none

/-- **`held_keeps_seq`.** If pins hold the entry at every moment from `a`
up to (not including) `a + n`, its sequence at `a + n` is the one it had
at `a`. Example ruled out: a snapshot announced at 5 whose entry says 7 a
moment later while the snapshot still holds it. -/
theorem held_keeps_seq (e : Entry) (a : Nat) :
    -- The statement: for every n, if the entry is held at every moment from a up to (not including)
    -- a + n, its sequence at a + n is the one at a.
    ∀ n, (∀ t, a ≤ t → t < a + n → 1 ≤ e.pins t) → e.seq (a + n) = e.seq a := by
  -- Count the moments one at a time, from zero upward.
  intro n
  -- Prove it for every `n` by induction: true for 0, and each next step.
  induction n with
  -- Case: n is 0.
  | zero =>
    -- No moment has passed: `a + 0` is `a`, so both sides are the same.
    intro _
    -- Both sides are the same thing.
    rfl
  -- Case: n is k + 1, with the claim for k (ih) known.
  | succ k ih =>
    -- Assume pins hold the entry over the first `k + 1` moments.
    intro hheld
    -- Over the first `k` moments too, so by the induction the sequence at
    -- `a + k` is still the one at `a`.
    have hk : e.seq (a + k) = e.seq a := ih (fun t h1 h2 => hheld t h1 (by omega))
    -- At moment `a + k` itself a pin holds the entry.
    have hp : 1 ≤ e.pins (a + k) := hheld (a + k) (by omega) (by omega)
    -- So by E1 the next moment keeps the sequence, which is the one at `a`.
    rw [show a + (k + 1) = (a + k) + 1 by omega, e.stable _ hp, hk]

/-- One live snapshot and the facts that protect it.

* `v` is the snapshot's sequence.
* `tS` is the announce that put `v` in the entry with a count; for a copy
  it is the original's announce.
* `tC` is the confirming read of the horizon, after a fence, that saw `v`.
* `tRel` is when the snapshot is released.
* From `tS` until `tRel` at least one pin holds the entry: its own count
  or, for a copy, the original's until the copy's count is in. -/
structure Pin (H : Nat → Nat) where
  /-- The entry the snapshot's count is on. -/
  entry : Entry
  /-- The snapshot's sequence. -/
  v : Nat
  /-- The announce: when `v` landed in the entry with a count. -/
  tS : Nat
  /-- The confirming read of the horizon. -/
  tC : Nat
  /-- The release. -/
  tRel : Nat
  /-- The confirm came after the announce (the fence sits between them). -/
  announce_before_confirm : tS < tC
  /-- The confirm read `v`: the horizon had not moved. -/
  confirmed : H tC = v
  /-- At the announce the entry held `v`. -/
  announced : entry.seq tS = v
  /-- From the announce until the release, a pin holds the entry. -/
  held : ∀ t, tS ≤ t → t < tRel → 1 ≤ entry.pins t

/-- While the snapshot is not released, a scan that reads its entry at or
after the announce sees `v`. -/
theorem Pin.reads_v {H : Nat → Nat} (p : Pin H) :
    -- The statement: at every moment from the announce up to (not including) the release, a scan of
    -- the entry reads v.
    ∀ t, p.tS ≤ t → t < p.tRel → p.entry.reads t = some p.v := by
  -- Take a moment `t` in the protected stretch.
  intro t h1 h2
  -- Write `t` as the announce plus some number of moments.
  obtain ⟨n, rfl⟩ : ∃ n, t = p.tS + n := ⟨t - p.tS, by omega⟩
  -- The entry is held at `t`, so a scan lists its sequence...
  have hp : 1 ≤ p.entry.pins (p.tS + n) := p.held _ h1 h2
  -- ...and that sequence is still the one announced, `v`.
  have hs : p.entry.seq (p.tS + n) = p.v := by
    -- A held entry keeps its sequence since the announce, and the announce wrote v.
    rw [held_keeps_seq p.entry p.tS n (fun t' a b => p.held t' a (by omega)), p.announced]
  -- Unfold what a scan reads and use both facts.
  simp [Entry.reads, hp, hs]

/-- One compaction's view of one entry: it sampled the horizon at `tA` (its
fence) and read the entry at `tB`. -/
structure Scan where
  /-- When the compaction sampled the horizon. -/
  tA : Nat
  /-- When it read this entry. -/
  tB : Nat
  /-- The sample came first: `live_seqs` fences, then scans. -/
  sample_first : tA < tB

/-- The minimum the compaction may use, as far as this entry decides it:
the least of the sampled horizon and what the scan saw there. -/
def minimum (H : Nat → Nat) (p : Pin H) (sc : Scan) : Nat :=
  -- The scan saw some value: the minimum cannot exceed it or the sample.
  match p.entry.reads sc.tB with
  -- It saw the value w: the smaller of the sampled horizon and w.
  | some w => min (H sc.tA) w
  -- It saw a free entry: only the sample bounds the minimum.
  | none => H sc.tA

/-- **`scan_respects_live`.** Let the compaction use its minimum at moment
`tU`, after its scan read the entry and before the snapshot is released.
Then the minimum is at most `v`, and the scan saw `v` or the sample is at
most `v`. Whether the snapshot became live before `tU` or after does not
matter. Example ruled out: the snapshot at 0, the minimum 1. -/
theorem scan_respects_live {H : Nat → Nat}
    -- The horizon only rises.
    (hH : Rises H) (p : Pin H) (sc : Scan) (tU : Nat)
    -- The snapshot is not yet released when the compaction uses the list...
    (hlive : tU < p.tRel)
    -- ...and the compaction uses it after its scan.
    (huse : sc.tB ≤ tU) :
    -- The statement: the minimum is at most v; and the scan saw v in the entry, or the sampled
    -- horizon is at most v.
    minimum H p sc ≤ p.v ∧ (p.entry.reads sc.tB = some p.v ∨ H sc.tA ≤ p.v) := by
  -- Two cases: the scan read the entry after the announce, or before.
  by_cases hB : p.tS ≤ sc.tB
  -- After: the entry held `v` and the scan saw it.
  ·
    -- A scan of the entry inside the protected stretch reads v.
    have hread := p.reads_v sc.tB hB (by omega)
    -- The scan listed `v`, which is the second half.
    refine ⟨?_, Or.inl hread⟩
    -- The minimum is a `min` with `v` in it...
    simp only [minimum, hread]
    -- ...so it is at most `v`.
    exact Nat.min_le_right _ _
  -- Before: the sample came before the scan, which came before the announce, which came before the
  -- confirm. The horizon only rises, and the confirm read `v`, so the sample read at most `v`.
  ·
    -- The sampled horizon is at most v:
    have hc : H sc.tA ≤ p.v := by
      -- Replace `v` by the horizon at the confirm.
      rw [← p.confirmed]
      -- The sample is earlier than the confirm.
      exact hH _ _ (by have := sc.sample_first; have := p.announce_before_confirm; omega)
    -- The sample bounds `v` from below, which is the second half.
    refine ⟨?_, Or.inr hc⟩
    -- Open up the minimum.
    unfold minimum
    -- Whatever the scan saw, the minimum is at most the sample.
    split
    -- It saw some value: the `min` is at most the sample, at most `v`.
    ·
      -- The smaller of two numbers is at most the first, the sample; and the sample is at most v.
      exact Nat.le_trans (Nat.min_le_left _ _) hc
    -- It saw nothing: the minimum is the sample, at most `v`.
    ·
      -- Exactly the bound just shown.
      exact hc

/-- A copy of the live snapshot `p`, made at `tJ` while `p` still held its
count (`p.tS ≤ tJ < p.tRel`), whose own count sits on the same entry from
`tJ` until its release `tRel2`. Its protection starts at the original's
announce and confirm. -/
def Pin.copy {H : Nat → Nat} (p : Pin H) (tJ tRel2 : Nat)
    -- A copy is made while the original lives. The first half is how the
    -- code runs (the original exists before it is copied); the proof only
    -- needs the second: the copy's count arrives before the original's leaves.
    (_hJ : p.tS ≤ tJ) (hJr : tJ < p.tRel)
    -- The copy's own count: the entry is held at every moment from the copy to the copy's release.
    (hcopy : ∀ t, tJ ≤ t → t < tRel2 → 1 ≤ p.entry.pins t) : Pin H :=
  -- Everything is the original's except the release.
  { p with
    tRel := tRel2
    -- Held throughout: before the original's release by the original's
    -- count, after it by the copy's, which arrived before the original left.
    held := fun t h1 h2 => by
      by_cases ht : t < p.tRel
      · exact p.held t h1 ht
      · exact hcopy t (by omega) h2 }

/-- **`clone_respects_live`.** A copy that joined its original's entry is
protected exactly like a registered snapshot: the scan's minimum is at most
it, and it was seen or is at or above the sample. Example ruled out: the
original is dropped and a scan that read its entry afterwards and the
copy's slot before misses the copy (that is the RED `CloneFresh`, which
does not join). -/
theorem clone_respects_live {H : Nat → Nat} (hH : Rises H) (p : Pin H)
    -- The moment the copy is made and the moment the copy is released; the copy is made while the
    -- original is live.
    (tJ tRel2 : Nat) (hJ : p.tS ≤ tJ) (hJr : tJ < p.tRel)
    -- The copy's own count holds the entry from then until its release.
    (hcopy : ∀ t, tJ ≤ t → t < tRel2 → 1 ≤ p.entry.pins t)
    -- A scan, and the moment the compaction uses its list: before the copy's release and after the
    -- scan.
    (sc : Scan) (tU : Nat) (hlive : tU < tRel2) (huse : sc.tB ≤ tU) :
    -- q is the copy, seen as a pin of its own.
    let q := p.copy tJ tRel2 hJ hJr hcopy
    -- The statement: the minimum is at most the copy's sequence; and the scan saw it, or the
    -- sampled horizon is at most it.
    minimum H q sc ≤ q.v ∧ (q.entry.reads sc.tB = some q.v ∨ H sc.tA ≤ q.v) :=
  -- The copy is a `Pin`, so the general theorem applies to it.
  scan_respects_live hH _ sc tU hlive huse

/-! ## Counting pins: a release goes where its pin is recorded -/

/-- What one step does to the counts. A place is a natural number naming
one entry anywhere in the registry (its slot and index). -/
inductive Op where
  /-- A pin lands on place `p`: a claim, a join or a copy. -/
  | add (p : Nat)
  /-- Thread `t` drops a handle whose pin recorded place `p`. -/
  | release (t : Nat) (p : Nat)

/-- The fix: a count goes onto, or off, the place the pin recorded, and the
releasing thread plays no part (`SnapshotRegistry::release`). -/
def step (c : Nat → Nat) : Op → (Nat → Nat)
  -- One more count at `p`; every other place unchanged.
  | .add p => fun q => if q = p then c q + 1 else c q
  -- One count fewer at `p`; every other place unchanged.
  | .release _ p => fun q => if q = p then c q - 1 else c q

/-- The live pins, as the list of places they recorded: an add puts one in,
a release takes its own out. -/
def live (l : List Nat) : Op → List Nat
  -- A new pin at `p`.
  | .add p => p :: l
  -- The released pin, at `p`, is gone.
  | .release _ p => l.erase p

/-- Run the ops in order: the counts and the live pins at the end. -/
def run (c : Nat → Nat) (l : List Nat) : List Op → (Nat → Nat) × List Nat
  -- No more ops: where we are.
  | [] => (c, l)
  -- One op, then the rest.
  | op :: ops => run (step c op) (live l op) ops

/-- **`pins_exact`.** Start with every count equal to the live pins at its
place; after any ops, it still is, at every place. A pin created on one
thread and released on another is no different: the release names the
place, not the thread. Example ruled out: an entry whose count falls to 0
while a live pin still records it, so a scan skips a live snapshot. -/
theorem pins_exact : ∀ (ops : List Op) (c : Nat → Nat) (l : List Nat),
    (∀ q, c q = l.count q) → ∀ q, (run c l ops).1 q = (run c l ops).2.count q := by
  -- Induct on the ops, for every starting state.
  intro ops
  -- Induction on the list of operations: first none, then one more in front.
  induction ops with
  -- Case: no operation.
  | nil =>
    -- No ops: the start already agrees.
    intro c l h q
    -- The starting counts agree, by assumption.
    exact h q
  -- Case: an operation op, then the rest.
  | cons op ops ih =>
    -- One op, then the rest: show the op keeps the counts exact, then use
    -- the induction for the rest.
    intro c l h
    -- By the claim for the rest, it is enough that the counts agree after op.
    apply ih
    -- Take any place `q`.
    intro q
    -- What the op is.
    cases op with
    -- Case: op adds a pin at place p.
    | add p =>
      -- An add: the count at `p` and the list both gain one at `p`.
      simp only [step, live, List.count_cons, h q]
      -- Whether `q` is `p` decides both sides the same way.
      by_cases hq : q = p
      -- q is p: both sides gain one.
      · subst hq; simp
      -- q is another place: neither side changes.
      · simp [hq, Ne.symm hq]
    -- Case: op releases a pin recorded at place p.
    | release t p =>
      -- A release: both lose one at `p` (never below zero).
      simp only [step, live, List.count_erase, h q]
      -- Again `q` is `p` or not.
      by_cases hq : q = p
      -- q is p: both sides lose one.
      · subst hq; simp
      -- q is another place: neither side changes.
      · simp [hq, Ne.symm hq]

/-- A release of a live pin never takes a count below zero: while the
counts are exact, the place of a live pin has a count of at least 1. -/
theorem release_has_a_count (c : Nat → Nat) (l : List Nat) (p : Nat)
    -- The counts are exact, and a live pin is recorded at p.
    (h : ∀ q, c q = l.count q) (hp : p ∈ l) : 1 ≤ c p := by
  -- The count is the number of live pins at `p`, and one of them is live.
  rw [h p]
  -- A member of a list is counted at least once.
  exact List.count_pos_iff.mpr hp

/-! ## The RED cases -/

/-- The horizon in the counterexamples: 0 at moment 0, then 1. -/
def redH (t : Nat) : Nat := if 1 ≤ t then 1 else 0

/-- The reader's entry in RED `NoConfirm`: empty until it stores, at
moment 3, the value 0 it read at moment 0. -/
def redSlot (t : Nat) : Option Nat := if 3 ≤ t then some 0 else none

/-- **`no_confirm_breaks_min`.** The reader reads the horizon 0 at moment 0;
the horizon rises to 1 at moment 1; the compaction samples 1 at moment 1
and reads the still-empty entry at moment 2; the reader stores 0 at moment
3 and, without a confirming read, is live at 0. The compaction's minimum is
1, above the snapshot. With the confirm, every later read gives 1, not 0,
so the reader announces again instead. -/
theorem no_confirm_breaks_min :
    -- The statement, seven facts about the example: the horizon only rises; it starts at 0; the
    -- reader's entry is free at moment 2 and holds 0 at moment 3;
    Rises redH ∧ redH 0 = 0 ∧ redSlot 2 = none ∧ redSlot 3 = some 0 ∧
    -- the horizon is 1 at moment 1, which is above 0; and after moment 3 it is never 0 again.
    redH 1 = 1 ∧ 0 < redH 1 ∧ (∀ t, 3 < t → redH t ≠ 0) := by
  -- Seven facts: the two that need an argument first, the rest by computing.
  refine ⟨?_, by decide, by decide, by decide, by decide, by decide, ?_⟩
  -- `redH` is 0 then 1: it never goes down.
  ·
    -- Take two moments, i no later than j.
    intro i j hij
    -- Unfold it and compare the cases.
    unfold redH
    -- Four cases, by which side of the change each moment is on; each is arithmetic.
    split <;> split <;> omega
  -- After moment 3 the horizon is 1, never 0.
  ·
    -- Take a moment t after 3.
    intro t ht
    -- Unfold the example horizon.
    unfold redH
    -- Two cases, by which side of the change t is on; each is arithmetic.
    split <;> omega

/-- The original's entry in RED `CloneFresh`: it holds 0 with one count
until the original is released at moment 4. -/
def origEntry : Entry where
  -- Always 0.
  seq := fun _ => 0
  -- One count before moment 4, none after.
  pins := fun t => if t < 4 then 1 else 0
  -- The sequence never changes, so E1 holds trivially.
  stable := fun _ _ => rfl

/-- The copy's fresh entry in RED `CloneFresh`: free until the copy claims it
at moment 3 with sequence 0 and one count. -/
def copyEntry : Entry where
  -- 0, the original's sequence.
  seq := fun _ => 0
  -- No count before moment 3, one after.
  pins := fun t => if 3 ≤ t then 1 else 0
  -- The sequence never changes, so E1 holds trivially.
  stable := fun _ _ => rfl

/-- **`fresh_clone_breaks_cover`.** The original is live at 0. The horizon
rises to 1 at moment 1 and the compaction samples 1 then. It reads the
copy's entry at moment 2, still free; the copy claims it at moment 3 with no
confirm; the original is released at moment 4; the compaction reads the
original's entry at moment 5, now free. The copy is live at 0 from moment 3
on, yet the scan saw it nowhere and the sample, 1, is above it. Joining the
original's entry instead (`clone_respects_live`) keeps 0 in view. -/
theorem fresh_clone_breaks_cover :
    -- The statement, five facts about the example: the horizon is 1 at moment 1; the copy's entry
    -- is free at moment 2; the original's entry is free at moment 5;
    redH 1 = 1 ∧ copyEntry.reads 2 = none ∧ origEntry.reads 5 = none ∧
    -- the copy's entry holds 0 at moment 6; and the sampled horizon, 1, is not at most 0.
    copyEntry.reads 6 = some 0 ∧ ¬ (redH 1 ≤ 0) := by
  -- Every fact is a small computation.
  refine ⟨by decide, ?_, ?_, ?_, by decide⟩ <;> simp [Entry.reads, copyEntry, origEntry]

/-- RED `ReleaseHere`: the release goes to the place at the same index in the
releasing thread's slot, here named by `here`. -/
def stepHere (here : Nat → Nat → Nat) (c : Nat → Nat) : Op → (Nat → Nat)
  -- Adds are the same as the fix.
  | .add p => fun q => if q = p then c q + 1 else c q
  -- A release by thread `t` goes to `here t p`, not to `p`.
  | .release t p => fun q => if q = here t p then c q - 1 else c q

/-- The RED's place map: thread 2's slot holds places 2 and 3, so place 0
(slot 1, index 0) released on thread 2 lands on place 2. -/
def redHere (t p : Nat) : Nat := if t = 2 then 2 + p % 2 else p

/-- The counts after the RED run: pin A at place 0 on thread 1, pin B at
place 2 on thread 2; A moves to thread 2 and is released there. -/
def redCounts : Nat → Nat :=
  -- Start with no counts, add A, add B, release A on thread 2.
  stepHere redHere (stepHere redHere (stepHere redHere (fun _ => 0) (.add 0)) (.add 2))
    -- (The last operation: the release of A, done on thread 2.)
    (.release 2 0)

/-- **`release_elsewhere_breaks_count`.** After that run, B is the only live
pin and it records place 2, but place 2's count is 0: a scan skips B's
entry while B is live. Place 0 keeps A's stale count forever. -/
theorem release_elsewhere_breaks_count :
    -- The statement: place 2's count is 0,
    redCounts 2 = 0 ∧
      -- although exactly one live pin is recorded at place 2,
      (live (live (live [] (.add 0)) (.add 2)) (.release 2 0)).count 2 = 1 ∧
      -- and place 0 keeps a count of 1.
      redCounts 0 = 1 := by
  -- Every fact is a small computation.
  decide

-- End of the namespace opened at the top.
end Regolith.SnapshotRegistry
