---- MODULE MCTestPortReservationPerRunnerSpread ----
EXTENDS TestPortReservation

CONSTANTS a, b

MCCursorStart == [t \in Tests |-> IF t = a THEN 1 ELSE 2]

====
