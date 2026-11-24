/-!
# EnvFiles: a read sees every write that finished before it

This file backs the TLA+ model `proofs/tla/EnvFiles.tla`. TLC checks the
protocol there for two or three writers and a reader or two. Here the same
rules are proved for every number of readers, writers and steps.

## The story, for a reader who has never seen the code

regolith reads and writes files through an `Env`. Every `Env` must keep one
rule: a read sees the bytes of every write that finished before it began,
at the offset it asked for.

Part 1, the disk. An open file has one shared "cursor". Reading by "seek,
then read" is two steps on that one cursor, so two readers can step on each
other: A seeks to 0, B seeks to 1, A reads at 1. A positional read (`pread`,
`StdReadFile` in `src/env/std_env.rs`) names its offset in one call.

Part 2, a file in memory (`MemFile`, `src/env/mem_file.rs`). The file is a
buffer (an *extent*) with a published length; readers read below it with no
lock. An append that fits takes a WRITER flag, writes its record at the
length, and publishes length + 1. Anything else builds a new extent from the
old one's published records plus its own, FREEZES the old extent (no append
may publish there after that), and swaps the new extent in. A writer whose
publish meets a freeze writes again, on the new extent.

## What is proved, in plain words

1. `positional_reads_own`: with positional reads, every disk reader gets
   the bytes at its own offset, whatever the order (TLA+ `ReadsOwnBytes`).
2. `current_holds_finished`: every append that finished is in the current
   extent's published records (TLA+ `CurrentHoldsFinished`).
3. `read_sees_finished`: a memory read that began after an append finished
   returns that append's record (TLA+ `ReadSeesFinished`).
4. The RED cases, as counterexamples: a shared cursor hands a reader
   another offset's bytes (`shared_cursor_reads_other`); a copy that swaps
   without freezing loses a finished append (`no_freeze_loses_append`).

## How to read the Lean

`def` defines a thing, `theorem` states a fact and its proof follows
`:= by`. Lines starting with `--` are comments, in plain words. A proof is a
list of *tactics*; each one changes the goal still to be shown, and the
comment above it says how. `simp` rewrites with known facts; `omega` solves
arithmetic over natural numbers; `cases` splits on the ways a fact could be
true; `induction` proves a fact for every number of steps by proving it for
none and then for one more.
-/

-- Everything below is named Regolith.EnvFiles.<name>.
namespace Regolith.EnvFiles

/-- `f` with its value at `k` replaced by `v`. -/
def set {α : Type} (f : Nat → α) (k : Nat) (v : α) : Nat → α :=
  -- At `k` the new value, everywhere else the old one.
  fun x => if x = k then v else f x

/-- Reading `set` at the key it changed gives the new value. -/
@[simp] theorem set_same {α : Type} (f : Nat → α) (k : Nat) (v : α) :
    -- The claim itself; its proof follows.
    set f k v k = v := by
  -- Unfold `set`; the condition `k = k` holds.
  simp [set]

/-- Reading `set` anywhere else gives the old value. -/
theorem set_other {α : Type} (f : Nat → α) (k x : Nat) (v : α) (h : x ≠ k) :
    -- The claim itself; its proof follows.
    set f k v x = f x := by
  -- Unfold `set`; the condition `x = k` fails by `h`.
  simp [set, h]

/-! ## Part 1. Disk reads: positional, or on a shared cursor -/

/-- One disk read event. -/
inductive DiskEv where
  /-- Reader `r` reads at its own offset in one call (`pread`). -/
  | pread (r : Nat)
  /-- Reader `r` moves the file's one cursor to its offset. -/
  | seek (r : Nat)
  /-- Reader `r` reads at the cursor, wherever it is now. -/
  | readCursor (r : Nat)

/-- What the disk side holds: the shared cursor, and what each reader got
(`none` until it reads). -/
structure Disk where
  /-- The file's one shared position. -/
  cursor : Nat
  /-- Each reader's answer. -/
  got : Nat → Option Nat

/-- One event, on a file whose bytes at offset `o` are `data o`, read by
readers whose offsets are `at_`. -/
def Disk.step (data at_ : Nat → Nat) (d : Disk) : DiskEv → Disk
  -- A positional read: the bytes at the reader's own offset.
  | .pread r => { d with got := set d.got r (some (data (at_ r))) }
  -- A seek: the shared cursor moves to the reader's offset.
  | .seek r => { d with cursor := at_ r }
  -- A cursor read: the bytes wherever the cursor is.
  | .readCursor r => { d with got := set d.got r (some (data d.cursor)) }

/-- Run a schedule of positional reads, one reader after another. -/
def runPread (data at_ : Nat → Nat) (d : Disk) : List Nat → Disk
  -- No more reads: the disk as it is.
  | [] => d
  -- Reader `r` reads, then the rest of the schedule runs.
  | r :: rest => runPread data at_ (d.step data at_ (.pread r)) rest

/-- **`positional_reads_own`.** Whatever order positional reads run in, every
reader that got something got the bytes at its own offset. Rules out the
story's reader A getting offset 1's bytes. -/
theorem positional_reads_own (data at_ : Nat → Nat) (sched : List Nat) (d : Disk)
    -- Before the schedule, every answer is a reader's own bytes.
    (h0 : ∀ r v, d.got r = some v → v = data (at_ r)) :
    -- The claim itself; its proof follows.
    ∀ r v, (runPread data at_ d sched).got r = some v → v = data (at_ r) := by
  -- By the schedule, one read at a time, for any starting disk.
  induction sched generalizing d with
  -- Case: no read at all.
  | nil =>
    -- No read: the starting fact.
    exact h0
  -- Case: one read, then the rest, with the claim for the rest (ih) known.
  | cons q rest ih =>
    -- One read by `q`, then the rest: show the fact after `q`'s read.
    apply ih
    -- After `q` reads, each answer is still its reader's own bytes.
    intro r v hv
    -- Is `r` the reader who just read?
    by_cases hrq : r = q
    -- Yes: it got `data (at_ q)`, its own bytes.
    ·
      -- Replace one side of `hrq` by the other everywhere.
      subst hrq
      -- Simplify the named facts with the definitions; a false one closes the goal.
      simp [Disk.step, set_same] at hv
      -- This is exactly what the goal asks.
      exact hv.symm
    -- No: its answer did not change.
    ·
      -- Read through `set` at a key it did not change in `hv`: the old value.
      simp only [Disk.step, set_other _ _ _ _ hrq] at hv
      -- This is exactly what the goal asks.
      exact h0 r v hv

/-- **`shared_cursor_reads_other`.** With one shared cursor, reader 1 seeks
to offset 0, reader 2 seeks to offset 1, and reader 1 reads: it gets offset
1's bytes (101), not its own (100). -/
theorem shared_cursor_reads_other :
    -- Name a value the claim below uses.
    let data := fun o => 100 + o
    -- Name a value the claim below uses.
    let at_ := fun r => r - 1
    -- Name a value the claim below uses.
    let d0 : Disk := ⟨0, fun _ => none⟩
    -- Name a value the claim below uses.
    let d3 := ((d0.step data at_ (.seek 1)).step data at_ (.seek 2)).step data at_ (.readCursor 1)
    -- The claim itself; its proof follows.
    d3.got 1 = some 101 ∧ data (at_ 1) = 100 := by
  -- Every value is a small number: compute it.
  simp [Disk.step, set]

/-! ## Part 2. The in-memory file -/

/-- One buffer of the file (`Extent`): its records by position, its
published length, and the two flags of its state word. -/
structure Ext where
  /-- The record at each position. Below `len` it never changes. -/
  buf : Nat → Nat
  /-- The published length. -/
  len : Nat
  /-- The WRITER bit: an append in place holds the spare room. -/
  wbit : Bool
  /-- The FROZEN bit: no append publishes here any more. -/
  frozen : Bool

/-- Where a writer is in its append. Writer `w` appends the record `w`. -/
inductive WPc where
  /-- About to append. -/
  | start
  /-- Took WRITER and wrote its record above the length (in place). -/
  | written
  /-- Built a new extent (copy). -/
  | built
  /-- Froze the old extent, or found it frozen when it built (copy). -/
  | frz
  /-- Its append finished. -/
  | done
  -- Two steps can be compared for equality.
  deriving DecidableEq

/-- Where a reader is in its read. -/
inductive RPc where
  /-- Not begun. -/
  | start
  /-- Begun: it knows which appends had finished. -/
  | begun
  /-- Loaded the current extent. -/
  | loaded
  /-- Loaded that extent's length. -/
  | lenRead
  /-- Copied the records below the length. -/
  | done
  -- Two steps can be compared for equality.
  deriving DecidableEq

/-- Everything about the in-memory file at one moment. -/
structure St where
  /-- The extent readers load (`MemFile::current`). -/
  cur : Nat
  /-- Every extent, by number. -/
  ext : Nat → Ext
  /-- The next new extent's number; every extent in use is below it. -/
  next : Nat
  /-- The appends that returned. -/
  finished : List Nat
  /-- Each writer's step. -/
  wpc : Nat → WPc
  /-- The extent each writer works on. -/
  wext : Nat → Nat
  /-- The length each writer saw. -/
  wlen : Nat → Nat
  /-- The WRITER bit a copier saw when it built. -/
  wsawBit : Nat → Bool
  /-- The FROZEN bit a copier saw when it built. -/
  wsawFrz : Nat → Bool
  /-- The records of each copier's new extent. -/
  newBuf : Nat → Nat → Nat
  /-- Each reader's step. -/
  rpc : Nat → RPc
  /-- The appends that had finished when each reader began. -/
  rfin : Nat → List Nat
  /-- The extent each reader loaded. -/
  rext : Nat → Nat
  /-- The length each reader loaded. -/
  rlen : Nat → Nat

/-- An empty extent. -/
def Ext.empty : Ext := ⟨fun _ => 0, 0, false, false⟩

