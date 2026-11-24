---- MODULE SyncNotify ----
\* ===========================================================================
\* THE STORY
\* ===========================================================================
\* Notify (src/sync/notify.rs) is a doorbell for async tasks. A task that
\* has nothing to do calls `notified()` and goes to sleep. Another task
\* rings: `notify_one` wakes the task that has waited longest, and
\* `notify_waiters` wakes everybody who is waiting right now. If nobody
\* waits when `notify_one` rings, the ring is not lost: one notification is
\* stored, and the next task to call `notified()` takes it at once.
\*
\* Tiny example of what can go wrong. A task looks: no notification stored.
\* Before it has put itself in the waiter queue, `notify_one` rings, finds
\* the queue empty, and stores the notification. The task then joins the
\* queue and sleeps, beside a stored notification that nobody will hand to
\* it. So after joining the queue a waiter looks again.
\*
\* Second example. The ringer wakes a waiter first and gives it the
\* notification afterwards. The woken waiter runs at once, finds nothing,
\* and goes back to sleep; the notification then arrives with no wake after
\* it. So the notification is handed over first, and the wake comes second.
\*
\* What this model shows, for three tasks in every interleaving: the oldest
\* waiter is served first, a handed notification is always followed by a
\* wake, nobody misses a broadcast it registered before, nobody sleeps
\* beside a stored notification, and a waiter that gives up passes its
\* notification on.
\*
\* The primitive covered: Notify (plan 4.11): notify_one stores one permit,
\* notified() consumes one, notify_waiters wakes every waiter registered
\* before it. The waiter queue hands a notification to the oldest waiter
\* before waking it.
\*
\* WHY NOTIFY KEEPS FIFO HANDOFF. D49 replaced strict handoff with barging
\* for the lock-like primitives (Sync.tla): handing a lock to a suspended
\* task stalls every other acquirer until that task runs. A notification is
\* addressed to a waiter, holds nothing other tasks need, and the D49 note
\* keeps Notify's semantics; so its queue stays FIFO with handoff. This model
\* is the one that checked Notify before D49, with the lock-only parts
\* (Semaphore, ReentrantMutex, owned guards) moved to the barging Sync.tla.
\*
\* PROVED FOR EVERY SIZE in Lean, proofs/lean/Regolith/SyncFifo.lean (the
\* FIFO waiter queue with handoff before wake, one permit per request):
\*   step_inv, reachable_inv      the queue's invariant holds in every state
\*   fifo_served, served_in_order no request is served ahead of one that
\*                                entered before it and still waits
\*                                (FifoHandoff here)
\*   no_stranded_waiter           a waiter never waits beside a stored permit
\*                                (NoLostWakeup's Stranded here)
\*   handed_holds,                a waiter is woken only once it holds its
\*   no_lost_handoff              notification (GrantedWoken here)
\*   position_never_grows,        a waiter's place only moves forward, one
\*   handoff_advances             place per handoff
\*   wake_before_handoff_strands, cancel_without_pass_on_strands
\*                                the RED cases, as counterexamples
\* TLC checks the concurrent protocol here, step by atomic step.
\*
\* THE PROTOCOL MODELLED, one atomic step per word-sized CAS or queue
\* operation:
\*   - one state word holds `avail` (the stored permit, 0 or 1), `waiters`
\*     (registered waiters not yet popped) and `bgen` (the broadcast
\*     generation). notified() consumes a stored permit at once only when
\*     nobody is registered;
\*   - a waiter registers (waiters + 1, recording bgen), pushes its node with
\*     its Waker, then drains: the re-check after the push;
\*   - a node's state is CAS'd from "waiting" to exactly one of "granted"
\*     (handed the permit), "cancelled" or "broadcast";
\*   - the drain token: only its holder pops the queue and hands permits on.
\*     Nobody waits for it: a task that finds it taken leaves, and the holder
\*     re-checks as it lets go;
\*   - notify_waiters bumps bgen and CASes every waiting node to "broadcast"
\*     (one step here); a waiter registered under an older generation that
\*     pushes afterwards notifies itself at the push. In the code that check
\*     is the drain pass that takes the node in (Notify::pass): it grants
\*     every arrival of an older generation, wherever the arrival lands in
\*     the queue. Generation order is not queue order: a waiter can pass
\*     its check, stop, and push after a newer waiter, so a pass that only
\*     granted from the front would stop at that newer waiter.
\*
\* WHAT THE MODEL LEAVES OUT, and why that loses nothing.
\*   - A task has one request at a time and may not register again while its
\*     previous node is still queued: two nodes of one task behave as two
\*     tasks.
\*   - A cancel is one step; a drainer's pop and its `waiters` decrement are
\*     one step; letting the token go and the re-check are one step. Each
\*     split only adds redundant or conservative interleavings.
\*   - Memory ordering is loom's (plan 7.1).
\*
\* CONFIGURATIONS.
\*   MC_SyncNotify_Green              task 3 notifies (notify_one and two
\*                                    notify_waiters), tasks 1 and 2 wait and
\*                                    cancel: every safety invariant.
\*   MC_SyncNotify_Green_Live         the same with one waiter: a notified
\*                                    waiter completes.
\*   MC_SyncNotify_Red_WakeBeforeHandoff  wake, then hand: NoLostWakeup.
\*   MC_SyncNotify_Red_CancelNoPassOn a dropped waiter keeps the notification
\*                                    handed to it: CancelPassesOn.
\*   MC_SyncNotify_Red_NoRecheck      park right after the push: NoLostWakeup.
\*   MC_SyncNotify_Red_NoGenCheck     a waiter that pushed after a broadcast
\*                                    misses it: NoLostWakeup.
\*   MC_SyncNotify_Red_FrontGenCheck  the older waiter's generation is checked
\*                                    only when every waiting node ahead of it
\*                                    is older too: one that pushed behind a
\*                                    newer waiter misses the broadcast,
\*                                    NoLostWakeup.

\* Borrow the standard TLA+ books: counting numbers, lists, and sizes of finite sets.
EXTENDS Naturals, Sequences, FiniteSets

\* The fixed inputs a configuration chooses.
CONSTANTS
  \* The tasks that use the Notify, named by positive numbers.
  Tasks,
  \* The tasks that send notifications; the others wait on notified().
  Notifiers,
  \* The tasks whose "a notified waiter completes" promise is checked: one waiter is enough.
  Watched,
  \* The largest number of notify_waiters calls in one run.
  MaxBcast,
  \* "none" for the design, or the name of one planted bug.
  Mutant

\* The value "no task", for the drain token and a drainer's target.
NoTask == 0

\* Tasks are positive numbers, and the notifiers and the watched tasks are among them.
ASSUME Tasks \subseteq Nat \ {0} /\ Notifiers \subseteq Tasks /\ Watched \subseteq Tasks
\* The broadcast budget is a number.
ASSUME MaxBcast \in Nat
\* The mutant is "none" or one of the five planted bugs.
ASSUME Mutant \in {"none", "WakeBeforeHandoff", "CancelNoPassOn", "NoRecheck", "NoGenCheck",
                   "FrontGenCheck"}

\* The state: everything that changes from one step to the next. Each line says what the variable
\* stands for in the real program.
VARIABLES
  \* 1 when a notification is stored for the next waiter, else 0 (the PERMIT bit of the state word,
  \* src/sync/notify.rs).
  avail,
  \* How many waiters are registered and not yet removed from the queue. The engine keeps only "is
  \* anybody queued", the QUEUED bit (src/sync/queue.rs).
  waiters,
  \* The waiter queue: the waiting tasks, oldest first, each with at most one node (the Waiter nodes
  \* of src/sync/waiter.rs, kept in Notify::waiting).
  fifo,
  \* The state of each task's waiter node: see NodeStates below (a Waiter's phase).
  node,
  \* Whether each waiter was woken since it registered its waker.
  woken,
  \* Whether each task holds a notification that was handed to it and that it has not consumed yet:
  \* 1 or 0.
  held,
  \* The drain token: which task is the one allowed to look at the queue right now, or NoTask (the
  \* DRAINING bit of the state word, src/sync/queue.rs).
  token,
  \* The notification a draining task has taken out of the store for the head of the queue: 1 or 0,
  \* per draining task.
  hand,
  \* Which waiter a draining task is serving, per draining task.
  target,
  \* Where a task goes on once its drain is over, per task.
  ret,
  \* Where each task is in its operation: its "program counter" (see PCs below).
  pc,
  \* Model bookkeeping (a "ghost"): the tasks with a live request, in the order they got in line.
  \* The promise FifoHandoff reads it.
  line,
  \* How many notify_waiters calls there have been: the broadcast generation (Notify::generation).
  bgen,
  \* The broadcast generation each waiter saw when it registered (a waiter carries it as its
  \* payload).
  gen,
  \* The waiters a broadcasting task has marked and has not woken yet, per broadcasting task.
  bwake

