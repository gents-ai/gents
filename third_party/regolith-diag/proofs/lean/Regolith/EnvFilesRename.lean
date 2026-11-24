-- The one-key update of a function (`set`) and its two laws come from OpenFileTable.lean.
import Regolith.OpenFileTable

/-!
# EnvFilesRename: a reopen always finds a table that is being renamed aside

This file extends `Regolith/EnvFiles.lean` with Part 3 of the TLA+ model
`proofs/tla/EnvFiles.tla` (`OpenFindsFile`). TLC checks it for two reopens;
here it is proved for any number of reopens and every order of steps.

## The story, for a reader who has never seen the code

Under `max_open_files` a table's file is closed while nobody reads it, and
reopened by name (`PathEntry::reopen`, `src/env/open_file_limit/mod.rs`). A
compaction that removes a table a reader still holds moves the file aside
instead (`Shared::relocate`), in four steps, each one atomic:

1. record both names: the new one, "B", as current, the old one, "A", as
   "previous";
2. the Env's rename puts the file under "B" (one map write);
3. the Env's rename takes the file away from "A" (a second map write);
4. record only "B".

(`MemEnv::rename` first marks "A" as leaving, a map write that takes the
file from no name, so it changes nothing a reopen sees and is left out
here. What the mark is for, a removal or a second rename racing the move, is
checked by `proofs/tla/EnvRename.tla`.)

A reopen loads the recorded names, tries "previous" (if any), then
"current". If neither holds the file, it loads the names again: if the move
recorded new ones since, it tries those; if not, the file is really gone.

Tiny example. Thread 1 loads the names: just "A". Thread 2 runs the whole
move. Thread 1 opens "A": nothing. It loads the names again, finds "B",
and opens it.

## What is proved, in plain words

1. `open_finds_file`: in the real code, no reopen ever ends with nothing.
2. The RED cases, as counterexamples: a rename that takes "A" away before
   it puts the file at "B" loses a reopen (`remove_first_loses`, what
   `MemEnv::rename` did before the fix); a reopen that does not load the
   names again loses one too (`no_reload_loses`, what `PathEntry::reopen`
   did before the fix).

## How to read the Lean

`def` defines a thing, `theorem` states a fact and its proof follows
`:= by`. Lines starting with `--` are comments, in plain words. A proof is a
list of *tactics*; each one changes the goal still to be shown, and the
comment above it says how. `simp` rewrites with known facts; `omega` solves
arithmetic over natural numbers; `cases` splits on the ways a fact could be
true; `induction` proves a fact for every number of steps by proving it for
none and then for one more.
-/

-- Everything below is named Regolith.EnvFilesRename.<name>.
namespace Regolith.EnvFilesRename

-- Reuse the one-key update of `OpenFileTable.lean`.
open Regolith.OpenFileTable (set set_same set_other)

/-! ## Part 1. The state and the steps -/

/-- The two names the table's file can have. -/
inductive Name where
  /-- Where the file was. -/
  | a
  /-- Where the move puts it. -/
  | b
  -- Two names can be compared for equality.
  deriving DecidableEq

/-- What the table's entry records (`PathEntry::names`): the current name,
and the old one while a move runs. -/
structure Names where
  /-- The name the file has now, or will have once the rename lands. -/
  cur : Name
  /-- The name it had, while the move runs; `none` otherwise. -/
  prev : Option Name

/-- The names the move has recorded after `v` stores: before the move only
"A"; while the rename runs "B" with "A" as previous; once settled only "B". -/
def namesAt : Nat → Names
  -- Before the move.
  | 0 => ⟨.a, none⟩
  -- While the rename runs.
  | 1 => ⟨.b, some .a⟩
  -- Settled.
  | _ => ⟨.b, none⟩

/-- How many stores the move has made by phase `p` (0 to 4): none at 0, the
first through phases 1 to 3, the second at 4. -/
def version (p : Nat) : Nat :=
  -- Phase 0: nothing stored; phase 4: settled; in between: the first store.
  if p = 0 then 0 else if 4 ≤ p then 2 else 1

/-- The real protocol, or one of the planted bugs. -/
inductive Variant where
  /-- The code as written. -/
  | real
  /-- The Env's rename takes the old name away before it adds the new one. -/
  | removeFirst
  /-- A reopen that found nothing gives up without loading the names again. -/
  | noReload
  -- Two variants can be compared for equality.
  deriving DecidableEq

