/-!
# OpenFileTable: at most `cap` open files, never one closed under a reader

This file backs the TLA+ model `proofs/tla/OpenFileTable.tla`. TLC checks
the protocol there for one or two slots and two or three threads. Here the
same rules are proved for every number of slots, threads and steps.

## The story, for a reader who has never seen the code

With `max_open_files` set, regolith keeps every open table file in a small
table of *slots* (`SlotTable`, `src/env/open_file_limit/slots.rs`), one file
per slot, `cap` slots. A read that finds its table in a slot *joins* the slot
(it counts as one more reader of it); a read that does not *claims* a slot
nobody is reading, closes the file there, opens its own, and reads.

Tiny example. One slot. Thread 1 opened table A there and is reading it.
Thread 2 wants table B: it may not take the slot while thread 1 reads, or
thread 1 would read a closed file. Once thread 1 leaves, thread 2 claims the
slot, closes A, opens B. Thread 1, back for A, sees B in the slot and must
claim the slot again: it must never read B by mistake.

The join is one compare-and-swap (CAS) on the slot's state word, which holds
the phase and the reader count but not the owner. So the code checks the
owner again AFTER its CAS: counted as a reader, nobody can claim the slot,
and the owner it sees then is final. In this file a join may join ANY open
slot, whatever it holds: that is every join the CAS can let through (and
more), so the proofs cover the CAS's whole gap.

## What is proved, in plain words

1. `open_le_cap`: never more open files than slots (TLA+ `AtMostCap`).
2. `never_closes_in_use`: the file a thread is reading through sits, open,
   in its slot until it leaves (TLA+ `NeverClosedInUse`).
3. `reads_own_file`: a read returns the table its thread asked for (TLA+
   `ReadsOwnFile`).
4. The RED cases, as counterexamples: a claim that ignores readers closes a
   file in use (`ignore_readers_closes_in_use`); a join with no owner check
   reads another table (`no_recheck_reads_other`); a thread that opens its
   file outside the table goes over the limit (`outside_table_over_cap`).

## How to read the Lean

`def` defines a thing, `theorem` states a fact and its proof follows
`:= by`. Lines starting with `--` are comments, in plain words. A proof is a
list of *tactics*; each one changes the goal still to be shown, and the
comment above it says how. `simp` rewrites with known facts; `omega` solves
arithmetic over natural numbers; `cases` splits on the ways a fact could be
true; `induction` proves a fact for every number of steps by proving it for
none and then for one more.
-/

-- Everything below is named Regolith.OpenFileTable.<name>.
namespace Regolith.OpenFileTable

/-- `f` with its value at `k` replaced by `v`. -/
def set {α : Type} (f : Nat → α) (k : Nat) (v : α) : Nat → α :=
  -- At `k` the new value, everywhere else the old one.
  fun x => if x = k then v else f x

/-- Reading `set` at the key it changed gives the new value. -/
@[simp] theorem set_same {α : Type} (f : Nat → α) (k : Nat) (v : α) :
    -- The claim itself; its proof follows.
    set f k v k = v := by
  -- Unfold `set`; the condition `k = k` holds.
  simp [set]

/-- Reading `set` anywhere else gives the old value. -/
theorem set_other {α : Type} (f : Nat → α) (k x : Nat) (v : α) (h : x ≠ k) :
    -- The claim itself; its proof follows.
    set f k v x = f x := by
  -- Unfold `set`; the condition `x = k` fails by `h`.
  simp [set, h]

/-! ## Part 1. Counting open files -/

/-- How many of the slots `0 .. n-1` hold a file (a nonzero opening number). -/
def countOpen (desc : Nat → Nat) : Nat → Nat
  -- No slot, no file.
  | 0 => 0
  -- The first `n` slots, plus one if slot `n` holds a file.
  | n + 1 => countOpen desc n + (if desc n = 0 then 0 else 1)

/-- At most one file per slot: `n` slots hold at most `n` files. -/
theorem countOpen_le (desc : Nat → Nat) (n : Nat) : countOpen desc n ≤ n := by
  -- By the number of slots.
  induction n with
  -- Case: no slot.
  | zero =>
    -- No slot: zero files.
    simp [countOpen]
  -- Case: one more slot, with the claim for n (ih) known.
  | succ n ih =>
    -- One more slot adds at most one file.
    simp only [countOpen]
    -- Split on the `if`.
    split <;> omega

/-- Changing a slot at or past `n` does not change the count of the first
`n`. -/
theorem countOpen_set_out (desc : Nat → Nat) (i v n : Nat) (h : n ≤ i) :
    -- The claim itself; its proof follows.
    countOpen (set desc i v) n = countOpen desc n := by
  -- By the number of slots counted.
  induction n with
  -- Case: no slot.
  | zero =>
    -- Nothing counted on either side.
    rfl
  -- Case: one more slot, with the claim for n (ih) known.
  | succ n ih =>
    -- Slot `n` is below `i`, so `set` leaves it; the rest by induction.
    simp only [countOpen, ih (by omega), set_other _ _ _ _ (show n ≠ i by omega)]

/-- Setting slot `i` (below `n`) to `v` changes the count by what slot `i`
held and what it holds now. Stated without subtraction: the old count plus
the new slot's contribution equals the new count plus the old slot's. -/
theorem countOpen_set (desc : Nat → Nat) (i v n : Nat) (h : i < n) :
    -- The statement, written without subtraction:
    countOpen (set desc i v) n + (if desc i = 0 then 0 else 1) =
      -- The claim itself; its proof follows.
      countOpen desc n + (if v = 0 then 0 else 1) := by
  -- By the number of slots counted.
  induction n with
  -- Case: no slot.
  | zero =>
    -- No slot is below 0: impossible.
    omega
  -- Case: one more slot, with the claim for n (ih) known.
  | succ n ih =>
    -- Is slot `n` the one that changed?
    by_cases hin : i = n
    -- Yes: below it nothing changed; slot `n` changed from old to new.
    ·
      -- Replace n by i.
      subst hin
      -- Unfold the count by one slot.
      simp only [countOpen, set_same, countOpen_set_out desc i v i (Nat.le_refl i)]
      -- Split on the `if`.
      split <;> split <;> omega
    -- No: slot `n` is unchanged, and `i` is below `n`.
    ·
      -- i is below n.
      have hlt : i < n := by omega
      -- Unfold the count by one slot.
      simp only [countOpen, set_other _ _ _ _ (show n ≠ i by omega)]
      -- Keep this fact for the next step.
      have := ih hlt
      -- The arithmetic over natural numbers that is left holds.
      omega

/-! ## Part 2. The state and the steps -/

/-- A slot's phase (the phase bits of `Slot::state`). -/
inductive Phase where
  /-- Holds no file. -/
  | empty
  /-- One thread owns it, closing the old file and opening its own. -/
  | claimed
  /-- Holds an open file; readers may join. -/
  | open_
  -- Two phases can be compared for equality.
  deriving DecidableEq

/-- Where a thread is in one read. -/
inductive Pc where
  /-- About to read. -/
  | start
  /-- Joined a slot; about to check its owner. -/
  | recheck
  /-- Claimed a slot; about to close its old file. -/
  | claimed
  /-- Closed the old file; about to open its own. -/
  | opening
  /-- About to read through its file. -/
  | read
  /-- Has read; about to leave the slot. -/
  | leave
  -- Two steps can be compared for equality.
  deriving DecidableEq

/-- Everything about the table at one moment. A slot, a thread, a table and
an opening are each named by a natural number; opening `0` means "no file". -/
structure St where
  /-- Each slot's phase. -/
  phase : Nat → Phase
  /-- `holds i u`: thread `u` counts as a reader of slot `i`. -/
  holds : Nat → Nat → Bool
  /-- The table each slot's file is (`Slot::owner`). -/
  owner : Nat → Nat
  /-- The opening each slot holds; `0` for none (`Slot::file`). -/
  desc : Nat → Nat
  /-- The table each opening opened. -/
  fileOf : Nat → Nat
  /-- The next opening's number. -/
  next : Nat
  /-- How many files are open right now. -/
  alive : Nat
  /-- Each thread's step. -/
  pc : Nat → Pc
  /-- The slot each thread is at. -/
  at_ : Nat → Nat
  /-- The opening each thread reads through. -/
  via : Nat → Nat
  /-- The table each thread's last read returned. -/
  got : Nat → Nat

/-- The start: every slot empty, nothing open, every thread about to read. -/
def St.init : St :=
  -- Empty slots, no reader, no file; openings start at 1.
  { phase := fun _ => .empty, holds := fun _ _ => false, owner := fun _ => 0,
    desc := fun _ => 0, fileOf := fun _ => 0, next := 1, alive := 0,
    -- Every thread is at its start.
    pc := fun _ => .start, at_ := fun _ => 0, via := fun _ => 0, got := fun _ => 0 }

/-- A thread is counted as a reader: it joined and has not left. -/
def Pc.reading (p : Pc) : Prop := p = .recheck ∨ p = .read ∨ p = .leave

/-- A thread reads through its file: past its owner check, not yet left. -/
def Pc.through (p : Pc) : Prop := p = .read ∨ p = .leave

/-- A thread owns a slot: it claimed it and has not opened its file yet. -/
def Pc.owning (p : Pc) : Prop := p = .claimed ∨ p = .opening

