/-!
# LogTruncate: dropping a prefix of a commit-ordered log keeps the log's law

This file backs `proofs/tla/LogTruncate.tla`, invariants `INV_LiveLaw`,
`INV_HeadMonotone`, `INV_ReaderStable` and `INV_NoConflict` (configurations
`MC_LogTruncate_Green*`), and the REDs `MC_LogTruncate_Red_NoHeadGuard`,
`MC_LogTruncate_Red_ErasesOnTruncate` and `MC_LogTruncate_Red_OffByOne`.

## The story, for a reader who has never seen the code

A log is a numbered list of entries that only grows: `Transaction::append`
(src/transaction/append.rs) gives each new entry the next position after
the log's head. `Transaction::truncate_log` (src/transaction/truncate.rs)
drops a range of positions `lo..=hi` at commit, and never touches the head.
The range must not reach past the head the transaction read.

Each position's entry key is stored as a short list of versions, newest
first: a commit that writes the key puts a new version on the front, a
`put` for an append and a delete for a truncate. A reader pinned at a
snapshot number `k` reads the newest version whose commit number is at most
`k`. That is how every key in regolith is read, and a truncate is nothing
more than a delete.

Tiny example. Two appends of one entry each: positions 1 and 2, head 2, two
commits. A truncate of `1..=1` is the third commit: position 1 now has two
versions, the delete (commit 3) on top of the put (commit 1). A reader pinned
at commit 2 reads the put, and a reader at commit 3 reads the delete.

What could go wrong, and what the theorems here show cannot happen, for any
number of commits and any sizes:

1. `live_is_head_less_truncated`: after any run of appends and truncates, a
   position has a live entry exactly when it is between 1 and the head and
   no truncate ever covered it.
2. `snapshot_reads_as_of` and `step_keeps_reads`: a reader pinned at a
   commit number reads the same thing for ever, however many appends and
   truncates commit after it: the log as of its snapshot.
3. `no_version_past_head`: no entry key past the head has any version, so an
   append only ever writes keys no truncate (whose range stays within the
   head) can name.
4. `truncate_never_conflicts`: the check of a truncate's delete against the
   versions committed after its snapshot never finds a put there, which is
   why it is elided and never a conflict.
5. `head_monotone` and `truncate_keeps_head`: the head never moves back, so
   positions are never given out twice.
6. `truncate_commutes` and `truncate_idempotent`: two truncates leave the
   same entries in either order, and truncating a range twice is truncating
   it once. This is why two overlapping truncates may both commit.
7. `unguarded_truncate_breaks_law` and `erasing_loses_snapshot`: two REDs as
   concrete counterexamples. A truncate with no head guard lets a later
   append resurrect a truncated position; one that erases versions instead
   of adding a delete takes the entry from a reader that pinned before it.

How to read the Lean: `def` defines a function or a property, `theorem`
states a fact and its proof follows `:= by`. Lines starting with `--` are
comments. Positions and commit numbers are plain natural numbers.
-/

-- Everything below is named Regolith.LogTruncate.<name>.
namespace Regolith.LogTruncate

/-! ## The objects -/

/-- The versions of one entry key, newest first. Each is a pair: the number of
the commit that wrote it, and `true` for a put (an append) or `false` for a
delete (a truncate). -/
abbrev Versions := List (Nat × Bool)

/-- The database as far as one log is concerned. -/
structure Db where
  /-- How many commits have happened: the newest commit number. -/
  seq : Nat
  /-- The head key's value: the newest position given out, 0 while empty. -/
  head : Nat
  /-- For each position, the versions of its entry key. -/
  vers : Nat → Versions
  /-- Ghost: every range `(lo, hi)` some truncate covered. Only the claims
  below read it; the code never stores it. -/
  cuts : List (Nat × Nat)

/-- The database before any commit: nothing written. -/
def init : Db := ⟨0, 0, fun _ => [], []⟩

/-- What a reader pinned at commit number `s` finds in one key: the newest
version at or below `s`, and whether it is a put. No such version: no entry.
`List.find?` returns the first element that passes, which is the newest. -/
def readVers (vs : Versions) (s : Nat) : Bool :=
  -- Look for the first version that is not newer than the snapshot.
  match vs.find? (fun v => decide (v.1 ≤ s)) with
  -- Found one: it is an entry exactly when it is a put.
  | some v => v.2
  -- Nothing at or below the snapshot: no entry.
  | none => false

