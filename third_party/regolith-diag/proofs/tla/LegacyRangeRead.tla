---- MODULE LegacyRangeRead ----
\* ===========================================================================
\* THE STORY
\* ===========================================================================
\* A range delete says "every key from a to g is gone as of sequence 2". A
\* read of key b finds that out by walking the layers of the database from
\* newest to oldest. It remembers the newest range delete over b it has
\* passed, and it stops at the FIRST layer that holds b: if the range delete
\* it remembers is newer than that b, b is deleted. So a range delete only
\* works on an older b if the read passes the delete no later than the b.
\* The engine keeps it so: whatever writes a range delete puts it above
\* every older key it covers.
\*
\* Release 0.1.x broke that. It ingested a table carrying a range delete,
\* gave the whole table one fresh sequence, and put it in the BOTTOM level,
\* below tables holding older keys the delete covers. Two shapes follow.
\*   Moved (E14): the table's recorded range overlaps another table of its
\*     level. This version's open moves the levels into level 0, oldest
\*     level first, so the table with the range delete stays behind the
\*     tables that were above it.
\*   Buried (E37): the recorded range overlaps nothing in its level, so
\*     nothing is moved, and the range delete stays at the bottom, below the
\*     older keys it deletes in the levels above.
\* In the buried shape this version adds a second failure: a compaction that
\* writes the bottom level drops ("retires") a range delete once nothing
\* below it holds what it covers. The older key it deleted, still above it,
\* is then back for good, for scans too.
\*
\* Tiny example of what goes wrong. Key b was put at sequence 1 and sits in
\* level 1. The ingested table, in level 6, deletes a..g at sequence 2. A
\* read finds b in level 1 first, has passed no range delete yet, and
\* returns b. But b was deleted at 2. Later a compaction of level 6 retires
\* the range delete, and even a scan returns b.
\*
\* What this model shows. The open copies, into the memtable it recovers
\* (the first layer any read walks), the range deletes of every table it
\* moves, and every range delete that has something read before it (a
\* superset of the buried ones). From there the delete is passed before
\* every b, wherever a later flush or compaction takes the tables and
\* whatever a bottom compaction retires, so every read returns what the
\* sequences say. And the open writes that memtable to the log BEFORE the
\* manifest records the move and the check, so a power cut never leaves them
\* recorded and the copy lost.
\*
\* PROVED FOR EVERY SIZE in Lean, proofs/lean/Regolith/RangeDelete.lean:
\*   hoisted_delete_reads_by_sequence  with the delete in the first layer, a
\*                                     read returns what the sequences say,
\*                                     however the layers below are ordered
\*   delete_behind_put_reads_deleted_key
\*                                     the RED NoHoist case, as a
\*                                     counterexample
\* TLC checks the same promise here over every interleaving of the open's
\* steps, a power cut, puts, flushes and compactions, for one key.
\*
\* THE ENGINE.
\*   legacy_overlap::Repair::prepare, Repair::demotion
\*                                   src/engine/manifest/legacy_overlap.rs
\*     Manifest replay moves overlapping levels in its own version and
\*     gathers the range deletes of the moved tables (Demotion).
\*   buried_range_deletes            src/engine/buried_range_deletes.rs
\*     When the manifest does not record the check: every range delete of a
\*     table whose range meets a source read before that table (Candidates
\*     here).
\*   recovery::hoist_range_deletes   src/engine/recovery.rs (Load here)
\*     After the logs are replayed into the memtable, those range deletes
\*     are added to it.
\*   recovery::rewrite_recovered_memtable_to_wal
\*                                   src/engine/recovery.rs (WriteLog here)
\*     The memtable is written to a fresh log, which is synced.
\*   VersionSet::apply               src/engine/manifest.rs (Record here)
\*     The open's first manifest edit carries the record of the move and the
\*     record that the range deletes were checked, in one synced batch.
\*     RegolithEngine::open (src/engine/mod.rs) makes that edit only after
\*     the log above is written.
\*   ReadView::walk_newest_first     src/engine/source_walk.rs (Read here)
\*     The walk: the memtables, the level-0 tables newest first, then the
\*     deeper levels, remembering the newest range delete passed.
\*   flush, compaction, compaction::retire
\*                                   src/engine/flush.rs, compaction.rs
\*     A flush turns the memtable into the newest level-0 table. A
\*     compaction out of level 0 takes a table and every older table whose
\*     keys overlap its own (l0_inputs, LsmOrder.tla) down a level. A pass
\*     that writes the bottom level drops every range delete of its inputs,
\*     and every older version they delete among its inputs (E27).
\*
\* THE DEFECTS, one per mutant constant.
\*   Hoist = FALSE. The open copies nothing. The read above returns the
\*     deleted key; with Retire, a bottom pass makes it come back for a scan
\*     too (RED BuriedRetired checks the scan).
\*   PreScan = TRUE with Hoist = FALSE. Instead of the copy, every read
\*     first gathers the range deletes of ALL level-0 tables (the first
\*     shape of the E14 fix). Right until a compaction takes the table with
\*     the delete down a level and leaves the table with the older key in
\*     level 0: the read no longer finds the delete in level 0, and the
\*     key comes back. A compaction changed a read.
\*   RecordFirst = TRUE. The manifest records the move and the check before
\*     the log holding the copy is written. A power cut between the two
\*     leaves a database that looks already repaired, so the next open
\*     copies nothing.
\*
\* WHAT THE MODEL LEAVES OUT, and why that loses nothing.
\*   - Other keys. A range delete acts on each key it covers on its own.
\*     For one key, "a range meets a source" is "the source holds an entry
\*     of the key", which the engine's range test only ever widens.
\*   - Levels 1 to 5 apart: they are one layer, `mid`, read before the
\*     bottom level, which is what matters to a read of the buried shape.
\*   - Registered snapshots. Without Retire nothing is ever dropped, so
\*     reads are checked at every snapshot. With Retire, a bottom pass drops
\*     what no registered snapshot needs, and this model registers none, so
\*     reads are checked at the newest sequence, against every write ever
\*     made.
\*   - A read-only open. It is the open stopped after Load: it reads, and
\*     writes nothing. Reads are checked there too.
\*
\* CONFIGURATIONS.
\*   MC_LegacyRangeRead_Green                 moved: the copy, logged first
\*   MC_LegacyRangeRead_Red_NoHoist           ReadNewest fails
\*   MC_LegacyRangeRead_Red_PreScanOnly       ReadNewest fails
\*   MC_LegacyRangeRead_Red_MoveBeforeLog     ReadNewest fails
\*   MC_LegacyRangeRead_Green_Buried          buried: the copy, bottom passes
\*   MC_LegacyRangeRead_Red_BuriedRetired     ScanNewest fails
\*   MC_LegacyRangeRead_Red_CheckBeforeLog    HeadNewest fails

