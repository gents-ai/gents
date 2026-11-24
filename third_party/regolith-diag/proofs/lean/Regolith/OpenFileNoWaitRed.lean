-- The model whose buggy variants are run here: its state, steps and rules.
import Regolith.OpenFileNoWait

/-!
# OpenFileNoWaitRed: each planted bug breaks the rule it is planted against

The RED half of `Regolith/OpenFileNoWait.lean` (decision D60). Each theorem
here is a counterexample: a run of the steps of one buggy variant, with one
slot, thread 1 a Blocking read and thread 2 a queue unit, that ends in a
state the rule forbids. They mirror the RED configurations of
`proofs/tla/OpenFileTable.tla`:

- `waits_waits` (`MC_OpenFileTable_Red_Waits`): a unit that waits for a slot
  like a Blocking read has, at some point, no step of its own;
- `mark_first_loses_retry` (`MC_OpenFileTable_Red_MarkFirst`): a park that
  marks before it joins the list is never woken;
- `publish_drops_loses_retry` (`MC_OpenFileTable_Red_PublishDropsWanted`): a
  publish that drops the mark leaves a parked unit nobody will wake;
- `wake_needs_parked_loses_retry` (`MC_OpenFileTable_Red_WakeNeedsParked`): a
  wake that tells only parked units skips one about to park.

