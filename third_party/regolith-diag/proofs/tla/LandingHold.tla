---- MODULE LandingHold ----
\* ===========================================================================
\* THE STORY
\* ===========================================================================
\* A read through a CacheOnly handle never touches the disk. When a block it
\* needs is not in the block cache, the read records a wait on its thread's
\* queue and returns WouldBlock; the queue's owner reads the block at its
\* next poll, and the caller runs the read again. With no block cache, the
\* read run again finds the block only in what landed for its queue
\* (QueueShared::landed), and that table is bounded: past the bound the
\* oldest landings go (src/io_queue/ledger.rs).
\*
\* Some calls need several blocks at once: a multi_get, a scan that collects
\* a range, a read that merges operands from several tables. Such a call
\* starts over each time it is run. Picture a multi_get of keys a and b, each
\* in a block that takes the whole bound. Run 1 misses a. The poll lands a.
\* Run 2 reads a and misses b. The poll lands b, and to fit the bound it
\* drops the oldest landing: a. Run 3 misses a. The poll lands a and drops b.
\* Every wait completes, and the call never does.
\*
\* The fix: a landing that a call run again still needs is held, and only
\* the others count against the bound. A wait carries the landed blocks its
\* call read before it (WaitSlot::holds). While the wait is pending, those
\* are held. When it completes it is kept, holding them and the block it
\* waited for, until the call is seen again: run again, the call either
\* misses one more block (its new wait holds everything it read, the old one
\* is let go) or reads every block it needs and tells the queue so
\* (Message::Finished), and the kept wait is let go.
\*
\* What this model checks:
\*   - a call run again finds every block its run before found
\*     (RunsKeepProgress): progress is kept across runs, so a call needing
\*     n blocks finishes in n + 1 runs;
\*   - every call finishes when each run follows the poll that completed its
\*     wait (EveryCallFinishes, under weak fairness);
\*   - right after a poll, what stays landed past the bound is held by a
\*     call in progress (OverBoundIsHeld): the table holds the bound plus
\*     what the calls asked for, never more.
\*
\* WHAT IS MODELLED, and the code each piece mirrors:
\*   landed  the blocks landed for the queue, oldest first
\*           (QueueShared::landed, Ledger::order)
\*   pc      where each call is: "run" (about to run), "wait" (its wait is
\*           pending) or "done"
\*   wait    each call's pending wait: the block it waits for and the
\*           landed blocks its run read before it (WaitSlot::holds)
\*   kept    the completed waits not let go yet (Ledger::kept)
\*   fresh   a ghost: the last step was a poll, so the eviction just ran
\*   found   a ghost: how many blocks each call's last run found
\*   lost    a ghost: some run found fewer blocks than its call's run before
\*
\* WHAT IS LEFT OUT, and why that loses nothing:
\*   - Bytes. Each block costs one, and Bound counts blocks: two blocks
\*     past a bound of one stand for two values each larger than half the
\*     bound, which is the reviewer's case (two 2 MiB blocks, a 1 MiB bound).
\*   - The block cache: it is off, the hard case, where only the landing
\*     carries a read.
\*   - The aging of kept waits (KEEP_ROUNDS): a backstop for a call never
\*     run again, which lets its holds go after that many landing polls.
\*     Here every call is run again before the next poll that lands
\*     anything, so no kept wait ages. A call run again later than that may
\*     find a block gone and miss it once more; it is not modelled here.
\*     LandingLifetime.tla separately checks that a live caller prevents
\*     this aging regardless of how many other landing polls intervene.
\*   - Other threads: a run here is one step, as on the queue's owner,
\*     whose own poll cannot fall inside it. The queue's messages (a wait,
\*     a Finished) reach the owner in order before its next eviction, which
\*     the one step also stands for.
\*   - Several copies of a landing: landing a block again moves it to the
\*     back, as the newer copy is the one that stays.
\*
\* CONFIGURATIONS (each GREEN must pass, each RED must break the one promise
\* it names):
\*   MC_LandingHold_Green          two calls, one reading a then b, the
\*                                 other b then a, a bound of one block.
\*   MC_LandingHold_Green_Three    one call reading three blocks.
\*   MC_LandingHold_Red_Fifo       the eviction before the fix: oldest
\*                                 first, holding nothing, keeping the
\*                                 newest. EveryCallFinishes breaks.
\*   MC_LandingHold_Red_NoHolds    a wait holds nothing the call read before
\*                                 it, only the block it waited for once it
\*                                 lands. RunsKeepProgress breaks: the call
\*                                 still finishes here, where no kept wait
\*                                 ages, but each run may lose what the one
\*                                 before found, and with the aging in the
\*                                 code a long enough call never finishes.

