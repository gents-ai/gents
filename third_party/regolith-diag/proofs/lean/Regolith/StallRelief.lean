/-!
# StallRelief: no stopped writer waits on a relief nobody runs

This file backs the TLA+ model `proofs/tla/StallRelief.tla`. TLC checks
two writers there; here the rules are proved for any number of writers.

## The story, for a reader who has never seen the code

A write "stop" refuses writes until the background makes room. With no
background worker, a stopped writer makes room itself, with flush and
compaction steps, and for each step it needs the "gate". If another thread
holds the gate, the writer does not wait for the gate: it waits on the
stall's unit, trusting the gate's holder.

Three kinds of thread hold the gate: a stopped writer relieving the stop;
a writer whose write left one step owing, which it runs once and returns;
and a foreground job (an ingest, a checkpoint), which holds it for its
whole run.

Tiny example: writer 1 holds the gate for one step, writer 2 finds it held
and waits, writer 1's step leaves the stop on and writer 1 returns. If the
unit landed only when the stop cleared, writer 2 would wait for ever. So
every piece of progress lands the unit, stop or no stop ("relief handed
back"), and each waiter looks again. A writer that found the gate held but
registers only after such a landing would miss it, so it reads a progress
count before its step and again once registered, and lands its own unit if
the count moved. A job that held the gate relieves the stop once it
releases the gate.

Two more stories (E35). The engine "latches" when its log is left in a
state nobody can vouch for: every write fails from then on, so no stop is
relieved any more. A writer waiting on the unit would wait until the
database closes, so the latch lands the unit, and the writer, writing
again, is told the latch. The latch stores its flag before it lands the
unit, and a writer reads the flag again once registered, so a writer that
registers after the landing is told too. And a writer let in with no stop
may have to flush at the memtable cap, under the gate, before it seals: its
flush changes what the stop counts, so it counts its progress on every way
out, whether its seal went through or failed.

One more (E36). The work that relieves a stop, a flush or a compaction, can
fail, and its failure stays recorded until that work next succeeds. A write
stopped while it is recorded is not handed a wait: it is told the failure.
A writer that was stopped a moment earlier and waits on the unit would wait
on, so recording a failure lands the unit, and the writer, writing again,
is told what a fresh write is told. The failure is stored before the unit
is landed, and a writer reads it again once registered, while the stop
holds. When the relief recovers, a stopped writer waits as before.

## What is proved, in plain words

1. `no_stranded_wait`: in every state the real code reaches, once every
   writer has finished or waits, and the job is not running or still to
   relieve, nobody waits (TLA+ `NoStrandedWait`).
2. `no_wait_once_latched`: in every state the real code reaches, once the
   engine latched and its landing is done, nobody waits (TLA+
   `NoWaitOnceLatched`).
3. `no_wait_once_failing`: in every state the real code reaches, while the
   relief's failure is recorded and its landing is done, nobody waits (TLA+
   `NoWaitOnceFailing`). Its two RED runs end with a writer waiting while
   the failure is recorded: `no_fail_land_strands_a_waiter` (recording a
   failure lands nothing) and `no_fail_recheck_strands_a_waiter` (a writer
   registers without reading the recorded failure again).
