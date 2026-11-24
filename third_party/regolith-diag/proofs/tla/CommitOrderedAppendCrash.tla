---- MODULE CommitOrderedAppendCrash ----
\* ===========================================================================
\* THE STORY
\* ===========================================================================
\* CommitOrderedAppend.tla shows that a log's entries get the numbers 1, 2,
\* 3, ... with no hole and no number used twice, as long as the machine
\* keeps running. This module asks what a power cut does to that.
\*
\* Every commit is first written to a file, the write-ahead log (Wal,
\* src/engine/wal.rs), and only a "sync" forces the file onto the disk. A
\* power cut loses whatever was written but not yet synced. The database can
\* tell a caller "your commit is done" either only after the sync
\* (DurabilityMode::Immediate, src/options.rs) or before it
\* (DurabilityMode::Eventual, the faster default).
\*
\* Tiny example. Under Eventual, a writer appends entry X and is told "done:
\* X is number 2". A consumer outside the database notes "2 is X". The power
\* is cut before the sync, and entry 2 is gone. After the restart the next
\* writer appends Y, and Y becomes number 2. The log itself is fine: 1, 2,
\* with no hole. But the outside consumer's note is now wrong. Under
\* Immediate nobody is shown number 2 before it is on disk, so this cannot
\* happen.
\*
\* What this model shows: in both modes the log stays dense and without
\* duplicates across power cuts, and a reader that keeps its cursor inside
\* the same database never skips an entry. Under Immediate, in addition,
\* nothing anyone saw is ever lost. Under Eventual a reader that keeps its
\* cursor outside the database can see a number reused: that is the RED.
\*
\* (Plan 3.6 "Durability"; plan 7.1, CommitOrderedAppend.tla item 9.)
\*
\* PROVED FOR EVERY SIZE in Lean:
\*   Regolith/Append.lean, snapshot_sees_prefix
\*       the state after any prefix of the commit order holds exactly the
\*       rows 1..H of the full log, H being that prefix's head: a recovered
\*       prefix is dense, and its rows never change as later commits land
\*   Regolith/Append.lean, dense_from_one, unique_positions, at_most_once
\*       every such prefix is dense from 1, unique, and holds each once key
\*       once
\*   Regolith/WalRecovery.lean, recovers_prefix
\*       recovery yields a gap-free prefix of commit order holding every
\*       durable record (here, a crash keeps such a prefix by construction)
\* TLC checks the invariants below over every interleaving of a few
\* commits, syncs, publications, two readers and a power cut.
\*
\* THE DESIGN (plan 3.6 "Durability", 4.2 and 4.7), and the code.
\*   Each commit's record (its rows, once keys and head) is one WAL record,
\*   at its place in commit order (Log::write_record,
\*   src/engine/wal_log.rs; a sync vouches only for the records with none
\*   missing before them, so "written" here is that prefix). A sync makes
\*   every written record durable (a sync round, src/engine/commit/round.rs).
\*   Publication makes the next record visible; under Immediate durability
\*   only once it is durable, so the visible prefix never passes the durable
\*   one. A power cut keeps a gap-free prefix of the records that holds every
\*   durable one (WalRotation.tla), and recovery makes it all visible. The
\*   next commit after recovery numbers from the recovered head.
\*
\* TWO READERS.
\*   The internal reader keeps its cursor in the same database: it reads
\*   positions (cursor, H] of the visible view, H being the head there, and
\*   commits cursor = H as a record of its own. That record follows every
\*   row it read in the WAL, so a crash that keeps the cursor keeps the rows.
\*   The external reader keeps its cursor and what it has recorded outside
\*   the database, so a crash does not roll them back.
\*
\* WHAT EACH MODE PROMISES (plan 3.6).
\*   Immediate: visible at or below durable, so an acknowledged commit and
\*     the positions anyone saw survive any crash (INV_VisibleDurable,
\*     INV_AckStable, INV_ExternalStable).
\*   Eventual: a crash may drop acknowledged commits and their positions are
\*     assigned again. The log stays dense and unique (INV_Dense,
\*     INV_UniqueCursors, INV_AtMostOnce) and the internal reader never
\*     skips (INV_NoSkip). An external reader can see a position reassigned:
\*     the RED.
\*
\* WHAT THE MODEL LEAVES OUT, and why that loses nothing.
\*   - One writer per record. A group's members share one WAL record, which
\*     a crash keeps or drops whole, so a group is a writer with several
\*     appends; CommitOrderedAppend.tla checks the group's numbering.
\*   - Validation and aborts: an aborted commit writes no record.
\*   - The once key a record sets is read off its rows: the row and
\*     once_key = p are written in the same record.
\*   - A writer whose commit was cut short does not retry; the writers that
\*     have not begun play the commits after recovery.
\*
\* CONFIGURATIONS.
\*   MC_CommitOrderedAppendCrash_Green_Immediate   Durability = "Immediate"
\*     every invariant holds.
\*   MC_CommitOrderedAppendCrash_Green_Eventual    Durability = "Eventual"
\*     INV_Dense, INV_UniqueCursors, INV_AtMostOnce and INV_NoSkip hold. It
\*     is also the RED's teeth: the same constants, every invariant the RED
\*     is not about, over the whole state space.
\*   MC_CommitOrderedAppendCrash_Red_EventualExternal
\*     INV_ExternalStable fails.

