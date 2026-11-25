---- MODULE PluginJobCrashObservation ----
\* lean: none
\* crates: gents
(***************************************************************************)
(* `plugin_job_survives_a_restart_mid_panel` kills a worker runtime after  *)
(* at least one job child is processing, then reboots on the retained      *)
(* store and asserts every child that was still pending at the crash runs  *)
(* to completion.                                                          *)
(*                                                                         *)
(* Actors. The worker runtime spawns three pending job children and its    *)
(* live request loop claims pending children into `processing` as long as  *)
(* the worker process is alive. Once any child is `processing`, the worker *)
(* sends a readiness frame over a Unix socket and blocks until `SIGKILL`.  *)
(* The parent test receives the frame, kills and reaps the worker, opens   *)
(* the retained store, and boots the recovery runtime. On reboot, startup  *)
(* recovery fails every `processing` child that died mid-inference and     *)
(* runs every `pending` child to `completed`.                              *)
(*                                                                         *)
(* `PendingEndsCompleted`: every child observed as `pending` ends          *)
(* `completed` after reboot.                                               *)
(*                                                                         *)
(* GREEN (`SampleAfterKill = TRUE`): the parent snapshots `AgentRequest`   *)
(* rows from the retained store after the worker is reaped and before the  *)
(* recovery runtime boots, so no writer can advance a child between the    *)
(* observation and the kill.                                               *)
(*                                                                         *)
(* RED (`SampleAfterKill = FALSE`): the worker snapshots `child_states`    *)
(* before sending the Unix socket frame (the CI failure of 2026-11-25 on   *)
(* PR #4239). Another child can transition from `pending` to `processing`  *)
(* during the IPC round-trip before `SIGKILL`, get recovered as `failed`,  *)
(* and break the stale-snapshot assertion. The product is exonerated.      *)
(***************************************************************************)

EXTENDS Naturals

CONSTANTS
  SampleAfterKill    \* TRUE: parent reads store after SIGKILL; FALSE: worker snapshots pre-IPC

Children == {1, 2, 3}

VARIABLES
  worker,    \* running | signalled | killed
  parent,    \* waiting | killing | rebooting | done
  store,     \* per child: pending | processing | completed | failed
  observed   \* per child: none | pending | processing

vars == <<worker, parent, store, observed>>

TypeOK ==
  /\ worker \in {"running", "signalled", "killed"}
  /\ parent \in {"waiting", "killing", "rebooting", "done"}
  /\ store \in [Children -> {"pending", "processing", "completed", "failed"}]
  /\ observed \in [Children -> {"none", "pending", "processing"}]

Init ==
  /\ worker = "running"
  /\ parent = "waiting"
  /\ store = [c \in Children |-> "pending"]
  /\ observed = [c \in Children |-> "none"]

\* While the worker process is alive (including after sending the socket
\* frame and before SIGKILL lands), its live agent loop may claim any
\* pending child into processing.
ClaimChild(c) ==
  /\ worker \in {"running", "signalled"}
  /\ store[c] = "pending"
  /\ store' = [store EXCEPT ![c] = "processing"]
  /\ UNCHANGED <<worker, parent, observed>>

\* Once at least one child is processing, the worker signals readiness to
\* the parent over the Unix socket. With `SampleAfterKill = FALSE`, it also
\* ships its pre-IPC snapshot of `store`.
SignalReady ==
  /\ worker = "running"
  /\ \E c \in Children : store[c] = "processing"
  /\ worker' = "signalled"
  /\ parent' = "killing"
  /\ observed' = IF SampleAfterKill THEN observed ELSE store
  /\ UNCHANGED <<store>>

\* Parent kills and reaps the worker. With `SampleAfterKill = TRUE`, it
\* opens the retained store with zero live writers and snapshots `store`
\* before booting the recovery runtime.
KillWorker ==
  /\ parent = "killing"
  /\ worker = "signalled"
  /\ worker' = "killed"
  /\ parent' = "rebooting"
  /\ observed' = IF SampleAfterKill THEN store ELSE observed
  /\ UNCHANGED <<store>>

\* Recovery runtime boots: crashed processing children are recovered as
\* failed, and still-pending children run to completion.
RebootRecover ==
  /\ parent = "rebooting"
  /\ worker = "killed"
  /\ parent' = "done"
  /\ store' = [c \in Children |->
       IF store[c] = "processing" THEN "failed"
       ELSE IF store[c] = "pending" THEN "completed"
       ELSE store[c]]
  /\ UNCHANGED <<worker, observed>>

Next ==
  \/ \E c \in Children : ClaimChild(c)
  \/ SignalReady
  \/ KillWorker
  \/ RebootRecover

Spec == Init /\ [][Next]_vars

PendingEndsCompleted ==
  parent = "done" =>
    \A c \in Children : observed[c] = "pending" => store[c] = "completed"

====