/-- The start: one empty extent, number 0, and nobody has done anything. -/
def St.init : St :=
  -- Extent 0 is current; every number holds an empty extent.
  { cur := 0, ext := fun _ => Ext.empty, next := 1, finished := [],
    -- Every writer and reader is at its start.
    wpc := fun _ => .start, wext := fun _ => 0, wlen := fun _ => 0,
    wsawBit := fun _ => false, wsawFrz := fun _ => false,
    newBuf := fun _ _ => 0,
    rpc := fun _ => .start, rfin := fun _ => [], rext := fun _ => 0, rlen := fun _ => 0 }

/-- Record `x` is published in extent `e`: at some position below its
length. -/
def St.inPub (s : St) (e x : Nat) : Prop :=
  -- Some position below the length holds `x`.
  ∃ i, i < (s.ext e).len ∧ (s.ext e).buf i = x

/-- The steps, one atomic step each, as `MemFile` takes them. -/
inductive Step : St → St → Prop
  /-- Writer `w` takes WRITER on the current extent with a CAS that needs
  neither flag set, and writes its record at the length (`in_place`). -/
  | claimWrite (s : St) (w : Nat)
      -- The writer is about to append.
      (hpc : s.wpc w = .start)
      -- The current extent has no WRITER and is not frozen.
      (hb : (s.ext s.cur).wbit = false) (hf : (s.ext s.cur).frozen = false) :
      -- The state after this step: only the fields named here change.
      Step s { s with
        -- WRITER is set and the record sits at the length.
        ext := set s.ext s.cur { s.ext s.cur with
          buf := set (s.ext s.cur).buf (s.ext s.cur).len w, wbit := true },
        -- The writer remembers the extent and the length.
        wext := set s.wext w s.cur, wlen := set s.wlen w (s.ext s.cur).len,
        wpc := set s.wpc w .written }
  /-- The publishing CAS wins: the extent is not frozen, so the length goes
  one up, WRITER clears, and the append has finished. -/
  | publishOk (s : St) (w : Nat)
      -- The writer wrote its record.
      (hpc : s.wpc w = .written)
      -- Its extent is not frozen.
      (hf : (s.ext (s.wext w)).frozen = false) :
      -- The state after this step: only the fields named here change.
      Step s { s with
        -- One more record published, WRITER cleared.
        ext := set s.ext (s.wext w) { s.ext (s.wext w) with
          len := s.wlen w + 1, wbit := false },
        -- The append returned.
        finished := w :: s.finished, wpc := set s.wpc w .done }
  /-- The publishing CAS fails on a frozen extent: start over. -/
  | publishFail (s : St) (w : Nat)
      -- The writer wrote its record.
      (hpc : s.wpc w = .written)
      -- Its extent was frozen meanwhile.
      (hf : (s.ext (s.wext w)).frozen = true) :
      -- The state after this step: only the fields named here change.
      Step s { s with wpc := set s.wpc w .start }
  /-- A copier builds a new extent: the current one's published records,
  then its own; it remembers the state word it saw (`copy`). -/
  | build (s : St) (w : Nat)
      -- The writer is about to append.
      (hpc : s.wpc w = .start) :
      -- The state after this step: only the fields named here change.
      Step s { s with
        -- Positions below the length copied, then the writer's record.
        newBuf := set s.newBuf w (fun j =>
          if j < (s.ext s.cur).len then (s.ext s.cur).buf j else w),
        -- The extent, its length and its two flags, as seen.
        wext := set s.wext w s.cur, wlen := set s.wlen w (s.ext s.cur).len,
        wsawBit := set s.wsawBit w (s.ext s.cur).wbit,
        wsawFrz := set s.wsawFrz w (s.ext s.cur).frozen,
        wpc := set s.wpc w .built }
  /-- The extent was frozen when the copier built: its length was final,
  nothing to freeze. -/
  | freezeSkip (s : St) (w : Nat)
      -- The copier built.
      (hpc : s.wpc w = .built)
      -- It saw FROZEN.
      (hz : s.wsawFrz w = true) :
      -- The state after this step: only the fields named here change.
      Step s { s with wpc := set s.wpc w .frz }
  /-- The freezing CAS wins: the state word is still the one the copier saw
  (same length, same WRITER bit, not frozen). -/
  | freezeOk (s : St) (w : Nat)
      -- The copier built from an extent it saw unfrozen.
      (hpc : s.wpc w = .built) (hz : s.wsawFrz w = false)
      -- The word is unchanged.
      (hl : (s.ext (s.wext w)).len = s.wlen w)
      -- Another premise of the claim, named so the proof can use it.
      (hb : (s.ext (s.wext w)).wbit = s.wsawBit w)
      -- Another premise of the claim, named so the proof can use it.
      (hf : (s.ext (s.wext w)).frozen = false) :
      -- The state after this step: only the fields named here change.
      Step s { s with
        -- FROZEN is set.
        ext := set s.ext (s.wext w) { s.ext (s.wext w) with frozen := true },
        wpc := set s.wpc w .frz }
  /-- The freezing CAS fails: the word changed. Start over. -/
  | freezeFail (s : St) (w : Nat)
      -- The copier built from an extent it saw unfrozen.
      (hpc : s.wpc w = .built) (hz : s.wsawFrz w = false)
      -- The word is not the one it saw.
      (hch : ¬ ((s.ext (s.wext w)).len = s.wlen w ∧
                (s.ext (s.wext w)).wbit = s.wsawBit w ∧
                (s.ext (s.wext w)).frozen = false)) :
      -- The state after this step: only the fields named here change.
      Step s { s with wpc := set s.wpc w .start }
  /-- The swap CAS wins: the file still points at the old extent, so the new
  one becomes current, with every record published, and the append has
  finished. -/
  | swapOk (s : St) (w : Nat)
      -- The copier froze, or found frozen.
      (hpc : s.wpc w = .frz)
      -- The pointer still names its old extent.
      (hc : s.cur = s.wext w) :
      -- The state after this step: only the fields named here change.
      Step s { s with
        -- The new extent: the copier's records, all published.
        ext := set s.ext s.next ⟨s.newBuf w, s.wlen w + 1, false, false⟩,
        -- It is current, and the next number moves on.
        cur := s.next, next := s.next + 1,
        -- The append returned.
        finished := w :: s.finished, wpc := set s.wpc w .done }
  /-- The swap CAS fails: another copy swapped first. Start over. -/
  | swapFail (s : St) (w : Nat)
      -- The copier froze, or found frozen.
      (hpc : s.wpc w = .frz)
      -- The pointer moved on.
      (hc : s.cur ≠ s.wext w) :
      -- The state after this step: only the fields named here change.
      Step s { s with wpc := set s.wpc w .start }
  /-- Reader `r` begins: it notes the appends finished by now. -/
  | begin (s : St) (r : Nat) (hpc : s.rpc r = .start) :
      -- The state after this step: only the fields named here change.
      Step s { s with rfin := set s.rfin r s.finished, rpc := set s.rpc r .begun }
  /-- Reader `r` loads the current extent. -/
  | load (s : St) (r : Nat) (hpc : s.rpc r = .begun) :
      -- The state after this step: only the fields named here change.
      Step s { s with rext := set s.rext r s.cur, rpc := set s.rpc r .loaded }
  /-- Reader `r` loads that extent's length. -/
  | lenRead (s : St) (r : Nat) (hpc : s.rpc r = .loaded) :
      -- The state after this step: only the fields named here change.
      Step s { s with rlen := set s.rlen r (s.ext (s.rext r)).len,
                      rpc := set s.rpc r .lenRead }
  /-- Reader `r` copies the records below that length: its answer is the
  records of `rext r` below `rlen r`. -/
  | copy (s : St) (r : Nat) (hpc : s.rpc r = .lenRead) :
      -- The state after this step: only the fields named here change.
      Step s { s with rpc := set s.rpc r .done }

/-- The states the steps can reach from the start. -/
inductive Reach : St → Prop
  /-- The start is reachable. -/
  | init : Reach St.init
  /-- One step from a reachable state reaches another. -/
  | step {s t : St} : Reach s → Step s t → Reach t

/-- The facts every reachable state has. -/
structure Inv (s : St) : Prop where
  /-- The current extent is in use. -/
  cur_lt : s.cur < s.next
  /-- Every other extent in use is frozen: only the current one takes
  appends. -/
  old_frozen : ∀ e, e < s.next → e ≠ s.cur → (s.ext e).frozen = true
  /-- An in-place writer holds WRITER on its extent, at the length it saw,
  with its record written there. -/
  writer : ∀ w, s.wpc w = .written → s.wext w < s.next ∧ (s.ext (s.wext w)).wbit = true ∧
    (s.ext (s.wext w)).len = s.wlen w ∧ (s.ext (s.wext w)).buf (s.wlen w) = w
  /-- At most one in-place writer per extent. -/
  one_writer : ∀ w v, s.wpc w = .written → s.wpc v = .written → s.wext w = s.wext v → w = v
  /-- A copier's new extent copies its old one's published records below
  the length it saw, then holds its own record. -/
  copier : ∀ w, (s.wpc w = .built ∨ s.wpc w = .frz) → s.wext w < s.next ∧
    s.wlen w ≤ (s.ext (s.wext w)).len ∧
    (∀ j, j < s.wlen w → s.newBuf w j = (s.ext (s.wext w)).buf j) ∧ s.newBuf w (s.wlen w) = w
  /-- A copier that saw FROZEN: the extent is frozen at the length it saw. -/
  saw_frozen : ∀ w, s.wpc w = .built → s.wsawFrz w = true →
    (s.ext (s.wext w)).frozen = true ∧ (s.ext (s.wext w)).len = s.wlen w
  /-- A copier past its freeze: the extent is frozen at the length it saw. -/
  frozen_at : ∀ w, s.wpc w = .frz →
    (s.ext (s.wext w)).frozen = true ∧ (s.ext (s.wext w)).len = s.wlen w
  /-- Every finished append is published in the current extent. -/
  holds : ∀ x ∈ s.finished, s.inPub s.cur x
  /-- A reader that began knows only finished appends. -/
  r_begun : ∀ r, s.rpc r = .begun → ∀ x ∈ s.rfin r, x ∈ s.finished
  /-- A reader that loaded holds an extent where all it must see is
  published. -/
  r_loaded : ∀ r, s.rpc r = .loaded → s.rext r < s.next ∧ ∀ x ∈ s.rfin r, s.inPub (s.rext r) x
  /-- A reader that loaded its length sees all it must below that length. -/
  r_len : ∀ r, (s.rpc r = .lenRead ∨ s.rpc r = .done) → s.rext r < s.next ∧
    s.rlen r ≤ (s.ext (s.rext r)).len ∧
    ∀ x ∈ s.rfin r, ∃ i, i < s.rlen r ∧ (s.ext (s.rext r)).buf i = x

