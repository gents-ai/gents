---- MODULE AppendedEntries ----
\* ===========================================================================
\* THE STORY
\* ===========================================================================
\* A commit-ordered log is a numbered list of entries, 1, 2, 3, ... that only
\* grows (CommitOrderedAppend.tla). Transaction::append does not write an
\* entry into the log. It puts the entry on a queue inside the transaction,
\* and the commit later gives the queued entries their positions, one after
\* another, in the order they were queued. Until then the entry has no
\* position, so a read of the log, by position or through the head, never
\* shows it (RepeatableRead.tla, the guard behind
\* MC_DefraLevel_Red_OwnAppendVisible: a reader must never see its own append
\* at a log position).
\*
\* A program that searches inside the transaction still needs to see its own
\* pending entries. Transaction::appended(log) answers that. It reads the
\* transaction's own queue, with no I/O and no read recorded, and returns the
\* entries of that one log, oldest first. It returns entries, never
\* positions, so it opens no way to see an entry at a position.
\*
\* The queue is a chain that is walked newest first.
\* Transaction::with_appended(log) hands the log's entries over in that order,
\* borrowed, for a reader that does not care about order; appended(log)
\* collects them and reverses them. A savepoint remembers how many
\* appends the queue held, and a rollback to it drops the newer ones.
\*
\* Tiny example. The transaction appends x to log l1, y to log l2, then z to
\* log l1. appended(l1) is [x, z] and appended(l2) is [y]. The commit gives
\* x and z the next two positions of l1, in that order, and y the next
\* position of l2. If a savepoint was set after x and a rollback came after
\* z, appended(l1) is [x] again.
\*
\* What can go wrong, and what this model checks cannot happen:
\*   - appended(log) is not the entries the commit numbers for that log, in
\*     the order it numbers them (INV_AppendedIsCommitOrder);
\*   - after a rollback, appended(log) is not what it was at the savepoint
\*     (INV_RollbackRestores);
\*   - before the commit, the log or its head shows a pending entry
\*     (INV_OwnAppendInvisible).
\*
\* The buffer laws for any number of appends and logs are proved in
\* proofs/lean/Regolith/Appended.lean: appended_push_same,
\* appended_push_other, rollback_restores and appended_is_commit_order. TLC
\* checks them against the commit's own numbering, step by step.
\*
\* WHAT IS MODELLED, and the code each piece mirrors (src/ unless named):
\*   | Model             | Rust                                                |
\*   |-------------------|-----------------------------------------------------|
\*   | buf               | transaction.rs `appends` (OnceLock<Box<TxnBuffer<(),|
\*   |                   | PendingAppend>>>), made by transaction/append.rs    |
\*   |                   | :338 appends_buffer; walk_chain visits it newest    |
\*   |                   | first (txn_buffer.rs:435)                           |
\*   | Append            | transaction/append.rs:146 Transaction::append       |
\*   | Appended(l)       | transaction/append.rs:246 Transaction::appended,    |
\*   |                   | the reversal, over :309 Transaction::with_appended, |
\*   |                   | the walk and the head_key filter (newest first)     |
\*   | Savepoint, mark   | transaction.rs:1772 set_savepoint, which keeps      |
\*   |                   | append.rs:343 appends_len                           |
\*   | Rollback          | transaction.rs:1800 rollback_to_savepoint, which    |
\*   |                   | calls append.rs:348 truncate_appends                |
\*   | Commit, todo      | engine/commit/append.rs:103 AppendOrder::number:    |
\*   |                   | the queue taken oldest first                        |
\*   | Number, store     | engine/commit/append.rs:136 number_appends: each    |
\*   |                   | append takes the log's next position                |
\*   | Base              | the head each log has when the transaction commits  |
\*   | told, saved,      | ghost: what appended returned at commit, and at the |
\*   |  expect, rolled   | savepoint; no Rust counterpart                      |
\*   | INV_Appended...   | tests/commit_ordered_appended.rs                    |
\*   |                   | after_commit_the_log_positions_follow_the_order_    |
\*   |                   | appended_returned, entries_come_oldest_first        |
\*   | INV_Rollback...   | a_rollback_to_a_savepoint_keeps_only_the_appends_   |
\*   |                   | before_it                                           |
\*
\* WHAT IS LEFT OUT, and why that loses nothing:
\*   - Other transactions: appended reads only this transaction's queue, so
\*     what others do cannot change it. The positions they take move the
\*     numbers (Base), never the order.
\*   - Once keys: a skipped append is listed by appended and gets no
\*     position. The model numbers every append, which is the case where the
\*     answer and the numbering are comparable one for one; a skipped entry
\*     only removes one position from the numbering. The test
\*     an_append_whose_once_key_the_commit_will_skip_is_still_listed pins it.
\*   - Several savepoints: one is enough, because rollback to one drops the
\*     newest appends exactly as the nested ones do, and Lean proves it for
\*     any count (rollback_restores).
\*   - Entry bytes: an entry is a number that is never reused, so two
\*     appends are never confused.
\*
\* CONFIGURATIONS (each GREEN must pass, each RED must break the one promise
\* it names):
\*   MC_AppendedEntries_Green               two logs, up to three appends,
\*                                          a savepoint and a rollback.
\*   MC_AppendedEntries_Red_NewestFirst     the reversal is missing:
\*                                          INV_AppendedIsCommitOrder.
\*   MC_AppendedEntries_Red_OtherLog        entries of other logs are
\*                                          returned too:
\*                                          INV_AppendedIsCommitOrder.
\*   MC_AppendedEntries_Red_RollbackKeeps   a rollback leaves the queue as it
\*                                          is: INV_RollbackRestores.