\* Every variable, so a step that changes none of them is a stutter.
vars == <<avail, waiters, fifo, node, woken, held, token, hand, target, ret, pc,
          line, bgen, gen, bwake>>

\* The steps of the drain, where a task may hold the token.
DrainPCs == {"dtake", "dloop", "dgrant", "dwake", "dfree"}

\* The places a task can be: "idle", "push" (registered, about to push its
\* node), "parked" (its future returned Pending), "bwake" (a broadcaster
\* waking waiters), or one of the drain steps.
PCs == {"idle", "push", "parked", "bwake"} \cup DrainPCs

\* The states of a node. "none": no node, or one nobody looks at any more.
NodeStates == {"none", "waiting", "granted", "cancelled", "broadcast"}

----------------------------------------------------------------------------
\* Helpers.

\* Sequence s without element x.
Remove(s, x) == SelectSeq(s, LAMBDA y : y # x)

\* The smaller of two naturals.
Min(a, b) == IF a <= b THEN a ELSE b

\* Task t has a node in the queue (live or stale).
InFifo(t) == \E i \in 1..Len(fifo) : fifo[i] = t

\* Some node in the queue still waits and belongs to the current broadcast
\* generation: a waiter that registered after the last notify_waiters.
CurrentWaiting == \E i \in 1..Len(fifo) : node[fifo[i]] = "waiting" /\ gen[fifo[i]] = bgen

\* Task t's future still waits on its node: parked, or running the drain
\* that re-checks right after its push.
Waits(t) == pc[t] = "parked" \/ (pc[t] \in DrainPCs /\ ret[t] = "parked")

\* The head of the queue is work for a drainer: a stale node to drop, or a
\* waiting node and a stored permit.
Work == fifo # <<>> /\ (node[Head(fifo)] # "waiting" \/ avail >= 1)

\* Task t goes on to drain (try the token), and resumes at `r` afterwards.
DrainThen(t, r) ==
  \* It goes to the first step of the drain,
  /\ pc' = [pc EXCEPT ![t] = "dtake"]
  \* and remembers where to go on afterwards.
  /\ ret' = [ret EXCEPT ![t] = r]

\* Task t's drain is over: it resumes where it was going.
Resume(t) ==
  \* It goes where it meant to go,
  /\ pc' = [pc EXCEPT ![t] = ret[t]]
  \* and that memory is cleared.
  /\ ret' = [ret EXCEPT ![t] = "idle"]

----------------------------------------------------------------------------
\* Waiting.

\* notified(), first poll. One CAS on the state word: consume a stored
\* permit if nobody is registered; otherwise register, recording bgen.
Acquire(t) ==
  \* The task is idle.
  /\ pc[t] = "idle"
  \* It is a waiter, not a notifier.
  /\ t \notin Notifiers
  \* It has no node in the queue.
  /\ ~InFifo(t)
  \* Is a notification stored, with nobody registered ahead?
  /\ IF waiters = 0 /\ avail >= 1
       \* Yes: the task consumes it,
       THEN /\ avail' = avail - 1
            \* and is done at once.
            /\ UNCHANGED <<waiters, gen, pc>>
       \* No: it registers as a waiter,
       ELSE /\ waiters' = waiters + 1
            \* notes the broadcast generation it saw,
            /\ gen' = [gen EXCEPT ![t] = bgen]
            \* and goes on to push its node.
            /\ pc' = [pc EXCEPT ![t] = "push"]
            \* The store is untouched.
            /\ UNCHANGED avail
  \* Nothing else changes.
  /\ UNCHANGED <<fifo, node, woken, held, token, hand, target, ret, line, bgen, bwake>>

\* The registered waiter pushes its node, Waker inside, then drains (the
\* re-check). A broadcast that ran since registration could not see this
\* node, so the waiter notifies itself: the pass that takes the node in
\* grants it, whatever waits ahead of it. Mutant NoRecheck: it parks at
\* once. Mutant NoGenCheck: it skips the generation check. Mutant
\* FrontGenCheck: the pass grants older waiters only from the front of the
\* queue and stops at the first waiter of the current generation, so the
\* check is skipped when such a waiter is queued ahead.
Push(t) ==
  \* The task registered and has not pushed yet.
  /\ pc[t] = "push"
  \* Its node joins the tail of the queue.
  /\ fifo' = Append(fifo, t)
  \* It gets in line.
  /\ line' = Append(line, t)
  \* Did a broadcast happen since it registered? The pass that takes the node in asks,
  /\ IF /\ gen[t] < bgen
        \* unless the bug NoGenCheck skips the question,
        /\ Mutant # "NoGenCheck"
        \* or the bug FrontGenCheck stops at a newer waiter queued ahead and never reaches this node.
        /\ ~(Mutant = "FrontGenCheck" /\ CurrentWaiting)
       \* Yes: that broadcast could not see this node, so the waiter marks itself notified,
       THEN /\ node' = [node EXCEPT ![t] = "broadcast"]
            \* counts as woken,
            /\ woken' = [woken EXCEPT ![t] = TRUE]
            \* and drains, then waits for its poll.
            /\ DrainThen(t, "parked")
       \* No: its node is waiting,
       ELSE /\ node' = [node EXCEPT ![t] = "waiting"]
            \* it has not been woken,
            /\ woken' = [woken EXCEPT ![t] = FALSE]
            \* and, under the bug NoRecheck,
            /\ IF Mutant = "NoRecheck"
                 \* it goes straight to sleep;
                 THEN pc' = [pc EXCEPT ![t] = "parked"] /\ UNCHANGED ret
                 \* in the design it drains first (the second look), then sleeps.
                 ELSE DrainThen(t, "parked")
  \* Nothing else changes.
  /\ UNCHANGED <<avail, waiters, held, token, hand, target, bgen, gen, bwake>>

\* The executor polls a woken future. Granted or broadcast: notified()
\* completes, consuming the handed permit if any. Otherwise it registers its
\* waker again.
Poll(t) ==
  \* The task is asleep.
  /\ pc[t] = "parked"
  \* It was woken.
  /\ woken[t]
  \* Its woken bit is cleared.
  /\ woken' = [woken EXCEPT ![t] = FALSE]
  \* Was it handed a notification, or reached by a broadcast?
  /\ IF node[t] \in {"granted", "broadcast"}
       \* Yes. A broadcast node still in the queue stays marked for the drainer to pop; any other
       \* finished node is forgotten.
       THEN
            \* Its node: kept as it is while still queued, forgotten otherwise.
            /\ node' = [node EXCEPT ![t] = IF InFifo(t) THEN node[t] ELSE "none"]
            \* It forgets its generation.
            /\ gen' = [gen EXCEPT ![t] = 0]
            \* Its notified() completes: it is idle.
            /\ pc' = [pc EXCEPT ![t] = "idle"]
            \* It consumes what it was handed.
            /\ held' = [held EXCEPT ![t] = 0]
            \* It leaves the line.
            /\ line' = Remove(line, t)
       \* No: it sleeps on, with its waker registered again.
       ELSE UNCHANGED <<node, gen, pc, held, line>>
  \* Nothing else changes.
  /\ UNCHANGED <<avail, waiters, fifo, token, hand, target, ret, bgen, bwake>>

\* The pending future is dropped. A waiting node is CAS'd to "cancelled". A
\* granted one passes the permit on: stored again, then a drain hands it to
\* the next waiter. Either way it drains. Mutant CancelNoPassOn: a granted
\* waiter keeps the permit.
Cancel(t) ==
  \* The task is asleep.
  /\ pc[t] = "parked"
  \* It leaves the line.
  /\ line' = Remove(line, t)
  \* Its woken bit is cleared.
  /\ woken' = [woken EXCEPT ![t] = FALSE]
  \* It forgets its generation.
  /\ gen' = [gen EXCEPT ![t] = 0]
  \* Its node was still waiting:
  /\ CASE node[t] = "waiting" ->
            \* the node is marked cancelled, for the drainer to pop,
            /\ node' = [node EXCEPT ![t] = "cancelled"]
            \* and the task drains, then is idle.
            /\ DrainThen(t, "idle")
            \* It held nothing.
            /\ UNCHANGED <<avail, held>>
       \* Its node had been handed the notification:
       [] node[t] = "granted" ->
            \* the node is gone.
            /\ node' = [node EXCEPT ![t] = "none"]
            \* Under the bug CancelNoPassOn
            /\ IF Mutant = "CancelNoPassOn"
                 \* the task just goes idle,
                 THEN /\ pc' = [pc EXCEPT ![t] = "idle"]
                      \* keeping the notification.
                      /\ UNCHANGED <<avail, held, ret>>
                 \* In the design the notification goes back into the store,
                 ELSE /\ avail' = Min(1, avail + held[t])
                      \* the task holds nothing,
                      /\ held' = [held EXCEPT ![t] = 0]
                      \* and it drains, so the next waiter gets it.
                      /\ DrainThen(t, "idle")
       \* Its node was reached by a broadcast:
       [] OTHER ->
            \* "broadcast": notified with no permit, so nothing to pass on.
            \* a node still in the queue stays marked for the drainer; otherwise it is forgotten;
            /\ node' = [node EXCEPT ![t] = IF InFifo(t) THEN "broadcast" ELSE "none"]
            \* the task drains, then is idle.
            /\ DrainThen(t, "idle")
            \* Nothing was held.
            /\ UNCHANGED <<avail, held>>
  \* Nothing else changes.
  /\ UNCHANGED <<waiters, fifo, token, hand, target, bgen, bwake>>

----------------------------------------------------------------------------
\* Notifying.

\* notify_one: store the permit, then drain, so a waiting task is handed
\* it. One that finds a permit stored changes no word (notifications
\* coalesce); the call that stored it drains for it, so it is not a step.
NotifyOne(t) ==
  \* The task is a notifier.
  /\ t \in Notifiers
  \* It is idle.
  /\ pc[t] = "idle"
  \* No notification is stored yet.
  /\ avail = 0
  \* One is stored now.
  /\ avail' = 1
  \* The task drains, so a waiting task is handed it.
  /\ DrainThen(t, "idle")
  \* Nothing else changes.
  /\ UNCHANGED <<waiters, fifo, node, woken, held, token, hand, target, line, bgen, gen, bwake>>

\* notify_waiters: bump the generation and CAS every waiting node to
\* "broadcast", storing nothing; then wake each of them and drain.
NotifyWaiters(t) ==
  \* The task is a notifier.
  /\ t \in Notifiers
  \* It is idle.
  /\ pc[t] = "idle"
  \* The broadcast budget is not used up.
  /\ bgen < MaxBcast
  \* live: the waiters whose nodes are in the queue and still waiting.
  /\ LET live == {u \in Tasks : InFifo(u) /\ node[u] = "waiting"}
     \* The broadcast generation moves on.
     IN /\ bgen' = bgen + 1
        \* Each of their nodes is marked "broadcast".
        /\ node' = [u \in Tasks |-> IF u \in live THEN "broadcast" ELSE node[u]]
        \* The task will wake each of them.
        /\ bwake' = [bwake EXCEPT ![t] = live]
        \* They leave the line.
        /\ line' = SelectSeq(line, LAMBDA u : u \notin live)
  \* The task goes on to wake them.
  /\ pc' = [pc EXCEPT ![t] = "bwake"]
  \* Nothing else changes; nothing is stored.
  /\ UNCHANGED <<avail, waiters, fifo, woken, held, token, hand, target, ret, gen>>

\* The broadcaster wakes the waiters it notified, one per step; then drains.
BWake(t) ==
  \* The task is waking the waiters it marked.
  /\ pc[t] = "bwake"
  \* None left?
  /\ IF bwake[t] = {}
       \* Then it drains, and is idle afterwards.
       THEN /\ DrainThen(t, "idle")
            \* Nobody is woken.
            /\ UNCHANGED <<woken, bwake>>
       \* Otherwise it picks one waiter u:
       ELSE \E u \in bwake[t] :
              \* u is woken,
              /\ woken' = [woken EXCEPT ![u] = TRUE]
              \* and leaves the list.
              /\ bwake' = [bwake EXCEPT ![t] = @ \ {u}]
              \* The task stays here for the rest.
              /\ UNCHANGED <<pc, ret>>
  \* Nothing else changes.
  /\ UNCHANGED <<avail, waiters, fifo, node, held, token, hand, target, line, bgen, gen>>

----------------------------------------------------------------------------
\* The drain: the only code that pops the queue and hands permits over.

\* Take the drain token if it is free; if another task holds it, leave.
DTake(t) ==
  \* The task is about to try the token.
  /\ pc[t] = "dtake"
  \* Is the token free?
  /\ IF token = NoTask
       \* Yes: it takes it,
       THEN /\ token' = t
            \* and starts serving the queue.
            /\ pc' = [pc EXCEPT ![t] = "dloop"]
            \* It still remembers where to go on afterwards.
            /\ UNCHANGED ret
       \* No: it leaves and goes where it meant to go.
       ELSE /\ Resume(t)
            \* The token stays with its holder.
            /\ UNCHANGED token
  \* Nothing else changes.
  /\ UNCHANGED <<avail, waiters, fifo, node, woken, held, hand, target, line, bgen, gen, bwake>>

\* Holding the token, look at the head: pop a stale node; debit the stored
\* permit for a waiting head; otherwise stop. Mutant WakeBeforeHandoff: wake
\* the head before handing it anything.
DLoop(t) ==
  \* The task holds the token and looks at the queue.
  /\ pc[t] = "dloop"
  \* The queue is empty:
  /\ IF fifo = <<>>
       \* the task goes on to let the token go.
       THEN /\ pc' = [pc EXCEPT ![t] = "dfree"]
            \* Nothing else changes.
            /\ UNCHANGED <<avail, waiters, fifo, node, hand, target>>
       \* Otherwise h is the head.
       ELSE LET h == Head(fifo) IN
            \* The head's node is stale (cancelled, or reached by a broadcast):
            IF node[h] # "waiting"
              \* it is popped,
              THEN /\ fifo' = Tail(fifo)
                   \* one waiter fewer is counted,
                   /\ waiters' = waiters - 1
                   \* and its node is forgotten,
                   /\ node' = [node EXCEPT ![h] =
                                 \* unless a broadcast marked it and its task has not been polled
                                 \* yet.
                                 IF node[h] = "broadcast" /\ Waits(h) THEN "broadcast" ELSE "none"]
                   \* The task looks again.
                   /\ UNCHANGED <<pc, avail, hand, target>>
              \* The head is waiting and a notification is stored:
              ELSE IF avail >= 1
                \* the notification leaves the store,
                THEN /\ avail' = avail - 1
                     \* into the drainer's hand,
                     /\ hand' = [hand EXCEPT ![t] = 1]
                     \* for this head,
                     /\ target' = [target EXCEPT ![t] = h]
                     \* and the drainer goes on to hand it over
                     /\ pc' = [pc EXCEPT ![t] =
                                 \* (the bug WakeBeforeHandoff wakes first).
                                 IF Mutant = "WakeBeforeHandoff" THEN "dwake" ELSE "dgrant"]
                     \* The queue is untouched.
                     /\ UNCHANGED <<waiters, fifo, node>>
                \* The head is waiting and nothing is stored: the drainer goes on to let the token
                \* go.
                ELSE /\ pc' = [pc EXCEPT ![t] = "dfree"]
                     \* Nothing else changes.
                     /\ UNCHANGED <<avail, waiters, fifo, node, hand, target>>
  \* The woken bits, the holdings, the token and the ghosts are untouched.
  /\ UNCHANGED <<woken, held, token, ret, line, bgen, gen, bwake>>

