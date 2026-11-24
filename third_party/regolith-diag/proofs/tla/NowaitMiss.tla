---- MODULE NowaitMiss ----
\* ===========================================================================
\* THE STORY
\* ===========================================================================
\* A thread that must never block commits a transaction with commit_nowait
\* through a CacheOnly handle: no read the commit makes may touch the disk on
\* that thread. Checking the commit reads, though. Before the commit takes a
\* slot of the commit ring, its early check looks at every version of its
\* keys up to a horizon h (src/engine/commit/early.rs); when its slot is
\* decided, the check looks above h (read_mode.rs, pipeline.rs). Either look
\* can need a table block the cache does not hold.
\*
\* A transaction's commit cannot be made a second time: commit_nowait
\* consumed it. So a miss must not end the commit. What happens instead:
\*   - a miss in the early check leaves the commit "owed" on its queue: the
\*     queue's poll reads the block, keeps it landed for the commit, and
\*     submits the commit again there, which runs the early check again at
\*     a new horizon (RegolithEngine::check_request, NowaitCommit::Owed,
\*     Committed::Owed in src/transaction/nowait.rs);
\*   - a miss while a slot is decided, by a thread that must not read the
\*     disk (the committer, or another CacheOnly thread helping), leaves the
\*     decision as a unit on that thread's queue; its poll decides the slot
\*     with the disk. A thread that may read the disk decides it directly.
\*
\* Tiny example. T commits a blind put to key k, whose only version sits in
\* a cold table block. Another commit moved the horizon, so the early check
\* must look k up in the table: a miss. T's queue reads the block, keeps it
\* landed for T, and runs T's commit again: the check finds the block and
\* the commit takes its slot and lands. Before the fix the miss ended the
\* commit with WouldBlock, and the transaction was gone.
\*
\* What can go wrong, and what this model checks cannot happen:
\*   - a commit that would land is ended by a miss (INV_NoMissEnds);
\*   - the committing call, or a CacheOnly helper, reads the disk
\*     (INV_CallNeverReads);
\*   - a commit lands though a version newer than its snapshot was committed
\*     before its slot, or fails though none was (INV_Exact): the check run
\*     again must look at every version up to its new horizon, never trust
\*     the horizon of the run that missed;
\*   - the commit gets two outcomes (INV_OneOutcome);
\*   - and, over whole runs: every commit gets its outcome
\*     (LIVE_EveryCommitEnds).
\*
\* Why splitting the check at a horizon decides as one full check does, for
\* any horizon at or above the snapshot, so a check run again at a later
\* horizon is as exact as the first: early_check_split in
\* proofs/lean/Regolith/PipelineCheck.lean.
\*
\* WHAT IS MODELLED, and the code each piece mirrors (src/ unless named):
\*   | Model             | Rust                                                  |
\*   |-------------------|-------------------------------------------------------|
\*   | EarlyCheck        | engine/commit/mod.rs RegolithEngine::check_request:   |
\*   |                   | early.rs check_early under scope::cache_only          |
\*   | Owe (EarlyCheck   | check_request's Requested::Owed, NowaitCommit::Owed;  |
\*   |  on a miss)       | transaction/nowait.rs Committed::Owed, ReadOwed job   |
\*   |                   | waited on through IoRuntime::wait_on                  |
\*   | PollRead          | io_queue/queue.rs IoQueue::poll runs the read unit,   |
\*   |                   | QueueShared::land; held = the ledger's holds          |
\*   |                   | (LandingHold.tla)                                     |
\*   | PollResubmit      | nowait.rs Resubmit::deliver on the queue's poll ->    |
\*   |                   | RegolithEngine::resubmit_nowait -> submit_nowait      |
\*   | Enter             | submit_nowait: the descriptor, advance_nowait taking  |
\*   |                   | its slot (pipeline.rs)                                |
\*   | DecideByCommitter | read_mode.rs decide_reading under the committer's     |
\*   |                   | scope: a miss is Err(DecideSlot job), left on its     |
\*   |                   | queue by resume_nowait                                |
\*   | DecideByCacheHelp | decide_reading under the helper's own scope: a miss  |
\*   |                   | leaves DecideSlot on the helper's queue               |
\*   | DecideByBlocking  | decide_reading on a thread outside any CacheOnly     |
\*   |                   | scope: decide_work reads the device                   |
\*   | PollUnit          | io/mod.rs step_with (scope::blocking) steps           |
\*   |                   | DecideSlot::step                                      |
\*   | Tell              | resume.rs resume_nowait -> told.deliver, the ticket's |
\*   |                   | completion (nowait.rs)                                |
\*   | Write, Other,     | other commits, a memtable flush, the block cache's   |
\*   | Flush, Evict      | CLOCK eviction                                        |
\*
\* WHAT IS LEFT OUT, and why that loses nothing:
\*   - One key and two blocks. A commit with many keys misses at most once
\*     per block per run, and a block a run waited for stays landed for the
\*     next run (LandingHold.tla, RunsKeepProgress), so more keys only make
\*     more of the same steps.
\*   - A write-free commit (commit_write_free) takes no slot: its check is
\*     the early check here with no Decide step, owed on a miss the same way
\*     (Owed::WriteFree in nowait.rs, submitted again by Resubmit::deliver).
\*   - A queue dropped while the commit is owed: its job is settled and the
\*     commit told on the dropping thread, which submits it with the device
\*     (Resubmit::deliver), a blocking helper's case here.
\*   - Close: it completes every read wait, so the owed commit is submitted
\*     again at its queue's next poll, and resubmit_nowait refuses it with
\*     Closed (ensure_writable), as it refuses a commit made then; no step
\*     here waits on it.
\*
\* CONFIGURATIONS (each GREEN must pass, each RED must break the one promise
\* it names):
\*   MC_NowaitMiss_Green               the design: every promise holds.
\*   MC_NowaitMiss_Red_Refuse          a miss ends the commit with WouldBlock,
\*                                     as the code did before:
\*                                     INV_NoMissEnds breaks.
\*   MC_NowaitMiss_Red_ReadOnCaller    the committer decides its own slot
\*                                     with the disk: INV_CallNeverReads
\*                                     breaks.
\*   MC_NowaitMiss_Red_NoResubmit      an owed commit is never submitted
\*                                     again: LIVE_EveryCommitEnds breaks.
\*   MC_NowaitMiss_Red_StaleHorizon    the commit submitted again skips its
\*                                     early check and trusts the horizon of
\*                                     the run that missed: INV_Exact breaks.