\* Borrow the standard TLA+ books: counting numbers, lists, and sizes of finite sets.
EXTENDS Naturals, Sequences, FiniteSets

\* The fixed inputs a configuration chooses.
CONSTANTS
  \* The writers that append one entry and give no once key.
  Singles,
  \* The writers that append one entry with the once key "o1", which they all share.
  SharedOnce,
  \* The writers that append two entries, with no once key.
  Doubles,
  \* When a commit counts as done: "Immediate" (only once it is on disk) or "Eventual" (before
  \* that). DurabilityMode, src/options.rs.
  Durability,
  \* The largest number of power cuts the model injects.
  MaxCrashes

\* Every writer; each commits at most once.
Writers == Singles \cup SharedOnce \cup Doubles

\* The marker for an append made without a once key.
NoOnce == "none"

\* The internal reader's name on its cursor records.
Reader == "reader"

\* The appends writer w makes, in order: the once key passed, or NoOnce.
Appends(w) ==
  \* A single: one append with no once key.
  CASE w \in Singles    -> <<NoOnce>>
    \* A shared-once writer: one append with the once key "o1".
    [] w \in SharedOnce -> <<"o1">>
    \* A double: two appends with no once key.
    [] w \in Doubles    -> <<NoOnce, NoOnce>>

\* Two different shapes share no writer.
ASSUME \A A, B \in {Singles, SharedOnce, Doubles} : A # B => A \cap B = {}
\* The internal reader's name is no writer's name.
ASSUME Reader \notin Writers
\* The durability is one of the two modes, and the power-cut budget is a number.
ASSUME Durability \in {"Immediate", "Eventual"} /\ MaxCrashes \in Nat

\* The state: everything that changes from one step to the next. Each line says what the variable
\* stands for in the real program.
VARIABLES
  \* The write-ahead log: one record per commit, in commit order (Wal, src/engine/wal.rs). A record
  \* lists the rows the commit added and the head it wrote.
  wal,
  \* How many records, counted from the start of the log, a sync has put safely on disk.
  durable,
  \* How many records, counted from the start, readers may see (the engine's visible sequence, a
  \* ReadHorizon in src/engine/read_horizon.rs).
  visible,
  \* Where each writer is: "idle" (not committed), "written" (its record is in the log), "acked"
  \* (its commit returned success), "recovered" (its record survived a power cut before it was
  \* told), "lost" (its record was dropped by a power cut).
  phase,
  \* The writers whose commit returned success. A power cut does not change this: the caller was
  \* told.
  acked,
  \* How many power cuts happened so far.
  crashes,
  \* The entries <<w, i>> the internal reader was handed. It lives outside the database, so a power
  \* cut does not undo it.
  iseen,
  \* The external reader's cursor, kept outside the database.
  xcur,
  \* The <<position, entry>> pairs the external reader wrote down, also outside the database.
  xseen

\* Every variable, so a step that changes none of them is a stutter.
vars == <<wal, durable, visible, phase, acked, crashes, iseen, xcur, xseen>>

