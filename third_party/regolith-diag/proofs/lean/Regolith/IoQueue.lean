/-!
# IoQueue: per-thread I/O queues and single-flight reads (D53)

This file backs the TLA+ model `proofs/tla/NonBlocking.tla`. TLC checks the
protocol there, step by atomic step, for two threads and a few units. Here
the same rules are proved for every number of threads, units and steps.

## The story, for a reader who has never seen the code

regolith reads table blocks from the disk. A thread that must never wait
(a worker in a thread-per-core pool) reads through a `CacheOnly` handle:
when the block is not in memory, the read does not touch the disk. It
records the block on the thread's own I/O queue and returns at once with
"would block". When the thread has nothing else to run, it *polls* its queue:
the poll does the disk reads its thread waits on and leaves a "you can run
again" note for every waiting read.

Tiny example. Threads A and B both miss block 7. There is one *unit* for
block 7. A's poll grabs it with one compare-and-swap (CAS), reads the block
once, and then puts one "done" note in A's inbox and one in B's. If B was
asleep, the note to B wakes B up; A is awake, so nothing wakes A.

## What is proved, in plain words

1. `claim_once`, `runs_le_one`: a unit is grabbed by one CAS, so its disk
   read runs at most once, however the threads interleave
   (TLA+ `SingleRun`).
2. `told_at_most_once`, `told_only_registered`, `every_waiter_told`: every
   queue that registered on a unit gets exactly one "done" note, and no
   other queue gets one (TLA+ `ExactlyOnce`, `EveryWaiterTold`).
3. `own_queue`: a read's request only ever sits in its own thread's inbox,
   so only that thread's poll finishes it (TLA+ `OwnQueue`).
4. `no_lost_idle_wakeup`, `busy_never_woken`: an owner asleep with nobody
   having woken it has an empty inbox, and only a sleeping owner is ever
   woken (TLA+ `NoLostIdleWakeup`, `BusyNeverWoken`).
5. `job_held`, `single_thread_finishes`: I/O regolith starts itself sits on
   the starting thread's queue, and one thread polling its own queue
   finishes every read it waits on (TLA+ `NoOrphanUnit`, `AllDone`).
6. The RED cases, as counterexamples: a claim that is a load then a store
   runs a unit twice (`load_store_claim_runs_twice`); a request sent to the
   wrong queue breaks `own_queue` (`wrong_queue_breaks_own_queue`); going
   idle in two steps loses a wakeup (`split_rest_loses_wakeup`); a poll that
   leaves the idle bit set wakes a busy owner (`stale_idle_bit_wakes_busy`);
   a job put on no queue is never run (`orphan_never_runs`).
7. The commit side (Part 5). The `commit_nowait` commits one sync of the
   log covers wait on one job, the sync round, a unit like any other, so 1
   and 2 hold for it: one claim, one fsync, one note per waiting queue. On
   top of that: `round_sync_once`, the fsync runs at most once (TLA+
   `SingleRun` on a round); `round_visible_after_sync`, readers see the
   round's commits only after its fsync (TLA+ `VisibleAfterSync`);
   `ticket_ready_only_at_own_poll`, a commit's ticket becomes ready only at
   its own queue's poll (TLA+ `DeliveredOnOwnPoll`);
   `stall_lands_when_clear`, a cleared stall leaves no unlanded unit behind
   except one a writer is about to land (TLA+ `StallLandsWhenClear`). RED:
   a runner that readies the waiting commits' tickets itself
   (`lander_delivery_breaks_own_poll`), and a writer that does not look at
   the stall again (`no_recheck_strands_stall`).
8. Timed waits (Part 6), the wait a retry backs off on. Nobody is woken for
   one: the owner's poll completes it when the clock reaches its due time.
   `timed_wait_completed_once`, it is completed at most once;
   `timed_wait_completed_by_owner`, only by a step of its queue's owner
   (TLA+ `TimerOnOwnQueue`); `timed_wait_not_before_due`, a poll never
   completes it before its due time (TLA+ `TimerNotEarly`);
   `dropped_queue_holds_no_wait`, a dropped queue leaves none pending (TLA+
   `DroppedHoldsNothing`); `poll_after_wake_in_completes`, an owner that
   polls again within what its poll reported completes it (TLA+ `AllDone`
   with a timed wait). RED: a poll that does not look at the clock
   (`early_fire_breaks_due`), and an owner that never polls again
   (`idle_owner_never_completes`).

## How to read the Lean

`def` defines a thing, `theorem` states a fact and its proof follows
`:= by`. Lines starting with `--` are comments, in plain words. A proof is a
list of *tactics*; each one changes the goal still to be shown, and the
comment above it says how. `simp` rewrites with known facts; `omega` solves
arithmetic over natural numbers; `cases` splits on the ways a fact could be
true; `induction` proves a fact for every number of steps by proving it for
none and then for one more.
-/

-- Everything below is named Regolith.IoQueue.<name>.
namespace Regolith.IoQueue

/-! ## Part 1. A unit: one disk read, single-flight, with its waiters

This part mirrors `Unit` in `src/engine/io/unit.rs` and the queue side of
registering on it (`IoQueue::accept` in `src/io_queue/queue.rs`). -/

/-- The state of one unit (`Unit::state`). -/
inductive UState where
  /-- The unit number is not in use yet. -/
  | none
  /-- Made, and nobody has grabbed it. -/
  | free
  /-- Grabbed by thread `t`, whose poll is reading the block. -/
  | claimed (t : Nat)
  /-- Finished: read, or let go by close. -/
  | done
  -- Two states can be compared for equality.
  deriving DecidableEq

/-- Everything about the units at one moment. A unit and a queue (a thread)
are both named by a natural number. -/
structure Units where
  /-- Each unit's state. -/
  st : Nat → UState
  /-- The queues registered on each unit: its waiter list (a lock-free
  stack in the code). -/
  waiters : Nat → List Nat
  /-- The SHUT flag on each waiter list: once set, registrations are
  refused. -/
  shut : Nat → Bool
  /-- Ghost: every queue whose registration on the unit succeeded. -/
  regd : Nat → List Nat
  /-- The queues the finisher still has to give a "done" note. -/
  owed : Nat → List Nat
  /-- Ghost: every "done" note given, as (queue, unit). -/
  told : List (Nat × Nat)
  /-- Ghost: how many times each unit's disk read ran. -/
  runs : Nat → Nat

/-- The start: no unit in use, nobody waiting, nothing told or run. -/
def Units.init : Units :=
  -- Every field starts empty.
  ⟨fun _ => .none, fun _ => [], fun _ => false, fun _ => [], fun _ => [], [], fun _ => 0⟩

/-- `f` with its value at `k` replaced by `v`. -/
def set {α : Type} (f : Nat → α) (k : Nat) (v : α) : Nat → α :=
  -- At `k` the new value, everywhere else the old one.
  fun x => if x = k then v else f x

/-- The steps of the units, one atomic step each, as the code takes them. -/
inductive UStep : Units → Units → Prop
  /-- A miss makes unit `u` (it goes into the unit table, free). -/
  | make (s : Units) (u : Nat)
      -- The number `u` is not in use yet.
      (h : s.st u = .none) :
      -- The state after the step:
      UStep s { s with st := set s.st u .free }
  /-- Queue `q` registers on unit `u`: one CAS push onto the waiter list,
  which succeeds because the list is not shut. A queue registers on a unit
  at most once (its `waiting` map is keyed by the unit). -/
  | register (s : Units) (q u : Nat)
      -- The waiter list is open.
      (hopen : s.shut u = false)
      -- `q` has not registered on `u` before.
      (hnew : q ∉ s.regd u) :
      -- The state after the step:
      UStep s { s with waiters := set s.waiters u (q :: s.waiters u),
                       regd := set s.regd u (q :: s.regd u) }
  /-- Queue `q` tries to register on a unit whose list is shut: the push is
  refused and nothing changes (the queue reads the outcome itself). -/
  | refused (s : Units) (q u : Nat)
      -- The waiter list is shut.
      (hshut : s.shut u = true) :
      -- The state after the step:
      UStep s s
  /-- Thread `t` grabs unit `u` with one CAS from free to claimed. -/
  | claim (s : Units) (t u : Nat)
      -- The CAS succeeds only if the unit is free.
      (hfree : s.st u = .free) :
      -- The state after the step:
      UStep s { s with st := set s.st u (.claimed t) }
  /-- The claimer finishes: the read ran, the unit is done, and the waiter
  list is shut and taken whole in one swap: every queue on it is owed a
  note. -/
  | finish (s : Units) (t u : Nat)
      -- `t` is the claimer.
      (hcl : s.st u = .claimed t) :
      -- The state after the step:
      UStep s { s with st := set s.st u .done,
                       runs := set s.runs u (s.runs u + 1),
                       shut := set s.shut u true,
                       owed := set s.owed u (s.waiters u),
                       waiters := set s.waiters u [] }
  /-- Close lets a free unit go: it is done without a read, and its waiter
  list is shut and taken whole, as in `finish`. -/
  | release (s : Units) (u : Nat)
      -- Nobody has grabbed it.
      (hfree : s.st u = .free) :
      -- The state after the step:
      UStep s { s with st := set s.st u .done,
                       shut := set s.shut u true,
                       owed := set s.owed u (s.waiters u),
                       waiters := set s.waiters u [] }
  /-- The finisher puts the "done" note for `u` into queue `q`'s inbox. -/
  | tell (s : Units) (q u : Nat)
      -- `q` is still owed its note.
      (howed : q ∈ s.owed u) :
      -- The state after the step:
      UStep s { s with told := (q, u) :: s.told, owed := set s.owed u ((s.owed u).erase q) }

/-- Any number of steps, one after another. -/
inductive USteps : Units → Units → Prop
  /-- No step at all. -/
  | refl (s : Units) : USteps s s
  /-- Some steps, then one more. -/
  | tail {s t u : Units} : USteps s t → UStep t u → USteps s u

/-- What holds of every unit in every reachable state. -/
structure UInv (s : Units) : Prop where
  /-- A unit that is not done has never run its read. -/
  runs_done : ∀ u, s.st u ≠ .done → s.runs u = 0
  /-- No unit ran its read more than once. -/
  runs_le : ∀ u, s.runs u ≤ 1
  /-- Each queue appears at most once among a unit's registered queues. -/
  regd_nodup : ∀ u, (s.regd u).Nodup
  /-- For every queue and unit, notes given + notes owed + still on the
  waiter list = registered (each counted 0 or 1 times). -/
  balance : ∀ u q, s.told.count (q, u) + (s.owed u).count q + (s.waiters u).count q
      -- equals how many times it registered.
      = (s.regd u).count q
  /-- An open waiter list owes nobody yet. -/
  open_owes_none : ∀ u, s.shut u = false → s.owed u = []
  /-- A shut waiter list is empty. -/
  shut_empty : ∀ u, s.shut u = true → s.waiters u = []
  /-- A unit's waiter list stays open until the unit is done. -/
  open_until_done : ∀ u, s.st u ≠ .done → s.shut u = false

/-- `set` at the key it sets gives the new value. -/
@[simp] theorem set_same {α : Type} (f : Nat → α) (k : Nat) (v : α) : set f k v k = v := by
  -- Unfold `set`; the key equals itself, so the new value is picked.
  simp [set]

/-- `set` at another key gives the old value. -/
@[simp] theorem set_other {α : Type} (f : Nat → α) (k x : Nat) (v : α) (h : x ≠ k) :
    -- Then:
    set f k v x = f x := by
  -- Unfold `set`; the keys differ, so the old value is picked.
  simp [set, h]

/-- The start satisfies the invariant. -/
theorem uinv_init : UInv Units.init := by
  -- Each field of the invariant holds because everything starts empty.
  refine ⟨?_, ?_, ?_, ?_, ?_, ?_, ?_⟩ <;> intros <;> simp [Units.init]

