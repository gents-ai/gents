---- MODULE LogTruncate ----
\* ===========================================================================
\* THE STORY
\* ===========================================================================
\* A commit-ordered log is a numbered list of entries, 1, 2, 3, ... that only
\* grows: Transaction::append gives each new entry the next position after
\* the log's head (CommitOrderedAppend.tla). A program that has finished with
\* the front of the log wants to drop it, or the log grows for ever.
\* Transaction::truncate_log(log, lo..=hi) does that inside a transaction:
\* at commit it deletes the entry key of every position in lo..=hi, in the
\* same atomic commit as the transaction's other writes, and nothing else.
\* The head key stays where it is, so the next entry is still numbered from
\* the head.
\*
\* What makes it safe is a plain fact about the engine, not new machinery:
\*   - the delete of a key that another commit already deleted is no
\*     conflict (the commit finds the key already holds what it would
\*     write; write_check.rs WriteCheck::Elided), so two truncates that
\*     overlap both commit;
\*   - the truncate reads the head in its snapshot and refuses a range past
\*     it; an append only ever writes positions past the head the
\*     truncating transaction could see (it numbers from the newest head),
\*     so no entry a truncate deletes was written after its snapshot, and a
\*     truncate and an append never touch a key in common;
\*   - the head read is not recorded, so a concurrent append that moves the
\*     head does not invalidate it;
\*   - a delete is a version like any other, so a snapshot taken before the
\*     truncating commit still reads the entries, until nothing pins them
\*     and a compaction removes them.
\*
\* Tiny example. The log holds entries 1 and 2 (head 2). Writer A begins and
\* truncates 1..=2. Writer B begins and appends one entry. Both commit, in
\* either order: B's entry takes position 3, A deletes entries 1 and 2, the
\* head says 3. A reader that pinned its snapshot before either commit still
\* reads entries 1 and 2.
\*
\* What can go wrong, and what this model checks cannot happen:
\*   - a truncate or an append fails a commit (INV_NoConflict);
\*   - the live entries are not exactly the positions 1..head less every
\*     position some commit truncated (INV_LiveLaw);
\*   - a truncate drops a position that did not exist in its snapshot
\*     (INV_WithinSeenHead);
\*   - the head moves back, so positions are given out twice
\*     (INV_HeadMonotone);
\*   - a reader sees something other than the log as of its snapshot
\*     (INV_ReaderStable);
\*   - a truncate rolled back to a savepoint is applied anyway
\*     (INV_RollbackHonoured).
\*
\* The data-structure law, for any number of commits and any size, is proved
\* in proofs/lean/Regolith/LogTruncate.lean: live_is_head_less_truncated,
\* snapshot_reads_as_of, truncate_never_conflicts, truncate_commutes and
\* truncate_idempotent. TLC checks
\* the interleavings of a few commits, which is where a conflict or a lost
\* reader would show.
\*
\* WHAT IS MODELLED, and the code each piece mirrors (src/ unless named):
\*   | Model            | Rust                                                   |
\*   |------------------|--------------------------------------------------------|
\*   | Begin, snap      | transaction.rs:393 and :511, register_snapshot_at_     |
\*   |                  | horizon: the snapshot_seq every read is served at      |
\*   | AppendOp         | transaction/append.rs:146 Transaction::append: queued  |
\*   |                  | only, numbered at commit                               |
\*   | TruncateOp       | transaction/truncate.rs:229 Transaction::truncate_log  |
\*   | HeadAt(snap[w])  | truncate.rs:259 read_log_head: an unrecorded read at   |
\*   |  in the guard    | the snapshot (read_horizon), never validated           |
\*   | the guard        | truncate.rs:240 `end > head` -> Error::InvalidArgument |
\*   | dels             | truncate.rs:250 the Write::Delete of each entry key in |
\*   |                  | the write buffer (`self.writes`)                       |
\*   | Rollback         | transaction.rs:1800 rollback_to_savepoint, callbacks.  |
\*   |                  | rs:628 roll_back_to: the buffer shrinks to the mark    |
\*   | Commit, conflict | engine/commit/txn.rs:272 the write loop calling        |
\*   |  test (clash)    | check_write, engine/commit/write_check.rs:47: a newer  |
\*   |                  | version of a written key conflicts unless the write    |
\*   |                  | stores what the key holds (:87 WriteCheck::Elided,     |
\*   |                  | counted at txn.rs:282)                                 |
\*   | Commit, numbering| engine/commit/append.rs:103 AppendOrder::number, :216  |
\*   |  (fresh, head)   | head_of: the next position after the newest head, in   |
\*   |                  | the ordered step                                       |
\*   | vers, headVers   | the versions of each entry key and of the head key in  |
\*   |                  | the memtables and tables (engine/mod.rs:2735           |
\*   |                  | latest_version_in_view walks them newest first)        |
\*   | Pin, ReadEntry,  | Db::snapshot, Snapshot::get: the newest version at or  |
\*   |  VisibleAt       | below the snapshot's sequence                          |
\*   | Compact, Floor   | engine/compaction/ keeps the newest version at or      |
\*   |                  | below the oldest live snapshot, engine/snapshot_       |
\*   |                  | registry.rs:321 oldest_live_seq                        |
\*   | PlainPut         | transaction.rs:1568 Transaction::put, refused by       |
\*   |                  | transaction/append.rs:328 refuse_log_key               |
\*   | hist, wanted,    | ghost: the log as of each sequence, what the caller    |
\*   |  got, conflicted | still wants dropped, what the commit dropped, whether  |
\*   |                  | a commit conflicted; no Rust counterpart               |
\*   | INV_NoConflict   | tests/commit_ordered_truncate.rs                       |
\*   |                  | a_truncate_is_never_a_conflict_at_any_level_in_either_ |
\*   |                  | flavor                                                 |
\*   | INV_LiveLaw,     | the same file: a_truncate_drops_exactly_the_entries_of_ |
\*   |  INV_Head...     | its_range, a_truncate_leaves_the_head_and_the_once_    |
\*   |                  | keys_alone_so_positions_keep_counting                  |
\*   | INV_ReaderStable | a_snapshot_or_transaction_begun_before_the_truncate_   |
\*   |                  | still_reads_every_entry                                |
\*   | INV_WithinSeen.. | a_range_past_the_head_the_transaction_reads_is_        |
\*   |                  | refused_and_buffers_nothing                            |
\*   | INV_Rollback...  | rolling_back_to_a_savepoint_drops_the_truncates_made_  |
\*   |                  | after_it                                               |
\*
\* WHAT IS LEFT OUT, and why that loses nothing:
\*   - Pessimistic transactions: truncate_log takes no lock and buffers the
\*     same deletes, and a pessimistic commit validates no write, so there is
\*     nothing it could conflict on. tests/commit_ordered_truncate.rs runs it.
\*   - Isolation levels: no level validates a truncate's writes any
\*     differently, and the head read is unrecorded at all of them; the test
\*     runs all five.
\*   - Durability and crashes: a truncate is one atomic commit, so a crash
\*     keeps it or loses it whole, as for any commit (CommitOrderedAppendCrash.tla).
\*   - The layout contract (entry_key differs for every position, the head
\*     key is no entry key): a position is its key here.
\*   - The once keys: truncate_log never touches them.
\*   - Several truncates in one transaction: each is the same buffered
\*     deletes, and Lean proves ranges compose (truncate_commutes).
\*
\* CONFIGURATIONS (each GREEN must pass, each RED must break the one promise
\* it names):
\*   MC_LogTruncate_Green              two writers and a reader, every
\*                                     promise holds.
\*   MC_LogTruncate_Green_Three        three writers, no reader: more commits
\*                                     in every order.
\*   MC_LogTruncate_Red_OrdinaryWrite  a delete is validated like an ordinary
\*                                     write, with no elision of an
\*                                     already-deleted key: two overlapping
\*                                     truncates conflict. INV_NoConflict.
\*   MC_LogTruncate_Red_ReadHead       the head read is recorded and
\*                                     validated: an append beside a truncate
\*                                     conflicts. INV_NoConflict.
\*   MC_LogTruncate_Red_NoHeadGuard    a range past the head is accepted:
\*                                     INV_WithinSeenHead.
\*   MC_LogTruncate_Red_NoHeadGuard_Conflict  the same bug, seen through its
\*                                     effect on an append: INV_NoConflict.
\*   MC_LogTruncate_Red_OffByOne       a truncate also drops the next
\*                                     position: INV_LiveLaw.
\*   MC_LogTruncate_Red_RewindsHead    a truncate to the head lowers it:
\*                                     INV_HeadMonotone.
\*   MC_LogTruncate_Red_ErasesOnTruncate  the commit erases the versions of
\*                                     the entries instead of adding a
\*                                     delete: INV_ReaderStable.
\*   MC_LogTruncate_Red_CompactIgnoresPins  compaction drops versions a
\*                                     snapshot still needs, as a
\*                                     CompactionFilter would:
\*                                     INV_ReaderStable.
\*   MC_LogTruncate_Red_RollbackKeepsTruncate  a rollback to a savepoint
\*                                     leaves the truncate buffered:
\*                                     INV_RollbackHonoured.
\*   MC_LogTruncate_Red_OpenKeys       a plain put of a log key is accepted:
\*                                     INV_LiveLaw.

