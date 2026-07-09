---- MODULE MCReplicatedRequestConvergenceStuck ----
EXTENDS ReplicatedRequestConvergence

\* Constants are bound via .cfg (Cap = 1 — budget too small for the delivery
\* loss). Diagnostic: reproduces the reachable stuck state (owner terminal, a
\* peer non-terminal, budget spent, no enabled fixing action) as a
\* TerminalConverges violation. This is the shipping cap's failure mode when a
\* peer's losses exceed its re-emit budget.
====
