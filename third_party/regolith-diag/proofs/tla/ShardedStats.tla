---- MODULE ShardedStats ----
\* THE STORY.
\* regolith counts things it does (reads, writes, cache hits) in "tickers".
\* With one counter shared by every core, every core writes the same memory
\* and they slow each other down. So each counter is split into shards, one
\* per thread number: a thread adds to its own shard, and a reader adds the
\* shards up. Threads past the number of shards share one.
\*
\* What can go wrong: two threads on one shard both read 5 and both write
\* 6, so one add is lost; or a reader forgets a shard and reports less than
\* was counted. Tiny example: threads A and B share shard 1, each adds 1
\* once; the total must end at 2, and a reader that started after both
\* adds must report 2.
\*
\* WHAT THIS MODELS (src/statistics.rs, `Statistics::add` and
\* `Statistics::get_ticker`). An add is one atomic `fetch_add` on the
\* calling thread's shard. A read loads the shards one at a time and sums
\* them. Shards only grow (a reset is outside this model).
\*
\* WHAT IS CHECKED, for three threads on two shards (so two share):
\*   QuiescentExact  once every thread is done, the shards sum to exactly
\*                   the adds made;
\*   ReadBetween     a read reports a total between the total when it began
\*                   and the total when it ended;
\*   NeverBackwards  a read never reports less than the read before it.
\*
\* PROVED FOR EVERY SIZE in Lean, proofs/lean/Regolith/ShardedStats.lean:
\*   total_after_adds (QuiescentExact), read_between (ReadBetween),
\*   reads_monotone (NeverBackwards), and lost_update_drops_an_add as the
\*   RED counterexample.
\*
\* Memory ordering: the adds and loads are relaxed in the code. A read of
\* one shard after an earlier read of it never sees an older value (that is
\* per-location coherence, which every atomic has), and that is all the
\* checks above use; each step here is one such access.
\*
\* CONFIGURATIONS.
\*   MC_ShardedStats_Green            every invariant holds
\*   MC_ShardedStats_Red_LostUpdate   an add is a load, then a store:
\*                                    QuiescentExact
\*   MC_ShardedStats_Red_PartialRead  a read sums only its own shard:
\*                                    ReadBetween

\* Natural numbers and sequences of them are all this needs.
EXTENDS Naturals

\* The knobs of a run. Each configuration file (MC_ShardedStats_*.cfg) gives them values.
CONSTANTS
  \* The threads that add, as model values.
  Threads,
  \* How many shards: `Statistics::shards.len()`.
  NShards,
  \* How many adds each thread makes, so the model is finite.
  MaxAdds,
  \* "Atomic" (the code: one fetch_add) or "LoadStore" (RED: read, then
  \* write back one more).
  AddMode,
  \* "AllShards" (the code) or "OwnShard" (RED: the read sums shard 1 only).
  ReadMode

\* At least one shard...
ASSUME NShards \in Nat \ {0}
\* ...and the adds per thread are a natural number.
ASSUME MaxAdds \in Nat
\* Each switch takes one of its two settings: the add...
ASSUME AddMode \in {"Atomic", "LoadStore"}
\* ...and the read.
ASSUME ReadMode \in {"AllShards", "OwnShard"}

\* The shards, numbered from 1.
Shards == 1..NShards

\* The state: everything that changes from one step to the next. Each line says what the variable
\* stands for in the real program.
VARIABLES
  \* Each shard's value: one `AtomicU64` per shard for this ticker.
  shard,
  \* Which shard each thread adds to: `per_thread::index()` masked to the
  \* shard count. Threads may share.
  shardOf,
  \* RED LoadStore only: "idle", or "loaded" between the load and the store.
  pc,
  \* RED LoadStore only: the value a thread loaded.
  tmp,
  \* How many adds each thread has finished.
  done,
  \* Ghost: the adds finished so far, the number the shards must sum to.
  total,
  \* The reader: "idle", "reading" or "reported".
  rphase,
  \* The next shard the reader loads.
  rnext,
  \* What the reader has summed so far.
  rsum,
  \* Ghost: the total when the read began.
  rstart,
  \* Ghost: the total when the read ended.
  rend,
  \* The total the last read reported.
  reported,
  \* The total the read before that reported.
  prev

