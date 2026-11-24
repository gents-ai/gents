---- MODULE SyncRwLock ----
\* ===========================================================================
\* THE STORY
\* ===========================================================================
\* A reader-writer lock lets many tasks read at once, or one task write, but
\* never both. regolith's (src/sync/rwlock.rs, src/sync/raw_rwlock.rs and
\* src/sync/reentrant_rwlock.rs) is for async code: a task that cannot enter
\* joins a queue and is woken later instead of blocking its thread.
\*
\* Like the Mutex of Sync.tla it allows "barging": a request that fits the
\* lock right now enters, even past the queue. Tiny example of why that
\* needs a limit: a writer waits; readers keep arriving so that at every
\* moment some reader is inside. Each time a reader leaves, the writer is
\* nudged, finds another reader still inside, and loses. Without a limit it
\* waits for ever. So the waiting writer counts the races it loses, and the
\* retry that would be its Bound-th loss raises a flag on the lock, in the
\* same indivisible step, that says "a handoff is owed". While the flag is
\* up no newcomer gets in: the readers inside leave, and the pass that
\* serves the queue takes the lock for the writer and grants it.
\*
\* The lock also has an "upgradable read": a read guard that can later
\* become the write guard without letting go in between. Tiny example of
\* what can go wrong there: if ANY reader could upgrade, two readers could
\* both ask; each waits for the other to leave, and neither does. So only
\* one upgradable guard exists at a time, and while it waits to upgrade no
\* new plain reader is let in.
\*
\* A third example, which this model found in the code (defect E33). Task
\* A holds the upgradable read and waits to upgrade. A gives up (its future
\* is dropped). Task B now takes the upgradable read. If the pass that
\* serves the queue still believed A's request was alive, and took the
\* write "for A" the moment the last reader left, it would spend the
\* upgradable read that is now B's: the lock would look free while B holds
\* it, and a writer could enter beside B. So a pass never changes the
\* upgradable read for anybody. It only tells a waiting upgrade "the
\* readers have left", and the upgrader takes the write itself.
\*
\* A fourth piece (E48): a reader or writer that finds the lock taken spins a
\* little before it queues, and a queued waiter that loses a retry spins on
\* its own waiter node before it sleeps, exactly as the Mutex does in Sync.tla
\* (steps PreSpin, PreSpinOut, SpinStart, SpinEnd; the table of where each
\* lives in the code is in Sync.tla). Tiny example of what must not go wrong:
\* a writer spins on its node; a reader leaves and nudges it, waking nobody,
\* because the writer is awake; the writer's spin then runs out. It must read
\* its node before it sleeps, or it sleeps beside the nudge for ever
\* (MC_SyncRwLock_Red_SpinNoRecheck). The upgrade's own wait never spins, so
\* D50's upgradable read is untouched.
\*
\* What this model shows, over every interleaving of two or three tasks, one
\* step of the lock at a time: a writer is alone, every guard a task holds
\* is counted in the lock, nobody loses more than Bound races, a handoff
\* cannot be barged, an upgrade never deadlocks and never waits behind new
\* readers, an owner never queues behind itself, no waiter sleeps beside a
\* lock it could enter, and a thread serving the queue runs one pass plus
\* at most one for each thing another thread does to the queue meanwhile.
\* Each planted bug breaks one of these.
\*
\* The locks covered: RwLock and ReentrantRwLock after D49 (barging with
\* bounded bypass between readers and writers), D50 (the upgradable read)
\* and E33 (who takes the write for an upgrade). The code this matches is
\* src/sync/raw_rwlock.rs as fixed for E33 (commit 7bec2a7, which this tree
\* has by merge).
\*
\* PROVED FOR EVERY SIZE in Lean, proofs/lean/Regolith/Sync.lean: the
\* barging lock's laws (mutual_exclusion, bounded_bypass, owed_at_bound,
\* owed_exclusive) for an exclusive lock, which this lock's write side
\* follows. The reader-writer sharing, the upgradable read and the reentrant
\* owner rules are checked here only, for two or three tasks; no Lean
\* theorem covers them.
\*
\* CHECKED ON THE REAL CODE by loom (tests/loom_sync.rs) and by unit tests:
\*   a_rwlock_writer_and_readers_exclude_each_other   MutualExclusion
\*   an_upgradable_read_upgrades_while_readers_and_a_writer_contend
\*                                                    MutualExclusion,
\*                                                    NoLostWakeup for an
\*                                                    upgrade
\*   a_dropped_upgrade_racing_the_last_reader_leaves_nothing_held
\*                                                    CancelPassesOn for an
\*                                                    upgrade
\*   a_dropped_upgrade_never_lets_a_writer_in_beside_the_next_upgradable_read
\*                                                    GuardsCounted (E33),
\*                                                    with a calibration
\*                                                    that rebuilds the old
\*                                                    step and must fail
\*   a_reentrant_reader_never_waits_behind_a_writer_waiting_for_it
\*                                                    NoSelfDeadlock
\*   a_reentrant_write_becomes_its_owners_read_while_another_owner_waits
\*                                                    the downgrade
\*   src/sync/raw_rwlock.rs, tests
\*     an_upgrade_dropped_while_another_thread_drains_does_not_cost_the_next_upgrade
\*     a_pass_between_a_dropped_upgrades_withdraw_and_its_release_frees_nothing_twice
\*                                                    GuardsCounted (E33)
\* The E33 loom model has a calibration,
\* calibration_a_pass_that_takes_the_write_for_a_dropped_upgrade_lets_a_writer_in,
\* that rebuilds the old step out of plain atomics and must fail.
\*
\* THE CODE MODELLED.
\*   - The state word (WaitQueue::state) holds, above the queue's six flag
\*     bits, the count of readers and three bits: WRITER, UPGRADABLE and
\*     UPGRADING. Each is a variable below.
\*   - Acquire (Contend::poll over RawRwLock, RwWait). One compare-and-swap
\*     enters when the request fits the word and HANDOFF is down, whoever
\*     waits: barging. A read fits while no writer is in and no upgrade is
\*     under way; a write while nobody is in; an upgradable read while no
\*     writer and no other upgradable read is in (raw_rwlock::admits). When
\*     it fails the caller pushes a node, raises QUEUED and looks at its
\*     node once more before sleeping. RwLock::write_owned is the same wait
\*     as write(), in a future and a guard that keep the lock alive
\*     themselves: a task whose role is "write" stands for both.
\*   - A nudged waiter takes back the NUDGE_OUT marker and tries again. A
\*     failed try counts one bypass in the waiting future (Contend::bypass).
\*     The try that would be its Bound-th counted loss is
\*     Lock::take_or_owe: it either enters or raises HANDOFF.
\*   - Release (RawRwLock::read_unlock, write_unlock, upgradable_unlock):
\*     one read-modify-write on the word. Only if waiters need telling does
\*     it go on to ask for a drain pass (Policy::released, a Step::Wake):
\*     waiters queued with no nudge on its way, or a handoff owed, or the
\*     last reader leaving while an upgrade waits.
\*   - The drain role, DIRTY and WAKE_FRONT are the queue's, as in Sync.tla.
\*   - One pass (RawRwLock::pass). It lets go of a withdrawn upgrade's node,
\*     sorts the arrivals (an upgrade's node goes to a slot of its own, the
\*     others to the list), and, if an upgrade waits and no reader is in,
\*     grants its node. While HANDOFF is up it enters the lock for the front
\*     waiters and grants them, oldest first, stopping after a writer. While
\*     no waiter is owed it nudges the front waiter if it fits (or after any
\*     release, even if it does not fit yet), and with a sharing front the
\*     readers behind it for as long as they could share: more than the
\*     front.
\*   - The upgrade (UpgradeWait). The holder of the upgradable read raises
\*     UPGRADING, which holds new readers back. If no reader is in, one
\*     compare-and-swap makes it the writer (RawRwLock::try_upgrade).
\*     Otherwise it queues a node and sleeps. When a pass has granted the
\*     node, its poll makes it the writer with one flip of the three bits
\*     (RawRwLock::finish_upgrade). A dropped upgrade withdraws its node and
\*     releases the upgradable read with upgradable_unlock, whatever the
\*     node's state was.
\*   - ReentrantRwLock keeps, beside the raw lock, each owner's count of
\*     reads and the writing owner with its count of writes. An owner that
\*     reads or writes reads again at once; an owner that writes writes
\*     again at once; an owner that only reads is refused the write
\*     (ReadHeldByOwner). An owner that writes and reads, and drops its last
\*     write, keeps reading: the write becomes one read (downgrade).
\*
\* WHAT IS TAKEN WHOLE HERE, and where it is checked step by step. The
\* queue's own instructions are Sync.tla's, for the same WaitQueue and the
\* same Contend::poll: the seven instructions of take_or_owe (one step
\* here), the waker slots of a node, and the raising of NUDGE_OUT before a
\* pass's first nudge (kept here). A compare-and-swap loop on the state word
\* is one step where its outcome depends on one read of the word.
\*
\* WHAT THE MODEL LEAVES OUT, and why that loses nothing.
\*   - A task has one role (it reads, writes, or takes upgradable reads) and
\*     one waiter node at a time, and does not start an upgrade while a node
\*     it withdrew is still linked; two nodes of one task act as two tasks.
\*   - The tasks of one owner of a ReentrantRwLock run on one thread (Owner,
\*     src/sync/owner.rs), so one of them is inside an operation or waiting
\*     at a time. Two pending requests of one owner are not modelled.
\*   - Polls with no wake, and memory ordering (loom's, plan 7.1).
\*
\* SHORT SCENARIOS. As in Sync.tla, three tasks that each ask as often as
\* they like are far more states than a row may take. Turns says how often
\* each task asks, Start who holds a guard at the beginning, Follows fixes
\* the order two waiters arrive in. The two-task configurations run free.
\*
\* CONFIGURATIONS. See each MC_SyncRwLock_*.cfg for its story.
\*   MC_SyncRwLock_Green_Plain          a writer waits behind two readers.
\*   MC_SyncRwLock_Green_WriterLive     a writer against a reader that
\*                                      keeps coming back: it is served.
\*   MC_SyncRwLock_Green_ReaderLive     a reader against a writer that
\*                                      keeps coming back: it is served.
\*   MC_SyncRwLock_Green_Upgrade        an upgrade behind a reader, with a
\*                                      writer queued.
\*   MC_SyncRwLock_Green_UpgradeLive    an upgrade against a reader that
\*                                      keeps coming back: it completes.
\*   MC_SyncRwLock_Green_UpgradeDropped E33: an upgrade is dropped while the
\*                                      reader leaves and another task takes
\*                                      the upgradable read.
\*   MC_SyncRwLock_Green_Reentrant      two tasks of one owner nest a write
\*                                      and a read; another owner waits.
\*   MC_SyncRwLock_Green_ReentrantLive  one task of each owner: if no owner
\*                                      holds forever, the waiter is served.
\*   MC_SyncRwLock_Red_UnboundedBarging the waiter never raises HANDOFF:
\*                                      BoundedBypass.
\*   MC_SyncRwLock_Red_AnyReaderUpgrade plain readers may upgrade: two wait
\*                                      for each other, NoUpgradeDeadlock.
\*   MC_SyncRwLock_Red_UpgradeNoHoldBack  new readers enter while an upgrade
\*                                      waits: UpgradeHoldsBack.
\*   MC_SyncRwLock_Red_OwnerUnaware     an owner's second read queues while
\*                                      it holds a read: NoSelfDeadlock.
\*   MC_SyncRwLock_Red_PassTakesWrite   E33 as the code was: the pass reads
\*                                      UPGRADING and takes the write for
\*                                      the node in its slot: GuardsCounted.
\*   MC_SyncRwLock_Red_SpinNoRecheck   a waiter whose spin on its node runs
\*                                      out sleeps without reading the marks;
\*                                      the release that nudged it while it
\*                                      spun woke nobody: NoLostWakeup.
\* Every row above runs the spins.

\* Borrow the standard TLA+ books: counting numbers, lists, and sizes of finite sets.
EXTENDS Naturals, Sequences, FiniteSets

\* The fixed inputs a configuration chooses.
CONSTANTS
  \* The tasks that use the lock, named by positive numbers.
  Tasks,
  \* Which lock is modelled: "Plain" (RwLock, src/sync/rwlock.rs) or "Reentrant" (ReentrantRwLock,
  \* src/sync/reentrant_rwlock.rs).
  Kind,
  \* Each task's owner token, as a map from task to a positive number (Owner, src/sync/owner.rs).
  \* Tasks with the same token count as one owner.
  Owner,
  \* The largest number of guards one task holds at once (more than 1 only on the reentrant lock).
  MaxDepth,
  \* After how many lost races a waiter is owed a handoff: MAX_BYPASS in src/sync/contend.rs (16 in
  \* the engine, 2 under loom).
  Bound,
  \* What each task asks for, as a map from task to "read", "write" or "upread" (the upgradable
  \* read): the payload READ, WRITE or UPGRADABLE_READ of its waiter node.
  Role,
  \* The tasks that, once they hold the upgradable read, may ask to upgrade it to the write.
  Upgrades,
  \* The tasks that wait when their request does not fit (read, write, upgradable_read). The others
  \* only try (try_read, try_write, try_upgradable_read): when their one attempt fails they go away.
  Waits,
  \* The tasks whose waiting future (an acquire, or an upgrade) may be dropped before it is served.
  Drops,
  \* How many times each task may ask for the lock, as a map from task to number; 0 means as often
  \* as it likes. A small number makes a run a short scenario, the way a test has a few calls.
  Turns,
  \* The tasks that hold a guard of their role when the run starts, the way a test takes the lock
  \* before it spawns its threads. Their first hold counts as one of their turns.
  Start,
  \* For each task, a task that must have queued (or entered) before this one may ask, or 0 for
  \* none: the way a test waits for a "queued" flag before it starts the next thread.
  Follows,
  \* The tasks whose "is eventually served" promises are checked.
  Watched,
  \* "none" for the code as built, or the name of one planted bug.
  Mutant

\* The value "no owner holds the write", and "no task".
NoOwner == 0
\* Every Owner token some task uses.
Owners == {Owner[t] : t \in Tasks}

\* The kind is one of the two.
ASSUME Kind \in {"Plain", "Reentrant"}
\* Tasks and owner tokens are positive numbers (0 means "none").
ASSUME Tasks \subseteq Nat \ {0} /\ Owners \subseteq Nat \ {0}
\* At least one guard may be held, and the bound is at least 1.
ASSUME MaxDepth \in Nat \ {0} /\ Bound \in Nat \ {0}
\* The five sets of tasks are sets of tasks.
ASSUME Upgrades \subseteq Tasks /\ Waits \subseteq Tasks /\ Drops \subseteq Tasks
\* (the other two)
ASSUME Start \subseteq Tasks /\ Watched \subseteq Tasks
\* Every task has one of the three roles, a number of turns, and comes after a task or nobody.
ASSUME \A t \in Tasks : /\ Role[t] \in {"read", "write", "upread"}
                        \* (its turns, and the task it follows)
                        /\ Turns[t] \in Nat /\ Follows[t] \in Tasks \cup {0}
\* Only a task that takes upgradable reads upgrades (the planted bug AnyReaderUpgrade aside).
ASSUME Mutant # "AnyReaderUpgrade" => \A t \in Upgrades : Role[t] = "upread"
\* On a plain RwLock a task holds one guard at a time and every task is its own owner.
ASSUME Kind = "Plain" => MaxDepth = 1 /\ \A t, u \in Tasks : t # u => Owner[t] # Owner[u]
\* The reentrant lock has no upgradable read.
ASSUME Kind = "Reentrant" => Upgrades = {} /\ \A t \in Tasks : Role[t] # "upread"
\* The mutant is "none" or one of the six planted bugs.
ASSUME Mutant \in {"none", "UnboundedBarging", "AnyReaderUpgrade", "UpgradeNoHoldBack",
                   "OwnerUnaware", "PassTakesWrite", "SpinNoRecheck"}

\* Configuration helpers, named in the MC_SyncRwLock_*.cfg files.
\* Every task is its own Owner (a plain RwLock).
OwnerEach == [t \in Tasks |-> t]
\* Tasks 1 and 2 share Owner 1; every other task is Owner 2.
OwnerPair == [t \in Tasks |-> IF t \in {1, 2} THEN 1 ELSE 2]
\* Task 1 writes; the others read.
RoleWriterReaders == [t \in Tasks |-> IF t = 1 THEN "write" ELSE "read"]
\* Task 1 reads; the others write.
RoleReaderWriters == [t \in Tasks |-> IF t = 1 THEN "read" ELSE "write"]
\* Task 1 takes the upgradable read; the others read.
RoleUpgraderReaders == [t \in Tasks |-> IF t = 1 THEN "upread" ELSE "read"]
\* Task 1 takes the upgradable read, task 2 reads, task 3 writes.
RoleUpgraderReaderWriter == [t \in Tasks |-> IF t = 1 THEN "upread" ELSE IF t = 2 THEN "read"
                                                                    \* (task 3)
                                                                    ELSE "write"]
\* Tasks 1 and 3 take the upgradable read; task 2 reads.
RoleTwoUpgraders == [t \in Tasks |-> IF t = 2 THEN "read" ELSE "upread"]
\* Every task reads.
RoleReaders == [t \in Tasks |-> "read"]
\* Tasks 1 and 2 read; task 3 writes.
RoleReadersWriter == [t \in Tasks |-> IF t = 3 THEN "write" ELSE "read"]
\* Task 1 writes, task 2 reads, task 3 writes.
RoleWriteReadWrite == [t \in Tasks |-> IF t = 2 THEN "read" ELSE "write"]
\* No task has to come after another.
FollowsNobody == [t \in Tasks |-> 0]
\* Task 2 asks only after task 1 has queued or entered.
SecondFollowsFirst == [t \in Tasks |-> IF t = 2 THEN 1 ELSE 0]
\* Task 3 asks only after task 2 has queued or entered.
ThirdFollowsSecond == [t \in Tasks |-> IF t = 3 THEN 2 ELSE 0]
\* Task 3 asks only after task 1 has queued or entered.
ThirdFollowsFirst == [t \in Tasks |-> IF t = 3 THEN 1 ELSE 0]
\* Every task asks as often as it likes.
TurnsAny == [t \in Tasks |-> 0]
\* Every task asks once.
TurnsOnce == [t \in Tasks |-> 1]
\* Task 2 asks twice, the others once.
TurnsSecondTwice == [t \in Tasks |-> IF t = 2 THEN 2 ELSE 1]

\* The state: everything that changes from one step to the next. Each line says what the variable
\* stands for in the real program.
VARIABLES
  \* The count of readers in the state word (the bits above READER_SHIFT in src/sync/raw_rwlock.rs).
  \* The reentrant lock counts reading owners here, not read guards.
  readers,
  \* The WRITER bit: a writer is in.
  writer,
  \* The UPGRADABLE bit: the upgradable read is out.
  upgradable,
  \* The UPGRADING bit: the holder of the upgradable read has asked to upgrade, so new readers are
  \* held back.
  upgrading,
  \* The DRAINING bit of the state word (src/sync/queue.rs): some thread holds the drain role, the
  \* only one allowed to sort arrivals, grant and nudge.
  drainOn,
  \* The DIRTY bit: something changed while the drain role was held, so its holder must pass again
  \* before it lets go.
  dirty,
  \* The QUEUED bit: waiters may be queued, so a release has somebody to tell.
  queued,
  \* The NUDGE_OUT bit: a nudge is on its way to a waiter that has not polled yet, so a release need
  \* not run another pass.
  nudgeOut,
  \* The HANDOFF bit: a waiter is owed a handoff. While it is up nobody may barge.
  handoff,
  \* The WAKE_FRONT bit: a release (or a nudged waiter that withdrew) asks the next pass to nudge
  \* the front waiter whatever it asks for.
  wakeFront,
  \* How many queued waiters count themselves owed a handoff (WaitQueue::owed).
  owedCount,
  \* The arrival stack: nodes pushed by waiters and not yet taken by a pass, oldest first.
  arrivals,
  \* The waiting list a pass keeps between passes, oldest first (Waiting::list in
  \* src/sync/raw_rwlock.rs). Only the holder of the drain role touches it. A withdrawn node stays
  \* linked until a pass lets go of it.
  waiting,
  \* The slot a pass keeps the waiting upgrade's node in (Waiting::upgrade): a task, or 0 for none.
  upSlot,
  \* The phase of each task's waiter node: "none" (no node), "waiting", "granted" (a pass granted
  \* it) or "cancelled" (its future withdrew).
  phase,
  \* Each node's NUDGED bit: a pass asked the waiter to try again and it has not looked yet.
  nudged,
  \* Each node's OWED bit: the waiter reached the bypass bound and is counted in owedCount.
  owedMark,
  \* What each node asks for (its payload): "read", "write", "upread", or "upgrade" (the upgradable
  \* read's wait to become the write).
  nkind,
  \* Whether each task's waker was called since the task last polled: the executor will poll it.
  wake,
  \* The wake list of the pass under way: the sleeping tasks it granted or nudged, whose wakers it
  \* calls when it gives the drain role up (WakeList, src/sync/waiter.rs).
  wl,
  \* How many times each task's pending acquire has entered the lock word (by its own
  \* compare-and-swap, or by a pass on its behalf) without being a guard yet.
  got,
  \* How many guards each task holds.
  held,
  \* The kind of guard each task holds: "none", "read", "write" or "upread".
  mode,
  \* ReentrantRwLock only: how many reads each owner holds (the `reads` of its Record).
  rdepth,
  \* ReentrantRwLock only: the owner that writes, or NoOwner (ReentrantRwLock::writer).
  wowner,
  \* ReentrantRwLock only: how many write guards the writing owner holds (write_depth).
  wdepth,
  \* Each waiting future's count of lost races (Contend::bypass, src/sync/contend.rs).
  bypass,
  \* A local of Contend::cancel: whether the withdrawn node carried a nudge nobody used.
  passOn,
  \* A local of UpgradeWait::cancel: whether the dropped upgrade's node withdrew (it had not been
  \* granted), so the queue must be told.
  upW,
  \* Whether the future spins on its own node after a lost try (Wait::spin_park), as in Sync.tla:
  \* while it is TRUE the node has no waker a release could claim, so a release that nudges or grants
  \* it only sets the mark, and the spin itself sees the mark. The upgrade's own wait never spins.
  spinning,
  \* A local of the pass under way: whether WAKE_FRONT was up when the pass began (`wake_front`).
  dWF,
  \* A local of the pass under way: the reader count of the lock as its nudging pictures it
  \* (`sharing`: the word it read, with the waiters it has nudged so far entered).
  dSR,
  \* A local of the pass under way: the WRITER bit of that picture.
  dSW,
  \* A local of the pass under way: the UPGRADABLE bit of that picture.
  dSU,
  \* A local of the pass under way: the UPGRADING bit of that picture.
  dSG,
  \* A local of the pass under way: the place in the waiting list its nudging has reached.
  dPos,
  \* A local of the pass under way: TRUE until its nudging has passed the first live waiter.
  dFirst,
  \* A local of the pass under way: whether it raised NUDGE_OUT (Nudges::raised).
  dRaised,
  \* A local of the pass under way: whether one of its nudges reached a waiter (Nudges::sent).
  dSent,
  \* A local of the pass under way: the kind it entered the lock for and has not granted yet, or
  \* "none".
  dHand,
  \* A local of the pass under way: whether the front waiter, when its nudging began, asked to
  \* write.
  dFrontW,
  \* A local of the drain: whether the pass found no waiter left, so QUEUED may come down.
  dClearQ,
  \* A local of the drain: whether no waiter was counted owed, so HANDOFF may come down.
  dClearH,
  \* Where each task is in its operation: its "program counter" (see PCs below).
  pc,
  \* Where a task goes on once the drain it runs is over, per task.
  ret,
  \* Model bookkeeping (a "ghost"): whether the drain under way (or the next one) has a pass to
  \* run that something earned, as in Sync.tla.
  dEarned,
  \* Model bookkeeping (a "ghost"): how many counted tries of each task's current acquire failed
  \* while its node still waited. The promise BoundedBypass reads it.
  lost,
  \* Model bookkeeping: TRUE once somebody barged while a handoff was owed.
  bargedOwed,
  \* Model bookkeeping: TRUE once a plain reader entered while an upgrade was waiting.
  readInUpgrade,
  \* Model bookkeeping: TRUE once some drain began a pass nothing had earned.
  overran,
  \* Model bookkeeping: how many times each task with a limited number of turns has asked so far.
  turns