/-- **One step keeps the invariant.** Each kind of step is checked in turn. -/
theorem ustep_inv {s t : Units}
    -- The invariant holds before the step.
    (hs : UInv s)
    -- One step from `s` to `t`.
    (h : UStep s t) :
    -- Then:
    UInv t := by
  -- Look at which kind of step it was.
  cases h with
  -- The step made a unit.
  | make u hnone =>
    -- Making a unit only changes its state from "not in use" to "free".
    refine ⟨fun x hx => ?_, hs.runs_le, hs.regd_nodup, hs.balance, hs.open_owes_none,
      hs.shut_empty, fun x hx => ?_⟩
    -- A unit that is not done has not run: for `u` its old state was
    ·
      -- "not in use", which is not done either; other units are untouched.
      by_cases hxu : x = u
      -- It is `u`: use the old state, "not in use".
      · subst hxu
        -- "Not in use" is not "done", so the old fact applies.
        exact hs.runs_done x (by simp [hnone])
      -- Another unit: its state did not change.
      · exact hs.runs_done x (by simpa [hxu] using hx)
    -- The waiter list stays open until done: the same split.
    · by_cases hxu : x = u
      -- It is `u`: its old state was "not in use", not done.
      · subst hxu
        -- So its list was open, and still is.
        exact hs.open_until_done x (by simp [hnone])
      -- Another unit: unchanged.
      · exact hs.open_until_done x (by simpa [hxu] using hx)
  -- The step registered a queue on a unit.
  | register q u hopen hnew =>
    -- Registering puts `q` on `u`'s waiter list and its registered list.
    refine ⟨hs.runs_done, hs.runs_le, fun x => ?_, fun x p => ?_, hs.open_owes_none,
      fun x hx => ?_, hs.open_until_done⟩
    -- The registered list stays free of repeats: `q` was not on it.
    · by_cases hxu : x = u
      -- It is `u`: `q` plus a list without `q`, itself without repeats.
      · subst hxu
        -- Unfold the new list and use both facts.
        simpa [List.nodup_cons, hnew] using hs.regd_nodup x
      -- Another unit: unchanged.
      · simpa [hxu] using hs.regd_nodup x
    -- The balance: `q` gains one on the waiter list and one registered.
    · by_cases hxu : x = u
      -- It is `u`.
      · subst hxu
        -- The old balance for `u`.
        have hb := hs.balance x p
        -- Nothing is owed on an open list.
        have ho := hs.open_owes_none x hopen
        -- Count both new lists: each gains exactly `q`.
        simp only [set_same, List.count_cons]
        -- The arithmetic now closes it.
        rw [ho] at hb ⊢
        -- Both sides grew by the same amount.
        simp at hb ⊢
        -- Finish with arithmetic.
        omega
      -- Another unit: unchanged.
      · simpa [hxu] using hs.balance x p
    -- A shut list is empty: `u`'s list is open, others are unchanged.
    · by_cases hxu : x = u
      -- It is `u`: it is open, so it cannot be shut.
      · subst hxu
        -- Open and shut at once is impossible.
        simp [hopen] at hx
      -- Another unit: unchanged.
      · simpa [hxu] using hs.shut_empty x hx
  -- The step was a refused registration.
  | refused q u hshut =>
    -- A refused push changes nothing.
    exact hs
  -- The step claimed a unit.
  | claim t u hfree =>
    -- Claiming only changes `u`'s state from free to claimed.
    refine ⟨fun x hx => ?_, hs.runs_le, hs.regd_nodup, hs.balance, hs.open_owes_none,
      hs.shut_empty, fun x hx => ?_⟩
    -- A free unit had not run, and a claimed one has not either.
    · by_cases hxu : x = u
      -- It is `u`: it was free, not done.
      · subst hxu
        -- Free is not done, so it had not run.
        exact hs.runs_done x (by simp [hfree])
      -- Another unit: unchanged.
      · exact hs.runs_done x (by simpa [hxu] using hx)
    -- Its list was open while it was free, and stays open.
    · by_cases hxu : x = u
      -- It is `u`: free is not done.
      · subst hxu
        -- So the list was open.
        exact hs.open_until_done x (by simp [hfree])
      -- Another unit: unchanged.
      · exact hs.open_until_done x (by simpa [hxu] using hx)
  -- The step finished a unit.
  | finish t u hcl =>
    -- Finishing: done, one run, the list shut and taken whole as owed.
    -- Before, the unit was claimed: not done, so it had not run.
    have hr0 : s.runs u = 0 := hs.runs_done u (by simp [hcl])
    -- And its list was open.
    have hop : s.shut u = false := hs.open_until_done u (by simp [hcl])
    -- So nothing was owed yet.
    have hown : s.owed u = [] := hs.open_owes_none u hop
    -- Prove each part of the invariant in turn:
    refine ⟨fun x hx => ?_, fun x => ?_, hs.regd_nodup, fun x p => ?_, fun x hx => ?_,
      fun x hx => ?_, fun x hx => ?_⟩
    -- Only units other than `u` can be not done; they are unchanged.
    · by_cases hxu : x = u
      -- It is `u`: it is done now, so this case cannot happen.
      · subst hxu
        -- "Done" is done.
        simp at hx
      -- Another unit: unchanged.
      · simpa [hxu] using hs.runs_done x (by simpa [hxu] using hx)
    -- At most one run: `u` goes from 0 to 1.
    · by_cases hxu : x = u
      -- It is `u`.
      · subst hxu
        -- 0 + 1 is at most 1.
        simp [hr0]
      -- Another unit: unchanged.
      · simpa [hxu] using hs.runs_le x
    -- The balance: the waiter list's queues move to the owed list.
    · by_cases hxu : x = u
      -- It is `u`.
      · subst hxu
        -- The old balance, with nothing owed.
        have hb := hs.balance x p
        -- Rewrite the empty owed list away and compare.
        rw [hown] at hb
        -- The new owed list is the old waiter list; the waiter list is empty.
        simp at hb ⊢
        -- Same total.
        omega
      -- Another unit: unchanged.
      · simpa [hxu] using hs.balance x p
    -- Only open lists owe nothing; `u` is shut now.
    · by_cases hxu : x = u
      -- It is `u`: shut, so not open.
      · subst hxu
        -- Shut is not open.
        simp at hx
      -- Another unit: unchanged.
      · simpa [hxu] using hs.open_owes_none x (by simpa [hxu] using hx)
    -- A shut list is empty: `u`'s was emptied.
    · by_cases hxu : x = u
      -- It is `u`: emptied.
      · subst hxu
        -- The new list is empty.
        simp
      -- Another unit: unchanged.
      · simpa [hxu] using hs.shut_empty x (by simpa [hxu] using hx)
    -- Not done means open: `u` is done, others are unchanged.
    · by_cases hxu : x = u
      -- It is `u`: done, so this case cannot happen.
      · subst hxu
        -- "Done" is done.
        simp at hx
      -- Another unit: unchanged.
      · simpa [hxu] using hs.open_until_done x (by simpa [hxu] using hx)
  -- The step released a unit.
  | release u hfree =>
    -- Releasing is finishing without a run.
    -- The unit was free: its list was open.
    have hop : s.shut u = false := hs.open_until_done u (by simp [hfree])
    -- So nothing was owed.
    have hown : s.owed u = [] := hs.open_owes_none u hop
    -- Prove each part of the invariant in turn:
    refine ⟨fun x hx => ?_, hs.runs_le, hs.regd_nodup, fun x p => ?_, fun x hx => ?_,
      fun x hx => ?_, fun x hx => ?_⟩
    -- Units other than `u` are unchanged; `u` is done now.
    · by_cases hxu : x = u
      -- It is `u`: done, so this case cannot happen.
      · subst hxu
        -- "Done" is done.
        simp at hx
      -- Another unit: unchanged.
      · exact hs.runs_done x (by simpa [hxu] using hx)
    -- The balance: the waiter list moves to the owed list.
    · by_cases hxu : x = u
      -- It is `u`.
      · subst hxu
        -- The old balance, with nothing owed.
        have hb := hs.balance x p
        -- Rewrite the empty owed list away.
        rw [hown] at hb
        -- Compare the new lists.
        simp at hb ⊢
        -- Same total.
        omega
      -- Another unit: unchanged.
      · simpa [hxu] using hs.balance x p
    -- `u` is shut now; others are unchanged.
    · by_cases hxu : x = u
      -- It is `u`: shut, so not open.
      · subst hxu
        -- Shut is not open.
        simp at hx
      -- Another unit: unchanged.
      · simpa [hxu] using hs.open_owes_none x (by simpa [hxu] using hx)
    -- `u`'s list was emptied; others are unchanged.
    · by_cases hxu : x = u
      -- It is `u`: emptied.
      · subst hxu
        -- The new list is empty.
        simp
      -- Another unit: unchanged.
      · simpa [hxu] using hs.shut_empty x (by simpa [hxu] using hx)
    -- `u` is done; others are unchanged.
    · by_cases hxu : x = u
      -- It is `u`: done, so this case cannot happen.
      · subst hxu
        -- "Done" is done.
        simp at hx
      -- Another unit: unchanged.
      · simpa [hxu] using hs.open_until_done x (by simpa [hxu] using hx)
  -- The step told a queue.
  | tell q u howed =>
    -- Telling `q`: one more note given, one fewer owed.
    refine ⟨hs.runs_done, hs.runs_le, hs.regd_nodup, fun x p => ?_, fun x hx => ?_,
      hs.shut_empty, hs.open_until_done⟩
    -- The balance moves one from owed to given, for `(q, u)` only.
    · by_cases hxu : x = u
      -- It is `u`.
      · subst hxu
        -- The old balance.
        have hb := hs.balance x p
        -- Split on whether p = q (call that fact `hpq`).
        by_cases hpq : p = q
        -- It is `q`: owed loses one, given gains one.
        · subst hpq
          -- Removing one `p` from a list that holds it lowers its count by one.
          have hc := List.count_erase_self (a := p) (l := s.owed x)
          -- `p` is on the owed list, so it was counted at least once.
          have hpos := List.count_pos_iff.2 howed
          -- Count the new given list: the new note is `(p, x)`.
          simp only [set_same, List.count_cons, beq_self_eq_true, ite_true] at hc ⊢
          -- The arithmetic now closes it.
          omega
        -- Another queue: its counts are unchanged.
        ·
          -- Removing `q` leaves `p`'s count alone.
          have hc : ((s.owed x).erase q).count p = (s.owed x).count p :=
            -- The library fact for erasing something other than `p`.
            List.count_erase_of_ne hpq
          -- The new note is not `(p, x)`.
          have hne : ((q, x) == (p, x)) = false := by simp [Ne.symm hpq]
          -- Count the new given list and the new owed list: the new note
          -- adds nothing for `p`.
          simp only [set_same, List.count_cons, hne, Bool.false_eq_true, ite_false,
            Nat.add_zero] at hb ⊢
          -- Same total as before, once `hc` is used.
          omega
      -- Another unit: its lists are unchanged, and the new note is not
      ·
        -- about it.
        have hne : ((q, u) == (p, x)) = false := by simp [Ne.symm hxu]
        -- Count the new given list.
        simp only [List.count_cons, hne, set_other _ _ _ _ hxu]
        -- Same total as before.
        simpa using hs.balance x p
    -- Only open lists owe nothing: `u` owed `q`, so it was shut; erasing
    ·
      -- keeps that.
      by_cases hxu : x = u
      -- It is `u`: an open list owes nothing, but `u` owed `q`.
      · subst hxu
        -- So `u`'s list was not open.
        have := hs.open_owes_none x hx
        -- `q` cannot be on an empty list.
        simp [this] at howed
      -- Another unit: unchanged.
      · simpa [hxu] using hs.open_owes_none x hx

/-- Any number of steps keep the invariant. -/
theorem usteps_inv {s t : Units}
    -- The invariant holds at the start of the steps.
    (hs : UInv s)
    -- Any number of steps.
    (h : USteps s t) :
    -- Then:
    UInv t := by
  -- Prove it for every number of steps.
  induction h with
  -- No step: nothing changed.
  | refl => exact hs
  -- The steps so far keep it, and so does one more.
  | tail _ hstep ih => exact ustep_inv ih hstep

/-- **Every reachable state satisfies the invariant.** -/
theorem ureachable_inv {s : Units}
    -- `s` is reachable from the start.
    (h : USteps Units.init s) :
    -- Then:
    UInv s :=
  -- The start satisfies it, and steps keep it.
  usteps_inv uinv_init h

/-- **A unit's disk read runs at most once.** However many threads try to
claim it, in whatever order, one CAS wins and only the winner runs it.
TLA+: `SingleRun`. Rules out: A and B both reading block 7. -/
theorem runs_le_one {s : Units}
    -- `s` is reachable from the start.
    (h : USteps Units.init s)
    -- Any unit.
    (u : Nat) :
    -- Then:
    s.runs u ≤ 1 :=
  -- It is one field of the invariant.
  (ureachable_inv h).runs_le u

