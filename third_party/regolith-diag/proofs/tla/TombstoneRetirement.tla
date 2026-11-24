---- MODULE TombstoneRetirement ----
\* THE STORY, for a reader who has never seen the code.
\* The database never erases in place. Deleting a range of keys writes a
\* marker, a "range tombstone", that says "every older version of these keys
\* is gone". The data sits in sorted files ("runs") that a background job,
\* compaction, keeps merging. A tombstone that is carried along for ever
\* costs every later merge, so compaction wants to throw it away ("retire"
\* it) once it has done its job. Readers may hold "snapshots": a promise to
\* keep seeing the database as it was at one moment.
\*
\* Tiny example of what can go wrong. Key 1 has value A in an old, deep run.
\* A tombstone over key 1 is written; reads now say "absent". Compaction
\* merges the tombstone's run with its neighbour and throws the tombstone
\* away, but A still sits in the deeper run that was not part of the merge.
\* The next read finds A again: deleted data came back. The model shows the
\* rule "retire only when nothing deeper can hold a covered key and no
\* snapshot is older than the tombstone" never lets that happen, and the RED
\* configurations show it happening when either half of the rule is dropped.
\*
\* E27: when compaction may drop a range tombstone. Dropped column families
\* leave range tombstones, and a tombstone a pass carries forever makes every
\* later pass over its range pay for it.
\*
\* PROVED FOR EVERY SIZE in Lean, proofs/lean/Regolith/TombstoneRetirement.lean:
\*   retire_keeps_reads             at a live snapshot or the head, the same
\*                                  put wins every key's read before and after
\*                                  a pass that retires by the fix's rule
\*                                  (SnapshotReadsKept and HeadKept here, for
\*                                  any keys, writes and snapshots)
\*   retire_keeps_read_value        so every read returns the same value
\*   ignore_snapshots_breaks_reads, ignore_deeper_breaks_reads
\*                                  the RED cases, as counterexamples
\* The Lean proof tests a deeper run entry by entry rather than by its span,
\* a weaker condition than the model's, so it covers the model's rule too.
\*
\* THE ENGINE.
\*   Retirement::carried_tombstones     src/engine/compaction/retire.rs
\*     The range tombstones a pass writes out: all of its inputs' tombstones
\*     but those it retires. One retires when the pass writes the deepest
\*     run holding a key of its range and no live snapshot is below its
\*     sequence. A run here is a level below L0, or one table of L0: L0 is
\*     in age order and read newest first, so a pass that writes L0 (a
\*     universal merge, a table rewritten in place) has the older L0 tables
\*     it leaves out below it, like the levels (Retirement::new, `older`).
\*   run_level_deletion_pass,           src/engine/compaction.rs
\*   run_universal_deletion_merge
\*     Passes picked for their deletions (D14). They retire by the rule
\*     above and no other. One of them is new in shape: a single table
\*     rewritten where it is, at the bottom level or as the only run of a
\*     universal pool. Rewrite below is that pass.
\*   Retirement::retire_deletion        src/engine/compaction/retire.rs
\*     The same rule for a point deletion, which is a range tombstone over
\*     its one key here, and which the engine retires only as the oldest
\*     entry of its key with no merge operand on it.
\*   Stripes::shadowed                  src/engine/compaction/stripes.rs
\*     The pass drops every entry a tombstone of its inputs covers in the
\*     entry's own snapshot stripe, retired tombstones included.
\*
\* THE DEFECTS, one per mutant value of Retire.
\*   "IgnoreSnapshots": retire at the bottom whatever the snapshots. A
\*     snapshot below the tombstone keeps a covered version alive in an older
\*     stripe; with the tombstone gone a snapshot above it reads that version
\*     back.
\*   "IgnoreDeeper": retire with the snapshots clear but a deeper run still
\*     holding a covered version. A snapshot above the tombstone reads it
\*     back.
\*
\* THE FIX. Retire a tombstone only when no deeper run meets its range and
\* every live snapshot is at or above its sequence. Every version it covers
\* is then in the pass's inputs, in its stripe, and dropped with it.
\*
\* WHAT THE MODEL LEAVES OUT, and why that loses nothing.
\*   - The read order across sources: a read merges every entry of the key by
\*     sequence, which is what the engine's read order returns on an ordered
\*     tree (LsmOrder.tla, Lean read_newest). A version older than a
\*     tombstone is never above it in that order: both enter at the top and
\*     a pass merges adjacent runs.
\*   - Point deletions and merge operands, and dropping a put under a newer
\*     put: StripeCompaction.tla checks those reductions. Here a pass drops
\*     only what a tombstone covers, which is all a retirement relies on.
\*   - Which pass runs, and when: the size triggers, the deletion rule and
\*     its turns only choose among passes. Here any pass may run at any
\*     step, so every choice the engine can make is checked.
\*   - Merges of more than two runs at once (a universal merge may take
\*     several): the Lean proof takes any window of entries, newer ones
\*     above and older ones below, and TLC shows two runs and one.
\*   - Several concurrent passes and the capture window (E5): the passes'
\*     inputs are disjoint, and the engine reads the live list once the
\*     inputs are fixed (StripeCompaction.tla); here a pass is one step.
\*   - A run is one table: the engine tests each deeper table's range, the
\*     model each deeper run's span, which is the same test on one table.
\*
\* CONFIGURATIONS.
\*   MC_TombstoneRetirement_Green                 the fix: reads kept
\*   MC_TombstoneRetirement_Green_Retires         the fix retires: the
\*                                                witness NothingRetired fails
\*   MC_TombstoneRetirement_Red_IgnoreSnapshots   SnapshotReadsKept fails
\*   MC_TombstoneRetirement_Red_IgnoreDeeper      SnapshotReadsKept fails
\*   MC_TombstoneRetirement_Green_Rewrite         merges and one-run passes
\*                                                together: reads kept
\*   MC_TombstoneRetirement_Green_RewriteRetires  a one-run pass alone
\*                                                retires: NothingRetired fails
\*   MC_TombstoneRetirement_Red_RewriteIgnoreSnapshots
\*                                                one-run passes only:
\*                                                SnapshotReadsKept fails
\*   MC_TombstoneRetirement_Red_RewriteIgnoreDeeper
\*                                                one-run passes only:
\*                                                SnapshotReadsKept fails

