/-!
# ClockCache: the lock-free CLOCK block cache never evicts a pinned block

This file backs the TLA+ model `proofs/tla/ClockCache.tla`, invariants
`NoFreedPin`, `ByteBound`, `UsedExact`, `LandedReadable` and `RefsExact`
(configurations `MC_ClockCache_*`).

## The story

The block cache (`src/engine/block_cache.rs`) keeps table blocks under a
byte budget. A block's strong count is its references: one for the cache's
entry, one per reader holding it, one per I/O queue whose landing table
names it. A reader pins a block by taking a reference. The CLOCK hand
evicts an entry only by turning the count from exactly one (the cache's) to
zero in one compare-and-swap, so it never evicts a block anyone else holds,
and the block's memory is freed exactly when its count reaches zero. Every
insert reserves its bytes against the budget in one bounded
compare-and-swap.

Tiny example: block 1 is cached and reader A holds it, so its count is 2.
The hand's swap from 1 to 0 fails, and block 1 stays. When A lets go the
count is 1, and the next pass evicts it and frees it.

## What is proved, for any number of readers, queues and blocks

1. `evict_only_unpinned`: an eviction succeeds only on a block no reader
   holds and no landing names.
2. `reachable_safe`: every block reachable through any sequence of hits,
   drops, landings, re-runs, inserts and evictions keeps its count exact,
   and a block someone holds or a landing names is in memory
   (`NoFreedPin`, `LandedReadable`, `RefsExact`).
3. `reserve_bounded` and `two_level_bounded`: a bounded reservation never
   takes a total past its budget, and the cache's two-level reservation
   (the cache-wide total first, then the shard) keeps every shard's bytes
   within the total and the total within the budget (`ByteBound`).
4. `used_exact`: inserting and evicting entries keeps the bytes counted
   equal to the bytes of what is cached (`UsedExact`).
5. The RED cases as counterexamples: an eviction that ignores pins frees a
   held block (`ignore_pins_breaks`), an unchecked reservation passes the
   budget (`unbounded_breaks`), and a landing with no reference of its own
   is freed by an eviction (`landing_unheld_breaks`).
-/

-- Everything below is named Regolith.ClockCache.<name>.
namespace Regolith.ClockCache

/-- One block, as the cache sees it. -/
structure Block where
  /-- Whether the cache holds an entry for it. -/
  cached : Bool
  /-- How many readers hold it. -/
  holders : Nat
  /-- How many queues' landing tables name it. -/
  landings : Nat
  /-- Its strong count: every reference that exists. -/
  refs : Nat
  /-- Whether its memory exists. -/
  live : Bool

/-- What every reachable block satisfies. -/
structure Inv (b : Block) : Prop where
  /-- **`RefsExact`.** The count is the cache's reference, the readers'
  and the landings'. -/
  refs_exact : b.refs = (if b.cached then 1 else 0) + b.holders + b.landings
  /-- Memory exists exactly while some reference does. -/
  live_iff : b.live = true ↔ 0 < b.refs

/-- A block nobody has read: no entry, no reference, no memory. -/
def fresh : Block := ⟨false, 0, 0, 0, false⟩

/-- The real steps on one block. -/
inductive Step : Block → Block → Prop where
  /-- A reader misses a block not in memory, reads it and keeps it; the
  cache takes an entry when it has room (`admitted`). -/
  | miss (b : Block) (admitted : Bool) (h : b.live = false) :
      -- After the step: the cache has an entry if it admitted the block; one more holder;
      Step b { b with cached := admitted, holders := b.holders + 1,
                      -- one more reference for the reader, and one for the cache if admitted; and
                      -- the block is in memory.
                      refs := b.refs + 1 + (if admitted then 1 else 0), live := true }
  /-- A reader hits a cached block: it takes a reference (its pin). -/
  | hit (b : Block) (h : b.cached = true) :
      -- After the step: one more holder and one more reference.
      Step b { b with holders := b.holders + 1, refs := b.refs + 1 }
  /-- A reader drops a block it held; the last reference frees it. -/
  | drop (b : Block) (h : 0 < b.holders) :
      -- After the step: one holder and one reference fewer;
      Step b { b with holders := b.holders - 1, refs := b.refs - 1,
                      -- the block stays in memory exactly while a reference is left.
                      live := decide (0 < b.refs - 1) }
  /-- A unit lands a block not in memory for a waiting queue: the landing
  takes a reference, and the cache an entry when it has room. -/
  | land (b : Block) (admitted : Bool) (h : b.live = false) :
      -- After the step: the cache has an entry if it admitted the block; one more landing;
      Step b { b with cached := admitted, landings := b.landings + 1,
                      -- one more reference for the landing, and one for the cache if admitted; and
                      -- the block is in memory.
                      refs := b.refs + 1 + (if admitted then 1 else 0), live := true }
  /-- A queue's read runs again and its owner forgets the landing. -/
  | forget (b : Block) (h : 0 < b.landings) :
      -- After the step: one landing and one reference fewer;
      Step b { b with landings := b.landings - 1, refs := b.refs - 1,
                      -- the block stays in memory exactly while a reference is left.
                      live := decide (0 < b.refs - 1) }
  /-- The hand evicts a cached block: only when the count is exactly the
  cache's one reference (the compare-and-swap from 1 to 0). -/
  | evict (b : Block) (hc : b.cached = true) (h1 : b.refs = 1) :
      -- After the step: no entry, no reference, and the memory is freed.
      Step b { b with cached := false, refs := 0, live := false }

/-- The block states reachable from a fresh block. -/
inductive Reachable : Block → Prop where
  /-- A fresh block is reachable. -/
  | fresh : Reachable fresh
  /-- One step from a reachable block reaches another. -/
  | step {b c : Block} : Reachable b → Step b c → Reachable c

/-- The invariant holds for a fresh block. -/
theorem inv_fresh : Inv fresh := by
  -- Every count is zero and nothing is live: both fields evaluate.
  constructor <;> simp [fresh]

/-- **The pin check.** An eviction succeeds only on a block no reader
holds and no landing names: with the cache's entry counted, a count of
one leaves nothing for anyone else. -/
theorem evict_only_unpinned {b : Block} (hi : Inv b) (hc : b.cached = true) (h1 : b.refs = 1) :
    -- The statement: no reader holds the block and no landing names it.
    b.holders = 0 ∧ b.landings = 0 := by
  -- The exact count of this block.
  have := hi.refs_exact
  -- With the entry cached, the count is 1 + holders + landings.
  simp [hc] at this
  -- And it is 1, so both are zero.
  omega

/-- Every step keeps the invariant. -/
theorem inv_step {b c : Block} (hi : Inv b) (hs : Step b c) : Inv c := by
  -- The exact count, before the step.
  have hr := hi.refs_exact
  -- One case per kind of step.
  cases hs with
  -- Case: a miss.
  | miss admitted hlive =>
    -- Not live means no reference: nothing was cached, held or landed.
    have h0 : b.refs = 0 := by
      -- Live iff positive, and it is not live, so the count is not positive.
      have := hi.live_iff; simp [hlive] at this; omega
    -- Prove both fields for the new block.
    constructor
    -- The new count: the reader's, plus the cache's if it was admitted.
    -- Split on both choices; a zero count means no holder or landing and
    -- no entry, so the arithmetic closes.
    · cases admitted <;> cases hcb : b.cached <;> simp [hcb] at hr ⊢ <;> omega
    -- It is live, and its count is at least one.
    · cases admitted <;> simp
  -- Case: a hit.
  | hit hc =>
    -- Prove both fields for the new block.
    constructor
    -- One more holder, one more reference: read the cached count, add one.
    · simp [hc] at hr ⊢; omega
    -- Cached means counted, so the count was positive and stays positive.
    · simp [hc] at hr ⊢; have := hi.live_iff; simp_all; omega
  -- Case: a reader drops the block.
  | drop hh =>
    -- Prove both fields for the new block.
    constructor
    -- One holder less, one reference less: split on whether it is cached
    -- and do the arithmetic.
    · simp at hr ⊢; split <;> simp_all <;> omega
    -- Live exactly while the count stays above zero, by how it is set.
    · simp
  -- Case: a landing.
  | land admitted hlive =>
    -- Not live means no reference at all, as for a miss.
    have h0 : b.refs = 0 := by
      -- Live iff positive, and it is not live, so the count is not positive.
      have := hi.live_iff; simp [hlive] at this; omega
    -- Prove both fields for the new block.
    constructor
    -- The landing's reference, plus the cache's if admitted: the same
    -- split as for a miss.
    · cases admitted <;> cases hcb : b.cached <;> simp [hcb] at hr ⊢ <;> omega
    -- It is live, and its count is at least one.
    · cases admitted <;> simp
  -- Case: the owner forgets a landing.
  | forget hl =>
    -- Prove both fields for the new block.
    constructor
    -- One landing less, one reference less: split on whether it is cached
    -- and do the arithmetic.
    · simp at hr ⊢; split <;> simp_all <;> omega
    -- Live exactly while the count stays above zero, by how it is set.
    · simp
  -- Case: an eviction.
  | evict hc h1 =>
    -- A count of one is the cache's own: nobody else is counted.
    have ⟨hh, hl⟩ := evict_only_unpinned hi hc h1
    -- Prove both fields for the evicted block.
    constructor
    -- No entry, no holder, no landing: a count of zero.
    · simp [hh, hl]
    -- Not live, and nothing counted.
    · simp

/-- Every reachable block satisfies the invariant. -/
theorem inv_reachable {b : Block} (h : Reachable b) : Inv b := by
  -- Induction on how the block was reached.
  induction h with
  -- Case: a fresh block.
  | fresh =>
    -- A fresh block satisfies it.
    exact inv_fresh
  -- Case: the steps so far, then one more.
  | step _ hs ih =>
    -- One more step keeps it.
    exact inv_step ih hs

/-- **`NoFreedPin`, `LandedReadable`, `RefsExact`, for every reachable
block.** A block a reader holds or a landing names is in memory, and its
count is exact. -/
theorem reachable_safe {b : Block} (h : Reachable b) :
    -- The statement: a held block is in memory; a landed block is in memory;
    (0 < b.holders → b.live = true) ∧ (0 < b.landings → b.live = true) ∧
    -- and the count is the cache's entry (if any) plus the holders plus the landings.
    b.refs = (if b.cached then 1 else 0) + b.holders + b.landings := by
  -- The invariant holds for the reachable block.
  have hi := inv_reachable h
  -- Its exact count.
  have hr := hi.refs_exact
  -- Three goals: holders, landings, and the count itself.
  refine ⟨fun hh => ?_, fun hl => ?_, hr⟩
  -- A holder makes the count positive, and a positive count is live.
  ·
    -- The count is positive, and the invariant says that means "in memory".
    exact hi.live_iff.mpr (by omega)
  -- A landing makes the count positive, and a positive count is live.
  ·
    -- The count is positive, and the invariant says that means "in memory".
    exact hi.live_iff.mpr (by omega)

/-! ## The byte bound -/

/-- `bounded_add`: add `size` to `used` only if the result stays within
`cap`; `none` means refused, with nothing changed. -/
def reserve (used size cap : Nat) : Option Nat :=
  -- One compare-and-swap that checks and adds together.
  if used + size ≤ cap then some (used + size) else none

/-- **`ByteBound`, one level.** A reservation that succeeds never passes
the budget. -/
theorem reserve_bounded {used size cap u : Nat} (h : reserve used size cap = some u) :
    -- The statement: the new count is within the budget.
    u ≤ cap := by
  -- Unfold: success happens only in the branch where the sum fits.
  unfold reserve at h
  -- Split on whether the sum fit.
  split at h
  -- It fit: the result is that sum, which fits.
  ·
    -- The result is that sum, and the branch says it fits.
    cases h; assumption
  -- It did not: the result is `none`, not `some u`.
  ·
    -- "none = some u" is impossible.
    cases h

/-- The cache's two counters: the cache-wide total and one shard's bytes. -/
structure Counters where
  /-- Bytes reserved across the cache. -/
  total : Nat
  /-- Bytes this shard holds. -/
  shard : Nat

/-- `BlockCache::reserve`: the cache-wide total first, then the shard; if
the shard refuses, the total is given back. -/
def reserve2 (c : Counters) (size cap shardCap : Nat) : Option Counters :=
  -- First the cache-wide budget ...
  match reserve c.total size cap with
  -- The cache-wide budget refuses: nothing is reserved.
  | none => none
  -- It accepts, with the new total t.
  | some t =>
    -- ... then the shard's share; a refusal leaves both as they were.
    match reserve c.shard size shardCap with
    -- The shard refuses: nothing is reserved, and the total is not kept.
    | none => none
    -- Both accept: the new total and the new shard count.
    | some sh => some ⟨t, sh⟩

/-- `BlockCache::release`: the shard first, then the total. -/
def release2 (c : Counters) (size : Nat) : Counters :=
  -- Both lose the same bytes.
  ⟨c.total - size, c.shard - size⟩

/-- **`ByteBound`, two levels.** Starting from a shard within the total, a
reservation that succeeds keeps the shard within the total, the total within
the budget and the shard within its share, whatever the total was before:
both checks are made by the reservations themselves. -/
theorem two_level_bounded {c c' : Counters} {size cap shardCap : Nat}
    -- Before, the shard's count is within the total.
    (hin : c.shard ≤ c.total)
    -- The two-level reservation succeeds, giving the counters c'.
    (h : reserve2 c size cap shardCap = some c') :
    -- The statement: after it the shard is within the total, the total within the budget, and the
    -- shard within its share.
    c'.shard ≤ c'.total ∧ c'.total ≤ cap ∧ c'.shard ≤ shardCap := by
  -- Unfold the two-level reservation into its two single ones.
  unfold reserve2 at h
  -- Case on the cache-wide reservation.
  cases ht : reserve c.total size cap with
  -- Case: the cache-wide reservation refused.
  | none =>
    -- Refused: nothing is reserved, so `h` cannot say `some`.
    simp [ht] at h
  -- Case: it accepted, with the new total t.
  | some t =>
    -- Case on the shard's reservation.
    cases hsh : reserve c.shard size shardCap with
    -- Case: the shard's reservation refused.
    | none =>
      -- Refused: again nothing is reserved.
      simp [ht, hsh] at h
    -- Case: it accepted, with the new shard count sh.
    | some sh =>
      -- Both went in: the new counters are `t` and `sh`.
      simp [ht, hsh] at h
      -- Replace c' by those counters.
      subst h
      -- Each went in only because it fit, adding `size`.
      have ht' : t = c.total + size ∧ c.total + size ≤ cap := by
        -- Unfold the reservation and look at its two branches: only the one where the sum fits
        -- gives a result.
        unfold reserve at ht; split at ht <;> simp_all
      -- The same for the shard:
      have hs' : sh = c.shard + size ∧ c.shard + size ≤ shardCap := by
        -- unfold the reservation and look at its two branches.
        unfold reserve at hsh; split at hsh <;> simp_all
      -- The shard grew by what the total did, within both budgets.
      simp only
      -- Arithmetic.
      omega

/-- A release of bytes the shard holds keeps the shard within the total. -/
theorem release_keeps {c : Counters} {size : Nat} (hin : c.shard ≤ c.total)
    -- The bytes released are bytes the shard holds.
    (hs : size ≤ c.shard) :
    -- The statement: after the release the shard is still within the total.
    (release2 c size).shard ≤ (release2 c size).total := by
  -- Unfold the release: both counters lose `size`, and the shard held at
  -- least that, so the shard stays within the total.
  simp [release2]; omega

/-! ## The bytes counted are the bytes held -/

/-- The sum of a list of sizes. -/
def total : List Nat → Nat
  -- Nothing cached costs nothing.
  | [] => 0
  -- One entry's size plus the rest.
  | s :: rest => s + total rest

/-- An insert adds its size to the count and its entry to the cache. -/
theorem total_insert (s : Nat) (cached : List Nat) : total (s :: cached) = s + total cached := by
  -- By definition.
  rfl

/-- Removing an entry from the cache (anywhere in it) takes exactly its
size off the sum. -/
theorem total_remove (pre post : List Nat) (s : Nat) :
    -- The statement: the sum with the entry s in it is s plus the sum without it.
    total (pre ++ s :: post) = s + total (pre ++ post) := by
  -- Induction on the entries before it.
  induction pre with
  -- Case: nothing before it.
  | nil =>
    -- Nothing before it: both sides are `s` plus the rest, by definition.
    rfl
  -- Case: an entry p before it, then the rest, with the claim for the rest (ih) known.
  | cons p rest ih =>
    -- One entry `p` before it: unfold one step of the sum on each side,
    -- use the hypothesis for the rest, and rearrange the additions.
    simp [total, ih]; omega

/-- **`UsedExact`.** Starting from `used = total cached`, an insert that
adds `s` to both, or an eviction that takes an entry and its size away,
keeps the count equal to what is cached. -/
theorem used_exact (pre post : List Nat) (s used : Nat)
    -- The count equals what is cached.
    (h : used = total (pre ++ post)) :
    -- The statement: after an insert of s the count equals what is cached, with s in it;
    used + s = total (pre ++ s :: post) ∧
    -- and after evicting s again it equals what is cached without it.
    (used + s) - s = total (pre ++ post) := by
  -- Rewrite the sum with the entry as `s` plus the sum without it, and the
  -- old count as that sum; what is left is arithmetic.
  rw [total_remove, h]; omega

/-! ## The RED cases, as counterexamples -/

/-- A cached block one reader holds: a count of 2. -/
def heldBlock : Block := ⟨true, 1, 0, 2, true⟩

/-- The IgnorePins eviction: drop the cache's reference and free the block,
whoever else holds it. -/
def evictIgnoringPins (b : Block) : Block :=
  -- No entry, one reference fewer, memory gone.
  { b with cached := false, refs := b.refs - 1, live := false }

/-- **IgnorePins.** The held block is a reachable kind of state (its count
is exact), and the eviction that ignores pins leaves its reader holding
freed memory. -/
theorem ignore_pins_breaks :
    -- The statement: the held block's count is exact; after the broken eviction it still has a
    -- holder,
    Inv heldBlock ∧ 0 < (evictIgnoringPins heldBlock).holders ∧
    -- yet its memory is freed.
    (evictIgnoringPins heldBlock).live = false := by
  -- The invariant's two fields evaluate on the concrete block; the two
  -- facts after the eviction evaluate too.
  refine ⟨⟨by decide, by decide⟩, by decide, by decide⟩

/-- **Unbounded.** Two blocks of size 1 fill a budget of 2; a third insert
that skips the check takes the bytes to 3. -/
theorem unbounded_breaks : reserve 2 1 2 = none ∧ ¬ (2 + 1 ≤ 2) := by
  -- The checked reservation refuses; the unchecked one would pass the cap.
  decide

/-- A block landed for one queue without a reference of its own: its
count is only the cache's. -/
def unheldLanding : Block := ⟨true, 0, 1, 1, true⟩

/-- **LandingUnheld.** The eviction's count check passes on that block (its
count is 1), the eviction frees it, and the landing still names it. Its
count misses the landing, so the real protocol never reaches it. -/
theorem landing_unheld_breaks :
    -- The statement: the block's count is 1;
    unheldLanding.refs = 1 ∧
    -- after the eviction a landing still names it,
    0 < ({ unheldLanding with cached := false, refs := 0, live := false } : Block).landings ∧
    ({ unheldLanding with cached := false, refs := 0, live := false } : Block).live = false ∧
    ¬ Inv unheldLanding := by
  -- The three facts evaluate on the concrete block ...
  refine ⟨by decide, by decide, by decide, ?_⟩
  -- ... and an exact count would have to include the landing: 1 is not 2.
  intro hi
  -- An exact count would be the cache's entry plus the landing.
  have := hi.refs_exact
  -- Computing it on this block gives 1 = 2: impossible.
  simp [unheldLanding] at this

-- End of the namespace opened at the top.
end Regolith.ClockCache
