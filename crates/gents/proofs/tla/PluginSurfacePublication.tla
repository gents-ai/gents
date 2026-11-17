---- MODULE PluginSurfacePublication ----
\* lean: ToolPolicy.PluginPublication.retry, RuntimeReconcile.SurfacePublication.deferAuthority
\* crates: gents-tools, gents
\* The installer holds the cross-process exclusive store lock; construction
\* observes one shared-lock attempt. A failed proposal does not replace the
\* active generation. A request waits for the first executor's surface, or
\* uses the already-admitted generation while reconciliation is pending.
\* Remaining abstracts the original monotonic wait deadline, never reset by
\* a busy observation. Release may occur before or after that deadline.
EXTENDS Naturals, LeanDecision_ToolPolicy_PluginPublication,
        LeanDecision_RuntimeReconcile_SurfacePublication

CONSTANTS HavePrevious, WaitForPublication, KeepPrevious
VARIABLES locked, remaining, constructing, ready, request, prematureFailure, busyDemotion
vars == <<locked, remaining, constructing, ready, request, prematureFailure, busyDemotion>>

Init ==
  /\ locked = TRUE
  /\ remaining = 2
  /\ constructing = TRUE
  /\ ready = HavePrevious
  /\ request = "pending"
  /\ prematureFailure = FALSE
  /\ busyDemotion = FALSE

Release ==
  /\ locked
  /\ locked' = FALSE
  /\ UNCHANGED <<remaining, constructing, ready, request, prematureFailure, busyDemotion>>

Construct ==
  /\ constructing
  /\ IF HavePrevious /\ ~KeepPrevious /\ deferAuthority(locked, remaining > 0)
        THEN /\ constructing' = FALSE /\ ready' = FALSE /\ busyDemotion' = TRUE
             /\ UNCHANGED <<remaining, prematureFailure>>
        ELSE IF WaitForPublication /\ retry(locked, remaining > 0)
        THEN /\ remaining' = remaining - 1
             /\ UNCHANGED <<constructing, ready, prematureFailure, busyDemotion>>
        ELSE /\ constructing' = FALSE
             /\ ready' = (IF locked /\ KeepPrevious /\ deferAuthority(locked, remaining > 0) THEN ready ELSE ~locked)
             /\ busyDemotion' = (locked /\ HavePrevious /\ remaining > 0 /\ ~KeepPrevious)
             /\ prematureFailure' = (locked /\ remaining > 0 /\ ~HavePrevious)
             /\ UNCHANGED remaining
  /\ UNCHANGED <<locked, request>>

Route ==
  /\ request = "pending"
  /\ (ready \/ ~constructing)
  /\ request' = (IF ready THEN "completed" ELSE "failed")
  /\ UNCHANGED <<locked, remaining, constructing, ready, prematureFailure, busyDemotion>>

Next == Release \/ Construct \/ Route
Spec == Init /\ [][Next]_vars /\ WF_vars(Release) /\ WF_vars(Construct) /\ WF_vars(Route)
NoBusyDemotion == ~busyDemotion
NoPrematureFailure == ~prematureFailure
Settles == <> (request \in {"completed", "failed"})
====