\* The lock's part of the state word.
lockw == <<readers, writer, upgradable, upgrading>>
\* The queue's part of the state word.
qbits == <<drainOn, dirty, queued, nudgeOut, handoff, wakeFront>>
\* The fields of every waiter node.
nodes == <<phase, nudged, owedMark, nkind>>
\* What the tasks hold, and the reentrant lock's own counts.
guards == <<got, held, mode, rdepth, wowner, wdepth>>
\* The locals of a waiting future.
futs == <<bypass, passOn, upW, spinning>>
\* The locals of the pass under way.
dloc == <<dWF, dSR, dSW, dSU, dSG, dPos, dFirst, dRaised, dSent, dHand, dFrontW, dClearQ, dClearH>>
\* The ghosts no step of the queue touches.
ghosts == <<lost, bargedOwed, readInUpgrade, overran, turns>>
\* Every variable, so a step that changes none of them is a stutter.
vars == <<lockw, qbits, owedCount, arrivals, waiting, upSlot, nodes, wake, wl, guards, futs, dloc,
          pc, ret, dEarned, ghosts>>

\* The steps of the drain, where a task holds the drain role.
DrainPCs == {"d0", "du", "dug", "duu", "dhr", "dh", "dg", "dgb", "dho", "dnb", "dni", "dnt", "dnf",
             "dend", "dex"}

\* The places where a task is between two polls or two calls: nothing of its own is under way.
Rest == {"idle", "holding", "parked", "upparked", "rupwait"}

\* The places a task can be. "idle": between operations. "holding": it has a guard. "parked": its
\* acquire returned Pending and sleeps. "upparked": its upgrade returned Pending and sleeps.
\* "rupwait": only under the planted bug AnyReaderUpgrade, a plain reader waiting to upgrade. The
\* others are instructions inside one poll, one release, one cancellation or one drain, named where
\* they are used.
PCs == Rest \cup DrainPCs \cup
       {"pspin", "push", "enqt", "park0", "repark", "nspin", "gcons", "ncons", "try", "owe", "wd1",
        "wd2", "gb1",
        "gb2", "gb3", "relt", "uptry", "uppush", "upenq", "uppark0", "upfin", "upunl", "upwul",
        "uprel", "upwd"}

----------------------------------------------------------------------------
\* Helpers.

\* The sum of f[x] over a finite set S.
RECURSIVE SumOf(_, _)
\* The sum over the empty set is 0.
SumOf(f, S) == IF S = {} THEN 0
               \* Otherwise pick any element x, and add f[x] to the sum over the rest.
               ELSE LET x == CHOOSE y \in S : TRUE IN f[x] + SumOf(f, S \ {x})

\* The elements of a sequence, as a set.
Range(s) == {s[i] : i \in 1..Len(s)}

