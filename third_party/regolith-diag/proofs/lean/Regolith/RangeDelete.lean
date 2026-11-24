/-!
# RangeDelete: a range delete copied into the first layer deletes every older key below it (E14)

This file backs the TLA+ model `proofs/tla/LegacyRangeRead.tla`, invariant
`ReadNewest` (configurations `MC_LegacyRangeRead_Green` and
`MC_LegacyRangeRead_Red_NoHoist`). The model checks one put, one range delete
and a few layers; this file proves the law for any number of layers, holding
anything, in any order.

## The story, with a tiny example

A read of one key walks the layers of the database from the newest to the
oldest: the memtable, then the tables. It remembers the newest range delete
over the key it has passed, and it stops at the first layer that holds the
key: if the range delete it remembers is newer than that key, the key is
deleted. So a range delete only works on an older key if the walk passes the
delete no later than the key.

Release 0.1.x could leave a table whose range delete is newer than a key in a
layer above it. Example: key `b` was put at sequence 1, in the second layer.
The third layer holds a range delete over `b` at sequence 2. The walk finds
`b` in the second layer, has passed no range delete, and returns `b`. But `b`
was deleted at 2.

The fix (`recovery::hoist_range_deletes` in `src/engine/recovery.rs`): the open
copies into the memtable it recovers, the first layer of every walk, the
range deletes of the tables it moves out of overlapping levels (E14), and,
once per database, every range delete that has a layer read before it
holding a key it may cover (`buried_range_deletes`, E37). Then the walk has
passed the delete before it meets any key. A range delete the check leaves
where it is has no layer before it holding the key, so the walk passes it
before any key of that layer or a later one too; this file proves the case
where every delete is copied, and `proofs/tla/LegacyRangeRead.tla` checks the
check's own choice for one key (`MC_LegacyRangeRead_Green_Buried`).

## What is proved, in plain words

1. `walk_under_delete`: once the walk remembers a range delete at least as
   new as every one still ahead of it, the rest of the walk returns the first
   key it meets if that key is newer than the remembered delete, and nothing
   otherwise. The layers ahead may hold anything, in any order.
2. `any_delete_is_the_copy`: when the first layer holds a range delete at
   least as new as each of the others', the newest range delete anywhere is
   the first layer's.
3. `hoisted_delete_reads_by_sequence`: so with the copy in the first layer, a
   read returns what the sequences say: the first key met if it is newer than
   every range delete in the database, and nothing otherwise.
4. `delete_behind_put_reads_deleted_key`: the example above, computed. Without
   the copy the read returns the deleted key; with it, nothing. This is the
   RED configuration `MC_LegacyRangeRead_Red_NoHoist` as a counterexample.

Which key is "the first met" is not this file's subject: `LsmOrder.lean`
proves it is the newest one (`read_newest`), and that moving the levels keeps
it so (`demote_reads_newest`). This file adds the range deletes.
-/

-- Everything below lives in this namespace, so its names do not clash.
namespace Regolith.RangeDelete

/-- One thing a layer holds about the one key this file follows. -/
inductive Entry where
  /-- The key was written at this sequence. -/
  | put (seq : Nat)
  /-- A range delete covering the key, at this sequence. -/
  | del (seq : Nat)

/-- A layer, a memtable or a table, as what it holds about the key. -/
abbrev Layer := List Entry

/-- The newest put of a layer that a snapshot at `snap` may see, or 0 when
it holds none (sequences start at 1). -/
def newestPut (snap : Nat) : Layer → Nat
  -- An empty layer holds no put.
  | [] => 0
  -- A put the snapshot may see counts against the rest; one it may not see is skipped.
  | .put s :: rest => if s ≤ snap then max s (newestPut snap rest) else newestPut snap rest
  -- A range delete is not a put.
  | .del _ :: rest => newestPut snap rest

