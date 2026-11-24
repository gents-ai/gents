/-!
# HistogramBounds: a histogram sample skips only updates that change nothing

THE STORY. regolith keeps latency and size histograms in per-thread shards
(`src/statistics.rs`). Each shard's histogram is four counters: how many
samples, their sum, the smallest and the largest (`HistogramData`,
statistics.rs:405). Threads past the shard count share a shard, so several
threads may record into one at once, each field with its own relaxed atomic
operations (`HistogramData::record`, statistics.rs:431).

The plain way to record a sample is four read-modify-writes: add one to the
count, add the sample to the sum, lower the minimum to it (`fetch_min`),
raise the maximum to it (`fetch_max`). `record` skips the ones that cannot
change anything: a sample of 0 leaves the sum alone (statistics.rs:433), and the
minimum and maximum are first loaded, and lowered or raised only when the
sample lies outside them (statistics.rs:444-449).

Tiny example of why a skip is safe even though another thread may change the
minimum between the load and the skip. Two threads share a shard whose
minimum is 9. Thread A records 12: it loads 9, sees 12 is not below it, and
skips. Meanwhile thread B records 3 and lowers the minimum to 3. A's skip is
still right: the minimum only ever falls, so a value that was at most 12 when
A loaded it is at most 12 forever, and `fetch_min(12)` would have changed
nothing.

What must never happen: a skip that drops a sample's effect, leaving a
minimum above the smallest sample, a maximum below the largest, or a sum
short of the samples.

THE MEMORY MODEL, in plain words. All four fields are relaxed atomics, and
each is a field of its own: what matters is one location at a time. Every
atomic location has one order of the writes made to it (its "modification
order"), and C11 coherence says: a load reads some write in that order, and a
read-modify-write made later by the same thread reads a write no earlier in
that order than the one the load read. So when a thread loads the minimum
and then (if it does not skip) lowers it, the value its `fetch_min` lowers
is at or after the one it loaded. Below, a run of the minimum is the list of
its values in modification order, `h 0, h 1, ...`; turn `k` is one sample
landing (or skipping) at its place in that order, and `seen k <= k` is the
place its earlier load read. Every thread, every number of samples and every
interleaving of them is some such run. A skipped sample writes nothing; it
is given the place right after its turn, where it leaves the value as it was,
so the other turns read exactly the values they read in the program.

WHAT IS PROVED, for every number of samples and every run:

1. `minRun_eq`: in every run of the code's minimum (load, then `fetch_min`
   only if below), every value of the register is the value it has in the
   run where nothing is skipped. `noSkipMin_is_run` shows that run is one
   the code can take, so the statement is about real runs.
   `noSkipMin_le` and `noSkipMin_attained`: that value, at the end, is the
   smallest of the samples and the start value `u64::MAX`.
2. `maxRun_eq`, `noSkipMax_is_run`, `noSkipMax_ge`, `noSkipMax_attained`:
   the same for the maximum, which only rises from 0.
3. `sumRun_eq` and `noSkipSum_eq`: skipping the sum's update for a sample of
   0 changes no value either, and the sum at the end is the samples' total,
   held at `u64::MAX` (`saturating_add`) in any order.
4. `store_after_skip_loses`: the RED case. A minimum lowered by a plain
   store of the sample after the check, instead of a `fetch_min`, loses a
   smaller sample: two threads both load `u64::MAX`, one stores 5, the other
   then stores 9, and the minimum ends at 9.

The count is one `fetch_add` per sample, with no skip; it is unchanged and
not modelled here (`proofs/tla/ShardedStats.tla` covers a `fetch_add`
shard).

OUT OF SCOPE: `HistogramData::clear` (statistics.rs:462, run by
`Statistics::reset`) racing a record. It stores the start values back with
plain stores, so the minimum no longer only falls across it; the code
documents a reset racing a recorder as approximate (statistics.rs:398-403),
before and after the skips alike.

BACKS `proofs/tla/HistogramBounds.tla`, which checks every interleaving of a
few recorders sharing one shard, each record split into its atomic steps:
`MinExact`, `MaxExact` and `SumExact` there are `minRun_eq`, `maxRun_eq`
and `sumRun_eq` here for every size, and `MC_HistogramBounds_Red_StoreAfterSkip`
there is `store_after_skip_loses` here.

How to read the Lean: `def` defines a function or a property, `theorem`
states a fact and its proof follows `:= by`. Lines starting with `--` are
comments; each proof step is commented. Numbers are natural numbers; a
`u64` is one up to `cap`.
-/

