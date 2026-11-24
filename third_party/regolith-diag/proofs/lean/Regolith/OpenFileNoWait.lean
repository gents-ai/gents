-- The slot table this file extends: its slot phases and its one-key update (`set`).
import Regolith.OpenFileTable

/-!
# OpenFileNoWait: a queue unit never waits for a slot, and is never forgotten

This file extends `Regolith/OpenFileTable.lean` with decision D60, and backs
the second story of the TLA+ model `proofs/tla/OpenFileTable.tla`
(`NeverWaits` and `RetryNeverLost`). TLC checks them for one or two slots and
two or three threads; here they are proved for every number of slots,
threads and steps.

## The story, for a reader who has never seen the code

Under `max_open_files`, every open table file lives in one of `cap` slots
(`SlotTable`, `src/env/open_file_limit/slots.rs`). Some reads promise never
to wait for another thread: a `CacheOnly` read's device work runs later as a
*unit* on its thread's own I/O queue (`src/engine/io`). When that unit must
reopen its table while every slot is busy with other threads' reads, it may
not wait for a slot (a `Blocking` read may). It *parks* instead:

1. it puts its queue on the table's parked list (`register`);
2. it looks at each slot again: one that freed it claims, each busy one it
   marks WANTED (`mark`);
3. the unit is parked and the poll returns (`park`).

The read that frees a WANTED slot clears the mark and tells every queue on
the list "a slot freed" (`clearW`, `wake`). The message waits in the queue's
inbox, and the queue's next poll unparks the unit and runs it again (`poll`).

Tiny example. One slot. Thread 1 (Blocking) reads table A in it. Thread 2's
unit wants table B: it joins the list, marks the slot, parks. Thread 1
leaves: the slot is free and WANTED, so thread 1 clears the mark and tells
thread 2. Thread 2 polls and runs its unit again, which now claims the slot.

## What is proved, in plain words

1. `never_waits` (here): a queue unit that is in the middle of its read
   always has a step of its own to take, whatever every other thread is
   doing (TLA+ `NeverWaits`).
2. `retry_never_lost` (`Regolith/OpenFileNoWaitProof.lean`, from the facts
   `Inv` and the lemmas here): a parked unit already has its message, or
   will get one: its queue is on the list and some slot is WANTED and busy,
   or a reader that freed a WANTED slot is on its way to waking the list
   (TLA+ `RetryNeverLost`).
3. The RED cases, as counterexamples (`Regolith/OpenFileNoWaitRed.lean`): a
   unit that waits for a slot like a Blocking read (`waits_waits`); a park
   that marks before it joins the list (`mark_first_loses_retry`); a publish
   that drops the mark (`publish_drops_loses_retry`); a wake that tells only
   units already parked (`wake_needs_parked_loses_retry`).

## What is left out, and why

Which table a slot holds, its file, and the owner check of a join are what
`OpenFileTable.lean` proves things about; whether a unit is woken depends
only on whether slots are busy, so here a slot is its phase, its reader
count and its WANTED bit. A join is one step (the owner check's failures
only send a thread back to its start). Memory ordering is the loom models'
job (`tests/loom_tables.rs`).

## How to read the Lean

`def` defines a thing, `theorem` states a fact and its proof follows
`:= by`. Lines starting with `--` are comments, in plain words. A proof is a
list of *tactics*; each one changes the goal still to be shown, and the
comment above it says how. `simp` rewrites with known facts; `omega` solves
arithmetic over natural numbers; `cases` splits on the ways a fact could be
true; `induction` proves a fact for every number of steps by proving it for
none and then for one more.
-/

-- Everything below is named Regolith.OpenFileNoWait.<name>.
namespace Regolith.OpenFileNoWait

-- Reuse the slot phases and the one-key update of `OpenFileTable.lean`.
open Regolith.OpenFileTable (Phase set set_same set_other)

/-! ## Part 1. The state and the steps -/