/-- The steps, one atomic step each, as `SlotTable` takes them. `cap` is the
number of slots and `want u` the table thread `u` reads. -/
inductive Step (cap : Nat) (want : Nat → Nat) : St → St → Prop
  /-- Thread `u` joins open slot `i`: its CAS adds it as a reader. Any open
  slot, whatever it holds: every join the CAS can let through. -/
  | join (s : St) (u i : Nat) (hpc : s.pc u = .start) (hi : i < cap)
      -- Another premise of the claim, named so the proof can use it.
      (hph : s.phase i = .open_) :
      -- The state after this step: only the fields named here change.
      Step cap want s { s with holds := set s.holds i (set (s.holds i) u true),
                               at_ := set s.at_ u i, pc := set s.pc u .recheck }
  /-- The owner check after the CAS finds the thread's table: it reads
  through the slot's file. -/
  | recheckOk (s : St) (u : Nat) (hpc : s.pc u = .recheck)
      -- Another premise of the claim, named so the proof can use it.
      (ho : s.owner (s.at_ u) = want u) :
      -- The state after this step: only the fields named here change.
      Step cap want s { s with via := set s.via u (s.desc (s.at_ u)),
                               pc := set s.pc u .read }
  /-- The owner check finds another table: the thread leaves and starts over. -/
  | recheckFail (s : St) (u : Nat) (hpc : s.pc u = .recheck)
      -- Another premise of the claim, named so the proof can use it.
      (ho : s.owner (s.at_ u) ≠ want u) :
      -- The state after this step: only the fields named here change.
      Step cap want s { s with
        holds := set s.holds (s.at_ u) (set (s.holds (s.at_ u)) u false),
        pc := set s.pc u .start }
  /-- Thread `u` claims slot `i` by CAS: empty, or open with no reader. -/
  | claim (s : St) (u i : Nat) (hpc : s.pc u = .start) (hi : i < cap)
      -- Another premise of the claim, named so the proof can use it.
      (hfree : s.phase i = .empty ∨ (s.phase i = .open_ ∧ ∀ v, s.holds i v = false)) :
      -- The state after this step: only the fields named here change.
      Step cap want s { s with phase := set s.phase i .claimed,
                               at_ := set s.at_ u i, pc := set s.pc u .claimed }
  /-- The claimer closes the slot's old file, if any. -/
  | close (s : St) (u : Nat) (hpc : s.pc u = .claimed) :
      -- The state after this step: only the fields named here change.
      Step cap want s { s with
        alive := s.alive - (if s.desc (s.at_ u) = 0 then 0 else 1),
        desc := set s.desc (s.at_ u) 0, pc := set s.pc u .opening }
  /-- The claimer opens its table in the slot and publishes it open, counting
  itself as the one reader. -/
  | open_ (s : St) (u : Nat) (hpc : s.pc u = .opening) :
      -- The state after this step: only the fields named here change.
      Step cap want s { s with
        desc := set s.desc (s.at_ u) s.next, fileOf := set s.fileOf s.next (want u),
        owner := set s.owner (s.at_ u) (want u), phase := set s.phase (s.at_ u) .open_,
        holds := set s.holds (s.at_ u) (set (s.holds (s.at_ u)) u true),
        alive := s.alive + 1, next := s.next + 1,
        via := set s.via u s.next, pc := set s.pc u .read }
  /-- The thread reads through its file: it gets that file's table. -/
  | read (s : St) (u : Nat) (hpc : s.pc u = .read) :
      -- The state after this step: only the fields named here change.
      Step cap want s { s with got := set s.got u (s.fileOf (s.via u)),
                               pc := set s.pc u .leave }
  /-- The thread leaves the slot: one decrement. -/
  | leave (s : St) (u : Nat) (hpc : s.pc u = .leave) :
      -- The state after this step: only the fields named here change.
      Step cap want s { s with
        holds := set s.holds (s.at_ u) (set (s.holds (s.at_ u)) u false),
        pc := set s.pc u .start }

/-- The states the steps can reach from the start. -/
inductive Reach (cap : Nat) (want : Nat → Nat) : St → Prop
  /-- The start is reachable. -/
  | init : Reach cap want St.init
  /-- One step from a reachable state reaches another. -/
  | step {s t : St} : Reach cap want s → Step cap want s t → Reach cap want t

/-! ## Part 3. What every reachable state keeps -/

/-- The facts every reachable state has. The three rules follow from them. -/
structure Inv (cap : Nat) (want : Nat → Nat) (s : St) : Prop where
  /-- The open-file count is the number of slots holding a file. -/
  alive_eq : s.alive = countOpen s.desc cap
  /-- A slot past the table never holds a file, a reader or a claim. -/
  outside : ∀ i, cap ≤ i → s.desc i = 0 ∧ s.phase i = .empty ∧ ∀ v, s.holds i v = false
  /-- A claimed slot has no reader. -/
  claimed_free : ∀ i, s.phase i = .claimed → ∀ v, s.holds i v = false
  /-- An empty slot holds no file and no reader. -/
  empty_clean : ∀ i, s.phase i = .empty → s.desc i = 0 ∧ ∀ v, s.holds i v = false
  /-- An open slot holds a file, and that file is its owner's table. -/
  open_file : ∀ i, s.phase i = .open_ → s.desc i ≠ 0 ∧ s.fileOf (s.desc i) = s.owner i
  /-- A slot's reader is a reading thread at that slot. -/
  holder : ∀ i v, s.holds i v = true → (s.pc v).reading ∧ s.at_ v = i
  /-- A reading thread is counted at its slot, open and in the table. -/
  reader : ∀ v, (s.pc v).reading →
    s.holds (s.at_ v) v = true ∧ s.at_ v < cap ∧ s.phase (s.at_ v) = .open_
  /-- A thread past its owner check reads through its slot's file, which is
  its own table's. -/
  through : ∀ v, (s.pc v).through → s.via v = s.desc (s.at_ v) ∧ s.owner (s.at_ v) = want v
  /-- A thread that read got its own table. -/
  got_own : ∀ v, s.pc v = .leave → s.got v = want v
  /-- A claimer holds its slot claimed, in the table, alone. -/
  owner_claim : ∀ v, (s.pc v).owning → s.at_ v < cap ∧ s.phase (s.at_ v) = .claimed ∧
    ∀ u, u ≠ v → (s.pc u).owning → s.at_ u ≠ s.at_ v
  /-- Openings are numbered from 1, each below `next`. -/
  fresh : 0 < s.next ∧ ∀ i, s.desc i < s.next
  /-- A claimer that closed the old file holds a slot with no file. -/
  closed : ∀ v, s.pc v = .opening → s.desc (s.at_ v) = 0

/-- The start keeps every fact: nothing is open and nobody reads. -/
theorem inv_init (cap : Nat) (want : Nat → Nat) : Inv cap want St.init := by
  -- No slot holds a file, so the count is 0 for any number of slots.
  have hcount : ∀ n, countOpen (fun _ => 0) n = 0 := by
    -- Take any number of slots n; induction on it.
    intro n; induction n with
    -- No slot: 0 by definition.
    | zero => rfl
    -- One more slot, which holds no file: still 0.
    | succ n ih => simp [countOpen, ih]
  -- Each fact, about an empty start.
  constructor
  -- No file is open, and no slot holds one.
  · simp [St.init, hcount]
  -- Every slot is empty, with no file and no reader.
  · intro i _; simp [St.init]
  -- No slot is claimed.
  · intro i h; simp [St.init] at h
  -- Every slot is empty, with no file and no reader.
  · intro i _; simp [St.init]
  -- No slot is open.
  · intro i h; simp [St.init] at h
  -- No slot has a reader.
  · intro i v h; simp [St.init] at h
  -- No thread is reading.
  · intro v h; simp [St.init, Pc.reading] at h
  -- No thread is past its check.
  · intro v h; simp [St.init, Pc.through] at h
  -- No thread has read.
  · intro v h; simp [St.init] at h
  -- No thread holds a claim.
  · intro v h; simp [St.init, Pc.owning] at h
  -- The first opening is 1, and every slot holds 0.
  · simp [St.init]
  -- No thread has closed a file.
  · intro v h; simp [St.init] at h

/-- A thread at its start counts as a reader nowhere. -/
theorem start_holds_nothing {cap : Nat} {want : Nat → Nat} {s : St} (hs : Inv cap want s)
    -- The claim itself; its proof follows.
    {u : Nat} (hpc : s.pc u = .start) (i : Nat) : s.holds i u = false := by
  -- If it were counted at slot `i`, it would be reading, not at its start.
  cases h : s.holds i u
  -- Both sides are the same.
  · rfl
  -- Keep this fact for the next step.
  · have := (hs.holder i u h).1
    -- Rewrite `this` with the equalities given.
    rw [hpc] at this
    -- Simplify the named facts with the definitions; a false one closes the goal.
    simp [Pc.reading] at this