\* The greatest element of a finite, nonempty set of naturals.
Max(S) == CHOOSE m \in S : \A n \in S : n <= m

----------------------------------------------------------------------------
\* The database as of the first n records. A record is
\* [kind |-> "commit", w, rows, head] for a writer's commit (head 0 when it
\* assigned nothing) or [kind |-> "cursor", w |-> Reader, rows |-> {},
\* head |-> 0, value] for the internal reader's cursor commit.

\* The rows the first n records hold.
RowsIn(n) == UNION {wal[k].rows : k \in 1..n}

\* The head key after the first n records: the last head any of them wrote.
HeadIn(n) ==
  \* ks: the records among the first n that wrote the head.
  LET ks == {k \in 1..n : wal[k].head # 0}
  \* None: the head is 0. Otherwise the head the last of them wrote.
  IN IF ks = {} THEN 0 ELSE wal[Max(ks)].head

\* Once key o holds a position after the first n records.
OnceTaken(n, o) == \E r \in RowsIn(n) : r.once = o

\* The internal reader's cursor after the first n records: the last value
\* it committed, 0 before its first.
CursorIn(n) ==
  \* ks: the cursor records among the first n.
  LET ks == {k \in 1..n : wal[k].kind = "cursor"}
  \* None: the cursor is 0. Otherwise the value the last of them wrote.
  IN IF ks = {} THEN 0 ELSE wal[Max(ks)].value

\* The rows writer w's commit assigns when the first n records are the
\* decided prefix: positions after the head there, in append order, each
\* append skipped when its once key is taken (there or by an earlier
\* append of w).
RECURSIVE AssignFrom(_, _, _, _, _)
\* Number w's appends from the i-th on. h is the running head, taken the once keys that hold a
\* position, acc the rows so far.
AssignFrom(w, i, h, taken, acc) ==
  \* No append left: the rows so far.
  IF i > Len(Appends(w)) THEN acc
  \* o: this append's once key, or NoOnce.
  ELSE LET o == Appends(w)[i]
       \* Its once key is already taken:
       IN IF o # NoOnce /\ o \in taken
            \* skip it and go on.
            THEN AssignFrom(w, i + 1, h, taken, acc)
            \* Otherwise it takes position h + 1, its once key (if any) is now taken,
            ELSE AssignFrom(w, i + 1, h + 1, taken \cup ({o} \ {NoOnce}),
                            \* and its row joins the rows.
                            acc \cup {[pos |-> h + 1, w |-> w, i |-> i, once |-> o]})
\* The rows writer w's commit gets when the first n records are everything decided before it.
NewRows(w, n) ==
  \* Start from the head and the once keys the first n records left, with no rows yet.
  AssignFrom(w, 1, HeadIn(n), {o \in {"o1"} : OnceTaken(n, o)}, {})

----------------------------------------------------------------------------
\* Actions.

\* The start: an empty log, nothing synced or visible, nobody begun.
Init ==
  \* The log is empty.
  /\ wal     = <<>>
  \* Nothing is on disk.
  /\ durable = 0
  \* Nothing is visible.
  /\ visible = 0
  \* No writer has committed.
  /\ phase   = [w \in Writers |-> "idle"]
  \* No commit returned success.
  /\ acked   = {}
  \* No power cut happened.
  /\ crashes = 0
  \* The internal reader was handed nothing.
  /\ iseen   = {}
  \* The external reader's cursor is 0.
  /\ xcur    = 0
  \* It wrote nothing down.
  /\ xseen   = {}

\* Writer w commits: the ordered step numbers its appends from the decided
\* prefix (every record written so far) and writes its record to the WAL.
Commit(w) ==
  \* The writer has not committed yet.
  /\ phase[w] = "idle"
  \* rs: the rows its appends get, numbered after every record written so far.
  /\ LET rs == NewRows(w, Len(wal))
     \* Its record joins the end of the log: its rows,
     IN wal' = Append(wal, [kind |-> "commit", w |-> w, rows |-> rs,
                            \* and the head it writes (0 when it added no row).
                            head |-> IF rs = {} THEN 0 ELSE Max({r.pos : r \in rs})])
  \* Its record is written.
  /\ phase' = [phase EXCEPT ![w] = "written"]
  \* Nothing else changes.
  /\ UNCHANGED <<durable, visible, acked, crashes, iseen, xcur, xseen>>