\* Numbers, sets and sequences.
EXTENDS Naturals, FiniteSets, Sequences

\* The fixed inputs of a configuration.
CONSTANTS
  \* "none" for the real code, or the name of one planted bug.
  Mutant,
  \* The transactions that write: they append and truncate.
  Writers,
  \* The readers that pin a snapshot and read entries.
  Readers,
  \* The largest position the log can reach in this run.
  MaxPos,
  \* The most appends one transaction makes.
  MaxApp,
  \* The newest commit sequence this run reaches.
  MaxSeq

\* The bug names this model knows.
ASSUME Mutant \in {"none", "OrdinaryWrite", "ReadHead", "NoHeadGuard", "OffByOne",
                   "RewindsHead", "ErasesOnTruncate", "CompactIgnoresPins",
                   "RollbackKeepsTruncate", "OpenKeys"}

\* The log starts with positions 1 and 2, so it must have room for them.
ASSUME MaxPos >= 2

\* The positions the log can hold.
Positions == 1..MaxPos

\* The state that changes from step to step.
VARIABLES
  \* The newest committed sequence: how many commits the database has made.
  \* Mirrors the engine's visible sequence.
  seq,
  \* For each position, the versions of its entry key: a set of pairs
  \* <<sequence, "put" or "del">>. Mirrors the entry key's versions in the
  \* memtables and tables.
  vers,
  \* The versions of the head key: a set of pairs <<sequence, value>>. It is
  \* only ever put. Mirrors the head key's versions.
  headVers,
  \* Ghost. The log as of each sequence 1, 2, ...: a record of its head, its
  \* live entries and every position some commit truncated. No Rust
  \* counterpart; the invariants compare the store with it.
  hist,
  \* Where each writer is: "idle", "open" (a transaction begun), "done" or
  \* "failed" (its commit conflicted). Mirrors the transaction's state.
  pc,
  \* The sequence each open writer's snapshot is at. Mirrors snapshot_seq.
  snap,
  \* How many appends each writer has queued. Mirrors the appends buffer.
  app,
  \* The positions each writer has buffered a delete for. Mirrors the
  \* Write::Delete entries truncate_log put in the write buffer.
  dels,
  \* Ghost. The positions each writer still wants dropped: what the caller
  \* would say after its rollbacks. No Rust counterpart.
  wanted,
  \* Ghost. The positions each writer's commit actually deleted.
  got,
  \* The sequence each reader has pinned, 0 while it has none. Mirrors a
  \* Snapshot.
  rsnap,
  \* What each reader has read of each position: "unread", "yes" (the entry
  \* was there) or "no".
  rseen,
  \* Ghost. Whether any commit was refused for a conflict.
  conflicted

