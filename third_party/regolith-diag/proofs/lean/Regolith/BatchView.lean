-- Sources, reads and the read-order theorems come from LsmOrder.lean.
import Regolith.LsmOrder

/-!
# BatchView: a batch read resolves every key against one view (E6)

This file backs the TLA+ model `proofs/tla/BatchRead.tla`, invariants
`BatchConsistent` and `BatchExact` (configurations `MC_BatchRead_Green`,
`MC_BatchRead_Red_ViewPerKey` and `MC_BatchRead_Red_ViewPerKey_Exact`).

## The story, for a reader who has never seen the code

A batch read (`multi_get`; `RegolithEngine::multi_get_in_view`,
src/engine/mod.rs) asks for several keys at once, and every answer must
describe the same moment. A "view" is one consistent list of where the data
is right now: this memtable, these files. Flushes keep replacing the
current view with a new one.

Tiny example of what can go wrong: the batch may see versions up to
sequence 1 and asks for keys 1 and 2; key 2 holds the version of sequence
1. After the batch answered key 1, key 2 is written again (sequence 3) and
a flush writes the memtable out keeping only that newest version. Looked up
in the new view, key 2 has only a version too new for the batch, and the
batch answers "absent". Looked up in the view the batch loaded at the
start, which still holds the old memtable, key 2 is found.

The defect (E6): a batch read that loads a fresh view for each key. The
batch registers no snapshot, so a flush (or a compaction) between two keys
may write out only the newest version of a key and drop the older one the
batch's snapshot needs. A key resolved against the new view then reads
absent or older. The engine resolves every key against the one view it
loaded at the start.

What is proved, in plain words:

1. `batch_one_view_point_in_time`: a batch resolved against the view it
   captured at its start answers every key exactly as it would have at the
   start, whatever writes landed in that view meanwhile.
2. `batch_one_view_newest` and `batch_one_view_absent`: each of those
   answers is the newest version of its key the snapshot may see in the
   captured view, or absent when there is none.
3. `view_per_key_breaks_batch`: a batch that loads a view per key, with a
   flush between two keys, answers a key absent although the captured view
   holds a version the snapshot may see. This is the RED configuration as
   a concrete counterexample.

The read order and `read_newest` come from `LsmOrder.lean`.
-/

-- Everything below is named Regolith.BatchView.<name>.
namespace Regolith.BatchView

-- Use LsmOrder's names (Source, read, Ordered, ...) without their prefix.
open Regolith.LsmOrder

/-- A view: the sources a read walks, in read order (the memtable, the L0
files newest first, the deeper levels), captured at one moment. The engine's
view holds the memtable it captured by reference, so a write made after the
capture lands in the view's first source. Such a write always takes a
sequence newer than any snapshot sampled before it. -/
abbrev View := List Source

/-- A batch read of `keys` at snapshot `snap`, resolved against the one view
`v`. Each key is returned with its answer. -/
def batchOneView (v : View) (keys : List Nat) (snap : Nat) : List (Nat × Option Nat) :=
  -- For each key of the batch: the key paired with its read through v at the snapshot.
  keys.map fun k => (k, read v k snap)

/-- A batch read that loads a view per key: key number `i` is resolved
against `views[i]`, whatever view was current when its turn came. -/
def batchPerKey (views : List View) (keys : List Nat) (snap : Nat) : List (Nat × Option Nat) :=
  -- Pair the views with the keys, in order: each key is read through the view at its own position.
  List.zipWith (fun v k => (k, read v k snap)) views keys

/-- Writes after the capture only add versions newer than the snapshot to
the view's memtable, so the view answers every read at the snapshot as it
did at the capture. -/
theorem read_grow_invisible {mem extra : Source} {rest : List Source} {k snap : Nat}
    -- Every later write is newer than the snapshot.
    (hnew : ∀ e ∈ extra, snap < e.2) :
    -- The statement: the read through the view with the later writes added to its memtable equals
    -- the read through the view as it was loaded.
    read ((extra ++ mem) :: rest) k snap = read (mem :: rest) k snap := by
  -- The memtable's answer is unchanged (`newestIn_append_invisible`), and
  -- the rest of the view is untouched.
  simp only [Regolith.LsmOrder.read, newestIn_append_invisible hnew]

/-- **E6, point in time.** A batch resolved against the view it captured,
`mem :: rest`, after writes `extra` landed in that view's memtable, answers
every key as the captured view did at the start. -/
theorem batch_one_view_point_in_time {mem extra : Source} {rest : List Source}
    -- (for any batch of keys and any snapshot)
    {keys : List Nat} {snap : Nat}
    -- Every write after the capture is newer than the snapshot.
    (hnew : ∀ e ∈ extra, snap < e.2) :
    -- The statement: the batch through the grown view answers exactly as the batch through the view
    -- as loaded.
    batchOneView ((extra ++ mem) :: rest) keys snap = batchOneView (mem :: rest) keys snap := by
  -- Unfold the batch: a read per key.
  unfold batchOneView
  -- Key by key, the read is unchanged.
  -- The two lists are built from the same keys, so it is enough that the per-key function is the
  -- same.
  congr 1
  -- Take any key k.
  funext k
  -- Its read is unchanged by the later writes.
  rw [read_grow_invisible hnew]

