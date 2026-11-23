---- MODULE PluginSurfaceRecovery ----
\* lean: RuntimeReconcile.SurfacePublication.retryAfterFailure, RuntimeReconcile.SurfacePublication.sendProposal
\* crates: gents
\* models: RuntimeReconcile.SurfacePublication.recreateSlot
EXTENDS Naturals, LeanDecision_RuntimeReconcile_SurfacePublication

\* A slot parked because an installer held the plugin store, or an agent a
\* held store left unavailable at startup, asks the watcher for a recovery.
\* The installer's release emits no database event, and the recovered
\* configuration is the one already active, so only the pending recovery can
\* bring the agent back. The watcher looks at the store on each attempt; a
\* reconcile may also fail once for an unrelated, transient reason after the
\* settle window has closed.
\*
\* Safeguard, removed by one constant: `KeepRecovery` carries the pending
\* recovery through a failed reconcile (Lean `retryAfterFailure`) and past the
\* fingerprint deduplication (Lean `sendProposal`).

CONSTANT KeepRecovery

VARIABLES locked, pending, scheduled, transient, proposed
vars == <<locked, pending, scheduled, transient, proposed>>

Init ==
  /\ locked = TRUE
  /\ pending = TRUE
  /\ scheduled = TRUE
  /\ transient = TRUE
  /\ proposed = FALSE

Release ==
  /\ locked
  /\ locked' = FALSE
  /\ UNCHANGED <<pending, scheduled, transient, proposed>>

Tracked == KeepRecovery /\ pending

\* A reconcile fails once for a transient reason.
Fail ==
  /\ scheduled /\ ~locked /\ transient
  /\ transient' = FALSE
  /\ scheduled' = retryAfterFailure(FALSE, Tracked)
  /\ UNCHANGED <<locked, pending, proposed>>

\* A reconcile succeeds with an unchanged configuration.
Succeed ==
  /\ scheduled /\ ~locked
  /\ IF sendProposal(FALSE, Tracked)
       THEN proposed' = TRUE /\ pending' = FALSE
       ELSE UNCHANGED <<proposed, pending>>
  /\ scheduled' = FALSE
  /\ UNCHANGED <<locked, transient>>

Next == Release \/ Fail \/ Succeed
Spec == Init /\ [][Next]_vars /\ WF_vars(Release) /\ WF_vars(Fail \/ Succeed)

\* The parked agent comes back without another configuration change.
RecoversWithoutNewEvent == <>proposed
====
