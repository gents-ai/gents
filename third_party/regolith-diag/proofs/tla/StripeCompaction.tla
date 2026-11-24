---- MODULE StripeCompaction ----
\* ===========================================================================
\* THE STORY
\* ===========================================================================
\* The database never overwrites a value in place. Every write adds a new
\* version of its key, stamped with a growing sequence number. Old versions
\* pile up, so a background job, compaction, rewrites the files and throws
\* away versions nobody can read any more.
\*
\* A snapshot is a reader's frozen picture: "show me the database as of
\* sequence 5". As long as that reader holds its snapshot, the version each
\* key had at sequence 5 must stay, even when newer versions exist.
\*
\* Tiny example. A key is written at sequences 3, 6 and 9. A reader holds a
\* snapshot at 5, so it reads the version of sequence 3. If compaction kept
\* only "the newest version" (9), that reader would suddenly read something
\* else, or nothing. So the live snapshots cut the sequence line into
\* "stripes" (here: up to 5, and above 5), and compaction keeps the newest
\* state of every stripe: version 3 for the reader, version 9 for everyone
\* newer; version 6 is the only one nobody can see, and it goes.
\*
\* What can go wrong, each a RED configuration below:
\*   - compaction asks "which snapshots are live?" too early, a reader takes
\*     a snapshot just after, and its version is thrown away (E5);
\*   - compaction squashes merge operands ("+1", "+1") with a rule that
\*     rounds, and the key reads differently afterwards;
\*   - compaction ignores the stripes altogether.
\*
\* What this model shows: with the engine's rules, every live snapshot and
\* every read without a snapshot returns the same value before and after any
\* flush and any compaction, in every interleaving with writes and with
\* snapshots being taken and released.
\*
\* The rules (plan 4.8): every key's versions are reduced per snapshot
\* stripe, merge operands are folded only by an exact `partial_merge`, the
\* compaction filter runs on every stripe, and the live-snapshot list is read
\* only after the inputs are fixed (E5).
\*
\* PROVED FOR EVERY SIZE in Lean, proofs/lean/Regolith/Stripes.lean:
\*   combine_reads              one fold of two neighbours of one stripe
\*                              changes no read at a stripe boundary
\*   reduce_reads               the whole reduction changes no read at a
\*                              live snapshot or at the head, on top of any
\*                              base the older versions resolve to
\*   window_reads_kept          so a compaction of any window of a key's
\*                              versions, newer ones above and older below,
\*                              changes no such read
\*   reduce_keeps_newest        the newest version's sequence is kept
\*   inexact_fold_breaks_reads  the RED Inexact case, as a counterexample
\*   unlisted_snapshot_breaks_reads
\*                              the RED CaptureEarly and IgnoreStripes cases:
\*                              a snapshot missing from the list loses the
\*                              version it reads
\* TLC checks the same laws here over every interleaving of writes,
\* flushes, snapshot registrations and releases, and one compaction at a
\* time whose steps interleave with all of them.
\*
\* THE ENGINE.
\*   Stripes::reduce_group, reduce_stripe, fold
\*                                  src/engine/compaction/stripes.rs
\*     The live snapshots cut the sequence line into stripes. An entry at
\*     sequence s belongs to the stripe bounded by the smallest live
\*     snapshot at or above s, or to the top stripe. Inside a stripe only
\*     the newest state is visible to any reader, so the entries beneath
\*     its terminator (the newest value or deletion) are dropped, the
\*     operands over the terminator fold into it with `full_merge`, and an
\*     operand-only stripe folds with `partial_merge` when the operator has
\*     an exact one. The filter judges the value that ends each stripe.
\*   SnapshotRegistry::live_seqs     src/engine/snapshot_registry.rs
\*     The list the stripes are cut at. SnapshotRegistry.tla shows the
\*     lock-free registry returns every live snapshot or one at or above
\*     the horizon sampled before the scan.
\*
\* THE DEFECTS.
\*   Capture = "BeforeInputs" (E5): the list is read before the inputs are
\*     fixed. A snapshot registered in between is missing from the list,
\*     yet the inputs hold versions newer than it (a flush landed in the
\*     window), so its version is dropped beneath a newer one.
\*   FoldOperandOnly = "Always" with Operator = "Inexact": an operand-only
\*     stripe is folded with a `partial_merge` that does not agree with
\*     applying the operands one at a time (a floating-point sum), so the
\*     value read through it changes.
\*   Stripes = "Ignore": the reduction treats the whole key as one stripe,
\*     as if no snapshot were live.
\*
\* THE FIX. Fix the inputs, then read the list. A snapshot registered after
\* that reads at the visible sequence, which is at or above every version
\* in the inputs, so it reads each key's top stripe, whose newest state is
\* always kept. Fold an operand-only stripe only with an exact
\* `partial_merge`; with an inexact operator such a stripe stays operands
\* until a base lands in the same stripe.
\*
\* FILTERS. A filter that changes or removes a value changes what a live
\* snapshot reads, by design (plan 3.1: expiry applies under snapshots). So
\* the expected reads move with the filter's decisions: when a compaction
\* installs, each live reader's expected value becomes what it reads in the
\* tree just before the install with the filter applied in place to every
\* stripe's terminator in the inputs. The invariant then says the
\* reduction itself (drops and folds) changes nothing beyond that. With
\* Filter = "None" the expectations never move, and the invariant is the
\* plain "every live snapshot reads the same value before and after".
\*
\* DESIGN CHOICES where the plan leaves room (recorded in the report):
\*   - A compaction's inputs are any window of consecutive runs. In an LSM
\*     ordered as LsmOrder.tla proves, the inputs of any compaction hold a
\*     consecutive stretch of each key's versions: newer ones may sit above
\*     the window (memtable, newer runs) and older ones below it, so both a
\*     stripe cut at the window's top and an operand-only stripe whose base
\*     lies below the window occur.
\*   - The filter runs on each stripe's terminator before the operands
\*     over it fold, as Stripes::reduce_stripe does.
\*
\* WHAT THE MODEL LEAVES OUT, and why that loses nothing.
\*   - The read order across memtable and runs: a read merges every entry
\*     of the key by sequence, which is what the engine's read order returns
\*     on an ordered tree (LsmOrder.tla, Lean read_newest).
\*   - Range tombstones (`Stripes::shadowed`): they act as deletions with a
\*     sequence and follow the same stripe rule. When a pass may drop the
\*     tombstone itself (E27) is TombstoneRetirement.tla.
\*   - Several concurrent compactions: their windows are disjoint and each
\*     reads the list after fixing its own inputs, so each is the case here.
\*   - The configurations use one key: the reduction treats each key on its
\*     own (StripeSets splits by key), so a second key only multiplies the
\*     states. Each reader registers once; a later registration is another
\*     reader.
\*
\* CONFIGURATIONS.
\*   MC_StripeCompaction_Green                Exact operator, no filter
\*   MC_StripeCompaction_Green_Filter         Exact operator, Expire filter
\*   MC_StripeCompaction_Green_Inexact        Inexact operator, never folds
\*                                            an operand-only stripe
\*   MC_StripeCompaction_Red_CaptureEarly     SnapshotReadsKept fails (E5)
\*   MC_StripeCompaction_Red_InexactFold      HeadKept fails
\*   MC_StripeCompaction_Red_IgnoreStripes    SnapshotReadsKept fails