\* Numbers, sequences, and the size of a set.
EXTENDS Naturals, Sequences, FiniteSets

\* The fixed inputs of a configuration.
CONSTANTS
  \* "one", "two" or "three": which calls run, and what each reads.
  Story,
  \* How many landed blocks the queue keeps before it evicts.
  Bound,
  \* "none" for the real code, or the name of one planted bug.
  Mutant

\* The stories this model knows.
ASSUME Story \in {"one", "two", "three"}
\* The bound keeps at least one block.
ASSUME Bound \in Nat \ {0}
\* The bug names this model knows.
ASSUME Mutant \in {"none", "Fifo", "NoHolds"}

\* The calls of the story: one, or two that read the same blocks in opposite
\* orders.
Calls == IF Story = "two" THEN {"c1", "c2"} ELSE {"c1"}

\* The blocks each call reads, in the order it reads them.
Need == CASE Story = "one" -> [c \in Calls |-> <<"a", "b">>]
          \* Two calls, crossing.
          [] Story = "two" -> [c \in Calls |-> IF c = "c1" THEN <<"a", "b">> ELSE <<"b", "a">>]
          \* One call, three blocks.
          [] Story = "three" -> [c \in Calls |-> <<"a", "b", "c">>]

\* Every block some call reads.
Blocks == UNION {{Need[c][i] : i \in 1..Len(Need[c])} : c \in Calls}

\* The blocks of a sequence, as a set.
Range(s) == {s[i] : i \in 1..Len(s)}

\* The smaller of two numbers.
Min(m, n) == IF m < n THEN m ELSE n

\* A call with no pending wait.
NoWait == [on |-> FALSE, awaited |-> "none", holds |-> {}]

\* The state that changes from step to step.
VARIABLES
  \* Seq of Blocks: what landed for the queue, oldest first.
  landed,
  \* [Calls -> {"run", "wait", "done"}]: where each call is.
  pc,
  \* [Calls -> a wait record]: each call's pending wait, or NoWait.
  wait,
  \* A set of [call, awaited, holds]: the completed waits not let go yet.
  \* The call is a ghost that keeps two equal records apart; the code
  \* matches a kept wait by what a later run read, never by its call.
  kept,
  \* Ghost: the last step was a poll.
  fresh,
  \* Ghost, [Calls -> Nat]: how many blocks each call's last run found.
  found,
  \* Ghost: some run found fewer blocks than its call's run before it.
  lost

\* All of them, for "nothing else changes".
vars == <<landed, pc, wait, kept, fresh, found, lost>>

\* The start: nothing landed, every call about to run, nothing waiting.
Init ==
  \* Nothing landed.
  /\ landed = <<>>
  \* Every call is about to run for the first time.
  /\ pc = [c \in Calls |-> "run"]
  \* No wait is pending.
  /\ wait = [c \in Calls |-> NoWait]
  \* No wait is kept.
  /\ kept = {}
  \* No poll has run.
  /\ fresh = FALSE
  \* No run has found anything.
  /\ found = [c \in Calls |-> 0]
  \* No run has lost anything.
  /\ lost = FALSE

\* ---------------------------------------------------------------------------
\* What the queue holds (Ledger::held)
\* ---------------------------------------------------------------------------