/-- The newest range delete of a layer that a snapshot at `snap` may see, or
0 when it holds none. -/
def newestDel (snap : Nat) : Layer → Nat
  -- An empty layer holds no range delete.
  | [] => 0
  -- A put is not a range delete.
  | .put _ :: rest => newestDel snap rest
  -- A range delete the snapshot may see counts against the rest; one it may not see is skipped.
  | .del s :: rest => if s ≤ snap then max s (newestDel snap rest) else newestDel snap rest

/-- The key the walk meets first: the newest put of the first layer holding
one the snapshot may see, or 0 when no layer does. -/
def firstPut (snap : Nat) : List Layer → Nat
  -- No layer: no key.
  | [] => 0
  -- A layer without the key is passed; the first one with it answers.
  | l :: rest => if newestPut snap l = 0 then firstPut snap rest else newestPut snap l

/-- The newest range delete the snapshot may see in any of the layers. -/
def anyDel (snap : Nat) : List Layer → Nat
  -- No layer: no range delete.
  | [] => 0
  -- The newer of this layer's and the rest's.
  | l :: rest => max (newestDel snap l) (anyDel snap rest)

/-- The walk of `ReadView::walk_newest_first` (`src/engine/source_walk.rs`)
with a point read's visit. `d` is the newest range delete passed so far. -/
def walk (snap : Nat) : Nat → List Layer → Nat
  -- No layer left: the key is absent.
  | _, [] => 0
  -- A layer's own range deletes count before its keys are looked at.
  | d, l :: rest =>
    -- The layer does not hold the key: go on, remembering the newest range delete so far.
    if newestPut snap l = 0 then walk snap (max d (newestDel snap l)) rest
    -- It does: the key answers if it is newer than that range delete, else it is deleted.
    else if max d (newestDel snap l) < newestPut snap l then newestPut snap l else 0

/-- A point read at `snap`: the walk, having passed no range delete yet. -/
def read (snap : Nat) (layers : List Layer) : Nat :=
  -- Start the walk with nothing remembered.
  walk snap 0 layers

/-- What the sequences say: the key the walk meets first, when it is newer
than every range delete in the database; otherwise nothing. Where the range
deletes sit plays no part. -/
def truth (snap : Nat) (layers : List Layer) : Nat :=
  -- The key, when it is newer than the newest range delete anywhere.
  if anyDel snap layers < firstPut snap layers then firstPut snap layers else 0

/-- **`walk_under_delete`.** If the walk already remembers a range delete at
least as new as every one in the layers ahead, those layers' range deletes
change nothing: the walk returns the first key it meets when that key is
newer than the remembered delete, and nothing otherwise. -/
theorem walk_under_delete (snap : Nat) (layers : List Layer) :
    -- For every remembered delete `d` at least as new as each layer's own,
    ∀ d, (∀ l, l ∈ layers → newestDel snap l ≤ d) →
      -- the walk answers by comparing `d` with the first key ahead.
      walk snap d layers = if d < firstPut snap layers then firstPut snap layers else 0 := by
  -- Go through the layers one at a time.
  induction layers with
  -- No layer: the walk returns nothing, and no key is ahead.
  | nil =>
    -- Take the remembered delete.
    intro d _
    -- Both sides compute to nothing.
    simp [walk, firstPut]
  -- A layer `l`, then the rest, for which the claim holds (`ih`).
  | cons l rest ih =>
    -- Take the remembered delete and the bound on every layer's own.
    intro d h
    -- This layer's range delete is no newer than the remembered one,
    have hl : newestDel snap l ≤ d := h l (List.mem_cons.mpr (Or.inl rfl))
    -- so passing this layer leaves the remembered delete as it was.
    have hmax : max d (newestDel snap l) = d := Nat.max_eq_left hl
    -- The bound still holds for the layers after this one.
    have hrest : ∀ l', l' ∈ rest → newestDel snap l' ≤ d :=
      -- A layer of the rest is a layer of the whole list.
      fun l' hl' => h l' (List.mem_cons.mpr (Or.inr hl'))
    -- Does this layer hold the key?
    by_cases hp : newestPut snap l = 0
    -- It does not: the walk and the first key ahead both move on to the rest,
    · simp only [walk, firstPut, hmax, hp, ↓reduceIte]
      -- where the claim holds already.
      exact ih d hrest
    -- It does: the walk stops here, and this layer's key is the first key ahead.
    · simp only [walk, firstPut, hmax, hp, ↓reduceIte]

