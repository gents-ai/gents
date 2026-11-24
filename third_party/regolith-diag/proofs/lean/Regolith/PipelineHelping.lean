/-!
# PipelineHelping: a stopped thread holds nobody up

This file backs the TLA+ model `proofs/tla/CommitPipeline.tla`, the safety
half of its liveness properties `LockFree` and `EveryLiveCommitCompletes`,
its invariant `NoLiveThreadBlocked`, and its RED `StopHoldsClaim`. The
sequence numbering and the publication order of the same pipeline are in
`Regolith/Pipeline.lean`.

## The story

The commit pipeline keeps commits in a ring of slots (`src/engine/commit`).
Think of a bakery with a ticket machine: every commit pulls a numbered
ticket, and whoever stands at the counter does the next step of the next
order, its own or somebody else's. So a customer who wanders off holds
nobody up: the next one finishes their order for them.

A slot goes through stages. Each step that moves it one stage on may be
taken by any thread whose own slot is the same one or a later one. Each
step has an ordering rule, and every rule looks back only: at the slots
before this one, and at whether the round (the one claim on the log's
device step) is free. None looks at a later slot, and none names a thread.

Tiny example: thread A pulled ticket 1 and stopped for good before writing
its order down. Thread B, at ticket 2, finds ticket 1 empty and throws it
away (that settles it), passes it, and then takes its own commit through
every stage. B never waits for A.

## What is proved, for every number of slots and every mix of stages

1. `first_unfinished_can_step`: with every earlier slot finished and the
   round free, whatever step a slot needs next may be taken.
2. `step_advances`, `no_step_goes_back`: a step moves a slot exactly one
   stage on; no step of any thread moves any slot back.
3. `helper_step_makes_progress`: while work is left, the next step of the
   first unfinished slot is enabled, and taking it leaves exactly one step
   fewer to take.
4. `helper_alone_finishes`: one thread running alone finishes every slot
   up to its own, wherever every other thread stopped: the safety half of
   lock-freedom.
5. `helping_is_bounded` (E21): that takes at most eight steps a slot, so no
   thread serves an unbounded queue of others.
6. `held_round_blocks`: the one exception, stated and proved. A thread that
   stops while it holds the round stops the commits that wait for the
   sync, which is why the pipeline treats the holder as doing device I/O.
7. `run_step_counts`, `run_never_goes_back`: records reach the log in runs
   (`src/engine/commit/write_unit.rs`): one step of one thread, the holder
   of the write unit, takes the write step of several slots at once. That
   step moves each slot it writes one stage on and no slot back, and
   leaves exactly as many steps fewer as it wrote records. So the safety
   half of lock-freedom stands with runs: a run is many helper steps taken
   together, and the count of steps left only falls.
8. `a_park_is_bounded`, `patience_bounds_the_wait`: a thread whose record
   is in another thread's hands (the unit's holder, or an owner that has
   not encoded its record yet) parks, and after a bounded number of its own
   steps it writes the record itself, whatever every other thread does,
   the holder that stopped for good included. Wakes from other threads only
   shorten that (`wakes_only_shorten`). So the write step of the first
   unfinished slot is always within reach of one live thread, and
   `helper_alone_finishes` keeps its meaning. `without_patience_waits_for_a_wake`
   says why the patience is there: without it a parked thread needs another
   thread to wake it (the TLA+ RED `LostWake`).

The rule that changed: a thread writes the records up to its own, and a run
it writes as the unit's holder may reach past its own, by one run at most,
bounded in records and bytes (P5 in `pipeline.rs`, WU6 in `write_unit.rs`);
`helping_is_bounded` bounds the steps for the slots up to its own, and the
run past it adds one step.

What stays in TLA+: that the interleavings of several threads keep
finishing slots (the liveness half). TLC checks it for two and three
threads.

How to read the Lean: `def` defines a function or a property, `theorem`
states a fact and its proof follows `:= by`. Lines starting with `--` are
comments. Inside a proof, each step is commented with what it establishes.
-/

-- Everything below is named Regolith.PipelineHelping.<name>.
namespace Regolith.PipelineHelping

/-! ## The stages of a slot, and the steps between them -/

/-- What kind of order a slot holds. -/
inductive Kind where
  /-- Nothing: the ticket was pulled and its owner went no further. -/
  | empty
  /-- A commit that does not wait for the disk. -/
  | eventual
  /-- A commit that waits for its record to be durable. -/
  | immediate
  /-- A slot that will land nothing: it loses a conflict or is refused. -/
  | abort
  /-- An ordered unit: an ingest's install, a rotation, a close. -/
  | unit
  -- Two such values can be compared for equality.
  deriving DecidableEq

/-- How far a slot has come, in the order the stages are passed. -/
inductive Stage where
  /-- The ticket is pulled; no order is in the cell. -/
  | reserved
  /-- The order is in the cell; it has no verdict. -/
  | ready
  /-- The verdict is installed. -/
  | decided
  /-- Its writes are in the memtable. -/
  | applied
  /-- The tip moved to its verdict. -/
  | tipped
  /-- The decided frontier passed it. -/
  | settled
  /-- Its record is in the log. -/
  | written
  /-- Its record is durable. -/
  | durable
  /-- The read horizon reached its sequence. -/
  | shown
  /-- The published frontier passed it: its owner's call returns. -/
  | finished
  -- Two such values can be compared for equality.
  deriving DecidableEq

/-- The stages as numbers, so that "further on" is "greater". -/
def rank : Stage → Nat
  -- A ticket with no order comes first.
  | .reserved => 0
  -- Then the order is in the cell.
  | .ready => 1
  -- Then it has its verdict.
  | .decided => 2
  -- Then its writes are in the memtable.
  | .applied => 3
  -- Then the tip is at its verdict.
  | .tipped => 4
  -- Then the decided frontier is past it.
  | .settled => 5
  -- Then its record is in the log.
  | .written => 6
  -- Then the record is durable.
  | .durable => 7
  -- Then readers see it.
  | .shown => 8
  -- Then the published frontier is past it.
  | .finished => 9