/-- **Join.** Thread `u` joins open slot `i`. -/
theorem inv_join {cap : Nat} {want : Nat → Nat} {s : St} (hs : Inv cap want s)
    -- The claim itself; its proof follows.
    (u i : Nat) (hpc : s.pc u = .start) (hi : i < cap) (hph : s.phase i = .open_) :
    -- The claim, which continues on the lines below.
    Inv cap want { s with holds := set s.holds i (set (s.holds i) u true),
                          at_ := set s.at_ u i, pc := set s.pc u .recheck } := by
  -- `u` held nothing before.
  have hnone := start_holds_nothing hs hpc
  -- Prove each fact of the structure in turn.
  constructor
  -- No file changes.
  · exact hs.alive_eq
  -- Slots past the table are not slot `i`: untouched.
  · intro j hj
    -- First show `j ≠ i`, named `hji`.
    have hji : j ≠ i := by omega
    -- Read through `set` at a key it did not change: the old value.
    simp only [set_other _ _ _ _ hji]
    -- The state's `outside` fact from before the step gives exactly this.
    exact hs.outside j hj
  -- A claimed slot is not `i` (which is open): untouched.
  · intro j hj v
    -- First show `j ≠ i`, named `hji`, then replace one side of `h` by the other everywhere, then rewrite `hj` with the equalities given, then split on the ways `hj` can hold: none is left.
    have hji : j ≠ i := by intro h; subst h; rw [hph] at hj; cases hj
    -- Read through `set` at a key it did not change: the old value.
    simp only [set_other _ _ _ _ hji]
    -- The state's `claimed_free` fact from before the step gives exactly this.
    exact hs.claimed_free j hj v
  -- An empty slot is not `i` (which is open): untouched.
  · intro j hj
    -- First show `j ≠ i`, named `hji`, then replace one side of `h` by the other everywhere, then rewrite `hj` with the equalities given, then split on the ways `hj` can hold: none is left.
    have hji : j ≠ i := by intro h; subst h; rw [hph] at hj; cases hj
    -- Read through `set` at a key it did not change: the old value.
    simp only [set_other _ _ _ _ hji]
    -- The state's `empty_clean` fact from before the step gives exactly this.
    exact hs.empty_clean j hj
  -- No file and no owner changes.
  · exact hs.open_file
  -- Readers: `u` at `i` now, and every old reader as before.
  · intro j v hv
    -- Split on whether `v = u` holds.
    by_cases hvu : v = u
    -- `u`: reading (rechecking) at `i`.
    ·
      -- Replace v by u.
      subst hvu
      -- Split on whether `j = i` holds.
      by_cases hji : j = i
      -- Replace one side of `hji` by the other everywhere, then simplify with the definitions until the goal is closed or plain.
      · subst hji; simp [Pc.reading]
      -- Read through `set` at a key it did not change: the old value, then rewrite `hv` with the equalities given, then split on the ways `hv` can hold: none is left.
      · simp only [set_other _ _ _ _ hji] at hv; rw [hnone j] at hv; cases hv
    -- Another thread: it was a reader of `j` already.
    ·
      -- It was counted as a reader of j before the step:
      have hold : s.holds j v = true := by
        -- Split on whether j is the slot i.
        by_cases hji : j = i
        -- Replace one side of `hji` by the other everywhere, then read through `set`: the new value at the key it wrote, the old value elsewhere, then this is exactly what the goal asks.
        · subst hji; simp only [set_same, set_other _ _ _ _ hvu] at hv; exact hv
        -- Read through `set` at a key it did not change: the old value, then this is exactly what the goal asks.
        · simp only [set_other _ _ _ _ hji] at hv; exact hv
      -- Read through `set` at a key it did not change: the old value.
      simp only [set_other _ _ _ _ hvu]
      -- The state's `holder` fact from before the step gives exactly this.
      exact hs.holder j v hold
  -- Reading threads: `u` at open slot `i`; the others as before.
  · intro v hv
    -- Split on whether `v = u` holds.
    by_cases hvu : v = u
    -- Replace one side of `hvu` by the other everywhere, then simplify with the definitions until the goal is closed or plain.
    · subst hvu; simp [hi, hph]
    -- Read through `set` at a key it did not change: the old value.
    · simp only [set_other _ _ _ _ hvu] at hv ⊢
      -- Unpack the facts on the right and name them.
      obtain ⟨hh, hlt, hop⟩ := hs.reader v hv
      -- Prove the parts one at a time; each `?_` becomes a goal of its own.
      refine ⟨?_, hlt, hop⟩
      -- Split on whether `s.at_ v = i` holds.
      by_cases hvi : s.at_ v = i
      -- Rewrite the goal with the equalities given, then read through `set`: the new value at the key it wrote, the old value elsewhere, then rewrite the goal with the equalities given, then this is exactly what the goal asks.
      · rw [hvi]; simp only [set_same, set_other _ _ _ _ hvu]; rw [← hvi]; exact hh
      -- Read through `set` at a key it did not change: the old value, then this is exactly what the goal asks.
      · simp only [set_other _ _ _ _ hvi]; exact hh
  -- Threads past their check: not `u`, untouched.
  · intro v hv
    -- First show `v ≠ u`, named `hvu`, then replace one side of `h` by the other everywhere, then simplify the named facts with the definitions; a false one closes the goal.
    have hvu : v ≠ u := by intro h; subst h; simp [Pc.through] at hv
    -- Read through `set` at a key it did not change: the old value.
    simp only [set_other _ _ _ _ hvu] at hv ⊢
    -- The state's `through` fact from before the step gives exactly this.
    exact hs.through v hv
  -- Threads that read: not `u`, untouched.
  · intro v hv
    -- First show `v ≠ u`, named `hvu`, then replace one side of `h` by the other everywhere, then simplify the named facts with the definitions; a false one closes the goal.
    have hvu : v ≠ u := by intro h; subst h; simp at hv
    -- Read through `set` at a key it did not change: the old value.
    simp only [set_other _ _ _ _ hvu] at hv
    -- The state's `got_own` fact from before the step gives exactly this.
    exact hs.got_own v hv
  -- Claimers: not `u`, untouched.
  · intro v hv
    -- First show `v ≠ u`, named `hvu`, then replace one side of `h` by the other everywhere, then simplify the named facts with the definitions; a false one closes the goal.
    have hvu : v ≠ u := by intro h; subst h; simp [Pc.owning] at hv
    -- Read through `set` at a key it did not change: the old value.
    simp only [set_other _ _ _ _ hvu] at hv ⊢
    -- Unpack the facts on the right and name them.
    obtain ⟨hlt, hcl, huniq⟩ := hs.owner_claim v hv
    -- Prove the parts one at a time; each `?_` becomes a goal of its own.
    refine ⟨hlt, hcl, ?_⟩
    -- Name the things the goal is about: `w`, `hw`, `hwo`.
    intro w hw hwo
    -- First show `w ≠ u`, named `hwu`, then replace one side of `h` by the other everywhere, then simplify the named facts with the definitions; a false one closes the goal.
    have hwu : w ≠ u := by intro h; subst h; simp [Pc.owning] at hwo
    -- Read through `set` at a key it did not change: the old value.
    simp only [set_other _ _ _ _ hwu] at hwo ⊢
    -- This is exactly what the goal asks.
    exact huniq w hw hwo
  -- No opening changes.
  · exact hs.fresh
  -- Closers: not `u`, untouched.
  · intro v hv
    -- First show `v ≠ u`, named `hvu`, then replace one side of `h` by the other everywhere, then simplify the named facts with the definitions; a false one closes the goal.
    have hvu : v ≠ u := by intro h; subst h; simp at hv
    -- Read through `set` at a key it did not change: the old value.
    simp only [set_other _ _ _ _ hvu] at hv ⊢
    -- The state's `closed` fact from before the step gives exactly this.
    exact hs.closed v hv

/-- **Recheck, found.** Thread `u`'s slot holds its table: it reads through
the slot's file. -/
theorem inv_recheckOk {cap : Nat} {want : Nat → Nat} {s : St} (hs : Inv cap want s)
    -- The claim itself; its proof follows.
    (u : Nat) (hpc : s.pc u = .recheck) (ho : s.owner (s.at_ u) = want u) :
    -- The claim itself; its proof follows.
    Inv cap want { s with via := set s.via u (s.desc (s.at_ u)), pc := set s.pc u .read } := by
  -- Prove each fact of the invariant in turn.
  constructor
  -- No file changes.
  · exact hs.alive_eq
  -- No slot changes.
  · exact hs.outside
  -- No slot changes.
  · exact hs.claimed_free
  -- No slot changes.
  · exact hs.empty_clean
  -- No slot changes.
  · exact hs.open_file
  -- Readers: `u` still reads (now past its check); others as before.
  · intro j v hv
    -- Unpack the facts on the right and name them.
    obtain ⟨hr, hat⟩ := hs.holder j v hv
    -- Split on whether `v = u` holds.
    by_cases hvu : v = u
    -- Replace one side of `hvu` by the other everywhere, then simplify with the definitions until the goal is closed or plain.
    · subst hvu; simp [Pc.reading, hat]
    -- Read through `set` at a key it did not change: the old value, then put the pieces together.
    · simp only [set_other _ _ _ _ hvu]; exact ⟨hr, hat⟩
  -- Reading threads: `u` was reading; others as before.
  · intro v hv
    -- Split on whether `v = u` holds.
    by_cases hvu : v = u
    -- Replace one side of `hvu` by the other everywhere, then the state's `reader` fact from before the step gives exactly this.
    · subst hvu; exact hs.reader v (by simp [Pc.reading, hpc])
    -- Read through `set` at a key it did not change: the old value, then the state's `reader` fact from before the step gives exactly this.
    · simp only [set_other _ _ _ _ hvu] at hv; exact hs.reader v hv
  -- Past the check: `u` reads its slot's file, its own table.
  · intro v hv
    -- Split on whether `v = u` holds.
    by_cases hvu : v = u
    -- Replace one side of `hvu` by the other everywhere, then simplify with the definitions until the goal is closed or plain.
    · subst hvu; simp [ho]
    -- Read through `set` at a key it did not change: the old value, then the state's `through` fact from before the step gives exactly this.
    · simp only [set_other _ _ _ _ hvu] at hv ⊢; exact hs.through v hv
  -- Threads that read: not `u`.
  · intro v hv
    -- First show `v ≠ u`, named `hvu`, then replace one side of `h` by the other everywhere, then simplify the named facts with the definitions; a false one closes the goal.
    have hvu : v ≠ u := by intro h; subst h; simp at hv
    -- Read through `set` at a key it did not change: the old value, then the state's `got_own` fact from before the step gives exactly this.
    simp only [set_other _ _ _ _ hvu] at hv; exact hs.got_own v hv
  -- Claimers: not `u`.
  · intro v hv
    -- First show `v ≠ u`, named `hvu`, then replace one side of `h` by the other everywhere, then simplify the named facts with the definitions; a false one closes the goal.
    have hvu : v ≠ u := by intro h; subst h; simp [Pc.owning] at hv
    -- Read through `set` at a key it did not change: the old value.
    simp only [set_other _ _ _ _ hvu] at hv
    -- Unpack the facts on the right and name them.
    obtain ⟨hlt, hcl, huniq⟩ := hs.owner_claim v hv
    -- Prove the parts one at a time; each `?_` becomes a goal of its own.
    refine ⟨hlt, hcl, ?_⟩
    -- Name the things the goal is about: `w`, `hw`, `hwo`.
    intro w hw hwo
    -- First show `w ≠ u`, named `hwu`, then replace one side of `h` by the other everywhere, then simplify the named facts with the definitions; a false one closes the goal.
    have hwu : w ≠ u := by intro h; subst h; simp [Pc.owning] at hwo
    -- Read through `set` at a key it did not change: the old value, then this is exactly what the goal asks.
    simp only [set_other _ _ _ _ hwu] at hwo; exact huniq w hw hwo
  -- No opening changes.
  · exact hs.fresh
  -- Closers: not `u`.
  · intro v hv
    -- First show `v ≠ u`, named `hvu`, then replace one side of `h` by the other everywhere, then simplify the named facts with the definitions; a false one closes the goal.
    have hvu : v ≠ u := by intro h; subst h; simp at hv
    -- Read through `set` at a key it did not change: the old value, then the state's `closed` fact from before the step gives exactly this.
    simp only [set_other _ _ _ _ hvu] at hv; exact hs.closed v hv

