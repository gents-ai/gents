/-!
# Sync: the barging lock with bounded bypass (regolith::sync, D49)

This file backs the TLA+ model `proofs/tla/Sync.tla`, invariants
`MutualExclusion`, `BoundedBypass`, `HandoffExclusive` and `NoLostWakeup`
(configurations `MC_Sync_*`), the exclusive side of
`proofs/tla/SyncRwLock.tla` and the turn semaphore of
`proofs/tla/SyncOnce.tla`. TLC checks the concurrent protocol there, one
machine instruction at a time, for two or three tasks. Here the Mutex is a
sequential state machine over the same steps taken whole (barging fast
path, enqueue, release, four kinds of poll, two kinds of cancel); a
reachable state is the result of any sequence of them, so every
interleaving of those steps is covered. The theorems hold for every number
of requests, every number of steps and every bound `B` of at least 1 (the
code's `MAX_BYPASS`: 16 in the engine, 2 under loom).

## The story, for a reader who has never seen the code

A lock lets one task at a time into a piece of code. regolith's async locks
(src/sync/) let a task take the lock whenever it is free, even when others
are queued for it ("barging"): the alternative, handing the lock to the
longest waiter, leaves it idle until that sleeping task is scheduled.

Tiny example of what barging can do: task A waits. Every time the lock is
released A is woken, but task B, already running, grabs the lock first.
A loses the race again and again. So a waiter counts the races it loses.
The retry that would be its `B`-th loss does two things in one indivisible
step: it finds the lock taken, and it raises a flag that says "a handoff is
owed". While the flag is up nobody may barge, and the release that follows
hands the lock straight to that waiter.

What could go wrong: two holders at once; a waiter that loses for ever; a
newcomer slipping in while a handoff is owed; a waiter asleep beside a free
lock. The theorems below rule each of these out for the abstract lock, for
any number of requests.

## The steps, and the code each one stands for

* `fast`: `Contend::poll`'s first try (`Lock::try_take`, also `try_lock`):
  one compare-and-swap takes a free lock when no handoff is owed, whoever
  waits.
* `enqueue`: the try failed, so the caller queues at the tail with a count
  of 0 (`Lock::enqueue`).
* `release`: a guard is dropped (`RawSemaphore::release`) and the drain
  pass that follows runs (`RawSemaphore::pass`). With no handoff owed the
  lock is free and the head is nudged (woken). With a handoff owed the
  pass takes the lock for the head and grants it.
* `pollHanded`: a waiter that was granted the lock is polled and holds it.
* `pollWin`: a nudged waiter's retry takes the free lock.
* `pollLose`: a nudged waiter's retry finds the lock taken, below the
  bound: the count goes up by one (`Contend::bypass`).
* `pollOwe`: the retry that would be the `B`-th loss (`Lock::take_or_owe`)
  finds the lock taken: the count reaches `B` and the owed flag goes up,
  in the same step.
* `cancelWaiting`, `cancelHanded`: the waiting future is dropped
  (`Contend::cancel`). A nudge it carried passes to the new head; a lock it
  had been granted is released like any other.

Three things are simpler here than in the code, and why that is sound for
these theorems. The release and the drain pass after it are one step: while
a handoff is owed nobody can take what the release freed (theorem
`owed_blocks_barging`), so nothing can happen in between that these laws
could see. The owed flag names its waiter: the code keeps a bit (HANDOFF)
and a count of waiters marked owed; with one permit only the head is ever
nudged, so only the head can be the one. And there is no drain role: who
runs the pass, and how many passes one thread runs, is not in this machine
at all. That last bound (`DrainBounded` in `Sync.tla`: one pass, plus one
for each waiter operation or release with a node queued that lands on it)
is checked in TLA+ only, where the drain is modelled instruction by
instruction.

## What is proved, in plain words

1. `step_inv` and `reachable_inv`: the invariant holds in every reachable
   state.
2. `mutual_exclusion`: at most one request holds the lock, and none while
   it is free. TLA+: `MutualExclusion`.
3. `bounded_bypass`, `lose_below_bound`, `bypass_counts_losses`,
   `owed_at_bound`, `at_bound_owed`: a queued request's count never exceeds
   `B`; a loss happens only below `B`; only a loss changes a request's
   count, by one; the loss that brings the count to `B` raises the owed
   flag in the same step; and a waiter whose count is `B` is owed. So a
   request loses at most `B` races. TLA+: `BoundedBypass`.
4. `release_hands_off_at_bound`: once a waiter is owed, the release that
   follows hands it the lock.
5. `owed_exclusive` and `owed_blocks_barging`: while a handoff is owed the
   lock is not free and nobody competes for it, and no step makes anyone
   but the owed waiter an owner. TLA+: `HandoffExclusive`.
6. `no_stranded_head` and `handed_woken`: while the lock is free, the head
   waiter has been woken; a request handed the lock has been woken.
   TLA+: `NoLostWakeup`.
7. E48, the waiter's node while its future spins: `no_lost_wakeup_spinning`
   (a future asleep with its waker armed never has an unread mark on its node,
   spins included), `mark_spinning_wakes_nobody` (a release that finds the
   waiter spinning calls no waker; the spinner reads the mark itself) and the
   RED case `spin_without_look_strands` (a spin that sleeps without its look
   leaves the waiter asleep beside a mark). TLA+: `NoLostWakeup`,
   `MC_Sync_Red_SpinNoRecheck`.
8. The RED cases, as counterexamples: `unbounded_barging_breaks_bound` and
   `unbounded_barging_leaves_barging_open` (a waiter that never raises the
   owed flag loses more than `B` races, with barging still open) and
   `release_without_wake_strands` (a release that wakes nobody leaves the
   head asleep beside a free lock).

`Regolith/LockTable.lean` builds the pessimistic lock table on this
machine: it uses `M`, `init`, `Step`, `Steps`, the four laws
`mutual_exclusion`, `bounded_bypass`, `no_stranded_head` and
`handed_woken`, the helper `eq_single_of_mem`, and the RED relation
`StepsNoHand` with `unbounded_barging_breaks_bound`. Those keep their names
and their statements here.

How to read the Lean: `def` defines a function or a property, `theorem`
states a fact and its proof follows `:= by`. Lines starting with `--` are
comments. `l.erase x` is `l` without its first `x`. `grind` is Lean's
automatic prover: it is given the facts in scope and must find the
argument itself; the comment above each use says what that argument is.
-/

-- Everything below is named Regolith.Sync.<name>.
namespace Regolith.Sync

/-! ## The state -/

/-- The lock's state. A request is named by a natural number, fresh for
every acquire. -/
structure M where
  /-- The lock is free. -/
  free : Bool
  /-- The handoff-owed flag (the HANDOFF bit), naming the waiter that
  raised it. -/
  owed : Option Nat
  /-- The waiting requests, oldest first. A woken head keeps its place. -/
  queue : List Nat
  /-- Requests that hold the lock and know it. -/
  owners : List Nat
  /-- Requests the lock was handed to, not yet polled: they hold it. -/
  handed : List Nat
  /-- Requests that were nudged or granted and not polled since. -/
  woken : List Nat
  /-- Each request's bypass count: races it lost. -/
  bypass : Nat → Nat
  /-- Ghost: every request that ever entered (fast path or queue). -/
  enq : List Nat

/-- The start: the lock free, nobody waiting, holding or woken. -/
def init : M := ⟨true, none, [], [], [], [], fun _ => 0, []⟩

/-- `f` with `f r` replaced by `v`. -/
def upd (f : Nat → Nat) (r v : Nat) : Nat → Nat := fun x => if x = r then v else f x

/-- The list `l` with every `r` taken out. -/
def without (l : List Nat) (r : Nat) : List Nat := l.filter (fun x => x != r)

/-- Wake the head waiter, unless it is woken already. -/
def wakeHead (s : M) : M :=
  -- Look at the queue.
  match s.queue with
  -- Nobody waits: nobody to wake.
  | [] => s
  -- The head `h` is woken if it is not already.
  | h :: _ => if h ∈ s.woken then s else { s with woken := s.woken ++ [h] }

/-- A release, after the owner's guard is gone, with the drain pass that
follows it. With a handoff owed, the pass takes the lock for the head and
grants it: the lock stays held, the head leaves the queue and is woken.
Otherwise the lock is free and the head is woken to compete. -/
def relFrom (s : M) : M :=
  -- Look at the queue and at the owed flag.
  match s.queue, s.owed with
  -- A head `h` and a handoff owed: the lock goes to the head.
  | h :: t, some _ => { s with handed := s.handed ++ [h], queue := t, woken := s.woken ++ [h] }
  -- Nobody waits, or nothing is owed: free the lock and wake the head, if any.
  | _, _ => wakeHead { s with free := true }

/-- The state after the waiting request `r` leaves the queue: it is in
neither the queue nor the woken list, and a handoff it was owed is owed no
more. -/
def leave (s : M) (r : Nat) : M :=
  -- `r` leaves the queue and the woken list,
  { s with queue := without s.queue r, woken := without s.woken r,
           -- and the owed flag comes down if it was `r`'s.
           owed := if s.owed = some r then none else s.owed }

/-- After a waiting request leaves: if the lock is free and no handoff is
owed, the new head is woken (a nudge the leaver carried is passed on). -/
def wakeIfFree (s : M) : M :=
  -- With the lock free and no handoff owed, wake the head; otherwise change nothing.
  if s.free = true ∧ s.owed = none then wakeHead s else s

/-! ## The steps -/

/-- The design's steps, for bound `B`. -/
inductive Step (B : Nat) : M → M → Prop
  /-- The fast path (try_lock, or lock uncontended): a fresh request `r`
  takes the free lock if no handoff is owed, whoever waits. -/
  | fast (s : M) (r : Nat)
      -- `r` is a fresh request.
      (hr : r ∉ s.enq)
      -- The lock is free.
      (hf : s.free = true)
      -- No handoff is owed.
      (ho : s.owed = none) :
      -- After the step: the lock is held, r is an owner, and r has entered.
      Step B s { s with free := false, owners := r :: s.owners, enq := s.enq ++ [r] }
  /-- A contended lock: the fast path failed, so the fresh request `r`
  queues at the tail with a count of 0. (Its look at its node after
  queueing finds nothing: nobody can have nudged it yet.) -/
  | enqueue (s : M) (r : Nat)
      -- `r` is a fresh request.
      (hr : r ∉ s.enq)
      -- The fast path is shut: the lock is held, or a handoff is owed.
      (hc : s.free = false ∨ s.owed ≠ none) :
      -- After the step: r is at the tail of the queue, has entered, and has a count of 0.
      Step B s { s with queue := s.queue ++ [r], enq := s.enq ++ [r], bypass := upd s.bypass r 0 }
  /-- An owner drops its guard, and the pass that follows runs. -/
  | release (s : M) (r : Nat)
      -- `r` holds the lock.
      (hr : r ∈ s.owners) :
      -- After the step: r is no longer an owner, and the release rule ran.
      Step B s (relFrom { s with owners := s.owners.erase r })
  /-- A woken request that was handed the lock takes it; the owed flag
  comes down. -/
  | pollHanded (s : M) (h : Nat)
      -- `h` was woken ...
      (hw : h ∈ s.woken)
      -- ... and handed the lock.
      (hh : h ∈ s.handed) :
      -- After the step: h is an owner, no longer "handed",
      Step B s { s with owners := h :: s.owners, handed := s.handed.erase h,
                        -- no longer "woken", and no handoff is owed.
                        woken := without s.woken h, owed := none }
  /-- A woken request wins the race: the lock is free and no handoff owed. -/
  | pollWin (s : M) (h : Nat)
      -- `h` was woken ...
      (hw : h ∈ s.woken)
      -- ... and not handed the lock.
      (hh : h ∉ s.handed)
      -- The lock is free.
      (hf : s.free = true)
      -- No handoff is owed.
      (ho : s.owed = none) :
      -- After the step: the lock is held, h is an owner, h has left the queue,
      Step B s { s with free := false, owners := h :: s.owners, queue := without s.queue h,
                        -- and h is no longer "woken".
                        woken := without s.woken h }
  /-- A woken request loses the race (someone barged in), below the bound:
  it keeps its place, its count goes up by one, and it sleeps again. -/
  | pollLose (s : M) (h : Nat)
      -- `h` was woken ...
      (hw : h ∈ s.woken)
      -- ... and not handed the lock.
      (hh : h ∉ s.handed)
      -- The lock is taken.
      (hf : s.free = false)
      -- No handoff is owed.
      (ho : s.owed = none)
      -- This loss leaves its count below the bound.
      (hb : s.bypass h + 1 < B) :
      -- After the step: h's count is one more, and h is no longer "woken".
      Step B s { s with bypass := upd s.bypass h (s.bypass h + 1), woken := without s.woken h }
  /-- A woken request loses the race for the `B`-th time (`take_or_owe`):
  its count reaches the bound and, in the same step, the owed flag goes up
  for it. It keeps its place and sleeps again. -/
  | pollOwe (s : M) (h : Nat)
      -- `h` was woken ...
      (hw : h ∈ s.woken)
      -- ... and not handed the lock.
      (hh : h ∉ s.handed)
      -- The lock is taken.
      (hf : s.free = false)
      -- No handoff is owed yet.
      (ho : s.owed = none)
      -- This loss brings its count to the bound.
      (hb : s.bypass h + 1 = B) :
      -- After the step: h's count is one more, h is no longer "woken",
      Step B s { s with bypass := upd s.bypass h (s.bypass h + 1), woken := without s.woken h,
                        -- and a handoff is owed to h.
                        owed := some h }
  /-- A waiting request's future is dropped: it leaves, and a nudge it
  carried passes to the new head. -/
  | cancelWaiting (s : M) (r : Nat)
      -- `r` waits.
      (hr : r ∈ s.queue) :
      -- After the step: r has left, and the new head is woken if the lock is free.
      Step B s (wakeIfFree (leave s r))
  /-- A request handed the lock is dropped before it was polled: it gives
  the lock back, which is a release like any other. -/
  | cancelHanded (s : M) (r : Nat)
      -- `r` was handed the lock.
      (hr : r ∈ s.handed) :
      -- After the step: r is no longer "handed" or "woken", nothing is owed,
      Step B s (relFrom { s with handed := s.handed.erase r, woken := without s.woken r,
                                 -- and the release rule ran.
                                 owed := none })

/-- Any number of steps, one after another. -/
inductive Steps (B : Nat) : M → M → Prop
  /-- No step at all. -/
  | refl (s : M) : Steps B s s
  /-- Some steps, then one more. -/
  | tail {s t u : M} : Steps B s t → Step B t u → Steps B s u

/-! ## The invariant -/

/-- The protocol's invariant, for bound `B`. -/
structure Inv (B : Nat) (s : M) : Prop where
  /-- The lock is free, or held by exactly one request. -/
  excl : (s.owners ++ s.handed).length + (if s.free = true then 1 else 0) = 1
  /-- A request handed the lock is the one the handoff is owed to. -/
  handed_owed : ∀ x ∈ s.handed, s.owed = some x
  /-- The waiter a handoff is owed to was handed the lock, or is the head
  with a count of exactly the bound. -/
  owed_ok : ∀ w, s.owed = some w → w ∈ s.handed ∨ ∃ t, s.queue = w :: t ∧ s.bypass w = B
  /-- While a handoff is owed the lock is not free. -/
  owed_held : ∀ w, s.owed = some w → s.free = false
  /-- While a handoff is owed, only the request handed the lock is woken:
  nobody competes. -/
  owed_quiet : ∀ w, s.owed = some w → ∀ x ∈ s.woken, x ∈ s.handed
  /-- A woken request was handed the lock, or is the head. -/
  woken_ok : ∀ x ∈ s.woken, x ∈ s.handed ∨ ∃ t, s.queue = x :: t
  /-- A request handed the lock has been woken. -/
  handed_woken : ∀ x ∈ s.handed, x ∈ s.woken
  /-- Every waiting request's count is below the bound, unless the handoff
  is owed to it. -/
  below : ∀ x ∈ s.queue, s.bypass x < B ∨ s.owed = some x
  /-- While the lock is free, the head has been woken. -/
  stranded : ∀ h t, s.queue = h :: t → s.free = true → h ∈ s.woken
  /-- Every request in the lock's lists entered. -/
  all_enq : ∀ x, x ∈ s.queue ∨ x ∈ s.owners ∨ x ∈ s.handed ∨ x ∈ s.woken → x ∈ s.enq
  /-- No request waits twice. -/
  queue_nodup : s.queue.Nodup
  /-- A waiting request holds nothing. -/
  queue_fresh : ∀ x ∈ s.queue, x ∉ s.owners ∧ x ∉ s.handed

/-! ## Helper facts about lists -/

/-- A list of length one that holds `x` is `[x]`. (`Regolith/LockTable.lean`
uses it: the one holder of a key is the pending committer.) -/
theorem eq_single_of_mem {l : List Nat} {x : Nat}
    -- `l` has one element ...
    (hl : l.length = 1)
    -- ... and `x` is in it.
    (hx : x ∈ l) :
    -- The statement: l is the one-element list [x].
    l = [x] := by
  -- Look at l, knowing its length is 1.
  match l, hl with
  -- It is [a] for some a; x is in it, so x is a; replace.
  | [a], _ => simp only [List.mem_singleton] at hx; rw [hx]

/-- `x` is in "`l` without `r`" exactly when it is in `l` and is not `r`. -/
theorem mem_without {l : List Nat} {r x : Nat} :
    -- The statement.
    x ∈ without l r ↔ x ∈ l ∧ x ≠ r := by
  -- Unfold "without": it keeps the elements that differ from r.
  simp [without, List.mem_filter]

/-- Taking `r` out of a list whose first element is not `r` keeps that
first element first. -/
theorem without_cons_ne {x r : Nat} {t : List Nat}
    -- The first element is not `r`.
    (h : x ≠ r) :
    -- The statement: x stays in front of what is left of the rest.
    without (x :: t) r = x :: without t r := by
  -- Unfold "without": x passes the test "differs from r".
  simp [without, h]

/-- A list with no repeats still has none after `r` is taken out. -/
theorem nodup_without {l : List Nat} (r : Nat)
    -- `l` has no repeats.
    (h : l.Nodup) :
    -- The statement: neither has l without r.
    (without l r).Nodup :=
  -- Keeping some elements of a list without repeats gives a list without repeats.
  h.filter _

/-! ## What the helpers change -/

/-- Waking the head changes only the woken list: here is every other
field, unchanged. -/
theorem wakeHead_fields (u : M) :
    -- The statement: after waking the head, every field but the woken list is what it was.
    (wakeHead u).free = u.free ∧ (wakeHead u).owed = u.owed ∧ (wakeHead u).queue = u.queue ∧
    (wakeHead u).owners = u.owners ∧ (wakeHead u).handed = u.handed ∧
    (wakeHead u).bypass = u.bypass ∧ (wakeHead u).enq = u.enq := by
  -- Each branch keeps the fields, or replaces only `woken`.
  unfold wakeHead
  -- Split on the queue.
  split
  -- Nobody waits: nothing changed at all.
  · exact ⟨rfl, rfl, rfl, rfl, rfl, rfl, rfl⟩
  -- A head: whether it was already woken or not, only the woken list can differ.
  · split <;> exact ⟨rfl, rfl, rfl, rfl, rfl, rfl, rfl⟩

/-- After waking the head, the woken requests are the ones woken before
and the head. -/
theorem wakeHead_woken (u : M) (x : Nat) :
    -- The statement: x is woken afterwards exactly when it was woken before or is the head.
    x ∈ (wakeHead u).woken ↔ x ∈ u.woken ∨ ∃ t, u.queue = x :: t := by
  -- Unfold "wake the head".
  unfold wakeHead
  -- Split on the queue.
  split
  -- Nobody waits: nothing changed, and nobody is the head.
  · next hq => simp [hq]
  -- A head h, with t behind it.
  · next h t hq =>
    -- Was the head woken already?
    split
    -- Yes: the woken list is unchanged.
    · next hm =>
      -- Show both directions.
      constructor
      -- Woken afterwards means woken before.
      · exact Or.inl
      -- Woken before, or the head:
      · rintro (hx | ⟨t', ht'⟩)
        -- woken before: still woken.
        · exact hx
        -- the head: that is h, which was woken already.
        · rw [hq] at ht'; cases ht'; exact hm
    -- No: the head joins the woken list. Spell out membership in the longer list.
    · simp only [List.mem_append, List.mem_singleton, hq, List.cons.injEq]
      -- Show both directions.
      constructor
      -- Woken afterwards: woken before, or it is h.
      · rintro (hx | rfl)
        -- Woken before.
        · exact Or.inl hx
        -- It is h, the head.
        · exact Or.inr ⟨t, rfl, rfl⟩
      -- Woken before, or the head:
      · rintro (hx | ⟨t', rfl, -⟩)
        -- woken before.
        · exact Or.inl hx
        -- the head, which was just woken.
        · exact Or.inr rfl

/-- Waking the head changes no count. -/
theorem wakeHead_bypass (u : M) : (wakeHead u).bypass = u.bypass :=
  -- That is one of the unchanged fields.
  (wakeHead_fields u).2.2.2.2.2.1

/-- Waking the head changes no owner. -/
theorem wakeHead_owners (u : M) : (wakeHead u).owners = u.owners :=
  -- That is one of the unchanged fields.
  (wakeHead_fields u).2.2.2.1

/-- With a head and a handoff owed, the release rule hands the lock to the
head. -/
theorem relFrom_handoff {s : M} {h w : Nat} {t : List Nat}
    -- The head is `h`.
    (hq : s.queue = h :: t)
    -- A handoff is owed.
    (ho : s.owed = some w) :
    -- The statement: the head is handed the lock, leaves the queue and is woken.
    relFrom s = { s with handed := s.handed ++ [h], queue := t, woken := s.woken ++ [h] } := by
  -- Unfold the release rule: with that queue and that flag it takes the first branch.
  simp [relFrom, hq, ho]

/-- With no handoff owed, the release rule frees the lock and wakes the
head. -/
theorem relFrom_free {s : M}
    -- No handoff is owed.
    (ho : s.owed = none) :
    -- The statement: the lock is free and the head is woken.
    relFrom s = wakeHead { s with free := true } := by
  -- Unfold the release rule.
  unfold relFrom
  -- Split on its two branches.
  split
  -- The handoff branch needs a handoff owed: there is none.
  · next heq => rw [ho] at heq; cases heq
  -- The other branch is the claim.
  · rfl

/-- The release rule changes no count. -/
theorem relFrom_bypass (u : M) : (relFrom u).bypass = u.bypass := by
  -- Unfold the release rule.
  unfold relFrom
  -- Split on its two branches.
  split
  -- The handoff branch does not touch the counts.
  · rfl
  -- The other branch only wakes the head.
  · exact wakeHead_bypass _

/-- The release rule makes nobody an owner. -/
theorem relFrom_owners (u : M) : (relFrom u).owners = u.owners := by
  -- Unfold the release rule.
  unfold relFrom
  -- Split on its two branches.
  split
  -- The handoff branch does not touch the owners.
  · rfl
  -- The other branch only wakes the head.
  · exact wakeHead_owners _

/-- With the lock free and nothing owed, `wakeIfFree` wakes the head. -/
theorem wakeIfFree_free {u : M}
    -- The lock is free ...
    (hf : u.free = true)
    -- ... and nothing is owed.
    (ho : u.owed = none) :
    -- The statement: here "wake if free" is "wake the head".
    wakeIfFree u = wakeHead u := by
  -- Unfold "wake if free".
  unfold wakeIfFree
  -- Its condition is true.
  simp [hf, ho]

/-- With the lock held, `wakeIfFree` changes nothing. -/
theorem wakeIfFree_held {u : M}
    -- The lock is held.
    (hf : u.free = false) :
    -- The statement: here "wake if free" changes nothing.
    wakeIfFree u = u := by
  -- Unfold "wake if free".
  unfold wakeIfFree
  -- Its condition is false.
  simp [hf]

/-- `wakeIfFree` changes no count. -/
theorem wakeIfFree_bypass (u : M) : (wakeIfFree u).bypass = u.bypass := by
  -- Unfold "wake if free".
  unfold wakeIfFree
  -- Split on its condition.
  split
  -- It wakes the head, which changes no count.
  · exact wakeHead_bypass _
  -- It does nothing.
  · rfl

/-- `wakeIfFree` makes nobody an owner. -/
theorem wakeIfFree_owners (u : M) : (wakeIfFree u).owners = u.owners := by
  -- Unfold "wake if free".
  unfold wakeIfFree
  -- Split on its condition.
  split
  -- It wakes the head, which changes no owner.
  · exact wakeHead_owners _
  -- It does nothing.
  · rfl

/-! ## What the invariant says about who holds the lock -/

/-- Under the invariant, while the lock is free nobody holds it. -/
theorem Inv.free_empty {B : Nat} {s : M}
    -- The invariant holds.
    (hs : Inv B s)
    -- The lock is free.
    (hf : s.free = true) :
    -- The statement: there is no owner, and nobody was handed the lock.
    s.owners = [] ∧ s.handed = [] := by
  -- Start from "the holders plus one-if-free make exactly 1".
  have hx := hs.excl
  -- The lock is free.
  rw [hf] at hx
  -- So the count says: holders plus 1 is 1.
  simp only [ite_true] at hx
  -- So there are no holders.
  have hl : (s.owners ++ s.handed).length = 0 := by omega
  -- An empty joined list means both parts are empty.
  rw [List.length_eq_zero_iff, List.append_eq_nil_iff] at hl
  -- That is the claim.
  exact hl

/-- Under the invariant, if somebody holds the lock (as an owner or by
handoff), the lock is not free and there is exactly one holder. -/
theorem Inv.holder {B : Nat} {s : M}
    -- The invariant holds.
    (hs : Inv B s) {x : Nat}
    -- `x` holds the lock.
    (hx : x ∈ s.owners ++ s.handed) :
    -- The statement: the lock is not free, and owners and handed requests together number 1.
    s.free = false ∧ s.owners.length + s.handed.length = 1 := by
  -- Start from "the holders plus one-if-free make exactly 1".
  have hx' := hs.excl
  -- `x` is among the holders, so there is at least one.
  have hpos : 0 < (s.owners ++ s.handed).length := List.length_pos_of_mem hx
  -- The length of the joined list is the sum of the two lengths.
  rw [List.length_append] at hx' hpos
  -- Split on whether the lock is free.
  cases hf : s.free
  -- Not free: the count says the holders number exactly 1.
  · rw [hf] at hx'; simp only [Bool.false_eq_true, ↓reduceIte, Nat.add_zero] at hx'; exact ⟨rfl, hx'⟩
  -- Free: at least one holder plus one for "free" is more than 1. Arithmetic says no.
  · rw [hf] at hx'; simp only [↓reduceIte] at hx'; omega

/-- Under the invariant, when the fast path is shut the lock is not free:
a handoff is only ever owed on a lock that is held. -/
theorem Inv.shut_held {B : Nat} {s : M}
    -- The invariant holds.
    (hs : Inv B s)
    -- The fast path is shut.
    (hc : s.free = false ∨ s.owed ≠ none) :
    -- The statement: the lock is not free.
    s.free = false := by
  -- Two ways the fast path is shut.
  rcases hc with h | h
  -- The lock is held: that is the claim.
  · exact h
  -- A handoff is owed. Split on the flag.
  · cases ho : s.owed with
    -- No handoff owed: against "a handoff is owed".
    | none => exact absurd ho h
    -- Owed to w: the invariant says the lock is then not free.
    | some w => exact hs.owed_held w ho

/-- Under the invariant, a waiting request is below the bound, or the
handoff is owed to it and its count is exactly the bound. -/
theorem Inv.at_bound {B : Nat} {s : M}
    -- The invariant holds.
    (hs : Inv B s) {x : Nat}
    -- `x` waits.
    (hx : x ∈ s.queue) :
    -- The statement: below the bound, or owed with a count of B.
    s.bypass x < B ∨ (s.owed = some x ∧ s.bypass x = B) := by
  -- A waiter is below the bound, or owed.
  rcases hs.below x hx with h | h
  -- Below the bound: the first alternative.
  · exact Or.inl h
  -- Owed: then it was handed the lock, or is the head with a count of B.
  · rcases hs.owed_ok x h with h' | ⟨t, -, hb⟩
    -- Handed the lock: but a waiter holds nothing.
    · exact absurd h' (hs.queue_fresh x hx).2
    -- The head with a count of B: the second alternative.
    · exact Or.inr ⟨h, hb⟩

/-- Under the invariant, a woken request that was not handed the lock
waits, no handoff is owed, and its count is below the bound. -/
theorem Inv.woken_waits {B : Nat} {s : M}
    -- The invariant holds.
    (hs : Inv B s) {x : Nat}
    -- `x` was woken ...
    (hw : x ∈ s.woken)
    -- ... and not handed the lock.
    (hh : x ∉ s.handed) :
    -- The statement: x waits, nothing is owed, and x's count is below B.
    x ∈ s.queue ∧ s.owed = none ∧ s.bypass x < B := by
  -- A woken request was handed the lock, or is the head.
  rcases hs.woken_ok x hw with h | ⟨t, ht⟩
  -- Handed: but x was not.
  · exact absurd h hh
  -- The head: so it waits.
  · have hq : x ∈ s.queue := by rw [ht]; simp
    -- Nothing is owed:
    have ho : s.owed = none := by
      -- split on the flag.
      cases ho : s.owed with
      -- None owed: that is the claim.
      | none => rfl
      -- Owed to w: then every woken request was handed the lock, but x was not.
      | some w => exact absurd (hs.owed_quiet w ho x hw) hh
    -- Two of the three claims; "below the bound" is left.
    refine ⟨hq, ho, ?_⟩
    -- A waiter is below the bound, or owed.
    rcases hs.below x hq with h | h
    -- Below the bound: that is the claim.
    · exact h
    -- Owed: but nothing is owed.
    · rw [ho] at h; cases h

/-! ## A free lock whose head is woken -/

/-- **Waking the head of a lock that is held by no handoff.** Take a state
in which nothing is owed, nobody was handed the lock, every woken request
is the head, and every waiter is below the bound. After waking the head it
satisfies the invariant. Three steps end this way: a release with nothing
owed, a waiter leaving a free lock, and a handed request giving the lock
back. -/
theorem inv_wake {B : Nat} {u : M}
    -- Nothing is owed.
    (ho : u.owed = none)
    -- The lock is free, or held by exactly one request.
    (excl : (u.owners ++ u.handed).length + (if u.free = true then 1 else 0) = 1)
    -- Nobody was handed the lock.
    (hh : u.handed = [])
    -- Every woken request is the head.
    (woken_ok : ∀ x ∈ u.woken, ∃ t, u.queue = x :: t)
    -- Every waiter is below the bound.
    (below : ∀ x ∈ u.queue, u.bypass x < B)
    -- Every request in the lists entered.
    (all_enq : ∀ x, x ∈ u.queue ∨ x ∈ u.owners ∨ x ∈ u.woken → x ∈ u.enq)
    -- No request waits twice.
    (queue_nodup : u.queue.Nodup)
    -- A waiting request is no owner.
    (queue_fresh : ∀ x ∈ u.queue, x ∉ u.owners) :
    -- The statement: after waking the head, the invariant holds.
    Inv B (wakeHead u) := by
  -- Waking the head changes only the woken list. Name the unchanged fields.
  obtain ⟨hf, ho', hq, hown, hhand, hb, he⟩ := wakeHead_fields u
  -- And the woken requests afterwards are the old ones and the head.
  have hw := wakeHead_woken u
  -- Twelve facts to show, one after another.
  constructor
  -- One holder or free: owners, handed and "free" are unchanged.
  · rw [hown, hhand, hf]; exact excl
  -- Handed means owed: nobody was handed the lock.
  · rw [hhand, hh]; simp
  -- The owed waiter is in its place: nothing is owed.
  · rw [ho', ho]; simp
  -- Owed means held: nothing is owed.
  · rw [ho', ho]; simp
  -- Owed means nobody competes: nothing is owed.
  · rw [ho', ho]; simp
  -- A woken request is the head. Take a woken request x.
  · intro x hx
    -- The queue is unchanged.
    rw [hq]
    -- It was woken before, or is the head.
    rcases (hw x).1 hx with h | h
    -- Woken before: it was the head.
    · exact Or.inr (woken_ok x h)
    -- The head.
    · exact Or.inr h
  -- A handed request is woken: nobody was handed the lock.
  · rw [hhand, hh]; simp
  -- Every waiter is below the bound. Take a waiter x.
  · intro x hx
    -- The queue and the counts are unchanged.
    rw [hq] at hx; rw [hb]
    -- It was below the bound.
    exact Or.inl (below x hx)
  -- The head is woken: take the head h.
  · intro h t hqq _
    -- The queue is unchanged.
    rw [hq] at hqq
    -- The head is among the woken requests afterwards.
    exact (hw h).2 (Or.inr ⟨t, hqq⟩)
  -- Every request in the lists entered. Take such a request x.
  · intro x hx
    -- The queue, the owners and the entry list are unchanged; nobody is handed.
    rw [hq, hown, hhand, hh] at hx; rw [he]
    -- Which list is x in?
    rcases hx with h | h | h | h
    -- The queue: it entered.
    · exact all_enq x (Or.inl h)
    -- The owners: it entered.
    · exact all_enq x (Or.inr (Or.inl h))
    -- The handed list: that list is empty.
    · simp at h
    -- The woken list: x was woken before, or is the head.
    · rcases (hw x).1 h with h | ⟨t, ht⟩
      -- Woken before: it entered.
      · exact all_enq x (Or.inr (Or.inr h))
      -- The head: it is in the queue, so it entered.
      · exact all_enq x (Or.inl (by rw [ht]; simp))
  -- No request waits twice: the queue is unchanged.
  · rw [hq]; exact queue_nodup
  -- A waiter holds nothing. Take a waiter x.
  · intro x hx
    -- The queue and the owners are unchanged; nobody is handed.
    rw [hq] at hx; rw [hown, hhand, hh]
    -- It is no owner, and the handed list is empty.
    exact ⟨queue_fresh x hx, by simp⟩

/-! ## Every step keeps the invariant -/

/-- The start satisfies the invariant. -/
theorem init_inv (B : Nat) : Inv B init := by
  -- Twelve facts about a free lock with every list empty: each one computes.
  constructor <;> simp [init]

/-- The fast path keeps the invariant. -/
theorem step_inv_fast {B : Nat} {s : M}
    -- The invariant holds before.
    (hs : Inv B s) (r : Nat)
    -- `r` is fresh.
    (hr : r ∉ s.enq)
    -- The lock is free.
    (hf : s.free = true)
    -- No handoff is owed.
    (ho : s.owed = none) :
    -- The statement: the invariant holds once r holds the lock.
    Inv B { s with free := false, owners := r :: s.owners, enq := s.enq ++ [r] } := by
  -- The lock was free, so nobody held it.
  obtain ⟨hown, hhand⟩ := hs.free_empty hf
  -- Twelve facts. The holders are now exactly r. Nothing is owed, so the three facts about an
  -- owed handoff ask nothing. The queue, the woken list and the counts are untouched. The lock
  -- is not free, so "the head of a free lock is woken" asks nothing. r is fresh, so it is no
  -- waiter, and it has now entered.
  constructor <;> grind [Inv]

/-- Joining the queue keeps the invariant. -/
theorem step_inv_enqueue {B : Nat}
    -- The bound is at least 1.
    (hB : 0 < B) {s : M}
    -- The invariant holds before.
    (hs : Inv B s) (r : Nat)
    -- `r` is fresh.
    (hr : r ∉ s.enq)
    -- The fast path is shut.
    (hc : s.free = false ∨ s.owed ≠ none) :
    -- The statement: the invariant holds once r is at the tail with a count of 0.
    Inv B { s with queue := s.queue ++ [r], enq := s.enq ++ [r], bypass := upd s.bypass r 0 } := by
  -- The lock is not free.
  have hf := hs.shut_held hc
  -- Twelve facts. The holders, the owed flag and the woken list are untouched. A head stays the
  -- head when r joins at the tail, and its count is untouched because r is fresh, so it is not
  -- that head. r's own count is 0, below a bound of at least 1. The lock is not free, so "the
  -- head of a free lock is woken" asks nothing. r is fresh, so it is not in the queue already
  -- and holds nothing.
  constructor <;> grind [Inv, upd]

/-- A release keeps the invariant. -/
theorem step_inv_release {B : Nat} {s : M}
    -- The invariant holds before.
    (hs : Inv B s) (r : Nat)
    -- `r` holds the lock.
    (hr : r ∈ s.owners) :
    -- The statement: the invariant holds after r's guard is gone and the release rule ran.
    Inv B (relFrom { s with owners := s.owners.erase r }) := by
  -- r holds the lock, so the lock is not free and there is exactly one holder.
  obtain ⟨hf, hlen⟩ := hs.holder (List.mem_append.2 (Or.inl hr))
  -- Taking r out of the owners makes that list one shorter.
  have hl := List.length_erase_of_mem hr
  -- There is at least one owner, r.
  have hpos : 0 < s.owners.length := List.length_pos_of_mem hr
  -- So without r there is no owner,
  have he : s.owners.erase r = [] := List.length_eq_zero_iff.1 (by omega)
  -- and nobody was handed the lock.
  have hhand : s.handed = [] := List.length_eq_zero_iff.1 (by omega)
  -- A handoff is owed, or not.
  have hcase : s.owed = none ∨ ∃ w, s.owed = some w := by cases s.owed <;> simp
  -- Take the two cases in turn.
  rcases hcase with ho | ⟨w, ho⟩
  -- Nothing owed: the lock is freed and the head woken.
  ·
    -- That is what the release rule does here.
    rw [relFrom_free (s := { s with owners := s.owners.erase r }) ho]
    -- The free lock with its head woken: seven conditions to check.
    refine inv_wake (u := { s with owners := s.owners.erase r, free := true }) ho ?_ ?_ ?_ ?_ ?_ ?_ ?_
    -- No holder, and the lock free: the count is 1.
    · simp [he, hhand]
    -- Nobody was handed the lock.
    · exact hhand
    -- A woken request x is the head:
    · intro x hx
      -- it was handed the lock, or is the head.
      rcases hs.woken_ok x hx with h | h
      -- Handed: nobody was.
      · rw [hhand] at h; simp at h
      -- The head.
      · exact h
    -- A waiter x is below the bound:
    · intro x hx
      -- it is below the bound, or owed.
      rcases hs.below x hx with h | h
      -- Below the bound.
      · exact h
      -- Owed: nothing is owed.
      · rw [ho] at h; cases h
    -- A request x in the lists entered:
    · intro x hx
      -- by the invariant, once we say which of the old lists it is in.
      apply hs.all_enq
      -- Which list?
      rcases hx with h | h | h
      -- The queue.
      · exact Or.inl h
      -- The owners without r: that list is empty.
      · rw [he] at h; simp at h
      -- The woken list.
      · exact Or.inr (Or.inr (Or.inr h))
    -- No request waits twice.
    · exact hs.queue_nodup
    -- A waiter is no owner: there is no owner.
    · intro x _
      -- The owners without r are nobody.
      rw [he]; simp
  -- A handoff is owed to w: the lock goes to the head.
  ·
    -- The owed waiter was handed the lock, or is the head with a count of B.
    rcases hs.owed_ok w ho with h | ⟨t, hq, hb⟩
    -- Handed: nobody was.
    · rw [hhand] at h; simp at h
    -- The head: the release rule hands it the lock.
    · rw [relFrom_handoff (s := { s with owners := s.owners.erase r }) hq ho]
      -- The queue has no repeats,
      have hnd := hs.queue_nodup
      -- so w is not among those behind it.
      rw [hq] at hnd
      -- Twelve facts. The one holder is now w, by handoff, and the lock stays held. The handoff
      -- is owed to w, which was handed the lock and woken. Before the step every woken request
      -- had been handed the lock and nobody had, so w is the only woken one. Those behind w keep
      -- their counts, are still not owners, and none of them is w.
      constructor <;> grind [Inv]

/-- Taking a handoff keeps the invariant. -/
theorem step_inv_pollHanded {B : Nat} {s : M}
    -- The invariant holds before.
    (hs : Inv B s) (h : Nat)
    -- `h` was handed the lock.
    (hh : h ∈ s.handed) :
    -- The statement: the invariant holds once h owns the lock and nothing is owed.
    Inv B { s with owners := h :: s.owners, handed := s.handed.erase h,
                   -- (h is no longer woken, and the flag is down)
                   woken := without s.woken h, owed := none } := by
  -- h holds the lock, so the lock is not free and there is exactly one holder.
  obtain ⟨hf, hlen⟩ := hs.holder (List.mem_append.2 (Or.inr hh))
  -- Taking h out of the handed list makes it one shorter.
  have hl := List.length_erase_of_mem hh
  -- At least one request was handed the lock, h.
  have hpos : 0 < s.handed.length := List.length_pos_of_mem hh
  -- So without h nobody is handed the lock,
  have he : s.handed.erase h = [] := List.length_eq_zero_iff.1 (by omega)
  -- and there was no owner.
  have hown : s.owners = [] := List.length_eq_zero_iff.1 (by omega)
  -- Twelve facts. The one holder is now h, as an owner. Nothing is owed and nobody is handed
  -- the lock, so the facts about a handoff ask nothing. The handoff was owed to h, so every
  -- woken request was h: with h polled nobody is woken. A waiter was below the bound unless the
  -- handoff was owed to it, and it was owed to h, which did not wait.
  constructor <;> grind [Inv, mem_without]

/-- Winning the race keeps the invariant. -/
theorem step_inv_pollWin {B : Nat} {s : M}
    -- The invariant holds before.
    (hs : Inv B s) (h : Nat)
    -- `h` was woken ...
    (hw : h ∈ s.woken)
    -- ... and not handed the lock.
    (hh : h ∉ s.handed)
    -- The lock is free.
    (hf : s.free = true)
    -- No handoff is owed.
    (ho : s.owed = none) :
    -- The statement: the invariant holds once h owns the lock and has left the queue.
    Inv B { s with free := false, owners := h :: s.owners, queue := without s.queue h,
                   -- (and h is no longer woken)
                   woken := without s.woken h } := by
  -- The lock was free, so nobody held it.
  obtain ⟨hown, hhand⟩ := hs.free_empty hf
  -- The queue without h still has no repeats.
  have hnd := nodup_without h hs.queue_nodup
  -- Twelve facts. The one holder is now h. Nothing is owed. Every woken request was the head,
  -- which is h: with h polled nobody is woken. The waiters left keep their counts, below the
  -- bound, and none of them is h. The lock is not free, so "the head of a free lock is woken"
  -- asks nothing.
  constructor <;> grind [Inv, mem_without]

/-- Losing a race below the bound keeps the invariant. -/
theorem step_inv_pollLose {B : Nat} {s : M}
    -- The invariant holds before.
    (hs : Inv B s) (h : Nat)
    -- `h` was woken ...
    (hw : h ∈ s.woken)
    -- ... and not handed the lock.
    (hh : h ∉ s.handed)
    -- The lock is taken.
    (hf : s.free = false)
    -- No handoff is owed.
    (ho : s.owed = none)
    -- The loss leaves the count below the bound.
    (hb : s.bypass h + 1 < B) :
    -- The statement: the invariant holds once h's count is one more and h sleeps again.
    Inv B { s with bypass := upd s.bypass h (s.bypass h + 1), woken := without s.woken h } := by
  -- Twelve facts. The holders, the queue and the owed flag are untouched. Nothing is owed. h's
  -- new count is below the bound, and every other count is unchanged. Fewer requests are woken,
  -- and a handed request is not h, so it is still woken. The lock is not free, so "the head of
  -- a free lock is woken" asks nothing.
  constructor <;> grind [Inv, upd, mem_without]

/-- The loss that reaches the bound keeps the invariant. -/
theorem step_inv_pollOwe {B : Nat} {s : M}
    -- The invariant holds before.
    (hs : Inv B s) (h : Nat)
    -- `h` was woken ...
    (hw : h ∈ s.woken)
    -- ... and not handed the lock.
    (hh : h ∉ s.handed)
    -- The lock is taken.
    (hf : s.free = false)
    -- No handoff is owed yet.
    (ho : s.owed = none)
    -- The loss brings the count to the bound.
    (hb : s.bypass h + 1 = B) :
    -- The statement: the invariant holds once h's count is B, h sleeps again,
    Inv B { s with bypass := upd s.bypass h (s.bypass h + 1), woken := without s.woken h,
                   -- and the handoff is owed to h.
                   owed := some h } := by
  -- Twelve facts. The holders and the queue are untouched. Nothing was owed, so nobody had been
  -- handed the lock. h was woken and not handed, so it is the head, and its count is now B: the
  -- owed waiter is in its place. The lock is taken. Every woken request was the head, h: with
  -- h asleep nobody is woken, so nobody competes. Every other waiter keeps a count below the
  -- bound.
  constructor <;> grind [Inv, upd, mem_without]

/-- A waiter leaving the queue keeps the invariant. -/
theorem step_inv_cancelWaiting {B : Nat} {s : M}
    -- The invariant holds before.
    (hs : Inv B s) (r : Nat)
    -- `r` waits.
    (hr : r ∈ s.queue) :
    -- The statement: the invariant holds once r has left and the new head was woken if needed.
    Inv B (wakeIfFree (leave s r)) := by
  -- The queue without r still has no repeats.
  have hnd := nodup_without r hs.queue_nodup
  -- The lock is free, or not.
  rcases Bool.eq_false_or_eq_true s.free with hf | hf
  -- The lock is free.
  ·
    -- Then nothing is owed:
    have ho : s.owed = none := by
      -- split on the flag.
      cases ho : s.owed with
      -- None owed: that is the claim.
      | none => rfl
      -- Owed to w: then the lock would not be free.
      | some w => have := hs.owed_held w ho; rw [hf] at this; cases this
    -- Nobody holds a free lock.
    obtain ⟨hown, hhand⟩ := hs.free_empty hf
    -- Nothing is owed after r leaves either.
    have ho' : (if s.owed = some r then none else s.owed) = none := by rw [ho]; simp
    -- So the new head is woken.
    rw [wakeIfFree_free (u := leave s r) hf ho']
    -- The free lock with its head woken: the conditions left to check.
    refine inv_wake (u := leave s r) ho' ?_ hhand ?_ ?_ ?_ hnd ?_
    -- The holders and "free" are untouched.
    · exact hs.excl
    -- A request x still woken is the head of the queue without r:
    · intro x hx
      -- it was woken before, and is not r.
      obtain ⟨hxw, hne⟩ := mem_without.1 hx
      -- It was handed the lock, or was the head.
      rcases hs.woken_ok x hxw with h | ⟨t, ht⟩
      -- Handed: nobody was.
      · rw [hhand] at h; simp at h
      -- The head, and not r: it is still the head once r is gone.
      · exact ⟨without t r, by show without s.queue r = _; rw [ht]; exact without_cons_ne hne⟩
    -- A waiter x that is left is below the bound:
    · intro x hx
      -- it waited before, so it was below the bound, or owed.
      rcases hs.below x (mem_without.1 hx).1 with h | h
      -- Below the bound.
      · exact h
      -- Owed: nothing was owed.
      · rw [ho] at h; cases h
    -- A request x in the lists entered:
    · intro x hx
      -- by the invariant, once we say which of the old lists it is in.
      apply hs.all_enq
      -- Which list?
      rcases hx with h | h | h
      -- The queue without r: it was in the queue.
      · exact Or.inl (mem_without.1 h).1
      -- The owners.
      · exact Or.inr (Or.inl h)
      -- The woken list without r: it was woken.
      · exact Or.inr (Or.inr (Or.inr (mem_without.1 h).1))
    -- A waiter that is left is no owner: it waited before.
    · intro x hx
      -- A waiter was no owner.
      exact (hs.queue_fresh x (mem_without.1 hx).1).1
  -- The lock is held: nobody is woken, and the state is just "r has left".
  ·
    -- "Wake if free" does nothing.
    rw [wakeIfFree_held (u := leave s r) hf]
    -- The owed waiter is still in its place:
    have hok : ∀ w, (leave s r).owed = some w →
        w ∈ (leave s r).handed ∨ ∃ t, (leave s r).queue = w :: t ∧ (leave s r).bypass w = B := by
      -- take a waiter w that the handoff is owed to after r left.
      intro w hw
      -- It was owed before, and w is not r (r's own handoff was dropped).
      have hw' : s.owed = some w ∧ w ≠ r := by
        -- Unfold what "owed after r left" means.
        simp only [leave] at hw
        -- Was the handoff r's?
        split at hw
        -- Yes: then nothing is owed now, against "owed to w".
        · cases hw
        -- No: the flag is unchanged, and w is not r.
        · next hne => exact ⟨hw, fun e => hne (e ▸ hw)⟩
      -- Before the step w was handed the lock, or was the head with a count of B.
      rcases hs.owed_ok w hw'.1 with h | ⟨t, ht, hb⟩
      -- Handed: it still is.
      · exact Or.inl h
      -- The head, and not r: it is still the head once r is gone, with the same count.
      · exact Or.inr ⟨without t r, by show without s.queue r = _; rw [ht]; exact without_cons_ne hw'.2, hb⟩
    -- A woken request is still handed the lock, or the head:
    have hwok : ∀ x ∈ (leave s r).woken,
        x ∈ (leave s r).handed ∨ ∃ t, (leave s r).queue = x :: t := by
      -- take a request x still woken.
      intro x hx
      -- It was woken before, and is not r.
      obtain ⟨hxw, hne⟩ := mem_without.1 hx
      -- It was handed the lock, or was the head.
      rcases hs.woken_ok x hxw with h | ⟨t, ht⟩
      -- Handed: it still is.
      · exact Or.inl h
      -- The head, and not r: it is still the head once r is gone.
      · exact Or.inr ⟨without t r, by show without s.queue r = _; rw [ht]; exact without_cons_ne hne⟩
    -- Twelve facts. Two were just shown. The holders are untouched, and a handed request is no
    -- waiter, so it is not r: its handoff is still owed and it is still woken. If a handoff is
    -- still owed it was owed before, so the lock is held and nobody competes. A waiter that is
    -- left keeps its count, and if the handoff was owed to it, it still is. The lock is not
    -- free, so "the head of a free lock is woken" asks nothing.
    constructor <;> grind [Inv, leave, mem_without]

/-- A handed request giving the lock back keeps the invariant. -/
theorem step_inv_cancelHanded {B : Nat} {s : M}
    -- The invariant holds before.
    (hs : Inv B s) (r : Nat)
    -- `r` was handed the lock.
    (hr : r ∈ s.handed) :
    -- The statement: the invariant holds once r is gone, nothing is owed,
    Inv B (relFrom { s with handed := s.handed.erase r, woken := without s.woken r,
                            -- and the release rule ran.
                            owed := none }) := by
  -- r holds the lock, so the lock is not free and there is exactly one holder.
  obtain ⟨hf, hlen⟩ := hs.holder (List.mem_append.2 (Or.inr hr))
  -- Taking r out of the handed list makes it one shorter.
  have hl := List.length_erase_of_mem hr
  -- At least one request was handed the lock, r.
  have hpos : 0 < s.handed.length := List.length_pos_of_mem hr
  -- So without r nobody is handed the lock,
  have he : s.handed.erase r = [] := List.length_eq_zero_iff.1 (by omega)
  -- and there is no owner.
  have hown : s.owners = [] := List.length_eq_zero_iff.1 (by omega)
  -- The handoff was owed to r.
  have hor := hs.handed_owed r hr
  -- With nothing owed, the release rule frees the lock and wakes the head.
  rw [relFrom_free rfl]
  -- The free lock with its head woken: seven conditions to check.
  refine inv_wake (u := { s with handed := s.handed.erase r, woken := without s.woken r,
                                 -- (the state the head is woken in)
                                 owed := none, free := true }) rfl ?_ ?_ ?_ ?_ ?_ ?_ ?_
  -- No holder, and the lock free: the count is 1.
  · simp [he, hown]
  -- Nobody is handed the lock.
  · exact he
  -- Nobody but r was woken, so nobody is woken now. Take a request x still woken:
  · intro x hx
    -- it was woken before, and is not r.
    have hx' := mem_without.1 hx
    -- While the handoff was owed to r every woken request had been handed the lock, so the
    -- handoff was owed to x too.
    have := hs.handed_owed x (hs.owed_quiet r hor x hx'.1)
    -- It was owed to r.
    rw [hor] at this
    -- So x is r: against "x is not r".
    exact absurd (Option.some.inj this).symm hx'.2
  -- A waiter x is below the bound:
  · intro x hx
    -- it is below the bound, or the handoff was owed to it.
    rcases hs.below x hx with h | h
    -- Below the bound.
    · exact h
    -- Owed to x: but it was owed to r,
    · rw [hor] at h
      -- so x is r,
      cases h
      -- which was handed the lock and did not wait.
      exact absurd hr (hs.queue_fresh r hx).2
  -- A request x in the lists entered:
  · intro x hx
    -- by the invariant, once we say which of the old lists it is in.
    apply hs.all_enq
    -- Which list?
    rcases hx with h | h | h
    -- The queue.
    · exact Or.inl h
    -- The owners.
    · exact Or.inr (Or.inl h)
    -- The woken list without r: it was woken.
    · exact Or.inr (Or.inr (Or.inr (mem_without.1 h).1))
  -- No request waits twice.
  · exact hs.queue_nodup
  -- A waiter is no owner.
  · intro x hx
    -- That held before, and the owners are untouched.
    exact (hs.queue_fresh x hx).1

/-- **Every step keeps the invariant**, for a bound of at least 1. -/
theorem step_inv {B : Nat}
    -- The bound is at least 1.
    (hB : 0 < B) {s t : M}
    -- The invariant holds before the step.
    (hs : Inv B s)
    -- One step of the design.
    (hstep : Step B s t) :
    -- The statement: the invariant holds after the step.
    Inv B t := by
  -- Split on the kind of step; each kind has its own theorem above.
  cases hstep with
  -- The fast path.
  | fast r hr hf ho => exact step_inv_fast hs r hr hf ho
  -- Joining the queue.
  | enqueue r hr hc => exact step_inv_enqueue hB hs r hr hc
  -- A release.
  | release r hr => exact step_inv_release hs r hr
  -- Taking a handoff.
  | pollHanded h _ hh => exact step_inv_pollHanded hs h hh
  -- Winning the race.
  | pollWin h hw hh hf ho => exact step_inv_pollWin hs h hw hh hf ho
  -- Losing below the bound.
  | pollLose h hw hh hf ho hb => exact step_inv_pollLose hs h hw hh hf ho hb
  -- The loss that reaches the bound.
  | pollOwe h hw hh hf ho hb => exact step_inv_pollOwe hs h hw hh hf ho hb
  -- A waiter leaves.
  | cancelWaiting r hr => exact step_inv_cancelWaiting hs r hr
  -- A handed request gives the lock back.
  | cancelHanded r hr => exact step_inv_cancelHanded hs r hr

/-- Any number of steps keeps the invariant. -/
theorem steps_inv {B : Nat}
    -- The bound is at least 1.
    (hB : 0 < B) {s t : M}
    -- The invariant holds at the start.
    (hs : Inv B s)
    -- Any number of steps.
    (hsteps : Steps B s t) :
    -- The statement: the invariant holds at the end.
    Inv B t := by
  -- Go through the steps one at a time.
  induction hsteps with
  -- No step: nothing changed.
  | refl => exact hs
  -- The steps so far, then one more.
  | tail _ hstep ih => exact step_inv hB ih hstep

/-- **Every reachable state satisfies the invariant.** -/
theorem reachable_inv {B : Nat}
    -- The bound is at least 1.
    (hB : 0 < B) {s : M}
    -- `s` is reachable from the start.
    (h : Steps B init s) :
    -- The statement: the invariant holds in s.
    Inv B s :=
  -- The start satisfies it, and steps keep it.
  steps_inv hB (init_inv B) h

/-! ## The laws -/

/-- **Mutual exclusion.** At most one request holds the lock, and none
while it is free. TLA+: `MutualExclusion`. -/
theorem mutual_exclusion {B : Nat}
    -- The bound is at least 1.
    (hB : 0 < B) {s : M}
    -- `s` is reachable from the start.
    (h : Steps B init s) :
    -- The statement: at most one request holds the lock, and while the lock is free nobody does.
    (s.owners ++ s.handed).length ≤ 1 ∧ (s.free = true → s.owners ++ s.handed = []) := by
  -- The invariant says: holders plus one-if-free make exactly 1.
  have hx := (reachable_inv hB h).excl
  -- First part: whether the lock is free or not, the holders number at most 1. Second part: suppose
  -- the lock is free.
  refine ⟨by split at hx <;> omega, fun hf => ?_⟩
  -- Use "free" in the count.
  rw [hf] at hx
  -- The count says: holders plus 1 is 1.
  simp only [ite_true] at hx
  -- So there are no holders: the list is empty.
  exact List.length_eq_zero_iff.1 (by omega)

/-- **Bounded bypass.** A waiting request's count of lost races never
exceeds the bound `B`. TLA+: `BoundedBypass`. -/
theorem bounded_bypass {B : Nat}
    -- The bound is at least 1.
    (hB : 0 < B) {s : M}
    -- `s` is reachable from the start.
    (h : Steps B init s) {x : Nat}
    -- `x` waits.
    (hx : x ∈ s.queue) :
    -- The statement: x's count is at most B.
    s.bypass x ≤ B := by
  -- A waiter is below the bound, or owed with a count of exactly B: at most B either way.
  rcases (reachable_inv hB h).at_bound hx with h' | ⟨-, h'⟩ <;> omega

/-- **At the bound, owed.** A waiting request whose count is `B` is the one
the handoff is owed to: barging is shut for as long as it waits.
TLA+: `BoundedBypass` (a waiter at Bound has HANDOFF up). -/
theorem at_bound_owed {B : Nat}
    -- The bound is at least 1.
    (hB : 0 < B) {s : M}
    -- `s` is reachable from the start.
    (h : Steps B init s) {x : Nat}
    -- `x` waits ...
    (hx : x ∈ s.queue)
    -- ... with a count of B.
    (hb : s.bypass x = B) :
    -- The statement: the handoff is owed to x.
    s.owed = some x := by
  -- A waiter is below the bound, or owed with a count of B.
  rcases (reachable_inv hB h).at_bound hx with h' | ⟨h', -⟩
  -- Below the bound: but its count is B.
  · omega
  -- Owed: that is the claim.
  · exact h'

/-- **A loss happens only below the bound.** A woken request that was not
handed the lock (the only kind that can lose) has lost fewer than `B`
races; so a loss takes its count to at most `B`. -/
theorem lose_below_bound {B : Nat}
    -- The bound is at least 1.
    (hB : 0 < B) {s : M}
    -- `s` is reachable from the start.
    (h : Steps B init s) {x : Nat}
    -- `x` was woken ...
    (hw : x ∈ s.woken)
    -- ... and not handed the lock.
    (hh : x ∉ s.handed) :
    -- The statement: x's count is below B.
    s.bypass x < B :=
  -- A woken request that was not handed the lock waits with a count below B.
  ((reachable_inv hB h).woken_waits hw hh).2.2

/-- **The count counts losses.** A step that changes the count of a request
that has entered adds one to it, and the request was woken, was not handed
the lock, and found the lock taken: it lost a race. With `lose_below_bound`
and `bounded_bypass`, a request loses at most `B` races in one acquire. -/
theorem bypass_counts_losses {B : Nat} {s t : M}
    -- One step.
    (hstep : Step B s t) {x : Nat}
    -- `x` has entered.
    (hx : x ∈ s.enq)
    -- Its count changed.
    (hne : t.bypass x ≠ s.bypass x) :
    -- The statement: the count went up by exactly one, x had been woken and not handed the lock,
    -- and the lock was taken.
    t.bypass x = s.bypass x + 1 ∧ x ∈ s.woken ∧ x ∉ s.handed ∧ s.free = false := by
  -- Split on the kind of step.
  cases hstep with
  -- The fast path changes no count: against "the count changed".
  | fast => exact absurd rfl hne
  -- A fresh request r joins the queue.
  | enqueue r hr _ =>
    -- `r` is fresh, so not `x`, which entered before.
    have : x ≠ r := fun e => hr (e ▸ hx)
    -- So x's count is untouched: against "the count changed".
    simp [upd, this] at hne
  -- A release changes no count.
  | release r _ => rw [relFrom_bypass] at hne; exact absurd rfl hne
  -- Taking a handoff changes no count.
  | pollHanded => exact absurd rfl hne
  -- Winning the race changes no count.
  | pollWin => exact absurd rfl hne
  -- A request h loses below the bound.
  | pollLose h hw hh hf _ _ =>
    -- Is x that request?
    by_cases e : x = h
    -- Yes: its count went up by one, and it was woken, not handed, with the lock taken.
    · subst e; exact ⟨by simp [upd], hw, hh, hf⟩
    -- No: x's count is untouched, against "the count changed".
    · simp [upd, e] at hne
  -- A request h loses for the B-th time.
  | pollOwe h hw hh hf _ _ =>
    -- Is x that request?
    by_cases e : x = h
    -- Yes: its count went up by one, and it was woken, not handed, with the lock taken.
    · subst e; exact ⟨by simp [upd], hw, hh, hf⟩
    -- No: x's count is untouched, against "the count changed".
    · simp [upd, e] at hne
  -- A waiter leaving changes no count.
  | cancelWaiting r _ => rw [wakeIfFree_bypass] at hne; exact absurd rfl hne
  -- A handed request giving the lock back changes no count.
  | cancelHanded r _ => rw [relFrom_bypass] at hne; exact absurd rfl hne

/-- **The loss that reaches the bound raises the owed flag, in the same
step.** If one step takes a request's count from below `B` to `B`, the
handoff is owed to that request right after that step: there is no moment
at which a waiter has lost `B` races and barging is still open.
TLA+: `BoundedBypass`; the code: `Lock::take_or_owe`. -/
theorem owed_at_bound {B : Nat} {s t : M}
    -- One step.
    (hstep : Step B s t) {x : Nat}
    -- `x` has entered.
    (hx : x ∈ s.enq)
    -- Its count was below the bound ...
    (hlt : s.bypass x < B)
    -- ... and is the bound after the step.
    (hb : t.bypass x = B) :
    -- The statement: after the step the handoff is owed to x.
    t.owed = some x := by
  -- The count changed.
  have hne : t.bypass x ≠ s.bypass x := by omega
  -- Split on the kind of step.
  cases hstep with
  -- The fast path changes no count: against "the count changed".
  | fast => exact absurd rfl hne
  -- A fresh request r joins the queue.
  | enqueue r hr _ =>
    -- `r` is fresh, so not `x`, which entered before.
    have : x ≠ r := fun e => hr (e ▸ hx)
    -- So x's count is untouched: against "the count changed".
    simp [upd, this] at hne
  -- A release changes no count.
  | release r _ => rw [relFrom_bypass] at hne; exact absurd rfl hne
  -- Taking a handoff changes no count.
  | pollHanded => exact absurd rfl hne
  -- Winning the race changes no count.
  | pollWin => exact absurd rfl hne
  -- A request h loses below the bound.
  | pollLose h _ _ _ _ hlose =>
    -- Is x that request?
    by_cases e : x = h
    -- Yes: but then its new count is still below B, not B.
    · subst e; simp [upd] at hb; omega
    -- No: x's count is untouched, against "the count changed".
    · simp [upd, e] at hne
  -- A request h loses for the B-th time.
  | pollOwe h _ _ _ _ _ =>
    -- Is x that request?
    by_cases e : x = h
    -- Yes: that step sets the owed flag to x.
    · subst e; rfl
    -- No: x's count is untouched, against "the count changed".
    · simp [upd, e] at hne
  -- A waiter leaving changes no count.
  | cancelWaiting r _ => rw [wakeIfFree_bypass] at hne; exact absurd rfl hne
  -- A handed request giving the lock back changes no count.
  | cancelHanded r _ => rw [relFrom_bypass] at hne; exact absurd rfl hne

/-- **Once a waiter is owed, the next release hands it the lock.** If a
handoff is owed to `w` and the owner releases, `w` is handed the lock: it
holds it, it is woken, the lock is not free, and the flag stays up until
`w` takes it. -/
theorem release_hands_off_at_bound {B : Nat}
    -- The bound is at least 1.
    (hB : 0 < B) {s : M}
    -- `s` is reachable from the start.
    (h : Steps B init s) {w r : Nat}
    -- A handoff is owed to `w`.
    (hw : s.owed = some w)
    -- `r` holds the lock.
    (hr : r ∈ s.owners) :
    -- Name the state after the release u.
    let u := relFrom { s with owners := s.owners.erase r }
    -- The statement: in u, w was handed the lock and woken, the lock is not free, and the handoff
    -- is still owed to w.
    w ∈ u.handed ∧ w ∈ u.woken ∧ u.free = false ∧ u.owed = some w := by
  -- The invariant holds before the release.
  have hs := reachable_inv hB h
  -- r holds the lock, so the lock is not free and there is exactly one holder.
  obtain ⟨hf, hlen⟩ := hs.holder (List.mem_append.2 (Or.inl hr))
  -- There is at least one owner, r.
  have hpos : 0 < s.owners.length := List.length_pos_of_mem hr
  -- So nobody was handed the lock.
  have hhand : s.handed = [] := List.length_eq_zero_iff.1 (by omega)
  -- The owed waiter was handed the lock, or is the head.
  rcases hs.owed_ok w hw with h' | ⟨t, hq, -⟩
  -- Handed: nobody was.
  · rw [hhand] at h'; simp at h'
  -- The head. Name the state after the release.
  · intro u
    -- The release rule hands the lock to the head, w.
    have hu : u = _ := relFrom_handoff (s := { s with owners := s.owners.erase r }) hq hw
    -- Use that.
    rw [hu]
    -- The four claims read off the new state.
    simp [hf, hw]

/-- **Handoff-owed exclusivity, state.** While a handoff is owed to `w`,
the lock is not free, nobody but `w` was handed it, and nobody but `w` is
woken: nobody competes. TLA+: `HandoffExclusive`. -/
theorem owed_exclusive {B : Nat}
    -- The bound is at least 1.
    (hB : 0 < B) {s : M}
    -- `s` is reachable from the start.
    (h : Steps B init s) {w : Nat}
    -- A handoff is owed to `w`.
    (hw : s.owed = some w) :
    -- The statement: the lock is not free, and every handed or woken request is w.
    s.free = false ∧ (∀ x ∈ s.handed, x = w) ∧ (∀ x ∈ s.woken, x = w) := by
  -- The invariant holds.
  have hs := reachable_inv hB h
  -- A request handed the lock is w:
  have hh : ∀ x ∈ s.handed, x = w := fun x hx => by
    -- the handoff is owed to it,
    have := hs.handed_owed x hx
    -- and it is owed to w.
    rw [hw] at this
    -- So it is w.
    exact (Option.some.inj this).symm
  -- The lock is held; a handed request is w; a woken request was handed the lock, so it is w.
  exact ⟨hs.owed_held w hw, hh, fun x hx => hh x (hs.owed_quiet w hw x hx)⟩

/-- **Handoff-owed exclusivity, steps.** While a handoff is owed to `w`,
no step makes anyone but `w` an owner: barging is shut, and the lock goes
to `w`. TLA+: `HandoffExclusive`. -/
theorem owed_blocks_barging {B : Nat}
    -- The bound is at least 1.
    (hB : 0 < B) {s t : M}
    -- `s` is reachable from the start.
    (h : Steps B init s) {w : Nat}
    -- A handoff is owed to `w`.
    (hw : s.owed = some w)
    -- One step.
    (hstep : Step B s t) :
    -- The statement: every owner after the step was an owner before, or is w.
    ∀ x ∈ t.owners, x ∈ s.owners ∨ x = w := by
  -- The invariant holds before the step.
  have hs := reachable_inv hB h
  -- Take an owner x after the step.
  intro x hx
  -- Split on the kind of step.
  cases hstep with
  -- The fast path needs "nothing owed": but a handoff is owed. Impossible.
  | fast _ _ _ ho => rw [hw] at ho; cases ho
  -- Joining the queue changes no owner.
  | enqueue => exact Or.inl hx
  -- A release makes nobody an owner:
  | release r _ =>
    -- the owners afterwards are the old ones without r,
    rw [relFrom_owners] at hx
    -- so x was an owner before.
    exact Or.inl (List.mem_of_mem_erase hx)
  -- A request h' takes a handoff.
  | pollHanded h' _ hh =>
    -- The handoff is owed to the request handed the lock,
    have := hs.handed_owed h' hh
    -- and it is owed to w.
    rw [hw] at this
    -- So h' is w.
    cases this
    -- The owners are now w and the old ones.
    simp only [List.mem_cons] at hx
    -- x is w, or an old owner.
    rcases hx with rfl | hx
    -- w: the second alternative.
    · exact Or.inr rfl
    -- An old owner: the first alternative.
    · exact Or.inl hx
  -- Winning the race needs "nothing owed". Impossible.
  | pollWin _ _ _ _ ho => rw [hw] at ho; cases ho
  -- Losing a race needs "nothing owed". Impossible.
  | pollLose _ _ _ _ ho => rw [hw] at ho; cases ho
  -- The loss that reaches the bound needs "nothing owed yet". Impossible.
  | pollOwe _ _ _ _ ho => rw [hw] at ho; cases ho
  -- A waiter leaving makes nobody an owner.
  | cancelWaiting r _ => rw [wakeIfFree_owners] at hx; exact Or.inl hx
  -- A handed request giving the lock back makes nobody an owner.
  | cancelHanded r _ => rw [relFrom_owners] at hx; exact Or.inl hx

/-- **No stranded head.** While the lock is free, the head waiter has been
woken: no waiter sleeps beside a free lock. TLA+: `NoLostWakeup`. -/
theorem no_stranded_head {B : Nat}
    -- The bound is at least 1.
    (hB : 0 < B) {s : M}
    -- `s` is reachable from the start.
    (h : Steps B init s) {x : Nat} {t : List Nat}
    -- `x` is the head.
    (hq : s.queue = x :: t)
    -- The lock is free.
    (hf : s.free = true) :
    -- The statement: x has been woken.
    x ∈ s.woken :=
  -- That is one fact of the invariant, which every reachable state has.
  (reachable_inv hB h).stranded x t hq hf

/-- **A handoff is woken.** A request handed the lock has been woken.
TLA+: `NoLostWakeup` (a marked sleeper is woken). -/
theorem handed_woken {B : Nat}
    -- The bound is at least 1.
    (hB : 0 < B) {s : M}
    -- `s` is reachable from the start.
    (h : Steps B init s) {x : Nat}
    -- `x` was handed the lock.
    (hx : x ∈ s.handed) :
    -- The statement: x has been woken.
    x ∈ s.woken :=
  -- That is one fact of the invariant, which every reachable state has.
  (reachable_inv hB h).handed_woken x hx

/-! ## The RED cases -/

/-- The unbounded-barging design: a waiter never makes the try that raises
the owed flag. Every retry is a plain try, and a loss just counts. With the
flag never up, no release ever hands the lock over: a lock that never hands
off. TLA+: the mutant `UnboundedBarging`. (`Regolith/LockTable.lean` names
this relation for its own RED case.) -/
inductive StepNoHand : M → M → Prop
  /-- As `Step.fast`. -/
  | fast (s : M) (r : Nat)
      -- `r` is fresh, the lock free, nothing owed.
      (hr : r ∉ s.enq) (hf : s.free = true) (ho : s.owed = none) :
      -- After the step: the lock is held, r is an owner, and r has entered.
      StepNoHand s { s with free := false, owners := r :: s.owners, enq := s.enq ++ [r] }
  /-- As `Step.enqueue`. -/
  | enqueue (s : M) (r : Nat)
      -- `r` is fresh, and the fast path is shut.
      (hr : r ∉ s.enq) (hc : s.free = false ∨ s.owed ≠ none) :
      -- After the step: r is at the tail of the queue, has entered,
      StepNoHand s { s with queue := s.queue ++ [r], enq := s.enq ++ [r],
                            -- and has a count of 0.
                            bypass := upd s.bypass r 0 }
  /-- As `Step.release`. -/
  | release (s : M) (r : Nat)
      -- `r` holds the lock.
      (hr : r ∈ s.owners) :
      -- After the step: r is no longer an owner, and the release rule ran.
      StepNoHand s (relFrom { s with owners := s.owners.erase r })
  /-- The defect: a loss counts, whatever the count, and never raises the
  owed flag. -/
  | pollLose (s : M) (h : Nat)
      -- `h` was woken, not handed, the lock is taken, and nothing is owed.
      (hw : h ∈ s.woken) (hh : h ∉ s.handed) (hf : s.free = false) (ho : s.owed = none) :
      -- After the step: h's count is one more, and h is no longer "woken".
      StepNoHand s { s with bypass := upd s.bypass h (s.bypass h + 1), woken := without s.woken h }

/-- Any number of `StepNoHand` steps. -/
inductive StepsNoHand : M → M → Prop
  /-- No step at all. -/
  | refl (s : M) : StepsNoHand s s
  /-- Some steps, then one more. -/
  | tail {s t u : M} : StepsNoHand s t → StepNoHand t u → StepsNoHand s u

/-- The run both RED theorems below use: with bound 1 and no owed flag,
request 2 waits, is woken, and loses to barging request 3 (the design would
raise the flag here); at release it is woken again and loses to barging
request 4. -/
theorem barging_run :
    -- The statement: some state the defective steps reach has request 2 still waiting with a count
    -- of 2 and nothing owed.
    ∃ s, StepsNoHand init s ∧ 2 ∈ s.queue ∧ s.bypass 2 = 2 ∧ s.owed = none := by
  -- The run, written from the inside out: eight steps from the start, then the three facts about
  -- the final state, each computed.
  refine ⟨_, .tail (.tail (.tail (.tail (.tail (.tail (.tail (.tail (.refl init)
    -- Request 1 takes the lock; request 2 waits.
    (.fast _ 1 (by decide) rfl rfl))
    (.enqueue _ 2 (by decide) (Or.inl rfl)))
    -- Request 1 releases: the lock is free and request 2 woken.
    (.release _ 1 (by decide)))
    -- Request 3 barges in; request 2 is polled and loses.
    (.fast _ 3 (by decide) rfl rfl))
    (.pollLose _ 2 (by decide) (by decide) rfl rfl))
    -- Request 3 releases: no handoff, request 2 woken again.
    (.release _ 3 (by decide)))
    -- Request 4 barges in; request 2 loses again.
    (.fast _ 4 (by decide) rfl rfl))
    (.pollLose _ 2 (by decide) (by decide) rfl rfl), by decide, by decide, by decide⟩

/-- **`unbounded_barging_breaks_bound`.** With bound 1 and a lock that
never hands off, request 2 has lost 2 races, more than the bound: the law
of `bounded_bypass` fails. -/
theorem unbounded_barging_breaks_bound :
    -- The statement: some state the defective steps reach has request 2 still waiting with a count
    -- of 2.
    ∃ s, StepsNoHand init s ∧ 2 ∈ s.queue ∧ s.bypass 2 = 2 := by
  -- The run above ends in such a state.
  obtain ⟨s, hs, hq, hb, -⟩ := barging_run
  -- Its first three facts are the claim.
  exact ⟨s, hs, hq, hb⟩

/-- **`unbounded_barging_leaves_barging_open`.** In that same state nothing
is owed, so the next caller may barge again: the law of `at_bound_owed`
fails too. -/
theorem unbounded_barging_leaves_barging_open :
    -- The statement: request 2 waits with a count of 2, past the bound of 1, and nothing is owed.
    ∃ s, StepsNoHand init s ∧ 2 ∈ s.queue ∧ 1 < s.bypass 2 ∧ s.owed = none := by
  -- The run above ends in such a state.
  obtain ⟨s, hs, hq, hb, ho⟩ := barging_run
  -- A count of 2 is past 1.
  exact ⟨s, hs, hq, by omega, ho⟩

/-- The release-without-wake design: a release with waiters frees the lock
and wakes nobody. TLA+: the mutant `ReleaseNoWake`. -/
inductive StepNoWake : M → M → Prop
  /-- As `Step.fast`. -/
  | fast (s : M) (r : Nat)
      -- `r` is fresh, the lock free, nothing owed.
      (hr : r ∉ s.enq) (hf : s.free = true) (ho : s.owed = none) :
      -- After the step: the lock is held, r is an owner, and r has entered.
      StepNoWake s { s with free := false, owners := r :: s.owners, enq := s.enq ++ [r] }
  /-- As `Step.enqueue`. -/
  | enqueue (s : M) (r : Nat)
      -- `r` is fresh, and the fast path is shut.
      (hr : r ∉ s.enq) (hc : s.free = false ∨ s.owed ≠ none) :
      -- After the step: r is at the tail of the queue, has entered,
      StepNoWake s { s with queue := s.queue ++ [r], enq := s.enq ++ [r],
                            -- and has a count of 0.
                            bypass := upd s.bypass r 0 }
  /-- The defect: free the lock, wake nobody. -/
  | release (s : M) (r : Nat)
      -- `r` holds the lock.
      (hr : r ∈ s.owners) :
      -- After the step: r is no longer an owner and the lock is free. Nobody is woken.
      StepNoWake s { s with owners := s.owners.erase r, free := true }

/-- Any number of `StepNoWake` steps. -/
inductive StepsNoWake : M → M → Prop
  /-- No step at all. -/
  | refl (s : M) : StepsNoWake s s
  /-- Some steps, then one more. -/
  | tail {s t u : M} : StepsNoWake s t → StepNoWake t u → StepsNoWake s u

/-- **`release_without_wake_strands`.** Request 1 holds, request 2 waits,
request 1 releases without waking anyone: the lock is free and its head,
request 2, is not woken. The law of `no_stranded_head` fails. -/
theorem release_without_wake_strands :
    -- The statement: some state the defective steps reach has request 2 at the head of the queue,
    -- the lock free, and request 2 not woken.
    ∃ s, StepsNoWake init s ∧ s.queue = [2] ∧ s.free = true ∧ 2 ∉ s.woken := by
  -- The run, written from the inside out: three steps from the start, then the facts about the
  -- final state, each computed.
  refine ⟨_, .tail (.tail (.tail (.refl init)
    -- Request 1 takes the lock; request 2 waits.
    (.fast _ 1 (by decide) rfl rfl))
    (.enqueue _ 2 (by decide) (Or.inl rfl)))
    -- Request 1 releases and wakes nobody.
    (.release _ 1 (by decide)), by decide, rfl, by decide⟩

/-! ## E48: the waiter's node while its future spins

The machine above takes a waiter's look at its node whole: "woken" there
means the waiter will look at its node again, whether a release called its
waker or the waiter was awake anyway. E48 adds a waiter that is awake on
purpose: after it loses a retry it spins on its own node for a while
(`Wait::spin_park` in src/sync/waiter.rs) instead of going to sleep. To make
that cheap, the spin starts by taking the node's ARMED bit down
(`Waiter::begin_spin`): while it spins, a release that nudges or grants the
node finds no waker to claim, so it only sets the mark and calls nobody.

Tiny example of what could go wrong: waiter W spins. Holder H lets go and its
release nudges W's node: W is spinning, so no waker is called. W's spin then
runs out of rounds and W goes to sleep. If W went to sleep without reading its
node, the nudge would sit on the node for ever and nobody would ever call W's
waker again: a lost wakeup. The code prevents it by ending every spin in the
same look as before (`Wait::park` with the waker), whose last compare-and-swap
arms the waker and reads the marks together, so a mark that came during the
spin sends W back to retry instead of to sleep.

This section is that node, alone: one waiter's future, its node's ARMED bit
and its marks, with every order in which releases and the future's own steps
can happen. It proves that a sleeping future is always armed and never has an
unread mark (`no_lost_wakeup_spinning`), that a spinning future is never armed,
so a release that finds it spinning calls no waker (`mark_spinning_wakes_nobody`),
and, as the RED case, that a spin which gave up without the look would leave
the waiter asleep beside a mark (`spin_without_look_strands`). TLA+:
`NoLostWakeup` in `Sync.tla`, with `MC_Sync_Red_SpinNoRecheck`.

What it does not cover: it is not joined to the queue machine above. That
machine's `woken` stands for "this waiter will look at its node again"; this
one shows that the node-level handshake (a release's mark against the
future's look and spin) never breaks that promise, for one node and any
interleaving of its own steps. Which waiter a release marks, and that the
head of a free lock is marked at all, is `no_stranded_head` above; that a
handed waiter is marked is `handed_woken`. TLA+ checks the two together,
instruction by instruction, for two and three tasks.

The spin before queueing needs nothing new here: each of its rounds either
takes the free lock with no handoff owed, which is the `fast` step above (its
compare-and-swap refuses an owed handoff exactly as `fast` requires), or
changes nothing at all.
-/

/-- Where a waiter's future is, as its node sees it. -/
inductive Where where
  /-- It returned `Pending` and sleeps until its waker is called. -/
  | asleep
  /-- It is inside its poll, spinning on its node. -/
  | spinning
  /-- It is inside its poll doing anything else: looking, retrying, registering. -/
  | running
  /-- Its waker was called while it slept; the executor will poll it. -/
  | woken
  -- Lean writes the comparison of two places for us.
  deriving DecidableEq

/-- One waiter's node and future. -/
structure Node where
  /-- Where the future is. -/
  place : Where
  /-- The node's ARMED bit: a waker sits in the node that a release may claim
  and call. -/
  armed : Bool
  /-- A nudge or grant sits on the node that the future has not read yet. -/
  marked : Bool
  -- Lean writes the comparison of two nodes for us.
  deriving DecidableEq

/-- A future that has just queued: inside its poll, its waker not armed yet
(it is still registering), nothing marked. -/
def Node.start : Node := ⟨.running, false, false⟩

/-- The steps that can happen to one node, in any order. `lookAfterSpin` says
whether a spin ends with the look, as the code does (`true`), or, as the RED
case, may also go to sleep without it (`false`). -/
inductive NodeStep (lookAfterSpin : Bool) : Node → Node → Prop
  /-- A release nudges or grants an armed node (`Waiter::touch` claims the
  waker): the waker is taken and called, and the mark is set. A sleeping
  future is woken; a running one simply has a wake on its way. -/
  | markArmed (n : Node)
      -- The node is armed.
      (h : n.armed = true) :
      -- After the step: not armed, marked, and woken if it slept.
      NodeStep lookAfterSpin n
        -- (a sleeper is woken; any other future stays where it is)
        ⟨if n.place = .asleep then .woken else n.place, false, true⟩
  /-- A release nudges or grants a node that is not armed: only the mark is set,
  and no waker is called. -/
  | markUnarmed (n : Node)
      -- The node is not armed.
      (h : n.armed = false) :
      -- After the step: marked where it is.
      NodeStep lookAfterSpin n { n with marked := true }
  /-- The future looks at its node and nothing is marked (`Waiter::park` with
  the waker): its last swap arms the waker and reads the marks together, and
  the future sleeps. -/
  | lookSleep (n : Node)
      -- The future is inside its poll,
      (h : n.place = .running)
      -- and nothing is marked.
      (hm : n.marked = false) :
      -- After the step: asleep and armed.
      NodeStep lookAfterSpin n ⟨.asleep, true, false⟩
  /-- The future looks at its node and finds a mark: it takes the mark and goes
  on retrying, awake. It may come out armed (the mark landed while it was
  registering, and the arming swap read it) or not (it saw the mark before it
  registered); `a` is which. -/
  | lookMarked (n : Node) (a : Bool)
      -- The future is inside its poll,
      (h : n.place = .running)
      -- and a mark is there.
      (hm : n.marked = true) :
      -- After the step: running, the mark read, armed or not.
      NodeStep lookAfterSpin n ⟨.running, a || n.armed, false⟩
  /-- After a lost retry, the future starts to spin (`Waiter::begin_spin`): with
  nothing marked, it takes ARMED down and spins. -/
  | spinStart (n : Node)
      -- The future is inside its poll,
      (h : n.place = .running)
      -- and nothing is marked.
      (hm : n.marked = false) :
      -- After the step: spinning, not armed.
      NodeStep lookAfterSpin n ⟨.spinning, false, n.marked⟩
  /-- The spin ends, because it saw the mark or its rounds ran out; the future
  goes on to its look. -/
  | spinEnd (n : Node)
      -- The future spins.
      (h : n.place = .spinning) :
      -- After the step: inside its poll, about to look.
      NodeStep lookAfterSpin n { n with place := .running }
  /-- The RED case only: a spin that runs out arms the waker and sleeps without
  reading the mark. -/
  | spinSleep (n : Node)
      -- Only when the spin may skip its look,
      (hb : lookAfterSpin = false)
      -- and the future spins.
      (h : n.place = .spinning) :
      -- After the step: asleep and armed, whatever is marked.
      NodeStep lookAfterSpin n { n with place := .asleep, armed := true }
  /-- The executor polls a woken future: it is inside its poll again. -/
  | poll (n : Node)
      -- The waker was called.
      (h : n.place = .woken) :
      -- After the step: inside its poll.
      NodeStep lookAfterSpin n { n with place := .running }

/-- Any number of node steps, one after another. -/
inductive NodeSteps (lookAfterSpin : Bool) : Node → Node → Prop
  /-- No step at all. -/
  | refl (n : Node) : NodeSteps lookAfterSpin n n
  /-- Some steps, then one more. -/
  | tail {a b c : Node} :
      -- Steps from a to b, then one step from b to c, make steps from a to c.
      NodeSteps lookAfterSpin a b → NodeStep lookAfterSpin b c → NodeSteps lookAfterSpin a c

/-- The promise: a sleeping future is armed and has no unread mark on its node
(so a release that marks it calls its waker), and a spinning future is not
armed (so a release that marks it calls none, and the spin reads the mark). -/
def NodeInv (n : Node) : Prop :=
  -- A sleeper is armed and unmarked,
  (n.place = .asleep → n.armed = true ∧ n.marked = false) ∧
  -- and a spinner is not armed.
  (n.place = .spinning → n.armed = false)

/-- One step of the code keeps the promise. -/
theorem nodeStep_inv {n m : Node}
    -- The promise holds before the step,
    (hi : NodeInv n)
    -- and the step is one of the code's, with the look after every spin.
    (hs : NodeStep true n m) :
    -- The statement: it holds after.
    NodeInv m := by
  -- Unfold the promise on both sides.
  unfold NodeInv at *
  -- Name the two halves of the promise before the step.
  obtain ⟨hsleep, hspin⟩ := hi
  -- Look at which step it was.
  cases hs with
  -- An armed node marked: a sleeper wakes; a spinner cannot be armed.
  | markArmed h =>
    -- Split on whether the future slept, and settle each place.
    split <;> rename_i hp <;> cases hpl : n.place <;> simp_all
  -- An unarmed node marked: it stays where it is; a sleeper cannot be unarmed.
  | markUnarmed h =>
    -- Each place in turn; the promise before rules out an unarmed sleeper.
    cases hpl : n.place <;> simp_all
  -- The look that sleeps: asleep, armed, nothing marked.
  | lookSleep h hm =>
    -- Both halves hold outright.
    simp
  -- The look that finds a mark: the future runs on.
  | lookMarked a h hm =>
    -- It is neither asleep nor spinning.
    simp
  -- Starting to spin: spinning, not armed.
  | spinStart h hm =>
    -- Both halves hold outright.
    simp
  -- Ending the spin: the future runs again.
  | spinEnd h =>
    -- It is neither asleep nor spinning.
    simp
  -- The bad way out of a spin is not a step of the code.
  | spinSleep hb h =>
    -- `true = false` is impossible.
    exact absurd hb (by decide)
  -- A woken future polled runs again.
  | poll h =>
    -- It is neither asleep nor spinning.
    simp

/-- **`no_lost_wakeup_spinning`.** From a fresh node, after any steps of the
code, spins included, a sleeping future is armed with no unread mark, and a
spinning one is not armed. Rules out: a release nudges a spinning waiter
(calling no waker), the waiter stops spinning and sleeps, and nothing ever
wakes it. -/
theorem no_lost_wakeup_spinning {n : Node}
    -- Any node reached from a fresh one by the code's steps.
    (hr : NodeSteps true Node.start n) :
    -- The statement: the promise holds there.
    NodeInv n := by
  -- By induction over the steps taken.
  induction hr with
  -- At the start the future runs: neither half has anything to check.
  | refl => unfold NodeInv Node.start; simp
  -- Each further step keeps the promise.
  | tail _ hs ih => exact nodeStep_inv ih hs

/-- **`mark_spinning_wakes_nobody`.** In any node the code reaches, a release
that marks a spinning future finds it unarmed, so it only sets the mark: the
future stays spinning, no waker is called, and the spin reads the mark. This is
the wake E48 saves. -/
theorem mark_spinning_wakes_nobody {n : Node}
    -- A node the code reaches,
    (hr : NodeSteps true Node.start n)
    -- whose future spins.
    (h : n.place = .spinning) :
    -- The statement: the node is unarmed, and the release's step leaves it spinning, marked.
    n.armed = false ∧ NodeStep true n { n with marked := true } := by
  -- The promise says a spinner is unarmed.
  have hu : n.armed = false := (no_lost_wakeup_spinning hr).2 h
  -- So the release takes the "mark only" step.
  exact ⟨hu, .markUnarmed n hu⟩

/-- **`spin_without_look_strands`** (the RED case). If a spin that runs out may
sleep without its look, a fresh node can end asleep beside an unread mark: it
spins, a release marks it (calling no waker, since it spins), and it sleeps.
The promise of `no_lost_wakeup_spinning` fails. -/
theorem spin_without_look_strands :
    -- The statement: some node the defective steps reach is asleep and marked.
    ∃ n, NodeSteps false Node.start n ∧ n.place = .asleep ∧ n.marked = true := by
  -- The run: start spinning, get marked, sleep without looking.
  refine ⟨⟨.asleep, true, true⟩, .tail (.tail (.tail (.refl _)
    -- The fresh future starts to spin.
    (.spinStart Node.start rfl rfl))
    -- A release marks it; it was spinning and unarmed, so only the mark is set.
    (.markUnarmed _ rfl))
    -- Its spin runs out and it sleeps without reading the mark.
    (.spinSleep _ rfl rfl), rfl, rfl⟩

-- The end of this file's names.
end Regolith.Sync