\* Borrow the standard TLA+ books: counting numbers, lists, and sizes of finite sets.
EXTENDS Naturals, Sequences, FiniteSets

\* The fixed inputs a configuration chooses.
CONSTANTS
  \* The last sequence number a write may take. It bounds how many puts a run makes after the open.
  MaxSeq,
  \* What 0.1.x left: "Moved" (the overlapping level the open moves into level 0) or "Buried"
  \* (the range delete at the bottom, below the key, nothing moved).
  Layout,
  \* Whether the open copies range deletes into the memtable: TRUE (the engine) or FALSE (the
  \* planted bug).
  Hoist,
  \* Whether a read first gathers the range deletes of every level-0 table: FALSE (the engine) or
  \* TRUE (the first shape of the E14 fix, which a compaction defeats).
  PreScan,
  \* Whether the manifest records the move and the check before the log is written: FALSE (the
  \* engine) or TRUE (the planted bug).
  RecordFirst,
  \* Whether a pass writing the bottom level retires range deletes (the engine since E27): TRUE,
  \* or FALSE to keep every entry and check reads at every snapshot.
  Retire

\* The bound is a number, and leaves room for the put at 1 and the range delete at 2.
ASSUME MaxSeq \in Nat /\ MaxSeq >= 2
\* The layout is one of the two.
ASSUME Layout \in {"Moved", "Buried"}
\* Each switch is a yes or a no.
ASSUME Hoist \in BOOLEAN /\ PreScan \in BOOLEAN /\ RecordFirst \in BOOLEAN /\ Retire \in BOOLEAN