/-- What a step does to every extent already in use: its length never goes
down, its records below the length never change, and a frozen extent stays
frozen at its length. Readers rely on all three. -/
theorem step_grows {s t : St} (hs : Inv s) (hst : Step s t) (e : Nat) (he : e < s.next) :
    -- The claim, which continues on the lines below.
    s.next ≤ t.next ∧ (s.ext e).len ≤ (t.ext e).len ∧
    (∀ i, i < (s.ext e).len → (t.ext e).buf i = (s.ext e).buf i) ∧
    ((s.ext e).frozen = true → (t.ext e).frozen = true ∧ (t.ext e).len = (s.ext e).len) := by
  -- One case per kind of step.
  cases hst with
  -- Case: a writer claims the current extent and writes its record after the published ones.
  | claimWrite w hpc hb hf =>
    -- Only the current extent changes: its record goes at the length.
    by_cases hec : e = s.cur
    -- The current extent: same length, same records below it, not frozen.
    ·
      -- Replace one side of `hec` by the other everywhere.
      subst hec
      -- Prove the parts one at a time; each `?_` becomes a goal of its own.
      refine ⟨Nat.le_refl _, ?_, ?_, ?_⟩
      -- The length is unchanged.
      · simp
      -- Below the length, the new record's position is not reached.
      · intro i hi; simp [set_other _ _ _ _ (Nat.ne_of_lt hi)]
      -- It was not frozen, so there is nothing to keep.
      · intro hfz; rw [hf] at hfz; cases hfz
    -- Any other extent is untouched.
    ·
      -- Read through `set` at a key it did not change: the old value.
      simp [set_other _ _ _ _ hec]
  -- Case: an in-place writer publishes its record.
  | publishOk w hpc hf =>
    -- Only the writer's extent changes: one more record published.
    have hw := hs.writer w hpc
    -- Split on whether `e = s.wext w` holds.
    by_cases hew : e = s.wext w
    -- The writer's extent: the length was `wlen`, now `wlen + 1`.
    ·
      -- Replace one side of `hew` by the other everywhere.
      subst hew
      -- Prove the parts one at a time; each `?_` becomes a goal of its own.
      refine ⟨Nat.le_refl _, ?_, ?_, ?_⟩
      -- The length grows by one.
      · simp; omega
      -- The records are unchanged.
      · intro i _; simp
      -- It was not frozen, so there is nothing to keep.
      · intro hfz; rw [hf] at hfz; cases hfz
    -- Any other extent is untouched.
    ·
      -- Read through `set` at a key it did not change: the old value.
      simp [set_other _ _ _ _ hew]
  -- Case: an in-place writer finds its extent frozen and starts over.
  | publishFail w hpc hf =>
    -- No extent changes.
    simp
  -- Case: a writer builds a copy of the current extent with its record added.
  | build w hpc =>
    -- No extent changes.
    simp
  -- Case: a copier that saw its extent frozen already goes straight on to the swap.
  | freezeSkip w hpc hz =>
    -- No extent changes.
    simp
  -- Case: a copier freezes its extent.
  | freezeOk w hpc hz hl hb hf =>
    -- Only the frozen flag of the copier's extent changes.
    by_cases hew : e = s.wext w
    -- The copier's extent: same length and records, now frozen.
    ·
      -- Replace one side of `hew` by the other everywhere.
      subst hew
      -- Prove the parts one at a time; each `?_` becomes a goal of its own.
      refine ⟨Nat.le_refl _, ?_, ?_, ?_⟩
      -- The length is unchanged.
      · simp
      -- The records are unchanged.
      · intro i _; simp
      -- Frozen, at the same length.
      · intro _; simp
    -- Any other extent is untouched.
    ·
      -- Read through `set` at a key it did not change: the old value.
      simp [set_other _ _ _ _ hew]
  -- Case: a copier finds its extent's state word changed and starts over.
  | freezeFail w hpc hz hch =>
    -- No extent changes.
    simp
  -- Case: a copier swaps its new extent in.
  | swapOk w hpc hc =>
    -- Only the new extent, numbered `next`, is written: `e` is below it.
    have hne : e ≠ s.next := Nat.ne_of_lt he
    -- Prove the parts one at a time; each `?_` becomes a goal of its own.
    refine ⟨Nat.le_succ _, ?_, ?_, ?_⟩
    -- The length of `e` is unchanged.
    · simp [set_other _ _ _ _ hne]
    -- The records of `e` are unchanged.
    · intro i _; simp [set_other _ _ _ _ hne]
    -- The flags of `e` are unchanged.
    · intro hfz; simp [set_other _ _ _ _ hne, hfz]
  -- Case: a copier finds the pointer moved and starts over.
  | swapFail w hpc hc =>
    -- No extent changes.
    simp
  -- Case: a memory reader begins, noting the appends that have returned.
  | begin r hpc =>
    -- No extent changes.
    simp
  -- Case: a memory reader loads the pointer.
  | load r hpc =>
    -- No extent changes.
    simp
  -- Case: a memory reader reads its extent's published length.
  | lenRead r hpc =>
    -- No extent changes.
    simp
  -- Case: a memory reader copies the published records.
  | copy r hpc =>
    -- No extent changes.
    simp

/-- Published records stay published: a record at a position below an
extent's length is still there, below its length, after any step. -/
theorem inPub_step {s t : St} (hs : Inv s) (hst : Step s t) {e x : Nat} (he : e < s.next)
    -- Another premise of the claim, named so the proof can use it.
    (hx : s.inPub e x) : t.inPub e x := by
  -- The position that held `x` before.
  obtain ⟨i, hi, hix⟩ := hx
  -- The step keeps lengths up and records below them.
  obtain ⟨_, hlen, hbuf, _⟩ := step_grows hs hst e he
  -- The same position still holds `x`, still below the length.
  exact ⟨i, Nat.lt_of_lt_of_le hi hlen, by rw [hbuf i hi]; exact hix⟩

/-- The start keeps every fact: nobody has done anything. -/
theorem inv_init : Inv St.init := by
  -- Each field, about the start, holds at once.
  constructor
  -- Extent 0 is current and below 1.
  · decide
  -- The only extent in use is the current one.
  · intro e he hne; simp [St.init] at he hne; omega
  -- No writer has written.
  · intro w h; simp [St.init] at h
  -- No writer has written.
  · intro w v h; simp [St.init] at h
  -- No copier has built.
  · intro w h; simp [St.init] at h
  -- No copier has built.
  · intro w h; simp [St.init] at h
  -- No copier has frozen.
  · intro w h; simp [St.init] at h
  -- No append has finished.
  · intro x hx; simp [St.init] at hx
  -- No reader has begun.
  · intro r h; simp [St.init] at h
  -- No reader has loaded.
  · intro r h; simp [St.init] at h
  -- No reader has loaded a length.
  · intro r h; simp [St.init] at h

/-- The next number only grows, so an extent in use stays in use. -/
theorem next_grows {s t : St} (hs : Inv s) (hst : Step s t) : s.next ≤ t.next :=
  -- `step_grows` says so for the current extent, which is in use.
  (step_grows hs hst s.cur hs.cur_lt).1

/-- The current extent stays in use. -/
theorem cur_lt_step {s t : St} (hs : Inv s) (hst : Step s t) : t.cur < t.next := by
  -- Only a winning swap moves the pointer, to the old `next`.
  cases hst <;> first
    -- The swap: the new current is `next`, below `next + 1`.
    | (simp only; omega)
    -- Every other step leaves the pointer and the number as they were.
    | exact hs.cur_lt

