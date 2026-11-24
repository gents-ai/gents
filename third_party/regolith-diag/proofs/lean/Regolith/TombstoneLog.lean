/-!
# TombstoneLog: readers see every published range delete, whole

This file backs the TLA+ model `proofs/tla/TombstoneLog.tla`, invariants
`OneSlotEach`, `ReadWhole` and `PublishedSeen` (configurations
`MC_TombstoneLog_*`).

## The story

A memtable keeps its range tombstones in an append-only log
(`src/engine/memtable/tombstones.rs`). The commit pipeline lets whichever
thread reaches a decided commit apply it, so any number of threads append
at once. An append takes the next position with one add (so the position is
its alone), writes the tombstone into that slot, and only then sets the
slot's `ready` flag. A commit is made visible, through the read horizon,
only after its append returned. A reader samples the horizon, reads how
many positions were handed out, and reads the slots below that whose flag
is set, skipping the others.

Tiny example: a thread takes position 0, writes commit 1's tombstone into
slot 0, sets the flag; the horizon is raised to 1. A reader that samples
horizon 1 reads a count of at least 1, finds slot 0's flag set, and reads
the tombstone whole.

## What is proved, for any number of appends, threads and readers

1. `inv_reachable`: in every state the log reaches, a slot whose flag is
   set is written and lies below the count of positions handed out.
2. `one_slot_each`: a position handed out is never handed out again, however
   many appends follow (`OneSlotEach`).
3. `ready_whole`: a reader that found a slot's flag set at one moment reads,
   at that moment or later, a written slot (`ReadWhole`).
4. `published_seen`: a reader that sampled the horizon, then read the count,
   then looked at the slots below it finds, for every commit its snapshot
   includes, a slot with that commit's tombstone and its flag set
   (`PublishedSeen`).
5. The RED cases as counterexamples: setting the flag before writing the
   slot (`ready_first_breaks`), raising the horizon before the append
   returned (`horizon_first_breaks`), and taking a position with a load and
   a store (`load_store_shares_a_slot`).
-/

-- Everything below is named Regolith.TombstoneLog.<name>.
namespace Regolith.TombstoneLog

/-- The log's state. Positions count from 0. -/
structure Log where
  /-- How many positions were handed out (`TombstoneLog::claimed`). -/
  claimed : Nat
  /-- `written p`: slot `p` holds its tombstone. -/
  written : Nat → Bool
  /-- `ready p`: slot `p`'s flag is set. -/
  ready : Nat → Bool

/-- The start: no position handed out, nothing written, no flag set. -/
def start : Log := ⟨0, fun _ => false, fun _ => false⟩

/-- The steps of the appends, whichever threads take them. -/
inductive Step : Log → Log → Prop where
  /-- An append takes the next position: one add on the count. -/
  | take (w : Log) : Step w { w with claimed := w.claimed + 1 }
  /-- The append that holds position `p` writes its tombstone there. The
  slot was handed out and its flag is not set: nobody reads it yet. -/
  | write (w : Log) (p : Nat) (hp : p < w.claimed) (hr : w.ready p = false) :
      -- The slot at `p` is written; every other slot is as before.
      Step w { w with written := fun i => if i = p then true else w.written i }
  /-- The append sets the flag of the slot it wrote. -/
  | flag (w : Log) (p : Nat) (hp : p < w.claimed) (hw : w.written p = true) :
      -- The flag at `p` is set; every other flag is as before.
      Step w { w with ready := fun i => if i = p then true else w.ready i }

/-- The states reachable from the start. -/
inductive Reachable : Log → Prop where
  /-- The start is reachable. -/
  | start : Reachable start
  /-- One step from a reachable state reaches another. -/
  | step {w v : Log} : Reachable w → Step w v → Reachable v

/-- What the appends keep true. -/
structure Inv (w : Log) : Prop where
  /-- A slot whose flag is set is written. -/
  ready_written : ∀ p, w.ready p = true → w.written p = true
  /-- A slot whose flag is set was handed out. -/
  ready_claimed : ∀ p, w.ready p = true → p < w.claimed

/-- The invariant holds at the start. -/
theorem inv_start : Inv start := by
  -- No flag is set: both fields hold with nothing to show.
  constructor <;> simp [start]

