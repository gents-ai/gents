/-!
# SnapshotWait: waiting for the last snapshot loses no wake, and never waits alone (E15)

This file backs the TLA+ model `proofs/tla/SnapshotWait.tla`, invariants
`NoLostWake`, `NeverWaitsAlone` and `ReportsStillPinned` (configurations
`MC_SnapshotWait_*`). The model checks two pins and two calls; this file
proves the laws for any number of pins, releases and calls.

## The story, with a tiny example

A reader of the database can take a "snapshot": a promise that it keeps
seeing the data as it was at one moment. Each live snapshot holds a "pin".
`Db::wait_for_snapshots` hands back a `SnapshotWait` at once; that handle's
`wait` waits until no pin is held and reports how many still are when its
timeout runs out
(`SnapshotRegistry::wait_until_drained`, `src/engine/snapshot_registry.rs`).

There is no lock. The waiter and a release talk through three things:

* the count of pins still held (the bits of one word);
* a "watched" mark the waiter leaves on that word in the same atomic step
  in which it reads the bits (`Chunk::watch`,
  `src/engine/snapshot_registry/chain.rs`);
* a generation number (`Notify`, `src/sync/notify.rs`): the waiter writes
  it down first, and later goes to sleep only if it has not moved; a
  release that found the mark raises it and wakes a sleeper.

Example: one pin is held. The waiter writes down generation 0, marks, and
sees the pin. The owner releases the pin: it sees the mark, so it raises the
generation to 1 and wakes the waiter. If the waiter was asleep it is woken;
if it had not gone to sleep yet, it sees 1 instead of 0 and does not sleep.
Either way it looks again and returns 0.

What could go wrong: the waiter falls asleep exactly as the last pin goes,
nobody wakes it, and it sleeps for its whole timeout with nothing held. And
on a target with one thread (a wasm build without threads) a sleeping
waiter could never be woken at all, because the release would have to run
on the thread that sleeps.

## What is proved, in plain words

1. `step_inv` and `reachable_inv`: the waiting protocol keeps its invariant.
2. `no_lost_wake`: in every reachable state, a waiter that is asleep and
   not woken still has a pin held or a release on its way to wake it. So it
   is never left asleep with nothing held and nobody coming.
3. `one_thread_never_waits`: on a target with one thread the call returns
   the number of pins held and changes nothing: it never enters the wait.
4. `future_after_watch_loses_wake` and `read_then_mark_loses_wake`: do
   either of the waiter's two steps in the wrong order and one pin, one
   release and one waiter end with the waiter asleep, unwoken, and nothing
   held. These are the RED configurations as concrete counterexamples.

How to read the Lean: `def` defines a function or a property, `theorem`
states a fact and its proof follows `:= by`. Lines starting with `--` are
comments. `{ s with a := x }` is the state `s` with its field `a` set to `x`.
-/

-- Everything below is named Regolith.SnapshotWait.<name>.
namespace Regolith.SnapshotWait

/-! ## The state -/

/-- Where the waiter is. -/
inductive Phase where
  /-- Not in a call. -/
  | out
  /-- In the wait, at the top of a round. -/
  | counted
  /-- It wrote the generation down. -/
  | armed
  /-- It marked the word, read the bits and saw a pin held. -/
  | looked
  /-- Asleep. -/
  | parked
  -- Lean may decide whether two phases are equal, so the examples at the end can be computed.
  deriving DecidableEq

/-- Everything the waiter and the releases share, and where the waiter is. -/
structure St where
  /-- How many pins are still held (bits set in `Chunk::taken`). -/
  taken : Nat
  /-- How many releases cleared their bit, saw the mark, and have yet to
  notify (`SnapshotRegistry::freed` not yet run). -/
  owing : Nat
  /-- The watched mark (`WATCHED`), never cleared once set. -/
  watched : Bool
  /-- Whether the waiter is counted in (`SnapshotRegistry::watchers > 0`). -/
  inside : Bool
  /-- The notification's generation. -/
  gen : Nat
  /-- The generation the waiter wrote down. -/
  wgen : Nat
  /-- Where the waiter is. -/
  phase : Phase
  /-- A release has woken the sleeping waiter, which has not run again yet. -/
  woken : Bool

