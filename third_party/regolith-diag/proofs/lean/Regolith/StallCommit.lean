/-!
# StallCommit: no commit is lost to a write stall

This file backs the commit rows of the TLA+ model `proofs/tla/StallRelief.tla`
(`NoLostCommit`). TLC checks one commit beside one plain writer there; here
the rule is proved for one commit's call against anything at all the rest of
the database does.

## The story, for a reader who has never seen the code

A write "stop" refuses writes until the background makes room. A plain write
that a stop holds can be handed a wait and return: its caller still has the
data and writes again once the stop clears. A transaction's commit cannot be
made a second time, so it must never be handed that wait, and it must never
give up while it is making room itself. It may end in two ways only: its
write went in, or it was told a failure that says the write cannot be made
(the engine latched, or the work that makes room keeps failing).

Tiny example of what goes wrong without the rule: a commit meets a stop, is
handed the wait as a plain write would be, and returns. Its transaction is
over, nothing of it was written, and nothing said the stop would never clear.
The caller's work is lost to a stall.

On a database with no background worker and inline compaction turned off,
a plain write is handed the wait at once, and its caller makes room. A
commit is not: nobody else is certain to make room, so it does so itself
(RegolithEngine::admit_commit, src/engine/mod.rs).

## How the rest is modelled

The commit's call is one small state machine. Everything else (whether a
write is stopped, whether another thread holds the gate, whether the engine
latched, whether the work that makes room is failing, whether inline
compaction is off) is a `World` chosen afresh at every step. So what is
proved holds whatever the other writers, the background job and the engine
do. That a commit left waiting is not left waiting for ever is
`no_stranded_wait` in `StallRelief.lean`: a waiting commit is a waiting
writer there.

## What is proved, in plain words

1. `no_lost_commit`: in the real code, every place a commit's call can reach
   is one that has not ended, or one where its write went in, or one where it
   was told a failure. Never handed the wait, never given up. The worlds
   it holds against include those with inline compaction off, where a plain
   write is handed the wait.