\* Numbers and set sizes.
EXTENDS Naturals, FiniteSets

\* The fixed inputs of a configuration.
CONSTANTS
  \* "none" for the real code, or the name of one planted bug.
  Mutant

\* The bug names this model knows.
ASSUME Mutant \in {"none", "Refuse", "ReadOnCaller", "NoResubmit", "StaleHorizon"}

\* The table blocks key k can need: its old version's, and the block a flush
\* writes the other writer's version into.
Blocks == {"base", "w"}

\* Who may read the disk, and who must not.
Readers == {"caller", "cachehelper", "blockinghelper", "poll"}

\* The state that changes from step to step.
VARIABLES
  \* Where the committer is: "early" (its early check runs next), "owed"
  \* (a miss left it on its queue), "enter" (its early check passed and it
  \* takes its slot next), "ring" (it holds a slot not decided yet),
  \* "decided" (its outcome is known) or "done" (it was told).
  pc,
  \* The commit's outcome: "none", "committed", "conflict" or "wouldblock".
  outcome,
  \* How many times an outcome was set.
  decisions,
  \* The newest sequence committed: 0, 1 or 2 (two other commits at most).
  seq,
  \* The sequence the other writer's version of k took, 0 while it has none.
  wSeq,
  \* Where that version is: "none", "mem" (a memtable) or "table".
  wAt,
  \* Whether the unrelated commit happened.
  other,
  \* The horizon of the early check that passed, the one the slot's
  \* decision looks above.
  hSeq,
  \* The horizon of the early check that missed.
  hMiss,
  \* The blocks the block cache holds.
  cache,
  \* The blocks landed for the committer's queue and held for its commit.
  held,
  \* The block a read recorded on the committer's queue waits for, or "none".
  pending,
  \* The slot's decision is left as a unit on the committer's queue.
  unitQ,
  \* The slot's decision is left as a unit on the CacheOnly helper's queue.
  unitP,
  \* Who read the disk.
  readers

\* All of them, for "nothing else changes".
vars == <<pc, outcome, decisions, seq, wSeq, wAt, other, hSeq, hMiss, cache,
          held, pending, unitQ, unitP, readers>>