/-- The start: `n` pins held, nobody waiting, nothing marked. -/
def init (n : Nat) : St := ⟨n, 0, false, false, 0, 0, .out, false⟩

/-! ## The steps -/

/-- The design's steps, each one atomic. -/
inductive Step : St → St → Prop
  /-- The program calls the wait (on a target with threads): the waiter
  counts itself in. -/
  | call (s : St)
      -- The waiter is not in a call.
      (h : s.phase = .out) :
      -- After the step: counted in, at the top of a round.
      Step s { s with inside := true, phase := .counted }
  /-- A round begins: the waiter writes the generation down. -/
  | arm (s : St)
      -- The waiter is at the top of a round.
      (h : s.phase = .counted) :
      -- After the step: the generation is written down.
      Step s { s with wgen := s.gen, phase := .armed }
  /-- The look finds nothing held: the call returns 0. -/
  | watchEmpty (s : St)
      -- The waiter wrote the generation down.
      (h : s.phase = .armed)
      -- No pin is held.
      (h0 : s.taken = 0) :
      -- After the step: the mark is left, and the waiter is out of the wait.
      Step s { s with watched := true, inside := false, phase := .out }
  /-- The look finds a pin held: the mark is left in the same step. -/
  | watchHeld (s : St)
      -- The waiter wrote the generation down.
      (h : s.phase = .armed)
      -- A pin is held.
      (hpos : 0 < s.taken) :
      -- After the step: the mark is left, and the waiter has looked.
      Step s { s with watched := true, phase := .looked }
  /-- The waiter finds the generation moved: no sleep, a new round. -/
  | pollReady (s : St)
      -- The waiter looked and saw a pin.
      (h : s.phase = .looked)
      -- The generation is no longer the one written down.
      (hg : s.gen ≠ s.wgen) :
      -- After the step: at the top of a new round.
      Step s { s with phase := .counted }
  /-- The waiter finds the generation unchanged and goes to sleep. -/
  | pollPark (s : St)
      -- The waiter looked and saw a pin.
      (h : s.phase = .looked)
      -- The generation is still the one written down.
      (hg : s.gen = s.wgen) :
      -- After the step: asleep.
      Step s { s with phase := .parked }
  /-- A woken waiter runs again. -/
  | wake (s : St)
      -- The waiter is asleep ...
      (h : s.phase = .parked)
      -- ... and was woken.
      (hw : s.woken = true) :
      -- After the step: the wake is used up, and a new round starts.
      Step s { s with woken := false, phase := .counted }
  /-- The deadline passes: one more look, and the call returns. -/
  | timeOut (s : St)
      -- The waiter is asleep.
      (h : s.phase = .parked) :
      -- After the step: the mark is there, and the waiter is out of the wait.
      Step s { s with watched := true, inside := false, woken := false, phase := .out }
  /-- A release clears its bit and sees the mark: it owes a notification. -/
  | unpinSeen (s : St)
      -- A pin is held.
      (hpos : 0 < s.taken)
      -- The mark is there.
      (hm : s.watched = true) :
      -- After the step: one pin fewer, one notification owed.
      Step s { s with taken := s.taken - 1, owing := s.owing + 1 }
  /-- A release clears its bit and sees no mark: it tells nobody. -/
  | unpinUnseen (s : St)
      -- A pin is held.
      (hpos : 0 < s.taken)
      -- No mark is there.
      (hm : s.watched = false) :
      -- After the step: one pin fewer.
      Step s { s with taken := s.taken - 1 }
  /-- A release that saw the mark finds a waiter counted in: it raises the
  generation and wakes a sleeping waiter. -/
  | tell (s : St)
      -- A notification is owed.
      (hpos : 0 < s.owing)
      -- A waiter is counted in.
      (hin : s.inside = true) :
      -- After the step: the generation is one higher, and a sleeping waiter is woken.
      Step s { s with owing := s.owing - 1, gen := s.gen + 1,
                      -- (woken already, or asleep now)
                      woken := s.woken || decide (s.phase = .parked) }
  /-- A release that saw the mark finds no waiter counted in: it tells
  nobody. -/
  | tellNobody (s : St)
      -- A notification is owed.
      (hpos : 0 < s.owing)
      -- No waiter is counted in.
      (hout : s.inside = false) :
      -- After the step: nothing is owed for that release.
      Step s { s with owing := s.owing - 1 }

