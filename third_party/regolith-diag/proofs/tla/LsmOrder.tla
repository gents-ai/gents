---- MODULE LsmOrder ----
\* ===========================================================================
\* THE STORY
\* ===========================================================================
\* The database keeps its data in layers. New writes go into a table in
\* memory (the memtable). A full memtable is frozen and later written to a
\* file. Those files pile up in "level 0", and background work merges them
\* into deeper levels. Nothing is ever overwritten in place: a key that was
\* written three times has three versions, possibly in three different
\* layers.
\*
\* A read does not look everywhere and compare. It walks the layers from
\* newest to oldest and stops at the FIRST layer that holds the key. That is
\* only right if a newer version of a key always sits in a layer the read
\* reaches before the layer holding an older version. Every step that moves
\* data between layers must keep that order.
\*
\* Tiny example of what can go wrong. Key 7 was written at sequence 2 (now
\* in an old level-0 file) and again at sequence 5 (now in a newer level-0
\* file). A compaction moves only the newer file down to level 1. Level 0 is
\* read before level 1, so the next read of key 7 finds the old file first
\* and returns the version of sequence 2: the database went back in time.
\*
\* What this model shows: through every write, freeze and flush, level-0
\* install, `compact_range` (E1), level compaction, a universal merge inside
\* level 0 (E38), external ingest, and the demotion of an overlapping level
\* at open (E14, D13), a point read returns the newest version visible at
\* its snapshot, with every level below level 0 searched by binary search on
\* its tables' key ranges. Each way of breaking the order is a RED
\* configuration.
\*
\* PROVED FOR EVERY SIZE in Lean, proofs/lean/Regolith/LsmOrder.lean:
\*   read_newest                    the read order returns the newest visible
\*                                  version, on ordered sources
\*   put_ordered, flush_ordered     a write and a flush keep the sources ordered
\*   install_oldest_same_order      installing the oldest frozen memtable in
\*                                  L0 leaves the read order as it was
\*   install_newer_breaks_reads     installing a newer one first, the RED
\*                                  FlushAnyOrder case, as a counterexample
\*   flush_keeps_sources_disjoint   a flush that installs its table and
\*                                  retires its memtable in one step leaves
\*                                  no version in two sources
\*   split_install_holds_a_memtable_twice
\*                                  the RED SplitRetire case: the table
\*                                  published before the memtable is retired
\*                                  puts every version of it in two sources
\*   split_install_reads_an_operand_twice
\*                                  and a read that collects a key's merge
\*                                  operands then sees each of them twice
\*   compact_range_reads_newest     compacting the closure keeps the sources
\*                                  ordered and changes no read
\*   pickL0_closed, pickL0_covers   the closure picker returns a closed set
\*                                  holding every file that meets the range
\*   intersect_only_breaks_reads    the RED Intersect case, as a counterexample
\*   ingest_ordered                 a file of the newest sequence placed below
\*                                  every source that holds none of its keys
\*                                  keeps the sources ordered
\*   ingest_below_holder_breaks_reads
\*                                  the RED IngestIgnoresUpper case
\*   demote_reads_newest            demoting the overlapping level and every
\*                                  level above it to L0, in age order, keeps
\*                                  the read order and every read
\*   noShared_level_ordered         the overlapping level's tables share no
\*                                  key, so any order of them is ordered
\*   demote_level_only_breaks_reads the RED DemoteLevelOnly case
\*   bsearch_eq_scan, level_read_bsearch
\*                                  binary search over a sorted level whose
\*                                  tables do not overlap finds the table a
\*                                  linear scan finds
\*   merge_window_ordered, merge_window_reads_newest
\*                                  merging sources that lie next to one
\*                                  another, in place, keeps the read order
\*                                  and every read
\*   merge_gapped_breaks_reads      the RED L0MergeGap case
\* TLC checks the same invariants here over every interleaving of those
\* steps, for a few keys and versions.
\*
\* THE ENGINE, as it reads and compacts.
\*   RegolithEngine::lookup_in_view   src/engine/mod.rs
\*     A point read walks the memtables, then the L0 files newest first,
\*     then each deeper level, where a binary search on key ranges picks
\*     the one table that may hold the key. The first source holding a
\*     version of the key at or below the snapshot answers, with its newest
\*     such version.
\*   run_compact_range, l0_inputs     src/engine/compaction.rs
\*     At L0 the inputs are the files that intersect [start, end] and every
\*     older file whose keys overlap theirs, until nothing more joins (the
\*     closure, E1). They and the L1 tables their key range overlaps are
\*     merged into L1.
\*   pick_and_run_universal,          src/engine/compaction.rs
\*   perform_compaction_to (L0 to L0)
\*     Under universal compaction a merge takes the newest L0 files, in the
\*     order L0 lists them, writes them as one file, and puts that file in
\*     the place of the oldest of them; a file flushed while the merge ran
\*     stays newer. So the files merged lie next to one another in L0
\*     (MergeL0 here).
\*   RegolithEngine::install          src/engine/ingest.rs
\*     An external file takes a fresh sequence and is placed at a level
\*     (`placement`), after the memtables holding a key of its range are
\*     flushed (`flush_memtables_holding`).
\*   manifest replay, Repair::prepare src/engine/manifest.rs,
\*                                    src/engine/manifest/legacy_overlap.rs
\*     Replay finds the deepest level whose tables overlap and moves every
\*     level from 1 down to it into L0 (E14, D13): the deepest level's
\*     tables oldest, the L0 files it already held newest, each level's
\*     tables in the order the manifest lists them. The move reaches the
\*     manifest as one batch, with the open's first edit
\*     (VersionSet::apply). That is Open below, with Legacy = "Demote".
\*   rotate, Ordered::seal            src/engine/commit/turn.rs, ordered.rs
\*                                    (Freeze here)
\*     A write whose turn comes with the active memtable full has the
\*     rotation run first, as an ordered unit of the commit ring: the
\*     memtable becomes the newest frozen one and a fresh one takes the
\*     writes. The flush is not done there (E9). A rotation
\*     that would leave more than max_write_buffer_number memtables first
\*     writes the oldest frozen one out itself: that is the bound MaxImm.
\*   Flusher::flush_oldest            src/engine/flush.rs (FlushInstall)
\*     Whoever flushes (the compaction worker, the bounded step a write owes
\*     with no worker, a rotation at the cap, an explicit flush) takes the
\*     `flushing` exclusion, picks the OLDEST frozen memtable, writes its
\*     table, then installs it as the newest L0 file and retires the
\*     memtable in ONE publication of the read view
\*     (VersionGuard::retire_with_version, src/engine/read_view.rs), so a
\*     flush is one step here, and no reader loads a view between the two.
\*
\* THE DEFECTS, one per mutant constant.
\*   Picker = "Intersect" (E1). Picking only the L0 files that intersect the
\*     range can move a newer version of a key into L1 while an older one
\*     stays in an older, unpicked L0 file, which every read consults first.
\*   FlushOrder = "Any". With several frozen memtables, a newer one's file
\*     is installed in L0 before an older one is flushed. The older frozen
\*     memtable is read before L0, so its older version answers first. This
\*     is what flushes off the commit path would do without the `flushing`
\*     exclusion: two threads flushing at once, the newer memtable's flush
\*     finishing first.
\*   FlushPublish = "Split" (E31). The flush publishes the view holding its
\*     table, and only then, in a second publication, the view without its
\*     memtable. A reader that loads the view in between holds the memtable
\*     and its table at once: every version of that memtable twice, at the
\*     same sequence. A point read is not fooled (both copies agree), but a
\*     read of a merged key collects the operands of every source, so it
\*     applies each operand twice: merges "a" then "b" read back as "abab".
\*   Ingest = "IgnoreUpper". The file is placed at the deepest level whose
\*     own tables it does not overlap, without checking L0 and the levels
\*     above, which may hold older versions of its keys and are read first.
\*   Legacy = "DemoteLevelOnly". At open, only the overlapping level moves
\*     to L0, in front of the levels above it, which hold newer versions.
\*   Legacy = "Keep". The overlapping level stays, and binary search over
\*     ranges that overlap lands on a table that does not hold the key.
\*   L0Merge = "Gapped" (E38). A universal merge takes L0 files that do not
\*     lie next to one another and leaves one between them out. The merged
\*     file takes the place of the oldest file merged, below the file left
\*     out, which is older than the newest file merged: a key both hold
\*     reads as the older version. The engine did this when it took the
\*     order of file ids for the order of L0: a merge's output draws its id
\*     when it starts writing and takes its oldest input's place, so a file
\*     flushed while the merge ran has the smaller id and the newer place.
\*
\* THE FIXES.
\*   Picker = "Closure": add every older L0 file whose key range overlaps
\*     the picked set's, until nothing changes (l0_inputs in the engine).
\*   FlushOrder = "Oldest": a flush installs in memtable order; a file
\*     written early waits for the older ones. The code gets this from the
\*     `flushing` exclusion and from always taking the oldest frozen
\*     memtable, whichever thread flushes.
\*   FlushPublish = "Together": the table joins L0 and the memtable leaves
\*     the frozen list in the same publication, so the sources of every view
\*     a reader can load share no version (NoDoubleSource).
\*   Ingest = "Placed": the deepest level L such that no L0 file holds a key
\*     in the file's range and no table of levels 1..L overlaps it; L0 when
\*     there is none. A memtable holding a key in the range is flushed
\*     first (D48), so the action requires none does.
\*   Legacy = "Demote": at open, if levels overlap, take the deepest such
\*     level d and move every table of levels 1..d to the end of L0, level
\*     by level. Within one level the tables may be taken in ANY order: no
\*     two of them hold the same key (Lean: noShared_level_ordered), so no
\*     read can tell. The engine takes them in the order the manifest lists
\*     them, since a table does not record its newest sequence; Open below
\*     tries every order, the engine's among them.
\*   L0Merge = "Window": a universal merge takes files that lie next to one
\*     another in L0, in the order L0 lists them and not by file id.
\*
\* DESIGN CHOICE where the plan leaves room (recorded in the report). Plan
\* 4.3 says the overlapping level's tables move to L0. Moved alone they sit
\* in front of levels 1..d-1, which hold newer versions of the same keys,
\* so for d >= 2 reads go backwards (RED DemoteLevelOnly). The reading that
\* keeps reads correct moves levels 1..d together. Those levels are smaller
\* than level d by the level multiplier, so this adds about a tenth to a
\* one-time move. For d = 1 the two readings coincide.
\*
\* HOW AN OVERLAPPING LEVEL ARISES (E14). 0.1.x ingested a file carrying a
\* range tombstone at a level chosen by its point keys but recorded its key
\* range widened by the tombstone. LegacyIngest models that: before Open,
\* a file of one key is placed by its point key and recorded with a wider
\* range. Range tombstones themselves are left out: the overlap they cause
\* in recorded ranges is what breaks the binary search. What the open does
\* with the tombstones of the tables it moves is LegacyRangeRead.tla.
\*
\* WHAT THE MODEL LEAVES OUT, and why that loses nothing.
\*   - compact_range flushes the memtable before it picks; here a flush may
\*     happen at any moment, that one included.
\*   - A compaction writes one output table; splitting it into several
\*     tables with disjoint ranges changes no read.
\*   - A compaction may drop a version no registered snapshot can see
\*     (StripeCompaction.tla). The reads checked here are at every snapshot,
\*     so every version is kept.
\*
\* CONFIGURATIONS.
\*   MC_LsmOrder_Green                       E1 closure, L1 as tables
\*   MC_LsmOrder_Red_Intersect               ReadNewest fails
\*   MC_LsmOrder_Green_Flush                 frozen memtables, in order
\*   MC_LsmOrder_Red_FlushAnyOrder           ReadNewest fails
\*   MC_LsmOrder_Red_SplitRetire             NoDoubleSource fails (E31)
\*   MC_LsmOrder_Green_Ingest                two levels, ingest placement
\*   MC_LsmOrder_Red_IngestIgnoresUpper      ReadNewest fails
\*   MC_LsmOrder_Green_Demote                legacy overlap, demoted at open
\*   MC_LsmOrder_Red_DemoteLevelOnly         ReadNewest fails
\*   MC_LsmOrder_Red_NoDemotion              ReadNewest fails
\*   MC_LsmOrder_Green_L0Merge               universal merges of neighbours
\*   MC_LsmOrder_Red_L0MergeGap              ReadNewest fails (E38)