/-- A read of position `p`'s entry key at commit number `s`. -/
def readAt (db : Db) (p s : Nat) : Bool := readVers (db.vers p) s

/-- The commit of `Transaction::append` with `n` entries: the entries take
positions `head + 1` to `head + n`, and the head moves to `head + n`. Each
new position's key gains a put version numbered with the new commit. -/
def appendDb (db : Db) (n : Nat) : Db :=
  -- The record of the database after the commit.
  { -- One more commit.
    seq := db.seq + 1
    -- The head moves up by the number of entries.
    head := db.head + n
    -- A position in the new range gains a put on top; the others are as they were.
    vers := fun p =>
      if db.head < p ∧ p ≤ db.head + n then (db.seq + 1, true) :: db.vers p else db.vers p
    -- No truncate happened.
    cuts := db.cuts }

/-- The commit of `Transaction::truncate_log(lo..=hi)`: each position in the
range gains a delete version on top. The head stays. -/
def truncDb (db : Db) (lo hi : Nat) : Db :=
  -- The record of the database after the commit.
  { -- One more commit.
    seq := db.seq + 1
    -- The head key is not written.
    head := db.head
    -- A position in the range gains a delete on top; the others are as they were.
    vers := fun p =>
      if lo ≤ p ∧ p ≤ hi then (db.seq + 1, false) :: db.vers p else db.vers p
    -- The ghost remembers the range.
    cuts := (lo, hi) :: db.cuts }

/-- One commit. A truncate is allowed only when its range stays within the
head: that is the refusal `truncate_log` makes with `Error::InvalidArgument`. -/
inductive Step : Db → Db → Prop where
  /-- An append of `n` entries. -/
  | append (db : Db) (n : Nat) : Step db (appendDb db n)
  /-- A truncate of `lo..=hi`, with `hi` at or below the head. -/
  | truncate (db : Db) (lo hi : Nat) (h : hi ≤ db.head) : Step db (truncDb db lo hi)

/-- The databases reachable from the empty one by commits. -/
inductive Reach : Db → Prop where
  /-- The empty database is reachable. -/
  | init : Reach init
  /-- A commit of a reachable database is reachable. -/
  | step {a b : Db} : Reach a → Step a b → Reach b

/-- `Steps a b`: `b` is `a` after zero or more commits. -/
inductive Steps : Db → Db → Prop where
  /-- Zero commits. -/
  | refl (a : Db) : Steps a a
  /-- One more commit after some. -/
  | step {a b c : Db} : Steps a b → Step b c → Steps a c

/-- A position is covered by one of the ranges in `cuts`. -/
def Dropped (cuts : List (Nat × Nat)) (p : Nat) : Prop :=
  -- Some range in the list contains it.
  ∃ c ∈ cuts, c.1 ≤ p ∧ p ≤ c.2

/-- The entries the law says are live: the positions from 1 to the head that
no truncate covered. -/
def Live (db : Db) (p : Nat) : Prop := 1 ≤ p ∧ p ≤ db.head ∧ ¬ Dropped db.cuts p

/-! ## Reading versions -/

/-- A version at or below the snapshot is what the read finds. -/
theorem readVers_take {v : Nat × Bool} {vs : Versions} {s : Nat}
    -- The version is not newer than the snapshot.
    (h : v.1 ≤ s) :
    -- The statement: the read finds it, and returns its kind.
    readVers (v :: vs) s = v.2 := by
  -- Unfold the read and look at the first version, which passes the test.
  simp [readVers, h]

/-- A version newer than the snapshot is passed over. -/
theorem readVers_skip {v : Nat × Bool} {vs : Versions} {s : Nat}
    -- The version is newer than the snapshot.
    (h : s < v.1) :
    -- The statement: the read is what it would be without that version.
    readVers (v :: vs) s = readVers vs s := by
  -- The first version fails the test, so the search goes on with the rest.
  simp [readVers, Nat.not_le.mpr h]

