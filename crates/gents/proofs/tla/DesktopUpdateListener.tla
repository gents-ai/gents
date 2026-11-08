---- MODULE DesktopUpdateListener ----
(***************************************************************************)
(* The last leg of the desktop's event path: the bridge emitter, Tauri     *)
(* delivery, the frontend's listener registration, its projection drain    *)
(* loop, and the selected session's load phases.                           *)
(*                                                                         *)
(* Rust side (crates/gents-desktop-bridge/src/state.rs,                    *)
(* spawn_client_update_task). The client's store and sync watches publish  *)
(* a committed version; `changed()` coalesces, so the emitter sends one     *)
(* `desktop://client-updated` event per wake and never misses the latest   *)
(* version. Store writes are DefraDB commits; a read sees exactly the      *)
(* versions committed before it starts (snapshot visibility), so a read is *)
(* modeled as taking `rust` at its start.                                  *)
(*                                                                         *)
(* Tauri drops an event emitted while no handler is registered: there is   *)
(* no replay. The frontend registers asynchronously (`listen` resolves     *)
(* later), and the registration may reject.                                *)
(*                                                                         *)
(* Frontend (apps/gents-desktop/src/hooks): clientObservation.run starts   *)
(* the listener and requests reads; projectionController coalesces         *)
(* requests into one pending set drained one pass at a time;               *)
(* sessionReads.refreshSession writes the load phase. A selected request  *)
(* that has not reached its owner answers `pending`; the runtime may then *)
(* catch up with no store notice at all (`ArriveSilently`), because the    *)
(* desktop holds no replica of a remote runtime's session.                *)
(*                                                                         *)
(* Guards (each RED config turns one off):                                 *)
(*   InitialRead      until a registration succeeds, a rejection or a      *)
(*                    deadline reads the selection once without it, so a  *)
(*                    listener that never registers cannot leave it       *)
(*                    unread (StartupRead; the deadline is not modeled).  *)
(*   FullAfterListen  run() reads in full again once the listener         *)
(*                    resolves, covering commits Tauri dropped before.    *)
(*   Relisten         a rejected registration is retried (bounded backoff;*)
(*                    the delay is not modeled).                           *)
(*   PendingReread    a waiting phase schedules bounded re-reads and,      *)
(*                    once they are spent, shows the retry action.         *)
(*   MismatchExit     a reply for another session fails visibly. A read    *)
(*                    whose selection moved needs no guard: the selection  *)
(*                    change already queued a read (MovedReadQueued).      *)
(*                                                                         *)
(* SpecListenerUnfair drops fairness from registration: the listener may   *)
(* keep rejecting or never settle.                                         *)
(***************************************************************************)
EXTENDS Naturals

CONSTANTS
  MaxVersion,       \* committed store versions after startup
  ListenFailures,   \* registration rejections the environment may cause
  Mismatches,       \* replies for another target the bridge may return
  Selections,       \* selection changes the user may make
  RereadBudget,     \* re-reads a waiting phase is given
  InitialRead, FullAfterListen, Relisten, PendingReread, MismatchExit

Sessions == {1, 2}
NoRead == [v |-> 0, s |-> 0]
\* a selected request not at its owner yet, or a message still in delivery
Waiting == {"pending", "delivering", "notConnected", "accepted"}

VARIABLES
  rust,         \* the latest committed store version
  registered,   \* Tauri has a handler for desktop://client-updated
  listener,     \* "listening" | "ready" | "failed"
  failures,     \* registration rejections left
  inbox,        \* a delivered event the handler has not run yet (coalesced)
  queued,       \* the drain loop owes a read
  reading,      \* the latest refresh while it runs, or NoRead
  view,         \* the version the rendered session reflects
  arrived,      \* the runtime has the selected request's row
  selected,
  load,         \* the session the load state names (0: idle)
  phase,        \* "idle" | "loading" | "loaded" | "failed", or a Waiting phase
  timer,        \* a waiting phase's re-read is scheduled
  budget,
  mismatches,
  selections,
  startup       \* the observer's read without a listener is still owed

vars == <<rust, registered, listener, failures, inbox, queued, reading, view,
          arrived, selected, load, phase, timer, budget, mismatches, selections,
          startup>>

TypeOK ==
  /\ rust \in 0..MaxVersion
  /\ registered \in BOOLEAN
  /\ listener \in {"listening", "ready", "failed"}
  /\ inbox \in BOOLEAN /\ queued \in BOOLEAN /\ timer \in BOOLEAN
  /\ view \in 0..MaxVersion
  /\ selected \in Sessions /\ load \in Sessions \cup {0}
  /\ phase \in {"idle", "loading", "loaded", "failed"} \cup Waiting

Init ==
  /\ rust = 0
  /\ registered = FALSE
  /\ listener = "listening"
  /\ failures = ListenFailures
  /\ inbox = FALSE
  /\ queued = FALSE
  /\ startup = InitialRead
  /\ reading = NoRead
  /\ view = 0
  /\ arrived = FALSE
  /\ selected = 1
  /\ load = 0
  /\ phase = "idle"
  /\ timer = FALSE
  /\ budget = RereadBudget
  /\ mismatches = Mismatches
  /\ selections = Selections

(* Environment *)

\* A commit wakes the emitter; Tauri delivers only to a registered handler.
Commit ==
  /\ rust < MaxVersion
  /\ rust' = rust + 1
  /\ inbox' = (inbox \/ registered)
  /\ UNCHANGED <<registered, listener, failures, queued, reading, view, arrived,
                 selected, load, phase, timer, budget, mismatches, selections, startup>>

\* The selected request reaches its owner with a store notice ...
ArriveWithNotice ==
  /\ ~arrived /\ rust < MaxVersion
  /\ arrived' = TRUE
  /\ rust' = rust + 1
  /\ inbox' = (inbox \/ registered)
  /\ UNCHANGED <<registered, listener, failures, queued, reading, view,
                 selected, load, phase, timer, budget, mismatches, selections, startup>>

\* ... or with none: a remote runtime has no event source here.
ArriveSilently ==
  /\ ~arrived
  /\ arrived' = TRUE
  /\ UNCHANGED <<rust, registered, listener, failures, inbox, queued, reading, view,
                 selected, load, phase, timer, budget, mismatches, selections, startup>>

ListenRejects ==
  /\ listener = "listening" /\ failures > 0
  /\ listener' = "failed"
  /\ failures' = failures - 1
  /\ UNCHANGED <<rust, registered, inbox, queued, reading, view, arrived,
                 selected, load, phase, timer, budget, mismatches, selections, startup>>

\* The user selects another session; the selection subscription requests it.
Select ==
  /\ selections > 0
  /\ selections' = selections - 1
  /\ selected' \in Sessions \ {selected}
  /\ queued' = TRUE
  /\ timer' = FALSE
  /\ budget' = RereadBudget
  /\ UNCHANGED <<rust, registered, listener, failures, inbox, reading, view, arrived,
                 load, phase, mismatches, startup>>

(* Frontend *)

ListenResolves ==
  /\ listener = "listening"
  /\ listener' = "ready"
  /\ registered' = TRUE
  /\ queued' = (queued \/ FullAfterListen)
  \* the registration's full read replaces the read without a listener;
  \* before the fix nothing replaced it
  /\ startup' = (startup /\ ~FullAfterListen)
  /\ UNCHANGED <<rust, failures, inbox, reading, view, arrived, selected, load,
                 phase, timer, budget, mismatches, selections>>

\* A rejection or the deadline, before any registration succeeded.
StartupRead ==
  /\ startup
  /\ startup' = FALSE
  /\ queued' = TRUE
  /\ UNCHANGED <<rust, registered, listener, failures, inbox, reading, view, arrived,
                 selected, load, phase, timer, budget, mismatches, selections>>

Relistens ==
  /\ Relisten /\ listener = "failed"
  /\ listener' = "listening"
  /\ UNCHANGED <<rust, registered, failures, inbox, queued, reading, view, arrived,
                 selected, load, phase, timer, budget, mismatches, selections, startup>>

\* A read the re-read timer did not start begins a new re-read streak.
Handle ==
  /\ inbox
  /\ inbox' = FALSE
  /\ queued' = TRUE
  /\ budget' = RereadBudget
  /\ UNCHANGED <<rust, registered, listener, failures, reading, view, arrived,
                 selected, load, phase, timer, mismatches, selections, startup>>

StartRead ==
  /\ queued /\ reading = NoRead
  /\ queued' = FALSE
  /\ reading' = [v |-> rust, s |-> selected]
  /\ phase' = IF load = selected /\ phase \in Waiting THEN phase ELSE "loading"
  /\ load' = selected
  /\ timer' = FALSE
  /\ UNCHANGED <<rust, registered, listener, failures, inbox, view, arrived,
                 selected, budget, mismatches, selections, startup>>

FinishMoved ==
  /\ reading # NoRead /\ reading.s # selected
  /\ reading' = NoRead
  /\ UNCHANGED <<rust, registered, listener, failures, inbox, queued, view, arrived,
                 selected, load, phase, timer, budget, mismatches, selections, startup>>

FinishMismatch ==
  /\ reading # NoRead /\ reading.s = selected /\ mismatches > 0
  /\ mismatches' = mismatches - 1
  /\ reading' = NoRead
  /\ phase' = IF MismatchExit THEN "failed" ELSE phase
  /\ UNCHANGED <<rust, registered, listener, failures, inbox, queued, view, arrived,
                 selected, load, timer, budget, selections, startup>>

FinishRead ==
  /\ reading # NoRead /\ reading.s = selected
  /\ reading' = NoRead
  /\ IF arrived
       THEN /\ view' = reading.v
            /\ phase' = "loaded"
            /\ budget' = RereadBudget
            /\ timer' = FALSE
       ELSE /\ phase' \in Waiting
            /\ timer' = (PendingReread /\ budget > 0)
            /\ budget' = IF budget > 0 THEN budget - 1 ELSE 0
            /\ UNCHANGED view
  /\ UNCHANGED <<rust, registered, listener, failures, inbox, queued, arrived,
                 selected, load, mismatches, selections, startup>>

Fire ==
  /\ timer
  /\ timer' = FALSE
  /\ queued' = TRUE
  /\ UNCHANGED <<rust, registered, listener, failures, inbox, reading, view, arrived,
                 selected, load, phase, budget, mismatches, selections, startup>>

Environment == Commit \/ ArriveWithNotice \/ ArriveSilently \/ ListenRejects \/ Select
Frontend == ListenResolves \/ StartupRead \/ Relistens \/ Handle \/ StartRead \/ FinishMoved \/
            FinishMismatch \/ FinishRead \/ Fire

Next == Environment \/ Frontend

Finish == FinishMoved \/ FinishMismatch \/ FinishRead

\* The frontend's own steps are fair; the environment's are not.
Spec == Init /\ [][Next]_vars
          /\ WF_vars(ListenResolves) /\ WF_vars(Relistens) /\ WF_vars(Handle)
          /\ WF_vars(StartupRead)
          /\ WF_vars(StartRead) /\ WF_vars(Finish) /\ WF_vars(Fire)

SpecListenerUnfair == Init /\ [][Next]_vars /\ WF_vars(Handle) /\ WF_vars(StartupRead)
          /\ WF_vars(StartRead) /\ WF_vars(Finish) /\ WF_vars(Fire)

\* A read whose selection moved while it ran leaves the moved selection's
\* read owed: Select queued it, and only StartRead clears it.
MovedReadQueued == (reading # NoRead /\ reading.s # selected) => queued

(* What the user sees *)

\* The shell offers an action: a failed read, or (with the fix) a waiting
\* phase with nothing running or scheduled.
ActionShown ==
  \/ phase = "failed"
  \/ (PendingReread /\ phase \in Waiting /\ ~timer /\ reading = NoRead)

\* The selected session's status is final: shown, or naming an action.
Exposed == load = selected /\ reading = NoRead /\ ~queued /\
           ((phase = "loaded") \/ ActionShown)

\* Once commits stop, the rendered session equals the latest version, or
\* the shell names the action that would bring it there.
ViewConverges == <>[](Exposed /\ (view = rust \/ ActionShown))

\* A session pending, opening or loading eventually leaves that phase or
\* exposes an action.
NoStuckPending == [](~Exposed => <>Exposed)

====