\* Borrow the standard definitions of numbers, sequences (ordered lists) and finite sets, and TLC's
\* Permutations.
EXTENDS Naturals, Sequences, FiniteSets, TLC

\* The knobs of a run. Each configuration file (MC_TombstoneRetirement_*.cfg) gives them values.
CONSTANTS
  \* The user keys: a set of numbers.
  Keys,
  \* The last sequence number a write may take. It bounds how many writes a run has.
  MaxSeq,
  \* The most runs (levels of the tree) the model lets exist at once.
  MaxRuns,
  \* The readers: a set of names. Each registers one snapshot, at most once.
  Readers,
  \* Which retirement rule compaction uses: "Fix" is the design; "IgnoreSnapshots" and
  \* "IgnoreDeeper" are the two planted bugs.
  Retire,
  \* Which shapes of pass may run: "Merge" is two adjacent runs into the deeper one; "Rewrite" is
  \* one run rewritten where it is; "Both" allows either at every step.
  Passes

\* The keys are numbers, and there is at least one.
ASSUME Keys \subseteq Nat /\ Keys # {}
\* The bounds are numbers, and at least one run is allowed.
ASSUME MaxSeq \in Nat /\ MaxRuns \in Nat \ {0}
\* TLC refuses a configuration that names no known rule.
ASSUME Retire \in {"Fix", "IgnoreSnapshots", "IgnoreDeeper"}
\* TLC refuses a configuration that names no known shape of pass.
ASSUME Passes \in {"Merge", "Rewrite", "Both"}

\* What a read of a key with no visible put returns. Puts read as their
\* sequence, so every value is at least 1.
Absent == 0

\* A sequence above every sequence a write takes: the head reads at it.
Infinity == MaxSeq + 1

\* A put of key k at sequence s: <<"put", k, k, s>>. A range tombstone over
\* the keys lo..hi at sequence s: <<"rt", lo, hi, s>>. The second and third
\* fields are the keys the entry holds or covers.
Put(k, s) == <<"put", k, k, s>>
\* A range tombstone as a tuple: its kind, the first and last key it covers, and its sequence.
Rt(lo, hi, s) == <<"rt", lo, hi, s>>

\* The fields of an entry e.
Kind(e) == e[1]
\* The first key an entry holds or covers.
Lo(e) == e[2]
\* The last key an entry holds or covers.
Hi(e) == e[3]
\* The sequence number an entry was written at.
SeqOf(e) == e[4]

