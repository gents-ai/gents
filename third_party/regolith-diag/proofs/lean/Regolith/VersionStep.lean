/-!
# VersionStep: one section at a time, in the order handed over, none left behind

This file backs the TLA+ model `proofs/tla/VersionStep.tla`. TLC checks
there two callers and one job that keeps the step, in every interleaving.
Here the rules are proved for every number of threads and sections.

## The story, for a reader who has never seen the code

The version set says which tables the database is made of. Every change to
it reads the current version, builds the next one from it and installs that,
so two changes must never overlap: the second would overwrite the first.

A mutex used to keep them apart. The step replaces it
(`src/engine/step.rs`). Think of a workshop with one workbench and a tray.
A worker with a job for the bench puts it on the tray. If nobody is at the
bench, that worker is at the bench from that moment, and does every job on
the tray, oldest first, its own and other workers', until the tray is
empty; then it leaves. If somebody is at the bench, the worker waits until
its job has been done by whoever is there. A worker that may not wait takes
the bench first, when it is free, and does its jobs there itself.

Tiny example of what must not happen. A worker leaves the bench at the very
moment another puts a job on the tray and, seeing somebody at the bench,
walks off to wait. Now nobody is at the bench and a job is on the tray: it
is never done. The code prevents it by leaving with one compare-and-swap
that goes in only when the tray is empty.

## What is proved, in plain words

1. `one_holder`: at most one thread holds the step, whatever the threads do
   (TLA+ `OneAtATime`). Taking the step without looking whether it is free
   gives two (`blind_take_makes_two_holders`, TLA+ RED `HoldWithoutCas`).
2. `runs_in_push_order`: the sections that ran are the first of the
   sections handed over, in the same order (TLA+ `InPushOrder`).
3. `none_left_behind`: whenever nobody holds the step, every section handed
   over has run (TLA+ `NoneLeftBehind`).
4. The RED cases, as counterexamples: giving the step up without looking at
   the list leaves a section with nobody to run it
   (`giving_up_blind_leaves_a_section`, TLA+ RED `GiveUpBlind`), and taking
   the list newest first runs sections out of order
   (`newest_first_runs_out_of_order`, TLA+ RED `NewestFirst`).

## How to read the Lean

`def` defines a thing, `theorem` states a fact and its proof follows
`:= by`. Lines starting with `--` are comments, in plain words. A proof is a
list of *tactics*; each one changes the goal still to be shown, and the
comment above it says how. `simp` rewrites with known facts; `cases` splits
on the ways a fact could be true; `induction` proves a fact for every number
of steps by proving it for none and then for one more.
-/

-- Everything below is named Regolith.VersionStep.<name>.
namespace Regolith.VersionStep

/-! ## Part 1. One holder -/

/-- Who is at the bench. Threads are named by number. -/
structure Bench where
  /-- The flag in the list's head word: nobody holds the step. -/
  idle : Bool
  /-- Whether each thread holds the step. -/
  has : Nat → Bool

/-- The start: nobody holds the step. -/
def Bench.start : Bench :=
  -- The flag is set, and no thread holds anything.
  ⟨true, fun _ => false⟩

/-- `has` with thread `t` set to `b`. -/
def setHas (has : Nat → Bool) (t : Nat) (b : Bool) : Nat → Bool :=
  -- `b` for `t`, the old answer for every other thread.
  fun x => if x = t then b else has x

/-- What a thread can do to the bench. -/
inductive Move : Bench → Bench → Prop
  /-- Thread `t` takes the step: the compare-and-swap that clears the flag
  goes in only when the flag is set. -/
  | take (b : Bench) (t : Nat) (h : b.idle = true) :
      -- The flag is cleared, and `t` holds the step.
      Move b ⟨false, setHas b.has t true⟩
  /-- Thread `t`, which holds the step, gives it up. -/
  | giveUp (b : Bench) (t : Nat) (h : b.has t = true) :
      -- The flag is set, and `t` holds nothing.
      Move b ⟨true, setHas b.has t false⟩

/-- The benches the moves can reach from the start. -/
inductive Reachable : Bench → Prop
  /-- The start is reachable. -/
  | start : Reachable Bench.start
  /-- One move from a reachable bench reaches another. -/
  | move {b c : Bench} : Reachable b → Move b c → Reachable c

