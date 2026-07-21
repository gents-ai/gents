---- MODULE MCReplicatedRequestConvergence ----
EXTENDS ReplicatedRequestConvergence

\* Constants are bound via .cfg (Cap = 3 — shipping TERMINAL_REDRIVE_CAP;
\* reconnect replay enabled). Bounded loss and an arbitrarily long initial
\* partition both converge without unbounded request writes.
====