/-- Thread `u` stops counting at its slot and starts over: the shared shape
of a failed recheck and a leave. -/
theorem inv_drop_out {cap : Nat} {want : Nat → Nat} {s : St} (hs : Inv cap want s)
    -- The claim itself; its proof follows.
    (u : Nat) (hpc : (s.pc u).reading) :
    -- The claim, which continues on the lines below.
    Inv cap want { s with holds := set s.holds (s.at_ u) (set (s.holds (s.at_ u)) u false),
                          pc := set s.pc u .start } := by
  -- `u` is counted only at its own slot.
  have honly : ∀ j, s.holds j u = true → j = s.at_ u := fun j h => (hs.holder j u h).2.symm
  -- After the step, nobody is counted anywhere they were not before.
  have hsub : ∀ j v, set s.holds (s.at_ u) (set (s.holds (s.at_ u)) u false) j v = true →
      s.holds j v = true ∧ v ≠ u := by
    -- Take a slot j and a thread v counted there after the step.
    intro j v hv
    -- Split on whether `j = s.at_ u` holds.
    by_cases hju : j = s.at_ u
    -- Replace one side of `hju` by the other everywhere.
    · subst hju
      -- Split on whether `v = u` holds.
      by_cases hvu : v = u
      -- Replace one side of `hvu` by the other everywhere, then simplify the named facts with the definitions; a false one closes the goal.
      · subst hvu; simp at hv
      -- Read through `set`: the new value at the key it wrote, the old value elsewhere, then put the pieces together.
      · simp only [set_same, set_other _ _ _ _ hvu] at hv; exact ⟨hv, hvu⟩
    -- Read through `set` at a key it did not change: the old value.
    · simp only [set_other _ _ _ _ hju] at hv
      -- Prove the parts one at a time; each `?_` becomes a goal of its own.
      refine ⟨hv, ?_⟩
      -- Name the things the goal is about: `hvu`, then replace one side of `hvu` by the other everywhere, then this is exactly what the goal asks.
      intro hvu; subst hvu; exact hju (honly j hv)
  -- Removing a reader keeps "no reader" true.
  have hfalse : ∀ j, (∀ v, s.holds j v = false) →
      ∀ v, set s.holds (s.at_ u) (set (s.holds (s.at_ u)) u false) j v = false := by
    -- Take a slot j with no reader, and a thread v.
    intro j hj v
    -- Split on the two values it can have.
    cases h : set s.holds (s.at_ u) (set (s.holds (s.at_ u)) u false) j v
    -- Both sides are the same.
    · rfl
    -- Keep this fact for the next step, then rewrite `this` with the equalities given, then split on the ways `this` can hold: none is left.
    · have := (hsub j v h).1; rw [hj v] at this; cases this
  -- Prove each fact of the structure in turn.
  constructor
  -- No file changes.
  · exact hs.alive_eq
  -- Past the table: still no file, no claim, no reader.
  · intro j hj
    -- Unpack the facts on the right and name them.
    obtain ⟨hd, hp, hh⟩ := hs.outside j hj
    -- Put the pieces together.
    exact ⟨hd, hp, hfalse j hh⟩
  -- Claimed slots: still no reader.
  · intro j hj; exact hfalse j (hs.claimed_free j hj)
  -- Empty slots: still no file and no reader.
  · intro j hj
    -- Unpack the facts on the right and name them.
    obtain ⟨hd, hh⟩ := hs.empty_clean j hj
    -- Put the pieces together.
    exact ⟨hd, hfalse j hh⟩
  -- No file or owner changes.
  · exact hs.open_file
  -- Readers: only old readers other than `u`, unchanged.
  · intro j v hv
    -- Unpack the facts on the right and name them.
    obtain ⟨hold, hvu⟩ := hsub j v hv
    -- Read through `set` at a key it did not change: the old value.
    simp only [set_other _ _ _ _ hvu]
    -- The state's `holder` fact from before the step gives exactly this.
    exact hs.holder j v hold
  -- Reading threads: not `u` (it is at its start); unchanged.
  · intro v hv
    -- First show `v ≠ u`, named `hvu`, then replace one side of `h` by the other everywhere, then simplify the named facts with the definitions; a false one closes the goal.
    have hvu : v ≠ u := by intro h; subst h; simp [Pc.reading] at hv
    -- Read through `set` at a key it did not change: the old value.
    simp only [set_other _ _ _ _ hvu] at hv ⊢
    -- Unpack the facts on the right and name them.
    obtain ⟨hh, hlt, hop⟩ := hs.reader v hv
    -- Prove the parts one at a time; each `?_` becomes a goal of its own.
    refine ⟨?_, hlt, hop⟩
    -- Split on whether `s.at_ v = s.at_ u` holds.
    by_cases hvj : s.at_ v = s.at_ u
    -- Rewrite the goal with the equalities given, then read through `set`: the new value at the key it wrote, the old value elsewhere, then rewrite the goal with the equalities given, then this is exactly what the goal asks.
    · rw [hvj]; simp only [set_same, set_other _ _ _ _ hvu]; rw [← hvj]; exact hh
    -- Read through `set` at a key it did not change: the old value, then this is exactly what the goal asks.
    · simp only [set_other _ _ _ _ hvj]; exact hh
  -- Past the check: not `u`; unchanged.
  · intro v hv
    -- First show `v ≠ u`, named `hvu`, then replace one side of `h` by the other everywhere, then simplify the named facts with the definitions; a false one closes the goal.
    have hvu : v ≠ u := by intro h; subst h; simp [Pc.through] at hv
    -- Read through `set` at a key it did not change: the old value, then the state's `through` fact from before the step gives exactly this.
    simp only [set_other _ _ _ _ hvu] at hv; exact hs.through v hv
  -- Threads that read: not `u`; unchanged.
  · intro v hv
    -- First show `v ≠ u`, named `hvu`, then replace one side of `h` by the other everywhere, then simplify the named facts with the definitions; a false one closes the goal.
    have hvu : v ≠ u := by intro h; subst h; simp at hv
    -- Read through `set` at a key it did not change: the old value, then the state's `got_own` fact from before the step gives exactly this.
    simp only [set_other _ _ _ _ hvu] at hv; exact hs.got_own v hv
  -- Claimers: not `u` (it was reading); unchanged.
  · intro v hv
    -- First show `v ≠ u`, named `hvu`, then replace one side of `h` by the other everywhere, then simplify the named facts with the definitions; a false one closes the goal.
    have hvu : v ≠ u := by intro h; subst h; simp [Pc.owning] at hv
    -- Read through `set` at a key it did not change: the old value.
    simp only [set_other _ _ _ _ hvu] at hv
    -- Unpack the facts on the right and name them.
    obtain ⟨hlt, hcl, huniq⟩ := hs.owner_claim v hv
    -- Prove the parts one at a time; each `?_` becomes a goal of its own.
    refine ⟨hlt, hcl, ?_⟩
    -- Name the things the goal is about: `w`, `hw`, `hwo`.
    intro w hw hwo
    -- First show `w ≠ u`, named `hwu`, then replace one side of `h` by the other everywhere, then simplify the named facts with the definitions; a false one closes the goal.
    have hwu : w ≠ u := by intro h; subst h; simp [Pc.owning] at hwo
    -- Read through `set` at a key it did not change: the old value, then this is exactly what the goal asks.
    simp only [set_other _ _ _ _ hwu] at hwo; exact huniq w hw hwo
  -- No opening changes.
  · exact hs.fresh
  -- Closers: not `u`; unchanged.
  · intro v hv
    -- First show `v ≠ u`, named `hvu`, then replace one side of `h` by the other everywhere, then simplify the named facts with the definitions; a false one closes the goal.
    have hvu : v ≠ u := by intro h; subst h; simp at hv
    -- Read through `set` at a key it did not change: the old value, then the state's `closed` fact from before the step gives exactly this.
    simp only [set_other _ _ _ _ hvu] at hv; exact hs.closed v hv