/-- Every extent in use but the current one stays frozen. -/
theorem old_frozen_step {s t : St} (hs : Inv s) (hst : Step s t) :
    -- The claim itself; its proof follows.
    ∀ e, e < t.next → e ≠ t.cur → (t.ext e).frozen = true := by
  -- Keep the step itself for the cases that only need `step_grows`.
  have hst' := hst
  -- One case per kind of step.
  cases hst with
  -- Case: a copier swaps its new extent in.
  | swapOk w hpc hc =>
    -- The new current extent is `next`; every other one is below it.
    intro e he hne
    -- Read off the fields of the record the step wrote.
    simp only at he hne
    -- So `e` is below the old `next`, and is not the new extent.
    have hlt : e < s.next := by omega
    -- The new extent is not `e`: `e` keeps its flags.
    simp only [set_other _ _ _ _ hne]
    -- Was `e` the old current extent?
    by_cases hec : e = s.cur
    -- Yes: the copier froze it before the swap.
    ·
      -- Rewrite the goal with the equalities given, then this is exactly what the goal asks.
      rw [hec, hc]; exact (hs.frozen_at w hpc).1
    -- No: it was frozen already.
    ·
      -- This is exactly what the goal asks.
      exact hs.old_frozen e hlt hec
  -- Case: every other kind of step.
  | _ =>
    -- Every other step leaves the pointer and the number as they were, and
    -- only sets flags or extends an extent: frozen stays frozen.
    intro e he hne
    -- This is exactly what the goal asks.
    exact ((step_grows hs hst' e he).2.2.2 (hs.old_frozen e he hne)).1

/-- A writer that is not `w0` keeps its step after a step that only moved
`w0`. -/
theorem wpc_other {s : St} {w0 w : Nat} {p : WPc} (h : w ≠ w0) :
    -- The claim itself; its proof follows.
    set s.wpc w0 p w = s.wpc w :=
  -- `set` changes only `w0`.
  set_other _ _ _ _ h

/-- In-place writers keep their WRITER, length and record. -/
theorem writer_step {s t : St} (hs : Inv s) (hst : Step s t) :
    -- The claim, which continues on the lines below.
    ∀ w, t.wpc w = .written → t.wext w < t.next ∧ (t.ext (t.wext w)).wbit = true ∧
      (t.ext (t.wext w)).len = t.wlen w ∧ (t.ext (t.wext w)).buf (t.wlen w) = w := by
  -- One case per kind of step.
  cases hst with
  -- Case: a writer claims the current extent and writes its record after the published ones.
  | claimWrite w0 hpc hb hf =>
    -- Name what the goal starts with: `w`, `hw`.
    intro w hw
    -- Is `w` the writer that just claimed?
    by_cases hww : w = w0
    -- Yes: its extent is the current one, now with WRITER and its record.
    ·
      -- Replace one side of `hww` by the other everywhere.
      subst hww
      -- Read the value `set` just wrote at this key.
      simp only [set_same]
      -- Put the pieces together.
      exact ⟨hs.cur_lt, by simp, by simp, by simp⟩
    -- No: it was writing before, on an extent with WRITER set, which is.
    ·
      -- therefore not the current one (that one had no WRITER).
      simp only [set_other _ _ _ _ hww] at hw ⊢
      -- Unpack the facts on the right and name them.
      obtain ⟨hlt, hwb, hl, hbuf⟩ := hs.writer w hw
      -- First show this small fact; its proof follows on the next lines.
      have hne : s.wext w ≠ s.cur := by
        -- Name what the goal starts with: `heq`, then rewrite `hwb` with the equalities given, then
        -- split on the ways `hwb` can hold: none is left.
        intro heq; rw [heq, hb] at hwb; cases hwb
      -- Read through `set` at a key it did not change: the old value.
      simp only [set_other _ _ _ _ hne]
      -- Put the pieces together.
      exact ⟨hlt, hwb, hl, hbuf⟩
  -- Case: an in-place writer publishes its record.
  | publishOk w0 hpc hf =>
    -- Name what the goal starts with: `w`, `hw`.
    intro w hw
    -- `w0` is done now, so `w` is another writer.
    have hww : w ≠ w0 := by intro h; subst h; simp at hw
    -- Read through `set` at a key it did not change: the old value.
    simp only [set_other _ _ _ _ hww] at hw
    -- It writes on another extent than `w0`'s (one writer per extent).
    have hne : s.wext w ≠ s.wext w0 := fun h => hww (hs.one_writer w w0 hw hpc h)
    -- Read through `set` at a key it did not change: the old value.
    simp only [set_other _ _ _ _ hne]
    -- The state's `writer` fact from before the step gives exactly this.
    exact hs.writer w hw
  -- Case: an in-place writer finds its extent frozen and starts over.
  | publishFail w0 hpc hf =>
    -- Name what the goal starts with: `w`, `hw`.
    intro w hw
    -- `w0` starts over, so `w` is another writer, untouched.
    have hww : w ≠ w0 := by intro h; subst h; simp at hw
    -- Read through `set` at a key it did not change: the old value.
    simp only [set_other _ _ _ _ hww] at hw
    -- The state's `writer` fact from before the step gives exactly this.
    exact hs.writer w hw
  -- Case: a writer builds a copy of the current extent with its record added.
  | build w0 hpc =>
    -- Name what the goal starts with: `w`, `hw`.
    intro w hw
    -- `w0` is a copier now, so `w` is another writer, untouched.
    have hww : w ≠ w0 := by intro h; subst h; simp at hw
    -- Read through `set` at a key it did not change: the old value.
    simp only [set_other _ _ _ _ hww] at hw ⊢
    -- The state's `writer` fact from before the step gives exactly this.
    exact hs.writer w hw
  -- Case: a copier that saw its extent frozen already goes straight on to the swap.
  | freezeSkip w0 hpc hz =>
    -- Name what the goal starts with: `w`, `hw`.
    intro w hw
    -- `w0` is a copier, so `w` is another writer, untouched.
    have hww : w ≠ w0 := by intro h; subst h; simp at hw
    -- Read through `set` at a key it did not change: the old value.
    simp only [set_other _ _ _ _ hww] at hw
    -- The state's `writer` fact from before the step gives exactly this.
    exact hs.writer w hw
  -- Case: a copier freezes its extent.
  | freezeOk w0 hpc hz hl hb hf =>
    -- Name what the goal starts with: `w`, `hw`.
    intro w hw
    -- `w0` is a copier, so `w` is another writer.
    have hww : w ≠ w0 := by intro h; subst h; simp at hw
    -- Read through `set` at a key it did not change: the old value.
    simp only [set_other _ _ _ _ hww] at hw
    -- Unpack the facts on the right and name them.
    obtain ⟨hlt, hwb, hlen, hbuf⟩ := hs.writer w hw
    -- The freeze changes only the frozen flag, whichever extent it is.
    by_cases he : s.wext w = s.wext w0
    -- Simplify with the definitions until the goal is closed or plain, then rewrite `hwb`, `hlen`, `hbuf` with the equalities given, then put the pieces together.
    · simp only [he, set_same]; rw [he] at hwb hlen hbuf; exact ⟨he ▸ hlt, hwb, hlen, hbuf⟩
    -- Read through `set` at a key it did not change: the old value, then put the pieces together.
    · simp only [set_other _ _ _ _ he]; exact ⟨hlt, hwb, hlen, hbuf⟩
  -- Case: a copier finds its extent's state word changed and starts over.
  | freezeFail w0 hpc hz hch =>
    -- Name what the goal starts with: `w`, `hw`.
    intro w hw
    -- `w0` starts over, so `w` is another writer, untouched.
    have hww : w ≠ w0 := by intro h; subst h; simp at hw
    -- Read through `set` at a key it did not change: the old value.
    simp only [set_other _ _ _ _ hww] at hw
    -- The state's `writer` fact from before the step gives exactly this.
    exact hs.writer w hw
  -- Case: a copier swaps its new extent in.
  | swapOk w0 hpc hc =>
    -- Name what the goal starts with: `w`, `hw`.
    intro w hw
    -- `w0` is done, so `w` is another writer.
    have hww : w ≠ w0 := by intro h; subst h; simp at hw
    -- Read through `set` at a key it did not change: the old value.
    simp only [set_other _ _ _ _ hww] at hw
    -- Unpack the facts on the right and name them.
    obtain ⟨hlt, hwb, hlen, hbuf⟩ := hs.writer w hw
    -- Its extent is below `next`, so not the new one.
    have hne : s.wext w ≠ s.next := Nat.ne_of_lt hlt
    -- Read through `set` at a key it did not change: the old value.
    simp only [set_other _ _ _ _ hne]
    -- Put the pieces together.
    exact ⟨by omega, hwb, hlen, hbuf⟩
  -- Case: a copier finds the pointer moved and starts over.
  | swapFail w0 hpc hc =>
    -- Name what the goal starts with: `w`, `hw`.
    intro w hw
    -- `w0` starts over, so `w` is another writer, untouched.
    have hww : w ≠ w0 := by intro h; subst h; simp at hw
    -- Read through `set` at a key it did not change: the old value.
    simp only [set_other _ _ _ _ hww] at hw
    -- The state's `writer` fact from before the step gives exactly this.
    exact hs.writer w hw
  -- Case: a memory reader begins, noting the appends that have returned.
  | begin r hpc =>
    -- Readers do not touch writers.
    exact hs.writer
  -- Case: a memory reader loads the pointer.
  | load r hpc =>
    -- Readers do not touch writers.
    exact hs.writer
  -- Case: a memory reader reads its extent's published length.
  | lenRead r hpc =>
    -- Readers do not touch writers.
    exact hs.writer
  -- Case: a memory reader copies the published records.
  | copy r hpc =>
    -- Readers do not touch writers.
    exact hs.writer

/-- At most one in-place writer per extent, after any step. -/
theorem one_writer_step {s t : St} (hs : Inv s) (hst : Step s t) :
    -- The claim itself; its proof follows.
    ∀ w v, t.wpc w = .written → t.wpc v = .written → t.wext w = t.wext v → w = v := by
  -- One case per kind of step.
  cases hst with
  -- Case: a writer claims the current extent and writes its record after the published ones.
  | claimWrite w0 hpc hb hf =>
    -- Name what the goal starts with: `w`, `v`, `hw`, `hv`, `he`.
    intro w v hw hv he
    -- A writer other than `w0` was writing on an extent with WRITER set,
    -- so not on the current one, which `w0` just claimed.
    have other : ∀ u, u ≠ w0 → s.wpc u = .written → s.wext u ≠ s.cur := by
      -- Name what the goal starts with: `u`, `hu`, `hup`, `heq`.
      intro u hu hup heq
      -- Keep this fact for the next step.
      have := (hs.writer u hup).2.1
      -- Rewrite `this` with the equalities given, then split on the ways `this` can hold: none is left.
      rw [heq, hb] at this; cases this
    -- Split on whether `w = w0 <` holds.
    by_cases hww : w = w0 <;> by_cases hvw : v = w0
    -- Both are `w0`.
    ·
      -- Rewrite the goal with the equalities given.
      rw [hww, hvw]
    -- `w` is `w0` on the current extent; `v` is not there.
    ·
      -- Replace one side of `hww` by the other everywhere.
      subst hww
      -- Read through `set`: the new value at the key it wrote, the old value elsewhere.
      simp only [set_same, set_other _ _ _ _ hvw] at hv he
      -- The two facts contradict each other.
      exact absurd he.symm (other v hvw hv)
    -- `v` is `w0` on the current extent; `w` is not there.
    ·
      -- Replace one side of `hvw` by the other everywhere.
      subst hvw
      -- Read through `set`: the new value at the key it wrote, the old value elsewhere.
      simp only [set_same, set_other _ _ _ _ hww] at hw he
      -- The two facts contradict each other.
      exact absurd he (other w hww hw)
    -- Neither is `w0`: as before.
    ·
      -- Read through `set` at a key it did not change in `hw hv he`: the old value.
      simp only [set_other _ _ _ _ hww, set_other _ _ _ _ hvw] at hw hv he
      -- The state's `one_writer` fact from before the step gives exactly this.
      exact hs.one_writer w v hw hv he
  -- Case: a writer builds a copy of the current extent with its record added.
  | build w0 hpc =>
    -- Name what the goal starts with: `w`, `v`, `hw`, `hv`, `he`.
    intro w v hw hv he
    -- `w0` is a copier now: both are other writers, untouched.
    have hww : w ≠ w0 := by intro h; subst h; simp at hw
    -- First show `v ≠ w0`, named `hvw`, then replace one side of `h` by the other everywhere, then simplify the named facts with the definitions; a false one closes the goal.
    have hvw : v ≠ w0 := by intro h; subst h; simp at hv
    -- Read through `set` at a key it did not change: the old value.
    simp only [set_other _ _ _ _ hww, set_other _ _ _ _ hvw] at hw hv he
    -- The state's `one_writer` fact from before the step gives exactly this.
    exact hs.one_writer w v hw hv he
  -- Case: every other kind of step.
  | _ =>
    -- Every other step moves at most one writer, off "written" (or not at
    -- all), and changes no writer's extent.
    intro w v hw hv he
    -- Try each way below in turn.
    first
      -- First way: the old fact applies as it is.
      | exact hs.one_writer w v hw hv he
      -- Second way, for a step that carries six arguments: name the writer that moved, w0.
      | (rename_i w0 _ _ _ _ _
         -- First show `w ≠ w0`, named `hww`, then replace one side of `h` by the other everywhere, then simplify the named facts with the definitions; a false one closes the goal.
         have hww : w ≠ w0 := by intro h; subst h; simp at hw
         -- First show `v ≠ w0`, named `hvw`, then replace one side of `h` by the other everywhere, then simplify the named facts with the definitions; a false one closes the goal.
         have hvw : v ≠ w0 := by intro h; subst h; simp at hv
         -- Read through `set` at a key it did not change: the old value.
         simp only [set_other _ _ _ _ hww, set_other _ _ _ _ hvw] at hw hv
         -- The state's `one_writer` fact from before the step gives exactly this.
         exact hs.one_writer w v hw hv he)
      -- Third way, for a step that carries four arguments: name the writer that moved, w0.
      | (rename_i w0 _ _ _
         -- First show `w ≠ w0`, named `hww`, then replace one side of `h` by the other everywhere, then simplify the named facts with the definitions; a false one closes the goal.
         have hww : w ≠ w0 := by intro h; subst h; simp at hw
         -- First show `v ≠ w0`, named `hvw`, then replace one side of `h` by the other everywhere, then simplify the named facts with the definitions; a false one closes the goal.
         have hvw : v ≠ w0 := by intro h; subst h; simp at hv
         -- Read through `set` at a key it did not change: the old value.
         simp only [set_other _ _ _ _ hww, set_other _ _ _ _ hvw] at hw hv
         -- The state's `one_writer` fact from before the step gives exactly this.
         exact hs.one_writer w v hw hv he)
      -- Fourth way, for a step that carries three arguments: name the writer that moved, w0.
      | (rename_i w0 _ _
         -- First show `w ≠ w0`, named `hww`, then replace one side of `h` by the other everywhere, then simplify the named facts with the definitions; a false one closes the goal.
         have hww : w ≠ w0 := by intro h; subst h; simp at hw
         -- First show `v ≠ w0`, named `hvw`, then replace one side of `h` by the other everywhere, then simplify the named facts with the definitions; a false one closes the goal.
         have hvw : v ≠ w0 := by intro h; subst h; simp at hv
         -- Read through `set` at a key it did not change: the old value.
         simp only [set_other _ _ _ _ hww, set_other _ _ _ _ hvw] at hw hv
         -- The state's `one_writer` fact from before the step gives exactly this.
         exact hs.one_writer w v hw hv he)

/-- The three copier facts, together: they move through the same steps. -/
def CopierFacts (s : St) : Prop :=
  -- A copier's new extent copies its old one below the length it saw.
  (∀ w, (s.wpc w = .built ∨ s.wpc w = .frz) → s.wext w < s.next ∧
    s.wlen w ≤ (s.ext (s.wext w)).len ∧
    (∀ j, j < s.wlen w → s.newBuf w j = (s.ext (s.wext w)).buf j) ∧ s.newBuf w (s.wlen w) = w) ∧
  -- A copier that saw FROZEN: frozen at the length it saw.
  (∀ w, s.wpc w = .built → s.wsawFrz w = true →
    (s.ext (s.wext w)).frozen = true ∧ (s.ext (s.wext w)).len = s.wlen w) ∧
  -- A copier past its freeze: frozen at the length it saw.
  (∀ w, s.wpc w = .frz →
    (s.ext (s.wext w)).frozen = true ∧ (s.ext (s.wext w)).len = s.wlen w)

/-- A copier whose fields the step left alone keeps its facts: the extent
only grows, keeps its records below the length, and stays frozen. -/
theorem copier_kept {s t : St} (hs : Inv s) (hst : Step s t) (w : Nat)
    -- The step left this copier's step, extent, length, records and flags.
    (hpc : t.wpc w = s.wpc w) (hext : t.wext w = s.wext w) (hlen : t.wlen w = s.wlen w)
    -- Another premise of the claim, named so the proof can use it.
    (hbuf : t.newBuf w = s.newBuf w) (hz : t.wsawFrz w = s.wsawFrz w) :
    -- The claim, which continues on the lines below.
    ((t.wpc w = .built ∨ t.wpc w = .frz) → t.wext w < t.next ∧
      t.wlen w ≤ (t.ext (t.wext w)).len ∧
      (∀ j, j < t.wlen w → t.newBuf w j = (t.ext (t.wext w)).buf j) ∧ t.newBuf w (t.wlen w) = w) ∧
    (t.wpc w = .built → t.wsawFrz w = true →
      (t.ext (t.wext w)).frozen = true ∧ (t.ext (t.wext w)).len = t.wlen w) ∧
    (t.wpc w = .frz → (t.ext (t.wext w)).frozen = true ∧ (t.ext (t.wext w)).len = t.wlen w) := by
  -- Rewrite every field of `t` to the field of `s` it equals.
  rw [hpc, hext, hlen, hbuf, hz]
  -- Prove the parts one at a time; each `?_` becomes a goal of its own.
  refine ⟨?_, ?_, ?_⟩
  -- The copy facts: the extent grew and kept its records.
  ·
    -- Name what the goal starts with: `hw`.
    intro hw
    -- Unpack the facts on the right and name them.
    obtain ⟨hlt, hle, hcp, hlast⟩ := hs.copier w hw
    -- Unpack the facts on the right and name them.
    obtain ⟨hnext, hgrow, hkeep, _⟩ := step_grows hs hst (s.wext w) hlt
    -- Prove the parts one at a time; each `?_` becomes a goal of its own.
    refine ⟨by omega, by omega, ?_, hlast⟩
    -- Name the things the goal is about: `j`, `hj`.
    intro j hj
    -- Rewrite the goal with the equalities given.
    rw [hcp j hj, hkeep j (by omega)]
  -- Frozen when seen: still frozen, at the same length.
  ·
    -- Name what the goal starts with: `hw`, `hzz`.
    intro hw hzz
    -- Unpack the facts on the right and name them.
    obtain ⟨hfz, hl⟩ := hs.saw_frozen w hw hzz
    -- Unpack the facts on the right and name them.
    obtain ⟨hlt, _, _, _⟩ := hs.copier w (Or.inl hw)
    -- Unpack the facts on the right and name them.
    obtain ⟨_, _, _, hstay⟩ := step_grows hs hst (s.wext w) hlt
    -- Unpack the facts on the right and name them.
    obtain ⟨hfz', hl'⟩ := hstay hfz
    -- Put the pieces together.
    exact ⟨hfz', by rw [hl', hl]⟩
  -- Frozen after the freeze: still frozen, at the same length.
  ·
    -- Name what the goal starts with: `hw`.
    intro hw
    -- Unpack the facts on the right and name them.
    obtain ⟨hfz, hl⟩ := hs.frozen_at w hw
    -- Unpack the facts on the right and name them.
    obtain ⟨hlt, _, _, _⟩ := hs.copier w (Or.inr hw)
    -- Unpack the facts on the right and name them.
    obtain ⟨_, _, _, hstay⟩ := step_grows hs hst (s.wext w) hlt
    -- Unpack the facts on the right and name them.
    obtain ⟨hfz', hl'⟩ := hstay hfz
    -- Put the pieces together.
    exact ⟨hfz', by rw [hl', hl]⟩

