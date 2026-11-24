/-!
# Parking: a job that parks for a tool is told when the tool is put down

This file backs the TLA+ model `proofs/tla/Parking.tla`. TLC checks there
two queues, a blocking caller and a worker, in every interleaving. Here the
rule that makes parking safe, the note and the second look, is proved for
every number of jobs and every number of times the tool changes hands.

## The story, for a reader who has never seen the code

A queue's poll never waits for another thread (`src/engine/commit/park.rs`).
When the job it runs needs something another thread has (the round, the
version store's step, the flush exclusion: here, "the tool"), the job parks:
the poll puts it back and goes on with the rest of its queue. Somebody has
to tell the job when the tool is free again, or it would sit there for ever.

So the job leaves a note on the tool: "tell me when you are put down".
Whoever puts the tool down reads the notes and tells every job named.

Tiny example of what can go wrong. The job looks: the tool is in use. At
that very moment the tool is put down and the notes are read: there are
none. Now the job leaves its note and parks. Nobody will ever read that
note: the tool is lying there free, and the job waits for a call that
already happened.

The cure is to look twice. The job looks, leaves its note, and looks AGAIN:
if the tool is free now, it takes it; only if it is still in use does the
job park. A tool still in use after the note was left will be put down
later, and the notes read then.

## What is proved, in plain words

1. `no_parked_job_is_forgotten`: whenever the tool is free and everybody who
   put it down has read the notes, every parked job has been told. So no
   job is parked with nobody left to tell it (TLA+ `JobLands`, for the part
   that is about the note).
2. The RED case, as a counterexample: a job that parks without its second
   look can be left parked and untold beside a free tool
   (`no_second_look_forgets_a_job`, TLA+ RED `NoSecondLook`; the same state
   is what a tool put down without reading the notes leaves, TLA+ RED
   `ReleaseTellsNobody`).

## How to read the Lean

`def` defines a thing, `theorem` states a fact and its proof follows
`:= by`. Lines starting with `--` are comments, in plain words. A proof is a
list of *tactics*; each one changes the goal still to be shown, and the
comment above it says how. `simp` rewrites with known facts; `omega` solves
arithmetic over natural numbers; `rcases` splits on the ways a fact could be
true; `induction` proves a fact for every number of steps by proving it for
none and then for one more.
-/

-- Everything below is named Regolith.Parking.<name>.
namespace Regolith.Parking

/-! ## Part 1. The steps -/

/-- One tool and the jobs around it, at one moment. Jobs are named by
number. -/
structure State where
  /-- Whether a thread holds the tool. -/
  held : Bool
  /-- How many threads put the tool down and have not read the notes yet. -/
  reading : Nat
  /-- The notes on the tool: the jobs to tell when it is put down. -/
  notes : List Nat
  /-- Every job that left a note at some time. -/
  noted : List Nat
  /-- The jobs that parked. -/
  parked : List Nat
  /-- The jobs told that the tool was put down. -/
  told : List Nat

/-- The start: a worker holds the tool, and no job has come yet. -/
def init : State :=
  -- Held, nobody reading, and four empty lists.
  ⟨true, 0, [], [], [], []⟩

/-- The steps of `io::parked::Parked` and of the threads that use the tool,
each one atomic step there. -/
inductive Step : State → State → Prop
  /-- Job `q` leaves its note on the tool (`take_or_park` registers the
  waker). It may do so whatever the tool's state: it looked a moment ago. -/
  | note (s : State) (q : Nat) :
      -- The note is on the tool, and `q` is remembered as having left one.
      Step s { s with notes := q :: s.notes, noted := q :: s.noted }
  /-- The second look of a job that left a note finds the tool free: the
  job takes it. Its note stays, and tells it once for nothing. -/
  | lookTake (s : State) (q : Nat) (hq : q ∈ s.noted) (hfree : s.held = false) :
      -- The tool is held.
      Step s { s with held := true }
  /-- The second look finds the tool still held: the job parks. -/
  | lookPark (s : State) (q : Nat) (hq : q ∈ s.noted) (hheld : s.held = true) :
      -- The job is parked.
      Step s { s with parked := q :: s.parked }
  /-- Any thread takes the free tool: a job at its first look, or a
  blocking caller. -/
  | take (s : State) (hfree : s.held = false) :
      -- The tool is held.
      Step s { s with held := true }
  /-- The holder puts the tool down. It reads the notes in a step of its
  own, after this one. -/
  | putDown (s : State) (hheld : s.held = true) :
      -- The tool is free, and one more thread owes a reading of the notes.
      Step s { s with held := false, reading := s.reading + 1 }
  /-- A thread that put the tool down reads the notes: every job named is
  told, and the notes are gone (`Parked::released`). -/
  | readNotes (s : State) (n : Nat) (hn : s.reading = n + 1) :
      -- The jobs on the notes are told; one reading less is owed.
      Step s { s with told := s.notes ++ s.told, notes := [], reading := n }

/-- The states the steps can reach from the start. -/
inductive Reach : State → Prop
  /-- The start is reachable. -/
  | init : Reach init
  /-- One step from a reachable state reaches another. -/
  | step {s t : State} : Reach s → Step s t → Reach t

/-! ## Part 2. What every reachable state keeps -/

/-- The two facts every reachable state has. -/
structure Inv (s : State) : Prop where
  /-- A job that left a note has it on the tool still, or was told: reading
  the notes is the only thing that takes one away, and it tells the job. -/
  noted_kept : ∀ q ∈ s.noted, q ∈ s.notes ∨ q ∈ s.told
  /-- A parked job was told, or its note is on the tool and somebody will
  read it: the thread that holds the tool, or one that put it down and has
  not read the notes yet. -/
  parked_safe : ∀ q ∈ s.parked, q ∈ s.told ∨ (q ∈ s.notes ∧ (s.held = true ∨ 0 < s.reading))

/-- One step keeps both facts. -/
theorem inv_step {s t : State} (hs : Inv s) (hst : Step s t) : Inv t := by
  -- One case per step.
  cases hst with
  -- Case: job q leaves its note.
  | note q =>
    -- The two facts, one at a time.
    refine ⟨?_, ?_⟩
    -- Jobs with a note: the new one has it on the tool, the others as before.
    · intro x hx
      -- A member of `q :: l` is `q` or in `l`, in the hypothesis and the goal.
      simp only [List.mem_cons] at hx ⊢
      -- Split on which.
      rcases hx with hx | hx
      -- The new one: its note is the first on the tool.
      · left; left; exact hx
      -- An old one: its note is still there, or it was told.
      · rcases hs.noted_kept x hx with h | h
        -- Still on the tool, behind the new note.
        · left; right; exact h
        -- Told.
        · right; exact h
    -- Parked jobs: one more note takes none away.
    · intro x hx
      -- How it was safe before.
      rcases hs.parked_safe x hx with h | ⟨h1, h2⟩
      -- Told.
      · left; exact h
      -- Its note is still on the tool, behind the new one.
      · right; exact ⟨List.mem_cons_of_mem _ h1, h2⟩
  -- Case: a second look finds the tool free and takes it.
  | lookTake q hq hfree =>
    -- The two facts, one at a time.
    refine ⟨hs.noted_kept, ?_⟩
    -- Parked jobs: the tool is held now, so its holder will read the notes.
    intro x hx
    -- How it was safe before.
    rcases hs.parked_safe x hx with h | ⟨h1, _⟩
    -- Told.
    · left; exact h
    -- Its note is on the tool, which is held.
    · right; exact ⟨h1, Or.inl rfl⟩
  -- Case: a second look finds the tool held, and job q parks.
  | lookPark q hq hheld =>
    -- The two facts, one at a time.
    refine ⟨hs.noted_kept, ?_⟩
    -- Parked jobs: the new one, and the others as before.
    intro x hx
    -- A member of `q :: l` is `q` or in `l`.
    simp only [List.mem_cons] at hx
    -- Split on which.
    rcases hx with hx | hx
    -- The new one: it left a note, which is on the tool or was read.
    · rw [hx]
      -- Which of the two?
      rcases hs.noted_kept q hq with h | h
      -- On the tool, and the tool is held: this is the second look at work.
      · right; exact ⟨h, Or.inl hheld⟩
      -- Read already: the job was told.
      · left; exact h
    -- An old one: as before.
    · exact hs.parked_safe x hx
  -- Case: a thread takes the free tool.
  | take hfree =>
    -- The two facts, one at a time.
    refine ⟨hs.noted_kept, ?_⟩
    -- Parked jobs: the tool is held now, so its holder will read the notes.
    intro x hx
    -- How it was safe before.
    rcases hs.parked_safe x hx with h | ⟨h1, _⟩
    -- Told.
    · left; exact h
    -- Its note is on the tool, which is held.
    · right; exact ⟨h1, Or.inl rfl⟩
  -- Case: the holder puts the tool down.
  | putDown hheld =>
    -- The two facts, one at a time.
    refine ⟨hs.noted_kept, ?_⟩
    -- Parked jobs: the thread that put the tool down owes a reading.
    intro x hx
    -- How it was safe before.
    rcases hs.parked_safe x hx with h | ⟨h1, _⟩
    -- Told.
    · left; exact h
    -- Its note is on the tool, and one more reading is owed.
    · right; exact ⟨h1, Or.inr (Nat.succ_pos _)⟩
  -- Case: a thread that put the tool down reads the notes.
  | readNotes n hn =>
    -- The two facts, one at a time.
    refine ⟨?_, ?_⟩
    -- Jobs with a note: all told now.
    · intro x hx
      -- Where its note was before.
      rcases hs.noted_kept x hx with h | h
      -- On the tool: read now, so it is told.
      · right; exact List.mem_append.mpr (Or.inl h)
      -- Told before.
      · right; exact List.mem_append.mpr (Or.inr h)
    -- Parked jobs: told before, or named on a note that was just read.
    · intro x hx
      -- How it was safe before.
      rcases hs.parked_safe x hx with h | ⟨h1, _⟩
      -- Told before.
      · left; exact List.mem_append.mpr (Or.inr h)
      -- Its note was read now.
      · left; exact List.mem_append.mpr (Or.inl h1)

/-- Every reachable state has both facts. -/
theorem inv_reach {s : State} (h : Reach s) : Inv s := by
  -- By how the state was reached.
  induction h with
  -- Case: the start.
  | init =>
    -- No job has left a note or parked.
    exact ⟨fun q hq => by simp [init] at hq, fun q hq => by simp [init] at hq⟩
  -- Case: a reachable state, with both facts (ih), then one step, which keeps them.
  | step _ hst ih => exact inv_step ih hst

/-! ## Part 3. The rule -/

/-- **`no_parked_job_is_forgotten`.** Whenever the tool is free and every
thread that put it down has read the notes, every parked job has been told.
A job is never parked beside a free tool with nobody left to tell it. -/
theorem no_parked_job_is_forgotten {s : State} (h : Reach s)
    -- The tool is free.
    (hfree : s.held = false)
    -- Nobody still owes a reading of the notes.
    (hread : s.reading = 0) :
    -- The claim itself; its proof follows.
    ∀ q ∈ s.parked, q ∈ s.told := by
  -- Take a parked job.
  intro q hq
  -- It was told, or its note waits for a holder or for a reader.
  rcases (inv_reach h).parked_safe q hq with ht | ⟨_, hh | hr⟩
  -- Told.
  · exact ht
  -- A holder: but the tool is free.
  · rw [hfree] at hh; cases hh
  -- A reader: but none is owed.
  · exact absurd hr (by omega)

/-! ## Part 4. The RED case -/

/-- **`no_second_look_forgets_a_job`.** A job that parks without looking
again (bug `NoSecondLook`). Job 1 looked and found the tool held; the tool
was put down and the notes, none yet, were read; job 1 left its note and
parked. The tool is free, nobody owes a reading, and job 1 is parked and
was not told: the rule of `no_parked_job_is_forgotten` fails. With the
second look job 1 would have found the tool free and taken it: parking
needs a tool that is still held (`Step.lookPark`). A tool put down without
reading the notes (bug `ReleaseTellsNobody`) leaves this same state. -/
theorem no_second_look_forgets_a_job :
    -- Free tool, no reading owed, job 1's note on the tool, job 1 parked.
    let forgotten : State := ⟨false, 0, [1], [1], [1], []⟩
    -- The tool is free and no reading is owed, and a parked job was not told.
    forgotten.held = false ∧ forgotten.reading = 0 ∧
      ¬ (∀ q ∈ forgotten.parked, q ∈ forgotten.told) := by
  -- All three are read off the lists.
  simp

-- The end of this file's names.
end Regolith.Parking