\* Whether block b is held: a pending wait holds it, or a kept wait holds it
\* or waited for it. NoHolds plants the bug: only a kept wait's own block is
\* held. Fifo holds nothing.
Held(b) ==
  \* The design.
  \/ /\ Mutant = "none"
     \* A pending wait's call read it, or a kept wait holds it or waited for it.
     /\ \/ \E c \in Calls : wait[c].on /\ b \in wait[c].holds
        \* A kept wait holds or waited for it.
        \/ \E k \in kept : b = k.awaited \/ b \in k.holds
  \* The bug NoHolds: only the block a kept wait waited for.
  \/ /\ Mutant = "NoHolds"
     \* A kept wait waited for it.
     /\ \E k \in kept : b = k.awaited

\* The kept waits a run that read `read` lets go: each one whose block and
\* holds the run read (Ledger::seen).
Covered(read) == {k \in kept : k.awaited \in read /\ k.holds \subseteq read}

\* ---------------------------------------------------------------------------
\* A call runs (multi_get, a collecting scan, a merge read)
\* ---------------------------------------------------------------------------

\* How many of call c's blocks, from the first, are landed now: the run reads
\* those, and stops at the first one that is not.
Found(c) ==
  \* The longest prefix whose blocks are all landed.
  CHOOSE n \in 0..Len(Need[c]) :
    \* Every block up to n is landed...
    /\ \A i \in 1..n : Need[c][i] \in Range(landed)
    \* ...and the next one, if any, is not.
    /\ n = Len(Need[c]) \/ Need[c][n + 1] \notin Range(landed)

\* Call c runs, start to end, in one step. It reads its landed blocks in
\* order; at the first block not landed it records a wait holding what it
\* read, and returns WouldBlock. Reading every block, it finishes and tells
\* the queue what it read. Either way the kept waits its read covers were its
\* own earlier runs, and are let go.
Run(c) ==
  \* The call is about to run.
  /\ pc[c] = "run"
  \* With n the blocks it found, and read the set of them...
  /\ LET n == Found(c)
         \* ...what it read.
         read == {Need[c][i] : i \in 1..n}
     \* The kept waits it covers go.
     IN /\ kept' = kept \ Covered(read)
        \* The ghosts: what this run found, and whether it found less than the run before.
        /\ found' = [found EXCEPT ![c] = n]
        \* Less than the run before is progress lost.
        /\ lost' = (lost \/ n < found[c])
        \* Did it find every block?
        /\ IF n = Len(Need[c])
           \* Yes: it answers, and is done.
           THEN /\ pc' = [pc EXCEPT ![c] = "done"]
                \* No wait.
                /\ UNCHANGED wait
           \* No: it waits for the next block, holding what it read.
           ELSE /\ pc' = [pc EXCEPT ![c] = "wait"]
                \* The wait, with its holds.
                /\ wait' = [wait EXCEPT ![c] = [on |-> TRUE, awaited |-> Need[c][n + 1],
                                                holds |-> read]]
  \* Nothing lands in a run, and it is not a poll.
  /\ landed' = landed
  \* The last step was not a poll.
  /\ fresh' = FALSE

\* ---------------------------------------------------------------------------
\* The owner polls (IoQueue::poll)
\* ---------------------------------------------------------------------------

\* The landed blocks evicted to fit the bound, given that the blocks in h are
\* held: the oldest of the others, as many as the table is over the bound.
\* Fifo, the code before: the oldest of all, keeping the newest.
Gone(s, h) ==
  \* How far over the bound the table is.
  LET over == IF Len(s) > Bound THEN Len(s) - Bound ELSE 0
      \* The landings that may go, oldest first.
      free == SelectSeq(s, LAMBDA b : b \notin h)
  \* Which of them go.
  IN IF Mutant = "Fifo"
     \* The bug: the oldest, held or not, but never the newest.
     THEN {s[i] : i \in 1..Min(over, Len(s) - 1)}
     \* The design: the oldest of those no call holds.
     ELSE {free[i] : i \in 1..Min(over, Len(free))}