\* Sequence s without the nodes that have withdrawn.
Alive(s) == SelectSeq(s, LAMBDA u : phase[u] # "cancelled")

\* The nodes of sequence s that have withdrawn.
Dead(s) == {u \in Range(s) : phase[u] = "cancelled"}

\* How many withdrawn nodes sequence s starts with.
RECURSIVE DeadRun(_)
\* None if s is empty or its first node has not withdrawn; else one more than for the rest.
DeadRun(s) == IF s # <<>> /\ phase[Head(s)] = "cancelled" THEN 1 + DeadRun(Tail(s)) ELSE 0

\* List s after List::front let go of every withdrawn node ahead of the first live one.
FrontOf(s) == SubSeq(s, DeadRun(s) + 1, Len(s))

\* The nodes List::front lets go of, for list s.
FrontOff(s) == {s[i] : i \in 1..DeadRun(s)}

\* The waiting list after List::front.
Pruned == FrontOf(waiting)

\* The nodes List::front lets go of, for the waiting list.
PrunedOff == FrontOff(waiting)

\* A request of kind k fits a lock whose word reads r readers, writer bit w, upgradable bit u and
\* upgrading bit g (raw_rwlock::admits; the HANDOFF bit is not looked at here). A read: no writer,
\* and no upgrade holding readers back (mutant UpgradeNoHoldBack: whatever the upgrade). A write:
\* nobody in. An upgradable read: no writer and no other upgradable read.
AdmitsIn(k, r, w, u, g) ==
  \* A read:
  CASE k = "read" -> ~w /\ (~g \/ Mutant = "UpgradeNoHoldBack")
    \* a write:
    [] k = "write" -> ~w /\ ~u /\ r = 0
    \* an upgradable read.
    [] OTHER -> ~w /\ ~u

\* A request of kind k fits the lock as it is now.
Admits(k) == AdmitsIn(k, readers, writer, upgradable, upgrading)

\* The compare-and-swap a fresh caller makes succeeds: the request fits and no handoff is owed.
\* Waiters do not matter: barging.
CanBarge(k) == Admits(k) /\ ~handoff

\* The lock word after a request of kind k entered (raw_rwlock::enter).
EnterWord(k) ==
  \* A read is one more reader;
  /\ readers' = IF k = "read" THEN readers + 1 ELSE readers
  \* a write raises WRITER;
  /\ writer' = (writer \/ k = "write")
  \* an upgradable read raises UPGRADABLE.
  /\ upgradable' = (upgradable \/ k = "upread")
  \* UPGRADING is not touched.
  /\ upgrading' = upgrading

\* The lock word after a guard of kind k is given up (read_unlock, write_unlock,
\* upgradable_unlock: one read-modify-write each).
LeaveWord(k) ==
  \* A read is one reader fewer;
  /\ readers' = IF k = "read" THEN readers - 1 ELSE readers
  \* a write lowers WRITER;
  /\ writer' = (writer /\ k # "write")
  \* an upgradable read lowers UPGRADABLE,
  /\ upgradable' = (upgradable /\ k # "upread")
  \* and UPGRADING with it.
  /\ upgrading' = (upgrading /\ k # "upread")

\* A release that saw this state must run a drain pass (wants_drain, src/sync/queue.rs): waiters
\* are queued, and no nudge is already on its way or a handoff is owed.
WantsDrain == queued /\ (~nudgeOut \/ handoff)

\* A release of a guard of kind k, seeing the state as it is now, goes on to ask for a pass: the
\* queue wants one, or this is the last reader leaving while an upgrade waits, or the upgradable
\* read is dropped with its upgrade begun.
ReleaseWants(k) ==
  \* What the queue wants,
  \/ WantsDrain
  \* the last reader leaving under an upgrade,
  \/ k = "read" /\ readers = 1 /\ upgrading
  \* or an upgrade given up.
  \/ k = "upread" /\ upgrading

\* Task t has lost Bound counted races and still waits: it is owed a handoff.
AtBound(t) == phase[t] = "waiting" /\ lost[t] >= Bound

\* Task t may ask for the lock once more: it has a turn left, and the task it comes after, if any,
\* has asked and is past pushing its node.
HasTurn(t) ==
  \* It has a turn left,
  /\ Turns[t] = 0 \/ turns[t] < Turns[t]
  \* and it comes after nobody, or that task has asked and has queued or entered.
  /\ IF Follows[t] = 0 THEN TRUE
     \* (the task it follows: one turn used, and past the two steps that queue its node)
     ELSE turns[Follows[t]] >= 1 /\ pc[Follows[t]] \notin {"push", "enqt"}

\* Task t uses one of its turns (nothing is counted for a task with no limit).
TakeTurn(t) == turns' = [turns EXCEPT ![t] = IF Turns[t] = 0 THEN 0 ELSE @ + 1]

\* No other task of t's owner is inside an operation or waiting: the tasks of one owner share a
\* thread, and the model gives an owner one pending request at a time. On a plain lock every task
\* is its own owner, so this always holds.
OwnerQuiet(t) == \A u \in Tasks \ {t} : Owner[u] = Owner[t] => pc[u] \in {"idle", "holding"}

\* Reentrant: t's owner already holds the lock in a way that covers t's request, so t enters at
\* once (reenter_read, reenter_write). A read is covered by the owner's write or by a read it
\* holds (mutant OwnerUnaware: only by its write); a write by its write.
Covered(t) ==
  \* Only on the reentrant lock.
  /\ Kind = "Reentrant"
  \* A read under the owner's write, or under a read it holds;
  /\ \/ Role[t] = "read" /\ (wowner = Owner[t] \/ (rdepth[Owner[t]] > 0 /\ Mutant # "OwnerUnaware"))
     \* a write under the owner's write.
     \/ Role[t] = "write" /\ wowner = Owner[t]

\* Reentrant: t's owner reads without writing and t asks to write. The write would wait for the
\* owner's own read, so the lock refuses it (ReadHeldByOwner).
Refused(t) ==
  \* Only on the reentrant lock, for a write,
  /\ Kind = "Reentrant" /\ Role[t] = "write"
  \* by an owner that reads and does not write.
  /\ wowner # Owner[t] /\ rdepth[Owner[t]] > 0

\* Task t gets one more guard of its role out of a request that entered the lock word. The
\* reentrant lock notes the reading owner (enter_read) or the writing owner (enter_write).
NewGuard(t) ==
  \* The task holds one more guard,
  /\ held' = [held EXCEPT ![t] = @ + 1]
  \* of its role's kind.
  /\ mode' = [mode EXCEPT ![t] = Role[t]]
  \* A reentrant read is counted for its owner.
  /\ rdepth' = IF Kind = "Reentrant" /\ Role[t] = "read"
                 \* (one more read of that owner)
                 THEN [rdepth EXCEPT ![Owner[t]] = @ + 1] ELSE rdepth
  \* A reentrant write names its owner,
  /\ wowner' = IF Kind = "Reentrant" /\ Role[t] = "write" THEN Owner[t] ELSE wowner
  \* with one write guard out.
  /\ wdepth' = IF Kind = "Reentrant" /\ Role[t] = "write" THEN 1 ELSE wdepth

\* Task t's acquire is over with n entries of the lock word in its hand: one becomes its guard,
\* none leaves its guards as they were.
SettleTo(t, n) ==
  \* With an entry in hand it becomes the guard,
  IF n > 0 THEN NewGuard(t) /\ got' = [got EXCEPT ![t] = n - 1]
  \* with none, nothing is pending and the guards are as they were.
  ELSE got' = [got EXCEPT ![t] = 0] /\ UNCHANGED <<held, mode, rdepth, wowner, wdepth>>

\* Where a task whose poll or cancellation is over goes, when it ends with `extra` more guards
\* than it holds now: it holds if it has any guard, else it is idle.
RestPc(t, extra) == IF held[t] + extra > 0 THEN "holding" ELSE "idle"

\* The future of task t is done with this acquire: its locals are forgotten.
Forget(t) ==
  \* Its count of lost races,
  /\ bypass' = [bypass EXCEPT ![t] = 0]
  \* and the ghost count.
  /\ lost' = [lost EXCEPT ![t] = 0]

\* A state change that must be seen by a drain pass (Step::Drain in Policy::transition): in the
\* same compare-and-swap the task claims the drain role, or, when another thread holds it, raises
\* DIRTY and goes on to `r`. A task that claimed the role runs the drain, then goes on to `r`.
Transit(t, r) ==
  \* Is the drain role taken?
  IF drainOn
    \* Yes: raise DIRTY, so the holder passes again,
    THEN /\ dirty' = TRUE
         \* and go on.
         /\ pc' = [pc EXCEPT ![t] = r]
         \* The role stays where it is, and the task remembers nothing.
         /\ drainOn' = drainOn /\ ret' = [ret EXCEPT ![t] = "idle"]
         \* The ghost: this waiter operation landed on the drain under way, which has earned one
         \* more pass.
         /\ dEarned' = TRUE
    \* No: take the role,
    ELSE /\ drainOn' = TRUE
         \* start a pass,
         /\ pc' = [pc EXCEPT ![t] = "d0"]
         \* and remember where to go on afterwards.
         /\ ret' = [ret EXCEPT ![t] = r]
         \* DIRTY is as it was, and the task's own drain has its first pass to run.
         /\ dirty' = dirty /\ dEarned' = dEarned

\* A release's request for a pass (Step::Wake in Policy::transition): WAKE_FRONT goes up, and in
\* the same compare-and-swap the task claims the drain role, or, when another thread holds it,
\* leaves the request with that thread and goes on to `r`, raising no DIRTY.
Asks(t, r) ==
  \* WAKE_FRONT goes up: a pass nudges the front whatever it asks for.
  /\ wakeFront' = TRUE
  \* Is the drain role taken?
  /\ IF drainOn
       \* Yes: the request stays with the holder, and the task goes on.
       THEN /\ dirty' = dirty /\ pc' = [pc EXCEPT ![t] = r]
            \* The role stays where it is, and the task remembers nothing.
            /\ drainOn' = drainOn /\ ret' = [ret EXCEPT ![t] = "idle"]
            \* The ghost: if a node is queued, this release has somebody to tell, and the drain
            \* under way has earned one more pass. Over an empty queue it has earned nothing.
            /\ dEarned' = (dEarned \/ arrivals \o waiting # <<>> \/ upSlot # 0)
       \* No: take the role, and start a pass.
       ELSE /\ drainOn' = TRUE /\ pc' = [pc EXCEPT ![t] = "d0"]
            \* Remember where to go on afterwards.
            /\ ret' = [ret EXCEPT ![t] = r]
            \* DIRTY is as it was, and the task's own drain has its first pass to run.
            /\ dirty' = dirty /\ dEarned' = dEarned

----------------------------------------------------------------------------
\* Acquiring: Contend::poll over RawRwLock (RwWait), and the reentrant lock's entry rules.

\* read(), write() or upgradable_read(), first poll, and the try_ forms. A reentrant request its
\* owner covers enters at once, with no step of the raw lock. Otherwise one compare-and-swap
\* enters if the request fits and no handoff is owed, barging past any waiters. If it fails, a
\* task that waits goes on to queue; a task that only tries stays where it is.
Acquire(t) ==
  \* The task is between operations (on the reentrant lock it may hold guards already).
  /\ pc[t] \in (IF Kind = "Reentrant" THEN {"idle", "holding"} ELSE {"idle"})
  \* It may hold one more guard, and no node of an earlier wait of this task is still linked.
  /\ held[t] < MaxDepth /\ phase[t] = "none"
  \* No other task of its owner is busy, and the lock does not refuse it outright.
  /\ OwnerQuiet(t) /\ ~Refused(t)
  \* It has a turn left, and this one is counted.
  /\ HasTurn(t) /\ TakeTurn(t)
  \* Does its owner cover the request already?
  /\ IF Covered(t)
       \* Yes: one more guard, counted on the owner, and nothing of the raw lock moves.
       THEN /\ held' = [held EXCEPT ![t] = @ + 1] /\ mode' = [mode EXCEPT ![t] = Role[t]]
            \* A read is one more read of the owner;
            /\ rdepth' = IF Role[t] = "read" THEN [rdepth EXCEPT ![Owner[t]] = @ + 1] ELSE rdepth
            \* a write is one more write guard.
            /\ wdepth' = IF Role[t] = "write" THEN wdepth + 1 ELSE wdepth
            \* It holds.
            /\ pc' = [pc EXCEPT ![t] = "holding"]
            \* The lock word and the two entry ghosts are untouched.
            /\ UNCHANGED <<lockw, got, wowner, bargedOwed, readInUpgrade>>
       \* No. Does the request fit, with no handoff owed?
       ELSE IF CanBarge(Role[t])
         \* Yes: it enters the lock word,
         THEN /\ EnterWord(Role[t])
              \* and is a guard at once.
              /\ NewGuard(t) /\ UNCHANGED got
              \* If a handoff was owed at this moment, that is recorded;
              /\ bargedOwed' = (bargedOwed \/ handoff)
              \* and so is a reader that entered past a waiting upgrade.
              /\ readInUpgrade' = (readInUpgrade \/ (Role[t] = "read" /\ upgrading))
              \* It holds.
              /\ pc' = [pc EXCEPT ![t] = "holding"]
         \* No: only a task that waits goes on,
         ELSE /\ t \in Waits
              \* to spin, and then to push its node. (Losing this first attempt is no bypass:
              \* nobody has passed the caller yet.)
              /\ pc' = [pc EXCEPT ![t] = "pspin"]
              \* Nothing is entered.
              /\ UNCHANGED <<lockw, guards, bargedOwed, readInUpgrade>>
  \* Nothing else changes.
  /\ UNCHANGED <<qbits, owedCount, arrivals, waiting, upSlot, nodes, wake, wl, futs, dloc, ret,
                 dEarned, lost, overran>>

\* Whether a spinner that reads this state word stops and queues (queue::stops_spin): a handoff is
\* owed, or waiters are queued and nobody holds the drain role, as in Sync.tla.
StopsSpin == handoff \/ (queued /\ ~drainOn)

\* One round of the spin before queueing (contend::spin_take), as in Sync.tla: one load of the state
\* word, and, when its request fits and the spin need not stop, one compare-and-swap from exactly the
\* state loaded, which is one step here at the instant of the swap. A round that sees no room
\* changes nothing, so it is no step.
PreSpin(t) ==
  \* The task is spinning before it queues.
  /\ pc[t] = "pspin"
  \* Must it stop?
  /\ IF StopsSpin
       \* Yes: it goes on to push its node,
       THEN /\ pc' = [pc EXCEPT ![t] = "push"]
            \* entering nothing.
            /\ UNCHANGED <<lockw, guards, bargedOwed, readInUpgrade>>
       \* No: the round enters the lock word if the request fits (no handoff is owed here),
       ELSE /\ CanBarge(Role[t])
            \* exactly as the first attempt would,
            /\ EnterWord(Role[t])
            \* and it is a guard at once.
            /\ NewGuard(t) /\ UNCHANGED got
            \* (No handoff is owed, so nothing is barged past one.)
            /\ UNCHANGED bargedOwed
            \* A reader that entered past a waiting upgrade is recorded.
            /\ readInUpgrade' = (readInUpgrade \/ (Role[t] = "read" /\ upgrading))
            \* It holds.
            /\ pc' = [pc EXCEPT ![t] = "holding"]
  \* Nothing else changes.
  /\ UNCHANGED <<qbits, owedCount, arrivals, waiting, upSlot, nodes, wake, wl, futs, dloc, ret,
                 dEarned, lost, overran, turns>>

\* The spin before queueing runs out of rounds (spin::Budget): the task goes on to push its node,
\* after any number of rounds, none included.
PreSpinOut(t) ==
  \* The task is spinning before it queues, and goes on to push its node.
  /\ pc[t] = "pspin" /\ pc' = [pc EXCEPT ![t] = "push"]
  \* Nothing else changes.
  /\ UNCHANGED <<lockw, qbits, owedCount, arrivals, waiting, upSlot, nodes, wake, wl, guards, futs,
                 dloc, ret, dEarned, ghosts>>

\* The waiter arms a fresh node and pushes it on the arrival stack (Wait::enqueue, then
\* WaitQueue::push: one compare-and-swap on the stack).
Push(t) ==
  \* The task's first attempt failed and it is about to push.
  /\ pc[t] = "push"
  \* Its node joins the arrivals, behind the ones already there, waiting.
  /\ arrivals' = Append(arrivals, t) /\ phase' = [phase EXCEPT ![t] = "waiting"]
  \* Nobody has nudged it, and it is not marked owed.
  /\ nudged' = [nudged EXCEPT ![t] = FALSE] /\ owedMark' = [owedMark EXCEPT ![t] = FALSE]
  \* It asks for what its task asks for.
  /\ nkind' = [nkind EXCEPT ![t] = Role[t]]
  \* Next it publishes the node.
  /\ pc' = [pc EXCEPT ![t] = "enqt"]
  \* Nothing else changes.
  /\ UNCHANGED <<lockw, qbits, owedCount, waiting, upSlot, wake, wl, guards, futs, dloc, ret,
                 dEarned, ghosts>>

\* The waiter publishes its node: it raises QUEUED and makes sure a pass sees it (Policy::enqueue:
\* a transition with Step::Drain).
Publish(t) ==
  \* The task has pushed its node.
  /\ pc[t] = "enqt"
  \* QUEUED is up: a release now knows somebody waits.
  /\ queued' = TRUE
  \* A pass will see the node: this task's own, or the one under way. Then it looks at its node.
  /\ Transit(t, "park0")
  \* Nothing else changes.
  /\ UNCHANGED <<lockw, nudgeOut, handoff, wakeFront, owedCount, arrivals, waiting, upSlot, nodes,
                 wake, wl, guards, futs, dloc, ghosts>>

\* The future looks at its node (Waiter::park: one compare-and-swap that reads the two marks).
\* Granted: it lets the node go and goes on to finish. Nudged: it takes the nudge and goes on to
\* try again. Neither: it sleeps, and a later mark will wake it.
Look(t) ==
  \* A pass granted it what it asked for:
  IF phase[t] = "granted"
    \* the future lets its node go, with whatever nudge was on it,
    THEN /\ phase' = [phase EXCEPT ![t] = "none"] /\ nudged' = [nudged EXCEPT ![t] = FALSE]
         \* and goes on to take back the nudge marker.
         /\ pc' = [pc EXCEPT ![t] = "gcons"]
    \* A pass nudged it:
    ELSE IF nudged[t]
      \* the nudge is taken, and the node stays queued.
      THEN /\ nudged' = [nudged EXCEPT ![t] = FALSE] /\ phase' = phase
           \* The future goes on to take back the nudge marker.
           /\ pc' = [pc EXCEPT ![t] = "ncons"]
      \* Neither: the future returns Pending, with its node as it was.
      ELSE pc' = [pc EXCEPT ![t] = "parked"] /\ UNCHANGED <<phase, nudged>>

\* Right after publishing, the future settles its node (Wait::park with no waker).
Park0(t) ==
  \* The task has published its node, and looks at it.
  /\ pc[t] = "park0" /\ Look(t)
  \* Nothing else changes.
  /\ UNCHANGED <<lockw, qbits, owedCount, arrivals, waiting, upSlot, owedMark, nkind, wake, wl,
                 guards, futs, dloc, ret, dEarned, ghosts>>

\* After a failed try the future looks at its node at once (Wait::spin_park with a budget of zero,
\* or a node marked already: then Wait::park with the waker).
Repark(t) ==
  \* The task's try failed, and it looks at its node.
  /\ pc[t] = "repark" /\ Look(t)
  \* Nothing else changes.
  /\ UNCHANGED <<lockw, qbits, owedCount, arrivals, waiting, upSlot, owedMark, nkind, wake, wl,
                 guards, futs, dloc, ret, dEarned, ghosts>>

\* After a failed try the future spins on its own node instead (Wait::spin_park), as in Sync.tla:
\* Waiter::begin_spin found nothing marked and took the node's ARMED bit down, so a release that
\* nudges or grants it now only sets the mark (a task that is not "parked" goes on no wake list).
SpinStart(t) ==
  \* The task's try failed,
  /\ pc[t] = "repark"
  \* and its node is still waiting, with no nudge on it yet.
  /\ phase[t] = "waiting" /\ ~nudged[t]
  \* The future spins, awake: a release that marks its node now calls no waker.
  /\ spinning' = [spinning EXCEPT ![t] = TRUE]
  \* It goes on spinning.
  /\ pc' = [pc EXCEPT ![t] = "nspin"]
  \* Nothing else changes.
  /\ UNCHANGED <<lockw, qbits, owedCount, arrivals, waiting, upSlot, nodes, wake, wl, guards, bypass,
                 passOn, upW, dloc, ret, dEarned, ghosts>>

\* The spin on the node ends (it saw a mark, or ran out) and the future looks at its node, arming the
\* waker and reading the marks in one compare-and-swap. (Mutant SpinNoRecheck: a spin that runs out
\* sleeps without reading the marks; a nudge that came while it spun called no waker, so the waiter
\* sleeps beside it for ever.)
SpinEnd(t) ==
  \* The task is spinning on its node,
  /\ pc[t] = "nspin"
  \* and the spin is over.
  /\ spinning' = [spinning EXCEPT ![t] = FALSE]
  \* As built it looks at its node; under the bug it may also go to sleep without looking.
  /\ \/ Look(t)
     \* (only under the bug: straight to sleep,
     \/ /\ Mutant = "SpinNoRecheck"
        \* without reading what is on its node.)
        /\ pc' = [pc EXCEPT ![t] = "parked"] /\ UNCHANGED <<phase, nudged>>
  \* Nothing else changes.
  /\ UNCHANGED <<lockw, qbits, owedCount, arrivals, waiting, upSlot, owedMark, nkind, wake, wl,
                 guards, bypass, passOn, upW, dloc, ret, dEarned, ghosts>>

\* The executor polls a future whose waker was called. The future looks at its node.
Poll(t) ==
  \* The task sleeps, and its waker was called.
  /\ pc[t] = "parked" /\ wake[t]
  \* That call is used up, and it looks at its node.
  /\ wake' = [wake EXCEPT ![t] = FALSE] /\ Look(t)
  \* Nothing else changes.
  /\ UNCHANGED <<lockw, qbits, owedCount, arrivals, waiting, upSlot, owedMark, nkind, wl, guards,
                 futs, dloc, ret, dEarned, ghosts>>

\* A granted future takes back the nudge marker (WaitQueue::consume_nudge: one fetch-and), and its
\* acquire is complete: what the pass entered for it is its guard.
GrantCons(t) ==
  \* The task found its node granted.
  /\ pc[t] = "gcons"
  \* NUDGE_OUT comes down: a nudge may have been on its way when the grant came.
  /\ nudgeOut' = FALSE
  \* The entry the pass made for it becomes its guard, and its locals are forgotten.
  /\ SettleTo(t, got[t]) /\ Forget(t)
  \* It holds.
  /\ pc' = [pc EXCEPT ![t] = "holding"]
  \* Nothing else changes.
  /\ UNCHANGED <<lockw, drainOn, dirty, queued, handoff, wakeFront, owedCount, arrivals, waiting,
                 upSlot, nodes, wake, wl, passOn, upW, spinning, dloc, ret, dEarned, bargedOwed,
                 readInUpgrade, overran, turns>>

\* A nudged future takes back the nudge marker (one fetch-and), then chooses its try: the one that
\* would be its Bound-th counted loss is Lock::take_or_owe, every other is Lock::try_take.
\* (Mutant UnboundedBarging: always try_take, so HANDOFF is never raised.)
NudgeCons(t) ==
  \* The task found its node nudged.
  /\ pc[t] = "ncons"
  \* NUDGE_OUT comes down: this waiter has polled.
  /\ nudgeOut' = FALSE
  \* Is this the try that would bring the count to Bound?
  /\ pc' = [pc EXCEPT ![t] = IF bypass[t] + 1 = Bound /\ Mutant # "UnboundedBarging"
                               \* Then it is take_or_owe; otherwise a plain try.
                               THEN "owe" ELSE "try"]
  \* Nothing else changes.
  /\ UNCHANGED <<lockw, drainOn, dirty, queued, handoff, wakeFront, owedCount, arrivals, waiting,
                 upSlot, nodes, wake, wl, guards, futs, dloc, ret, dEarned, ghosts>>

\* The plain try (Lock::try_take): the same compare-and-swap as a fresh caller's. Winning, the
\* future goes on to withdraw its node. Losing counts one bypass, unless HANDOFF was up, and never
\* past Bound; then the future looks at its node again.
Try(t) ==
  \* The task is about to try.
  /\ pc[t] = "try"
  \* Does the request fit, with no handoff owed?
  /\ IF CanBarge(Role[t])
       \* Yes: it enters the lock word, on the future's account.
       THEN /\ EnterWord(Role[t]) /\ got' = [got EXCEPT ![t] = @ + 1]
            \* If a handoff was owed at this moment, that is recorded;
            /\ bargedOwed' = (bargedOwed \/ handoff)
            \* and so is a reader that entered past a waiting upgrade.
            /\ readInUpgrade' = (readInUpgrade \/ (Role[t] = "read" /\ upgrading))
            \* Next it withdraws its node. Nothing is counted.
            /\ pc' = [pc EXCEPT ![t] = "wd1"] /\ UNCHANGED <<bypass, spinning, lost>>
       \* No: a loss, counted unless HANDOFF was up. The ghost counts it if the node still waits.
       ELSE /\ lost' = [lost EXCEPT ![t] = IF ~handoff /\ phase[t] = "waiting" THEN @ + 1 ELSE @]
            \* The future's own count goes up by one, but never past Bound.
            /\ bypass' = [bypass EXCEPT ![t] = IF ~handoff /\ @ < Bound THEN @ + 1 ELSE @]
            \* It looks at its node again.
            /\ pc' = [pc EXCEPT ![t] = "repark"]
            \* Nothing is entered.
            /\ UNCHANGED <<lockw, got, bargedOwed, readInUpgrade>>
  \* Nothing else changes.
  /\ UNCHANGED <<qbits, owedCount, arrivals, waiting, upSlot, nodes, wake, wl, held, mode, rdepth,
                 wowner, wdepth, passOn, upW, spinning, dloc, ret, dEarned, overran, turns>>

\* The try that would be the waiter's Bound-th counted loss (Lock::take_or_owe, taken whole: its
\* seven instructions are checked one by one in Sync.tla). A waiter whose node was granted already,
\* or that finds HANDOFF up for another waiter, changes nothing. Otherwise it enters if the request
\* fits, or raises HANDOFF, counted owed and marked, and makes sure a pass sees it: no release can
\* fall between "the waiter found the lock taken for the Bound-th time" and "barging is shut".
Owe(t) ==
  \* The task is about to make that try.
  /\ pc[t] = "owe"
  \* Was its node granted meanwhile, or is HANDOFF up already?
  /\ IF phase[t] # "waiting" \/ handoff
       \* Yes: nothing counts, and it looks at its node again.
       THEN /\ pc' = [pc EXCEPT ![t] = "repark"]
            \* Nothing else of the try changes.
            /\ UNCHANGED <<lockw, drainOn, dirty, handoff, owedCount, owedMark, got, bypass, spinning, lost,
                           ret, dEarned, readInUpgrade>>
       \* No. Does the request fit?
       ELSE IF Admits(Role[t])
         \* Yes: it enters the lock word, on the future's account.
         THEN /\ EnterWord(Role[t]) /\ got' = [got EXCEPT ![t] = @ + 1]
              \* A reader that entered past a waiting upgrade is recorded.
              /\ readInUpgrade' = (readInUpgrade \/ (Role[t] = "read" /\ upgrading))
              \* Next it withdraws its node.
              /\ pc' = [pc EXCEPT ![t] = "wd1"]
              \* Nothing is owed or counted.
              /\ UNCHANGED <<drainOn, dirty, handoff, owedCount, owedMark, bypass, spinning, lost, ret,
                             dEarned>>
         \* No: HANDOFF goes up, in this same compare-and-swap,
         ELSE /\ handoff' = TRUE
              \* the waiter counts itself owed and marks its node,
              /\ owedCount' = owedCount + 1 /\ owedMark' = [owedMark EXCEPT ![t] = TRUE]
              \* and this is its Bound-th counted loss.
              /\ lost' = [lost EXCEPT ![t] = @ + 1] /\ bypass' = [bypass EXCEPT ![t] = @ + 1]
              \* A pass will see the bit: this task's own, or the one under way. Then it looks at
              \* its node again.
              /\ Transit(t, "repark")
              \* Nothing is entered.
              /\ UNCHANGED <<lockw, got, readInUpgrade>>
  \* Nothing else changes.
  /\ UNCHANGED <<queued, nudgeOut, wakeFront, arrivals, waiting, upSlot, phase, nudged, nkind, wake,
                 wl, held, mode, rdepth, wowner, wdepth, passOn, upW, spinning, dloc, bargedOwed, overran,
                 turns>>

----------------------------------------------------------------------------
\* Leaving the queue: Contend::cancel, src/sync/contend.rs.

\* The future leaves its node (Wait::cancel, Waiter::withdraw: one compare-and-swap). Still
\* waiting: the node becomes "cancelled" and the future notes whether a nudge sat on it unused.
\* Granted first: the future lets the node go and will give back what was entered for it.
Withdraw(t) ==
  \* A pass granted the node first:
  IF phase[t] = "granted"
    \* the future lets the node go, with whatever nudge was on it,
    THEN /\ phase' = [phase EXCEPT ![t] = "none"] /\ nudged' = [nudged EXCEPT ![t] = FALSE]
         \* and goes on to give the grant back. It carries no nudge to pass on.
         /\ pc' = [pc EXCEPT ![t] = "gb1"] /\ UNCHANGED passOn
    \* The node still waits: it becomes "cancelled",
    ELSE /\ phase' = [phase EXCEPT ![t] = "cancelled"]
         \* the future notes whether it carried an unused nudge, which is gone from the node,
         /\ passOn' = [passOn EXCEPT ![t] = nudged[t]] /\ nudged' = [nudged EXCEPT ![t] = FALSE]
         \* and it goes on to tell the queue.
         /\ pc' = [pc EXCEPT ![t] = "wd2"]

\* A future that entered by its own try leaves the queue.
Wd1(t) ==
  \* The task's try succeeded, and it leaves its node.
  /\ pc[t] = "wd1" /\ Withdraw(t)
  \* Nothing else changes.
  /\ UNCHANGED <<lockw, qbits, owedCount, arrivals, waiting, upSlot, owedMark, nkind, wake, wl,
                 guards, bypass, upW, spinning, dloc, ret, dEarned, ghosts>>

\* The waiting future is dropped (the caller gave up): it leaves its node. A wake on its way to it
\* now has nobody to wake.
Cancel(t) ==
  \* The task's future may be dropped, and it sleeps.
  /\ t \in Drops /\ pc[t] = "parked"
  \* A call of its waker, made or about to be made, is void.
  /\ wake' = [wake EXCEPT ![t] = FALSE] /\ wl' = wl \ {t}
  \* Its locals are forgotten, and it leaves its node.
  /\ Forget(t) /\ Withdraw(t)
  \* Nothing else changes.
  /\ UNCHANGED <<lockw, qbits, owedCount, arrivals, waiting, upSlot, owedMark, nkind, guards, upW, spinning,
                 dloc, ret, dEarned, bargedOwed, readInUpgrade, overran, turns>>

\* The withdrawn future tells the queue (Policy::withdrawn: a transition with Step::Drain): a
\* nudge marker that was for it is void, so NUDGE_OUT comes down; a nudge it carried unused passes
\* to the next front, through WAKE_FRONT; and a pass must look, to prune the node and reconsider
\* what it was blocking. With that its poll, or its drop, is over.
Wd2(t) ==
  \* The task withdrew its node.
  /\ pc[t] = "wd2"
  \* NUDGE_OUT comes down, and WAKE_FRONT goes up if the node carried an unused nudge.
  /\ nudgeOut' = FALSE /\ wakeFront' = (wakeFront \/ passOn[t])
  \* That local is forgotten.
  /\ passOn' = [passOn EXCEPT ![t] = FALSE]
  \* What it entered itself becomes its guard, and its other locals are forgotten.
  /\ SettleTo(t, got[t]) /\ Forget(t)
  \* A pass will look: this task's own, or the one under way. Then it holds, or is idle.
  /\ Transit(t, RestPc(t, got[t]))
  \* Nothing else changes.
  /\ UNCHANGED <<lockw, queued, handoff, owedCount, arrivals, waiting, upSlot, nodes, wake, wl, upW, spinning,
                 dloc, bargedOwed, readInUpgrade, overran, turns>>

\* A future that leaves after a grant takes back the nudge marker (one fetch-and) before it gives
\* the grant back.
Gb1(t) ==
  \* The task found its node granted as it left.
  /\ pc[t] = "gb1"
  \* NUDGE_OUT comes down, and next it gives the grant back.
  /\ nudgeOut' = FALSE /\ pc' = [pc EXCEPT ![t] = "gb2"]
  \* Nothing else changes.
  /\ UNCHANGED <<lockw, drainOn, dirty, queued, handoff, wakeFront, owedCount, arrivals, waiting,
                 upSlot, nodes, wake, wl, guards, futs, dloc, ret, dEarned, ghosts>>

\* It gives the grant back (Lock::give_back, which is unlock_kind: one read-modify-write). If
\* waiters need telling it goes on to ask for a pass; otherwise its poll, or its drop, is over.
Gb2(t) ==
  \* The task took back the marker.
  /\ pc[t] = "gb2"
  \* What was entered for it leaves the lock word.
  /\ LeaveWord(Role[t])
  \* Must a pass run?
  /\ IF ReleaseWants(Role[t])
       \* Yes: the entry leaves its hand, and it goes on to ask for the pass.
       THEN /\ got' = [got EXCEPT ![t] = @ - 1] /\ pc' = [pc EXCEPT ![t] = "gb3"]
            \* Its guard and its locals wait for that step.
            /\ UNCHANGED <<held, mode, rdepth, wowner, wdepth, bypass, spinning, lost>>
       \* No: what it entered itself, if anything, becomes its guard,
       ELSE /\ SettleTo(t, got[t] - 1)
            \* its locals are forgotten, and it holds, or is idle.
            /\ Forget(t) /\ pc' = [pc EXCEPT ![t] = RestPc(t, got[t] - 1)]
  \* Nothing else changes.
  /\ UNCHANGED <<qbits, owedCount, arrivals, waiting, upSlot, nodes, wake, wl, passOn, upW, spinning, dloc,
                 ret, dEarned, bargedOwed, readInUpgrade, overran, turns>>

\* It asks for the pass (Policy::released: a transition with Step::Wake that raises WAKE_FRONT).
Gb3(t) ==
  \* The task gave a grant back while waiters needed telling.
  /\ pc[t] = "gb3"
  \* What it entered itself, if anything, becomes its guard, and its locals are forgotten.
  /\ SettleTo(t, got[t]) /\ Forget(t)
  \* It asks: its own pass, or a request left with the holder of the role. Then it holds, or is
  \* idle.
  /\ Asks(t, RestPc(t, got[t]))
  \* Nothing else changes.
  /\ UNCHANGED <<lockw, queued, nudgeOut, handoff, owedCount, arrivals, waiting, upSlot, nodes,
                 wake, wl, passOn, upW, spinning, dloc, bargedOwed, readInUpgrade, overran, turns>>

----------------------------------------------------------------------------
\* Releasing.

\* A guard of the plain lock is dropped (read_unlock, write_unlock or upgradable_unlock: one
\* read-modify-write on the state word). If waiters need telling, the task goes on to ask for a
\* pass.
ReleasePlain(t) ==
  \* The plain lock, and the task holds a guard.
  /\ Kind = "Plain" /\ pc[t] = "holding" /\ held[t] > 0
  \* The guard leaves the lock word, and the task holds nothing.
  /\ LeaveWord(mode[t]) /\ held' = [held EXCEPT ![t] = 0] /\ mode' = [mode EXCEPT ![t] = "none"]
  \* Must a pass run?
  /\ IF ReleaseWants(mode[t])
       \* Yes: it goes on to ask for it, and is idle after that.
       THEN pc' = [pc EXCEPT ![t] = "relt"] /\ ret' = [ret EXCEPT ![t] = "idle"]
       \* No: it is idle at once.
       ELSE pc' = [pc EXCEPT ![t] = "idle"] /\ UNCHANGED ret
  \* Nothing else changes.
  /\ UNCHANGED <<qbits, owedCount, arrivals, waiting, upSlot, nodes, wake, wl, got, rdepth, wowner,
                 wdepth, futs, dloc, dEarned, ghosts>>

\* A read guard of the reentrant lock is dropped (ReentrantRwLock::read_unlock). The owner's last
\* read gives its share of the raw lock back, unless the owner also writes: then its reads hold no
\* share of their own.
ReleaseRead(t) ==
  \* The reentrant lock, and the task holds a read; no other task of its owner is busy.
  /\ Kind = "Reentrant" /\ pc[t] = "holding" /\ held[t] > 0 /\ Role[t] = "read" /\ OwnerQuiet(t)
  \* The task holds one guard fewer,
  /\ held' = [held EXCEPT ![t] = @ - 1]
  \* of no kind if that was its last,
  /\ mode' = [mode EXCEPT ![t] = IF held[t] = 1 THEN "none" ELSE @]
  \* and its owner one read fewer.
  /\ rdepth' = [rdepth EXCEPT ![Owner[t]] = @ - 1]
  \* next: it keeps holding if it has a guard left, else it is idle.
  /\ LET next == IF held[t] > 1 THEN "holding" ELSE "idle"
     \* Was that the owner's last read, with no write of its own? (Mutant OwnerUnaware: every
     \* read took a share of its own, so every read gives one back.)
     IN IF (rdepth[Owner[t]] = 1 \/ Mutant = "OwnerUnaware") /\ wowner # Owner[t]
          \* Yes: one reader leaves the raw lock.
          THEN /\ LeaveWord("read")
               \* Must a pass run?
               /\ IF ReleaseWants("read")
                    \* Yes: it goes on to ask for it.
                    THEN pc' = [pc EXCEPT ![t] = "relt"] /\ ret' = [ret EXCEPT ![t] = next]
                    \* No: it goes on at once.
                    ELSE pc' = [pc EXCEPT ![t] = next] /\ UNCHANGED ret
          \* No: the raw lock is not touched.
          ELSE pc' = [pc EXCEPT ![t] = next] /\ UNCHANGED <<lockw, ret>>
  \* Nothing else changes.
  /\ UNCHANGED <<qbits, owedCount, arrivals, waiting, upSlot, nodes, wake, wl, got, wowner, wdepth,
                 futs, dloc, dEarned, ghosts>>

