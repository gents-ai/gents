---- MODULE MCReplicatedRequestConvergenceStuck ----
EXTENDS ReplicatedRequestConvergence

\* Constants are bound via .cfg (Reemit = FALSE — single-shot, no re-drive).
\* Diagnostic: reproduces the reachable stuck state (owner terminal, a peer
\* non-terminal, no enabled fixing action) as a TerminalConverges violation.
====
