---- MODULE HistogramBounds ----
\* ===========================================================================
\* THE STORY
\* ===========================================================================
\* regolith keeps latency and size histograms (src/statistics.rs). Each one is
\* kept per shard, one shard per thread number, and threads past the shard
\* count share a shard. A shard's histogram is four counters: how many
\* samples (`count`), their total (`sum`), the smallest (`min`) and the
\* largest (`max`). Recording a sample (`HistogramData::record`) touches each
\* counter with its own atomic operations, and several threads sharing a
\* shard may record at once.
\*
\* Recording skips each read-modify-write that cannot change its counter:
\* the count always goes up, a sample of 0 leaves the sum alone, and the
\* minimum and maximum are loaded first and lowered or raised only when the
\* sample lies outside them. The minimum only ever falls and the maximum only
\* ever rises, so a bound that covers the sample when it is loaded covers it
\* for good, even if another thread changes it before the skip.
\*
\* Tiny example. Three threads share a shard. Thread 2 records 2, so the
\* minimum is 2. Thread 1 records 5: it loads the minimum, 2, and will skip,
\* since 5 is not below 2. Before it does, thread 3 records 0 and lowers the
\* minimum to 0. Thread 1 now skips on a value the minimum has left behind,
\* and that is still right: lowering 0 to 5 would change nothing.
\*
\* What this model checks cannot happen: once every thread is done, a count,
\* sum, minimum or maximum that is not exactly what the samples make
\* (CountExact, SumExact, MinExact, MaxExact).
\*
\* PROVED FOR EVERY SIZE in Lean, proofs/lean/Regolith/HistogramBounds.lean:
\* minRun_eq and maxRun_eq (every value of the minimum and maximum, skips and
\* all, is the value with no skip: MinExact, MaxExact), sumRun_eq and
\* noSkipSum_eq (SumExact), and store_after_skip_loses (the RED
\* MC_HistogramBounds_Red_StoreAfterSkip). TLC checks every interleaving of
\* three threads with one sample each.
\*
\* MEMORY ORDERING. Every access is relaxed, and each counter is a location
\* of its own. Each step below is one access to one counter, taken in one
\* order. That is what per-location coherence gives a relaxed atomic: one
\* order of the writes to it, a load that reads one of them, and a later
\* read-modify-write by the same thread that reads one no earlier. Nothing
\* here relies on an order between two counters.
\*
\* WHAT IS MODELLED, and the code each piece mirrors (src/statistics.rs, the
\* tree this model was checked against):
\*   count       HistogramData::count (406), 0 at the start (416).
\*   sum         HistogramData::sum (407), 0 at the start (417).
\*   mn          HistogramData::min (409), `u64::MAX` at the start (418);
\*               Cap stands for `u64::MAX` here.
\*   mx          HistogramData::max (410), 0 at the start (419).
\*   sample      the `value` each thread passes to `record` (431).
\*   pc          where each thread is in `record` (431-450).
\*   seenMin     what the thread's `self.min.load(Relaxed)` read (444).
\*   seenMax     what the thread's `self.max.load(Relaxed)` read (447).
\*   seenSum     RED SumLoadStore only: the sum a thread loaded.
\*   staleSkip   ghost: some thread skipped on a loaded bound that the
\*               counter had already left.
\*
\*   Model step   Rust
\*   Count        `self.count.fetch_add(1, Relaxed)` (432)
\*   Sum          `if value != 0` (433) and the compare-and-swap loop
\*                (434-442) that adds the sample with `saturating_add`. The
\*                loop retries until its swap lands on the value it read, so
\*                its one landed swap is one atomic step here.
\*   SumStore     RED SumLoadStore only: the second half of a load and a
\*                store standing in for the loop.
\*   MinLoad      `self.min.load(Relaxed)` (444)
\*   MinAct       `if value < ..` (444) and `self.min.fetch_min(..)` (445)
\*   MaxLoad      `self.max.load(Relaxed)` (447)
\*   MaxAct       `if value > ..` (447) and `self.max.fetch_max(..)` (448)
\*
\*   Rule         Rust it holds for
\*   CountExact   the count's `fetch_add` (432)
\*   SumExact     the zero skip (433) and the loop (434-442)
\*   MinExact     the minimum's load, check and `fetch_min` (444-446)
\*   MaxExact     the maximum's load, check and `fetch_max` (447-449)
\*
\* WHAT IS LEFT OUT, and why that loses nothing:
\*   - Which shard a thread uses (`Statistics::shard`, 609): all threads here
\*     share one, the case where they can race. Shards are summed by a read,
\*     which proofs/tla/ShardedStats.tla checks.
\*   - A read while threads record (`fold_into`, 455): each field is loaded
\*     on its own, so a read during a record may see part of it; the code
\*     documents that (398-403) and it is the same with or without skips.
\*   - `clear` (462, from `Statistics::reset`) racing a record: its plain
\*     stores put the start values back, so the minimum no longer only falls
\*     across it; the code documents a reset racing a recorder as
\*     approximate (403), before the skips as after.
\*   - The level (`StatsLevel`): it decides whether `record` is called at
\*     all (`Statistics::record`, 694), not what it does.
\*   - Many samples per thread: one thread's samples are recorded one after
\*     another, so a second sample is a second thread that starts later;
\*     three threads with one sample each already let a skip meet a change
\*     made between its load and its check (the witness below).
\*
\* CONFIGURATIONS (each GREEN must pass, each RED must break the one rule it
\* names):
\*   MC_HistogramBounds_Green                 the code: three threads, each
\*                                            sample one of 0, 2 and 5.
\*   MC_HistogramBounds_Green_ReachesStaleSkip  the same, with the witness
\*                                            NeverSkipsStale, which must
\*                                            break: a skip on a bound the
\*                                            counter has since left is
\*                                            reached.
\*   MC_HistogramBounds_Red_StoreAfterSkip    the minimum is set by a plain
\*                                            store after the check: MinExact.
\*   MC_HistogramBounds_Red_WrongDirection    the minimum is lowered only
\*                                            when the sample is above what
\*                                            was loaded: MinExact.
\*   MC_HistogramBounds_Red_MaxStoreAfterSkip the maximum is set by a plain
\*                                            store after the check: MaxExact.
\*   MC_HistogramBounds_Red_SumLoadStore      the sum's loop is a load and
\*                                            then a store: SumExact.