/-- Where a thread is in one read (the `pc` of the TLA+ model). -/
inductive Pc where
  /-- About to read: it may join its slot or go to claim one. -/
  | start
  /-- Wants a slot to load its table into. -/
  | claim
  /-- A unit whose sweep found no slot, about to join the parked list. -/
  | register
  /-- A unit looking at each slot: claim a free one, mark a busy one. -/
  | mark
  /-- A unit whose look claimed a slot, about to wake the parked list. -/
  | parkwake
  /-- A unit that found every slot busy, about to park. -/
  | park
  /-- A parked unit: its poll returned, its read is pending on its queue. -/
  | idle
  /-- Holds a slot claimed; about to open its table there and publish it. -/
  | opening
  /-- Reads through a slot; about to leave it. -/
  | read
  /-- Freed a WANTED slot; about to clear the mark. -/
  | clearW
  /-- Cleared the mark; about to wake the parked list. -/
  | wake
  -- Two steps can be compared for equality.
  deriving DecidableEq

/-- The real protocol, or one of the planted bugs of the TLA+ model. -/
inductive Variant where
  /-- The code as written. -/
  | real
  /-- A unit that finds no free slot waits for one, like a Blocking read. -/
  | waits
  /-- A park marks the slots before it joins the parked list. -/
  | markFirst
  /-- Publishing a loaded slot writes its word whole and drops the mark. -/
  | publishDrops
  /-- A wake tells only the queues whose unit is already parked. -/
  | wakeNeedsParked
  -- Two variants can be compared for equality.
  deriving DecidableEq

/-- Everything about the table and the threads at one moment. Slots and
threads are each named by a natural number. -/
structure St where
  /-- Each slot's phase (the phase bits of `Slot::state`). -/
  phase : Nat → Phase
  /-- Each slot's reader count (the low bits of `Slot::state`). -/
  readers : Nat → Nat
  /-- Each slot's WANTED bit. -/
  wanted : Nat → Bool
  /-- Which queues are on the parked list (`SlotTable::parked`). -/
  list : Nat → Bool
  /-- Which queues have a "slot freed" message waiting (`Message::SlotFreed`). -/
  told : Nat → Bool
  /-- Which threads' units are parked (`Unit`'s PARKED state). -/
  parked : Nat → Bool
  /-- Each thread's step. -/
  pc : Nat → Pc
  /-- The slot each thread holds, reads, or has just freed. -/
  at_ : Nat → Nat
  /-- The next slot each unit's look will examine. -/
  markAt : Nat → Nat

/-- The start: every slot empty and unmarked, nobody on the list, no message,
no unit parked, every thread about to read. -/
def St.init : St :=
  -- Empty, unread, unmarked slots.
  { phase := fun _ => .empty, readers := fun _ => 0, wanted := fun _ => false,
    -- An empty list, no message, nothing parked.
    list := fun _ => false, told := fun _ => false, parked := fun _ => false,
    -- Every thread at its start, at slot 0, its look not begun.
    pc := fun _ => .start, at_ := fun _ => 0, markAt := fun _ => 0 }

/-- Whether slot `j` may be claimed: empty, or open with no reader. -/
def St.free (s : St) (j : Nat) : Bool :=
  -- Look at the slot's phase.
  match s.phase j with
  -- An empty slot may be taken.
  | .empty => true
  -- An open slot may be taken only when nobody reads it.
  | .open_ => s.readers j == 0
  -- A claimed slot belongs to its claimer.
  | .claimed => false

/-- Who a wake leaves a message for: every queue on the list, keeping the
messages already there. The bug `wakeNeedsParked` skips a queue whose unit is
not parked yet. -/
def tell (v : Variant) (s : St) : Nat → Bool :=
  -- A queue keeps its message, or gets one if it was on the list (and, with
  -- the bug, only if its unit is already parked).
  fun u => s.told u || (s.list u && (v != .wakeNeedsParked || s.parked u))