\* The state: everything that changes from one step to the next. Each line says what the variable
\* stands for in the real program. An entry is <<"put", s>>, the key written at sequence s, or
\* <<"del", s>>, a range delete over the key at sequence s.
VARIABLES
  \* The last sequence number handed out (the engine's commit sequence).
  seq,
  \* The memtable: the entries in memory, the first layer a read walks (ReadView::active,
  \* src/engine/read_view.rs). Lost at a power cut.
  mem,
  \* The write-ahead log: the memtable's entries on disk, which the next open replays
  \* (src/engine/wal.rs). Kept through a power cut.
  log,
  \* The level-0 tables, newest first, each a set of entries (Version::levels[0],
  \* src/engine/manifest.rs). Kept through a power cut.
  l0,
  \* Levels 1 to 5, as one set of entries, read after level 0. Kept through a power cut.
  mid,
  \* The bottom level, as one set of entries, read last. Kept through a power cut.
  bottom,
  \* Whether the manifest records the move and the check (the batch VersionSet::apply wrote).
  \* FALSE means the next open moves and checks again.
  recorded,
  \* Where the open is: "closed" (not running, or cut off), "loaded" (the memtable is rebuilt),
  \* "logged" (the engine: the new log is written), "recorded" (the planted bug: the manifest
  \* was written first), "open" (the open returned).
  pc,
  \* Every entry ever written, whatever was dropped since: what reads are checked against once a
  \* bottom pass may drop entries. A ghost: the program has no such thing.
  ever

\* Every variable, so a step that changes none of them is a stutter.
vars == <<seq, mem, log, l0, mid, bottom, recorded, pc, ever>>

----------------------------------------------------------------------------
\* Helpers.

\* The greatest element of a finite, nonempty set of naturals.
Max(S) == CHOOSE m \in S : \A n \in S : n <= m
\* The least element of a finite, nonempty set of naturals.
Min(S) == CHOOSE m \in S : \A n \in S : m <= n
\* The greatest element of a finite set of naturals, or 0 when it is empty (sequences start at 1).
Newest(S) == IF S = {} THEN 0 ELSE Max(S)

\* The sequences of the puts in the set of entries B that a snapshot at `snap` may see.
Puts(B, snap) == {e[2] : e \in {x \in B : x[1] = "put" /\ x[2] <= snap}}
\* The sequences of the range deletes in B that a snapshot at `snap` may see.
Dels(B, snap) == {e[2] : e \in {x \in B : x[1] = "del" /\ x[2] <= snap}}
\* The range deletes among the entries B.
DelsIn(B) == {e \in B : e[1] = "del"}

\* The range deletes of the tables the open moved: in the moved layout, every level-0 table
\* (Demotion::range_deletes). Every table below level 0 was moved into it.
MovedDeletes == IF Layout = "Moved" THEN UNION {DelsIn(l0[j]) : j \in 1..Len(l0)} ELSE {}

\* The layers the open's check looks at, in read order, as replay leaves them: the recovered
\* memtable (what the log held), the level-0 tables newest first, levels 1 to 5, the bottom.
Replayed == <<log>> \o l0 \o <<mid, bottom>>

\* The positions in Replayed of the tables the open moved: every level-0 table in the moved
\* layout, none in the buried one. The check leaves them to the move.
MovedAt == IF Layout = "Moved" THEN 2..(1 + Len(l0)) ELSE {}

\* buried_range_deletes: every range delete of a table that is not moved and has a layer read
\* before it holding an entry of the key.
Candidates ==
  \* The range deletes of the unmoved table layers
  UNION {DelsIn(Replayed[i]) :
           \* that some earlier layer is not empty for.
           i \in {k \in 2..Len(Replayed) \ MovedAt : \E j \in 1..(k - 1) : Replayed[j] # {}}}

----------------------------------------------------------------------------
\* Reads.

\* The layers a read walks, in order: the memtable, the level-0 tables newest first, levels 1 to
\* 5, the bottom.
Sources == <<mem>> \o l0 \o <<mid, bottom>>

\* The newest range delete a snapshot may see in the first i layers of S: what the walk
\* remembers by the time it looks for the key in layer i (a layer's own range deletes count
\* before its keys).
DeleteBy(S, i, snap) == Newest(UNION {Dels(S[j], snap) : j \in 1..i})

\* The newest range delete a snapshot may see in any level-0 table: what the planted pre-scan
\* gathers before the walk starts.
Level0Delete(snap) == Newest(UNION {Dels(l0[j], snap) : j \in 1..Len(l0)})

\* ReadView::walk_newest_first with the point read's visit: the first layer holding a put the
\* snapshot may see answers with its newest one, unless a range delete passed by then is newer.
\* 0 means the key reads as absent.
Read(snap) ==
  \* S: the layers.
  LET S    == Sources
      \* hits: the positions of the layers holding a put the snapshot may see.
      hits == {i \in 1..Len(S) : Puts(S[i], snap) # {}}
  \* No layer holds the key: absent.
  IN IF hits = {} THEN 0
     \* i: the first layer that holds it, where the walk stops.
     ELSE LET i == Min(hits)
              \* p: that layer's newest put the snapshot may see.
              p == Max(Puts(S[i], snap))
              \* d: the range delete the walk compares it with. Under the planted pre-scan,
              \* the newer of what the walk passed and what any level-0 table holds;
              d == IF PreScan THEN Max({DeleteBy(S, i, snap), Level0Delete(snap)})
                   \* in the engine, only what the walk passed.
                   ELSE DeleteBy(S, i, snap)
          \* The put answers if it is newer than that range delete; otherwise the key is deleted.
          IN IF p > d THEN p ELSE 0

\* Every entry the database holds, whichever layer holds it.
Everything == mem \cup UNION {l0[j] : j \in 1..Len(l0)} \cup mid \cup bottom

\* What the sequences say over the entries B: the newest put the snapshot may see, unless a range
\* delete the snapshot may see is newer; 0 when the key is absent or deleted.
ByOrder(B, snap) ==
  \* p: the newest put the snapshot may see in B.
  LET p == Newest(Puts(B, snap))
      \* d: the newest range delete the snapshot may see in B.
      d == Newest(Dels(B, snap))
  \* The put, when it is the newer of the two.
  IN IF p > d THEN p ELSE 0

\* A scan: it gathers every range delete of every source before it compares, so it answers
\* what the sequences of the entries the database holds say.
Scan(snap) == ByOrder(Everything, snap)

\* What a correct read returns: what the sequences of every write ever made say.
Truth(snap) == ByOrder(ever, snap)

----------------------------------------------------------------------------
\* Actions.

\* The start: a 0.1.x database, not open. The key was put at sequence 1 and range-deleted at 2
\* by the ingested table. The put is in a table, or still in the log, or in both (a log replayed
\* again after its writes were flushed).
Init ==
  \* Two sequences are taken: the put's and the range delete's.
  /\ seq = 2
  \* Nothing is in memory: the database is closed.
  /\ mem = {}
  \* The put is in one of three places.
  /\ \E place \in {"table", "log", "both"} :
       \* The log holds the put when it was not flushed, or was and is replayed again.
       /\ log = IF place = "table" THEN {} ELSE {<<"put", 1>>}
       \* The moved layout: level 0 after the move.
       /\ IF Layout = "Moved"
            \* With the put only in the log: the table with the range delete alone; with the
            \* put in a table, that table in front of the one with the range delete. Nothing is
            \* below level 0: every level was moved into it.
            THEN /\ l0 = IF place = "log" THEN <<{<<"del", 2>>}>> ELSE <<{<<"put", 1>>}, {<<"del", 2>>}>>
                 \* Levels 1 to 5 were moved too.
                 /\ mid = {}
                 \* And so was the bottom level.
                 /\ bottom = {}
            \* The buried layout: nothing moved; the put in level 1 when a table holds it, the
            \* range delete in the bottom level.
            ELSE /\ l0 = <<>>
                 \* Level 1 holds the put, when a table does.
                 /\ mid = IF place = "log" THEN {} ELSE {<<"put", 1>>}
                 \* The ingested table, at the bottom.
                 /\ bottom = {<<"del", 2>>}
  \* The manifest records neither a move nor a check.
  /\ recorded = FALSE
  \* The database is not open.
  /\ pc = "closed"
  \* The two writes made so far.
  /\ ever = {<<"put", 1>>, <<"del", 2>>}

\* The open rebuilds the memtable: it replays the log and, when the manifest records neither a
\* move nor a check, moves and checks, and (the fix) copies the moved tables' range deletes and
\* the check's candidates in (recovery::hoist_range_deletes). A manifest that records them
\* gives nothing to copy.
Load ==
  \* The database is closed.
  /\ pc = "closed"
  \* The memtable holds what the log held, and the copies when the open owes them and the fix
  \* is on.
  /\ mem' = log \cup (IF Hoist /\ ~recorded THEN MovedDeletes \cup Candidates ELSE {})
  \* The memtable is rebuilt. A read-only open stops here and reads.
  /\ pc' = "loaded"
  \* Nothing on disk changes.
  /\ UNCHANGED <<seq, log, l0, mid, bottom, recorded, ever>>

\* The open writes the memtable to a fresh log and syncs it
\* (recovery::rewrite_recovered_memtable_to_wal). The engine does this before the manifest
\* records the move and the check; the planted bug does it after.
WriteLog ==
  \* The engine writes the log right after loading; the bug only once the record is written.
  /\ pc = IF RecordFirst THEN "recorded" ELSE "loaded"
  \* The log now holds every entry of the memtable, the copied range deletes included.
  /\ log' = mem
  \* The engine goes on to write the record; under the bug the open is done.
  /\ pc' = IF RecordFirst THEN "open" ELSE "logged"
  \* Nothing else changes.
  /\ UNCHANGED <<seq, mem, l0, mid, bottom, recorded, ever>>

\* The open's first manifest edit records the move and the check, synced (VersionSet::apply).
\* The engine writes it after the log; the planted bug writes it first.
Record ==
  \* The engine records once the log is written; the bug right after loading.
  /\ pc = IF RecordFirst THEN "loaded" ELSE "logged"
  \* From now on no open moves or checks again.
  /\ recorded' = TRUE
  \* The engine's open is done; under the bug the log is still to write.
  /\ pc' = IF RecordFirst THEN "recorded" ELSE "open"
  \* Nothing else changes.
  /\ UNCHANGED <<seq, mem, log, l0, mid, bottom, ever>>

\* The power is cut, at any moment the database is running. Memory is lost; the log, the tables
\* and the manifest keep what was synced, which is all this model ever writes to them.
PowerCut ==
  \* The database is running, or in the middle of its open.
  /\ pc # "closed"
  \* It is not running any more.
  /\ pc' = "closed"
  \* The memtable is gone.
  /\ mem' = {}
  \* Everything on disk stays.
  /\ UNCHANGED <<seq, log, l0, mid, bottom, recorded, ever>>

\* A put of the key at the next sequence, once the database is open: into the memtable and the log.
Put ==
  \* The open has returned.
  /\ pc = "open"
  \* The model may still write.
  /\ seq < MaxSeq
  \* The write takes the next sequence.
  /\ seq' = seq + 1
  \* It is in the memtable,
  /\ mem' = mem \cup {<<"put", seq + 1>>}
  \* and in the log,
  /\ log' = log \cup {<<"put", seq + 1>>}
  \* and among the writes ever made.
  /\ ever' = ever \cup {<<"put", seq + 1>>}
  \* Nothing else changes.
  /\ UNCHANGED <<l0, mid, bottom, recorded, pc>>

\* A flush: the memtable becomes the newest level-0 table, and its log is retired.
Flush ==
  \* The open has returned.
  /\ pc = "open"
  \* There is something to flush.
  /\ mem # {}
  \* The memtable's entries are the newest level-0 table.
  /\ l0' = <<mem>> \o l0
  \* The memtable is empty again,
  /\ mem' = {}
  \* and so is the log: its entries are in the table.
  /\ log' = {}
  \* Nothing else changes.
  /\ UNCHANGED <<seq, mid, bottom, recorded, pc, ever>>

\* A compaction out of level 0 that picks table i: it takes that table and every older one
\* (their keys overlap: they all hold or cover the one key) into levels 1 to 5 (l0_inputs). The
\* tables newer than i stay in level 0.
Compact(i) ==
  \* The open has returned.
  /\ pc = "open"
  \* i names a level-0 table.
  /\ i \in 1..Len(l0)
  \* Table i and the older ones join levels 1 to 5.
  /\ mid' = mid \cup UNION {l0[j] : j \in i..Len(l0)}
  \* The newer ones stay.
  /\ l0' = SubSeq(l0, 1, i - 1)
  \* Nothing else changes.
  /\ UNCHANGED <<seq, mem, log, bottom, recorded, pc, ever>>

\* What a pass writing the bottom level writes from its inputs I. With retirement (E27, the
\* bottom has nothing below it and no snapshot is registered), every range delete is dropped,
\* and with it every older put it deletes among the inputs; the newer puts stay. Without, every
\* input stays.
BottomOut(I) ==
  \* With retirement: only the puts newer than every range delete of the inputs.
  IF Retire THEN {e \in I : e[1] = "put" /\ e[2] > Newest(Dels(I, MaxSeq))}
  \* Without: everything.
  ELSE I

\* A pass that writes the bottom level, taking levels 1 to 5 with it or not (compaction.rs; a
\* pass of a level-5 table elsewhere in the key space that meets the bottom table, as in the
\* story, takes the bottom alone).
BottomPass(withMid) ==
  \* The open has returned.
  /\ pc = "open"
  \* The inputs: the bottom, and levels 1 to 5 when the pass takes them.
  /\ LET I == bottom \cup (IF withMid THEN mid ELSE {})
     \* The bottom level is what the pass writes from them.
     IN bottom' = BottomOut(I)
  \* Levels 1 to 5 are empty when the pass took them; otherwise they stay.
  /\ mid' = IF withMid THEN {} ELSE mid
  \* Nothing else changes.
  /\ UNCHANGED <<seq, mem, log, l0, recorded, pc, ever>>

\* Every step the system can take.
Next ==
  \* The open rebuilds the memtable.
  \/ Load
  \* The open writes the log.
  \/ WriteLog
  \* The open records the move and the check.
  \/ Record
  \* The power is cut.
  \/ PowerCut
  \* The key is put again.
  \/ Put
  \* The memtable is flushed.
  \/ Flush
  \* A level-0 table and the older ones are compacted into levels 1 to 5.
  \/ \E i \in 1..Len(l0) : Compact(i)
  \* A pass writes the bottom level.
  \/ \E withMid \in BOOLEAN : BottomPass(withMid)

\* Every behaviour: start in Init, then take Next steps or stutter.
Spec == Init /\ [][Next]_vars

----------------------------------------------------------------------------
\* Invariants.

\* The entries a layer may hold: a put or a range delete at a sequence handed out.
Entries == {"put", "del"} \X (1..MaxSeq)

\* Every variable holds what its comment says.
TypeOK ==
  \* The sequence stays within the bound.
  /\ seq \in 2..MaxSeq
  \* The memtable and the log hold entries.
  /\ mem \subseteq Entries /\ log \subseteq Entries
  \* Each level-0 table holds entries,
  /\ \A j \in 1..Len(l0) : l0[j] \subseteq Entries
  \* and so do levels 1 to 5 and the bottom,
  /\ mid \subseteq Entries /\ bottom \subseteq Entries
  \* and the writes ever made.
  /\ ever \subseteq Entries
  \* The manifest records the move and the check, or does not.
  /\ recorded \in BOOLEAN
  \* The open is at one of its stages.
  /\ pc \in {"closed", "loaded", "logged", "recorded", "open"}

\* THE HEADLINE, where nothing is ever dropped (Retire = FALSE). Whenever the database can be
\* read (from the moment its memtable is rebuilt, which is where a read-only open reads), every
\* read at every snapshot returns what the sequences say. It rules out the story's case: the key
\* put at 1 and range-deleted at 2 reads as present at snapshot 2, because the walk met the put
\* before the delete. It also rules out the same read turning from deleted to present after a
\* compaction or a power cut. Lean: hoisted_delete_reads_by_sequence.
ReadNewest ==
  \* Once the memtable is rebuilt, for every snapshot up to the newest sequence, the walk
  \* returns what the sequences say.
  pc # "closed" => \A snap \in 0..seq : Read(snap) = Truth(snap)

\* THE HEADLINE where a bottom pass may drop entries (Retire = TRUE): a read at the newest
\* sequence returns what every write ever made says. It rules out the key deleted at 2 reading
\* as present, before a bottom pass and after it.
HeadNewest ==
  \* Once the memtable is rebuilt, the walk at the newest sequence returns what the writes say.
  pc # "closed" => Read(seq) = Truth(seq)

\* A scan at the newest sequence returns what every write ever made says. A scan gathers every
\* range delete the database holds, so it is wrong only once a range delete it needs is gone: it
\* rules out the bottom pass that retires the range delete while the key it deleted stays above.
ScanNewest ==
  \* Once the memtable is rebuilt, a scan at the newest sequence returns what the writes say.
  pc # "closed" => Scan(seq) = Truth(seq)

====