\* The handoff: CAS the target's node from "waiting" to "granted"; on
\* success it holds the permit and is popped, before anyone wakes it. If a
\* cancel or a broadcast won the CAS, the permit goes back.
DGrant(t) ==
  \* The drainer is about to hand over.
  /\ pc[t] = "dgrant"
  \* h: the waiter it serves.
  /\ LET h == target[t] IN
     \* Is that waiter's node still waiting?
     IF node[h] = "waiting"
       \* Yes: the node is marked granted.
       THEN /\ node' = [node EXCEPT ![h] = "granted"]
            \* The waiter holds the notification.
            /\ held' = [held EXCEPT ![h] = hand[t]]
            \* The drainer's hand is empty.
            /\ hand' = [hand EXCEPT ![t] = 0]
            \* The node leaves the queue.
            /\ fifo' = Tail(fifo)
            \* One waiter fewer is counted.
            /\ waiters' = waiters - 1
            \* Under the bug WakeBeforeHandoff (the wake already happened),
            /\ IF Mutant = "WakeBeforeHandoff"
                 \* the drainer looks at the queue again,
                 THEN /\ pc' = [pc EXCEPT ![t] = "dloop"]
                      \* serving nobody.
                      /\ target' = [target EXCEPT ![t] = NoTask]
                 \* In the design the drainer goes on to wake the waiter,
                 ELSE /\ pc' = [pc EXCEPT ![t] = "dwake"]
                      \* still serving it.
                      /\ UNCHANGED target
            \* The store is untouched.
            /\ UNCHANGED avail
       \* No (a cancel or a broadcast got there first): the notification goes back into the store,
       ELSE /\ avail' = Min(1, avail + hand[t])
            \* the drainer's hand is empty,
            /\ hand' = [hand EXCEPT ![t] = 0]
            \* it serves nobody,
            /\ target' = [target EXCEPT ![t] = NoTask]
            \* and it looks at the queue again.
            /\ pc' = [pc EXCEPT ![t] = "dloop"]
            \* Nothing else changes.
            /\ UNCHANGED <<node, held, fifo, waiters>>
  \* The woken bits, the token and the ghosts are untouched.
  /\ UNCHANGED <<woken, token, ret, line, bgen, gen, bwake>>

