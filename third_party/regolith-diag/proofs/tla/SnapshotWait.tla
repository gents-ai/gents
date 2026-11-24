---- MODULE SnapshotWait ----
\* ===========================================================================
\* THE STORY, for a reader who has never seen the code
\* ===========================================================================
\* A reader of the database can take a "snapshot": a promise that it keeps
\* seeing the data as it was at one moment. While a snapshot is alive the
\* database must keep old versions around for it. Each live snapshot holds a
\* "pin" in a table called the snapshot registry.
\*
\* Before a program shuts the database down tidily it may want to wait until
\* every snapshot is gone. `Db::wait_for_snapshots()` (src/lib.rs) hands back
\* a `SnapshotWait` at once, and that handle's `wait(timeout)`
\* (src/snapshot_wait.rs) does the waiting: it comes back with the number of
\* pins still held, 0 when the wait succeeded. "The call" below is that wait.
\* The work is done by
\* `SnapshotRegistry::wait_until_drained` (src/engine/snapshot_registry.rs).
\*
\* There is no lock. The waiter and a thread that releases a pin talk
\* through three shared things:
\*   - the "taken" bits: one per pin still held;
\*   - a "watched" mark a waiter leaves beside those bits, in the same
\*     atomic step in which it reads them;
\*   - a notification with a generation number (a `Notify`, src/sync/notify.rs):
\*     a waiter first writes down the generation, later goes to sleep only if
\*     the generation is still the one it wrote down, and a release that
\*     found the mark raises the generation and wakes sleepers.
\*
\* Tiny example of the good case. One pin is held. The waiter writes down
\* generation 0, marks "watched" and sees the pin. The owner of the pin
\* releases it: it clears the bit and, in the same step, sees the mark, so it
\* raises the generation to 1 and wakes the waiter. Whether the waiter was
\* already asleep (it is woken) or had not gone to sleep yet (it sees 1, not
\* 0, and does not sleep), it looks again, finds nothing held, and returns 0.
\*
\* WHAT CAN GO WRONG (E15), and what this model checks cannot happen.
\*   1. A lost wake. The waiter goes to sleep just as the last pin is
\*      released, nobody wakes it, and it sleeps for the whole timeout (for
\*      ever, where the platform has no clock) although nothing is held.
\*      NoLostWake.
\*   2. Waiting where nobody can answer. A wasm build without threads has
\*      exactly one thread. If that thread went to sleep waiting for a pin
\*      to be released, the release could never run: the only thread there
\*      is, is asleep. So on such a target the call must not wait at all: it
\*      returns the number of pins still held at once. NeverWaitsAlone and
\*      ReportsStillPinned.
\*
\* PROVED FOR EVERY SIZE in Lean, proofs/lean/Regolith/SnapshotWait.lean:
\*   step_inv, reachable_inv   the waiting protocol keeps its invariant
\*   no_lost_wake              a sleeping, unwoken waiter always has a pin
\*                             still held or a release still on its way to
\*                             wake it (NoLostWake here, for any number of
\*                             pins and calls)
\*   one_thread_never_waits    with one thread the call returns the count
\*                             of pins held, in one step, and is never
\*                             asleep (NeverWaitsAlone, ReportsStillPinned)
\*   future_after_watch_loses_wake, read_then_mark_loses_wake
\*                             the two RED cases, as counterexamples
\*
\* THE ENGINE.
\*   SnapshotRegistry::wait_until_drained_on   src/engine/snapshot_registry.rs
\*     Without threads (`PLATFORM_THREADS` is false, src/env/mod.rs): return
\*     `live_count()` at once. With threads: `wait_drained`, parking the
\*     thread between rounds until the deadline.
\*   SnapshotRegistry::wait_drained            the rounds: count this waiter
\*     in `watchers`; create the notification future; `watch` (mark every
\*     chunk and read it); if something is taken, wait on the future.
\*   SnapshotRegistry::until_drained           the same rounds for a task that
\*     awaits the `SnapshotWait` instead of blocking: where the thread would
\*     park, the task is suspended, and the wake resumes it. "parked" below
\*     stands for both.
\*   Chunk::watch, Chunk::unpin                src/engine/snapshot_registry/chain.rs
\*     One read-modify-write each on the chunk's `taken` word: `watch` sets
\*     the WATCHED bit and reads the entry bits; `unpin` clears an entry bit
\*     and reads the WATCHED bit.
\*   SnapshotRegistry::freed                   a release that saw the mark,
\*     with a waiter counted, calls `Notify::notify_waiters`.
\*
\* THE DEFECTS, one per mutant value.
\*   "WaitAnyway"        the call ignores that the target has one thread and
\*                       waits like everywhere else.
\*   "FutureAfterWatch"  the waiter writes the generation down after it
\*                       looked at the bits instead of before.
\*   "ReadThenMark"      the waiter reads the bits first and leaves its mark
\*                       in a second step.
\*
\* WHAT THE MODEL LEAVES OUT, and why that loses nothing.
\*   - New snapshots taken during the wait: they only add pins, and a pin
\*     that appears after a look is seen by the next look.
\*   - Several pins on one registry entry: only the release of an entry's
\*     last pin clears its bit, and that is the release modelled here.
\*   - Several chunks: each has its own word and mark, read one after the
\*     other; one word is the case where the race is tightest.
\*   - Several waiters at once: `notify_waiters` wakes them all, and each
\*     runs the rounds below on its own.
\*   - The gap between registering the waker and parking the thread: a wake
\*     in that gap sets the thread's park token, so the park returns at
\*     once. Here "check the generation and sleep" is one step. The Notify
\*     itself is checked in SyncNotify.tla and by loom
\*     (tests/loom_snapshots.rs, a_drain_waiter_is_always_woken).
\*
\* CONFIGURATIONS.
\*   MC_SnapshotWait_Green                  threads: no wake is lost
\*   MC_SnapshotWait_Green_NoThreads        one thread: the call never waits
\*                                          and reports what is still pinned
\*   MC_SnapshotWait_Red_WaitAnyway         NeverWaitsAlone fails
\*   MC_SnapshotWait_Red_FutureAfterWatch   NoLostWake fails
\*   MC_SnapshotWait_Red_ReadThenMark       NoLostWake fails

