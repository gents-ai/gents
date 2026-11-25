---- MODULE GoalNoProgress ----
\* lean: none
\* crates: gents-automation
\* models: GoalProgress.step, GoalProgress.noProgressLimit
(***************************************************************************)
(* The no-progress pause against an Agent that never reports blocked.      *)
(*                                                                          *)
(* Actors and their atomic steps:                                           *)
(*   Agent   ends a goal turn that either repeats the previous turn's calls *)
(*           or makes a new call, and may start one background job with a   *)
(*           deadline (`AgentToolCall.deadline_at`).                        *)
(*   Clock   advances.                                                      *)
(*   Source  once the turn ends, reads the Goal by document id and the     *)
(*           turn's rows, takes Lean `GoalProgress.step` and writes the new *)
(*           record compare-and-set on the status it read, or pauses the    *)
(*           Goal; then publishes the next turn while the Goal is active.   *)
(* `WaitBounded` is the safeguard that a live job counts as a verified wait *)
(* only before its deadline; `PersistRecord` that the owner writes the     *)
(* streak on the Goal with each decision, so the next decision reads it.  *)
(***************************************************************************)
EXTENDS Naturals

CONSTANTS
  Limit,             \* GoalProgress.noProgressLimit
  MaxTime,
  MaxTurns,
  Deadline,          \* the background job's deadline
  WaitBounded,
  PersistRecord

VARIABLES
  status,     \* "active" | "paused"
  streak,
  turnEnded,  \* the Agent's turn ended and awaits the source
  repeated,   \* the ended turn repeated the previous turn's calls
  now,
  turns,
  idlePast    \* ghost: turns without progress published after the deadline

vars == <<status, streak, turnEnded, repeated, now, turns, idlePast>>

Init ==
  /\ status = "active"
  /\ streak = 0
  /\ turnEnded = FALSE
  /\ repeated = TRUE
  /\ now = 0
  /\ turns = 0
  /\ idlePast = 0

\* The Agent never makes progress: every turn repeats the previous one.
AgentTurn ==
  /\ status = "active"
  /\ ~turnEnded
  /\ turns < MaxTurns
  /\ turns' = turns + 1
  /\ turnEnded' = TRUE
  /\ repeated' = TRUE
  /\ UNCHANGED <<status, streak, now, idlePast>>

Tick ==
  /\ now < MaxTime
  /\ now' = now + 1
  /\ UNCHANGED <<status, streak, turnEnded, repeated, turns, idlePast>>

Waiting == IF WaitBounded THEN now < Deadline ELSE TRUE

SourceStep ==
  /\ turnEnded
  /\ status = "active"
  /\ turnEnded' = FALSE
  /\ UNCHANGED <<repeated, now, turns>>
  /\ IF ~repeated
       THEN /\ streak' = 0 /\ UNCHANGED <<status, idlePast>>
     ELSE IF Waiting
       THEN /\ idlePast' = IF now >= Deadline THEN idlePast + 1 ELSE idlePast
            /\ UNCHANGED <<status, streak>>
     ELSE LET next == IF PersistRecord THEN streak + 1 ELSE 1
          IN /\ streak' = next
             /\ idlePast' = idlePast + 1
             /\ status' = IF next >= Limit THEN "paused" ELSE "active"

Next == AgentTurn \/ Tick \/ SourceStep

Spec == Init /\ [][Next]_vars

TypeOK ==
  /\ status \in {"active", "paused"}
  /\ streak \in Nat
  /\ turnEnded \in BOOLEAN
  /\ now \in 0..MaxTime
  /\ turns \in 0..MaxTurns
  /\ idlePast \in Nat

\* Past the job's deadline, an Agent that makes no progress gets at most
\* `Limit` turns before its Goal pauses.
IdleBounded == idlePast <= Limit
====