\* Waker::wake for the waiter just handed the permit.
DWake(t) ==
  \* The drainer is about to wake.
  /\ pc[t] = "dwake"
  \* The waiter it serves is woken.
  /\ woken' = [woken EXCEPT ![target[t]] = TRUE]
  \* Under the bug WakeBeforeHandoff
  /\ IF Mutant = "WakeBeforeHandoff"
       \* the handover comes only now, after the wake.
       THEN pc' = [pc EXCEPT ![t] = "dgrant"] /\ UNCHANGED target
       \* In the design the drainer looks at the queue again,
       ELSE /\ pc' = [pc EXCEPT ![t] = "dloop"]
            \* serving nobody.
            /\ target' = [target EXCEPT ![t] = NoTask]
  \* Nothing else changes.
  /\ UNCHANGED <<avail, waiters, fifo, node, held, token, hand, ret, line, bgen, gen, bwake>>

\* Let the token go and look again: work left behind by a task that found
\* the token taken is picked up here.
DFree(t) ==
  \* The drainer is about to let the token go.
  /\ pc[t] = "dfree"
  \* The token is free.
  /\ token' = NoTask
  \* Is there work left (something a task that found the token taken left behind)?
  /\ IF Work
       \* Yes: try the token again.
       THEN pc' = [pc EXCEPT ![t] = "dtake"] /\ UNCHANGED ret
       \* No: go where the task meant to go.
       ELSE Resume(t)
  \* Nothing else changes.
  /\ UNCHANGED <<avail, waiters, fifo, node, woken, held, hand, target, line, bgen, gen, bwake>>