\* A sync makes every written record durable.
Sync ==
  \* Some record is not on disk yet.
  /\ durable < Len(wal)
  \* Now every written record is.
  /\ durable' = Len(wal)
  \* Nothing else changes.
  /\ UNCHANGED <<wal, visible, phase, acked, crashes, iseen, xcur, xseen>>

\* The next record is published. Under Immediate it must be durable first.
\* A writer's commit returns success when its record is published.
Publish ==
  \* Some record is not visible yet.
  /\ visible < Len(wal)
  \* Under Immediate, the next one must already be on disk.
  /\ Durability = "Immediate" => visible < durable
  \* One more record is visible.
  /\ visible' = visible + 1
  \* r: that record.
  /\ LET r == wal[visible + 1]
     \* A writer's commit:
     IN IF r.kind = "commit"
          \* the writer is told it succeeded,
          THEN /\ phase' = [phase EXCEPT ![r.w] = "acked"]
               \* and is remembered as told.
               /\ acked' = acked \cup {r.w}
          \* A cursor record: nobody is told anything.
          ELSE UNCHANGED <<phase, acked>>
  \* Nothing else changes.
  /\ UNCHANGED <<wal, durable, crashes, iseen, xcur, xseen>>

\* The internal reader reads positions (cursor, H] of the visible view and
\* commits its new cursor H in the same database. It waits for its last
\* cursor record to be published before reading again.
ReadInternal ==
  \* c: the cursor in the visible view.
  LET c == CursorIn(visible)
      \* h: the head in the visible view.
      h == HeadIn(visible)
  \* There is something past the cursor.
  IN /\ h > c
     \* Its last cursor record, if any, is already visible.
     /\ \A k \in (visible + 1)..Len(wal) : wal[k].kind # "cursor"
     \* It is handed every visible row above its cursor up to the head.
     /\ iseen' = iseen \cup {<<r.w, r.i>> : r \in {x \in RowsIn(visible) : c < x.pos /\ x.pos <= h}}
     \* It writes a cursor record with the new cursor h, as a commit of its own.
     /\ wal' = Append(wal, [kind |-> "cursor", w |-> Reader, rows |-> {}, head |-> 0, value |-> h])
     \* Nothing else changes.
     /\ UNCHANGED <<durable, visible, phase, acked, crashes, xcur, xseen>>

\* The external reader reads positions (xcur, H] of the visible view,
\* records each position with the entry it held, and moves its cursor.
ReadExternal ==
  \* h: the head in the visible view.
  LET h == HeadIn(visible)
  \* There is something past its cursor.
  IN /\ h > xcur
     \* It writes down each visible position above its cursor up to the head, with the entry found
     \* there.
     /\ xseen' = xseen \cup {<<r.pos, <<r.w, r.i>>>> : r \in {x \in RowsIn(visible) : xcur < x.pos /\ x.pos <= h}}
     \* Its cursor moves to the head.
     /\ xcur' = h
     \* Nothing in the database changes.
     /\ UNCHANGED <<wal, durable, visible, phase, acked, crashes, iseen>>

\* A power cut. It keeps a gap-free prefix of the records holding every
\* durable one, and recovery publishes all of it. A writer whose record was
\* dropped lost its commit (acknowledged or not); one whose unpublished
\* record survived is recovered without ever being told. The external
\* reader's state and the internal reader's handed entries are outside the
\* database and survive.
Crash ==
  \* The power-cut budget is not used up.
  /\ crashes < MaxCrashes
  \* The log is cut at some record k: nothing on disk is lost, anything after may be.
  /\ \E k \in durable..Len(wal) :
       \* Only the first k records remain.
       /\ wal'     = SubSeq(wal, 1, k)
       \* All of them are on disk now.
       /\ durable' = k
       \* Recovery makes all of them visible.
       /\ visible' = k
       \* Each writer's phase after the cut:
       /\ phase'   = [w \in Writers |->
                        \* one that had written a record, told or not:
                        IF phase[w] \in {"written", "acked"}
                          \* if its record survived,
                          THEN IF \E j \in 1..k : wal[j].w = w
                                 \* it is "recovered" when it was never told, and stays "acked" when
                                 \* it was;
                                 THEN IF phase[w] = "written" THEN "recovered" ELSE "acked"
                                 \* if its record was dropped, it is "lost".
                                 ELSE "lost"
                          \* Any other writer stays as it was.
                          ELSE phase[w]]
  \* One more power cut happened.
  /\ crashes' = crashes + 1
  \* Who was told, and everything the readers keep outside the database, stays.
  /\ UNCHANGED <<acked, iseen, xcur, xseen>>