/-- The states the protocol can reach from `n` held pins. -/
inductive Reachable (n : Nat) : St → Prop
  /-- The start is reachable. -/
  | init : Reachable n (init n)
  /-- One step from a reachable state reaches another. -/
  | step {s t : St} : Reachable n s → Step s t → Reachable n t

/-! ## The invariant -/

/-- What holds in every reachable state. The last field is the heart: while
the waiter has looked, the generation has not moved and nobody woke it,
something is still pending that will move the generation. -/
structure Inv (s : St) : Prop where
  /-- A waiter that has looked left its mark. -/
  marked : s.phase = .looked ∨ s.phase = .parked → s.watched = true
  /-- A waiter in a call is counted in. -/
  counted_in : s.phase ≠ .out → s.inside = true
  /-- Only a sleeping waiter is woken. -/
  woken_parked : s.woken = true → s.phase = .parked
  /-- The generation written down is not ahead of the real one. -/
  wgen_le : s.phase = .armed ∨ s.phase = .looked ∨ s.phase = .parked → s.wgen ≤ s.gen
  /-- If the generation moved while the waiter slept, it was woken. -/
  moved_woken : s.phase = .parked → s.gen ≠ s.wgen → s.woken = true
  /-- While the waiter has looked, the generation has not moved and nobody
  woke it, a pin is still held or a notification is still owed. -/
  pending : s.phase = .looked ∨ s.phase = .parked → s.gen = s.wgen → s.woken = false →
      0 < s.taken + s.owing

/-- A waiter that is not asleep is not woken. -/
theorem not_woken {s : St}
    -- Only a sleeping waiter is woken.
    (h4 : s.woken = true → s.phase = .parked)
    -- The waiter is not asleep.
    (hp : s.phase ≠ .parked) :
    -- The statement: it is not woken.
    s.woken = false := by
  -- Look at the flag.
  cases hw : s.woken with
  -- Not woken: that is the claim.
  | false => rfl
  -- Woken: then it would be asleep, and it is not.
  | true => exact absurd (h4 hw) hp

/-- The start satisfies the invariant. -/
theorem inv_init (n : Nat) : Inv (init n) := by
  -- The waiter is out and nothing is woken: every field holds on its face.
  refine ⟨?_, ?_, ?_, ?_, ?_, ?_⟩ <;> simp [init]

