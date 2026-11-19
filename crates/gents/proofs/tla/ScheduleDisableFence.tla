---- MODULE ScheduleDisableFence ----
\* lean: none
\* crates: gents-automation, gents
(***************************************************************************)
(* A User disables a schedule Trigger while the runtime's schedule source   *)
(* is firing it. Actors and their atomic steps, from the code:              *)
(*                                                                          *)
(* Reconciler (`runtime/agent/reconcile.rs`): publishes the new generation's *)
(* readiness source, then sends the snapshot on the active watch.           *)
(*                                                                          *)
(* Router (`runtime/agent/runtime/router.rs`): borrows the watch and, once  *)
(* the readiness source names the same generation, writes                   *)
(* `router_generation`. The observer's "generation active" is this write.   *)
(*                                                                          *)
(* Schedule source (`triggers/engine/schedule_source.rs`): one sequential   *)
(* loop. A tick borrows the watch, observes Schedule visibility, loads the  *)
(* Trigger cursors (seeding a missing one: an interval cadence seeds `now`, *)
(* so it is due at once; a cron cadence seeds its next run), and returns    *)
(* the first due Trigger of its snapshot in an unspecified order.           *)
(*                                                                          *)
(* Engine (`triggers/engine/{mod,dispatch}.rs`): awaits each intent's       *)
(* dispatch before the source's next tick. Dispatch borrows the watch again *)
(* and admits only a Trigger enabled in that snapshot (Lean                 *)
(* `dispatchEnabledForSchedule`, theorem `T1_enabled_gate`), else skips;    *)
(* an admitted fire commits its `AgentRequest`. Either way `on_result`      *)
(* spawns the Trigger runtime write (advanced cursor, `fire_count`), which  *)
(* lands at any later point.                                                *)
(*                                                                          *)
(* Observer (`schedule_insert_and_trigger_disable_bump_active_generation`): *)
(* starts with generation 1 active (alpha and the witness beta enabled),    *)
(* disables alpha, waits for `router_generation` 2, writes alpha's cursor   *)
(* due and reads the baseline count of alpha's requests.                    *)
(*                                                                          *)
(* Safeguards judged here:                                                  *)
(*  - `Recheck` (product): dispatch borrows the watch again instead of      *)
(*    trusting the tick's snapshot.                                         *)
(*  - `AckAfterWatch` (product): the router acknowledges only the           *)
(*    generation it borrowed from the watch.                                *)
(*  - `WaitSeeded` (observer): before disabling, wait for alpha's seeded    *)
(*    fire to be recorded (`fire_count` 1) and beta's cursor to be seeded,  *)
(*    so no fire admitted under generation 1 is still in flight and no      *)
(*    seed write races the observer's cursor writes.                        *)
(*  - `Witness` (observer): after the baseline, write beta's cursor due and *)
(*    judge once beta's request commits. The source is sequential, so every *)
(*    alpha intent loaded from the due cursor was dispatched before it.     *)
(*                                                                          *)
(* Premise: alpha's interval is far longer than the test, so its seeded     *)
(* fire's advanced cursor is not due again while the test runs.             *)
(*                                                                          *)
(* The product's guarantee is `AdmissionFence` (no admission of alpha once  *)
(* generation 2 is acknowledged) and `NoDueFire` (the cursor written after  *)
(* the acknowledgement never fires). A fire admitted under generation 1     *)
(* may still commit after the acknowledgement, as a retired slot's request  *)
(* finishes under its old generation; the observer before the fix counted   *)
(* it (`OldObserver`, the CI failure).                                      *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets

CONSTANTS
  Recheck,        \* dispatch re-borrows the active snapshot
  AckAfterWatch,  \* router acknowledges only a generation it borrowed
  WaitSeeded,     \* observer waits for the seeded fire before disabling
  Witness         \* observer judges after the witness Trigger fires

Items == {"alpha", "beta"}
Cursors == {"none", "seed", "due", "future"}
Due == {"seed", "due"}
Idle == [pc |-> "idle", snap |-> 0, item |-> "none", key |-> "none"]

VARIABLES
  enabled,      \* alpha's desired state
  readinessGen, \* readiness source generation
  watchGen,     \* snapshot on the active watch
  routerAck,    \* router_generation
  cursor,       \* Trigger.next_run_at per item
  reqs,         \* committed AgentRequest fire keys per item
  alphaFires,   \* alpha's recorded fire_count
  src,          \* schedule source and its awaited dispatch
  pending,      \* spawned on_result writes not yet landed
  obs,          \* observer
  baseline,     \* observer's baseline count of alpha's requests
  lateAdmit     \* alpha admitted after generation 2 was acknowledged

vars == <<enabled, readinessGen, watchGen, routerAck, cursor, reqs, alphaFires,
          src, pending, obs, baseline, lateAdmit>>

Has(gen, item) == item = "beta" \/ (item = "alpha" /\ gen = 1)

Init ==
  /\ enabled = TRUE
  /\ readinessGen = 1
  /\ watchGen = 1
  /\ routerAck = 1
  /\ cursor = [i \in Items |-> "none"]
  /\ reqs = [i \in Items |-> {}]
  /\ alphaFires = 0
  /\ src = Idle
  /\ pending = {}
  /\ obs = "start"
  /\ baseline = 0
  /\ lateAdmit = FALSE

\* Reconciler
Publish ==
  /\ ~enabled /\ readinessGen = 1
  /\ readinessGen' = 2
  /\ UNCHANGED <<enabled, watchGen, routerAck, cursor, reqs, alphaFires, src, pending, obs, baseline, lateAdmit>>

Send ==
  /\ readinessGen = 2 /\ watchGen = 1
  /\ watchGen' = 2
  /\ UNCHANGED <<enabled, readinessGen, routerAck, cursor, reqs, alphaFires, src, pending, obs, baseline, lateAdmit>>

\* Router
Ack ==
  /\ IF AckAfterWatch
       THEN watchGen = readinessGen /\ routerAck < watchGen /\ routerAck' = watchGen
       ELSE routerAck < readinessGen /\ routerAck' = readinessGen
  /\ UNCHANGED <<enabled, readinessGen, watchGen, cursor, reqs, alphaFires, src, pending, obs, baseline, lateAdmit>>

\* Schedule source: a tick borrows the watch ...
TickBorrow ==
  /\ src.pc = "idle"
  /\ src' = [pc |-> "borrowed", snap |-> watchGen, item |-> "none", key |-> "none"]
  /\ UNCHANGED <<enabled, readinessGen, watchGen, routerAck, cursor, reqs, alphaFires, pending, obs, baseline, lateAdmit>>

Seeded(snap, i) ==
  IF Has(snap, i) /\ cursor[i] = "none"
    THEN IF i = "alpha" THEN "seed" ELSE "future"
    ELSE cursor[i]

\* ... then, after the visibility read, loads (and seeds) its cursors and
\* returns one due Trigger of its snapshot.
TickLoad ==
  /\ src.pc = "borrowed"
  /\ LET next == [i \in Items |-> Seeded(src.snap, i)]
         due == {i \in Items : Has(src.snap, i) /\ next[i] \in Due}
     IN /\ cursor' = next
        /\ IF due = {}
             THEN src' = Idle
             ELSE \E i \in due :
                    src' = [pc |-> "intent", snap |-> src.snap, item |-> i, key |-> next[i]]
  /\ UNCHANGED <<enabled, readinessGen, watchGen, routerAck, reqs, alphaFires, pending, obs, baseline, lateAdmit>>

\* Engine dispatch: the admission gate.
Dispatch ==
  /\ src.pc = "intent"
  /\ LET gate == IF Recheck THEN watchGen ELSE src.snap
     IN IF Has(gate, src.item)
          THEN /\ src' = [src EXCEPT !.pc = "admitted"]
               /\ lateAdmit' = (lateAdmit \/ (src.item = "alpha" /\ routerAck = 2))
               /\ UNCHANGED pending
          ELSE /\ src' = Idle
               /\ pending' = pending \cup {[item |-> src.item, fired |-> FALSE]}
               /\ UNCHANGED lateAdmit
  /\ UNCHANGED <<enabled, readinessGen, watchGen, routerAck, cursor, reqs, alphaFires, obs, baseline>>

Materialize ==
  /\ src.pc = "admitted"
  /\ reqs' = [reqs EXCEPT ![src.item] = @ \cup {src.key}]
  /\ pending' = pending \cup {[item |-> src.item, fired |-> TRUE]}
  /\ src' = Idle
  /\ UNCHANGED <<enabled, readinessGen, watchGen, routerAck, cursor, alphaFires, obs, baseline, lateAdmit>>

OnResult ==
  \E p \in pending :
    /\ cursor' = [cursor EXCEPT ![p.item] = "future"]
    /\ alphaFires' = IF p.item = "alpha" /\ p.fired THEN alphaFires + 1 ELSE alphaFires
    /\ pending' = pending \ {p}
    /\ UNCHANGED <<enabled, readinessGen, watchGen, routerAck, reqs, src, obs, baseline, lateAdmit>>

\* Observer
ObsDisable ==
  /\ obs = "start"
  /\ WaitSeeded => (alphaFires >= 1 /\ cursor["beta"] # "none")
  /\ enabled' = FALSE
  /\ obs' = "awaitAck"
  /\ UNCHANGED <<readinessGen, watchGen, routerAck, cursor, reqs, alphaFires, src, pending, baseline, lateAdmit>>

ObsCursor ==
  /\ obs = "awaitAck" /\ routerAck = 2
  /\ cursor' = [cursor EXCEPT !["alpha"] = "due"]
  /\ obs' = "baseline"
  /\ UNCHANGED <<enabled, readinessGen, watchGen, routerAck, reqs, alphaFires, src, pending, baseline, lateAdmit>>

ObsBaseline ==
  /\ obs = "baseline"
  /\ baseline' = Cardinality(reqs["alpha"])
  /\ obs' = IF Witness THEN "setWitness" ELSE "checked"
  /\ UNCHANGED <<enabled, readinessGen, watchGen, routerAck, cursor, reqs, alphaFires, src, pending, lateAdmit>>

ObsWitnessCursor ==
  /\ obs = "setWitness"
  /\ cursor' = [cursor EXCEPT !["beta"] = "due"]
  /\ obs' = "awaitWitness"
  /\ UNCHANGED <<enabled, readinessGen, watchGen, routerAck, reqs, alphaFires, src, pending, baseline, lateAdmit>>

ObsWitness ==
  /\ obs = "awaitWitness" /\ reqs["beta"] # {}
  /\ obs' = "checked"
  /\ UNCHANGED <<enabled, readinessGen, watchGen, routerAck, cursor, reqs, alphaFires, src, pending, baseline, lateAdmit>>

Next ==
  \/ Publish \/ Send \/ Ack
  \/ TickBorrow \/ TickLoad \/ Dispatch \/ Materialize \/ OnResult
  \/ ObsDisable \/ ObsCursor \/ ObsBaseline \/ ObsWitnessCursor \/ ObsWitness

Spec == Init /\ [][Next]_vars

TypeOK ==
  /\ enabled \in BOOLEAN
  /\ readinessGen \in {1, 2} /\ watchGen \in {1, 2} /\ routerAck \in {1, 2}
  /\ cursor \in [Items -> Cursors]
  /\ reqs \in [Items -> SUBSET Cursors]
  /\ alphaFires \in 0..3
  /\ src.pc \in {"idle", "borrowed", "intent", "admitted"}
  /\ obs \in {"start", "awaitAck", "baseline", "setWitness", "awaitWitness", "checked"}
  /\ baseline \in 0..3
  /\ lateAdmit \in BOOLEAN

\* Product: nothing admits alpha once generation 2 is acknowledged.
AdmissionFence == ~lateAdmit

\* Product: the cursor written after the acknowledgement never fires.
NoDueFire == "due" \notin reqs["alpha"]

\* The test's assertion, held from its verdict on.
TestAssertion == obs = "checked" => Cardinality(reqs["alpha"]) = baseline

\* The verdict is final: no tick of generation 1 and no alpha intent remains.
AlphaPossible == src.pc # "idle" /\ (src.snap = 1 \/ src.item = "alpha")
Settled == obs = "checked" => ~AlphaPossible

====