----------------------------------------------------------------------------
\* The specification.

\* The start: no permit stored, nobody waiting.
Init ==
  \* No notification is stored.
  /\ avail   = 0
  \* Nobody is registered.
  /\ waiters = 0
  \* The queue is empty.
  /\ fifo    = <<>>
  \* No task has a node.
  /\ node    = [t \in Tasks |-> "none"]
  \* Nobody was woken.
  /\ woken   = [t \in Tasks |-> FALSE]
  \* Nobody holds a notification.
  /\ held    = [t \in Tasks |-> 0]
  \* The drain token is free.
  /\ token   = NoTask
  \* No drainer has a notification in hand.
  /\ hand    = [t \in Tasks |-> 0]
  \* No drainer serves anybody.
  /\ target  = [t \in Tasks |-> NoTask]
  \* Every task would go on to idle.
  /\ ret     = [t \in Tasks |-> "idle"]
  \* Every task is idle.
  /\ pc      = [t \in Tasks |-> "idle"]
  \* Nobody is in line.
  /\ line    = <<>>
  \* No broadcast happened.
  /\ bgen    = 0
  \* No waiter has seen a generation.
  /\ gen     = [t \in Tasks |-> 0]
  \* No broadcaster has anybody to wake.
  /\ bwake   = [t \in Tasks |-> {}]