\* Borrow the standard TLA+ books: counting numbers, lists, and sizes of finite sets.
EXTENDS Naturals, Sequences, FiniteSets

\* The fixed inputs a configuration chooses.
CONSTANTS
  \* The user keys: a finite set of numbers, ordered the way keys are ordered.
  Keys,
  \* The last sequence number a write may take. It bounds how many writes a run of the model makes.
  MaxSeq,
  \* The largest number of L0 files a flush or an ingest lets pile up.
  MaxL0,
  \* How compact_range picks its L0 inputs: "Closure" (the engine's l0_inputs) or "Intersect" (the
  \* planted bug, the picker before E1).
  Picker,
  \* The largest number of frozen memtables (Options::max_write_buffer_number, less the active one);
  \* 0 means a flush writes the memtable out directly.
  MaxImm,
  \* Which frozen memtable a flush installs: "Oldest" (the engine) or "Any" (the planted bug).
  FlushOrder,
  \* How a flush publishes: "Together" (the engine: the flush's table and the retirement of its
  \* memtable are one publication) or "Split" (the planted bug, E31: two publications).
  FlushPublish,
  \* How many levels lie below L0.
  Levels,
  \* External file ingest: "Off", "Placed" (the engine's placement) or "IgnoreUpper" (the planted
  \* bug).
  Ingest,
  \* A database written by 0.1.x with an overlapping level: "Off" (none), "Demote" (the plan's fix
  \* at open), "DemoteLevelOnly" or "Keep" (the planted bugs).
  Legacy,
  \* A universal merge of L0 files into one L0 file: "Off" (none), "Window" (the engine: the files
  \* merged lie next to one another in L0) or "Gapped" (the planted bug, E38: a file between them may
  \* be left out).
  L0Merge