/-- The copier facts hold after every step. -/
theorem copier_step {s t : St} (hs : Inv s) (hst : Step s t) : CopierFacts t := by
  -- Keep the step for `copier_kept`.
  have hst' := hst
  -- Split the three facts into one statement per copier.
  suffices h : ∀ w,
      ((t.wpc w = .built ∨ t.wpc w = .frz) → t.wext w < t.next ∧
        t.wlen w ≤ (t.ext (t.wext w)).len ∧
        (∀ j, j < t.wlen w → t.newBuf w j = (t.ext (t.wext w)).buf j) ∧
        t.newBuf w (t.wlen w) = w) ∧
      (t.wpc w = .built → t.wsawFrz w = true →
        (t.ext (t.wext w)).frozen = true ∧ (t.ext (t.wext w)).len = t.wlen w) ∧
      (t.wpc w = .frz → (t.ext (t.wext w)).frozen = true ∧
        (t.ext (t.wext w)).len = t.wlen w) by
    -- This is exactly what the goal asks.
    exact ⟨fun w => (h w).1, fun w => (h w).2.1, fun w => (h w).2.2⟩
  -- Name the things the goal is about: `w`.
  intro w
  -- One case per kind of step.
  cases hst with
  -- Case: a writer claims the current extent and writes its record after the published ones.
  | claimWrite w0 hpc hb hf =>
    -- Split on whether `w = w0` holds.
    by_cases hww : w = w0
    -- `w0` is an in-place writer now: none of the copier facts apply.
    ·
      -- Replace one side of `hww` by the other everywhere.
      subst hww
      -- Simplify with the definitions until the goal is closed or plain.
      simp
    -- Another copier: its fields are untouched.
    ·
      -- This is exactly what the goal asks.
      exact copier_kept hs hst' w (set_other _ _ _ _ hww) (set_other _ _ _ _ hww)
        -- The rest of the arguments: each condition of the step, checked.
        (set_other _ _ _ _ hww) rfl rfl
  -- Case: an in-place writer publishes its record.
  | publishOk w0 hpc hf =>
    -- Split on whether `w = w0` holds.
    by_cases hww : w = w0
    -- `w0` is done: none of the copier facts apply.
    ·
      -- Replace one side of `hww` by the other everywhere.
      subst hww
      -- Simplify with the definitions until the goal is closed or plain.
      simp
    -- Another copier: its fields are untouched.
    ·
      -- This is exactly what the goal asks.
      exact copier_kept hs hst' w (set_other _ _ _ _ hww) rfl rfl rfl rfl
  -- Case: an in-place writer finds its extent frozen and starts over.
  | publishFail w0 hpc hf =>
    -- Split on whether `w = w0` holds.
    by_cases hww : w = w0
    -- `w0` starts over: none of the copier facts apply.
    ·
      -- Replace one side of `hww` by the other everywhere.
      subst hww
      -- Simplify with the definitions until the goal is closed or plain.
      simp
    -- Another copier: its fields are untouched.
    ·
      -- This is exactly what the goal asks.
      exact copier_kept hs hst' w (set_other _ _ _ _ hww) rfl rfl rfl rfl
  -- Case: a writer builds a copy of the current extent with its record added.
  | build w0 hpc =>
    -- Split on whether `w = w0` holds.
    by_cases hww : w = w0
    -- `w0` just built from the current extent.
    ·
      -- Replace one side of `hww` by the other everywhere.
      subst hww
      -- Prove the parts one at a time; each `?_` becomes a goal of its own.
      refine ⟨?_, ?_, ?_⟩
      -- Its copy: the published records below the length, then its own.
      ·
        -- Take the assumption the goal starts with.
        intro _
        -- Read the value `set` just wrote at this key.
        simp only [set_same]
        -- Prove the parts one at a time; each `?_` becomes a goal of its own.
        refine ⟨hs.cur_lt, Nat.le_refl _, ?_, by simp⟩
        -- Below the length, the new extent holds the old one's records.
        intro j hj
        -- Simplify with the definitions until the goal is closed or plain.
        simp [hj]
      -- It saw FROZEN: the current extent is frozen, at the length seen.
      ·
        -- Name what the goal starts with: `hz`.
        intro _ hz
        -- Read the value `set` just wrote at this key.
        simp only [set_same] at hz ⊢
        -- Put the pieces together.
        exact ⟨hz, trivial⟩
      -- It is built, not past its freeze.
      ·
        -- Name what the goal starts with: `h`.
        intro h
        -- Simplify the named facts with the definitions; a false one closes the goal.
        simp at h
    -- Another copier: its fields are untouched.
    ·
      -- This is exactly what the goal asks.
      exact copier_kept hs hst' w (set_other _ _ _ _ hww) (set_other _ _ _ _ hww)
        -- The rest of the arguments: each condition of the step, checked.
        (set_other _ _ _ _ hww) (set_other _ _ _ _ hww) (set_other _ _ _ _ hww)
  -- Case: a copier that saw its extent frozen already goes straight on to the swap.
  | freezeSkip w0 hpc hz =>
    -- Split on whether `w = w0` holds.
    by_cases hww : w = w0
    -- `w0` moves from built to frozen: it saw FROZEN, so the extent is.
    ·
      -- frozen at the length it saw.
      subst hww
      -- Unpack the facts on the right and name them.
      obtain ⟨hlt, hle, hcp, hlast⟩ := hs.copier w (Or.inl hpc)
      -- Unpack the facts on the right and name them.
      obtain ⟨hfz, hl⟩ := hs.saw_frozen w hpc hz
      -- Prove the parts one at a time; each `?_` becomes a goal of its own.
      refine ⟨?_, ?_, ?_⟩
      -- The copy facts carry over unchanged.
      ·
        -- Take the assumption the goal starts with.
        intro _
        -- Put the pieces together.
        exact ⟨hlt, hle, hcp, hlast⟩
      -- It is past its freeze, not built.
      ·
        -- Name what the goal starts with: `h`.
        intro h
        -- Simplify the named facts with the definitions; a false one closes the goal.
        simp at h
      -- Frozen, at the length it saw.
      ·
        -- Take the assumption the goal starts with.
        intro _
        -- Put the pieces together.
        exact ⟨hfz, hl⟩
    -- Another copier: its fields are untouched.
    ·
      -- This is exactly what the goal asks.
      exact copier_kept hs hst' w (set_other _ _ _ _ hww) rfl rfl rfl rfl
  -- Case: a copier freezes its extent.
  | freezeOk w0 hpc hz hl hb hf =>
    -- Split on whether `w = w0` holds.
    by_cases hww : w = w0
    -- `w0` froze its extent at the length it saw.
    ·
      -- Replace one side of `hww` by the other everywhere.
      subst hww
      -- Unpack the facts on the right and name them.
      obtain ⟨hlt, hle, hcp, hlast⟩ := hs.copier w (Or.inl hpc)
      -- Prove the parts one at a time; each `?_` becomes a goal of its own.
      refine ⟨?_, ?_, ?_⟩
      -- The copy facts carry over: the freeze changed only the flag.
      ·
        -- Take the assumption the goal starts with.
        intro _
        -- Prove the parts one at a time; each `?_` becomes a goal of its own.
        refine ⟨hlt, ?_, ?_, hlast⟩
        -- The length is unchanged.
        ·
          -- Read the value `set` just wrote at this key, then this is exactly what the goal asks.
          simp only [set_same]; exact hle
        -- The records are unchanged.
        ·
          -- Name what the goal starts with: `j`, `hj`, then read the value `set` just wrote at this
          -- key, then this is exactly what the goal asks.
          intro j hj; simp only [set_same]; exact hcp j hj
      -- It is past its freeze, not built.
      ·
        -- Name what the goal starts with: `h`.
        intro h
        -- Simplify the named facts with the definitions; a false one closes the goal.
        simp at h
      -- Frozen now, at the length it saw.
      ·
        -- Take the assumption the goal starts with.
        intro _
        -- Read the value `set` just wrote at this key.
        simp only [set_same]
        -- Put the pieces together.
        exact ⟨trivial, hl⟩
    -- Another copier: its fields are untouched.
    ·
      -- This is exactly what the goal asks.
      exact copier_kept hs hst' w (set_other _ _ _ _ hww) rfl rfl rfl rfl
  -- Case: a copier finds its extent's state word changed and starts over.
  | freezeFail w0 hpc hz hch =>
    -- Split on whether `w = w0` holds.
    by_cases hww : w = w0
    -- `w0` starts over: none of the copier facts apply.
    ·
      -- Replace one side of `hww` by the other everywhere.
      subst hww
      -- Simplify with the definitions until the goal is closed or plain.
      simp
    -- Another copier: its fields are untouched.
    ·
      -- This is exactly what the goal asks.
      exact copier_kept hs hst' w (set_other _ _ _ _ hww) rfl rfl rfl rfl
  -- Case: a copier swaps its new extent in.
  | swapOk w0 hpc hc =>
    -- Split on whether `w = w0` holds.
    by_cases hww : w = w0
    -- `w0` is done: none of the copier facts apply.
    ·
      -- Replace one side of `hww` by the other everywhere.
      subst hww
      -- Simplify with the definitions until the goal is closed or plain.
      simp
    -- Another copier: its fields are untouched.
    ·
      -- This is exactly what the goal asks.
      exact copier_kept hs hst' w (set_other _ _ _ _ hww) rfl rfl rfl rfl
  -- Case: a copier finds the pointer moved and starts over.
  | swapFail w0 hpc hc =>
    -- Split on whether `w = w0` holds.
    by_cases hww : w = w0
    -- `w0` starts over: none of the copier facts apply.
    ·
      -- Replace one side of `hww` by the other everywhere.
      subst hww
      -- Simplify with the definitions until the goal is closed or plain.
      simp
    -- Another copier: its fields are untouched.
    ·
      -- This is exactly what the goal asks.
      exact copier_kept hs hst' w (set_other _ _ _ _ hww) rfl rfl rfl rfl
  -- Case: a memory reader begins, noting the appends that have returned.
  | begin r hpc =>
    -- Readers do not touch copiers.
    exact copier_kept hs hst' w rfl rfl rfl rfl rfl
  -- Case: a memory reader loads the pointer.
  | load r hpc =>
    -- Readers do not touch copiers.
    exact copier_kept hs hst' w rfl rfl rfl rfl rfl
  -- Case: a memory reader reads its extent's published length.
  | lenRead r hpc =>
    -- Readers do not touch copiers.
    exact copier_kept hs hst' w rfl rfl rfl rfl rfl
  -- Case: a memory reader copies the published records.
  | copy r hpc =>
    -- Readers do not touch copiers.
    exact copier_kept hs hst' w rfl rfl rfl rfl rfl