\* Borrow the standard TLA+ books: whole numbers (a missing value is -1), lists, sizes of finite
\* sets, and TLC's Permutations.
EXTENDS Integers, Sequences, FiniteSets, TLC

\* The fixed inputs a configuration chooses.
CONSTANTS
  \* The user keys, as numbers.
  Keys,
  \* The last sequence number a write may take. It bounds how many writes a run of the model makes.
  MaxSeq,
  \* The largest number of runs (sorted files on disk) the model lets exist at once.
  MaxRuns,
  \* The readers, as names that can be swapped for one another. Each holds at most one snapshot.
  Readers,
  \* The values a put may store.
  PutValues,
  \* The largest value a merge result may reach (results are capped there, to keep the model
  \* finite).
  Cap,
  \* "Exact" or "Inexact": whether the merge operator's partial_merge means the same as applying the
  \* operands one at a time (MergeOperator::partial_merge, src/options.rs).
  Operator,
  \* When a stripe holding only operands is folded: "IfExact" (the engine: only when the operator
  \* offers a partial_merge) or "Always" (the planted bug).
  FoldOperandOnly,
  \* When the compaction reads the list of live snapshots: "AfterInputs" (the engine) or
  \* "BeforeInputs" (the planted bug, E5).
  Capture,
  \* How the reduction cuts a key's versions: "PerSnapshot" (the engine) or "Ignore" (the planted
  \* bug: as if no snapshot were live).
  Stripes,
  \* The compaction filter: "None", or "Expire", which removes the value 0 and changes the value Cap
  \* to Cap - 1 (CompactionFilter, src/options.rs).
  Filter