/-- **Claim.** Thread `u` claims slot `i`, empty or open with no reader. -/
theorem inv_claim {cap : Nat} {want : Nat → Nat} {s : St} (hs : Inv cap want s)
    -- The claim, which continues on the lines below.
    (u i : Nat) (hpc : s.pc u = .start) (hi : i < cap)
    -- Another premise of the claim, named so the proof can use it.
    (hfree : s.phase i = .empty ∨ (s.phase i = .open_ ∧ ∀ v, s.holds i v = false)) :
    -- The claim, which continues on the lines below.
    Inv cap want { s with phase := set s.phase i .claimed,
                          at_ := set s.at_ u i, pc := set s.pc u .claimed } := by
  -- Slot `i` has no reader.
  have hnoread : ∀ v, s.holds i v = false := by
    -- Slot i is claimable: it is empty, or open with nobody reading.
    rcases hfree with he | ⟨_, hh⟩
    -- This is exactly what the goal asks.
    · exact (hs.empty_clean i he).2
    -- This is exactly what the goal asks.
    · exact hh
  -- Slot `i` was not claimed.
  have hnotcl : s.phase i ≠ .claimed := by
    -- In both cases its phase is "empty" or "open", not "claimed".
    rcases hfree with he | ⟨ho, _⟩ <;> simp [*]
  -- `u` held nothing.
  have hnone := start_holds_nothing hs hpc
  -- Prove each fact of the structure in turn.
  constructor
  -- No file changes.
  · exact hs.alive_eq
  -- Past the table: not `i`.
  · intro j hj
    -- First show `j ≠ i`, named `hji`.
    have hji : j ≠ i := by omega
    -- Read through `set` at a key it did not change: the old value, then the state's `outside` fact from before the step gives exactly this.
    simp only [set_other _ _ _ _ hji]; exact hs.outside j hj
  -- Claimed slots: `i` has no reader; others as before.
  · intro j hj v
    -- Split on whether `j = i` holds.
    by_cases hji : j = i
    -- Replace one side of `hji` by the other everywhere, then this is exactly what the goal asks.
    · subst hji; exact hnoread v
    -- Read through `set` at a key it did not change: the old value, then the state's `claimed_free` fact from before the step gives exactly this.
    · simp only [set_other _ _ _ _ hji] at hj; exact hs.claimed_free j hj v
  -- Empty slots: `i` is claimed now; others as before.
  · intro j hj
    -- First show `j ≠ i`, named `hji`, then replace one side of `h` by the other everywhere, then simplify the named facts with the definitions; a false one closes the goal.
    have hji : j ≠ i := by intro h; subst h; simp at hj
    -- Read through `set` at a key it did not change: the old value, then the state's `empty_clean` fact from before the step gives exactly this.
    simp only [set_other _ _ _ _ hji] at hj; exact hs.empty_clean j hj
  -- Open slots: `i` is claimed now; others as before.
  · intro j hj
    -- First show `j ≠ i`, named `hji`, then replace one side of `h` by the other everywhere, then simplify the named facts with the definitions; a false one closes the goal.
    have hji : j ≠ i := by intro h; subst h; simp at hj
    -- Read through `set` at a key it did not change: the old value, then the state's `open_file` fact from before the step gives exactly this.
    simp only [set_other _ _ _ _ hji] at hj; exact hs.open_file j hj
  -- Readers: unchanged, and none is `u`.
  · intro j v hv
    -- Unpack the facts on the right and name them.
    obtain ⟨hr, hat⟩ := hs.holder j v hv
    -- First show `v ≠ u`, named `hvu`, then replace one side of `h` by the other everywhere, then rewrite `hv` with the equalities given, then split on the ways `hv` can hold: none is left.
    have hvu : v ≠ u := by intro h; subst h; rw [hnone j] at hv; cases hv
    -- Read through `set` at a key it did not change: the old value, then put the pieces together.
    simp only [set_other _ _ _ _ hvu]; exact ⟨hr, hat⟩
  -- Reading threads: not `u`; their slot is open, so not `i`.
  · intro v hv
    -- First show `v ≠ u`, named `hvu`, then replace one side of `h` by the other everywhere, then simplify the named facts with the definitions; a false one closes the goal.
    have hvu : v ≠ u := by intro h; subst h; simp [Pc.reading] at hv
    -- Read through `set` at a key it did not change: the old value.
    simp only [set_other _ _ _ _ hvu] at hv ⊢
    -- Unpack the facts on the right and name them.
    obtain ⟨hh, hlt, hop⟩ := hs.reader v hv
    -- First show `s.at_ v ≠ i`, named `hvi`, then rewrite `hh` with the equalities given, then split on the ways `hh` can hold: none is left.
    have hvi : s.at_ v ≠ i := by intro h; rw [h, hnoread v] at hh; cases hh
    -- Read through `set` at a key it did not change: the old value, then put the pieces together.
    simp only [set_other _ _ _ _ hvi]; exact ⟨hh, hlt, hop⟩
  -- Past the check: not `u`; unchanged.
  · intro v hv
    -- First show `v ≠ u`, named `hvu`, then replace one side of `h` by the other everywhere, then simplify the named facts with the definitions; a false one closes the goal.
    have hvu : v ≠ u := by intro h; subst h; simp [Pc.through] at hv
    -- Read through `set` at a key it did not change: the old value, then the state's `through` fact from before the step gives exactly this.
    simp only [set_other _ _ _ _ hvu] at hv ⊢; exact hs.through v hv
  -- Threads that read: not `u`; unchanged.
  · intro v hv
    -- First show `v ≠ u`, named `hvu`, then replace one side of `h` by the other everywhere, then simplify the named facts with the definitions; a false one closes the goal.
    have hvu : v ≠ u := by intro h; subst h; simp at hv
    -- Read through `set` at a key it did not change: the old value, then the state's `got_own` fact from before the step gives exactly this.
    simp only [set_other _ _ _ _ hvu] at hv; exact hs.got_own v hv
  -- Claimers: `u` holds `i`, claimed, alone; the others keep theirs.
  · intro v hv
    -- Split on whether `v = u` holds.
    by_cases hvu : v = u
    -- `u`: slot `i`, claimed now; no other claimer was at `i`.
    ·
      -- Replace v by u.
      subst hvu
      -- Read the value `set` just wrote at this key.
      simp only [set_same]
      -- Prove the parts one at a time; each `?_` becomes a goal of its own.
      refine ⟨hi, trivial, ?_⟩
      -- Name the things the goal is about: `w`, `hw`, `hwo`.
      intro w hw hwo
      -- Read through `set` at a key it did not change: the old value.
      simp only [set_other _ _ _ _ hw] at hwo ⊢
      -- Name the things the goal is about: `hwi`.
      intro hwi
      -- Keep this fact for the next step.
      have := (hs.owner_claim w hwo).2.1
      -- Rewrite `this` with the equalities given, then this is exactly what the goal asks.
      rw [hwi] at this; exact hnotcl this
    -- Another claimer: its slot is claimed, so not `i`.
    ·
      -- Its entries are untouched by the step.
      simp only [set_other _ _ _ _ hvu] at hv ⊢
      -- Unpack the facts on the right and name them.
      obtain ⟨hlt, hcl, huniq⟩ := hs.owner_claim v hv
      -- First show `s.at_ v ≠ i`, named `hvi`, then rewrite `hcl` with the equalities given, then this is exactly what the goal asks.
      have hvi : s.at_ v ≠ i := by intro h; rw [h] at hcl; exact hnotcl hcl
      -- Read through `set` at a key it did not change: the old value.
      simp only [set_other _ _ _ _ hvi]
      -- Prove the parts one at a time; each `?_` becomes a goal of its own.
      refine ⟨hlt, hcl, ?_⟩
      -- Name the things the goal is about: `w`, `hw`, `hwo`.
      intro w hw hwo
      -- Split on whether `w = u` holds.
      by_cases hwu : w = u
      -- Replace one side of `hwu` by the other everywhere, then read the value `set` just wrote at this key, then this is exactly what the goal asks.
      · subst hwu; simp only [set_same]; exact fun h => hvi h.symm
      -- Read through `set` at a key it did not change: the old value, then this is exactly what the goal asks.
      · simp only [set_other _ _ _ _ hwu] at hwo ⊢; exact huniq w hw hwo
  -- No opening changes.
  · exact hs.fresh
  -- Closers: not `u`; unchanged.
  · intro v hv
    -- First show `v ≠ u`, named `hvu`, then replace one side of `h` by the other everywhere, then simplify the named facts with the definitions; a false one closes the goal.
    have hvu : v ≠ u := by intro h; subst h; simp at hv
    -- Read through `set` at a key it did not change: the old value, then the state's `closed` fact from before the step gives exactly this.
    simp only [set_other _ _ _ _ hvu] at hv ⊢; exact hs.closed v hv

/-- **Close.** Claimer `u` closes the old file of its slot, if any. -/
theorem inv_close {cap : Nat} {want : Nat → Nat} {s : St} (hs : Inv cap want s)
    -- The claim itself; its proof follows.
    (u : Nat) (hpc : s.pc u = .claimed) :
    -- The claim, which continues on the lines below.
    Inv cap want { s with alive := s.alive - (if s.desc (s.at_ u) = 0 then 0 else 1),
                          desc := set s.desc (s.at_ u) 0, pc := set s.pc u .opening } := by
  -- `u` holds its slot claimed, in the table, alone.
  obtain ⟨hlt, hcl, huniq⟩ := hs.owner_claim u (by simp [Pc.owning, hpc])
  -- Any slot that is not claimed is not `u`'s.
  have hne : ∀ j, s.phase j ≠ .claimed → j ≠ s.at_ u := by
    -- If j were u's slot, its phase would be "claimed".
    intro j hj h; rw [h] at hj; exact hj hcl
  -- Prove each fact of the structure in turn.
  constructor
  -- One file fewer if the slot held one: the count follows.
  · show s.alive - (if s.desc (s.at_ u) = 0 then 0 else 1) =
      countOpen (set s.desc (s.at_ u) 0) cap
    -- How the count moves when the slot's opening becomes 0.
    have h := countOpen_set s.desc (s.at_ u) 0 cap hlt
    -- Rewrite the goal with the equalities given.
    rw [hs.alive_eq]
    -- With a file there, one fewer; with none, the same.
    by_cases hd : s.desc (s.at_ u) = 0 <;> simp [hd] at h ⊢ <;> omega
  -- Past the table: not `u`'s slot.
  · intro j hj
    -- First show `j ≠ s.at_ u`, named `hji`.
    have hji : j ≠ s.at_ u := by omega
    -- Read through `set` at a key it did not change: the old value, then the state's `outside` fact from before the step gives exactly this.
    simp only [set_other _ _ _ _ hji]; exact hs.outside j hj
  -- Claimed slots: readers unchanged.
  · exact hs.claimed_free
  -- Empty slots: not `u`'s (claimed); unchanged.
  · intro j hj
    -- Keep this fact as `hji`.
    have hji := hne j (by rw [hj]; simp)
    -- Read through `set` at a key it did not change: the old value, then the state's `empty_clean` fact from before the step gives exactly this.
    simp only [set_other _ _ _ _ hji]; exact hs.empty_clean j hj
  -- Open slots: not `u`'s (claimed); unchanged.
  · intro j hj
    -- Keep this fact as `hji`.
    have hji := hne j (by rw [hj]; simp)
    -- Read through `set` at a key it did not change: the old value, then the state's `open_file` fact from before the step gives exactly this.
    simp only [set_other _ _ _ _ hji]; exact hs.open_file j hj
  -- Readers: unchanged; none is `u` (it is claiming).
  · intro j v hv
    -- Unpack the facts on the right and name them.
    obtain ⟨hr, hat⟩ := hs.holder j v hv
    -- First show `v ≠ u`, named `hvu`, then replace one side of `h` by the other everywhere, then rewrite `hr` with the equalities given, then simplify the named facts with the definitions; a false one closes the goal.
    have hvu : v ≠ u := by intro h; subst h; rw [hpc] at hr; simp [Pc.reading] at hr
    -- Read through `set` at a key it did not change: the old value, then put the pieces together.
    simp only [set_other _ _ _ _ hvu]; exact ⟨hr, hat⟩
  -- Reading threads: not `u`; their slot is open, so not `u`'s.
  · intro v hv
    -- First show `v ≠ u`, named `hvu`, then replace one side of `h` by the other everywhere, then simplify the named facts with the definitions; a false one closes the goal.
    have hvu : v ≠ u := by intro h; subst h; simp [Pc.reading] at hv
    -- Read through `set` at a key it did not change: the old value.
    simp only [set_other _ _ _ _ hvu] at hv
    -- The state's `reader` fact from before the step gives exactly this.
    exact hs.reader v hv
  -- Past the check: not `u`; its slot is open, so its file is unchanged.
  · intro v hv
    -- First show `v ≠ u`, named `hvu`, then replace one side of `h` by the other everywhere, then simplify the named facts with the definitions; a false one closes the goal.
    have hvu : v ≠ u := by intro h; subst h; simp [Pc.through] at hv
    -- Read through `set` at a key it did not change: the old value.
    simp only [set_other _ _ _ _ hvu] at hv
    -- Keep this fact as `hop`.
    have hop := (hs.reader v (by rcases hv with h | h <;> simp [Pc.reading, h])).2.2
    -- Keep this fact as `hvi`.
    have hvi := hne (s.at_ v) (by rw [hop]; simp)
    -- Read through `set` at a key it did not change: the old value, then the state's `through` fact from before the step gives exactly this.
    simp only [set_other _ _ _ _ hvi]; exact hs.through v hv
  -- Threads that read: not `u`; unchanged.
  · intro v hv
    -- First show `v ≠ u`, named `hvu`, then replace one side of `h` by the other everywhere, then simplify the named facts with the definitions; a false one closes the goal.
    have hvu : v ≠ u := by intro h; subst h; simp at hv
    -- Read through `set` at a key it did not change: the old value, then the state's `got_own` fact from before the step gives exactly this.
    simp only [set_other _ _ _ _ hvu] at hv; exact hs.got_own v hv
  -- Claimers: `u` still holds its slot; the others keep theirs.
  · intro v hv
    -- Split on whether `v = u` holds.
    by_cases hvu : v = u
    -- Replace one side of `hvu` by the other everywhere.
    · subst hvu
      -- Prove the parts one at a time; each `?_` becomes a goal of its own.
      refine ⟨hlt, hcl, ?_⟩
      -- Name the things the goal is about: `w`, `hw`, `hwo`.
      intro w hw hwo
      -- Read through `set` at a key it did not change: the old value, then this is exactly what the goal asks.
      simp only [set_other _ _ _ _ hw] at hwo; exact huniq w hw hwo
    -- Read through `set` at a key it did not change: the old value.
    · simp only [set_other _ _ _ _ hvu] at hv ⊢
      -- Unpack the facts on the right and name them.
      obtain ⟨hlt', hcl', huniq'⟩ := hs.owner_claim v hv
      -- Prove the parts one at a time; each `?_` becomes a goal of its own.
      refine ⟨hlt', hcl', ?_⟩
      -- Name the things the goal is about: `w`, `hw`, `hwo`.
      intro w hw hwo
      -- Split on whether `w = u` holds.
      by_cases hwu : w = u
      -- Replace one side of `hwu` by the other everywhere, then this is exactly what the goal asks.
      · subst hwu; exact huniq v hvu hv |>.symm
      -- Read through `set` at a key it did not change: the old value, then this is exactly what the goal asks.
      · simp only [set_other _ _ _ _ hwu] at hwo; exact huniq' w hw hwo
  -- Openings: slot `u` holds 0 now, below every number; others as before.
  · refine ⟨hs.fresh.1, ?_⟩
    -- Name the things the goal is about: `j`.
    intro j
    -- Split on whether `j = s.at_ u` holds.
    by_cases hji : j = s.at_ u
    -- Replace one side of `hji` by the other everywhere, then simplify with the definitions until the goal is closed or plain, then the state's `fresh` fact from before the step gives exactly this.
    · subst hji; simp; exact hs.fresh.1
    -- Read through `set` at a key it did not change: the old value, then the state's `fresh` fact from before the step gives exactly this.
    · simp only [set_other _ _ _ _ hji]; exact hs.fresh.2 j
  -- Closers: `u` holds no file now; the others are at other slots.
  · intro v hv
    -- Split on whether `v = u` holds.
    by_cases hvu : v = u
    -- Replace one side of `hvu` by the other everywhere, then simplify with the definitions until the goal is closed or plain.
    · subst hvu; simp
    -- Read through `set` at a key it did not change: the old value.
    · simp only [set_other _ _ _ _ hvu] at hv
      -- First show `s.at_ v ≠ s.at_ u`, named `hvj`.
      have hvj : s.at_ v ≠ s.at_ u := huniq v hvu (by simp [Pc.owning, hv])
      -- Read through `set` at a key it did not change: the old value, then the state's `closed` fact from before the step gives exactly this.
      simp only [set_other _ _ _ _ hvj]; exact hs.closed v hv