\* The steps a task takes on its own once started: they must happen.
Internal(t) ==
  \* A waiter's push and poll, and a broadcaster's wakes.
  \/ Push(t) \/ Poll(t) \/ BWake(t)
  \* The drain's steps.
  \/ DTake(t) \/ DLoop(t) \/ DGrant(t) \/ DWake(t) \/ DFree(t)

\* Every step the system can take.
Next ==
  \* Some task
  \E t \in Tasks :
    \* starts waiting, drops its wait, notifies one, notifies all, or takes one of its own steps.
    Acquire(t) \/ Cancel(t) \/ NotifyOne(t) \/ NotifyWaiters(t) \/ Internal(t)

\* Fairness: a started operation finishes and a woken future is polled.
\* Waiting, cancelling and notifying are the caller's choices.
Fairness == \A t \in Tasks : WF_vars(Internal(t))

\* Every behaviour: start in Init, take Next steps or stutter, fairly.
Spec == Init /\ [][Next]_vars /\ Fairness

----------------------------------------------------------------------------
\* Invariants.

\* Every variable holds what its comment says.
TypeOK ==
  \* At most one notification is stored.
  /\ avail \in 0..1
  \* No more waiters than tasks.
  /\ waiters \in 0..Cardinality(Tasks)
  \* The queue holds tasks.
  /\ \A i \in 1..Len(fifo) : fifo[i] \in Tasks
  \* Every node is in one of the node states.
  /\ node \in [Tasks -> NodeStates]
  \* Every woken bit is a yes or a no.
  /\ woken \in [Tasks -> BOOLEAN]
  \* A task holds at most one notification.
  /\ held \in [Tasks -> 0..1]
  \* The token is with a task or free.
  /\ token \in Tasks \cup {NoTask}
  \* A drainer has at most one notification in hand.
  /\ hand \in [Tasks -> 0..1]
  \* Every task is at one of the known places.
  /\ pc \in [Tasks -> PCs]
  \* No more broadcasts than the budget.
  /\ bgen \in 0..MaxBcast