\* The start: the commit about to run its early check, k's old version in a
\* cold block, nothing else committed.
Init ==
  \* The committer's call is about to run the early check.
  /\ pc = "early"
  \* No outcome yet.
  /\ outcome = "none"
  \* None set.
  /\ decisions = 0
  \* Nothing committed after the commit's snapshot (sequence 0).
  /\ seq = 0
  \* The other writer has not written k.
  /\ wSeq = 0
  \* So its version is nowhere.
  /\ wAt = "none"
  \* The unrelated commit has not happened.
  /\ other = FALSE
  \* No early check passed.
  /\ hSeq = 0
  \* None missed.
  /\ hMiss = 0
  \* The cache is cold.
  /\ cache = {}
  \* Nothing landed for the queue.
  /\ held = {}
  \* No read recorded.
  /\ pending = "none"
  \* No unit left on the committer's queue.
  /\ unitQ = FALSE
  \* None on the helper's.
  /\ unitP = FALSE
  \* Nobody read the disk.
  /\ readers = {}

\* ---------------------------------------------------------------------------
\* What a look at k needs
\* ---------------------------------------------------------------------------

\* The block the early check at horizon h must read, or "none": the newest
\* version of k at or below h. The other writer's version, when it is at or
\* below h, is found in its memtable or its table; otherwise the old
\* version, in the cold table. With nothing committed since the snapshot
\* (h = 0), every item is clean and nothing is read.
EarlyNeed(h) ==
  \* Nothing committed since the snapshot: no lookup at all.
  IF h = 0 THEN "none"
  \* The other writer's version is the newest at or below h.
  ELSE IF wSeq # 0 /\ wSeq <= h THEN (IF wAt = "mem" THEN "none" ELSE "w")
  \* Only the old version is: in its table.
  ELSE "base"

\* The block deciding the slot must read, or "none": a version of k above the
\* horizon, which a flush moved into a table.
DecideNeed ==
  \* The other writer's version landed above the horizon and was flushed.
  IF wSeq # 0 /\ wSeq > hSeq /\ wAt = "table" THEN "w" ELSE "none"

\* The verdict deciding the slot gives: a conflict when a version of k
\* committed above the horizon the early check passed at.
Verdict == IF wSeq # 0 /\ wSeq > hSeq THEN "conflict" ELSE "committed"

\* Set the outcome, counting it.
Settle(o) ==
  \* The outcome.
  /\ outcome' = o
  \* One more decision.
  /\ decisions' = decisions + 1

\* ---------------------------------------------------------------------------
\* Other commits and the cache, at any time before the commit takes its slot
\* ---------------------------------------------------------------------------

\* Another transaction writes k and commits, before the committer's slot.
Write ==
  \* Once.
  /\ wSeq = 0
  \* The committer has not taken its slot: this commit's slot comes first.
  /\ pc \in {"early", "owed", "enter"}
  \* It takes the next sequence.
  /\ seq' = seq + 1
  \* That sequence is its version's.
  /\ wSeq' = seq + 1
  \* The version is in a memtable.
  /\ wAt' = "mem"
  \* Nothing else changes.
  /\ UNCHANGED <<pc, outcome, decisions, other, hSeq, hMiss, cache, held, pending,
                 unitQ, unitP, readers>>

\* An unrelated commit moves the horizon past the committer's snapshot.
Other ==
  \* Once.
  /\ ~other
  \* The committer has not taken its slot.
  /\ pc \in {"early", "owed", "enter"}
  \* It happened.
  /\ other' = TRUE
  \* It takes the next sequence.
  /\ seq' = seq + 1
  \* Nothing else changes.
  /\ UNCHANGED <<pc, outcome, decisions, wSeq, wAt, hSeq, hMiss, cache, held, pending,
                 unitQ, unitP, readers>>

\* A flush writes the other writer's version into a new table, cold.
Flush ==
  \* The version is in a memtable.
  /\ wAt = "mem"
  \* Now it is in a table.
  /\ wAt' = "table"
  \* Its block is not in the cache.
  /\ cache' = cache \ {"w"}
  \* Nothing else changes.
  /\ UNCHANGED <<pc, outcome, decisions, seq, wSeq, other, hSeq, hMiss, held, pending,
                 unitQ, unitP, readers>>

\* The cache evicts a block. A block held for the commit stays landed.
Evict(b) ==
  \* It is cached.
  /\ b \in cache
  \* Now it is not.
  /\ cache' = cache \ {b}
  \* Nothing else changes.
  /\ UNCHANGED <<pc, outcome, decisions, seq, wSeq, wAt, other, hSeq, hMiss, held,
                 pending, unitQ, unitP, readers>>

