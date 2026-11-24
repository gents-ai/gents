---- MODULE OpenFileTable ----
\* ===========================================================================
\* THE STORY
\* ===========================================================================
\* A database keeps one file open for every table it reads. A big database,
\* or a process allowed only a few open files, runs out. So when
\* max_open_files is set, regolith keeps the open files in a small table of
\* "slots" (SlotTable, src/env/open_file_limit/slots.rs): one open file per
\* slot, never more slots than the limit. A read that finds its file in a
\* slot "joins" the slot; a read that does not "claims" a slot, closes the
\* file that was there, opens its own, and joins that.
\*
\* Tiny example. One slot, two threads. Thread 1 reads table A: it claims
\* the empty slot, opens A, reads, leaves. Thread 2 reads table B: the slot
\* holds A and nobody is reading it, so thread 2 claims it, closes A, opens B.
\* Meanwhile thread 1 comes back for A: it finds B in the slot, so it must
\* claim the slot again (later), never read B by mistake.
\*
\* What can go wrong, and what this model checks cannot happen:
\*   - more files open at once than the limit (AtMostCap);
\*   - a file closed while a thread is in the middle of reading it, so the
\*     read fails or reads freed memory (NeverClosedInUse);
\*   - a thread that saw its file in a slot joins just as the slot is
\*     reloaded with another file, and reads the wrong file (ReadsOwnFile).
\*
\* HOW A JOIN WORKS IN THE CODE (and here). A slot has a state word: its
\* phase (empty, claimed, open) and how many readers it has. A thread looks
\* at the word and at the slot's owner, then joins with one compare-and-swap
\* (CAS) on the word that adds one reader. The CAS only compares the word,
\* not the owner, so between the look and the CAS the slot could have been
\* claimed, reloaded with another file, read and left, ending with the very
\* same word. That is why the code checks the owner again AFTER the CAS:
\* once it counts as a reader, nobody can claim the slot, so the owner it
\* sees then is final.
\*
\* ---------------------------------------------------------------------------
\* THE SECOND STORY: A READ THAT MAY NOT WAIT (decision D60)
\* ---------------------------------------------------------------------------
\* Some reads are "CacheOnly": they promise never to wait for another thread.
\* Their device read runs later as a "unit" on the reading thread's own I/O
\* queue (src/engine/io). When that unit must reopen its table and every
\* slot is busy with other threads' reads, it may NOT wait for a slot the
\* way a "Blocking" read may. Instead it "parks":
\*   1. it puts its queue on the table's parked list (SlotTable::park,
\*      register);
\*   2. it looks at every slot again: a slot that freed meanwhile it claims;
\*      every busy slot it marks WANTED (mark_or_claim);
\*   3. the unit is parked and the poll returns (Unit::park).
\* The step that makes a WANTED slot free again (the last reader leaving)
\* clears the mark and tells every queue on the parked list "a slot freed"
\* (SlotTable::freed, wake). That message waits in the queue's inbox, and
\* the queue's next poll unparks the unit and runs it again.
\*
\* Tiny example. One slot. Thread 1 (Blocking) is reading table A in it.
\* Thread 2's unit wants table B: the slot is busy, so it puts itself on the
\* list, marks the slot WANTED, parks, and returns. Thread 1 leaves: the slot
\* is free and WANTED, so thread 1 clears the mark and tells thread 2.
\* Thread 2 polls, sees the message, and runs its unit again: it claims the
\* slot and reads B.
\*
\* What can go wrong, and what this model checks cannot happen:
\*   - the unit waits for a slot after all (NeverWaits);
\*   - the unit parks and nobody ever tells it a slot freed, so it never
\*     runs again (RetryNeverLost). Three ways to get this wrong are planted
\*     below: marking before joining the list, a publish that forgets the
\*     mark, and a wake that drops a message for a unit not yet parked.
\*
\* WHAT IS MODELLED, and the code each piece mirrors:
\*   phase, holders  the state word of each slot: Slot::state (holders is
\*                   the set of reading threads; the word holds its size)
\*   owner           Slot::owner, the table whose file the slot holds
\*   desc            Slot::file, the open file (a number per opening)
\*   alive, fileOf   the files open right now, and which table each is
\*   wanted          the WANTED bit of each slot's state word
\*   list            SlotTable::parked, the queues waiting for a free slot
\*   told            a SlotFreed message waiting in a thread's queue inbox
\*   ustate          whether a thread's unit is parked (Unit, PARKED)
\*
\* WHAT IS LEFT OUT, and why that loses nothing:
\*   - The CLOCK reference bit: it only chooses WHICH free slot a claim
\*     takes, never lets a claim take a slot with readers. The CLOCK sweep
\*     of a unit can miss a free slot; here a unit's sweep may always give
\*     up (SweepMiss), which covers every way it can miss.
\*   - The DRAIN bit: it makes a starved Blocking thread wait for one slot's
\*     current readers instead of looking forever; like CLOCK it changes
\*     which claim happens and when, not whether a claim can take a slot
\*     with readers. The loom model a_starved_open_drains_a_busy_slot checks
\*     it, and the unit test a_drained_slot_keeps_its_mark_and_the_drainers_
\*     leave_wakes checks that a drain keeps the WANTED mark.
\*   - Memory ordering: the loom models (tests/loom_tables.rs) check it.
\*   - Removing a table while it is read (renamed aside, unlinked by the
\*     last reader): it never closes a slot's file, so it adds no step here.
\*   - A claim that fails to open its file and empties its slot: it wakes
\*     the parked list exactly like a leave (SlotTable::settle).
\*
\* CONFIGURATIONS (each GREEN must pass, each RED must break the one
\* invariant it names):
\*   MC_OpenFileTable_Green           one slot, two threads, two tables.
\*   MC_OpenFileTable_Green_TwoSlots  two slots, three threads.
\*   MC_OpenFileTable_Green_NoWait    one slot; thread 2 is a queue unit;
\*                                    the liveness check UnitsFinish too.
\*   MC_OpenFileTable_Green_NoWaitTwoSlots  two slots; threads 2 and 3 are
\*                                    units, on the parked list together.
\*   MC_OpenFileTable_Red_IgnoreReaders   a claim that takes a slot whatever
\*                                    its readers: NeverClosedInUse breaks.
\*   MC_OpenFileTable_Red_NoOwnerRecheck  a join that trusts the owner it
\*                                    saw before its CAS: ReadsOwnFile breaks.
\*   MC_OpenFileTable_Red_OutsideTable    a thread that finds no free slot
\*                                    opens its file outside the table:
\*                                    AtMostCap breaks.
\*   MC_OpenFileTable_Red_Waits       a unit that waits for a slot like a
\*                                    Blocking read: NeverWaits breaks.
\*   MC_OpenFileTable_Red_MarkFirst   a park that marks before it joins the
\*                                    list: RetryNeverLost breaks.
\*   MC_OpenFileTable_Red_PublishDropsWanted  a publish that rewrites the
\*                                    word without the mark: RetryNeverLost
\*                                    breaks.
\*   MC_OpenFileTable_Red_WakeNeedsParked  a wake that tells only units
\*                                    already parked: RetryNeverLost breaks.
\*
\* Lean, proofs/lean/Regolith/OpenFileTable.lean, proves the first three
\* rules for every number of slots, threads and steps;
\* proofs/lean/Regolith/OpenFileNoWait.lean proves NeverWaits and
\* RetryNeverLost the same way.