/-- The steps of the pipeline: what a thread does to a slot. -/
inductive Step where
  /-- Throw away a ticket nobody filled. -/
  | void
  /-- Work out the verdict and install it. -/
  | decide
  /-- Put the writes into the memtable. -/
  | apply
  /-- Move the tip to the verdict. -/
  | moveTip
  /-- Pass the decided frontier over the slot. -/
  | pass
  /-- Write the record at its place in the log. -/
  | write
  /-- Claim the round and sync the written prefix. -/
  | sync
  /-- Raise the read horizon to the slot's sequence. -/
  | raiseHorizon
  /-- Pass the published frontier over the slot. -/
  | passFrontier
  /-- Claim and run an ordered unit. -/
  | runUnit
  -- Two such values can be compared for equality.
  deriving DecidableEq

/-- One slot: what it holds and how far it has come. -/
structure Cell where
  /-- What kind of order it holds. -/
  kind : Kind
  /-- How far it has come. -/
  stage : Stage
  -- Two such values can be compared for equality.
  deriving DecidableEq

/-- The next step of a slot and the stage it leads to, or `none` when the
slot is finished (or its stage is one its kind never passes through). -/
def next (c : Cell) : Option (Step × Stage) :=
  -- By what the slot holds and how far it has come.
  match c.kind, c.stage with
  -- An empty ticket is thrown away, which settles it...
  | .empty, .reserved => some (.void, .settled)
  -- ...and then the published frontier passes it.
  | .empty, .settled => some (.passFrontier, .finished)
  -- A commit that does not wait for the disk gets its verdict...
  | .eventual, .ready => some (.decide, .decided)
  -- ...its writes go into the memtable...
  | .eventual, .decided => some (.apply, .applied)
  -- ...the tip moves to it...
  | .eventual, .applied => some (.moveTip, .tipped)
  -- ...the decided frontier passes it...
  | .eventual, .tipped => some (.pass, .settled)
  -- ...its record is written...
  | .eventual, .settled => some (.write, .written)
  -- ...readers are shown it...
  | .eventual, .written => some (.raiseHorizon, .shown)
  -- ...and the published frontier passes it.
  | .eventual, .shown => some (.passFrontier, .finished)
  -- A commit that waits for the disk gets its verdict...
  | .immediate, .ready => some (.decide, .decided)
  -- ...its writes go into the memtable...
  | .immediate, .decided => some (.apply, .applied)
  -- ...the tip moves to it...
  | .immediate, .applied => some (.moveTip, .tipped)
  -- ...the decided frontier passes it...
  | .immediate, .tipped => some (.pass, .settled)
  -- ...its record is written...
  | .immediate, .settled => some (.write, .written)
  -- ...and synced, which is the step that needs the round...
  | .immediate, .written => some (.sync, .durable)
  -- ...readers are shown it...
  | .immediate, .durable => some (.raiseHorizon, .shown)
  -- ...and the published frontier passes it.
  | .immediate, .shown => some (.passFrontier, .finished)
  -- An abort gets its verdict...
  | .abort, .ready => some (.decide, .decided)
  -- ...the tip moves to it, with nothing to apply...
  | .abort, .decided => some (.moveTip, .tipped)
  -- ...the decided frontier passes it...
  | .abort, .tipped => some (.pass, .settled)
  -- ...and the published frontier passes it, with nothing to write.
  | .abort, .settled => some (.passFrontier, .finished)
  -- An ordered unit is run, which decides and settles it...
  | .unit, .ready => some (.runUnit, .settled)
  -- ...readers are shown what it installed...
  | .unit, .settled => some (.raiseHorizon, .shown)
  -- ...and the published frontier passes it.
  | .unit, .shown => some (.passFrontier, .finished)
  -- Anything else: finished, or not a stage of that kind.
  | _, _ => none

/-- How many steps a slot still needs. -/
def todo (c : Cell) : Nat :=
  -- By what the slot holds and how far it has come.
  match c.kind, c.stage with
  -- An empty ticket: void, then pass.
  | .empty, .reserved => 2
  -- Voided: pass.
  | .empty, .settled => 1
  -- A commit that does not wait for the disk needs seven steps in all.
  | .eventual, .ready => 7
  -- Decided: six left.
  | .eventual, .decided => 6
  -- Applied: five left.
  | .eventual, .applied => 5
  -- The tip moved: four left.
  | .eventual, .tipped => 4
  -- Settled: write, show, pass.
  | .eventual, .settled => 3
  -- Written: show, pass.
  | .eventual, .written => 2
  -- Shown: pass.
  | .eventual, .shown => 1
  -- A commit that waits for the disk needs eight.
  | .immediate, .ready => 8
  -- Decided: seven left.
  | .immediate, .decided => 7
  -- Applied: six left.
  | .immediate, .applied => 6
  -- The tip moved: five left.
  | .immediate, .tipped => 5
  -- Settled: write, sync, show, pass.
  | .immediate, .settled => 4
  -- Written: sync, show, pass.
  | .immediate, .written => 3
  -- Durable: show, pass.
  | .immediate, .durable => 2
  -- Shown: pass.
  | .immediate, .shown => 1
  -- An abort needs four.
  | .abort, .ready => 4
  -- Decided: tip, frontier, pass.
  | .abort, .decided => 3
  -- The tip moved: frontier, pass.
  | .abort, .tipped => 2
  -- Settled: pass.
  | .abort, .settled => 1
  -- An ordered unit needs three.
  | .unit, .ready => 3
  -- Run: show, pass.
  | .unit, .settled => 2
  -- Shown: pass.
  | .unit, .shown => 1
  -- Anything else has nothing left.
  | _, _ => 0

/-- A slot whose stage is one its kind passes through. -/
def WellFormed (c : Cell) : Prop := c.stage = .finished ∨ (next c).isSome = true

/-- Whether a step waits for every earlier slot to be settled (the decided
frontier stands just before this slot). -/
def needsDecided : Step → Bool
  -- Voiding, deciding, applying, the tip and the frontier are all done to
  -- the next slot in line.
  | .void | .decide | .apply | .moveTip | .pass => true
  -- A record is written at its own place, whatever its neighbours do, and
  -- a sync asks nothing of the other slots.
  | .write | .sync => false
  -- Publishing and a unit ask more: see `needsPublished`.
  | .raiseHorizon | .passFrontier | .runUnit => false