/-- Whether name `n` holds the file at phase `p`. The real rename adds "B"
at phase 2 and takes "A" away at phase 3; `removeFirst` the other way round. -/
def holds (v : Variant) (p : Nat) : Name → Bool
  -- "A" holds the file until it is taken away.
  | .a => if v = .removeFirst then decide (p < 2) else decide (p < 3)
  -- "B" holds it from when it is added.
  | .b => if v = .removeFirst then decide (3 ≤ p) else decide (2 ≤ p)

/-- Where one reopen is. -/
inductive Pc where
  /-- About to load the recorded names. -/
  | load
  /-- About to try the "previous" name. -/
  | prev
  /-- About to try the "current" name. -/
  | cur
  /-- Neither held the file; about to load the names again. -/
  | reload
  /-- It opened the file. -/
  | found
  /-- It reported the file missing. -/
  | lost
  -- Two steps can be compared for equality.
  deriving DecidableEq

/-- The move's phase and every reopen's step and loaded names. Reopens are
named by natural numbers. -/
structure St where
  /-- How far the move has got, 0 to 4. -/
  ph : Nat
  /-- Each reopen's step. -/
  pc : Nat → Pc
  /-- How many stores each reopen's loaded names reflect. -/
  ver : Nat → Nat

/-- The start: the move has not begun, every reopen is about to load. -/
def St.init : St :=
  -- Phase 0, everyone about to load, every loaded version 0.
  { ph := 0, pc := fun _ => .load, ver := fun _ => 0 }

/-- The steps, one atomic step each. -/
inductive Step (v : Variant) : St → St → Prop
  /-- The move takes its next step: a store, or one of the Env's two map
  writes. -/
  | move (s : St) (h : s.ph < 4) :
      -- One phase further; no reopen moves.
      Step v s { s with ph := s.ph + 1 }
  /-- Reopen `o` loads the recorded names: it tries "previous" next when
  there is one, else "current". -/
  | load (s : St) (o : Nat) (hpc : s.pc o = .load) :
      -- The state after this step: only the fields named here change.
      Step v s { s with
        -- It holds the names stored so far.
        ver := set s.ver o (version s.ph),
        -- Its next try.
        pc := set s.pc o (if (namesAt (version s.ph)).prev.isSome then .prev else .cur) }
  /-- Reopen `o` tries "previous": found, or it tries "current". -/
  | prev (s : St) (o : Nat) (hpc : s.pc o = .prev) :
      -- The state after this step: only its step changes.
      Step v s { s with
        -- Found if the previous name holds the file.
        pc := set s.pc o (match (namesAt (s.ver o)).prev with
          -- The previous name holds it: found; it does not: try current.
          | some n => if holds v s.ph n then .found else .cur
          -- No previous name: try current.
          | none => .cur) }
  /-- Reopen `o` tries "current": found, or it goes to load the names again. -/
  | cur (s : St) (o : Nat) (hpc : s.pc o = .cur) :
      -- The state after this step: only its step changes.
      Step v s { s with
        -- Found if the current name holds the file, else load again.
        pc := set s.pc o (if holds v s.ph (namesAt (s.ver o)).cur then .found else .reload) }
  /-- Reopen `o` found nothing: newer names mean a move ran, so it loads
  them; the same names mean the file is gone. `noReload` always gives up. -/
  | reload (s : St) (o : Nat) (hpc : s.pc o = .reload) :
      -- The state after this step: only its step changes.
      Step v s { s with
        -- Newer names (and not the bug): load them; otherwise NotFound.
        pc := set s.pc o (if version s.ph ≠ s.ver o ∧ v ≠ .noReload then .load else .lost) }