\* We use numbers and finite sets.
EXTENDS Naturals, FiniteSets

\* The fixed inputs of a configuration.
CONSTANTS
  \* The threads that read.
  Threads,
  \* The tables (files) they read, named by numbers above 0.
  Files,
  \* How many slots the table has: max_open_files.
  Cap,
  \* [Threads -> Files]: the one table each thread reads.
  Want,
  \* How many times each thread reads its table.
  Reads,
  \* The most files that may ever be opened in one run (a bound for TLC).
  MaxDesc,
  \* The threads whose reads are queue units, which never wait for a slot
  \* (CacheOnly reads, D60). The others are Blocking reads.
  NoWait,
  \* "none" for the real code, or the name of one planted bug.
  Mutant

\* The slot numbers.
Slots == 1..Cap
\* The numbers a file opening can get.
Descs == 1..MaxDesc
\* "No file" in a slot, and "no owner".
None == 0

\* The limit is at least one slot.
ASSUME Cap \in Nat \ {0}
\* Every thread reads one table.
ASSUME Want \in [Threads -> Files]
\* Table names are positive, so None never names a table.
ASSUME \A f \in Files : f \in Nat \ {0}
\* The queue units are some of the threads.
ASSUME NoWait \subseteq Threads
\* The bug names this model knows.
ASSUME Mutant \in {"none", "IgnoreReaders", "NoOwnerRecheck", "OutsideTable",
                   \* and the D60 bugs: a waiting unit, a park that marks first, a publish
                   \* that drops the mark, a wake that tells only parked units.
                   "Waits", "MarkFirst", "PublishDropsWanted", "WakeNeedsParked"}

\* For the configurations: thread 1 reads table 1, thread 2 reads table 2.
WantTwo == [t \in {1, 2} |-> t]
\* For the configurations: threads 1 and 2 read table 1, thread 3 table 2.
WantThree == [t \in {1, 2, 3} |-> IF t = 3 THEN 2 ELSE 1]

