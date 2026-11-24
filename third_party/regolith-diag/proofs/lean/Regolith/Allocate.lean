/-!
# Allocate: reserved ranges never overlap, only grow, and survive crashes (plan 3.7)

This file backs the TLA+ model `proofs/tla/Allocate.tla`, invariants
`INV_Unique`, `INV_Monotonic`, `INV_UseDurable` and `INV_UsesUnique`
(configurations `MC_Allocate_Green_Immediate` and
`MC_Allocate_Green_Eventual`), and the REDs `MC_Allocate_Red_LogAfterUse`
and `MC_Allocate_Red_LogAfterUse_Reuse`.

## The story, for a reader who has never seen the code

Programs need fresh numbers, such as the next document id.
`Db::allocate(key, n)` (src/allocate.rs) hands out the next `n` numbers of a
counter. It runs in the one step of the engine that handles one write at a
time (`RegolithEngine::allocate`, src/engine/commit/allocate.rs): it reads
the counter `c` from the log, reserves `c + 1, ..., c + n`, and writes an
allocation record holding the new counter, before it returns. A commit that
uses those values comes later, because allocate returned before it began,
so its record is newer than the allocation's. A power cut keeps a front
part of the log in time order and drops only the newest records
(`WalRecovery.lean`, `recovers_prefix`).

Tiny example of what could go wrong: a caller is handed 1 and commits a
document with id 1. If the record "1 was handed out" could reach the log
after that commit, a power cut could keep the commit and lose the record.
The counter would restart at 0, the next caller would get 1 too, and two
documents would carry id 1. The proofs below show the design cannot do
that, and the last section shows the broken variant doing exactly that.

What is proved, in plain words, for any number of allocations, uses and
crashes:

1. `step_inv` and `reachable_inv`: every allocation starts right after the
   counter the older records left, and every use is covered by an older
   allocation. Both survive a crash, because a crash only drops the newest
   records.
2. `ranges_grow` and `ranges_disjoint`: a newer allocation starts after
   every older one ends, so no two share a value.
3. `uses_allocated`: every use has an allocation older than it, so any
   prefix a crash keeps that holds the use holds its allocation too.
4. `alloc_fresh`: the next allocation starts above every value the log
   allocates or uses, so a value a surviving commit used is never handed
   out again, after any crash.
5. `log_after_use_reissues`: when the allocation record may be logged after
   the commit that uses it (the RED LogAfterUse), a crash keeps the use,
   drops the allocation, and the same values are handed out again.
   `design_refuses_use_before_alloc`: the design's steps cannot log a use
   before its allocation.

How to read the Lean: `def` defines a function or a property, `theorem`
states a fact and its proof follows `:= by`. Lines starting with `--` are
comments. Values and counters are plain natural numbers.
-/

-- Everything below is named Regolith.Allocate.<name>.
namespace Regolith.Allocate

/-! ## The objects -/

/-- One record of the log. -/
inductive Rec where
  /-- An allocation of the values `lo, ..., hi` (none when `hi < lo`, which
  is `allocate(key, 0)`). -/
  | alloc (lo hi : Nat)
  /-- A commit using the values `vs`. -/
  | use (vs : List Nat)
  -- Lean may decide whether two records are equal by comparing them.
  deriving DecidableEq

/-
The log is a list of records, NEWEST FIRST: a new record goes on the
front. A crash keeps a gap-free prefix of the log in time order, which is
a suffix of this list: `log.drop j` drops the `j` newest records.
-/

/-- The counter key: the `hi` of the newest allocation, 0 while there is
none (an absent key reads as 0). -/
def counter : List Rec → Nat
  -- No record: absent.
  | [] => 0
  -- The newest record is an allocation: it wrote the counter.
  | .alloc _ hi :: _ => hi
  -- The newest record is a use: the counter is what the older ones left.
  | .use _ :: rest => counter rest

/-- `Allocated log v`: some allocation in `log` covers the value `v`. -/
def Allocated (log : List Rec) (v : Nat) : Prop :=
  -- Some allocation lo..hi is in the log, and v lies between lo and hi.
  ∃ lo hi, Rec.alloc lo hi ∈ log ∧ lo ≤ v ∧ v ≤ hi

/-! ## The design's steps and its invariant -/