/-- The two facts every reachable bench has. -/
structure BenchInv (b : Bench) : Prop where
  /-- With the flag set, no thread holds the step. -/
  idle_empty : b.idle = true → ∀ t, b.has t = false
  /-- Two threads that hold the step are the same thread. -/
  one : ∀ t u, b.has t = true → b.has u = true → t = u

/-- One move keeps both facts. -/
theorem bench_move {b c : Bench} (hb : BenchInv b) (hm : Move b c) : BenchInv c := by
  -- One case per move.
  cases hm with
  -- Case: thread t takes the step, the flag having been set.
  | take t hidle =>
    -- Before the move nobody held the step.
    have hnone := hb.idle_empty hidle
    -- The two facts, one at a time.
    refine ⟨?_, ?_⟩
    -- The flag is cleared now, so the first fact asks nothing.
    · intro hi; simp at hi
    -- Whoever holds the step now is `t`: nobody else held it before.
    · intro x y hx hy
      -- `x` holds it: it is `t`, or it held it before, which nobody did.
      have hxt : x = t := by
        -- Split on whether `x` is `t`.
        by_cases hxe : x = t
        -- It is.
        · exact hxe
        -- It is not: then it held the step before, against `hnone`.
        · simp [setHas, hxe, hnone x] at hx
      -- The same for `y`.
      have hyt : y = t := by
        -- Split on whether `y` is `t`.
        by_cases hye : y = t
        -- It is.
        · exact hye
        -- It is not: then it held the step before, against `hnone`.
        · simp [setHas, hye, hnone y] at hy
      -- Both are `t`.
      rw [hxt, hyt]
  -- Case: thread t, which holds the step, gives it up.
  | giveUp t ht =>
    -- The two facts, one at a time.
    refine ⟨?_, ?_⟩
    -- The flag is set: no thread holds the step any more.
    · intro _ x
      -- Split on whether `x` is `t`.
      by_cases hxe : x = t
      -- `t` just gave it up.
      · simp [setHas, hxe]
      -- Another thread keeps its old answer ...
      · show (if x = t then false else b.has x) = false
        -- ... which is the one to look at.
        simp only [hxe, ↓reduceIte]
        -- A Boolean is false or true.
        cases hx : b.has x with
        -- It did not hold the step.
        | false => rfl
        -- It did: then it is `t`, which it is not.
        | true => exact absurd (hb.one x t hx ht) hxe
    -- Holders after the move held the step before it.
    · intro x y hx hy
      -- `x` is not `t`, and held the step before.
      have hx' : b.has x = true := by
        -- Split on whether `x` is `t`.
        by_cases hxe : x = t
        -- `t` holds nothing now, against `hx`.
        · simp [setHas, hxe] at hx
        -- Another thread: its answer is the old one.
        · simpa [setHas, hxe] using hx
      -- The same for `y`.
      have hy' : b.has y = true := by
        -- Split on whether `y` is `t`.
        by_cases hye : y = t
        -- `t` holds nothing now, against `hy`.
        · simp [setHas, hye] at hy
        -- Another thread: its answer is the old one.
        · simpa [setHas, hye] using hy
      -- Two holders before the move were one thread.
      exact hb.one x y hx' hy'

/-- Every reachable bench has both facts. -/
theorem bench_inv {b : Bench} (h : Reachable b) : BenchInv b := by
  -- By how the bench was reached.
  induction h with
  -- Case: the start.
  | start =>
    -- Nobody holds anything, so both facts are immediate.
    exact ⟨fun _ _ => rfl, fun t u ht _ => by simp [Bench.start] at ht⟩
  -- Case: a reachable bench, with both facts (ih), then one move, which keeps them.
  | move _ hm ih => exact bench_move ih hm

/-- **`one_holder`.** At most one thread holds the step: two threads that
hold it are the same thread. So sections, which only the holder runs, never
overlap. -/
theorem one_holder {b : Bench} (h : Reachable b) :
    -- The claim itself; its proof follows.
    ∀ t u, b.has t = true → b.has u = true → t = u :=
  -- One of the two facts every reachable bench has.
  (bench_inv h).one