/-- Whether a step waits for every earlier slot to be finished (the
published frontier stands just before this slot). -/
def needsPublished : Step → Bool
  -- Publishing goes in ring order, and a unit runs with the pipeline
  -- drained.
  | .raiseHorizon | .passFrontier | .runUnit => true
  -- No other step waits for publication.
  | .void | .decide | .apply | .moveTip | .pass | .write | .sync => false

/-- Whether a step needs the round: the one claim on the log's device
step. -/
def needsRound : Step → Bool
  -- A sync is the device step, and a unit holds the round for the length
  -- of its body.
  | .sync | .runUnit => true
  -- No other step touches the device step.
  | .void | .decide | .apply | .moveTip | .pass | .write | .raiseHorizon | .passFrontier => false

/-- When a step may be taken on a slot, given the slots before it in the
ring and whether the round is free. Nothing here mentions a later slot, and
nothing names a thread: any thread whose own slot is this one or a later
one may take the step. -/
def Enabled (roundFree : Bool) (before : List Cell) (step : Step) : Prop :=
  -- Every earlier slot is settled, if the step waits for that.
  (needsDecided step = true → ∀ b ∈ before, rank .settled ≤ rank b.stage) ∧
  -- Every earlier slot is finished, if the step waits for that.
  (needsPublished step = true → ∀ b ∈ before, b.stage = .finished) ∧
  -- The round is free, if the step needs it.
  (needsRound step = true → roundFree = true)

/-- **The first unfinished slot always has a step.** With every earlier
slot finished and the round free, whatever step the slot needs next may be
taken: every ordering rule is met, because every rule looks back only. -/
theorem first_unfinished_can_step (before : List Cell) (step : Step)
    -- Every slot before this one is finished.
    (hbefore : ∀ b ∈ before, b.stage = .finished) :
    -- The claim itself; its proof follows.
    Enabled true before step := by
  -- The three rules, one by one.
  refine ⟨?_, ?_, ?_⟩
  -- Settled: a finished slot is past settled.
  · intro _ b hb
    -- It is finished...
    rw [hbefore b hb]
    -- ...and finished ranks above settled.
    decide
  -- Finished: take any earlier slot.
  · intro _ b hb
    -- That it is finished is the hypothesis itself.
    exact hbefore b hb
  -- The round: whatever the step needs...
  · intro _
    -- ...the round is free.
    rfl

/-- A slot that is well formed and not finished has a next step. -/
theorem unfinished_has_next (c : Cell)
    -- Its stage is one its kind passes through.
    (hwf : WellFormed c)
    -- It is not finished.
    (hun : c.stage ≠ .finished) : ∃ step st, next c = some (step, st) := by
  -- Well formed means finished, or with a next step.
  cases hwf with
  -- Case: the left alternative holds.
  | inl hfin =>
    -- Finished is ruled out.
    exact absurd hfin hun
  -- Case: the right alternative holds.
  | inr hsome =>
    -- A next step exists: look at what `next` gives.
    match hn : next c with
    -- A step and a stage: those are the ones asked for.
    | some (step, st) => exact ⟨step, st, rfl⟩
    -- Case: there is none.
    | none =>
      -- No next step contradicts the hypothesis.
      simp [hn] at hsome

/-- **A step moves a slot one stage on, and never back.** The stage a step
leads to is further on, the slot then needs exactly one step fewer, and it
is still well formed. -/
theorem step_advances (c : Cell) (step : Step) (st : Stage)
    -- `step` is the slot's next step, and leads to stage `st`.
    (hn : next c = some (step, st)) :
    -- The claim itself; its proof follows.
    rank c.stage < rank st ∧ todo ⟨c.kind, st⟩ + 1 = todo c ∧ WellFormed ⟨c.kind, st⟩ := by
  -- Name the slot's kind and stage.
  obtain ⟨k, s0⟩ := c
  -- Go through every kind and stage: each case is read off the tables.
  cases k <;> cases s0 <;> simp [next] at hn <;> obtain ⟨rfl, rfl⟩ := hn <;>
    -- (Each case then holds by reading the tables.)
    simp [rank, todo, WellFormed, next]

/-- **No step goes back.** Whatever thread takes whatever step on whatever
slot, the slot's stage only moves forward: nobody undoes another thread's
work, so progress made is progress kept. -/
theorem no_step_goes_back (c : Cell) (step : Step) (st : Stage)
    -- `step` is the slot's next step, and leads to stage `st`.
    (hn : next c = some (step, st)) : rank c.stage < rank st :=
  -- The first part of `step_advances`.
  (step_advances c step st hn).1

/-- A finished slot needs nothing more. -/
theorem todo_finished (k : Kind) : todo ⟨k, .finished⟩ = 0 := by
  -- Read off the table, kind by kind.
  cases k <;> rfl

/-- A well-formed slot that needs nothing more is finished. -/
theorem finished_of_todo_zero (c : Cell)
    -- Its stage is one its kind passes through.
    (hwf : WellFormed c)
    -- It needs no step.
    (hz : todo c = 0) : c.stage = .finished := by
  -- Well formed means finished, or with a next step.
  cases hwf with
  -- Case: the left alternative holds.
  | inl hfin =>
    -- Finished: nothing to show.
    exact hfin
  -- Case: the right alternative holds.
  | inr hsome =>
    -- With a next step the slot needs at least one step: name the step.
    match hn : next c with
    -- Case: there is one.
    | some (step, st) =>
      -- Then `todo c` is one more than something...
      have := (step_advances c step st hn).2.1
      -- ...so it is not zero.
      omega
    -- Case: there is none.
    | none =>
      -- No next step contradicts the hypothesis.
      simp [hn] at hsome

/-- The steps the slots of a list still need, in all. -/
def total : List Cell → Nat
  -- No slot, no step.
  | [] => 0
  -- A slot's own steps, and the rest's.
  | c :: rest => todo c + total rest

