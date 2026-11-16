---- MODULE PairingGrantRevocation ----
\* lean: none
\* crates: gents-p2p
(***************************************************************************)
(* An application pairing from node A toward enrolled peer B, racing the   *)
(* withdrawal of its author's P2P mutation grant. The pairing reconciler   *)
(* (`run_pairing_reconciler`, one sequential loop per node) reads the       *)
(* author's authority when it prepares a peer (`prepare_pairing_peer` ->    *)
(* `load_desired_with_authority` -> `evaluate_overlay`; reconcile.rs:67,    *)
(* store.rs:479-505, overlay_binding.rs:183) and then issues the owned diff *)
(* one transport operation at a time (reconcile.rs:266-284). A scope change *)
(* at a live address is a teardown followed by an install of the same      *)
(* address (diff.rs:132), so the route is absent between the two. An       *)
(* enrollment renewal re-issues the member's endpoint and can refuse the    *)
(* overlay for one sweep, so a sweep may decide a base-only route while the *)
(* author still holds the grant and the next one restores the collection,   *)
(* or reinstall an unchanged scope (`Churn`).                               *)
(*                                                                          *)
(* Premise (transport, defradb.rs): a replicator consults no document ACP   *)
(* or Gents grant when it pushes. Installing one that carries a collection  *)
(* pushes every document of that collection already on A (backfill), and   *)
(* it keeps pushing new ones while it stands. `PushChecksGrant` models the  *)
(* proposed transport change that removes this premise: each push and      *)
(* each backfill of the collection is authorized against the pairing       *)
(* author's grant at push time.                                             *)
(*                                                                          *)
(* The observer is an operator, and the                                     *)
(* `application_pairing_replicates_only_while_its_author_holds_the_grant`   *)
(* test: after withdrawing the grant it waits until the pairing status     *)
(* reports `author_lacks_grant` (evaluated live, not from the route) and    *)
(* the route toward B no longer carries the collection, then writes a       *)
(* document it expects to stay on A.                                        *)
(*                                                                          *)
(* Fixes judged here:                                                       *)
(*  - `RecheckBeforeInstall`: each install re-derives its scope from        *)
(*    authority read after the operations before it. This is also the only  *)
(*    Gents-side form of fencing an install by an authority generation: the *)
(*    generation is compared by a read in Gents, not by `add_replicator`,   *)
(*    so a withdrawal between the comparison and the install still lands.   *)
(*  - `RevokeTearsDown`: the withdrawal itself removes the collection from  *)
(*    the route in the same step (stronger than any real revoke path, which *)
(*    is a separate write).                                                 *)
(*  - `PushChecksGrant`: the transport premise removed.                     *)
(* Only `PushChecksGrant` keeps every document written after the withdrawal *)
(* off B (`StrictNoLeak`): a standing replicator pushes such a document     *)
(* before any reconciler or revoke path can act, and an install whose       *)
(* check preceded the withdrawal lands after it. Gents-side, the            *)
(* reconciler's guarantee is `NoStaleInstall` and the observer's is         *)
(* `ObservedWithdrawalHolds` once it waits for the base route to stand      *)
(* without the collection (`ObserverNeedsRoute`).                           *)
(***************************************************************************)
EXTENDS Naturals

CONSTANTS
  RecheckBeforeInstall, \* an install re-derives its scope from fresh authority
  ObserverNeedsRoute,   \* the observer requires the base route to be present
  Churn,                \* renewals: transient base-only decisions, same-scope reinstalls
  RevokeTearsDown,      \* the withdrawal removes the collection from the route itself
  PushChecksGrant       \* the transport authorizes each push against the grant

VARIABLES
  grant,      \* the pairing's author holds the P2P mutation grant
  route,      \* the replicator toward B: "cp" (carries the collection), "base", "none"
  rc,         \* reconciler: idle | decided | tornDown | checked
  want,       \* the scope the reconciler will install: "cp" or "base"
  atA,        \* documents of the paired collection on A
  atB,        \* documents of the paired collection that reached B
  obs,        \* observer: waiting | saw | wrote
  opAfter,    \* ghost: a transport operation of this sweep ran after the withdrawal
  stale       \* ghost: an install carried the collection after such an operation

vars == <<grant, route, rc, want, atA, atB, obs, opAfter, stale>>

\* "before": written while the grant was held; "x": written by anyone after
\* the withdrawal; "w": written by the observer once it saw the withdrawal.
Docs == {"before", "x", "w"}

TypeOK ==
  /\ grant \in BOOLEAN
  /\ route \in {"cp", "base", "none"}
  /\ rc \in {"idle", "decided", "tornDown", "checked"}
  /\ want \in {"cp", "base"}
  /\ atA \subseteq Docs /\ atB \subseteq Docs
  /\ obs \in {"waiting", "saw", "wrote"}
  /\ opAfter \in BOOLEAN /\ stale \in BOOLEAN

Init ==
  /\ grant = TRUE
  /\ route = "cp"
  /\ rc = "idle"
  /\ want = "cp"
  /\ atA = {"before"} /\ atB = {"before"}
  /\ obs = "waiting"
  /\ opAfter = FALSE /\ stale = FALSE

\* `evaluate_overlay`: the overlay contributes only while the author holds the
\* grant; a renewal may refuse it transiently.
Scope(transient) == IF grant /\ ~transient THEN "cp" ELSE "base"

Transients == IF Churn THEN BOOLEAN ELSE {FALSE}

\* What one push or backfill of the collection delivers.
Pushed == IF PushChecksGrant /\ ~grant THEN {} ELSE atA

(* The author's User withdraws the grant (a Tools write). *)
Withdraw ==
  /\ grant
  /\ grant' = FALSE
  /\ route' = IF RevokeTearsDown /\ route = "cp" THEN "base" ELSE route
  /\ UNCHANGED <<rc, want, atA, atB, obs, opAfter, stale>>

(* The reconciler. *)

\* `prepare_pairing_peer`: read the authority and decide the diff.
Prepare ==
  /\ rc = "idle"
  /\ \E transient \in Transients, reinstall \in Transients :
       /\ want' = Scope(transient)
       /\ rc' = IF route = want' /\ ~reinstall THEN "idle" ELSE "decided"
  /\ opAfter' = FALSE
  /\ UNCHANGED <<grant, route, atA, atB, obs, stale>>

\* `apply_op` teardown (remote_topology.rs:216).
Teardown ==
  /\ rc = "decided"
  /\ route' = "none"
  /\ rc' = "tornDown"
  /\ opAfter' = (opAfter \/ ~grant)
  /\ UNCHANGED <<grant, want, atA, atB, obs, stale>>

\* The recheck before an install.
Recheck ==
  /\ RecheckBeforeInstall
  /\ rc = "tornDown"
  /\ \E transient \in Transients : want' = Scope(transient)
  /\ rc' = "checked"
  /\ UNCHANGED <<grant, route, atA, atB, obs, opAfter, stale>>

\* `apply_op` install (remote_topology.rs:195): the replicator backfills.
Install ==
  /\ rc = IF RecheckBeforeInstall THEN "checked" ELSE "tornDown"
  /\ route' = want
  /\ atB' = IF want = "cp" THEN atB \cup Pushed ELSE atB
  /\ stale' = (stale \/ (want = "cp" /\ opAfter))
  /\ rc' = "idle"
  /\ UNCHANGED <<grant, want, atA, obs, opAfter>>

(* The transport: a standing replicator pushes. *)

Push ==
  /\ route = "cp" /\ ~(Pushed \subseteq atB)
  /\ atB' = atB \cup Pushed
  /\ UNCHANGED <<grant, route, rc, want, atA, obs, opAfter, stale>>

(* Writers. *)

WriteAfter ==
  /\ ~grant /\ "x" \notin atA
  /\ atA' = atA \cup {"x"}
  /\ UNCHANGED <<grant, route, rc, want, atB, obs, opAfter, stale>>

RouteWithout ==
  IF ObserverNeedsRoute THEN route = "base" ELSE route # "cp"

Observe ==
  /\ obs = "waiting" /\ ~grant /\ RouteWithout
  /\ obs' = "saw"
  /\ UNCHANGED <<grant, route, rc, want, atA, atB, opAfter, stale>>

Write ==
  /\ obs = "saw"
  /\ atA' = atA \cup {"w"}
  /\ obs' = "wrote"
  /\ UNCHANGED <<grant, route, rc, want, atB, opAfter, stale>>

Next ==
  \/ Withdraw \/ Prepare \/ Teardown \/ Recheck \/ Install \/ Push
  \/ WriteAfter \/ Observe \/ Write

Fairness ==
  /\ WF_vars(Prepare) /\ WF_vars(Teardown) /\ WF_vars(Recheck)
  /\ WF_vars(Install) /\ WF_vars(Push)

Spec == Init /\ [][Next]_vars /\ Fairness

(* Properties. *)

\* No install carries the collection once an operation of its sweep ran after
\* the withdrawal: the reconciler never installs on authority older than its
\* own last transport operation.
NoStaleInstall == ~stale

\* A document written after the observer saw the withdrawal reported and the
\* base route standing without the collection never reaches B.
ObservedWithdrawalHolds == "w" \notin atB

\* No document written after the withdrawal is committed reaches B.
StrictNoLeak == "x" \notin atB

\* A withdrawal stops the collection at the next sweep and the base route
\* stands again.
WithdrawalTakesEffect == (~grant) ~> (route = "base" /\ rc = "idle")

====
