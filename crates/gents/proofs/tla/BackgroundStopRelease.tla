---- MODULE BackgroundStopRelease ----
\* lean: none
\* crates: gents-toolcalls, gents
\* traces: violates
\* models: Background.ProcessControl.signalledReply
(***************************************************************************)
(* `cancel_process` stopping a running background invocation its runtime   *)
(* executes, while the invocation's worker settles it                       *)
(* (`generated_background_budget_uses_accepted_dispatch`).                  *)
(*                                                                          *)
(* Actors and their atomic steps, from the code:                           *)
(*  - Canceller (`call::control::cancel_loaded_background_tool_call`):     *)
(*    loads the row; a terminal row is reported as it is. A running row's  *)
(*    live execution is signalled (its cancellation token), and the        *)
(*    canceller waits for the worker's release, bounded by                  *)
(*    `LIVE_STOP_RELEASE_WAIT` (`stop_execution`). On the release the stop *)
(*    settled (`Stopped`): its own compare from running loses to the       *)
(*    worker's terminal and re-reads it. When the bound ends the wait the  *)
(*    stop is `StillRunning` and the canceller re-reads the row.            *)
(*  - Worker (the spawned execution task): its tool either returns first   *)
(*    (a completion it commits) or the biased select observes the signal  *)
(*    and drops the tool future; it then commits the terminal (compare     *)
(*    from running) and only then releases (`remove_now`).                  *)
(* The reply after a re-read is the Lean decision `signalledReply`         *)
(* (`Decision = "lean"`), or the code before the fix, which reported every *)
(* terminal it re-read after a bounded wait as `already_terminal`          *)
(* (`Decision = "terminal"`).                                               *)
(*                                                                          *)
(* `Bound`: "wallclock" is the 10 s bound on a loaded host. Nothing bounds *)
(* how long the worker's commit waits to be scheduled (`WallClockBound`),  *)
(* so the bound may end the wait before any worker step. "unreached" is a  *)
(* bound at the suite's hang ceiling: the worker always releases first.    *)
(*                                                                          *)
(* Properties: `NoFalseAlreadyTerminal` (the reply names a terminal only   *)
(* when the invocation ended other than by this stop) and                   *)
(* `CancelledMeansCancelled` hold for the Lean decision under either bound *)
(* (`MCBackgroundStopRelease`, `MCBackgroundStopReleaseLoadedHost`).       *)
(* `ObserverAgrees` is the test's assertion: a tool that never returns is  *)
(* stopped with the reply `cancelled`. RED: `TerminalReread` (the CI       *)
(* failure: the worker committed its cancellation after the bound and the  *)
(* stop reported `already_terminal`) and `WallBound` (the same test with   *)
(* the product's bound reachable gets the honest `unverified`).            *)
(***************************************************************************)
EXTENDS Naturals

CONSTANTS
  Decision,   \* "lean" | "terminal"
  Bound,      \* "wallclock" | "unreached"
  Completes   \* the tool may return before it observes the signal

VARIABLES
  row,        \* "running" | "completed" | "cancelled"
  signalled,  \* the live execution's token is cancelled
  worker,     \* "busy" | "dropped" | "committed" | "released"
  canceller,  \* "idle" | "waiting" | "expired" | "done"
  reply       \* "none" | "cancelled" | "already_terminal" | "unverified"

vars == <<row, signalled, worker, canceller, reply>>

Terminal(state) == state # "running"

Reread(state) ==
  IF state = "running" THEN "unverified"
  ELSE IF Decision = "lean" /\ state = "cancelled" THEN "cancelled"
  ELSE "already_terminal"

Init ==
  /\ row = "running"
  /\ signalled = FALSE
  /\ worker = "busy"
  /\ canceller = "idle"
  /\ reply = "none"

Complete ==
  /\ Completes /\ worker = "busy"
  /\ row' = IF row = "running" THEN "completed" ELSE row
  /\ worker' = "committed"
  /\ UNCHANGED <<signalled, canceller, reply>>

Observe ==
  /\ worker = "busy" /\ signalled
  /\ worker' = "dropped"
  /\ UNCHANGED <<row, signalled, canceller, reply>>

CommitCancel ==
  /\ worker = "dropped"
  /\ row' = IF row = "running" THEN "cancelled" ELSE row
  /\ worker' = "committed"
  /\ UNCHANGED <<signalled, canceller, reply>>

Release ==
  /\ worker = "committed"
  /\ worker' = "released"
  /\ UNCHANGED <<row, signalled, canceller, reply>>

Load ==
  /\ canceller = "idle"
  /\ IF Terminal(row)
       THEN /\ reply' = "already_terminal" /\ canceller' = "done"
            /\ UNCHANGED signalled
       ELSE /\ signalled' = TRUE /\ canceller' = "waiting"
            /\ UNCHANGED reply
  /\ UNCHANGED <<row, worker>>

Released ==
  /\ canceller = "waiting" /\ worker = "released"
  /\ reply' = IF row = "cancelled" THEN "cancelled" ELSE "already_terminal"
  /\ canceller' = "done"
  /\ UNCHANGED <<row, signalled, worker>>

Expire ==
  /\ canceller = "waiting" /\ Bound = "wallclock" /\ worker # "released"
  /\ canceller' = "expired"
  /\ UNCHANGED <<row, signalled, worker, reply>>

RereadRow ==
  /\ canceller = "expired"
  /\ reply' = Reread(row)
  /\ canceller' = "done"
  /\ UNCHANGED <<row, signalled, worker>>

Next == Complete \/ Observe \/ CommitCancel \/ Release \/ Load \/ Released \/ Expire \/ RereadRow

Spec == Init /\ [][Next]_vars

TypeOK ==
  /\ row \in {"running", "completed", "cancelled"}
  /\ signalled \in BOOLEAN
  /\ worker \in {"busy", "dropped", "committed", "released"}
  /\ canceller \in {"idle", "waiting", "expired", "done"}
  /\ reply \in {"none", "cancelled", "already_terminal", "unverified"}

NoFalseAlreadyTerminal == reply = "already_terminal" => row = "completed"

CancelledMeansCancelled == reply = "cancelled" => row = "cancelled"

ObserverAgrees == (reply # "none" /\ ~Completes) => reply = "cancelled"
====