\* A write guard of the reentrant lock is dropped (ReentrantRwLock::write_unlock). The owner's
\* last write frees the raw lock, unless the owner also reads: then the write becomes one read in
\* a single transition (RawRwLock::downgrade), which asks for a pass if waiters are queued.
ReleaseWrite(t) ==
  \* The reentrant lock, and the task holds a write; no other task of its owner is busy.
  /\ Kind = "Reentrant" /\ pc[t] = "holding" /\ held[t] > 0 /\ Role[t] = "write" /\ OwnerQuiet(t)
  \* The task holds one guard fewer,
  /\ held' = [held EXCEPT ![t] = @ - 1]
  \* of no kind if that was its last,
  /\ mode' = [mode EXCEPT ![t] = IF held[t] = 1 THEN "none" ELSE @]
  \* and one write guard fewer is out. The last one leaves the lock with no writing owner.
  /\ wdepth' = wdepth - 1 /\ wowner' = IF wdepth = 1 THEN NoOwner ELSE wowner
  \* next: it keeps holding if it has a guard left, else it is idle.
  /\ LET next == IF held[t] > 1 THEN "holding" ELSE "idle"
     \* Was that the owner's last write?
     IN IF wdepth > 1
          \* No: the raw lock is not touched.
          THEN pc' = [pc EXCEPT ![t] = next] /\ UNCHANGED <<lockw, qbits, ret, dEarned>>
          \* Yes. Does the owner read as well?
          ELSE IF rdepth[Owner[t]] > 0
            \* Yes: the write becomes a read,
            THEN /\ writer' = FALSE /\ readers' = readers + 1
                 \* (the other two bits stay)
                 /\ UNCHANGED <<upgradable, upgrading>>
                 \* and, in the same transition, a pass is asked for if waiters are queued.
                 /\ IF queued THEN Asks(t, next) /\ UNCHANGED <<queued, nudgeOut, handoff>>
                    \* (nobody queued: the transition only sets the word)
                    ELSE pc' = [pc EXCEPT ![t] = next] /\ UNCHANGED <<qbits, ret, dEarned>>
            \* No: the writer leaves the raw lock.
            ELSE /\ LeaveWord("write") /\ UNCHANGED <<qbits, dEarned>>
                 \* Must a pass run?
                 /\ IF ReleaseWants("write")
                      \* Yes: it goes on to ask for it.
                      THEN pc' = [pc EXCEPT ![t] = "relt"] /\ ret' = [ret EXCEPT ![t] = next]
                      \* No: it goes on at once.
                      ELSE pc' = [pc EXCEPT ![t] = next] /\ UNCHANGED ret
  \* Nothing else changes.
  /\ UNCHANGED <<owedCount, arrivals, waiting, upSlot, nodes, wake, wl, got, rdepth, futs, dloc,
                 ghosts>>