\* Natural numbers, and counting the elements of a set.
EXTENDS Naturals, FiniteSets

\* The knobs of a run. Each configuration file (MC_HistogramBounds_*.cfg) gives them values.
CONSTANTS
  \* How many threads record, numbered 1..NRecorders, all on one shard.
  NRecorders,
  \* The samples a thread may record; every choice is checked.
  Values,
  \* `u64::MAX`: the minimum's start value and where the sum stops.
  Cap,
  \* "none" for the code, or the name of the one bug a RED run plants.
  Mutant

\* At least one thread.
ASSUME NRecorders \in Nat \ {0}
\* Every sample fits in a `u64`.
ASSUME Values \subseteq 0..Cap
\* The planted bug is one of these.
ASSUME Mutant \in {"none", "StoreAfterSkip", "WrongDirection", "MaxStoreAfterSkip", "SumLoadStore"}

\* The threads.
Recorders == 1..NRecorders

\* The smallest number in a set that has one.
SetMin(S) == CHOOSE x \in S : \A y \in S : x <= y

\* The largest number in a set that has one.
SetMax(S) == CHOOSE x \in S : \A y \in S : y <= x

\* The smaller of two numbers.
Min2(a, b) == IF a <= b THEN a ELSE b

\* The larger of two numbers.
Max2(a, b) == IF a <= b THEN b ELSE a

\* `saturating_add`: the sum, held at Cap.
Sat(a, b) == Min2(a + b, Cap)

\* Where a thread can be in `record`, in program order.
Steps == {"count", "sum", "sumStore", "minLoad", "minAct", "maxLoad", "maxAct", "done"}

\* The state: everything that changes from one step to the next. Each line says what the variable
\* stands for in the real program.
VARIABLES
  \* HistogramData::count (statistics.rs:406).
  count,
  \* HistogramData::sum (statistics.rs:407).
  sum,
  \* HistogramData::min (statistics.rs:409).
  mn,
  \* HistogramData::max (statistics.rs:410).
  mx,
  \* Each thread's sample: the `value` passed to `record` (statistics.rs:431).
  sample,
  \* Where each thread is in `record`.
  pc,
  \* What each thread's load of the minimum read (statistics.rs:444).
  seenMin,
  \* What each thread's load of the maximum read (statistics.rs:447).
  seenMax,
  \* RED SumLoadStore only: what each thread's load of the sum read.
  seenSum,
  \* Ghost: some thread skipped on a bound the counter had already left.
  staleSkip

\* Every variable, so a step that changes none of them is a pause.
vars == <<count, sum, mn, mx, sample, pc, seenMin, seenMax, seenSum, staleSkip>>

\* The samples' total, with no cap: T[i] adds the first i threads' samples.
Total ==
  \* None for no thread, else one more thread's sample.
  LET T[i \in 0..NRecorders] == IF i = 0 THEN 0 ELSE T[i - 1] + sample[i]
  \* All of them.
  IN T[NRecorders]