\* Borrow the standard definitions of numbers and of finite sets (for counting pins).
EXTENDS Naturals, FiniteSets

\* The knobs of a run. Each configuration file (MC_SnapshotWait_*.cfg) gives them values.
CONSTANTS
  \* The snapshot pins held when the run starts: a set of names. Each is released at most once.
  Pins,
  \* TRUE when the target has other threads; FALSE for a target with exactly one thread
  \* (`PLATFORM_THREADS`, src/env/mod.rs).
  Threads,
  \* How many times the program calls `wait_for_snapshots` in one run. It bounds the run.
  MaxCalls,
  \* "none" for the design, or the name of one planted bug.
  Mutant

\* TLC refuses a configuration whose thread switch is not a yes or a no,
ASSUME Threads \in BOOLEAN
\* or that allows no call at all,
ASSUME MaxCalls \in Nat \ {0}
\* or that names no known bug.
ASSUME Mutant \in {"none", "WaitAnyway", "FutureAfterWatch", "ReadThenMark"}

\* The state: everything that changes from one step to the next. Each line says what the variable
\* stands for in the real program.
VARIABLES
  \* The pins still held: the entry bits of the chunk's `taken` word (Chunk::taken, an AtomicU64,
  \* src/engine/snapshot_registry/chain.rs). A set of pins.
  taken,
  \* TRUE once a waiter left its mark: the WATCHED bit of the same word. Never cleared.
  watched,
  \* How many waiters are inside the wait: SnapshotRegistry::watchers (an AtomicUsize). A number.
  watchers,
  \* The notification's generation: how many times `notify_waiters` ran (Notify, src/sync/notify.rs).
  gen,
  \* Where the waiter is: "out" (not in a call), "counted" (it added itself to `watchers`), "armed"
  \* (it wrote the generation down), "late" (only under a bug: it looked, and has a step to make
  \* up), "looked" (it marked and read the bits and saw a pin), "parked" (asleep).
  wpc,
  \* The generation the waiter wrote down: what its notification future compares against.
  wgen,
  \* TRUE when a release has woken the sleeping waiter and it has not run again yet.
  woken,
  \* TRUE once a call has returned.
  answered,
  \* What the last call returned: the number of pins it reported as still held. A number.
  wret,
  \* How many calls have been made. It bounds the run.
  calls,
  \* A ghost (bookkeeping the program does not have; it only lets a promise be stated): the pins
  \* released since the last call began.
  since,
  \* For each pin, where its release is: "held" (not released), "freed" (its bit is cleared and
  \* the release has yet to decide whether to notify), or "done".
  rpc,
  \* For each pin, whether its release saw the watched mark when it cleared the bit.
  rsaw

