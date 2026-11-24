---- MODULE IngestDurability ----
\* THE STORY, for a reader who has never seen the code.
\* A database promises that after a power cut it comes back with "everything
\* up to some point": if a later write survived, every earlier one did too.
\* Ordinary writes become safe by being appended to a log file that is synced
\* to disk now and then. An "ingest" (src/engine/ingest.rs) is a write that
\* adds a whole ready-made table file, and it becomes safe another way: by a
\* synced record in the manifest, the file that lists the tables.
\*
\* Tiny example of what can go wrong. Commit c1 is appended to the log; the
\* log is not synced yet. The ingest then makes its manifest record durable.
\* The power fails. After restart the ingest is there but c1, which came
\* before it, is gone: a hole in the history. The model shows that syncing
\* the log first, in the same ordered step, makes that impossible, and the
\* RED configuration shows the hole when the sync is left out.
\*
\* D48: what a power cut leaves of an ingest that is not rewritten.
\*
\* Under `Eventual` durability a power cut must leave a gap-free prefix of
\* commit order (plan 4.2). An ingest is a commit in that order: its install
\* is an ordered unit of the commit ring (src/engine/commit/ordered.rs), which
\* runs with every earlier commit written and no later one decided, and takes
\* the next sequence there like any commit.
\* It is made durable by its own channel, the synced manifest edit that
\* installs its table, not by the log. Before D48 the ingest flushed every
\* memtable first, which made every earlier commit durable on the way. It
\* now flushes only a memtable holding a key of its file's range, so the
\* commits in the other memtables may still be only in the log, unsynced.
\*
\* PROVED FOR EVERY SIZE in Lean, proofs/lean/Regolith/IngestDurability.lean:
\*   step_inv, reachable_inv        the fixed protocol keeps its invariant:
\*                                  once installed, every commit before the
\*                                  ingest's place is synced
\*   surviving_ingest_keeps_prefix  so an ingest that survives a power cut
\*                                  keeps every commit before it, and what
\*                                  survives is a gap-free prefix (GapFreePrefix
\*                                  here, for any number of commits)
\*   no_log_sync_loses_commit       RED NoLogSync, as a counterexample
\*
\* THE ENGINE.
\*   RegolithEngine::install_ordered    src/engine/ingest.rs
\*     In its ordered unit: sync the active log (`Ordered::sync_log`), take
\*     the sequence (`Ordered::take_seq`), apply the manifest edit (synced),
\*     publish. A commit's record is written to the log in ring order too,
\*     before the unit or after it, never during; under `Eventual` nothing
\*     syncs the log except a rotation, a sync round for a commit that asked
\*     for `Immediate`, and the ingest.
\*
\* THE DEFECT. Mutant "NoLogSync": the ingest's manifest record becomes
\* durable while earlier commits are only in the unsynced log. A power cut
\* then recovers the ingest and loses a commit ordered before it.
\*
\* THE FIX. The ingest syncs the log before its manifest record, in its
\* ordered unit, so every commit ordered before it is durable first.
\*
\* WHAT THE MODEL LEAVES OUT, and why that loses nothing.
\*   - Sequences: slots are decided in ring order and none is decided
\*     while a unit runs, so the order of the steps here is commit order.
\*   - The memtable flushes: a flush makes a commit durable through an
\*     SSTable instead of the log, which only adds to what survives; the
\*     prefix it leaves is the one the log sync leaves.
\*   - Sealed logs: a rotation syncs the log it seals (E2, WalRotation.tla),
\*     so only the active log can hold unsynced records.
\*   - The crash itself: the durable sets only grow, so checking the
\*     invariant in every state checks a power cut in every state.
\*
\* CONFIGURATIONS.
\*   MC_IngestDurability_Green            Mutant = "none"
\*   MC_IngestDurability_Red_NoLogSync    GapFreePrefix fails.

\* Borrow the standard definitions of numbers, sequences (ordered lists) and finite sets.
EXTENDS Naturals, Sequences, FiniteSets

\* The knobs of a run. Each configuration file (MC_IngestDurability_*.cfg) gives them values.
CONSTANTS
  \* The ordinary commits of the run: a set of names. Each commits once. In the code: writes that go
  \* through the commit pipeline (src/engine/commit/mod.rs).
  Commits,
  \* "none" for the design, or "NoLogSync", the planted bug: the ingest does not sync the log before
  \* its manifest record.
  Mutant

\* The ingest's id.
Ingest == "ingest"

\* The ingest's name is not also the name of a commit.
ASSUME Ingest \notin Commits
\* TLC refuses a configuration that names no known bug.
ASSUME Mutant \in {"none", "NoLogSync"}

\* Everyone who commits.
Writers == Commits \cup {Ingest}