\* The state that changes from step to step.
VARIABLES
  \* [Slots -> {"empty","claimed","open"}]: the phase bits of each slot's
  \* state word (Slot::state).
  phase,
  \* [Slots -> SUBSET Threads]: the threads counted as readers of each
  \* slot; the state word holds how many there are.
  holders,
  \* [Slots -> Files \cup {None}]: which table each slot's file is
  \* (Slot::owner).
  owner,
  \* [Slots -> Descs \cup {None}]: the open file in each slot (Slot::file).
  desc,
  \* The open files right now: every number here is a real open file.
  alive,
  \* [Descs -> Files \cup {None}]: which table each opening opened.
  fileOf,
  \* The next opening's number.
  next,
  \* [Threads -> step name]: where each thread is in its read.
  pc,
  \* [Threads -> Slots \cup {None}]: the slot a thread is joining, holds, or
  \* has just freed.
  at,
  \* [Threads -> [phase, count]]: the state word a thread looked at before
  \* its CAS.
  seen,
  \* [Threads -> Descs \cup {None}]: the open file a thread reads through.
  using,
  \* [Threads -> Files \cup {None}]: the table a thread's last read returned.
  got,
  \* [Threads -> Nat]: how many reads each thread still has to do.
  left,
  \* [Slots -> BOOLEAN]: the WANTED bit of each slot's state word: a parked
  \* unit waits for this slot to free.
  wanted,
  \* SUBSET Threads: the queues on the table's parked list (SlotTable::parked).
  list,
  \* [Threads -> BOOLEAN]: a "slot freed" message (Message::SlotFreed) waits
  \* in the thread's queue inbox.
  told,
  \* [Threads -> {"running","parked"}]: whether the thread's unit is parked
  \* (Unit's PARKED state) or not.
  ustate,
  \* [Threads -> 1..(Cap + 1)]: the next slot a park looks at
  \* (SlotTable::mark_or_claim's loop).
  markAt

\* All the variables, for "nothing else changes".
vars == <<phase, holders, owner, desc, alive, fileOf, next, pc, at, seen,
          \* (continued: the rest of a read, then the second story's variables)
          using, got, left, wanted, list, told, ustate, markAt>>

\* The variables of the second story, for "nothing of the park changes".
parkVars == <<wanted, list, told, ustate, markAt>>

\* Every variable holds the kind of value it should.
TypeOK ==
  \* Each slot is in one of the three phases.
  /\ phase \in [Slots -> {"empty", "claimed", "open"}]
  \* Each slot's readers are some of the threads.
  /\ holders \in [Slots -> SUBSET Threads]
  \* Each slot's owner is a table or nobody.
  /\ owner \in [Slots -> Files \cup {None}]
  \* Each slot holds an opening or nothing.
  /\ desc \in [Slots -> Descs \cup {None}]
  \* The open files are openings.
  /\ alive \subseteq Descs
  \* Each opening opened a table, or has not happened yet.
  /\ fileOf \in [Descs -> Files \cup {None}]
  \* The next opening number is in range (one past the last at most).
  /\ next \in 1..(MaxDesc + 1)
  \* Each thread is at one of the steps of a read.
  /\ pc \in [Threads -> {"start", "cas", "recheck", "claim", "close",
                          \* (continued: the rest of a read's steps)
                          "open", "read", "leave", "done",
                          \* (continued: a unit's park, step by step)
                          "register", "mark", "parkwake", "park", "idle",
                          \* (continued: a leave that clears a mark and wakes the parked)
                          "clearW", "wake"}]
  \* Each thread names a slot or none.
  /\ at \in [Threads -> Slots \cup {None}]
  \* Each thread reads through an opening or none.
  /\ using \in [Threads -> Descs \cup {None}]
  \* Each thread's last answer is a table or nothing yet.
  /\ got \in [Threads -> Files \cup {None}]
  \* Each thread has some reads left.
  /\ left \in [Threads -> 0..Reads]
  \* Each slot's WANTED bit is a yes or a no.
  /\ wanted \in [Slots -> BOOLEAN]
  \* The parked list holds some threads' queues.
  /\ list \subseteq Threads
  \* Each queue's inbox holds a "slot freed" message or not.
  /\ told \in [Threads -> BOOLEAN]
  \* Each unit is running or parked.
  /\ ustate \in [Threads -> {"running", "parked"}]
  \* Each park's next slot is a slot, or one past the last when done.
  /\ markAt \in [Threads -> 1..(Cap + 1)]

\* The start: every slot empty, no file open, every thread about to read.
Init ==
  \* No slot holds anything.
  /\ phase = [s \in Slots |-> "empty"]
  \* Nobody reads any slot.
  /\ holders = [s \in Slots |-> {}]
  \* No slot has an owner.
  /\ owner = [s \in Slots |-> None]
  \* No slot has a file.
  /\ desc = [s \in Slots |-> None]
  \* No file is open.
  /\ alive = {}
  \* No opening has happened.
  /\ fileOf = [d \in Descs |-> None]
  \* The first opening will be number 1.
  /\ next = 1
  \* Every thread starts its first read.
  /\ pc = [t \in Threads |-> "start"]
  \* No thread is at a slot.
  /\ at = [t \in Threads |-> None]
  \* No thread has looked at a word yet.
  /\ seen = [t \in Threads |-> [phase |-> "empty", count |-> 0]]
  \* No thread reads through a file.
  /\ using = [t \in Threads |-> None]
  \* No thread has an answer.
  /\ got = [t \in Threads |-> None]
  \* Every thread has all its reads to do.
  /\ left = [t \in Threads |-> Reads]
  \* No slot is wanted.
  /\ wanted = [s \in Slots |-> FALSE]
  \* Nobody is on the parked list.
  /\ list = {}
  \* No inbox holds a message.
  /\ told = [t \in Threads |-> FALSE]
  \* No unit is parked.
  /\ ustate = [t \in Threads |-> "running"]
  \* No park is under way; each would start at slot 1.
  /\ markAt = [t \in Threads |-> 1]

\* A slot a claim may take: empty, or open with nobody reading it. The bug
\* IgnoreReaders also takes an open slot that has readers.
Claimable(s) ==
  \* An empty slot;
  \/ phase[s] = "empty"
  \* or an open slot
  \/ /\ phase[s] = "open"
     \* that nobody reads (the bug IgnoreReaders does not ask).
     /\ (holders[s] = {} \/ Mutant = "IgnoreReaders")

\* Who a wake tells. The real code tells every queue it took off the list;
\* the bug WakeNeedsParked tells only those whose unit is already parked,
\* dropping the message for a unit that is still on its way to parking.
Telling(taken) ==
  \* With the bug WakeNeedsParked ...
  IF Mutant = "WakeNeedsParked"
  \* ... only the queues whose unit is parked already;
  THEN {u \in taken : ustate[u] = "parked"}
  \* the real code tells every queue it took off the list.
  ELSE taken

\* Thread t looks at slot s and finds its own table there, open: it
\* remembers the state word it saw and goes to join with a CAS
\* (SlotTable::join, the check before the CAS).
Look(t, s) ==
  \* The thread is starting a read and has reads left.
  /\ pc[t] = "start"
  \* (It has reads left.)
  /\ left[t] > 0
  \* The slot is open and holds the table this thread wants.
  /\ phase[s] = "open"
  \* (It holds the table this thread wants.)
  /\ owner[s] = Want[t]
  \* It remembers the word: the phase and how many readers.
  /\ seen' = [seen EXCEPT ![t] = [phase |-> phase[s], count |-> Cardinality(holders[s])]]
  \* It remembers the slot.
  /\ at' = [at EXCEPT ![t] = s]
  \* Next it tries the CAS.
  /\ pc' = [pc EXCEPT ![t] = "cas"]
  \* Nothing else changes.
  /\ UNCHANGED <<phase, holders, owner, desc, alive, fileOf, next, using, got, left>>
  \* Nothing of the second story changes: marks, list, messages, units, looks.
  /\ UNCHANGED parkVars

\* Thread t decides to load its table into a slot: its file is not where it
\* looked, or it did not look (a stale hint). Duplicates are allowed: the
\* code allows two slots to hold one table.
Miss(t) ==
  \* The thread is starting a read and has reads left.
  /\ pc[t] = "start"
  \* (It has reads left.)
  /\ left[t] > 0
  \* It goes to claim a slot.
  /\ pc' = [pc EXCEPT ![t] = "claim"]
  \* Nothing else changes.
  /\ UNCHANGED <<phase, holders, owner, desc, alive, fileOf, next, at, seen, using, got, left>>
  \* Nothing of the second story changes: marks, list, messages, units, looks.
  /\ UNCHANGED parkVars

\* The join's CAS: it succeeds only if the slot's word is exactly the word
\* the thread saw, and then adds this thread as a reader. It compares the
\* word only, never the owner: that is the gap the recheck closes.
Cas(t) ==
  \* The thread is at its CAS, on its slot s.
  /\ pc[t] = "cas"
  \* s is the slot the thread is at.
  /\ LET s == at[t] IN
       \* The word now equals the word it saw ...
       IF /\ phase[s] = seen[t].phase
          \* and the count of readers equals the count it saw ...
          /\ Cardinality(holders[s]) = seen[t].count
       \* ... so the CAS wins: the thread counts as a reader, then rechecks.
       THEN /\ holders' = [holders EXCEPT ![s] = holders[s] \cup {t}]
            \* It rechecks next.
            /\ pc' = [pc EXCEPT ![t] = "recheck"]
            \* It stays at its slot.
            /\ UNCHANGED at
       \* ... otherwise the CAS fails and the thread starts over.
       ELSE /\ pc' = [pc EXCEPT ![t] = "start"]
            \* It is at no slot.
            /\ at' = [at EXCEPT ![t] = None]
            \* The readers do not change.
            /\ UNCHANGED holders
  \* Nothing else changes.
  /\ UNCHANGED <<phase, owner, desc, alive, fileOf, next, seen, using, got, left>>
  \* Nothing of the second story changes: marks, list, messages, units, looks.
  /\ UNCHANGED parkVars

\* After the CAS the thread checks the owner again. Counted as a reader, the
\* slot cannot be claimed under it, so this answer is final. The bug
\* NoOwnerRecheck skips this check.
Recheck(t) ==
  \* The thread won its CAS on slot s.
  /\ pc[t] = "recheck"
  \* s is the slot the thread is at.
  /\ LET s == at[t] IN
       \* The slot still holds its table (or the bug does not look) ...
       IF owner[s] = Want[t] \/ Mutant = "NoOwnerRecheck"
       \* ... so it reads through the slot's open file.
       THEN /\ using' = [using EXCEPT ![t] = desc[s]]
            \* It reads next.
            /\ pc' = [pc EXCEPT ![t] = "read"]
            \* It stays a reader of its slot.
            /\ UNCHANGED <<holders, at>>
       \* ... otherwise the slot was reloaded with another table: it stops
       \* counting as a reader (the code drops its hold, Held::drop, the same
       \* decrement as a leave) and starts over.
       ELSE /\ holders' = [holders EXCEPT ![s] = holders[s] \ {t}]
            \* It reads through nothing.
            /\ UNCHANGED using
            \* Like a leave, a drop that frees a WANTED slot goes on to clear
            \* the mark and wake the list; this read has not happened, so
            \* `left` stays, and the wake sends the thread back to its start.
            /\ IF holders[s] = {t} /\ phase[s] = "open" /\ wanted[s]
               \* It freed a WANTED slot: it clears the mark next.
               THEN /\ pc' = [pc EXCEPT ![t] = "clearW"]
                    \* It stays at its slot.
                    /\ UNCHANGED at
               \* Otherwise it starts over.
               ELSE /\ pc' = [pc EXCEPT ![t] = "start"]
                    \* It is at no slot now.
                    /\ at' = [at EXCEPT ![t] = None]
  \* Nothing else changes.
  /\ UNCHANGED <<phase, owner, desc, alive, fileOf, next, seen, got, left>>
  \* Nothing of the second story changes: marks, list, messages, units, looks.
  /\ UNCHANGED parkVars

\* Thread t claims slot s with one CAS: from empty, or from open with no
\* reader (SlotTable::try_claim). Claimed, the slot is this thread's alone.
\* For a queue unit this is its one sweep finding a free slot. The WANTED
\* bit is kept: the slot is busy again, and a parked unit still needs it.
Claim(t, s) ==
  \* The thread wants a slot.
  /\ pc[t] = "claim"
  \* This slot may be taken.
  /\ Claimable(s)
  \* The CAS moves it to "claimed" (wanted is not touched).
  /\ phase' = [phase EXCEPT ![s] = "claimed"]
  \* The thread remembers it.
  /\ at' = [at EXCEPT ![t] = s]
  \* Next it closes the old file.
  /\ pc' = [pc EXCEPT ![t] = "close"]
  \* Nothing else changes.
  /\ UNCHANGED <<holders, owner, desc, alive, fileOf, next, seen, using, got, left>>
  \* Nothing of the second story changes: marks, list, messages, units, looks.
  /\ UNCHANGED parkVars

\* The bug OutsideTable: a thread that finds no slot to claim opens its file
\* anyway, outside the table, instead of waiting for a slot.
ClaimOutside(t) ==
  \* Only with the bug.
  /\ Mutant = "OutsideTable"
  \* The thread wants a slot ...
  /\ pc[t] = "claim"
  \* ... and none may be taken.
  /\ \A s \in Slots : ~Claimable(s)
  \* There is an opening number left.
  /\ next <= MaxDesc
  \* It opens its table: one more open file.
  /\ alive' = alive \cup {next}
  \* That opening is its table.
  /\ fileOf' = [fileOf EXCEPT ![next] = Want[t]]
  \* The next opening gets the next number.
  /\ next' = next + 1
  \* It reads through it, at no slot.
  /\ using' = [using EXCEPT ![t] = next]
  \* It is at no slot.
  /\ at' = [at EXCEPT ![t] = None]
  \* It reads next.
  /\ pc' = [pc EXCEPT ![t] = "read"]
  \* Nothing else changes.
  /\ UNCHANGED <<phase, holders, owner, desc, seen, got, left>>
  \* Nothing of the second story changes: marks, list, messages, units, looks.
  /\ UNCHANGED parkVars

\* A queue unit's one sweep comes back empty-handed (SlotTable::sweep
\* returned None). The CLOCK sweep can miss a free slot, so this step is
\* allowed whatever the slots hold. The unit does not wait: it goes to park.
\* The bug Waits removes this step, so a unit that finds no free slot sits
\* at "claim" like a Blocking read, waiting for another thread.
SweepMiss(t) ==
  \* Only a queue unit takes this step.
  /\ t \in NoWait
  \* Not with the bug Waits, which has no such step.
  /\ Mutant # "Waits"
  \* The unit wants a slot.
  /\ pc[t] = "claim"
  \* The real code joins the list first; the bug MarkFirst marks first.
  /\ pc' = [pc EXCEPT ![t] = IF Mutant = "MarkFirst" THEN "mark" ELSE "register"]
  \* Its look at the slots starts at slot 1.
  /\ markAt' = [markAt EXCEPT ![t] = 1]
  \* Nothing else changes.
  /\ UNCHANGED <<phase, holders, owner, desc, alive, fileOf, next, at, seen, using, got, left>>
  \* These stay as they are too.
  /\ UNCHANGED <<wanted, list, told, ustate>>

\* The unit puts its queue on the parked list (SlotTable::register): one
\* push. From here on, any wake tells this queue.
Register(t) ==
  \* The unit is about to register.
  /\ pc[t] = "register"
  \* Its queue joins the list.
  /\ list' = list \cup {t}
  \* The real code then looks at the slots; the bug MarkFirst already did,
  \* and parks now.
  /\ pc' = [pc EXCEPT ![t] = IF Mutant = "MarkFirst" THEN "park" ELSE "mark"]
  \* Nothing else changes.
  /\ UNCHANGED <<phase, holders, owner, desc, alive, fileOf, next, at, seen, using, got, left>>
  \* These stay as they are too.
  /\ UNCHANGED <<wanted, told, ustate, markAt>>

\* The unit looks at slot markAt[t] once (one CAS in
\* SlotTable::mark_or_claim). A slot that freed since the sweep it claims;
\* a busy one it marks WANTED and moves on.
Mark(t) ==
  \* The unit is looking at the slots.
  /\ pc[t] = "mark"
  \* It has a slot left to look at.
  /\ markAt[t] <= Cap
  \* Call the slot the unit looks at s.
  /\ LET s == markAt[t] IN
       \* The slot freed meanwhile: the CAS claims it, keeping its mark.
       IF Claimable(s)
       \* The CAS claims the slot.
       THEN /\ phase' = [phase EXCEPT ![s] = "claimed"]
            \* The unit remembers the slot it now holds.
            /\ at' = [at EXCEPT ![t] = s]
            \* The real code wakes the list before filling the slot; the bug
            \* MarkFirst never joined the list, and fills at once.
            /\ pc' = [pc EXCEPT ![t] = IF Mutant = "MarkFirst" THEN "close" ELSE "parkwake"]
            \* The mark of this slot and the look position stay as they are.
            /\ UNCHANGED <<wanted, markAt>>
       \* The slot is busy: the CAS sets its WANTED bit (already set stays
       \* set), and the unit moves on to the next slot.
       ELSE /\ wanted' = [wanted EXCEPT ![s] = TRUE]
            \* The look moves on to the next slot.
            /\ markAt' = [markAt EXCEPT ![t] = s + 1]
            \* Nothing about the slot's phase, or the unit's place, changes.
            /\ UNCHANGED <<phase, at, pc>>
  \* Nothing else changes.
  /\ UNCHANGED <<holders, owner, desc, alive, fileOf, next, seen, using, got, left>>
  \* These stay as they are too.
  /\ UNCHANGED <<list, told, ustate>>

\* The unit has looked at every slot and every one was busy (Parking::Parked).
MarkDone(t) ==
  \* The unit is looking at the slots.
  /\ pc[t] = "mark"
  \* It has looked at every slot: the look went past the last one.
  /\ markAt[t] > Cap
  \* The real code parks now; the bug MarkFirst only now joins the list.
  /\ pc' = [pc EXCEPT ![t] = IF Mutant = "MarkFirst" THEN "register" ELSE "park"]
  \* Nothing else changes.
  /\ UNCHANGED <<phase, holders, owner, desc, alive, fileOf, next, at, seen, using, got, left>>
  \* Nothing of the second story changes: marks, list, messages, units, looks.
  /\ UNCHANGED parkVars

\* A park that claimed a slot takes the whole parked list and tells every
\* queue on it (SlotTable::park, then wake): a slot did free, and this
\* park's own entry leaves the list. Then it fills the slot as a claim does.
ParkWake(t) ==
  \* The unit's look claimed a slot.
  /\ pc[t] = "parkwake"
  \* Every queue it told gets a "slot freed" message in its inbox.
  /\ told' = [u \in Threads |-> told[u] \/ u \in Telling(list)]
  \* The list is taken whole: it is empty now.
  /\ list' = {}
  \* Next it closes the slot's old file and opens its own.
  /\ pc' = [pc EXCEPT ![t] = "close"]
  \* Nothing else changes.
  /\ UNCHANGED <<phase, holders, owner, desc, alive, fileOf, next, at, seen, using, got, left>>
  \* These stay as they are too.
  /\ UNCHANGED <<wanted, ustate, markAt>>

\* The unit is parked and the poll returns (Unit::park): the thread is free
\* to do other work. Its read is still pending on its queue.
Park(t) ==
  \* Every slot was busy.
  /\ pc[t] = "park"
  \* The unit is parked.
  /\ ustate' = [ustate EXCEPT ![t] = "parked"]
  \* The thread is idle as far as this read goes.
  /\ pc' = [pc EXCEPT ![t] = "idle"]
  \* Nothing else changes.
  /\ UNCHANGED <<phase, holders, owner, desc, alive, fileOf, next, at, seen, using, got, left>>
  \* These stay as they are too.
  /\ UNCHANGED <<wanted, list, told, markAt>>

\* The queue's owner polls and finds a "slot freed" message (IoQueue::poll
\* taking the inbox, then IoQueue::unpark): the unit is unparked and its
\* read starts over, joining its slot or claiming one.
Poll(t) ==
  \* The read is parked: its poll has returned.
  /\ pc[t] = "idle"
  \* A message waits in its inbox.
  /\ told[t]
  \* The message is taken.
  /\ told' = [told EXCEPT ![t] = FALSE]
  \* The unit runs again.
  /\ ustate' = [ustate EXCEPT ![t] = "running"]
  \* Its read starts over.
  /\ pc' = [pc EXCEPT ![t] = "start"]
  \* Nothing else changes.
  /\ UNCHANGED <<phase, holders, owner, desc, alive, fileOf, next, at, seen, using, got, left>>
  \* These stay as they are too.
  /\ UNCHANGED <<wanted, list, markAt>>

\* The claimer closes the file its slot held, before opening its own: the
\* table never holds more than one file per slot, even for an instant.
Close(t) ==
  \* The thread claimed slot s.
  /\ pc[t] = "close"
  \* s is the slot the thread claimed.
  /\ LET s == at[t] IN
       \* The old file is closed (if there was one).
       /\ alive' = alive \ {desc[s]}
       \* The slot holds no file.
       /\ desc' = [desc EXCEPT ![s] = None]
  \* Next it opens its own.
  /\ pc' = [pc EXCEPT ![t] = "open"]
  \* Nothing else changes.
  /\ UNCHANGED <<phase, holders, owner, fileOf, next, at, seen, using, got, left>>
  \* Nothing of the second story changes: marks, list, messages, units, looks.
  /\ UNCHANGED parkVars

\* The claimer opens its table in the slot and publishes it open, counting
\* itself as the one reader (SlotTable::fill). The publish is an add that
\* keeps the WANTED bit; the bug PublishDropsWanted writes the word whole
\* and loses it.
Open(t) ==
  \* The thread closed the old file of slot s.
  /\ pc[t] = "open"
  \* There is an opening number left.
  /\ next <= MaxDesc
  \* s is the slot the thread claimed.
  /\ LET s == at[t] IN
       \* Its table is now open ...
       /\ alive' = alive \cup {next}
       \* ... as this opening ...
       /\ fileOf' = [fileOf EXCEPT ![next] = Want[t]]
       \* ... held in the slot ...
       /\ desc' = [desc EXCEPT ![s] = next]
       \* ... whose owner is now this table ...
       /\ owner' = [owner EXCEPT ![s] = Want[t]]
       \* ... and which is open, with this thread as a reader.
       /\ phase' = [phase EXCEPT ![s] = "open"]
       \* (This thread is one of its readers.)
       /\ holders' = [holders EXCEPT ![s] = holders[s] \cup {t}]
       \* The mark stays, unless the bug drops it.
       /\ wanted' = IF Mutant = "PublishDropsWanted"
                    \* the bug: the mark is lost;
                    THEN [wanted EXCEPT ![s] = FALSE]
                    \* the real code: every mark stays.
                    ELSE wanted
  \* The thread reads through the new opening.
  /\ using' = [using EXCEPT ![t] = next]
  \* The next opening gets the next number.
  /\ next' = next + 1
  \* Next it reads.
  /\ pc' = [pc EXCEPT ![t] = "read"]
  \* Nothing else changes.
  /\ UNCHANGED <<at, seen, got, left>>
  \* These stay as they are too.
  /\ UNCHANGED <<list, told, ustate, markAt>>

\* The thread reads through its open file: it gets that file's table.
Read(t) ==
  \* The thread is ready to read.
  /\ pc[t] = "read"
  \* It gets the table its open file is (nothing, if it holds no file).
  /\ got' = [got EXCEPT ![t] = IF using[t] \in Descs THEN fileOf[using[t]] ELSE None]
  \* Next it leaves.
  /\ pc' = [pc EXCEPT ![t] = "leave"]
  \* Nothing else changes.
  /\ UNCHANGED <<phase, holders, owner, desc, alive, fileOf, next, at, seen, using, left>>
  \* Nothing of the second story changes: marks, list, messages, units, looks.
  /\ UNCHANGED parkVars

\* The thread stops counting as a reader (Held::drop): one decrement. A file
\* the bug opened outside the table is closed here. A leave that makes a
\* WANTED slot free (it was the last reader of an open slot) goes on to
\* clear the mark and wake the parked list.
Leave(t) ==
  \* The thread has read.
  /\ pc[t] = "leave"
  \* Did this leave free a wanted slot?
  /\ LET s == at[t]
         \* frees: it read through a slot ...
         frees == /\ s # None
                  \* ... it is the slot's last reader ...
                  /\ holders[s] = {t}
                  \* ... the slot is open ...
                  /\ phase[s] = "open"
                  \* ... and marked WANTED.
                  /\ wanted[s]
     \* (that is the definition; the step follows)
     IN
     \* If it read through a slot, it leaves the slot's readers ...
     /\ IF s # None
        \* It stops counting as a reader of its slot.
        THEN /\ holders' = [holders EXCEPT ![s] = holders[s] \ {t}]
             \* No file closes.
             /\ UNCHANGED alive
        \* ... otherwise it closes its outside file.
        ELSE /\ alive' = alive \ {using[t]}
             \* Nobody's reading changes.
             /\ UNCHANGED holders
     \* A wanted slot it freed: it keeps the slot in mind and wakes next.
     /\ IF frees
        \* It freed a WANTED slot: it clears the mark next.
        THEN /\ pc' = [pc EXCEPT ![t] = "clearW"]
             \* It stays at its slot.
             /\ UNCHANGED at
        \* Otherwise it starts its next read, or is done.
        ELSE /\ pc' = [pc EXCEPT ![t] = IF left[t] = 1 THEN "done" ELSE "start"]
             \* It is at no slot now.
             /\ at' = [at EXCEPT ![t] = None]
  \* One read fewer to do.
  /\ left' = [left EXCEPT ![t] = left[t] - 1]
  \* It is no longer reading.
  /\ using' = [using EXCEPT ![t] = None]
  \* Nothing else changes.
  /\ UNCHANGED <<phase, owner, desc, fileOf, next, seen, got>>
  \* Nothing of the second story changes: marks, list, messages, units, looks.
  /\ UNCHANGED parkVars

\* The leaver clears the slot's WANTED bit (SlotTable::freed, fetch_and),
\* before it takes the list. Clearing first means a park that saw the mark
\* still set, and so did not mark again, is already on the list it takes.
ClearW(t) ==
  \* The leaver freed a wanted slot.
  /\ pc[t] = "clearW"
  \* The mark is cleared.
  /\ wanted' = [wanted EXCEPT ![at[t]] = FALSE]
  \* Next it wakes the list.
  /\ pc' = [pc EXCEPT ![t] = "wake"]
  \* Nothing else changes.
  /\ UNCHANGED <<phase, holders, owner, desc, alive, fileOf, next, at, seen, using, got, left>>
  \* These stay as they are too.
  /\ UNCHANGED <<list, told, ustate, markAt>>

\* The leaver takes the parked list whole and tells every queue on it
\* (SlotTable::wake, then QueueShared::slot_freed): each gets a message in
\* its inbox, whatever its unit is doing right now.
Wake(t) ==
  \* The leaver cleared the mark.
  /\ pc[t] = "wake"
  \* Every queue it tells gets a "slot freed" message.
  /\ told' = [u \in Threads |-> told[u] \/ u \in Telling(list)]
  \* The list is taken whole.
  /\ list' = {}
  \* The leaver is done with the slot.
  /\ at' = [at EXCEPT ![t] = None]
  \* It starts its next read, or is done (left already counts this read).
  /\ pc' = [pc EXCEPT ![t] = IF left[t] = 0 THEN "done" ELSE "start"]
  \* Nothing else changes.
  /\ UNCHANGED <<phase, holders, owner, desc, alive, fileOf, next, seen, using, got, left>>
  \* These stay as they are too.
  /\ UNCHANGED <<wanted, ustate, markAt>>

\* Every step thread t can take on its own.
OwnStep(t) ==
  \* It looks at a slot and finds its table there;
  \/ \E s \in Slots : Look(t, s)
  \* or it finds its table in no slot;
  \/ Miss(t)
  \* or it tries its compare-and-swap;
  \/ Cas(t)
  \* or it rechecks the slot it joined;
  \/ Recheck(t)
  \* or it claims a slot for its table;
  \/ \E s \in Slots : Claim(t, s)
  \* or, with no slot to claim, it opens the file outside the table;
  \/ ClaimOutside(t)
  \* or, a unit, its sweep comes back empty;
  \/ SweepMiss(t)
  \* or, a unit, it joins the parked list;
  \/ Register(t)
  \* or, a unit, it looks at one slot;
  \/ Mark(t)
  \* or, a unit, its look is done;
  \/ MarkDone(t)
  \* or, a unit that claimed in its look, it wakes the list;
  \/ ParkWake(t)
  \* or, a unit, it parks;
  \/ Park(t)
  \* or, a unit, its poll takes its message;
  \/ Poll(t)
  \* or it closes the file of the slot it claimed;
  \/ Close(t)
  \* or it opens its table's file in that slot;
  \/ Open(t)
  \* or it reads;
  \/ Read(t)
  \* or it leaves;
  \/ Leave(t)
  \* or, having freed a wanted slot, it clears the mark;
  \/ ClearW(t)
  \* or, having cleared the mark, it wakes the list.
  \/ Wake(t)

\* Every step any thread can take.
Next == \E t \in Threads : OwnStep(t)

\* The behaviours: start in Init, take Next steps.
Spec == Init /\ [][Next]_vars

\* The behaviours where no thread stops for good while it has a step: each
\* thread's steps are weakly fair. Used for the liveness check below.
FairSpec == Spec /\ \A t \in Threads : WF_vars(OwnStep(t))

\* ===========================================================================
\* THE RULES
\* ===========================================================================

\* Never more open files than slots. Rules out: every slot busy, and a
\* thread opening its table anyway, so the process holds one file more than
\* max_open_files allows. The code keeps every open file in a slot.
AtMostCap == Cardinality(alive) <= Cap

\* A file a thread is reading through is still open. Rules out: thread 1
\* mid-read on table A's file, thread 2 claims the slot and closes it, and
\* thread 1's read fails or reads a closed file. The code's claim CAS only
\* wins with no readers counted.
NeverClosedInUse ==
  \* For every thread that is reading or about to leave, its file is open.
  \A t \in Threads : pc[t] \in {"read", "leave"} => using[t] \in alive

\* A read returns the table its thread asked for. Rules out: a thread sees
\* A in the slot, the slot is reloaded with B, read and left with the same
\* state word, and the thread's CAS joins B. The recheck after the CAS
\* catches it.
ReadsOwnFile ==
  \* For every thread that has read, what it got is the table it wanted.
  \A t \in Threads : pc[t] = "leave" => got[t] = Want[t]

\* A queue unit never waits for another thread (D60). Whatever state the
\* other threads are in, a unit that is in the middle of its read (not
\* parked with its poll returned, not done) always has a step of its own it
\* can take. Rules out: thread 1 reads in the only slot, thread 2's unit
\* finds no free slot and sits at "claim" until thread 1 leaves, so the
\* poll that ran it waits on thread 1. A Blocking read may do exactly that.
NeverWaits ==
  \* For every unit in the middle of a read: some step of its own can be taken.
  \A t \in NoWait : pc[t] \notin {"idle", "done"} => ENABLED OwnStep(t)

\* A parked unit is never forgotten. Each parked unit already has a "slot
\* freed" message waiting, or will get one: its queue is on the parked list
\* and some slot is WANTED and busy (the step that frees it wakes the
\* list), or a leaver that freed a wanted slot is on its way to waking the
\* list. Rules out: thread 2 marks the slot, thread 1 leaves and wakes an
\* empty list, then thread 2 joins the list and parks: nobody will ever
\* tell it, and its read never runs again.
RetryNeverLost ==
  \* For every parked unit:
  \A t \in NoWait : ustate[t] = "parked" =>
    \* its message is there already, or
    \/ told[t]
    \* its queue is on the list, and
    \/ /\ t \in list
       \* some slot is marked and busy, or
       /\ \/ \E s \in Slots : wanted[s] /\ ~Claimable(s)
          \* a reader that freed a marked slot is on its way to waking the list.
          \/ \E r \in Threads : pc[r] \in {"clearW", "wake"}

\* Liveness, under fair scheduling: every queue unit finishes all its reads.
\* Checked by the GREEN no-wait configurations.
UnitsFinish == \A t \in NoWait : <>(pc[t] = "done")

====