Each proof builds the run one step at a time (`have rN := Reach.step ...`,
each step's conditions checked by `simp` or `decide`), then shows the final
state breaks the rule.
-/

-- Everything below is named Regolith.OpenFileNoWait.<name>.
namespace Regolith.OpenFileNoWait

-- Reuse the one-key update of `OpenFileTable.lean`.
open Regolith.OpenFileTable (set set_same set_other)

/-- Thread 2's reads are queue units; every other thread's are Blocking. -/
def unitTwo : Nat → Bool := fun u => u == 2

/-- The rule `retry_never_lost` proves for the real code, for thread 2 with
one slot: told, or listed with a busy marked slot or a wake on its way. -/
def Kept (s : St) : Prop :=
  -- Thread 2's message is there, or it will come.
  s.told 2 = true ∨ (s.list 2 = true ∧
    -- (the only slot busy and marked, or a reader on its way to wake)
    ((∃ j, j < 1 ∧ s.wanted j = true ∧ s.free j = false) ∨ s.waking))

/-- **`waits_waits`.** With the bug that waits: thread 1 claims the only slot
and reads; thread 2's unit wants a slot. No step of thread 2 exists: the slot
has a reader, and the bug has no sweep that comes back empty. The unit sits
at `claim` until thread 1 moves, which is waiting on another thread. -/
theorem waits_waits :
    -- A reachable state of the bug where thread 2 is a unit wanting a slot ...
    ∃ s, Reach .waits 1 unitTwo s ∧ unitTwo 2 = true ∧ s.pc 2 = .claim ∧
      -- ... with no step of its own.
      ¬ ∃ s', Step .waits 1 unitTwo 2 s s' := by
  -- Thread 1 goes to claim a slot.
  have r1 := Reach.step 1 Reach.init
    -- The rest of the arguments: each condition of the step, checked.
    (Step.miss (v := .waits) (cap := 1) (nw := unitTwo) St.init 1 rfl)
  -- Thread 1 claims the empty slot 0.
  have r2 := Reach.step 1 r1 (Step.claim _ 1 0 (by simp [St.init]) (by decide) (by simp [St.free, St.init]))
  -- Thread 1 opens its table there and reads.
  have r3 := Reach.step 1 r2 (Step.open_ _ 1 (by simp [St.init]))
  -- Thread 2 goes to claim a slot.
  have r4 := Reach.step 2 r3 (Step.miss _ 2 (by simp [set_apply, St.init]))
  -- That state is the witness; what is left is that thread 2 has no step.
  refine ⟨_, r4, rfl, by simp, ?_⟩
  -- Suppose thread 2 had a step.
  rintro ⟨s', h⟩
  -- Look at each step it could take; each one's conditions fail.
  cases h <;> simp_all [St.free]

/-- **`mark_first_loses_retry`.** With the park that marks first: thread 1
reads in the only slot; thread 2's unit marks the slot. Thread 1 leaves,
clears the mark and wakes an empty list. Only then does thread 2 join the
list and park: no message, no mark, no wake on its way. -/
theorem mark_first_loses_retry :
    -- A reachable state of the bug where thread 2's unit is parked ...
    ∃ s, Reach .markFirst 1 unitTwo s ∧ s.parked 2 = true ∧ ¬ Kept s := by
  -- Thread 1 goes to claim, claims slot 0, opens its table and reads.
  have r1 := Reach.step 1 Reach.init
    -- The rest of the arguments: each condition of the step, checked.
    (Step.miss (v := .markFirst) (cap := 1) (nw := unitTwo) St.init 1 rfl)
  -- Keep this fact as `r2`: the claim.
  have r2 := Reach.step 1 r1 (Step.claim _ 1 0 (by simp [St.init]) (by decide) (by simp [St.free, St.init]))
  -- Keep this fact as `r3`: the open.
  have r3 := Reach.step 1 r2 (Step.open_ _ 1 (by simp [St.init]))
  -- Thread 2 goes to claim, and its sweep comes back empty.
  have r4 := Reach.step 2 r3 (Step.miss _ 2 (by simp [set_apply, St.init]))
  -- Keep this fact as `r5`: with the bug, it goes to mark first.
  have r5 := Reach.step 2 r4 (Step.sweepMiss _ 2 (by simp [St.init]) rfl (by decide))
  -- Thread 2 marks the busy slot 0.
  have r6 := Reach.step 2 r5
    -- The rest of the arguments: each condition of the step, checked.
    (Step.markBusy _ 2 (by simp [St.init]) (by simp [St.init]) (by simp [St.free, St.init]))
  -- Thread 2's look is done; with the bug it goes to join the list now.
  have r7 := Reach.step 2 r6 (Step.markDone _ 2 (by simp [St.init]) (by simp [St.init]))
  -- Thread 1 leaves the marked slot as its last reader.
  have r8 := Reach.step 1 r7 (Step.leave _ 1 (by simp [set_apply, St.init]))
  -- Thread 1 clears the mark.
  have r9 := Reach.step 1 r8 (Step.clearW _ 1 (by simp [St.init]))
  -- Thread 1 wakes the list, which is empty.
  have r10 := Reach.step 1 r9 (Step.wake _ 1 (by simp [St.init]))
  -- Thread 2 joins the list, and with the bug goes to park.
  have r11 := Reach.step 2 r10 (Step.register _ 2 (by simp [set_apply, St.init]))
  -- Thread 2 parks.
  have r12 := Reach.step 2 r11 (Step.park _ 2 (by simp [St.init]))
  -- That final state is the witness.
  refine ⟨_, r12, by simp [St.init], ?_⟩
  -- No message, the mark gone, and nobody on the way to wake.
  simp [Kept, St.free, St.waking, tell, set_apply, St.init]
  -- Every thread is at its start or idle: nobody is clearing or waking.
  intro r; by_cases h2 : r = 2 <;> by_cases h1 : r = 1 <;> simp_all

/-- **`publish_drops_loses_retry`.** With the publish that drops the mark:
thread 1 claims the only slot to load its table; thread 2's unit finds it
busy, joins the list, marks it and parks. Thread 1 publishes the slot and
the mark is lost: no message, no mark, no wake on its way. -/
theorem publish_drops_loses_retry :
    -- A reachable state of the bug where thread 2's unit is parked ...
    ∃ s, Reach .publishDrops 1 unitTwo s ∧ s.parked 2 = true ∧ ¬ Kept s := by
  -- Thread 1 goes to claim and claims slot 0.
  have r1 := Reach.step 1 Reach.init
    -- The rest of the arguments: each condition of the step, checked.
    (Step.miss (v := .publishDrops) (cap := 1) (nw := unitTwo) St.init 1 rfl)
  -- Keep this fact as `r2`: the claim.
  have r2 := Reach.step 1 r1 (Step.claim _ 1 0 (by simp [St.init]) (by decide) (by simp [St.free, St.init]))
  -- Thread 2 goes to claim, and its sweep comes back empty.
  have r3 := Reach.step 2 r2 (Step.miss _ 2 (by simp [set_apply, St.init]))
  -- Keep this fact as `r4`: it goes to join the list.
  have r4 := Reach.step 2 r3 (Step.sweepMiss _ 2 (by simp [St.init]) rfl (by decide))
  -- Thread 2 joins the list.
  have r5 := Reach.step 2 r4 (Step.register _ 2 (by simp [St.init]))
  -- Thread 2 marks the claimed slot 0.
  have r6 := Reach.step 2 r5
    -- The rest of the arguments: each condition of the step, checked.
    (Step.markBusy _ 2 (by simp [St.init]) (by simp [St.init]) (by simp [St.free, St.init]))
  -- Thread 2's look is done.
  have r7 := Reach.step 2 r6 (Step.markDone _ 2 (by simp [St.init]) (by simp [St.init]))
  -- Thread 2 parks.
  have r8 := Reach.step 2 r7 (Step.park _ 2 (by simp [St.init]))
  -- Thread 1 publishes its table, dropping the mark.
  have r9 := Reach.step 1 r8 (Step.open_ _ 1 (by simp [set_apply, St.init]))
  -- That final state is the witness.
  refine ⟨_, r9, by simp [St.init], ?_⟩
  -- No message, the mark gone, and nobody on the way to wake.
  simp [Kept, St.free, St.waking, set_apply, St.init]
  -- Thread 1 reads, thread 2 is idle, anyone else is at its start.
  intro r; by_cases h2 : r = 2 <;> by_cases h1 : r = 1 <;> simp_all

/-- **`wake_needs_parked_loses_retry`.** With the wake that tells only parked
units: thread 1 reads in the only slot; thread 2's unit joins the list, marks
the slot and is about to park. Thread 1 leaves, clears the mark and wakes the
list, skipping thread 2, which is not parked yet. Thread 2 then parks: off the
list, no message. -/
theorem wake_needs_parked_loses_retry :
    -- A reachable state of the bug where thread 2's unit is parked ...
    ∃ s, Reach .wakeNeedsParked 1 unitTwo s ∧ s.parked 2 = true ∧ ¬ Kept s := by
  -- Thread 1 goes to claim, claims slot 0, opens its table and reads.
  have r1 := Reach.step 1 Reach.init
    -- The rest of the arguments: each condition of the step, checked.
    (Step.miss (v := .wakeNeedsParked) (cap := 1) (nw := unitTwo) St.init 1 rfl)
  -- Keep this fact as `r2`: the claim.
  have r2 := Reach.step 1 r1 (Step.claim _ 1 0 (by simp [St.init]) (by decide) (by simp [St.free, St.init]))
  -- Keep this fact as `r3`: the open.
  have r3 := Reach.step 1 r2 (Step.open_ _ 1 (by simp [St.init]))
  -- Thread 2 goes to claim, and its sweep comes back empty.
  have r4 := Reach.step 2 r3 (Step.miss _ 2 (by simp [set_apply, St.init]))
  -- Keep this fact as `r5`: it goes to join the list.
  have r5 := Reach.step 2 r4 (Step.sweepMiss _ 2 (by simp [St.init]) rfl (by decide))
  -- Thread 2 joins the list.
  have r6 := Reach.step 2 r5 (Step.register _ 2 (by simp [St.init]))
  -- Thread 2 marks the busy slot 0.
  have r7 := Reach.step 2 r6
    -- The rest of the arguments: each condition of the step, checked.
    (Step.markBusy _ 2 (by simp [St.init]) (by simp [St.init]) (by simp [St.free, St.init]))
  -- Thread 2's look is done: it is about to park.
  have r8 := Reach.step 2 r7 (Step.markDone _ 2 (by simp [St.init]) (by simp [St.init]))
  -- Thread 1 leaves the marked slot as its last reader.
  have r9 := Reach.step 1 r8 (Step.leave _ 1 (by simp [set_apply, St.init]))
  -- Thread 1 clears the mark.
  have r10 := Reach.step 1 r9 (Step.clearW _ 1 (by simp [St.init]))
  -- Thread 1 wakes the list, skipping thread 2, which is not parked yet.
  have r11 := Reach.step 1 r10 (Step.wake _ 1 (by simp [St.init]))
  -- Thread 2 parks.
  have r12 := Reach.step 2 r11 (Step.park _ 2 (by simp [set_apply, St.init]))
  -- That final state is the witness.
  refine ⟨_, r12, by simp [St.init], ?_⟩
  -- No message, and off the list.
  simp [Kept, tell, set_apply, St.init]

-- The end of this file's names.
end Regolith.OpenFileNoWait