/-- Every finished append stays published in the current extent. -/
theorem holds_step {s t : St} (hs : Inv s) (hst : Step s t) :
    -- The claim itself; its proof follows.
    ∀ x ∈ t.finished, t.inPub t.cur x := by
  -- Keep the step for `inPub_step`.
  have hst' := hst
  -- One case per kind of step.
  cases hst with
  -- Case: an in-place writer publishes its record.
  | publishOk w0 hpc hf =>
    -- The writer's extent is not frozen, so it is the current one.
    obtain ⟨hlt, _, hlen, hbuf⟩ := hs.writer w0 hpc
    -- First show this small fact; its proof follows on the next lines.
    have hcur : s.wext w0 = s.cur := by
      -- Split on whether `s.wext w0 = s.cur` holds.
      by_cases h : s.wext w0 = s.cur
      -- This is exactly what the goal asks.
      · exact h
      -- Rewrite `hf` with the equalities given, then split on the ways `hf` can hold: none is left.
      · rw [hs.old_frozen _ hlt h] at hf; cases hf
    -- Name the things the goal is about: `x`, `hx`.
    intro x hx
    -- A member of `x :: l` is `x` itself or a member of `l`.
    simp only [List.mem_cons] at hx
    -- Split the `or` into its cases.
    rcases hx with rfl | hx
    -- The new record: at position `wlen`, now below the length.
    ·
      -- Give the shape of the answer; each `?_` becomes a goal of its own.
      refine ⟨s.wlen x, ?_, ?_⟩
      -- Simplify with the definitions until the goal is closed or plain, then the arithmetic over natural numbers that is left holds.
      · simp only [← hcur, set_same]; omega
      -- Simplify with the definitions until the goal is closed or plain, then this is exactly what the goal asks.
      · simp only [← hcur, set_same]; exact hbuf
    -- An older record: still published.
    ·
      -- This is exactly what the goal asks.
      exact inPub_step hs hst' hs.cur_lt (hs.holds x hx)
  -- Case: a copier swaps its new extent in.
  | swapOk w0 hpc hc =>
    -- The old current extent is the copier's, frozen at the length it saw.
    obtain ⟨hlt, _, hcp, hlast⟩ := hs.copier w0 (Or.inr hpc)
    -- Unpack the facts on the right and name them.
    obtain ⟨_, hl⟩ := hs.frozen_at w0 hpc
    -- Name the things the goal is about: `x`, `hx`.
    intro x hx
    -- A member of `x :: l` is `x` itself or a member of `l`.
    simp only [List.mem_cons] at hx
    -- Split the `or` into its cases.
    rcases hx with rfl | hx
    -- The copier's own record: at position `wlen` of the new extent.
    ·
      -- This is exactly what the goal asks.
      exact ⟨s.wlen x, by simp, by simp [hlast]⟩
    -- An older record: below the old length, copied to the new extent.
    ·
      -- Unpack the fact on the right and name its pieces.
      obtain ⟨i, hi, hix⟩ := hs.holds x hx
      -- Rewrite `hi` with the equalities given.
      rw [hc, hl] at hi
      -- Prove the parts one at a time; each `?_` becomes a goal of its own.
      refine ⟨i, by simp; omega, ?_⟩
      -- Read the value `set` just wrote at this key.
      simp only [set_same]
      -- Rewrite the goal with the equalities given.
      rw [hcp i hi, ← hc]
      -- This is exactly what the goal asks.
      exact hix
  -- Case: every other kind of step.
  | _ =>
    -- Every other step keeps the pointer and the finished list, and only
    -- grows the current extent.
    intro x hx
    -- This is exactly what the goal asks.
    exact inPub_step hs hst' hs.cur_lt (hs.holds x hx)

