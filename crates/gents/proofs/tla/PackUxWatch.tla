---- MODULE PackUxWatch ----
\* A pack page's live data watch and the desktop state that races it.
\*
\* Writers. The desktop bridge's watch task (`ux_watch::run`) acquires the
\* runtime endpoint and the User's signed authorization from the client core,
\* posts `/packs/ux/watch` and forwards each server-sent snapshot to the page's
\* IPC channel. The runtime (`pack_watch::watch`) sends a full snapshot on every
\* connect and a newer one after each write, and ends every stream after its
\* lifetime (30 s). A runtime restart moves it to a new endpoint (port and
\* credentials); the old endpoint never answers again. The window selects a
\* node under the bridge lock (`desktop_set_selected_node`) and cancels the
\* window's watches there; the page's unwatch and the window's destruction
\* cancel through the watch registry. A finished task releases its registry
\* entry.
\*
\* Reader. The window shows what the channel delivered; a delivery is for node
\* A, the watched node, so it is cross-node when the window then selects
\* anything else.
\*
\* Guards, each disabled by one RED config:
\*   RetryTransient   a transient failure backs off and reconnects;
\*   Reacquire        every connect re-acquires endpoint and authorization;
\*   CancelOnSelect   a selection change cancels the window's watches;
\*   AtomicDeliver    the registration check and the channel send are one step
\*                    under the registry lock (`UxWatchRegistry::deliver`);
\*   CancelOnDestroy  window destruction cancels the window's watches.
\*
\* No DefraDB transaction is involved: the runtime's stream is a read, and the
\* registry and selection are in-process state behind two mutexes, whose
\* critical sections are the atomic steps here.
EXTENDS Integers

CONSTANTS
  RetryTransient,
  Reacquire,
  CancelOnSelect,
  AtomicDeliver,
  CancelOnDestroy,
  MaxRestarts,  \* runtime restarts
  MaxFaults,    \* read timeouts on an otherwise healthy stream
  MaxRev,       \* writes to the watched data
  MaxSelects    \* window selection changes

ASSUME /\ RetryTransient \in BOOLEAN /\ Reacquire \in BOOLEAN
       /\ CancelOnSelect \in BOOLEAN /\ AtomicDeliver \in BOOLEAN
       /\ CancelOnDestroy \in BOOLEAN
       /\ MaxRestarts \in Nat /\ MaxFaults \in Nat /\ MaxRev \in Nat
       /\ MaxSelects \in Nat

None == -1
Phases == {"idle", "starting", "streaming", "backoff", "terminal", "cancelled"}
Live == {"starting", "streaming", "backoff"}

VARIABLES
  sel,        \* the window's selected node, or "destroyed"
  selects,
  phase,      \* the watch task
  registered, \* the registry holds the watch's live token
  unwatched,
  used,       \* endpoint epoch the task holds
  epoch,      \* the runtime's current endpoint epoch
  up,         \* the runtime answers on its current endpoint
  rev,        \* the watched data's latest revision
  seen,       \* latest revision the open stream emitted
  pending,    \* decoded snapshot not yet sent, or None
  checked,    \* unlocked path: the registration check passed, send not done
  sent,       \* latest revision delivered to the window, or None
  faults,
  cross       \* a snapshot of A reached a window that no longer selects A

vars == <<sel, selects, phase, registered, unwatched, used, epoch, up, rev,
          seen, pending, checked, sent, faults, cross>>

TypeOK ==
  /\ sel \in {"A", "B", "destroyed"}
  /\ selects \in 0..MaxSelects
  /\ phase \in Phases
  /\ registered \in BOOLEAN /\ unwatched \in BOOLEAN
  /\ used \in 0..MaxRestarts /\ epoch \in 0..MaxRestarts
  /\ up \in BOOLEAN
  /\ rev \in 0..MaxRev
  /\ seen \in {None} \cup 0..MaxRev
  /\ pending \in {None} \cup 0..MaxRev
  /\ checked \in BOOLEAN
  /\ sent \in {None} \cup 0..MaxRev
  /\ faults \in 0..MaxFaults
  /\ cross \in BOOLEAN