\* There is at least one key, and keys are numbers.
ASSUME Keys \subseteq Nat /\ Keys # {}
\* The bounds are numbers, and there is at least one level below L0.
ASSUME MaxSeq \in Nat /\ MaxL0 \in Nat /\ MaxImm \in Nat /\ Levels \in Nat \ {0}
\* The picker is one of the two.
ASSUME Picker \in {"Closure", "Intersect"}
\* The flush order is one of the two.
ASSUME FlushOrder \in {"Oldest", "Any"}
\* A flush publishes its table and retires its memtable together, or apart.
ASSUME FlushPublish \in {"Together", "Split"}
\* The ingest rule is one of the three.
ASSUME Ingest \in {"Off", "Placed", "IgnoreUpper"}
\* The legacy rule is one of the four.
ASSUME Legacy \in {"Off", "Demote", "DemoteLevelOnly", "Keep"}
\* The universal merge rule is one of the three.
ASSUME L0Merge \in {"Off", "Window", "Gapped"}

\* The state: everything that changes from one step to the next. Each line says what the variable
\* stands for in the real program.
VARIABLES
  \* The last sequence number handed out, 0 before the first write (the engine's commit sequence).
  seq,
  \* The active memtable, where new writes go: a set of versions <<key, sequence>>
  \* (ReadView::active, a MemTable; src/engine/read_view.rs).
  mem,
  \* The frozen memtables, newest first: full memtables waiting to be written to a file
  \* (ReadView::frozen).
  imm,
  \* The L0 files, newest first, each a set of versions. L0 files may overlap each other in keys
  \* (Version::levels[0], src/engine/manifest.rs; the engine stores them oldest first and reads from
  \* the back).
  l0,
  \* The deeper levels: lv[j] is level j's tables in key order, each a record [v |-> its versions,
  \* lo |-> its least key, hi |-> its greatest key], where lo and hi are the range the manifest
  \* records for the table (SsTableMeta::smallest_key and largest_key).
  lv,
  \* Whether the open that deals with overlapping levels has run. FALSE only while a legacy (0.1.x)
  \* database is being built.
  opened,
  \* The frozen memtable (its set of versions) whose table a split flush has published while the
  \* memtable is still frozen; {} when no flush is between its two publications. Always {} with
  \* FlushPublish = "Together": the real code has no such moment. It stands for the flush still
  \* holding the `flushing` exclusion (Flusher::flush_oldest, src/engine/flush.rs) after its table
  \* is in.
  owed

\* Every variable, so a step that changes none of them is a stutter.
vars == <<seq, mem, imm, l0, lv, opened, owed>>

----------------------------------------------------------------------------
\* Helpers.

\* The greatest element of a finite, nonempty set of naturals.
Max(S) == CHOOSE m \in S : \A n \in S : n <= m
\* The least element of a finite, nonempty set of naturals.
Min(S) == CHOOSE m \in S : \A n \in S : m <= n