\* The state: everything that changes from one step to the next. Each line says what the variable
\* stands for in the real program.
VARIABLES
  \* The last sequence number handed out, 0 before the first write: the engine's commit counter. A
  \* number.
  seq,
  \* The memtable, the in-memory table that takes new writes (src/engine/memtable.rs): a set of
  \* entries.
  mem,
  \* The runs of the tree, NEWEST FIRST: a sequence of sets of entries. Each stands for one level's
  \* table on disk. A pass merges runs i and i + 1 into the deeper position; runs after i + 1 are
  \* deeper still.
  runs,
  \* A ghost (bookkeeping the program does not have): every entry ever written, never compacted. It
  \* is the ground truth a read is compared with.
  hist,
  \* For each reader, TRUE while it holds its snapshot (SnapshotRegistry,
  \* src/engine/snapshot_registry.rs).
  live,
  \* The readers that have registered a snapshot: a set.
  used,
  \* For each reader, the sequence its snapshot reads at. A number.
  snap,
  \* A ghost: for each reader and key, what the snapshot read when it was taken.
  seen,
  \* A ghost: how many tombstones compaction passes have retired. A number.
  retired

\* Every variable, so a step that changes none of them is a stutter.
vars == <<seq, mem, runs, hist, live, used, snap, seen, retired>>

\* The greatest element of a finite, nonempty set of naturals.
Max(S) == CHOOSE m \in S : \A n \in S : n <= m
\* The least element of a finite, nonempty set of naturals.
Min(S) == CHOOSE m \in S : \A n \in S : m <= n

\* Every entry the tree holds: the memtable and every run.
Tree == mem \cup UNION {runs[i] : i \in 1..Len(runs)}

\* A read of key k at snapshot s over the entries E: the newest put of k at
\* or below s, unless a tombstone at or below s covering k is newer than it.
ReadIn(E, k, s) ==
  \* The sequences of the puts of k that the snapshot can see.
  LET puts == {SeqOf(e) : e \in {x \in E : Kind(x) = "put" /\ Lo(x) = k /\ SeqOf(x) <= s}}
      \* The sequences of the tombstones that cover k and that the snapshot can see.
      rts  == {SeqOf(e) : e \in {x \in E : Kind(x) = "rt" /\ Lo(x) <= k /\ k <= Hi(x) /\ SeqOf(x) <= s}}
  \* No visible put: the key reads as absent.
  IN IF puts = {} THEN Absent
     \* Otherwise take the newest put p; a tombstone newer than p hides it, else the read returns p.
     ELSE LET p == Max(puts) IN IF \E t \in rts : t > p THEN Absent ELSE p

\* The sequences of the live snapshots.
LiveSeqs == {snap[r] : r \in {x \in Readers : live[x]}}

\* The upper bound of the stripe sequence s falls in: the smallest live
\* snapshot at or above it, or Infinity for the top stripe.
Top(s) == LET above == {l \in LiveSeqs : l >= s} IN IF above = {} THEN Infinity ELSE Min(above)

\* Stripes::shadowed: entry e of the window W is a put that a tombstone of W
\* covers in e's own stripe.
Shadowed(W, e) ==
  \* e is a put,
  /\ Kind(e) = "put"
  \* and some tombstone t of the window covers e's key,
  /\ \E t \in W : Kind(t) = "rt" /\ Lo(t) <= Lo(e) /\ Lo(e) <= Hi(t)
                  \* is newer than e, and sits in e's own stripe (no live snapshot separates them).
                  /\ SeqOf(e) < SeqOf(t) /\ SeqOf(t) <= Top(SeqOf(e))

\* The keys of run R span from its least to its greatest key, the range a
\* table records; that span meets lo..hi.
RunMeets(R, lo, hi) ==
  \* The run holds something,
  /\ R # {}
  \* its least key is at or below hi,
  /\ Min({Lo(e) : e \in R}) <= hi
  \* and its greatest key is at or above lo.
  /\ lo <= Max({Hi(e) : e \in R})

\* A pass writing position i + 1 retires tombstone t: in the fix, no run
\* deeper than i + 1 meets t's range and no live snapshot is below t.
\* Each mutant drops one conjunct.
Retires(i, t) ==
  \* No run deeper than the pass's output meets the tombstone's range.
  LET clearBelow == \A j \in (i + 2)..Len(runs) : ~RunMeets(runs[j], Lo(t), Hi(t))
      \* No live snapshot is below the tombstone's sequence.
      clearSnaps == \A l \in LiveSeqs : SeqOf(t) <= l
  \* The design asks for both.
  IN CASE Retire = "Fix"             -> clearBelow /\ clearSnaps
       \* The bug IgnoreSnapshots asks only for the first.
       [] Retire = "IgnoreSnapshots" -> clearBelow
       \* The bug IgnoreDeeper asks only for the second.
       [] OTHER                      -> clearSnaps