/-- The steps of the design. -/
inductive Step : List Rec → List Rec → Prop
  /-- `allocate(key, n)`: read the counter `c`, reserve `c + 1, ..., c + n`,
  log the allocation. No sync: it only joins the log. -/
  | alloc (log : List Rec) (n : Nat) :
      -- The log gains, at its newest end, the allocation from counter + 1 to counter + n.
      Step log (.alloc (counter log + 1) (counter log + n) :: log)
  /-- A commit using values that allocations already in the log returned:
  allocate returned them before the commit began. -/
  | use (log : List Rec) (vs : List Nat)
      -- Every value used was allocated by an older record.
      (hvs : ∀ v ∈ vs, Allocated log v) :
      -- The log gains, at its newest end, the use of those values.
      Step log (.use vs :: log)
  /-- A crash keeps a gap-free prefix in time order: it drops the `j`
  newest records. -/
  | crash (log : List Rec) (j : Nat) : Step log (log.drop j)

/-- Any number of steps, one after another. -/
inductive Steps : List Rec → List Rec → Prop
  /-- No step at all. -/
  | refl (log : List Rec) : Steps log log
  /-- Some steps, then one more. -/
  | tail {a b c : List Rec} : Steps a b → Step b c → Steps a c

/-- `Grows log`: every allocation starts right after the counter the older
records left, and leaves the counter no lower. -/
def Grows : List Rec → Prop
  -- An empty log.
  | [] => True
  -- An allocation: it starts at the older counter plus one, ends at or
  -- above that counter, and the older records grow too.
  | .alloc lo hi :: rest => lo = counter rest + 1 ∧ counter rest ≤ hi ∧ Grows rest
  -- A use: the older records grow.
  | .use _ :: rest => Grows rest

/-- `Covered log`: every use is covered by allocations older than it. -/
def Covered : List Rec → Prop
  -- An empty log.
  | [] => True
  -- An allocation: the older records are covered.
  | .alloc _ _ :: rest => Covered rest
  -- A use: every value it uses is allocated by an older record, and the
  -- older records are covered.
  | .use vs :: rest => (∀ v ∈ vs, Allocated rest v) ∧ Covered rest

/-- The design's invariant. -/
structure Inv (log : List Rec) : Prop where
  /-- The allocations grow from the counter. -/
  grows : Grows log
  /-- Every use has its older allocation. -/
  covered : Covered log

/-- Dropping the newest records keeps `Grows`: it is a property of each
record and the records older than it. -/
theorem grows_drop : ∀ (j : Nat) (log : List Rec), Grows log → Grows (log.drop j)
  -- Dropping nothing: the same log.
  | 0, _, h => h
  -- Nothing to drop from: the empty log grows trivially.
  | _ + 1, [], _ => trivial
  -- Dropping the newest allocation and j more: the older records grow (the third part of the fact),
  -- so the claim for them applies.
  | j + 1, .alloc _ _ :: rest, h => grows_drop j rest h.2.2
  -- Dropping the newest use and j more: the older records grow, so the claim for them applies.
  | j + 1, .use _ :: rest, h => grows_drop j rest h

/-- Dropping the newest records keeps `Covered`, for the same reason. -/
theorem covered_drop : ∀ (j : Nat) (log : List Rec), Covered log → Covered (log.drop j)
  -- Dropping nothing: the same log.
  | 0, _, h => h
  -- Nothing to drop from: the empty log is covered trivially.
  | _ + 1, [], _ => trivial
  -- Dropping the newest allocation and j more: the older records are covered, so the claim for them
  -- applies.
  | j + 1, .alloc _ _ :: rest, h => covered_drop j rest h
  -- Dropping the newest use and j more: the older records are covered (the second part of the
  -- fact), so the claim for them applies.
  | j + 1, .use _ :: rest, h => covered_drop j rest h.2