\* The sequences of the versions of key k in a set of versions F that a
\* snapshot at `snap` may see: those at or below it.
Visible(F, k, snap) == {v[2] : v \in {x \in F : x[1] = k /\ x[2] <= snap}}

\* The keys a file holds. Its key range runs from the least to the greatest.
KeysOf(F) == {v[1] : v \in F}

\* The closed ranges [a, b] and [c, d] share a key.
RangesMeet(a, b, c, d) == a <= d /\ c <= b

\* The elements of T at positions 1.. not in S, in order.
RECURSIVE Keep(_, _, _)
\* Walk T from position i:
Keep(T, S, i) ==
  \* past the end, nothing; otherwise this element unless its position is in S, followed by the
  \* rest.
  IF i > Len(T) THEN <<>> ELSE (IF i \in S THEN <<>> ELSE <<T[i]>>) \o Keep(T, S, i + 1)

\* Table t inserted into the key-ordered tables T: after every table whose
\* range starts lower, or starts at the same key and ends lower.
Insert(T, t) ==
  \* i: how many tables of T sort before t.
  LET i == Cardinality({x \in 1..Len(T) : T[x].lo < t.lo \/ (T[x].lo = t.lo /\ T[x].hi < t.hi)})
  \* Those tables, then t, then the others.
  IN SubSeq(T, 1, i) \o <<t>> \o SubSeq(T, i + 1, Len(T))