\* The elements of T at positions 1.. not in S, in order.
RECURSIVE Keep(_, _, _)
\* Walk the sequence T from position i.
Keep(T, S, i) ==
  \* Past the end: nothing. Else keep T[i] unless i is in S, and go on with i + 1.
  IF i > Len(T) THEN <<>> ELSE (IF i \in S THEN <<>> ELSE <<T[i]>>) \o Keep(T, S, i + 1)

----------------------------------------------------------------------------
\* Actions.

\* The start: no write, empty memtable and runs, no snapshot.
Init ==
  \* No write yet.
  /\ seq     = 0
  \* The memtable is empty.
  /\ mem     = {}
  \* No runs.
  /\ runs    = <<>>
  \* Nothing was ever written.
  /\ hist    = {}
  \* No reader holds a snapshot.
  /\ live    = [r \in Readers |-> FALSE]
  \* No reader has registered.
  /\ used    = {}
  \* Every snapshot sequence is 0, unused.
  /\ snap    = [r \in Readers |-> 0]
  \* Nothing was read.
  /\ seen    = [r \in Readers |-> [k \in Keys |-> Absent]]
  \* Nothing was retired.
  /\ retired = 0

\* A put of key k at the next sequence.
DoPut(k) ==
  \* When: a sequence number is left.
  /\ seq < MaxSeq
  \* The next sequence number is taken.
  /\ seq' = seq + 1
  \* The put goes into the memtable,
  /\ mem' = mem \cup {Put(k, seq + 1)}
  \* and into the history.
  /\ hist' = hist \cup {Put(k, seq + 1)}
  \* Nothing else changes.
  /\ UNCHANGED <<runs, live, used, snap, seen, retired>>

\* A range delete of the keys lo..hi at the next sequence: what
\* drop_column_family writes over its family's keys.
DoRangeDelete(lo, hi) ==
  \* When: a sequence number is left
  /\ seq < MaxSeq
  \* and the range is not empty.
  /\ lo <= hi
  \* The next sequence number is taken.
  /\ seq' = seq + 1
  \* The tombstone goes into the memtable,
  /\ mem' = mem \cup {Rt(lo, hi, seq + 1)}
  \* and into the history.
  /\ hist' = hist \cup {Rt(lo, hi, seq + 1)}
  \* Nothing else changes.
  /\ UNCHANGED <<runs, live, used, snap, seen, retired>>

\* A flush turns the memtable into the newest run.
Flush ==
  \* When: the memtable holds something
  /\ mem # {}
  \* and one more run is allowed.
  /\ Len(runs) < MaxRuns
  \* The memtable's entries become the newest run.
  /\ runs' = <<mem>> \o runs
  \* The memtable is empty again.
  /\ mem'  = {}
  \* Nothing else changes.
  /\ UNCHANGED <<seq, hist, live, used, snap, seen, retired>>

\* A pass merges run i into run i + 1: it drops what a tombstone of its
\* window shadows and writes out every tombstone it does not retire. An
\* output with nothing left in it is no run at all.
Compact(i) ==
  \* When: this configuration lets a pass merge two runs,
  /\ Passes \in {"Merge", "Both"}
  \* and runs i and i + 1 both exist.
  /\ i + 1 <= Len(runs)
  \* W is the window: every entry of the two runs.
  /\ LET W    == runs[i] \cup runs[i + 1]
         \* The tombstones of the window that the rule retires.
         gone == {t \in W : Kind(t) = "rt" /\ Retires(i, t)}
         \* The output: every entry no tombstone shadows, without the retired tombstones.
         out  == {e \in W : ~Shadowed(W, e)} \ gone
     \* The output replaces run i + 1 and run i disappears; an empty output removes both.
     IN /\ runs'    = Keep([runs EXCEPT ![i + 1] = out],
                           IF out = {} THEN {i, i + 1} ELSE {i}, 1)
        \* The retired tombstones are counted.
        /\ retired' = retired + Cardinality(gone)
  \* Nothing else changes.
  /\ UNCHANGED <<seq, mem, hist, live, used, snap, seen>>