\* A guard is dropped, on either lock.
Release(t) == ReleasePlain(t) \/ ReleaseRead(t) \/ ReleaseWrite(t)

\* The releasing task asks for the pass (Policy::released: a transition with Step::Wake that
\* raises WAKE_FRONT).
Released(t) ==
  \* The task gave something back while waiters needed telling.
  /\ pc[t] = "relt"
  \* It asks: its own pass, or a request left with the holder of the role. Then it goes on.
  /\ Asks(t, ret[t])
  \* Nothing else changes.
  /\ UNCHANGED <<lockw, queued, nudgeOut, handoff, owedCount, arrivals, waiting, upSlot, nodes,
                 wake, wl, guards, futs, dloc, ghosts>>

----------------------------------------------------------------------------
\* The upgrade: UpgradeWait in src/sync/raw_rwlock.rs.

\* upgrade() on the upgradable guard, first poll: the holder raises UPGRADING (one fetch-or), which
\* holds new readers back from now on.
UpStart(t) ==
  \* The task may upgrade, holds the upgradable read, and no node of its earlier wait is linked.
  /\ t \in Upgrades /\ pc[t] = "holding" /\ mode[t] = "upread" /\ phase[t] = "none"
  \* UPGRADING goes up, and next it tries to take the write.
  /\ upgrading' = TRUE /\ pc' = [pc EXCEPT ![t] = "uptry"]
  \* Nothing else changes.
  /\ UNCHANGED <<readers, writer, upgradable, qbits, owedCount, arrivals, waiting, upSlot, nodes,
                 wake, wl, guards, futs, dloc, ret, dEarned, ghosts>>

\* The upgrader tries to take the write (RawRwLock::try_upgrade: one compare-and-swap): with no
\* reader inside, the upgradable read becomes the write. Otherwise it goes on to queue.
UpTry(t) ==
  \* The task raised UPGRADING.
  /\ pc[t] = "uptry"
  \* Is no reader inside?
  /\ IF readers = 0
       \* Yes: UPGRADABLE and UPGRADING come down and WRITER goes up, in one step.
       THEN /\ upgradable' = FALSE /\ upgrading' = FALSE /\ writer' = TRUE /\ readers' = readers
            \* Its guard is the write guard now, and it holds.
            /\ mode' = [mode EXCEPT ![t] = "write"] /\ pc' = [pc EXCEPT ![t] = "holding"]
       \* No: it goes on to push a node. Nothing changes yet.
       ELSE pc' = [pc EXCEPT ![t] = "uppush"] /\ UNCHANGED <<lockw, mode>>
  \* Nothing else changes.
  /\ UNCHANGED <<qbits, owedCount, arrivals, waiting, upSlot, nodes, wake, wl, got, held, rdepth,
                 wowner, wdepth, futs, dloc, ret, dEarned, ghosts>>

\* The upgrader pushes a node that asks for the upgrade (Wait::enqueue with payload UPGRADE).
UpPush(t) ==
  \* The task found readers inside.
  /\ pc[t] = "uppush"
  \* Its node joins the arrivals, waiting.
  /\ arrivals' = Append(arrivals, t) /\ phase' = [phase EXCEPT ![t] = "waiting"]
  \* Nobody has nudged it, and it is not marked owed.
  /\ nudged' = [nudged EXCEPT ![t] = FALSE] /\ owedMark' = [owedMark EXCEPT ![t] = FALSE]
  \* It asks for the upgrade.
  /\ nkind' = [nkind EXCEPT ![t] = "upgrade"]
  \* Next it publishes the node.
  /\ pc' = [pc EXCEPT ![t] = "upenq"]
  \* Nothing else changes.
  /\ UNCHANGED <<lockw, qbits, owedCount, waiting, upSlot, wake, wl, guards, futs, dloc, ret,
                 dEarned, ghosts>>

\* The upgrader publishes its node (Policy::enqueue: a transition with Step::Drain).
UpPublish(t) ==
  \* The task has pushed its node.
  /\ pc[t] = "upenq"
  \* QUEUED is up.
  /\ queued' = TRUE
  \* A pass will see the node: this task's own, or the one under way. Then it looks at its node.
  /\ Transit(t, "uppark0")
  \* Nothing else changes.
  /\ UNCHANGED <<lockw, nudgeOut, handoff, wakeFront, owedCount, arrivals, waiting, upSlot, nodes,
                 wake, wl, guards, futs, dloc, ghosts>>

\* The upgrader looks at its node (Wait::park). Granted: a pass saw the last reader gone, so the
\* upgrader lets the node go and goes on to take the write. Otherwise it sleeps.
UpLook(t) ==
  \* A pass granted the node:
  IF phase[t] = "granted"
    \* the future lets it go, and goes on to take the write.
    THEN phase' = [phase EXCEPT ![t] = "none"] /\ pc' = [pc EXCEPT ![t] = "upfin"]
    \* Not yet: it returns Pending.
    ELSE pc' = [pc EXCEPT ![t] = "upparked"] /\ UNCHANGED phase

\* Right after publishing, the upgrader settles its node.
UpPark0(t) ==
  \* The task has published its node, and looks at it.
  /\ pc[t] = "uppark0" /\ UpLook(t)
  \* Nothing else changes.
  /\ UNCHANGED <<lockw, qbits, owedCount, arrivals, waiting, upSlot, nudged, owedMark, nkind, wake,
                 wl, guards, futs, dloc, ret, dEarned, ghosts>>

\* The executor polls an upgrade whose waker was called.
UpPoll(t) ==
  \* The upgrade sleeps, and its waker was called.
  /\ pc[t] = "upparked" /\ wake[t]
  \* That call is used up, and it looks at its node.
  /\ wake' = [wake EXCEPT ![t] = FALSE] /\ UpLook(t)
  \* Nothing else changes.
  /\ UNCHANGED <<lockw, qbits, owedCount, arrivals, waiting, upSlot, nudged, owedMark, nkind, wl,
                 guards, futs, dloc, ret, dEarned, ghosts>>

\* The granted upgrader takes the write itself (RawRwLock::finish_upgrade: one fetch-xor of the
\* three bits). Until this step UPGRADABLE and UPGRADING were its own and kept every reader and
\* writer out. (Mutant PassTakesWrite: the pass already changed the word for it, as the code did
\* before E33.)
UpFinish(t) ==
  \* The task found its upgrade granted.
  /\ pc[t] = "upfin"
  \* Under the bug the word is already the writer's;
  /\ IF Mutant = "PassTakesWrite" THEN UNCHANGED lockw
     \* as built, UPGRADABLE and UPGRADING come down and WRITER goes up, in one step.
     ELSE upgradable' = FALSE /\ upgrading' = FALSE /\ writer' = TRUE /\ readers' = readers
  \* Its guard is the write guard now, and it holds.
  /\ mode' = [mode EXCEPT ![t] = "write"] /\ pc' = [pc EXCEPT ![t] = "holding"]
  \* Nothing else changes.
  /\ UNCHANGED <<qbits, owedCount, arrivals, waiting, upSlot, nodes, wake, wl, got, held, rdepth,
                 wowner, wdepth, futs, dloc, ret, dEarned, ghosts>>

\* The waiting upgrade is dropped (UpgradeWait::cancel, first instruction: Wait::cancel, one
\* compare-and-swap on the node). Whatever the node's state, the upgradable read is still the
\* task's own, and it goes on to let it go. (Mutant PassTakesWrite: a granted node means the pass
\* made the task the writer, so it goes on to drop the write instead.)
UpCancel(t) ==
  \* The task's upgrade may be dropped, and it sleeps.
  /\ t \in Drops /\ pc[t] = "upparked"
  \* A call of its waker, made or about to be made, is void.
  /\ wake' = [wake EXCEPT ![t] = FALSE] /\ wl' = wl \ {t}
  \* Was the node granted first?
  /\ IF phase[t] = "granted"
       \* Yes: the future lets it go; the queue need not be told.
       THEN /\ phase' = [phase EXCEPT ![t] = "none"] /\ upW' = [upW EXCEPT ![t] = FALSE]
            \* As built it lets the upgradable read go next; under the bug, the write.
            /\ pc' = [pc EXCEPT ![t] = IF Mutant = "PassTakesWrite" THEN "upwul" ELSE "upunl"]
       \* No: the node becomes "cancelled", and the queue must be told afterwards.
       ELSE /\ phase' = [phase EXCEPT ![t] = "cancelled"] /\ upW' = [upW EXCEPT ![t] = TRUE]
            \* It lets the upgradable read go next.
            /\ pc' = [pc EXCEPT ![t] = "upunl"]
  \* Nothing else changes.
  /\ UNCHANGED <<lockw, qbits, owedCount, arrivals, waiting, upSlot, nudged, owedMark, nkind,
                 guards, bypass, passOn, spinning, dloc, ret, dEarned, ghosts>>

\* The dropped upgrade lets the upgradable read go (RawRwLock::upgradable_unlock: one fetch-and
\* that lowers UPGRADABLE and UPGRADING). It is the only step that ever releases them for this
\* task. Since UPGRADING was up, or if the queue wants it, it goes on to ask for a pass.
UpUnlock(t) ==
  \* The task withdrew from its upgrade.
  /\ pc[t] = "upunl"
  \* The two bits come down, and the task holds nothing.
  /\ LeaveWord("upread") /\ held' = [held EXCEPT ![t] = 0] /\ mode' = [mode EXCEPT ![t] = "none"]
  \* Must a pass run? Then it asks for it next; else it tells the queue about its node if that
  \* withdrew, or is idle.
  /\ pc' = [pc EXCEPT ![t] = IF ReleaseWants("upread") THEN "uprel"
                               \* (no pass asked for)
                               ELSE IF upW[t] THEN "upwd" ELSE "idle"]
  \* Nothing else changes.
  /\ UNCHANGED <<qbits, owedCount, arrivals, waiting, upSlot, nodes, wake, wl, got, rdepth, wowner,
                 wdepth, futs, dloc, ret, dEarned, ghosts>>

\* Mutant PassTakesWrite only: a dropped upgrade whose node was granted drops the write the pass
\* took for it (RawRwLock::write_unlock), as the code did before E33.
UpWriteUnlock(t) ==
  \* The task found its node granted as it dropped the upgrade.
  /\ pc[t] = "upwul"
  \* WRITER comes down, and the task holds nothing.
  /\ LeaveWord("write") /\ held' = [held EXCEPT ![t] = 0] /\ mode' = [mode EXCEPT ![t] = "none"]
  \* If the queue wants a pass it asks for it next; else it is idle.
  /\ pc' = [pc EXCEPT ![t] = IF ReleaseWants("write") THEN "uprel" ELSE "idle"]
  \* Nothing else changes.
  /\ UNCHANGED <<qbits, owedCount, arrivals, waiting, upSlot, nodes, wake, wl, got, rdepth, wowner,
                 wdepth, futs, dloc, ret, dEarned, ghosts>>

\* The dropped upgrade asks for the pass (Policy::released).
UpAsk(t) ==
  \* The task let its guard go while a pass was needed.
  /\ pc[t] = "uprel"
  \* It asks: its own pass, or a request left with the holder of the role. Then it tells the queue
  \* about its node if that withdrew, or is idle.
  /\ Asks(t, IF upW[t] THEN "upwd" ELSE "idle")
  \* Nothing else changes.
  /\ UNCHANGED <<lockw, queued, nudgeOut, handoff, owedCount, arrivals, waiting, upSlot, nodes,
                 wake, wl, guards, futs, dloc, ghosts>>

\* The dropped upgrade tells the queue that its node withdrew (Policy::withdrawn with no nudge to
\* pass on: a transition with Step::Drain that lowers NUDGE_OUT). With that the drop is over.
UpWithdrawn(t) ==
  \* The task's node withdrew and its guard is gone.
  /\ pc[t] = "upwd"
  \* NUDGE_OUT comes down, and the local is forgotten.
  /\ nudgeOut' = FALSE /\ upW' = [upW EXCEPT ![t] = FALSE]
  \* A pass will look: this task's own, or the one under way. Then it is idle.
  /\ Transit(t, "idle")
  \* Nothing else changes.
  /\ UNCHANGED <<lockw, queued, handoff, wakeFront, owedCount, arrivals, waiting, upSlot, nodes,
                 wake, wl, guards, bypass, passOn, spinning, dloc, ghosts>>

\* Mutant AnyReaderUpgrade only: a plain reader asks to upgrade. It holds new readers back and
\* waits to be the only reader, which another reader doing the same never lets happen.
ReaderUpgrade(t) ==
  \* Only the planted bug has this step, for a task that holds a plain read.
  /\ Mutant = "AnyReaderUpgrade" /\ t \in Upgrades /\ pc[t] = "holding" /\ mode[t] = "read"
  \* UPGRADING goes up, and the task waits.
  /\ upgrading' = TRUE /\ pc' = [pc EXCEPT ![t] = "rupwait"]
  \* Nothing else changes.
  /\ UNCHANGED <<readers, writer, upgradable, qbits, owedCount, arrivals, waiting, upSlot, nodes,
                 wake, wl, guards, futs, dloc, ret, dEarned, ghosts>>

----------------------------------------------------------------------------
\* The drain: Policy::drain in src/sync/queue.rs, running RawRwLock::pass. Only the task that
\* holds the drain role takes these steps.

\* The queue lets go of the nodes in G (release_queued, src/sync/waiter.rs): each is free for its
\* task to use again, and each one that was marked owed is taken out of the owed count.
LetGo(G) ==
  \* The nodes are free.
  /\ phase' = [u \in Tasks |-> IF u \in G THEN "none" ELSE phase[u]]
  \* Their owed marks are gone,
  /\ owedMark' = [u \in Tasks |-> IF u \in G THEN FALSE ELSE owedMark[u]]
  \* and so is their share of the owed count.
  /\ owedCount' = owedCount - Cardinality({u \in G : owedMark[u]})

