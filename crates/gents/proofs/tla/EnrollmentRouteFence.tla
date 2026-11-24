---- MODULE EnrollmentRouteFence ----
\* lean: none
\* crates: gents-p2p
\* models: Enrollment.RouteFence.routeWrite, Enrollment.RouteFence.revisionWrite, TransactionFence.conflicts
\* One enrolled member's route on the node that authorizes it, against the
\* revocation of that member and a peer that syncs through the route.
\*
\* The reconcile (`reconcile_data_plane`) writes the route in one transaction:
\* it reads the member's authorization on the enrollment documents and, when
\* it is current, writes the route (`PeerPairingDesired`). The revoker
\* (`revoke_request`) commits the revocation under the enrollment decision
\* lock; a merge (`Merges`) delivers an admin-signed revocation without any
\* lock. Either wakes the reconcile, whose next pass deletes a route the
\* authority no longer names. The syncing peer (the pairing reconciler)
\* installs a replicator toward the member and tears it down; it derives each
\* install from its own fresh read of the authority, never from the route
\* (`PeerTrustsRoute` = FALSE).
\*
\* Premise (`TransactionFence`): the authorization read by filter is not
\* validated at commit, so a revocation committed between the reconcile's read
\* and its commit does not abort it. With `ReconcileLocks` the route write
\* holds the decision lock from its read through its commit, and one runtime
\* process holds a home, so a local revocation commits wholly before its read
\* or after its commit. The pairing reconciler's own check-then-install window
\* is `PairingGrantRevocation.tla`; here a check and its install are one step.
EXTENDS Naturals

CONSTANTS
  ReconcileLocks,   \* the route write holds the enrollment decision lock
  Merges,           \* a revocation can arrive by merge, holding no lock
  PeerTrustsRoute   \* the syncing peer installs from the route alone

VARIABLES
  authorized,   \* the member's authorization is current
  revokedBy,    \* "none", "local" or "merge"
  route,        \* the route document stands
  rc,           \* reconcile: "idle", "read" (route write open), "done"
  readAuth,     \* what the open route write read of the authorization
  lock,         \* the route write holds the enrollment decision lock
  pending,      \* a revocation woke the reconcile and its pass has not run
  replicator,   \* the syncing peer's replicator toward the member stands
  staleRoute,   \* ghost: a route write committed after a local revocation
  staleInstall  \* ghost: a replicator was installed after a revocation

vars == <<authorized, revokedBy, route, rc, readAuth, lock, pending,
          replicator, staleRoute, staleInstall>>

TypeOK ==
  /\ authorized \in BOOLEAN
  /\ revokedBy \in {"none", "local", "merge"}
  /\ route \in BOOLEAN
  /\ rc \in {"idle", "read", "done"}
  /\ readAuth \in BOOLEAN
  /\ lock \in BOOLEAN
  /\ pending \in BOOLEAN
  /\ replicator \in BOOLEAN
  /\ staleRoute \in BOOLEAN
  /\ staleInstall \in BOOLEAN

Init ==
  /\ authorized = TRUE
  /\ revokedBy = "none"
  /\ route = FALSE
  /\ rc = "idle"
  /\ readAuth = FALSE
  /\ lock = FALSE
  /\ pending = FALSE
  /\ replicator = FALSE
  /\ staleRoute = FALSE
  /\ staleInstall = FALSE

RouteRead ==
  /\ rc = "idle"
  /\ ReconcileLocks => ~lock
  /\ rc' = "read"
  /\ readAuth' = authorized
  /\ lock' = ReconcileLocks
  /\ UNCHANGED <<authorized, revokedBy, route, pending, replicator, staleRoute, staleInstall>>

\* The filtered read is not validated: the write commits on what it read.
RouteCommit ==
  /\ rc = "read"
  /\ rc' = "done"
  /\ lock' = FALSE
  /\ route' = (route \/ readAuth)
  /\ staleRoute' = (staleRoute \/ (readAuth /\ revokedBy = "local"))
  /\ UNCHANGED <<authorized, revokedBy, readAuth, pending, replicator, staleInstall>>

Revoke ==
  /\ authorized
  /\ ~lock
  /\ authorized' = FALSE
  /\ revokedBy' = "local"
  /\ pending' = TRUE
  /\ UNCHANGED <<route, rc, readAuth, lock, replicator, staleRoute, staleInstall>>

MergeRevocation ==
  /\ Merges
  /\ authorized
  /\ authorized' = FALSE
  /\ revokedBy' = "merge"
  /\ pending' = TRUE
  /\ UNCHANGED <<route, rc, readAuth, lock, replicator, staleRoute, staleInstall>>

\* The pass a revocation woke, once the route write settled.
Sweep ==
  /\ pending
  /\ rc # "read"
  /\ pending' = FALSE
  /\ route' = (route /\ authorized)
  /\ UNCHANGED <<authorized, revokedBy, rc, readAuth, lock, replicator, staleRoute, staleInstall>>

PeerInstall ==
  /\ ~replicator
  /\ route
  /\ PeerTrustsRoute \/ authorized
  /\ replicator' = TRUE
  /\ staleInstall' = (staleInstall \/ ~authorized)
  /\ UNCHANGED <<authorized, revokedBy, route, rc, readAuth, lock, pending, staleRoute>>

PeerTeardown ==
  /\ replicator
  /\ ~authorized \/ ~route
  /\ replicator' = FALSE
  /\ UNCHANGED <<authorized, revokedBy, route, rc, readAuth, lock, pending, staleRoute, staleInstall>>

Next == RouteRead \/ RouteCommit \/ Revoke \/ MergeRevocation \/ Sweep
        \/ PeerInstall \/ PeerTeardown

Spec == Init /\ [][Next]_vars
        /\ WF_vars(RouteCommit) /\ WF_vars(Sweep) /\ WF_vars(PeerTeardown)

\* A route is written only for a member whose authorization no revocation of
\* this node ended before the write committed.
NoRouteForRevokedMember == ~staleRoute

\* Nothing is installed toward a member after its revocation.
NoInstallForRevokedMember == ~staleInstall

\* A revoked member's route and replicator are removed, however it was revoked.
RevokedMemberIsDisconnected == []((~authorized) => <>(~route /\ ~replicator))
====