\* All of them, for "nothing else changes".
vars == <<seq, vers, headVers, hist, pc, snap, app, dels, wanted, got, rsnap, rseen,
          conflicted>>

\* ---------------------------------------------------------------------------
\* Small helpers
\* ---------------------------------------------------------------------------

\* The pair with the largest sequence in a non-empty set of pairs.
NewestOf(vs) == CHOOSE v \in vs : \A u \in vs : u[1] <= v[1]

\* The largest member of a non-empty set of numbers.
MaxOf(s) == CHOOSE x \in s : \A y \in s : y <= x

\* The smallest member of a non-empty set of numbers.
MinOf(s) == CHOOSE x \in s : \A y \in s : x <= y

\* Whether position p's entry is there for a read at sequence s: the newest
\* version at or below s is a put.
VisibleAt(v, p, s) ==
  \* The versions a read at s may see.
  LET vs == {x \in v[p] : x[1] <= s}
  \* None: no entry. Otherwise the newest decides.
  IN IF vs = {} THEN FALSE ELSE NewestOf(vs)[2] = "put"

\* The head a read at sequence s finds: the newest head version at or below
\* it, 0 while the log is empty.
HeadAt(s) ==
  \* The head versions a read at s may see.
  LET hs == {x \in headVers : x[1] <= s}
  \* None: the log is empty. Otherwise the newest value.
  IN IF hs = {} THEN 0 ELSE NewestOf(hs)[2]

