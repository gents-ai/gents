---- MODULE LockTableCalls ----
\* ===========================================================================
\* THE STORY
\* ===========================================================================
\* One pessimistic transaction can be used from several threads at once, so
\* two of its calls can ask for the same key at the same time. If another
\* transaction holds the key, the transaction waits for it, and every call of
\* the transaction that wants the key shares that one wait (a PendingLock in
\* src/transaction/lock_table.rs). The transaction keeps its waits in a map,
\* one per key (Transaction::lock_waits, src/transaction/locking.rs).
\*
\* A call that wants the key looks its wait up. If there is none, it makes
\* one and publishes it in the map. Then it records how it is to be told
\* (a "note"), and polls the wait: the first poll joins the lock's waiting
\* line, a later poll tries the lock when the line said "try now". Only one
\* call polls the wait at a time; a call that finds another one polling goes
\* back to its caller and waits for its note.
\*
\* Two bugs this model was written for.
\*
\* 1. Two waits for one key (review comment 4256035717). Calls A and B of
\*    transaction T both look up, both find no wait, both make one. A
\*    publishes its wait a and joins the line; B publishes b and joins the
\*    line behind a. The lookup returns the newest wait, b. The holder
\*    releases: the front of the line, a, is told "try now", and A's note
\*    says so. A calls again, looks up, finds b, polls b: b was not told
\*    anything, so it waits. The key is free, a holds the "try now", and no
\*    call will ever poll a again. The fix: publishing is one atomic step of
\*    the map (insert into an empty place, or replace a wait that is over),
\*    and a call that loses gives its new wait back before it ever polls it.
\*
\* 2. A wake that falls inside a poll (review comment 4256035723). A polls
\*    the wait and finds nothing yet. Before A marks the wait "nobody
\*    polling", the holder releases and tells the notes recorded so far. B
\*    records its note now, after that wake, finds A polling, and goes back to
\*    wait for its note. A marks the wait free and leaves. If A's caller never
\*    calls again, B waits for ever next to a free key. The fix: a wake that
\*    comes while a poll runs marks it ("woken"), and a poll that ends with
\*    the mark tells the notes once more.
\*
\* WHAT THE MODEL PROMISES
\*   OneQueuedWait   the transaction never has two waits in the line;
\*   NobodyStranded  when the key is free and the transaction still wants it,
\*                   some call of the transaction is running or has been told.
\*
\* WHAT IS MODELLED, and the code each piece mirrors:
\*   held, released  the other transaction's hold on the key, given up once:
\*                   OwnedLock::unlock, src/sync/owned_lock.rs
\*   mine            the transaction holds the key (its name in the state word)
\*   line, nudged    the lock's waiting line and its "try now" marks: the
\*                   lock's WaitQueue and the NUDGED bit of a waiter
\*   wstate, woken   a wait's state word and its WOKEN mark: Wakeup::state,
\*                   lock_table.rs
\*   notes, told     the notes recorded on a wait, and each call's LockWait
\*                   being ready: PendingLock::note, Wakeup::wake_all
\*   published       the transaction's map entry for the key, newest first
\*                   (the fixed map holds one; the old buffer held several)
\*   pc, use, cand   where each call is, the wait it polls, the wait it made
\*
\* WHAT IS LEFT OUT, and why that loses nothing.
\*   - Other waiting transactions, barging by them, bounded bypass, the
\*     table's entries and close: LockTable.tla checks all of those with one
\*     call per transaction. Here the only other transaction is the holder.
\*   - Joining the line and its second look at the lock are one step: the
\*     first poll either takes a free lock or joins while it is held.
\*     LockTable.tla checks the second look on its own (its NoRecheck bug).
\*   - Memory order: the loom models two_pollers_on_one_wait_are_both_told
\*     and two_calls_on_one_transaction_share_one_wait check it on the real
\*     types (src/transaction/lock_loom.rs).
\*
\* CONFIGURATIONS (MC_LockTableCalls_*.cfg).
\*   Green               two calls, either may give up its attempt; both
\*                       invariants hold.
\*   Red_NewestShadows   bug 1, the old publication: OneQueuedWait breaks.
\*   Red_NoHandBack      bug 2, the old end of a poll: NobodyStranded breaks.