/-- **E6, every answer is the newest.** In the batch of
`batch_one_view_point_in_time`, every key answered `some v` is answered with
the newest version of that key the snapshot may see in the captured view,
provided the captured view is ordered. -/
theorem batch_one_view_newest {mem extra : Source} {rest : List Source}
    -- (for any batch of keys and any snapshot)
    {keys : List Nat} {snap : Nat}
    -- The captured view is ordered: each source newer than the ones after it.
    (hord : Ordered (mem :: rest))
    -- Every write after the capture is newer than the snapshot.
    (hnew : ∀ e ∈ extra, snap < e.2) :
    -- The statement: for every (key, answer) pair p of the batch and every sequence v, if the
    -- answer is `some v` then v is the newest version of that key the snapshot may see in the
    -- loaded view.
    ∀ p ∈ batchOneView ((extra ++ mem) :: rest) keys snap, ∀ v,
      p.2 = some v → IsNewest (mem :: rest) p.1 snap v := by
  -- The batch answers as the loaded view did.
  rw [batch_one_view_point_in_time hnew]
  -- Take a pair p of the batch whose answer is `some v`.
  intro p hp v hv
  -- `p` is `(k, read (mem :: rest) k snap)` for some key `k` of the batch.
  obtain ⟨k, -, rfl⟩ := List.mem_map.mp hp
  -- A read that answers v on an ordered view answers the newest visible version.
  exact read_some_newest hord hv

/-- **E6, every absent answer is right.** In the same batch, a key
answered absent has no version the snapshot may see in the captured view. -/
theorem batch_one_view_absent {mem extra : Source} {rest : List Source}
    -- (for any batch of keys and any snapshot)
    {keys : List Nat} {snap : Nat}
    -- Every write after the capture is newer than the snapshot.
    (hnew : ∀ e ∈ extra, snap < e.2) :
    -- The statement: for every (key, answer) pair p of the batch, if the answer is `none` then
    -- every version of that key the loaded view holds is above the snapshot.
    ∀ p ∈ batchOneView ((extra ++ mem) :: rest) keys snap,
      p.2 = none → ∀ s, Holds (mem :: rest) (p.1, s) → snap < s := by
  -- The batch answers as the loaded view did.
  rw [batch_one_view_point_in_time hnew]
  -- Take a pair p of the batch whose answer is `none`.
  intro p hp hnone
  -- p is (k, the read of k through the loaded view) for some key k of the batch.
  obtain ⟨k, -, rfl⟩ := List.mem_map.mp hp
  -- A read that answers nothing means every version of k the view holds is above the snapshot.
  exact read_eq_none.mp hnone

/-- `flushNewest mem` is what a flush writes when no registered snapshot
needs an older version: for each key, only its newest version in `mem`. A
version survives when no version of the same key in `mem` is newer. -/
def flushNewest (mem : Source) : Source :=
  -- Keep a version e of the memtable when every version e' in it has another key or a sequence at
  -- or below e's.
  mem.filter fun e => mem.all fun e' => e'.1 != e.1 || decide (e'.2 ≤ e.2)

/-- A flush of view `v`: a new, empty memtable, and the old memtable's
newest versions as the newest L0 file. The deeper sources are unchanged. -/
def flushDrop : View → View
  -- A view with no source: nothing to flush.
  | [] => []
  -- A memtable and the rest: an empty memtable, then the old memtable's newest versions as a file,
  -- then the rest.
  | mem :: rest => [] :: flushNewest mem :: rest

/-- The memtable of the counterexample's captured view, after one write
that landed in it after the capture: key 1 at sequence 1, key 2 at
sequence 1, and key 2 at sequence 3 (the later write). The snapshot is 1. -/
def capturedMem : Source := [(2, 3), (2, 1), (1, 1)]

/-- The counterexample's captured view: that memtable and nothing older. -/
def captured : View := [capturedMem, []]

/-- **`view_per_key_breaks_batch`.** The batch reads keys 1 and 2 at
snapshot 1. Between the two keys a flush writes the memtable out keeping
only the newest version of each key, which drops key 2's version at 1.

* One view for the batch answers both keys from the captured view: key 1
  and key 2 both at sequence 1.
* One view per key resolves key 2 against the flushed view, where its only
  version is at 3, too new for the snapshot. It answers key 2 absent,
  although the batch's snapshot holds it at sequence 1. -/
theorem view_per_key_breaks_batch :
    -- The statement: one view for the whole batch answers key 1 at sequence 1 and key 2 at sequence
    -- 1; one view per key answers key 1 at sequence 1 and key 2 absent.
    batchOneView captured [1, 2] 1 = [(1, some 1), (2, some 1)] ∧
    batchPerKey [captured, flushDrop captured] [1, 2] 1 = [(1, some 1), (2, none)] := by
  -- Both sides are concrete; Lean evaluates them.
  decide

-- The end of this file's names.
end Regolith.BatchView
