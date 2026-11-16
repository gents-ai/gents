---- MODULE BackendFirstProbe ----
EXTENDS Naturals, BackendFirstProbeCases

CONSTANTS HoldPending, ReleaseDeferred, StartupFailed
VARIABLES probe, generation, request, deferredAt, refusedBeforeProbe
vars == <<probe, generation, request, deferredAt, refusedBeforeProbe>>

AwaitProbe(waiting) ==
  (CHOOSE row \in AwaitProbeTable :
    row[1] = waiting /\ row[2] = StartupFailed)[3]

Init ==
  /\ probe = "unknown"
  /\ generation = 0
  /\ request = "absent"
  /\ deferredAt = 0
  /\ refusedBeforeProbe = FALSE

Submit ==
  /\ request = "absent"
  /\ request' = "pending"
  /\ UNCHANGED <<probe, generation, deferredAt, refusedBeforeProbe>>

Probe ==
  /\ probe = "unknown"
  /\ probe' \in {"healthy", "failed"}
  /\ generation' = generation + 1
  /\ UNCHANGED <<request, deferredAt, refusedBeforeProbe>>

Route ==
  /\ request = "pending"
       \/ (request = "deferred" /\ generation > deferredAt /\ ReleaseDeferred)
  /\ IF HoldPending /\ AwaitProbe(probe = "unknown")
        THEN /\ request' = "deferred"
             /\ deferredAt' = generation
             /\ UNCHANGED refusedBeforeProbe
        ELSE /\ request' = IF probe = "healthy" /\ ~StartupFailed
                             THEN "running" ELSE "failed"
             /\ refusedBeforeProbe' = (probe = "unknown" /\ ~StartupFailed)
             /\ UNCHANGED deferredAt
  /\ UNCHANGED <<probe, generation>>

Interrupt ==
  /\ request \in {"pending", "deferred"}
  /\ request' = "stopped"
  /\ UNCHANGED <<probe, generation, deferredAt, refusedBeforeProbe>>

Next == Submit \/ Probe \/ Route \/ Interrupt
Spec == Init /\ [][Next]_vars /\ WF_vars(Submit) /\ WF_vars(Probe) /\ WF_vars(Route)
NoPrematureRefusal == ~refusedBeforeProbe
ProviderAdmission == request = "running" => probe = "healthy" /\ ~StartupFailed
Settles == <> (request \in {"running", "failed", "stopped"})
====