-- Everything below is named Regolith.HistogramBounds.<name>.
namespace Regolith.HistogramBounds

/-! ## The minimum -/

/-- The minimum's values when no sample is skipped: start at `init`, and
each turn `k` lowers it to the sample `xs k` (a `fetch_min`). -/
def noSkipMin (init : Nat) (xs : Nat → Nat) : Nat → Nat
  -- Before any turn: the start value.
  | 0 => init
  -- After turn `k`: the smaller of the value before and the sample.
  | k + 1 => min (noSkipMin init xs k) (xs k)

/-- A run of the code's minimum over `n` turns: `h` starts at `init`, and
turn `k`, whose load read the value at place `seen k` (no later than `k`),
lowers the minimum to its sample only when the sample is below what it
loaded, and else leaves it (statistics.rs:444-446). -/
def MinRun (init : Nat) (xs seen : Nat → Nat) (n : Nat) (h : Nat → Nat) : Prop :=
  -- The start value, and each turn as the code takes it.
  h 0 = init ∧ ∀ k, k < n → seen k ≤ k ∧
    h (k + 1) = (if xs k < h (seen k) then min (h k) (xs k) else h k)

/-- With no skip the minimum only falls: a later value is at most an earlier
one. -/
theorem noSkipMin_antitone (init : Nat) (xs : Nat → Nat) :
    -- The statement: for places `j <= k`, the value at `k` is at most the value at `j`.
    ∀ k j, j ≤ k → noSkipMin init xs k ≤ noSkipMin init xs j := by
  -- Take the later place `k`.
  intro k
  -- Induction on `k`.
  induction k with
  -- Case: `k = 0`.
  | zero =>
    -- Then `j` is 0 too.
    intro j hj; have : j = 0 := by omega
    -- A value is at most itself.
    subst this; exact Nat.le_refl _
  -- Case: `k + 1`, with the claim for `k` known (`ih`).
  | succ k ih =>
    -- Take `j <= k + 1`.
    intro j hj
    -- Either `j` is `k + 1` itself, or `j <= k`.
    by_cases hjk : j = k + 1
    -- The same place: a value is at most itself.
    · subst hjk; exact Nat.le_refl _
    -- An earlier place.
    · have := ih j (by omega)
      -- The value at `k + 1` is the smaller of the value at `k` and a sample.
      show min (noSkipMin init xs k) (xs k) ≤ _
      -- That is at most the value at `k`, which is at most the value at `j`.
      exact Nat.le_trans (Nat.min_le_left _ _) this