\* Numbers, sequences and negative numbers.
EXTENDS Naturals, Integers, Sequences

\* The fixed inputs of a configuration.
CONSTANTS
  \* "none" for the real code, or the name of one planted bug.
  Mutant,
  \* The logs a transaction can append to.
  Logs,
  \* The most appends one transaction makes, those a rollback drops included.
  MaxApp

\* The bug names this model knows.
ASSUME Mutant \in {"none", "NewestFirst", "OtherLog", "RollbackKeeps"}

\* The state that changes from step to step.
VARIABLES
  \* The transaction's append buffer, newest first: a sequence of pairs
  \* <<log, entry>>. Mirrors the chain walk_chain visits.
  buf,
  \* The length the buffer had at the savepoint, or -1 while none is set.
  \* Mirrors the mark a savepoint keeps (appends_len).
  mark,
  \* Where the transaction is: "open" (appending), "numbering" (the commit is
  \* giving positions), or "done". Mirrors the transaction's state.
  phase,
  \* The appends the commit has still to number, oldest first. Mirrors the
  \* queue AppendOrder::number walks.
  todo,
  \* For each log, the entries the commit has numbered, as pairs
  \* <<position, entry>>. Mirrors the puts number_appends makes.
  store,
  \* The next entry id: each append gets a new one, so no two are alike.
  next,
  \* Ghost. What appended returned for each log at the savepoint. No Rust
  \* counterpart.
  saved,
  \* Ghost. Whether the last step was a rollback. No Rust counterpart.
  rolled,
  \* Ghost. What appended(l) must be after that rollback: the saved answers.
  expect,
  \* Ghost. What appended returned for each log at the moment of commit. No
  \* Rust counterpart.
  told

\* All of them, for "nothing else changes".
vars == <<buf, mark, phase, todo, store, next, saved, rolled, expect, told>>

\* ---------------------------------------------------------------------------
\* Small helpers
\* ---------------------------------------------------------------------------

\* The head each log has before this transaction commits: both start with two
\* entries, so the positions this transaction takes begin at 3.
Base == 2