\* The state: everything that changes from one step to the next. Each line says what the variable
\* stands for in the real program.
VARIABLES
  \* The writers that committed, in commit order: a sequence. In the code this is the order in which
  \* sequence numbers are taken, which is ring order (RegolithEngine::decide_work and
  \* Ordered::take_seq, src/engine/commit).
  order,
  \* The commits whose record is in the write-ahead log file, synced or not (Log::write_record,
  \* src/engine/wal_log.rs): a set.
  logged,
  \* The commits whose log record has been synced to disk, so that it survives a power cut
  \* (Log::sync_through, src/engine/wal_log.rs): a set.
  synced,
  \* TRUE once the manifest edit that installs the ingest's table is on disk
  \* (RegolithEngine::install_ordered, src/engine/ingest.rs).
  installed,
  \* TRUE while the ingest's ordered unit is between its log sync and its manifest record: it has
  \* the pipeline to itself, so no commit is decided and no other sync runs meanwhile.
  holding

\* Every variable, so a step that changes none of them is a stutter.
vars == <<order, logged, synced, installed, holding>>

\* The writers that have committed, as a set.
Committed == {order[i] : i \in 1..Len(order)}

\* The start: nothing committed, logged, synced or installed.
Init ==
  \* No writer has committed.
  /\ order     = <<>>
  \* The log holds nothing.
  /\ logged    = {}
  \* Nothing is synced.
  /\ synced    = {}
  \* The ingest's table is not installed.
  /\ installed = FALSE
  \* No unit is running.
  /\ holding   = FALSE

\* Commit c is decided: it is next in commit order, and its record is
\* written to the log, unsynced under `Eventual`. It is not decided while
\* the ingest's unit runs.
Commit(c) ==
  \* When: c has not committed yet
  /\ c \notin Committed
  \* and the ingest's unit is not running.
  /\ ~holding
  \* c is next in commit order.
  /\ order'  = Append(order, c)
  \* Its record is in the log, not yet synced.
  /\ logged' = logged \cup {c}
  \* Nothing else changes.
  /\ UNCHANGED <<synced, installed, holding>>

\* The log is synced: a rotation, or a sync round for a commit that asked
\* for `Immediate`. Every logged record becomes durable.
SyncLog ==
  \* When: the ingest's unit is not running (a unit holds the round, so no other sync runs
  \* under it).
  /\ ~holding
  \* Every record in the log is now on disk.
  /\ synced' = logged
  \* Nothing else changes.
  /\ UNCHANGED <<order, logged, installed, holding>>

\* The ingest's unit begins and, in the fix, syncs the log. The mutant
\* skips the sync.
IngestSync ==
  \* When: the ingest has not committed
  /\ Ingest \notin Committed
  \* and its unit is not running already.
  /\ ~holding
  \* Its unit runs from here.
  /\ holding' = TRUE
  \* The design syncs the log here (Ordered::sync_log), so every record in it is on disk; the bug
  \* skips that.
  /\ synced'  = IF Mutant = "NoLogSync" THEN synced ELSE logged
  \* Nothing else changes.
  /\ UNCHANGED <<order, logged, installed>>

\* Still in its unit, the ingest takes its place in commit order and its
\* synced manifest edit makes it durable; then the unit ends.
IngestInstall ==
  \* When: the ingest's unit is running.
  /\ holding
  \* The ingest is next in commit order.
  /\ order'     = Append(order, Ingest)
  \* Its manifest record is on disk.
  /\ installed' = TRUE
  \* The unit ends.
  /\ holding'   = FALSE
  \* The log is untouched.
  /\ UNCHANGED <<logged, synced>>

\* Every step the system can take.
Next ==
  \* A commit commits,
  \/ \E c \in Commits : Commit(c)
  \* or the log is synced,
  \/ SyncLog
  \* or the ingest's unit begins and syncs the log,
  \/ IngestSync
  \* or the ingest installs its table.
  \/ IngestInstall

\* Every behaviour: start in Init, then take Next steps or stutter.
Spec == Init /\ [][Next]_vars

----------------------------------------------------------------------------
\* Invariants.

\* Every variable holds what its comment says.
TypeOK ==
  \* The order is a sequence of writers.
  /\ order \in Seq(Writers)
  \* Only commits have log records.
  /\ logged \subseteq Commits
  \* Only a logged record can be synced.
  /\ synced \subseteq logged
  \* A yes or a no.
  /\ installed \in BOOLEAN
  \* A yes or a no.
  /\ holding \in BOOLEAN

\* What a power cut now leaves: the synced commits, and the ingest once its
\* manifest record is durable.
Recovered == synced \cup (IF installed THEN {Ingest} ELSE {})

\* THE HEADLINE. A power cut in any state recovers the first n writers of
\* commit order, for some n: no writer survives while one before it is lost.
\* Example ruled out: the order is c1, ingest, and a power cut leaves the
\* ingest but not c1. The program needs this because a reader after restart
\* would see the ingest's keys without a write the ingest was ordered after.
GapFreePrefix ==
  \* For some n, what a power cut leaves is exactly the first n writers of the order.
  \E n \in 0..Len(order) : Recovered = {order[i] : i \in 1..n}

====
