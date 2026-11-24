---- MODULE ProcessLifecycle ----
\* lean: ProcessState.next
\* crates: gents, gents-protocol
(***************************************************************************)
(* One runtime process's durable process state against the actors that     *)
(* write it (`startup.rs`, `startup/lifecycle_coordinator.rs`).             *)
(*                                                                          *)
(* Actors and their atomic steps, from the code. Every durable write goes  *)
(* through the readiness publisher, which applies Lean `ProcessState.next` *)
(* to the state it last persisted and writes the result in the same step; *)
(* a refused or failed write leaves the durable state unchanged.           *)
(*  - Startup: Initialize persists `uninitialized`; recovery then runs and *)
(*    reports stuck requests, written as `startupRecover`. A signal during *)
(*    recovery ends the run instead.                                        *)
(*  - Readiness task (`become_ready`, under the coordinator lock): unless   *)
(*    shutdown has begun, writes `startupClean` or `recoveryComplete` and   *)
(*    opens the admission gate only once that write is persisted.           *)
(*  - Router: admits a request while the gate is open.                      *)
(*  - Teardown, on a signal or a fatal error (`shutdown_runtime`):          *)
(*    `begin_shutdown` marks closing, closes the gate and writes            *)
(*    `beginShutdown`; it joins every task that runs requests; then         *)
(*    `finish_shutdown` records the drain and writes `finishShutdown`. Its  *)
(*    deadline can cut it after it began, leaving admitted requests to the  *)
(*    next start's recovery.                                                *)
(*  - Finisher (`finish_run`), once the run is over: begins shutdown if it  *)
(*    has not begun, then writes `finishShutdown` unless an admitted        *)
(*    request was not drained.                                              *)
(* Any write can fail (an unwritable or unavailable store).                 *)
(*                                                                          *)
(* `GateOnlyWhenReady`: requests enter only while `ready` is durable.       *)
(* `NoAdmissionAfterClosing`: once shutdown begins the gate stays closed.  *)
(* `ShutdownMeansDrained`: `shutdown` is durable only with no request left *)
(* active.                                                                  *)
(*                                                                          *)
(* GREEN `MC_ProcessLifecycle_Green`. RED, one safeguard removed each:      *)
(* `Closing` (become_ready ignores the closing flag: a failed beginShutdown *)
(* write lets a late startup reopen the gate), `Guard` (the finisher writes *)
(* `shutdown` without the drain fact or Lean, after a cut teardown) and     *)
(* `Drain` (finish_shutdown before the join).                               *)
(***************************************************************************)
EXTENDS Naturals, LeanDecision_ProcessState

CONSTANTS IgnoreClosing, IgnoreGuard, FinishBeforeJoin

VARIABLES durable, stuck, phase, readiness, teardown, closing, admitted, drained,
  gate, active, finisher
vars == <<durable, stuck, phase, readiness, teardown, closing, admitted, drained,
  gate, active, finisher>>

Step(pre, action, zero, ok) ==
  LET written == next(pre, action, stuck, zero) IN
  IF ok /\ written # "none" THEN written ELSE pre

Zero == ~admitted \/ drained

Init ==
  /\ durable = "uninitialized"
  /\ stuck \in BOOLEAN
  /\ phase = "resolving"
  /\ readiness = "waiting"
  /\ teardown = "idle"
  /\ closing = FALSE /\ admitted = FALSE /\ drained = FALSE /\ gate = FALSE
  /\ active = 0
  /\ finisher = "waiting"

Resolve ==
  /\ phase = "resolving"
  /\ \E ok \in BOOLEAN:
       IF stuck
       THEN /\ durable' = Step(durable, "startup_recover", Zero, ok)
            /\ phase' = IF durable' = "recovering" THEN "started" ELSE "failed"
       ELSE /\ UNCHANGED durable
            /\ phase' = "started"
  /\ UNCHANGED <<stuck, readiness, teardown, closing, admitted, drained, gate, active,
       finisher>>

SignalDuringRecovery ==
  /\ phase = "resolving"
  /\ phase' = "failed"
  /\ UNCHANGED <<durable, stuck, readiness, teardown, closing, admitted, drained, gate,
       active, finisher>>

BecomeReady ==
  /\ phase = "started" /\ readiness = "waiting"
  /\ readiness' = "done"
  /\ IF closing /\ ~IgnoreClosing
     THEN UNCHANGED <<durable, admitted, gate>>
     ELSE \E ok \in BOOLEAN:
            LET action == IF stuck THEN "recovery_complete" ELSE "startup_clean" IN
            /\ durable' = Step(durable, action, Zero, ok)
            /\ IF durable' = "ready" /\ durable # "ready"
               THEN gate' = TRUE /\ admitted' = TRUE
               ELSE UNCHANGED <<admitted, gate>>
  /\ UNCHANGED <<stuck, phase, teardown, closing, drained, active, finisher>>

Admit ==
  /\ gate /\ active = 0
  /\ active' = 1
  /\ UNCHANGED <<durable, stuck, phase, readiness, teardown, closing, admitted, drained,
       gate, finisher>>

Complete ==
  /\ active = 1 /\ teardown \in {"idle", "began"}
  /\ active' = 0
  /\ UNCHANGED <<durable, stuck, phase, readiness, teardown, closing, admitted, drained,
       gate, finisher>>

BeginShutdown ==
  /\ phase = "started" /\ teardown = "idle"
  /\ teardown' = "began" /\ closing' = TRUE /\ gate' = FALSE
  /\ \E ok \in BOOLEAN: durable' = Step(durable, "begin_shutdown", Zero, ok)
  /\ UNCHANGED <<stuck, phase, readiness, admitted, drained, active, finisher>>

Join ==
  /\ teardown = "began"
  /\ teardown' = "joined" /\ active' = 0
  /\ UNCHANGED <<durable, stuck, phase, readiness, closing, admitted, drained, gate,
       finisher>>

FinishShutdown ==
  /\ \/ teardown = "joined"
     \/ FinishBeforeJoin /\ teardown = "began"
  /\ teardown' = "finished" /\ phase' = "ended" /\ drained' = TRUE
  /\ \E ok \in BOOLEAN: durable' = Step(durable, "finish_shutdown", TRUE, ok)
  /\ UNCHANGED <<stuck, readiness, closing, admitted, gate, active, finisher>>

Cut ==
  /\ teardown \in {"began", "joined"}
  /\ teardown' = "cut" /\ phase' = "ended"
  /\ UNCHANGED <<durable, stuck, readiness, closing, admitted, drained, gate, active,
       finisher>>

FinishRun ==
  /\ phase \in {"failed", "ended"} /\ finisher = "waiting"
  /\ finisher' = "done" /\ closing' = TRUE /\ gate' = FALSE
  /\ \E ok1, ok2 \in BOOLEAN:
       IF IgnoreGuard
       THEN durable' = IF ok2 THEN "shutdown" ELSE durable
       ELSE LET begun == IF closing THEN durable
                         ELSE Step(durable, "begin_shutdown", Zero, ok1)
            IN durable' =
                 IF (admitted /\ ~drained) \/ begun = "shutdown" THEN begun
                 ELSE Step(begun, "finish_shutdown", Zero, ok2)
  /\ UNCHANGED <<stuck, phase, readiness, teardown, admitted, drained, active>>

Next ==
  \/ Resolve \/ SignalDuringRecovery \/ BecomeReady \/ Admit \/ Complete
  \/ BeginShutdown \/ Join \/ FinishShutdown \/ Cut \/ FinishRun

Spec == Init /\ [][Next]_vars

GateOnlyWhenReady == gate => durable = "ready"
NoAdmissionAfterClosing == closing => ~gate
ShutdownMeansDrained == durable = "shutdown" => active = 0
====
