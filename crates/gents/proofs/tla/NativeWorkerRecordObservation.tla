---- MODULE NativeWorkerRecordObservation ----
\* lean: none
\* crates: gents-host, gents-toolcalls, gents-session
(***************************************************************************)
(* A test observing two background native workers until each is running   *)
(* and has recorded its process ownership                                  *)
(* (`authenticated_process_control_stops_only_the_selected_native_worker`). *)
(*                                                                          *)
(* The owner. The turn's two `spawn_process` calls run one at a time        *)
(* (`run_batch`): each handler commits its parent row running and the       *)
(* spawned `bash` row pending (`Admit`), then moves the spawned row to      *)
(* running (`Run`, `start_running`) and hands the execution to a task of    *)
(* its own before the next call starts. That task spawns the child          *)
(* (`Spawn`, `run_managed_exec`) and only then renames the ownership record *)
(* into the runtime's record directory (`Record`, `record_spawned` ->       *)
(* `HostRecordStore::write`). The record is a host file, not a document:    *)
(* no document commit follows it. The turn's completion (`Tail`) commits    *)
(* once both rows run, in any order with the records.                       *)
(*                                                                          *)
(* The host schedules every runnable step within `MaxDelay` ticks (the      *)
(* premise, as in `WallClockBound`) and lets time pass while nothing is     *)
(* runnable. The test asserts `Reached`: both spawned rows running and both *)
(* records present.                                                         *)
(*                                                                          *)
(* The observers. With `Window > 0` the test polls under a fixed timeout    *)
(* and fails at tick `Window` (the observer on main: 20 s). With           *)
(* `Window = 0` it re-reads after each notification it subscribed to and   *)
(* fails once none arrives for `Ceiling` ticks (`STALL_CEILING`).           *)
(* `WatchRecords`: it is also notified by changes in the record directory,  *)
(* not only by `AgentToolCall` commits. `SubscribeFirst`: it subscribes     *)
(* before its first read rather than after a read that came up short.      *)
(*                                                                          *)
(* GREEN `MCNativeWorkerRecordObservation`: the waiting observer watching   *)
(* both owners, subscribed first, passes under every schedule and always    *)
(* settles. RED: `FixedWindow` (the CI failure: a correct, progressing      *)
(* owner on a host slower than the window); `DocumentsOnly` (the last       *)
(* record lands after the last document commit, so the waiter sees no      *)
(* change and fails a finished owner at the ceiling); `SubscribeAfterRead`  *)
(* (a change between a short read and the subscription is never           *)
(* signalled); `StalledOwner` (an owner that never records is still         *)
(* reported, so dropping the window hides no lost record).                 *)
(*                                                                          *)
(* The product side holds in every configuration: under the scheduling     *)
(* premise every worker runs and records, so the observer is the side that *)
(* can be wrong.                                                            *)
(***************************************************************************)
EXTENDS Naturals

CONSTANTS
  MaxDelay,        \* ticks a runnable step may wait to be scheduled
  Ceiling,         \* ticks a waiting observer may go without a notification
  Window,          \* 0: the observer waits on notifications; n: it fails at tick n
  WatchRecords,    \* the record directory notifies the observer
  SubscribeFirst,  \* the observer subscribes before its first read
  OwnerStalls      \* the owner never records the second worker's process

Workers == {1, 2}
Stages == <<"none", "admitted", "running", "spawned", "recorded">>
Rank(s) == CHOOSE i \in 1..5 : Stages[i] = s

VARIABLES
  stage,       \* per worker: none | admitted | running | spawned | recorded
  tail,        \* the turn's completion committed
  obs,         \* "read" | "wait" | "pass" | "fail"
  subscribed,  \* the observer's notifications are open
  pending,     \* a notification arrived since the observer's last read
  clock,       \* ticks since the scenario started (saturating at Cap)
  stall,       \* ticks since the last step
  idle         \* ticks the observer has waited without a notification

vars == <<stage, tail, obs, subscribed, pending, clock, stall, idle>>

Cap == Ceiling + Window + 1

TypeOK ==
  /\ stage \in [Workers -> {Stages[i] : i \in 1..5}]
  /\ tail \in BOOLEAN
  /\ obs \in {"read", "wait", "pass", "fail"}
  /\ subscribed \in BOOLEAN
  /\ pending \in BOOLEAN
  /\ clock \in 0..Cap
  /\ stall \in 0..(Ceiling + MaxDelay)
  /\ idle \in 0..Ceiling

Reached ==
  /\ \A w \in Workers : stage[w] = "recorded"

Running == obs \in {"read", "wait"}

Init ==
  /\ stage = [w \in Workers |-> "none"]
  /\ tail = FALSE
  /\ obs = "read"
  /\ subscribed = SubscribeFirst
  /\ pending = FALSE
  /\ clock = 0
  /\ stall = 0
  /\ idle = 0

(* A document commit notifies a subscribed observer; a record does only   *)
(* when the observer watches the record directory. A notification restarts *)
(* the ceiling, which bounds each wait for the next one.                  *)
Notifies(document) == subscribed /\ (document \/ WatchRecords)
Notify(document) ==
  /\ pending' = (pending \/ Notifies(document))
  /\ idle' = IF Notifies(document) THEN 0 ELSE idle

Step(w, from, to) ==
  /\ stage[w] = from
  /\ stage' = [stage EXCEPT ![w] = to]
  /\ stall' = 0

(* `run_batch`: the second call starts only after the first handler has  *)
(* moved its spawned row to running.                                      *)
Admit(w) ==
  /\ Running
  /\ \A v \in Workers : v < w => Rank(stage[v]) >= Rank("running")
  /\ Step(w, "none", "admitted")
  /\ Notify(TRUE)
  /\ UNCHANGED <<tail, obs, subscribed, clock>>

Run(w) ==
  /\ Running
  /\ Step(w, "admitted", "running")
  /\ Notify(TRUE)
  /\ UNCHANGED <<tail, obs, subscribed, clock>>

Spawn(w) ==
  /\ Running
  /\ Step(w, "running", "spawned")
  /\ UNCHANGED <<tail, obs, subscribed, pending, clock, idle>>

Record(w) ==
  /\ Running
  /\ ~(OwnerStalls /\ w = 2)
  /\ Step(w, "spawned", "recorded")
  /\ Notify(FALSE)
  /\ UNCHANGED <<tail, obs, subscribed, clock>>

Tail ==
  /\ Running /\ ~tail
  /\ \A w \in Workers : Rank(stage[w]) >= Rank("running")
  /\ tail' = TRUE
  /\ stall' = 0
  /\ Notify(TRUE)
  /\ UNCHANGED <<stage, obs, subscribed, clock>>

OwnerStep ==
  \/ \E w \in Workers : Admit(w) \/ Run(w) \/ Spawn(w) \/ Record(w)
  \/ Tail

(* One read of the rows and the record directory. *)
Read ==
  /\ obs = "read"
  /\ obs' = IF Reached THEN "pass" ELSE "wait"
  /\ pending' = FALSE
  /\ stall' = 0
  /\ idle' = 0
  /\ UNCHANGED <<stage, tail, subscribed, clock>>

(* The observer that subscribes late opens its notifications only after a *)
(* read came up short; what changed in between is not signalled.          *)
Subscribe ==
  /\ obs = "wait" /\ ~subscribed /\ Window = 0
  /\ subscribed' = TRUE
  /\ stall' = 0
  /\ UNCHANGED <<stage, tail, obs, pending, clock, idle>>

(* The waiting observer reads again after a notification; the polling    *)
(* observer reads again whenever it likes.                               *)
Wake ==
  /\ obs = "wait"
  /\ (Window > 0 \/ (subscribed /\ pending))
  /\ obs' = "read"
  /\ stall' = 0
  /\ UNCHANGED <<stage, tail, subscribed, pending, clock, idle>>

Stalled ==
  /\ obs = "wait" /\ Window = 0 /\ idle >= Ceiling
  /\ obs' = "fail"
  /\ UNCHANGED <<stage, tail, subscribed, pending, clock, stall, idle>>

Expire ==
  /\ Running /\ Window > 0 /\ clock >= Window
  /\ obs' = IF Reached THEN "pass" ELSE "fail"
  /\ UNCHANGED <<stage, tail, subscribed, pending, clock, stall, idle>>

OwnerRunnable == ENABLED OwnerStep
ObserverRunnable ==
  \/ obs = "read"
  \/ (obs = "wait" /\ Window = 0 /\ (~subscribed \/ pending))

Tick ==
  /\ Running
  /\ IF OwnerRunnable \/ ObserverRunnable THEN stall < MaxDelay ELSE TRUE
  /\ (obs = "wait" /\ Window = 0) => idle < Ceiling
  /\ clock' = IF Window > 0 /\ clock < Cap THEN clock + 1 ELSE clock
  /\ stall' = IF stall < Ceiling + MaxDelay THEN stall + 1 ELSE stall
  /\ idle' = IF obs = "wait" /\ Window = 0 THEN idle + 1 ELSE idle
  /\ UNCHANGED <<stage, tail, obs, subscribed, pending>>

Next == OwnerStep \/ Read \/ Subscribe \/ Wake \/ Stalled \/ Expire \/ Tick

Spec ==
  /\ Init /\ [][Next]_vars
  /\ WF_vars(OwnerStep) /\ WF_vars(Read) /\ WF_vars(Subscribe) /\ WF_vars(Wake)
  /\ WF_vars(Stalled) /\ WF_vars(Expire) /\ WF_vars(Tick)

TestPasses == obs # "fail"

ObserverSettles == <>(obs \in {"pass", "fail"})

====