\* Numbers, lists and set sizes come from these standard modules.
EXTENDS Naturals, Sequences, FiniteSets

\* The things a configuration chooses.
CONSTANTS
  \* The calls of the one waiting transaction, each on its own thread.
  Calls,
  \* Names for the waits the calls may make (PendingLock allocations).
  Waits,
  \* The calls whose caller may give its attempt up while it waits.
  MayDrop,
  \* "none" for the real code, or the name of one planted bug.
  Mutant

\* The calls and the waits are numbered from 1, so 0 can mean "none".
ASSUME Calls \subseteq Nat \ {0} /\ Waits \subseteq Nat \ {0}
\* The calls that may give up are calls.
ASSUME MayDrop \subseteq Calls
\* The bug names this model knows.
ASSUME Mutant \in {"none", "NewestShadows", "NoHandBack"}

\* The state that changes from step to step.
VARIABLES
  \* The other transaction still holds the key.
  held,
  \* The other transaction has let the key go (it does so once).
  released,
  \* The waiting transaction holds the key.
  mine,
  \* The lock's waiting line, oldest first: the waits of the transaction in it.
  line,
  \* [Waits -> BOOLEAN]: the lock told this wait "try now", and no poll has
  \* looked at it yet (the NUDGED bit of the wait's node).
  nudged,
  \* [Waits -> state]: "none" not made yet, "idle" nobody polls it,
  \* "polling" a call polls it, "done" it is over, "dropped" made and given
  \* back before anybody polled it.
  wstate,
  \* [Waits -> BOOLEAN]: a wake came while a call polled this wait (WOKEN).
  woken,
  \* [Waits -> SUBSET Calls]: the calls whose note is recorded on the wait
  \* and not yet told.
  notes,
  \* [Calls -> BOOLEAN]: the call's LockWait is ready: it was told.
  told,
  \* [Calls -> a step name]: where each call is.
  pc,
  \* [Calls -> Waits or 0]: the wait the call polls.
  use,
  \* [Calls -> Waits or 0]: the wait the call made and has not published yet.
  cand,
  \* [Calls -> result]: what the call's poll found, between its two halves.
  res,
  \* The transaction's map entry for the key: published waits, newest first.
  published

\* Every variable, so a step that changes none of them is a stutter.
vars == <<held, released, mine, line, nudged, wstate, woken, notes, told, pc,
          use, cand, res, published>>

------------------------------------------------------------------------------
\* SMALL HELPERS

\* The key is free: neither transaction holds it.
Free == ~held /\ ~mine
\* The published waits that are not over, newest first.
Live == SelectSeq(published, LAMBDA w : wstate[w] \notin {"done", "dropped"})
\* What a lookup finds: the newest published wait that is not over, or 0.
Lookup == IF Live = <<>> THEN 0 ELSE Head(Live)
\* The line without wait w.
Without(q, w) == SelectSeq(q, LAMBDA x : x # w)
\* Whether wait w is in the line.
Queued(w) == \E i \in 1..Len(line) : line[i] = w
\* Told after every note on wait w is told: wake_all delivers each of them.
ToldAll(w) == [c \in Calls |-> told[c] \/ c \in notes[w]]

------------------------------------------------------------------------------
\* HOW IT STARTS

\* The other transaction holds the key; both calls are about to ask for it.
Init ==
  \* The other transaction holds the key,
  /\ held = TRUE
  \* and has not let it go yet.
  /\ released = FALSE
  \* The waiting transaction holds nothing.
  /\ mine = FALSE
  \* Nobody waits in the line.
  /\ line = <<>>
  \* Nobody was told "try now".
  /\ nudged = [w \in Waits |-> FALSE]
  \* No wait is made yet.
  /\ wstate = [w \in Waits |-> "none"]
  \* No poll was woken.
  /\ woken = [w \in Waits |-> FALSE]
  \* No note is recorded.
  /\ notes = [w \in Waits |-> {}]
  \* No call is told.
  /\ told = [c \in Calls |-> FALSE]
  \* Every call is about to look its wait up.
  /\ pc = [c \in Calls |-> "look"]
  \* No call polls a wait yet.
  /\ use = [c \in Calls |-> 0]
  \* No call made a wait yet.
  /\ cand = [c \in Calls |-> 0]
  \* No poll has a result.
  /\ res = [c \in Calls |-> "none"]
  \* The map has no entry for the key.
  /\ published = <<>>

------------------------------------------------------------------------------
\* A CALL

\* The call looks the transaction's wait for the key up (live_wait). Found:
\* it will poll that wait. Not found: it tries the lock itself.
Look(c) ==
  \* The call is at its lookup.
  /\ pc[c] = "look"
  \* Is there a wait that is not over?
  /\ IF Lookup # 0
       \* Yes: the call will use it,
       THEN /\ use' = [use EXCEPT ![c] = Lookup]
            \* and records its note next.
            /\ pc' = [pc EXCEPT ![c] = "note"]
       \* No: the call tries the lock itself next.
       ELSE /\ pc' = [pc EXCEPT ![c] = "try"]
            \* It polls nothing yet.
            /\ UNCHANGED use
  \* Nothing else changes.
  /\ UNCHANGED <<held, released, mine, line, nudged, wstate, woken, notes,
                 told, cand, res, published>>

\* The call tries the lock once (OwnedLock::try_lock). Free: taken. Held by
\* its own transaction (another call took it): nothing to do. Held by the
\* other transaction: it makes a new wait (PendingLock::new), not published.
Try(c) ==
  \* The call found no wait.
  /\ pc[c] = "try"
  \* Is the key free?
  /\ IF Free
       \* Yes: the transaction takes it, and the call is done.
       THEN /\ mine' = TRUE
            \* Done.
            /\ pc' = [pc EXCEPT ![c] = "done"]
            \* No wait is made.
            /\ UNCHANGED <<wstate, cand>>
       \* No. Does the transaction hold it already?
       ELSE IF mine
         \* Yes: the call is done, and nothing is taken.
         THEN /\ pc' = [pc EXCEPT ![c] = "done"]
              \* The lock and the waits stay.
              /\ UNCHANGED <<mine, wstate, cand>>
         \* No, the other transaction holds it: make a wait, with the
         \* lowest name not used yet (there must be one).
         ELSE LET free == {w \in Waits : wstate[w] = "none"}
                  \* The lowest of them.
                  w == CHOOSE x \in free : \A y \in free : x <= y
              \* With that name:
              IN /\ free # {}
                 \* the wait exists, and nobody polls it,
                 /\ wstate' = [wstate EXCEPT ![w] = "idle"]
                 \* the call holds it, unpublished,
                 /\ cand' = [cand EXCEPT ![c] = w]
                 \* and publishes it next.
                 /\ pc' = [pc EXCEPT ![c] = "publish"]
                 \* The lock stays held.
                 /\ UNCHANGED mine
  \* Nothing else changes.
  /\ UNCHANGED <<held, released, line, nudged, woken, notes, told, use, res,
                 published>>

\* The call publishes the wait it made (Transaction::publish_wait). In the
\* real code this is one atomic step: if the map holds a wait that is not
\* over, the call loses, gives its own wait back unpolled, and looks again.
\* The NewestShadows bug publishes without looking: the old buffer took a
\* second wait for the key, and the newest one hid the older.
Publish(c) ==
  \* The call made a wait.
  /\ pc[c] = "publish"
  \* Its wait.
  /\ LET w == cand[c]
     \* Does another call's wait, not over, already stand for the key?
     IN IF Lookup # 0 /\ Mutant # "NewestShadows"
          \* Yes: this call's wait is given back before anybody polls it,
          THEN /\ wstate' = [wstate EXCEPT ![w] = "dropped"]
               \* and the call looks the published wait up again.
               /\ pc' = [pc EXCEPT ![c] = "look"]
               \* The map does not change.
               /\ UNCHANGED <<published, use>>
          \* No (or the bug): the wait goes into the map, newest first,
          ELSE /\ published' = <<w>> \o published
               \* the call will poll it,
               /\ use' = [use EXCEPT ![c] = w]
               \* and records its note next.
               /\ pc' = [pc EXCEPT ![c] = "note"]
               \* The wait itself does not change.
               /\ UNCHANGED wstate
  \* The call holds no unpublished wait any more.
  /\ cand' = [cand EXCEPT ![c] = 0]
  \* Nothing else changes.
  /\ UNCHANGED <<held, released, mine, line, nudged, woken, notes, told, res>>

\* Before it polls, the call records how it is to be told (PendingLock::note),
\* so a wake between the note and the poll is never missed.
Note(c) ==
  \* The call has a wait to poll.
  /\ pc[c] = "note"
  \* Its note joins the wait's notes.
  /\ notes' = [notes EXCEPT ![use[c]] = @ \cup {c}]
  \* This call's new LockWait is not ready.
  /\ told' = [told EXCEPT ![c] = FALSE]
  \* It polls next.
  /\ pc' = [pc EXCEPT ![c] = "poll"]
  \* Nothing else changes.
  /\ UNCHANGED <<held, released, mine, line, nudged, wstate, woken, use, cand,
                 res, published>>

\* The poll starts with one compare-and-swap on the wait's state word. Over
\* already: the call looks again. Somebody polls it: the call goes back to
\* its caller and waits for its note (Polled::Busy). Free: this call polls.
PollStart(c) ==
  \* The call recorded its note.
  /\ pc[c] = "poll"
  \* The wait it polls.
  /\ LET w == use[c]
     \* Is the wait over?
     IN IF wstate[w] = "done"
          \* Yes: the note is void (its LockWait is ready), and the call looks
          \* again.
          THEN /\ told' = [told EXCEPT ![c] = TRUE]
               \* Its note is no longer pending.
               /\ notes' = [notes EXCEPT ![w] = @ \ {c}]
               \* Back to the lookup.
               /\ pc' = [pc EXCEPT ![c] = "look"]
               \* The wait does not change.
               /\ UNCHANGED <<wstate, woken>>
        \* No. Is another call polling it?
        ELSE IF wstate[w] = "polling"
          \* Yes: this call returns WouldBlock, its note still recorded.
          THEN /\ pc' = [pc EXCEPT ![c] = "waiting"]
               \* The wait and the notes do not change.
               /\ UNCHANGED <<wstate, woken, notes, told>>
          \* No: this call polls it now, and no wake has marked this poll.
          ELSE /\ wstate' = [wstate EXCEPT ![w] = "polling"]
               \* The mark starts clear.
               /\ woken' = [woken EXCEPT ![w] = FALSE]
               \* Next: the poll itself.
               /\ pc' = [pc EXCEPT ![c] = "inpoll"]
               \* The notes do not change.
               /\ UNCHANGED <<notes, told>>
  \* Nothing else changes.
  /\ UNCHANGED <<held, released, mine, line, nudged, use, cand, res, published>>

\* The poll itself (OwnedWait::poll, then the check for the transaction's own
\* name). Not in the line yet: take the key if free, else join the line.
\* In the line and told "try now": try, and take the key if free. The
\* transaction holds the key already (another call took it): leave the line.
PollBody(c) ==
  \* The call polls its wait.
  /\ pc[c] = "inpoll"
  \* The wait.
  /\ LET w == use[c]
     \* Does the transaction hold the key already?
     IN IF mine
          \* Yes: the wait leaves the line (Polled::Mine).
          THEN /\ line' = Without(line, w)
               \* Its "try now", if any, goes with it.
               /\ nudged' = [nudged EXCEPT ![w] = FALSE]
               \* The result.
               /\ res' = [res EXCEPT ![c] = "mine"]
               \* The key stays the transaction's.
               /\ UNCHANGED mine
        \* No. Is the key free, and the wait new or told "try now"?
        ELSE IF Free /\ (~Queued(w) \/ nudged[w])
          \* Yes: the transaction takes it (Polled::Acquired),
          THEN /\ mine' = TRUE
               \* and the wait leaves the line,
               /\ line' = Without(line, w)
               \* with its "try now" used up.
               /\ nudged' = [nudged EXCEPT ![w] = FALSE]
               \* The result.
               /\ res' = [res EXCEPT ![c] = "acquired"]
          \* No: the wait joins the line if it is new, its "try now" is used
          \* up, and the call waits (Polled::Pending).
          ELSE /\ line' = IF Queued(w) THEN line ELSE Append(line, w)
               \* The "try now", if any, is used up.
               /\ nudged' = [nudged EXCEPT ![w] = FALSE]
               \* The result.
               /\ res' = [res EXCEPT ![c] = "pending"]
               \* Nothing is taken.
               /\ UNCHANGED mine
  \* Next: the end of the poll.
  /\ pc' = [pc EXCEPT ![c] = "endpoll"]
  \* Nothing else changes.
  /\ UNCHANGED <<held, released, wstate, woken, notes, told, use, cand,
                 published>>

\* The poll ends. A wait that ended (taken, or held already) is over, and
\* every call noted on it is told (PendingLock::finish). A wait still pending
\* is free to poll again: the swap reads the WOKEN mark, and a poll a wake
\* fell into tells every note once more. The NoHandBack bug skips that.
EndPoll(c) ==
  \* The poll ran.
  /\ pc[c] = "endpoll"
  \* The wait.
  /\ LET w == use[c]
     \* Did the poll end the wait?
     IN IF res[c] \in {"acquired", "mine"}
          \* Yes: the wait is over,
          THEN /\ wstate' = [wstate EXCEPT ![w] = "done"]
               \* every call noted on it is told,
               /\ told' = ToldAll(w)
               \* its notes are used up,
               /\ notes' = [notes EXCEPT ![w] = {}]
               \* and this call is done: the transaction holds the key.
               /\ pc' = [pc EXCEPT ![c] = "done"]
          \* No: the wait is free to poll again,
          ELSE /\ wstate' = [wstate EXCEPT ![w] = "idle"]
               \* and if a wake marked this poll (not in the bug),
               /\ IF woken[w] /\ Mutant # "NoHandBack"
                    \* every note on the wait is told now,
                    THEN /\ told' = ToldAll(w)
                         \* and they are used up.
                         /\ notes' = [notes EXCEPT ![w] = {}]
                    \* Otherwise the notes wait for the next wake.
                    ELSE UNCHANGED <<told, notes>>
               \* The call returns WouldBlock.
               /\ pc' = [pc EXCEPT ![c] = "waiting"]
  \* The mark is cleared with the swap.
  /\ woken' = [woken EXCEPT ![use[c]] = FALSE]
  \* Nothing else changes.
  /\ UNCHANGED <<held, released, mine, line, nudged, use, cand, res,
                 published>>

\* A call that waits outside the database and was told calls again, and
\* looks its wait up first, as every call does.
Rerun(c) ==
  \* The call waits, and its LockWait is ready.
  /\ pc[c] = "waiting" /\ told[c]
  \* It calls again.
  /\ pc' = [pc EXCEPT ![c] = "look"]
  \* Nothing else changes.
  /\ UNCHANGED <<held, released, mine, line, nudged, wstate, woken, notes,
                 told, use, cand, res, published>>

\* A caller gives its attempt up while the call waits: it never calls again.
\* Its note stays recorded until a wake uses it up, as in the code.
Drop(c) ==
  \* Only a call whose caller may give up, and only while it waits.
  /\ c \in MayDrop /\ pc[c] = "waiting"
  \* The call is gone.
  /\ pc' = [pc EXCEPT ![c] = "gone"]
  \* Nothing else changes.
  /\ UNCHANGED <<held, released, mine, line, nudged, wstate, woken, notes,
                 told, use, cand, res, published>>

------------------------------------------------------------------------------
\* THE HOLDER

\* The other transaction lets the key go (OwnedLock::unlock), and the pass
\* that follows tells the front of the line "try now": its node is nudged and
\* its waker runs (Wakeup::wake_all). The wake marks a poll under way on that
\* wait, then tells every note recorded on it so far.
Release ==
  \* The other transaction holds the key.
  /\ held
  \* It lets it go,
  /\ held' = FALSE
  \* once.
  /\ released' = TRUE
  \* Is anybody in the line?
  /\ IF line # <<>>
       \* Yes: the front wait.
       THEN LET w == Head(line)
            \* With it:
            IN /\ nudged' = [nudged EXCEPT ![w] = TRUE]
               \* a poll under way on it is marked,
               /\ woken' = [woken EXCEPT ![w] = (wstate[w] = "polling")]
               \* every call noted on it is told,
               /\ told' = ToldAll(w)
               \* and its notes are used up.
               /\ notes' = [notes EXCEPT ![w] = {}]
       \* No: nobody is told anything.
       ELSE UNCHANGED <<nudged, woken, told, notes>>
  \* Nothing else changes.
  /\ UNCHANGED <<mine, line, wstate, pc, use, cand, res, published>>

------------------------------------------------------------------------------
\* ALL THE STEPS

\* One step is one step of one call, or the holder's release.
Next ==
  \* A call looks its wait up.
  \/ \E c \in Calls : Look(c)
  \* It tries the lock.
  \/ \E c \in Calls : Try(c)
  \* It publishes the wait it made.
  \/ \E c \in Calls : Publish(c)
  \* It records its note.
  \/ \E c \in Calls : Note(c)
  \* Its poll starts.
  \/ \E c \in Calls : PollStart(c)
  \* Its poll runs.
  \/ \E c \in Calls : PollBody(c)
  \* Its poll ends.
  \/ \E c \in Calls : EndPoll(c)
  \* A told call calls again.
  \/ \E c \in Calls : Rerun(c)
  \* A caller gives up.
  \/ \E c \in Calls : Drop(c)
  \* The holder lets go.
  \/ Release

\* The whole behaviour: start, then steps.
Spec == Init /\ [][Next]_vars

------------------------------------------------------------------------------
\* WHAT MUST ALWAYS HOLD

\* Every variable holds the kind of value it should.
TypeOK ==
  \* The holder's hold and its release are yes or no.
  /\ held \in BOOLEAN /\ released \in BOOLEAN
  \* So is the transaction's.
  /\ mine \in BOOLEAN
  \* The line is a list of waits.
  /\ line \in Seq(Waits)
  \* "Try now" marks are yes or no.
  /\ nudged \in [Waits -> BOOLEAN]
  \* Each wait is in a known state.
  /\ wstate \in [Waits -> {"none", "idle", "polling", "done", "dropped"}]
  \* WOKEN marks are yes or no.
  /\ woken \in [Waits -> BOOLEAN]
  \* Notes are calls.
  /\ notes \in [Waits -> SUBSET Calls]
  \* Told is yes or no.
  /\ told \in [Calls -> BOOLEAN]
  \* Each call is at a known step.
  /\ pc \in [Calls -> {"look", "try", "publish", "note", "poll", "inpoll",
                       "endpoll", "waiting", "done", "gone"}]
  \* A call polls a wait or none.
  /\ use \in [Calls -> Waits \cup {0}]
  \* A call holds an unpublished wait or none.
  /\ cand \in [Calls -> Waits \cup {0}]
  \* A poll result is one of these.
  /\ res \in [Calls -> {"none", "acquired", "mine", "pending"}]
  \* The map holds a list of waits.
  /\ published \in Seq(Waits)

\* RULE 1. The transaction has at most one wait in the line. Two would split
\* the "try now" from the calls that poll: the release tells the front one,
\* and the lookup finds the other. Bug 1 breaks it.
OneQueuedWait ==
  \* At most one place in the line.
  Len(line) <= 1

\* RULE 2. Nobody is stranded. When the key is free and the transaction
\* still waits for it, some call is running (it will poll or look), or some
\* waiting call was told (its caller will call again). Without it, a call
\* waits for ever next to a free key. Bug 2 breaks it, and so does bug 1.
NobodyStranded ==
  \* If the holder let go and the transaction does not hold the key,
  (released /\ ~mine /\ \E c \in Calls : pc[c] = "waiting")
  \* then some call
  => \E c \in Calls :
       \* is running (it will poll or look again),
       \/ pc[c] \notin {"waiting", "gone", "done"}
       \* or waits and was told (its caller will call again).
       \/ (pc[c] = "waiting" /\ told[c])
====
