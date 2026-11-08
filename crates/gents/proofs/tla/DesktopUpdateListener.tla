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
(*   FullAfterListen  run() reads in full once the listener resolves,      *)
(*                    instead of reading the session before registration. *)
(*   Relisten         a rejected registration is retried (bounded backoff;*)
(*                    the delay is not modeled).                           *)
(*   PendingReread    a waiting phase schedules bounded re-reads and,      *)
(*                    once they are spent, shows the retry action.         *)
(*   MismatchExit     a reply that cannot be shown fails visibly; a read   *)
(*                    whose selection moved leaves a read queued.          *)
(***************************************************************************)
EXTENDS Naturals

CONSTANTS
  MaxVersion,       \* committed store versions after startup
  ListenFailures,   \* registration rejections the environment may cause
  Mismatches,       \* replies for another target the bridge may return
  Selections,       \* selection changes the user may make
  RereadBudget,     \* re-reads a waiting phase is given
  FullAfterListen, Relisten, PendingReread, MismatchExit

Sessions == {1, 2}
NoRead == [v |-> 0, s |-> 0]
Waiting == {"pending"}

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
  phase,        \* "idle" | "loading" | "pending" | "loaded" | "failed"
  timer,        \* a waiting phase's re-read is scheduled
  budget,
  mismatches,
  selections

vars == <<rust, registered, listener, failures, inbox, queued, reading, view,
          arrived, selected, load, phase, timer, budget, mismatches, selections>>

TypeOK ==
  /\ rust \in 0..MaxVersion
  /\ registered \in BOOLEAN
  /\ listener \in {"listening", "ready", "failed"}
  /\ inbox \in BOOLEAN /\ queued \in BOOLEAN /\ timer \in BOOLEAN
  /\ view \in 0..MaxVersion
  /\ selected \in Sessions /\ load \in Sessions \cup {0}
  /\ phase \in {"idle", "loading", "pending", "loaded", "failed"}

Init ==
  /\ rust = 0
  /\ registered = FALSE
  /\ listener = "listening"
  /\ failures = ListenFailures
  /\ inbox = FALSE
  \* before the fix, run() requested 'session' before the listener resolved
  /\ queued = ~FullAfterListen
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
                 selected, load, phase, timer, budget, mismatches, selections>>

\* The selected request reaches its owner with a store notice ...
ArriveWithNotice ==
  /\ ~arrived /\ rust < MaxVersion
  /\ arrived' = TRUE
  /\ rust' = rust + 1
  /\ inbox' = (inbox \/ registered)
  /\ UNCHANGED <<registered, listener, failures, queued, reading, view,
                 selected, load, phase, timer, budget, mismatches, selections>>

\* ... or with none: a remote runtime has no event source here.
ArriveSilently ==
  /\ ~arrived
  /\ arrived' = TRUE
  /\ UNCHANGED <<rust, registered, listener, failures, inbox, queued, reading, view,
                 selected, load, phase, timer, budget, mismatches, selections>>

ListenRejects ==
  /\ listener = "listening" /\ failures > 0
  /\ listener' = "failed"
  /\ failures' = failures - 1
  /\ UNCHANGED <<rust, registered, inbox, queued, reading, view, arrived,
                 selected, load, phase, timer, budget, mismatches, selections>>

\* The user selects another session; the selection subscription requests it.
Select ==
  /\ selections > 0
  /\ selections' = selections - 1
  /\ selected' \in Sessions \ {selected}
  /\ queued' = TRUE
  /\ timer' = FALSE
  /\ budget' = RereadBudget
  /\ UNCHANGED <<rust, registered, listener, failures, inbox, reading, view, arrived,
                 load, phase, mismatches>>

(* Frontend *)

ListenResolves ==
  /\ listener = "listening"
  /\ listener' = "ready"
  /\ registered' = TRUE
  /\ queued' = (queued \/ FullAfterListen)
  /\ UNCHANGED <<rust, failures, inbox, reading, view, arrived, selected, load,
                 phase, timer, budget, mismatches, selections>>

Relistens ==
  /\ Relisten /\ listener = "failed"
  /\ listener' = "listening"
  /\ UNCHANGED <<rust, registered, failures, inbox, queued, reading, view, arrived,
                 selected, load, phase, timer, budget, mismatches, selections>>

\* A read the re-read timer did not start begins a new re-read streak.
Handle ==
  /\ inbox
  /\ inbox' = FALSE
  /\ queued' = TRUE
  /\ budget' = RereadBudget
  /\ UNCHANGED <<rust, registered, listener, failures, reading, view, arrived,
                 selected, load, phase, timer, mismatches, selections>>

StartRead ==
  /\ queued /\ reading = NoRead
  /\ queued' = FALSE
  /\ reading' = [v |-> rust, s |-> selected]
  /\ phase' = IF load = selected /\ phase \in Waiting THEN phase ELSE "loading"
  /\ load' = selected
  /\ timer' = FALSE
  /\ UNCHANGED <<rust, registered, listener, failures, inbox, view, arrived,
                 selected, budget, mismatches, selections>>

FinishMoved ==
  /\ reading # NoRead /\ reading.s # selected
  /\ reading' = NoRead
  /\ queued' = (queued \/ MismatchExit)
  /\ UNCHANGED <<rust, registered, listener, failures, inbox, view, arrived,
                 selected, load, phase, timer, budget, mismatches, selections>>

FinishMismatch ==
  /\ reading # NoRead /\ reading.s = selected /\ mismatches > 0
  /\ mismatches' = mismatches - 1
  /\ reading' = NoRead
  /\ phase' = IF MismatchExit THEN "failed" ELSE phase
  /\ UNCHANGED <<rust, registered, listener, failures, inbox, queued, view, arrived,
                 selected, load, timer, budget, selections>>

FinishRead ==
  /\ reading # NoRead /\ reading.s = selected
  /\ reading' = NoRead
  /\ IF arrived
       THEN /\ view' = reading.v
            /\ phase' = "loaded"
            /\ budget' = RereadBudget
            /\ timer' = FALSE
       ELSE /\ phase' = "pending"
            /\ timer' = (PendingReread /\ budget > 0)
            /\ budget' = IF budget > 0 THEN budget - 1 ELSE 0
            /\ UNCHANGED view
  /\ UNCHANGED <<rust, registered, listener, failures, inbox, queued, arrived,
                 selected, load, mismatches, selections>>

Fire ==
  /\ timer
  /\ timer' = FALSE
  /\ queued' = TRUE
  /\ UNCHANGED <<rust, registered, listener, failures, inbox, reading, view, arrived,
                 selected, load, phase, budget, mismatches, selections>>

Environment == Commit \/ ArriveWithNotice \/ ArriveSilently \/ ListenRejects \/ Select
Frontend == ListenResolves \/ Relistens \/ Handle \/ StartRead \/ FinishMoved \/
            FinishMismatch \/ FinishRead \/ Fire

Next == Environment \/ Frontend

Finish == FinishMoved \/ FinishMismatch \/ FinishRead

\* The frontend's own steps are fair; the environment's are not.
Spec == Init /\ [][Next]_vars
          /\ WF_vars(ListenResolves) /\ WF_vars(Relistens) /\ WF_vars(Handle)
          /\ WF_vars(StartRead) /\ WF_vars(Finish) /\ WF_vars(Fire)

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