\* The snapshots something still reads at: every open writer's and every
\* pinned reader's.
Pins ==
  \* The writers that have begun and not ended.
  {snap[w] : w \in {x \in Writers : pc[x] = "open"}}
  \* The readers that hold a snapshot.
  \cup {rsnap[r] : r \in {x \in Readers : rsnap[x] # 0}}

\* The newest sequence a compaction may collapse versions up to: the oldest
\* pinned snapshot, or the newest commit when nothing is pinned.
Floor ==
  \* The bug: compaction does not look at what is pinned.
  IF Mutant = "CompactIgnoresPins" \/ Pins = {} THEN seq
  \* The oldest pinned snapshot, or the newest commit if that is older.
  ELSE MinOf(Pins \cup {seq})

\* The live entries of a store of versions v: the positions whose newest
\* version is a put.
LiveOf(v) ==
  \* A position with a version whose newest one is a put.
  {p \in Positions : v[p] # {} /\ NewestOf(v[p])[2] = "put"}

\* The record of the log after a commit that left the store v and head hd,
\* given the positions truncated so far.
Record(v, hd, dr) == [head |-> hd, live |-> LiveOf(v), dropped |-> dr]

\* ---------------------------------------------------------------------------
\* The start
\* ---------------------------------------------------------------------------

\* The log holds entries 1 and 2, written by commit 1; nothing is open.
Init ==
  \* One commit has happened.
  /\ seq = 1
  \* Positions 1 and 2 were put by commit 1; the others have no version.
  /\ vers = [p \in Positions |-> IF p <= 2 THEN {<<1, "put">>} ELSE {}]
  \* The head was put to 2 by commit 1.
  /\ headVers = {<<1, 2>>}
  \* The log as of sequence 1: head 2, entries 1 and 2, nothing truncated.
  /\ hist = <<[head |-> 2, live |-> {1, 2}, dropped |-> {}]>>
  \* No writer has begun.
  /\ pc = [w \in Writers |-> "idle"]
  \* No snapshot yet.
  /\ snap = [w \in Writers |-> 0]
  \* No append queued.
  /\ app = [w \in Writers |-> 0]
  \* No delete buffered.
  /\ dels = [w \in Writers |-> {}]
  \* Nothing wanted dropped.
  /\ wanted = [w \in Writers |-> {}]
  \* Nothing dropped by a commit.
  /\ got = [w \in Writers |-> {}]
  \* No reader holds a snapshot.
  /\ rsnap = [r \in Readers |-> 0]
  \* No reader has read anything.
  /\ rseen = [r \in Readers |-> [p \in Positions |-> "unread"]]
  \* Nothing has conflicted.
  /\ conflicted = FALSE

\* ---------------------------------------------------------------------------
\* A writer
\* ---------------------------------------------------------------------------

\* A writer begins a transaction: its snapshot is the newest commit.
Begin(w) ==
  \* It has not begun.
  /\ pc[w] = "idle"
  \* It is open.
  /\ pc' = [pc EXCEPT ![w] = "open"]
  \* Its snapshot is the newest commit.
  /\ snap' = [snap EXCEPT ![w] = seq]
  \* Nothing else changes.
  /\ UNCHANGED <<seq, vers, headVers, hist, app, dels, wanted, got, rsnap, rseen, conflicted>>

\* A writer queues an append. Nothing is written until commit.
AppendOp(w) ==
  \* It is open and has room for another append.
  /\ pc[w] = "open"
  \* At most MaxApp appends each.
  /\ app[w] < MaxApp
  \* One more append is queued.
  /\ app' = [app EXCEPT ![w] = app[w] + 1]
  \* Nothing else changes.
  /\ UNCHANGED <<seq, vers, headVers, hist, pc, snap, dels, wanted, got, rsnap, rseen, conflicted>>

\* A writer calls truncate_log(lo..=hi). The call reads the head in its
\* snapshot and refuses a range past it; otherwise it buffers a delete of
\* each position's entry key.
TruncateOp(w, lo, hi) ==
  \* It is open and has not buffered a truncate.
  /\ pc[w] = "open"
  \* Only one truncate per transaction.
  /\ dels[w] = {}
  \* A range of existing positions: start no later than end.
  /\ lo <= hi
  \* The guard: the range stays within the head this transaction reads.
  \* The bug (NoHeadGuard) accepts a range past it.
  /\ Mutant = "NoHeadGuard" \/ hi <= HeadAt(snap[w])
  \* The deletes are buffered.
  /\ dels' = [dels EXCEPT ![w] = lo..hi]
  \* The caller wants them dropped.
  /\ wanted' = [wanted EXCEPT ![w] = lo..hi]
  \* Nothing else changes.
  /\ UNCHANGED <<seq, vers, headVers, hist, pc, snap, app, got, rsnap, rseen, conflicted>>

\* A writer rolls back to a savepoint taken before its truncate: the buffered
\* deletes made after it are dropped.
Rollback(w) ==
  \* It is open and has a truncate buffered.
  /\ pc[w] = "open"
  \* It has deletes buffered.
  /\ dels[w] # {}
  \* The caller no longer wants the range dropped.
  /\ wanted' = [wanted EXCEPT ![w] = {}]
  \* The buffer shrinks, unless the bug leaves the deletes in it.
  /\ dels' = IF Mutant = "RollbackKeepsTruncate" THEN dels ELSE [dels EXCEPT ![w] = {}]
  \* Nothing else changes.
  /\ UNCHANGED <<seq, vers, headVers, hist, pc, snap, app, got, rsnap, rseen, conflicted>>

\* A writer commits. The ordered step checks its written keys against the
\* versions committed after its snapshot; if none conflicts, it numbers the
\* appends from the newest head and applies the deletes, in one commit.
Commit(w) ==
  \* It is open, and there is a sequence left for its commit.
  /\ pc[w] = "open"
  \* The run has a sequence left to give.
  /\ seq < MaxSeq
  \* The commit, worked out step by step.
  /\ LET
       \* The newest head: the ordered step numbers from it.
       head == HeadAt(seq)
       \* How many entries the commit appends.
       n == app[w]
       \* The positions the appends take: one past the head, in order.
       fresh == (head + 1)..(head + n)
       \* Whether position p has a version newer than the snapshot.
       newer(p) == \E x \in vers[p] : x[1] > snap[w]
       \* A written key conflicts when a newer version exists and the write
       \* would not store what the key already holds. A delete of a key whose
       \* newest version is a delete stores the same: elided. The bug
       \* (OrdinaryWrite) has no elision.
       clash(p) == newer(p) /\ (Mutant = "OrdinaryWrite" \/ NewestOf(vers[p])[2] = "put")
       \* The deletes that conflict.
       writeConflict == \E p \in dels[w] : clash(p)
       \* The bug (ReadHead): the head read is recorded, so a head that moved
       \* since the snapshot conflicts.
       readConflict == Mutant = "ReadHead" /\ dels[w] # {} /\ HeadAt(seq) # HeadAt(snap[w])
       \* The sequence the commit takes.
       s == seq + 1
       \* The bug (OffByOne): the next position goes too.
       extra ==
         \* The bug: one more position, when the log has it.
         IF Mutant = "OffByOne" /\ dels[w] # {} /\ MaxOf(dels[w]) + 1 <= head
           \* The position after the range.
           THEN {MaxOf(dels[w]) + 1}
           \* Otherwise none.
           ELSE {}
       \* The positions that get a delete version: not one an append of this
       \* commit takes (a put wins there; only the NoHeadGuard bug gets here).
       gone == (dels[w] \cup extra) \ fresh
       \* The versions after the commit.
       newVers ==
         \* Each position keeps its versions and gains a put (an append) or a
         \* delete (a truncate). The bug (ErasesOnTruncate) erases them all.
         [p \in Positions |->
            IF p \in gone /\ Mutant = "ErasesOnTruncate" THEN {<<s, "del">>}
            ELSE (vers[p] \cup (IF p \in fresh THEN {<<s, "put">>} ELSE {})
                        \cup (IF p \in gone THEN {<<s, "del">>} ELSE {}))]
       \* The head after the commit: moved by the appends. The bug
       \* (RewindsHead): a truncate that reaches the head lowers it.
       newHead ==
         \* Appends move the head up by their number.
         IF n > 0 THEN head + n
         \* The bug: a truncate through the head moves it to before the range.
         ELSE IF Mutant = "RewindsHead" /\ dels[w] # {} /\ MaxOf(dels[w]) = head
                \* The head becomes the position before the range.
                THEN MinOf(dels[w]) - 1
                \* Otherwise the head stays.
                ELSE head
     \* The commit is one of three things.
     IN
       \* A conflict: the commit fails and writes nothing.
       IF writeConflict \/ readConflict
         \* The writer ends as failed.
         THEN /\ pc' = [pc EXCEPT ![w] = "failed"]
              \* The ghost flag records it.
              /\ conflicted' = TRUE
              \* Nothing else changes.
              /\ UNCHANGED <<seq, vers, headVers, hist, snap, app, dels, wanted, got, rsnap,
                             rseen>>
       \* A transaction with nothing to write ends without taking a sequence.
       ELSE IF dels[w] = {} /\ n = 0
         \* The writer is done.
         THEN /\ pc' = [pc EXCEPT ![w] = "done"]
              \* Nothing else changes.
              /\ UNCHANGED <<seq, vers, headVers, hist, snap, app, dels, wanted, got, rsnap,
                             rseen, conflicted>>
       \* Otherwise the commit takes the next sequence and applies everything.
       ELSE /\ head + n <= MaxPos
            \* The writer is done.
            /\ pc' = [pc EXCEPT ![w] = "done"]
            \* The commit took a sequence.
            /\ seq' = s
            \* The versions of the entry keys.
            /\ vers' = newVers
            \* The head key gains a version only when it moved.
            /\ headVers' = IF newHead # head THEN headVers \cup {<<s, newHead>>} ELSE headVers
            \* The ghost log gains this commit's record; the truncated positions
            \* are those the caller asked for.
            /\ hist' = Append(hist, Record(newVers, newHead, hist[seq].dropped \cup dels[w]))
            \* The commit's deletes are recorded.
            /\ got' = [got EXCEPT ![w] = dels[w]]
            \* Nothing else changes.
            /\ UNCHANGED <<snap, app, dels, wanted, rsnap, rseen, conflicted>>

\* ---------------------------------------------------------------------------
\* A caller that breaks the contract
\* ---------------------------------------------------------------------------

\* A plain put of a log entry key by a writer that is not append or truncate.
\* regolith refuses it (Error::LogKeyWrite) where the classifier applies, so
\* this step exists only in the OpenKeys bug, where the refusal is gone.
PlainPut(p) ==
  \* Only the bug lets it through.
  /\ Mutant = "OpenKeys"
  \* There is a sequence left, and the position is one the log has given out.
  /\ seq < MaxSeq
  \* The position is one the log has given out.
  /\ p <= HeadAt(seq)
  \* The put is a new version of the entry key.
  /\ vers' = [vers EXCEPT ![p] = vers[p] \cup {<<seq + 1, "put">>}]
  \* It took a sequence.
  /\ seq' = seq + 1
  \* The ghost log records it: the head and the truncated set are as before.
  /\ hist' = Append(hist, Record(vers', HeadAt(seq), hist[seq].dropped))
  \* Nothing else changes.
  /\ UNCHANGED <<headVers, pc, snap, app, dels, wanted, got, rsnap, rseen, conflicted>>

\* ---------------------------------------------------------------------------
\* A reader
\* ---------------------------------------------------------------------------

\* A reader takes a snapshot at the newest commit.
Pin(r) ==
  \* It holds none.
  /\ rsnap[r] = 0
  \* Now it does.
  /\ rsnap' = [rsnap EXCEPT ![r] = seq]
  \* Nothing else changes.
  /\ UNCHANGED <<seq, vers, headVers, hist, pc, snap, app, dels, wanted, got, rseen, conflicted>>

\* A reader reads one position's entry in its snapshot.
ReadEntry(r, p) ==
  \* It holds a snapshot and has not read this position.
  /\ rsnap[r] # 0
  \* Not read yet.
  /\ rseen[r][p] = "unread"
  \* It records whether it found the entry.
  /\ rseen' = [rseen EXCEPT ![r][p] = IF VisibleAt(vers, p, rsnap[r]) THEN "yes" ELSE "no"]
  \* Nothing else changes.
  /\ UNCHANGED <<seq, vers, headVers, hist, pc, snap, app, dels, wanted, got, rsnap, conflicted>>

\* A reader lets go of its snapshot and forgets what it read.
Unpin(r) ==
  \* It holds one.
  /\ rsnap[r] # 0
  \* It holds none.
  /\ rsnap' = [rsnap EXCEPT ![r] = 0]
  \* Nothing read.
  /\ rseen' = [rseen EXCEPT ![r] = [p \in Positions |-> "unread"]]
  \* Nothing else changes.
  /\ UNCHANGED <<seq, vers, headVers, hist, pc, snap, app, dels, wanted, got, conflicted>>

\* ---------------------------------------------------------------------------
\* Compaction
\* ---------------------------------------------------------------------------

\* A compaction drops the versions of one entry key that are older than the
\* newest version at or below the floor: nothing pinned can read them.
Compact(p) ==
  \* The versions a read at the floor may see.
  /\ LET vs == {x \in vers[p] : x[1] <= Floor}
     \* The versions it keeps.
     IN
       \* There is one, and some older version can go.
       /\ vs # {}
       \* Some version is older than the newest one at or below the floor.
       /\ \E x \in vers[p] : x[1] < NewestOf(vs)[1]
       \* Only the newest one at or below the floor, and everything newer, stay.
       /\ vers' = [vers EXCEPT ![p] = {x \in vers[p] : x[1] >= NewestOf(vs)[1]}]
  \* Nothing else changes.
  /\ UNCHANGED <<seq, headVers, hist, pc, snap, app, dels, wanted, got, rsnap, rseen, conflicted>>

\* ---------------------------------------------------------------------------
\* The next step
\* ---------------------------------------------------------------------------

\* Any one step.
Next ==
  \* A writer begins, appends, truncates, rolls back or commits.
  \/ \E w \in Writers : Begin(w) \/ AppendOp(w) \/ Rollback(w) \/ Commit(w)
  \* A writer truncates some range.
  \/ \E w \in Writers, lo \in Positions, hi \in Positions : TruncateOp(w, lo, hi)
  \* A reader pins, reads or lets go.
  \/ \E r \in Readers : Pin(r) \/ Unpin(r)
  \* A reader reads one position.
  \/ \E r \in Readers, p \in Positions : ReadEntry(r, p)
  \* A compaction runs over one entry key.
  \/ \E p \in Positions : Compact(p)
  \* A caller breaks the closed-keys contract.
  \/ \E p \in Positions : PlainPut(p)

\* The runs checked: start in Init, then take steps of Next.
Spec == Init /\ [][Next]_vars

\* ---------------------------------------------------------------------------
\* What must always hold
\* ---------------------------------------------------------------------------

\* Every variable holds the kind of value its comment says.
TypeOK ==
  \* The sequence is a count of commits.
  /\ seq \in 1..MaxSeq
  \* The ghost log has one record per commit.
  /\ Len(hist) = seq
  \* A writer is in one of four states.
  /\ pc \in [Writers -> {"idle", "open", "done", "failed"}]
  \* Every buffered delete is a position.
  /\ \A w \in Writers : dels[w] \subseteq Positions
  \* Every reader's read is one of three answers.
  /\ \A r \in Readers : \A p \in Positions : rseen[r][p] \in {"unread", "yes", "no"}

\* INV_NoConflict: no append and no truncate ever fails a commit. A truncate
\* deletes only entries that existed in its snapshot, an append writes only
\* past the head, and the head read is not validated.
INV_NoConflict == ~conflicted

\* INV_LiveLaw: at every commit the live entries are exactly the positions up
\* to the head less every position some commit truncated. This is the law the
\* Lean file proves for every sequence of appends and truncates.
INV_LiveLaw ==
  \* At every sequence of the ghost log.
  \A s \in 1..Len(hist) :
    \* The live entries are the positions to the head, less the truncated ones.
    hist[s].live = (1..hist[s].head) \ hist[s].dropped

\* INV_WithinSeenHead: a truncate that commits deleted only positions that
\* existed in its snapshot, that is, at or below the head it read.
INV_WithinSeenHead ==
  \* For each writer that committed.
  \A w \in Writers : pc[w] = "done" => got[w] \subseteq 1..HeadAt(snap[w])

\* INV_HeadMonotone: the head never moves back, so a position is never given
\* out twice, whatever was truncated.
INV_HeadMonotone ==
  \* Each commit's head is at least the one before.
  \A s \in 1..(Len(hist) - 1) : hist[s].head <= hist[s + 1].head

\* INV_ReaderStable: whatever a reader read is exactly the log as of its
\* snapshot, however many commits truncated since and however many compactions
\* ran.
INV_ReaderStable ==
  \* For each reader that holds a snapshot and each position it read.
  \A r \in Readers : \A p \in Positions :
    \* What it read is whether the ghost log had the entry at its snapshot.
    (rsnap[r] # 0 /\ rseen[r][p] # "unread")
      => ((rseen[r][p] = "yes") <=> (p \in hist[rsnap[r]].live))

\* INV_RollbackHonoured: a commit drops nothing the caller rolled back.
INV_RollbackHonoured ==
  \* For each writer that committed.
  \A w \in Writers : pc[w] = "done" => got[w] \subseteq wanted[w]

====