\* ---------------------------------------------------------------------------
\* The committer: its call, and its commit submitted again on its queue
\* ---------------------------------------------------------------------------

\* The early check at the current horizon, from the cache or what landed for
\* the committer's queue, never the disk.
EarlyCheck ==
  \* Its turn.
  /\ pc = "early"
  \* What it must read at the horizon sampled now.
  /\ LET need == EarlyNeed(seq)
     \* Found without the disk.
     IN IF need = "none" \/ need \in cache \/ need \in held
           \* Found.
           THEN /\ IF wSeq # 0 /\ wSeq <= seq
                     \* A version newer than the snapshot is certain: the
                     \* early abort, decided without a slot.
                     THEN /\ Settle("conflict")
                          \* Decided.
                          /\ pc' = "decided"
                          \* No horizon to keep.
                          /\ UNCHANGED hSeq
                     \* Clean up to the horizon: the commit takes its slot next.
                     ELSE /\ pc' = "enter"
                          \* The decision looks above this horizon.
                          /\ hSeq' = seq
                          \* Undecided.
                          /\ UNCHANGED <<outcome, decisions>>
                \* The call ended without a wait: the queue lets go of what
                \* it held for it.
                /\ held' = {}
                \* No miss.
                /\ UNCHANGED <<hMiss, pending>>
           \* A miss.
           ELSE /\ IF Mutant = "Refuse"
                     \* The bug: the miss ends the commit.
                     THEN /\ Settle("wouldblock")
                          \* Decided.
                          /\ pc' = "decided"
                          \* The read is still recorded.
                          /\ pending' = need
                          \* Nothing else of the attempt.
                          /\ UNCHANGED hMiss
                     \* The design: the commit is owed on its queue.
                     ELSE /\ pc' = "owed"
                          \* The read is recorded on the queue.
                          /\ pending' = need
                          \* The horizon this run sampled.
                          /\ hMiss' = seq
                          \* Undecided.
                          /\ UNCHANGED <<outcome, decisions>>
                \* Nothing passed.
                /\ UNCHANGED <<hSeq, held>>
  \* The rest stays.
  /\ UNCHANGED <<seq, wSeq, wAt, other, cache, unitQ, unitP, readers>>

\* The commit takes its slot of the ring. A commit that lands after this
\* takes a later slot, decided after this one, and is no conflict of it.
Enter ==
  \* Its early check passed.
  /\ pc = "enter"
  \* It holds a slot.
  /\ pc' = "ring"
  \* Nothing else changes.
  /\ UNCHANGED <<outcome, decisions, seq, wSeq, wAt, other, hSeq, hMiss, cache, held,
                 pending, unitQ, unitP, readers>>

\* The committer's queue reads the recorded block and keeps it landed for
\* the commit.
PollRead ==
  \* A read waits.
  /\ pending # "none"
  \* The poll reads the disk.
  /\ readers' = readers \cup {"poll"}
  \* The block is cached.
  /\ cache' = cache \cup {pending}
  \* And held for the commit run again.
  /\ held' = held \cup {pending}
  \* The read is done.
  /\ pending' = "none"
  \* Nothing else changes.
  /\ UNCHANGED <<pc, outcome, decisions, seq, wSeq, wAt, other, hSeq, hMiss, unitQ, unitP>>

\* The queue's poll submits the owed commit again, once its read is done.
PollResubmit ==
  \* Owed, and its read done.
  /\ pc = "owed"
  \* Its read landed.
  /\ pending = "none"
  \* The bug NoResubmit never does this.
  /\ Mutant # "NoResubmit"
  \* What it runs.
  /\ IF Mutant = "StaleHorizon"
        \* The bug: no early check, as if the run that missed had passed.
        THEN /\ pc' = "enter"
             \* The horizon of the run that missed.
             /\ hSeq' = hMiss
        \* The design: the early check runs again, at a new horizon.
        ELSE /\ pc' = "early"
             \* Nothing passed yet.
             /\ UNCHANGED hSeq
  \* Nothing else changes.
  /\ UNCHANGED <<outcome, decisions, seq, wSeq, wAt, other, hMiss, cache, held, pending,
                 unitQ, unitP, readers>>