/-- If every version is at or below `s₀`, a read at any later snapshot finds
what a read at `s₀` finds. -/
theorem readVers_stable {vs : Versions} {s₀ s : Nat}
    -- Every version is at or below `s₀`.
    (hall : ∀ v ∈ vs, v.1 ≤ s₀)
    -- The later snapshot is not older.
    (hle : s₀ ≤ s) :
    -- The statement: the two reads agree.
    readVers vs s = readVers vs s₀ := by
  -- Look at the first version, if there is one.
  cases vs with
  -- No version: both reads find nothing.
  | nil => simp [readVers]
  -- A first version.
  | cons v rest =>
    -- It is at or below `s₀`,
    have h0 : v.1 ≤ s₀ := hall v (List.mem_cons_self ..)
    -- so both reads find it.
    rw [readVers_take h0, readVers_take (Nat.le_trans h0 hle)]

/-- A delete version at or below the snapshot hides everything under it. -/
theorem readVers_delete {c s : Nat} {vs : Versions}
    -- The delete is not newer than the snapshot.
    (h : c ≤ s) :
    -- The statement: the read finds no entry.
    readVers ((c, false) :: vs) s = false := by
  -- The delete is the first version and passes the test.
  rw [readVers_take (v := (c, false)) h]

/-! ## The invariant -/

/-- What stays true of every reachable database. -/
structure Inv (db : Db) : Prop where
  /-- Every truncated range ends at or below the head. -/
  cuts_le : ∀ c ∈ db.cuts, c.2 ≤ db.head
  /-- No version is newer than the newest commit. -/
  seq_le : ∀ p, ∀ v ∈ db.vers p, v.1 ≤ db.seq
  /-- No key past the head has any version. -/
  past_head : ∀ p, db.head < p → db.vers p = []
  /-- A read at the newest commit finds exactly the live entries. -/
  reads : ∀ p, readAt db p db.seq = true ↔ Live db p

/-- A position outside a range is dropped by the longer list exactly when it
was by the shorter one. -/
theorem dropped_cons_of_outside {c : Nat × Nat} {cs : List (Nat × Nat)} {p : Nat}
    -- The position is outside the new range.
    (h : ¬ (c.1 ≤ p ∧ p ≤ c.2)) :
    -- The statement: the new range adds nothing for it.
    Dropped (c :: cs) p ↔ Dropped cs p := by
  -- Prove both directions.
  constructor
  -- Dropped by the longer list implies dropped by the shorter.
  ·
    -- Take the range that covers it.
    rintro ⟨d, hd, hp⟩
    -- That range is the new one, or one of the old.
    rcases List.mem_cons.mp hd with rfl | hd'
    -- The new one is excluded by the hypothesis.
    · exact absurd hp h
    -- An old one: the shorter list has it too.
    · exact ⟨d, hd', hp⟩
  -- Dropped by the shorter list implies dropped by the longer.
  ·
    -- Take the range that covers it.
    rintro ⟨d, hd, hp⟩
    -- The longer list has it too.
    exact ⟨d, List.mem_cons_of_mem _ hd, hp⟩

/-- The empty database satisfies the invariant. -/
theorem inv_init : Inv init where
  -- No range was truncated.
  cuts_le := by simp [init]
  -- No version exists.
  seq_le := by simp [init]
  -- No key has a version.
  past_head := by simp [init]
  -- A read finds nothing, and nothing is live: the head is 0.
  reads := by
    -- For any position:
    intro p
    -- the read finds nothing, and position p is not between 1 and the head 0.
    simp [readAt, readVers, init, Live]
    -- The remaining case: a position at least 1 and equal to 0, which cannot be.
    intro h1 h2
    -- Arithmetic.
    omega

