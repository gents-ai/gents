---- MODULE ScheduleOccurrence ----
\* lean: none
\* crates: gents-automation, gents-session, gents-protocol
\* models: Triggers.ScheduleOccurrence.materialize, Triggers.ScheduleOccurrence.retryKey, TransactionFence.conflicts
\* Dispatches of a plain schedule Trigger's occurrences (no session template,
\* no Goal) against its cursor writes and a restart.
\*
\* The schedule source (`ScheduleSource::next_fire`) reads the cursor
\* (`Trigger.next_run_at`) and, when it is due, dispatches that occurrence.
\* The materializer (`write_pending_agent_request_with_identity`) runs one
\* transaction per dispatch: with `Keyed` it reads the request by the
\* occurrence's `retry_key` (a unique key the store validates like a read by
\* id) and returns it if present, otherwise it inserts the request under the
\* unique index, so a commit that finds the key taken aborts and its rebuilt
\* read finds the winner. FALSE is a random request identity with no retry key.
\* The cursor writer runs after each dispatch in its own task: its write lands
\* in any order, lags past any number of ticks, or fails; a restart loses every
\* write and every dispatch still in flight.
EXTENDS Naturals, FiniteSets

CONSTANTS
  Occurrences,    \* occurrences the cadence offers: 1..Occurrences
  MaxDispatches,  \* dispatches the source makes in a run
  MaxRestarts,    \* process restarts
  Keyed           \* the request's identity and retry_key come from the occurrence

ASSUME /\ Occurrences \in Nat \ {0}
       /\ MaxDispatches \in Nat \ {0}
       /\ MaxRestarts \in Nat
       /\ Keyed \in BOOLEAN

D == 1..MaxDispatches
O == 1..Occurrences

VARIABLES
  cursor,    \* the persisted next_run_at: the next occurrence a tick offers
  writes,    \* cursor writes spawned and not yet landed or failed
  status,    \* per dispatch: "idle", "read", "commit", "done", "lost"
  occ,       \* per dispatch: the occurrence it carries
  requests,  \* committed requests: [occ, id]
  issued,    \* dispatches made
  restarts

vars == <<cursor, writes, status, occ, requests, issued, restarts>>

Id(d) == IF Keyed THEN occ[d] ELSE Occurrences + d

TypeOK ==
  /\ cursor \in 1..(Occurrences + 1)
  /\ writes \subseteq 1..(Occurrences + 1)
  /\ status \in [D -> {"idle", "read", "commit", "done", "lost"}]
  /\ occ \in [D -> 0..Occurrences]
  /\ requests \subseteq [occ : O, id : Nat]
  /\ issued \in 0..MaxDispatches
  /\ restarts \in 0..MaxRestarts

Init ==
  /\ cursor = 1
  /\ writes = {}
  /\ status = [d \in D |-> "idle"]
  /\ occ = [d \in D |-> 0]
  /\ requests = {}
  /\ issued = 0
  /\ restarts = 0

Taken(d) == \E r \in requests : r.id = Id(d)

\* A tick that finds the cursor's occurrence due dispatches it.
Tick ==
  /\ issued < MaxDispatches
  /\ cursor <= Occurrences
  /\ LET d == issued + 1 IN
       /\ status' = [status EXCEPT ![d] = "read"]
       /\ occ' = [occ EXCEPT ![d] = cursor]
       /\ issued' = d
  /\ UNCHANGED <<cursor, writes, requests, restarts>>

\* The retry-key read: a found request answers the dispatch (a duplicate,
\* which also advances the cursor).
Read(d) ==
  /\ status[d] = "read"
  /\ IF Keyed /\ Taken(d)
       THEN /\ status' = [status EXCEPT ![d] = "done"]
            /\ writes' = writes \cup {occ[d] + 1}
       ELSE /\ status' = [status EXCEPT ![d] = "commit"]
            /\ UNCHANGED writes
  /\ UNCHANGED <<cursor, occ, requests, issued, restarts>>

\* The insert commits unless the unique index finds its retry key taken.
Commit(d) ==
  /\ status[d] = "commit"
  /\ IF Keyed /\ Taken(d)
       THEN /\ status' = [status EXCEPT ![d] = "read"]
            /\ UNCHANGED <<requests, writes>>
       ELSE /\ requests' = requests \cup {[occ |-> occ[d], id |-> Id(d)]}
            /\ status' = [status EXCEPT ![d] = "done"]
            /\ writes' = writes \cup {occ[d] + 1}
  /\ UNCHANGED <<cursor, occ, issued, restarts>>

CursorLands(w) ==
  /\ w \in writes
  /\ cursor' = w
  /\ writes' = writes \ {w}
  /\ UNCHANGED <<status, occ, requests, issued, restarts>>

CursorFails(w) ==
  /\ w \in writes
  /\ writes' = writes \ {w}
  /\ UNCHANGED <<cursor, status, occ, requests, issued, restarts>>

Restart ==
  /\ restarts < MaxRestarts
  /\ restarts' = restarts + 1
  /\ writes' = {}
  /\ status' = [d \in D |-> IF status[d] \in {"read", "commit"} THEN "lost" ELSE status[d]]
  /\ UNCHANGED <<cursor, occ, requests, issued>>

Next ==
  \/ Tick
  \/ \E d \in D : Read(d) \/ Commit(d)
  \/ \E w \in writes : CursorLands(w) \/ CursorFails(w)
  \/ Restart

Spec == Init /\ [][Next]_vars

AtMostOneRequestPerOccurrence ==
  \A o \in O : Cardinality({r \in requests : r.occ = o}) <= 1
====