/-- **A claimed or finished unit is never free again**, so no second claim
can succeed on it. -/
theorem claim_once {s t : Units}
    -- One step.
    (h : UStep s t)
    -- Any unit that is past "free" (claimed or done).
    (u : Nat) (hu : s.st u ≠ .free) (hn : s.st u ≠ .none) :
    -- Then:
    t.st u ≠ .free := by
  -- Look at the step.
  cases h with
  -- The step made a unit.
  | make v hv =>
    -- Making `v`: `v` was not in use, so it is not `u`.
    by_cases huv : u = v
    -- It would be `u`, but `u` is in use: impossible.
    · subst huv
      -- `u` is "not in use" and not "not in use" at once.
      exact absurd hv hn
    -- Another unit: `u` is unchanged.
    · simpa [huv] using hu
  -- The step claimed a unit.
  | claim t' v hv =>
    -- Claiming `v` needs `v` free, so it is not `u`; or it makes `u`
    -- claimed, which is not free.
    by_cases huv : u = v
    -- It is `u`: claimed is not free.
    · subst huv
      -- Claimed is not free.
      simp
    -- Another unit: `u` is unchanged.
    · simpa [huv] using hu
  -- The step finished a unit.
  | finish t' v hv =>
    -- Finishing sets "done", never "free".
    by_cases huv : u = v
    -- It is `u`: done is not free.
    · subst huv
      -- Done is not free.
      simp
    -- Another unit: unchanged.
    · simpa [huv] using hu
  -- The step released a unit.
  | release v hv =>
    -- Releasing sets "done", never "free".
    by_cases huv : u = v
    -- It is `u`: done is not free.
    · subst huv
      -- Done is not free.
      simp
    -- Another unit: unchanged.
    · simpa [huv] using hu
  -- The other steps do not touch any state.
  | register q v ho hn' => exact hu
  -- A refused push changes nothing.
  | refused q v hsh => exact hu
  -- Telling changes no state.
  | tell q v hq => exact hu

/-- **A queue gets at most one "done" note per unit.** TLA+: `ExactlyOnce`.
Rules out: B being told twice about block 7. -/
theorem told_at_most_once {s : Units}
    -- `s` is reachable from the start.
    (h : USteps Units.init s)
    -- Any unit and queue.
    (u q : Nat) :
    -- Then:
    s.told.count (q, u) ≤ 1 := by
  -- The invariant of `s`.
  have hi := ureachable_inv h
  -- Notes given are part of the balance, which totals the registrations.
  have hb := hi.balance u q
  -- A list without repeats holds `q` at most once.
  have hn := List.nodup_iff_count.1 (hi.regd_nodup u) q
  -- So the notes given are at most one.
  omega

/-- **Only a queue that registered is ever told.** TLA+: `ExactlyOnce`.
Rules out: C being told about a read it never asked for. -/
theorem told_only_registered {s : Units}
    -- `s` is reachable from the start.
    (h : USteps Units.init s)
    -- A queue was told about a unit.
    {u q : Nat} (ht : 0 < s.told.count (q, u)) :
    -- Then:
    q ∈ s.regd u := by
  -- The balance for this unit and queue.
  have hb := (ureachable_inv h).balance u q
  -- The registered count is at least the notes given, so positive.
  have hpos : 0 < (s.regd u).count q := by omega
  -- A positive count means `q` is on the list.
  exact List.count_pos_iff.1 hpos

/-- **Every queue that registered is told, once the finisher is done.**
When the unit's list is shut and nobody is owed any more, each registered
queue has exactly one note. TLA+: `EveryWaiterTold`. Rules out: B
registering on block 7 and never hearing it finished. -/
theorem every_waiter_told {s : Units}
    -- `s` is reachable from the start.
    (h : USteps Units.init s)
    -- The unit's list is shut and the finisher owes nobody.
    {u q : Nat} (hshut : s.shut u = true) (howed : s.owed u = [])
    -- `q` registered.
    (hq : q ∈ s.regd u) :
    -- Then:
    s.told.count (q, u) = 1 := by
  -- The invariant of `s`.
  have hi := ureachable_inv h
  -- The balance for this unit and queue.
  have hb := hi.balance u q
  -- A shut list is empty.
  have hw := hi.shut_empty u hshut
  -- `q` is on the registered list, which has no repeats: count exactly 1.
  have hone : (s.regd u).count q = 1 :=
    -- No repeats means "on the list" is "counted once".
    (hi.regd_nodup u).count_of_mem hq
  -- Nothing owed and nothing waiting: the notes given carry the whole count.
  rw [howed, hw] at hb
  -- Count the empty lists as zero.
  simp at hb
  -- So exactly one note.
  omega

/-! ### RED: a claim written as a load and then a store

The bug: a thread loads the unit's state and, if it saw "free", later
stores "claimed" and runs the read. Two threads can both load "free"
before either stores. -/

/-- The broken claim's state: the unit's state, the threads that loaded
"free" and have not stored yet, and how many reads ran. -/
structure Broken where
  /-- The unit's state. -/
  st : UState
  /-- Threads that saw "free" and will store and run. -/
  sawFree : List Nat
  /-- Reads that ran. -/
  runs : Nat

/-- The broken claim's two half steps. -/
inductive BStep : Broken → Broken → Prop
  /-- Thread `t` loads the state; if free, it remembers so. -/
  | load (b : Broken) (t : Nat) :
      -- The state after the step:
      BStep b { b with sawFree := if b.st = .free then t :: b.sawFree else b.sawFree }
  /-- Thread `t`, having seen free, stores "claimed" and runs the read. -/
  | storeRun (b : Broken) (t : Nat) (h : t ∈ b.sawFree) :
      -- The state after the step:
      BStep b { b with st := .claimed t, sawFree := b.sawFree.erase t, runs := b.runs + 1 }

/-- Any number of broken steps. -/
inductive BSteps : Broken → Broken → Prop
  /-- No step. -/
  | refl (b : Broken) : BSteps b b
  /-- Some steps, then one more. -/
  | tail {a b c : Broken} : BSteps a b → BStep b c → BSteps a c

/-- **RED: the load-then-store claim runs a unit twice.** Threads 1 and 2
both load "free", then both store and run. TLA+:
`MC_NonBlocking_Red_DoubleRun` breaks `SingleRun`. -/
theorem load_store_claim_runs_twice :
    -- Then:
    ∃ b, BSteps ⟨.free, [], 0⟩ b ∧ b.runs = 2 := by
  -- The four steps: 1 loads, 2 loads, 1 stores and runs, 2 stores and runs.
  refine ⟨_, .tail (.tail (.tail (.tail (.refl _) (.load _ 1)) (.load _ 2))
    (.storeRun _ 1 (by simp))) (.storeRun _ 2 (by simp)), ?_⟩
  -- Two reads ran.
  simp

/-! ## Part 2. A read's request sits on its own queue

This part mirrors `IoRuntime::miss` (src/engine/io/mod.rs) pushing a
read's request onto the queue its handle names, and the owner's poll taking
its inbox. A request is (reader, unit). -/

/-- Each queue's inbox of read requests. -/
abbrev Inboxes := Nat → List (Nat × Nat)

/-- The steps that move read requests. -/
inductive QStep : Inboxes → Inboxes → Prop
  /-- Reader `r` misses unit `u`: the request goes to `r`'s own queue. -/
  | miss (i : Inboxes) (r u : Nat) : QStep i (set i r (i r ++ [(r, u)]))
  /-- The owner of queue `q` takes its whole inbox. -/
  | take (i : Inboxes) (q : Nat) : QStep i (set i q [])

/-- Every request in a queue's inbox is from that queue's own reader. -/
def OwnQueue (i : Inboxes) : Prop :=
  -- For every queue and every request in its inbox, the reader is the queue.
  ∀ q m, m ∈ i q → m.1 = q

/-- **A request only ever sits on its own reader's queue**, so only that
reader's poll finishes it. TLA+: `OwnQueue`. Rules out: A's read finished
by B's poll on B's thread. -/
theorem own_queue {i j : Inboxes}
    -- It holds before.
    (hi : OwnQueue i)
    -- One step.
    (h : QStep i j) :
    -- Then:
    OwnQueue j := by
  -- Look at the step.
  cases h with
  -- The step was a miss.
  | miss r u =>
    -- A miss adds `(r, u)` to `r`'s inbox.
    intro q m hm
    -- Split on whether q = r (call that fact `hq`).
    by_cases hq : q = r
    -- It is `r`'s inbox: old requests are `r`'s, and so is the new one.
    · subst hq
      -- The inbox is the old one plus the new request.
      simp only [set_same, List.mem_append, List.mem_singleton] at hm
      -- Either an old request or the new one.
      rcases hm with hm | hm
      -- An old one: from before.
      · exact hi q m hm
      -- The new one: its reader is `q`.
      · simp [hm]
    -- Another inbox: unchanged.
    · exact hi q m (by simpa [hq] using hm)
  -- The step took an inbox.
  | take q =>
    -- Taking empties `q`'s inbox.
    intro p m hm
    -- Split on whether p = q (call that fact `hp`).
    by_cases hp : p = q
    -- It is `q`: empty now, so there is nothing to check.
    · subst hp
      -- Nothing is in an empty list.
      simp at hm
    -- Another inbox: unchanged.
    · exact hi p m (by simpa [hp] using hm)

/-- **RED: a request sent to another queue breaks `OwnQueue`.** Reader 1's
request lands in queue 2's inbox. TLA+: `MC_NonBlocking_Red_WrongQueue`. -/
theorem wrong_queue_breaks_own_queue :
    -- Then:
    ¬ OwnQueue (set (fun _ => []) 2 [(1, 7)]) := by
  -- Suppose it held.
  intro h
  -- Then the request in queue 2 would be from reader 2, but it is from 1.
  have := h 2 (1, 7) (by simp)
  -- 1 is not 2.
  simp at this

/-! ## Part 3. Going idle and being woken

This part mirrors one queue's inbox head word with its IDLE bit
(`Stack` in src/engine/io/stack.rs, `QueueShared::{deliver, rest,
wake_up}` in src/engine/io/shared.rs) and its owner thread. -/

/-- One queue and its owner. -/
structure Owner where
  /-- Notes in the inbox. -/
  inbox : Nat
  /-- The IDLE bit in the inbox's head word. -/
  idle : Bool
  /-- The owner thread is asleep. -/
  asleep : Bool
  /-- The owner's idle waker fired and the owner has not run since. -/
  woken : Bool

/-- The steps of one queue and its owner. -/
inductive OStep : Owner → Owner → Prop
  /-- Any thread pushes a note (one CAS): the push clears the IDLE bit, and
  if the bit was set, this push wakes the owner. -/
  | push (o : Owner) :
      -- The state after the step:
      OStep o ⟨o.inbox + 1, false, o.asleep, o.woken || o.idle⟩
  /-- The awake owner, with an empty inbox, goes to sleep: it registers its
  waker and sets IDLE with one CAS that only succeeds on an empty inbox. -/
  | rest (o : Owner) (ha : o.asleep = false) (he : o.inbox = 0) :
      -- The state after the step:
      OStep o ⟨0, true, true, o.woken⟩
  /-- The awake owner polls and takes its whole inbox (one swap, which also
  clears IDLE). -/
  | take (o : Owner) (ha : o.asleep = false) :
      -- The state after the step:
      OStep o ⟨0, false, false, o.woken⟩
  /-- The sleeping owner whose waker fired runs again. -/
  | wake (o : Owner) (hs : o.asleep = true) (hw : o.woken = true) :
      -- The state after the step:
      OStep o ⟨o.inbox, o.idle, false, false⟩
  /-- The sleeping owner runs again on its own, and its poll clears IDLE
  first. -/
  | selfWake (o : Owner) (hs : o.asleep = true) :
      -- The state after the step:
      OStep o ⟨o.inbox, false, false, false⟩

/-- Any number of owner steps. -/
inductive OSteps : Owner → Owner → Prop
  /-- No step. -/
  | refl (o : Owner) : OSteps o o
  /-- Some steps, then one more. -/
  | tail {a b c : Owner} : OSteps a b → OStep b c → OSteps a c