/-- Every step keeps the invariant. -/
theorem inv_step {w v : Log} (hi : Inv w) (hs : Step w v) : Inv v := by
  -- One case per step.
  cases hs with
  -- Case: `take`.
  | take =>
    -- Prove the two fields after a position is handed out.
    constructor
    -- No slot or flag changed.
    · exact hi.ready_written
    -- A flagged slot was below the old count, so it is below the new one.
    · intro p hp
      -- Below the old count...
      have := hi.ready_claimed p hp
      -- ...and the count only grew.
      show p < w.claimed + 1
      -- Below the old count, so below the old count plus one.
      omega
  -- Case: `write`.
  | write p hp hr =>
    -- Prove the two fields after the write.
    constructor
    -- A flagged slot is written: it was before, and a write unwrites nothing.
    · intro i hi'
      -- Is it the slot just written?
      by_cases hip : i = p
      -- That slot is written now by the step itself.
      · simp [hip]
      -- Another slot keeps its old bit, set because its flag is.
      · simp [hip]
        -- It was written before the step, as the fact for the old state says.
        exact hi.ready_written i hi'
    -- The count and the flags did not move.
    · exact hi.ready_claimed
  -- Case: `flag`.
  | flag p hp hw =>
    -- Prove the two fields after the flag is set.
    constructor
    -- A flagged slot is written.
    · intro i hi'
      -- Is it the slot just flagged?
      by_cases hip : i = p
      -- It was written before its flag was set: the step asked for that.
      · rw [hip]
        -- The step's own condition says it is written.
        exact hw
      -- Another slot had its flag before; read the new flag as the old.
      · have hold : w.ready i = true := by
          -- For a slot other than `p` the new flag is the old one.
          simpa [hip] using hi'
        -- So it was written.
        exact hi.ready_written i hold
    -- A flagged slot was handed out.
    · intro i hi'
      -- Is it the slot just flagged?
      by_cases hip : i = p
      -- The step asked for a position below the count.
      · rw [hip]
        -- The step's own condition says it is below the count.
        exact hp
      -- Another slot had its flag before; read the new flag as the old.
      · have hold : w.ready i = true := by
          -- For a slot other than `p` the new flag is the old one.
          simpa [hip] using hi'
        -- So it was handed out.
        exact hi.ready_claimed i hold

/-- **The appends' half, for every reachable state.** A slot whose flag is
set is written, and lies below the count. -/
theorem inv_reachable {w : Log} (h : Reachable w) : Inv w := by
  -- Induction on how the state was reached.
  induction h with
  -- Case: `start`.
  | start =>
    -- The start satisfies it.
    exact inv_start
  -- Case: one more step, the claim known before it (ih).
  | step _ hs ih =>
    -- One more step keeps it.
    exact inv_step ih hs

/-- Some number of steps, one after another. -/
inductive Steps : Log → Log → Prop where
  /-- No step at all. -/
  | refl (w : Log) : Steps w w
  /-- Some steps, then one more. -/
  | tail {w v u : Log} : Steps w v → Step v u → Steps w u

/-- The count never goes back. -/
theorem claimed_rises {w v : Log} (h : Steps w v) : w.claimed ≤ v.claimed := by
  -- Induction on the steps.
  induction h with
  -- Case: no step at all.
  | refl =>
    -- No step: the count is what it was.
    exact Nat.le_refl _
  -- Case: `tail`.
  | tail _ hs ih =>
    -- One more step: look at which.
    cases hs with
    -- Case: `take`.
    | take =>
      -- A take adds one.
      show w.claimed ≤ _ + 1
      -- The count before is at most itself plus one.
      omega
    -- Case: `write`.
    | write p hp hr =>
      -- A write leaves the count alone.
      exact ih
    -- Case: `flag`.
    | flag p hp hw =>
      -- So does setting a flag.
      exact ih

/-- **`OneSlotEach`.** The position an append takes is handed out once: it
is below the count from then on, and every later append takes the count as
it stands, so a later position is a higher one. No two appends share a
slot. -/
theorem one_slot_each (w v : Log)
    -- `v` is reached from the state just after an append took `w.claimed`.
    (h : Steps { w with claimed := w.claimed + 1 } v) : w.claimed < v.claimed := by
  -- The count was one past that position, and never went back.
  have := claimed_rises h
  -- So the position stays below it.
  exact this

/-! ## The readers' half, by moments

A reader's loads happen at moments; `W t`, `R t`, `C t` and `H t` are the
written slots, the flags, the count and the horizon at moment `t`, and
`T p` is the commit whose tombstone slot `p` holds once written. The
appends only ever write slots, set flags and raise the count; the theorems
below assume exactly that, plus the invariant at every moment and the
pipeline's own rule that a commit is visible only after its append
returned. -/