/-- **Open.** Claimer `u` opens its table in its slot (as opening `next`)
and publishes it open, counting itself as the one reader. -/
theorem inv_open {cap : Nat} {want : Nat → Nat} {s : St} (hs : Inv cap want s)
    -- The claim itself; its proof follows.
    (u : Nat) (hpc : s.pc u = .opening) :
    -- The claim, which continues on the lines below.
    Inv cap want { s with
      desc := set s.desc (s.at_ u) s.next, fileOf := set s.fileOf s.next (want u),
      owner := set s.owner (s.at_ u) (want u), phase := set s.phase (s.at_ u) .open_,
      holds := set s.holds (s.at_ u) (set (s.holds (s.at_ u)) u true),
      alive := s.alive + 1, next := s.next + 1,
      via := set s.via u s.next, pc := set s.pc u .read } := by
  -- `u` holds its slot claimed, in the table, alone, with no file in it.
  obtain ⟨hlt, hcl, huniq⟩ := hs.owner_claim u (by simp [Pc.owning, hpc])
  -- Keep this fact as `hzero`.
  have hzero := hs.closed u hpc
  -- Unpack the facts on the right and name them.
  obtain ⟨hpos, hbelow⟩ := hs.fresh
  -- Any slot that is not claimed is not `u`'s.
  have hne : ∀ j, s.phase j ≠ .claimed → j ≠ s.at_ u := by
    -- If j were u's slot, its phase would be "claimed".
    intro j hj h; rw [h] at hj; exact hj hcl
  -- `u`'s slot had no reader.
  have hnoread := hs.claimed_free (s.at_ u) hcl
  -- Prove each fact of the structure in turn.
  constructor
  -- One file more: the slot went from no file to opening `next`.
  · show s.alive + 1 = countOpen (set s.desc (s.at_ u) s.next) cap
    -- How the count moves when the empty slot gets opening `next`.
    have h := countOpen_set s.desc (s.at_ u) s.next cap hlt
    -- First show `s.next ≠ 0`, named `hn`.
    have hn : s.next ≠ 0 := by omega
    -- Rewrite the goal with the equalities given.
    rw [hs.alive_eq]
    -- The slot held no file and now holds one: one more.
    simp [hzero, hn] at h
    -- The arithmetic over natural numbers that is left holds.
    omega
  -- Past the table: not `u`'s slot.
  · intro j hj
    -- First show `j ≠ s.at_ u`, named `hji`.
    have hji : j ≠ s.at_ u := by omega
    -- Read through `set` at a key it did not change: the old value, then the state's `outside` fact from before the step gives exactly this.
    simp only [set_other _ _ _ _ hji]; exact hs.outside j hj
  -- Claimed slots: `u`'s is open now; others as before.
  · intro j hj v
    -- First show `j ≠ s.at_ u`, named `hji`, then replace one side of `h` by the other everywhere, then simplify the named facts with the definitions; a false one closes the goal.
    have hji : j ≠ s.at_ u := by intro h; subst h; simp at hj
    -- Read through `set` at a key it did not change: the old value, then the state's `claimed_free` fact from before the step gives exactly this.
    simp only [set_other _ _ _ _ hji] at hj ⊢; exact hs.claimed_free j hj v
  -- Empty slots: `u`'s is open now; others as before.
  · intro j hj
    -- First show `j ≠ s.at_ u`, named `hji`, then replace one side of `h` by the other everywhere, then simplify the named facts with the definitions; a false one closes the goal.
    have hji : j ≠ s.at_ u := by intro h; subst h; simp at hj
    -- Read through `set` at a key it did not change: the old value, then the state's `empty_clean` fact from before the step gives exactly this.
    simp only [set_other _ _ _ _ hji] at hj ⊢; exact hs.empty_clean j hj
  -- Open slots: `u`'s holds opening `next`, of table `want u`; another
  -- holds an older opening, whose table is unchanged.
  · intro j hj
    -- Split on whether `j = s.at_ u` holds.
    by_cases hji : j = s.at_ u
    -- Replace one side of `hji` by the other everywhere, then simplify with the definitions until the goal is closed or plain, then the arithmetic over natural numbers that is left holds.
    · subst hji; simp; omega
    -- Read through `set` at a key it did not change: the old value.
    · simp only [set_other _ _ _ _ hji] at hj ⊢
      -- Unpack the facts on the right and name them.
      obtain ⟨hd, hf⟩ := hs.open_file j hj
      -- First show `s.desc j ≠ s.next`, named `hnext`.
      have hnext : s.desc j ≠ s.next := Nat.ne_of_lt (hbelow j)
      -- Read through `set` at a key it did not change: the old value, then put the pieces together.
      simp only [set_other _ _ _ _ hnext]; exact ⟨hd, hf⟩
  -- Readers: `u` at its slot now; nobody else was there.
  · intro j v hv
    -- Split on whether `j = s.at_ u` holds.
    by_cases hji : j = s.at_ u
    -- Replace one side of `hji` by the other everywhere.
    · subst hji
      -- Split on whether `v = u` holds.
      by_cases hvu : v = u
      -- Replace one side of `hvu` by the other everywhere, then simplify with the definitions until the goal is closed or plain.
      · subst hvu; simp [Pc.reading]
      -- Read through `set`: the new value at the key it wrote, the old value elsewhere, then rewrite `hv` with the equalities given, then split on the ways `hv` can hold: none is left.
      · simp only [set_same, set_other _ _ _ _ hvu] at hv; rw [hnoread v] at hv; cases hv
    -- Read through `set` at a key it did not change: the old value.
    · simp only [set_other _ _ _ _ hji] at hv
      -- Unpack the facts on the right and name them.
      obtain ⟨hr, hat⟩ := hs.holder j v hv
      -- First show `v ≠ u`, named `hvu`, then replace one side of `h` by the other everywhere, then rewrite `hr` with the equalities given, then simplify the named facts with the definitions; a false one closes the goal.
      have hvu : v ≠ u := by intro h; subst h; rw [hpc] at hr; simp [Pc.reading] at hr
      -- Read through `set` at a key it did not change: the old value, then put the pieces together.
      simp only [set_other _ _ _ _ hvu]; exact ⟨hr, hat⟩
  -- Reading threads: `u` at its open slot; others at other, open slots.
  · intro v hv
    -- Split on whether `v = u` holds.
    by_cases hvu : v = u
    -- Replace one side of `hvu` by the other everywhere, then simplify with the definitions until the goal is closed or plain.
    · subst hvu; simp [hlt]
    -- Read through `set` at a key it did not change: the old value.
    · simp only [set_other _ _ _ _ hvu] at hv ⊢
      -- Unpack the facts on the right and name them.
      obtain ⟨hh, hlt', hop⟩ := hs.reader v hv
      -- Keep this fact as `hvi`.
      have hvi := hne (s.at_ v) (by rw [hop]; simp)
      -- Read through `set` at a key it did not change: the old value, then put the pieces together.
      simp only [set_other _ _ _ _ hvi]; exact ⟨hh, hlt', hop⟩
  -- Past the check: `u` reads opening `next`, its own table; others as
  -- before, at other slots.
  · intro v hv
    -- Split on whether `v = u` holds.
    by_cases hvu : v = u
    -- Replace one side of `hvu` by the other everywhere, then simplify with the definitions until the goal is closed or plain.
    · subst hvu; simp
    -- Read through `set` at a key it did not change: the old value.
    · simp only [set_other _ _ _ _ hvu] at hv ⊢
      -- Keep this fact as `hop`.
      have hop := (hs.reader v (by rcases hv with h | h <;> simp [Pc.reading, h])).2.2
      -- Keep this fact as `hvi`.
      have hvi := hne (s.at_ v) (by rw [hop]; simp)
      -- Read through `set` at a key it did not change: the old value, then the state's `through` fact from before the step gives exactly this.
      simp only [set_other _ _ _ _ hvi]; exact hs.through v hv
  -- Threads that read: not `u` (it is about to read); unchanged.
  · intro v hv
    -- First show `v ≠ u`, named `hvu`, then replace one side of `h` by the other everywhere, then simplify the named facts with the definitions; a false one closes the goal.
    have hvu : v ≠ u := by intro h; subst h; simp at hv
    -- Read through `set` at a key it did not change: the old value, then the state's `got_own` fact from before the step gives exactly this.
    simp only [set_other _ _ _ _ hvu] at hv ⊢; exact hs.got_own v hv
  -- Claimers: not `u` any more; the others at other slots, unchanged.
  · intro v hv
    -- First show `v ≠ u`, named `hvu`, then replace one side of `h` by the other everywhere, then simplify the named facts with the definitions; a false one closes the goal.
    have hvu : v ≠ u := by intro h; subst h; simp [Pc.owning] at hv
    -- Read through `set` at a key it did not change: the old value.
    simp only [set_other _ _ _ _ hvu] at hv ⊢
    -- Unpack the facts on the right and name them.
    obtain ⟨hlt', hcl', huniq'⟩ := hs.owner_claim v hv
    -- First show `s.at_ v ≠ s.at_ u`, named `hvj`.
    have hvj : s.at_ v ≠ s.at_ u := huniq v hvu hv
    -- Read through `set` at a key it did not change: the old value.
    simp only [set_other _ _ _ _ hvj]
    -- Prove the parts one at a time; each `?_` becomes a goal of its own.
    refine ⟨hlt', hcl', ?_⟩
    -- Name the things the goal is about: `w`, `hw`, `hwo`.
    intro w hw hwo
    -- First show `w ≠ u`, named `hwu`, then replace one side of `h` by the other everywhere, then simplify the named facts with the definitions; a false one closes the goal.
    have hwu : w ≠ u := by intro h; subst h; simp [Pc.owning] at hwo
    -- Read through `set` at a key it did not change: the old value, then this is exactly what the goal asks.
    simp only [set_other _ _ _ _ hwu] at hwo ⊢; exact huniq' w hw hwo
  -- Openings: `next` moves on; `u`'s slot holds the old `next`.
  · refine ⟨by show 0 < s.next + 1; omega, ?_⟩
    -- Name the things the goal is about: `j`.
    intro j
    -- Split on whether `j = s.at_ u` holds.
    by_cases hji : j = s.at_ u
    -- Replace one side of `hji` by the other everywhere, then simplify with the definitions until the goal is closed or plain.
    · subst hji; simp
    -- Read through `set` at a key it did not change: the old value, then keep this fact for the next step, then the arithmetic over natural numbers that is left holds.
    · simp only [set_other _ _ _ _ hji]; have := hbelow j; omega
  -- Closers: not `u`; at other slots, unchanged.
  · intro v hv
    -- First show `v ≠ u`, named `hvu`, then replace one side of `h` by the other everywhere, then simplify the named facts with the definitions; a false one closes the goal.
    have hvu : v ≠ u := by intro h; subst h; simp at hv
    -- Read through `set` at a key it did not change: the old value.
    simp only [set_other _ _ _ _ hvu] at hv ⊢
    -- First show `s.at_ v ≠ s.at_ u`, named `hvj`.
    have hvj : s.at_ v ≠ s.at_ u := huniq v hvu (by simp [Pc.owning, hv])
    -- Read through `set` at a key it did not change: the old value, then the state's `closed` fact from before the step gives exactly this.
    simp only [set_other _ _ _ _ hvj]; exact hs.closed v hv