/-- **The skips change nothing.** In every run of the code's minimum, every
value is the value with no skip. -/
theorem minRun_eq (init : Nat) (xs seen : Nat → Nat) (n : Nat) (h : Nat → Nat)
    -- `h` is a run of the code's minimum over `n` turns.
    (hr : MinRun init xs seen n h) :
    -- The statement: up to the last turn, every place holds the no-skip value.
    ∀ k, k ≤ n → ∀ j, j ≤ k → h j = noSkipMin init xs j := by
  -- Take the place `k` reached so far.
  intro k
  -- Induction on `k`, the claim being for every place up to `k`.
  induction k with
  -- Case: `k = 0`: only the start.
  | zero =>
    -- The only place is 0.
    intro _ j hj; have : j = 0 := by omega
    -- Both start at `init`.
    subst this; exact hr.1
  -- Case: `k + 1`, with the claim up to `k` known (`ih`).
  | succ k ih =>
    -- Take a place `j <= k + 1`.
    intro hk j hj
    -- Either `j` is the new place `k + 1`, or an old one.
    by_cases hjk : j = k + 1
    -- The new place.
    · subst hjk
      -- Turn `k`: its load read place `seen k <= k`, and the step it took.
      have ⟨hs, hstep⟩ := hr.2 k (by omega)
      -- The value at `k` is the no-skip value.
      have hk' := ih (by omega) k (Nat.le_refl _)
      -- So is the value its load read.
      have hseen := ih (by omega) (seen k) hs
      -- Rewrite the step with the no-skip values.
      rw [hstep, hk', hseen]
      -- The no-skip value at `k + 1` is a `fetch_min`.
      show _ = min (noSkipMin init xs k) (xs k)
      -- Split on whether the sample was below what was loaded.
      split
      -- Below: the code lowers too; the same value.
      · rfl
      -- Not below: the code skips.
      · next hnot =>
        -- The value now is at most the value loaded, which is at most the sample...
        have hmono := noSkipMin_antitone init xs k (seen k) hs
        -- ...so lowering to the sample would have changed nothing.
        omega
    -- An old place: the claim for `k`.
    · exact ih (by omega) j (by omega)

/-- The run with no skip is a run of the code, whatever each load read: the
statement above is about runs that exist. -/
theorem noSkipMin_is_run (init : Nat) (xs seen : Nat → Nat) (n : Nat)
    -- Given every load reads a place no later than its turn: the no-skip values are a run.
    (hs : ∀ k, seen k ≤ k) : MinRun init xs seen n (noSkipMin init xs) := by
  -- The start holds by definition; each turn is left.
  refine ⟨rfl, fun k _ => ⟨hs k, ?_⟩⟩
  -- The no-skip step is a `fetch_min`.
  show min (noSkipMin init xs k) (xs k) = _
  -- Split on whether the sample was below what was loaded.
  split
  -- Below: the code's step is the same `fetch_min`.
  · rfl
  -- Not below: the code skips.
  · next hnot =>
    -- The value now is at most the value loaded, which is at most the sample.
    have hmono := noSkipMin_antitone init xs k (seen k) (hs k)
    -- So the `fetch_min` leaves it: the same value.
    omega

/-- The minimum at the end is at most the start value and every sample. -/
theorem noSkipMin_le (init : Nat) (xs : Nat → Nat) :
    -- The statement: after `n` turns, at most `init` and at most each of the `n` samples.
    ∀ n, noSkipMin init xs n ≤ init ∧ ∀ i, i < n → noSkipMin init xs n ≤ xs i := by
  -- Take the number of turns.
  intro n
  -- Induction on it.
  induction n with
  -- Case: no turn: the start value, and no sample.
  | zero => exact ⟨Nat.le_refl _, fun i hi => absurd hi (Nat.not_lt_zero _)⟩
  -- Case: one turn more, with the claim before it known (`ih`).
  | succ k ih =>
    -- Below the start: the new value is at most the old, which is.
    refine ⟨Nat.le_trans (Nat.min_le_left _ _) ih.1, fun i hi => ?_⟩
    -- Each sample: the new one, or an earlier one.
    by_cases hik : i = k
    -- The new sample: the minimum is at most it.
    · subst hik; exact Nat.min_le_right _ _
    -- An earlier sample: the new value is at most the old, which is at most it.
    · exact Nat.le_trans (Nat.min_le_left _ _) (ih.2 i (by omega))

/-- The minimum at the end is the start value or one of the samples: it is
the smallest of them, not merely a bound. -/
theorem noSkipMin_attained (init : Nat) (xs : Nat → Nat) :
    -- The statement: after `n` turns, the value is `init` or some sample among the `n`.
    ∀ n, noSkipMin init xs n = init ∨ ∃ i, i < n ∧ noSkipMin init xs n = xs i := by
  -- Take the number of turns.
  intro n
  -- Induction on it.
  induction n with
  -- Case: no turn: the start value.
  | zero => exact Or.inl rfl
  -- Case: one turn more, with the claim before it known (`ih`).
  | succ k ih =>
    -- The new value is the smaller of the old and the new sample.
    show min (noSkipMin init xs k) (xs k) = init ∨
      ∃ i, i < k + 1 ∧ min (noSkipMin init xs k) (xs k) = xs i
    -- Either the old value is the smaller, or the sample is.
    by_cases hle : noSkipMin init xs k ≤ xs k
    -- The old value stays.
    · rw [Nat.min_eq_left hle]
      -- It was the start value or an earlier sample.
      rcases ih with h | ⟨i, hi, he⟩
      -- The start value.
      · exact Or.inl h
      -- An earlier sample, still one of the first `k + 1`.
      · exact Or.inr ⟨i, by omega, he⟩
    -- The new sample is smaller: it is the value.
    · rw [Nat.min_eq_right (by omega)]
      -- Sample `k`.
      exact Or.inr ⟨k, by omega, rfl⟩

/-! ## The maximum, the mirror image -/

/-- The maximum's values when no sample is skipped: start at `init`, and
each turn raises it to its sample (a `fetch_max`). -/
def noSkipMax (init : Nat) (xs : Nat → Nat) : Nat → Nat
  -- Before any turn: the start value.
  | 0 => init
  -- After turn `k`: the larger of the value before and the sample.
  | k + 1 => max (noSkipMax init xs k) (xs k)

/-- A run of the code's maximum: turn `k` raises it only when its sample is
above what its load read (statistics.rs:447-449). -/
def MaxRun (init : Nat) (xs seen : Nat → Nat) (n : Nat) (h : Nat → Nat) : Prop :=
  -- The start value, and each turn as the code takes it.
  h 0 = init ∧ ∀ k, k < n → seen k ≤ k ∧
    h (k + 1) = (if h (seen k) < xs k then max (h k) (xs k) else h k)

/-- With no skip the maximum only rises. -/
theorem noSkipMax_monotone (init : Nat) (xs : Nat → Nat) :
    -- The statement: for places `j <= k`, the value at `j` is at most the value at `k`.
    ∀ k j, j ≤ k → noSkipMax init xs j ≤ noSkipMax init xs k := by
  -- Take the later place `k`.
  intro k
  -- Induction on `k`.
  induction k with
  -- Case: `k = 0`.
  | zero =>
    -- Then `j` is 0 too.
    intro j hj; have : j = 0 := by omega
    -- A value is at most itself.
    subst this; exact Nat.le_refl _
  -- Case: `k + 1`, with the claim for `k` known (`ih`).
  | succ k ih =>
    -- Take `j <= k + 1`.
    intro j hj
    -- Either `j` is `k + 1` itself, or `j <= k`.
    by_cases hjk : j = k + 1
    -- The same place: a value is at most itself.
    · subst hjk; exact Nat.le_refl _
    -- An earlier place.
    · have := ih j (by omega)
      -- The value at `k + 1` is the larger of the value at `k` and a sample.
      show _ ≤ max (noSkipMax init xs k) (xs k)
      -- The value at `j` is at most the value at `k`, which is at most that.
      exact Nat.le_trans this (Nat.le_max_left _ _)

/-- **The skips change nothing**, for the maximum. -/
theorem maxRun_eq (init : Nat) (xs seen : Nat → Nat) (n : Nat) (h : Nat → Nat)
    -- `h` is a run of the code's maximum over `n` turns.
    (hr : MaxRun init xs seen n h) :
    -- The statement: up to the last turn, every place holds the no-skip value.
    ∀ k, k ≤ n → ∀ j, j ≤ k → h j = noSkipMax init xs j := by
  -- Take the place `k` reached so far.
  intro k
  -- Induction on `k`, the claim being for every place up to `k`.
  induction k with
  -- Case: `k = 0`: only the start.
  | zero =>
    -- The only place is 0.
    intro _ j hj; have : j = 0 := by omega
    -- Both start at `init`.
    subst this; exact hr.1
  -- Case: `k + 1`, with the claim up to `k` known (`ih`).
  | succ k ih =>
    -- Take a place `j <= k + 1`.
    intro hk j hj
    -- Either `j` is the new place `k + 1`, or an old one.
    by_cases hjk : j = k + 1
    -- The new place.
    · subst hjk
      -- Turn `k`: its load read place `seen k <= k`, and the step it took.
      have ⟨hs, hstep⟩ := hr.2 k (by omega)
      -- The value at `k` is the no-skip value.
      have hk' := ih (by omega) k (Nat.le_refl _)
      -- So is the value its load read.
      have hseen := ih (by omega) (seen k) hs
      -- Rewrite the step with the no-skip values.
      rw [hstep, hk', hseen]
      -- The no-skip value at `k + 1` is a `fetch_max`.
      show _ = max (noSkipMax init xs k) (xs k)
      -- Split on whether the sample was above what was loaded.
      split
      -- Above: the code raises too; the same value.
      · rfl
      -- Not above: the code skips.
      · next hnot =>
        -- The value now is at least the value loaded, which is at least the sample...
        have hmono := noSkipMax_monotone init xs k (seen k) hs
        -- ...so raising to the sample would have changed nothing.
        omega
    -- An old place: the claim for `k`.
    · exact ih (by omega) j (by omega)

/-- The run with no skip is a run of the code's maximum. -/
theorem noSkipMax_is_run (init : Nat) (xs seen : Nat → Nat) (n : Nat)
    -- Given every load reads a place no later than its turn: the no-skip values are a run.
    (hs : ∀ k, seen k ≤ k) : MaxRun init xs seen n (noSkipMax init xs) := by
  -- The start holds by definition; each turn is left.
  refine ⟨rfl, fun k _ => ⟨hs k, ?_⟩⟩
  -- The no-skip step is a `fetch_max`.
  show max (noSkipMax init xs k) (xs k) = _
  -- Split on whether the sample was above what was loaded.
  split
  -- Above: the code's step is the same `fetch_max`.
  · rfl
  -- Not above: the code skips.
  · next hnot =>
    -- The value now is at least the value loaded, which is at least the sample.
    have hmono := noSkipMax_monotone init xs k (seen k) (hs k)
    -- So the `fetch_max` leaves it: the same value.
    omega

/-- The maximum at the end is at least the start value and every sample. -/
theorem noSkipMax_ge (init : Nat) (xs : Nat → Nat) :
    -- The statement: after `n` turns, at least `init` and at least each of the `n` samples.
    ∀ n, init ≤ noSkipMax init xs n ∧ ∀ i, i < n → xs i ≤ noSkipMax init xs n := by
  -- Take the number of turns.
  intro n
  -- Induction on it.
  induction n with
  -- Case: no turn: the start value, and no sample.
  | zero => exact ⟨Nat.le_refl _, fun i hi => absurd hi (Nat.not_lt_zero _)⟩
  -- Case: one turn more, with the claim before it known (`ih`).
  | succ k ih =>
    -- Above the start: the old value is, and the new one is at least the old.
    refine ⟨Nat.le_trans ih.1 (Nat.le_max_left _ _), fun i hi => ?_⟩
    -- Each sample: the new one, or an earlier one.
    by_cases hik : i = k
    -- The new sample: the maximum is at least it.
    · subst hik; exact Nat.le_max_right _ _
    -- An earlier sample: at most the old value, which is at most the new.
    · exact Nat.le_trans (ih.2 i (by omega)) (Nat.le_max_left _ _)

/-- The maximum at the end is the start value or one of the samples. -/
theorem noSkipMax_attained (init : Nat) (xs : Nat → Nat) :
    -- The statement: after `n` turns, the value is `init` or some sample among the `n`.
    ∀ n, noSkipMax init xs n = init ∨ ∃ i, i < n ∧ noSkipMax init xs n = xs i := by
  -- Take the number of turns.
  intro n
  -- Induction on it.
  induction n with
  -- Case: no turn: the start value.
  | zero => exact Or.inl rfl
  -- Case: one turn more, with the claim before it known (`ih`).
  | succ k ih =>
    -- The new value is the larger of the old and the new sample.
    show max (noSkipMax init xs k) (xs k) = init ∨
      ∃ i, i < k + 1 ∧ max (noSkipMax init xs k) (xs k) = xs i
    -- Either the sample is at most the old value, or above it.
    by_cases hle : xs k ≤ noSkipMax init xs k
    -- The old value stays.
    · rw [Nat.max_eq_left hle]
      -- It was the start value or an earlier sample.
      rcases ih with h | ⟨i, hi, he⟩
      -- The start value.
      · exact Or.inl h
      -- An earlier sample, still one of the first `k + 1`.
      · exact Or.inr ⟨i, by omega, he⟩
    -- The new sample is larger: it is the value.
    · rw [Nat.max_eq_right (by omega)]
      -- Sample `k`.
      exact Or.inr ⟨k, by omega, rfl⟩

/-! ## The sum -/

/-- `u64::MAX`, where `saturating_add` holds the sum. -/
def cap : Nat := 2 ^ 64 - 1

/-- `saturating_add`: the sum, held at `cap`. -/
def sat (a b : Nat) : Nat := min (a + b) cap

/-- The sum's values when no sample is skipped: from 0, each turn adds its
sample, held at `cap`. The compare-and-swap loop (statistics.rs:434-442)
retries until its write lands on the value it read, so each landed write is
one such step: an atomic read-modify-write. -/
def noSkipSum (xs : Nat → Nat) : Nat → Nat
  -- Before any turn: 0.
  | 0 => 0
  -- After turn `k`: the sample added, held at `cap`.
  | k + 1 => sat (noSkipSum xs k) (xs k)

/-- A run of the code's sum: a sample of 0 skips, any other is added
(statistics.rs:433). -/
def SumRun (xs : Nat → Nat) (n : Nat) (h : Nat → Nat) : Prop :=
  -- The start value, and each turn as the code takes it.
  h 0 = 0 ∧ ∀ k, k < n → h (k + 1) = (if xs k ≠ 0 then sat (h k) (xs k) else h k)

/-- The samples' true total, with no cap. -/
def total (xs : Nat → Nat) : Nat → Nat
  -- No samples: 0.
  | 0 => 0
  -- One more sample.
  | k + 1 => total xs k + xs k

/-- **The sum is the saturating total**, whatever the order: adding with a
cap at each step is adding everything and then capping once. -/
theorem noSkipSum_eq (xs : Nat → Nat) : ∀ n, noSkipSum xs n = min (total xs n) cap := by
  -- Take the number of turns.
  intro n
  -- Induction on it.
  induction n with
  -- Case: no turn: 0 on both sides.
  | zero => simp [noSkipSum, total, cap]
  -- Case: one turn more, with the claim before it known (`ih`).
  | succ k ih =>
    -- The new sum is the old one plus the sample, held at `cap`.
    show sat (noSkipSum xs k) (xs k) = min (total xs k + xs k) cap
    -- Replace the old sum by the capped total, unfold the cap, and do the arithmetic.
    rw [ih]; unfold sat; omega

/-- **Skipping zeros changes nothing.** In every run of the code's sum,
every value is the value with no skip. -/
theorem sumRun_eq (xs : Nat → Nat) (n : Nat) (h : Nat → Nat) (hr : SumRun xs n h) :
    -- The statement: up to the last turn, every place holds the no-skip value.
    ∀ k, k ≤ n → h k = noSkipSum xs k := by
  -- Take the place.
  intro k
  -- Induction on it.
  induction k with
  -- Case: the start: both 0.
  | zero => intro _; exact hr.1
  -- Case: `k + 1`, with the claim for `k` known (`ih`).
  | succ k ih =>
    -- The turn is within the run.
    intro hk
    -- Rewrite the step, and the value before it, with the no-skip value.
    rw [hr.2 k (by omega), ih (by omega)]
    -- The no-skip value at `k + 1` adds the sample.
    show _ = sat (noSkipSum xs k) (xs k)
    -- Split on whether the sample is 0.
    split
    -- Not 0: the code adds too; the same value.
    · rfl
    -- 0: the code skips.
    · next h0 =>
      -- The sample is 0.
      have hz : xs k = 0 := by omega
      -- The sum so far is at most `cap`.
      have hle : noSkipSum xs k ≤ cap := by rw [noSkipSum_eq]; exact Nat.min_le_right _ _
      -- So adding 0, held at `cap`, leaves it.
      unfold sat; omega

/-! ## RED: a plain store after the check -/

/-- The bug: after the check, the minimum is set by a plain store of the
sample instead of a `fetch_min`. -/
def StoreRun (init : Nat) (xs seen : Nat → Nat) (n : Nat) (h : Nat → Nat) : Prop :=
  -- The start value, and each turn as the buggy code takes it.
  h 0 = init ∧ ∀ k, k < n → seen k ≤ k ∧
    h (k + 1) = (if xs k < h (seen k) then xs k else h k)

/-- The minimum's values in the bad run: 100 stands for `u64::MAX`, then 5,
then 9. -/
def redH : Nat → Nat
  -- The start.
  | 0 => 100
  -- After thread A stores 5.
  | 1 => 5
  -- After thread B, whose load read the start, stores 9.
  | _ => 9

/-- **RED.** Two threads both load the start value; A stores 5, then B,
whose 9 is below what it loaded, stores 9. This is a run of the buggy code,
it ends at 9, and the true minimum is 5. -/
theorem store_after_skip_loses :
    -- The statement: `redH` is a run of the bug over two turns, ending at 9, against 5 with no skip.
    StoreRun 100 (fun k => if k = 0 then 5 else 9) (fun _ => 0) 2 redH ∧
      redH 2 = 9 ∧ noSkipMin 100 (fun k => if k = 0 then 5 else 9) 2 = 5 := by
  -- The start, and the two end values, compute; each turn is left.
  refine ⟨⟨rfl, ?_⟩, rfl, rfl⟩
  -- Take a turn of the two.
  intro k hk
  -- One case per turn.
  match k, hk with
  -- Turn 0: its load read place 0; it stores 5.
  | 0, _ => exact ⟨Nat.le_refl _, rfl⟩
  -- Turn 1: its load read place 0, the start; it stores 9.
  | 1, _ => exact ⟨Nat.zero_le _, rfl⟩
  -- No third turn.
  | _ + 2, hk => omega

-- The end of the names starting Regolith.HistogramBounds.
end Regolith.HistogramBounds
