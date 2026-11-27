---- MODULE MCInterruptLatchContentionTwoRenewalsWitness ----
EXTENDS InterruptLatchContention

TwoRenewalsSettled == ~(latch = "done" /\ tries = 3)

====
