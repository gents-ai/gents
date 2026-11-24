---- MODULE SyncLatch ----
\* ===========================================================================
\* THE STORY
\* ===========================================================================
\* A gate holds tasks back until something has happened, and then lets all
\* of them through at once. regolith has three for async code (src/sync/):
\*   Event    opens when somebody calls `set` (src/sync/event.rs);
\*   Latch    opens when it has been counted down to 0 (src/sync/latch.rs);
\*   Barrier  opens for a group as soon as the group is complete, then
\*            closes again for the next group (src/sync/barrier.rs).
\* A task that finds the gate shut does not block its thread: it leaves a
\* note with its "waker" in the gate's waiter list and goes to sleep. Who
\* opens the gate takes the list and wakes everybody on it.
\*
\* Tiny example of what can go wrong. A task looks at the gate: shut. Before
\* it has put its note on the list, another task opens the gate, takes the
\* (still empty) list and wakes nobody. Now the first task puts its note on
\* the list and goes to sleep, beside an open gate, and nobody will ever
\* wake it. So after putting its note down, a waiter looks at the gate a
\* second time.
\*
\* What this model shows, for three tasks in every interleaving: nobody gets
\* through a shut gate, and nobody sleeps beside an open one. The two
\* planted bugs (no second look; a Barrier waiter that looks at the wrong
\* thing) each leave a sleeper behind.
\*
\* The gates covered: Event, Latch and Barrier (plan 4.11): one-shot and
\* reusable gates whose waiters all pass together.
\*
\* PROVED FOR EVERY SIZE in Lean: nothing specific to these gates. Their
\* waiter list is the registration-then-re-check pattern of SyncNotify.tla,
\* whose queue laws proofs/lean/Regolith/SyncFifo.lean proves (handed_holds,
\* no_stranded_waiter); the gate conditions are checked here only, for
\* three tasks. D49 changes nothing here: a gate releases every waiter at
\* once, so there is no handoff to replace with barging.
\*
\* THE DESIGN (plan 4.11).
\*   Event   `set`, `wait()`, `is_set`: a one-shot event. It is a Latch of
\*           count 1, with `set` its count_down; setting a set Event changes
\*           nothing.
\*   Latch   `count_down`, `wait()`, `is_set`: wait() completes once the
\*           count reaches 0, and stays complete.
\*   Barrier `wait()`: a fixed number of parties. The party whose arrival
\*           completes the generation releases every waiter of that
\*           generation, and the barrier is reused for the next one.
\*
\* THE PROTOCOL MODELLED. The count (Barrier: arrivals and the generation)
\* is one atomic word; waiters register in a lock-free list, here a set,
\* since a release wakes all of them. A waiter that finds the gate shut
\* pushes its node with its Waker, then re-checks the gate: a release that
\* ran between the first check and the push found no node to wake. The
\* release takes the whole list in one swap, then wakes each waiter in it.
\* A Barrier waiter checks its generation, never the arrival count, since
\* the count of the next generation starts while the woken waiters of the
\* last one have yet to be polled.
\*
\* DESIGN CHOICE WHERE THE PLAN IS SILENT, recorded in the final report.
\*   A Barrier arrival stays counted when its wait() future is dropped:
\*   withdrawing it would race with the arrival that completes the
\*   generation. The generation still completes once enough parties arrive;
\*   the dropped waiter is simply not woken.
\*
\* WHAT THE MODEL LEAVES OUT. Memory ordering (loom's, plan 7.1).
\*
\* CONFIGURATIONS.
\*   MC_SyncLatch_Green_Event        an Event set by any task.
\*   MC_SyncLatch_Green_Latch        a Latch of count 2.
\*   MC_SyncLatch_Green_Barrier      a Barrier of 2 parties, three tasks,
\*                                   two rounds each.
\*   MC_SyncLatch_Red_NoRecheck      an Event waiter parks without the
\*                                   re-check: NoLostWakeup.
\*   MC_SyncLatch_Red_CountCheck     a Barrier waiter checks the arrival
\*                                   count, not its generation: NoLostWakeup.

\* Borrow the standard TLA+ books: counting numbers and sizes of finite sets.
EXTENDS Naturals, FiniteSets

\* The fixed inputs a configuration chooses.
CONSTANTS
  \* The tasks that use the gate, named by positive numbers.
  Tasks,
  \* Which gate is modelled: "Latch" (an Event is a Latch that counts down from 1) or "Barrier".
  Kind,
  \* For a Latch: the count it starts from (Latch::new). For a Barrier: how many parties make a full
  \* group (Barrier::new).
  Count,
  \* For a Barrier: the largest number of times each task waits at it.
  Rounds,
  \* "none" for the design, or the planted bug: "NoRecheck" or "CountCheck".
  Mutant

\* The kind is one of the two.
ASSUME Kind \in {"Latch", "Barrier"}
\* The count is at least 1, and the number of rounds is a number.
ASSUME Count \in Nat \ {0} /\ Rounds \in Nat
\* The mutant is "none" or one of the two planted bugs.
ASSUME Mutant \in {"none", "NoRecheck", "CountCheck"}

\* The state: everything that changes from one step to the next. Each line says what the variable
\* stands for in the real program.
VARIABLES
  \* For a Latch: how much is left to count down (Latch::count, src/sync/latch.rs; the gate itself
  \* is an Event, src/sync/event.rs, set when the count reaches 0).
  count,
  \* For a Barrier: how many parties have arrived in the current group (the low half of the barrier
  \* word, src/sync/barrier.rs).
  arrived,
  \* For a Barrier: the number of the current group, its "generation" (the high half of the barrier
  \* word).
  bgen,
  \* For a Barrier: the generation each task arrived in (a waiter carries its generation).
  mygen,
  \* For a Barrier: how many waits each task has made so far. Model bookkeeping that keeps the runs
  \* finite.
  rounds,
  \* The waiters registered right now: their nodes in the gate's waiter list (the Waiter nodes of
  \* src/sync/waiter.rs). A set is enough, because a release wakes all of them.
  q,
  \* Whether each waiter was woken since it registered its waker.
  woken,
  \* The waiters a releasing task has taken from the list and has not woken yet, per releasing task.
  towake,
  \* Where a releasing task goes on once its wakes are delivered, per task.
  ret,
  \* Where each task is: its "program counter" (see PCs below).
  pc