\* Every variable, so a step that changes none of them is a stutter.
vars == <<taken, watched, watchers, gen, wpc, wgen, woken, answered, wret, calls, since, rpc, rsaw>>

\* The waiter's own variables, grouped so steps that leave them alone say so briefly.
waiter == <<wpc, wgen, woken, answered, wret, calls>>
\* The releases' variables.
releases == <<rpc, rsaw>>

\* On a target with one thread, a release runs only while the waiter is not in the middle of
\* its call: the one thread does one thing at a time.
MayRelease == Threads \/ wpc = "out"

----------------------------------------------------------------------------
\* The waiter: one call of wait_for_snapshots after another.

\* The start: every pin held, nobody waiting, nothing marked.
Init ==
  \* Every pin is held.
  /\ taken = Pins
  \* No waiter has left a mark.
  /\ watched = FALSE
  \* No waiter is inside the wait.
  /\ watchers = 0
  \* The notification has never fired.
  /\ gen = 0
  \* The waiter is not in a call.
  /\ wpc = "out"
  \* It has written no generation down.
  /\ wgen = 0
  \* Nobody was woken.
  /\ woken = FALSE
  \* No call has returned.
  /\ answered = FALSE
  \* Nothing was reported.
  /\ wret = 0
  \* No call was made.
  /\ calls = 0
  \* Nothing was released.
  /\ since = {}
  \* No release has begun.
  /\ rpc = [p \in Pins |-> "held"]
  \* No release has seen anything.
  /\ rsaw = [p \in Pins |-> FALSE]

\* The program calls wait_for_snapshots. On a target with one thread the design answers at
\* once with the number of pins held and never waits. Everywhere else, and under the bug
\* WaitAnyway, the waiter counts itself in and starts its rounds.
Call ==
  \* When: the waiter is not in a call,
  /\ wpc = "out"
  \* a call is left in the budget,
  /\ calls < MaxCalls
  \* and, with one thread, no release is half done (the thread finishes one thing first).
  /\ Threads \/ \A p \in Pins : rpc[p] # "freed"
  \* One more call.
  /\ calls' = calls + 1
  \* The ghost starts afresh for this call.
  /\ since' = {}
  \* One thread, and no bug?
  /\ IF ~Threads /\ Mutant # "WaitAnyway"
       \* The call returns at once ...
       THEN /\ answered' = TRUE
            \* ... with the number of pins still held.
            /\ wret' = Cardinality(taken)
            \* It never entered the wait.
            /\ UNCHANGED <<wpc, watchers>>
       \* Otherwise the waiter counts itself in ...
       ELSE /\ watchers' = watchers + 1
            \* ... and starts a round.
            /\ wpc' = "counted"
            \* Nothing is returned yet.
            /\ UNCHANGED <<answered, wret>>
  \* Nothing else changes.
  /\ UNCHANGED <<taken, watched, gen, wgen, woken, releases>>

\* A round begins: the waiter creates its notification future, which writes down the current
\* generation. The bug FutureAfterWatch leaves this for after the look.
Arm ==
  \* When: the waiter is counted in and at the top of a round.
  /\ wpc = "counted"
  \* It writes the generation down (the bug does not, yet).
  /\ wgen' = IF Mutant = "FutureAfterWatch" THEN wgen ELSE gen
  \* The look comes next.
  /\ wpc' = "armed"
  \* Nothing else changes.
  /\ UNCHANGED <<taken, watched, watchers, gen, woken, answered, wret, calls, since, releases>>