\* Every thread has finished its record.
Quiescent == \A r \in Recorders : pc[r] = "done"

\* The start: the shard as `HistogramData::default()` makes it (statistics.rs:413-422), each thread
\* with a sample of its own and about to record it.
Init ==
  \* No sample counted.
  /\ count = 0
  \* Nothing summed.
  /\ sum = 0
  \* The minimum starts at `u64::MAX`.
  /\ mn = Cap
  \* The maximum starts at 0.
  /\ mx = 0
  \* Any sample for each thread: every choice is a run.
  /\ sample \in [Recorders -> Values]
  \* Every thread about to count.
  /\ pc = [r \in Recorders |-> "count"]
  \* Nothing loaded yet.
  /\ seenMin = [r \in Recorders |-> 0]
  \* (nor the maximum)
  /\ seenMax = [r \in Recorders |-> 0]
  \* (nor the sum)
  /\ seenSum = [r \in Recorders |-> 0]
  \* No skip yet.
  /\ staleSkip = FALSE

\* A thread counts its sample: one `fetch_add` (statistics.rs:432), first in `record`.
Count(r) ==
  \* The thread is at the start of `record`.
  /\ pc[r] = "count"
  \* One more sample.
  /\ count' = count + 1
  \* On to the sum.
  /\ pc' = [pc EXCEPT ![r] = "sum"]
  \* Nothing else moves.
  /\ UNCHANGED <<sum, mn, mx, sample, seenMin, seenMax, seenSum, staleSkip>>

\* A thread adds its sample to the sum (statistics.rs:433-442): skipped for 0, else the loop's one
\* landed swap. RED SumLoadStore: a load here, and the store comes later (SumStore).
Sum(r) ==
  \* The thread has counted.
  /\ pc[r] = "sum"
  \* The bug: the sum is loaded now and stored later.
  /\ IF Mutant = "SumLoadStore" /\ sample[r] # 0
        \* Remember the sum seen; the store is the next step.
        THEN /\ seenSum' = [seenSum EXCEPT ![r] = sum]
             \* The store comes next.
             /\ pc' = [pc EXCEPT ![r] = "sumStore"]
             \* The sum is not written yet.
             /\ UNCHANGED sum
        \* The code: a sample of 0 skips, any other is added at once, held at Cap.
        ELSE /\ sum' = IF sample[r] # 0 THEN Sat(sum, sample[r]) ELSE sum
             \* On to the minimum.
             /\ pc' = [pc EXCEPT ![r] = "minLoad"]
             \* No sum was loaded.
             /\ UNCHANGED seenSum
  \* Nothing else moves.
  /\ UNCHANGED <<count, mn, mx, sample, seenMin, seenMax, staleSkip>>

\* RED SumLoadStore, the second half: the sum loaded earlier, plus the sample, is stored. Another
\* thread's add in between is overwritten.
SumStore(r) ==
  \* The thread loaded the sum.
  /\ pc[r] = "sumStore"
  \* The loaded sum plus the sample, held at Cap.
  /\ sum' = Sat(seenSum[r], sample[r])
  \* On to the minimum.
  /\ pc' = [pc EXCEPT ![r] = "minLoad"]
  \* Nothing else moves.
  /\ UNCHANGED <<count, mn, mx, sample, seenMin, seenMax, seenSum, staleSkip>>

\* A thread loads the minimum (statistics.rs:444).
MinLoad(r) ==
  \* The thread is past the sum.
  /\ pc[r] = "minLoad"
  \* Remember the minimum seen.
  /\ seenMin' = [seenMin EXCEPT ![r] = mn]
  \* The check comes next.
  /\ pc' = [pc EXCEPT ![r] = "minAct"]
  \* Nothing else moves.
  /\ UNCHANGED <<count, sum, mn, mx, sample, seenMax, seenSum, staleSkip>>

\* Whether a thread lowers the minimum: the code lowers it when the sample is below what was loaded;
\* RED WrongDirection when it is above.
LowersMin(r) ==
  \* The comparison the code makes, or the reversed one the bug makes.
  IF Mutant = "WrongDirection" THEN sample[r] > seenMin[r] ELSE sample[r] < seenMin[r]