\* There is at least one key, and keys are numbers.
ASSUME Keys \subseteq Nat /\ Keys # {}
\* The bounds are numbers; at least one run may exist and the cap is at least 1.
ASSUME MaxSeq \in Nat /\ MaxRuns \in Nat \ {0} /\ Cap \in Nat \ {0}
\* A put stores a value between 0 and the cap.
ASSUME PutValues \subseteq 0..Cap
\* The operator is one of the two kinds.
ASSUME Operator \in {"Exact", "Inexact"}
\* The folding rule is one of the two.
ASSUME FoldOperandOnly \in {"IfExact", "Always"}
\* The moment the list is read is one of the two.
ASSUME Capture \in {"AfterInputs", "BeforeInputs"}
\* The cutting rule is one of the two.
ASSUME Stripes \in {"PerSnapshot", "Ignore"}
\* The filter is one of the two.
ASSUME Filter \in {"None", "Expire"}

\* What a read of a key with no visible state returns. Values are >= 0.
Absent == -1

\* A sequence above every sequence a write takes: the top stripe's bound,
\* and the snapshot the head reads at.
Infinity == MaxSeq + 1

\* The state: everything that changes from one step to the next. Each line says what the variable
\* stands for in the real program.
VARIABLES
  \* The last sequence number handed out, 0 before the first write (the engine's commit sequence).
  seq,
  \* The memtable: the newest writes, still in memory, as a set of entries <<key, sequence, kind,
  \* value>> (MemTable, src/engine/memtable.rs).
  mem,
  \* The runs: sorted files on disk, OLDEST FIRST, each a set of entries (the tables of a Version,
  \* src/engine/manifest.rs). A flush adds at the end, so the positions a compaction picked stay
  \* valid.
  runs,
  \* For each reader, whether it holds a snapshot right now (a pin in the SnapshotRegistry,
  \* src/engine/snapshot_registry.rs).
  live,
  \* The readers that have ever registered. Each registers once, which keeps the model finite and
  \* loses nothing: a later registration is just another reader.
  used,
  \* The sequence each reader's snapshot reads at.
  snap,
  \* What each live snapshot must read for each key. Model bookkeeping: it is written down when the
  \* snapshot is taken and compared against ever after.
  seen,
  \* What a read with no snapshot (at the head) must return for each key. Model bookkeeping.
  headSeen,
  \* The sequence of each key's newest write, 0 for none. Model bookkeeping for the check that
  \* compaction keeps it.
  last,
  \* How far the one compaction has got: "idle", "fixed" (inputs chosen), "sampled" (list read) or
  \* "ready" (both done).
  cphase,
  \* The positions <<lo, hi>> of the runs the compaction takes as inputs, once chosen.
  win,
  \* The live-snapshot sequences the compaction read (SnapshotRegistry::live_seqs), once read.
  list

\* Every variable, so a step that changes none of them is a stutter.
vars == <<seq, mem, runs, live, used, snap, seen, headSeen, last, cphase, win, list>>

----------------------------------------------------------------------------
\* Helpers.

\* The smaller of two integers.
Min2(a, b) == IF a <= b THEN a ELSE b

\* The least element of a finite, nonempty set of integers.
Min(S) == CHOOSE m \in S : \A n \in S : m <= n

\* The greatest element of a finite, nonempty set of integers.
Max(S) == CHOOSE m \in S : \A n \in S : n <= m

\* The entry with the greatest sequence in a nonempty set of entries.
\* Sequences are unique per entry, so it is well defined.
Newest(S) == CHOOSE e \in S : \A f \in S : f[2] <= e[2]

\* The entry with the least sequence in a nonempty set of entries.
Oldest(S) == CHOOSE e \in S : \A f \in S : e[2] <= f[2]

\* Every entry the tree holds: the memtable and every run.
Tree == mem \cup UNION {runs[i] : i \in 1..Len(runs)}

----------------------------------------------------------------------------
\* The merge operator. A merge operand is a natural added to the value
\* beneath it; a missing base counts as 0.

\* full_merge one operand at a time: apply operand x to `base`. The exact
\* operator is a saturating sum. The inexact one rounds after every
\* addition, as a floating-point sum does: adding 1 to an even total rounds
\* it back down, so small increments applied one at a time are lost.
Apply(base, x) ==
  \* b: the value the operand is added to; a missing base counts as 0.
  LET b == IF base = Absent THEN 0 ELSE base
  \* Exact: the sum, capped. Inexact: the sum rounded down to an even number, capped.
  IN IF Operator = "Exact" THEN Min2(b + x, Cap) ELSE Min2(2 * ((b + x) \div 2), Cap)

\* partial_merge of two operands, older x then newer y: their sum. For the
\* exact operator, applying it equals applying x then y, for every base.
\* For the inexact one it does not: Apply(0, 1) twice is 0, Apply(0, 2) is 2.
PM(x, y) == Min2(x + y, Cap)

\* full_merge of a set of operands onto `acc`, oldest first.
RECURSIVE FoldOps(_, _)
\* Apply the operands of S to acc, one at a time, oldest first.
FoldOps(acc, S) ==
  \* None left: acc is the answer. Otherwise apply the oldest and go on with the rest.
  IF S = {} THEN acc ELSE LET e == Oldest(S) IN FoldOps(Apply(acc, e[4]), S \ {e})

\* partial_merge folded over a set of operands, oldest first, onto `acc`.
RECURSIVE PMRest(_, _)
\* Combine the operands of S into acc with partial_merge, oldest first.
PMRest(acc, S) ==
  \* None left: acc is the answer. Otherwise combine the oldest in and go on with the rest.
  IF S = {} THEN acc ELSE LET e == Oldest(S) IN PMRest(PM(acc, e[4]), S \ {e})

\* One operand standing for a nonempty set of operands: partial_merge
\* folded from the oldest.
PMFold(S) == LET o == Oldest(S) IN PMRest(o[4], S \ {o})

----------------------------------------------------------------------------
\* Reads. All sets of entries below hold the entries of one key.

\* The terminators: values and deletions.
Terms(S) == {e \in S : e[3] # "merge"}

\* The operands over the newest terminator, or every operand when there is
\* no terminator. These are what a read applies.
Leading(S) ==
  \* No value or deletion at all: every operand.
  IF Terms(S) = {} THEN {e \in S : e[3] = "merge"}
  \* Otherwise: the operands newer than the newest value or deletion.
  ELSE {e \in S : e[3] = "merge" /\ e[2] > Newest(Terms(S))[2]}

\* What the operands build on: the newest terminator's value, or Absent
\* when it is a deletion or there is none.
Base(S) ==
  \* No value or deletion at all: nothing to build on.
  IF Terms(S) = {} THEN Absent
  \* Otherwise t is the newest of them: its value if it is a put, nothing if it is a deletion.
  ELSE LET t == Newest(Terms(S)) IN IF t[3] = "put" THEN t[4] ELSE Absent

\* What a read returns from the entries it sees: Absent when it sees none,
\* the newest terminator's value when nothing is merged over it, otherwise
\* the operands applied oldest first onto that base.
Resolve(S) ==
  \* No entry visible: the key reads as absent.
  IF S = {} THEN Absent
  \* Nothing merged on top: the base.
  ELSE IF Leading(S) = {} THEN Base(S)
  \* Otherwise the operands applied to the base, oldest first.
  ELSE FoldOps(Base(S), Leading(S))

\* A read of key k at snapshot s over the set of entries T.
Read(T, k, s) == Resolve({e \in T : e[1] = k /\ e[2] <= s})

\* The sequences the live snapshots read at.
LiveSeqs == {snap[r] : r \in {x \in Readers : live[x]}}

\* Renaming the readers changes nothing the invariants look at, so TLC
\* checks one state of each set of states that differ only by a renaming.
ReaderSymmetry == Permutations(Readers)

----------------------------------------------------------------------------
\* The reduction.

\* The bound of the stripe sequence s falls in, under the list L: the
\* smallest listed snapshot at or above s, or Infinity for the top stripe.
\* Stripes = "Ignore" puts every entry in the top stripe.
Top(L, s) ==
  \* With stripes ignored, or with no listed snapshot at or above s: the top stripe.
  IF Stripes = "Ignore" \/ ~(\E l \in L : l >= s) THEN Infinity
  \* Otherwise the smallest listed snapshot at or above s.
  ELSE Min({l \in L : l >= s})

\* The filter's decision on a value: Expire removes the value 0 (as a
\* time-to-live filter removes an expired value) ...
FilterRemoves(v) == Filter = "Expire" /\ v = 0

\* ... and changes the value Cap to Cap - 1; every other value is kept.
FilterChange(v) == IF Filter = "Expire" /\ v = Cap THEN Cap - 1 ELSE v

\* Stripes::filter_value on one entry: a removed value becomes a deletion
\* at the same sequence, so an older version cannot resurface; deletions
\* and operands are not judged.
FilterEntry(e) ==
  \* A deletion or an operand: unchanged.
  IF e[3] # "put" THEN e
  \* A value the filter removes: a deletion at the same sequence.
  ELSE IF FilterRemoves(e[4]) THEN <<e[1], e[2], "del", 0>>
  \* Any other value: the value the filter leaves.
  ELSE <<e[1], e[2], "put", FilterChange(e[4])>>

\* The stripes of the inputs I under the list L: for each key and each
\* stripe bound, the entries of that key in that stripe.
StripeSets(I, L) ==
  \* For each key k and each stripe bound t that occurs: the entries of k whose stripe bound is t.
  {{e \in I : e[1] = k /\ Top(L, e[2]) = t} :
     \* (An empty group is dropped.)
     k \in Keys, t \in {Top(L, f[2]) : f \in I}} \ {{}}

\* Stripes::reduce_stripe on one stripe G of one key: keep its newest
\* state. With a terminator: filter it, drop what is beneath it, and fold
\* the operands over it into one value at the stripe's newest sequence.
\* Without one: fold the operands into one operand at the newest sequence,
\* only through an exact partial_merge (or always, in the defect).
ReduceStripe(G) ==
  \* ops: the operands a read of this stripe applies.
  LET ops == Leading(G)
      \* ts: the values and deletions in the stripe.
      ts  == Terms(G)
      \* k: the key.
      k   == Newest(G)[1]
      \* top: the stripe's newest sequence.
      top == Newest(G)[2]
  \* The stripe has a value or deletion:
  IN IF ts # {} THEN
       \* t: the newest of them, after the filter judged it.
       LET t == FilterEntry(Newest(ts))
       \* Nothing merged on top: keep just t.
       IN IF ops = {} THEN {t}
          \* Operands on top: one value at the stripe's newest sequence, holding the operands
          \* applied to t (or to nothing, if t is a deletion).
          ELSE {<<k, top, "put", FoldOps(IF t[3] = "put" THEN t[4] ELSE Absent, ops)>>}
     \* Only operands, more than one, and folding is allowed (the operator is exact, or the planted
     \* bug folds always):
     ELSE IF Cardinality(ops) > 1 /\ (Operator = "Exact" \/ FoldOperandOnly = "Always")
       \* one operand at the stripe's newest sequence, holding their partial merge.
       THEN {<<k, top, "merge", PMFold(ops)>>}
     \* Otherwise the operands stay as they are.
     ELSE ops

\* The run a compaction writes: every stripe of the inputs reduced.
Reduce(I, L) == UNION {ReduceStripe(G) : G \in StripeSets(I, L)}

\* The inputs with only the filter applied, in place, to every stripe's
\* terminator: what the filter decided, without any drop or fold. The
\* expected reads after an install are the reads of this.
FilterInPlace(I, L) ==
  \* For each stripe: unchanged when it has no value or deletion;
  UNION {IF Terms(G) = {} THEN G
         \* otherwise its newest value or deletion is replaced by what the filter makes of it.
         ELSE (G \ {Newest(Terms(G))}) \cup {FilterEntry(Newest(Terms(G)))} :
           \* (G ranges over the stripes of the inputs.)
           G \in StripeSets(I, L)}

----------------------------------------------------------------------------
\* Actions.

\* The start: no write, no run, no snapshot, no compaction.
Init ==
  \* No write yet: sequence 0.
  /\ seq      = 0
  \* The memtable is empty.
  /\ mem      = {}
  \* No run exists.
  /\ runs     = <<>>
  \* No reader holds a snapshot.
  /\ live     = [r \in Readers |-> FALSE]
  \* No reader has registered.
  /\ used     = {}
  \* No snapshot sequence is set.
  /\ snap     = [r \in Readers |-> 0]
  \* No reader expects anything.
  /\ seen     = [r \in Readers |-> [k \in Keys |-> Absent]]
  \* The head expects every key absent.
  /\ headSeen = [k \in Keys |-> Absent]
  \* No key was written.
  /\ last     = [k \in Keys |-> 0]
  \* No compaction is under way.
  /\ cphase   = "idle"
  \* No inputs are chosen.
  /\ win      = <<0, 0>>
  \* No list was read.
  /\ list     = {}

\* A write of key k: an entry of the given kind and value at the next
\* sequence, into the memtable. The head now reads what this write made it.
Write(k, kind, v) ==
  \* The model may still write.
  /\ seq < MaxSeq
  \* The write takes the next sequence.
  /\ seq'      = seq + 1
  \* Its entry goes into the memtable.
  /\ mem'      = mem \cup {<<k, seq + 1, kind, v>>}
  \* It is now the key's newest write.
  /\ last'     = [last EXCEPT ![k] = seq + 1]
  \* The head must now read what the tree with this entry gives.
  /\ headSeen' = [j \in Keys |-> Read(Tree \cup {<<k, seq + 1, kind, v>>}, j, Infinity)]
  \* Nothing else changes.
  /\ UNCHANGED <<runs, live, used, snap, seen, cphase, win, list>>

\* A flush: the memtable becomes the newest run.
Flush ==
  \* There is something in the memtable.
  /\ mem # {}
  \* One more run is allowed.
  /\ Len(runs) < MaxRuns
  \* The memtable's entries become the newest run.
  /\ runs' = Append(runs, mem)
  \* The memtable is empty again.
  /\ mem'  = {}
  \* Nothing else changes: a flush moves entries, it does not change any read.
  /\ UNCHANGED <<seq, live, used, snap, seen, headSeen, last, cphase, win, list>>

\* Reader r registers a snapshot at the visible sequence, and records what
\* it reads for every key at that moment.
Register(r) ==
  \* The reader has not registered before.
  /\ r \notin used
  \* It holds a snapshot now.
  /\ live' = [live EXCEPT ![r] = TRUE]
  \* It has registered.
  /\ used' = used \cup {r}
  \* Its snapshot reads at the newest sequence.
  /\ snap' = [snap EXCEPT ![r] = seq]
  \* It writes down what it reads for every key at this moment.
  /\ seen' = [seen EXCEPT ![r] = [k \in Keys |-> Read(Tree, k, seq)]]
  \* Nothing else changes.
  /\ UNCHANGED <<seq, mem, runs, headSeen, last, cphase, win, list>>

\* Reader r releases its snapshot. What it held is reset, so states that
\* differ only in a released reader's past are one state.
Release(r) ==
  \* The reader holds a snapshot.
  /\ live[r]
  \* It no longer does.
  /\ live' = [live EXCEPT ![r] = FALSE]
  \* Its snapshot sequence is cleared.
  /\ snap' = [snap EXCEPT ![r] = 0]
  \* So is what it expected.
  /\ seen' = [seen EXCEPT ![r] = [k \in Keys |-> Absent]]
  \* Nothing else changes.
  /\ UNCHANGED <<seq, mem, runs, used, headSeen, last, cphase, win, list>>

\* The compaction fixes its inputs: the runs at positions lo..hi. The fix
\* does this first; the defect does it after it read the list.
FixInputs(lo, hi) ==
  \* The engine chooses its inputs first, from idle;
  /\ \/ Capture = "AfterInputs" /\ cphase = "idle"
     \* the planted bug chooses them after it read the list.
     \/ Capture = "BeforeInputs" /\ cphase = "sampled"
  \* The inputs are the runs at positions lo to hi, which exist.
  /\ 1 <= lo /\ lo <= hi /\ hi <= Len(runs)
  \* Those positions are remembered.
  /\ win'    = <<lo, hi>>
  \* The engine goes on to read the list; the bug is now ready to install.
  /\ cphase' = IF Capture = "AfterInputs" THEN "fixed" ELSE "ready"
  \* Nothing else changes.
  /\ UNCHANGED <<seq, mem, runs, live, used, snap, seen, headSeen, last, list>>

\* The compaction reads the live-snapshot list. The fix does this after it
\* fixed its inputs; the defect does it first.
SampleLive ==
  \* The engine reads the list after it chose its inputs;
  /\ \/ Capture = "AfterInputs" /\ cphase = "fixed"
     \* the planted bug reads it first, from idle.
     \/ Capture = "BeforeInputs" /\ cphase = "idle"
  \* The list is the sequences of the snapshots live at this moment.
  /\ list'   = LiveSeqs
  \* The engine is now ready to install; the bug goes on to choose inputs.
  /\ cphase' = IF Capture = "AfterInputs" THEN "ready" ELSE "sampled"
  \* Nothing else changes.
  /\ UNCHANGED <<seq, mem, runs, live, used, snap, seen, headSeen, last, win>>

\* The compaction installs: the input runs are replaced, in place, by one
\* run holding their reduction. Every live reader's and the head's
\* expected read becomes what it reads in the tree just before, with the
\* filter's decisions applied in place (no change when Filter = "None").
Install ==
  \* Inputs are chosen and the list is read.
  /\ cphase = "ready"
  \* lo: the first input position.
  /\ LET lo       == win[1]
         \* hi: the last input position.
         hi       == win[2]
         \* inputs: every entry of the input runs.
         inputs   == UNION {runs[i] : i \in lo..hi}
         \* others: every entry outside them, the memtable included.
         others   == mem \cup UNION {runs[i] : i \in (1..Len(runs)) \ (lo..hi)}
         \* filtered: the tree just before the install, with only the filter's decisions applied to
         \* the inputs.
         filtered == others \cup FilterInPlace(inputs, list)
     \* The input runs are replaced, in place, by one run holding their reduction.
     IN /\ runs' = SubSeq(runs, 1, lo - 1) \o <<Reduce(inputs, list)>>
                     \o SubSeq(runs, hi + 1, Len(runs))
        \* With no filter, what readers expect does not move.
        /\ seen' = IF Filter = "None" THEN seen
                   \* With a filter, each live reader
                   ELSE [r \in Readers |->
                           \* now expects what it reads in `filtered`;
                           IF live[r] THEN [k \in Keys |-> Read(filtered, k, snap[r])]
                           \* a reader with no snapshot keeps its (cleared) expectation.
                           ELSE seen[r]]
        \* The same for the head: unchanged with no filter,
        /\ headSeen' = IF Filter = "None" THEN headSeen
                       \* and with a filter, what a head read of `filtered` gives.
                       ELSE [k \in Keys |-> Read(filtered, k, Infinity)]
  \* The compaction is done.
  /\ cphase' = "idle"
  \* Its inputs are forgotten.
  /\ win'    = <<0, 0>>
  \* So is its list.
  /\ list'   = {}
  \* Nothing else changes.
  /\ UNCHANGED <<seq, mem, live, used, snap, last>>

\* Every step the system can take.
Next ==
  \* For some key:
  \/ \E k \in Keys :
       \* a put of some value,
       \/ \E v \in PutValues : Write(k, "put", v)
       \* a merge operand of 1,
       \/ Write(k, "merge", 1)
       \* or a deletion is written.
       \/ Write(k, "del", 0)
  \* The memtable is flushed.
  \/ Flush
  \* A reader registers a snapshot or releases it.
  \/ \E r \in Readers : Register(r) \/ Release(r)
  \* The compaction chooses its inputs.
  \/ \E lo, hi \in 1..MaxRuns : FixInputs(lo, hi)
  \* The compaction reads the list of live snapshots.
  \/ SampleLive
  \* The compaction installs its result.
  \/ Install

\* Every behaviour: start in Init, then take Next steps or stutter.
Spec == Init /\ [][Next]_vars

----------------------------------------------------------------------------
\* Invariants.

\* Every variable holds what its comment says.
TypeOK ==
  \* The sequence stays within the bound.
  /\ seq \in 0..MaxSeq
  \* No more runs than allowed.
  /\ Len(runs) <= MaxRuns
  \* Each reader holds a snapshot or does not.
  /\ live \in [Readers -> BOOLEAN]
  \* Each snapshot sequence is within the bound.
  /\ snap \in [Readers -> 0..MaxSeq]
  \* Each key's newest sequence is within the bound.
  /\ last \in [Keys -> 0..MaxSeq]
  \* The compaction is in one of its four phases.
  /\ cphase \in {"idle", "fixed", "sampled", "ready"}
  \* The list holds sequences within the bound.
  /\ list \subseteq 0..MaxSeq

\* THE HEADLINE. Every live snapshot reads every key the same at every step
\* after it registered, flushes and compactions included; with a filter,
\* up to the values the filter changed or removed. Lean: reduce_reads.
SnapshotReadsKept ==
  \* Rules out, for example: a reader takes a snapshot when the key holds 5; a newer write makes it
  \* 7; a compaction keeps only 7; and the reader, still holding its snapshot, now reads 7 or
  \* nothing. A snapshot must show one frozen moment for as long as it lives.
  \A r \in Readers : live[r] => \A k \in Keys : Read(Tree, k, snap[r]) = seen[r][k]

\* A read at the head returns what the writes made it, through every
\* flush and compaction; with a filter, up to its decisions. Lean:
\* reduce_reads, at a snapshot above every sequence.
\* Rules out, for example: three "+1" operands on an empty key read as 3 before a compaction and as
\* 2 after it, because the compaction folded them with a partial_merge that rounds.
HeadKept == \A k \in Keys : Read(Tree, k, Infinity) = headSeen[k]

\* The newest version of every key keeps its sequence, so commit
\* validation sees the same newest write. Lean: reduce_keeps_newest.
NewestKept ==
  \* For every key:
  \A k \in Keys :
    \* ss: the sequences of its entries in the tree.
    LET ss == {e[2] : e \in {f \in Tree : f[1] = k}}
    \* The largest of them (0 when there is none) is the sequence of its newest write. Rules out a
    \* compaction that folds versions into an entry with an older sequence: commit validation
    \* compares that sequence with transactions' snapshots, and would miss a conflict.
    IN (IF ss = {} THEN 0 ELSE Max(ss)) = last[k]

====
