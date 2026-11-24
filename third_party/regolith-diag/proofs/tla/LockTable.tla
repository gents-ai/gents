---- MODULE LockTable ----
\* ===========================================================================
\* THE STORY
\* ===========================================================================
\* A pessimistic transaction in regolith locks a key before it reads or
\* writes it, so two transactions can never change one key at the same time.
\* The locks live in a table: one small lock per key that is locked or waited
\* for right now (src/transaction/lock_table.rs). Nothing in it is a mutex:
\* taking a lock is one compare-and-swap that writes the taker's name into
\* the lock's state word (src/sync/owned_lock.rs).
\*
\* A transaction that finds the lock taken can do two things:
\*   - try_get_for_update returns at once with a "wait" (a LockWait). The
\*     transaction's thread carries on with other work. When the lock may be
\*     free, a note is dropped into that thread's own inbox (its IoQueue),
\*     and the wait becomes ready the next time the thread looks at its
\*     inbox. Then it calls again;
\*   - get_for_update goes to sleep (the thread is parked) and is woken when
\*     the lock may be free. It gives up after a timeout.
\* Both keep the transaction's place in the lock's waiting line.
\*
\* Who gets a lock that was just released? Whoever asks first, even if others
\* are waiting in line ("barging"): a running thread never waits for a
\* sleeping one. To stop a waiter starving, each time the front waiter is
\* told "try now" and finds the lock gone again, it counts one loss. At
\* Bound losses (MAX_BYPASS in the code) it is "owed": nobody may barge any
\* more, and the next release hands the lock straight to it.
\*
\* Tiny example. Three transactions, one key, Bound = 1.
\*   1 takes the lock. 2 asks, finds it taken, and joins the line.
\*   1 releases; 2 gets a note "try now". Before 2's thread reads the note,
\*   3 asks and takes the free lock (barging).
\*   2 tries, loses: one loss, which is the bound, so 2 is owed.
\*   3 releases: the lock goes straight to 2. Nobody could barge in between.
\*
\* A commit made with commit_nowait is written first and becomes visible
\* later, when it "lands" (src/transaction/nowait.rs). Its locks are
\* released by the thread that lands it, at that moment. Not earlier, or the
\* next holder would read the key without the commit in it. Not later, at
\* the moment the committer's own thread hears about the outcome, or
\* everybody would wait for that thread to look at its inbox.
\*
\* What can go wrong, and what this model checks cannot happen:
\*   - two transactions hold one key (MutualExclusion), also when the key's
\*     lock is taken out of the table while somebody is about to use it
\*     (TableSound), and also when a call believes it holds the key and the
\*     name in the lock says otherwise. A handoff is two steps here, as in
\*     the code: the releaser writes the waiter's name, then grants the
\*     waiter. The name carries a mark in between, so the waiter's own poll
\*     never takes it for a hold (defect F1: it did, withdrew, and the grant
\*     then cleared the name under a call that said "held");
\*   - the lock is free, somebody waits, and nobody tells them
\*     (NoStrandedFront); or somebody is told and the note never reaches
\*     them (WokenIsTold);
\*   - a waiter loses more than Bound races (BoundedBypass);
\*   - a waiter's note lands in another thread's inbox, or its wait is
\*     completed by another thread (OwnQueue);
\*   - somebody takes the key while a commit to it is written but not yet
\*     visible (IsolationAtLanding), or a commit keeps its lock after it
\*     landed (HeldOnlyWhileNeeded);
\*   - and, over whole runs: everybody who asks is served or gives up
\*     (Served), even if a committer never looks at its inbox.
\*
\* PROVED FOR EVERY SIZE in Lean, proofs/lean/Regolith/LockTable.lean.
\*
\* WHAT IS MODELLED, and the code each piece mirrors:
\*   cur, uses     the table's one entry for the key and its use count:
\*                 LockTable::keys and KeyLock::uses, lock_table.rs
\*   holder        the name in the lock's state word: OwnedLock, owned_lock.rs
\*   fifo, nudged, the lock's waiting line and its "try now" marks: WaitQueue,
\*   owed, lost    Waiter and Contend in src/sync (queue.rs, waiter.rs,
\*                 contend.rs)
\*   note          the completion a call recorded for itself before it
\*                 polled: PendingLock::note and sleep_here, lock_table.rs
\*   inbox, ready  a thread's IoQueue inbox and the LockWait's slot:
\*                 QueueShared::deliver and WaitSlot, src/engine/io/shared.rs
\*   pending       commits written and not yet visible: a slot of the
\*                 commit ring and the list of those to tell when it lands,
\*                 Descriptor::listen, src/engine/commit/descriptor.rs
\*
\* WHAT THE MODEL LEAVES OUT, and why that loses nothing.
\*   - One key. Each key has its own lock, and no step touches two.
\*   - One call per transaction at a time. A second thread of the same
\*     transaction reads its own name in the state word and returns, or
\*     shares the transaction's one wait for the key: LockTableCalls.tla
\*     checks two calls of one transaction, one wait between them, and a
\*     wake that falls inside one call's poll.
\*   - Close. It wakes every waiter and refuses new ones; the loom model
\*     no_lock_wait_outlives_close checks it against the real types.
\*   - The drain role (which thread runs a pass over the waiting line). A
\*     pass is one step here, except a handoff, which is two (Drain writes
\*     the name, Grant grants it); Sync.tla models the role step by step.
\*     The handoff a give-up passes on (Cancel) stays one step: it runs the
\*     same code as Drain's, whose two steps are checked.
\*   - Memory ordering is loom's (tests/loom_locks.rs).
\*
\* CONFIGURATIONS (MC_LockTable_*.cfg).
\*   Green_Try       three transactions that never wait, Bound 1.
\*   Green_Bound2    the same with Bound 2: the count reaches 2.
\*   Green_Blocking  one sleeps for the lock and may time out.
\*   Green_Nowait    commits land later; locks go at the landing.
\*   Green_Cancel    waiters may give up at any time.
\*   Green_Table     two entry names, so a retired entry is told apart.
\*   Green_Mixed     a sleeper, a commit_nowait and a give-up at once:
\*                   every invariant, five lock calls.
\*   Green_MixedLive the same with four lock calls: everybody is served.
\*   Green_Four      four transactions, three in line: every invariant.
\*   Green_EarlyPoll an owed waiter that may call again untold, the moment
\*                   between a handoff's two steps included (defect F1).
\*   Red_RetireNoCas, Red_NoRecheck, Red_ReleaseNoWake, Red_NoteAfterPoll,
\*   Red_CancelNoPassOn, Red_UnboundedBarging, Red_WrongQueue,
\*   Red_CompleteOnWaker, Red_ReleaseAtWrite, Red_ReleaseAtDelivery,
\*   Red_MineBeforeGrant: one planted bug each; the comment in each file
\*   tells its story.

\* Numbers, lists and set sizes come from these standard modules.
EXTENDS Naturals, Sequences, FiniteSets

\* The things a configuration chooses.
CONSTANTS
  \* The transactions: positive numbers. Each runs on its own thread, and
  \* that thread's inbox (IoQueue) has the same number.
  Txns,
  \* Names for table entries (one KeyLock allocation each). An entry's name
  \* can be used again once nobody refers to it, like freed memory.
  Entries,
  \* The transactions that sleep for a lock (get_for_update, put, delete,
  \* merge). The others use try_get_for_update and their own inbox.
  Blocking,
  \* The transactions that commit with commit_nowait.
  Nowait,
  \* The transactions that may give a wait up: they end without the lock,
  \* or their sleep times out.
  MayCancel,
  \* MAX_BYPASS: how many races a waiter may lose before it is owed.
  Bound,
  \* How many lock calls may start in a whole run, so the run is finite.
  Takes,
  \* The transactions whose wait must end, checked over whole runs.
  Watched,
  \* The transactions that may call again before they are told: a caller
  \* of try_get_for_update may, and a sleep that timed out does.
  Early,
  \* "none" for the real code, or the name of one planted bug.
  Mutant