/-- **Every step keeps the invariant.** -/
theorem step_inv {s t : St}
    -- The invariant holds before the step.
    (hs : Inv s)
    -- One step of the design.
    (hst : Step s t) :
    -- The statement: the invariant holds after the step.
    Inv t := by
  -- Take the invariant apart into its six facts.
  obtain ⟨h1, h3, h4, h5, h6, hj⟩ := hs
  -- Split on the kind of step.
  cases hst with
  -- Case: the call. The waiter is counted in and at the top of a round.
  | call h =>
    -- It was out, so not asleep, so not woken.
    have hw : s.woken = false := not_woken h4 (by simp [h])
    -- "Counted" is none of the phases the other facts speak of; it is counted in; not woken.
    exact ⟨by simp, by simp, by simp [hw], by simp, by simp, by simp⟩
  -- Case: the waiter writes the generation down.
  | arm h =>
    -- It was at the top of a round, so not asleep, so not woken.
    have hw : s.woken = false := not_woken h4 (by simp [h])
    -- Still counted in; not woken; the generation written down is the real one, so not ahead.
    exact ⟨by simp, fun _ => h3 (by simp [h]), by simp [hw], fun _ => Nat.le_refl _, by simp,
      -- (It has not looked yet, so the last fact asks nothing.)
      by simp⟩
  -- Case: the look finds nothing held, and the call returns.
  | watchEmpty h h0 =>
    -- It was armed, so not asleep, so not woken.
    have hw : s.woken = false := not_woken h4 (by simp [h])
    -- The waiter is out: every fact about a waiter in a call asks nothing.
    exact ⟨by simp, by simp, by simp [hw], by simp, by simp, by simp⟩
  -- Case: the look finds a pin held.
  | watchHeld h hpos =>
    -- It was armed, so not asleep, so not woken.
    have hw : s.woken = false := not_woken h4 (by simp [h])
    -- The mark was just left; still counted in; not woken; the written generation as before;
    exact ⟨by simp, fun _ => h3 (by simp [h]), by simp [hw], fun _ => h5 (Or.inl h), by simp,
      -- and a pin is held, so something is pending.
      fun _ _ _ => Nat.lt_of_lt_of_le hpos (Nat.le_add_right _ _)⟩
  -- Case: the generation moved, so the waiter starts a new round.
  | pollReady h hg =>
    -- It had looked, so not asleep, so not woken.
    have hw : s.woken = false := not_woken h4 (by simp [h])
    -- At the top of a round: still counted in, not woken, and the other facts ask nothing.
    exact ⟨by simp, fun _ => h3 (by simp [h]), by simp [hw], by simp, by simp, by simp⟩
  -- Case: the generation did not move, and the waiter goes to sleep.
  | pollPark h hg =>
    -- The mark is there since the look; still counted in; a woken waiter is asleep, as it is now;
    exact ⟨fun _ => h1 (Or.inl h), fun _ => h3 (by simp [h]), by simp,
      -- the written generation as before; the generation has not moved, so nothing to show there;
      fun _ => h5 (Or.inr (Or.inl h)), fun _ hne => absurd hg hne,
      -- and what was pending when it had looked is still pending.
      fun _ he hwf => hj (Or.inl h) he hwf⟩
  -- Case: a woken waiter runs again.
  | wake h hwk =>
    -- At the top of a round: still counted in, the wake is used up, the other facts ask nothing.
    exact ⟨by simp, fun _ => h3 (by simp [h]), by simp, by simp, by simp, by simp⟩
  -- Case: the deadline passes and the call returns.
  | timeOut h =>
    -- The waiter is out and not woken: every fact holds on its face.
    exact ⟨by simp, by simp, by simp, by simp, by simp, by simp⟩
  -- Case: a release clears its bit and sees the mark.
  | unpinSeen hpos hm =>
    -- The waiter's side is untouched: five facts are as before; the last is left.
    refine ⟨h1, h3, h4, h5, h6, fun hp he hwf => ?_⟩
    -- Something was pending before the step.
    have hold := hj hp he hwf
    -- One pin fewer and one notification more: the sum is the same.
    show 0 < (s.taken - 1) + (s.owing + 1)
    -- Arithmetic.
    omega
  -- Case: a release clears its bit and sees no mark.
  | unpinUnseen hpos hm =>
    -- No mark means the waiter has not looked, so the last fact asks nothing; the rest is as before.
    exact ⟨h1, h3, h4, h5, h6, fun hp _ _ => absurd (h1 hp) (by simp [hm])⟩
  -- Case: a release notifies: the generation goes up and a sleeper is woken.
  | tell hpos hin =>
    -- The mark and the count are untouched, and the generation written down, at most the old
    -- generation, is at most the new one. Three facts are left.
    refine ⟨h1, h3, ?_, fun hp => Nat.le_succ_of_le (h5 hp), ?_, fun hp he _ => ?_⟩
    -- A woken waiter is asleep: take the new flag apart.
    · intro hh
      -- It was woken before, or it is asleep now.
      simp only [Bool.or_eq_true, decide_eq_true_eq] at hh
      -- Split on which.
      rcases hh with hh | hh
      -- Woken before: then it was asleep, and still is.
      · exact h4 hh
      -- Asleep now: that is the claim.
      · exact hh
    -- The generation moved while the waiter slept: the step itself woke it.
    · intro hp _
      -- The waiter is asleep (the step did not change where it is).
      have hp' : s.phase = .parked := hp
      -- So the new flag, "woken before or asleep now", is true.
      simp [hp']
    -- The last fact: its condition cannot hold.
    · have hle := h5 (Or.inr hp)
      -- The condition says the new generation is the one written down.
      have he' : s.gen + 1 = s.wgen := he
      -- But the one written down is at most the old generation.
      omega
  -- Case: a release that saw the mark finds no waiter counted in.
  | tellNobody hpos hout =>
    -- Then the waiter is out: a waiter in a call is counted in.
    have hpo : s.phase = .out := Classical.byContradiction fun hne => by
      -- Otherwise it would be counted in,
      have hin := h3 hne
      -- and it is not.
      rw [hout] at hin
      -- "false = true" is impossible.
      cases hin
    -- Five facts are as before; the last asks nothing of a waiter that is out.
    exact ⟨h1, h3, h4, h5, h6, fun hp _ _ => by
      -- The waiter would have looked or be asleep,
      rcases hp with hp | hp <;>
      -- but it is out.
      (rw [hpo] at hp; cases hp)⟩

/-- **Every reachable state satisfies the invariant.** -/
theorem reachable_inv {n : Nat} {s : St}
    -- `s` is reachable from `n` held pins.
    (h : Reachable n s) :
    -- The statement: the invariant holds in `s`.
    Inv s := by
  -- Go through the steps that reached `s`, one at a time.
  induction h with
  -- The start satisfies it.
  | init => exact inv_init n
  -- One more step keeps it.
  | step _ hst ih => exact step_inv ih hst

/-! ## The laws -/

/-- **`no_lost_wake`.** In every reachable state, a waiter that is asleep
and not woken still has a pin held or a release on its way to notify. So
the waiter is never left asleep with nothing held and nobody coming: the
release that drains the last pin wakes it. TLA+: `NoLostWake`. Example ruled
out: the last pin is released just as the waiter goes to sleep, and the
shutdown that waits for snapshots hangs for its whole timeout. -/
theorem no_lost_wake {n : Nat} {s : St}
    -- `s` is reachable from `n` held pins.
    (h : Reachable n s)
    -- The waiter is asleep ...
    (hp : s.phase = .parked)
    -- ... and not woken.
    (hw : s.woken = false) :
    -- The statement: a pin is held, or a notification is owed.
    0 < s.taken + s.owing := by
  -- The invariant holds in `s`.
  have hi := reachable_inv h
  -- Did the generation move since the waiter wrote it down?
  by_cases hg : s.gen = s.wgen
  -- No: then something is pending, by the invariant's last fact.
  · exact hi.pending (Or.inr hp) hg hw
  -- Yes: then the waiter was woken, and it is not.
  · have hwk := hi.moved_woken hp hg
    -- "woken = true" and "woken = false" together.
    rw [hw] at hwk
    -- Impossible.
    cases hwk

/-- What the call does first. On a target with other threads the waiter
counts itself in and nothing is returned yet; on a target with one thread
the call returns the number of pins held and touches nothing
(`SnapshotRegistry::wait_until_drained_on`). -/
def enter (threads : Bool) (s : St) : Option Nat × St :=
  -- With threads: no answer yet, and the waiter is counted in.
  if threads then (none, { s with inside := true, phase := .counted })
  -- With one thread: the count of pins held, and the state as it was.
  else (some s.taken, s)

/-- **`one_thread_never_waits`.** On a target with one thread the call
answers at once with the number of pins held, changes nothing, and leaves
the waiter where it was, never asleep. TLA+: `NeverWaitsAlone`,
`ReportsStillPinned`. Example ruled out: the one thread holds a snapshot,
calls the wait, and sleeps, with nobody else to release the snapshot. -/
theorem one_thread_never_waits (s : St)
    -- The waiter is not in a call.
    (h : s.phase = .out) :
    -- The statement: the answer is the count of pins held; the state is unchanged;
    (enter false s).1 = some s.taken ∧ (enter false s).2 = s ∧
      -- and the waiter is not asleep.
      (enter false s).2.phase ≠ .parked := by
  -- Unfold the call on a target with one thread; "out" is not "parked".
  simp [enter, h]

/-! ## The RED cases, as counterexamples

Each runs one waiter and one release of one pin, as plain functions applied
one after the other, with one of the waiter's steps out of place. -/

/-- The waiter counts itself in. -/
def opCall (s : St) : St :=
  -- Counted in, at the top of a round.
  { s with inside := true, phase := .counted }

/-- The waiter writes the generation down. -/
def opArm (s : St) : St :=
  -- The written generation is the current one.
  { s with wgen := s.gen }

/-- The waiter reads the bits and sees a pin held. -/
def opRead (s : St) : St :=
  -- It has looked.
  { s with phase := .looked }

/-- The waiter leaves its mark. -/
def opMark (s : St) : St :=
  -- The mark is there.
  { s with watched := true }

/-- The waiter polls: asleep if the generation did not move. -/
def opPoll (s : St) : St :=
  -- Parked when the generation is the one written down, else a new round.
  { s with phase := if s.gen = s.wgen then .parked else .counted }

/-- A release clears its bit, and owes a notification if it sees the mark. -/
def opUnpin (s : St) : St :=
  -- One pin fewer; one more owed only when the mark is there.
  { s with taken := s.taken - 1, owing := if s.watched then s.owing + 1 else s.owing }

/-- A release that owes a notification gives it. -/
def opTell (s : St) : St :=
  -- With something owed and a waiter counted in:
  if 0 < s.owing ∧ s.inside then
    -- the generation goes up and a sleeper is woken.
    { s with owing := s.owing - 1, gen := s.gen + 1,
             -- (woken already, or asleep now)
             woken := s.woken || decide (s.phase = .parked) }
  -- Otherwise nothing happens.
  else s

/-- The design's order: write the generation down, then mark and read in
one step, then poll. -/
def goodRun : St :=
  -- call; arm; mark and read; the release and its notification; poll.
  opPoll (opTell (opUnpin (opRead (opMark (opArm (opCall (init 1)))))))

/-- The bug `FutureAfterWatch`: the generation is written down after the
look, here after the release has already notified. -/
def lateArmRun : St :=
  -- call; mark and read; the release and its notification; only now arm; poll.
  opPoll (opArm (opTell (opUnpin (opRead (opMark (opCall (init 1)))))))

/-- The bug `ReadThenMark`: the bits are read first, and the mark is left
after the release has already cleared its bit. -/
def lateMarkRun : St :=
  -- call; arm; read; the release (no mark, so it owes nothing); only now mark; poll.
  opPoll (opTell (opMark (opUnpin (opRead (opArm (opCall (init 1)))))))

/-- With the design's order, the same release leaves the waiter awake: the
generation moved, so the poll starts a new round. -/
theorem good_order_stays_awake :
    -- The statement: after the run the waiter is at the top of a round, not asleep.
    goodRun.phase = .counted := by
  -- Settled by computing the run.
  decide

/-- **`future_after_watch_loses_wake`.** One pin, one release, one waiter
that writes the generation down too late: the waiter is asleep and not
woken, with nothing held and no notification owed. TLA+:
`MC_SnapshotWait_Red_FutureAfterWatch` breaks `NoLostWake`. -/
theorem future_after_watch_loses_wake :
    -- The statement: asleep, not woken,
    lateArmRun.phase = .parked ∧ lateArmRun.woken = false ∧
      -- nothing held and nothing owed.
      lateArmRun.taken + lateArmRun.owing = 0 := by
  -- Settled by computing the run.
  decide

/-- **`read_then_mark_loses_wake`.** One pin, one release, one waiter that
leaves its mark too late: the release saw no mark and told nobody, and the
waiter is asleep and not woken, with nothing held and no notification owed.
TLA+: `MC_SnapshotWait_Red_ReadThenMark` breaks `NoLostWake`. -/
theorem read_then_mark_loses_wake :
    -- The statement: asleep, not woken,
    lateMarkRun.phase = .parked ∧ lateMarkRun.woken = false ∧
      -- nothing held and nothing owed.
      lateMarkRun.taken + lateMarkRun.owing = 0 := by
  -- Settled by computing the run.
  decide

-- End of the namespace opened at the top.
end Regolith.SnapshotWait