4. The RED cases, each a run of the buggy steps that ends with a writer
   waiting and nobody left to land its unit:
   `no_relief_strands_a_waiter` (the job ends with no relief),
   `no_hand_back_strands_a_waiter` (progress lands the unit only when the
   stop cleared), `no_recheck_strands_a_waiter` (a contended writer
   registers without reading the count again),
   `no_rotate_refresh_strands_a_waiter` (a rotation whose seal fails counts
   no progress); and two that end with a writer waiting on a latched
   engine: `no_latch_land_strands_a_waiter` (the latch lands nothing) and
   `no_latch_recheck_strands_a_waiter` (a writer registers without reading
   the latch's flag again).

## How the writers are counted

Writers are counted by what they are doing, not named. A contended writer
is "fresh" while no progress was counted since it read the count, and
"stale" once some was: that is all the count's value is used for. A writer
whose step holds the gate is counted the same whether the step relieves a
stop, is the one a write owed, or is a rotation's flush.

## Where the steps here allow more than the model

On a latched engine the model has every write told the latch. Here a
writer may be told and finish (`told`, `owedLatched`), or go on as if
nothing had latched; only its registering is held to the latch's flag. The
same for a relief that is failing: a stopped writer may be told the failure
and finish (`toldFailing`), or take its step or be contended as if the
relief worked; only its registering is held to the recorded failure.
Every run of the model is a run of these steps, so what is proved of these
holds for the model.

## How to read the Lean

`def` defines a thing, `theorem` states a fact and its proof follows
`:= by`. Lines starting with `--` are comments, in plain words. A proof is a
list of *tactics*; each one changes the goal still to be shown, and the
comment above it says how. `simp` rewrites with known facts; `omega` solves
arithmetic over natural numbers; `cases` splits on the ways a fact could be
true; `induction` proves a fact for every number of steps.
-/

-- Everything below is named Regolith.StallRelief.<name>.
namespace Regolith.StallRelief

/-- Which code the steps follow. -/
inductive Mode where
  /-- The real code. -/
  | green
  /-- Bug: the job ends with no relief. -/
  | noRelief
  /-- Bug: progress lands the unit only when the stop cleared. -/
  | noHandBack
  /-- Bug: a contended writer registers without reading the count again. -/
  | noRecheck
  /-- Bug: the latch stores its flag and lands nothing. -/
  | noLatchLand
  /-- Bug: a writer registers without reading the latch's flag again. -/
  | noLatchRecheck
  /-- Bug: a rotation whose seal fails counts no progress. -/
  | noRotateRefresh
  /-- Bug: recording the relief's failure lands nothing. -/
  | noFailLand
  /-- Bug: a writer registers without reading the recorded failure again. -/
  | noFailRecheck
  -- Two modes can be compared for equality.
  deriving DecidableEq

/-- Who holds the gate. -/
inductive Gate where
  /-- Nobody. -/
  | free
  /-- The foreground job. -/
  | job
  /-- A writer's step: relieving, or the one it owed. -/
  | writer
  -- Two holders can be compared for equality.
  deriving DecidableEq

/-- The foreground job's progress. -/
inductive Job where
  /-- Not started. -/
  | idle
  /-- Holding the gate, doing its work. -/
  | holding
  /-- Released the gate, about to relieve the stop. -/
  | relieving
  /-- Finished. -/
  | done
  -- Two stages can be compared for equality.
  deriving DecidableEq

/-- Everything at one moment. Writers are counted by what they are doing;
finished writers are not counted. -/
structure State where
  /-- Whether a write would be stopped now. -/
  stopped : Bool
  /-- Who holds the gate. -/
  gate : Gate
  /-- The job's progress. -/
  job : Job
  /-- Writers about to write. -/
  writing : Nat
  /-- Writers whose write went in and left one step owing. -/
  owing : Nat
  /-- Writers whose step holds the gate (at most one). -/
  holding : Nat
  /-- Writers that released the gate and have yet to count their progress. -/
  refreshing : Nat
  /-- Contended writers that have missed no progress. -/
  fresh : Nat
  /-- Contended writers that have missed some progress. -/
  stale : Nat
  /-- Writers waiting on the stall's unit. -/
  waiting : Nat
  /-- Whether the engine latched: every write fails from then on. -/
  latched : Bool
  /-- Whether a latch stored its flag and has yet to land the unit. -/
  landing : Bool
  /-- Whether the relief's failure is recorded: a stopped write is told it. -/
  failing : Bool
  /-- Whether a failure was recorded and has yet to land the unit. -/
  telling : Bool
  -- Two states can be compared for equality.
  deriving DecidableEq

/-- The start with `n` writers, stopped or not: the gate free, the job not
started, every writer about to write, nothing latched, the relief working. -/
def start (n : Nat) (b : Bool) : State :=
  -- Only `writing` counts anyone, the engine takes writes, the relief works.
  ⟨b, .free, .idle, n, 0, 0, 0, 0, 0, 0, false, false, false, false⟩

/-- The unit lands: every waiter is about to write again. -/
def land (s : State) : State :=
  -- The waiters join the writers.
  { s with writing := s.writing + s.waiting, waiting := 0 }

/-- A piece of progress is counted: every contended writer has now missed
one. -/
def count (s : State) : State :=
  -- The fresh ones become stale.
  { s with stale := s.stale + s.fresh, fresh := 0 }

/-- Whether a piece of progress lands the unit, in mode `m`, with the stop
now `b`. -/
def lands : Mode → Bool → Bool
  -- The bug: only once the stop cleared.
  | .noHandBack, b => !b
  -- The real code: always.
  | _, _ => true

/-- A piece of progress in mode `m`: counted, and the unit landed as `lands`
says. -/
def progress (m : Mode) (s : State) : State :=
  -- Count first; land when `lands` says so.
  if lands m s.stopped then land (count s) else count s

/-- The steps, in mode `m`. -/
inductive Step (m : Mode) : State → State → Prop
  /-- The job takes the free gate. -/
  | jobStart (s : State) (h1 : s.job = .idle) (h2 : s.gate = .free) :
      -- It holds the gate.
      Step m s { s with gate := .job, job := .holding }
  /-- The job ends its work and releases the gate; it relieves next, or,
  in bug `noRelief`, is finished. -/
  | jobEnd (s : State) (h : s.job = .holding) :
      -- The gate is free.
      Step m s { s with gate := .free, job := if m = .noRelief then .done else .relieving }
  /-- The job's relief with the gate free: its steps leave the stop as `b`,
  and its progress is counted and lands the unit. -/
  | jobRelieveFree (s : State) (b : Bool) (h1 : s.job = .relieving) (h2 : s.gate = .free) :
      -- Finished, after its progress.
      Step m s (land (count { s with stopped := b, job := .done }))
  /-- The job's relief with a writer's step holding the gate: left to that
  writer. -/
  | jobRelieveHeld (s : State) (h1 : s.job = .relieving) (h2 : s.gate = .writer) :
      -- Only the job's progress changes.
      Step m s { s with job := .done }
  /-- A writer's write goes in: no stop. It may bring on the stop (`b`),
  and leaves one step owing. -/
  | write (s : State) (b : Bool) (h1 : 0 < s.writing) (h2 : s.stopped = false) :
      -- One writer moves from writing to owing.
      Step m s { s with stopped := b, writing := s.writing - 1, owing := s.owing + 1 }
  /-- A stopped writer finds the gate free: its step takes it. -/
  | relieve (s : State) (h1 : 0 < s.writing) (h2 : s.stopped = true) (h3 : s.gate = .free) :
      -- It holds the gate.
      Step m s { s with gate := .writer, writing := s.writing - 1, holding := s.holding + 1 }
  /-- A stopped writer reads the count, and its step finds the gate held. -/
  | contend (s : State) (h1 : 0 < s.writing) (h2 : s.stopped = true) (h3 : s.gate ≠ .free) :
      -- One more fresh contended writer.
      Step m s { s with writing := s.writing - 1, fresh := s.fresh + 1 }
  /-- An owing writer's step finds the gate free and takes it. -/
  | owedTake (s : State) (h1 : 0 < s.owing) (h2 : s.gate = .free) :
      -- It holds the gate.
      Step m s { s with gate := .writer, owing := s.owing - 1, holding := s.holding + 1 }
  /-- An owing writer's step finds the gate held: the writer returns. -/
  | owedContended (s : State) (h1 : 0 < s.owing) (h2 : s.gate ≠ .free) :
      -- It is finished.
      Step m s { s with owing := s.owing - 1 }
  /-- A writer's step ends, leaving the stop as `b`, and releases the gate. -/
  | stepEnd (s : State) (b : Bool) (h : 0 < s.holding) :
      -- The gate is free; the writer counts its progress next.
      Step m s { s with stopped := b, gate := .free, holding := s.holding - 1,
                        refreshing := s.refreshing + 1 }
  /-- A writer counts its step's progress; then it writes again
  (`back = 1`) or is finished (`back = 0`: it gave up, or its step was the
  owed one). -/
  | refresh (s : State) (back : Nat) (hb : back ≤ 1) (h : 0 < s.refreshing) :
      -- Its progress, then the writer's next place.
      Step m s (progress m { s with refreshing := s.refreshing - 1,
                                    writing := s.writing + back })
  /-- A fresh contended writer registers and finds the stop cleared: it
  counts that progress, which lands the unit, and writes again. -/
  | clearedFresh (s : State) (h1 : 0 < s.fresh) (h2 : s.stopped = false) :
      -- Counted and landed.
      Step m s (land (count { s with fresh := s.fresh - 1, writing := s.writing + 1 }))
  /-- A stale contended writer registers and finds the stop cleared: the
  same. -/
  | clearedStale (s : State) (h1 : 0 < s.stale) (h2 : s.stopped = false) :
      -- Counted and landed.
      Step m s (land (count { s with stale := s.stale - 1, writing := s.writing + 1 }))
  /-- A stale contended writer registers with the stop on: the count moved,
  so it lands the unit and writes again; in bug `noRecheck` it waits. -/
  | registerStale (s : State) (h1 : 0 < s.stale) (h2 : s.stopped = true) :
      -- Landed, or, for the bug, one more waiter.
      Step m s (if m = .noRecheck then { s with stale := s.stale - 1, waiting := s.waiting + 1 }
                else land { s with stale := s.stale - 1, writing := s.writing + 1 })
  /-- A fresh contended writer registers with the stop on: it waits. Only
  on an engine that has not latched (`hl`); bug `noLatchRecheck` does not
  look. And only while the relief's failure is not recorded (`hf`); bug
  `noFailRecheck` does not look. -/
  | registerFresh (s : State) (hl : s.latched = false ∨ m = .noLatchRecheck)
      -- The relief's failure is not recorded, or the mode is the bug's, which does not look.
      (hf : s.failing = false ∨ m = .noFailRecheck)
      -- A fresh contended writer is there, and the stop is on.
      (h1 : 0 < s.fresh) (h2 : s.stopped = true) :
      -- One more waiter.
      Step m s { s with fresh := s.fresh - 1, waiting := s.waiting + 1 }
  /-- A fresh contended writer registers on a latched engine and reads the
  latch's flag: it lands the unit and is told the latch. Not in bug
  `noLatchRecheck`. -/
  | registerLatchedFresh (s : State) (h0 : m ≠ .noLatchRecheck) (h1 : 0 < s.fresh)
      -- The engine has latched.
      (h2 : s.latched = true) :
      -- Landed, and the writer is finished.
      Step m s (land { s with fresh := s.fresh - 1 })
  /-- A stale contended writer registers on a latched engine: the same. -/
  | registerLatchedStale (s : State) (h0 : m ≠ .noLatchRecheck) (h1 : 0 < s.stale)
      -- The engine has latched.
      (h2 : s.latched = true) :
      -- Landed, and the writer is finished.
      Step m s (land { s with stale := s.stale - 1 })
  /-- A writer about to write on a latched engine is told the latch and
  returns. -/
  | told (s : State) (h1 : 0 < s.writing) (h2 : s.latched = true) :
      -- It is finished.
      Step m s { s with writing := s.writing - 1 }
  /-- An owing writer's step fails before it starts on a latched engine: the
  writer returns. -/
  | owedLatched (s : State) (h1 : 0 < s.owing) (h2 : s.latched = true) :
      -- It is finished.
      Step m s { s with owing := s.owing - 1 }
  /-- A writer let in with no stop has to rotate at the memtable cap: its
  flush takes the free gate, and the stop may come on meanwhile (`b`). -/
  | rotate (s : State) (b : Bool) (h1 : 0 < s.writing) (h2 : s.stopped = false)
      -- The gate is free.
      (h3 : s.gate = .free) :
      -- It holds the gate.
      Step m s { s with stopped := b, gate := .writer, writing := s.writing - 1,
                        holding := s.holding + 1 }
  /-- A rotation's seal goes through after its flush: the progress is
  counted, and the write goes in and leaves one step owing. -/
  | rotated (s : State) (h : 0 < s.refreshing) :
      -- Its progress, then the writer owes a step.
      Step m s (progress m { s with refreshing := s.refreshing - 1, owing := s.owing + 1 })
  /-- Bug `noRotateRefresh` only: a rotation's seal fails after its flush,
  and the writer returns with nothing counted. (The real code counts, which
  is `refresh` with the writer finished.) -/
  | rotateFailUncounted (s : State) (h0 : m = .noRotateRefresh) (h : 0 < s.refreshing) :
      -- It is finished, and nothing else changes.
      Step m s { s with refreshing := s.refreshing - 1 }
  /-- The engine latches, first half: its flag is stored. Once. -/
  | latchFlag (s : State) (h : s.latched = false) :
      -- Latched, and the landing is owed.
      Step m s { s with latched := true, landing := true }
  /-- The engine latches, second half: the unit is landed; bug `noLatchLand`
  lands nothing. -/
  | latchLand (s : State) (h : s.landing = true) :
      -- The landing is done; the waiters wake, or, for the bug, stay.
      Step m s (if m = .noLatchLand then { s with landing := false }
                else land { s with landing := false })
  /-- The relief fails, first half: its failure is stored. Only while it is
  not already recorded. -/
  | failFlag (s : State) (h : s.failing = false) :
      -- Recorded, and the landing is owed.
      Step m s { s with failing := true, telling := true }
  /-- The relief fails, second half: the unit is landed; bug `noFailLand`
  lands nothing. -/
  | failLand (s : State) (h : s.telling = true) :
      -- The landing is done; the waiters wake, or, for the bug, stay.
      Step m s (if m = .noFailLand then { s with telling := false }
                else land { s with telling := false })
  /-- The relief succeeds again: the record is cleared. A landing still
  owed stays owed. -/
  | recover (s : State) (h : s.failing = true) :
      -- No longer recorded.
      Step m s { s with failing := false }
  /-- A writer about to write is stopped while the relief's failure is
  recorded: it is told the failure and returns. -/
  | toldFailing (s : State) (h1 : 0 < s.writing) (h2 : s.stopped = true)
      -- The relief's failure is recorded.
      (h3 : s.failing = true) :
      -- It is finished.
      Step m s { s with writing := s.writing - 1 }
  /-- A fresh contended writer registers with the stop on and reads the
  recorded failure: it lands the unit and is told the failure. Not in bug
  `noFailRecheck`. -/
  | registerFailingFresh (s : State) (h0 : m ≠ .noFailRecheck) (h1 : 0 < s.fresh)
      -- The stop is on, and the relief's failure is recorded.
      (h2 : s.stopped = true) (h3 : s.failing = true) :
      -- Landed, and the writer is finished.
      Step m s (land { s with fresh := s.fresh - 1 })
  /-- A stale contended writer registers with the stop on and reads the
  recorded failure: the same. -/
  | registerFailingStale (s : State) (h0 : m ≠ .noFailRecheck) (h1 : 0 < s.stale)
      -- The stop is on, and the relief's failure is recorded.
      (h2 : s.stopped = true) (h3 : s.failing = true) :
      -- Landed, and the writer is finished.
      Step m s (land { s with stale := s.stale - 1 })

/-- The states the steps of mode `m` reach from a start. -/
inductive Reach (m : Mode) : State → Prop
  /-- A start is reachable. -/
  | start (n : Nat) (b : Bool) : Reach m (start n b)
  /-- One step from a reachable state reaches another. -/
  | step {s t : State} : Reach m s → Step m s t → Reach m t

/-- Someone is on the way to count a piece of progress: the job at work or
about to relieve, a writer holding the gate, or one about to count. -/
def Pending (s : State) : Prop :=
  -- Any of the four.
  s.job = .holding ∨ s.job = .relieving ∨ s.holding = 1 ∨ 0 < s.refreshing

/-- The facts every state the real code reaches has. -/
structure Inv (s : State) : Prop where
  /-- A writer holds the gate exactly when one writer's step does. -/
  writer_gate : (s.gate = .writer ↔ s.holding = 1) ∧ s.holding ≤ 1
  /-- The job holds the gate exactly while it does its work. -/
  job_gate : s.gate = .job ↔ s.job = .holding
  /-- A waiter waits on someone on the way. -/
  waiter_pending : 0 < s.waiting → Pending s
  /-- So does a contended writer that has missed no progress. -/
  fresh_pending : 0 < s.fresh → Pending s

/-- Every start has the facts. -/
theorem inv_start (n : Nat) (b : Bool) : Inv (start n b) := by
  -- Nobody holds the gate, nobody waits, nobody is contended.
  refine ⟨⟨?_, ?_⟩, ?_, ?_, ?_⟩ <;> simp [start]

/-- A free gate means no writer's step holds it. -/
theorem holding_zero {s : State} (hw1 : s.gate = .writer ↔ s.holding = 1)
    -- At most one writer's step holds the gate, and the gate is free.
    (hw2 : s.holding ≤ 1) (h : s.gate = .free) : s.holding = 0 := by
  -- One holding would mean a writer's gate, not a free one.
  have : s.holding ≠ 1 := fun h' => by rw [hw1.mpr h'] at h; cases h
  -- So none.
  omega