/-- The start: an empty inbox, an awake owner, no wake. -/
def Owner.init : Owner := ⟨0, false, false, false⟩

/-- What holds of one queue and its owner in every reachable state. -/
structure OInv (o : Owner) : Prop where
  /-- The IDLE bit is set only on an empty inbox of a sleeping, unwoken
  owner. -/
  idle_sleeps : o.idle = true → o.asleep = true ∧ o.inbox = 0 ∧ o.woken = false
  /-- Only a sleeping owner is ever woken. -/
  woken_sleeps : o.woken = true → o.asleep = true
  /-- A sleeping owner has its IDLE bit set or has been woken. -/
  sleeps_idle_or_woken : o.asleep = true → o.idle = true ∨ o.woken = true

/-- The start satisfies the invariant. -/
theorem oinv_init : OInv Owner.init := by
  -- Every flag starts clear, so each clause holds at once.
  refine ⟨?_, ?_, ?_⟩ <;> simp [Owner.init]

/-- **One step keeps the invariant.** -/
theorem ostep_inv {o p : Owner}
    -- The invariant holds before.
    (ho : OInv o)
    -- One step.
    (h : OStep o p) :
    -- Then:
    OInv p := by
  -- Look at the step.
  cases h with
  -- The step was a push.
  | push =>
    -- A push clears IDLE and wakes the owner if IDLE was set.
    refine ⟨fun hi => by simp at hi, fun hw => ?_, fun hs => ?_⟩
    -- Woken now: either it was woken before, or IDLE was set; both mean
    ·
      -- the owner sleeps.
      cases hwo : o.woken
      -- Not woken before: IDLE was set, so the owner sleeps.
      · have hid : o.idle = true := by simpa [hwo] using hw
        -- IDLE means asleep.
        exact (ho.idle_sleeps hid).1
      -- Woken before: then it sleeps.
      · exact ho.woken_sleeps hwo
    -- A sleeping owner after a push: IDLE is clear, so it must be woken.
    · right
      -- Before the push it had IDLE set or was woken; either way it is
      -- woken now.
      rcases ho.sleeps_idle_or_woken hs with hi | hw
      -- IDLE was set: this push wakes it.
      · simp [hi]
      -- It was woken already.
      · simp [hw]
  -- The step put the owner to sleep.
  | rest ha he =>
    -- Going to sleep on an empty inbox; it was awake, so not woken.
    have hnw : o.woken = false := by
      -- If it were woken it would be asleep, but it is awake.
      cases hw : o.woken
      -- Not woken: done.
      · rfl
      -- Woken would mean asleep: a contradiction.
      · have := ho.woken_sleeps hw
        -- Awake and asleep at once.
        simp [ha] at this
    -- Asleep with IDLE set and an empty inbox, not woken.
    refine ⟨fun _ => ⟨rfl, rfl, hnw⟩, fun _ => rfl, fun _ => Or.inl rfl⟩
  -- The step took an inbox.
  | take ha =>
    -- Taking the inbox: awake, IDLE clear, so not woken.
    refine ⟨fun hi => by simp at hi, fun hw => ?_, fun hs => by simp at hs⟩
    -- It was awake, so it was not woken.
    · have := ho.woken_sleeps hw
      -- Awake and asleep at once.
      simp [ha] at this
  -- The step woke the owner.
  | wake hs hw =>
    -- Running again after a wake: awake, not woken; IDLE was clear because
    -- the owner was woken.
    refine ⟨fun hi => ?_, fun hw' => by simp at hw', fun hs' => by simp at hs'⟩
    -- IDLE set would mean not woken, but it was woken.
    · have := (ho.idle_sleeps hi).2.2
      -- Woken and not woken at once.
      simp [hw] at this
  -- The owner woke on its own.
  | selfWake hs =>
    -- Running again on its own: every flag that needs sleep is clear.
    refine ⟨fun hi => by simp at hi, fun hw => by simp at hw, fun hs' => by simp at hs'⟩

/-- Any number of steps keep the invariant. -/
theorem osteps_inv {o p : Owner} (ho : OInv o) (h : OSteps o p) : OInv p := by
  -- Prove it for every number of steps.
  induction h with
  -- No step: nothing changed.
  | refl => exact ho
  -- One more step keeps it.
  | tail _ hstep ih => exact ostep_inv ih hstep

/-- **No lost idle wakeup.** A sleeping owner that nobody has woken has an
empty inbox: a note that arrives always wakes it. TLA+: `NoLostIdleWakeup`.
Rules out: B going to sleep as A's "done" note lands and sleeping on it
forever. -/
theorem no_lost_idle_wakeup {o : Owner}
    -- `o` is reachable from the start.
    (h : OSteps Owner.init o)
    -- The owner sleeps and was not woken.
    (hs : o.asleep = true) (hw : o.woken = false) :
    -- Then:
    o.inbox = 0 := by
  -- The invariant of `o`.
  have hi := osteps_inv oinv_init h
  -- Asleep means IDLE set or woken; not woken, so IDLE is set.
  rcases hi.sleeps_idle_or_woken hs with hid | hwk
  -- IDLE set means an empty inbox.
  · exact (hi.idle_sleeps hid).2.1
  -- Woken contradicts "not woken".
  · simp [hw] at hwk

/-- **A busy owner is never woken.** Only a sleeping owner's waker fires.
TLA+: `BusyNeverWoken`. Rules out: a note waking A while A runs its own
reads. -/
theorem busy_never_woken {o : Owner}
    -- `o` is reachable from the start.
    (h : OSteps Owner.init o)
    -- The waker fired.
    (hw : o.woken = true) :
    -- Then:
    o.asleep = true :=
  -- It is one field of the invariant.
  (osteps_inv oinv_init h).woken_sleeps hw

/-! ### RED: going idle in two steps

The bug: the owner looks ("is the inbox empty?") and, in a second step,
sets IDLE and sleeps, without looking again. A push in between finds no
IDLE bit and wakes nobody. The concrete run: the owner looks (empty), a
note is pushed (no IDLE yet), the owner sets IDLE and sleeps. -/

/-- **RED: going idle in two steps loses a wakeup.** After look, push,
mark: the owner sleeps, was not woken, and has a note. TLA+:
`MC_NonBlocking_Red_LostIdleWakeup`. -/
theorem split_rest_loses_wakeup :
    -- Start awake and empty; the look saw an empty inbox.
    let looked : Owner := Owner.init
    -- A note is pushed: the IDLE bit is still clear, so nobody is woken.
    let pushed : Owner := ⟨looked.inbox + 1, false, looked.asleep, looked.woken || looked.idle⟩
    -- The owner marks IDLE and sleeps, without looking again.
    let marked : Owner := ⟨pushed.inbox, true, true, pushed.woken⟩
    -- It sleeps unwoken with a note in its inbox.
    marked.asleep = true ∧ marked.woken = false ∧ marked.inbox = 1 := by
  -- Compute the three states.
  simp [Owner.init]

/-- **RED: a poll that leaves IDLE set wakes a busy owner.** The owner
sleeps with IDLE set, runs again on its own without clearing IDLE, and a
push then sees the stale bit and wakes it while it runs. TLA+:
`MC_NonBlocking_Red_BusyWoken`. -/
theorem stale_idle_bit_wakes_busy :
    -- The owner went to sleep: IDLE set.
    let slept : Owner := ⟨0, true, true, false⟩
    -- It runs again but forgets to clear IDLE.
    let running : Owner := ⟨slept.inbox, slept.idle, false, false⟩
    -- A push sees IDLE and wakes it.
    let pushed : Owner := ⟨running.inbox + 1, false, running.asleep, running.woken || running.idle⟩
    -- Woken while awake.
    pushed.woken = true ∧ pushed.asleep = false := by
  -- Compute the three states.
  simp

/-! ## Part 4. I/O regolith starts itself, and one thread doing it all

A unit is run only by the poll of a queue that holds it. This part mirrors
the request a miss, or a job regolith starts on a thread's call, puts on
that thread's own queue, and the poll that runs what its queue holds
(`IoQueue::poll`, src/io_queue/queue.rs). -/

/-- Units made, units each queue holds, and units that ran. -/
structure Jobs where
  /-- Every unit made. -/
  made : List Nat
  /-- The units each queue holds, oldest first. -/
  held : Nat → List Nat
  /-- The units that ran. -/
  ran : List Nat

/-- The steps that start and run units. -/
inductive JStep : Jobs → Jobs → Prop
  /-- A call on thread `q` starts unit `u`: it is made and put on `q`'s own
  queue. -/
  | start (j : Jobs) (q u : Nat) :
      -- The state after the step:
      JStep j ⟨u :: j.made, set j.held q (j.held q ++ [u]), j.ran⟩
  /-- Queue `q`'s poll runs at most `b` of the units it holds, oldest
  first. -/
  | poll (j : Jobs) (q b : Nat) :
      -- The state after the step:
      JStep j ⟨j.made, set j.held q ((j.held q).drop b), j.ran ++ (j.held q).take b⟩

/-- Any number of job steps. -/
inductive JSteps : Jobs → Jobs → Prop
  /-- No step. -/
  | refl (j : Jobs) : JSteps j j
  /-- Some steps, then one more. -/
  | tail {a b c : Jobs} : JSteps a b → JStep b c → JSteps a c

/-- Every unit made is held by some queue or has run. -/
def Accounted (j : Jobs) : Prop :=
  -- For every unit made: it ran, or some queue holds it.
  ∀ u ∈ j.made, u ∈ j.ran ∨ ∃ q, u ∈ j.held q

/-- **Every unit regolith starts is held by a queue until it runs**, so some
poll will run it. TLA+: `NoOrphanUnit`. Rules out: a flush started with no
worker that no thread ever runs. -/
theorem job_held {j k : Jobs}
    -- It holds before.
    (hj : Accounted j)
    -- One step.
    (h : JStep j k) :
    -- Then:
    Accounted k := by
  -- Look at the step.
  cases h with
  -- The step started a unit.
  | start q u =>
    -- A start puts the new unit on `q`'s queue.
    intro v hv
    -- Either the new unit or an old one.
    rcases List.mem_cons.1 hv with rfl | hv
    -- The new unit: held by `q`.
    · exact Or.inr ⟨q, by simp⟩
    -- An old unit: it ran, or a queue holds it.
    · rcases hj v hv with hr | ⟨p, hp⟩
      -- It ran: still ran.
      · exact Or.inl hr
      -- Held by `p`: still held, the new unit only adds to `q`'s queue.
      · refine Or.inr ⟨p, ?_⟩
        -- Split on whether p = q (call that fact `hpq`).
        by_cases hpq : p = q
        -- It is `q`'s queue: the old units are still there.
        · subst hpq
          -- Old units stay at the front.
          simp [hp]
        -- Another queue: unchanged.
        · simpa [hpq] using hp
  -- The step was a poll.
  | poll q b =>
    -- A poll moves the oldest `b` units of `q` from held to ran.
    intro v hv
    -- What the unit was before.
    rcases hj v hv with hr | ⟨p, hp⟩
    -- It ran: still ran.
    · exact Or.inl (List.mem_append_left _ hr)
    -- Held by `p`.
    · by_cases hpq : p = q
      -- It is `q`'s: it is in the part that ran or the part kept.
      · subst hpq
        -- Split `q`'s units into the first `b` and the rest.
        have hsplit := List.take_append_drop b (j.held p)
        -- `v` is in one of the two parts.
        rw [← hsplit] at hp
        -- The member is in one of the two parts; look at each.
        rcases List.mem_append.1 hp with ht | hd
        -- In the first `b`: it ran now.
        · exact Or.inl (List.mem_append_right _ ht)
        -- In the rest: still held by `p`.
        · exact Or.inr ⟨p, by simpa using hd⟩
      -- Another queue: unchanged.
      · exact Or.inr ⟨p, by simpa [hpq] using hp⟩

/-- `n` polls of queue `q`, each running at most `b` units. -/
def polls (q b : Nat) : Nat → Jobs → Jobs
  -- No poll: unchanged.
  | 0, j => j
  -- One poll, then the rest.
  | n + 1, j => polls q b n ⟨j.made, set j.held q ((j.held q).drop b), j.ran ++ (j.held q).take b⟩

