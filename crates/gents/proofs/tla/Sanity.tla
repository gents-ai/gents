---- MODULE Sanity ----
EXTENDS Naturals

VARIABLE x

Init == x = 0

Next == x' = (x + 1) % 4

Spec == Init /\ [][Next]_x

Bounded == x \in 0..3

====