\* `waiters` counts the registered tasks not yet pushed plus the queued
\* nodes, so the fast path is shut whenever anyone waits.
\* Rules out a waiter that is queued but not counted: a newcomer would then take a stored
\* notification past it.
WaitersCounted == waiters = Cardinality({t \in Tasks : pc[t] = "push"}) + Len(fifo)

\* Only the token holder is inside the drain, and a drainer granting a head
\* still has it at the head of the queue.
DrainExclusive ==
  \* A task is inside the drain exactly when it holds the token.
  /\ \A t \in Tasks : pc[t] \in {"dloop", "dgrant", "dwake", "dfree"} <=> token = t
  \* A drainer about to hand over still has its waiter at the front of the queue.
  /\ \A t \in Tasks : pc[t] = "dgrant" => fifo # <<>> /\ Head(fifo) = target[t]

\* FIFO HANDOFF. No task holds a notification it got after a task still
\* waiting got in line. Lean: SyncFifo.fifo_served.
FifoHandoff ==
  \* For every two tasks in line, the first ahead of the second:
  \A i, j \in 1..Len(line) :
    \* if the first is still waiting, the second holds no notification. Rules out, for example:
    \* tasks 1 and 2 wait, in that order, one notify_one arrives, and task 2 gets it.
    i < j /\ node[line[i]] = "waiting" => held[line[j]] = 0