/-- **`blind_take_makes_two_holders`.** A thread that takes the step without
looking at the flag (bug `HoldWithoutCas`): thread 1 holds the step, thread
2 takes it too, and now two different threads hold it. -/
theorem blind_take_makes_two_holders :
    -- Thread 1 at the bench.
    let one : Bench := ⟨false, setHas (fun _ => false) 1 true⟩
    -- Thread 2 takes the step although the flag is cleared.
    let two : Bench := ⟨false, setHas one.has 2 true⟩
    -- Both hold it, and they are different threads.
    two.has 1 = true ∧ two.has 2 = true := by
  -- Both are read off the definition.
  simp [setHas]

/-! ## Part 2. The order, and nothing left behind -/

/-- The list and the sections, at one moment. Sections are named by number. -/
structure State where
  /-- Every section handed over, oldest first (a ghost). -/
  pushed : List Nat
  /-- The sections on the list: handed over and not taken yet, oldest first. -/
  list : List Nat
  /-- The sections the holder took and has not run yet, oldest first. -/
  batch : List Nat
  /-- The sections that ran, in the order they ran. -/
  ran : List Nat
  /-- Whether a thread holds the step (Part 1: at most one does). -/
  held : Bool

/-- The start: nothing handed over, nobody at the step. -/
def init : State :=
  -- Four empty lists, and the step free.
  ⟨[], [], [], [], false⟩

/-- The steps of `src/engine/step.rs`, each one atomic step there. -/
inductive Step : State → State → Prop
  /-- A section is pushed and finds nobody holding the step: the push takes
  the step (`Step::run`, the push that finds `IDLE`). -/
  | pushTake (s : State) (x : Nat) (h : s.held = false) :
      -- The section is on the list, and the step is held.
      Step s { s with pushed := s.pushed ++ [x], list := s.list ++ [x], held := true }
  /-- A section is pushed while a thread holds the step: it waits on the
  list for that thread. -/
  | pushWait (s : State) (x : Nat) (h : s.held = true) :
      -- The section is on the list.
      Step s { s with pushed := s.pushed ++ [x], list := s.list ++ [x] }
  /-- A job takes the step to keep it (`Step::hold_or_park`), nobody
  holding it. -/
  | hold (s : State) (h : s.held = false) :
      -- The step is held.
      Step s { s with held := true }
  /-- The holder, with nothing left in its hands, takes the whole list
  (`Step::drain`). -/
  | take (s : State) (h : s.held = true) (hb : s.batch = []) :
      -- The list is in its hands, in the same order, and the list is empty.
      Step s { s with batch := s.list, list := [] }
  /-- The holder runs the oldest section in its hands. -/
  | run (s : State) (x : Nat) (rest : List Nat) (h : s.held = true) (hb : s.batch = x :: rest) :
      -- It ran, after every section that ran before.
      Step s { s with batch := rest, ran := s.ran ++ [x] }
  /-- The holder gives the step up: only with nothing in its hands and an
  empty list (`Step::sweep`, the compare-and-swap of `rest_if_empty`). -/
  | giveUp (s : State) (h : s.held = true) (hb : s.batch = []) (hl : s.list = []) :
      -- Nobody holds the step.
      Step s { s with held := false }

/-- The states the steps can reach from the start. -/
inductive Reach : State → Prop
  /-- The start is reachable. -/
  | init : Reach init
  /-- One step from a reachable state reaches another. -/
  | step {s t : State} : Reach s → Step s t → Reach t

/-- The two facts every reachable state has. -/
structure Inv (s : State) : Prop where
  /-- What ran, then what the holder has, then what is on the list, is
  everything handed over, in the order handed over. -/
  split : s.ran ++ s.batch ++ s.list = s.pushed
  /-- With nobody holding the step, nothing is waiting anywhere. -/
  free_empty : s.held = false → s.batch = [] ∧ s.list = []

