---- MODULE PluginSurfaceRecovery ----
\* lean: RuntimeReconcile.SurfacePublication.retryProposal
\* crates: gents
\* Publication can outlast both one bounded construction and the generic
\* document settle window. A rollback releases the lock without a DB event.
\* Each Attempt is an exhausted bounded construction; Release is eventually
\* fair, but it is not required to precede any particular Attempt. The correct
\* owner keeps its retry scheduled; the broken owner falls permanently idle.
EXTENDS LeanDecision_RuntimeReconcile_SurfacePublication

CONSTANT RetryBusy
VARIABLES locked, scheduled, proposed
vars == <<locked, scheduled, proposed>>

Init == locked = TRUE /\ scheduled = TRUE /\ proposed = FALSE
Release ==
  /\ locked
  /\ locked' = FALSE
  /\ UNCHANGED <<scheduled, proposed>>
Attempt ==
  /\ scheduled
  /\ scheduled' = IF locked THEN retryProposal(RetryBusy, FALSE) ELSE FALSE
  /\ proposed' = ~locked
  /\ UNCHANGED locked
Next == Release \/ Attempt
Spec == Init /\ [][Next]_vars /\ WF_vars(Release) /\ WF_vars(Attempt)
RecoversWithoutNewEvent == <> proposed
====
