---- MODULE MCClientSubmissionContentionTwoPublicationsWitness ----
EXTENDS ClientSubmissionContention

ReservationRebuiltTwice == ~(\E i \in 1..Len(wire) : wire[i].stage = "reserving" /\ wire[i].tries = 3)

====