\* A pass begins. One fetch-and brings DIRTY and WAKE_FRONT down, remembering whether WAKE_FRONT
\* was up; one swap takes every arrival. The pass lets go of a withdrawn upgrade's node in its
\* slot first (the upgradable read is free once its holder dropped the upgrade, so the next
\* upgrade's node may be among these arrivals). Then it sorts the arrivals: withdrawn ones are let
\* go, an upgrade's node goes to the slot, the others join the list. It may sweep withdrawn nodes
\* out of the list (List::tidy), or not, and lets go of the withdrawn nodes at the front.
\* (Mutant PassTakesWrite: no look at the slot's node first, as before E33.)
D0(d) ==
  \* The task holds the drain role and starts a pass.
  /\ pc[d] = "d0"
  \* DIRTY and WAKE_FRONT come down, and the pass remembers whether WAKE_FRONT was up.
  /\ dirty' = FALSE /\ wakeFront' = FALSE /\ dWF' = wakeFront
  \* The arrival stack is empty.
  /\ arrivals' = <<>>
  \* stale: the withdrawn node in the slot, if there is one and the pass looks.
  /\ LET stale == IF upSlot # 0 /\ phase[upSlot] = "cancelled" /\ Mutant # "PassTakesWrite"
                    \* (that node, or nothing)
                    THEN {upSlot} ELSE {}
         \* ups: the arriving upgrade nodes that still wait (at most one, as built).
         ups == {u \in Range(arrivals) : nkind[u] = "upgrade" /\ phase[u] # "cancelled"}
         \* others: the other arrivals, in order.
         others == SelectSeq(arrivals, LAMBDA u : nkind[u] # "upgrade")
         \* whole: the list with the live ones of those appended.
         whole == waiting \o Alive(others)
     \* The pass sweeps, or not; there is a choice only if the list holds a withdrawn node.
     IN \E sweep \in IF Dead(whole) = {} THEN {FALSE} ELSE BOOLEAN :
          \* kept: the list with every node, or only the live ones.
          LET kept == IF sweep THEN Alive(whole) ELSE whole
          \* The list is that, without the withdrawn nodes at its front.
          IN /\ waiting' = FrontOf(kept)
             \* An arriving upgrade takes the slot; else the slot keeps its node, unless that one
             \* was let go.
             /\ upSlot' = IF ups # {} THEN CHOOSE u \in ups : TRUE
                          \* (no upgrade arrived)
                          ELSE IF stale # {} THEN 0 ELSE upSlot
             \* The queue lets go of the stale node, the withdrawn arrivals, the swept nodes and
             \* those at the front.
             /\ LetGo(stale \cup Dead(arrivals) \cup (IF sweep THEN Dead(whole) ELSE {})
                      \* (the front)
                      \cup FrontOff(kept))
             \* With an upgrade in the slot the pass looks at it next; else at HANDOFF.
             /\ pc' = [pc EXCEPT ![d] = IF (IF ups # {} THEN TRUE ELSE upSlot # 0 /\ stale = {})
                                          \* (the slot holds a node / it is empty)
                                          THEN "du" ELSE "dhr"]
  \* The ghost: the pass uses up what earned it, and a pass that nothing earned is noted.
  /\ dEarned' = FALSE /\ overran' = (overran \/ ~dEarned)
  \* Nothing else changes.
  /\ UNCHANGED <<lockw, drainOn, queued, nudgeOut, handoff, nudged, nkind, wake, wl, guards, futs,
                 dSR, dSW, dSU, dSG, dPos, dFirst, dRaised, dSent, dHand, dFrontW, dClearQ, dClearH,
                 ret, lost, bargedOwed, readInUpgrade, turns>>

\* The pass looks at the lock for the waiting upgrade (RawRwLock::grant_upgrade: one load). With
\* UPGRADING up and no reader inside it goes on to grant the node; otherwise the upgrade keeps
\* waiting. The word is not changed. (Mutant PassTakesWrite, the code before E33: with UPGRADING
\* down it takes the upgrade for dropped and lets the node go; with no reader inside it takes the
\* write for the node, lowering UPGRADABLE and UPGRADING, whoever raised them.)
DU(d) ==
  \* The pass has a node in its upgrade slot.
  /\ pc[d] = "du"
  \* As built:
  /\ IF Mutant # "PassTakesWrite"
       \* the upgrade is ready when UPGRADING is up and no reader is inside,
       THEN /\ pc' = [pc EXCEPT ![d] = IF upgrading /\ readers = 0 THEN "dug" ELSE "dhr"]
            \* and nothing is changed.
            /\ UNCHANGED <<lockw, upSlot, phase, owedMark, owedCount, dHand>>
       \* Under the bug, with UPGRADING down,
       ELSE IF ~upgrading
         \* the node is let go and the slot is empty.
         THEN /\ LetGo({upSlot}) /\ upSlot' = 0 /\ pc' = [pc EXCEPT ![d] = "dhr"]
              \* The word is not changed.
              /\ UNCHANGED <<lockw, dHand>>
         \* With a reader inside the upgrade keeps waiting.
         ELSE IF readers # 0
           \* Nothing is changed.
           THEN /\ pc' = [pc EXCEPT ![d] = "dhr"]
                \* (nothing at all)
                /\ UNCHANGED <<lockw, upSlot, phase, owedMark, owedCount, dHand>>
           \* With none, the pass takes the write: the two bits come down and WRITER goes up.
           ELSE /\ upgradable' = FALSE /\ upgrading' = FALSE /\ writer' = TRUE /\ readers' = readers
                \* It holds a write for the node, and grants it next.
                /\ dHand' = "write" /\ pc' = [pc EXCEPT ![d] = "dug"]
                \* The node is not touched yet.
                /\ UNCHANGED <<upSlot, phase, owedMark, owedCount>>
  \* Nothing else changes.
  /\ UNCHANGED <<qbits, arrivals, waiting, nudged, nkind, wake, wl, guards, futs, dWF, dSR, dSW,
                 dSU, dSG, dPos, dFirst, dRaised, dSent, dFrontW, dClearQ, dClearH, ret, dEarned,
                 ghosts>>

\* The pass grants the upgrade's node (WakeList::grant: one compare-and-swap on the node), which
\* tells the upgrader that the readers have left. A node that withdrew meanwhile takes no grant.
\* Either way the queue lets go of the node and the slot is empty. (Mutant PassTakesWrite: if the
\* node withdrew, the pass goes on to put back the write it took.)
DUG(d) ==
  \* The pass found the upgrade ready.
  /\ pc[d] = "dug"
  \* The slot is empty.
  /\ upSlot' = 0
  \* Does the node still wait?
  /\ IF phase[upSlot] = "waiting"
       \* Yes: it is granted, and a sleeping upgrader is to be woken.
       THEN /\ phase' = [phase EXCEPT ![upSlot] = "granted"]
            \* (the wake list)
            /\ wl' = IF pc[upSlot] = "upparked" THEN wl \cup {upSlot} ELSE wl
            \* The pass's hand is empty, and it goes on to look at HANDOFF.
            /\ dHand' = "none" /\ pc' = [pc EXCEPT ![d] = "dhr"]
       \* No, it withdrew: the node is free, and nobody is woken.
       ELSE /\ phase' = [phase EXCEPT ![upSlot] = "none"] /\ UNCHANGED <<wl, dHand>>
            \* As built the pass goes on; under the bug it puts the write back first.
            /\ pc' = [pc EXCEPT ![d] = IF Mutant = "PassTakesWrite" THEN "duu" ELSE "dhr"]
  \* Nothing else changes.
  /\ UNCHANGED <<lockw, qbits, owedCount, arrivals, waiting, nudged, owedMark, nkind, wake, guards,
                 futs, dWF, dSR, dSW, dSU, dSG, dPos, dFirst, dRaised, dSent, dFrontW, dClearQ,
                 dClearH, ret, dEarned, ghosts>>

\* Mutant PassTakesWrite only: the write taken for a node that had withdrawn goes back (one
\* fetch-and that lowers WRITER). The upgradable read it was made from is not put back.
DUU(d) ==
  \* The pass holds a write nobody took.
  /\ pc[d] = "duu"
  \* WRITER comes down, the pass's hand is empty, and it goes on to look at HANDOFF.
  /\ writer' = FALSE /\ dHand' = "none" /\ pc' = [pc EXCEPT ![d] = "dhr"]
  \* Nothing else changes.
  /\ UNCHANGED <<readers, upgradable, upgrading, qbits, owedCount, arrivals, waiting, upSlot, nodes,
                 wake, wl, guards, futs, dWF, dSR, dSW, dSU, dSG, dPos, dFirst, dRaised, dSent,
                 dFrontW, dClearQ, dClearH, ret, dEarned, ghosts>>

\* The pass reads the HANDOFF bit (one load): up, it serves the queue by handoff; down, the queue
\* competes.
DHR(d) ==
  \* The pass is past the upgrade slot.
  /\ pc[d] = "dhr"
  \* With HANDOFF up it enters the handoff loop; else it goes on to read the owed count.
  /\ pc' = [pc EXCEPT ![d] = IF handoff THEN "dh" ELSE "dho"]
  \* Nothing else changes.
  /\ UNCHANGED <<lockw, qbits, owedCount, arrivals, waiting, upSlot, nodes, wake, wl, guards, futs,
                 dloc, ret, dEarned, ghosts>>

\* The handoff loop, one round: let go of withdrawn nodes at the front, then try to enter the lock
\* for the front waiter (Lock::take_for_waiter: a compare-and-swap that may pass the HANDOFF bit,
\* because it serves the queue). No waiter, or its request does not fit: the loop ends.
DH(d) ==
  \* The pass is in the handoff loop.
  /\ pc[d] = "dh"
  \* The withdrawn nodes at the front leave the list, and the queue lets go of them.
  /\ waiting' = Pruned /\ LetGo(PrunedOff)
  \* Is a live waiter at the front whose request fits?
  /\ IF Pruned # <<>> /\ Admits(nkind[Head(Pruned)])
       \* Yes: the pass enters the lock for it,
       THEN /\ EnterWord(nkind[Head(Pruned)])
            \* holds that entry, and goes on to grant it.
            /\ dHand' = nkind[Head(Pruned)] /\ pc' = [pc EXCEPT ![d] = "dg"]
            \* A reader entered past a waiting upgrade is recorded.
            /\ readInUpgrade' = (readInUpgrade \/ (nkind[Head(Pruned)] = "read" /\ upgrading))
       \* No: the loop ends, and the pass goes on to read the owed count, with nothing entered.
       ELSE pc' = [pc EXCEPT ![d] = "dho"] /\ UNCHANGED <<lockw, dHand, readInUpgrade>>
  \* Nothing else changes.
  /\ UNCHANGED <<qbits, arrivals, upSlot, nudged, nkind, wake, wl, guards, futs, dWF, dSR, dSW, dSU,
                 dSG, dPos, dFirst, dRaised, dSent, dFrontW, dClearQ, dClearH, ret, dEarned, lost,
                 bargedOwed, overran, turns>>

\* The grant (WakeList::grant: one compare-and-swap on the node). The front node leaves the list.
\* If it still waits it becomes "granted": the entry in the pass's hand is its future's, a sleeping
\* future goes on the wake list, and the queue lets go of the node, settling its owed mark. After
\* a writer the loop ends: nobody else fits. If the future withdrew meanwhile, the node is let go
\* and the entry is undone.
DG(d) ==
  \* The pass holds an entry for the front waiter.
  /\ pc[d] = "dg"
  \* h: that waiter.
  /\ LET h == Head(waiting) IN
       \* Its node leaves the list.
       /\ waiting' = Tail(waiting)
       \* Whatever happens, the queue settles its owed mark and its share of the owed count.
       /\ owedMark' = [owedMark EXCEPT ![h] = FALSE]
       \* (the count)
       /\ owedCount' = owedCount - IF owedMark[h] THEN 1 ELSE 0
       \* Does the node still wait?
       /\ IF phase[h] = "waiting"
            \* Yes: it is granted, and the entry is its future's now.
            THEN /\ phase' = [phase EXCEPT ![h] = "granted"] /\ got' = [got EXCEPT ![h] = @ + 1]
                 \* The pass's hand is empty, and a sleeping future is to be woken.
                 /\ dHand' = "none" /\ wl' = IF pc[h] = "parked" THEN wl \cup {h} ELSE wl
                 \* After a writer the loop ends; after a reader it goes round again.
                 /\ pc' = [pc EXCEPT ![d] = IF dHand = "write" THEN "dho" ELSE "dh"]
            \* No, its future withdrew: the node is free, and the pass goes on to undo the entry.
            ELSE /\ phase' = [phase EXCEPT ![h] = "none"] /\ pc' = [pc EXCEPT ![d] = "dgb"]
                 \* Nobody gets it and nobody is woken.
                 /\ UNCHANGED <<got, dHand, wl>>
  \* Nothing else changes.
  /\ UNCHANGED <<lockw, qbits, arrivals, upSlot, nudged, nkind, wake, held, mode, rdepth, wowner,
                 wdepth, futs, dWF, dSR, dSW, dSU, dSG, dPos, dFirst, dRaised, dSent, dFrontW,
                 dClearQ, dClearH, ret, dEarned, ghosts>>

\* The entry made for a waiter that had withdrawn is undone (RawRwLock::undo: one
\* read-modify-write that takes back exactly what was entered).
DGB(d) ==
  \* The pass holds an entry nobody took.
  /\ pc[d] = "dgb"
  \* A read is one reader fewer; a write lowers WRITER; an upgradable read lowers UPGRADABLE.
  /\ readers' = IF dHand = "read" THEN readers - 1 ELSE readers
  \* (the writer bit)
  /\ writer' = (writer /\ dHand # "write")
  \* (the upgradable bit; UPGRADING is not touched)
  /\ upgradable' = (upgradable /\ dHand # "upread") /\ upgrading' = upgrading
  \* The pass's hand is empty, and the loop goes round again.
  /\ dHand' = "none" /\ pc' = [pc EXCEPT ![d] = "dh"]
  \* Nothing else changes.
  /\ UNCHANGED <<qbits, owedCount, arrivals, waiting, upSlot, nodes, wake, wl, guards, futs, dWF,
                 dSR, dSW, dSU, dSG, dPos, dFirst, dRaised, dSent, dFrontW, dClearQ, dClearH, ret,
                 dEarned, ghosts>>

\* Past the handoff loop, the pass asks whether the queue still serves by handoff
\* (WaitQueue::hands_off: the HANDOFF bit and the owed count, two loads taken as one step here: a
\* waiter that raises the bit also raises DIRTY, so a raise that lands later makes the drain pass
\* again). While HANDOFF is up and a waiter is counted owed nobody is nudged. Otherwise the queue
\* competes: the pass lets go of withdrawn nodes at the front, and if a live waiter is left it goes
\* on to nudge.
DHO(d) ==
  \* The pass is past the handoff loop.
  /\ pc[d] = "dho"
  \* Is HANDOFF up with a waiter counted owed?
  /\ IF handoff /\ owedCount # 0
       \* Yes: no nudges; the pass goes to its end, touching nothing.
       THEN pc' = [pc EXCEPT ![d] = "dend"] /\ UNCHANGED <<waiting, phase, owedMark, owedCount>>
       \* No: the withdrawn nodes at the front leave the list, and the queue lets go of them.
       ELSE /\ waiting' = Pruned /\ LetGo(PrunedOff)
            \* With a live waiter left the pass reads the word for its nudging; else it ends.
            /\ pc' = [pc EXCEPT ![d] = IF Pruned # <<>> THEN "dnb" ELSE "dend"]
  \* Nothing else changes.
  /\ UNCHANGED <<lockw, qbits, arrivals, upSlot, nudged, nkind, wake, wl, guards, futs, dloc, ret,
                 dEarned, ghosts>>

\* The pass reads the state word (one load): that is the picture of the lock its nudging starts
\* from. It also notes whether the front waiter asks to write.
DNB(d) ==
  \* The pass is about to nudge.
  /\ pc[d] = "dnb"
  \* The picture is the lock as it is now.
  /\ dSR' = readers /\ dSW' = writer /\ dSU' = upgradable /\ dSG' = upgrading
  \* It starts at the front of the list, with the first live waiter still to come,
  /\ dPos' = 1 /\ dFirst' = TRUE
  \* no marker raised and no nudge sent.
  /\ dRaised' = FALSE /\ dSent' = FALSE
  \* It notes whether the front waiter asks to write.
  /\ dFrontW' = (nkind[Head(waiting)] = "write")
  \* Next it walks the list.
  /\ pc' = [pc EXCEPT ![d] = "dni"]
  \* Nothing else changes.
  /\ UNCHANGED <<lockw, qbits, owedCount, arrivals, waiting, upSlot, nodes, wake, wl, guards, futs,
                 dWF, dHand, dClearQ, dClearH, ret, dEarned, ghosts>>

\* The place of the next live node at or after place i of the waiting list, or 0 if there is none
\* (List::iter skips withdrawn nodes).
NextLive(i) ==
  \* Is some node at or after place i live?
  IF \E j \in i..Len(waiting) : phase[waiting[j]] # "cancelled"
    \* Then the first such place;
    THEN CHOOSE j \in i..Len(waiting) :
           \* it is live, and nothing live comes before it.
           phase[waiting[j]] # "cancelled" /\ \A k \in i..(j - 1) : phase[waiting[k]] = "cancelled"
    \* else none.
    ELSE 0

\* The waiter at place i fits the pass's picture of the lock.
FitsPicture(i) == AdmitsIn(nkind[waiting[i]], dSR, dSW, dSU, dSG)

\* The pass nudges the waiter at place i: the front waiter if it fits or a release asked for it to
\* be nudged anyway; a later waiter only if it fits, is no writer, and the front was no writer.
NudgesIt(i) ==
  \* The front waiter:
  IF dFirst THEN FitsPicture(i) \/ dWF
  \* a later one.
  ELSE FitsPicture(i) /\ nkind[waiting[i]] # "write" /\ ~dFrontW

\* The walk goes on past the waiter at place i: it fits, it is no writer, and it is the front or
\* the front was no writer.
WalksOn(i) == FitsPicture(i) /\ nkind[waiting[i]] # "write" /\ (dFirst \/ ~dFrontW)

\* Where the pass goes when its walk is over: to lower NUDGE_OUT if it raised it for nothing, else
\* to its end. `raised` and `sent` are the two notes of the walk.
WalkEnds(raised, sent) == IF raised /\ ~sent THEN "dnf" ELSE "dend"

\* The nudge itself (WakeList::nudge: one compare-and-swap on the node), and what follows it. A
\* node that still waits and carries no nudge gets the NUDGED mark, and a sleeping future goes on
\* the wake list. A node that is nudged already is left alone, but a nudge is on its way to it all
\* the same, so the marker must stay up. Then the walk goes on with this waiter entered in the
\* pass's picture, or ends. `i` is the node's place.
NudgeAt(d, i) ==
  \* n: the node at that place.
  LET n == waiting[i]
      \* hit: the nudge reaches it.
      hit == phase[n] = "waiting" /\ ~nudged[n]
      \* sent: a nudge is on its way to some waiter, this one or an earlier one.
      sent == dSent \/ phase[n] = "waiting"
  \* The node is marked, if the nudge reaches it, and the pass notes what is on its way.
  IN /\ nudged' = [nudged EXCEPT ![n] = @ \/ hit] /\ dSent' = sent
     \* A sleeping future is to be woken.
     \* Only a sleeping future (its waker armed) is to be woken; one that spins on its node reads
     \* the mark itself, so the release skips its wake.
     /\ wl' = IF hit /\ pc[n] = "parked" THEN wl \cup {n} ELSE wl
     \* Does the walk go on?
     /\ IF WalksOn(i)
          \* Yes: the waiter is entered in the picture (a read is one more reader, an upgradable
          \* read raises that bit),
          THEN /\ dSR' = IF nkind[n] = "read" THEN dSR + 1 ELSE dSR
               \* (the upgradable bit of the picture)
               /\ dSU' = (dSU \/ nkind[n] = "upread")
               \* and the walk moves past it, no longer at the front.
               /\ dPos' = i + 1 /\ dFirst' = FALSE /\ pc' = [pc EXCEPT ![d] = "dni"]
          \* No: the walk is over.
          ELSE /\ pc' = [pc EXCEPT ![d] = WalkEnds(TRUE, sent)]
               \* The picture is not needed any more.
               /\ UNCHANGED <<dSR, dSU, dPos, dFirst>>

\* One step of the walk. The walk stops at the end of the list, or at a waiter it does not nudge.
\* Otherwise the pass nudges the waiter; before its first nudge it raises NUDGE_OUT (one fetch-or),
\* so that a waiter that polls at once still finds the marker to take back.
DNI(d) ==
  \* The pass walks the list.
  /\ pc[d] = "dni"
  \* i: the place of the next live waiter, if any.
  /\ LET i == NextLive(dPos) IN
       \* Is the walk over: no waiter left, or one the pass does not nudge?
       IF (IF i = 0 THEN TRUE ELSE ~NudgesIt(i))
         \* Yes: the pass goes on, having touched nothing.
         THEN /\ pc' = [pc EXCEPT ![d] = WalkEnds(dRaised, dSent)]
              \* (nothing)
              /\ UNCHANGED <<nudgeOut, nudged, wl, dSR, dSU, dPos, dFirst, dRaised, dSent>>
         \* No. Is this the pass's first nudge?
         ELSE IF ~dRaised
           \* Yes: NUDGE_OUT goes up first, the pass notes that it raised it,
           THEN /\ nudgeOut' = TRUE /\ dRaised' = TRUE
                \* stays at this waiter, and nudges it next.
                /\ dPos' = i /\ pc' = [pc EXCEPT ![d] = "dnt"]
                \* The node is not touched yet.
                /\ UNCHANGED <<nudged, wl, dSR, dSU, dFirst, dSent>>
           \* No, the marker is up already: the pass nudges the waiter.
           ELSE NudgeAt(d, i) /\ UNCHANGED <<nudgeOut, dRaised>>
  \* Nothing else changes.
  /\ UNCHANGED <<lockw, drainOn, dirty, queued, handoff, wakeFront, owedCount, arrivals, waiting,
                 upSlot, phase, owedMark, nkind, wake, guards, futs, dWF, dSW, dSG, dHand, dFrontW,
                 dClearQ, dClearH, ret, dEarned, ghosts>>

\* The pass's first nudge, after it raised NUDGE_OUT.
DNT(d) ==
  \* The pass raised the marker and stands at a waiter, which it nudges.
  /\ pc[d] = "dnt" /\ NudgeAt(d, dPos)
  \* Nothing else changes.
  /\ UNCHANGED <<lockw, qbits, owedCount, arrivals, waiting, upSlot, phase, owedMark, nkind, wake,
                 guards, futs, dWF, dSW, dSG, dRaised, dHand, dFrontW, dClearQ, dClearH, ret,
                 dEarned, ghosts>>

\* The walk raised NUDGE_OUT and no nudge reached a waiter (each one it tried had withdrawn):
\* Nudges::finish lowers the marker again (one fetch-and).
DNF(d) ==
  \* The pass raised the marker for nothing.
  /\ pc[d] = "dnf"
  \* NUDGE_OUT comes down, and the pass goes to its end.
  /\ nudgeOut' = FALSE /\ pc' = [pc EXCEPT ![d] = "dend"]
  \* Nothing else changes.
  /\ UNCHANGED <<lockw, drainOn, dirty, queued, handoff, wakeFront, owedCount, arrivals, waiting,
                 upSlot, nodes, wake, wl, guards, futs, dloc, ret, dEarned, ghosts>>

\* The end of a pass. It lets go of withdrawn nodes at the front and notes whether any waiter is
\* left, in the list or in the upgrade slot: if none, QUEUED may come down. Then the drain reads
\* the owed count (one load): if nobody is counted owed, HANDOFF may come down.
DEnd(d) ==
  \* The pass is done granting and nudging.
  /\ pc[d] = "dend"
  \* The withdrawn nodes at the front leave the list, and the queue lets go of them.
  /\ waiting' = Pruned /\ LetGo(PrunedOff)
  \* QUEUED may come down if no waiter is left, in the list or in the slot.
  /\ dClearQ' = (Pruned = <<>> /\ upSlot = 0)
  \* HANDOFF may come down if nobody is counted owed (the count after this pass's own letting go).
  /\ dClearH' = (owedCount - Cardinality({u \in PrunedOff : owedMark[u]}) = 0)
  \* Next the pass tries to give the role up.
  /\ pc' = [pc EXCEPT ![d] = "dex"]
  \* Nothing else changes.
  /\ UNCHANGED <<lockw, qbits, arrivals, upSlot, nudged, nkind, wake, wl, guards, futs, dWF, dSR,
                 dSW, dSU, dSG, dPos, dFirst, dRaised, dSent, dHand, dFrontW, ret, dEarned, ghosts>>

\* The drain tries to give the role up: one compare-and-swap. It needs DIRTY down, and it needs
\* WAKE_FRONT down unless the pass found nobody queued. Otherwise it passes again. When it
\* succeeds DRAINING comes down together with the bits the pass may clear, the wakers on the wake
\* list are called, and the task goes on with what it was doing.
DEx(d) ==
  \* The pass is over.
  /\ pc[d] = "dex"
  \* Did somebody queue or withdraw meanwhile, or did a release ask while a waiter is queued?
  /\ IF dirty \/ (wakeFront /\ ~dClearQ)
       \* Yes: another pass, with the role, the bits, the wake list and the ghost kept.
       THEN /\ pc' = [pc EXCEPT ![d] = "d0"]
            \* (kept)
            /\ UNCHANGED <<drainOn, queued, handoff, wakeFront, wake, wl, ret, dEarned>>
       \* No: the role is free.
       ELSE /\ drainOn' = FALSE
            \* QUEUED comes down if the pass found no waiter, and with it a release's request that
            \* had no front to nudge.
            /\ queued' = (queued /\ ~dClearQ) /\ wakeFront' = (wakeFront /\ ~dClearQ)
            \* HANDOFF comes down if nobody was counted owed.
            /\ handoff' = (handoff /\ ~dClearH)
            \* Every waker on the wake list is called, and the list is empty.
            /\ wake' = [u \in Tasks |-> wake[u] \/ u \in wl] /\ wl' = {}
            \* The task goes on where it meant to go, and that memory is cleared.
            /\ pc' = [pc EXCEPT ![d] = ret[d]] /\ ret' = [ret EXCEPT ![d] = "idle"]
            \* The ghost: the next drain has its first pass to run.
            /\ dEarned' = TRUE
  \* The pass's locals are forgotten.
  /\ dWF' = FALSE /\ dSR' = 0 /\ dSW' = FALSE /\ dSU' = FALSE /\ dSG' = FALSE /\ dPos' = 0
  \* (the rest of them)
  /\ dFirst' = FALSE /\ dRaised' = FALSE /\ dSent' = FALSE /\ dFrontW' = FALSE
  \* (the drain's two)
  /\ dClearQ' = FALSE /\ dClearH' = FALSE
  \* Nothing else changes.
  /\ UNCHANGED <<lockw, dirty, nudgeOut, owedCount, arrivals, waiting, upSlot, nodes, guards, futs,
                 dHand, ghosts>>

----------------------------------------------------------------------------
\* The specification.

\* What a task holds when the run starts: a guard of its role if it is in Start.
StartsWith(t, k) == t \in Start /\ Role[t] = k

\* The start: the tasks in Start hold their guards, nobody waits.
Init ==
  \* The lock word counts the starting readers,
  /\ readers = IF Kind = "Plain" THEN Cardinality({t \in Tasks : StartsWith(t, "read")})
               \* (the reentrant lock counts owners, and an owner that also writes has no share)
               ELSE Cardinality({Owner[t] : t \in {u \in Tasks : StartsWith(u, "read")}}
                                \ {Owner[t] : t \in {u \in Tasks : StartsWith(u, "write")}})
  \* a starting writer, and a starting upgradable read; no upgrade is under way.
  /\ writer = (\E t \in Tasks : StartsWith(t, "write"))
  \* (the upgradable read)
  /\ upgradable = (\E t \in Tasks : StartsWith(t, "upread")) /\ upgrading = FALSE
  \* Nobody holds the drain role, and nothing changed behind anybody's back.
  /\ drainOn = FALSE /\ dirty = FALSE
  \* No waiter is queued, no nudge is on its way, no handoff is owed, no front is to be nudged.
  /\ queued = FALSE /\ nudgeOut = FALSE /\ handoff = FALSE /\ wakeFront = FALSE
  \* Nobody is counted owed; no arrival, an empty waiting list, an empty upgrade slot.
  /\ owedCount = 0 /\ arrivals = <<>> /\ waiting = <<>> /\ upSlot = 0
  \* No task has a node.
  /\ phase = [t \in Tasks |-> "none"] /\ nkind = [t \in Tasks |-> Role[t]]
  \* No node is nudged or marked owed.
  /\ nudged = [t \in Tasks |-> FALSE] /\ owedMark = [t \in Tasks |-> FALSE]
  \* No waker was called, and no pass has anybody to wake.
  /\ wake = [t \in Tasks |-> FALSE] /\ wl = {}
  \* Nobody has an entry pending.
  /\ got = [t \in Tasks |-> 0]
  \* Only a starting task holds a guard, of its role.
  /\ held = [t \in Tasks |-> IF t \in Start THEN 1 ELSE 0]
  \* (its kind)
  /\ mode = [t \in Tasks |-> IF t \in Start THEN Role[t] ELSE "none"]
  \* The reentrant lock's counts match: each owner's reads,
  /\ rdepth = [o \in Owners |-> IF Kind = "Plain" THEN 0
                               \* (the starting readers of that owner)
                               ELSE Cardinality({t \in Tasks : StartsWith(t, "read") /\ Owner[t] = o})]
  \* the writing owner,
  /\ wowner = IF Kind = "Reentrant" /\ \E t \in Tasks : StartsWith(t, "write")
                \* (the owner of a starting writer)
                THEN Owner[CHOOSE t \in Tasks : StartsWith(t, "write")] ELSE NoOwner
  \* and its write guards.
  /\ wdepth = IF Kind = "Plain" THEN 0 ELSE Cardinality({t \in Tasks : StartsWith(t, "write")})
  \* No future has lost a race or carries anything.
  /\ bypass = [t \in Tasks |-> 0] /\ passOn = [t \in Tasks |-> FALSE] /\ upW = [t \in Tasks |-> FALSE]
  \* Nobody spins on its node.
  /\ spinning = [t \in Tasks |-> FALSE]
  \* No pass is under way.
  /\ dWF = FALSE /\ dSR = 0 /\ dSW = FALSE /\ dSU = FALSE /\ dSG = FALSE /\ dPos = 0
  \* (its other locals)
  /\ dFirst = FALSE /\ dRaised = FALSE /\ dSent = FALSE /\ dHand = "none" /\ dFrontW = FALSE
  \* (the drain's two)
  /\ dClearQ = FALSE /\ dClearH = FALSE
  \* Every task is idle, but a starting holder, which holds.
  /\ pc = [t \in Tasks |-> IF t \in Start THEN "holding" ELSE "idle"]
  \* Every task would go on to idle.
  /\ ret = [t \in Tasks |-> "idle"]
  \* The first drain has its first pass to run, and none overran.
  /\ dEarned = TRUE /\ overran = FALSE
  \* No task has lost a race, nobody barged past an owed handoff or a waiting upgrade.
  /\ lost = [t \in Tasks |-> 0] /\ bargedOwed = FALSE /\ readInUpgrade = FALSE
  \* Nobody has asked yet, but a starting holder, whose hold used a turn if its turns are counted.
  /\ turns = [t \in Tasks |-> IF t \in Start /\ Turns[t] > 0 THEN 1 ELSE 0]

\* The steps a task takes on its own once started: they must happen.
Internal(t) ==
  \* The spin before queueing,
  \/ PreSpin(t) \/ PreSpinOut(t)
  \* the waiter's own steps up to its try,
  \/ Push(t) \/ Publish(t) \/ Park0(t) \/ Repark(t) \/ Poll(t) \/ GrantCons(t) \/ NudgeCons(t)
  \* the spin on its own node after a failed try,
  \/ SpinStart(t) \/ SpinEnd(t)
  \* its two kinds of try, leaving the queue,
  \/ Try(t) \/ Owe(t) \/ Wd1(t) \/ Wd2(t) \/ Gb1(t) \/ Gb2(t) \/ Gb3(t)
  \* asking for a pass after a release,
  \/ Released(t)
  \* the upgrade's own steps,
  \/ UpTry(t) \/ UpPush(t) \/ UpPublish(t) \/ UpPark0(t) \/ UpPoll(t) \/ UpFinish(t)
  \* the steps of a dropped upgrade,
  \/ UpUnlock(t) \/ UpWriteUnlock(t) \/ UpAsk(t) \/ UpWithdrawn(t)
  \* and the drain's steps.
  \/ D0(t) \/ DU(t) \/ DUG(t) \/ DUU(t) \/ DHR(t) \/ DH(t) \/ DG(t) \/ DGB(t) \/ DHO(t)
  \* (the nudging, and the end)
  \/ DNB(t) \/ DNI(t) \/ DNT(t) \/ DNF(t) \/ DEnd(t) \/ DEx(t)

\* Every step the system can take.
Next ==
  \* Some task
  \E t \in Tasks :
    \* acquires, releases, drops its wait, takes one of its own steps,
    \/ Acquire(t) \/ Release(t) \/ Cancel(t) \/ Internal(t)
    \* starts an upgrade, drops one, or (under a planted bug) upgrades from a plain read.
    \/ UpStart(t) \/ UpCancel(t) \/ ReaderUpgrade(t)

\* Fairness: a started operation finishes, a woken future is polled, and a holder eventually drops
\* its guard. Acquiring, upgrading and cancelling are the caller's choices.
Fairness == \A t \in Tasks : WF_vars(Internal(t)) /\ WF_vars(Release(t))

\* Every behaviour: start in Init, take Next steps or stutter, fairly.
Spec == Init /\ [][Next]_vars /\ Fairness

----------------------------------------------------------------------------
\* Invariants.

\* Every variable holds what its comment says.
TypeOK ==
  \* The reader count is a number no larger than the tasks.
  /\ readers \in 0..Cardinality(Tasks)
  \* The flag bits are each a yes or a no.
  /\ {writer, upgradable, upgrading, drainOn, dirty, queued, nudgeOut, handoff, wakeFront}
       \subseteq BOOLEAN
  \* No more waiters are counted owed than there are tasks.
  /\ owedCount \in 0..Cardinality(Tasks)
  \* The arrival stack and the waiting list hold tasks, and the slot a task or nothing.
  /\ Range(arrivals) \subseteq Tasks /\ Range(waiting) \subseteq Tasks /\ upSlot \in Tasks \cup {0}
  \* Every node is in one of the four phases and asks for one of the four things.
  /\ phase \in [Tasks -> {"none", "waiting", "granted", "cancelled"}]
  \* (what it asks for)
  /\ nkind \in [Tasks -> {"read", "write", "upread", "upgrade"}]
  \* Nobody has more than two entries pending, or holds more guards than allowed.
  /\ got \in [Tasks -> 0..2] /\ held \in [Tasks -> 0..MaxDepth]
  \* Every guard is of one of the kinds.
  /\ mode \in [Tasks -> {"none", "read", "write", "upread"}]
  \* The writing owner is an owner token or none.
  /\ wowner \in Owners \cup {NoOwner}
  \* A pass's hand holds one kind of entry or nothing.
  /\ dHand \in {"none", "read", "write", "upread"}
  \* Every task is at one of the known places.
  /\ pc \in [Tasks -> PCs]

\* Only the holder of the drain role is inside the drain, and there is at most one.
\* Rules out two threads sorting the same waiting list at once, which would corrupt it: the list
\* has no lock of its own.
DrainExclusive ==
  \* The DRAINING bit is up exactly while some task is at a drain step.
  /\ drainOn <=> \E t \in Tasks : pc[t] \in DrainPCs
  \* No two tasks are at a drain step.
  /\ \A t, u \in Tasks : pc[t] \in DrainPCs /\ pc[u] \in DrainPCs => t = u

\* Every node is in exactly one place: a node that waits, or that withdrew and is not let go yet,
\* is on the arrival stack, in the waiting list or in the upgrade slot, once.
\* Rules out a waiter that is linked nowhere, which no pass could ever grant or let go: under the
\* bug of E33 a second upgrade's node took the slot from a withdrawn one that was still in it.
NodesLinked ==
  \* No node is linked twice.
  /\ Len(arrivals \o waiting) = Cardinality(Range(arrivals \o waiting))
  \* The slot's node is not in the list, and asks for the upgrade.
  /\ upSlot # 0 => upSlot \notin Range(waiting) /\ nkind[upSlot] = "upgrade"
  \* A node that waits or withdrew is linked.
  /\ \A t \in Tasks : phase[t] \in {"waiting", "cancelled"} =>
       \* (on the stack, in the list, or in the slot)
       t \in Range(arrivals \o waiting) \/ t = upSlot

\* The tasks that hold a guard of kind k.
Holders(k) == {t \in Tasks : held[t] > 0 /\ mode[t] = k}

\* Task t is inside an upgrade it has begun and not ended: between raising UPGRADING and taking
\* the write or letting the upgradable read go. That includes the drain it may run itself when it
\* publishes its node.
InUpgrade(t) ==
  \* At one of the upgrade's own places,
  \/ pc[t] \in {"uptry", "uppush", "upenq", "uppark0", "upparked", "upfin", "upunl"}
  \* or draining on the way to its first look at its node.
  \/ pc[t] \in DrainPCs /\ ret[t] = "uppark0"

\* THE LOCK WORD COUNTS EXACTLY WHAT IS OUT. The reader count, the WRITER bit and the UPGRADABLE bit
\* say exactly what guards exist, what entries are pending for a future, and what a pass holds in
\* its hand; and UPGRADING is up exactly while an upgrade is under way.
\* Rules out, for example: a reader's share that is never given back (no writer ever enters
\* again), or a share given back twice (a writer enters beside a reader).
WordMatchesGuards ==
  \* The readers: the read guards (on the reentrant lock: the owners that read and do not write),
  /\ readers = (IF Kind = "Plain" THEN Cardinality(Holders("read"))
                \* (the reading owners with no write of their own)
                ELSE Cardinality({o \in Owners : rdepth[o] > 0 /\ wowner # o}))
               \* the entries pending for readers, and a read in a pass's hand.
               + SumOf(got, {t \in Tasks : Role[t] = "read"}) + (IF dHand = "read" THEN 1 ELSE 0)
  \* The writer: a write guard, an entry pending for a writer, or a write in a pass's hand.
  /\ writer <=> \/ (IF Kind = "Plain" THEN Holders("write") # {} ELSE wowner # NoOwner)
                \* (pending, or in hand)
                \/ (\E t \in Tasks : Role[t] = "write" /\ got[t] > 0) \/ dHand = "write"
  \* The upgradable read: its guard, an entry pending for it, or one in a pass's hand.
  /\ upgradable <=> \/ Holders("upread") # {}
                    \* (pending, or in hand)
                    \/ (\E t \in Tasks : Role[t] = "upread" /\ got[t] > 0) \/ dHand = "upread"
  \* An upgrade is under way exactly while the holder of the upgradable read is inside one.
  /\ upgrading <=> \E t \in Holders("upread") : InUpgrade(t)

\* EVERY GUARD HELD IS COUNTED IN THE LOCK. A task that holds a guard and is at rest has it in the
\* lock word: a reader is counted, a writer has WRITER up, the upgradable read has UPGRADABLE up.
\* Rules out, for example (E33): task A's dropped upgrade, or the pass that still held A's node,
\* lowers the UPGRADABLE bit that task B raised; B holds the upgradable read, the lock looks free,
\* and a writer enters beside B.
GuardsCounted ==
  \* For every task at rest with a guard:
  \A t \in Tasks : pc[t] = "holding" /\ held[t] > 0 =>
    \* a plain read is counted,
    /\ (Kind = "Plain" /\ mode[t] = "read") => readers > 0
    \* a plain write has WRITER up,
    /\ (Kind = "Plain" /\ mode[t] = "write") => writer
    \* and the upgradable read has UPGRADABLE up.
    /\ mode[t] = "upread" => upgradable

\* MUTUAL EXCLUSION. While a task holds a write guard, no task of another owner holds any guard;
\* and at most one task holds the upgradable read. Lean (exclusive side): mutual_exclusion.
\* Rules out a writer changing the value while a reader of another owner is looking at it.
MutualExclusion ==
  \* A write guard excludes every guard of another owner.
  /\ \A t, u \in Tasks : (held[t] > 0 /\ mode[t] = "write" /\ held[u] > 0) => Owner[t] = Owner[u]
  \* At most one upgradable guard is out.
  /\ Cardinality(Holders("upread")) <= 1

\* REENTRANCY DEPTH. The reentrant lock's counts are exactly the guards out: each owner's reads,
\* the write guards, and a writing owner exactly while a write guard is out.
\* Rules out, for example: an owner drops one of two write guards and the lock is free while it
\* still holds the other.
ReentrancyDepth ==
  \* Only the reentrant lock keeps these counts.
  Kind = "Reentrant" =>
    \* The write guards out are the write guards the tasks hold,
    /\ wdepth = SumOf(held, {t \in Tasks : Role[t] = "write"})
    \* held by the writing owner's tasks only,
    /\ \A t \in Tasks : (Role[t] = "write" /\ held[t] > 0) => Owner[t] = wowner
    \* each owner's reads are the read guards its tasks hold,
    /\ \A o \in Owners : rdepth[o] = SumOf(held, {t \in Tasks : Role[t] = "read" /\ Owner[t] = o})
    \* and there is a writing owner exactly while a write guard is out.
    /\ (wowner = NoOwner <=> wdepth = 0)

\* BOUNDED BYPASS. A waiter, reader or writer, loses the race at most Bound times, and at Bound
\* the HANDOFF bit is up, so nobody barges past it again. Lean: bounded_bypass, at_bound_owed.
\* Rules out, for example: a writer waits while readers keep arriving so that some reader is always
\* inside; every time the writer is nudged it finds a reader and loses, for ever.
BoundedBypass ==
  \* For every task:
  \A t \in Tasks :
    \* it lost at most Bound counted races, and its own count says no more;
    /\ lost[t] <= Bound /\ bypass[t] <= Bound
    \* and once it has lost Bound of them and still waits, barging is shut.
    /\ AtBound(t) => handoff

\* HANDOFF EXCLUSIVE. Nobody enters through the fast path while a handoff is owed.
\* Lean: owed_blocks_barging.
\* Rules out a newcomer slipping in while the lock is kept for a waiter that lost Bound times.
HandoffExclusive == ~bargedOwed

\* UPGRADE HOLDS READERS BACK. No plain reader enters while an upgrade waits.
\* Rules out, for example: an upgrade waits for the readers to leave, and new readers keep entering,
\* so it waits for ever.
UpgradeHoldsBack == ~readInUpgrade

\* NO UPGRADE DEADLOCK. Every reader an upgrade waits for is free to drop its guard: it is never
\* itself asleep on the lock.
\* Rules out, for example: two readers both ask to upgrade; each waits for the other to leave, and
\* neither does.
NoUpgradeDeadlock ==
  \* For every task whose upgrade waits,
  \A t \in Tasks : pc[t] \in {"upparked", "rupwait"} =>
    \* every other task that holds a read guard is not asleep waiting for the lock.
    \A u \in Tasks \ {t} : (held[u] > 0 /\ mode[u] = "read") =>
      pc[u] \notin {"parked", "upparked", "rupwait"}

\* NO SELF-DEADLOCK. No task waits in the queue for a lock its own owner holds a guard of.
\* Rules out, for example: an owner that holds a read guard asks for another read, is queued behind
\* a waiting writer, and so waits on a lock that its own guard keeps the writer out of.
NoSelfDeadlock ==
  \* For every task whose acquire waits,
  \A t \in Tasks : (phase[t] = "waiting" /\ nkind[t] # "upgrade") =>
    \* no task of its owner holds a guard.
    \A u \in Tasks : Owner[u] = Owner[t] => held[u] = 0

\* A sleeping future whose node carries a mark (granted, or nudged) has had its waker called, or
\* is on the wake list of the pass under way. Lean: handed_woken.
MarkedWoken ==
  \* For every task that sleeps with its node granted or nudged:
  \A t \in Tasks : (pc[t] \in {"parked", "upparked"} /\ (phase[t] = "granted" \/ nudged[t])) =>
    \* its waker was called, or the pass under way is about to call it.
    wake[t] \/ t \in wl

\* Every task is between operations and nobody holds the drain role.
Quiescent == ~drainOn /\ \A t \in Tasks : pc[t] \in Rest

\* The live waiters of the list, oldest first.
LiveQ == SelectSeq(waiting, LAMBDA u : phase[u] = "waiting")

\* Left behind with nobody to act, once everything is quiet.
Stranded ==
  \* An arrival no pass took in;
  \/ arrivals # <<>>
  \* or a waiter, in the list or in the slot, with the QUEUED bit down, so that no release would
  \* look for it;
  \/ (LiveQ # <<>> \/ (upSlot # 0 /\ phase[upSlot] = "waiting")) /\ ~queued
  \* or a front waiter whose request fits, and either a handoff is owed (the pass should have
  \* granted it) or its waker was not called (nobody told it to try);
  \/ LiveQ # <<>> /\ Admits(nkind[Head(LiveQ)]) /\ (handoff \/ ~wake[Head(LiveQ)])
  \* or an upgrade asleep although no reader is inside;
  \/ upSlot # 0 /\ phase[upSlot] = "waiting" /\ upgrading /\ readers = 0
  \* or the NUDGE_OUT marker up with no woken waiter left to take it back, so that every later
  \* release would skip its pass;
  \/ nudgeOut /\ ~\E t \in Tasks : pc[t] = "parked" /\ wake[t]
  \* or a release's request to nudge the front still up, with no pass left to act on it.
  \/ wakeFront

\* NO LOST WAKEUP. A mark on a sleeping future comes with a wake; and once every task is between
\* operations, no waiter sleeps while it could proceed. Lean: no_stranded_head, handed_woken.
\* Rules out, for example: the last reader leaves and tells nobody; the writer, or the upgrade,
\* sleeps for ever beside a lock it could have.
NoLostWakeup == MarkedWoken /\ (Quiescent => ~Stranded)

\* CANCELLATION PASSES ON. An idle task holds nothing: a waiter dropped after a grant gave it
\* back, and a dropped upgrade gave the upgradable read back.
\* Rules out, for example: a writer is granted the lock, its future is dropped before it runs, and
\* the lock stays held by nobody.
CancelPassesOn == \A t \in Tasks : pc[t] = "idle" => held[t] = 0 /\ got[t] = 0

\* A DRAIN'S PASSES ARE EARNED, as in Sync.tla. A drain runs one pass. It runs another only if,
\* since that pass began, an operation of another thread landed on it that has something to tell
\* the queue: a waiter that queues, withdraws, or claims the handoff it is owed; or a release that
\* finds a node queued. A release over an empty queue costs it none. The locks are lock-free, not
\* wait-free: there is no fixed number of passes.
\* Rules out a thread kept serving the queue by releases that have nobody to tell.
DrainBounded == ~overran

----------------------------------------------------------------------------
\* Liveness. Checked for the tasks in Watched.

\* Every waiter stops waiting: it is granted the lock, enters itself, or cancels.
\* Rules out a writer that waits for ever although the readers keep leaving, and a reader that
\* waits for ever behind writers.
EveryWaiterServed ==
  \* For every watched task, whenever its acquire waits, later it does not.
  \A t \in Watched : (phase[t] = "waiting" /\ nkind[t] # "upgrade") ~> (phase[t] # "waiting")

\* Every upgrade completes: the upgrader comes to hold the write, or its upgrade is dropped.
\* Rules out an upgrade that waits for ever behind readers that keep arriving.
UpgradeCompletes ==
  \* For every watched task, whenever it has begun an upgrade, later it holds or is idle.
  \A t \in Watched : (pc[t] = "uptry") ~> (pc[t] \in {"holding", "idle"})

\* A waiter operation lands on a drain: task t queues, withdraws, or claims its handoff, and
\* finds the drain role taken, so the holder of the role passes once more for it.
Lands(t) ==
  \* The role is taken,
  /\ drainOn
  \* and the task publishes a node, or tells the queue it withdrew,
  /\ \/ Publish(t) \/ UpPublish(t) \/ Wd2(t) \/ UpWithdrawn(t)
     \* or raises HANDOFF.
     \/ (Owe(t) /\ ~handoff /\ handoff')

\* From some point on, no waiter operation lands on a drain.
WaitersSettle == <>[][\A t \in Tasks : ~Lands(t)]_vars

\* Again and again, each task is outside the drain.
DrainsEnd == \A t \in Tasks : []<>(pc[t] \notin DrainPCs)

\* EVERY DRAIN ENDS, ONCE THE WAITERS SETTLE, as in Sync.tla. The condition is needed: each waiter
\* that queues or withdraws while a drain runs costs its holder one more pass (DrainBounded).
\* Rules out a pass that never finishes its walk, or a drain that goes on passing when nothing asks
\* it to.
DrainsEndWhenWaitersSettle == WaitersSettle => DrainsEnd

\* Owner o holds the lock in some way.
OwnerHolds(o) == \E t \in Tasks : Owner[t] = o /\ held[t] > 0

\* No owner keeps the lock forever.
OwnersLetGo == \A o \in Owners : []<>(~OwnerHolds(o))

\* If no owner holds the lock forever, every waiter is served.
ServedIfOwnersLetGo == OwnersLetGo => EveryWaiterServed

====