\* The committer decides its own slot, from the cache and its queue's
\* landings.
DecideByCommitter ==
  \* Its slot is undecided.
  /\ pc = "ring"
  \* It has not left the decision on its queue already.
  /\ ~unitQ
  \* What deciding needs.
  /\ LET need == DecideNeed
     \* Found without the disk.
     IN IF need = "none" \/ need \in cache \/ need \in held
           \* Decided.
           THEN /\ Settle(Verdict)
                \* Done deciding.
                /\ pc' = "decided"
                \* No unit, no read.
                /\ UNCHANGED <<unitQ, readers>>
           \* A miss.
           ELSE CASE Mutant = "Refuse" ->
                       \* The bug: the miss refuses the slot.
                       /\ Settle("wouldblock")
                       \* Decided.
                       /\ pc' = "decided"
                       \* Nothing left.
                       /\ UNCHANGED <<unitQ, readers>>
                  \* The bug: the call reads the disk itself.
                  [] Mutant = "ReadOnCaller" ->
                       /\ Settle(Verdict)
                       \* Decided.
                       /\ pc' = "decided"
                       \* The call read the disk.
                       /\ readers' = readers \cup {"caller"}
                       \* No unit.
                       /\ UNCHANGED unitQ
                  \* The design: the decision is left on the queue.
                  [] OTHER ->
                       /\ unitQ' = TRUE
                       \* Still undecided.
                       /\ UNCHANGED <<pc, outcome, decisions, readers>>
  \* The rest stays.
  /\ UNCHANGED <<seq, wSeq, wAt, other, hSeq, hMiss, cache, held, pending, unitP>>

\* A CacheOnly helper reaches the committer's slot first and decides it from
\* the cache; its own queue's landings hold nothing of k.
DecideByCacheHelper ==
  \* The slot is undecided.
  /\ pc = "ring"
  \* The helper has not left the decision on its queue already.
  /\ ~unitP
  \* What deciding needs.
  /\ LET need == DecideNeed
     \* Found in the cache.
     IN IF need = "none" \/ need \in cache
           \* Decided.
           THEN /\ Settle(Verdict)
                \* Done deciding.
                /\ pc' = "decided"
                \* No unit.
                /\ UNCHANGED unitP
           \* A miss.
           ELSE IF Mutant = "Refuse"
                   \* The bug: the miss refuses the committer's slot.
                   THEN /\ Settle("wouldblock")
                        \* Decided.
                        /\ pc' = "decided"
                        \* No unit.
                        /\ UNCHANGED unitP
                   \* The design: the decision is left on the helper's queue.
                   ELSE /\ unitP' = TRUE
                        \* Still undecided.
                        /\ UNCHANGED <<pc, outcome, decisions>>
  \* The rest stays.
  /\ UNCHANGED <<seq, wSeq, wAt, other, hSeq, hMiss, cache, held, pending, unitQ, readers>>

\* A thread that may read the disk reaches the committer's slot first.
DecideByBlocking ==
  \* The slot is undecided.
  /\ pc = "ring"
  \* What deciding needs.
  /\ LET need == DecideNeed
     \* Found in the cache.
     IN IF need = "none" \/ need \in cache
           \* Decided.
           THEN /\ Settle(Verdict)
                \* Done deciding.
                /\ pc' = "decided"
                \* No read.
                /\ UNCHANGED <<readers, cache>>
           \* A miss.
           ELSE IF Mutant = "Refuse"
                   \* The bug: the committer's slot is decided from the cache
                   \* alone, whoever decides it, and refused.
                   THEN /\ Settle("wouldblock")
                        \* Decided.
                        /\ pc' = "decided"
                        \* No read.
                        /\ UNCHANGED <<readers, cache>>
                   \* The design: it reads the disk and decides.
                   ELSE /\ Settle(Verdict)
                        \* Decided.
                        /\ pc' = "decided"
                        \* It read the disk, as it may.
                        /\ readers' = readers \cup {"blockinghelper"}
                        \* The block is cached now.
                        /\ cache' = cache \cup {need}
  \* The rest stays.
  /\ UNCHANGED <<seq, wSeq, wAt, other, hSeq, hMiss, held, pending, unitQ, unitP>>