/-- **One thread with one queue finishes everything.** A thread that polls
its own queue, running at least one unit per poll, empties the queue after
as many polls as it holds units, and every unit it held has run. This is
the single-threaded wasm case. TLA+: `AllDone` in
`MC_NonBlocking_Green_Single`. -/
theorem single_thread_finishes (q b : Nat)
    -- Each poll runs at least one unit.
    (hb : 0 < b) :
    -- For every number of units held and every state:
    ∀ n (j : Jobs), (j.held q).length ≤ n →
      -- after `n` polls the queue holds nothing...
      (polls q b n j).held q = [] ∧
      -- ...and every unit it held has run.
      ∀ u ∈ j.held q, u ∈ (polls q b n j).ran := by
  -- Prove it for every number of polls.
  intro n
  -- By induction on the number of polls.
  induction n with
  -- The case of no step at all.
  | zero =>
    -- No polls: the queue must have held nothing.
    intro j hlen
    -- A list of length at most 0 is empty.
    have he : j.held q = [] := List.eq_nil_of_length_eq_zero (by omega)
    -- Nothing held, so nothing to have run.
    simp [polls, he]
  -- The case of one more step.
  | succ n ih =>
    -- One poll, then `n` more.
    intro j hlen
    -- The state after the first poll.
    let j' : Jobs := ⟨j.made, set j.held q ((j.held q).drop b), j.ran ++ (j.held q).take b⟩
    -- After it, the queue holds at most `n` units: at least one ran.
    have hlen' : (j'.held q).length ≤ n := by
      -- What the queue keeps is the list without its first `b` units.
      simp only [j', set_same, List.length_drop]
      -- Taking away at least one from at most `n + 1` leaves at most `n`.
      omega
    -- The remaining `n` polls finish from `j'`.
    have hrest := ih j' hlen'
    -- The `n + 1` polls are the first poll and then the rest.
    refine ⟨hrest.1, fun u hu => ?_⟩
    -- `u` was in the first `b` units or in the rest.
    have hsplit := List.take_append_drop b (j.held q)
    -- Rewrite the held list as its two parts.
    rw [← hsplit] at hu
    -- The member is in one of the two parts; look at each.
    rcases List.mem_append.1 hu with ht | hd
    -- In the first `b`: it ran in the first poll, and runs stay run.
    ·
      -- Runs only grow over the remaining polls.
      have hgrow : ∀ m (k : Jobs), ∀ v ∈ k.ran, v ∈ (polls q b m k).ran := by
        -- By induction on the polls left.
        intro m
        -- Each poll appends to what ran.
        induction m with
        -- The case of no step at all.
        | zero =>
          -- No poll: unchanged.
          intro k v hv
          -- The same list.
          simpa [polls] using hv
        -- The case of one more step.
        | succ m ihm =>
          -- One poll appends, then the rest keep it.
          intro k v hv
          -- Apply the rest to the state after one poll.
          exact ihm _ v (List.mem_append_left _ hv)
      -- So `u` has run at the end.
      exact hgrow n j' u (List.mem_append_right _ ht)
    -- In the rest: the remaining polls run it.
    · exact hrest.2 u (by simpa [j'] using hd)

/-! ### RED: a job put on no queue

The bug: regolith starts a unit on a thread's call but puts it on no
queue. Polls only run what a queue holds, so the unit never runs. -/

/-- A start that makes unit `u` and puts it nowhere. -/
def startNowhere (j : Jobs) (u : Nat) : Jobs :=
  -- Made, but no queue holds it.
  ⟨u :: j.made, j.held, j.ran⟩

/-- **RED: a job on no queue never runs.** Start unit `u` on no queue in a
state where no queue holds it and it has not run; after any number of
polls of any queue, it still has not run. TLA+:
`MC_NonBlocking_Red_SelfIoNowhere` breaks `NoOrphanUnit`. -/
theorem orphan_never_runs (u q b : Nat) :
    -- For every number of polls and every state where `u` is unheld and
    -- has not run:
    ∀ n (j : Jobs), (∀ p, u ∉ j.held p) → u ∉ j.ran →
      -- it still has not run after `n` polls of `q`.
      u ∉ (polls q b n (startNowhere j u)).ran := by
  -- Polls never run an unheld unit; prove it for every number of polls.
  have key : ∀ n (k : Jobs), (∀ p, u ∉ k.held p) → u ∉ k.ran → u ∉ (polls q b n k).ran := by
    -- By induction on the polls.
    intro n
    -- Prove it for every number of steps, one more at a time.
    induction n with
    -- The case of no step at all.
    | zero =>
      -- No poll: unchanged.
      intro k _ hr
      -- Still not run.
      simpa [polls] using hr
    -- The case of one more step.
    | succ n ih =>
      -- One poll, then the rest.
      intro k hh hr
      -- After one poll `u` is still unheld and not run.
      apply ih
      -- Still unheld: the poll only shrinks `q`'s queue.
      · intro p hp
        -- Split on whether p = q (call that fact `hpq`).
        by_cases hpq : p = q
        -- It is `q`'s queue: what is kept was there before.
        · subst hpq
          -- Kept units come from the old list.
          simp only [set_same] at hp
          -- So `u` was held before, which it was not.
          exact hh p (List.mem_of_mem_drop hp)
        -- Another queue: unchanged.
        · exact hh p (by simpa [hpq] using hp)
      -- Still not run: what ran is the old list plus held units.
      · simp only [List.mem_append, not_or]
        -- Not in the old list, and not among `q`'s held units.
        exact ⟨hr, fun ht => hh q (List.mem_of_mem_take ht)⟩
  -- Apply it to the state after the bad start.
  intro n j hh hr
  -- The bad start leaves `u` unheld and not run.
  exact key n (startNowhere j u) hh hr

/-! ## Part 5. The commit side: a sync round, and a stall's unit

This part mirrors the units the commit side adds (D53). The commits made by
`commit_nowait` that need an fsync wait on one job, the sync round that
covers their records (`Round`, `src/engine/commit/round.rs`). That job
is a unit of Part 1, so Part 1 already says it is claimed by one CAS and
that every waiting queue gets one "done" note. Here we follow what the
claimer does with it (sync, then apply and publish, then tell), and where
each commit's ticket becomes ready: at its own queue's poll.

Then the write stall (`StallSignal`, `src/engine/commit/stall.rs`). A write
a stall stops waits on a *passive* unit, which no poll runs: only the
stall's clearing lands it. A stopped writer finds or installs the unit,
registers, and only then looks at the stall again, landing the unit itself
if the stall cleared meanwhile. Tiny example of what that second look
saves: writer A sees the stall; the clearer clears it and finds no unit to
land; A installs a fresh unit. Without A's second look, nobody would ever
land it, and A's write would wait forever. -/

/-- One sync round's job, how far its claimer got, and the waiting commits'
tickets. -/
structure Round where
  /-- The queues of the commits that wait on the round, each registered on
  the job. -/
  queues : List Nat
  /-- The job's state: free, claimed by a thread, or done. -/
  st : UState
  /-- How far the claimer got: 0 claimed, 1 synced, 2 published. -/
  phase : Nat
  /-- Ghost: how many times the round's fsync ran. -/
  syncs : Nat
  /-- The fsync ran: the round's records are durable. -/
  synced : Bool
  /-- The round's commits are published: readers see them. -/
  visible : Bool
  /-- The waiting queues the claimer still owes a "done" note. -/
  owed : List Nat
  /-- How many "done" notes wait in each queue's inbox. -/
  inbox : Nat → Nat
  /-- Ghost: (waiting queue, thread whose step made its ticket ready). -/
  ready : List (Nat × Nat)

/-- A round nobody has claimed: its job free, nothing synced, told or ready. -/
def Round.init (queues : List Nat) : Round :=
  -- Only the waiting queues are set; everything else starts empty.
  ⟨queues, .free, 0, 0, false, false, [], fun _ => 0, []⟩

/-- The steps of one round's job, one atomic step each. -/
inductive RoundStep : Round → Round → Prop
  /-- Thread `t` claims the job with one CAS from free: the poll of a
  waiting queue, or a thread that needs the log durable inside a blocking
  call. -/
  | claim (r : Round) (t : Nat)
      -- The CAS succeeds only on a free job.
      (h : r.st = .free) :
      -- The state after the step:
      RoundStep r { r with st := .claimed t }
  /-- The claimer runs the round's fsync. -/
  | sync (r : Round) (t : Nat)
      -- `t` holds the claim.
      (h : r.st = .claimed t)
      -- It has not synced yet.
      (hp : r.phase = 0) :
      -- The state after the step:
      RoundStep r { r with phase := 1, syncs := r.syncs + 1, synced := true }
  /-- The claimer publishes the round's commits to readers. -/
  | publish (r : Round) (t : Nat)
      -- `t` holds the claim.
      (h : r.st = .claimed t)
      -- It synced.
      (hp : r.phase = 1) :
      -- The state after the step:
      RoundStep r { r with phase := 2, visible := true }
  /-- The claimer finishes: the job is done and every waiting queue is owed
  a "done" note. -/
  | finish (r : Round) (t : Nat)
      -- `t` holds the claim.
      (h : r.st = .claimed t)
      -- It published.
      (hp : r.phase = 2) :
      -- The state after the step:
      RoundStep r { r with st := .done, owed := r.queues }
  /-- The claimer puts the "done" note into waiting queue `q`'s inbox. -/
  | tell (r : Round) (q : Nat)
      -- `q` is still owed its note.
      (h : q ∈ r.owed) :
      -- The state after the step:
      RoundStep r { r with owed := r.owed.erase q, inbox := set r.inbox q (r.inbox q + 1) }
  /-- Queue `q`'s owner polls, takes the note, and the ticket is ready: the
  step is `q`'s own thread's. -/
  | deliver (r : Round) (q : Nat)
      -- A note waits in `q`'s inbox.
      (h : 0 < r.inbox q) :
      -- The state after the step:
      RoundStep r { r with inbox := set r.inbox q (r.inbox q - 1), ready := (q, q) :: r.ready }

/-- Any number of a round's steps, one after another. -/
inductive RoundSteps : Round → Round → Prop
  /-- No step at all. -/
  | refl (r : Round) : RoundSteps r r
  /-- Some steps, then one more. -/
  | tail {a b c : Round} : RoundSteps a b → RoundStep b c → RoundSteps a c

/-- What holds of a round's job in every reachable state. -/
structure RoundInv (r : Round) : Prop where
  /-- The fsync ran once if the round is synced, and never otherwise. -/
  syncs_eq : r.syncs = if r.synced then 1 else 0
  /-- Synced exactly when the claimer got past its fsync. -/
  synced_phase : r.synced = true ↔ 1 ≤ r.phase
  /-- A free job has nobody's progress on it. -/
  free_phase : r.st = .free → r.phase = 0
  /-- Readers see the round's commits only once it is synced. -/
  vis_synced : r.visible = true → r.synced = true
  /-- Every ready ticket was made ready by its own queue's thread. -/
  own : ∀ p ∈ r.ready, p.1 = p.2

/-- A round nobody has claimed satisfies the invariant. -/
theorem round_inv_init (m : List Nat) : RoundInv (Round.init m) := by
  -- Each field holds because everything starts empty or false.
  refine ⟨?_, ?_, ?_, ?_, ?_⟩ <;> simp [Round.init]

/-- **One step keeps the invariant.** Each kind of step is checked in turn. -/
theorem round_step_inv {r r' : Round}
    -- The invariant holds before the step.
    (hi : RoundInv r)
    -- One step.
    (h : RoundStep r r') :
    -- Then:
    RoundInv r' := by
  -- Look at the step.
  cases h with
  -- A claim only changes the job's state, to claimed.
  | claim t hfree =>
    -- The rest of the invariant is untouched; a claimed job is not free.
    exact ⟨hi.syncs_eq, hi.synced_phase, fun h => by simp at h, hi.vis_synced, hi.own⟩
  -- The fsync.
  | sync t hcl hp =>
    -- Before it the round was not synced: the claimer was at phase 0.
    have hs : r.synced = false := by
      -- Either it was synced or not.
      cases hsy : r.synced with
      -- Not synced: that is the claim.
      | false => rfl
      -- Synced would mean phase at least 1, but it is 0.
      | true => have := hi.synced_phase.1 hsy; omega
    -- So the fsync had run no times.
    have h0 : r.syncs = 0 := by simpa [hs] using hi.syncs_eq
    -- Now it ran once, the round is synced, at phase 1.
    refine ⟨?_, ?_, ?_, ?_, hi.own⟩
    -- One fsync, and synced.
    · simp [h0]
    -- Synced, and past phase 0.
    · simp
    -- The job is claimed, not free.
    · intro h
      -- The step left the job's state as it was.
      have h' : r.st = .free := h
      -- It was claimed by `t`, not free.
      rw [hcl] at h'
      -- Claimed and free are different states.
      cases h'
    -- Synced now.
    · intro _; rfl
  -- The publication.
  | publish t hcl hp =>
    -- The claimer is at phase 1, so the round is synced.
    have hs : r.synced = true := hi.synced_phase.2 (by omega)
    -- Visible now, still synced, at phase 2.
    refine ⟨hi.syncs_eq, ?_, ?_, ?_, hi.own⟩
    -- Synced, and past phase 0.
    · simp [hs]
    -- The job is claimed, not free.
    · intro h
      -- The step left the job's state as it was.
      have h' : r.st = .free := h
      -- It was claimed by `t`, not free.
      rw [hcl] at h'
      -- Claimed and free are different states.
      cases h'
    -- Visible, and synced.
    · intro _; exact hs
  -- The finish only marks the job done and owes the notes.
  | finish t hcl hp =>
    -- A done job is not free; the rest is untouched.
    exact ⟨hi.syncs_eq, hi.synced_phase, fun h => by simp at h, hi.vis_synced, hi.own⟩
  -- A note told changes only the inboxes and what is owed.
  | tell q hq =>
    -- Nothing the invariant reads changes.
    exact ⟨hi.syncs_eq, hi.synced_phase, hi.free_phase, hi.vis_synced, hi.own⟩
  -- A delivery adds one ticket, made ready by its own queue's thread.
  | deliver q hq =>
    -- Everything else is untouched; check the new ready ticket.
    refine ⟨hi.syncs_eq, hi.synced_phase, hi.free_phase, hi.vis_synced, ?_⟩
    -- A ready ticket is the new one or an old one.
    intro p hp
    -- Split the list membership.
    rcases List.mem_cons.1 hp with rfl | hp'
    -- The new one: queue `q`, made ready by `q`.
    · rfl
    -- An old one: the invariant had it.
    · exact hi.own p hp'

/-- **The invariant holds after any number of steps.** -/
theorem round_steps_inv {r r' : Round}
    -- Steps from `r` to `r'`.
    (h : RoundSteps r r')
    -- The invariant holds at `r`.
    (hi : RoundInv r) :
    -- Then:
    RoundInv r' := by
  -- By induction on the steps.
  induction h with
  -- No step: the same state.
  | refl => exact hi
  -- Steps, then one more: the last step keeps it.
  | tail _ hs ih => exact round_step_inv ih hs

/-- **A round's fsync runs at most once.** One CAS lets one claimer in, and
only the claimer syncs. TLA+: `SingleRun` on a round
(`MC_NonBlocking_Green_Commit`), `OneRound` (`CommitPipeline.tla`). Rules
out: queues 1 and 2 both polling and both syncing round 4. -/
theorem round_sync_once {m : List Nat} {r : Round}
    -- `r` is reachable from a round nobody has claimed.
    (h : RoundSteps (Round.init m) r) :
    -- Then:
    r.syncs ≤ 1 := by
  -- The invariant of `r`.
  have hi := round_steps_inv h (round_inv_init m)
  -- The count is 1 or 0 by the invariant.
  rw [hi.syncs_eq]
  -- Either way it is at most 1.
  split <;> omega

/-- **Readers see a round's commits only after its fsync.** TLA+:
`VisibleAfterSync`, `DurableBeforeVisible` (`CommitPipeline.tla`). Rules
out: the commits of round 4 visible while its fsync still waits as a job on
the waiting queues. -/
theorem round_visible_after_sync {m : List Nat} {r : Round}
    -- `r` is reachable from a round nobody has claimed.
    (h : RoundSteps (Round.init m) r)
    -- The round's commits are visible.
    (hv : r.visible = true) :
    -- Then:
    r.synced = true :=
  -- One field of the invariant.
  (round_steps_inv h (round_inv_init m)).vis_synced hv

/-- **A commit's ticket becomes ready only at its own queue's poll.** TLA+:
`DeliveredOnOwnPoll`. Rules out: queue 2's thread running round 4 and
making queue 1's ticket ready on thread 2, running 1's callbacks there
while thread 1 is busy. -/
theorem ticket_ready_only_at_own_poll {m : List Nat} {r : Round}
    -- `r` is reachable from a round nobody has claimed.
    (h : RoundSteps (Round.init m) r)
    -- A ticket that is ready, and the thread whose step made it so.
    {q t : Nat} (hr : (q, t) ∈ r.ready) :
    -- Then:
    t = q :=
  -- One field of the invariant, read backwards.
  ((round_steps_inv h (round_inv_init m)).own (q, t) hr).symm

/-! ### RED: the runner readies the waiting commits' tickets itself

The bug: the thread that runs the round tells every waiting queue by making
its ticket ready right there, instead of putting a note in the queue's inbox
for its own poll. -/

/-- The bad finish: the job is done and every waiting queue's ticket is ready,
made so by the landing thread `t`. -/
def finishDelivering (r : Round) (t : Nat) : Round :=
  -- Done, and each waiting queue marked ready by `t`.
  { r with st := .done, ready := r.queues.map (fun q => (q, t)) ++ r.ready }

/-- **RED: a runner that readies the tickets breaks `ticket_ready_only_at_own_poll`.**
Queues 1 and 2 wait; thread 2 runs the round and makes queue 1's ticket ready
on thread 2. TLA+: `MC_NonBlocking_Red_DeliverOnLander` breaks
`DeliveredOnOwnPoll`. -/
theorem lander_delivery_breaks_own_poll :
    -- Some ticket ends up ready by a thread that is not its own.
    ∃ q t, (q, t) ∈ (finishDelivering (Round.init [1, 2]) 2).ready ∧ t ≠ q := by
  -- Queue 1, made ready by thread 2.
  refine ⟨1, 2, ?_, by decide⟩
  -- It is on the list the bad finish builds.
  simp [finishDelivering, Round.init]

/-- Where a writer the stall stopped is: running, it saw the stall, or it
registered its wait and is about to look at the stall again. -/
inductive WPhase where
  /-- Not inside a stopped write. -/
  | none
  /-- It saw the stall. -/
  | seen
  /-- It registered its wait and will look at the stall again. -/
  | recheck
  -- Two phases can be compared for equality.
  deriving DecidableEq

/-- One stall, its passive unit, and its writers. -/
structure Stall where
  /-- The stall is in force. -/
  stalled : Bool
  /-- An unlanded unit for the stall is installed. -/
  pending : Bool
  /-- Each writer's phase. -/
  writer : Nat → WPhase

/-- The start: the stall is in force, no unit is installed, nobody wrote. -/
def Stall.init : Stall :=
  -- In force, nothing installed, every writer outside a stopped write.
  ⟨true, false, fun _ => .none⟩

/-- The steps of a stall, one atomic step each, as the code takes them. -/
inductive SStep : Stall → Stall → Prop
  /-- Writer `w`'s write meets the stall and stops. -/
  | see (s : Stall) (w : Nat)
      -- The stall is in force.
      (hst : s.stalled = true)
      -- `w` is not inside another stopped write.
      (hw : s.writer w = .none) :
      -- The state after the step:
      SStep s { s with writer := set s.writer w .seen }
  /-- Writer `w` finds or installs the stall's unit and registers on it. -/
  | install (s : Stall) (w : Nat)
      -- `w` saw the stall.
      (hw : s.writer w = .seen) :
      -- The state after the step:
      SStep s { s with pending := true, writer := set s.writer w .recheck }
  /-- Writer `w` looks at the stall again: if it cleared, `w` lands the
  unit itself. -/
  | recheck (s : Stall) (w : Nat)
      -- `w` registered and has not looked again.
      (hw : s.writer w = .recheck) :
      -- The state after the step:
      SStep s { s with pending := s.pending && s.stalled, writer := set s.writer w .none }
  /-- Background work clears the stall and lands the unit, if one is
  installed. -/
  | clear (s : Stall)
      -- The stall is in force.
      (hst : s.stalled = true) :
      -- The state after the step:
      SStep s { s with stalled := false, pending := false }

/-- Any number of stall steps, one after another. -/
inductive SSteps : Stall → Stall → Prop
  /-- No step at all. -/
  | refl (s : Stall) : SSteps s s
  /-- Some steps, then one more. -/
  | tail {a b c : Stall} : SSteps a b → SStep b c → SSteps a c

/-- The promise: a cleared stall with an unlanded unit has a writer about to
look again, which will land it. -/
def StallSafe (s : Stall) : Prop :=
  -- Cleared and still pending means somebody will land it.
  s.stalled = false → s.pending = true → ∃ w, s.writer w = .recheck

/-- **Every step leaves the promise true**, whatever came before: the
order (register, then look again) carries it on its own. -/
theorem sstep_safe {s s' : Stall}
    -- One step.
    (h : SStep s s') :
    -- Then:
    StallSafe s' := by
  -- Look at the step.
  cases h with
  -- A write that meets the stall needs it in force, and it stays so.
  | see w hst hw =>
    -- Assume the stall cleared: it did not.
    intro hc _
    -- The step left the stall as it was: in force.
    simp [hst] at hc
  -- Installing: the installer itself is about to look again.
  | install w hw =>
    -- Whatever the stall, `w` is the writer that will land it.
    intro _ _
    -- `w` is at its second look.
    exact ⟨w, by simp⟩
  -- The second look lands the unit if the stall cleared.
  | recheck w hw =>
    -- Assume the stall cleared and the unit is still pending.
    intro hc hp
    -- The step leaves the stall as it was, so it had cleared.
    simp only at hc
    -- Then the second look landed the unit: not pending.
    simp [hc] at hp
  -- Clearing lands the unit.
  | clear hst =>
    -- Assume the unit is still pending after the clear.
    intro _ hp
    -- The clear set it landed.
    simp at hp

/-- **A cleared stall leaves no unlanded unit behind, except one a writer
is about to land.** TLA+: `StallLandsWhenClear`. Rules out: a writer
installing its unit just after the clearer looked, and nobody landing it. -/
theorem stall_lands_when_clear {s : Stall}
    -- `s` is reachable from the start.
    (h : SSteps Stall.init s) :
    -- Then:
    StallSafe s := by
  -- By induction on the steps.
  induction h with
  -- At the start the stall is in force, so the promise is empty.
  | refl => intro hc; simp [Stall.init] at hc
  -- Steps, then one more: the last step makes it true.
  | tail _ hs _ => exact sstep_safe hs

/-! ### RED: a writer that never looks at the stall again

The bug: a stopped writer installs or finds the unit, registers, and stops
there. -/

/-- The broken steps: seeing the stall, clearing it, and an install with no
second look. -/
inductive BadStall : Stall → Stall → Prop
  /-- Writer `w`'s write meets the stall and stops. -/
  | see (s : Stall) (w : Nat)
      -- The stall is in force.
      (hst : s.stalled = true)
      -- `w` is not inside another stopped write.
      (hw : s.writer w = .none) :
      -- The state after the step:
      BadStall s { s with writer := set s.writer w .seen }
  /-- Background work clears the stall and lands the unit, if installed. -/
  | clear (s : Stall)
      -- The stall is in force.
      (hst : s.stalled = true) :
      -- The state after the step:
      BadStall s { s with stalled := false, pending := false }
  /-- The bug: `w` installs the unit and leaves, never looking again. -/
  | installAndLeave (s : Stall) (w : Nat)
      -- `w` saw the stall.
      (hw : s.writer w = .seen) :
      -- The state after the step:
      BadStall s { s with pending := true, writer := set s.writer w .none }

/-- Any number of broken steps. -/
inductive BadStalls : Stall → Stall → Prop
  /-- No step at all. -/
  | refl (s : Stall) : BadStalls s s
  /-- Some steps, then one more. -/
  | tail {a b c : Stall} : BadStalls a b → BadStall b c → BadStalls a c

/-- **RED: without the second look a unit is stranded.** Writer 1 sees the
stall; the stall clears with no unit to land; writer 1 installs one and
leaves. The stall is cleared, the unit unlanded, and no writer will look
again. TLA+: `MC_NonBlocking_Red_NoRecheck` breaks `StallLandsWhenClear`. -/
theorem no_recheck_strands_stall :
    -- Some reachable state breaks the promise.
    ∃ s, BadStalls Stall.init s ∧ ¬ StallSafe s := by
  -- The three steps: 1 sees, the stall clears, 1 installs and leaves.
  refine ⟨_, .tail (.tail (.tail (.refl _) (.see _ 1 rfl rfl)) (.clear _ rfl))
    (.installAndLeave _ 1 (by simp [Stall.init])), ?_⟩
  -- The promise would need a writer at its second look.
  intro hs
  -- The stall is cleared and the unit pending, so the promise names one.
  obtain ⟨w, hw⟩ := hs rfl rfl
  -- But every writer is outside a stopped write.
  by_cases h1 : w = 1
  -- Writer 1 left.
  · subst h1
    -- Its phase is none.
    simp at hw
  -- Any other writer never wrote.
  · simp [Stall.init, set, h1] at hw

/-! ## Part 6. Timed waits: once, by the owner's poll, not before the due time

This part mirrors the timed waits of a queue: `Timers` in
`src/io_queue/timers.rs`, `IoRuntime::timer` in `src/engine/io/mod.rs`, and
the end of `IoQueue::poll` and the drop of a queue in
`src/io_queue/queue.rs`. TLA+ checks the same rules together with the rest
of the queue (`TimerNotEarly`, `TimerOnOwnQueue`, `DroppedHoldsNothing`, and
`AllDone` for a wait that must finish).

The story. A transaction that lost its commit may wait a little before it
runs again. A thread that must never block cannot sleep, so it asks its
queue for a *timed wait*: "ready when the clock reads 100". Nobody is woken
for it and nothing sleeps for it. The owner's poll looks at the clock, and
the first poll that finds 100 or later marks the wait ready. Each poll also
says how long the owner may idle until the next wait is due, and an owner
that polls again by then completes it.

Tiny example. At time 40 thread A asks for a wait due at 100. A's poll at
time 60 leaves it alone and says "idle at most 40". A's poll at time 100
completes it. If A drops its queue at time 70 instead, the wait is completed
then: nothing else ever would. -/

/-- Where one timed wait is. -/
inductive WState where
  /-- Nobody has asked for it. -/
  | unasked
  /-- Asked for: its note is in the queue's inbox. -/
  | noted
  /-- The owner's poll took the note in and holds the wait. -/
  | held
  /-- Completed: the task that waits on it may run again. -/
  | over
  -- Two states can be compared for equality.
  deriving DecidableEq

/-- Whether a wait is asked for and not completed yet. -/
def WState.pending : WState → Bool
  -- A note in the inbox is pending.
  | .noted => true
  -- So is a wait the owner holds.
  | .held => true
  -- One nobody asked for is not.
  | .unasked => false
  -- Nor is one that is over.
  | .over => false

/-- A pending wait is not over. -/
theorem pending_not_over {w : WState}
    -- The wait is pending.
    (h : w.pending = true) :
    -- Then:
    w ≠ .over := by
  -- Look at where the wait is: "over" is not pending.
  cases w <;> simp [WState.pending] at h ⊢

/-- One queue's timed waits at one moment. A wait is named by a natural
number, and so is a thread. -/
structure Timed where
  /-- What the queue's clock reads. -/
  now : Nat
  /-- Where each wait is. -/
  st : Nat → WState
  /-- Each wait's due time, fixed when it is asked for. -/
  due : Nat → Nat
  /-- Whether the queue was dropped. -/
  gone : Bool
  /-- Ghost: how many times each wait was completed. -/
  fires : Nat → Nat
  /-- Ghost: the thread whose step completed each wait. -/
  who : Nat → Nat
  /-- Ghost: what the clock read when each wait was completed. -/
  seen : Nat → Nat
  /-- Ghost: whether the wait was completed by the drop of its queue. -/
  byDrop : Nat → Bool

/-- The start: time zero, nothing asked for, the queue open. -/
def Timed.init : Timed :=
  -- Every field starts empty.
  ⟨0, fun _ => .unasked, fun _ => 0, false, fun _ => 0, fun _ => 0, fun _ => 0, fun _ => false⟩

/-- The steps of the timed waits of a queue that thread `owner` owns, one
atomic step each, as the code takes them. -/
inductive TStep (owner : Nat) : Timed → Timed → Prop
  /-- Some thread asks for wait `w`, due at time `d`: a note goes to the
  queue's inbox (`IoRuntime::timer`). -/
  | ask (s : Timed) (w d : Nat)
      -- Nobody asked for `w` before.
      (hnew : s.st w = .unasked)
      -- The queue is open (a dropped one hands the wait back ready).
      (hopen : s.gone = false) :
      -- The state after the step:
      TStep owner s { s with st := set s.st w .noted, due := set s.due w d }
  /-- The owner's poll takes the note for `w` out of the inbox and holds the
  wait (`Timers::record`). -/
  | record (s : Timed) (w : Nat)
      -- The note is in the inbox.
      (hn : s.st w = .noted) :
      -- The state after the step:
      TStep owner s { s with st := set s.st w .held }
  /-- Time passes. -/
  | tick (s : Timed) :
      -- The state after the step:
      TStep owner s { s with now := s.now + 1 }
  /-- The owner's poll finds `w` due and completes it (`Timers::fire`). -/
  | fire (s : Timed) (w : Nat)
      -- The owner holds the wait.
      (hh : s.st w = .held)
      -- The clock reads its due time or later.
      (hdue : s.due w ≤ s.now) :
      -- The state after the step:
      TStep owner s { s with st := set s.st w .over,
                             fires := set s.fires w (s.fires w + 1),
                             who := set s.who w owner,
                             seen := set s.seen w s.now }
  /-- The owner drops the queue: every wait asked for and not completed is
  completed now (`Timers::complete_all`, and the notes still in the inbox). -/
  | drop (s : Timed)
      -- The queue was open.
      (hopen : s.gone = false) :
      -- The state after the step:
      TStep owner s { s with
        gone := true,
        st := fun w => if (s.st w).pending then .over else s.st w,
        fires := fun w => if (s.st w).pending then s.fires w + 1 else s.fires w,
        who := fun w => if (s.st w).pending then owner else s.who w,
        seen := fun w => if (s.st w).pending then s.now else s.seen w,
        byDrop := fun w => if (s.st w).pending then true else s.byDrop w }

/-- Any number of steps, one after another. -/
inductive TSteps (owner : Nat) : Timed → Timed → Prop
  /-- No step at all. -/
  | refl (s : Timed) : TSteps owner s s
  /-- Some steps, then one more. -/
  | tail {a b c : Timed} : TSteps owner a b → TStep owner b c → TSteps owner a c

/-- What holds of every timed wait in every reachable state. -/
structure TInv (owner : Nat) (s : Timed) : Prop where
  /-- A wait that is not over was never completed. -/
  fires_pending : ∀ w, s.st w ≠ .over → s.fires w = 0
  /-- No wait was completed more than once. -/
  fires_le : ∀ w, s.fires w ≤ 1
  /-- A completed wait was completed by a step of the owner. -/
  by_owner : ∀ w, s.st w = .over → s.who w = owner
  /-- A wait a poll completed was due when that poll read the clock. -/
  not_early : ∀ w, s.st w = .over → s.byDrop w = false → s.due w ≤ s.seen w
  /-- A dropped queue holds no wait that is still pending. -/
  gone_empty : s.gone = true → ∀ w, (s.st w).pending = false

/-- The start satisfies the invariant. -/
theorem tinv_init (owner : Nat) : TInv owner Timed.init := by
  -- Each field holds because nothing is asked for and the queue is open.
  refine ⟨?_, ?_, ?_, ?_, ?_⟩ <;> intros <;> simp_all [Timed.init]

/-- **One step keeps the invariant.** Each kind of step is checked in turn. -/
theorem tstep_inv {owner : Nat} {s t : Timed}
    -- The invariant holds before the step.
    (hs : TInv owner s)
    -- One step from `s` to `t`.
    (h : TStep owner s t) :
    -- Then:
    TInv owner t := by
  -- Look at which kind of step it was.
  cases h with
  -- A wait was asked for.
  | ask w d hnew hopen =>
    -- Asking only moves `w` from "not asked" to "noted" and fixes its due time.
    refine ⟨fun x hx => ?_, hs.fires_le, fun x hx => ?_, fun x hx hd => ?_, fun hg => ?_⟩
    -- Not completed: `w` was not over before either; others are unchanged.
    · by_cases hxw : x = w
      -- It is `w`: it was not asked for, so it was not over.
      · subst hxw
        -- "Not asked for" is not "over", so the old fact applies.
        exact hs.fires_pending x (by simp [hnew])
      -- Another wait: unchanged.
      · exact hs.fires_pending x (by simpa [hxw] using hx)
    -- Completed by the owner: `w` is noted, not over, so `x` is another wait.
    · by_cases hxw : x = w
      -- It is `w`: noted is not over.
      · subst hxw
        -- So the assumption cannot hold.
        simp at hx
      -- Another wait: unchanged.
      · exact hs.by_owner x (by simpa [hxw] using hx)
    -- Not early: the same split.
    · by_cases hxw : x = w
      -- It is `w`: noted is not over.
      · subst hxw
        -- So the assumption cannot hold.
        simp at hx
      -- Another wait: its state and its due time are unchanged.
      · simpa [hxw] using hs.not_early x (by simpa [hxw] using hx) hd
    -- The queue is open, so it is not a dropped one.
    · simp [hopen] at hg
  -- The owner took a note in.
  | record w hn =>
    -- Recording only moves `w` from "noted" to "held".
    refine ⟨fun x hx => ?_, hs.fires_le, fun x hx => ?_, fun x hx hd => ?_, fun hg => ?_⟩
    -- Not completed: `w` was noted, not over; others are unchanged.
    · by_cases hxw : x = w
      -- It is `w`: it was noted.
      · subst hxw
        -- "Noted" is not "over", so the old fact applies.
        exact hs.fires_pending x (by simp [hn])
      -- Another wait: unchanged.
      · exact hs.fires_pending x (by simpa [hxw] using hx)
    -- Completed by the owner: `w` is held, not over.
    · by_cases hxw : x = w
      -- It is `w`: held is not over.
      · subst hxw
        -- So the assumption cannot hold.
        simp at hx
      -- Another wait: unchanged.
      · exact hs.by_owner x (by simpa [hxw] using hx)
    -- Not early: the same split.
    · by_cases hxw : x = w
      -- It is `w`: held is not over.
      · subst hxw
        -- So the assumption cannot hold.
        simp at hx
      -- Another wait: unchanged.
      · exact hs.not_early x (by simpa [hxw] using hx) hd
    -- A dropped queue held no pending wait, but `w` was noted: impossible.
    · have hp := hs.gone_empty hg w
      -- "Noted" is pending, which contradicts that.
      simp [hn, WState.pending] at hp
  -- Time passed.
  | tick =>
    -- No fact of the invariant mentions the clock.
    exact ⟨hs.fires_pending, hs.fires_le, hs.by_owner, hs.not_early, hs.gone_empty⟩
  -- The owner's poll completed a wait.
  | fire w hh hdue =>
    -- Before, `w` was held: not over, so never completed.
    have h0 : s.fires w = 0 := hs.fires_pending w (by simp [hh])
    -- Firing makes `w` over, counts it once, and records who and when.
    refine ⟨fun x hx => ?_, fun x => ?_, fun x hx => ?_, fun x hx hd => ?_, fun hg => ?_⟩
    -- Not completed: `w` is over now, so `x` is another wait.
    · by_cases hxw : x = w
      -- It is `w`: it is over.
      · subst hxw
        -- So the assumption cannot hold.
        simp at hx
      -- Another wait: its state and its count are unchanged.
      · simpa [hxw] using hs.fires_pending x (by simpa [hxw] using hx)
    -- At most once: `w` goes from zero to one.
    · by_cases hxw : x = w
      -- It is `w`: zero plus one.
      · subst hxw
        -- The count was zero.
        simp [h0]
      -- Another wait: unchanged.
      · simpa [hxw] using hs.fires_le x
    -- By the owner: this step is the owner's.
    · by_cases hxw : x = w
      -- It is `w`: the step recorded the owner.
      · subst hxw
        -- Read the new value.
        simp
      -- Another wait: unchanged.
      · simpa [hxw] using hs.by_owner x (by simpa [hxw] using hx)
    -- Not early: the poll saw the clock at the due time or later.
    · by_cases hxw : x = w
      -- It is `w`: the clock it saw is the clock now.
      · subst hxw
        -- And the step needed the wait to be due.
        simpa using hdue
      -- Another wait: unchanged.
      · simpa [hxw] using hs.not_early x (by simpa [hxw] using hx) hd
    -- A dropped queue held no pending wait, but `w` was held: impossible.
    · have hp := hs.gone_empty hg w
      -- "Held" is pending, which contradicts that.
      simp [hh, WState.pending] at hp
  -- The owner dropped the queue.
  | drop hopen =>
    -- Every pending wait is over now, counted once, by the owner, by the drop.
    refine ⟨fun x hx => ?_, fun x => ?_, fun x hx => ?_, fun x hx hd => ?_, fun _ x => ?_⟩
    -- Not completed: such a wait was not pending, so the drop left it alone.
    · by_cases hp : (s.st x).pending = true
      -- It was pending: then it is over now.
      · simp [hp] at hx
      -- It was not pending: its state and its count are unchanged.
      · simp only [hp] at hx ⊢
        -- The old fact applies.
        simpa using hs.fires_pending x (by simpa using hx)
    -- At most once: a pending wait had count zero.
    · by_cases hp : (s.st x).pending = true
      -- It was pending: zero plus one.
      · simp [hp, hs.fires_pending x (pending_not_over hp)]
      -- It was not pending: unchanged.
      · simpa [hp] using hs.fires_le x
    -- By the owner: the drop is the owner's step.
    · by_cases hp : (s.st x).pending = true
      -- It was pending: the drop recorded the owner.
      · simp [hp]
      -- It was not pending: it was over before, by the owner.
      · simp only [hp] at hx ⊢
        -- The old fact applies.
        simpa using hs.by_owner x (by simpa using hx)
    -- Not early: a wait the drop completed is marked so.
    · by_cases hp : (s.st x).pending = true
      -- It was pending: then the drop marked it, against the assumption.
      · simp [hp] at hd
      -- It was not pending: unchanged.
      · simp only [hp] at hx hd ⊢
        -- The old fact applies.
        simpa using hs.not_early x (by simpa using hx) (by simpa using hd)
    -- Nothing is pending after the drop: say what `x`'s new state is.
    · show (if (s.st x).pending then WState.over else s.st x).pending = false
      -- Was it pending before?
      by_cases hp : (s.st x).pending = true
      -- It was: put that into the test of the new state,
      · rw [hp]
        -- which picks "over", and "over" is not pending.
        rfl
      -- It was not: say so as an equation,
      · have hf : (s.st x).pending = false := by simpa using hp
        -- put it into the test of the new state,
        rw [hf]
        -- which picks the old state, still not pending.
        exact hf

/-- The invariant holds in every reachable state. -/
theorem timed_inv {owner : Nat} {s : Timed}
    -- `s` is reachable from the start.
    (h : TSteps owner Timed.init s) :
    -- Then:
    TInv owner s := by
  -- By induction on the steps.
  induction h with
  -- No step: the start.
  | refl => exact tinv_init owner
  -- Steps, then one more: one step keeps it.
  | tail _ hstep ih => exact tstep_inv ih hstep

/-- **A timed wait is completed at most once.** TLA+: a wait's slot goes
from pending to ready once. Rules out: a wait fired by one poll and fired
again by the next, waking its task twice. -/
theorem timed_wait_completed_once {owner : Nat} {s : Timed}
    -- `s` is reachable from the start.
    (h : TSteps owner Timed.init s)
    -- Any wait.
    (w : Nat) :
    -- Then:
    s.fires w ≤ 1 :=
  -- This is one fact of the invariant.
  (timed_inv h).fires_le w

/-- **A timed wait is completed only by a step of its queue's owner.**
TLA+: `TimerOnOwnQueue`, with `DeliveredOnOwnPoll`. Rules out: thread B
making A's wait ready, running A's retry on B's thread. -/
theorem timed_wait_completed_by_owner {owner : Nat} {s : Timed}
    -- `s` is reachable from the start.
    (h : TSteps owner Timed.init s)
    -- Any wait
    (w : Nat)
    -- that is completed.
    (hover : s.st w = .over) :
    -- Then:
    s.who w = owner :=
  -- This is one fact of the invariant.
  (timed_inv h).by_owner w hover

/-- **A poll never completes a timed wait before its due time.** TLA+:
`TimerNotEarly`. Rules out: a retry that asked to wait until 100 running
again at 60. -/
theorem timed_wait_not_before_due {owner : Nat} {s : Timed}
    -- `s` is reachable from the start.
    (h : TSteps owner Timed.init s)
    -- Any wait
    (w : Nat)
    -- that is completed,
    (hover : s.st w = .over)
    -- and not by the drop of its queue.
    (hpoll : s.byDrop w = false) :
    -- Then:
    s.due w ≤ s.seen w :=
  -- This is one fact of the invariant.
  (timed_inv h).not_early w hover hpoll

/-- **A dropped queue leaves no timed wait pending.** TLA+:
`DroppedHoldsNothing`. Rules out: a task waiting for ever on a wait whose
queue is gone, which no poll will ever look at. -/
theorem dropped_queue_holds_no_wait {owner : Nat} {s : Timed}
    -- `s` is reachable from the start.
    (h : TSteps owner Timed.init s)
    -- The queue was dropped.
    (hgone : s.gone = true)
    -- Any wait.
    (w : Nat) :
    -- Then:
    (s.st w).pending = false :=
  -- This is one fact of the invariant.
  (timed_inv h).gone_empty hgone w

/-- How long the owner may idle before wait `w` is due: what a poll reports
(`Timers::wake_in`, `IoProgress::wake_in`). -/
def wakeIn (s : Timed) (w : Nat) : Nat :=
  -- The time left until the due time, zero when it is due already.
  s.due w - s.now

/-- The clock can move on by any amount. -/
theorem ticks (owner : Nat) (s : Timed) (n : Nat) :
    -- Then:
    TSteps owner s { s with now := s.now + n } := by
  -- By induction on the amount.
  induction n with
  -- By nothing: no step.
  | zero => exact .refl s
  -- By one more: one more tick.
  | succ n ih => exact .tail ih (.tick _)

/-- **An owner that polls again within what its poll reported completes the
wait.** TLA+: `AllDone` in `MC_NonBlocking_Green_TimerLive`. Whatever the
clock reads when the owner holds wait `w`, after the time `wakeIn` says has
passed, the poll finds `w` due and completes it, on the owner's thread. -/
theorem poll_after_wake_in_completes (owner : Nat) (s : Timed) (w : Nat)
    -- The owner holds the wait.
    (hh : s.st w = .held) :
    -- Then:
    ∃ t, TSteps owner s t ∧ t.st w = .over ∧ t.who w = owner := by
  -- The state once the reported time has passed.
  let later : Timed := { s with now := s.now + wakeIn s w }
  -- By then the wait is due.
  have hdue : later.due w ≤ later.now := by
    -- Unfold the names; the rest is arithmetic.
    simp only [later, wakeIn]
    -- The due time is at most now plus what was left of it.
    omega
  -- The clock moves on, then the poll fires the wait.
  refine ⟨_, .tail (ticks owner s (wakeIn s w)) (.fire later w hh hdue), ?_, ?_⟩
  -- The wait is over.
  · simp
  -- And the owner's step did it.
  · simp

/-! ### RED: a poll that does not look at the clock

The bug: the poll completes every wait it holds, due or not. -/

/-- The broken steps: asking, recording, and a fire with no due check. -/
inductive EarlyStep (owner : Nat) : Timed → Timed → Prop
  /-- Some thread asks for wait `w`, due at time `d`. -/
  | ask (s : Timed) (w d : Nat)
      -- Nobody asked for `w` before.
      (hnew : s.st w = .unasked) :
      -- The state after the step:
      EarlyStep owner s { s with st := set s.st w .noted, due := set s.due w d }
  /-- The owner's poll takes the note for `w` in. -/
  | record (s : Timed) (w : Nat)
      -- The note is in the inbox.
      (hn : s.st w = .noted) :
      -- The state after the step:
      EarlyStep owner s { s with st := set s.st w .held }
  /-- The bug: the poll completes `w` whatever the clock reads. -/
  | fireEarly (s : Timed) (w : Nat)
      -- The owner holds the wait.
      (hh : s.st w = .held) :
      -- The state after the step:
      EarlyStep owner s { s with st := set s.st w .over,
                                 fires := set s.fires w (s.fires w + 1),
                                 who := set s.who w owner,
                                 seen := set s.seen w s.now }

/-- Any number of broken steps. -/
inductive EarlySteps (owner : Nat) : Timed → Timed → Prop
  /-- No step at all. -/
  | refl (s : Timed) : EarlySteps owner s s
  /-- Some steps, then one more. -/
  | tail {a b c : Timed} : EarlySteps owner a b → EarlyStep owner b c → EarlySteps owner a c

/-- **RED: without the due check a wait is completed early.** Wait 1 is
asked for at time 0, due at 100; the poll takes it in and completes it at
once, at time 0. TLA+: `MC_NonBlocking_Red_EarlyFire` breaks
`TimerNotEarly`. -/
theorem early_fire_breaks_due :
    -- Some reachable state has a wait a poll completed before its due time.
    ∃ s, EarlySteps 0 Timed.init s ∧ s.st 1 = .over ∧ s.byDrop 1 = false
      ∧ ¬ s.due 1 ≤ s.seen 1 := by
  -- The three steps: ask for wait 1 due at 100, record it, fire it.
  refine ⟨_, .tail (.tail (.tail (.refl _) (.ask _ 1 100 rfl)) (.record _ 1 (by simp)))
    (.fireEarly _ 1 (by simp)), ?_, ?_, ?_⟩
  -- The wait is over.
  · simp
  -- No drop completed it.
  · simp [Timed.init]
  -- It was due at 100 and the poll read the clock at 0.
  · simp [Timed.init]

/-! ### RED: an owner that idles past the due time

The bug is the owner's: it holds a wait and never polls again. Waits are
still asked for and time still passes. -/

/-- The steps left when the owner never polls: asking, and time passing. -/
inductive IdleStep : Timed → Timed → Prop
  /-- Some thread asks for wait `w`, due at time `d`. -/
  | ask (s : Timed) (w d : Nat)
      -- Nobody asked for `w` before.
      (hnew : s.st w = .unasked) :
      -- The state after the step:
      IdleStep s { s with st := set s.st w .noted, due := set s.due w d }
  /-- Time passes. -/
  | tick (s : Timed) :
      -- The state after the step:
      IdleStep s { s with now := s.now + 1 }

/-- Any number of steps without a poll. -/
inductive IdleSteps : Timed → Timed → Prop
  /-- No step at all. -/
  | refl (s : Timed) : IdleSteps s s
  /-- Some steps, then one more. -/
  | tail {a b c : Timed} : IdleSteps a b → IdleStep b c → IdleSteps a c

/-- **RED: with no poll a wait is never completed, however late it gets.**
Nobody is woken for a timed wait, so an owner that ignores what its poll
reported and idles for ever leaves it pending. TLA+:
`MC_NonBlocking_Red_IdlePastDue` breaks `AllDone`. -/
theorem idle_owner_never_completes {s t : Timed} {w : Nat}
    -- The wait is not completed.
    (hw : s.st w ≠ .over)
    -- Any number of steps without a poll.
    (h : IdleSteps s t) :
    -- Then:
    t.st w ≠ .over := by
  -- By induction on the steps.
  induction h with
  -- No step: as assumed.
  | refl => exact hw
  -- Steps, then one more.
  | tail _ hstep ih =>
    -- Look at the last step.
    cases hstep with
    -- A wait was asked for: it is noted, and no other wait changed.
    | ask x d hnew =>
      -- Is it the wait we follow?
      by_cases hxw : w = x
      -- It is: noted is not over.
      · subst hxw
        -- Read the new state.
        simp
      -- Another wait: ours is unchanged.
      · simpa [hxw] using ih
    -- Time passed: no wait changed.
    | tick => exact ih

-- The end of the Regolith.IoQueue names.
end Regolith.IoQueue