\* Every step the system can take.
Next ==
  \* A writer commits.
  \/ \E w \in Writers : Commit(w)
  \* The log is synced.
  \/ Sync
  \* The next record is published.
  \/ Publish
  \* The internal reader reads.
  \/ ReadInternal
  \* The external reader reads.
  \/ ReadExternal
  \* The power is cut.
  \/ Crash

\* Every behaviour: start in Init, then take Next steps or stutter.
Spec == Init /\ [][Next]_vars

----------------------------------------------------------------------------
\* Invariants.

\* Every variable holds what its comment says.
TypeOK ==
  \* The on-disk count is at most the log's length.
  /\ durable \in 0..Len(wal)
  \* The visible count is at most the log's length.
  /\ visible \in 0..Len(wal)
  \* Every writer is in one of the five phases.
  /\ phase \in [Writers -> {"idle", "written", "acked", "recovered", "lost"}]
  \* Only writers are told.
  /\ acked \subseteq Writers
  \* No more power cuts than the budget.
  /\ crashes \in 0..MaxCrashes
  \* What the internal reader was handed names a writer and its first or second append.
  /\ iseen \subseteq Writers \X (1..2)
  \* The external cursor is a number.
  /\ xcur \in Nat

\* The visible view with head H holds rows at exactly positions 1..H, after
\* any crash too. Lean: dense_from_one and snapshot_sees_prefix.
\* Rules out, for example: after a power cut the log holds entries 1 and 3 but not 2.
INV_Dense == {r.pos : r \in RowsIn(visible)} = 1..HeadIn(visible)

\* No two rows anywhere in the log share a position. Lean: unique_positions.
\* Rules out, for example: a power cut drops entry 2, the next commit writes a new entry 2, and the
\* old entry 2 is somehow still there beside it.
INV_UniqueCursors == \A a, b \in RowsIn(Len(wal)) : a.pos = b.pos => a = b

\* No two rows anywhere in the log carry one once key. Lean: at_most_once.
INV_AtMostOnce ==
  \* Rules out, for example: a once key's entry is lost in a power cut and yet the log later holds
  \* two entries for that key.
  \A a, b \in RowsIn(Len(wal)) : a.once # NoOnce /\ a.once = b.once => a = b

\* The internal reader, whose cursor commits in the same database, has been
\* handed every visible row at or below its cursor, across crashes.
INV_NoSkip ==
  \* Rules out, for example: a power cut keeps the reader's cursor record "I read up to 3" but loses
  \* entry 3, and a new entry 3 appears that the reader never looks at.
  \A r \in RowsIn(visible) : r.pos <= CursorIn(visible) => <<r.w, r.i>> \in iseen

\* Immediate: nothing is visible that is not durable.
\* Rules out, for example: a reader sees entry 2, the power is cut before entry 2 reached the disk,
\* and entry 2 is gone.
INV_VisibleDurable == visible <= durable

\* Immediate: every commit that returned success is still in the visible
\* view, so the positions it was given still hold its entries.
INV_AckStable ==
  \* Rules out, for example: a commit returns success, the power is cut, and after restart its entry
  \* is not in the log.
  \A w \in acked : \E k \in 1..visible : wal[k].kind = "commit" /\ wal[k].w = w

\* A position the external reader recorded never holds another entry.
INV_ExternalStable ==
  \* Rules out, for example: an outside consumer notes "position 2 is entry X", a power cut drops X,
  \* and the next commit puts entry Y at position 2. Under Immediate this cannot happen; under
  \* Eventual it can, which is the RED.
  \A pr \in xseen : \A r \in RowsIn(visible) : r.pos = pr[1] => <<r.w, r.i>> = pr[2]

====
