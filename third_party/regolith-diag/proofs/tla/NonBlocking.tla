---- MODULE NonBlocking ----
\* ===========================================================================
\* THE STORY
\* ===========================================================================
\* regolith reads a table block from the disk when a reader needs it and the
\* block cache does not hold it. A thread that must never block cannot wait
\* for that disk read. So each such thread holds its own "I/O queue" (an
\* IoQueue in src/io_queue/queue.rs), and reads through a CacheOnly handle:
\*   - the read looks in the cache; on a miss it does NOT touch the disk.
\*     It records the block on its own queue and returns WouldBlock at once;
\*   - later, when the thread has nothing else to do, it polls its queue.
\*     The poll does the disk reads its thread is waiting for, and hands
\*     every waiting read a "you can run again now" note;
\*   - the read runs again and finds its block.
\*
\* Tiny example. Two threads, A and B, both miss block 7 at the same moment.
\* There is ONE "unit" for block 7 (a single-flight read, src/engine/io/unit.rs).
\* A polls first and grabs the unit with one compare-and-swap; B's poll sees
\* it taken and leaves it alone. A reads block 7 once, then puts one "done"
\* note into A's inbox and one into B's inbox. B was asleep ("idle"), so the
\* note to B also wakes B up. A was awake, so nothing wakes A: it just finds
\* the note at its next poll.
\*
\* What can go wrong, and what this model checks cannot happen:
\*   - a note lands in the wrong thread's inbox, so the wrong thread finishes
\*     a read and the right one never hears (OwnQueue);
\*   - a thread is told twice, or a thread that asked is never told
\*     (ExactlyOnce, EveryWaiterTold);
\*   - two threads both grab the same unit and read the block twice
\*     (SingleRun);
\*   - a thread goes to sleep right as a note arrives and sleeps forever with
\*     a note in its inbox (NoLostIdleWakeup);
\*   - a thread that is busy gets woken anyway (BusyNeverWoken);
\*   - some I/O regolith starts by itself is put on nobody's queue, so it
\*     never runs (NoOrphanUnit);
\*   - and, over whole runs: every read and every job finishes, with many
\*     threads or with one (the single-threaded wasm case) (AllDone).
\*
\* THE COMMIT SIDE (commit units). Writes take the same path:
\*   - commits made by commit_nowait that need an fsync wait on ONE unit,
\*     the sync round that covers their records (the round is the one claim
\*     on the log's device step, src/engine/commit/round.rs). Each such
\*     commit puts a note for it on its OWN queue; the first of those queues
\*     to poll claims it with one CAS and runs the fsync; every one of them
\*     is told once. A thread that needs the log durable inside a blocking
\*     call may run it too (LandHere), and close runs it. Tiny example:
\*     threads A and B commit, and round 4 covers both; B polls first, syncs
\*     once, and the note to A wakes A.
\*     What can go wrong: the fsync runs twice (SingleRun), the commits are
\*     visible before the fsync (VisibleAfterSync), or B's poll makes A's
\*     ticket ready on B's thread instead of at A's own poll
\*     (DeliveredOnOwnPoll);
\*   - a write a stall stops returns at once with a wait; every writer
\*     stopped during one stall waits on one PASSIVE unit, which no poll
\*     runs and which lands only when the stall clears. A writer installs
\*     or finds the unit, registers on its own queue, and only then looks at
\*     the stall again, landing the unit itself if it cleared meanwhile.
\*     What can go wrong: a writer installs the unit just after the clearer
\*     looked, and nobody ever lands it (StallLandsWhenClear).
\*     A transaction's commit cannot be made a second time, so a stall does
\*     not end it: a commit_nowait commit is left on its queue behind the
\*     stall's unit and goes on at the poll that finds the unit landed, and
\*     a blocking commit sleeps on the unit (RegolithEngine::admit_commit and
\*     admit_commit_nowait, src/engine/mod.rs). Both are waiters of the unit
\*     like any stopped write and look at the stall again once they have it:
\*     the writer below that registers, looks again and later runs again
\*     (WriteTry, Rerun) stands for them.
\*
\* TIMED WAITS. A transaction that lost its commit may wait a little before
\* it runs again (RetryPolicy::backoff). A thread that must never block
\* cannot sleep, so its task asks the thread's queue for a "timed wait"
\* (IoRuntime::timer, src/engine/io/mod.rs): ready once the clock reaches a
\* due time. The request is a note in the queue's inbox like any other. The
\* owner's poll takes it in, and the first poll that finds the clock at the
\* due time or later completes it (Timers, src/io_queue/timers.rs). Nobody
\* is woken for a timed wait and nothing sleeps for one: each poll tells the
\* owner how long it may idle until the next one is due
\* (IoProgress::wake_in), and the owner polls again by then.
\* Tiny example. At time 40 thread A asks for a wait due at 100. A's poll at
\* 60 leaves it and says "idle at most 40". A's poll at 100 completes it.
\* What can go wrong: the wait lands on another thread's queue
\* (TimerOnOwnQueue); a poll completes it before it is due (TimerNotEarly);
\* A goes idle, the wait comes due, and A sleeps for ever because nothing
\* wakes it (AllDone, with an owner that polls again in time: TimerWake);
\* A drops its queue and the task waiting on the wait is never told
\* (DroppedHoldsNothing).
\*
\* WHAT IS MODELLED, and the code each piece mirrors:
\*   unit        a single-flight read: Unit, src/engine/io/unit.rs
\*   table       the unit table: IoRuntime::units, src/engine/io/mod.rs,
\*               whose entries are weak: the queues waiting on a unit hold
\*               it, and LandingReclaim.tla checks that a removed entry
\*               keeps no block alive
\*   inbox       a queue's lock-free inbox: QueueShared::inbox,
\*               src/engine/io/shared.rs, a Stack with an IDLE flag bit
\*   waiting     the units a queue registered on: IoQueue::waiting
\*   slot        one read's wait: WaitSlot, behind each IoWait
\*   have        what a re-run read finds for its queue: the reads that
\*               landed for the queue (QueueShared::landed). The block cache
\*               is left out on purpose: it only ever makes a re-run find its
\*               block sooner, so this is the hard case, the one where only
\*               the landing carries the read (a disabled cache). `have`
\*               only grows: a re-run finds every block that landed for it.
\*               The code bounds the landings by bytes, and keeps that
\*               promise by holding past the bound what a call run again
\*               still needs (src/io_queue/ledger.rs); LandingHold.tla
\*               checks that, for calls that need several blocks at once.
\*   timed       the timed waits a queue's owner holds: IoQueue::timers,
\*               src/io_queue/queue.rs
\*   elapsed     the clock, as far as a timed wait can tell: the waits whose
\*               due time it has reached (Env::now_micros read by a poll)
\*   dropped     the queues that are gone: Drop for IoQueue
\*
\* WHAT IS LEFT OUT, and why that loses nothing:
\*   - Memory ordering and the exact CAS loops: the loom models check those
\*     (tests/loom_io_queue.rs). Here each CAS is one atomic step.
\*   - The byte bound on a queue: it delays a read (a "room" wait) but never
\*     changes who is told what. The bound on what stays landed is
\*     LandingHold.tla's, see `have` above.
\*   - What a dropped queue does with its units: it releases them exactly
\*     as close does (Release below), which adds no new kind of step. What
\*     it does with its timed waits is a step of its own (DropQueue), so the
\*     threads that may drop a queue here wait on timed waits only.
\*   - The clock as a number, and how long an owner may idle: a timed wait
\*     is either due or not, and an owner that holds a due one runs again
\*     (TimerWake). Lean keeps the numbers: poll_after_wake_in_completes.
\*   - Read failures: a failed unit is told to its waiters like a landed one;
\*     only what lands differs.
\*   - Jobs (I/O regolith starts itself: the flush or the bounded step a
\*     write leaves owing with no worker, the disk check, compact_range,
\*     an ingest or a checkpoint run on the caller's queue) take the very
\*     same path as a read's miss (a unit, a request on the starting
\*     thread's own queue, src/engine/io/job.rs); SelfStart stands for all
\*     of them.
\*   - What a sync round writes and publishes: CommitPipeline.tla checks
\*     which commits a round covers, their order and the failure path. Here
\*     a round is a unit whose finish is its fsync, then the publication of
\*     the commits it covers.
\*
\* CONFIGURATIONS (each GREEN must pass, each RED must break the one
\* invariant it names):
\*   MC_NonBlocking_Green_Pool       two threads miss one block, and one of
\*                                   them a second block of its own.
\*   MC_NonBlocking_Green_Jobs       two threads miss one block while a
\*                                   call on one of them starts a job.
\*   MC_NonBlocking_Green_Single     one thread does everything (wasm).
\*   Those three check every invariant. That everything finishes (AllDone)
\*   is checked by three smaller runs, since the property costs three times
\*   the invariants at full size:
\*   MC_NonBlocking_Green_PoolLive   two threads miss one block.
\*   MC_NonBlocking_Green_JobsLive   one thread misses a block, the other
\*                                   starts a job.
\*   MC_NonBlocking_Green_SingleLive one thread, one block and a job.
\*   (Both pool setups at once, two blocks and a job, also passes: 2,283,135
\*   distinct states, about three minutes on eight workers; too slow for
\*   the recipe's single worker, so the recipe checks the two halves.)
\*   MC_NonBlocking_Red_WrongQueue   a read's request goes to the other
\*                                   thread's queue: OwnQueue breaks.
\*   MC_NonBlocking_Red_LostIdleWakeup  an owner checks "inbox empty" and
\*                                   marks itself idle in two steps:
\*                                   NoLostIdleWakeup breaks.
\*   MC_NonBlocking_Red_DoubleRun    a claim that also succeeds on a claimed
\*                                   unit: SingleRun breaks.
\*   MC_NonBlocking_Red_SelfIoNowhere  a job's unit is put on no queue:
\*                                   NoOrphanUnit breaks.
\*   MC_NonBlocking_Red_BusyWoken    a poll that forgets to clear the idle
\*                                   flag: BusyNeverWoken breaks.
\*   MC_NonBlocking_Green_Commit     two threads commit with commit_nowait
\*                                   and one round covers both; any thread
\*                                   may run it.
\*   MC_NonBlocking_Green_Stall      two writers stopped by one stall.
\*   MC_NonBlocking_Red_DoubleSync   the DoubleRun claim on a round: its
\*                                   fsync runs twice, SingleRun breaks.
\*   MC_NonBlocking_Red_PublishAtWrite  commits visible as soon as they are
\*                                   written: VisibleAfterSync breaks.
\*   MC_NonBlocking_Red_DeliverOnLander  the thread that runs a round makes
\*                                   every waiting commit's ticket ready
\*                                   itself: DeliveredOnOwnPoll breaks.
\*   MC_NonBlocking_Red_NoRecheck    a stopped writer that does not look at
\*                                   the stall again after registering:
\*                                   StallLandsWhenClear breaks.
\*   MC_NonBlocking_Green_Timer      a thread asks for a timed wait beside
\*                                   its read; another asks for one and may
\*                                   drop its queue. Every invariant.
\*   MC_NonBlocking_Green_TimerLive  two threads with a timed wait each, one
\*                                   of which may drop its queue: every
\*                                   timed wait finishes (AllDone).
\*   MC_NonBlocking_Red_IdlePastDue  an owner that stays idle past a timed
\*                                   wait's due time: AllDone breaks.
\*   MC_NonBlocking_Red_EarlyFire    a poll that completes a timed wait
\*                                   without looking at the clock:
\*                                   TimerNotEarly breaks.
\*   MC_NonBlocking_Red_DropForgets  a dropped queue that leaves its timed
\*                                   waits pending: DroppedHoldsNothing
\*                                   breaks.
\*   MC_NonBlocking_Red_TimerWrongQueue  a timed wait's note goes to the
\*                                   other thread's queue: TimerOnOwnQueue
\*                                   breaks.
\*
\* Lean, proofs/lean/Regolith/IoQueue.lean, proves the same rules for every
\* number of threads and units, the commit units included: round_sync_once
\* (SingleRun for a round), round_visible_after_sync (VisibleAfterSync),
\* ticket_ready_only_at_own_poll (DeliveredOnOwnPoll) and
\* stall_lands_when_clear (StallLandsWhenClear), with the RED cases
\* lander_delivery_breaks_own_poll and no_recheck_strands_stall. For timed
\* waits, with the clock as a number: timed_wait_completed_once,
\* timed_wait_completed_by_owner (TimerOnOwnQueue), timed_wait_not_before_due
\* (TimerNotEarly), dropped_queue_holds_no_wait (DroppedHoldsNothing) and
\* poll_after_wake_in_completes (AllDone for a timed wait), with the RED
\* cases early_fire_breaks_due and idle_owner_never_completes.

\* We use numbers, sequences (for inboxes) and finite sets.
EXTENDS Naturals, Sequences, FiniteSets

\* The fixed inputs of a configuration.
CONSTANTS
  \* The threads. Each holds exactly one queue, named after it.
  Threads,
  \* The blocks reads can miss.
  Blocks,
  \* [Threads -> SUBSET Blocks]: the blocks each thread's reads need.
  Want,
  \* Names for I/O regolith starts itself; disjoint from Blocks.
  Jobs,
  \* [Jobs -> Threads]: the thread whose call started each job.
  JobOf,
  \* Names for sync rounds; disjoint from Blocks and Jobs.
  Rounds,
  \* [Rounds -> SUBSET Threads]: the threads with a commit_nowait commit that
  \* each round covers.
  CommittersOf,
  \* Names for write stalls; disjoint from the rest.
  Stalls,
  \* [Stalls -> SUBSET Threads]: the threads whose write each stall stops.
  StoppedBy,
  \* Names for timed waits; disjoint from the rest.
  Timers,
  \* [Timers -> Threads]: the thread that asks for each timed wait, on its
  \* own queue.
  TimerOf,
  \* The threads that may drop their queue while the run goes on.
  Droppers,
  \* How many units may ever be made (each miss of a gone unit makes one).
  MaxUnits,
  \* "none" for the real code, or the name of one planted bug.
  Mutant

\* Unit number 0 means "no unit".
NoUnit == 0
\* The unit numbers that can exist.
Ids == 1..MaxUnits
\* Everything a wait can be for: a block, a job, a sync round or a stall
\* (each of which gets a unit), or a timed wait (which gets none).
Things == Blocks \cup Jobs \cup Rounds \cup Stalls \cup Timers
\* What thread t is waiting to see finished: its reads, its jobs, its
\* commits, its stopped writes, and its timed waits.
Goal(t) == Want[t] \cup {j \in Jobs : JobOf[j] = t}
           \cup {r \in Rounds : t \in CommittersOf[r]}
           \cup {s \in Stalls : t \in StoppedBy[s]}
           \cup {x \in Timers : TimerOf[x] = t}
\* The other thread (only used by the WrongQueue bug, with two threads).
Other(t) == CHOOSE o \in Threads : o # t

\* Blocks and jobs are different things.
ASSUME Blocks \cap Jobs = {}
\* Rounds are neither blocks nor jobs.
ASSUME Rounds \cap (Blocks \cup Jobs) = {}
\* Stalls are none of the others.
ASSUME Stalls \cap (Blocks \cup Jobs \cup Rounds) = {}
\* Each round has the threads whose commits it covers.
ASSUME CommittersOf \in [Rounds -> SUBSET Threads]
\* Each stall stops some threads' writes.
ASSUME StoppedBy \in [Stalls -> SUBSET Threads]
\* Timed waits are none of the others.
ASSUME Timers \cap (Blocks \cup Jobs \cup Rounds \cup Stalls) = {}
\* Each timed wait is asked for by one thread.
ASSUME TimerOf \in [Timers -> Threads]
\* The threads that may drop their queue are threads.
ASSUME Droppers \subseteq Threads
\* A thread that may drop its queue waits on timed waits only, so its drop
\* has no unit to let go: units are let go as close lets them go (Release).
ASSUME \A t \in Droppers : Goal(t) \subseteq Timers
\* Each thread wants a set of blocks.
ASSUME Want \in [Threads -> SUBSET Blocks]
\* Each job belongs to one thread.
ASSUME JobOf \in [Jobs -> Threads]
\* Unit counts are natural numbers.
ASSUME MaxUnits \in Nat
\* The bug names this model knows.
ASSUME Mutant \in {"none", "WrongQueue", "LostIdleWakeup", "DoubleRun",
                   "SelfIoNowhere", "BusyWoken", "PublishAtWrite",
                   "DeliverOnLander", "NoRecheck", "IdlePastDue",
                   "EarlyFire", "DropForgets"}

\* The state that changes from step to step.
VARIABLES
  \* [Things -> Ids \cup {NoUnit}]: which unit the table holds for each
  \* block or job right now (IoRuntime::units).
  table,
  \* [Ids -> Things \cup {NoUnit}]: what each unit reads.
  what,
  \* [Ids -> {"none","free","claimed","done"}]: Unit::state. "none" means
  \* the number is not handed out yet.
  state,
  \* [Ids -> {"none","landed","released"}]: how a done unit ended: it read
  \* its block ("landed"), or close let it go without reading ("released").
  outcome,
  \* [Ids -> SUBSET Threads]: queues registered on the unit (its waiter
  \* Stack).
  waiters,
  \* [Ids -> BOOLEAN]: the SHUT flag on the unit's waiter Stack.
  shut,
  \* [Ids -> SUBSET Threads]: ghost, every queue whose registration
  \* succeeded, kept to check who must be told.
  regd,
  \* [Ids -> SUBSET Threads]: threads that claimed the unit and have not
  \* finished running it.
  runners,
  \* [Ids -> Nat]: ghost, how many times the unit's disk read ran.
  runs,
  \* [Ids -> SUBSET Threads]: queues the finishing thread still has to put
  \* a "done" note into.
  toTell,
  \* [Threads -> [Ids -> Nat]]: ghost, "done" notes each queue was given
  \* for each unit.
  told,
  \* The next unit number to hand out.
  next,
  \* [Threads -> Seq(message)]: each queue's inbox, oldest first.
  inbox,
  \* [Threads -> BOOLEAN]: the IDLE flag bit in each inbox's head word.
  idleFlag,
  \* [Threads -> Seq(message)]: notes the owner took out of its inbox and
  \* is still handling, one by one.
  taken,
  \* [Threads -> SUBSET Ids]: units each queue registered on (IoQueue::
  \* waiting).
  waiting,
  \* [Threads -> [Ids -> SUBSET Threads]]: for each queue and unit, the
  \* threads whose reads that queue recorded for it (the WaitSlots kept in
  \* the waiting entry).
  attached,
  \* [Threads -> [Things -> {"none","pending","ready"}]]: each read's wait:
  \* none, waiting, or ready to run again.
  slot,
  \* [Threads -> SUBSET Things]: blocks that landed for each queue.
  have,
  \* [Threads -> SUBSET Things]: reads that returned an answer, and jobs
  \* whose I/O finished.
  done,
  \* The jobs regolith has started.
  started,
  \* [Threads -> {"busy","idle"}]: is the owner thread running or asleep.
  own,
  \* [Threads -> BOOLEAN]: the owner's idle waker fired and it has not run
  \* since.
  woken,
  \* [Threads -> BOOLEAN]: bug LostIdleWakeup only: the owner saw an empty
  \* inbox and has not marked itself idle yet.
  checking,
  \* close() has run.
  closedDb,
  \* [Rounds -> Ids \cup {NoUnit}]: the unit each round got when the first
  \* commit to wait on it put it on a queue (the round's job, Round::job,
  \* src/engine/commit/round.rs).
  roundUnit,
  \* [Rounds -> SUBSET Threads]: the threads whose commit_nowait returned.
  joined,
  \* The rounds whose fsync ran.
  synced,
  \* The rounds whose commits readers can see.
  visible,
  \* Ghost: <<thread, thing>> for every wait made ready by a step that was
  \* not that thread's own poll.
  offPoll,
  \* The stalls in force now.
  stalled,
  \* [Threads -> [Stalls -> {"none", "seen", "recheck"}]]: a stopped write's
  \* progress: it saw the stall, or it registered its wait and is about to
  \* look at the stall again.
  spc,
  \* [Threads -> SUBSET Timers]: the timed waits each queue's owner took in
  \* and holds until they are due (IoQueue::timers).
  timed,
  \* The timed waits whose due time the clock has reached.
  elapsed,
  \* The threads whose queue is dropped.
  dropped

\* Every variable, so "nothing changed" can be written once.
vars == <<table, what, state, outcome, waiters, shut, regd, runners, runs,
          toTell, told, next, inbox, idleFlag, taken, waiting, attached,
          slot, have, done, started, own, woken, checking, closedDb,
          roundUnit, joined, synced, visible, offPoll, stalled, spc,
          timed, elapsed, dropped>>

\* The commit side's variables, so a step that leaves them alone says so once.
commitVars == <<roundUnit, joined, synced, visible, offPoll, stalled, spc>>

\* The variables of the timed waits, for the same reason.
timerVars == <<timed, elapsed, dropped>>

\* A note in an inbox: "read" (a read recorded unit u for reader r), "done"
\* (unit u finished), or "timer" (reader r asked for timed wait u).
Msg == [kind : {"read", "done", "timer"}, u : Ids \cup Timers,
        reader : Threads \cup {0}]

\* The start: no unit, empty inboxes, every thread busy, nothing done.
Init ==
  \* No block or job has a unit in the table.
  /\ table = [x \in Things |-> NoUnit]
  \* No unit number reads anything yet.
  /\ what = [u \in Ids |-> NoUnit]
  \* No unit number is in use.
  /\ state = [u \in Ids |-> "none"]
  \* No unit has ended.
  /\ outcome = [u \in Ids |-> "none"]
  \* Nobody waits on any unit.
  /\ waiters = [u \in Ids |-> {}]
  \* Every waiter list is open.
  /\ shut = [u \in Ids |-> FALSE]
  \* Nobody has registered anywhere.
  /\ regd = [u \in Ids |-> {}]
  \* Nobody is running a unit.
  /\ runners = [u \in Ids |-> {}]
  \* No disk read has run.
  /\ runs = [u \in Ids |-> 0]
  \* Nobody is owed a note.
  /\ toTell = [u \in Ids |-> {}]
  \* No note has been given.
  /\ told = [q \in Threads |-> [u \in Ids |-> 0]]
  \* The first unit gets number 1.
  /\ next = 1
  \* Every inbox is empty.
  /\ inbox = [q \in Threads |-> <<>>]
  \* No owner is marked idle.
  /\ idleFlag = [q \in Threads |-> FALSE]
  \* Nobody is in the middle of handling notes.
  /\ taken = [q \in Threads |-> <<>>]
  \* No queue is registered on a unit.
  /\ waiting = [q \in Threads |-> {}]
  \* No queue holds a read for a unit.
  /\ attached = [q \in Threads |-> [u \in Ids |-> {}]]
  \* No read is waiting.
  /\ slot = [t \in Threads |-> [x \in Things |-> "none"]]
  \* Nothing has landed anywhere.
  /\ have = [q \in Threads |-> {}]
  \* No read has answered.
  /\ done = [t \in Threads |-> {}]
  \* No job has started.
  /\ started = {}
  \* Every thread is running.
  /\ own = [t \in Threads |-> "busy"]
  \* No waker has fired.
  /\ woken = [t \in Threads |-> FALSE]
  \* Nobody is half way into going idle.
  /\ checking = [t \in Threads |-> FALSE]
  \* The database is open.
  /\ closedDb = FALSE
  \* No round is on a queue yet.
  /\ roundUnit = [r \in Rounds |-> NoUnit]
  \* No commit_nowait has returned.
  /\ joined = [r \in Rounds |-> {}]
  \* No fsync ran.
  /\ synced = {}
  \* Nothing is visible.
  /\ visible = {}
  \* No wait was made ready off its own poll.
  /\ offPoll = {}
  \* Every stall is in force at the start.
  /\ stalled = Stalls
  \* No write has met a stall yet.
  /\ spc = [t \in Threads |-> [s \in Stalls |-> "none"]]
  \* No owner holds a timed wait.
  /\ timed = [t \in Threads |-> {}]
  \* No timed wait is due.
  /\ elapsed = {}
  \* Every queue is open.
  /\ dropped = {}

-----------------------------------------------------------------------------
\* HELPERS

\* Put note m at the back of queue q's inbox (QueueShared::deliver). If q
\* was marked idle, this push is the one that clears the mark and wakes q.
Push(q, m) ==
  \* The note goes to the back of q's inbox.
  /\ inbox' = [inbox EXCEPT ![q] = Append(@, m)]
  \* Any push clears the IDLE bit (the CAS writes a word without it).
  /\ idleFlag' = [idleFlag EXCEPT ![q] = FALSE]
  \* If the bit was set, this push wakes q's idle waker.
  /\ woken' = [woken EXCEPT ![q] = @ \/ idleFlag[q]]

\* Queue q keeps what unit u read, if it read anything, for q's re-runs.
Land(q, u) ==
  \* A landed unit leaves its block for q; a released one leaves nothing.
  have' = [have EXCEPT ![q] = IF outcome[u] = "landed" THEN @ \cup {what[u]} ELSE @]

\* Mark ready the waits of readers rs for thing x (WaitSlot::complete).
Ready(rs, x) ==
  \* Each reader in rs that waits on x is now ready to run again.
  slot' = [t \in Threads |-> IF t \in rs /\ slot[t][x] = "pending"
                             THEN [slot[t] EXCEPT ![x] = "ready"] ELSE slot[t]]

\* Is unit u free and held by nobody: in no queue's waiting set and no
\* inbox? Such a unit would never run.
Held(u) ==
  \* Some queue has registered on u, or has a request for u on its way.
  \E q \in Threads :
    \* q registered on u.
    \/ u \in waiting[q]
    \* A request for u sits in q's inbox.
    \/ \E i \in 1..Len(inbox[q]) : inbox[q][i].kind = "read" /\ inbox[q][i].u = u
    \* A request for u is among the notes q is handling.
    \/ \E i \in 1..Len(taken[q]) : taken[q][i].kind = "read" /\ taken[q][i].u = u

\* Is unit u passive: a stall's, which no poll runs and only the stall's
\* clearing lands?
Passive(u) == what[u] \in Stalls

\* Thread t's tasks have nothing to run: every goal is done or waiting.
Quiet(t) ==
  \* Each read is answered, or waits on the queue.
  /\ \A x \in Goal(t) : x \in done[t] \/ slot[t][x] = "pending"
  \* No stopped write is half way through registering its wait.
  /\ \A s \in Stalls : spc[t][s] = "none"
  \* Every job of t has been started.
  /\ \A j \in Jobs : JobOf[j] = t => j \in started
  \* t is not in the middle of running a unit.
  /\ \A u \in Ids : t \notin runners[u]
  \* t has no notes left to handle.
  /\ taken[t] = <<>>

-----------------------------------------------------------------------------
\* WHAT A READER DOES (on its own thread, while the thread is busy)

\* Thread t runs its read of block x and finds it: it landed for t's queue,
\* or the database is closed (the read then answers Closed). Either way the
\* read is over.
Hit(t, x) ==
  \* Only a running thread runs reads.
  /\ own[t] = "busy"
  \* t wants x and has no answer yet.
  /\ x \in Want[t] \ done[t]
  \* No earlier read of x is still waiting.
  /\ slot[t][x] = "none"
  \* The block is there, or the database is closed.
  /\ x \in have[t] \/ closedDb
  \* The read returns its answer.
  /\ done' = [done EXCEPT ![t] = @ \cup {x}]
  \* Nothing else changes.
  /\ UNCHANGED <<table, what, state, outcome, waiters, shut, regd, runners, runs,
                 toTell, told, next, inbox, idleFlag, taken, waiting, attached,
                 slot, have, started, own, woken, checking, closedDb>>
  \* The commit side is untouched.
  /\ UNCHANGED commitVars
  \* No timed wait is touched.
  /\ UNCHANGED timerVars

\* Thread t runs its read of block x and misses (IoRuntime::miss). It does
\* not touch the disk: it finds or makes the unit for x, puts a "read" note
\* on its own queue, and returns WouldBlock.
Miss(t, x) ==
  \* The unit the table holds for x now.
  LET cur == table[x]
      \* No live unit: this miss makes a new one (a finished one is replaced).
      fresh == cur = NoUnit \/ state[cur] = "done"
      \* The unit this read waits on.
      u == IF fresh THEN next ELSE cur
      \* The queue the note goes to: t's own, unless the WrongQueue bug.
      q == IF Mutant = "WrongQueue" THEN Other(t) ELSE t
  \* With those names, the step is:
  IN
  \* Only a running thread runs reads.
  /\ own[t] = "busy"
  \* t wants x and has no answer yet.
  /\ x \in Want[t] \ done[t]
  \* No earlier read of x is still waiting.
  /\ slot[t][x] = "none"
  \* The block has not landed for t.
  /\ x \notin have[t]
  \* The database is open (a closed one answers Closed, in Hit). In the code
  \* the read checked this when it started, and checks again as the miss
  \* counts itself in at the close gate (IoRuntime::miss, CloseGate::enter),
  \* so a miss either comes before close's mark or answers Closed.
  /\ ~closedDb
  \* A new unit needs a free number.
  /\ fresh => next <= MaxUnits
  \* A new unit goes into the table for x...
  /\ table' = IF fresh THEN [table EXCEPT ![x] = next] ELSE table
  \* ...reading x...
  /\ what' = IF fresh THEN [what EXCEPT ![next] = x] ELSE what
  \* ...and free: nobody has claimed it.
  /\ state' = IF fresh THEN [state EXCEPT ![next] = "free"] ELSE state
  \* The next number moves on when one was used.
  /\ next' = IF fresh THEN next + 1 ELSE next
  \* The "read" note goes to the queue.
  /\ Push(q, [kind |-> "read", u |-> u, reader |-> t])
  \* The read now waits.
  /\ slot' = [slot EXCEPT ![t][x] = "pending"]
  \* Nothing else changes.
  /\ UNCHANGED <<outcome, waiters, shut, regd, runners, runs, toTell, told, taken,
                 waiting, attached, have, done, started, own, checking, closedDb>>
  \* The commit side is untouched.
  /\ UNCHANGED commitVars
  \* No timed wait is touched.
  /\ UNCHANGED timerVars

\* A call on thread t makes regolith start job j itself (a flush with no
\* worker). The job takes the miss path: a unit, and a "read" note on the
\* starting thread's own queue. Bug SelfIoNowhere puts it on no queue.
SelfStart(t, j) ==
  \* Only a running thread makes calls.
  /\ own[t] = "busy"
  \* j belongs to t and has not started.
  /\ JobOf[j] = t /\ j \notin started
  \* A free unit number is needed.
  /\ next <= MaxUnits
  \* The job's unit goes into the table...
  /\ table' = [table EXCEPT ![j] = next]
  \* ...doing job j...
  /\ what' = [what EXCEPT ![next] = j]
  \* ...unclaimed.
  /\ state' = [state EXCEPT ![next] = "free"]
  \* The next number moves on.
  /\ next' = next + 1
  \* The job counts as started.
  /\ started' = started \cup {j}
  \* The call that started it waits for it on t's queue.
  /\ slot' = [slot EXCEPT ![t][j] = "pending"]
  \* The note goes to t's own queue; bug SelfIoNowhere sends it nowhere.
  /\ IF Mutant = "SelfIoNowhere"
       \* Nothing is pushed anywhere.
       THEN UNCHANGED <<inbox, idleFlag, woken>>
       \* The note lands on the starting thread's own queue.
       ELSE Push(t, [kind |-> "read", u |-> next, reader |-> t])
  \* Nothing else changes.
  /\ UNCHANGED <<outcome, waiters, shut, regd, runners, runs, toTell, told, taken,
                 waiting, attached, have, done, own, checking, closedDb>>
  \* The commit side is untouched.
  /\ UNCHANGED commitVars
  \* No timed wait is touched.
  /\ UNCHANGED timerVars

\* Thread t's wait for x is ready: it runs again (Hit or Miss decide what
\* happens). A job's caller sees its job finished, a commit's caller its
\* ticket ready, and a task that asked for a timed wait goes on; a stopped
\* write will run again (WriteTry).
Rerun(t, x) ==
  \* Only a running thread runs tasks.
  /\ own[t] = "busy"
  \* The wait for x is ready.
  /\ slot[t][x] = "ready"
  \* The wait is used up.
  /\ slot' = [slot EXCEPT ![t][x] = "none"]
  \* A job, a commit or a timed wait is finished for its caller; a read or a
  \* write will run again.
  /\ done' = IF x \in Jobs \cup Rounds \cup Timers
               \* Finished: it joins what t has done.
               THEN [done EXCEPT ![t] = @ \cup {x}]
               \* A read or a write: not done yet.
               ELSE done
  \* Nothing else changes.
  /\ UNCHANGED <<table, what, state, outcome, waiters, shut, regd, runners, runs,
                 toTell, told, next, inbox, idleFlag, taken, waiting, attached,
                 have, started, own, woken, checking, closedDb>>
  \* The commit side is untouched.
  /\ UNCHANGED commitVars
  \* No timed wait is touched.
  /\ UNCHANGED timerVars

-----------------------------------------------------------------------------
\* WHAT A COMMITTER DOES (on its own thread): commit_nowait

\* Thread t's commit_nowait returns, its commit waiting on round r. The
\* first such commit finds the round with nobody on it (in the code the
\* round is there already: each round installs the next before it lands,
\* src/engine/commit/round.rs). Every such commit puts a note for the
\* round's unit on its OWN queue and returns with a ticket that waits on
\* it. Bug PublishAtWrite makes the round's commits visible as soon as they
\* are written.
CommitNowait(t, r) ==
  \* The unit the round has, or the one the first commit to wait gives it now.
  LET fresh == roundUnit[r] = NoUnit
      \* The unit this commit's ticket waits on.
      u == IF fresh THEN next ELSE roundUnit[r]
  \* With those names, the step is:
  IN
  \* Only a running thread commits.
  /\ own[t] = "busy"
  \* t has a commit round r covers, and its commit_nowait has not returned.
  /\ t \in CommittersOf[r] /\ t \notin joined[r]
  \* The round was on a queue before close, or the database is open (a commit
  \* close came before is CommitClosed).
  /\ ~closedDb \/ ~fresh
  \* A new unit needs a free number.
  /\ fresh => next <= MaxUnits
  \* The first commit to wait puts the unit in the table, free.
  /\ table' = IF fresh THEN [table EXCEPT ![r] = next] ELSE table
  \* It stands for the round.
  /\ what' = IF fresh THEN [what EXCEPT ![next] = r] ELSE what
  \* Nobody has claimed it.
  /\ state' = IF fresh THEN [state EXCEPT ![next] = "free"] ELSE state
  \* The next number moves on when one was used.
  /\ next' = IF fresh THEN next + 1 ELSE next
  \* The round remembers its unit.
  /\ roundUnit' = IF fresh THEN [roundUnit EXCEPT ![r] = next] ELSE roundUnit
  \* t's commit returned.
  /\ joined' = [joined EXCEPT ![r] = @ \cup {t}]
  \* The note goes to t's own queue.
  /\ Push(t, [kind |-> "read", u |-> u, reader |-> t])
  \* The ticket waits.
  /\ slot' = [slot EXCEPT ![t][r] = "pending"]
  \* The bug publishes the round's commits before any fsync.
  /\ visible' = IF Mutant = "PublishAtWrite" THEN visible \cup {r} ELSE visible
  \* Nothing else changes.
  /\ UNCHANGED <<outcome, waiters, shut, regd, runners, runs, toTell, told, taken,
                 waiting, attached, have, done, started, own, checking, closedDb>>
  \* The rest of the commit side is untouched.
  /\ UNCHANGED <<synced, offPoll, stalled, spc>>
  \* No timed wait is touched.
  /\ UNCHANGED timerVars

\* Thread t's commit_nowait comes after close, and no round was on a queue
\* for it: it is refused with Closed, and its ticket is ready at once, on t.
CommitClosed(t, r) ==
  \* Only a running thread commits.
  /\ own[t] = "busy"
  \* t has a commit round r covers, and its commit_nowait has not returned.
  /\ t \in CommittersOf[r] /\ t \notin joined[r]
  \* Close came first, before any commit waited on the round.
  /\ closedDb /\ roundUnit[r] = NoUnit
  \* t's commit returned.
  /\ joined' = [joined EXCEPT ![r] = @ \cup {t}]
  \* With Closed: the commit is over for its caller.
  /\ done' = [done EXCEPT ![t] = @ \cup {r}]
  \* Nothing else changes.
  /\ UNCHANGED <<table, what, state, outcome, waiters, shut, regd, runners, runs,
                 toTell, told, next, inbox, idleFlag, taken, waiting, attached,
                 slot, have, started, own, woken, checking, closedDb>>
  \* The rest of the commit side is untouched.
  /\ UNCHANGED <<roundUnit, synced, visible, offPoll, stalled, spc>>
  \* No timed wait is touched.
  /\ UNCHANGED timerVars

\* Thread t, not polling, needs the log durable (a blocking commit, a
\* rotation, an ordered unit) and finds the round with nobody running it:
\* it claims the round's unit with the same one CAS
\* (RegolithEngine::run_or_wait, src/engine/commit/park.rs). It need not
\* have a commit the round covers.
LandHere(t, u) ==
  \* Only a running thread does this.
  /\ own[t] = "busy"
  \* u is a round.
  /\ what[u] \in Rounds
  \* The CAS succeeds only on a free unit (the bug: also on a claimed one).
  /\ \/ state[u] = "free"
     \* The bug: a unit someone else already claimed.
     \/ Mutant = "DoubleRun" /\ state[u] = "claimed" /\ t \notin runners[u]
  \* The unit is claimed.
  /\ state' = [state EXCEPT ![u] = "claimed"]
  \* t is running it now.
  /\ runners' = [runners EXCEPT ![u] = @ \cup {t}]
  \* Nothing else changes.
  /\ UNCHANGED <<table, what, outcome, waiters, shut, regd, runs, toTell, told,
                 next, inbox, idleFlag, taken, waiting, attached, slot, have,
                 done, started, own, woken, checking, closedDb>>
  \* The commit side is untouched.
  /\ UNCHANGED commitVars
  \* No timed wait is touched.
  /\ UNCHANGED timerVars

-----------------------------------------------------------------------------
\* WHAT A WRITER DOES UNDER A STALL (on its own thread)

\* Thread t runs its write of stall s's kind. With the stall cleared it
\* applies and is done; under the stall it is stopped and goes on to
\* register a wait (it never sleeps).
WriteTry(t, s) ==
  \* Only a running thread writes.
  /\ own[t] = "busy"
  \* The stall stops t's write, which is not done.
  /\ t \in StoppedBy[s] /\ s \notin done[t]
  \* No wait of this write is outstanding, and it is not mid-way.
  /\ slot[t][s] = "none" /\ spc[t][s] = "none"
  \* Applied when clear (or answered Closed); stopped otherwise.
  /\ IF s \notin stalled \/ closedDb
       \* The write is over.
       THEN /\ done' = [done EXCEPT ![t] = @ \cup {s}]
            \* Nothing to register.
            /\ UNCHANGED spc
       \* It saw the stall.
       ELSE /\ spc' = [spc EXCEPT ![t][s] = "seen"]
            \* Not done.
            /\ UNCHANGED done
  \* Nothing else changes.
  /\ UNCHANGED <<table, what, state, outcome, waiters, shut, regd, runners, runs,
                 toTell, told, next, inbox, idleFlag, taken, waiting, attached,
                 slot, have, started, own, woken, checking, closedDb>>
  \* The rest of the commit side is untouched.
  /\ UNCHANGED <<roundUnit, joined, synced, visible, offPoll, stalled>>
  \* No timed wait is touched.
  /\ UNCHANGED timerVars

\* The stopped writer finds the stall's unit, or installs a fresh passive
\* one, and puts a note for it on its own queue (StallSignal::wait). It has
\* not looked at the stall again yet.
StallWait(t, s) ==
  \* The unit the table holds for the stall now.
  LET cur == table[s]
      \* None live: this writer installs one.
      fresh == cur = NoUnit \/ state[cur] = "done"
      \* The unit this write waits on.
      u == IF fresh THEN next ELSE cur
  \* With those names, the step is:
  IN
  \* Only a running thread writes.
  /\ own[t] = "busy"
  \* The writer saw the stall.
  /\ spc[t][s] = "seen"
  \* A new unit needs a free number.
  /\ fresh => next <= MaxUnits
  \* A new unit goes into the table, standing for the stall, unlanded.
  /\ table' = IF fresh THEN [table EXCEPT ![s] = next] ELSE table
  \* It stands for the stall.
  /\ what' = IF fresh THEN [what EXCEPT ![next] = s] ELSE what
  \* Free: not landed (and, being passive, never claimed).
  /\ state' = IF fresh THEN [state EXCEPT ![next] = "free"] ELSE state
  \* The next number moves on when one was used.
  /\ next' = IF fresh THEN next + 1 ELSE next
  \* The note goes to t's own queue.
  /\ Push(t, [kind |-> "read", u |-> u, reader |-> t])
  \* The write's wait is pending.
  /\ slot' = [slot EXCEPT ![t][s] = "pending"]
  \* Now it looks at the stall again.
  /\ spc' = [spc EXCEPT ![t][s] = "recheck"]
  \* Nothing else changes.
  /\ UNCHANGED <<outcome, waiters, shut, regd, runners, runs, toTell, told, taken,
                 waiting, attached, have, done, started, own, checking, closedDb>>
  \* The rest of the commit side is untouched.
  /\ UNCHANGED <<roundUnit, joined, synced, visible, offPoll, stalled>>
  \* No timed wait is touched.
  /\ UNCHANGED timerVars

\* Land the stall unit u: it ends without a read, its waiter list is shut,
\* and every queue on it is owed a note (Job::release of a passive job).
LandStall(u) ==
  \* u ends here.
  /\ state' = [state EXCEPT ![u] = "done"]
  \* It landed: the stall cleared.
  /\ outcome' = [outcome EXCEPT ![u] = "landed"]
  \* Its waiter list is shut.
  /\ shut' = [shut EXCEPT ![u] = TRUE]
  \* Every registered queue is owed a note.
  /\ toTell' = [toTell EXCEPT ![u] = waiters[u]]
  \* The list was taken whole.
  /\ waiters' = [waiters EXCEPT ![u] = {}]
  \* The table forgets it.
  /\ table' = [table EXCEPT ![what[u]] = NoUnit]

\* The writer looks at the stall again, now that its wait is recorded: if
\* the stall cleared meanwhile, it lands the unit itself, so a clearer that
\* came before the registration is not missed. Bug NoRecheck skips this.
StallRecheck(t, s) ==
  \* The unit the table holds for the stall now.
  LET u == table[s]
  \* With that name, the step is:
  IN
  \* Only a running thread writes.
  /\ own[t] = "busy"
  \* The writer registered and is about to look again.
  /\ spc[t][s] = "recheck"
  \* The look is over.
  /\ spc' = [spc EXCEPT ![t][s] = "none"]
  \* Cleared, with a unit still unlanded: land it (the bug never does).
  /\ IF s \notin stalled /\ u # NoUnit /\ state[u] = "free" /\ Mutant # "NoRecheck"
       \* The writer lands the unit.
       THEN LandStall(u)
       \* Still stalled (the clearer will land it), or nothing to land.
       ELSE UNCHANGED <<state, outcome, shut, toTell, waiters, table>>
  \* Nothing else changes.
  /\ UNCHANGED <<what, regd, runners, runs, told, next, inbox, idleFlag, taken,
                 waiting, attached, slot, have, done, started, own, woken,
                 checking, closedDb>>
  \* The rest of the commit side is untouched.
  /\ UNCHANGED <<roundUnit, joined, synced, visible, offPoll, stalled>>
  \* No timed wait is touched.
  /\ UNCHANGED timerVars

\* Background work (a flush, a compaction pass) clears stall s and lands
\* the unit its stopped writers wait on, if there is one (StallSignal::
\* refresh). It records the cleared level before it takes the unit.
Clear(s) ==
  \* The unit the table holds for the stall now.
  LET u == table[s]
  \* With that name, the step is:
  IN
  \* The stall is in force.
  /\ s \in stalled
  \* It is cleared.
  /\ stalled' = stalled \ {s}
  \* A unit still unlanded is landed now.
  /\ IF u # NoUnit /\ state[u] = "free"
       \* Land it.
       THEN LandStall(u)
       \* Nothing to land.
       ELSE UNCHANGED <<state, outcome, shut, toTell, waiters, table>>
  \* Nothing else changes.
  /\ UNCHANGED <<what, regd, runners, runs, told, next, inbox, idleFlag, taken,
                 waiting, attached, slot, have, done, started, own, woken,
                 checking, closedDb>>
  \* The rest of the commit side is untouched.
  /\ UNCHANGED <<roundUnit, joined, synced, visible, offPoll, spc>>
  \* No timed wait is touched.
  /\ UNCHANGED timerVars

-----------------------------------------------------------------------------
\* WHAT A QUEUE'S OWNER DOES WHEN IT POLLS (IoQueue::poll)

\* The owner takes its whole inbox in one swap, which also clears IDLE.
Take(t) ==
  \* Only a running owner polls.
  /\ own[t] = "busy"
  \* Something is in the inbox.
  /\ inbox[t] # <<>>
  \* The notes join the ones being handled, oldest first.
  /\ taken' = [taken EXCEPT ![t] = @ \o inbox[t]]
  \* The inbox is empty now.
  /\ inbox' = [inbox EXCEPT ![t] = <<>>]
  \* The swap writes a word with no IDLE bit.
  /\ idleFlag' = [idleFlag EXCEPT ![t] = FALSE]
  \* Nothing else changes.
  /\ UNCHANGED <<table, what, state, outcome, waiters, shut, regd, runners, runs,
                 toTell, told, next, waiting, attached, slot, have, done, started,
                 own, woken, checking, closedDb>>
  \* The commit side is untouched.
  /\ UNCHANGED commitVars
  \* No timed wait is touched.
  /\ UNCHANGED timerVars

\* The owner handles its oldest taken note.
Handle(t) ==
  \* Only a running owner handles notes.
  /\ own[t] = "busy"
  \* There is a note to handle.
  /\ taken[t] # <<>>
  \* The note, and the unit it names.
  /\ LET m == Head(taken[t])
         \* The unit the note is about.
         u == m.u
     \* With those names, the step is:
     IN
     \* The note is handled now.
     /\ taken' = [taken EXCEPT ![t] = Tail(@)]
     \* A "timer" note: the owner takes the wait in and holds it until it is
     \* due (Timers::record). Any other note leaves the held waits alone.
     /\ timed' = IF m.kind = "timer" THEN [timed EXCEPT ![t] = @ \cup {m.u}]
                                     \* Not a "timer" note: as they were.
                                     ELSE timed
     \* What else happens depends on the note and the unit:
     /\ CASE
          \* A read of a unit this queue already registered on: the read
          \* joins it (one more WaitSlot in the waiting entry).
          m.kind = "read" /\ u \in waiting[t] ->
            \* The reader is recorded with the unit.
            /\ attached' = [attached EXCEPT ![t][u] = @ \cup {m.reader}]
            \* Nothing else changes.
            /\ UNCHANGED <<waiters, shut, regd, waiting, slot, have>>
          \* A read of a unit this queue has not registered on, still open:
          \* register (one CAS push onto the unit's waiter Stack).
          [] m.kind = "read" /\ u \notin waiting[t] /\ ~shut[u] ->
            \* This queue is on the unit's waiter list now.
            /\ waiters' = [waiters EXCEPT ![u] = @ \cup {t}]
            \* Ghost: the registration succeeded.
            /\ regd' = [regd EXCEPT ![u] = @ \cup {t}]
            \* The queue waits on the unit.
            /\ waiting' = [waiting EXCEPT ![t] = @ \cup {u}]
            \* The reader is recorded with the unit.
            /\ attached' = [attached EXCEPT ![t][u] = {m.reader}]
            \* Nothing else changes.
            /\ UNCHANGED <<shut, slot, have>>
          \* A read of a unit that finished before the queue could register:
          \* the push is refused, and the queue reads the outcome itself.
          [] m.kind = "read" /\ u \notin waiting[t] /\ shut[u] ->
            \* Whatever the unit read lands for this queue.
            /\ Land(t, u)
            \* The reader's wait is ready.
            /\ Ready({m.reader}, what[u])
            \* Nothing else changes.
            /\ UNCHANGED <<waiters, shut, regd, waiting, attached>>
          \* A unit this queue waits on finished: what it read lands, and
          \* every read the queue recorded for it is ready.
          [] m.kind = "done" /\ u \in waiting[t] ->
            \* The queue stops waiting on the unit.
            /\ waiting' = [waiting EXCEPT ![t] = @ \ {u}]
            \* Whatever the unit read lands for this queue.
            /\ Land(t, u)
            \* Every read recorded here for the unit is ready.
            /\ Ready(attached[t][u], what[u])
            \* The entry is gone.
            /\ attached' = [attached EXCEPT ![t][u] = {}]
            \* Nothing else changes.
            /\ UNCHANGED <<waiters, shut, regd>>
          \* A "done" note for a unit the queue no longer waits on, or a
          \* "timer" note (recorded above): nothing about units changes.
          [] OTHER ->
            \* Nothing changes.
            UNCHANGED <<waiters, shut, regd, waiting, attached, slot, have>>
  \* Nothing else changes.
  /\ UNCHANGED <<table, what, state, outcome, runners, runs, toTell, told, next,
                 inbox, idleFlag, done, started, own, woken, checking, closedDb>>
  \* The commit side is untouched.
  /\ UNCHANGED commitVars
  \* The clock and the dropped queues are untouched.
  /\ UNCHANGED <<elapsed, dropped>>

\* The owner claims a unit it waits on with ONE compare-and-swap from free
\* to claimed (Unit::claim). Bug DoubleRun also lets a claimed unit be
\* claimed again.
Claim(t, u) ==
  \* Only a running owner claims.
  /\ own[t] = "busy"
  \* The queue waits on u.
  /\ u \in waiting[t]
  \* A passive unit (a stall's) is never claimed: only its clearing lands it.
  /\ ~Passive(u)
  \* The CAS succeeds only on a free unit (the bug: also on a claimed one).
  /\ \/ state[u] = "free"
     \* The bug: a unit someone else already claimed.
     \/ Mutant = "DoubleRun" /\ state[u] = "claimed" /\ t \notin runners[u]
  \* The unit is claimed.
  /\ state' = [state EXCEPT ![u] = "claimed"]
  \* t is running it now.
  /\ runners' = [runners EXCEPT ![u] = @ \cup {t}]
  \* Nothing else changes.
  /\ UNCHANGED <<table, what, outcome, waiters, shut, regd, runs, toTell, told,
                 next, inbox, idleFlag, taken, waiting, attached, slot, have,
                 done, started, own, woken, checking, closedDb>>
  \* The commit side is untouched.
  /\ UNCHANGED commitVars
  \* No timed wait is touched.
  /\ UNCHANGED timerVars

\* The claimer reads the block and finishes the unit (Unit::finish): it
\* writes the outcome, marks the unit done, shuts the waiter list in ONE
\* swap (taking every registered queue), and drops the unit from the table.
\* For a round's unit the "read" is the round's fsync: its commits are
\* synced, then published (visible). Bug DeliverOnLander also makes every
\* registered commit's ticket ready right here, on the runner's thread,
\* instead of at each one's own poll.
Finish(t, u) ==
  \* t is running u.
  /\ t \in runners[u]
  \* t is done running it.
  /\ runners' = [runners EXCEPT ![u] = @ \ {t}]
  \* Ghost: the disk read ran once more.
  /\ runs' = [runs EXCEPT ![u] = @ + 1]
  \* The first finisher ends the unit; under the bug a second finisher
  \* finds it already done and only adds a read.
  /\ IF state[u] = "claimed"
       \* The unit ends here.
       THEN /\ state' = [state EXCEPT ![u] = "done"]
            \* It read its block.
            /\ outcome' = [outcome EXCEPT ![u] = "landed"]
            \* The waiter list is shut: later registrations are refused.
            /\ shut' = [shut EXCEPT ![u] = TRUE]
            \* Every queue on the list is owed one "done" note.
            /\ toTell' = [toTell EXCEPT ![u] = waiters[u]]
            \* The list is empty (it was taken whole).
            /\ waiters' = [waiters EXCEPT ![u] = {}]
            \* The table forgets the unit.
            /\ table' = IF table[what[u]] = u THEN [table EXCEPT ![what[u]] = NoUnit] ELSE table
       \* Already ended by the first finisher.
       ELSE UNCHANGED <<state, outcome, shut, toTell, waiters, table>>
  \* A round's fsync ran: its commits are durable, then visible.
  /\ synced' = IF what[u] \in Rounds THEN synced \cup {what[u]} ELSE synced
  \* Applied and published only after the fsync.
  /\ visible' = IF what[u] \in Rounds THEN visible \cup {what[u]} ELSE visible
  \* The bug readies each registered commit's ticket on this thread.
  /\ IF Mutant = "DeliverOnLander" /\ what[u] \in Rounds /\ state[u] = "claimed"
       \* Every registered commit's wait is ready now, made so by t.
       THEN /\ Ready(regd[u], what[u])
            \* Ghost: each of those, not t, was readied off its own poll.
            /\ offPoll' = offPoll \cup {<<q, what[u]>> : q \in regd[u] \ {t}}
       \* The real code: readiness comes only at each commit's own queue's poll.
       ELSE UNCHANGED <<slot, offPoll>>
  \* Nothing else changes.
  /\ UNCHANGED <<what, regd, told, next, inbox, idleFlag, taken, waiting,
                 attached, have, done, started, own, woken, checking, closedDb>>
  \* The rest of the commit side is untouched.
  /\ UNCHANGED <<roundUnit, joined, stalled, spc>>
  \* No timed wait is touched.
  /\ UNCHANGED timerVars

\* The finishing thread puts the "done" note for u into queue q's inbox:
\* one push per queue it took off the waiter list.
Tell(u, q) ==
  \* q is still owed its note.
  /\ q \in toTell[u]
  \* The note goes to q (and wakes q if q is idle).
  /\ Push(q, [kind |-> "done", u |-> u, reader |-> 0])
  \* q is no longer owed.
  /\ toTell' = [toTell EXCEPT ![u] = @ \ {q}]
  \* Ghost: q was given one more note for u.
  /\ told' = [told EXCEPT ![q][u] = @ + 1]
  \* Nothing else changes.
  /\ UNCHANGED <<table, what, state, outcome, waiters, shut, regd, runners, runs,
                 next, taken, waiting, attached, slot, have, done, started,
                 own, checking, closedDb>>
  \* The commit side is untouched.
  /\ UNCHANGED commitVars
  \* No timed wait is touched.
  /\ UNCHANGED timerVars

-----------------------------------------------------------------------------
\* GOING IDLE AND WAKING UP (IoQueue::idle_waker, QueueShared::rest)

\* The owner has nothing to run and goes idle: it registers its waker and
\* sets IDLE with one CAS that succeeds only on an empty inbox. If a unit
\* it waits on is still free (only it can run that one), or a note is
\* already in the inbox, it is woken at once instead: this action is then
\* simply not taken. Bug LostIdleWakeup does the "inbox empty?" look and
\* the marking as two steps.
Rest(t) ==
  \* Only a running owner goes idle.
  /\ own[t] = "busy"
  \* Its tasks have nothing to run.
  /\ Quiet(t)
  \* No unit it waits on is left for it to run (a passive one is not its to
  \* run: it waits for the stall to clear).
  /\ \A u \in waiting[t] : state[u] # "free" \/ Passive(u)
  \* Its inbox is empty (the CAS checks this).
  /\ inbox[t] = <<>>
  \* No timed wait it holds is due: with one due it is woken at once
  \* instead, to poll and complete it (IoQueue::idle_waker).
  /\ \A x \in timed[t] : x \notin elapsed
  \* Not already half way into going idle.
  /\ ~checking[t]
  \* How the owner goes idle depends on the bug:
  /\ IF Mutant = "LostIdleWakeup"
       \* The bug: only the look happens now; the mark comes later.
       THEN /\ checking' = [checking EXCEPT ![t] = TRUE]
            \* Nothing else changes yet.
            /\ UNCHANGED <<idleFlag, own>>
       \* The real code: the look and the mark are the same CAS.
       ELSE /\ idleFlag' = [idleFlag EXCEPT ![t] = TRUE]
            \* The owner sleeps.
            /\ own' = [own EXCEPT ![t] = "idle"]
            \* Nothing else changes.
            /\ UNCHANGED checking
  \* Nothing else changes.
  /\ UNCHANGED <<table, what, state, outcome, waiters, shut, regd, runners, runs,
                 toTell, told, next, inbox, taken, waiting, attached, slot, have,
                 done, started, woken, closedDb>>
  \* The commit side is untouched.
  /\ UNCHANGED commitVars
  \* No timed wait is touched.
  /\ UNCHANGED timerVars

\* Bug LostIdleWakeup, second half: mark idle without looking again. A note
\* pushed between the look and this mark saw no IDLE bit and woke nobody.
MarkIdle(t) ==
  \* The look was done.
  /\ checking[t]
  \* The look is over.
  /\ checking' = [checking EXCEPT ![t] = FALSE]
  \* The IDLE bit is set, whatever the inbox holds now.
  /\ idleFlag' = [idleFlag EXCEPT ![t] = TRUE]
  \* The owner sleeps.
  /\ own' = [own EXCEPT ![t] = "idle"]
  \* Nothing else changes.
  /\ UNCHANGED <<table, what, state, outcome, waiters, shut, regd, runners, runs,
                 toTell, told, next, inbox, taken, waiting, attached, slot, have,
                 done, started, woken, closedDb>>
  \* The commit side is untouched.
  /\ UNCHANGED commitVars
  \* No timed wait is touched.
  /\ UNCHANGED timerVars

\* An idle owner whose waker fired runs again.
Wake(t) ==
  \* It was asleep.
  /\ own[t] = "idle"
  \* Its waker fired.
  /\ woken[t]
  \* It runs.
  /\ own' = [own EXCEPT ![t] = "busy"]
  \* The wake is used up.
  /\ woken' = [woken EXCEPT ![t] = FALSE]
  \* Nothing else changes.
  /\ UNCHANGED <<table, what, state, outcome, waiters, shut, regd, runners, runs,
                 toTell, told, next, inbox, idleFlag, taken, waiting, attached,
                 slot, have, done, started, checking, closedDb>>
  \* The commit side is untouched.
  /\ UNCHANGED commitVars
  \* No timed wait is touched.
  /\ UNCHANGED timerVars

\* An idle owner runs again on its own (its executor got other work) and
\* polls, which clears IDLE first (QueueShared::wake_up). Bug BusyWoken
\* forgets to clear it. Not fair: an owner may never do this.
SelfWake(t) ==
  \* It was asleep.
  /\ own[t] = "idle"
  \* It runs.
  /\ own' = [own EXCEPT ![t] = "busy"]
  \* Its poll clears IDLE (the bug leaves it set).
  /\ idleFlag' = IF Mutant = "BusyWoken" THEN idleFlag ELSE [idleFlag EXCEPT ![t] = FALSE]
  \* A wake that fired meanwhile is used up by this run.
  /\ woken' = [woken EXCEPT ![t] = FALSE]
  \* Nothing else changes.
  /\ UNCHANGED <<table, what, state, outcome, waiters, shut, regd, runners, runs,
                 toTell, told, next, inbox, taken, waiting, attached, slot, have,
                 done, started, checking, closedDb>>
  \* The commit side is untouched.
  /\ UNCHANGED commitVars
  \* No timed wait is touched.
  /\ UNCHANGED timerVars

-----------------------------------------------------------------------------
\* TIMED WAITS (IoRuntime::timer, IoQueue::fire_timers, Drop for IoQueue)
\*
\* A task that must wait a while without blocking asks its queue for a timed
\* wait. Nobody is woken for one and nothing sleeps for one: the owner's
\* poll completes it once the clock has reached its due time, and each poll
\* tells the owner how long it may idle until the next one is due
\* (IoProgress::wake_in). The clock is not a number here: `elapsed` holds
\* the waits whose due time has passed.

\* Thread t asks its queue for timed wait x (IoRuntime::timer): a "timer"
\* note goes to the queue's inbox, as any request does, and the task waits.
\* On a dropped queue nothing would ever complete the wait, so it comes back
\* ready. Bug WrongQueue puts the note on the other thread's queue.
AskTimer(t, x) ==
  \* The queue the note goes to: t's own, unless the WrongQueue bug.
  LET q == IF Mutant = "WrongQueue" THEN Other(t) ELSE t
  \* With that name, the step is:
  IN
  \* Only a running thread asks.
  /\ own[t] = "busy"
  \* The wait is t's, and t's task has not been through it yet.
  /\ TimerOf[x] = t /\ x \notin done[t]
  \* It is not asked for already.
  /\ slot[t][x] = "none"
  \* What happens depends on whether the queue is still there:
  /\ IF t \in dropped
       \* Gone: the wait is ready at once, on the asker's own thread.
       THEN /\ slot' = [slot EXCEPT ![t][x] = "ready"]
            \* Nothing is pushed anywhere.
            /\ UNCHANGED <<inbox, idleFlag, woken>>
       \* There: the note goes to the queue, waking an idle owner once.
       ELSE /\ Push(q, [kind |-> "timer", u |-> x, reader |-> t])
            \* The task waits.
            /\ slot' = [slot EXCEPT ![t][x] = "pending"]
  \* Nothing else changes.
  /\ UNCHANGED <<table, what, state, outcome, waiters, shut, regd, runners, runs,
                 toTell, told, next, taken, waiting, attached, have, done,
                 started, own, checking, closedDb>>
  \* The commit side is untouched.
  /\ UNCHANGED commitVars
  \* No held wait, the clock, or a dropped queue changes.
  /\ UNCHANGED timerVars

\* The clock reaches timed wait x's due time. The due time is fixed when the
\* wait is asked for and lies after that moment, so only a wait asked for
\* and not yet completed comes due.
Elapse(x) ==
  \* The wait is asked for and still pending.
  /\ slot[TimerOf[x]][x] = "pending"
  \* Its due time has not passed yet.
  /\ x \notin elapsed
  \* Now it has.
  /\ elapsed' = elapsed \cup {x}
  \* Nothing else changes.
  /\ UNCHANGED <<table, what, state, outcome, waiters, shut, regd, runners, runs,
                 toTell, told, next, inbox, idleFlag, taken, waiting, attached,
                 slot, have, done, started, own, woken, checking, closedDb>>
  \* The commit side is untouched.
  /\ UNCHANGED commitVars
  \* No held wait and no dropped queue changes.
  /\ UNCHANGED <<timed, dropped>>

\* The owner's poll completes timed wait x, which it holds and which is due
\* (IoQueue::fire_timers, the last thing a poll does, after the inbox was
\* taken in whole). Bug EarlyFire does not look at the clock.
Fire(t, x) ==
  \* Only a running owner polls.
  /\ own[t] = "busy"
  \* The owner holds the wait.
  /\ x \in timed[t]
  \* Every note the poll took is handled.
  /\ taken[t] = <<>>
  \* The clock reached the due time (the bug does not ask).
  /\ x \in elapsed \/ Mutant = "EarlyFire"
  \* The owner no longer holds it.
  /\ timed' = [timed EXCEPT ![t] = @ \ {x}]
  \* The task that asked may run again (WaitSlot::complete).
  /\ Ready({TimerOf[x]}, x)
  \* Ghost: a wait made ready by a poll that is not its own thread's.
  /\ offPoll' = IF TimerOf[x] = t THEN offPoll
                                 \* Another thread's wait: readied off its poll.
                                 ELSE offPoll \cup {<<TimerOf[x], x>>}
  \* Nothing else changes.
  /\ UNCHANGED <<table, what, state, outcome, waiters, shut, regd, runners, runs,
                 toTell, told, next, inbox, idleFlag, taken, waiting, attached,
                 have, done, started, own, woken, checking, closedDb>>
  \* The rest of the commit side is untouched.
  /\ UNCHANGED <<roundUnit, joined, synced, visible, stalled, spc>>
  \* The clock and the dropped queues are untouched.
  /\ UNCHANGED <<elapsed, dropped>>

\* An idle owner that holds a due timed wait runs again: it idled no longer
\* than its last poll said it might (IoProgress::wake_in; IoQueue::block_on
\* parks for that long and no longer). Nobody pushes anything for a timed
\* wait, so this is the one way out of idle a due wait gives. Like any poll
\* it clears IDLE first. Bug IdlePastDue is an owner that ignores what its
\* poll said and stays idle.
TimerWake(t) ==
  \* It was asleep.
  /\ own[t] = "idle"
  \* A timed wait it holds is due.
  /\ \E x \in timed[t] : x \in elapsed
  \* The owner follows what its poll reported (the bug does not).
  /\ Mutant # "IdlePastDue"
  \* It runs.
  /\ own' = [own EXCEPT ![t] = "busy"]
  \* Its poll clears IDLE.
  /\ idleFlag' = [idleFlag EXCEPT ![t] = FALSE]
  \* A wake that fired meanwhile is used up by this run.
  /\ woken' = [woken EXCEPT ![t] = FALSE]
  \* Nothing else changes.
  /\ UNCHANGED <<table, what, state, outcome, waiters, shut, regd, runners, runs,
                 toTell, told, next, inbox, taken, waiting, attached, slot, have,
                 done, started, checking, closedDb>>
  \* The commit side is untouched.
  /\ UNCHANGED commitVars
  \* No timed wait is touched.
  /\ UNCHANGED timerVars

\* Thread t drops its queue (Drop for IoQueue). Nothing would ever complete
\* a timed wait recorded on it, so the drop completes every one: those the
\* owner holds (Timers::complete_all) and those still a note in the inbox.
\* Bug DropForgets leaves them pending.
DropQueue(t) ==
  \* The thread may drop its queue, and has not.
  /\ t \in Droppers \ dropped
  \* The owner drops it, on its own thread, while running.
  /\ own[t] = "busy"
  \* The queue is gone.
  /\ dropped' = dropped \cup {t}
  \* Every timed wait of t that is pending is ready (the bug forgets them).
  /\ slot' = [slot EXCEPT ![t] =
                [x \in Things |-> IF x \in Timers /\ slot[t][x] = "pending"
                                     /\ Mutant # "DropForgets"
                                  THEN "ready" ELSE slot[t][x]]]
  \* The owner holds none any more.
  /\ timed' = [timed EXCEPT ![t] = {}]
  \* Its inbox is emptied: only "timer" notes were in it, since a thread
  \* that may drop its queue has no other wait.
  /\ inbox' = [inbox EXCEPT ![t] = <<>>]
  \* So are the notes it had taken.
  /\ taken' = [taken EXCEPT ![t] = <<>>]
  \* Nothing else changes.
  /\ UNCHANGED <<table, what, state, outcome, waiters, shut, regd, runners, runs,
                 toTell, told, next, idleFlag, waiting, attached, have, done,
                 started, own, woken, checking, closedDb>>
  \* The commit side is untouched.
  /\ UNCHANGED commitVars
  \* The clock is untouched.
  /\ UNCHANGED elapsed

-----------------------------------------------------------------------------
\* CLOSE (RegolithEngine::close -> IoRuntime::close)
\*
\* Close marks the gate (CloseGate::close), then sweeps the table. A miss
\* already counted in when the mark lands releases its own unit when it
\* counts itself out (CloseGate::leave), so Release below stands for the
\* sweep and for that miss alike. Close does not wait for that miss: the
\* promise is that once close has returned and the miss has ended, no unit
\* is left in the table. The loom model no_unit_outlives_close checks the
\* gate itself at that boundary, witnesses that close can return with the
\* unit still in the table, and has a calibration that checks for close
\* only when the miss starts and leaves a unit behind.

\* The database closes. Reads run after this answer Closed.
Close ==
  \* Not closed yet.
  /\ ~closedDb
  \* Closed now.
  /\ closedDb' = TRUE
  \* Nothing else changes.
  /\ UNCHANGED <<table, what, state, outcome, waiters, shut, regd, runners, runs,
                 toTell, told, next, inbox, idleFlag, taken, waiting, attached,
                 slot, have, done, started, own, woken, checking>>
  \* The commit side is untouched.
  /\ UNCHANGED commitVars
  \* No timed wait is touched.
  /\ UNCHANGED timerVars

\* Close lets a free unit in the table go without reading it: it claims it
\* (so no runner can) and finishes it as "released", telling every
\* registered queue. A unit someone is running finishes on its own. A
\* round's unit is the exception: close lands it, running its fsync (the
\* final sync covers it, so every waiting commit's ticket says committed); a stall's
\* unit lands too (close tells every stopped writer).
Release(u) ==
  \* The database is closing.
  /\ closedDb
  \* u is free and in the table.
  /\ state[u] = "free" /\ table[what[u]] = u
  \* u ends here.
  /\ state' = [state EXCEPT ![u] = "done"]
  \* A block or job read nothing; a round or a stall landed.
  /\ outcome' = [outcome EXCEPT ![u] = IF what[u] \in Rounds \cup Stalls
                                         THEN "landed" ELSE "released"]
  \* A round's fsync ran once, here.
  /\ runs' = IF what[u] \in Rounds THEN [runs EXCEPT ![u] = @ + 1] ELSE runs
  \* So the round's commits are durable...
  /\ synced' = IF what[u] \in Rounds THEN synced \cup {what[u]} ELSE synced
  \* ...and then visible.
  /\ visible' = IF what[u] \in Rounds THEN visible \cup {what[u]} ELSE visible
  \* Its waiter list is shut.
  /\ shut' = [shut EXCEPT ![u] = TRUE]
  \* Every registered queue is owed a note.
  /\ toTell' = [toTell EXCEPT ![u] = waiters[u]]
  \* The list was taken whole.
  /\ waiters' = [waiters EXCEPT ![u] = {}]
  \* The table forgets it.
  /\ table' = [table EXCEPT ![what[u]] = NoUnit]
  \* Nothing else changes.
  /\ UNCHANGED <<what, regd, runners, told, next, inbox, idleFlag, taken,
                 waiting, attached, slot, have, done, started, own, woken,
                 checking, closedDb>>
  \* The rest of the commit side is untouched.
  /\ UNCHANGED <<roundUnit, joined, offPoll, stalled, spc>>
  \* No timed wait is touched.
  /\ UNCHANGED timerVars

-----------------------------------------------------------------------------
\* THE WHOLE SYSTEM

\* Any step of thread t (as reader, owner, or runner).
ThreadStep(t) ==
  \* A read that finds its block, or the database closed.
  \/ \E x \in Blocks : Hit(t, x)
  \* A read that misses.
  \/ \E x \in Blocks : Miss(t, x)
  \* A job regolith starts on t's call.
  \/ \E j \in Jobs : SelfStart(t, j)
  \* A commit_nowait returns.
  \/ \E r \in Rounds : CommitNowait(t, r)
  \* A commit_nowait after close is refused.
  \/ \E r \in Rounds : CommitClosed(t, r)
  \* t, not polling, runs a round nobody runs.
  \/ \E u \in Ids : LandHere(t, u)
  \* A write runs, or meets a stall.
  \/ \E s \in Stalls : WriteTry(t, s)
  \* A stopped write registers its wait.
  \/ \E s \in Stalls : StallWait(t, s)
  \* A stopped write looks at the stall again.
  \/ \E s \in Stalls : StallRecheck(t, s)
  \* A ready wait runs again.
  \/ \E x \in Things : Rerun(t, x)
  \* The poll takes the inbox.
  \/ Take(t)
  \* The poll handles a note.
  \/ Handle(t)
  \* The poll claims a unit.
  \/ \E u \in Ids : Claim(t, u)
  \* A claimed unit's read finishes.
  \/ \E u \in Ids : Finish(t, u)
  \* The owner goes idle.
  \/ Rest(t)
  \* Bug only: the second half of going idle.
  \/ MarkIdle(t)
  \* A woken owner runs.
  \/ Wake(t)
  \* A task asks for a timed wait.
  \/ \E x \in Timers : AskTimer(t, x)
  \* The poll completes a due timed wait.
  \/ \E x \in Timers : Fire(t, x)
  \* An idle owner runs again for a due timed wait.
  \/ TimerWake(t)
  \* The owner drops its queue.
  \/ DropQueue(t)

\* Every possible step.
Next ==
  \* Some thread steps.
  \/ \E t \in Threads : ThreadStep(t)
  \* The clock reaches a timed wait's due time.
  \/ \E x \in Timers : Elapse(x)
  \* An owner wakes on its own.
  \/ \E t \in Threads : SelfWake(t)
  \* A finisher tells a queue.
  \/ \E u \in Ids, q \in Threads : Tell(u, q)
  \* The database closes.
  \/ Close
  \* Close lets a unit go.
  \/ \E u \in Ids : Release(u)
  \* Background work clears a stall.
  \/ \E s \in Stalls : Clear(s)

\* The behaviours: start at Init, take Next steps, and never stop a step
\* that stays possible (weak fairness). An owner waking on its own, close
\* and the drop of a queue are not forced: liveness must not lean on them.
Spec ==
  \* Start.
  /\ Init
  \* Step.
  /\ [][Next]_vars
  \* Each thread keeps going while it can.
  /\ \A t \in Threads :
       \* Its reads that hit.
       /\ WF_vars(\E x \in Blocks : Hit(t, x))
       \* Its reads that miss.
       /\ WF_vars(\E x \in Blocks : Miss(t, x))
       \* Its jobs.
       /\ WF_vars(\E j \in Jobs : SelfStart(t, j))
       \* Its commits.
       /\ WF_vars(\E r \in Rounds : CommitNowait(t, r))
       \* Its commits refused after close.
       /\ WF_vars(\E r \in Rounds : CommitClosed(t, r))
       \* Its writes.
       /\ WF_vars(\E s \in Stalls : WriteTry(t, s))
       \* Its stopped writes' registrations.
       /\ WF_vars(\E s \in Stalls : StallWait(t, s))
       \* Its stopped writes' second looks.
       /\ WF_vars(\E s \in Stalls : StallRecheck(t, s))
       \* Its ready waits.
       /\ WF_vars(\E x \in Things : Rerun(t, x))
       \* Taking its inbox.
       /\ WF_vars(Take(t))
       \* Handling its notes.
       /\ WF_vars(Handle(t))
       \* Claiming.
       /\ WF_vars(\E u \in Ids : Claim(t, u))
       \* Finishing what it claimed.
       /\ WF_vars(\E u \in Ids : Finish(t, u))
       \* Waking when woken.
       /\ WF_vars(Wake(t))
       \* The bug's second half (the real code has none).
       /\ WF_vars(MarkIdle(t))
       \* Asking for its timed waits.
       /\ WF_vars(\E x \in Timers : AskTimer(t, x))
       \* Completing the due timed waits it holds.
       /\ WF_vars(\E x \in Timers : Fire(t, x))
       \* Running again when a timed wait it holds is due: the owner polls
       \* again within what its last poll reported.
       /\ WF_vars(TimerWake(t))
  \* Time passes: every timed wait asked for comes due in the end.
  /\ \A x \in Timers : WF_vars(Elapse(x))
  \* Every finisher tells every queue it owes.
  /\ \A u \in Ids : WF_vars(\E q \in Threads : Tell(u, q))
  \* Close, once it began, lets every free unit go.
  /\ \A u \in Ids : WF_vars(Release(u))
  \* Background work catches up: every stall clears in the end.
  /\ \A s \in Stalls : WF_vars(Clear(s))

-----------------------------------------------------------------------------
\* WHAT MUST ALWAYS HOLD

\* Every variable holds the kind of value it should.
TypeOK ==
  \* Table entries are unit numbers or none.
  /\ table \in [Things -> Ids \cup {NoUnit}]
  \* Each unit reads a thing, or nothing yet.
  /\ what \in [Ids -> Things \cup {NoUnit}]
  \* Each unit is in one of four states.
  /\ state \in [Ids -> {"none", "free", "claimed", "done"}]
  \* Each unit has one of three outcomes.
  /\ outcome \in [Ids -> {"none", "landed", "released"}]
  \* Waiter lists are sets of queues.
  /\ waiters \in [Ids -> SUBSET Threads]
  \* SHUT flags are booleans.
  /\ shut \in [Ids -> BOOLEAN]
  \* Inboxes are sequences of notes.
  /\ \A q \in Threads : \A i \in 1..Len(inbox[q]) : inbox[q][i] \in Msg
  \* Taken notes are notes too.
  /\ \A q \in Threads : \A i \in 1..Len(taken[q]) : taken[q][i] \in Msg
  \* Waits have one of three values.
  /\ slot \in [Threads -> [Things -> {"none", "pending", "ready"}]]
  \* Owners are busy or idle.
  /\ own \in [Threads -> {"busy", "idle"}]
  \* Each round has a unit, or none yet.
  /\ roundUnit \in [Rounds -> Ids \cup {NoUnit}]
  \* Synced and visible rounds are rounds.
  /\ synced \subseteq Rounds /\ visible \subseteq Rounds
  \* Stalls in force are stalls.
  /\ stalled \subseteq Stalls
  \* Stopped writes are in one of three places.
  /\ spc \in [Threads -> [Stalls -> {"none", "seen", "recheck"}]]
  \* The waits an owner holds are timed waits.
  /\ timed \in [Threads -> SUBSET Timers]
  \* The waits that are due are timed waits.
  /\ elapsed \subseteq Timers
  \* The dropped queues are threads' queues.
  /\ dropped \subseteq Threads

\* A unit's disk read runs at most once: one claim, one run.
\* Rules out: A and B both claiming block 7's unit and reading it twice.
SingleRun == \A u \in Ids : runs[u] <= 1

\* A queue is given at most one "done" note per unit, and only if it
\* registered on that unit.
\* Rules out: B being told twice, or C being told about a read it never
\* asked for.
ExactlyOnce ==
  \* For every queue and unit:
  \A q \in Threads, u \in Ids :
    \* at most one note...
    /\ told[q][u] <= 1
    \* ...and only to a queue that registered.
    /\ told[q][u] = 1 => q \in regd[u]

\* Once a finished unit has told everyone it owed, every queue that
\* registered got its note.
\* Rules out: B registering on block 7's unit and never hearing it finished.
EveryWaiterTold ==
  \* For every unit that ended and has nobody left to tell:
  \A u \in Ids : (state[u] = "done" /\ toTell[u] = {}) =>
    \* each queue that registered got exactly one note.
    \A q \in regd[u] : told[q][u] = 1

\* A read's request sits only on its own thread's queue, so only that
\* queue's poll can ever finish it.
\* Rules out: A's read recorded on B's queue, finished by B's poll on B's
\* thread, while A never hears.
OwnQueue ==
  \* In every queue's inbox and taken notes...
  \A q \in Threads :
    \* every "read" note in the inbox is from q's own reader...
    /\ \A i \in 1..Len(inbox[q]) : inbox[q][i].kind = "read" => inbox[q][i].reader = q
    \* ...and so is every one being handled.
    /\ \A i \in 1..Len(taken[q]) : taken[q][i].kind = "read" => taken[q][i].reader = q

\* An idle owner that nobody has woken has an empty inbox.
\* Rules out: B going to sleep right as A's "done" note lands, sleeping
\* forever with the note unread.
NoLostIdleWakeup ==
  \* For every thread: asleep and not woken means nothing in the inbox.
  \A t \in Threads : (own[t] = "idle" /\ ~woken[t]) => inbox[t] = <<>>

\* Only an idle owner is ever woken.
\* Rules out: a note waking A while A is busy running its own reads.
BusyNeverWoken ==
  \* For every thread: a fired waker means the thread was asleep.
  \A t \in Threads : woken[t] => own[t] = "idle"

\* Every free unit in the table is held by some queue (registered there, or
\* on its way in a "read" note), so some poll will run it.
\* Rules out: a job started with no queue, which no poll ever runs.
NoOrphanUnit ==
  \* For every block or job:
  \A x \in Things :
    \* If the table holds a free unit for x...
    (table[x] # NoUnit /\ state[table[x]] = "free") =>
      \* ...some queue holds it.
      Held(table[x])

\* A round's commits are visible only once its fsync ran: an Immediate
\* commit a reader saw survives a power cut.
\* Rules out: the commits of round 4 visible as soon as they are written,
\* before the fsync a queue's poll will run.
VisibleAfterSync == visible \subseteq synced

\* A ticket (any wait) becomes ready only at its own thread's poll: no
\* other thread's step makes it ready.
\* Rules out: B running round 4 and making A's ticket ready on B's thread,
\* running A's callbacks there, while A is busy.
DeliveredOnOwnPoll == offPoll = {}

\* A cleared stall leaves no unlanded unit behind, except one a writer is
\* about to land on its second look.
\* Rules out: the clearer looking before the writer installed its unit, and
\* the writer never looking again, so its write waits forever.
StallLandsWhenClear ==
  \* For every stall:
  \A s \in Stalls :
    \* If it cleared and the table still holds an unlanded unit for it...
    (s \notin stalled /\ table[s] # NoUnit /\ state[table[s]] = "free") =>
      \* ...some writer has registered and is about to look again.
      \E t \in Threads : spc[t][s] = "recheck"

\* A timed wait's note and its record sit only on the queue of the thread
\* that asked for it, so only that thread's poll completes it.
\* Rules out: A's backoff recorded on B's queue and ended by B's poll, on
\* B's thread.
TimerOnOwnQueue ==
  \* On every queue:
  \A q \in Threads :
    \* every "timer" note in the inbox is for a wait of q's own thread...
    /\ \A i \in 1..Len(inbox[q]) : inbox[q][i].kind = "timer" => TimerOf[inbox[q][i].u] = q
    \* ...so is every one being handled...
    /\ \A i \in 1..Len(taken[q]) : taken[q][i].kind = "timer" => TimerOf[taken[q][i].u] = q
    \* ...and so is every wait the owner holds.
    /\ \A x \in timed[q] : TimerOf[x] = q

\* A timed wait is ready only once the clock reached its due time, or its
\* queue is gone.
\* Rules out: a retry that asked to wait until its due time running again
\* at the very next poll.
TimerNotEarly ==
  \* For every timed wait:
  \A x \in Timers :
    \* ready means due, or dropped with its queue.
    slot[TimerOf[x]][x] = "ready" => (x \in elapsed \/ TimerOf[x] \in dropped)

\* A dropped queue holds no timed wait and leaves none pending.
\* Rules out: a task waiting for ever on a wait whose queue is gone, which
\* no poll will ever look at.
DroppedHoldsNothing ==
  \* For every dropped queue:
  \A t \in dropped :
    \* the owner holds no wait...
    /\ timed[t] = {}
    \* ...and no timed wait of its thread is still pending.
    /\ \A x \in Timers : slot[t][x] # "pending"

\* Over a whole run: every thread's reads, jobs, commits, writes and timed
\* waits all finish. With one thread this is the wasm case: it finishes
\* everything by itself.
AllDone == <>(\A t \in Threads : done[t] = Goal(t))

-----------------------------------------------------------------------------
\* THE SETUPS THE CONFIGURATIONS NAME

\* Two threads: thread 1 reads blocks 1 and 2, thread 2 reads block 1 (the
\* block both miss; block 2 is thread 1's alone).
WantPool == [t \in {1, 2} |-> IF t = 1 THEN {1, 2} ELSE {1}]
\* No jobs in that setup.
JobOfNone == [j \in {} |-> 1]
\* Two threads that both read block 1, while thread 2's call starts job 3.
WantJobs == [t \in {1, 2} |-> {1}]
\* Job 3 belongs to thread 2.
JobOfJobs == [j \in {3} |-> 2]
\* Two threads: thread 1 reads block 1, thread 2 reads nothing (it starts a
\* job in the run that uses this).
WantOne == [t \in {1, 2} |-> IF t = 1 THEN {1} ELSE {}]
\* One thread reads block 1 alone.
WantSingleOne == [t \in {1} |-> {1}]
\* One thread reads both blocks and starts the job itself.
WantSingle == [t \in {1} |-> {1, 2}]
\* Job 3 belongs to the one thread.
JobOfSingle == [j \in {3} |-> 1]
\* No rounds in a setup.
CommittersNone == [r \in {} |-> {}]
\* No stalls in a setup.
StoppedNone == [s \in {} |-> {}]
\* No reads in a setup.
WantNone == [t \in {1, 2} |-> {}]
\* Round 4 covers a commit of thread 1 and one of thread 2.
CommittersBoth == [r \in {4} |-> {1, 2}]
\* Stall 5 stops writes on threads 1 and 2.
StoppedBoth == [s \in {5} |-> {1, 2}]
\* No timed waits in a setup.
TimerOfNone == [x \in {} |-> 1]
\* Timed wait 6 is thread 1's.
TimerOfOne == [x \in {6} |-> 1]
\* Timed wait 6 is thread 1's and timed wait 7 is thread 2's.
TimerOfEach == [x \in {6, 7} |-> IF x = 6 THEN 1 ELSE 2]
\* One thread that reads nothing.
WantSingleNone == [t \in {1} |-> {}]

====