/-- One step keeps both facts. -/
theorem inv_step {s t : State} (hs : Inv s) (hst : Step s t) : Inv t := by
  -- One case per step.
  cases hst with
  -- Case: a push that takes the step.
  | pushTake x hfree =>
    -- The two facts, one at a time.
    refine ⟨?_, ?_⟩
    -- The new section is last on the list and last handed over.
    · simp [← hs.split, List.append_assoc]
    -- The step is held now, so the second fact asks nothing.
    · intro hh; simp at hh
  -- Case: a push that waits.
  | pushWait x hheld =>
    -- The two facts, one at a time.
    refine ⟨?_, ?_⟩
    -- The new section is last on the list and last handed over.
    · simp [← hs.split, List.append_assoc]
    -- The step is still held, so the second fact asks nothing.
    · intro hh; simp [hheld] at hh
  -- Case: a job takes the step to keep it.
  | hold hfree =>
    -- The lists are unchanged; the step is held, so the second fact asks
    -- nothing.
    exact ⟨hs.split, fun hh => by simp at hh⟩
  -- Case: the holder takes the list.
  | take hheld hb =>
    -- The two facts, one at a time.
    refine ⟨?_, ?_⟩
    -- Its hands were empty, so what was on the list is now in them.
    · simpa [hb] using hs.split
    -- The step is still held, so the second fact asks nothing.
    · intro hh; simp [hheld] at hh
  -- Case: the holder runs the oldest section in its hands.
  | run x rest hheld hb =>
    -- The two facts, one at a time.
    refine ⟨?_, ?_⟩
    -- The section moved from the front of its hands to the end of `ran`.
    · simpa [hb, List.append_assoc] using hs.split
    -- The step is still held, so the second fact asks nothing.
    · intro hh; simp [hheld] at hh
  -- Case: the holder gives the step up.
  | giveUp hheld hb hl =>
    -- The lists are unchanged, and both were empty.
    exact ⟨hs.split, fun _ => ⟨hb, hl⟩⟩

/-- Every reachable state has both facts. -/
theorem inv_reach {s : State} (h : Reach s) : Inv s := by
  -- By how the state was reached.
  induction h with
  -- Case: the start.
  | init =>
    -- Every list is empty.
    exact ⟨rfl, fun _ => ⟨rfl, rfl⟩⟩
  -- Case: a reachable state, with both facts (ih), then one step, which keeps them.
  | step _ hst ih => exact inv_step ih hst

/-- **`runs_in_push_order`.** The sections that ran are the first of the
sections handed over, in the same order: what is left over is what the
holder has and what is on the list. -/
theorem runs_in_push_order {s : State} (h : Reach s) :
    -- The claim itself; its proof follows.
    ∃ rest, s.ran ++ rest = s.pushed :=
  -- The rest is what the holder has, then what is on the list.
  ⟨s.batch ++ s.list, by simpa [List.append_assoc] using (inv_reach h).split⟩

/-- **`none_left_behind`.** Whenever nobody holds the step, every section
handed over has run: none is waiting with nobody to run it. -/
theorem none_left_behind {s : State} (h : Reach s) (hfree : s.held = false) :
    -- The claim itself; its proof follows.
    s.ran = s.pushed := by
  -- With nobody at the step, the holder's hands and the list are empty.
  obtain ⟨hb, hl⟩ := (inv_reach h).free_empty hfree
  -- So everything handed over is what ran.
  simpa [hb, hl] using (inv_reach h).split

/-! ## Part 3. The RED cases -/

/-- **`giving_up_blind_leaves_a_section`.** The holder gives the step up
without looking at the list (bug `GiveUpBlind`). Section 1 ran; section 2
was pushed while the step was held, and waits on the list; the holder sets
the flag. Nobody holds the step and section 2 has not run: the rule of
`none_left_behind` fails. -/
theorem giving_up_blind_leaves_a_section :
    -- Section 1 ran, section 2 waits, and the step was given up all the same.
    let left : State := ⟨[1, 2], [2], [], [1], false⟩
    -- Nobody holds the step, and not everything handed over has run.
    left.held = false ∧ left.ran ≠ left.pushed := by
  -- Both are read off the lists.
  simp

/-- Sections 1 and 2 were handed over in that order, and section 2 ran
first: section 1 is still in the holder's hands. -/
def ranNewestFirst : State :=
  -- Handed over 1 then 2; an empty list; 1 in the holder's hands; 2 ran; the step held.
  ⟨[1, 2], [], [1], [2], true⟩

/-- **`newest_first_runs_out_of_order`.** The holder runs what it took
newest first (bug `NewestFirst`). Sections 1 and 2 were pushed in that
order; it runs 2. What ran is not the first of what was handed over. -/
theorem newest_first_runs_out_of_order :
    -- No rest makes what ran the start of what was handed over.
    ¬ ∃ rest, ranNewestFirst.ran ++ rest = ranNewestFirst.pushed := by
  -- Suppose there were one: then the first elements, 2 and 1, would agree.
  intro ⟨rest, h⟩
  -- They do not.
  simp [ranNewestFirst] at h

-- The end of this file's names.
end Regolith.VersionStep