\* Every variable, so a step that changes none of them is a stutter.
vars == <<count, arrived, bgen, mygen, rounds, q, woken, towake, ret, pc>>

\* The places a task can be: "idle", "push" (found the gate shut, about to
\* push its node), "recheck" (pushed, about to look again), "parked" (its
\* future returned Pending), "wake" (a releaser waking waiters), "passed"
\* (its wait completed).
PCs == {"idle", "push", "recheck", "parked", "wake", "passed"}

\* Task t's gate is open. Latch: the count is 0. Barrier: its generation
\* was released (mutant CountCheck: the arrival count reads 0).
Open(t) ==
  \* A Latch: the count is 0.
  IF Kind = "Latch" THEN count = 0
  \* A Barrier under the planted bug CountCheck: the arrival count reads 0.
  ELSE IF Mutant = "CountCheck" THEN arrived = 0
  \* A Barrier in the design: the current generation is no longer the one the task arrived in.
  ELSE bgen # mygen[t]

\* Task t's gate really is open: the Latch count is 0, or t's Barrier
\* generation was released. (Open(t) is what a waiter checks, which a
\* mutant may get wrong; this is the truth it is judged against.)
Released(t) == IF Kind = "Latch" THEN count = 0 ELSE bgen > mygen[t]

\* Task t takes the whole waiter list and goes on to wake it, then resumes
\* at `next`.
ReleaseAll(t, next) ==
  \* The task takes every registered waiter onto its own wake list.
  /\ towake' = [towake EXCEPT ![t] = q]
  \* The gate's list is empty.
  /\ q' = {}
  \* The task goes on to wake them.
  /\ pc' = [pc EXCEPT ![t] = "wake"]
  \* It remembers where to go on afterwards.
  /\ ret' = [ret EXCEPT ![t] = next]

----------------------------------------------------------------------------
\* Actions.

\* Latch::count_down / Event::set. The count drops; the step that takes it
\* to 0 releases every waiter. On a count already at 0 it changes nothing,
\* so it is not a step.
CountDown(t) ==
  \* Only a Latch counts down.
  /\ Kind = "Latch"
  \* The task is idle.
  /\ pc[t] = "idle"
  \* There is something left to count.
  /\ count > 0
  \* One less is left.
  /\ count' = count - 1
  \* Was that the last one?
  /\ IF count = 1
       \* Yes: every waiter is released.
       THEN ReleaseAll(t, "idle")
       \* No: nobody is released yet.
       ELSE UNCHANGED <<q, towake, pc, ret>>
  \* Nothing else changes.
  /\ UNCHANGED <<arrived, bgen, mygen, rounds, woken>>

\* Latch::wait / Event::wait, first poll: complete at once if the count is
\* 0, otherwise go on to register.
LatchWait(t) ==
  \* Only a Latch.
  /\ Kind = "Latch"
  \* The task is idle.
  /\ pc[t] = "idle"
  \* With the count at 0 it passes at once; otherwise it goes on to register.
  /\ pc' = [pc EXCEPT ![t] = IF count = 0 THEN "passed" ELSE "push"]
  \* Nothing else changes.
  /\ UNCHANGED <<count, arrived, bgen, mygen, rounds, q, woken, towake, ret>>

