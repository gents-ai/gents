---- MODULE PackUxWatch ----
\* A pack page's live data watch and the desktop state that races it.
\*
\* Writers. The watch task (`gents_desktop_core::pack_watch::run`, spawned by
\* the bridge's `desktop_ux_data_watch`) acquires the runtime endpoint and the
\* User's signed authorization from the client core, posts `/packs/ux/watch`
\* and forwards each server-sent snapshot, and a notice on every entry into
\* backoff or terminal, to the page's IPC channel. The runtime
\* (`pack_watch::watch`) sends a full snapshot on every connect and a newer one
\* after each write, and ends every stream after its lifetime (30 s). A runtime
\* restart moves it to a new endpoint (port and credentials); the old endpoint
\* never answers again. The window changes its selected node under the bridge
\* lock in two steps, the registry cancellation and the selection write
\* (`desktop_set_selected_node`); the page's unwatch and the window's
\* destruction cancel through the watch registry. A finished task releases its
\* registry entry.
\*
\* Reader. The window shows what the channel delivered (`shown`); a delivery
\* is for node A, the watched node, so it is cross-node when the window then
\* selects anything else.
\*
\* Hazards, each an unfair one-shot environment action: `stop` (the runtime
\* stops for good), `revoke` (it refuses the User's credentials), `broken`
\* (its watcher fails right after every connect's snapshot), `malformed` (it
\* emits an undecodable event).
\*
\* Guards, each disabled by one RED config:
\*   RetryTransient       a transient failure backs off and reconnects;
\*   Reacquire            every connect re-acquires endpoint and authorization;
\*   CancelOnSelect       a selection change cancels the window's watches;
\*   CancelBeforeSelect   the cancellation happens before the selection write;
\*   AtomicDeliver        the registration check and the channel send are one
\*                        step under the registry lock (`UxWatchRegistry::deliver`);
\*   CancelOnDestroy      window destruction cancels the window's watches;
\*   NotifyStall          a failed attempt sends the page a reconnecting notice;
\*   KeepFailuresOnAccept an accepted connection keeps the failure count; only
\*                        a clean end resets it;
\*   BoundedRetry         RetryBudget consecutive failures end the watch.
\*
\* No DefraDB transaction is involved: the runtime's stream is a read, and the
\* registry and selection are in-process state behind two mutexes. Each
\* critical section is one step; the selection change holds the bridge lock
\* across both of its steps (so Start and Destroy, which take it, wait), but
\* the registry lock only for the cancellation, so a delivery can run between
\* them.
EXTENDS Integers

CONSTANTS
  RetryTransient,
  Reacquire,
  CancelOnSelect,
  CancelBeforeSelect,
  AtomicDeliver,
  CancelOnDestroy,
  NotifyStall,
  KeepFailuresOnAccept,
  BoundedRetry,
  Hazards,        \* subset of {"stop", "revoke", "broken", "malformed"}
  RetryBudget,    \* Lean `retryBudget`, scaled down
  OutageAttempts, \* failed connects one restart's outage can cause
  MaxRestarts,    \* runtime restarts
  MaxFaults,      \* read timeouts on an otherwise healthy stream
  MaxRev,         \* writes to the watched data
  MaxSelects      \* window selection changes

ASSUME /\ RetryTransient \in BOOLEAN /\ Reacquire \in BOOLEAN
       /\ CancelOnSelect \in BOOLEAN /\ CancelBeforeSelect \in BOOLEAN
       /\ AtomicDeliver \in BOOLEAN /\ CancelOnDestroy \in BOOLEAN
       /\ NotifyStall \in BOOLEAN /\ KeepFailuresOnAccept \in BOOLEAN
       /\ BoundedRetry \in BOOLEAN
       /\ Hazards \subseteq {"stop", "revoke", "broken", "malformed"}
       /\ RetryBudget \in Nat \ {0} /\ OutageAttempts \in Nat
       /\ MaxRestarts \in Nat /\ MaxFaults \in Nat /\ MaxRev \in Nat
       /\ MaxSelects \in Nat

\* External premise, the time abstraction: the backoff spacing outlasts a
\* runtime restart within OutageAttempts failed connects, and the budget
\* covers every transient failure a healthy runtime can cause.
ASSUME MaxFaults + MaxRestarts * (1 + OutageAttempts) < RetryBudget

None == -1
Phases == {"idle", "starting", "streaming", "backoff", "terminal", "cancelled"}
Live == {"starting", "streaming", "backoff"}
Reasons == {"none", "transient", "auth", "permanent", "unreachable"}

VARIABLES
  sel,        \* the window's selected node, or "destroyed"
  selecting,  \* a selection change between its two steps: its target, or "none"
  selects,
  left,       \* the window selected away from A after the watch started
  phase,      \* the watch task
  failures,   \* consecutive failed attempts since the last clean end
  reason,     \* why the watch is terminal
  notice,     \* the notice the task owes the page: "none", "reconnecting", "unavailable"
  registered, \* the registry holds the watch's live token
  unwatched,
  used,       \* endpoint epoch the task holds
  epoch,      \* the runtime's current endpoint epoch
  up,         \* the runtime answers on its current endpoint
  outage,     \* failed connects during the current restart's outage
  stopped, revoked, broken, malformed,
  rev,        \* the watched data's latest revision
  seen,       \* latest revision the open stream emitted
  pending,    \* decoded snapshot not yet sent, or None
  checked,    \* unlocked path: the registration check passed, send not done
  sent,       \* latest revision delivered to the window, or None
  shown,      \* what the page shows: "none", "live", "reconnecting", "unavailable"
  faults,
  cross       \* a delivery of A reached a window that no longer selects A

win == <<sel, selecting, selects, left, unwatched>>
task == <<phase, failures, reason, notice, used, seen, pending, checked>>
env == <<epoch, up, outage, stopped, revoked, broken, malformed, rev, faults>>
page == <<sent, shown, cross>>
vars == <<win, task, env, page, registered>>

TypeOK ==
  /\ sel \in {"A", "B", "destroyed"}
  /\ selecting \in {"none", "A", "B"}
  /\ selects \in 0..MaxSelects /\ left \in BOOLEAN
  /\ phase \in Phases
  /\ failures \in 0..RetryBudget /\ reason \in Reasons
  /\ notice \in {"none", "reconnecting", "unavailable"}
  /\ registered \in BOOLEAN /\ unwatched \in BOOLEAN
  /\ used \in 0..MaxRestarts /\ epoch \in 0..MaxRestarts
  /\ up \in BOOLEAN /\ outage \in 0..OutageAttempts
  /\ stopped \in BOOLEAN /\ revoked \in BOOLEAN
  /\ broken \in BOOLEAN /\ malformed \in BOOLEAN
  /\ rev \in 0..MaxRev
  /\ seen \in {None} \cup 0..MaxRev
  /\ pending \in {None} \cup 0..MaxRev
  /\ checked \in BOOLEAN
  /\ sent \in {None} \cup 0..MaxRev
  /\ shown \in {"none", "live", "reconnecting", "unavailable"}
  /\ faults \in 0..MaxFaults
  /\ cross \in BOOLEAN

Init ==
  /\ sel = "A" /\ selecting = "none" /\ selects = 0 /\ left = FALSE
  /\ phase = "idle" /\ failures = 0 /\ reason = "none" /\ notice = "none"
  /\ registered = FALSE /\ unwatched = FALSE
  /\ used = 0 /\ epoch = 0 /\ up = TRUE /\ outage = 0
  /\ stopped = FALSE /\ revoked = FALSE /\ broken = FALSE /\ malformed = FALSE
  /\ rev = 0 /\ seen = None /\ pending = None /\ checked = FALSE
  /\ sent = None /\ shown = "none" /\ faults = 0 /\ cross = FALSE

Min(a, b) == IF a < b THEN a ELSE b

\* The task's own step ends with a terminal reason and owes the page a notice.
Terminate(r) ==
  /\ phase' = "terminal" /\ reason' = r /\ notice' = "unavailable"
  /\ UNCHANGED failures

\* A failed attempt (Lean `fail`).
Fail ==
  IF ~RetryTransient THEN Terminate("transient")
  ELSE IF BoundedRetry /\ failures + 1 >= RetryBudget THEN Terminate("unreachable")
  ELSE /\ phase' = "backoff" /\ failures' = Min(failures + 1, RetryBudget)
       /\ notice' = IF NotifyStall THEN "reconnecting" ELSE "none"
       /\ UNCHANGED reason

\* The task is at an await point: it owes the page nothing.
Ready == notice = "none" /\ ~checked

\* The watch command: gated on the window selecting A, under the bridge lock.
Start ==
  /\ phase = "idle" /\ sel = "A" /\ selecting = "none"
  /\ phase' = "starting" /\ registered' = TRUE /\ used' = epoch
  /\ UNCHANGED <<win, failures, reason, notice, seen, pending, checked, env, page>>

CancelWindow == registered' = IF CancelOnSelect THEN FALSE ELSE registered

\* The first step of a selection change, under the bridge lock.
BeginSelect(n) ==
  /\ selecting = "none" /\ sel \in {"A", "B"} /\ n \in {"A", "B"} /\ n # sel
  /\ selects < MaxSelects
  /\ selecting' = n /\ selects' = selects + 1
  /\ left' = (left \/ phase # "idle")
  /\ IF CancelBeforeSelect
     THEN CancelWindow /\ UNCHANGED sel
     ELSE sel' = n /\ UNCHANGED registered
  /\ UNCHANGED <<unwatched, task, env, page>>

\* The second step, still under the bridge lock.
EndSelect ==
  /\ selecting # "none"
  /\ selecting' = "none"
  /\ IF CancelBeforeSelect
     THEN sel' = selecting /\ UNCHANGED registered
     ELSE CancelWindow /\ UNCHANGED sel
  /\ UNCHANGED <<selects, left, unwatched, task, env, page>>

Unwatch ==
  /\ phase # "idle" /\ ~unwatched
  /\ unwatched' = TRUE /\ registered' = FALSE
  /\ UNCHANGED <<sel, selecting, selects, left, task, env, page>>

Destroy ==
  /\ sel # "destroyed" /\ selecting = "none"
  /\ sel' = "destroyed"
  /\ registered' = IF CancelOnDestroy THEN FALSE ELSE registered
  /\ UNCHANGED <<selecting, selects, left, unwatched, task, env, page>>

\* The task observes its cancelled token at an await point.
ObserveCancel ==
  /\ phase \in Live /\ ~registered /\ Ready
  /\ phase' = "cancelled" /\ pending' = None
  /\ UNCHANGED <<win, registered, failures, reason, notice, used, seen, checked,
                 env, page>>

Connect ==
  /\ phase = "starting" /\ registered /\ Ready
  /\ LET e == IF Reacquire THEN epoch ELSE used IN
       /\ used' = e
       /\ IF up /\ e = epoch
          THEN IF revoked
               THEN Terminate("auth") /\ UNCHANGED <<pending, seen, outage>>
               ELSE /\ phase' = "streaming" /\ pending' = rev /\ seen' = rev
                    /\ failures' = IF KeepFailuresOnAccept THEN failures ELSE 0
                    /\ UNCHANGED <<reason, notice, outage>>
          ELSE IF e = epoch /\ ~stopped
               \* a restart's outage: bounded by the time premise
               THEN /\ outage < OutageAttempts /\ outage' = outage + 1
                    /\ Fail /\ UNCHANGED <<pending, seen>>
               ELSE Fail /\ UNCHANGED <<pending, seen, outage>>
  /\ UNCHANGED <<win, registered, checked, epoch, up, stopped, revoked, broken,
                 malformed, rev, faults, page>>

Write ==
  /\ rev < MaxRev /\ rev' = rev + 1
  /\ UNCHANGED <<win, task, registered, epoch, up, outage, stopped, revoked,
                 broken, malformed, faults, page>>

Healthy == up /\ used = epoch /\ ~broken /\ ~malformed

Emit ==
  /\ phase = "streaming" /\ pending = None /\ Ready
  /\ Healthy /\ seen < rev
  /\ pending' = rev /\ seen' = rev
  /\ UNCHANGED <<win, registered, phase, failures, reason, notice, used, checked,
                 env, page>>

Deliver ==
  /\ sent' = pending /\ pending' = None /\ shown' = "live"
  /\ cross' = (cross \/ sel # "A")

\* A closed channel (destroyed window) or a refused check ends the task.
Drop ==
  /\ phase' = "cancelled" /\ pending' = None /\ registered' = FALSE
  /\ UNCHANGED page

SendAtomic ==
  /\ AtomicDeliver /\ phase = "streaming" /\ pending # None /\ notice = "none"
  /\ IF registered /\ sel # "destroyed"
     THEN Deliver /\ UNCHANGED <<phase, registered>>
     ELSE Drop
  /\ UNCHANGED <<win, failures, reason, notice, used, seen, checked, env>>

Check ==
  /\ ~AtomicDeliver /\ phase = "streaming" /\ pending # None /\ Ready
  /\ IF registered
     THEN checked' = TRUE /\ UNCHANGED <<phase, pending, registered, page>>
     ELSE Drop /\ UNCHANGED checked
  /\ UNCHANGED <<win, failures, reason, notice, used, seen, env>>

SendChecked ==
  /\ checked /\ checked' = FALSE
  /\ IF sel # "destroyed"
     THEN Deliver /\ UNCHANGED <<phase, registered>>
     ELSE Drop
  /\ UNCHANGED <<win, failures, reason, notice, used, seen, env>>

\* The task delivers the notice it owes, under the registry lock like a
\* snapshot; a destroyed window's closed channel ends a live task.
Notify ==
  /\ notice # "none"
  /\ notice' = "none"
  /\ IF registered /\ sel # "destroyed"
     THEN /\ shown' = notice /\ cross' = (cross \/ sel # "A")
          /\ UNCHANGED <<phase, registered, sent>>
     ELSE IF registered /\ phase = "backoff"
          THEN phase' = "cancelled" /\ registered' = FALSE /\ UNCHANGED page
          ELSE UNCHANGED <<phase, registered, page>>
  /\ UNCHANGED <<win, failures, reason, used, seen, pending, checked, env>>

\* A finished task releases its registry entry after its last notice.
Release ==
  /\ phase = "terminal" /\ registered /\ notice = "none"
  /\ registered' = FALSE
  /\ UNCHANGED <<win, task, env, page>>

StreamIdle == phase = "streaming" /\ pending = None /\ Ready

\* The runtime's stream lifetime ends a healthy stream: the only reset. The
\* data is current and the next connect is a second away, so the page is not
\* told (Lean `notice (.backoff 0)`).
ServerEnd ==
  /\ StreamIdle /\ Healthy
  /\ phase' = "backoff" /\ failures' = 0
  /\ UNCHANGED <<win, registered, reason, notice, used, seen, pending, checked,
                 env, page>>

ReadTimeout ==
  /\ StreamIdle
  /\ faults < MaxFaults /\ faults' = faults + 1
  /\ Fail
  /\ UNCHANGED <<win, registered, used, seen, pending, checked, epoch, up, outage,
                 stopped, revoked, broken, malformed, rev, page>>

\* The open connection breaks when the runtime goes away, or its watcher fails
\* right after the connect's snapshot.
StreamBroken ==
  /\ StreamIdle
  /\ (~up \/ used # epoch \/ broken)
  /\ Fail
  /\ UNCHANGED <<win, registered, used, seen, pending, checked, env, page>>

Garbage ==
  /\ StreamIdle /\ up /\ used = epoch /\ malformed
  /\ Terminate("permanent")
  /\ UNCHANGED <<win, registered, used, seen, pending, checked, env, page>>

Restart ==
  /\ epoch < MaxRestarts /\ ~stopped
  /\ epoch' = epoch + 1 /\ up' = FALSE /\ outage' = 0
  /\ UNCHANGED <<win, task, registered, stopped, revoked, broken, malformed, rev,
                 faults, page>>

ServerUp ==
  /\ ~up /\ ~stopped /\ up' = TRUE
  /\ UNCHANGED <<win, task, registered, epoch, outage, stopped, revoked, broken,
                 malformed, rev, faults, page>>

Hazard(h) ==
  /\ h \in Hazards
  /\ \/ h = "stop" /\ ~stopped /\ stopped' = TRUE /\ up' = FALSE
        /\ UNCHANGED <<revoked, broken, malformed>>
     \/ h = "revoke" /\ ~revoked /\ revoked' = TRUE
        /\ UNCHANGED <<stopped, up, broken, malformed>>
     \/ h = "broken" /\ ~broken /\ broken' = TRUE
        /\ UNCHANGED <<stopped, up, revoked, malformed>>
     \/ h = "malformed" /\ ~malformed /\ malformed' = TRUE
        /\ UNCHANGED <<stopped, up, revoked, broken>>
  /\ UNCHANGED <<win, task, registered, epoch, outage, rev, faults, page>>

Retry ==
  /\ phase = "backoff" /\ registered /\ Ready
  /\ phase' = "starting"
  /\ UNCHANGED <<win, registered, failures, reason, notice, used, seen, pending,
                 checked, env, page>>

Next ==
  \/ Start \/ \E n \in {"A", "B"} : BeginSelect(n) \/ EndSelect
  \/ Unwatch \/ Destroy
  \/ ObserveCancel \/ Connect \/ Write \/ Emit \/ SendAtomic \/ Check
  \/ SendChecked \/ Notify \/ Release \/ ServerEnd \/ ReadTimeout \/ Restart
  \/ ServerUp \/ StreamBroken \/ Garbage \/ Retry
  \/ \E h \in {"stop", "revoke", "broken", "malformed"} : Hazard(h)

\* Fairness covers the task's own steps, the runtime coming back up after a
\* restart, the runtime's stream lifetime and its reaction to a hazard, and
\* the selection change finishing; faults, writes, hazards and user actions
\* are unfair.
Fairness ==
  /\ WF_vars(ObserveCancel) /\ WF_vars(Connect) /\ WF_vars(Emit)
  /\ WF_vars(SendAtomic) /\ WF_vars(Check) /\ WF_vars(SendChecked)
  /\ WF_vars(Notify) /\ WF_vars(Release) /\ WF_vars(EndSelect)
  /\ WF_vars(ServerUp) /\ WF_vars(StreamBroken) /\ WF_vars(Garbage)
  /\ WF_vars(Retry) /\ WF_vars(ServerEnd)

Spec == Init /\ [][Next]_vars /\ Fairness

NoCrossNodeDelivery == ~cross

\* A cancelled or destroyed window's watch holds no registry entry.
RegistryReleased == (sel = "destroyed" \/ unwatched) => ~registered

StillWatched == ~unwatched /\ ~left /\ sel # "destroyed"

\* Nothing gets silently stuck: a watch that is backing off for a page still
\* watching it is shown as reconnecting, and one that ended is shown as
\* unavailable, once the task has delivered what it owes.
NotSilent ==
  (notice = "none" /\ StillWatched /\ registered /\ phase = "backoff" /\ failures > 0)
    => shown = "reconnecting"

TerminalNamed ==
  (notice = "none" /\ StillWatched /\ phase = "terminal") => shown = "unavailable"

Hazarded == stopped \/ revoked \/ broken \/ malformed

Resumed == phase = "streaming" /\ sent = rev /\ pending = None /\ Healthy

\* After a failure the watch resumes with the latest snapshot, or ends and
\* says so only because the runtime is gone for good, refuses the User, or
\* misbehaves on every stream; unless the page stopped watching A.
EventualResume ==
  (phase \in {"backoff", "terminal"}) ~>
    (\/ phase = "cancelled" \/ ~StillWatched \/ Resumed
     \/ (phase = "terminal" /\ Hazarded /\ shown = "unavailable"))

\* After cancellation or window destruction no task remains, and a finished
\* task gives up its registry entry.
NoLeakedWatch ==
  ((phase \in Live) /\ (~registered \/ sel = "destroyed")) ~>
    (phase \in {"cancelled", "terminal"})

EntryReleased == (phase \in {"cancelled", "terminal"}) ~> ~registered

====