/-- Every step of the real code keeps the facts. -/
theorem inv_step {s t : State} (hs : Inv s) (hst : Step .green s t) : Inv t := by
  -- What the facts say before the step.
  obtain ⟨⟨hw1, hw2⟩, hj, hwait, hfresh⟩ := hs
  -- One case per kind of step.
  cases hst with
  -- Case: the job takes the free gate.
  | jobStart h1 h2 =>
    -- The gate was free, so no writer's step held it.
    have h0 := holding_zero hw1 hw2 h2
    -- Each fact, with the job holding the gate.
    refine ⟨⟨?_, ?_⟩, ?_, ?_, ?_⟩
    -- Not a writer's gate, and no writer holds it.
    · simp [h0]
    -- At most one.
    · simp only; omega
    -- The job's gate, and the job at work.
    · simp
    -- A waiter waits on the job at work.
    · intro _; exact Or.inl rfl
    -- So does a fresh contended writer.
    · intro _; exact Or.inl rfl
  -- Case: the job ends its work and releases the gate.
  | jobEnd h =>
    -- The job held the gate, so no writer's step did.
    have hg : s.gate = .job := hj.mpr h
    -- So no writer holds it.
    have h0 : s.holding ≠ 1 := fun h' => by rw [hw1.mpr h'] at hg; cases hg
    -- Each fact, with the gate free and the job about to relieve.
    refine ⟨⟨?_, hw2⟩, ?_, ?_, ?_⟩
    -- Not a writer's gate, and no writer holds it.
    · simp; omega
    -- Not the job's gate, and the job not at work.
    · simp
    -- A waiter waits on the job's relief.
    · intro _; exact Or.inr (Or.inl rfl)
    -- So does a fresh contended writer.
    · intro _; exact Or.inr (Or.inl rfl)
  -- Case: the job relieves, with the gate free.
  | jobRelieveFree b h1 h2 =>
    -- The gate is free, so no writer holds it.
    have h0 := holding_zero hw1 hw2 h2
    -- Each fact: the gate still free, nobody waits, nobody fresh.
    refine ⟨⟨?_, ?_⟩, ?_, ?_, ?_⟩
    -- Not a writer's gate, and no writer holds it.
    · simp [land, count, h2, h0]
    -- At most one.
    · simp only [land, count]; omega
    -- Not the job's gate, and the job finished.
    · simp [land, count, h2]
    -- Nobody waits.
    · intro h; simp [land, count] at h
    -- Nobody is fresh.
    · intro h; simp [land, count] at h
  -- Case: the job's relief finds a writer's step holding the gate.
  | jobRelieveHeld h1 h2 =>
    -- A writer's step holds the gate.
    have hrel : s.holding = 1 := hw1.mp h2
    -- Each fact: only the job changed.
    refine ⟨⟨hw1, hw2⟩, ?_, ?_, ?_⟩
    -- A writer's gate is not the job's, and the job is finished.
    · simp [h2]
    -- A waiter waits on the writer holding the gate.
    · intro _; exact Or.inr (Or.inr (Or.inl hrel))
    -- So does a fresh contended writer.
    · intro _; exact Or.inr (Or.inr (Or.inl hrel))
  -- Case: a writer's write goes in.
  | write b h1 h2 =>
    -- Only the stop and two writer counts change: the facts stay.
    exact ⟨⟨hw1, hw2⟩, hj, hwait, hfresh⟩
  -- Case: a stopped writer finds the gate free, and its step takes it.
  | relieve h1 h2 h3 =>
    -- The gate was free, so no writer's step held it.
    have h0 := holding_zero hw1 hw2 h3
    -- And the job was not at work.
    have hnj : s.job ≠ .holding := fun h => by rw [hj.mpr h] at h3; cases h3
    -- Each fact, with the writer's step holding the gate.
    refine ⟨⟨?_, ?_⟩, ?_, ?_, ?_⟩
    -- A writer's gate, and one writer holds it.
    · simp; omega
    -- At most one.
    · simp only; omega
    -- Not the job's gate, and the job not at work.
    · simp [hnj]
    -- A waiter waits on the writer holding the gate.
    · intro _; exact Or.inr (Or.inr (Or.inl (by simp only; omega)))
    -- So does a fresh contended writer.
    · intro _; exact Or.inr (Or.inr (Or.inl (by simp only; omega)))
  -- Case: a stopped writer reads the count, and its step finds the gate held.
  | contend h1 h2 h3 =>
    -- The gate is held: by the job at work, or by a writer's step.
    have hp : Pending s := by
      -- Split on who holds it.
      cases hg : s.gate with
      -- Free: ruled out by the step's condition.
      | free => exact absurd hg h3
      -- The job: it is at work.
      | job => exact Or.inl (hj.mp hg)
      -- A writer: its step holds it.
      | writer => exact Or.inr (Or.inr (Or.inl (hw1.mp hg)))
    -- Each fact: one more fresh writer, everything else as it was.
    refine ⟨⟨hw1, hw2⟩, hj, ?_, ?_⟩
    -- A waiter waits on whoever holds the gate.
    · intro _; exact hp
    -- So does the new fresh writer.
    · intro _; exact hp
  -- Case: an owing writer's step finds the gate free and takes it.
  | owedTake h1 h2 =>
    -- The gate was free, so no writer's step held it.
    have h0 := holding_zero hw1 hw2 h2
    -- And the job was not at work.
    have hnj : s.job ≠ .holding := fun h => by rw [hj.mpr h] at h2; cases h2
    -- Each fact, with the owed step holding the gate.
    refine ⟨⟨?_, ?_⟩, ?_, ?_, ?_⟩
    -- A writer's gate, and one writer holds it.
    · simp; omega
    -- At most one.
    · simp only; omega
    -- Not the job's gate, and the job not at work.
    · simp [hnj]
    -- A waiter waits on the owed step.
    · intro _; exact Or.inr (Or.inr (Or.inl (by simp only; omega)))
    -- So does a fresh contended writer.
    · intro _; exact Or.inr (Or.inr (Or.inl (by simp only; omega)))
  -- Case: an owing writer's step finds the gate held, and the writer returns.
  | owedContended h1 h2 =>
    -- Only the owing count changes: the facts stay.
    exact ⟨⟨hw1, hw2⟩, hj, hwait, hfresh⟩
  -- Case: a writer's step ends and releases the gate.
  | stepEnd b h =>
    -- A writer's step held the gate, so the job was not at work.
    have hg : s.gate = .writer := hw1.mpr (by omega)
    -- So the job was not at work.
    have hnj : s.job ≠ .holding := fun h' => by rw [hj.mpr h'] at hg; cases hg
    -- Each fact, with the gate free and one writer about to count.
    refine ⟨⟨?_, ?_⟩, ?_, ?_, ?_⟩
    -- Not a writer's gate, and no writer holds it.
    · simp; omega
    -- At most one.
    · simp only; omega
    -- Not the job's gate, and the job not at work.
    · simp [hnj]
    -- A waiter waits on the writer about to count.
    · intro _; exact Or.inr (Or.inr (Or.inr (by simp only; omega)))
    -- So does a fresh contended writer.
    · intro _; exact Or.inr (Or.inr (Or.inr (by simp only; omega)))
  -- Case: a writer counts its step's progress.
  | refresh back hb h =>
    -- Each fact: the gate as it was, nobody waits, nobody fresh.
    refine ⟨⟨?_, ?_⟩, ?_, ?_, ?_⟩
    -- The gate and the holders are as they were.
    · simp [progress, lands, land, count, hw1]
    -- At most one.
    · simp only [progress, lands, land, count, ↓reduceIte]; omega
    -- The gate and the job are as they were.
    · simp [progress, lands, land, count, hj]
    -- Nobody waits: the progress landed the unit.
    · intro h'; simp [progress, lands, land, count] at h'
    -- Nobody is fresh: the progress was counted.
    · intro h'; simp [progress, lands, land, count] at h'
  -- Case: a contended writer that missed no progress registers and finds the stop cleared.
  | clearedFresh h1 h2 =>
    -- Each fact: the gate as it was, nobody waits, nobody fresh.
    refine ⟨⟨?_, ?_⟩, ?_, ?_, ?_⟩
    -- The gate and the holders are as they were.
    · simp [land, count, hw1]
    -- At most one.
    · simp only [land, count]; omega
    -- The gate and the job are as they were.
    · simp [land, count, hj]
    -- Nobody waits.
    · intro h'; simp [land, count] at h'
    -- Nobody is fresh.
    · intro h'; simp [land, count] at h'
  -- Case: a contended writer that missed some progress registers and finds the stop cleared.
  | clearedStale h1 h2 =>
    -- Each fact: the gate as it was, nobody waits, nobody fresh.
    refine ⟨⟨?_, ?_⟩, ?_, ?_, ?_⟩
    -- The gate and the holders are as they were.
    · simp [land, count, hw1]
    -- At most one.
    · simp only [land, count]; omega
    -- The gate and the job are as they were.
    · simp [land, count, hj]
    -- Nobody waits.
    · intro h'; simp [land, count] at h'
    -- Nobody is fresh.
    · intro h'; simp [land, count] at h'
  -- Case: a contended writer that missed some progress registers with the stop on.
  | registerStale h1 h2 =>
    -- Each fact: the real code lands the unit; the fresh are as they were.
    refine ⟨⟨?_, ?_⟩, ?_, ?_, ?_⟩
    -- The gate and the holders are as they were.
    · simp [land, hw1]
    -- At most one.
    · simp only [land, reduceCtorEq, ↓reduceIte]; omega
    -- The gate and the job are as they were.
    · simp [land, hj]
    -- Nobody waits.
    · intro h'; simp [land] at h'
    -- A fresh writer still waits on whoever it did.
    · intro h'; simp only [land, reduceCtorEq, ↓reduceIte] at h' ⊢; exact hfresh h'
  -- Case: a contended writer that missed no progress registers with the stop on, and waits.
  | registerFresh hl hf h1 h2 =>
    -- It missed no progress, so someone is on the way.
    have hp : Pending s := hfresh h1
    -- Each fact: one more waiter, everything else as it was.
    refine ⟨⟨hw1, hw2⟩, hj, ?_, ?_⟩
    -- The waiters wait on that someone.
    · intro _; exact hp
    -- So do the remaining fresh writers.
    · intro _; exact hp
  -- Case: a contended writer that missed no progress registers on a latched engine.
  | registerLatchedFresh h0 h1 h2 =>
    -- It missed no progress, so someone is on the way.
    have hp : Pending s := hfresh h1
    -- Each fact: the unit landed, everything else as it was.
    refine ⟨⟨?_, ?_⟩, ?_, ?_, ?_⟩
    -- The gate and the holders are as they were.
    · simp [land, hw1]
    -- At most one.
    · simp only [land]; omega
    -- The gate and the job are as they were.
    · simp [land, hj]
    -- Nobody waits.
    · intro h'; simp [land] at h'
    -- The remaining fresh writers wait on that someone.
    · intro _; exact hp
  -- Case: a contended writer that missed some progress registers on a latched engine.
  | registerLatchedStale h0 h1 h2 =>
    -- Each fact: the unit landed, the fresh are as they were.
    refine ⟨⟨?_, ?_⟩, ?_, ?_, ?_⟩
    -- The gate and the holders are as they were.
    · simp [land, hw1]
    -- At most one.
    · simp only [land]; omega
    -- The gate and the job are as they were.
    · simp [land, hj]
    -- Nobody waits.
    · intro h'; simp [land] at h'
    -- A fresh writer still waits on whoever it did.
    · intro h'; exact hfresh h'
  -- Case: a writer about to write on a latched engine is told the latch and returns.
  | told h1 h2 =>
    -- Only the count of writers about to write changes: the facts stay.
    exact ⟨⟨hw1, hw2⟩, hj, hwait, hfresh⟩
  -- Case: an owing writer's step fails before it starts on a latched engine.
  | owedLatched h1 h2 =>
    -- Only the owing count changes: the facts stay.
    exact ⟨⟨hw1, hw2⟩, hj, hwait, hfresh⟩
  -- Case: a writer let in has to rotate, and its flush takes the free gate.
  | rotate b h1 h2 h3 =>
    -- The gate was free, so no writer's step held it.
    have h0 := holding_zero hw1 hw2 h3
    -- And the job was not at work.
    have hnj : s.job ≠ .holding := fun h => by rw [hj.mpr h] at h3; cases h3
    -- Each fact, with the rotation's flush holding the gate.
    refine ⟨⟨?_, ?_⟩, ?_, ?_, ?_⟩
    -- A writer's gate, and one writer holds it.
    · simp; omega
    -- At most one.
    · simp only; omega
    -- Not the job's gate, and the job not at work.
    · simp [hnj]
    -- A waiter waits on the writer holding the gate.
    · intro _; exact Or.inr (Or.inr (Or.inl (by simp only; omega)))
    -- So does a fresh contended writer.
    · intro _; exact Or.inr (Or.inr (Or.inl (by simp only; omega)))
  -- Case: a rotation's seal goes through, and its progress is counted.
  | rotated h =>
    -- Each fact: the gate as it was, nobody waits, nobody fresh.
    refine ⟨⟨?_, ?_⟩, ?_, ?_, ?_⟩
    -- The gate and the holders are as they were.
    · simp [progress, lands, land, count, hw1]
    -- At most one.
    · simp only [progress, lands, land, count, ↓reduceIte]; omega
    -- The gate and the job are as they were.
    · simp [progress, lands, land, count, hj]
    -- Nobody waits: the progress landed the unit.
    · intro h'; simp [progress, lands, land, count] at h'
    -- Nobody is fresh: the progress was counted.
    · intro h'; simp [progress, lands, land, count] at h'
  -- Case: a rotation's seal fails and nothing is counted, the step of bug `noRotateRefresh`.
  | rotateFailUncounted h0 h =>
    -- Not a step of the real code: the mode is not the bug's.
    exact absurd h0 (by decide)
  -- Case: the engine latches, first half: its flag is stored.
  | latchFlag h =>
    -- Only the latch's two flags change: the facts stay.
    exact ⟨⟨hw1, hw2⟩, hj, hwait, hfresh⟩
  -- Case: the engine latches, second half: the unit is landed.
  | latchLand h =>
    -- Each fact: the real code lands the unit; the fresh are as they were.
    refine ⟨⟨?_, ?_⟩, ?_, ?_, ?_⟩
    -- The gate and the holders are as they were.
    · simp [land, hw1]
    -- At most one.
    · simp only [land, reduceCtorEq, ↓reduceIte]; omega
    -- The gate and the job are as they were.
    · simp [land, hj]
    -- Nobody waits.
    · intro h'; simp [land] at h'
    -- A fresh writer still waits on whoever it did.
    · intro h'; simp only [land, reduceCtorEq, ↓reduceIte] at h' ⊢; exact hfresh h'
  -- Case: the relief fails, first half: its failure is stored.
  | failFlag h =>
    -- Only the failure's two flags change: the facts stay.
    exact ⟨⟨hw1, hw2⟩, hj, hwait, hfresh⟩
  -- Case: the relief fails, second half: the unit is landed.
  | failLand h =>
    -- Each fact: the real code lands the unit; the fresh are as they were.
    refine ⟨⟨?_, ?_⟩, ?_, ?_, ?_⟩
    -- The gate and the holders are as they were.
    · simp [land, hw1]
    -- At most one.
    · simp only [land, reduceCtorEq, ↓reduceIte]; omega
    -- The gate and the job are as they were.
    · simp [land, hj]
    -- Nobody waits.
    · intro h'; simp [land] at h'
    -- A fresh writer still waits on whoever it did.
    · intro h'; simp only [land, reduceCtorEq, ↓reduceIte] at h' ⊢; exact hfresh h'
  -- Case: the relief succeeds again, and the record is cleared.
  | recover h =>
    -- Only the failure's record changes: the facts stay.
    exact ⟨⟨hw1, hw2⟩, hj, hwait, hfresh⟩
  -- Case: a stopped writer about to write is told the recorded failure and returns.
  | toldFailing h1 h2 h3 =>
    -- Only the count of writers about to write changes: the facts stay.
    exact ⟨⟨hw1, hw2⟩, hj, hwait, hfresh⟩
  -- Case: a contended writer that missed no progress registers and reads the recorded failure.
  | registerFailingFresh h0 h1 h2 h3 =>
    -- It missed no progress, so someone is on the way.
    have hp : Pending s := hfresh h1
    -- Each fact: the unit landed, everything else as it was.
    refine ⟨⟨?_, ?_⟩, ?_, ?_, ?_⟩
    -- The gate and the holders are as they were.
    · simp [land, hw1]
    -- At most one.
    · simp only [land]; omega
    -- The gate and the job are as they were.
    · simp [land, hj]
    -- Nobody waits.
    · intro h'; simp [land] at h'
    -- The remaining fresh writers wait on that someone.
    · intro _; exact hp
  -- Case: a contended writer that missed some progress registers and reads the recorded
  -- failure.
  | registerFailingStale h0 h1 h2 h3 =>
    -- Each fact: the unit landed, the fresh are as they were.
    refine ⟨⟨?_, ?_⟩, ?_, ?_, ?_⟩
    -- The gate and the holders are as they were.
    · simp [land, hw1]
    -- At most one.
    · simp only [land]; omega
    -- The gate and the job are as they were.
    · simp [land, hj]
    -- Nobody waits.
    · intro h'; simp [land] at h'
    -- A fresh writer still waits on whoever it did.
    · intro h'; exact hfresh h'

