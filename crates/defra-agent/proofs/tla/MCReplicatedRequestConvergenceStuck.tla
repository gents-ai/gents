---- MODULE MCReplicatedRequestConvergenceStuck ----
EXTENDS ReplicatedRequestConvergence

\* Constants are bound via .cfg (Cap = 1; reconnect replay disabled).
\* Diagnostic: reproduces the reachable stuck state (owner terminal, peer
\* non-terminal, budget spent, no enabled fixing action) and proves replay is
\* load-bearing beyond the bounded same-value write window.
====
