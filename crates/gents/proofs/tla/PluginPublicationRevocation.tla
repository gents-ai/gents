---- MODULE PluginPublicationRevocation ----
\* lean: RuntimeReconcile.SurfacePublication.deferAuthority
\* crates: gents
\* models: ToolPolicy.PluginPublication.digestAuthorized
\* Each Tick is one second of the epoch's TOTAL 60s budget; neither config
\* events nor retries reset it. Revocation is independent of the store lock.
\* At expiry publish latest pins, demoting plugin slots if still busy.
EXTENDS Naturals, FiniteSets, LeanDecision_RuntimeReconcile_SurfacePublication
CONSTANT Bound, EnforceBound, EnforcePins
VARIABLES busy, elapsed, desiredPins, activePins, running, revoked, applied
vars == <<busy, elapsed, desiredPins, activePins, running, revoked, applied>>
Init ==
  /\ busy = TRUE /\ elapsed = 0 /\ desiredPins = {1}
  /\ activePins = {1} /\ running = {1} /\ revoked = FALSE /\ applied = FALSE
Revoke ==
  /\ ~revoked /\ revoked' = TRUE /\ desiredPins' = {}
  /\ elapsed' = 0
  /\ UNCHANGED <<busy, activePins, running, applied>>
Tick ==
  /\ revoked /\ ~applied /\ elapsed < Bound
  /\ elapsed' = elapsed + 1
  /\ IF EnforceBound /\ ~deferAuthority(busy, elapsed' < Bound)
       THEN /\ activePins' = desiredPins /\ running' = {} /\ applied' = TRUE
       ELSE UNCHANGED <<activePins, running, applied>>
  /\ UNCHANGED <<busy, desiredPins, revoked>>
Publish ==
  /\ revoked /\ ~applied /\ ~busy
  /\ activePins' = desiredPins /\ running' = {} /\ applied' = TRUE
  /\ UNCHANGED <<busy, elapsed, desiredPins, revoked>>
Release ==
  /\ busy /\ busy' = FALSE
  /\ UNCHANGED <<elapsed, desiredPins, activePins, running, revoked, applied>>
Execute ==
  \E digest \in {1, 2}:
    /\ (~EnforcePins \/ digest \in activePins)
    /\ running' = {digest}
    /\ UNCHANGED <<busy, elapsed, desiredPins, activePins, revoked, applied>>
Next == Revoke \/ Tick \/ Publish \/ Release \/ Execute
Spec == Init /\ [][Next]_vars /\ WF_vars(Tick) /\ WF_vars(Publish)
RevocationBound == (revoked /\ elapsed = Bound) => applied
RunningWithinPins == running \subseteq activePins
NoRevokedDigest == applied => running \cap {1} = {}
====