2. The RED cases, each a run of the buggy steps that ends with the commit
   lost: `takes_wait_loses_a_commit` (handed the wait where a writer would
   wait), `gives_up_loses_a_commit` (gives up at a plain writer's bound),
   `left_to_caller_loses_a_commit` (handed the wait with inline compaction
   off, as a plain write is).

## How to read the Lean

`def` defines a thing, `theorem` states a fact and its proof follows `:= by`.
Lines starting with `--` are comments, in plain words. A proof is a list of
*tactics*; each one changes the goal still to be shown, and the comment
above it says how.
-/

-- Everything below is named Regolith.StallCommit.<name>.
namespace Regolith.StallCommit

/-- Which code the commit's steps follow. -/
inductive Mode where
  /-- The real code. -/
  | green
  /-- Bug: a commit the stall would leave waiting is handed the wait. -/
  | takesWait
  /-- Bug: a commit gives up at a plain writer's bound, its steps making
  progress and the stop still on. -/
  | givesUp
  /-- Bug: with inline compaction off, a commit is handed the wait as a
  plain write is. -/
  | leftToCaller
  -- Two modes can be compared for equality.
  deriving DecidableEq

/-- What the commit sees of the rest of the database at one step. -/
structure World where
  /-- Whether a write would be stopped now. -/
  stopped : Bool
  /-- Whether nobody holds the gate the steps that make room need. -/
  gateFree : Bool
  /-- Whether the engine latched: every write fails. -/
  latched : Bool
  /-- Whether the work that makes room keeps failing. -/
  failing : Bool
  /-- Whether inline compaction is off on a database with no worker. -/
  inlineOff : Bool

/-- Where the commit's call is. -/
inductive Call where
  /-- About to write: the stall is looked at. -/
  | writing
  /-- Its own step holds the gate, making room. -/
  | relieving
  /-- Its step released the gate; it counts the step's progress next. -/
  | refreshing
  /-- It found the gate held and is about to register on the stall's unit. -/
  | contended
  /-- It waits on the stall's unit, inside its call. -/
  | waiting
  /-- Ended: its write went in. -/
  | inWrite
  /-- Ended: told a failure that says the write cannot be made. -/
  | told
  /-- Ended: gave up with the stop on. -/
  | busy
  /-- Ended: handed the wait, nothing written. -/
  | handedWait
  -- Two places can be compared for equality.
  deriving DecidableEq

/-- Whether a call that is here lost the commit to the stall. -/
def Call.lost : Call → Bool
  -- Given up with the stop on: lost.
  | .busy => true
  -- Handed the wait: lost.
  | .handedWait => true
  -- Anywhere else: not lost.
  | _ => false

/-- One step of the commit's call, in mode `m`, against what it sees. -/
inductive Step (m : Mode) : Call → Call → Prop
  /-- No stop, no latch: the write goes in. -/
  | write (w : World) (h1 : w.stopped = false) (h2 : w.latched = false) :
      -- The call ends with its write in.
      Step m .writing .inWrite
  /-- The engine latched: told the latch. -/
  | toldLatched (w : World) (h : w.latched = true) :
      -- The call ends told.
      Step m .writing .told
  /-- Stopped while the work that makes room is failing: told that failure. -/
  | toldFailing (w : World) (h1 : w.stopped = true) (h2 : w.failing = true) :
      -- The call ends told.
      Step m .writing .told
  /-- Stopped, the gate free: its own step takes it, inline compaction off
  or not. -/
  | relieve (w : World) (h1 : w.stopped = true) (h2 : w.gateFree = true) :
      -- It makes room.
      Step m .writing .relieving
  /-- Stopped, the gate held: it reads the count and is contended. -/
  | contend (w : World) (h1 : w.stopped = true) (h2 : w.gateFree = false) :
      -- About to register.
      Step m .writing .contended
  /-- Bug `leftToCaller` only: with inline compaction off a stopped commit
  is handed the wait. -/
  | leftToCaller (w : World) (h0 : m = .leftToCaller) (h1 : w.inlineOff = true)
      -- The stop is on.
      (h2 : w.stopped = true) :
      -- The call ends with the wait.
      Step m .writing .handedWait
  /-- Its step ends and releases the gate. -/
  | stepEnd :
      -- It counts the progress next.
      Step m .relieving .refreshing
  /-- It counts its progress and looks at the stall again. -/
  | again :
      -- Back to the stall.
      Step m .refreshing .writing
  /-- Bug `givesUp` only: it gives up with the stop on. -/
  | giveUp (w : World) (h0 : m = .givesUp) (h1 : w.stopped = true) :
      -- The call ends busy.
      Step m .refreshing .busy
  /-- It registers on the stall's unit and waits, inside its call. -/
  | register :
      -- Waiting.
      Step m .contended .waiting
  /-- Bug `takesWait` only: where it would wait, it is handed the wait. -/
  | consumed (h0 : m = .takesWait) :
      -- The call ends with the wait.
      Step m .contended .handedWait
  /-- It registers and reads the latch or the recorded failure: told. -/
  | registerTold (w : World) (h : w.latched = true ∨ w.failing = true) :
      -- The call ends told.
      Step m .contended .told
  /-- It registers and finds the stop cleared, or progress it missed: it
  looks at the stall again. -/
  | registerAgain :
      -- Back to the stall.
      Step m .contended .writing
  /-- The unit lands: it looks at the stall again. -/
  | wake :
      -- Back to the stall.
      Step m .waiting .writing

/-- The places the steps of mode `m` reach from a call about to write. -/
inductive Reach (m : Mode) : Call → Prop
  /-- A call starts about to write. -/
  | start : Reach m .writing
  /-- One step from a reachable place reaches another. -/
  | step {c d : Call} : Reach m c → Step m c d → Reach m d

/-- **`no_lost_commit`.** In the real code a commit's call never ends
handed the wait and never ends given up, whatever the rest of the database
does at each step. -/
theorem no_lost_commit {c : Call} (h : Reach .green c) : c.lost = false := by
  -- By induction on the run that reached the place.
  induction h with
  -- The start: about to write, not lost.
  | start => rfl
  -- One more step: look at which step it was.
  | step _ hst _ =>
    -- Every real-code step ends somewhere not lost; each bug step needs a mode the run is not in.
    cases hst <;> first | rfl | contradiction

/-- **`takes_wait_loses_a_commit`.** Bug `takesWait`: the commit finds the
gate held, and where it would wait it is handed the wait. -/
theorem takes_wait_loses_a_commit : ∃ c, Reach .takesWait c ∧ c.lost = true := by
  -- Stopped, the gate held: contended.
  have h1 := Reach.step (Reach.start (m := .takesWait))
    -- The world: stopped, the gate held.
    (Step.contend ⟨true, false, false, false, false⟩ rfl rfl)
  -- Handed the wait.
  have h2 := Reach.step h1 (Step.consumed rfl)
  -- That place lost the commit.
  exact ⟨_, h2, rfl⟩

/-- **`gives_up_loses_a_commit`.** Bug `givesUp`: the commit makes room,
its step leaves the stop on, and it gives up. -/
theorem gives_up_loses_a_commit : ∃ c, Reach .givesUp c ∧ c.lost = true := by
  -- Stopped, the gate free: its step takes it.
  have h1 := Reach.step (Reach.start (m := .givesUp))
    -- The world: stopped, the gate free.
    (Step.relieve ⟨true, true, false, false, false⟩ rfl rfl)
  -- The step ends.
  have h2 := Reach.step h1 Step.stepEnd
  -- With the stop still on, it gives up.
  have h3 := Reach.step h2 (Step.giveUp ⟨true, true, false, false, false⟩ rfl rfl)
  -- That place lost the commit.
  exact ⟨_, h3, rfl⟩

/-- **`left_to_caller_loses_a_commit`.** Bug `leftToCaller`: with inline
compaction off, the stopped commit is handed the wait as a plain write is. -/
theorem left_to_caller_loses_a_commit : ∃ c, Reach .leftToCaller c ∧ c.lost = true := by
  -- Stopped with inline compaction off: handed the wait.
  have h1 := Reach.step (Reach.start (m := .leftToCaller))
    -- The world: stopped, inline compaction off.
    (Step.leftToCaller ⟨true, true, false, false, true⟩ rfl rfl rfl)
  -- That place lost the commit.
  exact ⟨_, h1, rfl⟩

-- The end of this file's names.
end Regolith.StallCommit