Init ==
  /\ sel = "A" /\ selects = 0
  /\ phase = "idle" /\ registered = FALSE /\ unwatched = FALSE
  /\ used = 0 /\ epoch = 0 /\ up = TRUE
  /\ rev = 0 /\ seen = None /\ pending = None /\ checked = FALSE
  /\ sent = None /\ faults = 0 /\ cross = FALSE

\* A transient failure: back off, or end the watch and release its entry.
Fail ==
  IF RetryTransient
  THEN phase' = "backoff" /\ UNCHANGED registered
  ELSE phase' = "terminal" /\ registered' = FALSE

\* The watch command: gated on the window selecting A, under the bridge lock.
Start ==
  /\ phase = "idle" /\ sel = "A"
  /\ phase' = "starting" /\ registered' = TRUE /\ used' = epoch
  /\ UNCHANGED <<sel, selects, unwatched, epoch, up, rev, seen, pending,
                 checked, sent, faults, cross>>

Select(n) ==
  /\ sel \in {"A", "B"} /\ n \in {"A", "B"} /\ n # sel
  /\ selects < MaxSelects
  /\ sel' = n /\ selects' = selects + 1
  /\ registered' = IF CancelOnSelect THEN FALSE ELSE registered
  /\ UNCHANGED <<phase, unwatched, used, epoch, up, rev, seen, pending,
                 checked, sent, faults, cross>>

Unwatch ==
  /\ phase # "idle" /\ ~unwatched
  /\ unwatched' = TRUE /\ registered' = FALSE
  /\ UNCHANGED <<sel, selects, phase, used, epoch, up, rev, seen, pending,
                 checked, sent, faults, cross>>

Destroy ==
  /\ sel # "destroyed"
  /\ sel' = "destroyed"
  /\ registered' = IF CancelOnDestroy THEN FALSE ELSE registered
  /\ UNCHANGED <<selects, phase, unwatched, used, epoch, up, rev, seen,
                 pending, checked, sent, faults, cross>>

\* The task observes its cancelled token at an await point.
ObserveCancel ==
  /\ phase \in Live /\ ~registered /\ ~checked
  /\ phase' = "cancelled" /\ pending' = None
  /\ UNCHANGED <<sel, selects, registered, unwatched, used, epoch, up, rev,
                 seen, checked, sent, faults, cross>>

Connect ==
  /\ phase = "starting" /\ registered
  /\ LET e == IF Reacquire THEN epoch ELSE used IN
       /\ used' = e
       /\ IF up /\ e = epoch
          THEN /\ phase' = "streaming" /\ pending' = rev /\ seen' = rev
               /\ UNCHANGED registered
          ELSE /\ Fail /\ UNCHANGED <<pending, seen>>
  /\ UNCHANGED <<sel, selects, unwatched, epoch, up, rev, checked, sent,
                 faults, cross>>

Write ==
  /\ rev < MaxRev /\ rev' = rev + 1
  /\ UNCHANGED <<sel, selects, phase, registered, unwatched, used, epoch, up,
                 seen, pending, checked, sent, faults, cross>>

Emit ==
  /\ phase = "streaming" /\ pending = None /\ ~checked
  /\ up /\ used = epoch /\ seen < rev
  /\ pending' = rev /\ seen' = rev
  /\ UNCHANGED <<sel, selects, phase, registered, unwatched, used, epoch, up,
                 rev, checked, sent, faults, cross>>

Deliver ==
  /\ sent' = pending /\ pending' = None
  /\ cross' = (cross \/ sel # "A")

\* A closed channel (destroyed window) or a refused check ends the task.
Drop ==
  /\ phase' = "cancelled" /\ pending' = None /\ registered' = FALSE
  /\ UNCHANGED <<sent, cross>>

SendAtomic ==
  /\ AtomicDeliver /\ phase = "streaming" /\ pending # None
  /\ IF registered /\ sel # "destroyed"
     THEN Deliver /\ UNCHANGED <<phase, registered>>
     ELSE Drop
  /\ UNCHANGED <<sel, selects, unwatched, used, epoch, up, rev, seen, checked,
                 faults>>

