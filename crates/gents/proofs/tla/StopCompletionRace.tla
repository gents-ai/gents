---- MODULE StopCompletionRace ----
\* lean: none
\* crates: gents-toolcalls
\* models: Background.ProcessControl.firstTerminalIntent
(***************************************************************************)
(* A client stop racing a background invocation's completion               *)
(* (`client_cancel_races_completion_without_replacing_terminal_state`).   *)
(*                                                                          *)
(* Actors and their atomic steps, from the code:                           *)
(*  - Completion: commits the row `completed` with its canonical delivery  *)
(*    in one transaction (a compare from running), then drops the          *)
(*    execution's reservation.                                              *)
(*  - Stop (`cancel_loaded_background_tool_call`): loads the row; a       *)
(*    terminal row is reported as it is. For a running row it asks the    *)
(*    registry whether it still owns a live execution. Without one         *)
(*    (`cancel_unowned_background_tool`, the process not owned) it fails   *)
(*    the row as lost in one terminal transaction. That transaction reads *)
(*    the row and any delivery: a running row is failed; a row another     *)
(*    writer already ended is adopted when `Adopt` holds (the contender    *)
(*    lost its compare and reports the terminal), and is otherwise checked *)
(*    as a replay of its own plan, whose payload then differs: an error.   *)
(*    With a live execution the stop settles through the worker, which     *)
(*    this model leaves out: the completion still holds the reservation.   *)
(*                                                                          *)
(* `FirstTerminalWins` (Lean `firstTerminalIntent`): the first terminal is *)
(* never replaced. `StopSettles`: the stop never fails with an error.      *)
(* `ReplyFollowsRow`: a stop that reports a terminal names the row's.      *)
(*                                                                          *)
(* GREEN `MCStopCompletionRace`. RED: `NoAdopt` (the CI failure: the stop *)
(* loaded the row running, the completion committed and released, and the  *)
(* stop's failure checked the completion's delivery as its own replay) and *)
(* `NoCompare` (a terminal write that does not compare from running        *)
(* replaces the completion).                                               *)
(***************************************************************************)
EXTENDS Naturals

CONSTANTS
  Adopt,    \* the stop's terminal transaction adopts a competing terminal
  Compare   \* terminal writes compare from running

VARIABLES
  row,        \* "running" | "completed" | "failed"
  first,      \* the first terminal written: "none" | "completed" | "failed"
  reserved,   \* the completion still holds the execution's reservation
  completion, \* "running" | "committed" | "released"
  stop,       \* "idle" | "loaded" | "unowned" | "done"
  reply       \* "none" | "lost" | "already_terminal" | "error"

vars == <<row, first, reserved, completion, stop, reply>>

Init ==
  /\ row = "running"
  /\ first = "none"
  /\ reserved = TRUE
  /\ completion = "running"
  /\ stop = "idle"
  /\ reply = "none"

Write(state) ==
  /\ row' = IF Compare /\ row # "running" THEN row ELSE state
  /\ first' = IF first = "none" THEN state ELSE first

Complete ==
  /\ completion = "running"
  /\ IF Compare /\ row # "running"
       THEN UNCHANGED <<row, first>>
       ELSE Write("completed")
  /\ completion' = "committed"
  /\ UNCHANGED <<reserved, stop, reply>>

Release ==
  /\ completion = "committed"
  /\ reserved' = FALSE
  /\ completion' = "released"
  /\ UNCHANGED <<row, first, stop, reply>>

Load ==
  /\ stop = "idle"
  /\ IF row # "running"
       THEN stop' = "done" /\ reply' = "already_terminal"
       ELSE stop' = "loaded" /\ UNCHANGED reply
  /\ UNCHANGED <<row, first, reserved, completion>>

\* A stop that finds the execution still reserved settles through its
\* worker, outside this model.
CheckOwned ==
  /\ stop = "loaded"
  /\ IF reserved
       THEN stop' = "done" /\ reply' = "none"
       ELSE stop' = "unowned" /\ UNCHANGED reply
  /\ UNCHANGED <<row, first, reserved, completion>>

FailLost ==
  /\ stop = "unowned"
  /\ stop' = "done"
  /\ IF row = "running" \/ ~Compare
       THEN /\ Write("failed")
            /\ reply' = "lost"
       ELSE /\ reply' = IF Adopt THEN "already_terminal" ELSE "error"
            /\ UNCHANGED <<row, first>>
  /\ UNCHANGED <<reserved, completion>>

Next == Complete \/ Release \/ Load \/ CheckOwned \/ FailLost

Spec == Init /\ [][Next]_vars

TypeOK ==
  /\ row \in {"running", "completed", "failed"}
  /\ first \in {"none", "completed", "failed"}
  /\ reserved \in BOOLEAN
  /\ completion \in {"running", "committed", "released"}
  /\ stop \in {"idle", "loaded", "unowned", "done"}
  /\ reply \in {"none", "lost", "already_terminal", "error"}

FirstTerminalWins == first # "none" => row = first

StopSettles == reply # "error"

ReplyFollowsRow == reply = "already_terminal" => row = "completed"
====