/-- The steps, one atomic step each, as the code takes them. `Step v cap nw u
s s'`: thread `u` moves the state from `s` to `s'`, in variant `v`, with `cap`
slots, and `nw u` says whether `u`'s reads are queue units. -/
inductive Step (v : Variant) (cap : Nat) (nw : Nat → Bool) : Nat → St → St → Prop
  /-- Thread `u` joins open slot `j` (`SlotTable::join`): one more reader. -/
  | join (s : St) (u j : Nat) (hpc : s.pc u = .start) (hj : j < cap)
      -- The slot must be open.
      (hph : s.phase j = .open_) :
      -- The state after this step: only the fields named here change.
      Step v cap nw u s { s with
        -- The slot gains a reader.
        readers := set s.readers j (s.readers j + 1),
        -- The thread is at that slot, reading.
        at_ := set s.at_ u j, pc := set s.pc u .read }
  /-- Thread `u` will load its table into a slot. -/
  | miss (s : St) (u : Nat) (hpc : s.pc u = .start) :
      -- The state after this step: the thread now wants a slot.
      Step v cap nw u s { s with pc := set s.pc u .claim }
  /-- Thread `u` claims free slot `j` with one CAS; the mark stays. -/
  | claim (s : St) (u j : Nat) (hpc : s.pc u = .claim) (hj : j < cap)
      -- The slot must be free.
      (hfree : s.free j = true) :
      -- The state after this step: only the fields named here change.
      Step v cap nw u s { s with
        -- The slot is claimed (its mark, if any, is kept).
        phase := set s.phase j .claimed,
        -- The thread holds it, and will open its table there.
        at_ := set s.at_ u j, pc := set s.pc u .opening }
  /-- A unit's sweep comes back empty-handed (allowed whatever the slots
  hold: the CLOCK sweep can miss). The unit does not wait: it goes to park.
  The bug `waits` has no such step. -/
  | sweepMiss (s : St) (u : Nat) (hpc : s.pc u = .claim) (hnw : nw u = true)
      -- Without the bug that waits.
      (hv : v ≠ .waits) :
      -- The state after this step: only the fields named here change.
      Step v cap nw u s { s with
        -- It joins the list next (the bug `markFirst`: it marks first).
        pc := set s.pc u (if v = .markFirst then .mark else .register),
        -- Its look will start at slot 0.
        markAt := set s.markAt u 0 }
  /-- The unit puts its queue on the parked list (`SlotTable::register`). -/
  | register (s : St) (u : Nat) (hpc : s.pc u = .register) :
      -- The state after this step: only the fields named here change.
      Step v cap nw u s { s with
        -- Its queue is on the list.
        list := set s.list u true,
        -- It looks at the slots next (the bug `markFirst`: it already did, and parks).
        pc := set s.pc u (if v = .markFirst then .park else .mark) }
  /-- The unit's look finds slot `markAt u` busy: its CAS sets WANTED. -/
  | markBusy (s : St) (u : Nat) (hpc : s.pc u = .mark) (hm : s.markAt u < cap)
      -- The slot it looks at is busy.
      (hbusy : s.free (s.markAt u) = false) :
      -- The state after this step: only the fields named here change.
      Step v cap nw u s { s with
        -- The slot is marked WANTED.
        wanted := set s.wanted (s.markAt u) true,
        -- The look moves on to the next slot.
        markAt := set s.markAt u (s.markAt u + 1) }
  /-- The unit's look finds slot `markAt u` free: its CAS claims it. -/
  | markClaim (s : St) (u : Nat) (hpc : s.pc u = .mark) (hm : s.markAt u < cap)
      -- The slot it looks at is free.
      (hfree : s.free (s.markAt u) = true) :
      -- The state after this step: only the fields named here change.
      Step v cap nw u s { s with
        -- The slot is claimed.
        phase := set s.phase (s.markAt u) .claimed,
        -- The unit holds it.
        at_ := set s.at_ u (s.markAt u),
        -- It wakes the list next (the bug `markFirst`, not on the list: it fills).
        pc := set s.pc u (if v = .markFirst then .opening else .parkwake) }
  /-- The unit's look went past the last slot: every slot was busy. -/
  | markDone (s : St) (u : Nat) (hpc : s.pc u = .mark) (hm : cap ≤ s.markAt u) :
      -- The state after this step: it parks next (the bug `markFirst`: it registers).
      Step v cap nw u s { s with
        -- Its next step.
        pc := set s.pc u (if v = .markFirst then .register else .park) }
  /-- A unit whose look claimed a slot wakes the whole list, then fills it. -/
  | parkWake (s : St) (u : Nat) (hpc : s.pc u = .parkwake) :
      -- The state after this step: only the fields named here change.
      Step v cap nw u s { s with
        -- Every queue on the list gets a message, and the list is emptied.
        told := tell v s, list := fun _ => false,
        -- It opens its table in the slot it claimed.
        pc := set s.pc u .opening }
  /-- The unit is parked and its poll returns (`Unit::park`). -/
  | park (s : St) (u : Nat) (hpc : s.pc u = .park) :
      -- The state after this step: only the fields named here change.
      Step v cap nw u s { s with
        -- The unit is parked.
        parked := set s.parked u true,
        -- The thread is idle as far as this read goes.
        pc := set s.pc u .idle }
  /-- The queue's poll finds a message: the unit is unparked and runs again. -/
  | poll (s : St) (u : Nat) (hpc : s.pc u = .idle) (htold : s.told u = true) :
      -- The state after this step: only the fields named here change.
      Step v cap nw u s { s with
        -- The message is taken.
        told := set s.told u false,
        -- The unit is unparked, and its read starts over.
        parked := set s.parked u false, pc := set s.pc u .start }
  /-- The claimer opens its table in its slot and publishes it open with
  itself as a reader (`SlotTable::fill`). The add keeps the mark; the bug
  `publishDrops` loses it. -/
  | open_ (s : St) (u : Nat) (hpc : s.pc u = .opening) :
      -- The state after this step: only the fields named here change.
      Step v cap nw u s { s with
        -- The slot is open ...
        phase := set s.phase (s.at_ u) .open_,
        -- ... with this thread as a reader ...
        readers := set s.readers (s.at_ u) (s.readers (s.at_ u) + 1),
        -- ... and its mark kept, unless the bug drops it.
        wanted := if v = .publishDrops then set s.wanted (s.at_ u) false else s.wanted,
        -- The thread reads.
        pc := set s.pc u .read }
  /-- The thread leaves its slot (`Held::drop`): one decrement. The last
  reader of an open, WANTED slot goes on to clear the mark and wake. -/
  | leave (s : St) (u : Nat) (hpc : s.pc u = .read) :
      -- The state after this step: only the fields named here change.
      Step v cap nw u s { s with
        -- The slot loses a reader.
        readers := set s.readers (s.at_ u) (s.readers (s.at_ u) - 1),
        -- It was the last reader of an open, marked slot: clear and wake next;
        -- otherwise back to the start.
        pc := set s.pc u (if s.readers (s.at_ u) = 1 ∧ s.phase (s.at_ u) = .open_ ∧
                              -- (the slot is marked)
                              s.wanted (s.at_ u) = true then .clearW else .start) }
  /-- The leaver clears the slot's mark (`SlotTable::freed`), before it takes
  the list. -/
  | clearW (s : St) (u : Nat) (hpc : s.pc u = .clearW) :
      -- The state after this step: only the fields named here change.
      Step v cap nw u s { s with
        -- The mark is cleared.
        wanted := set s.wanted (s.at_ u) false,
        -- It wakes the list next.
        pc := set s.pc u .wake }
  /-- The leaver takes the list whole and leaves each queue on it a message
  (`SlotTable::wake`, `QueueShared::slot_freed`). -/
  | wake (s : St) (u : Nat) (hpc : s.pc u = .wake) :
      -- The state after this step: only the fields named here change.
      Step v cap nw u s { s with
        -- Every queue on the list gets a message, and the list is emptied.
        told := tell v s, list := fun _ => false,
        -- The leaver goes back to its start.
        pc := set s.pc u .start }