/-- Every state the real code reaches has the facts. -/
theorem inv_reach {s : State} (h : Reach .green s) : Inv s := by
  -- By how `s` was reached.
  induction h with
  -- A start has them.
  | start n b => exact inv_start n b
  -- A step keeps them.
  | step _ hst ih => exact inv_step ih hst

/-- Nobody is left but waiters: no writer about to write, owing, holding
the gate, about to count, or contended, and the job not running or still
to relieve. -/
def OnlyWaiters (s : State) : Prop :=
  -- Every count but `waiting` is zero, and the job is idle or finished.
  s.writing = 0 ∧ s.owing = 0 ∧ s.holding = 0 ∧ s.refreshing = 0 ∧ s.fresh = 0 ∧
    s.stale = 0 ∧ s.job ≠ .holding ∧ s.job ≠ .relieving

/-- **`no_stranded_wait`.** In every state the real code reaches, once
nobody is left but waiters, nobody waits: someone was always on the way to
land the unit. Rules out the story's writer 2 waiting for ever. -/
theorem no_stranded_wait {s : State} (h : Reach .green s) (ho : OnlyWaiters s) :
    -- The statement: no writer is waiting.
    s.waiting = 0 := by
  -- The facts of the reachable state.
  have hi := inv_reach h
  -- What nobody-but-waiters says.
  obtain ⟨_, _, hh, hr, _, _, hjh, hjr⟩ := ho
  -- Either nobody waits, or somebody does.
  rcases Nat.eq_zero_or_pos s.waiting with h0 | hpos
  -- Nobody waits: that is the claim.
  · exact h0
  -- Somebody waits: then someone is on the way, and each kind is ruled out.
  · rcases hi.waiter_pending hpos with h' | h' | h' | h'
    -- Not the job at work.
    · exact absurd h' hjh
    -- Not the job about to relieve.
    · exact absurd h' hjr
    -- No writer holds the gate.
    · omega
    -- No writer is about to count.
    · omega