/-- What one step of a helper does: it takes the next step of the first
slot that is not finished, and leaves every other slot as it is. -/
def helperStep : List Cell → List Cell
  -- No slot, nothing to do.
  | [] => []
  -- Look at the first slot.
  | c :: rest =>
    -- A finished slot is passed over: the step is taken further on.
    if c.stage = .finished then c :: helperStep rest
    -- The first unfinished one moves on, by its next step.
    else match next c with
      -- Its next step leads to stage `st`.
      | some (_, st) => ⟨c.kind, st⟩ :: rest
      -- (A slot with no next step is left as it is.)
      | none => c :: rest

/-- **A helper's step is always there to take, and always counts.** In a
list of well-formed slots with work left, the helper's step is the next
step of the first unfinished slot; that step is enabled when the round is
free (every slot before it is finished); and after it the slots need
exactly one step fewer and are still well formed. -/
theorem helper_step_makes_progress (l : List Cell)
    -- Every slot's stage is one its kind passes through.
    (hwf : ∀ c ∈ l, WellFormed c)
    -- Some step is still needed.
    (hpos : 0 < total l) :
    -- The claim itself; its proof follows.
    total (helperStep l) + 1 = total l ∧ (∀ c ∈ helperStep l, WellFormed c) ∧
      ∃ before c after step st, l = before ++ c :: after ∧
        (∀ b ∈ before, b.stage = .finished) ∧ next c = some (step, st) ∧
        Enabled true before step := by
  -- By induction on the slots.
  induction l with
  -- Case: the empty list.
  | nil =>
    -- No slot needs no step: the hypothesis is false.
    simp [total] at hpos
  -- Case: a first element and then the rest, the claim known for the rest (ih).
  | cons c rest ih =>
    -- The first slot is well formed...
    have hc : WellFormed c := hwf c (List.mem_cons_self)
    -- ...and so are the rest.
    have hrest : ∀ x ∈ rest, WellFormed x := fun x hx => hwf x (List.mem_cons_of_mem _ hx)
    -- Is the first slot finished?
    by_cases hfin : c.stage = .finished
    -- It is, so it needs nothing: the work left is in the rest.
    · have hz : todo c = 0 := by
        -- Name its kind; its stage is `finished`.
        obtain ⟨k, s0⟩ := c
        -- Read the hypothesis as a fact about that stage...
        simp only at hfin
        -- ...put `finished` in its place...
        subst hfin
        -- ...and read the table.
        exact todo_finished k
      -- So the rest has work left.
      have hpos' : 0 < total rest := by
        -- The total is the first slot's none plus the rest's.
        simpa [total, hz] using hpos
      -- The induction step gives the answer for the rest.
      obtain ⟨ht, hw, before, c', after, step, st, hl, hb, hn, he⟩ := ih hrest hpos'
      -- Put the finished slot back in front of each part.
      refine ⟨?_, ?_, c :: before, c', after, step, st, ?_, ?_, hn, ?_⟩
      -- The count: the first slot adds nothing on either side.
      · simp [helperStep, hfin, total, hz, ht]
      -- Well formed: the first slot is unchanged, the rest by induction.
      · intro x hx
        -- `x` is the first slot or one of the stepped rest.
        simp [helperStep, hfin] at hx
        -- Either way it is well formed.
        cases hx with
        -- The first slot: by hypothesis.
        | inl h => exact h ▸ hc
        -- One of the rest: by induction.
        | inr h => exact hw x h
      -- The list splits at the same slot, one place later.
      · simp [hl]
      -- Every slot before it is finished: the first one, and the others.
      · intro b hb'
        -- `b` is the first slot or one of `before`.
        simp at hb'
        -- Look at which.
        cases hb' with
        -- The first slot is finished: that is this case.
        | inl h => exact h ▸ hfin
        -- One of `before` is finished by induction.
        | inr h => exact hb b h
      -- The step is enabled: every slot before it is finished.
      · exact first_unfinished_can_step (c :: before) step (by
          -- Take any slot before it.
          intro b hb'
          -- It is the first slot or one of `before`.
          simp at hb'
          -- Look at which.
          cases hb' with
          -- The first slot is finished: that is this case.
          | inl h => exact h ▸ hfin
          -- One of `before` is finished by induction.
          | inr h => exact hb b h)
    -- It is not: it is the first unfinished slot, and has a next step.
    · obtain ⟨step, st, hn⟩ := unfinished_has_next c hc hfin
      -- That step moves it one stage on.
      obtain ⟨_, htodo, hwf'⟩ := step_advances c step st hn
      -- Nothing stands before it.
      refine ⟨?_, ?_, [], c, rest, step, st, rfl, ?_, hn, ?_⟩
      -- The count: unfold the helper's step and the totals.
      · simp [helperStep, hfin, hn, total]
        -- This slot needs one step fewer, the rest the same.
        omega
      -- Well formed: take any slot after the step.
      · intro x hx
        -- `x` is the stepped slot or one of the rest.
        simp [helperStep, hfin, hn] at hx
        -- Look at which.
        cases hx with
        -- The stepped slot: by `step_advances`.
        | inl h => exact h ▸ hwf'
        -- One of the rest: as before.
        | inr h => exact hrest x h
      -- No slot is before it: suppose there were one.
      · intro b hb
        -- The empty list has no member.
        simp at hb
      -- The step is enabled: there is nothing before it to wait for.
      · exact first_unfinished_can_step [] step (by
          -- Suppose there were a slot before it.
          intro b hb
          -- The empty list has no member.
          simp at hb)

/-- `run n l`: the slots after `n` steps of one helper. -/
def run : Nat → List Cell → List Cell
  -- No step: the slots as they are.
  | 0, l => l
  -- One step, then the rest.
  | n + 1, l => run n (helperStep l)