/-- The states the steps of variant `v` can reach from the start. -/
inductive Reach (v : Variant) (cap : Nat) (nw : Nat → Bool) : St → Prop
  /-- The start is reachable. -/
  | init : Reach v cap nw St.init
  /-- One step, by any thread, from a reachable state reaches another. -/
  | step {s s' : St} (u : Nat) : Reach v cap nw s → Step v cap nw u s s' → Reach v cap nw s'

/-! ## Part 2. A unit never waits -/

/-- **`never_waits`.** In the real code, a queue unit `u` whose read is under
way (it is not parked with its poll returned) always has a step of its own,
in every state, whatever the other threads hold: no step of `u` needs
another thread to move first. TLA+ `NeverWaits`. -/
theorem never_waits (cap : Nat) (nw : Nat → Bool) (s : St) (u : Nat)
    -- `u` is a queue unit, and it is not parked.
    (hnw : nw u = true) (hidle : s.pc u ≠ .idle) :
    -- The claim: some step of `u` exists.
    ∃ s', Step .real cap nw u s s' := by
  -- Split on where `u` is in its read.
  cases hpc : s.pc u with
  -- At its start it can always go and claim a slot.
  | start => exact ⟨_, Step.miss s u hpc⟩
  -- Wanting a slot, its sweep can always come back empty: it parks, never waits.
  | claim => exact ⟨_, Step.sweepMiss s u hpc hnw (by decide)⟩
  -- About to join the list: the push always lands.
  | register => exact ⟨_, Step.register s u hpc⟩
  -- Looking at the slots: one CAS on the next slot, or the look is over.
  | mark =>
    -- Is there a slot left to look at?
    by_cases hm : s.markAt u < cap
    -- Yes: there is a slot left to look at.
    ·
      -- That slot is free or busy, and either way the CAS takes one step.
      cases hf : s.free (s.markAt u)
      -- Busy: it marks the slot.
      · exact ⟨_, Step.markBusy s u hpc hm hf⟩
      -- Free: it claims the slot.
      · exact ⟨_, Step.markClaim s u hpc hm hf⟩
    -- No slot left: the look is done, and the unit goes to park.
    · exact ⟨_, Step.markDone s u hpc (by omega)⟩
  -- About to wake the list it found a slot for: always possible.
  | parkwake => exact ⟨_, Step.parkWake s u hpc⟩
  -- About to park: always possible.
  | park => exact ⟨_, Step.park s u hpc⟩
  -- Parked: excluded by `hidle`.
  | idle => exact absurd hpc hidle
  -- Holding a claimed slot: it opens its table there.
  | opening => exact ⟨_, Step.open_ s u hpc⟩
  -- Reading: it leaves.
  | read => exact ⟨_, Step.leave s u hpc⟩
  -- About to clear a mark: always possible.
  | clearW => exact ⟨_, Step.clearW s u hpc⟩
  -- About to wake the list: always possible.
  | wake => exact ⟨_, Step.wake s u hpc⟩

/-! ## Part 3. A parked unit is never forgotten -/

/-- Some reader freed a WANTED slot and is on its way to waking the list. -/
def St.waking (s : St) : Prop := ∃ r, s.pc r = .clearW ∨ s.pc r = .wake

/-- What a unit that waits to be woken has: its message already, or its
queue on the list and either a wake on its way or every slot it has looked
at still marked. -/
def St.covered (s : St) (t : Nat) : Prop :=
  -- Told already, or listed with a wake coming or its marks in place.
  s.told t = true ∨ (s.list t = true ∧ (s.waking ∨ ∀ j, j < s.markAt t → s.wanted j = true))

/-- The facts every state the real code reaches has. The rule follows. -/
structure Inv (cap : Nat) (s : St) : Prop where
  /-- A unit is parked exactly when its thread is idle. -/
  idle_iff : ∀ t, s.parked t = true ↔ s.pc t = .idle
  /-- A unit about to park has looked at every slot. -/
  park_done : ∀ t, s.pc t = .park → cap ≤ s.markAt t
  /-- A parked unit has looked at every slot. -/
  parked_done : ∀ t, s.parked t = true → cap ≤ s.markAt t
  /-- A unit about to join the list has looked at no slot yet. -/
  register_zero : ∀ t, s.pc t = .register → s.markAt t = 0
  /-- A slot that is WANTED and free has a reader about to clear its mark. -/
  free_wanted : ∀ j, s.wanted j = true → s.free j = true → ∃ r, s.pc r = .clearW ∧ s.at_ r = j
  /-- A unit that is looking, about to park, or parked is covered. -/
  relying : ∀ t, (s.pc t = .mark ∨ s.pc t = .park ∨ s.parked t = true) → s.covered t

/-- The start keeps every fact: nobody parks, looks or marks. -/
theorem inv_init (cap : Nat) : Inv cap St.init := by
  -- Each fact, about the start.
  constructor
  -- Nothing is parked and nobody is idle.
  · intro t; simp [St.init]
  -- Nobody is about to park.
  · intro t h; simp [St.init] at h
  -- Nothing is parked.
  · intro t h; simp [St.init] at h
  -- Nobody is about to join the list.
  · intro t h; simp [St.init] at h
  -- No slot is marked.
  · intro j h; simp [St.init] at h
  -- Nobody is looking, about to park, or parked.
  · intro t h; simp [St.init] at h

/-- A wake on its way stays on its way through a step that moves only `u`,
when `u` was not on its way already. -/
theorem waking_frame {s s' : St} {u : Nat} (hu : s.pc u ≠ .clearW ∧ s.pc u ≠ .wake)
    -- Every other thread's step is unchanged.
    (hpc : ∀ r, r ≠ u → s'.pc r = s.pc r) : s.waking → s'.waking := by
  -- Take the reader on its way.
  rintro ⟨r, hr⟩
  -- It is not `u`, which was not on its way.
  have hru : r ≠ u := by
    -- If it were `u`, `u`'s step would be clearW or wake.
    rintro rfl; rcases hr with h | h <;> simp_all
  -- Its step did not change, so it is still on its way.
  exact ⟨r, by rw [hpc r hru]; exact hr⟩

/-- A covered unit stays covered through a step that keeps its message and
list entry, keeps a wake on its way, keeps its look's position, and only
adds marks. -/
theorem covered_of {s s' : St} {t : Nat} (h : s.covered t)
    -- Its message and its list entry are as they were.
    (htold : s'.told t = s.told t) (hlist : s'.list t = s.list t)
    -- A wake on its way stays on its way.
    (hwake : s.waking → s'.waking)
    -- Its look is where it was, and no mark was removed.
    (hmark : s'.markAt t = s.markAt t) (hwanted : ∀ j, s.wanted j = true → s'.wanted j = true) :
    -- The claim: covered after the step.
    s'.covered t := by
  -- Unfold what covered means.
  unfold St.covered at h ⊢
  -- Rewrite the unchanged fields.
  rw [htold, hlist, hmark]
  -- Each way it was covered carries over.
  rcases h with h | ⟨hl, hw | hw⟩
  -- Told: still told.
  · exact Or.inl h
  -- Listed with a wake coming: still.
  · exact Or.inr ⟨hl, Or.inl (hwake hw)⟩
  -- Listed with its marks: the marks stay.
  · exact Or.inr ⟨hl, Or.inr (fun j hj => hwanted j (hw j hj))⟩

/-- A wake (or a park that claimed) leaves a message for every queue on the
list, so every covered unit is told after it. -/
theorem covered_told {s : St} {t : Nat} (h : s.covered t) :
    -- The claim: after the wake, its message is there.
    tell .real s t = true := by
  -- Unfold the wake and what covered means.
  unfold tell; unfold St.covered at h
  -- Told before, or on the list: either way the message is there.
  rcases h with h | ⟨h, _⟩ <;> simp [h]

/-- Reading `set` at the key it changed or anywhere else. -/
theorem set_apply {α : Type} (f : Nat → α) (k v : Nat) (x : α) :
    -- The claim itself; its proof follows.
    set f k x v = if v = k then x else f v := by
  -- It is the definition.
  rfl

/-- A step that matters to no fact: not looking, parking, idle, about to join
the list, clearing a mark or waking. -/
def Pc.calm (p : Pc) : Prop :=
  -- None of the six steps the facts are about.
  p ≠ .park ∧ p ≠ .idle ∧ p ≠ .mark ∧ p ≠ .register ∧ p ≠ .clearW ∧ p ≠ .wake

/-- A step that moves only `u`, from a calm step to a calm step, changes no
mark, list entry, message, parking or look, and frees no marked slot, keeps
every fact. Joins, misses, claims, publishes and most leaves are such steps. -/
theorem inv_frame {cap : Nat} {s s' : St} {u : Nat} (hs : Inv cap s)
    -- `u` was calm and is calm.
    (hold : (s.pc u).calm) (hnew : (s'.pc u).calm)
    -- Every other thread is where it was, at the slot it was.
    (hpc : ∀ r, r ≠ u → s'.pc r = s.pc r) (hat : ∀ r, r ≠ u → s'.at_ r = s.at_ r)
    -- Parking, messages, the list, every look and every mark are unchanged.
    (hparked : s'.parked = s.parked) (htold : s'.told = s.told) (hlist : s'.list = s.list)
    -- Every look and every mark are unchanged.
    (hmark : s'.markAt = s.markAt) (hwanted : s'.wanted = s.wanted)
    -- A marked slot free now was free before.
    (hfree : ∀ j, s'.wanted j = true → s'.free j = true → s.free j = true) :
    -- The claim: every fact holds after the step.
    Inv cap s' := by
  -- `u` was not idle, so it was not parked.
  have hp : s.parked u = false := by
    -- Look at whether it was parked.
    cases h : s.parked u
    -- It was not.
    · rfl
    -- It was: then it was idle, which `hold` rules out.
    · exact absurd ((hs.idle_iff u).1 h) hold.2.1
  -- Each fact after the step.
  refine ⟨?_, ?_, ?_, ?_, ?_, ?_⟩
  -- Parked iff idle.
  · intro t
    -- Is `t` the thread that moved?
    by_cases htu : t = u
    -- It is: not parked, and not idle after its calm step.
    · subst htu; rw [hparked, hp]
      -- Both sides are false.
      exact ⟨fun h => (by cases h), fun h => absurd h hnew.2.1⟩
    -- It is not: both sides are as they were.
    · rw [hparked, hpc t htu]; exact hs.idle_iff t
  -- About to park: not `u`; anyone else as before.
  · intro t ht
    -- Is `t` the thread that moved?
    by_cases htu : t = u
    -- It is: its calm step is not park.
    · subst htu; exact absurd ht hnew.1
    -- It is not: as before.
    · rw [hpc t htu] at ht; rw [hmark]; exact hs.park_done t ht
  -- Parked: as before.
  · intro t ht; rw [hparked] at ht; rw [hmark]; exact hs.parked_done t ht
  -- About to join the list: not `u`; anyone else as before.
  · intro t ht
    -- Is `t` the thread that moved?
    by_cases htu : t = u
    -- It is: its calm step is not register.
    · subst htu; exact absurd ht hnew.2.2.2.1
    -- It is not: as before.
    · rw [hpc t htu] at ht; rw [hmark]; exact hs.register_zero t ht
  -- Marked and free: it was marked and free before, so a reader was on its way.
  · intro j hw hf
    -- It was free before.
    have hf0 := hfree j hw hf
    -- It was marked before.
    rw [hwanted] at hw
    -- The reader on its way then.
    obtain ⟨r, hr, ha⟩ := hs.free_wanted j hw hf0
    -- It is not `u`, which was calm.
    have hru : r ≠ u := by rintro rfl; exact hold.2.2.2.2.1 hr
    -- It is still there, at the same slot.
    exact ⟨r, by rw [hpc r hru]; exact hr, by rw [hat r hru]; exact ha⟩
  -- Covered: a relying thread is not `u`, and stays covered.
  · intro t ht
    -- `t` is not `u`: `u` is calm and not parked after the step.
    have htu : t ≠ u := by
      -- Suppose it were.
      rintro rfl
      -- Each way of relying contradicts `u`'s calm, unparked step.
      rcases ht with h | h | h
      -- Looking: not calm.
      · exact hnew.2.2.1 h
      -- About to park: not calm.
      · exact hnew.1 h
      -- Parked: it was not parked, and parking did not change.
      · rw [hparked, hp] at h; cases h
    -- `t` relied before the step too.
    have ht0 : s.pc t = .mark ∨ s.pc t = .park ∨ s.parked t = true := by
      -- Its step and its parking are as they were.
      rw [hpc t htu, hparked] at ht; exact ht
    -- It was covered, and nothing it relies on changed.
    exact covered_of (hs.relying t ht0) (by rw [htold]) (by rw [hlist])
      -- A wake on its way stays on its way.
      (waking_frame (u := u) ⟨hold.2.2.2.2.1, hold.2.2.2.2.2⟩ hpc)
      -- Its look and every mark are as they were.
      (by rw [hmark]) (fun j h => by rw [hwanted]; exact h)

-- The end of this file's names.
end Regolith.OpenFileNoWait