/-- The latch's fact: once the engine latched and its landing is done,
nobody waits. -/
def Settled (s : State) : Prop :=
  -- Latched, the landing done: no waiter.
  s.latched = true → s.landing = false → s.waiting = 0

/-- Every start has the latch's fact: nothing latched. -/
theorem settled_start (n : Nat) (b : Bool) : Settled (start n b) := by
  -- The start is not latched, so there is nothing to show.
  intro h; simp [start] at h

/-- Every step of the real code keeps the latch's fact. The only steps that
add a waiter are a registration, which the real code makes on an engine
that has not latched; the latch's own landing empties the unit. -/
theorem settled_step {s t : State} (hs : Settled s) (hst : Step .green s t) :
    -- The statement: the latch's fact holds after the step.
    Settled t := by
  -- One case per kind of step.
  cases hst with
  -- The job's steps leave the latch and the waiters alone.
  | jobStart h1 h2 => exact hs
  -- The job's end: the same.
  | jobEnd h => exact hs
  -- The job's relief lands the unit: nobody waits.
  | jobRelieveFree b h1 h2 => intro _ _; simp [land, count]
  -- The job's relief left to a writer: no waiter is added.
  | jobRelieveHeld h1 h2 => exact hs
  -- A write, a relieving step, a contended step: no waiter is added.
  | write b h1 h2 => exact hs
  -- A relieving step's start: the same.
  | relieve h1 h2 h3 => exact hs
  -- A contended step: the writer is not on the unit yet, so the same.
  | contend h1 h2 h3 => exact hs
  -- The owed step's start: no waiter is added.
  | owedTake h1 h2 => exact hs
  -- The owed step contended: the writer returns, and no waiter is added.
  | owedContended h1 h2 => exact hs
  -- A step's end: no waiter is added.
  | stepEnd b h => exact hs
  -- A piece of progress lands the unit: nobody waits.
  | refresh back hb h => intro _ _; simp [progress, lands, land, count]
  -- A registration that finds the stop cleared lands the unit.
  | clearedFresh h1 h2 => intro _ _; simp [land, count]
  -- The same for a stale one.
  | clearedStale h1 h2 => intro _ _; simp [land, count]
  -- A stale registration lands the unit in the real code.
  | registerStale h1 h2 => intro _ _; simp [land]
  -- A fresh registration adds a waiter, and only on an engine not latched.
  | registerFresh hl hf h1 h2 =>
    -- Suppose the engine is latched after the step: it was before it.
    intro hlatched _
    -- The step's condition says it was not, or the mode is the bug's.
    rcases hl with hl | hl
    -- Not latched and latched: impossible.
    · rw [hl] at hlatched; cases hlatched
    -- The real code is not the bug.
    · cases hl
  -- A registration on a latched engine lands the unit.
  | registerLatchedFresh h0 h1 h2 => intro _ _; simp [land]
  -- The same for a stale one.
  | registerLatchedStale h0 h1 h2 => intro _ _; simp [land]
  -- A writer told the latch, an owed step that fails: no waiter is added.
  | told h1 h2 => exact hs
  -- An owed step that fails on a latched engine: no waiter is added.
  | owedLatched h1 h2 => exact hs
  -- A rotation's start: no waiter is added.
  | rotate b h1 h2 h3 => exact hs
  -- A rotation's progress lands the unit.
  | rotated h => intro _ _; simp [progress, lands, land, count]
  -- Not a step of the real code.
  | rotateFailUncounted h0 h => exact absurd h0 (by decide)
  -- The flag is stored and the landing owed: nothing to show yet.
  | latchFlag h => intro _ hlanding; cases hlanding
  -- The landing: the unit is emptied.
  | latchLand h => intro _ _; simp [land]
  -- The relief's failure is recorded or cleared: no waiter is added.
  | failFlag h => exact hs
  -- The record cleared: the same.
  | recover h => exact hs
  -- The failure's landing empties the unit.
  | failLand h => intro _ _; simp [land]
  -- A writer told the failure: no waiter is added.
  | toldFailing h1 h2 h3 => exact hs
  -- A registration that reads the recorded failure lands the unit.
  | registerFailingFresh h0 h1 h2 h3 => intro _ _; simp [land]
  -- The same for a stale one.
  | registerFailingStale h0 h1 h2 h3 => intro _ _; simp [land]