/-- When no step is needed, every slot is finished. -/
theorem all_finished_of_total_zero (l : List Cell)
    -- Every slot's stage is one its kind passes through.
    (hwf : ∀ c ∈ l, WellFormed c)
    -- No step is needed.
    (hz : total l = 0) : ∀ c ∈ l, c.stage = .finished := by
  -- By induction on the slots.
  induction l with
  -- Case: the empty list.
  | nil =>
    -- No slot: suppose there were one.
    intro c hc
    -- The empty list has no member.
    simp at hc
  -- Case: a first element and then the rest, the claim known for the rest (ih).
  | cons c rest ih =>
    -- The first slot needs nothing...
    have h1 : todo c = 0 := by
      -- The total is its steps plus the rest's...
      simp [total] at hz
      -- ...and a sum that is zero has zero parts.
      omega
    -- ...and neither does the rest.
    have h2 : total rest = 0 := by
      -- The same sum...
      simp [total] at hz
      -- ...and the same arithmetic.
      omega
    -- Take any slot of the list.
    intro x hx
    -- It is the first slot or one of the rest.
    simp at hx
    -- Look at which.
    cases hx with
    -- Case: the left alternative holds.
    | inl h =>
      -- The first: well formed and needing nothing, so finished.
      exact h ▸ finished_of_todo_zero c (hwf c (List.mem_cons_self)) h1
    -- Case: the right alternative holds.
    | inr h =>
      -- One of the rest: by induction.
      exact ih (fun y hy => hwf y (List.mem_cons_of_mem _ hy)) h2 x h

/-- **One live thread alone finishes every slot up to its own** (the
safety half of lock-freedom). Take the slots of the ring up to a thread's
own, each at any stage at all: the others may have stopped anywhere. With
the round free, that thread's own steps, `total l` of them and each one
enabled when it is taken, leave every slot finished: its own commit
returns. No other thread has to take a step. -/
theorem helper_alone_finishes (l : List Cell)
    -- Every slot's stage is one its kind passes through.
    (hwf : ∀ c ∈ l, WellFormed c) : ∀ c ∈ run (total l) l, c.stage = .finished := by
  -- By induction on the number of steps needed, for every list of slots.
  generalize hn : total l = n
  -- Induct on that number, for every list of slots.
  induction n generalizing l with
  -- Case: zero.
  | zero =>
    -- No step needed: every slot is finished already.
    exact all_finished_of_total_zero l hwf hn
  -- Case: one more, the claim known for the number before (ih).
  | succ n ih =>
    -- Work is left...
    have hpos : 0 < total l := by omega
    -- ...so the helper's step counts, and leaves the slots well formed.
    obtain ⟨ht, hw, _⟩ := helper_step_makes_progress l hwf hpos
    -- After it, `n` steps are needed, and the induction step applies.
    exact ih (helperStep l) hw (by omega)

/-- A slot never needs more than eight steps. -/
theorem todo_le (c : Cell) : todo c ≤ 8 := by
  -- Name its kind and stage.
  obtain ⟨k, s0⟩ := c
  -- Read the table case by case.
  cases k <;> cases s0 <;> simp [todo]

/-- **Helping is bounded** (E21). The steps one thread takes for the slots
up to its own, other threads' slots included, are at most eight a slot:
no thread serves an unbounded queue of others. -/
theorem helping_is_bounded (l : List Cell) : total l ≤ 8 * l.length := by
  -- By induction on the slots.
  induction l with
  -- Case: the empty list.
  | nil =>
    -- No slot, no step.
    simp [total]
  -- Case: a first element and then the rest, the claim known for the rest (ih).
  | cons c rest ih =>
    -- The first slot needs at most eight.
    have := todo_le c
    -- The total is its steps plus the rest's, and the list is one longer.
    simp [total]
    -- The rest is bounded by induction; add the two bounds.
    omega

/-- **The stated limit: a held round blocks the sync.** With the round not
free, the sync an Immediate commit waits for cannot be taken, however far
every other slot has come. A thread that stops while it holds the round
therefore stops the commits that need it, which is why the pipeline treats
the holder as doing device I/O (TLA+ RED `StopHoldsClaim`). -/
theorem held_round_blocks (before : List Cell) : ¬ Enabled false before .sync := by
  -- Suppose the sync were enabled.
  intro h
  -- Its third rule says the round is free; it is not.
  have := h.2.2 rfl
  -- `false = true` is absurd.
  exact Bool.noConfusion this

/-! ## The write unit: runs of records

The records reach the log through the write unit
(`src/engine/commit/write_unit.rs`). Each owner encodes its own record; one
thread at a time, the holder, writes the run of encoded records that waits
first with one write. Tiny example: slots 1, 2 and 3 are settled and their
owners encoded their records; the holder writes all three with one write.
In the steps of this file that is three `write` steps taken at once. -/

/-- What a run does to one slot: a slot whose next step is the write moves
past it, and any other slot is left as it is. -/
def writeOne (c : Cell) : Cell :=
  -- Look at the slot's next step.
  match next c with
  -- Its write: it moves to the stage after.
  | some (.write, st) => ⟨c.kind, st⟩
  -- Anything else: the run does not touch it.
  | _ => c

/-- One run over a list of slots: every slot whose next step is the write
is written, in one step of one thread. (A real run writes one stretch of
them, the first that waits; proving it for all of them at once covers every
stretch, since a slot the run leaves out is simply left as it is.) -/
def runStep : List Cell → List Cell
  -- No slot, nothing written.
  | [] => []
  -- The first slot as the run leaves it, then the rest.
  | c :: rest => writeOne c :: runStep rest

/-- One for a slot whose next step is the write, else none. -/
def writes (c : Cell) : Nat :=
  -- Look at the slot's next step.
  match next c with
  -- Its write: one record.
  | some (.write, _) => 1
  -- Anything else: none.
  | _ => 0

/-- How many records a run over the list writes. -/
def writable : List Cell → Nat
  -- No slot, no record.
  | [] => 0
  -- The first slot's, then the rest's.
  | c :: rest => writes c + writable rest

/-- One slot under a run: its steps left fall by one when it was written
and stay the same otherwise, and it stays well formed. -/
theorem write_one_counts (c : Cell)
    -- The slot's stage is one its kind passes through.
    (hwf : WellFormed c) :
    -- In symbols: the steps the slot needs after the run's write, plus the
    -- writes the run made of it, are the steps it needed before; and it
    -- is still well formed.
    todo (writeOne c) + writes c = todo c ∧ WellFormed (writeOne c) := by
  -- Name the slot's kind and stage.
  obtain ⟨k, s0⟩ := c
  -- Try every kind of slot and every stage it can stand at; for each one
  -- the definitions' tables decide the claim by plain arithmetic.
  cases k <;> cases s0 <;> simp_all [writeOne, writes, next, todo, WellFormed]