Check ==
  /\ ~AtomicDeliver /\ phase = "streaming" /\ pending # None /\ ~checked
  /\ IF registered
     THEN checked' = TRUE /\ UNCHANGED <<phase, pending, registered, sent, cross>>
     ELSE Drop /\ UNCHANGED checked
  /\ UNCHANGED <<sel, selects, unwatched, used, epoch, up, rev, seen, faults>>

SendChecked ==
  /\ checked /\ checked' = FALSE
  /\ IF sel # "destroyed"
     THEN Deliver /\ UNCHANGED <<phase, registered>>
     ELSE Drop
  /\ UNCHANGED <<sel, selects, unwatched, used, epoch, up, rev, seen, faults>>

\* The runtime's stream lifetime ends a healthy stream.
ServerEnd ==
  /\ phase = "streaming" /\ pending = None /\ ~checked
  /\ phase' = "backoff"
  /\ UNCHANGED <<sel, selects, registered, unwatched, used, epoch, up, rev,
                 seen, pending, checked, sent, faults, cross>>

ReadTimeout ==
  /\ phase = "streaming" /\ pending = None /\ ~checked
  /\ faults < MaxFaults /\ faults' = faults + 1
  /\ Fail
  /\ UNCHANGED <<sel, selects, unwatched, used, epoch, up, rev, seen, pending,
                 checked, sent, cross>>

Restart ==
  /\ epoch < MaxRestarts
  /\ epoch' = epoch + 1 /\ up' = FALSE
  /\ UNCHANGED <<sel, selects, phase, registered, unwatched, used, rev, seen,
                 pending, checked, sent, faults, cross>>

ServerUp ==
  /\ ~up /\ up' = TRUE
  /\ UNCHANGED <<sel, selects, phase, registered, unwatched, used, epoch, rev,
                 seen, pending, checked, sent, faults, cross>>

\* The open connection breaks when the runtime goes away.
StreamBroken ==
  /\ phase = "streaming" /\ pending = None /\ ~checked
  /\ (~up \/ used # epoch)
  /\ Fail
  /\ UNCHANGED <<sel, selects, unwatched, used, epoch, up, rev, seen, pending,
                 checked, sent, faults, cross>>

Retry ==
  /\ phase = "backoff" /\ registered
  /\ phase' = "starting"
  /\ UNCHANGED <<sel, selects, registered, unwatched, used, epoch, up, rev,
                 seen, pending, checked, sent, faults, cross>>

Next ==
  \/ Start \/ \E n \in {"A", "B"} : Select(n) \/ Unwatch \/ Destroy
  \/ ObserveCancel \/ Connect \/ Write \/ Emit \/ SendAtomic \/ Check
  \/ SendChecked \/ ServerEnd \/ ReadTimeout \/ Restart \/ ServerUp
  \/ StreamBroken \/ Retry

\* Fairness covers the task's own steps, the runtime coming back up, and the
\* runtime's stream lifetime; faults, writes and user actions are unfair.
Fairness ==
  /\ WF_vars(ObserveCancel) /\ WF_vars(Connect) /\ WF_vars(Emit)
  /\ WF_vars(SendAtomic) /\ WF_vars(Check) /\ WF_vars(SendChecked)
  /\ WF_vars(ServerUp) /\ WF_vars(StreamBroken) /\ WF_vars(Retry)
  /\ WF_vars(ServerEnd)

Spec == Init /\ [][Next]_vars /\ Fairness

NoCrossNodeDelivery == ~cross

\* A cancelled or destroyed window's watch holds no registry entry.
RegistryReleased == (sel = "destroyed" \/ unwatched) => ~registered

\* After a failure the watch resumes with the latest snapshot, unless the page
\* or the window stopped watching A.
EventualResume ==
  (phase \in {"backoff", "terminal"}) ~>
    (\/ phase = "cancelled" \/ unwatched \/ sel # "A"
     \/ (phase = "streaming" /\ sent = rev /\ pending = None))

\* After cancellation or window destruction no task remains.
NoLeakedWatch ==
  ((phase \in Live) /\ (~registered \/ sel = "destroyed")) ~>
    (phase \in {"cancelled", "terminal"})

====
