---- MODULE TombstoneLog ----
\* ===========================================================================
\* THE STORY
\* ===========================================================================
\* A range delete ("delete every key from a to m") is stored in the
\* memtable as a range tombstone, beside the point writes. Every read in
\* that memtable checks the tombstones: a key a tombstone covers is deleted.
\* So the tombstones are read all the time, by many threads at once, while
\* commits append new ones (src/engine/memtable/tombstones.rs).
\*
\* Who appends. The commit pipeline lets whichever thread reaches a decided
\* commit apply it (src/engine/commit), so there is no one writer: two
\* threads can apply the same commit at once, and a thread that fell behind
\* can still be appending for one commit while another thread appends for
\* the next.
\*
\* They live in an append-only log that readers walk without a lock:
\*   - an append first looks whether the log already holds its tombstone
\*     (another thread applied the same commit), and adds nothing if so;
\*   - otherwise it takes the next position with one fetch_add, so no two
\*     appends ever share a slot;
\*   - it writes the tombstone into that slot, and only then sets the
\*     slot's "ready" flag;
\*   - a reader reads how many positions were handed out, then looks at
\*     each slot below that: one whose flag is set it reads, one whose flag
\*     is not set yet it skips.
\* A commit is marked applied only after its append returned, and it is made
\* visible (the read horizon is raised over it) only after it is applied. A
\* reader takes its snapshot from that horizon before it reads anything.
\*
\* Tiny example. Thread A applies commit 1: it takes position 1, writes
\* tombstone 1 there, sets the flag, marks the commit applied. The horizon
\* is raised to 1. A reader samples horizon 1, reads "1 position handed
\* out", finds slot 1's flag set and reads tombstone 1, whole.
\*
\* What can go wrong, and what this model checks cannot happen:
\*   - two appends write into one slot, and one tombstone is lost
\*     (OneSlotEach);
\*   - a reader reads a slot before its tombstone is in it (ReadWhole);
\*   - a reader whose snapshot includes a commit misses the tombstone that
\*     commit wrote (PublishedSeen).
\* Two threads that both append the same commit's tombstone are harmless: it
\* is listed twice and deletes what it deletes once. The GREEN run includes
\* that case, and MC_TombstoneLog_Green_Twice is the witness that it does.
\*
\* WHAT IS MODELLED, and the code each piece mirrors:
\*   claimed   how many positions were handed out (TombstoneLog::claimed)
\*   slot      what each slot holds: the number of the commit whose
\*             tombstone it is, or 0 before it is written (Slot::value)
\*   ready     each slot's flag (Slot::ready)
\*   applied   which commits are applied (the descriptor's APPLIED flag)
\*   horizon   the read horizon: commits visible to new snapshots
\*   astate, acommit, apos
\*             where each applying thread is inside one append, for which
\*             commit, at which position (TombstoneLog::push)
\*   rstate    a reader's progress; rsnap, rcount, rnext, rseen and rtorn
\*             what it sampled, read and saw
\*
\* WHAT IS LEFT OUT, and why that loses nothing:
\*   - The sorted index an append publishes every few appends. It is built
\*     only from a prefix of ready slots and read beside the scan of the
\*     rest, so it is a faster way to read the same slots; the property
\*     tests check that it answers exactly what the scan does.
\*   - What a tombstone covers. Only whether a reader sees it whole matters.
\*   - Memory ordering: the loom models in src/engine/loom_model/tombstones.rs
\*     run the real log and catch a slot read that is not ordered after its
\*     write. Here each store and load is one atomic step.
\*
\* CONFIGURATIONS (each GREEN must pass, each RED must break the one
\* invariant it names):
\*   MC_TombstoneLog_Green              take a position with one add, write
\*                                      the slot, set its flag, mark the
\*                                      commit applied, then raise the horizon.
\*   MC_TombstoneLog_Green_Twice        the witness: the GREEN run with the
\*                                      claim "no tombstone is ever listed
\*                                      twice" added. It must break.
\*   MC_TombstoneLog_Red_ReadyFirst     the flag is set before the slot is
\*                                      written: ReadWhole breaks.
\*   MC_TombstoneLog_Red_HorizonFirst   the horizon is raised before the
\*                                      append returned: PublishedSeen
\*                                      breaks.
\*   MC_TombstoneLog_Red_LoadStore      a position is taken with a load and
\*                                      a store instead of one add:
\*                                      OneSlotEach breaks.
\*   MC_TombstoneLog_Red_LoadStore_Lost what that costs, with OneSlotEach
\*                                      left out: a tombstone is written over
\*                                      and PublishedSeen breaks.
\*
\* Lean, proofs/lean/Regolith/TombstoneLog.lean, proves the same rules for
\* any number of appends, appliers and readers.

\* Numbers and the size of a set.
EXTENDS Naturals, FiniteSets

\* The fixed inputs of a configuration.
CONSTANTS
  \* How many commits there are; each brings one range tombstone.
  Commits,
  \* The threads that apply commits.
  Appliers,
  \* The reader threads.
  Readers,
  \* "Green" for the real code, or the name of one planted bug.
  Mode

\* The bug names this model knows.
ASSUME Mode \in {"Green", "ReadyFirst", "HorizonFirst", "LoadStore"}
\* At least one commit.
ASSUME Commits \in Nat /\ Commits >= 1

\* The most positions a run can hand out: one for each applier and commit.
MaxPos == Cardinality(Appliers) * Commits

\* The state that changes from step to step.
VARIABLES
  \* How many positions were handed out.
  claimed,
  \* [1..MaxPos -> 0..Commits]: the commit whose tombstone the slot holds,
  \* 0 before it is written.
  slot,
  \* [1..MaxPos -> BOOLEAN]: the slot's flag.
  ready,
  \* [1..Commits -> BOOLEAN]: the commit is applied.
  applied,
  \* The read horizon: how many commits are visible.
  horizon,
  \* [Appliers -> {"idle", "going", "loaded", "took", "half", "appended"}].
  astate,
  \* [Appliers -> 0..Commits]: the commit a thread is applying.
  acommit,
  \* [Appliers -> 0..MaxPos]: the position it took.
  apos,
  \* [Readers -> {"idle", "sampled", "reading", "done"}].
  rstate,
  \* [Readers -> Nat]: the horizon a reader sampled, its snapshot.
  rsnap,
  \* [Readers -> Nat]: how many positions a reader read as handed out.
  rcount,
  \* [Readers -> Nat]: the next position it will look at.
  rnext,
  \* [Readers -> SUBSET (1..Commits)]: the tombstones it read.
  rseen,
  \* [Readers -> BOOLEAN]: it read a slot that held no tombstone yet.
  rtorn

\* Every variable, so a step that changes none of them is a stutter.
vars == <<claimed, slot, ready, applied, horizon, astate, acommit, apos, rstate, rsnap, rcount,
          rnext, rseen, rtorn>>

\* The log's own state, for the steps that leave it alone.
logVars == <<claimed, slot, ready>>
\* An applying thread's state.
applierVars == <<astate, acommit, apos>>
\* A reader's state.
readerVars == <<rstate, rsnap, rcount, rnext, rseen, rtorn>>

\* The log already holds commit c's tombstone, as a reader of it would
\* see: in a slot handed out whose flag is set (TombstoneLog::holds).
Holds(c) == \E p \in 1..claimed : ready[p] /\ slot[p] = c

\* The start: an empty log, nothing applied, everybody idle.
Init ==
  \* No position is handed out ...
  /\ claimed = 0
  \* ... no slot is written ...
  /\ slot = [p \in 1..MaxPos |-> 0]
  \* ... no flag is set ...
  /\ ready = [p \in 1..MaxPos |-> FALSE]
  \* ... no commit is applied ...
  /\ applied = [c \in 1..Commits |-> FALSE]
  \* ... and none is visible.
  /\ horizon = 0
  \* Every applying thread is idle ...
  /\ astate = [a \in Appliers |-> "idle"]
  \* ... with no commit ...
  /\ acommit = [a \in Appliers |-> 0]
  \* ... and no position.
  /\ apos = [a \in Appliers |-> 0]
  \* Every reader is idle ...
  /\ rstate = [r \in Readers |-> "idle"]
  \* ... with no snapshot ...
  /\ rsnap = [r \in Readers |-> 0]
  \* ... no count read ...
  /\ rcount = [r \in Readers |-> 0]
  \* ... nowhere to look yet ...
  /\ rnext = [r \in Readers |-> 1]
  \* ... nothing seen ...
  /\ rseen = [r \in Readers |-> {}]
  \* ... and nothing torn.
  /\ rtorn = [r \in Readers |-> FALSE]

\* Thread a starts applying commit c: the next commit not applied yet. It
\* looks whether the log already holds c's tombstone. If so it has nothing
\* to append; if not it goes on to take a position.
Look(a, c) ==
  \* The thread is idle.
  /\ astate[a] = "idle"
  \* Commits are applied in order: every commit before c is applied ...
  /\ \A d \in 1..(c - 1) : applied[d]
  \* ... and c is not.
  /\ ~applied[c]
  \* It is on commit c now.
  /\ acommit' = [acommit EXCEPT ![a] = c]
  \* With the tombstone there already, the append is over; else it goes on.
  /\ astate' = [astate EXCEPT ![a] = IF Holds(c) THEN "appended" ELSE "going"]
  \* Nothing else changes.
  /\ UNCHANGED <<logVars, applied, horizon, apos, readerVars>>

\* Thread a takes the next position: one fetch_add, so the position is its
\* alone. (Mode LoadStore: it only loads the count here, and stores it one
\* step later.)
Take(a) ==
  \* The thread is about to append.
  /\ astate[a] = "going"
  \* Its position is the one after the last handed out.
  /\ apos' = [apos EXCEPT ![a] = claimed + 1]
  \* The real code raises the count in the same step; the bug does not.
  /\ claimed' = IF Mode = "LoadStore" THEN claimed ELSE claimed + 1
  \* It holds a position, or has only loaded the count.
  /\ astate' = [astate EXCEPT ![a] = IF Mode = "LoadStore" THEN "loaded" ELSE "took"]
  \* Nothing else changes.
  /\ UNCHANGED <<slot, ready, applied, horizon, acommit, readerVars>>

\* Mode LoadStore only: thread a stores the count it loaded, plus one.
StoreCount(a) ==
  \* The thread loaded the count.
  /\ astate[a] = "loaded"
  \* It stores its own position as the count, whatever the count is now.
  /\ claimed' = apos[a]
  \* It holds the position.
  /\ astate' = [astate EXCEPT ![a] = "took"]
  \* Nothing else changes.
  /\ UNCHANGED <<slot, ready, applied, horizon, acommit, apos, readerVars>>

\* Thread a does the first half of filling its slot. The real code writes
\* the tombstone; mode ReadyFirst sets the flag instead.
FirstHalf(a) ==
  \* The thread holds a position.
  /\ astate[a] = "took"
  \* The real code: the tombstone goes into the slot.
  /\ slot' = IF Mode = "ReadyFirst" THEN slot ELSE [slot EXCEPT ![apos[a]] = acommit[a]]
  \* The bug: the flag is set while the slot is still empty.
  /\ ready' = IF Mode = "ReadyFirst" THEN [ready EXCEPT ![apos[a]] = TRUE] ELSE ready
  \* Half done.
  /\ astate' = [astate EXCEPT ![a] = "half"]
  \* Nothing else changes.
  /\ UNCHANGED <<claimed, applied, horizon, acommit, apos, readerVars>>

\* Thread a does the second half: the real code sets the flag of the slot
\* it wrote; mode ReadyFirst writes the tombstone now.
SecondHalf(a) ==
  \* The thread is half done.
  /\ astate[a] = "half"
  \* The real code: the flag says the slot is whole.
  /\ ready' = IF Mode = "ReadyFirst" THEN ready ELSE [ready EXCEPT ![apos[a]] = TRUE]
  \* The bug: the tombstone arrives after the flag.
  /\ slot' = IF Mode = "ReadyFirst" THEN [slot EXCEPT ![apos[a]] = acommit[a]] ELSE slot
  \* The append has returned.
  /\ astate' = [astate EXCEPT ![a] = "appended"]
  \* Nothing else changes.
  /\ UNCHANGED <<claimed, applied, horizon, acommit, apos, readerVars>>

\* Thread a marks its commit applied: its append returned first.
MarkApplied(a) ==
  \* The append returned, or found the tombstone there.
  /\ astate[a] = "appended"
  \* The commit is applied.
  /\ applied' = [applied EXCEPT ![acommit[a]] = TRUE]
  \* The thread is idle again.
  /\ astate' = [astate EXCEPT ![a] = "idle"]
  \* Nothing else changes.
  /\ UNCHANGED <<logVars, horizon, acommit, apos, readerVars>>

\* The next commit is made visible: the horizon is raised over it. The real
\* pipeline does this only for an applied commit; mode HorizonFirst does it
\* as soon as some thread is applying it.
Publish ==
  \* The commit after the horizon.
  LET c == horizon + 1
  \* There is one ...
  IN /\ c <= Commits
     \* ... and it is applied (the bug: or merely being applied).
     /\ \/ applied[c]
        \* The bug's shortcut.
        \/ Mode = "HorizonFirst" /\ \E a \in Appliers : acommit[a] = c /\ astate[a] # "idle"
     \* The horizon covers it.
     /\ horizon' = c
     \* Nothing else changes.
     /\ UNCHANGED <<logVars, applied, applierVars, readerVars>>

\* A reader takes its snapshot: it samples the horizon (an acquire load of
\* the read horizon) before it reads anything.
Sample(r) ==
  \* An idle reader ...
  /\ rstate[r] = "idle"
  \* ... takes the horizon as its snapshot ...
  /\ rsnap' = [rsnap EXCEPT ![r] = horizon]
  \* ... and is ready to read.
  /\ rstate' = [rstate EXCEPT ![r] = "sampled"]
  \* Nothing else changes.
  /\ UNCHANGED <<logVars, applied, horizon, applierVars, rcount, rnext, rseen, rtorn>>

\* The reader reads how many positions were handed out (TombstoneLog::len,
\* an acquire load).
ReadCount(r) ==
  \* A reader with its snapshot ...
  /\ rstate[r] = "sampled"
  \* ... reads the count ...
  /\ rcount' = [rcount EXCEPT ![r] = claimed]
  \* ... and starts on the slots below it.
  /\ rstate' = [rstate EXCEPT ![r] = "reading"]
  \* Nothing else changes.
  /\ UNCHANGED <<logVars, applied, horizon, applierVars, rsnap, rnext, rseen, rtorn>>

\* The reader looks at its next slot: it reads the flag, and the slot only
\* if the flag is set (TombstoneLog::get).
ReadSlot(r) ==
  \* The position it looks at.
  LET p == rnext[r]
  \* A reading reader ...
  IN /\ rstate[r] = "reading"
     \* ... with slots left below its count ...
     /\ p <= rcount[r]
     \* ... reads the tombstone of a slot whose flag is set ...
     /\ rseen' = [rseen EXCEPT ![r] = IF ready[p] /\ slot[p] # 0 THEN @ \cup {slot[p]} ELSE @]
     \* ... notes it if such a slot holds nothing yet ...
     /\ rtorn' = [rtorn EXCEPT ![r] = @ \/ (ready[p] /\ slot[p] = 0)]
     \* ... and moves on; a slot whose flag is not set is skipped.
     /\ rnext' = [rnext EXCEPT ![r] = p + 1]
     \* Nothing else changes.
     /\ UNCHANGED <<logVars, applied, horizon, applierVars, rstate, rsnap, rcount>>

\* The reader has looked at every slot below its count.
Finish(r) ==
  \* A reading reader ...
  /\ rstate[r] = "reading"
  \* ... past the last slot below its count ...
  /\ rnext[r] > rcount[r]
  \* ... is done.
  /\ rstate' = [rstate EXCEPT ![r] = "done"]
  \* Nothing else changes.
  /\ UNCHANGED <<logVars, applied, horizon, applierVars, rsnap, rcount, rnext, rseen, rtorn>>

\* Every step the system can take.
Next ==
  \* An applying thread starts on a commit ...
  \/ \E a \in Appliers : \E c \in 1..Commits : Look(a, c)
  \* ... or takes one of the steps of its append ...
  \/ \E a \in Appliers : Take(a) \/ StoreCount(a) \/ FirstHalf(a) \/ SecondHalf(a) \/ MarkApplied(a)
  \* ... or a commit is made visible ...
  \/ Publish
  \* ... or some reader takes a step.
  \/ \E r \in Readers : Sample(r) \/ ReadCount(r) \/ ReadSlot(r) \/ Finish(r)

\* Every behaviour: start in Init, then take Next steps or stutter.
Spec == Init /\ [][Next]_vars

----------------------------------------------------------------------------
\* Invariants.

\* Every variable holds what its comment says.
TypeOK ==
  \* The count stays within the positions a run can hand out ...
  /\ claimed \in 0..MaxPos
  \* ... each slot holds a commit's number or 0 ...
  /\ slot \in [1..MaxPos -> 0..Commits]
  \* ... each flag is set or not ...
  /\ ready \in [1..MaxPos -> BOOLEAN]
  \* ... the horizon stays within the commits ...
  /\ horizon \in 0..Commits
  \* ... every applying thread is in one of its states ...
  /\ astate \in [Appliers -> {"idle", "going", "loaded", "took", "half", "appended"}]
  \* ... and so is every reader.
  /\ rstate \in [Readers -> {"idle", "sampled", "reading", "done"}]

\* T1: no two appends share a slot. Two threads in the middle of an append
\* hold different positions, and a slot whose flag is set is nobody's to
\* write any more. Rules out: two appends take position 1, the second
\* tombstone lands on top of the first, and the first range delete is gone.
\* Lean: one_slot_each.
OneSlotEach ==
  \* Take two different threads that each hold a position.
  \A a, b \in Appliers :
    \* Two threads, each between taking its position and finishing its write.
    a # b /\ astate[a] \in {"took", "half"} /\ astate[b] \in {"took", "half"}
      \* Their positions differ.
      => apos[a] # apos[b]

\* T2, THE HEADLINE. A reader never reads a slot before its tombstone is in
\* it. Rules out: the flag is set first, a reader reads the slot in between
\* and finds no tombstone, and a deleted key reads back as live. Lean:
\* ready_whole.
ReadWhole ==
  \* No reader saw a flagged slot that held nothing.
  \A r \in Readers : ~rtorn[r]

\* A reader whose snapshot includes a commit sees the tombstone that commit
\* wrote. Rules out: the horizon covers commit 1 while its append has not
\* returned; a reader samples it, finds the slot's flag not set, skips it,
\* and misses the delete its own snapshot includes. Lean: published_seen.
PublishedSeen ==
  \* For every reader that is done: it read every tombstone up to its snapshot.
  \A r \in Readers : rstate[r] = "done" => \A c \in 1..rsnap[r] : c \in rseen[r]

----------------------------------------------------------------------------
\* The witness.

\* "No tombstone is ever in the log twice." FALSE in the design: two threads
\* that apply the same commit can both append its tombstone, which is
\* harmless. A run that checks this as an invariant must break it, which
\* shows the GREEN run includes that case.
NeverTwice ==
  \* No two different slots hold the same commit's tombstone.
  \A p, q \in 1..MaxPos : p # q /\ slot[p] # 0 => slot[p] # slot[q]

====