/-- **A run counts every record it writes, once.** After one run the slots
need exactly as many steps fewer as the run wrote records, and are all
still well formed: many write steps taken as one, so the count of steps
left, which one live thread alone brings to zero (`helper_alone_finishes`),
only falls. Rules out a run that writes a record and leaves its slot
needing the write still, or that undoes a slot it passed over. -/
theorem run_step_counts (l : List Cell)
    -- Every slot's stage is one its kind passes through.
    (hwf : ∀ c ∈ l, WellFormed c) :
    -- In symbols: the steps all slots need after the run, plus the records
    -- it wrote, are the steps they needed before; and every slot is still
    -- well formed.
    total (runStep l) + writable l = total l ∧ ∀ c ∈ runStep l, WellFormed c := by
  -- By induction on the slots.
  induction l with
  -- Case: the empty list.
  | nil =>
    -- Nothing to write, nothing to count.
    simp [runStep, writable, total]
  -- Case: a first slot and the rest, the claim known for the rest (ih).
  | cons c rest ih =>
    -- The first slot under the run.
    obtain ⟨h1, hw1⟩ := write_one_counts c (hwf c List.mem_cons_self)
    -- The rest under the run, by induction.
    obtain ⟨h2, hw2⟩ := ih (fun x hx => hwf x (List.mem_cons_of_mem _ hx))
    -- Split the claim in two.
    refine ⟨?_, ?_⟩
    -- The count: unfold one level of each definition...
    · simp only [runStep, writable, total]
      -- ...and add the two equations.
      omega
    -- Well formed: take any slot after the run.
    · intro x hx
      -- It is the first slot or one of the rest.
      simp only [runStep, List.mem_cons] at hx
      -- Look at which.
      cases hx with
      -- The first slot: `write_one_counts` showed it well formed.
      | inl h => exact h ▸ hw1
      -- One of the rest: by induction.
      | inr h => exact hw2 x h

/-- **A run moves no slot back.** Every slot a run writes goes one stage on,
and every other stays where it was: nobody's progress is undone, whoever
holds the write unit. -/
theorem run_never_goes_back (c : Cell) : rank c.stage ≤ rank (writeOne c).stage := by
  -- Name the slot's kind and stage.
  obtain ⟨k, s0⟩ := c
  -- Try every kind of slot and every stage it can stand at; for each one
  -- the tables show its stage's rank does not fall.
  cases k <;> cases s0 <;> simp [writeOne, next, rank]

/-! ## The write unit: a park is bounded

A thread that needs a record written finds it in another thread's hands:
the unit's holder is writing a run, or the record's owner has not encoded
it yet. It spins about as long as one write takes, then parks on the
record. It wakes when another thread writes the record (the holder's run,
or a copy) or when its patience runs out; then it writes the record itself,
as a copy through the gate (`write_unit.rs`, WU5).

Tiny example, with a patience of two slices: the holder stops for good with
the unit. The waiter spins (one step), parks with two slices left, one
slice passes, the other passes, its patience is out, it writes the record:
five steps of its own, and nobody else did anything. -/

/-- Where a thread stands whose record is in another thread's hands. -/
inductive Waiting where
  /-- It spins, about as long as one write takes. -/
  | spinning
  /-- It sleeps on the record, with `left` more slices of its patience. -/
  | parked (left : Nat)
  /-- Its patience ran out: it is about to write the record itself. -/
  | copying
  /-- The record is in the log. -/
  | written
  -- Two such values can be compared for equality.
  deriving DecidableEq

/-- The waiting thread's own next step, which needs no other thread: the
spin ends and it parks with its whole patience, a slice of patience passes,
the patience runs out, or the copy is written. (In the code the slices are
`park_timeout`s, each at most what is left of `PATIENCE`, or the logical
triggers that end the wait sooner.) -/
def own (patience : Nat) : Waiting → Waiting
  -- The spin ends: it parks with all its patience.
  | .spinning => .parked patience
  -- A slice passes.
  | .parked (n + 1) => .parked n
  -- No slice left: it goes to write the record itself.
  | .parked 0 => .copying
  -- The copy is written, through the gate (or found written by then).
  | .copying => .written
  -- A written record stays written.
  | .written => .written

/-- What another thread can do to the waiting thread, as this machine
counts it: write its record (a run of the holder's, or another's copy),
which wakes it with its record in the log. Another thread can also do
nothing at all, as one that stopped.

What this leaves out: in the code a wake does not always mean the record
is written. A thread woken because the holder let the unit go looks again
and may park again; its patience then runs from its first wait, not from
the new park (`out_of_patience`: `PATIENCE` since the first wait, and at
most `MAX_PARKS` parks). So the code's bound is that patience in all, plus
`MAX_PARKS` parks, plus the copy; this machine shows the shape of that
bound for one park, and `wakes_only_shorten` promises only that, in this
machine, another thread's step never adds to what the waiter needs. -/
def woken (_ : Waiting) : Waiting := .written

/-- The steps the waiting thread still needs on its own. -/
def stepsLeft (patience : Nat) : Waiting → Nat
  -- Spin, every slice, the run out, the copy.
  | .spinning => patience + 3
  -- The slices left, the run out, the copy.
  | .parked n => n + 2
  -- The copy.
  | .copying => 1
  -- Nothing.
  | .written => 0