\* A pass rewrites run i where it is, with no other run in it: what a pass
\* picked for its deletions does to a table of the bottom level, and to the
\* only run of a universal pool. It drops what a tombstone of the run
\* shadows in the run itself, and writes out every tombstone it does not
\* retire. Everything after run i is deeper than its output.
Rewrite(i) ==
  \* When: this configuration lets a pass rewrite one run,
  /\ Passes \in {"Rewrite", "Both"}
  \* and run i exists.
  /\ i <= Len(runs)
  \* W is the window: every entry of the one run.
  /\ LET W    == runs[i]
         \* The tombstones of the run that the rule retires. A pass writing position i has the
         \* runs from i + 1 on below it, which is what Retires asks of a pass writing (i - 1) + 1.
         gone == {t \in W : Kind(t) = "rt" /\ Retires(i - 1, t)}
         \* The output: every entry no tombstone shadows, without the retired tombstones.
         out  == {e \in W : ~Shadowed(W, e)} \ gone
     \* The output replaces run i; an empty output removes the run.
     IN /\ runs'    = Keep([runs EXCEPT ![i] = out], IF out = {} THEN {i} ELSE {}, 1)
        \* The retired tombstones are counted.
        /\ retired' = retired + Cardinality(gone)
  \* Nothing else changes.
  /\ UNCHANGED <<seq, mem, hist, live, used, snap, seen>>

\* Reader r registers a snapshot at the newest sequence and records what it
\* reads for every key.
TakeSnapshot(r) ==
  \* When: reader r has not registered before.
  /\ r \notin used
  \* It has now.
  /\ used' = used \cup {r}
  \* It holds its snapshot.
  /\ live' = [live EXCEPT ![r] = TRUE]
  \* The snapshot reads at the newest sequence.
  /\ snap' = [snap EXCEPT ![r] = seq]
  \* What it reads for every key, right now, is recorded.
  /\ seen' = [seen EXCEPT ![r] = [k \in Keys |-> ReadIn(Tree, k, seq)]]
  \* Nothing else changes.
  /\ UNCHANGED <<seq, mem, runs, hist, retired>>

\* Reader r releases its snapshot.
Release(r) ==
  \* When: reader r holds its snapshot.
  /\ live[r]
  \* It lets go.
  /\ live' = [live EXCEPT ![r] = FALSE]
  \* Nothing else changes.
  /\ UNCHANGED <<seq, mem, runs, hist, used, snap, seen, retired>>

\* Every step the system can take.
Next ==
  \* A put,
  \/ \E k \in Keys : DoPut(k)
  \* or a range delete,
  \/ \E lo, hi \in Keys : DoRangeDelete(lo, hi)
  \* or a flush,
  \/ Flush
  \* or a compaction pass over two adjacent runs,
  \/ \E i \in 1..MaxRuns : Compact(i)
  \* or a compaction pass over one run alone,
  \/ \E i \in 1..MaxRuns : Rewrite(i)
  \* or a reader takes or releases a snapshot.
  \/ \E r \in Readers : TakeSnapshot(r) \/ Release(r)

\* Every behaviour: start in Init, then take Next steps or stutter.
Spec == Init /\ [][Next]_vars

\* Readers are interchangeable.
ReaderSymmetry == Permutations(Readers)

----------------------------------------------------------------------------
\* Invariants.

\* Every variable holds what its comment says.
TypeOK ==
  \* The sequence stays within its bound.
  /\ seq \in 0..MaxSeq
  \* Never more runs than allowed.
  /\ Len(runs) <= MaxRuns
  \* Each reader holds its snapshot or does not.
  /\ live \in [Readers -> BOOLEAN]
  \* Only readers register.
  /\ used \subseteq Readers
  \* Every snapshot sequence is within the bound.
  /\ snap \in [Readers -> 0..MaxSeq]
  \* The count of retired tombstones is a number.
  /\ retired \in Nat

\* THE HEADLINE. Every live snapshot reads every key as it did when taken.
\* Example ruled out: a snapshot taken after a range delete read key 1 as
\* absent; a pass retires the tombstone while a covered put of key 1 is still
\* in a deeper run, and the same snapshot now reads that put. The program
\* needs this because a snapshot is a promise that reads do not change.
SnapshotReadsKept ==
  \* For every reader that holds its snapshot and every key: reading the tree at the snapshot gives
  \* what it gave when the snapshot was taken.
  \A r \in Readers : live[r] => \A k \in Keys : ReadIn(Tree, k, snap[r]) = seen[r][k]

\* A read at the head returns what the whole history says.
\* Example ruled out: the newest state says key 1 is deleted, yet a read
\* without a snapshot returns an old value that compaction uncovered.
HeadKept == \A k \in Keys : ReadIn(Tree, k, Infinity) = ReadIn(hist, k, Infinity)

\* The witness: nothing was ever retired. The fix must break it, or it
\* retires nothing and keeps the reads by never trying.
NothingRetired == retired = 0

====
