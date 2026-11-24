---- MODULE ValidationProgress ----
\* A cold early-check item waits for its block. Its completed prefix and
\* marker stay owned by the same commit. Only the current item is retried.
\* The original horizon is retained: the slot still checks every later write.
\* Correspondence: cursor/dirty are EarlyProgress.at/marker; ReadMiss is
\* check_early_with_progress returning WouldBlock; PollRead is IoQueue's
\* completion; ReadHit advances that same check; Commit is decide_txn's
\* look above Early.horizon. Lean ValidationProgress proves ordered suffix
\* reuse for any marker transition and any input length.
EXTENDS Naturals, FiniteSets
\* Number of keys, defect to plant, and keys changed before the horizon.
CONSTANTS Keys, Mutant, OldChanged
\* The commit's cursor, I/O wait and one-block landing, saved marker,
\* writes above its original horizon, work counters and outcome.
VARIABLES cursor, waiting, landed, dirty, changed, visits, misses, pc, result
\* The complete state vector.
vars == <<cursor, waiting, landed, dirty, changed, visits, misses, pc, result>>
\* An out-of-range key means no I/O is pending.
None == Keys
\* The transaction validates every key in this bounded example.
All == 0..(Keys - 1)
\* No item has been checked, and the first block is cold.
Init ==
  \* The first item has no pending read and no landed block.
  /\ cursor = 0 /\ waiting = None /\ landed = {}
  \* No dirty marker or later write has been seen.
  /\ dirty = {} /\ changed = {}
  \* No validation work has run yet.
  /\ visits = 0 /\ misses = 0 /\ pc = "read" /\ result = FALSE
\* A concurrent write above the original early horizon changes one key.
Write(k) ==
  \* Each key needs at most one later write to exercise the split check.
  /\ pc # "done" /\ k \notin changed /\ changed' = changed \cup {k}
  \* The commit's own progress is untouched.
  /\ UNCHANGED <<cursor, waiting, landed, dirty, visits, misses, pc, result>>
\* A landed current block completes exactly one item, preserving order.
ReadHit ==
  \* The current item is available on this queue.
  /\ pc = "read" /\ cursor < Keys /\ cursor \in landed
  \* The marker records changes this item sees.
  /\ dirty' = IF cursor \in OldChanged \cup changed THEN dirty \cup {cursor} ELSE dirty
  \* Only a completed item advances the cursor.
  /\ cursor' = cursor + 1 /\ visits' = visits + 1
  \* No I/O or outcome changes.
  /\ UNCHANGED <<waiting, landed, changed, misses, pc, result>>
\* A miss keeps the marker and current item until its block lands.
ReadMiss ==
  \* The current item's block is unavailable.
  /\ pc = "read" /\ cursor < Keys /\ cursor \notin landed
  \* The queue owes exactly that block.
  /\ waiting' = cursor /\ pc' = "wait"
  \* The attempted visit is counted as a miss.
  /\ visits' = visits + 1 /\ misses' = misses + 1
  \* Restart loses completed-prefix progress; SkipMiss falsely completes an item.
  /\ cursor' = IF Mutant = "Restart" THEN 0 ELSE IF Mutant = "SkipMiss" THEN cursor + 1 ELSE cursor
  \* A miss changes no validation fact or outcome.
  /\ UNCHANGED <<landed, dirty, changed, result>>
\* The queue reads its pending block and resubmits the same commit.
PollRead ==
  \* A block is owed.
  /\ pc = "wait"
  \* A one-block landing need not retain any completed-prefix blocks.
  /\ landed' = {waiting} /\ waiting' = None /\ pc' = "read"
  \* Reading the block does not complete its validation item.
  /\ UNCHANGED <<cursor, dirty, changed, visits, misses, result>>
\* The slot is taken only after every early-check item completed.
Enter ==
  \* All items are checked.
  /\ pc = "read" /\ cursor = Keys /\ pc' = "commit"
  \* The marker and original-horizon obligation are retained.
  /\ UNCHANGED <<cursor, waiting, landed, dirty, changed, visits, misses, result>>
\* The slot checks changes above the retained horizon.
Commit ==
  \* This slot is ready to be decided.
  /\ pc = "commit" /\ pc' = "done"
  \* Dropping the later check misses a write to an already completed item.
  /\ result' = (dirty # {} \/ (Mutant # "SkipLateCheck" /\ changed # {}))
  \* The check does not rewrite progress or the store.
  /\ UNCHANGED <<cursor, waiting, landed, dirty, changed, visits, misses>>
\* Every possible interleaving of I/O, validation and later writes.
Next == (\E k \in All : Write(k)) \/ ReadHit \/ ReadMiss \/ PollRead \/ Enter \/ Commit
\* Completed items plus misses account for every visit, with no replayed prefix.
Cost == visits = cursor + misses
\* The split check agrees with checking all keys against every visible change.
Exact == pc = "done" => result = (OldChanged \cup changed # {})
\* Polling the queue and running enabled validation steps eventually finishes.
EveryCheckEnds == <>(pc = "done")
\* Fair queue polling is the caller contract; no thread or timeout is added.
Spec == Init /\ [][Next]_vars
  \* Each enabled cached read, miss and device completion eventually runs.
  /\ WF_vars(ReadHit) /\ WF_vars(ReadMiss) /\ WF_vars(PollRead)
  \* A complete prefix eventually takes its slot and is decided.
  /\ WF_vars(Enter) /\ WF_vars(Commit)
====