\* Every way of listing the positions 1..n once each: the orders n tables can be taken in.
Orders(n) == {p \in [1..n -> 1..n] : \A a, b \in 1..n : a # b => p[a] # p[b]}

\* The tables T taken in the order p, as L0 files.
Listed(T, p) == [i \in 1..Len(T) |-> T[p[i]].v]

\* The version sets of a sequence of tables, as L0 files.
Files(T) == [i \in 1..Len(T) |-> T[i].v]

----------------------------------------------------------------------------
\* Reads.

\* Binary search on level tables T for key k: the least position i in a..b
\* with i = b or T[i].hi >= k, halving [a, b) each step. On tables sorted
\* by range and not overlapping, that is the one table whose range may
\* hold k. Lean: bsearch_eq_scan.
RECURSIVE Bisect(_, _, _, _)
\* Search the positions a up to, not including, b.
Bisect(T, k, a, b) ==
  \* One position left: that is the answer.
  IF a >= b THEN a
  \* mid: the middle position.
  ELSE LET mid == (a + b) \div 2
       \* The middle table ends below k: look to its right; otherwise look at it and to its left.
       IN IF T[mid].hi < k THEN Bisect(T, k, mid + 1, b) ELSE Bisect(T, k, a, mid)

\* The source level j offers a read of k: the versions of the table the
\* binary search lands on, when its range holds k; else nothing.
Candidate(j, k) ==
  \* T: the tables of level j.
  LET T == lv[j]
      \* i: where the binary search lands (one past the end when every table ends below k).
      i == Bisect(T, k, 1, Len(T) + 1)
  \* If it landed on a table whose range holds k, that table's versions; otherwise nothing.
  IN IF i <= Len(T) /\ T[i].lo <= k THEN T[i].v ELSE {}

\* What a linear scan of level j offers: every table whose range holds k.
ScanCandidate(j, k) == UNION {lv[j][i].v : i \in {x \in 1..Len(lv[j]) : lv[j][x].lo <= k /\ k <= lv[j][x].hi}}

\* The sources a read of k consults in order: the memtable, the frozen
\* memtables newest first, the L0 files newest first, then one table per
\* level.
Sources(k) == <<mem>> \o imm \o l0 \o [j \in 1..Levels |-> Candidate(j, k)]

\* RegolithEngine::lookup_in_view over the sources S of key k: the first
\* source in read order holding a version of k the snapshot may see
\* answers, with its newest one. 0 means the key reads as absent
\* (sequences start at 1).
ReadIn(S, k, snap) ==
  \* hits: the positions of the sources that hold a version of k the snapshot may see.
  LET hits == {i \in 1..Len(S) : Visible(S[i], k, snap) # {}}
  \* None: the key reads as absent. Otherwise the newest visible version in the first such source.
  IN IF hits = {} THEN 0 ELSE Max(Visible(S[Min(hits)], k, snap))

\* A point read of k at snapshot snap.
Read(k, snap) == ReadIn(Sources(k), k, snap)

\* Every version the tree holds, whichever source holds it.
AllVersions ==
  \* The active memtable, every frozen memtable, every L0 file,
  mem \cup UNION {imm[i] : i \in 1..Len(imm)} \cup UNION {l0[i] : i \in 1..Len(l0)}
      \* and every table of every level.
      \cup UNION {UNION {lv[j][t].v : t \in 1..Len(lv[j])} : j \in 1..Levels}

\* What a correct read returns over the versions A: the newest version of k
\* that the snapshot may see, or 0 when there is none.
NewestIn(A, k, snap) == IF Visible(A, k, snap) = {} THEN 0 ELSE Max(Visible(A, k, snap))

\* The newest visible version of k anywhere in the tree.
Newest(k, snap) == NewestIn(AllVersions, k, snap)

----------------------------------------------------------------------------
\* Picking the L0 inputs of compact_range(lo, hi). A picked set is a set of
\* positions in l0; a larger position is an older file.

\* The file holds a key inside [lo, hi].
Intersects(F, lo, hi) == \E k \in KeysOf(F) : lo <= k /\ k <= hi

\* The keys of the files at positions P. The picked set's key range runs
\* from the least to the greatest of them.
Hull(P) == UNION {KeysOf(l0[i]) : i \in P}

\* The file's key range meets the range the keys H span: the interval test
\* min(F) <= max(H) /\ min(H) <= max(F). An empty side meets nothing.
Overlaps(F, H) ==
  \* The file holds a key,
  /\ KeysOf(F) # {}
  \* the span is not empty,
  /\ H # {}
  \* the file's least key is at or below the span's greatest,
  /\ Min(KeysOf(F)) <= Max(H)
  \* and the span's least key is at or below the file's greatest.
  /\ Min(H) <= Max(KeysOf(F))

\* Where the picker starts (and all the picker before E1 did): every file
\* that intersects the range.
Initial(lo, hi) == {i \in 1..Len(l0) : Intersects(l0[i], lo, hi)}

\* One closure step: add every file older than some picked file (a later
\* position) whose key range meets the picked set's key range.
Grow(P) ==
  \* P, plus every file j that is older than some picked file and whose key range meets the picked
  \* files' key range.
  P \cup {j \in 1..Len(l0) : (\E i \in P : i < j) /\ Overlaps(l0[j], Hull(P))}

\* Grow to the fixpoint. Each step that changes P adds a file, so it ends
\* within Len(l0) steps.
RECURSIVE Close(_)
\* Nothing more to add: done. Otherwise grow again.
Close(P) == IF Grow(P) = P THEN P ELSE Close(Grow(P))

\* The input set the configured picker chooses.
Pick(lo, hi) == IF Picker = "Closure" THEN Close(Initial(lo, hi)) ELSE Initial(lo, hi)

----------------------------------------------------------------------------
\* Placement of an ingested file of key range [a, b].

\* Some memtable, active or frozen, holds a key in [a, b].
MemHolds(a, b) == \E v \in mem \cup UNION {imm[i] : i \in 1..Len(imm)} : a <= v[1] /\ v[1] <= b

\* Some L0 file holds a key in [a, b].
L0Holds(a, b) == \E i \in 1..Len(l0) : \E v \in l0[i] : a <= v[1] /\ v[1] <= b

\* Some table of level j records a range that meets [a, b].
LevelMeets(j, a, b) == \E t \in 1..Len(lv[j]) : RangesMeet(lv[j][t].lo, lv[j][t].hi, a, b)

\* The file may go to level j. The fix: nothing read before level j holds a
\* key in its range, and no table of level j meets it. The defect checks
\* only level j itself.
Fits(j, a, b) ==
  \* The planted bug: only level j's own tables are checked.
  IF Ingest = "IgnoreUpper" THEN ~LevelMeets(j, a, b)
  \* The engine: no L0 file holds a key of the range, and no table of levels 1 to j meets it.
  ELSE ~L0Holds(a, b) /\ \A i \in 1..j : ~LevelMeets(i, a, b)

----------------------------------------------------------------------------
\* Demotion at open.

\* The levels whose recorded ranges overlap: two tables share a key range.
OverlapLevels ==
  \* The levels j
  {j \in 1..Levels :
     \* that have two different tables
     \E t, u \in 1..Len(lv[j]) :
       \* whose recorded ranges share a key.
       t # u /\ RangesMeet(lv[j][t].lo, lv[j][t].hi, lv[j][u].lo, lv[j][u].hi)}

\* One order for each level's tables: ord[j] is an order of level j's.
OrderChoices ==
  \* Every function from levels to orders
  {ord \in [1..Levels -> UNION {Orders(Len(lv[j])) : j \in 1..Levels}] :
     \* that gives each level an order of its own tables.
     \A j \in 1..Levels : ord[j] \in Orders(Len(lv[j]))}

\* The tables of the levels in D, in level order, each level's tables in
\* the order ord gives it: the files they become in L0, in read order.
RECURSIVE Demoted(_, _, _)
\* Walk the levels from j down:
Demoted(D, j, ord) ==
  \* past the last level, nothing;
  IF j > Levels THEN <<>>
  \* otherwise this level's tables in its order if the level is in D, followed by the deeper levels.
  ELSE (IF j \in D THEN Listed(lv[j], ord[j]) ELSE <<>>) \o Demoted(D, j + 1, ord)

\* The levels the configured open demotes. The fix: every level from L1
\* down to the deepest overlapping one. The defect: the overlapping ones.
DemotedLevels ==
  \* No level overlaps, or the bug "Keep": nothing moves.
  CASE OverlapLevels = {} \/ Legacy = "Keep" -> {}
    \* The fix: every level from 1 down to the deepest overlapping one.
    [] Legacy = "Demote"                     -> 1..Max(OverlapLevels)
    \* The bug "DemoteLevelOnly": only the overlapping levels.
    [] OTHER                                 -> OverlapLevels

----------------------------------------------------------------------------
\* Actions.

\* The start: no write yet, empty memtables, L0 and levels. A legacy
\* configuration starts before the open.
Init ==
  \* No write yet.
  /\ seq    = 0
  \* The active memtable is empty.
  /\ mem    = {}
  \* No frozen memtable.
  /\ imm    = <<>>
  \* No L0 file.
  /\ l0     = <<>>
  \* Every level is empty.
  /\ lv     = [j \in 1..Levels |-> <<>>]
  \* The database counts as opened unless a legacy database is still to be built.
  /\ opened = (Legacy = "Off")
  \* No flush has started, so none is between its two publications.
  /\ owed   = {}

\* A put of key k: the next sequence, inserted into the memtable.
Put(k) ==
  \* The model may still write.
  /\ seq < MaxSeq
  \* The write takes the next sequence.
  /\ seq' = seq + 1
  \* Its version goes into the active memtable.
  /\ mem' = mem \cup {<<k, seq + 1>>}
  \* Nothing else changes.
  /\ UNCHANGED <<imm, l0, lv, opened, owed>>

\* With no frozen memtables (MaxImm = 0), a flush turns the memtable into
\* the newest L0 file, and a new, empty memtable takes the writes.
Flush ==
  \* Only in configurations that keep no frozen memtables.
  /\ MaxImm = 0
  \* There is something to flush.
  /\ mem # {}
  \* L0 has room.
  /\ Len(l0) < MaxL0
  \* The memtable's versions become the newest L0 file.
  /\ l0'  = <<mem>> \o l0
  \* The memtable is empty again.
  /\ mem' = {}
  \* Nothing else changes.
  /\ UNCHANGED <<seq, imm, lv, opened, owed>>

\* Rotation freezes the memtable (Ordered::seal, in an ordered unit of the
\* commit ring): it becomes the newest frozen memtable, still read before
\* L0, and a new, empty memtable takes the writes. Nothing is flushed here
\* (E9).
Freeze ==
  \* This configuration keeps frozen memtables.
  /\ MaxImm > 0
  \* There is something to freeze.
  /\ mem # {}
  \* Under the cap: at the cap the rotation flushes the oldest frozen memtable first.
  /\ Len(imm) < MaxImm
  \* The memtable joins the front: it is the newest frozen one.
  /\ imm' = <<mem>> \o imm
  \* Writes go to a fresh, empty memtable.
  /\ mem' = {}
  \* No sequence, file, level or flush in flight changes.
  /\ UNCHANGED <<seq, l0, lv, opened, owed>>

\* A flush of frozen memtable i (Flusher::flush_oldest, by whatever thread)
\* installs its file as the newest L0 file and drops the memtable. The fix
\* installs only the oldest (the last in `imm`); the defect installs
\* whichever flush finished first.
\*
\* The table and the retirement are ONE publication in the real code
\* (`versions.apply` then `retire_with_version` in one section of the
\* version store's step, published together by VersionGuard::drop). Example: memtable
\* {<<1, 1>>} is frozen; after this step L0 holds the file {<<1, 1>>} and
\* the frozen list is empty, and no state in between exists for a reader to
\* load. The E31 bug ("Split") publishes the table here and leaves the
\* memtable frozen for FlushRetire below.
FlushInstall(i) ==
  \* i names a frozen memtable.
  /\ i \in 1..Len(imm)
  \* It is the oldest one, unless the bug is planted.
  /\ FlushOrder = "Any" \/ i = Len(imm)
  \* No other flush is half done (the `flushing` exclusion): none has published its table and
  \* still owes the retirement of its memtable.
  /\ owed = {}
  \* L0 has room (this keeps the model small).
  /\ Len(l0) < MaxL0
  \* Its file is the newest L0 file.
  /\ l0'  = <<imm[i]>> \o l0
  \* What happens to the memtable depends on how the flush publishes.
  /\ IF FlushPublish = "Together"
       \* The real code: the memtable leaves the frozen list in the same
       \* publication,
       THEN /\ imm'  = Keep(imm, {i}, 1)
            \* and the flush is over: it owes nothing.
            /\ owed' = {}
       \* The E31 bug: the memtable stays frozen. Readers can load this state.
       ELSE /\ imm'  = imm
            \* The flush still owes the retirement of this memtable.
            /\ owed' = imm[i]
  \* Nothing else changes.
  /\ UNCHANGED <<seq, mem, lv, opened>>

\* The E31 bug only: the second publication of a split flush. The memtable
\* whose table FlushInstall already published leaves the frozen list now
\* (ReadViewCell::retire_memtable called on its own). The real code never
\* takes this step, because `owed` is never set there.
FlushRetire ==
  \* A flush published its table and still owes the retirement of its memtable.
  /\ owed # {}
  \* The frozen list without that memtable. It is found by what it holds,
  \* as the code finds it by identity: no two memtables hold the same
  \* versions, since every write takes a sequence of its own.
  /\ imm'  = Keep(imm, {j \in 1..Len(imm) : imm[j] = owed}, 1)
  \* The flush is over: it owes nothing.
  /\ owed' = {}
  \* Nothing else changes.
  /\ UNCHANGED <<seq, mem, l0, lv, opened>>

\* compact_range(lo, hi) at L0: the picked files leave L0 and merge with
\* the L1 tables their key range meets into one L1 table.
CompactRange(lo, hi) ==
  \* P: the positions of the L0 files picked.
  LET P   == Pick(lo, hi)
      \* V: every version in them.
      V   == UNION {l0[i] : i \in P}
      \* L1: the tables of level 1.
      L1  == lv[1]
      \* ins: the L1 tables whose range meets the range the picked versions span.
      ins == {t \in 1..Len(L1) : RangesMeet(L1[t].lo, L1[t].hi, Min(KeysOf(V)), Max(KeysOf(V)))}
      \* out: the one table that replaces them all: their versions together,
      out == [v  |-> V \cup UNION {L1[t].v : t \in ins},
              \* from the least key of any of them
              lo |-> Min({Min(KeysOf(V))} \cup {L1[t].lo : t \in ins}),
              \* to the greatest key of any of them.
              hi |-> Max({Max(KeysOf(V))} \cup {L1[t].hi : t \in ins})]
  \* Something was picked.
  IN /\ P # {}
     \* The picked files leave L0.
     /\ l0' = Keep(l0, P, 1)
     \* In level 1, the met tables are replaced by the output table, put in key order.
     /\ lv' = [lv EXCEPT ![1] = Insert(Keep(L1, ins, 1), out)]
     \* Nothing else changes.
     /\ UNCHANGED <<seq, mem, imm, opened, owed>>

\* A level compaction: table t of level j merges with the tables of level
\* j + 1 its range meets into one table of level j + 1.
CompactLevel(j, t) ==
  \* There is a level below j.
  /\ j < Levels
  \* t names a table of level j.
  /\ t \in 1..Len(lv[j])
  \* T: that table.
  /\ LET T   == lv[j][t]
         \* N: the tables of the next level.
         N   == lv[j + 1]
         \* ins: the next level's tables whose range meets T's.
         ins == {u \in 1..Len(N) : RangesMeet(N[u].lo, N[u].hi, T.lo, T.hi)}
         \* out: the one table that replaces them all: their versions together,
         out == [v  |-> T.v \cup UNION {N[u].v : u \in ins},
                 \* from the least key of any of them
                 lo |-> Min({T.lo} \cup {N[u].lo : u \in ins}),
                 \* to the greatest key of any of them.
                 hi |-> Max({T.hi} \cup {N[u].hi : u \in ins})]
     \* T leaves level j, and in level j + 1 the met tables are replaced by the output table.
     IN lv' = [lv EXCEPT ![j] = Keep(lv[j], {t}, 1), ![j + 1] = Insert(Keep(N, ins, 1), out)]
  \* Nothing else changes.
  /\ UNCHANGED <<seq, mem, imm, l0, opened, owed>>

\* A universal merge: the L0 files at the positions P (1 is the newest)
\* become one file, which takes the place of the oldest of them. A file
\* flushed while the real merge ran is a newer file here, in front of every
\* position of P, so the merge is one step.
MergeL0(P) ==
  \* Universal merges are part of this configuration.
  /\ L0Merge # "Off"
  \* The database is open.
  /\ opened
  \* P names L0 files,
  /\ P \subseteq 1..Len(l0)
  \* at least two of them.
  /\ Cardinality(P) >= 2
  \* The engine merges files that lie next to one another: every position from the newest picked
  \* to the oldest picked is picked. The planted bug does not ask for that.
  /\ L0Merge = "Window" => \A i \in Min(P)..Max(P) : i \in P
  \* The files in front of the oldest picked one that were not picked stay in front, in order; then
  \* comes the merged file, holding every version of the picked ones; then the older files.
  /\ l0' = Keep(SubSeq(l0, 1, Max(P)), P, 1)
            \o <<UNION {l0[i] : i \in P}>>
            \o SubSeq(l0, Max(P) + 1, Len(l0))
  \* Nothing else changes.
  /\ UNCHANGED <<seq, mem, imm, lv, opened, owed>>

\* An ingest of a file holding the keys ks, all at the next sequence. A
\* memtable holding a key in its range is flushed first (D48), so here none
\* may. The file goes to the deepest level it fits, else to the L0 front.
IngestFile(ks) ==
  \* Ingest is part of this configuration.
  /\ Ingest # "Off"
  \* The database is open.
  /\ opened
  \* The model may still take a sequence.
  /\ seq < MaxSeq
  \* a: the file's least key.
  /\ LET a    == Min(ks)
         \* b: the file's greatest key.
         b    == Max(ks)
         \* F: the file as a table: every key of ks at the next sequence, with the range a to b.
         F    == [v |-> {<<k, seq + 1>> : k \in ks}, lo |-> a, hi |-> b]
         \* fits: the levels the file may go to.
         fits == {j \in 1..Levels : Fits(j, a, b)}
     \* No memtable holds a key of the file's range (the engine flushes such a memtable first).
     IN /\ ~MemHolds(a, b)
        \* The file fits no level:
        /\ IF fits = {}
             \* L0 has room,
             THEN /\ Len(l0) < MaxL0
                  \* the file becomes the newest L0 file,
                  /\ l0' = <<F.v>> \o l0
                  \* and the levels stay.
                  /\ UNCHANGED lv
             \* Otherwise it goes into the deepest level it fits, in key order,
             ELSE /\ lv' = [lv EXCEPT ![Max(fits)] = Insert(@, F)]
                  \* and L0 stays.
                  /\ UNCHANGED l0
  \* The ingest took the next sequence.
  /\ seq' = seq + 1
  \* Nothing else changes.
  /\ UNCHANGED <<mem, imm, opened, owed>>

\* A 0.1.x ingest (E14): a file holding key k at the next sequence, placed
\* at level j by its point key, which nothing read before level j holds and
\* no table of level j meets, but recorded with the wider range [a, b].
LegacyIngest(j, k, a, b) ==
  \* The legacy database is still being built.
  /\ ~opened
  \* The model may still take a sequence.
  /\ seq < MaxSeq
  \* The recorded range a to b holds the key.
  /\ a <= k /\ k <= b
  \* No memtable and no L0 file holds the key.
  /\ ~MemHolds(k, k) /\ ~L0Holds(k, k)
  \* No table of levels 1 to j has a range holding the key.
  /\ \A i \in 1..j : ~LevelMeets(i, k, k)
  \* A table holding just this key, but recorded with the range a to b, joins level j.
  /\ lv'  = [lv EXCEPT ![j] = Insert(@, [v |-> {<<k, seq + 1>>}, lo |-> a, hi |-> b])]
  \* It took the next sequence.
  /\ seq' = seq + 1
  \* Nothing else changes.
  /\ UNCHANGED <<mem, imm, l0, opened, owed>>

\* The first open by this version: the configured demotion moves levels to
\* the end of L0, as one version edit.
Open ==
  \* The database has not been opened by this version yet.
  /\ ~opened
  \* Now it has.
  /\ opened' = TRUE
  \* The demoted tables are added at the old end of L0, level after level, each level's tables
  \* in some order: every order is tried.
  /\ \E ord \in OrderChoices : l0' = l0 \o Demoted(DemotedLevels, 1, ord)
  \* The demoted levels are empty; the others stay.
  /\ lv'     = [j \in 1..Levels |-> IF j \in DemotedLevels THEN <<>> ELSE lv[j]]
  \* Nothing else changes.
  /\ UNCHANGED <<seq, mem, imm, owed>>

\* Every step the system can take.
Next ==
  \* A key is written.
  \/ \E k \in Keys : Put(k)
  \* The memtable is flushed straight to L0.
  \/ Flush
  \* The memtable is frozen.
  \/ Freeze
  \* A frozen memtable's file is installed in L0.
  \/ \E i \in 1..MaxImm : FlushInstall(i)
  \* The planted bug only (E31): a split flush retires its memtable in a second publication.
  \/ FlushRetire
  \* A range is compacted out of L0.
  \/ \E lo, hi \in Keys : lo <= hi /\ CompactRange(lo, hi)
  \* A table is compacted one level down.
  \/ \E j \in 1..Levels : \E t \in 1..Len(lv[j]) : CompactLevel(j, t)
  \* Some L0 files are merged into one L0 file.
  \/ \E P \in SUBSET (1..MaxL0) : MergeL0(P)
  \* A file holding some keys is ingested.
  \/ \E ks \in (SUBSET Keys) \ {{}} : IngestFile(ks)
  \* A 0.1.x ingest builds a legacy level.
  \/ Legacy # "Off" /\ \E j \in 1..Levels, k, a, b \in Keys : LegacyIngest(j, k, a, b)
  \* The database is opened.
  \/ Open

\* Every behaviour: start in Init, then take Next steps or stutter.
Spec == Init /\ [][Next]_vars

----------------------------------------------------------------------------
\* Invariants.

\* Every variable holds what its comment says.
TypeOK ==
  \* The sequence stays within the bound.
  /\ seq \in 0..MaxSeq
  \* The active memtable holds versions of known keys.
  /\ mem \subseteq Keys \X (1..MaxSeq)
  \* No more frozen memtables than the cap.
  /\ Len(imm) <= MaxImm
  \* Each frozen memtable holds versions of known keys.
  /\ \A i \in 1..Len(imm) : imm[i] \subseteq Keys \X (1..MaxSeq)
  \* Each L0 file holds versions of known keys.
  /\ \A i \in 1..Len(l0) : l0[i] \subseteq Keys \X (1..MaxSeq)
  \* There are exactly Levels levels.
  /\ Len(lv) = Levels
  \* Every table of every level
  /\ \A j \in 1..Levels : \A t \in 1..Len(lv[j]) :
       \* holds versions of known keys,
       /\ lv[j][t].v \subseteq Keys \X (1..MaxSeq)
       \* all inside the range recorded for it.
       /\ \A x \in lv[j][t].v : lv[j][t].lo <= x[1] /\ x[1] <= lv[j][t].hi
  \* The open flag is a yes or a no.
  /\ opened \in BOOLEAN
  \* The memtable a split flush still owes holds versions of known keys (or is empty).
  /\ owed \subseteq Keys \X (1..MaxSeq)

\* THE HEADLINE. Once open, every read, of every key, at every snapshot up
\* to the newest sequence, returns the newest version of that key the
\* snapshot may see. Lean: read_newest, with the theorem for each step.
\* (Binding A and S by membership in a one-element set makes TLC compute
\* each once per state and key rather than once per snapshot.)
ReadNewest ==
  \* Once the database is open:
  opened =>
    \* (A is every version in the tree; S is the sources a read of k consults, in order)
    \A A \in {AllVersions} : \A k \in Keys : \A S \in {Sources(k)} :
      \* for every key and every snapshot, the read returns the newest version of that key the
      \* snapshot may see. Rules out, for example: a key is written at sequence 2 and again at
      \* sequence 5, yet a read returns the version of sequence 2 because the file holding it is
      \* consulted first.
      \A snap \in 0..seq : ReadIn(S, k, snap) = NewestIn(A, k, snap)

\* Every table of levels j..Levels as one sequence of version sets, level
\* by level.
RECURSIVE LevelFiles(_)
\* Past the last level there is nothing; otherwise level j's tables, then those of the levels below.
LevelFiles(j) == IF j > Levels THEN <<>> ELSE Files(lv[j]) \o LevelFiles(j + 1)

\* Every source a reader's view holds, one after another: the active
\* memtable, the frozen memtables, the L0 files, then every table of every
\* level. (Sources(k) above is the same walk narrowed to the one table per
\* level that may hold k.)
EverySource == <<mem>> \o imm \o l0 \o LevelFiles(1)

\* NO VERSION IN TWO SOURCES (E31). In every view a reader can load, no two
\* sources hold the same version. The engine needs it because a read of a
\* merged key gathers that key's operands from EVERY source and applies
\* them all: a version held twice is an operand applied twice.
\* The case it rules out: merges "a" (sequence 1) and "b" (sequence 2) sit
\* in a frozen memtable; a split flush publishes their table; a reader
\* loads the view before the memtable is retired, finds both operands in
\* the memtable and both again in the table, and returns "abab" for "ab".
\* Lean: flush_keeps_sources_disjoint, with the RED case as
\* split_install_holds_a_memtable_twice.
NoDoubleSource ==
  \* Name the list of sources once per state.
  \A S \in {EverySource} :
    \* Any two different places in that list ...
    \A a, b \in 1..Len(S) :
      \* ... hold no version in common.
      a # b => S[a] \cap S[b] = {}

\* Once open, every level's tables are in key order and their ranges do
\* not overlap: what the binary search needs.
LevelsSorted ==
  \* Once the database is open, in every level each table ends before the next one begins. Rules out
  \* two tables of one level both covering key 7: a binary search would land on one of them and
  \* might miss the version in the other.
  opened => \A j \in 1..Levels : \A t \in 1..(Len(lv[j]) - 1) : lv[j][t].hi < lv[j][t + 1].lo

\* Once open, the binary search on each level offers exactly what a linear
\* scan of that level does. Lean: bsearch_eq_scan, level_read_bsearch.
BisectIsScan ==
  \* Once the database is open, for every level and key the binary search offers exactly what
  \* looking at every table would. Rules out a search that lands on the wrong table and reports a
  \* key as absent.
  opened => \A j \in 1..Levels : \A k \in Keys : Candidate(j, k) = ScanCandidate(j, k)

====