\* The poll reads block x, which some pending wait waits for, and lands it:
\* every wait for x completes and is kept, and its call is told it may run
\* again. Then, every message in, the landings no call holds go until the
\* rest fit the bound (Ledger::settle).
Poll(x) ==
  \* Some call waits for x.
  /\ \E c \in Calls : wait[c].on /\ wait[c].awaited = x
  \* The calls whose wait is for x.
  /\ LET done == {c \in Calls : wait[c].on /\ wait[c].awaited = x}
         \* x lands, at the back.
         grown == Append(SelectSeq(landed, LAMBDA b : b # x), x)
         \* Their waits, kept with their holds and x.
         keptNow == kept \cup {[call |-> c, awaited |-> x, holds |-> wait[c].holds] : c \in done}
         \* The waits still pending.
         waitNow == [c \in Calls |-> IF c \in done THEN NoWait ELSE wait[c]]
         \* What is held once they are kept, under each mutant.
         held == IF Mutant = "none"
                 \* Pending holds, kept holds and kept blocks.
                 THEN UNION {waitNow[c].holds : c \in Calls}
                        \cup UNION {k.holds \cup {k.awaited} : k \in keptNow}
                 \* NoHolds: kept blocks only; Fifo holds nothing either way.
                 ELSE {k.awaited : k \in keptNow}
     \* The table after the eviction.
     IN /\ landed' = SelectSeq(grown, LAMBDA b : b \notin Gone(grown, held))
        \* The completed waits are kept.
        /\ kept' = keptNow
        \* They are no longer pending.
        /\ wait' = waitNow
        \* Their calls may run again.
        /\ pc' = [c \in Calls |-> IF c \in done THEN "run" ELSE pc[c]]
  \* The eviction just ran.
  /\ fresh' = TRUE
  \* No run happened.
  /\ UNCHANGED <<found, lost>>

\* Every step anything can take.
Next ==
  \* A call runs...
  \/ \E c \in Calls : Run(c)
  \* ...or the owner polls a block some call waits for.
  \/ \E x \in Blocks : Poll(x)

\* Start at Init and take steps; every call that may run runs, and every
\* block waited for is polled, sooner or later.
Spec ==
  \* The runs.
  /\ Init /\ [][Next]_vars
  \* A call told it may run again runs again.
  /\ \A c \in Calls : WF_vars(Run(c))
  \* A pending wait is polled.
  /\ \A x \in Blocks : WF_vars(Poll(x))

\* ---------------------------------------------------------------------------
\* What must hold
\* ---------------------------------------------------------------------------

\* Every variable holds what its comment says.
TypeOK ==
  \* The table holds blocks.
  /\ Range(landed) \subseteq Blocks
  \* Each call is somewhere.
  /\ pc \in [Calls -> {"run", "wait", "done"}]
  \* A pending wait is for a block, and holds blocks.
  /\ \A c \in Calls : wait[c].on => wait[c].awaited \in Blocks /\ wait[c].holds \subseteq Blocks
  \* A call waits exactly when it has a pending wait.
  /\ \A c \in Calls : (pc[c] = "wait") = wait[c].on

\* Right after a poll, the table is within the bound, or every block in it is
\* held: what stays past the bound is what calls in progress need. It rules
\* out a table that grows with landings nobody needs.
OverBoundIsHeld ==
  \* Right after a poll...
  fresh =>
    \* ...the table fits...
    \/ Len(landed) <= Bound
    \* ...or each block in it is held.
    \/ \A i \in 1..Len(landed) : Held(landed[i])

\* No run finds fewer blocks than its call's run before it: what a call found
\* stays landed until it runs again. It rules out a call that, run after
\* every poll, finds a and misses b, then misses a again because b's landing
\* evicted it.
RunsKeepProgress == ~lost

\* Every call finishes. It rules out the story above: a multi_get of a and b
\* run after every poll, each run losing the block the one before found.
EveryCallFinishes == \A c \in Calls : <>(pc[c] = "done")

====