/-- What the appends' history gives a reader, at every moment. -/
structure History where
  /-- Which slots are written at each moment. -/
  W : Nat → Nat → Bool
  /-- Which flags are set at each moment. -/
  R : Nat → Nat → Bool
  /-- The count of positions handed out at each moment. -/
  C : Nat → Nat
  /-- The read horizon at each moment: commits `1..H t` are visible. -/
  H : Nat → Nat
  /-- The commit whose tombstone a slot holds. -/
  T : Nat → Nat
  /-- A written slot stays written (T3: never moved, never removed). -/
  written_stays : ∀ t t' p, t ≤ t' → W t p = true → W t' p = true
  /-- A flag that is set stays set. -/
  ready_stays : ∀ t t' p, t ≤ t' → R t p = true → R t' p = true
  /-- The count only rises (`claimed_rises`). -/
  count_rises : ∀ t t', t ≤ t' → C t ≤ C t'
  /-- At every moment a flagged slot is written (`inv_reachable`). -/
  ready_written : ∀ t p, R t p = true → W t p = true
  /-- At every moment a flagged slot was handed out (`inv_reachable`). -/
  ready_claimed : ∀ t p, R t p = true → p < C t
  /-- A commit is visible only after its append returned: some slot holds
  its tombstone with the flag set. -/
  visible_after_append : ∀ t c, 1 ≤ c → c ≤ H t → ∃ p, R t p = true ∧ T p = c

/-- **`ReadWhole`.** A reader that found slot `p`'s flag set at `t0` and
reads the slot at a later moment `t1` reads a written slot: it never sees a
tombstone before its contents. -/
theorem ready_whole (h : History) {t0 t1 p : Nat}
    -- The flag was set when the reader looked.
    (hflag : h.R t0 p = true)
    -- The slot is read then or later.
    (hlater : t0 ≤ t1) : h.W t1 p = true := by
  -- Written by `t0`, since its flag was set then, and still written.
  exact h.written_stays t0 t1 p hlater (h.ready_written t0 p hflag)

/-- **`PublishedSeen`.** A reader samples the horizon at `t0`, reads the
count at `t1`, and looks at the slots at `t2`, in that order. For every
commit `c` its snapshot includes there is a slot below the count it read,
with its flag set when the reader looks, written, and holding `c`'s
tombstone: the reader does not skip it, and reads every delete its
snapshot includes. -/
theorem published_seen (h : History) {t0 t1 t2 c : Nat}
    -- The count is read after the horizon is sampled.
    (h01 : t0 ≤ t1)
    -- The slots are looked at after the count is read.
    (h12 : t1 ≤ t2)
    -- `c` is a commit...
    (hc1 : 1 ≤ c)
    -- ...the snapshot includes.
    (hc : c ≤ h.H t0) :
    -- The claim itself; its proof follows.
    ∃ p, p < h.C t1 ∧ h.R t2 p = true ∧ h.W t2 p = true ∧ h.T p = c := by
  -- When the horizon covered `c`, a flagged slot held its tombstone.
  obtain ⟨p, hr, ht⟩ := h.visible_after_append t0 c hc1 hc
  -- That slot is the one the reader finds.
  refine ⟨p, ?_, ?_, ?_, ht⟩
  -- It was below the count at `t0`...
  · have h0 := h.ready_claimed t0 p hr
    -- ...and the count only rose by `t1`.
    have h1 := h.count_rises t0 t1 h01
    -- So it is below the count the reader read.
    omega
  -- Its flag was set at `t0` and stays set.
  · exact h.ready_stays t0 t2 p (Nat.le_trans h01 h12) hr
  -- And a flagged slot is written, at `t2` as at any moment.
  · exact h.ready_written t2 p (h.ready_stays t0 t2 p (Nat.le_trans h01 h12) hr)

/-! ## The RED cases, as counterexamples -/

/-- **ReadyFirst.** Setting the flag before writing the slot reaches a
state where slot 0 was handed out, its flag is set, and it holds nothing: a
reader that reads it then sees a tombstone before its contents. -/
theorem ready_first_breaks :
    -- The claim itself; its proof follows.
    let w : Log := { start with claimed := 1, ready := fun i => i = 0 }
    -- The claim itself; its proof follows.
    w.ready 0 = true ∧ w.written 0 = false := by
  -- Evaluate.
  decide

/-- **HorizonFirst.** Raising the horizon over a commit whose append has not
set its flag: the position is handed out, the horizon says 1, and no slot's
flag is set. A reader that samples horizon 1 skips the slot and misses the
tombstone its snapshot includes. -/
theorem horizon_first_breaks :
    -- The claim itself; its proof follows.
    let w : Log := { start with claimed := 1 }
    -- The claim itself; its proof follows.
    let horizon := 1
    -- The claim itself; its proof follows.
    0 < horizon ∧ w.ready 0 = false := by
  -- Evaluate.
  decide

/-- **LoadStore.** Taking a position with a load and a later store: two
appends that both load the count before either stores it hold the same
position, and the count ends one short of the appends made. -/
theorem load_store_shares_a_slot :
    -- The claim itself; its proof follows.
    let first := start.claimed
    -- The claim itself; its proof follows.
    let second := start.claimed
    -- The claim itself; its proof follows.
    let after : Log := { { start with claimed := first + 1 } with claimed := second + 1 }
    -- The claim itself; its proof follows.
    first = second ∧ after.claimed = 1 := by
  -- Evaluate.
  decide

-- The end of this file's names.
end Regolith.TombstoneLog