\* The look (Chunk::watch): in one atomic step the waiter leaves its mark and reads the bits.
\* Nothing held: the call returns 0. Something held: it goes on to wait. The bug ReadThenMark
\* reads here and marks in a later step.
Watch ==
  \* When: the waiter has started its round.
  /\ wpc = "armed"
  \* The mark is left (the bug ReadThenMark leaves it later).
  /\ watched' = (watched \/ Mutant # "ReadThenMark")
  \* Is anything held?
  /\ IF taken = {}
       \* Nothing: the call returns ...
       THEN /\ answered' = TRUE
            \* ... 0, the wait succeeded.
            /\ wret' = 0
            \* The waiter counts itself out,
            /\ watchers' = watchers - 1
            \* and is no longer in a call.
            /\ wpc' = "out"
       \* Something is held: the wait comes next (a bug first has a step to make up).
       ELSE /\ wpc' = IF Mutant \in {"FutureAfterWatch", "ReadThenMark"} THEN "late" ELSE "looked"
            \* Nothing is returned, and the waiter stays counted in.
            /\ UNCHANGED <<answered, wret, watchers>>
  \* Nothing else changes.
  /\ UNCHANGED <<taken, gen, wgen, woken, calls, since, releases>>

\* Only under a bug: the step the design does earlier. FutureAfterWatch writes the generation
\* down now, after the look; ReadThenMark leaves its mark now, after the read.
Late ==
  \* When: a bug left a step to make up.
  /\ wpc = "late"
  \* FutureAfterWatch: the generation is written down only now.
  /\ wgen' = IF Mutant = "FutureAfterWatch" THEN gen ELSE wgen
  \* ReadThenMark: the mark is left only now. (Under the other bug it is there already.)
  /\ watched' = TRUE
  \* The wait comes next.
  /\ wpc' = "looked"
  \* Nothing else changes.
  /\ UNCHANGED <<taken, watchers, gen, woken, answered, wret, calls, since, releases>>

\* The wait: the waiter polls its future. If the generation moved since it was written down,
\* the future is ready and a new round starts. If not, the thread goes to sleep.
Poll ==
  \* When: the waiter looked and saw a pin held.
  /\ wpc = "looked"
  \* Did the generation move?
  /\ IF gen # wgen
       \* Yes: no sleep, a new round.
       THEN wpc' = "counted"
       \* No: the thread parks.
       ELSE wpc' = "parked"
  \* Nothing else changes.
  /\ UNCHANGED <<taken, watched, watchers, gen, wgen, woken, answered, wret, calls, since, releases>>

\* A woken waiter runs again and starts a new round.
WakeUp ==
  \* When: the waiter is asleep
  /\ wpc = "parked"
  \* and a release has woken it.
  /\ woken
  \* The wake is used up.
  /\ woken' = FALSE
  \* A new round.
  /\ wpc' = "counted"
  \* Nothing else changes.
  /\ UNCHANGED <<taken, watched, watchers, gen, wgen, answered, wret, calls, since, releases>>

\* The deadline passes while the waiter sleeps: it looks once more and the call returns what is
\* still held (0 if the pins went meanwhile).
TimeOut ==
  \* When: the waiter is asleep.
  /\ wpc = "parked"
  \* Its last look leaves the mark, as every look does.
  /\ watched' = TRUE
  \* The call returns ...
  /\ answered' = TRUE
  \* ... the number of pins still held.
  /\ wret' = Cardinality(taken)
  \* The waiter counts itself out,
  /\ watchers' = watchers - 1
  \* forgets any wake,
  /\ woken' = FALSE
  \* and is no longer in a call.
  /\ wpc' = "out"
  \* Nothing else changes.
  /\ UNCHANGED <<taken, gen, wgen, calls, since, releases>>

----------------------------------------------------------------------------
\* The releases: the owner of a pin drops its snapshot.

\* The release clears the pin's bit and, in the same atomic step, reads the watched mark
\* (Chunk::unpin).
Unpin(p) ==
  \* When: the pin is held,
  /\ rpc[p] = "held"
  \* and this thread may run (see MayRelease).
  /\ MayRelease
  \* The bit is cleared.
  /\ taken' = taken \ {p}
  \* The release remembers whether the mark was there.
  /\ rsaw' = [rsaw EXCEPT ![p] = watched]
  \* It decides about the notification next.
  /\ rpc' = [rpc EXCEPT ![p] = "freed"]
  \* The ghost notes the release.
  /\ since' = since \cup {p}
  \* Nothing else changes.
  /\ UNCHANGED <<watched, watchers, gen, waiter>>