/-- **Every step keeps the invariant**, a crash included. -/
theorem step_inv {log log' : List Rec}
    -- The invariant holds before the step.
    (h : Inv log)
    -- One step of the design.
    (hstep : Step log log') :
    -- The statement: the invariant holds after the step.
    Inv log' := by
  -- Split on the kind of step.
  cases hstep with
  -- Case: an allocation of n values.
  | alloc n =>
    -- The new allocation starts at the counter plus one and ends at the
    -- counter plus `n`; nothing older changes.
    exact ⟨⟨rfl, Nat.le_add_right _ _, h.grows⟩, h.covered⟩
  -- Case: a use of the values vs.
  | use vs hvs =>
    -- The new use is covered by the step's hypothesis.
    exact ⟨h.grows, hvs, h.covered⟩
  -- Case: a crash that drops the j newest records.
  | crash j =>
    -- Both properties survive dropping the newest records.
    exact ⟨grows_drop j log h.grows, covered_drop j log h.covered⟩

/-- Every log the design reaches from the empty log satisfies the
invariant. -/
theorem reachable_inv {log : List Rec} (hreach : Steps [] log) : Inv log := by
  -- Go through the steps one at a time.
  induction hreach with
  -- No step: the empty log grows and is covered, trivially.
  | refl => exact ⟨trivial, trivial⟩
  -- Some steps, then one more: the invariant held before the last step (the claim so far), and a
  -- step keeps it.
  | tail _ hstep ih => exact step_inv ih hstep

/-! ## The laws -/

/-- In a growing log, every allocation ends at or below the counter. -/
theorem le_counter : ∀ {log : List Rec} {lo hi : Nat},
    -- The log grows ...
    Grows log →
    -- ... and holds the allocation of `lo, ..., hi`.
    Rec.alloc lo hi ∈ log → hi ≤ counter log
  -- The empty log holds no allocation.
  | [], _, _, _, hmem => absurd hmem List.not_mem_nil
  -- The newest record is an allocation lo'..hi'.
  | .alloc lo' hi' :: rest, lo, hi, h, hmem => by
    -- The counter is the newest allocation's `hi'`.
    obtain ⟨-, hle, hrest⟩ := h
    -- Our allocation is that newest record, or it is among the older ones.
    rcases List.mem_cons.mp hmem with he | hm
    -- It is the newest allocation itself.
    ·
      -- Then hi is hi'.
      cases he
      -- And the counter is hi': equal, so at most.
      exact Nat.le_refl _
    -- An older one: it ends at or below the older counter, which `hi'` is at least.
    ·
      -- The claim for the older records.
      have := le_counter hrest hm
      -- The counter of the whole log is hi'.
      simp only [counter]
      -- hi is at most the older counter, which is at most hi': arithmetic.
      omega
  -- The newest record is a use.
  | .use _ :: rest, lo, hi, h, hmem => by
    -- A use leaves the counter as the older records left it.
    -- Our allocation is that newest record, or it is among the older ones.
    rcases List.mem_cons.mp hmem with he | hm
    -- The newest record is a use, not an allocation: impossible.
    · cases he
    -- The goal, spelled out: hi is at most the counter the older records left.
    · show hi ≤ counter rest
      -- The claim for the older records.
      exact le_counter (log := rest) h hm

/-- **Ranges grow.** For every two allocations, the newer one starts after
the older one ends. `List.Pairwise R log` means `R x y` for every record
`x` newer than `y` in `log`. TLA+: `INV_Monotonic`. -/
theorem ranges_grow : ∀ {log : List Rec},
    -- The log grows.
    Grows log →
    -- The conclusion: for every newer record and every older record of the log,
    log.Pairwise (fun newer older =>
      -- if both are allocations, the older one ends before the newer one starts.
      ∀ lo hi lo' hi', newer = .alloc lo hi → older = .alloc lo' hi' → hi' < lo)
  -- The empty log: nothing to compare.
  | [], _ => List.Pairwise.nil
  -- The newest record is an allocation lo..hi.
  | .alloc lo hi :: rest, h => by
    -- Take the growing fact apart: lo is the older counter plus one, and the older records grow.
    obtain ⟨hlo, -, hrest⟩ := h
    -- Two parts: the newest record against every older one, and the older ones among themselves
    -- (the claim for them).
    refine List.pairwise_cons.mpr ⟨?_, ranges_grow hrest⟩
    -- Every older allocation ends at or below the older counter, and this
    -- one starts one above it.
    -- Take an older record b that is an allocation lo'..hi'.
    intro b hb lo₁ hi₁ lo' hi' hnew hold
    -- The newest record is the allocation lo..hi itself.
    cases hnew
    -- Replace b by the allocation lo'..hi'.
    subst hold
    -- It ends at or below the older counter.
    have := le_counter hrest hb
    -- And lo is that counter plus one: so hi' is below lo.
    omega
  -- The newest record is a use.
  | .use _ :: rest, h => by
    -- A use is no allocation, so it relates to nothing.
    -- Two parts: the newest record against every older one, and the older ones among themselves.
    refine List.pairwise_cons.mpr ⟨?_, ranges_grow h⟩
    -- Take an older record, and suppose the newest were an allocation.
    intro b _ lo hi lo' hi' hnew _
    -- It is a use: impossible, so nothing to show.
    cases hnew

/-- **Ranges never overlap.** No value lies in two allocations. TLA+:
`INV_Unique`. -/
theorem ranges_disjoint {log : List Rec}
    -- The invariant holds.
    (h : Inv log) :
    -- The conclusion: for every newer and older record of the log,
    log.Pairwise (fun newer older =>
      -- if both are allocations and some value v lies in both ranges,
      ∀ lo hi lo' hi' v, newer = .alloc lo hi → older = .alloc lo' hi' →
        -- we have a contradiction.
        lo ≤ v → v ≤ hi → lo' ≤ v → v ≤ hi' → False) := by
  -- The older range ends before the newer one starts.
  -- It is enough to derive this from "the older ends before the newer starts", pair by pair.
  refine (ranges_grow h.grows).imp ?_
  -- Take a pair with that fact, both allocations, and a value v in both ranges.
  intro a b hab lo hi lo' hi' v ha hb h1 _ _ h4
  -- The older range ends below where the newer starts.
  have := hab lo hi lo' hi' ha hb
  -- But v is at least the newer start and at most the older end: arithmetic says no.
  omega

/-- A value allocated in the older records is allocated in the log. -/
theorem allocated_cons {rest : List Rec} {v : Nat} (r : Rec)
    -- (given that the older records allocate v) The statement: the log with r in front allocates v
    -- too.
    (h : Allocated rest v) : Allocated (r :: rest) v := by
  -- Take the allocation that covers v.
  obtain ⟨lo, hi, hmem, h1, h2⟩ := h
  -- It is still in the longer log, and still covers v.
  exact ⟨lo, hi, List.mem_cons_of_mem _ hmem, h1, h2⟩

/-- **Every use has its allocation.** In a covered log, every value a use
takes is allocated by a record older than the use. So any gap-free prefix
in time order that holds the use holds that allocation too: a committed
use implies its allocation is as durable as the use. TLA+:
`INV_UseDurable`. -/
theorem uses_allocated : ∀ (newer : List Rec) {older : List Rec} {vs : List Nat},
    -- The log is the newer records, the use, then the older records ...
    Covered (newer ++ Rec.use vs :: older) →
    -- ... and every value the use takes is allocated among the older ones.
    ∀ v ∈ vs, Allocated older v
  -- No newer record: the use is the newest, and "covered" says its values are allocated among the
  -- older ones.
  | [], _, _, h => h.1
  -- The newest record is an allocation: drop it and use the claim for the rest.
  | .alloc _ _ :: newer, _, _, h => uses_allocated newer h
  -- The newest record is another use: drop it (the rest is covered) and use the claim for the rest.
  | .use _ :: newer, _, _, h => uses_allocated newer h.2

/-- In a covered log, every value any use takes is allocated somewhere in
the log. -/
theorem use_allocated_mem : ∀ {log : List Rec} {vs : List Nat} {v : Nat},
    -- The statement: if the log is covered, holds the use of vs, and v is one of vs, then the log
    -- allocates v.
    Covered log → Rec.use vs ∈ log → v ∈ vs → Allocated log v
  -- The empty log holds no use.
  | [], _, _, _, hmem, _ => absurd hmem List.not_mem_nil
  -- The newest record is an allocation.
  | .alloc lo hi :: rest, vs, v, h, hmem, hv => by
    -- The use is older than the newest record.
    -- The use is that newest record, or it is among the older ones.
    rcases List.mem_cons.mp hmem with he | hm
    -- The newest record is an allocation, not a use: impossible.
    · cases he
    -- Among the older ones: they allocate v (the claim for them), so the whole log does.
    · exact allocated_cons _ (use_allocated_mem h hm hv)
  -- The newest record is a use of ws.
  | .use ws :: rest, vs, v, h, hmem, hv => by
    -- Our use is that newest record, or it is among the older ones.
    rcases List.mem_cons.mp hmem with he | hm
    -- It is the newest use: covered by an older allocation.
    ·
      -- Then vs is ws.
      cases he
      -- "Covered" says the older records allocate v, so the whole log does.
      exact allocated_cons _ (h.1 v hv)
    -- An older use.
    ·
      -- The older records allocate v (the claim for them), so the whole log does.
      exact allocated_cons _ (use_allocated_mem h.2 hm hv)

/-- **The next allocation is fresh.** Every value the log allocates or uses
is at most the counter, and the next allocation starts at the counter plus
one. So after any crash (which is a step, and keeps the invariant), the
next range shares no value with any surviving allocation or use: a value a
surviving commit used is never handed out again. TLA+: `INV_UsesUnique`. -/
theorem alloc_fresh {log : List Rec} {v : Nat}
    -- The invariant holds.
    (h : Inv log)
    -- `v` is allocated or used in the log.
    (hv : Allocated log v ∨ ∃ vs, Rec.use vs ∈ log ∧ v ∈ vs) :
    -- The statement: v is below the counter plus one, where the next allocation starts.
    v < counter log + 1 := by
  -- A used value is allocated, so either way some allocation covers it.
  have hal : Allocated log v := by
    -- v is allocated, or some use takes it.
    rcases hv with ha | ⟨vs, hmem, hin⟩
    -- Allocated: done.
    · exact ha
    -- Used: then allocated too, by the lemma above.
    · exact use_allocated_mem h.covered hmem hin
  -- That allocation ends at or below the counter.
  -- Take the allocation lo..hi that covers v: v is at most hi.
  obtain ⟨lo, hi, hmem, -, hle⟩ := hal
  -- And hi is at most the counter.
  have := le_counter h.grows hmem
  -- So v is below the counter plus one.
  omega

/-! ## The RED case: the allocation record logged after its use -/

/-- The defective steps: allocations as in the design, but a use may be
logged before the allocation that returned its values (allocate handed
them out of an in-memory counter and logs the record later). -/
inductive StepLate : List Rec → List Rec → Prop
  /-- As `Step.alloc`. -/
  | alloc (log : List Rec) (n : Nat) :
      -- The log gains the allocation from counter + 1 to counter + n.
      StepLate log (.alloc (counter log + 1) (counter log + n) :: log)
  /-- The defect: a use needs no older allocation. -/
  | use (log : List Rec) (vs : List Nat) : StepLate log (.use vs :: log)
  /-- As `Step.crash`. -/
  | crash (log : List Rec) (j : Nat) : StepLate log (log.drop j)

/-- Newest first: the allocation of 1..2 reached the log after the commit
that used 1 and 2. -/
def lateLog : List Rec := [.alloc 1 2, .use [1, 2]]

/-- **`log_after_use_reissues`.** The defective steps log the use of 1 and
2, then their allocation. That log is not covered. A crash that drops the
newest record keeps the use and loses the allocation, the counter reads 0,
and the next allocation hands out 1 and 2 again, beside the surviving use
of them. -/
theorem log_after_use_reissues :
    -- The statement: the defective steps log the use of 1 and 2 first and their allocation second;
    -- that log is not covered; a crash keeps the use and loses the allocation; and the next
    -- allocation hands out 1 and 2 again.
    StepLate [.use [1, 2]] lateLog ∧ StepLate [] [.use [1, 2]] ∧
      ¬ Covered lateLog ∧
      StepLate lateLog [.use [1, 2]] ∧
      StepLate [.use [1, 2]] [.alloc 1 2, .use [1, 2]] := by
  -- Each step is one of the defective steps, named in order; the middle claim, that the log is not
  -- covered, is left to show.
  refine ⟨.alloc [.use [1, 2]] 2, .use [] [1, 2], ?_, .crash lateLog 1, .alloc [.use [1, 2]] 2⟩
  -- Covering the use needs an allocation older than it, and there is none.
  -- Suppose the log were covered.
  intro hcov
  -- Then the use's values are allocated among the records older than it.
  obtain ⟨hall, -⟩ := hcov
  -- Take the allocation that would cover value 1: it is a member of the older records, and there
  -- are none.
  obtain ⟨lo, hi, hmem, -⟩ := hall 1 (by simp)
  -- A member of the empty list: impossible.
  exact absurd hmem List.not_mem_nil

/-- The design refuses that order: from the empty log, no step logs a use
of 1 and 2, because no allocation covers them yet. -/
theorem design_refuses_use_before_alloc : ¬ Step [] [.use [1, 2]] := by
  -- Suppose such a step existed.
  intro h
  -- Name the target, so each kind of step can be compared with it.
  generalize ht : [Rec.use [1, 2]] = t at h
  -- Split on the kind of step.
  cases h with
  -- Case: an allocation.
  | alloc n =>
    -- An allocation is no use.
    simp at ht
  -- Case: a use of vs with every value allocated in the empty log.
  | use vs hvs =>
    -- The use's values must be allocated in the empty log: impossible.
    -- The step's result is the target, so vs is [1, 2].
    simp only [List.cons.injEq, Rec.use.injEq, and_true] at ht
    -- Replace vs by it.
    subst ht
    -- Value 1 would be covered by an allocation in the empty log.
    obtain ⟨lo, hi, hmem, -⟩ := hvs 1 (by simp)
    -- A member of the empty list: impossible.
    exact absurd hmem List.not_mem_nil
  -- Case: a crash.
  | crash j =>
    -- A crash of the empty log leaves it empty.
    simp at ht

-- The end of this file's names.
end Regolith.Allocate
