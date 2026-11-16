---------------------- MODULE DesktopStartOwnership ----------------------
\* lean: ServiceLifecycle.startDisposition
\* crates: gents-desktop-core
EXTENDS Naturals, LeanDecision_ServiceLifecycle
CONSTANT ObservePending
VARIABLES owner, service, cancelled, starts
vars == <<owner, service, cancelled, starts>>
Init == /\ owner = 0 /\ service = "stopped" /\ cancelled = {} /\ starts = 0
FirstStart == /\ owner = 0 /\ starts = 0
              /\ owner' = 1 /\ service' = "booting" /\ starts' = 1
              /\ UNCHANGED cancelled
RepeatedStart == /\ owner = 1 /\ starts = 1
                 /\ IF ObservePending /\ startDisposition(owner # 0) = "observe"
                       THEN /\ UNCHANGED <<owner, service, cancelled>>
                       ELSE /\ owner' = 2 /\ cancelled' = {1} /\ UNCHANGED service
                 /\ starts' = 2
Ready == /\ owner # 0 /\ service = "booting"
         /\ service' = "ready" /\ owner' = 0
         /\ UNCHANGED <<cancelled, starts>>
Rollback == /\ 1 \in cancelled /\ service # "stopped"
            /\ service' = "stopped" /\ UNCHANGED <<owner, cancelled, starts>>
Next == FirstStart \/ RepeatedStart \/ Ready \/ Rollback
Spec == Init /\ [][Next]_vars
PendingOwnerIsNotCancelled == cancelled = {}
=============================================================================
