---- MODULE DiagnosedReadiness ----
\* lean: none
\* crates: gents-cli, gents-host, gents
\* models: ClientAgentReadiness.project
(***************************************************************************)
(* A test waits for a Tools edit to demote an Agent, then runs             *)
(* `gents runtime diagnose` and expects the Agent among its unavailable    *)
(* agents (`remote_pack_install_update_and_diagnose_keep_plugin_failures_  *)
(* isolated`).                                                              *)
(*                                                                          *)
(* Actors and their atomic steps, from the code:                           *)
(*  - Reconciler (`runtime/agent/reconcile.rs`, through the readiness      *)
(*    publisher's `PublishSource`): commits the node's readiness row with  *)
(*    the new active generation and its agent entries, the edited Agent    *)
(*    unavailable. The router generation in that row is still the old one. *)
(*  - Router (`runtime/agent/runtime/router.rs`, `SetRouterGeneration`):   *)
(*    once the readiness source names the generation it borrowed, commits  *)
(*    the row's router generation.                                         *)
(*  - Observer: re-reads on each readiness change until its predicate      *)
(*    holds.                                                                *)
(*  - Diagnose: reads the node's newest readiness row and projects it with *)
(*    the owner (`project_node_readiness_summary`, Lean                     *)
(*    `ClientAgentReadiness.project`): a row whose router generation is not *)
(*    its active generation is unknown (`router_generation_stale`) and     *)
(*    lists no unavailable agents.                                         *)
(*                                                                          *)
(* `Observer`: "projected" waits until the owner's projection of the row   *)
(* diagnose reads lists the Agent unavailable; "decoded" (the test before  *)
(* the fix) waits until any decoded row lists it.                          *)
(*                                                                          *)
(* GREEN `MCDiagnosedReadiness`. RED: `Decoded` (the CI failure: diagnose  *)
(* reads the row between the two commits) and `NoRouterAck` (a router that *)
(* never acknowledges leaves the projected observer waiting forever).      *)
(***************************************************************************)
EXTENDS Naturals

CONSTANTS
  Observer,   \* "projected" | "decoded"
  RouterAcks  \* the router acknowledges the generation the reconciler published

VARIABLES
  active,     \* the row's active generation
  router,     \* the row's router generation
  demoted,    \* the row lists the edited Agent unavailable
  observer,   \* "waiting" | "settled"
  diagnosed   \* "none" | "shown" | "missing"

vars == <<active, router, demoted, observer, diagnosed>>

Projected == active > 0 /\ router = active

Shown == Projected /\ demoted

Init ==
  /\ active = 1
  /\ router = 1
  /\ demoted = FALSE
  /\ observer = "waiting"
  /\ diagnosed = "none"

PublishSource ==
  /\ active = 1
  /\ active' = 2
  /\ demoted' = TRUE
  /\ UNCHANGED <<router, observer, diagnosed>>

AckRouter ==
  /\ RouterAcks /\ router < active
  /\ router' = active
  /\ UNCHANGED <<active, demoted, observer, diagnosed>>

Waited == IF Observer = "projected" THEN Shown ELSE demoted

Settle ==
  /\ observer = "waiting" /\ Waited
  /\ observer' = "settled"
  /\ UNCHANGED <<active, router, demoted, diagnosed>>

Diagnose ==
  /\ observer = "settled" /\ diagnosed = "none"
  /\ diagnosed' = IF Shown THEN "shown" ELSE "missing"
  /\ UNCHANGED <<active, router, demoted, observer>>

Next == PublishSource \/ AckRouter \/ Settle \/ Diagnose

Spec == Init /\ [][Next]_vars /\ WF_vars(Next)

TypeOK ==
  /\ active \in 1..2
  /\ router \in 1..2
  /\ demoted \in BOOLEAN
  /\ observer \in {"waiting", "settled"}
  /\ diagnosed \in {"none", "shown", "missing"}

\* Diagnose, run once the observer settled, lists the Agent unavailable.
DiagnoseShowsDemotion == diagnosed # "missing"

\* The observer reaches a verdict.
ObserverSettles == <>(observer = "settled")
====
