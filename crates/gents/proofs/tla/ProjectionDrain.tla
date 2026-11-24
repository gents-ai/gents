---- MODULE ProjectionDrain ----
\* lean: none
\* crates: none
\* paths: apps/gents-desktop/src/hooks/projectionController.ts
(***************************************************************************)
(* The kinds of work the desktop's projection drain loop owes, and the      *)
(* session list reads a followed turn owes                                  *)
(* (apps/gents-desktop/src/hooks/projectionController.ts).                 *)
(* `DesktopUpdateListener` models the same loop as one owed read; this     *)
(* models what that read is made of.                                        *)
(*                                                                          *)
(* The open session's tracked turn runs, and may end. Its end changes the   *)
(* session list (the turn's row) and is delivered as a store update while   *)
(* the turn is still tracked. Other sessions' rows change at any time,      *)
(* also as store updates, and so do the turn's reply pieces, which change   *)
(* no row. A store update while the turn is tracked asks for a delta        *)
(* (`delta`, `sessionDelta`); with `ListOnTrackedUpdate` (the fix), one     *)
(* that changed a row the list shows (its session list fingerprint is new)  *)
(* asks for a list read too (`list`, `storeUpdate`). Other requests ask    *)
(* for a full read of the session (`session`: a selection or tracked        *)
(* request change) at any time.                                             *)
(*                                                                          *)
(* Requests join one pending set. A pass takes the whole set and then       *)
(* reads: `list` reads the session list; a full session read sees the turn  *)
(* as it is when it reads; a delta alone is due at once, so it asks for a   *)
(* full read, a session event (a session read with `idx`, the list read     *)
(* owed once the turn has ended) unless the pass read the list; a session   *)
(* read with `idx` that finds the turn ended reads the list. A full session *)
(* read covers a delta's own work, so a delta joining one is dropped from   *)
(* the set; `KeepObligation` keeps its `idx`. The read that                 *)
(* readSessionUpdate does is not modelled: due at once is the case that     *)
(* asks for a full read.                                                    *)
(*                                                                          *)
(* The turn need not end: a followed turn can run as long as it likes, and *)
(* the list must not wait for it.                                           *)
(*                                                                          *)
(* GREEN `MCProjectionDrain`: the merge keeps `idx`, and a tracked update   *)
(* that changed a row reads the list. RED                                   *)
(* `MCProjectionDrainMergeDropsIndex`: without the list read, the merge     *)
(* drops `idx`, and a turn's end delivered beside a session request never   *)
(* reaches the list. RED `MCProjectionDrainTrackedUpdateSkipsList`: a       *)
(* tracked update reads only the open session, and another session's       *)
(* change waits for the turn's end, which may never come.                   *)
(***************************************************************************)
EXTENDS Naturals

CONSTANTS
  SessionRequests,     \* session requests the environment may make
  OtherChanges,        \* changes to other sessions' rows the environment may make
  ReplyPieces,         \* the followed turn's reply pieces
  KeepObligation,      \* a delta merged into a session read keeps its list read
  ListOnTrackedUpdate  \* a store update while the turn is tracked reads the list

Work == {"session", "delta", "idx", "list"}

VARIABLES
  turn,      \* "running" | "ended"
  list,      \* "current" | "stale": whether the list shows the turn's row as it is
  others,    \* "current" | "stale": whether the list shows the other rows as they are
  pending,   \* the work the next pass takes
  taken,     \* the work the running pass took, or {} with none running
  requests,  \* session requests left
  changes,   \* other sessions' row changes left
  pieces     \* reply pieces left

vars == <<turn, list, others, pending, taken, requests, changes, pieces>>

TypeOK ==
  /\ turn \in {"running", "ended"}
  /\ list \in {"current", "stale"}
  /\ others \in {"current", "stale"}
  /\ pending \subseteq Work
  /\ taken \subseteq Work
  /\ requests \in 0..SessionRequests
  /\ changes \in 0..OtherChanges
  /\ pieces \in 0..ReplyPieces

\* `enqueue`: a full session read covers a delta, which leaves the set; with
\* the fix its list read stays owed.
Enqueue(set, work) ==
  LET joined == set \cup work
  IN IF "session" \in joined /\ "delta" \in joined
       THEN (joined \ {"delta"}) \cup (IF KeepObligation THEN {"idx"} ELSE {})
       ELSE joined

\* `desktopUpdateRefreshScope` for a store update that changed a list row
\* while the turn is tracked; one that changed none asks for the delta alone.
TrackedRowUpdate ==
  IF ListOnTrackedUpdate THEN {"delta", "list"} ELSE {"delta"}

Init ==
  /\ turn = "running"
  /\ list = "current"
  /\ others = "current"
  /\ pending = {}
  /\ taken = {}
  /\ requests = SessionRequests
  /\ changes = OtherChanges
  /\ pieces = ReplyPieces

(* Environment *)

\* The turn ends; its store update arrives while it is still tracked.
TurnEnds ==
  /\ turn = "running"
  /\ turn' = "ended"
  /\ list' = "stale"
  /\ pending' = Enqueue(pending, TrackedRowUpdate)
  /\ UNCHANGED <<others, taken, requests, changes, pieces>>

\* Another session's row changes (its status, say) while the turn is
\* tracked; its store update is one more tracked update.
OtherRowChanges ==
  /\ turn = "running"
  /\ changes > 0
  /\ changes' = changes - 1
  /\ others' = "stale"
  /\ pending' = Enqueue(pending, TrackedRowUpdate)
  /\ UNCHANGED <<turn, list, taken, requests, pieces>>

\* A reply piece of the followed turn: an output row, which no list row
\* shows, so the session list fingerprint stays and only the delta is read.
ReplyPiece ==
  /\ turn = "running"
  /\ pieces > 0
  /\ pieces' = pieces - 1
  /\ pending' = Enqueue(pending, {"delta"})
  /\ UNCHANGED <<turn, list, others, taken, requests, changes>>

RequestSession ==
  /\ requests > 0
  /\ requests' = requests - 1
  /\ pending' = Enqueue(pending, {"session"})
  /\ UNCHANGED <<turn, list, others, taken, changes, pieces>>

(* The drain loop *)

Take ==
  /\ taken = {} /\ pending # {}
  /\ taken' = pending
  /\ pending' = {}
  /\ UNCHANGED <<turn, list, others, requests, changes, pieces>>

\* The pass reads the list when it took `list`, or when its session read
\* with `idx` finds the turn ended; a list read shows every row as it is.
Finish ==
  /\ taken # {}
  /\ taken' = {}
  /\ LET readsList ==
           \/ "list" \in taken
           \/ "session" \in taken /\ "idx" \in taken /\ turn = "ended"
     IN /\ list' = IF readsList THEN "current" ELSE list
        /\ others' = IF readsList THEN "current" ELSE others
  /\ pending' =
       IF "session" \notin taken /\ "delta" \in taken
         THEN \* a delta alone is due at once: a full read follows
              Enqueue(pending,
                      IF "list" \in taken THEN {"session"} ELSE {"session", "idx"})
         ELSE pending
  /\ UNCHANGED <<turn, requests, changes, pieces>>

Next == TurnEnds \/ OtherRowChanges \/ ReplyPiece \/ RequestSession \/ Take \/ Finish

Spec == Init /\ [][Next]_vars /\ WF_vars(Take) /\ WF_vars(Finish)

\* Once the turn has ended and requests stop, the session list shows it.
ListCatchesUp == <>[](turn = "ended" => list = "current")

\* Another session's change reaches the list whether or not the turn ends.
OthersCatchUp == [](others = "stale" => <>(others = "current"))

====