/-- **Read.** Thread `u` reads through its file and gets its table. -/
theorem inv_read {cap : Nat} {want : Nat → Nat} {s : St} (hs : Inv cap want s)
    -- The claim itself; its proof follows.
    (u : Nat) (hpc : s.pc u = .read) :
    -- The claim itself; its proof follows.
    Inv cap want { s with got := set s.got u (s.fileOf (s.via u)), pc := set s.pc u .leave } := by
  -- What `u` reads through: its open slot's file, of its own table.
  obtain ⟨hvia, hown⟩ := hs.through u (by simp [Pc.through, hpc])
  -- Keep this fact as `hop`.
  have hop := (hs.reader u (by simp [Pc.reading, hpc])).2.2
  -- Keep this fact as `hfile`.
  have hfile := (hs.open_file (s.at_ u) hop).2
  -- Prove each fact of the structure in turn.
  constructor
  -- No file changes.
  · exact hs.alive_eq
  -- No slot changes.
  · exact hs.outside
  -- No slot changes.
  · exact hs.claimed_free
  -- No slot changes.
  · exact hs.empty_clean
  -- No slot changes.
  · exact hs.open_file
  -- Readers: `u` still reads (leaving); others as before.
  · intro j v hv
    -- Unpack the facts on the right and name them.
    obtain ⟨hr, hat⟩ := hs.holder j v hv
    -- Split on whether `v = u` holds.
    by_cases hvu : v = u
    -- Replace one side of `hvu` by the other everywhere, then simplify with the definitions until the goal is closed or plain.
    · subst hvu; simp [Pc.reading, hat]
    -- Read through `set` at a key it did not change: the old value, then put the pieces together.
    · simp only [set_other _ _ _ _ hvu]; exact ⟨hr, hat⟩
  -- Reading threads: `u` as before; others as before.
  · intro v hv
    -- Split on whether `v = u` holds.
    by_cases hvu : v = u
    -- Replace one side of `hvu` by the other everywhere, then the state's `reader` fact from before the step gives exactly this.
    · subst hvu; exact hs.reader v (by simp [Pc.reading, hpc])
    -- Read through `set` at a key it did not change: the old value, then the state's `reader` fact from before the step gives exactly this.
    · simp only [set_other _ _ _ _ hvu] at hv; exact hs.reader v hv
  -- Past the check: `u` as before; others as before.
  · intro v hv
    -- Split on whether `v = u` holds.
    by_cases hvu : v = u
    -- Replace one side of `hvu` by the other everywhere, then put the pieces together.
    · subst hvu; exact ⟨hvia, hown⟩
    -- Read through `set` at a key it did not change: the old value, then the state's `through` fact from before the step gives exactly this.
    · simp only [set_other _ _ _ _ hvu] at hv; exact hs.through v hv
  -- Threads that read: `u` got its file's table, which is its own.
  · intro v hv
    -- Split on whether `v = u` holds.
    by_cases hvu : v = u
    -- Replace one side of `hvu` by the other everywhere, then read the value `set` just wrote at this key, then rewrite the goal with the equalities given.
    · subst hvu; simp only [set_same]; rw [hvia, hfile, hown]
    -- Read through `set` at a key it did not change: the old value, then the state's `got_own` fact from before the step gives exactly this.
    · simp only [set_other _ _ _ _ hvu] at hv ⊢; exact hs.got_own v hv
  -- Claimers: not `u`.
  · intro v hv
    -- First show `v ≠ u`, named `hvu`, then replace one side of `h` by the other everywhere, then simplify the named facts with the definitions; a false one closes the goal.
    have hvu : v ≠ u := by intro h; subst h; simp [Pc.owning] at hv
    -- Read through `set` at a key it did not change: the old value.
    simp only [set_other _ _ _ _ hvu] at hv
    -- Unpack the facts on the right and name them.
    obtain ⟨hlt, hcl, huniq⟩ := hs.owner_claim v hv
    -- Prove the parts one at a time; each `?_` becomes a goal of its own.
    refine ⟨hlt, hcl, ?_⟩
    -- Name the things the goal is about: `w`, `hw`, `hwo`.
    intro w hw hwo
    -- First show `w ≠ u`, named `hwu`, then replace one side of `h` by the other everywhere, then simplify the named facts with the definitions; a false one closes the goal.
    have hwu : w ≠ u := by intro h; subst h; simp [Pc.owning] at hwo
    -- Read through `set` at a key it did not change: the old value, then this is exactly what the goal asks.
    simp only [set_other _ _ _ _ hwu] at hwo; exact huniq w hw hwo
  -- No opening changes.
  · exact hs.fresh
  -- Closers: not `u`.
  · intro v hv
    -- First show `v ≠ u`, named `hvu`, then replace one side of `h` by the other everywhere, then simplify the named facts with the definitions; a false one closes the goal.
    have hvu : v ≠ u := by intro h; subst h; simp at hv
    -- Read through `set` at a key it did not change: the old value, then the state's `closed` fact from before the step gives exactly this.
    simp only [set_other _ _ _ _ hvu] at hv; exact hs.closed v hv

/-- Every step keeps every fact. -/
theorem inv_step {cap : Nat} {want : Nat → Nat} {s t : St} (hs : Inv cap want s)
    -- Another premise of the claim, named so the proof can use it.
    (hst : Step cap want s t) : Inv cap want t := by
  -- One lemma per kind of step.
  cases hst with
  -- A thread joins a slot's readers.
  | join u i hpc hi hph => exact inv_join hs u i hpc hi hph
  -- A recheck that passes.
  | recheckOk u hpc ho => exact inv_recheckOk hs u hpc ho
  -- A recheck that fails: the thread stops counting as a reader.
  | recheckFail u hpc _ => exact inv_drop_out hs u (by simp [Pc.reading, hpc])
  -- A claim.
  | claim u i hpc hi hfree => exact inv_claim hs u i hpc hi hfree
  -- A close.
  | close u hpc => exact inv_close hs u hpc
  -- An open.
  | open_ u hpc => exact inv_open hs u hpc
  -- A read.
  | read u hpc => exact inv_read hs u hpc
  -- A leave: the thread stops counting as a reader.
  | leave u hpc => exact inv_drop_out hs u (by simp [Pc.reading, hpc])

/-- Every reachable state keeps every fact. -/
theorem inv_reach {cap : Nat} {want : Nat → Nat} {s : St} (h : Reach cap want s) :
    -- The claim itself; its proof follows.
    Inv cap want s := by
  -- By how `s` was reached: the start, or one step from a reachable state.
  induction h with
  -- The start.
  | init => exact inv_init cap want
  -- One more step.
  | step _ hst ih => exact inv_step ih hst

/-! ## Part 4. The three rules -/