/-- A reader that began knows only finished appends, after any step. -/
theorem r_begun_step {s t : St} (hs : Inv s) (hst : Step s t) :
    -- The claim itself; its proof follows.
    ∀ r, t.rpc r = .begun → ∀ x ∈ t.rfin r, x ∈ t.finished := by
  -- One case per kind of step.
  cases hst with
  -- Case: a memory reader begins, noting the appends that have returned.
  | begin r0 hpc =>
    -- Name what the goal starts with: `r`, `hr`, `x`, `hx`.
    intro r hr x hx
    -- Is `r` the reader that just began?
    by_cases hrr : r = r0
    -- Yes: it noted exactly the finished appends.
    ·
      -- Replace one side of `hrr` by the other everywhere, then simplify with the definitions until
      -- the fact given is the goal.
      subst hrr; simpa using hx
    -- No: untouched.
    ·
      -- Read through `set` at a key it did not change in `hr hx`: the old value.
      simp only [set_other _ _ _ _ hrr] at hr hx
      -- The state's `r_begun` fact from before the step gives exactly this.
      exact hs.r_begun r hr x hx
  -- Case: a memory reader loads the pointer.
  | load r0 hpc =>
    -- Name what the goal starts with: `r`, `hr`, `x`, `hx`.
    intro r hr x hx
    -- `r0` loaded, so `r` is another reader, untouched.
    have hrr : r ≠ r0 := by intro h; subst h; simp at hr
    -- Read through `set` at a key it did not change: the old value.
    simp only [set_other _ _ _ _ hrr] at hr
    -- The state's `r_begun` fact from before the step gives exactly this.
    exact hs.r_begun r hr x hx
  -- Case: a memory reader reads its extent's published length.
  | lenRead r0 hpc =>
    -- Name what the goal starts with: `r`, `hr`, `x`, `hx`.
    intro r hr x hx
    -- `r0` was loaded, not begun: `r` is another reader.
    have hrr : r ≠ r0 := by intro h; subst h; simp at hr
    -- Read through `set` at a key it did not change: the old value.
    simp only [set_other _ _ _ _ hrr] at hr
    -- The state's `r_begun` fact from before the step gives exactly this.
    exact hs.r_begun r hr x hx
  -- Case: a memory reader copies the published records.
  | copy r0 hpc =>
    -- Name what the goal starts with: `r`, `hr`, `x`, `hx`.
    intro r hr x hx
    -- `r0` was past its length, not begun: `r` is another reader.
    have hrr : r ≠ r0 := by intro h; subst h; simp at hr
    -- Read through `set` at a key it did not change: the old value.
    simp only [set_other _ _ _ _ hrr] at hr
    -- The state's `r_begun` fact from before the step gives exactly this.
    exact hs.r_begun r hr x hx
  -- Case: an in-place writer publishes its record.
  | publishOk w0 hpc hf =>
    -- One more finished append: the old ones are still there.
    intro r hr x hx
    -- This is exactly what the goal asks.
    exact List.mem_cons_of_mem _ (hs.r_begun r hr x hx)
  -- Case: a copier swaps its new extent in.
  | swapOk w0 hpc hc =>
    -- One more finished append: the old ones are still there.
    intro r hr x hx
    -- This is exactly what the goal asks.
    exact List.mem_cons_of_mem _ (hs.r_begun r hr x hx)
  -- Case: every other kind of step.
  | _ =>
    -- Writer steps that finish nothing touch no reader.
    exact hs.r_begun