/-- Every state the real code reaches has the latch's fact. -/
theorem settled_reach {s : State} (h : Reach .green s) : Settled s := by
  -- By how `s` was reached.
  induction h with
  -- A start has it.
  | start n b => exact settled_start n b
  -- A step keeps it.
  | step _ hst ih => exact settled_step ih hst

/-- **`no_wait_once_latched`.** In every state the real code reaches, once
the engine latched and its landing is done, nobody waits: a latched engine
clears no stop, so a writer still waiting then would wait until the
database closes. -/
theorem no_wait_once_latched {s : State} (h : Reach .green s) (hl : s.latched = true)
    -- Its landing is done. The statement: no writer is waiting.
    (hd : s.landing = false) : s.waiting = 0 :=
  -- The latch's fact of the reachable state, applied.
  settled_reach h hl hd

/-- The failing relief's fact: while the relief's failure is recorded and
its landing is done, nobody waits. -/
def Told (s : State) : Prop :=
  -- Recorded, the landing done: no waiter.
  s.failing = true → s.telling = false → s.waiting = 0

/-- Every start has the failing relief's fact: nothing is recorded. -/
theorem told_start (n : Nat) (b : Bool) : Told (start n b) := by
  -- The start records no failure, so there is nothing to show.
  intro h; simp [start] at h

