---- MODULE GoalSourceObservation ----
(***************************************************************************)
(* A test observing the goal owner's decision on a settled request (#3820). *)
(* The owner is `GoalSource` (`goal_source.rs`): its rescans run inside     *)
(* `next_fire`, so it makes progress only while its caller polls that       *)
(* future. On a rescan it reads the goal and the request evidence (`Scan`)  *)
(* and commits the Lean decision, here a status change such as `active` to  *)
(* `usage_limited` or a recorded readiness wait, without a continuation     *)
(* (`Commit`). `OwnerFires` stands for an owner that would also publish a   *)
(* continuation (`Fire`), which the decision forbids for these goals.       *)
(*                                                                          *)
(* The goal push-down tests assert: after the owner has processed the       *)
(* evidence, the goal holds the decided status and no continuation fired.   *)
(* Two observers:                                                           *)
(*  - `FixedWindow`: poll `next_fire` under a fixed wall-clock timeout,     *)
(*    then read the goal once. The timeout is unrelated to the owner's      *)
(*    progress, so it can expire, and drop the rescan, before `Commit`.     *)
(*  - otherwise `wait_for_goal_transition_as`: poll `next_fire` while       *)
(*    re-reading the goal on every native update, panic on a fire, return   *)
(*    once the decided status is read.                                      *)
(*                                                                          *)
(* `MCGoalSourceObservation`: the waiting observer and the decided owner;   *)
(* every property holds. `MCGoalSourceObservationFixedWindow`: the same     *)
(* owner under the fixed window fails the test, so the window asserts a     *)
(* scheduling budget, not the product. `MCGoalSourceObservationFiringOwner`: *)
(* the waiting observer still fails an owner that fires.                    *)
(***************************************************************************)
EXTENDS Naturals

CONSTANTS
  FixedWindow,  \* the observer reads once after a fixed timeout
  OwnerFires    \* the owner publishes a continuation for this goal

VARIABLES
  owner,     \* "idle" | "scanned" | "committed"
  status,    \* "active" | "decided"
  fired,     \* the owner published a continuation
  polling,   \* the observer is polling `next_fire`
  observed   \* "none" | "active" | "decided" | "fired"

vars == <<owner, status, fired, polling, observed>>

TypeOK ==
  /\ owner \in {"idle", "scanned", "committed"}
  /\ status \in {"active", "decided"}
  /\ fired \in BOOLEAN
  /\ polling \in BOOLEAN
  /\ observed \in {"none", "active", "decided", "fired"}

Init ==
  /\ owner = "idle"
  /\ status = "active"
  /\ fired = FALSE
  /\ polling = TRUE
  /\ observed = "none"

Scan ==
  /\ polling /\ owner = "idle"
  /\ owner' = "scanned"
  /\ UNCHANGED <<status, fired, polling, observed>>

Commit ==
  /\ polling /\ owner = "scanned"
  /\ owner' = "committed"
  /\ status' = "decided"
  /\ UNCHANGED <<fired, polling, observed>>

Fire ==
  /\ OwnerFires /\ polling /\ owner = "committed" /\ ~fired
  /\ fired' = TRUE
  /\ observed' = IF FixedWindow THEN observed ELSE "fired"
  /\ polling' = FixedWindow
  /\ UNCHANGED <<owner, status>>

(* The fixed timeout expires: the rescan is dropped and the goal read once. *)
Expire ==
  /\ FixedWindow /\ polling
  /\ polling' = FALSE
  /\ observed' = IF fired THEN "fired" ELSE status
  /\ UNCHANGED <<owner, status, fired>>

(* The waiting observer reads the goal after an update and stops at the *)
(* decided status.                                                       *)
Read ==
  /\ ~FixedWindow /\ polling /\ status = "decided"
  /\ polling' = FALSE
  /\ observed' = "decided"
  /\ UNCHANGED <<owner, status, fired>>

Next == Scan \/ Commit \/ Fire \/ Expire \/ Read

Spec == Init /\ [][Next]_vars /\ WF_vars(Scan) /\ WF_vars(Commit) /\ WF_vars(Read)
            /\ WF_vars(Fire) /\ WF_vars(Expire)

(* What the test asserts once it has observed. *)
TestPasses == observed # "none" => observed = "decided"

(* The waiting observer always reaches a verdict. *)
ObserverSettles == <>(observed # "none")

====