\* The sequence s read backwards.
Reverse(s) == [i \in 1..Len(s) |-> s[Len(s) - i + 1]]

\* What Transaction::appended returns for log l: walk the buffer newest first,
\* keep the entries of l, reverse them to oldest first.
Appended(l) ==
  \* The pairs the walk keeps: those naming l (the bug keeps them all).
  LET walk == SelectSeq(buf, LAMBDA p: Mutant = "OtherLog" \/ p[1] = l)
      \* Just the entries of those pairs, still newest first.
      entries == [i \in 1..Len(walk) |-> walk[i][2]]
  \* The bug leaves them newest first; the real code reverses them.
  IN IF Mutant = "NewestFirst" THEN entries ELSE Reverse(entries)

\* What the commit gave the log l, as a list of entries in position order.
Numbered(l) == [i \in 1..Len(store[l]) |-> store[l][i][2]]

\* ---------------------------------------------------------------------------
\* The start
\* ---------------------------------------------------------------------------

\* Nothing is queued, nothing is committed.
Init ==
  \* The buffer is empty.
  /\ buf = <<>>
  \* No savepoint.
  \* No savepoint is set.
  /\ mark = -1
  \* The transaction is open.
  /\ phase = "open"
  \* Nothing to number.
  \* No append is left.
  /\ todo = <<>>
  \* Nothing numbered.
  /\ store = [l \in Logs |-> <<>>]
  \* The first entry id is 1.
  /\ next = 1
  \* Nothing saved.
  /\ saved = [l \in Logs |-> <<>>]
  \* No rollback yet.
  /\ rolled = FALSE
  \* Nothing expected.
  /\ expect = [l \in Logs |-> <<>>]
  \* Nothing told.
  /\ told = [l \in Logs |-> <<>>]

\* ---------------------------------------------------------------------------
\* The transaction
\* ---------------------------------------------------------------------------

\* The transaction appends a new entry to log l.
AppendTo(l) ==
  \* It is still open, and has not yet made its last append. The count
  \* includes appends a rollback has dropped, which bounds the whole run.
  /\ phase = "open"
  \* Fewer than the allowed number of appends have been made.
  /\ next <= MaxApp
  \* The new pair goes on the front: the front is the newest.
  /\ buf' = <<<<l, next>>>> \o buf
  \* The next entry gets a new id.
  /\ next' = next + 1
  \* This is not a rollback.
  /\ rolled' = FALSE
  \* Nothing else changes.
  /\ UNCHANGED <<mark, phase, todo, store, saved, expect, told>>

\* The transaction sets a savepoint: it keeps the buffer's length and what
\* appended returns now.
Savepoint ==
  \* It is open and has no savepoint yet.
  /\ phase = "open"
  \* No savepoint is set.
  /\ mark = -1
  \* The mark is the number of appends so far.
  /\ mark' = Len(buf)
  \* What appended returns for each log, kept for the check.
  /\ saved' = [l \in Logs |-> Appended(l)]
  \* This is not a rollback.
  /\ rolled' = FALSE
  \* Nothing else changes.
  /\ UNCHANGED <<buf, phase, todo, store, next, expect, told>>

\* The transaction rolls back to its savepoint: the appends made after it
\* are the newest, at the front, and are dropped.
Rollback ==
  \* It is open and a savepoint is set.
  /\ phase = "open"
  \* A savepoint is set.
  /\ mark >= 0
  \* The bug leaves the buffer; the real code keeps only the oldest `mark`.
  /\ buf' = IF Mutant = "RollbackKeeps" THEN buf
            \* The real code keeps the oldest `mark` appends, the tail of the newest-first buffer.
            ELSE SubSeq(buf, Len(buf) - mark + 1, Len(buf))
  \* The savepoint is used up.
  /\ mark' = -1
  \* The check is armed: appended must now give the saved answers.
  /\ rolled' = TRUE
  \* The expected answers are the saved ones.
  /\ expect' = saved
  \* Nothing else changes.
  /\ UNCHANGED <<phase, todo, store, next, saved, told>>