/-- **`open_le_cap`.** Never more open files than slots. -/
theorem open_le_cap {cap : Nat} {want : Nat → Nat} {s : St} (h : Reach cap want s) :
    -- The claim itself; its proof follows.
    s.alive ≤ cap := by
  -- The open files are the slots holding one, at most one each.
  rw [(inv_reach h).alive_eq]
  -- This is exactly what the goal asks.
  exact countOpen_le _ _

/-- **`never_closes_in_use`.** The file a thread reads through is open, in
an open slot of the table, for as long as it reads. -/
theorem never_closes_in_use {cap : Nat} {want : Nat → Nat} {s : St} (h : Reach cap want s)
    -- The claim itself; its proof follows.
    (v : Nat) (hv : (s.pc v).through) :
    -- The claim itself; its proof follows.
    s.via v ≠ 0 ∧ ∃ i, i < cap ∧ s.phase i = .open_ ∧ s.desc i = s.via v := by
  -- The facts of the reachable state.
  have hs := inv_reach h
  -- It is a reader at its slot, which is open and in the table ...
  obtain ⟨_, hlt, hop⟩ := hs.reader v (by rcases hv with h | h <;> simp [Pc.reading, h])
  -- ... and it reads through that slot's file, which is a real file.
  have hvia := (hs.through v hv).1
  -- Keep this fact as `hd`.
  have hd := (hs.open_file (s.at_ v) hop).1
  -- Put the pieces together.
  exact ⟨by rw [hvia]; exact hd, s.at_ v, hlt, hop, hvia.symm⟩

/-- **`reads_own_file`.** A read returns the table its thread asked for. -/
theorem reads_own_file {cap : Nat} {want : Nat → Nat} {s : St} (h : Reach cap want s)
    -- The claim itself; its proof follows.
    (v : Nat) (hv : s.pc v = .leave) : s.got v = want v :=
  -- One of the facts every reachable state keeps.
  (inv_reach h).got_own v hv

/-! ## Part 5. The RED cases -/

/-- The steps with a claim that ignores readers (bug `IgnoreReaders`): an
open slot can be claimed while someone reads it. -/
inductive IgnoreStep (cap : Nat) (want : Nat → Nat) : St → St → Prop
  /-- The same steps as the real code ... -/
  | real {s t : St} : Step cap want s t → IgnoreStep cap want s t
  /-- ... plus a claim of an open slot, whatever its readers. -/
  | claimAny (s : St) (u i : Nat) (hpc : s.pc u = .start) (hi : i < cap)
      -- Another premise of the claim, named so the proof can use it.
      (hph : s.phase i = .open_) :
      -- The state after this step: only the fields named here change.
      IgnoreStep cap want s { s with phase := set s.phase i .claimed,
                                     at_ := set s.at_ u i, pc := set s.pc u .claimed }

/-- The states the bug can reach from the start. -/
inductive IgnoreReach (cap : Nat) (want : Nat → Nat) : St → Prop
  /-- The start is reachable. -/
  | init : IgnoreReach cap want St.init
  /-- One step from a reachable state reaches another. -/
  | step {s t : St} : IgnoreReach cap want s → IgnoreStep cap want s t → IgnoreReach cap want t

/-- **`ignore_readers_closes_in_use`.** With one slot: thread 1 claims it,
opens table 1, and is about to read; thread 2 claims the slot anyway and
closes table 1's file. Thread 1 now reads through a file no open slot
holds. -/
theorem ignore_readers_closes_in_use :
    -- The statement: under the bug, some reachable state has thread 1 reading through a file,
    ∃ s, IgnoreReach 1 id s ∧ (s.pc 1).through ∧
      -- while no open slot holds that file.
      ¬ ∃ i, i < 1 ∧ s.phase i = .open_ ∧ s.desc i = s.via 1 := by
  -- Thread 1 claims the empty slot 0.
  have r1 := IgnoreReach.step IgnoreReach.init
    -- The rest of the arguments: each condition of the step, checked.
    (IgnoreStep.real (Step.claim (cap := 1) (want := id) St.init 1 0 rfl (by decide) (Or.inl rfl)))
  -- Thread 1 closes the (absent) old file.
  have r2 := IgnoreReach.step r1 (IgnoreStep.real (Step.close _ 1 (by simp [set])))
  -- Thread 1 opens table 1 and is about to read.
  have r3 := IgnoreReach.step r2 (IgnoreStep.real (Step.open_ _ 1 (by simp [set])))
  -- Thread 2 claims slot 0 although thread 1 reads it.
  have r4 := IgnoreReach.step r3
    -- The rest of the arguments: each condition of the step, checked.
    (IgnoreStep.claimAny _ 2 0 (by simp [set, St.init]) (by decide) (by simp [set]))
  -- Thread 2 closes table 1's file.
  have r5 := IgnoreReach.step r4 (IgnoreStep.real (Step.close _ 2 (by simp [set])))
  -- That final state is the witness.
  refine ⟨_, r5, by simp [set, Pc.through], ?_⟩
  -- The only slot is claimed now, not open: no open slot holds the file.
  simp [set]

/-- The steps with a join that skips the owner check (bug
`NoOwnerRecheck`): a thread reads through whatever the slot it joined
holds. -/
inductive NoRecheckStep (cap : Nat) (want : Nat → Nat) : St → St → Prop
  /-- The same steps as the real code ... -/
  | real {s t : St} : Step cap want s t → NoRecheckStep cap want s t
  /-- ... plus a join that goes straight to reading the slot's file. -/
  | joinRead (s : St) (u i : Nat) (hpc : s.pc u = .start) (hi : i < cap)
      -- Another premise of the claim, named so the proof can use it.
      (hph : s.phase i = .open_) :
      -- The state after this step: only the fields named here change.
      NoRecheckStep cap want s { s with holds := set s.holds i (set (s.holds i) u true),
                                        at_ := set s.at_ u i, via := set s.via u (s.desc i),
                                        pc := set s.pc u .read }

/-- The states the bug can reach from the start. -/
inductive NoRecheckReach (cap : Nat) (want : Nat → Nat) : St → Prop
  /-- The start is reachable. -/
  | init : NoRecheckReach cap want St.init
  /-- One step from a reachable state reaches another. -/
  | step {s t : St} : NoRecheckReach cap want s → NoRecheckStep cap want s t →
      NoRecheckReach cap want t

/-- **`no_recheck_reads_other`.** With one slot: thread 2 loads table 2,
reads and leaves; thread 1, which wants table 1, joins the open slot without
checking its owner and reads table 2. -/
theorem no_recheck_reads_other :
    -- The claim itself; its proof follows.
    ∃ s, NoRecheckReach 1 id s ∧ s.pc 1 = .leave ∧ s.got 1 ≠ 1 := by
  -- Thread 2 claims the empty slot, closes nothing, opens table 2.
  have r1 := NoRecheckReach.step NoRecheckReach.init
    -- The rest of the arguments: each condition of the step, checked.
    (NoRecheckStep.real (Step.claim (cap := 1) (want := id) St.init 2 0 rfl (by decide) (Or.inl rfl)))
  -- Keep this fact as `r2`.
  have r2 := NoRecheckReach.step r1 (NoRecheckStep.real (Step.close _ 2 (by simp [set])))
  -- Keep this fact as `r3`.
  have r3 := NoRecheckReach.step r2 (NoRecheckStep.real (Step.open_ _ 2 (by simp [set])))
  -- Thread 2 reads and leaves.
  have r4 := NoRecheckReach.step r3 (NoRecheckStep.real (Step.read _ 2 (by simp [set])))
  -- Keep this fact as `r5`.
  have r5 := NoRecheckReach.step r4 (NoRecheckStep.real (Step.leave _ 2 (by simp [set])))
  -- Thread 1 joins the open slot and, with no check, reads its file.
  have r6 := NoRecheckReach.step r5
    -- The rest of the arguments: each condition of the step, checked.
    (NoRecheckStep.joinRead _ 1 0 (by simp [set, St.init]) (by decide) (by simp [set]))
  -- Keep this fact as `r7`.
  have r7 := NoRecheckReach.step r6 (NoRecheckStep.real (Step.read _ 1 (by simp [set])))
  -- That final state is the witness: thread 1 got table 2.
  exact ⟨_, r7, by simp [set], by simp [set]⟩

/-- The steps with a way around the table (bug `OutsideTable`): a thread
that finds no slot to claim opens its file anyway, outside the table. -/
inductive OutsideStep (cap : Nat) (want : Nat → Nat) : St → St → Prop
  /-- The same steps as the real code ... -/
  | real {s t : St} : Step cap want s t → OutsideStep cap want s t
  /-- ... plus an opening that sits in no slot. -/
  | openOutside (s : St) (u : Nat) (hpc : s.pc u = .start) :
      -- The state after this step: only the fields named here change.
      OutsideStep cap want s { s with alive := s.alive + 1, next := s.next + 1,
                                      fileOf := set s.fileOf s.next (want u),
                                      via := set s.via u s.next, pc := set s.pc u .read }

/-- The states the bug can reach from the start. -/
inductive OutsideReach (cap : Nat) (want : Nat → Nat) : St → Prop
  /-- The start is reachable. -/
  | init : OutsideReach cap want St.init
  /-- One step from a reachable state reaches another. -/
  | step {s t : St} : OutsideReach cap want s → OutsideStep cap want s t →
      OutsideReach cap want t

/-- **`outside_table_over_cap`.** With one slot: thread 1 opens table 1 in
it and reads; thread 2 finds no free slot and opens table 2 outside the
table. Two files are open; the limit is one. -/
theorem outside_table_over_cap : ∃ s, OutsideReach 1 id s ∧ s.alive > 1 := by
  -- Thread 1 claims the empty slot, closes nothing, opens table 1.
  have r1 := OutsideReach.step OutsideReach.init
    -- The rest of the arguments: each condition of the step, checked.
    (OutsideStep.real (Step.claim (cap := 1) (want := id) St.init 1 0 rfl (by decide) (Or.inl rfl)))
  -- Keep this fact as `r2`.
  have r2 := OutsideReach.step r1 (OutsideStep.real (Step.close _ 1 (by simp [set])))
  -- Keep this fact as `r3`.
  have r3 := OutsideReach.step r2 (OutsideStep.real (Step.open_ _ 1 (by simp [set])))
  -- Thread 2 opens table 2 with no slot.
  have r4 := OutsideReach.step r3 (OutsideStep.openOutside _ 2 (by simp [set, St.init]))
  -- That final state is the witness: two open files.
  exact ⟨_, r4, by simp [set, St.init]⟩

-- The end of this file's names.
end Regolith.OpenFileTable