/-- Every step of the real code keeps the failing relief's fact. The only
step that adds a waiter is a fresh registration, which the real code makes
while no failure is recorded; the failure's own landing empties the unit;
and a recovery clears the record, after which the fact asks nothing. -/
theorem told_step {s t : State} (hs : Told s) (hst : Step .green s t) : Told t := by
  -- One case per kind of step.
  cases hst with
  -- The job's steps leave the record and the waiters alone.
  | jobStart h1 h2 => exact hs
  -- The job's end: the same.
  | jobEnd h => exact hs
  -- The job's relief lands the unit: nobody waits.
  | jobRelieveFree b h1 h2 => intro _ _; simp [land, count]
  -- The job's relief left to a writer: no waiter is added.
  | jobRelieveHeld h1 h2 => exact hs
  -- A write, a relieving step, a contended step: no waiter is added.
  | write b h1 h2 => exact hs
  -- A relieving step's start: the same.
  | relieve h1 h2 h3 => exact hs
  -- A contended step: the writer is not on the unit yet, so the same.
  | contend h1 h2 h3 => exact hs
  -- The owed step's start: no waiter is added.
  | owedTake h1 h2 => exact hs
  -- The owed step contended: the writer returns, and no waiter is added.
  | owedContended h1 h2 => exact hs
  -- A step's end: no waiter is added.
  | stepEnd b h => exact hs
  -- A piece of progress lands the unit: nobody waits.
  | refresh back hb h => intro _ _; simp [progress, lands, land, count]
  -- A registration that finds the stop cleared lands the unit.
  | clearedFresh h1 h2 => intro _ _; simp [land, count]
  -- The same for a stale one.
  | clearedStale h1 h2 => intro _ _; simp [land, count]
  -- A stale registration lands the unit in the real code.
  | registerStale h1 h2 => intro _ _; simp [land]
  -- A fresh registration adds a waiter, and only while nothing is recorded.
  | registerFresh hl hf h1 h2 =>
    -- Suppose the failure is recorded after the step: it was before it.
    intro hfailing _
    -- The step's condition says it was not, or the mode is the bug's.
    rcases hf with hf | hf
    -- Not recorded and recorded: impossible.
    · rw [hf] at hfailing; cases hfailing
    -- The real code is not the bug.
    · cases hf
  -- A registration on a latched engine lands the unit.
  | registerLatchedFresh h0 h1 h2 => intro _ _; simp [land]
  -- The same for a stale one.
  | registerLatchedStale h0 h1 h2 => intro _ _; simp [land]
  -- A writer told the latch, an owed step that fails: no waiter is added.
  | told h1 h2 => exact hs
  -- An owed step that fails on a latched engine: no waiter is added.
  | owedLatched h1 h2 => exact hs
  -- A rotation's start: no waiter is added.
  | rotate b h1 h2 h3 => exact hs
  -- A rotation's progress lands the unit.
  | rotated h => intro _ _; simp [progress, lands, land, count]
  -- Not a step of the real code.
  | rotateFailUncounted h0 h => exact absurd h0 (by decide)
  -- The latch's flag: the record and the waiters are as they were.
  | latchFlag h => exact hs
  -- The latch's landing empties the unit.
  | latchLand h => intro _ _; simp [land]
  -- The failure is stored and the landing owed: nothing to show yet.
  | failFlag h => intro _ htelling; cases htelling
  -- The landing: the unit is emptied.
  | failLand h => intro _ _; simp [land]
  -- The record is cleared: the fact asks nothing.
  | recover h => intro hfailing _; cases hfailing
  -- A writer told the failure: no waiter is added.
  | toldFailing h1 h2 h3 => exact hs
  -- A registration that reads the recorded failure lands the unit.
  | registerFailingFresh h0 h1 h2 h3 => intro _ _; simp [land]
  -- The same for a stale one.
  | registerFailingStale h0 h1 h2 h3 => intro _ _; simp [land]

/-- Every state the real code reaches has the failing relief's fact. -/
theorem told_reach {s : State} (h : Reach .green s) : Told s := by
  -- By how `s` was reached.
  induction h with
  -- A start has it.
  | start n b => exact told_start n b
  -- A step keeps it.
  | step _ hst ih => exact told_step ih hst

/-- **`no_wait_once_failing`.** In every state the real code reaches, while
the relief's failure is recorded and its landing is done, nobody waits: a
write that arrives then is told the failure, so one that was already
waiting has been told it too. -/
theorem no_wait_once_failing {s : State} (h : Reach .green s) (hf : s.failing = true)
    -- Its landing is done. The statement: no writer is waiting.
    (hd : s.telling = false) : s.waiting = 0 :=
  -- The failing relief's fact of the reachable state, applied.
  told_reach h hf hd

/-- **`no_fail_land_strands_a_waiter`.** Bug `noFailLand`: the stop is on;
writer 1's relieving step takes the gate; writer 2 finds it held and waits;
the relief fails, its failure is recorded, and nothing is landed. Writer 2
waits while a fresh write would be told the failure. -/
theorem no_fail_land_strands_a_waiter :
    -- Some state the buggy steps reach has the failure recorded, its landing done, and a waiter.
    ∃ s, Reach .noFailLand s ∧ s.failing = true ∧ s.telling = false ∧ s.waiting = 1 := by
  -- The run, step by step from two writers and the stop on.
  have h0 := Reach.start (m := .noFailLand) 2 true
  -- Writer 1's step takes the gate.
  have h1 := Reach.step h0 (Step.relieve _ (by decide) rfl rfl)
  -- Writer 2 is stopped and contended.
  have h2 := Reach.step h1 (Step.contend _ (by decide) rfl (by decide))
  -- It registers, with nothing latched and nothing recorded, and waits.
  have h3 := Reach.step h2 (Step.registerFresh _ (Or.inl rfl) (Or.inl rfl) (by decide) rfl)
  -- The relief fails: its failure is recorded.
  have h4 := Reach.step h3 (Step.failFlag _ rfl)
  -- Its landing lands nothing.
  have h5 := Reach.step h4 (Step.failLand _ rfl)
  -- That last state has the failure recorded, with a waiter.
  exact ⟨_, h5, rfl, rfl, rfl⟩

/-- **`no_fail_recheck_strands_a_waiter`.** Bug `noFailRecheck`: the stop
is on; writer 1's relieving step holds the gate; writer 2 finds it held; the
relief fails, record and landing, with writer 2 not on the unit yet; writer
2 registers without reading the record, and waits. -/
theorem no_fail_recheck_strands_a_waiter :
    -- Some state the buggy steps reach has the failure recorded, its landing done, and a waiter.
    ∃ s, Reach .noFailRecheck s ∧ s.failing = true ∧ s.telling = false ∧ s.waiting = 1 := by
  -- The run, step by step from two writers and the stop on.
  have h0 := Reach.start (m := .noFailRecheck) 2 true
  -- Writer 1's step takes the gate.
  have h1 := Reach.step h0 (Step.relieve _ (by decide) rfl rfl)
  -- Writer 2 is stopped and contended.
  have h2 := Reach.step h1 (Step.contend _ (by decide) rfl (by decide))
  -- The relief fails: its failure is recorded.
  have h3 := Reach.step h2 (Step.failFlag _ rfl)
  -- Its landing finds nobody on the unit.
  have h4 := Reach.step h3 (Step.failLand _ rfl)
  -- Writer 2 registers without reading the record, and waits.
  have h5 := Reach.step h4 (Step.registerFresh _ (Or.inl rfl) (Or.inr rfl) (by decide) rfl)
  -- That last state has the failure recorded, with a waiter.
  exact ⟨_, h5, rfl, rfl, rfl⟩

/-- **`no_latch_land_strands_a_waiter`.** Bug `noLatchLand`: the stop is
on; writer 1's relieving step takes the gate; writer 2 finds it held and
waits; the engine latches and lands nothing. Writer 2 waits on a latched
engine. -/
theorem no_latch_land_strands_a_waiter :
    -- Some state the buggy steps reach is latched, its landing done, with a waiter.
    ∃ s, Reach .noLatchLand s ∧ s.latched = true ∧ s.landing = false ∧ s.waiting = 1 := by
  -- The run, step by step from two writers and the stop on.
  have h0 := Reach.start (m := .noLatchLand) 2 true
  -- Writer 1's step takes the gate.
  have h1 := Reach.step h0 (Step.relieve _ (by decide) rfl rfl)
  -- Writer 2 is stopped and contended.
  have h2 := Reach.step h1 (Step.contend _ (by decide) rfl (by decide))
  -- It registers, on an engine not latched, and waits.
  have h3 := Reach.step h2 (Step.registerFresh _ (Or.inl rfl) (Or.inl rfl) (by decide) rfl)
  -- The engine latches: its flag is stored.
  have h4 := Reach.step h3 (Step.latchFlag _ rfl)
  -- Its landing lands nothing.
  have h5 := Reach.step h4 (Step.latchLand _ rfl)
  -- That last state is latched, with a waiter.
  exact ⟨_, h5, rfl, rfl, rfl⟩