\* A queue's poll runs the unit left on it: it decides the slot with the
\* disk, unless the slot was decided meanwhile.
PollUnit(onQ) ==
  \* The unit is on this queue.
  /\ IF onQ THEN unitQ ELSE unitP
  \* It is let go.
  /\ IF onQ THEN unitQ' = FALSE /\ UNCHANGED unitP ELSE unitP' = FALSE /\ UNCHANGED unitQ
  \* What it does.
  /\ IF pc = "ring"
        \* The slot is undecided: decided now, with the disk.
        THEN /\ Settle(Verdict)
             \* Decided.
             /\ pc' = "decided"
             \* The poll read the disk.
             /\ readers' = readers \cup {"poll"}
             \* Whatever it read is cached.
             /\ cache' = IF DecideNeed = "none" THEN cache ELSE cache \cup {DecideNeed}
        \* Decided meanwhile: nothing to do.
        ELSE UNCHANGED <<pc, outcome, decisions, readers, cache>>
  \* The rest stays.
  /\ UNCHANGED <<seq, wSeq, wAt, other, hSeq, hMiss, held, pending>>

\* The committer is told its outcome, once, on its queue.
Tell ==
  \* Decided.
  /\ pc = "decided"
  \* Told.
  /\ pc' = "done"
  \* Nothing else changes.
  /\ UNCHANGED <<outcome, decisions, seq, wSeq, wAt, other, hSeq, hMiss, cache, held,
                 pending, unitQ, unitP, readers>>

\* ---------------------------------------------------------------------------
\* The whole system
\* ---------------------------------------------------------------------------

\* What the committer and its queue do: they must keep going.
Commit ==
  \* Its own steps.
  \/ EarlyCheck
  \* Taking its slot.
  \/ Enter
  \* Its slot, decided by itself.
  \/ DecideByCommitter
  \* Its queue's reads.
  \/ PollRead
  \* Its queue submitting it again.
  \/ PollResubmit
  \* Its queue running the decision.
  \/ PollUnit(TRUE)
  \* Being told.
  \/ Tell

\* Any step of anyone.
Next ==
  \* The committer and its queue.
  \/ Commit
  \* The helpers, which may or may not come.
  \/ DecideByCacheHelper
  \* The blocking helper.
  \/ DecideByBlocking
  \* The helper's queue running a unit the helper left there.
  \/ PollUnit(FALSE)
  \* Other commits, a flush, eviction.
  \/ Write
  \* The unrelated commit.
  \/ Other
  \* A flush.
  \/ Flush
  \* An eviction.
  \/ \E b \in Blocks : Evict(b)

\* The committer and its queue keep taking their steps; so does the helper's
\* queue with a unit on it. Nothing else is promised.
Spec == Init /\ [][Next]_vars /\ WF_vars(Commit) /\ WF_vars(PollUnit(FALSE))

\* ---------------------------------------------------------------------------
\* The promises
\* ---------------------------------------------------------------------------

\* Every variable keeps its type.
TypeOK ==
  \* Where the committer is.
  /\ pc \in {"early", "owed", "enter", "ring", "decided", "done"}
  \* Its outcome.
  /\ outcome \in {"none", "committed", "conflict", "wouldblock"}
  \* A count.
  /\ decisions \in 0..2
  \* Sequences.
  /\ seq \in 0..2
  \* The writer's sequence.
  /\ wSeq \in 0..2
  \* Where its version is.
  /\ wAt \in {"none", "mem", "table"}
  \* A flag.
  /\ other \in BOOLEAN
  \* Horizons.
  /\ hSeq \in 0..2
  \* The missed horizon.
  /\ hMiss \in 0..2
  \* Blocks.
  /\ cache \subseteq Blocks
  \* Held blocks.
  /\ held \subseteq Blocks
  \* A recorded read.
  /\ pending \in Blocks \cup {"none"}
  \* Units.
  /\ unitQ \in BOOLEAN
  \* The helper's unit.
  /\ unitP \in BOOLEAN
  \* Who read the disk.
  /\ readers \subseteq Readers

\* Promise: a miss never ends the commit.
INV_NoMissEnds == outcome # "wouldblock"

\* Promise: the committing call and a CacheOnly helper never read the disk.
INV_CallNeverReads == readers \cap {"caller", "cachehelper"} = {}

\* Promise: the commit lands exactly when no version of k newer than its
\* snapshot was committed before its slot.
INV_Exact ==
  \* A landed commit had nothing newer before it.
  /\ outcome = "committed" => wSeq = 0
  \* A conflict had something newer before it.
  /\ outcome = "conflict" => wSeq # 0

\* Promise: one outcome, set once.
INV_OneOutcome == decisions <= 1

\* Promise: every commit is told its outcome.
LIVE_EveryCommitEnds == <>(pc = "done")

====
