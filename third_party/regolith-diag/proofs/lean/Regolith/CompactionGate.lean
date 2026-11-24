/-!
# CompactionGate: the gate between compaction passes and jobs (plan 4.6, E28)

This file backs the TLA+ model `proofs/tla/CompactionGate.tla` (configurations
`MC_CompactionGate_*`). TLC checks that model for a handful of threads. Here
the same rules are state machines whose steps may be taken in any order, any
number of times, by any number of threads, so every interleaving is covered,
for every bound `B` (the code's `MAX_BYPASS`).

## The story, for a reader who knows neither Lean nor the code

A database keeps its data in files called tables. A background worker keeps
tidying them: one round of tidying is a *pass*, and several passes may run
side by side. A few operations (a checkpoint, a backup, an ingest, a manual
compaction, dropping everything) must not see a table deleted under them:
each is a *job*, and a job runs with no pass and no other job beside it.

The door that enforces this is the *compaction gate*
(`src/engine/compaction_gate.rs`). A pass is inside it "shared", a job is
inside it "alone".

The old gate could starve the worker (defect E28). Its rule was "while a job
waits, no new pass may enter". If jobs kept arriving, each while the one
before was still inside, a job was always waiting and the pass never got in.
The old worker also ran every job it was handed before any pass, so a stream
of jobs kept it from ever tidying.

The new rules, proved here:

1. Whoever finds the gate free for them walks in, even past waiters.
2. A waiter woken to try again that loses counts one loss.
3. The loss that brings its count to `B` makes it *owed*: nobody walks in any
   more, the threads inside leave, and the gate is handed out in queue order
   until the owed waiter is in.
4. The worker runs one job, then one pass, in turns.
5. A job parked on a caller's I/O queue is woken by a note in that caller's
   inbox, through a handle that is renewed every time the job parks again.

A tiny example of rule 3 with `B = 2`: job A is inside, the worker's pass
waits. A leaves and job B walks in first: loss 1. B leaves and job C walks in
first: loss 2. The pass is owed now: job D may not walk in. C leaves, and the
gate is handed to the pass.

## What is proved, in plain words

- `mutual_exclusion`: at most one job is inside, and then no pass is.
  TLA+: `MutualExclusion`.
- `bounded_bypass`, `loss_only_below_bound`, `owed_at_bound`: no waiter's
  count of losses exceeds `B`; a loss is only ever counted below `B`; a
  waiter whose count is `B` is the owed one. TLA+: `BoundedBypass`.
- `owed_shuts_barging`, `owed_front_is_handed_the_free_gate`,
  `owed_only_gets_nearer`: while a waiter is owed, nobody enters except by
  the handoff in queue order; once the gate is empty that handoff can happen;
  and every step either serves the owed waiter, brings it strictly nearer,
  or is a newcomer queueing behind it. So the worker's pass always gets its
  turn (E28). TLA+: `OwedShutsBarging` and the liveness properties
  `WorkerAlwaysPasses` and `EveryWaiterServed`.
- `worker_takes_turns`: a worker never runs two jobs without a pass between
  them. TLA+: `WorkerTakesTurns`.
- `parked_job_keeps_its_wake`, `wake_leaves_a_note`: a parked job always has
  either a live handle or a note already in the inbox, so the gate's wake
  always leaves a note. TLA+: `NoLostWake`.
- The RED cases, as counterexamples: `old_gate_starves_the_pass`,
  `no_handoff_starves_the_job`, `drain_jobs_starves_the_pass`,
  `wake_dropped_strands_the_job`, `stale_waker_strands_the_job` and
  `skip_gate_breaks_exclusion`.

## How to read the Lean

`structure` describes a state: a record with named fields. `inductive ...
: S → S → Prop` lists the steps the system can take: each line says "from
this state, under these conditions, to that state". `theorem` states a fact,
and what follows `:= by` is its proof, one small instruction (a *tactic*)
per line. Lines starting with `--` are comments. `{ s with x := v }` is the
state `s` with field `x` replaced by `v`. `l ++ [t]` is the list `l` with `t`
added at the back, `l.erase t` is `l` without `t`, and `h :: rest` is a list
whose first element is `h`.
-/

-- Everything below is named Regolith.CompactionGate.<name>.
namespace Regolith.CompactionGate

/-! ## Part 1. The gate -/

/-- What a thread asks the gate for. -/
inductive Kind where
  /-- A compaction pass: it shares the gate with other passes. -/
  | shared
  /-- A job: it is inside with nobody else. -/
  | alone

/-- The gate's state. A thread is named by a number. In the code these are
the bits and lists of the lock under the gate (`sync/raw_rwlock.rs`). -/
structure Gate where
  /-- How many passes are inside. The code's reader count. -/
  passes : Nat
  /-- How many jobs are inside. The code has one bit for this; a count lets
  the proof say "never two". -/
  jobs : Nat
  /-- The threads that wait, oldest first. The code's waiter list. -/
  queue : List Nat
  /-- What each waiting thread asks for. -/
  kind : Nat → Kind
  /-- How many times each waiting thread was woken, tried, and lost. The
  code's `Contend::bypass`. -/
  lost : Nat → Nat
  /-- The waiter that is owed the gate, if any. The code's HANDOFF bit,
  with the mark on that waiter. -/
  owed : Option Nat

/-- The start: nobody inside, nobody waiting, nobody owed. -/
def init : Gate := ⟨0, 0, [], fun _ => Kind.alone, fun _ => 0, none⟩

/-- The function `f`, changed to give `v` at `t` and what it gave before
everywhere else. -/
def upd {α : Type} (f : Nat → α) (t : Nat) (v : α) : Nat → α :=
  -- Asked about `x`: answer `v` if `x` is `t`, and what `f` answered otherwise.
  fun x => if x = t then v else f x

/-- Does a request of this kind fit the gate as it is? A pass fits unless a
job is inside. A job fits only an empty gate. -/
def fits (s : Gate) : Kind → Prop
  -- A pass: no job may be inside.
  | Kind.shared => s.jobs = 0
  -- A job: nobody at all may be inside.
  | Kind.alone => s.jobs = 0 ∧ s.passes = 0

/-- The gate with one more thread inside, of the given kind. -/
def enter (s : Gate) : Kind → Gate
  -- One more pass inside.
  | Kind.shared => { s with passes := s.passes + 1 }
  -- One more job inside.
  | Kind.alone => { s with jobs := s.jobs + 1 }

/-- Everything that can happen at the gate, for bound `B`. -/
inductive Step (B : Nat) : Gate → Gate → Prop
  /-- Walking in (barging): a thread that asks finds that its request fits
  and that nobody is owed, and is inside at once, whoever waits. In the code:
  the fast path of `RwLock::read` and `write`. -/
  | barge (s : Gate) (k : Kind)
      -- The request fits the gate as it is.
      (hf : fits s k)
      -- Nobody is owed the gate.
      (ho : s.owed = none) :
      -- Afterwards: one more thread of that kind is inside.
      Step B s (enter s k)
  /-- Queueing: a thread `t` that is not waiting yet joins the back of the
  queue, asking for `k`, with no loss counted. -/
  | enqueue (s : Gate) (t : Nat) (k : Kind)
      -- `t` is not in the queue already.
      (ht : t ∉ s.queue) :
      -- Afterwards: `t` is at the back of the queue, asking for `k`,
      Step B s { s with queue := s.queue ++ [t], kind := upd s.kind t k,
                        -- with no loss counted.
                        lost := upd s.lost t 0 }
  /-- A pass inside finishes and leaves. -/
  | leavePass (s : Gate)
      -- A pass is inside.
      (hp : 0 < s.passes) :
      -- Afterwards: one pass fewer inside.
      Step B s { s with passes := s.passes - 1 }
  /-- A job inside finishes and leaves. -/
  | leaveJob (s : Gate)
      -- A job is inside.
      (hj : 0 < s.jobs) :
      -- Afterwards: one job fewer inside.
      Step B s { s with jobs := s.jobs - 1 }
  /-- A woken waiter tries again and wins: its request fits and nobody is
  owed, so it is inside and out of the queue. -/
  | win (s : Gate) (t : Nat)
      -- `t` waits.
      (ht : t ∈ s.queue)
      -- Nobody is owed the gate.
      (ho : s.owed = none)
      -- `t`'s request fits.
      (hf : fits s (s.kind t)) :
      -- Afterwards: `t` is inside and no longer in the queue.
      Step B s { enter s (s.kind t) with queue := s.queue.erase t }
  /-- A woken waiter tries again and loses, because somebody walked in
  first: it keeps its place and counts the loss, and the loss that brings its
  count to `B` makes it the owed waiter in the same step. In the code:
  `owe_or_take` (`sync/queue.rs`). -/
  | lose (s : Gate) (t : Nat)
      -- `t` waits.
      (ht : t ∈ s.queue)
      -- Nobody is owed yet: while somebody is, a loss is not counted.
      (ho : s.owed = none)
      -- `t`'s request does not fit.
      (hf : ¬ fits s (s.kind t)) :
      -- Afterwards: `t` has one more loss,
      Step B s { s with lost := upd s.lost t (s.lost t + 1),
                        -- and is the owed waiter if that loss was its `B`-th.
                        owed := if s.lost t + 1 = B then some t else none }
  /-- The handoff: while a waiter is owed, the gate is given to the waiter
  at the front of the queue as soon as its request fits. If that was the
  owed waiter, nobody is owed any more. -/
  | grant (s : Gate) (h : Nat) (rest : List Nat)
      -- `h` is at the front of the queue.
      (hq : s.queue = h :: rest)
      -- Somebody is owed the gate.
      (ho : s.owed ≠ none)
      -- `h`'s request fits.
      (hf : fits s (s.kind h)) :
      -- Afterwards: `h` is inside,
      Step B s { enter s (s.kind h) with
                   -- out of the queue, and no longer owed if it was the owed one.
                   queue := rest, owed := if s.owed = some h then none else s.owed }

/-- The states the gate can be in: the start, and whatever a step leads to
from a state it can be in. -/
inductive Reach (B : Nat) : Gate → Prop
  /-- The start can be reached: it is where the gate begins. -/
  | start : Reach B init
  /-- One more step from a state that can be reached. -/
  | step {s t : Gate} : Reach B s → Step B s t → Reach B t

/-! ### What entering changes -/

/-- Entering does not touch the queue. -/
theorem enter_queue (s : Gate) (k : Kind) : (enter s k).queue = s.queue := by
  -- Look at both kinds; in each, `enter` changes another field.
  cases k <;> rfl

/-- Entering does not touch the loss counts. -/
theorem enter_lost (s : Gate) (k : Kind) : (enter s k).lost = s.lost := by
  -- Look at both kinds; in each, `enter` changes another field.
  cases k <;> rfl

/-- Entering does not touch who is owed. -/
theorem enter_owed (s : Gate) (k : Kind) : (enter s k).owed = s.owed := by
  -- Look at both kinds; in each, `enter` changes another field.
  cases k <;> rfl

/-- After a request that fits has entered, the gate holds no job, or it
holds exactly one job and no pass. -/
theorem enter_excl {s : Gate} {k : Kind}
    -- The request fits the gate before it enters.
    (hf : fits s k) :
    -- The claim: no job inside, or exactly one job and no pass.
    (enter s k).jobs = 0 ∨ ((enter s k).jobs = 1 ∧ (enter s k).passes = 0) := by
  -- Look at the two kinds one at a time.
  cases k with
  -- A pass fitted, so no job was inside, and none is now.
  | shared => exact Or.inl hf
  -- A job fitted, so nobody was inside: now it is the one job, with no pass.
  | alone =>
    -- Take the "one job and no pass" side.
    refine Or.inr ⟨?_, hf.2⟩
    -- The jobs inside are the old count plus one, and the old count was 0.
    show s.jobs + 1 = 1
    -- Rewrite the old count to 0; `0 + 1 = 1` is then true as written.
    rw [hf.1]

/-- A list with no repeats, plus one element it did not hold, at the back:
still no repeats. -/
theorem nodup_snoc {l : List Nat} {t : Nat}
    -- `l` has no repeats.
    (hl : l.Nodup)
    -- `t` is not in `l`.
    (ht : t ∉ l) :
    -- The claim: `l` with `t` at the back has no repeats.
    (l ++ [t]).Nodup := by
  -- Go through `l` from its first element.
  induction l with
  -- The empty list: `[t]` has one element, so no repeats.
  | nil => simp
  -- A list `a :: l`: `a` must not reappear, and the rest is as before.
  | cons a l ih =>
    -- Move the brackets: `(a :: l) ++ [t]` is `a :: (l ++ [t])`, and "no
    -- repeats" for such a list means: `a` is not in the rest, and the rest
    -- has no repeats.
    rw [List.cons_append, List.nodup_cons]
    -- The same reading of the fact that `a :: l` has no repeats.
    rw [List.nodup_cons] at hl
    -- Prove the two halves; the second is the earlier case, since `t` is
    -- not in `l` either.
    refine ⟨?_, ih hl.2 (fun h => ht (List.mem_cons_of_mem a h))⟩
    -- Suppose `a` were in `l ++ [t]`.
    intro ha
    -- Then `a` is in `l`, or `a` is `t`.
    rw [List.mem_append, List.mem_singleton] at ha
    -- Take the two possibilities in turn.
    rcases ha with ha | ha
    -- `a` in `l`: but `a :: l` had no repeats.
    · exact hl.1 ha
    -- `a` is `t`: but `t` was not in `a :: l`, whose first element is `a`.
    · exact ht (ha ▸ List.mem_cons_self)

/-! ### The invariant: what is true in every state the gate can reach -/

/-- The facts that hold at the start and that every step keeps. -/
structure Inv (B : Nat) (s : Gate) : Prop where
  /-- No job is inside, or exactly one job and no pass. -/
  excl : s.jobs = 0 ∨ (s.jobs = 1 ∧ s.passes = 0)
  /-- No waiter has lost more than `B` times. -/
  bound : ∀ t ∈ s.queue, s.lost t ≤ B
  /-- A waiter that has lost `B` times is the owed one. -/
  atBound : ∀ t ∈ s.queue, s.lost t = B → s.owed = some t
  /-- The owed waiter is in the queue. -/
  owedQueued : ∀ t, s.owed = some t → t ∈ s.queue
  /-- No thread waits twice. -/
  nodup : s.queue.Nodup

/-- The invariant holds at the start. -/
theorem init_inv (B : Nat) : Inv B init := by
  -- Give the five facts one by one.
  refine ⟨Or.inl rfl, ?_, ?_, ?_, List.nodup_nil⟩
  -- Nobody waits, so "every waiter ..." has nothing to check.
  · intro t ht; cases ht
  -- Nobody waits, so "every waiter ..." has nothing to check.
  · intro t ht; cases ht
  -- Nobody is owed at the start, so "the owed waiter ..." cannot arise.
  · intro t ht; cases ht

/-- Under the invariant, while nobody is owed, every waiter has lost fewer
than `B` times: so a counted loss always starts below the bound. -/
theorem Inv.below {B : Nat} {s : Gate}
    -- The invariant holds.
    (hs : Inv B s) {t : Nat}
    -- `t` waits.
    (ht : t ∈ s.queue)
    -- Nobody is owed.
    (ho : s.owed = none) :
    -- The claim: `t` has lost fewer than `B` times.
    s.lost t < B := by
  -- `t` has lost at most `B` times.
  have hle := hs.bound t ht
  -- And not exactly `B` times: then `t` would be owed, but nobody is.
  have hne : s.lost t ≠ B := by
    -- Suppose it were exactly `B`.
    intro e
    -- Then the invariant says `t` is the owed waiter.
    have h := hs.atBound t ht e
    -- But nobody is owed: "nobody" and "`t`" cannot be equal.
    rw [ho] at h
    -- That equation is impossible, which ends this case.
    cases h
  -- At most `B` and not `B` leaves fewer than `B`: plain arithmetic.
  omega

/-- Every step keeps the invariant. -/
theorem step_inv {B : Nat}
    -- The bound is at least one.
    (hB : 0 < B) {s t : Gate}
    -- The invariant holds before the step.
    (hs : Inv B s)
    -- The step.
    (hst : Step B s t) :
    -- The claim: the invariant holds after the step.
    Inv B t := by
  -- Take the seven kinds of step one at a time.
  cases hst with
  -- Walking in.
  | barge k hf ho =>
    -- The five facts: the first from `enter_excl`, the others below.
    refine ⟨enter_excl hf, ?_, ?_, ?_, ?_⟩
    -- Entering changes neither the queue nor the counts: as before.
    · rw [enter_queue, enter_lost]; exact hs.bound
    -- Entering changes none of queue, counts or the owed waiter: as before.
    · rw [enter_queue, enter_lost, enter_owed]; exact hs.atBound
    -- Entering changes neither the owed waiter nor the queue: as before.
    · rw [enter_queue, enter_owed]; exact hs.owedQueued
    -- Entering does not change the queue: still no repeats.
    · rw [enter_queue]; exact hs.nodup
  -- Queueing.
  | enqueue t k ht =>
    -- Nobody entered or left, so the first fact is as before.
    refine ⟨hs.excl, ?_, ?_, ?_, nodup_snoc hs.nodup ht⟩
    -- Every waiter's count is at most `B`.
    ·
      -- Take a waiter `x` of the new queue.
      intro x hx
      -- Its count is 0 if it is the newcomer, and the old count otherwise.
      show (if x = t then 0 else s.lost x) ≤ B
      -- Is `x` the newcomer?
      by_cases hxt : x = t
      -- The newcomer: its count is 0.
      · rw [ite_eq_left hxt]; omega
      -- An old waiter: its count did not change, and it was in the old queue.
      · rw [ite_eq_right hxt]
        -- `x` is in the old queue or is the newcomer, and it is not the newcomer.
        rcases List.mem_append.1 hx with h | h
        -- In the old queue: the invariant bounds its count.
        · exact hs.bound x h
        -- The newcomer: ruled out just above.
        · exact absurd (List.mem_singleton.1 h) hxt
    -- A waiter at the bound is the owed one.
    ·
      -- Take a waiter `x` of the new queue whose count is `B`.
      intro x hx hl
      -- Its count is 0 if it is the newcomer, and the old count otherwise.
      change (if x = t then 0 else s.lost x) = B at hl
      -- Is `x` the newcomer?
      by_cases hxt : x = t
      -- The newcomer's count is 0, which is not `B` since `B` is at least 1.
      · rw [ite_eq_left hxt] at hl; omega
      -- An old waiter: the invariant already says it is the owed one.
      · rw [ite_eq_right hxt] at hl
        -- `x` is in the old queue or is the newcomer, and it is not the newcomer.
        rcases List.mem_append.1 hx with h | h
        -- In the old queue: the invariant applies.
        · exact hs.atBound x h hl
        -- The newcomer: ruled out just above.
        · exact absurd (List.mem_singleton.1 h) hxt
    -- The owed waiter was in the old queue, so it is in the longer one.
    · intro x hx; exact List.mem_append.2 (Or.inl (hs.owedQueued x hx))
  -- A pass leaves.
  | leavePass hp =>
    -- Only the number of passes changed, so four facts are as before.
    refine ⟨?_, hs.bound, hs.atBound, hs.owedQueued, hs.nodup⟩
    -- Before, either no job was inside, or one job and no pass.
    rcases hs.excl with hj | ⟨hj, hp0⟩
    -- No job inside: still none.
    · exact Or.inl hj
    -- One job and no pass: but a pass was inside, so this cannot be.
    · exact Or.inr ⟨hj, by show s.passes - 1 = 0; omega⟩
  -- A job leaves.
  | leaveJob hj =>
    -- Only the number of jobs changed, so four facts are as before.
    refine ⟨Or.inl ?_, hs.bound, hs.atBound, hs.owedQueued, hs.nodup⟩
    -- The jobs inside are now one fewer.
    show s.jobs - 1 = 0
    -- Before there was no job (impossible, one is leaving) or exactly one.
    rcases hs.excl with h0 | ⟨h1, _⟩
    -- None before: one fewer than 0 is still 0.
    · omega
    -- One before: one fewer is 0.
    · omega
  -- A woken waiter wins.
  | win t ht ho hf =>
    -- The first fact from `enter_excl`; the last because removing a thread
    -- from a list without repeats leaves no repeats.
    refine ⟨enter_excl hf, ?_, ?_, ?_, hs.nodup.erase t⟩
    -- Those still waiting waited before, and counts did not change.
    ·
      -- Take a waiter `x` of the new queue.
      intro x hx
      -- Its count is its old count.
      show (enter s (s.kind t)).lost x ≤ B
      -- Entering does not touch the counts.
      rw [enter_lost]
      -- `x` was in the old queue, where the invariant bounds it.
      exact hs.bound x (List.mem_of_mem_erase hx)
    -- Nobody was owed, so no remaining waiter can be at the bound.
    ·
      -- Take a waiter `x` of the new queue whose count is `B`.
      intro x hx hl
      -- Its count is its old count.
      change (enter s (s.kind t)).lost x = B at hl
      -- Entering does not touch the counts.
      rw [enter_lost] at hl
      -- Then the invariant made `x` the owed waiter before the step.
      have h := hs.atBound x (List.mem_of_mem_erase hx) hl
      -- But nobody was owed: impossible.
      rw [ho] at h
      -- "nobody" is not "`x`".
      cases h
    -- Nobody is owed after the step either.
    ·
      -- Suppose `x` were owed after the step.
      intro x hx
      -- Who is owed did not change.
      change (enter s (s.kind t)).owed = some x at hx
      -- Entering does not touch who is owed, and before it was nobody.
      rw [enter_owed, ho] at hx
      -- "nobody" is not "`x`".
      cases hx
  -- A woken waiter loses.
  | lose t ht ho hf =>
    -- `t` had lost fewer than `B` times, because nobody was owed.
    have hlt := hs.below ht ho
    -- Nobody entered or left and the queue is the same.
    refine ⟨hs.excl, ?_, ?_, ?_, hs.nodup⟩
    -- Every waiter's count is at most `B`.
    ·
      -- Take a waiter `x`.
      intro x hx
      -- Its count is one more if it is `t`, and unchanged otherwise.
      show (if x = t then s.lost t + 1 else s.lost x) ≤ B
      -- Is `x` the loser?
      by_cases hxt : x = t
      -- The loser: one more than a count below `B` is at most `B`.
      · rw [ite_eq_left hxt]; omega
      -- Another waiter: unchanged, and bounded before.
      · rw [ite_eq_right hxt]; exact hs.bound x hx
    -- A waiter at the bound is the owed one.
    ·
      -- Take a waiter `x` whose count is now `B`.
      intro x hx hl
      -- Its count is one more if it is `t`, and unchanged otherwise.
      change (if x = t then s.lost t + 1 else s.lost x) = B at hl
      -- The owed waiter after the step is `t` if `t` reached `B`, else nobody.
      show (if s.lost t + 1 = B then some t else none) = some x
      -- Is `x` the loser?
      by_cases hxt : x = t
      -- The loser reached `B`, so the step made it the owed waiter.
      · rw [ite_eq_left hxt] at hl; rw [ite_eq_left hl, hxt]
      -- Another waiter at `B` would have been owed before; nobody was.
      · rw [ite_eq_right hxt] at hl
        -- The invariant says `x` was the owed waiter before the step.
        have h := hs.atBound x hx hl
        -- But nobody was owed: impossible.
        rw [ho] at h
        -- "nobody" is not "`x`".
        cases h
    -- The owed waiter, if there is one now, is `t`, which waits.
    ·
      -- Suppose `x` is owed after the step.
      intro x hx
      -- The owed waiter after the step is `t` if `t` reached `B`, else nobody.
      change (if s.lost t + 1 = B then some t else none) = some x at hx
      -- Did `t` reach `B`?
      by_cases hb : s.lost t + 1 = B
      -- Yes: the owed waiter is `t`, so `x` is `t`, which waits.
      · rw [ite_eq_left hb] at hx; cases hx; exact ht
      -- No: nobody is owed, so `x` cannot be.
      · rw [ite_eq_right hb] at hx; cases hx
  -- The handoff to the front waiter.
  | grant h rest hq ho hf =>
    -- The old queue `h :: rest` had no repeats: `h` is not in `rest`, and
    -- `rest` has none.
    have hnd := hs.nodup
    -- Write the old queue as `h :: rest`.
    rw [hq] at hnd
    -- Split "no repeats" for `h :: rest` into its two halves.
    rw [List.nodup_cons] at hnd
    -- A thread in `rest` was in the old queue.
    have hin : ∀ x, x ∈ rest → x ∈ s.queue := by
      -- Take such a thread.
      intro x hx
      -- The old queue is `h :: rest`, and `x` is in its tail.
      rw [hq]; exact List.mem_cons_of_mem h hx
    -- The first fact from `enter_excl`; the last is the second half above.
    refine ⟨enter_excl hf, ?_, ?_, ?_, hnd.2⟩
    -- Those still waiting waited before, and counts did not change.
    ·
      -- Take a waiter `x` of the new queue.
      intro x hx
      -- Its count is its old count.
      show (enter s (s.kind h)).lost x ≤ B
      -- Entering does not touch the counts; `x` was in the old queue.
      rw [enter_lost]; exact hs.bound x (hin x hx)
    -- A remaining waiter at the bound is still the owed one.
    ·
      -- Take a waiter `x` of the new queue whose count is `B`.
      intro x hx hl
      -- Its count is its old count.
      change (enter s (s.kind h)).lost x = B at hl
      -- Entering does not touch the counts.
      rw [enter_lost] at hl
      -- So `x` was the owed waiter before the step.
      have hox := hs.atBound x (hin x hx) hl
      -- `x` is not `h`: `x` is in `rest` and `h` is not.
      have hne : x ≠ h := fun e => hnd.1 (e ▸ hx)
      -- After the step the owed waiter is nobody if it was `h`, else unchanged.
      show (if s.owed = some h then none else s.owed) = some x
      -- It was `x`, which is not `h`, so it is unchanged: still `x`.
      rw [hox, ite_eq_right (fun e => hne (Option.some.inj e))]
    -- The owed waiter after the step is in the new queue.
    ·
      -- Suppose `x` is owed after the step.
      intro x hx
      -- After the step the owed waiter is nobody if it was `h`, else unchanged.
      change (if s.owed = some h then none else s.owed) = some x at hx
      -- Was the owed waiter `h`?
      by_cases hoh : s.owed = some h
      -- Yes: nobody is owed now, so `x` cannot be.
      · rw [ite_eq_left hoh] at hx; cases hx
      -- No: `x` was owed before, so it was in `h :: rest`.
      · rw [ite_eq_right hoh] at hx
        -- `x` was in the old queue.
        have hxq := hs.owedQueued x hx
        -- Write the old queue as `h :: rest`: `x` is `h`, or `x` is in `rest`.
        rw [hq, List.mem_cons] at hxq
        -- Take the two possibilities in turn.
        rcases hxq with e | e
        -- `x` is `h`: but the owed waiter was not `h`.
        · exact absurd (e ▸ hx) hoh
        -- `x` is in `rest`: what was to be shown.
        · exact e

/-- The invariant holds in every state the gate can reach. -/
theorem reach_inv {B : Nat}
    -- The bound is at least one.
    (hB : 0 < B) {s : Gate}
    -- `s` can be reached.
    (hs : Reach B s) :
    -- The claim: the invariant holds in `s`.
    Inv B s := by
  -- Follow how `s` was reached, one step at a time.
  induction hs with
  -- The start: shown above.
  | start => exact init_inv B
  -- One more step: the step keeps what held before it.
  | step _ hst ih => exact step_inv hB ih hst

/-- **Mutual exclusion.** In every state the gate can reach, at most one job
is inside, and when one is, no pass is. It rules out a checkpoint copying a
table while a pass deletes it. -/
theorem mutual_exclusion {B : Nat}
    -- The bound is at least one.
    (hB : 0 < B) {s : Gate}
    -- `s` can be reached.
    (hs : Reach B s) :
    -- The claim: at most one job inside, and with a job inside, no pass.
    s.jobs ≤ 1 ∧ (s.jobs = 1 → s.passes = 0) := by
  -- The invariant's first fact: no job, or one job and no pass.
  rcases (reach_inv hB hs).excl with h | ⟨h1, h0⟩
  -- No job: at most one, and "one job" is false, so nothing more to show.
  · exact ⟨by omega, fun e => by omega⟩
  -- One job and no pass: both halves are these facts.
  · exact ⟨by omega, fun _ => h0⟩

/-- **Bounded bypass, both ways (E28).** In every state the gate can reach,
no waiter, pass or job, has lost more than `B` times. It rules out the
worker's pass being passed over without end by a stream of jobs, and a job
being passed over without end by passes that keep overlapping. -/
theorem bounded_bypass {B : Nat}
    -- The bound is at least one.
    (hB : 0 < B) {s : Gate}
    -- `s` can be reached.
    (hs : Reach B s) :
    -- The claim: every waiter has lost at most `B` times.
    ∀ t ∈ s.queue, s.lost t ≤ B :=
  -- It is the invariant's second fact.
  (reach_inv hB hs).bound

/-- A loss is only ever counted below the bound: the step `lose` needs
nobody to be owed, and then every waiter has lost fewer than `B` times. -/
theorem loss_only_below_bound {B : Nat}
    -- The bound is at least one.
    (hB : 0 < B) {s : Gate}
    -- `s` can be reached.
    (hs : Reach B s) {t : Nat}
    -- `t` waits.
    (ht : t ∈ s.queue)
    -- Nobody is owed, which is what `lose` asks for.
    (ho : s.owed = none) :
    -- The claim: `t` has lost fewer than `B` times, so one more is allowed.
    s.lost t < B :=
  -- Shown for any state with the invariant.
  (reach_inv hB hs).below ht ho

/-- A waiter that has lost `B` times is the owed waiter: the gate is being
kept for it. -/
theorem owed_at_bound {B : Nat}
    -- The bound is at least one.
    (hB : 0 < B) {s : Gate}
    -- `s` can be reached.
    (hs : Reach B s) {t : Nat}
    -- `t` waits.
    (ht : t ∈ s.queue)
    -- `t` has lost `B` times.
    (hl : s.lost t = B) :
    -- The claim: `t` is the owed waiter.
    s.owed = some t :=
  -- It is the invariant's third fact.
  (reach_inv hB hs).atBound t ht hl

/-- **An owed waiter shuts barging.** While a waiter is owed, a step either
leaves no more threads inside than before, or is the handoff to the waiter
at the front of the queue. So nobody gets in ahead of the queue. -/
theorem owed_shuts_barging {B : Nat} {s t : Gate}
    -- Any step.
    (hst : Step B s t)
    -- Somebody is owed before it.
    (ho : s.owed ≠ none) :
    -- The claim: no more threads are inside than before,
    t.passes + t.jobs ≤ s.passes + s.jobs ∨
      -- or the step took the front waiter out of the queue (the handoff).
      ∃ h rest, s.queue = h :: rest ∧ t.queue = rest := by
  -- Take the seven kinds of step one at a time.
  cases hst with
  -- Walking in needs nobody to be owed: it cannot happen here.
  | barge k hf ho' => exact absurd ho' ho
  -- Queueing lets nobody in.
  | enqueue t k ht => exact Or.inl (Nat.le_refl _)
  -- A pass leaving makes the number inside smaller.
  | leavePass hp => exact Or.inl (by show s.passes - 1 + s.jobs ≤ s.passes + s.jobs; omega)
  -- A job leaving makes the number inside smaller.
  | leaveJob hj => exact Or.inl (by show s.passes + (s.jobs - 1) ≤ s.passes + s.jobs; omega)
  -- A waiter winning needs nobody to be owed: it cannot happen here.
  | win t ht ho' hf => exact absurd ho' ho
  -- A counted loss needs nobody to be owed: it cannot happen here.
  | lose t ht ho' hf => exact absurd ho' ho
  -- The handoff: the front waiter leaves the queue.
  | grant h rest hq ho' hf => exact Or.inr ⟨h, rest, hq, rfl⟩

/-- **The handoff can happen.** While a waiter is owed and the gate is
empty, the waiter at the front of the queue can be handed the gate. With
`owed_shuts_barging` this is why an owed pass gets its turn: nobody walks
in, the threads inside leave, and then the gate goes down the queue to it. -/
theorem owed_front_is_handed_the_free_gate {B : Nat} {s : Gate} {h : Nat} {rest : List Nat}
    -- Somebody is owed.
    (ho : s.owed ≠ none)
    -- `h` is at the front of the queue.
    (hq : s.queue = h :: rest)
    -- No pass is inside.
    (hp : s.passes = 0)
    -- No job is inside.
    (hj : s.jobs = 0) :
    -- The claim: a step exists that takes `h` out of the queue and leaves
    -- exactly one thread inside, which is `h`.
    ∃ t, Step B s t ∧ t.queue = rest ∧ t.passes + t.jobs = 1 := by
  -- Whatever `h` asks for fits an empty gate.
  have hf : fits s (s.kind h) := by
    -- Look at the two kinds.
    cases s.kind h with
    -- A pass needs no job inside.
    | shared => exact hj
    -- A job needs nobody inside.
    | alone => exact ⟨hj, hp⟩
  -- The state after the handoff, the handoff step, and its two properties.
  refine ⟨_, Step.grant s h rest hq ho hf, rfl, ?_⟩
  -- Count who is inside after `h` entered.
  show (enter s (s.kind h)).passes + (enter s (s.kind h)).jobs = 1
  -- Look at the two kinds.
  cases s.kind h with
  -- A pass: one pass and no job.
  | shared => show s.passes + 1 + s.jobs = 1; omega
  -- A job: no pass and one job.
  | alone => show s.passes + (s.jobs + 1) = 1; omega

/-! ### The owed waiter gets nearer with every step (E28: it always gets a turn) -/

/-- How many threads stand in front of `w` in a queue: 0 when `w` is first
(or not there at all). -/
def ahead (w : Nat) : List Nat → Nat
  -- An empty queue: nobody is in front.
  | [] => 0
  -- A queue starting with `h`: if `h` is `w`, nobody is in front; otherwise
  -- `h` is, plus whoever is in front of `w` in the rest.
  | h :: rest => if h = w then 0 else ahead w rest + 1

/-- A newcomer at the back does not change how many stand in front of a
thread already in the queue. -/
theorem ahead_snoc {w x : Nat} {l : List Nat}
    -- `w` is in the queue.
    (hw : w ∈ l) :
    -- The claim: as many stand in front of `w` with `x` at the back as without.
    ahead w (l ++ [x]) = ahead w l := by
  -- Go through the queue from its first thread.
  induction l with
  -- An empty queue does not hold `w`: nothing to show.
  | nil => cases hw
  -- A queue `a :: l`.
  | cons a l ih =>
    -- Write `(a :: l) ++ [x]` as `a :: (l ++ [x])` and unfold "in front of"
    -- on both sides: each is "0 if `a` is `w`, else the rest's count plus 1".
    show (if a = w then 0 else ahead w (l ++ [x]) + 1) = (if a = w then 0 else ahead w l + 1)
    -- Is the first thread `w`?
    by_cases haw : a = w
    -- Yes: both sides are 0.
    · rw [ite_eq_left haw, ite_eq_left haw]
    -- No: both sides are "the rest's count plus 1", and `w` is in the rest,
    -- where the earlier case applies.
    · rw [ite_eq_right haw, ite_eq_right haw]
      -- `w` is `a` or in `l`; it is not `a`.
      rcases List.mem_cons.1 hw with e | e
      -- `w` is `a`: ruled out.
      · exact absurd e.symm haw
      -- `w` is in `l`: use the earlier case.
      · rw [ih e]

/-- Entering puts exactly one more thread inside. -/
theorem enter_inside (s : Gate) (k : Kind) :
    -- The claim: the threads inside afterwards are those before, plus one.
    (enter s k).passes + (enter s k).jobs = s.passes + s.jobs + 1 := by
  -- Look at the two kinds.
  cases k with
  -- A pass: the passes grow by one. Plain arithmetic.
  | shared => show s.passes + 1 + s.jobs = s.passes + s.jobs + 1; omega
  -- A job: the jobs grow by one. Plain arithmetic.
  | alone => show s.passes + (s.jobs + 1) = s.passes + s.jobs + 1; omega

/-- How far the owed waiter `w` is from getting in: two for every thread in
front of it in the queue (each must be handed the gate and then leave), plus
one for every thread inside (each must leave). -/
def distance (w : Nat) (s : Gate) : Nat := 2 * ahead w s.queue + s.passes + s.jobs

/-- **Once owed, a waiter only gets nearer (E28).** Take any step from a
state where `w` is owed the gate. Then one of three things is true:
`w` is served (nobody is owed afterwards); or `w` is still owed and strictly
nearer than before; or a newcomer queued at the back, behind `w`, and `w` is
exactly as near as before. Nothing ever moves `w` further away. So after at
most `distance w s` steps by the threads inside and in front, `w` is in: a
stream of jobs cannot keep the worker's pass out, and the other way round. -/
theorem owed_only_gets_nearer {B : Nat} {s t : Gate} {w : Nat}
    -- The invariant holds before the step.
    (hs : Inv B s)
    -- Any step.
    (hst : Step B s t)
    -- `w` is owed before it.
    (how : s.owed = some w) :
    -- The claim, first possibility: `w` was served.
    t.owed = none ∨
      -- Second possibility: `w` is still owed, and strictly nearer.
      (t.owed = some w ∧ distance w t < distance w s) ∨
      -- Third possibility: `w` is still owed, as near as before, and the
      -- step was a newcomer `x` joining the back of the queue.
      (t.owed = some w ∧ distance w t = distance w s ∧ ∃ x, t.queue = s.queue ++ [x]) := by
  -- "Somebody is owed", in the form the steps ask about.
  have hsome : s.owed ≠ none := by rw [how]; exact fun e => by cases e
  -- Take the seven kinds of step one at a time.
  cases hst with
  -- Walking in needs nobody to be owed: it cannot happen here.
  | barge k hf ho => exact absurd ho hsome
  -- A newcomer queues at the back: the third possibility.
  | enqueue x k hx =>
    -- `w` is still owed; the distance is the same; the queue grew by `x`.
    refine Or.inr (Or.inr ⟨how, ?_, x, rfl⟩)
    -- The distance uses the new queue, `s.queue ++ [x]`.
    show 2 * ahead w (s.queue ++ [x]) + s.passes + s.jobs = 2 * ahead w s.queue + s.passes + s.jobs
    -- `w` is in the old queue, so the newcomer does not change who is in front.
    rw [ahead_snoc (hs.owedQueued w how)]
  -- A pass leaves: one fewer inside, so strictly nearer.
  | leavePass hp =>
    -- `w` is still owed, and the distance dropped by one.
    refine Or.inr (Or.inl ⟨how, ?_⟩)
    -- Spell both distances out; a pass was inside, so "minus one" is real.
    show 2 * ahead w s.queue + (s.passes - 1) + s.jobs < 2 * ahead w s.queue + s.passes + s.jobs
    -- Plain arithmetic, using that a pass was inside.
    omega
  -- A job leaves: one fewer inside, so strictly nearer.
  | leaveJob hj =>
    -- `w` is still owed, and the distance dropped by one.
    refine Or.inr (Or.inl ⟨how, ?_⟩)
    -- Spell both distances out; a job was inside, so "minus one" is real.
    show 2 * ahead w s.queue + s.passes + (s.jobs - 1) < 2 * ahead w s.queue + s.passes + s.jobs
    -- Plain arithmetic, using that a job was inside.
    omega
  -- A waiter winning needs nobody to be owed: it cannot happen here.
  | win x hx ho hf => exact absurd ho hsome
  -- A counted loss needs nobody to be owed: it cannot happen here.
  | lose x hx ho hf => exact absurd ho hsome
  -- The handoff to the front waiter `h`.
  | grant h rest hq ho hf =>
    -- Is the front waiter the owed one?
    by_cases hhw : h = w
    -- Yes: `w` is handed the gate, and nobody is owed afterwards.
    ·
      -- The first possibility is the one to show.
      refine Or.inl ?_
      -- After the step the owed waiter is nobody if it was `h`, else unchanged.
      show (if s.owed = some h then none else s.owed) = none
      -- The owed waiter was `w`, and `h` is `w`: the `then` side.
      rw [how, hhw, ite_eq_left rfl]
    -- No: `h` was in front of `w` and is gone from the queue, so `w` is nearer.
    ·
      -- The second possibility is the one to show, in two halves.
      refine Or.inr (Or.inl ⟨?_, ?_⟩)
      -- `w` is still owed: the owed waiter was not `h`, so it is unchanged.
      · show (if s.owed = some h then none else s.owed) = some w
        -- The owed waiter was `w`, which is not `h`: the `else` side.
        rw [how, ite_eq_right (fun e => hhw (Option.some.inj e).symm)]
      -- The distance: one thread fewer in front (worth 2), one more inside.
      · show 2 * ahead w rest + (enter s (s.kind h)).passes + (enter s (s.kind h)).jobs
            -- (the right-hand side: the distance before the step)
            < 2 * ahead w s.queue + s.passes + s.jobs
        -- In the old queue `h :: rest`, `h` is not `w`, so one more stood in front.
        have ha : ahead w s.queue = ahead w rest + 1 := by
          -- Write the old queue as `h :: rest` and unfold "in front of".
          rw [hq]; show (if h = w then 0 else ahead w rest + 1) = ahead w rest + 1
          -- `h` is not `w`: the `else` side.
          rw [ite_eq_right hhw]
        -- Entering put exactly one more thread inside.
        have he := enter_inside s (s.kind h)
        -- Plain arithmetic from those two facts.
        omega

/-! ### RED: a job that skips the gate (TLA+ `Red_SkipGate`) -/

/-- The gate's steps, plus the defect: a job walks in without asking. -/
inductive SkipStep (B : Nat) : Gate → Gate → Prop
  /-- Any real step. -/
  | real {s t : Gate} : Step B s t → SkipStep B s t
  /-- The defect: a job is inside, whatever the gate held. -/
  | skip (s : Gate) : SkipStep B s (enter s Kind.alone)

/-- The states the gate with the defect can reach. -/
inductive SkipReach (B : Nat) : Gate → Prop
  /-- The start. -/
  | start : SkipReach B init
  /-- One more step. -/
  | step {s t : Gate} : SkipReach B s → SkipStep B s t → SkipReach B t

/-- With the defect, a job and a pass are inside together: a pass walks in,
and then a job walks in without asking. -/
theorem skip_gate_breaks_exclusion (B : Nat) :
    -- The claim: a reachable state with one job and one pass inside.
    ∃ s, SkipReach B s ∧ s.jobs = 1 ∧ s.passes = 1 := by
  -- A pass walks into the empty gate: it fits, and nobody is owed.
  have h1 : SkipReach B (enter init Kind.shared) :=
    -- One real step (a barge) from the start.
    SkipReach.step SkipReach.start (SkipStep.real (Step.barge init Kind.shared rfl rfl))
  -- Then the job walks in without asking.
  have h2 : SkipReach B (enter (enter init Kind.shared) Kind.alone) :=
    -- One defect step from there.
    SkipReach.step h1 (SkipStep.skip _)
  -- That state holds one job and one pass.
  exact ⟨_, h2, rfl, rfl⟩

/-! ## Part 2. RED: the old gate starves the pass (TLA+ `Red_OldGate`) -/

/-- The old gate, seen from one pass that waits throughout. -/
structure Old where
  /-- A job is inside. -/
  writing : Bool
  /-- How many jobs wait. -/
  waiting : Nat
  /-- How many times the waiting pass looked and was held back. -/
  heldBack : Nat

/-- What can happen at the old gate while the pass waits. -/
inductive OldStep : Old → Old → Prop
  /-- A job arrives and waits. -/
  | arrive (s : Old) : OldStep s { s with waiting := s.waiting + 1 }
  /-- A waiting job enters the gate when no job is inside. -/
  | enter (s : Old)
      -- No job is inside.
      (hw : s.writing = false)
      -- A job waits.
      (hq : 0 < s.waiting) :
      -- Afterwards: a job is inside, and one fewer waits.
      OldStep s { s with writing := true, waiting := s.waiting - 1 }
  /-- The job inside leaves. -/
  | leave (s : Old)
      -- A job is inside.
      (hw : s.writing = true) :
      -- Afterwards: no job is inside.
      OldStep s { s with writing := false }
  /-- The pass looks. The old rule holds it back while a job is inside OR a
  job waits. -/
  | held (s : Old)
      -- A job is inside, or one waits.
      (h : s.writing = true ∨ 0 < s.waiting) :
      -- Afterwards: the pass was held back once more.
      OldStep s { s with heldBack := s.heldBack + 1 }

/-- The states the old gate can reach. -/
inductive OldReach : Old → Prop
  /-- The start: no job, and the pass has not looked yet. -/
  | start : OldReach ⟨false, 0, 0⟩
  /-- One more step. -/
  | step {s t : Old} : OldReach s → OldStep s t → OldReach t

/-- **The old gate starves the pass.** For every number `n`, the old gate
can reach a state where the pass has been held back `n` times and a job is
inside again: jobs that keep arriving keep it out for as long as they like.
There is no bound, which is exactly what `bounded_bypass` gives the new
gate. -/
theorem old_gate_starves_the_pass (n : Nat) : OldReach ⟨true, 0, n⟩ := by
  -- Build the states for 0, 1, 2, ... in turn.
  induction n with
  -- Held back 0 times: a job arrives, then enters.
  | zero =>
    -- A job arrives: one waits.
    have h1 : OldReach ⟨false, 1, 0⟩ := OldReach.step OldReach.start (OldStep.arrive _)
    -- It enters: a job is inside and none waits.
    exact OldReach.step h1 (OldStep.enter _ rfl (by decide))
  -- From "held back n times" to "held back n + 1 times".
  | succ n ih =>
    -- The next job arrives while the first is still inside.
    have h1 : OldReach ⟨true, 1, n⟩ := OldReach.step ih (OldStep.arrive _)
    -- The job inside leaves; the next one still waits.
    have h2 : OldReach ⟨false, 1, n⟩ := OldReach.step h1 (OldStep.leave _ rfl)
    -- The pass looks: a job waits, so it is held back once more.
    have h3 : OldReach ⟨false, 1, n + 1⟩ :=
      -- (the step `held`, allowed because one job waits: `0 < 1`)
      OldReach.step h2 (OldStep.held _ (Or.inr (by show (0 : Nat) < 1; decide)))
    -- The waiting job enters: the same picture as before, one loss later.
    -- (`0 < 1` is checked by computing it.)
    exact OldReach.step h3 (OldStep.enter _ rfl (by show (0 : Nat) < 1; decide))

/-! ## Part 3. RED: nobody is ever owed (TLA+ `Red_NoHandoff`) -/

/-- A gate that never owes anybody anything, seen from one job that waits
throughout while passes come and go. -/
structure Unfair where
  /-- How many passes are inside. -/
  passes : Nat
  /-- How many times the waiting job looked and lost. -/
  lost : Nat

/-- What can happen at that gate while the job waits. -/
inductive UnfairStep : Unfair → Unfair → Prop
  /-- A pass walks in: nothing ever holds a new pass back. -/
  | passIn (s : Unfair) : UnfairStep s { s with passes := s.passes + 1 }
  /-- A pass leaves. -/
  | passOut (s : Unfair)
      -- A pass is inside.
      (hp : 0 < s.passes) :
      -- Afterwards: one pass fewer inside.
      UnfairStep s { s with passes := s.passes - 1 }
  /-- The job is woken, looks, finds a pass inside and loses. -/
  | lose (s : Unfair)
      -- A pass is inside.
      (hp : 0 < s.passes) :
      -- Afterwards: the job has lost once more.
      UnfairStep s { s with lost := s.lost + 1 }

/-- The states that gate can reach. -/
inductive UnfairReach : Unfair → Prop
  /-- The start: an empty gate, and the job has not lost yet. -/
  | start : UnfairReach ⟨0, 0⟩
  /-- One more step. -/
  | step {s t : Unfair} : UnfairReach s → UnfairStep s t → UnfairReach t

/-- **Without the handoff, passes starve a job.** For every number `n`, that
gate can reach a state where the job has lost `n` times and a pass is still
inside: two passes that keep overlapping keep it out without end. -/
theorem no_handoff_starves_the_job (n : Nat) : UnfairReach ⟨1, n⟩ := by
  -- Build the states for 0, 1, 2, ... in turn.
  induction n with
  -- Lost 0 times: one pass walks in.
  | zero => exact UnfairReach.step UnfairReach.start (UnfairStep.passIn _)
  -- From "lost n times" to "lost n + 1 times".
  | succ n ih =>
    -- A second pass walks in before the first leaves.
    have h1 : UnfairReach ⟨2, n⟩ := UnfairReach.step ih (UnfairStep.passIn _)
    -- The first pass leaves, which wakes the job; a pass is still inside.
    have h2 : UnfairReach ⟨1, n⟩ :=
      -- (the step `passOut`, allowed because two passes are inside: `0 < 2`)
      UnfairReach.step h1 (UnfairStep.passOut _ (by show (0 : Nat) < 2; decide))
    -- The job looks and loses once more.
    exact UnfairReach.step h2 (UnfairStep.lose _ (by show (0 : Nat) < 1; decide))

/-! ## Part 4. The worker takes turns -/

/-- One compaction worker. In the code: one round of `compaction_loop`
(`src/engine/compaction.rs`). -/
structure Worker where
  /-- `true` when a job turn is next, `false` when a pass turn is. -/
  jobTurn : Bool
  /-- How many jobs callers have handed over that no worker has run yet. -/
  queued : Nat
  /-- How many jobs this worker has run since its last pass. -/
  sincePass : Nat

/-- What a worker and its callers can do. `drain` is the defect switch:
`false` is the real loop, `true` is the old one, which went on to the next
job as long as one was queued. -/
inductive WorkerStep (drain : Bool) : Worker → Worker → Prop
  /-- A caller hands over one more job. Callers may do this at any time. -/
  | submit (s : Worker) : WorkerStep drain s { s with queued := s.queued + 1 }
  /-- On a job turn with a job queued, the worker runs one job. The pass is
  next, unless the defect is on and another job is queued. -/
  | runJob (s : Worker)
      -- A job turn.
      (ht : s.jobTurn = true)
      -- A job is queued.
      (hq : 0 < s.queued) :
      -- Afterwards:
      WorkerStep drain s
        -- one job fewer is queued, and one more was run since the last pass;
        { s with queued := s.queued - 1, sincePass := s.sincePass + 1,
                 -- a job turn is next only under the defect, with a job left.
                 jobTurn := drain && decide (0 < s.queued - 1) }
  /-- On a job turn with nothing queued, the turn passes to the pass. -/
  | noJob (s : Worker)
      -- A job turn.
      (ht : s.jobTurn = true)
      -- Nothing is queued.
      (hq : s.queued = 0) :
      -- Afterwards: a pass turn is next.
      WorkerStep drain s { s with jobTurn := false }
  /-- On a pass turn, the worker runs one pass. A job turn is next. -/
  | pass (s : Worker)
      -- A pass turn.
      (ht : s.jobTurn = false) :
      -- Afterwards: a job turn is next, and no job was run since this pass.
      WorkerStep drain s { s with jobTurn := true, sincePass := 0 }

/-- The states a worker can reach. -/
inductive WorkerReach (drain : Bool) : Worker → Prop
  /-- The start: a job turn, nothing queued, nothing run. -/
  | start : WorkerReach drain ⟨true, 0, 0⟩
  /-- One more step. -/
  | step {s t : Worker} : WorkerReach drain s → WorkerStep drain s t → WorkerReach drain t

/-- What is true of the real worker in every state: it has run at most one
job since its last pass, and none when a job turn is next. -/
theorem worker_inv {s : Worker}
    -- `s` can be reached by the real loop.
    (hs : WorkerReach false s) :
    -- The claim: at most one job since the last pass, none on a job turn.
    s.sincePass ≤ 1 ∧ (s.jobTurn = true → s.sincePass = 0) := by
  -- Follow how `s` was reached, one step at a time.
  induction hs with
  -- The start: nothing run.
  | start => exact ⟨by decide, fun _ => rfl⟩
  -- One more step from a state where both facts hold.
  | step _ hst ih =>
    -- Take the four kinds of step one at a time.
    cases hst with
    -- Handing a job over changes neither the count nor the turn.
    | submit => exact ih
    -- Running a job: it was a job turn, so the count was 0 and is now 1,
    -- and the next turn is a pass turn.
    | runJob ht hq =>
      -- The count before was 0, since a job turn was next.
      have h0 := ih.2 ht
      -- The new count is the old one plus one, so 1; the next turn is not
      -- a job turn, so the second fact has nothing to say.
      refine ⟨?_, fun e => ?_⟩
      -- The old count was 0, so the new one is 1.
      · show _ + 1 ≤ 1; omega
      -- "`false && ...` is `true`" is false, so this case cannot arise.
      · simp at e
    -- No job to run: the count is unchanged, and a pass turn is next.
    | noJob ht hq => exact ⟨ih.1, fun e => by cases e⟩
    -- A pass: the count starts over at 0.
    | pass ht => exact ⟨by show (0 : Nat) ≤ 1; decide, fun _ => rfl⟩

/-- **The worker takes turns (E28).** The real worker never runs two jobs
without a pass between them, however fast jobs are handed to it. -/
theorem worker_takes_turns {s : Worker}
    -- `s` can be reached by the real loop.
    (hs : WorkerReach false s) :
    -- The claim: at most one job since the last pass.
    s.sincePass ≤ 1 :=
  -- The first fact of `worker_inv`.
  (worker_inv hs).1

/-- **RED: the old loop starves the pass (TLA+ `Red_DrainJobs`).** With the
defect on, for every number `n` the worker can have run `n` jobs since its
last pass with another still queued: callers that hand jobs over as fast as
it runs them keep it from ever passing. -/
theorem drain_jobs_starves_the_pass (n : Nat) : WorkerReach true ⟨true, 1, n⟩ := by
  -- Build the states for 0, 1, 2, ... in turn.
  induction n with
  -- No job run yet: one job is handed over.
  | zero => exact WorkerReach.step WorkerReach.start (WorkerStep.submit _)
  -- From "n jobs run" to "n + 1 jobs run".
  | succ n ih =>
    -- Another job is handed over: two are queued.
    have h1 : WorkerReach true ⟨true, 2, n⟩ := WorkerReach.step ih (WorkerStep.submit _)
    -- The worker runs one; one is still queued, so the old loop stays on
    -- jobs. The state after that step is the one wanted.
    exact WorkerReach.step h1 (WorkerStep.runJob _ rfl (by show (0 : Nat) < 2; decide))

/-! ## Part 5. A parked job keeps its wake -/

/-- One job of a caller's I/O queue that needs the gate. In the code:
`src/engine/io/job.rs`, Parking. -/
structure Parked where
  /-- The job is parked on its queue, waiting for the gate. -/
  parked : Bool
  /-- The wake-up handle the job registered at its last step can still
  fire. Each handle fires once (`Unpark::fired`). -/
  armed : Bool
  /-- A note for this job sits in its owner's inbox (`Message::Ready`): the
  owner will step the job at its next poll. -/
  note : Bool

/-- What can happen to that job. Two defect switches: `dropNote` makes a
firing handle leave no note; `stale` makes a job that parks again keep its
used-up handle. Both `false` is the real code. -/
inductive ParkStep (dropNote stale : Bool) : Parked → Parked → Prop
  /-- The owner steps the job for the first time, the gate is busy, and the
  job parks with a fresh handle. -/
  | park (s : Parked)
      -- The job is not parked yet.
      (hp : s.parked = false) :
      -- Afterwards: parked, with a live handle, and whatever note was there.
      ParkStep dropNote stale s ⟨true, true, s.note⟩
  /-- The gate wakes the job. A fresh handle is used up and leaves a note; a
  used-up handle does nothing. -/
  | wake (s : Parked) :
      -- Afterwards:
      ParkStep dropNote stale s
        -- a live handle is used up and leaves its note unless the defect
        -- drops it; a used-up handle changes nothing.
        (if s.armed then ⟨s.parked, false, s.note || !dropNote⟩ else s)
  /-- The owner takes the note and steps the job; the gate is still busy,
  and the job parks again, registering a fresh handle. -/
  | again (s : Parked)
      -- The job is parked.
      (hp : s.parked = true)
      -- A note is in the inbox.
      (hn : s.note = true) :
      -- Afterwards: parked, the note taken, and a live handle unless the
      -- defect kept the used-up one.
      ParkStep dropNote stale s ⟨true, !stale, false⟩
  /-- The owner takes the note and steps the job; the gate admits it, and
  the job runs. -/
  | run (s : Parked)
      -- The job is parked.
      (hp : s.parked = true)
      -- A note is in the inbox.
      (hn : s.note = true) :
      -- Afterwards: not parked, and neither handle nor note is left.
      ParkStep dropNote stale s ⟨false, false, false⟩

/-- The states that job can reach. -/
inductive ParkReach (dropNote stale : Bool) : Parked → Prop
  /-- The start: not parked, no handle, no note. -/
  | start : ParkReach dropNote stale ⟨false, false, false⟩
  /-- One more step. -/
  | step {s t : Parked} :
      -- From a state that can be reached, one step leads to another such state.
      ParkReach dropNote stale s → ParkStep dropNote stale s t → ParkReach dropNote stale t

/-- **A parked job keeps its wake.** In the real code, a parked job always
has a live handle or a note already in its owner's inbox. It rules out a job
parked for good beside a free gate. -/
theorem parked_job_keeps_its_wake {s : Parked}
    -- `s` can be reached by the real code.
    (hs : ParkReach false false s) :
    -- The claim: if the job is parked, its handle is live or a note is there.
    s.parked = true → s.armed = true ∨ s.note = true := by
  -- Follow how `s` was reached, one step at a time.
  induction hs with
  -- The start: the job is not parked, so there is nothing to show.
  | start => intro h; cases h
  -- One more step, from a state `s` where the fact holds.
  | @step s t _ hst ih =>
    -- Take the four kinds of step one at a time.
    cases hst with
    -- Parking registers a fresh handle.
    | park hp => intro _; exact Or.inl rfl
    -- A wake: look at whether the handle was fresh.
    | wake =>
      -- Suppose the job is parked after the wake.
      intro h
      -- Was the handle fresh?
      cases ha : s.armed with
      -- Used up: the wake changed nothing, and the fact held before.
      | false =>
        -- With a used-up handle, "the state after the wake" is the state
        -- before: replace the handle by `false` and pick the `else` side.
        simp only [ha, Bool.false_eq_true, ite_false] at h ⊢
        -- So the job was parked before, and the fact held then.
        rcases ih h with h1 | h1
        -- "The handle was live" contradicts "used up".
        · rw [ha] at h1; cases h1
        -- A note was there, and still is.
        · exact Or.inr h1
      -- Fresh: it fired and left a note.
      | true =>
        -- With a fresh handle the state after the wake is the `then` side,
        -- whose note is "the old note, or true". (Looking at the two cases
        -- already wrote `true` for the handle in what is to be shown.)
        simp only [ite_true]
        -- "Anything or true" is true.
        exact Or.inr (by simp)
    -- Parking again registers a fresh handle.
    | again hp hn => intro _; exact Or.inl rfl
    -- The job ran: it is not parked, so there is nothing to show.
    | run hp hn => intro h; cases h

/-- **The gate's wake always leaves a note.** In the real code, after the
gate wakes a parked job, a note for it is in its owner's inbox. -/
theorem wake_leaves_a_note {s : Parked}
    -- `s` can be reached by the real code.
    (hs : ParkReach false false s)
    -- The job is parked.
    (hp : s.parked = true) :
    -- The claim: in the state after a wake (written out as in the step
    -- `wake`, with the defect off), the note is there.
    (if s.armed then (⟨s.parked, false, s.note || !false⟩ : Parked) else s).note = true := by
  -- Was the handle fresh?
  cases ha : s.armed with
  -- Used up: the wake changes nothing, and by the theorem above a note was
  -- already there.
  | false =>
    -- The state after the wake is the state before.
    show s.note = true
    -- A parked job has a live handle or a note.
    rcases parked_job_keeps_its_wake hs hp with h | h
    -- "Live" contradicts "used up".
    · rw [ha] at h; cases h
    -- The note.
    · exact h
  -- Fresh: the handle fires and leaves the note.
  | true => show (s.note || !false) = true; simp

/-- **RED: a wake that leaves no note (TLA+ `Red_WakeDropped`).** With that
defect, the job can be parked with a used-up handle and no note: nothing
will ever make its owner look. -/
theorem wake_dropped_strands_the_job :
    -- The claim: "parked, handle used up, no note" can be reached.
    ParkReach true false ⟨true, false, false⟩ := by
  -- The job parks with a fresh handle.
  have h1 : ParkReach true false ⟨true, true, false⟩ :=
    -- One step, `park`, from the start.
    ParkReach.step ParkReach.start (ParkStep.park _ rfl)
  -- The gate wakes it: the handle is used up, and the defect drops the note.
  exact ParkReach.step h1 (ParkStep.wake _)

/-- **RED: a handle that is not renewed (TLA+ `Red_StaleWaker`).** With that
defect, a job that was woken once and parks again has a used-up handle and
no note: the gate's next wake reaches nobody. -/
theorem stale_waker_strands_the_job :
    -- The claim: "parked, handle used up, no note" can be reached.
    ParkReach false true ⟨true, false, false⟩ := by
  -- The job parks with a fresh handle.
  have h1 : ParkReach false true ⟨true, true, false⟩ :=
    -- One step, `park`, from the start.
    ParkReach.step ParkReach.start (ParkStep.park _ rfl)
  -- The gate wakes it: the handle is used up and a note is left.
  have h2 : ParkReach false true ⟨true, false, true⟩ := ParkReach.step h1 (ParkStep.wake _)
  -- The owner steps it; the gate is busy again; it parks with the used-up
  -- handle, and the note is gone.
  exact ParkReach.step h2 (ParkStep.again _ rfl rfl)

-- The end of this file's names.
end Regolith.CompactionGate