/-- **`no_latch_recheck_strands_a_waiter`.** Bug `noLatchRecheck`: the stop
is on; writer 1's relieving step holds the gate; writer 2 finds it held;
the engine latches, flag and landing, with writer 2 not on the unit yet;
writer 2 registers without reading the flag, and waits on a latched
engine. -/
theorem no_latch_recheck_strands_a_waiter :
    -- Some state the buggy steps reach is latched, its landing done, with a waiter.
    ∃ s, Reach .noLatchRecheck s ∧ s.latched = true ∧ s.landing = false ∧ s.waiting = 1 := by
  -- The run, step by step from two writers and the stop on.
  have h0 := Reach.start (m := .noLatchRecheck) 2 true
  -- Writer 1's step takes the gate.
  have h1 := Reach.step h0 (Step.relieve _ (by decide) rfl rfl)
  -- Writer 2 is stopped and contended.
  have h2 := Reach.step h1 (Step.contend _ (by decide) rfl (by decide))
  -- The engine latches: its flag is stored.
  have h3 := Reach.step h2 (Step.latchFlag _ rfl)
  -- Its landing finds nobody on the unit.
  have h4 := Reach.step h3 (Step.latchLand _ rfl)
  -- Writer 2 registers without reading the flag, and waits.
  have h5 := Reach.step h4 (Step.registerFresh _ (Or.inr rfl) (Or.inl rfl) (by decide) rfl)
  -- That last state is latched, with a waiter.
  exact ⟨_, h5, rfl, rfl, rfl⟩

/-- **`no_rotate_refresh_strands_a_waiter`.** Bug `noRotateRefresh`: no
stop; writer 1's write is let in and has to rotate: its flush takes the
gate, and the stop comes on; writer 2 finds the gate held and waits; writer
1's flush ends with the stop on, its seal fails, and it returns with
nothing counted. Nobody is left but the waiter. -/
theorem no_rotate_refresh_strands_a_waiter :
    -- Some state the buggy steps reach has only waiters, and one.
    ∃ s, Reach .noRotateRefresh s ∧ OnlyWaiters s ∧ s.waiting = 1 := by
  -- The run, step by step from two writers and no stop.
  have h0 := Reach.start (m := .noRotateRefresh) 2 false
  -- Writer 1's rotation takes the gate, and the stop comes on.
  have h1 := Reach.step h0 (Step.rotate _ true (by decide) rfl rfl)
  -- Writer 2 is stopped and contended.
  have h2 := Reach.step h1 (Step.contend _ (by decide) rfl (by decide))
  -- It registers and waits.
  have h3 := Reach.step h2 (Step.registerFresh _ (Or.inl rfl) (Or.inl rfl) (by decide) rfl)
  -- Writer 1's flush ends with the stop still on.
  have h4 := Reach.step h3 (Step.stepEnd _ true (by decide))
  -- Its seal fails, and it returns with nothing counted.
  have h5 := Reach.step h4 (Step.rotateFailUncounted _ rfl (by decide))
  -- That last state has only a waiter.
  exact ⟨_, h5, by unfold OnlyWaiters; decide, rfl⟩

/-- **`no_relief_strands_a_waiter`.** Bug `noRelief`: the stop is on; the
job takes the gate; the one writer finds it held and waits; the job ends
with no relief. Nobody is left but the waiter. -/
theorem no_relief_strands_a_waiter :
    -- Some state the buggy steps reach has only waiters, and one.
    ∃ s, Reach .noRelief s ∧ OnlyWaiters s ∧ s.waiting = 1 := by
  -- The run, step by step from one writer and the stop on.
  have h0 := Reach.start (m := .noRelief) 1 true
  -- The job takes the gate.
  have h1 := Reach.step h0 (Step.jobStart _ rfl rfl)
  -- The writer is stopped and contended.
  have h2 := Reach.step h1 (Step.contend _ (by decide) rfl (by decide))
  -- It registers and waits.
  have h3 := Reach.step h2 (Step.registerFresh _ (Or.inl rfl) (Or.inl rfl) (by decide) rfl)
  -- The job ends, with no relief.
  have h4 := Reach.step h3 (Step.jobEnd _ rfl)
  -- That last state has only a waiter.
  exact ⟨_, h4, by unfold OnlyWaiters; decide, rfl⟩

/-- **`no_hand_back_strands_a_waiter`.** Bug `noHandBack`: the stop is on;
writer 1's relieving step takes the gate; writer 2 finds it held and waits;
writer 1's step leaves the stop on, its progress lands nothing, and writer
1 gives up. Nobody is left but the waiter. -/
theorem no_hand_back_strands_a_waiter :
    -- Some state the buggy steps reach has only waiters, and one.
    ∃ s, Reach .noHandBack s ∧ OnlyWaiters s ∧ s.waiting = 1 := by
  -- The run, step by step from two writers and the stop on.
  have h0 := Reach.start (m := .noHandBack) 2 true
  -- Writer 1's step takes the gate.
  have h1 := Reach.step h0 (Step.relieve _ (by decide) rfl rfl)
  -- Writer 2 is stopped and contended.
  have h2 := Reach.step h1 (Step.contend _ (by decide) rfl (by decide))
  -- It registers and waits.
  have h3 := Reach.step h2 (Step.registerFresh _ (Or.inl rfl) (Or.inl rfl) (by decide) rfl)
  -- Writer 1's step ends with the stop still on.
  have h4 := Reach.step h3 (Step.stepEnd _ true (by decide))
  -- Writer 1 counts its progress, which lands nothing, and gives up.
  have h5 := Reach.step h4 (Step.refresh _ 0 (by decide) (by decide))
  -- That last state has only a waiter.
  exact ⟨_, h5, by unfold OnlyWaiters; decide, rfl⟩

/-- **`no_recheck_strands_a_waiter`.** Bug `noRecheck`: the stop is on;
writer 1's relieving step holds the gate; writer 2 reads the count and
finds the gate held; writer 1's step ends, its progress lands the unit with
nobody on it, and writer 1 gives up; writer 2 registers and waits. Nobody is
left but the waiter. -/
theorem no_recheck_strands_a_waiter :
    -- Some state the buggy steps reach has only waiters, and one.
    ∃ s, Reach .noRecheck s ∧ OnlyWaiters s ∧ s.waiting = 1 := by
  -- The run, step by step from two writers and the stop on.
  have h0 := Reach.start (m := .noRecheck) 2 true
  -- Writer 1's step takes the gate.
  have h1 := Reach.step h0 (Step.relieve _ (by decide) rfl rfl)
  -- Writer 2 is stopped and contended.
  have h2 := Reach.step h1 (Step.contend _ (by decide) rfl (by decide))
  -- Writer 1's step ends with the stop still on.
  have h3 := Reach.step h2 (Step.stepEnd _ true (by decide))
  -- Writer 1 counts its progress, landing nobody, and gives up.
  have h4 := Reach.step h3 (Step.refresh _ 0 (by decide) (by decide))
  -- Writer 2, stale now, registers without looking at the count, and waits.
  have h5 := Reach.step h4 (Step.registerStale _ (by decide) rfl)
  -- That last state has only a waiter.
  exact ⟨_, h5, by unfold OnlyWaiters; decide, rfl⟩

-- The end of this file's names.
end Regolith.StallRelief