/-- **An append keeps the invariant.** The new entries are put on keys past
the head, which no truncate range reaches. -/
theorem inv_append {db : Db}
    -- The invariant holds before.
    (h : Inv db) (n : Nat) :
    -- The statement: it holds after the append.
    Inv (appendDb db n) where
  -- Every range still ends at or below the head, which only grew.
  cuts_le := by
    -- Take a range of the new database; its ranges are the old ones.
    intro c hc
    -- The old bound, and the head grew: arithmetic.
    have := h.cuts_le c hc
    -- The new head is at least the old one.
    simp only [appendDb] at hc ⊢
    -- Arithmetic.
    omega
  -- No version is newer than the new commit.
  seq_le := by
    -- Take a position and one of its new versions.
    intro p v hv
    -- Unfold the append.
    simp only [appendDb] at hv ⊢
    -- Split on whether the position is in the new range.
    split at hv
    -- In the range: the version is the new one, or an old one.
    · rcases List.mem_cons.mp hv with rfl | hv'
      -- The new one carries the new commit number.
      · exact Nat.le_refl _
      -- An old one is at most the old commit number, one less.
      · exact Nat.le_succ_of_le (h.seq_le p v hv')
    -- Outside the range: an old version, at most the old commit number.
    · exact Nat.le_succ_of_le (h.seq_le p v hv)
  -- No key past the new head has a version.
  past_head := by
    -- Take a position past the new head.
    intro p hp
    -- It is past the old head too, and outside the new range.
    have hold := h.past_head p (by simp only [appendDb] at hp; omega)
    -- Unfold the append.
    simp only [appendDb] at hp ⊢
    -- The position is not in the new range, so the key is as it was.
    simp only [show ¬ (db.head < p ∧ p ≤ db.head + n) from by omega, ite_false]
    -- And the old key had no version.
    exact hold
  -- A read at the new commit finds exactly the live entries.
  reads := by
    -- Take a position.
    intro p
    -- Split on whether the append wrote it.
    by_cases ht : db.head < p ∧ p ≤ db.head + n
    -- The append wrote it.
    ·
      -- The key's versions are the old ones with a put on top.
      have hv : (appendDb db n).vers p = (db.seq + 1, true) :: db.vers p := by
        -- Unfold the append and take the first branch.
        simp [appendDb, ht.1, ht.2]
      -- The read finds the put.
      have hr : readAt (appendDb db n) p (appendDb db n).seq = true := by
        -- Unfold the read, and use the version list.
        unfold readAt; rw [hv]
        -- The put is at the new commit number, so it is found.
        exact readVers_take (v := (db.seq + 1, true)) (Nat.le_refl _)
      -- So the read is true, and the position must be live.
      rw [hr]
      -- Both sides are true: show the position is live.
      constructor
      -- The right side follows the left.
      · intro _
        -- Between 1 and the new head, and covered by no range.
        refine ⟨by omega, by simp only [appendDb]; omega, ?_⟩
        -- A range covering it would reach past the old head, which it cannot.
        rintro ⟨c, hc, h1, h2⟩
        -- Every old range ends at or below the old head.
        have := h.cuts_le c hc
        -- The position is past the old head: contradiction.
        simp only [appendDb] at hc; omega
      -- The left side follows the right.
      · intro _
        -- It is `true = true`.
        rfl
    -- The append did not write it.
    ·
      -- The key is as it was.
      have hv : (appendDb db n).vers p = db.vers p := by
        -- Unfold the append and take the second branch.
        simp [appendDb, ht]
      -- A read at the new commit finds what a read at the old one found.
      have hr : readAt (appendDb db n) p (appendDb db n).seq = readAt db p db.seq := by
        -- Unfold, use the version list, and the stability of reads.
        unfold readAt; rw [hv]
        -- Every version is at or below the old commit number.
        exact readVers_stable (h.seq_le p) (Nat.le_succ _)
      -- So the read is what it was.
      rw [hr, h.reads p]
      -- The position is live after exactly when it was live before.
      unfold Live
      -- The ranges are the same, and the head moved only past the position.
      simp only [appendDb]
      -- Prove both directions.
      constructor
      -- Live before implies live after.
      · rintro ⟨a, b, c⟩
        -- The bound only gets weaker.
        exact ⟨a, by omega, c⟩
      -- Live after implies live before.
      · rintro ⟨a, b, c⟩
        -- The position is not in the new range, so it is at or below the old head.
        exact ⟨a, by omega, c⟩

/-- **A truncate within the head keeps the invariant.** -/
theorem inv_truncate {db : Db}
    -- The invariant holds before.
    (h : Inv db) {lo hi : Nat}
    -- The range stays within the head.
    (hh : hi ≤ db.head) :
    -- The statement: it holds after the truncate.
    Inv (truncDb db lo hi) where
  -- Every range, the new one included, ends at or below the head.
  cuts_le := by
    -- Take a range of the new list.
    intro c hc
    -- It is the new one, or an old one.
    simp only [truncDb, List.mem_cons] at hc ⊢
    -- Split.
    rcases hc with rfl | hc
    -- The new one ends at `hi`, within the head.
    · exact hh
    -- An old one ends within the head.
    · exact h.cuts_le c hc
  -- No version is newer than the new commit.
  seq_le := by
    -- Take a position and one of its versions.
    intro p v hv
    -- Unfold the truncate.
    simp only [truncDb] at hv ⊢
    -- Split on whether the position is in the range.
    split at hv
    -- In the range: the version is the new delete, or an old one.
    · rcases List.mem_cons.mp hv with rfl | hv'
      -- The new one carries the new commit number.
      · exact Nat.le_refl _
      -- An old one is at most the old commit number.
      · exact Nat.le_succ_of_le (h.seq_le p v hv')
    -- Outside: an old version.
    · exact Nat.le_succ_of_le (h.seq_le p v hv)
  -- No key past the head has a version.
  past_head := by
    -- Take a position past the head.
    intro p hp
    -- Unfold the truncate.
    simp only [truncDb] at hp ⊢
    -- It is past `hi` too, so outside the range, and the key is as it was.
    simp only [show ¬ (lo ≤ p ∧ p ≤ hi) from by omega, ite_false]
    -- And the old key had no version.
    exact h.past_head p hp
  -- A read at the new commit finds exactly the live entries.
  reads := by
    -- Take a position.
    intro p
    -- Split on whether the truncate covers it.
    by_cases ht : lo ≤ p ∧ p ≤ hi
    -- The truncate covers it.
    ·
      -- The key's versions are the old ones with a delete on top.
      have hv : (truncDb db lo hi).vers p = (db.seq + 1, false) :: db.vers p := by
        -- Unfold and take the first branch.
        simp [truncDb, ht.1, ht.2]
      -- The read finds the delete.
      have hr : readAt (truncDb db lo hi) p (truncDb db lo hi).seq = false := by
        -- Unfold the read, and use the version list.
        unfold readAt; rw [hv]
        -- The delete is at the new commit number.
        exact readVers_delete (Nat.le_refl _)
      -- So the read is false, and the position is not live.
      rw [hr]
      -- `false = true` is false; the position is dropped by the new range.
      constructor
      -- A false read implies anything.
      · intro hf
        -- It is not true.
        exact absurd hf (by decide)
      -- Live after is impossible.
      · rintro ⟨_, _, hn⟩
        -- The new range covers the position.
        exact absurd ⟨(lo, hi), by simp [truncDb], ht.1, ht.2⟩ hn
    -- The truncate does not cover it.
    ·
      -- The key is as it was.
      have hv : (truncDb db lo hi).vers p = db.vers p := by
        -- Unfold and take the second branch.
        simp [truncDb, ht]
      -- A read at the new commit finds what a read at the old one found.
      have hr : readAt (truncDb db lo hi) p (truncDb db lo hi).seq = readAt db p db.seq := by
        -- Unfold, use the version list, and the stability of reads.
        unfold readAt; rw [hv]
        -- Every version is at or below the old commit number.
        exact readVers_stable (h.seq_le p) (Nat.le_succ _)
      -- So the read is what it was.
      rw [hr, h.reads p]
      -- The position is live after exactly when it was live before.
      unfold Live
      -- The head is the same; the new range adds nothing for this position.
      simp only [truncDb]
      -- Outside the new range, the lists drop the position alike.
      rw [dropped_cons_of_outside (c := (lo, hi)) ht]

/-- Every reachable database satisfies the invariant. -/
theorem inv_reach {db : Db}
    -- It is reachable.
    (h : Reach db) :
    -- The statement: the invariant holds.
    Inv db := by
  -- By induction on the commits that led to it.
  induction h with
  -- The empty database.
  | init => exact inv_init
  -- One more commit after a reachable database that satisfies it.
  | step _ hs ih =>
    -- The commit is an append or a truncate.
    cases hs with
    -- An append.
    | append n => exact inv_append ih n
    -- A truncate within the head.
    | truncate lo hi hh => exact inv_truncate ih hh

/-! ## The law -/

/-- **The log's law.** After any run of appends and truncates, the entry of a
position is there exactly when the position is from 1 to the head and no
truncate covered it. -/
theorem live_is_head_less_truncated {db : Db}
    -- The database is reachable.
    (h : Reach db) (p : Nat) :
    -- The statement: a read at the newest commit finds the entry exactly for the positions
    -- from 1 to the head that no range of `cuts` covers.
    readAt db p db.seq = true ↔ (1 ≤ p ∧ p ≤ db.head ∧ ¬ ∃ c ∈ db.cuts, c.1 ≤ p ∧ p ≤ c.2) :=
  -- This is the invariant's last field, with `Live` and `Dropped` unfolded.
  (inv_reach h).reads p

/-- The head never moves back. -/
theorem head_monotone {a b : Db}
    -- One commit.
    (h : Step a b) :
    -- The statement: the head did not decrease.
    a.head ≤ b.head := by
  -- An append moves it up; a truncate leaves it.
  cases h with
  -- An append of `n`.
  | append n => simp [appendDb]
  -- A truncate.
  | truncate lo hi _ => simp [truncDb]

/-- A truncate leaves the head key alone, so positions keep counting from
where they were. -/
theorem truncate_keeps_head (db : Db) (lo hi : Nat) :
    -- The statement: the head is the same after.
    (truncDb db lo hi).head = db.head := rfl

/-- No entry key past the head has a version, in any reachable database. -/
theorem no_version_past_head {db : Db}
    -- The database is reachable.
    (h : Reach db) {p : Nat}
    -- The position is past the head.
    (hp : db.head < p) :
    -- The statement: its key has never been written.
    db.vers p = [] :=
  -- This is the invariant's third field.
  (inv_reach h).past_head p hp

/-- After a commit, a read pinned at an older commit number finds the same
thing as before: a new version is newer than the snapshot. -/
theorem step_keeps_reads {a b : Db}
    -- One commit.
    (h : Step a b) (p k : Nat)
    -- The snapshot is not newer than the database before.
    (hk : k ≤ a.seq) :
    -- The statement: the read is unchanged.
    readAt b p k = readAt a p k := by
  -- Unfold the read.
  unfold readAt
  -- An append or a truncate.
  cases h with
  -- An append.
  | append n =>
    -- Unfold the new version list.
    simp only [appendDb]
    -- The key gained a version or not.
    split
    -- It gained one, numbered after the snapshot: the read passes over it.
    · exact readVers_skip (by simp only; omega)
    -- It did not.
    · rfl
  -- A truncate.
  | truncate lo hi _ =>
    -- Unfold the new version list.
    simp only [truncDb]
    -- The key gained a version or not.
    split
    -- It gained one, numbered after the snapshot: the read passes over it.
    · exact readVers_skip (by simp only; omega)
    -- It did not.
    · rfl

/-- One commit adds one to the commit number. -/
theorem step_seq {a b : Db}
    -- One commit.
    (h : Step a b) :
    -- The statement: the new number is one more.
    b.seq = a.seq + 1 := by
  -- Both kinds of commit say so in their definition.
  cases h <;> rfl

/-- Commits only move the commit number up. -/
theorem steps_seq {a b : Db}
    -- Some commits.
    (h : Steps a b) :
    -- The statement: the number did not decrease.
    a.seq ≤ b.seq := by
  -- By induction on the commits.
  induction h with
  -- None: equal.
  | refl => exact Nat.le_refl _
  -- One more: it adds one.
  | step _ hs ih => exact Nat.le_trans ih (by rw [step_seq hs]; exact Nat.le_succ _)

/-- Commits only move the head up. -/
theorem steps_head {a b : Db}
    -- Some commits.
    (h : Steps a b) :
    -- The statement: the head did not decrease.
    a.head ≤ b.head := by
  -- By induction on the commits.
  induction h with
  -- None: equal.
  | refl => exact Nat.le_refl _
  -- One more: it does not decrease the head.
  | step _ hs ih => exact Nat.le_trans ih (head_monotone hs)

/-- **A pinned reader sees the log as of its snapshot, for ever.** A reader
pinned at the newest commit of a reachable database reads, after any later
appends and truncates, exactly the entries that were live when it pinned. -/
theorem snapshot_reads_as_of {a b : Db}
    -- The database it pinned is reachable.
    (ha : Reach a)
    -- Later commits followed.
    (hs : Steps a b) (p : Nat) :
    -- The statement: a read at the pinned number finds the entry exactly for the live positions
    -- of the database it pinned.
    readAt b p a.seq = true ↔ Live a p := by
  -- The reads at the pinned number never change: by induction on the later commits.
  have hsame : readAt b p a.seq = readAt a p a.seq := by
    -- Induction on the commits.
    induction hs with
    -- None.
    | refl => rfl
    -- One more commit after some.
    | step hs' hstep ih =>
      -- The pinned number is not newer than the database before the commit.
      rw [step_keeps_reads hstep p a.seq (steps_seq hs'), ih]
  -- So it is the read at the newest commit of `a`, which is the live entries.
  rw [hsame]
  -- The invariant says so.
  exact (inv_reach ha).reads p

/-! ## Never a conflict -/

/-- The check a delete of key `p` faces at commit: the key's newest version is
a put committed after the snapshot `s`. Only that is a conflict; a newer
delete means the key already holds what the delete would store. -/
def PutAfter (db : Db) (s p : Nat) : Prop :=
  -- The newest version, if there is one,
  ∃ v rest, db.vers p = v :: rest ∧
    -- is newer than the snapshot and a put.
    s < v.1 ∧ v.2 = true

/-- Every version of a position at or below an earlier database's head that
was committed after it is a delete. -/
theorem later_versions_are_deletes {a b : Db}
    -- The earlier database is reachable.
    (ha : Reach a)
    -- Commits followed.
    (hs : Steps a b) {p : Nat}
    -- The position is at or below the earlier head.
    (hp : p ≤ a.head) :
    -- The statement: each version newer than the earlier database is a delete.
    ∀ v ∈ b.vers p, a.seq < v.1 → v.2 = false := by
  -- By induction on the commits.
  induction hs with
  -- None: no version is newer than the database itself.
  | refl =>
    -- Take a version newer than the database.
    intro v hv hn
    -- But none is.
    exact absurd ((inv_reach ha).seq_le p v hv) (Nat.not_le.mpr hn)
  -- One more commit after some.
  | step hs' hstep ih =>
    -- The commit is an append or a truncate.
    cases hstep with
    -- An append of `n` entries.
    | append n =>
      -- Take a version of the new list.
      intro v hv hn
      -- Unfold the append.
      simp only [appendDb] at hv
      -- The position is in the new range or not.
      split at hv
      -- In the new range: but the range is past the head, which is past the earlier head.
      · next ht =>
        -- The head only grew.
        have := steps_head hs'
        -- The position is past the earlier head: contradiction.
        omega
      -- Outside: an old version.
      · exact ih v hv hn
    -- A truncate.
    | truncate lo hi hh =>
      -- Take a version of the new list.
      intro v hv hn
      -- Unfold the truncate.
      simp only [truncDb] at hv
      -- The position is in the range or not.
      split at hv
      -- In the range: the new delete, or an old version.
      · rcases List.mem_cons.mp hv with rfl | hv'
        -- The new delete.
        · rfl
        -- An old version.
        · exact ih v hv' hn
      -- Outside: an old version.
      · exact ih v hv hn

/-- **A truncate never conflicts.** The delete of a position at or below the
head its transaction read is checked against the versions committed after the
snapshot, and none of them is a put on top. So the check finds nothing that
is not already a delete, and the write is elided. -/
theorem truncate_never_conflicts {a b : Db}
    -- The database at the transaction's snapshot is reachable.
    (ha : Reach a)
    -- Commits followed before this one.
    (hs : Steps a b) {p : Nat}
    -- The position is at or below the head the transaction read.
    (hp : p ≤ a.head) :
    -- The statement: no put newer than the snapshot sits on top of the key.
    ¬ PutAfter b a.seq p := by
  -- Suppose there were.
  rintro ⟨v, rest, hv, hn, hput⟩
  -- Every version newer than the snapshot is a delete.
  have := later_versions_are_deletes ha hs hp v (by rw [hv]; exact List.mem_cons_self ..) hn
  -- But this one is a put.
  rw [hput] at this
  -- `true = false`.
  exact Bool.noConfusion this

/-- An append writes only positions past the head it numbers from, and a
truncate only positions at or below the head it read, which is not newer: the
two never name the same key. -/
theorem truncate_append_disjoint {h₀ h₁ hi p : Nat}
    -- The truncate's range ends within the head it read.
    (hh : hi ≤ h₀)
    -- The head the append numbers from is not older.
    (hle : h₀ ≤ h₁)
    -- The position is one the append writes.
    (hp : h₁ < p) :
    -- The statement: the truncate does not cover it.
    ¬ p ≤ hi := by
  -- Arithmetic.
  omega

/-! ## Two truncates -/

/-- Two truncates, in either order, leave the same entries. -/
theorem truncate_commutes (db : Db) (a b c d : Nat) (p : Nat) :
    -- The statement: a read at the newest commit is the same either way.
    readAt (truncDb (truncDb db a b) c d) p (db.seq + 2) =
      readAt (truncDb (truncDb db c d) a b) p (db.seq + 2) := by
  -- Unfold the reads and the commits.
  simp only [readAt, truncDb]
  -- Split on whether each range covers the position.
  by_cases h1 : a ≤ p ∧ p ≤ b <;> by_cases h2 : c ≤ p ∧ p ≤ d <;> simp [h1, h2] <;>
    -- Whatever covers it, the newest version is a delete at or below the snapshot; with no
    -- cover, both sides are the old key.
    first
      -- A delete on top: the read finds it.
      | (rw [readVers_delete (by omega)]; rw [readVers_delete (by omega)])
      -- Or the same on one side.
      | (rw [readVers_delete (by omega)]; rw [readVers_skip (by simp only; omega)])
      -- Or the same on the other.
      | (rw [readVers_skip (by simp only; omega)]; rw [readVers_delete (by omega)])
      -- Or no cover at all.
      | rfl

/-- Truncating a range twice is truncating it once. -/
theorem truncate_idempotent (db : Db) (a b : Nat) (p : Nat)
    -- No version of the key is newer than the database: true of every reachable one.
    (hall : ∀ v ∈ db.vers p, v.1 ≤ db.seq) :
    -- The statement: a read at the newest commit is the same as after one truncate.
    readAt (truncDb (truncDb db a b) a b) p (db.seq + 2) =
      readAt (truncDb db a b) p (db.seq + 1) := by
  -- Unfold the reads and the commits.
  simp only [readAt, truncDb]
  -- Split on whether the range covers the position.
  by_cases h : a ≤ p ∧ p ≤ b
  -- It does: both reads find a delete.
  · simp only [h, and_self, ite_true]
    -- The twice-truncated key has a delete on top.
    rw [readVers_delete (by omega)]
    -- So does the once-truncated key.
    rw [readVers_delete (by omega)]
  -- It does not: both reads are of the old key at later numbers.
  · simp only [h, ite_false]
    -- No version is newer than the database, so a later number finds the same thing.
    rw [readVers_stable hall (by omega : db.seq ≤ db.seq + 1 + 1),
      readVers_stable hall (by omega : db.seq ≤ db.seq + 1)]

/-! ## Two REDs, as concrete counterexamples -/

/-- **RED, no head guard** (`MC_LogTruncate_Red_NoHeadGuard`). A truncate of
position 1 on an empty log, with no refusal for reaching past the head, then
an append: the appended entry is live, though the law says a truncated
position has no entry. -/
theorem unguarded_truncate_breaks_law :
    -- The statement: the entry is there, and position 1 was truncated.
    readAt (appendDb (truncDb init 1 1) 1) 1 2 = true ∧
      Dropped (appendDb (truncDb init 1 1) 1).cuts 1 := by
  -- Two parts.
  refine ⟨?_, ?_⟩
  -- The read finds the put: the append wrote position 1 over the delete.
  · decide
  -- The truncate's range covers position 1.
  · exact ⟨(1, 1), by simp [appendDb, truncDb], by decide, by decide⟩

/-- **RED, erase instead of delete** (`MC_LogTruncate_Red_ErasesOnTruncate`).
A truncate that erases the key's versions takes the entry from a reader that
pinned before it. Here the reader pinned at commit 1, after an append of one
entry. -/
theorem erasing_loses_snapshot :
    -- The statement: the reader finds the entry before the erase and not after.
    readAt (appendDb init 1) 1 1 = true ∧
      readAt { appendDb init 1 with vers := fun p =>
        if 1 ≤ p ∧ p ≤ 1 then [] else (appendDb init 1).vers p } 1 1 = false := by
  -- Two parts, both computed.
  exact ⟨by decide, by decide⟩

-- Close the namespace.
end Regolith.LogTruncate