\* The release finishes (SnapshotRegistry::freed): if it saw the mark and a waiter is counted in,
\* it raises the generation and wakes a sleeping waiter.
Tell(p) ==
  \* When: the release cleared its bit,
  /\ rpc[p] = "freed"
  \* and this thread may run.
  /\ MayRelease
  \* Did it see the mark, and is a waiter counted in?
  /\ IF rsaw[p] /\ watchers > 0
       \* Yes: the generation goes up,
       THEN /\ gen' = gen + 1
            \* and a sleeping waiter is woken.
            /\ woken' = (woken \/ wpc = "parked")
       \* No: it tells nobody.
       ELSE UNCHANGED <<gen, woken>>
  \* The release is over.
  /\ rpc' = [rpc EXCEPT ![p] = "done"]
  \* Nothing else changes.
  /\ UNCHANGED <<taken, watched, watchers, wpc, wgen, answered, wret, calls, since, rsaw>>

\* Every step the system can take.
Next ==
  \* The waiter takes one of its steps,
  \/ Call \/ Arm \/ Watch \/ Late \/ Poll \/ WakeUp \/ TimeOut
  \* or a release takes one of its two.
  \/ \E p \in Pins : Unpin(p) \/ Tell(p)

\* Every behaviour: start in Init, then take Next steps or stutter.
Spec == Init /\ [][Next]_vars

----------------------------------------------------------------------------
\* Invariants.

\* Every variable holds what its comment says.
TypeOK ==
  \* The held pins are pins.
  /\ taken \subseteq Pins
  \* The mark is there or not.
  /\ watched \in BOOLEAN
  \* At most the one waiter is counted in.
  /\ watchers \in 0..1
  \* The generation rises once per release at most.
  /\ gen \in 0..Cardinality(Pins)
  \* The waiter is at one of its places.
  /\ wpc \in {"out", "counted", "armed", "late", "looked", "parked"}
  \* The generation written down is one that existed.
  /\ wgen \in 0..Cardinality(Pins)
  \* A wake is pending or not.
  /\ woken \in BOOLEAN
  \* A call has returned or not.
  /\ answered \in BOOLEAN
  \* A reported count is at most the number of pins.
  /\ wret \in 0..Cardinality(Pins)
  \* The calls stay within their budget.
  /\ calls \in 0..MaxCalls
  \* The ghost holds pins.
  /\ since \subseteq Pins
  \* Each release is at one of its places.
  /\ rpc \in [Pins -> {"held", "freed", "done"}]
  \* Each release saw the mark or did not.
  /\ rsaw \in [Pins -> BOOLEAN]

\* THE HEADLINE. The waiter is never asleep and unwoken while nothing is held and every release
\* has finished: the release that drained the last pin woke it (or it never went to sleep).
\* Example ruled out: one pin; the waiter looks and sees it; the pin is released and its
\* notification fires before the waiter wrote the generation down; the waiter then writes down
\* the new generation, sees no change and sleeps, with nothing held and nobody left to wake it.
\* The program needs this because a shutdown that waits for snapshots would otherwise hang for
\* its whole timeout after the last reader has gone. Lean: no_lost_wake.
NoLostWake ==
  \* Not all four at once: asleep,
  ~ /\ wpc = "parked"
    \* not woken,
    /\ ~woken
    \* nothing held,
    /\ taken = {}
    \* and every release finished.
    /\ \A p \in Pins : rpc[p] = "done"

\* On a target with one thread the waiter is never asleep. Example ruled out: the one thread
\* holds a snapshot and calls wait_for_snapshots; the call parks the thread; the only code that
\* could release the snapshot is on that same, sleeping thread. Lean: one_thread_never_waits.
NeverWaitsAlone ==
  \* With one thread, the waiter is not parked.
  ~Threads => wpc # "parked"

\* On a target with one thread, a call reports exactly the pins held when it was made: what is
\* held now plus what was released since. Example ruled out: a call that says 0, "all clear",
\* while a snapshot is still alive, or one that says 2 when one is. Lean: one_thread_never_waits.
ReportsStillPinned ==
  \* With one thread, once a call has returned and no other is under way,
  (~Threads /\ answered /\ wpc = "out") =>
    \* its answer counts the pins held now and those released after it began.
    wret = Cardinality(taken) + Cardinality(since)

\* A call that returned 0 found nothing held, and nothing is held since (the model takes no new
\* snapshot). Example ruled out: "0, the wait succeeded" while a pin is still held.
ZeroMeansDrained ==
  \* Once a call has returned 0 and no other is under way,
  (answered /\ wret = 0 /\ wpc = "out") =>
    \* nothing is held.
    taken = {}

====