\* Every variable, so a step that changes none of them is a pause (the list
\* goes on to the reader's variables on the next line).
vars == <<shard, shardOf, pc, tmp, done, total, rphase, rnext, rsum,
          rstart, rend, reported, prev>>

\* The sum of the shards' values: what a read made in one instant would see.
SumShards ==
  \* S[i] is the sum of shards 1..i: nothing for none, else one more shard.
  LET S[i \in 0..NShards] == IF i = 0 THEN 0 ELSE S[i - 1] + shard[i]
  \* All of them.
  IN S[NShards]

\* The start: every shard at 0, threads on any shards, nothing added, no
\* read made.
Init ==
  \* All shards empty.
  /\ shard = [s \in Shards |-> 0]
  \* Any assignment of threads to shards, sharing included.
  /\ shardOf \in [Threads -> Shards]
  \* No add in progress.
  /\ pc = [t \in Threads |-> "idle"]
  \* Nothing loaded.
  /\ tmp = [t \in Threads |-> 0]
  \* No adds done.
  /\ done = [t \in Threads |-> 0]
  \* The true total is 0.
  /\ total = 0
  \* The reader has not started...
  /\ rphase = "idle"
  \* ...would start at shard 1...
  /\ rnext = 1
  \* ...has summed nothing...
  /\ rsum = 0
  \* ...and its ghosts are 0.
  /\ rstart = 0
  \* (the total at the end of a read)
  /\ rend = 0
  \* Nothing reported yet...
  /\ reported = 0
  \* ...and nothing before that.
  /\ prev = 0

\* The code's add: one atomic step on the thread's own shard.
AddAtomic(t) ==
  \* Only in the code's setting...
  /\ AddMode = "Atomic"
  \* ...while the thread has adds to make.
  /\ done[t] < MaxAdds
  \* The shard gains one, all at once.
  /\ shard' = [shard EXCEPT ![shardOf[t]] = @ + 1]
  \* The thread has made one more add...
  /\ done' = [done EXCEPT ![t] = @ + 1]
  \* ...and the true total grows by one.
  /\ total' = total + 1
  \* Nothing else moves.
  /\ UNCHANGED <<shardOf, pc, tmp, rphase, rnext, rsum, rstart, rend, reported, prev>>

\* RED LoadStore, first half: read the shard.
Load(t) ==
  \* Only in the RED setting...
  /\ AddMode = "LoadStore"
  \* ...while the thread has adds to make...
  /\ done[t] < MaxAdds
  \* ...and is not half-way through one.
  /\ pc[t] = "idle"
  \* Remember the value seen.
  /\ tmp' = [tmp EXCEPT ![t] = shard[shardOf[t]]]
  \* Half-way now: the store comes later.
  /\ pc' = [pc EXCEPT ![t] = "loaded"]
  \* Nothing else moves.
  /\ UNCHANGED <<shard, shardOf, done, total, rphase, rnext, rsum, rstart, rend, reported, prev>>

\* RED LoadStore, second half: write back what was read, plus one. Another
\* thread's add to the same shard in between is overwritten.
Store(t) ==
  \* The thread loaded earlier.
  /\ pc[t] = "loaded"
  \* The shard becomes the remembered value plus one.
  /\ shard' = [shard EXCEPT ![shardOf[t]] = tmp[t] + 1]
  \* No longer half-way.
  /\ pc' = [pc EXCEPT ![t] = "idle"]
  \* The thread counts its add as done...
  /\ done' = [done EXCEPT ![t] = @ + 1]
  \* ...and so does the true total.
  /\ total' = total + 1
  \* Nothing else moves.
  /\ UNCHANGED <<shardOf, tmp, rphase, rnext, rsum, rstart, rend, reported, prev>>

\* A read begins (`get_ticker`): it will load shards 1, 2, ... in turn.
StartRead ==
  \* Not in the middle of another read.
  /\ rphase \in {"idle", "reported"}
  \* Reading now...
  /\ rphase' = "reading"
  \* ...from shard 1...
  /\ rnext' = 1
  \* ...with nothing summed yet.
  /\ rsum' = 0
  \* Ghost: the true total now.
  /\ rstart' = total
  \* The previous report becomes "the read before".
  /\ prev' = reported
  \* Nothing else moves.
  /\ UNCHANGED <<shard, shardOf, pc, tmp, done, total, rend, reported>>

\* The read loads one shard and adds it to its sum. RED OwnShard counts
\* only shard 1.
ReadShard ==
  \* A read is under way...
  /\ rphase = "reading"
  \* ...with a shard left to load.
  /\ rnext <= NShards
  \* The shard's value now joins the sum (RED: only shard 1's does).
  /\ rsum' = IF ReadMode = "AllShards" \/ rnext = 1 THEN rsum + shard[rnext] ELSE rsum
  \* On to the next shard.
  /\ rnext' = rnext + 1
  \* Nothing else moves.
  /\ UNCHANGED <<shard, shardOf, pc, tmp, done, total, rphase, rstart, rend, reported, prev>>

\* Every shard is loaded: the read reports its sum.
Report ==
  \* A read is under way...
  /\ rphase = "reading"
  \* ...and every shard is loaded.
  /\ rnext > NShards
  \* It is done...
  /\ rphase' = "reported"
  \* ...and returns its sum.
  /\ reported' = rsum
  \* Ghost: the true total now.
  /\ rend' = total
  \* Nothing else moves.
  /\ UNCHANGED <<shard, shardOf, pc, tmp, done, total, rnext, rsum, rstart, prev>>

\* Every step the system can take, one of:
Next ==
  \* a thread's add (the code's one step, or the RED's two),
  \/ \E t \in Threads : AddAtomic(t) \/ Load(t) \/ Store(t)
  \* a read beginning,
  \/ StartRead
  \* a read loading one shard,
  \/ ReadShard
  \* or a read reporting.
  \/ Report

\* Every behaviour: start in Init, then take Next steps or pause.
Spec == Init /\ [][Next]_vars

----------------------------------------------------------------------------
\* What must always be true.

\* Every variable holds a value of the kind its comment says.
TypeOK ==
  \* Every shard holds a count.
  /\ shard \in [Shards -> Nat]
  \* Every thread has a shard.
  /\ shardOf \in [Threads -> Shards]
  \* Every thread is idle or half-way through a RED add.
  /\ pc \in [Threads -> {"idle", "loaded"}]
  \* No thread makes more adds than its bound.
  /\ done \in [Threads -> 0..MaxAdds]
  \* The reader is in one of its phases.
  /\ rphase \in {"idle", "reading", "reported"}

\* Exact once quiescent: when every thread has made all its adds and none
\* is half-way, the shards sum to the adds made. It rules out a lost add.
\* Lean: total_after_adds.
QuiescentExact ==
  \* Once every thread has made all its adds and is idle, the shards sum to the number of adds made.
  (\A t \in Threads : done[t] = MaxAdds /\ pc[t] = "idle") => SumShards = total

\* A read reports a total between the true total when it began and when it
\* ended. It rules out a read that misses a shard. Lean: read_between.
ReadBetween == rphase = "reported" => rstart <= reported /\ reported <= rend

\* A read never reports less than the read before it. Lean: reads_monotone.
NeverBackwards == rphase = "reported" => prev <= reported

====