\* Barrier::wait, first poll: one atomic step on the arrival word. The
\* arrival that completes the generation starts the next one and releases
\* every waiter; any other goes on to register.
Arrive(t) ==
  \* Only a Barrier.
  /\ Kind = "Barrier"
  \* The task is idle.
  /\ pc[t] = "idle"
  \* It has rounds left.
  /\ rounds[t] < Rounds
  \* This is one more of its rounds.
  /\ rounds' = [rounds EXCEPT ![t] = @ + 1]
  \* It arrives in the current generation.
  /\ mygen' = [mygen EXCEPT ![t] = bgen]
  \* Does its arrival complete the group?
  /\ IF arrived + 1 = Count
       \* Yes: the next group starts with no arrival,
       THEN /\ arrived' = 0
            \* the generation moves on,
            /\ bgen' = bgen + 1
            \* and every waiter is released; the task itself passes once it has woken them.
            /\ ReleaseAll(t, "passed")
       \* No: one more party has arrived,
       ELSE /\ arrived' = arrived + 1
            \* and the task goes on to register.
            /\ pc' = [pc EXCEPT ![t] = "push"]
            \* The generation and the waiter list are untouched.
            /\ UNCHANGED <<bgen, q, towake, ret>>
  \* Nothing else changes.
  /\ UNCHANGED <<count, woken>>

\* The waiter pushes its node, Waker inside. Mutant NoRecheck: it parks at
\* once, without looking at the gate again.
Push(t) ==
  \* The task found the gate shut and has not registered yet.
  /\ pc[t] = "push"
  \* Its node joins the waiter list.
  /\ q' = q \cup {t}
  \* It has not been woken.
  /\ woken' = [woken EXCEPT ![t] = FALSE]
  \* It goes on to look at the gate once more (under the bug NoRecheck it goes straight to sleep).
  /\ pc' = [pc EXCEPT ![t] = IF Mutant = "NoRecheck" THEN "parked" ELSE "recheck"]
  \* Nothing else changes.
  /\ UNCHANGED <<count, arrived, bgen, mygen, rounds, towake, ret>>

\* The re-check after the push: if the gate opened meanwhile, complete
\* (taking the node back out); otherwise park.
Recheck(t) ==
  \* The task is at its second look.
  /\ pc[t] = "recheck"
  \* Is the gate open now?
  /\ IF Open(t)
       \* Yes: it passes, and takes its node back out of the list.
       THEN pc' = [pc EXCEPT ![t] = "passed"] /\ q' = q \ {t}
       \* No: it goes to sleep, registered.
       ELSE pc' = [pc EXCEPT ![t] = "parked"] /\ UNCHANGED q
  \* Nothing else changes.
  /\ UNCHANGED <<count, arrived, bgen, mygen, rounds, woken, towake, ret>>

\* The executor polls a woken future: if the gate is open it completes,
\* otherwise it registers its waker again (its node is gone with the swap,
\* so a wrong answer here strands it).
Poll(t) ==
  \* The task is asleep.
  /\ pc[t] = "parked"
  \* It was woken.
  /\ woken[t]
  \* Its woken bit is cleared.
  /\ woken' = [woken EXCEPT ![t] = FALSE]
  \* If the gate is open it passes; otherwise it sleeps on.
  /\ pc' = [pc EXCEPT ![t] = IF Open(t) THEN "passed" ELSE "parked"]
  \* Nothing else changes.
  /\ UNCHANGED <<count, arrived, bgen, mygen, rounds, q, towake, ret>>

\* The pending wait is dropped: its node leaves the list. A Barrier
\* arrival stays counted.
Cancel(t) ==
  \* The task is asleep.
  /\ pc[t] = "parked"
  \* Its node leaves the list.
  /\ q' = q \ {t}
  \* Its woken bit is cleared.
  /\ woken' = [woken EXCEPT ![t] = FALSE]
  \* It is idle again.
  /\ pc' = [pc EXCEPT ![t] = "idle"]
  \* Nothing else changes (a Barrier arrival stays counted).
  /\ UNCHANGED <<count, arrived, bgen, mygen, rounds, towake, ret>>