\* ---------------------------------------------------------------------------
\* The commit
\* ---------------------------------------------------------------------------

\* The commit begins: it takes the queue oldest first, the order it numbers
\* in, and the ghost notes what appended said just before.
BeginCommit ==
  \* The transaction is open.
  /\ phase = "open"
  \* The commit is giving positions now.
  /\ phase' = "numbering"
  \* The queue is the buffer read oldest first.
  /\ todo' = Reverse(buf)
  \* What appended returned for each log at this moment.
  /\ told' = [l \in Logs |-> Appended(l)]
  \* Nothing else changes.
  /\ UNCHANGED <<buf, mark, store, next, saved, rolled, expect>>

\* The commit numbers the next queued append: it takes the log's next
\* position, one past the newest.
Number ==
  \* The commit is numbering and appends remain.
  /\ phase = "numbering"
  \* At least one append is left.
  /\ todo # <<>>
  \* The first append's log takes the next position with that entry.
  /\ store' = [store EXCEPT ![Head(todo)[1]] =
                 Append(@, <<Base + Len(@) + 1, Head(todo)[2]>>)]
  \* That append is done.
  /\ todo' = Tail(todo)
  \* Nothing else changes.
  /\ UNCHANGED <<buf, mark, phase, next, saved, rolled, expect, told>>

\* Every append is numbered: the commit is over.
Finish ==
  \* The commit is numbering and nothing remains.
  /\ phase = "numbering"
  \* No append is left.
  /\ todo = <<>>
  \* The transaction is done.
  /\ phase' = "done"
  \* Nothing else changes.
  /\ UNCHANGED <<buf, mark, todo, store, next, saved, rolled, expect, told>>

\* Any one step.
Next ==
  \* Append to some log.
  \/ \E l \in Logs : AppendTo(l)
  \* Set a savepoint.
  \/ Savepoint
  \* Roll back to it.
  \/ Rollback
  \* Start the commit.
  \/ BeginCommit
  \* Number one append.
  \/ Number
  \* End the commit.
  \/ Finish

\* Every run starts in Init and takes steps of Next.
Spec == Init /\ [][Next]_vars

\* ---------------------------------------------------------------------------
\* The promises
\* ---------------------------------------------------------------------------

\* Every variable holds the kind of value its comment says.
TypeOK ==
  \* The phase is one of the three.
  /\ phase \in {"open", "numbering", "done"}
  \* The mark is -1 or a length the buffer can have had.
  /\ mark \in -1..MaxApp
  \* The buffer is a sequence of at most MaxApp pairs.
  /\ Len(buf) <= MaxApp
  \* The ghost flag is a boolean.
  /\ rolled \in BOOLEAN

\* Promise: before the commit gives positions, nothing at a log position or
\* behind a head shows a pending entry, because the log holds only what it
\* held when the transaction began. appended gives entries, not positions.
INV_OwnAppendInvisible ==
  \* While the transaction is open, no log has a numbered entry.
  phase = "open" => \A l \in Logs : store[l] = <<>>

\* Promise: appended(l), asked just before the commit, is exactly the entries
\* the commit then numbered for l, in the order it numbered them, at
\* consecutive positions from the head up.
INV_AppendedIsCommitOrder ==
  \* Once the commit is over, for each log:
  phase = "done" => \A l \in Logs :
    \* The answer equals the numbered entries,
    /\ told[l] = Numbered(l)
    \* and they sit at the positions Base + 1, Base + 2, ...
    /\ \A i \in 1..Len(store[l]) : store[l][i][1] = Base + i

\* Promise: right after a rollback to a savepoint, appended gives for every
\* log what it gave at the savepoint.
INV_RollbackRestores ==
  \* Just after a rollback, for each log:
  rolled => \A l \in Logs : Appended(l) = expect[l]

====