/-- When every layer's range delete is at most `d`, so is the newest of them
all. -/
theorem anyDel_le (snap : Nat) (layers : List Layer) (d : Nat)
    -- Each layer's own range delete is at most `d`.
    (h : ∀ l, l ∈ layers → newestDel snap l ≤ d) :
    -- The statement: the newest range delete in any layer is at most `d`.
    anyDel snap layers ≤ d := by
  -- Go through the layers one at a time.
  induction layers with
  -- No layer: the newest range delete is 0.
  | nil => exact Nat.zero_le d
  -- A layer `l`, then the rest, for which the claim holds (`ih`).
  | cons l rest ih =>
    -- The newest of all is the newer of this layer's and the rest's; bound each.
    refine Nat.max_le.mpr ⟨h l (List.mem_cons.mpr (Or.inl rfl)), ?_⟩
    -- The rest's is bounded because each of its layers is.
    exact ih (fun l' hl' => h l' (List.mem_cons.mpr (Or.inr hl')))

/-- **`any_delete_is_the_copy`.** When the first layer holds a range delete
at least as new as each of the others' (the open copied every one of them
into it), the newest range delete anywhere is the first layer's. -/
theorem any_delete_is_the_copy (snap : Nat) (m : Layer) (rest : List Layer)
    -- The first layer's range delete is at least as new as each other layer's.
    (hcopy : ∀ l, l ∈ rest → newestDel snap l ≤ newestDel snap m) :
    -- The statement: the newest range delete in the database is the first layer's.
    anyDel snap (m :: rest) = newestDel snap m :=
  -- It is the newer of the first layer's and the rest's, and the rest's is no newer.
  Nat.max_eq_left (anyDel_le snap rest (newestDel snap m) hcopy)

/-- **`hoisted_delete_reads_by_sequence`.** With the range deletes of the
layers below copied into the first layer, a read returns what the sequences
say, whatever the layers below hold and however they are ordered. -/
theorem hoisted_delete_reads_by_sequence (snap : Nat) (m : Layer) (rest : List Layer)
    -- The first layer's range delete is at least as new as each other layer's.
    (hcopy : ∀ l, l ∈ rest → newestDel snap l ≤ newestDel snap m) :
    -- The statement: the read returns what the sequences say.
    read snap (m :: rest) = truth snap (m :: rest) := by
  -- The truth compares the first key with the first layer's range delete.
  rw [truth, any_delete_is_the_copy snap m rest hcopy]
  -- Does the first layer itself hold the key?
  by_cases hp : newestPut snap m = 0
  -- It does not: the walk goes on remembering the first layer's range delete,
  · simp only [read, walk, firstPut, hp, ↓reduceIte, Nat.zero_max]
    -- and from there `walk_under_delete` gives the answer.
    exact walk_under_delete snap rest (newestDel snap m) hcopy
  -- It does: the walk stops there, comparing its key with its own range delete.
  · simp only [read, walk, firstPut, hp, ↓reduceIte, Nat.zero_max]

/-- **`delete_behind_put_reads_deleted_key`.** The memtable is empty, the
second layer holds the key at sequence 1, the third a range delete at 2.
The read at snapshot 2 returns the key, though the sequences say it is
deleted. With the range delete copied into the first layer, the read
returns nothing. -/
theorem delete_behind_put_reads_deleted_key :
    -- The statement: without the copy the read returns the key put at 1,
    read 2 [[], [.put 1], [.del 2]] = 1 ∧
    -- which the sequences say is deleted;
    truth 2 [[], [.put 1], [.del 2]] = 0 ∧
    -- with the copy, the read returns nothing.
    read 2 [[.del 2], [.put 1], [.del 2]] = 0 := by
  -- All three are small computations: compute them.
  decide

-- The namespace ends here.
end Regolith.RangeDelete
