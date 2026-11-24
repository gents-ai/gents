---- MODULE SyncOnce ----
\* ===========================================================================
\* THE STORY
\* ===========================================================================
\* A OnceCell is a box that is filled at most once and never changes after
\* that (src/sync/once_cell.rs; Lazy, src/sync/lazy.rs, is a OnceCell that
\* knows how to fill itself). Many tasks may try to fill it at the same
\* time. Whatever happens, they must all end up reading the same value.
\*
\* There are three ways to fill it: `set(v)` (here is the value), a racing
\* build (everybody who finds it empty builds a value, one wins), and an
\* async initializer (one task runs a future that produces the value while
\* the others wait for it).
\*
\* The cell is two things. The value lives behind one pointer, empty at
\* first: publishing is one compare-and-swap of that pointer from "empty",
\* and reading is one load, so nobody ever sees a half-written value. And
\* the async initializers take turns through a semaphore with one permit:
\* whoever holds the permit runs its future, the others queue for it.
\*
\* Tiny example of what can go wrong. Task 1 holds the turn and runs its
\* future; task 2 queues for the turn. Task 1's future is dropped half way.
\* If the turn were not given back, task 2 would wait for ever for a value
\* nobody is producing. So the turn is a guard that returns its permit when
\* it is dropped, whatever the reason, and that release wakes task 2.
\*
\* Second example: task 1 is still running its future, task 2 waits behind
\* it, and task 3 calls `set(7)`. The cell now has its value, but task 2
\* waits for a turn that task 1 may keep for a long time. So whoever
\* publishes also opens the semaphore wide (it adds far more permits than
\* there can be waiters), which lets every queued task through to find the
\* value.
\*
\* Third example: task 1 publishes value 1 and task 3 reads it. If task 2's
\* `set` could then simply store value 2, task 3 holds a value the cell no
\* longer has. So publishing is a compare-and-swap that succeeds only on a
\* cell that holds no value yet, and a task whose compare-and-swap loses
\* drops its own value and returns the one that won.
\*
\* What this model shows, for three tasks in every interleaving: at most one
\* value is ever published, everybody who got a value got that one, the
\* permits add up, and no waiter is left asleep.
\*
\* PROVED FOR EVERY SIZE in Lean: nothing specific to OnceCell. The publish
\* is one compare-and-swap; the turn is the semaphore of Sync.tla, whose
\* laws proofs/lean/Regolith/Sync.lean proves for one permit.
\*
\* CHECKED ON THE REAL CODE by loom (tests/loom_sync.rs):
\*   once_cell_initializers_agree_on_one_value        PublishedOnce,
\*                                                    ReadersAgree
\*   a_cancelled_once_cell_initializer_hands_the_turn_on
\*                                                    NoLostWakeup (the
\*                                                    turn comes back)
\*   a_set_wakes_a_waiter_queued_behind_a_running_initializer
\*                                                    NoLostWakeup (the
\*                                                    publisher opens)
\*
\* THE CODE MODELLED (src/sync/once_cell.rs), one step per instruction of
\* the cell's own.
\*   - OnceCell::get: one load of the pointer.
\*   - OnceCell::try_publish: one compare-and-swap of the pointer from
\*     empty; on success `self.init.release(OPEN)`, on failure the value
\*     that won comes back.
\*   - OnceCell::set: a load (a cell that already has a value refuses at
\*     once), then try_publish.
\*   - OnceCell::get_or_init_racy (and Lazy::force): a load, then build,
\*     then try_publish; a loser returns the winner's value.
\*   - OnceCell::get_or_try_init (get_or_init is the same with a future
\*     that cannot fail): a load; then `self.init.acquire(1).await`, the
\*     turn; then a second load, because the value may have been published
\*     while the task waited; then the future; then try_publish. The turn
\*     is a guard: it gives its permit back when the call ends, with a
\*     value, with an error, or because the future was dropped.
\*
\* THE SEMAPHORE, as the cell uses it. `init` is the RawSemaphore that
\* Sync.tla checks instruction by instruction (its queue, its drain role,
\* its nudges). Here it is what those instructions add up to: a count of
\* free permits; a queue of waiters, oldest first; a release that puts
\* permits back and nudges the waiters the free permits could serve, unless
\* a nudge is already on its way; a nudged waiter that takes a permit if one
\* is free (and whose leaving the queue nudges those behind it), or else
\* goes back to sleep. Bounded bypass and the owed handoff are Sync.tla's:
\* each task here runs one operation, so no waiter can be passed over more
\* than twice.
\*
\* WHAT THE MODEL LEAVES OUT. Each task runs one operation; task t's value
\* is t, so values are told apart by who built them. A task that queues for
\* the turn does so in one step. Memory ordering is loom's (plan 7.1).
\*
\* CONFIGURATIONS.
\*   MC_SyncOnce_Green              three tasks, each one of get_or_init,
\*                                  get_or_try_init, set or
\*                                  get_or_init_racy, with failures and
\*                                  drops: one value, every reader agrees,
\*                                  every waiter finishes.
\*   MC_SyncOnce_Red_PlainStore     publish with a store, not a
\*                                  compare-and-swap: PublishedOnce.
\*   MC_SyncOnce_Red_LoserKeepsOwn  a task whose compare-and-swap lost
\*                                  returns the value it built itself:
\*                                  ReadersAgree.
\*   MC_SyncOnce_Red_CancelNoReset  a dropped initializer keeps the turn:
\*                                  NoLostWakeup.
\*   MC_SyncOnce_Red_PublishNoOpen  a publisher does not open the
\*                                  semaphore: NoLostWakeup.