/-- Each own step of a thread not yet written leaves exactly one step
fewer. -/
theorem own_counts (p : Nat) (w : Waiting)
    -- The record is not in the log yet.
    (hw : w ≠ .written) :
    -- In symbols: one own step later, it needs exactly one step fewer.
    stepsLeft p (own p w) + 1 = stepsLeft p w := by
  -- Look at where the thread stands.
  cases w with
  -- Spinning: it parks with all its patience, one step fewer.
  | spinning => simp [own, stepsLeft]
  -- Parked with some slices left: look at how many.
  | parked n =>
    -- None or some.
    cases n with
    -- None: it goes to copy, one step fewer.
    | zero => simp [own, stepsLeft]
    -- Some: one slice fewer.
    | succ m => simp [own, stepsLeft]
  -- Copying: the copy is written, one step fewer.
  | copying => simp [own, stepsLeft]
  -- Written: ruled out by the hypothesis.
  | written => exact absurd rfl hw

/-- **Wakes only shorten the wait, in this machine.** Whatever another
thread does to the waiting thread here, it never needs more of its own
steps afterwards. In the code a woken thread may park again; its bound is
then the patience since its first wait and `MAX_PARKS` parks (see
`woken`). -/
theorem wakes_only_shorten (p : Nat) (w : Waiting) : stepsLeft p (woken w) ≤ stepsLeft p w := by
  -- A woken thread needs no step at all.
  simp [woken, stepsLeft]

/-- A schedule: who takes the next step, the waiting thread (`true`) or
some other thread (`false`). -/
def exec (p : Nat) : List Bool → Waiting → Waiting
  -- No step left: where it stands.
  | [], w => w
  -- The waiting thread's own step, then the rest.
  | true :: rest, w => exec p rest (own p w)
  -- Another thread's step, then the rest.
  | false :: rest, w => exec p rest (woken w)

/-- How many of a schedule's steps are the waiting thread's own. -/
def ownSteps : List Bool → Nat
  -- None in an empty schedule.
  | [] => 0
  -- One more for each of its own.
  | true :: rest => ownSteps rest + 1
  -- None for another thread's.
  | false :: rest => ownSteps rest

/-- A written record stays written, whoever steps. -/
theorem written_stays (p : Nat) (s : List Bool) : exec p s .written = .written := by
  -- By induction on the schedule.
  induction s with
  -- Case: no step.
  | nil => rfl
  -- Case: a first step, the claim known for the rest (ih).
  | cons b rest ih =>
    -- Whose step it is.
    cases b with
    -- Another thread's step: a written record stays written; the rest of
    -- the schedule keeps it so, by the induction.
    | false => simpa [exec, woken] using ih
    -- Its own step: a written record stays written; the rest of the
    -- schedule keeps it so, by the induction.
    | true => simpa [exec, own] using ih

/-- Needing no own step means the record is written. -/
theorem written_of_no_steps (p : Nat) (w : Waiting)
    -- It needs no step of its own.
    (h : stepsLeft p w = 0) : w = .written := by
  -- Look at where the thread stands.
  cases w with
  -- Spinning needs `p + 3`, not zero.
  | spinning => simp [stepsLeft] at h
  -- Parked needs at least two, not zero.
  | parked n => simp [stepsLeft] at h
  -- Copying needs one, not zero.
  | copying => simp [stepsLeft] at h
  -- Written: that is the claim.
  | written => rfl

/-- **A park is bounded.** However the other threads' steps fall between
its own, the waiting thread's record is in the log once it has taken as
many of its own steps as it needed: other threads may wake it sooner, and
may do nothing at all, as a holder that stopped for good with the write
unit claimed. Rules out a waiter that sleeps for ever behind a preempted or
stopped holder: the defect of the reverted write stage (E45, L55). -/
theorem a_park_is_bounded (p : Nat) (s : List Bool) (w : Waiting)
    -- The schedule holds at least as many of its own steps as it needs.
    (h : stepsLeft p w ≤ ownSteps s) : exec p s w = .written := by
  -- By induction on the schedule, for every place the thread may stand.
  induction s generalizing w with
  -- Case: no step left.
  | nil =>
    -- No own step in an empty schedule, so it needed none...
    have : stepsLeft p w = 0 := by
      -- ...the bound says at most zero...
      simp [ownSteps] at h
      -- ...so exactly zero.
      omega
    -- ...and needing none means written.
    exact written_of_no_steps p w this
  -- Case: a first step, the claim known for the rest (ih).
  | cons b rest ih =>
    -- Whose step it is.
    cases b with
    -- Another thread's: it writes the record, and written stays written.
    | false =>
      -- Unfold one step of the schedule.
      simp only [exec, woken]
      -- Written stays written.
      exact written_stays p rest
    -- Its own.
    | true =>
      -- Unfold one step of the schedule.
      simp only [exec]
      -- Written already, or not.
      by_cases hw : w = .written
      -- Written: its own step keeps it written, and so does the rest.
      · subst hw
        -- Written stays written.
        exact written_stays p rest
      -- Not written: one own step fewer is needed after this one.
      · apply ih
        -- The count of `own_counts`...
        have := own_counts p w hw
        -- ...against the schedule's own steps, one fewer after this one.
        simp [ownSteps] at h
        -- The arithmetic.
        omega

/-- An own step never adds to what the thread needs. -/
theorem own_never_adds (p : Nat) (w : Waiting) : stepsLeft p (own p w) ≤ stepsLeft p w := by
  -- Written already, or not.
  by_cases hw : w = .written
  -- Written: its own step keeps it written.
  · subst hw
    -- Read the tables.
    simp [own, stepsLeft]
  -- Not written: one step fewer, by `own_counts`.
  · have := own_counts p w hw
    -- The arithmetic.
    omega

/-- No schedule adds to what the thread needs: every step, its own or
another thread's, leaves it needing as many own steps or fewer. -/
theorem exec_never_adds (p : Nat) (s : List Bool) (w : Waiting) :
    -- In symbols: after any schedule it needs no more own steps than
    -- before it.
    stepsLeft p (exec p s w) ≤ stepsLeft p w := by
  -- By induction on the schedule, for every place the thread may stand.
  induction s generalizing w with
  -- Case: no step.
  | nil =>
    -- Nothing changes.
    simp [exec]
  -- Case: a first step, the claim known for the rest (ih).
  | cons b rest ih =>
    -- Whose step it is.
    cases b with
    -- Another's: the wake, then the rest.
    | false =>
      -- The rest after the wake needs no more than the wake left...
      have h1 := ih (woken w)
      -- ...and the wake left no more than before.
      have h2 := wakes_only_shorten p w
      -- Unfold one step and chain the two.
      simp only [exec]
      -- The arithmetic.
      omega
    -- Its own: the step, then the rest.
    | true =>
      -- The rest after the step needs no more than the step left...
      have h1 := ih (own p w)
      -- ...and the step left no more than before.
      have h2 := own_never_adds p w
      -- Unfold one step and chain the two.
      simp only [exec]
      -- The arithmetic.
      omega