\* A releaser wakes the waiters it took, one per step.
Wake(t) ==
  \* The task is delivering wakes.
  /\ pc[t] = "wake"
  \* None left?
  /\ IF towake[t] = {}
       \* Then it goes where it meant to go,
       THEN /\ pc' = [pc EXCEPT ![t] = ret[t]]
            \* and that memory is cleared.
            /\ ret' = [ret EXCEPT ![t] = "idle"]
            \* Nobody is woken.
            /\ UNCHANGED <<woken, towake>>
       \* Otherwise it picks one waiter u from its list:
       ELSE \E u \in towake[t] :
              \* u is woken,
              /\ woken' = [woken EXCEPT ![u] = TRUE]
              \* and leaves the list.
              /\ towake' = [towake EXCEPT ![t] = @ \ {u}]
              \* The task stays here for the rest.
              /\ UNCHANGED <<pc, ret>>
  \* Nothing else changes.
  /\ UNCHANGED <<count, arrived, bgen, mygen, rounds, q>>

\* Barrier: a task that passed goes on to its next round.
Leave(t) ==
  \* Only a Barrier.
  /\ Kind = "Barrier"
  \* The task has passed.
  /\ pc[t] = "passed"
  \* It is idle again, free to wait at the next group.
  /\ pc' = [pc EXCEPT ![t] = "idle"]
  \* Nothing else changes.
  /\ UNCHANGED <<count, arrived, bgen, mygen, rounds, q, woken, towake, ret>>

----------------------------------------------------------------------------
\* The specification.

\* The start: the full count, no arrival, nobody waiting.
Init ==
  \* A Latch starts at its full count (a Barrier does not use this).
  /\ count = IF Kind = "Latch" THEN Count ELSE 0
  \* No party has arrived.
  /\ arrived = 0
  \* The first generation is 0.
  /\ bgen = 0
  \* No task has arrived in any generation.
  /\ mygen = [t \in Tasks |-> 0]
  \* No task has made a round.
  /\ rounds = [t \in Tasks |-> 0]
  \* Nobody is registered.
  /\ q = {}
  \* Nobody was woken.
  /\ woken = [t \in Tasks |-> FALSE]
  \* Nobody has wakes to deliver.
  /\ towake = [t \in Tasks |-> {}]
  \* Every task would go on to idle.
  /\ ret = [t \in Tasks |-> "idle"]
  \* Every task is idle.
  /\ pc = [t \in Tasks |-> "idle"]

\* The steps a task takes on its own once started.
Internal(t) == Push(t) \/ Recheck(t) \/ Poll(t) \/ Wake(t)

\* Every step the system can take.
Next ==
  \* Some task
  \E t \in Tasks :
    \* counts down, waits at the latch, arrives at the barrier, drops its wait, leaves, or takes one
    \* of its own steps.
    CountDown(t) \/ LatchWait(t) \/ Arrive(t) \/ Cancel(t) \/ Leave(t) \/ Internal(t)

\* Fairness: a started operation finishes and a woken future is polled.
\* Counting down, arriving, waiting and cancelling are the caller's choices.
Fairness == \A t \in Tasks : WF_vars(Internal(t))

\* Every behaviour: start in Init, take Next steps or stutter, fairly.
Spec == Init /\ [][Next]_vars /\ Fairness

----------------------------------------------------------------------------
\* Invariants.

\* Every variable holds what its comment says.
TypeOK ==
  \* The count stays between 0 and its start.
  /\ count \in 0..Count
  \* Fewer parties than a full group have arrived.
  /\ arrived \in 0..(Count - 1)
  \* Only tasks are registered.
  /\ q \subseteq Tasks
  \* Every task is at one of the known places.
  /\ pc \in [Tasks -> PCs]
  \* Every woken bit is a yes or a no.
  /\ woken \in [Tasks -> BOOLEAN]

\* THE GATE HOLDS. Latch: no wait completes while the count is above 0.
\* Barrier: no task passes a generation before that generation is
\* released, which only the arrival of its last party does.
\* Rules out, for example: a wait on a Latch of count 2 completes after one count_down; or a Barrier
\* party of 2 walks through alone.
GateHolds == \A t \in Tasks : pc[t] = "passed" => Released(t)

\* NO LOST WAKEUP. A parked waiter whose gate is open has been woken, or
\* its releaser is about to wake it.
NoLostWakeup ==
  \* For every task
  \A t \in Tasks :
    \* that sleeps although its gate really is open: it was woken, or a releasing task has it on its
    \* wake list. Rules out, for example: the gate opens between a waiter's first look and its
    \* registration, nobody sees its node, and it sleeps for ever beside an open gate.
    (pc[t] = "parked" /\ Released(t)) => (woken[t] \/ \E d \in Tasks : t \in towake[d])

----------------------------------------------------------------------------
\* Liveness.

\* A parked waiter whose gate opened stops waiting.
ReleasedWaitersPass ==
  \* For every task: once it sleeps beside an open gate, it stops sleeping. Rules out a released
  \* waiter that is never polled awake.
  \A t \in Tasks : (pc[t] = "parked" /\ Released(t)) ~> (pc[t] # "parked")

====