\* Borrow the standard TLA+ books: counting numbers, lists, and sizes of finite sets.
EXTENDS Naturals, Sequences, FiniteSets

\* The fixed inputs a configuration chooses.
CONSTANTS
  \* The tasks that use the cell, named by positive numbers. The value task t would publish is the
  \* number t, so values tell who built them.
  Tasks,
  \* "none" for the code as built, or the planted bug: "PlainStore", "LoserKeepsOwn",
  \* "CancelNoReset" or "PublishNoOpen".
  Mutant

\* Tasks are positive numbers (0 means "no task").
ASSUME Tasks \subseteq Nat \ {0}
\* The mutant is "none" or one of the four planted bugs.
ASSUME Mutant \in {"none", "PlainStore", "LoserKeepsOwn", "CancelNoReset", "PublishNoOpen"}

\* The value "no value": the empty pointer.
None == 0

\* The permits a publisher adds (OPEN in src/sync/once_cell.rs): more than the tasks that can ever
\* be queued, so every one of them gets through.
Open == Cardinality(Tasks) + 1

\* The state: everything that changes from one step to the next. Each line says what the variable
\* stands for in the real program.
VARIABLES
  \* The published value, or None while the cell is empty (OnceCell::value, an AtomicPtr: null
  \* until one compare-and-swap publishes a value, never changed after).
  val,
  \* The free permits of the turn semaphore (OnceCell::init, a RawSemaphore that starts with one:
  \* the count in its state word).
  permits,
  \* The tasks queued for the turn, oldest first (the semaphore's waiting list).
  waiting,
  \* Whether each queued task was nudged and has not been polled since: a wake is on its way to it
  \* (the NUDGED bit of its waiter node).
  nudged,
  \* Whether each task holds the turn: one permit, in a guard that gives it back when dropped (the
  \* RawPermit `_turn` in get_or_try_init).
  turn,
  \* The value each finished task returned to its caller, or None.
  result,
  \* Where each task is: its "program counter" (see PCs below).
  pc,
  \* Model bookkeeping (a "ghost"): every value ever published, in order. The promise PublishedOnce
  \* reads it.
  published

\* Every variable, so a step that changes none of them is a stutter.
vars == <<val, permits, waiting, nudged, turn, result, pc, published>>

\* The places a task can be. "idle": not started. "setcas": `set` found the cell empty and is about
\* to publish. "building": a racing builder is building its value. "acquire": get_or_try_init found
\* the cell empty and asks for the turn. "parked": it is queued for the turn. "look": it has the
\* turn and looks at the cell again. "running": it has the turn and its future runs. "open": it
\* published and is about to open the semaphore. "giveback": it is about to drop its turn. "done".
PCs == {"idle", "setcas", "building", "acquire", "parked", "look", "running", "open", "giveback",
        "done"}

----------------------------------------------------------------------------
\* The semaphore's pass, taken whole (RawSemaphore::pass, checked step by step in Sync.tla).

\* The elements of a sequence, as a set.
Range(s) == {s[i] : i \in 1..Len(s)}

\* Sequence s without element x.
Remove(s, x) == SelectSeq(s, LAMBDA y : y # x)

\* The tasks a pass nudges when the queue is q and p permits are free: the oldest p of them (each
\* asks for one permit), and at least the front one when `front` says so (a release asks for the
\* front to be nudged whatever is free).
Reached(q, p, front) ==
  \* The first p tasks of the queue,
  {q[i] : i \in 1..(IF p < Len(q) THEN p ELSE Len(q))}
    \* and the front one when asked for and there is one.
    \cup (IF front /\ q # <<>> THEN {q[1]} ELSE {})

\* The nudge marks after a pass over queue q with p permits free.
Passed(q, p, front) == [u \in Tasks |-> nudged[u] \/ u \in Reached(q, p, front)]

\* A nudge is on its way to some queued task (the NUDGE_OUT marker of the state word): a release
\* then runs no pass, since a wake is already coming.
NudgeOut == \E u \in Range(waiting) : nudged[u]

\* Permits go back to the semaphore (RawSemaphore::release): the count goes up by n, and unless a
\* nudge is already on its way, a pass nudges the waiters the free permits could serve.
ReleaseTo(n) ==
  \* The permits are free.
  /\ permits' = permits + n
  \* A pass runs unless a nudge is out; it nudges the front in any case.
  /\ nudged' = IF NudgeOut THEN nudged ELSE Passed(waiting, permits + n, TRUE)

----------------------------------------------------------------------------
\* Publishing: OnceCell::try_publish.

\* Task t publishes its value: the pointer goes from empty to the value. (Mutant PlainStore: a
\* store, whatever was there.) It returns its own value and goes on to open the semaphore.
Publish(t) ==
  \* The cell holds this task's value.
  /\ val' = t
  \* One more publication is recorded.
  /\ published' = Append(published, t)
  \* The task returns its own value.
  /\ result' = [result EXCEPT ![t] = t]
  \* Next it opens the semaphore. (Mutant PublishNoOpen: it skips that.)
  /\ pc' = [pc EXCEPT ![t] = IF Mutant = "PublishNoOpen"
                               \* (the bug: straight on to what comes after opening)
                               THEN (IF turn[t] THEN "giveback" ELSE "done")
                               \* (as built)
                               ELSE "open"]

\* Task t's compare-and-swap found a value already there: it drops its own and returns the one
\* that won. (Mutant LoserKeepsOwn: it returns its own.)
Lose(t) ==
  \* What it returns to its caller.
  /\ result' = [result EXCEPT ![t] = IF Mutant = "LoserKeepsOwn" THEN t ELSE val]
  \* A task that holds the turn gives it back next; any other is done.
  /\ pc' = [pc EXCEPT ![t] = IF turn[t] THEN "giveback" ELSE "done"]
  \* The cell is untouched.
  /\ UNCHANGED <<val, published>>

\* The compare-and-swap of try_publish: it publishes if the cell is empty (mutant PlainStore: in
\* any case), else it loses.
TryPublish(t) == IF val = None \/ Mutant = "PlainStore" THEN Publish(t) ELSE Lose(t)

\* The publisher opens the semaphore (`self.init.release(OPEN)`): every task queued for a turn can
\* now get through and find the value.
OpenUp(t) ==
  \* The task has just published.
  /\ pc[t] = "open"
  \* Many permits go back.
  /\ ReleaseTo(Open)
  \* A task that holds the turn gives it back next; any other is done.
  /\ pc' = [pc EXCEPT ![t] = IF turn[t] THEN "giveback" ELSE "done"]
  \* Nothing else changes.
  /\ UNCHANGED <<val, waiting, turn, result, published>>

----------------------------------------------------------------------------
\* set and the racing build.

\* set(v), its first instruction: one load. A cell that holds a value refuses at once (Err(v),
\* recorded as no result).
SetStart(t) ==
  \* The task has not started.
  /\ pc[t] = "idle"
  \* With a value there it is done; with the cell empty it goes on to publish.
  /\ pc' = [pc EXCEPT ![t] = IF val # None THEN "done" ELSE "setcas"]
  \* Nothing else changes.
  /\ UNCHANGED <<val, permits, waiting, nudged, turn, result, published>>

\* set(v), the compare-and-swap. If it loses, set returns Err(v): no result either way but its own.
SetCas(t) ==
  \* The task saw the cell empty.
  /\ pc[t] = "setcas"
  \* It publishes, or it lost: then it is done, having returned its value back to the caller.
  /\ IF val = None \/ Mutant = "PlainStore" THEN Publish(t)
     \* (the cell got a value meanwhile)
     ELSE pc' = [pc EXCEPT ![t] = "done"] /\ UNCHANGED <<val, published, result>>
  \* Nothing else changes.
  /\ UNCHANGED <<permits, waiting, nudged, turn>>

\* get_or_init_racy(f) / Lazy::force, the first instruction: one load. With a value there it
\* returns it; otherwise it builds one.
RacyStart(t) ==
  \* The task has not started.
  /\ pc[t] = "idle"
  \* Is a value published?
  /\ IF val # None
       \* Yes: the task returns it
       THEN /\ result' = [result EXCEPT ![t] = val]
            \* and is done.
            /\ pc' = [pc EXCEPT ![t] = "done"]
       \* No: it goes on to build its own value.
       ELSE /\ pc' = [pc EXCEPT ![t] = "building"]
            \* It has returned nothing yet.
            /\ UNCHANGED result
  \* Nothing else changes.
  /\ UNCHANGED <<val, permits, waiting, nudged, turn, published>>

\* The racer has built its value: try_publish.
RacyPublish(t) ==
  \* The task has built its value.
  /\ pc[t] = "building"
  \* It publishes, or loses and returns the winner's value.
  /\ TryPublish(t)
  \* Nothing else changes.
  /\ UNCHANGED <<permits, waiting, nudged, turn>>

----------------------------------------------------------------------------
\* The async initializer: get_or_init and get_or_try_init.

\* The first instruction: one load. With a value there the task returns it; otherwise it asks for
\* the turn.
InitStart(t) ==
  \* The task has not started.
  /\ pc[t] = "idle"
  \* Is a value published?
  /\ IF val # None
       \* Yes: the task returns it
       THEN /\ result' = [result EXCEPT ![t] = val]
            \* and is done.
            /\ pc' = [pc EXCEPT ![t] = "done"]
       \* No: it goes on to ask for the turn.
       ELSE /\ pc' = [pc EXCEPT ![t] = "acquire"]
            \* It has returned nothing yet.
            /\ UNCHANGED result
  \* Nothing else changes.
  /\ UNCHANGED <<val, permits, waiting, nudged, turn, published>>

\* `self.init.acquire(1)`, first poll: the task takes the permit if one is free, else it joins the
\* queue and sleeps.
Acquire(t) ==
  \* The task asks for the turn.
  /\ pc[t] = "acquire"
  \* Is a permit free?
  /\ IF permits >= 1
       \* Yes: it leaves the count
       THEN /\ permits' = permits - 1
            \* and is the task's turn.
            /\ turn' = [turn EXCEPT ![t] = TRUE]
            \* Next the task looks at the cell again.
            /\ pc' = [pc EXCEPT ![t] = "look"]
            \* It never queued.
            /\ UNCHANGED <<waiting, nudged>>
       \* No: the task joins the queue at the back,
       ELSE /\ waiting' = Append(waiting, t)
            \* not nudged,
            /\ nudged' = [nudged EXCEPT ![t] = FALSE]
            \* and sleeps.
            /\ pc' = [pc EXCEPT ![t] = "parked"]
            \* Nothing is taken.
            /\ UNCHANGED <<permits, turn>>
  \* Nothing else changes.
  /\ UNCHANGED <<val, result, published>>

\* The executor polls a nudged waiter. It takes a permit if one is free: it leaves the queue, and
\* the pass its leaving runs nudges the waiters the permits still free could serve. With none free
\* (another task was faster) it goes back to sleep, and the next release nudges it again.
Poll(t) ==
  \* The task is queued for the turn
  /\ pc[t] = "parked"
  \* and was nudged.
  /\ nudged[t]
  \* Is a permit free?
  /\ IF permits >= 1
       \* Yes: it leaves the count
       THEN /\ permits' = permits - 1
            \* and is the task's turn.
            /\ turn' = [turn EXCEPT ![t] = TRUE]
            \* The task leaves the queue.
            /\ waiting' = Remove(waiting, t)
            \* Its nudge is used up, and those behind it are nudged for the permits still free.
            /\ nudged' = [Passed(Remove(waiting, t), permits - 1, FALSE) EXCEPT ![t] = FALSE]
            \* Next the task looks at the cell again.
            /\ pc' = [pc EXCEPT ![t] = "look"]
       \* No: its nudge is used up,
       ELSE /\ nudged' = [nudged EXCEPT ![t] = FALSE]
            \* and it stays queued, asleep.
            /\ UNCHANGED <<permits, turn, waiting, pc>>
  \* Nothing else changes.
  /\ UNCHANGED <<val, result, published>>

\* With the turn, the second load (`if let Some(value) = self.get()`): the value may have been
\* published while the task waited. Then it returns it and gives the turn back; otherwise its
\* future runs.
Look(t) ==
  \* The task has the turn and looks at the cell again.
  /\ pc[t] = "look"
  \* Is a value published?
  /\ IF val # None
       \* Yes: the task returns it
       THEN /\ result' = [result EXCEPT ![t] = val]
            \* and goes on to give the turn back.
            /\ pc' = [pc EXCEPT ![t] = "giveback"]
       \* No: its future runs.
       ELSE /\ pc' = [pc EXCEPT ![t] = "running"]
            \* It has returned nothing yet.
            /\ UNCHANGED result
  \* Nothing else changes.
  /\ UNCHANGED <<val, permits, waiting, nudged, turn, published>>

\* The initializer's future completes with a value: try_publish. If `set` or a racing builder got
\* there first, the task drops its value and returns the published one.
InitOk(t) ==
  \* The task's future has finished with a value.
  /\ pc[t] = "running"
  \* It publishes, or loses and returns the winner's value.
  /\ TryPublish(t)
  \* Nothing else changes.
  /\ UNCHANGED <<permits, waiting, nudged, turn>>

\* get_or_try_init's future fails: the caller gets the error, and the turn goes back so that the
\* next waiter tries its own.
InitErr(t) ==
  \* The task's future has failed.
  /\ pc[t] = "running"
  \* It goes on to give the turn back.
  /\ pc' = [pc EXCEPT ![t] = "giveback"]
  \* Nothing else changes.
  /\ UNCHANGED <<val, permits, waiting, nudged, turn, result, published>>

\* The initializer's future is dropped mid-run: the turn guard is dropped with it, so the turn
\* goes back all the same. Mutant CancelNoReset: the task just goes, keeping the turn.
CancelInit(t) ==
  \* The task's future is running, and is dropped.
  /\ pc[t] = "running"
  \* As built it gives the turn back next; under the bug it is done at once.
  /\ pc' = [pc EXCEPT ![t] = IF Mutant = "CancelNoReset" THEN "done" ELSE "giveback"]
  \* Nothing else changes.
  /\ UNCHANGED <<val, permits, waiting, nudged, turn, result, published>>

\* The turn guard is dropped (RawPermit's Drop: `sem.release(1)`): the permit goes back, which
\* nudges the next waiter.
GiveBack(t) ==
  \* The task is about to drop its turn.
  /\ pc[t] = "giveback"
  \* It no longer holds it.
  /\ turn' = [turn EXCEPT ![t] = FALSE]
  \* The permit goes back.
  /\ ReleaseTo(1)
  \* The task is done.
  /\ pc' = [pc EXCEPT ![t] = "done"]
  \* Nothing else changes.
  /\ UNCHANGED <<val, waiting, result, published>>

\* A queued waiter's future is dropped: it leaves the queue. The pass its leaving runs nudges the
\* waiters the free permits could serve, and passes on a nudge the task carried unused to the new
\* front.
CancelWait(t) ==
  \* The task is queued for the turn.
  /\ pc[t] = "parked"
  \* It leaves the queue.
  /\ waiting' = Remove(waiting, t)
  \* Those left are nudged for the free permits, the front in any case if the task was nudged.
  /\ nudged' = [Passed(Remove(waiting, t), permits, nudged[t]) EXCEPT ![t] = FALSE]
  \* It is done, having returned nothing.
  /\ pc' = [pc EXCEPT ![t] = "done"]
  \* Nothing else changes.
  /\ UNCHANGED <<val, permits, turn, result, published>>

----------------------------------------------------------------------------
\* The specification.

\* The start: an empty cell (`const fn new`) with one free turn, nobody started.
Init ==
  \* No value is published.
  /\ val = None
  \* The turn is free.
  /\ permits = 1
  \* Nobody is queued or nudged.
  /\ waiting = <<>> /\ nudged = [t \in Tasks |-> FALSE]
  \* Nobody holds the turn.
  /\ turn = [t \in Tasks |-> FALSE]
  \* Nobody has returned anything.
  /\ result = [t \in Tasks |-> None]
  \* No task has started.
  /\ pc = [t \in Tasks |-> "idle"]
  \* Nothing was ever published.
  /\ published = <<>>

\* The steps a task takes on its own once started. A running initializer finishes, with a value
\* or an error.
Internal(t) ==
  \* The publishers' steps,
  \/ SetCas(t) \/ RacyPublish(t) \/ OpenUp(t)
  \* the steps of a task after the turn,
  \/ Acquire(t) \/ Poll(t) \/ Look(t) \/ GiveBack(t)
  \* and an initializer's future ending well or badly.
  \/ InitOk(t) \/ InitErr(t)

\* Every step the system can take.
Next ==
  \* Some task
  \E t \in Tasks :
    \* calls set, starts a racing build, starts get_or_init,
    \/ SetStart(t) \/ RacyStart(t) \/ InitStart(t)
    \* drops its initializer, drops its wait, or takes one of its own steps.
    \/ CancelInit(t) \/ CancelWait(t) \/ Internal(t)

\* Fairness: started operations finish and nudged futures are polled.
\* Starting one, and dropping a future, are the caller's choices.
Fairness == \A t \in Tasks : WF_vars(Internal(t))

\* Every behaviour: start in Init, take Next steps or stutter, fairly.
Spec == Init /\ [][Next]_vars /\ Fairness

----------------------------------------------------------------------------
\* Invariants.

\* Every variable holds what its comment says.
TypeOK ==
  \* The value is a task's or none.
  /\ val \in Tasks \cup {None}
  \* The free permits are at most the one turn plus what a publisher added.
  /\ permits \in 0..(1 + Open)
  \* Only tasks are queued, each once.
  /\ Range(waiting) \subseteq Tasks /\ Len(waiting) = Cardinality(Range(waiting))
  \* Every task is at one of the known places.
  /\ pc \in [Tasks -> PCs]
  \* The queued tasks are exactly the sleeping ones.
  /\ Range(waiting) = {t \in Tasks : pc[t] = "parked"}

\* EXACTLY ONE VALUE IS PUBLISHED. At most one publication ever happens,
\* so the value never changes once set.
\* Rules out, for example: one task publishes value 1, another task has already returned it to its
\* caller, and then a second publisher stores value 2 over it.
PublishedOnce == Len(published) <= 1

\* EVERY READER AGREES. Every value a task returned is the published one.
\* Rules out, for example: two tasks each believe they initialized the cell and return two different
\* values.
ReadersAgree == \A t \in Tasks : result[t] # None => (val # None /\ result[t] = val)

\* A publisher that has published and not yet opened the semaphore: its opening is still to come.
Opening == \E t \in Tasks : pc[t] = "open"

\* NO LOST WAKEUP. The turn is never lost, and no waiter is left asleep
\* beside a turn it could take or a value it could read.
NoLostWakeup ==
  \* The turn is held only by a task that is still at work, which will give it back. Rules out, for
  \* example: an initializer's future is dropped and keeps the turn; everybody behind it waits for
  \* ever.
  /\ \A t \in Tasks : turn[t] => pc[t] \in {"look", "running", "open", "giveback"}
  \* While a permit is free, the front waiter has a wake on its way.
  /\ (waiting # <<>> /\ permits >= 1) => nudged[Head(waiting)]
  \* Once the value is published and its publisher has opened the semaphore, a permit is free for
  \* every waiter. Rules out, for example: `set` publishes while an initializer is still running,
  \* and the task queued behind that initializer goes on sleeping beside the value.
  /\ (val # None /\ ~Opening /\ waiting # <<>>) => permits >= Len(waiting)

----------------------------------------------------------------------------
\* Liveness.

\* Every waiter finishes: it gets the turn, or its future is dropped.
\* Rules out a waiter that never learns the value and never gets its own turn to initialize.
EveryWaiterFinishes == \A t \in Tasks : (pc[t] = "parked") ~> (pc[t] # "parked")

====