\* The number 0 stands for "nobody" and "no entry".
None == 0
\* The number 0 also stands for "a thread other than the waiter's own".
NotOwn == 0
\* Some other transaction (only the WrongQueue bug uses it).
Other(t) == CHOOSE o \in Txns : o # t

\* Transactions and entries are numbered from 1, so 0 can mean "none".
ASSUME Txns \subseteq Nat \ {0} /\ Entries \subseteq Nat \ {0}
\* The special groups of transactions are made of transactions.
ASSUME Blocking \subseteq Txns /\ Nowait \subseteq Txns
\* So are the ones that may give up, the ones we watch and the early ones.
ASSUME MayCancel \subseteq Txns /\ Watched \subseteq Txns /\ Early \subseteq Txns
\* The bound is at least 1, and the budget is a number.
ASSUME Bound \in Nat \ {0} /\ Takes \in Nat
\* The bug names this model knows.
ASSUME Mutant \in {"none", "RetireNoCas", "NoRecheck", "ReleaseNoWake",
                   "NoteAfterPoll", "CancelNoPassOn", "UnboundedBarging",
                   "WrongQueue", "CompleteOnWaker", "ReleaseAtWrite",
                   "ReleaseAtDelivery", "MineBeforeGrant"}

\* The state that changes from step to step.
VARIABLES
  \* The entry the table holds for the key, or None when the key has none
  \* (LockTable::keys).
  cur,
  \* [Entries -> Nat]: how many transactions use each entry: its holder,
  \* each waiter, each call between its lookup and its outcome
  \* (KeyLock::uses).
  uses,
  \* [Entries -> Txns \cup {None}]: the name in each entry's state word: who
  \* holds the lock (OwnedLock::holder).
  holder,
  \* [Entries -> Seq(Txns)]: each entry's waiting line, oldest first.
  fifo,
  \* [Entries -> Txns \cup {None}]: the waiter that reached the bound and is
  \* owed the next release (the HANDOFF bit and the waiter's owed mark).
  owed,
  \* [Entries -> Txns \cup {None}]: the waiter whose name a handoff wrote into
  \* the state word and has not granted yet (the GRANTING mark beside the
  \* name, owned_lock.rs). While it is set the name is nobody's hold.
  granting,
  \* [Txns -> Entries \cup {None}]: the entry each transaction looked up and
  \* still refers to (its Arc<KeyLock>).
  pin,
  \* [Txns -> a step name]: where each transaction is in its lock call.
  pc,
  \* [Txns -> BOOLEAN]: a release asked this waiter to try again and it has
  \* not tried yet (the NUDGED bit of its waiter node).
  nudged,
  \* [Txns -> Nat]: races this waiter lost since it joined the line
  \* (Contend::bypass).
  lost,
  \* [Txns -> BOOLEAN]: the call recorded how it is to be told, and nobody
  \* has told it yet (a note in PendingLock's Wakeup).
  note,
  \* [Txns -> SUBSET Txns]: each thread's inbox: the waits whose "the lock
  \* may be free" note was pushed there and not taken in yet.
  inbox,
  \* [Txns -> BOOLEAN]: the LockWait a transaction was handed is ready.
  ready,
  \* [Txns -> Txns \cup {NotOwn}]: ghost, the thread that made it ready.
  readyBy,
  \* [Txns -> BOOLEAN]: a sleeping transaction's thread was unparked.
  unparked,
  \* The transactions whose commit is written and not yet visible.
  pending,
  \* The transactions whose commit outcome their own thread has not heard.
  undelivered,
  \* How many more lock calls may start.
  budget

\* Every variable, so a step that changes none of them is a stutter.
vars == <<cur, uses, holder, fifo, owed, granting, pin, pc, nudged, lost, note,
          inbox, ready, readyBy, unparked, pending, undelivered, budget>>

------------------------------------------------------------------------------
\* SMALL HELPERS

\* Transaction t holds the key, through whichever entry.
Holds(t) == \E e \in Entries : holder[e] = t
\* Transaction t holds the key by a finished grant or its own take: its name
\* is in the state word and no handoff to it is half made.
Granted(t) == \E e \in Entries : holder[e] = t /\ granting[e] # t
\* Transaction t owns the key: by its name, or because its call returned
\* "held" and it has not let go (holding, or committing what it holds).
Owns(t) == Holds(t) \/ pc[t] \in {"holding", "written", "landed", "kept"}
\* A caller may take entry e right now: nobody holds it and nobody is owed.
Free(e) == holder[e] = None /\ owed[e] = None
\* Transaction t uses entry e: it refers to it and has not given its use
\* back yet (at "retire" the use is already given back).
Using(t, e) == pin[t] = e /\ pc[t] # "retire"
\* The line q without transaction t.
Without(q, t) == SelectSeq(q, LAMBDA x : x # t)
\* The step names in which a transaction is asking for the lock.
Asking == {"busy", "noted", "enqueue", "parking", "lateNote", "waiting",
           "renoted"}

\* A wake reaches waiter w only if w has returned from its call with a note
\* recorded: then the note is delivered. A waiter still inside its call
\* sees the nudge or the handoff itself when it looks.
Go(w) == w # None /\ pc[w] = "waiting" /\ note[w]
\* The notes after waking w: w's note is used up.
NoteT(w) == IF Go(w) THEN [note EXCEPT ![w] = FALSE] ELSE note
\* The unparked flags after waking w: a sleeper's thread is unparked.
UnparkedT(w) == IF Go(w) /\ w \in Blocking
                \* A sleeper that is woken: its flag goes up. Anybody else: no flag changes.
                THEN [unparked EXCEPT ![w] = TRUE] ELSE unparked
\* The inboxes after waking w: the note goes into w's OWN inbox. The
\* WrongQueue bug puts it into another thread's; the CompleteOnWaker bug
\* skips the inbox altogether.
InboxT(w) == IF Go(w) /\ w \notin Blocking /\ Mutant # "CompleteOnWaker"
             \* A polling waiter that is woken: w's name is added to its own inbox (in the
             \* WrongQueue bug, to the other thread's).
             THEN [inbox EXCEPT ![IF Mutant = "WrongQueue" THEN Other(w) ELSE w]
                     = @ \cup {w}]
             \* Nobody is woken that way: every inbox stays as it is.
             ELSE inbox
\* The ready flags after waking w: nothing, in the real code (only w's own
\* poll makes its wait ready). The CompleteOnWaker bug makes it ready here.
ReadyT(w) == IF Go(w) /\ w \notin Blocking /\ Mutant = "CompleteOnWaker"
             \* Only in the bug: w's wait is made ready by the waker. Otherwise no flag changes.
             THEN [ready EXCEPT ![w] = TRUE] ELSE ready
\* Who made it ready: in the bug, a thread that is not w's own.
ReadyByT(w) == IF Go(w) /\ w \notin Blocking /\ Mutant = "CompleteOnWaker"
               \* Only in the bug: the record says a thread that is not w's own did it.
               THEN [readyBy EXCEPT ![w] = NotOwn] ELSE readyBy
\* The nudge marks after nudging w.
NudgedT(w) == IF w # None THEN [nudged EXCEPT ![w] = TRUE] ELSE nudged

------------------------------------------------------------------------------
\* HOW IT STARTS

\* Nothing is locked, nobody waits, no entry is in the table.
Init ==
  \* The table has no entry for the key.
  /\ cur = None
  \* No entry is used.
  /\ uses = [e \in Entries |-> 0]
  \* No entry is held.
  /\ holder = [e \in Entries |-> None]
  \* No entry has a waiting line.
  /\ fifo = [e \in Entries |-> <<>>]
  \* Nobody is owed.
  /\ owed = [e \in Entries |-> None]
  \* No handoff is half made.
  /\ granting = [e \in Entries |-> None]
  \* No transaction refers to an entry.
  /\ pin = [t \in Txns |-> None]
  \* Every transaction is idle.
  /\ pc = [t \in Txns |-> "idle"]
  \* Nobody was nudged.
  /\ nudged = [t \in Txns |-> FALSE]
  \* Nobody lost a race.
  /\ lost = [t \in Txns |-> 0]
  \* Nobody recorded a note.
  /\ note = [t \in Txns |-> FALSE]
  \* Every inbox is empty.
  /\ inbox = [t \in Txns |-> {}]
  \* No wait is ready.
  /\ ready = [t \in Txns |-> FALSE]
  \* So nobody made one ready; each slot starts as its owner's.
  /\ readyBy = [t \in Txns |-> t]
  \* No thread was unparked.
  /\ unparked = [t \in Txns |-> FALSE]
  \* No commit is waiting to become visible.
  /\ pending = {}
  \* No commit outcome is waiting to be heard.
  /\ undelivered = {}
  \* The whole budget of lock calls is left.
  /\ budget = Takes

------------------------------------------------------------------------------
\* TAKING THE LOCK

\* A transaction starts a lock call: it looks the key up in the table and
\* takes a use of its entry, or makes the entry if the key has none
\* (LockTable::pin).
Lookup(t) ==
  \* It is not in a call already.
  /\ pc[t] = "idle"
  \* The run has a call left.
  /\ budget > 0
  \* Its last commit is behind it: landed, and heard by its own thread.
  /\ t \notin pending /\ t \notin undelivered
  \* One call fewer is left.
  /\ budget' = budget - 1
  \* It now tries the fast path.
  /\ pc' = [pc EXCEPT ![t] = "pinned"]
  \* Either the key has an entry ...
  /\ IF cur # None
       \* ... and then:
       THEN
            \* the transaction refers to it,
            /\ pin' = [pin EXCEPT ![t] = cur]
            \* and takes one more use of it.
            /\ uses' = [uses EXCEPT ![cur] = @ + 1]
            \* The table and the lock itself do not change.
            /\ UNCHANGED <<cur, holder, fifo, owed>>
       \* ... or it has none, and a new entry is made.
       ELSE
            \* The names nobody refers to are free:
            LET free == {e \in Entries : \A u \in Txns : pin[u] # e}
                \* the lowest of them is the new entry's name.
                e == CHOOSE x \in free : \A y \in free : x <= y
            \* With that name:
            IN
                \* (there must be a free name)
                /\ free # {}
                \* The table now holds the new entry for the key.
                /\ cur' = e
                \* The transaction refers to it,
                /\ pin' = [pin EXCEPT ![t] = e]
                \* and is its one user (KeyLock::new starts at 1).
                /\ uses' = [uses EXCEPT ![e] = 1]
                \* A new lock is held by nobody,
                /\ holder' = [holder EXCEPT ![e] = None]
                \* has nobody waiting,
                /\ fifo' = [fifo EXCEPT ![e] = <<>>]
                \* and owes nobody.
                /\ owed' = [owed EXCEPT ![e] = None]
  \* Nothing else changes.
  /\ UNCHANGED <<granting, nudged, lost, note, inbox, ready, readyBy, unparked, pending,
                 undelivered>>

\* The fast path: one compare-and-swap that takes a free lock and writes
\* the taker's name (OwnedLock::try_lock). It works even if others wait.
TryFast(t) ==
  \* The transaction has just looked the entry up.
  /\ pc[t] = "pinned"
  \* If the lock is free and nobody is owed ...
  /\ IF Free(pin[t])
       \* ... the transaction takes it:
       THEN
            \* its name goes into the state word,
            /\ holder' = [holder EXCEPT ![pin[t]] = t]
            \* and it holds the lock.
            /\ pc' = [pc EXCEPT ![t] = "holding"]
       \* ... otherwise it does not:
       ELSE
            \* the lock is as it was,
            /\ UNCHANGED holder
            \* and the transaction will have to wait.
            /\ pc' = [pc EXCEPT ![t] = "busy"]
  \* Nothing else changes.
  /\ UNCHANGED <<granting, cur, uses, fifo, owed, pin, nudged, lost, note, inbox, ready,
                 readyBy, unparked, pending, undelivered, budget>>

\* Before it polls, the call records how it is to be told: a completion for
\* its inbox, or its thread to unpark (PendingLock::note, sleep_here). The
\* NoteAfterPoll bug records it only after the poll.
Note(t) ==
  \* The fast path failed.
  /\ pc[t] = "busy"
  \* The note is recorded (not in the bug).
  /\ note' = [note EXCEPT ![t] = (Mutant # "NoteAfterPoll")]
  \* Next: the first poll.
  /\ pc' = [pc EXCEPT ![t] = "noted"]
  \* Nothing else changes.
  /\ UNCHANGED <<granting, cur, uses, holder, fifo, owed, pin, nudged, lost, inbox,
                 ready, readyBy, unparked, pending, undelivered, budget>>

\* The first poll starts with one more try (Contend::poll, arriving).
TryAgain(t) ==
  \* The note is recorded.
  /\ pc[t] = "noted"
  \* If the lock came free meanwhile ...
  /\ IF Free(pin[t])
       \* ... the transaction takes it:
       THEN
            \* its name goes into the state word,
            /\ holder' = [holder EXCEPT ![pin[t]] = t]
            \* its note is void (it does not wait after all),
            /\ note' = [note EXCEPT ![t] = FALSE]
            \* and it holds the lock.
            /\ pc' = [pc EXCEPT ![t] = "holding"]
       \* ... otherwise it does not:
       ELSE
            \* nothing is taken, and its note stays,
            /\ UNCHANGED <<holder, note>>
            \* and it will join the line.
            /\ pc' = [pc EXCEPT ![t] = "enqueue"]
  \* Nothing else changes.
  /\ UNCHANGED <<granting, cur, uses, fifo, owed, pin, nudged, lost, inbox, ready,
                 readyBy, unparked, pending, undelivered, budget>>

\* The transaction joins the back of the line, and the line is looked at
\* once more (the pass a new waiter triggers): if the lock is free right
\* now, the front is told to try. Without that look, a release that came
\* just before the transaction joined would be missed (the NoRecheck bug).
Enqueue(t) ==
  \* The second try failed.
  /\ pc[t] = "enqueue"
  \* The line with t at the back,
  /\ LET q == Append(fifo[pin[t]], t)
         \* and the waiter the look nudges. The look happens (not in the
         \* NoRecheck bug),
         w == IF /\ Mutant # "NoRecheck"
                 \* the lock is free and nobody is owed,
                 /\ Free(pin[t])
                 \* and the front is not nudged already:
                 /\ ~nudged[Head(q)]
              \* then it is the front; otherwise nobody.
              THEN Head(q) ELSE None
     \* With the line and the waiter to tell worked out:
     IN
         \* t is in the line.
         /\ fifo' = [fifo EXCEPT ![pin[t]] = q]
         \* The front is marked "try again".
         /\ nudged' = NudgedT(w)
         \* If the front is somebody already waiting, it is told (t itself
         \* is still inside its call, so nothing is delivered to it).
         /\ note' = NoteT(w)
         \* A sleeper is unparked.
         /\ unparked' = UnparkedT(w)
         \* A polling waiter gets the note in its own inbox.
         /\ inbox' = InboxT(w)
         \* Only the planted bug touches the ready flags here.
         /\ ready' = ReadyT(w)
         \* And who made it ready.
         /\ readyBy' = ReadyByT(w)
  \* Next: t looks at its own place in the line.
  /\ pc' = [pc EXCEPT ![t] = "parking"]
  \* Nothing else changes.
  /\ UNCHANGED <<granting, cur, uses, holder, owed, pin, lost, pending, undelivered,
                 budget>>

\* Still inside the first poll, the transaction looks at its place in the
\* line (Wait::park). Handed the lock: done. Nudged: one try, and a loss in
\* this first poll is not counted (nobody passed it yet). Else it returns
\* and waits.
Park(t) ==
  \* It has just joined the line.
  /\ pc[t] = "parking"
  \* Where it goes when it has to wait: back to the caller. In the
  \* NoteAfterPoll bug it still has its note to record.
  /\ LET rest == IF Mutant = "NoteAfterPoll" THEN "lateNote" ELSE "waiting"
     \* With that worked out, the transaction looks at the lock.
     IN
         \* Was the lock handed to it while it joined (its grant through)?
         IF holder[pin[t]] = t /\ granting[pin[t]] # t
           \* Yes: it holds the lock,
           THEN
                \* so its call is done,
                /\ pc' = [pc EXCEPT ![t] = "holding"]
                \* its note is void,
                /\ note' = [note EXCEPT ![t] = FALSE]
                \* and the lock and the line are as the handoff left them.
                /\ UNCHANGED <<holder, fifo, nudged>>
           \* No. Was it nudged, with the lock free?
           ELSE IF nudged[t] /\ Free(pin[t])
             \* Yes: it takes the lock,
             THEN
                  \* writing its name into the state word,
                  /\ holder' = [holder EXCEPT ![pin[t]] = t]
                  \* leaves the line,
                  /\ fifo' = [fifo EXCEPT ![pin[t]] = Without(@, t)]
                  \* the nudge is used up,
                  /\ nudged' = [nudged EXCEPT ![t] = FALSE]
                  \* its note is void,
                  /\ note' = [note EXCEPT ![t] = FALSE]
                  \* and it holds the lock.
                  /\ pc' = [pc EXCEPT ![t] = "holding"]
             \* No: it was not nudged, or it was nudged and beaten to the lock.
             ELSE
                  \* The nudge, if any, is used up,
                  /\ nudged' = [nudged EXCEPT ![t] = FALSE]
                  \* the lock and the line stay,
                  /\ UNCHANGED <<holder, fifo, note>>
                  \* and the call returns: the transaction waits.
                  /\ pc' = [pc EXCEPT ![t] = rest]
  \* Nothing else changes.
  /\ UNCHANGED <<granting, cur, uses, owed, pin, lost, inbox, ready, readyBy, unparked,
                 pending, undelivered, budget>>

\* Only in the NoteAfterPoll bug: the note is recorded after the poll. A
\* wake that came in between found nothing to deliver.
LateNote(t) ==
  \* The bug's extra step.
  /\ pc[t] = "lateNote"
  \* The note is recorded, too late.
  /\ note' = [note EXCEPT ![t] = TRUE]
  \* The call has returned.
  /\ pc' = [pc EXCEPT ![t] = "waiting"]
  \* Nothing else changes.
  /\ UNCHANGED <<granting, cur, uses, holder, fifo, owed, pin, nudged, lost, inbox,
                 ready, readyBy, unparked, pending, undelivered, budget>>

------------------------------------------------------------------------------
\* WAITING AND TRYING AGAIN

\* A thread looks at its inbox (IoQueue::poll): every note in it makes the
\* wait it names ready. This is the only place a wait becomes ready.
PollQueue(q) ==
  \* There is something in the inbox.
  /\ inbox[q] # {}
  \* Each wait named there is ready now.
  /\ ready' = [t \in Txns |-> IF t \in inbox[q] THEN TRUE ELSE ready[t]]
  \* And it was this thread, q, that made it ready.
  /\ readyBy' = [t \in Txns |-> IF t \in inbox[q] THEN q ELSE readyBy[t]]
  \* The inbox is empty again.
  /\ inbox' = [inbox EXCEPT ![q] = {}]
  \* Nothing else changes.
  /\ UNCHANGED <<granting, cur, uses, holder, fifo, owed, pin, pc, nudged, lost, note,
                 unparked, pending, undelivered, budget>>

\* Told that the lock may be free, the transaction calls again. The new
\* call first records a fresh note, as every call does.
Rerun(t) ==
  \* It is waiting, outside any call.
  /\ pc[t] = "waiting"
  \* Its wait is ready (a polling transaction), or its thread was unparked
  \* (a sleeping one); or it may call again untold, as a caller may at any
  \* time and a sleep that timed out does.
  /\ \/ t \in Early
     \* Otherwise only once told.
     \/ IF t \in Blocking THEN unparked[t] ELSE ready[t]
  \* The old wait is used up.
  /\ ready' = [ready EXCEPT ![t] = FALSE]
  \* So is the unpark.
  /\ unparked' = [unparked EXCEPT ![t] = FALSE]
  \* The new call's note is recorded before it polls.
  /\ note' = [note EXCEPT ![t] = TRUE]
  \* Next: the poll.
  /\ pc' = [pc EXCEPT ![t] = "renoted"]
  \* Nothing else changes.
  /\ UNCHANGED <<granting, cur, uses, holder, fifo, owed, pin, nudged, lost, inbox,
                 readyBy, pending, undelivered, budget>>

\* The poll of a transaction already in the line (Contend::poll). Handed
\* the lock: done. Nudged: one try; a loss is counted, and the loss that
\* reaches the bound makes it owed in the same step, so nobody barges
\* after it. Not nudged: it goes on waiting.
PollQueued(t) ==
  \* The call's note is recorded.
  /\ pc[t] = "renoted"
  \* The entry it waits on.
  /\ LET e == pin[t]
     \* Only in the MineBeforeGrant bug: the name in the state word is this
     \* transaction's, written by a handoff not granted yet, and the poll takes
     \* it for a hold another call of the transaction made. It withdraws from
     \* the line and the call says "held". The grant then finds it gone and
     \* clears the name: the transaction believes it holds a free lock.
     IN  IF Mutant = "MineBeforeGrant" /\ holder[e] = t /\ granting[e] = t
           \* The bug's step:
           THEN
                \* the wait leaves the line,
                /\ fifo' = [fifo EXCEPT ![e] = Without(@, t)]
                \* nobody is owed by it any more,
                /\ owed' = [owed EXCEPT ![e] = None]
                \* its nudge and its note go,
                /\ nudged' = [nudged EXCEPT ![t] = FALSE]
                \* (the note too),
                /\ note' = [note EXCEPT ![t] = FALSE]
                \* and the call returns as if the transaction held the key.
                /\ pc' = [pc EXCEPT ![t] = "holding"]
                \* The name itself is untouched, and nothing is counted.
                /\ UNCHANGED <<holder, lost>>
         \* Was the lock handed to it, its grant through?
         ELSE IF holder[e] = t /\ granting[e] # t
           \* Yes: it holds it,
           THEN
                \* so its call is done,
                /\ pc' = [pc EXCEPT ![t] = "holding"]
                \* its note is void,
                /\ note' = [note EXCEPT ![t] = FALSE]
                \* and nothing else about the lock changes.
                /\ UNCHANGED <<holder, fifo, owed, nudged, lost>>
           \* No. Was it nudged, with the lock free?
           ELSE IF nudged[t] /\ Free(e)
             \* Yes: it takes the lock,
             THEN
                  \* writing its name into the state word,
                  /\ holder' = [holder EXCEPT ![e] = t]
                  \* leaves the line,
                  /\ fifo' = [fifo EXCEPT ![e] = Without(@, t)]
                  \* the nudge is used up,
                  /\ nudged' = [nudged EXCEPT ![t] = FALSE]
                  \* its note is void,
                  /\ note' = [note EXCEPT ![t] = FALSE]
                  \* and it holds the lock.
                  /\ pc' = [pc EXCEPT ![t] = "holding"]
                  \* Nobody is owed, and the count no longer matters.
                  /\ UNCHANGED <<owed, lost>>
             \* No. Was it nudged at all, with nobody owed yet?
             ELSE IF nudged[t] /\ owed[e] = None
               \* Yes: it tried and was beaten to the lock.
               THEN
                    \* The nudge is used up,
                    /\ nudged' = [nudged EXCEPT ![t] = FALSE]
                    \* one more loss is counted,
                    /\ lost' = [lost EXCEPT ![t] = @ + 1]
                    \* and at the bound it is owed (not in the
                    \* UnboundedBarging bug, where nobody ever is).
                    /\ owed' = [owed EXCEPT ![e] =
                                  \* this loss is the Bound-th one,
                                  IF lost[t] + 1 = Bound /\ Mutant # "UnboundedBarging"
                                  \* so the owed waiter is t itself.
                                  THEN t ELSE None]
                    \* It keeps its place and its note, and waits.
                    /\ UNCHANGED <<holder, fifo, note>>
                    \* The call returns.
                    /\ pc' = [pc EXCEPT ![t] = "waiting"]
               \* No: it was not nudged, or somebody is owed.
               ELSE
                    \* Any nudge is used up without a count,
                    /\ nudged' = [nudged EXCEPT ![t] = FALSE]
                    \* nothing else changes,
                    /\ UNCHANGED <<holder, fifo, owed, lost, note>>
                    \* and the call returns: it waits.
                    /\ pc' = [pc EXCEPT ![t] = "waiting"]
  \* Nothing else changes.
  /\ UNCHANGED <<granting, cur, uses, pin, inbox, ready, readyBy, unparked, pending,
                 undelivered, budget>>

\* A waiting transaction gives up: it ends without the lock, or its sleep
\* timed out (PendingLock::abandon). A lock already handed to it is given
\* back. Otherwise it leaves the line, and what it carried is passed on: a
\* nudge it did not use goes to the new front, and if it was the owed one,
\* nobody is owed any more.
Cancel(t) ==
  \* It may give up, and it is waiting outside any call.
  /\ t \in MayCancel /\ pc[t] = "waiting"
  \* The entry it waits on,
  /\ LET e == pin[t]
         \* and that entry's line without t.
         q == Without(fifo[e], t)
     \* Was it handed the lock, its grant through, without ever taking it?
     \* (A handoff not granted yet is not its own: it withdraws, and the
     \* grant clears the name.)
     IN  IF holder[e] = t /\ granting[e] # t
           \* Yes: it lets go,
           THEN
                \* its name leaves the state word,
                /\ holder' = [holder EXCEPT ![e] = None]
                \* and owes the line a look, like any release.
                /\ pc' = [pc EXCEPT ![t] = "drain"]
                \* The line and the marks stay; t's own marks are cleared.
                /\ UNCHANGED <<fifo, owed>>
                \* Its nudge mark goes.
                /\ nudged' = [nudged EXCEPT ![t] = FALSE]
                \* Its note goes.
                /\ note' = [note EXCEPT ![t] = FALSE]
                \* Nobody is unparked here; its own flag is cleared.
                /\ unparked' = [unparked EXCEPT ![t] = FALSE]
                \* Notes for its dropped wait are dropped with it.
                /\ inbox' = [x \in Txns |-> inbox[x] \ {t}]
                \* Its wait is gone.
                /\ ready' = [ready EXCEPT ![t] = FALSE]
                \* Nobody made anything ready.
                /\ UNCHANGED readyBy
           \* No. Was it the owed one, with the lock free and others waiting?
           ELSE IF owed[e] = t /\ holder[e] = None /\ q # <<>>
             \* Yes: it was owed, the lock is free and others wait.
             THEN
                  \* The handoff it was owed goes to the new front.
                  /\ holder' = [holder EXCEPT ![e] = Head(q)]
                  \* The new front leaves the line with the lock.
                  /\ fifo' = [fifo EXCEPT ![e] = Tail(q)]
                  \* Nobody is owed any more.
                  /\ owed' = [owed EXCEPT ![e] = None]
                  \* t's nudge mark goes.
                  /\ nudged' = [nudged EXCEPT ![t] = FALSE]
                  \* The new holder is told; t's own note goes.
                  /\ note' = [NoteT(Head(q)) EXCEPT ![t] = FALSE]
                  \* A sleeping new holder is unparked; t's flag is cleared.
                  /\ unparked' = [UnparkedT(Head(q)) EXCEPT ![t] = FALSE]
                  \* A polling new holder gets its note; t's are dropped.
                  /\ inbox' = [x \in Txns |-> InboxT(Head(q))[x] \ {t}]
                  \* t's wait is gone.
                  /\ ready' = [ReadyT(Head(q)) EXCEPT ![t] = FALSE]
                  \* Who made the new holder's wait ready (the bug only).
                  /\ readyBy' = ReadyByT(Head(q))
                  \* t has only its use of the entry to give back.
                  /\ pc' = [pc EXCEPT ![t] = "unpin"]
             \* No: the plain case.
             ELSE
                  \* Nobody is owed once t, if it was the owed one, is gone.
                  LET o == IF owed[e] = t THEN None ELSE owed[e]
                      \* The waiter told to try next. Somebody is told
                      \* (not in the CancelNoPassOn bug) when
                      w == IF /\ Mutant # "CancelNoPassOn"
                              \* somebody still waits,
                              /\ q # <<>>
                              \* nobody is owed,
                              /\ o = None
                              \* t carried an unused nudge or the lock is free,
                              /\ (nudged[t] \/ holder[e] = None)
                              \* and the new front is not nudged already:
                              /\ ~nudged[Head(q)]
                           \* then it is the new front; otherwise nobody.
                           THEN Head(q) ELSE None
                  \* With the owed mark and the waiter to tell worked out:
                  IN
                      \* t leaves the line.
                      /\ fifo' = [fifo EXCEPT ![e] = q]
                      \* The owed mark is settled.
                      /\ owed' = [owed EXCEPT ![e] = o]
                      \* The lock itself does not change hands.
                      /\ UNCHANGED holder
                      \* The new front is marked; t's mark goes.
                      /\ nudged' = [NudgedT(w) EXCEPT ![t] = FALSE]
                      \* The new front is told; t's own note goes.
                      /\ note' = [NoteT(w) EXCEPT ![t] = FALSE]
                      \* A sleeping front is unparked; t's flag is cleared.
                      /\ unparked' = [UnparkedT(w) EXCEPT ![t] = FALSE]
                      \* A polling front gets its note; t's are dropped.
                      /\ inbox' = [x \in Txns |-> InboxT(w)[x] \ {t}]
                      \* t's wait is gone.
                      /\ ready' = [ReadyT(w) EXCEPT ![t] = FALSE]
                      \* Who made the front's wait ready (the bug only).
                      /\ readyBy' = ReadyByT(w)
                      \* t has only its use of the entry to give back.
                      /\ pc' = [pc EXCEPT ![t] = "unpin"]
  \* Its count starts over next time.
  /\ lost' = [lost EXCEPT ![t] = 0]
  \* Nothing else changes.
  /\ UNCHANGED <<granting, cur, uses, pin, pending, undelivered, budget>>

------------------------------------------------------------------------------
\* COMMITTING AND RELEASING

\* A transaction that commits the plain way (or rolls back) lets its lock
\* go: its commit, if any, is visible before commit() returns, so the
\* release comes after it (OwnedLock::unlock in release_locks).
Unlock(t) ==
  \* It holds the lock and does not use commit_nowait.
  /\ pc[t] = "holding" /\ t \notin Nowait
  \* The name leaves the state word.
  /\ holder' = [holder EXCEPT ![pin[t]] = None]
  \* The releasing thread now owes the waiting line a look.
  /\ pc' = [pc EXCEPT ![t] = "drain"]
  \* Nothing else changes.
  /\ UNCHANGED <<granting, cur, uses, fifo, owed, pin, nudged, lost, note, inbox, ready,
                 readyBy, unparked, pending, undelivered, budget>>

\* commit_nowait: the commit is decided and written to the log, and the
\* call returns. It is not visible yet, and the lock stays held. The
\* ReleaseAtWrite bug lets the lock go here.
Write(t) ==
  \* It holds the lock and commits with commit_nowait.
  /\ pc[t] = "holding" /\ t \in Nowait
  \* Its commit is written, not visible.
  /\ pending' = pending \cup {t}
  \* Its own thread has not heard the outcome.
  /\ undelivered' = undelivered \cup {t}
  \* In the real code the lock stays held.
  /\ IF Mutant = "ReleaseAtWrite"
       \* The bug: the lock goes at the write.
       THEN
            \* The name leaves the state word now,
            /\ holder' = [holder EXCEPT ![pin[t]] = None]
            \* and the line is looked at.
            /\ pc' = [pc EXCEPT ![t] = "drain"]
       \* The real code:
       ELSE
            \* the lock is untouched,
            /\ UNCHANGED holder
            \* and the transaction waits for its commit to land.
            /\ pc' = [pc EXCEPT ![t] = "written"]
  \* Nothing else changes.
  /\ UNCHANGED <<granting, cur, uses, fifo, owed, pin, nudged, lost, note, inbox, ready,
                 readyBy, unparked, budget>>

\* The commit lands: it is applied, synced and published, so it is visible
\* (Descriptor::land, src/engine/commit/descriptor.rs; who is told, and
\* that each is told once, is Landing.tla). Any thread may be the one to
\* land it.
Land(t) ==
  \* Its commit is written and not yet visible.
  /\ t \in pending
  \* Now it is visible.
  /\ pending' = pending \ {t}
  \* The transaction's locks are the lander's next job.
  /\ pc' = IF pc[t] = "written" THEN [pc EXCEPT ![t] = "landed"] ELSE pc
  \* Nothing else changes.
  /\ UNCHANGED <<granting, cur, uses, holder, fifo, owed, pin, nudged, lost, note, inbox,
                 ready, readyBy, unparked, undelivered, budget>>

\* Right after the landing, on the landing thread, the commit's locks are
\* released (LandingLocks::deliver). The ReleaseAtDelivery bug keeps them
\* until the committer's own thread hears the outcome.
LanderRelease(t) ==
  \* Its commit has just landed.
  /\ pc[t] = "landed"
  \* In the real code the lock goes now.
  /\ IF Mutant = "ReleaseAtDelivery"
       \* The bug: the lock is kept past the landing.
       THEN
            \* It stays held,
            /\ UNCHANGED holder
            \* until the outcome is delivered.
            /\ pc' = [pc EXCEPT ![t] = "kept"]
       \* The real code: the lock goes as the commit lands.
       ELSE
            \* The name leaves the state word,
            /\ holder' = [holder EXCEPT ![pin[t]] = None]
            \* and the landing thread looks at the line.
            /\ pc' = [pc EXCEPT ![t] = "drain"]
  \* Nothing else changes.
  /\ UNCHANGED <<granting, cur, uses, fifo, owed, pin, nudged, lost, note, inbox, ready,
                 readyBy, unparked, pending, undelivered, budget>>

\* The committer's own thread looks at its inbox and hears the outcome of
\* its commit (the CommitTicket is delivered). Nothing about the locks
\* depends on this step, and nothing forces it to happen.
Deliver(t) ==
  \* The outcome is not heard yet, and the commit has landed.
  /\ t \in undelivered /\ t \notin pending
  \* Now it is heard.
  /\ undelivered' = undelivered \ {t}
  \* Only in the ReleaseAtDelivery bug is the lock still held here.
  /\ IF pc[t] = "kept"
       \* The bug lets it go only now:
       THEN
            \* the name leaves the state word,
            /\ holder' = [holder EXCEPT ![pin[t]] = None]
            \* and the line is looked at.
            /\ pc' = [pc EXCEPT ![t] = "drain"]
       \* In the real code there is nothing to do.
       ELSE
            \* The lock and the step stay as they are.
            UNCHANGED <<holder, pc>>
  \* Nothing else changes.
  /\ UNCHANGED <<granting, cur, uses, fifo, owed, pin, nudged, lost, note, inbox, ready,
                 readyBy, unparked, pending, budget>>

\* After a release, the releasing thread looks at the waiting line once
\* (OwnedLock::pass). If somebody is owed and the lock is free, the front
\* gets the lock itself, in its own name. Otherwise the front is told to
\* try again, whether or not the lock is still free: if a barger took it
\* meanwhile, the front tries, loses and counts the loss.
Drain(t) ==
  \* It released, and owes the line a look.
  /\ pc[t] = "drain"
  \* The entry it released,
  /\ LET e == pin[t]
         \* and that entry's line.
         q == fifo[e]
     \* Is somebody owed, with the lock free and the line not empty?
     IN  IF Mutant # "ReleaseNoWake" /\ owed[e] # None /\ holder[e] = None /\ q # <<>>
           \* Yes: a handoff, whose first step writes the front's name into
           \* the state word (take_for_waiter). The name carries the GRANTING
           \* mark until the grant, the second step (Grant), is through.
           THEN
                \* The front's name goes into the state word,
                /\ holder' = [holder EXCEPT ![e] = Head(q)]
                \* marked: its grant is still to come.
                /\ granting' = [granting EXCEPT ![e] = Head(q)]
                \* The line, the owed mark, the nudges and the notes wait for the
                \* grant: the waiter may still withdraw.
                /\ UNCHANGED <<fifo, owed, nudged, note, unparked, inbox, ready,
                               readyBy>>
                \* The releaser grants next.
                /\ pc' = [pc EXCEPT ![t] = "grant"]
           \* No: no handoff, at most a nudge.
           ELSE
                \* The waiter to nudge: somebody is nudged (not in the ReleaseNoWake bug) when
                LET w == IF /\ Mutant # "ReleaseNoWake"
                            \* nobody is owed,
                            /\ owed[e] = None
                            \* somebody waits,
                            /\ q # <<>>
                            \* and the front is not nudged already:
                            /\ ~nudged[Head(q)]
                         \* then it is the front; otherwise nobody.
                         THEN Head(q) ELSE None
                \* With the waiter to nudge worked out:
                IN
                    \* The lock, its mark and the line stay as they are.
                    /\ UNCHANGED <<holder, granting, fifo, owed>>
                    \* The front is marked "try again",
                    /\ nudged' = NudgedT(w)
                    \* and told.
                    /\ note' = NoteT(w)
                    \* A sleeping one is unparked.
                    /\ unparked' = UnparkedT(w)
                    \* A polling one gets the note in its own inbox.
                    /\ inbox' = InboxT(w)
                    \* Only the planted bug touches the ready flags here.
                    /\ ready' = ReadyT(w)
                    \* And who made it ready.
                    /\ readyBy' = ReadyByT(w)
                    \* The transaction now gives its use of the entry back.
                    /\ pc' = [pc EXCEPT ![t] = "unpin"]
  \* Nothing else changes.
  /\ UNCHANGED <<cur, uses, pin, lost, pending, undelivered, budget>>

\* The second step of a handoff (OwnedLock::pass, the grant after
\* take_for_waiter): the waiter the name was written for is granted, if it
\* is still in the line. It leaves the line with the lock, nobody is owed any
\* more, the mark goes, and the new holder is told. A waiter that withdrew
\* in between is not granted: its name is cleared again (clear_holder), and
\* the releaser looks at the line once more.
Grant(t) ==
  \* The releaser wrote a waiter's name and has not granted it yet.
  /\ pc[t] = "grant"
  \* The entry, and the waiter whose name it carries.
  /\ LET e == pin[t]
         \* The waiter the handoff is for.
         w == granting[e]
     \* Is that waiter still in the line?
     IN  IF \E i \in 1..Len(fifo[e]) : fifo[e][i] = w
           \* Yes: the grant goes through.
           THEN
                \* It leaves the line with the lock,
                /\ fifo' = [fifo EXCEPT ![e] = Without(@, w)]
                \* nobody is owed any more,
                /\ owed' = [owed EXCEPT ![e] = None]
                \* and its name stays in the state word.
                /\ UNCHANGED holder
                \* The new holder is told.
                /\ note' = NoteT(w)
                \* A sleeping one is unparked.
                /\ unparked' = UnparkedT(w)
                \* A polling one gets the note in its own inbox.
                /\ inbox' = InboxT(w)
                \* Only the planted bug touches the ready flags here.
                /\ ready' = ReadyT(w)
                \* And who made it ready.
                /\ readyBy' = ReadyByT(w)
                \* The releaser gives its use of the entry back next.
                /\ pc' = [pc EXCEPT ![t] = "unpin"]
           \* No: it withdrew, so it is not granted.
           ELSE
                \* The name the pass wrote is cleared again,
                /\ holder' = [holder EXCEPT ![e] = None]
                \* nobody is owed by it any more,
                /\ owed' = [owed EXCEPT ![e] = IF @ = w THEN None ELSE @]
                \* the line and the notes stay,
                /\ UNCHANGED <<fifo, note, unparked, inbox, ready, readyBy>>
                \* and the releaser looks at the line again.
                /\ pc' = [pc EXCEPT ![t] = "drain"]
  \* Either way the mark goes: the handoff is over.
  /\ granting' = [granting EXCEPT ![pin[t]] = None]
  \* Nothing else changes.
  /\ UNCHANGED <<cur, uses, pin, nudged, lost, pending, undelivered, budget>>

------------------------------------------------------------------------------
\* GIVING THE ENTRY BACK

\* What a transaction's own marks are once its call is over.
Forget(f, t, v) == [f EXCEPT ![t] = v]

\* The transaction gives its use of the entry back (LockTable::unpin, the
\* decrement). If that was the last use, it goes on to retire the entry.
Unpin(t) ==
  \* It is done with the lock.
  /\ pc[t] = "unpin"
  \* One use fewer.
  /\ uses' = [uses EXCEPT ![pin[t]] = @ - 1]
  \* If it was the last one ...
  /\ IF uses[pin[t]] = 1
       \* ... the entry may have to go:
       THEN
            \* the transaction tries to retire the entry next,
            /\ pc' = [pc EXCEPT ![t] = "retire"]
            \* still holding its reference to it.
            /\ UNCHANGED pin
       \* ... otherwise others still use the entry:
       ELSE
            \* the transaction is finished with it,
            /\ pc' = [pc EXCEPT ![t] = "idle"]
            \* and refers to nothing.
            /\ pin' = [pin EXCEPT ![t] = None]
  \* Its marks from this call are cleared.
  /\ nudged' = Forget(nudged, t, FALSE)
  \* Its count starts over.
  /\ lost' = Forget(lost, t, 0)
  \* Its note is gone.
  /\ note' = Forget(note, t, FALSE)
  \* Its wait is gone.
  /\ ready' = Forget(ready, t, FALSE)
  \* Its unpark is gone.
  /\ unparked' = Forget(unparked, t, FALSE)
  \* Nothing else changes.
  /\ UNCHANGED <<granting, cur, holder, fifo, owed, inbox, readyBy, pending, undelivered,
                 budget>>

\* The transaction that gave back the last use takes the entry out of the
\* table, but only if nobody took a use in the meantime: the check and the
\* retirement are one compare-and-swap on the count, made inside the map's
\* removal of exactly this entry (KeyLock::retire in remove_if). The
\* RetireNoCas bug retires without looking.
TryRetire(t) ==
  \* It gave back the last use.
  /\ pc[t] = "retire"
  \* If the entry is still unused (or in the bug, whatever its count) ...
  /\ IF uses[pin[t]] = 0 \/ Mutant = "RetireNoCas"
       \* ... the entry is retired:
       THEN
            \* the table lets go of it, if it is still the key's entry.
            cur' = IF cur = pin[t] THEN None ELSE cur
       \* ... otherwise somebody uses it:
       ELSE
            \* it stays in the table.
            UNCHANGED cur
  \* The transaction refers to nothing now,
  /\ pin' = [pin EXCEPT ![t] = None]
  \* and its call is over.
  /\ pc' = [pc EXCEPT ![t] = "idle"]
  \* Nothing else changes.
  /\ UNCHANGED <<granting, uses, holder, fifo, owed, nudged, lost, note, inbox, ready,
                 readyBy, unparked, pending, undelivered, budget>>

------------------------------------------------------------------------------
\* ALL THE STEPS

\* One step of the system is one step of one transaction or one thread.
Next ==
  \* A transaction starts a lock call.
  \/ \E t \in Txns : Lookup(t)
  \* It tries the fast path.
  \/ \E t \in Txns : TryFast(t)
  \* It records how it is to be told.
  \/ \E t \in Txns : Note(t)
  \* It tries once more.
  \/ \E t \in Txns : TryAgain(t)
  \* It joins the line.
  \/ \E t \in Txns : Enqueue(t)
  \* It looks at its place in the line.
  \/ \E t \in Txns : Park(t)
  \* The planted bug's late note.
  \/ \E t \in Txns : LateNote(t)
  \* A thread looks at its inbox.
  \/ \E q \in Txns : PollQueue(q)
  \* A told transaction calls again.
  \/ \E t \in Txns : Rerun(t)
  \* Its call polls its place in the line.
  \/ \E t \in Txns : PollQueued(t)
  \* A waiter gives up.
  \/ \E t \in Txns : Cancel(t)
  \* A holder releases.
  \/ \E t \in Txns : Unlock(t)
  \* A holder commits with commit_nowait.
  \/ \E t \in Txns : Write(t)
  \* A written commit lands.
  \/ \E t \in Txns : Land(t)
  \* The lander releases the commit's lock.
  \/ \E t \in Txns : LanderRelease(t)
  \* A committer's thread hears its outcome.
  \/ \E t \in Txns : Deliver(t)
  \* A releaser looks at the line.
  \/ \E t \in Txns : Drain(t)
  \* A releaser grants the waiter it named.
  \/ \E t \in Txns : Grant(t)
  \* A transaction gives its use of the entry back.
  \/ \E t \in Txns : Unpin(t)
  \* The last user retires the entry.
  \/ \E t \in Txns : TryRetire(t)

\* The whole behaviour: start, then steps, and every transaction keeps
\* going while it can. Three things are never forced: starting a new call,
\* giving up, and a committer hearing its own outcome.
Spec ==
  \* Start.
  /\ Init
  \* Step.
  /\ [][Next]_vars
  \* Each transaction keeps going:
  /\ \A t \in Txns :
       \* the fast path,
       /\ WF_vars(TryFast(t))
       \* the note,
       /\ WF_vars(Note(t))
       \* the second try,
       /\ WF_vars(TryAgain(t))
       \* joining the line,
       /\ WF_vars(Enqueue(t))
       \* looking at its place,
       /\ WF_vars(Park(t))
       \* the bug's late note,
       /\ WF_vars(LateNote(t))
       \* looking at its inbox,
       /\ WF_vars(PollQueue(t))
       \* calling again once told,
       /\ WF_vars(Rerun(t))
       \* polling its place,
       /\ WF_vars(PollQueued(t))
       \* releasing what it holds,
       /\ WF_vars(Unlock(t))
       \* committing what it holds,
       /\ WF_vars(Write(t))
       \* its commit landing,
       /\ WF_vars(Land(t))
       \* the lander's release,
       /\ WF_vars(LanderRelease(t))
       \* the look at the line after a release,
       /\ WF_vars(Drain(t))
       \* the grant of a handoff,
       /\ WF_vars(Grant(t))
       \* giving the use back,
       /\ WF_vars(Unpin(t))
       \* and retiring the entry.
       /\ WF_vars(TryRetire(t))

------------------------------------------------------------------------------
\* WHAT MUST ALWAYS HOLD

\* Every variable holds the kind of value it should.
TypeOK ==
  \* The table holds an entry or none.
  /\ cur \in Entries \cup {None}
  \* Use counts are numbers.
  /\ uses \in [Entries -> Nat]
  \* A holder is a transaction or nobody.
  /\ holder \in [Entries -> Txns \cup {None}]
  \* A waiting line is a list of transactions.
  /\ fifo \in [Entries -> Seq(Txns)]
  \* The owed waiter is a transaction or nobody.
  /\ owed \in [Entries -> Txns \cup {None}]
  \* So is the waiter a half-made handoff names.
  /\ granting \in [Entries -> Txns \cup {None}]
  \* A transaction refers to an entry or none.
  /\ pin \in [Txns -> Entries \cup {None}]
  \* Each transaction is at a known step.
  /\ pc \in [Txns -> {"idle", "pinned", "busy", "noted", "enqueue", "parking",
                      "lateNote", "waiting", "renoted", "holding", "written",
                      "landed", "kept", "drain", "grant", "unpin", "retire"}]
  \* Nudge marks are yes or no.
  /\ nudged \in [Txns -> BOOLEAN]
  \* Loss counts are numbers.
  /\ lost \in [Txns -> Nat]
  \* Notes are there or not.
  /\ note \in [Txns -> BOOLEAN]
  \* An inbox holds waits, named by their transaction.
  /\ inbox \in [Txns -> SUBSET Txns]
  \* A wait is ready or not.
  /\ ready \in [Txns -> BOOLEAN]
  \* It was made ready by a thread, or by "not its own".
  /\ readyBy \in [Txns -> Txns \cup {NotOwn}]
  \* A thread was unparked or not.
  /\ unparked \in [Txns -> BOOLEAN]
  \* Pending commits are transactions.
  /\ pending \subseteq Txns
  \* So are the unheard outcomes.
  /\ undelivered \subseteq Txns
  \* The budget never goes above its start.
  /\ budget \in 0..Takes

\* RULE 1, mutual exclusion. At most one transaction holds the key, by the
\* name in the state word or by what its own call believes (a call that
\* returned "held" and has not let go). Without it, two transactions could
\* both read a counter at 5 and both write 6. The MineBeforeGrant bug breaks
\* it through the belief: the name is gone, the call still says "held".
MutualExclusion ==
  \* Any two transactions that own the key are the same one.
  \A t, u \in Txns : (Owns(t) /\ Owns(u)) => t = u

\* RULE 1, the table's half. An entry that left the table is used by
\* nobody: no holder, no waiter, no call about to use it. And the entry in
\* the table counts its users exactly. Without it, one transaction could
\* take the old entry's lock while another takes the new entry's.
TableSound ==
  \* For every entry name:
  \A e \in Entries :
    \* is it the entry the table holds for the key?
    IF e = cur
      \* Yes: it is the entry in the table.
      THEN
           \* Its count is exactly the number of transactions using it.
           uses[e] = Cardinality({t \in Txns : Using(t, e)})
      \* No: it is an entry out of the table, which
      ELSE
           \* has no holder,
           /\ holder[e] = None
           \* no waiting line,
           /\ fifo[e] = <<>>
           \* and no user.
           /\ \A t \in Txns : ~Using(t, e)

\* RULE 2, no lost wake-up, first half. If the lock is free, nobody is
\* owed, somebody waits and no releaser is about to look at the line, then
\* the front waiter has been told to try. Without it, a free lock and a
\* waiter that sleeps forever would sit side by side.
NoStrandedFront ==
  \* For every entry:
  \A e \in Entries :
    \* if it is free and nobody is owed,
    (/\ Free(e)
     \* somebody waits,
     /\ fifo[e] # <<>>
     \* and no releaser still owes the line a look,
     /\ ~\E t \in Txns : pin[t] = e /\ pc[t] = "drain")
    \* then its front waiter carries the "try again" mark.
    => nudged[Head(fifo[e])]

\* A waiting transaction has been told: a sleeper's thread was unparked; a
\* polling one has the note in its own inbox, or its wait is ready already.
Told(t) == IF t \in Blocking THEN unparked[t] ELSE (t \in inbox[t] \/ ready[t])

\* RULE 2, second half. A transaction that waits outside any call, and was
\* nudged or handed the lock, has been told. Without it, the mark would be
\* set on a waiter that never looks again: its note fell between two steps.
WokenIsTold ==
  \* For every transaction waiting outside a call,
  \A t \in Txns :
    \* that was nudged or holds the lock by handoff:
    (pc[t] = "waiting" /\ (nudged[t] \/ (holder[pin[t]] = t /\ granting[pin[t]] # t)))
    \* it was told.
    => Told(t)

\* RULE 3, bounded bypass. No waiter loses more than Bound races. Without
\* it, two busy transactions could pass the lock back and forth for ever
\* while a third starves.
BoundedBypass ==
  \* Every loss count is within the bound.
  \A t \in Txns : lost[t] <= Bound

\* RULE 4, delivery on the waiter's own queue. A note is only ever in the
\* inbox of the transaction whose wait it completes, and a wait is made
\* ready only by its own thread. Without it, thread 2's poll would finish
\* thread 1's wait, and thread 1, which never polls for it, would be
\* resumed on the wrong thread or not at all.
OwnQueue ==
  \* Every inbox holds only its own transaction's waits,
  /\ \A q \in Txns : inbox[q] \subseteq {q}
  \* and a ready wait was made ready by its own thread.
  /\ \A t \in Txns : ready[t] => readyBy[t] = t

\* RULE 5, release at landing keeps isolation. While a commit is written
\* and not yet visible, nobody else holds the key. Without it, the next
\* holder would read the key as it was before the commit, add 1, and
\* overwrite the commit: a lost update.
IsolationAtLanding ==
  \* For every commit not yet visible, every other transaction
  \A t \in pending : \A u \in Txns \ {t} :
    \* does not hold the key.
    ~Holds(u)

\* RULE 5, the other side: the lock does not outlive the landing. A
\* transaction holds the key only while it uses it, while its commit has
\* not landed, or while it waits with the lock just handed to it. Without
\* it, every other transaction would wait until the committer's thread
\* gets round to looking at its inbox.
HeldOnlyWhileNeeded ==
  \* For every transaction:
  \A t \in Txns :
    \* if it holds the key by a finished grant or its own take, it is at one
    \* of these steps (a half-made handoff is the releaser's, not the waiter's).
    Granted(t) => pc[t] \in {"holding", "written", "landed", "parking",
                             "waiting", "renoted"}

\* Over whole runs: a watched transaction that asks for the lock stops
\* asking: it gets the lock, or gives up. Nothing here depends on a
\* committer hearing its own outcome, since that step is never forced.
Served ==
  \* For every watched transaction, asking leads to not asking.
  \A t \in Watched : (pc[t] \in Asking) ~> (pc[t] \notin Asking)
====