/-- A loaded reader's extent keeps every record it must see, after any
step. -/
theorem r_loaded_step {s t : St} (hs : Inv s) (hst : Step s t) :
    -- The claim itself; its proof follows.
    ∀ r, t.rpc r = .loaded → t.rext r < t.next ∧ ∀ x ∈ t.rfin r, t.inPub (t.rext r) x := by
  -- Keep the step for `inPub_step` and `next_grows`.
  have hst' := hst
  -- For a reader the step left alone: its extent still holds what it held.
  have kept : ∀ r, s.rpc r = .loaded → t.rext r = s.rext r → t.rfin r = s.rfin r →
      t.rext r < t.next ∧ ∀ x ∈ t.rfin r, t.inPub (t.rext r) x := by
    -- Name what the goal starts with: `r`, `hr`, `he`, `hf`.
    intro r hr he hf
    -- Unpack the facts on the right and name them.
    obtain ⟨hlt, hpub⟩ := hs.r_loaded r hr
    -- Rewrite the goal with the equalities given.
    rw [he, hf]
    -- Put the pieces together.
    exact ⟨Nat.lt_of_lt_of_le hlt (next_grows hs hst'),
      fun x hx => inPub_step hs hst' hlt (hpub x hx)⟩
  -- One case per kind of step.
  cases hst with
  -- Case: a memory reader begins, noting the appends that have returned.
  | begin r0 hpc =>
    -- Name what the goal starts with: `r`, `hr`.
    intro r hr
    -- `r0` began, so `r` is another reader.
    have hrr : r ≠ r0 := by intro h; subst h; simp at hr
    -- Read through `set` at a key it did not change: the old value.
    simp only [set_other _ _ _ _ hrr] at hr
    -- This is exactly what the goal asks.
    exact kept r hr rfl (set_other _ _ _ _ hrr)
  -- Case: a memory reader loads the pointer.
  | load r0 hpc =>
    -- Name what the goal starts with: `r`, `hr`.
    intro r hr
    -- Split on whether `r = r0` holds.
    by_cases hrr : r = r0
    -- `r0` loaded the current extent: every finished append is there.
    ·
      -- Replace one side of `hrr` by the other everywhere.
      subst hrr
      -- Read the value `set` just wrote at this key.
      simp only [set_same]
      -- Prove the parts one at a time; each `?_` becomes a goal of its own.
      refine ⟨hs.cur_lt, ?_⟩
      -- Name the things the goal is about: `x`, `hx`.
      intro x hx
      -- The state's `holds` fact from before the step gives exactly this.
      exact hs.holds x (hs.r_begun r hpc x hx)
    -- Another reader: untouched.
    ·
      -- Read through `set` at a key it did not change in `hr`: the old value.
      simp only [set_other _ _ _ _ hrr] at hr
      -- This is exactly what the goal asks.
      exact kept r hr (set_other _ _ _ _ hrr) rfl
  -- Case: a memory reader reads its extent's published length.
  | lenRead r0 hpc =>
    -- Name what the goal starts with: `r`, `hr`.
    intro r hr
    -- `r0` is past its length now, so `r` is another reader.
    have hrr : r ≠ r0 := by intro h; subst h; simp at hr
    -- Read through `set` at a key it did not change: the old value.
    simp only [set_other _ _ _ _ hrr] at hr
    -- This is exactly what the goal asks.
    exact kept r hr rfl rfl
  -- Case: a memory reader copies the published records.
  | copy r0 hpc =>
    -- Name what the goal starts with: `r`, `hr`.
    intro r hr
    -- `r0` is done, so `r` is another reader.
    have hrr : r ≠ r0 := by intro h; subst h; simp at hr
    -- Read through `set` at a key it did not change: the old value.
    simp only [set_other _ _ _ _ hrr] at hr
    -- This is exactly what the goal asks.
    exact kept r hr rfl rfl
  -- Case: every other kind of step.
  | _ =>
    -- Writer steps touch no reader's fields.
    intro r hr
    -- This is exactly what the goal asks.
    exact kept r hr rfl rfl

/-- A reader past its length keeps every record it must see, below that
length, after any step. -/
theorem r_len_step {s t : St} (hs : Inv s) (hst : Step s t) :
    -- The claim, which continues on the lines below.
    ∀ r, (t.rpc r = .lenRead ∨ t.rpc r = .done) → t.rext r < t.next ∧
      t.rlen r ≤ (t.ext (t.rext r)).len ∧
      ∀ x ∈ t.rfin r, ∃ i, i < t.rlen r ∧ (t.ext (t.rext r)).buf i = x := by
  -- Keep the step for `step_grows`.
  have hst' := hst
  -- For a reader the step left alone: below its length nothing changed.
  have kept : ∀ r, (s.rpc r = .lenRead ∨ s.rpc r = .done) → t.rext r = s.rext r →
      t.rfin r = s.rfin r → t.rlen r = s.rlen r →
      t.rext r < t.next ∧ t.rlen r ≤ (t.ext (t.rext r)).len ∧
      ∀ x ∈ t.rfin r, ∃ i, i < t.rlen r ∧ (t.ext (t.rext r)).buf i = x := by
    -- Name what the goal starts with: `r`, `hr`, `he`, `hf`, `hl`.
    intro r hr he hf hl
    -- Unpack the facts on the right and name them.
    obtain ⟨hlt, hle, hsee⟩ := hs.r_len r hr
    -- Unpack the facts on the right and name them.
    obtain ⟨hnext, hgrow, hkeep, _⟩ := step_grows hs hst' (s.rext r) hlt
    -- Rewrite the goal with the equalities given.
    rw [he, hf, hl]
    -- Prove the parts one at a time; each `?_` becomes a goal of its own.
    refine ⟨by omega, by omega, ?_⟩
    -- Name the things the goal is about: `x`, `hx`.
    intro x hx
    -- Unpack the facts on the right and name them.
    obtain ⟨i, hi, hix⟩ := hsee x hx
    -- Put the pieces together.
    exact ⟨i, hi, by rw [hkeep i (by omega)]; exact hix⟩
  -- One case per kind of step.
  cases hst with
  -- Case: a memory reader begins, noting the appends that have returned.
  | begin r0 hpc =>
    -- Name what the goal starts with: `r`, `hr`.
    intro r hr
    -- `r0` began, so `r` is another reader.
    have hrr : r ≠ r0 := by intro h; subst h; simp at hr
    -- Read through `set` at a key it did not change: the old value.
    simp only [set_other _ _ _ _ hrr] at hr
    -- This is exactly what the goal asks.
    exact kept r hr rfl (set_other _ _ _ _ hrr) rfl
  -- Case: a memory reader loads the pointer.
  | load r0 hpc =>
    -- Name what the goal starts with: `r`, `hr`.
    intro r hr
    -- `r0` loaded, so `r` is another reader.
    have hrr : r ≠ r0 := by intro h; subst h; simp at hr
    -- Read through `set` at a key it did not change: the old value.
    simp only [set_other _ _ _ _ hrr] at hr
    -- This is exactly what the goal asks.
    exact kept r hr (set_other _ _ _ _ hrr) rfl rfl
  -- Case: a memory reader reads its extent's published length.
  | lenRead r0 hpc =>
    -- Name what the goal starts with: `r`, `hr`.
    intro r hr
    -- Split on whether `r = r0` holds.
    by_cases hrr : r = r0
    -- `r0` loaded its extent's length: what it must see is below it.
    ·
      -- Replace one side of `hrr` by the other everywhere.
      subst hrr
      -- Unpack the facts on the right and name them.
      obtain ⟨hlt, hpub⟩ := hs.r_loaded r hpc
      -- Read the value `set` just wrote at this key.
      simp only [set_same]
      -- Put the pieces together.
      exact ⟨hlt, Nat.le_refl _, hpub⟩
    -- Another reader: untouched.
    ·
      -- Read through `set` at a key it did not change in `hr`: the old value.
      simp only [set_other _ _ _ _ hrr] at hr
      -- This is exactly what the goal asks.
      exact kept r hr rfl rfl (set_other _ _ _ _ hrr)
  -- Case: a memory reader copies the published records.
  | copy r0 hpc =>
    -- Name what the goal starts with: `r`, `hr`.
    intro r hr
    -- Split on whether `r = r0` holds.
    by_cases hrr : r = r0
    -- `r0` copied: its fields are as they were at its length.
    ·
      -- Replace one side of `hrr` by the other everywhere.
      subst hrr
      -- This is exactly what the goal asks.
      exact kept r (Or.inl hpc) rfl rfl rfl
    -- Another reader: untouched.
    ·
      -- Read through `set` at a key it did not change in `hr`: the old value.
      simp only [set_other _ _ _ _ hrr] at hr
      -- This is exactly what the goal asks.
      exact kept r hr rfl rfl rfl
  -- Case: every other kind of step.
  | _ =>
    -- Writer steps touch no reader's fields.
    intro r hr
    -- This is exactly what the goal asks.
    exact kept r hr rfl rfl rfl

/-- Every step keeps every fact. -/
theorem inv_step {s t : St} (hs : Inv s) (hst : Step s t) : Inv t := by
  -- The copier facts come together.
  obtain ⟨hcp, hsaw, hfrz⟩ := copier_step hs hst
  -- Each field from its own lemma.
  exact ⟨cur_lt_step hs hst, old_frozen_step hs hst, writer_step hs hst,
    one_writer_step hs hst, hcp, hsaw, hfrz, holds_step hs hst, r_begun_step hs hst,
    r_loaded_step hs hst, r_len_step hs hst⟩

/-- Every reachable state keeps every fact. -/
theorem inv_reach {s : St} (h : Reach s) : Inv s := by
  -- By how `s` was reached: the start, or one step from a reachable state.
  induction h with
  -- The start satisfies the invariant.
  | init => exact inv_init
  -- One more step keeps it.
  | step _ hst ih => exact inv_step ih hst

/-- **`current_holds_finished`.** Every append that returned is published in
the extent readers load now. Rules out the story's append that published on
an extent a copy had already replaced. -/
theorem current_holds_finished {s : St} (h : Reach s) : ∀ x ∈ s.finished, s.inPub s.cur x :=
  -- One of the facts every reachable state keeps.
  (inv_reach h).holds

/-- **`read_sees_finished`.** A read that is done returns, below the length
it loaded, every append that had finished when it began. -/
theorem read_sees_finished {s : St} (h : Reach s) (r : Nat) (hr : s.rpc r = .done) :
    -- The claim itself; its proof follows.
    ∀ x ∈ s.rfin r, ∃ i, i < s.rlen r ∧ (s.ext (s.rext r)).buf i = x :=
  -- One of the facts every reachable state keeps, for a reader that is done.
  ((inv_reach h).r_len r (Or.inr hr)).2.2

/-! ## Part 3. The RED case: a copy that swaps without freezing -/

/-- The steps with the freeze left out (bug `NoFreeze`): a copier swaps its
new extent in straight after building, and the old extent is never frozen,
so an append in place can still publish there. -/
inductive NoFreezeStep : St → St → Prop
  /-- The same steps as the real code ... -/
  | real {s t : St} : Step s t → NoFreezeStep s t
  /-- ... plus a swap straight after the build, with no freeze. -/
  | swapNoFreeze (s : St) (w : Nat) (hpc : s.wpc w = .built) (hc : s.cur = s.wext w) :
      -- The state after this step: only the fields named here change.
      NoFreezeStep s { s with
        ext := set s.ext s.next ⟨s.newBuf w, s.wlen w + 1, false, false⟩,
        cur := s.next, next := s.next + 1,
        finished := w :: s.finished, wpc := set s.wpc w .done }

/-- The states the bug can reach from the start. -/
inductive NoFreezeReach : St → Prop
  /-- The start is reachable. -/
  | init : NoFreezeReach St.init
  /-- One step from a reachable state reaches another. -/
  | step {s t : St} : NoFreezeReach s → NoFreezeStep s t → NoFreezeReach t

/-- **`no_freeze_loses_append`.** Without the freeze, a finished append can
be missing from the current extent. The story: writer 1 takes WRITER on
extent 0 and writes its record at position 0; writer 2 builds a copy of
extent 0 (length 0, so just its own record) and swaps it in as extent 1;
writer 1 then publishes on extent 0, which nobody reads any more. Both
appends finished, and extent 1 holds only writer 2's record. -/
theorem no_freeze_loses_append :
    -- The claim itself; its proof follows.
    ∃ s, NoFreezeReach s ∧ 1 ∈ s.finished ∧ ¬ s.inPub s.cur 1 := by
  -- Writer 1 claims extent 0 and writes its record.
  have r1 := NoFreezeReach.step NoFreezeReach.init
    -- The rest of the arguments: each condition of the step, checked.
    (NoFreezeStep.real (Step.claimWrite St.init 1 rfl rfl rfl))
  -- Writer 2 builds its copy from extent 0, still at length 0.
  have r2 := NoFreezeReach.step r1 (NoFreezeStep.real (Step.build _ 2 (by simp [set, St.init])))
  -- Writer 2 swaps its copy in without freezing extent 0.
  have r3 := NoFreezeReach.step r2
    -- The rest of the arguments: each condition of the step, checked.
    (NoFreezeStep.swapNoFreeze _ 2 (by simp [set]) (by simp [set, St.init]))
  -- Writer 1 publishes on extent 0, which is not frozen.
  have r4 := NoFreezeReach.step r3
    -- The rest of the arguments: each condition of the step, checked.
    (NoFreezeStep.real (Step.publishOk _ 1 (by simp [set]) (by simp [set, St.init, Ext.empty])))
  -- That final state is the witness.
  refine ⟨_, r4, by simp, ?_⟩
  -- The current extent is 1, of length 1, holding writer 2's record, not 1:
  -- unfolding the steps leaves a contradiction.
  intro ⟨i, hi, hix⟩
  -- Simplify the named facts with the definitions; a false one closes the goal.
  simp [set, St.init, Ext.empty] at hi hix

-- The end of this file's names.
end Regolith.EnvFiles