\* A parked future handed a notification has been woken, or is about to be.
\* Lean: SyncFifo.handed_holds.
GrantedWoken ==
  \* For every task
  \A t \in Tasks :
    \* that sleeps with its node marked granted or broadcast:
    pc[t] = "parked" /\ node[t] \in {"granted", "broadcast"} =>
      \* it was woken,
      \/ woken[t]
      \* or a drainer or a broadcaster is just about to wake it.
      \/ \E d \in Tasks : (pc[d] = "dwake" /\ target[d] = t) \/ (pc[d] = "bwake" /\ t \in bwake[d])

\* Every task is between operations.
Quiescent == \A t \in Tasks : pc[t] \in {"idle", "parked"}

\* A live waiter that a drain would serve (every node ahead of it stale, a
\* permit stored), with nobody draining.
Stranded ==
  \* Some queued node
  \E i \in 1..Len(fifo) :
    \* is still waiting,
    /\ node[fifo[i]] = "waiting"
    \* is the first waiting one (everything ahead of it is stale),
    /\ \A j \in 1..(i - 1) : node[fifo[j]] # "waiting"
    \* and a notification is stored.
    /\ avail >= 1

\* A waiting node registered before a broadcast was not notified.
MissedBroadcast == \E t \in Tasks : node[t] = "waiting" /\ gen[t] < bgen

\* NO LOST WAKEUP. A handed notification is followed by a wake, no waiter
\* misses a broadcast, and between operations nobody waits beside a stored
\* permit. Lean: SyncFifo.no_stranded_waiter.
\* Rules out, for example: notify_one stores its notification in the moment between a waiter's check
\* and its registration, finds nobody to hand it to, and the waiter then sleeps beside it for ever.
NoLostWakeup == GrantedWoken /\ ~MissedBroadcast /\ (Quiescent => ~Stranded)

\* CANCELLATION PASSES ON. An idle task holds no notification.
\* Rules out, for example: a waiter is handed a notification, its future is dropped before it runs,
\* and the notification is lost although another waiter is still waiting for one.
CancelPassesOn == \A t \in Tasks : pc[t] = "idle" => held[t] = 0

----------------------------------------------------------------------------
\* Liveness.

\* A waiter handed a notification completes. (A waiter nobody notifies may
\* wait forever.) Checked for the tasks in Watched: waiters are
\* interchangeable, so one stands for all.
NotifiedCompletes ==
  \* For every watched task:
  \A t \in Watched :
    \* once it sleeps with a notification handed to it, it stops sleeping.
    (pc[t] = "parked" /\ node[t] \in {"granted", "broadcast"}) ~> (pc[t] # "parked")

====