/-- The states the steps of variant `v` can reach from the start. -/
inductive Reach (v : Variant) : St → Prop
  /-- The start is reachable. -/
  | init : Reach v St.init
  /-- One step from a reachable state reaches another. -/
  | step {s s' : St} : Reach v s → Step v s s' → Reach v s'

/-! ## Part 2. What every reachable state keeps -/

/-- The facts every state the real code reaches has. -/
structure Inv (s : St) : Prop where
  /-- The move has at most five phases. -/
  ph_le : s.ph ≤ 4
  /-- A reopen about to try "previous" loaded the names with one. -/
  prev_ver : ∀ o, s.pc o = .prev → s.ver o = 1
  /-- A reopen about to try "current" with newer names has "B" holding the
  file: either it loaded the settled names, or "A" was already gone. -/
  cur_b : ∀ o, s.pc o = .cur → s.ver o ≠ 0 → 2 ≤ s.ph
  /-- A reopen about to load again found "A" gone with the old names, so the
  move has stored newer ones. -/
  reload_moved : ∀ o, s.pc o = .reload → s.ver o = 0 ∧ 3 ≤ s.ph
  /-- No reopen ever ended up with nothing. -/
  never_lost : ∀ o, s.pc o ≠ .lost

/-- Reading `set` at the key it changed or anywhere else. -/
theorem set_apply {α : Type} (f : Nat → α) (k x : Nat) (a : α) :
    -- The claim itself; its proof follows.
    set f k a x = if x = k then a else f x := by
  -- It is the definition.
  rfl

/-- The start keeps every fact. -/
theorem inv_init : Inv St.init := by
  -- Each fact, about the start: phase 0, everyone about to load.
  refine ⟨by simp [St.init], ?_, ?_, ?_, ?_⟩ <;> intro o h <;> simp [St.init] at h ⊢

/-- Every step of the real code keeps every fact. -/
theorem inv_step {s s' : St} (hs : Inv s) (h : Step .real s s') : Inv s' := by
  -- One case per step.
  cases h with
  -- The move takes its next step.
  | move hlt =>
    -- The move goes one phase further; no reopen moved.
    refine ⟨by simp; omega, hs.prev_ver, ?_, ?_, hs.never_lost⟩
    -- "B" holding the file stays true as the phase grows.
    · intro o hpc hv; have := hs.cur_b o hpc hv; simp; omega
    -- "A" gone stays true as the phase grows.
    · intro o hpc; have := hs.reload_moved o hpc; simp; omega
  -- Reopen `o` loads the names.
  | load o hpc =>
    -- Reopen `o` loads the names of the current phase.
    refine ⟨hs.ph_le, ?_, ?_, ?_, ?_⟩
    -- About to try "previous": only with the names that have one.
    · intro o' h
      -- Is it the reopen that loaded?
      by_cases ho : o' = o
      -- It is: it goes to "previous" only when the names have one: version 1.
      -- It is the reopen that tried.
      · subst ho
        -- Split on the phase's version.
        simp only [set_apply, ite_eq_left] at h ⊢
        -- Unfold the version and the names, then split on the phase.
        unfold version at h ⊢; split at h <;> split at h <;> simp_all [namesAt] <;> omega
      -- Another reopen: as before.
      · simp [set_apply, ho] at h ⊢; exact hs.prev_ver o' h
    -- About to try "current" with newer names: "B" holds the file.
    · intro o' h hv
      -- Is it the reopen that loaded?
      by_cases ho : o' = o
      -- It is: newer names that send it to "current" are the settled ones.
      -- It is the reopen that tried.
      · subst ho
        -- Unfold, and split on the phase.
        simp only [set_apply, ite_eq_left] at h hv
        -- Phase 0 gives version 0; phases 1 to 3 go to "previous"; 4 is settled.
        unfold version at h hv; split at h <;> split at h <;> simp_all [namesAt] <;> omega
      -- Another reopen: as before.
      · simp [set_apply, ho] at h hv; exact hs.cur_b o' h hv
    -- About to load again: not the reopen that just loaded.
    · intro o' h
      -- Is it the reopen that loaded?
      by_cases ho : o' = o
      -- It is: it went to "previous" or "current", not to load again.
      · subst ho; simp at h; split at h <;> simp at h
      -- Another reopen: as before.
      · simp [set_apply, ho] at h ⊢; exact hs.reload_moved o' h
    -- Nobody is lost: the loader went to a try.
    · intro o'
      -- Is it the reopen that loaded?
      by_cases ho : o' = o
      -- It is: its next step is a try.
      · subst ho; simp; split <;> simp
      -- Another reopen: as before.
      · simp [set_apply, ho]; exact hs.never_lost o'
  -- Reopen `o` tries its "previous" name.
  | prev o hpc =>
    -- Reopen `o` tries "previous", which it has: version 1, "A".
    have hv := hs.prev_ver o hpc
    -- Each fact after the try.
    refine ⟨hs.ph_le, ?_, ?_, ?_, ?_⟩
    -- About to try "previous": not `o` any more.
    · intro o' h
      -- Is it the reopen that tried?
      by_cases ho : o' = o
      -- It is: found, or on to "current".
      · subst ho; simp [hv, namesAt, holds] at h; split at h <;> simp at h
      -- Another reopen: as before.
      · simp [set_apply, ho] at h ⊢; exact hs.prev_ver o' h
    -- On to "current" with version 1: "A" was gone, so "B" holds the file.
    · intro o' h hv'
      -- Is it the reopen that tried?
      by_cases ho : o' = o
      -- It is: "A" did not hold the file, so the phase is 3 or more.
      · subst ho; simp [hv, namesAt, holds] at h ⊢; omega
      -- Another reopen: as before.
      · simp [set_apply, ho] at h hv'; exact hs.cur_b o' h hv'
    -- About to load again: not `o`.
    · intro o' h
      -- Is it the reopen that tried?
      by_cases ho : o' = o
      -- It is: found or "current", not load again.
      · subst ho; simp [hv, namesAt, holds] at h; split at h <;> simp at h
      -- Another reopen: as before.
      · simp [set_apply, ho] at h ⊢; exact hs.reload_moved o' h
    -- Nobody is lost.
    · intro o'
      -- Is it the reopen that tried?
      by_cases ho : o' = o
      -- It is: found or "current".
      · subst ho; simp [hv, namesAt, holds]; split <;> simp
      -- Another reopen: as before.
      · simp [set_apply, ho]; exact hs.never_lost o'
  -- Reopen `o` tries its "current" name.
  | cur o hpc =>
    -- Reopen `o` tries "current".
    refine ⟨hs.ph_le, ?_, ?_, ?_, ?_⟩
    -- About to try "previous": not `o`.
    · intro o' h
      -- Is it the reopen that tried?
      by_cases ho : o' = o
      -- It is: found or load again.
      · subst ho; simp at h; split at h <;> simp at h
      -- Another reopen: as before.
      · simp [set_apply, ho] at h ⊢; exact hs.prev_ver o' h
    -- About to try "current": not `o`.
    · intro o' h hv'
      -- Is it the reopen that tried?
      by_cases ho : o' = o
      -- It is: found or load again.
      · subst ho; simp at h; split at h <;> simp at h
      -- Another reopen: as before.
      · simp [set_apply, ho] at h hv'; exact hs.cur_b o' h hv'
    -- About to load again: only if "current" held nothing, which means
    -- it had the old names and "A" is gone.
    · intro o' h
      -- Is it the reopen that tried?
      by_cases ho : o' = o
      -- It is the reopen that tried.
      · subst ho
        -- It is: the try found nothing under its current name.
        simp only [set_apply, ite_eq_left] at h
        -- Split on whether the current name held the file.
        split at h
        -- It held it: found, not load again.
        · simp at h
        -- It did not: with newer names "B" would hold it, so the names are
        -- the old ones, and "A" not holding it means phase 3 or more.
        · rename_i hno
          -- Look at the names it loaded.
          by_cases h0 : s.ver o' = 0
          -- The old names: "A" did not hold the file, so phase 3 or more.
          · simp [h0, namesAt, holds] at hno; exact ⟨h0, by omega⟩
          -- Newer names: "B" holds the file, so the try could not fail.
          · have := hs.cur_b o' hpc h0
            -- Versions 1 and 2 both name "B", which holds the file.
            exfalso; apply hno
            -- Split on which newer version it is.
            rcases Nat.lt_or_ge (s.ver o') 2 with hlt | hge
            -- Version 1: its current name is "B".
            · have h1 : s.ver o' = 1 := by omega
              -- "B" holds the file from phase 2 on.
              simp [h1, namesAt, holds]; omega
            -- Version 2 and on: the settled names, "B".
            · have hn : namesAt (s.ver o') = ⟨.b, none⟩ := by
                -- Every version from 2 on is the settled names.
                unfold namesAt; split <;> simp_all <;> omega
              -- "B" holds the file from phase 2 on.
              simp [hn, holds]; omega
      -- Another reopen: as before.
      · simp [set_apply, ho] at h ⊢; exact hs.reload_moved o' h
    -- Nobody is lost: the try found it or goes to load again.
    · intro o'
      -- Is it the reopen that tried?
      by_cases ho : o' = o
      -- It is: found or load again.
      · subst ho; simp; split <;> simp
      -- Another reopen: as before.
      · simp [set_apply, ho]; exact hs.never_lost o'
  -- Reopen `o` looks at the names again.
  | reload o hpc =>
    -- Reopen `o` found nothing with the old names, after "A" went.
    have hr := hs.reload_moved o hpc
    -- So the move has stored newer names: it loads them, never gives up.
    have hnew : version s.ph ≠ s.ver o := by
      -- Phase 3 or 4 has version 1 or 2; the reopen holds version 0.
      have h3 := hr.2
      -- Unfold the version, and split on both of its cases.
      unfold version; rw [hr.1]; split <;> (try split) <;> omega
    -- Each fact after the second look.
    refine ⟨hs.ph_le, ?_, ?_, ?_, ?_⟩
    -- About to try "previous": not `o`, which loads again.
    · intro o' h
      -- Is it the reopen that looked again?
      by_cases ho : o' = o
      -- It is: about to load.
      · subst ho; simp [hnew] at h
      -- Another reopen: as before.
      · simp [set_apply, ho] at h ⊢; exact hs.prev_ver o' h
    -- About to try "current": not `o`.
    · intro o' h hv'
      -- Is it the reopen that looked again?
      by_cases ho : o' = o
      -- It is: about to load.
      · subst ho; simp [hnew] at h
      -- Another reopen: as before.
      · simp [set_apply, ho] at h hv'; exact hs.cur_b o' h hv'
    -- About to load again: not `o`.
    · intro o' h
      -- Is it the reopen that looked again?
      by_cases ho : o' = o
      -- It is: about to load.
      · subst ho; simp [hnew] at h
      -- Another reopen: as before.
      · simp [set_apply, ho] at h ⊢; exact hs.reload_moved o' h
    -- Nobody is lost: `o` loads the newer names.
    · intro o'
      -- Is it the reopen that looked again?
      by_cases ho : o' = o
      -- It is: about to load, not lost.
      · subst ho; simp [hnew]
      -- Another reopen: as before.
      · simp [set_apply, ho]; exact hs.never_lost o'

/-- Every reachable state of the real code has every fact. -/
theorem inv_reach {s : St} (h : Reach .real s) : Inv s := by
  -- By the number of steps.
  induction h with
  -- The start has every fact.
  | init => exact inv_init
  -- One more step keeps them.
  | step _ hstep ih => exact inv_step ih hstep

/-- **`open_finds_file`.** In the real code, no reopen ever reports the file
missing while it is renamed aside, however many reopens race the move and in
whatever order. TLA+ `OpenFindsFile`. -/
theorem open_finds_file {s : St} (h : Reach .real s) (o : Nat) : s.pc o ≠ .lost :=
  -- It is one of the facts every reachable state keeps.
  (inv_reach h).never_lost o

/-! ## Part 3. The RED cases -/

/-- **`remove_first_loses`.** With the rename that removes first: the move
records both names; reopen 1 loads them; the rename takes "A" away; reopen 1
tries "A" (gone) and "B" (not there yet), looks at the names again, finds
the same ones, and reports the table missing. -/
theorem remove_first_loses : ∃ s, Reach .removeFirst s ∧ s.pc 1 = .lost := by
  -- The move records both names.
  have r1 := Reach.step Reach.init (Step.move (v := .removeFirst) St.init (by simp [St.init]))
  -- Reopen 1 loads them: version 1, so it tries "previous" next.
  have r2 := Reach.step r1 (Step.load _ 1 (by simp [St.init]))
  -- The rename's first write takes "A" away.
  have r3 := Reach.step r2 (Step.move _ (by simp [St.init]))
  -- Reopen 1 tries "A": gone.
  have r4 := Reach.step r3 (Step.prev _ 1 (by simp [St.init, version, namesAt]))
  -- Reopen 1 tries "B": not there yet.
  have r5 := Reach.step r4 (Step.cur _ 1 (by simp [St.init, version, namesAt, holds]))
  -- Reopen 1 looks again: the same names. It gives up.
  have r6 := Reach.step r5 (Step.reload _ 1 (by simp [St.init, version, namesAt, holds]))
  -- That final state is the witness.
  exact ⟨_, r6, by simp [St.init, version]⟩

/-- **`no_reload_loses`.** With the reopen that never looks again: reopen 1
loads the names before the move (only "A"); the whole move runs; reopen 1
tries "A", which is gone, and gives up although the file is at "B". -/
theorem no_reload_loses : ∃ s, Reach .noReload s ∧ s.pc 1 = .lost := by
  -- Reopen 1 loads the names before the move: only "A".
  have r1 := Reach.step Reach.init (Step.load (v := .noReload) St.init 1 rfl)
  -- The move records both names ...
  have r2 := Reach.step r1 (Step.move _ (by simp [St.init]))
  -- ... the rename adds "B" ...
  have r3 := Reach.step r2 (Step.move _ (by simp [St.init]))
  -- ... takes "A" away ...
  have r4 := Reach.step r3 (Step.move _ (by simp [St.init]))
  -- ... and records only "B".
  have r5 := Reach.step r4 (Step.move _ (by simp [St.init]))
  -- Reopen 1 tries "A": gone.
  have r6 := Reach.step r5 (Step.cur _ 1 (by simp [St.init, version, namesAt]))
  -- It gives up without looking at the names again.
  have r7 := Reach.step r6 (Step.reload _ 1 (by simp [St.init, version, namesAt, holds]))
  -- That final state is the witness.
  exact ⟨_, r7, by simp [St.init, version, namesAt, holds]⟩

-- The end of this file's names.
end Regolith.EnvFilesRename