/-- **The patience bounds the wait.** A thread that began to wait (it
spins first) never needs more than its patience and three more of its own
steps, whatever has happened since: spin, the slices, the run out, the
copy. With `a_park_is_bounded`: within that many of its own steps its
record is in the log. -/
theorem patience_bounds_the_wait (p : Nat) (s : List Bool) :
    -- In symbols: whatever the schedule, a thread that began by spinning
    -- needs at most its patience and three more own steps.
    stepsLeft p (exec p s .spinning) ≤ p + 3 := by
  -- No schedule adds to what it needed when it began...
  have := exec_never_adds p s .spinning
  -- ...which was its patience and three.
  simpa [stepsLeft] using this

/-- What a parked thread does on its own when it has no patience: it goes
on sleeping. -/
def ownNoPatience : Waiting → Waiting
  -- The spin ends: it parks.
  | .spinning => .parked 0
  -- Parked: it stays parked, with no time out.
  | .parked n => .parked n
  -- The others as with patience.
  | .copying => .written
  -- Written stays written.
  | .written => .written

/-- A schedule run with no patience. -/
def execNoPatience : List Bool → Waiting → Waiting
  -- No step left: where it stands.
  | [], w => w
  -- Its own step, then the rest.
  | true :: rest, w => execNoPatience rest (ownNoPatience w)
  -- Another thread's step, then the rest.
  | false :: rest, w => execNoPatience rest (woken w)

/-- **Without patience a parked thread waits for a wake.** With no time
out, a parked thread taking only its own steps sleeps on for ever, however
many it takes: only another thread's wake gets its record written. That is
why the code has the patience (WU5), and why a lost wake would be a stuck
commit (the TLA+ RED `LostWake`, checked with the time out taken away). -/
theorem without_patience_waits_for_a_wake (n k : Nat) :
    -- In symbols: however many own steps it takes, it is still parked,
    -- with the same slices left.
    execNoPatience (List.replicate k true) (.parked n) = .parked n := by
  -- By induction on the number of own steps.
  induction k with
  -- Case: none.
  | zero => rfl
  -- Case: one more, the claim known for the fewer (ih).
  | succ k ih =>
    -- An own step with no patience leaves it parked as it was; the rest
    -- of the steps do the same, by the induction.
    simpa [List.replicate, execNoPatience, ownNoPatience] using ih

/-! ## Shortcuts that keep the bound

Two steps of the code end a wait sooner than the machine above allows, and
neither adds a step the thread needs.

A record nobody is encoding: its owner has not begun, or stopped before it
began. The thread that needs it claims the encoding and encodes it itself
instead of parking (`write_unit.rs`, `RegolithEngine::fill`; the TLA+
`EncodeStart`): its spin ends straight in a write of its own. The fast
path (`write_unit.rs`, `write_alone`, WU8; the TLA+ `DirectStart`), taken
by an owner that finds the write unit free, is the same step taken before
any wait: it encodes and writes its own record at once, so it needs no
step this machine counts. Several owners may take it at once, since the
unit does not see their writes; each writes its own record.
A commit that waits for a sync and finds its own record in another
thread's hands copies it at once rather than park (the TLA+ `CopyOwn`):
its spin ends in `copying`, never in `parked`, which only shortens the
count `a_park_is_bounded` uses.

A thread far from its turn (`pipeline.rs`, P6) parks on its own slot until
its turn nears: a finished commit tells it (NEAR), its slot lands or fails,
or `TURN_PATIENCE` runs out. That is this same machine with one change of
name: where it says the record is written, read that the thread is awake
and helping again. A wake from another thread is `woken`; a slice of
`TURN_PATIENCE` is a slice of patience; and the patience running out wakes
the thread, which is one step more than this count allows for. -/

/-- The waiting thread's own step when it may encode the record itself:
from the spin straight to writing it, otherwise as before. -/
def ownEncoding (patience : Nat) : Waiting → Waiting
  -- The spin ends in a write of its own: nobody is encoding the record.
  | .spinning => .copying
  -- Any other place: the step it took before.
  | w => own patience w

/-- **Encoding it itself never adds a step.** A thread that encodes the
record nobody is encoding needs no more of its own steps than one that
parks: the bound of `a_park_is_bounded` still holds. -/
theorem encoding_never_adds (p : Nat) (w : Waiting) :
    -- In symbols: encoding the record itself leaves it needing no more
    -- own steps than the step it would have taken otherwise.
    stepsLeft p (ownEncoding p w) ≤ stepsLeft p w := by
  -- Look at where the thread stands.
  cases w with
  -- Spinning: one step to write, against the whole patience.
  | spinning => simp [ownEncoding, stepsLeft]
  -- Any other place: the step it took before, which never adds.
  | parked n => exact own_never_adds p (.parked n)
  -- (Copying: the same.)
  | copying => exact own_never_adds p .copying
  -- (Written: the same.)
  | written => exact own_never_adds p .written

/-- **A park far from the turn is bounded.** A thread parked far from its
turn with `n` slices of its patience left is awake and helping once it has
taken `n + 2` of its own steps, however the other threads' steps fall,
whether or not any of them tells it. With `helping_is_bounded`, the slots
before its own are then finished within a bounded number of steps. -/
theorem a_turn_park_is_bounded (p n : Nat) (s : List Bool)
    -- The schedule holds at least `n + 2` of its own steps.
    (h : n + 2 ≤ ownSteps s) : exec p s (.parked n) = .written :=
  -- The park bound, with what a parked thread needs read off the table.
  a_park_is_bounded p s (.parked n) (by simpa [stepsLeft] using h)

-- The end of this file's names.
end Regolith.PipelineHelping