\* A thread checks its sample against the minimum it loaded and lowers it (`fetch_min`,
\* statistics.rs:444-446) or skips. RED StoreAfterSkip: it stores the sample instead.
MinAct(r) ==
  \* The thread loaded the minimum.
  /\ pc[r] = "minAct"
  \* Lower it, or skip.
  /\ mn' = IF LowersMin(r)
              \* The bug stores the sample; the code takes the smaller of the two.
              THEN IF Mutant = "StoreAfterSkip" THEN sample[r] ELSE Min2(mn, sample[r])
              \* A skip writes nothing.
              ELSE mn
  \* Ghost: a skip on a loaded minimum that the counter has since left.
  /\ staleSkip' = (staleSkip \/ (~LowersMin(r) /\ seenMin[r] # mn))
  \* On to the maximum.
  /\ pc' = [pc EXCEPT ![r] = "maxLoad"]
  \* Nothing else moves.
  /\ UNCHANGED <<count, sum, mx, sample, seenMin, seenMax, seenSum>>

\* A thread loads the maximum (statistics.rs:447).
MaxLoad(r) ==
  \* The thread is past the minimum.
  /\ pc[r] = "maxLoad"
  \* Remember the maximum seen.
  /\ seenMax' = [seenMax EXCEPT ![r] = mx]
  \* The check comes next.
  /\ pc' = [pc EXCEPT ![r] = "maxAct"]
  \* Nothing else moves.
  /\ UNCHANGED <<count, sum, mn, mx, sample, seenMin, seenSum, staleSkip>>

\* A thread checks its sample against the maximum it loaded and raises it (`fetch_max`,
\* statistics.rs:447-449) or skips. RED MaxStoreAfterSkip: it stores the sample instead.
MaxAct(r) ==
  \* The thread loaded the maximum.
  /\ pc[r] = "maxAct"
  \* Raise it, or skip.
  /\ mx' = IF sample[r] > seenMax[r]
              \* The bug stores the sample; the code takes the larger of the two.
              THEN IF Mutant = "MaxStoreAfterSkip" THEN sample[r] ELSE Max2(mx, sample[r])
              \* A skip writes nothing.
              ELSE mx
  \* Ghost: a skip on a loaded maximum that the counter has since left.
  /\ staleSkip' = (staleSkip \/ (sample[r] <= seenMax[r] /\ seenMax[r] # mx))
  \* The record is done.
  /\ pc' = [pc EXCEPT ![r] = "done"]
  \* Nothing else moves.
  /\ UNCHANGED <<count, sum, mn, sample, seenMin, seenMax, seenSum>>

\* Every step the system can take: one thread takes its next step of `record`.
Next ==
  \* Some thread, at one of its steps.
  \E r \in Recorders : \/ Count(r)
                       \* (the sum, or its RED second half)
                       \/ Sum(r) \/ SumStore(r)
                       \* (the minimum)
                       \/ MinLoad(r) \/ MinAct(r)
                       \* (the maximum)
                       \/ MaxLoad(r) \/ MaxAct(r)

\* Every behaviour: start in Init, then take Next steps or pause.
Spec == Init /\ [][Next]_vars

\* ===========================================================================
\* RULES
\* ===========================================================================

\* Every variable holds the kind of value it should.
TypeOK ==
  \* The counters are numbers in range.
  /\ count \in 0..NRecorders
  \* The sum never passes Cap.
  /\ sum \in 0..Cap
  \* The minimum and the maximum are `u64`s.
  /\ mn \in 0..Cap
  \* (the maximum)
  /\ mx \in 0..Cap
  \* Each sample is one of the values.
  /\ sample \in [Recorders -> Values]
  \* Each thread is somewhere in `record`.
  /\ pc \in [Recorders -> Steps]
  \* What was loaded is a `u64`.
  /\ seenMin \in [Recorders -> 0..Cap]
  \* (the maximum loaded)
  /\ seenMax \in [Recorders -> 0..Cap]
  \* (the sum loaded)
  /\ seenSum \in [Recorders -> 0..Cap]
  \* The ghost is a flag.
  /\ staleSkip \in BOOLEAN

\* Once every thread is done, the count is the number of samples.
CountExact == Quiescent => count = NRecorders

\* Once every thread is done, the sum is the samples' total, held at `u64::MAX`.
SumExact == Quiescent => sum = Min2(Total, Cap)

\* Once every thread is done, the minimum is the smallest of the samples and `u64::MAX`.
MinExact == Quiescent => mn = SetMin({Cap} \cup {sample[r] : r \in Recorders})

\* Once every thread is done, the maximum is the largest of the samples and 0.
MaxExact == Quiescent => mx = SetMax({0} \cup {sample[r] : r \in Recorders})

\* ===========================================================================
\* WITNESS: it must break in a GREEN run, which shows that run reaches the
\* race the rules above are about.
\* ===========================================================================

\* "No thread ever skips on a bound the counter has already left": it breaks in
\* MC_HistogramBounds_Green_ReachesStaleSkip, where the rules still hold.
NeverSkipsStale == ~staleSkip

====
