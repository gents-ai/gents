---- MODULE ProjectionDrain ----
\* lean: none
\* crates: none
\* paths: apps/gents-desktop/src/hooks/projectionController.ts
(***************************************************************************)
(* The kinds of work the desktop's projection drain loop owes, and the      *)
(* session list read a tracked turn's end owes                             *)
(* (apps/gents-desktop/src/hooks/projectionController.ts).                 *)
(* `DesktopUpdateListener` models the same loop as one owed read; this     *)
(* models what that read is made of.                                        *)
(*                                                                          *)
(* The open session's tracked turn runs, then ends. Its end changes the     *)
(* session list (the turn's row) and is delivered as a store update while   *)
(* the turn is still tracked, so the update asks for a delta (`delta`,      *)
(* `sessionDelta`). Other requests ask for a full read of the session      *)
(* (`session`: a selection or tracked request change) at any time.          *)
(*                                                                          *)
(* Requests join one pending set. A pass takes the whole set and then       *)
(* reads: a full session read sees the turn as it is when it reads; a       *)
(* delta alone is due at once, so it asks for a session event (a session    *)
(* read with `idx`, the list read owed once the turn has ended); a session  *)
(* read with `idx` that finds the turn ended reads the list. A full session *)
(* read covers a delta's own work, so a delta joining one is dropped from   *)
(* the set; `KeepObligation` keeps its `idx` (the fix). The read that       *)
(* readSessionUpdate does is not modelled: due at once is the case that     *)
(* asks for a session event.                                                *)
(*                                                                          *)
(* GREEN `MCProjectionDrain`: the merge keeps `idx`. RED                    *)
(* `MCProjectionDrainMergeDropsIndex`: the merge drops it, and a turn's end *)
(* delivered beside a session request never reaches the list.              *)
(***************************************************************************)
EXTENDS Naturals

CONSTANTS
  SessionRequests,  \* session requests the environment may make
  KeepObligation    \* a delta merged into a session read keeps its list read

Work == {"session", "delta", "idx"}

VARIABLES
  turn,      \* "running" | "ended"
  list,      \* "current" | "stale": whether the session list shows the turn as it is
  pending,   \* the work the next pass takes
  taken,     \* the work the running pass took, or {} with none running
  requests   \* session requests left

vars == <<turn, list, pending, taken, requests>>

TypeOK ==
  /\ turn \in {"running", "ended"}
  /\ list \in {"current", "stale"}
  /\ pending \subseteq Work
  /\ taken \subseteq Work
  /\ requests \in 0..SessionRequests

\* `enqueue`: a full session read covers a delta, which leaves the set; with
\* the fix its list read stays owed.
Enqueue(set, work) ==
  LET joined == set \cup work
  IN IF "session" \in joined /\ "delta" \in joined
       THEN (joined \ {"delta"}) \cup (IF KeepObligation THEN {"idx"} ELSE {})
       ELSE joined

Init ==
  /\ turn = "running"
  /\ list = "current"
  /\ pending = {}
  /\ taken = {}
  /\ requests = SessionRequests

(* Environment *)

\* The turn ends; its store update arrives while it is still tracked.
TurnEnds ==
  /\ turn = "running"
  /\ turn' = "ended"
  /\ list' = "stale"
  /\ pending' = Enqueue(pending, {"delta"})
  /\ UNCHANGED <<taken, requests>>

RequestSession ==
  /\ requests > 0
  /\ requests' = requests - 1
  /\ pending' = Enqueue(pending, {"session"})
  /\ UNCHANGED <<turn, list, taken>>

(* The drain loop *)

Take ==
  /\ taken = {} /\ pending # {}
  /\ taken' = pending
  /\ pending' = {}
  /\ UNCHANGED <<turn, list, requests>>

Finish ==
  /\ taken # {}
  /\ taken' = {}
  /\ IF "session" \in taken
       THEN /\ list' = IF "idx" \in taken /\ turn = "ended" THEN "current" ELSE list
            /\ UNCHANGED pending
       ELSE \* a delta alone is due at once: a session event follows
            /\ pending' = Enqueue(pending, {"session", "idx"})
            /\ UNCHANGED list
  /\ UNCHANGED <<turn, requests>>

Next == TurnEnds \/ RequestSession \/ Take \/ Finish

Spec == Init /\ [][Next]_vars /\ WF_vars(Take) /\ WF_vars(Finish)

\* Once the turn has ended and requests stop, the session list shows it.
ListCatchesUp == <>[](turn = "ended" => list = "current")

====
